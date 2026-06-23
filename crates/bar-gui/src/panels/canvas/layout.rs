//! Auto-layout for the node canvas. Distributed `impl BarEditorApp`
//! block.
//!
//! The columnar barycentric layout puts each unit in a column based
//! on its longest path from a source, then orders within the column
//! by the average Y of its already-placed inputs. That delivers
//! short connectors and few crossings without the cost of full
//! ILP-style optimisation. See `auto_layout_selection`.
//!
//! Other helpers here: `output_port_screen_pos` (canvas-space port
//! coordinate lookup), `count_unwired_inputs` (gates the auto-wire
//! affordance), `auto_wire_nodes` (kind-compatible best-effort
//! wiring of newly-pasted clusters).

use bar_graph::{NodeId, PortId, PortPlacement};
use eframe::egui;

use crate::app::*;

impl BarEditorApp {
    /// Reflow nodes into a left-to-right column layout that respects
    /// each unit's actual bounding box. The previous version used a
    /// fixed row pitch and could place a 240 px tall Bundler under
    /// itself; this one stacks using real heights so no two units
    /// overlap.
    ///
    /// Priorities (in order):
    /// 1. **No overlap.** Column widths = max unit width in that
    ///    column + horizontal gap; intra-column stacks add up actual
    ///    heights + vertical gap. Both invariants are guaranteed by
    ///    the geometry, not by hope.
    /// 2. **Short connectors.** Within a column, units are ordered
    ///    by the average Y of their already-placed sources
    ///    (barycentric sort). A node sits near the centroid of its
    ///    inputs, so wires don't have to climb across the canvas.
    /// 3. **Fewer crossings.** Falls out of the barycentric order
    ///    above for free in most cases — well-known to approximate
    ///    a minimum-crossing placement on layered DAGs without the
    ///    cost of full ILP-style optimisation.
    ///
    /// Selection rules:
    /// - `selected_group` set → that group as one rigid block.
    /// - Individual nodes selected → those nodes; collapsed-subgraph
    ///   members get bundled with their group.
    /// - Otherwise → every top-level unit.
    pub(crate) fn auto_layout_selection(&mut self) {
        let target_units = self.collect_layout_units();
        if target_units.is_empty() {
            return;
        }
        // "Whole-graph" layout (no selection narrowing) wants its
        // result anchored to the visible viewport so the user can
        // see the reflow. "Selection" layout must NOT pin to
        // viewport — moving a selected subset into view while their
        // external connections stay put would produce 3000-px wires
        // across the canvas. The selection case keeps the existing
        // "anchor at the target's current top-left" behaviour.
        let layout_everything = self.selection.group.is_none() && self.selection.nodes.is_empty();

        self.push_undo("Auto Layout");

        // Per-unit bounding-box size (width × height). For a
        // subgraph, this is the bounding box of every member node.
        let sizes: Vec<egui::Vec2> = target_units.iter().map(|u| u.bounding_size(self)).collect();

        // Topological depth → column index for each unit.
        let depths = self.compute_layout_depths(&target_units);
        let mut columns: std::collections::BTreeMap<u32, Vec<usize>> =
            std::collections::BTreeMap::new();
        for (idx, unit) in target_units.iter().enumerate() {
            let d = depths.get(&unit.representative_id()).copied().unwrap_or(0);
            columns.entry(d).or_default().push(idx);
        }

        // Pre-compute incoming-edge map for barycentric sort.
        // edges_from[u] = set of unit indices feeding into u (within
        // the target set; external feeds are ignored).
        let edges_from = self.compute_incoming_unit_edges(&target_units);

        // Origin selection. Whole-graph: pin to the visible canvas
        // top-left (translated into world space via canvas_offset)
        // so the result lands on screen. Selection: stay at the
        // current bounding-rect top-left to avoid yanking nodes
        // away from external connections.
        let origin = if layout_everything && self.canvas.rect_last.is_positive() {
            // canvas_rect_last is in screen space; node positions are
            // world space. `to_world` applies World = (screen - offset)
            // / zoom, so the layout lands on screen at any zoom. A 40 px
            // screen margin keeps it off the very edge of the canvas
            // where it'd butt against the palette / scrollbar.
            const VIEWPORT_MARGIN: f32 = 40.0;
            self.canvas.to_world(egui::pos2(
                self.canvas.rect_last.left() + VIEWPORT_MARGIN,
                self.canvas.rect_last.top() + VIEWPORT_MARGIN,
            ))
        } else {
            target_units
                .iter()
                .map(|u| u.current_top_left(self))
                .reduce(|acc, p| egui::pos2(acc.x.min(p.x), acc.y.min(p.y)))
                .unwrap_or(egui::pos2(80.0, 80.0))
        };

        const H_GAP: f32 = 80.0;
        const V_GAP: f32 = 40.0;

        // Process columns left-to-right so each column's barycentric
        // sort can read the already-placed Y of its predecessors.
        let mut placed_top_y: std::collections::HashMap<usize, f32> =
            std::collections::HashMap::new();
        let mut col_x = origin.x;
        let mut translations: Vec<(usize, egui::Vec2)> = Vec::new();
        for (_depth, indices) in columns.iter() {
            let mut indices = indices.clone();
            // Barycentric sort: each unit's key = mean Y of its
            // already-placed sources (which sit in earlier columns,
            // already in `placed_top_y`). A unit with no in-target
            // sources falls back to its current Y so the user's
            // manual ordering is preserved for unconnected units.
            indices.sort_by(|&a, &b| {
                let ka =
                    barycentric_key(a, &edges_from, &sizes, &placed_top_y, &target_units, self);
                let kb =
                    barycentric_key(b, &edges_from, &sizes, &placed_top_y, &target_units, self);
                ka.total_cmp(&kb)
            });

            // Stack vertically using each unit's actual height so
            // tall units don't overlap their neighbours below.
            let col_w = indices.iter().map(|&i| sizes[i].x).fold(0.0_f32, f32::max);
            let mut y = origin.y;
            for &i in &indices {
                let target_pos = egui::pos2(col_x, y);
                let current = target_units[i].current_top_left(self);
                translations.push((i, target_pos - current));
                placed_top_y.insert(i, y);
                y += sizes[i].y + V_GAP;
            }
            col_x += col_w + H_GAP;
        }

        // Apply all translations in one pass at the end so reads of
        // current positions (during sort / barycentric) see the
        // pre-layout state, not a half-moved graph.
        for (i, delta) in translations {
            target_units[i].translate(self, delta);
        }

        self.project.is_dirty = true;
        self.dialog.status_message = Some("Auto Layout applied.".to_string());
    }

    /// Map every unit index to the set of unit indices feeding INTO
    /// it. Used for barycentric ordering. Connections from outside
    /// the target set are ignored — those edges hang off the layout
    /// and don't influence in-set placement.
    pub(crate) fn compute_incoming_unit_edges(&self, units: &[LayoutUnit]) -> Vec<Vec<usize>> {
        let mut node_to_unit: std::collections::HashMap<NodeId, usize> =
            std::collections::HashMap::new();
        for (idx, u) in units.iter().enumerate() {
            for nid in u.member_ids() {
                node_to_unit.insert(nid, idx);
            }
        }
        let mut incoming: Vec<Vec<usize>> = vec![Vec::new(); units.len()];
        for conn in self.graph.connections() {
            let Some(&to_idx) = node_to_unit.get(&conn.to.node_id) else {
                continue;
            };
            let Some(&from_idx) = node_to_unit.get(&conn.from.node_id) else {
                continue;
            };
            if from_idx == to_idx {
                continue;
            }
            if !incoming[to_idx].contains(&from_idx) {
                incoming[to_idx].push(from_idx);
            }
        }
        incoming
    }

    /// Build the list of units to lay out — one entry per regular
    /// node, or one entry per subgraph (member ids bundled together).
    /// Honours selection priority: group-only → group, otherwise
    /// node selection, otherwise everything visible at top level.
    pub(crate) fn collect_layout_units(&self) -> Vec<LayoutUnit> {
        // 0. Inside a subgraph view, Auto Layout only touches that
        //    subgraph's members. Without this, triggering Auto Layout
        //    while editing a subgraph re-laid out the *outer* graph
        //    instead — leaving the subgraph's contents unchanged and
        //    silently shuffling everything outside.
        if let Some(CanvasView::SubGraph(gid)) = self.canvas.tabs.get(self.canvas.active_tab) {
            if let Some(group) = self.visuals.groups.get(gid) {
                // If a subset is explicitly selected inside the subgraph,
                // honour that selection. Otherwise lay out every member.
                let candidates: std::collections::HashSet<NodeId> =
                    group.member_ids.iter().copied().collect();
                let mut units: Vec<LayoutUnit> = Vec::new();
                if !self.selection.nodes.is_empty() {
                    for &nid in &self.selection.nodes {
                        if candidates.contains(&nid) {
                            units.push(LayoutUnit::Node(nid));
                        }
                    }
                } else {
                    for nid in &group.member_ids {
                        units.push(LayoutUnit::Node(*nid));
                    }
                }
                return units;
            }
        }

        // 1. Subgraph alone is selected → just that group.
        if let Some(gid) = self.selection.group {
            if let Some(group) = self.visuals.groups.get(&gid) {
                if group.is_subgraph {
                    return vec![LayoutUnit::Subgraph {
                        members: group.member_ids.iter().copied().collect(),
                    }];
                }
            }
        }

        // 2. Individual node selection.
        if !self.selection.nodes.is_empty() {
            // For nodes that are members of collapsed subgraphs,
            // collapse to the subgraph unit. Other nodes go in
            // individually.
            let mut units: Vec<LayoutUnit> = Vec::new();
            let mut emitted_groups: std::collections::HashSet<u64> =
                std::collections::HashSet::new();
            for &nid in &self.selection.nodes {
                if let Some(gid) = self.visuals.node_to_group.get(&nid).copied() {
                    if let Some(group) = self.visuals.groups.get(&gid) {
                        if group.is_subgraph && group.collapsed {
                            if emitted_groups.insert(gid) {
                                units.push(LayoutUnit::Subgraph {
                                    members: group.member_ids.iter().copied().collect(),
                                });
                            }
                            continue;
                        }
                    }
                }
                units.push(LayoutUnit::Node(nid));
            }
            return units;
        }

        // 3. Everything visible at top level. Members of collapsed
        // subgraphs are bundled into their group; everyone else is
        // a standalone node.
        let mut units: Vec<LayoutUnit> = Vec::new();
        let mut handled: std::collections::HashSet<NodeId> = std::collections::HashSet::new();
        for (gid, group) in &self.visuals.groups {
            if group.is_subgraph && group.collapsed {
                let members: Vec<NodeId> = group.member_ids.iter().copied().collect();
                for m in &members {
                    handled.insert(*m);
                }
                let _ = gid;
                units.push(LayoutUnit::Subgraph { members });
            }
        }
        for nid in self.graph.nodes().keys() {
            if handled.contains(nid) {
                continue;
            }
            units.push(LayoutUnit::Node(*nid));
        }
        units
    }

    /// Topological depth per unit, keyed by the unit's
    /// representative NodeId. Edges from sources outside the target
    /// set are ignored — those count as "external feeds" that don't
    /// influence in-set depth, so a subset can be re-laid out
    /// without dragging the rest of the graph along.
    pub(crate) fn compute_layout_depths(
        &self,
        units: &[LayoutUnit],
    ) -> std::collections::HashMap<NodeId, u32> {
        // Map every member NodeId to the unit's representative.
        let mut node_to_rep: std::collections::HashMap<NodeId, NodeId> =
            std::collections::HashMap::new();
        for unit in units {
            let rep = unit.representative_id();
            for nid in unit.member_ids() {
                node_to_rep.insert(nid, rep);
            }
        }

        // Iterate in graph topo order so a unit's dependencies are
        // resolved before it.
        let topo = self.graph.topological_sort().unwrap_or_default();

        let mut depths: std::collections::HashMap<NodeId, u32> = std::collections::HashMap::new();
        for nid in topo {
            let Some(rep) = node_to_rep.get(&nid).copied() else {
                continue;
            };
            // Look at every connection landing on this node.
            let mut max_dep: Option<u32> = None;
            for conn in self.graph.connections() {
                if conn.to.node_id != nid {
                    continue;
                }
                let Some(src_rep) = node_to_rep.get(&conn.from.node_id).copied() else {
                    continue; // external feed — doesn't influence depth
                };
                if src_rep == rep {
                    continue; // intra-unit connection (e.g. inside a subgraph)
                }
                let from_depth = depths.get(&src_rep).copied().unwrap_or(0);
                let candidate = from_depth + 1;
                max_dep = Some(max_dep.map_or(candidate, |m| m.max(candidate)));
            }
            // Update the rep's depth to the max over all member nodes.
            let new_dep = max_dep.unwrap_or(0);
            let entry = depths.entry(rep).or_insert(0);
            if new_dep > *entry {
                *entry = new_dep;
            }
        }
        depths
    }

    /// Screen-space centre of a node's named output port. Mirrors
    /// the layout the node-rendering pass uses (right edge of the
    /// node rect, vertically stacked at PORT_Y_BASE + i*PORT_Y_STEP),
    /// so a wire originating here lines up exactly with the painted
    /// port handle. Used by the disconnect-by-input gesture to
    /// re-anchor the in-flight wire at the source's output.
    pub(crate) fn output_port_screen_pos(
        &self,
        node_id: NodeId,
        port_name: &str,
        offset: egui::Vec2,
        zoom: f32,
    ) -> Option<egui::Pos2> {
        let node = self.graph.get_node(node_id)?;
        let visual = self.visuals.node_visuals.get(&node_id)?;
        let port_index = node.outputs.iter().position(|p| p.name == port_name)?;
        let node_rect = egui::Rect::from_min_size(
            egui::pos2(
                visual.position.x * zoom + offset.x,
                visual.position.y * zoom + offset.y,
            ),
            visual.size * zoom,
        );
        Some(node_port_pos(
            &node.node_type,
            node_rect,
            PortPlacement::Right,
            port_index,
            zoom,
        ))
    }

    /// Count input ports across `ids` that have no incoming
    /// connection. Used to enable / disable the Auto Wire menu item:
    /// nothing to do when every input is already wired.
    pub(crate) fn count_unwired_inputs(&self, ids: &[NodeId]) -> usize {
        let mut total = 0;
        for &nid in ids {
            let Some(node) = self.graph.get_node(nid) else {
                continue;
            };
            for input_port in &node.inputs {
                let wired = self
                    .graph
                    .connections()
                    .iter()
                    .any(|c| c.to.node_id == nid && c.to.port_name == input_port.name);
                if !wired {
                    total += 1;
                }
            }
        }
        total
    }

    /// Auto-wire the unwired inputs of every node in `ids`.
    ///
    /// For each unwired input on a target node, candidate sources
    /// must (a) have their right edge to the left of the target's
    /// left edge, (b) sit within `AUTO_WIRE_MAX_DX` horizontally
    /// and `AUTO_WIRE_MAX_DY` vertically of the target. Within that
    /// bounded window, the closest candidate by Euclidean distance
    /// (right-edge midpoint to left-edge midpoint) wins; vertical
    /// and horizontal separation cost the same.
    ///
    /// The hard distance bound serves two purposes: it keeps the
    /// search O(N) over the candidate window rather than O(N) over
    /// the whole graph for large maps, and it stops the heuristic
    /// from quietly creating very long wires when the only matching
    /// source happens to live across the canvas.
    ///
    /// Multi-select: targets are processed left-to-right so a later
    /// target can pick up a connection an earlier target just made
    /// — useful when the user selected a chain of nodes and asked
    /// to wire the whole thing.
    ///
    /// Type compatibility delegates to `GraphEngine::connect`; the
    /// helper just attempts each candidate in distance order and
    /// keeps the first that connects successfully.
    ///
    /// Returns the count of new connections — 0 means "nothing
    /// landed" (either the inputs were all wired, no compatible
    /// source was within range, or none of the in-range candidates
    /// could connect).
    pub(crate) fn auto_wire_nodes(&mut self, ids: &[NodeId]) -> usize {
        // Hard window for candidate sources, in canvas pixels. Wide
        // enough to cover most layouts that span more than one
        // viewport without sweeping the whole canvas.
        const AUTO_WIRE_MAX_DX: f32 = 1600.0;
        const AUTO_WIRE_MAX_DY: f32 = 1000.0;

        // Process targets left-to-right so later wiring can chain
        // off earlier wiring within the same selection.
        let mut sorted: Vec<NodeId> = ids.to_vec();
        sorted.sort_by(|a, b| {
            let ax = self
                .visuals
                .node_visuals
                .get(a)
                .map(|v| v.position.x)
                .unwrap_or(0.0);
            let bx = self
                .visuals
                .node_visuals
                .get(b)
                .map(|v| v.position.x)
                .unwrap_or(0.0);
            ax.total_cmp(&bx)
        });

        let mut connections_made = 0usize;

        for target_id in sorted {
            let Some(target_visual) = self.visuals.node_visuals.get(&target_id).cloned() else {
                continue;
            };
            let Some(target_node) = self.graph.get_node(target_id).cloned() else {
                continue;
            };
            // Anchor for distance ranking: the midpoint of the
            // target's left edge, where input ports cluster.
            let target_left = target_visual.position.x;
            let target_anchor = egui::pos2(
                target_left,
                target_visual.position.y + target_visual.size.y * 0.5,
            );

            for input_port in &target_node.inputs {
                // Skip already-wired inputs.
                let already_wired = self
                    .graph
                    .connections()
                    .iter()
                    .any(|c| c.to.node_id == target_id && c.to.port_name == input_port.name);
                if already_wired {
                    continue;
                }

                // Outputs of every node sitting to the left and
                // within the search window are candidates. The two
                // axis-aligned bounds short-circuit before we compute
                // a square root, so the cost stays linear in nodes
                // that are roughly anywhere near the target rather
                // than every node in the graph. Within the window,
                // rank by Euclidean distance from the candidate's
                // right-edge midpoint (where outputs cluster) to
                // the target's left-edge midpoint. Type
                // compatibility is left to graph.connect() so the
                // auto-wire heuristic can't drift from canonical
                // rules.
                let mut candidates: Vec<(NodeId, String, f32)> = Vec::new();
                for (other_id, other_node) in self.graph.nodes() {
                    if *other_id == target_id {
                        continue;
                    }
                    let Some(other_visual) = self.visuals.node_visuals.get(other_id) else {
                        continue;
                    };
                    let other_right = other_visual.position.x + other_visual.size.x;
                    if other_right > target_left {
                        continue;
                    }
                    let dx = target_left - other_right;
                    if dx > AUTO_WIRE_MAX_DX {
                        continue;
                    }
                    let other_anchor = egui::pos2(
                        other_right,
                        other_visual.position.y + other_visual.size.y * 0.5,
                    );
                    let dy = (target_anchor.y - other_anchor.y).abs();
                    if dy > AUTO_WIRE_MAX_DY {
                        continue;
                    }
                    let dist = (target_anchor - other_anchor).length();
                    for output_port in &other_node.outputs {
                        candidates.push((*other_id, output_port.name.clone(), dist));
                    }
                }
                candidates.sort_by(|a, b| a.2.total_cmp(&b.2));

                // First candidate that actually connects wins.
                // graph.connect() validates port-kind compatibility
                // and rejects anything that would create a cycle.
                for (src_id, src_port, _) in candidates {
                    let from = PortId {
                        node_id: src_id,
                        port_name: src_port,
                    };
                    let to = PortId {
                        node_id: target_id,
                        port_name: input_port.name.clone(),
                    };
                    if self.graph.connect(from, to).is_ok() {
                        connections_made += 1;
                        break;
                    }
                }
            }
        }

        if connections_made > 0 {
            self.project.is_dirty = true;
        }
        connections_made
    }
}

/// Sort key for barycentric ordering inside an auto-layout column.
/// A unit whose sources have already been placed in earlier columns
/// gets a key equal to the mean of those source Y positions; a unit
/// with no in-target sources falls back to its current canvas Y so
/// the user's manual order is preserved for unconnected units.
pub(crate) fn barycentric_key(
    idx: usize,
    incoming: &[Vec<usize>],
    sizes: &[egui::Vec2],
    placed_top_y: &std::collections::HashMap<usize, f32>,
    units: &[LayoutUnit],
    app: &BarEditorApp,
) -> f32 {
    let preds = &incoming[idx];
    let mut placed_centres: Vec<f32> = Vec::new();
    for &p in preds {
        if let Some(top) = placed_top_y.get(&p) {
            placed_centres.push(top + sizes[p].y * 0.5);
        }
    }
    if placed_centres.is_empty() {
        units[idx].current_top_left(app).y
    } else {
        placed_centres.iter().sum::<f32>() / placed_centres.len() as f32
    }
}

/// One target of the Auto Layout pass. Either a standalone graph
/// node (movable on its own) or a subgraph (treated as one rigid
/// unit -- the whole block moves and members keep their relative
/// positions).
pub(crate) enum LayoutUnit {
    Node(NodeId),
    Subgraph { members: Vec<NodeId> },
}

impl LayoutUnit {
    pub(crate) fn representative_id(&self) -> NodeId {
        match self {
            LayoutUnit::Node(id) => *id,
            LayoutUnit::Subgraph { members } => members
                .iter()
                .min_by_key(|n| n.0)
                .copied()
                .expect("subgraph must have at least one member"),
        }
    }

    pub(crate) fn member_ids(&self) -> Vec<NodeId> {
        match self {
            LayoutUnit::Node(id) => vec![*id],
            LayoutUnit::Subgraph { members } => members.clone(),
        }
    }

    pub(crate) fn current_top_left(&self, app: &BarEditorApp) -> egui::Pos2 {
        match self {
            LayoutUnit::Node(id) => app
                .visuals
                .node_visuals
                .get(id)
                .map(|v| v.position)
                .unwrap_or(egui::pos2(0.0, 0.0)),
            LayoutUnit::Subgraph { members } => members
                .iter()
                .filter_map(|m| app.visuals.node_visuals.get(m))
                .map(|v| v.position)
                .reduce(|a, b| egui::pos2(a.x.min(b.x), a.y.min(b.y)))
                .unwrap_or(egui::pos2(0.0, 0.0)),
        }
    }

    pub(crate) fn bounding_size(&self, app: &BarEditorApp) -> egui::Vec2 {
        match self {
            LayoutUnit::Node(id) => app
                .visuals
                .node_visuals
                .get(id)
                .map(|v| v.size)
                .unwrap_or(egui::vec2(150.0, 80.0)),
            LayoutUnit::Subgraph { members } => {
                let mut min = egui::pos2(f32::INFINITY, f32::INFINITY);
                let mut max = egui::pos2(f32::NEG_INFINITY, f32::NEG_INFINITY);
                for m in members {
                    if let Some(v) = app.visuals.node_visuals.get(m) {
                        let p0 = v.position;
                        let p1 = egui::pos2(p0.x + v.size.x, p0.y + v.size.y);
                        min.x = min.x.min(p0.x);
                        min.y = min.y.min(p0.y);
                        max.x = max.x.max(p1.x);
                        max.y = max.y.max(p1.y);
                    }
                }
                if min.x.is_finite() {
                    egui::vec2(max.x - min.x, max.y - min.y)
                } else {
                    egui::vec2(180.0, 100.0)
                }
            }
        }
    }

    pub(crate) fn translate(&self, app: &mut BarEditorApp, delta: egui::Vec2) {
        match self {
            LayoutUnit::Node(id) => {
                if let Some(v) = app.visuals.node_visuals.get_mut(id) {
                    v.position += delta;
                }
            }
            LayoutUnit::Subgraph { members } => {
                for m in members {
                    if let Some(v) = app.visuals.node_visuals.get_mut(m) {
                        v.position += delta;
                    }
                }
            }
        }
    }
}
