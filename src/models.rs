use serde::{Deserialize, Serialize};

// ── Tipo de medio ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MediaType {
    Print3D,
    Video,
    Audio,
    Image,
    Other,
}

impl MediaType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Print3D => "3d",
            Self::Video   => "video",
            Self::Audio   => "audio",
            Self::Image   => "image",
            Self::Other   => "other",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "3d"    => Self::Print3D,
            "video" => Self::Video,
            "audio" => Self::Audio,
            "image" => Self::Image,
            _       => Self::Other,
        }
    }

    /// Detectar tipo por extensión
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext.to_lowercase().as_str() {
            // 3D
            "stl" | "obj" | "3mf" => Some(Self::Print3D),
            // Video
            "mp4" | "mkv" | "avi" | "mov" | "wmv" | "flv" | "webm"
            | "m4v" | "mpg" | "mpeg" | "ts" | "mts" | "m2ts" | "vob"
            | "divx" | "xvid" | "rmvb" | "3gp" => Some(Self::Video),
            // Audio
            "mp3" | "flac" | "ogg" | "opus" | "m4a" | "aac" | "wav"
            | "aiff" | "aif" | "wma" | "ape" | "wv" | "mka" | "alac"
            | "dsf" | "dff" => Some(Self::Audio),
            // Imagen
            "jpg" | "jpeg" | "png" | "webp" | "tiff" | "tif" | "bmp"
            | "gif" | "avif" | "heic" | "heif" | "raw" | "cr2" | "cr3"
            | "nef" | "arw" | "dng" | "orf" | "rw2" | "psd" | "xcf" => Some(Self::Image),
            _ => None,
        }
    }
}

// ── Entrada genérica ──────────────────────────────────────────────────────

/// Un archivo indexado de cualquier tipo
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaEntry {
    pub blake3_hash:     String,
    pub size_bytes:      u64,
    pub original_name:   String,
    pub current_path:    String,
    pub extension:       String,
    pub media_type:      MediaType,
    pub metadata:        Metadata,
    /// Origen si vino de un comprimido
    pub source_archive:  Option<String>,
    pub path_in_archive: Option<String>,
}

// ── Metadatos por tipo ────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Metadata {
    Print3D(Meta3D),
    Video(MetaVideo),
    Audio(MetaAudio),
    Image(MetaImage),
    None,
}

// ── 3D ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Meta3D {
    pub format:         String, // stl | obj | 3mf
    pub triangle_count: Option<u64>,
    pub vertex_count:   Option<u64>,
    pub object_count:   Option<u32>,
    pub dim_x:          Option<f64>,
    pub dim_y:          Option<f64>,
    pub dim_z:          Option<f64>,
}

// ── Video ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MetaVideo {
    pub duration_secs:  Option<f64>,
    pub width:          Option<u32>,
    pub height:         Option<u32>,
    pub codec_video:    Option<String>,
    pub codec_audio:    Option<String>,
    pub bitrate_kbps:   Option<u64>,
    pub fps:            Option<f64>,
    /// Tags embebidos
    pub title:          Option<String>,
    pub year:           Option<u32>,
    pub container:      Option<String>,
}

// ── Audio ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MetaAudio {
    pub duration_secs:  Option<f64>,
    pub bitrate_kbps:   Option<u32>,
    pub sample_rate_hz: Option<u32>,
    pub channels:       Option<u8>,
    pub title:          Option<String>,
    pub artist:         Option<String>,
    pub album:          Option<String>,
    pub year:           Option<u32>,
    pub genre:          Option<String>,
    pub track_number:   Option<u32>,
}

// ── Imagen ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MetaImage {
    pub width:        Option<u32>,
    pub height:       Option<u32>,
    pub color_space:  Option<String>,
    pub has_alpha:    Option<bool>,
    // EXIF
    pub camera_make:  Option<String>,
    pub camera_model: Option<String>,
    pub taken_at:     Option<String>,
    pub gps_lat:      Option<f64>,
    pub gps_lon:      Option<f64>,
    pub iso:          Option<u32>,
    pub focal_length: Option<f64>,
}

// ── Comprimidos ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum ArchiveType {
    Zip,
    SevenZip,
    Rar,
}

impl ArchiveType {
    pub fn from_path(path: &std::path::Path) -> Option<Self> {
        let name = path.file_name()?.to_string_lossy().to_lowercase();
        if name.ends_with(".zip") {
            return Some(Self::Zip);
        }
        if name.ends_with(".7z") || is_7z_multipart(&name) {
            return Some(Self::SevenZip);
        }
        if name.ends_with(".rar") || is_rar_multipart(&name) {
            return Some(Self::Rar);
        }
        None
    }
}

pub fn is_7z_multipart(name: &str) -> bool {
    let tail = name.rsplit('.').next().unwrap_or("");
    name.contains(".7z.") && tail.chars().all(|c| c.is_ascii_digit())
}

pub fn is_rar_multipart(name: &str) -> bool {
    name.contains(".part") && name.ends_with(".rar")
}

// ── Stats de escaneo ──────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct ScanStats {
    pub indexed_3d:      usize,
    pub indexed_video:   usize,
    pub indexed_audio:   usize,
    pub indexed_image:   usize,
    pub indexed_other:   usize,
    pub archives_opened: usize,
    pub duplicates:      usize,
    pub bytes_dup:       u64,
    pub errors:          usize,
}

impl ScanStats {
    pub fn total_indexed(&self) -> usize {
        self.indexed_3d + self.indexed_video + self.indexed_audio
            + self.indexed_image + self.indexed_other
    }
}
