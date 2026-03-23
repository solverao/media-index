use std::sync::mpsc;
use eframe::egui;
use humansize::{format_size, DECIMAL};

use crate::db::DbStats;
use super::TaskResult;

#[derive(Default)]
pub struct DashboardState {
    pub stats: Option<DbStats>,
}

pub fn show(
    ui:      &mut egui::Ui,
    state:   &mut DashboardState,
    ctx:     &egui::Context,
    db_path: &str,
    tx:      &mpsc::Sender<TaskResult>,
) {
    ui.horizontal(|ui| {
        ui.heading("📊 Dashboard");
        if ui.button("↺ Actualizar").clicked() {
            load(ctx.clone(), db_path, tx);
        }
    });
    ui.separator();

    match &state.stats {
        None => {
            ui.add_space(40.0);
            ui.vertical_centered(|ui| {
                ui.label("Abre una base de datos y pulsa Actualizar.");
                ui.add_space(10.0);
                if ui.button("Cargar estadísticas").clicked() {
                    load(ctx.clone(), db_path, tx);
                }
            });
        }
        Some(s) => {
            show_stats(ui, s);
        }
    }
}

fn show_stats(ui: &mut egui::Ui, s: &DbStats) {
    // ── Stat cards ────────────────────────────────────────────────────────
    ui.horizontal_wrapped(|ui| {
        stat_card(ui, "Archivos únicos",       &s.total.to_string(),
                  egui::Color32::from_rgb(90, 160, 255));
        stat_card(ui, "Duplicados",            &s.dupes.to_string(),
                  egui::Color32::from_rgb(255, 90, 90));
        stat_card(ui, "Tamaño total",          &format_size(s.bytes as u64, DECIMAL),
                  egui::Color32::from_rgb(80, 200, 120));
        stat_card(ui, "Recuperable (dupes)",   &format_size(s.bytes_dup as u64, DECIMAL),
                  egui::Color32::from_rgb(255, 190, 50));
    });

    ui.add_space(20.0);
    ui.separator();
    ui.heading("Por tipo");
    ui.add_space(8.0);

    // ── Type breakdown ────────────────────────────────────────────────────
    let total = s.total.max(1) as f32;
    for (type_name, count, bytes) in &s.by_type {
        let (icon, color) = type_style(type_name);
        ui.horizontal(|ui| {
            ui.add_sized([14.0, 18.0], egui::Label::new(
                egui::RichText::new(icon).color(color)
            ));
            ui.add_sized([60.0, 18.0], egui::Label::new(
                egui::RichText::new(type_name.to_uppercase())
                    .color(color)
                    .strong()
            ));
            ui.add_sized([70.0, 18.0], egui::Label::new(
                format!("{count} archivos")
            ));
            ui.add_sized([90.0, 18.0], egui::Label::new(
                egui::RichText::new(format_size(*bytes as u64, DECIMAL)).weak()
            ));
            let frac = *count as f32 / total;
            ui.add(
                egui::ProgressBar::new(frac)
                    .desired_width(220.0)
                    .fill(color)
                    .text(format!("{:.1}%", frac * 100.0))
            );
        });
        ui.add_space(2.0);
    }
}

fn stat_card(ui: &mut egui::Ui, label: &str, value: &str, color: egui::Color32) {
    egui::Frame::default()
        .fill(ui.visuals().faint_bg_color)
        .rounding(egui::Rounding::same(8.0))
        .inner_margin(egui::Margin::same(14.0))
        .show(ui, |ui| {
            ui.set_min_width(150.0);
            ui.vertical(|ui| {
                ui.add(egui::Label::new(
                    egui::RichText::new(value)
                        .size(30.0)
                        .strong()
                        .color(color),
                ));
                ui.label(label);
            });
        });
}

fn type_style(t: &str) -> (&'static str, egui::Color32) {
    match t {
        "3d"    => ("⬡", egui::Color32::from_rgb(80, 210, 210)),
        "video" => ("▶", egui::Color32::from_rgb(80, 140, 255)),
        "audio" => ("♪", egui::Color32::from_rgb(200, 90, 255)),
        "image" => ("🖼", egui::Color32::from_rgb(255, 210, 50)),
        "other" => ("·", egui::Color32::GRAY),
        _       => ("?", egui::Color32::WHITE),
    }
}

pub fn load(ctx: egui::Context, db_path: &str, tx: &mpsc::Sender<TaskResult>) {
    let db_path = db_path.to_string();
    let tx = tx.clone();
    std::thread::spawn(move || {
        let result = crate::db::Database::open(&db_path)
            .and_then(|db| db.stats())
            .map(TaskResult::StatsLoaded)
            .unwrap_or_else(|e| TaskResult::Error(e.to_string()));
        let _ = tx.send(result);
        ctx.request_repaint();
    });
}
