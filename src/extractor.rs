//! Typed yt-dlp broker helpers for SoundCloud download fallback.

use serde::{Deserialize, Serialize};

use crate::error::PluginError;

#[derive(Debug, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum YtDlpRequest {
    Download {
        url: String,
        quality: Option<u32>,
        format: Option<String>,
        output_dir: String,
        audio_only: bool,
    },
}

#[derive(Debug, Deserialize)]
pub struct YtDlpResponse {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

pub fn build_ytdlp_download_request(url: &str, format: &str, output_dir: &str) -> YtDlpRequest {
    YtDlpRequest::Download {
        url: url.to_string(),
        quality: None,
        format: Some(normalize_audio_format(format).to_string()),
        output_dir: output_dir.to_string(),
        audio_only: true,
    }
}

pub fn parse_ytdlp_response(response_json: &str) -> Result<String, PluginError> {
    let resp: YtDlpResponse = serde_json::from_str(response_json)?;
    if resp.exit_code != 0 {
        return Err(PluginError::Subprocess {
            exit_code: resp.exit_code,
            stderr: truncate_stderr(&resp.stderr),
        });
    }
    Ok(resp.stdout)
}

pub fn parse_download_path_from_stdout(stdout: &str) -> Result<String, PluginError> {
    stdout
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string)
        .ok_or(PluginError::NoStreamAvailable)
}

fn normalize_audio_format(format: &str) -> &str {
    match format {
        "aac" => "m4a",
        "ogg" => "vorbis",
        "best" | "flac" | "m4a" | "mp3" | "opus" | "vorbis" | "wav" => format,
        _ => "mp3",
    }
}

fn truncate_stderr(stderr: &str) -> String {
    const MAX_CHARS: usize = 512;
    let trimmed = stderr.trim();
    if trimmed.chars().count() <= MAX_CHARS {
        trimmed.to_string()
    } else {
        let mut out: String = trimmed.chars().take(MAX_CHARS).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn download_request_serializes_only_typed_fields() {
        let request = build_ytdlp_download_request(
            "https://soundcloud.com/forss/flickermood",
            "mp3",
            "/tmp/100%done",
        );
        let value = serde_json::to_value(request).unwrap();

        assert_eq!(
            value,
            serde_json::json!({
                "action": "download",
                "url": "https://soundcloud.com/forss/flickermood",
                "quality": null,
                "format": "mp3",
                "output_dir": "/tmp/100%done",
                "audio_only": true,
            })
        );
    }

    #[test]
    fn normalize_audio_format_maps_ogg_to_vorbis() {
        assert_eq!(normalize_audio_format("ogg"), "vorbis");
        assert_eq!(normalize_audio_format("aac"), "m4a");
        assert_eq!(normalize_audio_format("garbage"), "mp3");
    }

    #[test]
    fn parse_download_path_uses_last_non_empty_line() {
        let stdout = "\n[download] done\n/tmp/out.mp3\n";
        assert_eq!(
            parse_download_path_from_stdout(stdout).unwrap(),
            "/tmp/out.mp3"
        );
    }

    #[test]
    fn download_request_defaults_unknown_format_to_mp3() {
        let request = build_ytdlp_download_request(
            "https://soundcloud.com/forss/flickermood",
            "unknown",
            "/tmp/vx",
        );
        let value = serde_json::to_value(request).unwrap();

        assert_eq!(value["format"], "mp3");
    }
}
