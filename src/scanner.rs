use anyhow::Result;
use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use walkdir::WalkDir;

use crate::archive::extract_media_files;
use crate::db::Database;
use crate::models::*;
use crate::parsers;

/// For files larger than this limit, only hash head + tail + size.
/// Avoids reading 50 GB of video into RAM. The hash remains
/// unique enough for practical deduplication.
const PARTIAL_HASH_THRESHOLD: u64 = 100 * 1024 * 1024; // 100 MB
const PARTIAL_CHUNK_SIZE: u64 = 4 * 1024 * 1024; //   4 MB per side

pub struct Scanner {
    db: Arc<Mutex<Database>>,
    verbose: bool,
    no_archives: bool,
    /// Optional GUI progress sink — updated from the worker threads.
    pub gui_progress: Option<Arc<GuiProgress>>,
}

impl Scanner {
    pub fn new(db: Database, verbose: bool, no_archives: bool) -> Self {
        Self {
            db: Arc::new(Mutex::new(db)),
            verbose,
            no_archives,
            gui_progress: None,
        }
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
            // Safety net: prevent infinite loops from circular symlinks (Fix #10)
            .max_depth(256)
            .into_iter()
            .filter_map(|e| match e {
                Ok(entry) => Some(entry),
                Err(err) => {
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
            eprintln!(
                "  {} {} inaccessible directory/ies ignored",
                "⚠".yellow(),
                walk_errors
            );
        }
        println!("{} {} files found", "→".green(), entries.len());

        let pb = ProgressBar::new(entries.len() as u64);
        pb.set_style(
            ProgressStyle::with_template(
                "{spinner:.cyan} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} {msg}",
            )?
            .progress_chars("█▓░"),
        );

        let gui_prog = self.gui_progress.clone();
        let total_files = entries.len();
        let done_counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        // Macro-like closure: increment both the indicatif bar and the GUI counter.
        // Captures pb and done_counter by reference.
        let tick = |pb: &ProgressBar, done_counter: &Arc<std::sync::atomic::AtomicUsize>,
                    gui_prog: &Option<Arc<GuiProgress>>, file: &str| {
            pb.inc(1);
            let n = done_counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            if let Some(gp) = gui_prog {
                gp.update(n, total_files, file);
            }
        };

        entries.par_iter().for_each(|path| {
            let file_name = path
                .file_name()
                .map(|n| n.to_string_lossy().chars().take(45).collect::<String>())
                .unwrap_or_default();
            pb.set_message(file_name.clone());

            let ext = path
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();

            let name_lower = path
                .file_name()
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
                tick(&pb, &done_counter, &gui_prog, &file_name);
                return;
            }

            // ── Archive ────────────────────────────────────────────────────
            let is_archive = ArchiveType::from_path(path).is_some();

            if !self.no_archives {
                if let Some(archive_type) = ArchiveType::from_path(path) {
                    // Skip extra parts of multi-part archives
                    if archive_type == ArchiveType::Rar && is_rar_multipart(&name_lower) {
                        let is_first =
                            name_lower.contains(".part1.") || name_lower.contains(".part01.");
                        if !is_first {
                            tick(&pb, &done_counter, &gui_prog, &file_name);
                            return;
                        }
                    }
                    if archive_type == ArchiveType::SevenZip && is_7z_multipart(&name_lower) {
                        if !name_lower.ends_with(".001") {
                            tick(&pb, &done_counter, &gui_prog, &file_name);
                            return;
                        }
                    }

                    // ── Archive incremental cache ─────────────────────────
                    // For single-part archives: mtime+size of the file itself.
                    // For multi-part archives: mtime+size of part-1 PLUS the
                    // combined size of all sibling parts — so that a change in
                    // any part invalidates the cache entry.
                    let archive_path_str = path.to_string_lossy().to_string();
                    let arch_meta = std::fs::metadata(path).ok();
                    let arch_mtime = arch_meta
                        .as_ref()
                        .and_then(|m| m.modified().ok())
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs());
                    let part1_size = arch_meta.as_ref().map(|m| m.len());
                    let arch_size = part1_size.map(|s| {
                        s + multipart_siblings_size(path, &archive_type, &name_lower)
                    });

                    if let (Some(mtime), Some(size)) = (arch_mtime, arch_size) {
                        if self.db.lock().unwrap().is_archive_cached(&archive_path_str, mtime, size) {
                            let mut s = stats.lock().unwrap();
                            s.skipped_cached += 1;
                            tick(&pb, &done_counter, &gui_prog, &file_name);
                            return;
                        }
                    }

                    // Heavy work WITHOUT lock: extract + build entries
                    match extract_media_files(path, &archive_type) {
                        Ok(files) => {
                            let built: Vec<MediaEntry> = files
                                .into_iter()
                                .map(|extracted| {
                                    let mt = MediaType::from_extension(&extracted.ext)
                                        .unwrap_or(MediaType::Other);
                                    build_entry_from_memory(
                                        &extracted.data,
                                        &extracted.name,
                                        &extracted.ext,
                                        &mt,
                                        path.to_string_lossy().as_ref(),
                                    )
                                })
                                .collect();

                            // Brief lock: insert the whole batch and record cache
                            let mut s = stats.lock().unwrap();
                            s.archives_opened += 1;
                            for entry in built {
                                self.insert_entry(entry, &mut s);
                            }
                            if let (Some(mtime), Some(size)) = (arch_mtime, arch_size) {
                                let _ = self.db.lock().unwrap()
                                    .mark_archive_processed(&archive_path_str, mtime, size);
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

            tick(&pb, &done_counter, &gui_prog, &file_name);
        });

        pb.finish_with_message("done");
        Ok(Arc::try_unwrap(stats).unwrap().into_inner().unwrap())
    }

    // ── Build entry without touching DB or stats (heavy work) ────────────

    fn build_entry(&self, path: &Path, ext: &str, media_type: &MediaType) -> Result<MediaEntry> {
        let meta = std::fs::metadata(path)?;
        let size = meta.len();
        let path_str = path.to_string_lossy().to_string();

        // ── Incremental re-scan: check cache by mtime + size ─────────────
        // If the file is already indexed and neither size nor modification
        // timestamp changed, reuse the cached hash without reading the file —
        // saves I/O on re-scans and allows resuming after interruptions.
        let current_mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs());

        if let Some(mtime) = current_mtime {
            let cached = self.db.lock().unwrap().find_by_path(&path_str);
            if let Some(c) = cached {
                if c.size_bytes == size && c.mtime == Some(mtime) {
                    // No changes — return entry with cached hash
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    return Ok(MediaEntry {
                        blake3_hash: c.blake3_hash,
                        size_bytes: size,
                        original_name: name,
                        current_path: path_str,
                        extension: ext.to_string(),
                        media_type: media_type.clone(),
                        metadata: Metadata::None, // already stored in DB
                        source_archive: None,
                        path_in_archive: None,
                        mtime: Some(mtime),
                        from_cache: true,
                    });
                }
            }
        }

        // ── New or modified file: hash and parse ─────────────────────────
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();

        // For files within the threshold, read once and reuse the buffer
        // for both hashing and metadata parsing (Fix #5: avoids double I/O).
        let (hash, metadata) = if size <= PARTIAL_HASH_THRESHOLD {
            let data = std::fs::read(path)?;
            let h = blake3::hash(&data).to_hex().to_string();
            let m = match media_type {
                MediaType::Other => Metadata::None,
                MediaType::Video => Metadata::Video(parsers::video::parse_from_path(&path_str)),
                _ => parsers::parse(&data, ext, media_type, &path_str),
            };
            (h, m)
        } else {
            // Large file: partial hash (head+tail), no metadata extraction
            let h = hash_file(path, size)?;
            let m = match media_type {
                MediaType::Video => Metadata::Video(parsers::video::parse_from_path(&path_str)),
                _ => Metadata::None,
            };
            (h, m)
        };

        Ok(MediaEntry {
            blake3_hash: hash,
            size_bytes: size,
            original_name: name,
            current_path: path_str,
            extension: ext.to_string(),
            media_type: media_type.clone(),
            metadata,
            source_archive: None,
            path_in_archive: None,
            mtime: current_mtime,
            from_cache: false,
        })
    }

    // ── Insert into DB + update stats (fast, under lock) ─────────────────

    fn insert_entry(&self, entry: MediaEntry, stats: &mut ScanStats) {
        let media_type = entry.media_type.clone();
        let size = entry.size_bytes;
        let path = entry.current_path.clone();
        let from_cache = entry.from_cache;
        let is_partial = entry.source_archive.is_none() && size > PARTIAL_HASH_THRESHOLD;

        match self.db.lock().unwrap().insert(&entry) {
            Ok(_) if from_cache => {
                // Incremental re-scan: mtime+size matched → no hash I/O needed.
                stats.skipped_cached += 1;
            }
            Ok((_, true, Some(orig))) => {
                stats.duplicates += 1;
                stats.bytes_dup += size;
                if is_partial {
                    stats.partial_hashes += 1;
                }
                if self.verbose {
                    eprintln!("  {} dupl: {} ← {}", "≡".yellow(), path, orig);
                }
            }
            Ok(_) => {
                if is_partial {
                    stats.partial_hashes += 1;
                }
                match media_type {
                    MediaType::Print3D => stats.indexed_3d += 1,
                    MediaType::Video => stats.indexed_video += 1,
                    MediaType::Audio => stats.indexed_audio += 1,
                    MediaType::Image => stats.indexed_image += 1,
                    MediaType::Other => stats.indexed_other += 1,
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
                let name_lower = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_lowercase())
                    .unwrap_or_default();

                // Ignore extra parts of multi-part archives
                if archive_type == ArchiveType::Rar && is_rar_multipart(&name_lower) {
                    if !name_lower.contains(".part1.") && !name_lower.contains(".part01.") {
                        return;
                    }
                }
                if archive_type == ArchiveType::SevenZip && is_7z_multipart(&name_lower) {
                    if !name_lower.ends_with(".001") {
                        return;
                    }
                }

                let archive_path_str = path.to_string_lossy().to_string();
                let arch_meta = std::fs::metadata(path).ok();
                let arch_mtime = arch_meta
                    .as_ref()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs());
                let arch_size = arch_meta.as_ref().map(|m| m.len()).map(|s| {
                    s + multipart_siblings_size(path, &archive_type, &name_lower)
                });

                if let (Some(mtime), Some(size)) = (arch_mtime, arch_size) {
                    if self.db.lock().unwrap().is_archive_cached(&archive_path_str, mtime, size) {
                        return;
                    }
                }

                match extract_media_files(path, &archive_type) {
                    Ok(files) => {
                        let built: Vec<MediaEntry> = files
                            .into_iter()
                            .map(|extracted| {
                                let mt = MediaType::from_extension(&extracted.ext)
                                    .unwrap_or(MediaType::Other);
                                build_entry_from_memory(
                                    &extracted.data,
                                    &extracted.name,
                                    &extracted.ext,
                                    &mt,
                                    path.to_string_lossy().as_ref(),
                                )
                            })
                            .collect();

                        let mut dummy = ScanStats::default();
                        for entry in built {
                            self.insert_entry(entry, &mut dummy);
                        }
                        if let (Some(mtime), Some(size)) = (arch_mtime, arch_size) {
                            let _ = self.db.lock().unwrap()
                                .mark_archive_processed(&archive_path_str, mtime, size);
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
    data: &[u8],
    name: &str,
    ext: &str,
    media_type: &MediaType,
    archive_path: &str,
) -> MediaEntry {
    let hash = blake3::hash(data).to_hex().to_string();
    let metadata = parsers::parse(data, ext, media_type, "");

    MediaEntry {
        blake3_hash: hash,
        size_bytes: data.len() as u64,
        original_name: name.to_string(),
        current_path: format!("{}::{}", archive_path, name),
        extension: ext.to_string(),
        media_type: media_type.clone(),
        metadata,
        source_archive: Some(archive_path.to_string()),
        path_in_archive: Some(name.to_string()),
        mtime: None,
        from_cache: false,
    }
}

/// Returns the combined size (bytes) of all sibling parts of a multi-part
/// archive, excluding the first part (whose size is already counted by the
/// caller). Returns 0 for single-part archives.
///
/// For RAR: `archive.part1.rar` → sums `archive.part2.rar`, `.part3.rar`, …
/// For 7z:  `archive.7z.001`   → sums `archive.7z.002`, `.003`, …
fn multipart_siblings_size(first_part: &Path, archive_type: &ArchiveType, name_lower: &str) -> u64 {
    let dir = match first_part.parent() {
        Some(d) => d,
        None => return 0,
    };

    match archive_type {
        ArchiveType::Rar if is_rar_multipart(name_lower) => {
            // Stem before ".partN.rar": "backup.part1.rar" → stem "backup"
            let stem = name_lower
                .rfind(".part")
                .map(|i| &name_lower[..i])
                .unwrap_or("");
            if stem.is_empty() {
                return 0;
            }
            let mut total = 0u64;
            if let Ok(rd) = std::fs::read_dir(dir) {
                for entry in rd.flatten() {
                    let sibling = entry.file_name().to_string_lossy().to_lowercase();
                    if sibling == *name_lower {
                        continue; // skip first part itself
                    }
                    if sibling.starts_with(stem)
                        && sibling.ends_with(".rar")
                        && is_rar_multipart(&sibling)
                    {
                        total += entry.metadata().map(|m| m.len()).unwrap_or(0);
                    }
                }
            }
            total
        }
        ArchiveType::SevenZip if is_7z_multipart(name_lower) => {
            // Stem before ".001": "backup.7z.001" → prefix "backup.7z."
            let prefix = name_lower
                .strip_suffix("001")
                .unwrap_or("");
            if prefix.is_empty() {
                return 0;
            }
            let mut total = 0u64;
            if let Ok(rd) = std::fs::read_dir(dir) {
                for entry in rd.flatten() {
                    let sibling = entry.file_name().to_string_lossy().to_lowercase();
                    if sibling == *name_lower {
                        continue;
                    }
                    if sibling.starts_with(prefix) && is_7z_multipart(&sibling) {
                        total += entry.metadata().map(|m| m.len()).unwrap_or(0);
                    }
                }
            }
            total
        }
        _ => 0,
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
    let mut file = std::fs::File::open(path)?;

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
