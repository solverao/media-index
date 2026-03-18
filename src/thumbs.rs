use std::path::{Path, PathBuf};
use anyhow::Result;

// Paleta para thumbnails 3D
const DARK_BG:     [u8; 3] = [28,  28,  32 ];
const POINT_COLOR: [u8; 3] = [200, 220, 255];
const EDGE_COLOR:  [u8; 3] = [100, 140, 200];

// Cap de vértices/triángulos para no bloquear con modelos enormes
const MAX_VERTS: usize = 60_000;

// ── Rutas ─────────────────────────────────────────────────────────────────

/// Calcula la ruta del thumbnail dado el directorio base y el hash.
/// Usa los primeros 2 caracteres del hash como subdirectorio para no llenar
/// un solo directorio con miles de archivos.
pub fn thumb_path(thumb_dir: &Path, hash: &str) -> PathBuf {
    let prefix = hash.get(..2).unwrap_or("xx");
    thumb_dir.join(prefix).join(format!("{hash}.jpg"))
}

/// Deriva el directorio de thumbnails a partir de la ruta de la BD.
/// Ej: `/datos/media.db` → `/datos/media.thumbs/`
pub fn thumb_dir_for_db(db_path: &str) -> PathBuf {
    let p    = Path::new(db_path);
    let stem = p.file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "media".to_string());
    let dir  = p.parent().unwrap_or(Path::new("."));
    dir.join(format!("{stem}.thumbs"))
}

// ── Generadores públicos ──────────────────────────────────────────────────

/// Thumbnail de imagen desde su ruta en disco.
pub fn generate_image(
    path:      &str,
    hash:      &str,
    thumb_dir: &Path,
    size:      u32,
    quality:   u8,
) -> Result<()> {
    let data = std::fs::read(path)?;
    let img  = image::load_from_memory(&data)?;
    let thumb = img.thumbnail(size, size);
    write_jpeg(&thumb.to_rgb8(), hash, thumb_dir, quality)
}

/// Thumbnail de video usando ffmpeg: extrae un frame cerca del inicio.
/// Requiere que `ffmpeg` esté instalado en el PATH.
pub fn generate_video(
    path:      &str,
    hash:      &str,
    thumb_dir: &Path,
    size:      u32,
    quality:   u8,
) -> Result<()> {
    let out = thumb_path(thumb_dir, hash);
    std::fs::create_dir_all(out.parent().unwrap())?;

    // vf: escala manteniendo aspecto, rellena con negro para cuadrar
    let vf = format!(
        "scale='if(gt(iw,ih),{size},-2)':'if(gt(iw,ih),-2,{size})',\
         pad={size}:{size}:(ow-iw)/2:(oh-ih)/2:color=black"
    );
    let qscale = quality_to_qscale(quality).to_string();
    let out_str = out.to_string_lossy().to_string();

    // Intentar extraer frame en segundo 5; si falla, en segundo 0
    for seek in ["5", "0"] {
        let ok = std::process::Command::new("ffmpeg")
            .args(["-ss", seek, "-i", path,
                   "-vframes", "1", "-vf", &vf,
                   "-q:v", &qscale, "-y", &out_str])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);

        if ok && out.exists() && out.metadata().map(|m| m.len()).unwrap_or(0) > 100 {
            return Ok(());
        }
    }

    // Limpiar archivo parcial si quedó
    let _ = std::fs::remove_file(&out);
    anyhow::bail!("ffmpeg no generó thumbnail para {path}")
}

/// Thumbnail de modelo 3D: proyección isométrica de la malla.
/// Soporta STL (binario y ASCII), OBJ y 3MF.
pub fn generate_3d(
    data:      &[u8],
    ext:       &str,
    hash:      &str,
    thumb_dir: &Path,
    size:      u32,
    quality:   u8,
) -> Result<()> {
    let (verts, tris) = parse_3d_geometry(data, ext);
    if verts.is_empty() {
        anyhow::bail!("Sin vértices en el modelo .{ext}");
    }
    let img = render_isometric(&verts, &tris, size);
    write_jpeg(&img, hash, thumb_dir, quality)
}

// ── Renderizador isométrico ───────────────────────────────────────────────

fn render_isometric(
    verts: &[[f32; 3]],
    tris:  &[[usize; 3]],
    size:  u32,
) -> image::RgbImage {
    // 1. Normalizar a [-0.9, 0.9] respetando proporciones
    let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
    for v in verts {
        for i in 0..3 { lo[i] = lo[i].min(v[i]); hi[i] = hi[i].max(v[i]); }
    }
    let range = (0..3).map(|i| (hi[i] - lo[i]).max(1e-6)).fold(f32::MIN, f32::max);
    let center = [(lo[0]+hi[0])/2.0, (lo[1]+hi[1])/2.0, (lo[2]+hi[2])/2.0];
    let norm: Vec<[f32; 3]> = verts.iter().map(|v| [
        (v[0] - center[0]) / range * 1.8,
        (v[1] - center[1]) / range * 1.8,
        (v[2] - center[2]) / range * 1.8,
    ]).collect();

    // 2. Proyección isométrica (azimut 45°, elevación 30°)
    let az = 45_f32.to_radians();
    let el = 30_f32.to_radians();
    let proj: Vec<[f32; 2]> = norm.iter().map(|v| {
        let x =  v[0] * az.cos() - v[2] * az.sin();
        let y = -v[0] * az.sin() * el.sin()
                - v[1] * el.cos()
                - v[2] * az.cos() * el.sin();
        [x, y]
    }).collect();

    // 3. Mapear a píxeles
    let half  = size as f32 / 2.0;
    let scale = half * 0.85;
    let to_px = |p: [f32; 2]| -> (i32, i32) {
        ((half + p[0] * scale).round() as i32,
         (half + p[1] * scale).round() as i32)
    };

    let mut img = image::RgbImage::from_pixel(size, size, image::Rgb(DARK_BG));

    // 4. Aristas de triángulos
    for tri in tris {
        for &(a, b) in &[(tri[0], tri[1]), (tri[1], tri[2]), (tri[2], tri[0])] {
            if a < proj.len() && b < proj.len() {
                let (x0, y0) = to_px(proj[a]);
                let (x1, y1) = to_px(proj[b]);
                draw_line(&mut img, x0, y0, x1, y1, EDGE_COLOR);
            }
        }
    }

    // 5. Puntos (siempre, encima de aristas)
    for p in &proj {
        let (cx, cy) = to_px(*p);
        draw_dot(&mut img, cx, cy, POINT_COLOR);
    }

    img
}

fn draw_line(img: &mut image::RgbImage, x0: i32, y0: i32, x1: i32, y1: i32, color: [u8; 3]) {
    let (w, h) = (img.width() as i32, img.height() as i32);
    let (dx, dy) = ((x1-x0).abs(), (y1-y0).abs());
    let (sx, sy) = (if x0 < x1 { 1 } else { -1 }, if y0 < y1 { 1 } else { -1 });
    let (mut x, mut y, mut err) = (x0, y0, dx - dy);
    loop {
        if x >= 0 && x < w && y >= 0 && y < h {
            img.put_pixel(x as u32, y as u32, image::Rgb(color));
        }
        if x == x1 && y == y1 { break; }
        let e2 = 2 * err;
        if e2 > -dy { err -= dy; x += sx; }
        if e2 <  dx { err += dx; y += sy; }
    }
}

fn draw_dot(img: &mut image::RgbImage, cx: i32, cy: i32, color: [u8; 3]) {
    let (w, h) = (img.width() as i32, img.height() as i32);
    for dy in -1i32..=1 { for dx in -1i32..=1 {
        let (x, y) = (cx + dx, cy + dy);
        if x >= 0 && x < w && y >= 0 && y < h {
            img.put_pixel(x as u32, y as u32, image::Rgb(color));
        }
    }}
}

// ── Parsers de geometría 3D ───────────────────────────────────────────────

/// Retorna (vértices, triángulos-como-índices).
fn parse_3d_geometry(data: &[u8], ext: &str) -> (Vec<[f32; 3]>, Vec<[usize; 3]>) {
    match ext.to_lowercase().as_str() {
        "stl" => parse_stl(data),
        "obj" => parse_obj(data),
        "3mf" => parse_3mf(data),
        _     => (vec![], vec![]),
    }
}

// ── STL ──────────────────────────────────────────────────────────────────

fn parse_stl(data: &[u8]) -> (Vec<[f32; 3]>, Vec<[usize; 3]>) {
    let is_ascii = data.starts_with(b"solid ")
        && std::str::from_utf8(&data[..data.len().min(256)]).is_ok();
    if is_ascii { parse_stl_ascii(data) } else { parse_stl_binary(data) }
}

fn parse_stl_binary(data: &[u8]) -> (Vec<[f32; 3]>, Vec<[usize; 3]>) {
    if data.len() < 84 { return (vec![], vec![]); }
    let n = (u32::from_le_bytes([data[80], data[81], data[82], data[83]]) as usize)
        .min(MAX_VERTS / 3);

    let mut verts = Vec::with_capacity(n * 3);
    let mut tris  = Vec::with_capacity(n);

    for i in 0..n {
        let base = 84 + i * 50;
        if base + 50 > data.len() { break; }
        let mut tri_v = [[0f32; 3]; 3];
        for (j, tv) in tri_v.iter_mut().enumerate() {
            let off = base + 12 + j * 12;
            tv[0] = f32::from_le_bytes(data[off  ..off+4 ].try_into().unwrap_or([0;4]));
            tv[1] = f32::from_le_bytes(data[off+4..off+8 ].try_into().unwrap_or([0;4]));
            tv[2] = f32::from_le_bytes(data[off+8..off+12].try_into().unwrap_or([0;4]));
        }
        let base_idx = verts.len();
        verts.extend_from_slice(&tri_v);
        tris.push([base_idx, base_idx + 1, base_idx + 2]);
    }
    (verts, tris)
}

fn parse_stl_ascii(data: &[u8]) -> (Vec<[f32; 3]>, Vec<[usize; 3]>) {
    let text = match std::str::from_utf8(data) { Ok(t) => t, Err(_) => return (vec![], vec![]) };
    let mut verts = vec![];
    let mut tris  = vec![];
    let mut buf   = 0usize;  // vértices acumulados en el triángulo actual

    for line in text.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.first() == Some(&"vertex") && parts.len() == 4 {
            if let (Ok(x), Ok(y), Ok(z)) = (
                parts[1].parse::<f32>(), parts[2].parse::<f32>(), parts[3].parse::<f32>()
            ) {
                verts.push([x, y, z]);
                buf += 1;
                if buf == 3 {
                    let n = verts.len();
                    tris.push([n-3, n-2, n-1]);
                    buf = 0;
                }
            }
        }
        if verts.len() >= MAX_VERTS { break; }
    }
    (verts, tris)
}

// ── OBJ ──────────────────────────────────────────────────────────────────

fn parse_obj(data: &[u8]) -> (Vec<[f32; 3]>, Vec<[usize; 3]>) {
    let text = match std::str::from_utf8(data) { Ok(t) => t, Err(_) => return (vec![], vec![]) };
    let mut verts = vec![];
    let mut tris  = vec![];

    for line in text.lines() {
        let mut parts = line.split_whitespace();
        match parts.next() {
            Some("v") => {
                let c: Vec<f32> = parts.filter_map(|p| p.parse().ok()).collect();
                if c.len() >= 3 { verts.push([c[0], c[1], c[2]]); }
            }
            Some("f") => {
                // Índices 1-based, pueden venir como "1/2/3" o "1//3" o solo "1"
                let idx: Vec<usize> = parts
                    .filter_map(|p| p.split('/').next()?.parse::<usize>().ok())
                    .filter(|&i| i > 0)
                    .map(|i| i - 1)
                    .collect();
                // Fan-triangulation para n-gons
                for i in 1..idx.len().saturating_sub(1) {
                    tris.push([idx[0], idx[i], idx[i+1]]);
                }
            }
            _ => {}
        }
        if verts.len() >= MAX_VERTS { break; }
    }
    (verts, tris)
}

// ── 3MF ──────────────────────────────────────────────────────────────────

fn parse_3mf(data: &[u8]) -> (Vec<[f32; 3]>, Vec<[usize; 3]>) {
    use std::io::Read;
    use quick_xml::{Reader, events::Event};

    let cursor = std::io::Cursor::new(data);
    let mut archive = match zip::ZipArchive::new(cursor) { Ok(a) => a, Err(_) => return (vec![], vec![]) };

    let model_idx = (0..archive.len()).find_map(|i| {
        let e = archive.by_index(i).ok()?;
        if e.name().to_lowercase().ends_with(".model") { Some(i) } else { None }
    });
    let idx = match model_idx { Some(i) => i, None => return (vec![], vec![]) };

    let mut content = String::new();
    if archive.by_index(idx).ok()
        .and_then(|mut e| e.read_to_string(&mut content).ok()).is_none()
    { return (vec![], vec![]); }

    let mut reader = Reader::from_str(&content);
    reader.config_mut().trim_text(true);
    let mut verts = vec![];
    let mut tris  = vec![];

    loop {
        match reader.read_event() {
            Ok(Event::Empty(ref e)) | Ok(Event::Start(ref e)) => {
                match e.name().as_ref() {
                    b"vertex" => {
                        let (mut x, mut y, mut z) = (0f32, 0f32, 0f32);
                        for attr in e.attributes().flatten() {
                            let val: f32 = std::str::from_utf8(&attr.value).ok()
                                .and_then(|s| s.parse().ok()).unwrap_or(0.0);
                            match attr.key.as_ref() {
                                b"x" => x = val, b"y" => y = val, b"z" => z = val, _ => {}
                            }
                        }
                        verts.push([x, y, z]);
                    }
                    b"triangle" => {
                        let mut v = [0usize; 3];
                        for attr in e.attributes().flatten() {
                            let i: usize = std::str::from_utf8(&attr.value).ok()
                                .and_then(|s| s.parse().ok()).unwrap_or(0);
                            match attr.key.as_ref() {
                                b"v1" => v[0] = i, b"v2" => v[1] = i, b"v3" => v[2] = i, _ => {}
                            }
                        }
                        tris.push(v);
                    }
                    _ => {}
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        if verts.len() >= MAX_VERTS { break; }
    }
    (verts, tris)
}

// ── Escritura JPEG ────────────────────────────────────────────────────────

fn write_jpeg(img: &image::RgbImage, hash: &str, thumb_dir: &Path, quality: u8) -> Result<()> {
    use image::codecs::jpeg::JpegEncoder;

    let path = thumb_path(thumb_dir, hash);
    std::fs::create_dir_all(path.parent().unwrap())?;
    let mut file = std::fs::File::create(&path)?;
    JpegEncoder::new_with_quality(&mut file, quality).encode_image(img)?;
    Ok(())
}

/// Convierte calidad 1–100 al qscale de ffmpeg (2=mejor, 31=peor)
fn quality_to_qscale(quality: u8) -> u8 {
    let q = quality.clamp(1, 100) as f32;
    (2.0 + (100.0 - q) / 100.0 * 29.0).round() as u8
}
