//! `worlds-cli check-export`: the export fixtures, one test per problem
//! kind, and `--fresh` against an in-memory bake.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::{Value, json};

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_worlds-cli"))
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/exports")
}

fn check(args: &[&std::ffi::OsStr]) -> (Option<i32>, String) {
    let out: Output = cli().arg("check-export").args(args).output().unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.code(), text)
}

/// `check-export <dir> --json`, plus `extra`.
fn check_json(dir: &Path, extra: &[&str]) -> (Option<i32>, Value) {
    let out = cli()
        .arg("check-export")
        .arg(dir)
        .arg("--json")
        .args(extra)
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "{e}: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    });
    (out.status.code(), v)
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

/// A copy of fixture `case` in a temp dir.
fn case(name: &str) -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join(name);
    copy_dir(&fixtures().join(name), &dir);
    (tmp, dir)
}

fn edit(path: &Path, f: impl FnOnce(&mut Value)) {
    let mut v: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    f(&mut v);
    std::fs::write(path, serde_json::to_string_pretty(&v).unwrap()).unwrap();
}

/// The one problem of an edited clean case: its kind and message.
fn one_problem(dir: &Path) -> (String, String, String) {
    let (code, v) = check_json(dir, &[]);
    assert_eq!(code, Some(1), "{v:#}");
    let p = v["problems"].as_array().unwrap();
    assert_eq!(p.len(), 1, "{v:#}");
    (
        p[0]["kind"].as_str().unwrap().to_string(),
        format!(
            "{}: {}",
            p[0]["file"].as_str().unwrap(),
            p[0]["path"].as_str().unwrap()
        ),
        p[0]["message"].as_str().unwrap().to_string(),
    )
}

#[test]
fn fixture_cases() {
    let mut cases: Vec<PathBuf> = std::fs::read_dir(fixtures())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.join("expect.json").is_file())
        .collect();
    cases.sort();
    assert!(cases.len() >= 11, "{cases:?}");
    let readme = std::fs::read_to_string(fixtures().join("README.md")).unwrap();
    for dir in cases {
        let name = dir.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            readme.contains(&format!("| `{name}` |")),
            "{name} has no row in the fixtures README"
        );
        let expect: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("expect.json")).unwrap())
                .unwrap();
        let (code, v) = check_json(&dir, &[]);
        let accept = expect["accept"].as_bool().unwrap();
        assert_eq!(code, Some(if accept { 0 } else { 1 }), "{name}: {v:#}");
        assert_eq!(v["ok"], json!(accept), "{name}");
        let problems = v["problems"].as_array().unwrap();
        if accept {
            assert!(problems.is_empty(), "{name}: {v:#}");
        } else {
            let want = expect["error_contains"].as_str().unwrap();
            assert!(!want.is_empty(), "{name}: a refused case names its error");
            assert!(
                problems
                    .iter()
                    .any(|p| p["message"].as_str().unwrap().contains(want)),
                "{name}: no problem contains {want:?}: {v:#}"
            );
        }
        let warnings = v["warnings"].as_array().unwrap();
        match expect["warning_contains"].as_str() {
            Some(w) => assert!(
                warnings.iter().any(
                    |x| x["message"].as_str().unwrap().contains(w) && x["kind"] == "annotation"
                ),
                "{name}: no warning contains {w:?}: {v:#}"
            ),
            None => assert!(warnings.is_empty(), "{name}: {v:#}"),
        }
    }
}

#[test]
fn text_output_and_exit_codes() {
    let (code, text) = check(&[fixtures().join("clean_minimal").as_os_str()]);
    assert_eq!(code, Some(0), "{text}");
    assert_eq!(text, "ok: 1 levels, no problems\n");
    let (code, text) = check(&[fixtures().join("flip_out_of_range").as_os_str()]);
    assert_eq!(code, Some(1));
    assert!(
        text.starts_with(
            "levels/room.json: tile_layers[back].tilesets[0].tiles[0]: flip 8 is outside 0..7"
        ),
        "{text}"
    );
    assert!(text.ends_with("1 problem(s)\n"), "{text}");
    let (code, text) = check(&[fixtures().join("dangling_note_link").as_os_str()]);
    assert_eq!(code, Some(0));
    assert!(
        text.contains("warning: world.json: worlds[main].notes[pathwalkaa].links[0]:"),
        "{text}"
    );
    assert!(
        text.ends_with("ok: 1 levels, no problems, 1 warning(s)\n"),
        "{text}"
    );
    // Usage and I/O errors: 2.
    let tmp = tempfile::tempdir().unwrap();
    let (code, text) = check(&[tmp.path().join("nope").as_os_str()]);
    assert_eq!(code, Some(2), "{text}");
    assert_eq!(check(&[]).0, Some(2));
    let (code, _) = check(&["x".as_ref(), "--bogus".as_ref()]);
    assert_eq!(code, Some(2));
    let (code, text) = check(&[
        fixtures().join("clean_minimal").as_os_str(),
        "--project".as_ref(),
        "p".as_ref(),
    ]);
    assert_eq!(code, Some(2), "{text}");
}

#[test]
fn format_problems() {
    type Edit = Box<dyn Fn(&Path)>;
    let level = |f: fn(&mut Value)| -> Edit {
        Box::new(move |d: &Path| edit(&d.join("levels/room.json"), f))
    };
    let world =
        |f: fn(&mut Value)| -> Edit { Box::new(move |d: &Path| edit(&d.join("world.json"), f)) };
    let cases: Vec<(&str, Edit, &str, &str)> = vec![
        (
            "format",
            world(|w| w["format"] = json!(2)),
            "world.json: format",
            "format 2; this checker reads format 1",
        ),
        (
            "missing world.json",
            Box::new(|d: &Path| std::fs::remove_file(d.join("world.json")).unwrap()),
            "world.json: ",
            "missing",
        ),
        (
            "not JSON",
            Box::new(|d: &Path| std::fs::write(d.join("enums.json"), "{").unwrap()),
            "enums.json: ",
            "not JSON",
        ),
        (
            "missing key",
            level(|l| {
                l.as_object_mut().unwrap().remove("tile_layers");
            }),
            "levels/room.json: ",
            "missing key \"tile_layers\"",
        ),
        (
            "slug mismatch",
            level(|l| l["slug"] = json!("hall")),
            "levels/room.json: slug",
            "\"hall\", but world.json says \"room\"",
        ),
        (
            "rect mismatch",
            world(|w| w["worlds"][0]["levels"][0]["rect"] = json!([0, 0, 320, 160])),
            "levels/room.json: ",
            "don't match world.json's rect [0, 0, 320, 160]",
        ),
        (
            "rows count",
            level(|l| {
                l["int_grids"]["collision"]["rows"]
                    .as_array_mut()
                    .unwrap()
                    .pop();
            }),
            "levels/room.json: int_grids.collision.rows",
            "5 rows, the level has 6",
        ),
        (
            "glyph",
            level(|l| l["int_grids"]["collision"]["rows"][1] = json!("#..?.....#")),
            "levels/room.json: int_grids.collision.rows[1]",
            "glyph '?' isn't in the legend",
        ),
        (
            "tile outside the level",
            level(|l| l["tile_layers"][0]["tilesets"][0]["tiles"][0] = json!([320, 64, 1, 1, 0])),
            "levels/room.json: tile_layers[back].tilesets[0].tiles[0]",
            "outside the level's 320x192 px",
        ),
        (
            "open edges",
            level(|l| l["int_grids"]["collision"]["rows"][2] = json!("..........")),
            "world.json: worlds[main].levels[room].open_edges",
            "doesn't match the collision rows of room",
        ),
        (
            "touching",
            world(|w| {
                w["worlds"][0]["levels"][0]["touching"] =
                    json!([{ "level": "x", "edge": "left", "span": [0, 32] }])
            }),
            "world.json: worlds[main].levels[room].touching",
            "doesn't match the level rects",
        ),
        (
            "stale level file",
            Box::new(|d: &Path| {
                std::fs::copy(d.join("levels/room.json"), d.join("levels/old.json")).unwrap();
            }),
            "levels/old.json: ",
            "not listed in world.json (a stale level file)",
        ),
        (
            "annotation shape",
            world(|w| {
                w["worlds"][0]["notes"] =
                    json!([{ "id": "pinaaaaaaa", "kind": "pin", "points": [[0, 0]] }])
            }),
            "world.json: worlds[main].notes[pinaaaaaaa]",
            "not an annotation",
        ),
    ];
    for (what, f, at, msg) in cases {
        let (_t, dir) = case("clean_minimal");
        f(&dir);
        let (kind, path, message) = one_problem(&dir);
        assert_eq!(kind, "format", "{what}: {message}");
        assert_eq!(path, at, "{what}: {message}");
        assert!(message.contains(msg), "{what}: {message}");
    }
}

#[test]
fn reference_problems() {
    type Case = (&'static str, fn(&Path), &'static str);
    let cases: Vec<Case> = vec![
        (
            "missing image",
            |d| std::fs::remove_file(d.join("tiles.png")).unwrap(),
            "image \"tiles.png\" doesn't exist",
        ),
        (
            "atlas tile outside the image",
            |d| {
                edit(&d.join("levels/room.json"), |l| {
                    l["tile_layers"][0]["tilesets"][0]["tiles"][0] = json!([64, 64, 4, 0, 0])
                })
            },
            "atlas tile [4, 0] lies outside the 128x64 px image",
        ),
        (
            "unknown entity type",
            |d| {
                edit(&d.join("levels/room.json"), |l| {
                    l["entities"][0]["type"] = json!("ghost")
                })
            },
            "\"ghost\" isn't in world.json's entity_types",
        ),
        (
            "dangling entity reference",
            |d| {
                edit(&d.join("levels/room.json"), |l| {
                    l["entities"][0]["fields"]["target"] =
                        json!({ "level": null, "id": "zzzzzzzzzz" })
                })
            },
            "entity reference with \"level\": null",
        ),
    ];
    for (what, f, msg) in cases {
        let (_t, dir) = case("clean_minimal");
        f(&dir);
        let (code, v) = check_json(&dir, &[]);
        assert_eq!(code, Some(1), "{what}: {v:#}");
        // A missing image is reported for each layer that uses it.
        for p in v["problems"].as_array().unwrap() {
            assert_eq!(p["kind"], "reference", "{what}: {v:#}");
            assert!(
                p["message"].as_str().unwrap().contains(msg),
                "{what}: {v:#}"
            );
        }
    }
}

/// `open_edges` missing is reported even when the rows don't fit the
/// level; a derived collision layer is reported in each level file that
/// has it, not as one world.json path repeated.
#[test]
fn open_edges_and_derived_collision() {
    let (_t, dir) = case("clean_minimal");
    edit(&dir.join("world.json"), |w| {
        w["worlds"][0]["levels"][0]
            .as_object_mut()
            .unwrap()
            .remove("open_edges");
    });
    edit(&dir.join("levels/room.json"), |l| {
        l["int_grids"]["collision"]["rows"]
            .as_array_mut()
            .unwrap()
            .pop();
    });
    let (code, v) = check_json(&dir, &[]);
    assert_eq!(code, Some(1), "{v:#}");
    let msgs: Vec<String> = v["problems"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| format!("{}: {}: {}", p["file"], p["path"], p["message"]))
        .collect();
    assert_eq!(msgs.len(), 2, "{msgs:#?}");
    assert!(msgs[0].contains("5 rows, the level has 6"), "{msgs:#?}");
    assert!(
        msgs[1].contains("missing key \\\"open_edges\\\""),
        "{msgs:#?}"
    );
    let (_t, dir) = case("clean_minimal");
    edit(&dir.join("levels/room.json"), |l| {
        l["int_grids"]["collision"]["derived"] = json!(true)
    });
    let (kind, path, message) = one_problem(&dir);
    assert_eq!(kind, "format");
    assert_eq!(path, "levels/room.json: int_grids.collision.derived");
    assert!(message.contains("collision_layer"), "{message}");
}

// ---- --fresh ----

/// `AutoLayers_6_OptionalRules` (4 levels) with its IntGrid layer renamed
/// `Collision` and `walls` renamed `solid`, imported into a temp dir next
/// to a copy of the sample atlases (so the bake reads real images and
/// nothing is written into the repo's fixtures).
fn sample() -> (tempfile::TempDir, PathBuf) {
    let samples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/ldtk-samples");
    let tmp = tempfile::tempdir().unwrap();
    let mut v: Value = serde_json::from_slice(
        &std::fs::read(samples.join("AutoLayers_6_OptionalRules.ldtk")).unwrap(),
    )
    .unwrap();
    for l in v["defs"]["layers"].as_array_mut().unwrap() {
        if l["identifier"] == "IntGrid" {
            l["identifier"] = "Collision".into();
            for val in l["intGridValues"].as_array_mut().unwrap() {
                if val["identifier"] == "walls" {
                    val["identifier"] = "solid".into();
                }
            }
        }
    }
    let ldtk = tmp.path().join("sample.ldtk");
    std::fs::write(&ldtk, serde_json::to_vec(&v).unwrap()).unwrap();
    std::fs::create_dir(tmp.path().join("atlas")).unwrap();
    for e in std::fs::read_dir(samples.join("atlas")).unwrap() {
        let e = e.unwrap();
        std::fs::copy(e.path(), tmp.path().join("atlas").join(e.file_name())).unwrap();
    }
    let dir = tmp.path().join("sample.worlds");
    let imported = worlds_core::ldtk::import(&ldtk, &dir).unwrap();
    worlds_core::ldtk::write(&imported, &dir).unwrap();
    (tmp, dir)
}

fn bake(dir: &Path) -> String {
    let out = cli().arg("bake").arg(dir).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn fresh_compares_an_in_memory_bake() {
    let (_t, dir) = sample();
    let first = bake(&dir);
    // The summary counts what changed, not what was kept.
    assert!(first.contains("; 6 files (6 changed) in "), "{first}");
    assert!(bake(&dir).contains("; 6 files (0 changed) in "));
    let export = dir.join("export");
    let (code, text) = check(&[export.as_os_str(), "--fresh".as_ref()]);
    assert_eq!(code, Some(0), "{text}");
    let (code, text) = check(&["--project".as_ref(), dir.as_os_str(), "--fresh".as_ref()]);
    assert_eq!(code, Some(0), "{text}");
    // Edit one collision cell: the level file and world.json (its open
    // edges) are stale, and nothing was written.
    let mut p = worlds_core::load(&dir).unwrap();
    let level = p.worlds[0]
        .levels
        .iter_mut()
        .find(|l| l.slug == "frozen_caves")
        .unwrap();
    let grid = level
        .int_grids
        .iter_mut()
        .find(|g| g.layer == "collision")
        .unwrap();
    assert_ne!(grid.index_at(0, 0), Some(0));
    assert!(grid.set_index(0, 0, 0));
    worlds_core::save(&mut p, &dir).unwrap();
    let before = std::fs::read(export.join("levels/frozen_caves.json")).unwrap();
    let (code, v) = check_json(&export, &["--fresh"]);
    assert_eq!(code, Some(1), "{v:#}");
    let stale: Vec<&str> = v["problems"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p["kind"] == "stale")
        .map(|p| p["file"].as_str().unwrap())
        .collect();
    assert_eq!(stale, ["world.json", "levels/frozen_caves.json"], "{v:#}");
    assert_eq!(
        std::fs::read(export.join("levels/frozen_caves.json")).unwrap(),
        before
    );
    // Rebaked: fresh again; the bake reports the two changed files.
    assert!(bake(&dir).contains("; 6 files (2 changed) in "));
    assert_eq!(check(&[export.as_os_str(), "--fresh".as_ref()]).0, Some(0));
    // A level the project no longer has, and one the export lacks.
    std::fs::copy(
        export.join("levels/frozen_caves.json"),
        export.join("levels/gone.json"),
    )
    .unwrap();
    std::fs::remove_file(export.join("levels/green_hills.json")).unwrap();
    let (_, v) = check_json(&export, &["--fresh"]);
    let stale: Vec<(&str, &str)> = v["problems"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p["kind"] == "stale")
        .map(|p| (p["file"].as_str().unwrap(), p["message"].as_str().unwrap()))
        .collect();
    assert_eq!(
        stale,
        [(
            "levels/green_hills.json",
            "export is stale: missing, a fresh bake writes it"
        )]
    );
    // gone.json isn't listed in world.json either: reported once, as that.
    let gone: Vec<&Value> = v["problems"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p["file"] == "levels/gone.json")
        .collect();
    assert_eq!(gone.len(), 1, "{v:#}");
    assert_eq!(gone[0]["kind"], "format", "{v:#}");
    // A full bake prunes the stale file and says so.
    assert!(bake(&dir).contains("; 6 files (1 changed, 1 removed) in "));
}

/// `--fresh` renders image paths for the directory being checked: an
/// export baked with `--out` elsewhere is fresh against that directory.
#[test]
fn fresh_uses_the_checked_directory_for_image_paths() {
    let (t, dir) = sample();
    let out = t.path().join("game/export");
    let o = cli()
        .arg("bake")
        .arg(&dir)
        .arg("--out")
        .arg(&out)
        .output()
        .unwrap();
    assert!(o.status.success());
    let (code, text) = check(&[
        out.as_os_str(),
        "--project".as_ref(),
        dir.as_os_str(),
        "--fresh".as_ref(),
    ]);
    assert_eq!(code, Some(0), "{text}");
    // Without --project, an export dir with no project around it.
    let (code, text) = check(&[out.as_os_str(), "--fresh".as_ref()]);
    assert_eq!(code, Some(2), "{text}");
}

/// docs/format.md's `--json` example is the real output for `flip_out_of_range`.
#[test]
fn format_md_json_example_is_real() {
    let doc =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/format.md"))
            .unwrap();
    let section = &doc[doc.find("### Checking an export").unwrap()..];
    let block = section.split("```json\n").nth(1).unwrap();
    let example: Value = serde_json::from_str(block.split("\n```").next().unwrap()).unwrap();
    let (_, real) = check_json(&fixtures().join("flip_out_of_range"), &[]);
    assert_eq!(example, real);
}

/// An exported connector (format.md, "Annotations in the export"): its
/// export-only keys (`points`, an entity end's `level`) are read, checked
/// and set aside, and the rest parses as an annotation.
#[test]
fn connectors_in_the_export() {
    let with = |c: Value| {
        let (t, dir) = case("clean_minimal");
        edit(&dir.join("world.json"), |w| {
            w["worlds"][0]["notes"] = json!([c]);
        });
        (t, dir)
    };
    let good = json!({ "id": "connroomaa", "kind": "connector", "label": "drop",
        "from": { "level": "room" }, "to": { "entity": "spawnaaaaa", "level": "room" },
        "waypoints": [[40, -20]], "points": [[40, 0], [40, -20], [48, 96]] });
    let (_t, dir) = with(good.clone());
    let (code, v) = check_json(&dir, &[]);
    assert_eq!(code, Some(0), "{v:#}");
    let mut short = good.clone();
    short["points"] = json!([[40, 0], [48, 96]]);
    let (_t, dir) = with(short);
    let (kind, at, msg) = one_problem(&dir);
    assert_eq!(kind, "format");
    assert_eq!(at, "world.json: worlds[main].notes[connroomaa].points");
    assert!(msg.contains("3 [x, y]"), "{msg}");
    let mut gone = good.clone();
    gone["to"] = json!({ "entity": "goneaaaaaa", "level": "room" });
    let (_t, dir) = with(gone);
    let (kind, at, msg) = one_problem(&dir);
    assert_eq!(kind, "reference");
    assert_eq!(at, "world.json: worlds[main].notes[connroomaa].to");
    assert_eq!(msg, "entity goneaaaaaa is not in the export");
    let mut extra = good;
    extra["from"] = json!({ "level": "room", "entity": "spawnaaaaa", "px": [0, 0] });
    let (_t, dir) = with(extra);
    let (kind, _, msg) = one_problem(&dir);
    assert_eq!(kind, "format");
    assert!(msg.contains("not an annotation"), "{msg}");
}
