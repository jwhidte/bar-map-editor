//! Group rendering, hit-test caches, and group create / add /
//! remove / dissolve. Distributed `impl BarEditorApp` block.
//!
//! Two render passes live here:
//! - `draw_groups` paints the translucent backdrop + label bar
//!   behind every (non-collapsed) group, populating
//!   `visuals.group_header_rects` / `visuals.group_body_rects` for
//!   the next frame's hit-testing.
//! - `draw_collapsed_subgraphs` paints the compact block that
//!   replaces a collapsed subgraph's contents, and the external
//!   ports on its perimeter.

use std::collections::HashMap;

use bar_graph::NodeId;
use eframe::egui;

use crate::app::*;
use crate::panels::canvas::{draw_port_circle, CollapsedSubgraphsDraw};
use crate::panels::tokens;
use crate::state::GroupRuntime;

impl BarEditorApp {
    /// Paint a labelled rectangle behind every group. The rect's
    /// bounding box is the union of the member nodes' rects, expanded
    /// by a margin so the group reads as a frame around them rather
    /// than touching their edges.
    pub(crate) fn draw_groups(&mut self, painter: &egui::Painter, offset: egui::Vec2, zoom: f32) {
        // Reset cached header/body rects each frame; we repopulate as
        // we draw. Hit-testing in the click pass below uses these.
        self.visuals.group_header_rects.clear();
        self.visuals.group_body_rects.clear();
        // On a SubGraph tab we don't draw any group decoration —
        // the canvas is showing only that subgraph's contents, so a
        // backdrop rectangle would be misleading.
        if matches!(self.current_view(), CanvasView::SubGraph(_)) {
            return;
        }
        let margin = 14.0_f32 * zoom;
        let header_h = 20.0_f32 * zoom;
        for (gid, group) in &self.visuals.groups {
            // Collapsed subgraphs draw as a compact block in a
            // separate pass after nodes (so they render on top, like
            // nodes themselves). Skip them in this backdrop pass.
            if group.is_subgraph && group.collapsed {
                continue;
            }
            // Compute union of member rects in canvas-screen space.
            let mut min: Option<egui::Pos2> = None;
            let mut max: Option<egui::Pos2> = None;
            for nid in &group.member_ids {
                let Some(visual) = self.visuals.node_visuals.get(nid) else {
                    continue;
                };
                let p0 = egui::pos2(
                    visual.position.x * zoom + offset.x,
                    visual.position.y * zoom + offset.y,
                );
                let p1 = egui::pos2(p0.x + visual.size.x * zoom, p0.y + visual.size.y * zoom);
                min = Some(match min {
                    Some(m) => egui::pos2(m.x.min(p0.x), m.y.min(p0.y)),
                    None => p0,
                });
                max = Some(match max {
                    Some(m) => egui::pos2(m.x.max(p1.x), m.y.max(p1.y)),
                    None => p1,
                });
            }
            let (Some(min), Some(max)) = (min, max) else {
                continue;
            };
            let rect = egui::Rect::from_min_max(
                egui::pos2(min.x - margin, min.y - margin - header_h),
                egui::pos2(max.x + margin, max.y + margin),
            );
            let tint = group_color(group.color_idx);
            let is_selected = self.selection.group == Some(*gid);
            // Translucent body so wires + nodes drawn after this still
            // read clearly.
            painter.rect_filled(
                rect,
                6.0,
                egui::Color32::from_rgba_unmultiplied(tint.r(), tint.g(), tint.b(), 32),
            );
            // Header band — opaque enough to read the label clearly.
            // Painted BEFORE the border so the border lands on top.
            let header_rect = egui::Rect::from_min_size(
                egui::pos2(rect.left(), rect.top()),
                egui::vec2(rect.width(), header_h),
            );
            painter.rect_filled(
                header_rect,
                egui::CornerRadius {
                    nw: 6,
                    ne: 6,
                    sw: 0,
                    se: 0,
                },
                egui::Color32::from_rgba_unmultiplied(tint.r(), tint.g(), tint.b(), 200),
            );
            // Border is painted last so the header fill never covers
            // it — keeps groups visually consistent with nodes (which
            // don't lose their border to the title block).
            // The selected style mirrors nodes: same blue at 1.5 px so
            // the user reads "this is the active selection" the same
            // way regardless of what kind of thing is selected.
            let stroke = if is_selected {
                egui::Stroke::new(1.5, tokens::NODE_BORDER_SEL)
            } else {
                egui::Stroke::new(1.5, tint.gamma_multiply(0.9))
            };
            painter.rect_stroke(rect, 6.0, stroke, egui::StrokeKind::Outside);
            let label_text = if group.label.is_empty() {
                format!("Group {gid}")
            } else {
                group.label.clone()
            };
            painter.text(
                egui::pos2(rect.left() + 8.0 * zoom, header_rect.center().y),
                egui::Align2::LEFT_CENTER,
                label_text,
                egui::FontId::proportional(11.5 * zoom),
                egui::Color32::WHITE,
            );
            // Body rect = full minus header, for click hit-testing.
            let body_rect =
                egui::Rect::from_min_max(egui::pos2(rect.left(), header_rect.bottom()), rect.max);
            self.visuals.group_header_rects.insert(*gid, header_rect);
            self.visuals.group_body_rects.insert(*gid, body_rect);
        }
    }

    /// Create a new empty group and return its id. The caller is
    /// expected to push undo BEFORE this if it represents a discrete
    /// user action; bulk operations (e.g. "create group from
    /// selection") push once at the call site.
    pub(crate) fn create_group(&mut self, label: impl Into<String>) -> u64 {
        let id = self.visuals.alloc_group_id();
        let color_idx = (id as u8) % (GROUP_PALETTE.len() as u8);
        self.visuals.groups.insert(
            id,
            GroupRuntime {
                label: label.into(),
                member_ids: std::collections::HashSet::new(),
                color_idx,
                collapsed: false,
                is_subgraph: false,
                subgraph_inputs: Vec::new(),
                subgraph_outputs: Vec::new(),
                macro_params: Vec::new(),
            },
        );
        self.project.is_dirty = true;
        id
    }

    /// Add a node to a group. Removes it from any previous group first
    /// (a node can only live in one group at a time — same as folder
    /// membership in a filesystem). Caller is responsible for the
    /// `push_undo` if the move should be undoable on its own; the
    /// helper itself doesn't push so callers that perform bulk moves
    /// ("group selection of N nodes") only push once.
    pub(crate) fn add_node_to_group(&mut self, node_id: NodeId, group_id: u64) {
        if let Some(prev) = self.visuals.node_to_group.get(&node_id).copied() {
            if prev == group_id {
                return;
            }
            if let Some(g) = self.visuals.groups.get_mut(&prev) {
                g.member_ids.remove(&node_id);
            }
        }
        if let Some(g) = self.visuals.groups.get_mut(&group_id) {
            g.member_ids.insert(node_id);
            self.visuals.node_to_group.insert(node_id, group_id);
            self.project.is_dirty = true;
        }
    }

    /// Remove a node from its group (if any). If that empties the
    /// group, the group is deleted to avoid orphaned empty rectangles
    /// piling up. Same caller-pushes-undo contract as
    /// `add_node_to_group`.
    pub(crate) fn remove_node_from_group(&mut self, node_id: NodeId) {
        let Some(group_id) = self.visuals.node_to_group.remove(&node_id) else {
            return;
        };
        let mut delete = false;
        if let Some(g) = self.visuals.groups.get_mut(&group_id) {
            g.member_ids.remove(&node_id);
            delete = g.member_ids.is_empty();
        }
        if delete {
            self.visuals.groups.remove(&group_id);
        }
        self.project.is_dirty = true;
    }

    /// Dissolve a group entirely (members keep their positions, just
    /// lose group membership). Caller-pushes-undo as above.
    pub(crate) fn dissolve_group(&mut self, group_id: u64) {
        let Some(g) = self.visuals.groups.remove(&group_id) else {
            return;
        };
        for nid in &g.member_ids {
            self.visuals.node_to_group.remove(nid);
        }
        self.project.is_dirty = true;
    }
    /// Layout-only computation of every collapsed subgraph's rect and
    /// the screen-space position of each of its external port handles.
    /// Called BEFORE wire rendering so the wire pass can reroute
    /// hidden inner endpoints through the visible external port.
    /// Cheap — no painting, no allocation beyond the result maps.
    pub(crate) fn collapsed_subgraph_layout(
        &self,
        offset: egui::Vec2,
        zoom: f32,
    ) -> (
        HashMap<u64, egui::Rect>,
        HashMap<(NodeId, String), egui::Pos2>,
    ) {
        let mut rects = HashMap::new();
        let mut handles: HashMap<(NodeId, String), egui::Pos2> = HashMap::new();
        if matches!(self.current_view(), CanvasView::SubGraph(_)) {
            return (rects, handles);
        }
        // Block geometry is fixed in world units, scaled to screen by
        // zoom — must match `draw_collapsed_subgraphs` exactly so the
        // rerouted-wire handle positions land on the painted ports.
        let block_w = 180.0_f32 * zoom;
        let header_h = 22.0_f32 * zoom;
        let row_h = 18.0_f32 * zoom;
        let port_inset = 8.0_f32 * zoom;
        for (gid, group) in &self.visuals.groups {
            if !(group.is_subgraph && group.collapsed) {
                continue;
            }
            let mut cx = 0.0_f32;
            let mut cy = 0.0_f32;
            let mut n = 0_f32;
            for nid in &group.member_ids {
                if let Some(v) = self.visuals.node_visuals.get(nid) {
                    cx += (v.position.x + v.size.x * 0.5) * zoom + offset.x;
                    cy += (v.position.y + v.size.y * 0.5) * zoom + offset.y;
                    n += 1.0;
                }
            }
            let centre = if n > 0.0 {
                egui::pos2(cx / n, cy / n)
            } else {
                egui::pos2(300.0, 200.0)
            };
            let rows = group
                .subgraph_inputs
                .len()
                .max(group.subgraph_outputs.len());
            let block_h = header_h + (rows.max(1) as f32) * row_h + 10.0 * zoom;
            let rect = egui::Rect::from_min_size(
                egui::pos2(centre.x - block_w * 0.5, centre.y - block_h * 0.5),
                egui::vec2(block_w, block_h),
            );
            for (i, port) in group.subgraph_inputs.iter().enumerate() {
                let y = rect.top() + header_h + port_inset + i as f32 * row_h;
                let p = egui::pos2(rect.left(), y);
                if let Some((nid, pname)) = &port.binding {
                    handles.insert((*nid, pname.clone()), p);
                }
            }
            for (i, port) in group.subgraph_outputs.iter().enumerate() {
                let y = rect.top() + header_h + port_inset + i as f32 * row_h;
                let p = egui::pos2(rect.right(), y);
                if let Some((nid, pname)) = &port.binding {
                    handles.insert((*nid, pname.clone()), p);
                }
            }
            rects.insert(*gid, rect);
        }
        (rects, handles)
    }

    /// Returns `(per-group rect, bound-inner-port → external-handle-pos)`
    /// for every collapsed subgraph drawn this frame. The handle map
    /// feeds two things: visual rerouting of wires whose endpoints are
    /// hidden inner nodes, and (future) wire creation at subgraph
    /// external ports.
    pub(crate) fn draw_collapsed_subgraphs(
        &mut self,
        ui: &mut egui::Ui,
        offset: egui::Vec2,
        zoom: f32,
    ) -> CollapsedSubgraphsDraw {
        // Reset the cached collapsed-block rects every frame; we
        // refill below as each block is drawn so the props-popup
        // hit-test sees current positions.
        self.visuals.collapsed_subgraph_rects.clear();
        let mut rects = HashMap::new();
        // Bound inner port → external handle position. Used by the
        // wire-render pass below to reroute connections from hidden
        // inner endpoints onto the visible external port.
        let mut handle_positions: HashMap<(NodeId, String), egui::Pos2> = HashMap::new();
        let mut conn_start: Option<DragConnection> = None;
        let mut conn_end: Option<(NodeId, String)> = None;
        if matches!(self.current_view(), CanvasView::SubGraph(_)) {
            return (rects, handle_positions, conn_start, conn_end);
        }
        let painter = ui.painter().clone();
        let (sg_border_default, sg_border_sel, sg_label_col) = {
            let vis = ui.visuals();
            let towards = if vis.dark_mode {
                egui::Color32::WHITE
            } else {
                egui::Color32::BLACK
            };
            (
                vis.window_fill().lerp_to_gamma(towards, 0.35),
                tokens::NODE_BORDER_SEL,
                vis.strong_text_color(),
            )
        };
        // Must mirror `collapsed_subgraph_layout` exactly (scaled by
        // zoom) so the wire-reroute handles align with these ports.
        let block_w = 180.0_f32 * zoom;
        let header_h = 22.0_f32 * zoom;
        let row_h = 18.0_f32 * zoom;
        let port_inset = 8.0_f32 * zoom;
        for (gid, group) in &self.visuals.groups {
            if !(group.is_subgraph && group.collapsed) {
                continue;
            }
            // Centroid of members in canvas-screen space.
            let mut cx = 0.0_f32;
            let mut cy = 0.0_f32;
            let mut n = 0_f32;
            for nid in &group.member_ids {
                if let Some(v) = self.visuals.node_visuals.get(nid) {
                    cx += (v.position.x + v.size.x * 0.5) * zoom + offset.x;
                    cy += (v.position.y + v.size.y * 0.5) * zoom + offset.y;
                    n += 1.0;
                }
            }
            let centre = if n > 0.0 {
                egui::pos2(cx / n, cy / n)
            } else {
                egui::pos2(300.0, 200.0)
            };
            let rows = group
                .subgraph_inputs
                .len()
                .max(group.subgraph_outputs.len());
            let block_h = header_h + (rows.max(1) as f32) * row_h + 10.0 * zoom;
            let rect = egui::Rect::from_min_size(
                egui::pos2(centre.x - block_w * 0.5, centre.y - block_h * 0.5),
                egui::vec2(block_w, block_h),
            );
            let tint = group_color(group.color_idx);
            // Body — opaque so it reads as a node, not a translucent
            // backdrop.
            painter.rect_filled(
                rect,
                6.0,
                egui::Color32::from_rgba_unmultiplied(tint.r(), tint.g(), tint.b(), 230),
            );
            // Header band.
            let header_rect =
                egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), header_h));
            painter.rect_filled(
                header_rect,
                egui::CornerRadius {
                    nw: 6,
                    ne: 6,
                    sw: 0,
                    se: 0,
                },
                tint.gamma_multiply(0.7),
            );
            let label_text = if group.label.is_empty() {
                format!("SubGraph {gid}")
            } else {
                group.label.clone()
            };
            painter.text(
                egui::pos2(rect.left() + 10.0 * zoom, header_rect.center().y),
                egui::Align2::LEFT_CENTER,
                label_text,
                egui::FontId::proportional(12.0 * zoom),
                egui::Color32::WHITE,
            );
            // Border last so the header doesn't cover it (matches
            // node + group rendering).
            let is_selected = self.selection.group == Some(*gid);
            let stroke = if is_selected {
                egui::Stroke::new(1.5, sg_border_sel)
            } else {
                egui::Stroke::new(1.5, sg_border_default)
            };
            painter.rect_stroke(rect, 6.0, stroke, egui::StrokeKind::Outside);
            // Port handles + labels. Inputs on the left, outputs on
            // the right. The actual wiring of these handles to the
            // surrounding graph lands in the next phase along with
            // subgraph eval.
            let hit_size = egui::Vec2::splat((14.0 * zoom).max(10.0));
            for (i, port) in group.subgraph_inputs.iter().enumerate() {
                let y = rect.top() + header_h + port_inset + i as f32 * row_h;
                let p = egui::pos2(rect.left(), y);
                let port_resp = ui.interact(
                    egui::Rect::from_center_size(p, hit_size),
                    egui::Id::new(("subgraph_port_in", *gid, i as u32)),
                    egui::Sense::click_and_drag(),
                );
                draw_port_circle(
                    &painter,
                    p,
                    4.0 * zoom,
                    tokens::PORT_HEIGHTMAP,
                    port_resp.hovered(),
                );
                painter.text(
                    egui::pos2(p.x + 8.0 * zoom, p.y),
                    egui::Align2::LEFT_CENTER,
                    &port.label,
                    egui::FontId::proportional(11.0 * zoom),
                    sg_label_col,
                );
                if let Some((nid, pname)) = &port.binding {
                    handle_positions.insert((*nid, pname.clone()), p);
                    if self.canvas.drag_connection.is_some()
                        && ui.input(|inp| inp.pointer.primary_released())
                        && port_resp.contains_pointer()
                    {
                        conn_end = Some((*nid, pname.clone()));
                    }
                }
            }
            for (i, port) in group.subgraph_outputs.iter().enumerate() {
                let y = rect.top() + header_h + port_inset + i as f32 * row_h;
                let p = egui::pos2(rect.right(), y);
                let port_resp = ui.interact(
                    egui::Rect::from_center_size(p, hit_size),
                    egui::Id::new(("subgraph_port_out", *gid, i as u32)),
                    egui::Sense::click_and_drag(),
                );
                draw_port_circle(
                    &painter,
                    p,
                    4.0 * zoom,
                    tokens::PORT_HEIGHTMAP,
                    port_resp.hovered(),
                );
                painter.text(
                    egui::pos2(p.x - 8.0 * zoom, p.y),
                    egui::Align2::RIGHT_CENTER,
                    &port.label,
                    egui::FontId::proportional(11.0 * zoom),
                    sg_label_col,
                );
                if let Some((nid, pname)) = &port.binding {
                    handle_positions.insert((*nid, pname.clone()), p);
                    if port_resp.drag_started_by(egui::PointerButton::Primary)
                        && self.canvas.drag_connection.is_none()
                    {
                        conn_start = Some(DragConnection {
                            from_node: *nid,
                            from_port: pname.clone(),
                            from_pos: p,
                        });
                    }
                }
            }
            rects.insert(*gid, rect);
            self.visuals.collapsed_subgraph_rects.insert(*gid, rect);
        }
        (rects, handle_positions, conn_start, conn_end)
    }
}

use bar_graph::{Node, NodeType};

use crate::app::IO_NODE_SIZE;
use crate::editor::{CanvasView, PropsTarget};
use crate::state::NodeVisual;

impl BarEditorApp {
    pub(crate) fn instantiate_macro(&mut self, macro_name: &str, pos: egui::Pos2) {
        let Some(template) = crate::macros::parse(macro_name) else {
            self.dialog.status_message = Some(format!("Macro '{macro_name}' not found"));
            return;
        };
        self.push_undo(&format!("Drop macro '{}'", template.name));
        // Compute the numbered wrapper label BEFORE instantiation so
        // we don't double-count nodes about to be added.
        let numbered_label = self.next_label_for(&template.name);
        let mut inst = match crate::macros::instantiate(&template, &mut self.graph, pos) {
            Ok(i) => i,
            Err(e) => {
                self.dialog.status_message =
                    Some(format!("Macro '{macro_name}' failed to instantiate: {e}"));
                return;
            }
        };
        inst.group.label = numbered_label;
        for (id, visual) in inst.visuals {
            self.visuals.node_visuals.insert(id, visual);
        }
        let gid = self.visuals.alloc_group_id();
        for nid in &inst.member_ids {
            self.visuals.node_to_group.insert(*nid, gid);
        }
        self.visuals.groups.insert(gid, inst.group);
        self.select_group(gid);
        // Same direct-open as `add_node_at` — drop a macro, see its
        // properties immediately so you can tweak the parameters
        // without a separate click + hover.
        self.props.active = Some(PropsTarget::Group(gid));
        self.dialog.pending_props_open = None;
        self.project.is_dirty = true;
        self.dialog.status_message = Some(format!("Dropped '{}' onto the canvas.", template.name));
    }

    pub(crate) fn add_node_at(&mut self, node_type: NodeType, label: &str, pos: egui::Pos2) {
        self.push_undo("Add node");
        let numbered = self.next_label_for(label);
        let node = Node::new(NodeId(0), node_type.clone(), numbered);
        let id = self.graph.add_node(node);
        // A freshly-dropped node is "what the user wants to look at"
        // — open the contextual properties panel immediately
        // without waiting for the hover gate. Skipping the gate is
        // intentional: there's no ambiguity here (no "did they
        // mean to drag instead?"), the node is the click result.
        self.props.active = Some(PropsTarget::Node(id));
        self.dialog.pending_props_open = None;
        let default_size = match node_type {
            NodeType::PassThrough => egui::vec2(180.0, 200.0),
            NodeType::FinalComposition => egui::vec2(210.0, 240.0),
            NodeType::SubgraphInput | NodeType::SubgraphOutput => IO_NODE_SIZE,
            // Start at the correct height for its default port count (2 inputs).
            NodeType::TextureWeightmap => egui::vec2(150.0, PORT_Y_BASE + 2.0 * PORT_Y_STEP + 10.0),
            _ => egui::vec2(150.0, 80.0),
        };
        self.visuals.node_visuals.insert(
            id,
            NodeVisual {
                position: pos,
                size: default_size,
            },
        );
        self.selection.node = Some(id);
        // If the user is viewing a subgraph tab when they drop a
        // node, the drop goes INTO that subgraph: add it to the
        // group's member set so `hidden_nodes_this_frame` (which
        // hides everything outside the active subgraph in the
        // subgraph view) doesn't immediately hide it. Without this,
        // the new node lives at the top level of the graph and
        // becomes invisible the moment it's dropped — properties
        // panel opens on a node the user can't see.
        if let Some(CanvasView::SubGraph(scope)) =
            self.canvas.tabs.get(self.canvas.active_tab).cloned()
        {
            if let Some(group) = self.visuals.groups.get_mut(&scope) {
                group.member_ids.insert(id);
                self.visuals.node_to_group.insert(id, scope);
            }
        }
    }
}
