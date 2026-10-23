//! `defs.*` of the field kinds (`entity_field`, `level_field`): shapes,
//! when `confirm` is needed (a retype, including an enum source change, while
//! values are stored; every remove), errors on the wire, and the
//! `project.describe` palette data.

mod common;

use serde_json::{Value, json};
use worlds_server::RpcError;
use worlds_server::headless::Session;
use worlds_server::rpc::code;

fn session() -> (tempfile::TempDir, Session) {
    let (tmp, dir) = common::mini();
    (tmp, Session::open(&dir).unwrap().client("claude-code"))
}

fn field(s: &mut Session, entity: &str, slug: &str) -> Value {
    s.call(
        "defs.get",
        json!({ "kind": "entity_field", "entity": entity, "slug": slug }),
    )
    .unwrap()["def"]
        .clone()
}

fn set_field(s: &mut Session, entity: &str, def: Value) -> Result<Value, RpcError> {
    s.call(
        "defs.set",
        json!({ "kind": "entity_field", "entity": entity, "def": def }),
    )
}

/// The `needs_confirm` reason of `r`, or a panic.
fn reason(r: Result<Value, RpcError>) -> String {
    let e = r.expect_err("expected needs_confirm");
    assert_eq!(e.code, code::NEEDS_CONFIRM, "{}", e.message);
    e.data.unwrap()["reason"].as_str().unwrap().to_string()
}

fn stored(s: &Session, id: &str, field: &str) -> Option<Value> {
    s.project()
        .levels()
        .flat_map(|l| l.all_entities())
        .find(|e| e.id.as_str() == id)
        .unwrap()
        .fields
        .get(field)
        .cloned()
}

fn field_slugs(s: &Session, entity: &str) -> Vec<String> {
    s.project()
        .entity_def(entity)
        .unwrap()
        .fields
        .iter()
        .map(|f| f.slug.to_string())
        .collect()
}

#[test]
fn defs_get_lists_and_finds_fields() {
    let (_t, mut s) = session();
    let r = s
        .call(
            "defs.get",
            json!({ "kind": "entity_field", "entity": "floor_button" }),
        )
        .unwrap();
    assert_eq!(r["rev"], 0);
    let fields = r["defs"]["fields"].as_array().unwrap();
    assert_eq!(fields.len(), 3);
    assert_eq!(
        fields[0],
        json!({ "slug": "channel", "type": "int", "default": 0, "min": 0, "max": 15, "display": { "canvas": true } })
    );
    let r = s
        .call(
            "defs.get",
            json!({ "kind": "entity_field", "entity": "floor_button", "slug": "latching" }),
        )
        .unwrap();
    assert_eq!(
        r,
        json!({ "rev": 0, "kind": "entity_field", "entity": "floor_button",
                "def": { "slug": "latching", "type": "bool", "default": false } })
    );
    let r = s
        .call("defs.get", json!({ "kind": "level_field" }))
        .unwrap();
    assert_eq!(r["defs"]["fields"][1]["slug"], "music");
    let r = s
        .call(
            "defs.get",
            json!({ "kind": "level_field", "slug": "music" }),
        )
        .unwrap();
    assert_eq!(
        r,
        json!({ "rev": 0, "kind": "level_field", "def": { "slug": "music", "type": "string", "nullable": true } })
    );
}

#[test]
fn field_kinds_check_entity_and_name_what_is_missing() {
    let (_t, mut s) = session();
    let code_of = |r: Result<Value, RpcError>| r.unwrap_err().code;
    // entity is required with entity_field and refused elsewhere.
    for (m, p) in [
        ("defs.get", json!({ "kind": "entity_field" })),
        (
            "defs.get",
            json!({ "kind": "level_field", "entity": "floor_button" }),
        ),
        (
            "defs.get",
            json!({ "kind": "entity", "entity": "floor_button" }),
        ),
        ("defs.get", json!({ "entity": "floor_button" })),
        (
            "defs.set",
            json!({ "kind": "entity_field", "def": { "slug": "x", "type": "int" } }),
        ),
        (
            "defs.rename",
            json!({ "kind": "layer", "entity": "floor_button", "from": "a", "to": "b" }),
        ),
        (
            "defs.remove",
            json!({ "kind": "entity_field", "slug": "x", "confirm": true }),
        ),
        // Not a field definition.
        (
            "defs.set",
            json!({ "kind": "level_field", "def": { "slug": "x" } }),
        ),
    ] {
        assert_eq!(
            code_of(s.call(m, p.clone())),
            code::INVALID_PARAMS,
            "{m} {p}"
        );
    }
    let e = s
        .call(
            "defs.get",
            json!({ "kind": "entity_field", "entity": "lever" }),
        )
        .unwrap_err();
    assert_eq!(e.code, code::COMMAND_ERROR);
    assert_eq!(e.data.unwrap()["kind"], "unknown_entity_type");
    let e = s
        .call(
            "defs.get",
            json!({ "kind": "entity_field", "entity": "floor_button", "slug": "delay" }),
        )
        .unwrap_err();
    assert_eq!(
        e.data.unwrap(),
        json!({ "kind": "unknown_field", "owner": "entity type \"floor_button\"", "field": "delay",
                "message": "entity type \"floor_button\" has no field \"delay\"" })
    );
    let e = s
        .call("defs.get", json!({ "kind": "level_field", "slug": "zone" }))
        .unwrap_err();
    assert_eq!(e.data.unwrap()["owner"], "project level_fields");
    // Not reserved any more: an unknown field to remove is a command error.
    let e = s
        .call(
            "defs.remove",
            json!({ "kind": "level_field", "slug": "zone", "confirm": true }),
        )
        .unwrap_err();
    assert_eq!(e.code, code::COMMAND_ERROR);
}

#[test]
fn retyping_a_stored_field_needs_confirm_and_keeps_the_values() {
    let (_t, mut s) = session();
    let mut def = field(&mut s, "locked_door", "needs_item");
    def["type"] = json!("int");
    def["default"] = json!(0);
    assert_eq!(
        reason(set_field(&mut s, "locked_door", def.clone())),
        "1 entity stores a value for field needs_item, whose type changes"
    );
    assert_eq!(
        s.project().entity_def("locked_door").unwrap().fields[1]
            .ty
            .name(),
        "string"
    );
    assert_eq!(s.history().undo_entries().len(), 0);
    let r = s
        .call(
            "defs.set",
            json!({ "kind": "entity_field", "entity": "locked_door", "def": def, "confirm": true }),
        )
        .unwrap();
    assert_eq!(
        r["label"],
        "claude-code: Set field needs_item of locked_door"
    );
    assert_eq!(
        r["changes"],
        json!([{ "change": "def", "kind": "entity", "slug": "locked_door" }])
    );
    assert_eq!(
        stored(&s, "doorcrev22", "needs_item"),
        Some(json!("brass_key"))
    );
}

#[test]
fn an_enum_source_change_is_a_retype() {
    let (_t, mut s) = session();
    // doorcrev22 stores facing: right (project enum facing).
    let mut inline = field(&mut s, "locked_door", "facing");
    inline.as_object_mut().unwrap().remove("enum");
    inline["values"] = json!(["left", "right"]);
    assert_eq!(
        reason(set_field(&mut s, "locked_door", inline)),
        "1 entity stores a value for field facing, whose type changes"
    );
    let mut other = field(&mut s, "locked_door", "facing");
    other["enum"] = json!("biome");
    other["default"] = json!("dry");
    reason(set_field(&mut s, "locked_door", other));

    // style is inline on both sides: different values are no retype.
    s.call(
        "entity.update",
        json!({ "id": "doorcrev22", "fields": { "style": "iron" } }),
    )
    .unwrap();
    let mut style = field(&mut s, "locked_door", "style");
    style["values"] = json!(["wood", "iron", "stone"]);
    set_field(&mut s, "locked_door", style).unwrap();
    // inline -> project enum is.
    let mut style = field(&mut s, "locked_door", "style");
    style.as_object_mut().unwrap().remove("values");
    style["enum"] = json!("facing");
    style["default"] = json!("left");
    reason(set_field(&mut s, "locked_door", style));
}

#[test]
fn defs_set_entity_counts_an_enum_source_change() {
    let (_t, mut s) = session();
    let mut def = s
        .call(
            "defs.get",
            json!({ "kind": "entity", "slug": "locked_door" }),
        )
        .unwrap()["def"]
        .clone();
    let facing = def["fields"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|f| f["slug"] == "facing")
        .unwrap();
    facing.as_object_mut().unwrap().remove("enum");
    facing["values"] = json!(["left", "right"]);
    assert_eq!(
        reason(s.call("defs.set", json!({ "kind": "entity", "def": def }))),
        "1 entity stores a value for a field that no longer exists or becomes invalid: facing"
    );
}

#[test]
fn an_array_flag_change_is_a_retype() {
    let (_t, mut s) = session();
    let mut def = field(&mut s, "locked_door", "waypoints");
    def.as_object_mut().unwrap().remove("array");
    def["default"] = json!([0, 0]);
    reason(set_field(&mut s, "locked_door", def));
}

#[test]
fn moves_and_other_edits_need_no_confirm() {
    let (_t, mut s) = session();
    // A pure move of a stored field.
    let def = field(&mut s, "locked_door", "needs_item");
    let r = s
        .call(
            "defs.set",
            json!({ "kind": "entity_field", "entity": "locked_door", "def": def, "index": 0 }),
        )
        .unwrap();
    assert_eq!(r["status"], "applied");
    assert_eq!(
        field_slugs(&s, "locked_door")[..2],
        ["needs_item", "channel"]
    );
    // A new default on a stored field.
    let mut def = field(&mut s, "locked_door", "speed");
    def["default"] = json!(3.0);
    set_field(&mut s, "locked_door", def).unwrap();
    // A retype nobody stores a value for.
    let mut def = field(&mut s, "locked_door", "label");
    def["type"] = json!("string");
    set_field(&mut s, "locked_door", def).unwrap();
    // A new field.
    set_field(
        &mut s,
        "locked_door",
        json!({ "slug": "key_color", "type": "color", "default": "#ffd040" }),
    )
    .unwrap();
    assert_eq!(field_slugs(&s, "locked_door").last().unwrap(), "key_color");
    // Renames never ask.
    s.call(
        "defs.rename",
        json!({ "kind": "entity_field", "entity": "locked_door", "from": "needs_item", "to": "key_item" }),
    )
    .unwrap();
    assert_eq!(
        stored(&s, "doorcrev22", "key_item"),
        Some(json!("brass_key"))
    );
    assert_eq!(s.history().undo_entries().len(), 5);
}

#[test]
fn removing_a_field_always_needs_confirm_and_says_what_goes() {
    let (_t, mut s) = session();
    assert_eq!(
        reason(s.call(
            "defs.remove",
            json!({ "kind": "entity_field", "entity": "locked_door", "slug": "label" })
        )),
        "removes field label of locked_door"
    );
    s.call(
        "entity.update",
        json!({ "id": "doorshaft2", "fields": { "needs_item": "iron_key" } }),
    )
    .unwrap();
    let p = json!({ "kind": "entity_field", "entity": "locked_door", "slug": "needs_item" });
    assert_eq!(
        reason(s.call("defs.remove", p.clone())),
        "removes field needs_item of locked_door and the values 2 entities store; undo brings them back"
    );
    assert!(stored(&s, "doorcrev22", "needs_item").is_some());
    let mut p = p;
    p["confirm"] = json!(true);
    let r = s.call("defs.remove", p).unwrap();
    assert_eq!(
        r["label"],
        "claude-code: Remove field needs_item of locked_door"
    );
    assert_eq!(
        r["changes"],
        json!([
            { "change": "entity", "world": "main", "level": "crevasse", "id": "doorcrev22" },
            { "change": "entity", "world": "main", "level": "shaft", "id": "doorshaft2" },
            { "change": "def", "kind": "entity", "slug": "locked_door" }
        ])
    );
    assert!(stored(&s, "doorcrev22", "needs_item").is_none());
    assert!(!field_slugs(&s, "locked_door").contains(&"needs_item".to_string()));
    // One undo entry brings the field and both values back.
    s.call("history.undo", json!({})).unwrap();
    assert_eq!(field_slugs(&s, "locked_door")[1], "needs_item");
    assert_eq!(
        stored(&s, "doorshaft2", "needs_item"),
        Some(json!("iron_key"))
    );
}

#[test]
fn level_fields_count_levels() {
    let (_t, mut s) = session();
    // crevasse stores music (null) and biome.
    let p = json!({ "kind": "level_field", "def": { "slug": "music", "type": "int", "nullable": true } });
    assert_eq!(
        reason(s.call("defs.set", p.clone())),
        "1 level stores a value for level field music, whose type changes"
    );
    let mut biome = s
        .call(
            "defs.get",
            json!({ "kind": "level_field", "slug": "biome" }),
        )
        .unwrap()["def"]
        .clone();
    biome["enum"] = json!("facing");
    reason(s.call("defs.set", json!({ "kind": "level_field", "def": biome })));
    assert_eq!(
        reason(s.call(
            "defs.remove",
            json!({ "kind": "level_field", "slug": "music" })
        )),
        "removes level field music and the value 1 level stores; undo brings it back"
    );
    // A new level field at an index, then a rename that rewrites the rule
    // group requiring biome.
    let r = s
        .call(
            "defs.set",
            json!({ "kind": "level_field", "index": 0, "def": { "slug": "ambience", "type": "float", "default": 0.5 } }),
        )
        .unwrap();
    assert_eq!(r["label"], "claude-code: Set level field ambience");
    assert_eq!(
        r["changes"],
        json!([{ "change": "project", "key": "level_fields" }])
    );
    assert_eq!(s.project().level_fields[0].slug.as_str(), "ambience");
    let r = s
        .call(
            "defs.rename",
            json!({ "kind": "level_field", "from": "biome", "to": "zone" }),
        )
        .unwrap();
    let changes = r["changes"].as_array().unwrap();
    assert!(changes.contains(
        &json!({ "change": "level_meta", "world": "main", "level": "crevasse", "part": "fields" })
    ));
    assert!(changes.contains(&json!({ "change": "def", "kind": "rules", "slug": "main_tiling" })));
    assert!(
        s.project()
            .level("crevasse")
            .unwrap()
            .fields
            .contains_key("zone")
    );
}

#[test]
fn duplicate_field_is_a_command_error_with_owner_and_field() {
    let (_t, mut s) = session();
    let e = s
        .call(
            "defs.rename",
            json!({ "kind": "entity_field", "entity": "floor_button", "from": "latching", "to": "channel" }),
        )
        .unwrap_err();
    assert_eq!(e.code, code::COMMAND_ERROR);
    assert_eq!(
        e.message,
        "entity type \"floor_button\" already has a field \"channel\""
    );
    assert_eq!(
        e.data.unwrap(),
        json!({ "kind": "duplicate_field", "owner": "entity type \"floor_button\"", "field": "channel",
                "message": "entity type \"floor_button\" already has a field \"channel\"" })
    );
    let e = s
        .call(
            "defs.rename",
            json!({ "kind": "level_field", "from": "music", "to": "biome" }),
        )
        .unwrap_err();
    assert_eq!(
        (
            e.data.as_ref().unwrap()["kind"].clone(),
            e.data.unwrap()["owner"].clone()
        ),
        (json!("duplicate_field"), json!("project level_fields"))
    );
    assert!(s.history().undo_entries().is_empty());
}

#[test]
fn describe_has_palette_data() {
    let (_t, mut s) = session();
    let d = s.call("project.describe", json!({})).unwrap();
    let button = &d["entity_types"][0];
    assert_eq!(button["color"], "#c04040");
    assert!(button.get("icon").is_none() && button.get("limit").is_none());
    let door = &d["entity_types"][1];
    assert_eq!(door["color"], "#c08040");
    assert_eq!(
        door["icon"],
        json!({ "tileset": "castle_main", "tile": [3, 7] })
    );
    assert_eq!(
        door["limit"],
        json!({ "max": 4, "scope": "level", "behavior": "discard_oldest" })
    );
    // Defaults are spelled out.
    let mut def = s
        .call(
            "defs.get",
            json!({ "kind": "entity", "slug": "floor_button" }),
        )
        .unwrap()["def"]
        .clone();
    def["limit"] = json!({ "max": 1 });
    s.call("defs.set", json!({ "kind": "entity", "def": def }))
        .unwrap();
    let d = s.call("project.describe", json!({})).unwrap();
    assert_eq!(
        d["entity_types"][0]["limit"],
        json!({ "max": 1, "scope": "level", "behavior": "prevent_adding" })
    );
}
