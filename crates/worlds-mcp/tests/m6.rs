//! The M6 tools in-process, headless, on a temp copy of `mini.worlds`:
//! `edit_definitions` creating and removing a layer, `edit_rules` with a
//! region rule writing a derived layer, `find_regions`, `preview_rule` on
//! region and derived-grid rules, `paint` with `"!"`, `read_level` showing
//! effective grids, `bake_and_validate` warnings.

mod common;

use common::{call, engine, err, mini, ok};
use serde_json::json;

/// The derived layer `marks` and an auto layer `pillars` whose rules write
/// marks along the floor of wide rooms (pass 0) and draw a stamp on each
/// (pass 1), through the tools.
fn author(e: &mut worlds_mcp::Engine) {
    let r = ok(
        e,
        "edit_definitions",
        json!({ "patch": { "set_layers": [
            { "def": { "slug": "marks", "name": "Marks", "kind": "int_grid", "derived": true,
                "values": [ { "slug": "mark", "name": "Mark", "color": "#e0a040", "glyph": "m" } ] }, "index": 1 },
            { "def": { "slug": "pillars", "name": "Pillars", "kind": "auto", "source": "collision" } } ] } }),
    );
    assert_eq!(r["undo_label"], "Agent: edit_definitions marks, pillars");
    assert_eq!(
        r["changes"],
        json!([ { "change": "def", "kind": "layer", "slug": "marks" },
                { "change": "def", "kind": "layer", "slug": "pillars" } ])
    );
    ok(
        e,
        "edit_rules",
        json!({ "layer": "pillars", "patch": {
            "set_stamps": [ { "stamp": { "slug": "post", "tileset": "castle_main", "rect": [4, 4, 2, 2], "pivot": [0, 1.0], "occupies": true } } ],
            "set_groups": [
                { "slug": "marks", "rules": [
                    { "slug": "floor_marks", "region": { "cells": ".", "min_width": 6 },
                      "place": { "along": "floor", "every": 3, "margin": 1 },
                      "write": { "grid": "marks", "value": "mark" } } ] },
                { "slug": "art", "pass": 1, "rules": [
                    { "slug": "post_on_mark", "match": { "grid": "marks", "rows": ["m"] },
                      "out": { "stamp": "post", "avoid_overlap": true } } ] } ] } }),
    );
}

#[test]
fn layers_are_created_and_removed_with_edit_definitions() {
    let (_t, dir) = mini();
    let mut e = engine(&dir, true);
    author(&mut e);
    let layers: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("layers.json")).unwrap()).unwrap();
    let slugs: Vec<&str> = layers["layers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["slug"].as_str().unwrap())
        .collect();
    assert_eq!(
        slugs,
        [
            "collision",
            "marks",
            "main_tiling",
            "decor_back",
            "objects",
            "pillars"
        ]
    );
    let d = ok(&mut e, "describe_project", json!({}));
    let marks = d["layers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["slug"] == "marks")
        .unwrap()
        .clone();
    assert_eq!(
        (marks["derived"].clone(), marks["writer"].clone()),
        (json!(true), json!("pillars"))
    );

    // Removing needs confirm; without it nothing changes.
    let msg = err(
        &mut e,
        "edit_definitions",
        json!({ "patch": { "remove_layers": ["pillars"] } }),
    );
    assert!(msg.contains("confirm"), "{msg}");
    assert!(dir.join("rules/pillars.json").exists());
    let r = ok(
        &mut e,
        "edit_definitions",
        json!({ "patch": { "remove_layers": ["pillars", "marks"] }, "confirm": true }),
    );
    assert_eq!(r["undo_label"], "Agent: edit_definitions pillars, marks");
    assert!(!dir.join("rules/pillars.json").exists());
    let layers = std::fs::read_to_string(dir.join("layers.json")).unwrap();
    assert!(!layers.contains("marks") && !layers.contains("pillars"));
    // One undo brings both back, rules file included.
    ok(&mut e, "undo", json!({}));
    assert!(dir.join("rules/pillars.json").exists());
    assert!(
        std::fs::read_to_string(dir.join("layers.json"))
            .unwrap()
            .contains("\"marks\"")
    );
}

#[test]
fn regions_previews_and_derived_paint() {
    let (_t, dir) = mini();
    let mut e = engine(&dir, true);
    ok(
        &mut e,
        "create_level",
        json!({ "slug": "hall", "name": "Hall", "size": [16, 6], "world_px": [0, 160] }),
    );
    ok(
        &mut e,
        "paint",
        json!({ "level": "hall", "layer": "collision", "rect": [0, 0, 16, 6], "outline": true, "value": "solid" }),
    );
    ok(
        &mut e,
        "paint",
        json!({ "level": "hall", "layer": "collision", "rect": [10, 1, 1, 4], "value": "solid" }),
    );
    author(&mut e);

    let r = ok(
        &mut e,
        "find_regions",
        json!({ "level": "hall", "cells": ".", "along": "floor" }),
    );
    assert_eq!(r["grid"], "collision");
    let regions = r["regions"].as_array().unwrap();
    assert_eq!(regions.len(), 2);
    assert_eq!(regions[0]["letter"], "a");
    assert_eq!(regions[0]["width"], 9);
    assert_eq!(regions[0]["edges"], json!({ "floor": [[1, 4, 9]] }));
    assert_eq!(regions[1]["letter"], "b");
    let pic = r["picture"].as_str().unwrap();
    assert!(pic.contains("1 #aaaaaaaaa#bbbb#\n"), "{pic}");

    // The region rule: regions, write, its pass, the region grid's picture.
    let p = ok(
        &mut e,
        "preview_rule",
        json!({ "layer": "pillars", "rule": "floor_marks", "level": "hall" }),
    );
    assert_eq!(p["pass"], 0);
    assert_eq!(p["grid"], "collision");
    assert_eq!(p["cells"], json!([[2, 4], [5, 4], [8, 4]]));
    assert_eq!(p["write"], json!({ "grid": "marks", "value": "mark" }));
    assert_eq!(p["regions"][0]["selected"], true);
    assert_eq!(p["regions"][1]["selected"], false);
    assert!(
        p["picture"]
            .as_str()
            .unwrap()
            .contains("4 #.@..@..@.#....#\n"),
        "{}",
        p["picture"]
    );

    // Paint overrides: a mark at x 3 (covered by the post at x 2), x 8
    // suppressed.
    ok(
        &mut e,
        "paint",
        json!({ "level": "hall", "layer": "marks", "text_block": { "text": "m....!", "origin": [3, 4] } }),
    );
    let p = ok(
        &mut e,
        "preview_rule",
        json!({ "layer": "pillars", "rule": "post_on_mark", "level": "hall" }),
    );
    assert_eq!(p["pass"], 1);
    assert_eq!(p["grid"], "marks");
    assert_eq!(p["cells"], json!([[2, 4], [5, 4]]));
    assert_eq!(p["occupied"], json!([[3, 4]]));
    assert!(p.get("regions").is_none() && p.get("write").is_none());
    // The picture is the derived grid's effective values, marked.
    assert!(
        p["picture"]
            .as_str()
            .unwrap()
            .contains("4 ..@?.@..........\n"),
        "{}",
        p["picture"]
    );

    // read_level shows the effective grid and the overrides.
    let (is_err, text) = call(
        &mut e,
        "read_level",
        json!({ "level": "hall", "layers": ["marks"] }),
    );
    assert!(!is_err, "{text}");
    assert!(text.contains("int_grid marks (16x6 cells of 16 px) derived by pillars\n"));
    assert!(text.contains("4 ..mm.m..........\n"), "{text}");
    assert!(text.contains("4 ...m....!.......\n"), "{text}");
    let j = ok(
        &mut e,
        "read_level",
        json!({ "level": "hall", "layers": ["marks"], "format": "json" }),
    );
    assert_eq!(j["derived"]["marks"]["writer"], "pillars");

    // "!" anywhere else is refused, and the cells and value forms take it.
    let msg = err(
        &mut e,
        "paint",
        json!({ "level": "hall", "layer": "collision", "cells": [[1, 1, "!"]] }),
    );
    assert!(msg.contains("only applies to derived layers"), "{msg}");
    ok(
        &mut e,
        "paint",
        json!({ "level": "hall", "layer": "marks", "rect": [12, 4, 2, 1], "value": "!" }),
    );
    ok(
        &mut e,
        "paint",
        json!({ "level": "hall", "layer": "marks", "cells": [[3, 4, null]] }),
    );
    let rows = common::rows_on_disk(&dir, "hall", "marks");
    assert_eq!(rows[4], "........!...!!..");
    let file = std::fs::read_to_string(dir.join("worlds/main/levels/hall/marks.json")).unwrap();
    assert!(file.contains("\"overrides\": true"), "{file}");

    // Two auto layers with tiles in hall: no warning since M7 (the Godot
    // importer reads both), but the field stays.
    let b = ok(&mut e, "bake_and_validate", json!({ "levels": ["hall"] }));
    assert_eq!(b["warnings"], json!([]), "{b}");
}
