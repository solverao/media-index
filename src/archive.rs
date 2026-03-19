use std::path::Path;
use anyhow::Result;
use crate::models::ArchiveType;

/// Un archivo extraído en memoria
pub struct ExtractedFile {
    pub name: String,
    pub data: Vec<u8>,
    pub ext:  String,
}

/// Extrae todos los archivos de media de un comprimido
pub fn extract_media_files(path: &Path, archive_type: &ArchiveType) -> Result<Vec<ExtractedFile>> {
    match archive_type {
        ArchiveType::Zip      => extract_zip(path),
        ArchiveType::SevenZip => extract_7z(path),
        ArchiveType::Rar      => extract_rar(path),
    }
}

const MAX_IN_MEMORY: u64 = 2 * 1024 * 1024 * 1024; // 2 GB

/// Devuelve true si la entrada es basura generada por macOS que no debe indexarse:
/// - carpeta __MACOSX/ completa (metadatos HFS+ embebidos en ZIPs creados en macOS)
/// - archivos AppleDouble con prefijo ._ (resource forks)
/// - .DS_Store (metadatos de Finder)
fn is_macos_junk(name: &str) -> bool {
    // Normalizar separadores para comparar de forma uniforme
    let n = name.replace('\\', "/");
    // Cualquier componente del path que sea __MACOSX
    if n.split('/').any(|seg| seg == "__MACOSX") { return true; }
    // Archivo cuyo nombre base empieza por ._  (AppleDouble resource fork)
    if n.split('/').last().map(|base| base.starts_with("._")).unwrap_or(false) { return true; }
    // .DS_Store
    if n.split('/').last().map(|base| base == ".DS_Store").unwrap_or(false) { return true; }
    false
}

fn extract_zip(path: &Path) -> Result<Vec<ExtractedFile>> {
    let file = std::fs::File::open(path)?;
    let mut archive = zip::ZipArchive::new(file)?;
    let mut results = vec![];

    for i in 0..archive.len() {
        let mut entry = match archive.by_index(i) {
            Ok(e) => e, Err(_) => continue,
        };
        if entry.is_dir() { continue; }
        let name = entry.name().to_string();
        if is_macos_junk(&name) { continue; }
        let ext  = ext_of(&name);
        if entry.size() > MAX_IN_MEMORY { continue; }

        let mut data = Vec::with_capacity(entry.size() as usize);
        if std::io::copy(&mut entry, &mut data).is_err() { continue; }
        results.push(ExtractedFile { name, data, ext });
    }
    Ok(results)
}

fn extract_7z(path: &Path) -> Result<Vec<ExtractedFile>> {
    use sevenz_rust::SevenZReader;

    let mut archive = SevenZReader::open(path, sevenz_rust::Password::empty())?;
    let mut results = vec![];

    archive.for_each_entries(|entry, reader| {
        if entry.is_directory() { return Ok(true); }
        let name = entry.name().to_string();
        if is_macos_junk(&name) { return Ok(true); }
        let ext  = ext_of(&name);
        if entry.size() > MAX_IN_MEMORY { return Ok(true); }
        let mut data = Vec::with_capacity(entry.size() as usize);
        if std::io::copy(reader, &mut data).is_ok() {
            results.push(ExtractedFile { name, data, ext });
        }
        Ok(true)
    })?;

    Ok(results)
}

fn extract_rar(path: &Path) -> Result<Vec<ExtractedFile>> {
    use std::process::Command;

    let check = Command::new("unrar").arg("--help").output();
    if check.is_err() {
        anyhow::bail!(
            "unrar no encontrado. Instalar con:\n\
             Debian/Ubuntu: sudo apt install unrar\n\
             Arch: sudo pacman -S unrar\n\
             macOS: brew install rar"
        );
    }

    // Incluir el hash del path absoluto para evitar colisiones cuando
    // dos RAR distintos comparten nombre (en carpetas diferentes) y rayon
    // los extrae en paralelo al mismo directorio temporal.
    let path_hash = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        path.hash(&mut h);
        h.finish()
    };
    let tmp = std::env::temp_dir()
        .join(format!("media_idx_{}_{:016x}",
            path.file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "tmp".into()),
            path_hash,
        ));
    std::fs::create_dir_all(&tmp)?;

    Command::new("unrar")
        .args(["x", "-y", "-inul"])
        .arg(path)
        .arg(&tmp)
        .status()?;

    let mut results = vec![];
    for entry in walkdir::WalkDir::new(&tmp).into_iter().flatten() {
        if !entry.file_type().is_file() { continue; }
        let p = entry.path();
        // Obtener path relativo al tmp para evaluar __MACOSX y ._
        let rel = p.strip_prefix(&tmp).unwrap_or(p)
            .to_string_lossy().to_string();
        if is_macos_junk(&rel) { continue; }

        let ext = p.extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();

        let meta = std::fs::metadata(p)?;
        if meta.len() > MAX_IN_MEMORY { continue; }

        let data = std::fs::read(p)?;
        let name = p.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        results.push(ExtractedFile { name, data, ext });
    }

    let _ = std::fs::remove_dir_all(&tmp);
    Ok(results)
}

fn ext_of(name: &str) -> String {
    Path::new(name)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── is_macos_junk ─────────────────────────────────────────────────────

    #[test]
    fn detecta_carpeta_macosx() {
        assert!(is_macos_junk("__MACOSX/file.txt"));
        assert!(is_macos_junk("dir/__MACOSX/file.txt"));
        assert!(is_macos_junk("a/b/__MACOSX/c/d.jpg"));
    }

    #[test]
    fn detecta_resource_fork_dot_underscore() {
        assert!(is_macos_junk("._archivo.stl"));
        assert!(is_macos_junk("dir/._foto.jpg"));
        assert!(is_macos_junk("__MACOSX/._hidden"));
    }

    #[test]
    fn detecta_ds_store() {
        assert!(is_macos_junk(".DS_Store"));
        assert!(is_macos_junk("subdir/.DS_Store"));
    }

    #[test]
    fn detecta_separador_windows() {
        // Nombres con backslash (ZIPs creados en Windows con rutas macOS)
        assert!(is_macos_junk("__MACOSX\\file.jpg"));
        assert!(is_macos_junk("dir\\__MACOSX\\file.jpg"));
    }

    #[test]
    fn no_marca_archivos_normales() {
        assert!(!is_macos_junk("archivo.jpg"));
        assert!(!is_macos_junk("fotos/vacaciones.jpg"));
        assert!(!is_macos_junk("modelo.stl"));
        assert!(!is_macos_junk("dir/subdir/video.mp4"));
    }

    #[test]
    fn no_marca_nombres_que_contienen_macosx_no_como_segmento() {
        // "__MACOSX" en el nombre de archivo pero no como directorio
        assert!(!is_macos_junk("mi__MACOSXarchivo.jpg"));
    }

    // ── ext_of ────────────────────────────────────────────────────────────

    #[test]
    fn ext_of_devuelve_extension_en_minuscula() {
        assert_eq!(ext_of("foto.JPG"), "jpg");
        assert_eq!(ext_of("video.MP4"), "mp4");
        assert_eq!(ext_of("modelo.STL"), "stl");
    }

    #[test]
    fn ext_of_sin_extension_es_vacio() {
        assert_eq!(ext_of("Makefile"), "");
        assert_eq!(ext_of(""), "");
    }

    #[test]
    fn ext_of_ruta_con_directorios() {
        assert_eq!(ext_of("dir/subdir/archivo.flac"), "flac");
    }
}
