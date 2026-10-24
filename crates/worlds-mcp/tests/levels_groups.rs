//! `reorder_levels`, `edit_worlds` `layout` and `edit_groups`, headless on
//! a copy of `mini.worlds`.

mod common;

use common::{engine, err, mini, ok};
use serde_json::json;

#[test]
fn reorder_levels_and_strip_layouts() {
    let (_t, dir) = mini();
    let mut e = engine(&dir, true);
    let r = ok(
        &mut e,
        "reorder_levels",
        json!({ "order": ["shaft", "crevasse"] }),
    );
    assert_eq!(r["undo_label"], "Agent: reorder_levels shaft");
    let world = std::fs::read_to_string(dir.join("worlds/main/world.json")).unwrap();
    assert!(
        world.contains(r#""levels": ["shaft", "crevasse"]"#),
        "{world}"
    );
    let m = err(&mut e, "reorder_levels", json!({ "order": ["shaft"] }));
    assert!(m.contains("leaves out level crevasse"), "{m}");

    let r = ok(
        &mut e,
        "edit_worlds",
        json!({ "layout": [ { "world": "main", "mode": "horizontal", "gap": 16 } ] }),
    );
    assert_eq!(r["undo_label"], "Agent: edit_worlds main");
    let crevasse =
        std::fs::read_to_string(dir.join("worlds/main/levels/crevasse/level.json")).unwrap();
    // shaft is 6 cells (96 px) wide, then a 16 px gap.
    assert!(crevasse.contains(r#""world_px": [112, 0]"#), "{crevasse}");
    let m = err(
        &mut e,
        "edit_worlds",
        json!({ "layout": [ { "world": "main", "mode": "spiral" } ] }),
    );
    assert!(m.contains("spiral"), "{m}");
}

#[test]
fn edit_groups_creates_changes_and_removes() {
    let (_t, dir) = mini();
    let mut e = engine(&dir, true);
    let r = ok(
        &mut e,
        "edit_groups",
        json!({ "level": "crevasse", "create": [ { "members": ["btncrev222"], "name": "Gap" } ] }),
    );
    let id = r["ids"][0].as_str().unwrap().to_string();
    assert_eq!(r["changes"][0]["part"], "groups");
    ok(
        &mut e,
        "edit_groups",
        json!({ "level": "crevasse", "add": [ { "id": id, "members": ["doorcrev22", "notecrev22"] } ],
                "update": [ { "id": id, "name": "Gap puzzle" } ] }),
    );
    let level =
        std::fs::read_to_string(dir.join("worlds/main/levels/crevasse/level.json")).unwrap();
    assert!(level.contains(&format!(r#""id": "{id}""#)), "{level}");
    assert!(level.contains(r#""name": "Gap puzzle""#), "{level}");
    assert!(
        level.contains(r#""members": ["btncrev222", "doorcrev22", "notecrev22"]"#),
        "{level}"
    );
    ok(
        &mut e,
        "edit_groups",
        json!({ "level": "crevasse", "remove_members": [ { "id": id, "members": ["doorcrev22"] } ] }),
    );
    // remove_entity takes the button out of its group too.
    ok(&mut e, "remove_entity", json!({ "ids": ["btncrev222"] }));
    let level =
        std::fs::read_to_string(dir.join("worlds/main/levels/crevasse/level.json")).unwrap();
    assert!(level.contains(r#""members": ["notecrev22"]"#), "{level}");
    let m = err(
        &mut e,
        "edit_groups",
        json!({ "level": "crevasse", "add": [ { "id": "nosuchgrp2", "members": ["doorcrev22"] } ] }),
    );
    assert!(m.contains("no group"), "{m}");
    let m = err(&mut e, "edit_groups", json!({ "level": "crevasse" }));
    assert!(m.contains("nothing to do"), "{m}");
    ok(
        &mut e,
        "edit_groups",
        json!({ "level": "crevasse", "remove": [id] }),
    );
    let level =
        std::fs::read_to_string(dir.join("worlds/main/levels/crevasse/level.json")).unwrap();
    assert!(!level.contains(r#""groups":"#), "{level}");
}
