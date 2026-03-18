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

/// Para archivos > este límite, sólo hashemos cabeza + cola + tamaño.
/// Evita leer 50 GB de video completo en RAM. El hash sigue siendo
/// suficientemente único para deduplicación práctica.
const PARTIAL_HASH_THRESHOLD: u64 = 100 * 1024 * 1024; // 100 MB
const PARTIAL_CHUNK_SIZE:     u64 = 4  * 1024 * 1024;  //   4 MB por extremo

pub struct Scanner {
    db:      Arc<Mutex<Database>>,
    verbose: bool,
}

impl Scanner {
    pub fn new(db: Database, verbose: bool) -> Self {
        Self { db: Arc::new(Mutex::new(db)), verbose }
    }

    pub fn scan(&self, root: &Path) -> Result<ScanStats> {
        let stats = Arc::new(Mutex::new(ScanStats::default()));

        // Limpiar entradas obsoletas antes de escanear, para que los archivos
        // borrados manualmente no aparezcan como duplicados en el siguiente escaneo.
        {
            let db = self.db.lock().unwrap();
            match db.cleanup_stale() {
                Ok((files, dupes)) if files > 0 || dupes > 0 => {
                    println!(
                        "{}  Limpieza: {} canónico(s) y {} duplicado(s) eliminados de la BD (ya no existen en disco)",
                        "🧹", files, dupes
                    );
                }
                Ok(_) => {}
                Err(e) => eprintln!("Advertencia: Error en limpieza de BD: {e}"),
            }
        }

        println!("{}", "Recolectando archivos...".cyan());

        let entries: Vec<PathBuf> = WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .map(|e| e.path().to_path_buf())
            .collect();

        println!("{} {} archivos encontrados", "→".green(), entries.len());

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

            // ── Archivo de media directo ───────────────────────────────────
            if let Some(media_type) = MediaType::from_extension(&ext) {
                // Trabajo pesado SIN lock: leer, hashear, parsear
                match self.build_entry(path, &ext, &media_type) {
                    Ok(entry) => {
                        // Lock breve: solo insertar en BD + actualizar stats
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

            // ── Comprimido ─────────────────────────────────────────────────
            if let Some(archive_type) = ArchiveType::from_path(path) {
                // Filtrar partes extra de multi-part
                if archive_type == ArchiveType::Rar && is_rar_multipart(&name_lower) {
                    let is_first = name_lower.contains(".part1.") || name_lower.contains(".part01.");
                    if !is_first { pb.inc(1); return; }
                }
                if archive_type == ArchiveType::SevenZip && is_7z_multipart(&name_lower) {
                    if !name_lower.ends_with(".001") { pb.inc(1); return; }
                }

                // Trabajo pesado SIN lock: extraer + construir entries
                match extract_media_files(path, &archive_type) {
                    Ok(files) => {
                        let built: Vec<MediaEntry> = files.into_iter()
                            .filter_map(|extracted| {
                                let mt = MediaType::from_extension(&extracted.ext)?;
                                Some(build_entry_from_memory(
                                    &extracted.data, &extracted.name,
                                    &extracted.ext, &mt,
                                    path.to_string_lossy().as_ref(),
                                ))
                            })
                            .collect();

                        // Lock breve: insertar todo el batch
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

            pb.inc(1);
        });

        pb.finish_with_message("listo");
        Ok(Arc::try_unwrap(stats).unwrap().into_inner().unwrap())
    }

    // ── Construir entry sin tocar BD ni stats (trabajo pesado) ────────────

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

    // ── Insertar en BD + actualizar stats (trabajo rápido, bajo lock) ─────

    fn insert_entry(&self, entry: MediaEntry, stats: &mut ScanStats) {
        let media_type = entry.media_type.clone();
        let size = entry.size_bytes;
        let path = entry.current_path.clone();

        match self.db.lock().unwrap().insert(&entry) {
            Ok((_, true, Some(orig))) => {
                stats.duplicates += 1;
                stats.bytes_dup  += size;
                if self.verbose {
                    eprintln!("  {} dupl: {} ← {}", "≡".yellow(), path, orig);
                }
            }
            Ok(_) => {
                match media_type {
                    MediaType::Print3D => stats.indexed_3d    += 1,
                    MediaType::Video   => stats.indexed_video  += 1,
                    MediaType::Audio   => stats.indexed_audio  += 1,
                    MediaType::Image   => stats.indexed_image  += 1,
                }
            }
            Err(e) => {
                stats.errors += 1;
                if self.verbose {
                    eprintln!("  {} BD error: {e}", "✗".red());
                }
            }
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

/// Hash completo para archivos pequeños, parcial (cabeza+cola) para grandes.
fn hash_file(path: &Path, size: u64) -> Result<String> {
    if size <= PARTIAL_HASH_THRESHOLD {
        let data = std::fs::read(path)?;
        return Ok(blake3::hash(&data).to_hex().to_string());
    }

    // Hash parcial: primeros 4 MB + últimos 4 MB + tamaño como salt
    let chunk = PARTIAL_CHUNK_SIZE as usize;
    let mut hasher = blake3::Hasher::new();
    let mut file   = std::fs::File::open(path)?;

    // Cabeza
    let mut head = vec![0u8; chunk];
    let n = file.read(&mut head)?;
    hasher.update(&head[..n]);

    // Cola
    if size > (2 * PARTIAL_CHUNK_SIZE) {
        use std::io::Seek;
        file.seek(std::io::SeekFrom::End(-(chunk as i64)))?;
        let mut tail = vec![0u8; chunk];
        let n = file.read(&mut tail)?;
        hasher.update(&tail[..n]);
    }

    // Tamaño como parte del hash (evita colisiones entre archivos distintos con misma cabeza)
    hasher.update(&size.to_le_bytes());

    Ok(hasher.finalize().to_hex().to_string())
}
