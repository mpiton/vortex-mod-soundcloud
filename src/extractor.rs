//! yt-dlp subprocess helpers for SoundCloud download fallback.

use serde::{Deserialize, Serialize};

use crate::error::PluginError;

#[derive(Debug, Serialize)]
pub struct SubprocessRequest {
    pub binary: String,
    pub args: Vec<String>,
    pub timeout_ms: u64,
}

#[derive(Debug, Deserialize)]
pub struct SubprocessResponse {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

pub const DEFAULT_DOWNLOAD_TIMEOUT_MS: u64 = 1_800_000;

pub fn yt_dlp_args_for_download_to_file(url: &str, format: &str, output_dir: &str) -> Vec<String> {
    let audio_format = normalize_audio_format(format);
    // yt-dlp treats `%(...)s` in the -o template as a format specifier, so a
    // literal `%` in output_dir (e.g. "/tmp/100%done/") would either fail or
    // silently redirect the download to an unintended path. Doubling `%` is
    // the documented escape.
    let sanitized_dir = output_dir.replace('%', "%%");
    let output_template = format!("{sanitized_dir}/%(id)s.%(ext)s");

    vec![
        "--extract-audio".into(),
        "--audio-format".into(),
        audio_format.into(),
        "--output".into(),
        output_template,
        "--print".into(),
        "after_move:%(filepath)s".into(),
        "--no-playlist".into(),
        "--no-warnings".into(),
        "--quiet".into(),
        "--".into(),
        url.into(),
    ]
}

pub fn parse_subprocess_response(response_json: &str) -> Result<String, PluginError> {
    let resp: SubprocessResponse = serde_json::from_str(response_json)?;
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
    fn download_args_include_extract_audio() {
        let args = yt_dlp_args_for_download_to_file(
            "https://soundcloud.com/forss/flickermood",
            "mp3",
            "/tmp/vx",
        );
        assert!(args.contains(&"--extract-audio".into()));
        assert!(args.contains(&"--audio-format".into()));
        assert!(args.contains(&"mp3".into()));
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
    fn download_args_escape_percent_in_output_dir() {
        // A directory containing `%` must be escaped as `%%` so yt-dlp
        // doesn't interpret it as the start of a format specifier.
        let args = yt_dlp_args_for_download_to_file(
            "https://soundcloud.com/forss/flickermood",
            "mp3",
            "/tmp/100%done",
        );
        let output_idx = args.iter().position(|a| a == "--output").unwrap();
        let template = &args[output_idx + 1];
        assert_eq!(template, "/tmp/100%%done/%(id)s.%(ext)s");
    }

    #[test]
    fn download_args_leave_clean_dir_untouched() {
        let args = yt_dlp_args_for_download_to_file(
            "https://soundcloud.com/forss/flickermood",
            "mp3",
            "/tmp/vx",
        );
        let output_idx = args.iter().position(|a| a == "--output").unwrap();
        assert_eq!(&args[output_idx + 1], "/tmp/vx/%(id)s.%(ext)s");
    }
}
