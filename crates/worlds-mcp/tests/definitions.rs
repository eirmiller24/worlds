//! `edit_definitions`, headless (every write on disk after the call): patch
//! order, `confirm` passthrough, a refused write mid-patch cancelling the
//! whole call, level fields, and the same edits over an editor's socket.

mod common;

use std::path::Path;

use common::{engine, err, mini, ok};
use serde_json::{Value, json};
use worlds_mcp::backend::{Backend, EditorBackend};
use worlds_server::ServerInfo;
use worlds_server::headless;

fn read(dir: &Path, file: &str) -> String {
    std::fs::read_to_string(dir.join(file)).unwrap_or_default()
}

fn json_file(dir: &Path, file: &str) -> Value {
    serde_json::from_str(&read(dir, file)).unwrap()
}

/// The definition files and the level files an edit can touch.
const FILES: [&str; 7] = [
    "project.json",
    "enums.json",
    "entities/floor_button.json",
    "entities/locked_door.json",
    "worlds/main/levels/crevasse/entities.json",
    "worlds/main/levels/crevasse/level.json",
    "worlds/main/levels/shaft/entities.json",
];

fn snapshot(dir: &Path) -> Vec<String> {
    FILES.iter().map(|f| read(dir, f)).collect()
}

fn instance(dir: &Path, id: &str) -> Value {
    let v = json_file(dir, "worlds/main/levels/crevasse/entities.json");
    v["layers"][0]["entities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == id)
        .unwrap()
        .clone()
}

#[test]
fn renames_go_first_so_sets_can_use_the_new_names() {
    let (_t, dir) = mini();
    let mut e = engine(&dir, true);
    // Listed in the opposite order of how they apply.
    let r = ok(
        &mut e,
        "edit_definitions",
        json!({ "patch": {
            "set_entity_fields": [ { "entity": "pressure_plate", "def": { "slug": "delay", "type": "float", "default": 0, "min": 0 }, "index": 1 } ],
            "rename_entity_fields": [ { "entity": "pressure_plate", "from": "channel", "to": "circuit" } ],
            "rename_entity_types": [ { "from": "floor_button", "to": "pressure_plate" } ] } }),
    );
    assert_eq!(r["applied"], true);
    assert_eq!(r["saved"], true);
    assert_eq!(r["undo_label"], "Agent: edit_definitions pressure_plate");
    assert!(!dir.join("entities/floor_button.json").exists());
    let def = json_file(&dir, "entities/pressure_plate.json");
    let slugs: Vec<&str> = def["fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["slug"].as_str().unwrap())
        .collect();
    assert_eq!(slugs, ["circuit", "delay", "latching", "target"]);
    let btn = instance(&dir, "btncrev222");
    assert_eq!(btn["type"], "pressure_plate");
    assert_eq!(btn["fields"]["circuit"], 3);

    // One undo entry takes all of it back.
    let u = ok(&mut e, "undo", json!({}));
    assert_eq!(
        u["undone"],
        json!(["Agent: edit_definitions pressure_plate"])
    );
    assert!(dir.join("entities/floor_button.json").exists());
    assert!(!dir.join("entities/pressure_plate.json").exists());
    assert_eq!(instance(&dir, "btncrev222")["fields"]["channel"], 3);
}

#[test]
fn removals_need_confirm_which_is_passed_through() {
    let (_t, dir) = mini();
    let mut e = engine(&dir, true);
    let before = snapshot(&dir);
    let p =
        json!({ "remove_entity_fields": [ { "entity": "locked_door", "field": "needs_item" } ] });
    let m = err(&mut e, "edit_definitions", json!({ "patch": p }));
    assert!(m.starts_with("nothing was changed: this needs \"confirm\": true (removes field needs_item of locked_door and the value 1 entity stores; undo brings it back)"), "{m}");
    assert_eq!(snapshot(&dir), before);
    let r = ok(
        &mut e,
        "edit_definitions",
        json!({ "patch": p, "confirm": true, "label": "drop keys" }),
    );
    assert_eq!(r["undo_label"], "Agent: edit_definitions drop keys");
    assert!(
        instance(&dir, "doorcrev22")["fields"]
            .get("needs_item")
            .is_none()
    );
    let door = json_file(&dir, "entities/locked_door.json");
    assert!(
        door["fields"]
            .as_array()
            .unwrap()
            .iter()
            .all(|f| f["slug"] != "needs_item")
    );

    // confirm doesn't make the server ask for less: a rename never needs it,
    // and a set that needs none applies with or without it.
    ok(
        &mut e,
        "edit_definitions",
        json!({ "patch": { "rename_entity_fields": [ { "entity": "locked_door", "from": "tint", "to": "colour" } ] } }),
    );
    ok(
        &mut e,
        "edit_definitions",
        json!({ "confirm": true, "patch": { "set_entity_fields": [ { "entity": "locked_door", "def": { "slug": "opened", "type": "bool", "default": false } } ] } }),
    );
    assert_eq!(instance(&dir, "doorcrev22")["fields"]["colour"], "#ff8080");
}

#[test]
fn a_needs_confirm_mid_patch_cancels_the_whole_call() {
    let (_t, dir) = mini();
    let mut e = engine(&dir, true);
    let before = snapshot(&dir);
    // The enum is created first, then the retype of a stored field asks.
    let m = err(
        &mut e,
        "edit_definitions",
        json!({ "patch": {
            "set_enums": [ { "slug": "key", "values": [ { "slug": "brass_key" }, { "slug": "iron_key" } ] } ],
            "set_entity_fields": [ { "entity": "locked_door", "def": { "slug": "needs_item", "type": "enum", "enum": "key", "nullable": true } } ] } }),
    );
    assert!(
        m.contains("1 entity stores a value for field needs_item, whose type changes"),
        "{m}"
    );
    assert_eq!(snapshot(&dir), before, "nothing reaches the disk");
    let d = ok(&mut e, "describe_project", json!({}));
    assert_eq!(
        d["enums"].as_array().unwrap().len(),
        2,
        "the enum was reverted"
    );
    let u = ok(&mut e, "undo", json!({}));
    assert_eq!(u["undone"], json!([]), "no undo entry: {u}");

    // The same call with confirm applies all of it as one entry.
    let r = ok(
        &mut e,
        "edit_definitions",
        json!({ "confirm": true, "patch": {
            "set_enums": [ { "slug": "key", "values": [ { "slug": "brass_key" }, { "slug": "iron_key" } ] } ],
            "set_entity_fields": [ { "entity": "locked_door", "def": { "slug": "needs_item", "type": "enum", "enum": "key", "nullable": true } } ] } }),
    );
    assert_eq!(r["undo_label"], "Agent: edit_definitions key, locked_door");
    assert!(read(&dir, "enums.json").contains("iron_key"));
    assert_eq!(
        instance(&dir, "doorcrev22")["fields"]["needs_item"],
        "brass_key"
    );
}

#[test]
fn any_refused_write_cancels_the_call() {
    let (_t, dir) = mini();
    let mut e = engine(&dir, true);
    let before = snapshot(&dir);
    let m = err(
        &mut e,
        "edit_definitions",
        json!({ "patch": {
            "rename_enum_values": [ { "enum": "facing", "from": "right", "to": "east" } ],
            "rename_entity_fields": [ { "entity": "locked_door", "from": "needs_item", "to": "consumes" } ] } }),
    );
    assert_eq!(
        m,
        "entity type \"locked_door\" already has a field \"consumes\""
    );
    assert_eq!(snapshot(&dir), before);
    assert_eq!(instance(&dir, "doorcrev22")["fields"]["facing"], "right");
}

#[test]
fn level_fields_enums_and_types() {
    let (_t, dir) = mini();
    let mut e = engine(&dir, true);
    let r = ok(
        &mut e,
        "edit_definitions",
        json!({ "patch": {
            "rename_level_fields": [ { "from": "biome", "to": "zone" } ],
            "set_level_fields": [ { "def": { "slug": "ambience", "type": "float", "default": 0.5, "min": 0, "max": 1 }, "index": 0 } ] } }),
    );
    assert_eq!(r["undo_label"], "Agent: edit_definitions level fields");
    let project = json_file(&dir, "project.json");
    let slugs: Vec<&str> = project["level_fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["slug"].as_str().unwrap())
        .collect();
    assert_eq!(slugs, ["ambience", "zone", "music"]);
    let level = json_file(&dir, "worlds/main/levels/crevasse/level.json");
    assert_eq!(level["fields"]["zone"], json!(["flooded"]));
    assert!(read(&dir, "rules/main_tiling.json").contains("\"zone\""));
    let m = err(
        &mut e,
        "edit_definitions",
        json!({ "patch": { "remove_level_fields": ["music"] } }),
    );
    assert!(
        m.contains("removes level field music and the value 1 level stores"),
        "{m}"
    );
    ok(
        &mut e,
        "edit_definitions",
        json!({ "patch": { "remove_level_fields": ["music"] }, "confirm": true }),
    );
    assert!(!read(&dir, "project.json").contains("music"));

    // An entity file as the resource shows it ($schema, format) goes back in
    // whole; enums are renamed, set and removed; a type is removed.
    let mut door = json_file(&dir, "entities/locked_door.json");
    assert!(door.get("$schema").is_some());
    door["doc"] = json!("A door that opens with a key.");
    // Sets run after renames, so a whole type uses the new enum name.
    for f in door["fields"].as_array_mut().unwrap() {
        if f["enum"] == "facing" {
            f["enum"] = json!("side");
        }
    }
    let r = ok(
        &mut e,
        "edit_definitions",
        json!({ "confirm": true, "patch": {
            "rename_enums": [ { "from": "facing", "to": "side" } ],
            "rename_enum_values": [ { "enum": "side", "from": "right", "to": "east" } ],
            "set_entity_types": [ door ],
            "set_enums": [ { "slug": "mood", "values": [ { "slug": "calm" } ] } ],
            "remove_enums": ["biome"],
            "remove_entity_types": ["floor_button"] } }),
    );
    assert_eq!(
        r["undo_label"],
        "Agent: edit_definitions side, mood, locked_door, biome, floor_button"
    );
    assert!(read(&dir, "entities/locked_door.json").contains("A door that opens with a key."));
    assert!(read(&dir, "entities/locked_door.json").contains("\"enum\": \"side\""));
    assert_eq!(instance(&dir, "doorcrev22")["fields"]["facing"], "east");
    assert!(!dir.join("entities/floor_button.json").exists());
    let enums = read(&dir, "enums.json");
    assert!(
        enums.contains("mood") && !enums.contains("\"biome\""),
        "{enums}"
    );
}

#[test]
fn argument_errors() {
    let (_t, dir) = mini();
    let mut e = engine(&dir, true);
    assert!(err(&mut e, "edit_definitions", json!({ "patch": {} })).contains("patch is empty"));
    assert!(
        err(
            &mut e,
            "edit_definitions",
            json!({ "patch": { "set_entity_fields": [ { "entity": "locked_door", "def": { "type": "int" } } ] } })
        )
        .contains("every set_entity_fields def entry needs a slug")
    );
    assert!(
        err(
            &mut e,
            "edit_definitions",
            json!({ "patch": { "set_fields": [] } })
        )
        .contains("set_fields")
    );
}

/// The same patch headless and over an editor's socket ends in the same
/// files, as one history entry with the agent's label; a refused write
/// mid-patch leaves the editor's project and history untouched.
#[test]
fn editor_mode_matches_headless() {
    let patch = json!({ "confirm": true, "patch": {
        "rename_entity_types": [ { "from": "floor_button", "to": "pressure_plate" } ],
        "set_entity_fields": [ { "entity": "locked_door", "def": { "slug": "key_color", "type": "color", "default": "#ffd040" }, "index": 2 } ],
        "remove_entity_fields": [ { "entity": "locked_door", "field": "tint" } ],
        "remove_level_fields": ["music"] } });

    let (_ta, headless_dir) = mini();
    let mut h = engine(&headless_dir, true);
    let rh = ok(&mut h, "edit_definitions", patch.clone());

    let (_tb, dir) = mini();
    let server = headless::run(&dir, headless::Options::default()).unwrap();
    let info = ServerInfo::read(&dir).unwrap().unwrap();
    let mut e = engine(&dir, false);
    let re = ok(&mut e, "edit_definitions", patch);
    assert_eq!(re["mode"], "editor");
    assert_eq!(re["saved"], false);
    assert_eq!(re["rev"], 4, "four API writes");
    assert_eq!(re["undo_label"], rh["undo_label"]);
    assert_eq!(re["changes"], rh["changes"]);

    let mut other = EditorBackend::connect(&info, &dir, "observer").unwrap();
    let before = other
        .call(
            "defs.get",
            json!({ "kind": "entity", "slug": "locked_door" }),
        )
        .unwrap()["def"]
        .clone();
    // Refused mid-patch: the first write applied, then reverted.
    let m = err(
        &mut e,
        "edit_definitions",
        json!({ "patch": {
            "set_entity_fields": [ { "entity": "locked_door", "def": { "slug": "opened", "type": "bool" } } ],
            "remove_entity_types": ["pressure_plate"] } }),
    );
    assert!(m.contains("removes entity type pressure_plate"), "{m}");
    let after = other
        .call(
            "defs.get",
            json!({ "kind": "entity", "slug": "locked_door" }),
        )
        .unwrap();
    assert_eq!(after["def"], before);
    let u = other
        .call("history.undo", json!({ "n": 5, "any": true }))
        .unwrap();
    assert_eq!(
        u["steps"],
        json!([{ "label": "Agent: edit_definitions pressure_plate, locked_door, level fields",
                 "origin": { "agent": "claude-code" } }])
    );
    other.call("history.redo", json!({ "any": true })).unwrap();
    other.call("project.save", json!({})).unwrap();
    for f in [
        "project.json",
        "entities/pressure_plate.json",
        "entities/locked_door.json",
        "worlds/main/levels/crevasse/entities.json",
        "worlds/main/levels/crevasse/level.json",
    ] {
        assert_eq!(read(&dir, f), read(&headless_dir, f), "{f}");
    }
    assert!(!dir.join("entities/floor_button.json").exists());
    drop(other);
    server.stop();
}
