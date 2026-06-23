//! Project save / load persistence: building a `bar_project::Project`
//! from the live editor state, saving to disk, and the inverse of
//! re-installing absolute paths after a load. Distributed
//! `impl BarEditorApp` block.
//!
//! The save flow is two-step: first `pack_assets_for_save` copies
//! external assets into `<stem>.assets/` and rewrites params to
//! `bar://...` URLs, then `build_project` snapshots the live state
//! into a `Project` and `save_project` serialises and writes.

use std::collections::HashMap;

use bar_graph::{NodeId, NodeType};

use crate::app::{BarEditorApp, CanvasView};
use crate::project::path::{
    pack_painted_asset, pack_passthrough_files, pack_path_param, pack_raw_asset,
    resolve_passthrough_files, resolve_path_param,
};
use crate::t;

impl BarEditorApp {
    /// User-initiated save. Packs any external assets and passthrough files
    /// into the project directory, then writes `recipe.json` + `layout.json`.
    /// The project directory IS the `.barproj` path (directory, not a file).
    pub(crate) fn save_project(&mut self, path: std::path::PathBuf) {
        if let Err(e) = self.pack_assets_for_save(&path) {
            self.log_error(format!("Asset packing failed: {e}"));
            return;
        }
        // Copy map-bundled feature data into the project directory on first save
        // so the project is self-contained: no back-reference to the work dir.
        // - objects3d/ : S3O model files referenced by feature defs
        // - features/  : map-local feature def lua files
        // - unittextures/ : S3O diffuse / normal textures (.tga/.dds/.png)
        if let Some(src_dir) = self.project.pending_map_data_dir.take() {
            for subdir in &["objects3d", "features", "unittextures"] {
                let src = src_dir.join(subdir);
                if src.is_dir() {
                    if let Err(e) = copy_dir_flat(&src, &path.join(subdir)) {
                        self.log_error(format!("Failed to copy {subdir} into project: {e}"));
                    }
                }
            }
            // Copy the SMF-embedded minimap sidecar (written during sd7
            // extract) into the .barproj so the map-edge-extension
            // fallback keeps working after reload, when the work dir is
            // no longer reachable.
            let minimap_src = src_dir.join(bar_project::SMF_MINIMAP_SIDE_CAR);
            if minimap_src.is_file() {
                let minimap_dest = path.join(bar_project::SMF_MINIMAP_SIDE_CAR);
                if let Err(e) = std::fs::copy(&minimap_src, &minimap_dest) {
                    self.log_error(format!("Failed to copy SMF minimap sidecar: {e}"));
                }
            }
            // Copy the entire `passthrough/` subtree from the staging
            // dir into the project's `passthrough/`. Used by the
            // Assemble Map wizard (which stages picked resource
            // files there before any project dir exists) and by any
            // future flow that needs to defer the project-dir copy
            // until first save. Existing destination files are
            // overwritten -- the staging dir is authoritative.
            let staged_pt = src_dir.join("passthrough");
            if staged_pt.is_dir() {
                let dest_pt = path.join("passthrough");
                if let Some(parent) = dest_pt.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if let Err(e) = copy_dir_flat(&staged_pt, &dest_pt) {
                    self.log_error(format!("Failed to copy staged passthrough: {e}"));
                }
            }

            // Copy the SMF grass distribution map (`grassmap.png`,
            // written by `import_sd7_to_project` when the SMF carries
            // a `MEH_Vegetation` extra header) into the .barproj so
            // the grass widget can sample it on reload. Mirrors the
            // BAR widget's `Spring.GetGrass` fallback path.
            let grassmap_src = src_dir.join("grassmap.png");
            if grassmap_src.is_file() {
                let grassmap_dest = path.join("passthrough").join("grassmap.png");
                if let Some(parent) = grassmap_dest.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if let Err(e) = std::fs::copy(&grassmap_src, &grassmap_dest) {
                    self.log_error(format!("Failed to copy grass map: {e}"));
                }
            }
        }
        let project = self.build_project(&path);
        match project.save(&path) {
            Ok(()) => {
                self.project.path = Some(path.clone());
                self.project.is_dirty = false;
                self.log_info(t!(
                    "editor.project.saved",
                    path = path.display().to_string()
                ));
                self.settings.add_recent(&path);
                self.settings.save();
                self.project.last_autosave_at = Some(std::time::Instant::now());
                // `pack_assets_for_save` rewrote in-memory params to
                // `bar://...` URLs so the on-disk recipe.json is
                // portable. The live editor session needs absolute
                // paths back so subsequent evaluations (Test-in-BAR,
                // Compile, Bundle) can read the files. Without this
                // step the bundler sees `bar://passthrough/foo` and
                // tries to open it as a literal filesystem path,
                // dropping every PassThrough file from the bundle.
                self.resolve_relative_paths(&path);
            }
            Err(e) => {
                self.log_error(t!("editor.project.save_failed", error = e.to_string()));
            }
        }
    }

    /// Walk every node holding a file-path param and replace any
    /// `bar://...` entries with absolute paths anchored at
    /// `project_dir`. Also injects `asset_path` for painted nodes that
    /// have an `asset_id` set. Called after a project loads, before any
    /// evaluation, so executors always see absolute paths they can read.
    pub(crate) fn resolve_relative_paths(&mut self, project_dir: &std::path::Path) {
        let assets_dir = project_dir.join("assets");
        for (_, node) in self.graph.nodes_mut() {
            match node.node_type {
                NodeType::FileReference => {
                    resolve_path_param(&mut node.params, "path", project_dir);
                }
                NodeType::PassThrough => {
                    resolve_passthrough_files(&mut node.params, project_dir);
                }
                NodeType::PaintedHeightmap | NodeType::PaintedTexture => {
                    if let Some(bar_graph::ParamValue::String(id)) =
                        node.params.get("asset_id").cloned()
                    {
                        if !id.is_empty() {
                            let abs = assets_dir
                                .join(format!("{id}.bin"))
                                .to_string_lossy()
                                .into_owned();
                            node.params.insert(
                                "asset_path".to_string(),
                                bar_graph::ParamValue::String(abs),
                            );
                        }
                    }
                }
                NodeType::FinalComposition => {
                    // Paint layers live under `<project>/final_composition/`,
                    // NOT `assets/`, so the on-disk story matches the
                    // logical ownership (paint state owned by Sculpt3D
                    // via FC, separate from procedural graph assets).
                    let fc_dir = project_dir.join("final_composition");
                    for kind in ["heightmap", "color", "metalmap", "typemap"] {
                        let id_key = format!("{kind}_layer_asset_id");
                        let path_key = format!("{kind}_layer_asset_path");
                        if let Some(bar_graph::ParamValue::String(id)) =
                            node.params.get(&id_key).cloned()
                        {
                            if !id.is_empty() {
                                let abs = fc_dir
                                    .join(format!("{id}.bin"))
                                    .to_string_lossy()
                                    .into_owned();
                                node.params
                                    .insert(path_key, bar_graph::ParamValue::String(abs));
                            }
                        }
                    }
                }
                NodeType::ImportedTexture => {
                    if let Some(bar_graph::ParamValue::String(id)) =
                        node.params.get("asset_id").cloned()
                    {
                        if !id.is_empty() {
                            let abs = assets_dir
                                .join(format!("{id}.smt"))
                                .to_string_lossy()
                                .into_owned();
                            node.params.insert(
                                "asset_path".to_string(),
                                bar_graph::ParamValue::String(abs),
                            );
                        }
                    }
                    if let Some(bar_graph::ParamValue::String(id)) =
                        node.params.get("tile_index_id").cloned()
                    {
                        if !id.is_empty() {
                            let abs = assets_dir
                                .join(format!("{id}.idx"))
                                .to_string_lossy()
                                .into_owned();
                            node.params.insert(
                                "tile_index_path".to_string(),
                                bar_graph::ParamValue::String(abs),
                            );
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// Pack: walk every node that holds a file path; copy external files
    /// into the project directory and rewrite params to `bar://` relative
    /// paths. Painted node asset files (`.bin`) are copied into `assets/`;
    /// PassThrough files go into `passthrough/`; FileReference files into
    /// `assets/`. In-memory paths are rewritten so the running session
    /// uses the new local copies without needing a reload.
    pub(crate) fn pack_assets_for_save(
        &mut self,
        project_dir: &std::path::Path,
    ) -> Result<(), String> {
        let assets_dir = project_dir.join("assets");
        let passthrough_dir = project_dir.join("passthrough");
        let fc_dir = project_dir.join("final_composition");

        for (_, node) in self.graph.nodes_mut() {
            match node.node_type {
                NodeType::FileReference => {
                    pack_path_param(&mut node.params, "path", project_dir, &assets_dir, "")?;
                }
                NodeType::PassThrough => {
                    pack_passthrough_files(&mut node.params, project_dir, &passthrough_dir)?;
                }
                NodeType::PaintedHeightmap | NodeType::PaintedTexture => {
                    pack_painted_asset(&mut node.params, project_dir, &assets_dir)?;
                }
                NodeType::FinalComposition => {
                    // FC paint-layer assets live under
                    // `<project>/final_composition/`. Pack each kind's
                    // (asset_id, asset_path) pair the same way as the
                    // other binary assets -- copy in from wherever the
                    // session stamped it (temp dir, prior project dir)
                    // and rewrite the asset_path to point at the
                    // packed location.
                    for kind in ["heightmap", "color", "metalmap", "typemap"] {
                        let id_param = format!("{kind}_layer_asset_id");
                        let path_param = format!("{kind}_layer_asset_path");
                        pack_raw_asset(&mut node.params, &fc_dir, &id_param, &path_param, "bin")?;
                    }
                }
                NodeType::ImportedTexture => {
                    pack_raw_asset(
                        &mut node.params,
                        &assets_dir,
                        "asset_id",
                        "asset_path",
                        "smt",
                    )?;
                    pack_raw_asset(
                        &mut node.params,
                        &assets_dir,
                        "tile_index_id",
                        "tile_index_path",
                        "idx",
                    )?;
                }
                _ => {}
            }
        }

        Ok(())
    }

    /// Snapshot the live editor state for serialisation, stripping runtime-only
    /// params (`asset_path`) that must not be persisted to `recipe.json`.
    pub(crate) fn build_project(&mut self, path: &std::path::Path) -> bar_project::Project {
        use bar_project::recipe::{
            MapSettings, OutputConfig, Recipe, RecipeConnection, RecipeNode,
        };
        use bar_project::{EditorLayout, Position, Project};

        let mut nodes = Vec::new();
        let mut key_map: HashMap<NodeId, String> = HashMap::new();
        let mut layout_positions: HashMap<String, Position> = HashMap::new();
        let mut layout_sizes: HashMap<String, bar_project::NodeSize> = HashMap::new();

        for (id, node) in self.graph.nodes() {
            let key = format!("node_{}", id.0);
            key_map.insert(*id, key.clone());
            // Strip runtime-only params before persisting.
            let mut params = node.params.clone();
            params.remove("asset_path");
            params.remove("tile_index_path");
            nodes.push(RecipeNode {
                key: key.clone(),
                node_type: node.node_type.clone(),
                label: node.label.clone(),
                params,
            });

            if let Some(visual) = self.visuals.node_visuals.get(id) {
                layout_positions.insert(
                    key.clone(),
                    Position {
                        x: visual.position.x,
                        y: visual.position.y,
                    },
                );
                layout_sizes.insert(
                    key,
                    bar_project::NodeSize {
                        width: visual.size.x,
                        height: visual.size.y,
                    },
                );
            }
        }

        let connections = self
            .graph
            .connections()
            .iter()
            .filter_map(|conn| {
                let from_key = key_map.get(&conn.from.node_id)?;
                let to_key = key_map.get(&conn.to.node_id)?;
                Some(RecipeConnection {
                    from: format!("{}.{}", from_key, conn.from.port_name),
                    to: format!("{}.{}", to_key, conn.to.port_name),
                })
            })
            .collect();

        let recipe = Recipe {
            // Preserve the imported source name through save; the
            // .barproj directory stem is only a fallback for fresh
            // projects that never had a source mapinfo. Without this
            // check, every save rewrites recipe.json's `name` to the
            // lowercase directory slug -- silently downgrading the
            // human-readable engine identifier.
            name: self
                .map
                .recipe_meta
                .name
                .clone()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| {
                    path.file_stem()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_else(|| "Untitled".to_string())
                }),
            shortname: self.map.recipe_meta.shortname.clone(),
            description: self.map.recipe_meta.description.clone(),
            author: self.map.recipe_meta.author.clone(),
            version: self.map.recipe_meta.version.clone(),
            tip: self.map.recipe_meta.tip.clone(),
            depend: self.map.recipe_meta.depend.clone(),
            nodes,
            connections,
            output: OutputConfig {
                width: self.map.width,
                height: self.map.height,
                map_settings: MapSettings {
                    min_height: Some(self.map.min_height),
                    max_height: Some(self.map.max_height),
                    start_positions: self.map.settings.start_positions.clone(),
                    ..self.map.settings.clone()
                },
            },
            features: self.map.features.clone(),
        };

        Project {
            recipe,
            layout: EditorLayout {
                node_positions: layout_positions,
                node_sizes: layout_sizes,
                canvas_offset: (self.canvas.offset.x, self.canvas.offset.y),
                canvas_zoom: self.canvas.zoom,
                groups: self
                    .visuals
                    .groups
                    .iter()
                    .map(|(id, g)| bar_project::NodeGroup {
                        id: *id,
                        label: g.label.clone(),
                        member_keys: g
                            .member_ids
                            .iter()
                            .filter_map(|nid| key_map.get(nid).cloned())
                            .collect(),
                        color_idx: g.color_idx,
                        collapsed: g.collapsed,
                        is_subgraph: g.is_subgraph,
                        subgraph_inputs: g
                            .subgraph_inputs
                            .iter()
                            .map(|p| bar_project::SubgraphPort {
                                name: p.name.clone(),
                                label: p.label.clone(),
                                kind: p.kind.clone(),
                                binding: p.binding.as_ref().and_then(|(nid, port_name)| {
                                    key_map.get(nid).map(|k| format!("{}:{}", k, port_name))
                                }),
                            })
                            .collect(),
                        subgraph_outputs: g
                            .subgraph_outputs
                            .iter()
                            .map(|p| bar_project::SubgraphPort {
                                name: p.name.clone(),
                                label: p.label.clone(),
                                kind: p.kind.clone(),
                                binding: p.binding.as_ref().and_then(|(nid, port_name)| {
                                    key_map.get(nid).map(|k| format!("{}:{}", k, port_name))
                                }),
                            })
                            .collect(),
                        macro_params: g
                            .macro_params
                            .iter()
                            .filter_map(|p| {
                                let (nid, param_name) = p.binding.as_ref()?;
                                let key = key_map.get(nid)?;
                                Some(bar_project::MacroParamSpec {
                                    name: p.name.clone(),
                                    label: p.label.clone(),
                                    kind: p.kind.clone(),
                                    binding: format!("{}:{}", key, param_name),
                                    min: p.min,
                                    max: p.max,
                                })
                            })
                            .collect(),
                    })
                    .collect(),
                open_tabs: self
                    .canvas
                    .tabs
                    .iter()
                    // NodeEdit tabs are ephemeral authoring state; they
                    // don't survive save/load (filtered out here).
                    .filter_map(|view| match view {
                        CanvasView::Main => Some(bar_project::PersistedCanvasView::Main),
                        CanvasView::SubGraph(gid) => {
                            Some(bar_project::PersistedCanvasView::SubGraph { group_id: *gid })
                        }
                        CanvasView::NodeEdit(_) => None,
                    })
                    .collect(),
                active_tab: self.canvas.active_tab as u32,
            },
        }
    }
}

fn copy_dir_flat(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            std::fs::copy(entry.path(), dst.join(entry.file_name()))?;
        }
    }
    Ok(())
}
