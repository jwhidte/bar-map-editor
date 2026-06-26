//! Contextual properties surface -- the floating panel that pops up
//! after a brief hover gate when the user clicks a node, group, or
//! connection.
//!
//! `mod.rs` holds the panel lifecycle (`tick_props_panel`, hover-gate
//! resolution, positioning, click-outside dismissal) and the
//! dispatcher (`draw_properties`) that picks which per-NodeType body
//! to render. Each non-trivial node type gets its own file:
//!
//! - `pass_through` -- PassThrough file-list editor
//! - `group` -- group label / colour / collapse state +
//!   macro-parameter binding editor
//! - `painted_heightmap` -- PaintedHeightmap brush settings
//! - `painted_texture` -- PaintedTexture brush settings
//!
//! All of those add `impl BarEditorApp { ... }` blocks in their
//! respective files; methods stay on `BarEditorApp` rather than free
//! `pub(crate) fn draw(app, ...)` so the deep field access remains
//! clean -- `&mut self` already grants what's needed.

pub(crate) mod color_ramp;
pub(crate) mod group;
pub(crate) mod layout;
pub(crate) mod painted_heightmap;
pub(crate) mod painted_texture;
pub(crate) mod pass_through;
pub(crate) mod properties_canvas;
pub(crate) mod texture_weightmap;

use std::time::Instant;

use bar_graph::{self, NodeType, ParamValue};
use eframe::egui;

use crate::app::*;

/// A compact top-right close affordance for the contextual properties
/// popup. Draws an X from line segments (matching the canvas tab close
/// buttons) rather than a glyph, so it stays font-independent. Returns
/// true when clicked.
pub(crate) fn close_icon_button(ui: &mut egui::Ui) -> bool {
    let size = egui::vec2(18.0, 18.0);
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
    let color = if resp.hovered() {
        crate::panels::tokens::SEVERITY_ERROR
    } else {
        ui.visuals().weak_text_color()
    };
    let m = 5.0;
    let painter = ui.painter();
    painter.line_segment(
        [
            rect.left_top() + egui::vec2(m, m),
            rect.right_bottom() - egui::vec2(m, m),
        ],
        egui::Stroke::new(1.5, color),
    );
    painter.line_segment(
        [
            rect.right_top() + egui::vec2(-m, m),
            rect.left_bottom() + egui::vec2(m, -m),
        ],
        egui::Stroke::new(1.5, color),
    );
    resp.on_hover_text("Close").clicked()
}

impl BarEditorApp {
    pub(crate) fn tick_props_panel(&mut self, ctx: &egui::Context) {
        // ── Pending → active promotion ────────────────────────────────
        // The user clicked a node / group. Once the cursor has held
        // (mostly) still on top of the same target for the gate's
        // duration, the panel pops open.
        let now = Instant::now();
        let pointer = ctx.pointer_latest_pos();
        let target_now = pointer.and_then(|p| self.props_target_under_pointer(p));
        if let Some(pending) = self.dialog.pending_props_open.clone() {
            let elapsed = now.duration_since(pending.armed_at).as_millis() as u64;
            let drift = pointer
                .map(|p| p.distance(pending.armed_pos))
                .unwrap_or(f32::INFINITY);
            let still_on_target = target_now.as_ref() == Some(&pending.target);
            if !still_on_target || drift > PROPS_OPEN_MOVE_TOLERANCE {
                // User moved on (or clicked an action button etc.).
                self.dialog.pending_props_open = None;
            } else if elapsed >= PROPS_OPEN_DELAY_MS {
                self.props.active = Some(pending.target.clone());
                self.dialog.pending_props_open = None;
            } else {
                // Still inside the gate — request a repaint so we
                // come back and check again before the user has to
                // wiggle the mouse.
                ctx.request_repaint_after(std::time::Duration::from_millis(16));
            }
        }

        // ── Render ─────────────────────────────────────────────────────
        let Some(target) = self.props.active.clone() else {
            self.props.active_rect = None;
            return;
        };
        // Validate the target still exists; if it doesn't, drop the
        // panel cleanly.
        let target_rect = match self.props_target_screen_rect(&target) {
            Some(r) => r,
            None => {
                self.props.active = None;
                self.props.active_rect = None;
                return;
            }
        };

        // Estimate panel size up front so the position-finder can
        // pick a side that fits. The actual rendered size is captured
        // afterwards for the next frame's click-outside test.
        let est_size = egui::vec2(300.0, 360.0);
        let pos = self.position_props_panel(target_rect, est_size, ctx);

        let mut close_panel = false;
        let area_id = egui::Id::new(("contextual_props", target.id_hash()));
        let resp = egui::Area::new(area_id)
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .interactable(true)
            .movable(false)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style())
                    .fill(ui.visuals().window_fill)
                    .stroke(egui::Stroke::new(1.0, ui.visuals().window_stroke.color))
                    .inner_margin(egui::Margin::same(10))
                    .show(ui, |ui| {
                        ui.set_min_width(280.0);
                        ui.set_max_width(360.0);
                        ui.set_max_height(420.0);
                        egui::ScrollArea::vertical()
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                self.draw_properties_for(ui, &target);
                            });
                    });
            });
        let panel_rect = resp.response.rect;
        self.props.active_rect = Some(panel_rect);

        // The popup's own ✕ (or an action that supersedes the popup,
        // like entering a node's edit view) requests closure here.
        if std::mem::take(&mut self.props.close_requested) {
            close_panel = true;
        }

        // Click-outside-to-close. Only triggers on the press, not on
        // hold/drag, so dragging from inside the panel doesn't close
        // it. Also closes on Esc.
        let pressed = ctx.input(|i| i.pointer.any_pressed());
        let pointer = ctx.pointer_interact_pos();
        if pressed {
            if let Some(p) = pointer {
                let inside_panel = panel_rect.contains(p);
                // A press outside the panel that lands on the bare canvas
                // (background layer) is a genuine dismiss. One that lands
                // on another floating layer is not -- most importantly an
                // open ComboBox/menu popup opened from a widget *inside*
                // the panel, which egui renders in its own foreground area
                // outside the panel's rect. Without this guard, picking a
                // dropdown value would close the panel mid-edit.
                let on_background = ctx
                    .layer_id_at(p)
                    .is_none_or(|layer| layer.order == egui::Order::Background);
                if !inside_panel && on_background {
                    close_panel = true;
                }
            }
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            close_panel = true;
        }
        if close_panel {
            self.props.active = None;
            self.props.active_rect = None;
        }
    }

    /// Which target (if any) the cursor is currently hovering over.
    /// Used to decide whether the post-click hover gate is still
    /// pointing at the same thing it was armed against.
    pub(crate) fn props_target_under_pointer(&self, p: egui::Pos2) -> Option<PropsTarget> {
        // Collapsed SubGraph blocks render on top of the canvas and
        // hide their member nodes, so check them BEFORE walking
        // node_visuals — otherwise a hidden inner node at a
        // coincident position could intercept the hit.
        for (gid, rect) in &self.visuals.collapsed_subgraph_rects {
            if rect.contains(p) {
                return Some(PropsTarget::Group(*gid));
            }
        }
        // Skip nodes that aren't being rendered this frame. Without
        // this, hovering an inner subgraph node (which is hidden)
        // would erroneously match it as a Node target and reset the
        // hover gate that was armed for the SubGraph.
        let hidden = self.hidden_nodes_this_frame();
        for (id, visual) in &self.visuals.node_visuals {
            if hidden.contains(id) {
                continue;
            }
            let rect = egui::Rect::from_min_size(
                self.canvas.to_screen(visual.position),
                visual.size * self.canvas.zoom,
            );
            if rect.contains(p) {
                return Some(PropsTarget::Node(*id));
            }
        }
        // Plain groups (non-collapsed): header and body cached from
        // the previous draw_groups frame.
        for (gid, hr) in &self.visuals.group_header_rects {
            if hr.contains(p) {
                return Some(PropsTarget::Group(*gid));
            }
        }
        for (gid, br) in &self.visuals.group_body_rects {
            if br.contains(p) {
                return Some(PropsTarget::Group(*gid));
            }
        }
        None
    }

    /// Screen-space rect of the target the popup should anchor to.
    /// Returns `None` when the target no longer exists (e.g. the
    /// node was deleted while the popup was up — the popup then
    /// closes itself).
    pub(crate) fn props_target_screen_rect(&self, target: &PropsTarget) -> Option<egui::Rect> {
        match target {
            PropsTarget::Node(id) => {
                let v = self.visuals.node_visuals.get(id)?;
                Some(egui::Rect::from_min_size(
                    self.canvas.to_screen(v.position),
                    v.size * self.canvas.zoom,
                ))
            }
            PropsTarget::Group(gid) => {
                // Collapsed subgraph block rect first — when a group
                // is collapsed, its header / body rects don't exist.
                if let Some(r) = self.visuals.collapsed_subgraph_rects.get(gid) {
                    return Some(*r);
                }
                let h = self.visuals.group_header_rects.get(gid)?;
                let b = self.visuals.group_body_rects.get(gid)?;
                Some(h.union(*b))
            }
        }
    }

    /// Pick a position for the props panel near `target_rect`,
    /// preferring right > left > below > above. Falls back to the
    /// canvas's right margin when the panel doesn't fit anywhere
    /// without straddling a screen edge.
    pub(crate) fn position_props_panel(
        &self,
        target_rect: egui::Rect,
        size: egui::Vec2,
        ctx: &egui::Context,
    ) -> egui::Pos2 {
        let screen = ctx.screen_rect();
        let margin = 8.0;
        // Right
        if target_rect.right() + margin + size.x <= screen.right() {
            return egui::pos2(target_rect.right() + margin, target_rect.top());
        }
        // Left
        if target_rect.left() - margin - size.x >= screen.left() {
            return egui::pos2(target_rect.left() - margin - size.x, target_rect.top());
        }
        // Below
        if target_rect.bottom() + margin + size.y <= screen.bottom() {
            return egui::pos2(target_rect.left(), target_rect.bottom() + margin);
        }
        // Above
        if target_rect.top() - margin - size.y >= screen.top() {
            return egui::pos2(target_rect.left(), target_rect.top() - margin - size.y);
        }
        // Fallback: clamp to top-right of screen with a margin so
        // the panel is at least visible.
        egui::pos2(
            (screen.right() - size.x - margin).max(screen.left() + margin),
            screen.top() + margin,
        )
    }

    /// Render the contents of the contextual properties panel for
    /// the given target. Routes Node targets through the node
    /// properties UI and Group targets through the group properties
    /// UI — same rendering logic the right-side sidebar used to use.
    pub(crate) fn draw_properties_for(&mut self, ui: &mut egui::Ui, target: &PropsTarget) {
        match target {
            PropsTarget::Node(id) => {
                // Mirror the previous draw_properties node path: set
                // selected_node so the existing render code finds
                // the node's data.
                self.selection.node = Some(*id);
                self.draw_properties(ui);
            }
            PropsTarget::Group(gid) => {
                self.draw_group_properties(ui, *gid);
            }
        }
    }

    pub(crate) fn draw_properties(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        // Group selection takes priority over node selection (they're
        // mutually exclusive by invariant, but checking the group
        // first gives a clear ownership story).
        if let Some(gid) = self.selection.group {
            self.draw_group_properties(ui, gid);
            return;
        }
        if let Some(node_id) = self.selection.node {
            // Extract node data upfront so we don't hold a borrow on self.graph
            // while also needing to mutate other fields (e.g. passthrough_edit).
            let node_data = self
                .graph
                .get_node(node_id)
                .map(|n| (n.node_type.clone(), n.label.clone(), n.params.clone()));

            if let Some((node_type, node_label, node_params)) = node_data {
                let is_io = matches!(
                    node_type,
                    NodeType::SubgraphInput | NodeType::SubgraphOutput
                );

                // IO nodes don't carry a meaningful node-level
                // label or type — the visual itself already tells
                // the user it's an "Input" or "Output" of a given
                // kind. Showing the label edit and `Type:` line
                // here is just noise, so suppress both for IO
                // nodes. (`name` and `kind` params still render
                // below via the generic param editor.)
                let mut label_buf = node_label.clone();
                let mut label_changed = false;
                // Header row: name field fills the width, close ✕ sits
                // in the top-right corner. IO nodes skip the name field
                // (their visual already names them) but keep the ✕.
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if close_icon_button(ui) {
                            self.props.close_requested = true;
                        }
                        if !is_io {
                            let edit_resp = ui.add(
                                egui::TextEdit::singleline(&mut label_buf)
                                    .desired_width(f32::INFINITY)
                                    .font(egui::TextStyle::Heading),
                            );
                            crate::panels::widgets::select_all_on_focus(ui, &edit_resp, &label_buf);
                            label_changed = edit_resp.changed();
                        }
                    });
                });
                if !is_io {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Type:").weak());
                        ui.label(format!("{:?}", node_type));
                    });
                }

                let mut changed_params: Vec<(String, ParamValue)> = Vec::new();
                let mut new_label: Option<String> = None;
                if label_changed && label_buf != node_label {
                    new_label = Some(label_buf);
                }

                if node_type == NodeType::PassThrough {
                    ui.separator();
                    self.draw_passthrough_properties(ui, node_id, &node_params);
                } else if node_type == NodeType::PaintedHeightmap {
                    ui.separator();
                    self.draw_painted_heightmap_properties(ui, node_id, &node_params);
                } else if node_type == NodeType::PaintedTexture {
                    ui.separator();
                    self.draw_painted_texture_properties(ui, node_id, &node_params);
                } else if node_type == NodeType::TextureWeightmap {
                    ui.separator();
                    self.draw_texture_weightmap_properties(ui, node_id, &node_params);
                } else if node_type == NodeType::ColorRamp {
                    ui.separator();
                    self.draw_color_ramp_properties(ui, node_id, &node_params);
                } else if node_type == NodeType::Layout {
                    ui.separator();
                    self.draw_layout_properties(ui, node_id, &node_params);
                } else {
                    // Generic parameter editor — show every param the type
                    // declares, with sorted keys for stable layout.
                    let mut params_to_show: Vec<(String, ParamValue)> =
                        bar_graph::default_params(&node_type).into_iter().collect();
                    params_to_show.sort_by(|a, b| a.0.cmp(&b.0));

                    // IO node kind and name are system-managed; don't
                    // expose them as editable fields.
                    if matches!(
                        node_type,
                        NodeType::SubgraphInput | NodeType::SubgraphOutput
                    ) {
                        params_to_show.retain(|(k, _)| k != "kind" && k != "name");
                    }

                    // Only show the section when there's something to edit.
                    // Nodes with no configurable params (Invert, SlopeMap, etc.)
                    // would otherwise render two back-to-back separators.
                    if !params_to_show.is_empty() {
                        ui.separator();
                        egui::Grid::new(("params_grid", node_id.0))
                            .num_columns(2)
                            .spacing([8.0, 4.0])
                            .show(ui, |ui| {
                                for (key, default_val) in &params_to_show {
                                    let current = node_params.get(key).unwrap_or(default_val);
                                    match current {
                                        ParamValue::Float(v) => {
                                            let mut val = *v;
                                            ui.label(key);
                                            let changed = if let Some((mn, mx)) =
                                                bar_graph::param_float_range(&node_type, key)
                                            {
                                                ui.add(crate::panels::widgets::ParamSlider::new(
                                                    &mut val, mn, mx,
                                                ))
                                                .changed()
                                            } else {
                                                ui.add(egui::DragValue::new(&mut val).speed(0.01))
                                                    .changed()
                                            };
                                            if changed {
                                                changed_params
                                                    .push((key.clone(), ParamValue::Float(val)));
                                            }
                                            ui.end_row();
                                        }
                                        ParamValue::UInt(v) => {
                                            let mut val = *v as i32;
                                            ui.label(key);
                                            let changed = if let Some((mn, mx)) =
                                                bar_graph::param_uint_range(&node_type, key)
                                            {
                                                let mut vf = val as f32;
                                                let r = ui.add(
                                                    crate::panels::widgets::ParamSlider::new(
                                                        &mut vf, mn as f32, mx as f32,
                                                    )
                                                    .integer(),
                                                );
                                                val = vf as i32;
                                                r.changed()
                                            } else {
                                                ui.add(egui::DragValue::new(&mut val).range(1..=20))
                                                    .changed()
                                            };
                                            if changed {
                                                changed_params.push((
                                                    key.clone(),
                                                    ParamValue::UInt(val.max(0) as u32),
                                                ));
                                            }
                                            ui.end_row();
                                        }
                                        ParamValue::Int(v) => {
                                            let mut val = *v;
                                            ui.label(key);
                                            if ui.add(egui::DragValue::new(&mut val)).changed() {
                                                changed_params
                                                    .push((key.clone(), ParamValue::Int(val)));
                                            }
                                            ui.end_row();
                                        }
                                        ParamValue::Bool(v) => {
                                            let mut val = *v;
                                            ui.label("");
                                            if ui.checkbox(&mut val, key).changed() {
                                                changed_params
                                                    .push((key.clone(), ParamValue::Bool(val)));
                                            }
                                            ui.end_row();
                                        }
                                        ParamValue::String(v) => {
                                            let mut val = v.clone();
                                            ui.label(key);
                                            if let Some(choices) =
                                                bar_graph::param_choices(&node_type, key)
                                            {
                                                let id = ("param_choice", node_id.0, key.as_str());
                                                egui::ComboBox::from_id_salt(id)
                                                    .selected_text(&val)
                                                    .show_ui(ui, |ui| {
                                                        for choice in choices {
                                                            if ui
                                                                .selectable_label(
                                                                    val == *choice,
                                                                    *choice,
                                                                )
                                                                .clicked()
                                                            {
                                                                let prev = val.clone();
                                                                val = (*choice).to_string();
                                                                changed_params.push((
                                                                    key.clone(),
                                                                    ParamValue::String(val.clone()),
                                                                ));
                                                                if node_type
                                                                    == NodeType::AutoTexture
                                                                    && key == "biome"
                                                                    && val != prev
                                                                {
                                                                    let bd =
                                                                        bar_graph::biome_defaults(
                                                                            &val,
                                                                        );
                                                                    changed_params.push((
                                                                        "rock_color".to_string(),
                                                                        ParamValue::String(
                                                                            bd.rock_color
                                                                                .to_string(),
                                                                        ),
                                                                    ));
                                                                    changed_params.push((
                                                                        "slope_power".to_string(),
                                                                        ParamValue::Float(
                                                                            bd.slope_power,
                                                                        ),
                                                                    ));
                                                                }
                                                                let is_noise = matches!(
                                                                    node_type,
                                                                    NodeType::PerlinNoise
                                                                        | NodeType::SimplexNoise
                                                                        | NodeType::WorleyNoise
                                                                        | NodeType::RidgedNoise
                                                                );
                                                                if is_noise
                                                                    && key == "character"
                                                                    && val != prev
                                                                {
                                                                    let cd =
                                                                    bar_graph::character_defaults(
                                                                        &node_type, &val,
                                                                    );
                                                                    changed_params.push((
                                                                        "frequency".to_string(),
                                                                        ParamValue::Float(
                                                                            cd.frequency,
                                                                        ),
                                                                    ));
                                                                    changed_params.push((
                                                                        "octaves".to_string(),
                                                                        ParamValue::UInt(
                                                                            cd.octaves,
                                                                        ),
                                                                    ));
                                                                    changed_params.push((
                                                                        "lacunarity".to_string(),
                                                                        ParamValue::Float(
                                                                            cd.lacunarity,
                                                                        ),
                                                                    ));
                                                                    changed_params.push((
                                                                        "persistence".to_string(),
                                                                        ParamValue::Float(
                                                                            cd.persistence,
                                                                        ),
                                                                    ));
                                                                }
                                                            }
                                                        }
                                                    });
                                            } else if bar_graph::param_is_color(&node_type, key) {
                                                let rgb = parse_hex_color(&val)
                                                    .unwrap_or([128, 128, 128]);
                                                let mut c32 =
                                                    egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
                                                if ui.color_edit_button_srgba(&mut c32).changed() {
                                                    let new_hex = format!(
                                                        "{:02X}{:02X}{:02X}",
                                                        c32.r(),
                                                        c32.g(),
                                                        c32.b()
                                                    );
                                                    val = new_hex;
                                                    changed_params.push((
                                                        key.clone(),
                                                        ParamValue::String(val.clone()),
                                                    ));
                                                }
                                            } else {
                                                ui.horizontal(|ui| {
                                                    if ui
                                                        .add(
                                                            egui::TextEdit::singleline(&mut val)
                                                                .desired_width(f32::INFINITY),
                                                        )
                                                        .changed()
                                                    {
                                                        changed_params.push((
                                                            key.clone(),
                                                            ParamValue::String(val.clone()),
                                                        ));
                                                    }
                                                    if key == "path" && ui.button("…").clicked() {
                                                        if let Some(picked) =
                                                            make_path_dialog(self, &node_type)
                                                                .pick_file()
                                                        {
                                                            changed_params.push((
                                                                key.clone(),
                                                                ParamValue::String(
                                                                    picked
                                                                        .to_string_lossy()
                                                                        .to_string(),
                                                                ),
                                                            ));
                                                        }
                                                    }
                                                });
                                            }
                                            ui.end_row();
                                        }
                                        ParamValue::Vec2(_) => {}
                                        // Splines are only meaningful in a 2D canvas
                                        // editor (the Layout node has its own panel);
                                        // the generic property grid skips them rather
                                        // than try to surface raw point arrays.
                                        ParamValue::Spline(_) => {}
                                    }
                                }
                            });
                    } // end if !params_to_show.is_empty()
                } // end else (generic params branch)

                // Apply parameter changes
                if !changed_params.is_empty() {
                    self.push_undo("Change parameter");
                    if let Some(node) = self.graph.get_node_mut(node_id) {
                        for (key, value) in changed_params {
                            node.params.insert(key, value);
                        }
                        node.mark_dirty();
                    }
                }
                if let Some(new_label) = new_label {
                    self.push_undo("Rename node");
                    if let Some(node) = self.graph.get_node_mut(node_id) {
                        node.label = new_label;
                    }
                }

                ui.separator();
            } else {
                self.selection.node = None;
                ui.label("Select a node to edit properties.");
            }
        } else {
            ui.label("Select a node to edit properties.");
        }
    }
}
