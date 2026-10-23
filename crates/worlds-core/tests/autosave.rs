//! Autosave and recovery against saving (`docs/format.md`
//! "Autosave"): `render_dirty` is exactly what `save_dirty` writes and
//! deletes, for every shape of dirty set; and an autosave written from it,
//! loaded on a pristine copy with `load_with_overlay` and saved, gives the
//! same tree byte for byte as saving in the first place. Fixed shapes, then
//! random command sequences (proptest).

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use proptest::prelude::*;
use serde_json::{Value, json};
use worlds_core::autosave::{self, Inspect};
use worlds_core::format::{load_with_overlay, render_dirty};
use worlds_core::{Command, Project, load, save_dirty};

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let p = e.unwrap().path();
        let t = to.join(p.file_name().unwrap());
        if p.is_dir() {
            copy_dir(&p, &t);
        } else {
            std::fs::copy(&p, &t).unwrap();
        }
    }
}

/// Every file (bytes) and directory under `dir`, except `.worlds/`, by
/// `/`-separated relative path on every platform.
fn tree(dir: &Path) -> BTreeMap<String, Option<Vec<u8>>> {
    fn walk(root: &Path, d: &Path, out: &mut BTreeMap<String, Option<Vec<u8>>>) {
        for e in std::fs::read_dir(d).unwrap() {
            let p = e.unwrap().path();
            let rel = autosave::slash(p.strip_prefix(root).unwrap());
            if rel == ".worlds" {
                continue;
            }
            if p.is_dir() {
                out.insert(format!("{rel}/"), None);
                walk(root, &p, out);
            } else {
                out.insert(rel, Some(std::fs::read(&p).unwrap()));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

fn cmd(v: Value) -> Command {
    serde_json::from_value(v.clone()).unwrap_or_else(|e| panic!("{v}: {e}"))
}

/// Apply `first`, save, apply `then` (commands that fail are skipped, as a
/// user's refused edit would be), then check:
///
/// 1. `render_dirty` names exactly the files `save_dirty` then writes (same
///    bytes) and deletes;
/// 2. an autosave of that state, recovered onto a copy of the tree as it
///    was before the second batch and saved, gives the same tree and the
///    same project.
///
/// Returns how many of the commands applied.
fn roundtrip(first: &[Command], then: &[Command]) -> Result<usize, TestCaseError> {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a.worlds");
    let b = tmp.path().join("b.worlds");
    copy_dir(&common::fixture_dir(), &a);
    let mut p = load(&a).unwrap();
    let mut applied = 0;
    for c in first {
        applied += usize::from(p.apply(c.clone()).is_ok());
    }
    save_dirty(&mut p, &a).unwrap();
    copy_dir(&a, &b);
    for c in then {
        applied += usize::from(p.apply(c.clone()).is_ok());
    }
    let render = render_dirty(&p).unwrap();
    let before = tree(&a);
    let g = autosave::generation(&p, render.clone(), 7, 1, Some("test".into()));
    save_dirty(&mut p, &a).unwrap();
    let after = tree(&a);

    // 1. render_dirty is the save.
    for (rel, bytes) in &render.files {
        let rel = autosave::slash(rel);
        prop_assert_eq!(after.get(&rel), Some(&Some(bytes.clone())), "{}", rel);
    }
    for (rel, bytes) in &after {
        if bytes.is_some() && before.get(rel) != Some(bytes) {
            prop_assert!(
                render.files.contains_key(Path::new(rel)),
                "{} changed but render_dirty didn't render it",
                rel
            );
        }
    }
    let removed: Vec<PathBuf> = before
        .iter()
        .filter(|(rel, b)| b.is_some() && !after.contains_key(*rel))
        .map(|(rel, _)| PathBuf::from(rel))
        .collect();
    let deleted: Vec<PathBuf> = render.deleted.iter().cloned().collect();
    prop_assert_eq!(removed, deleted);

    // 2. Recovery onto the tree before the second batch.
    autosave::write(&b, &g).unwrap();
    let found = match autosave::inspect(&b) {
        Inspect::Found(f) => f,
        other => return Err(TestCaseError::fail(format!("{other:?}"))),
    };
    prop_assert!(!found.has_conflicts());
    let plan = found.plan(false);
    let mut q: Project = load_with_overlay(&b, &plan.overlay, plan.dirty.clone()).unwrap();
    prop_assert_eq!(&q, &p);
    save_dirty(&mut q, &b).unwrap();
    prop_assert!(
        tree(&b) == after,
        "recovered tree differs from the saved one"
    );
    Ok(applied)
}

fn paint(x: i64, y: i64, v: Option<&str>) -> Command {
    cmd(
        json!({ "op": "paint_cells", "level": "crevasse", "layer": "collision", "cells": [[x, y, v]] }),
    )
}

fn annex() -> Command {
    cmd(json!({ "op": "create_level", "world": "main",
        "level": { "id": "annexlvl22", "slug": "annex", "name": "Annex", "world_px": [640, 0], "size": [6, 3], "depth": 0 },
        "content": { "int_grids": [ { "layer": "collision", "rows": ["######", "#....#", "######"] } ] } }))
}

#[test]
fn one_file() {
    roundtrip(&[], &[paint(3, 1, Some("solid"))]).unwrap();
}

#[test]
fn a_new_level_directory() {
    roundtrip(&[], &[annex()]).unwrap();
}

#[test]
fn a_deleted_level() {
    let n = roundtrip(
        &[],
        &[cmd(
            json!({ "op": "delete_level", "world": "main", "level": "shaft" }),
        )],
    )
    .unwrap();
    assert_eq!(n, 1);
}

#[test]
fn a_renamed_level() {
    let n = roundtrip(
        &[],
        &[cmd(
            json!({ "op": "rename_level", "world": "main", "level": "shaft", "slug": "wing", "name": "Wing" }),
        )],
    )
    .unwrap();
    assert_eq!(n, 1);
}

#[test]
fn a_deleted_entity_type() {
    let n = roundtrip(
        &[],
        &[
            cmd(json!({ "op": "remove_entity", "id": "btncrev222" })),
            cmd(json!({ "op": "remove_entity_def", "entity": "floor_button" })),
        ],
    )
    .unwrap();
    assert_eq!(n, 2);
}

#[test]
fn a_deleted_rules_file_and_layer() {
    let n = roundtrip(
        &[],
        &[cmd(
            json!({ "op": "remove_layer_def", "layer": "main_tiling" }),
        )],
    )
    .unwrap();
    assert_eq!(n, 1);
}

#[test]
fn a_cleared_layer_and_entities_file() {
    let n = roundtrip(
        &[],
        &[
            cmd(json!({ "op": "clear_tiles", "level": "crevasse", "layer": "decor_back", "rect": [0, 0, 40, 10] })),
            cmd(json!({ "op": "remove_entity", "id": "btncrev222" })),
            cmd(json!({ "op": "remove_entity", "id": "doorcrev22" })),
        ],
    )
    .unwrap();
    assert_eq!(n, 3);
}

#[test]
fn edits_after_a_save() {
    // Bases after a save are the saved bytes: no conflict, same tree.
    roundtrip(
        &[annex(), paint(1, 1, Some("water"))],
        &[
            cmd(json!({ "op": "delete_level", "world": "main", "level": "annex" })),
            paint(2, 1, Some("solid")),
        ],
    )
    .unwrap();
}

/// The commands the random sequences draw from: grid, tiles, levels
/// (create, delete, rename, back), entities and entity types, notes, a
/// layer and its rules file.
fn pool() -> Vec<Command> {
    let mut v = vec![
        paint(3, 1, Some("solid")),
        paint(4, 2, Some("water")),
        paint(3, 1, None),
        cmd(
            json!({ "op": "fill_rect", "level": "crevasse", "layer": "collision", "rect": [1, 1, 4, 2], "value": "fortified" }),
        ),
        cmd(
            json!({ "op": "set_tiles", "level": "crevasse", "layer": "decor_back",
            "tiles": [ { "cell": [4, 2], "tileset": "castle_main", "tile": [1, 1] } ] }),
        ),
        cmd(
            json!({ "op": "clear_tiles", "level": "crevasse", "layer": "decor_back", "rect": [0, 0, 40, 10] }),
        ),
        annex(),
        cmd(json!({ "op": "delete_level", "world": "main", "level": "annex" })),
        cmd(json!({ "op": "delete_level", "world": "main", "level": "shaft" })),
        cmd(
            json!({ "op": "rename_level", "world": "main", "level": "shaft", "slug": "wing", "name": "Wing" }),
        ),
        cmd(
            json!({ "op": "rename_level", "world": "main", "level": "wing", "slug": "shaft", "name": "Shaft" }),
        ),
        cmd(json!({ "op": "remove_entity", "id": "btncrev222" })),
        cmd(
            json!({ "op": "add_entity", "level": "shaft", "layer": "objects",
            "entity": { "id": "shaftbtn22", "type": "floor_button", "cell": [1, 1] } }),
        ),
        cmd(json!({ "op": "remove_entity_def", "entity": "floor_button" })),
        cmd(json!({ "op": "rename_entity_type", "from": "locked_door", "to": "door" })),
        cmd(json!({ "op": "remove_note", "id": "notecrev22" })),
        cmd(json!({ "op": "remove_layer_def", "layer": "main_tiling" })),
    ];
    v.push(cmd(
        json!({ "op": "add_entity", "level": "crevasse", "layer": "objects",
        "entity": { "id": "crevbtn222", "type": "floor_button", "cell": [2, 2] } }),
    ));
    v
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

    #[test]
    fn random_sequences_recover_to_the_saved_tree(
        first in proptest::collection::vec(0usize..18, 0..4),
        then in proptest::collection::vec(0usize..18, 1..8),
    ) {
        let pool = pool();
        let pick = |ix: &[usize]| ix.iter().map(|&i| pool[i].clone()).collect::<Vec<_>>();
        roundtrip(&pick(&first), &pick(&then))?;
    }
}

#[test]
fn the_pool_mostly_applies() {
    // Guard against a pool of commands that all fail (the property would
    // hold trivially): in this order nearly all apply.
    let n = roundtrip(&[], &pool()).unwrap();
    assert!(n >= 14, "only {n} of {} applied", pool().len());
}
