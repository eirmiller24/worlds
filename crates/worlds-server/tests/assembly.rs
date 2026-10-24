//! Assemblies over the API (`defs.*` kind `assembly`, `edit.save_assembly`,
//! `edit.place_assembly`) and transforms (`edit.transform`, `level.arrange`).

mod common;

use serde_json::{Value, json};
use worlds_server::headless::Session;
use worlds_server::rpc::code;

fn session() -> (tempfile::TempDir, Session) {
    let (tmp, dir) = common::mini();
    (tmp, Session::open(&dir).unwrap().client("claude-code"))
}

fn kind(r: Result<Value, worlds_server::RpcError>) -> Value {
    r.expect_err("expected an error").data.unwrap()["kind"].clone()
}

#[test]
fn assemblies_are_defs_and_place_like_a_paste() {
    let (_tmp, mut s) = session();
    let sel = json!({ "entities": ["btncrev222", "doorcrev22"] });
    let r = s
        .call(
            "edit.save_assembly",
            json!({ "selection": sel, "slug": "doorway", "name": "Doorway" }),
        )
        .unwrap();
    assert_eq!(r["items"], 2);
    // Saving over it needs replace.
    let again = s.call(
        "edit.save_assembly",
        json!({ "selection": sel, "slug": "doorway", "name": "Doorway" }),
    );
    assert_eq!(kind(again), "duplicate_def");
    s.call(
        "edit.save_assembly",
        json!({ "selection": sel, "slug": "doorway", "name": "Door way", "replace": true }),
    )
    .unwrap();
    let r = s
        .call("defs.get", json!({ "kind": "assembly", "slug": "doorway" }))
        .unwrap();
    assert_eq!(r["def"]["name"], "Door way");
    assert_eq!(r["def"]["entities"].as_array().unwrap().len(), 2);
    let all = s.call("defs.get", json!({})).unwrap();
    assert_eq!(all["defs"]["assemblies"].as_array().unwrap().len(), 1);

    s.call(
        "defs.rename",
        json!({ "kind": "assembly", "from": "doorway", "to": "door_pair" }),
    )
    .unwrap();
    let r = s
        .call(
            "edit.place_assembly",
            json!({ "level": "crevasse", "slug": "door_pair", "origin": [1, 1] }),
        )
        .unwrap();
    assert_eq!(r["label"], "claude-code: Place assembly door_pair");
    assert_eq!(r["entities"].as_array().unwrap().len(), 2);
    let missing = s.call(
        "edit.place_assembly",
        json!({ "level": "crevasse", "slug": "doorway", "origin": [1, 1] }),
    );
    assert_eq!(kind(missing), "unknown_assembly");
    // Removing needs confirm.
    let r = s.call(
        "defs.remove",
        json!({ "kind": "assembly", "slug": "door_pair" }),
    );
    assert_eq!(r.unwrap_err().code, code::NEEDS_CONFIRM);
    s.call(
        "defs.remove",
        json!({ "kind": "assembly", "slug": "door_pair", "confirm": true }),
    )
    .unwrap();
    assert!(s.project().assemblies.is_empty());
}

#[test]
fn transform_and_arrange_are_one_undo_entry_each() {
    let (_tmp, mut s) = session();
    let r = s
        .call(
            "edit.transform",
            json!({ "selection": { "entities": ["btncrev222", "doorcrev22"] }, "op": "mirror_x" }),
        )
        .unwrap();
    assert_eq!(r["label"], "claude-code: Mirror 2 items left-right");
    let door = |s: &Session| {
        s.project()
            .levels()
            .flat_map(|l| l.all_entities())
            .find(|e| e.id.as_str() == "doorcrev22")
            .unwrap()
            .cell
    };
    assert_eq!(door(&s), [12, 3]);
    s.call("history.undo", json!({})).unwrap();
    assert_eq!(door(&s), [18, 3]);
    // One entity mirrored about its own box doesn't move.
    let r = s.call(
        "edit.transform",
        json!({ "selection": { "entities": ["btncrev222"] }, "op": "mirror_y" }),
    );
    assert_eq!(kind(r), "invalid");

    s.call(
        "level.arrange",
        json!({ "levels": ["crevasse", "shaft"], "op": "align_left" }),
    )
    .unwrap();
    assert_eq!(s.project().level("shaft").unwrap().world_px, [0, 80]);
    let r = s.call(
        "level.arrange",
        json!({ "levels": ["shaft"], "op": "align_left" }),
    );
    assert_eq!(kind(r), "invalid");
}
