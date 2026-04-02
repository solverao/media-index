use eframe::egui;
use humansize::{DECIMAL, format_size};
use std::path::PathBuf;
use std::sync::{mpsc, Arc};

use super::TaskResult;
use crate::models::{GuiProgress, ScanStats};

pub struct ScannerState {
    pub is_running: bool,
    pub last_stats: Option<ScanStats>,
    pub log: Vec<String>,
    /// Live progress shared with the scanner thread (None when idle).
    pub progress: Option<Arc<GuiProgress>>,
    // Form fields
    scan_path: String,
    verbose: bool,
    no_archives: bool,
    skip_small: bool,
    /// Active exclusion patterns (directory/filename substrings to skip).
    pub exclude_patterns: Vec<String>,
    /// Buffer for the "add pattern" text input.
    exclude_input: String,
}

impl Default for ScannerState {
    fn default() -> Self {
        Self {
            is_running: false,
            last_stats: None,
            log: vec![],
            progress: None,
            scan_path: String::new(),
            verbose: false,
            no_archives: false,
            skip_small: true,
            exclude_patterns: vec![],
            exclude_input: String::new(),
        }
    }
}

pub fn show(
    ui: &mut egui::Ui,
    state: &mut ScannerState,
    ctx: &egui::Context,
    db_path: &str,
    tx: &mpsc::Sender<TaskResult>,
) {
    ui.heading("🔍 Scanner");
    ui.separator();

    // ── Directory picker ──────────────────────────────────────────────────
    ui.group(|ui| {
        ui.label(egui::RichText::new("Directorio a escanear").strong());
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut state.scan_path)
                    .hint_text("/ruta/a/tu/colección")
                    .desired_width(400.0),
            );
            // Native folder dialog (optional — user can also type the path)
            let _ = ui
                .small_button("📂 Pegar ruta")
                .on_hover_text("Escribe la ruta manualmente");
        });

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.checkbox(&mut state.verbose, "Verbose");
            ui.checkbox(&mut state.no_archives, "Ignorar archivos comprimidos");
            ui.checkbox(&mut state.skip_small, "Ignorar archivos < 1 KB")
                .on_hover_text("Omite archivos de sistema, temporales y vacíos (acelera el escaneo)");
        });

        ui.add_space(6.0);
        ui.label(egui::RichText::new("Excluir directorios / patrones").strong());
        ui.add_space(2.0);

        // Quick-add buttons for common patterns
        ui.horizontal_wrapped(|ui: &mut egui::Ui| {
            for pat in [".git", "node_modules", "__pycache__", ".cache", "Thumbs.db", ".DS_Store", "tmp", "temp"] {
                let already = state.exclude_patterns.iter().any(|p| p == pat);
                if already {
                    ui.add_enabled(
                        false,
                        egui::Button::new(format!("✓ {pat}")).small(),
                    );
                } else if ui.small_button(format!("+ {pat}")).clicked() {
                    state.exclude_patterns.push(pat.to_string());
                }
            }
        });

        ui.add_space(2.0);
        ui.horizontal(|ui: &mut egui::Ui| {
            ui.add(
                egui::TextEdit::singleline(&mut state.exclude_input)
                    .hint_text("Patrón personalizado…")
                    .desired_width(220.0),
            );
            let can_add = !state.exclude_input.trim().is_empty()
                && !state.exclude_patterns.contains(&state.exclude_input.trim().to_string());
            if ui.add_enabled(can_add, egui::Button::new("Añadir")).clicked()
                || (ui.input(|i| i.key_pressed(egui::Key::Enter)) && can_add)
            {
                state.exclude_patterns.push(state.exclude_input.trim().to_string());
                state.exclude_input.clear();
            }
        });

        // Active patterns — show as chips with × button
        if !state.exclude_patterns.is_empty() {
            ui.add_space(2.0);
            let mut to_remove: Option<usize> = None;
            ui.horizontal_wrapped(|ui: &mut egui::Ui| {
                for (i, pat) in state.exclude_patterns.iter().enumerate() {
                    egui::Frame::default()
                        .fill(egui::Color32::from_rgb(60, 40, 40))
                        .rounding(egui::Rounding::same(4.0))
                        .inner_margin(egui::Margin::symmetric(6.0, 2.0))
                        .show(ui, |ui: &mut egui::Ui| {
                            ui.horizontal(|ui: &mut egui::Ui| {
                                ui.label(
                                    egui::RichText::new(pat)
                                        .color(egui::Color32::from_rgb(255, 160, 120)),
                                );
                                if ui
                                    .add(
                                        egui::Button::new(
                                            egui::RichText::new("×").color(egui::Color32::GRAY),
                                        )
                                        .frame(false)
                                        .small(),
                                    )
                                    .clicked()
                                {
                                    to_remove = Some(i);
                                }
                            });
                        });
                }
            });
            if let Some(i) = to_remove {
                state.exclude_patterns.remove(i);
            }
        }
    });

    ui.add_space(8.0);

    // ── Action buttons ────────────────────────────────────────────────────
    ui.horizontal(|ui| {
        let can_act = !state.is_running && !state.scan_path.is_empty();

        ui.add_enabled_ui(can_act, |ui| {
            if ui.button("▶ Escanear").clicked() {
                start_scan(state, ctx.clone(), db_path, tx);
            }
        });

        if state.is_running {
            ui.spinner();
        }

        if ui.button("🗑 Limpiar log").clicked() {
            state.log.clear();
            state.last_stats = None;
        }
    });

    // ── Live progress bar ─────────────────────────────────────────────────
    if state.is_running {
        if let Some(ref prog) = state.progress {
            let (done, total, current) = prog.get();
            ui.add_space(6.0);
            if total > 0 {
                let fraction = done as f32 / total as f32;
                ui.add(
                    egui::ProgressBar::new(fraction)
                        .text(format!("{done} / {total}"))
                        .animate(true),
                );
            } else {
                ui.add(egui::ProgressBar::new(0.0).text("Recopilando archivos…").animate(true));
            }
            if !current.is_empty() {
                ui.label(
                    egui::RichText::new(format!("↳ {current}"))
                        .weak()
                        .size(11.0),
                );
            }
        }
    }

    ui.add_space(8.0);

    // ── Last scan result ──────────────────────────────────────────────────
    if let Some(s) = &state.last_stats {
        ui.separator();
        ui.label(egui::RichText::new("Resultado del último escaneo").strong());
        ui.add_space(4.0);
        egui::Grid::new("scan_stats")
            .num_columns(2)
            .spacing([20.0, 4.0])
            .show(ui, |ui| {
                stat_row(
                    ui,
                    "3D",
                    s.indexed_3d,
                    egui::Color32::from_rgb(80, 210, 210),
                );
                stat_row(
                    ui,
                    "Video",
                    s.indexed_video,
                    egui::Color32::from_rgb(80, 140, 255),
                );
                stat_row(
                    ui,
                    "Audio",
                    s.indexed_audio,
                    egui::Color32::from_rgb(200, 90, 255),
                );
                stat_row(
                    ui,
                    "Imágenes",
                    s.indexed_image,
                    egui::Color32::from_rgb(255, 210, 50),
                );
                stat_row(ui, "Otros", s.indexed_other, egui::Color32::GRAY);
                ui.separator();
                ui.end_row();
                stat_row(ui, "Archivados", s.archives_opened, egui::Color32::GRAY);
                stat_row(
                    ui,
                    "Duplicados encontrados",
                    s.duplicates,
                    egui::Color32::from_rgb(255, 90, 90),
                );
                ui.label("Espacio duplicado:");
                ui.label(format_size(s.bytes_dup, DECIMAL));
                ui.end_row();
                if s.skipped_cached > 0 {
                    stat_row(
                        ui,
                        "Omitidos (caché)",
                        s.skipped_cached,
                        egui::Color32::GRAY,
                    );
                }
                if s.errors > 0 {
                    stat_row(
                        ui,
                        "Errores",
                        s.errors,
                        egui::Color32::from_rgb(255, 90, 90),
                    );
                }
                ui.separator();
                ui.end_row();
                stat_row(
                    ui,
                    "Total indexado",
                    s.total_indexed(),
                    egui::Color32::from_rgb(90, 160, 255),
                );
            });
    }

    // ── Log output ────────────────────────────────────────────────────────
    if !state.log.is_empty() {
        ui.add_space(8.0);
        ui.separator();
        ui.label(egui::RichText::new("Log").strong());
        egui::ScrollArea::vertical()
            .max_height(180.0)
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for line in &state.log {
                    let color = if line.starts_with("Error") {
                        egui::Color32::from_rgb(255, 90, 90)
                    } else {
                        ui.visuals().text_color()
                    };
                    ui.colored_label(color, line);
                }
            });
    }
}

fn stat_row(ui: &mut egui::Ui, label: &str, value: usize, color: egui::Color32) {
    ui.label(format!("{label}:"));
    ui.colored_label(color, value.to_string());
    ui.end_row();
}

fn start_scan(
    state: &mut ScannerState,
    ctx: egui::Context,
    db_path: &str,
    tx: &mpsc::Sender<TaskResult>,
) {
    let path = PathBuf::from(&state.scan_path);
    if !path.exists() {
        state.log.push(format!(
            "Error: el directorio '{}' no existe.",
            state.scan_path
        ));
        return;
    }

    state.is_running = true;
    state
        .log
        .push(format!("Iniciando escaneo de {}...", state.scan_path));

    let progress = GuiProgress::new();
    state.progress = Some(Arc::clone(&progress));

    let db_path = db_path.to_string();
    let verbose = state.verbose;
    let no_archives = state.no_archives;
    let skip_small = state.skip_small;
    let exclude_patterns = state.exclude_patterns.clone();
    let tx = tx.clone();

    std::thread::spawn(move || {
        let result = (|| -> anyhow::Result<ScanStats> {
            let db = crate::db::Database::open(&db_path)?;
            let mut scanner = crate::scanner::Scanner::new(db, verbose, no_archives);
            scanner.skip_small = skip_small;
            scanner.exclude_patterns = exclude_patterns;
            scanner.gui_progress = Some(progress);
            scanner.scan(&path)
        })();

        let msg = match result {
            Ok(s) => TaskResult::ScanComplete(s),
            Err(e) => TaskResult::Error(e.to_string()),
        };
        let _ = tx.send(msg);
        ctx.request_repaint();
    });
}
