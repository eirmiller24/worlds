//! Assemblies (`assemblies/<slug>.json`, set/rename/remove, placing) and
//! transforms (fragments, selections, arranging levels).

mod common;

use std::collections::BTreeMap;

use worlds_core::assembly::{self, Assembly, AssemblyInfo};
use worlds_core::command::{Command, CommandError, Rect};
use worlds_core::fragment::{self, CopySelection, Fragment, FragmentGrid};
use worlds_core::transform::{self, ArrangeOp};
use worlds_core::*;

fn s(x: &str) -> Slug {
    slug(x)
}

fn id(x: &str) -> Id {
    Id::new(x).unwrap()
}

fn rows(p: &Project, level: &str, layer: &str) -> Vec<String> {
    let def = p.layer(layer).unwrap();
    p.level(level)
        .unwrap()
        .int_grid(layer)
        .map(|g| g.to_rows(def).unwrap())
        .unwrap_or_default()
}

fn entity<'p>(p: &'p Project, id: &Id) -> &'p EntityInstance {
    p.levels()
        .flat_map(|l| l.all_entities())
        .find(|e| e.id == *id)
        .unwrap_or_else(|| panic!("no entity {id}"))
}

/// Apply, check undo and redo are exact; leave it applied.
fn apply_exact(p: &mut Project, cmd: Command) {
    let json = serde_json::to_string(&cmd).unwrap();
    assert_eq!(serde_json::from_str::<Command>(&json).unwrap(), cmd);
    let before = p.clone();
    let a = p.apply(cmd).unwrap();
    assert!(!a.changes.is_empty());
    let after = p.clone();
    let b = p.apply(a.inverse).unwrap();
    assert!(*p == before, "undo isn't exact");
    p.apply(b.inverse).unwrap();
    assert!(*p == after, "redo isn't exact");
}

fn info(slug: &str) -> AssemblyInfo {
    AssemblyInfo {
        slug: s(slug),
        name: "Doorway".into(),
        category: Some("Puzzle/Doors".into()),
        tags: vec!["door".into()],
        doc: None,
    }
}

fn doorway(p: &Project) -> Assembly {
    let frag = fragment::copy(
        p,
        &CopySelection {
            entities: vec![id("btncrev222"), id("doorcrev22")],
            ..Default::default()
        },
    )
    .unwrap();
    Assembly::new(info("doorway"), frag)
}

#[test]
fn assembly_commands_are_exact_and_save_their_own_file() {
    let mut p = common::fixture();
    assert!(p.assemblies.is_empty(), "old projects load with none");
    let a = doorway(&p);
    assert_eq!(a.fragment().source, None);
    apply_exact(
        &mut p,
        Command::SetAssembly {
            assembly: a.clone(),
        },
    );
    assert_eq!(p.assembly("doorway"), Some(&a));
    // Setting the same again is a no-op.
    let applied = p
        .apply(Command::SetAssembly {
            assembly: a.clone(),
        })
        .unwrap();
    assert!(applied.changes.is_empty());
    let mut b = a.clone();
    b.name = "Door way".into();
    apply_exact(&mut p, Command::SetAssembly { assembly: b });

    let dir = tempfile::tempdir().unwrap();
    save(&mut p, dir.path()).unwrap();
    let text = std::fs::read_to_string(dir.path().join("assemblies/doorway.json")).unwrap();
    assert!(text.starts_with("{\n  \"$schema\": \"../schema/assembly.json\",\n  \"format\": 1,\n  \"slug\": \"doorway\""), "{text}");
    let back = load(dir.path()).unwrap();
    assert_eq!(back.assemblies, p.assemblies);

    apply_exact(
        &mut p,
        Command::RenameAssembly {
            slug: s("doorway"),
            new_slug: s("door_pair"),
        },
    );
    assert!(p.assembly("doorway").is_none());
    let err = p
        .apply(Command::RenameAssembly {
            slug: s("nope"),
            new_slug: s("x"),
        })
        .unwrap_err();
    assert_eq!(err, CommandError::UnknownAssembly(s("nope")));
    // The rename deletes the old file on an incremental save.
    save_dirty(&mut p, dir.path()).unwrap();
    assert!(!dir.path().join("assemblies/doorway.json").exists());
    assert!(dir.path().join("assemblies/door_pair.json").exists());

    apply_exact(
        &mut p,
        Command::RemoveAssembly {
            slug: s("door_pair"),
        },
    );
    save_dirty(&mut p, dir.path()).unwrap();
    assert!(!dir.path().join("assemblies/door_pair.json").exists());
}

#[test]
fn assembly_file_name_must_match_and_validation_checks_references() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = common::fixture();
    let mut a = doorway(&p);
    a.category = Some("/Doors".into());
    a.grid_size = 8;
    a.entities[0].ty = s("ghost");
    p.assemblies.push(a);
    save_all(&mut p, dir.path()).unwrap();
    let problems: Vec<String> = validate(&load(dir.path()).unwrap())
        .iter()
        .map(|p| p.to_string())
        .collect();
    let mine: Vec<&String> = problems
        .iter()
        .filter(|p| p.starts_with("assemblies/"))
        .collect();
    assert_eq!(mine.len(), 3, "{problems:#?}");
    assert!(mine.iter().any(|p| p.contains("category")));
    assert!(mine.iter().any(|p| p.contains("grid_size")));
    assert!(mine.iter().any(|p| p.contains("no entity type \"ghost\"")));

    std::fs::rename(
        dir.path().join("assemblies/doorway.json"),
        dir.path().join("assemblies/other.json"),
    )
    .unwrap();
    let err = load(dir.path()).unwrap_err().to_string();
    assert!(err.contains("other.json"), "{err}");
}

#[test]
fn placing_an_assembly_is_a_paste_with_new_ids() {
    let mut p = common::fixture();
    p.apply(Command::SetAssembly {
        assembly: doorway(&p),
    })
    .unwrap();
    let plan = assembly::place(&p, &s("doorway"), &s("shaft"), [0, 2], None).unwrap();
    assert_eq!(plan.label, "Place assembly doorway");
    assert_eq!(plan.entities.len(), 1, "{:?}", plan.problems);
    // The door lands outside the 6-wide shaft, so only the button goes in.
    assert!(plan.problems.iter().any(|m| m.contains("outside")));
    let plan = assembly::place(&p, &s("doorway"), &s("crevasse"), [2, 1], None).unwrap();
    assert_eq!(plan.entities.len(), 2);
    p.apply(plan.command()).unwrap();
    let btn = entity(&p, &plan.entities[0]);
    assert_eq!(btn.cell, [2, 1]);
    assert_eq!(btn.fields["target"], plan.entities[1].as_str());

    let err = assembly::place(&p, &s("nope"), &s("crevasse"), [0, 0], None).unwrap_err();
    assert_eq!(err, CommandError::UnknownAssembly(s("nope")));

    // Mirrored before it lands: the door is now on the left.
    let plan = assembly::place(&p, &s("doorway"), &s("crevasse"), [2, 1], Some(Flip::X)).unwrap();
    p.apply(plan.command()).unwrap();
    assert_eq!(entity(&p, &plan.entities[0]).cell, [8, 1]);
    assert_eq!(entity(&p, &plan.entities[1]).cell, [2, 1]);
}

fn tile_fragment(tileset: &str) -> Fragment {
    let mut tiles = BTreeMap::new();
    // decor_back has an 8 px grid: a 2x1 fragment of the 16 px project
    // grid is 4x2 of its cells, and a 16 px tile covers 2x2 of them.
    tiles.insert(
        s("decor_back"),
        vec![TilePlacement {
            cell: [0, 0],
            tileset: s(tileset),
            tile: [5, 1],
            flip: None,
            alpha: 1.0,
        }],
    );
    let mut grids = BTreeMap::new();
    grids.insert(
        s("collision"),
        FragmentGrid {
            legend: BTreeMap::new(),
            rows: vec!["#o".into()],
        },
    );
    Fragment {
        worlds_fragment: fragment::FRAGMENT_VERSION,
        grid_size: 16,
        size: [2, 1],
        source: None,
        grids,
        tiles,
        entities: Vec::new(),
        notes: Vec::new(),
    }
}

#[test]
fn fragment_turns_move_cells_and_compose_tile_flips() {
    let p = common::fixture();
    let f = tile_fragment("castle_main");
    let m = transform::fragment(&p, &f, Flip::X).unwrap();
    assert_eq!(m.size, [2, 1]);
    assert_eq!(m.grids[&s("collision")].rows, vec!["o#"]);
    let t = &m.tiles[&s("decor_back")][0];
    assert_eq!(t.cell, [2, 0]);
    assert_eq!(t.flip, Some(Flip::X));
    // Mirroring twice is where it started.
    assert_eq!(transform::fragment(&p, &m, Flip::X).unwrap(), f);

    // A quarter turn clockwise: 2 wide becomes 2 tall, the tile turns xt.
    let r = transform::fragment(&p, &f, Flip::XT).unwrap();
    assert_eq!(r.size, [1, 2]);
    assert_eq!(r.grids[&s("collision")].rows, vec!["#", "o"]);
    let t = &r.tiles[&s("decor_back")][0];
    assert_eq!((t.cell, t.flip), ([0, 0], Some(Flip::XT)));
    // Four quarter turns: back where it started.
    let mut q = f.clone();
    for _ in 0..4 {
        q = transform::fragment(&p, &q, Flip::XT).unwrap();
    }
    assert_eq!(q, f);
    // `then` composes like turning the tile twice.
    assert_eq!(transform::then(Some(Flip::XT), Flip::XT), Some(Flip::XY));
    assert_eq!(transform::then(Some(Flip::X), Flip::X), None);
}

#[test]
fn rotating_refuses_tiles_that_are_not_square() {
    let mut p = common::fixture();
    let mut def = p.tileset("castle_main").unwrap().clone();
    def.slug = s("tall");
    def.tile_size = [16, 32];
    p.apply(Command::SetTilesetDef {
        tileset: s("tall"),
        def,
        index: None,
    })
    .unwrap();
    let f = tile_fragment("tall");
    let err = transform::fragment(&p, &f, Flip::XT).unwrap_err();
    assert!(err.to_string().contains("square tiles"), "{err}");
    // Mirrors are fine.
    transform::fragment(&p, &f, Flip::Y).unwrap();
}

#[test]
fn selection_mirror_moves_entities_keeping_ids_and_fields() {
    let mut p = common::fixture();
    let sel = CopySelection {
        entities: vec![id("btncrev222"), id("doorcrev22")],
        ..Default::default()
    };
    let before_door = entity(&p, &id("doorcrev22")).clone();
    let plan = transform::selection(&p, &sel, Flip::X).unwrap();
    assert_eq!(plan.label, "Mirror 2 items left-right");
    // The door's type has `facing` (left/right): the status bar says so.
    assert!(
        plan.notes.iter().any(|n| n.contains("locked_door.facing")),
        "{:?}",
        plan.notes
    );
    apply_exact(&mut p, plan.command());
    let btn = entity(&p, &id("btncrev222"));
    let door = entity(&p, &id("doorcrev22"));
    assert_eq!((btn.cell, btn.offset_px), ([18, 3], [0, -2]));
    assert_eq!(door.cell, [12, 3]);
    assert_eq!(door.size, before_door.size);
    assert_eq!(door.fields, before_door.fields, "fields never change");

    // One entity mirrored about its own box stays put.
    let one = CopySelection {
        entities: vec![id("btncrev222")],
        ..Default::default()
    };
    for flip in [Flip::X, Flip::Y] {
        let plan = transform::selection(&p, &one, flip).unwrap();
        assert!(plan.commands.is_empty(), "{flip:?}: {:?}", plan.commands);
    }
}

#[test]
fn selection_rotate_turns_grid_cells_about_the_center() {
    let mut p = common::fixture();
    // "oo" at row 2, cells 8 and 9.
    let sel = CopySelection {
        level: Some(s("crevasse")),
        rect: Some(Rect::new(8, 2, 2, 1)),
        layers: vec![s("collision")],
        ..Default::default()
    };
    let plan = transform::selection(&p, &sel, Flip::XT).unwrap();
    apply_exact(&mut p, plan.command());
    let r = rows(&p, "crevasse", "collision");
    assert_eq!(r[2], "#........ooo.......#");
    assert_eq!(r[3], "#L..~~~..o....ff####");
    assert_eq!(plan.rect, [9, 2, 1, 2]);
}

#[test]
fn selection_turns_tile_stacks_and_drops_what_leaves_the_level() {
    let mut p = common::fixture();
    // decor_back (8 px cells): [4, 2] and [5, 2] (flip x) of castle_main.
    let mut tiles = BTreeMap::new();
    tiles.insert(s("decor_back"), vec![[4, 2], [5, 2]]);
    let sel = CopySelection {
        level: Some(s("crevasse")),
        tiles,
        ..Default::default()
    };
    let plan = transform::selection(&p, &sel, Flip::X).unwrap();
    apply_exact(&mut p, plan.command());
    let t = &p
        .level("crevasse")
        .unwrap()
        .tiles_layer("decor_back")
        .unwrap()
        .tiles;
    let at = |c: [u32; 2]| t.iter().find(|x| x.cell == c).cloned();
    // The two swap, and each one's flip gains an x.
    assert_eq!(at([4, 2]).unwrap().tile, [6, 1]);
    assert_eq!(at([4, 2]).unwrap().flip, None);
    assert_eq!(at([5, 2]).unwrap().tile, [5, 1]);
    assert_eq!(at([5, 2]).unwrap().flip, Some(Flip::X));
}

#[test]
fn arrange_levels_aligns_and_butts() {
    let p = common::fixture();
    let levels = [s("crevasse"), s("shaft")];
    // crevasse: [0, 0] 320x80; shaft: [320, 80] 96x128.
    let moved = |op: ArrangeOp| -> Vec<(String, [i64; 2])> {
        let Command::Batch { commands, .. } = transform::arrange_levels(&p, &levels, op).unwrap()
        else {
            panic!()
        };
        commands
            .into_iter()
            .map(|c| match c {
                Command::MoveLevel {
                    level, world_px, ..
                } => (level.to_string(), world_px),
                c => panic!("{c:?}"),
            })
            .collect()
    };
    assert_eq!(moved(ArrangeOp::AlignLeft), [("shaft".into(), [0, 80])]);
    assert_eq!(moved(ArrangeOp::AlignTop), [("shaft".into(), [320, 0])]);
    assert_eq!(moved(ArrangeOp::AlignRight), [("crevasse".into(), [96, 0])]);
    assert_eq!(
        moved(ArrangeOp::AlignBottom),
        [("crevasse".into(), [0, 128])]
    );
    assert_eq!(
        moved(ArrangeOp::AlignCenterX),
        [("shaft".into(), [112, 80])]
    );
    // Already side by side and stacked.
    assert!(moved(ArrangeOp::ButtLeft).is_empty());
    assert!(moved(ArrangeOp::ButtUp).is_empty());
    assert!(moved(ArrangeOp::DistributeX).is_empty());

    let err = transform::arrange_levels(&p, &levels[..1], ArrangeOp::AlignLeft).unwrap_err();
    assert!(err.to_string().contains("at least two"), "{err}");
}

#[test]
fn distribute_spaces_levels_evenly() {
    let mut p = common::fixture();
    // A third level (96 px wide, like the shaft), between the two.
    p.apply(Command::DuplicateLevel {
        level: s("shaft"),
        slug: s("mid"),
        name: None,
        world: None,
        world_px: Some([400, 0]),
    })
    .unwrap();
    // crevasse 0..320, shaft 320..416: move shaft to 600 (ends 696).
    p.apply(Command::MoveLevel {
        world: s("main"),
        level: s("shaft"),
        world_px: [600, 80],
    })
    .unwrap();
    let cmd = transform::arrange_levels(
        &p,
        &[s("crevasse"), s("mid"), s("shaft")],
        ArrangeOp::DistributeX,
    )
    .unwrap();
    apply_exact(&mut p, cmd);
    // Span 0..696, widths 320 + 96 + 96 = 512: two gaps of 92, rounded
    // to the grid: mid at 320 + 92 = 412 -> 416.
    assert_eq!(p.level("mid").unwrap().world_px, [416, 0]);
    let cmd = transform::arrange_levels(
        &p,
        &[s("crevasse"), s("mid"), s("shaft")],
        ArrangeOp::ButtLeft,
    )
    .unwrap();
    apply_exact(&mut p, cmd);
    assert_eq!(p.level("mid").unwrap().world_px, [320, 0]);
    assert_eq!(p.level("shaft").unwrap().world_px, [416, 80]);
}
