//! A non-rectangular level through the real exporter: `worlds-cli bake`
//! writes `void`, `outline` and the seams along it, and `check-export`
//! (its own reading of the files) accepts them, fresh included.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};
use worlds_core::command::{Command as Cmd, Rect};

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_worlds-cli"))
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let dst = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &dst);
        } else {
            std::fs::copy(e.path(), dst).unwrap();
        }
    }
}

/// The `game` fixture copied, with `level_1` (19x13 at [2048, -1280]) made
/// an L: its top-right 6x5 cells cut out. `level_2` touches its right edge
/// below the notch.
fn shaped_game() -> (tempfile::TempDir, PathBuf) {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/game");
    let tmp = tempfile::tempdir().unwrap();
    copy_dir(&src, &tmp.path().join("game"));
    let dir = tmp.path().join("game/game.worlds");
    let mut p = worlds_core::load(&dir).unwrap();
    p.apply(Cmd::SetLevelVoid {
        level: worlds_core::slug("level_1"),
        void: vec![Rect::new(13, 0, 6, 5)],
        restore: false,
    })
    .unwrap();
    worlds_core::save_dirty(&mut p, &dir).unwrap();
    (tmp, dir)
}

#[test]
fn a_baked_shaped_level_passes_check_export() {
    let (_t, dir) = shaped_game();
    let out = cli().arg("bake").arg(&dir).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let export = dir.join("export");
    let world: Value =
        serde_json::from_str(&std::fs::read_to_string(export.join("world.json")).unwrap()).unwrap();
    let levels = world["worlds"][0]["levels"].as_array().unwrap();
    let l1 = levels.iter().find(|l| l["slug"] == "level_1").unwrap();
    assert_eq!(l1["void"], json!([[13, 0, 6, 5]]));
    assert_eq!(
        l1["outline"],
        json!([
            [2048, -1280],
            [2464, -1280],
            [2464, -1120],
            [2656, -1120],
            [2656, -864],
            [2048, -864]
        ])
    );
    let to_l2: Vec<&Value> = l1["touching"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["level"] == "level_2")
        .collect();
    assert_eq!(
        to_l2,
        [&json!({ "level": "level_2", "edge": "right", "span": [-1120, -864], "at": 2656 })]
    );
    let level: Value =
        serde_json::from_str(&std::fs::read_to_string(export.join("levels/level_1.json")).unwrap())
            .unwrap();
    assert_eq!(level["outline"], l1["outline"]);
    // Untouched rectangles export no shape keys.
    let l3 = levels.iter().find(|l| l["slug"] == "level_3").unwrap();
    assert!(l3.get("void").is_none() && l3.get("outline").is_none());

    for args in [
        vec![export.as_os_str()],
        vec!["--project".as_ref(), dir.as_os_str(), "--fresh".as_ref()],
    ] {
        let out = cli().arg("check-export").args(&args).output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{text}{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(text.contains("no problems"), "{text}");
    }
}
