//! `add_reference`, `update_reference`, `remove_reference` and
//! `layer_operation`, headless on a copy of `mini.worlds`.

mod common;

use common::{engine, err, mini, ok};
use serde_json::json;

#[test]
fn reference_tools_write_level_json() {
    let (_t, dir) = mini();
    let mut e = engine(&dir, true);
    let r = ok(
        &mut e,
        "add_reference",
        json!({ "scope": { "level": "crevasse" }, "image": "art/sketch.png", "px": [0, -16], "opacity": 0.5 }),
    );
    assert_eq!(r["undo_label"], "Agent: add_reference crevasse");
    let id = r["id"].as_str().unwrap().to_string();
    assert_eq!(r["changes"][0]["change"], "reference");
    let level =
        std::fs::read_to_string(dir.join("worlds/main/levels/crevasse/level.json")).unwrap();
    assert!(
        level.contains(&format!(r#""id": "{id}", "image": "art/sketch.png""#)),
        "{level}"
    );

    let r = ok(
        &mut e,
        "update_reference",
        json!({ "id": id, "patch": { "scale": 2, "locked": true } }),
    );
    assert_eq!(r["undo_label"], format!("Agent: update_reference {id}"));
    let m = err(
        &mut e,
        "update_reference",
        json!({ "id": id, "patch": { "px": [4, 4] } }),
    );
    assert!(m.contains("confirm"), "{m}");
    let m = err(&mut e, "update_reference", json!({ "id": id, "patch": {} }));
    assert!(m.contains("patch is empty"), "{m}");
    let m = err(&mut e, "remove_reference", json!({ "ids": [id] }));
    assert!(m.contains("locked"), "{m}");
    ok(
        &mut e,
        "remove_reference",
        json!({ "ids": [id], "confirm": true }),
    );
    let level =
        std::fs::read_to_string(dir.join("worlds/main/levels/crevasse/level.json")).unwrap();
    assert!(!level.contains("references"), "{level}");
    let m = err(
        &mut e,
        "add_reference",
        json!({ "scope": { "level": "crevasse", "world": "main" }, "image": "a.png", "px": [0, 0] }),
    );
    assert!(m.contains("scope"), "{m}");
}

#[test]
fn layer_operations() {
    let (_t, dir) = mini();
    let mut e = engine(&dir, true);
    let r = ok(
        &mut e,
        "layer_operation",
        json!({ "op": "duplicate", "layer": "decor_back" }),
    );
    assert_eq!(
        r["undo_label"],
        "Agent: layer_operation duplicate decor_back"
    );
    assert!(
        dir.join("worlds/main/levels/crevasse/decor_back_copy.json")
            .is_file()
    );
    let r = ok(
        &mut e,
        "layer_operation",
        json!({ "op": "move_tiles", "layer": "decor_back", "to": "decor_back_copy", "level": "crevasse", "cells": [[4, 2]] }),
    );
    assert_eq!(
        r["undo_label"],
        "Agent: layer_operation move_tiles decor_back in crevasse"
    );
    ok(
        &mut e,
        "layer_operation",
        json!({ "op": "merge_down", "layer": "decor_back_copy" }),
    );
    assert!(
        !dir.join("worlds/main/levels/crevasse/decor_back_copy.json")
            .exists()
    );
    let m = err(
        &mut e,
        "layer_operation",
        json!({ "op": "merge_down", "layer": "decor_back" }),
    );
    assert!(m.contains("auto layer"), "{m}");
    let m = err(
        &mut e,
        "layer_operation",
        json!({ "op": "move_tiles", "layer": "decor_back" }),
    );
    assert!(m.contains("move_tiles needs to"), "{m}");
    let m = err(
        &mut e,
        "layer_operation",
        json!({ "op": "duplicate", "layer": "objects", "ids": ["doorcrev22"] }),
    );
    assert!(m.contains("doesn't take ids"), "{m}");
    let m = err(
        &mut e,
        "layer_operation",
        json!({ "op": "move_entities", "ids": ["doorcrev22"], "to": "collision" }),
    );
    assert!(m.contains("int_grid"), "{m}");
}
