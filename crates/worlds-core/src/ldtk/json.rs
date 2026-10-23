//! The parts of LDtk's JSON (1.5) the importer reads. Unknown keys are
//! ignored, missing keys take defaults, so older or newer files still parse.
//! Derived `__*` fields are read only where they are the resolved value of
//! something (`__value` of a field instance) or a cross-check (`__cWid`).

use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Project {
    pub json_version: String,
    pub default_grid_size: u32,
    pub default_level_bg_color: Option<String>,
    pub world_layout: Option<String>,
    pub external_levels: bool,
    pub defs: Defs,
    pub levels: Vec<Level>,
    pub worlds: Vec<World>,
    pub flags: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Defs {
    pub layers: Vec<LayerDef>,
    pub entities: Vec<EntityDef>,
    pub tilesets: Vec<Tileset>,
    pub enums: Vec<EnumDef>,
    pub external_enums: Vec<EnumDef>,
    pub level_fields: Vec<FieldDef>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct LayerDef {
    pub identifier: String,
    #[serde(rename = "type")]
    pub ty: String,
    pub uid: i64,
    pub doc: Option<String>,
    pub grid_size: u32,
    pub display_opacity: Option<f64>,
    pub px_offset_x: i32,
    pub px_offset_y: i32,
    pub parallax_factor_x: f64,
    pub parallax_factor_y: f64,
    pub required_tags: Vec<String>,
    pub excluded_tags: Vec<String>,
    pub auto_tiles_killed_by_other_layer_uid: Option<i64>,
    pub int_grid_values: Vec<IntGridValue>,
    pub int_grid_values_groups: Vec<IntGridGroup>,
    pub auto_source_layer_def_uid: Option<i64>,
    pub tileset_def_uid: Option<i64>,
    pub tile_pivot_x: f64,
    pub tile_pivot_y: f64,
    pub biome_field_uid: Option<i64>,
    pub auto_rule_groups: Vec<RuleGroup>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct IntGridValue {
    pub value: i64,
    pub identifier: Option<String>,
    pub color: String,
    pub tile: Option<TileRect>,
    pub group_uid: i64,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct IntGridGroup {
    pub uid: i64,
    pub identifier: Option<String>,
    pub color: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RuleGroup {
    pub uid: i64,
    pub name: String,
    pub color: Option<String>,
    pub icon: Option<TileRect>,
    pub active: bool,
    pub is_optional: bool,
    pub required_biome_values: Vec<String>,
    pub biome_requirement_mode: i64,
    pub rules: Vec<Rule>,
}

#[derive(Debug, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Rule {
    pub uid: i64,
    pub active: bool,
    pub size: usize,
    pub tile_rects_ids: Vec<Vec<u32>>,
    pub alpha: f64,
    pub chance: f64,
    pub break_on_match: bool,
    pub pattern: Vec<i64>,
    pub flip_x: bool,
    pub flip_y: bool,
    pub x_modulo: i64,
    pub y_modulo: i64,
    pub x_offset: i32,
    pub y_offset: i32,
    pub tile_x_offset: i32,
    pub tile_y_offset: i32,
    pub tile_random_x_min: i32,
    pub tile_random_x_max: i32,
    pub tile_random_y_min: i32,
    pub tile_random_y_max: i32,
    pub checker: String,
    pub tile_mode: String,
    pub pivot_x: f64,
    pub pivot_y: f64,
    pub out_of_bounds_value: Option<i64>,
    pub perlin_active: bool,
    pub perlin_seed: f64,
    pub perlin_scale: f64,
    pub perlin_octaves: f64,
}

impl Default for Rule {
    fn default() -> Self {
        Rule {
            uid: 0,
            active: true,
            size: 1,
            tile_rects_ids: Vec::new(),
            alpha: 1.0,
            chance: 1.0,
            break_on_match: false,
            pattern: Vec::new(),
            flip_x: false,
            flip_y: false,
            x_modulo: 1,
            y_modulo: 1,
            x_offset: 0,
            y_offset: 0,
            tile_x_offset: 0,
            tile_y_offset: 0,
            tile_random_x_min: 0,
            tile_random_x_max: 0,
            tile_random_y_min: 0,
            tile_random_y_max: 0,
            checker: "None".into(),
            tile_mode: "Single".into(),
            pivot_x: 0.0,
            pivot_y: 0.0,
            out_of_bounds_value: None,
            perlin_active: false,
            perlin_seed: 0.0,
            perlin_scale: 0.2,
            perlin_octaves: 2.0,
        }
    }
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TileRect {
    pub tileset_uid: i64,
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Tileset {
    #[serde(rename = "__cWid")]
    pub c_wid: Option<u32>,
    pub identifier: String,
    pub uid: i64,
    pub rel_path: Option<String>,
    pub embed_atlas: Option<String>,
    pub px_wid: u32,
    pub px_hei: u32,
    pub tile_grid_size: u32,
    pub spacing: u32,
    pub padding: u32,
    pub tags: Vec<String>,
    pub tags_source_enum_uid: Option<i64>,
    pub enum_tags: Vec<Value>,
    pub custom_data: Vec<Value>,
    pub cached_pixel_data: Option<CachedPixelData>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CachedPixelData {
    pub opaque_tiles: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct EnumDef {
    pub identifier: String,
    pub uid: i64,
    pub values: Vec<EnumValue>,
    pub icon_tileset_uid: Option<i64>,
    pub external_rel_path: Option<String>,
    pub tags: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct EnumValue {
    pub id: String,
    pub tile_rect: Option<TileRect>,
    pub color: i64,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct EntityDef {
    pub identifier: String,
    pub uid: i64,
    pub tags: Vec<String>,
    pub doc: Option<String>,
    pub width: u32,
    pub height: u32,
    pub resizable_x: bool,
    pub resizable_y: bool,
    pub min_width: Option<f64>,
    pub max_width: Option<f64>,
    pub min_height: Option<f64>,
    pub max_height: Option<f64>,
    pub keep_aspect_ratio: bool,
    pub allow_out_of_bounds: bool,
    pub color: String,
    pub tileset_id: Option<i64>,
    pub tile_rect: Option<TileRect>,
    pub ui_tile_rect: Option<TileRect>,
    pub max_count: u32,
    pub limit_scope: String,
    pub limit_behavior: String,
    pub pivot_x: f64,
    pub pivot_y: f64,
    pub field_defs: Vec<FieldDef>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct FieldDef {
    pub identifier: String,
    pub doc: Option<String>,
    pub uid: i64,
    #[serde(rename = "type")]
    pub ty: String,
    pub is_array: bool,
    pub can_be_null: bool,
    pub array_min_length: Option<i64>,
    pub array_max_length: Option<i64>,
    pub editor_display_mode: String,
    pub editor_show_in_world: bool,
    pub min: Option<serde_json::Number>,
    pub max: Option<serde_json::Number>,
    pub regex: Option<String>,
    pub accept_file_types: Option<Vec<String>>,
    pub default_override: Option<EditorValue>,
    pub symmetrical_ref: bool,
    pub allow_out_of_level_ref: bool,
    pub allowed_refs: String,
    pub allowed_refs_entity_uid: Option<i64>,
    pub allowed_ref_tags: Vec<String>,
    pub tileset_uid: Option<i64>,
}

/// `{ "id": "V_Int", "params": [3] }`
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct EditorValue {
    pub id: String,
    pub params: Vec<Value>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct World {
    pub identifier: String,
    pub world_layout: Option<String>,
    pub levels: Vec<Level>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Level {
    pub identifier: String,
    pub iid: String,
    pub world_x: i64,
    pub world_y: i64,
    pub world_depth: i32,
    pub px_wid: u32,
    pub px_hei: u32,
    pub bg_color: Option<String>,
    pub bg_rel_path: Option<String>,
    pub external_rel_path: Option<String>,
    pub field_instances: Vec<FieldInstance>,
    pub layer_instances: Option<Vec<LayerInstance>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct FieldInstance {
    pub def_uid: i64,
    pub real_editor_values: Vec<Value>,
    /// The resolved value (defaults applied), in LDtk's export shape.
    #[serde(rename = "__value")]
    pub value: Value,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct LayerInstance {
    pub layer_def_uid: i64,
    pub px_offset_x: i32,
    pub px_offset_y: i32,
    pub optional_rules: Vec<i64>,
    pub int_grid_csv: Vec<i64>,
    pub auto_layer_tiles: Vec<Tile>,
    pub seed: i64,
    pub override_tileset_uid: Option<i64>,
    pub grid_tiles: Vec<Tile>,
    pub entity_instances: Vec<EntityInstance>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Tile {
    pub px: [i32; 2],
    pub t: u32,
    pub f: u8,
    pub d: Vec<i64>,
    pub a: Option<f64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct EntityInstance {
    pub iid: String,
    pub def_uid: i64,
    pub px: [i32; 2],
    pub width: u32,
    pub height: u32,
    pub field_instances: Vec<FieldInstance>,
}
