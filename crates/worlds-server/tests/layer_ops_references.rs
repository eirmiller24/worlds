//! Layer operations (`defs.duplicate`, `defs.merge_down`,
//! `tiles.move_to_layer`, `entity.move_to_layer`), reference images
//! (`reference.*`) and what the history records for an agent's batch.

mod common;

use std::path::PathBuf;

use serde_json::{Value, json};
use worlds_server::RpcError;
use worlds_server::headless::Session;
use worlds_server::rpc::code;

fn session() -> (tempfile::TempDir, Session) {
    let (tmp, dir) = common::mini();
    (tmp, Session::open(&dir).unwrap().client("claude-code"))
}

fn err(r: Result<Value, RpcError>) -> RpcError {
    r.expect_err("expected an error")
}

#[test]
fn duplicate_merge_and_move_are_one_entry_each() {
    let (_t, mut s) = session();
    let r = s
        .call(
            "defs.duplicate",
            json!({ "kind": "layer", "slug": "objects" }),
        )
        .unwrap();
    assert_eq!(
        r["label"],
        "claude-code: Duplicate layer objects as objects_copy"
    );
    assert!(s.project().layer("objects_copy").is_some());
    let e = err(s.call(
        "defs.duplicate",
        json!({ "kind": "entity", "slug": "door" }),
    ));
    assert_eq!(e.code, code::INVALID_PARAMS);
    // Move the door to the copy, then back.
    let r = s
        .call(
            "entity.move_to_layer",
            json!({ "ids": ["doorcrev22"], "layer": "objects_copy" }),
        )
        .unwrap();
    assert_eq!(r["label"], "claude-code: Move doorcrev22 to objects_copy");
    let on = |s: &Session, layer: &str| {
        s.project()
            .level("crevasse")
            .unwrap()
            .entities_layer(layer)
            .is_some_and(|l| l.entities.iter().any(|e| e.id.as_str() == "doorcrev22"))
    };
    assert!(on(&s, "objects_copy") && !on(&s, "objects"));
    s.call("history.undo", json!({})).unwrap();
    assert!(on(&s, "objects") && !on(&s, "objects_copy"));

    // Tiles: duplicate decor_back, move a stack, merge the copy down.
    s.call(
        "defs.duplicate",
        json!({ "kind": "layer", "slug": "decor_back", "to": "decor_front" }),
    )
    .unwrap();
    s.call(
        "tiles.move_to_layer",
        json!({ "level": "crevasse", "layer": "decor_back", "to": "decor_front", "cells": [[4, 2]] }),
    )
    .unwrap();
    let r = s
        .call(
            "defs.merge_down",
            json!({ "kind": "layer", "slug": "decor_front" }),
        )
        .unwrap();
    assert_eq!(
        r["label"],
        "claude-code: Merge decor_front down into decor_back"
    );
    assert!(s.project().layer("decor_front").is_none());
    let stack: Vec<[u32; 2]> = s
        .project()
        .level("crevasse")
        .unwrap()
        .tiles_layer("decor_back")
        .unwrap()
        .tiles
        .iter()
        .filter(|t| t.cell == [4, 2])
        .map(|t| t.tile)
        .collect();
    // The moved stack, and the copy's own copy of it on top.
    assert_eq!(stack, [[5, 1], [5, 1]]);
    let e = err(s.call(
        "defs.merge_down",
        json!({ "kind": "layer", "slug": "decor_back" }),
    ));
    assert_eq!(e.code, code::COMMAND_ERROR);
}

#[test]
fn references_over_the_api() {
    let (_t, mut s) = session();
    let r = s
        .call(
            "reference.add",
            json!({ "scope": { "world": "main" }, "reference": { "image": "art/sketch.png", "px": [0, 0] } }),
        )
        .unwrap();
    let id = r["id"].as_str().unwrap().to_string();
    assert_eq!(id.len(), 10);
    assert_eq!(
        r["changes"],
        json!([{ "change": "reference", "world": "main", "level": null, "id": id }])
    );
    let l = s.call("reference.list", json!({})).unwrap();
    assert_eq!(l["references"][0]["scope"], json!({ "world": "main" }));
    // A whole entry with the id left out keeps the id.
    s.call(
        "reference.update",
        json!({ "id": id, "reference": { "image": "art/sketch.png", "px": [8, 8], "locked": true } }),
    )
    .unwrap();
    let e = err(s.call(
        "reference.update",
        json!({ "id": id, "patch": { "scale": 3 } }),
    ));
    assert_eq!(e.code, code::NEEDS_CONFIRM);
    // Opacity is fine without confirm, and null puts a key back.
    s.call(
        "reference.update",
        json!({ "id": id, "patch": { "opacity": 0.25, "locked": null } }),
    )
    .unwrap();
    let w = &s.project().worlds[0].references[0];
    assert_eq!((w.opacity, w.locked, w.px), (0.25, false, [8, 8]));
    let e = err(s.call("reference.remove", json!({ "ids": ["nosuchref2"] })));
    assert_eq!(e.code, code::COMMAND_ERROR);
    s.call("reference.remove", json!({ "ids": [id] })).unwrap();
    assert!(s.project().worlds[0].references.is_empty());
    let l = s
        .call(
            "reference.list",
            json!({ "scope": { "level": "crevasse" } }),
        )
        .unwrap();
    assert_eq!(l["references"], json!([]));
}

#[test]
fn an_agent_batch_entry_lists_its_writes_and_files() {
    let (_t, mut s) = session();
    s.call("history.begin_batch", json!({ "label": "Agent: sketch" }))
        .unwrap();
    s.call(
        "reference.add",
        json!({ "scope": { "level": "crevasse" }, "reference": { "id": "refsketch2", "image": "a.png", "px": [0, 0] } }),
    )
    .unwrap();
    s.call(
        "grid.set_cells",
        json!({ "level": "crevasse", "layer": "collision", "cells": [[3, 1, "solid"]] }),
    )
    .unwrap();
    s.call("history.end_batch", json!({})).unwrap();
    let e = s.history().undo_entries().last().unwrap();
    assert_eq!(e.label, "Agent: sketch");
    assert_eq!(
        e.parts,
        ["Add reference image", "Paint 1 cell on collision"]
    );
    assert_eq!(
        e.files,
        [
            PathBuf::from("worlds/main/levels/crevasse/level.json"),
            PathBuf::from("worlds/main/levels/crevasse/collision.json"),
        ]
    );
    // A single write is an entry of one part.
    s.call("reference.remove", json!({ "ids": ["refsketch2"] }))
        .unwrap();
    let e = s.history().undo_entries().last().unwrap();
    assert_eq!(e.parts, ["Remove reference image refsketch2"]);
}
