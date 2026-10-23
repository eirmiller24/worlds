//! `project.update`, `level.set_layer_state` and `level.replace`: what each
//! writes, the changes reported, undo, `expect_rev`, batches, `confirm` and
//! the refusals.

mod common;

use serde_json::{Value, json};
use worlds_core::{Level, Project};
use worlds_server::RpcError;
use worlds_server::headless::Session;
use worlds_server::rpc::code;

fn session() -> (tempfile::TempDir, Session) {
    let (tmp, dir) = common::mini();
    (tmp, Session::open(&dir).unwrap().client("claude-code"))
}

fn err(r: Result<Value, RpcError>, code: i64) -> RpcError {
    let e = r.expect_err("expected an error");
    assert_eq!(e.code, code, "{}", e.message);
    e
}

fn kind(e: &RpcError) -> String {
    e.data.as_ref().unwrap()["kind"]
        .as_str()
        .unwrap()
        .to_string()
}

fn level(s: &Session, slug: &str) -> Level {
    s.project().level(slug).unwrap().clone()
}

fn settings(p: &Project) -> Value {
    json!([p.name, p.bg_color, p.collision_layer, p.seed])
}

#[test]
fn project_update_sets_each_key_and_undoes_in_one_step() {
    let (_t, mut s) = session();
    let before = settings(s.project());
    let r = s
        .call(
            "project.update",
            json!({ "name": "Mini 2", "bg_color": "#000000", "collision_layer": null, "seed": 1,
                "expect_rev": 0 }),
        )
        .unwrap();
    assert_eq!(r["rev"], 1);
    assert_eq!(r["label"], "claude-code: Set project name");
    assert_eq!(
        r["changes"],
        json!([
            { "change": "project", "key": "name" },
            { "change": "project", "key": "bg_color" },
            { "change": "project", "key": "collision_layer" },
            { "change": "project", "key": "seed" }
        ])
    );
    let p = s.project();
    assert_eq!(p.name, "Mini 2");
    assert_eq!(json!(p.bg_color), json!("#000000"));
    assert_eq!(p.collision_layer, None);
    assert_eq!(p.seed, 1);
    assert_eq!(
        s.call("project.describe", json!({})).unwrap()["seed"],
        json!(1)
    );
    // One write, one entry.
    let u = s.call("history.undo", json!({})).unwrap();
    assert_eq!(u["steps"].as_array().unwrap().len(), 1);
    assert_eq!(settings(s.project()), before);
    s.call("history.redo", json!({})).unwrap();
    assert_eq!(s.project().seed, 1);
}

#[test]
fn project_update_of_one_key_is_one_command_and_a_noop_changes_nothing() {
    let (_t, mut s) = session();
    let r = s.call("project.update", json!({ "seed": 5 })).unwrap();
    assert_eq!(r["label"], "claude-code: Set project seed");
    assert_eq!(
        r["changes"],
        json!([{ "change": "project", "key": "seed" }])
    );
    let entries = s.history().undo_entries().len();
    // The value it already has: no mutation.
    let r = s.call("project.update", json!({ "seed": 5 })).unwrap();
    assert_eq!(r["rev"], 1);
    assert_eq!(r["changes"], json!([]));
    assert_eq!(s.history().undo_entries().len(), entries);
    // project.json is written on save.
    s.call("project.save", json!({})).unwrap();
    let text = std::fs::read_to_string(s.dir().join("project.json")).unwrap();
    assert!(text.contains("\"seed\": 5"), "{text}");
}

#[test]
fn project_update_refusals_change_nothing() {
    let (_t, mut s) = session();
    let before = settings(s.project());
    let e = err(
        s.call(
            "project.update",
            json!({ "collision_layer": "main_tiling" }),
        ),
        code::COMMAND_ERROR,
    );
    assert_eq!(kind(&e), "wrong_layer_kind");
    let e = err(
        s.call("project.update", json!({ "collision_layer": "lava" })),
        code::COMMAND_ERROR,
    );
    assert_eq!(kind(&e), "unknown_layer");
    // A batch of settings is all or nothing.
    err(
        s.call(
            "project.update",
            json!({ "name": "Renamed", "collision_layer": "objects" }),
        ),
        code::COMMAND_ERROR,
    );
    let e = err(s.call("project.update", json!({})), code::INVALID_PARAMS);
    assert!(e.message.contains("nothing to update"), "{}", e.message);
    err(
        s.call("project.update", json!({ "grid_size": 8 })),
        code::INVALID_PARAMS,
    );
    err(
        s.call("project.update", json!({ "bg_color": "red" })),
        code::INVALID_PARAMS,
    );
    let e = err(
        s.call("project.update", json!({ "seed": 3, "expect_rev": 9 })),
        code::STALE_REV,
    );
    assert_eq!(e.data.unwrap(), json!({ "expected": 9, "current": 0 }));
    assert_eq!(settings(s.project()), before);
    assert_eq!(s.call("project.describe", json!({})).unwrap()["rev"], 0);
}

#[test]
fn set_layer_state_replaces_removes_and_undoes() {
    let (_t, mut s) = session();
    let old = level(&s, "shaft").layers;
    let r = s
        .call(
            "level.set_layer_state",
            json!({ "level": "shaft", "layer": "main_tiling",
                "state": { "seed": 11, "optional_groups": ["flooded_decor"], "tilesets": { "ssw_set5": "castle_main" } } }),
        )
        .unwrap();
    assert_eq!(
        r["label"],
        "claude-code: Set main_tiling rule state on shaft"
    );
    assert_eq!(
        r["changes"],
        json!([{ "change": "layer_state", "world": "main", "level": "shaft", "layer": "main_tiling" }])
    );
    let st = &level(&s, "shaft").layers["main_tiling"];
    assert_eq!(st.seed, 11);
    assert_eq!(st.optional_groups.len(), 1);
    assert_eq!(st.tilesets.len(), 1);
    // null takes the entry out.
    s.call(
        "level.set_layer_state",
        json!({ "level": "shaft", "layer": "main_tiling", "state": null }),
    )
    .unwrap();
    assert!(level(&s, "shaft").layers.is_empty());
    let text = s
        .call("level.get", json!({ "level": "shaft", "format": "text" }))
        .unwrap()["text"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(!text.contains("rule state"), "{text}");
    s.call("history.undo", json!({ "n": 2 })).unwrap();
    assert_eq!(level(&s, "shaft").layers, old);
    // The same state again is no mutation.
    let st = json!(old["main_tiling"]);
    let r = s
        .call(
            "level.set_layer_state",
            json!({ "level": "shaft", "layer": "main_tiling", "state": st }),
        )
        .unwrap();
    assert_eq!(r["changes"], json!([]));
}

#[test]
fn set_layer_state_refusals() {
    let (_t, mut s) = session();
    let old = level(&s, "crevasse");
    let call = |s: &mut Session, p: Value| s.call("level.set_layer_state", p);
    let e = err(
        call(
            &mut s,
            json!({ "level": "crevasse", "layer": "collision", "state": {} }),
        ),
        code::COMMAND_ERROR,
    );
    assert_eq!(kind(&e), "wrong_layer_kind");
    let e = err(
        call(
            &mut s,
            json!({ "level": "crevasse", "layer": "main_tiling", "state": { "optional_groups": ["nope"] } }),
        ),
        code::COMMAND_ERROR,
    );
    assert_eq!(kind(&e), "unknown_rule_group");
    for g in ["walls", "retired"] {
        let e = err(
            call(
                &mut s,
                json!({ "level": "crevasse", "layer": "main_tiling", "state": { "optional_groups": [g] } }),
            ),
            code::COMMAND_ERROR,
        );
        assert_eq!(kind(&e), "invalid");
        assert!(e.message.contains("not optional"), "{}", e.message);
    }
    let e = err(
        call(
            &mut s,
            json!({ "level": "crevasse", "layer": "main_tiling", "state": { "tilesets": { "castle_main": "lava" } } }),
        ),
        code::COMMAND_ERROR,
    );
    assert_eq!(kind(&e), "unknown_tileset");
    let e = err(
        call(
            &mut s,
            json!({ "level": "nowhere", "layer": "main_tiling", "state": null }),
        ),
        code::COMMAND_ERROR,
    );
    assert_eq!(kind(&e), "unknown_level");
    let e = err(
        call(
            &mut s,
            json!({ "level": "crevasse", "world": "other", "layer": "main_tiling", "state": null }),
        ),
        code::COMMAND_ERROR,
    );
    assert_eq!(kind(&e), "level_not_in_world");
    let e = err(
        call(
            &mut s,
            json!({ "level": "crevasse", "layer": "main_tiling" }),
        ),
        code::INVALID_PARAMS,
    );
    assert!(e.message.contains("missing state"), "{}", e.message);
    err(
        call(
            &mut s,
            json!({ "level": "crevasse", "layer": "main_tiling", "state": { "sead": 1 } }),
        ),
        code::INVALID_PARAMS,
    );
    assert_eq!(level(&s, "crevasse"), old);
}

/// `level.get` json's `level` and `content`, as `level.replace` takes them.
fn as_stored(s: &mut Session, slug: &str) -> (Value, Value) {
    let r = s.call("level.get", json!({ "level": slug })).unwrap();
    (r["level"].clone(), r["content"].clone())
}

#[test]
fn replace_with_the_level_as_read_needs_no_confirm_and_changes_nothing() {
    let (_t, mut s) = session();
    let old = level(&s, "crevasse");
    let (meta, content) = as_stored(&mut s, "crevasse");
    let r = s
        .call(
            "level.replace",
            json!({ "level": meta, "content": content }),
        )
        .unwrap();
    assert_eq!(r["status"], "applied");
    assert_eq!(level(&s, "crevasse"), old);
}

#[test]
fn replace_needs_confirm_for_what_it_drops_and_undo_restores_it() {
    let (_t, mut s) = session();
    let old = level(&s, "crevasse");
    let files = |s: &mut Session| -> Vec<String> {
        [
            "level.json",
            "collision.json",
            "decor_back.json",
            "entities.json",
            "notes.json",
        ]
        .iter()
        .map(|f| {
            s.call(
                "project.read_file",
                json!({ "path": format!("worlds/main/levels/crevasse/{f}") }),
            )
            .map(|r| r["text"].as_str().unwrap().to_string())
            .unwrap_or_default()
        })
        .collect()
    };
    let before = files(&mut s);
    let params = json!({
        "level": { "slug": "crevasse", "name": "Crevasse", "world_px": [0, 0], "size": [4, 2], "depth": 0 },
        "content": { "int_grids": [ { "layer": "collision", "rows": ["####", "#..#"] } ],
            "entities": [ { "layer": "objects", "entities": [ { "id": "doorcrev22", "type": "locked_door", "cell": [1, 1] } ] } ] } });
    let e = err(s.call("level.replace", params.clone()), code::NEEDS_CONFIRM);
    let reason = e.data.unwrap()["reason"].as_str().unwrap().to_string();
    assert_eq!(
        reason,
        "drops the decor_back tiles, 1 entity (btncrev222) and 2 notes (notecrev22, pathcrev22), which the new content leaves out"
    );
    assert_eq!(level(&s, "crevasse"), old);

    let mut p = params.clone();
    p["confirm"] = json!(true);
    p["expect_rev"] = json!(0);
    let r = s.call("level.replace", p).unwrap();
    assert_eq!(r["rev"], 1);
    assert_eq!(r["label"], "claude-code: Replace level crevasse");
    let changes = r["changes"].as_array().unwrap();
    assert_eq!(
        changes[0],
        json!({ "change": "level_meta", "world": "main", "level": "crevasse", "part": "all" })
    );
    for c in [
        json!({ "change": "grid", "world": "main", "level": "crevasse", "layer": "collision", "rect": [0, 0, 20, 5] }),
        json!({ "change": "entity", "world": "main", "level": "crevasse", "id": "btncrev222" }),
        json!({ "change": "note", "world": "main", "level": "crevasse", "id": "notecrev22" }),
    ] {
        assert!(changes.contains(&c), "{c} not in {changes:?}");
    }
    let new = level(&s, "crevasse");
    assert_eq!(new.id, old.id, "a left-out id keeps the level's");
    assert_eq!(new.size, [4, 2]);
    assert!(new.layers.is_empty(), "wholesale: rule state goes too");
    assert!(new.notes.is_empty() && new.tiles.is_empty());
    assert_eq!(new.all_entities().count(), 1);

    s.call("history.undo", json!({})).unwrap();
    assert_eq!(level(&s, "crevasse"), old);
    assert_eq!(files(&mut s), before);
}

#[test]
fn replace_refusals() {
    let (_t, mut s) = session();
    let lvl =
        json!({ "slug": "crevasse", "name": "C", "world_px": [0, 0], "size": [20, 5], "depth": 0 });
    let e = err(
        s.call(
            "level.replace",
            json!({ "level": { "slug": "nowhere", "name": "N", "world_px": [0, 0], "size": [1, 1], "depth": 0 } }),
        ),
        code::COMMAND_ERROR,
    );
    assert_eq!(kind(&e), "unknown_level");
    let e = err(
        s.call("level.replace", json!({ "level": { "name": "N" } })),
        code::INVALID_PARAMS,
    );
    assert!(e.message.contains("level.slug"), "{}", e.message);
    let e = err(
        s.call(
            "level.replace",
            json!({ "world": "other", "level": lvl, "confirm": true }),
        ),
        code::COMMAND_ERROR,
    );
    assert_eq!(kind(&e), "level_not_in_world");
    err(
        s.call("level.replace", json!({ "level": lvl, "extra": 1 })),
        code::INVALID_PARAMS,
    );
    // A grid of the wrong size is the command's refusal.
    let e = err(
        s.call(
            "level.replace",
            json!({ "level": lvl, "confirm": true,
                "content": { "int_grids": [ { "layer": "collision", "rows": ["##"] } ] } }),
        ),
        code::COMMAND_ERROR,
    );
    assert_eq!(kind(&e), "grid_size");
    assert_eq!(s.call("project.describe", json!({})).unwrap()["rev"], 0);
}

#[test]
fn the_three_in_one_batch_are_one_undo_entry() {
    let (_t, mut s) = session();
    let project = s.project().clone();
    s.call(
        "history.begin_batch",
        json!({ "label": "Agent: settings pass" }),
    )
    .unwrap();
    s.call("project.update", json!({ "seed": 99 })).unwrap();
    s.call(
        "level.set_layer_state",
        json!({ "level": "crevasse", "layer": "main_tiling", "state": { "seed": 3 } }),
    )
    .unwrap();
    s.call(
        "level.replace",
        json!({ "level": { "slug": "shaft", "name": "Shaft", "world_px": [320, 80], "size": [6, 8], "depth": 1 },
            "confirm": true }),
    )
    .unwrap();
    let end = s.call("history.end_batch", json!({})).unwrap();
    assert_eq!(end["entries"], 1);
    assert_eq!(end["rev"], 3);
    let u = s.call("history.undo", json!({})).unwrap();
    assert_eq!(
        u["steps"],
        json!([{ "label": "Agent: settings pass", "origin": { "agent": "claude-code" } }])
    );
    assert_eq!(s.project().seed, project.seed);
    assert_eq!(s.project().level("crevasse"), project.level("crevasse"));
    assert_eq!(s.project().level("shaft"), project.level("shaft"));
}
