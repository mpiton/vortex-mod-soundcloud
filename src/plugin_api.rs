//! WASM-only module: `#[plugin_fn]` exports and `#[host_fn]` imports.
//!
//! Gated behind `cfg(target_family = "wasm")` because the macros emit
//! code that only compiles for a WASM target.

use extism_pdk::*;

use crate::api::{
    build_resolve_request, build_stream_request, build_user_tracks_request, parse_http_response,
    parse_resolve_response, parse_stream_url_response, parse_track_collection_response,
    pick_best_transcoding, track_resource_id, user_resource_id, ResolveResponse, Track, User,
};
use crate::error::PluginError;
use crate::extractor::{
    parse_download_path_from_stdout, parse_subprocess_response, yt_dlp_args_for_download_to_file,
    DEFAULT_DOWNLOAD_TIMEOUT_MS,
};
use crate::{
    build_artist_response, build_playlist_response, build_single_track_response, ensure_playlist,
    ensure_soundcloud_url, ensure_track, handle_can_handle, handle_supports_playlist,
    response_to_extract_links,
};

const MAX_ARTIST_TRACK_PAGES: usize = 20;

// ── Host function imports ─────────────────────────────────────────────────────

#[host_fn]
extern "ExtismHost" {
    /// JSON in → JSON out — see `HttpRequest` / `HttpResponse` envelopes.
    fn http_request(req: String) -> String;
    fn get_config(key: String) -> String;
    fn run_subprocess(req: String) -> String;
}

// ── Plugin function exports ───────────────────────────────────────────────────

#[plugin_fn]
pub fn can_handle(url: String) -> FnResult<String> {
    Ok(handle_can_handle(&url))
}

#[plugin_fn]
pub fn supports_playlist(url: String) -> FnResult<String> {
    Ok(handle_supports_playlist(&url))
}

#[plugin_fn]
pub fn extract_links(url: String) -> FnResult<String> {
    ensure_soundcloud_url(&url).map_err(error_to_fn_error)?;

    let resolved = resolve(&url)?;
    let response = match resolved {
        ResolveResponse::User(user) => {
            let tracks = fetch_all_user_tracks(&user)?;
            build_artist_response(&user, tracks)
        }
        other => response_to_extract_links(other).map_err(error_to_fn_error)?,
    };
    Ok(serde_json::to_string(&response)?)
}

#[plugin_fn]
pub fn extract_playlist(url: String) -> FnResult<String> {
    ensure_playlist(&url).map_err(error_to_fn_error)?;

    let resolved = resolve(&url)?;
    let response = match resolved {
        ResolveResponse::Playlist(p) => build_playlist_response(p),
        ResolveResponse::User(user) => build_artist_response(&user, fetch_all_user_tracks(&user)?),
        ResolveResponse::Track(_) => {
            return Err(error_to_fn_error(PluginError::UnsupportedUrl(
                "single track cannot be extracted as playlist".into(),
            )))
        }
        ResolveResponse::Unknown => {
            return Err(error_to_fn_error(PluginError::UnsupportedUrl(
                "unknown resource kind".into(),
            )))
        }
    };
    Ok(serde_json::to_string(&response)?)
}

#[plugin_fn]
pub fn extract_track(url: String) -> FnResult<String> {
    ensure_track(&url).map_err(error_to_fn_error)?;

    let resolved = resolve(&url)?;
    let response = match resolved {
        ResolveResponse::Track(t) => build_single_track_response(t),
        _ => {
            return Err(error_to_fn_error(PluginError::UnsupportedUrl(
                "resolved resource is not a track".into(),
            )))
        }
    };
    Ok(serde_json::to_string(&response)?)
}

/// Resolve the direct CDN stream URL for a single SoundCloud track.
///
/// Input JSON: `{ "url", "quality"?, "format"?, "audio_only"? }`.
/// Returns the raw CDN audio URL. SoundCloud resolution requires two HTTP
/// round-trips: one to `/resolve` (get track metadata + transcoding templates)
/// and one to the chosen template URL (get the actual CDN URL).
///
/// `quality`, `format`, and `audio_only` are accepted for API parity with
/// other plugins but are not used: SoundCloud provides only one quality level
/// per track for non-Go+ accounts, and the stream is always audio-only.
#[plugin_fn]
pub fn resolve_stream_url(input: String) -> FnResult<String> {
    #[derive(serde::Deserialize)]
    struct Input {
        url: String,
    }

    let params: Input =
        serde_json::from_str(&input).map_err(|e| error_to_fn_error(PluginError::SerdeJson(e)))?;

    ensure_track(&params.url).map_err(error_to_fn_error)?;

    let resolved = resolve(&params.url)?;
    let track = match resolved {
        ResolveResponse::Track(t) => t,
        _ => {
            return Err(error_to_fn_error(PluginError::UnsupportedUrl(
                "resolved resource is not a track".into(),
            )))
        }
    };

    let transcodings = track
        .media
        .as_ref()
        .map(|m| m.transcodings.as_slice())
        .unwrap_or(&[]);

    let best = pick_best_transcoding(transcodings)
        .ok_or_else(|| error_to_fn_error(PluginError::NoStreamAvailable))?;

    if best
        .format
        .as_ref()
        .is_some_and(|format| format.protocol == "hls")
    {
        return Err(error_to_fn_error(PluginError::AdaptiveStreamOnly));
    }

    fetch_stream_url(&best.url)
}

#[plugin_fn]
pub fn download_to_file(input: String) -> FnResult<String> {
    #[derive(serde::Deserialize)]
    struct Input {
        url: String,
        #[serde(default)]
        format: String,
        output_dir: String,
    }

    let params: Input =
        serde_json::from_str(&input).map_err(|e| error_to_fn_error(PluginError::SerdeJson(e)))?;

    ensure_track(&params.url).map_err(error_to_fn_error)?;

    let args = yt_dlp_args_for_download_to_file(&params.url, &params.format, &params.output_dir);
    let req = crate::extractor::SubprocessRequest {
        binary: "yt-dlp".into(),
        args,
        timeout_ms: DEFAULT_DOWNLOAD_TIMEOUT_MS,
    };
    let req_json =
        serde_json::to_string(&req).map_err(|e| error_to_fn_error(PluginError::SerdeJson(e)))?;

    let resp_json = unsafe { run_subprocess(req_json)? };
    let stdout = parse_subprocess_response(&resp_json).map_err(error_to_fn_error)?;
    parse_download_path_from_stdout(&stdout).map_err(error_to_fn_error)
}

fn fetch_all_user_tracks(user: &User) -> FnResult<Vec<Track>> {
    let client_id = read_client_id();
    let user_id = user_resource_id(user).ok_or_else(|| {
        error_to_fn_error(PluginError::UnsupportedUrl(format!(
            "artist profile '{}' has no stable id or urn in the resolve response",
            user.username
        )))
    })?;

    let mut next_href: Option<String> = None;
    let mut tracks = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for _ in 0..MAX_ARTIST_TRACK_PAGES {
        let req_json = build_user_tracks_request(&user_id, &client_id, next_href.as_deref())
            .map_err(error_to_fn_error)?;
        let body = perform_soundcloud_request(req_json)?;
        let page = parse_track_collection_response(&body).map_err(error_to_fn_error)?;

        for track in page.collection {
            let dedupe_key = track_resource_id(&track)
                .or_else(|| track.permalink_url.clone())
                .unwrap_or_else(|| track.title.clone());
            if !seen.insert(dedupe_key) {
                continue;
            }
            if track
                .permalink_url
                .as_deref()
                .is_some_and(|url| !url.trim().is_empty())
            {
                tracks.push(track);
            }
        }

        match page.next_href.filter(|href| !href.trim().is_empty()) {
            Some(href) => next_href = Some(href),
            None => {
                if tracks.is_empty() {
                    return Err(error_to_fn_error(PluginError::UnsupportedUrl(format!(
                        "artist profile '{}' has no downloadable public tracks",
                        user.username
                    ))));
                }
                return Ok(tracks);
            }
        }
    }

    Err(error_to_fn_error(PluginError::UnsupportedUrl(format!(
        "artist profile '{}' exceeded the pagination limit ({MAX_ARTIST_TRACK_PAGES} pages)",
        user.username
    ))))
}

// ── Host function wiring ──────────────────────────────────────────────────────

/// Issue a `/resolve` call against api-v2.soundcloud.com via the host and
/// return the parsed envelope.
fn resolve(url: &str) -> FnResult<ResolveResponse> {
    let client_id = read_client_id();
    let req_json = build_resolve_request(url, &client_id).map_err(error_to_fn_error)?;
    // SAFETY: `http_request` is resolved by the Vortex plugin host at
    // load time (see src-tauri/src/adapters/driven/plugin/host_functions.rs:
    // `make_http_request_function`). Invariants:
    //   1. The host registers `http_request` in the `ExtismHost` namespace
    //      before any `#[plugin_fn]` export is callable — a missing
    //      symbol would abort `Plugin::new` in extism_loader.rs.
    //   2. The ABI is `(I64) -> I64` — a single u64 Extism memory handle
    //      in, a single u64 handle out. The `#[host_fn]` macro marshals
    //      `String` to/from the memory handle.
    //   3. The host enforces capability `http=true` from the manifest
    //      before invoking the implementation; rejections return an
    //      error which `?` propagates safely.
    //   4. Inputs and outputs are owned, serialisable JSON strings — no
    //      aliasing or mutability concerns.
    let body = perform_soundcloud_request(req_json)?;
    parse_resolve_response(&body).map_err(error_to_fn_error)
}

/// Read the `client_id` config value. Returns an empty string if the
/// host has not yet wired `get_config` (forward-compatible with the
/// manifest parser, which currently ignores `[config]`).
fn read_client_id() -> String {
    // SAFETY: `get_config` is registered host-side before plugin exports
    // run (see src-tauri/src/adapters/driven/plugin/host_functions.rs:
    // `make_get_config_function`). Invariants:
    //   1. The symbol is registered in the `ExtismHost` namespace
    //      before any `#[plugin_fn]` export is callable.
    //   2. The ABI is `(I64) -> I64`; the `#[host_fn]` macro marshals
    //      `String` in/out.
    //   3. A missing key or transient error returns the empty default
    //      so the plugin still builds the URL — the host surfaces the
    //      401/403 via `http_request`, which `HttpResponse::into_success_body`
    //      maps to `PluginError::Private` and the user sees a clear
    //      "SoundCloud resource is private" error.
    //   4. Inputs/outputs are owned JSON strings — no aliasing concerns.
    unsafe { get_config("client_id".to_string()) }.unwrap_or_default()
}

/// Call a SoundCloud transcoding template URL to obtain the actual CDN
/// stream URL.
///
/// The template URL is appended with `?client_id=<id>` and the response
/// JSON `{ "url": "..." }` is parsed to extract the CDN URL.
fn fetch_stream_url(template_url: &str) -> FnResult<String> {
    let client_id = read_client_id();
    let req_json = build_stream_request(template_url, &client_id).map_err(error_to_fn_error)?;
    let body = perform_soundcloud_request(req_json)?;
    parse_stream_url_response(&body).map_err(error_to_fn_error)
}

fn perform_soundcloud_request(req_json: String) -> FnResult<String> {
    // SAFETY: `http_request` is resolved by the Vortex plugin host at
    // load time. Inputs and outputs are owned JSON strings, and host-side
    // capability checks run before the request executes.
    let resp_json = unsafe { http_request(req_json)? };
    let response = parse_http_response(&resp_json).map_err(error_to_fn_error)?;
    response.into_success_body().map_err(error_to_fn_error)
}

fn error_to_fn_error(err: PluginError) -> WithReturnCode<extism_pdk::Error> {
    extism_pdk::Error::msg(err.to_string()).into()
}
