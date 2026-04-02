use crate::models::MetaVideo;
use std::process::Command;

/// Extracts video metadata using `ffprobe` (part of FFmpeg).
///
/// ffprobe is the de-facto standard and supports all containers:
/// MKV, MP4, AVI, MOV, WebM, TS, MTS, VOB, etc.
///
/// If ffprobe is not available, returns MetaVideo::default() without a fatal error.
pub fn parse_from_path(path: &str) -> MetaVideo {
    let mut meta = MetaVideo::default();

    // Check ffprobe availability (OS-level implicit cache)
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "quiet",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
            path,
        ])
        .output();

    let output = match output {
        Ok(o) if o.status.success() => o,
        Ok(_) => return meta, // ffprobe failed (corrupt file, etc.)
        Err(_) => {
            // ffprobe not installed — continue without video metadata
            return meta;
        }
    };

    let json: serde_json::Value = match serde_json::from_slice(&output.stdout) {
        Ok(v) => v,
        Err(_) => return meta,
    };

    // ── Format (container info) ───────────────────────────────────────────
    if let Some(fmt) = json.get("format") {
        meta.duration_secs = fmt["duration"].as_str().and_then(|s| s.parse::<f64>().ok());

        meta.bitrate_kbps = fmt["bit_rate"]
            .as_str()
            .and_then(|s| s.parse::<u64>().ok())
            .map(|bps| bps / 1000);

        meta.container = fmt["format_name"]
            .as_str()
            .map(|s| s.split(',').next().unwrap_or(s).to_string());

        // Container tags (title, year…)
        if let Some(tags) = fmt.get("tags") {
            meta.title = tags["title"]
                .as_str()
                .or_else(|| tags["TITLE"].as_str())
                .map(str::to_string);

            meta.year = tags["date"]
                .as_str()
                .or_else(|| tags["DATE"].as_str())
                .and_then(|s| s.get(..4))
                .and_then(|s| s.parse::<u32>().ok());
        }
    }

    // ── Streams ───────────────────────────────────────────────────────────
    if let Some(streams) = json["streams"].as_array() {
        for stream in streams {
            let codec_type = stream["codec_type"].as_str().unwrap_or("");

            match codec_type {
                "video" if meta.width.is_none() => {
                    meta.width = stream["width"].as_u64().map(|v| v as u32);
                    meta.height = stream["height"].as_u64().map(|v| v as u32);
                    meta.codec_video = stream["codec_name"].as_str().map(str::to_string);

                    // FPS: comes as a fraction "24000/1001" or "30/1"
                    meta.fps = stream["r_frame_rate"].as_str().and_then(parse_fraction);
                }
                "audio" if meta.codec_audio.is_none() => {
                    meta.codec_audio = stream["codec_name"].as_str().map(str::to_string);
                }
                _ => {}
            }
        }
    }

    meta
}

/// Parses fractions like "24000/1001" → 23.976
fn parse_fraction(s: &str) -> Option<f64> {
    let mut parts = s.splitn(2, '/');
    let num: f64 = parts.next()?.parse().ok()?;
    let den: f64 = parts.next()?.parse().ok()?;
    if den == 0.0 {
        return None;
    }
    Some(num / den)
}

/// Checks if ffprobe is available in PATH.
/// Result is cached after the first check (Fix #18).
pub fn ffprobe_available() -> bool {
    static AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        Command::new("ffprobe")
            .arg("-version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_fraction_works() {
        assert!((parse_fraction("24000/1001").unwrap() - 23.976).abs() < 0.01);
        assert!((parse_fraction("30/1").unwrap() - 30.0).abs() < 0.01);
        assert!(parse_fraction("0/0").is_none());
    }
}
