//! SoundCloud API request / response types.
//!
//! The SoundCloud `/resolve` endpoint returns JSON with very permissive shape
//! — many fields are optional depending on the resource kind and the
//! visibility (public/private, go+/regular). This module models only the
//! subset of fields we care about and tolerates unknown keys.
//!
//! ## HTTP host-function envelope
//!
//! The plugin wraps every outgoing request in an [`HttpRequest`] JSON and
//! expects an [`HttpResponse`] back from the host. The schemas mirror
//! `src-tauri/src/adapters/driven/plugin/host_functions.rs`.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::error::PluginError;

// ── Host function envelope ────────────────────────────────────────────────────

/// Matches `HttpRequest` in `host_functions.rs`.
#[derive(Debug, Serialize)]
pub struct HttpRequest {
    pub method: String,
    pub url: String,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub headers: HashMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
}

/// Matches `HttpResponse` in `host_functions.rs`.
#[derive(Debug, Deserialize)]
pub struct HttpResponse {
    pub status: u16,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    #[serde(default)]
    pub body: String,
}

impl HttpResponse {
    /// Returns the body if the status is 2xx, else a typed error.
    pub fn into_success_body(self) -> Result<String, PluginError> {
        if (200..300).contains(&self.status) {
            Ok(self.body)
        } else if self.status == 401 || self.status == 403 {
            Err(PluginError::Private(format!("status {}", self.status)))
        } else {
            Err(PluginError::HttpStatus {
                status: self.status,
                message: truncate(&self.body, 256),
            })
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut cut = max;
        while !s.is_char_boundary(cut) && cut > 0 {
            cut -= 1;
        }
        format!("{}…", &s[..cut])
    }
}

pub fn build_resolve_request(original_url: &str, client_id: &str) -> Result<String, PluginError> {
    let resolve_url = format!(
        "https://api-v2.soundcloud.com/resolve?url={}&client_id={}",
        urlencode(original_url),
        urlencode(client_id),
    );
    let req = HttpRequest {
        method: "GET".into(),
        url: resolve_url,
        headers: HashMap::new(),
        body: None,
    };
    Ok(serde_json::to_string(&req)?)
}

pub fn build_user_tracks_request(
    user_id: &str,
    client_id: &str,
    next_href: Option<&str>,
) -> Result<String, PluginError> {
    let url = match next_href {
        Some(next) => append_client_id(next, client_id),
        None => format!(
            "https://api-v2.soundcloud.com/users/{}/tracks?linked_partitioning=true&page_size=50&client_id={}",
            urlencode(user_id),
            urlencode(client_id),
        ),
    };
    let req = HttpRequest {
        method: "GET".into(),
        url,
        headers: HashMap::new(),
        body: None,
    };
    Ok(serde_json::to_string(&req)?)
}

pub fn parse_http_response(raw: &str) -> Result<HttpResponse, PluginError> {
    serde_json::from_str(raw).map_err(|e| PluginError::HostResponse(e.to_string()))
}

/// Minimal URL-encode for query parameters (RFC 3986 unreserved + '%').
///
/// A full percent-encoder would pull in an extra dependency for just two
/// call sites; the lookup table here covers every byte the resolve
/// endpoint accepts in a `url=` query string.
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

fn append_client_id(url: &str, client_id: &str) -> String {
    if url.contains("client_id=") {
        return url.to_string();
    }
    let separator = if url.contains('?') { '&' } else { '?' };
    format!("{url}{separator}client_id={}", urlencode(client_id))
}

// ── SoundCloud resource types ─────────────────────────────────────────────────

/// `/resolve` response envelope discriminated by the `kind` field.
///
/// Known kinds: `track`, `playlist`, `user`. Unknown kinds are mapped to
/// [`ResolveResponse::Unknown`] so the plugin can surface a clear error
/// instead of panicking.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind")]
pub enum ResolveResponse {
    #[serde(rename = "track")]
    Track(Track),
    #[serde(rename = "playlist")]
    Playlist(Playlist),
    #[serde(rename = "user")]
    User(User),
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ApiId {
    Numeric(u64),
    Text(String),
}

impl ApiId {
    pub fn as_string(&self) -> String {
        match self {
            Self::Numeric(id) => id.to_string(),
            Self::Text(id) => id.clone(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct Track {
    #[serde(default)]
    pub id: Option<ApiId>,
    #[serde(default)]
    pub urn: Option<String>,
    pub title: String,
    #[serde(default)]
    pub duration: Option<u64>,
    #[serde(default)]
    pub permalink_url: Option<String>,
    #[serde(default)]
    pub artwork_url: Option<String>,
    #[serde(default)]
    pub user: Option<TrackUser>,
    #[serde(default)]
    pub metadata_artist: Option<String>,
    #[serde(default)]
    pub streamable: Option<bool>,
    /// Transcodings provided by SoundCloud — each entry is a template URL
    /// that must be resolved to obtain the actual CDN stream URL.
    #[serde(default)]
    pub media: Option<TrackMedia>,
}

#[derive(Debug, Deserialize, Default)]
pub struct TrackMedia {
    #[serde(default)]
    pub transcodings: Vec<Transcoding>,
}

/// A single transcoding entry from the `/resolve` response.
///
/// The `url` field is a SoundCloud API endpoint (not a CDN URL). Calling it
/// with `?client_id=<id>` as a GET request returns a
/// `{ "url": "<actual_cdn_url>" }` JSON payload.
#[derive(Debug, Deserialize)]
pub struct Transcoding {
    pub url: String,
    #[serde(default)]
    pub format: Option<TranscodingFormat>,
    #[serde(default)]
    pub quality: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct TranscodingFormat {
    #[serde(default)]
    pub protocol: String,
    #[serde(default)]
    pub mime_type: String,
}

#[derive(Debug, Deserialize)]
pub struct TrackUser {
    pub username: String,
}

/// Build a request to resolve a transcoding template URL into the actual
/// CDN stream URL. SoundCloud requires `client_id` as a query parameter.
pub fn build_stream_request(transcoding_url: &str, client_id: &str) -> Result<String, PluginError> {
    let separator = if transcoding_url.contains('?') {
        '&'
    } else {
        '?'
    };
    let url = format!(
        "{}{}client_id={}",
        transcoding_url,
        separator,
        urlencode(client_id),
    );
    let req = HttpRequest {
        method: "GET".into(),
        url,
        headers: HashMap::new(),
        body: None,
    };
    Ok(serde_json::to_string(&req)?)
}

/// Parse the `{ "url": "<cdn_url>" }` JSON returned by a transcoding
/// template URL call.
pub fn parse_stream_url_response(body: &str) -> Result<String, PluginError> {
    #[derive(Deserialize)]
    struct StreamUrlResponse {
        url: String,
    }
    let parsed: StreamUrlResponse =
        serde_json::from_str(body).map_err(|e| PluginError::ParseJson(e.to_string()))?;
    Ok(parsed.url)
}

/// Select the best transcoding from a track's media list.
///
/// Preference order:
/// 1. `progressive` protocol (direct HTTP download, no HLS segmentation)
/// 2. `hls` (streaming, but universally supported)
///
/// Within each protocol, no further quality sorting is attempted — SoundCloud
/// typically provides only one quality level per protocol for non-Go+ tracks.
pub fn pick_best_transcoding(transcodings: &[Transcoding]) -> Option<&Transcoding> {
    // Prefer progressive (direct CDN link) over HLS adaptive.
    transcodings
        .iter()
        .find(|t| {
            t.format
                .as_ref()
                .map(|f| f.protocol == "progressive")
                .unwrap_or(false)
        })
        .or_else(|| {
            transcodings.iter().find(|t| {
                t.format
                    .as_ref()
                    .map(|f| f.protocol == "hls")
                    .unwrap_or(false)
            })
        })
}

#[derive(Debug, Deserialize)]
pub struct Playlist {
    #[serde(default)]
    pub id: Option<ApiId>,
    #[serde(default)]
    pub urn: Option<String>,
    pub title: String,
    #[serde(default)]
    pub permalink_url: Option<String>,
    #[serde(default)]
    pub artwork_url: Option<String>,
    #[serde(default)]
    pub tracks: Vec<Track>,
    #[serde(default)]
    pub track_count: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct User {
    #[serde(default)]
    pub id: Option<ApiId>,
    #[serde(default)]
    pub urn: Option<String>,
    pub username: String,
    #[serde(default)]
    pub permalink_url: Option<String>,
    #[serde(default)]
    pub avatar_url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TrackCollectionResponse {
    #[serde(default)]
    pub collection: Vec<Track>,
    #[serde(default)]
    pub next_href: Option<String>,
}

pub fn parse_track_collection_response(body: &str) -> Result<TrackCollectionResponse, PluginError> {
    serde_json::from_str(body).map_err(|e| PluginError::ParseJson(e.to_string()))
}

pub fn track_resource_id(track: &Track) -> Option<String> {
    stable_resource_id(track.urn.as_ref(), track.id.as_ref())
}

pub fn user_resource_id(user: &User) -> Option<String> {
    stable_resource_id(user.urn.as_ref(), user.id.as_ref())
}

fn stable_resource_id(urn: Option<&String>, id: Option<&ApiId>) -> Option<String> {
    urn.cloned().or_else(|| id.map(ApiId::as_string))
}

pub fn parse_resolve_response(body: &str) -> Result<ResolveResponse, PluginError> {
    serde_json::from_str(body).map_err(|e| PluginError::ParseJson(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRACK_JSON: &str = r#"{
        "kind": "track",
        "id": 12345,
        "title": "Flickermood",
        "duration": 225000,
        "permalink_url": "https://soundcloud.com/forss/flickermood",
        "artwork_url": "https://i1.sndcdn.com/artworks-12345.jpg",
        "streamable": true,
        "user": { "username": "Forss" }
    }"#;

    const PLAYLIST_JSON: &str = r#"{
        "kind": "playlist",
        "id": 99,
        "title": "Soulhack",
        "permalink_url": "https://soundcloud.com/forss/sets/soulhack",
        "tracks": [
            {"kind": "track", "id": 1, "title": "Flickermood"},
            {"kind": "track", "id": 2, "title": "Journeyman"}
        ],
        "track_count": 2
    }"#;

    const USER_JSON: &str = r#"{
        "kind": "user",
        "id": 42,
        "username": "forss",
        "permalink_url": "https://soundcloud.com/forss",
        "avatar_url": "https://i1.sndcdn.com/avatars-42.jpg"
    }"#;

    const UNKNOWN_KIND_JSON: &str = r#"{"kind": "system-playlist", "id": 1}"#;

    #[test]
    fn parse_track_response() {
        let resolved = parse_resolve_response(TRACK_JSON).unwrap();
        match resolved {
            ResolveResponse::Track(t) => {
                assert_eq!(track_resource_id(&t).as_deref(), Some("12345"));
                assert_eq!(t.title, "Flickermood");
                assert_eq!(t.duration, Some(225000));
                assert_eq!(t.user.unwrap().username, "Forss");
                assert!(t.artwork_url.is_some());
            }
            other => panic!("expected Track, got {other:?}"),
        }
    }

    #[test]
    fn parse_playlist_response() {
        let resolved = parse_resolve_response(PLAYLIST_JSON).unwrap();
        match resolved {
            ResolveResponse::Playlist(p) => {
                assert_eq!(
                    stable_resource_id(p.urn.as_ref(), p.id.as_ref()).as_deref(),
                    Some("99")
                );
                assert_eq!(p.title, "Soulhack");
                assert_eq!(p.tracks.len(), 2);
                assert_eq!(p.track_count, Some(2));
            }
            other => panic!("expected Playlist, got {other:?}"),
        }
    }

    #[test]
    fn parse_user_response() {
        let resolved = parse_resolve_response(USER_JSON).unwrap();
        match resolved {
            ResolveResponse::User(u) => {
                assert_eq!(user_resource_id(&u).as_deref(), Some("42"));
                assert_eq!(u.username, "forss");
            }
            other => panic!("expected User, got {other:?}"),
        }
    }

    #[test]
    fn parse_unknown_kind_falls_through() {
        let resolved = parse_resolve_response(UNKNOWN_KIND_JSON).unwrap();
        assert!(matches!(resolved, ResolveResponse::Unknown));
    }

    #[test]
    fn parse_resolve_rejects_malformed_json() {
        let err = parse_resolve_response("not json").unwrap_err();
        assert!(matches!(err, PluginError::ParseJson(_)));
    }

    #[test]
    fn http_response_2xx_returns_body() {
        let resp = HttpResponse {
            status: 200,
            headers: HashMap::new(),
            body: "ok".into(),
        };
        assert_eq!(resp.into_success_body().unwrap(), "ok");
    }

    #[test]
    fn http_response_401_is_private() {
        let resp = HttpResponse {
            status: 401,
            headers: HashMap::new(),
            body: "forbidden".into(),
        };
        assert!(matches!(
            resp.into_success_body().unwrap_err(),
            PluginError::Private(_)
        ));
    }

    #[test]
    fn http_response_500_is_http_status_error() {
        let resp = HttpResponse {
            status: 500,
            headers: HashMap::new(),
            body: "boom".into(),
        };
        match resp.into_success_body().unwrap_err() {
            PluginError::HttpStatus { status, .. } => assert_eq!(status, 500),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn urlencode_roundtrips_safe_chars() {
        assert_eq!(urlencode("abc-_.~"), "abc-_.~");
        assert_eq!(
            urlencode("https://soundcloud.com/a/b"),
            "https%3A%2F%2Fsoundcloud.com%2Fa%2Fb"
        );
    }

    #[test]
    fn build_resolve_request_encodes_target() {
        let req_str =
            build_resolve_request("https://soundcloud.com/forss/flickermood", "abc123").unwrap();
        assert!(req_str.contains("\"method\":\"GET\""));
        assert!(req_str.contains("client_id=abc123"));
        assert!(req_str.contains("url=https%3A%2F%2Fsoundcloud.com%2Fforss%2Fflickermood"));
    }

    #[test]
    fn build_user_tracks_request_uses_initial_collection_endpoint() {
        let req_str = build_user_tracks_request("soundcloud:users:42", "abc123", None).unwrap();
        assert!(req_str.contains("users/soundcloud%3Ausers%3A42/tracks"));
        assert!(req_str.contains("linked_partitioning=true"));
        assert!(req_str.contains("page_size=50"));
        assert!(req_str.contains("client_id=abc123"));
    }

    #[test]
    fn build_user_tracks_request_preserves_next_href_cursor() {
        let req_str = build_user_tracks_request(
            "ignored",
            "abc123",
            Some("https://api-v2.soundcloud.com/users/42/tracks?linked_partitioning=true&cursor=next-page"),
        )
        .unwrap();
        assert!(req_str.contains("cursor=next-page"));
        assert!(req_str.contains("client_id=abc123"));
    }

    #[test]
    fn pick_best_transcoding_prefers_progressive_over_hls() {
        let transcodings = vec![
            Transcoding {
                url: "hls_url".into(),
                format: Some(TranscodingFormat {
                    protocol: "hls".into(),
                    mime_type: "".into(),
                }),
                quality: None,
            },
            Transcoding {
                url: "prog_url".into(),
                format: Some(TranscodingFormat {
                    protocol: "progressive".into(),
                    mime_type: "".into(),
                }),
                quality: None,
            },
        ];
        assert_eq!(
            pick_best_transcoding(&transcodings).unwrap().url,
            "prog_url"
        );
    }

    #[test]
    fn pick_best_transcoding_falls_back_to_hls() {
        let transcodings = vec![Transcoding {
            url: "hls_url".into(),
            format: Some(TranscodingFormat {
                protocol: "hls".into(),
                mime_type: "".into(),
            }),
            quality: None,
        }];
        assert_eq!(pick_best_transcoding(&transcodings).unwrap().url, "hls_url");
    }

    #[test]
    fn pick_best_transcoding_returns_none_for_empty() {
        assert!(pick_best_transcoding(&[]).is_none());
    }

    #[test]
    fn build_stream_request_appends_client_id_without_existing_query() {
        let req_str = build_stream_request("https://cf-media.sndcdn.com/123", "myid").unwrap();
        assert!(req_str.contains("client_id=myid"));
        assert!(req_str.contains("?client_id="));
    }

    #[test]
    fn build_stream_request_uses_ampersand_when_query_already_present() {
        let req_str =
            build_stream_request("https://cf-media.sndcdn.com/123?foo=bar", "myid").unwrap();
        assert!(req_str.contains("&client_id=myid"));
    }

    #[test]
    fn parse_stream_url_response_extracts_url() {
        let url =
            parse_stream_url_response(r#"{"url":"https://cdn.example.com/audio.mp3"}"#).unwrap();
        assert_eq!(url, "https://cdn.example.com/audio.mp3");
    }

    #[test]
    fn parse_stream_url_response_rejects_malformed() {
        assert!(matches!(
            parse_stream_url_response("not json").unwrap_err(),
            PluginError::ParseJson(_)
        ));
    }
}
