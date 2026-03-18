use anyhow::{Context, Result};
use rusqlite::{Connection, params};
use crate::models::*;

pub struct Database {
    conn: Connection,
}

impl Database {
    pub fn open(path: &str) -> Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("No se pudo abrir la BD: {path}"))?;
        let db = Self { conn };
        db.init_schema()?;
        Ok(db)
    }

    fn init_schema(&self) -> Result<()> {
        self.conn.execute_batch("
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous   = NORMAL;
            PRAGMA foreign_keys  = ON;

            -- Tabla principal: un registro por contenido único (hash)
            CREATE TABLE IF NOT EXISTS files (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                blake3_hash     TEXT    NOT NULL UNIQUE,
                size_bytes      INTEGER NOT NULL,
                original_name   TEXT    NOT NULL,
                current_path    TEXT    NOT NULL,
                extension       TEXT    NOT NULL,
                media_type      TEXT    NOT NULL,  -- 3d | video | audio | image
                source_archive  TEXT,
                path_in_archive TEXT,
                indexed_at      TEXT    NOT NULL DEFAULT (datetime('now'))
            );

            -- Metadatos 3D
            CREATE TABLE IF NOT EXISTS meta_3d (
                file_id        INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
                format         TEXT,
                triangle_count INTEGER,
                vertex_count   INTEGER,
                object_count   INTEGER,
                dim_x          REAL,
                dim_y          REAL,
                dim_z          REAL
            );

            -- Metadatos de video
            CREATE TABLE IF NOT EXISTS meta_video (
                file_id       INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
                duration_secs REAL,
                width         INTEGER,
                height        INTEGER,
                codec_video   TEXT,
                codec_audio   TEXT,
                bitrate_kbps  INTEGER,
                fps           REAL,
                title         TEXT,
                year          INTEGER,
                container     TEXT
            );

            -- Metadatos de audio
            CREATE TABLE IF NOT EXISTS meta_audio (
                file_id        INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
                duration_secs  REAL,
                bitrate_kbps   INTEGER,
                sample_rate_hz INTEGER,
                channels       INTEGER,
                title          TEXT,
                artist         TEXT,
                album          TEXT,
                year           INTEGER,
                genre          TEXT,
                track_number   INTEGER
            );

            -- Metadatos de imagen
            CREATE TABLE IF NOT EXISTS meta_image (
                file_id      INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
                width        INTEGER,
                height       INTEGER,
                color_space  TEXT,
                has_alpha    INTEGER,
                camera_make  TEXT,
                camera_model TEXT,
                taken_at     TEXT,
                gps_lat      REAL,
                gps_lon      REAL,
                iso          INTEGER,
                focal_length REAL
            );

            -- Duplicados
            CREATE TABLE IF NOT EXISTS duplicates (
                id             INTEGER PRIMARY KEY AUTOINCREMENT,
                canonical_id   INTEGER NOT NULL REFERENCES files(id),
                duplicate_path TEXT    NOT NULL,
                found_at       TEXT    NOT NULL DEFAULT (datetime('now')),
                UNIQUE(canonical_id, duplicate_path)
            );

            -- Índices
            CREATE INDEX IF NOT EXISTS idx_files_hash      ON files(blake3_hash);
            CREATE INDEX IF NOT EXISTS idx_files_type      ON files(media_type);
            CREATE INDEX IF NOT EXISTS idx_files_name      ON files(original_name);
            CREATE INDEX IF NOT EXISTS idx_files_ext       ON files(extension);
            CREATE INDEX IF NOT EXISTS idx_audio_artist    ON meta_audio(artist);
            CREATE INDEX IF NOT EXISTS idx_audio_album     ON meta_audio(album);
            CREATE INDEX IF NOT EXISTS idx_video_title     ON meta_video(title);
        ")?;
        Ok(())
    }

    // ── Inserción ─────────────────────────────────────────────────────────

    /// Retorna (file_id, es_duplicado, path_canónico_si_duplicado)
    pub fn insert(&self, entry: &MediaEntry) -> Result<(i64, bool, Option<String>)> {
        // ¿Ya existe por hash?
        let existing: Option<(i64, String)> = self.conn.query_row(
            "SELECT id, current_path FROM files WHERE blake3_hash = ?1",
            params![entry.blake3_hash],
            |r| Ok((r.get(0)?, r.get(1)?)),
        ).ok();

        if let Some((canonical_id, canonical_path)) = existing {
            // Mismo path = re-escaneo, no es duplicado real
            if canonical_path == entry.current_path {
                return Ok((canonical_id, false, None));
            }

            // Verificar si ya estaba registrado como duplicado (re-escaneo)
            let already_known: bool = self.conn.query_row(
                "SELECT COUNT(*) FROM duplicates WHERE canonical_id = ?1 AND duplicate_path = ?2",
                params![canonical_id, entry.current_path],
                |r| r.get::<_, i64>(0),
            ).map(|c| c > 0).unwrap_or(false);

            if already_known {
                return Ok((canonical_id, false, None));
            }

            // ¿El recién llegado es más "original" que el canónico actual?
            // Si es así, promoverlo: actualizar current_path en files y registrar
            // el viejo canónico como duplicado en su lugar.
            let incoming_score  = copy_score(&entry.current_path);
            let canonical_score = copy_score(&canonical_path);

            if incoming_score < canonical_score {
                // El recién llegado parece más original — promoverlo a canónico
                self.conn.execute(
                    "UPDATE files SET current_path = ?1, original_name = ?2 WHERE id = ?3",
                    params![entry.current_path, entry.original_name, canonical_id],
                )?;
                // El viejo canónico pasa a ser duplicado
                self.conn.execute(
                    "INSERT OR IGNORE INTO duplicates (canonical_id, duplicate_path)
                     VALUES (?1, ?2)",
                    params![canonical_id, canonical_path],
                )?;
                return Ok((canonical_id, false, None)); // el recién llegado es ahora canónico
            }

            self.conn.execute(
                "INSERT OR IGNORE INTO duplicates (canonical_id, duplicate_path)
                 VALUES (?1, ?2)",
                params![canonical_id, entry.current_path],
            )?;
            return Ok((canonical_id, true, Some(canonical_path)));
        }

        // Insertar en tabla principal
        self.conn.execute(
            "INSERT INTO files
             (blake3_hash, size_bytes, original_name, current_path, extension,
              media_type, source_archive, path_in_archive)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                entry.blake3_hash,
                entry.size_bytes,
                entry.original_name,
                entry.current_path,
                entry.extension,
                entry.media_type.as_str(),
                entry.source_archive,
                entry.path_in_archive,
            ],
        )?;

        let file_id = self.conn.last_insert_rowid();

        // Insertar metadatos específicos
        match &entry.metadata {
            Metadata::Print3D(m) => self.insert_meta_3d(file_id, m)?,
            Metadata::Video(m)   => self.insert_meta_video(file_id, m)?,
            Metadata::Audio(m)   => self.insert_meta_audio(file_id, m)?,
            Metadata::Image(m)   => self.insert_meta_image(file_id, m)?,
            Metadata::None       => {}
        }

        Ok((file_id, false, None))
    }

    fn insert_meta_3d(&self, id: i64, m: &Meta3D) -> Result<()> {
        self.conn.execute(
            "INSERT INTO meta_3d
             (file_id, format, triangle_count, vertex_count, object_count, dim_x, dim_y, dim_z)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                id, m.format,
                m.triangle_count.map(|v| v as i64),
                m.vertex_count.map(|v| v as i64),
                m.object_count, m.dim_x, m.dim_y, m.dim_z,
            ],
        )?;
        Ok(())
    }

    fn insert_meta_video(&self, id: i64, m: &MetaVideo) -> Result<()> {
        self.conn.execute(
            "INSERT INTO meta_video
             (file_id, duration_secs, width, height, codec_video, codec_audio,
              bitrate_kbps, fps, title, year, container)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![
                id, m.duration_secs, m.width, m.height,
                m.codec_video, m.codec_audio,
                m.bitrate_kbps.map(|v| v as i64),
                m.fps, m.title, m.year, m.container,
            ],
        )?;
        Ok(())
    }

    fn insert_meta_audio(&self, id: i64, m: &MetaAudio) -> Result<()> {
        self.conn.execute(
            "INSERT INTO meta_audio
             (file_id, duration_secs, bitrate_kbps, sample_rate_hz, channels,
              title, artist, album, year, genre, track_number)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![
                id, m.duration_secs, m.bitrate_kbps, m.sample_rate_hz,
                m.channels.map(|v| v as i32),
                m.title, m.artist, m.album, m.year, m.genre, m.track_number,
            ],
        )?;
        Ok(())
    }

    fn insert_meta_image(&self, id: i64, m: &MetaImage) -> Result<()> {
        self.conn.execute(
            "INSERT INTO meta_image
             (file_id, width, height, color_space, has_alpha, camera_make,
              camera_model, taken_at, gps_lat, gps_lon, iso, focal_length)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            params![
                id, m.width, m.height, m.color_space,
                m.has_alpha.map(|v| v as i32),
                m.camera_make, m.camera_model, m.taken_at,
                m.gps_lat, m.gps_lon, m.iso, m.focal_length,
            ],
        )?;
        Ok(())
    }

    // ── Mantenimiento ─────────────────────────────────────────────────────

    /// Elimina entradas cuyo path ya no existe en disco.
    /// Debe llamarse al inicio de cada escaneo para que los duplicados
    /// borrados manualmente no persistan en la BD.
    ///
    /// Devuelve (archivos_canónicos_eliminados, duplicados_eliminados).
    pub fn cleanup_stale(&self) -> Result<(usize, usize)> {
        // 1. Duplicados cuyo duplicate_path ya no existe
        let dup_paths: Vec<(i64, String)> = {
            let mut stmt = self.conn.prepare(
                "SELECT id, duplicate_path FROM duplicates"
            )?;
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .filter_map(|r| r.ok())
                .collect()
        };

        let mut dupes_removed = 0usize;
        for (id, path) in &dup_paths {
            // Ignorar entradas de archivos dentro de .zip/.rar (contienen "::")
            if path.contains("::") { continue; }
            if !std::path::Path::new(path).exists() {
                self.conn.execute(
                    "DELETE FROM duplicates WHERE id = ?1",
                    rusqlite::params![id],
                )?;
                dupes_removed += 1;
            }
        }

        // 2. Archivos canónicos cuyo current_path ya no existe
        // (ON DELETE CASCADE limpia duplicates + meta_* automáticamente)
        let file_paths: Vec<(i64, String)> = {
            let mut stmt = self.conn.prepare(
                "SELECT id, current_path FROM files"
            )?;
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .filter_map(|r| r.ok())
                .collect()
        };

        let mut files_removed = 0usize;
        for (id, path) in &file_paths {
            if path.contains("::") { continue; }
            if !std::path::Path::new(path).exists() {
                self.conn.execute(
                    "DELETE FROM files WHERE id = ?1",
                    rusqlite::params![id],
                )?;
                files_removed += 1;
            }
        }

        Ok((files_removed, dupes_removed))
    }

    /// Devuelve true si TODOS los archivos indexados del comprimido son duplicados.
    /// Se usa para decidir si se puede borrar el comprimido entero en modo --aggressive.
    pub fn all_contents_are_duplicates(&self, archive_path: &str) -> Result<bool> {
        // Archivos del comprimido que están como canónicos en `files`
        let canonical_count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM files WHERE source_archive = ?1",
            params![archive_path],
            |r| r.get(0),
        )?;

        // Archivos del comprimido que están en `duplicates`
        let dup_count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM duplicates WHERE duplicate_path LIKE ?1",
            params![format!("{archive_path}::%")],
            |r| r.get(0),
        )?;

        // El comprimido es prescindible si no tiene ningún canónico propio
        // y al menos tiene algún duplicado registrado
        Ok(canonical_count == 0 && dup_count > 0)
    }

    // ── Consultas ─────────────────────────────────────────────────────────

    pub fn stats(&self) -> Result<DbStats> {
        let total: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM files", [], |r| r.get(0))?;
        let dupes: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM duplicates", [], |r| r.get(0))?;
        let bytes: i64 = self.conn.query_row(
            "SELECT COALESCE(SUM(size_bytes),0) FROM files", [], |r| r.get(0))?;
        let bytes_dup: i64 = self.conn.query_row(
            "SELECT COALESCE(SUM(f.size_bytes),0)
             FROM duplicates d JOIN files f ON f.id = d.canonical_id", [], |r| r.get(0))?;

        let by_type: Vec<(String, i64, i64)> = {
            let mut stmt = self.conn.prepare(
                "SELECT media_type, COUNT(*), COALESCE(SUM(size_bytes),0)
                 FROM files GROUP BY media_type ORDER BY 2 DESC"
            )?;
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .filter_map(|r| r.ok())
                .collect()
        };

        Ok(DbStats { total, dupes, bytes, bytes_dup, by_type })
    }

    pub fn duplicates(&self) -> Result<Vec<DuplicateGroup>> {
        let mut stmt = self.conn.prepare("
            SELECT f.blake3_hash, f.original_name, f.current_path,
                   f.size_bytes, f.media_type, d.duplicate_path
            FROM duplicates d
            JOIN files f ON f.id = d.canonical_id
            ORDER BY f.media_type, f.size_bytes DESC
        ")?;

        let mut groups: std::collections::HashMap<String, DuplicateGroup> =
            std::collections::HashMap::new();

        stmt.query_map([], |r| {
            Ok((r.get::<_,String>(0)?, r.get::<_,String>(1)?,
                r.get::<_,String>(2)?, r.get::<_,i64>(3)?,
                r.get::<_,String>(4)?, r.get::<_,String>(5)?))
        })?.filter_map(|r| r.ok()).for_each(|(hash, name, path, size, mtype, dupe)| {
            let entry = groups.entry(hash.clone()).or_insert(DuplicateGroup {
                hash, canonical_name: name, canonical_path: path,
                size_bytes: size as u64, media_type: mtype, duplicates: vec![],
            });
            entry.duplicates.push(dupe);
        });

        let mut list: Vec<_> = groups.into_values().collect();
        list.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes));
        Ok(list)
    }

    pub fn search(&self, query: &str, media_type: Option<&str>) -> Result<Vec<SearchResult>> {
        let pattern = format!("%{query}%");
        let type_filter = media_type.map(|t| format!("AND f.media_type = '{t}'")).unwrap_or_default();

        let sql = format!("
            SELECT f.id, f.original_name, f.current_path, f.media_type,
                   f.size_bytes, f.extension,
                   a.duration_secs, a.artist, a.title as audio_title, a.album,
                   v.duration_secs, v.width, v.height, v.title as video_title,
                   i.width, i.height, i.camera_model,
                   d.triangle_count
            FROM files f
            LEFT JOIN meta_audio a ON a.file_id = f.id
            LEFT JOIN meta_video v ON v.file_id = f.id
            LEFT JOIN meta_image i ON i.file_id = f.id
            LEFT JOIN meta_3d   d ON d.file_id  = f.id
            WHERE f.original_name LIKE ?1 {type_filter}
            ORDER BY f.original_name
            LIMIT 200
        ");

        let mut stmt = self.conn.prepare(&sql)?;
        let results = stmt.query_map(params![pattern], |r| {
            let media_type_str: String = r.get(3)?;
            let media_type = MediaType::from_str(&media_type_str);

            let detail = match media_type {
                MediaType::Audio => SearchDetail::Audio {
                    duration: r.get(6)?,
                    artist:   r.get(7)?,
                    title:    r.get(8)?,
                    album:    r.get(9)?,
                },
                MediaType::Video => SearchDetail::Video {
                    duration: r.get(10)?,
                    width:    r.get(11)?,
                    height:   r.get(12)?,
                    title:    r.get(13)?,
                },
                MediaType::Image => SearchDetail::Image {
                    width:  r.get(14)?,
                    height: r.get(15)?,
                    camera: r.get(16)?,
                },
                MediaType::Print3D => SearchDetail::Print3D {
                    triangles: r.get::<_, Option<i64>>(17)?.map(|v| v as u64),
                },
                MediaType::Other => SearchDetail::Other,
            };

            Ok(SearchResult {
                name:       r.get(1)?,
                path:       r.get(2)?,
                media_type: media_type_str,
                size_bytes: r.get::<_, i64>(4)? as u64,
                extension:  r.get(5)?,
                detail,
            })
        })?.filter_map(|r| r.ok()).collect();

        Ok(results)
    }
}

// ── DTOs de resultado ─────────────────────────────────────────────────────

pub struct DbStats {
    pub total:    i64,
    pub dupes:    i64,
    pub bytes:    i64,
    pub bytes_dup: i64,
    pub by_type:  Vec<(String, i64, i64)>,
}

pub struct DuplicateGroup {
    pub hash:           String,
    pub canonical_name: String,
    pub canonical_path: String,
    pub size_bytes:     u64,
    pub media_type:     String,
    pub duplicates:     Vec<String>,
}

pub struct SearchResult {
    pub name:       String,
    pub path:       String,
    pub media_type: String,
    pub size_bytes: u64,
    pub extension:  String,
    pub detail:     SearchDetail,
}

pub enum SearchDetail {
    Audio  { duration: Option<f64>, artist: Option<String>, title: Option<String>, album: Option<String> },
    Video  { duration: Option<f64>, width: Option<u32>, height: Option<u32>, title: Option<String> },
    Image  { width: Option<u32>, height: Option<u32>, camera: Option<String> },
    Print3D { triangles: Option<u64> },
    Other,
}

// ── Helpers internos ──────────────────────────────────────────────────────

/// Devuelve una puntuación de "cuánto parece una copia" basada en el nombre del archivo.
/// Menor puntuación = más original. Se usa para decidir qué path queda como canónico.
///
/// Patrones detectados (Windows/macOS/Linux en español e inglés):
///   " - copia", " - copia (2)", "- Copy", " (1)", "_copy", "backup", etc.
fn copy_score(path: &str) -> u32 {
    let name = std::path::Path::new(path)
        .file_stem()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default();

    let mut score = 0u32;

    // Windows español: "archivo - copia", "archivo - copia (2)"
    if name.contains(" - copia") { score += 100; }

    // Windows inglés: "file - Copy", "file - Copy (2)"
    if name.contains(" - copy") { score += 100; }

    // macOS / Linux: "file (1)", "file (2)", ...
    if name.ends_with(')') {
        let re = name.trim_end_matches(|c: char| c.is_ascii_digit() || c == ' ' || c == '(');
        let suffix = &name[re.len()..];
        if suffix.trim().starts_with('(') { score += 80; }
    }

    // Sufijos numéricos: "file_1", "file_2", "file 1", "file 2"
    if name.chars().last().map(|c| c.is_ascii_digit()).unwrap_or(false) {
        let trimmed = name.trim_end_matches(|c: char| c.is_ascii_digit());
        if trimmed.ends_with('_') || trimmed.ends_with(' ') { score += 50; }
    }

    // Palabras clave genéricas en el nombre
    for keyword in &["_copy", "_backup", "_bak", " backup", " bak", "copy_of", "copia_de"] {
        if name.contains(keyword) { score += 60; }
    }

    score
}
