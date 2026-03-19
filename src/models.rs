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
    /// Archivos hasheados parcialmente por superar el umbral de tamaño.
    /// La deduplicación es aproximada para estos: ver verify.
    pub partial_hashes:  usize,
}

impl ScanStats {
    pub fn total_indexed(&self) -> usize {
        self.indexed_3d + self.indexed_video + self.indexed_audio
            + self.indexed_image + self.indexed_other
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    // ── MediaType::from_extension ─────────────────────────────────────────

    #[test]
    fn from_extension_imagen() {
        assert_eq!(MediaType::from_extension("jpg"),  Some(MediaType::Image));
        assert_eq!(MediaType::from_extension("JPG"),  Some(MediaType::Image));  // case insensitive
        assert_eq!(MediaType::from_extension("jpeg"), Some(MediaType::Image));
        assert_eq!(MediaType::from_extension("png"),  Some(MediaType::Image));
        assert_eq!(MediaType::from_extension("webp"), Some(MediaType::Image));
        assert_eq!(MediaType::from_extension("heic"), Some(MediaType::Image));
        assert_eq!(MediaType::from_extension("cr2"),  Some(MediaType::Image));
    }

    #[test]
    fn from_extension_audio() {
        assert_eq!(MediaType::from_extension("mp3"),  Some(MediaType::Audio));
        assert_eq!(MediaType::from_extension("flac"), Some(MediaType::Audio));
        assert_eq!(MediaType::from_extension("ogg"),  Some(MediaType::Audio));
        assert_eq!(MediaType::from_extension("wav"),  Some(MediaType::Audio));
        assert_eq!(MediaType::from_extension("m4a"),  Some(MediaType::Audio));
    }

    #[test]
    fn from_extension_video() {
        assert_eq!(MediaType::from_extension("mp4"),  Some(MediaType::Video));
        assert_eq!(MediaType::from_extension("mkv"),  Some(MediaType::Video));
        assert_eq!(MediaType::from_extension("avi"),  Some(MediaType::Video));
        assert_eq!(MediaType::from_extension("mov"),  Some(MediaType::Video));
        assert_eq!(MediaType::from_extension("webm"), Some(MediaType::Video));
    }

    #[test]
    fn from_extension_3d() {
        assert_eq!(MediaType::from_extension("stl"), Some(MediaType::Print3D));
        assert_eq!(MediaType::from_extension("obj"), Some(MediaType::Print3D));
        assert_eq!(MediaType::from_extension("3mf"), Some(MediaType::Print3D));
        assert_eq!(MediaType::from_extension("STL"), Some(MediaType::Print3D));
    }

    #[test]
    fn from_extension_unknown_is_none() {
        assert_eq!(MediaType::from_extension("txt"),  None);
        assert_eq!(MediaType::from_extension("exe"),  None);
        assert_eq!(MediaType::from_extension("pdf"),  None);
        assert_eq!(MediaType::from_extension(""),     None);
        assert_eq!(MediaType::from_extension("zip"),  None);
    }

    // ── MediaType::as_str / from_str ─────────────────────────────────────

    #[test]
    fn as_str_from_str_roundtrip() {
        for mt in [
            MediaType::Print3D,
            MediaType::Video,
            MediaType::Audio,
            MediaType::Image,
            MediaType::Other,
        ] {
            assert_eq!(MediaType::from_str(mt.as_str()), mt);
        }
    }

    #[test]
    fn from_str_unknown_es_other() {
        assert_eq!(MediaType::from_str("desconocido"), MediaType::Other);
        assert_eq!(MediaType::from_str(""),            MediaType::Other);
    }

    // ── ArchiveType::from_path ────────────────────────────────────────────

    #[test]
    fn archive_type_zip() {
        assert_eq!(ArchiveType::from_path(Path::new("archivo.zip")), Some(ArchiveType::Zip));
        assert_eq!(ArchiveType::from_path(Path::new("/ruta/al/fondo.ZIP")), Some(ArchiveType::Zip));
    }

    #[test]
    fn archive_type_7z() {
        assert_eq!(ArchiveType::from_path(Path::new("archivo.7z")),    Some(ArchiveType::SevenZip));
        assert_eq!(ArchiveType::from_path(Path::new("archivo.7z.001")), Some(ArchiveType::SevenZip));
        assert_eq!(ArchiveType::from_path(Path::new("archivo.7z.099")), Some(ArchiveType::SevenZip));
    }

    #[test]
    fn archive_type_rar() {
        assert_eq!(ArchiveType::from_path(Path::new("archivo.rar")),       Some(ArchiveType::Rar));
        assert_eq!(ArchiveType::from_path(Path::new("archivo.part1.rar")), Some(ArchiveType::Rar));
        assert_eq!(ArchiveType::from_path(Path::new("archivo.part10.rar")),Some(ArchiveType::Rar));
    }

    #[test]
    fn archive_type_none() {
        assert_eq!(ArchiveType::from_path(Path::new("archivo.mp4")), None);
        assert_eq!(ArchiveType::from_path(Path::new("archivo.txt")), None);
        assert_eq!(ArchiveType::from_path(Path::new("Makefile")),    None);
    }

    // ── is_7z_multipart / is_rar_multipart ───────────────────────────────

    #[test]
    fn multipart_7z() {
        assert!(is_7z_multipart("backup.7z.001"));
        assert!(is_7z_multipart("backup.7z.999"));
        assert!(!is_7z_multipart("backup.7z"));
        assert!(!is_7z_multipart("backup.7z.abc")); // letras, no dígitos
    }

    #[test]
    fn multipart_rar() {
        assert!(is_rar_multipart("backup.part1.rar"));
        assert!(is_rar_multipart("backup.part99.rar"));
        assert!(!is_rar_multipart("backup.rar"));           // sin .part
        assert!(!is_rar_multipart("backup.part1.zip"));     // extensión distinta
    }

    // ── ScanStats::total_indexed ──────────────────────────────────────────

    #[test]
    fn total_indexed_suma_todos_los_tipos() {
        let s = ScanStats {
            indexed_3d:    1,
            indexed_video: 2,
            indexed_audio: 3,
            indexed_image: 4,
            indexed_other: 5,
            ..Default::default()
        };
        assert_eq!(s.total_indexed(), 15);
    }

    #[test]
    fn total_indexed_vacio_es_cero() {
        assert_eq!(ScanStats::default().total_indexed(), 0);
    }
}
