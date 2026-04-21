//! WASM-only module: `#[plugin_fn]` exports and `#[host_fn]` imports.
//!
//! Gated behind `cfg(target_family = "wasm")` because the macros emit
//! code that only compiles for a WASM target.

use extism_pdk::*;

use std::collections::HashMap;

use crate::api::{
    build_resolve_request, build_stream_request, build_user_tracks_request, parse_http_response,
    parse_resolve_response, parse_stream_url_response, parse_track_collection_response,
    pick_best_transcoding, track_resource_id, user_resource_id, HttpRequest, ResolveResponse,
    Track, User,
};
use crate::client_id::{extract_client_id, extract_js_urls};
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
const SOUNDCLOUD_HOMEPAGE: &str = "https://soundcloud.com/";

// ── Host function imports ─────────────────────────────────────────────────────

#[host_fn]
extern "ExtismHost" {
    /// JSON in → JSON out — see `HttpRequest` / `HttpResponse` envelopes.
    fn http_request(req: String) -> String;
    fn get_config(key: String) -> String;
    fn set_config(entry: String);
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
    let client_id = read_client_id().map_err(error_to_fn_error)?;
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

    // Hit MAX_ARTIST_TRACK_PAGES with more pages still available: return
    // what we've collected so far (truncation) instead of discarding it.
    if tracks.is_empty() {
        return Err(error_to_fn_error(PluginError::UnsupportedUrl(format!(
            "artist profile '{}' has no downloadable public tracks",
            user.username
        ))));
    }
    Ok(tracks)
}

// ── Host function wiring ──────────────────────────────────────────────────────

/// Issue a `/resolve` call against api-v2.soundcloud.com via the host and
/// return the parsed envelope.
fn resolve(url: &str) -> FnResult<ResolveResponse> {
    let client_id = read_client_id().map_err(error_to_fn_error)?;
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

/// Return the `client_id` needed to talk to `api-v2.soundcloud.com`.
///
/// Lookup order:
///   1. The per-plugin config value (populated by a prior successful
///      discovery or, eventually, by a user-facing settings screen).
///   2. Auto-discovery by scraping `https://soundcloud.com/` for an
///      app-bundle URL and regex-extracting the embedded literal.
///
/// On discovery success the value is cached via `set_config` so the
/// next call short-circuits on step 1.
///
/// A discovery failure is propagated — swallowing it into an empty
/// string would cause SoundCloud to 401 on the next call, which maps
/// to the misleading `PluginError::Private` ("resource is private")
/// message. Surfacing the real cause lets the user see that the
/// plugin failed to obtain an id, not that their track is locked.
fn read_client_id() -> Result<String, PluginError> {
    // SAFETY: `get_config` is registered host-side before plugin exports
    // run (see src-tauri/src/adapters/driven/plugin/host_functions.rs:
    // `make_get_config_function`). Invariants:
    //   1. The symbol is registered in the `ExtismHost` namespace
    //      before any `#[plugin_fn]` export is callable.
    //   2. The ABI is `(I64) -> I64`; the `#[host_fn]` macro marshals
    //      `String` in/out.
    //   3. Inputs/outputs are owned JSON strings — no aliasing concerns.
    let cached = unsafe { get_config("client_id".to_string()) }
        .map_err(|e| PluginError::HostResponse(e.to_string()))?;
    if !cached.is_empty() {
        return Ok(cached);
    }
    let id = discover_client_id()?;
    // Persist so we only pay the two-hop discovery cost once per plugin
    // lifetime. The host expects the JSON shape
    // `{"key":"...","value":"..."}` — see `ConfigEntry` in
    // `src-tauri/src/adapters/driven/plugin/host_functions.rs`. A
    // cache-write failure is non-fatal; the current call still gets
    // the fresh id.
    let entry = serde_json::json!({ "key": "client_id", "value": id }).to_string();
    // SAFETY: `set_config` is registered host-side and ABI-compatible
    // the same way `get_config` is; it accepts a JSON string.
    let _ = unsafe { set_config(entry) };
    Ok(id)
}

/// Fetch soundcloud.com, pull an app-bundle JS URL out of the HTML, and
/// regex-extract the public `client_id` literal. Returns an error only
/// if every candidate bundle either fails to download or doesn't match
/// the marker.
fn discover_client_id() -> Result<String, PluginError> {
    let home = fetch_body(SOUNDCLOUD_HOMEPAGE)?;
    let mut last_err: Option<PluginError> = None;
    for url in extract_js_urls(&home) {
        match fetch_body(&url) {
            Ok(js) => {
                if let Some(id) = extract_client_id(&js) {
                    return Ok(id);
                }
            }
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or(PluginError::HostResponse(
        "no client_id marker in any SoundCloud bundle".into(),
    )))
}

/// Minimal GET helper for discovery — no auth, no custom headers.
fn fetch_body(url: &str) -> Result<String, PluginError> {
    let req = HttpRequest {
        method: "GET".into(),
        url: url.to_string(),
        headers: HashMap::new(),
        body: None,
    };
    let req_json = serde_json::to_string(&req)?;
    // SAFETY: same invariants as `http_request` in `perform_soundcloud_request`.
    let resp_json = unsafe { http_request(req_json) }
        .map_err(|e| PluginError::HostResponse(e.to_string()))?;
    let response = parse_http_response(&resp_json)?;
    response.into_success_body()
}

/// Call a SoundCloud transcoding template URL to obtain the actual CDN
/// stream URL.
///
/// The template URL is appended with `?client_id=<id>` and the response
/// JSON `{ "url": "..." }` is parsed to extract the CDN URL.
fn fetch_stream_url(template_url: &str) -> FnResult<String> {
    let client_id = read_client_id().map_err(error_to_fn_error)?;
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
