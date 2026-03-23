use eframe::egui;
use std::path::Path;
use std::sync::mpsc;

use super::{TaskResult, VerifyEntry, VerifyStatus};

#[derive(Default)]
pub struct MaintenanceState {
    pub verify_results: Vec<VerifyEntry>,
    pub thumbs_result: Option<(usize, usize, usize)>, // ok, skipped, errors
    pub clean_result: Option<usize>,
    // Thumbs options
    thumb_size: u32,
    thumb_quality: u8,
    thumb_force: bool,
    thumb_type: ThumbType,
    // Active sub-panel
    panel: MainPanel,
    // Verify prune option
    verify_prune: bool,
}

impl MaintenanceState {
    pub fn initialized() -> Self {
        Self {
            thumb_size: 256,
            thumb_quality: 85,
            ..Default::default()
        }
    }
}

#[derive(Default, PartialEq, Clone, Copy)]
enum MainPanel {
    #[default]
    Verify,
    Thumbs,
    Clean,
    Doctor,
    Empty,
}

#[derive(Default, PartialEq, Clone, Copy)]
enum ThumbType {
    #[default]
    All,
    Image,
    Video,
    Td,
}

impl ThumbType {
    fn label(self) -> &'static str {
        match self {
            Self::All => "Todos",
            Self::Image => "Imágenes",
            Self::Video => "Video",
            Self::Td => "3D",
        }
    }
    fn as_db_str(self) -> Option<&'static str> {
        match self {
            Self::All => None,
            Self::Image => Some("image"),
            Self::Video => Some("video"),
            Self::Td => Some("3d"),
        }
    }
}

pub fn show(
    ui: &mut egui::Ui,
    state: &mut MaintenanceState,
    ctx: &egui::Context,
    db_path: &str,
    tx: &mpsc::Sender<TaskResult>,
) {
    // Lazy init
    if state.thumb_size == 0 {
        *state = MaintenanceState::initialized();
    }

    ui.heading("🛠 Mantenimiento");
    ui.separator();

    // ── Tab bar ───────────────────────────────────────────────────────────
    ui.horizontal(|ui| {
        for (panel, icon, label) in [
            (MainPanel::Verify, "✔", "Verificar"),
            (MainPanel::Thumbs, "🖼", "Miniaturas"),
            (MainPanel::Clean, "🧹", "Limpiar BD"),
            (MainPanel::Doctor, "💊", "Doctor"),
            (MainPanel::Empty, "🗑", "Vacíos/Rotos"),
        ] {
            let text = format!("{icon} {label}");
            if ui.selectable_label(state.panel == panel, &text).clicked() {
                state.panel = panel;
            }
        }
    });

    ui.separator();

    match state.panel {
        MainPanel::Verify => panel_verify(ui, state, ctx, db_path, tx),
        MainPanel::Thumbs => panel_thumbs(ui, state, ctx, db_path, tx),
        MainPanel::Clean => panel_clean(ui, state, ctx, db_path, tx),
        MainPanel::Doctor => panel_doctor(ui),
        MainPanel::Empty => panel_empty(ui, db_path),
    }
}

// ── Verify ────────────────────────────────────────────────────────────────

fn panel_verify(
    ui: &mut egui::Ui,
    state: &mut MaintenanceState,
    ctx: &egui::Context,
    db_path: &str,
    tx: &mpsc::Sender<TaskResult>,
) {
    ui.label("Re-verifica los hashes de todos los archivos indexados.");
    ui.horizontal(|ui| {
        ui.checkbox(
            &mut state.verify_prune,
            "Eliminar de BD los archivos faltantes o corruptos",
        );
        if ui.button("▶ Verificar").clicked() {
            run_verify(ctx.clone(), db_path, state.verify_prune, tx);
        }
    });
    ui.add_space(6.0);

    if state.verify_results.is_empty() {
        ui.label("Pulsa Verificar para comprobar la integridad de tu colección.");
        return;
    }

    let ok = state
        .verify_results
        .iter()
        .filter(|e| matches!(e.status, VerifyStatus::Ok))
        .count();
    let missing = state
        .verify_results
        .iter()
        .filter(|e| matches!(e.status, VerifyStatus::Missing))
        .count();
    let corrupt = state
        .verify_results
        .iter()
        .filter(|e| matches!(e.status, VerifyStatus::Corrupted))
        .count();

    ui.horizontal(|ui| {
        ui.colored_label(egui::Color32::from_rgb(80, 200, 80), format!("✓ OK: {ok}"));
        ui.separator();
        ui.colored_label(
            egui::Color32::from_rgb(255, 200, 50),
            format!("⚠ Faltantes: {missing}"),
        );
        ui.separator();
        ui.colored_label(
            egui::Color32::from_rgb(255, 90, 90),
            format!("✗ Corruptos: {corrupt}"),
        );
    });
    ui.separator();

    egui::ScrollArea::vertical().show(ui, |ui| {
        egui_extras::TableBuilder::new(ui)
            .striped(true)
            .resizable(true)
            .column(egui_extras::Column::initial(70.0))
            .column(egui_extras::Column::initial(200.0))
            .column(egui_extras::Column::remainder())
            .header(20.0, |mut h| {
                h.col(|ui| {
                    ui.strong("Estado");
                });
                h.col(|ui| {
                    ui.strong("Nombre");
                });
                h.col(|ui| {
                    ui.strong("Ruta");
                });
            })
            .body(|mut body| {
                for entry in &state.verify_results {
                    body.row(18.0, |mut row| {
                        row.col(|ui| {
                            let (label, color) = match entry.status {
                                VerifyStatus::Ok => ("✓ OK", egui::Color32::from_rgb(80, 200, 80)),
                                VerifyStatus::Missing => {
                                    ("⚠ Faltante", egui::Color32::from_rgb(255, 200, 50))
                                }
                                VerifyStatus::Corrupted => {
                                    ("✗ Corrupto", egui::Color32::from_rgb(255, 90, 90))
                                }
                            };
                            ui.colored_label(color, label);
                        });
                        row.col(|ui| {
                            ui.label(&entry.name);
                        });
                        row.col(|ui| {
                            ui.label(egui::RichText::new(&entry.path).weak().size(11.0));
                        });
                    });
                }
            });
    });
}

fn run_verify(ctx: egui::Context, db_path: &str, prune: bool, tx: &mpsc::Sender<TaskResult>) {
    let db_path = db_path.to_string();
    let tx = tx.clone();
    std::thread::spawn(move || {
        let result: anyhow::Result<Vec<VerifyEntry>> = (|| {
            let db = crate::db::Database::open(&db_path)?;
            let files = db.files_for_verify()?;
            let mut entries = Vec::new();

            for (id, stored_hash, path, _size) in files {
                let p = Path::new(&path);
                if !p.exists() {
                    if prune {
                        let _ = db.remove_file(id);
                    }
                    entries.push(VerifyEntry {
                        name: p
                            .file_name()
                            .map(|n| n.to_string_lossy().into())
                            .unwrap_or_default(),
                        path: path.clone(),
                        status: VerifyStatus::Missing,
                    });
                    continue;
                }

                // Re-hash the file
                let current_hash = hash_file(p);
                let status = match current_hash {
                    Ok(h) if h == stored_hash => VerifyStatus::Ok,
                    Ok(_) => {
                        if prune {
                            let _ = db.remove_file(id);
                        }
                        VerifyStatus::Corrupted
                    }
                    Err(_) => VerifyStatus::Missing,
                };
                entries.push(VerifyEntry {
                    name: p
                        .file_name()
                        .map(|n| n.to_string_lossy().into())
                        .unwrap_or_default(),
                    path,
                    status,
                });
            }
            Ok(entries)
        })();

        let msg = result
            .map(TaskResult::VerifyResults)
            .unwrap_or_else(|e| TaskResult::Error(e.to_string()));
        let _ = tx.send(msg);
        ctx.request_repaint();
    });
}

/// Minimal BLAKE3 hash matching the scanner logic (full or partial).
fn hash_file(path: &Path) -> anyhow::Result<String> {
    use std::io::Read;
    const THRESHOLD: u64 = 100 * 1024 * 1024;
    const CHUNK: u64 = 4 * 1024 * 1024;

    let meta = std::fs::metadata(path)?;
    let size = meta.len();
    let mut file = std::fs::File::open(path)?;

    if size <= THRESHOLD {
        let mut hasher = blake3::Hasher::new();
        std::io::copy(&mut file, &mut hasher)?;
        Ok(hasher.finalize().to_hex().to_string())
    } else {
        let mut data = Vec::with_capacity((CHUNK * 2) as usize + 8);
        file.take(CHUNK).read_to_end(&mut data)?;
        let tail_start = size.saturating_sub(CHUNK);
        let mut file2 = std::fs::File::open(path)?;
        std::io::Seek::seek(&mut file2, std::io::SeekFrom::Start(tail_start))?;
        file2.read_to_end(&mut data)?;
        data.extend_from_slice(&size.to_le_bytes());
        Ok(blake3::hash(&data).to_hex().to_string())
    }
}

// ── Thumbnails ────────────────────────────────────────────────────────────

fn panel_thumbs(
    ui: &mut egui::Ui,
    state: &mut MaintenanceState,
    ctx: &egui::Context,
    db_path: &str,
    tx: &mpsc::Sender<TaskResult>,
) {
    ui.label("Genera miniaturas JPEG para imágenes, videos y modelos 3D.");
    ui.add_space(6.0);

    egui::Grid::new("thumb_opts")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label("Tipo:");
            ui.horizontal(|ui| {
                for t in [
                    ThumbType::All,
                    ThumbType::Image,
                    ThumbType::Video,
                    ThumbType::Td,
                ] {
                    if ui
                        .selectable_label(state.thumb_type == t, t.label())
                        .clicked()
                    {
                        state.thumb_type = t;
                    }
                }
            });
            ui.end_row();

            ui.label("Tamaño (px):");
            ui.add(egui::Slider::new(&mut state.thumb_size, 64..=512).text("px"));
            ui.end_row();

            ui.label("Calidad JPEG:");
            ui.add(egui::Slider::new(&mut state.thumb_quality, 50..=100).text("%"));
            ui.end_row();

            ui.label("Opciones:");
            ui.checkbox(&mut state.thumb_force, "Regenerar existentes (--force)");
            ui.end_row();
        });

    ui.add_space(6.0);
    if ui.button("▶ Generar miniaturas").clicked() {
        run_thumbs(ctx.clone(), db_path, state, tx);
    }

    if let Some((ok, skipped, errors)) = state.thumbs_result {
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.colored_label(
                egui::Color32::from_rgb(80, 200, 80),
                format!("✓ Generadas: {ok}"),
            );
            ui.separator();
            ui.colored_label(egui::Color32::GRAY, format!("→ Omitidas: {skipped}"));
            if errors > 0 {
                ui.separator();
                ui.colored_label(
                    egui::Color32::from_rgb(255, 90, 90),
                    format!("✗ Errores: {errors}"),
                );
            }
        });
    }
}

fn run_thumbs(
    ctx: egui::Context,
    db_path: &str,
    state: &MaintenanceState,
    tx: &mpsc::Sender<TaskResult>,
) {
    use crate::thumbs::{
        generate_3d, generate_image, generate_video, thumb_dir_for_db, thumb_path,
    };

    let db_path = db_path.to_string();
    let size = state.thumb_size;
    let quality = state.thumb_quality;
    let force = state.thumb_force;
    let type_str = state.thumb_type.as_db_str();
    let tx = tx.clone();

    std::thread::spawn(move || {
        let result: anyhow::Result<(usize, usize, usize)> = (|| {
            let db = crate::db::Database::open(&db_path)?;
            let thumb_dir = thumb_dir_for_db(&db_path);
            let files = db.files_for_thumbs(type_str)?;

            let (mut ok, mut skipped, mut errors) = (0, 0, 0);
            for (hash, path, media_type, ext) in &files {
                let t_path = thumb_path(&thumb_dir, hash);
                if t_path.exists() && !force {
                    skipped += 1;
                    continue;
                }
                let res = match media_type.as_str() {
                    "image" => generate_image(path, hash, &thumb_dir, size, quality),
                    "video" => generate_video(path, hash, &thumb_dir, size, quality),
                    "3d" => match std::fs::read(path) {
                        Ok(data) => generate_3d(&data, ext, hash, &thumb_dir, size, quality),
                        Err(e) => Err(anyhow::anyhow!("{e}")),
                    },
                    _ => {
                        skipped += 1;
                        continue;
                    }
                };
                if res.is_ok() {
                    ok += 1;
                } else {
                    errors += 1;
                }
            }
            Ok((ok, skipped, errors))
        })();

        let msg = result
            .map(|(ok, sk, er)| TaskResult::ThumbsComplete {
                ok,
                skipped: sk,
                errors: er,
            })
            .unwrap_or_else(|e| TaskResult::Error(e.to_string()));
        let _ = tx.send(msg);
        ctx.request_repaint();
    });
}

// ── Clean ─────────────────────────────────────────────────────────────────

fn panel_clean(
    ui: &mut egui::Ui,
    state: &mut MaintenanceState,
    ctx: &egui::Context,
    db_path: &str,
    tx: &mpsc::Sender<TaskResult>,
) {
    ui.label("Elimina entradas no deseadas de la base de datos sin tocar los archivos.");
    ui.add_space(8.0);

    ui.group(|ui| {
        ui.label(egui::RichText::new("Limpieza de entradas obsoletas").strong());
        ui.label("Elimina de la BD los archivos que ya no existen en disco.");
        if ui.button("🧹 Limpiar entradas obsoletas").clicked() {
            run_cleanup_stale(ctx.clone(), db_path, tx);
        }
    });

    ui.add_space(8.0);

    ui.group(|ui| {
        ui.label(egui::RichText::new("Basura macOS").strong());
        ui.label("Elimina entradas __MACOSX/, ._, .DS_Store indexadas por error.");
        if ui.button("🧹 Purgar basura macOS").clicked() {
            run_purge_macos(ctx.clone(), db_path, tx);
        }
    });

    if let Some(n) = state.clean_result {
        ui.add_space(8.0);
        ui.colored_label(
            egui::Color32::from_rgb(80, 200, 80),
            format!("✓ {n} entrada(s) eliminada(s) de la BD."),
        );
    }
}

fn run_cleanup_stale(ctx: egui::Context, db_path: &str, tx: &mpsc::Sender<TaskResult>) {
    let db_path = db_path.to_string();
    let tx = tx.clone();
    std::thread::spawn(move || {
        let result = crate::db::Database::open(&db_path)
            .and_then(|db| db.cleanup_stale().map(|(f, d)| f + d))
            .map(TaskResult::CleanDone)
            .unwrap_or_else(|e| TaskResult::Error(e.to_string()));
        let _ = tx.send(result);
        ctx.request_repaint();
    });
}

fn run_purge_macos(ctx: egui::Context, db_path: &str, tx: &mpsc::Sender<TaskResult>) {
    let db_path = db_path.to_string();
    let tx = tx.clone();
    std::thread::spawn(move || {
        let result = crate::db::Database::open(&db_path)
            .and_then(|db| db.purge_macos_junk())
            .map(TaskResult::CleanDone)
            .unwrap_or_else(|e| TaskResult::Error(e.to_string()));
        let _ = tx.send(result);
        ctx.request_repaint();
    });
}

// ── Doctor ────────────────────────────────────────────────────────────────

fn panel_doctor(ui: &mut egui::Ui) {
    ui.label("Comprueba dependencias opcionales del sistema.");
    ui.add_space(10.0);

    let checks = [
        (
            "ffprobe (metadatos de video)",
            std::process::Command::new("ffprobe")
                .arg("-version")
                .output()
                .is_ok(),
            "sudo apt install ffmpeg  /  brew install ffmpeg",
        ),
        (
            "unrar (archivos .rar)",
            std::process::Command::new("unrar")
                .arg("--help")
                .output()
                .is_ok(),
            "sudo apt install unrar  /  brew install rar",
        ),
        (
            "stl-thumb (miniaturas 3D)",
            crate::thumbs::stl_thumb_available(),
            "https://github.com/unlimitedbacon/stl-thumb/releases",
        ),
    ];

    egui::Grid::new("doctor_grid")
        .num_columns(3)
        .spacing([12.0, 8.0])
        .show(ui, |ui| {
            for (name, ok, hint) in &checks {
                if *ok {
                    ui.colored_label(egui::Color32::from_rgb(80, 200, 80), "✓");
                } else {
                    ui.colored_label(egui::Color32::from_rgb(255, 90, 90), "✗");
                }
                ui.label(*name);
                if !ok {
                    ui.label(
                        egui::RichText::new(format!("Instalar: {hint}"))
                            .weak()
                            .size(11.0),
                    );
                } else {
                    ui.label(egui::RichText::new("Disponible").weak());
                }
                ui.end_row();
            }
        });

    ui.add_space(10.0);
    ui.colored_label(
        egui::Color32::from_rgb(80, 200, 80),
        "✓ ZIP, 7Z, audio, imagen: Rust puro — sin dependencias externas.",
    );
}

// ── Empty / Broken ────────────────────────────────────────────────────────

fn panel_empty(ui: &mut egui::Ui, _db_path: &str) {
    ui.label("Busca archivos vacíos y enlaces simbólicos rotos desde la CLI:");
    ui.add_space(10.0);

    ui.group(|ui| {
        ui.label(egui::RichText::new("Archivos vacíos").strong());
        ui.code("media-index empty /ruta --files-only");
        ui.label("Añade --delete para eliminarlos.");
    });

    ui.add_space(8.0);

    ui.group(|ui| {
        ui.label(egui::RichText::new("Directorios vacíos").strong());
        ui.code("media-index empty /ruta --dirs-only");
    });

    ui.add_space(8.0);

    ui.group(|ui| {
        ui.label(egui::RichText::new("Enlaces rotos").strong());
        ui.code("media-index broken /ruta");
        ui.label("Añade --delete para eliminarlos.");
    });
}
