use std::process::Command;
use crate::models::MetaVideo;

/// Extrae metadatos de video usando `ffprobe` (parte de FFmpeg).
///
/// ffprobe es el estándar de facto y soporta todos los contenedores:
/// MKV, MP4, AVI, MOV, WebM, TS, MTS, VOB, etc.
///
/// Si ffprobe no está disponible, retorna MetaVideo::default() sin error fatal.
pub fn parse_from_path(path: &str) -> MetaVideo {
    let mut meta = MetaVideo::default();

    // Verificar disponibilidad de ffprobe (caché implícita por el OS)
    let output = Command::new("ffprobe")
        .args([
            "-v", "quiet",
            "-print_format", "json",
            "-show_format",
            "-show_streams",
            path,
        ])
        .output();

    let output = match output {
        Ok(o) if o.status.success() => o,
        Ok(_) => return meta,  // ffprobe falló (archivo corrupto, etc.)
        Err(_) => {
            // ffprobe no instalado — continuar sin metadatos de video
            return meta;
        }
    };

    let json: serde_json::Value = match serde_json::from_slice(&output.stdout) {
        Ok(v)  => v,
        Err(_) => return meta,
    };

    // ── Format (info del contenedor) ──────────────────────────────────────
    if let Some(fmt) = json.get("format") {
        meta.duration_secs = fmt["duration"]
            .as_str()
            .and_then(|s| s.parse::<f64>().ok());

        meta.bitrate_kbps = fmt["bit_rate"]
            .as_str()
            .and_then(|s| s.parse::<u64>().ok())
            .map(|bps| bps / 1000);

        meta.container = fmt["format_name"]
            .as_str()
            .map(|s| s.split(',').next().unwrap_or(s).to_string());

        // Tags del contenedor (título, año…)
        if let Some(tags) = fmt.get("tags") {
            meta.title = tags["title"].as_str()
                .or_else(|| tags["TITLE"].as_str())
                .map(str::to_string);

            meta.year = tags["date"].as_str()
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
                    meta.width      = stream["width"].as_u64().map(|v| v as u32);
                    meta.height     = stream["height"].as_u64().map(|v| v as u32);
                    meta.codec_video = stream["codec_name"].as_str().map(str::to_string);

                    // FPS: viene como fracción "24000/1001" o "30/1"
                    meta.fps = stream["r_frame_rate"]
                        .as_str()
                        .and_then(parse_fraction);
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

/// Parsea fracciones tipo "24000/1001" → 23.976
fn parse_fraction(s: &str) -> Option<f64> {
    let mut parts = s.splitn(2, '/');
    let num: f64 = parts.next()?.parse().ok()?;
    let den: f64 = parts.next()?.parse().ok()?;
    if den == 0.0 { return None; }
    Some(num / den)
}

/// Verifica si ffprobe está disponible en el PATH
pub fn ffprobe_available() -> bool {
    Command::new("ffprobe")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
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
