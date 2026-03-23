use eframe::egui;
use egui_extras::{Column, TableBuilder};
use humansize::{DECIMAL, format_size};
use std::sync::mpsc;

use super::TaskResult;
use crate::db::{SearchDetail, SearchResult};

#[derive(Default)]
pub struct BrowserState {
    pub results: Vec<SearchResult>,
    query: String,
    type_filter: TypeFilter,
    selected: Option<usize>,
}

#[derive(Default, PartialEq, Clone, Copy)]
enum TypeFilter {
    #[default]
    All,
    Td,
    Video,
    Audio,
    Image,
    Other,
}

impl TypeFilter {
    fn label(self) -> &'static str {
        match self {
            Self::All => "Todos",
            Self::Td => "3D",
            Self::Video => "Video",
            Self::Audio => "Audio",
            Self::Image => "Imagen",
            Self::Other => "Otros",
        }
    }
    fn as_db_str(self) -> Option<&'static str> {
        match self {
            Self::All => None,
            Self::Td => Some("3d"),
            Self::Video => Some("video"),
            Self::Audio => Some("audio"),
            Self::Image => Some("image"),
            Self::Other => Some("other"),
        }
    }
}

pub fn show(
    ui: &mut egui::Ui,
    state: &mut BrowserState,
    ctx: &egui::Context,
    db_path: &str,
    tx: &mpsc::Sender<TaskResult>,
) {
    ui.heading("📁 Explorador de archivos");
    ui.separator();

    // ── Search bar ────────────────────────────────────────────────────────
    ui.horizontal(|ui| {
        ui.label("Buscar:");
        let r = ui.add(
            egui::TextEdit::singleline(&mut state.query)
                .hint_text("nombre de archivo...")
                .desired_width(300.0),
        );
        if ui.button("🔎 Buscar").clicked()
            || (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
        {
            do_search(ctx.clone(), db_path, &state.query, state.type_filter, tx);
            state.selected = None;
        }

        ui.separator();
        ui.label("Tipo:");
        for filter in [
            TypeFilter::All,
            TypeFilter::Td,
            TypeFilter::Video,
            TypeFilter::Audio,
            TypeFilter::Image,
            TypeFilter::Other,
        ] {
            if ui
                .selectable_label(state.type_filter == filter, filter.label())
                .clicked()
            {
                state.type_filter = filter;
            }
        }
    });

    ui.add_space(4.0);

    if state.results.is_empty() {
        ui.add_space(30.0);
        ui.centered_and_justified(|ui| {
            ui.label("Sin resultados. Escribe un término y pulsa Buscar.");
        });
        return;
    }

    ui.label(format!("{} resultados", state.results.len()));
    ui.separator();

    // ── Split: table left, detail right ──────────────────────────────────
    let detail_width = if state.selected.is_some() { 280.0 } else { 0.0 };

    egui::SidePanel::right("browser_detail")
        .resizable(true)
        .min_width(0.0)
        .default_width(detail_width)
        .show_inside(ui, |ui| {
            if let Some(idx) = state.selected {
                if let Some(r) = state.results.get(idx) {
                    show_detail(ui, r);
                }
            }
        });

    egui::CentralPanel::default().show_inside(ui, |ui| {
        show_table(ui, state);
    });
}

fn show_table(ui: &mut egui::Ui, state: &mut BrowserState) {
    TableBuilder::new(ui)
        .striped(true)
        .resizable(true)
        .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
        .column(Column::initial(60.0).at_least(40.0)) // Tipo
        .column(Column::initial(220.0).at_least(80.0)) // Nombre
        .column(Column::initial(80.0)) // Tamaño
        .column(Column::remainder().at_least(100.0)) // Ruta
        .header(22.0, |mut h| {
            h.col(|ui| {
                ui.strong("Tipo");
            });
            h.col(|ui| {
                ui.strong("Nombre");
            });
            h.col(|ui| {
                ui.strong("Tamaño");
            });
            h.col(|ui| {
                ui.strong("Ruta");
            });
        })
        .body(|mut body| {
            for (i, r) in state.results.iter().enumerate() {
                let selected = state.selected == Some(i);
                body.row(20.0, |mut row| {
                    row.set_selected(selected);
                    row.col(|ui| {
                        let (badge, color) = type_badge(&r.media_type);
                        ui.colored_label(color, badge);
                    });
                    row.col(|ui| {
                        if ui.selectable_label(selected, &r.name).clicked() {
                            state.selected = Some(i);
                        }
                    });
                    row.col(|ui| {
                        ui.label(format_size(r.size_bytes, DECIMAL));
                    });
                    row.col(|ui| {
                        ui.label(egui::RichText::new(&r.path).weak());
                    });
                });
            }
        });
}

fn show_detail(ui: &mut egui::Ui, r: &SearchResult) {
    ui.heading("Detalles");
    ui.separator();

    let (badge, color) = type_badge(&r.media_type);
    ui.horizontal(|ui| {
        ui.colored_label(color, egui::RichText::new(badge).size(20.0));
        ui.label(egui::RichText::new(&r.name).strong().size(14.0));
    });
    ui.add_space(4.0);

    egui::Grid::new("detail_grid")
        .num_columns(2)
        .spacing([8.0, 4.0])
        .show(ui, |ui| {
            detail_row(ui, "Tamaño", &format_size(r.size_bytes, DECIMAL));
            detail_row(ui, "Extensión", &r.extension.to_uppercase());

            match &r.detail {
                SearchDetail::Audio {
                    duration,
                    artist,
                    title,
                    album,
                } => {
                    if let Some(d) = duration {
                        detail_row(ui, "Duración", &fmt_duration(*d));
                    }
                    if let Some(a) = artist {
                        detail_row(ui, "Artista", a);
                    }
                    if let Some(t) = title {
                        detail_row(ui, "Título", t);
                    }
                    if let Some(a) = album {
                        detail_row(ui, "Álbum", a);
                    }
                }
                SearchDetail::Video {
                    duration,
                    width,
                    height,
                    title,
                } => {
                    if let Some(d) = duration {
                        detail_row(ui, "Duración", &fmt_duration(*d));
                    }
                    if let (Some(w), Some(h)) = (width, height) {
                        detail_row(ui, "Resolución", &format!("{w}×{h}"));
                    }
                    if let Some(t) = title {
                        detail_row(ui, "Título", t);
                    }
                }
                SearchDetail::Image {
                    width,
                    height,
                    camera,
                } => {
                    if let (Some(w), Some(h)) = (width, height) {
                        detail_row(ui, "Dimensiones", &format!("{w}×{h} px"));
                    }
                    if let Some(c) = camera {
                        detail_row(ui, "Cámara", c);
                    }
                }
                SearchDetail::Print3D { triangles } => {
                    if let Some(t) = triangles {
                        detail_row(ui, "Triángulos", &format!("{t}"));
                    }
                }
                SearchDetail::Other => {}
            }
        });

    ui.add_space(8.0);
    ui.label(egui::RichText::new("Ruta").weak());
    egui::ScrollArea::vertical()
        .max_height(80.0)
        .show(ui, |ui| {
            ui.label(egui::RichText::new(&r.path).weak().size(11.0));
        });

    ui.add_space(8.0);
    if ui.button("📋 Copiar ruta").clicked() {
        ui.output_mut(|o| o.copied_text = r.path.clone());
    }
    #[cfg(target_os = "linux")]
    if ui.button("📂 Abrir ubicación").clicked() {
        let dir = std::path::Path::new(&r.path)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        let _ = std::process::Command::new("xdg-open").arg(&dir).spawn();
    }
}

fn detail_row(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.label(egui::RichText::new(format!("{label}:")).weak());
    ui.label(value);
    ui.end_row();
}

fn type_badge(t: &str) -> (&'static str, egui::Color32) {
    match t {
        "3d" => ("[3D]", egui::Color32::from_rgb(80, 210, 210)),
        "video" => ("[VID]", egui::Color32::from_rgb(80, 140, 255)),
        "audio" => ("[AUD]", egui::Color32::from_rgb(200, 90, 255)),
        "image" => ("[IMG]", egui::Color32::from_rgb(255, 210, 50)),
        "other" => ("[OTR]", egui::Color32::GRAY),
        _ => ("[?]", egui::Color32::WHITE),
    }
}

fn fmt_duration(secs: f64) -> String {
    let s = secs as u64;
    if s >= 3600 {
        format!("{}h {:02}m {:02}s", s / 3600, (s % 3600) / 60, s % 60)
    } else {
        format!("{}m {:02}s", s / 60, s % 60)
    }
}

fn do_search(
    ctx: egui::Context,
    db_path: &str,
    query: &str,
    filter: TypeFilter,
    tx: &mpsc::Sender<TaskResult>,
) {
    let db_path = db_path.to_string();
    let query = query.to_string();
    let tx = tx.clone();
    std::thread::spawn(move || {
        let result = crate::db::Database::open(&db_path)
            .and_then(|db| db.search(&query, filter.as_db_str()))
            .map(TaskResult::SearchResults)
            .unwrap_or_else(|e| TaskResult::Error(e.to_string()));
        let _ = tx.send(result);
        ctx.request_repaint();
    });
}
