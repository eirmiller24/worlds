//! Annotations over the API, in-process: `notes.*` filters, the locked
//! rule, `notes.move`, guide groups, level tags, `describe` counts,
//! `changed` files, and `view.select` / `view.focus { note }` against a
//! stand-in editor host.

mod common;

use serde_json::{Value, json};
use worlds_core::Project;
use worlds_server::headless::{HeadlessHost, Session};
use worlds_server::rpc::code;
use worlds_server::{
    Client, Doc, Handler, HandlerConfig, Host, Out, Outcome, RpcError, Selection, ViewTarget,
};

/// A mini copy, opened headless.
fn session() -> (tempfile::TempDir, Session) {
    let (tmp, dir) = common::mini();
    (tmp, Session::open(&dir).unwrap().client("claude-code"))
}

/// Four layout guides in world `main`: two in the guide group `shifted`
/// (offset 769 px down) tagged `east`, two ungrouped tagged `west`.
fn add_guides(s: &mut Session) {
    s.call(
        "notes.set_guide_group",
        json!({ "group": { "slug": "shifted", "offset": [0, 769] } }),
    )
    .unwrap();
    for (x, tag, group) in [
        (0, "east", Some("shifted")),
        (100, "east", Some("shifted")),
        (200, "west", None),
        (300, "west", None),
    ] {
        let mut note = json!({ "kind": "guide", "locked": true, "points": [[x, 0], [x, 400]],
            "tags": ["layout", tag] });
        if let Some(g) = group {
            note["group"] = json!(g);
        }
        s.call(
            "notes.add",
            json!({ "scope": { "world": "main" }, "note": note }),
        )
        .unwrap();
    }
}

fn err(r: Result<Value, RpcError>) -> RpcError {
    r.expect_err("expected an error")
}

fn ids(v: &Value) -> Vec<String> {
    v["notes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["note"]["id"].as_str().unwrap().to_string())
        .collect()
}

fn note<'p>(p: &'p Project, id: &str) -> &'p worlds_core::Annotation {
    p.worlds
        .iter()
        .flat_map(|w| w.notes.iter().chain(w.levels.iter().flat_map(|l| &l.notes)))
        .find(|n| n.id.as_str() == id)
        .unwrap()
}

#[test]
fn list_filters_kind_kinds_group_and_export() {
    let (_t, mut s) = session();
    let all = s.call("notes.list", json!({})).unwrap();
    assert_eq!(
        ids(&all),
        [
            "guidestem2",
            "pinstatue2",
            "regioneast",
            "notecrev22",
            "pathcrev22"
        ],
        "the raw API lists every kind, guides included"
    );
    let r = s.call("notes.list", json!({ "kind": "guide" })).unwrap();
    assert_eq!(ids(&r), ["guidestem2"]);
    let r = s
        .call(
            "notes.list",
            json!({ "kinds": ["note", "pin", "path", "region"] }),
        )
        .unwrap();
    assert_eq!(
        ids(&r),
        ["pinstatue2", "regioneast", "notecrev22", "pathcrev22"]
    );
    let r = s.call("notes.list", json!({ "export": true })).unwrap();
    assert_eq!(ids(&r), ["pinstatue2"]);
    let r = s.call("notes.list", json!({ "export": false })).unwrap();
    assert_eq!(r["notes"].as_array().unwrap().len(), 4);
    // group: only guides of that group (none yet).
    let r = s.call("notes.list", json!({ "group": "shifted" })).unwrap();
    assert_eq!(ids(&r), Vec::<String>::new());
    add_guides(&mut s);
    let r = s.call("notes.list", json!({ "group": "shifted" })).unwrap();
    assert_eq!(
        r["notes"].as_array().unwrap().len(),
        2,
        "the grouped guides"
    );
    let r = s
        .call("notes.list", json!({ "kind": "guide", "tag": "west" }))
        .unwrap();
    assert!(!ids(&r).is_empty());
    // Unknown kinds are an error, not an empty list.
    for p in [json!({ "kind": "pins" }), json!({ "kinds": ["note", "x"] })] {
        assert_eq!(err(s.call("notes.list", p)).code, code::INVALID_PARAMS);
    }
}

#[test]
fn locked_notes_need_confirm_to_move_or_go() {
    let (_t, mut s) = session();
    let moved = json!({ "id": "guidestem2", "patch": { "points": [[0, 0], [0, 480]] } });
    let e = err(s.call("notes.update", moved.clone()));
    assert_eq!(e.code, code::NEEDS_CONFIRM);
    assert_eq!(
        e.data.unwrap()["reason"],
        "note guidestem2 is locked; moving it needs confirm: true"
    );
    assert_eq!(s.history().undo_entries().len(), 0, "nothing applied");
    // Wholesale form and group change count too.
    let mut whole = serde_json::to_value(note(s.project(), "guidestem2")).unwrap();
    whole["points"] = json!([[1, 0], [1, 480]]);
    assert_eq!(
        err(s.call("notes.update", json!({ "id": "guidestem2", "note": whole }))).code,
        code::NEEDS_CONFIRM
    );
    s.call(
        "notes.set_guide_group",
        json!({ "group": { "slug": "g1" } }),
    )
    .unwrap();
    assert_eq!(
        err(s.call(
            "notes.update",
            json!({ "id": "guidestem2", "patch": { "group": "g1" } })
        ))
        .code,
        code::NEEDS_CONFIRM
    );
    // Text, tags, status, export, locked itself: never.
    for patch in [
        json!({ "title": "Stem" }),
        json!({ "tags": ["layout", "forest"] }),
        json!({ "status": "done" }),
        json!({ "color": "#ff0000" }),
    ] {
        s.call(
            "notes.update",
            json!({ "id": "guidestem2", "patch": patch }),
        )
        .unwrap();
    }
    s.call(
        "notes.resolve",
        json!({ "id": "guidestem2", "status": "open" }),
    )
    .unwrap();
    // With confirm it applies.
    let mut c = moved.clone();
    c["confirm"] = json!(true);
    s.call("notes.update", c).unwrap();
    assert_eq!(
        note(s.project(), "guidestem2").shape.points(),
        vec![[0, 0], [0, 480]]
    );
    // Removing: refused, then applied with confirm.
    let e = err(s.call(
        "notes.remove",
        json!({ "ids": ["pinstatue2", "guidestem2"] }),
    ));
    assert_eq!(e.code, code::NEEDS_CONFIRM);
    assert_eq!(
        e.data.unwrap()["reason"],
        "note guidestem2 is locked; removing it needs confirm: true"
    );
    s.call(
        "notes.remove",
        json!({ "ids": ["pinstatue2", "guidestem2"], "confirm": true }),
    )
    .unwrap();
    // Unlocking it first frees it: `locked` before the edit is what counts.
    s.call(
        "notes.update",
        json!({ "id": "regioneast", "patch": { "locked": true } }),
    )
    .unwrap();
    assert_eq!(
        err(s.call(
            "notes.update",
            json!({ "id": "regioneast", "patch": { "locked": false, "rect": [0, 0, 10, 10] } })
        ))
        .code,
        code::NEEDS_CONFIRM
    );
    s.call(
        "notes.update",
        json!({ "id": "regioneast", "patch": { "locked": false } }),
    )
    .unwrap();
    s.call(
        "notes.update",
        json!({ "id": "regioneast", "patch": { "rect": [0, 0, 10, 10] } }),
    )
    .unwrap();
}

#[test]
fn move_translates_every_kind_in_one_entry() {
    let (_t, mut s) = session();
    let r = s
        .call(
            "notes.move",
            json!({ "ids": ["pinstatue2", "regioneast", "notecrev22", "pathcrev22"], "delta": [5, -3] }),
        )
        .unwrap();
    assert_eq!(r["label"], "claude-code: Move 4 notes");
    assert_eq!(r["changes"].as_array().unwrap().len(), 4);
    assert_eq!(s.history().undo_entries().len(), 1);
    let p = s.project();
    assert_eq!(note(p, "pinstatue2").shape.points(), vec![[101, 61]]);
    assert_eq!(note(p, "regioneast").bbox(), Some([5, -3, 640, 480]));
    assert_eq!(note(p, "notecrev22").shape.points(), vec![[165, 45]]);
    assert_eq!(
        note(p, "pathcrev22").shape.points(),
        vec![[21, 61], [305, 61]]
    );
    let r = s
        .call(
            "notes.move",
            json!({ "ids": ["pinstatue2"], "delta": [1, 1] }),
        )
        .unwrap();
    assert_eq!(r["label"], "claude-code: Move note pinstatue2");
    // Locked ones need confirm; unknown ids, duplicates and empty lists fail.
    let e = err(s.call(
        "notes.move",
        json!({ "ids": ["pinstatue2", "guidestem2"], "delta": [1, 0] }),
    ));
    assert_eq!(e.code, code::NEEDS_CONFIRM);
    let e = err(s.call(
        "notes.move",
        json!({ "ids": ["nosuchnote"], "delta": [1, 0] }),
    ));
    assert_eq!(e.code, code::COMMAND_ERROR);
    assert_eq!(e.data.unwrap()["kind"], "unknown_note");
    for p in [
        json!({ "ids": ["pinstatue2", "pinstatue2"], "delta": [1, 0] }),
        json!({ "ids": [], "delta": [1, 0] }),
        json!({ "ids": ["pinstatue2"] }),
    ] {
        assert_eq!(err(s.call("notes.move", p)).code, code::INVALID_PARAMS);
    }
    // Undo puts every point back.
    s.call("history.undo", json!({ "n": 2 })).unwrap();
    assert_eq!(
        note(s.project(), "pinstatue2").shape.points(),
        vec![[96, 64]]
    );
}

#[test]
fn guide_groups_set_and_remove() {
    let (_t, mut s) = session();
    s.call("events.subscribe", json!({ "events": ["changed"] }))
        .unwrap();
    let r = s
        .call(
            "notes.set_guide_group",
            json!({ "group": { "slug": "ruined", "name": "Ruined wing", "offset": [0, 769], "color": "#c08040" } }),
        )
        .unwrap();
    assert_eq!(r["label"], "claude-code: Set guide group ruined");
    assert_eq!(
        r["changes"],
        json!([{ "change": "guide_group", "world": "main", "slug": "ruined" }])
    );
    let ev = s.take_events();
    assert_eq!(ev[0]["params"]["files"], json!(["worlds/main/world.json"]));
    s.call(
        "notes.set_guide_group",
        json!({ "world": "main", "group": { "slug": "other" }, "index": 0 }),
    )
    .unwrap();
    let w = &s.project().worlds[0];
    assert_eq!(w.guide_groups[0].slug.as_str(), "other");
    assert_eq!(w.guide_groups[1].offset, [0, 769]);
    // Used by a guide: refused; unused: removed.
    s.call(
        "notes.update",
        json!({ "id": "guidestem2", "patch": { "group": "ruined" }, "confirm": true }),
    )
    .unwrap();
    let e = err(s.call("notes.remove_guide_group", json!({ "slug": "ruined" })));
    assert_eq!(e.code, code::COMMAND_ERROR);
    assert_eq!(e.data.unwrap()["kind"], "invalid");
    let r = s
        .call("notes.remove_guide_group", json!({ "slug": "other" }))
        .unwrap();
    assert_eq!(r["label"], "claude-code: Remove guide group other");
    let e = err(s.call("notes.remove_guide_group", json!({ "slug": "nope" })));
    assert_eq!(e.data.unwrap()["kind"], "unknown_guide_group");
    let e = err(s.call(
        "notes.set_guide_group",
        json!({ "world": "elsewhere", "group": { "slug": "x" } }),
    ));
    assert_eq!(e.data.unwrap()["kind"], "unknown_world");
}

#[test]
fn level_tags_through_level_update() {
    let (_t, mut s) = session();
    s.call("events.subscribe", json!({ "events": ["changed"] }))
        .unwrap();
    let r = s
        .call(
            "level.update",
            json!({ "level": "crevasse", "tags": ["forest", "ruined"] }),
        )
        .unwrap();
    assert_eq!(r["label"], "claude-code: Tag level crevasse");
    assert_eq!(
        r["changes"],
        json!([{ "change": "level_meta", "world": "main", "level": "crevasse", "part": "tags" }])
    );
    let ev = s.take_events();
    assert_eq!(
        ev[0]["params"]["files"],
        json!(["worlds/main/levels/crevasse/level.json"])
    );
    let d = s.call("project.describe", json!({})).unwrap();
    assert_eq!(d["levels"][0]["tags"], json!(["forest", "ruined"]));
    assert_eq!(d["levels"][1]["tags"], json!([]));
    // With a rename, the tags go to the new slug in the same batch.
    let r = s
        .call(
            "level.update",
            json!({ "level": "crevasse", "slug": "chasm", "tags": [] }),
        )
        .unwrap();
    assert_eq!(s.history().undo_entries().len(), 2);
    assert!(
        r["changes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["part"] == "tags" && c["level"] == "chasm")
    );
    assert!(s.project().level("chasm").unwrap().tags.is_empty());
    let e = err(s.call("level.update", json!({ "level": "chasm" })));
    assert!(e.message.contains("tags"), "{}", e.message);
}

#[test]
fn describe_counts_guides_apart() {
    let (_t, mut s) = session();
    let d = s.call("project.describe", json!({})).unwrap();
    assert_eq!(d["notes"], json!({ "open": 3, "done": 1 }));
    let w = &d["worlds"][0];
    assert_eq!(
        (w["notes"].clone(), w["guides"].clone()),
        (json!(2), json!(1))
    );
    assert_eq!(w["guide_groups"], json!([]));
    add_guides(&mut s);
    let d = s.call("project.describe", json!({})).unwrap();
    assert_eq!(
        d["notes"],
        json!({ "open": 3, "done": 1 }),
        "guides don't count"
    );
    let w = &d["worlds"][0];
    assert_eq!(w["notes"], 2);
    assert_eq!(w["guides"], 5);
    assert_eq!(
        w["guide_groups"],
        json!([{ "slug": "shifted", "offset": [0, 769], "guides": 2 }])
    );
}

#[test]
fn changed_names_world_json_for_world_notes() {
    let (_t, mut s) = session();
    s.call("events.subscribe", json!({ "events": ["changed"] }))
        .unwrap();
    s.call(
        "notes.move",
        json!({ "ids": ["pinstatue2", "notecrev22"], "delta": [1, 0] }),
    )
    .unwrap();
    let ev = s.take_events();
    assert_eq!(
        ev[0]["params"]["files"],
        json!([
            "worlds/main/world.json",
            "worlds/main/levels/crevasse/notes.json"
        ])
    );
    s.call(
        "notes.add",
        json!({ "scope": { "world": "main" }, "note": { "kind": "guide", "points": [[0, 0], [0, 64]] } }),
    )
    .unwrap();
    let ev = s.take_events();
    assert_eq!(ev[0]["params"]["files"], json!(["worlds/main/world.json"]));
}

/// A stand-in editor: the headless document plus a record of what the view
/// was asked to do.
struct ViewHost {
    inner: HeadlessHost,
    focused: Vec<ViewTarget>,
    selected: Vec<Selection>,
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
    fn focus(&mut self, target: &ViewTarget) -> Result<(), RpcError> {
        self.focused.push(target.clone());
        Ok(())
    }
    fn select(&mut self, selection: &Selection) -> Result<(), RpcError> {
        self.selected.push(selection.clone());
        Ok(())
    }
}

fn view() -> (tempfile::TempDir, Handler, ViewHost, Client) {
    let (tmp, dir) = common::mini();
    let host = ViewHost {
        inner: HeadlessHost::open(&dir).unwrap(),
        focused: Vec::new(),
        selected: Vec::new(),
    };
    (
        tmp,
        Handler::new(HandlerConfig::default()),
        host,
        Client::in_process("claude-code"),
    )
}

fn reply(o: Outcome) -> Result<Value, RpcError> {
    match o {
        Outcome::Reply(r) => r,
        Outcome::Pending { .. } => panic!("pending"),
    }
}

#[test]
fn view_select_adds_up_and_checks_every_id() {
    let (_t, mut h, mut host, c) = view();
    reply(h.call(
        &mut host,
        &c,
        "view.select",
        json!({ "entities": ["doorcrev22"], "notes": ["pinstatue2", "notecrev22"], "levels": ["shaft"] }),
    ))
    .unwrap();
    let sel = host.selected.last().unwrap();
    assert_eq!(sel.level, None);
    assert_eq!(sel.entities.len(), 1);
    assert_eq!(sel.notes.len(), 2);
    assert_eq!(sel.levels[0].as_str(), "shaft");
    // Empty lists are fine (clears); level alone; level with a list is not.
    reply(h.call(&mut host, &c, "view.select", json!({ "notes": [] }))).unwrap();
    assert_eq!(host.selected.last().unwrap(), &Selection::default());
    reply(h.call(&mut host, &c, "view.select", json!({ "level": "shaft" }))).unwrap();
    let e = reply(h.call(
        &mut host,
        &c,
        "view.select",
        json!({ "level": "shaft", "levels": ["crevasse"] }),
    ))
    .unwrap_err();
    assert_eq!(e.code, code::INVALID_PARAMS);
    // Unknown ids never reach the editor.
    let n = host.selected.len();
    for (p, kind) in [
        (json!({ "entities": ["nosuchthin"] }), "unknown_entity"),
        (
            json!({ "notes": ["pinstatue2", "nosuchnote"] }),
            "unknown_note",
        ),
        (json!({ "levels": ["atlantis"] }), "unknown_level"),
        (json!({ "level": "atlantis" }), "unknown_level"),
    ] {
        let e = reply(h.call(&mut host, &c, "view.select", p)).unwrap_err();
        assert_eq!(e.code, code::COMMAND_ERROR);
        assert_eq!(e.data.unwrap()["kind"], kind);
    }
    assert_eq!(host.selected.len(), n);
}

#[test]
fn view_focus_note_reaches_the_editor_as_a_note_target() {
    let (_t, mut h, mut host, c) = view();
    reply(h.call(&mut host, &c, "view.focus", json!({ "note": "notecrev22" }))).unwrap();
    assert_eq!(
        host.focused,
        vec![ViewTarget::Note(
            worlds_core::Id::new("notecrev22").unwrap()
        )]
    );
    let e =
        reply(h.call(&mut host, &c, "view.focus", json!({ "note": "nosuchnote" }))).unwrap_err();
    assert_eq!(e.data.unwrap()["kind"], "unknown_note");
    for p in [
        json!({ "note": "pinstatue2", "level": "crevasse" }),
        json!({ "note": "pinstatue2", "world_rect": [0, 0, 10, 10] }),
    ] {
        let e = reply(h.call(&mut host, &c, "view.focus", p)).unwrap_err();
        assert_eq!(e.code, code::INVALID_PARAMS);
    }
    assert_eq!(
        host.focused.len(),
        1,
        "refused requests never reach the editor"
    );
    assert_eq!(h.rev(), 0, "view.* never moves rev");
}

#[test]
fn selection_events_carry_levels() {
    let (_t, mut h, mut host, c) = view();
    reply(h.call(
        &mut host,
        &c,
        "events.subscribe",
        json!({ "events": ["selection"] }),
    ))
    .unwrap();
    h.notify_selection(
        Some("crevasse"),
        &[],
        &["pinstatue2".to_string()],
        &["crevasse".to_string(), "shaft".to_string()],
    );
    let out = h.take_out();
    let Out::Notify { message, .. } = &out[0] else {
        panic!("{out:?}")
    };
    assert_eq!(message["method"], "selection");
    assert_eq!(
        message["params"],
        json!({ "level": "crevasse", "entities": [], "notes": ["pinstatue2"], "levels": ["crevasse", "shaft"] })
    );
}

/// Connectors over the API: `notes.add` takes the kind, `notes.list`
/// filters by it, a locked one needs `confirm` to change its ends or
/// waypoints but not its label or style, and `project.validate` reports a
/// dangling end.
#[test]
fn connectors_are_added_listed_and_locked_like_any_note() {
    let (_t, mut s) = session();
    let r = s
        .call(
            "notes.add",
            json!({ "scope": { "world": "main" }, "note": { "id": "connshaft2", "kind": "connector",
                "label": "one-way drop", "locked": true,
                "from": { "level": "crevasse" }, "to": { "entity": "doorshaft2" },
                "waypoints": [[360, 40]] } }),
        )
        .unwrap();
    assert_eq!(r["label"], "claude-code: Add connector");
    let l = s
        .call("notes.list", json!({ "kind": "connector" }))
        .unwrap();
    assert_eq!(ids(&l), ["connshaft2"]);
    assert_eq!(
        l["notes"][0]["note"]["from"],
        json!({ "level": "crevasse" })
    );
    let l = s
        .call("notes.list", json!({ "kinds": ["note", "connector"] }))
        .unwrap();
    assert!(ids(&l).contains(&"connshaft2".to_string()));
    // Locked: a new end or waypoint is a move, a label or style isn't.
    for patch in [
        json!({ "to": { "level": "shaft" } }),
        json!({ "waypoints": [[360, 32]] }),
    ] {
        assert_eq!(
            err(s.call(
                "notes.update",
                json!({ "id": "connshaft2", "patch": patch })
            ))
            .code,
            code::NEEDS_CONFIRM
        );
    }
    for patch in [json!({ "label": "drop" }), json!({ "style": "double" })] {
        s.call(
            "notes.update",
            json!({ "id": "connshaft2", "patch": patch }),
        )
        .unwrap();
    }
    s.call(
        "notes.move",
        json!({ "ids": ["connshaft2"], "delta": [0, -8], "confirm": true }),
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(note(s.project(), "connshaft2")).unwrap(),
        json!({ "id": "connshaft2", "kind": "connector", "locked": true, "label": "drop",
                "from": { "level": "crevasse" }, "to": { "entity": "doorshaft2" },
                "waypoints": [[360, 32]], "style": "double" })
    );
    // A wrong key is refused; a dangling end is a validation problem.
    assert!(
        s.call(
            "notes.add",
            json!({ "scope": { "world": "main" }, "note": { "kind": "connector",
                "from": { "level": "crevasse" }, "to": { "level": "shaft" }, "points": [[0, 0]] } }),
        )
        .is_err()
    );
    s.call(
        "notes.add",
        json!({ "scope": { "world": "main" }, "note": { "id": "conngone22", "kind": "connector",
            "from": { "level": "crevasse" }, "to": { "level": "nowhere" } } }),
    )
    .unwrap();
    let v = s.call("project.validate", json!({})).unwrap();
    assert!(
        v.to_string()
            .contains("level \\\"nowhere\\\" does not exist: the connector's end dangles"),
        "{v}"
    );
}
