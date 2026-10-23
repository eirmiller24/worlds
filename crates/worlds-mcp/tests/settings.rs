//! Project settings, per-level rule state and replacing a level through the
//! tools: `edit_definitions` `set_project`, `edit_rules` `set_level_states`
//! and `create_level` with `content` and `replace`. Headless, so every
//! write is on disk after the call.

mod common;

use std::path::Path;

use common::{engine, err, mini, ok, rows_on_disk};
use serde_json::{Value, json};
use worlds_mcp::engine::Engine;

fn json_file(dir: &Path, file: &str) -> Value {
    serde_json::from_str(&std::fs::read_to_string(dir.join(file)).unwrap()).unwrap()
}

/// A level's files as saving would write them now (the resources), so
/// hand-formatted fixture files compare equal to what undo puts back.
fn level_files(e: &mut Engine, level: &str) -> Vec<String> {
    [
        "level.json",
        "collision.json",
        "decor_back.json",
        "entities.json",
        "notes.json",
    ]
    .iter()
    .map(|f| {
        e.read_resource(&format!("worlds://levels/{level}/{f}"))
            .unwrap_or_default()
    })
    .collect()
}

fn resource(e: &mut Engine, uri: &str) -> String {
    e.read_resource(uri).unwrap()
}

#[test]
fn set_project_changes_settings_in_one_undo_entry() {
    let (_t, dir) = mini();
    let mut e = engine(&dir, true);
    let before = resource(&mut e, "worlds://project.json");
    let r = ok(
        &mut e,
        "edit_definitions",
        json!({ "patch": { "set_project": { "name": "Mini Deluxe", "seed": 7, "bg_color": null } } }),
    );
    assert_eq!(r["applied"], true);
    assert_eq!(r["undo_label"], "Agent: edit_definitions project");
    assert_eq!(
        r["changes"],
        json!([
            { "change": "project", "key": "name" },
            { "change": "project", "key": "bg_color" },
            { "change": "project", "key": "seed" }
        ])
    );
    let p = json_file(&dir, "project.json");
    assert_eq!(p["name"], "Mini Deluxe");
    assert_eq!(p["seed"], 7);
    assert!(p.get("bg_color").is_none());
    assert_eq!(p["collision_layer"], "collision");

    let u = ok(&mut e, "undo", json!({}));
    assert_eq!(u["undone"], json!(["Agent: edit_definitions project"]));
    assert_eq!(resource(&mut e, "worlds://project.json"), before);
    assert_eq!(json_file(&dir, "project.json")["name"], "Mini");
}

#[test]
fn set_project_goes_after_the_layer_sets_and_a_refusal_cancels_the_call() {
    let (_t, dir) = mini();
    let mut e = engine(&dir, true);
    // A new int_grid layer becomes the collision layer in the same call.
    let r = ok(
        &mut e,
        "edit_definitions",
        json!({ "patch": {
            "set_project": { "collision_layer": "walls2" },
            "set_layers": [ { "def": { "slug": "walls2", "name": "Walls 2", "kind": "int_grid",
                "values": [ { "slug": "rock", "name": "Rock", "color": "#808080", "glyph": "r" } ] } } ] } }),
    );
    assert_eq!(r["undo_label"], "Agent: edit_definitions walls2, project");
    assert_eq!(json_file(&dir, "project.json")["collision_layer"], "walls2");
    let before = std::fs::read_to_string(dir.join("project.json")).unwrap();
    let layers = std::fs::read_to_string(dir.join("layers.json")).unwrap();

    // An auto layer can't be the collision layer: nothing of the call stays.
    let m = err(
        &mut e,
        "edit_definitions",
        json!({ "patch": {
            "set_enums": [ { "slug": "mood", "name": "Mood", "values": [ { "slug": "calm", "name": "Calm" } ] } ],
            "set_project": { "name": "Other", "collision_layer": "main_tiling" } } }),
    );
    assert!(m.contains("main_tiling"), "{m}");
    assert_eq!(
        std::fs::read_to_string(dir.join("project.json")).unwrap(),
        before
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("layers.json")).unwrap(),
        layers
    );
    assert!(
        !std::fs::read_to_string(dir.join("enums.json"))
            .unwrap()
            .contains("mood")
    );

    let m = err(
        &mut e,
        "edit_definitions",
        json!({ "patch": { "set_project": {} } }),
    );
    assert!(m.contains("set_project is empty"), "{m}");
    let m = err(
        &mut e,
        "edit_definitions",
        json!({ "patch": { "set_project": { "grid_size": 8 } } }),
    );
    assert!(m.contains("grid_size"), "{m}");
}

#[test]
fn set_level_states_sets_and_removes_rule_state() {
    let (_t, dir) = mini();
    let mut e = engine(&dir, true);
    let shaft = resource(&mut e, "worlds://levels/shaft/level.json");
    let r = ok(
        &mut e,
        "edit_rules",
        json!({ "layer": "main_tiling", "patch": { "set_level_states": [
            { "level": "crevasse", "state": { "seed": 5, "optional_groups": ["pillars"] } },
            { "level": "shaft", "state": null } ] } }),
    );
    assert_eq!(r["undo_label"], "Agent: edit_rules main_tiling");
    assert_eq!(
        r["changes"],
        json!([
            { "change": "layer_state", "world": "main", "level": "crevasse", "layer": "main_tiling" },
            { "change": "layer_state", "world": "main", "level": "shaft", "layer": "main_tiling" }
        ])
    );
    let c = json_file(&dir, "worlds/main/levels/crevasse/level.json");
    assert_eq!(
        c["layers"]["main_tiling"],
        json!({ "seed": 5, "optional_groups": ["pillars"] })
    );
    let s = json_file(&dir, "worlds/main/levels/shaft/level.json");
    assert!(s.get("layers").is_none(), "{s}");
    // read_level shows the new state.
    let (_, text) = common::call(
        &mut e,
        "read_level",
        json!({ "level": "crevasse", "layers": [] }),
    );
    assert!(
        text.contains("rule state main_tiling: seed 5 optional_groups [pillars]"),
        "{text}"
    );

    ok(&mut e, "undo", json!({}));
    assert_eq!(resource(&mut e, "worlds://levels/shaft/level.json"), shaft);
    assert_eq!(
        json_file(&dir, "worlds/main/levels/shaft/level.json")["layers"]["main_tiling"]["seed"],
        4
    );
}

#[test]
fn set_level_states_refusals() {
    let (_t, dir) = mini();
    let mut e = engine(&dir, true);
    let before =
        std::fs::read_to_string(dir.join("worlds/main/levels/crevasse/level.json")).unwrap();
    let m = err(
        &mut e,
        "edit_rules",
        json!({ "layer": "main_tiling", "patch": { "set_level_states": [ { "level": "crevasse" } ] } }),
    );
    assert!(m.contains("needs state"), "{m}");
    let m = err(
        &mut e,
        "edit_rules",
        json!({ "layer": "main_tiling", "patch": { "set_level_states": [
            { "level": "crevasse", "state": { "optional_groups": ["walls"] } } ] } }),
    );
    assert!(m.contains("not optional"), "{m}");
    let m = err(
        &mut e,
        "edit_rules",
        json!({ "layer": "collision", "patch": { "set_level_states": [
            { "level": "crevasse", "state": { "seed": 1 } } ] } }),
    );
    assert!(m.contains("collision"), "{m}");
    // A refusal after a set that applied cancels the whole call.
    err(
        &mut e,
        "edit_rules",
        json!({ "layer": "main_tiling", "patch": { "set_level_states": [
            { "level": "crevasse", "state": { "seed": 1 } },
            { "level": "nowhere", "state": { "seed": 1 } } ] } }),
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("worlds/main/levels/crevasse/level.json")).unwrap(),
        before
    );
}

#[test]
fn create_level_with_content() {
    let (_t, dir) = mini();
    let mut e = engine(&dir, true);
    let r = ok(
        &mut e,
        "create_level",
        json!({ "slug": "annex", "name": "Annex", "size": [4, 3], "world_px": [640, 0],
            "content": { "int_grids": [ { "layer": "collision", "rows": ["####", "#..#", "####"] } ] } }),
    );
    assert_eq!(r["applied"], true);
    assert_eq!(
        rows_on_disk(&dir, "annex", "collision"),
        ["####", "#..#", "####"]
    );
    let m = err(
        &mut e,
        "create_level",
        json!({ "slug": "annex2", "name": "A", "size": [1, 1], "world_px": [0, 0], "confirm": true }),
    );
    assert!(m.contains("replace"), "{m}");
}

#[test]
fn create_level_replace_needs_confirm_keeps_metadata_and_undoes() {
    let (_t, dir) = mini();
    let mut e = engine(&dir, true);
    let before = level_files(&mut e, "crevasse");
    let args = json!({ "slug": "crevasse", "name": "Crevasse", "size": [4, 2], "world_px": [0, 0],
        "replace": true,
        "content": { "int_grids": [ { "layer": "collision", "rows": ["####", "#~~#"] } ] } });
    let m = err(&mut e, "create_level", args.clone());
    assert!(m.contains("confirm"), "{m}");
    assert!(
        m.contains("btncrev222") && m.contains("decor_back"),
        "names what would go: {m}"
    );
    assert_eq!(level_files(&mut e, "crevasse"), before);

    let mut a = args.clone();
    a["confirm"] = json!(true);
    let r = ok(&mut e, "create_level", a);
    assert_eq!(r["undo_label"], "Agent: create_level crevasse");
    assert_eq!(
        r["changes"][0],
        json!({ "change": "level_meta", "world": "main", "level": "crevasse", "part": "all" })
    );
    let l = json_file(&dir, "worlds/main/levels/crevasse/level.json");
    // What the tool has no argument for, and what was left out, stays.
    assert_eq!(l["id"], "crevasse22");
    assert_eq!(l["bg_color"], "#202020");
    assert_eq!(l["fields"]["biome"], json!(["flooded"]));
    assert_eq!(
        l["layers"]["main_tiling"],
        json!({ "seed": 8123, "optional_groups": ["flooded_decor"] })
    );
    assert_eq!(l["size"], json!([4, 2]));
    assert_eq!(
        rows_on_disk(&dir, "crevasse", "collision"),
        ["####", "#~~#"]
    );
    assert!(
        !dir.join("worlds/main/levels/crevasse/decor_back.json")
            .exists()
    );

    let u = ok(&mut e, "undo", json!({}));
    assert_eq!(u["undone"], json!(["Agent: create_level crevasse"]));
    assert_eq!(level_files(&mut e, "crevasse"), before);

    let m = err(
        &mut e,
        "create_level",
        json!({ "slug": "nowhere", "name": "N", "size": [1, 1], "world_px": [0, 0], "replace": true }),
    );
    assert!(m.contains("nowhere"), "{m}");
}

#[test]
fn create_level_replace_round_trips_read_level_json() {
    let (_t, dir) = mini();
    let mut e = engine(&dir, true);
    let before = level_files(&mut e, "crevasse");
    let r = ok(
        &mut e,
        "read_level",
        json!({ "level": "crevasse", "format": "json", "include_done_notes": true }),
    );
    let lvl = &r["level"];
    // Nothing left out: no confirm needed, and nothing changes on disk.
    ok(
        &mut e,
        "create_level",
        json!({ "slug": "crevasse", "name": lvl["name"], "size": lvl["size"], "world_px": lvl["world_px"],
            "replace": true, "content": r["content"] }),
    );
    assert_eq!(level_files(&mut e, "crevasse"), before);
}
