//! Project file format for BAR map editor.
//!
//! A `.barproj` file is a single JSON file containing:
//! - The full recipe (graph + output config)
//! - Editor state (node positions, canvas offset, map dimensions)
//!
//! This allows round-tripping the entire workspace state through save/load.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::recipe::Recipe;

/// A complete project file — recipe + editor layout.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    /// The pipeline recipe (nodes, connections, output config).
    pub recipe: Recipe,
    /// Editor layout state (node positions, etc.).
    #[serde(default)]
    pub layout: EditorLayout,
}

/// Editor visual state that isn't part of the pipeline logic.
///
/// `Default` is implemented by hand rather than derived so `canvas_zoom`
/// defaults to `1.0` (identity) instead of `0.0`. A zero zoom would be
/// invalid — it collapses the graph and divides by zero in the
/// canvas-to-world transform — so neither a missing field nor a
/// default-constructed layout may produce it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditorLayout {
    /// Node positions keyed by their recipe key.
    #[serde(default)]
    pub node_positions: HashMap<String, Position>,
    /// Node sizes keyed by their recipe key. Optional — omitted from older saves uses defaults.
    #[serde(default)]
    pub node_sizes: HashMap<String, NodeSize>,
    /// Canvas pan offset.
    #[serde(default)]
    pub canvas_offset: (f32, f32),
    /// Canvas zoom factor (graph units → screen pixels). Defaults to
    /// `1.0` so saves predating zoom support open at identity scale.
    #[serde(default = "default_canvas_zoom")]
    pub canvas_zoom: f32,
    /// Visual node groupings. Purely organisational — they don't change
    /// graph topology or evaluation. The chip-style "subgraph as a
    /// reusable component" model lives at a separate layer once it
    /// lands; until then, a group is just a labelled rectangle drawn
    /// behind its member nodes on the canvas.
    #[serde(default)]
    pub groups: Vec<NodeGroup>,
    /// Canvas tabs the user had open. Saved so reopening a project
    /// puts you back on the SubGraph / Sculpt tab you were editing.
    /// Always implicitly contains Main at index 0; serialised
    /// entries describe additional tabs.
    #[serde(default)]
    pub open_tabs: Vec<PersistedCanvasView>,
    /// Index of the active tab. 0 = Main; otherwise an index into
    /// `open_tabs + 1`.
    #[serde(default)]
    pub active_tab: u32,
}

/// Default canvas zoom (`1.0` = identity). Used by serde for saves that
/// predate the zoom field and by `EditorLayout`'s `Default` impl.
fn default_canvas_zoom() -> f32 {
    1.0
}

impl Default for EditorLayout {
    fn default() -> Self {
        Self {
            node_positions: HashMap::new(),
            node_sizes: HashMap::new(),
            canvas_offset: (0.0, 0.0),
            canvas_zoom: default_canvas_zoom(),
            groups: Vec::new(),
            open_tabs: Vec::new(),
            active_tab: 0,
        }
    }
}

/// Persisted form of a canvas tab. SubGraphs are referenced by stable
/// group id (which round-trips through save/load already).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum PersistedCanvasView {
    Main,
    SubGraph { group_id: u64 },
}

/// A visual group on the editor canvas. Members are referenced by
/// recipe key (rather than runtime NodeId) so the grouping survives a
/// save/load cycle even though NodeIds get reassigned at load time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeGroup {
    /// Stable group identifier. Monotonically allocated by the editor;
    /// not reused after a group is deleted.
    pub id: u64,
    /// Human-readable label drawn at the top of the group's rect.
    pub label: String,
    /// Recipe keys of the nodes inside this group.
    pub member_keys: Vec<String>,
    /// Index into a fixed palette of group tints. Stored as a small int
    /// so future palette tweaks don't break old projects.
    #[serde(default)]
    pub color_idx: u8,
    /// True when the group is rendered collapsed (only meaningful for
    /// subgraphs — visual groups are always expanded).
    #[serde(default)]
    pub collapsed: bool,
    /// True when the group is a reusable subgraph: a node-like
    /// container with explicit external input / output ports.
    /// Visual-only groups have this set to `false`.
    #[serde(default)]
    pub is_subgraph: bool,
    /// Subgraph external port definitions. Empty when the group is
    /// not a subgraph; otherwise lists the heightmap inputs and
    /// outputs the surrounding graph sees.
    #[serde(default)]
    pub subgraph_inputs: Vec<SubgraphPort>,
    #[serde(default)]
    pub subgraph_outputs: Vec<SubgraphPort>,
    /// High-level parameters the SubGraph exposes on its property
    /// panel. Each one is bound to a specific inner-node parameter:
    /// editing the macro param writes through to the inner node
    /// immediately. Lets a macro present one or two domain-meaningful
    /// knobs (`Peak Density`, `Erosion Strength`) instead of
    /// requiring the user to expand the SubGraph to find them.
    #[serde(default)]
    pub macro_params: Vec<MacroParamSpec>,
}

/// One macro parameter on a SubGraph. The binding format is
/// `"<node_key>:<param_name>"`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MacroParamSpec {
    pub name: String,
    pub label: String,
    /// One of `"Float" | "UInt" | "Int" | "Bool" | "String"` —
    /// matches `ParamValue`'s variants.
    pub kind: String,
    /// Inner node + parameter this macro param drives, formatted
    /// `"<node_key>:<param_name>"`.
    pub binding: String,
    /// Optional inclusive min / max for numeric kinds, used to
    /// constrain the slider in the properties panel.
    #[serde(default)]
    pub min: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
}

/// One side of a subgraph's external interface — a single named port
/// on the collapsed subgraph block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubgraphPort {
    /// Stable identifier for the port (URL-safe, lowercase). Used
    /// when wiring connections to/from the subgraph.
    pub name: String,
    /// Display label shown next to the port handle.
    pub label: String,
    /// What kind of value flows through this port. Mirrors
    /// `bar_graph::PortKind` but stored as a string here so the
    /// project format doesn't depend directly on bar-graph for
    /// deserialisation.
    pub kind: String,
    /// Recipe-key + port name of the inner node this external port
    /// maps to, in the form `"<node_key>:<port_name>"`. The editor
    /// reroutes outer connections through this binding so the
    /// underlying graph engine sees them as direct wires to/from the
    /// inner node. Empty / None when the port hasn't been bound yet.
    #[serde(default)]
    pub binding: Option<String>,
}

/// Width/height for a node in the editor canvas.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct NodeSize {
    pub width: f32,
    pub height: f32,
}

/// 2D position for editor layout.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Position {
    pub x: f32,
    pub y: f32,
}

impl Project {
    /// Create a new project from a recipe with default layout.
    pub fn from_recipe(recipe: Recipe) -> Self {
        Self {
            recipe,
            layout: EditorLayout::default(),
        }
    }

    /// Serialize the recipe to pretty-printed JSON (no layout, no binary blobs).
    pub fn recipe_to_json(&self) -> Result<String> {
        serde_json::to_string_pretty(&self.recipe).context("Failed to serialize recipe")
    }

    /// Serialize the layout to pretty-printed JSON.
    pub fn layout_to_json(&self) -> Result<String> {
        serde_json::to_string_pretty(&self.layout).context("Failed to serialize layout")
    }

    /// Save to a `.barproj` package directory. Writes `recipe.json` and
    /// `layout.json` inside `dir`. Binary assets are NOT written here --
    /// the caller (GUI save flow) is responsible for writing asset files
    /// to `<dir>/assets/` before calling this.
    pub fn save(&self, dir: &Path) -> Result<()> {
        std::fs::create_dir_all(dir).with_context(|| format!("Cannot create {}", dir.display()))?;
        let recipe_json = self.recipe_to_json()?;
        std::fs::write(dir.join("recipe.json"), &recipe_json)
            .with_context(|| format!("Failed to write recipe.json in {}", dir.display()))?;
        let layout_json = self.layout_to_json()?;
        std::fs::write(dir.join("layout.json"), &layout_json)
            .with_context(|| format!("Failed to write layout.json in {}", dir.display()))?;
        Ok(())
    }

    /// Load from a `.barproj` package directory. Reads `recipe.json` and
    /// optionally `layout.json` (defaults to empty layout if absent).
    pub fn load(dir: &Path) -> Result<Self> {
        let recipe_json = std::fs::read_to_string(dir.join("recipe.json"))
            .with_context(|| format!("Cannot read recipe.json in {}", dir.display()))?;
        let recipe: Recipe =
            serde_json::from_str(&recipe_json).context("Failed to parse recipe.json")?;

        let layout_path = dir.join("layout.json");
        let layout = if layout_path.exists() {
            let layout_json = std::fs::read_to_string(&layout_path)
                .with_context(|| format!("Cannot read layout.json in {}", dir.display()))?;
            serde_json::from_str(&layout_json).context("Failed to parse layout.json")?
        } else {
            EditorLayout::default()
        };

        Ok(Self { recipe, layout })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recipe::Recipe;

    #[test]
    fn test_groups_roundtrip() {
        let recipe = Recipe::sample();
        let mut project = Project::from_recipe(recipe);
        project.layout.groups.push(super::NodeGroup {
            id: 7,
            label: "Mountain stack".to_string(),
            member_keys: vec!["perlin".to_string(), "blur".to_string()],
            color_idx: 2,
            collapsed: false,
            is_subgraph: false,
            subgraph_inputs: Vec::new(),
            subgraph_outputs: Vec::new(),
            macro_params: Vec::new(),
        });
        let dir = std::env::temp_dir().join("bar_groups_roundtrip_test.barproj");
        let _ = std::fs::remove_dir_all(&dir);
        project.save(&dir).unwrap();
        let loaded = Project::load(&dir).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(loaded.layout.groups.len(), 1);
        let g = &loaded.layout.groups[0];
        assert_eq!(g.id, 7);
        assert_eq!(g.label, "Mountain stack");
        assert_eq!(g.member_keys, vec!["perlin", "blur"]);
        assert_eq!(g.color_idx, 2);
    }

    #[test]
    fn test_project_roundtrip() {
        let recipe = Recipe::sample();
        let mut project = Project::from_recipe(recipe);
        project
            .layout
            .node_positions
            .insert("perlin".to_string(), Position { x: 100.0, y: 200.0 });
        project.layout.canvas_offset = (50.0, -30.0);
        project.layout.canvas_zoom = 1.75;
        project.recipe.output.width = 512;
        project.recipe.output.height = 512;

        let dir = std::env::temp_dir().join("bar_project_roundtrip_test.barproj");
        let _ = std::fs::remove_dir_all(&dir);
        project.save(&dir).unwrap();
        let loaded = Project::load(&dir).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(loaded.recipe.name, project.recipe.name);
        assert_eq!(loaded.recipe.nodes.len(), project.recipe.nodes.len());
        assert_eq!(loaded.layout.canvas_offset, (50.0, -30.0));
        assert_eq!(loaded.layout.canvas_zoom, 1.75);
        assert_eq!(loaded.recipe.output.width, 512);
        assert!(loaded.layout.node_positions.contains_key("perlin"));
    }

    #[test]
    fn editor_layout_defaults_zoom_to_one() {
        // Saves predating the zoom field (and any default-constructed
        // layout) must open at identity scale, never the invalid 0.0.
        assert_eq!(EditorLayout::default().canvas_zoom, 1.0);
        let without_zoom = r#"{ "canvas_offset": [10.0, 20.0] }"#;
        let layout: EditorLayout = serde_json::from_str(without_zoom).unwrap();
        assert_eq!(layout.canvas_zoom, 1.0);
    }

    #[test]
    fn test_project_save_load() {
        let recipe = Recipe::sample();
        let project = Project::from_recipe(recipe);

        let base = std::env::temp_dir().join("bar_project_test");
        let pkg_dir = base.join("test.barproj");
        let _ = std::fs::remove_dir_all(&pkg_dir);

        project.save(&pkg_dir).unwrap();
        let loaded = Project::load(&pkg_dir).unwrap();

        assert_eq!(loaded.recipe.name, project.recipe.name);
        assert_eq!(loaded.recipe.nodes.len(), project.recipe.nodes.len());

        std::fs::remove_dir_all(&base).ok();
    }
}
