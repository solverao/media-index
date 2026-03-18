pub mod audio;
pub mod video;
pub mod image;
pub mod print3d;

use crate::models::{Metadata, MediaType};

/// Elige el parser según el tipo de medio y la extensión
pub fn parse(data: &[u8], ext: &str, media_type: &MediaType, path: &str) -> Metadata {
    match media_type {
        MediaType::Print3D => Metadata::Print3D(print3d::parse(data, ext)),
        MediaType::Audio   => Metadata::Audio(audio::parse(data, ext)),
        MediaType::Video   => Metadata::Video(video::parse_from_path(path)),
        MediaType::Image   => Metadata::Image(image::parse(data)),
        MediaType::Other   => Metadata::None,
    }
}
