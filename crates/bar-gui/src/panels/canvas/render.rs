//! Main per-frame canvas render + interaction. Distributed
//! `impl BarEditorApp` block.
//!
//! `draw_node_graph` is the centerpiece -- it does, in order,
//! per frame:
//!   1. Cache the current canvas rect for next frame's drag-drop
//!      hit tests.
//!   2. Reset the per-frame collapsed-subgraph rect cache.
//!   3. Allocate the canvas painter; paint group backdrops first so
//!      they sit under nodes.
//!   4. Draw collapsed subgraph blocks (atop groups, beneath nodes).
//!   5. Draw nodes (body + ports + selected outline).
//!   6. Draw wires and the in-progress drag connection.
//!   7. Process input: hit-tests, click dispatch, drag start /
//!      finish, marquee selection, port drag, palette drop.
//!   8. Apply any deferred mutations gathered above.
//!
//! Several of the big intra-method blocks would benefit from being
//! lifted into helpers (or their own files); that's a follow-up
//! after the structural split has settled.

use std::time::Instant;

use bar_graph::{NodeId, NodeType, ParamValue, PortId, PortPlacement};
use eframe::egui;

use crate::app::*;
use crate::panels::canvas::{draw_port_circle, NodeStyle};
use crate::panels::tokens;
use crate::t;

impl BarEditorApp {
    pub(crate) fn draw_node_graph(&mut self, ui: &mut egui::Ui) {
        // Tab bar — clean up first so we don't render tabs for
        // sub-graphs / nodes that no longer exist. The tab strip
        // draws its own baseline separator that the active tab
        // visually breaks, so no extra ui.separator() is needed.
        self.prune_dangling_tabs();
        self.draw_canvas_tabs(ui);

        // Pressing Escape on a non-Main tab returns to Main without
        // closing the tab — same speed-back affordance the old
        // confined-edit Esc had, but you don't lose the open tab.
        if self.canvas.active_tab != 0 && ui.ctx().input(|i| i.key_pressed(egui::Key::Escape)) {
            self.set_active_tab(0);
        }
        // Ctrl/Cmd+W closes the current tab. Main is unclosable; the
        // shortcut is a no-op when Main is active.
        if ui
            .ctx()
            .input(|i| (i.modifiers.ctrl || i.modifiers.command) && i.key_pressed(egui::Key::W))
            && self.canvas.active_tab != 0
        {
            self.close_tab(self.canvas.active_tab);
        }
        // Ctrl/Cmd+Tab swaps with the previously-active tab — the
        // standard "back to where I was" shortcut. Skipped when
        // there's only one tab (or `last_active_tab` is the same).
        if ui
            .ctx()
            .input(|i| (i.modifiers.ctrl || i.modifiers.command) && i.key_pressed(egui::Key::Tab))
        {
            let target = self.canvas.last_active_tab;
            if target != self.canvas.active_tab && target < self.canvas.tabs.len() {
                self.set_active_tab(target);
            }
        }

        // Node-edit view replaces the graph body with a bespoke
        // full-area editor for one node. The tab strip above stays so
        // the user can switch / close back to the graph.
        if let CanvasView::NodeEdit(id) = self.current_view() {
            self.draw_layout_editor(ui, id);
            return;
        }

        let available = ui.available_size();
        let (canvas_rect, response) =
            ui.allocate_exact_size(available, egui::Sense::click_and_drag());
        self.canvas.rect_last = canvas_rect;

        // Drain any layout that was deferred from the welcome panel
        // (template card click, File → New from Preset). The
        // viewport-pin path inside `auto_layout_selection` reads
        // `canvas_rect_last`, which is now fresh — so the layout
        // lands on screen instead of off-canvas.
        if self.canvas.pending_auto_layout_all {
            self.canvas.pending_auto_layout_all = false;
            self.auto_layout_selection();
        }

        let painter = ui.painter_at(canvas_rect);

        // Canvas → screen transform for this frame. `offset` (pan) and
        // `zoom` are captured once so every node / port / wire / group
        // position is mapped consistently: `screen = world * zoom +
        // offset`. The `to_screen` closure borrows only these Copy
        // locals, so it can be used freely inside the borrow-heavy
        // draw loops below without touching `self`.
        let offset = self.canvas.offset;
        let zoom = self.canvas.zoom;
        let to_screen =
            |world: egui::Pos2| egui::pos2(world.x * zoom + offset.x, world.y * zoom + offset.y);

        // Draw grid. Spacing scales with zoom so the backdrop reads as
        // part of the zoomed content rather than a fixed overlay.
        let grid_spacing = 30.0 * zoom;
        let grid_color = ui
            .visuals()
            .widgets
            .noninteractive
            .bg_stroke
            .color
            .linear_multiply(0.2);

        let grid_offset_x = offset.x % grid_spacing;
        let grid_offset_y = offset.y % grid_spacing;

        let mut x = canvas_rect.left() + grid_offset_x;
        while x <= canvas_rect.right() {
            painter.line_segment(
                [
                    egui::pos2(x, canvas_rect.top()),
                    egui::pos2(x, canvas_rect.bottom()),
                ],
                egui::Stroke::new(1.0, grid_color),
            );
            x += grid_spacing;
        }
        let mut y = canvas_rect.top() + grid_offset_y;
        while y <= canvas_rect.bottom() {
            painter.line_segment(
                [
                    egui::pos2(canvas_rect.left(), y),
                    egui::pos2(canvas_rect.right(), y),
                ],
                egui::Stroke::new(1.0, grid_color),
            );
            y += grid_spacing;
        }

        // Handle canvas panning with middle-click or right-click drag
        if response.dragged_by(egui::PointerButton::Secondary)
            || response.dragged_by(egui::PointerButton::Middle)
        {
            self.canvas.offset += response.drag_delta();
        }

        // Scroll-wheel zoom and `0`-to-reset, both anchored on the
        // viewport centre so the graph scales symmetrically in place
        // rather than drifting toward the cursor or a stale pan. A
        // per-notch factor clamped to a gentle range keeps perceived
        // speed even across zoom levels. The new zoom takes effect next
        // frame (same one-frame lag as the pan above, which captured
        // `offset`/`zoom` before this block).
        if response.hovered() {
            let centre = canvas_rect.center();
            let scroll = ui.ctx().input(|i| i.smooth_scroll_delta.y);
            if scroll.abs() > 0.0 {
                let factor = (1.0 + scroll * 0.0015).clamp(0.7, 1.4);
                self.canvas.zoom_to(centre, self.canvas.zoom * factor);
            }
            if ui.ctx().input(|i| i.key_pressed(egui::Key::Num0)) {
                self.canvas.zoom_to(centre, 1.0);
            }
        }

        // Click on empty space to deselect (suppress when palette drag
        // is in progress, and skip if the click landed on a group
        // header / body — handled separately below).
        let pointer = ui.ctx().pointer_latest_pos();
        let clicked_in_group = pointer.is_some_and(|p| {
            self.visuals
                .group_header_rects
                .values()
                .any(|r| r.contains(p))
                || self
                    .visuals
                    .group_body_rects
                    .values()
                    .any(|r| r.contains(p))
        });
        if response.clicked() && self.palette_drag.is_none() && !clicked_in_group {
            self.clear_selection();
        }

        // ── Marquee selection ──────────────────────────────────────────────
        // Primary-button drag on the empty canvas (i.e. not landing on
        // a node, group, or palette item) sweeps out a rectangle; on
        // release every node whose rect intersects the rectangle is
        // selected. Ctrl/Cmd makes the marquee additive (added to the
        // current selection rather than replacing it).
        if response.drag_started_by(egui::PointerButton::Primary)
            && self.palette_drag.is_none()
            && !clicked_in_group
        {
            if let Some(p) = pointer {
                self.canvas.marquee_start = Some(p);
            }
        }
        if let Some(anchor) = self.canvas.marquee_start {
            let cur = pointer.unwrap_or(anchor);
            let marquee = egui::Rect::from_two_pos(anchor, cur);
            painter.rect_filled(marquee, 2.0, tokens::NODE_BORDER_SEL.gamma_multiply(0.11));
            painter.rect_stroke(
                marquee,
                2.0,
                egui::Stroke::new(1.0, tokens::NODE_BORDER_SEL),
                egui::StrokeKind::Inside,
            );
            // On drag end, finalize the selection.
            if !ui
                .ctx()
                .input(|i| i.pointer.button_down(egui::PointerButton::Primary))
            {
                let additive = ui.ctx().input(|i| i.modifiers.ctrl || i.modifiers.command);
                let mut hits: Vec<NodeId> = Vec::new();
                // Subgraph membership set: nodes inside a is_subgraph group.
                // Used to prevent marquee from crossing the main/subgraph boundary.
                let subgraph_members: std::collections::HashSet<NodeId> = self
                    .visuals
                    .groups
                    .values()
                    .filter(|g| g.is_subgraph)
                    .flat_map(|g| g.member_ids.iter().copied())
                    .collect();
                for (id, visual) in &self.visuals.node_visuals {
                    let in_scope = match self.current_view() {
                        CanvasView::Main => !subgraph_members.contains(id),
                        CanvasView::SubGraph(gid) => self
                            .visuals
                            .groups
                            .get(&gid)
                            .is_some_and(|g| g.member_ids.contains(id)),
                        // The node-edit view replaces the graph body
                        // entirely; no graph nodes are in scope.
                        CanvasView::NodeEdit(_) => false,
                    };
                    if !in_scope {
                        continue;
                    }
                    let r =
                        egui::Rect::from_min_size(to_screen(visual.position), visual.size * zoom);
                    if marquee.intersects(r) {
                        hits.push(*id);
                    }
                }

                // Also pick up subgraphs / groups whose visible rect
                // intersects the marquee. Collapsed subgraphs have
                // no member-node visuals to catch (their innards are
                // hidden behind the compact block) so without this
                // pass the marquee couldn't select them at all.
                // Expanded groups are caught here too — selecting a
                // group whose body the user dragged over reads as
                // "I want this whole chunk", regardless of whether
                // the marquee also clipped each individual member.
                let mut group_hits: Vec<u64> = Vec::new();
                // Collapsed subgraphs: cached rect IS the whole
                // visible footprint.
                for (gid, rect) in &self.visuals.collapsed_subgraph_rects {
                    if marquee.intersects(*rect) {
                        group_hits.push(*gid);
                    }
                }
                // Expanded groups: union of header + body rects from
                // the previous draw_groups frame.
                for gid in self.visuals.groups.keys() {
                    if self.visuals.collapsed_subgraph_rects.contains_key(gid) {
                        continue;
                    }
                    let header = self.visuals.group_header_rects.get(gid).copied();
                    let body = self.visuals.group_body_rects.get(gid).copied();
                    let group_rect = match (header, body) {
                        (Some(h), Some(b)) => Some(h.union(b)),
                        (Some(h), None) => Some(h),
                        (None, Some(b)) => Some(b),
                        (None, None) => None,
                    };
                    if let Some(r) = group_rect {
                        if marquee.intersects(r) {
                            group_hits.push(*gid);
                        }
                    }
                }

                if !additive {
                    self.selection.nodes.clear();
                    self.selection.node = None;
                }
                // Single-group-selection model: if the marquee hit
                // exactly one group, that's the active group; if it
                // hit several, leave selected_group cleared (the
                // properties popup can't disambiguate). Either way,
                // every hit group's members are added to the node
                // selection so subsequent operations (move, delete)
                // act on the whole chunk.
                self.selection.group = match group_hits.len() {
                    1 => Some(group_hits[0]),
                    _ => None,
                };
                for gid in &group_hits {
                    if let Some(group) = self.visuals.groups.get(gid) {
                        for id in &group.member_ids {
                            self.selection.nodes.insert(*id);
                            hits.push(*id);
                        }
                    }
                }
                for id in &hits {
                    self.selection.nodes.insert(*id);
                }
                if self.selection.node.is_none() {
                    self.selection.node = hits.first().copied();
                }
                self.canvas.marquee_start = None;
            }
        }

        // Draw group rectangles BEHIND connections + nodes so the
        // grouping reads as a backdrop, not a foreground decoration.
        self.draw_groups(&painter, offset, zoom);

        // Group hit-testing on the cached rects from the previous
        // draw_groups frame: clicking a group header selects the group;
        // clicking the body (only when the click didn't land on a
        // child node) also selects the group; dragging the header
        // moves every member.
        let group_hits: Vec<(u64, egui::Rect, egui::Rect)> = self
            .visuals
            .groups
            .keys()
            .filter_map(|gid| {
                let h = *self.visuals.group_header_rects.get(gid)?;
                let b = *self.visuals.group_body_rects.get(gid)?;
                Some((*gid, h, b))
            })
            .collect();
        for (gid, header_rect, body_rect) in group_hits {
            // Header is the priority hit zone — that's the title bar
            // the user grabs to drag. Body is a fallback for clicks on
            // empty space inside the group rect (we filter out clicks
            // that land on a child node by checking node rects later
            // in the node pass; here we treat any header/body click
            // as a group-claim).
            let header_resp = ui.interact(
                header_rect,
                egui::Id::new(("group_header", gid)),
                egui::Sense::click_and_drag(),
            );
            let body_resp = ui.interact(
                body_rect,
                egui::Id::new(("group_body", gid)),
                egui::Sense::click_and_drag(),
            );
            if header_resp.clicked() || body_resp.clicked() {
                self.select_group(gid);
                if let Some(p) = ui.ctx().pointer_latest_pos() {
                    self.dialog.pending_props_open = Some(PendingPropsOpen {
                        target: PropsTarget::Group(gid),
                        armed_at: Instant::now(),
                        armed_pos: p,
                    });
                }
            }
            // Drag either header or body to move every member node by
            // the same delta — analogous to dragging a folder in a
            // file manager.
            let drag_resp = if header_resp.dragged() {
                Some(&header_resp)
            } else if body_resp.dragged() {
                Some(&body_resp)
            } else {
                None
            };
            if let Some(r) = drag_resp {
                // Screen-space drag → world-space member movement.
                let delta = r.drag_delta() / zoom;
                if let Some(g) = self.visuals.groups.get(&gid) {
                    let ids: Vec<NodeId> = g.member_ids.iter().copied().collect();
                    for id in ids {
                        if let Some(v) = self.visuals.node_visuals.get_mut(&id) {
                            v.position += delta;
                        }
                    }
                }
            }
            // Group-level context menu — delete with confirm.
            let is_sub = self
                .visuals
                .groups
                .get(&gid)
                .map(|g| g.is_subgraph)
                .unwrap_or(false);
            header_resp.context_menu(|ui| {
                let label = if is_sub {
                    "Delete subgraph"
                } else {
                    "Delete group…"
                };
                if ui.button(label).clicked() {
                    if is_sub {
                        self.delete_subgraph_with_contents(gid);
                    } else {
                        self.selection.pending_group_delete = Some(gid);
                    }
                    ui.close_menu();
                }
            });
        }

        // When the user is dragging a connection, compute which nodes
        // have at least one compatible input port so everything else
        // can be dimmed. The source node is always considered active.
        let drag_active_nodes: Option<std::collections::HashSet<NodeId>> =
            self.canvas.drag_connection.as_ref().and_then(|drag| {
                let drag_kind = self
                    .graph
                    .get_node(drag.from_node)?
                    .outputs
                    .iter()
                    .find(|p| p.name == drag.from_port)
                    .map(|p| p.kind)?;
                let mut active = std::collections::HashSet::new();
                active.insert(drag.from_node);
                for (nid, node) in self.graph.nodes() {
                    // IO boundary nodes accept any kind via the engine bypass.
                    let is_io = matches!(
                        node.node_type,
                        NodeType::SubgraphInput | NodeType::SubgraphOutput
                    );
                    if is_io
                        || node
                            .inputs
                            .iter()
                            .any(|p| drag_kind.compatible_with(p.kind))
                    {
                        active.insert(*nid);
                    }
                }
                Some(active)
            });

        // Draw connections + hit-test against the cursor for selection.
        // Compute the layout of every collapsed subgraph upfront so
        // wires can reroute through their external port handles when
        // an endpoint is on a hidden inner node that's bound to one.
        let (_collapsed_rects_pre, subgraph_handles) = self.collapsed_subgraph_layout(offset, zoom);
        let hidden_for_wires = self.hidden_nodes_this_frame();
        let connections_snapshot = self.graph.connections().to_vec();
        let mut wire_polylines: Vec<((PortId, PortId), Vec<egui::Pos2>)> = Vec::new();
        for conn in &connections_snapshot {
            // Pick the visible endpoint for each side. If the endpoint
            // is on a hidden inner node, look it up in the subgraph
            // handle map; if found, use the external port's handle
            // position. Skip the wire only when an endpoint is
            // genuinely invisible (hidden + not exposed by binding).
            let from_hidden = hidden_for_wires.contains(&conn.from.node_id);
            let to_hidden = hidden_for_wires.contains(&conn.to.node_id);
            let from_pos: Option<egui::Pos2> = if from_hidden {
                subgraph_handles
                    .get(&(conn.from.node_id, conn.from.port_name.clone()))
                    .copied()
            } else {
                self.visuals
                    .node_visuals
                    .get(&conn.from.node_id)
                    .and_then(|visual| {
                        let node = self.graph.get_node(conn.from.node_id)?;
                        let out_idx = node
                            .outputs
                            .iter()
                            .position(|p| p.name == conn.from.port_name)?;
                        let node_rect = egui::Rect::from_min_size(
                            to_screen(visual.position),
                            visual.size * zoom,
                        );
                        Some(node_port_pos(
                            &node.node_type,
                            node_rect,
                            PortPlacement::Right,
                            out_idx,
                            zoom,
                        ))
                    })
            };
            // to_pos and to_placement computed together so control-point
            // axis can reflect which edge the input port sits on.
            let (to_pos, to_placement): (Option<egui::Pos2>, PortPlacement) = if to_hidden {
                (
                    subgraph_handles
                        .get(&(conn.to.node_id, conn.to.port_name.clone()))
                        .copied(),
                    PortPlacement::Left,
                )
            } else {
                let result = self
                    .visuals
                    .node_visuals
                    .get(&conn.to.node_id)
                    .and_then(|visual| {
                        let node = self.graph.get_node(conn.to.node_id)?;
                        let port = node.inputs.iter().find(|p| p.name == conn.to.port_name)?;
                        let placement = PortPlacement::for_input(port.kind);
                        let side_idx = if matches!(placement, PortPlacement::Left) {
                            node.inputs
                                .iter()
                                .filter(|p| {
                                    matches!(PortPlacement::for_input(p.kind), PortPlacement::Left)
                                })
                                .position(|p| p.name == conn.to.port_name)?
                        } else {
                            0
                        };
                        let node_rect = egui::Rect::from_min_size(
                            to_screen(visual.position),
                            visual.size * zoom,
                        );
                        let pos =
                            node_port_pos(&node.node_type, node_rect, placement, side_idx, zoom);
                        Some((pos, placement))
                    });
                match result {
                    Some((pos, pl)) => (Some(pos), pl),
                    None => (None, PortPlacement::Left),
                }
            };
            let (Some(from_pos), Some(to_pos)) = (from_pos, to_pos) else {
                continue;
            };
            let to_axis = match to_placement {
                PortPlacement::Left => egui::vec2(-1.0, 0.0),
                PortPlacement::Right => egui::vec2(1.0, 0.0),
                PortPlacement::Top(_) => egui::vec2(0.0, -1.0),
                PortPlacement::Bottom => egui::vec2(0.0, 1.0),
            };
            let dist = (from_pos - to_pos).length().max(40.0);
            let strength = 0.4 * dist;
            let ctrl1 = from_pos + egui::vec2(1.0, 0.0) * strength;
            let ctrl2 = to_pos + to_axis * strength;
            let points: Vec<egui::Pos2> = (0..=20)
                .map(|i| {
                    let t = i as f32 / 20.0;
                    cubic_bezier(from_pos, ctrl1, ctrl2, to_pos, t)
                })
                .collect();
            wire_polylines.push(((conn.from.clone(), conn.to.clone()), points));
        }

        // Hit-test the cursor against every wire's polyline. The
        // closest wire within `hit_radius` is the candidate; if the
        // user clicked on empty canvas this frame, that candidate
        // becomes the new selection.
        let hit_radius = 6.0_f32;
        let mut wire_hit: Option<(PortId, PortId)> = None;
        if let Some(p) = pointer {
            let mut best: f32 = hit_radius;
            for (key, points) in &wire_polylines {
                let d = polyline_distance(p, points);
                if d < best {
                    best = d;
                    wire_hit = Some(key.clone());
                }
            }
        }
        // Plain primary-click on empty canvas (the canvas response
        // owns the click since no node took it) selects the wire if
        // one is under the cursor; otherwise the click was
        // already handled above as "deselect".
        if response.clicked() && self.palette_drag.is_none() && !clicked_in_group {
            if let Some(key) = wire_hit.clone() {
                self.select_connection(key.0, key.1);
            }
        }
        // Right-click on a wire offers a delete shortcut without
        // having to reach for the keyboard.
        let mut wire_to_delete: Option<(PortId, PortId)> = None;
        if let Some(key) = wire_hit.clone() {
            if response.secondary_clicked() {
                self.select_connection(key.0.clone(), key.1.clone());
            }
            response.clone().context_menu(|ui| {
                if ui.button("Delete connection").clicked() {
                    wire_to_delete = Some(key.clone());
                    ui.close_menu();
                }
            });
        }
        if let Some((from, to)) = wire_to_delete {
            self.push_undo("Delete connection");
            self.graph.disconnect(&from, &to);
            self.selection.connection = None;
        }

        // Stroke pass — the selected wire renders thicker + brighter
        // so the user can see what they've targeted before deleting.
        for ((from, to), points) in &wire_polylines {
            let is_selected = self
                .selection
                .connection
                .as_ref()
                .map(|(sf, st)| sf == from && st == to)
                .unwrap_or(false);
            let wire_active = drag_active_nodes.as_ref().is_none_or(|active| {
                active.contains(&from.node_id) || active.contains(&to.node_id)
            });
            let (w, color) = if is_selected {
                (3.0, tokens::WIRE_SELECTED)
            } else {
                (2.0, tokens::WIRE_DEFAULT)
            };
            let color = if wire_active {
                color
            } else {
                color.gamma_multiply(0.25)
            };
            let stroke = egui::Stroke::new(w, color);
            for i in 0..points.len() - 1 {
                painter.line_segment([points[i], points[i + 1]], stroke);
            }
        }

        // Draw in-progress connection
        if let Some(ref drag) = self.canvas.drag_connection {
            if let Some(pointer_pos) = ui.ctx().pointer_latest_pos() {
                let from = drag.from_pos;
                let dist = (pointer_pos - from).length().max(40.0);
                let strength = 0.4 * dist;
                // Outputs always emit rightward, so the source-end tangent is +X.
                let ctrl1 = egui::pos2(from.x + strength, from.y);
                let to_dir = (from - pointer_pos).normalized();
                let ctrl2 = pointer_pos + to_dir * strength;

                let points: Vec<egui::Pos2> = (0..=20)
                    .map(|i| {
                        let t = i as f32 / 20.0;
                        cubic_bezier(from, ctrl1, ctrl2, pointer_pos, t)
                    })
                    .collect();

                for i in 0..points.len() - 1 {
                    painter.line_segment(
                        [points[i], points[i + 1]],
                        egui::Stroke::new(2.0, tokens::WIRE_DRAG),
                    );
                }
            }
        }

        // Draw nodes
        // Per-frame theme-aware colors shared across the node loop.
        let port_label_col = ui.visuals().strong_text_color();
        let (handle_idle_col, handle_active_col, io_port_ring_col) = {
            let vis = ui.visuals();
            let towards = if vis.dark_mode {
                egui::Color32::WHITE
            } else {
                egui::Color32::BLACK
            };
            let neutral = vis.window_fill().lerp_to_gamma(towards, 0.35);
            (neutral, vis.selection.bg_fill, neutral)
        };
        let node_ids: Vec<NodeId> = self.visuals.node_visuals.keys().copied().collect();
        // (id, additive). additive=true means "Ctrl+click — toggle in
        // the multi-selection set"; false means "plain click — replace".
        let mut new_selection: Option<(NodeId, bool)> = None;
        // Nodes whose drag ended this frame — we'll resolve their
        // landing group (if any) below.
        let mut drag_drop_into_group: Vec<NodeId> = Vec::new();
        // Nodes that should not render or accept interaction this
        // frame (members of a collapsed subgraph, or anything outside
        // the current SubGraph tab's scope).
        let hidden_nodes = self.hidden_nodes_this_frame();
        // Contextual properties panel: arm when a node was clicked,
        // clear when a drag started or a tab was opened. Applied
        // after the per-node loop so the pending state is consistent
        // for the whole frame.
        let mut pending_props_arm: Option<PendingPropsOpen> = None;
        let mut pending_props_clear = false;
        let mut connection_start: Option<DragConnection> = None;
        let mut connection_end: Option<(NodeId, String)> = None;

        for node_id in &node_ids {
            if hidden_nodes.contains(node_id) {
                continue;
            }
            // Extract all owned data upfront so later `get_mut` calls don't conflict.
            let extracted = self.graph.get_node(*node_id).map(|n| {
                let passthrough_files = if n.node_type == NodeType::PassThrough {
                    let s = match n.params.get("files") {
                        Some(ParamValue::String(s)) => s.clone(),
                        _ => String::new(),
                    };
                    Some(parse_passthrough_files(&s))
                } else {
                    None
                };
                // For IO nodes, pull the user-supplied name and the
                // port kind. The bottom line of the tag shows the
                // user's name when set (and not equal to the default
                // "input"/"output" placeholder); otherwise it shows
                // the kind. The kind also colours the port circle.
                let (io_name, io_kind) = if matches!(
                    n.node_type,
                    NodeType::SubgraphInput | NodeType::SubgraphOutput
                ) {
                    let name = match n.params.get("name") {
                        Some(ParamValue::String(s)) if !s.is_empty() => Some(s.clone()),
                        _ => None,
                    };
                    let kind = match n.params.get("kind") {
                        Some(ParamValue::String(s)) if !s.is_empty() => Some(s.clone()),
                        _ => None,
                    };
                    (name, kind)
                } else {
                    (None, None)
                };
                (
                    n.node_type.clone(),
                    n.label.clone(),
                    n.inputs.clone(),
                    n.outputs.clone(),
                    passthrough_files,
                    io_name,
                    io_kind,
                )
            });
            let Some((
                node_type,
                node_label,
                node_inputs,
                node_outputs,
                passthrough_files,
                io_name,
                io_kind,
            )) = extracted
            else {
                continue;
            };
            let visual_data = self
                .visuals
                .node_visuals
                .get(node_id)
                .map(|v| (v.position, v.size));
            let Some((node_pos_raw, node_size)) = visual_data else {
                continue;
            };
            // All borrows on self.graph and self.visuals.node_visuals released here.

            let node_rect = egui::Rect::from_min_size(to_screen(node_pos_raw), node_size * zoom);

            if !canvas_rect.intersects(node_rect) {
                continue;
            }

            // Per-node minimum height from port count. IO nodes
            // render proportionally and don't stack ports, so they
            // get a smaller floor than regular nodes — the user can
            // shrink them below the 60-px default if they want to
            // pack a subgraph tightly.
            let is_primary = self.selection.node == Some(*node_id);
            let is_selected = is_primary || self.selection.nodes.contains(node_id);
            let is_io_input = matches!(node_type, NodeType::SubgraphInput);
            let is_io_output = matches!(node_type, NodeType::SubgraphOutput);
            let is_io = is_io_input || is_io_output;
            // Fade nodes that have no compatible port for the in-flight drag.
            let node_fade = drag_active_nodes.as_ref().map_or(1.0_f32, |active| {
                if active.contains(node_id) {
                    1.0
                } else {
                    0.3
                }
            });
            let left_input_count = node_inputs
                .iter()
                .filter(|p| matches!(PortPlacement::for_input(p.kind), PortPlacement::Left))
                .count();
            let n_ports = left_input_count.max(node_outputs.len());
            let (node_min_h, node_min_w) = if is_io {
                (28.0_f32, 90.0_f32)
            } else {
                (
                    (PORT_Y_BASE + n_ports as f32 * PORT_Y_STEP + 10.0).max(60.0),
                    100.0_f32,
                )
            };

            if is_io {
                // ── Subgraph IO node: directional tag ───────────────
                // The whole silhouette (rounded body on the far side,
                // chevron point on the port side) is built as one
                // convex polygon with arc samples on the rounded
                // corners. That keeps the fill and the border in
                // perfect register at any size — no chamfered
                // border-vs-curved-fill mismatch — and lets every
                // dimension scale proportionally with the node's
                // actual height. At the reference 60-px height the
                // numbers reproduce the design spec exactly.
                let h = node_rect.height();
                let scale = h / IO_REF_H;
                let chevron_w = h * 0.30;
                let body_radius = (h / 6.0).min(node_rect.width() / 4.0);
                // Vertical packing — earlier passes left ~20–28% of
                // the node height as empty space above and below
                // the icon/text stack. The fixes:
                //
                // - The icon now spans 80% of the node's height
                //   (was 60%). At 60-px height it's 48 px, leaving
                //   only 6 px above and below — about 10% per side.
                // - Text sizes scale up too so the text block
                //   doesn't look stranded next to the larger icon.
                //   Top 18 / bottom 15 at the reference height,
                //   stack ≈ 35 px = 58% of the node height.
                let inner_pad = 6.0 * scale;
                let icon_size = 48.0 * scale;
                let icon_text_gap = 8.0 * scale;
                let top_text_size = 18.0 * scale;
                let bottom_text_size = 15.0 * scale;

                let (io_body, io_border_base, io_label_pri, io_label_sec) = {
                    let vis = ui.visuals();
                    let towards = if vis.dark_mode {
                        egui::Color32::WHITE
                    } else {
                        egui::Color32::BLACK
                    };
                    let base = vis.window_fill();
                    (
                        base.lerp_to_gamma(towards, 0.12),
                        base.lerp_to_gamma(towards, 0.30),
                        vis.text_color(),
                        vis.weak_text_color(),
                    )
                };
                let body_color = io_body.gamma_multiply(node_fade);
                let border_color = if is_selected {
                    tokens::IO_BORDER_SEL
                } else {
                    io_border_base.gamma_multiply(node_fade)
                };
                let border_width = (if is_selected { 2.0 } else { 1.5 }) * zoom;
                let mid_y = node_rect.center().y;

                let outline_pts = build_io_outline(node_rect, chevron_w, body_radius, is_io_input);
                painter.add(egui::Shape::convex_polygon(
                    outline_pts,
                    body_color,
                    egui::Stroke::new(border_width, border_color),
                ));

                // Icon container, anchored on the side opposite the
                // port, vertically centred.
                let icon_rect = if is_io_input {
                    egui::Rect::from_min_size(
                        egui::pos2(node_rect.left() + inner_pad, mid_y - icon_size / 2.0),
                        egui::vec2(icon_size, icon_size),
                    )
                } else {
                    egui::Rect::from_min_size(
                        egui::pos2(
                            node_rect.right() - inner_pad - icon_size,
                            mid_y - icon_size / 2.0,
                        ),
                        egui::vec2(icon_size, icon_size),
                    )
                };
                draw_io_icon(&painter, icon_rect, is_io_input);

                // Two-line text. Top line is the role
                // ("Input"/"Output"); bottom line is always the
                // port type. The node's `name` param drives the
                // *wrapper block*'s external port label and isn't
                // shown on the IO node itself — the IO node is a
                // marker, not a carrier of semantic identity.
                // `io_name` (extracted earlier) is therefore unused
                // in the visual; keeping the param around so the
                // wrapper render can read it.
                let _ = &io_name;
                let top_text = if is_io_input { "Input" } else { "Output" };
                let bottom_text = match io_kind.as_deref() {
                    Some(s) if !s.is_empty() => s,
                    _ => "Unknown",
                };
                let text_left = if is_io_input {
                    icon_rect.right() + icon_text_gap
                } else {
                    node_rect.left() + chevron_w + inner_pad
                };
                // 2 px was too tight — the "Input"/"Output" label
                // and the type label below it visually touched.
                // 6 px gives them room to breathe without breaking
                // the stack's visual unity.
                let line_gap = 6.0 * scale;
                let stack_h = top_text_size + line_gap + bottom_text_size;
                let text_top = mid_y - stack_h / 2.0;
                painter.text(
                    egui::pos2(text_left, text_top),
                    egui::Align2::LEFT_TOP,
                    top_text,
                    egui::FontId::proportional(top_text_size),
                    io_label_pri,
                );
                painter.text(
                    egui::pos2(text_left, text_top + top_text_size + line_gap),
                    egui::Align2::LEFT_TOP,
                    bottom_text,
                    egui::FontId::proportional(bottom_text_size),
                    io_label_sec,
                );
            } else {
                // Scale the node-body geometry (corner radius, title
                // height, border widths) by zoom so the chrome grows /
                // shrinks in lock-step with the node rect. Colours are
                // zoom-independent.
                let mut ns = NodeStyle::from_visuals(ui.visuals());
                ns.rounding *= zoom;
                ns.title_h *= zoom;
                ns.border_w *= zoom;
                ns.border_w_sel *= zoom;
                // Node background — slightly lighter when in the multi-
                // selection, even lighter for the primary so the user can
                // tell which one's properties are showing.
                let bg_color = if is_primary {
                    ns.bg_pri
                } else if is_selected {
                    ns.bg_sel
                } else {
                    ns.bg
                };
                painter.rect_filled(node_rect, ns.rounding, bg_color.gamma_multiply(node_fade));

                // Node border
                let (border_color, bw) = if is_selected {
                    (ns.border_sel, ns.border_w_sel)
                } else {
                    (ns.border, ns.border_w)
                };
                painter.rect_stroke(
                    node_rect,
                    ns.rounding,
                    egui::Stroke::new(bw, border_color.gamma_multiply(node_fade)),
                    egui::StrokeKind::Outside,
                );

                // Node title bar -- inset by TITLE_Y_OFFSET so the top-port
                // circles (centered on node_rect.min.y) don't overlap the text.
                let title_rect = egui::Rect::from_min_size(
                    egui::pos2(node_rect.min.x, node_rect.min.y + TITLE_Y_OFFSET * zoom),
                    egui::vec2(node_rect.width(), ns.title_h),
                );
                let title_color = node_type_color(&node_type).gamma_multiply(node_fade);
                painter.rect_filled(title_rect, ns.title_rounding, title_color);
                painter.text(
                    title_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    &node_label,
                    egui::FontId::proportional(12.0 * zoom),
                    egui::Color32::WHITE,
                );
            }

            // Input + output port handles, registered as proper egui
            // interactions so their click/drag events are z-ordered
            // ahead of the canvas's marquee detection. Without this,
            // a click on a port handle (which sits on the node's
            // edge) would also fire `drag_started_by(Primary)` on
            // the canvas behind it — the user would see the marquee
            // and the connection-start fire simultaneously.
            let port_radius = 5.0 * zoom;
            // Hit target stays a comfortable size at small zoom (so
            // ports remain clickable when zoomed out) but never shrinks
            // below the visual handle.
            let hit_size = egui::Vec2::splat((14.0 * zoom).max(10.0));
            let mut left_port_idx = 0_usize;
            for (i, input) in node_inputs.iter().enumerate() {
                // SubgraphInput's input is the EXTERNAL side, only ever
                // reached from outside the subgraph (via the collapsed
                // block's port). Don't draw it on the IO pill.
                if is_io_input {
                    continue;
                }
                let placement = PortPlacement::for_input(input.kind);
                let port_pos = node_port_pos(&node_type, node_rect, placement, left_port_idx, zoom);
                if matches!(placement, PortPlacement::Left) {
                    left_port_idx += 1;
                }
                let port_color = port_kind_color(&input.kind);
                let hit_rect = egui::Rect::from_center_size(port_pos, hit_size);
                let port_resp = ui.interact(
                    hit_rect,
                    egui::Id::new(("port_in", node_id.0, i)),
                    egui::Sense::click_and_drag(),
                );
                draw_port_circle(
                    &painter,
                    port_pos,
                    port_radius,
                    port_color,
                    port_resp.hovered(),
                );
                if is_io {
                    painter.circle_stroke(
                        port_pos,
                        port_radius,
                        egui::Stroke::new(1.0, io_port_ring_col),
                    );
                }
                if !is_io {
                    if matches!(placement, PortPlacement::Left) {
                        painter.text(
                            egui::pos2(port_pos.x + 10.0 * zoom, port_pos.y),
                            egui::Align2::LEFT_CENTER,
                            &input.label,
                            egui::FontId::proportional(10.0 * zoom),
                            port_label_col,
                        );
                    } else {
                        // Top / Bottom ports skip the inline label; show tooltip
                        // instantly (no delay) so the user gets feedback without waiting.
                        if port_resp.hovered() {
                            egui::show_tooltip_text(
                                ui.ctx(),
                                ui.layer_id(),
                                egui::Id::new(("port_tip", node_id.0, i as u64)),
                                &input.label,
                            );
                        }
                    }
                }
                // End an in-flight connection when the user releases
                // primary while their cursor is over this input's
                // hit rect.
                if self.canvas.drag_connection.is_some()
                    && ui.input(|i| i.pointer.primary_released())
                    && port_resp.contains_pointer()
                {
                    connection_end = Some((*node_id, input.name.clone()));
                }
                // Disconnect-by-grabbing-input: if the user starts a
                // drag on an input port that already has a connection,
                // detach it and turn the drag into a re-wire from the
                // existing source's output. This makes "I want to
                // move this wire to a different input" a one-gesture
                // operation: grab the input end, drag to new target.
                if port_resp.drag_started_by(egui::PointerButton::Primary)
                    && self.canvas.drag_connection.is_none()
                {
                    let existing = self
                        .graph
                        .connections()
                        .iter()
                        .find(|c| c.to.node_id == *node_id && c.to.port_name == input.name)
                        .map(|c| {
                            (
                                c.from.node_id,
                                c.from.port_name.clone(),
                                c.to.node_id,
                                c.to.port_name.clone(),
                            )
                        });
                    if let Some((src_node, src_port, dst_node, dst_port)) = existing {
                        self.push_undo("Disconnect");
                        self.graph.disconnect(
                            &PortId {
                                node_id: src_node,
                                port_name: src_port.clone(),
                            },
                            &PortId {
                                node_id: dst_node,
                                port_name: dst_port,
                            },
                        );
                        if let Some(src_pos) =
                            self.output_port_screen_pos(src_node, &src_port, offset, zoom)
                        {
                            connection_start = Some(DragConnection {
                                from_node: src_node,
                                from_port: src_port,
                                from_pos: src_pos,
                            });
                        }
                    }
                }
            }

            for (i, output) in node_outputs.iter().enumerate() {
                // SubgraphOutput's output is the EXTERNAL side, only
                // ever reached from outside the subgraph. Don't draw
                // it on the IO pill.
                if is_io_output {
                    continue;
                }
                let port_pos = node_port_pos(&node_type, node_rect, PortPlacement::Right, i, zoom);
                let port_color = port_kind_color(&output.kind);
                let hit_rect = egui::Rect::from_center_size(port_pos, hit_size);
                let port_resp = ui.interact(
                    hit_rect,
                    egui::Id::new(("port_out", node_id.0, i)),
                    egui::Sense::click_and_drag(),
                );
                draw_port_circle(
                    &painter,
                    port_pos,
                    port_radius,
                    port_color,
                    port_resp.hovered(),
                );
                if !is_io {
                    painter.text(
                        egui::pos2(port_pos.x - 10.0 * zoom, port_pos.y),
                        egui::Align2::RIGHT_CENTER,
                        &output.label,
                        egui::FontId::proportional(10.0 * zoom),
                        port_label_col,
                    );
                }
                // Begin a connection only when the port's own response
                // says a drag started on it. egui's z-order ensures
                // this fires only when the cursor is genuinely on the
                // port handle, not on the surrounding node body.
                if port_resp.drag_started_by(egui::PointerButton::Primary)
                    && self.canvas.drag_connection.is_none()
                {
                    connection_start = Some(DragConnection {
                        from_node: *node_id,
                        from_port: output.name.clone(),
                        from_pos: port_pos,
                    });
                }
            }

            // PassThrough: draw file hierarchy in node body
            if let Some(ref files) = passthrough_files {
                draw_passthrough_body(&painter, node_rect, files, zoom);
            }

            // Export / Compile / Test-in-BAR actions live on the
            // application-level top action bar, not on the
            // FinalComposition node itself.

            // ── Interaction ──────────────────────────────────────────────────────────
            // Node interact is registered FIRST.  Button interacts are registered
            // SECOND so that egui gives them higher priority when clicks land on them.
            // Neither button click changes the node selection.

            // Node interaction (click / double-click / drag)
            let node_response = ui.interact(
                node_rect,
                egui::Id::new(("node", node_id.0)),
                egui::Sense::click_and_drag(),
            );

            // No node-body action buttons -- export / compile run from
            // the top-level action bar instead.

            // Node selection. Also arm the contextual Properties panel:
            // the click is the user's intent to "look at" this node;
            // the 100 ms hover gate filters out accidental clicks on
            // the way to dragging.
            if node_response.clicked() {
                let additive = ui.ctx().input(|i| i.modifiers.ctrl || i.modifiers.command);
                new_selection = Some((*node_id, additive));
                // Ctrl-click is "build a multi-selection", not "look
                // at this node" — don't pop a per-node panel for
                // each toggle; the user is composing a selection.
                if !additive {
                    if let Some(p) = ui.ctx().pointer_latest_pos() {
                        pending_props_arm = Some(PendingPropsOpen {
                            target: PropsTarget::Node(*node_id),
                            armed_at: Instant::now(),
                            armed_pos: p,
                        });
                    }
                }
            }
            // Double-click a Layout node to descend into its bespoke
            // edit view (mirrors double-click-to-enter for subgraphs).
            if node_response.double_clicked()
                && self
                    .graph
                    .get_node(*node_id)
                    .is_some_and(|n| n.node_type == NodeType::Layout)
            {
                self.open_or_activate_tab(CanvasView::NodeEdit(*node_id));
                self.clear_selection();
                self.props.close();
                pending_props_clear = true;
            }
            // Drag-start on a node cancels any pending props popup —
            // the user wanted to drag, not inspect.
            if node_response.drag_started_by(egui::PointerButton::Primary) {
                pending_props_clear = true;
            }

            // Right-click context menu — group operations + delete.
            // Building the menu here keeps the per-node-id context tight.
            let this_id = *node_id;
            let in_group = self.visuals.node_to_group.get(&this_id).copied();
            let other_groups: Vec<(u64, String)> = self
                .visuals
                .groups
                .iter()
                .filter_map(|(gid, g)| {
                    if Some(*gid) == in_group {
                        None
                    } else {
                        let label = if g.label.is_empty() {
                            format!("Group {gid}")
                        } else {
                            g.label.clone()
                        };
                        Some((*gid, label))
                    }
                })
                .collect();
            let mut group_op: Option<GroupOp> = None;
            let mut auto_wire_targets: Option<Vec<NodeId>> = None;
            let mut auto_layout_requested = false;
            // Snapshot the set so the menu closure can iterate without
            // re-borrowing `self`.
            let multi: Vec<NodeId> = self.selection.nodes.iter().copied().collect();
            let multi_active = multi.len() > 1 && self.selection.nodes.contains(&this_id);
            // Pre-compute the unwired-input count so the menu item
            // can render disabled when there's nothing to wire.
            let auto_wire_pool: Vec<NodeId> = if multi_active {
                multi.clone()
            } else {
                vec![this_id]
            };
            let unwired_count = self.count_unwired_inputs(&auto_wire_pool);
            node_response.context_menu(|ui| {
                if multi_active {
                    let label = format!("Group {} selected nodes", multi.len());
                    if ui.button(label).clicked() {
                        group_op = Some(GroupOp::CreateFromSelection(multi.clone()));
                        ui.close_menu();
                    }
                    if !other_groups.is_empty() {
                        ui.menu_button("Move selection to group", |ui| {
                            for (gid, label) in &other_groups {
                                if ui.button(label).clicked() {
                                    group_op = Some(GroupOp::AddManyTo(multi.clone(), *gid));
                                    ui.close_menu();
                                }
                            }
                        });
                    }
                } else {
                    if ui.button("New group with this node").clicked() {
                        group_op = Some(GroupOp::CreateWith(this_id));
                        ui.close_menu();
                    }
                    if !other_groups.is_empty() {
                        ui.menu_button("Move to group", |ui| {
                            for (gid, label) in &other_groups {
                                if ui.button(label).clicked() {
                                    group_op = Some(GroupOp::AddTo(this_id, *gid));
                                    ui.close_menu();
                                }
                            }
                        });
                    }
                    if in_group.is_some() && ui.button("Remove from group").clicked() {
                        group_op = Some(GroupOp::RemoveFrom(this_id));
                        ui.close_menu();
                    }
                }
                ui.separator();
                // Auto Wire: fill every unwired input on the
                // target(s) by looking left for compatible outputs.
                // Disabled when there's nothing to wire so the
                // menu doesn't pretend to have an action.
                let label = if multi_active {
                    format!("Auto Wire {} nodes...", multi.len())
                } else {
                    "Auto Wire...".to_string()
                };
                if ui
                    .add_enabled(unwired_count > 0, egui::Button::new(label))
                    .clicked()
                {
                    auto_wire_targets = Some(auto_wire_pool.clone());
                    ui.close_menu();
                }
                // Auto Layout: reflow the selection (or the
                // right-clicked node) into a left-to-right depth
                // layout. Subgraphs travel as one block.
                if ui.button(t!("editor.menu.auto_layout")).clicked() {
                    auto_layout_requested = true;
                    ui.close_menu();
                }
                ui.separator();
                // FinalComposition is the project's singleton terminal
                // node and can't be deleted. Hide the entry entirely
                // for it (right-click → no Delete option).
                if self.graph.can_delete_node(this_id) && ui.button("Delete node").clicked() {
                    if !multi_active {
                        self.select_only_node(this_id);
                    }
                    self.delete_selected_node();
                    ui.close_menu();
                }
            });
            if let Some(targets) = auto_wire_targets {
                self.push_undo("Auto wire");
                let made = self.auto_wire_nodes(&targets);
                self.dialog.status_message = Some(if made == 0 {
                    "Auto wire: no compatible sources to the left.".to_string()
                } else if made == 1 {
                    "Auto wire: 1 connection added.".to_string()
                } else {
                    format!("Auto wire: {made} connections added.")
                });
            }
            if auto_layout_requested {
                // If the user right-clicked a node that wasn't in
                // their existing selection, treat the click as
                // "select just this node" so Auto Layout acts on it
                // alone rather than on a stale selection.
                if !self.selection.nodes.contains(&this_id) {
                    self.select_only_node(this_id);
                }
                self.auto_layout_selection();
            }
            if let Some(op) = group_op {
                // Each user-visible group action is one undo step; we
                // push once before the helpers modify state.
                self.push_undo(match &op {
                    GroupOp::CreateWith(_) => "Create group",
                    GroupOp::CreateFromSelection(_) => "Group selection",
                    GroupOp::AddTo(_, _) | GroupOp::AddManyTo(_, _) => "Move to group",
                    GroupOp::RemoveFrom(_) => "Remove from group",
                });
                match op {
                    GroupOp::CreateWith(nid) => {
                        let gid = self.create_group(String::new());
                        self.add_node_to_group(nid, gid);
                        self.select_group(gid);
                    }
                    GroupOp::CreateFromSelection(ids) => {
                        let gid = self.create_group(String::new());
                        for id in ids {
                            self.add_node_to_group(id, gid);
                        }
                        self.select_group(gid);
                    }
                    GroupOp::AddTo(nid, gid) => self.add_node_to_group(nid, gid),
                    GroupOp::AddManyTo(ids, gid) => {
                        for id in ids {
                            self.add_node_to_group(id, gid);
                        }
                    }
                    GroupOp::RemoveFrom(nid) => self.remove_node_from_group(nid),
                }
            }

            // Export trigger moved to the top-level action bar; no
            // node-body button to dispatch from here.

            // Resize corner handles (8 px squares; processed after node interact so they
            // are "on top" in egui's interaction stack). Scaled by zoom so
            // the grab squares track the node's painted corners.
            let handle_sz = 8.0_f32 * zoom;
            let corners: [(i8, i8); 4] = [(-1, -1), (1, -1), (-1, 1), (1, 1)];
            let mut any_resize = false;
            for (cx, cy) in corners {
                let corner_pos = match (cx, cy) {
                    (-1, -1) => node_rect.left_top(),
                    (1, -1) => node_rect.right_top(),
                    (-1, 1) => node_rect.left_bottom(),
                    _ => node_rect.right_bottom(),
                };
                let handle_rect =
                    egui::Rect::from_center_size(corner_pos, egui::vec2(handle_sz, handle_sz));
                let handle_resp = ui.interact(
                    handle_rect,
                    egui::Id::new(("resize", node_id.0, cx, cy)),
                    egui::Sense::drag(),
                );
                let handle_active = handle_resp.hovered() || handle_resp.dragged();
                if handle_active {
                    let cursor = if cx == cy {
                        egui::CursorIcon::ResizeNwSe
                    } else {
                        egui::CursorIcon::ResizeNeSw
                    };
                    ui.ctx().set_cursor_icon(cursor);
                }
                if node_response.hovered() || handle_active {
                    painter.rect_filled(
                        handle_rect,
                        2.0,
                        if handle_active {
                            handle_active_col
                        } else {
                            handle_idle_col
                        },
                    );
                }
                if handle_resp.dragged() {
                    any_resize = true;
                    // drag_delta is in screen pixels; node size/position
                    // are world units, so convert by dividing by zoom.
                    let delta = handle_resp.drag_delta() / zoom;
                    if let Some(v) = self.visuals.node_visuals.get_mut(node_id) {
                        if cx == 1 {
                            v.size.x = (v.size.x + delta.x).max(node_min_w);
                        } else {
                            let new_w = (v.size.x - delta.x).max(node_min_w);
                            let dw = new_w - v.size.x;
                            v.position.x -= dw;
                            v.size.x = new_w;
                        }
                        if cy == 1 {
                            v.size.y = (v.size.y + delta.y).max(node_min_h);
                        } else {
                            let new_h = (v.size.y - delta.y).max(node_min_h);
                            let dh = new_h - v.size.y;
                            v.position.y -= dh;
                            v.size.y = new_h;
                        }
                    }
                }
            }

            // Move node on drag (suppressed while a resize handle is
            // active). When the dragged node is part of a multi-
            // selection, every selected node moves by the same delta
            // — same shortcut as Photoshop / Figma / Blender.
            if node_response.dragged() && !any_resize {
                // Screen-space drag → world-space node movement.
                let delta = node_response.drag_delta() / zoom;
                if self.selection.nodes.contains(node_id) && self.selection.nodes.len() > 1 {
                    let to_move: Vec<NodeId> = self.selection.nodes.iter().copied().collect();
                    for id in to_move {
                        if let Some(visual) = self.visuals.node_visuals.get_mut(&id) {
                            visual.position += delta;
                        }
                    }
                } else if let Some(visual) = self.visuals.node_visuals.get_mut(node_id) {
                    visual.position += delta;
                }
            }

            // On drag-end, fold a node into whatever group its centre
            // landed inside. Nodes already in that group are skipped.
            // Multi-selection drops together — the group accepts all
            // selected nodes whose centres landed in its rect.
            if node_response.drag_stopped() {
                let drop_targets: Vec<NodeId> =
                    if self.selection.nodes.contains(node_id) && self.selection.nodes.len() > 1 {
                        self.selection.nodes.iter().copied().collect()
                    } else {
                        vec![*node_id]
                    };
                drag_drop_into_group.extend(drop_targets);
            }
        }

        // Collapsed subgraphs render as compact node-like blocks AFTER
        // nodes (so they appear in the foreground). They handle their
        // own selection / double-click-to-enter-confined-mode.
        let (subgraph_block_rects, _subgraph_handle_positions, sg_conn_start, sg_conn_end) =
            self.draw_collapsed_subgraphs(ui, offset, zoom);
        if let Some(s) = sg_conn_start {
            connection_start = Some(s);
        }
        if let Some(e) = sg_conn_end {
            connection_end = Some(e);
        }
        for (gid, rect) in subgraph_block_rects {
            let resp = ui.interact(
                rect,
                egui::Id::new(("subgraph_block", gid)),
                egui::Sense::click_and_drag(),
            );
            if resp.clicked() {
                self.select_group(gid);
                if let Some(p) = ui.ctx().pointer_latest_pos() {
                    self.dialog.pending_props_open = Some(PendingPropsOpen {
                        target: PropsTarget::Group(gid),
                        armed_at: Instant::now(),
                        armed_pos: p,
                    });
                }
            }
            if resp.double_clicked() {
                self.open_or_activate_tab(CanvasView::SubGraph(gid));
                self.clear_selection();
            }
            // Drag the collapsed block to translate every member node
            // by the same delta — same affordance as dragging the
            // expanded group's title bar.
            if resp.dragged() {
                // Screen-space drag → world-space member movement.
                let delta = resp.drag_delta() / zoom;
                if let Some(g) = self.visuals.groups.get(&gid) {
                    let ids: Vec<NodeId> = g.member_ids.iter().copied().collect();
                    for id in ids {
                        if let Some(v) = self.visuals.node_visuals.get_mut(&id) {
                            v.position += delta;
                        }
                    }
                }
            }
            resp.context_menu(|ui| {
                if ui.button("Enter (edit contents)").clicked() {
                    self.open_or_activate_tab(CanvasView::SubGraph(gid));
                    self.clear_selection();
                    ui.close_menu();
                }
                if ui.button("Expand").clicked() {
                    self.push_undo("Expand subgraph");
                    if let Some(g) = self.visuals.groups.get_mut(&gid) {
                        g.collapsed = false;
                    }
                    ui.close_menu();
                }
                if ui.button("Delete subgraph").clicked() {
                    self.delete_subgraph_with_contents(gid);
                    ui.close_menu();
                }
            });
        }

        // Double-click on an expanded subgraph's header / body also
        // enters confined-edit mode (parallel affordance to double-
        // clicking the collapsed block).
        let subgraph_double_click: Vec<u64> = self
            .visuals
            .group_header_rects
            .iter()
            .filter_map(|(gid, rect)| {
                let g = self.visuals.groups.get(gid)?;
                if g.is_subgraph
                    && pointer.is_some_and(|p| rect.contains(p))
                    && ui.ctx().input(|i| {
                        i.pointer
                            .button_double_clicked(egui::PointerButton::Primary)
                    })
                {
                    Some(*gid)
                } else {
                    None
                }
            })
            .collect();
        if let Some(gid) = subgraph_double_click.first().copied() {
            self.open_or_activate_tab(CanvasView::SubGraph(gid));
            self.clear_selection();
        }
        // Apply the pending-props state changes from the node loop.
        // Clear takes precedence over arm so a drag-start on the
        // same frame as a click reliably cancels the popup.
        if pending_props_clear {
            self.dialog.pending_props_open = None;
        } else if let Some(p) = pending_props_arm {
            self.dialog.pending_props_open = Some(p);
        }

        // Apply selection. Plain click replaces; Ctrl+click toggles.
        if let Some((id, additive)) = new_selection {
            if additive {
                self.toggle_select_node(id);
            } else {
                self.select_only_node(id);
            }
        }

        // Resolve drag-and-drop into groups: for each node that just
        // finished a drag, check whether its centre is inside any
        // group's rect. If so, add it to that group (or move it from
        // its old group). If it ended outside every group, remove it
        // from any group it had been in. We push a single undo entry
        // BEFORE applying any of these changes so the whole drag is
        // one undo step (matching typical editor behaviour).
        //
        // SubGraph view: the entire canvas IS the active subgraph,
        // so `draw_groups` deliberately doesn't paint the group's
        // backdrop. That leaves `group_header_rects` empty, which
        // would make the drop logic below conclude every node landed
        // "outside every group" and evict each one from the
        // subgraph it's a member of. Skip the whole pass — group
        // membership only changes via Main-view drags or the right-
        // click menu.
        let in_subgraph_view = matches!(self.current_view(), CanvasView::SubGraph(_));
        if in_subgraph_view {
            drag_drop_into_group.clear();
        }
        if !drag_drop_into_group.is_empty() {
            // Determine if the drag actually changes any membership;
            // if not, skip the undo push so trivial drags don't pile
            // up empty undo entries.
            let mut group_membership_changed = false;
            // Compute candidate landing without mutating yet.
            let group_rects: Vec<(u64, egui::Rect)> = self
                .visuals
                .group_header_rects
                .iter()
                .filter_map(|(gid, h)| {
                    let b = self.visuals.group_body_rects.get(gid)?;
                    Some((*gid, h.union(*b)))
                })
                .collect();
            for nid in &drag_drop_into_group {
                let Some(visual) = self.visuals.node_visuals.get(nid) else {
                    continue;
                };
                let centre = to_screen(visual.position + visual.size * 0.5);
                let landed_in = group_rects
                    .iter()
                    .find(|(_, r)| r.contains(centre))
                    .map(|(gid, _)| *gid);
                let prev = self.visuals.node_to_group.get(nid).copied();
                match (prev, landed_in) {
                    (Some(p), Some(n)) if p != n => group_membership_changed = true,
                    (None, Some(_)) | (Some(_), None) => group_membership_changed = true,
                    _ => {}
                }
            }
            if group_membership_changed {
                self.push_undo("Drag into/out of group");
            }
        }
        if !drag_drop_into_group.is_empty() {
            // Build (group_id, full_rect) snapshot. We use the cached
            // header + body rects to reconstruct the union.
            let group_rects: Vec<(u64, egui::Rect)> = self
                .visuals
                .group_header_rects
                .iter()
                .filter_map(|(gid, h)| {
                    let b = self.visuals.group_body_rects.get(gid)?;
                    Some((*gid, h.union(*b)))
                })
                .collect();
            for nid in drag_drop_into_group {
                let Some(visual) = self.visuals.node_visuals.get(&nid) else {
                    continue;
                };
                let centre = to_screen(visual.position + visual.size * 0.5);
                let landed_in = group_rects
                    .iter()
                    .find(|(_, r)| r.contains(centre))
                    .map(|(gid, _)| *gid);
                match (self.visuals.node_to_group.get(&nid).copied(), landed_in) {
                    (Some(prev), Some(new)) if prev != new => {
                        self.add_node_to_group(nid, new);
                    }
                    (None, Some(new)) => {
                        self.add_node_to_group(nid, new);
                    }
                    (Some(_), None) => {
                        // Dragged a member entirely outside every
                        // group rect → remove it from its group so
                        // the rect doesn't balloon to chase it.
                        self.remove_node_from_group(nid);
                    }
                    _ => {}
                }
            }
        }

        // Handle connection creation. If a connection drag started
        // this frame, cancel any marquee that started in the same
        // frame — the user wanted to wire a port, not select a region.
        // (Belt-and-braces guard; the ui.interact() registrations on
        // each port should already steal the click from the canvas's
        // marquee detection, but this catches edge cases where two
        // overlapping interactions both think they started.)
        if let Some(start) = connection_start {
            self.canvas.drag_connection = Some(start);
            self.canvas.marquee_start = None;
        }

        if let Some((to_node, to_port)) = connection_end {
            if let Some(drag) = self.canvas.drag_connection.clone() {
                self.push_undo("Connect nodes");
                let from = PortId {
                    node_id: drag.from_node,
                    port_name: drag.from_port,
                };
                let to = PortId {
                    node_id: to_node,
                    port_name: to_port,
                };
                let _ = self.graph.connect(from, to);
            }
            self.canvas.drag_connection = None;
        }

        // Cancel drag on release without target
        if ui.input(|i| i.pointer.any_released()) {
            self.canvas.drag_connection = None;
        }
    }
}
