//! Editor mode: the engine finds a live server through `.worlds/server.json`
//! and works over its socket. `worlds_server::headless::run` stands in for
//! the editor (same server, same discovery file).

mod common;

use common::{call, engine, mini, ok};
use serde_json::json;
use worlds_mcp::backend::{Backend, EditorBackend};
use worlds_server::ServerInfo;
use worlds_server::headless;

#[test]
fn tools_go_over_the_socket_as_one_entry() {
    let (_t, dir) = mini();
    let server = headless::run(&dir, headless::Options::default()).unwrap();
    let info = ServerInfo::read(&dir).unwrap().unwrap();
    let mut e = engine(&dir, false);

    let text = call(&mut e, "read_level", json!({ "level": "crevasse" })).1;
    assert!(
        text.contains("int_grid collision (20x5 cells of 16 px)"),
        "{text}"
    );
    let r = ok(
        &mut e,
        "paint",
        json!({ "level": "crevasse", "layer": "collision", "rect": [2, 1, 2, 1], "value": "solid",
            "line": { "from": [5, 1], "to": [7, 1] }, "label": "ledges" }),
    );
    assert_eq!(r["mode"], "editor");
    assert_eq!(r["saved"], false, "the editor saves when the user saves");
    assert_eq!(r["rev"], 2, "two API writes in one batch");
    assert_eq!(r["undo_label"], "Agent: paint ledges");
    assert!(r.get("notices").is_none(), "{r}");
    let text = call(&mut e, "read_level", json!({ "level": "crevasse" })).1;
    assert!(text.contains("1 #.##.###...........#"), "{text}");

    // Another client sees the server's rev moved, and the call is one
    // history entry with the agent's origin and label.
    let mut other = EditorBackend::connect(&info, &dir, "observer").unwrap();
    let rev = other.call("level.list", json!({})).unwrap()["rev"].clone();
    assert_eq!(rev, 2);
    let u = other
        .call("history.undo", json!({ "n": 5, "any": true }))
        .unwrap();
    assert_eq!(
        u["steps"],
        json!([{ "label": "Agent: paint ledges", "origin": { "agent": "claude-code" } }])
    );
    other.call("history.redo", json!({ "any": true })).unwrap();

    // Someone else edits the level the agent read: its next write says so.
    other
        .call(
            "grid.set_cells",
            json!({ "level": "crevasse", "layer": "collision", "cells": [[10, 1, "water"]] }),
        )
        .unwrap();
    let r = ok(
        &mut e,
        "paint",
        json!({ "level": "crevasse", "layer": "collision", "cells": [[12, 1, "solid"]] }),
    );
    let notices = r["notices"].as_array().unwrap();
    assert!(
        notices.iter().any(|n| n
            .as_str()
            .unwrap()
            .starts_with("observer changed crevasse (collision [10, 1, 1, 1])")),
        "{r}"
    );
    assert!(
        notices.iter().any(|n| n
            .as_str()
            .unwrap()
            .contains("undid \"Agent: paint ledges\"")),
        "{r}"
    );
    // Nothing was saved: editor mode leaves that to the user.
    assert!(common::rows_on_disk(&dir, "crevasse", "collision")[1].starts_with("#......"));

    // Undo goes through the editor too.
    let u = ok(&mut e, "undo", json!({}));
    assert_eq!(u["undone"], json!(["Agent: paint collision in crevasse"]));

    // Headless screenshot isn't used: view.snapshot is asked for (and the
    // stand-in server has no renderer, so it says so).
    let (is_err, m) = call(&mut e, "screenshot", json!({ "level": "crevasse" }));
    assert!(is_err && m.contains("view.snapshot"), "{m}");

    // The editor goes away: the next call works on the files.
    drop(other);
    server.stop();
    let d = ok(&mut e, "describe_project", json!({}));
    assert_eq!(d["mode"]["headless"], true);
    assert!(
        d["notices"][0]
            .as_str()
            .unwrap()
            .contains("closed the project"),
        "{d}"
    );
}

#[test]
fn stale_server_json_falls_back_to_headless() {
    let (_t, dir) = mini();
    // A pid that no longer runs.
    let mut child = std::process::Command::new("true").spawn().unwrap();
    let dead = child.id();
    child.wait().unwrap();
    std::fs::create_dir_all(dir.join(".worlds")).unwrap();
    let stale = ServerInfo {
        port: 9,
        token: "0".repeat(64),
        pid: dead,
        project: dir.to_string_lossy().into_owned(),
    };
    std::fs::write(
        ServerInfo::path(&dir),
        serde_json::to_string(&stale).unwrap(),
    )
    .unwrap();
    let mut e = engine(&dir, false);
    let r = ok(
        &mut e,
        "paint",
        json!({ "level": "crevasse", "layer": "collision", "cells": [[1, 1, "water"]] }),
    );
    assert_eq!(r["mode"], "headless");
    assert_eq!(r["saved"], true);
    assert!(common::rows_on_disk(&dir, "crevasse", "collision")[1].starts_with("#~"));
    // The file is left alone.
    assert!(ServerInfo::path(&dir).exists());
}

#[test]
fn a_live_server_for_another_directory_is_ignored_and_headless_refuses_a_live_editor() {
    let (_t, dir) = mini();
    let (_t2, other) = mini();
    let server = headless::run(&other, headless::Options::default()).unwrap();
    // Copy the other project's live server.json here: project doesn't match.
    std::fs::create_dir_all(dir.join(".worlds")).unwrap();
    std::fs::copy(ServerInfo::path(&other), ServerInfo::path(&dir)).unwrap();
    let mut e = engine(&dir, false);
    assert_eq!(
        ok(&mut e, "describe_project", json!({}))["mode"]["headless"],
        true
    );

    // --headless with the editor open on this project: every call refuses.
    let mut e = engine(&other, true);
    let (is_err, m) = call(&mut e, "describe_project", json!({}));
    assert!(is_err && m.contains("--headless"), "{m}");
    server.stop();
}

/// A stand-in editor with approval mode on: a real server drained on a
/// thread of its own, which accepts every pending batch (the user clicking
/// Apply at once) when `accept` is set.
struct ApprovingEditor {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    /// Pending batches seen, and how many were open at the end.
    seen: std::sync::Arc<std::sync::Mutex<(Vec<String>, usize)>>,
}

impl ApprovingEditor {
    fn start(dir: &std::path::Path, accept: bool) -> Self {
        use std::sync::atomic::Ordering;
        let mut host = headless::HeadlessHost::open(dir).unwrap();
        let mut server = worlds_server::Server::start(worlds_server::ServerConfig {
            project_dir: dir.to_path_buf(),
            project_name: host.project.name.clone(),
            headless: false,
            wake: None,
        })
        .unwrap();
        server.set_approval(true);
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let seen = std::sync::Arc::new(std::sync::Mutex::new((Vec::new(), 0)));
        let (stop2, seen2) = (stop.clone(), seen.clone());
        let thread = std::thread::spawn(move || {
            while !stop2.load(Ordering::SeqCst) {
                server.drain(&mut host);
                let ids: Vec<String> = server.pending().iter().map(|p| p.id.clone()).collect();
                for id in ids {
                    seen2.lock().unwrap().0.push(id.clone());
                    if accept {
                        server.apply_pending(&mut host, &id).unwrap();
                    }
                }
                seen2.lock().unwrap().1 = server.open_batches() + server.pending().len();
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            server.stop();
        });
        ApprovingEditor {
            stop,
            thread: Some(thread),
            seen,
        }
    }
}

impl Drop for ApprovingEditor {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Approval mode: a multi-write call waits for the user like a single
/// write (applied when accepted in time), and a call that fails part-way
/// submits nothing.
#[test]
fn approval_mode_multi_write_calls() {
    let (_t, dir) = mini();
    let ed = ApprovingEditor::start(&dir, true);
    let mut e = engine(&dir, false);
    // rect and line: two API writes, one batch, accepted at once.
    let r = ok(
        &mut e,
        "paint",
        json!({ "level": "crevasse", "layer": "collision", "rect": [2, 1, 2, 1], "value": "solid",
            "line": { "from": [5, 1], "to": [7, 1] }, "label": "ledges" }),
    );
    assert_eq!(r["applied"], true, "{r}");
    assert_eq!(r["undo_label"], "Agent: paint ledges");
    assert_eq!(ed.seen.lock().unwrap().0.len(), 1);
    // create_level waits for approval like every other write.
    let r = ok(
        &mut e,
        "create_level",
        json!({ "slug": "annex", "name": "Annex", "size": [6, 3], "world_px": [416, 0] }),
    );
    assert_eq!(r["applied"], true, "{r}");
    assert_eq!(r["mode"], "editor");
    assert_eq!(r["undo_label"], "Agent: create_level annex");
    assert_eq!(ed.seen.lock().unwrap().0.len(), 2);
    drop(ed);

    let ed = ApprovingEditor::start(&dir, false);
    let mut e = engine(&dir, false);
    // The rect is fine, the line leaves the level: the call fails and
    // nothing waits for approval.
    let m = common::err(
        &mut e,
        "paint",
        json!({ "level": "crevasse", "layer": "collision", "rect": [2, 2, 2, 1], "value": "solid",
            "line": { "from": [5, 1], "to": [70, 1] } }),
    );
    assert!(m.contains("outside") || m.contains("out of"), "{m}");
    std::thread::sleep(std::time::Duration::from_millis(50));
    let seen = ed.seen.lock().unwrap().clone();
    assert!(seen.0.is_empty(), "submitted {:?}", seen.0);
    assert_eq!(seen.1, 0, "a batch is still open or pending");
}

/// The editor is live but the connection fails (here: a token that doesn't
/// match): the call fails and nothing is written to the files, which the
/// editor owns.
#[test]
fn a_live_editor_that_refuses_the_connection_is_an_error_not_a_fallback() {
    let (_t, dir) = mini();
    let server = headless::run(&dir, headless::Options::default()).unwrap();
    let mut info = ServerInfo::read(&dir).unwrap().unwrap();
    info.token = "0".repeat(64);
    std::fs::write(
        ServerInfo::path(&dir),
        serde_json::to_string(&info).unwrap(),
    )
    .unwrap();
    let before = common::rows_on_disk(&dir, "crevasse", "collision");
    let mut e = engine(&dir, false);
    let m = common::err(
        &mut e,
        "paint",
        json!({ "level": "crevasse", "layer": "collision", "cells": [[1, 1, "water"]] }),
    );
    assert!(m.contains("couldn't connect"), "{m}");
    assert_eq!(common::rows_on_disk(&dir, "crevasse", "collision"), before);
    server.stop();
}
