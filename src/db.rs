use crate::models::*;
use anyhow::{Context, Result};
use rusqlite::{Connection, params};

pub struct Database {
    conn: Connection,
}

impl Database {
    pub fn open(path: &str) -> Result<Self> {
        let conn = Connection::open(path).with_context(|| format!("Could not open DB: {path}"))?;
        let db = Self { conn };
        db.init_schema()?;
        Ok(db)
    }

    fn init_schema(&self) -> Result<()> {
        self.conn.execute_batch(
            "
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous   = NORMAL;
            PRAGMA foreign_keys  = ON;

            -- Main table: one record per unique content (hash)
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
                mtime           INTEGER,           -- unix timestamp, for incremental re-scan
                indexed_at      TEXT    NOT NULL DEFAULT (datetime('now'))
            );

            -- 3D metadata
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

            -- Video metadata
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

            -- Audio metadata
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

            -- Image metadata
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
                focal_length REAL,
                phash        TEXT
            );

            -- Duplicates
            CREATE TABLE IF NOT EXISTS duplicates (
                id             INTEGER PRIMARY KEY AUTOINCREMENT,
                canonical_id   INTEGER NOT NULL REFERENCES files(id),
                duplicate_path TEXT    NOT NULL,
                found_at       TEXT    NOT NULL DEFAULT (datetime('now')),
                UNIQUE(canonical_id, duplicate_path)
            );

            -- Indexes
            CREATE INDEX IF NOT EXISTS idx_files_hash      ON files(blake3_hash);
            CREATE INDEX IF NOT EXISTS idx_files_type      ON files(media_type);
            CREATE INDEX IF NOT EXISTS idx_files_name      ON files(original_name);
            CREATE INDEX IF NOT EXISTS idx_files_ext       ON files(extension);
            CREATE INDEX IF NOT EXISTS idx_audio_artist    ON meta_audio(artist);
            CREATE INDEX IF NOT EXISTS idx_audio_album     ON meta_audio(album);
            CREATE INDEX IF NOT EXISTS idx_video_title     ON meta_video(title);
            CREATE INDEX IF NOT EXISTS idx_image_phash     ON meta_image(phash);
        ",
        )?;

        // Migration for existing DBs: add phash if the column does not exist.
        // ALTER TABLE fails silently if the column already exists.
        let _ = self
            .conn
            .execute("ALTER TABLE meta_image ADD COLUMN phash TEXT", []);
        let _ = self.conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_image_phash ON meta_image(phash)",
            [],
        );
        // Migration: add mtime if it does not exist
        let _ = self
            .conn
            .execute("ALTER TABLE files ADD COLUMN mtime INTEGER", []);
        let _ = self.conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_files_path ON files(current_path)",
            [],
        );

        Ok(())
    }

    // ── Insertion ─────────────────────────────────────────────────────────

    /// Returns (file_id, is_duplicate, canonical_path_if_duplicate)
    pub fn insert(&self, entry: &MediaEntry) -> Result<(i64, bool, Option<String>)> {
        // Already exists by hash?
        let existing: Option<(i64, String)> = self
            .conn
            .query_row(
                "SELECT id, current_path FROM files WHERE blake3_hash = ?1",
                params![entry.blake3_hash],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok();

        if let Some((canonical_id, canonical_path)) = existing {
            // Same path = re-scan, not a real duplicate
            if canonical_path == entry.current_path {
                return Ok((canonical_id, false, None));
            }

            // Check if it was already registered as a duplicate (re-scan)
            let already_known: bool = self.conn.query_row(
                "SELECT COUNT(*) FROM duplicates WHERE canonical_id = ?1 AND duplicate_path = ?2",
                params![canonical_id, entry.current_path],
                |r| r.get::<_, i64>(0),
            ).map(|c| c > 0).unwrap_or(false);

            if already_known {
                return Ok((canonical_id, false, None));
            }

            // Is the newcomer more "original" than the current canonical?
            // If so, promote it: update current_path in files and register
            // the old canonical as a duplicate instead.
            let incoming_score = copy_score(&entry.current_path);
            let canonical_score = copy_score(&canonical_path);

            if incoming_score < canonical_score {
                // The newcomer looks more original — promote it to canonical.
                // Update current_path, original_name, source_archive and path_in_archive
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
                // The old canonical becomes a duplicate
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

        // Insert into main table
        self.conn.execute(
            "INSERT INTO files
             (blake3_hash, size_bytes, original_name, current_path, extension,
              media_type, source_archive, path_in_archive, mtime)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![
                entry.blake3_hash,
                entry.size_bytes,
                entry.original_name,
                entry.current_path,
                entry.extension,
                entry.media_type.as_str(),
                entry.source_archive,
                entry.path_in_archive,
                entry.mtime,
            ],
        )?;

        let file_id = self.conn.last_insert_rowid();

        // Insert type-specific metadata
        match &entry.metadata {
            Metadata::Print3D(m) => self.insert_meta_3d(file_id, m)?,
            Metadata::Video(m) => self.insert_meta_video(file_id, m)?,
            Metadata::Audio(m) => self.insert_meta_audio(file_id, m)?,
            Metadata::Image(m) => self.insert_meta_image(file_id, m)?,
            Metadata::None => {}
        }

        Ok((file_id, false, None))
    }

    /// Looks up an already-indexed file by its exact path.
    /// Returns (blake3_hash, size_bytes, mtime) if found.
    /// Used by the scanner for incremental re-scan: if mtime + size have not
    /// changed, the file was not modified and we can reuse the cached hash.
    pub fn find_by_path(&self, path: &str) -> Option<CachedFile> {
        self.conn
            .query_row(
                "SELECT blake3_hash, size_bytes, mtime FROM files WHERE current_path = ?1",
                params![path],
                |r| {
                    Ok(CachedFile {
                        blake3_hash: r.get(0)?,
                        size_bytes: r.get::<_, i64>(1)? as u64,
                        mtime: r.get::<_, Option<i64>>(2)?.map(|v| v as u64),
                    })
                },
            )
            .ok()
    }

    fn insert_meta_3d(&self, id: i64, m: &Meta3D) -> Result<()> {
        self.conn.execute(
            "INSERT INTO meta_3d
             (file_id, format, triangle_count, vertex_count, object_count, dim_x, dim_y, dim_z)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                id,
                m.format,
                m.triangle_count.map(|v| v as i64),
                m.vertex_count.map(|v| v as i64),
                m.object_count,
                m.dim_x,
                m.dim_y,
                m.dim_z,
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
                id,
                m.duration_secs,
                m.width,
                m.height,
                m.codec_video,
                m.codec_audio,
                m.bitrate_kbps.map(|v| v as i64),
                m.fps,
                m.title,
                m.year,
                m.container,
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
                id,
                m.duration_secs,
                m.bitrate_kbps,
                m.sample_rate_hz,
                m.channels.map(|v| v as i32),
                m.title,
                m.artist,
                m.album,
                m.year,
                m.genre,
                m.track_number,
            ],
        )?;
        Ok(())
    }

    fn insert_meta_image(&self, id: i64, m: &MetaImage) -> Result<()> {
        self.conn.execute(
            "INSERT INTO meta_image
             (file_id, width, height, color_space, has_alpha, camera_make,
              camera_model, taken_at, gps_lat, gps_lon, iso, focal_length, phash)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            params![
                id,
                m.width,
                m.height,
                m.color_space,
                m.has_alpha.map(|v| v as i32),
                m.camera_make,
                m.camera_model,
                m.taken_at,
                m.gps_lat,
                m.gps_lon,
                m.iso,
                m.focal_length,
                m.phash,
            ],
        )?;
        Ok(())
    }

    // ── Maintenance ───────────────────────────────────────────────────────

    /// Removes entries whose path no longer exists on disk, including those
    /// from archives whose parent file was manually deleted.
    /// Should be called at the start of each scan so that manually deleted
    /// duplicates do not persist in the DB.
    ///
    /// Returns (canonical_files_removed, duplicates_removed).
    pub fn cleanup_stale(&self) -> Result<(usize, usize)> {
        // 1. Duplicates whose duplicate_path no longer exists
        let dup_paths: Vec<(i64, String)> = {
            let mut stmt = self
                .conn
                .prepare("SELECT id, duplicate_path FROM duplicates")?;
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .filter_map(|r| r.ok())
                .collect()
        };

        let mut dupes_removed = 0usize;
        for (id, path) in &dup_paths {
            let stale = if let Some(archive) =
                path.splitn(2, "::").next().filter(|_| path.contains("::"))
            {
                // Entry inside an archive: stale if the archive no longer exists
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

        // 2. Canonical files whose current_path no longer exists
        // (ON DELETE CASCADE cleans up duplicates + meta_* automatically)
        let file_paths: Vec<(i64, String)> = {
            let mut stmt = self.conn.prepare("SELECT id, current_path FROM files")?;
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .filter_map(|r| r.ok())
                .collect()
        };

        let mut files_removed = 0usize;
        for (id, path) in &file_paths {
            let stale = if let Some(archive) =
                path.splitn(2, "::").next().filter(|_| path.contains("::"))
            {
                // Canonical inside an archive: stale if the archive no longer exists
                !std::path::Path::new(archive).exists()
            } else {
                !std::path::Path::new(path).exists()
            };
            if stale {
                self.conn
                    .execute("DELETE FROM files WHERE id = ?1", rusqlite::params![id])?;
                files_removed += 1;
            }
        }

        Ok((files_removed, dupes_removed))
    }

    /// Returns true if the archive can be safely deleted.
    /// Condition: ALL files it contains have at least one copy
    /// elsewhere that is not being deleted in this same operation.
    ///
    /// Does not depend on the canonical/duplicate distinction — works directly
    /// with hashes, which are the real source of truth.
    pub fn can_safely_delete_archive(
        &self,
        archive_path: &str,
        deleted_paths: &std::collections::HashSet<String>,
        archives_to_del: &std::collections::HashSet<String>,
    ) -> Result<bool> {
        // 1. Get ALL hashes from the archive (canonical + duplicates)
        let hashes: Vec<String> = {
            let mut stmt = self.conn.prepare(
                "-- Canonical files whose source is this archive
                 SELECT blake3_hash FROM files
                 WHERE source_archive = ?1
                   AND path_in_archive IS NOT NULL
                 UNION
                 -- Duplicates whose path is inside this archive
                 SELECT DISTINCT f.blake3_hash
                 FROM files f
                 JOIN duplicates d ON d.canonical_id = f.id
                 WHERE d.duplicate_path LIKE ?2",
            )?;
            stmt.query_map(params![archive_path, format!("{archive_path}::%")], |r| {
                r.get(0)
            })?
            .filter_map(|r| r.ok())
            .collect()
        };

        // No indexed files found → do not delete (empty or not yet scanned)
        if hashes.is_empty() {
            return Ok(false);
        }

        // 2. For each hash, verify that another copy exists outside this archive
        for hash in &hashes {
            let copies: Vec<String> = {
                let mut stmt = self.conn.prepare(
                    "SELECT current_path FROM files WHERE blake3_hash = ?1
                     UNION
                     SELECT d.duplicate_path
                     FROM duplicates d
                     JOIN files f ON f.id = d.canonical_id
                     WHERE f.blake3_hash = ?1",
                )?;
                stmt.query_map(params![hash], |r| r.get(0))?
                    .filter_map(|r| r.ok())
                    .collect()
            };

            let has_surviving_copy = copies.iter().any(|copy| {
                // Exclude copies inside THIS archive
                if copy.starts_with(&format!("{archive_path}::")) || copy == archive_path {
                    return false;
                }
                // Exclude paths already deleted in this run
                if deleted_paths.contains(copy) {
                    return false;
                }

                if copy.contains("::") {
                    // Copy inside another archive
                    let parent = copy.splitn(2, "::").next().unwrap_or("");
                    // That archive must not be getting deleted
                    if archives_to_del.contains(parent) {
                        return false;
                    }
                    // And it must exist on disk
                    std::path::Path::new(parent).exists()
                } else {
                    // Loose file: must exist on disk
                    std::path::Path::new(copy).exists()
                }
            });

            if !has_surviving_copy {
                return Ok(false);
            }
        }

        Ok(true)
    }

    /// Returns all indexed files with their hash and size for verification.
    /// Excludes those living inside archives (cannot be re-hashed without extracting).
    pub fn files_for_verify(&self) -> Result<Vec<(i64, String, String, u64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, blake3_hash, current_path, size_bytes
             FROM files
             WHERE source_archive IS NULL
             ORDER BY size_bytes DESC",
        )?;
        let results = stmt
            .query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get::<_, i64>(3)? as u64))
            })?
            .filter_map(|r| r.ok())
            .collect();
        Ok(results)
    }

    /// Removes a canonical file from the DB by id (and its duplicates via cascade).
    pub fn remove_file(&self, id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM files WHERE id = ?1", rusqlite::params![id])?;
        Ok(())
    }

    /// Removes all macOS junk entries (__MACOSX/, ._*, .DS_Store)
    /// that may have been indexed before the filter existed.
    /// Returns the number of entries removed.
    pub fn purge_macos_junk(&self) -> Result<usize> {
        // Duplicates with macOS junk path
        let dup_del = self.conn.execute(
            "DELETE FROM duplicates
             WHERE duplicate_path LIKE '%::__MACOSX/%'
                OR duplicate_path LIKE '%::.__%'
                OR duplicate_path LIKE '%::.DS_Store'",
            [],
        )?;

        // Canonical files with macOS junk path (CASCADE removes their duplicates and meta)
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

    /// Returns all files that are thumbnail candidates (images, videos and 3D),
    /// including those inside archives.
    pub fn files_for_thumbs(
        &self,
        media_type: Option<&str>,
    ) -> Result<Vec<(String, String, String, String)>> {
        let type_filter = match media_type {
            Some(t) => format!("media_type = '{t}'"),
            None => "media_type IN ('image', 'video', '3d')".to_string(),
        };

        let sql = format!(
            "SELECT blake3_hash, current_path, media_type, extension
             FROM files
             WHERE {type_filter}
             ORDER BY media_type, size_bytes DESC"
        );

        let mut stmt = self.conn.prepare(&sql)?;
        let results = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    // ── Queries ───────────────────────────────────────────────────────────

    pub fn stats(&self) -> Result<DbStats> {
        let total: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))?;
        let dupes: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM duplicates", [], |r| r.get(0))?;
        let bytes: i64 =
            self.conn
                .query_row("SELECT COALESCE(SUM(size_bytes),0) FROM files", [], |r| {
                    r.get(0)
                })?;
        let bytes_dup: i64 = self.conn.query_row(
            "SELECT COALESCE(SUM(f.size_bytes),0)
             FROM duplicates d JOIN files f ON f.id = d.canonical_id",
            [],
            |r| r.get(0),
        )?;

        let by_type: Vec<(String, i64, i64)> = {
            let mut stmt = self.conn.prepare(
                "SELECT media_type, COUNT(*), COALESCE(SUM(size_bytes),0)
                 FROM files GROUP BY media_type ORDER BY 2 DESC",
            )?;
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .filter_map(|r| r.ok())
                .collect()
        };

        Ok(DbStats {
            total,
            dupes,
            bytes,
            bytes_dup,
            by_type,
        })
    }

    pub fn duplicates(&self) -> Result<Vec<DuplicateGroup>> {
        let mut stmt = self.conn.prepare(
            "
            SELECT f.blake3_hash, f.original_name, f.current_path,
                   f.size_bytes, f.media_type, d.duplicate_path
            FROM duplicates d
            JOIN files f ON f.id = d.canonical_id
            ORDER BY f.media_type, f.size_bytes DESC
        ",
        )?;

        let mut groups: std::collections::HashMap<String, DuplicateGroup> =
            std::collections::HashMap::new();

        stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
            ))
        })?
        .filter_map(|r| r.ok())
        .for_each(|(hash, name, path, size, mtype, dupe)| {
            let entry = groups.entry(hash.clone()).or_insert(DuplicateGroup {
                hash,
                canonical_name: name,
                canonical_path: path,
                size_bytes: size as u64,
                media_type: mtype,
                duplicates: vec![],
            });
            entry.duplicates.push(dupe);
        });

        let mut list: Vec<_> = groups.into_values().collect();
        list.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes));
        Ok(list)
    }

    pub fn search(&self, query: &str, media_type: Option<&str>) -> Result<Vec<SearchResult>> {
        let pattern = format!("%{query}%");
        let type_filter = media_type
            .map(|t| format!("AND f.media_type = '{t}'"))
            .unwrap_or_default();

        let sql = format!(
            "
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
        "
        );

        let mut stmt = self.conn.prepare(&sql)?;
        let results = stmt
            .query_map(params![pattern], |r| {
                let media_type_str: String = r.get(3)?;
                let media_type = MediaType::from_str(&media_type_str);

                let detail = match media_type {
                    MediaType::Audio => SearchDetail::Audio {
                        duration: r.get(6)?,
                        artist: r.get(7)?,
                        title: r.get(8)?,
                        album: r.get(9)?,
                    },
                    MediaType::Video => SearchDetail::Video {
                        duration: r.get(10)?,
                        width: r.get(11)?,
                        height: r.get(12)?,
                        title: r.get(13)?,
                    },
                    MediaType::Image => SearchDetail::Image {
                        width: r.get(14)?,
                        height: r.get(15)?,
                        camera: r.get(16)?,
                    },
                    MediaType::Print3D => SearchDetail::Print3D {
                        triangles: r.get::<_, Option<i64>>(17)?.map(|v| v as u64),
                    },
                    MediaType::Other => SearchDetail::Other,
                };

                Ok(SearchResult {
                    name: r.get(1)?,
                    path: r.get(2)?,
                    media_type: media_type_str,
                    size_bytes: r.get::<_, i64>(4)? as u64,
                    extension: r.get(5)?,
                    detail,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    // ── Perceptual similarity ─────────────────────────────────────────────

    /// Groups images by Hamming distance of their phash.
    /// threshold: 0 = identical, ≤10 = very similar, ≤20 = similar.
    pub fn similar_images(&self, threshold: u32) -> Result<Vec<crate::models::SimilarImageGroup>> {
        use crate::models::{SimilarImageEntry, SimilarImageGroup};

        let rows: Vec<(String, String, i64, i64, String)> = {
            let mut stmt = self.conn.prepare(
                "SELECT f.current_path, f.original_name,
                        COALESCE(i.width,0), COALESCE(i.height,0), i.phash
                 FROM files f
                 JOIN meta_image i ON i.file_id = f.id
                 WHERE i.phash IS NOT NULL
                 ORDER BY f.size_bytes DESC",
            )?;
            stmt.query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .filter_map(|r| r.ok())
            .collect()
        };

        let n = rows.len();
        if n < 2 {
            return Ok(vec![]);
        }

        // Union-Find to group by similarity
        let mut parent: Vec<usize> = (0..n).collect();
        for i in 0..n {
            for j in (i + 1)..n {
                if let Some(dist) = phash_distance(&rows[i].4, &rows[j].4) {
                    if dist <= threshold {
                        let ri = uf_find(&mut parent, i);
                        let rj = uf_find(&mut parent, j);
                        if ri != rj {
                            parent[ri] = rj;
                        }
                    }
                }
            }
        }

        let mut groups: std::collections::HashMap<usize, Vec<usize>> = Default::default();
        for i in 0..n {
            groups.entry(uf_find(&mut parent, i)).or_default().push(i);
        }

        Ok(groups
            .into_values()
            .filter(|v| v.len() >= 2)
            .map(|idx| SimilarImageGroup {
                files: idx
                    .into_iter()
                    .map(|i| SimilarImageEntry {
                        path: rows[i].0.clone(),
                        name: rows[i].1.clone(),
                        width: if rows[i].2 > 0 {
                            Some(rows[i].2 as u32)
                        } else {
                            None
                        },
                        height: if rows[i].3 > 0 {
                            Some(rows[i].3 as u32)
                        } else {
                            None
                        },
                        phash: rows[i].4.clone(),
                    })
                    .collect(),
            })
            .collect())
    }

    /// Groups songs with the same title + artist (normalized to lowercase).
    pub fn similar_audio(&self) -> Result<Vec<crate::models::SimilarAudioGroup>> {
        use crate::models::{SimilarAudioEntry, SimilarAudioGroup};

        let rows: Vec<(String, String, String, String, Option<f64>, Option<String>)> = {
            let mut stmt = self.conn.prepare(
                "SELECT f.current_path, f.original_name,
                        LOWER(TRIM(COALESCE(a.title,''))),
                        LOWER(TRIM(COALESCE(a.artist,''))),
                        a.duration_secs, a.album
                 FROM files f JOIN meta_audio a ON a.file_id = f.id
                 WHERE a.title  IS NOT NULL AND TRIM(a.title)  != ''
                   AND a.artist IS NOT NULL AND TRIM(a.artist) != ''
                 ORDER BY LOWER(a.artist), LOWER(a.title)",
            )?;
            stmt.query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            })?
            .filter_map(|r| r.ok())
            .collect()
        };

        let mut map: std::collections::HashMap<
            (String, String),
            (String, String, Vec<SimilarAudioEntry>),
        > = Default::default();

        for (path, name, title, artist, dur, album) in rows {
            let e = map
                .entry((title.clone(), artist.clone()))
                .or_insert_with(|| (title, artist, vec![]));
            e.2.push(SimilarAudioEntry {
                path,
                name,
                duration_secs: dur,
                album,
            });
        }

        Ok(map
            .into_values()
            .filter(|(_, _, files)| files.len() >= 2)
            .map(|(title, artist, files)| SimilarAudioGroup {
                title,
                artist,
                files,
            })
            .collect())
    }
}

// ── Private similarity helpers ────────────────────────────────────────────

fn uf_find(parent: &mut Vec<usize>, mut x: usize) -> usize {
    while parent[x] != x {
        parent[x] = parent[parent[x]]; // path compression
        x = parent[x];
    }
    x
}

/// Hamming distance between two hex-encoded phashes. Returns None if either string is invalid.
fn phash_distance(a: &str, b: &str) -> Option<u32> {
    if a.len() != b.len() || a.len() % 2 != 0 {
        return None;
    }
    let decode = |s: &str| -> Option<Vec<u8>> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
            .collect()
    };
    let ab = decode(a)?;
    let bb = decode(b)?;
    Some(
        ab.iter()
            .zip(bb.iter())
            .map(|(x, y)| (x ^ y).count_ones())
            .sum(),
    )
}

pub struct CachedFile {
    pub blake3_hash: String,
    pub size_bytes: u64,
    pub mtime: Option<u64>,
}

pub struct DbStats {
    pub total: i64,
    pub dupes: i64,
    pub bytes: i64,
    pub bytes_dup: i64,
    pub by_type: Vec<(String, i64, i64)>,
}

pub struct DuplicateGroup {
    pub hash: String,
    pub canonical_name: String,
    pub canonical_path: String,
    pub size_bytes: u64,
    pub media_type: String,
    pub duplicates: Vec<String>,
}

pub struct SearchResult {
    pub name: String,
    pub path: String,
    pub media_type: String,
    pub size_bytes: u64,
    pub extension: String,
    pub detail: SearchDetail,
}

pub enum SearchDetail {
    Audio {
        duration: Option<f64>,
        artist: Option<String>,
        title: Option<String>,
        album: Option<String>,
    },
    Video {
        duration: Option<f64>,
        width: Option<u32>,
        height: Option<u32>,
        title: Option<String>,
    },
    Image {
        width: Option<u32>,
        height: Option<u32>,
        camera: Option<String>,
    },
    Print3D {
        triangles: Option<u64>,
    },
    Other,
}

// ── Internal helpers ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{MediaEntry, MediaType, Metadata};

    fn mem_db() -> Database {
        Database::open(":memory:").unwrap()
    }

    fn entry(hash: &str, path: &str, name: &str) -> MediaEntry {
        MediaEntry {
            blake3_hash: hash.to_string(),
            size_bytes: 1_000,
            original_name: name.to_string(),
            current_path: path.to_string(),
            extension: "jpg".to_string(),
            media_type: MediaType::Image,
            metadata: Metadata::None,
            source_archive: None,
            path_in_archive: None,
            mtime: None,
            from_cache: false,
        }
    }

    // ── copy_score ────────────────────────────────────────────────────────

    #[test]
    fn copy_score_original_low() {
        let s = copy_score("/fotos/vacaciones.jpg");
        assert!(s < 5_000, "score={s}");
    }

    #[test]
    fn copy_score_copy_spanish() {
        assert!(copy_score("/fotos/foto - copia.jpg") >= 10_000);
        assert!(copy_score("/fotos/foto - copia (2).jpg") >= 10_000);
    }

    #[test]
    fn copy_score_copy_english() {
        assert!(copy_score("/fotos/photo - Copy.jpg") >= 10_000);
    }

    #[test]
    fn copy_score_numeric_suffix() {
        // "file_1", "file_2" → numeric suffix preceded by '_'
        assert!(copy_score("/fotos/imagen_2.jpg") >= 5_000);
        assert!(copy_score("/fotos/imagen_10.jpg") >= 5_000);
    }

    #[test]
    fn copy_score_backup() {
        assert!(copy_score("/fotos/foto_backup.jpg") >= 6_000);
        assert!(copy_score("/fotos/foto_bak.jpg") >= 6_000);
    }

    #[test]
    fn copy_score_original_beats_copy() {
        let orig = copy_score("/fotos/vacaciones.jpg");
        let copy = copy_score("/fotos/vacaciones - copia.jpg");
        assert!(orig < copy);
    }

    // ── insert: new file ──────────────────────────────────────────────────

    #[test]
    fn insert_new_is_not_duplicate() {
        let db = mem_db();
        let (_, is_dup, canon) = db.insert(&entry("h1", "/a.jpg", "a.jpg")).unwrap();
        assert!(!is_dup);
        assert!(canon.is_none());
    }

    #[test]
    fn insert_same_hash_different_path_is_duplicate() {
        let db = mem_db();
        db.insert(&entry("h1", "/orig/a.jpg", "a.jpg")).unwrap();
        let (_, is_dup, canon) = db.insert(&entry("h1", "/copy/a.jpg", "a.jpg")).unwrap();
        assert!(is_dup);
        assert_eq!(canon.unwrap(), "/orig/a.jpg");
    }

    #[test]
    fn insert_rescan_same_path_not_duplicate() {
        let db = mem_db();
        db.insert(&entry("h1", "/a.jpg", "a.jpg")).unwrap();
        let (_, is_dup, _) = db.insert(&entry("h1", "/a.jpg", "a.jpg")).unwrap();
        assert!(!is_dup);
    }

    #[test]
    fn insert_promotes_more_original_to_canonical() {
        let db = mem_db();
        // The copy is indexed first
        db.insert(&entry("h1", "/fotos/foto - copia.jpg", "foto - copia.jpg"))
            .unwrap();
        // Then the original — should be promoted to canonical
        let (_, is_dup, _) = db
            .insert(&entry("h1", "/fotos/foto.jpg", "foto.jpg"))
            .unwrap();
        assert!(!is_dup);
        let groups = db.duplicates().unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].canonical_path, "/fotos/foto.jpg");
        assert!(
            groups[0]
                .duplicates
                .contains(&"/fotos/foto - copia.jpg".to_string())
        );
    }

    // ── stats ─────────────────────────────────────────────────────────────

    #[test]
    fn stats_empty_db() {
        let s = mem_db().stats().unwrap();
        assert_eq!(s.total, 0);
        assert_eq!(s.dupes, 0);
        assert_eq!(s.bytes, 0);
    }

    #[test]
    fn stats_with_duplicate() {
        let db = mem_db();
        db.insert(&entry("h1", "/a.jpg", "a.jpg")).unwrap();
        db.insert(&entry("h2", "/b.jpg", "b.jpg")).unwrap();
        db.insert(&entry("h1", "/c.jpg", "c.jpg")).unwrap(); // dup of h1
        let s = db.stats().unwrap();
        assert_eq!(s.total, 2);
        assert_eq!(s.dupes, 1);
    }

    // ── search ────────────────────────────────────────────────────────────

    #[test]
    fn search_finds_by_partial_name() {
        let db = mem_db();
        db.insert(&entry("h1", "/fotos/vacaciones.jpg", "vacaciones.jpg"))
            .unwrap();
        let r = db.search("vacacion", None).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].name, "vacaciones.jpg");
    }

    #[test]
    fn search_no_results() {
        let db = mem_db();
        db.insert(&entry("h1", "/fotos/foto.jpg", "foto.jpg"))
            .unwrap();
        assert!(db.search("xyznotfound", None).unwrap().is_empty());
    }

    #[test]
    fn search_filter_by_type() {
        let db = mem_db();
        db.insert(&entry("h1", "/fotos/foto.jpg", "foto.jpg"))
            .unwrap();
        assert!(db.search("foto", Some("video")).unwrap().is_empty());
        assert_eq!(db.search("foto", Some("image")).unwrap().len(), 1);
    }

    #[test]
    fn search_case_insensitive() {
        let db = mem_db();
        db.insert(&entry("h1", "/Foto.jpg", "Foto.jpg")).unwrap();
        assert_eq!(db.search("foto", None).unwrap().len(), 1);
        assert_eq!(db.search("FOTO", None).unwrap().len(), 1);
    }

    // ── duplicates ────────────────────────────────────────────────────────

    #[test]
    fn duplicates_empty() {
        assert!(mem_db().duplicates().unwrap().is_empty());
    }

    #[test]
    fn duplicates_groups_correctly() {
        let db = mem_db();
        db.insert(&entry("h1", "/a.jpg", "a.jpg")).unwrap();
        db.insert(&entry("h1", "/b.jpg", "b.jpg")).unwrap();
        db.insert(&entry("h1", "/c.jpg", "c.jpg")).unwrap();
        let groups = db.duplicates().unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].duplicates.len(), 2);
    }

    #[test]
    fn duplicates_two_independent_groups() {
        let db = mem_db();
        db.insert(&entry("h1", "/a1.jpg", "a1.jpg")).unwrap();
        db.insert(&entry("h1", "/a2.jpg", "a2.jpg")).unwrap();
        db.insert(&entry("h2", "/b1.jpg", "b1.jpg")).unwrap();
        db.insert(&entry("h2", "/b2.jpg", "b2.jpg")).unwrap();
        assert_eq!(db.duplicates().unwrap().len(), 2);
    }

    // ── cleanup_stale ─────────────────────────────────────────────────────

    #[test]
    fn cleanup_stale_removes_nonexistent_path() {
        let db = mem_db();
        db.insert(&entry("h1", "/ruta/que/no/existe.jpg", "inexistente.jpg"))
            .unwrap();
        let (removed, _) = db.cleanup_stale().unwrap();
        assert_eq!(removed, 1);
        assert_eq!(db.stats().unwrap().total, 0);
    }

    #[test]
    fn cleanup_stale_keeps_existing_file() {
        let db = mem_db();
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_str().unwrap().to_string();
        db.insert(&entry("h1", &path, "tmp.jpg")).unwrap();
        let (removed, _) = db.cleanup_stale().unwrap();
        assert_eq!(removed, 0);
        assert_eq!(db.stats().unwrap().total, 1);
    }

    #[test]
    fn cleanup_stale_removes_orphan_duplicates() {
        let db = mem_db();
        // Canonical that actually exists
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_str().unwrap().to_string();
        db.insert(&entry("h1", &path, "real.jpg")).unwrap();
        // Duplicate with nonexistent path — cleanup_stale must remove it
        db.insert(&entry("h1", "/no/existe/copia.jpg", "copia.jpg"))
            .unwrap();
        let (_, dupes_removed) = db.cleanup_stale().unwrap();
        assert_eq!(dupes_removed, 1);
        assert_eq!(db.duplicates().unwrap().len(), 0);
    }

    // ── purge_macos_junk ─────────────────────────────────────────────────

    #[test]
    fn purge_removes_macosx_in_path() {
        let db = mem_db();
        db.insert(&entry("h1", "/arc.zip::__MACOSX/file.jpg", "file.jpg"))
            .unwrap();
        db.insert(&entry("h2", "/normal.jpg", "normal.jpg"))
            .unwrap();
        let removed = db.purge_macos_junk().unwrap();
        assert!(removed >= 1);
        assert_eq!(db.stats().unwrap().total, 1); // only the normal file remains
    }

    #[test]
    fn purge_removes_dot_underscore() {
        let db = mem_db();
        db.insert(&entry("h1", "/arc.zip::._hidden", "._hidden"))
            .unwrap();
        let removed = db.purge_macos_junk().unwrap();
        assert!(removed >= 1);
    }

    #[test]
    fn purge_removes_ds_store() {
        let db = mem_db();
        db.insert(&entry("h1", "/arc.zip::.DS_Store", ".DS_Store"))
            .unwrap();
        let removed = db.purge_macos_junk().unwrap();
        assert!(removed >= 1);
    }

    #[test]
    fn purge_via_source_archive_and_path_in_archive() {
        let db = mem_db();
        let mut e = entry("h1", "/arc.zip::__MACOSX/._icon", "._icon");
        e.source_archive = Some("/arc.zip".to_string());
        e.path_in_archive = Some("__MACOSX/._icon".to_string());
        db.insert(&e).unwrap();
        assert!(db.purge_macos_junk().unwrap() >= 1);
        assert_eq!(db.stats().unwrap().total, 0);
    }

    #[test]
    fn purge_does_not_touch_normal_files() {
        let db = mem_db();
        db.insert(&entry("h1", "/fotos/foto.jpg", "foto.jpg"))
            .unwrap();
        db.insert(&entry("h2", "/arc.zip::real.jpg", "real.jpg"))
            .unwrap();
        let removed = db.purge_macos_junk().unwrap();
        assert_eq!(removed, 0);
        assert_eq!(db.stats().unwrap().total, 2);
    }

    // ── files_for_verify ─────────────────────────────────────────────────

    #[test]
    fn files_for_verify_excludes_archive_entries() {
        let db = mem_db();
        db.insert(&entry("h1", "/archivo.jpg", "archivo.jpg"))
            .unwrap();
        let mut arc = entry("h2", "/arc.zip::inner.jpg", "inner.jpg");
        arc.source_archive = Some("/arc.zip".to_string());
        arc.path_in_archive = Some("inner.jpg".to_string());
        db.insert(&arc).unwrap();
        let files = db.files_for_verify().unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].2, "/archivo.jpg");
    }

    #[test]
    fn files_for_verify_includes_hash_and_size() {
        let db = mem_db();
        db.insert(&entry("deadbeef", "/x.jpg", "x.jpg")).unwrap();
        let files = db.files_for_verify().unwrap();
        assert_eq!(files[0].1, "deadbeef");
        assert_eq!(files[0].3, 1_000);
    }

    // ── remove_file ───────────────────────────────────────────────────────

    #[test]
    fn remove_file_removes_canonical_without_duplicates() {
        // remove_file is designed to be used on files with no active duplicates
        // (canonical_id in duplicates has no ON DELETE CASCADE;
        //  cleanup_stale deletes orphan duplicates before deleting the canonical)
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
    fn remove_file_removes_meta_in_cascade() {
        // Metadata tables do have ON DELETE CASCADE
        let db = mem_db();
        let mut e = entry("h1", "/cancion.mp3", "cancion.mp3");
        e.extension = "mp3".to_string();
        e.media_type = MediaType::Audio;
        e.metadata = Metadata::Audio(crate::models::MetaAudio {
            duration_secs: Some(180.0),
            artist: Some("Artista".to_string()),
            ..Default::default()
        });
        db.insert(&e).unwrap();
        let id = db.files_for_verify().unwrap()[0].0;
        db.remove_file(id).unwrap();
        assert_eq!(db.stats().unwrap().total, 0);
    }
}

/// Returns a "how much does this look like a copy" score based on the filename.
/// Lower score = more original. Used to decide which path becomes the canonical.
///
/// Detected patterns (Windows/macOS/Linux in Spanish and English):
///   " - copia", " - copia (2)", "- Copy", " (1)", "_copy", "backup", etc.
///
/// Tiebreaker: when two paths have the same copy score, the one with the
/// longer name (more descriptive) is preferred. E.g. "hellboy.rar::film" > "h.rar::film".
fn copy_score(path: &str) -> u32 {
    let name = std::path::Path::new(path)
        .file_stem()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default();

    let mut score = 0u32;

    // Windows Spanish: "archivo - copia", "archivo - copia (2)"
    if name.contains(" - copia") {
        score += 10_000;
    }

    // Windows English: "file - Copy", "file - Copy (2)"
    if name.contains(" - copy") {
        score += 10_000;
    }

    // macOS / Linux: "file (1)", "file (2)", ...
    if name.ends_with(')') {
        let re = name.trim_end_matches(|c: char| c.is_ascii_digit() || c == ' ' || c == '(');
        let suffix = &name[re.len()..];
        if suffix.trim().starts_with('(') {
            score += 8_000;
        }
    }

    // Numeric suffixes: "file_1", "file_2", "file 1", "file 2"
    if name
        .chars()
        .last()
        .map(|c| c.is_ascii_digit())
        .unwrap_or(false)
    {
        let trimmed = name.trim_end_matches(|c: char| c.is_ascii_digit());
        if trimmed.ends_with('_') || trimmed.ends_with(' ') {
            score += 5_000;
        }
    }

    // Generic keywords in the name
    for keyword in &[
        "_copy", "_backup", "_bak", " backup", " bak", "copy_of", "copia_de",
    ] {
        if name.contains(keyword) {
            score += 6_000;
        }
    }

    // Tiebreaker: penalize short names. Longer names are more descriptive
    // and probably more original (e.g. "hellboy" > "h", "document" > "doc1").
    // The penalty is small (max 255) so it cannot override any copy pattern.
    let name_len = name.chars().count().min(255) as u32;
    score += 255u32.saturating_sub(name_len);

    score
}
