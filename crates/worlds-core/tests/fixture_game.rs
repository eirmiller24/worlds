//! The `game` fixture (`tests/fixtures/game/`, made by its `make.py`): it
//! loads, validates clean, and its files are exactly what a save writes,
//! so tests that save a copy and compare files see only their own edits.

use std::path::{Path, PathBuf};

fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/game/game.worlds")
}

#[test]
fn game_fixture_is_valid_and_canonical() {
    let p = worlds_core::load(dir()).unwrap_or_else(|e| panic!("{e}"));
    let problems = worlds_core::validate(&p);
    assert!(problems.is_empty(), "{problems:?}");
    for (rel, bytes) in worlds_core::format::render_files(&p).unwrap() {
        let on_disk = std::fs::read(dir().join(&rel)).unwrap_or_default();
        assert!(
            on_disk == bytes,
            "{} isn't canonical: run this file's `canonicalize` test",
            rel.display()
        );
    }
}

/// Rewrite the fixture in canonical form after `make.py`:
/// `cargo test -p worlds-core --test fixture_game -- --ignored canonicalize`.
#[test]
#[ignore]
fn canonicalize() {
    let mut p = worlds_core::load(dir()).unwrap_or_else(|e| panic!("{e}"));
    worlds_core::format::save_all(&mut p, dir()).unwrap();
}
