//! `shape_level`, `create_level` with `void` and `read_level` of a shaped
//! level, headless against a temp copy of `mini.worlds`.

mod common;

use common::{call, engine, mini, ok, rows_on_disk};
use serde_json::json;

#[test]
fn shape_level_cuts_out_a_corner_and_read_level_shows_it() {
    let (_t, dir) = mini();
    let mut e = engine(&dir, true);
    let r = ok(
        &mut e,
        "shape_level",
        json!({ "level": "crevasse", "void": [[16, 0, 4, 2]] }),
    );
    assert_eq!(r["applied"], true);
    assert_eq!(r["undo_label"], "Agent: shape_level crevasse");
    assert_eq!(r["changes"][0]["part"], "void");
    let rows = rows_on_disk(&dir, "crevasse", "collision");
    assert_eq!(rows[0], "################....");
    let (is_err, text) = call(&mut e, "read_level", json!({ "level": "crevasse" }));
    assert!(!is_err, "{text}");
    assert!(text.contains("void [[16, 0, 4, 2]]\n"), "{text}");
    assert!(text.contains(", L ramp_l, * void\n"), "{text}");
    assert!(text.contains("0 ################****\n"), "{text}");
    // Painting over the corner only paints the level.
    ok(
        &mut e,
        "paint",
        json!({ "level": "crevasse", "layer": "collision", "rect": [14, 0, 6, 1], "value": "water" }),
    );
    assert_eq!(
        rows_on_disk(&dir, "crevasse", "collision")[0],
        "##############~~...."
    );
    // An entity can't go there; back to a rectangle.
    let (is_err, msg) = call(
        &mut e,
        "place_entity",
        json!({ "level": "crevasse", "type": "floor_button", "cell": [18, 1] }),
    );
    assert!(is_err && msg.contains("void"), "{msg}");
    let r = ok(
        &mut e,
        "shape_level",
        json!({ "level": "crevasse", "void": [] }),
    );
    assert_eq!(
        r["undo_label"],
        "Agent: shape_level crevasse as a rectangle"
    );
    let (_, msg) = call(
        &mut e,
        "shape_level",
        json!({ "level": "crevasse", "void": [[0, 0, 20, 5]] }),
    );
    assert!(msg.contains("whole level"), "{msg}");
}

#[test]
fn create_level_takes_a_void() {
    let (_t, dir) = mini();
    let mut e = engine(&dir, true);
    ok(
        &mut e,
        "create_level",
        json!({ "slug": "ell", "name": "Ell", "size": [4, 3], "world_px": [640, 0],
            "void": [[2, 0, 2, 2]],
            "content": { "int_grids": [ { "layer": "collision", "rows": ["####", "#..#", "####"] } ] } }),
    );
    assert_eq!(
        rows_on_disk(&dir, "ell", "collision"),
        ["##..", "#...", "####"]
    );
    let j = ok(
        &mut e,
        "read_level",
        json!({ "level": "ell", "format": "json", "layers": [] }),
    );
    assert_eq!(j["level"]["void"], json!([[2, 0, 2, 2]]));
    // replace keeps the void unless given.
    ok(
        &mut e,
        "create_level",
        json!({ "slug": "ell", "name": "Ell", "size": [4, 3], "world_px": [640, 0], "replace": true, "confirm": true,
            "content": { "int_grids": [ { "layer": "collision", "rows": ["##..", "#...", "####"] } ] } }),
    );
    let j = ok(
        &mut e,
        "read_level",
        json!({ "level": "ell", "format": "json", "layers": [] }),
    );
    assert_eq!(j["level"]["void"], json!([[2, 0, 2, 2]]));
}
