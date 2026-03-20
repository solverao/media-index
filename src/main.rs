mod archive;
mod db;
mod models;
mod parsers;
mod scanner;
mod thumbs;

use std::path::PathBuf;
use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use colored::Colorize;
use humansize::{format_size, DECIMAL};
use indicatif::{ProgressBar, ProgressStyle};

use db::Database;
use scanner::Scanner;
use parsers::video::ffprobe_available;

// ── CLI ───────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name    = "media-index",
    about   = "3D, video, audio and image file indexer with deduplication",
    version = "0.1.0",
)]
struct Cli {
    #[arg(short, long, default_value = "media.db")]
    db: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Scan a directory and index all media files
    Scan {
        path: PathBuf,
        #[arg(short, long)]
        verbose: bool,
        /// Do not open archives (.zip, .rar, .7z): index only loose files.
        /// Compressed files are registered as regular files (the archive itself is hashed).
        #[arg(long)]
        no_archives: bool,
    },

    /// Watch a directory and index changes in real time
    Watch {
        path: PathBuf,
        #[arg(short, long)]
        verbose: bool,
        /// Seconds to wait before processing an event (default: 2)
        #[arg(short, long, default_value = "2")]
        debounce: u64,
        /// Do not open archives (.zip, .rar, .7z): index only loose files.
        #[arg(long)]
        no_archives: bool,
    },

    /// General collection statistics
    Stats,

    /// List duplicates (by type or all)
    Dupes {
        #[arg(short, long)]
        r#type: Option<MediaTypeArg>,
        #[arg(short, long)]
        json: bool,
        /// Delete duplicates from disk. Those inside archives are reported but not touched.
        #[arg(short, long)]
        delete: bool,
        /// With --delete: show what would be deleted without actually deleting anything.
        #[arg(long)]
        dry_run: bool,
        /// With --delete: if ALL files in an archive are duplicates, delete the whole archive.
        #[arg(short, long)]
        aggressive: bool,
        /// With --delete: if the canonical is a loose file and already exists inside an archive,
        /// delete the loose file (the content remains in the archive).
        #[arg(short = 'p', long)]
        prefer_archive: bool,
        /// Rule for choosing which copy to keep: oldest|newest|largest|smallest|shortest-path
        /// Default: uses copy_score heuristic (keeps the most "original-looking" file).
        #[arg(long, value_name = "RULE")]
        keep: Option<String>,
    },

    /// Find visually or tonally similar files (images by perceptual hash, audio by tags)
    Similar {
        #[arg(value_enum)]
        kind: SimilarKind,
        /// Hamming distance threshold for images (0=identical .. 64=max, default 10)
        #[arg(short = 'u', long, default_value = "10")]
        threshold: u32,
        #[arg(short, long)]
        json: bool,
    },

    /// Find empty files and/or empty directories
    Empty {
        path: PathBuf,
        /// Only report empty directories
        #[arg(long)]
        dirs_only: bool,
        /// Only report empty files
        #[arg(long)]
        files_only: bool,
        /// Delete the found items
        #[arg(short, long)]
        delete: bool,
        /// Show what would be deleted without deleting anything
        #[arg(long)]
        dry_run: bool,
    },

    /// Find and optionally remove broken symbolic links
    Broken {
        path: PathBuf,
        /// Delete the broken symlinks
        #[arg(short, long)]
        delete: bool,
        /// Show what would be deleted without deleting anything
        #[arg(long)]
        dry_run: bool,
    },

    /// Search files by name
    Search {
        query: String,
        #[arg(short, long)]
        r#type: Option<MediaTypeArg>,
    },

    /// Export index to JSON
    Export {
        #[arg(short, long, default_value = "media_export.json")]
        output: PathBuf,
    },

    /// Check optional dependencies
    Doctor,

    /// Re-hash indexed files and detect modified, corrupted or missing ones
    Verify {
        /// Remove from the DB entries whose files no longer exist or are corrupted
        #[arg(short, long)]
        prune: bool,
        /// Show only files with problems (skip OK ones)
        #[arg(short, long)]
        quiet: bool,
    },

    /// Generate thumbnails for images, videos and 3D models
    Thumbs {
        /// Filter by type (generates all three types by default)
        #[arg(short, long)]
        r#type: Option<MediaTypeArg>,
        /// Thumbnail square side size in pixels
        #[arg(short, long, default_value = "256")]
        size: u32,
        /// JPEG quality (1-100)
        #[arg(short, long, default_value = "85")]
        quality: u8,
        /// Regenerate thumbnails that already exist
        #[arg(short, long)]
        force: bool,
        /// Show detailed errors for each file
        #[arg(short, long)]
        verbose: bool,
    },

    /// Remove unwanted DB entries without deleting files from disk
    Clean {
        /// Remove macOS junk entries (__MACOSX/, ._, .DS_Store) indexed by mistake
        #[arg(long)]
        macos_junk: bool,
    },

    /// Delete the entire database (asks for confirmation)
    Clear {
        /// Skip confirmation prompt (useful in scripts)
        #[arg(short, long)]
        force: bool,
    },
}

#[derive(Clone, ValueEnum)]
enum MediaTypeArg { Td, Video, Audio, Image, Other }

impl MediaTypeArg {
    fn as_db_str(&self) -> &'static str {
        match self {
            Self::Td    => "3d",
            Self::Video => "video",
            Self::Audio => "audio",
            Self::Image => "image",
            Self::Other => "other",
        }
    }
}

#[derive(Clone, ValueEnum)]
enum SimilarKind { Images, Audio }

// ── Main ──────────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Clear does not need to open (or create) the DB
    if let Commands::Clear { force } = cli.command {
        return cmd_clear(&cli.db, force);
    }

    let db = Database::open(&cli.db)?;

    match cli.command {
        Commands::Scan { path, verbose, no_archives }  => cmd_scan(db, &path, verbose, no_archives),
        Commands::Watch { path, verbose, debounce, no_archives } => cmd_watch(db, &path, verbose, debounce, no_archives),
        Commands::Stats                   => cmd_stats(db),
        Commands::Dupes { r#type, json, delete, dry_run, aggressive, prefer_archive, keep } =>
            cmd_dupes(db, r#type, json, delete, dry_run, aggressive, prefer_archive, keep),
        Commands::Search { query, r#type } => cmd_search(db, &query, r#type),
        Commands::Export { output }        => cmd_export(db, &output),
        Commands::Doctor                   => cmd_doctor(),
        Commands::Verify { prune, quiet }  => cmd_verify(db, prune, quiet),
        Commands::Clean { macos_junk }     => cmd_clean(db, macos_junk),
        Commands::Thumbs { r#type, size, quality, force, verbose } =>
            cmd_thumbs(db, &cli.db, r#type, size, quality, force, verbose),
        Commands::Similar { kind, threshold, json } => cmd_similar(db, kind, threshold, json),
        Commands::Empty { path, dirs_only, files_only, delete, dry_run } =>
            cmd_empty(&path, dirs_only, files_only, delete, dry_run),
        Commands::Broken { path, delete, dry_run } =>
            cmd_broken(&path, delete, dry_run),
        Commands::Clear { .. } => unreachable!(),
    }
}

// ── Commands ──────────────────────────────────────────────────────────────

fn cmd_scan(db: Database, path: &std::path::Path, verbose: bool, no_archives: bool) -> Result<()> {
    if !path.exists() {
        anyhow::bail!("Directory does not exist: {}", path.display());
    }

    if !ffprobe_available() {
        println!("{} ffprobe not found — video metadata unavailable",
            "⚠".yellow());
        println!("  Install: sudo apt install ffmpeg  /  brew install ffmpeg\n");
    }

    if no_archives {
        println!("{}", "  [--no-archives] The compressed files will be indexed as regular files, without opening their contents.".dimmed());
    }

    println!("{}", format!("Scanning: {}", path.display()).bold().cyan());

    let scanner = Scanner::new(db, verbose, no_archives);
    let s       = scanner.scan(path)?;

    println!("\n{}", "─── Result ───────────────────────────────────".dimmed());
    println!("  {} 3D files",   s.indexed_3d.to_string().green().bold());
    println!("  {} videos",     s.indexed_video.to_string().blue().bold());
    println!("  {} audio",      s.indexed_audio.to_string().magenta().bold());
    println!("  {} images",     s.indexed_image.to_string().yellow().bold());
    println!("  {} other",      s.indexed_other.to_string().white().bold());
    println!("  {} archives",   s.archives_opened.to_string().dimmed());
    println!("  {} duplicates ({})", s.duplicates.to_string().red().bold(),
        format_size(s.bytes_dup, DECIMAL).red());
    if s.skipped_cached > 0 {
        println!("  {} unchanged (cached)",
            s.skipped_cached.to_string().dimmed());
    }
    if s.errors > 0 {
        println!("  {} errors", s.errors.to_string().red());
    }
    println!("  {}", "──────────────────────────────────────".dimmed());
    println!("  {} indexed total", s.total_indexed().to_string().cyan().bold());

    if s.partial_hashes > 0 {
        println!("\n  {} {} large file(s) partially hashed (>{}) — approximate deduplication",
            "⚠".yellow(),
            s.partial_hashes.to_string().yellow(),
            "100 MB".bold());
        println!("  {} Use {} to detect false positives.",
            " ".normal(),
            "verify".bold());
    }
 
    Ok(())
}

fn cmd_watch(db: Database, path: &std::path::Path, verbose: bool, debounce_secs: u64, no_archives: bool) -> Result<()> {
    use notify_debouncer_mini::{new_debouncer, notify::RecursiveMode, DebouncedEventKind};
    use std::time::Duration;

    if !path.exists() {
        anyhow::bail!("Directory does not exist: {}", path.display());
    }

    if !ffprobe_available() {
        println!("{} ffprobe not found — video metadata unavailable", "⚠".yellow());
        println!("  Install: sudo apt install ffmpeg  /  brew install ffmpeg\n");
    }

    // Initial full scan
    println!("{}", format!("Initial scan: {}", path.display()).bold().cyan());
    let scanner = Scanner::new(db, verbose, no_archives);
    let s = scanner.scan(path)?;
    println!("  {} indexed  {} duplicates\n",
        s.total_indexed().to_string().cyan().bold(),
        s.duplicates.to_string().red());

    let (tx, rx) = std::sync::mpsc::channel();

    let mut debouncer = new_debouncer(
        Duration::from_secs(debounce_secs),
        move |res| { let _ = tx.send(res); },
    )?;

    debouncer.watcher().watch(path, RecursiveMode::Recursive)?;

    println!("{} Watching {}  {}",
        "👁",
        path.display().to_string().bold(),
        format!("(debounce {}s — Ctrl+C to quit)", debounce_secs).dimmed());

    for events in rx {
        let events = match events {
            Ok(e)  => e,
            Err(e) => { eprintln!("{} Watcher error: {e:?}", "✗".red()); continue; }
        };

        // Deduplicate paths in this batch
        let mut to_index:  std::collections::HashSet<PathBuf> = Default::default();
        let mut had_remove = false;

        for event in events {
            match event.kind {
                DebouncedEventKind::Any => {
                    let p = &event.path;
                    if p.is_file() {
                        to_index.insert(p.clone());
                    } else if !p.exists() {
                        had_remove = true;
                    }
                }
                _ => {}
            }
        }

        // Deletions → cleanup in DB
        if had_remove {
            match scanner.cleanup() {
                Ok((f, d)) if f > 0 || d > 0 =>
                    println!("  {} {} entry/ies removed from DB", "🧹", f + d),
                _ => {}
            }
        }

        // New / modified → index
        for file_path in &to_index {
            let ext = file_path.extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            let name = file_path.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();

            println!("  {} {}", "→".cyan(), name.bold());
            scanner.index_single(file_path, &ext);
        }
    }

    Ok(())
}

fn cmd_stats(db: Database) -> Result<()> {
    let s = db.stats()?;

    println!("{}", "─── Collection ───────────────────────────────".bold().cyan());
    println!("  Unique total   : {}", s.total.to_string().green().bold());
    println!("  Duplicates     : {}", s.dupes.to_string().red());
    println!("  Total size     : {}", format_size(s.bytes as u64, DECIMAL).yellow());
    println!("  Saved by dedup : {}", format_size(s.bytes_dup as u64, DECIMAL).red());

    println!("\n  {:>8}  {:>12}  {}", "Files", "Size", "Type");
    println!("  {}", "─".repeat(36).dimmed());

    let icons = [("3d", "⬡"), ("video", "▶"), ("audio", "♪"), ("image", "🖼"), ("other", "·")];
    for (type_str, count, bytes) in &s.by_type {
        let icon = icons.iter().find(|(k, _)| k == type_str).map(|(_, v)| *v).unwrap_or("·");
        println!("  {:>8}  {:>12}  {} {}",
            count.to_string().cyan(),
            format_size(*bytes as u64, DECIMAL).dimmed(),
            icon,
            type_str.as_str().to_uppercase().bold(),
        );
    }

    Ok(())
}

fn cmd_dupes(
    db:             Database,
    tipo:           Option<MediaTypeArg>,
    as_json:        bool,
    delete:         bool,
    dry_run:        bool,
    aggressive:     bool,
    prefer_archive: bool,
    keep:           Option<String>,
) -> Result<()> {
    use models::KeepRule;

    let keep_rule: Option<KeepRule> = match &keep {
        None    => None,
        Some(s) => match KeepRule::from_str(s) {
            Some(r) => Some(r),
            None    => {
                eprintln!("{} Unknown rule: '{}'. Options: oldest|newest|largest|smallest|shortest-path",
                    "✗".red(), s);
                return Ok(());
            }
        },
    };

    let mut groups = db.duplicates()?;

    if let Some(t) = &tipo {
        let filter = t.as_db_str();
        groups.retain(|g| g.media_type == filter);
    }

    if groups.is_empty() {
        println!("{}", "No duplicates found 🎉".green());
        return Ok(());
    }

    if as_json {
        let json: Vec<_> = groups.iter().map(|g| serde_json::json!({
            "hash":           g.hash,
            "media_type":     g.media_type,
            "canonical_name": g.canonical_name,
            "canonical_path": g.canonical_path,
            "size_bytes":     g.size_bytes,
            "duplicates":     g.duplicates,
        })).collect();
        println!("{}", serde_json::to_string_pretty(&json)?);
        return Ok(());
    }

    let total_bytes: u64 = groups.iter()
        .map(|g| g.size_bytes * g.duplicates.len() as u64)
        .sum();

    println!("{} duplicate groups  —  {} reclaimable\n",
        groups.len().to_string().red().bold(),
        format_size(total_bytes, DECIMAL).red());

    if delete {
        return cmd_dupes_delete(&db, &groups, dry_run, aggressive, prefer_archive, keep_rule.as_ref());
    }

    // ── List only ─────────────────────────────────────────────────────────
    for g in &groups {
        print_dupe_group(g);
    }

    Ok(())
}

fn print_dupe_group(g: &db::DuplicateGroup) {
    let type_badge = match g.media_type.as_str() {
        "3d"    => "⬡ 3D".cyan(),
        "video" => "▶ VID".blue(),
        "audio" => "♪ AUD".magenta(),
        "image" => "🖼 IMG".yellow(),
        "other" => "· OTR".white(),
        _       => "? ???".normal(),
    };
    println!("{} {} {} ({})",
        "●".red(), type_badge, g.canonical_name.bold(),
        format_size(g.size_bytes, DECIMAL).dimmed());
    println!("  {}", &g.hash[..16].dimmed());
    println!("  {}", g.canonical_path.green());
    for d in &g.duplicates {
        let in_archive = d.contains("::");
        let marker = if in_archive { "⊡".yellow() } else { "↳".red() };
        println!("  {} {}", marker, d.yellow());
    }
    println!();
}

fn cmd_dupes_delete(
    db:             &Database,
    groups:         &[db::DuplicateGroup],
    dry_run:        bool,
    aggressive:     bool,
    prefer_archive: bool,
    keep_rule:      Option<&models::KeepRule>,
) -> Result<()> {
    if dry_run {
        println!("{}", "  [DRY-RUN] No files will be modified.\n".yellow().bold());
    }

    if let Some(rule) = keep_rule {
        println!("  {} Using keep rule: {:?}\n", "→".cyan(), rule);
    }

    // ── Apply KeepRule: recalculate which paths are "duplicates" in each group ──
    // When the user passes --keep, we ignore the canonical/duplicate split from
    // the DB and decide ourselves who survives, marking the rest for deletion.
    let plans_from_keep: Option<Vec<String>> = keep_rule.map(|rule| {
        let mut to_delete: Vec<String> = vec![];
        for g in groups {
            // Build the full group list: canonical + duplicates
            let mut all: Vec<String> = vec![g.canonical_path.clone()];
            all.extend(g.duplicates.iter().cloned());
            // Keep only loose files (not inside archives) so we can read
            // their filesystem metadata
            let loose: Vec<&String> = all.iter().filter(|p| !p.contains("::")).collect();
            if loose.is_empty() { continue; }

            // Choose the winner according to the rule
            let winner = match rule {
                models::KeepRule::Oldest => loose.iter().min_by_key(|p| {
                    std::fs::metadata(p).and_then(|m| m.modified()).ok()
                }),
                models::KeepRule::Newest => loose.iter().max_by_key(|p| {
                    std::fs::metadata(p).and_then(|m| m.modified()).ok()
                }),
                models::KeepRule::Largest => loose.iter().max_by_key(|p| {
                    std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)
                }),
                models::KeepRule::Smallest => loose.iter().min_by_key(|p| {
                    std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)
                }),
                models::KeepRule::ShortestPath => loose.iter().min_by_key(|p| p.len()),
            };

            if let Some(keep_path) = winner {
                // Mark all others for deletion
                for p in &all {
                    if p != *keep_path && !p.contains("::") {
                        to_delete.push(p.clone());
                    }
                }
            }
        }
        to_delete
    });

    // Classify each duplicate_path as: loose file vs. inside archive
    struct DeletePlan {
        path:         String,         // full duplicate_path
        archive_path: Option<String>, // Some("/a/b.zip") if "b.zip::foo.jpg"
    }

    // If a KeepRule was given, plans come from it. Otherwise, use the normal duplicates.
    let plans: Vec<DeletePlan> = match &plans_from_keep {
        Some(paths) => paths.iter().map(|p| DeletePlan {
            path: p.clone(),
            archive_path: None,
        }).collect(),
        None => groups.iter()
            .flat_map(|g| g.duplicates.iter())
            .map(|d| {
                let archive_path = if d.contains("::") {
                    d.splitn(2, "::").next().map(|s| s.to_string())
                } else {
                    None
                };
                DeletePlan { path: d.clone(), archive_path }
            })
            .collect(),
    };

    // ── --prefer-archive: loose canonicals that already live in an archive ──
    let mut deleted_files     = 0usize;
    let mut freed_bytes       = 0u64;
    let mut errors_delete     = 0usize;
    // Paths of canonicals deleted in this step — needed so that
    // --aggressive does not assume the archive is the only copy when
    // the canonical was just deleted in the same run.
    let mut deleted_canonicals: std::collections::HashSet<String> = Default::default();

    if prefer_archive {
        for g in groups {
            let canonical_is_loose = !g.canonical_path.contains("::");
            let all_dupes_in_archive = !g.duplicates.is_empty()
                && g.duplicates.iter().all(|d| d.contains("::"));

            if canonical_is_loose && all_dupes_in_archive {
                let p = std::path::Path::new(&g.canonical_path);
                let size = p.metadata().map(|m| m.len()).unwrap_or(0);
                if dry_run {
                    freed_bytes   += size;
                    deleted_files += 1;
                    deleted_canonicals.insert(g.canonical_path.clone());
                    println!("  {} {} {}",
                        "~".cyan(),
                        g.canonical_path.dimmed(),
                        "(loose canonical — copy in archive)".dimmed());
                } else {
                    match p.metadata() {
                        Ok(meta) => {
                            match std::fs::remove_file(p) {
                                Ok(_) => {
                                    freed_bytes   += meta.len();
                                    deleted_files += 1;
                                    deleted_canonicals.insert(g.canonical_path.clone());
                                    println!("  {} {} {}",
                                        "✓".green(),
                                        g.canonical_path.dimmed(),
                                        "(loose canonical — copy in archive)".dimmed());
                                }
                                Err(e) => {
                                    errors_delete += 1;
                                    eprintln!("  {} {}: {e}", "✗".red(), g.canonical_path);
                                }
                            }
                        }
                        Err(_) => {
                            deleted_files += 1;
                            deleted_canonicals.insert(g.canonical_path.clone());
                            println!("  {} {} (no longer existed)", "·".dimmed(), g.canonical_path.dimmed());
                        }
                    }
                }
            }
        }
    }

    // ── Loose files ───────────────────────────────────────────────────────
    let loose: Vec<&DeletePlan> = plans.iter().filter(|p| p.archive_path.is_none()).collect();

    for plan in &loose {
        let p = std::path::Path::new(&plan.path);
        let size = p.metadata().map(|m| m.len()).unwrap_or(0);
        if dry_run {
            freed_bytes   += size;
            deleted_files += 1;
            println!("  {} {}", "~".cyan(), plan.path.dimmed());
        } else {
            match p.metadata() {
                Ok(meta) => {
                    match std::fs::remove_file(p) {
                        Ok(_) => {
                            freed_bytes   += meta.len();
                            deleted_files += 1;
                            println!("  {} {}", "✓".green(), plan.path.dimmed());
                        }
                        Err(e) => {
                            errors_delete += 1;
                            eprintln!("  {} {}: {e}", "✗".red(), plan.path);
                        }
                    }
                }
                Err(_) => {
                    deleted_files += 1;
                    println!("  {} {} (no longer existed)", "·".dimmed(), plan.path.dimmed());
                }
            }
        }
    }

    // ── Files inside archives ──────────────────────────────────────────────
    let in_archives: Vec<&DeletePlan> = plans.iter().filter(|p| p.archive_path.is_some()).collect();

    if !in_archives.is_empty() && !aggressive {
        println!("\n{} {} duplicate(s) inside archives — left untouched:",
            "⊡".yellow().bold(),
            in_archives.len().to_string().yellow());
        for plan in &in_archives {
            println!("  {} {}", "·".yellow(), plan.path.yellow());
        }
        println!("  {} Use {} to delete the archive if all its files are duplicates.",
            "→".dimmed(),
            "--aggressive".bold());
    }

    // ── Aggressive mode: delete whole archives if all their content has copies ──
    let mut deleted_archives = 0usize;

    if aggressive && !in_archives.is_empty() {
        // Group duplicate_paths by archive
        let mut by_archive: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        for plan in &in_archives {
            if let Some(arc) = &plan.archive_path {
                *by_archive.entry(arc.clone()).or_insert(0) += 1;
            }
        }

        // Iterative convergence: mark archives as deletable only if
        // all their content has a copy outside the deletable set.
        // Repeat until the set no longer changes (fixpoint).
        let mut to_delete: std::collections::HashSet<String> = Default::default();
        loop {
            let prev_len = to_delete.len();
            for archive_path in by_archive.keys() {
                if to_delete.contains(archive_path) { continue; }
                let arc = std::path::Path::new(archive_path);
                if !arc.exists() {
                    to_delete.insert(archive_path.clone());
                    continue;
                }
                match db.can_safely_delete_archive(archive_path, &deleted_canonicals, &to_delete) {
                    Ok(true)  => { to_delete.insert(archive_path.clone()); }
                    Ok(false) => {}
                    Err(e)    => eprintln!("  {} {}: {e}", "✗".red(), archive_path),
                }
            }
            if to_delete.len() == prev_len { break; } // fixpoint reached
        }

        // Execute (or simulate) deletions
        for (archive_path, dup_count) in &by_archive {
            let arc = std::path::Path::new(archive_path);

            if !arc.exists() {
                println!("  {} {} (no longer existed)", "·".dimmed(), archive_path.dimmed());
                deleted_archives += 1;
                continue;
            }

            if to_delete.contains(archive_path) {
                let bytes = arc.metadata().map(|m| m.len()).unwrap_or(0);
                if dry_run {
                    freed_bytes      += bytes;
                    deleted_archives += 1;
                    println!("  {} {} {}",
                        "~".cyan(),
                        "full archive:".dimmed(),
                        archive_path.dimmed());
                } else {
                    match std::fs::remove_file(arc) {
                        Ok(_) => {
                            freed_bytes      += bytes;
                            deleted_archives += 1;
                            println!("  {} {} {}",
                                "✓".green(),
                                "full archive:".dimmed(),
                                archive_path.dimmed());
                        }
                        Err(e) => {
                            errors_delete += 1;
                            eprintln!("  {} {}: {e}", "✗".red(), archive_path);
                        }
                    }
                }
            } else {
                println!("  {} {} — has unique files or is the only copy, skipping ({} duplicate(s) inside)",
                    "⊡".yellow(),
                    archive_path.yellow(),
                    dup_count);
            }
        }
    }

    // ── Summary ────────────────────────────────────────────────────────────
    println!("\n{}", "─── Result ───────────────────────────────────".dimmed());
    if dry_run {
        println!("  {} {}", "[DRY-RUN]".cyan().bold(),
            "nothing deleted — run without --dry-run to apply".dimmed());
        if deleted_files > 0 {
            println!("  {} file(s) would be deleted", deleted_files.to_string().cyan().bold());
        }
        if deleted_archives > 0 {
            println!("  {} archive(s) would be deleted", deleted_archives.to_string().cyan().bold());
        }
        println!("  {} would be freed", format_size(freed_bytes, DECIMAL).cyan().bold());
    } else {
        if deleted_files > 0 {
            println!("  {} file(s) deleted", deleted_files.to_string().green().bold());
        }
        if deleted_archives > 0 {
            println!("  {} archive(s) deleted", deleted_archives.to_string().green().bold());
        }
        println!("  {} freed", format_size(freed_bytes, DECIMAL).red().bold());
        if errors_delete > 0 {
            println!("  {} error(s)", errors_delete.to_string().red());
        }
        // Sync the DB with what was just deleted from disk
        if deleted_files > 0 || deleted_archives > 0 {
            match db.cleanup_stale() {
                Ok((f, d)) if f > 0 || d > 0 =>
                    println!("  {} DB synced ({} entry/ies removed)", "🧹".dimmed(), f + d),
                Ok(_)  => {}
                Err(e) => eprintln!("  {} Error syncing DB: {e}", "✗".red()),
            }
        }
    }

    Ok(())
}

fn cmd_search(db: Database, query: &str, tipo: Option<MediaTypeArg>) -> Result<()> {
    use db::SearchDetail;

    let type_filter = tipo.as_ref().map(|t| t.as_db_str());
    let results = db.search(query, type_filter)?;

    if results.is_empty() {
        println!("No results for \"{}\"", query);
        return Ok(());
    }

    println!("{} resultados\n", results.len().to_string().cyan());

    for r in &results {
        let badge = match r.media_type.as_str() {
            "3d"    => "[3D]".cyan(),
            "video" => "[VID]".blue(),
            "audio" => "[AUD]".magenta(),
            "image" => "[IMG]".yellow(),
            "other" => "[OTR]".white(),
            _       => "[?]".normal(),
        };

        println!("{} {} {}",
            "▸".cyan(), badge, r.name.bold());
        println!("  {}", r.path.dimmed());

        let detail = &r.detail;
        let info: Vec<String> = match detail {
            SearchDetail::Audio { duration, artist, title, album } => {
                let mut v = vec![format_size(r.size_bytes, DECIMAL)];
                if let Some(d) = duration { v.push(fmt_duration(*d)); }
                if let Some(a) = artist   { v.push(a.clone()); }
                if let Some(t) = title    { v.push(t.clone()); }
                if let Some(al) = album   { v.push(al.clone()); }
                v
            }
            SearchDetail::Video { duration, width, height, title } => {
                let mut v = vec![format_size(r.size_bytes, DECIMAL)];
                if let Some(d) = duration { v.push(fmt_duration(*d)); }
                if let (Some(w), Some(h)) = (width, height) { v.push(format!("{w}×{h}")); }
                if let Some(t) = title { v.push(t.clone()); }
                v
            }
            SearchDetail::Image { width, height, camera } => {
                let mut v = vec![format_size(r.size_bytes, DECIMAL)];
                if let (Some(w), Some(h)) = (width, height) { v.push(format!("{w}×{h}px")); }
                if let Some(c) = camera { v.push(c.clone()); }
                v
            }
            SearchDetail::Print3D { triangles } => {
                let mut v = vec![format_size(r.size_bytes, DECIMAL), r.extension.to_uppercase()];
                if let Some(t) = triangles { v.push(format!("{t} triangles")); }
                v
            }
            SearchDetail::Other => {
                vec![format_size(r.size_bytes, DECIMAL), r.extension.to_uppercase()]
            }
        };

        println!("  {}\n", info.join(" · ").dimmed());
    }

    Ok(())
}

fn cmd_export(db: Database, output: &std::path::Path) -> Result<()> {
    let stats = db.stats()?;
    let dupes = db.duplicates()?;

    let payload = serde_json::json!({
        "stats": {
            "total": stats.total, "duplicates": stats.dupes,
            "bytes": stats.bytes, "by_type": stats.by_type,
        },
        "duplicates": dupes.iter().map(|g| serde_json::json!({
            "hash": g.hash, "media_type": g.media_type,
            "canonical_name": g.canonical_name,
            "canonical_path": g.canonical_path,
            "size_bytes":     g.size_bytes,
            "duplicates":     g.duplicates,
        })).collect::<Vec<_>>(),
    });

    std::fs::write(output, serde_json::to_string_pretty(&payload)?)?;
    println!("Exported to {}", output.display().to_string().green());
    Ok(())
}

fn cmd_doctor() -> Result<()> {
    println!("{}\n", "─── Dependency check ─────────────────────────".bold().cyan());

    let check = |name: &str, available: bool, install: &str| {
        if available {
            println!("  {} {}", "✓".green(), name.bold());
        } else {
            println!("  {} {} — install: {}", "✗".red(), name.bold(), install.dimmed());
        }
    };

    check(
        "ffprobe (video metadata)",
        ffprobe_available(),
        "sudo apt install ffmpeg  /  brew install ffmpeg",
    );

    check(
        "unrar (.rar files)",
        std::process::Command::new("unrar").arg("--help").output().is_ok(),
        "sudo apt install unrar  /  brew install rar",
    );

    check(
        "stl-thumb (3D thumbnails via OpenGL — better quality)",
        thumbs::stl_thumb_available(),
        "https://github.com/unlimitedbacon/stl-thumb/releases",
    );

    // Debug: show exactly what stl-thumb returns
    for flag in ["-V", "--version", "--help", "-h", ""] {
        let result = if flag.is_empty() {
            std::process::Command::new("stl-thumb").output()
        } else {
            std::process::Command::new("stl-thumb").arg(flag).output()
        };
        match result {
            Ok(out) => {
                println!("    {} stl-thumb {} → exit={} stdout={:?}",
                    "·".dimmed(),
                    if flag.is_empty() { "(no args)" } else { flag },
                    out.status.code().unwrap_or(-1),
                    String::from_utf8_lossy(&out.stdout).trim(),
                );
                break; // if it works, this is enough
            }
            Err(e) => {
                println!("    {} stl-thumb {} → error: {e}",
                    "·".dimmed(),
                    if flag.is_empty() { "(no args)" } else { flag });
            }
        }
    }

    println!("\n  {} ZIP, 7Z, audio, image: pure Rust — no dependencies", "✓".green());
    Ok(())
}

fn cmd_thumbs(
    db:      Database,
    db_path: &str,
    tipo:    Option<MediaTypeArg>,
    size:    u32,
    quality: u8,
    force:   bool,
    verbose: bool,
) -> Result<()> {
    use thumbs::{thumb_dir_for_db, thumb_path, generate_image, generate_image_from_bytes,
                 generate_video, generate_video_from_archive, generate_3d};

    let thumb_dir   = thumb_dir_for_db(db_path);
    let type_filter = tipo.as_ref().map(|t| t.as_db_str());
    let files       = db.files_for_thumbs(type_filter)?;

    if files.is_empty() {
        println!("{}", "No candidate files for thumbnails.".dimmed());
        return Ok(());
    }

    println!("{} Generating thumbnails in {}",
        "🖼", thumb_dir.display().to_string().bold());
    println!("  {} files  {}px  quality {}\n",
        files.len().to_string().cyan(), size, quality);

    let pb = ProgressBar::new(files.len() as u64);
    pb.set_style(ProgressStyle::with_template(
        "{spinner:.cyan} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} {msg}"
    )?.progress_chars("█▓░"));

    let (mut ok, mut skipped, mut errors) = (0usize, 0usize, 0usize);

    for (hash, path, media_type, ext) in &files {
        let media_type: &str = media_type.as_str();
        let hash:       &str = hash.as_str();
        let path:       &str = path.as_str();
        let ext:        &str = ext.as_str();
        pb.set_message(
            std::path::Path::new(path)
                .file_name()
                .map(|n| n.to_string_lossy().chars().take(45).collect::<String>())
                .unwrap_or_default()
        );

        // Skip if already exists and --force was not requested
        let t_path = thumb_path(&thumb_dir, hash);
        if t_path.exists() && !force {
            skipped += 1;
            pb.inc(1);
            continue;
        }

        let result = if path.contains("::") {
            // ── File inside archive ───────────────────────────────────────
            let mut parts = path.splitn(2, "::");
            let archive_path = parts.next().unwrap_or("");
            let inner_name   = parts.next().unwrap_or("");

            // Extract the file's bytes from the archive
            let bytes_result = extract_entry_bytes(archive_path, inner_name);

            match bytes_result {
                Err(e) => Err(e),
                Ok(data) => match media_type {
                    "image" => generate_image_from_bytes(&data, hash, &thumb_dir, size, quality),
                    "video" => generate_video_from_archive(&data, ext, hash, &thumb_dir, size, quality),
                    "3d"    => generate_3d(&data, ext, hash, &thumb_dir, size, quality),
                    _       => { pb.inc(1); continue; }
                }
            }
        } else {
            // ── Loose file on disk ────────────────────────────────────────
            match media_type {
                "image" => generate_image(path, hash, &thumb_dir, size, quality),
                "video" => generate_video(path, hash, &thumb_dir, size, quality),
                "3d"    => {
                    match std::fs::read(path) {
                        Ok(data) => generate_3d(&data, ext, hash, &thumb_dir, size, quality),
                        Err(e)   => Err(anyhow::anyhow!(e)),
                    }
                }
                _ => { pb.inc(1); continue; }
            }
        };

        match result {
            Ok(_)  => ok += 1,
            Err(e) => {
                errors += 1;
                pb.println(format!("  {} {}: {}",
                    "✗".red(),
                    std::path::Path::new(path).file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default(),
                    if verbose { e.to_string() } else { e.to_string().chars().take(80).collect() }
                ));
            }
        }

        pb.inc(1);
    }

    pb.finish_with_message("done");

    println!("\n{}", "─── Result ───────────────────────────────────".dimmed());
    println!("  {} generated", ok.to_string().green().bold());
    if skipped > 0 {
        println!("  {} skipped (already existed — use {} to regenerate)",
            skipped.to_string().dimmed(), "--force".bold());
    }
    if errors > 0 {
        println!("  {} errors", errors.to_string().red());
    }
    println!("  → {}", thumb_dir.display().to_string().dimmed());

    Ok(())
}

/// Extracts the bytes of a specific file from inside an archive.
/// Supports .zip, .7z and .rar (same as archive.rs).
fn extract_entry_bytes(archive_path: &str, inner_name: &str) -> anyhow::Result<Vec<u8>> {
    use crate::models::ArchiveType;
    use std::path::Path;

    let arc_path = Path::new(archive_path);
    let arc_type = ArchiveType::from_path(arc_path)
        .ok_or_else(|| anyhow::anyhow!("Unsupported archive format: {archive_path}"))?;

    match arc_type {
        ArchiveType::Zip => {
            let file    = std::fs::File::open(arc_path)?;
            let mut zip = zip::ZipArchive::new(file)?;

            // First look for exact name; otherwise search by base filename
            let idx = if zip.by_name(inner_name).is_ok() {
                // by_name with is_ok does not keep the borrow — find the real index
                (0..zip.len()).find(|&i| {
                    zip.by_index(i).ok()
                        .map(|e| e.name() == inner_name)
                        .unwrap_or(false)
                })
            } else {
                let base = std::path::Path::new(inner_name)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                (0..zip.len()).find(|&i| {
                    zip.by_index(i).ok()
                        .map(|e| e.name().ends_with(&*base))
                        .unwrap_or(false)
                })
            }.ok_or_else(|| anyhow::anyhow!("{inner_name} not found in {archive_path}"))?;

            let mut entry = zip.by_index(idx)?;
            let mut data  = Vec::with_capacity(entry.size() as usize);
            std::io::copy(&mut entry, &mut data)?;
            Ok(data)
        }
        ArchiveType::SevenZip => {
            use sevenz_rust::SevenZReader;
            let mut archive = SevenZReader::open(arc_path, sevenz_rust::Password::empty())?;
            let mut found   = None;
            archive.for_each_entries(|entry, reader| {
                if entry.name() == inner_name
                    || entry.name().ends_with(&format!("/{inner_name}"))
                    || entry.name().ends_with(&format!("\\{inner_name}"))
                {
                    let mut data = Vec::new();
                    let _ = std::io::copy(reader, &mut data);
                    found = Some(data);
                    return Ok(false); // stop iteration
                }
                Ok(true)
            })?;
            found.ok_or_else(|| anyhow::anyhow!("{inner_name} not found in {archive_path}"))
        }
        ArchiveType::Rar => {
            // For RAR we extract everything to temp and read the target file
            use std::process::Command;
            let path_hash = {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                archive_path.hash(&mut h);
                h.finish()
            };
            let tmp = std::env::temp_dir()
                .join(format!("media_idx_rar_{}_{:016x}",
                    std::path::Path::new(archive_path)
                        .file_stem().map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "tmp".into()),
                    path_hash,
                ));
            std::fs::create_dir_all(&tmp)?;
            Command::new("unrar")
                .args(["x", "-y", "-inul", archive_path])
                .arg(&tmp)
                .status()?;
            // Find the file in the temp directory
            let target = walkdir::WalkDir::new(&tmp)
                .into_iter()
                .flatten()
                .find(|e| e.file_type().is_file() && {
                    let name = e.file_name().to_string_lossy();
                    let inner_base = Path::new(inner_name)
                        .file_name().map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    name == inner_base.as_str()
                })
                .map(|e| e.path().to_path_buf());
            let result = match &target {
                Some(p) => std::fs::read(p).map_err(|e| anyhow::anyhow!(e)),
                None    => Err(anyhow::anyhow!("{inner_name} not found in {archive_path}")),
            };
            let _ = std::fs::remove_dir_all(&tmp);
            result
        }
    }
}

fn cmd_clean(db: Database, macos_junk: bool) -> Result<()> {
    if !macos_junk {
        println!("{}", "Specify what to clean. Available options:".yellow());
        println!("  {} Remove macOS junk entries (__MACOSX/, ._, .DS_Store)",
            "--macos-junk".bold());
        return Ok(());
    }

    let removed = db.purge_macos_junk()?;
    if removed == 0 {
        println!("{}", "No macOS junk entries found in the DB 🎉".green());
    } else {
        println!("  {} {} macOS junk entry/ies removed from the DB",
            "✓".green(),
            removed.to_string().green().bold());
        println!("  {} Files on disk were {} touched.",
            " ".normal(), "not".bold());
    }
    Ok(())
}

fn cmd_verify(db: Database, prune: bool, quiet: bool) -> Result<()> {
    use humansize::{format_size, DECIMAL};

    let files = db.files_for_verify()?;

    if files.is_empty() {
        println!("{}", "No indexed files to verify.".dimmed());
        return Ok(());
    }

    println!("{} Verifying {} file(s){}\n",
        "🔍".cyan(),
        files.len().to_string().bold(),
        if prune { " (--prune active: invalid entries will be removed)" } else { "" },
    );

    let pb = indicatif::ProgressBar::new(files.len() as u64);
    pb.set_style(indicatif::ProgressStyle::with_template(
        "{spinner:.cyan} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} {msg}"
    )?.progress_chars("█▓░"));

    let (mut ok, mut missing, mut modified, mut pruned) = (0usize, 0usize, 0usize, 0usize);

    // Partial hash threshold — must match scanner.rs
    const PARTIAL_HASH_THRESHOLD: u64 = 100 * 1024 * 1024;
    const PARTIAL_CHUNK_SIZE:     u64 = 4   * 1024 * 1024;

    for (id, stored_hash, path, stored_size) in &files {
        let p = std::path::Path::new(path);

        pb.set_message(
            p.file_name()
                .map(|n| n.to_string_lossy().chars().take(45).collect::<String>())
                .unwrap_or_default()
        );

        // ── 1. Exists on disk? ────────────────────────────────────────────
        let meta = match p.metadata() {
            Ok(m)  => m,
            Err(_) => {
                missing += 1;
                pb.println(format!("  {} {} {}",
                    "✗".red(), "MISSING:".red().bold(), path.dimmed()));
                if prune {
                    let _ = db.remove_file(*id);
                    pruned += 1;
                }
                pb.inc(1);
                continue;
            }
        };

        let current_size = meta.len();

        // ── 2. Size changed? (fast, no re-hashing) ───────────────────────
        if current_size != *stored_size {
            modified += 1;
            pb.println(format!("  {} {} {} (was {}, now {})",
                "!".yellow().bold(),
                "MODIFIED:".yellow().bold(),
                path.dimmed(),
                format_size(*stored_size, DECIMAL).dimmed(),
                format_size(current_size, DECIMAL).yellow()));
            if prune {
                let _ = db.remove_file(*id);
                pruned += 1;
            }
            pb.inc(1);
            continue;
        }

        // ── 3. Re-hash and compare ────────────────────────────────────────
        let current_hash = if current_size <= PARTIAL_HASH_THRESHOLD {
            match std::fs::read(p) {
                Ok(data) => blake3::hash(&data).to_hex().to_string(),
                Err(e) => {
                    pb.println(format!("  {} {} {}: {e}",
                        "✗".red(), "ERROR:".red().bold(), path.dimmed()));
                    missing += 1;
                    pb.inc(1);
                    continue;
                }
            }
        } else {
            // Partial hash — same logic as scanner::hash_file
            use std::io::Read;
            let chunk = PARTIAL_CHUNK_SIZE as usize;
            let mut hasher = blake3::Hasher::new();
            match std::fs::File::open(p) {
                Err(e) => {
                    pb.println(format!("  {} {} {}: {e}",
                        "✗".red(), "ERROR:".red().bold(), path.dimmed()));
                    missing += 1;
                    pb.inc(1);
                    continue;
                }
                Ok(mut file) => {
                    let mut head = vec![0u8; chunk];
                    let n = file.read(&mut head).unwrap_or(0);
                    hasher.update(&head[..n]);
                    if current_size > 2 * PARTIAL_CHUNK_SIZE {
                        use std::io::Seek;
                        let _ = file.seek(std::io::SeekFrom::End(-(chunk as i64)));
                        let mut tail = vec![0u8; chunk];
                        let n = file.read(&mut tail).unwrap_or(0);
                        hasher.update(&tail[..n]);
                    }
                    hasher.update(&current_size.to_le_bytes());
                    hasher.finalize().to_hex().to_string()
                }
            }
        };

        if &current_hash != stored_hash {
            modified += 1;
            pb.println(format!("  {} {} {}",
                "!".yellow().bold(),
                "HASH CHANGED:".yellow().bold(),
                path.dimmed()));
            if prune {
                let _ = db.remove_file(*id);
                pruned += 1;
            }
        } else {
            ok += 1;
            if !quiet {
                pb.println(format!("  {} {}", "✓".green(), path.dimmed()));
            }
        }

        pb.inc(1);
    }

    pb.finish_with_message("done");

    println!("\n{}", "─── Result ───────────────────────────────────".dimmed());
    println!("  {} OK",            ok.to_string().green().bold());
    if missing > 0 {
        println!("  {} missing",  missing.to_string().red().bold());
    }
    if modified > 0 {
        println!("  {} modified / hash changed", modified.to_string().yellow().bold());
    }
    if prune && pruned > 0 {
        println!("  {} entry/ies removed from the DB", pruned.to_string().cyan().bold());
    } else if (missing > 0 || modified > 0) && !prune {
        println!("  {} Use {} to remove these entries from the DB.",
            "→".dimmed(), "--prune".bold());
    }

    Ok(())
}

// ── Similar images / similar audio ───────────────────────────────────────

fn cmd_similar(db: Database, kind: SimilarKind, threshold: u32, as_json: bool) -> Result<()> {
    match kind {
        SimilarKind::Images => {
            let groups = db.similar_images(threshold)?;
            if groups.is_empty() {
                println!("{}", "No similar images found 🎉".green());
                return Ok(());
            }
            if as_json {
                let json: Vec<_> = groups.iter().map(|g| {
                    serde_json::json!({
                        "files": g.files.iter().map(|f| serde_json::json!({
                            "path": f.path, "name": f.name,
                            "width": f.width, "height": f.height, "phash": f.phash,
                        })).collect::<Vec<_>>()
                    })
                }).collect();
                println!("{}", serde_json::to_string_pretty(&json)?);
                return Ok(());
            }
            println!("{} grupos de imágenes similares (umbral Hamming ≤{})\n",
                groups.len().to_string().yellow().bold(), threshold);
            for g in &groups {
                println!("{}", "──────────────────────────────────────".dimmed());
                for f in &g.files {
                    let dim = match (f.width, f.height) {
                        (Some(w), Some(h)) => format!(" {}×{}", w, h),
                        _ => String::new(),
                    };
                    println!("  {} {}{}", "🖼".normal(), f.name.bold(), dim.dimmed());
                    println!("    {} {}", f.phash.dimmed(), f.path.dimmed());
                }
                println!();
            }
        }
        SimilarKind::Audio => {
            let groups = db.similar_audio()?;
            if groups.is_empty() {
                println!("{}", "No similar audio found 🎉".green());
                return Ok(());
            }
            if as_json {
                let json: Vec<_> = groups.iter().map(|g| {
                    serde_json::json!({
                        "title": g.title, "artist": g.artist,
                        "files": g.files.iter().map(|f| serde_json::json!({
                            "path": f.path, "name": f.name,
                            "duration_secs": f.duration_secs, "album": f.album,
                        })).collect::<Vec<_>>()
                    })
                }).collect();
                println!("{}", serde_json::to_string_pretty(&json)?);
                return Ok(());
            }
            println!("{} grupos de audio similar\n",
                groups.len().to_string().yellow().bold());
            for g in &groups {
                println!("{} \"{}\" — {}",
                    "♪".magenta(), g.title.bold(), g.artist.cyan());
                for f in &g.files {
                    let dur = f.duration_secs
                        .map(|d| format!(" ({})", fmt_duration(d)))
                        .unwrap_or_default();
                    let album = f.album.as_deref()
                        .map(|a| format!(" [{a}]"))
                        .unwrap_or_default();
                    println!("  {} {}{}{}", "↳".dimmed(), f.name.bold(),
                        dur.dimmed(), album.dimmed());
                    println!("    {}", f.path.dimmed());
                }
                println!();
            }
        }
    }
    Ok(())
}

// ── Empty files and directories ───────────────────────────────────────────

fn cmd_empty(
    root:       &std::path::Path,
    dirs_only:  bool,
    files_only: bool,
    delete:     bool,
    dry_run:    bool,
) -> Result<()> {
    use walkdir::WalkDir;

    if !root.exists() {
        anyhow::bail!("Directory does not exist: {}", root.display());
    }

    if dry_run {
        println!("{}", "  [DRY-RUN] No files will be modified.\n".yellow().bold());
    }

    let mut found_files = 0usize;
    let mut found_dirs  = 0usize;
    let mut deleted     = 0usize;
    let mut errors      = 0usize;

    // Collect dirs bottom-up so we can detect newly-empty parents
    let entries: Vec<_> = WalkDir::new(root)
        .follow_links(false)
        .contents_first(true) // bottom-up
        .into_iter()
        .filter_map(|e| e.ok())
        .collect();

    for entry in &entries {
        let path = entry.path();
        if path == root { continue; }

        let is_dir  = entry.file_type().is_dir();
        let is_file = entry.file_type().is_file();

        let is_empty = if is_file {
            entry.metadata().map(|m| m.len() == 0).unwrap_or(false)
        } else if is_dir {
            std::fs::read_dir(path).map(|mut d| d.next().is_none()).unwrap_or(false)
        } else {
            false
        };

        if !is_empty { continue; }

        if (is_file && dirs_only) || (is_dir && files_only) { continue; }

        if is_file { found_files += 1; } else { found_dirs += 1; }

        let label = if is_dir { "DIR ".cyan() } else { "FILE".yellow() };
        if dry_run || !delete {
            println!("  {} {}", label, path.display().to_string().dimmed());
        } else {
            let result = if is_dir {
                std::fs::remove_dir(path).map_err(anyhow::Error::from)
            } else {
                std::fs::remove_file(path).map_err(anyhow::Error::from)
            };
            match result {
                Ok(_) => {
                    deleted += 1;
                    println!("  {} {} {}", "✓".green(), label, path.display().to_string().dimmed());
                }
                Err(e) => {
                    errors += 1;
                    eprintln!("  {} {}: {e}", "✗".red(), path.display());
                }
            }
        }
    }

    println!("\n{}", "─── Resultado ─────────────────────────────────".dimmed());
    println!("  {} archivo(s) vacío(s)", found_files.to_string().yellow().bold());
    println!("  {} carpeta(s) vacía(s)", found_dirs.to_string().cyan().bold());
    if delete && !dry_run {
        println!("  {} eliminado(s)", deleted.to_string().green().bold());
        if errors > 0 { println!("  {} error(es)", errors.to_string().red()); }
    } else if dry_run && (found_files + found_dirs) > 0 {
        println!("  {} Ejecuta sin {} para eliminar.", "→".dimmed(), "--dry-run".bold());
    }

    Ok(())
}

// ── Feature #5: symlinks rotos ───────────────────────────────────────────

fn cmd_broken(
    root:    &std::path::Path,
    delete:  bool,
    dry_run: bool,
) -> Result<()> {
    use walkdir::WalkDir;

    if !root.exists() {
        anyhow::bail!("Directory does not exist: {}", root.display());
    }

    if dry_run {
        println!("{}", "  [DRY-RUN] No files will be modified.\n".yellow().bold());
    }

    let mut found   = 0usize;
    let mut deleted = 0usize;
    let mut errors  = 0usize;

    for entry in WalkDir::new(root).follow_links(false).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if !entry.path_is_symlink() { continue; }

        // A symlink is broken if its target does not exist
        let target = std::fs::read_link(path).unwrap_or_default();
        let broken  = !path.exists(); // exists() follows the link; false = broken

        if !broken { continue; }

        found += 1;
        let target_str = target.display().to_string();

        if dry_run || !delete {
            println!("  {} {} {} {}",
                "⚠".yellow(),
                path.display().to_string().bold(),
                "→".dimmed(),
                target_str.red());
        } else {
            match std::fs::remove_file(path) {
                Ok(_) => {
                    deleted += 1;
                    println!("  {} {} (target: {})",
                        "✓".green(),
                        path.display().to_string().dimmed(),
                        target_str.red().dimmed());
                }
                Err(e) => {
                    errors += 1;
                    eprintln!("  {} {}: {e}", "✗".red(), path.display());
                }
            }
        }
    }

    println!("\n{}", "─── Resultado ─────────────────────────────────".dimmed());
    if found == 0 {
        println!("  {}", "No broken symlinks found 🎉".green());
    } else {
        println!("  {} symlink(s) roto(s) encontrado(s)", found.to_string().yellow().bold());
        if delete && !dry_run {
            println!("  {} eliminado(s)", deleted.to_string().green().bold());
            if errors > 0 { println!("  {} error(es)", errors.to_string().red()); }
        } else if dry_run {
            println!("  {} Ejecuta sin {} para eliminar.", "→".dimmed(), "--dry-run".bold());
        }
    }

    Ok(())
}

fn cmd_clear(db_path: &str, force: bool) -> Result<()> {
    if !force {
        println!(
            "{} This will permanently delete {} and cannot be undone.",
            "⚠".yellow().bold(),
            db_path.bold(),
        );
        print!("  Continue? [y/N] ");
        std::io::Write::flush(&mut std::io::stdout())?;

        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;

        if !matches!(input.trim().to_lowercase().as_str(), "y" | "yes") {
            println!("{}", "Cancelled.".dimmed());
            return Ok(());
        }
    }

    // Delete the .db file and WAL / SHM if they exist
    for suffix in ["", "-wal", "-shm"] {
        let path = format!("{db_path}{suffix}");
        if std::path::Path::new(&path).exists() {
            std::fs::remove_file(&path)?;
        }
    }

    println!("{} Database deleted: {}", "✓".green(), db_path.bold());
    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────

fn fmt_duration(secs: f64) -> String {
    let total = secs as u64;
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}