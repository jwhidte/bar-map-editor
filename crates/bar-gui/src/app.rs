use bar_graph::{GraphEngine, NodeId, NodeType, PortPlacement};
use eframe::egui;
use std::collections::HashMap;

use crate::log::LogLevel;
use crate::settings::Settings;
use crate::undo::UndoHistory;

// Re-export icon painters so that `use crate::app::*` in panel modules keeps
// finding them after they moved to panels/icons.rs.
pub(crate) use crate::panels::icons::{
    draw_io_icon, paint_atmosphere_icon, paint_bar_icon, paint_compile_icon, paint_dimensions_icon,
    paint_export_icon, paint_fog_icon, paint_grass_icon, paint_identity_icon, paint_lava_icon,
    paint_lighting_icon, paint_map_edge_icon, paint_physics_icon, paint_publish_icon,
    paint_resources_icon, paint_water_icon,
};

// Welcome-panel template list lives in `panels::welcome` now.

/// Port layout constants -- shared between rendering and wire drawing.
/// Title bar is 20 px, inset 8 px from the top edge; ports stack below it.
pub(crate) const PORT_Y_BASE: f32 = 38.0;
pub(crate) const PORT_Y_STEP: f32 = 20.0;
pub(crate) const TITLE_Y_OFFSET: f32 = 8.0;
pub(crate) const TOP_PORT_INSET: f32 = 28.0;
pub(crate) const TOP_PORT_STEP: f32 = 22.0;

/// Default visual size for `SubgraphInput` / `SubgraphOutput` nodes.
/// All other dimensions (chevron width, corner radius, icon size,
/// padding, text size) are computed at render time as proportions
/// of the node's actual height; the user can resize the node from
/// the corner anchors and every feature scales together.
pub(crate) const IO_NODE_SIZE: egui::Vec2 = egui::vec2(160.0, 52.0);

/// Reference height the proportional dimensions are calibrated
/// against. Heights other than this rescale every feature in
/// lock-step.
pub(crate) const IO_REF_H: f32 = 52.0;

/// Screen position of a node port. Replaces the old scalar `node_port_y`.
/// For IO nodes the port is always centered on the appropriate edge
/// regardless of placement or index. For regular nodes the position
/// depends on the placement kind:
///   Left / Right -- stacked at PORT_Y_BASE + side_index * PORT_Y_STEP
///   Top(slot)    -- fixed X slot on the top edge (centered vertically)
///   Bottom       -- centered on the bottom edge
///
/// `zoom` is the canvas zoom factor: `node_rect` is already in screen
/// space (its size scaled by zoom), so the fixed pixel offsets that
/// stack ports below the title bar must scale by the same factor or the
/// port handles drift off the (scaled) node body. At `zoom == 1.0` this
/// reduces to the original fixed-offset layout.
pub(crate) fn node_port_pos(
    node_type: &NodeType,
    node_rect: egui::Rect,
    placement: PortPlacement,
    side_index: usize,
    zoom: f32,
) -> egui::Pos2 {
    if matches!(
        node_type,
        NodeType::SubgraphInput | NodeType::SubgraphOutput
    ) {
        let x = match placement {
            PortPlacement::Right => node_rect.max.x,
            _ => node_rect.min.x,
        };
        return egui::pos2(x, node_rect.center().y);
    }
    match placement {
        PortPlacement::Left => egui::pos2(
            node_rect.min.x,
            node_rect.min.y + (PORT_Y_BASE + side_index as f32 * PORT_Y_STEP) * zoom,
        ),
        PortPlacement::Right => egui::pos2(
            node_rect.max.x,
            node_rect.min.y + (PORT_Y_BASE + side_index as f32 * PORT_Y_STEP) * zoom,
        ),
        PortPlacement::Top(slot) => egui::pos2(
            node_rect.min.x + (TOP_PORT_INSET + slot as f32 * TOP_PORT_STEP) * zoom,
            node_rect.min.y,
        ),
        PortPlacement::Bottom => egui::pos2(node_rect.center().x, node_rect.max.y),
    }
}

// `NodeVisual` and `GroupRuntime` live in `crate::state` so the undo
// module can snapshot them without going through stringly-typed JSON.
// Imported at the top of this file.

/// Parse a persisted `"<node_key>:<port_name>"` binding into runtime
/// `(NodeId, port_name)`. Returns `None` if the binding is missing,
/// malformed, or refers to a node key that didn't load.
pub(crate) fn parse_subgraph_binding(
    raw: Option<&str>,
    key_to_id: &HashMap<String, NodeId>,
) -> Option<(NodeId, String)> {
    let s = raw?;
    let (key, port_name) = s.split_once(':')?;

    let id = *key_to_id.get(key)?;
    Some((id, port_name.to_string()))
}

/// One-shot context-menu action carried out after the menu closes,
/// since the menu closure can't borrow `self` mutably while iterating
/// `self.visuals.groups`.
pub(crate) use crate::editor::{CanvasView, ValidationFilter};

pub(crate) enum GroupOp {
    CreateWith(NodeId),
    CreateFromSelection(Vec<NodeId>),
    AddTo(NodeId, u64),
    AddManyTo(Vec<NodeId>, u64),
    RemoveFrom(NodeId),
}

/// Fixed palette of subtle tints for group rectangles. Members store
/// an index into this so the colour serialises as a single u8.
pub(crate) const GROUP_PALETTE: &[(u8, u8, u8)] = &[
    (90, 110, 150), // slate blue
    (110, 130, 90), // moss
    (150, 110, 90), // terracotta
    (130, 90, 130), // mauve
    (90, 140, 130), // teal-grey
    (140, 130, 70), // ochre
];

pub(crate) fn group_color(idx: u8) -> egui::Color32 {
    let (r, g, b) = GROUP_PALETTE[idx as usize % GROUP_PALETTE.len()];
    egui::Color32::from_rgb(r, g, b)
}

/// Linear interpolation between two RGB colours. `t = 0.0` returns
/// `a`, `t = 1.0` returns `b`. Used to mix a SubGraph's identity
/// tint with the neutral tab background so tabs read as both
/// "tab-like" and "this group's tab".
pub(crate) fn blend(a: egui::Color32, b: egui::Color32, t: f32) -> egui::Color32 {
    let t = t.clamp(0.0, 1.0);
    let lerp = |x: u8, y: u8| (x as f32 * (1.0 - t) + y as f32 * t).round() as u8;
    egui::Color32::from_rgb(lerp(a.r(), b.r()), lerp(a.g(), b.g()), lerp(a.b(), b.b()))
}

pub(crate) use crate::dialog::{
    confirm_key_display_name, ConfirmAction, ConfirmDialog, DialogState, GroupDeleteChoice,
    PassthroughEdit, PendingAction, UnsavedDecision, CONFIRM_KEY_DELETE_CONNECTED_NODE,
};
pub(crate) use crate::editor::DragConnection;

/// What kind of palette item is being dragged. Regular node types
/// drop as a single node; macros drop as a SubGraph block plus a
/// freshly-instantiated cluster of inner nodes.
#[derive(Clone, Debug)]
pub enum PaletteKind {
    Node(NodeType),
    Macro {
        /// Canonical full name of one of the entries in `macros::BUILTIN_MACRO_GROUPS`.
        name: String,
    },
}

/// In-flight drag from the node palette onto the canvas.
#[derive(Clone, Debug)]
pub(crate) struct PaletteDrag {
    pub kind: PaletteKind,
    pub label: String,
}

pub use crate::paint::{BrushState, BrushTool, InspectorMode, PaintSession};

pub use crate::editor::SmfLightingSnapshot;

pub(crate) use crate::editor::{
    PendingPropsOpen, PropsTarget, PROPS_OPEN_DELAY_MS, PROPS_OPEN_MOVE_TOLERANCE,
};

pub use crate::editor::ExportStatus;

/// Top-level UI composition the user sees. Each variant maps to
/// one file in `crate::layouts::*` that decides which panels are
/// visible and how they're arranged. The active layout is purely a
/// UI/UX concern — the underlying `BarEditorApp` state (graph,
/// paint session, project, undo) is identical across layouts, so
/// switching is instant and never migrates data.
///
/// Today only `Standard` exists; future layouts (a sculpt-focus
/// view that hides the node graph, an export-only view that shows
/// just validation + bundler controls, etc.) drop in as new files
/// + a match arm in `layouts::dispatch::draw_active`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Layout {
    /// Node graph editor: left palette, central canvas, contextual
    /// properties, floating dialogs. No 3D viewport -- switch to
    /// Sculpt3D or use a future split layout for side-by-side editing.
    #[default]
    NodeGraph,
    /// Full-width 3D viewport with brush controls on the left.
    /// Writes brush strokes to `SculptState` directly; the
    /// export pipeline merges them onto graph output at bundle time.
    Sculpt3D,
    /// Read-only 3D viewport showing the compiled native-resolution
    /// BC1 texture. Available only when a compile has been run.
    Preview,
}

impl Layout {
    /// All variants in display order. Index 0 gets Ctrl+1, index 1 gets
    /// Ctrl+2, etc. Extend this slice when new layouts are added.
    pub const ALL: &'static [Layout] = &[Layout::NodeGraph, Layout::Sculpt3D, Layout::Preview];

    /// i18n key for this layout's display name.
    pub(crate) fn i18n_key(self) -> &'static str {
        match self {
            Layout::NodeGraph => "editor.menu.node_graph",
            Layout::Sculpt3D => "editor.menu.sculpt_3d",
            Layout::Preview => "editor.menu.preview",
        }
    }
}

pub(crate) use crate::editor::RecipeMeta;

/// Main application state for the BAR - Map Editor GUI. Field
/// ownership is grouped by concern; each sub-state lives in its own
/// module (see the field doc comments for the module path).
pub struct BarEditorApp {
    pub(crate) graph: GraphEngine,
    pub visuals: crate::editor::VisualsState,
    pub selection: crate::editor::SelectionState,
    pub canvas: crate::editor::CanvasState,
    pub map: crate::editor::MapState,
    pub(crate) history: UndoHistory,
    /// Side table holding interned paint-asset bytes referenced by undo
    /// snapshots. Each entry is content-hashed so equal bytes dedupe.
    pub(crate) paint_history: crate::paint_history::PaintHistoryStore,
    pub project: crate::project::ProjectState,
    /// Raw window + display handles of the editor's main window. Used
    /// to parent native file dialogs so they belong to the editor
    /// instead of whichever window happens to be foreground at the
    /// moment the dialog spawns. `bar-app` populates this each frame
    /// from `eframe::Frame`.
    pub(crate) parent_window_handles: Option<(
        raw_window_handle::RawWindowHandle,
        raw_window_handle::RawDisplayHandle,
    )>,
    pub preview: crate::editor::PreviewState,
    /// Available BAR game/engine versions. Populated by `bar-app` at startup
    /// once the BAR install is detected. Empty (no picker shown) if BAR is
    /// not installed or only one version of each is present.
    pub bar_versions: crate::editor::BarVersionState,
    pub dialog: DialogState,
    pub validation: crate::editor::ValidationState,
    pub props: crate::editor::PropsPanelState,
    /// Per-session state for the Map Edge action-bar panel (preview
    /// texture cache so flipping the modal doesn't re-decode the file).
    pub map_edge: crate::panels::action_bar_modals::map_edge::MapEdgePanelState,
    /// Per-session state for the Dimensions modal (minimap preview cache).
    pub dimensions: crate::panels::action_bar_modals::dimensions::DimensionsPanelState,
    /// Per-session state for the Assemble Map wizard. Holds the
    /// current page and the in-progress picks until Finish / Cancel.
    pub assemble_map: crate::panels::assemble_map::AssembleMapState,
    pub paint: PaintSession,
    /// In-flight drag from the node palette (set when pointer starts
    /// dragging an item, cleared on pointer release).
    pub(crate) palette_drag: Option<PaletteDrag>,
    /// Live text filter typed into the palette search box. Empty = show all.
    pub(crate) palette_filter: String,
    pub(crate) settings: Settings,
    /// Active top-level UI layout. Loaded from settings on launch,
    /// persisted via `set_active_layout`.
    pub(crate) active_layout: Layout,
    /// Set by `bar-app` from `GpuContext::supports_bc` on startup. Tells
    /// the Preview layout whether BC1 texture upload is available.
    pub supports_bc: bool,
    /// True when the wgpu adapter is software-only (`device_type ==
    /// DeviceType::Cpu`, e.g. lavapipe under WSLg without GPU
    /// passthrough). Set by `bar-app` at startup from
    /// `RenderState::adapter`. Layouts and rendering paths branch on
    /// this to disable expensive paths -- Preview is locked out, grass
    /// / advanced shading / shadows / features are short-circuited in
    /// Sculpt -- since a CPU-only Vulkan target makes those paths run
    /// at unusable framerates.
    pub software_renderer: bool,
    /// Sorted list of feature type names from the loaded catalog.
    /// Populated by `bar-app` when the feature catalog is loaded.
    pub feature_palette_names: Vec<String>,
    /// Live text filter typed into the feature library search box.
    /// Empty = show every catalog entry; otherwise case-insensitive
    /// substring match against the lowercased type name.
    pub(crate) feature_filter: String,
    /// Feature type currently selected in the feature palette for placement.
    /// When Some, clicking on the 3D terrain places a feature of this type.
    pub selected_feature_type: Option<String>,
    /// Spring heading the next placed feature will be created with. Set by
    /// the rotate gesture (Ctrl+scroll / horizontal scroll) while the user
    /// is in placement mode. Persists across placements so the user can
    /// drop a row of identically-oriented features without re-rotating.
    pub pending_placement_angle: f32,
    /// Translucent ghost of the to-be-placed feature, anchored at the
    /// cursor's terrain projection. Set by viewport input each frame
    /// while placement mode is active and the cursor is over the
    /// terrain; cleared otherwise. Drawn alongside committed features
    /// at reduced alpha so the user can preview position + rotation
    /// before the click commits.
    pub placement_ghost: Option<bar_project::recipe::PlacedFeature>,
    /// Viewport debug overlay toggles, exposed via the gear button in
    /// the Sculpt3D / Preview viewports. Session-only state -- not
    /// persisted across project loads.
    pub viewport_debug: ViewportDebug,
    /// Egui texture handles for rendered S3O thumbnails, keyed by
    /// lowercase feature type name. Populated by bar-app's runner
    /// when it drains `feature_thumb_requests`. Storing the handle
    /// (rather than just the `TextureId`) ties the GPU texture's
    /// lifetime to the cache entry -- dropping it via `remove` frees
    /// the egui side automatically.
    pub feature_thumb_cache: std::collections::HashMap<String, egui::TextureHandle>,
    /// Lowercase feature type names whose thumbnail the palette wants
    /// rendered. Bar-app's runner reads + clears entries it has
    /// fulfilled; the palette only inserts when a name is in neither
    /// `feature_thumb_cache` nor `feature_thumb_pending`, so the set
    /// converges rather than churning every frame.
    pub feature_thumb_requests: std::collections::HashSet<String>,
    /// Names the runner has kicked an S3O load for but hasn't yet
    /// uploaded a mesh + rendered a thumbnail. Used as a gate so the
    /// palette doesn't re-request every frame while a load is in
    /// flight. Cleared by the runner when the mesh arrives (so the
    /// palette re-fires the request and the thumbnail renders) or
    /// when the load fails terminally.
    pub feature_thumb_pending: std::collections::HashSet<String>,
    /// Live-preview handoff for the Layout node-edit view. The GUI
    /// sets `node` + `dirty` when the edited node's geometry changes;
    /// bar-app's runner evaluates that one node in isolation, renders
    /// it through the terrain renderer, uploads the result into
    /// `texture`, and clears `dirty`. The edit view draws `texture`
    /// if present, else a placeholder.
    pub layout_preview: LayoutPreview,
}

/// See [`BarEditorApp::layout_preview`].
#[derive(Default)]
pub struct LayoutPreview {
    pub node: Option<bar_graph::NodeId>,
    pub dirty: bool,
    /// Shared 3D-rendering primitive for the preview. Owns the
    /// `TerrainRenderer`, its camera, and the registered egui
    /// texture. Lazily constructed by bar-app's runner on the first
    /// frame the GPU context is available; both the runner (render +
    /// texture bind) and the edit view's pane (paint + camera
    /// input) operate on the same instance. See
    /// `crates/bar-gui/src/terrain_pane.rs` and
    /// `docs/terrain-pane-plan.md`.
    pub pane: Option<crate::terrain_pane::TerrainPane>,
}

/// Per-session viewport debug toggles. Surfaced via a small gear menu
/// in the Sculpt3D / Preview viewports.
#[derive(Clone, Copy, Debug)]
pub struct ViewportDebug {
    /// When true, render a corner overlay with the camera's world-space
    /// position + orientation. Useful when reproducing rendering bugs
    /// against specific viewpoints.
    pub show_camera_readout: bool,
    /// Exponent fed into the gamma post-pass uniform. 1.0 disables the
    /// correction (raw perceptual pixels through eframe's swapchain --
    /// visibly too bright), 2.2 applies the full display gamma decode
    /// (overshoots dark because egui_wgpu's compose does partial gamma
    /// handling already). Tuned visually against in-engine ref shots.
    pub gamma_exponent: f32,
    /// Grass-shader debug output selector. Bypasses the full blend
    /// pipeline so individual sampling stages can be inspected:
    ///   0 = normal output
    ///   1 = raw `map_color` (grassShadingTex sample)
    ///   2 = raw blade-colour sample
    ///   3 = post-blend rgb (before modulator)
    /// Fed into `grass_params.dbg.x`.
    pub grass_debug_output: i32,
    /// Grass alpha-test technique:
    ///   0 = hashed alpha (Wronski 2017 stochastic discard, BME default)
    ///   1 = binary discard at ALPHATHRESHOLD only -- matches the
    ///       engine widget's `frag.glsl:48` gate, without the MSAA +
    ///       AtoC path BME can't reproduce at sample_count=1.
    /// Useful for isolating whether the silhouette character comes
    /// from the alpha-test technique or from the colour pipeline.
    pub grass_alpha_test_mode: u32,
}

impl Default for ViewportDebug {
    fn default() -> Self {
        Self {
            show_camera_readout: false,
            gamma_exponent: 1.5,
            grass_debug_output: 0,
            grass_alpha_test_mode: 0,
        }
    }
}

impl Default for BarEditorApp {
    fn default() -> Self {
        Self {
            graph: GraphEngine::new(),
            visuals: crate::editor::VisualsState {
                next_group_id: 1,
                ..Default::default()
            },
            selection: crate::editor::SelectionState::default(),
            canvas: crate::editor::CanvasState::default(),
            map: crate::editor::MapState {
                width: 513,
                height: 513,
                min_height: 0.0,
                max_height: 800.0,
                ..Default::default()
            },
            history: UndoHistory::default(),
            paint_history: crate::paint_history::PaintHistoryStore::new(),
            project: crate::project::ProjectState::default(),
            parent_window_handles: None,
            preview: crate::editor::PreviewState::default(),
            bar_versions: crate::editor::BarVersionState::default(),
            dialog: DialogState::default(),
            validation: crate::editor::ValidationState::default(),
            props: crate::editor::PropsPanelState::default(),
            map_edge: crate::panels::action_bar_modals::map_edge::MapEdgePanelState::default(),
            dimensions: crate::panels::action_bar_modals::dimensions::DimensionsPanelState::default(
            ),
            assemble_map: crate::panels::assemble_map::AssembleMapState::default(),
            paint: PaintSession::default(),
            palette_drag: None,
            palette_filter: String::new(),
            settings: Settings::default(),
            active_layout: Layout::default(),
            supports_bc: false,
            software_renderer: false,
            feature_palette_names: Vec::new(),
            feature_filter: String::new(),
            selected_feature_type: None,
            pending_placement_angle: 0.0,
            placement_ghost: None,
            viewport_debug: ViewportDebug::default(),
            feature_thumb_cache: std::collections::HashMap::new(),
            layout_preview: LayoutPreview::default(),
            feature_thumb_requests: std::collections::HashSet::new(),
            feature_thumb_pending: std::collections::HashSet::new(),
        }
    }
}

impl BarEditorApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // Bundle BAR's Exo2 family and the host OS's symbol coverage
        // into a single FontDefinitions and register them once. Both
        // sets are needed up front: BAR's font powers the metal-spot
        // labels (and is available as `FontFamily::Name("bar")` for
        // other in-engine-style overlays); the system symbol fallback
        // keeps egui from rendering grey "missing glyph" boxes for
        // bullets, arrows, and the multiplication sign.
        crate::state::install_custom_fonts(&cc.egui_ctx);
        let mut app = Self::default();
        app.settings = Settings::load();
        // Restore the layout the user last had selected, falling
        // back to `Default` when settings are absent or pre-date
        // the field.
        app.active_layout = app.settings.active_layout;
        // Sculpt3D is currently hidden from the UI (see Layout::ALL).
        // Coerce a stale persisted choice back to the default so the
        // user lands somewhere visible on launch.
        if !Layout::ALL.contains(&app.active_layout) {
            app.active_layout = Layout::default();
            app.settings.active_layout = app.active_layout;
        }
        // Drop recents that no longer exist on disk so the menu stays useful.
        app.settings.recent_files.retain(|p| p.exists());
        // Reopen the most-recently-loaded project on launch so the
        // user picks up where they left off. Skipped when the user
        // turned the preference off, when there are no recent files,
        // or when the most recent file no longer exists. Errors are
        // surfaced as a status message; we don't crash the launch.
        if app.settings.restore_last_project {
            // Only restore .barproj files. .sd7 entries can appear in
            // recents for the menu but should never be auto-loaded on
            // startup -- they are import sources, not BME projects.
            if let Some(last) = app.settings.recent_files.first().cloned() {
                let is_barproj = last
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.eq_ignore_ascii_case("barproj"))
                    .unwrap_or(false);
                if is_barproj && last.exists() {
                    app.load_project(last);
                }
            }
        }
        app
    }

    pub fn graph(&self) -> &GraphEngine {
        &self.graph
    }

    // `set_parent_window_handles`, `parent_window`, `make_dialog`, and
    // `ParentWindow` itself live in `crate::io::dialogs` (distributed
    // `impl BarEditorApp` block).
}

impl BarEditorApp {
    /// Read-only access to user settings (vertical exaggeration, etc.).
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// Set the selected BAR game archive used for the feature catalog and
    /// persist the change to disk. Called from bar-app when auto-detection
    /// finds an archive the user hasn't configured yet.
    pub fn set_game_archive(&mut self, path: std::path::PathBuf) {
        self.settings.selected_game_archive = Some(path);
        self.settings.save();
    }

    /// Persist `bar_install_path` and save. Called from bar-app once
    /// on first launch when auto-detection finds an install the
    /// user hasn't configured yet.
    pub fn set_bar_install_path(&mut self, path: Option<std::path::PathBuf>) {
        self.settings.bar_install_path = path;
        self.settings.save();
    }

    /// True when the user is currently looking at a subgraph tab —
    /// the palette uses this to gate the "SubGraph IO" group so
    /// `SubgraphInput`/`SubgraphOutput` can't be dropped at the
    /// top level by accident.
    pub(crate) fn is_in_subgraph_view(&self) -> bool {
        matches!(
            self.canvas.tabs.get(self.canvas.active_tab),
            Some(CanvasView::SubGraph(_))
        )
    }

    /// Set the in-flight palette drag — populated when the user
    /// starts dragging a palette item; consumed by the canvas drop
    /// handler.
    pub(crate) fn set_palette_drag(&mut self, drag: PaletteDrag) {
        self.palette_drag = Some(drag);
    }

    /// Mutable access to the recipe-identity meta block — the
    /// mapinfo editor's Identity tab binds egui text fields
    /// directly into these.
    pub(crate) fn recipe_meta_mut(&mut self) -> &mut RecipeMeta {
        self.map.recipe_meta_mut()
    }
    /// Mutable access to MapSettings — binds the Physics /
    /// Atmosphere / Lighting / Water tabs.
    pub(crate) fn map_settings_mut(&mut self) -> &mut bar_project::MapSettings {
        self.map.settings_mut()
    }
    /// Read-only access to MapSettings. Used by `bar-app` to read fields
    /// (e.g. `atmosphere.skybox`) that drive asset loading at the
    /// renderer side.
    pub fn map_settings(&self) -> &bar_project::MapSettings {
        &self.map.settings
    }
    pub(crate) fn map_dimensions_mut(&mut self) -> (&mut u32, &mut u32) {
        self.map.dimensions_mut()
    }
    pub(crate) fn map_height_range_mut(&mut self) -> (&mut f32, &mut f32) {
        self.map.height_range_mut()
    }
    /// Mutable access to the live paint session — brush, sculpt
    /// lock, and per-layer paint caches. The 2D inspector and
    /// properties panel both need to mutate brush state and
    /// inspector caches.
    pub(crate) fn paint_mut(&mut self) -> &mut PaintSession {
        &mut self.paint
    }
    pub(crate) fn paint(&self) -> &PaintSession {
        &self.paint
    }

    /// Spawn-marker drag-index accessor used by the inspector's
    /// drag-and-drop handling.
    pub(crate) fn dragging_spawn(&self) -> Option<usize> {
        self.map.dragging_spawn
    }
    pub(crate) fn set_dragging_spawn(&mut self, idx: Option<usize>) {
        self.map.dragging_spawn = idx;
    }

    /// Mark the project as dirty (unsaved changes pending).
    pub(crate) fn mark_dirty(&mut self) {
        self.project.is_dirty = true;
        self.project.compile_dirty = true;
        // Bumping the commit counter feeds the validation
        // fingerprint, so a re-validation fires on the next frame
        // after any committable input event (blur, drag-stop, etc.).
        self.project.commits = self.project.commits.wrapping_add(1);
    }

    /// Append a message to the log buffer. Info/Warning/Error also update
    /// the footer status bar. Debug goes to the buffer only (never shown
    /// in the footer).
    pub(crate) fn log(&mut self, level: LogLevel, msg: impl Into<String>) {
        let msg = msg.into();
        if level != LogLevel::Debug {
            self.dialog.status_message = Some(msg.clone());
            self.dialog.status_level = level;
        }
        self.dialog.log_buffer.push(level, msg);
    }

    pub(crate) fn log_info(&mut self, msg: impl Into<String>) {
        self.log(LogLevel::Info, msg);
    }

    pub(crate) fn log_warning(&mut self, msg: impl Into<String>) {
        self.log(LogLevel::Warning, msg);
    }

    pub(crate) fn log_error(&mut self, msg: impl Into<String>) {
        self.log(LogLevel::Error, msg);
    }

    #[allow(dead_code)]
    pub(crate) fn log_debug(&mut self, msg: impl Into<String>) {
        self.log(LogLevel::Debug, msg);
    }

    /// Status-bar message setter used by panels that need to
    /// surface a result without going through the toast path
    /// (e.g. "Sculpt saved to /path/to/foo.png" after the user
    /// triggers a save dialog inside the inspector).
    pub(crate) fn set_status_message(&mut self, msg: String) {
        self.log_info(msg);
    }

    /// True if the project has unsaved changes since the last save/load.
    pub fn is_dirty(&self) -> bool {
        self.project.is_dirty()
    }

    /// True for one frame after the user has acknowledged the unsaved-changes
    /// dialog and confirmed a close (or there were no changes to discard).
    /// `bar-app` polls this each frame; `true` means the OS close request can
    /// be allowed to proceed. Also persists settings on the way out so window
    /// state, recents, and prefs survive a clean shutdown.
    pub fn take_allow_close(&mut self) -> bool {
        let v = self.dialog.allow_close;
        self.dialog.allow_close = false;
        if v {
            self.settings.save();
        }
        v
    }

    /// Update the persisted window position/size. Called once per frame from
    /// `bar-app` with the current viewport rect (no-op cost if unchanged —
    /// only the in-memory struct is touched; disk writes happen on close).
    pub fn update_window_state(
        &mut self,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        maximized: bool,
    ) {
        // When the window is maximized, the OS-reported rect is the
        // full screen — useless for restoring a sane "windowed" size on
        // next launch. Keep whatever rect was last saved while
        // unmaximized so the user's preferred restored dimensions
        // survive a maximize/unmaximize cycle.
        let new = if maximized {
            let prev = self.settings.window.as_ref();
            crate::settings::WindowState {
                x: prev.map(|w| w.x).unwrap_or(x),
                y: prev.map(|w| w.y).unwrap_or(y),
                width: prev.map(|w| w.width).unwrap_or(width),
                height: prev.map(|w| w.height).unwrap_or(height),
                maximized: true,
            }
        } else {
            crate::settings::WindowState {
                x,
                y,
                width,
                height,
                maximized: false,
            }
        };
        let differs = self
            .settings
            .window
            .as_ref()
            .map(|w| {
                (w.x - new.x).abs() > 0.5
                    || (w.y - new.y).abs() > 0.5
                    || (w.width - new.width).abs() > 0.5
                    || (w.height - new.height).abs() > 0.5
                    || w.maximized != new.maximized
            })
            .unwrap_or(true);
        if differs {
            self.settings.window = Some(new);
        }
    }

    /// Tell the GUI that the OS / user requested to close the window. If the
    /// project is dirty, this opens a save/discard/cancel modal. Otherwise
    /// the close is approved immediately (next `take_allow_close` returns
    /// true).
    pub fn request_close(&mut self) {
        if self.project.is_dirty {
            self.dialog.pending_action = Some(PendingAction::Close);
        } else {
            self.dialog.allow_close = true;
        }
    }

    /// Open a path queued externally (e.g. a drag-drop drop target). Routes
    /// through the unsaved-changes dialog if needed.
    pub fn open_path_external(&mut self, path: std::path::PathBuf) {
        self.start_open_path(path);
    }

    /// Begin a New Project, deferring through unsaved-changes confirmation
    /// when the current project is dirty.
    pub(crate) fn start_new_project(&mut self) {
        if self.project.is_dirty {
            self.dialog.pending_action = Some(PendingAction::NewProject);
        } else {
            self.do_new_project();
        }
    }

    /// Handle a click on the Edit Map Info toolbar button. If the user has
    /// already designated a map-info file for this project, open it in the
    /// in-app floating editor. Otherwise, prompt them to pick from the
    /// bundle's passthrough files.
    /// Render the 2D inspector window. Backdrop is the latest evaluated
    /// heightmap (rendered grayscale-with-water-tint); start positions are
    /// draggable markers on top.
    /// 2D Inspector window - see `crate::panels::inspector`.
    pub(crate) fn draw_inspector_window(&mut self, ctx: &egui::Context) {
        crate::panels::inspector::draw(self, ctx);
    }

    /// Render the structured Map Info editor: a single window with one
    /// `CollapsingHeader` per major section of the recipe + `MapSettings`.
    /// Edits write directly into `self.map.settings` and the recipe-side
    /// mirror fields. On save those values are folded into the project's
    /// `Recipe` and `MapSettings`.
    /// Perform the actual node deletion (the destructive-confirm path runs
    /// the dialog first; the no-confirm path calls this directly).
    /// Delete a SubGraph + every member node it owns, in one undo
    /// step. Used by the Delete-key path, the collapsed-block right-
    /// click "Delete subgraph", and the properties-panel "Delete"
    /// button when the selected group is a SubGraph. Skips the
    /// "delete with members or just dissolve?" modal because for a
    /// SubGraph the inner nodes are conceptually part of the same
    /// unit — leaving them orphaned is almost never what the user
    /// wants. Push_undo takes the full editor-state snapshot, so
    /// undo restores the SubGraph with all its inner nodes,
    /// connections, ports, bindings, and macro params intact.
    pub(crate) fn delete_subgraph_with_contents(&mut self, gid: u64) {
        if !self.visuals.groups.contains_key(&gid) {
            return;
        }
        self.push_undo("Delete subgraph");
        let members: Vec<NodeId> = self
            .visuals
            .groups
            .get(&gid)
            .map(|g| g.member_ids.iter().copied().collect())
            .unwrap_or_default();
        self.dissolve_group(gid);
        if self.selection.group == Some(gid) {
            self.selection.group = None;
        }
        for nid in &members {
            if !self.graph.can_delete_node(*nid) {
                continue;
            }
            let _ = self.graph.remove_node(*nid);
            self.visuals.node_visuals.remove(nid);
            self.remove_node_from_group(*nid);
        }
        self.project.passthrough_edit = None;
        self.clear_selection();
    }

    pub(crate) fn delete_selected_node(&mut self) {
        // Snapshot the IDs to delete: the primary plus everything else
        // in the multi-selection set. (The set always includes the
        // primary by invariant.) FinalComposition is filtered out
        // here -- it's the singleton terminal node and deleting it
        // would orphan everything downstream of the eval graph.
        let raw: Vec<NodeId> = if !self.selection.nodes.is_empty() {
            self.selection.nodes.iter().copied().collect()
        } else if let Some(id) = self.selection.node {
            vec![id]
        } else {
            return;
        };
        let to_delete: Vec<NodeId> = raw
            .into_iter()
            .filter(|id| self.graph.can_delete_node(*id))
            .collect();
        if to_delete.is_empty() {
            return;
        }
        self.push_undo("Delete node");
        for node_id in &to_delete {
            let _ = self.graph.remove_node(*node_id);
            self.visuals.node_visuals.remove(node_id);
            self.remove_node_from_group(*node_id);
        }
        self.project.passthrough_edit = None;
        self.clear_selection();
    }

    /// Resolve a queued PendingAction now that the unsaved-changes prompt
    /// has been answered.
    pub(crate) fn apply_pending_action(&mut self, action: PendingAction) {
        match action {
            PendingAction::Close => {
                self.dialog.allow_close = true;
            }
            PendingAction::NewProject => {
                self.do_new_project();
            }
            PendingAction::OpenPath(p) => {
                self.dispatch_open(p);
            }
            PendingAction::LoadMacro { name } => {
                self.start_with_macro(&name);
            }
        }
    }

    pub fn graph_mut(&mut self) -> &mut GraphEngine {
        &mut self.graph
    }

    /// Current sun direction with engine-faithful renormalisation
    /// applied. Read by the in-viewport sun gizmo so the gizmo
    /// position stays on the configured arc even when the recipe
    /// stores an un-normalised vector. Falls back to the engine
    /// default when no override is set.
    pub fn sun_dir_normalised(&self) -> [f32; 3] {
        let raw = self.map.settings.lighting.resolved().sun_dir;
        let v = glam::Vec3::from(raw);
        let n = v.length();
        let unit = if n.is_finite() && n > 1e-6 {
            v / n
        } else {
            glam::Vec3::from(bar_project::engine_defaults::LIGHTING_SUN_DIR).normalize()
        };
        [unit.x, unit.y, unit.z]
    }

    /// Set the sun direction from a UI gizmo drag. Bypasses the
    /// schema renderer (which produces one undo entry per axis edit
    /// session) so a continuous spherical drag commits as a single
    /// undo entry -- caller pushes that entry on drag-start. Marks
    /// the project dirty so the validation + autosave + compile-
    /// dirty hooks fire normally.
    pub fn set_sun_dir(&mut self, dir: [f32; 3]) {
        self.map.settings_mut().lighting.sun_dir = Some(dir);
        self.mark_dirty();
    }

    /// SMF ground shading inputs sourced from `MapSettings.lighting`
    /// and `MapSettings.water`. Snapshot of the values an in-engine
    /// renderer would read for the same map. Consumers (bar-app's
    /// preview pipeline) clone this each frame; never store.
    pub fn smf_lighting(&self) -> SmfLightingSnapshot {
        // Single source of truth: MapSettings -> bar_render::SmfLighting.
        // GUI + CLI + viewport all go through the same `From` impl in
        // bar-render, so there's no second-copy drift.
        SmfLightingSnapshot::from(&self.map.settings)
    }

    /// Node currently targeted by the Layout edit-view preview, with a
    /// clone of its type + params, if any. bar-app's runner calls this
    /// each frame to evaluate that one node in isolation and render the
    /// live preview. Returns `None` when no Layout node is being edited
    /// or the node has since been deleted.
    pub fn layout_preview_node(
        &self,
    ) -> Option<(
        bar_graph::NodeId,
        bar_graph::NodeType,
        std::collections::HashMap<String, bar_graph::ParamValue>,
    )> {
        let id = self.layout_preview.node?;
        let node = self.graph.get_node(id)?;
        Some((id, node.node_type.clone(), node.params.clone()))
    }

    /// Composite cache key for the 3D preview's input state. Bumps
    /// whenever ANY input that the preview depends on changes — graph
    /// revision, the active preview-target node, map dimensions, or
    /// the height range. The preview pipeline gates re-evaluations on
    /// this single value rather than on `graph.revision()` alone, so
    /// switching the preview target or resizing the map triggers a
    /// fresh eval the same way a graph mutation does.
    ///
    /// Hash output rather than tuple comparison so the gate value
    /// stays a single u64 — keeps the in-flight `PreviewResult` tag
    /// the same shape as before and the renderer's frame-revision
    /// arithmetic compatible.
    pub fn preview_cache_key(&self) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut h = DefaultHasher::new();
        // Hash upstream of the first Bundler so disconnected nodes
        // don't trigger a re-render; fall back to full graph revision.
        let bundler_id = self
            .graph
            .nodes()
            .iter()
            .find(|(_, n)| n.node_type == bar_graph::NodeType::FinalComposition)
            .map(|(id, _)| *id);
        if let Some(bn) = bundler_id {
            self.graph.upstream_content_hash(bn).hash(&mut h);
        } else {
            self.graph.revision().hash(&mut h);
        }
        self.map.width.hash(&mut h);
        self.map.height.hash(&mut h);
        self.map.min_height.to_bits().hash(&mut h);
        self.map.max_height.to_bits().hash(&mut h);
        // Paint asset bytes live outside the graph (in
        // `<project>/assets/*.bin`), so changing them doesn't bump
        // `upstream_content_hash`. Mix in a counter that's incremented
        // by paint-asset restores in `restore_snapshot` so any paint
        // mutation re-fires eval.
        self.paint.asset_revision.hash(&mut h);
        h.finish()
    }

    /// Build a `Recipe` snapshot with the live identity / dimensions /
    /// `MapSettings` the user has been editing. Used by `bar-app` when it
    /// fires off an export thread — the bundler reads identity from
    /// `Recipe`, so the simple "MapSettings::default()" shortcut no
    /// longer suffices. The graph nodes/connections are left empty
    /// because `execute_bundlers` walks the live `GraphEngine` directly,
    /// not the recipe — but identity, dimensions, and `MapSettings`
    /// must come from this snapshot.
    pub fn recipe_for_export(&self) -> bar_project::Recipe {
        bar_project::Recipe {
            // Recipe `name` is the engine-visible map identity (used
            // to build the archive ID `name .. " " .. version`).
            // Prefer the source-mapinfo name from `recipe_meta` (set
            // by `apply_project` on import); only fall back to the
            // `.barproj` directory stem for projects that have no
            // source mapinfo (freshly created). Without this, the
            // bundler emits the lowercase slug as the engine's map
            // name and the script's `MapName=` lookup misses the
            // archive.
            name: self
                .map
                .recipe_meta
                .name
                .clone()
                .filter(|s| !s.is_empty())
                .or_else(|| {
                    self.project
                        .path
                        .as_ref()
                        .and_then(|p| p.file_stem())
                        .map(|s| s.to_string_lossy().to_string())
                })
                .or_else(|| self.project.loaded_name.clone())
                .unwrap_or_else(|| "Untitled".to_string()),
            shortname: self.map.recipe_meta.shortname.clone(),
            description: self.map.recipe_meta.description.clone(),
            author: self.map.recipe_meta.author.clone(),
            version: self.map.recipe_meta.version.clone(),
            tip: self.map.recipe_meta.tip.clone(),
            depend: self.map.recipe_meta.depend.clone(),
            nodes: Vec::new(),
            connections: Vec::new(),
            output: bar_project::OutputConfig {
                width: self.map.width,
                height: self.map.height,
                map_settings: bar_project::MapSettings {
                    min_height: Some(self.map.min_height),
                    max_height: Some(self.map.max_height),
                    ..self.map.settings.clone()
                },
            },
            features: self.map.features.clone(),
        }
    }

    /// Set a status message to show in the status bar.
    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.log_info(msg);
    }

    /// Log a message at an explicit level. Used by the tracing bridge in bar-app.
    pub fn log_at(&mut self, level: crate::LogLevel, msg: impl Into<String>) {
        self.log(level, msg);
    }

    /// Clear the status message.
    pub fn clear_status(&mut self) {
        self.dialog.status_message = None;
        self.dialog.status_level = LogLevel::Info;
    }

    /// Push the latest evaluated heightmap so the 2D inspector can show
    /// it as a backdrop. Cheap when called every frame: only stores a
    /// clone and bumps a revision counter; the texture re-upload happens
    /// lazily inside the inspector's draw path.
    ///
    /// If the user has sculpted since the last reset, we keep their
    /// edits and ignore the incoming heightmap. Cleared by starting a
    /// new project.
    pub fn set_inspector_heightmap(&mut self, hm: bar_data::Heightmap) {
        self.paint.heightmap = Some(hm);
        self.paint.heightmap_rev = self.paint.heightmap_rev.wrapping_add(1);
    }

    pub fn sculpt_input_active(&self) -> bool {
        self.paint.brush.tool != BrushTool::Pointer
            && (self.active_layout == Layout::Sculpt3D
                || (self.paint.inspector_mode == InspectorMode::Sculpt
                    && self.dialog.show_inspector))
    }

    /// Current brush radius in heightmap pixels. The 3D viewport
    /// reads this to compute the world-space radius for the cursor
    /// ring overlay.
    pub fn brush_radius_px(&self) -> f32 {
        self.paint.brush.radius_px
    }

    /// Node palette - see `crate::panels::palette`.
    pub(crate) fn draw_node_palette(&mut self, ui: &mut egui::Ui) {
        crate::panels::palette::draw(self, ui);
    }
}

impl eframe::App for BarEditorApp {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        // Single dispatch point: the active `Layout` decides which
        // panels render and how they're arranged. Future layouts
        // plug in here without touching this body.
        crate::layouts::dispatch::draw_active(self, ctx, frame);
    }
}

impl BarEditorApp {
    /// The currently selected top-level UI layout.
    pub fn active_layout(&self) -> Layout {
        self.active_layout
    }

    /// True when the given layout is a 3D one (Sculpt3D / Preview) and
    /// the wgpu adapter is software-only -- those layouts are gated
    /// off entirely on lavapipe because the terrain shader alone
    /// pegs CPU cores. NodeGraph is always permitted.
    pub fn layout_blocked_by_software(&self, layout: Layout) -> bool {
        self.software_renderer && matches!(layout, Layout::Sculpt3D | Layout::Preview)
    }

    /// Switch to a different layout. Layouts are pure UI/UX, so the
    /// switch is instant and never migrates project state. Persists
    /// the choice via `Settings` so the user picks up where they
    /// left off after a restart.
    ///
    /// Software-only adapters (`software_renderer == true`) are locked
    /// to NodeGraph: the 3D layouts (Sculpt3D, Preview) run their
    /// terrain shader on the CPU under lavapipe and are unusable.
    /// Calls to switch into a blocked layout are silently ignored;
    /// the UI also disables those menu entries so this path is only
    /// hit via stale keyboard shortcuts or persisted settings.
    pub fn set_active_layout(&mut self, layout: Layout) {
        if self.layout_blocked_by_software(layout) {
            return;
        }
        if self.active_layout == layout {
            return;
        }
        self.active_layout = layout;
        self.settings.active_layout = layout;
        self.settings.save();
        // Close windows that are only meaningful in the layout we just left.
        // Map info editor is the exception -- it spans all layouts.
        self.dialog.show_inspector = false;
        self.dialog.show_validation_panel = false;
        self.props.active = None;
        self.dialog.pending_props_open = None;
    }
}

// Brush dab math + tests live in `crate::paint::brush_math`. Re-exported
// here under the historical name so existing callers don't break.
pub(crate) use crate::paint::brush_math::apply_brush_dab;

pub(crate) use crate::io::dialogs::make_path_dialog;
pub(crate) use crate::io::png::{heightmap_to_color_image, save_heightmap_as_png16};
pub(crate) use crate::panels::canvas::passthrough::{
    build_path_tree, draw_passthrough_body, draw_path_tree,
};
pub(crate) use crate::panels::canvas::style::{
    build_io_outline, cubic_bezier, node_type_color, polyline_distance, port_kind_color,
};

/// Encode a byte slice as a lowercase hex string (2 chars per byte).
pub(crate) fn mask_hex_encode(data: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(data.len() * 2);
    for b in data {
        let _ = write!(out, "{:02x}", b);
    }
    out
}

/// Parse a 6-character `RRGGBB` hex string into an `[r, g, b]` array.
/// Returns `None` if the string isn't six valid hex digits.
pub(crate) fn parse_hex_color(s: &str) -> Option<[u8; 3]> {
    let bytes = s.as_bytes();
    if bytes.len() != 6 {
        return None;
    }
    let mut out = [0u8; 3];
    for i in 0..3 {
        let hi = (bytes[i * 2] as char).to_digit(16)?;
        let lo = (bytes[i * 2 + 1] as char).to_digit(16)?;
        out[i] = (hi << 4 | lo) as u8;
    }
    Some(out)
}

/// Decode a hex string into bytes. Non-hex or odd-length trailing chars are skipped.
pub(crate) fn mask_hex_decode(s: &str) -> Vec<u8> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 2);
    let mut i = 0;
    while i + 1 < bytes.len() {
        let hi = (bytes[i] as char).to_digit(16);
        let lo = (bytes[i + 1] as char).to_digit(16);
        if let (Some(h), Some(l)) = (hi, lo) {
            out.push((h << 4 | l) as u8);
        }
        i += 2;
    }
    out
}

// Pure path helpers (PassThrough files, asset packing, bar:// URL
// resolution) live in `crate::project::path`. Re-export the names that
// callers outside this module use historically.
pub(crate) use crate::project::path::parse_passthrough_files;

// brush_tests module relocated to `crate::paint::brush_math`.
// session_reset_tests module relocated to `crate::project::lifecycle`.
