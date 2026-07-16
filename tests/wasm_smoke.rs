//! Real ABI smoke test for the release SoundCloud WASM artifact.

use std::path::PathBuf;
use std::sync::Mutex;

use extism::{Function, UserData, Val, PTR};
use serde_json::{json, Value};

const WASM_REL_PATH: &str = "target/wasm32-wasip1/release/vortex_mod_soundcloud.wasm";
const TRACK_URL: &str = "https://soundcloud.com/vortex/demo-track";
const PLAYLIST_URL: &str = "https://soundcloud.com/vortex/sets/demo-playlist";
const STREAM_API_URL: &str = "https://api-v2.soundcloud.com/media/demo/progressive";
const STREAM_CDN_URL: &str = "https://cdn.example.test/demo-track.mp3";
const DOWNLOAD_DIR: &str = "/tmp/vortex-downloads/job";
const DOWNLOAD_PATH: &str = "/tmp/vortex-downloads/job/demo-track.mp3";
const TRACK_BODY: &str = r#"{"kind":"track","id":42,"title":"Demo Track","duration":123000,"permalink_url":"https://soundcloud.com/vortex/demo-track","artwork_url":"https://i1.sndcdn.com/artworks-demo-large.jpg","user":{"username":"Vortex"},"media":{"transcodings":[{"url":"https://api-v2.soundcloud.com/media/demo/progressive","format":{"protocol":"progressive","mime_type":"audio/mpeg"},"quality":"sq"}]}}"#;
const PLAYLIST_BODY: &str = r#"{"kind":"playlist","id":7,"title":"Demo Playlist","permalink_url":"https://soundcloud.com/vortex/sets/demo-playlist","tracks":[{"id":42,"title":"Demo Track","duration":123000,"permalink_url":"https://soundcloud.com/vortex/demo-track","user":{"username":"Vortex"}}]}"#;

static YTDLP_REQUESTS: Mutex<Vec<Value>> = Mutex::new(Vec::new());

fn wasm_path() -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(WASM_REL_PATH);
    assert!(
        path.is_file(),
        "missing release WASM artifact at {}; run `cargo build --target wasm32-wasip1 --release` first",
        path.display()
    );
    path
}

fn stub_http_request() -> Function {
    Function::new(
        "http_request",
        [PTR],
        [PTR],
        UserData::<()>::default(),
        |plugin, inputs, outputs, _user_data: UserData<()>| {
            let input = inputs[0]
                .i64()
                .ok_or_else(|| extism::Error::msg("http_request expected i64 input"))?;
            let request: String = plugin.memory_get_val(&Val::I64(input))?;
            let request: Value = serde_json::from_str(&request)?;
            let url = request["url"]
                .as_str()
                .ok_or_else(|| extism::Error::msg("http_request URL is missing"))?;
            let body = if url.contains("/resolve?") && url.contains("%2Fsets%2F") {
                PLAYLIST_BODY
            } else if url.contains("/resolve?") {
                TRACK_BODY
            } else if url.starts_with(STREAM_API_URL) {
                r#"{"url":"https://cdn.example.test/demo-track.mp3"}"#
            } else {
                return Err(extism::Error::msg(format!(
                    "unexpected HTTP request URL: {url}"
                )));
            };
            let response = json!({ "status": 200, "headers": {}, "body": body }).to_string();
            let handle = plugin.memory_new(&response)?;
            outputs[0] = Val::I64(handle.offset() as i64);
            Ok(())
        },
    )
}

fn stub_get_config() -> Function {
    Function::new(
        "get_config",
        [PTR],
        [PTR],
        UserData::<()>::default(),
        |plugin, inputs, outputs, _user_data: UserData<()>| {
            let input = inputs[0]
                .i64()
                .ok_or_else(|| extism::Error::msg("get_config expected i64 input"))?;
            let key: String = plugin.memory_get_val(&Val::I64(input))?;
            if key != "client_id" {
                return Err(extism::Error::msg(format!("unexpected config key: {key}")));
            }
            let handle = plugin.memory_new("test-client-id")?;
            outputs[0] = Val::I64(handle.offset() as i64);
            Ok(())
        },
    )
}

fn stub_set_config() -> Function {
    Function::new(
        "set_config",
        [PTR],
        [],
        UserData::<()>::default(),
        |_plugin, _inputs, _outputs, _user_data: UserData<()>| Ok(()),
    )
}

fn stub_run_ytdlp() -> Function {
    Function::new(
        "run_ytdlp",
        [PTR],
        [PTR],
        UserData::<()>::default(),
        |plugin, inputs, outputs, _user_data: UserData<()>| {
            let input = inputs[0]
                .i64()
                .ok_or_else(|| extism::Error::msg("run_ytdlp expected i64 input"))?;
            let request: String = plugin.memory_get_val(&Val::I64(input))?;
            let request: Value = serde_json::from_str(&request)?;
            if ["binary", "args", "timeout_ms"]
                .iter()
                .any(|field| request.get(field).is_some())
            {
                return Err(extism::Error::msg("plugin exposed process controls"));
            }
            YTDLP_REQUESTS
                .lock()
                .map_err(|_| extism::Error::msg("request capture mutex poisoned"))?
                .push(request);
            let response = json!({
                "exit_code": 0,
                "stdout": format!("{DOWNLOAD_PATH}\n"),
                "stderr": ""
            })
            .to_string();
            let handle = plugin.memory_new(&response)?;
            outputs[0] = Val::I64(handle.offset() as i64);
            Ok(())
        },
    )
}

fn load_plugin() -> extism::Plugin {
    let manifest = extism::Manifest::new([extism::Wasm::file(wasm_path())]);
    extism::Plugin::new(
        &manifest,
        [
            stub_http_request(),
            stub_get_config(),
            stub_set_config(),
            stub_run_ytdlp(),
        ],
        true,
    )
    .expect("load SoundCloud release WASM")
}

#[test]
fn test_release_wasm_exports_and_typed_broker_contract() {
    YTDLP_REQUESTS.lock().unwrap().clear();
    let mut plugin = load_plugin();

    let can_handle: String = plugin.call("can_handle", TRACK_URL).expect("can_handle");
    let supports_playlist: String = plugin
        .call("supports_playlist", PLAYLIST_URL)
        .expect("supports_playlist");
    let links: String = plugin
        .call("extract_links", TRACK_URL)
        .expect("extract_links");
    let track: String = plugin
        .call("extract_track", TRACK_URL)
        .expect("extract_track");
    let playlist: String = plugin
        .call("extract_playlist", PLAYLIST_URL)
        .expect("extract_playlist");
    let stream: String = plugin
        .call(
            "resolve_stream_url",
            json!({ "url": TRACK_URL, "format": "mp3", "audio_only": true }).to_string(),
        )
        .expect("resolve_stream_url");
    let path: String = plugin
        .call(
            "download_to_file",
            json!({ "url": TRACK_URL, "format": "mp3", "output_dir": DOWNLOAD_DIR }).to_string(),
        )
        .expect("download_to_file");

    assert_eq!(can_handle.trim(), "true");
    assert_eq!(supports_playlist.trim(), "true");
    assert_eq!(
        serde_json::from_str::<Value>(&links).unwrap()["kind"],
        "track"
    );
    assert_eq!(
        serde_json::from_str::<Value>(&track).unwrap()["kind"],
        "track"
    );
    assert_eq!(
        serde_json::from_str::<Value>(&playlist).unwrap()["kind"],
        "playlist"
    );
    assert_eq!(stream, STREAM_CDN_URL);
    assert_eq!(path, DOWNLOAD_PATH);
    assert_eq!(
        YTDLP_REQUESTS.lock().unwrap().as_slice(),
        &[json!({
            "action": "download",
            "url": TRACK_URL,
            "quality": null,
            "format": "mp3",
            "output_dir": DOWNLOAD_DIR,
            "audio_only": true
        })]
    );
}
