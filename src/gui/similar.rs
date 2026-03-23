use eframe::egui;
use std::sync::mpsc;

use super::TaskResult;
use crate::models::{SimilarAudioGroup, SimilarImageGroup};

pub struct SimilarState {
    pub image_groups: Vec<SimilarImageGroup>,
    pub audio_groups: Vec<SimilarAudioGroup>,
    tab: SimilarTab,
    threshold: u32,
}

impl Default for SimilarState {
    fn default() -> Self {
        Self {
            image_groups: vec![],
            audio_groups: vec![],
            tab: SimilarTab::Images,
            threshold: 10,
        }
    }
}

#[derive(Default, PartialEq, Clone, Copy)]
enum SimilarTab {
    #[default]
    Images,
    Audio,
}

pub fn show(
    ui: &mut egui::Ui,
    state: &mut SimilarState,
    ctx: &egui::Context,
    db_path: &str,
    tx: &mpsc::Sender<TaskResult>,
) {
    ui.horizontal(|ui| {
        ui.heading("🔮 Archivos similares");
    });
    ui.separator();

    // ── Tab selector ──────────────────────────────────────────────────────
    ui.horizontal(|ui| {
        if ui
            .selectable_label(state.tab == SimilarTab::Images, "🖼 Imágenes similares")
            .clicked()
        {
            state.tab = SimilarTab::Images;
        }
        if ui
            .selectable_label(state.tab == SimilarTab::Audio, "♪ Audio duplicado")
            .clicked()
        {
            state.tab = SimilarTab::Audio;
        }
    });
    ui.separator();

    match state.tab {
        SimilarTab::Images => show_images(ui, state, ctx, db_path, tx),
        SimilarTab::Audio => show_audio(ui, state, ctx, db_path, tx),
    }
}

fn show_images(
    ui: &mut egui::Ui,
    state: &mut SimilarState,
    ctx: &egui::Context,
    db_path: &str,
    tx: &mpsc::Sender<TaskResult>,
) {
    ui.horizontal(|ui| {
        ui.label("Umbral de similitud (0 = idéntico, 64 = máximo):");
        ui.add(egui::Slider::new(&mut state.threshold, 0..=30).text("bits"));
        if ui.button("🔎 Buscar similares").clicked() {
            load_similar_images(ctx.clone(), db_path, state.threshold, tx);
        }
    });
    ui.add_space(4.0);

    if state.image_groups.is_empty() {
        ui.add_space(20.0);
        ui.centered_and_justified(|ui| {
            ui.label("Pulsa \"Buscar similares\" para analizar las imágenes indexadas.");
        });
        return;
    }

    ui.label(format!(
        "{} grupos de imágenes similares",
        state.image_groups.len()
    ));
    ui.separator();

    egui::ScrollArea::vertical().show(ui, |ui| {
        for (i, group) in state.image_groups.iter().enumerate() {
            egui::CollapsingHeader::new(format!(
                "Grupo {} — {} imágenes",
                i + 1,
                group.files.len()
            ))
            .id_salt(format!("img_group_{i}"))
            .default_open(i == 0)
            .show(ui, |ui| {
                egui::Grid::new(format!("img_g_{i}"))
                    .num_columns(4)
                    .spacing([10.0, 3.0])
                    .show(ui, |ui| {
                        ui.strong("Nombre");
                        ui.strong("Dimensiones");
                        ui.strong("pHash");
                        ui.strong("Ruta");
                        ui.end_row();

                        for entry in &group.files {
                            ui.label(&entry.name);
                            let dims = match (entry.width, entry.height) {
                                (Some(w), Some(h)) => format!("{w}×{h}"),
                                _ => "?".into(),
                            };
                            ui.label(dims);
                            ui.label(
                                egui::RichText::new(format!(
                                    "{}…",
                                    &entry.phash[..8.min(entry.phash.len())]
                                ))
                                .monospace()
                                .weak(),
                            );
                            ui.label(egui::RichText::new(&entry.path).weak().size(11.0));
                            ui.end_row();
                        }
                    });

                ui.horizontal(|ui| {
                    if ui.small_button("📋 Copiar rutas").clicked() {
                        let paths = group
                            .files
                            .iter()
                            .map(|f| f.path.as_str())
                            .collect::<Vec<_>>()
                            .join("\n");
                        ui.output_mut(|o| o.copied_text = paths);
                    }
                });
            });
            ui.add_space(2.0);
        }
    });
}

fn show_audio(
    ui: &mut egui::Ui,
    state: &mut SimilarState,
    ctx: &egui::Context,
    db_path: &str,
    tx: &mpsc::Sender<TaskResult>,
) {
    ui.horizontal(|ui| {
        ui.label("Detecta canciones con el mismo título + artista en múltiples archivos.");
        if ui.button("🔎 Buscar duplicados de audio").clicked() {
            load_similar_audio(ctx.clone(), db_path, tx);
        }
    });
    ui.add_space(4.0);

    if state.audio_groups.is_empty() {
        ui.add_space(20.0);
        ui.centered_and_justified(|ui| {
            ui.label("Pulsa \"Buscar duplicados de audio\" para analizar.");
        });
        return;
    }

    ui.label(format!(
        "{} grupos de canciones duplicadas",
        state.audio_groups.len()
    ));
    ui.separator();

    egui::ScrollArea::vertical().show(ui, |ui| {
        for (i, group) in state.audio_groups.iter().enumerate() {
            let header = format!(
                "♪ {} — {} · {} copias",
                group.title,
                group.artist,
                group.files.len()
            );
            egui::CollapsingHeader::new(
                egui::RichText::new(&header).color(egui::Color32::from_rgb(200, 90, 255)),
            )
            .id_salt(format!("aud_group_{i}"))
            .default_open(i == 0)
            .show(ui, |ui| {
                egui::Grid::new(format!("aud_g_{i}"))
                    .num_columns(3)
                    .spacing([10.0, 3.0])
                    .show(ui, |ui| {
                        ui.strong("Nombre");
                        ui.strong("Álbum");
                        ui.strong("Ruta");
                        ui.end_row();

                        for entry in &group.files {
                            ui.label(&entry.name);
                            ui.label(entry.album.as_deref().unwrap_or("—"));
                            ui.label(egui::RichText::new(&entry.path).weak().size(11.0));
                            ui.end_row();
                        }
                    });

                ui.horizontal(|ui| {
                    if ui.small_button("📋 Copiar rutas").clicked() {
                        let paths = group
                            .files
                            .iter()
                            .map(|f| f.path.as_str())
                            .collect::<Vec<_>>()
                            .join("\n");
                        ui.output_mut(|o| o.copied_text = paths);
                    }
                });
            });
            ui.add_space(2.0);
        }
    });
}

fn load_similar_images(
    ctx: egui::Context,
    db_path: &str,
    threshold: u32,
    tx: &mpsc::Sender<TaskResult>,
) {
    let db_path = db_path.to_string();
    let tx = tx.clone();
    std::thread::spawn(move || {
        let result = crate::db::Database::open(&db_path)
            .and_then(|db| db.similar_images(threshold))
            .map(TaskResult::SimilarImages)
            .unwrap_or_else(|e| TaskResult::Error(e.to_string()));
        let _ = tx.send(result);
        ctx.request_repaint();
    });
}

fn load_similar_audio(ctx: egui::Context, db_path: &str, tx: &mpsc::Sender<TaskResult>) {
    let db_path = db_path.to_string();
    let tx = tx.clone();
    std::thread::spawn(move || {
        let result = crate::db::Database::open(&db_path)
            .and_then(|db| db.similar_audio())
            .map(TaskResult::SimilarAudio)
            .unwrap_or_else(|e| TaskResult::Error(e.to_string()));
        let _ = tx.send(result);
        ctx.request_repaint();
    });
}
