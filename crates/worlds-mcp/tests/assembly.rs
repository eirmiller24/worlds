//! save_assembly, place_assembly, edit_definitions of assemblies and
//! arrange_levels, headless.

mod common;

use common::{engine, err, mini, ok};
use serde_json::json;

#[test]
fn save_place_and_edit_an_assembly() {
    let (_t, dir) = mini();
    let mut e = engine(&dir, true);
    let r = ok(
        &mut e,
        "save_assembly",
        json!({ "entities": ["btncrev222", "doorcrev22"], "slug": "doorway", "name": "Doorway",
                "category": "Puzzle/Doors", "tags": ["door"] }),
    );
    assert_eq!(r["items"], 2);
    assert_eq!(r["size"], json!([7, 1]));
    assert!(
        dir.join("assemblies/doorway.json").exists(),
        "saved headless"
    );
    let d = ok(&mut e, "describe_project", json!({}));
    assert_eq!(d["assemblies"][0]["slug"], "doorway");
    assert_eq!(d["assemblies"][0]["category"], "Puzzle/Doors");
    let m = err(
        &mut e,
        "save_assembly",
        json!({ "entities": ["btncrev222"], "slug": "doorway", "name": "Doorway" }),
    );
    assert!(m.contains("already exists"), "{m}");

    let r = ok(
        &mut e,
        "place_assembly",
        json!({ "level": "crevasse", "slug": "doorway", "cell": [2, 1], "flip": "x" }),
    );
    assert_eq!(r["undo_label"], "Agent: place_assembly doorway in crevasse");
    assert_eq!(r["items"], 2);
    let door = r["ids"]["doorcrev22"].as_str().unwrap().to_string();
    let f = ok(&mut e, "find_entities", json!({ "level": "crevasse" }));
    let placed = f["entities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == door.as_str())
        .unwrap()
        .clone();
    // Mirrored: the door is on the left now.
    assert_eq!(placed["cell"], json!([2, 1]));

    ok(
        &mut e,
        "edit_definitions",
        json!({ "patch": { "rename_assemblies": [{ "from": "doorway", "to": "door_pair" }] } }),
    );
    assert!(dir.join("assemblies/door_pair.json").exists());
    let m = err(
        &mut e,
        "edit_definitions",
        json!({ "patch": { "remove_assemblies": ["door_pair"] } }),
    );
    assert!(m.contains("confirm"), "{m}");
    ok(
        &mut e,
        "edit_definitions",
        json!({ "patch": { "remove_assemblies": ["door_pair"] }, "confirm": true }),
    );
    assert!(!dir.join("assemblies/door_pair.json").exists());
}

#[test]
fn arrange_levels_aligns_them() {
    let (_t, dir) = mini();
    let mut e = engine(&dir, true);
    let r = ok(
        &mut e,
        "arrange_levels",
        json!({ "levels": ["crevasse", "shaft"], "op": "align_left" }),
    );
    assert_eq!(r["applied"], true);
    let d = ok(&mut e, "describe_project", json!({}));
    let shaft = d["levels"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["slug"] == "shaft")
        .unwrap()
        .clone();
    assert_eq!(shaft["world_px"], json!([0, 80]));
    let m = err(
        &mut e,
        "arrange_levels",
        json!({ "levels": ["crevasse", "shaft"], "op": "sideways" }),
    );
    assert!(m.contains("sideways") || m.contains("op"), "{m}");
}
