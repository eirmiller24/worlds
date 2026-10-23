//! The tools in-process against a temp copy of `mini.worlds`, headless:
//! every write lands on disk after the call.

mod common;

use common::{call, engine, err, mini, ok, rows_on_disk};
use serde_json::{Value, json};

#[test]
fn describe_and_read_level() {
    let (_t, dir) = mini();
    let mut e = engine(&dir, true);
    let d = ok(&mut e, "describe_project", json!({}));
    assert_eq!(d["name"], "Mini");
    assert_eq!(d["mode"]["headless"], true);
    assert_eq!(d["levels"][0]["slug"], "crevasse");

    let (is_err, text) = call(&mut e, "read_level", json!({ "level": "crevasse" }));
    assert!(!is_err, "{text}");
    assert!(
        text.contains("int_grid collision (20x5 cells of 16 px)\n"),
        "{text}"
    );
    assert!(text.contains("legend: . empty, # solid, f fortified, o oneway, ~ water, L ramp_l\n"));
    assert!(text.contains("  0         1\n  01234567890123456789\n0 ####################\n"));
    assert!(text.contains("3 #L..~~~.......ff####\n"));
    // Open notes only by default.
    let all = call(
        &mut e,
        "read_level",
        json!({ "level": "crevasse", "include_done_notes": true }),
    )
    .1;
    assert!(all.len() > text.len(), "done notes add lines");
    let j = ok(
        &mut e,
        "read_level",
        json!({ "level": "shaft", "format": "json", "layers": [] }),
    );
    assert_eq!(j["level"]["slug"], "shaft");
    assert!(j["content"].is_object());
}

#[test]
fn paint_shapes_save_to_disk() {
    let (_t, dir) = mini();
    let mut e = engine(&dir, true);
    let r = ok(
        &mut e,
        "paint",
        json!({ "level": "crevasse", "layer": "collision", "cells": [[3, 1, "solid"], [8, 2, null]], "label": "two cells" }),
    );
    assert_eq!(r["applied"], true);
    assert_eq!(r["saved"], true);
    assert_eq!(r["mode"], "headless");
    assert_eq!(r["undo_label"], "Agent: paint two cells");
    assert_eq!(r["changes"][0]["rect"], json!([3, 1, 6, 2]));
    let rows = rows_on_disk(&dir, "crevasse", "collision");
    assert_eq!(rows[1], "#..#...............#");
    assert_eq!(rows[2], "#........ooo.......#");

    // rect fill, outline, text block with legend and holes, all in one call.
    let r = ok(
        &mut e,
        "paint",
        json!({ "level": "crevasse", "layer": "collision",
            "text_block": { "text": "W*W\n_", "origin": [5, 1], "legend": { "W": "water", "_": null } },
            "rect": [10, 1, 3, 3], "outline": true, "value": "fortified" }),
    );
    assert_eq!(r["undo_label"], "Agent: paint collision in crevasse");
    let rows = rows_on_disk(&dir, "crevasse", "collision");
    assert_eq!(rows[1], "#..#.~.~..fff......#");
    assert_eq!(
        &rows[2][10..13],
        "fof",
        "the outline leaves the inside alone"
    );
    assert_eq!(&rows[3][10..13], "fff");
    assert_eq!(&rows[2][5..6], ".");

    // line and flood fill.
    ok(
        &mut e,
        "paint",
        json!({ "level": "shaft", "layer": "collision", "line": { "from": [0, 7], "to": [5, 7] }, "value": "solid" }),
    );
    ok(
        &mut e,
        "paint",
        json!({ "level": "shaft", "layer": "collision", "flood_at": [0, 0], "value": "water" }),
    );
    let rows = rows_on_disk(&dir, "shaft", "collision");
    assert_eq!(rows[0], "~~~~~~");
    assert_eq!(rows[7], "######");
}

#[test]
fn paint_argument_errors() {
    let (_t, dir) = mini();
    let mut e = engine(&dir, true);
    let base = |extra: Value| {
        let mut v = json!({ "level": "crevasse", "layer": "collision" });
        v.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        v
    };
    assert!(err(&mut e, "paint", base(json!({}))).contains("at least one shape"));
    assert!(
        err(&mut e, "paint", base(json!({ "rect": [1, 1, 2, 2] }))).contains("\"value\": null")
    );
    assert!(
        err(
            &mut e,
            "paint",
            base(json!({ "cells": [[1, 1, "solid"]], "value": "solid" }))
        )
        .contains("only used with")
    );
    assert!(
        err(
            &mut e,
            "paint",
            base(json!({ "cells": [[1, 1, "solid"]], "outline": true }))
        )
        .contains("outline")
    );
    assert!(err(&mut e, "paint", base(json!({ "bogus": 1 }))).contains("unknown field"));
    // value null empties.
    ok(
        &mut e,
        "paint",
        base(json!({ "rect": [0, 0, 2, 1], "value": null })),
    );
    assert!(rows_on_disk(&dir, "crevasse", "collision")[0].starts_with("..#"));
    // API errors are tool errors with the API's message.
    let m = err(&mut e, "paint", base(json!({ "cells": [[1, 1, "lava"]] })));
    assert_eq!(m, "\"lava\" is not a value of layer \"collision\"");
    let m = err(
        &mut e,
        "paint",
        json!({ "level": "nowhere", "layer": "collision", "cells": [[1, 1, "solid"]] }),
    );
    assert!(m.contains("nowhere"), "{m}");
    let m = err(&mut e, "read_level", json!({ "level": "nowhere" }));
    assert!(m.contains("nowhere"), "{m}");
    // A later shape out of bounds fails the whole call: nothing changes.
    let before = rows_on_disk(&dir, "crevasse", "collision");
    let m = err(
        &mut e,
        "paint",
        base(
            json!({ "text_block": { "text": "~~", "origin": [1, 1] }, "rect": [18, 3, 5, 1], "value": "solid" }),
        ),
    );
    assert!(m.contains("out") || m.contains("outside"), "{m}");
    ok(&mut e, "describe_project", json!({}));
    assert_eq!(rows_on_disk(&dir, "crevasse", "collision"), before);
    assert!(err(&mut e, "frobnicate", json!({})).contains("no tool"));
}

#[test]
fn stale_rev_is_retried() {
    let (_t, dir) = mini();
    let mut e = engine(&dir, true);
    ok(&mut e, "describe_project", json!({}));
    ok(
        &mut e,
        "paint",
        json!({ "level": "crevasse", "layer": "collision", "cells": [[1, 1, "solid"]] }),
    );
    // Pretend the engine missed some revs: its next write is stale once.
    e.set_seen_rev(0);
    let r = ok(
        &mut e,
        "paint",
        json!({ "level": "crevasse", "layer": "collision", "cells": [[2, 1, "solid"]] }),
    );
    assert_eq!(r["rev"], 2);
    assert!(rows_on_disk(&dir, "crevasse", "collision")[1].starts_with("###"));
}

#[test]
fn entities_place_update_find_remove() {
    let (_t, dir) = mini();
    let mut e = engine(&dir, true);
    let r = ok(
        &mut e,
        "place_entity",
        json!({ "level": "shaft", "type": "floor_button", "cell": [1, 7], "fields": { "channel": 4 } }),
    );
    let id = r["id"].as_str().unwrap().to_string();
    assert_eq!(id.len(), 10);
    assert_eq!(r["undo_label"], "Agent: place_entity floor_button in shaft");
    let on_disk =
        std::fs::read_to_string(dir.join("worlds/main/levels/shaft/entities.json")).unwrap();
    assert!(on_disk.contains(&id));

    let r = ok(
        &mut e,
        "update_entity",
        json!({ "id": id, "patch": { "offset_px": [0, -2], "fields": { "latching": true } } }),
    );
    assert_eq!(r["undo_label"], format!("Agent: update_entity {id}"));
    let found = ok(
        &mut e,
        "find_entities",
        json!({ "type": "floor_button", "level": "shaft" }),
    );
    let ent = &found["entities"][0];
    assert_eq!(ent["cell"], json!([1, 7]));
    assert_eq!(ent["offset_px"], json!([0, -2]));
    assert_eq!(ent["fields"], json!({ "channel": 4, "latching": true }));
    ok(
        &mut e,
        "update_entity",
        json!({ "id": id, "patch": { "cell": [3, 7], "unset_fields": ["channel"] } }),
    );
    let found = ok(
        &mut e,
        "find_entities",
        json!({ "fields": { "latching": true } }),
    );
    assert_eq!(found["entities"].as_array().unwrap().len(), 2, "{found}");
    let mine = found["entities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == id.as_str())
        .unwrap()
        .clone();
    assert_eq!(mine["cell"], json!([3, 7]));
    assert_eq!(mine["fields"], json!({ "latching": true }));
    assert!(
        err(&mut e, "update_entity", json!({ "id": id, "patch": {} })).contains("patch is empty")
    );

    let r = ok(&mut e, "remove_entity", json!({ "ids": [id] }));
    assert_eq!(r["undo_label"], format!("Agent: remove_entity {id}"));
    let all = ok(&mut e, "find_entities", json!({}));
    assert!(
        all["entities"]
            .as_array()
            .unwrap()
            .iter()
            .all(|x| x["id"] != id.as_str())
    );
    let m = err(&mut e, "remove_entity", json!({ "ids": ["nosuchid22"] }));
    assert!(m.contains("nosuchid22"), "{m}");
}

#[test]
fn notes_add_list_resolve() {
    let (_t, dir) = mini();
    let mut e = engine(&dir, true);
    let r = ok(
        &mut e,
        "add_note",
        json!({ "scope": { "level": "shaft" }, "kind": "pin", "geometry": { "px": [16, 32] },
            "text": "Key goes here\nThe **brass** key.", "tags": ["todo"] }),
    );
    let id = r["id"].as_str().unwrap().to_string();
    assert_eq!(r["undo_label"], "Agent: add_note pin in shaft");
    let notes = ok(
        &mut e,
        "list_notes",
        json!({ "scope": { "level": "shaft" } }),
    );
    let n = &notes["notes"][0]["note"];
    assert_eq!(n["title"], "Key goes here");
    assert_eq!(n["body"], "The **brass** key.");
    assert_eq!(n["px"], json!([16, 32]));
    ok(&mut e, "resolve_note", json!({ "id": id }));
    let open = ok(
        &mut e,
        "list_notes",
        json!({ "scope": { "level": "shaft" } }),
    );
    assert_eq!(open["notes"], json!([]));
    let done = ok(
        &mut e,
        "list_notes",
        json!({ "scope": { "level": "shaft" }, "status": "done" }),
    );
    assert_eq!(done["notes"][0]["note"]["id"], id.as_str());
    assert!(err(&mut e, "add_note", json!({ "scope": { "level": "shaft", "world": "main" }, "kind": "note", "geometry": { "px": [0, 0] }, "text": "x" })).contains("exactly one"));
    let notes_file =
        std::fs::read_to_string(dir.join("worlds/main/levels/shaft/notes.json")).unwrap();
    assert!(notes_file.contains("\"done\""), "{notes_file}");
}

#[test]
fn undo_reverts_one_call() {
    let (_t, dir) = mini();
    let mut e = engine(&dir, true);
    let before = rows_on_disk(&dir, "crevasse", "collision");
    ok(
        &mut e,
        "paint",
        json!({ "level": "crevasse", "layer": "collision", "cells": [[1, 1, "water"]] }),
    );
    let mid = rows_on_disk(&dir, "crevasse", "collision");
    // Several API writes, one undo entry.
    ok(
        &mut e,
        "paint",
        json!({ "level": "crevasse", "layer": "collision",
        "rect": [2, 1, 2, 1], "value": "solid", "line": { "from": [5, 1], "to": [7, 1] }, "label": "ledges" }),
    );
    let u = ok(&mut e, "undo", json!({}));
    assert_eq!(u["undone"], json!(["Agent: paint ledges"]));
    assert_eq!(rows_on_disk(&dir, "crevasse", "collision"), mid);
    let u = ok(&mut e, "undo", json!({ "n": 5 }));
    assert_eq!(u["undone"].as_array().unwrap().len(), 1);
    assert_eq!(rows_on_disk(&dir, "crevasse", "collision"), before);
    let u = ok(&mut e, "undo", json!({}));
    assert_eq!(u["undone"], json!([]));
    assert!(u["stopped"].as_str().unwrap().contains("nothing"));
}

#[test]
fn tiles_and_rules() {
    let (_t, dir) = mini();
    let mut e = engine(&dir, true);
    let r = ok(
        &mut e,
        "place_tiles",
        json!({ "level": "shaft", "layer": "decor_back", "placements": [[0, 0, 1, 1], [2, 0, 3, 3, "x"]],
            "stamp": { "at": [4, 4], "rect": [0, 0, 2, 2] } }),
    );
    assert_eq!(r["undo_label"], "Agent: place_tiles decor_back in shaft");
    let tiles =
        std::fs::read_to_string(dir.join("worlds/main/levels/shaft/decor_back.json")).unwrap();
    assert!(tiles.contains("castle_main"), "{tiles}");
    ok(
        &mut e,
        "clear_tiles",
        json!({ "level": "shaft", "layer": "decor_back", "rect": [0, 0, 3, 1] }),
    );
    let text = call(
        &mut e,
        "read_level",
        json!({ "level": "shaft", "layers": ["decor_back"] }),
    )
    .1;
    assert!(text.contains("4 tiles"), "{text}");
    assert!(
        err(
            &mut e,
            "place_tiles",
            json!({ "level": "shaft", "layer": "collision", "placements": [[0, 0, 1, 1]] })
        )
        .contains("not a tiles layer")
    );

    // Rules: add a group with a rule, then a rule into it, then preview.
    let r = ok(
        &mut e,
        "edit_rules",
        json!({ "layer": "main_tiling", "patch": {
            "set_groups": [ { "slug": "accents", "rules": [ { "slug": "water_surface", "match": { "rows": [".", "~", "*"] }, "out": { "tileset": "castle_main", "tiles": [[7, 2]] } } ] } ],
            "set_rules": [ { "group": "accents", "rule": { "slug": "solid_top", "match": { "rows": [".", "#"] }, "out": { "tileset": "castle_main", "tiles": [[1, 1]] } }, "index": 0 } ] } }),
    );
    assert_eq!(r["undo_label"], "Agent: edit_rules main_tiling");
    let rules = std::fs::read_to_string(dir.join("rules/main_tiling.json")).unwrap();
    let solid = rules.find("solid_top").unwrap();
    assert!(
        solid < rules.find("water_surface").unwrap(),
        "inserted at index 0"
    );
    // Appended after the walls group, whose wall_fill takes every filled
    // cell first, so it never fires here.
    let p = ok(
        &mut e,
        "preview_rule",
        json!({ "layer": "main_tiling", "rule": "water_surface", "level": "crevasse" }),
    );
    assert_eq!(
        (p["grid"].clone(), p["count"].clone(), p["cells"].clone()),
        (json!("collision"), json!(0), json!([]))
    );
    let p = ok(
        &mut e,
        "preview_rule",
        json!({ "layer": "main_tiling", "rule": "wall_fill", "level": "crevasse" }),
    );
    let n = p["count"].as_u64().unwrap();
    assert!(
        n > 0 && n as usize == p["cells"].as_array().unwrap().len(),
        "{p}"
    );
    let pic = p["picture"].as_str().unwrap();
    assert!(
        pic.starts_with("  0         1\n  01234567890123456789\n0 "),
        "{pic}"
    );
    assert_eq!(pic.matches('@').count() as u64, n, "{pic}");
    assert!(
        err(
            &mut e,
            "preview_rule",
            json!({ "layer": "main_tiling", "rule": "nope", "level": "crevasse" })
        )
        .contains("no rule nope")
    );
    ok(
        &mut e,
        "edit_rules",
        json!({ "layer": "main_tiling", "patch": { "remove_rules": ["solid_top"] } }),
    );
    assert!(
        !std::fs::read_to_string(dir.join("rules/main_tiling.json"))
            .unwrap()
            .contains("solid_top")
    );
    ok(
        &mut e,
        "edit_rules",
        json!({ "layer": "main_tiling", "patch": { "remove_groups": ["accents"] } }),
    );
    assert!(
        !std::fs::read_to_string(dir.join("rules/main_tiling.json"))
            .unwrap()
            .contains("water_surface")
    );
    assert!(
        err(
            &mut e,
            "edit_rules",
            json!({ "layer": "main_tiling", "patch": { "remove_rules": ["nope"] } })
        )
        .contains("no rule or reference \"nope\"")
    );
}

/// Macros, references, moves and removals through `edit_rules`;
/// `preview_rule` of a reference marks fired and blocked cells;
/// `expand_rules` is `Project::expanded_rules`.
#[test]
fn templates_through_edit_rules() {
    let (_t, dir) = mini();
    let mut e = engine(&dir, true);
    ok(
        &mut e,
        "edit_rules",
        json!({ "layer": "main_tiling", "patch": {
            "set_macros": [ { "kind": "template", "name": "ledge", "def": { "legend": "walls", "rules": [
                { "slug": "top", "match": { "rows": [".", "#", "*"] }, "out": { "tiles": [[0, 0]] }, "break": true },
                { "slug": "face", "match": { "rows": ["#"] }, "out": { "tiles": [[0, 1]] } } ] } } ],
            "set_stamps": [ { "stamp": { "slug": "arch", "tileset": "castle_main", "rect": [4, 4, 3, 2] } } ],
            "set_rules": [ { "group": "walls", "rule": { "use": "ledge", "slug": "ledges", "tileset": "castle_main", "origin": [14, 0] }, "index": 0 } ] } }),
    );
    let p = ok(
        &mut e,
        "preview_rule",
        json!({ "layer": "main_tiling", "rule": "ledges", "level": "crevasse" }),
    );
    assert_eq!(
        p["rules"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["rule"].clone())
            .collect::<Vec<_>>(),
        vec![json!("ledges__top"), json!("ledges__face")],
        "{p}"
    );
    let pic = p["picture"].as_str().unwrap();
    let (fired, blocked) = (
        p["count"].as_u64().unwrap(),
        p["matched_count"].as_u64().unwrap(),
    );
    assert!(fired > 0 && blocked > 0, "{p}");
    // Cells `top` took (break) are `face`'s blocked cells, drawn as fired.
    assert_eq!(
        pic.matches('@').count(),
        p["cells"].as_array().unwrap().len(),
        "{pic}"
    );
    assert_eq!(
        pic.matches('?').count(),
        p["matched"].as_array().unwrap().len(),
        "{pic}"
    );
    // One expanded rule by its slug.
    let p = ok(
        &mut e,
        "preview_rule",
        json!({ "layer": "main_tiling", "rule": "ledges__top", "level": "crevasse" }),
    );
    assert!(
        p.get("rules").is_none() && p["count"].as_u64().unwrap() > 0,
        "{p}"
    );

    let x = ok(&mut e, "expand_rules", json!({ "layer": "main_tiling" }));
    let project = worlds_core::load(&dir).unwrap();
    let want: Vec<Value> = project
        .expanded_rules("main_tiling")
        .unwrap()
        .iter()
        .map(|g| {
            let rules: Vec<Value> = g
                .rules
                .iter()
                .map(|r| {
                    let mut v = serde_json::to_value(&*r.rule).unwrap();
                    if let Some(f) = &r.from {
                        v["from"] = json!(f);
                    }
                    v
                })
                .collect();
            json!({ "slug": g.group.slug, "rules": rules })
        })
        .collect();
    assert_eq!(x["groups"], json!(want));
    let one = ok(
        &mut e,
        "expand_rules",
        json!({ "layer": "main_tiling", "group": "walls" }),
    );
    assert_eq!(one["groups"][0]["rules"][0]["slug"], "ledges__top");
    assert_eq!(one["groups"][0]["rules"][0]["from"], "ledges");

    // Removing the template while used is refused; removals run after the
    // rest and rules go before macros, so one patch can remove both.
    assert!(
        err(
            &mut e,
            "edit_rules",
            json!({ "layer": "main_tiling", "patch": { "remove_macros": [ { "kind": "template", "name": "ledge" } ] } })
        )
        .contains("still used")
    );
    ok(
        &mut e,
        "edit_rules",
        json!({ "layer": "main_tiling", "patch": { "remove_rules": ["ledges"], "remove_stamps": ["arch"],
            "remove_macros": [ { "kind": "template", "name": "ledge" } ] } }),
    );
    let rules = std::fs::read_to_string(dir.join("rules/main_tiling.json")).unwrap();
    assert!(
        !rules.contains("ledges") && !rules.contains("arch"),
        "{rules}"
    );
    assert!(
        !std::fs::read_to_string(dir.join("rules/macros.json"))
            .unwrap()
            .contains("ledge")
    );
    assert!(
        err(
            &mut e,
            "edit_rules",
            json!({ "layer": "main_tiling", "patch": {} })
        )
        .contains("patch is empty")
    );
    let blob = e.read_resource("worlds://templates/blob47.json").unwrap();
    assert!(serde_json::from_str::<Value>(&blob).unwrap()["rules"].is_array());
    assert!(
        e.list_resources()
            .unwrap()
            .iter()
            .any(|l| l.uri == "worlds://templates/blob47.json")
    );
    assert!(e.read_resource("worlds://templates/nope.json").is_err());
}

#[test]
fn bake_and_validate_reports_problems() {
    let (_t, dir) = mini();
    let mut e = engine(&dir, true);
    let r = ok(&mut e, "bake_and_validate", json!({}));
    assert_eq!(r["problems"], json!([]));
    assert_eq!(r["bake"].as_array().unwrap().len(), 2);
    assert!(r["bake"][0]["tiles"].as_u64().unwrap() > 0);
    // A button whose target doesn't exist.
    ok(
        &mut e,
        "place_entity",
        json!({ "level": "shaft", "type": "floor_button", "cell": [1, 7], "fields": { "target": "missing222" } }),
    );
    let r = ok(&mut e, "bake_and_validate", json!({ "export": true }));
    let problems = r["problems"].as_array().unwrap();
    assert!(!problems.is_empty(), "{r}");
    assert!(
        problems[0]["message"]
            .as_str()
            .unwrap()
            .contains("missing222"),
        "{r}"
    );
    assert!(problems[0]["file"].is_string() && problems[0]["path"].is_string());
    assert!(
        r["exported"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f == "levels/shaft.json")
    );
    assert!(dir.join("export/levels/shaft.json").is_file());
}

#[test]
fn files_changed_on_disk_reload() {
    let (_t, dir) = mini();
    let mut e = engine(&dir, true);
    ok(
        &mut e,
        "read_level",
        json!({ "level": "crevasse", "format": "json" }),
    );
    let path = dir.join("worlds/main/levels/crevasse/collision.json");
    let text = std::fs::read_to_string(&path)
        .unwrap()
        .replace("#L..", "#L##");
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(&path, text).unwrap();
    let r = ok(&mut e, "describe_project", json!({}));
    assert!(
        r["notices"][0]
            .as_str()
            .unwrap()
            .contains("reloaded from disk"),
        "{r}"
    );
    let text = call(&mut e, "read_level", json!({ "level": "crevasse" })).1;
    assert!(text.contains("3 #L##~~~"), "{text}");
}

#[test]
fn resources() {
    let (_t, dir) = mini();
    let mut e = engine(&dir, true);
    let list = e.list_resources().unwrap();
    let uris: Vec<&str> = list.iter().map(|l| l.uri.as_str()).collect();
    for u in [
        "worlds://project.json",
        "worlds://defs/layers.json",
        "worlds://defs/entities/locked_door.json",
        "worlds://defs/rules/main_tiling.json",
        "worlds://levels/crevasse/collision.json",
        "worlds://levels/crevasse/entities.json",
        "worlds://levels/crevasse/notes.json",
        "worlds://levels/shaft/level.json",
        "worlds://schema/intgrid.json",
        "worlds://schema/command.json",
    ] {
        assert!(uris.contains(&u), "{u} missing from {uris:?}");
    }
    for u in &uris {
        let text = e.read_resource(u).unwrap_or_else(|m| panic!("{u}: {m}"));
        serde_json::from_str::<Value>(&text).unwrap_or_else(|x| panic!("{u}: {x}"));
    }
    let p: Value =
        serde_json::from_str(&e.read_resource("worlds://project.json").unwrap()).unwrap();
    assert_eq!(p["name"], "Mini");
    // Unsaved state is what's read: paint, then the level file shows it.
    ok(
        &mut e,
        "paint",
        json!({ "level": "crevasse", "layer": "collision", "cells": [[1, 1, "water"]] }),
    );
    let g = e
        .read_resource("worlds://levels/crevasse/collision.json")
        .unwrap();
    assert!(g.contains("#~......"), "{g}");
    for bad in [
        "worlds://defs/../project.json",
        "worlds://levels/crevasse/../../x.json",
        "file:///etc/passwd",
        "worlds://defs/project.json",
    ] {
        assert!(e.read_resource(bad).is_err(), "{bad}");
    }
    assert!(
        e.read_resource("worlds://levels/nowhere/level.json")
            .unwrap_err()
            .contains("nowhere")
    );
}

/// An outline of a huge rect is refused by the API's bounds check at once
/// (no cell-by-cell expansion), and a rect past the largest coordinate is
/// an argument error; nothing changes on disk.
#[test]
fn huge_outlines_fail_fast() {
    let (_t, dir) = mini();
    let mut e = engine(&dir, false);
    let before = rows_on_disk(&dir, "crevasse", "collision");
    let t = std::time::Instant::now();
    let m = err(
        &mut e,
        "paint",
        json!({ "level": "crevasse", "layer": "collision", "rect": [0, 0, 4000000000u32, 4000000000u32],
            "outline": true, "value": "solid" }),
    );
    assert!(m.contains("outside") || m.contains("out of"), "{m}");
    let m = err(
        &mut e,
        "paint",
        json!({ "level": "crevasse", "layer": "collision", "rect": [4294967295u32, 0, 2, 1],
            "outline": true, "value": "solid" }),
    );
    assert!(m.contains("largest coordinate"), "{m}");
    assert!(t.elapsed() < std::time::Duration::from_secs(2));
    assert_eq!(rows_on_disk(&dir, "crevasse", "collision"), before);
}

#[test]
fn create_level_then_fill_and_undo() {
    let (_t, dir) = mini();
    let mut e = engine(&dir, true);
    let r = ok(
        &mut e,
        "create_level",
        json!({ "slug": "annex", "name": "Annex", "size": [6, 3], "world_px": [416, 0],
            "fields": { "music": "cave_theme" } }),
    );
    assert_eq!(r["applied"], true, "{r}");
    assert_eq!(r["undo_label"], "Agent: create_level annex");
    assert_eq!(r["saved"], true);
    let id = r["id"].as_str().unwrap().to_string();
    assert_eq!(id.len(), 10);
    assert_eq!(
        r["changes"],
        json!([{ "change": "level_created", "world": "main", "level": "annex" }])
    );
    let level_dir = dir.join("worlds/main/levels/annex");
    let meta: Value =
        serde_json::from_str(&std::fs::read_to_string(level_dir.join("level.json")).unwrap())
            .unwrap();
    assert_eq!(meta["id"], json!(id));
    assert_eq!(meta["size"], json!([6, 3]));
    assert_eq!(meta["world_px"], json!([416, 0]));
    assert_eq!(meta["depth"], json!(0));
    assert_eq!(meta["fields"], json!({ "music": "cave_theme" }));
    let d = ok(&mut e, "describe_project", json!({}));
    let annex = d["levels"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["slug"] == "annex")
        .expect("annex in describe_project");
    assert_eq!(annex["size"], json!([6, 3]));

    // The new level takes edits like any other.
    ok(
        &mut e,
        "paint",
        json!({ "level": "annex", "layer": "collision", "rect": [0, 2, 6, 1], "value": "solid" }),
    );
    assert_eq!(
        rows_on_disk(&dir, "annex", "collision"),
        vec!["......", "......", "######"]
    );

    // Refusals name the problem and change nothing.
    let m = err(
        &mut e,
        "create_level",
        json!({ "slug": "crevasse", "name": "Again", "size": [4, 4], "world_px": [0, 400] }),
    );
    assert!(m.contains("crevasse"), "{m}");
    let m = err(
        &mut e,
        "create_level",
        json!({ "slug": "flat", "name": "Flat", "size": [4, 0], "world_px": [0, 400] }),
    );
    assert!(m.contains("at least"), "{m}");
    let m = err(
        &mut e,
        "create_level",
        json!({ "slug": "x", "name": "X", "size": [4, 4] }),
    );
    assert!(m.contains("world_px"), "{m}");
    assert!(!dir.join("worlds/main/levels/flat").exists());

    // Undo the paint, then the creation: the directory goes.
    let u = ok(&mut e, "undo", json!({ "n": 2 }));
    assert_eq!(
        u["undone"],
        json!([
            "Agent: paint collision in annex",
            "Agent: create_level annex"
        ])
    );
    assert!(!level_dir.exists());
}

/// `place_tiles` takes the transposed flips too, and they land as given.
#[test]
fn place_tiles_takes_transposed_flips() {
    let (_t, dir) = mini();
    let mut e = engine(&dir, true);
    ok(
        &mut e,
        "place_tiles",
        json!({ "level": "shaft", "layer": "decor_back",
            "placements": [[0, 0, 1, 1, "xt"], [1, 0, 2, 1, "t"], [2, 0, 3, 1, "yt"], [3, 0, 4, 1, "xyt"]] }),
    );
    let j = ok(
        &mut e,
        "read_level",
        json!({ "level": "shaft", "format": "json", "layers": ["decor_back"] }),
    );
    let text = j.to_string();
    for f in ["\"xt\"", "\"t\"", "\"yt\"", "\"xyt\""] {
        assert!(text.contains(f), "{f} missing: {text}");
    }
    let project = worlds_core::load(&dir).unwrap();
    let flips: Vec<_> = project
        .level("shaft")
        .unwrap()
        .tiles
        .iter()
        .flat_map(|l| l.tiles.iter().map(|t| t.flip.map(|f| f.as_str())))
        .collect();
    assert_eq!(flips, [Some("xt"), Some("t"), Some("yt"), Some("xyt")]);
    assert!(
        err(
            &mut e,
            "place_tiles",
            json!({ "level": "shaft", "layer": "decor_back", "placements": [[0, 0, 1, 1, "tx"]] })
        )
        .contains("\"xyt\"")
    );
}

/// A reference whose rules match on two grids gets a picture per grid,
/// each marking only its own rules' cells.
#[test]
fn preview_rule_draws_each_grid() {
    let (_t, dir) = mini();
    let mut p = worlds_core::load(&dir).unwrap();
    for cmd in [
        json!({ "op": "set_layer_def", "layer": "marks", "index": 1, "def": { "slug": "marks",
            "name": "Marks", "kind": "int_grid",
            "values": [{ "slug": "torch", "name": "Torch", "color": "#ffc040", "glyph": "t" }] } }),
        json!({ "op": "paint_cells", "level": "crevasse", "layer": "marks",
            "cells": [[3, 2, "torch"], [9, 2, "torch"]] }),
    ] {
        p.apply(serde_json::from_value(cmd).unwrap()).unwrap();
    }
    worlds_core::save_dirty(&mut p, &dir).unwrap();
    let mut e = engine(&dir, true);
    ok(
        &mut e,
        "edit_rules",
        json!({ "layer": "main_tiling", "patch": {
            "set_macros": [ { "kind": "template", "name": "lit", "def": { "rules": [
                { "slug": "torch", "match": { "grid": "marks", "rows": ["t"] }, "out": { "tiles": [[0, 0]] } },
                { "slug": "floor", "match": { "rows": ["."] }, "out": { "tiles": [[0, 1]] } } ] } } ],
            "set_rules": [ { "group": "walls", "rule": { "use": "lit", "tileset": "castle_main" }, "index": 0 } ] } }),
    );
    let r = ok(
        &mut e,
        "preview_rule",
        json!({ "layer": "main_tiling", "rule": "lit", "level": "crevasse" }),
    );
    assert_eq!(r["grid"], "marks", "{r}");
    let marks = r["picture"].as_str().unwrap();
    assert_eq!(marks.matches('@').count(), 2, "{marks}");
    let others = r["other_grids"].as_array().unwrap();
    assert_eq!(others.len(), 1, "{r}");
    assert_eq!(others[0]["grid"], "collision");
    let col = others[0]["picture"].as_str().unwrap();
    let floor = r["rules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["rule"] == "lit__floor")
        .unwrap();
    assert_eq!(floor["grid"], "collision");
    let n = floor["count"].as_u64().unwrap() + floor["matched_count"].as_u64().unwrap();
    assert!(n > 0);
    assert_eq!(
        (col.matches('@').count() + col.matches('?').count()) as u64,
        n,
        "{col}"
    );
}

/// The slug check is the API's: an unknown slug with `__` in it (not a
/// reference's rule) gets the same answer as any other, listing the rules.
#[test]
fn preview_rule_unknown_slugs_with_underscores_list_the_rules() {
    let (_t, dir) = mini();
    let mut e = engine(&dir, true);
    for slug in ["wall_fill__nope", "a__b"] {
        let m = err(
            &mut e,
            "preview_rule",
            json!({ "layer": "main_tiling", "rule": slug, "level": "crevasse" }),
        );
        assert!(
            m.contains(&format!("no rule {slug}")) && m.contains("wall_fill"),
            "{m}"
        );
    }
}
