mod dashboard;
mod scanner_view;
mod browser;
mod dupes;
mod similar;
mod maintenance;

use std::path::PathBuf;
use std::sync::mpsc;
use eframe::egui;

use crate::db::{DbStats, DuplicateGroup, SearchResult};
use crate::models::{ScanStats, SimilarImageGroup, SimilarAudioGroup};

pub use dashboard::DashboardState;
pub use scanner_view::ScannerState;
pub use browser::BrowserState;
pub use dupes::DupesState;
pub use similar::SimilarState;
pub use maintenance::MaintenanceState;

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
    DupesDeleted { deleted: usize, freed_bytes: u64 },
    SimilarImages(Vec<SimilarImageGroup>),
    SimilarAudio(Vec<SimilarAudioGroup>),
    VerifyResults(Vec<VerifyEntry>),
    ThumbsComplete { ok: usize, skipped: usize, errors: usize },
    CleanDone(usize),
    Error(String),
    Info(String),
}

pub struct VerifyEntry {
    pub name:   String,
    pub path:   String,
    pub status: VerifyStatus,
}

pub enum VerifyStatus { Ok, Missing, Corrupted }

// ── App ───────────────────────────────────────────────────────────────────

pub struct MediaIndexApp {
    db_path:       String,
    db_path_input: String,

    active_view: View,

    result_tx: mpsc::Sender<TaskResult>,
    result_rx: mpsc::Receiver<TaskResult>,

    pub dashboard:   DashboardState,
    pub scanner:     ScannerState,
    pub browser:     BrowserState,
    pub dupes:       DupesState,
    pub similar:     SimilarState,
    pub maintenance: MaintenanceState,

    is_loading:  bool,
    status_msg:  String,
    #[allow(dead_code)]
    thumb_scroll: f32,
}

impl MediaIndexApp {
    fn new(_cc: &eframe::CreationContext<'_>, db_path: String) -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            db_path_input: db_path.clone(),
            db_path,
            active_view:  View::Dashboard,
            result_tx:    tx,
            result_rx:    rx,
            dashboard:    DashboardState::default(),
            scanner:      ScannerState::default(),
            browser:      BrowserState::default(),
            dupes:        DupesState::default(),
            similar:      SimilarState::default(),
            maintenance:  MaintenanceState::default(),
            is_loading:   false,
            status_msg:   String::new(),
            thumb_scroll: 0.0,
        }
    }

    fn spawn<F>(&self, ctx: egui::Context, f: F)
    where F: FnOnce() -> TaskResult + Send + 'static
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
            match crate::db::Database::open(&db_path)
                .and_then(|db| db.stats())
            {
                Ok(s)  => TaskResult::StatsLoaded(s),
                Err(e) => TaskResult::Error(e.to_string()),
            }
        });
    }

    pub fn load_dupes(&mut self, ctx: egui::Context) {
        self.is_loading = true;
        let db_path = self.db_path.clone();
        self.spawn(ctx, move || {
            match crate::db::Database::open(&db_path)
                .and_then(|db| db.duplicates())
            {
                Ok(g)  => TaskResult::DupesLoaded(g),
                Err(e) => TaskResult::Error(e.to_string()),
            }
        });
    }

    fn process_results(&mut self) {
        while let Ok(result) = self.result_rx.try_recv() {
            self.is_loading = false;
            match result {
                TaskResult::StatsLoaded(s)  => { self.dashboard.stats = Some(s); }
                TaskResult::ScanComplete(s) => {
                    self.scanner.is_running = false;
                    self.scanner.last_stats = Some(s);
                    self.scanner.log.push("Scan complete.".into());
                }
                TaskResult::SearchResults(r) => { self.browser.results = r; }
                TaskResult::DupesLoaded(g)   => { self.dupes.groups = g; }
                TaskResult::DupesDeleted { deleted, freed_bytes } => {
                    use humansize::{format_size, DECIMAL};
                    self.status_msg = format!(
                        "Deleted {deleted} file(s), freed {}",
                        format_size(freed_bytes, DECIMAL)
                    );
                }
                TaskResult::SimilarImages(g) => { self.similar.image_groups = g; }
                TaskResult::SimilarAudio(g)  => { self.similar.audio_groups = g; }
                TaskResult::VerifyResults(r) => { self.maintenance.verify_results = r; }
                TaskResult::ThumbsComplete { ok, skipped, errors } => {
                    self.maintenance.thumbs_result = Some((ok, skipped, errors));
                }
                TaskResult::CleanDone(n) => {
                    self.maintenance.clean_result = Some(n);
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
                    ("Dashboard",     View::Dashboard,   "📊"),
                    ("Scanner",       View::Scanner,     "🔍"),
                    ("Explorador",    View::Browser,     "📁"),
                    ("Duplicados",    View::Duplicates,  "♊"),
                    ("Similares",     View::Similar,     "🔮"),
                    ("Miniaturas",    View::Thumbnails,  "🖼"),
                    ("Mantenimiento", View::Maintenance, "🛠"),
                ];
                for (label, view, icon) in entries {
                    let sel = self.active_view == *view;
                    let text = format!("{icon} {label}");
                    let btn = ui.add_sized(
                        [138.0, 32.0],
                        egui::SelectableLabel::new(sel, &text),
                    );
                    if btn.clicked() {
                        self.active_view = *view;
                        match view {
                            View::Dashboard  => self.load_stats(ctx.clone()),
                            View::Duplicates => self.load_dupes(ctx.clone()),
                            _ => {}
                        }
                    }
                }
            });
    }

    fn show_thumbnails_view(&mut self, ui: &mut egui::Ui, _ctx: &egui::Context) {
        let thumb_dir = crate::thumbs::thumb_dir_for_db(&self.db_path);

        ui.horizontal(|ui| {
            ui.heading("Miniaturas");
            if ui.button("Generar →").clicked() {
                self.active_view = View::Maintenance;
            }
        });
        ui.label(format!("Directorio: {}", thumb_dir.display()));
        ui.separator();

        if !thumb_dir.exists() {
            ui.colored_label(
                egui::Color32::from_rgb(255, 200, 50),
                "No hay miniaturas generadas todavía. Usa Mantenimiento → Generar Miniaturas.",
            );
            return;
        }

        // Collect .jpg files from prefix subdirs
        let thumbs: Vec<PathBuf> = std::fs::read_dir(&thumb_dir)
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

        ui.label(format!("{} miniaturas", thumbs.len()));
        ui.separator();

        let cell = 140.0_f32;
        let cols = ((ui.available_width() / cell) as usize).max(1);

        egui::ScrollArea::vertical().show(ui, |ui| {
            egui::Grid::new("thumb_grid")
                .min_col_width(cell)
                .max_col_width(cell)
                .spacing([4.0, 4.0])
                .show(ui, |ui| {
                    for (i, path) in thumbs.iter().enumerate() {
                        let uri = format!("file://{}", path.display());
                        let img = egui::Image::new(uri)
                            .max_size(egui::vec2(cell - 8.0, cell - 8.0))
                            .fit_to_exact_size(egui::vec2(cell - 8.0, cell - 8.0))
                            .rounding(egui::Rounding::same(4.0));
                        ui.add(img);
                        if (i + 1) % cols == 0 {
                            ui.end_row();
                        }
                    }
                });
        });
    }
}

impl eframe::App for MediaIndexApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.process_results();

        // Keep repainting while a background task is running
        if self.is_loading || self.scanner.is_running {
            ctx.request_repaint_after(std::time::Duration::from_millis(150));
        }

        self.show_top_bar(ctx);
        self.show_sidebar(ctx);

        egui::CentralPanel::default().show(ctx, |ui| {
            match self.active_view {
                View::Dashboard  => dashboard::show(ui, &mut self.dashboard,
                                        ctx, &self.db_path, &self.result_tx),
                View::Scanner    => scanner_view::show(ui, &mut self.scanner,
                                        ctx, &self.db_path, &self.result_tx),
                View::Browser    => browser::show(ui, &mut self.browser,
                                        ctx, &self.db_path, &self.result_tx),
                View::Duplicates => dupes::show(ui, &mut self.dupes,
                                        ctx, &self.db_path, &self.result_tx),
                View::Similar    => similar::show(ui, &mut self.similar,
                                        ctx, &self.db_path, &self.result_tx),
                View::Thumbnails => self.show_thumbnails_view(ui, ctx),
                View::Maintenance => maintenance::show(ui, &mut self.maintenance,
                                        ctx, &self.db_path, &self.result_tx),
            }
        });
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
