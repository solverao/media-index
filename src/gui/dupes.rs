use eframe::egui;
use humansize::{DECIMAL, format_size};
use std::collections::HashSet;
use std::sync::mpsc;

use super::TaskResult;
use crate::db::DuplicateGroup;

pub struct DupesState {
    pub groups: Vec<DuplicateGroup>,
    expanded: HashSet<String>,       // expanded group hashes
    selected_dupes: HashSet<String>, // paths marked for deletion
    type_filter: DupesTypeFilter,
    confirm_delete: bool,
    dry_run: bool,
    pub last_msg: Option<String>,
}

impl Default for DupesState {
    fn default() -> Self {
        Self {
            groups: vec![],
            expanded: HashSet::new(),
            selected_dupes: HashSet::new(),
            type_filter: DupesTypeFilter::All,
            confirm_delete: false,
            dry_run: true,
            last_msg: None,
        }
    }
}

#[derive(PartialEq, Clone, Copy, Default)]
enum DupesTypeFilter {
    #[default]
    All,
    Td,
    Video,
    Audio,
    Image,
}

impl DupesTypeFilter {
    fn label(self) -> &'static str {
        match self {
            Self::All => "Todos",
            Self::Td => "3D",
            Self::Video => "Video",
            Self::Audio => "Audio",
            Self::Image => "Imagen",
        }
    }
    fn as_db_str(self) -> Option<&'static str> {
        match self {
            Self::All => None,
            Self::Td => Some("3d"),
            Self::Video => Some("video"),
            Self::Audio => Some("audio"),
            Self::Image => Some("image"),
        }
    }
}

pub fn show(
    ui: &mut egui::Ui,
    state: &mut DupesState,
    ctx: &egui::Context,
    db_path: &str,
    tx: &mpsc::Sender<TaskResult>,
) {
    ui.horizontal(|ui| {
        ui.heading("♊ Duplicados");
        if ui.button("↺ Cargar").clicked() {
            state.selected_dupes.clear();
            state.confirm_delete = false;
            load(ctx.clone(), db_path, tx);
        }
    });
    ui.separator();

    if state.groups.is_empty() {
        ui.add_space(30.0);
        ui.centered_and_justified(|ui| {
            ui.label("Sin duplicados. Pulsa \"Cargar\" o escanea primero.");
        });
        return;
    }

    // ── Type filter ───────────────────────────────────────────────────────
    ui.horizontal(|ui| {
        ui.label("Filtrar:");
        for f in [
            DupesTypeFilter::All,
            DupesTypeFilter::Td,
            DupesTypeFilter::Video,
            DupesTypeFilter::Audio,
            DupesTypeFilter::Image,
        ] {
            if ui
                .selectable_label(state.type_filter == f, f.label())
                .clicked()
            {
                state.type_filter = f;
            }
        }
    });

    // ── Summary (computed without holding a reference into state.groups) ──
    let filter_str = state.type_filter.as_db_str();
    let visible_count: usize = state
        .groups
        .iter()
        .filter(|g| filter_str.map(|f| g.media_type == f).unwrap_or(true))
        .count();
    let total_reclaimable: u64 = state
        .groups
        .iter()
        .filter(|g| filter_str.map(|f| g.media_type == f).unwrap_or(true))
        .map(|g| g.size_bytes * g.duplicates.len() as u64)
        .sum();

    ui.horizontal(|ui| {
        ui.label(format!(
            "{visible_count} grupos  ·  {} recuperables",
            format_size(total_reclaimable, DECIMAL)
        ));
        let n_sel = state.selected_dupes.len();
        if n_sel > 0 {
            ui.separator();
            ui.colored_label(
                egui::Color32::from_rgb(255, 90, 90),
                format!("{n_sel} seleccionados para eliminar"),
            );
        }
    });

    ui.separator();

    // ── Delete controls ───────────────────────────────────────────────────
    if !state.selected_dupes.is_empty() {
        ui.horizontal(|ui| {
            ui.checkbox(&mut state.dry_run, "Simulación (dry-run)");
            if !state.confirm_delete {
                if ui
                    .add(
                        egui::Button::new("🗑 Eliminar seleccionados")
                            .fill(egui::Color32::from_rgb(180, 40, 40)),
                    )
                    .clicked()
                {
                    state.confirm_delete = true;
                }
            } else {
                ui.colored_label(
                    egui::Color32::from_rgb(255, 90, 90),
                    "¿Confirmar eliminación?",
                );
                if ui.button("✓ Sí, eliminar").clicked() {
                    delete_selected(state, ctx.clone(), db_path, tx);
                    state.confirm_delete = false;
                }
                if ui.button("✗ Cancelar").clicked() {
                    state.confirm_delete = false;
                }
            }
        });
        ui.separator();
    }

    if let Some(msg) = &state.last_msg.clone() {
        ui.colored_label(egui::Color32::from_rgb(100, 220, 100), msg);
        ui.separator();
    }

    // ── Group list: split borrows explicitly ──────────────────────────────
    let DupesState {
        groups,
        expanded,
        selected_dupes,
        type_filter,
        ..
    } = state;
    let filter_str2 = type_filter.as_db_str();

    egui::ScrollArea::vertical().show(ui, |ui| {
        for g in groups
            .iter()
            .filter(|g| filter_str2.map(|f| g.media_type == f).unwrap_or(true))
        {
            show_group(ui, g, expanded, selected_dupes);
        }
    });
}

fn show_group(
    ui: &mut egui::Ui,
    g: &DuplicateGroup,
    expanded: &mut HashSet<String>,
    selected: &mut HashSet<String>,
) {
    let is_open = expanded.contains(&g.hash);
    let (icon, color) = type_style(&g.media_type);

    let header_text = format!(
        "{icon} {}  [{}]  {} dupl. · {}",
        g.canonical_name,
        g.media_type.to_uppercase(),
        g.duplicates.len(),
        format_size(g.size_bytes, DECIMAL),
    );

    egui::CollapsingHeader::new(egui::RichText::new(&header_text).color(color))
        .id_salt(&g.hash)
        .default_open(false)
        .show(ui, |ui| {
            egui::Grid::new(format!("dg_{}", &g.hash))
                .num_columns(3)
                .spacing([6.0, 3.0])
                .show(ui, |ui| {
                    // Canonical
                    ui.label(
                        egui::RichText::new("✓ Original")
                            .color(egui::Color32::from_rgb(80, 200, 80)),
                    );
                    ui.label(&g.canonical_path);
                    ui.label(""); // no delete button for canonical
                    ui.end_row();

                    // Duplicates
                    for d in &g.duplicates {
                        let in_archive = d.contains("::");
                        let is_sel = selected.contains(d);

                        ui.label(if in_archive {
                            egui::RichText::new("⊡ Archivo")
                                .color(egui::Color32::from_rgb(200, 160, 50))
                        } else {
                            egui::RichText::new("↳ Copia")
                                .color(egui::Color32::from_rgb(255, 90, 90))
                        });

                        ui.label(egui::RichText::new(d).weak());

                        if !in_archive {
                            let chk_label = if is_sel { "☑ Marcar" } else { "☐ Marcar" };
                            if ui.small_button(chk_label).clicked() {
                                if is_sel {
                                    selected.remove(d);
                                } else {
                                    selected.insert(d.clone());
                                }
                            }
                        } else {
                            ui.label(egui::RichText::new("(en archivo)").weak());
                        }
                        ui.end_row();
                    }
                });

            // Select/deselect all in this group
            ui.horizontal(|ui| {
                let loose: Vec<&String> =
                    g.duplicates.iter().filter(|d| !d.contains("::")).collect();
                if !loose.is_empty() {
                    if ui.small_button("Marcar todos").clicked() {
                        for d in &loose {
                            selected.insert(d.to_string());
                        }
                    }
                    if ui.small_button("Desmarcar todos").clicked() {
                        for d in &loose {
                            selected.remove(*d);
                        }
                    }
                }
            });
        });

    // Track expanded state from CollapsingHeader (approximation via hash presence)
    let _ = is_open; // already handled by egui
}

fn type_style(t: &str) -> (&'static str, egui::Color32) {
    match t {
        "3d" => ("⬡", egui::Color32::from_rgb(80, 210, 210)),
        "video" => ("▶", egui::Color32::from_rgb(80, 140, 255)),
        "audio" => ("♪", egui::Color32::from_rgb(200, 90, 255)),
        "image" => ("🖼", egui::Color32::from_rgb(255, 210, 50)),
        _ => ("·", egui::Color32::GRAY),
    }
}

fn delete_selected(
    state: &mut DupesState,
    ctx: egui::Context,
    db_path: &str,
    tx: &mpsc::Sender<TaskResult>,
) {
    let paths: Vec<String> = state.selected_dupes.iter().cloned().collect();
    let dry_run = state.dry_run;
    let db_path = db_path.to_string();
    let tx = tx.clone();

    std::thread::spawn(move || {
        let mut deleted = 0usize;
        let mut freed = 0u64;
        let mut errors = 0usize;

        for path in &paths {
            let p = std::path::Path::new(path);
            if dry_run {
                freed += p.metadata().map(|m| m.len()).unwrap_or(0);
                deleted += 1;
            } else {
                match p.metadata() {
                    Ok(m) => {
                        let sz = m.len();
                        if std::fs::remove_file(p).is_ok() {
                            freed += sz;
                            deleted += 1;
                        } else {
                            errors += 1;
                        }
                    }
                    Err(_) => errors += 1,
                }
            }
        }

        // Sync DB
        if !dry_run {
            if let Ok(db) = crate::db::Database::open(&db_path) {
                let _ = db.cleanup_stale();
            }
        }

        let prefix = if dry_run { "[DRY-RUN] " } else { "" };
        let msg = format!(
            "{prefix}Eliminados {deleted} archivo(s), liberados {}{}",
            format_size(freed, DECIMAL),
            if errors > 0 {
                format!(", {errors} error(s)")
            } else {
                String::new()
            },
        );
        let _ = tx.send(TaskResult::Info(msg));
        // Reload dupes
        if let Ok(db) = crate::db::Database::open(&db_path) {
            if let Ok(g) = db.duplicates() {
                let _ = tx.send(TaskResult::DupesLoaded(g));
            }
        }
        ctx.request_repaint();
    });

    state.selected_dupes.clear();
}

pub fn load(ctx: egui::Context, db_path: &str, tx: &mpsc::Sender<TaskResult>) {
    let db_path = db_path.to_string();
    let tx = tx.clone();
    std::thread::spawn(move || {
        let result = crate::db::Database::open(&db_path)
            .and_then(|db| db.duplicates())
            .map(TaskResult::DupesLoaded)
            .unwrap_or_else(|e| TaskResult::Error(e.to_string()));
        let _ = tx.send(result);
        ctx.request_repaint();
    });
}
