//! The annotation tools in-process, headless, on a temp copy of
//! `mini.worlds`: add_note's new properties, list_notes leaving guides
//! out, resolve_note reopening, update_note, remove_note, set_guide_group,
//! tag_levels, create_level tags, and the world.json resource.

mod common;

use common::{engine, err, mini, ok};
use serde_json::{Value, json};

fn setup() -> (tempfile::TempDir, std::path::PathBuf) {
    mini()
}

fn world_json(dir: &std::path::Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(dir.join("worlds/main/world.json")).unwrap())
        .unwrap()
}

fn world_note(dir: &std::path::Path, id: &str) -> Value {
    world_json(dir)["notes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["id"] == id)
        .cloned()
        .unwrap_or(Value::Null)
}

fn ids(v: &Value) -> Vec<String> {
    v["notes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["note"]["id"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn add_note_takes_the_new_properties() {
    let (_t, dir) = setup();
    let mut e = engine(&dir, true);
    let r = ok(
        &mut e,
        "add_note",
        json!({ "scope": { "world": "main" }, "kind": "pin", "geometry": { "px": [400, 64] },
            "text": "Pool of Life", "tags": ["landmark", "garden"], "icon": "pool", "pin_label": "Pool",
            "export": true, "links": ["level:shaft", "note:pinstatue2"], "color": "#40a0ff" }),
    );
    assert_eq!(r["undo_label"], "Agent: add_note pin in world main");
    // docs/mcp.md's "Undo labels" gives this very label as the example.
    let doc =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/mcp.md")).unwrap();
    assert!(
        doc.contains(
            "`<kind> in <level>` or `<kind> in world <world>` for `add_note` (\"Agent: add_note pin in world main\")"
        ),
        "docs/mcp.md's add_note label rule"
    );
    let n = world_note(&dir, r["id"].as_str().unwrap());
    assert_eq!(n["icon"], "pool");
    assert_eq!(n["label"], "Pool");
    assert_eq!(n["export"], true);
    assert_eq!(n["links"], json!(["level:shaft", "note:pinstatue2"]));
    assert_eq!(n["color"], "#40a0ff");
    ok(
        &mut e,
        "set_guide_group",
        json!({ "slug": "ruined", "offset": [0, 10] }),
    );
    let r = ok(
        &mut e,
        "add_note",
        json!({ "scope": { "world": "main" }, "kind": "guide", "geometry": { "points": [[0, 0], [10, 0], [10, 10]] },
            "text": "Loop", "closed": true, "group": "ruined", "status": "done" }),
    );
    let n = world_note(&dir, r["id"].as_str().unwrap());
    assert_eq!(
        (n["closed"].clone(), n["group"].clone()),
        (json!(true), json!("ruined"))
    );
    assert_eq!(n["status"], "done");
    assert!(
        err(
            &mut e,
            "add_note",
            json!({ "scope": { "level": "shaft" }, "kind": "path",
            "geometry": { "points": [[0, 0], [5, 5]] }, "text": "x", "icon": "pin" })
        )
        .contains("only for a pin")
    );
}

#[test]
fn list_notes_leaves_guides_out_unless_asked() {
    let (_t, dir) = setup();
    let mut e = engine(&dir, true);
    let r = ok(&mut e, "list_notes", json!({ "status": "all" }));
    assert_eq!(
        ids(&r),
        ["pinstatue2", "regioneast", "notecrev22", "pathcrev22"]
    );
    let r = ok(&mut e, "list_notes", json!({ "kind": "guide" }));
    assert_eq!(ids(&r), ["guidestem2"]);
    let r = ok(&mut e, "list_notes", json!({ "export": true }));
    assert_eq!(ids(&r), ["pinstatue2"]);
    ok(
        &mut e,
        "set_guide_group",
        json!({ "slug": "shifted", "offset": [0, 769] }),
    );
    for x in [0, 100] {
        ok(
            &mut e,
            "add_note",
            json!({ "scope": { "world": "main" }, "kind": "guide", "geometry": { "points": [[x, 0], [x, 100]] },
                "text": "Wall line", "tags": ["east"], "group": "shifted" }),
        );
    }
    let r = ok(&mut e, "list_notes", json!({ "group": "shifted" }));
    assert_eq!(r["notes"].as_array().unwrap().len(), 2);
    let r = ok(&mut e, "list_notes", json!({ "tag": "east" }));
    assert_eq!(ids(&r), Vec::<String>::new(), "2 guides, none listed");
}

#[test]
fn resolve_note_reopens_with_status_open() {
    let (_t, dir) = setup();
    let mut e = engine(&dir, true);
    ok(&mut e, "resolve_note", json!({ "id": "notecrev22" }));
    let r = ok(
        &mut e,
        "resolve_note",
        json!({ "id": "notecrev22", "status": "open" }),
    );
    assert_eq!(r["undo_label"], "Agent: resolve_note notecrev22");
    let r = ok(
        &mut e,
        "list_notes",
        json!({ "scope": { "level": "crevasse" } }),
    );
    assert_eq!(ids(&r), ["notecrev22"]);
}

#[test]
fn update_note_patches_moves_and_respects_locks() {
    let (_t, dir) = setup();
    let mut e = engine(&dir, true);
    // Text, tags and a move in one call: one undo entry.
    let r = ok(
        &mut e,
        "update_note",
        json!({ "id": "pinstatue2", "patch": { "text": "Statue\nIt faces the pool.", "tags": ["landmark", "forest"],
            "pin_label": "M", "move": [10, -4] } }),
    );
    assert_eq!(r["undo_label"], "Agent: update_note pinstatue2");
    let n = world_note(&dir, "pinstatue2");
    assert_eq!(n["title"], "Statue");
    assert_eq!(n["body"], "It faces the pool.");
    assert_eq!(n["label"], "M");
    assert_eq!(n["px"], json!([106, 60]));
    let u = ok(&mut e, "undo", json!({}));
    assert_eq!(u["undone"], json!(["Agent: update_note pinstatue2"]));
    assert_eq!(world_note(&dir, "pinstatue2")["px"], json!([96, 64]));
    // A rect region becomes a polygon.
    ok(
        &mut e,
        "update_note",
        json!({ "id": "regioneast", "patch": { "geometry": { "points": [[0, 0], [100, 0], [0, 100]] }, "color": null } }),
    );
    let n = world_note(&dir, "regioneast");
    assert!(n.get("rect").is_none() && n.get("color").is_none(), "{n}");
    // Locked: moving needs confirm, retitling doesn't.
    let m = err(
        &mut e,
        "update_note",
        json!({ "id": "guidestem2", "patch": { "move": [1, 0] } }),
    );
    assert!(m.contains("confirm") && m.contains("guidestem2"), "{m}");
    ok(
        &mut e,
        "update_note",
        json!({ "id": "guidestem2", "patch": { "title": "Stem" } }),
    );
    ok(
        &mut e,
        "update_note",
        json!({ "id": "guidestem2", "patch": { "move": [1, 0] }, "confirm": true }),
    );
    assert_eq!(world_note(&dir, "guidestem2")["points"][0], json!([1, 0]));
    // Argument errors.
    assert!(
        err(
            &mut e,
            "update_note",
            json!({ "id": "pinstatue2", "patch": {} })
        )
        .contains("empty")
    );
    assert!(
        err(
            &mut e,
            "update_note",
            json!({ "id": "pinstatue2", "patch": { "move": [1, 1], "geometry": { "px": [0, 0] } } })
        )
        .contains("not both")
    );
    assert!(
        err(
            &mut e,
            "update_note",
            json!({ "id": "pinstatue2", "patch": { "text": "a", "title": "b" } })
        )
        .contains("not both")
    );
}

#[test]
fn remove_note_labels_and_locks() {
    let (_t, dir) = setup();
    let mut e = engine(&dir, true);
    let m = err(&mut e, "remove_note", json!({ "ids": ["guidestem2"] }));
    assert!(m.contains("confirm"), "{m}");
    let r = ok(
        &mut e,
        "remove_note",
        json!({ "ids": ["guidestem2", "pinstatue2"], "confirm": true }),
    );
    assert_eq!(r["undo_label"], "Agent: remove_note 2 notes");
    assert_eq!(world_json(&dir)["notes"].as_array().unwrap().len(), 1);
    let r = ok(&mut e, "remove_note", json!({ "ids": ["notecrev22"] }));
    assert_eq!(r["undo_label"], "Agent: remove_note notecrev22");
}

#[test]
fn set_guide_group_keeps_what_is_not_given() {
    let (_t, dir) = setup();
    let mut e = engine(&dir, true);
    let r = ok(
        &mut e,
        "set_guide_group",
        json!({ "slug": "ruined", "name": "Ruined wing", "color": "#c08040" }),
    );
    assert_eq!(r["undo_label"], "Agent: set_guide_group ruined");
    ok(
        &mut e,
        "set_guide_group",
        json!({ "slug": "ruined", "offset": [0, 769] }),
    );
    assert_eq!(
        world_json(&dir)["guide_groups"],
        json!([{ "slug": "ruined", "name": "Ruined wing", "offset": [0, 769], "color": "#c08040" }])
    );
    ok(
        &mut e,
        "update_note",
        json!({ "id": "guidestem2", "patch": { "group": "ruined" }, "confirm": true }),
    );
    assert!(
        err(
            &mut e,
            "set_guide_group",
            json!({ "slug": "ruined", "remove": true })
        )
        .contains("used by")
    );
    assert!(
        err(
            &mut e,
            "set_guide_group",
            json!({ "slug": "ruined", "remove": true, "offset": [0, 0] })
        )
        .contains("only slug")
    );
    ok(
        &mut e,
        "update_note",
        json!({ "id": "guidestem2", "patch": { "group": null }, "confirm": true }),
    );
    ok(
        &mut e,
        "set_guide_group",
        json!({ "slug": "ruined", "remove": true }),
    );
    assert!(world_json(&dir).get("guide_groups").is_none());
}

#[test]
fn tag_levels_and_create_level_tags() {
    let (_t, dir) = setup();
    let mut e = engine(&dir, true);
    let r = ok(
        &mut e,
        "tag_levels",
        json!({ "levels": { "shaft": ["caves", "ruined"], "crevasse": ["forest"] } }),
    );
    assert_eq!(r["undo_label"], "Agent: tag_levels crevasse, shaft");
    let level = |l: &str| -> Value {
        serde_json::from_str(
            &std::fs::read_to_string(dir.join(format!("worlds/main/levels/{l}/level.json")))
                .unwrap(),
        )
        .unwrap()
    };
    assert_eq!(level("crevasse")["tags"], json!(["forest"]));
    assert_eq!(level("shaft")["tags"], json!(["caves", "ruined"]));
    let u = ok(&mut e, "undo", json!({}));
    assert_eq!(
        u["undone"].as_array().unwrap().len(),
        1,
        "one entry for both"
    );
    assert!(level("shaft").get("tags").is_none());
    assert!(err(&mut e, "tag_levels", json!({ "levels": {} })).contains("empty"));
    assert!(
        err(
            &mut e,
            "tag_levels",
            json!({ "levels": { "atlantis": ["x"] } })
        )
        .contains("atlantis")
    );
    ok(
        &mut e,
        "create_level",
        json!({ "slug": "annex", "name": "Annex", "size": [4, 4], "world_px": [640, 0], "tags": ["vault"] }),
    );
    let d = ok(&mut e, "describe_project", json!({}));
    let annex = d["levels"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["slug"] == "annex")
        .unwrap();
    assert_eq!(annex["tags"], json!(["vault"]));
    assert_eq!(d["worlds"][0]["guides"], 1);
    // A long list of slugs is cut to 60 characters.
    let mut levels = serde_json::Map::new();
    for l in ["crevasse", "shaft", "annex"] {
        levels.insert(l.into(), json!(["x"]));
    }
    let r = ok(&mut e, "tag_levels", json!({ "levels": levels }));
    assert_eq!(r["undo_label"], "Agent: tag_levels annex, crevasse, shaft");
}

#[test]
fn world_json_is_a_resource() {
    let (_t, dir) = setup();
    let mut e = engine(&dir, true);
    let list = e.list_resources().unwrap();
    assert!(
        list.iter()
            .any(|l| l.uri == "worlds://worlds/main/world.json")
    );
    let text = e.read_resource("worlds://worlds/main/world.json").unwrap();
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["notes"][0]["id"], "guidestem2");
    // Unsaved state is what's read (the same text a save would write).
    ok(&mut e, "set_guide_group", json!({ "slug": "ruined" }));
    let text = e.read_resource("worlds://worlds/main/world.json").unwrap();
    assert!(text.contains("ruined"), "{text}");
    assert!(
        e.read_resource("worlds://worlds/elsewhere/world.json")
            .unwrap_err()
            .contains("elsewhere")
    );
    for bad in [
        "worlds://worlds/main/notes.json",
        "worlds://worlds/../world.json",
        "worlds://worlds//world.json",
    ] {
        assert!(e.read_resource(bad).is_err(), "{bad}");
    }
}
