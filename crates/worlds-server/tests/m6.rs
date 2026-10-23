//! M6 over the live API, in-process and headless: derived layers (effective
//! reads, `stored`, `"!"` writes and their refusal elsewhere), region rules
//! in `rules.preview`, `rules.regions`, `rules.list`, `level.get`,
//! `project.describe`, `project.bake` warnings, and the headless bake cache
//! following edits the way the editor's does.

mod common;

use serde_json::{Value, json};
use worlds_server::headless::Session;
use worlds_server::rpc::code;

fn session() -> (tempfile::TempDir, Session) {
    let (tmp, dir) = common::mini();
    (tmp, Session::open(&dir).unwrap().client("claude-code"))
}

/// The error's `data.kind` (command errors) and message.
fn command_error(r: Result<Value, worlds_server::RpcError>) -> (String, String) {
    let e = r.expect_err("expected a command error");
    assert_eq!(e.code, code::COMMAND_ERROR, "{e:?}");
    (
        e.data.as_ref().unwrap()["kind"]
            .as_str()
            .unwrap()
            .to_string(),
        e.message,
    )
}

/// A 16x6 level `hall` with a 9-wide room (x 1..9) and a 4-wide one
/// (x 11..14), a derived layer `marks`, a region rule writing a mark every
/// 3 cells along floors of rooms at least 6 wide (pass 0), and a pass 1
/// rule drawing an occupying 2x2 stamp on each mark (`docs/api.md`'s
/// walkthrough).
fn hall(s: &mut Session) {
    s.call(
        "level.create",
        json!({ "level": { "id": "hall222222", "slug": "hall", "name": "Hall", "world_px": [0, 160], "size": [16, 6], "depth": 0 },
            "content": { "int_grids": [ { "layer": "collision", "rows": [
                "################", "#.........#....#", "#.........#....#",
                "#.........#....#", "#.........#....#", "################"] } ] } }),
    )
    .unwrap();
    s.call(
        "defs.set",
        json!({ "kind": "layer", "index": 1, "def": { "slug": "marks", "name": "Marks", "kind": "int_grid",
            "derived": true, "values": [ { "slug": "mark", "name": "Mark", "color": "#e0a040", "glyph": "m" } ] } }),
    )
    .unwrap();
    s.call(
        "rules.set_stamp",
        json!({ "layer": "main_tiling", "stamp": "post",
            "def": { "slug": "post", "tileset": "castle_main", "rect": [4, 4, 2, 2], "pivot": [0, 1.0], "occupies": true } }),
    )
    .unwrap();
    s.call(
        "rules.set_group",
        json!({ "layer": "main_tiling", "group": { "slug": "room_marks", "rules": [
            { "slug": "floor_marks", "region": { "cells": ".", "min_width": 6 },
              "place": { "along": "floor", "every": 3, "margin": 1 },
              "write": { "grid": "marks", "value": "mark" } } ] } }),
    )
    .unwrap();
    s.call(
        "rules.set_group",
        json!({ "layer": "main_tiling", "group": { "slug": "posts", "pass": 1, "rules": [
            { "slug": "post_on_mark", "match": { "grid": "marks", "rows": ["m"] },
              "out": { "stamp": "post", "avoid_overlap": true } } ] } }),
    )
    .unwrap();
}

fn floor_row(s: &mut Session, stored: bool) -> String {
    s.call(
        "grid.get_cells",
        json!({ "level": "hall", "layer": "marks", "rect": [0, 4, 16, 1], "stored": stored }),
    )
    .unwrap()["rows"][0]
        .as_str()
        .unwrap()
        .to_string()
}

/// The effective grid computed from scratch, for comparing with what the
/// (incrementally kept) cache answers.
fn fresh_marks(s: &Session) -> Vec<String> {
    let p = s.project();
    let level = p.level("hall").unwrap();
    let images = worlds_rules::TilesetImages::new(s.dir(), &p.tilesets);
    let g = worlds_rules::effective_grid(p, level, "marks", &images).unwrap();
    g.to_rows(p.layer("marks").unwrap()).unwrap()
}

fn all_marks(s: &mut Session) -> Vec<String> {
    let r = s
        .call(
            "grid.get_cells",
            json!({ "level": "hall", "layer": "marks" }),
        )
        .unwrap();
    r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r.as_str().unwrap().to_string())
        .collect()
}

#[test]
fn get_cells_reads_effective_values_or_the_stored_overrides() {
    let (_t, mut s) = session();
    hall(&mut s);
    assert_eq!(floor_row(&mut s, false), "..m..m..m.......");
    assert_eq!(floor_row(&mut s, true), "................");
    // Overrides: a mark of our own, one suppressed. The cache follows the
    // edit (no re-read in between is needed for it to be current).
    s.call(
        "grid.set_cells",
        json!({ "level": "hall", "layer": "marks", "cells": [[3, 4, "mark"], [8, 4, "!"]] }),
    )
    .unwrap();
    assert_eq!(floor_row(&mut s, false), "..mm.m..........");
    let r = s
        .call(
            "grid.get_cells",
            json!({ "level": "hall", "layer": "marks", "rect": [0, 4, 16, 1], "stored": true }),
        )
        .unwrap();
    assert_eq!(r["rows"], json!(["...m....!......."]));
    // `.` means "no override" here, not "empty": the legend doesn't list it.
    assert_eq!(r["legend"], json!({ "m": "mark", "!": "!" }));
    // `stored` means nothing on a layer that isn't derived.
    let a = s
        .call(
            "grid.get_cells",
            json!({ "level": "hall", "layer": "collision", "stored": true }),
        )
        .unwrap();
    let b = s
        .call(
            "grid.get_cells",
            json!({ "level": "hall", "layer": "collision" }),
        )
        .unwrap();
    assert_eq!(a, b);
    assert!(a["legend"].get("!").is_none());
    // A text block: `.` gives the cell back to the rules, `!` suppresses.
    s.call(
        "grid.set_cells",
        json!({ "level": "hall", "layer": "marks", "text": "!.", "origin": [2, 4] }),
    )
    .unwrap();
    assert_eq!(floor_row(&mut s, true), "..!.....!.......");
    assert_eq!(floor_row(&mut s, false), ".....m..........");
    // grid.line and fill_rect take "!" and null too.
    s.call(
        "grid.line",
        json!({ "level": "hall", "layer": "marks", "from": [8, 4], "to": [9, 4], "value": null }),
    )
    .unwrap();
    s.call(
        "grid.fill_rect",
        json!({ "level": "hall", "layer": "marks", "rect": [12, 4, 2, 1], "value": "!" }),
    )
    .unwrap();
    assert_eq!(floor_row(&mut s, true), "..!.........!!..");
    assert_eq!(floor_row(&mut s, false), ".....m..m.......");
    assert_eq!(all_marks(&mut s), fresh_marks(&s));
    // Undo is byte-exact: back to no overrides, and the file goes away.
    s.call("history.undo", json!({ "n": 4 })).unwrap();
    assert_eq!(floor_row(&mut s, true), "................");
    assert_eq!(floor_row(&mut s, false), "..m..m..m.......");
    assert!(
        s.call(
            "project.read_file",
            json!({ "path": "worlds/main/levels/hall/marks.json" })
        )
        .is_err()
    );
}

#[test]
fn suppress_is_refused_on_a_layer_that_isnt_derived() {
    let (_t, mut s) = session();
    let msg = "\"!\" (suppress) only applies to derived layers; collision isn't derived";
    for (method, params) in [
        (
            "grid.set_cells",
            json!({ "level": "crevasse", "layer": "collision", "cells": [[1, 1, "!"]] }),
        ),
        (
            "grid.set_cells",
            json!({ "level": "crevasse", "layer": "collision", "text": "!", "origin": [1, 1] }),
        ),
        (
            "grid.set_cells",
            json!({ "level": "crevasse", "layer": "collision", "text": "x", "origin": [1, 1], "legend": { "x": "!" } }),
        ),
        (
            "grid.fill_rect",
            json!({ "level": "crevasse", "layer": "collision", "rect": [1, 1, 2, 1], "value": "!" }),
        ),
        (
            "grid.line",
            json!({ "level": "crevasse", "layer": "collision", "from": [1, 1], "to": [3, 1], "value": "!" }),
        ),
        (
            "grid.flood_fill",
            json!({ "level": "crevasse", "layer": "collision", "at": [1, 1], "value": "!" }),
        ),
    ] {
        let (kind, message) = command_error(s.call(method, params));
        assert_eq!(
            (kind.as_str(), message.as_str()),
            ("invalid", msg),
            "{method}"
        );
    }
    assert_eq!(s.call("level.list", json!({})).unwrap()["rev"], 0);
}

#[test]
fn regions_of_a_cell_test() {
    let (_t, mut s) = session();
    // crevasse:
    //   ####################
    //   #..................#
    //   #.......oooo.......#
    //   #L..~~~.......ff####
    //   ####################
    let r = s
        .call(
            "rules.regions",
            json!({ "level": "crevasse", "cells": "." }),
        )
        .unwrap();
    assert_eq!(r["grid"], "collision");
    let regions = r["regions"].as_array().unwrap();
    assert_eq!(regions.len(), 1);
    let a = &regions[0];
    assert_eq!(a["id"], 0);
    assert_eq!(a["bbox"], json!([1, 1, 18, 3]));
    assert_eq!(
        (a["width"].clone(), a["height"].clone()),
        (json!(18), json!(3))
    );
    assert_eq!(a["area"], 41);
    assert_eq!(
        a["edges"]["floor"],
        json!([
            [8, 1, 4],
            [1, 2, 1],
            [4, 2, 3],
            [14, 2, 5],
            [2, 3, 2],
            [7, 3, 7]
        ])
    );
    // The level's edge is no edge: the region's top row has a ceiling
    // (row 0 is solid), its ends are walls.
    assert_eq!(a["edges"]["ceiling"][0], json!([1, 1, 18]));
    assert_eq!(
        r["picture"],
        json!([
            "####################",
            "#aaaaaaaaaaaaaaaaaa#",
            "#aaaaaaa####aaaaaaa#",
            "##aa###aaaaaaa######",
            "####################"
        ])
    );
    // `along` keeps one kind of edge; `grid` names the layer; a group works
    // as a test.
    let r = s
        .call(
            "rules.regions",
            json!({ "level": "crevasse", "cells": "walls", "grid": "collision", "along": "ceiling" }),
        )
        .unwrap();
    let regions = r["regions"].as_array().unwrap();
    assert_eq!(regions.len(), 1, "{r}");
    assert_eq!(regions[0]["bbox"], json!([0, 0, 20, 5]));
    for g in regions {
        let keys: Vec<&String> = g["edges"].as_object().unwrap().keys().collect();
        assert_eq!(keys, ["ceiling"]);
    }
    // A test naming a value the grid doesn't have, a grid that isn't
    // int_grid, a malformed test.
    let (kind, _) = command_error(s.call(
        "rules.regions",
        json!({ "level": "crevasse", "cells": "lava" }),
    ));
    assert_eq!(kind, "invalid");
    let (kind, _) = command_error(s.call(
        "rules.regions",
        json!({ "level": "crevasse", "cells": ".", "grid": "main_tiling" }),
    ));
    assert_eq!(kind, "wrong_layer_kind");
    let e = s
        .call(
            "rules.regions",
            json!({ "level": "crevasse", "cells": "[.|" }),
        )
        .unwrap_err();
    assert_eq!(e.code, code::INVALID_PARAMS);
}

#[test]
fn regions_read_a_derived_grid_by_effective_value() {
    let (_t, mut s) = session();
    hall(&mut s);
    let r = s
        .call(
            "rules.regions",
            json!({ "level": "hall", "cells": "mark", "grid": "marks" }),
        )
        .unwrap();
    let bboxes: Vec<&Value> = r["regions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| &g["bbox"])
        .collect();
    assert_eq!(
        bboxes,
        [
            &json!([2, 4, 1, 1]),
            &json!([5, 4, 1, 1]),
            &json!([8, 4, 1, 1])
        ]
    );
}

#[test]
fn preview_of_region_and_occupancy_rules() {
    let (_t, mut s) = session();
    hall(&mut s);
    let r = s
        .call(
            "rules.preview",
            json!({ "layer": "main_tiling", "level": "hall", "rule": "floor_marks", "matches": true }),
        )
        .unwrap();
    assert_eq!(
        r["fires"],
        json!([{ "rule": "floor_marks", "group": "room_marks", "pass": 0, "grid": "collision",
            "cells": [[2, 4], [5, 4], [8, 4]], "count": 3, "matched": [], "matched_count": 0, "occupied": [],
            "regions": [
                { "id": 0, "bbox": [1, 1, 9, 4], "area": 36, "selected": true, "runs": [[1, 4, 9]],
                  "candidates": [[2, 4], [5, 4], [8, 4]] },
                { "id": 1, "bbox": [11, 1, 4, 4], "area": 16, "selected": false, "runs": [[11, 4, 4]],
                  "candidates": [] } ],
            "write": { "grid": "marks", "value": "mark" } }])
    );
    // `rect` keeps the regions whose bbox meets it.
    let r = s
        .call(
            "rules.preview",
            json!({ "layer": "main_tiling", "level": "hall", "rule": "floor_marks", "matches": true, "rect": [12, 1, 2, 2] }),
        )
        .unwrap();
    assert_eq!(r["fires"][0]["count"], 0);
    assert_eq!(r["fires"][0]["regions"].as_array().unwrap().len(), 1);
    assert_eq!(r["fires"][0]["regions"][0]["id"], 1);
    // Occupancy: the override at x 3 is covered by the post at x 2.
    s.call(
        "grid.set_cells",
        json!({ "level": "hall", "layer": "marks", "cells": [[3, 4, "mark"]] }),
    )
    .unwrap();
    let r = s
        .call(
            "rules.preview",
            json!({ "layer": "main_tiling", "level": "hall", "rule": "post_on_mark", "matches": true }),
        )
        .unwrap();
    assert_eq!(
        r["fires"],
        json!([{ "rule": "post_on_mark", "group": "posts", "pass": 1, "grid": "marks",
            "cells": [[2, 4], [5, 4], [8, 4]], "count": 3,
            "matched": [[3, 4]], "matched_count": 1, "occupied": [[3, 4]] }])
    );
    // Without `matches`, nothing but the firings.
    let r = s
        .call(
            "rules.preview",
            json!({ "layer": "main_tiling", "level": "hall", "rule": "post_on_mark" }),
        )
        .unwrap();
    assert!(r["fires"][0].get("occupied").is_none());
    assert!(r["fires"][0].get("matched").is_none());
}

/// More than 1000 candidates: every list is cut and the entry says so.
#[test]
fn region_preview_lists_are_truncated() {
    let (_t, mut s) = session();
    s.call(
        "level.create",
        json!({ "level": { "id": "field22222", "slug": "field", "name": "Field", "world_px": [0, 400], "size": [40, 30], "depth": 0 } }),
    )
    .unwrap();
    s.call(
        "defs.set",
        json!({ "kind": "layer", "def": { "slug": "dots", "name": "Dots", "kind": "int_grid", "derived": true,
            "values": [ { "slug": "dot", "name": "Dot", "color": "#ffffff", "glyph": "d" } ] } }),
    )
    .unwrap();
    s.call(
        "rules.set_group",
        json!({ "layer": "main_tiling", "group": { "slug": "dots", "rules": [
            { "slug": "everywhere", "region": { "cells": "." }, "place": { "across": "interior" },
              "write": { "grid": "dots", "value": "dot" } } ] } }),
    )
    .unwrap();
    let r = s
        .call(
            "rules.preview",
            json!({ "layer": "main_tiling", "level": "field", "rule": "everywhere", "matches": true }),
        )
        .unwrap();
    let f = &r["fires"][0];
    assert_eq!(f["count"], 1200);
    assert_eq!(f["cells"].as_array().unwrap().len(), 1000);
    assert_eq!(
        f["regions"][0]["candidates"].as_array().unwrap().len(),
        1000
    );
    assert_eq!(f["regions"][0]["runs"], json!([]));
    assert_eq!(f["truncated"], true);
}

#[test]
fn list_describe_and_level_get_show_m6() {
    let (_t, mut s) = session();
    hall(&mut s);
    let l = s
        .call("rules.list", json!({ "layer": "main_tiling" }))
        .unwrap();
    let groups = l["layers"][0]["groups"].as_array().unwrap();
    let marks = groups.iter().find(|g| g["slug"] == "room_marks").unwrap();
    assert_eq!(marks["pass"], 0);
    assert_eq!(
        marks["rules"],
        json!([{ "slug": "floor_marks", "active": true, "kind": "region", "write": "marks" }])
    );
    let posts = groups.iter().find(|g| g["slug"] == "posts").unwrap();
    assert_eq!(posts["pass"], 1);
    assert_eq!(
        posts["rules"],
        json!([{ "slug": "post_on_mark", "active": true }])
    );
    // requires_level_tag shows on the group.
    s.call(
        "rules.set_group",
        json!({ "layer": "main_tiling", "group": { "slug": "posts", "pass": 1,
            "requires_level_tag": { "tags": ["ruined"] }, "rules": [] } }),
    )
    .unwrap();
    let l = s
        .call("rules.list", json!({ "layer": "main_tiling" }))
        .unwrap();
    let posts = l["layers"][0]["groups"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["slug"] == "posts")
        .unwrap()
        .clone();
    assert_eq!(posts["requires_level_tag"], json!({ "tags": ["ruined"] }));

    s.call(
        "grid.set_cells",
        json!({ "level": "hall", "layer": "marks", "cells": [[3, 4, "mark"], [8, 4, "!"]] }),
    )
    .unwrap();
    let d = s.call("project.describe", json!({})).unwrap();
    let layer = |slug: &str| {
        d["layers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|l| l["slug"] == slug)
            .unwrap()
            .clone()
    };
    assert_eq!(layer("marks")["derived"], true);
    assert_eq!(layer("marks")["writer"], "main_tiling");
    assert!(layer("collision").get("derived").is_none());
    assert_eq!(layer("main_tiling")["passes"], json!([0, 1]));
    let hall = d["levels"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["slug"] == "hall")
        .unwrap();
    assert_eq!(hall["content"]["marks"], json!({ "overrides": 2 }));

    let j = s
        .call("level.get", json!({ "level": "hall", "layers": ["marks"] }))
        .unwrap();
    assert_eq!(j["derived"]["marks"]["writer"], "main_tiling");
    assert_eq!(j["derived"]["marks"]["rows"][4], "..mm.m..........");
    assert_eq!(j["derived"]["marks"]["overrides"][4], "...m....!.......");
    // Not asked for, not there; no derived layer shown, no key.
    let j = s
        .call(
            "level.get",
            json!({ "level": "hall", "layers": ["collision"] }),
        )
        .unwrap();
    assert!(j.get("derived").is_none());

    let t = s
        .call(
            "level.get",
            json!({ "level": "hall", "format": "text", "layers": ["marks"] }),
        )
        .unwrap();
    let text = t["text"].as_str().unwrap();
    assert!(
        text.contains("int_grid marks (16x6 cells of 16 px) derived by main_tiling\n"),
        "{text}"
    );
    assert!(text.contains("4 ..mm.m..........\n"), "{text}");
    assert!(text.contains("4 ...m....!.......\n"), "{text}");
}

#[test]
fn level_get_falls_back_to_the_overrides_when_the_writer_cant_bake() {
    let (_t, mut s) = session();
    hall(&mut s);
    s.call(
        "grid.set_cells",
        json!({ "level": "hall", "layer": "marks", "cells": [[3, 4, "mark"]] }),
    )
    .unwrap();
    // A tileset whose image is missing: main_tiling can't bake.
    s.call("project.save", json!({})).unwrap();
    std::fs::remove_file(s.dir().join("art/castle_main.png")).unwrap();
    s.call("project.reload", json!({ "confirm": true }))
        .unwrap();
    let t = s
        .call(
            "level.get",
            json!({ "level": "hall", "format": "text", "layers": ["marks"] }),
        )
        .unwrap();
    let text = t["text"].as_str().unwrap();
    assert!(
        text.contains("derived by main_tiling (not baked: overrides only)\n"),
        "{text}"
    );
    let j = s
        .call("level.get", json!({ "level": "hall", "layers": ["marks"] }))
        .unwrap();
    assert_eq!(j["derived"]["marks"]["rows"], Value::Null);
    assert!(j["derived"]["marks"]["error"].is_string());
    let e = s
        .call(
            "grid.get_cells",
            json!({ "level": "hall", "layer": "marks" }),
        )
        .unwrap_err();
    assert_eq!(e.code, code::COMMAND_ERROR);
    // The stored plane needs no bake.
    assert_eq!(floor_row(&mut s, true), "...m............");
}

/// `project.bake` and `project.export` keep their `warnings` field. Two
/// auto layers with tiles in one level were a warning until M7 (the old
/// Godot importer read one); the M7 importer reads both, so it is empty.
#[test]
fn bake_warnings() {
    let (_t, mut s) = session();
    let r = s
        .call("project.bake", json!({ "levels": ["crevasse"] }))
        .unwrap();
    assert_eq!(r["warnings"], json!([]));
    s.call(
        "defs.set",
        json!({ "kind": "layer", "def": { "slug": "trim", "name": "Trim", "kind": "auto", "source": "collision" } }),
    )
    .unwrap();
    s.call(
        "rules.set_group",
        json!({ "layer": "trim", "group": { "slug": "all", "rules": [
            { "slug": "on_walls", "match": { "rows": ["#"] }, "out": { "tileset": "castle_main", "tiles": [[0, 0]] } } ] } }),
    )
    .unwrap();
    let r = s.call("project.bake", json!({})).unwrap();
    let crevasse = r["levels"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["level"] == "crevasse")
        .unwrap();
    let with_tiles = crevasse["layers"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|l| l["tiles"].as_u64().unwrap() > 0)
        .count();
    assert_eq!(with_tiles, 2, "{crevasse}");
    assert_eq!(r["warnings"], json!([]));
    // `project.export` reports the same (empty) list for what it writes.
    let x = s.call("project.export", json!({})).unwrap();
    assert_eq!(x["warnings"], json!([]));
    let x = s
        .call("project.export", json!({ "levels": ["shaft"] }))
        .unwrap();
    assert_eq!(x["warnings"], json!([]));
}

/// Headless keeps its bake cache current through edits (collision strokes
/// that split a room, override paints, rule edits, a level rename) the way
/// the editor does, and every read equals a fresh bake.
#[test]
fn headless_cache_follows_edits() {
    let (_t, mut s) = session();
    hall(&mut s);
    assert_eq!(all_marks(&mut s), fresh_marks(&s));
    // Split the big room: x 1..5 (5 wide, not selected) and x 7..9.
    s.call(
        "grid.fill_rect",
        json!({ "level": "hall", "layer": "collision", "rect": [6, 1, 1, 4], "value": "solid" }),
    )
    .unwrap();
    assert_eq!(floor_row(&mut s, false), "................");
    assert_eq!(all_marks(&mut s), fresh_marks(&s));
    // Undo merges it again.
    s.call("history.undo", json!({})).unwrap();
    assert_eq!(floor_row(&mut s, false), "..m..m..m.......");
    // A rule edit with no stroke after it.
    s.call(
        "rules.set_rule",
        json!({ "layer": "main_tiling", "group": "room_marks", "rule":
            { "slug": "floor_marks", "region": { "cells": ".", "min_width": 4 },
              "place": { "along": "floor", "every": 2, "margin": 0 },
              "write": { "grid": "marks", "value": "mark" } } }),
    )
    .unwrap();
    assert_eq!(all_marks(&mut s), fresh_marks(&s));
    assert_eq!(floor_row(&mut s, false), ".m.m.m.m.m.m.m..");
    s.call(
        "grid.set_cells",
        json!({ "level": "hall", "layer": "marks", "text": "!!", "origin": [1, 4] }),
    )
    .unwrap();
    assert_eq!(all_marks(&mut s), fresh_marks(&s));
    // A rename moves the cached level.
    s.call(
        "level.update",
        json!({ "level": "hall", "slug": "great_hall" }),
    )
    .unwrap();
    let r = s
        .call(
            "grid.get_cells",
            json!({ "level": "great_hall", "layer": "marks", "rect": [0, 4, 16, 1] }),
        )
        .unwrap();
    assert_eq!(r["rows"], json!(["...m.m.m.m.m.m.."]));
}

/// Reading a derived layer bakes only its writer (and what that depends
/// on), never the level's other auto layers; so with a writer that can't
/// bake, every read retries that writer alone.
#[test]
fn a_derived_read_bakes_only_the_writer() {
    let (_t, mut s) = session();
    hall(&mut s);
    // An auto layer nothing reads; and main_tiling, the writer, can't bake
    // (its tileset image is missing).
    s.call(
        "defs.set",
        json!({ "kind": "layer", "def": { "slug": "trim", "name": "Trim", "kind": "auto", "source": "collision" } }),
    )
    .unwrap();
    s.call("project.save", json!({})).unwrap();
    std::fs::remove_file(s.dir().join("art/castle_main.png")).unwrap();
    s.call("project.reload", json!({ "confirm": true }))
        .unwrap();
    for _ in 0..2 {
        let e = s
            .call(
                "grid.get_cells",
                json!({ "level": "hall", "layer": "marks" }),
            )
            .unwrap_err();
        assert_eq!(e.code, code::COMMAND_ERROR);
        let (_, host, _) = s.parts();
        assert!(host.bakes.get("hall", "trim").is_none(), "trim baked");
        assert!(host.bakes.get("hall", "main_tiling").is_none());
    }
}

/// A level whose cache holds only effective derived grids (a derived layer
/// no layer writes, read before any bake) is still followed: an override
/// painted afterwards shows on the next read.
#[test]
fn a_level_with_only_derived_grids_cached_follows_edits() {
    let (_t, mut s) = session();
    hall(&mut s);
    s.call(
        "defs.set",
        json!({ "kind": "layer", "def": { "slug": "spots", "name": "Spots", "kind": "int_grid",
            "derived": true, "values": [ { "slug": "spot", "name": "Spot", "color": "#40a0e0", "glyph": "s" } ] } }),
    )
    .unwrap();
    let row = |s: &mut Session| {
        s.call(
            "grid.get_cells",
            json!({ "level": "hall", "layer": "spots", "rect": [0, 2, 16, 1] }),
        )
        .unwrap()["rows"][0]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert_eq!(row(&mut s), "................");
    {
        let (_, host, _) = s.parts();
        assert!(host.bakes.get("hall", "main_tiling").is_none());
        assert_eq!(host.bakes.cached_levels(), vec!["hall"]);
    }
    s.call(
        "grid.set_cells",
        json!({ "level": "hall", "layer": "spots", "cells": [[4, 2, "spot"]] }),
    )
    .unwrap();
    assert_eq!(row(&mut s), "....s...........");
}
