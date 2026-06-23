//! Result type for .sd7 work-directory scanning, plus the conversion to `Project`.
//!
//! `WorkDirScan` is produced by `bar_engine::extract_sd7_to_work_dir` and
//! consumed by the GUI's import workflow.  Keeping it here (rather than in
//! bar-engine) lets the GUI depend on this lightweight type without pulling in
//! the full engine.

use std::collections::HashMap;
use std::path::PathBuf;

use bar_graph::{NodeType, ParamValue};

/// Fixed sidecar filename for the SMF-embedded minimap. The extract step
/// decodes the embedded DXT1 minimap to RGBA8 and writes a PNG with this
/// name into the work directory; persistence copies it into the .barproj
/// on save. Loaded by `sync_grass_shading_tex` as the engine-faithful
/// fallback for the `MAP_BASE_GRASS_TEX` map-edge sampler when
/// `mapinfo.resources.grassShadingTex` is unset.
pub const SMF_MINIMAP_SIDE_CAR: &str = "_bme_smf_minimap.png";

use crate::package::{AssetHeader, AssetId, AssetKind};
use crate::project::{EditorLayout, NodeSize, Position, Project};
use crate::recipe::{MapSettings, OutputConfig, Recipe, RecipeConnection, RecipeNode};

/// A binary asset produced during SD7 import that must be written to the
/// project's `assets/` directory before the project can be evaluated.
pub struct PendingAsset {
    /// Key of the recipe node this asset belongs to.
    pub node_key: String,
    /// Stable identifier stored in the node's `asset_id` param.
    pub id: AssetId,
    /// Pixel format + dimensions.
    pub header: AssetHeader,
    /// Raw pixel bytes.
    pub data: Vec<u8>,
}

/// A raw (non-BARASSET-format) file produced during SD7 import. Used for
/// `.smt` tile atlases and `.idx` tile-index maps that have their own native
/// formats. The caller writes the file to `assets/<id>.<extension>` and injects
/// the resolved path into the matching graph node.
pub struct PendingRawFile {
    /// Key of the recipe node this file belongs to.
    pub node_key: String,
    /// Stable identifier stored in the node param named `match_param`.
    pub id: AssetId,
    /// Name of the node param holding this file's UUID (e.g. `"asset_id"`).
    pub match_param: String,
    /// Name of the node param to inject the resolved path into (e.g. `"asset_path"`).
    pub inject_param: String,
    /// Copy from this source path (avoids loading large files into RAM). When
    /// `None` the caller writes `data` directly.
    pub source_path: Option<PathBuf>,
    /// Raw bytes to write. Only used when `source_path` is `None`.
    pub data: Vec<u8>,
    /// File extension without leading dot (e.g. `"smt"`, `"idx"`).
    pub extension: String,
}

/// Convert a scanned SD7 work directory into a `Project` ready for `apply_project`
/// plus a list of binary assets that the caller must write to the project directory.
///
/// Creates PaintedHeightmap nodes for the heightmap, metalmap, and typemap,
/// a PaintedTexture (or ImportedTexture) for the assembled SMT texture, a
/// NormalMap, a PassThrough for ancillary files, and a FinalComposition
/// terminal that all procedural sources wire into.
///
/// The returned `PendingAsset` list must be written to `<proj_dir>/assets/<id>.bin`
/// before the graph can be evaluated; until then the executor will produce
/// empty outputs for those nodes. The returned `PendingRawFile` list must be
/// written to `<proj_dir>/assets/<id>.<ext>` (no BARASSET header).
pub fn scan_to_project(scan: &WorkDirScan) -> (Project, Vec<PendingAsset>, Vec<PendingRawFile>) {
    let source_x = 80.0_f32;
    let final_comp_x = 540.0_f32;
    let nm_x = (source_x + 165.0 + final_comp_x) / 2.0;

    let mut nodes: Vec<RecipeNode> = Vec::new();
    let mut connections: Vec<RecipeConnection> = Vec::new();
    let mut node_positions: HashMap<String, Position> = HashMap::new();
    let mut node_sizes: HashMap<String, NodeSize> = HashMap::new();
    let mut pending: Vec<PendingAsset> = Vec::new();
    let mut raw_files: Vec<PendingRawFile> = Vec::new();

    // Passthrough covers ancillary files only (LuaGaia gadgets,
    // LuaRules scripts, custom features under `features/`, sounds,
    // etc.) -- everything BME doesn't yet structurally model. Config
    // files BME owns in the recipe (currently `mapinfo.lua`) are
    // filtered out at the extract.rs boundary and must not appear
    // here. The original .smf/.smt are also excluded -- regenerated
    // on export from embedded node data.
    let passthrough_entries: Vec<(PathBuf, PathBuf)> = scan.passthrough_files.clone();

    // Helper: add a PaintedHeightmap node backed by a binary asset.
    // Carries separate width + height so non-square maps round-trip
    // without an aspect-ratio crop. The `kind` matches the byte
    // payload: heightmap is `GrayscaleF32` (4 bytes/pixel, captures
    // full SMF precision); metalmap / typemap are `GrayscaleU8` (1
    // byte/pixel, naturally quantised).
    //
    // `sampling` selects how the executor resamples the asset to the
    // eval resolution. "smooth" (bilinear) is correct for continuous
    // data; "nearest" must be set for quantised data (engine
    // metalmap / typemap) so single-pixel metal spots or terrain-type
    // boundaries round-trip through the eval pipeline without being
    // averaged into faded blobs. Bilinear-blurring a sparse metalmap
    // both dilutes individual spot density AND merges adjacent
    // spots, which the engine's `gui_metalspots` widget then
    // mis-aggregates or drops via the `maxValue = 15` gate.
    let add_hm = |key: &str,
                  label: &str,
                  y: f32,
                  data: &[u8],
                  width: u32,
                  height: u32,
                  kind: AssetKind,
                  sampling: &str,
                  nodes: &mut Vec<RecipeNode>,
                  positions: &mut HashMap<String, Position>,
                  sizes: &mut HashMap<String, NodeSize>,
                  pending: &mut Vec<PendingAsset>| {
        let id = AssetId::new();
        let mut params = HashMap::new();
        params.insert("asset_id".to_string(), ParamValue::String(id.0.clone()));
        params.insert("width".to_string(), ParamValue::UInt(width));
        params.insert("height".to_string(), ParamValue::UInt(height));
        params.insert(
            "sampling".to_string(),
            ParamValue::String(sampling.to_string()),
        );
        nodes.push(RecipeNode {
            key: key.to_string(),
            node_type: NodeType::PaintedHeightmap,
            label: label.to_string(),
            params,
        });
        positions.insert(key.to_string(), Position { x: source_x, y });
        sizes.insert(
            key.to_string(),
            NodeSize {
                width: 165.0,
                height: 80.0,
            },
        );
        pending.push(PendingAsset {
            node_key: key.to_string(),
            id,
            header: AssetHeader {
                kind,
                width,
                height,
            },
            data: data.to_vec(),
        });
    };

    // Heightmap node -- f32, captures full SMF precision. F32 path
    // ignores the `sampling` param (continuous data only flows
    // through the f32 helper) but we set "smooth" anyway so the
    // recipe carries an explicit value.
    let has_heightmap = !scan.heightmap_data.is_empty();
    if has_heightmap {
        add_hm(
            "hm",
            "Heightmap",
            80.0,
            &scan.heightmap_data,
            scan.heightmap_w,
            scan.heightmap_h,
            AssetKind::GrayscaleF32,
            "smooth",
            &mut nodes,
            &mut node_positions,
            &mut node_sizes,
            &mut pending,
        );
    }

    // Metalmap node -- u8 metal density per pixel. Use nearest-
    // neighbour resampling so single-pixel metal spots survive the
    // eval-pipeline upsample / SMF-export downsample round-trip and
    // the engine's `gui_metalspots` widget finds them.
    if !scan.metalmap_data.is_empty() {
        add_hm(
            "metal",
            "Metal Map",
            220.0,
            &scan.metalmap_data,
            scan.metalmap_w,
            scan.metalmap_h,
            AssetKind::GrayscaleU8,
            "nearest",
            &mut nodes,
            &mut node_positions,
            &mut node_sizes,
            &mut pending,
        );
    }

    // Typemap node -- u8 terrain-type index per pixel. Same nearest-
    // neighbour rationale as the metalmap: each value is an integer
    // terrain-type id, averaging neighbours produces nonsense type
    // ids on cell boundaries.
    if !scan.typemap_data.is_empty() {
        add_hm(
            "type",
            "Type Map",
            360.0,
            &scan.typemap_data,
            scan.typemap_w,
            scan.typemap_h,
            AssetKind::GrayscaleU8,
            "nearest",
            &mut nodes,
            &mut node_positions,
            &mut node_sizes,
            &mut pending,
        );
    }

    // Texture node: ImportedTexture when the original .smt is available (preferred
    // path -- preserves tile resolution for compile step); PaintedTexture as fallback
    // when only the assembled RGB preview is present.
    let has_texture = scan.smt_abs.is_some() || !scan.texture_data.is_empty();
    if let (Some(smt_path), Some((tiles_x, tiles_y))) = (&scan.smt_abs, &scan.tile_grid) {
        let smt_id = AssetId::new();
        let idx_id = AssetId::new();
        let mut params = HashMap::new();
        params.insert("asset_id".to_string(), ParamValue::String(smt_id.0.clone()));
        params.insert(
            "tile_index_id".to_string(),
            ParamValue::String(idx_id.0.clone()),
        );
        params.insert("tiles_x".to_string(), ParamValue::UInt(*tiles_x));
        params.insert("tiles_y".to_string(), ParamValue::UInt(*tiles_y));
        nodes.push(RecipeNode {
            key: "tex".to_string(),
            node_type: NodeType::ImportedTexture,
            label: "Texture".to_string(),
            params,
        });
        node_positions.insert(
            "tex".to_string(),
            Position {
                x: source_x,
                y: 500.0,
            },
        );
        node_sizes.insert(
            "tex".to_string(),
            NodeSize {
                width: 165.0,
                height: 80.0,
            },
        );
        // Serialize tile indices as raw i32 little-endian bytes for the .idx file.
        let idx_bytes: Vec<u8> = scan
            .tile_indices
            .iter()
            .flat_map(|&v| v.to_le_bytes())
            .collect();
        raw_files.push(PendingRawFile {
            node_key: "tex".to_string(),
            id: smt_id,
            match_param: "asset_id".to_string(),
            inject_param: "asset_path".to_string(),
            source_path: Some(smt_path.clone()),
            data: Vec::new(),
            extension: "smt".to_string(),
        });
        raw_files.push(PendingRawFile {
            node_key: "tex".to_string(),
            id: idx_id,
            match_param: "tile_index_id".to_string(),
            inject_param: "tile_index_path".to_string(),
            source_path: None,
            data: idx_bytes,
            extension: "idx".to_string(),
        });
    } else if !scan.texture_data.is_empty() {
        let id = AssetId::new();
        let tex_w = scan.texture_w;
        let tex_h = scan.texture_h;
        let mut params = HashMap::new();
        params.insert("asset_id".to_string(), ParamValue::String(id.0.clone()));
        params.insert("width".to_string(), ParamValue::UInt(tex_w));
        params.insert("height".to_string(), ParamValue::UInt(tex_h));
        nodes.push(RecipeNode {
            key: "tex".to_string(),
            node_type: NodeType::PaintedTexture,
            label: "Texture".to_string(),
            params,
        });
        node_positions.insert(
            "tex".to_string(),
            Position {
                x: source_x,
                y: 500.0,
            },
        );
        node_sizes.insert(
            "tex".to_string(),
            NodeSize {
                width: 165.0,
                height: 80.0,
            },
        );
        pending.push(PendingAsset {
            node_key: "tex".to_string(),
            id,
            header: AssetHeader {
                kind: AssetKind::RgbU8,
                width: tex_w,
                height: tex_h,
            },
            data: scan.texture_data.clone(),
        });
    }

    // PassThrough for ancillary files (LuaGaia / LuaRules scripts,
    // custom feature definitions, sounds, etc.)
    // Absolute paths point into the work dir; the GUI save flow copies
    // them into <proj_dir>/passthrough/ on first save.
    let has_pass = !passthrough_entries.is_empty();
    if has_pass {
        let file_list: String = passthrough_entries
            .iter()
            .map(|(abs, rel)| {
                let bundle = rel.to_string_lossy().replace('\\', "/");
                format!("{}|{}", abs.to_string_lossy(), bundle)
            })
            .collect::<Vec<_>>()
            .join("\n");
        let mut params = HashMap::new();
        params.insert("files".to_string(), ParamValue::String(file_list));
        nodes.push(RecipeNode {
            key: "pass".to_string(),
            node_type: NodeType::PassThrough,
            label: "Pass-Through".to_string(),
            params,
        });
        node_positions.insert(
            "pass".to_string(),
            Position {
                x: source_x,
                y: 640.0,
            },
        );
        node_sizes.insert(
            "pass".to_string(),
            NodeSize {
                width: 165.0,
                height: 80.0,
            },
        );
    }

    // NormalMap (derives from the heightmap)
    if has_heightmap {
        nodes.push(RecipeNode {
            key: "nm".to_string(),
            node_type: NodeType::NormalMap,
            label: "Normal Map".to_string(),
            params: HashMap::new(),
        });
        node_positions.insert("nm".to_string(), Position { x: nm_x, y: 478.0 });
        node_sizes.insert(
            "nm".to_string(),
            NodeSize {
                width: 140.0,
                height: 60.0,
            },
        );
    }

    // FinalComposition (always; mandatory anchor for paint layers).
    // Procedural outputs wire INTO this node, which forwards each
    // input to its matching output through the per-kind layer composite.
    // Asset ids for all four paintable layers are minted now so the
    // FC node ships with stable per-layer identity from the moment
    // the project comes into existence -- regardless of whether the
    // user ever paints into a given kind. The on-disk layer file is
    // created lazily on first stroke.
    {
        let mut fc_params: HashMap<String, ParamValue> = HashMap::new();
        crate::fc::mint_fc_layer_ids(&mut fc_params);
        nodes.push(RecipeNode {
            key: "final_composition".to_string(),
            node_type: NodeType::FinalComposition,
            label: "Final Composition".to_string(),
            params: fc_params,
        });
        node_positions.insert(
            "final_composition".to_string(),
            Position {
                x: final_comp_x,
                y: 270.0,
            },
        );
        node_sizes.insert(
            "final_composition".to_string(),
            NodeSize {
                width: 165.0,
                height: 210.0,
            },
        );
    }

    // Procedural source nodes wire directly into FC's inputs below.

    // Connections: procedural sources feed FinalComposition.
    if has_heightmap {
        connections.push(RecipeConnection {
            from: "hm.output".to_string(),
            to: "final_composition.heightmap".to_string(),
        });
        connections.push(RecipeConnection {
            from: "hm.output".to_string(),
            to: "nm.input".to_string(),
        });
        connections.push(RecipeConnection {
            from: "nm.output".to_string(),
            to: "final_composition.normalmap".to_string(),
        });
    }
    if !scan.metalmap_data.is_empty() {
        connections.push(RecipeConnection {
            from: "metal.output".to_string(),
            to: "final_composition.metalmap".to_string(),
        });
    }
    if !scan.typemap_data.is_empty() {
        connections.push(RecipeConnection {
            from: "type.output".to_string(),
            to: "final_composition.typemap".to_string(),
        });
    }
    if has_texture {
        connections.push(RecipeConnection {
            from: "tex.output".to_string(),
            to: "final_composition.texture".to_string(),
        });
    }
    if has_pass {
        connections.push(RecipeConnection {
            from: "pass.files".to_string(),
            to: "final_composition.files".to_string(),
        });
    }

    let (width, height) = scan.map_dims.unwrap_or((256, 256));
    let (min_height, max_height) = scan.height_range.unwrap_or((0.0, 800.0));
    let mut map_settings = MapSettings {
        min_height: Some(min_height),
        max_height: Some(max_height),
        ..MapSettings::default()
    };
    if let Some(lua) = scan.mapinfo_lua.as_deref() {
        crate::mapinfo::apply_mapinfo_overrides(lua, &mut map_settings);
    }

    // SMF grass-map fallback: if mapinfo didn't specify a custom
    // `grassDistTGA` and the scan materialised the SMF's vegetation
    // header into a `grassmap.png` (see `extract.rs::scan_work_dir`),
    // point the widget at that file. Mirrors BAR's widget
    // `Spring.GetGrass` fallback (`map_grass_gl4.lua:856-892`).
    let has_grassmap_png = scan
        .passthrough_files
        .iter()
        .any(|(_, rel)| rel.to_string_lossy().eq_ignore_ascii_case("grassmap.png"));
    if has_grassmap_png
        && map_settings
            .custom_grass
            .dist_tga
            .as_deref()
            .map(str::is_empty)
            .unwrap_or(true)
    {
        map_settings.custom_grass.dist_tga = Some("grassmap.png".to_string());
    }

    // Identity fields parsed straight from mapinfo.lua. Without these
    // the Edit Map Info -> Identity tab comes up empty on every
    // imported project. This path (work-dir scan -> scan_to_project)
    // is the GUI's "Import .sd7" flow; the CLI's
    // `bar_engine::import_sd7_to_project` now also routes through
    // here so the two stay in sync.
    let mapinfo_lua = scan.mapinfo_lua.as_deref().unwrap_or("");
    let recipe_shortname = crate::mapinfo::parse_mapinfo_string(mapinfo_lua, "shortname");
    let recipe_description =
        crate::mapinfo::parse_mapinfo_string(mapinfo_lua, "description").unwrap_or_default();
    let recipe_author = crate::mapinfo::parse_mapinfo_string(mapinfo_lua, "author");
    let recipe_version = crate::mapinfo::parse_mapinfo_string(mapinfo_lua, "version");
    let recipe_tip = crate::mapinfo::parse_mapinfo_string(mapinfo_lua, "tip");
    let recipe_depend = crate::mapinfo::parse_mapinfo_string_list(mapinfo_lua, "depend")
        .unwrap_or_else(|| vec!["Map Helper v1".to_string()]);
    // The engine builds the in-game archive identifier as
    // `name .. " " .. version` from mapinfo.lua. If we seed `name`
    // from the SD7 filename (e.g. "onyx_cauldron_2.2.3") instead of
    // the mapinfo's `name` field (e.g. "Onyx Cauldron"), the bundler
    // emits "onyx_cauldron_2.2.3 2.2.3" and the engine fails to find
    // its own map archive on test-in-BAR.
    let recipe_name = crate::mapinfo::parse_mapinfo_string(mapinfo_lua, "name")
        .unwrap_or_else(|| scan.map_name.clone());

    let recipe = Recipe {
        name: recipe_name,
        shortname: recipe_shortname,
        description: recipe_description,
        author: recipe_author,
        version: recipe_version,
        tip: recipe_tip,
        depend: recipe_depend,
        nodes,
        connections,
        output: OutputConfig {
            width,
            height,
            map_settings,
        },
        features: scan.features.clone(),
    };

    let layout = EditorLayout {
        node_positions,
        node_sizes,
        canvas_offset: (0.0, 0.0),
        canvas_zoom: 1.0,
        groups: Vec::new(),
        open_tabs: Vec::new(),
        active_tab: 0,
    };

    (Project { recipe, layout }, pending, raw_files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bar_graph::{NodeType, ParamValue as PV};
    use std::path::PathBuf;

    fn empty_scan(name: &str) -> WorkDirScan {
        WorkDirScan {
            work_dir: PathBuf::from("/tmp"),
            map_name: name.to_string(),
            smf_abs: None,
            smf_rel: None,
            smt_abs: None,
            smt_rel: None,
            tile_grid: None,
            map_dims: None,
            height_range: None,
            passthrough_files: Vec::new(),
            heightmap_data: Vec::new(),
            heightmap_w: 0,
            heightmap_h: 0,
            metalmap_data: Vec::new(),
            metalmap_w: 0,
            metalmap_h: 0,
            typemap_data: Vec::new(),
            typemap_w: 0,
            typemap_h: 0,
            texture_data: Vec::new(),
            texture_w: 0,
            texture_h: 0,
            tile_indices: Vec::new(),
            features: Vec::new(),
            mapinfo_lua: None,
        }
    }

    fn node_keys(p: &Project) -> Vec<&str> {
        p.recipe.nodes.iter().map(|n| n.key.as_str()).collect()
    }

    fn has_asset_id(p: &Project, node_key: &str) -> bool {
        p.recipe
            .nodes
            .iter()
            .find(|n| n.key == node_key)
            .and_then(|n| n.params.get("asset_id"))
            .map(|v| matches!(v, PV::String(s) if !s.is_empty()))
            .unwrap_or(false)
    }

    #[test]
    fn empty_scan_produces_final_composition() {
        // Every project (even one bootstrapped from an empty scan) has
        // FinalComposition as its sole terminal node.
        let scan = empty_scan("test_map");
        let (p, pending, raw) = scan_to_project(&scan);
        assert_eq!(p.recipe.nodes.len(), 1);
        assert_eq!(p.recipe.nodes[0].node_type, NodeType::FinalComposition);
        assert!(p.recipe.connections.is_empty());
        assert!(pending.is_empty());
        assert!(raw.is_empty());
    }

    #[test]
    fn heightmap_only_adds_hm_nm_preview_not_others() {
        let mut scan = empty_scan("test");
        // 4x4 grayscale heightmap stored as raw F32 bytes (one f32 per
        // pixel; 64 bytes total). Values aren't significant for this
        // test -- only the shape and the (w, h) bookkeeping matter.
        scan.heightmap_data = vec![0u8; 64];
        scan.heightmap_w = 4;
        scan.heightmap_h = 4;
        scan.map_dims = Some((512, 512));
        let (p, pending, _) = scan_to_project(&scan);
        let keys = node_keys(&p);
        for k in ["hm", "nm", "final_composition"] {
            assert!(keys.contains(&k), "missing: {k}");
        }
        for k in ["metal", "type", "tex", "pass"] {
            assert!(!keys.contains(&k), "unexpected: {k}");
        }
        assert_eq!(pending.len(), 1);
        assert!(has_asset_id(&p, "hm"));
    }

    #[test]
    fn full_scan_all_nodes_present() {
        let mut scan = empty_scan("full");
        scan.heightmap_data = vec![0u8; 64]; // 4*4 f32 = 64 bytes
        scan.heightmap_w = 4;
        scan.heightmap_h = 4;
        scan.metalmap_data = vec![0xcdu8; 16]; // 4*4 u8 = 16 bytes
        scan.metalmap_w = 4;
        scan.metalmap_h = 4;
        scan.typemap_data = vec![0xefu8; 16];
        scan.typemap_w = 4;
        scan.typemap_h = 4;
        scan.texture_data = vec![0x01u8; 48]; // 4*4*3 RGB bytes
        scan.texture_w = 4;
        scan.texture_h = 4;
        scan.passthrough_files = vec![(PathBuf::from("/tmp/a.lua"), PathBuf::from("a.lua"))];
        let (p, pending, _) = scan_to_project(&scan);
        let keys = node_keys(&p);
        for k in [
            "hm",
            "metal",
            "type",
            "tex",
            "nm",
            "pass",
            "final_composition",
        ] {
            assert!(keys.contains(&k), "missing: {k}");
        }
        assert_eq!(pending.len(), 4); // hm, metal, type, tex (PaintedTexture fallback -- no smt_abs)
    }

    #[test]
    fn connections_wire_heightmap_through_nm_to_final_composition() {
        let mut scan = empty_scan("wire");
        scan.heightmap_data = vec![0u8; 64];
        scan.heightmap_w = 4;
        scan.heightmap_h = 4;
        let (p, _, _) = scan_to_project(&scan);
        let froms: Vec<&str> = p
            .recipe
            .connections
            .iter()
            .map(|c| c.from.as_str())
            .collect();
        let tos: Vec<&str> = p.recipe.connections.iter().map(|c| c.to.as_str()).collect();
        assert!(froms.contains(&"hm.output"), "hm.output not in froms");
        assert!(
            tos.contains(&"final_composition.heightmap"),
            "final_composition.heightmap not in tos"
        );
        assert!(tos.contains(&"nm.input"), "nm.input not in tos");
    }

    #[test]
    fn height_range_propagates_to_map_settings() {
        let mut scan = empty_scan("heights");
        scan.height_range = Some((100.0, 900.0));
        let (p, _, _) = scan_to_project(&scan);
        assert_eq!(p.recipe.output.map_settings.min_height, Some(100.0));
        assert_eq!(p.recipe.output.map_settings.max_height, Some(900.0));
    }

    #[test]
    fn no_heightmap_skips_nm_and_preview() {
        let mut scan = empty_scan("nohm");
        scan.metalmap_data = vec![0xffu8; 16];
        scan.metalmap_w = 4;
        scan.metalmap_h = 4;
        let (p, _, _) = scan_to_project(&scan);
        let keys = node_keys(&p);
        assert!(!keys.contains(&"hm"), "unexpected hm");
        assert!(!keys.contains(&"nm"), "unexpected nm");
        assert!(!keys.contains(&"preview"), "unexpected preview");
        assert!(keys.contains(&"metal"), "missing metal");
        assert!(
            keys.contains(&"final_composition"),
            "missing final_composition"
        );
    }

    #[test]
    fn features_propagate_to_recipe() {
        let mut scan = empty_scan("feat_test");
        scan.features = vec![crate::recipe::PlacedFeature {
            feature_type: "arborreal".to_string(),
            x: 100.0,
            y: 0.0,
            z: 200.0,
            angle: 1.57,
            taken_damage: 0,
            source: crate::recipe::FeatureSource::Smf,
        }];
        let (p, _, _) = scan_to_project(&scan);
        assert_eq!(p.recipe.features.len(), 1);
        assert_eq!(p.recipe.features[0].feature_type, "arborreal");
        assert!((p.recipe.features[0].x - 100.0).abs() < 0.001);
    }

    #[test]
    fn passthrough_files_create_pass_node_and_connect_to_final_composition() {
        // Pass-through files wire directly into FinalComposition's
        // `files` input (FC is the terminal node; no separate Bundler).
        // Use a LuaGaia gadget here -- legitimate passthrough content.
        // `mapinfo.lua` would be wrong: it's owned by the recipe and
        // filtered out at the extract boundary.
        let mut scan = empty_scan("pass");
        scan.passthrough_files = vec![(
            PathBuf::from("/tmp/LuaGaia/main.lua"),
            PathBuf::from("LuaGaia/main.lua"),
        )];
        let (p, _, _) = scan_to_project(&scan);
        let keys = node_keys(&p);
        assert!(keys.contains(&"pass"), "missing pass");
        let pass_to_fc = p
            .recipe
            .connections
            .iter()
            .any(|c| c.from == "pass.files" && c.to == "final_composition.files");
        assert!(
            pass_to_fc,
            "pass.files -> final_composition.files connection missing"
        );
    }
}

/// Result of scanning an extracted .sd7 work directory.
#[derive(Debug)]
pub struct WorkDirScan {
    /// Absolute path to the work directory.
    pub work_dir: PathBuf,
    /// Map name derived from the archive filename stem.
    pub map_name: String,
    /// Absolute path to the first `.smf` file found (if any). Kept for
    /// PassThrough file-list construction (other callers that need the path).
    pub smf_abs: Option<PathBuf>,
    /// Archive-relative path to the `.smf` file (e.g. `maps/mymap.smf`).
    pub smf_rel: Option<PathBuf>,
    /// Absolute path to the first `.smt` file found (if any).
    pub smt_abs: Option<PathBuf>,
    /// Archive-relative path to the `.smt` file.
    pub smt_rel: Option<PathBuf>,
    /// Tile grid dimensions `(tiles_x, tiles_y)` read from the SMF header.
    pub tile_grid: Option<(u32, u32)>,
    /// Heightmap pixel dimensions read from the SMF header (`map_x + 1` x `map_y + 1`).
    /// `None` when no SMF file is present.
    pub map_dims: Option<(u32, u32)>,
    /// Terrain height range from the SMF header (world units).
    /// `None` when no SMF file is present.
    pub height_range: Option<(f32, f32)>,
    /// All other files as `(absolute_path, archive_relative_path)` pairs.
    /// Does NOT include the .smf or .smt themselves -- those are embedded
    /// in the node data below and regenerated on export.
    pub passthrough_files: Vec<(PathBuf, PathBuf)>,

    // Binary pixel data extracted from the SMF/SMT. Empty Vec means the data
    // was not available (e.g. no SMF found). Written to asset files by the
    // caller; never embedded in recipe.json. Each plane carries separate
    // width + height so non-square maps round-trip without an aspect-ratio
    // crop -- the extracted pixel buffer is `width * height` (heightmap is
    // 4 bytes/pixel f32; metal/type are 1 byte; texture is 3 bytes RGB).
    pub heightmap_data: Vec<u8>,
    pub heightmap_w: u32,
    pub heightmap_h: u32,

    pub metalmap_data: Vec<u8>,
    pub metalmap_w: u32,
    pub metalmap_h: u32,

    pub typemap_data: Vec<u8>,
    pub typemap_w: u32,
    pub typemap_h: u32,

    /// Raw RGB (3 bytes/pixel) assembled texture. Only populated when
    /// there is no `.smt` file (fallback path).
    pub texture_data: Vec<u8>,
    pub texture_w: u32,
    pub texture_h: u32,

    /// Tile index map from the SMF: maps each tile-grid slot to an SMT tile index.
    /// Length = tiles_x * tiles_y. Empty when no SMF is present.
    pub tile_indices: Vec<i32>,

    /// Feature placements extracted from the SMF feature section.
    pub features: Vec<crate::recipe::PlacedFeature>,

    /// Raw contents of `mapinfo.lua` from the work directory, if present.
    /// `scan_to_project` parses water/lighting/etc. overrides from this so
    /// per-map values (fresnel, sun direction, etc.) land in the recipe's
    /// `MapSettings`. The UI-driven SD7 import path goes through this
    /// scan; the parsing previously lived in `bar-engine::importer` and
    /// was bypassed here, leaving every imported map at the defaults.
    pub mapinfo_lua: Option<String>,
}
