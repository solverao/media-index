mod browser;
mod dashboard;
mod dupes;
mod maintenance;
mod scanner_view;
mod similar;

/// Opens `path` (file or directory) with the default system application.
/// Tries multiple methods in order until one succeeds.
pub(super) fn open_path(path: &str) {
    // 1. xdg-open (standard freedesktop, works on most desktop Linux)
    if std::process::Command::new("xdg-open").arg(path).spawn().is_ok() {
        return;
    }
    // 2. gio open (GNOME without xdg-open)
    if std::process::Command::new("gio").args(["open", path]).spawn().is_ok() {
        return;
    }
    // 3. D-Bus FileManager1 (works in WSLg and any session with a file manager)
    //    ShowFolders for directories, ShowItems for files (reveals in parent folder)
    // Fix #17: percent-encode the path so spaces and special chars produce valid URIs
    let encoded: String = path
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9'
            | b'-' | b'_' | b'.' | b'~' | b'/' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect();
    let uri = format!("file://{encoded}");
    let method = if std::path::Path::new(path).is_dir() {
        "org.freedesktop.FileManager1.ShowFolders"
    } else {
        "org.freedesktop.FileManager1.ShowItems"
    };
    let _ = std::process::Command::new("dbus-send")
        .args([
            "--session",
            "--dest=org.freedesktop.FileManager1",
            "--type=method_call",
            "/org/freedesktop/FileManager1",
            method,
            &format!("array:string:{uri}"),
            "string:",
        ])
        .spawn();
}

use eframe::egui;
use std::sync::mpsc;

use crate::db::{DbStats, DuplicateGroup, SearchResult};
use crate::models::{ScanStats, SimilarAudioGroup, SimilarImageGroup};

pub use browser::BrowserState;
pub use dashboard::DashboardState;
pub use dupes::DupesState;
pub use maintenance::MaintenanceState;
pub use scanner_view::ScannerState;
pub use similar::SimilarState;

// ── Navigation ────────────────────────────────────────────────────────────

#[derive(PartialEq, Clone, Copy)]
pub enum View {
    Dashboard,
    Scanner,
    Browser,
    Duplicates,
    Similar,
    Thumbnails,
    Maintenance,
}

// ── Background task results ───────────────────────────────────────────────

pub enum TaskResult {
    StatsLoaded(DbStats),
    ScanComplete(ScanStats),
    SearchResults(Vec<SearchResult>),
    DupesLoaded(Vec<DuplicateGroup>),
    #[allow(dead_code)]
    DupesDeleted {
        deleted: usize,
        freed_bytes: u64,
    },
    SimilarImages(Vec<SimilarImageGroup>),
    SimilarAudio(Vec<SimilarAudioGroup>),
    VerifyResults(Vec<VerifyEntry>),
    ThumbsComplete {
        ok: usize,
        skipped: usize,
        errors: usize,
    },
    ThumbsLoaded(Vec<ThumbEntry>),
    CleanDone(usize),
    ArchiveCacheCount(usize),
    ArchiveCacheCleared(usize),
    HistoryLoaded(Vec<crate::db::ScanHistoryEntry>),
    Error(String),
    Info(String),
}

pub struct VerifyEntry {
    pub name: String,
    pub path: String,
    pub status: VerifyStatus,
}

pub struct ThumbEntry {
    pub thumb_path: std::path::PathBuf,
    pub name: String,
    pub path: String,
    pub media_type: String,
}

pub enum VerifyStatus {
    Ok,
    Missing,
    Corrupted,
}

// ── App ───────────────────────────────────────────────────────────────────

/// Filter for the thumbnail gallery view.
#[derive(Default, PartialEq, Clone, Copy)]
enum ThumbFilter {
    #[default]
    All,
    Td,
    Video,
    Image,
}

impl ThumbFilter {
    fn label(self) -> &'static str {
        match self {
            Self::All   => "Todos",
            Self::Td    => "3D",
            Self::Video => "Video",
            Self::Image => "Imagen",
        }
    }

    fn matches(self, media_type: &str) -> bool {
        match self {
            Self::All   => true,
            Self::Td    => media_type == "3d",
            Self::Video => media_type == "video",
            Self::Image => media_type == "image",
        }
    }
}

pub struct MediaIndexApp {
    db_path: String,
    db_path_input: String,

    active_view: View,

    result_tx: mpsc::Sender<TaskResult>,
    result_rx: mpsc::Receiver<TaskResult>,

    pub dashboard: DashboardState,
    pub scan_history: Vec<crate::db::ScanHistoryEntry>,
    pub scanner: ScannerState,
    pub browser: BrowserState,
    pub dupes: DupesState,
    pub similar: SimilarState,
    pub maintenance: MaintenanceState,

    is_loading: bool,
    status_msg: String,
    thumb_entries: Vec<ThumbEntry>,
    thumb_selected: Option<usize>,
    thumb_filter: ThumbFilter,
    thumb_search: String,
}

impl MediaIndexApp {
    fn new(_cc: &eframe::CreationContext<'_>, db_path: String) -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            db_path_input: db_path.clone(),
            db_path,
            active_view: View::Dashboard,
            result_tx: tx,
            result_rx: rx,
            dashboard: DashboardState::default(),
            scan_history: vec![],
            scanner: ScannerState::default(),
            browser: BrowserState::default(),
            dupes: DupesState::default(),
            similar: SimilarState::default(),
            maintenance: MaintenanceState::default(),
            is_loading: false,
            status_msg: String::new(),
            thumb_entries: Vec::new(),
            thumb_selected: None,
            thumb_filter: ThumbFilter::default(),
            thumb_search: String::new(),
        }
    }

    fn spawn<F>(&self, ctx: egui::Context, f: F)
    where
        F: FnOnce() -> TaskResult + Send + 'static,
    {
        let tx = self.result_tx.clone();
        std::thread::spawn(move || {
            let result = f();
            let _ = tx.send(result);
            ctx.request_repaint();
        });
    }

    pub fn load_stats(&mut self, ctx: egui::Context) {
        self.is_loading = true;
        let db_path = self.db_path.clone();
        self.spawn(ctx, move || {
            match crate::db::Database::open(&db_path).and_then(|db| db.stats()) {
                Ok(s) => TaskResult::StatsLoaded(s),
                Err(e) => TaskResult::Error(e.to_string()),
            }
        });
    }

    pub fn load_scan_history(&mut self, ctx: egui::Context) {
        let db_path = self.db_path.clone();
        self.spawn(ctx, move || {
            match crate::db::Database::open(&db_path).and_then(|db| db.get_scan_history(20)) {
                Ok(h) => TaskResult::HistoryLoaded(h),
                Err(_) => TaskResult::HistoryLoaded(vec![]),
            }
        });
    }

    pub fn load_thumbs(&mut self, ctx: egui::Context) {
        self.is_loading = true;
        let db_path = self.db_path.clone();
        let thumb_dir = crate::thumbs::thumb_dir_for_db(&self.db_path);
        self.spawn(ctx, move || {
            // 1. Collect .jpg files from the thumbs directory
            let thumb_paths: Vec<std::path::PathBuf> = std::fs::read_dir(&thumb_dir)
                .into_iter()
                .flat_map(|rd| rd.flatten())
                .filter(|e| e.path().is_dir())
                .flat_map(|e| {
                    std::fs::read_dir(e.path())
                        .into_iter()
                        .flat_map(|rd| rd.flatten())
                        .map(|e| e.path())
                        .filter(|p| p.extension().map(|x| x == "jpg").unwrap_or(false))
                        .collect::<Vec<_>>()
                })
                .collect();

            if thumb_paths.is_empty() {
                return TaskResult::ThumbsLoaded(vec![]);
            }

            // 2. Extract hashes from filenames
            let hashes: Vec<String> = thumb_paths
                .iter()
                .filter_map(|p| p.file_stem()?.to_str().map(|s| s.to_string()))
                .collect();

            // 3. Look up file info in the database
            let db_info = match crate::db::Database::open(&db_path)
                .and_then(|db| db.files_by_hashes(&hashes))
            {
                Ok(info) => info,
                Err(_) => vec![],
            };

            // 4. Build a hash → (name, path, type) map
            let mut info_map: std::collections::HashMap<String, (String, String, String)> =
                db_info
                    .into_iter()
                    .map(|(hash, name, path, mt)| (hash, (name, path, mt)))
                    .collect();

            // 5. Build final entries; fall back to hash as name if not in DB
            let entries = thumb_paths
                .into_iter()
                .map(|thumb_path| {
                    let hash = thumb_path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("")
                        .to_string();
                    let (name, path, media_type) = info_map.remove(&hash).unwrap_or_else(|| {
                        (hash[..8.min(hash.len())].to_string(), String::new(), String::new())
                    });
                    ThumbEntry { thumb_path, name, path, media_type }
                })
                .collect();

            TaskResult::ThumbsLoaded(entries)
        });
    }

    pub fn load_dupes(&mut self, ctx: egui::Context) {
        self.is_loading = true;
        let db_path = self.db_path.clone();
        self.spawn(ctx, move || {
            match crate::db::Database::open(&db_path).and_then(|db| db.duplicates()) {
                Ok(g) => TaskResult::DupesLoaded(g),
                Err(e) => TaskResult::Error(e.to_string()),
            }
        });
    }

    fn process_results(&mut self, ctx: &egui::Context) {
        while let Ok(result) = self.result_rx.try_recv() {
            self.is_loading = false;
            match result {
                TaskResult::StatsLoaded(s) => {
                    self.dashboard.stats = Some(s);
                }
                TaskResult::ScanComplete(s) => {
                    self.scanner.is_running = false;
                    self.scanner.progress = None;
                    self.scanner.last_stats = Some(s);
                    self.scanner.log.push("Scan complete.".into());
                    // Refresh history after each scan
                    self.load_scan_history(ctx.clone());
                }
                TaskResult::SearchResults(r) => {
                    self.browser.results = r;
                }
                TaskResult::DupesLoaded(g) => {
                    self.dupes.groups = g;
                }
                TaskResult::DupesDeleted {
                    deleted,
                    freed_bytes,
                } => {
                    use humansize::{DECIMAL, format_size};
                    self.status_msg = format!(
                        "Deleted {deleted} file(s), freed {}",
                        format_size(freed_bytes, DECIMAL)
                    );
                }
                TaskResult::SimilarImages(g) => {
                    self.similar.image_groups = g;
                }
                TaskResult::SimilarAudio(g) => {
                    self.similar.audio_groups = g;
                }
                TaskResult::VerifyResults(r) => {
                    self.maintenance.verify_results = r;
                }
                TaskResult::ThumbsComplete {
                    ok,
                    skipped,
                    errors,
                } => {
                    self.maintenance.thumb_progress = None;
                    self.maintenance.thumbs_result = Some((ok, skipped, errors));
                }
                TaskResult::ThumbsLoaded(entries) => {
                    self.thumb_entries = entries;
                    self.thumb_selected = None;
                }
                TaskResult::CleanDone(n) => {
                    self.maintenance.clean_result = Some(n);
                }
                TaskResult::ArchiveCacheCount(n) => {
                    self.maintenance.archive_cache_count = Some(n);
                }
                TaskResult::ArchiveCacheCleared(n) => {
                    self.maintenance.archive_cache_count = Some(0);
                    self.maintenance.clean_result = Some(n);
                }
                TaskResult::HistoryLoaded(h) => {
                    self.scan_history = h;
                }
                TaskResult::Error(e) => {
                    self.scanner.is_running = false;
                    self.status_msg = format!("Error: {e}");
                }
                TaskResult::Info(msg) => {
                    self.status_msg = msg;
                }
            }
        }
    }

    fn show_top_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("top_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("🗂 Media Index");
                ui.separator();
                ui.label("Base de datos:");
                let r = ui.add(
                    egui::TextEdit::singleline(&mut self.db_path_input)
                        .hint_text("media.db")
                        .desired_width(260.0),
                );
                if ui.button("Abrir").clicked()
                    || (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                {
                    self.db_path = self.db_path_input.clone();
                    self.status_msg.clear();
                    self.load_stats(ctx.clone());
                    self.load_scan_history(ctx.clone());
                }
                if self.is_loading || self.scanner.is_running {
                    ui.spinner();
                }
                if !self.status_msg.is_empty() {
                    let color = if self.status_msg.starts_with("Error") {
                        egui::Color32::from_rgb(255, 100, 80)
                    } else {
                        egui::Color32::from_rgb(100, 220, 100)
                    };
                    ui.colored_label(color, &self.status_msg);
                }
            });
        });
    }

    fn show_sidebar(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("sidebar")
            .resizable(false)
            .exact_width(148.0)
            .show(ctx, |ui| {
                ui.add_space(8.0);
                let entries: &[(&str, View, &str)] = &[
                    ("Dashboard", View::Dashboard, "📊"),
                    ("Scanner", View::Scanner, "🔍"),
                    ("Explorador", View::Browser, "📁"),
                    ("Duplicados", View::Duplicates, "♊"),
                    ("Similares", View::Similar, "🔮"),
                    ("Miniaturas", View::Thumbnails, "🖼"),
                    ("Mantenimiento", View::Maintenance, "🛠"),
                ];
                for (label, view, icon) in entries {
                    let sel = self.active_view == *view;
                    let text = format!("{icon} {label}");
                    let btn = ui.add_sized([138.0, 32.0], egui::SelectableLabel::new(sel, &text));
                    if btn.clicked() {
                        self.active_view = *view;
                        match view {
                            View::Dashboard => {
                                self.load_stats(ctx.clone());
                                self.load_scan_history(ctx.clone());
                            }
                            View::Duplicates => self.load_dupes(ctx.clone()),
                            View::Thumbnails => self.load_thumbs(ctx.clone()),
                            _ => {}
                        }
                    }
                }
            });
    }

    fn show_thumbnails_view(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let thumb_dir = crate::thumbs::thumb_dir_for_db(&self.db_path);

        // ── Header ────────────────────────────────────────────────────────
        ui.horizontal(|ui| {
            ui.heading("🖼 Miniaturas");
            if ui.button("↺ Recargar").clicked() {
                self.load_thumbs(ctx.clone());
            }
            if ui.button("Generar →").clicked() {
                self.active_view = View::Maintenance;
            }
        });
        ui.separator();

        if !thumb_dir.exists() || self.thumb_entries.is_empty() {
            if self.is_loading {
                ui.spinner();
                ui.label("Cargando miniaturas…");
            } else {
                ui.colored_label(
                    egui::Color32::from_rgb(255, 200, 50),
                    "No hay miniaturas generadas todavía. Usa Mantenimiento → Generar Miniaturas.",
                );
            }
            return;
        }

        // ── Filter bar ────────────────────────────────────────────────────
        ui.horizontal(|ui| {
            ui.label("Tipo:");
            for filter in [
                ThumbFilter::All,
                ThumbFilter::Td,
                ThumbFilter::Video,
                ThumbFilter::Image,
            ] {
                if ui
                    .selectable_label(self.thumb_filter == filter, filter.label())
                    .clicked()
                {
                    if self.thumb_filter != filter {
                        self.thumb_filter = filter;
                        self.thumb_selected = None;
                    }
                }
            }

            ui.separator();
            ui.label("🔎");
            let search_resp = ui.add(
                egui::TextEdit::singleline(&mut self.thumb_search)
                    .hint_text("buscar por nombre…")
                    .desired_width(200.0),
            );
            if search_resp.changed() {
                self.thumb_selected = None;
            }
            if !self.thumb_search.is_empty() && ui.small_button("✕").clicked() {
                self.thumb_search.clear();
                self.thumb_selected = None;
            }
        });

        // Collect visible entries (original index, entry ref)
        let search_lc = self.thumb_search.to_lowercase();
        let visible: Vec<(usize, &ThumbEntry)> = self
            .thumb_entries
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                self.thumb_filter.matches(&e.media_type)
                    && (search_lc.is_empty()
                        || e.name.to_lowercase().contains(&search_lc))
            })
            .collect();

        let total_all = self.thumb_entries.len();
        let shown = visible.len();
        if shown == total_all {
            ui.label(format!("{total_all} miniaturas"));
        } else {
            ui.label(format!("{shown} de {total_all} miniaturas"));
        }
        ui.separator();

        // ── Detail panel (right) ──────────────────────────────────────────
        if let Some(sel_idx) = self.thumb_selected {
            if let Some(entry) = self.thumb_entries.get(sel_idx) {
                let name = entry.name.clone();
                let path = entry.path.clone();
                let media_type = entry.media_type.clone();
                let thumb_path = entry.thumb_path.clone();

                egui::SidePanel::right("thumb_detail")
                    .resizable(true)
                    .min_width(200.0)
                    .default_width(240.0)
                    .show_inside(ui, |ui| {
                        ui.add_space(4.0);
                        ui.heading("Archivo original");
                        ui.separator();

                        let uri = format!("file://{}", thumb_path.display());
                        ui.add(
                            egui::Image::new(uri)
                                .max_size(egui::vec2(220.0, 180.0))
                                .fit_to_exact_size(egui::vec2(220.0, 180.0))
                                .rounding(egui::Rounding::same(6.0)),
                        );
                        ui.add_space(8.0);

                        egui::Grid::new("thumb_detail_grid")
                            .num_columns(2)
                            .spacing([6.0, 4.0])
                            .show(ui, |ui| {
                                ui.label(egui::RichText::new("Nombre:").weak());
                                ui.label(&name);
                                ui.end_row();
                                if !media_type.is_empty() {
                                    ui.label(egui::RichText::new("Tipo:").weak());
                                    let (badge, color) = thumb_type_badge(&media_type);
                                    ui.colored_label(color, badge);
                                    ui.end_row();
                                }
                            });

                        ui.add_space(6.0);
                        ui.label(egui::RichText::new("Ruta:").weak());
                        egui::ScrollArea::vertical()
                            .id_salt("thumb_path_scroll")
                            .max_height(70.0)
                            .show(ui, |ui| {
                                ui.label(egui::RichText::new(&path).weak().size(11.0));
                            });

                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if ui.button("📋 Copiar ruta").clicked() {
                                ui.output_mut(|o| o.copied_text = path.clone());
                            }
                            if ui.button("📂 Abrir carpeta").clicked() {
                                let dir = std::path::Path::new(&path)
                                    .parent()
                                    .map(|p| p.to_string_lossy().to_string())
                                    .unwrap_or_default();
                                open_path(&dir);
                            }
                        });
                        if ui.button("▶ Abrir archivo").clicked() {
                            open_path(&path);
                        }
                    });
            }
        }

        // ── Thumbnail grid ────────────────────────────────────────────────
        egui::CentralPanel::default().show_inside(ui, |ui| {
            if visible.is_empty() {
                ui.add_space(30.0);
                ui.centered_and_justified(|ui| {
                    ui.label("No hay miniaturas que coincidan con el filtro.");
                });
                return;
            }

            let cell = 140.0_f32;
            let label_h = 32.0_f32;
            let cols = ((ui.available_width() / (cell + 4.0)) as usize).max(1);

            egui::ScrollArea::vertical().show(ui, |ui| {
                egui::Grid::new("thumb_grid")
                    .min_col_width(cell)
                    .max_col_width(cell)
                    .spacing([4.0, 4.0])
                    .show(ui, |ui| {
                        for (grid_pos, (orig_idx, entry)) in visible.iter().enumerate() {
                            let selected = self.thumb_selected == Some(*orig_idx);
                            let uri = format!("file://{}", entry.thumb_path.display());
                            let img = egui::Image::new(&uri)
                                .max_size(egui::vec2(cell, cell))
                                .fit_to_exact_size(egui::vec2(cell, cell))
                                .rounding(egui::Rounding::same(4.0))
                                .sense(egui::Sense::click());

                            ui.vertical(|ui| {
                                ui.set_min_width(cell);

                                let frame = if selected {
                                    egui::Frame::default()
                                        .stroke(egui::Stroke::new(2.0, egui::Color32::from_rgb(80, 160, 255)))
                                        .rounding(egui::Rounding::same(5.0))
                                        .inner_margin(egui::Margin::same(2.0))
                                } else {
                                    egui::Frame::default()
                                        .stroke(egui::Stroke::new(1.0, egui::Color32::TRANSPARENT))
                                        .rounding(egui::Rounding::same(5.0))
                                        .inner_margin(egui::Margin::same(2.0))
                                };

                                let resp = frame.show(ui, |ui| ui.add(img));
                                if resp.inner.clicked() {
                                    self.thumb_selected =
                                        if selected { None } else { Some(*orig_idx) };
                                }

                                // Type badge (small, top-right corner feel via label)
                                let (badge, color) = thumb_type_badge(&entry.media_type);
                                ui.horizontal(|ui| {
                                    ui.colored_label(color, egui::RichText::new(badge).size(10.0));
                                    let display_name = entry.name
                                        .get(..18.min(entry.name.len()))
                                        .map(|s| if entry.name.len() > 18 { format!("{s}…") } else { s.to_string() })
                                        .unwrap_or_default();
                                    ui.add_sized(
                                        [cell - 30.0, label_h],
                                        egui::Label::new(
                                            egui::RichText::new(display_name).size(11.0),
                                        )
                                        .truncate(),
                                    ).on_hover_text(&entry.path);
                                });
                            });

                            if (grid_pos + 1) % cols == 0 {
                                ui.end_row();
                            }
                        }
                    });
            });
        });
    }
}

impl eframe::App for MediaIndexApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.process_results(ctx);

        // Keep repainting while a background task is running
        if self.is_loading || self.scanner.is_running || self.maintenance.thumb_progress.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(150));
        }

        self.show_top_bar(ctx);
        self.show_sidebar(ctx);

        egui::CentralPanel::default().show(ctx, |ui| match self.active_view {
            View::Dashboard => {
                dashboard::show(ui, &mut self.dashboard, &self.scan_history, ctx, &self.db_path, &self.result_tx)
            }
            View::Scanner => {
                scanner_view::show(ui, &mut self.scanner, ctx, &self.db_path, &self.result_tx)
            }
            View::Browser => {
                browser::show(ui, &mut self.browser, ctx, &self.db_path, &self.result_tx)
            }
            View::Duplicates => {
                dupes::show(ui, &mut self.dupes, ctx, &self.db_path, &self.result_tx)
            }
            View::Similar => {
                similar::show(ui, &mut self.similar, ctx, &self.db_path, &self.result_tx)
            }
            View::Thumbnails => self.show_thumbnails_view(ui, ctx),
            View::Maintenance => maintenance::show(
                ui,
                &mut self.maintenance,
                ctx,
                &self.db_path,
                &self.result_tx,
            ),
        });
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────

fn thumb_type_badge(media_type: &str) -> (&'static str, egui::Color32) {
    match media_type {
        "3d"    => ("[3D]",  egui::Color32::from_rgb(80, 210, 210)),
        "video" => ("[VID]", egui::Color32::from_rgb(80, 140, 255)),
        "image" => ("[IMG]", egui::Color32::from_rgb(255, 210, 50)),
        _       => ("[?]",   egui::Color32::GRAY),
    }
}

// ── Entry point ───────────────────────────────────────────────────────────

pub fn run(db_path: &str) -> anyhow::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 820.0])
            .with_title("Media Index"),
        ..Default::default()
    };
    let db_path = db_path.to_string();
    eframe::run_native(
        "Media Index",
        options,
        Box::new(move |cc| {
            egui_extras::install_image_loaders(&cc.egui_ctx);
            Ok(Box::new(MediaIndexApp::new(cc, db_path)))
        }),
    )
    .map_err(|e| anyhow::anyhow!("GUI error: {e}"))
}
