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
                // El recién llegado parece más original — promoverlo a canónico.
                // Actualizar current_path, original_name, source_archive y path_in_archive
                self.conn.execute(
                    "UPDATE files
                     SET current_path    = ?1,
                         original_name   = ?2,
                         source_archive  = ?3,
                         path_in_archive = ?4
                     WHERE id = ?5",
                    params![
                        entry.current_path,
                        entry.original_name,
                        entry.source_archive,
                        entry.path_in_archive,
                        canonical_id,
                    ],
                )?;
                // El viejo canónico pasa a ser duplicado
                self.conn.execute(
                    "INSERT OR IGNORE INTO duplicates (canonical_id, duplicate_path)
                     VALUES (?1, ?2)",
                    params![canonical_id, canonical_path],
                )?;
                return Ok((canonical_id, false, None));
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

    /// Elimina entradas cuyo path ya no existe en disco, incluyendo las que
    /// provienen de comprimidos cuyo archivo padre fue borrado manualmente.
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
            let stale = if let Some(archive) = path.splitn(2, "::").next().filter(|_| path.contains("::")) {
                // Entrada dentro de un comprimido: stale si el comprimido ya no existe
                !std::path::Path::new(archive).exists()
            } else {
                !std::path::Path::new(path).exists()
            };
            if stale {
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
            let stale = if let Some(archive) = path.splitn(2, "::").next().filter(|_| path.contains("::")) {
                // Canónico dentro de un comprimido: stale si el comprimido ya no existe
                !std::path::Path::new(archive).exists()
            } else {
                !std::path::Path::new(path).exists()
            };
            if stale {
                self.conn.execute(
                    "DELETE FROM files WHERE id = ?1",
                    rusqlite::params![id],
                )?;
                files_removed += 1;
            }
        }

        Ok((files_removed, dupes_removed))
    }

    /// Devuelve true si el comprimido puede borrarse de forma segura.
    /// Condición: TODOS los archivos que contiene tienen al menos una copia
    /// en otro lugar que no esté siendo borrado en esta misma operación.
    ///
    /// No depende de la distinción canónico/duplicado — trabaja directamente
    /// con hashes, que es la fuente de verdad real.
    pub fn can_safely_delete_archive(
        &self,
        archive_path:      &str,
        deleted_paths:     &std::collections::HashSet<String>,
        archives_to_del:   &std::collections::HashSet<String>,
    ) -> Result<bool> {
        // 1. Obtener TODOS los hashes del comprimido (canónicos + duplicados)
        let hashes: Vec<String> = {
            let mut stmt = self.conn.prepare(
                "-- Canónicos cuya fuente es este comprimido
                 SELECT blake3_hash FROM files
                 WHERE source_archive = ?1
                   AND path_in_archive IS NOT NULL
                 UNION
                 -- Duplicados cuya ruta es dentro de este comprimido
                 SELECT DISTINCT f.blake3_hash
                 FROM files f
                 JOIN duplicates d ON d.canonical_id = f.id
                 WHERE d.duplicate_path LIKE ?2"
            )?;
            stmt.query_map(
                params![archive_path, format!("{archive_path}::%")],
                |r| r.get(0),
            )?.filter_map(|r| r.ok()).collect()
        };

        // Si no hay ningún archivo indexado → no borrar (vacío o no escaneado)
        if hashes.is_empty() { return Ok(false); }

        // 2. Para cada hash, verificar que existe otra copia fuera de este comprimido
        for hash in &hashes {
            let copies: Vec<String> = {
                let mut stmt = self.conn.prepare(
                    "SELECT current_path FROM files WHERE blake3_hash = ?1
                     UNION
                     SELECT d.duplicate_path
                     FROM duplicates d
                     JOIN files f ON f.id = d.canonical_id
                     WHERE f.blake3_hash = ?1"
                )?;
                stmt.query_map(params![hash], |r| r.get(0))?
                    .filter_map(|r| r.ok())
                    .collect()
            };

            let has_surviving_copy = copies.iter().any(|copy| {
                // Excluir copias dentro de ESTE comprimido
                if copy.starts_with(&format!("{archive_path}::")) || copy == archive_path {
                    return false;
                }
                // Excluir paths ya borrados en este run
                if deleted_paths.contains(copy) { return false; }

                if copy.contains("::") {
                    // Copia dentro de otro comprimido
                    let parent = copy.splitn(2, "::").next().unwrap_or("");
                    // Ese comprimido no debe estar siendo borrado
                    if archives_to_del.contains(parent) { return false; }
                    // Y debe existir en disco
                    std::path::Path::new(parent).exists()
                } else {
                    // Archivo suelto: debe existir en disco
                    std::path::Path::new(copy).exists()
                }
            });

            if !has_surviving_copy { return Ok(false); }
        }

        Ok(true)
    }

    /// Devuelve todos los archivos indexados con su hash y tamaño para verificación.
    /// Excluye los que viven dentro de comprimidos (no se pueden re-hashear sin extraer).
    pub fn files_for_verify(&self) -> Result<Vec<(i64, String, String, u64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, blake3_hash, current_path, size_bytes
             FROM files
             WHERE source_archive IS NULL
             ORDER BY size_bytes DESC"
        )?;
        let results = stmt.query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get::<_, i64>(3)? as u64))
        })?.filter_map(|r| r.ok()).collect();
        Ok(results)
    }

    /// Elimina de la BD un archivo canónico por id (y sus duplicados en cascade).
    pub fn remove_file(&self, id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM files WHERE id = ?1", rusqlite::params![id])?;
        Ok(())
    }

    /// Elimina todas las entradas de basura macOS (__MACOSX/, ._*, .DS_Store)
    /// que hayan podido indexarse antes de que existiera el filtro.
    /// Devuelve el número de entradas eliminadas.
    pub fn purge_macos_junk(&self) -> Result<usize> {
        // Duplicados con ruta de basura macOS
        let dup_del = self.conn.execute(
            "DELETE FROM duplicates
             WHERE duplicate_path LIKE '%::__MACOSX/%'
                OR duplicate_path LIKE '%::.__%'
                OR duplicate_path LIKE '%::.DS_Store'",
            [],
        )?;

        // Canónicos con ruta de basura macOS (CASCADE elimina sus duplicados y meta)
        let file_del = self.conn.execute(
            "DELETE FROM files
             WHERE current_path LIKE '%::__MACOSX/%'
                OR current_path LIKE '%::.__%'
                OR current_path LIKE '%::.DS_Store'
                OR (source_archive IS NOT NULL AND (
                       path_in_archive LIKE '__MACOSX/%'
                    OR path_in_archive LIKE '._%'
                    OR path_in_archive = '.DS_Store'
                ))",
            [],
        )?;

        Ok(dup_del + file_del)
    }

    /// Devuelve todos los archivos candidatos a thumbnail (imágenes, videos y 3D),
    /// incluyendo los que están dentro de comprimidos.
    pub fn files_for_thumbs(
        &self,
        media_type: Option<&str>,
    ) -> Result<Vec<(String, String, String, String)>> {
        let type_filter = match media_type {
            Some(t) => format!("media_type = '{t}'"),
            None    => "media_type IN ('image', 'video', '3d')".to_string(),
        };

        let sql = format!(
            "SELECT blake3_hash, current_path, media_type, extension
             FROM files
             WHERE {type_filter}
             ORDER BY media_type, size_bytes DESC"
        );

        let mut stmt = self.conn.prepare(&sql)?;
        let results = stmt.query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?.filter_map(|r| r.ok()).collect();

        Ok(results)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{MediaEntry, MediaType, Metadata};

    fn mem_db() -> Database {
        Database::open(":memory:").unwrap()
    }

    fn entry(hash: &str, path: &str, name: &str) -> MediaEntry {
        MediaEntry {
            blake3_hash:     hash.to_string(),
            size_bytes:      1_000,
            original_name:   name.to_string(),
            current_path:    path.to_string(),
            extension:       "jpg".to_string(),
            media_type:      MediaType::Image,
            metadata:        Metadata::None,
            source_archive:  None,
            path_in_archive: None,
        }
    }

    // ── copy_score ────────────────────────────────────────────────────────

    #[test]
    fn copy_score_original_bajo() {
        let s = copy_score("/fotos/vacaciones.jpg");
        assert!(s < 5_000, "score={s}");
    }

    #[test]
    fn copy_score_copia_espanol() {
        assert!(copy_score("/fotos/foto - copia.jpg")   >= 10_000);
        assert!(copy_score("/fotos/foto - copia (2).jpg") >= 10_000);
    }

    #[test]
    fn copy_score_copy_ingles() {
        assert!(copy_score("/fotos/photo - Copy.jpg")   >= 10_000);
    }

    #[test]
    fn copy_score_sufijo_numerico_con_guion() {
        // "file_1", "file_2" → sufijo numérico precedido de '_'
        assert!(copy_score("/fotos/imagen_2.jpg")  >= 5_000);
        assert!(copy_score("/fotos/imagen_10.jpg") >= 5_000);
    }

    #[test]
    fn copy_score_backup() {
        assert!(copy_score("/fotos/foto_backup.jpg") >= 6_000);
        assert!(copy_score("/fotos/foto_bak.jpg")    >= 6_000);
    }

    #[test]
    fn copy_score_original_gana_a_copia() {
        let orig = copy_score("/fotos/vacaciones.jpg");
        let copy = copy_score("/fotos/vacaciones - copia.jpg");
        assert!(orig < copy);
    }

    // ── insert: nuevo archivo ─────────────────────────────────────────────

    #[test]
    fn insert_nuevo_no_es_duplicado() {
        let db = mem_db();
        let (_, is_dup, canon) = db.insert(&entry("h1", "/a.jpg", "a.jpg")).unwrap();
        assert!(!is_dup);
        assert!(canon.is_none());
    }

    #[test]
    fn insert_mismo_hash_distinto_path_es_duplicado() {
        let db = mem_db();
        db.insert(&entry("h1", "/orig/a.jpg", "a.jpg")).unwrap();
        let (_, is_dup, canon) = db.insert(&entry("h1", "/copy/a.jpg", "a.jpg")).unwrap();
        assert!(is_dup);
        assert_eq!(canon.unwrap(), "/orig/a.jpg");
    }

    #[test]
    fn insert_rescan_mismo_path_no_es_duplicado() {
        let db = mem_db();
        db.insert(&entry("h1", "/a.jpg", "a.jpg")).unwrap();
        let (_, is_dup, _) = db.insert(&entry("h1", "/a.jpg", "a.jpg")).unwrap();
        assert!(!is_dup);
    }

    #[test]
    fn insert_promueve_mas_original_a_canonico() {
        let db = mem_db();
        // Primero se indexa la copia
        db.insert(&entry("h1", "/fotos/foto - copia.jpg", "foto - copia.jpg")).unwrap();
        // Luego el original — debe promoverse a canónico
        let (_, is_dup, _) = db.insert(&entry("h1", "/fotos/foto.jpg", "foto.jpg")).unwrap();
        assert!(!is_dup);
        let groups = db.duplicates().unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].canonical_path, "/fotos/foto.jpg");
        assert!(groups[0].duplicates.contains(&"/fotos/foto - copia.jpg".to_string()));
    }

    // ── stats ─────────────────────────────────────────────────────────────

    #[test]
    fn stats_bd_vacia() {
        let s = mem_db().stats().unwrap();
        assert_eq!(s.total, 0);
        assert_eq!(s.dupes, 0);
        assert_eq!(s.bytes, 0);
    }

    #[test]
    fn stats_con_duplicado() {
        let db = mem_db();
        db.insert(&entry("h1", "/a.jpg", "a.jpg")).unwrap();
        db.insert(&entry("h2", "/b.jpg", "b.jpg")).unwrap();
        db.insert(&entry("h1", "/c.jpg", "c.jpg")).unwrap(); // dup de h1
        let s = db.stats().unwrap();
        assert_eq!(s.total, 2);
        assert_eq!(s.dupes, 1);
    }

    // ── search ────────────────────────────────────────────────────────────

    #[test]
    fn search_encuentra_por_nombre_parcial() {
        let db = mem_db();
        db.insert(&entry("h1", "/fotos/vacaciones.jpg", "vacaciones.jpg")).unwrap();
        let r = db.search("vacacion", None).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].name, "vacaciones.jpg");
    }

    #[test]
    fn search_sin_resultados() {
        let db = mem_db();
        db.insert(&entry("h1", "/fotos/foto.jpg", "foto.jpg")).unwrap();
        assert!(db.search("xyznotfound", None).unwrap().is_empty());
    }

    #[test]
    fn search_filtro_por_tipo() {
        let db = mem_db();
        db.insert(&entry("h1", "/fotos/foto.jpg", "foto.jpg")).unwrap();
        assert!(db.search("foto", Some("video")).unwrap().is_empty());
        assert_eq!(db.search("foto", Some("image")).unwrap().len(), 1);
    }

    #[test]
    fn search_insensible_a_mayusculas() {
        let db = mem_db();
        db.insert(&entry("h1", "/Foto.jpg", "Foto.jpg")).unwrap();
        assert_eq!(db.search("foto", None).unwrap().len(), 1);
        assert_eq!(db.search("FOTO", None).unwrap().len(), 1);
    }

    // ── duplicates ────────────────────────────────────────────────────────

    #[test]
    fn duplicates_vacio() {
        assert!(mem_db().duplicates().unwrap().is_empty());
    }

    #[test]
    fn duplicates_agrupa_correctamente() {
        let db = mem_db();
        db.insert(&entry("h1", "/a.jpg", "a.jpg")).unwrap();
        db.insert(&entry("h1", "/b.jpg", "b.jpg")).unwrap();
        db.insert(&entry("h1", "/c.jpg", "c.jpg")).unwrap();
        let groups = db.duplicates().unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].duplicates.len(), 2);
    }

    #[test]
    fn duplicates_dos_grupos_independientes() {
        let db = mem_db();
        db.insert(&entry("h1", "/a1.jpg", "a1.jpg")).unwrap();
        db.insert(&entry("h1", "/a2.jpg", "a2.jpg")).unwrap();
        db.insert(&entry("h2", "/b1.jpg", "b1.jpg")).unwrap();
        db.insert(&entry("h2", "/b2.jpg", "b2.jpg")).unwrap();
        assert_eq!(db.duplicates().unwrap().len(), 2);
    }

    // ── cleanup_stale ─────────────────────────────────────────────────────

    #[test]
    fn cleanup_stale_elimina_path_inexistente() {
        let db = mem_db();
        db.insert(&entry("h1", "/ruta/que/no/existe.jpg", "inexistente.jpg")).unwrap();
        let (removed, _) = db.cleanup_stale().unwrap();
        assert_eq!(removed, 1);
        assert_eq!(db.stats().unwrap().total, 0);
    }

    #[test]
    fn cleanup_stale_conserva_archivo_existente() {
        let db = mem_db();
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_str().unwrap().to_string();
        db.insert(&entry("h1", &path, "tmp.jpg")).unwrap();
        let (removed, _) = db.cleanup_stale().unwrap();
        assert_eq!(removed, 0);
        assert_eq!(db.stats().unwrap().total, 1);
    }

    #[test]
    fn cleanup_stale_elimina_duplicados_de_archivo_borrado() {
        let db = mem_db();
        // Canónico que sí existe
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_str().unwrap().to_string();
        db.insert(&entry("h1", &path, "real.jpg")).unwrap();
        // Duplicado con path inexistente — cleanup_stale lo debe eliminar
        db.insert(&entry("h1", "/no/existe/copia.jpg", "copia.jpg")).unwrap();
        let (_, dupes_removed) = db.cleanup_stale().unwrap();
        assert_eq!(dupes_removed, 1);
        assert_eq!(db.duplicates().unwrap().len(), 0);
    }

    // ── purge_macos_junk ─────────────────────────────────────────────────

    #[test]
    fn purge_elimina_macosx_en_path() {
        let db = mem_db();
        db.insert(&entry("h1", "/arc.zip::__MACOSX/file.jpg", "file.jpg")).unwrap();
        db.insert(&entry("h2", "/normal.jpg", "normal.jpg")).unwrap();
        let removed = db.purge_macos_junk().unwrap();
        assert!(removed >= 1);
        assert_eq!(db.stats().unwrap().total, 1); // solo queda el normal
    }

    #[test]
    fn purge_elimina_dot_underscore() {
        let db = mem_db();
        db.insert(&entry("h1", "/arc.zip::._hidden", "._hidden")).unwrap();
        let removed = db.purge_macos_junk().unwrap();
        assert!(removed >= 1);
    }

    #[test]
    fn purge_elimina_ds_store() {
        let db = mem_db();
        db.insert(&entry("h1", "/arc.zip::.DS_Store", ".DS_Store")).unwrap();
        let removed = db.purge_macos_junk().unwrap();
        assert!(removed >= 1);
    }

    #[test]
    fn purge_por_source_archive_y_path_in_archive() {
        let db = mem_db();
        let mut e = entry("h1", "/arc.zip::__MACOSX/._icon", "._icon");
        e.source_archive  = Some("/arc.zip".to_string());
        e.path_in_archive = Some("__MACOSX/._icon".to_string());
        db.insert(&e).unwrap();
        assert!(db.purge_macos_junk().unwrap() >= 1);
        assert_eq!(db.stats().unwrap().total, 0);
    }

    #[test]
    fn purge_no_toca_archivos_normales() {
        let db = mem_db();
        db.insert(&entry("h1", "/fotos/foto.jpg", "foto.jpg")).unwrap();
        db.insert(&entry("h2", "/arc.zip::real.jpg", "real.jpg")).unwrap();
        let removed = db.purge_macos_junk().unwrap();
        assert_eq!(removed, 0);
        assert_eq!(db.stats().unwrap().total, 2);
    }

    // ── files_for_verify ─────────────────────────────────────────────────

    #[test]
    fn files_for_verify_excluye_entradas_de_comprimidos() {
        let db = mem_db();
        db.insert(&entry("h1", "/archivo.jpg", "archivo.jpg")).unwrap();
        let mut arc = entry("h2", "/arc.zip::inner.jpg", "inner.jpg");
        arc.source_archive  = Some("/arc.zip".to_string());
        arc.path_in_archive = Some("inner.jpg".to_string());
        db.insert(&arc).unwrap();
        let files = db.files_for_verify().unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].2, "/archivo.jpg");
    }

    #[test]
    fn files_for_verify_incluye_hash_y_size() {
        let db = mem_db();
        db.insert(&entry("deadbeef", "/x.jpg", "x.jpg")).unwrap();
        let files = db.files_for_verify().unwrap();
        assert_eq!(files[0].1, "deadbeef");
        assert_eq!(files[0].3, 1_000);
    }

    // ── remove_file ───────────────────────────────────────────────────────

    #[test]
    fn remove_file_elimina_canonico_sin_duplicados() {
        // remove_file está diseñado para usarse sobre archivos sin duplicados activos
        // (canonical_id en duplicates no tiene ON DELETE CASCADE;
        //  cleanup_stale borra los duplicados huérfanos antes de borrar el canónico)
        let db = mem_db();
        db.insert(&entry("h1", "/a.jpg", "a.jpg")).unwrap();
        db.insert(&entry("h2", "/b.jpg", "b.jpg")).unwrap();
        let files = db.files_for_verify().unwrap();
        let id_a = files.iter().find(|f| f.2 == "/a.jpg").unwrap().0;
        db.remove_file(id_a).unwrap();
        assert_eq!(db.stats().unwrap().total, 1);
        assert_eq!(db.files_for_verify().unwrap()[0].2, "/b.jpg");
    }

    #[test]
    fn remove_file_elimina_meta_en_cascade() {
        // Los metadatos sí tienen ON DELETE CASCADE
        let db = mem_db();
        let mut e = entry("h1", "/cancion.mp3", "cancion.mp3");
        e.extension  = "mp3".to_string();
        e.media_type = MediaType::Audio;
        e.metadata   = Metadata::Audio(crate::models::MetaAudio {
            duration_secs: Some(180.0),
            artist:        Some("Artista".to_string()),
            ..Default::default()
        });
        db.insert(&e).unwrap();
        let id = db.files_for_verify().unwrap()[0].0;
        db.remove_file(id).unwrap();
        assert_eq!(db.stats().unwrap().total, 0);
    }
}

/// Devuelve una puntuación de "cuánto parece una copia" basada en el nombre del archivo.
/// Menor puntuación = más original. Se usa para decidir qué path queda como canónico.
///
/// Patrones detectados (Windows/macOS/Linux en español e inglés):
///   " - copia", " - copia (2)", "- Copy", " (1)", "_copy", "backup", etc.
///
/// Tiebreaker: cuando dos paths tienen el mismo score de copia, se prefiere el de
/// nombre más largo (más descriptivo). Ej: "hellboy.rar::film" > "h.rar::film".
fn copy_score(path: &str) -> u32 {
    let name = std::path::Path::new(path)
        .file_stem()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default();

    let mut score = 0u32;

    // Windows español: "archivo - copia", "archivo - copia (2)"
    if name.contains(" - copia") { score += 10_000; }

    // Windows inglés: "file - Copy", "file - Copy (2)"
    if name.contains(" - copy") { score += 10_000; }

    // macOS / Linux: "file (1)", "file (2)", ...
    if name.ends_with(')') {
        let re = name.trim_end_matches(|c: char| c.is_ascii_digit() || c == ' ' || c == '(');
        let suffix = &name[re.len()..];
        if suffix.trim().starts_with('(') { score += 8_000; }
    }

    // Sufijos numéricos: "file_1", "file_2", "file 1", "file 2"
    if name.chars().last().map(|c| c.is_ascii_digit()).unwrap_or(false) {
        let trimmed = name.trim_end_matches(|c: char| c.is_ascii_digit());
        if trimmed.ends_with('_') || trimmed.ends_with(' ') { score += 5_000; }
    }

    // Palabras clave genéricas en el nombre
    for keyword in &["_copy", "_backup", "_bak", " backup", " bak", "copy_of", "copia_de"] {
        if name.contains(keyword) { score += 6_000; }
    }

    // Tiebreaker: penalizar nombres cortos. Nombres más largos son más descriptivos
    // y probablemente más originales (ej. "hellboy" > "h", "documento" > "doc1").
    // La penalización es pequeña (max 255) para no superar ningún patrón de copia.
    let name_len = name.chars().count().min(255) as u32;
    score += 255u32.saturating_sub(name_len);

    score
}
