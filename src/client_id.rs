//! Auto-discovery of a public SoundCloud `client_id` from the public
//! web bundle.
//!
//! SoundCloud's web app embeds a non-secret `client_id` in its JavaScript
//! bundles served from `a-v2.sndcdn.com`. The same identifier is required
//! on every `api-v2.soundcloud.com` request. We scrape the homepage for
//! the current set of JS bundle URLs, fetch one, and regex-search for
//! the `client_id:"XXXX"` literal.
//!
//! The extraction functions below are pure and native-testable. The
//! orchestration (HTTP calls via the host + config cache) lives in
//! `plugin_api.rs` because it requires the WASM `#[host_fn]` imports.

const SNDCDN_ASSET_PREFIX: &str = "https://a-v2.sndcdn.com/assets/";
const JS_SUFFIX: &str = ".js";
const CLIENT_ID_MARKER: &str = "client_id:\"";

/// Pull every `https://a-v2.sndcdn.com/assets/<hash>.js` URL out of the
/// SoundCloud homepage HTML. The page ships several — any one of them
/// contains a `client_id` literal, but older/newer variants may be
/// polyfill-only, so we return them all for the caller to try in order.
///
/// The scan is bounded to the current HTML attribute value (stops at
/// the first `"`, `'`, `>`, or whitespace). Without that guard a
/// stray `.js` later in the document could extend the slice across
/// unrelated markup and yield a garbage URL.
pub fn extract_js_urls(html: &str) -> Vec<String> {
    let mut urls: Vec<String> = Vec::new();
    let mut cursor = 0;
    while let Some(prefix_rel) = html[cursor..].find(SNDCDN_ASSET_PREFIX) {
        let start = cursor + prefix_rel;
        let tail = &html[start..];
        let token_len = tail
            .find(['"', '\'', '>', ' ', '\n', '\r', '\t'])
            .unwrap_or(tail.len());
        let end = start + token_len;
        let token = &html[start..end];
        if token.ends_with(JS_SUFFIX) && !urls.iter().any(|u| u == token) {
            urls.push(token.to_string());
        }
        // Advance past this token regardless of whether it was kept so a
        // non-JS asset under the same prefix doesn't trap the loop.
        cursor = end;
    }
    urls
}

/// Find the first `client_id:"<value>"` literal in a JS payload and
/// return the value. Returns `None` if no marker is present — the
/// caller is expected to try another bundle.
pub fn extract_client_id(js: &str) -> Option<String> {
    let start = js.find(CLIENT_ID_MARKER)?;
    let after_marker = start + CLIENT_ID_MARKER.len();
    let tail = &js[after_marker..];
    let end = tail.find('"')?;
    let id = &tail[..end];
    // Reject empty / whitespace-only / suspicious-char values so a
    // malformed bundle doesn't poison the cache.
    if id.is_empty() || id.chars().any(|c| !c.is_ascii_alphanumeric()) {
        return None;
    }
    Some(id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_js_urls_finds_all_unique_bundles() {
        let html = r#"
            <script crossorigin src="https://a-v2.sndcdn.com/assets/4-abcdef.js"></script>
            <script crossorigin src="https://a-v2.sndcdn.com/assets/app-12345.js"></script>
            <script crossorigin src="https://a-v2.sndcdn.com/assets/4-abcdef.js"></script>
        "#;
        let urls = extract_js_urls(html);
        assert_eq!(urls.len(), 2);
        assert!(urls.contains(&"https://a-v2.sndcdn.com/assets/4-abcdef.js".into()));
        assert!(urls.contains(&"https://a-v2.sndcdn.com/assets/app-12345.js".into()));
    }

    #[test]
    fn extract_js_urls_returns_empty_when_no_bundles() {
        assert!(extract_js_urls("<html><body>hello</body></html>").is_empty());
    }

    #[test]
    fn extract_js_urls_skips_non_js_assets_under_same_prefix() {
        // A CSS/PNG asset under /assets/ must not be mistaken for a JS
        // bundle even if a later `.js` appears elsewhere on the page.
        let html = r#"
            <link href="https://a-v2.sndcdn.com/assets/styles-abc.css" rel="stylesheet">
            <script src="https://a-v2.sndcdn.com/assets/bundle-def.js"></script>
        "#;
        let urls = extract_js_urls(html);
        assert_eq!(urls, vec!["https://a-v2.sndcdn.com/assets/bundle-def.js".to_string()]);
    }

    #[test]
    fn extract_js_urls_stops_at_quote_and_whitespace_boundaries() {
        // URL scan must not leak past the attribute-value quote, even
        // when a later `.js` exists further down the document.
        let html =
            r#"<script src="https://a-v2.sndcdn.com/assets/x.js"></script><span>other.js</span>"#;
        let urls = extract_js_urls(html);
        assert_eq!(urls, vec!["https://a-v2.sndcdn.com/assets/x.js".to_string()]);
    }

    #[test]
    fn extract_client_id_finds_literal() {
        let js = r#"foo,client_id:"AbCdEf123",bar:"baz""#;
        assert_eq!(extract_client_id(js).as_deref(), Some("AbCdEf123"));
    }

    #[test]
    fn extract_client_id_none_when_marker_absent() {
        assert_eq!(extract_client_id("no marker here"), None);
    }

    #[test]
    fn extract_client_id_rejects_empty_value() {
        assert_eq!(extract_client_id(r#"client_id:"""#), None);
    }

    #[test]
    fn extract_client_id_rejects_non_alphanumeric_value() {
        // If the bundle's variable layout changes and we match something
        // that isn't the raw id, bail out.
        assert_eq!(extract_client_id(r#"client_id:"ab!cd""#), None);
    }
}
