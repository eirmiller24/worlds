//! `level.reorder`, `world.set_layout` and `group.*` over the API, and
//! deletes taking members out of their groups.

mod common;

use serde_json::{Value, json};
use worlds_server::headless::Session;

fn session() -> (tempfile::TempDir, Session) {
    let (tmp, dir) = common::mini();
    (tmp, Session::open(&dir).unwrap().client("claude-code"))
}

fn slugs(s: &mut Session) -> Vec<String> {
    let r = s.call("level.list", json!({})).unwrap();
    r["levels"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["slug"].as_str().unwrap().to_string())
        .collect()
}

fn kinds(r: &Value) -> Vec<String> {
    r["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["change"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn reorder_and_layout_over_the_api() {
    let (_t, mut s) = session();
    assert_eq!(slugs(&mut s), ["crevasse", "shaft"]);
    // `world` comes from the first level.
    s.call("level.reorder", json!({ "order": ["shaft", "crevasse"] }))
        .unwrap();
    assert_eq!(slugs(&mut s), ["shaft", "crevasse"]);
    let e = s
        .call("level.reorder", json!({ "order": ["shaft"] }))
        .unwrap_err();
    assert!(e.message.contains("leaves out level crevasse"), "{e:?}");

    let r = s
        .call(
            "world.set_layout",
            json!({ "layout": { "mode": "vertical", "gap": 16 } }),
        )
        .unwrap();
    assert_eq!(kinds(&r), ["world_meta", "level_meta", "level_meta"]);
    let d = s.call("project.describe", json!({})).unwrap();
    assert_eq!(
        d["worlds"][0]["layout"],
        json!({ "mode": "vertical", "gap": 16 })
    );
    let l = s.call("level.list", json!({})).unwrap();
    assert_eq!(l["levels"][1]["world_px"], json!([0, 128 + 16]));
    let e = s
        .call(
            "world.set_layout",
            json!({ "layout": { "mode": "diagonal" } }),
        )
        .unwrap_err();
    assert!(e.message.contains("diagonal"), "{e:?}");
    // Undo puts the levels back where they were.
    s.call("history.undo", json!({})).unwrap();
    let l = s.call("level.list", json!({})).unwrap();
    assert_eq!(l["levels"][1]["world_px"], json!([0, 0]));
}

#[test]
fn groups_over_the_api() {
    let (_t, mut s) = session();
    let r = s
        .call(
            "group.create",
            json!({ "level": "crevasse", "members": ["btncrev222", "notecrev22"] }),
        )
        .unwrap();
    let id = r["id"].as_str().unwrap().to_string();
    assert_eq!(r["label"], "claude-code: Group 2 objects");
    let e = s
        .call(
            "group.create",
            json!({ "level": "crevasse", "members": ["doorshaft2"] }),
        )
        .unwrap_err();
    assert!(e.message.contains("not an entity or annotation"), "{e:?}");
    s.call(
        "group.update",
        json!({ "level": "crevasse", "id": id, "name": "Puzzle" }),
    )
    .unwrap();
    let g = s.call("group.list", json!({})).unwrap();
    assert_eq!(g["groups"][0]["name"], "Puzzle");
    // Removing a note drops it from the group; removing the last member
    // drops the group.
    s.call("notes.remove", json!({ "ids": ["notecrev22"] }))
        .unwrap();
    let g = s
        .call("group.list", json!({ "level": "crevasse" }))
        .unwrap();
    assert_eq!(g["groups"][0]["members"], json!(["btncrev222"]));
    s.call("entity.remove", json!({ "ids": ["btncrev222"] }))
        .unwrap();
    let g = s.call("group.list", json!({})).unwrap();
    assert_eq!(g["groups"], json!([]));
    // One undo brings back the entity and its group.
    s.call("history.undo", json!({})).unwrap();
    let g = s.call("group.list", json!({})).unwrap();
    assert_eq!(g["groups"][0]["members"], json!(["btncrev222"]));
    let e = s
        .call(
            "group.remove",
            json!({ "level": "crevasse", "ids": ["nosuchgrp2"] }),
        )
        .unwrap_err();
    assert!(e.message.contains("no group"), "{e:?}");
}
