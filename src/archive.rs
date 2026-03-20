use std::path::Path;
use anyhow::Result;
use crate::models::ArchiveType;

/// A file extracted in memory
pub struct ExtractedFile {
    pub name: String,
    pub data: Vec<u8>,
    pub ext:  String,
}

/// Extracts all media files from an archive
pub fn extract_media_files(path: &Path, archive_type: &ArchiveType) -> Result<Vec<ExtractedFile>> {
    match archive_type {
        ArchiveType::Zip      => extract_zip(path),
        ArchiveType::SevenZip => extract_7z(path),
        ArchiveType::Rar      => extract_rar(path),
    }
}

const MAX_IN_MEMORY: u64 = 2 * 1024 * 1024 * 1024; // 2 GB

/// Returns true if the entry is macOS-generated junk that should not be indexed:
/// - __MACOSX/ folder (HFS+ metadata embedded in ZIPs created on macOS)
/// - AppleDouble files with ._ prefix (resource forks)
/// - .DS_Store (Finder metadata)
fn is_macos_junk(name: &str) -> bool {
    // Normalize path separators for uniform comparison
    let n = name.replace('\\', "/");
    // Any path segment equal to __MACOSX
    if n.split('/').any(|seg| seg == "__MACOSX") { return true; }
    // File whose basename starts with ._ (AppleDouble resource fork)
    if n.split('/').last().map(|base| base.starts_with("._")).unwrap_or(false) { return true; }
    // .DS_Store
    if n.split('/').last().map(|base| base == ".DS_Store").unwrap_or(false) { return true; }
    false
}

/// Número máximo de entradas a leer de un solo comprimido.
/// Evita congelar el scanner con ZIPs que contienen decenas de miles de archivos.
const MAX_ARCHIVE_ENTRIES: usize = 5_000;

fn extract_zip(path: &Path) -> Result<Vec<ExtractedFile>> {
    let file = std::fs::File::open(path)?;
    let mut archive = zip::ZipArchive::new(file)?;
    let mut results = vec![];
    let mut entries_read = 0usize;

    for i in 0..archive.len() {
        if entries_read >= MAX_ARCHIVE_ENTRIES { break; }

        let mut entry = match archive.by_index(i) {
            Ok(e) => e, Err(_) => continue,
        };
        if entry.is_dir() { continue; }
        let name = entry.name().to_string();
        if is_macos_junk(&name) { continue; }
        let ext = ext_of(&name);

        if entry.size() > MAX_IN_MEMORY { continue; }

        entries_read += 1;
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
    let mut entries_read = 0usize;

    archive.for_each_entries(|entry, reader| {
        if entry.is_directory() { return Ok(true); }
        if entries_read >= MAX_ARCHIVE_ENTRIES { return Ok(false); }
        let name = entry.name().to_string();
        if is_macos_junk(&name) { return Ok(true); }
        let ext = ext_of(&name);

        // Filtrar antes de leer
        if entry.size() > MAX_IN_MEMORY { return Ok(true); }
        let mut data = Vec::with_capacity(entry.size() as usize);
        if std::io::copy(reader, &mut data).is_ok() {
            results.push(ExtractedFile { name, data, ext });
            entries_read += 1;
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
            "unrar not found. Install with:\n\
             Debian/Ubuntu: sudo apt install unrar\n\
             Arch: sudo pacman -S unrar\n\
             macOS: brew install rar"
        );
    }

    // Include the absolute path hash to avoid collisions when two different
    // RARs share the same filename (in different directories) and rayon
    // extracts them in parallel to the same temp directory.
    let path_hash = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        path.hash(&mut h);
        h.finish()
    };
    let tmp = std::env::temp_dir()
        .join(format!("media_idx_rar_{}_{:016x}",
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
    let mut entries_read = 0usize;

    for entry in walkdir::WalkDir::new(&tmp).into_iter().flatten() {
        if entries_read >= MAX_ARCHIVE_ENTRIES { break; }
        if !entry.file_type().is_file() { continue; }

        let rel = entry.path()
            .strip_prefix(&tmp)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();

        if is_macos_junk(&rel) { continue; }

        let name = rel.clone();
        let ext  = ext_of(&name);

        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
        if size > MAX_IN_MEMORY { continue; }

        if let Ok(data) = std::fs::read(entry.path()) {
            results.push(ExtractedFile { name, data, ext });
            entries_read += 1;
        }
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
    fn detects_macosx_folder() {
        assert!(is_macos_junk("__MACOSX/file.txt"));
        assert!(is_macos_junk("dir/__MACOSX/file.txt"));
        assert!(is_macos_junk("a/b/__MACOSX/c/d.jpg"));
    }

    #[test]
    fn detects_resource_fork_dot_underscore() {
        assert!(is_macos_junk("._file.stl"));
        assert!(is_macos_junk("dir/._photo.jpg"));
        assert!(is_macos_junk("__MACOSX/._hidden"));
    }

    #[test]
    fn detects_ds_store() {
        assert!(is_macos_junk(".DS_Store"));
        assert!(is_macos_junk("subdir/.DS_Store"));
    }

    #[test]
    fn detects_windows_separator() {
        // Names with backslash (ZIPs created on Windows with macOS paths)
        assert!(is_macos_junk("__MACOSX\\file.jpg"));
        assert!(is_macos_junk("dir\\__MACOSX\\file.jpg"));
    }

    #[test]
    fn does_not_flag_normal_files() {
        assert!(!is_macos_junk("file.jpg"));
        assert!(!is_macos_junk("photos/vacation.jpg"));
        assert!(!is_macos_junk("model.stl"));
        assert!(!is_macos_junk("dir/subdir/video.mp4"));
    }

    #[test]
    fn does_not_flag_macosx_as_substring_in_filename() {
        // "__MACOSX" in the filename but not as a directory segment
        assert!(!is_macos_junk("my__MACOSXfile.jpg"));
    }

    // ── ext_of ────────────────────────────────────────────────────────────

    #[test]
    fn ext_of_returns_lowercase_extension() {
        assert_eq!(ext_of("photo.JPG"), "jpg");
        assert_eq!(ext_of("video.MP4"), "mp4");
        assert_eq!(ext_of("model.STL"), "stl");
    }

    #[test]
    fn ext_of_no_extension_is_empty() {
        assert_eq!(ext_of("Makefile"), "");
        assert_eq!(ext_of(""), "");
    }

    #[test]
    fn ext_of_path_with_directories() {
        assert_eq!(ext_of("dir/subdir/file.flac"), "flac");
    }
}