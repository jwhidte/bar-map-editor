//! Project lifecycle: new / load / save / reset orchestration.
//!
//! Distributed `impl BarEditorApp` block. The single big function
//! here is `reset_project`, which is the *only* place that wipes
//! every per-project field on `BarEditorApp` -- every project-
//! switching path (new, open .barproj, open .sd7, load macro,
//! close) calls it first and then installs new state on top of the
//! blank slate. Adding a new per-project field anywhere in
//! `BarEditorApp` should come with a matching reset here.

use bar_graph::{GraphEngine, Node, NodeId, NodeType};
use eframe::egui;

use crate::app::{BarEditorApp, CanvasView, PendingAction, RecipeMeta};
use crate::state::NodeVisual;
use crate::t;

impl BarEditorApp {
    /// Wipe every transient + per-project field so the editor is in a
    /// well-defined "no project loaded" state. This is the ONLY
    /// place where project state is cleared en masse. Every
    /// project-switching path (new, open .barproj, open .sd7,
    /// load macro preset, close) calls this first, then installs
    /// new state on top of the blank slate.
    pub(crate) fn reset_project(&mut self) {
        // Graph engine — counter resets to 1 so the next project
        // gets clean NodeIds with no risk of colliding with stale
        // group member_ids from the previous project.
        self.graph = GraphEngine::new();
        self.visuals.node_visuals.clear();

        // Group / subgraph state — must be cleared together with the
        // graph so stale member_ids can never match new NodeIds.
        self.visuals.groups.clear();
        self.visuals.node_to_group.clear();
        self.visuals.next_group_id = 1;

        // Project identity and output configuration.
        self.project.path = None;
        self.project.loaded_name = None;
        self.project.is_dirty = false;
        // Compile tracking is per-project. Crossing a project
        // boundary without clearing these leaks "this project has
        // been compiled" state from the previous one, which makes
        // the Test-in-BAR chain skip the Compile step on a fresh
        // import even though the new project's `compiled/` dir
        // doesn't exist yet. Time fields aren't persisted (Instant
        // is monotonic-clock; never reaches disk), so the only way
        // they could be set after a fresh load is via this leak.
        self.project.compile_dirty = true;
        self.project.compiled_at = None;
        self.map.settings = bar_project::MapSettings::default();
        self.map.width = 513;
        self.map.height = 513;
        self.map.min_height = 0.0;
        self.map.max_height = 800.0;
        self.map.recipe_meta = RecipeMeta::default();
        self.map.features = Vec::new();
        self.map.selected_feature_idx = None;
        self.selected_feature_type = None;

        // Signal renderers to flush stale GPU resources.
        self.project.graph_reset = true;

        // Undo history — never cross a project boundary, otherwise
        // Ctrl+Z would resurrect nodes from a different project.
        self.history.clear();

        // Brush state + live paint caches — owned by `PaintSession`
        // which knows how to drop them all together. Sculpt lock
        // also released; the next graph eval repopulates the
        // heightmap from scratch.
        self.paint.invalidate_on_graph_reset();

        // Validation panel — findings cache from a different graph
        // would lie about the current state. Filter and panel-open
        // flag also reset so the user sees a clean panel state next
        // project.
        self.dialog.show_validation_panel = false;
        self.validation.reset();

        // Modal / window-open flags. These should never persist
        // across a project switch — the user expects the new project
        // to open with no dialogs up.
        self.dialog.show_inspector = false;
        self.dialog.show_identity_editor = false;
        self.dialog.show_dimensions_editor = false;
        self.dialog.show_physics_editor = false;
        self.dialog.show_atmosphere_editor = false;
        self.dialog.show_lighting_editor = false;
        self.dialog.show_water_editor = false;
        self.dialog.show_resources_editor = false;
        self.dialog.show_grass_editor = false;
        self.dialog.show_map_edge_editor = false;
        self.dialog.field_edit_in_progress = None;
        self.dialog.spawn_drag_in_progress = None;
        self.map_edge = crate::panels::action_bar_modals::map_edge::MapEdgePanelState::default();
        self.dimensions =
            crate::panels::action_bar_modals::dimensions::DimensionsPanelState::default();
        self.dialog.confirm_dialog = None;
        self.dialog.pending_action = None;
        self.selection.pending_group_delete = None;

        // Selection / drag state — selections from the previous
        // graph would point at NodeIds that no longer exist.
        self.selection.node = None;
        self.selection.nodes.clear();
        self.selection.group = None;
        self.selection.connection = None;
        self.canvas.drag_connection = None;
        self.canvas.marquee_start = None;
        self.map.dragging_spawn = None;
        self.palette_drag = None;
        self.palette_filter.clear();
        self.feature_filter.clear();
        self.project.passthrough_edit = None;
        self.dialog.pending_props_open = None;
        self.props.close();

        // Transient status / toast — messages from the previous
        // project would mislead the user about what just happened.
        self.dialog.toast = None;
        self.dialog.status_message = None;
        self.dialog.status_level = crate::log::LogLevel::Info;

        // Preview / export state -- run pulses and export status all reset together.
        self.preview.reset();

        // Canvas viewport — pan offset, zoom, and the cached canvas
        // rect from the previous project's layout would land the new
        // graph in the wrong viewport / scale. apply_project re-installs
        // the saved offset AFTER this reset for loaded projects.
        self.canvas.offset = egui::Vec2::ZERO;
        self.canvas.zoom = 1.0;
        self.canvas.rect_last = egui::Rect::NOTHING;

        // Tabs — only the Main tab survives a project switch; any
        // SubGraph / Sculpt tabs from the previous project refer to
        // NodeIds that no longer exist.
        self.canvas.tabs = vec![CanvasView::Main];
        self.canvas.active_tab = 0;
        self.canvas.last_active_tab = 0;
    }

    pub(crate) fn do_new_project(&mut self) {
        self.reset_project();

        // Drop the FinalComposition terminal node near the right edge of
        // the canvas so the user can build their pipeline left-to-right.
        let fc_pos = self.starter_bundler_position();
        let fc_id = self.add_final_composition_node("Final Composition");
        self.visuals.node_visuals.insert(
            fc_id,
            NodeVisual {
                position: fc_pos,
                size: egui::vec2(210.0, 240.0),
            },
        );
    }

    /// Add a fresh FinalComposition node to the graph with all four
    /// paint-layer asset ids minted and their asset paths pointing at
    /// the editor's current temp asset dir. Used by every code path
    /// that creates an FC from scratch in the GUI (do_new_project,
    /// welcome_blank_project, start_with_macro, finish_open_map's
    /// post-scan fixup). The layer files themselves are NOT written
    /// here -- they come into existence on first paint.
    pub(crate) fn add_final_composition_node(&mut self, label: &str) -> NodeId {
        let mut node = Node::new(NodeId(0), NodeType::FinalComposition, label);
        bar_project::mint_fc_layer_ids(&mut node.params);
        bar_project::populate_fc_layer_paths(&mut node.params, &self.fc_layer_base_dir());
        self.graph.add_node(node)
    }

    /// Where FC layer asset files live for the currently-loaded
    /// project. For saved projects, `<project>/final_composition/`;
    /// for unsaved sessions, a per-editor temp dir under the OS temp
    /// root. Matches the location chosen by `pack_assets_for_save`
    /// for the saved case.
    pub(crate) fn fc_layer_base_dir(&self) -> std::path::PathBuf {
        match self.project.path.as_ref() {
            Some(p) => p.join("final_composition"),
            None => std::env::temp_dir()
                .join("bar-editor-assets")
                .join("final_composition"),
        }
    }

    /// Where to place the Bundler terminal node on a fresh project.
    /// Anchors to the right edge of the most-recent canvas rect so
    /// the user can build their pipeline left-to-right.
    pub(crate) fn starter_bundler_position(&self) -> egui::Pos2 {
        let bundler_size = egui::vec2(210.0, 240.0);
        let margin = 40.0_f32;
        let canvas_w = if self.canvas.rect_last.is_positive() {
            self.canvas.rect_last.width()
        } else {
            // Welcome -> Blank Project on first launch can fire
            // before any canvas frame has run; pick a width that
            // matches the typical default viewport.
            1100.0
        };
        let right_x = canvas_w - margin;
        let bundler_x = right_x - bundler_size.x;
        egui::pos2(bundler_x, 80.0)
    }

    /// Drop the FinalComposition terminal node onto an empty graph --
    /// the welcome panel's "Empty graph" entry point.
    pub(crate) fn welcome_blank_project(&mut self) {
        let fc_pos = self.starter_bundler_position();
        let fc_id = self.add_final_composition_node("Final Composition");
        self.visuals.node_visuals.insert(
            fc_id,
            NodeVisual {
                position: fc_pos,
                size: egui::vec2(210.0, 240.0),
            },
        );
        self.project.is_dirty = true;
    }

    /// Welcome panel's "Open project / SD7…" button. Same as the
    /// File menu's Open — spawn the OS dialog on a worker so the
    /// egui main loop keeps rendering.
    pub(crate) fn welcome_open_dialog(&mut self) {
        self.open_file_dialog_async();
    }

    /// Welcome panel's "Assemble Map…" button -- opens the assemble
    /// wizard. Wizard state is reset on open so a previously cancelled
    /// session doesn't bleed picks into the new one.
    pub(crate) fn start_assemble_map_dialog(&mut self) {
        self.assemble_map.reset();
        self.dialog.show_assemble_map = true;
    }

    /// Finish handler for the Assemble Map wizard. Builds a fresh
    /// project from `self.assemble_map.picks`: clears any existing
    /// graph, drops the standard FinalComposition + per-asset input
    /// nodes, copies every picked file into the per-project temp
    /// asset dir, and populates `MapSettings` (identity, dimensions,
    /// height range, resource basenames). The user lands in the
    /// editor with the project dirty and no path -- next save
    /// prompts Save As.
    pub(crate) fn finish_assemble_map(&mut self) {
        let picks = self.assemble_map.picks.clone();
        self.reset_project();

        let fc_pos = self.starter_bundler_position();
        let fc_id = self.add_final_composition_node("Final Composition");
        self.visuals.node_visuals.insert(
            fc_id,
            NodeVisual {
                position: fc_pos,
                size: egui::vec2(210.0, 240.0),
            },
        );

        // Apply dimensions + height range.
        if picks.squares_x > 0 && picks.squares_z > 0 {
            self.map.width = picks.squares_x * 64 + 1;
            self.map.height = picks.squares_z * 64 + 1;
        }
        self.map.min_height = picks.min_height;
        self.map.max_height = picks.max_height;

        // Identity metadata onto the recipe-meta block. Empty strings
        // collapse to `None` so the optional fields don't ship empty
        // values into the bundled mapinfo.
        let opt_string = |s: &str| {
            let t = s.trim();
            if t.is_empty() {
                None
            } else {
                Some(t.to_string())
            }
        };
        self.map.recipe_meta.name = opt_string(&picks.name);
        self.map.recipe_meta.author = opt_string(&picks.author);
        self.map.recipe_meta.description = picks.description.trim().to_string();
        self.map.recipe_meta.version = opt_string(&picks.version);

        // Stash any picked optional resource files: copy each into
        // the per-session temp dir, then write the basename into the
        // matching MapSettings field so the bundler can find them in
        // `passthrough/` after the project is saved.
        // Stage every picked resource file inside a per-session temp
        // dir's `passthrough/` subtree. On first save, the persistence
        // layer copies that subtree into `<project>/passthrough/` so
        // the basenames stored on the recipe resolve cleanly. Until
        // the save lands the recipe references files that only exist
        // in the temp dir -- the editor-side preview path falls back
        // to `pending_map_data_dir` to resolve them.
        let temp_dir = std::env::temp_dir().join("bar-editor-assemble-map");
        let passthrough_dir = temp_dir.join("passthrough");
        let _ = std::fs::create_dir_all(&passthrough_dir);
        let stash_into_passthrough = |src: &std::path::PathBuf| -> Option<String> {
            let basename = src.file_name()?.to_str()?.to_string();
            let dest = passthrough_dir.join(&basename);
            if std::fs::copy(src, &dest).is_err() {
                tracing::warn!(src = %src.display(), "assemble_map: copy failed");
                return None;
            }
            Some(basename)
        };
        let settings = &mut self.map.settings;
        if let Some(p) = picks.minimap_path.as_ref() {
            settings.minimap = stash_into_passthrough(p);
        }
        if let Some(p) = picks.skybox_path.as_ref() {
            settings.atmosphere.skybox = stash_into_passthrough(p);
        }
        if let Some(p) = picks.splat_distribution_path.as_ref() {
            if let Some(n) = stash_into_passthrough(p) {
                settings.resources.splat_distr_tex = n;
            }
        }
        if let Some(p) = picks.splat_detail_normal_1_path.as_ref() {
            if let Some(n) = stash_into_passthrough(p) {
                settings.resources.splat_detail_normal_tex_1 = n;
            }
        }
        if let Some(p) = picks.splat_detail_normal_2_path.as_ref() {
            if let Some(n) = stash_into_passthrough(p) {
                settings.resources.splat_detail_normal_tex_2 = n;
            }
        }
        if let Some(p) = picks.splat_detail_normal_3_path.as_ref() {
            if let Some(n) = stash_into_passthrough(p) {
                settings.resources.splat_detail_normal_tex_3 = n;
            }
        }
        if let Some(p) = picks.splat_detail_normal_4_path.as_ref() {
            if let Some(n) = stash_into_passthrough(p) {
                settings.resources.splat_detail_normal_tex_4 = n;
            }
        }
        if let Some(p) = picks.specular_path.as_ref() {
            if let Some(n) = stash_into_passthrough(p) {
                settings.resources.specular_tex = n;
            }
        }
        if let Some(p) = picks.sky_reflect_mod_path.as_ref() {
            if let Some(n) = stash_into_passthrough(p) {
                settings.resources.sky_reflect_mod_tex = n;
            }
        }
        if let Some(p) = picks.detail_normal_path.as_ref() {
            if let Some(n) = stash_into_passthrough(p) {
                settings.resources.detail_normal_tex = n;
            }
        }
        if let Some(p) = picks.light_emission_path.as_ref() {
            if let Some(n) = stash_into_passthrough(p) {
                settings.resources.light_emission_tex = n;
            }
        }
        if let Some(p) = picks.grass_distribution_path.as_ref() {
            if let Some(n) = stash_into_passthrough(p) {
                settings.custom_grass.dist_tga = Some(n);
            }
        }

        // Pin the temp passthrough dir so save / preview can locate
        // the staged files before the first save lands.
        self.project.pending_map_data_dir = Some(temp_dir.clone());

        // Build the four core graph input nodes (heightmap, diffuse
        // texture, metalmap, typemap) and wire them to the Final
        // Composition. These are the ONLY layers that flow through the
        // node graph -- mapinfo-driven resources (splats, masks,
        // skybox, minimap, grass dist) live exclusively on
        // `MapSettings`. Strict no-duplication: every required file
        // belongs to exactly one of {graph, MapSettings}, never both.
        let assets_dir = temp_dir.join("assets");
        self.build_core_input_nodes(fc_id, fc_pos, &picks, &assets_dir);

        self.assemble_map.reset();
        self.project.is_dirty = true;
    }

    /// Stage each picked core-layer file as an asset and add the
    /// matching graph node wired to the Final Composition. Heightmap
    /// is special-cased: when present, also drops a NormalMap node
    /// driven off the heightmap (matches the .sd7-import shape so
    /// the bundler's normal-map input is populated automatically).
    fn build_core_input_nodes(
        &mut self,
        fc_id: NodeId,
        fc_pos: egui::Pos2,
        picks: &crate::panels::assemble_map::state::AssembleMapPicks,
        assets_dir: &std::path::Path,
    ) {
        use crate::panels::assemble_map::build::{
            stage_grayscale_u8, stage_heightmap, stage_texture,
        };
        // Stack source nodes vertically to the left of FC.
        let source_x = fc_pos.x - 320.0;
        let mut next_y = fc_pos.y;
        let mut place = |this: &mut Self, node_id: NodeId, size: egui::Vec2| {
            this.visuals.node_visuals.insert(
                node_id,
                NodeVisual {
                    position: egui::pos2(source_x, next_y),
                    size,
                },
            );
            next_y += size.y + 24.0;
        };

        // Heightmap -- f32, plus a derived NormalMap node.
        if let Some(src) = picks.heightmap_path.as_ref() {
            match stage_heightmap(src, assets_dir) {
                Ok(staged) => {
                    let id = self.add_painted_heightmap_node("Heightmap", &staged);
                    place(self, id, egui::vec2(165.0, 80.0));
                    self.connect_to_fc(id, fc_id, "heightmap");

                    let nm = self.add_normalmap_node();
                    place(self, nm, egui::vec2(140.0, 60.0));
                    let _ = self.graph.connect(
                        bar_graph::PortId {
                            node_id: id,
                            port_name: "output".to_string(),
                        },
                        bar_graph::PortId {
                            node_id: nm,
                            port_name: "input".to_string(),
                        },
                    );
                    self.connect_to_fc(nm, fc_id, "normalmap");
                }
                Err(e) => tracing::warn!(error = %e, "assemble_map: stage_heightmap failed"),
            }
        }

        // Diffuse texture -- PaintedTexture, RGB.
        if let Some(src) = picks.diffuse_path.as_ref() {
            match stage_texture(src, assets_dir) {
                Ok(staged) => {
                    let id = self.add_painted_texture_node("Texture", &staged);
                    place(self, id, egui::vec2(165.0, 80.0));
                    self.connect_to_fc(id, fc_id, "texture");
                }
                Err(e) => tracing::warn!(error = %e, "assemble_map: stage_texture failed"),
            }
        }

        // Metalmap -- u8.
        if let Some(src) = picks.metalmap_path.as_ref() {
            match stage_grayscale_u8(src, assets_dir) {
                Ok(staged) => {
                    let id = self.add_painted_heightmap_node("Metal Map", &staged);
                    place(self, id, egui::vec2(165.0, 80.0));
                    self.connect_to_fc(id, fc_id, "metalmap");
                }
                Err(e) => {
                    tracing::warn!(error = %e, "assemble_map: stage_grayscale_u8 (metal) failed")
                }
            }
        }

        // Typemap -- u8.
        if let Some(src) = picks.typemap_path.as_ref() {
            match stage_grayscale_u8(src, assets_dir) {
                Ok(staged) => {
                    let id = self.add_painted_heightmap_node("Type Map", &staged);
                    place(self, id, egui::vec2(165.0, 80.0));
                    self.connect_to_fc(id, fc_id, "typemap");
                }
                Err(e) => {
                    tracing::warn!(error = %e, "assemble_map: stage_grayscale_u8 (type) failed")
                }
            }
        }
    }

    fn add_painted_heightmap_node(
        &mut self,
        label: &str,
        staged: &crate::panels::assemble_map::build::StagedAsset,
    ) -> NodeId {
        let mut node = bar_graph::Node::new(NodeId(0), NodeType::PaintedHeightmap, label);
        node.params.insert(
            "asset_id".to_string(),
            bar_graph::ParamValue::String(staged.asset_id.0.clone()),
        );
        node.params.insert(
            "width".to_string(),
            bar_graph::ParamValue::UInt(staged.width),
        );
        node.params.insert(
            "height".to_string(),
            bar_graph::ParamValue::UInt(staged.height),
        );
        node.params.insert(
            "asset_path".to_string(),
            bar_graph::ParamValue::String(staged.asset_path.to_string_lossy().into_owned()),
        );
        self.graph.add_node(node)
    }

    fn add_painted_texture_node(
        &mut self,
        label: &str,
        staged: &crate::panels::assemble_map::build::StagedAsset,
    ) -> NodeId {
        let mut node = bar_graph::Node::new(NodeId(0), NodeType::PaintedTexture, label);
        node.params.insert(
            "asset_id".to_string(),
            bar_graph::ParamValue::String(staged.asset_id.0.clone()),
        );
        node.params.insert(
            "width".to_string(),
            bar_graph::ParamValue::UInt(staged.width),
        );
        node.params.insert(
            "height".to_string(),
            bar_graph::ParamValue::UInt(staged.height),
        );
        node.params.insert(
            "asset_path".to_string(),
            bar_graph::ParamValue::String(staged.asset_path.to_string_lossy().into_owned()),
        );
        self.graph.add_node(node)
    }

    fn add_normalmap_node(&mut self) -> NodeId {
        let node = bar_graph::Node::new(NodeId(0), NodeType::NormalMap, "Normal Map");
        self.graph.add_node(node)
    }

    fn connect_to_fc(&mut self, from_node: NodeId, fc_id: NodeId, fc_port: &str) {
        let _ = self.graph.connect(
            bar_graph::PortId {
                node_id: from_node,
                port_name: "output".to_string(),
            },
            bar_graph::PortId {
                node_id: fc_id,
                port_name: fc_port.to_string(),
            },
        );
    }

    /// Welcome panel's "Recent" menu entry click — defers to the
    /// existing dirty-aware open path.
    pub(crate) fn start_open_path_for_panel(&mut self, path: std::path::PathBuf) {
        self.start_open_path(path);
    }

    /// Begin loading a built-in macro preset, routing through
    /// unsaved-changes confirmation when the current project is dirty.
    /// Used by File → New from Preset; the welcome panel calls
    /// `start_with_macro` directly because its precondition (empty
    /// graph, no project loaded) means there's nothing to discard.
    pub(crate) fn start_load_macro(&mut self, name: &str) {
        if self.project.is_dirty {
            self.dialog.pending_action = Some(PendingAction::LoadMacro {
                name: name.to_string(),
            });
        } else {
            self.start_with_macro(name);
        }
    }

    /// True when there's an open project — either loaded from disk
    /// (`project_path` set) or built up in-memory (graph has nodes).
    /// Used to gate the action toolbar, node palette, and validation
    /// panel: those surfaces only make sense once the user has
    /// committed to a project, otherwise the welcome screen is what
    /// they should be looking at.
    pub fn has_project(&self) -> bool {
        self.project.path.is_some() || !self.graph.nodes().is_empty()
    }

    /// Save to the existing project path, or fall back to Save As
    /// when none is set yet (untitled project).
    pub(crate) fn save_or_save_as(&mut self) {
        if let Some(p) = self.project.path.clone() {
            self.save_project(p);
        } else {
            self.save_as();
        }
    }

    pub(crate) fn save_as(&mut self) {
        self.save_as_with_suggested_name(None);
    }

    /// Same as `save_as` but pre-populates the dialog's filename
    /// field. Used by the .sd7 import flow to suggest a sensible
    /// project name derived from the source archive.
    pub(crate) fn save_as_with_suggested_name(&mut self, suggested: Option<&str>) {
        let mut dialog = self
            .make_dialog()
            .set_title("Save Project As")
            .add_filter("BAR Map Editor Project", &["barproj"]);
        if let Some(name) = suggested {
            dialog = dialog.set_file_name(name);
        }
        if let Some(path) = dialog.save_file() {
            self.save_project(path);
        }
    }
}

impl BarEditorApp {
    /// Begin an open operation. If the current project is dirty, this
    /// defers the open through an unsaved-changes confirmation;
    /// otherwise it opens immediately. Routes both .barproj and .sd7
    /// paths.
    pub(crate) fn start_open_path(&mut self, path: std::path::PathBuf) {
        if self.project.is_dirty {
            self.dialog.pending_action = Some(PendingAction::OpenPath(path));
        } else {
            self.dispatch_open(path);
        }
    }

    pub(crate) fn dispatch_open(&mut self, path: std::path::PathBuf) {
        // The OS folder picker can't filter directories by extension,
        // so the Open Project dialog returns whatever the user
        // selected. Validate here: SD7 archives go through the
        // importer, `.barproj` directories load as projects, anything
        // else is a user mistake we surface cleanly rather than
        // letting `Project::load` produce a "recipe.json not found"
        // error from deep inside the loader.
        match path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref()
        {
            Some("sd7") => self.open_map_as_project(path),
            Some("barproj") => self.load_project(path),
            _ => {
                self.log_error(t!(
                    "editor.project.not_barproj",
                    path = path.display().to_string()
                ));
            }
        }
    }

    /// Load a project from a file.
    pub(crate) fn load_project(&mut self, path: std::path::PathBuf) {
        use bar_project::Project;
        let project = match Project::load(&path) {
            Ok(p) => p,
            Err(e) => {
                self.log_error(t!("editor.project.load_failed", error = e.to_string()));
                // If a recent-files entry pointed at a now-broken file, drop it.
                self.settings.remove_recent(&path);
                self.settings.save();
                return;
            }
        };
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Untitled".to_string());
        let display = path.display().to_string();
        self.settings.add_recent(&path);
        self.settings.save();
        self.apply_project(
            project,
            Some(path),
            name,
            t!("editor.project.loaded", name = display),
        );
    }

    /// Apply a loaded/parsed project as the current session.
    fn apply_project(
        &mut self,
        project: bar_project::Project,
        path: Option<std::path::PathBuf>,
        name: String,
        status: String,
    ) {
        use std::collections::HashMap;

        use crate::app::parse_subgraph_binding;
        use crate::state::GroupRuntime;

        let graph = match project.recipe.build_graph() {
            Ok(g) => g,
            Err(e) => {
                self.log_error(format!("Invalid project: {e}"));
                return;
            }
        };

        // Wipe all project state before installing the new one.
        self.reset_project();

        // Install the new project's graph (overrides reset_project's
        // GraphEngine::new()).
        self.graph = graph;

        // Install per-project layout, overriding reset_project's
        // zero-offset / unit-zoom defaults. Zoom is clamped on load so a
        // hand-edited or corrupt `.barproj` can't install an out-of-range
        // (or zero) factor that would break the canvas transform.
        self.canvas.offset = egui::vec2(
            project.layout.canvas_offset.0,
            project.layout.canvas_offset.1,
        );
        self.canvas.zoom = project.layout.canvas_zoom.clamp(
            crate::editor::CanvasState::MIN_ZOOM,
            crate::editor::CanvasState::MAX_ZOOM,
        );
        self.map.width = project.recipe.output.width;
        self.map.height = project.recipe.output.height;
        self.map.settings = project.recipe.output.map_settings.clone();
        self.map.recipe_meta = RecipeMeta {
            // Preserve the source's human-readable `mapinfo.name`
            // through the live editor session. Without this the
            // bundler falls back to the .barproj directory slug and
            // emits e.g. `name = "onyx_cauldron"` instead of the
            // author's `"Onyx Cauldron"`.
            name: Some(project.recipe.name.clone()).filter(|s| !s.is_empty()),
            shortname: project.recipe.shortname.clone(),
            description: project.recipe.description.clone(),
            author: project.recipe.author.clone(),
            version: project.recipe.version.clone(),
            tip: project.recipe.tip.clone(),
            depend: project.recipe.depend.clone(),
        };
        // Shadow fields take the resolved value -- engine default
        // when unset -- so the UI always has something concrete to
        // bind to. Editing the shadow flips the underlying setting
        // to `Some(value)` via `app::project_export` (and friends).
        let rs = self.map.settings.resolved();
        self.map.min_height = rs.min_height;
        self.map.max_height = rs.max_height;
        self.map.features = project.recipe.features.clone();
        if !self.map.features.is_empty() {
            self.project.features_changed = true;
        }

        // Resolve any project-relative file paths (`bar://...`) against the
        // .barproj directory so executors get absolute paths they can read.
        // `path` IS the project directory (not a file inside it), so use it
        // directly rather than calling .parent().
        if let Some(project_dir) = path.as_ref() {
            self.resolve_relative_paths(project_dir);
        }

        // FC layers carry per-kind asset ids from project creation
        // forward. Mint any that are still empty (any FC built before
        // pre-allocation was added). For loaded projects,
        // `resolve_relative_paths` above already stamped the correct
        // `<project>/final_composition/<id>.bin` asset_path; only
        // populate paths here for the no-path (fresh-from-scan, untitled
        // projects) case, where the brush flow needs SOMEWHERE to
        // write strokes pre-Save. We can't call `fc_layer_base_dir()`
        // because `self.project.path` is set further down in this
        // method, but at this point the `path` parameter tells us
        // everything we need.
        let fc_base: Option<std::path::PathBuf> = if path.is_some() {
            // Loaded saved project: `resolve_relative_paths` set the
            // correct asset_path; nothing more to do for path injection.
            None
        } else {
            Some(
                std::env::temp_dir()
                    .join("bar-editor-assets")
                    .join("final_composition"),
            )
        };
        for (_, node) in self.graph.nodes_mut() {
            if node.node_type == NodeType::FinalComposition {
                bar_project::mint_fc_layer_ids(&mut node.params);
                if let Some(base) = fc_base.as_deref() {
                    bar_project::populate_fc_layer_paths(&mut node.params, base);
                }
            }
        }

        // Restore node positions and sizes. Build a key->id map for
        // the groups restoration that follows.
        let mut key_to_id: HashMap<String, NodeId> = HashMap::new();
        for (idx, recipe_node) in project.recipe.nodes.iter().enumerate() {
            let node_id = NodeId((idx + 1) as u64);
            key_to_id.insert(recipe_node.key.clone(), node_id);
            let pos = project
                .layout
                .node_positions
                .get(&recipe_node.key)
                .map(|p| egui::pos2(p.x, p.y))
                .unwrap_or_else(|| egui::pos2(200.0 + (idx as f32 * 180.0), 200.0));
            let size = project
                .layout
                .node_sizes
                .get(&recipe_node.key)
                .map(|s| egui::vec2(s.width, s.height))
                .unwrap_or_else(|| egui::vec2(150.0, 80.0));
            self.visuals.node_visuals.insert(
                node_id,
                NodeVisual {
                    position: pos,
                    size,
                },
            );
        }

        // Restore groups: convert recipe-key references back to
        // NodeIds and rebuild the reverse index. Drop members whose
        // keys no longer resolve (rare; happens if a save was
        // hand-edited). (reset_project already cleared
        // groups/node_to_group above.)
        let mut max_group_id: u64 = 0;
        for g in &project.layout.groups {
            let member_ids: std::collections::HashSet<NodeId> = g
                .member_keys
                .iter()
                .filter_map(|k| key_to_id.get(k).copied())
                .collect();
            for &nid in &member_ids {
                self.visuals.node_to_group.insert(nid, g.id);
            }
            self.visuals.groups.insert(
                g.id,
                GroupRuntime {
                    label: g.label.clone(),
                    member_ids,
                    color_idx: g.color_idx,
                    collapsed: g.collapsed,
                    is_subgraph: g.is_subgraph,
                    subgraph_inputs: g
                        .subgraph_inputs
                        .iter()
                        .map(|p| crate::state::SubgraphPortRuntime {
                            name: p.name.clone(),
                            label: p.label.clone(),
                            kind: p.kind.clone(),
                            binding: parse_subgraph_binding(p.binding.as_deref(), &key_to_id),
                        })
                        .collect(),
                    subgraph_outputs: g
                        .subgraph_outputs
                        .iter()
                        .map(|p| crate::state::SubgraphPortRuntime {
                            name: p.name.clone(),
                            label: p.label.clone(),
                            kind: p.kind.clone(),
                            binding: parse_subgraph_binding(p.binding.as_deref(), &key_to_id),
                        })
                        .collect(),
                    macro_params: g
                        .macro_params
                        .iter()
                        .map(|p| crate::state::MacroParamRuntime {
                            name: p.name.clone(),
                            label: p.label.clone(),
                            kind: p.kind.clone(),
                            binding: parse_subgraph_binding(Some(&p.binding), &key_to_id),
                            min: p.min,
                            max: p.max,
                        })
                        .collect(),
                },
            );
            max_group_id = max_group_id.max(g.id);
        }
        self.visuals.next_group_id = max_group_id + 1;

        // Restore open tabs. Validate each persisted entry against
        // current state -- drop tabs whose target no longer exists
        // (rare; happens after hand-edits to the project file). Main
        // always lives at index 0; persisted "Main" entries are
        // collapsed away to avoid duplicates.
        let mut restored_tabs: Vec<CanvasView> = vec![CanvasView::Main];
        for view in &project.layout.open_tabs {
            match view {
                bar_project::PersistedCanvasView::Main => {}
                bar_project::PersistedCanvasView::SubGraph { group_id } => {
                    if self.visuals.groups.contains_key(group_id) {
                        let v = CanvasView::SubGraph(*group_id);
                        if !restored_tabs.contains(&v) {
                            restored_tabs.push(v);
                        }
                    }
                }
            }
        }
        self.canvas.tabs = restored_tabs;
        self.canvas.active_tab =
            (project.layout.active_tab as usize).min(self.canvas.tabs.len().saturating_sub(1));

        self.project.path = path;
        self.project.loaded_name = Some(name);
        self.log_info(status);
        self.project.is_dirty = false;
        self.project.graph_reset = true;
    }

    /// Open a .sd7 map archive as a new project.
    ///
    /// Resets graph state immediately and queues the SD7 path for
    /// extraction. The actual extraction runs in a background thread
    /// managed by `bar-app`, which calls `finish_open_map` when
    /// complete.
    fn open_map_as_project(&mut self, path: std::path::PathBuf) {
        self.reset_project();

        let map_name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.display().to_string());
        self.log_info(t!("editor.project.extracting", name = map_name));

        // Intentionally NOT added to recent_files: a .sd7 is an import
        // source, not a BME project. Adding it here causes startup restore
        // to try (and fail) to re-open it as a .barproj. The .barproj path
        // enters recents when the user explicitly saves.
        self.project.sd7_open_request = Some(path);
    }

    /// Take the pending SD7 open request (if any). Called by `bar-app`
    /// each frame; when Some, bar-app spawns the extraction thread.
    pub fn take_sd7_open_request(&mut self) -> Option<std::path::PathBuf> {
        self.project.sd7_open_request.take()
    }

    /// Build the node graph after a successful .sd7 extraction.
    pub fn finish_open_map(&mut self, scan: bar_project::WorkDirScan) {
        let name = scan.map_name.clone();
        let status = t!("editor.project.opened", name = name);
        let (project, pending_assets, raw_files) = bar_project::scan_to_project(&scan);
        self.apply_project(project, None, name.clone(), status);

        // Write pending binary assets to a temp dir + inject their
        // `asset_path` params on the matching graph nodes. This MUST
        // happen before the Save-As prompt below, because the save
        // flow's `pack_assets_for_save` migrates asset files from
        // wherever `asset_path` points to into `<proj>/assets/`. With
        // no `asset_path` injected, pack-for-save sees no asset to
        // copy and the saved `.barproj` has no heightmap binary, so
        // the reloaded project comes up with flat terrain.
        let temp_dir = std::env::temp_dir().join("bar-editor-assets");
        if !pending_assets.is_empty() {
            for asset in &pending_assets {
                let path = temp_dir.join(format!("{}.bin", asset.id.0));
                if let Err(e) = bar_project::write_asset_file(&path, asset.header, &asset.data) {
                    tracing::warn!(error = %e, "Failed to write temp asset");
                    continue;
                }
                let path_str = path.to_string_lossy().into_owned();
                for (_, node) in self.graph.nodes_mut() {
                    if let Some(bar_graph::ParamValue::String(aid)) = node.params.get("asset_id") {
                        if *aid == asset.id.0 {
                            node.params.insert(
                                "asset_path".to_string(),
                                bar_graph::ParamValue::String(path_str.clone()),
                            );
                        }
                    }
                }
            }
        }
        // Write raw (non-BARASSET) files and inject their paths.
        if !raw_files.is_empty() {
            if let Err(e) = std::fs::create_dir_all(&temp_dir) {
                tracing::warn!(error = %e, "Failed to create temp asset dir");
            } else {
                for raw in &raw_files {
                    let dest = temp_dir.join(format!("{}.{}", raw.id.0, raw.extension));
                    let ok = if let Some(src) = &raw.source_path {
                        std::fs::copy(src, &dest).is_ok()
                    } else {
                        std::fs::write(&dest, &raw.data).is_ok()
                    };
                    if !ok {
                        tracing::warn!(
                            dest = %dest.display(),
                            "Failed to write raw temp asset"
                        );
                        continue;
                    }
                    let path_str = dest.to_string_lossy().into_owned();
                    for (_, node) in self.graph.nodes_mut() {
                        if let Some(bar_graph::ParamValue::String(aid)) =
                            node.params.get(&raw.match_param)
                        {
                            if *aid == raw.id.0 {
                                node.params.insert(
                                    raw.inject_param.clone(),
                                    bar_graph::ParamValue::String(path_str.clone()),
                                );
                            }
                        }
                    }
                }
            }
        }
        // FC asset_path injection is handled centrally by
        // `apply_project` above (called via this method) -- it sees
        // every freshly-installed graph and stamps temp-dir paths for
        // any FC node that lacks them, so the brush can write strokes
        // immediately without a separate mint/ensure step.

        // Now that `asset_path` is injected everywhere, prompt the
        // user to commit the import to a `.barproj` on disk. Pre-
        // populates with the SD7 stem minus any trailing `_<version>`
        // suffix so "onyx_cauldron_2.2.2.sd7" suggests
        // "onyx_cauldron.barproj". The user can edit before
        // confirming or cancel to keep the project in-memory only
        // (legacy behaviour).
        let suggested = format!("{}.barproj", strip_trailing_version(&name));
        self.save_as_with_suggested_name(Some(&suggested));

        // Only mark dirty if the auto-Save-As prompt above didn't
        // commit the import to disk (user cancelled the dialog).
        // `save_project` clears the dirty flag on success and sets
        // `project.path`, so absence of `project.path` after the
        // Save-As call means "unsaved import" and we mark dirty so
        // the title bar's `*` indicator reflects that.
        if self.project.path.is_none() {
            self.project.is_dirty = true;
        }
    }

    /// Pick a default label for a new entity of the given base type
    /// (e.g. "Perlin Noise" or "Mountain Range"). Scans existing node
    /// and group labels for `"<base> <n>"` and returns the next free
    /// number. So dropping three Perlin Noise nodes gives "Perlin
    /// Noise 1", "Perlin Noise 2", "Perlin Noise 3" -- way easier
    /// to track than three identical labels.
    pub(crate) fn next_label_for(&self, base: &str) -> String {
        let prefix = format!("{} ", base);
        let mut max_n: u32 = 0;
        for node in self.graph.nodes().values() {
            if let Some(rest) = node.label.strip_prefix(&prefix) {
                if let Ok(n) = rest.parse::<u32>() {
                    if n > max_n {
                        max_n = n;
                    }
                }
            }
        }
        for group in self.visuals.groups.values() {
            if let Some(rest) = group.label.strip_prefix(&prefix) {
                if let Ok(n) = rest.parse::<u32>() {
                    if n > max_n {
                        max_n = n;
                    }
                }
            }
        }
        format!("{} {}", base, max_n + 1)
    }

    /// Drop a macro into a fresh project: clears any existing graph
    /// and session state, adds a Bundler and Preview to the right,
    /// and wires the macro's outputs through them. Used by both the
    /// welcome panel's preset cards and File → New from Preset; both
    /// surfaces produce identical starting state because they share
    /// this method.
    pub(crate) fn start_with_macro(&mut self, macro_name: &str) {
        use bar_graph::PortId;

        self.reset_project();

        // Right-edge placement for the terminal node; drop the
        // macro to the left of it so wires read left-to-right.
        let bundler_pos = self.starter_bundler_position();
        let macro_pos = egui::pos2((bundler_pos.x - 320.0).max(40.0), bundler_pos.y + 60.0);
        self.instantiate_macro(macro_name, macro_pos);

        // The macro's IO nodes were just added to a fresh group;
        // its `subgraph_outputs` runtime list is still empty (the
        // per-frame `recompute_all_subgraph_io` runs at the START
        // of `update`, before `start_with_macro` is reached).
        // Refresh now so the wiring loop below sees the actual
        // ports.
        self.recompute_all_subgraph_io();

        // Find the group we just created — it'll be the highest gid.
        let new_gid = match self.visuals.groups.keys().copied().max() {
            Some(g) => g,
            None => return,
        };

        // FinalComposition -- the terminal node every project ships
        // with. Always present so the project is shippable out of
        // the box.
        let bundler_id = self.add_final_composition_node("Final Composition");
        self.visuals.node_visuals.insert(
            bundler_id,
            NodeVisual {
                position: bundler_pos,
                size: egui::vec2(210.0, 240.0),
            },
        );

        // Wire each subgraph output to the Bundler. Macro IO
        // nodes are unnamed by default, so we route by *kind*:
        // the first Heightmap port goes to the bundler heightmap
        // input, the first Color port goes to texture, etc.
        // Subsequent ports of the same kind are skipped --
        // there's only one Bundler.heightmap to fill.
        // Collect subgraph outputs with their name so we can distinguish
        // "terrain" from "slope" (both are Heightmap kind).
        let outputs: Vec<(String, String, NodeId, String)> = self
            .visuals
            .groups
            .get(&new_gid)
            .map(|g| {
                g.subgraph_outputs
                    .iter()
                    .filter_map(|p| {
                        let (id, port) = p.binding.clone()?;
                        Some((p.name.clone(), p.kind.clone(), id, port))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut routed: std::collections::HashSet<&'static str> = std::collections::HashSet::new();
        let mut heightmap_src: Option<(NodeId, String)> = None;
        let mut slope_src: Option<(NodeId, String)> = None;
        for (name, kind, src_id, src_port) in outputs {
            let port_name: Option<&'static str> = match kind.as_str() {
                "Heightmap" => Some("heightmap"),
                "Color" => Some("texture"),
                _ => None,
            };
            let Some(port_name) = port_name else { continue };

            // Slope is a second Heightmap output that doesn't map to any
            // Bundler input. Capture it for use by SpecularMap/GrassMap.
            if port_name == "heightmap" && name == "slope" {
                slope_src = Some((src_id, src_port));
                continue;
            }

            if !routed.insert(port_name) {
                continue;
            }
            if port_name == "heightmap" {
                heightmap_src = Some((src_id, src_port.clone()));
            }
            let _ = self.graph.connect(
                PortId {
                    node_id: src_id,
                    port_name: src_port.clone(),
                },
                PortId {
                    node_id: bundler_id,
                    port_name: port_name.to_string(),
                },
            );
        }

        // Auto-fill the rest of the Bundler's inputs so the preset
        // exports a complete bundle out of the box. NormalMap, SpecularMap,
        // and GrassMap derive from the macro's terrain/slope outputs.
        // Metal and type get Constant(0) -- those are project-specific data
        // the user replaces manually. The user is free to swap any node out.
        let aux_x = bundler_pos.x - 220.0;
        let mut aux_y = bundler_pos.y;
        let aux_step = 70.0_f32;
        let aux_size = egui::vec2(150.0, 80.0);

        if let Some((hm_id, hm_port)) = heightmap_src {
            // NormalMap → Bundler.normalmap
            let nm = Node::new(NodeId(0), NodeType::NormalMap, "Normal Map");
            let nm_id = self.graph.add_node(nm);
            self.visuals.node_visuals.insert(
                nm_id,
                NodeVisual {
                    position: egui::pos2(aux_x, aux_y),
                    size: aux_size,
                },
            );
            let _ = self.graph.connect(
                PortId {
                    node_id: hm_id,
                    port_name: hm_port.clone(),
                },
                PortId {
                    node_id: nm_id,
                    port_name: "input".into(),
                },
            );
            let _ = self.graph.connect(
                PortId {
                    node_id: nm_id,
                    port_name: "output".into(),
                },
                PortId {
                    node_id: bundler_id,
                    port_name: "normalmap".into(),
                },
            );
            aux_y += aux_step;

            // SpecularMap → Bundler.specular (+ optional slope input)
            let sm = Node::new(NodeId(0), NodeType::SpecularMap, "Specular Map");
            let sm_id = self.graph.add_node(sm);
            self.visuals.node_visuals.insert(
                sm_id,
                NodeVisual {
                    position: egui::pos2(aux_x, aux_y),
                    size: aux_size,
                },
            );
            let _ = self.graph.connect(
                PortId {
                    node_id: hm_id,
                    port_name: hm_port.clone(),
                },
                PortId {
                    node_id: sm_id,
                    port_name: "input".into(),
                },
            );
            if let Some((s_id, ref s_port)) = slope_src {
                let _ = self.graph.connect(
                    PortId {
                        node_id: s_id,
                        port_name: s_port.clone(),
                    },
                    PortId {
                        node_id: sm_id,
                        port_name: "slope".into(),
                    },
                );
            }
            let _ = self.graph.connect(
                PortId {
                    node_id: sm_id,
                    port_name: "output".into(),
                },
                PortId {
                    node_id: bundler_id,
                    port_name: "specular".into(),
                },
            );
            aux_y += aux_step;

            // GrassMap → Bundler.grassmap (+ optional slope input)
            let gm = Node::new(NodeId(0), NodeType::GrassMap, "Grass Map");
            let gm_id = self.graph.add_node(gm);
            self.visuals.node_visuals.insert(
                gm_id,
                NodeVisual {
                    position: egui::pos2(aux_x, aux_y),
                    size: aux_size,
                },
            );
            let _ = self.graph.connect(
                PortId {
                    node_id: hm_id,
                    port_name: hm_port,
                },
                PortId {
                    node_id: gm_id,
                    port_name: "input".into(),
                },
            );
            if let Some((s_id, ref s_port)) = slope_src {
                let _ = self.graph.connect(
                    PortId {
                        node_id: s_id,
                        port_name: s_port.clone(),
                    },
                    PortId {
                        node_id: gm_id,
                        port_name: "slope".into(),
                    },
                );
            }
            let _ = self.graph.connect(
                PortId {
                    node_id: gm_id,
                    port_name: "output".into(),
                },
                PortId {
                    node_id: bundler_id,
                    port_name: "grassmap".into(),
                },
            );
            aux_y += aux_step;
        }

        // PaintedHeightmap for metal and type -- starts blank (all
        // zeros) but the map-maker can open either canvas and paint
        // ore spots or terrain-type zones directly.
        for (port, label) in [("metalmap", "Metal Map"), ("typemap", "Type Map")] {
            let node = Node::new(NodeId(0), NodeType::PaintedHeightmap, label);
            let nid = self.graph.add_node(node);
            self.visuals.node_visuals.insert(
                nid,
                NodeVisual {
                    position: egui::pos2(aux_x, aux_y),
                    size: aux_size,
                },
            );
            let _ = self.graph.connect(
                PortId {
                    node_id: nid,
                    port_name: "output".into(),
                },
                PortId {
                    node_id: bundler_id,
                    port_name: port.into(),
                },
            );
            aux_y += aux_step;
        }

        // Clear selection so the deferred auto-layout below treats
        // the call as "lay out everything top-level" rather than
        // "lay out only this group" -- the layout helper inspects
        // selection state. Auto-layout itself is deferred to the
        // next frame because we may still be rendering the welcome
        // panel or the File menu; `canvas_rect_last` won't be valid
        // until `draw_node_graph` runs at least once.
        self.selection.nodes.clear();
        self.selection.node = None;
        self.selection.group = None;
        self.props.active = None;
        self.canvas.pending_auto_layout_all = true;

        self.project.is_dirty = true;
        self.dialog.status_message = Some(format!(
            "Started a new project with the '{}' template.",
            macro_name
        ));
    }
}

/// Strip a trailing `_<version>` or `_v<version>` suffix from a map
/// slug so the .sd7 import flow can suggest a version-free .barproj
/// name. Versions are detected as the last `_`-prefixed run containing
/// only digits, dots, and an optional leading `v`. Examples:
///   "onyx_cauldron_2.2.2"     -> "onyx_cauldron"
///   "delta_siege_dry_v5.7.1"  -> "delta_siege_dry"
///   "tundra_v2"               -> "tundra"
///   "kolmog"                  -> "kolmog"            (unchanged)
///   "twin_lakes_park_redux_1.2.2" -> "twin_lakes_park_redux"
fn strip_trailing_version(slug: &str) -> &str {
    let Some(idx) = slug.rfind('_') else {
        return slug;
    };
    let tail = &slug[idx + 1..];
    if tail.is_empty() {
        return slug;
    }
    let after_v = tail.strip_prefix('v').unwrap_or(tail);
    let is_version = !after_v.is_empty() && after_v.chars().all(|c| c.is_ascii_digit() || c == '.');
    if is_version {
        &slug[..idx]
    } else {
        slug
    }
}

#[cfg(test)]
mod strip_trailing_version_tests {
    use super::strip_trailing_version;

    #[test]
    fn strips_dotted_version() {
        assert_eq!(
            strip_trailing_version("onyx_cauldron_2.2.2"),
            "onyx_cauldron"
        );
    }

    #[test]
    fn strips_v_prefixed_version() {
        assert_eq!(
            strip_trailing_version("delta_siege_dry_v5.7.1"),
            "delta_siege_dry"
        );
    }

    #[test]
    fn strips_simple_v_version() {
        assert_eq!(strip_trailing_version("tundra_v2"), "tundra");
    }

    #[test]
    fn leaves_slug_with_no_version_alone() {
        assert_eq!(strip_trailing_version("kolmog"), "kolmog");
    }

    #[test]
    fn leaves_slug_with_non_version_tail_alone() {
        assert_eq!(strip_trailing_version("foo_bar_baz"), "foo_bar_baz");
    }
}

#[cfg(test)]
mod session_reset_tests {
    use std::time::Instant;

    use eframe::egui;

    use crate::app::{BarEditorApp, BrushTool, ValidationFilter};

    /// Stuff a default app with as many transient session-state fields
    /// as the helper is meant to clear. Used by every test below so
    /// each behaviour is asserted against a richly populated baseline,
    /// not a fresh default.
    fn dirtied_app() -> BarEditorApp {
        let mut app = BarEditorApp::default();
        app.push_undo("seed snapshot");
        app.paint.brush.tool = BrushTool::Lower;
        app.paint.brush.color_rgb = [10, 20, 30];
        app.paint.brush.paint_value = 0.42;
        app.paint.brush_stroking = true;
        app.canvas.offset = egui::vec2(123.0, 456.0);
        app.dialog.show_validation_panel = true;
        app.validation.findings = vec![];
        app.validation.filter = ValidationFilter::Error;
        app.dialog.show_inspector = true;
        app.dialog.show_identity_editor = true;
        app.dialog.show_atmosphere_editor = true;
        app.dialog.show_map_edge_editor = true;
        app.dialog.toast = Some(("hi".into(), Instant::now()));
        app.dialog.status_message = Some("from previous project".into());
        app.preview.run_requested = true;
        app.preview.test_in_bar_requested = true;
        app
    }

    #[test]
    fn reset_project_clears_all_fields() {
        let mut app = dirtied_app();
        app.reset_project();
        assert!(!app.history.can_undo(), "history must be cleared");
        assert!(
            matches!(app.paint.brush.tool, BrushTool::Pointer),
            "brush tool defaults to Pointer (no-op until the user enters a brush layer)"
        );
        assert!(!app.paint.brush_stroking);
        assert_eq!(
            app.canvas.offset,
            egui::Vec2::ZERO,
            "canvas pan offset must reset to zero"
        );
        assert!(!app.dialog.show_validation_panel);
        assert!(matches!(app.validation.filter, ValidationFilter::All));
        assert!(!app.dialog.show_inspector);
        assert!(!app.dialog.show_identity_editor);
        assert!(!app.dialog.show_atmosphere_editor);
        assert!(!app.dialog.show_map_edge_editor);
        assert!(app.dialog.toast.is_none());
        assert!(app.dialog.status_message.is_none());
        assert!(!app.preview.run_requested);
        assert!(!app.preview.test_in_bar_requested);
        assert!(app.paint.color_buffer.is_none());
        assert!(app.paint.metalmap.is_none());
        assert!(app.paint.typemap.is_none());
    }

    #[test]
    fn start_with_macro_resets_transient_state() {
        let mut app = dirtied_app();
        let prior_depth = app.history.undo_depth();
        app.start_with_macro("Plains");
        // History from the previous project is gone. The macro drop
        // pushes exactly one new undo entry (so the user can undo
        // their first action), so depth is 1, not the pre-reset value.
        assert!(
            app.history.undo_depth() < prior_depth.saturating_add(1) + 1,
            "history must not accumulate the previous project's snapshots"
        );
        assert_eq!(
            app.history.undo_depth(),
            1,
            "after start_with_macro, history holds only the macro-drop snapshot"
        );
        assert!(matches!(app.paint.brush.tool, BrushTool::Pointer));
        assert_eq!(app.canvas.offset, egui::Vec2::ZERO);
        assert!(!app.dialog.show_validation_panel);
        assert!(
            !app.graph.nodes().is_empty(),
            "macro should have dropped nodes onto the graph"
        );
        assert!(
            app.project.is_dirty,
            "starting from a macro is a non-empty diff against the empty default"
        );
    }

    #[test]
    fn do_new_project_resets_transient_state() {
        let mut app = dirtied_app();
        app.do_new_project();
        assert!(!app.history.can_undo());
        assert!(matches!(app.paint.brush.tool, BrushTool::Pointer));
        assert_eq!(app.canvas.offset, egui::Vec2::ZERO);
        // do_new_project drops a single Bundler terminal node.
        assert_eq!(app.graph.nodes().len(), 1);
    }

    #[test]
    fn unknown_macro_name_is_a_noop_with_status() {
        let mut app = BarEditorApp::default();
        app.start_with_macro("Definitely Not A Real Macro");
        // The name lookup happens after the reset+graph-clear, so the
        // graph ends up empty and the user sees a status message.
        // (This documents current behaviour -- the menu only feeds in
        // names from BUILTIN_MACRO_GROUPS, so this branch is defensive.)
        assert!(app.dialog.status_message.is_some());
    }
}
