use crate::models::MetaImage;

pub fn parse(data: &[u8]) -> MetaImage {
    let mut meta = MetaImage::default();

    // ── Dimensiones ───────────────────────────────────────────────────────
    // image::io::Reader está deprecado desde image 0.25 → usar ImageReader
    if let Ok(reader) = image::ImageReader::new(std::io::Cursor::new(data))
        .with_guessed_format()
    {
        if let Ok((w, h)) = reader.into_dimensions() {
            meta.width  = Some(w);
            meta.height = Some(h);
        }
    }

    // ── EXIF ──────────────────────────────────────────────────────────────
    // La crate se llama `kamadak-exif` en crates.io pero su lib name es `exif`
    parse_exif(data, &mut meta);

    meta
}

fn parse_exif(data: &[u8], meta: &mut MetaImage) {
    use exif::{Reader as ExifReader, Tag, In, Value};

    let exif: exif::Exif = match ExifReader::new()
        .read_from_container(&mut std::io::Cursor::new(data))
    {
        Ok(e)  => e,
        Err(_) => return, // Sin EXIF — normal en PNG, WebP, etc.
    };

    // Helper: obtener string ASCII de un tag EXIF
    let get_str = |tag: Tag| -> Option<String> {
        exif.get_field(tag, In::PRIMARY)
            .and_then(|f| match &f.value {
                Value::Ascii(s) => s.first()
                    .map(|b| String::from_utf8_lossy(b).trim().to_string()),
                _ => None,
            })
            .filter(|s: &String| !s.is_empty())
    };

    // Helper: obtener u32
    let get_u32 = |tag: Tag| -> Option<u32> {
        exif.get_field(tag, In::PRIMARY)
            .and_then(|f| match &f.value {
                Value::Short(v)    => v.first().map(|&x| x as u32),
                Value::Long(v)     => v.first().copied(),
                Value::Rational(v) => v.first().map(|r| rational_f64(r) as u32),
                _ => None,
            })
    };

    // Helper: obtener f64 desde Rational o SRational
    let get_f64 = |tag: Tag| -> Option<f64> {
        exif.get_field(tag, In::PRIMARY)
            .and_then(|f| match &f.value {
                Value::Rational(v)  => v.first().map(|r| rational_f64(r)),
                Value::SRational(v) => v.first().map(|r| srational_f64(r)),
                _ => None,
            })
    };

    meta.camera_make  = get_str(Tag::Make);
    meta.camera_model = get_str(Tag::Model);
    meta.taken_at     = get_str(Tag::DateTimeOriginal)
        .or_else(|| get_str(Tag::DateTime));
    meta.iso          = get_u32(Tag::PhotographicSensitivity);
    meta.focal_length = get_f64(Tag::FocalLength);

    // ── GPS ───────────────────────────────────────────────────────────────
    meta.gps_lat = parse_gps(&exif, Tag::GPSLatitude,  Tag::GPSLatitudeRef);
    meta.gps_lon = parse_gps(&exif, Tag::GPSLongitude, Tag::GPSLongitudeRef);
}

fn parse_gps(exif: &exif::Exif, tag_dms: exif::Tag, tag_ref: exif::Tag) -> Option<f64> {
    use exif::{In, Value};

    let dms = exif.get_field(tag_dms, In::PRIMARY)?;
    let rationals = match &dms.value {
        Value::Rational(v) if v.len() >= 3 => v,
        _ => return None,
    };

    let deg = rational_f64(&rationals[0]);
    let min = rational_f64(&rationals[1]);
    let sec = rational_f64(&rationals[2]);
    let mut decimal = deg + min / 60.0 + sec / 3600.0;

    if let Some(ref_field) = exif.get_field(tag_ref, In::PRIMARY) {
        let ref_str = match &ref_field.value {
            Value::Ascii(s) => s.first()
                .map(|b| String::from_utf8_lossy(b).to_uppercase())
                .unwrap_or_default(),
            _ => String::new(),
        };
        if ref_str.starts_with('S') || ref_str.starts_with('W') {
            decimal = -decimal;
        }
    }

    Some(decimal)
}

// Funciones libres en vez de trait impl (más simples, evitan ambigüedades)
fn rational_f64(r: &exif::Rational) -> f64 {
    if r.denom == 0 { 0.0 } else { r.num as f64 / r.denom as f64 }
}

fn srational_f64(r: &exif::SRational) -> f64 {
    if r.denom == 0 { 0.0 } else { r.num as f64 / r.denom as f64 }
}
