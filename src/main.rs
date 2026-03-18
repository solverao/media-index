mod archive;
mod db;
mod models;
mod parsers;
mod scanner;

use std::path::PathBuf;
use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use colored::Colorize;
use humansize::{format_size, DECIMAL};

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

    /// Estadísticas generales de la colección
    Stats,

    /// Listar duplicados (por tipo o todos)
    Dupes {
        #[arg(short, long)]
        tipo: Option<MediaTypeArg>,
        #[arg(short, long)]
        json: bool,
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

    /// Borrar toda la base de datos (pide confirmación)
    Clear {
        /// No pedir confirmación (útil en scripts)
        #[arg(short, long)]
        force: bool,
    },
}

#[derive(Clone, ValueEnum)]
enum MediaTypeArg { Td, Video, Audio, Imagen }

impl MediaTypeArg {
    fn as_db_str(&self) -> &'static str {
        match self {
            Self::Td     => "3d",
            Self::Video  => "video",
            Self::Audio  => "audio",
            Self::Imagen => "image",
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
        Commands::Scan { path, verbose } => cmd_scan(db, &path, verbose),
        Commands::Stats                   => cmd_stats(db),
        Commands::Dupes { tipo, json }   => cmd_dupes(db, tipo, json),
        Commands::Search { query, tipo } => cmd_search(db, &query, tipo),
        Commands::Export { output }      => cmd_export(db, &output),
        Commands::Doctor                  => cmd_doctor(),
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
    println!("  {} comprimidos",    s.archives_opened.to_string().dimmed());
    println!("  {} duplicados ({})", s.duplicates.to_string().red().bold(),
        format_size(s.bytes_dup, DECIMAL).red());
    if s.errors > 0 {
        println!("  {} errores", s.errors.to_string().red());
    }
    println!("  {}", "──────────────────────────────────────".dimmed());
    println!("  {} indexados en total", s.total_indexed().to_string().cyan().bold());
 
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

    let icons = [("3d", "⬡"), ("video", "▶"), ("audio", "♪"), ("image", "🖼")];
    for (type_str, count, bytes) in &s.by_type {
        let icon = icons.iter().find(|(k, _)| k == type_str).map(|(_, v)| *v).unwrap_or("·");
        println!("  {:>8}  {:>12}  {} {}",
            count.to_string().cyan(),
            format_size(*bytes as u64, DECIMAL).dimmed(),
            icon,
            type_str.to_uppercase().bold(),
        );
    }

    Ok(())
}

fn cmd_dupes(db: Database, tipo: Option<MediaTypeArg>, as_json: bool) -> Result<()> {
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

    for g in &groups {
        let type_badge = match g.media_type.as_str() {
            "3d"    => "⬡ 3D".cyan(),
            "video" => "▶ VID".blue(),
            "audio" => "♪ AUD".magenta(),
            "image" => "🖼 IMG".yellow(),
            _       => "? ???".normal(),
        };
        println!("{} {} {} ({})",
            "●".red(), type_badge, g.canonical_name.bold(),
            format_size(g.size_bytes, DECIMAL).dimmed());
        println!("  {}", &g.hash[..16].dimmed());
        println!("  {}", g.canonical_path.green());
        for d in &g.duplicates {
            println!("  {} {}", "↳".red(), d.yellow());
        }
        println!();
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
            _       => "[?]".normal(),
        };

        println!("{} {} {}",
            "▸".cyan(), badge, r.name.bold());
        println!("  {}", r.path.dimmed());

        let info: Vec<String> = match &r.detail {
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

    println!("\n  {} ZIP, 7Z, audio, imagen: pure Rust — sin dependencias", "✓".green());
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
