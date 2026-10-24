//! Non-rectangular levels over the API: `level.set_void`, `void` in the
//! reads, and grid and tile writes clipping at the void.

mod common;

use serde_json::{Value, json};
use worlds_core::command::Rect;
use worlds_server::headless::Session;
use worlds_server::rpc::code;

fn session() -> (tempfile::TempDir, Session) {
    let (tmp, dir) = common::mini();
    (tmp, Session::open(&dir).unwrap().client("claude-code"))
}

fn rows(s: &Session, level: &str) -> Vec<String> {
    let p = s.project();
    let def = p.layer("collision").unwrap();
    p.level(level)
        .unwrap()
        .int_grid("collision")
        .unwrap()
        .to_rows(def)
        .unwrap()
}

#[test]
fn set_void_shapes_a_level_and_reads_report_it() {
    let (_t, mut s) = session();
    let r = s
        .call(
            "level.set_void",
            json!({ "level": "crevasse", "void": [[0, 3, 4, 2]] }),
        )
        .unwrap();
    assert_eq!(r["label"], "claude-code: Shape level crevasse");
    assert_eq!(
        s.project().level("crevasse").unwrap().void,
        [Rect::new(0, 3, 4, 2)]
    );
    assert_eq!(&rows(&s, "crevasse")[3][..5], "....~");
    let get = s
        .call("level.get", json!({ "level": "crevasse", "layers": [] }))
        .unwrap();
    assert_eq!(get["level"]["void"], json!([[0, 3, 4, 2]]));
    let list = s.call("level.list", json!({})).unwrap();
    let crevasse = &list["levels"][0];
    assert_eq!(crevasse["void"], json!([[0, 3, 4, 2]]));
    assert!(
        list["levels"][1].get("void").is_none(),
        "shaft is a rectangle"
    );
    let d = s.call("project.describe", json!({})).unwrap();
    let levels = d["levels"].as_array().unwrap();
    let c = levels.iter().find(|l| l["slug"] == "crevasse").unwrap();
    assert_eq!(c["void"], json!([[0, 3, 4, 2]]));
    // level.find gives levels as level.list does, void included.
    let found = s.call("level.find", json!({})).unwrap();
    let f = found["levels"].as_array().unwrap();
    let c = f.iter().find(|l| l["slug"] == "crevasse").unwrap();
    assert_eq!(c["void"], json!([[0, 3, 4, 2]]));
    assert!(
        f.iter()
            .filter(|l| l["slug"] != "crevasse")
            .all(|l| l.get("void").is_none())
    );
    // A crop moves the void with the content (crevasse's walls fill its
    // box, so it already fits).
    let crop = s
        .call("level.crop", json!({ "level": "crevasse" }))
        .unwrap();
    assert!(
        crop["changes"].as_array().is_none_or(|c| c.is_empty()),
        "{crop}"
    );
    // Undo puts the walls back.
    s.call("history.undo", json!({})).unwrap();
    assert_eq!(&rows(&s, "crevasse")[3][..5], "#L..~");
    assert!(s.project().level("crevasse").unwrap().void.is_empty());

    // Refusals change nothing.
    for void in [
        json!([[18, 3, 1, 1]]),
        json!([[19, 0, 2, 1]]),
        json!([[0, 0, 20, 5]]),
    ] {
        let e = s
            .call(
                "level.set_void",
                json!({ "level": "crevasse", "void": void }),
            )
            .unwrap_err();
        assert_eq!(e.code, code::COMMAND_ERROR, "{}", e.message);
    }
    let e = s
        .call(
            "level.set_void",
            json!({ "level": "crevasse", "void": [], "restore": true }),
        )
        .unwrap_err();
    assert_eq!(e.code, code::INVALID_PARAMS);
}

#[test]
fn grid_and_tile_writes_clip_at_the_void() {
    let (_t, mut s) = session();
    s.call(
        "level.set_void",
        json!({ "level": "crevasse", "void": [[1, 1, 3, 2]] }),
    )
    .unwrap();
    // A text block over the void paints only the level's cells.
    let r = s
        .call(
            "grid.set_cells",
            json!({ "level": "crevasse", "layer": "collision", "text": "~~~~~", "origin": [0, 1] }),
        )
        .unwrap();
    assert_eq!(
        r["changes"][0]["rect"],
        json!([0, 1, 5, 1]),
        "x 0 and 4 change, 1..4 are void"
    );
    assert_eq!(&rows(&s, "crevasse")[1][..6], "~...~.");
    // Clearing in the void is allowed (it cleans up a file).
    let r: Value = s
        .call(
            "grid.line",
            json!({ "level": "crevasse", "layer": "collision", "from": [1, 1], "to": [3, 1], "value": "solid" }),
        )
        .unwrap();
    assert_eq!(r["changes"], json!([]));
    // Tiles: decor_back is an 8 px layer; [2, 2] and [3, 3] are void
    // (project cell [1, 1]), [8, 2] isn't.
    let r = s
        .call(
            "tiles.set",
            json!({ "level": "crevasse", "layer": "decor_back", "tiles": [
                { "cell": [2, 2], "tileset": "castle_main", "tile": [0, 0] },
                { "cell": [8, 2], "tileset": "castle_main", "tile": [0, 0] } ] }),
        )
        .unwrap();
    assert_eq!(r["changes"][0]["rect"], json!([8, 2, 1, 1]));
    let r = s
        .call(
            "tiles.stamp",
            json!({ "level": "crevasse", "layer": "decor_back", "at": [6, 4], "tileset": "castle_main", "rect": [0, 0, 2, 1] }),
        )
        .unwrap();
    // The stamp steps 2 cells: [6, 4] is void, [8, 4] isn't.
    assert_eq!(r["changes"][0]["rect"], json!([8, 4, 1, 1]));
    let p = s.project();
    let tiles = &p
        .level("crevasse")
        .unwrap()
        .tiles_layer("decor_back")
        .unwrap()
        .tiles;
    assert!(tiles.iter().all(|t| t.cell != [2, 2] && t.cell != [6, 4]));
    // Entities refuse the void.
    let e = s
        .call(
            "entity.add",
            json!({ "level": "crevasse", "entity": { "type": "floor_button", "cell": [2, 2] } }),
        )
        .unwrap_err();
    assert_eq!(e.data.unwrap()["kind"], "invalid");
}
