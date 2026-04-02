pub mod audio;
pub mod image;
pub mod print3d;
pub mod video;

use crate::models::{MediaType, Metadata};

/// Picks the appropriate parser based on the media type and file extension
pub fn parse(data: &[u8], ext: &str, media_type: &MediaType, path: &str) -> Metadata {
    match media_type {
        MediaType::Print3D => Metadata::Print3D(print3d::parse(data, ext)),
        MediaType::Audio => Metadata::Audio(audio::parse(data, ext)),
        MediaType::Video => Metadata::Video(video::parse_from_path(path)),
        MediaType::Image => Metadata::Image(image::parse(data)),
        MediaType::Other => Metadata::None,
    }
}
