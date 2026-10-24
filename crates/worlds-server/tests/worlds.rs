//! Several worlds over the API: `world.*`, `level.move_to_world`, the
//! worlds in `project.describe`, saving, and `view.focus { world }`
//! against a stand-in editor host.

mod common;

use serde_json::{Value, json};
use worlds_server::headless::{HeadlessHost, Session};
use worlds_server::rpc::code;
use worlds_server::{
    Client, Doc, Handler, HandlerConfig, Host, Outcome, RpcError, Selection, ViewTarget,
};

fn session() -> (tempfile::TempDir, Session) {
    let (tmp, dir) = common::mini();
    (tmp, Session::open(&dir).unwrap().client("claude-code"))
}

fn kinds(r: &Value) -> Vec<String> {
    r["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["change"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn worlds_are_created_moved_into_renamed_reordered_and_removed() {
    let (_t, mut s) = session();
    let r = s
        .call(
            "world.create",
            json!({ "slug": "dungeon", "name": "Dungeon" }),
        )
        .unwrap();
    assert_eq!(kinds(&r), ["world_created", "project"]);
    let r = s
        .call(
            "level.move_to_world",
            json!({ "level": "shaft", "world": "dungeon", "world_px": [32, 48] }),
        )
        .unwrap();
    assert_eq!(kinds(&r), ["level_deleted", "level_created"]);

    let d = s.call("project.describe", json!({})).unwrap();
    let worlds = d["worlds"].as_array().unwrap();
    assert_eq!(worlds.len(), 2);
    assert_eq!(worlds[0]["slug"], "main");
    assert_eq!(worlds[0]["level_count"], 1);
    assert_eq!(worlds[0]["bbox"], json!([0, 0, 320, 80]));
    assert_eq!(worlds[1]["slug"], "dungeon");
    assert_eq!(worlds[1]["name"], "Dungeon");
    assert_eq!(worlds[1]["bbox"], json!([32, 48, 96, 128]));
    let lv = d["levels"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["slug"] == "shaft")
        .unwrap();
    assert_eq!(lv["world"], "dungeon");

    // A level method finds the level in its new world.
    s.call(
        "level.move",
        json!({ "level": "shaft", "world_px": [0, 0] }),
    )
    .unwrap();

    s.call(
        "world.rename",
        json!({ "world": "dungeon", "slug": "underdark" }),
    )
    .unwrap();
    let e = s
        .call("world.rename", json!({ "world": "underdark" }))
        .unwrap_err();
    assert_eq!(e.code, code::INVALID_PARAMS);
    s.call("world.reorder", json!({ "order": ["underdark", "main"] }))
        .unwrap();
    let d = s.call("project.describe", json!({})).unwrap();
    assert_eq!(d["worlds"][0]["slug"], "underdark");
    let e = s
        .call("world.reorder", json!({ "order": ["main"] }))
        .unwrap_err();
    assert_eq!(e.data.unwrap()["kind"], "invalid");

    // Not empty: needs confirm.
    let e = s
        .call("world.remove", json!({ "world": "underdark" }))
        .unwrap_err();
    assert_eq!(e.code, code::NEEDS_CONFIRM);
    s.call("project.save", json!({})).unwrap();
    assert!(
        s.dir()
            .join("worlds/underdark/levels/shaft/level.json")
            .is_file()
    );
    let project = std::fs::read_to_string(s.dir().join("project.json")).unwrap();
    assert!(
        project.contains("\"worlds\": [\"underdark\", \"main\"]"),
        "{project}"
    );
    let r = s
        .call(
            "world.remove",
            json!({ "world": "underdark", "confirm": true }),
        )
        .unwrap();
    assert_eq!(kinds(&r), ["level_deleted", "world_removed", "project"]);
    s.call("project.save", json!({})).unwrap();
    assert!(!s.dir().join("worlds/underdark").exists());
    // And undo brings it all back.
    s.call("history.undo", json!({})).unwrap();
    assert!(s.project().level("shaft").is_some());
    assert_eq!(s.project().worlds[0].slug.as_str(), "underdark");

    // An empty world goes without confirm; the last world never goes.
    s.call("world.create", json!({ "slug": "spare" })).unwrap();
    s.call("world.remove", json!({ "world": "spare" })).unwrap();
    let e = s
        .call("world.create", json!({ "slug": "main" }))
        .unwrap_err();
    assert_eq!(e.data.unwrap()["kind"], "duplicate_world");
}

/// A stand-in editor recording what the view was asked to do.
struct ViewHost {
    inner: HeadlessHost,
    focused: Vec<ViewTarget>,
}

impl Host for ViewHost {
    fn doc(&mut self) -> Doc<'_> {
        self.inner.doc()
    }
    fn applied(&mut self, changes: &[worlds_core::Change]) {
        self.inner.applied(changes);
    }
    fn reload(&mut self) -> Result<(), RpcError> {
        self.inner.reload()
    }
    fn focus(&mut self, target: &ViewTarget, _: Option<u32>) -> Result<(), RpcError> {
        self.focused.push(target.clone());
        Ok(())
    }
    fn select(&mut self, _: &Selection, _: Option<u32>) -> Result<(), RpcError> {
        Ok(())
    }
}

fn reply(o: Outcome) -> Result<Value, RpcError> {
    match o {
        Outcome::Reply(r) => r,
        Outcome::Pending { .. } => panic!("pending"),
    }
}

#[test]
fn view_focus_world_reaches_the_editor() {
    let (_t, dir) = common::mini();
    let mut host = ViewHost {
        inner: HeadlessHost::open(&dir).unwrap(),
        focused: Vec::new(),
    };
    let mut h = Handler::new(HandlerConfig::default());
    let c = Client::in_process("claude-code");
    reply(h.call(&mut host, &c, "view.focus", json!({ "world": "main" }))).unwrap();
    assert_eq!(
        host.focused,
        vec![ViewTarget::World(worlds_core::slug("main"))]
    );
    let e = reply(h.call(&mut host, &c, "view.focus", json!({ "world": "nowhere" }))).unwrap_err();
    assert_eq!(e.data.unwrap()["kind"], "unknown_world");
    // With a rect, `world` names the rect's world as before.
    reply(h.call(
        &mut host,
        &c,
        "view.focus",
        json!({ "world": "main", "world_rect": [0, 0, 10, 10] }),
    ))
    .unwrap();
    assert!(matches!(
        host.focused.last(),
        Some(ViewTarget::WorldRect { world: Some(_), .. })
    ));
    assert_eq!(host.focused.len(), 2);
}
