//! PassThrough node body rendering: directory-tree widget for the
//! properties panel and the on-canvas file list painter.
//!
//! Pure rendering -- no app state mutation. `build_path_tree` is a
//! pure transform; `draw_path_tree` writes its edit request into an
//! \&mut Option that the caller resolves.

use eframe::egui;

use crate::io::is_text_file;

#[derive(Default)]
pub(crate) struct PathTree {
    children: std::collections::BTreeMap<String, PathTree>,
    files: Vec<(String, String, String)>,
}

/// Build a `PathTree` from a flat list of `(abs_path, archive_path)` pairs.
pub(crate) fn build_path_tree(files: &[(String, String)]) -> PathTree {
    let mut root = PathTree::default();
    for (abs, archive) in files {
        // archive_path uses forward slashes (validate_bundle_path enforces it).
        let parts: Vec<&str> = archive.split('/').collect();
        let (dirs, file_name) = match parts.split_last() {
            Some((last, dirs)) => (dirs, last.to_string()),
            None => continue,
        };
        let mut node = &mut root;
        for d in dirs {
            if d.is_empty() {
                continue;
            }
            node = node.children.entry((*d).to_string()).or_default();
        }
        node.files.push((file_name, abs.clone(), archive.clone()));
    }
    root
}

/// Recursively render a `PathTree` as nested collapsing headers.
/// `edit_request` is set when the user clicks an edit button next to a file.
pub(crate) fn draw_path_tree(
    ui: &mut egui::Ui,
    tree: &PathTree,
    depth: usize,
    edit_request: &mut Option<(String, String)>,
) {
    // Render directories first, then loose files at this level.
    for (dir_name, child) in &tree.children {
        let id = ui.make_persistent_id(("pt_dir", depth, dir_name));
        egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, true)
            .show_header(ui, |ui| {
                let icon_h = ui.text_style_height(&egui::TextStyle::Body);
                let (icon_rect, _) =
                    ui.allocate_exact_size(egui::vec2(icon_h, icon_h), egui::Sense::hover());
                if ui.is_rect_visible(icon_rect) {
                    crate::panels::icons::paint_folder_icon(
                        &ui.painter_at(icon_rect),
                        icon_rect,
                        ui.visuals().text_color(),
                    );
                }
                ui.label(egui::RichText::new(dir_name).strong());
            })
            .body(|ui| {
                draw_path_tree(ui, child, depth + 1, edit_request);
            });
    }
    for (file_name, abs, archive) in &tree.files {
        ui.horizontal(|ui| {
            ui.label(file_name).on_hover_text(archive.as_str());
            // Right-align the edit button by filling the rest of the row.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if is_text_file(archive) {
                    let icon_size = egui::vec2(16.0, 16.0);
                    let (icon_rect, icon_resp) =
                        ui.allocate_exact_size(icon_size, egui::Sense::click());
                    if ui.is_rect_visible(icon_rect) {
                        if icon_resp.hovered() {
                            ui.painter_at(icon_rect).rect_filled(
                                icon_rect,
                                2.0,
                                ui.visuals().widgets.hovered.bg_fill,
                            );
                        }
                        let color = if icon_resp.hovered() {
                            ui.visuals().strong_text_color()
                        } else {
                            ui.visuals().text_color()
                        };
                        crate::panels::icons::paint_pencil_icon(
                            &ui.painter_at(icon_rect),
                            icon_rect,
                            color,
                        );
                    }
                    let clicked = icon_resp.clicked();
                    icon_resp.on_hover_text("Edit file");
                    if clicked {
                        *edit_request = Some((abs.clone(), archive.clone()));
                    }
                }
            });
        });
    }
}

/// Render a PassThrough node's file hierarchy directly onto the canvas using the painter.
/// Files are grouped by parent directory and clipped to the node body area.
pub(crate) fn draw_passthrough_body(
    painter: &egui::Painter,
    node_rect: egui::Rect,
    files: &[(String, String)],
    zoom: f32,
) {
    // node_rect is already scaled by zoom; the body insets, line
    // pitch, and font sizes scale by the same factor so the file
    // listing fills the node consistently at any zoom.
    let body_top = node_rect.min.y + 24.0 * zoom;
    let body_bottom = node_rect.max.y - 4.0 * zoom;
    let body_left = node_rect.min.x + 6.0 * zoom;
    let line_height = 13.0 * zoom;
    let font_size = 10.0 * zoom;
    let text_color = egui::Color32::from_rgb(190, 190, 190);
    let dir_color = egui::Color32::from_rgb(140, 190, 255);

    let clip_rect = egui::Rect::from_min_max(
        egui::pos2(node_rect.min.x, body_top),
        egui::pos2(node_rect.max.x, body_bottom),
    );
    let p = painter.with_clip_rect(clip_rect);

    if files.is_empty() {
        p.text(
            egui::pos2(body_left, body_top + 2.0 * zoom),
            egui::Align2::LEFT_TOP,
            "No files",
            egui::FontId::proportional(font_size),
            egui::Color32::GRAY,
        );
        return;
    }

    // Group files by parent directory (preserving stable order via BTreeMap)
    let mut dirs: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for (_, rel) in files {
        let path = std::path::Path::new(rel.as_str());
        let dir = path
            .parent()
            .map(|d| d.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| rel.clone());
        dirs.entry(dir).or_default().push(name);
    }

    let mut y = body_top;
    'outer: for (dir, names) in &dirs {
        if y + line_height > body_bottom {
            p.text(
                egui::pos2(body_left, y),
                egui::Align2::LEFT_TOP,
                "…",
                egui::FontId::monospace(font_size),
                text_color,
            );
            break;
        }
        if !dir.is_empty() {
            p.text(
                egui::pos2(body_left, y),
                egui::Align2::LEFT_TOP,
                format!("▸ {}/", dir),
                egui::FontId::monospace(font_size),
                dir_color,
            );
            y += line_height;
        }
        let indent = if dir.is_empty() {
            body_left
        } else {
            body_left + 8.0 * zoom
        };
        for name in names {
            if y + line_height > body_bottom {
                p.text(
                    egui::pos2(indent, y),
                    egui::Align2::LEFT_TOP,
                    "…",
                    egui::FontId::monospace(font_size),
                    text_color,
                );
                break 'outer;
            }
            p.text(
                egui::pos2(indent, y),
                egui::Align2::LEFT_TOP,
                name.as_str(),
                egui::FontId::monospace(font_size),
                text_color,
            );
            y += line_height;
        }
    }
}
