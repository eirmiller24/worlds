//! Layer operations (`duplicate_layer`, merge down, move to layer),
//! reference images (commands, format, validation) and the history's
//! entry details (time, files, parts, the disk origin).

mod common;

use std::path::PathBuf;

use serde_json::json;
use worlds_core::command::{Change, Command, CommandError, History, NoteScope, Origin, layer_ops};
use worlds_core::*;

fn s(x: &str) -> Slug {
    slug(x)
}

fn id(x: &str) -> Id {
    Id::new(x).unwrap()
}

/// Apply, check undo and redo are exact, leave it applied; the changes.
fn check(p: &mut Project, cmd: Command) -> Vec<Change> {
    let json = serde_json::to_string(&cmd).unwrap();
    let back: Command = serde_json::from_str(&json).unwrap();
    assert_eq!(back, cmd, "{json}");
    let before = p.clone();
    let a = p
        .apply(cmd.clone())
        .unwrap_or_else(|e| panic!("{}: {e}", cmd.label()));
    let after = p.clone();
    let b = p.apply(a.inverse.clone()).unwrap();
    assert!(*p == before, "undo of {} isn't exact", cmd.label());
    let c = p.apply(b.inverse).unwrap();
    assert!(*p == after, "redo of {} isn't exact", cmd.label());
    assert_eq!(c.changes, a.changes);
    a.changes
}

fn tiles_layer(slug_: &str, grid: Option<u32>) -> LayerDef {
    let mut v = json!({ "slug": slug_, "name": slug_, "kind": "tiles", "tileset": "castle_main" });
    if let Some(g) = grid {
        v["grid_size"] = g.into();
    }
    serde_json::from_value(v).unwrap()
}

fn tile(x: u32, y: u32, t: [u32; 2]) -> TilePlacement {
    TilePlacement {
        cell: [x, y],
        tileset: s("castle_main"),
        tile: t,
        flip: None,
        alpha: 1.0,
    }
}

fn stack(p: &Project, level: &str, layer: &str, c: [u32; 2]) -> Vec<[u32; 2]> {
    p.level(level)
        .and_then(|l| l.tiles_layer(layer))
        .map(|t| {
            t.tiles
                .iter()
                .filter(|t| t.cell == c)
                .map(|t| t.tile)
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn duplicate_a_tiles_layer_copies_content_and_sits_above() {
    let mut p = common::fixture();
    let changes = check(
        &mut p,
        Command::DuplicateLayer {
            layer: s("decor_back"),
            slug: s("decor_back_copy"),
        },
    );
    let i = p
        .layers
        .iter()
        .position(|l| l.slug == "decor_back")
        .unwrap();
    assert_eq!(p.layers[i + 1].slug, "decor_back_copy");
    assert_eq!(p.layers[i + 1].name, p.layers[i].name);
    let crev = p.level("crevasse").unwrap();
    assert_eq!(
        crev.tiles_layer("decor_back_copy").unwrap().tiles,
        crev.tiles_layer("decor_back").unwrap().tiles
    );
    assert_eq!(
        changes[0],
        Change::Def {
            kind: command::DefKind::Layer,
            slug: s("decor_back_copy")
        }
    );
    // A taken slug is refused.
    let e = p
        .apply(Command::DuplicateLayer {
            layer: s("decor_back"),
            slug: s("collision"),
        })
        .unwrap_err();
    assert!(matches!(e, CommandError::DuplicateDef { .. }), "{e}");
    let e = p
        .apply(Command::DuplicateLayer {
            layer: s("nope"),
            slug: s("nope_copy"),
        })
        .unwrap_err();
    assert_eq!(e, CommandError::UnknownLayer(s("nope")));
}

#[test]
fn duplicate_entities_gets_fresh_stable_ids() {
    let mut p = common::fixture();
    let originals: Vec<Id> = p
        .levels()
        .filter_map(|l| l.entities_layer("objects"))
        .flat_map(|e| e.entities.iter().map(|e| e.id.clone()))
        .collect();
    assert!(!originals.is_empty());
    let cmd = Command::DuplicateLayer {
        layer: s("objects"),
        slug: s("objects_copy"),
    };
    let mut q = p.clone();
    check(&mut p, cmd.clone());
    q.apply(cmd).unwrap();
    let copies: Vec<&EntityInstance> = p
        .levels()
        .filter_map(|l| l.entities_layer("objects_copy"))
        .flat_map(|e| e.entities.iter())
        .collect();
    assert_eq!(copies.len(), originals.len());
    for c in &copies {
        assert!(!originals.contains(&c.id), "{} is an original id", c.id);
    }
    // The same every time.
    assert!(p == q);
    assert!(
        validate(&p)
            .iter()
            .all(|pr| !pr.message.contains("duplicate entity id")),
        "{:?}",
        validate(&p)
    );
}

#[test]
fn duplicate_an_auto_layer_copies_its_rules_and_state() {
    let mut p = common::fixture();
    check(
        &mut p,
        Command::DuplicateLayer {
            layer: s("main_tiling"),
            slug: s("main_tiling_copy"),
        },
    );
    let r = p.rules_for("main_tiling_copy").unwrap();
    assert_eq!(r.layer, "main_tiling_copy");
    assert_eq!(r.groups, p.rules_for("main_tiling").unwrap().groups);
    assert_eq!(
        p.level("crevasse").unwrap().layers.get("main_tiling_copy"),
        p.level("crevasse").unwrap().layers.get("main_tiling")
    );
}

#[test]
fn duplicate_a_derived_layer_keeps_it_derived() {
    let mut p = common::fixture();
    let def: LayerDef = serde_json::from_value(json!({ "slug": "marks", "name": "Marks",
        "kind": "int_grid", "derived": true,
        "values": [{ "slug": "pillar_base", "name": "Pillar base", "color": "#ff0000", "glyph": "p" }] }))
    .unwrap();
    p.apply(Command::SetLayerDef {
        layer: s("marks"),
        def,
        index: None,
        content: vec![command::LayerContent {
            level: s("crevasse"),
            rows: Some(
                [
                    "....................",
                    "..p.................",
                    ".........!..........",
                    "....................",
                    "....................",
                ]
                .map(String::from)
                .to_vec(),
            ),
            tiles: None,
            entities: None,
            state: None,
        }],
        rules: None,
    })
    .unwrap();
    check(
        &mut p,
        Command::DuplicateLayer {
            layer: s("marks"),
            slug: s("marks_copy"),
        },
    );
    assert!(p.layer("marks_copy").unwrap().is_derived());
    let crev = p.level("crevasse").unwrap();
    assert_eq!(
        crev.overrides("marks_copy")
            .unwrap()
            .cells()
            .collect::<Vec<_>>(),
        crev.overrides("marks").unwrap().cells().collect::<Vec<_>>()
    );
}

/// The fixture plus a `decor_front` tiles layer right above `decor_back`
/// (same 8 px grid), with tiles over and beside decor_back's.
fn two_tile_layers() -> Project {
    let mut p = common::fixture();
    let i = p
        .layers
        .iter()
        .position(|l| l.slug == "decor_back")
        .unwrap();
    p.apply(Command::SetLayerDef {
        layer: s("decor_front"),
        def: tiles_layer("decor_front", Some(8)),
        index: Some(i + 1),
        content: vec![command::LayerContent {
            level: s("crevasse"),
            rows: None,
            tiles: Some(vec![tile(4, 2, [9, 9]), tile(7, 0, [1, 1])]),
            entities: None,
            state: None,
        }],
        rules: None,
    })
    .unwrap();
    p
}

#[test]
fn merge_down_stacks_the_upper_layer_over_the_lower() {
    let mut p = two_tile_layers();
    let before = p.clone();
    let cmd = layer_ops::merge_down(&p, &s("decor_front")).unwrap();
    assert_eq!(cmd.label(), "Merge decor_front down into decor_back");
    check(&mut p, cmd);
    assert!(p.layer("decor_front").is_none());
    assert_eq!(
        stack(&p, "crevasse", "decor_back", [4, 2]),
        [[5, 1], [9, 9]]
    );
    assert_eq!(stack(&p, "crevasse", "decor_back", [7, 0]), [[1, 1]]);
    assert_eq!(
        stack(&p, "crevasse", "decor_back", [5, 2]),
        stack(&before, "crevasse", "decor_back", [5, 2])
    );
}

#[test]
fn merge_down_is_refused_off_a_tiles_layer_or_grid() {
    let p = common::fixture();
    // decor_back sits on main_tiling, an auto layer.
    let e = layer_ops::merge_down(&p, &s("decor_back")).unwrap_err();
    assert!(e.to_string().contains("auto layer"), "{e}");
    let e = layer_ops::merge_down(&p, &s("collision")).unwrap_err();
    assert!(matches!(e, CommandError::WrongLayerKind { .. }), "{e}");
    // A tiles layer on another grid right above decor_back.
    let mut p = common::fixture();
    let i = p
        .layers
        .iter()
        .position(|l| l.slug == "decor_back")
        .unwrap();
    p.apply(Command::SetLayerDef {
        layer: s("big"),
        def: tiles_layer("big", None),
        index: Some(i + 1),
        content: Vec::new(),
        rules: None,
    })
    .unwrap();
    let e = layer_ops::merge_down(&p, &s("big")).unwrap_err();
    assert!(e.to_string().contains("same grid"), "{e}");
}

#[test]
fn move_tiles_and_entities_to_another_layer() {
    let mut p = two_tile_layers();
    let cmd = layer_ops::move_tiles(
        &p,
        &s("crevasse"),
        &s("decor_back"),
        &s("decor_front"),
        &[[4, 2], [5, 2], [0, 0]],
    )
    .unwrap();
    assert_eq!(cmd.label(), "Move 2 tile stacks to decor_front");
    check(&mut p, cmd);
    assert_eq!(
        stack(&p, "crevasse", "decor_front", [4, 2]),
        [[9, 9], [5, 1]]
    );
    assert!(stack(&p, "crevasse", "decor_back", [4, 2]).is_empty());
    assert!(stack(&p, "crevasse", "decor_back", [5, 2]).is_empty());
    assert_eq!(stack(&p, "crevasse", "decor_back", [4, 1]).len(), 1);
    // Nothing to move: an empty batch.
    let none = layer_ops::move_tiles(
        &p,
        &s("crevasse"),
        &s("decor_back"),
        &s("decor_front"),
        &[[0, 0]],
    )
    .unwrap();
    assert!(none.is_noop());

    // Entities: a second entities layer.
    let mut p = common::fixture();
    let def: LayerDef =
        serde_json::from_value(json!({ "slug": "props", "name": "Props", "kind": "entities" }))
            .unwrap();
    p.apply(Command::SetLayerDef {
        layer: s("props"),
        def,
        index: None,
        content: Vec::new(),
        rules: None,
    })
    .unwrap();
    let door = p
        .level("crevasse")
        .unwrap()
        .all_entities()
        .find(|e| e.id == id("doorcrev22"))
        .unwrap()
        .clone();
    let cmd = layer_ops::move_entities(&p, &[id("doorcrev22")], &s("props")).unwrap();
    assert_eq!(cmd.label(), "Move doorcrev22 to props");
    check(&mut p, cmd);
    let moved = &p
        .level("crevasse")
        .unwrap()
        .entities_layer("props")
        .unwrap()
        .entities[0];
    assert_eq!(*moved, door);
    assert!(
        layer_ops::move_entities(&p, &[id("doorcrev22")], &s("props"))
            .unwrap()
            .is_noop()
    );
    let e = layer_ops::move_entities(&p, &[id("doorcrev22")], &s("collision")).unwrap_err();
    assert!(matches!(e, CommandError::WrongLayerKind { .. }));
}

fn sketch(x: &str) -> Reference {
    Reference::new(id(x), "art/sketch.png", [0, -16])
}

#[test]
fn reference_commands_invert_and_report_their_file() {
    let mut p = common::fixture();
    let changes = check(
        &mut p,
        Command::AddReference {
            scope: NoteScope::Level(s("crevasse")),
            reference: sketch("refsketch2"),
            index: None,
        },
    );
    assert_eq!(
        changes,
        [Change::Reference {
            world: s("main"),
            level: Some(s("crevasse")),
            id: id("refsketch2"),
        }]
    );
    assert_eq!(
        changes[0].file(),
        PathBuf::from("worlds/main/levels/crevasse/level.json")
    );
    let changes = check(
        &mut p,
        Command::AddReference {
            scope: NoteScope::World(s("main")),
            reference: sketch("refworld22"),
            index: Some(0),
        },
    );
    assert_eq!(changes[0].file(), PathBuf::from("worlds/main/world.json"));
    // Ids are unique across scopes.
    let e = p
        .apply(Command::AddReference {
            scope: NoteScope::World(s("main")),
            reference: sketch("refsketch2"),
            index: None,
        })
        .unwrap_err();
    assert_eq!(e, CommandError::DuplicateId(id("refsketch2")));
    let mut r = sketch("refsketch2");
    r.scale = 2.0;
    r.locked = true;
    check(
        &mut p,
        Command::UpdateReference {
            id: id("refsketch2"),
            reference: r.clone(),
        },
    );
    assert_eq!(p.level("crevasse").unwrap().references, [r.clone()]);
    // The same entry again is a no-op.
    let a = p
        .apply(Command::UpdateReference {
            id: id("refsketch2"),
            reference: r,
        })
        .unwrap();
    assert!(a.changes.is_empty());
    let e = p
        .apply(Command::UpdateReference {
            id: id("refsketch2"),
            reference: sketch("refother22"),
        })
        .unwrap_err();
    assert!(matches!(e, CommandError::Invalid(_)));
    check(
        &mut p,
        Command::RemoveReference {
            id: id("refsketch2"),
        },
    );
    assert!(p.level("crevasse").unwrap().references.is_empty());
    let e = p
        .apply(Command::RemoveReference {
            id: id("refsketch2"),
        })
        .unwrap_err();
    assert_eq!(e, CommandError::UnknownReference(id("refsketch2")));
    assert_eq!(
        serde_json::to_value(&e).unwrap()["kind"],
        "unknown_reference"
    );
}

#[test]
fn references_move_with_the_content_on_a_resize_and_survive_delete() {
    let mut p = common::fixture();
    p.apply(Command::AddReference {
        scope: NoteScope::Level(s("crevasse")),
        reference: sketch("refsketch2"),
        index: None,
    })
    .unwrap();
    p.apply(Command::ResizeLevel {
        world: s("main"),
        level: s("crevasse"),
        size: [22, 5],
        anchor: [1, -1],
    })
    .unwrap();
    // Two cells more on the left: the image keeps its world position.
    assert_eq!(p.level("crevasse").unwrap().references[0].px, [32, -16]);
    let before = p.clone();
    let a = p
        .apply(Command::DeleteLevel {
            world: s("main"),
            level: s("crevasse"),
            keep_layout: false,
        })
        .unwrap();
    p.apply(a.inverse).unwrap();
    assert!(p == before);
}

#[test]
fn references_save_load_and_old_files_load() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = common::fixture();
    save_all(&mut p, dir.path()).unwrap();
    // The fixture predates references: nothing is written for them.
    let level =
        std::fs::read_to_string(dir.path().join("worlds/main/levels/crevasse/level.json")).unwrap();
    assert!(!level.contains("references"));
    let mut r = sketch("refsketch2");
    r.opacity = 0.5;
    p.apply(Command::AddReference {
        scope: NoteScope::Level(s("crevasse")),
        reference: r,
        index: None,
    })
    .unwrap();
    p.apply(Command::AddReference {
        scope: NoteScope::World(s("main")),
        reference: sketch("refworld22"),
        index: None,
    })
    .unwrap();
    save_dirty(&mut p, dir.path()).unwrap();
    let level =
        std::fs::read_to_string(dir.path().join("worlds/main/levels/crevasse/level.json")).unwrap();
    assert!(
        level.contains(
            r#"{ "id": "refsketch2", "image": "art/sketch.png", "px": [0, -16], "opacity": 0.5 }"#
        ),
        "{level}"
    );
    let q = load(dir.path()).unwrap();
    assert!(q == p);
    // The image doesn't exist yet: validation says so, once per reference.
    let missing: Vec<String> = validate(&q)
        .iter()
        .filter(|pr| pr.message.contains("doesn't exist"))
        .map(|pr| pr.to_string())
        .collect();
    assert_eq!(
        missing,
        [
            "worlds/main/world.json: references[refworld22].image: image file \"art/sketch.png\" doesn't exist",
            "worlds/main/levels/crevasse/level.json: references[refsketch2].image: image file \"art/sketch.png\" doesn't exist",
        ]
    );
    std::fs::create_dir_all(dir.path().join("art")).unwrap();
    std::fs::write(dir.path().join("art/sketch.png"), b"png").unwrap();
    assert!(
        validate(&q)
            .iter()
            .all(|pr| !pr.path.starts_with("references")),
        "{:?}",
        validate(&q)
    );
}

#[test]
fn reference_values_are_validated() {
    let mut p = common::fixture();
    let mut r = sketch("refsketch2");
    r.image = "../outside.png".into();
    r.scale = 0.0;
    r.opacity = 2.0;
    p.worlds[0].levels[0].references.push(r);
    let found: Vec<String> = validate(&p)
        .iter()
        .filter(|pr| pr.path.starts_with("references"))
        .map(|pr| format!("{}: {}", pr.path, pr.message))
        .collect();
    assert_eq!(found.len(), 3, "{found:?}");
    assert!(found[0].contains("inside the project"), "{found:?}");
    assert!(found[1].starts_with("references[refsketch2].scale"));
    assert!(found[2].starts_with("references[refsketch2].opacity"));
}

#[test]
fn history_entries_know_their_files_parts_and_time() {
    let mut p = common::fixture();
    let mut h = History::new();
    let t0 = std::time::SystemTime::now();
    h.push(
        &mut p,
        Command::AddReference {
            scope: NoteScope::Level(s("crevasse")),
            reference: sketch("refsketch2"),
            index: None,
        },
    )
    .unwrap();
    let e = h.undo_entries().last().unwrap();
    assert_eq!(e.parts, ["Add reference image"]);
    assert_eq!(
        e.files,
        [PathBuf::from("worlds/main/levels/crevasse/level.json")]
    );
    assert!(e.time >= t0);
    // An agent's batch recorded with its parts and changes.
    let a = p
        .apply(Command::RemoveReference {
            id: id("refsketch2"),
        })
        .unwrap();
    let b = p
        .apply(Command::AddReference {
            scope: NoteScope::World(s("main")),
            reference: sketch("refworld22"),
            index: None,
        })
        .unwrap();
    let changes: Vec<Change> = a.changes.iter().chain(&b.changes).cloned().collect();
    assert!(h.record_with(
        "claude: move the sketch",
        Origin::Agent("claude".into()),
        vec![a.inverse, b.inverse],
        vec![
            "Remove reference image refsketch2".into(),
            "Add reference image".into()
        ],
        &changes,
    ));
    let e = h.undo_entries().last().unwrap();
    assert_eq!(e.parts.len(), 2);
    assert_eq!(e.files.len(), 2);
    // `record` without changes learns its files on the first undo.
    let c = p
        .apply(Command::RemoveReference {
            id: id("refworld22"),
        })
        .unwrap();
    assert!(h.record("x", Origin::Disk, vec![c.inverse]));
    assert!(h.undo_entries().last().unwrap().files.is_empty());
    h.undo(&mut p).unwrap();
    assert_eq!(
        h.redo_entries().last().unwrap().files,
        [PathBuf::from("worlds/main/world.json")]
    );
    assert_eq!(h.redo_entries().last().unwrap().origin, Origin::Disk);
    assert_eq!(serde_json::to_value(Origin::Disk).unwrap(), json!("disk"));
}

#[test]
fn removing_or_renaming_a_world_keeps_its_references() {
    let mut p = common::fixture();
    p.apply(Command::CreateWorld {
        slug: s("caves"),
        name: None,
    })
    .unwrap();
    p.apply(Command::AddReference {
        scope: NoteScope::World(s("caves")),
        reference: sketch("refcaves22"),
        index: None,
    })
    .unwrap();
    // Not empty any more: removing it needs confirm.
    let e = p
        .apply(Command::RemoveWorld {
            world: s("caves"),
            confirm: false,
        })
        .unwrap_err();
    assert!(e.to_string().contains("reference image"), "{e}");
    let changes = check(
        &mut p,
        Command::RemoveWorld {
            world: s("caves"),
            confirm: true,
        },
    );
    assert!(changes.contains(&Change::Reference {
        world: s("caves"),
        level: None,
        id: id("refcaves22"),
    }));
    check(
        &mut p,
        Command::RenameWorld {
            world: s("main"),
            slug: Some(s("surface")),
            name: None,
        },
    );
    p.apply(Command::AddReference {
        scope: NoteScope::World(s("surface")),
        reference: sketch("refsurf222"),
        index: None,
    })
    .unwrap();
    check(
        &mut p,
        Command::RenameWorld {
            world: s("surface"),
            slug: Some(s("top")),
            name: None,
        },
    );
    assert_eq!(p.world("top").unwrap().references[0].id, id("refsurf222"));
}

#[test]
fn duplicate_level_gives_its_references_new_ids() {
    let mut p = common::fixture();
    p.apply(Command::AddReference {
        scope: NoteScope::Level(s("crevasse")),
        reference: sketch("refsketch2"),
        index: None,
    })
    .unwrap();
    check(
        &mut p,
        Command::DuplicateLevel {
            level: s("crevasse"),
            slug: s("crevasse_b"),
            name: None,
            world: None,
            world_px: None,
        },
    );
    let copy = &p.level("crevasse_b").unwrap().references;
    assert_eq!(copy.len(), 1);
    assert_ne!(copy[0].id, id("refsketch2"));
    assert_eq!(copy[0].image, "art/sketch.png");
}

#[test]
fn duplicate_layer_copies_into_every_world() {
    let mut p = common::fixture();
    p.apply(Command::CreateWorld {
        slug: s("caves"),
        name: None,
    })
    .unwrap();
    p.apply(Command::MoveLevelToWorld {
        level: s("crevasse"),
        world: s("caves"),
        world_px: None,
        keep_layout: false,
    })
    .unwrap();
    check(
        &mut p,
        Command::DuplicateLayer {
            layer: s("decor_back"),
            slug: s("decor_back_copy"),
        },
    );
    assert_eq!(p.world_of_level("crevasse").unwrap().slug, "caves");
    let crev = p.level("crevasse").unwrap();
    assert!(!crev.tiles_layer("decor_back").unwrap().tiles.is_empty());
    assert_eq!(
        crev.tiles_layer("decor_back_copy").unwrap().tiles,
        crev.tiles_layer("decor_back").unwrap().tiles
    );
}
