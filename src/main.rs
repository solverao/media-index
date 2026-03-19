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
    about   = "Indexador de archivos 3D, video, audio e imagen con deduplicación",
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
    /// Escanear un directorio e indexar todos los archivos de media
    Scan {
        path: PathBuf,
        #[arg(short, long)]
        verbose: bool,
    },

    /// Vigilar un directorio e indexar cambios en tiempo real
    Watch {
        path: PathBuf,
        #[arg(short, long)]
        verbose: bool,
        /// Segundos de espera antes de procesar un evento (default: 2)
        #[arg(short, long, default_value = "2")]
        debounce: u64,
    },

    /// Estadísticas generales de la colección
    Stats,

    /// Listar duplicados (por tipo o todos)
    Dupes {
        #[arg(short, long)]
        tipo: Option<MediaTypeArg>,
        #[arg(short, long)]
        json: bool,
        /// Borrar duplicados en disco. Los que están dentro de comprimidos se reportan sin tocar.
        #[arg(short, long)]
        delete: bool,
        /// Con --delete: mostrar qué se borraría sin borrar nada realmente.
        #[arg(long)]
        dry_run: bool,
        /// Con --delete: si TODOS los archivos de un comprimido son duplicados, borra el comprimido entero.
        #[arg(short, long)]
        aggressive: bool,
        /// Con --delete: si el canónico es un archivo suelto y ya existe dentro de un comprimido,
        /// borrar el archivo suelto (el contenido sigue en el comprimido).
        #[arg(short = 'p', long)]
        prefer_archive: bool,
    },

    /// Buscar archivos por nombre
    Search {
        query: String,
        #[arg(short, long)]
        tipo: Option<MediaTypeArg>,
    },

    /// Exportar índice a JSON
    Export {
        #[arg(short, long, default_value = "media_export.json")]
        output: PathBuf,
    },

    /// Verificar dependencias opcionales
    Doctor,

    /// Re-hashear archivos indexados y detectar modificados, corrompidos o faltantes
    Verify {
        /// Eliminar de la BD las entradas con archivos ya no existentes o corrompidos
        #[arg(short, long)]
        prune: bool,
        /// Mostrar solo los archivos con problemas (omitir los OK)
        #[arg(short, long)]
        quiet: bool,
    },

    /// Generar thumbnails de imágenes, videos y modelos 3D
    Thumbs {
        /// Filtrar por tipo (por defecto genera los tres tipos)
        #[arg(short, long)]
        tipo: Option<MediaTypeArg>,
        /// Tamaño en píxeles del lado del cuadrado
        #[arg(short, long, default_value = "256")]
        size: u32,
        /// Calidad JPEG (1-100)
        #[arg(short, long, default_value = "85")]
        quality: u8,
        /// Regenerar thumbnails que ya existen
        #[arg(short, long)]
        force: bool,
        /// Mostrar errores detallados por cada archivo
        #[arg(short, long)]
        verbose: bool,
    },

    /// Limpiar entradas no deseadas de la BD sin borrar archivos del disco
    Clean {
        /// Eliminar entradas de basura macOS (__MACOSX/, ._, .DS_Store) indexadas por error
        #[arg(long)]
        macos_junk: bool,
    },

    /// Borrar toda la base de datos (pide confirmación)
    Clear {
        /// No pedir confirmación (útil en scripts)
        #[arg(short, long)]
        force: bool,
    },
}

#[derive(Clone, ValueEnum)]
enum MediaTypeArg { Td, Video, Audio, Imagen, Otro }

impl MediaTypeArg {
    fn as_db_str(&self) -> &'static str {
        match self {
            Self::Td     => "3d",
            Self::Video  => "video",
            Self::Audio  => "audio",
            Self::Imagen => "image",
            Self::Otro   => "other",
        }
    }
}

// ── Main ──────────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Clear no necesita abrir (ni crear) la BD
    if let Commands::Clear { force } = cli.command {
        return cmd_clear(&cli.db, force);
    }

    let db = Database::open(&cli.db)?;

    match cli.command {
        Commands::Scan { path, verbose }  => cmd_scan(db, &path, verbose),
        Commands::Watch { path, verbose, debounce } => cmd_watch(db, &path, verbose, debounce),
        Commands::Stats                   => cmd_stats(db),
        Commands::Dupes { tipo, json, delete, dry_run, aggressive, prefer_archive } => cmd_dupes(db, tipo, json, delete, dry_run, aggressive, prefer_archive),
        Commands::Search { query, tipo } => cmd_search(db, &query, tipo),
        Commands::Export { output }      => cmd_export(db, &output),
        Commands::Doctor                  => cmd_doctor(),
        Commands::Verify { prune, quiet }  => cmd_verify(db, prune, quiet),
        Commands::Clean { macos_junk }      => cmd_clean(db, macos_junk),
        Commands::Thumbs { tipo, size, quality, force, verbose } => cmd_thumbs(db, &cli.db, tipo, size, quality, force, verbose),
        Commands::Clear { .. }           => unreachable!(),
    }
}

// ── Comandos ──────────────────────────────────────────────────────────────

fn cmd_scan(db: Database, path: &std::path::Path, verbose: bool) -> Result<()> {
    if !path.exists() {
        anyhow::bail!("El directorio no existe: {}", path.display());
    }

    if !ffprobe_available() {
        println!("{} ffprobe no encontrado — metadatos de video no disponibles",
            "⚠".yellow());
        println!("  Instalar: sudo apt install ffmpeg  /  brew install ffmpeg\n");
    }

    println!("{}", format!("Escaneando: {}", path.display()).bold().cyan());

    let scanner = Scanner::new(db, verbose);
    let s       = scanner.scan(path)?;

    println!("\n{}", "─── Resultado ────────────────────────────────".dimmed());
    println!("  {} archivos 3D",    s.indexed_3d.to_string().green().bold());
    println!("  {} videos",         s.indexed_video.to_string().blue().bold());
    println!("  {} audios",         s.indexed_audio.to_string().magenta().bold());
    println!("  {} imágenes",       s.indexed_image.to_string().yellow().bold());
    println!("  {} otros",          s.indexed_other.to_string().white().bold());
    println!("  {} comprimidos",    s.archives_opened.to_string().dimmed());
    println!("  {} duplicados ({})", s.duplicates.to_string().red().bold(),
        format_size(s.bytes_dup, DECIMAL).red());
    if s.errors > 0 {
        println!("  {} errores", s.errors.to_string().red());
    }
    println!("  {}", "──────────────────────────────────────".dimmed());
    println!("  {} indexados en total", s.total_indexed().to_string().cyan().bold());

    if s.partial_hashes > 0 {
        println!("\n  {} {} archivo(s) grandes hasheados parcialmente (>{}) — deduplicación aproximada",
            "⚠".yellow(),
            s.partial_hashes.to_string().yellow(),
            "100 MB".bold());
        println!("  {} Usa {} para detectar falsos positivos.",
            " ".normal(),
            "verify".bold());
    }
 
    Ok(())
}

fn cmd_watch(db: Database, path: &std::path::Path, verbose: bool, debounce_secs: u64) -> Result<()> {
    use notify_debouncer_mini::{new_debouncer, notify::RecursiveMode, DebouncedEventKind};
    use std::time::Duration;

    if !path.exists() {
        anyhow::bail!("El directorio no existe: {}", path.display());
    }

    if !ffprobe_available() {
        println!("{} ffprobe no encontrado — metadatos de video no disponibles", "⚠".yellow());
        println!("  Instalar: sudo apt install ffmpeg  /  brew install ffmpeg\n");
    }

    // Escaneo inicial completo
    println!("{}", format!("Escaneo inicial: {}", path.display()).bold().cyan());
    let scanner = Scanner::new(db, verbose);
    let s = scanner.scan(path)?;
    println!("  {} indexados  {} duplicados\n",
        s.total_indexed().to_string().cyan().bold(),
        s.duplicates.to_string().red());

    let (tx, rx) = std::sync::mpsc::channel();

    let mut debouncer = new_debouncer(
        Duration::from_secs(debounce_secs),
        move |res| { let _ = tx.send(res); },
    )?;

    debouncer.watcher().watch(path, RecursiveMode::Recursive)?;

    println!("{} Vigilando {}  {}",
        "👁",
        path.display().to_string().bold(),
        format!("(debounce {}s — Ctrl+C para salir)", debounce_secs).dimmed());

    for events in rx {
        let events = match events {
            Ok(e)  => e,
            Err(e) => { eprintln!("{} Error de watcher: {e:?}", "✗".red()); continue; }
        };

        // Deduplicar paths en este batch
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

        // Borrados → cleanup en BD
        if had_remove {
            match scanner.cleanup() {
                Ok((f, d)) if f > 0 || d > 0 =>
                    println!("  {} {} entrada(s) eliminadas de la BD", "🧹", f + d),
                _ => {}
            }
        }

        // Nuevos / modificados → indexar
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

    println!("{}", "─── Colección ────────────────────────────────".bold().cyan());
    println!("  Total único    : {}", s.total.to_string().green().bold());
    println!("  Duplicados     : {}", s.dupes.to_string().red());
    println!("  Tamaño total   : {}", format_size(s.bytes as u64, DECIMAL).yellow());
    println!("  Lib. por dedup : {}", format_size(s.bytes_dup as u64, DECIMAL).red());

    println!("\n  {:>8}  {:>12}  {}", "Archivos", "Tamaño", "Tipo");
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
) -> Result<()> {
    let mut groups = db.duplicates()?;

    if let Some(t) = &tipo {
        let filter = t.as_db_str();
        groups.retain(|g| g.media_type == filter);
    }

    if groups.is_empty() {
        println!("{}", "No hay duplicados 🎉".green());
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

    println!("{} grupos duplicados  —  {} liberables\n",
        groups.len().to_string().red().bold(),
        format_size(total_bytes, DECIMAL).red());

    if delete {
        return cmd_dupes_delete(&db, &groups, dry_run, aggressive, prefer_archive);
    }

    // ── Solo listar ────────────────────────────────────────────────────────
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
) -> Result<()> {
    if dry_run {
        println!("{}", "  [DRY-RUN] Ningún archivo será modificado.\n".yellow().bold());
    }

    // Separar cada duplicate_path en: archivo suelto vs. dentro de comprimido
    struct DeletePlan {
        path:         String,         // duplicate_path completo
        archive_path: Option<String>, // Some("/a/b.zip") si es "b.zip::foo.jpg"
    }

    let plans: Vec<DeletePlan> = groups.iter()
        .flat_map(|g| g.duplicates.iter())
        .map(|d| {
            let archive_path = if d.contains("::") {
                d.splitn(2, "::").next().map(|s| s.to_string())
            } else {
                None
            };
            DeletePlan { path: d.clone(), archive_path }
        })
        .collect();

    // ── --prefer-archive: canónicos sueltos que ya viven en un comprimido ──
    let mut deleted_files     = 0usize;
    let mut freed_bytes       = 0u64;
    let mut errors_delete     = 0usize;
    // Paths de canónicos que borramos en este paso — necesario para que
    // --aggressive no asuma que el comprimido es la única copia cuando
    // el canónico acaba de borrarse en la misma ejecución.
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
                        "(canónico suelto — copia en comprimido)".dimmed());
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
                                        "(canónico suelto — copia en comprimido)".dimmed());
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
                            println!("  {} {} (ya no existía)", "·".dimmed(), g.canonical_path.dimmed());
                        }
                    }
                }
            }
        }
    }

    // ── Archivos sueltos ───────────────────────────────────────────────────
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
                    println!("  {} {} (ya no existía)", "·".dimmed(), plan.path.dimmed());
                }
            }
        }
    }

    // ── Archivos en comprimidos ────────────────────────────────────────────
    let in_archives: Vec<&DeletePlan> = plans.iter().filter(|p| p.archive_path.is_some()).collect();

    if !in_archives.is_empty() && !aggressive {
        println!("\n{} {} duplicado(s) dentro de comprimidos — no se tocaron:",
            "⊡".yellow().bold(),
            in_archives.len().to_string().yellow());
        for plan in &in_archives {
            println!("  {} {}", "·".yellow(), plan.path.yellow());
        }
        println!("  {} Usa {} para borrar el comprimido si todos sus archivos son duplicados.",
            "→".dimmed(),
            "--aggressive".bold());
    }

    // ── Modo agresivo: borrar comprimidos completos si todo su contenido tiene copia ──
    let mut deleted_archives = 0usize;

    if aggressive && !in_archives.is_empty() {
        // Agrupar duplicate_paths por comprimido
        let mut by_archive: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        for plan in &in_archives {
            if let Some(arc) = &plan.archive_path {
                *by_archive.entry(arc.clone()).or_insert(0) += 1;
            }
        }

        // Convergencia iterativa: marcar comprimidos como borrables solo si
        // todo su contenido tiene copia fuera del conjunto de borrables.
        // Repetir hasta que el conjunto no cambie (fixpoint).
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
            if to_delete.len() == prev_len { break; } // fixpoint alcanzado
        }

        // Ejecutar (o simular) borrados
        for (archive_path, dup_count) in &by_archive {
            let arc = std::path::Path::new(archive_path);

            if !arc.exists() {
                println!("  {} {} (ya no existía)", "·".dimmed(), archive_path.dimmed());
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
                        "comprimido completo:".dimmed(),
                        archive_path.dimmed());
                } else {
                    match std::fs::remove_file(arc) {
                        Ok(_) => {
                            freed_bytes      += bytes;
                            deleted_archives += 1;
                            println!("  {} {} {}",
                                "✓".green(),
                                "comprimido completo:".dimmed(),
                                archive_path.dimmed());
                        }
                        Err(e) => {
                            errors_delete += 1;
                            eprintln!("  {} {}: {e}", "✗".red(), archive_path);
                        }
                    }
                }
            } else {
                println!("  {} {} — tiene archivos únicos o es la única copia, no se borra ({} duplicado(s) dentro)",
                    "⊡".yellow(),
                    archive_path.yellow(),
                    dup_count);
            }
        }
    }

    // ── Resumen ────────────────────────────────────────────────────────────
    println!("\n{}", "─── Resultado ────────────────────────────────".dimmed());
    if dry_run {
        println!("  {} {}", "[DRY-RUN]".cyan().bold(),
            "no se borró nada — ejecuta sin --dry-run para aplicar".dimmed());
        if deleted_files > 0 {
            println!("  {} archivo(s) se borrarían", deleted_files.to_string().cyan().bold());
        }
        if deleted_archives > 0 {
            println!("  {} comprimido(s) se borrarían", deleted_archives.to_string().cyan().bold());
        }
        println!("  {} se liberarían", format_size(freed_bytes, DECIMAL).cyan().bold());
    } else {
        if deleted_files > 0 {
            println!("  {} archivo(s) borrado(s)", deleted_files.to_string().green().bold());
        }
        if deleted_archives > 0 {
            println!("  {} comprimido(s) borrado(s)", deleted_archives.to_string().green().bold());
        }
        println!("  {} liberados", format_size(freed_bytes, DECIMAL).red().bold());
        if errors_delete > 0 {
            println!("  {} error(es)", errors_delete.to_string().red());
        }
        // Sincronizar la BD con lo que acaba de borrarse del disco
        if deleted_files > 0 || deleted_archives > 0 {
            match db.cleanup_stale() {
                Ok((f, d)) if f > 0 || d > 0 =>
                    println!("  {} BD sincronizada ({} entrada(s) eliminadas)", "🧹".dimmed(), f + d),
                Ok(_)  => {}
                Err(e) => eprintln!("  {} Error al sincronizar BD: {e}", "✗".red()),
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
        println!("Sin resultados para \"{}\"", query);
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
                if let Some(t) = triangles { v.push(format!("{t} triángulos")); }
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
    println!("Exportado a {}", output.display().to_string().green());
    Ok(())
}

fn cmd_doctor() -> Result<()> {
    println!("{}\n", "─── Diagnóstico de dependencias ──────────────".bold().cyan());

    let check = |name: &str, available: bool, install: &str| {
        if available {
            println!("  {} {}", "✓".green(), name.bold());
        } else {
            println!("  {} {} — instalar: {}", "✗".red(), name.bold(), install.dimmed());
        }
    };

    check(
        "ffprobe (metadatos de video)",
        ffprobe_available(),
        "sudo apt install ffmpeg  /  brew install ffmpeg",
    );

    check(
        "unrar (archivos .rar)",
        std::process::Command::new("unrar").arg("--help").output().is_ok(),
        "sudo apt install unrar  /  brew install rar",
    );

    check(
        "stl-thumb (thumbnails 3D con OpenGL — mejor calidad)",
        thumbs::stl_thumb_available(),
        "https://github.com/unlimitedbacon/stl-thumb/releases",
    );

    // Debug: mostrar qué devuelve stl-thumb exactamente
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
                    if flag.is_empty() { "(sin args)" } else { flag },
                    out.status.code().unwrap_or(-1),
                    String::from_utf8_lossy(&out.stdout).trim(),
                );
                break; // si funciona, con esto basta
            }
            Err(e) => {
                println!("    {} stl-thumb {} → error: {e}",
                    "·".dimmed(),
                    if flag.is_empty() { "(sin args)" } else { flag });
            }
        }
    }

    println!("\n  {} ZIP, 7Z, audio, imagen: pure Rust — sin dependencias", "✓".green());
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
        println!("{}", "No hay archivos candidatos para thumbnails.".dimmed());
        return Ok(());
    }

    println!("{} Generando thumbnails en {}",
        "🖼", thumb_dir.display().to_string().bold());
    println!("  {} archivos  {}px  calidad {}\n",
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

        // Saltar si ya existe y no se pidió --force
        let t_path = thumb_path(&thumb_dir, hash);
        if t_path.exists() && !force {
            skipped += 1;
            pb.inc(1);
            continue;
        }

        let result = if path.contains("::") {
            // ── Archivo dentro de comprimido ──────────────────────────────
            let mut parts = path.splitn(2, "::");
            let archive_path = parts.next().unwrap_or("");
            let inner_name   = parts.next().unwrap_or("");

            // Extraer los bytes del archivo del comprimido
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
            // ── Archivo suelto en disco ───────────────────────────────────
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

    pb.finish_with_message("listo");

    println!("\n{}", "─── Resultado ────────────────────────────────".dimmed());
    println!("  {} generados",  ok.to_string().green().bold());
    if skipped > 0 {
        println!("  {} omitidos (ya existían — usa {} para regenerar)",
            skipped.to_string().dimmed(), "--force".bold());
    }
    if errors > 0 {
        println!("  {} errores", errors.to_string().red());
    }
    println!("  → {}", thumb_dir.display().to_string().dimmed());

    Ok(())
}

/// Extrae los bytes de un archivo específico dentro de un comprimido.
/// Soporta .zip, .7z y .rar (igual que archive.rs).
fn extract_entry_bytes(archive_path: &str, inner_name: &str) -> anyhow::Result<Vec<u8>> {
    use crate::models::ArchiveType;
    use std::path::Path;

    let arc_path = Path::new(archive_path);
    let arc_type = ArchiveType::from_path(arc_path)
        .ok_or_else(|| anyhow::anyhow!("Formato de comprimido no soportado: {archive_path}"))?;

    match arc_type {
        ArchiveType::Zip => {
            let file    = std::fs::File::open(arc_path)?;
            let mut zip = zip::ZipArchive::new(file)?;

            // Primero buscar por nombre exacto; si no, buscar por nombre de archivo base
            let idx = if zip.by_name(inner_name).is_ok() {
                // by_name con is_ok no retiene el borrow — buscar el índice real
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
            }.ok_or_else(|| anyhow::anyhow!("{inner_name} no encontrado en {archive_path}"))?;

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
                    return Ok(false); // detener iteración
                }
                Ok(true)
            })?;
            found.ok_or_else(|| anyhow::anyhow!("{inner_name} no encontrado en {archive_path}"))
        }
        ArchiveType::Rar => {
            // Para RAR extraemos todo a temp y leemos el archivo buscado
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
            // Buscar el archivo en el directorio temporal
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
                None    => Err(anyhow::anyhow!("{inner_name} no encontrado en {archive_path}")),
            };
            let _ = std::fs::remove_dir_all(&tmp);
            result
        }
    }
}

fn cmd_clean(db: Database, macos_junk: bool) -> Result<()> {
    if !macos_junk {
        println!("{}", "Especifica qué limpiar. Opciones disponibles:".yellow());
        println!("  {} Eliminar basura macOS (__MACOSX/, ._, .DS_Store)",
            "--macos-junk".bold());
        return Ok(());
    }

    let removed = db.purge_macos_junk()?;
    if removed == 0 {
        println!("{}", "No se encontraron entradas de basura macOS en la BD 🎉".green());
    } else {
        println!("  {} {} entrada(s) de basura macOS eliminadas de la BD",
            "✓".green(),
            removed.to_string().green().bold());
        println!("  {} Los archivos en disco {} fueron tocados.",
            " ".normal(), "no".bold());
    }
    Ok(())
}

fn cmd_verify(db: Database, prune: bool, quiet: bool) -> Result<()> {
    use humansize::{format_size, DECIMAL};

    let files = db.files_for_verify()?;

    if files.is_empty() {
        println!("{}", "No hay archivos indexados para verificar.".dimmed());
        return Ok(());
    }

    println!("{} Verificando {} archivo(s){}\n",
        "🔍".cyan(),
        files.len().to_string().bold(),
        if prune { " (--prune activo: se eliminarán entradas inválidas)" } else { "" },
    );

    let pb = indicatif::ProgressBar::new(files.len() as u64);
    pb.set_style(indicatif::ProgressStyle::with_template(
        "{spinner:.cyan} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} {msg}"
    )?.progress_chars("█▓░"));

    let (mut ok, mut missing, mut modified, mut pruned) = (0usize, 0usize, 0usize, 0usize);

    // Umbral de hash parcial — debe coincidir con scanner.rs
    const PARTIAL_HASH_THRESHOLD: u64 = 100 * 1024 * 1024;
    const PARTIAL_CHUNK_SIZE:     u64 = 4   * 1024 * 1024;

    for (id, stored_hash, path, stored_size) in &files {
        let p = std::path::Path::new(path);

        pb.set_message(
            p.file_name()
                .map(|n| n.to_string_lossy().chars().take(45).collect::<String>())
                .unwrap_or_default()
        );

        // ── 1. ¿Existe en disco? ──────────────────────────────────────────
        let meta = match p.metadata() {
            Ok(m)  => m,
            Err(_) => {
                missing += 1;
                pb.println(format!("  {} {} {}",
                    "✗".red(), "FALTANTE:".red().bold(), path.dimmed()));
                if prune {
                    let _ = db.remove_file(*id);
                    pruned += 1;
                }
                pb.inc(1);
                continue;
            }
        };

        let current_size = meta.len();

        // ── 2. ¿Cambió de tamaño? (rápido, sin re-hashear) ───────────────
        if current_size != *stored_size {
            modified += 1;
            pb.println(format!("  {} {} {} (era {}, ahora {})",
                "!".yellow().bold(),
                "MODIFICADO:".yellow().bold(),
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

        // ── 3. Re-hashear y comparar ──────────────────────────────────────
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
            // Hash parcial — misma lógica que scanner::hash_file
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
                "HASH CAMBIADO:".yellow().bold(),
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

    pb.finish_with_message("listo");

    println!("\n{}", "─── Resultado ────────────────────────────────".dimmed());
    println!("  {} OK",            ok.to_string().green().bold());
    if missing > 0 {
        println!("  {} faltantes",  missing.to_string().red().bold());
    }
    if modified > 0 {
        println!("  {} modificados / hash cambiado", modified.to_string().yellow().bold());
    }
    if prune && pruned > 0 {
        println!("  {} entrada(s) eliminadas de la BD", pruned.to_string().cyan().bold());
    } else if (missing > 0 || modified > 0) && !prune {
        println!("  {} Usa {} para eliminar estas entradas de la BD.",
            "→".dimmed(), "--prune".bold());
    }

    Ok(())
}

fn cmd_clear(db_path: &str, force: bool) -> Result<()> {
    if !force {
        println!(
            "{} Esto borrará {} por completo y no se puede deshacer.",
            "⚠".yellow().bold(),
            db_path.bold(),
        );
        print!("  ¿Continuar? [s/N] ");
        std::io::Write::flush(&mut std::io::stdout())?;

        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;

        if !matches!(input.trim().to_lowercase().as_str(), "s" | "si" | "sí" | "y" | "yes") {
            println!("{}", "Cancelado.".dimmed());
            return Ok(());
        }
    }

    // Borrar el archivo .db y el WAL / SHM si existen
    for suffix in ["", "-wal", "-shm"] {
        let path = format!("{db_path}{suffix}");
        if std::path::Path::new(&path).exists() {
            std::fs::remove_file(&path)?;
        }
    }

    println!("{} Base de datos eliminada: {}", "✓".green(), db_path.bold());
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
