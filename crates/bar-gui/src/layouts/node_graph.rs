//! NodeGraph layout -- node graph editor with left palette, central
//! canvas, plus all floating windows (inspector, map info, etc.).
//!
//! No 3D viewport -- use the Sculpt3D layout, or a future split layout,
//! to view the terrain alongside the graph.
//!
//! The shell (menu bar, status bar, action bar, modals) is drawn by
//! `dispatch::draw_active` before this function is called, so this
//! module owns just the layout-specific panels: the left node-palette
//! sidebar (with validation summary footer) and the central canvas
//! (node graph + palette-drag ghost preview).

use bar_graph::NodeType;
use eframe::egui;

use crate::app::{
    build_io_outline, node_type_color, BarEditorApp, CanvasView, PaletteKind, IO_NODE_SIZE,
    IO_REF_H,
};
use crate::panels::icons::draw_io_icon;

pub fn draw(app: &mut BarEditorApp, ctx: &egui::Context, _frame: &mut eframe::Frame) {
    app.draw_node_palette_panel(ctx);
    app.draw_standard_central_panel(ctx);
}

impl BarEditorApp {
    pub(crate) fn draw_node_palette_panel(&mut self, ctx: &egui::Context) {
        if self.has_project() {
            egui::SidePanel::left("node_palette")
                .default_width(200.0)
                .show(ctx, |ui| {
                    // Validation summary anchors to the bottom of the
                    // sidebar; the node palette fills everything above it.
                    // Override the default panel frame so the summary's
                    // left/right padding lines up with the palette items
                    // above (default frame adds 8px asymmetric margins).
                    let frame = {
                        let mut f = egui::Frame::side_top_panel(ui.style());
                        f.inner_margin = egui::Margin {
                            left: 4,
                            right: 4,
                            top: 6,
                            bottom: 6,
                        };
                        f
                    };
                    egui::TopBottomPanel::bottom("validation_summary")
                        .resizable(false)
                        .frame(frame)
                        .show_inside(ui, |ui| {
                            self.draw_validation_summary(ui);
                        });
                    // The node palette is irrelevant inside a Layout
                    // edit view; swap it out for the editor's chrome
                    // (mode/symmetry/items/sidebar). The SidePanel
                    // container itself stays put, so its width and the
                    // palette's transient state (filter, scroll) are
                    // preserved across the swap.
                    if let CanvasView::NodeEdit(id) = self.current_view() {
                        self.draw_layout_editor_chrome(ui, id);
                    } else {
                        self.draw_node_palette(ui);
                    }
                });
        }
    }

    pub(crate) fn draw_standard_central_panel(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            self.draw_node_graph(ui);
        });

        // ── Palette drag: ghost preview + drop/cancel ────────────────────────────────
        // This runs AFTER all panels so the ghost paints on top of everything and
        // pointer position reflects the final state of the frame.

        if let Some(ref drag) = self.palette_drag {
            ctx.set_cursor_icon(egui::CursorIcon::Grabbing);

            if let Some(pos) = ctx.pointer_latest_pos() {
                let painter = ctx.layer_painter(egui::LayerId::new(
                    egui::Order::Tooltip,
                    egui::Id::new("palette_drag_ghost"),
                ));
                // IO nodes drop at their tag size so the ghost
                // matches; other node types use the generic 150×60
                // preview rect. Scaled by the canvas zoom so the ghost
                // matches the size the node will render at once dropped.
                let zoom = self.canvas.zoom;
                let is_io_input = matches!(drag.kind, PaletteKind::Node(NodeType::SubgraphInput));
                let is_io_output = matches!(drag.kind, PaletteKind::Node(NodeType::SubgraphOutput));
                let is_io = is_io_input || is_io_output;
                let ghost_size = (if is_io {
                    IO_NODE_SIZE
                } else {
                    egui::vec2(150.0, 60.0)
                }) * zoom;
                let ghost_rect =
                    egui::Rect::from_min_size(pos + egui::vec2(10.0, 10.0), ghost_size);
                let is_over_canvas =
                    self.canvas.rect_last.is_positive() && self.canvas.rect_last.contains(pos);
                let border_col = if is_over_canvas {
                    egui::Color32::from_rgba_unmultiplied(100, 200, 100, 220)
                } else {
                    egui::Color32::from_rgba_unmultiplied(220, 80, 80, 220)
                };

                if is_io {
                    // Match the on-canvas IO render: chevron-tipped
                    // tag with two-line text and a directional icon.
                    let h = ghost_rect.height();
                    let scale = h / IO_REF_H;
                    let chevron_w = h * 0.30;
                    let body_radius = (h / 6.0).min(ghost_rect.width() / 4.0);
                    let inner_pad = 6.0 * scale;
                    let icon_size = 48.0 * scale;
                    let icon_text_gap = 8.0 * scale;
                    let top_text_size = 18.0 * scale;
                    let bottom_text_size = 15.0 * scale;
                    let mid_y = ghost_rect.center().y;
                    let body_color = egui::Color32::from_rgba_unmultiplied(0x2F, 0x39, 0x45, 220);
                    let outline_pts =
                        build_io_outline(ghost_rect, chevron_w, body_radius, is_io_input);
                    painter.add(egui::Shape::convex_polygon(
                        outline_pts,
                        body_color,
                        egui::Stroke::new(1.5, border_col),
                    ));
                    let icon_rect = if is_io_input {
                        egui::Rect::from_min_size(
                            egui::pos2(ghost_rect.left() + inner_pad, mid_y - icon_size / 2.0),
                            egui::vec2(icon_size, icon_size),
                        )
                    } else {
                        egui::Rect::from_min_size(
                            egui::pos2(
                                ghost_rect.right() - inner_pad - icon_size,
                                mid_y - icon_size / 2.0,
                            ),
                            egui::vec2(icon_size, icon_size),
                        )
                    };
                    draw_io_icon(&painter, icon_rect, is_io_input);
                    let top_text = if is_io_input { "Input" } else { "Output" };
                    let bottom_text = "Heightmap";
                    let text_left = if is_io_input {
                        icon_rect.right() + icon_text_gap
                    } else {
                        ghost_rect.left() + chevron_w + inner_pad
                    };
                    let line_gap = 6.0 * scale;
                    let stack_h = top_text_size + line_gap + bottom_text_size;
                    let text_top = mid_y - stack_h / 2.0;
                    painter.text(
                        egui::pos2(text_left, text_top),
                        egui::Align2::LEFT_TOP,
                        top_text,
                        egui::FontId::proportional(top_text_size),
                        egui::Color32::from_rgb(0xE6, 0xED, 0xF3),
                    );
                    painter.text(
                        egui::pos2(text_left, text_top + top_text_size + line_gap),
                        egui::Align2::LEFT_TOP,
                        bottom_text,
                        egui::FontId::proportional(bottom_text_size),
                        egui::Color32::from_rgb(0x9A, 0xA7, 0xB2),
                    );
                } else {
                    // Generic node ghost: rect body + title bar.
                    painter.rect_filled(
                        ghost_rect,
                        4.0,
                        egui::Color32::from_rgba_unmultiplied(45, 50, 60, 200),
                    );
                    painter.rect_stroke(
                        ghost_rect,
                        4.0,
                        egui::Stroke::new(1.5, border_col),
                        egui::StrokeKind::Outside,
                    );
                    let title_rect = egui::Rect::from_min_size(
                        ghost_rect.min,
                        egui::vec2(ghost_rect.width(), 20.0 * zoom),
                    );
                    let title_color = match &drag.kind {
                        PaletteKind::Node(t) => node_type_color(t),
                        PaletteKind::Macro { .. } => egui::Color32::from_rgb(180, 90, 200),
                    };
                    painter.rect_filled(
                        title_rect,
                        egui::CornerRadius {
                            nw: 4,
                            ne: 4,
                            sw: 0,
                            se: 0,
                        },
                        title_color,
                    );
                    painter.text(
                        title_rect.center(),
                        egui::Align2::CENTER_CENTER,
                        &drag.label,
                        egui::FontId::proportional(12.0 * zoom),
                        egui::Color32::WHITE,
                    );
                }
                // Cancel hint when not over canvas
                if !is_over_canvas {
                    crate::panels::icons::paint_cancel_x(
                        &painter,
                        egui::pos2(ghost_rect.center().x, ghost_rect.center().y + 6.0),
                        7.0,
                        egui::Color32::from_rgb(220, 80, 80),
                    );
                }
            }

            ctx.request_repaint();
        }

        // Handle drop on primary pointer release
        let released = ctx.input(|i| i.pointer.primary_released());
        if released && self.palette_drag.is_some() {
            if let Some(drag) = self.palette_drag.take() {
                if let Some(pos) = ctx.pointer_latest_pos() {
                    if self.canvas.rect_last.is_positive() && self.canvas.rect_last.contains(pos) {
                        // Convert screen position → graph-space
                        // (accounts for canvas pan and zoom).
                        let drop_at = self.canvas.to_world(pos);
                        match drag.kind {
                            PaletteKind::Node(t) => {
                                self.add_node_at(t, &drag.label, drop_at);
                            }
                            PaletteKind::Macro { name } => {
                                self.instantiate_macro(&name, drop_at);
                            }
                        }
                    }
                    // else: released outside canvas → cancel
                }
            }
        }
    }
}
