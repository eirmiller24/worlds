//! `project.import_plan` and `project.import` (importing from another
//! project): the plan, applying it as one undo entry, files copied, and a
//! stale plan refused.

mod common;

use serde_json::json;
use worlds_server::headless::Session;
use worlds_server::rpc::code;

fn session() -> (tempfile::TempDir, std::path::PathBuf, Session) {
    let (tmp, dir) = common::mini();
    common::game_beside(&dir);
    let s = Session::open(&dir).unwrap().client("claude-code");
    (tmp, dir, s)
}

#[test]
fn plan_then_import_is_one_undo_entry() {
    let (_t, dir, mut s) = session();
    let plan = s
        .call(
            "project.import_plan",
            json!({ "source": "../game.worlds",
                    "selection": { "entities": ["lever", "locked_door"] },
                    "policy": { "on_conflict": "rename" } }),
        )
        .unwrap();
    assert_eq!(plan["plan_id"], "plan_1");
    assert_eq!(plan["empty"], false);
    assert_eq!(
        plan["summary"],
        "2 definitions, 0 levels, 1 renamed, 0 skipped"
    );
    assert_eq!(
        plan["items"],
        json!([
            { "kind": "entity", "slug": "lever", "action": "add" },
            { "kind": "entity", "slug": "locked_door", "action": "rename", "to": "locked_door_2" }
        ])
    );
    let r = s
        .call("project.import", json!({ "plan_id": "plan_1" }))
        .unwrap();
    assert_eq!(r["label"], "claude-code: Import from game");
    assert_eq!(r["copied"], json!([]));
    let d = s.call("defs.get", json!({ "kind": "entity", "slug": "lever" }));
    assert!(d.is_ok(), "{d:?}");
    // Used once: a second import of the same plan is unknown.
    let e = s
        .call("project.import", json!({ "plan_id": "plan_1" }))
        .unwrap_err();
    assert_eq!(e.code, code::INVALID_PARAMS);
    // One undo entry takes it all back.
    s.call("history.undo", json!({})).unwrap();
    let e = s
        .call(
            "defs.get",
            json!({ "kind": "entity", "slug": "locked_door_2" }),
        )
        .unwrap_err();
    assert_eq!(e.code, code::COMMAND_ERROR, "{e:?}");

    // The tileset images of a level's tilesets are copied under art/: the
    // game's grid differs, so only definitions go in here.
    let plan = s
        .call(
            "project.import_plan",
            json!({ "source": dir.parent().unwrap().join("game.worlds"),
                    "selection": { "tilesets": ["props"] } }),
        )
        .unwrap();
    assert_eq!(
        plan["assets"],
        json!([{ "from": "../assets/props.png", "to": "art/props.png", "action": "add" }])
    );
    let r = s
        .call("project.import", json!({ "plan_id": plan["plan_id"] }))
        .unwrap();
    assert_eq!(r["copied"], json!(["art/props.png"]));
    assert!(dir.join("art/props.png").is_file());
}

#[test]
fn a_stale_or_empty_plan_is_refused() {
    let (_t, _dir, mut s) = session();
    let plan = s
        .call(
            "project.import_plan",
            json!({ "source": "../game.worlds", "selection": { "entities": ["torch"] } }),
        )
        .unwrap();
    s.call("world.create", json!({ "slug": "annex" })).unwrap();
    let e = s
        .call("project.import", json!({ "plan_id": plan["plan_id"] }))
        .unwrap_err();
    assert_eq!(e.code, code::STALE_REV);

    // Importing the project into itself: everything is the same.
    let plan = s
        .call(
            "project.import_plan",
            json!({ "source": ".", "selection": { "preset": "defs_only" } }),
        )
        .unwrap();
    assert_eq!(plan["empty"], true);
    let e = s
        .call("project.import", json!({ "plan_id": plan["plan_id"] }))
        .unwrap_err();
    assert_eq!(e.code, code::COMMAND_ERROR);

    // A source that isn't a project, an unknown level.
    let e = s
        .call("project.import_plan", json!({ "source": "../nowhere" }))
        .unwrap_err();
    assert_eq!(e.code, code::INVALID_PARAMS);
    let e = s
        .call(
            "project.import_plan",
            json!({ "source": "../game.worlds", "selection": { "levels": ["nope"] } }),
        )
        .unwrap_err();
    assert_eq!(e.code, code::COMMAND_ERROR);
}
