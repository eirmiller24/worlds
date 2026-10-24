//! The level order (`reorder_levels`, `world.json` `levels`), world layouts
//! (`set_world_layout`: strips relayout, grid vania validates) and object
//! groups (`set_group`, `remove_group`, the group helpers), and layer
//! folders: exact inverses, saving, old files, validation.

mod common;

use std::fs;

use worlds_core::command::{Change, Command, groups};
use worlds_core::*;

fn s(x: &str) -> Slug {
    slug(x)
}

fn id(x: &str) -> Id {
    Id::new(x).unwrap()
}

/// Apply `cmd`, check undo and redo are exact, leave it applied and
/// return its changes.
fn check(p: &mut Project, cmd: Command) -> Vec<Change> {
    let json = serde_json::to_string(&cmd).unwrap();
    assert_eq!(serde_json::from_str::<Command>(&json).unwrap(), cmd);
    let before = p.clone();
    let a = p
        .apply(cmd.clone())
        .unwrap_or_else(|e| panic!("{}: {e}", cmd.label()));
    let after = p.clone();
    let inv: Command = serde_json::from_str(&serde_json::to_string(&a.inverse).unwrap()).unwrap();
    let b = p.apply(inv).unwrap();
    assert!(*p == before, "undo of {} isn't exact", cmd.label());
    p.apply(b.inverse).unwrap();
    assert!(*p == after, "redo of {} isn't exact", cmd.label());
    a.changes
}

fn levels(p: &Project) -> Vec<&str> {
    p.worlds[0].levels.iter().map(|l| l.slug.as_str()).collect()
}

fn px(p: &Project, l: &str) -> [i64; 2] {
    p.level(l).unwrap().world_px
}

fn reorder(order: &[&str]) -> Command {
    Command::ReorderLevels {
        world: s("main"),
        order: order.iter().map(|x| s(x)).collect(),
        restore: false,
    }
}

fn layout(mode: LayoutMode, gap: Option<u32>, cell: Option<[u32; 2]>) -> Command {
    Command::SetWorldLayout {
        world: s("main"),
        layout: WorldLayout { mode, gap, cell },
        restore: false,
    }
}

fn new_level(slug: &str, size: [u32; 2]) -> Command {
    Command::CreateLevel {
        world: s("main"),
        level: Level::new(s(slug), slug, size),
        content: Default::default(),
        restore: false,
    }
}

#[test]
fn reorder_levels_sets_the_order_the_export_follows() {
    let mut p = common::fixture();
    assert_eq!(levels(&p), ["crevasse", "shaft"]);
    let changes = check(&mut p, reorder(&["shaft", "crevasse"]));
    assert_eq!(levels(&p), ["shaft", "crevasse"]);
    assert_eq!(changes, [Change::WorldMeta { world: s("main") }]);
    // Free layout: nothing moved.
    assert_eq!(px(&p, "crevasse"), [0, 0]);
    // The export lists levels in that order.
    let world = worlds_core::export::world_json(&p);
    let (a, b) = (
        world.find(r#""shaft""#).unwrap(),
        world.find(r#""crevasse""#).unwrap(),
    );
    assert!(a < b, "{world}");
    // Refusals: a level twice, one left out, one of another world.
    for bad in [
        reorder(&["shaft", "shaft"]),
        reorder(&["shaft"]),
        reorder(&["shaft", "crevasse", "nope"]),
    ] {
        assert!(p.apply(bad).is_err());
    }
    // A new level goes last; a rename keeps its place; [] drops the list.
    check(&mut p, new_level("aaa", [4, 4]));
    assert_eq!(levels(&p), ["shaft", "crevasse", "aaa"]);
    check(
        &mut p,
        Command::RenameLevel {
            world: s("main"),
            level: s("shaft"),
            slug: s("zshaft"),
            name: "Shaft".into(),
        },
    );
    assert_eq!(levels(&p), ["zshaft", "crevasse", "aaa"]);
    assert_eq!(
        p.worlds[0].level_order,
        [s("zshaft"), s("crevasse"), s("aaa")]
    );
    check(
        &mut p,
        Command::DeleteLevel {
            world: s("main"),
            level: s("crevasse"),
            keep_layout: false,
        },
    );
    assert_eq!(p.worlds[0].level_order, [s("zshaft"), s("aaa")]);
    check(&mut p, reorder(&[]));
    assert_eq!(levels(&p), ["aaa", "zshaft"]);
}

#[test]
fn a_world_without_an_order_saves_byte_for_byte() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("p.worlds");
    let mut p = common::fixture();
    save_all(&mut p, &dir).unwrap();
    let before = common::read_tree(&dir);
    // Creating and deleting a level in a free world writes no order.
    p.apply(new_level("aaa", [4, 4])).unwrap();
    p.apply(Command::DeleteLevel {
        world: s("main"),
        level: s("aaa"),
        keep_layout: false,
    })
    .unwrap();
    save_dirty(&mut p, &dir).unwrap();
    assert_eq!(common::read_tree(&dir), before);
    // A reorder writes `levels`, and it loads back.
    p.apply(reorder(&["shaft", "crevasse"])).unwrap();
    save_dirty(&mut p, &dir).unwrap();
    let world = fs::read_to_string(dir.join("worlds/main/world.json")).unwrap();
    assert!(
        world.contains(r#""levels": ["shaft", "crevasse"]"#),
        "{world}"
    );
    let q = load(&dir).unwrap();
    assert_eq!(levels(&q), ["shaft", "crevasse"]);
    assert!(validate(&q).is_empty(), "{:?}", validate(&q));
}

#[test]
fn strip_layouts_follow_the_order() {
    let mut p = common::fixture();
    // crevasse 20x5 cells, shaft 6x8, 16 px grid.
    let changes = check(&mut p, layout(LayoutMode::Horizontal, Some(32), None));
    assert_eq!(px(&p, "crevasse"), [0, 0]);
    assert_eq!(px(&p, "shaft"), [320 + 32, 0]);
    assert!(changes.contains(&Change::WorldMeta { world: s("main") }));
    // Switching to a strip wrote the order down.
    assert_eq!(p.worlds[0].level_order, [s("crevasse"), s("shaft")]);
    assert!(validate(&p).is_empty(), "{:?}", validate(&p));

    check(&mut p, reorder(&["shaft", "crevasse"]));
    assert_eq!(px(&p, "shaft"), [0, 0]);
    assert_eq!(px(&p, "crevasse"), [96 + 32, 0]);

    // Create, resize, duplicate and delete relayout, one entry each.
    let changes = check(&mut p, new_level("aaa", [2, 2]));
    assert_eq!(px(&p, "aaa"), [96 + 32 + 320 + 32, 0]);
    assert!(
        changes
            .iter()
            .any(|c| matches!(c, Change::LevelMeta { .. }))
    );
    check(
        &mut p,
        Command::ResizeLevel {
            world: s("main"),
            level: s("shaft"),
            size: [10, 8],
            anchor: [-1, -1],
        },
    );
    assert_eq!(px(&p, "crevasse"), [160 + 32, 0]);
    check(
        &mut p,
        Command::DuplicateLevel {
            level: s("shaft"),
            slug: s("shaft_b"),
            name: None,
            world: None,
            world_px: Some([999, 999]),
        },
    );
    assert_eq!(levels(&p), ["shaft", "crevasse", "aaa", "shaft_b"]);
    assert_eq!(px(&p, "shaft_b"), [160 + 32 + 320 + 32 + 32 + 32, 0]);
    check(
        &mut p,
        Command::DeleteLevel {
            world: s("main"),
            level: s("shaft"),
            keep_layout: false,
        },
    );
    assert_eq!(px(&p, "crevasse"), [0, 0]);
    // Vertical stacks them.
    check(&mut p, layout(LayoutMode::Vertical, None, None));
    assert_eq!(px(&p, "aaa"), [0, 80]);
    // (Deleting shaft left crevasse's door pointing at its door.)
    let probs = validate(&p);
    assert!(
        probs.iter().all(|x| x.path.contains("exit_to")),
        "{probs:?}"
    );

    // A hand-made move in a strip is reported, not refused.
    p.apply(Command::MoveLevel {
        world: s("main"),
        level: s("aaa"),
        world_px: [5, 5],
    })
    .unwrap();
    let probs = validate(&p);
    assert!(
        probs.iter().any(|x| x
            .to_string()
            .contains("vertical layout puts this level at [0, 80]")),
        "{probs:?}"
    );
    // Undo of a later create stays exact even with the level out of place.
    check(&mut p, new_level("bbb", [3, 3]));
}

#[test]
fn moving_a_level_between_strips_relayouts_both() {
    let mut p = common::fixture();
    p.apply(Command::CreateWorld {
        slug: s("dungeon"),
        name: None,
    })
    .unwrap();
    check(&mut p, layout(LayoutMode::Horizontal, None, None));
    check(
        &mut p,
        Command::SetWorldLayout {
            world: s("dungeon"),
            layout: WorldLayout::new(LayoutMode::Vertical),
            restore: false,
        },
    );
    check(
        &mut p,
        Command::MoveLevelToWorld {
            level: s("crevasse"),
            world: s("dungeon"),
            world_px: Some([50, 50]),
            keep_layout: false,
        },
    );
    assert_eq!(px(&p, "crevasse"), [0, 0]);
    assert_eq!(px(&p, "shaft"), [0, 0]);
    assert_eq!(p.world("dungeon").unwrap().level_order, [s("crevasse")]);
    assert_eq!(p.world("main").unwrap().level_order, [s("shaft")]);
}

#[test]
fn grid_vania_reports_levels_off_the_lattice() {
    let mut p = common::fixture();
    check(&mut p, layout(LayoutMode::GridVania, None, Some([320, 80])));
    // Nothing moves; crevasse is 320x80 at the origin, shaft 96x128 at [320, 80].
    assert_eq!(px(&p, "shaft"), [320, 80]);
    let probs: Vec<String> = validate(&p).iter().map(|x| x.to_string()).collect();
    assert_eq!(probs.len(), 1, "{probs:?}");
    assert!(
        probs[0].starts_with("worlds/main/levels/shaft/level.json: size: grid vania:"),
        "{probs:?}"
    );
    p.apply(Command::MoveLevel {
        world: s("main"),
        level: s("crevasse"),
        world_px: [10, 0],
    })
    .unwrap();
    let probs: Vec<String> = validate(&p).iter().map(|x| x.to_string()).collect();
    assert!(
        probs
            .iter()
            .any(|x| x.contains("crevasse/level.json: world_px: grid vania")),
        "{probs:?}"
    );
    // The cell must be whole project cells; 0 is refused.
    p.apply(layout(LayoutMode::GridVania, None, Some([100, 80])))
        .unwrap();
    assert!(
        validate(&p)
            .iter()
            .any(|x| x.path == "layout.cell" && x.file == "worlds/main/world.json")
    );
    assert!(
        p.apply(layout(LayoutMode::GridVania, None, Some([0, 80])))
            .is_err()
    );
    // Snapping a position.
    let l = &p.worlds[0].layout;
    assert_eq!(l.snap(16, [149, 39]), [100, 0]);
}

#[test]
fn layouts_save_and_old_world_files_load_as_free() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("p.worlds");
    let mut p = common::fixture();
    assert!(p.worlds[0].layout.is_default());
    save_all(&mut p, &dir).unwrap();
    let world = fs::read_to_string(dir.join("worlds/main/world.json")).unwrap();
    assert!(!world.contains(r#""layout":"#), "{world}");
    p.apply(layout(LayoutMode::Horizontal, Some(8), None))
        .unwrap();
    save_dirty(&mut p, &dir).unwrap();
    let world = fs::read_to_string(dir.join("worlds/main/world.json")).unwrap();
    assert!(
        world.contains(r#""layout": { "mode": "horizontal", "gap": 8 }"#),
        "{world}"
    );
    let q = load(&dir).unwrap();
    assert_eq!(q.worlds[0].layout.mode, LayoutMode::Horizontal);
    assert_eq!(q, p);
}

#[test]
fn groups_are_set_removed_and_validated() {
    let mut p = common::fixture();
    let g = ObjectGroup::new(
        id("gapgroup22"),
        "Gap",
        vec![id("btncrev222"), id("notecrev22")],
    );
    let changes = check(
        &mut p,
        Command::SetGroup {
            level: s("crevasse"),
            group: g.clone(),
            index: None,
        },
    );
    assert_eq!(
        changes,
        [Change::LevelMeta {
            world: s("main"),
            level: s("crevasse"),
            part: command::LevelPart::Groups,
        }]
    );
    assert!(validate(&p).is_empty(), "{:?}", validate(&p));
    // Replace in place, then remove.
    let mut g2 = g.clone();
    g2.name = "Gap puzzle".into();
    check(
        &mut p,
        Command::SetGroup {
            level: s("crevasse"),
            group: g2,
            index: None,
        },
    );
    assert_eq!(p.level("crevasse").unwrap().groups[0].name, "Gap puzzle");
    assert!(
        p.apply(Command::RemoveGroup {
            level: s("crevasse"),
            id: id("nosuchgrp2"),
        })
        .is_err()
    );

    // Deleting a member: the helper drops it in the same batch.
    let mut cmds = vec![Command::RemoveEntity {
        id: id("btncrev222"),
        keep_empty: false,
    }];
    cmds.extend(groups::drop_members(&p, &[id("btncrev222")]));
    check(
        &mut p,
        Command::Batch {
            label: "Delete".into(),
            commands: cmds,
        },
    );
    assert_eq!(
        p.level("crevasse").unwrap().groups[0].members,
        [id("notecrev22")]
    );
    // (Only the note's link to the button dangles now.)
    let probs = validate(&p);
    assert!(probs.iter().all(|x| x.path.contains("links")), "{probs:?}");
    // Dropping the last member removes the group.
    let cmds = groups::drop_members(&p, &[id("notecrev22")]);
    assert!(matches!(cmds[..], [Command::RemoveGroup { .. }]));

    // Without the helper, validation reports the dangling member.
    p.apply(Command::RemoveNote {
        id: id("notecrev22"),
    })
    .unwrap();
    let probs: Vec<String> = validate(&p).iter().map(|x| x.to_string()).collect();
    assert_eq!(
        probs,
        [
            "worlds/main/levels/crevasse/level.json: groups[gapgroup22].members[0]: no entity or \
          annotation notecrev22 in this level; drop it from the group"
        ],
    );
    check(
        &mut p,
        Command::RemoveGroup {
            level: s("crevasse"),
            id: id("gapgroup22"),
        },
    );
}

#[test]
fn grouping_moves_members_out_of_their_old_group() {
    let mut p = common::fixture();
    let c = groups::group(
        &p,
        &s("crevasse"),
        &[id("btncrev222"), id("doorcrev22")],
        Some("Door".into()),
        Some(id("doorgroup2")),
    )
    .unwrap();
    assert_eq!(c.label(), "Group 2 objects");
    check(&mut p, c);
    let c = groups::group(
        &p,
        &s("crevasse"),
        &[id("doorcrev22"), id("notecrev22")],
        None,
        None,
    )
    .unwrap();
    check(&mut p, c);
    let l = p.level("crevasse").unwrap();
    assert_eq!(l.groups.len(), 2);
    assert_eq!(l.groups[0].members, [id("btncrev222")]);
    assert_eq!(l.groups[1].name, "Group 2");
    assert!(validate(&p).is_empty(), "{:?}", validate(&p));
    // Refused: a member of another level, an empty group.
    assert!(groups::group(&p, &s("crevasse"), &[id("doorshaft2")], None, None).is_err());
    assert!(groups::group(&p, &s("crevasse"), &[], None, None).is_err());
    let c = groups::ungroup(&p, &s("crevasse"), &id("doorgroup2")).unwrap();
    assert_eq!(c.label(), "Ungroup Door");
    check(&mut p, c);
    // Two groups listing one object, by hand: reported.
    let mut q = p.clone();
    let g = q.level("crevasse").unwrap().groups[0].clone();
    q.apply(Command::SetGroup {
        level: s("crevasse"),
        group: ObjectGroup::new(id("othergrp22"), "Other", g.members.clone()),
        index: None,
    })
    .unwrap();
    assert!(
        validate(&q)
            .iter()
            .any(|x| x.message.contains("is also in group"))
    );
}

#[test]
fn duplicating_a_level_copies_its_groups_onto_the_copies() {
    let mut p = common::fixture();
    p.apply(Command::SetGroup {
        level: s("crevasse"),
        group: ObjectGroup::new(
            id("gapgroup22"),
            "Gap",
            vec![id("btncrev222"), id("notecrev22")],
        ),
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
    let copy = p.level("crevasse_b").unwrap();
    assert_eq!(copy.groups[0].id, id("gapgroup22"));
    assert!(!copy.groups[0].members.contains(&id("btncrev222")));
    assert!(validate(&p).is_empty(), "{:?}", validate(&p));
}

#[test]
fn layer_folders_round_trip_and_are_checked() {
    let mut p = common::fixture();
    let mut def = p.layer("decor_back").unwrap().clone();
    def.folder = Some("Decor/Back".into());
    check(
        &mut p,
        Command::SetLayerDef {
            layer: s("decor_back"),
            def: def.clone(),
            index: None,
            content: vec![],
            rules: None,
        },
    );
    let json = serde_json::to_string(&def).unwrap();
    assert!(json.contains(r#""folder":"Decor/Back""#), "{json}");
    assert!(validate(&p).is_empty(), "{:?}", validate(&p));
    def.folder = Some("Decor//Back".into());
    p.apply(Command::SetLayerDef {
        layer: s("decor_back"),
        def,
        index: None,
        content: vec![],
        rules: None,
    })
    .unwrap();
    assert!(validate(&p).iter().any(|x| x.path.ends_with(".folder")));
}

fn rects(r: &[[u32; 4]]) -> Vec<command::Rect> {
    r.iter().map(|&r| command::Rect::from(r)).collect()
}

fn void(level: &str, r: &[[u32; 4]]) -> Command {
    Command::SetLevelVoid {
        level: s(level),
        void: rects(r),
        restore: true,
    }
}

fn gap_group() -> Command {
    Command::SetGroup {
        level: s("crevasse"),
        group: ObjectGroup::new(
            id("gapgroup22"),
            "Gap",
            vec![id("btncrev222"), id("notecrev22")],
        ),
        index: None,
    }
}

#[test]
fn arranging_is_refused_in_strips_and_snaps_in_grid_vania() {
    use worlds_core::transform::{ArrangeOp, arrange_levels};
    let mut p = common::fixture();
    let both = [s("crevasse"), s("shaft")];
    p.apply(layout(LayoutMode::Horizontal, None, None)).unwrap();
    let e = arrange_levels(&p, &both, ArrangeOp::AlignTop).unwrap_err();
    assert!(e.to_string().contains("horizontal layout"), "{e}");
    // Grid vania: align top puts shaft at y 0, and x 330 snaps to 320.
    p.apply(layout(LayoutMode::Free, None, None)).unwrap();
    p.apply(Command::MoveLevel {
        world: s("main"),
        level: s("shaft"),
        world_px: [330, 80],
    })
    .unwrap();
    p.apply(layout(LayoutMode::GridVania, None, Some([64, 64])))
        .unwrap();
    let cmd = arrange_levels(&p, &both, ArrangeOp::AlignTop).unwrap();
    check(&mut p, cmd);
    assert_eq!(px(&p, "shaft"), [320, 0]);
    assert_eq!(px(&p, "crevasse"), [0, 0]);
}

#[test]
fn void_rects_are_held_to_the_lattice_and_strips_use_the_box() {
    let mut p = common::fixture();
    // crevasse 320x80 px: whole 64x16 cells. Void [16, 0, 4, 2] is
    // 256,0 64x32: whole cells; [15, 0, 4, 2] starts at 240: not.
    p.apply(layout(LayoutMode::GridVania, None, Some([64, 16])))
        .unwrap();
    p.apply(void("crevasse", &[[16, 0, 4, 2]])).unwrap();
    let on_void = |p: &Project| {
        validate(p)
            .into_iter()
            .filter(|x| x.path.starts_with("void[") && x.message.contains("grid vania"))
            .map(|x| x.to_string())
            .collect::<Vec<_>>()
    };
    assert!(on_void(&p).is_empty(), "{:?}", on_void(&p));
    p.apply(void("crevasse", &[[15, 0, 4, 2]])).unwrap();
    assert_eq!(
        on_void(&p),
        [
            "worlds/main/levels/crevasse/level.json: void[0]: grid vania, void rect in pixels from \
          the level's corner: [240, 0] is not on the 64x16 px lattice (nearest point [256, 0])"
        ]
    );
    // A strip places by the bounding box: the void changes nothing.
    check(&mut p, layout(LayoutMode::Horizontal, Some(8), None));
    assert_eq!(px(&p, "shaft"), [320 + 8, 0]);
    assert!(on_void(&p).is_empty());
}

#[test]
fn the_void_leaves_groups_alone_and_deleting_in_a_strip_undoes_exactly() {
    let mut p = common::fixture();
    p.apply(gap_group()).unwrap();
    let groups = p.level("crevasse").unwrap().groups.clone();
    check(
        &mut p,
        Command::SetLevelVoid {
            level: s("crevasse"),
            void: rects(&[[16, 0, 4, 2]]),
            restore: false,
        },
    );
    assert_eq!(p.level("crevasse").unwrap().groups, groups);
    // A duplicate of a shaped, grouped level: groups on the copy's ids.
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
    let copy = p.level("crevasse_b").unwrap();
    assert_eq!(copy.void, rects(&[[16, 0, 4, 2]]));
    let own: Vec<&Id> = copy
        .all_entities()
        .map(|e| &e.id)
        .chain(copy.notes.iter().map(|n| &n.id))
        .collect();
    assert_eq!(copy.groups[0].members.len(), 2);
    assert!(copy.groups[0].members.iter().all(|m| own.contains(&m)));
    assert!(validate(&p).is_empty(), "{:?}", validate(&p));
    // Delete the shaped level in a strip: the rest close up, undo puts
    // everything back where it was (`keep_layout` on the inverse).
    check(&mut p, layout(LayoutMode::Horizontal, None, None));
    check(
        &mut p,
        Command::DeleteLevel {
            world: s("main"),
            level: s("crevasse"),
            keep_layout: false,
        },
    );
    assert_eq!(px(&p, "crevasse_b"), [0, 0]);
    assert_eq!(px(&p, "shaft"), [320, 0]);
}

/// An empty project saved in `dir`.
fn empty_project(dir: &std::path::Path) -> Project {
    let mut p = Project::new("Sequel");
    p.grid_size = 16;
    p.seed = 1;
    save(&mut p, dir).unwrap();
    load(dir).unwrap()
}

fn apply_exact(dst: &mut Project, cmd: Command) {
    let before = dst.clone();
    let a = dst.apply(cmd).unwrap();
    let mut undone = dst.clone();
    undone.apply(a.inverse).unwrap();
    assert!(undone == before, "undo of the import isn't exact");
}

#[test]
fn an_imported_world_keeps_its_layout_and_level_order() {
    use worlds_core::merge::{self, Policy, Preset, Selection};
    let tmp = tempfile::tempdir().unwrap();
    let mut src = common::fixture();
    src.apply(layout(LayoutMode::Horizontal, Some(8), None))
        .unwrap();
    src.apply(reorder(&["shaft", "crevasse"])).unwrap();
    let mut dst = empty_project(&tmp.path().join("a.worlds"));
    let plan = merge::plan(
        &src,
        &dst,
        &Selection::preset(Preset::Everything),
        &Policy::default(),
    )
    .unwrap();
    apply_exact(&mut dst, plan.command());
    let w = dst.world_of_level("shaft").unwrap();
    assert_eq!(w.layout, src.worlds[0].layout);
    assert_eq!(w.level_order, [s("shaft"), s("crevasse")]);
    assert_eq!(px(&dst, "shaft"), [0, 0]);
    assert_eq!(px(&dst, "crevasse"), [96 + 8, 0]);
    assert!(validate(&dst).is_empty(), "{:?}", validate(&dst));
}

#[test]
fn imported_levels_join_the_order_strip_and_lattice_with_their_groups() {
    use worlds_core::merge::{self, Conflict, Placement, PlacementMode, Policy, Selection};
    let mut src = common::fixture();
    src.apply(gap_group()).unwrap();
    let sel = Selection {
        levels: vec![s("crevasse")],
        ..Selection::default()
    };
    let into_main = |offset| Policy {
        on_conflict: Conflict::Rename,
        placement: Placement {
            mode: PlacementMode::World,
            world: Some(s("main")),
            offset,
        },
        ..Policy::default()
    };
    // A vertical strip: crevasse_2 goes last in the order and the strip.
    let mut dst = common::fixture();
    dst.apply(layout(LayoutMode::Vertical, None, None)).unwrap();
    let plan = merge::plan(&src, &dst, &sel, &into_main([0, 0])).unwrap();
    apply_exact(&mut dst, plan.command());
    assert_eq!(
        dst.worlds[0].level_order,
        [s("crevasse"), s("shaft"), s("crevasse_2")]
    );
    assert_eq!(px(&dst, "crevasse_2"), [0, 80 + 128]);
    // Every id collided: the group follows the re-rolled ones.
    let new_id = |old: &str| {
        plan.ids
            .iter()
            .find(|c| c.from.as_str() == old)
            .map(|c| c.to.clone())
            .unwrap()
    };
    let l = dst.level("crevasse_2").unwrap();
    assert_eq!(
        l.groups[0].members,
        [new_id("btncrev222"), new_id("notecrev22")]
    );
    assert!(
        validate(&dst).iter().all(|x| !x.path.starts_with("groups")
            && x.path != "world_px"
            && !x.file.ends_with("world.json")),
        "{:?}",
        validate(&dst)
    );
    // Grid vania: the offset position snaps to the lattice.
    let mut dst = common::fixture();
    dst.apply(layout(LayoutMode::GridVania, None, Some([32, 32])))
        .unwrap();
    let plan = merge::plan(&src, &dst, &sel, &into_main([20, 3])).unwrap();
    apply_exact(&mut dst, plan.command());
    assert_eq!(px(&dst, "crevasse_2"), [32, 0]);
}
