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

    let tmp = std::env::temp_dir()
        .join(format!("media_idx_{}", path.file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "tmp".into())));
    std::fs::create_dir_all(&tmp)?;

    Command::new("unrar")
        .args(["x", "-y", "-inul"])
        .arg(path)
        .arg(&tmp)
        .status()?;

    let mut results = vec![];
    for entry in walkdir::WalkDir::new(&tmp).into_iter().flatten() {
        if !entry.file_type().is_file() { continue; }
        let p   = entry.path();
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
