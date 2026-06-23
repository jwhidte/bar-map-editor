//! Canvas viewport + interaction state.
//!
//! Holds the pan offset, the open tabs (Main + drilldowns into
//! collapsed subgraphs), the active and previous tab indices, the
//! cached canvas rect from the previous frame (used by drop-target
//! tests during palette drag), and the in-progress marquee /
//! drag-connection state.

use bar_graph::NodeId;
use eframe::egui;

/// One tab in the canvas-area tab bar. The user can keep multiple
/// editing contexts open and switch between them. Tabs are
/// session-scoped state; they don't persist through save/load.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CanvasView {
    /// The whole graph. Always present, can't be closed.
    Main,
    /// Edit-in-isolation view of one sub-graph's contents -- the
    /// previous "confined edit mode" lifted into a tab so the user
    /// can keep the Main tab open alongside.
    SubGraph(u64),
    /// Bespoke full-area editor for a single node (currently the
    /// `Layout` node): a large 2D authoring canvas plus a live
    /// preview of that one node's output. Entered by double-clicking
    /// the node; backed out via the tab close button.
    NodeEdit(NodeId),
}

/// In-progress port drag for wire creation. Output ports always emit
/// from a Right placement, so the wire's tangent at the source end is
/// always +X.
#[derive(Clone, Debug)]
pub struct DragConnection {
    pub from_node: NodeId,
    pub from_port: String,
    pub from_pos: egui::Pos2,
}

/// Grouped canvas viewport + interaction state. See module docs.
#[derive(Debug, Clone)]
pub struct CanvasState {
    /// Pan offset of the canvas in screen pixels.
    pub offset: egui::Vec2,
    /// Zoom factor mapping graph-space units to screen pixels. The
    /// canvas-to-screen transform is `screen = world * zoom + offset`;
    /// `1.0` is the identity (one graph unit == one pixel) and was the
    /// implicit behaviour before zoom existed. Clamped on input to a
    /// sane range so the graph can't be lost to extreme scales.
    pub zoom: f32,
    /// Canvas rect from the previous frame -- used by palette drag
    /// to detect drops onto the canvas.
    pub rect_last: egui::Rect,
    /// Open canvas tabs. Index 0 is always `CanvasView::Main` and
    /// can't be closed. Drilldowns into collapsed subgraphs append
    /// here and close via the small x on each tab.
    pub tabs: Vec<CanvasView>,
    /// Index into `tabs`. Always valid: `tabs.len() > 0` and
    /// `active_tab < tabs.len()`.
    pub active_tab: usize,
    /// Tab the user was on before the current one. Ctrl+Tab swaps
    /// `active_tab` and this -- the conventional "back to where I
    /// was" shortcut.
    pub last_active_tab: usize,
    /// Set when a project-creation path needs an "everything" Auto
    /// Layout AFTER the canvas has rendered at least once. Consumed
    /// in `draw_node_graph` once `rect_last` is fresh.
    pub pending_auto_layout_all: bool,
    /// Anchor point of an in-progress marquee selection. Set when
    /// the user starts a primary-button drag on empty canvas;
    /// cleared on drag-stopped.
    pub marquee_start: Option<egui::Pos2>,
    /// In-progress port drag for wire creation. Output ports always
    /// emit from a Right placement, so the wire's tangent at the
    /// source end is always +X.
    pub drag_connection: Option<DragConnection>,
}

impl Default for CanvasState {
    fn default() -> Self {
        Self {
            offset: egui::Vec2::ZERO,
            zoom: 1.0,
            rect_last: egui::Rect::NOTHING,
            tabs: vec![CanvasView::Main],
            active_tab: 0,
            last_active_tab: 0,
            pending_auto_layout_all: false,
            marquee_start: None,
            drag_connection: None,
        }
    }
}

impl CanvasState {
    /// Lower bound on `zoom`. Below this the whole graph collapses into
    /// an illegible cluster; the user can't recover it by scrolling.
    pub const MIN_ZOOM: f32 = 0.25;
    /// Upper bound on `zoom`. Beyond this a single node fills the
    /// viewport and panning becomes the only way to navigate.
    pub const MAX_ZOOM: f32 = 2.5;

    /// Map a graph-space position to a screen position.
    /// `screen = world * zoom + offset`.
    pub fn to_screen(&self, world: egui::Pos2) -> egui::Pos2 {
        (world.to_vec2() * self.zoom + self.offset).to_pos2()
    }

    /// Inverse of [`to_screen`]: map a screen position back to graph
    /// space. Used for hit-testing and palette drops.
    /// `world = (screen - offset) / zoom`.
    pub fn to_world(&self, screen: egui::Pos2) -> egui::Pos2 {
        ((screen.to_vec2() - self.offset) / self.zoom).to_pos2()
    }

    /// Apply a zoom step while keeping the world point currently under
    /// `cursor` (a screen position) fixed on screen. `factor` multiplies
    /// the current zoom; the result is clamped to
    /// [`MIN_ZOOM`, `MAX_ZOOM`]. `offset` is adjusted so the cursor
    /// stays anchored: from `screen = world*zoom + offset`, holding
    /// `world` and `screen` fixed gives
    /// `offset += world * (old_zoom - new_zoom)`.
    pub fn zoom_at(&mut self, cursor: egui::Pos2, factor: f32) {
        let old_zoom = self.zoom;
        let new_zoom = (old_zoom * factor).clamp(Self::MIN_ZOOM, Self::MAX_ZOOM);
        if new_zoom == old_zoom {
            return;
        }
        let world = self.to_world(cursor).to_vec2();
        self.zoom = new_zoom;
        self.offset += world * (old_zoom - new_zoom);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_screen_round_trips_through_to_world() {
        let state = CanvasState {
            offset: egui::vec2(120.0, -45.0),
            zoom: 1.8,
            ..Default::default()
        };
        let world = egui::pos2(37.0, 412.0);
        let back = state.to_world(state.to_screen(world));
        assert!((back - world).length() < 1e-3, "{back:?} != {world:?}");
    }

    #[test]
    fn identity_transform_at_default_zoom() {
        // zoom == 1.0 must reduce to the old `world + offset` mapping so
        // behaviour is unchanged until the user scrolls.
        let state = CanvasState {
            offset: egui::vec2(10.0, 20.0),
            ..Default::default()
        };
        let world = egui::pos2(5.0, 7.0);
        assert_eq!(state.to_screen(world), egui::pos2(15.0, 27.0));
    }

    #[test]
    fn zoom_at_keeps_cursor_point_anchored() {
        let mut state = CanvasState {
            offset: egui::vec2(60.0, 90.0),
            ..Default::default()
        };
        let cursor = egui::pos2(300.0, 220.0);
        let world_under_cursor = state.to_world(cursor);
        state.zoom_at(cursor, 1.3);
        // The same world point must still project to the cursor pixel.
        let after = state.to_screen(world_under_cursor);
        assert!((after - cursor).length() < 1e-3, "{after:?} != {cursor:?}");
    }

    #[test]
    fn zoom_at_clamps_to_bounds() {
        let mut state = CanvasState::default();
        for _ in 0..50 {
            state.zoom_at(egui::pos2(100.0, 100.0), 1.3);
        }
        assert!(state.zoom <= CanvasState::MAX_ZOOM + 1e-6);
        for _ in 0..100 {
            state.zoom_at(egui::pos2(100.0, 100.0), 0.7);
        }
        assert!(state.zoom >= CanvasState::MIN_ZOOM - 1e-6);
    }
}
