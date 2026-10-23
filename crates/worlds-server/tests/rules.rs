//! The `rules.*` methods in-process: rule, macro and stamp writes (one
//! command, one undo entry, a `changed` event each), their refusals,
//! `rules.expand`, and `rules.preview` with `matches`.

mod common;

use serde_json::{Value, json};
use worlds_server::headless::Session;
use worlds_server::rpc::code;

fn session() -> (tempfile::TempDir, Session) {
    let (tmp, dir) = common::mini();
    (tmp, Session::open(&dir).unwrap().client("claude-code"))
}

fn file(s: &mut Session, path: &str) -> String {
    s.call("project.read_file", json!({ "path": path }))
        .unwrap()["text"]
        .as_str()
        .unwrap()
        .to_string()
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

fn ledge() -> Value {
    json!({ "kind": "template", "name": "ledge", "def": { "legend": "walls", "rules": [
        { "slug": "top", "match": { "rows": [".", "#", "*"] }, "out": { "tiles": [[0, 0]] }, "break": true },
        { "slug": "face", "match": { "rows": ["#"] }, "out": { "tiles": [[0, 1]] }, "chance": 0.5 } ] } })
}

fn ledges(index: usize) -> Value {
    json!({ "layer": "main_tiling", "group": "walls", "index": index,
        "rule": { "use": "ledge", "slug": "ledges", "tileset": "castle_main", "origin": [14, 0] } })
}

/// Every rule write is one command and one undo entry, takes `expect_rev`
/// and `label`, and announces a `changed` event with its `def` change; undo
/// puts the files back byte for byte.
#[test]
fn rule_writes_are_one_entry_each() {
    let (_t, mut s) = session();
    s.call("events.subscribe", json!({ "events": ["changed"] }))
        .unwrap();
    let rules0 = file(&mut s, "rules/main_tiling.json");
    let macros0 = file(&mut s, "rules/macros.json");

    let mut p = ledge();
    p["expect_rev"] = json!(0);
    p["label"] = json!("Agent: add ledge");
    let r = s.call("rules.set_macro", p).unwrap();
    assert_eq!(
        (r["rev"].clone(), r["label"].clone(), r["changes"].clone()),
        (
            json!(1),
            json!("Agent: add ledge"),
            json!([{ "change": "def", "kind": "macros", "slug": "macros" }])
        )
    );
    let ev = s.take_events();
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0]["params"]["label"], "Agent: add ledge");
    assert_eq!(ev[0]["params"]["files"], json!(["rules/macros.json"]));

    let r = s.call("rules.set_rule", ledges(1)).unwrap();
    assert_eq!(r["label"], "claude-code: Set rule ledges on main_tiling");
    assert_eq!(
        r["changes"],
        json!([{ "change": "def", "kind": "rules", "slug": "main_tiling" }])
    );
    // A stale rev changes nothing.
    let mut p = json!({ "layer": "main_tiling", "rule": "ledges", "group": "walls", "index": 0 });
    p["expect_rev"] = json!(1);
    assert_eq!(
        s.call("rules.move_rule", p).unwrap_err().code,
        code::STALE_REV
    );
    s.call(
        "rules.move_rule",
        json!({ "layer": "main_tiling", "rule": "ledges", "group": "walls", "index": 0 }),
    )
    .unwrap();
    let list = s
        .call("rules.list", json!({ "layer": "main_tiling" }))
        .unwrap();
    assert_eq!(
        list["layers"][0]["groups"][0]["rules"][0],
        json!({ "slug": "ledges", "active": true, "use": "ledge" })
    );
    assert_eq!(list["macros"]["templates"], json!(["ledge"]));
    assert_eq!(list["macros"]["builtin"], json!(["blob47"]));
    // An index on an existing rule moves it within its group.
    let mut p = ledges(2);
    p["rule"]["origin"] = json!([14, 1]);
    s.call("rules.set_rule", p).unwrap();
    let list = s
        .call("rules.list", json!({ "layer": "main_tiling" }))
        .unwrap();
    assert_eq!(
        list["layers"][0]["groups"][0]["rules"][2]["slug"],
        json!("ledges")
    );
    s.call(
        "rules.set_stamp",
        json!({ "layer": "main_tiling", "stamp": "arch",
            "def": { "slug": "arch", "tileset": "castle_main", "rect": [4, 4, 3, 2] } }),
    )
    .unwrap();
    s.call(
        "rules.rename_macro",
        json!({ "kind": "legend", "from": "walls", "to": "any_wall" }),
    )
    .unwrap();
    assert!(file(&mut s, "rules/macros.json").contains("\"any_wall\""));
    s.call(
        "rules.remove_rule",
        json!({ "layer": "main_tiling", "rule": "ledges" }),
    )
    .unwrap();
    s.call(
        "rules.remove_macro",
        json!({ "kind": "template", "name": "ledge" }),
    )
    .unwrap();
    s.call(
        "rules.remove_stamp",
        json!({ "layer": "main_tiling", "stamp": "arch" }),
    )
    .unwrap();
    assert_eq!(s.history().undo_entries().len(), 9);
    assert_eq!(s.take_events().len(), 8);

    let u = s.call("history.undo", json!({ "n": 9 })).unwrap();
    assert_eq!(u["steps"].as_array().unwrap().len(), 9);
    assert_eq!(file(&mut s, "rules/main_tiling.json"), rules0);
    assert_eq!(file(&mut s, "rules/macros.json"), macros0);
}

#[test]
fn refusals() {
    let (_t, mut s) = session();
    s.call("rules.set_macro", ledge()).unwrap();
    s.call("rules.set_rule", ledges(1)).unwrap();
    let rev = s.call("level.list", json!({})).unwrap()["rev"].clone();

    // Unknown rule, for remove and move.
    let (kind, msg) = command_error(s.call(
        "rules.remove_rule",
        json!({ "layer": "main_tiling", "rule": "nope" }),
    ));
    assert_eq!(kind, "unknown_rule");
    assert!(msg.contains("nope"), "{msg}");
    let (kind, _) = command_error(s.call(
        "rules.move_rule",
        json!({ "layer": "main_tiling", "rule": "nope", "group": "walls", "index": 0 }),
    ));
    assert_eq!(kind, "unknown_rule");
    // An expanded rule isn't an entry: the message points at skip.
    let (kind, msg) = command_error(s.call(
        "rules.remove_rule",
        json!({ "layer": "main_tiling", "rule": "ledges__top" }),
    ));
    assert_eq!(kind, "invalid");
    assert!(msg.contains("skip"), "{msg}");
    // A slug another group holds: use move_rule.
    let (kind, msg) = command_error(s.call(
        "rules.set_rule",
        json!({ "layer": "main_tiling", "group": "pillars",
            "rule": { "slug": "wall_fill", "match": { "rows": ["+"] }, "out": { "tileset": "castle_main", "tiles": [[1, 1]] } } }),
    ));
    assert_eq!(kind, "invalid");
    assert!(msg.contains("move_rule"), "{msg}");
    // A template in use.
    let (kind, msg) = command_error(s.call(
        "rules.remove_macro",
        json!({ "kind": "template", "name": "ledge" }),
    ));
    assert_eq!(kind, "in_use");
    assert!(msg.contains("ledges"), "{msg}");
    // A stamp in use.
    let e = s
        .call(
            "rules.remove_stamp",
            json!({ "layer": "main_tiling", "stamp": "pillar" }),
        )
        .unwrap_err();
    let data = e.data.unwrap();
    assert_eq!(
        (data["kind"].clone(), data["def_kind"].clone()),
        (json!("in_use"), json!("stamp"))
    );
    // Built-in templates are read-only; unknown macros are named.
    let (kind, _) = command_error(s.call(
        "rules.remove_macro",
        json!({ "kind": "template", "name": "blob47" }),
    ));
    assert_eq!(kind, "invalid");
    let (kind, _) = command_error(s.call(
        "rules.rename_macro",
        json!({ "kind": "pattern", "from": "nope", "to": "other" }),
    ));
    assert_eq!(kind, "unknown_macro");
    // Bad params.
    let e = s
        .call(
            "rules.set_rule",
            json!({ "layer": "main_tiling", "group": "walls", "rule": { "slug": "x", "bogus": 1 } }),
        )
        .unwrap_err();
    assert_eq!(e.code, code::INVALID_PARAMS);
    // None of that changed anything.
    assert_eq!(s.call("level.list", json!({})).unwrap()["rev"], rev);
    assert_eq!(s.history().undo_entries().len(), 2);
}

/// `defs.*` sends stamps and macros to the `rules.*` methods; `level_field`
/// stays unsupported; `defs.get` reads `rules/macros.json`.
#[test]
fn defs_kinds_stamp_and_macros() {
    let (_t, mut s) = session();
    for (method, p, names) in [
        (
            "defs.set",
            json!({ "kind": "stamp", "def": {} }),
            "rules.set_stamp",
        ),
        (
            "defs.remove",
            json!({ "kind": "stamp", "slug": "pillar", "confirm": true }),
            "rules.remove_stamp",
        ),
        (
            "defs.rename",
            json!({ "kind": "stamp", "from": "pillar", "to": "column" }),
            "rules.set_stamp",
        ),
        (
            "defs.set",
            json!({ "kind": "macros", "def": {} }),
            "rules.set_macro",
        ),
        (
            "defs.remove",
            json!({ "kind": "macros", "slug": "macros", "confirm": true }),
            "rules.remove_macro",
        ),
        (
            "defs.rename",
            json!({ "kind": "macros", "from": "a", "to": "b" }),
            "rules.rename_macro",
        ),
    ] {
        let e = s.call(method, p.clone()).unwrap_err();
        assert_eq!(e.code, code::INVALID_PARAMS, "{method} {p}");
        assert!(e.message.contains(names), "{method} {p}: {}", e.message);
    }
    // level_field is an editable kind now: a bad def is invalid params.
    let e = s
        .call("defs.set", json!({ "kind": "level_field", "def": {} }))
        .unwrap_err();
    assert_eq!(e.code, code::INVALID_PARAMS);

    let all = s.call("defs.get", json!({})).unwrap();
    assert_eq!(all["defs"]["macros"]["patterns"]["edge_top"][1], ".#.");
    let one = s.call("defs.get", json!({ "kind": "macros" })).unwrap();
    assert_eq!(one["defs"]["macros"], all["defs"]["macros"]);
    let def = s
        .call("defs.get", json!({ "kind": "macros", "slug": "macros" }))
        .unwrap();
    assert_eq!(def["def"], all["defs"]["macros"]);
    let m = s.call("rules.get_macros", json!({})).unwrap();
    assert_eq!(m["macros"], all["defs"]["macros"]);
    assert_eq!(m["builtin"], json!(["blob47"]));
}

/// `rules.expand` is `Project::expanded_rules` in file form, `from` on the
/// expanded rules.
#[test]
fn expand_is_the_expanded_rules() {
    let (_t, mut s) = session();
    s.call("rules.set_macro", ledge()).unwrap();
    s.call("rules.set_rule", ledges(1)).unwrap();
    s.call(
        "rules.set_rule",
        json!({ "layer": "main_tiling", "group": "pillars",
            "rule": { "use": "blob47", "tileset": "castle_main", "origin": [0, 20], "active": false } }),
    )
    .unwrap();
    let r = s
        .call("rules.expand", json!({ "layer": "main_tiling" }))
        .unwrap();
    let want: Vec<Value> = s
        .project()
        .expanded_rules("main_tiling")
        .unwrap()
        .iter()
        .map(|g| {
            let rules: Vec<Value> = g
                .rules
                .iter()
                .map(|x| {
                    let mut v = serde_json::to_value(&*x.rule).unwrap();
                    if let Some(f) = &x.from {
                        v["from"] = json!(f);
                    }
                    v
                })
                .collect();
            json!({ "slug": g.group.slug, "rules": rules })
        })
        .collect();
    assert_eq!(r["groups"], json!(want));
    let walls = &r["groups"][0]["rules"];
    assert_eq!(walls[1]["slug"], "ledges__top");
    assert_eq!(walls[1]["from"], "ledges");
    assert_eq!(walls[1]["out"]["tiles"], json!([[14, 0]]));
    assert!(walls[0].get("from").is_none());
    assert!(r["groups"][2]["rules"].as_array().unwrap().len() > 40);
    let one = s
        .call(
            "rules.expand",
            json!({ "layer": "main_tiling", "group": "walls" }),
        )
        .unwrap();
    assert_eq!(one["groups"], json!([r["groups"][0]]));
    let (kind, _) = command_error(s.call(
        "rules.expand",
        json!({ "layer": "main_tiling", "group": "nope" }),
    ));
    assert_eq!(kind, "unknown_rule_group");
    // A reference to a template that doesn't exist can't be expanded.
    s.call(
        "rules.set_rule",
        json!({ "layer": "main_tiling", "group": "walls", "rule": { "use": "nowhere", "tileset": "castle_main" } }),
    )
    .unwrap();
    let (kind, msg) = command_error(s.call("rules.expand", json!({ "layer": "main_tiling" })));
    assert_eq!(kind, "invalid");
    assert!(msg.contains("nowhere"), "{msg}");
}

/// `matches: true` lists where a rule matches but doesn't fire: a rule with
/// `chance: 0` matches but never fires, so it is listed with `count: 0`
/// only then. A reference previews all its rules, with `from`.
#[test]
fn preview_matches_and_references() {
    let (_t, mut s) = session();
    s.call(
        "rules.set_rule",
        json!({ "layer": "main_tiling", "group": "walls", "index": 0,
            "rule": { "slug": "never", "match": { "rows": ["#"] }, "out": { "tileset": "castle_main", "tiles": [[3, 3]] }, "chance": 0 } }),
    )
    .unwrap();
    let plain = s
        .call(
            "rules.preview",
            json!({ "layer": "main_tiling", "level": "crevasse", "rule": "never" }),
        )
        .unwrap();
    assert_eq!(plain["fires"], json!([]));
    let r = s
        .call(
            "rules.preview",
            json!({ "layer": "main_tiling", "level": "crevasse", "rule": "never", "matches": true }),
        )
        .unwrap();
    let f = &r["fires"][0];
    assert_eq!(
        (f["count"].clone(), f["cells"].clone()),
        (json!(0), json!([]))
    );
    let n = f["matched_count"].as_u64().unwrap();
    assert!(n > 0, "{r}");
    assert_eq!(f["matched"].as_array().unwrap().len() as u64, n);
    // Without a rule filter: entries without matches have no matched keys.
    let all = s
        .call(
            "rules.preview",
            json!({ "layer": "main_tiling", "level": "crevasse" }),
        )
        .unwrap();
    assert!(
        all["fires"]
            .as_array()
            .unwrap()
            .iter()
            .all(|f| f.get("matched").is_none() && f["count"].as_u64().unwrap() > 0)
    );
    // `rect` limits both lists.
    let r = s
        .call(
            "rules.preview",
            json!({ "layer": "main_tiling", "level": "crevasse", "rule": "never", "matches": true, "rect": [0, 0, 2, 1] }),
        )
        .unwrap();
    assert_eq!(r["fires"][0]["matched"], json!([[0, 0], [1, 0]]));

    // A reference: every rule it expands to, with `from`.
    s.call("rules.set_macro", ledge()).unwrap();
    s.call("rules.set_rule", ledges(1)).unwrap();
    let r = s
        .call(
            "rules.preview",
            json!({ "layer": "main_tiling", "level": "crevasse", "rule": "ledges", "matches": true }),
        )
        .unwrap();
    let rules: Vec<&str> = r["fires"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["rule"].as_str().unwrap())
        .collect();
    assert_eq!(rules, ["ledges__top", "ledges__face"], "{r}");
    assert!(
        r["fires"]
            .as_array()
            .unwrap()
            .iter()
            .all(|f| f["from"] == "ledges")
    );
    // `face` is blocked wherever `top` (break) fired.
    let top: Vec<Value> = r["fires"][0]["cells"].as_array().unwrap().clone();
    let face_matched = r["fires"][1]["matched"].as_array().unwrap();
    assert!(
        !top.is_empty() && top.iter().all(|c| face_matched.contains(c)),
        "{r}"
    );
    // One expanded rule by its own slug.
    let r = s
        .call(
            "rules.preview",
            json!({ "layer": "main_tiling", "level": "crevasse", "rule": "ledges__top" }),
        )
        .unwrap();
    assert_eq!(r["fires"].as_array().unwrap().len(), 1);
    let (kind, _) = command_error(s.call(
        "rules.preview",
        json!({ "layer": "main_tiling", "level": "crevasse", "rule": "nope" }),
    ));
    assert_eq!(kind, "unknown_rule");
}

/// `grid` is the rule's own `match.grid`, not the layer's source.
#[test]
fn preview_grid_is_the_rules_own() {
    let (_t, mut s) = session();
    s.call(
        "defs.set",
        json!({ "kind": "layer", "index": 1, "def": { "slug": "marks", "name": "Marks", "kind": "int_grid",
            "values": [{ "slug": "torch", "name": "Torch", "color": "#ffc040", "glyph": "t" }] } }),
    )
    .unwrap();
    s.call(
        "grid.set_cells",
        json!({ "level": "crevasse", "layer": "marks", "cells": [[3, 2, "torch"], [9, 1, "torch"]] }),
    )
    .unwrap();
    s.call(
        "rules.set_rule",
        json!({ "layer": "main_tiling", "group": "walls", "index": 0,
            "rule": { "slug": "torch", "match": { "grid": "marks", "rows": ["t"], "legend": { "t": "torch" } },
                "out": { "tileset": "castle_main", "tiles": [[9, 9]] } } }),
    )
    .unwrap();
    let r = s
        .call(
            "rules.preview",
            json!({ "layer": "main_tiling", "level": "crevasse", "rule": "torch" }),
        )
        .unwrap();
    assert_eq!(
        r["fires"],
        json!([{ "rule": "torch", "group": "walls", "pass": 0, "grid": "marks", "cells": [[9, 1], [3, 2]], "count": 2 }])
    );
    // The other rules still report the source.
    let r = s
        .call(
            "rules.preview",
            json!({ "layer": "main_tiling", "level": "crevasse", "rule": "wall_fill" }),
        )
        .unwrap();
    assert_eq!(r["fires"][0]["grid"], "collision");
}

/// Headless keeps its bake cache across a rule edit: `rules.set_rule` on one
/// layer rebakes that layer in the cached levels and leaves the other
/// layers' cached bakes as they were (not a cleared cache).
#[test]
fn a_headless_rule_edit_rebakes_only_its_layer() {
    let (_t, mut s) = session();
    s.call(
        "defs.set",
        json!({ "kind": "layer", "def": { "slug": "trim", "name": "Trim", "kind": "auto",
            "source": "collision" } }),
    )
    .unwrap();
    s.call(
        "rules.set_group",
        json!({ "layer": "trim", "group": { "slug": "edges", "rules": [
            { "slug": "edge", "match": { "rows": [".", "#"] },
              "out": { "tileset": "castle_main", "tiles": [[5, 5]] } } ] } }),
    )
    .unwrap();
    s.call("project.bake", json!({})).unwrap();
    let ptr = |s: &mut Session, level: &str, layer: &str| {
        let (_, host, _) = s.parts();
        host.bakes
            .get_shared(level, layer)
            .map(|b| std::sync::Arc::as_ptr(b) as usize)
    };
    let levels = ["crevasse", "shaft"];
    let trim: Vec<_> = levels.iter().map(|l| ptr(&mut s, l, "trim")).collect();
    assert!(trim.iter().all(Option::is_some), "{trim:?}");
    s.call(
        "rules.set_rule",
        json!({ "layer": "main_tiling", "group": "walls", "index": 0,
            "rule": { "slug": "cap", "match": { "rows": [".", "#"] },
                "out": { "tileset": "castle_main", "tiles": [[7, 7]] }, "break": true } }),
    )
    .unwrap();
    let (_, host, _) = s.parts();
    for (l, before) in levels.iter().zip(&trim) {
        let p = host
            .bakes
            .get_shared(l, "trim")
            .map(|b| std::sync::Arc::as_ptr(b) as usize);
        assert_eq!(p, *before, "{l}: trim was rebaked or dropped");
        let level = host.project.level(l).unwrap();
        let want = worlds_rules::bake_layer(
            &host.project,
            level,
            &worlds_core::slug("main_tiling"),
            &host.images,
        )
        .unwrap();
        assert_eq!(host.bakes.get(l, "main_tiling"), Some(&want), "{l}");
    }
    assert!(
        host.bakes
            .get("crevasse", "main_tiling")
            .unwrap()
            .placements
            .iter()
            .any(|p| p.rule == "cap")
    );
}
