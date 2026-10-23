//! The handler, in-process: revisions, confirmation, batches and the undo
//! history, events, text blocks, queries, error codes.

mod common;

use std::time::Duration;

use serde_json::{Value, json};
use worlds_core::command::{CellValue, Change};
use worlds_core::{Command, slug};
use worlds_server::headless::{HeadlessHost, Session};
use worlds_server::rpc::code;
use worlds_server::{ChangeKind, Client, Handler, HandlerConfig, Out, Outcome, UserChange};

fn session() -> (tempfile::TempDir, Session) {
    let (tmp, dir) = common::mini();
    (tmp, Session::open(&dir).unwrap().client("claude-code"))
}

fn err_code(r: Result<Value, worlds_server::RpcError>) -> i64 {
    r.expect_err("expected an error").code
}

fn cell(s: &Session, level: &str, x: u32, y: u32) -> Option<String> {
    let p = s.project();
    let def = p.layer("collision").unwrap();
    p.level(level)
        .unwrap()
        .int_grid("collision")
        .and_then(|g| g.value_at(def, x, y))
        .map(|v| v.to_string())
}

fn fill(x: u32, y: u32, value: &str) -> Value {
    json!({ "level": "crevasse", "layer": "collision", "rect": [x, y, 1, 1], "value": value })
}

/// Two clients on one handler, for the multi-client rules.
struct Multi {
    _tmp: tempfile::TempDir,
    h: Handler,
    host: HeadlessHost,
    a: Client,
    b: Client,
}

impl Multi {
    fn new() -> Self {
        let (tmp, dir) = common::mini();
        let mk = |conn, name: &str| Client {
            conn,
            name: name.into(),
            version: "1".into(),
            session: format!("sess{conn}"),
            approval_wait: Duration::ZERO,
        };
        Multi {
            _tmp: tmp,
            h: Handler::new(HandlerConfig::default()),
            host: HeadlessHost::open(&dir).unwrap(),
            a: mk(1, "claude-code"),
            b: mk(2, "other-agent"),
        }
    }

    fn call(
        &mut self,
        who: char,
        method: &str,
        params: Value,
    ) -> Result<Value, worlds_server::RpcError> {
        let c = if who == 'a' {
            self.a.clone()
        } else {
            self.b.clone()
        };
        match self.h.call(&mut self.host, &c, method, params) {
            Outcome::Reply(r) => r,
            Outcome::Pending { batch_id } => Ok(json!({ "pending": batch_id })),
        }
    }

    /// The user paints a cell in the editor: flush agent batches, record,
    /// announce.
    fn user_paint(&mut self, x: u32, y: u32, value: &str) {
        self.h.flush_batches(&mut self.host.history);
        let changes = self
            .host
            .history
            .push(
                &mut self.host.project,
                Command::PaintCells {
                    level: slug("crevasse"),
                    layer: slug("collision"),
                    cells: vec![CellValue::new(x, y, Some(slug(value)))],
                    keep_empty: false,
                },
            )
            .unwrap();
        self.h.notify_changed(UserChange {
            kind: ChangeKind::Edit,
            label: "Paint collision",
            entry_origin: None,
            changes: &changes,
        });
    }

    fn notifications(&mut self, conn: u64, event: &str) -> Vec<Value> {
        self.h
            .take_out()
            .into_iter()
            .filter_map(|o| match o {
                Out::Notify { conn: c, message } if c == conn && message["method"] == event => {
                    Some(message["params"].clone())
                }
                _ => None,
            })
            .collect()
    }
}

#[test]
fn stale_expect_rev_applies_nothing() {
    let (_t, mut s) = session();
    s.call("grid.fill_rect", fill(1, 1, "solid")).unwrap();
    let before = s.project().clone();
    let e = s
        .call("grid.fill_rect", json!({ "level": "crevasse", "layer": "collision", "rect": [2, 1, 1, 1], "value": "water", "expect_rev": 0 }))
        .unwrap_err();
    assert_eq!(e.code, code::STALE_REV);
    assert_eq!(e.data.unwrap(), json!({ "expected": 0, "current": 1 }));
    assert_eq!(*s.project(), before);
    // The right rev goes through.
    let mut p = fill(2, 1, "water");
    p["expect_rev"] = json!(1);
    assert_eq!(s.call("grid.fill_rect", p).unwrap()["rev"], 2);
    // A no-op is not a mutation.
    let r = s.call("grid.fill_rect", fill(2, 1, "water")).unwrap();
    assert_eq!(
        (r["rev"].clone(), r["changes"].clone()),
        (json!(2), json!([]))
    );
    assert_eq!(s.history().undo_entries().len(), 2);
}

#[test]
fn destructive_methods_need_confirm() {
    let (_t, mut s) = session();
    let e = s
        .call("level.delete", json!({ "level": "shaft" }))
        .unwrap_err();
    assert_eq!(e.code, code::NEEDS_CONFIRM);
    assert_eq!(e.data.unwrap()["reason"], "deletes level shaft");
    assert!(s.project().level("shaft").is_some());
    s.call("level.delete", json!({ "level": "shaft", "confirm": true }))
        .unwrap();
    assert!(s.project().level("shaft").is_none());

    // Taking out a field instances store a value for.
    let mut def = s
        .call(
            "defs.get",
            json!({ "kind": "entity", "slug": "floor_button" }),
        )
        .unwrap()["def"]
        .clone();
    def["fields"]
        .as_array_mut()
        .unwrap()
        .retain(|f| f["slug"] != "channel");
    let e = s
        .call("defs.set", json!({ "kind": "entity", "def": def }))
        .unwrap_err();
    assert_eq!(e.code, code::NEEDS_CONFIRM);
    assert_eq!(
        e.data.unwrap()["reason"],
        "1 entity stores a value for a field that no longer exists or becomes invalid: channel"
    );
    s.call(
        "defs.set",
        json!({ "kind": "entity", "def": def, "confirm": true }),
    )
    .unwrap();
    // Unsaved edits: reload needs confirm.
    let e = s.call("project.reload", json!({})).unwrap_err();
    assert_eq!(e.code, code::NEEDS_CONFIRM);
    let r = s
        .call("project.reload", json!({ "confirm": true }))
        .unwrap();
    assert!(s.project().level("shaft").is_some());
    assert_eq!(r["rev"], 3);
    assert!(s.history().undo_entries().is_empty());
}

#[test]
fn a_batch_is_one_undo_entry() {
    let (_t, mut s) = session();
    s.call(
        "history.begin_batch",
        json!({ "label": "Agent: two walls" }),
    )
    .unwrap();
    assert_eq!(
        err_code(s.call("history.begin_batch", json!({ "label": "x" }))),
        code::BATCH_STATE
    );
    let r1 = s.call("grid.fill_rect", fill(1, 1, "solid")).unwrap();
    let r2 = s.call("grid.fill_rect", fill(2, 1, "solid")).unwrap();
    assert_eq!((r1["rev"].clone(), r2["rev"].clone()), (json!(1), json!(2)));
    assert_eq!(r2["label"], "Agent: two walls");
    assert!(
        s.history().undo_entries().is_empty(),
        "recorded at end_batch"
    );
    let end = s.call("history.end_batch", json!({})).unwrap();
    assert_eq!(end["entries"], 1);
    assert_eq!(end["rev"], 2);
    assert_eq!(end["changes"][0]["rect"], json!([1, 1, 2, 1]));
    assert_eq!(s.history().undo_entries().len(), 1);
    assert_eq!(s.history().undo_entries()[0].label, "Agent: two walls");
    let u = s.call("history.undo", json!({})).unwrap();
    assert_eq!(u["steps"].as_array().unwrap().len(), 1);
    assert_eq!(
        (cell(&s, "crevasse", 1, 1), cell(&s, "crevasse", 2, 1)),
        (None, None)
    );
    assert_eq!(
        err_code(s.call("history.end_batch", json!({}))),
        code::BATCH_STATE
    );
    // An empty batch records nothing.
    s.call("history.begin_batch", json!({ "label": "nothing" }))
        .unwrap();
    assert_eq!(
        s.call("history.end_batch", json!({})).unwrap()["entries"],
        0
    );
}

#[test]
fn a_user_edit_mid_batch_splits_it() {
    let mut m = Multi::new();
    m.call(
        'a',
        "history.begin_batch",
        json!({ "label": "Agent: walls" }),
    )
    .unwrap();
    m.call('a', "grid.fill_rect", fill(1, 1, "solid")).unwrap();
    m.user_paint(10, 1, "water");
    m.call('a', "grid.fill_rect", fill(2, 1, "solid")).unwrap();
    let end = m.call('a', "history.end_batch", json!({})).unwrap();
    assert_eq!(end["entries"], 2);
    assert_eq!(end["rev"], 3);
    let labels: Vec<&str> = m
        .host
        .history
        .undo_entries()
        .iter()
        .map(|e| e.label.as_str())
        .collect();
    assert_eq!(
        labels,
        ["Agent: walls", "Paint 1 cell on collision", "Agent: walls"]
    );
    // Each entry undoes only its own part.
    m.host.history.undo(&mut m.host.project).unwrap();
    let def = m.host.project.layer("collision").unwrap().clone();
    let at = |m: &Multi, x| {
        m.host
            .project
            .level("crevasse")
            .unwrap()
            .int_grid("collision")
            .unwrap()
            .value_at(&def, x, 1)
            .map(|v| v.to_string())
    };
    assert_eq!(
        (at(&m, 1), at(&m, 2), at(&m, 10)),
        (Some("solid".into()), None, Some("water".into()))
    );
}

#[test]
fn a_dropped_connection_reverts_its_unrecorded_part() {
    let mut m = Multi::new();
    m.call(
        'b',
        "events.subscribe",
        json!({ "events": ["changed", "agent"] }),
    )
    .unwrap();
    m.call(
        'a',
        "history.begin_batch",
        json!({ "label": "Agent: walls" }),
    )
    .unwrap();
    m.call('a', "grid.fill_rect", fill(1, 1, "solid")).unwrap();
    m.user_paint(10, 1, "water"); // records the agent's part so far
    m.call('a', "grid.fill_rect", fill(2, 1, "solid")).unwrap();
    m.h.take_out();
    let rev = m.h.rev();
    m.h.disconnect(&mut m.host, 1);
    assert_eq!(m.h.rev(), rev + 1);
    let def = m.host.project.layer("collision").unwrap().clone();
    let g = m
        .host
        .project
        .level("crevasse")
        .unwrap()
        .int_grid("collision")
        .unwrap();
    assert_eq!(
        g.value_at(&def, 1, 1).map(|v| v.as_str()),
        Some("solid"),
        "recorded part stays"
    );
    assert_eq!(g.value_at(&def, 2, 1), None, "unrecorded part reverted");
    assert_eq!(
        g.value_at(&def, 10, 1).map(|v| v.as_str()),
        Some("water"),
        "the user's edit stays"
    );
    assert_eq!(m.host.history.undo_entries().len(), 2);
    let out = m.h.take_out();
    let kinds: Vec<(Value, Value)> = out
        .iter()
        .filter_map(|o| match o {
            Out::Notify { conn: 2, message } => {
                Some((message["method"].clone(), message["params"].clone()))
            }
            _ => None,
        })
        .map(|(m, p)| {
            (
                m,
                p.get("kind")
                    .or(p.get("status"))
                    .cloned()
                    .unwrap_or(Value::Null),
            )
        })
        .collect();
    assert_eq!(
        kinds,
        [
            (json!("changed"), json!("cancel")),
            (json!("agent"), json!("cancelled"))
        ]
    );
    // Its batch is gone.
    assert_eq!(m.h.open_batches().count(), 0);
}

#[test]
fn undo_stops_at_other_clients_entries() {
    let mut m = Multi::new();
    m.call('a', "grid.fill_rect", fill(1, 1, "solid")).unwrap();
    m.call('b', "grid.fill_rect", fill(2, 1, "solid")).unwrap();
    let r = m.call('a', "history.undo", json!({ "n": 5 })).unwrap();
    assert_eq!(r["steps"], json!([]));
    assert_eq!(r["stopped"]["origin"], json!({ "agent": "other-agent" }));
    assert_eq!(r["rev"], 2);
    let r = m
        .call('a', "history.undo", json!({ "n": 5, "any": true }))
        .unwrap();
    assert_eq!(r["steps"].as_array().unwrap().len(), 2);
    assert_eq!(r["rev"], 4);
    let r = m.call('b', "history.redo", json!({ "n": 2 })).unwrap();
    assert_eq!(r["steps"], json!([]), "the next redo is a's");
    let r = m
        .call('a', "history.redo", json!({ "expect_rev": 4 }))
        .unwrap();
    assert_eq!(r["steps"][0]["origin"], json!({ "agent": "claude-code" }));
}

#[test]
fn changed_events_carry_rev_origin_session_and_files() {
    let mut m = Multi::new();
    m.call('a', "events.subscribe", json!({ "events": ["changed"] }))
        .unwrap();
    m.call('b', "grid.fill_rect", fill(1, 1, "solid")).unwrap();
    let ev = m.notifications(1, "changed");
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0]["rev"], 1);
    assert_eq!(ev[0]["origin"], json!({ "agent": "other-agent" }));
    assert_eq!(ev[0]["session"], "sess2");
    assert_eq!(ev[0]["kind"], "edit");
    assert_eq!(
        ev[0]["files"],
        json!(["worlds/main/levels/crevasse/collision.json"])
    );
    // The user's undo of it: origin user, entry_origin the agent.
    m.h.flush_batches(&mut m.host.history);
    let changes: Vec<Change> = m.host.history.undo(&mut m.host.project).unwrap().unwrap();
    m.h.notify_changed(UserChange {
        kind: ChangeKind::Undo,
        label: "other-agent: Fill rectangle on collision",
        entry_origin: Some(worlds_core::command::Origin::Agent("other-agent".into())),
        changes: &changes,
    });
    let ev = m.notifications(1, "changed");
    assert_eq!(ev[0]["origin"], "user");
    assert_eq!(ev[0]["entry_origin"], json!({ "agent": "other-agent" }));
    assert_eq!(ev[0]["rev"], 2);
    // Unsubscribed: nothing.
    m.call('a', "events.unsubscribe", json!({ "events": ["changed"] }))
        .unwrap();
    m.call('b', "grid.fill_rect", fill(1, 1, "solid")).unwrap();
    assert!(m.notifications(1, "changed").is_empty());
    assert_eq!(
        err_code(m.call('a', "events.subscribe", json!({ "events": ["everything"] }))),
        code::INVALID_PARAMS
    );
}

#[test]
fn text_blocks_paint_shapes() {
    let (_t, mut s) = session();
    // Row 0 paints 3 cells, '*' and ' ' skip, a short row stops early,
    // the legend adds a glyph.
    let r = s
        .call(
            "grid.set_cells",
            json!({ "level": "crevasse", "layer": "collision", "origin": [2, 1],
            "text": "##w\n* .\nw\n", "legend": { "w": "water" } }),
        )
        .unwrap();
    assert_eq!(r["label"], "claude-code: Paint 5 cells on collision");
    assert_eq!(cell(&s, "crevasse", 2, 1).as_deref(), Some("solid"));
    assert_eq!(cell(&s, "crevasse", 4, 1).as_deref(), Some("water"));
    assert_eq!(cell(&s, "crevasse", 2, 3).as_deref(), Some("water"));
    assert_eq!(cell(&s, "crevasse", 2, 2), None, "skipped");
    assert_eq!(
        cell(&s, "crevasse", 1, 3).as_deref(),
        Some("ramp_l"),
        "outside the block"
    );
    // A glyph nobody defines; a cell outside; both forms.
    let bad = |s: &mut Session, p: Value| err_code(s.call("grid.set_cells", p));
    assert_eq!(
        bad(
            &mut s,
            json!({ "level": "crevasse", "layer": "collision", "origin": [0, 0], "text": "?" })
        ),
        code::INVALID_PARAMS
    );
    assert_eq!(
        bad(
            &mut s,
            json!({ "level": "crevasse", "layer": "collision", "origin": [19, 0], "text": "##" })
        ),
        code::COMMAND_ERROR
    );
    assert_eq!(
        bad(
            &mut s,
            json!({ "level": "crevasse", "layer": "collision", "text": "#" })
        ),
        code::INVALID_PARAMS
    );
    assert_eq!(
        bad(&mut s, json!({ "level": "crevasse", "layer": "collision" })),
        code::INVALID_PARAMS
    );
    assert_eq!(
        bad(
            &mut s,
            json!({ "level": "crevasse", "layer": "decor_back", "origin": [0, 0], "text": "#" })
        ),
        code::COMMAND_ERROR
    );
    // The read side round-trips it.
    let g = s
        .call(
            "grid.get_cells",
            json!({ "level": "crevasse", "layer": "collision", "rect": [2, 1, 3, 3] }),
        )
        .unwrap();
    assert_eq!(g["rows"], json!(["##~", "...", "~.~"]));
}

#[test]
fn entity_find_filters() {
    let (_t, mut s) = session();
    let ids = |v: Value| -> Vec<String> {
        v["entities"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["id"].as_str().unwrap().to_string())
            .collect()
    };
    let find = |s: &mut Session, p: Value| ids(s.call("entity.find", p).unwrap());
    assert_eq!(
        find(&mut s, json!({ "type": "locked_door" })),
        ["doorcrev22", "doorshaft2"]
    );
    assert_eq!(
        find(&mut s, json!({ "tag": "puzzle", "level": "shaft" })),
        ["doorshaft2"]
    );
    // Compared to the effective value: the default counts.
    assert_eq!(
        find(&mut s, json!({ "fields": { "facing": "left" } })),
        ["doorshaft2"]
    );
    assert_eq!(
        find(
            &mut s,
            json!({ "fields": { "channel": 3, "latching": true } })
        ),
        ["btncrev222"]
    );
    assert_eq!(
        find(
            &mut s,
            json!({ "level": "crevasse", "rect": [0, 0, 13, 5] })
        ),
        ["btncrev222"]
    );
    assert!(find(&mut s, json!({ "tag": "nope" })).is_empty());
    assert_eq!(
        err_code(s.call("entity.find", json!({ "rect": [0, 0, 1, 1] }))),
        code::INVALID_PARAMS
    );
    // entity.list and get agree.
    assert_eq!(
        ids(s
            .call("entity.list", json!({ "level": "crevasse" }))
            .unwrap()),
        ["doorcrev22", "btncrev222"]
    );
    let g = s.call("entity.get", json!({ "id": "btncrev222" })).unwrap();
    assert_eq!(
        (g["index"].clone(), g["effective_fields"]["target"].clone()),
        (json!(1), json!("doorcrev22"))
    );
}

#[test]
fn describe_has_the_documented_shape() {
    let (_t, mut s) = session();
    let d = s.call("project.describe", json!({})).unwrap();
    for k in [
        "rev",
        "name",
        "dir",
        "grid_size",
        "bg_color",
        "collision_layer",
        "seed",
        "mode",
        "level_fields",
        "worlds",
        "levels",
        "layers",
        "entity_types",
        "enums",
        "tilesets",
        "notes",
    ] {
        assert!(d.get(k).is_some(), "describe lacks {k}");
    }
    assert_eq!(
        d["mode"],
        json!({ "headless": true, "approval": false, "pending": 0 })
    );
    assert_eq!(d["dir"], json!(s.dir().to_string_lossy()));
    assert!(d["level_fields"][0].get("name").is_none());
    assert_eq!(d["worlds"][0]["levels"], json!(["crevasse", "shaft"]));
    let crevasse = &d["levels"][0];
    assert_eq!(crevasse["size_px"], json!([320, 80]));
    assert_eq!(crevasse["content"]["collision"], json!({ "cells": 59 }));
    assert_eq!(crevasse["content"]["objects"], json!({ "entities": 2 }));
    assert_eq!(crevasse["notes"], json!({ "open": 1, "done": 1 }));
    let layers = d["layers"].as_array().unwrap();
    assert_eq!(
        layers[0]["values"][0],
        json!({ "slug": "solid", "glyph": "#", "color": "#404040", "group": "walls" })
    );
    assert_eq!(layers[1]["source"], "collision");
    assert_eq!(layers[2]["grid_size"], 8);
    let door = &d["entity_types"][1];
    assert_eq!(
        (door["slug"].clone(), door["count"].clone()),
        (json!("locked_door"), json!(2))
    );
    assert!(
        door["fields"]
            .as_array()
            .unwrap()
            .iter()
            .all(|f| f.get("display").is_none() && f.get("name").is_none())
    );
    assert_eq!(d["tilesets"][0]["atlas"], json!([32, 32]));
    assert_eq!(d["tilesets"][1]["atlas"], json!([8, 8]));
    assert_eq!(d["tilesets"][1]["spacing"], 1);
    // The guide (guidestem2) is counted apart, not as an open note.
    assert_eq!(d["notes"], json!({ "open": 3, "done": 1 }));
}

#[test]
fn every_error_code() {
    let (_t, mut s) = session();
    assert_eq!(
        err_code(s.call("level.frobnicate", json!({}))),
        code::METHOD_NOT_FOUND
    );
    assert_eq!(
        err_code(s.call("level.list", json!({ "bogus": 1 }))),
        code::INVALID_PARAMS
    );
    assert_eq!(
        err_code(s.call("level.list", json!([1]))),
        code::INVALID_PARAMS
    );
    assert_eq!(
        err_code(s.call(
            "grid.fill_rect",
            json!({ "level": "crevasse", "layer": "collision", "value": "solid" })
        )),
        code::INVALID_PARAMS
    );
    let e = s.call("grid.fill_rect", json!({ "level": "nowhere", "layer": "collision", "rect": [0, 0, 1, 1], "value": "solid" })).unwrap_err();
    assert_eq!(
        (e.code, e.data.unwrap()["kind"].clone()),
        (code::COMMAND_ERROR, json!("unknown_level"))
    );
    let e = s
        .call(
            "level.move",
            json!({ "level": "crevasse", "world": "other", "world_px": [0, 0] }),
        )
        .unwrap_err();
    assert_eq!(e.data.unwrap()["kind"], "level_not_in_world");
    assert_eq!(
        err_code(s.call("view.focus", json!({ "level": "crevasse" }))),
        code::UNSUPPORTED
    );
    assert_eq!(
        err_code(s.call("defs.set", json!({ "kind": "level_field", "def": {} }))),
        code::INVALID_PARAMS
    );
    assert_eq!(
        err_code(s.call("defs.set", json!({ "kind": "macros", "def": {} }))),
        code::INVALID_PARAMS
    );
    assert_eq!(
        err_code(s.call("defs.set", json!({ "kind": "rules", "def": {} }))),
        code::INVALID_PARAMS
    );
    assert_eq!(
        err_code(s.call("history.end_batch", json!({}))),
        code::BATCH_STATE
    );
    assert_eq!(
        err_code(s.call("project.read_file", json!({ "path": "../secret" }))),
        code::COMMAND_ERROR
    );
    assert_eq!(
        err_code(s.call("history.batch_status", json!({ "batch_id": "b9" }))),
        code::COMMAND_ERROR
    );
    // io: a file sits where a new level's directory must go.
    s.call("level.create", json!({ "level": { "slug": "annex", "name": "Annex", "world_px": [400, 0], "size": [4, 4], "depth": 0 } }))
        .unwrap();
    std::fs::write(s.dir().join("worlds/main/levels/annex"), "in the way").unwrap();
    let e = s.call("project.save", json!({})).unwrap_err();
    assert_eq!(e.code, code::IO);
    assert!(e.data.unwrap()["path"].as_str().unwrap().contains("annex"));
}

#[test]
fn creating_methods_report_generated_ids() {
    let (_t, mut s) = session();
    let r = s
        .call(
            "entity.add",
            json!({ "level": "shaft", "entity": { "type": "floor_button", "cell": [1, 7] } }),
        )
        .unwrap();
    let id = r["id"].as_str().unwrap().to_string();
    assert_eq!(id.len(), 10);
    assert_eq!(
        s.call("entity.get", json!({ "id": id })).unwrap()["entity"]["level"],
        "shaft"
    );
    let r = s
        .call(
            "notes.add",
            json!({ "scope": { "world": "main" }, "note": { "kind": "pin", "px": [5, 5] } }),
        )
        .unwrap();
    assert_eq!(r["id"].as_str().unwrap().len(), 10);
    let notes = s
        .call(
            "notes.list",
            json!({ "scope": { "world": "main" }, "kind": "pin" }),
        )
        .unwrap();
    assert!(
        notes["notes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["note"]["id"] == r["id"])
    );
    let r = s.call("tiles.stamp", json!({ "level": "shaft", "layer": "decor_back", "at": [0, 0], "tileset": "ssw_set5", "rect": [0, 0, 2, 1] })).unwrap();
    // ssw_set5 tiles are 32 px on an 8 px layer: 4 cells apart.
    assert_eq!(r["changes"][0]["rect"], json!([0, 0, 5, 1]));
}

#[test]
fn an_idle_batch_ends_by_itself() {
    use std::time::Instant;
    let mut m = Multi::new();
    m.call(
        'a',
        "history.begin_batch",
        json!({ "label": "Agent: slow" }),
    )
    .unwrap();
    m.call('a', "grid.fill_rect", fill(1, 1, "solid")).unwrap();
    let idle = Duration::from_secs(60);
    // Recent activity (a queued request) keeps it open.
    let soon = Instant::now() + Duration::from_secs(61);
    assert!(
        m.h.expire_batches(&mut m.host, soon, idle, &|_| Some(soon))
            .is_empty()
    );
    assert_eq!(m.h.expire_batches(&mut m.host, soon, idle, &|_| None), [1]);
    assert_eq!(m.host.history.undo_entries().len(), 1);
    assert_eq!(m.host.history.undo_entries()[0].label, "Agent: slow");
    assert_eq!(
        err_code(m.call('a', "history.end_batch", json!({}))),
        code::BATCH_STATE
    );
}

/// Approval mode, handler side: an approval batch builds each request on
/// its scratch copy, `end_batch` waits like a write, and the user accepts
/// batches oldest first.
#[test]
fn approval_batches_build_on_their_scratch_and_apply_in_order() {
    let mut m = Multi::new();
    m.h.set_approval(true);
    m.call(
        'a',
        "history.begin_batch",
        json!({ "label": "Agent: hazards" }),
    )
    .unwrap();
    let r = m
        .call(
            'a',
            "defs.set",
            json!({ "kind": "layer", "def": { "slug": "hazards", "name": "Hazards", "kind": "int_grid",
                "values": [{ "slug": "spikes", "name": "Spikes", "color": "#c03030", "glyph": "^" }] } }),
        )
        .unwrap();
    assert_eq!(r["status"], "collected", "{r}");
    // The new layer exists only in the batch's scratch copy so far.
    assert!(m.host.project.layer("hazards").is_none());
    let r = m
        .call(
            'a',
            "grid.set_cells",
            json!({ "level": "crevasse", "layer": "hazards", "text": "^^", "origin": [2, 3] }),
        )
        .unwrap();
    assert_eq!(r["status"], "collected", "{r}");
    let r = m
        .call(
            'a',
            "grid.line",
            json!({ "level": "crevasse", "layer": "hazards", "from": [4, 3], "to": [6, 3], "value": "spikes" }),
        )
        .unwrap();
    assert_eq!(r["status"], "collected", "{r}");
    // end_batch submits and waits for the user, like a write.
    let first = m.call('a', "history.end_batch", json!({})).unwrap();
    let first = first["pending"].as_str().unwrap().to_string();
    let second = m.call('b', "grid.fill_rect", fill(1, 1, "solid")).unwrap();
    let second = second["pending"].as_str().unwrap().to_string();

    // Oldest first: the later batch is refused and stays pending.
    let e =
        m.h.apply_pending(&mut m.host, &second)
            .expect_err("a later batch first")
            .expect("refused, not missing");
    assert!(e.message.contains(&first), "{}", e.message);
    assert_eq!(m.h.pending().len(), 2);
    assert_eq!(m.h.rev(), 0);
    m.h.apply_pending(&mut m.host, &first).unwrap();
    m.h.apply_pending(&mut m.host, &second).unwrap();
    assert_eq!(m.h.rev(), 2);
    let def = m.host.project.layer("hazards").unwrap().clone();
    let rows = m
        .host
        .project
        .level("crevasse")
        .unwrap()
        .int_grid("hazards")
        .unwrap()
        .to_rows(&def)
        .unwrap();
    assert_eq!(rows[3], "..^^^^^.............");
    assert_eq!(cell_of(&m.host, 1, 1), Some("solid".into()));
}

fn cell_of(host: &HeadlessHost, x: u32, y: u32) -> Option<String> {
    let def = host.project.layer("collision").unwrap();
    host.project
        .level("crevasse")
        .unwrap()
        .int_grid("collision")
        .and_then(|g| g.value_at(def, x, y))
        .map(|v| v.to_string())
}

/// `grid.line` checks its ends against the layer before drawing anything:
/// a far-away end is `out_of_bounds` at once, not billions of cells.
#[test]
fn grid_line_checks_its_ends_first() {
    let (_t, mut s) = session();
    let t = std::time::Instant::now();
    for to in [
        [2_147_483_647u32, 0],
        [4_294_967_295, 4_294_967_295],
        [20, 1],
    ] {
        let e = s
            .call(
                "grid.line",
                json!({ "level": "crevasse", "layer": "collision", "from": [0, 1], "to": to, "value": "solid" }),
            )
            .expect_err("out of bounds");
        assert_eq!(e.code, code::COMMAND_ERROR);
        assert_eq!(e.data.as_ref().unwrap()["kind"], "out_of_bounds", "{e:?}");
    }
    assert!(t.elapsed() < Duration::from_secs(1));
    let r = s
        .call(
            "grid.line",
            json!({ "level": "crevasse", "layer": "collision", "from": [0, 1], "to": [19, 1], "value": "solid" }),
        )
        .unwrap();
    assert_eq!(r["changes"][0]["rect"], json!([1, 1, 18, 1]));
}

/// `history.cancel_batch` reverts what the batch applied and records
/// nothing; with no batch open it is `batch_state`.
#[test]
fn cancel_batch_reverts_and_records_nothing() {
    let mut m = Multi::new();
    m.call(
        'a',
        "history.begin_batch",
        json!({ "label": "Agent: oops" }),
    )
    .unwrap();
    m.call('a', "grid.fill_rect", fill(1, 1, "water")).unwrap();
    assert_eq!(cell_of(&m.host, 1, 1), Some("water".into()));
    let r = m.call('a', "history.cancel_batch", json!({})).unwrap();
    assert_eq!(r["rev"], 2, "the write and its revert");
    assert_eq!(cell_of(&m.host, 1, 1), None);
    assert!(m.host.history.undo_entries().is_empty());
    assert_eq!(
        err_code(m.call('a', "history.cancel_batch", json!({}))),
        code::BATCH_STATE
    );
    assert_eq!(
        err_code(m.call('a', "history.end_batch", json!({}))),
        code::BATCH_STATE
    );
}
