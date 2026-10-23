//! copy_fragment, paste_fragment and duplicate_level, headless.

mod common;

use common::{engine, err, mini, ok, rows_on_disk};
use serde_json::json;

#[test]
fn copy_then_paste_into_another_level() {
    let (_t, dir) = mini();
    let mut e = engine(&dir, true);
    let c = ok(
        &mut e,
        "copy_fragment",
        json!({ "entities": ["doorcrev22"], "rect": [14, 3, 2, 1] }),
    );
    let frag = c["fragment"].clone();
    assert_eq!(frag["worlds_fragment"], 1);
    assert_eq!(frag["grids"]["collision"]["rows"], json!(["ff"]));
    let r = ok(
        &mut e,
        "paste_fragment",
        json!({ "level": "shaft", "origin": [1, 3], "fragment": frag, "label": "door copy" }),
    );
    assert_eq!(r["applied"], true);
    assert_eq!(r["undo_label"], "Agent: paste_fragment door copy");
    assert_eq!(r["items"], 3);
    assert_eq!(r["problems"], json!([]));
    let new_door = r["ids"]["doorcrev22"].as_str().unwrap().to_string();
    let rows = rows_on_disk(&dir, "shaft", "collision");
    assert_eq!(&rows[3][1..3], "ff");
    let f = ok(&mut e, "find_entities", json!({ "level": "shaft" }));
    assert!(
        f["entities"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["id"] == new_door.as_str()),
        "{f}"
    );
    // A door that would land outside is refused with a problem; the cells
    // still go in.
    let r = ok(
        &mut e,
        "paste_fragment",
        json!({ "level": "shaft", "origin": [3, 3], "fragment": c["fragment"] }),
    );
    assert!(
        r["problems"][0]
            .as_str()
            .unwrap()
            .contains("would land outside"),
        "{r}"
    );
    let msg = err(
        &mut e,
        "paste_fragment",
        json!({ "level": "shaft", "origin": [0, 0], "fragment": { "nope": 1 } }),
    );
    assert!(msg.contains("fragment"), "{msg}");
}

#[test]
fn duplicate_a_level() {
    let (_t, dir) = mini();
    let mut e = engine(&dir, true);
    let r = ok(
        &mut e,
        "duplicate_level",
        json!({ "level": "crevasse", "slug": "crevasse_b" }),
    );
    assert_eq!(r["applied"], true);
    assert_eq!(r["world_px"], json!([320, 0]));
    assert!(r["id"].is_string());
    assert!(
        dir.join("worlds/main/levels/crevasse_b/level.json")
            .exists()
    );
    let d = ok(&mut e, "describe_project", json!({}));
    let names: Vec<&str> = d["levels"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|l| l["name"].as_str())
        .collect();
    assert!(names.contains(&"Crevasse copy"), "{names:?}");
    let msg = err(
        &mut e,
        "duplicate_level",
        json!({ "level": "crevasse", "slug": "shaft" }),
    );
    assert!(msg.contains("already exists"), "{msg}");
}
