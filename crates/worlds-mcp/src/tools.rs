//! The tools of `docs/mcp.md`, each a method of [`Engine`] mapping its
//! arguments onto API calls (the "Tools" table there).

use std::collections::BTreeMap;

use rmcp::model::ContentBlock;
use serde::{Deserialize, Deserializer};
use serde_json::{Map, Value, json};

use crate::backend::Mode;
use crate::engine::{Engine, ToolResult, args, json_block};

/// `Some(v)` when the key is present (`v` possibly `None` for `null`),
/// `None` when it is absent.
fn present<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Some(Option::deserialize(d)?))
}

/// Put `v` under `k` when it is `Some`.
fn put<T: serde::Serialize>(m: &mut Value, k: &str, v: &Option<T>) {
    if let Some(v) = v {
        m[k] = json!(v);
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoArgs {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadLevel {
    level: String,
    layers: Option<Vec<String>>,
    #[serde(default)]
    format: Format,
    #[serde(default)]
    include_done_notes: bool,
}

#[derive(Deserialize, Default, PartialEq)]
#[serde(rename_all = "lowercase")]
enum Format {
    #[default]
    Text,
    Json,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateLevel {
    slug: String,
    name: String,
    size: [u32; 2],
    world_px: [i64; 2],
    depth: Option<i32>,
    fields: Option<Map<String, Value>>,
    tags: Option<Vec<String>>,
    world: Option<String>,
    content: Option<Map<String, Value>>,
    #[serde(default)]
    replace: bool,
    #[serde(default)]
    confirm: bool,
    label: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TextBlock {
    text: String,
    origin: [u32; 2],
    legend: Option<BTreeMap<String, Option<String>>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Line {
    from: [u32; 2],
    to: [u32; 2],
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Paint {
    level: String,
    layer: String,
    text_block: Option<TextBlock>,
    rect: Option<[u32; 4]>,
    outline: Option<bool>,
    line: Option<Line>,
    cells: Option<Vec<(u32, u32, Option<String>)>>,
    flood_at: Option<[u32; 2]>,
    #[serde(default, deserialize_with = "present")]
    value: Option<Option<String>>,
    label: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Stamp {
    at: [u32; 2],
    rect: [u32; 4],
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PlaceTiles {
    level: String,
    layer: String,
    tileset: Option<String>,
    placements: Option<Vec<Vec<Value>>>,
    stamp: Option<Stamp>,
    label: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClearTiles {
    level: String,
    layer: String,
    rect: [u32; 4],
    label: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PlaceEntity {
    level: String,
    #[serde(rename = "type")]
    ty: String,
    cell: [i64; 2],
    layer: Option<String>,
    offset_px: Option<[i64; 2]>,
    fields: Option<Map<String, Value>>,
    label: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EntityPatch {
    cell: Option<[i64; 2]>,
    offset_px: Option<[i64; 2]>,
    #[serde(default, deserialize_with = "present")]
    size: Option<Option<[u32; 2]>>,
    fields: Option<Map<String, Value>>,
    unset_fields: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateEntity {
    id: String,
    patch: EntityPatch,
    label: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoveEntity {
    ids: Vec<String>,
    label: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FindEntities {
    #[serde(rename = "type")]
    ty: Option<String>,
    tag: Option<String>,
    fields: Option<Map<String, Value>>,
    level: Option<String>,
    rect: Option<[u32; 4]>,
}

#[derive(Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct Scope {
    #[serde(skip_serializing_if = "Option::is_none")]
    level: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    world: Option<String>,
}

impl Scope {
    fn check(&self) -> Result<(), String> {
        match (&self.level, &self.world) {
            (Some(_), None) | (None, Some(_)) => Ok(()),
            _ => Err(
                "scope must be exactly one of { \"level\": \"<slug>\" } or { \"world\": \"<slug>\" }"
                    .into(),
            ),
        }
    }

    fn name(&self) -> String {
        match (&self.level, &self.world) {
            (Some(l), _) => l.clone(),
            (_, Some(w)) => format!("world {w}"),
            _ => String::new(),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Geometry {
    px: Option<[i64; 2]>,
    points: Option<Vec<[i64; 2]>>,
    rect: Option<[i64; 4]>,
    from: Option<Value>,
    to: Option<Value>,
    waypoints: Option<Vec<[i64; 2]>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AddNote {
    scope: Scope,
    kind: String,
    geometry: Geometry,
    text: String,
    tags: Option<Vec<String>>,
    color: Option<String>,
    links: Option<Vec<String>>,
    export: Option<bool>,
    status: Option<String>,
    icon: Option<String>,
    pin_label: Option<String>,
    closed: Option<bool>,
    group: Option<String>,
    label: Option<String>,
    connector_label: Option<String>,
    style: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListNotes {
    scope: Option<Scope>,
    tag: Option<String>,
    kind: Option<String>,
    status: Option<String>,
    group: Option<String>,
    export: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResolveNote {
    id: String,
    status: Option<String>,
    label: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NotePatch {
    text: Option<String>,
    #[serde(default, deserialize_with = "present")]
    title: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    body: Option<Option<String>>,
    tags: Option<Vec<String>>,
    #[serde(default, deserialize_with = "present")]
    color: Option<Option<String>>,
    links: Option<Vec<String>>,
    status: Option<String>,
    export: Option<bool>,
    locked: Option<bool>,
    #[serde(default, deserialize_with = "present")]
    icon: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    pin_label: Option<Option<String>>,
    closed: Option<bool>,
    #[serde(default, deserialize_with = "present")]
    group: Option<Option<String>>,
    geometry: Option<Geometry>,
    #[serde(rename = "move")]
    move_by: Option<[i64; 2]>,
    #[serde(default, deserialize_with = "present")]
    connector_label: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    style: Option<Option<String>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateNote {
    id: String,
    patch: NotePatch,
    #[serde(default)]
    confirm: bool,
    label: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoveNote {
    ids: Vec<String>,
    #[serde(default)]
    confirm: bool,
    label: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetGuideGroup {
    slug: String,
    name: Option<String>,
    offset: Option<[i64; 2]>,
    color: Option<String>,
    #[serde(default)]
    remove: bool,
    world: Option<String>,
    label: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TagLevels {
    levels: BTreeMap<String, Vec<String>>,
    label: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResizeLevel {
    level: String,
    size: [u32; 2],
    anchor: Option<[i8; 2]>,
    world: Option<String>,
    label: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CropLevel {
    level: String,
    world: Option<String>,
    label: Option<String>,
}

#[derive(Deserialize, Clone)]
#[serde(deny_unknown_fields)]
struct SetRule {
    group: String,
    rule: Value,
    index: Option<usize>,
}

#[derive(Deserialize, Clone)]
#[serde(deny_unknown_fields)]
struct MoveRule {
    rule: String,
    group: String,
    index: usize,
}

#[derive(Deserialize, Clone)]
#[serde(deny_unknown_fields)]
struct SetMacro {
    kind: String,
    name: String,
    def: Value,
}

#[derive(Deserialize, Clone)]
#[serde(deny_unknown_fields)]
struct RemoveMacro {
    kind: String,
    name: String,
}

#[derive(Deserialize, Clone)]
#[serde(deny_unknown_fields)]
struct SetStamp {
    stamp: Value,
    index: Option<usize>,
}

#[derive(Deserialize, Clone)]
#[serde(deny_unknown_fields)]
struct SetLevelState {
    level: String,
    #[serde(default, deserialize_with = "present")]
    state: Option<Option<Value>>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RulesPatch {
    #[serde(default)]
    set_macros: Vec<SetMacro>,
    #[serde(default)]
    set_stamps: Vec<SetStamp>,
    #[serde(default)]
    set_groups: Vec<Value>,
    #[serde(default)]
    set_rules: Vec<SetRule>,
    #[serde(default)]
    move_rules: Vec<MoveRule>,
    #[serde(default)]
    set_level_states: Vec<SetLevelState>,
    #[serde(default)]
    remove_rules: Vec<String>,
    #[serde(default)]
    remove_groups: Vec<String>,
    #[serde(default)]
    remove_stamps: Vec<String>,
    #[serde(default)]
    remove_macros: Vec<RemoveMacro>,
}

impl RulesPatch {
    fn is_empty(&self) -> bool {
        self.set_macros.is_empty()
            && self.set_stamps.is_empty()
            && self.set_groups.is_empty()
            && self.set_rules.is_empty()
            && self.move_rules.is_empty()
            && self.set_level_states.is_empty()
            && self.remove_rules.is_empty()
            && self.remove_groups.is_empty()
            && self.remove_stamps.is_empty()
            && self.remove_macros.is_empty()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EditRules {
    layer: String,
    patch: RulesPatch,
    label: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Rename {
    from: String,
    to: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RenameEnumValue {
    #[serde(rename = "enum")]
    enum_: String,
    from: String,
    to: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RenameEntityField {
    entity: String,
    from: String,
    to: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetEntityField {
    entity: String,
    def: Map<String, Value>,
    index: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetLevelField {
    def: Map<String, Value>,
    index: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetLayer {
    def: Map<String, Value>,
    index: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoveEntityField {
    entity: String,
    field: String,
}

/// `edit_definitions` `set_project`: the project settings to change.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ProjectPatch {
    name: Option<String>,
    #[serde(default, deserialize_with = "present")]
    bg_color: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    collision_layer: Option<Option<String>>,
    seed: Option<i64>,
    grid_size: Option<u32>,
    #[serde(default, deserialize_with = "present")]
    blocking_values: Option<Option<Vec<String>>>,
    #[serde(default, deserialize_with = "present")]
    default_level_size: Option<Option<[u32; 2]>>,
    export_on_save: Option<bool>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct DefsPatch {
    #[serde(default)]
    rename_entity_types: Vec<Rename>,
    #[serde(default)]
    rename_enums: Vec<Rename>,
    #[serde(default)]
    rename_enum_values: Vec<RenameEnumValue>,
    #[serde(default)]
    rename_entity_fields: Vec<RenameEntityField>,
    #[serde(default)]
    rename_level_fields: Vec<Rename>,
    #[serde(default)]
    rename_assemblies: Vec<Rename>,
    #[serde(default)]
    set_layers: Vec<SetLayer>,
    #[serde(default)]
    set_enums: Vec<Map<String, Value>>,
    #[serde(default)]
    set_entity_types: Vec<Map<String, Value>>,
    #[serde(default)]
    set_entity_fields: Vec<SetEntityField>,
    #[serde(default)]
    set_level_fields: Vec<SetLevelField>,
    #[serde(default)]
    set_assemblies: Vec<Map<String, Value>>,
    set_project: Option<ProjectPatch>,
    #[serde(default)]
    remove_entity_fields: Vec<RemoveEntityField>,
    #[serde(default)]
    remove_level_fields: Vec<String>,
    #[serde(default)]
    remove_enums: Vec<String>,
    #[serde(default)]
    remove_entity_types: Vec<String>,
    #[serde(default)]
    remove_layers: Vec<String>,
    #[serde(default)]
    remove_assemblies: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EditDefinitions {
    patch: DefsPatch,
    #[serde(default)]
    confirm: bool,
    label: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PreviewRule {
    layer: String,
    rule: String,
    level: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FindRegions {
    level: String,
    cells: String,
    grid: Option<String>,
    along: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExpandRules {
    layer: String,
    group: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BakeAndValidate {
    levels: Option<Vec<String>>,
    #[serde(default)]
    export: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Screenshot {
    pub level: Option<String>,
    pub world_rect: Option<[i64; 4]>,
    pub layers: Option<Vec<String>>,
    pub size: Option<[u32; 2]>,
    /// The editor's map tab: alone, what it shows; with world_rect, the
    /// rect in its world.
    pub viewport: Option<u32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Undo {
    n: Option<u32>,
}

/// The border of `rect` as up to four rects (top, bottom, left, right
/// sides), each sent as a `grid.fill_rect` the server bounds-checks: nothing
/// is expanded cell by cell here, so a huge rect costs nothing.
fn outline_rects(rect: [u32; 4]) -> Result<Vec<[u32; 4]>, String> {
    let [x, y, w, h] = rect;
    let (Some(right), Some(bottom)) = (x.checked_add(w), y.checked_add(h)) else {
        return Err(format!(
            "rect {rect:?} reaches past the largest coordinate; give [x, y, width, height] inside the level"
        ));
    };
    if w <= 2 || h <= 2 {
        // Every cell is on the border.
        return Ok(vec![rect]);
    }
    Ok(vec![
        [x, y, w, 1],
        [x, bottom - 1, w, 1],
        [x, y + 1, 1, h - 2],
        [right - 1, y + 1, 1, h - 2],
    ])
}

/// One `place_tiles` placement: `[x, y, col, row]` or with a flip.
fn placement(p: &[Value], tileset: &str) -> Result<Value, String> {
    let bad = || {
        format!(
            "placement {} must be [x, y, col, row] or [x, y, col, row, flip] with non-negative integers and flip \"x\", \"y\", \"xy\", \"t\", \"xt\", \"yt\" or \"xyt\"",
            Value::Array(p.to_vec())
        )
    };
    if !(4..=5).contains(&p.len()) {
        return Err(bad());
    }
    let n: Vec<u64> = p[..4]
        .iter()
        .map(|v| v.as_u64().ok_or_else(bad))
        .collect::<Result<_, _>>()?;
    let mut t = json!({ "cell": [n[0], n[1]], "tileset": tileset, "tile": [n[2], n[3]] });
    if let Some(f) = p.get(4) {
        match f.as_str() {
            Some(s @ ("x" | "y" | "xy" | "t" | "xt" | "yt" | "xyt")) => t["flip"] = json!(s),
            _ => return Err(bad()),
        }
    }
    Ok(t)
}

/// The API writes of an `edit_rules` patch (`docs/mcp.md`), in its order:
/// macros, stamps, groups, rules, moves, then the removals (rules, groups,
/// stamps, macros: what refers to something goes before it).
fn plan_rules(layer: &str, pt: &RulesPatch) -> Result<Vec<(&'static str, Value)>, String> {
    let mut w: Vec<(&'static str, Value)> = Vec::new();
    for m in &pt.set_macros {
        w.push((
            "rules.set_macro",
            json!({ "kind": m.kind, "name": m.name, "def": m.def }),
        ));
    }
    for st in &pt.set_stamps {
        let slug = st.stamp["slug"]
            .as_str()
            .ok_or("every set_stamps stamp needs a slug")?;
        let mut p = json!({ "layer": layer, "stamp": slug, "def": st.stamp });
        put(&mut p, "index", &st.index);
        w.push(("rules.set_stamp", p));
    }
    for g in &pt.set_groups {
        w.push(("rules.set_group", json!({ "layer": layer, "group": g })));
    }
    for sr in &pt.set_rules {
        let mut p = json!({ "layer": layer, "group": sr.group, "rule": sr.rule });
        put(&mut p, "index", &sr.index);
        w.push(("rules.set_rule", p));
    }
    for m in &pt.move_rules {
        w.push((
            "rules.move_rule",
            json!({ "layer": layer, "rule": m.rule, "group": m.group, "index": m.index }),
        ));
    }
    for s in &pt.set_level_states {
        let Some(state) = &s.state else {
            return Err(format!(
                "set_level_states entry for {} needs state: the rule state, or null to remove it",
                s.level
            ));
        };
        w.push((
            "level.set_layer_state",
            json!({ "level": s.level, "layer": layer, "state": state }),
        ));
    }
    for r in &pt.remove_rules {
        w.push(("rules.remove_rule", json!({ "layer": layer, "rule": r })));
    }
    for g in &pt.remove_groups {
        w.push(("rules.remove_group", json!({ "layer": layer, "group": g })));
    }
    for st in &pt.remove_stamps {
        w.push(("rules.remove_stamp", json!({ "layer": layer, "stamp": st })));
    }
    for m in &pt.remove_macros {
        w.push((
            "rules.remove_macro",
            json!({ "kind": m.kind, "name": m.name }),
        ));
    }
    Ok(w)
}

/// One API write: method and params.
type ApiWrite = (&'static str, Value);

/// The `slug` of a definition in a patch list, for the summary and a clear
/// error when it is missing.
fn def_slug<'a>(def: &'a Map<String, Value>, list: &str) -> Result<&'a str, String> {
    def.get("slug")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("every {list} entry needs a slug"))
}

/// The API writes of an `edit_definitions` patch (`docs/mcp.md`), in its
/// order: renames (so the sets can use the new names), sets, then removals.
/// `confirm` goes on every `defs.set` and `defs.remove` only when the call
/// has it. Also returns the summary for the undo label: what it touches, in
/// order, each named once.
fn plan_defs(pt: &DefsPatch, confirm: bool) -> Result<(Vec<ApiWrite>, String), String> {
    let mut w: Vec<ApiWrite> = Vec::new();
    let mut touched: Vec<String> = Vec::new();
    let mut touch = |s: String| {
        if !touched.contains(&s) {
            touched.push(s);
        }
    };
    let mut push = |method: &'static str, mut p: Value| {
        if confirm && (method == "defs.set" || method == "defs.remove") {
            p["confirm"] = json!(true);
        }
        w.push((method, p));
    };
    for r in &pt.rename_entity_types {
        touch(r.to.clone());
        push(
            "defs.rename",
            json!({ "kind": "entity", "from": r.from, "to": r.to }),
        );
    }
    for r in &pt.rename_enums {
        touch(r.to.clone());
        push(
            "defs.rename",
            json!({ "kind": "enum", "from": r.from, "to": r.to }),
        );
    }
    for r in &pt.rename_enum_values {
        touch(r.enum_.clone());
        push(
            "defs.rename",
            json!({ "kind": "enum_value", "enum": r.enum_, "from": r.from, "to": r.to }),
        );
    }
    for r in &pt.rename_entity_fields {
        touch(r.entity.clone());
        push(
            "defs.rename",
            json!({ "kind": "entity_field", "entity": r.entity, "from": r.from, "to": r.to }),
        );
    }
    for r in &pt.rename_level_fields {
        touch("level fields".into());
        push(
            "defs.rename",
            json!({ "kind": "level_field", "from": r.from, "to": r.to }),
        );
    }
    for r in &pt.rename_assemblies {
        touch(r.to.clone());
        push(
            "defs.rename",
            json!({ "kind": "assembly", "from": r.from, "to": r.to }),
        );
    }
    for l in &pt.set_layers {
        touch(def_slug(&l.def, "set_layers def")?.to_string());
        let mut p = json!({ "kind": "layer", "def": l.def });
        put(&mut p, "index", &l.index);
        push("defs.set", p);
    }
    for d in &pt.set_enums {
        touch(def_slug(d, "set_enums")?.to_string());
        push("defs.set", json!({ "kind": "enum", "def": d }));
    }
    for d in &pt.set_entity_types {
        touch(def_slug(d, "set_entity_types")?.to_string());
        // The file as read from the resource carries these; the API's
        // definitions don't.
        let mut d = d.clone();
        d.remove("$schema");
        d.remove("format");
        push("defs.set", json!({ "kind": "entity", "def": d }));
    }
    for f in &pt.set_entity_fields {
        def_slug(&f.def, "set_entity_fields def")?;
        touch(f.entity.clone());
        let mut p = json!({ "kind": "entity_field", "entity": f.entity, "def": f.def });
        put(&mut p, "index", &f.index);
        push("defs.set", p);
    }
    for f in &pt.set_level_fields {
        def_slug(&f.def, "set_level_fields def")?;
        touch("level fields".into());
        let mut p = json!({ "kind": "level_field", "def": f.def });
        put(&mut p, "index", &f.index);
        push("defs.set", p);
    }
    for d in &pt.set_assemblies {
        touch(def_slug(d, "set_assemblies")?.to_string());
        let mut d = d.clone();
        d.remove("$schema");
        d.remove("format");
        push("defs.set", json!({ "kind": "assembly", "def": d }));
    }
    if let Some(s) = &pt.set_project {
        let mut p = json!({});
        put(&mut p, "name", &s.name);
        put(&mut p, "bg_color", &s.bg_color);
        put(&mut p, "collision_layer", &s.collision_layer);
        put(&mut p, "seed", &s.seed);
        put(&mut p, "grid_size", &s.grid_size);
        put(&mut p, "blocking_values", &s.blocking_values);
        put(&mut p, "default_level_size", &s.default_level_size);
        put(&mut p, "export_on_save", &s.export_on_save);
        if p.as_object().is_some_and(Map::is_empty) {
            return Err(
                "set_project is empty: give name, bg_color, collision_layer, seed, \
                 grid_size, blocking_values, default_level_size or export_on_save"
                    .into(),
            );
        }
        touch("project".into());
        push("project.update", p);
    }
    for f in &pt.remove_entity_fields {
        touch(f.entity.clone());
        push(
            "defs.remove",
            json!({ "kind": "entity_field", "entity": f.entity, "slug": f.field }),
        );
    }
    for f in &pt.remove_level_fields {
        touch("level fields".into());
        push("defs.remove", json!({ "kind": "level_field", "slug": f }));
    }
    for e in &pt.remove_enums {
        touch(e.clone());
        push("defs.remove", json!({ "kind": "enum", "slug": e }));
    }
    for t in &pt.remove_entity_types {
        touch(t.clone());
        push("defs.remove", json!({ "kind": "entity", "slug": t }));
    }
    for l in &pt.remove_layers {
        touch(l.clone());
        push("defs.remove", json!({ "kind": "layer", "slug": l }));
    }
    for a in &pt.remove_assemblies {
        touch(a.clone());
        push("defs.remove", json!({ "kind": "assembly", "slug": a }));
    }
    let mut summary = touched.join(", ");
    if summary.chars().count() > 60 {
        summary = summary.chars().take(57).collect::<String>() + "...";
    }
    Ok((w, summary))
}

/// A note's text: the first line is the title, the rest (if any) the
/// body, as `add_note` documents.
fn split_text(text: &str) -> (Option<String>, Option<String>) {
    let mut lines = text.splitn(2, '\n');
    let title = lines.next().unwrap_or("").trim().to_string();
    let body = lines
        .next()
        .map(|b| b.trim_matches('\n').to_string())
        .filter(|b| !b.trim().is_empty());
    ((!title.is_empty()).then_some(title), body)
}

/// The geometry keys given with their values: one of `px`, `points` or
/// `rect`, or a connector's `from`, `to` and `waypoints` (any of them).
fn geometry_keys(g: &Geometry) -> Result<Map<String, Value>, String> {
    let mut m = Map::new();
    let connector = [
        ("from", g.from.clone()),
        ("to", g.to.clone()),
        ("waypoints", g.waypoints.as_ref().map(|w| json!(w))),
    ];
    if connector.iter().any(|(_, v)| v.is_some()) {
        if g.px.is_some() || g.points.is_some() || g.rect.is_some() {
            return Err(
                "geometry: a connector's from, to and waypoints don't go with px, points or rect"
                    .into(),
            );
        }
        for (k, v) in connector {
            if let Some(v) = v {
                m.insert(k.into(), v);
            }
        }
        return Ok(m);
    }
    if let Some(px) = g.px {
        m.insert("px".into(), json!(px));
    }
    if let Some(points) = &g.points {
        m.insert("points".into(), json!(points));
    }
    if let Some(rect) = g.rect {
        m.insert("rect".into(), json!(rect));
    }
    if m.len() != 1 {
        return Err(
            "geometry needs exactly one of px, points or rect (a connector: from, to and waypoints)"
                .into(),
        );
    }
    Ok(m)
}

impl Engine {
    pub(crate) fn describe_project(&mut self, a: Value) -> ToolResult {
        let NoArgs {} = args("describe_project", a)?;
        let r = self.read("project.describe", json!({}))?;
        self.json_result(r)
    }

    pub(crate) fn read_level(&mut self, a: Value) -> ToolResult {
        let a: ReadLevel = args("read_level", a)?;
        let mut p = json!({
            "level": a.level,
            "format": if a.format == Format::Json { "json" } else { "text" },
            "notes": if a.include_done_notes { "all" } else { "open" },
        });
        put(&mut p, "layers", &a.layers);
        let r = self.read("level.get", p)?;
        let rev = r["rev"].as_u64().unwrap_or(self.rev);
        self.mark_read(&a.level, rev);
        if a.format == Format::Json {
            return self.json_result(r);
        }
        let mut text = r["text"].as_str().unwrap_or_default().to_string();
        let notices = self.pending_notices();
        if !notices.is_empty() {
            text.push_str("\nnotices:\n");
            for n in notices {
                text.push_str(&format!("- {n}\n"));
            }
        }
        Ok(vec![ContentBlock::text(text)])
    }

    pub(crate) fn create_level(&mut self, a: Value) -> ToolResult {
        let a: CreateLevel = args("create_level", a)?;
        if a.size.contains(&0) {
            return Err(format!(
                "size {:?} must be at least [1, 1] cells of the project grid",
                a.size
            ));
        }
        if a.confirm && !a.replace {
            return Err("confirm only goes with replace: true".into());
        }
        let label = Self::label("create_level", a.label.as_deref(), &a.slug);
        if !a.replace {
            let mut level = json!({ "slug": a.slug, "name": a.name, "size": a.size,
                "world_px": a.world_px, "depth": a.depth.unwrap_or(0) });
            put(&mut level, "fields", &a.fields);
            put(&mut level, "tags", &a.tags);
            let mut p = json!({ "level": level });
            put(&mut p, "world", &a.world);
            put(&mut p, "content", &a.content);
            let r = self.write(&label, |_| Ok(vec![("level.create", p.clone())]))?;
            return Ok(vec![json_block(&r)]);
        }
        let r = self.write(&label, |e| {
            // The level as it is: what this tool has no argument for (id,
            // background, rule state), and what the call leaves out, stays.
            let old = e.read(
                "level.get",
                json!({ "level": a.slug, "layers": [], "notes": "none" }),
            )?;
            let mut level = old["level"].clone();
            level["name"] = json!(a.name);
            level["size"] = json!(a.size);
            level["world_px"] = json!(a.world_px);
            put(&mut level, "depth", &a.depth);
            put(&mut level, "fields", &a.fields);
            put(&mut level, "tags", &a.tags);
            let mut p = json!({ "level": level });
            put(&mut p, "world", &a.world);
            put(&mut p, "content", &a.content);
            if a.confirm {
                p["confirm"] = json!(true);
            }
            Ok(vec![("level.replace", p)])
        })?;
        Ok(vec![json_block(&r)])
    }

    pub(crate) fn paint(&mut self, a: Value) -> ToolResult {
        let a: Paint = args("paint", a)?;
        let needs_value = a.rect.is_some() || a.line.is_some() || a.flood_at.is_some();
        if a.text_block.is_none() && !needs_value && a.cells.is_none() {
            return Err("paint needs at least one shape: text_block, rect + value, line + value, cells, or flood_at + value".into());
        }
        if needs_value && a.value.is_none() {
            return Err("rect, line and flood_at need value: give a value slug, or \"value\": null to make the cells empty".into());
        }
        if !needs_value && a.value.is_some() {
            return Err("value is only used with rect, line or flood_at; text_block and cells carry their own values".into());
        }
        if a.outline.is_some() && a.rect.is_none() {
            return Err("outline only goes with rect".into());
        }
        let value = a.value.clone().flatten();
        let (level, layer) = (a.level.clone(), a.layer.clone());
        let base = json!({ "level": level, "layer": layer });
        let mut writes: Vec<(&'static str, Value)> = Vec::new();
        if let Some(tb) = &a.text_block {
            let mut p = base.clone();
            p["text"] = json!(tb.text);
            p["origin"] = json!(tb.origin);
            put(&mut p, "legend", &tb.legend);
            writes.push(("grid.set_cells", p));
        }
        if let Some(rect) = a.rect {
            let mut p = base.clone();
            if a.outline == Some(true) {
                for r in outline_rects(rect)? {
                    let mut p = p.clone();
                    p["rect"] = json!(r);
                    p["value"] = json!(value);
                    writes.push(("grid.fill_rect", p));
                }
            } else {
                p["rect"] = json!(rect);
                p["value"] = json!(value);
                writes.push(("grid.fill_rect", p));
            }
        }
        if let Some(line) = &a.line {
            let mut p = base.clone();
            p["from"] = json!(line.from);
            p["to"] = json!(line.to);
            p["value"] = json!(value);
            writes.push(("grid.line", p));
        }
        if let Some(cells) = &a.cells {
            let mut p = base.clone();
            p["cells"] = json!(cells);
            writes.push(("grid.set_cells", p));
        }
        if let Some(at) = a.flood_at {
            let mut p = base.clone();
            p["at"] = json!(at);
            p["value"] = json!(value);
            writes.push(("grid.flood_fill", p));
        }
        let label = Self::label("paint", a.label.as_deref(), &format!("{layer} in {level}"));
        let r = self.write(&label, |_| Ok(writes.clone()))?;
        Ok(vec![json_block(&r)])
    }

    pub(crate) fn place_tiles(&mut self, a: Value) -> ToolResult {
        let a: PlaceTiles = args("place_tiles", a)?;
        if a.placements.is_none() && a.stamp.is_none() {
            return Err("place_tiles needs placements, stamp, or both".into());
        }
        let label = Self::label(
            "place_tiles",
            a.label.as_deref(),
            &format!("{} in {}", a.layer, a.level),
        );
        let r = self.write(&label, |e| {
            let tileset = match &a.tileset {
                Some(t) => t.clone(),
                None => e.layer_tileset(&a.layer)?,
            };
            let base = json!({ "level": a.level, "layer": a.layer });
            let mut writes = Vec::new();
            if let Some(ps) = &a.placements {
                let tiles: Vec<Value> = ps
                    .iter()
                    .map(|p| placement(p, &tileset))
                    .collect::<Result<_, _>>()?;
                let mut p = base.clone();
                p["tiles"] = json!(tiles);
                writes.push(("tiles.set", p));
            }
            if let Some(s) = &a.stamp {
                let mut p = base.clone();
                p["at"] = json!(s.at);
                p["rect"] = json!(s.rect);
                p["tileset"] = json!(tileset);
                writes.push(("tiles.stamp", p));
            }
            Ok(writes)
        })?;
        Ok(vec![json_block(&r)])
    }

    /// The tileset of a tiles layer (from its definition).
    fn layer_tileset(&mut self, layer: &str) -> Result<String, String> {
        let d = self.read("defs.get", json!({ "kind": "layer", "slug": layer }))?;
        let def = &d["def"];
        match def["tileset"].as_str() {
            Some(t) => Ok(t.to_string()),
            None => Err(format!(
                "layer {layer} is a {} layer, not a tiles layer with a tileset; give tileset or use a tiles layer",
                def["kind"].as_str().unwrap_or("different")
            )),
        }
    }

    pub(crate) fn clear_tiles(&mut self, a: Value) -> ToolResult {
        let a: ClearTiles = args("clear_tiles", a)?;
        let label = Self::label(
            "clear_tiles",
            a.label.as_deref(),
            &format!("{} in {}", a.layer, a.level),
        );
        let p = json!({ "level": a.level, "layer": a.layer, "rect": a.rect });
        let r = self.write(&label, |_| Ok(vec![("tiles.clear_rect", p.clone())]))?;
        Ok(vec![json_block(&r)])
    }

    pub(crate) fn place_entity(&mut self, a: Value) -> ToolResult {
        let a: PlaceEntity = args("place_entity", a)?;
        let label = Self::label(
            "place_entity",
            a.label.as_deref(),
            &format!("{} in {}", a.ty, a.level),
        );
        let mut entity = json!({ "type": a.ty, "cell": a.cell });
        put(&mut entity, "offset_px", &a.offset_px);
        put(&mut entity, "fields", &a.fields);
        let mut p = json!({ "level": a.level, "entity": entity });
        put(&mut p, "layer", &a.layer);
        let r = self.write(&label, |_| Ok(vec![("entity.add", p.clone())]))?;
        Ok(vec![json_block(&r)])
    }

    pub(crate) fn update_entity(&mut self, a: Value) -> ToolResult {
        let a: UpdateEntity = args("update_entity", a)?;
        let pt = &a.patch;
        let moves = pt.cell.is_some() || pt.offset_px.is_some();
        let updates = pt.fields.is_some() || pt.unset_fields.is_some() || pt.size.is_some();
        if !moves && !updates {
            return Err(
                "patch is empty: give cell, offset_px, size, fields or unset_fields".into(),
            );
        }
        let label = Self::label("update_entity", a.label.as_deref(), &a.id);
        let r = self.write(&label, |e| {
            let mut writes = Vec::new();
            if moves {
                let cell = match pt.cell {
                    Some(c) => json!(c),
                    // Only an offset: keep the entity's cell.
                    None => e.read("entity.get", json!({ "id": a.id }))?["entity"]["cell"].clone(),
                };
                let mut p = json!({ "id": a.id, "cell": cell });
                put(&mut p, "offset_px", &pt.offset_px);
                writes.push(("entity.move", p));
            }
            if updates {
                let mut p = json!({ "id": a.id });
                put(&mut p, "fields", &pt.fields);
                put(&mut p, "unset", &pt.unset_fields);
                if let Some(size) = &pt.size {
                    p["size"] = json!(size);
                }
                writes.push(("entity.update", p));
            }
            Ok(writes)
        })?;
        Ok(vec![json_block(&r)])
    }

    pub(crate) fn remove_entity(&mut self, a: Value) -> ToolResult {
        let a: RemoveEntity = args("remove_entity", a)?;
        if a.ids.is_empty() {
            return Err("ids is empty: give the ids of the entities to remove".into());
        }
        let summary = match a.ids.as_slice() {
            [one] => one.clone(),
            many => format!("{} entities", many.len()),
        };
        let label = Self::label("remove_entity", a.label.as_deref(), &summary);
        let p = json!({ "ids": a.ids });
        let r = self.write(&label, |_| Ok(vec![("entity.remove", p.clone())]))?;
        Ok(vec![json_block(&r)])
    }

    pub(crate) fn find_entities(&mut self, a: Value) -> ToolResult {
        let a: FindEntities = args("find_entities", a)?;
        let mut p = json!({});
        put(&mut p, "type", &a.ty);
        put(&mut p, "tag", &a.tag);
        put(&mut p, "fields", &a.fields);
        put(&mut p, "level", &a.level);
        put(&mut p, "rect", &a.rect);
        let r = self.read("entity.find", p)?;
        self.json_result(json!({ "rev": r["rev"], "entities": r["entities"] }))
    }

    pub(crate) fn add_note(&mut self, a: Value) -> ToolResult {
        let a: AddNote = args("add_note", a)?;
        a.scope.check()?;
        let (title, body) = split_text(&a.text);
        let mut note = json!({ "kind": a.kind });
        if let Some(t) = title {
            note["title"] = json!(t);
        }
        if let Some(b) = body {
            note["body"] = json!(b);
        }
        put(&mut note, "tags", &a.tags);
        put(&mut note, "color", &a.color);
        put(&mut note, "links", &a.links);
        put(&mut note, "export", &a.export);
        put(&mut note, "status", &a.status);
        let per_kind = [
            ("icon", a.icon.is_some(), "pin"),
            ("pin_label", a.pin_label.is_some(), "pin"),
            ("closed", a.closed.is_some(), "guide"),
            ("group", a.group.is_some(), "guide"),
            ("connector_label", a.connector_label.is_some(), "connector"),
            ("style", a.style.is_some(), "connector"),
        ];
        for (key, given, kind) in per_kind {
            if given && a.kind != kind {
                return Err(format!("{key} is only for a {kind}, not a {}", a.kind));
            }
        }
        put(&mut note, "icon", &a.icon);
        put(&mut note, "label", &a.pin_label);
        put(&mut note, "closed", &a.closed);
        put(&mut note, "group", &a.group);
        put(&mut note, "label", &a.connector_label);
        put(&mut note, "style", &a.style);
        for (k, v) in geometry_keys(&a.geometry)? {
            note[k.as_str()] = v;
        }
        let label = Self::label(
            "add_note",
            a.label.as_deref(),
            &format!("{} in {}", a.kind, a.scope.name()),
        );
        let p = json!({ "scope": a.scope, "note": note });
        let r = self.write(&label, |_| Ok(vec![("notes.add", p.clone())]))?;
        Ok(vec![json_block(&r)])
    }

    pub(crate) fn list_notes(&mut self, a: Value) -> ToolResult {
        let a: ListNotes = args("list_notes", a)?;
        if let Some(s) = &a.scope {
            s.check()?;
        }
        let mut p = json!({ "status": a.status.as_deref().unwrap_or("open") });
        put(&mut p, "scope", &a.scope);
        put(&mut p, "tag", &a.tag);
        put(&mut p, "kind", &a.kind);
        put(&mut p, "group", &a.group);
        put(&mut p, "export", &a.export);
        // Guides are reference lines, not work items: left out unless asked
        // for by kind or group.
        if a.kind.is_none() && a.group.is_none() {
            p["kinds"] = json!(["note", "pin", "path", "region", "connector"]);
        }
        let r = self.read("notes.list", p)?;
        self.json_result(json!({ "rev": r["rev"], "notes": r["notes"] }))
    }

    pub(crate) fn resolve_note(&mut self, a: Value) -> ToolResult {
        let a: ResolveNote = args("resolve_note", a)?;
        let label = Self::label("resolve_note", a.label.as_deref(), &a.id);
        let mut p = json!({ "id": a.id });
        put(&mut p, "status", &a.status);
        let r = self.write(&label, |_| Ok(vec![("notes.resolve", p.clone())]))?;
        Ok(vec![json_block(&r)])
    }

    pub(crate) fn update_note(&mut self, a: Value) -> ToolResult {
        let a: UpdateNote = args("update_note", a)?;
        let pt = &a.patch;
        if pt.text.is_some() && (pt.title.is_some() || pt.body.is_some()) {
            return Err("give text (title and body together) or title and body, not both".into());
        }
        if pt.geometry.is_some() && pt.move_by.is_some() {
            return Err("give geometry (the new shape) or move (an offset), not both".into());
        }
        let mut patch = json!({});
        if let Some(text) = &pt.text {
            let (title, body) = split_text(text);
            patch["title"] = json!(title);
            patch["body"] = json!(body);
        }
        put(&mut patch, "title", &pt.title);
        put(&mut patch, "body", &pt.body);
        put(&mut patch, "tags", &pt.tags);
        put(&mut patch, "color", &pt.color);
        put(&mut patch, "links", &pt.links);
        put(&mut patch, "status", &pt.status);
        put(&mut patch, "export", &pt.export);
        put(&mut patch, "locked", &pt.locked);
        put(&mut patch, "icon", &pt.icon);
        put(&mut patch, "label", &pt.pin_label);
        put(&mut patch, "closed", &pt.closed);
        put(&mut patch, "group", &pt.group);
        if pt.pin_label.is_some() && pt.connector_label.is_some() {
            return Err("give pin_label (a pin) or connector_label (a connector), not both".into());
        }
        put(&mut patch, "label", &pt.connector_label);
        put(&mut patch, "style", &pt.style);
        if let Some(g) = &pt.geometry {
            for (k, v) in geometry_keys(g)? {
                patch[k.as_str()] = v;
            }
            // A region is a rect or a polygon: the new one replaces the old.
            if g.rect.is_some() {
                patch["points"] = Value::Null;
            } else if g.points.is_some() {
                patch["rect"] = Value::Null;
            }
        }
        let has_patch = patch.as_object().is_some_and(|m| !m.is_empty());
        if !has_patch && pt.move_by.is_none() {
            return Err("patch is empty: give text, title, body, tags, color, links, status, export, locked, icon, pin_label, closed, group, connector_label, style, geometry or move".into());
        }
        let label = Self::label("update_note", a.label.as_deref(), &a.id);
        let mut writes = Vec::new();
        if has_patch {
            let mut p = json!({ "id": a.id, "patch": patch });
            if a.confirm {
                p["confirm"] = json!(true);
            }
            writes.push(("notes.update", p));
        }
        if let Some(d) = pt.move_by {
            let mut p = json!({ "ids": [a.id], "delta": d });
            if a.confirm {
                p["confirm"] = json!(true);
            }
            writes.push(("notes.move", p));
        }
        let r = self.write(&label, |_| Ok(writes.clone()))?;
        Ok(vec![json_block(&r)])
    }

    pub(crate) fn remove_note(&mut self, a: Value) -> ToolResult {
        let a: RemoveNote = args("remove_note", a)?;
        if a.ids.is_empty() {
            return Err("ids is empty: give the ids of the annotations to remove".into());
        }
        let summary = match a.ids.as_slice() {
            [one] => one.clone(),
            many => format!("{} notes", many.len()),
        };
        let label = Self::label("remove_note", a.label.as_deref(), &summary);
        let mut p = json!({ "ids": a.ids });
        if a.confirm {
            p["confirm"] = json!(true);
        }
        let r = self.write(&label, |_| Ok(vec![("notes.remove", p.clone())]))?;
        Ok(vec![json_block(&r)])
    }

    pub(crate) fn set_guide_group(&mut self, a: Value) -> ToolResult {
        let a: SetGuideGroup = args("set_guide_group", a)?;
        let label = Self::label("set_guide_group", a.label.as_deref(), &a.slug);
        if a.remove {
            if a.name.is_some() || a.offset.is_some() || a.color.is_some() {
                return Err(
                    "remove takes only slug (and world): leave out name, offset and color".into(),
                );
            }
            let mut p = json!({ "slug": a.slug });
            put(&mut p, "world", &a.world);
            let r = self.write(&label, |_| {
                Ok(vec![("notes.remove_guide_group", p.clone())])
            })?;
            return Ok(vec![json_block(&r)]);
        }
        let r = self.write(&label, |e| {
            // The group as it is, so what isn't given stays.
            let d = e.read("project.describe", json!({}))?;
            let worlds = d["worlds"].as_array().cloned().unwrap_or_default();
            let world = match &a.world {
                Some(w) => worlds
                    .iter()
                    .find(|x| x["slug"] == w.as_str())
                    .ok_or_else(|| format!("there is no world {w:?}"))?,
                None => match worlds.as_slice() {
                    [one] => one,
                    _ => return Err("the project has several worlds: give world".to_string()),
                },
            };
            let old = world["guide_groups"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|g| g["slug"] == a.slug.as_str());
            let mut group = json!({ "slug": a.slug });
            for key in ["name", "offset", "color"] {
                if let Some(v) = old.and_then(|g| g.get(key)) {
                    group[key] = v.clone();
                }
            }
            put(&mut group, "name", &a.name);
            put(&mut group, "offset", &a.offset);
            put(&mut group, "color", &a.color);
            Ok(vec![(
                "notes.set_guide_group",
                json!({ "world": world["slug"], "group": group }),
            )])
        })?;
        Ok(vec![json_block(&r)])
    }

    pub(crate) fn tag_levels(&mut self, a: Value) -> ToolResult {
        let a: TagLevels = args("tag_levels", a)?;
        if a.levels.is_empty() {
            return Err("levels is empty: give { \"<level slug>\": [tags], ... }".into());
        }
        let mut summary = a.levels.keys().cloned().collect::<Vec<_>>().join(", ");
        if summary.chars().count() > 60 {
            summary = summary.chars().take(57).collect::<String>() + "...";
        }
        let label = Self::label("tag_levels", a.label.as_deref(), &summary);
        let writes: Vec<(&'static str, Value)> = a
            .levels
            .iter()
            .map(|(level, tags)| ("level.update", json!({ "level": level, "tags": tags })))
            .collect();
        let r = self.write(&label, |_| Ok(writes.clone()))?;
        Ok(vec![json_block(&r)])
    }

    pub(crate) fn resize_level(&mut self, a: Value) -> ToolResult {
        let a: ResizeLevel = args("resize_level", a)?;
        let label = Self::label("resize_level", a.label.as_deref(), &a.level);
        let mut p = json!({ "level": a.level, "size": a.size });
        put(&mut p, "anchor", &a.anchor);
        put(&mut p, "world", &a.world);
        let r = self.write(&label, |_| Ok(vec![("level.resize", p.clone())]))?;
        Ok(vec![json_block(&r)])
    }

    pub(crate) fn crop_level(&mut self, a: Value) -> ToolResult {
        let a: CropLevel = args("crop_level", a)?;
        let label = Self::label("crop_level", a.label.as_deref(), &a.level);
        let mut p = json!({ "level": a.level });
        put(&mut p, "world", &a.world);
        let r = self.write(&label, |_| Ok(vec![("level.crop", p.clone())]))?;
        Ok(vec![json_block(&r)])
    }

    pub(crate) fn edit_rules(&mut self, a: Value) -> ToolResult {
        let a: EditRules = args("edit_rules", a)?;
        if a.patch.is_empty() {
            return Err("patch is empty: give set_macros, set_stamps, set_groups, set_rules, move_rules, set_level_states, remove_rules, remove_groups, remove_stamps or remove_macros".into());
        }
        let label = Self::label("edit_rules", a.label.as_deref(), &a.layer);
        let writes = plan_rules(&a.layer, &a.patch)?;
        let r = self.write(&label, |_| Ok(writes.clone()))?;
        Ok(vec![json_block(&r)])
    }

    pub(crate) fn edit_definitions(&mut self, a: Value) -> ToolResult {
        let a: EditDefinitions = args("edit_definitions", a)?;
        let (writes, summary) = plan_defs(&a.patch, a.confirm)?;
        if writes.is_empty() {
            return Err("patch is empty: give at least one of rename_entity_types, rename_enums, rename_enum_values, rename_entity_fields, rename_level_fields, rename_assemblies, set_layers, set_enums, set_entity_types, set_entity_fields, set_level_fields, set_assemblies, set_project, remove_entity_fields, remove_level_fields, remove_enums, remove_entity_types, remove_layers, remove_assemblies".into());
        }
        let label = Self::label("edit_definitions", a.label.as_deref(), &summary);
        let r = self.write(&label, |_| Ok(writes.clone()))?;
        Ok(vec![json_block(&r)])
    }

    pub(crate) fn preview_rule(&mut self, a: Value) -> ToolResult {
        let a: PreviewRule = args("preview_rule", a)?;
        // The API knows every slug, expanded ones included; an unknown one
        // comes back with the layer's rules and references listed.
        let r = match self.rpc(
            "rules.preview",
            json!({ "layer": a.layer, "level": a.level, "rule": a.rule, "matches": true }),
        ) {
            Ok(r) => r,
            Err(e) if e.data.as_ref().is_some_and(|d| d["kind"] == "unknown_rule") => {
                let list = self.read("rules.list", json!({ "layer": a.layer }))?;
                let known = list["layers"][0]["groups"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .flat_map(|g| g["rules"].as_array().into_iter().flatten())
                    .filter_map(|r| r["slug"].as_str())
                    .collect::<Vec<_>>();
                return Err(format!(
                    "layer {} has no rule {}; its rules and template references are {} (expand_rules lists the rules references expand to)",
                    a.layer,
                    a.rule,
                    known.join(", ")
                ));
            }
            Err(e) => return Err(crate::engine::rpc_message(&e)),
        };
        let fires: Vec<Value> = r["fires"].as_array().cloned().unwrap_or_default();
        let list_of = |fires: &[&Value], key: &str| -> Vec<Value> {
            let mut all: Vec<Value> = Vec::new();
            for f in fires {
                for c in f[key].as_array().into_iter().flatten() {
                    if !all.contains(c) {
                        all.push(c.clone());
                    }
                }
            }
            all.sort_by_key(|c| (c[1].as_i64(), c[0].as_i64()));
            all
        };
        // A cell one rule fired on and another was blocked on reads as fired.
        let fired_and_blocked = |fires: &[&Value]| {
            let cells = list_of(fires, "cells");
            let matched: Vec<Value> = list_of(fires, "matched")
                .into_iter()
                .filter(|c| !cells.contains(c))
                .collect();
            (cells, matched)
        };
        // One picture per grid the rules match on (a reference's rules, or
        // a rule with its own `match.grid`, can read different grids), each
        // marking only its own rules' cells.
        let mut grids: Vec<String> = Vec::new();
        for f in &fires {
            if let Some(g) = f["grid"].as_str()
                && !grids.iter().any(|x| x == g)
            {
                grids.push(g.to_string());
            }
        }
        if grids.is_empty() {
            let list = self.read("rules.list", json!({ "layer": a.layer }))?;
            grids.push(
                list["layers"][0]["source"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            );
        }
        let mut pictures = Vec::new();
        for grid in &grids {
            let on: Vec<&Value> = fires
                .iter()
                .filter(|f| f["grid"].as_str() == Some(grid) || grids.len() == 1)
                .collect();
            let (cells, matched) = fired_and_blocked(&on);
            let g = self.read("grid.get_cells", json!({ "level": a.level, "layer": grid }))?;
            let mut rows: Vec<Vec<char>> = g["rows"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|r| r.as_str().unwrap_or_default().chars().collect())
                .collect();
            for (list, mark) in [(&matched, '?'), (&cells, '@')] {
                for c in list {
                    if let (Some(x), Some(y)) = (c[0].as_u64(), c[1].as_u64())
                        && let Some(ch) = rows
                            .get_mut(y as usize)
                            .and_then(|row| row.get_mut(x as usize))
                    {
                        *ch = mark;
                    }
                }
            }
            let rows: Vec<String> = rows.into_iter().map(|r| r.into_iter().collect()).collect();
            pictures.push(json!({ "grid": grid, "picture": picture(&rows) }));
        }
        let all: Vec<&Value> = fires.iter().collect();
        let (cells, matched) = fired_and_blocked(&all);
        let sum = |key: &str| -> u64 { fires.iter().filter_map(|f| f[key].as_u64()).sum() };
        let first = pictures.remove(0);
        let mut out = json!({ "rev": r["rev"], "rule": a.rule, "level": a.level, "grid": first["grid"],
            "count": sum("count"), "cells": cells, "matched_count": sum("matched_count"),
            "matched": matched, "occupied": list_of(&all, "occupied"), "picture": first["picture"] });
        if let Some(f) = fires.first() {
            out["pass"] = f["pass"].clone();
        }
        if !pictures.is_empty() {
            out["other_grids"] = json!(pictures);
        }
        // A region rule's regions and a write rule's output (a template
        // reference holds neither, so these come from the one rule).
        if let [f] = fires.as_slice() {
            for key in ["regions", "write"] {
                if let Some(v) = f.get(key) {
                    out[key] = v.clone();
                }
            }
        }
        if fires.len() > 1 || fires.first().is_some_and(|f| f["rule"] != a.rule.as_str()) {
            out["rules"] = json!(
                fires
                    .iter()
                    .map(|f| json!({ "rule": f["rule"], "pass": f["pass"], "grid": f["grid"], "count": f["count"], "matched_count": f["matched_count"] }))
                    .collect::<Vec<_>>()
            );
        }
        if fires.iter().any(|f| f["truncated"] == true) {
            out["truncated"] = json!(true);
        }
        self.json_result(out)
    }

    pub(crate) fn find_regions(&mut self, a: Value) -> ToolResult {
        let a: FindRegions = args("find_regions", a)?;
        let mut p = json!({ "level": a.level, "cells": a.cells });
        put(&mut p, "grid", &a.grid);
        put(&mut p, "along", &a.along);
        let r = self.read("rules.regions", p)?;
        let rows: Vec<String> = r["picture"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|x| x.as_str().map(str::to_string))
            .collect();
        let regions: Vec<Value> = r["regions"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|g| {
                let mut g = g.clone();
                let id = g["id"].as_u64().unwrap_or(u64::MAX);
                g["letter"] = json!(if id < 26 {
                    ((b'a' + id as u8) as char).to_string()
                } else {
                    "+".to_string()
                });
                g
            })
            .collect();
        self.json_result(
            json!({ "rev": r["rev"], "level": a.level, "grid": r["grid"],
            "cells": a.cells, "regions": regions, "picture": picture(&rows) }),
        )
    }

    pub(crate) fn expand_rules(&mut self, a: Value) -> ToolResult {
        let a: ExpandRules = args("expand_rules", a)?;
        let mut p = json!({ "layer": a.layer });
        put(&mut p, "group", &a.group);
        let r = self.read("rules.expand", p)?;
        self.json_result(r)
    }

    pub(crate) fn bake_and_validate(&mut self, a: Value) -> ToolResult {
        let a: BakeAndValidate = args("bake_and_validate", a)?;
        let mut p = json!({});
        put(&mut p, "levels", &a.levels);
        let bake = self.read("project.bake", p.clone())?;
        let levels: Vec<Value> = bake["levels"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|l| {
                let tiles: u64 = l["layers"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|x| x["tiles"].as_u64())
                    .sum();
                let mut o = json!({ "level": l["level"], "tiles": tiles });
                if let Some(e) = l.get("error").filter(|e| !e.is_null()) {
                    o["error"] = e.clone();
                }
                o
            })
            .collect();
        let v = self.read("project.validate", json!({}))?;
        let mut out = json!({ "rev": v["rev"], "bake": levels, "problems": v["problems"],
            "warnings": bake.get("warnings").cloned().unwrap_or_else(|| json!([])) });
        if a.export {
            let x = self.read("project.export", p)?;
            out["exported"] = x["files"].clone();
        }
        self.json_result(out)
    }

    pub(crate) fn screenshot(&mut self, a: Value) -> ToolResult {
        let a: Screenshot = args("screenshot", a)?;
        if a.level.is_some() && a.world_rect.is_some() {
            return Err("screenshot needs exactly one of level or world_rect".into());
        }
        let shot = if self.mode()? == Mode::Editor {
            let mut p = json!({});
            put(&mut p, "level", &a.level);
            put(&mut p, "world_rect", &a.world_rect);
            put(&mut p, "layers", &a.layers);
            put(&mut p, "size", &a.size);
            put(&mut p, "viewport", &a.viewport);
            self.read("view.snapshot", p)?
        } else {
            if a.viewport.is_some() {
                return Err("viewport names a map tab of the Worlds editor, which isn't running: open the project in the editor, or give level or world_rect".into());
            }
            if a.level.is_none() && a.world_rect.is_none() {
                return Err("screenshot needs exactly one of level or world_rect".into());
            }
            crate::screenshot::headless(self, &a)?
        };
        let png = shot["png"].as_str().unwrap_or_default().to_string();
        let mut info = json!({ "rev": shot["rev"], "width": shot["width"], "height": shot["height"], "scale": shot["scale"] });
        let notices = self.pending_notices();
        if !notices.is_empty() {
            info["notices"] = json!(notices);
        }
        Ok(vec![
            ContentBlock::image(png, "image/png"),
            json_block(&info),
        ])
    }

    pub(crate) fn undo(&mut self, a: Value) -> ToolResult {
        let a: Undo = args("undo", a)?;
        let n = a.n.unwrap_or(1);
        if !(1..=50).contains(&n) {
            return Err("n must be 1 to 50".into());
        }
        let mut r = None;
        for attempt in 0..2 {
            match self.rpc("history.undo", json!({ "n": n, "expect_rev": self.rev })) {
                Ok(v) => {
                    r = Some(v);
                    break;
                }
                Err(e) if e.code == worlds_server::rpc::code::STALE_REV && attempt == 0 => {}
                Err(e) => return Err(crate::engine::rpc_message(&e)),
            }
        }
        let r = r.ok_or("the project kept changing; nothing was undone. Try again")?;
        let undone: Vec<Value> = r["steps"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|s| s["label"].clone())
            .collect();
        let mut out = json!({ "rev": r["rev"], "undone": undone });
        if let Some(s) = r.get("stopped").filter(|s| !s.is_null()) {
            let who = match s["origin"].get("agent").and_then(Value::as_str) {
                Some(a) => format!("an edit by {a}"),
                None => "an edit by the user".into(),
            };
            out["stopped"] = json!(format!(
                "stopped at \"{}\", {who}, which this tool never undoes",
                s["label"].as_str().unwrap_or("")
            ));
        }
        if undone.is_empty() && out.get("stopped").is_none() {
            out["stopped"] = json!("there is nothing of yours to undo");
        }
        if self.mode()? == Mode::Headless && !undone.is_empty() {
            self.read("project.save", json!({}))?;
        }
        let rev = r["rev"].as_u64().unwrap_or(self.rev);
        let mut levels = Vec::new();
        for c in r["changes"].as_array().into_iter().flatten() {
            if let Some(l) = c["level"].as_str()
                && !levels.iter().any(|x: &String| x == l)
            {
                levels.push(l.to_string());
            }
        }
        let notices = self.take_notices(&levels, rev, "read_level");
        if !notices.is_empty() {
            out["notices"] = json!(notices);
        }
        Ok(vec![json_block(&out)])
    }

    pub(crate) fn copy_fragment(&mut self, a: Value) -> ToolResult {
        // The arguments are `edit.copy`'s selection, checked there.
        if !a.is_object() && !a.is_null() {
            return Err("copy_fragment takes an object of arguments".into());
        }
        let selection = if a.is_null() { json!({}) } else { a };
        let r = self.read("edit.copy", json!({ "selection": selection }))?;
        self.json_result(r)
    }

    pub(crate) fn paste_fragment(&mut self, a: Value) -> ToolResult {
        let a: PasteFragment = args("paste_fragment", a)?;
        let label = Self::label(
            "paste_fragment",
            a.label.as_deref(),
            &format!("into {}", a.level),
        );
        let mut p = json!({ "level": a.level, "origin": a.origin, "fragment": a.fragment });
        put(&mut p, "layer_map", &a.layer_map);
        put(&mut p, "flip", &a.flip);
        let (mut out, raw) = self.write_full(&label, |_| Ok(vec![("edit.paste", p.clone())]))?;
        if let Some(raw) = raw {
            for k in ["items", "ids", "problems"] {
                if let Some(v) = raw.get(k) {
                    out[k] = v.clone();
                }
            }
        }
        Ok(vec![json_block(&out)])
    }

    pub(crate) fn save_assembly(&mut self, a: Value) -> ToolResult {
        let a: SaveAssembly = args("save_assembly", a)?;
        let label = Self::label("save_assembly", a.label.as_deref(), &a.slug);
        let mut sel = json!({});
        put(&mut sel, "level", &a.level);
        put(&mut sel, "entities", &a.entities);
        put(&mut sel, "notes", &a.notes);
        put(&mut sel, "tiles", &a.tiles);
        put(&mut sel, "rect", &a.rect);
        put(&mut sel, "layers", &a.layers);
        let has_sel = sel.as_object().is_some_and(|m| !m.is_empty());
        let mut p = json!({ "slug": a.slug, "name": a.name });
        match (has_sel, a.fragment) {
            (true, None) => p["selection"] = sel,
            (false, Some(f)) => p["fragment"] = f,
            (true, Some(_)) => {
                return Err("give either what to copy (level, entities, notes, tiles, rect, layers) or a fragment, not both".into());
            }
            (false, None) => {
                return Err("nothing to save: give entities, notes, tiles and/or rect (as for copy_fragment), or a fragment".into());
            }
        }
        put(&mut p, "category", &a.category);
        put(&mut p, "tags", &a.tags);
        put(&mut p, "doc", &a.doc);
        put(&mut p, "replace", &a.replace);
        let (mut out, raw) =
            self.write_full(&label, |_| Ok(vec![("edit.save_assembly", p.clone())]))?;
        if let Some(raw) = raw {
            for k in ["items", "size"] {
                if let Some(v) = raw.get(k) {
                    out[k] = v.clone();
                }
            }
        }
        Ok(vec![json_block(&out)])
    }

    pub(crate) fn place_assembly(&mut self, a: Value) -> ToolResult {
        let a: PlaceAssembly = args("place_assembly", a)?;
        let label = Self::label(
            "place_assembly",
            a.label.as_deref(),
            &format!("{} in {}", a.slug, a.level),
        );
        let mut p = json!({ "level": a.level, "slug": a.slug, "origin": a.cell });
        put(&mut p, "flip", &a.flip);
        let (mut out, raw) =
            self.write_full(&label, |_| Ok(vec![("edit.place_assembly", p.clone())]))?;
        if let Some(raw) = raw {
            for k in ["items", "ids", "problems"] {
                if let Some(v) = raw.get(k) {
                    out[k] = v.clone();
                }
            }
        }
        Ok(vec![json_block(&out)])
    }

    pub(crate) fn arrange_levels(&mut self, a: Value) -> ToolResult {
        let a: ArrangeLevels = args("arrange_levels", a)?;
        let label = Self::label(
            "arrange_levels",
            a.label.as_deref(),
            &format!("{} {}", a.op, a.levels.join(", ")),
        );
        let p = json!({ "levels": a.levels, "op": a.op });
        let r = self.write(&label, |_| Ok(vec![("level.arrange", p.clone())]))?;
        Ok(vec![json_block(&r)])
    }

    pub(crate) fn duplicate_level(&mut self, a: Value) -> ToolResult {
        let a: DuplicateLevel = args("duplicate_level", a)?;
        let label = Self::label(
            "duplicate_level",
            a.label.as_deref(),
            &format!("{} as {}", a.level, a.slug),
        );
        let mut p = json!({ "level": a.level, "slug": a.slug });
        put(&mut p, "name", &a.name);
        put(&mut p, "world", &a.world);
        put(&mut p, "world_px", &a.world_px);
        let (mut out, raw) =
            self.write_full(&label, |_| Ok(vec![("level.duplicate", p.clone())]))?;
        if let Some(wp) = raw.as_ref().and_then(|r| r.get("world_px")) {
            out["world_px"] = wp.clone();
        }
        Ok(vec![json_block(&out)])
    }

    /// `find`: `entity.find`, `level.find`, `notes.list` or `tiles.find`
    /// with the query, at most `limit` results.
    pub(crate) fn find(&mut self, a: Value) -> ToolResult {
        let a: Find = args("find", a)?;
        let query = a.query.unwrap_or_default();
        let parsed = worlds_core::find::Query::parse(&query).map_err(|e| format!("query: {e}"))?;
        let limit = a.limit.unwrap_or(FIND_LIMIT);
        let (method, key, mut p) = match a.kind.as_str() {
            "entities" => ("entity.find", "entities", json!({})),
            "levels" => ("level.find", "levels", json!({})),
            "notes" => {
                // Guides stay out unless asked for by kind.
                let p = if parsed.kinds.is_empty() {
                    json!({ "kinds": ["note", "pin", "path", "region", "connector"] })
                } else {
                    json!({})
                };
                ("notes.list", "notes", p)
            }
            "tiles" => ("tiles.find", "tiles", json!({ "limit": limit })),
            other => {
                return Err(format!(
                    "kind {other:?}: expected entities, levels, notes or tiles"
                ));
            }
        };
        if !query.trim().is_empty() {
            p["query"] = json!(query);
        }
        let r = self.read(method, p)?;
        let mut list = r[key].as_array().cloned().unwrap_or_default();
        let total = r
            .get("total")
            .and_then(Value::as_u64)
            .unwrap_or(list.len() as u64);
        list.truncate(limit);
        let mut out = json!({ "rev": r["rev"], "kind": a.kind, "total": total });
        out[key] = Value::Array(list);
        self.json_result(out)
    }
}

/// How many results `find` returns by default.
const FIND_LIMIT: usize = 100;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Find {
    kind: String,
    query: Option<String>,
    limit: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PasteFragment {
    level: String,
    origin: [i64; 2],
    fragment: Value,
    layer_map: Option<BTreeMap<String, String>>,
    flip: Option<String>,
    label: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SaveAssembly {
    level: Option<String>,
    entities: Option<Vec<String>>,
    notes: Option<Vec<String>>,
    tiles: Option<BTreeMap<String, Vec<[u32; 2]>>>,
    rect: Option<[u32; 4]>,
    layers: Option<Vec<String>>,
    fragment: Option<Value>,
    slug: String,
    name: String,
    category: Option<String>,
    tags: Option<Vec<String>>,
    doc: Option<String>,
    replace: Option<bool>,
    label: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PlaceAssembly {
    level: String,
    slug: String,
    cell: [i64; 2],
    flip: Option<String>,
    label: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ArrangeLevels {
    levels: Vec<String>,
    op: String,
    label: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DuplicateLevel {
    level: String,
    slug: String,
    name: Option<String>,
    world: Option<String>,
    world_px: Option<[i64; 2]>,
    label: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateWorld {
    slug: String,
    name: Option<String>,
    label: Option<String>,
}

#[derive(Deserialize, Clone)]
#[serde(deny_unknown_fields)]
struct RenameWorld {
    world: String,
    slug: Option<String>,
    name: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EditWorlds {
    #[serde(default)]
    rename: Vec<RenameWorld>,
    order: Option<Vec<String>>,
    #[serde(default)]
    remove: Vec<String>,
    #[serde(default)]
    confirm: bool,
    label: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MoveLevel {
    level: String,
    world_px: Option<[i64; 2]>,
    world: Option<String>,
    label: Option<String>,
}

/// The API writes of an `edit_worlds` call, in order: renames, the new
/// order, removals; and the undo label's summary (the worlds renamed or
/// removed, each once, or `order`).
fn plan_worlds(a: &EditWorlds) -> Result<(Vec<ApiWrite>, String), String> {
    let mut writes = Vec::new();
    let mut names: Vec<&str> = Vec::new();
    for r in &a.rename {
        if r.slug.is_none() && r.name.is_none() {
            return Err(format!(
                "rename of world {:?} changes nothing: give slug or name",
                r.world
            ));
        }
        let mut p = json!({ "world": r.world });
        put(&mut p, "slug", &r.slug);
        put(&mut p, "name", &r.name);
        writes.push(("world.rename", p));
        if !names.contains(&r.world.as_str()) {
            names.push(&r.world);
        }
    }
    if let Some(order) = &a.order {
        writes.push(("world.reorder", json!({ "order": order })));
    }
    for w in &a.remove {
        let mut p = json!({ "world": w });
        if a.confirm {
            p["confirm"] = json!(true);
        }
        writes.push(("world.remove", p));
        if !names.contains(&w.as_str()) {
            names.push(w);
        }
    }
    let mut summary = if names.is_empty() {
        "order".to_string()
    } else {
        names.join(", ")
    };
    if summary.chars().count() > 60 {
        summary = summary.chars().take(57).collect::<String>() + "...";
    }
    Ok((writes, summary))
}

impl Engine {
    pub(crate) fn create_world(&mut self, a: Value) -> ToolResult {
        let a: CreateWorld = args("create_world", a)?;
        let label = Self::label("create_world", a.label.as_deref(), &a.slug);
        let mut p = json!({ "slug": a.slug });
        put(&mut p, "name", &a.name);
        let r = self.write(&label, |_| Ok(vec![("world.create", p.clone())]))?;
        Ok(vec![json_block(&r)])
    }

    pub(crate) fn edit_worlds(&mut self, a: Value) -> ToolResult {
        let a: EditWorlds = args("edit_worlds", a)?;
        let (writes, summary) = plan_worlds(&a)?;
        if writes.is_empty() {
            return Err("nothing to do: give rename, order or remove".into());
        }
        let label = Self::label("edit_worlds", a.label.as_deref(), &summary);
        let r = self.write(&label, |_| Ok(writes.clone()))?;
        Ok(vec![json_block(&r)])
    }

    pub(crate) fn move_level(&mut self, a: Value) -> ToolResult {
        let a: MoveLevel = args("move_level", a)?;
        let label = Self::label("move_level", a.label.as_deref(), &a.level);
        let write = match (&a.world, a.world_px) {
            (Some(w), px) => {
                let mut p = json!({ "level": a.level, "world": w });
                put(&mut p, "world_px", &px);
                ("level.move_to_world", p)
            }
            (None, Some(px)) => ("level.move", json!({ "level": a.level, "world_px": px })),
            (None, None) => {
                return Err(
                    "give world_px (a new position), world (another world), or both".into(),
                );
            }
        };
        let r = self.write(&label, |_| Ok(vec![write.clone()]))?;
        Ok(vec![json_block(&r)])
    }

    pub(crate) fn import_project(&mut self, a: Value) -> ToolResult {
        let a: ImportProject = args("import_project", a)?;
        let mut p = json!({ "source": a.source });
        put(&mut p, "selection", &a.selection);
        put(&mut p, "policy", &a.policy);
        if a.dry_run {
            let r = self.read("project.import_plan", p)?;
            return self.json_result(r);
        }
        let label = Self::label(
            "import_project",
            a.label.as_deref(),
            &format!("from {}", a.source),
        );
        let mut plan = Value::Null;
        let (mut out, raw) = self.write_full(&label, |e| {
            let r = e.read("project.import_plan", p.clone())?;
            if r["empty"] == true {
                return Err(format!(
                    "nothing to import: {}",
                    r["summary"].as_str().unwrap_or("")
                ));
            }
            plan = r.clone();
            Ok(vec![("project.import", json!({ "plan_id": r["plan_id"] }))])
        })?;
        if let Some(o) = plan.as_object_mut() {
            o.remove("rev");
            o.remove("plan_id");
            o.remove("notices");
        }
        out["plan"] = plan;
        if let Some(c) = raw.as_ref().and_then(|r| r.get("copied")) {
            out["copied"] = c.clone();
        }
        Ok(vec![json_block(&out)])
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportProject {
    source: String,
    selection: Option<Value>,
    policy: Option<Value>,
    #[serde(default)]
    dry_run: bool,
    label: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AddReference {
    scope: Scope,
    image: String,
    px: [i64; 2],
    scale: Option<f64>,
    opacity: Option<f64>,
    locked: Option<bool>,
    visible: Option<bool>,
    label: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReferencePatch {
    image: Option<String>,
    px: Option<[i64; 2]>,
    scale: Option<f64>,
    opacity: Option<f64>,
    locked: Option<bool>,
    visible: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateReference {
    id: String,
    patch: ReferencePatch,
    #[serde(default)]
    confirm: bool,
    label: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoveReference {
    ids: Vec<String>,
    #[serde(default)]
    confirm: bool,
    label: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum LayerOp {
    Duplicate,
    MergeDown,
    MoveTiles,
    MoveEntities,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LayerOperation {
    op: LayerOp,
    layer: Option<String>,
    to: Option<String>,
    level: Option<String>,
    cells: Option<Vec<[u32; 2]>>,
    ids: Option<Vec<String>>,
    label: Option<String>,
}

impl Engine {
    pub(crate) fn add_reference(&mut self, a: Value) -> ToolResult {
        let a: AddReference = args("add_reference", a)?;
        a.scope.check()?;
        let mut reference = json!({ "image": a.image, "px": a.px });
        put(&mut reference, "scale", &a.scale);
        put(&mut reference, "opacity", &a.opacity);
        put(&mut reference, "locked", &a.locked);
        put(&mut reference, "visible", &a.visible);
        let label = Self::label("add_reference", a.label.as_deref(), &a.scope.name());
        let p = json!({ "scope": a.scope, "reference": reference });
        let r = self.write(&label, |_| Ok(vec![("reference.add", p.clone())]))?;
        Ok(vec![json_block(&r)])
    }

    pub(crate) fn update_reference(&mut self, a: Value) -> ToolResult {
        let a: UpdateReference = args("update_reference", a)?;
        let pt = &a.patch;
        let mut patch = json!({});
        put(&mut patch, "image", &pt.image);
        put(&mut patch, "px", &pt.px);
        put(&mut patch, "scale", &pt.scale);
        put(&mut patch, "opacity", &pt.opacity);
        put(&mut patch, "locked", &pt.locked);
        put(&mut patch, "visible", &pt.visible);
        if patch.as_object().is_some_and(|m| m.is_empty()) {
            return Err("patch is empty: give image, px, scale, opacity, locked or visible".into());
        }
        let label = Self::label("update_reference", a.label.as_deref(), &a.id);
        let mut p = json!({ "id": a.id, "patch": patch });
        if a.confirm {
            p["confirm"] = json!(true);
        }
        let r = self.write(&label, |_| Ok(vec![("reference.update", p.clone())]))?;
        Ok(vec![json_block(&r)])
    }

    pub(crate) fn remove_reference(&mut self, a: Value) -> ToolResult {
        let a: RemoveReference = args("remove_reference", a)?;
        if a.ids.is_empty() {
            return Err("ids is empty: give the ids of the reference images to remove".into());
        }
        let summary = match a.ids.as_slice() {
            [one] => one.clone(),
            many => format!("{} references", many.len()),
        };
        let label = Self::label("remove_reference", a.label.as_deref(), &summary);
        let mut p = json!({ "ids": a.ids });
        if a.confirm {
            p["confirm"] = json!(true);
        }
        let r = self.write(&label, |_| Ok(vec![("reference.remove", p.clone())]))?;
        Ok(vec![json_block(&r)])
    }

    pub(crate) fn layer_operation(&mut self, a: Value) -> ToolResult {
        let a: LayerOperation = args("layer_operation", a)?;
        let need = |v: &Option<String>, key: &str, op: &str| {
            v.clone().ok_or_else(|| format!("{op} needs {key}"))
        };
        let only = |given: &[(&str, bool)], op: &str| -> Result<(), String> {
            match given.iter().find(|(_, g)| *g) {
                Some((k, _)) => Err(format!("{op} doesn't take {k}")),
                None => Ok(()),
            }
        };
        let (summary, method, p) = match a.op {
            LayerOp::Duplicate => {
                let layer = need(&a.layer, "layer", "duplicate")?;
                only(
                    &[
                        ("level", a.level.is_some()),
                        ("cells", a.cells.is_some()),
                        ("ids", a.ids.is_some()),
                    ],
                    "duplicate",
                )?;
                let mut p = json!({ "kind": "layer", "slug": layer });
                put(&mut p, "to", &a.to);
                (format!("duplicate {layer}"), "defs.duplicate", p)
            }
            LayerOp::MergeDown => {
                let layer = need(&a.layer, "layer", "merge_down")?;
                only(
                    &[
                        ("to", a.to.is_some()),
                        ("level", a.level.is_some()),
                        ("cells", a.cells.is_some()),
                        ("ids", a.ids.is_some()),
                    ],
                    "merge_down",
                )?;
                (
                    format!("merge_down {layer}"),
                    "defs.merge_down",
                    json!({ "kind": "layer", "slug": layer }),
                )
            }
            LayerOp::MoveTiles => {
                let layer = need(&a.layer, "layer", "move_tiles")?;
                let to = need(&a.to, "to", "move_tiles")?;
                let level = need(&a.level, "level", "move_tiles")?;
                only(&[("ids", a.ids.is_some())], "move_tiles")?;
                let cells = a
                    .cells
                    .clone()
                    .filter(|c| !c.is_empty())
                    .ok_or("move_tiles needs cells: the [x, y] cells whose tiles move")?;
                (
                    format!("move_tiles {layer} in {level}"),
                    "tiles.move_to_layer",
                    json!({ "level": level, "layer": layer, "to": to, "cells": cells }),
                )
            }
            LayerOp::MoveEntities => {
                let to = need(&a.to, "to", "move_entities")?;
                only(
                    &[
                        ("layer", a.layer.is_some()),
                        ("level", a.level.is_some()),
                        ("cells", a.cells.is_some()),
                    ],
                    "move_entities",
                )?;
                let ids = a
                    .ids
                    .clone()
                    .filter(|i| !i.is_empty())
                    .ok_or("move_entities needs ids: the entities to move")?;
                (
                    format!("move_entities to {to}"),
                    "entity.move_to_layer",
                    json!({ "ids": ids, "layer": to }),
                )
            }
        };
        let label = Self::label("layer_operation", a.label.as_deref(), &summary);
        let r = self.write(&label, |_| Ok(vec![(method, p.clone())]))?;
        Ok(vec![json_block(&r)])
    }
}

/// Rows with the column rulers and row numbers of the text view.
fn picture(rows: &[String]) -> String {
    let rw = rows.first().map_or(0, |r| r.chars().count());
    let label = rows.len().saturating_sub(1).to_string().len();
    let pad = " ".repeat(label + 1);
    let mut out = String::new();
    if rw > 10 {
        let tens: String = (0..rw)
            .map(|x| {
                if x % 10 == 0 {
                    char::from_digit(((x / 10) % 10) as u32, 10).unwrap_or(' ')
                } else {
                    ' '
                }
            })
            .collect();
        out.push_str(&format!("{pad}{}\n", tens.trim_end()));
    }
    let ones: String = (0..rw)
        .map(|x| char::from_digit((x % 10) as u32, 10).unwrap_or(' '))
        .collect();
    out.push_str(&format!("{pad}{ones}\n"));
    for (y, row) in rows.iter().enumerate() {
        out.push_str(&format!("{y:>label$} {row}\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One entry in every list of an `edit_definitions` patch.
    fn full_patch() -> DefsPatch {
        serde_json::from_value(json!({
            "remove_entity_types": ["t"],
            "remove_enums": ["e"],
            "remove_level_fields": ["lf"],
            "remove_entity_fields": [ { "entity": "t", "field": "f" } ],
            "set_level_fields": [ { "def": { "slug": "lf" } } ],
            "set_entity_fields": [ { "entity": "t", "def": { "slug": "f" }, "index": 0 } ],
            "set_entity_types": [ { "$schema": "../schema/entity.json", "format": 1, "slug": "t" } ],
            "set_enums": [ { "slug": "e" } ],
            "set_layers": [ { "def": { "slug": "l" }, "index": 1 } ],
            "remove_layers": ["l"],
            "rename_level_fields": [ { "from": "a", "to": "lf" } ],
            "rename_entity_fields": [ { "entity": "t", "from": "a", "to": "f" } ],
            "rename_enum_values": [ { "enum": "e", "from": "a", "to": "b" } ],
            "rename_enums": [ { "from": "a", "to": "e" } ],
            "rename_entity_types": [ { "from": "a", "to": "t" } ]
        }))
        .unwrap()
    }

    #[test]
    fn definition_writes_follow_the_patch_order() {
        let (w, summary) = plan_defs(&full_patch(), false).unwrap();
        let got: Vec<String> = w
            .iter()
            .map(|(m, p)| format!("{m} {}", p["kind"].as_str().unwrap()))
            .collect();
        assert_eq!(
            got,
            [
                "defs.rename entity",
                "defs.rename enum",
                "defs.rename enum_value",
                "defs.rename entity_field",
                "defs.rename level_field",
                "defs.set layer",
                "defs.set enum",
                "defs.set entity",
                "defs.set entity_field",
                "defs.set level_field",
                "defs.remove entity_field",
                "defs.remove level_field",
                "defs.remove enum",
                "defs.remove entity",
                "defs.remove layer",
            ]
        );
        assert_eq!(summary, "t, e, level fields, l");
        assert_eq!(
            w[5].1,
            json!({ "kind": "layer", "def": { "slug": "l" }, "index": 1 })
        );
        // The file's $schema and format are dropped; index passes through.
        assert_eq!(w[7].1["def"], json!({ "slug": "t" }));
        assert_eq!(w[8].1["index"], 0);
        assert_eq!(
            w[10].1,
            json!({ "kind": "entity_field", "entity": "t", "slug": "f" })
        );
        assert!(w.iter().all(|(_, p)| p.get("confirm").is_none()));
    }

    #[test]
    fn confirm_goes_on_sets_and_removes_only_when_asked() {
        let (w, _) = plan_defs(&full_patch(), true).unwrap();
        for (m, p) in &w {
            assert_eq!(p.get("confirm").is_some(), *m != "defs.rename", "{m} {p}");
        }
    }

    #[test]
    fn definition_summaries_are_cut_to_60() {
        let pt: DefsPatch = serde_json::from_value(json!({
            "remove_entity_types": (0..20).map(|i| format!("type_{i}")).collect::<Vec<_>>()
        }))
        .unwrap();
        let (_, summary) = plan_defs(&pt, true).unwrap();
        assert_eq!(summary.chars().count(), 60);
        assert!(summary.starts_with("type_0, type_1, ") && summary.ends_with("..."));
    }

    #[test]
    fn outline_is_the_border() {
        assert_eq!(
            outline_rects([1, 1, 3, 3]).unwrap(),
            vec![[1, 1, 3, 1], [1, 3, 3, 1], [1, 2, 1, 1], [3, 2, 1, 1]]
        );
        assert_eq!(outline_rects([0, 0, 1, 1]).unwrap(), vec![[0, 0, 1, 1]]);
        assert_eq!(outline_rects([5, 5, 2, 9]).unwrap(), vec![[5, 5, 2, 9]]);
        // Huge or overflowing rects: four rects or an error, never a scan.
        assert_eq!(outline_rects([0, 0, u32::MAX, u32::MAX]).unwrap().len(), 4);
        assert!(outline_rects([u32::MAX, 0, 2, 1]).is_err());
    }

    #[test]
    fn picture_has_rulers() {
        let rows: Vec<String> = vec!["#".repeat(12), "#@..........".into()];
        assert_eq!(
            picture(&rows),
            "  0         1\n  012345678901\n0 ############\n1 #@..........\n"
        );
    }

    #[test]
    fn placements() {
        assert_eq!(
            placement(&[json!(1), json!(2), json!(3), json!(4), json!("x")], "t").unwrap(),
            json!({ "cell": [1, 2], "tileset": "t", "tile": [3, 4], "flip": "x" })
        );
        assert!(placement(&[json!(1), json!(2), json!(3)], "t").is_err());
        assert!(placement(&[json!(1), json!(2), json!(3), json!(4), json!("z")], "t").is_err());
    }
}
