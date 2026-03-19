use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::io::Read;
use anyhow::Result;
use walkdir::WalkDir;
use indicatif::{ProgressBar, ProgressStyle};
use colored::Colorize;
use rayon::prelude::*;

use crate::archive::extract_media_files;
use crate::db::Database;
use crate::models::*;
use crate::parsers;

/// For files larger than this limit, only hash head + tail + size.
/// Avoids reading 50 GB of video into RAM. The hash remains
/// unique enough for practical deduplication.
const PARTIAL_HASH_THRESHOLD: u64 = 100 * 1024 * 1024; // 100 MB
const PARTIAL_CHUNK_SIZE:     u64 = 4  * 1024 * 1024;  //   4 MB per side

pub struct Scanner {
    db:      Arc<Mutex<Database>>,
    verbose: bool,
    no_archives: bool,
}

impl Scanner {
    pub fn new(db: Database, verbose: bool, no_archives: bool) -> Self {
        Self { db: Arc::new(Mutex::new(db)), verbose, no_archives }
    }

    pub fn scan(&self, root: &Path) -> Result<ScanStats> {
        let stats = Arc::new(Mutex::new(ScanStats::default()));

        // Remove stale entries before scanning so that manually deleted files
        // do not show up as duplicates in the next scan.
        {
            let db = self.db.lock().unwrap();
            match db.cleanup_stale() {
                Ok((files, dupes)) if files > 0 || dupes > 0 => {
                    println!(
                        "{}  Cleanup: {} canonical(s) and {} duplicate(s) removed from DB (no longer on disk)",
                        "🧹", files, dupes
                    );
                }
                Ok(_) => {}
                Err(e) => eprintln!("Warning: Error cleaning up DB: {e}"),
            }
        }

        println!("{}", "Collecting files...".cyan());

        let mut walk_errors = 0usize;
        let entries: Vec<PathBuf> = WalkDir::new(root)
            // follow_links(true): required for WSL mount points (DrvFs)
            // and network drives exposed as symlinks in the Linux VFS.
            .follow_links(true)
            .into_iter()
            .filter_map(|e| match e {
                Ok(entry) => Some(entry),
                Err(err)  => {
                    walk_errors += 1;
                    // Show only the first error to avoid flooding the output
                    if walk_errors == 1 {
                        eprintln!("  {} Access error: {err}", "⚠".yellow());
                        eprintln!("  {} (additional errors omitted)", " ".normal());
                    }
                    None
                }
            })
            .filter(|e| e.file_type().is_file())
            .map(|e| e.path().to_path_buf())
            .collect();

        if walk_errors > 0 {
            eprintln!("  {} {} inaccessible directory/ies ignored", "⚠".yellow(), walk_errors);
        }
        println!("{} {} files found", "→".green(), entries.len());

        let pb = ProgressBar::new(entries.len() as u64);
        pb.set_style(ProgressStyle::with_template(
            "{spinner:.cyan} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} {msg}"
        )?.progress_chars("█▓░"));

        entries.par_iter().for_each(|path| {
            pb.set_message(
                path.file_name()
                    .map(|n| n.to_string_lossy().chars().take(45).collect::<String>())
                    .unwrap_or_default()
            );

            let ext = path.extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();

            let name_lower = path.file_name()
                .map(|n| n.to_string_lossy().to_lowercase())
                .unwrap_or_default();

            // ── Direct media file ──────────────────────────────────────────
            if let Some(media_type) = MediaType::from_extension(&ext) {
                match self.build_entry(path, &ext, &media_type) {
                    Ok(entry) => {
                        let mut s = stats.lock().unwrap();
                        self.insert_entry(entry, &mut s);
                    }
                    Err(e) => {
                        let mut s = stats.lock().unwrap();
                        s.errors += 1;
                        if self.verbose {
                            eprintln!("  {} {}: {e}", "✗".red(), path.display());
                        }
                    }
                }
                pb.inc(1);
                return;
            }

            // ── Archive ────────────────────────────────────────────────────
            let is_archive = ArchiveType::from_path(path).is_some();

            if !self.no_archives {
                if let Some(archive_type) = ArchiveType::from_path(path) {
                    // Skip extra parts of multi-part archives
                    if archive_type == ArchiveType::Rar && is_rar_multipart(&name_lower) {
                        let is_first = name_lower.contains(".part1.") || name_lower.contains(".part01.");
                        if !is_first { pb.inc(1); return; }
                    }
                    if archive_type == ArchiveType::SevenZip && is_7z_multipart(&name_lower) {
                        if !name_lower.ends_with(".001") { pb.inc(1); return; }
                    }

                    // Heavy work WITHOUT lock: extract + build entries
                    match extract_media_files(path, &archive_type) {
                        Ok(files) => {
                            let built: Vec<MediaEntry> = files.into_iter()
                                .map(|extracted| {
                                    let mt = MediaType::from_extension(&extracted.ext)
                                        .unwrap_or(MediaType::Other);
                                    build_entry_from_memory(
                                        &extracted.data, &extracted.name,
                                        &extracted.ext, &mt,
                                        path.to_string_lossy().as_ref(),
                                    )
                                })
                                .collect();

                            // Brief lock: insert the whole batch
                            let mut s = stats.lock().unwrap();
                            s.archives_opened += 1;
                            for entry in built {
                                self.insert_entry(entry, &mut s);
                            }
                        }
                        Err(e) => {
                            let mut s = stats.lock().unwrap();
                            s.errors += 1;
                            if self.verbose {
                                eprintln!("  {} {}: {e}", "✗".red(), path.display());
                            }
                        }
                    }
                }
            }

            // ── Any other file (unknown extension) ─────────────────────────
            // Indexed the same way: hash + size + path. No specific metadata.
            // With --no-archives, compressed files are also included here (the compressed file itself is hashed, without unpacking its contents).
            if !is_archive || self.no_archives {
                match self.build_entry(path, &ext, &MediaType::Other) {
                    Ok(entry) => {
                        let mut s = stats.lock().unwrap();
                        self.insert_entry(entry, &mut s);
                    }
                    Err(e) => {
                        let mut s = stats.lock().unwrap();
                        s.errors += 1;
                        if self.verbose {
                            eprintln!("  {} {}: {e}", "✗".red(), path.display());
                        }
                    }
                }
            }

            pb.inc(1);
        });

        pb.finish_with_message("done");
        Ok(Arc::try_unwrap(stats).unwrap().into_inner().unwrap())
    }

    // ── Build entry without touching DB or stats (heavy work) ────────────

    fn build_entry(
        &self,
        path:       &Path,
        ext:        &str,
        media_type: &MediaType,
    ) -> Result<MediaEntry> {
        let size     = std::fs::metadata(path)?.len();
        let hash     = hash_file(path, size)?;
        let path_str = path.to_string_lossy().to_string();
        let name     = path.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();

        let metadata = match media_type {
            MediaType::Other => Metadata::None,
            MediaType::Video => {
                Metadata::Video(parsers::video::parse_from_path(&path_str))
            }
            _ => {
                if size <= PARTIAL_HASH_THRESHOLD {
                    let data = std::fs::read(path)?;
                    parsers::parse(&data, ext, media_type, &path_str)
                } else {
                    Metadata::None
                }
            }
        };

        Ok(MediaEntry {
            blake3_hash:     hash,
            size_bytes:      size,
            original_name:   name,
            current_path:    path_str,
            extension:       ext.to_string(),
            media_type:      media_type.clone(),
            metadata,
            source_archive:  None,
            path_in_archive: None,
        })
    }

    // ── Insert into DB + update stats (fast, under lock) ─────────────────

    fn insert_entry(&self, entry: MediaEntry, stats: &mut ScanStats) {
        let media_type = entry.media_type.clone();
        let size = entry.size_bytes;
        let path = entry.current_path.clone();
        // Large loose files use partial hash — track for the warning
        let is_partial = entry.source_archive.is_none() && size > PARTIAL_HASH_THRESHOLD;

        match self.db.lock().unwrap().insert(&entry) {
            Ok((_, true, Some(orig))) => {
                stats.duplicates += 1;
                stats.bytes_dup  += size;
                if is_partial { stats.partial_hashes += 1; }
                if self.verbose {
                    eprintln!("  {} dupl: {} ← {}", "≡".yellow(), path, orig);
                }
            }
            Ok(_) => {
                if is_partial { stats.partial_hashes += 1; }
                match media_type {
                    MediaType::Print3D => stats.indexed_3d    += 1,
                    MediaType::Video   => stats.indexed_video  += 1,
                    MediaType::Audio   => stats.indexed_audio  += 1,
                    MediaType::Image   => stats.indexed_image  += 1,
                    MediaType::Other   => stats.indexed_other  += 1,
                }
            }
            Err(e) => {
                stats.errors += 1;
                if self.verbose {
                    eprintln!("  {} DB error: {e}", "✗".red());
                }
            }
        }
    }

    // ── Public API for watch mode ─────────────────────────────────────────

    /// Removes stale entries from the DB (delegates to db::cleanup_stale).
    pub fn cleanup(&self) -> Result<(usize, usize)> {
        self.db.lock().unwrap().cleanup_stale()
    }

    /// Indexes a single file (new or modified).
    /// Used by the watcher to process individual events without re-scanning everything.
    pub fn index_single(&self, path: &Path, ext: &str) {
        // Archive files: extract and process their contents
        // (only if --no-archives was not specified)
        if !self.no_archives {
            if let Some(archive_type) = ArchiveType::from_path(path) {
                let name_lower = path.file_name()
                    .map(|n| n.to_string_lossy().to_lowercase())
                    .unwrap_or_default();

                // Ignore extra parts of multi-part archives
                if archive_type == ArchiveType::Rar && is_rar_multipart(&name_lower) {
                    if !name_lower.contains(".part1.") && !name_lower.contains(".part01.") { return; }
                }
                if archive_type == ArchiveType::SevenZip && is_7z_multipart(&name_lower) {
                    if !name_lower.ends_with(".001") { return; }
                }

                match extract_media_files(path, &archive_type) {
                    Ok(files) => {
                        let built: Vec<MediaEntry> = files.into_iter()
                            .map(|extracted| {
                                let mt = MediaType::from_extension(&extracted.ext)
                                    .unwrap_or(MediaType::Other);
                                build_entry_from_memory(
                                    &extracted.data, &extracted.name,
                                    &extracted.ext, &mt,
                                    path.to_string_lossy().as_ref(),
                                )
                            })
                            .collect();

                        let mut dummy = ScanStats::default();
                        for entry in built {
                            self.insert_entry(entry, &mut dummy);
                        }
                    }
                    Err(e) if self.verbose => {
                        eprintln!("  {} {}: {e}", "✗".red(), path.display());
                    }
                    _ => {}
                }
                return;
            }
        }

        // Regular file: build entry and insert
        let media_type = MediaType::from_extension(ext).unwrap_or(MediaType::Other);
        match self.build_entry(path, ext, &media_type) {
            Ok(entry) => {
                let mut s = ScanStats::default();
                self.insert_entry(entry, &mut s);
                if self.verbose {
                    let label = match s.duplicates {
                        0 => "indexed".green(),
                        _ => "duplicate".red(),
                    };
                    println!("    {} {}", label, path.display());
                }
            }
            Err(e) if self.verbose => {
                eprintln!("  {} {}: {e}", "✗".red(), path.display());
            }
            _ => {}
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────

fn build_entry_from_memory(
    data:        &[u8],
    name:        &str,
    ext:         &str,
    media_type:  &MediaType,
    archive_path: &str,
) -> MediaEntry {
    let hash     = blake3::hash(data).to_hex().to_string();
    let metadata = parsers::parse(data, ext, media_type, "");

    MediaEntry {
        blake3_hash:     hash,
        size_bytes:      data.len() as u64,
        original_name:   name.to_string(),
        current_path:    format!("{}::{}", archive_path, name),
        extension:       ext.to_string(),
        media_type:      media_type.clone(),
        metadata,
        source_archive:  Some(archive_path.to_string()),
        path_in_archive: Some(name.to_string()),
    }
}

/// Full hash for small files, partial (head+tail) for large ones.
fn hash_file(path: &Path, size: u64) -> Result<String> {
    if size <= PARTIAL_HASH_THRESHOLD {
        let data = std::fs::read(path)?;
        return Ok(blake3::hash(&data).to_hex().to_string());
    }

    // Partial hash: first 4 MB + last 4 MB + size as salt
    let chunk = PARTIAL_CHUNK_SIZE as usize;
    let mut hasher = blake3::Hasher::new();
    let mut file   = std::fs::File::open(path)?;

    // Head
    let mut head = vec![0u8; chunk];
    let n = file.read(&mut head)?;
    hasher.update(&head[..n]);

    // Tail
    if size > (2 * PARTIAL_CHUNK_SIZE) {
        use std::io::Seek;
        file.seek(std::io::SeekFrom::End(-(chunk as i64)))?;
        let mut tail = vec![0u8; chunk];
        let n = file.read(&mut tail)?;
        hasher.update(&tail[..n]);
    }

    // Size as part of the hash (avoids collisions between different files with the same head)
    hasher.update(&size.to_le_bytes());

    Ok(hasher.finalize().to_hex().to_string())
}
