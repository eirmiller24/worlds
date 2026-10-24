//! The `find` tool, headless: every kind, the limit and the errors.

mod common;

use common::{engine, err, mini, ok};
use serde_json::{Value, json};

fn ids(r: &Value, key: &str, id: &str) -> Vec<String> {
    r[key]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x[id].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn find_searches_each_kind() {
    let (_t, dir) = mini();
    let mut e = engine(&dir, true);
    let r = ok(
        &mut e,
        "find",
        json!({ "kind": "entities", "query": "tag:puzzle channel:3" }),
    );
    assert_eq!(r["kind"], "entities");
    assert_eq!(r["total"], 1);
    assert_eq!(ids(&r, "entities", "id"), ["btncrev222"]);
    let r = ok(
        &mut e,
        "find",
        json!({ "kind": "entities", "query": "type:locked_door", "limit": 1 }),
    );
    assert_eq!(
        (r["total"].clone(), ids(&r, "entities", "id")),
        (json!(2), vec!["doorcrev22".to_string()])
    );
    let r = ok(
        &mut e,
        "find",
        json!({ "kind": "levels", "query": "biome:[]" }),
    );
    assert_eq!(ids(&r, "levels", "slug"), ["shaft"]);
    // Guides only when asked for.
    let r = ok(&mut e, "find", json!({ "kind": "notes" }));
    let notes: Vec<&str> = r["notes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["note"]["id"].as_str().unwrap())
        .collect();
    assert!(!notes.contains(&"guidestem2"), "{notes:?}");
    assert_eq!(notes.len(), 4);
    let r = ok(
        &mut e,
        "find",
        json!({ "kind": "notes", "query": "kind:guide" }),
    );
    assert_eq!(r["total"], 1);
    let r = ok(
        &mut e,
        "find",
        json!({ "kind": "tiles", "query": "tileset:castle_main tile:6,1" }),
    );
    assert_eq!(r["tiles"][0]["cell"], json!([5, 2]));
    assert_eq!(r["total"], 1);

    let m = err(
        &mut e,
        "find",
        json!({ "kind": "levels", "query": "type:door" }),
    );
    assert!(m.contains("type: is for entities"), "{m}");
    let m = err(&mut e, "find", json!({ "kind": "rooms" }));
    assert!(m.contains("entities, levels, notes or tiles"), "{m}");
    let m = err(
        &mut e,
        "find",
        json!({ "kind": "notes", "query": "kind:door" }),
    );
    assert!(m.contains("annotation kinds"), "{m}");
}
