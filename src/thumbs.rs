use std::path::{Path, PathBuf};
use anyhow::Result;

pub const DEFAULT_SIZE:    u32 = 256;
pub const DEFAULT_QUALITY: u8  = 85;

// Fondo oscuro
const DARK_BG: [u8; 3] = [28, 28, 32];

// Cap para no bloquear con modelos enormes
const MAX_VERTS: usize = 150_000;

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

/// Thumbnail de imagen desde bytes en memoria (para archivos dentro de comprimidos).
pub fn generate_image_from_bytes(
    data:      &[u8],
    hash:      &str,
    thumb_dir: &Path,
    size:      u32,
    quality:   u8,
) -> Result<()> {
    let img   = image::load_from_memory(data)?;
    let img   = apply_exif_orientation(img, data);
    let thumb = img.thumbnail(size, size);
    write_jpeg(&thumb.to_rgb8(), hash, thumb_dir, quality)
}

/// Thumbnail de imagen desde su ruta en disco.
pub fn generate_image(
    path:      &str,
    hash:      &str,
    thumb_dir: &Path,
    size:      u32,
    quality:   u8,
) -> Result<()> {
    let data  = std::fs::read(path)?;
    let img   = image::load_from_memory(&data)?;
    let img   = apply_exif_orientation(img, &data);
    let thumb = img.thumbnail(size, size);
    write_jpeg(&thumb.to_rgb8(), hash, thumb_dir, quality)
}

/// Corrige la orientación de la imagen según el tag EXIF Orientation.
/// Las cámaras y móviles almacenan las fotos rotadas con la corrección en EXIF.
fn apply_exif_orientation(img: image::DynamicImage, data: &[u8]) -> image::DynamicImage {
    use exif::{Reader as ExifReader, Tag, In, Value};

    let orientation = ExifReader::new()
        .read_from_container(&mut std::io::Cursor::new(data))
        .ok()
        .and_then(|exif| {
            exif.get_field(Tag::Orientation, In::PRIMARY)
                .and_then(|f| match &f.value {
                    Value::Short(v) => v.first().copied(),
                    _ => None,
                })
        })
        .unwrap_or(1);

    // Valores EXIF Orientation:
    // 1 = Normal          2 = Flip H
    // 3 = Rotate 180°     4 = Flip V
    // 5 = Transpose       6 = Rotate 90° CW
    // 7 = Transverse      8 = Rotate 270° CW
    match orientation {
        2 => img.fliph(),
        3 => img.rotate180(),
        4 => img.flipv(),
        5 => img.rotate90().fliph(),
        6 => img.rotate90(),
        7 => img.rotate270().fliph(),
        8 => img.rotate270(),
        _ => img, // 1 o desconocido: sin cambios
    }
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

/// Thumbnail de video desde bytes en memoria (archivo dentro de comprimido).
/// Escribe los bytes a un archivo temporal, llama a ffmpeg y lo borra.
pub fn generate_video_from_archive(
    data:      &[u8],
    ext:       &str,
    hash:      &str,
    thumb_dir: &Path,
    size:      u32,
    quality:   u8,
) -> Result<()> {
    let tmp_path = std::env::temp_dir()
        .join(format!("media_idx_thumb_{hash}.{ext}"));
    std::fs::write(&tmp_path, data)?;

    let result = generate_video(
        tmp_path.to_string_lossy().as_ref(),
        hash, thumb_dir, size, quality,
    );

    let _ = std::fs::remove_file(&tmp_path);
    result
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

// Color base del modelo y luz
const MODEL_COLOR: [f32; 3] = [0.60, 0.78, 0.95]; // azul claro
const LIGHT_DIR:   [f32; 3] = [0.57, 0.57, 0.57];  // luz diagonal ~(1,1,1) normalizada
const AMBIENT:     f32      = 0.35;                  // ambiente más alto = menos zonas negras

fn render_isometric(
    verts: &[[f32; 3]],
    tris:  &[[usize; 3]],
    size:  u32,
) -> image::RgbImage {
    // Renderizar a 2x y reducir (supersampling 4x) para eliminar gaps sub-pixel
    let render_size = size * 2;

    // 1. Normalizar por percentil (P1–P99) para ignorar vértices outlier.
    // Min/max absoluto hace que un solo vértice lejano encoja todo el modelo.
    let mut xs: Vec<f32> = verts.iter().map(|v| v[0]).collect();
    let mut ys: Vec<f32> = verts.iter().map(|v| v[1]).collect();
    let mut zs: Vec<f32> = verts.iter().map(|v| v[2]).collect();
    xs.sort_unstable_by(|a,b| a.partial_cmp(b).unwrap());
    ys.sort_unstable_by(|a,b| a.partial_cmp(b).unwrap());
    zs.sort_unstable_by(|a,b| a.partial_cmp(b).unwrap());

    let p = |v: &[f32], pct: f32| -> f32 {
        let i = ((v.len() as f32 * pct).round() as usize).min(v.len()-1);
        v[i]
    };
    let lo = [p(&xs,0.01), p(&ys,0.01), p(&zs,0.01)];
    let hi = [p(&xs,0.99), p(&ys,0.99), p(&zs,0.99)];

    let range  = (0..3).map(|i| (hi[i] - lo[i]).max(1e-6)).fold(f32::MIN, f32::max);
    let center = [(lo[0]+hi[0])/2.0, (lo[1]+hi[1])/2.0, (lo[2]+hi[2])/2.0];
    let norm: Vec<[f32; 3]> = verts.iter().map(|v| [
        (v[0] - center[0]) / range * 1.8,
        (v[1] - center[1]) / range * 1.8,
        (v[2] - center[2]) / range * 1.8,
    ]).collect();

    // 2. Proyección isométrica (azimut 45°, elevación 35°)
    let az = 45_f32.to_radians();
    let el = 35_f32.to_radians();

    let proj3d: Vec<[f32; 3]> = norm.iter().map(|v| {
        let rx =  v[0] * az.cos() - v[2] * az.sin();
        let ry =  v[1];
        let rz =  v[0] * az.sin() + v[2] * az.cos();
        let fx =  rx;
        let fy =  ry * el.cos() - rz * el.sin();
        let fz =  ry * el.sin() + rz * el.cos();
        [fx, fy, fz]
    }).collect();

    // 3. Proyección 2D al buffer de alta resolución
    let half  = render_size as f32 / 2.0;
    let scale = half * 0.88;
    let to_px = |p: &[f32; 3]| -> (i32, i32) {
        ((half + p[0] * scale).round() as i32,
         (half - p[1] * scale).round() as i32)
    };

    // 4. Calcular triángulos
    struct TriData { px: [(i32,i32); 3], depth: f32, color: [u8; 3] }
    let mut tri_data: Vec<TriData> = Vec::with_capacity(tris.len());

    for tri in tris {
        let (a, b, c) = (tri[0], tri[1], tri[2]);
        if a >= proj3d.len() || b >= proj3d.len() || c >= proj3d.len() { continue; }
        let va = proj3d[a]; let vb = proj3d[b]; let vc = proj3d[c];

        let ab = [vb[0]-va[0], vb[1]-va[1], vb[2]-va[2]];
        let ac = [vc[0]-va[0], vc[1]-va[1], vc[2]-va[2]];
        let nx = ab[1]*ac[2] - ab[2]*ac[1];
        let ny = ab[2]*ac[0] - ab[0]*ac[2];
        let nz = ab[0]*ac[1] - ab[1]*ac[0];
        let nlen = (nx*nx + ny*ny + nz*nz).sqrt().max(1e-8);
        let (nx, ny, nz) = (nx/nlen, ny/nlen, nz/nlen);

        let diffuse = (nx*LIGHT_DIR[0] + ny*LIGHT_DIR[1] + nz*LIGHT_DIR[2]).abs();
        let light   = (AMBIENT + (1.0 - AMBIENT) * diffuse).min(1.0);
        let color   = [
            (MODEL_COLOR[0] * light * 255.0) as u8,
            (MODEL_COLOR[1] * light * 255.0) as u8,
            (MODEL_COLOR[2] * light * 255.0) as u8,
        ];
        let depth = (va[2] + vb[2] + vc[2]) / 3.0;
        tri_data.push(TriData { px: [to_px(&va), to_px(&vb), to_px(&vc)], depth, color });
    }

    // 5. Z-buffer en resolución 2x
    let sz = (render_size * render_size) as usize;
    let mut zbuf   = vec![f32::NEG_INFINITY; sz];
    let mut pixels: Vec<[u8; 3]> = vec![DARK_BG; sz];

    for tri in &tri_data {
        fill_triangle_zbuf(&mut zbuf, &mut pixels, render_size,
            tri.px[0], tri.px[1], tri.px[2], tri.depth, tri.color);
    }

    // 6. Downsample 2x→1x promediando bloques 2×2 (anti-aliasing)
    let mut img = image::RgbImage::new(size, size);
    for y in 0..size {
        for x in 0..size {
            let mut r = 0u32; let mut g = 0u32; let mut b = 0u32;
            for dy in 0..2u32 { for dx in 0..2u32 {
                let idx = ((y*2+dy) * render_size + (x*2+dx)) as usize;
                r += pixels[idx][0] as u32;
                g += pixels[idx][1] as u32;
                b += pixels[idx][2] as u32;
            }}
            img.put_pixel(x, y, image::Rgb([(r/4) as u8, (g/4) as u8, (b/4) as u8]));
        }
    }

    img
}

/// Rellena un triángulo con z-buffer: solo pinta un píxel si está más cerca
/// que lo que ya había. Interpola la profundidad linealmente.
fn fill_triangle_zbuf(
    zbuf:   &mut Vec<f32>,
    pixels: &mut Vec<[u8; 3]>,
    size:   u32,
    p0: (i32, i32), p1: (i32, i32), p2: (i32, i32),
    depth: f32,
    color: [u8; 3],
) {
    let w = size as i32;
    let h = size as i32;

    // Ordenar vértices por Y ascendente
    let mut pts = [(p0.0, p0.1), (p1.0, p1.1), (p2.0, p2.1)];
    pts.sort_unstable_by_key(|p| p.1);
    let [(x0,y0), (x1,y1), (x2,y2)] = pts;

    let interp = |ya: i32, yb: i32, xa: i32, xb: i32, y: i32| -> i32 {
        if ya == yb { xa.min(xb) }
        else { xa + (xb - xa) * (y - ya) / (yb - ya) }
    };

    for y in y0.max(0)..=y2.min(h - 1) {
        let lx = if y < y1 {
            interp(y0, y1, x0, x1, y)
        } else {
            interp(y1, y2, x1, x2, y)
        };
        let rx = interp(y0, y2, x0, x2, y);
        let (lx, rx) = (lx.min(rx).max(0), lx.max(rx).min(w - 1));

        for x in lx..=rx {
            let idx = y as usize * size as usize + x as usize;
            if depth > zbuf[idx] {
                zbuf[idx]   = depth;
                pixels[idx] = color;
            }
        }
    }
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
