use crate::models::Meta3D;

pub fn parse(data: &[u8], ext: &str) -> Meta3D {
    let mut meta = match ext.to_lowercase().as_str() {
        "stl" => parse_stl(data),
        "obj" => parse_obj(data),
        "3mf" => parse_3mf(data),
        _     => Meta3D::default(),
    };
    meta.format = ext.to_lowercase();
    meta
}

// ── STL ───────────────────────────────────────────────────────────────────

fn parse_stl(data: &[u8]) -> Meta3D {
    let mut meta = Meta3D::default();
    if data.len() < 84 { return meta; }

    let is_ascii = data.starts_with(b"solid ")
        && std::str::from_utf8(&data[..data.len().min(256)]).is_ok();

    if is_ascii {
        let text = String::from_utf8_lossy(data);
        meta.triangle_count = Some(text.matches("facet normal").count() as u64);
    } else {
        let tri = u32::from_le_bytes([data[80], data[81], data[82], data[83]]) as u64;
        if data.len() as u64 >= 84 + tri * 50 {
            meta.triangle_count = Some(tri);
        }
    }
    meta
}

// ── OBJ ───────────────────────────────────────────────────────────────────

fn parse_obj(data: &[u8]) -> Meta3D {
    let mut meta = Meta3D::default();
    let text = match std::str::from_utf8(data) {
        Ok(t) => t, Err(_) => return meta,
    };

    let (mut v, mut f, mut o) = (0u64, 0u64, 0u32);
    for line in text.lines() {
        match line.split_whitespace().next() {
            Some("v")          => v += 1,
            Some("f")          => f += 1,
            Some("o") | Some("g") => o += 1,
            _ => {}
        }
    }
    meta.vertex_count   = Some(v);
    meta.triangle_count = Some(f);
    if o > 0 { meta.object_count = Some(o); }
    meta
}

// ── 3MF ───────────────────────────────────────────────────────────────────

fn parse_3mf(data: &[u8]) -> Meta3D {
    use std::io::Read;
    let mut meta = Meta3D::default();

    let cursor = std::io::Cursor::new(data);
    let mut archive = match zip::ZipArchive::new(cursor) {
        Ok(a) => a, Err(_) => return meta,
    };

    let model_index = (0..archive.len()).find_map(|i| {
        let e = archive.by_index(i).ok()?;
        if e.name().to_lowercase().ends_with(".model") { Some(i) } else { None }
    });

    let idx = match model_index { Some(i) => i, None => return meta };
    let mut content = String::new();
    if archive.by_index(idx).ok()
        .and_then(|mut e| e.read_to_string(&mut content).ok()).is_none()
    { return meta; }

    use quick_xml::{Reader, events::Event};
    let mut reader = Reader::from_str(&content);
    reader.config_mut().trim_text(true);
    let (mut v, mut t, mut o) = (0u64, 0u64, 0u32);

    loop {
        match reader.read_event() {
            Ok(Event::Empty(ref e)) | Ok(Event::Start(ref e)) => {
                match e.name().as_ref() {
                    b"vertex"   => v += 1,
                    b"triangle" => t += 1,
                    b"object"   => o += 1,
                    _ => {}
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }

    if v > 0 { meta.vertex_count   = Some(v); }
    if t > 0 { meta.triangle_count = Some(t); }
    if o > 0 { meta.object_count   = Some(o); }
    meta
}
