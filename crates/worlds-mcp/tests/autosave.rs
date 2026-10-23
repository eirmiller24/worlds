//! Headless worlds-mcp and an editor session's autosave. A crashed
//! session's (lock free, manifest present) is reported in describe_project's
//! `autosave` and as a notice; a live editor's (lock held) is not.

mod common;

use std::collections::BTreeMap;
use std::path::PathBuf;

use common::{engine, mini, ok};
use serde_json::json;
use worlds_core::autosave::{self, EditorLock, FileEntry, FileState, Generation, Manifest};

fn leave_autosave(dir: &std::path::Path) {
    let bytes = b"autosaved\n".to_vec();
    let g = Generation {
        manifest: Manifest {
            format: autosave::FORMAT,
            generation: 1,
            written: autosave::now_rfc3339(),
            pid: 999_999,
            rev: 4,
            last_label: Some("Paint collision".into()),
            dirty: vec!["worlds/main/levels/crevasse/collision.json".into()],
            files: BTreeMap::from([(
                "worlds/main/levels/crevasse/collision.json".to_string(),
                FileEntry {
                    state: FileState::Modified,
                    base: None,
                    hash: Some(autosave::hex(autosave::hash(&bytes))),
                },
            )]),
        },
        files: BTreeMap::from([(
            PathBuf::from("worlds/main/levels/crevasse/collision.json"),
            bytes,
        )]),
    };
    autosave::write(dir, &g).unwrap();
}

#[test]
fn describe_project_reports_a_crashed_sessions_autosave() {
    let (_t, dir) = mini();
    leave_autosave(&dir);
    let mut e = engine(&dir, true);
    let d = ok(&mut e, "describe_project", json!({}));
    let a = &d["autosave"];
    assert_eq!(a["unrecovered"], true, "{d}");
    assert_eq!(a["pid"], 999_999);
    assert_eq!(a["files"], 1);
    assert!(
        a["message"]
            .as_str()
            .unwrap()
            .contains("open the project in the Worlds editor"),
        "{a}"
    );
    let notices = d["notices"].to_string();
    assert!(notices.contains("unrecovered autosave"), "{d}");
    // Headless edits leave the autosave alone (the editor sees them as
    // conflicts when it recovers).
    ok(
        &mut e,
        "paint",
        json!({ "level": "crevasse", "layer": "collision", "cells": [[1, 1, "solid"]] }),
    );
    assert!(autosave::autosave_dir(&dir).join("manifest.json").exists());
}

#[test]
fn a_live_editors_autosave_is_not_mentioned() {
    let (_t, dir) = mini();
    leave_autosave(&dir);
    // A live editor holds the session lock (here: this process).
    let lock = EditorLock::acquire(&dir, "test").unwrap();
    let mut e = engine(&dir, true);
    let d = ok(&mut e, "describe_project", json!({}));
    assert!(d.get("autosave").is_none(), "{d}");
    assert!(!d.to_string().contains("unrecovered"), "{d}");
    drop(lock);
}

#[test]
fn no_autosave_no_field() {
    let (_t, dir) = mini();
    let mut e = engine(&dir, true);
    let d = ok(&mut e, "describe_project", json!({}));
    assert!(d.get("autosave").is_none(), "{d}");
}

/// `describe_project` looks for an unrecovered autosave each time, not
/// only when the headless backend opened.
#[test]
fn describe_project_sees_an_autosave_left_after_opening() {
    let (_t, dir) = mini();
    let mut e = engine(&dir, true);
    let d = ok(&mut e, "describe_project", json!({}));
    assert!(d.get("autosave").is_none(), "{d}");
    leave_autosave(&dir);
    let d = ok(&mut e, "describe_project", json!({}));
    assert_eq!(d["autosave"]["unrecovered"], true, "{d}");
    autosave::clear(&dir).unwrap();
    let d = ok(&mut e, "describe_project", json!({}));
    assert!(d.get("autosave").is_none(), "{d}");
}
