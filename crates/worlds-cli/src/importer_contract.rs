//! The `godot-rooms` importer contract (`check-export --profile
//! godot-rooms`): what a Godot importer that turns each level into a room
//! scene refuses in an export that the export format itself allows.
//! `docs/format.md` ("The godot-rooms profile") is the statement of these
//! rules; `tests/fixtures/importer_contract/` holds a case for each, so an
//! importer can run the same cases and reach the same verdicts.
//!
//! These are that importer's conventions, not Worlds': they live here, in
//! the CLI, and nowhere in `worlds-core`.

use std::collections::HashSet;

use serde_json::Value;

use crate::check_export::{Kind, LevelData, Report};

/// The auto layer that becomes the room's `Skin` TileMapLayer.
pub const SKIN_LAYER: &str = "main_tiling";
/// The room cell size in pixels; the skin is one tile of this size per cell.
pub const GRID: i64 = 32;
/// Prefixes a `tiles` layer's slug must have (`DecorBack*`, z −2, and
/// `DecorFront*`, z 2).
pub const DECOR_PREFIXES: [&str; 2] = ["decor_back", "decor_front"];
/// Node names a layer's TileMapLayer may not take: the room scene's own
/// children and the nodes the room builds at runtime, plus `Skin` for any
/// layer but the skin layer. Names starting with `_` are reserved too
/// (editor previews, sub-layers), and so is the empty name.
pub const RESERVED: [&str; 14] = [
    "Skin",
    "Collision",
    "Entries",
    "Exits",
    "Objects",
    "Enemies",
    "Props",
    "SkinLayer",
    "GreyboxLayer",
    "Backdrop",
    "Darkness",
    "Breakables",
    "SeamAprons",
    "_SkinPreview",
];
/// The node the skin layer becomes.
pub const SKIN_NODE: &str = "Skin";
/// The `blocking_values` the importer treats as walls; with others it
/// infers seams from the scenes instead.
pub const WALLS: [&str; 5] = ["solid", "fortified", "breakable", "ramp_r", "ramp_l"];

/// The node name the importer gives a layer: its display name with the
/// characters Godot doesn't allow in a node name (`. : @ / " %`) replaced by
/// `_` (`String.validate_node_name()`).
pub fn node_name(display: &str) -> String {
    display
        .chars()
        .map(|c| if ".:@/\"%".contains(c) { '_' } else { c })
        .collect()
}

/// The project-wide prerequisites: the 32 px grid and a collision layer
/// (the importer refuses the whole export without them), and the walls
/// [`WALLS`] as `blocking_values` (else seams come from the scenes: a warning).
pub fn check_project(world: &Value, report: &mut Report) {
    let file = "world.json";
    if let Some(g) = world.get("grid_size").and_then(Value::as_i64)
        && g > 0
        && g != GRID
    {
        report.problem(
            Kind::Importer,
            file,
            "grid_size",
            format!("project grid is {g} px; the godot-rooms profile needs {GRID} px rooms"),
        );
    }
    match world.get("collision_layer") {
        Some(Value::String(s)) if !s.is_empty() => {
            let given: HashSet<&str> = world
                .get("blocking_values")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            let walls: HashSet<&str> = WALLS.into_iter().collect();
            if given != walls {
                report.warn_kind(
                    Kind::Importer,
                    file,
                    "blocking_values",
                    format!(
                        "blocking_values aren't the importer's walls {WALLS:?}; the importer infers \
                         seams from the scenes' Collision instead"
                    ),
                );
            }
        }
        // Not a string is a format problem already.
        Some(v) if !v.is_string() => {}
        _ => {
            report.problem(
                Kind::Importer,
                file,
                "collision_layer",
                "no collision_layer; the godot-rooms profile needs one (set project.json \
                 `collision_layer` and re-bake)",
            );
        }
    }
}

/// A layer the importer places: its slug, node and z.
struct Placed<'a> {
    slug: &'a str,
    node: String,
    z: i32,
}

/// The per-level rules: which node each layer becomes (tiles layers named
/// `decor_back*`/`decor_front*`; an empty tiles layer of another name is
/// skipped, a non-empty one refused), reserved, empty and colliding node
/// names, skin exactness, transposed non-square tiles. `order` collects the
/// layer-order warnings (pairs Worlds stacks one way and the importer, by z,
/// the other), one per distinct pair across levels.
pub fn check_level(level: &LevelData, report: &mut Report, order: &mut Vec<(String, String)>) {
    let file = level.file.as_str();
    let skin_at = level
        .tile_layers
        .iter()
        .rposition(|l| l.kind == "auto" && l.slug == SKIN_LAYER);
    let mut placed: Vec<Placed> = Vec::new();
    for (i, layer) in level.tile_layers.iter().enumerate() {
        let p = format!("tile_layers[{}]", layer.slug);
        let has_tiles = layer.runs.iter().any(|r| !r.tiles.is_empty());
        let skin = layer.kind == "auto" && layer.slug == SKIN_LAYER;
        let before_skin = skin_at.is_none_or(|s| i < s);
        let (node, z) = match layer.kind.as_str() {
            "auto" if skin => (SKIN_NODE.to_string(), -1),
            "auto" => (node_name(&layer.name), if before_skin { -1 } else { 0 }),
            "tiles" if DECOR_PREFIXES.iter().any(|d| layer.slug.starts_with(d)) => {
                let back = layer.slug.starts_with(DECOR_PREFIXES[0]);
                (node_name(&layer.name), if back { -2 } else { 2 })
            }
            "tiles" => {
                // Empty, it has nothing to place: skipped.
                if has_tiles {
                    report.problem(
                        Kind::Importer,
                        file,
                        p.clone(),
                        format!(
                            "level {}: tiles layer {} isn't named decor_back* or decor_front*; \
                             the godot-rooms profile has no place for its tiles",
                            level.slug, layer.slug
                        ),
                    );
                }
                continue;
            }
            // Another kind is a format problem already.
            _ => continue,
        };
        if !skin {
            let why = if node.is_empty() {
                Some("an empty node name".to_string())
            } else if RESERVED.contains(&node.as_str()) || node.starts_with('_') {
                Some(format!(
                    "the node {node:?}, a reserved name in the room scene"
                ))
            } else {
                None
            };
            if let Some(why) = why {
                report.problem(
                    Kind::Importer,
                    file,
                    format!("{p}.name"),
                    format!(
                        "level {}: layer {} would be {why}; rename the layer",
                        level.slug, layer.slug
                    ),
                );
                continue;
            }
        }
        if let Some(other) = placed.iter().find(|o| o.node == node) {
            report.problem(
                Kind::Importer,
                file,
                format!("{p}.name"),
                format!(
                    "level {}: layers {} and {} would both be the node {node:?}; give them \
                     different names",
                    level.slug, other.slug, layer.slug
                ),
            );
            continue;
        }
        placed.push(Placed {
            slug: &layer.slug,
            node,
            z,
        });
        // Transposed non-square tiles: Worlds only turns square tiles.
        for (ri, run) in layer.runs.iter().enumerate() {
            let [tw, th] = run.tile_size;
            if tw == th {
                continue;
            }
            if let Some(ti) = run.tiles.iter().position(|t| t[4] & 4 != 0) {
                let n = run.tiles.iter().filter(|t| t[4] & 4 != 0).count();
                report.problem(
                    Kind::Importer,
                    file,
                    format!("{p}.tilesets[{ri}].tiles[{ti}]"),
                    format!(
                        "level {}: layer {}: {n} transposed {tw}x{th} tile(s); only square tiles \
                         can be transposed",
                        level.slug, layer.slug
                    ),
                );
            }
        }
        if skin {
            check_skin(level, layer, &p, report);
        }
    }
    // Export order is draw order (later on top); the importer draws by z.
    for (i, a) in placed.iter().enumerate() {
        for b in &placed[i + 1..] {
            if a.z > b.z {
                let msg = format!(
                    "Worlds draws {} above {}, but the importer draws {} (z {}) above {} (z {}); \
                     reorder the layers in Worlds (layers.json) so both agree",
                    b.slug, a.slug, a.node, a.z, b.node, b.z
                );
                if !order.iter().any(|(_, m)| *m == msg) {
                    order.push((file.to_string(), msg));
                }
            }
        }
    }
}

/// The skin layer: one 32x32 tile per cell, on the 32 px grid (the room's
/// skin is copied cell for cell).
fn check_skin(
    level: &LevelData,
    layer: &crate::check_export::TileLayer,
    p: &str,
    report: &mut Report,
) {
    let file = level.file.as_str();
    let mut first_size = None;
    let mut first_off = None;
    let mut first_stack = None;
    let (mut sized, mut off, mut stacked) = (0, 0, 0);
    let mut cells = HashSet::new();
    for (ri, run) in layer.runs.iter().enumerate() {
        for (ti, t) in run.tiles.iter().enumerate() {
            let at = format!("{p}.tilesets[{ri}].tiles[{ti}]");
            if run.tile_size != [GRID, GRID] {
                sized += 1;
                first_size.get_or_insert(at.clone());
            }
            if t[0].rem_euclid(GRID) != 0 || t[1].rem_euclid(GRID) != 0 {
                off += 1;
                first_off.get_or_insert(at.clone());
            } else if !cells.insert([t[0], t[1]]) {
                stacked += 1;
                first_stack.get_or_insert(at);
            }
        }
    }
    let rule = "the skin must be one 32x32 tile per cell, on the 32 px grid";
    let l = (&level.slug, &layer.slug);
    if let Some(at) = first_off {
        report.problem(
            Kind::Importer,
            file,
            at,
            format!(
                "level {}: {}: {off} tile(s) off the 32 px grid; {rule}",
                l.0, l.1
            ),
        );
    }
    if let Some(at) = first_size {
        report.problem(
            Kind::Importer,
            file,
            at,
            format!(
                "level {}: {}: {sized} tile(s) not 32x32 px; {rule}",
                l.0, l.1
            ),
        );
    }
    if let Some(at) = first_stack {
        report.problem(
            Kind::Importer,
            file,
            at,
            format!(
                "level {}: {}: {stacked} tile(s) share a cell with another; {rule}",
                l.0, l.1
            ),
        );
    }
}
