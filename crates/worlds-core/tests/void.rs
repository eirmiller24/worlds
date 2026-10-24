//! Non-rectangular levels (`level.json` `void`): the commands, validation,
//! the text view, saving and the export.

mod common;

use serde_json::{Value, json};
use worlds_core::command::{CellValue, Change, Command, GridValue, LevelPart, Rect};
use worlds_core::*;

fn s(x: &str) -> Slug {
    slug(x)
}

fn rects(r: &[[u32; 4]]) -> Vec<Rect> {
    r.iter().map(|&r| Rect::from(r)).collect()
}

fn rows(p: &Project, level: &str, layer: &str) -> Option<Vec<String>> {
    let def = p.layer(layer).unwrap();
    p.level(level)?
        .int_grid(layer)
        .map(|g| g.to_rows(def).unwrap())
}

/// Apply `cmd`; undo, redo and undo again must be exact. Leaves the edit
/// applied and returns its changes.
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
    let c = p.apply(b.inverse).unwrap();
    assert!(*p == after, "redo of {} isn't exact", cmd.label());
    assert_eq!(c.changes, a.changes, "redo emits the same changes");
    p.apply(c.inverse).unwrap();
    assert!(*p == before);
    p.apply(cmd).unwrap();
    a.changes
}

fn set_void(level: &str, r: &[[u32; 4]]) -> Command {
    Command::SetLevelVoid {
        level: s(level),
        void: rects(r),
        restore: false,
    }
}

#[test]
fn set_level_void_clears_what_falls_in_it_and_undo_brings_it_back() {
    let mut p = common::fixture();
    // An L with a notch at the top: the bottom-left corner and a column
    // through the decor tiles go.
    let changes = check(&mut p, set_void("crevasse", &[[0, 3, 4, 2], [2, 0, 1, 2]]));
    let l = p.level("crevasse").unwrap();
    assert_eq!(l.void, rects(&[[0, 3, 4, 2], [2, 0, 1, 2]]));
    let r = rows(&p, "crevasse", "collision").unwrap();
    assert_eq!(&r[3][..5], "....~");
    assert_eq!(&r[4][..5], "....#");
    assert_eq!(&r[0][..3], "##.", "the notch's wall cell is cleared");
    // Every decor tile sat in the column: the layer is gone.
    assert!(l.tiles_layer("decor_back").is_none());
    assert_eq!(
        changes[0],
        Change::LevelMeta {
            world: s("main"),
            level: s("crevasse"),
            part: LevelPart::Void,
        }
    );
    assert!(changes.iter().any(|c| matches!(c, Change::Grid { .. })));
    assert!(changes.iter().any(|c| matches!(c, Change::Tiles { .. })));
    // Back to a rectangle: nothing to clear, content stays as it is.
    check(&mut p, set_void("crevasse", &[]));
    assert!(p.level("crevasse").unwrap().void.is_empty());
    // The same void again is a no-op.
    let a = p.apply(set_void("crevasse", &[])).unwrap();
    assert!(a.changes.is_empty() && a.inverse.is_noop());
}

#[test]
fn set_level_void_refuses_bad_shapes_and_entities() {
    let mut p = common::fixture();
    let err =
        |p: &mut Project, r: &[[u32; 4]]| p.apply(set_void("crevasse", r)).unwrap_err().to_string();
    assert!(err(&mut p, &[[18, 3, 2, 2]]).contains("entity doorcrev22"));
    assert!(err(&mut p, &[[19, 0, 2, 1]]).contains("reaches outside"));
    assert!(err(&mut p, &[[1, 1, 0, 1]]).contains("is empty"));
    assert!(err(&mut p, &[[0, 0, 20, 5]]).contains("whole level"));
    // `restore` takes it as given (what undo uses).
    p.apply(Command::SetLevelVoid {
        level: s("crevasse"),
        void: rects(&[[18, 3, 2, 2]]),
        restore: true,
    })
    .unwrap();
    assert_eq!(p.level("crevasse").unwrap().void, rects(&[[18, 3, 2, 2]]));
}

#[test]
fn fills_skip_the_void_and_entities_are_refused_there() {
    let mut p = common::fixture();
    p.apply(set_void("crevasse", &[[1, 1, 3, 2]])).unwrap();
    // fill_rect over the void fills only level cells.
    check(
        &mut p,
        Command::FillRect {
            level: s("crevasse"),
            layer: s("collision"),
            rect: [0, 1, 6, 1].into(),
            value: GridValue::Value(s("water")),
        },
    );
    assert_eq!(
        &rows(&p, "crevasse", "collision").unwrap()[1][..7],
        "~...~~."
    );
    // A flood doesn't cross the void (row 2's cells 1..4 are void).
    check(
        &mut p,
        Command::FloodFill {
            level: s("crevasse"),
            layer: s("collision"),
            at: [5, 2],
            value: GridValue::Value(s("oneway")),
        },
    );
    let r = rows(&p, "crevasse", "collision").unwrap();
    assert_eq!(&r[2][..6], "#...oo");
    assert_eq!(&r[1][..4], "~...", "the void cells stay empty");
    // Starting in the void fills nothing.
    let a = p
        .apply(Command::FloodFill {
            level: s("crevasse"),
            layer: s("collision"),
            at: [2, 2],
            value: GridValue::Value(s("water")),
        })
        .unwrap();
    assert!(a.changes.is_empty());
    // replace_value from empty leaves the void alone.
    check(
        &mut p,
        Command::ReplaceValue {
            level: s("crevasse"),
            layer: s("collision"),
            from: GridValue::Clear,
            to: GridValue::Value(s("solid")),
        },
    );
    assert_eq!(&rows(&p, "crevasse", "collision").unwrap()[1][..5], "~...~");
    // paint_cells writes what it names (undo needs that); validation then
    // reports it.
    p.apply(Command::PaintCells {
        level: s("crevasse"),
        layer: s("collision"),
        cells: vec![CellValue::new(2, 2, s("solid"))],
        keep_empty: false,
    })
    .unwrap();
    assert!(
        validate(&p)
            .iter()
            .any(|x| x.message.contains("in the level's void hold content")),
        "{:?}",
        validate(&p)
    );
    // Entities: refused in the void, on add and on move.
    let e = EntityInstance {
        id: Id::generate(),
        ty: s("floor_button"),
        cell: [2, 1],
        offset_px: [0, 0],
        size: None,
        fields: Default::default(),
    };
    let err = p
        .apply(Command::AddEntity {
            level: s("crevasse"),
            layer: s("objects"),
            entity: e.clone(),
            index: None,
            allow_outside: false,
        })
        .unwrap_err();
    assert!(err.to_string().contains("void"), "{err}");
    let err = p
        .apply(Command::MoveEntity {
            id: Id::new("btncrev222").unwrap(),
            cell: [3, 2],
            offset_px: [0, 0],
            allow_outside: false,
        })
        .unwrap_err();
    assert!(err.to_string().contains("void"), "{err}");
}

#[test]
fn resize_moves_and_clips_the_void() {
    let mut p = common::fixture();
    p.apply(set_void("crevasse", &[[0, 3, 4, 2], [16, 0, 4, 1]]))
        .unwrap();
    // Grow at the left and top by 2 (anchor right/bottom): rects move with
    // the content.
    p.apply(Command::ResizeLevel {
        world: s("main"),
        level: s("crevasse"),
        size: [22, 7],
        anchor: [1, 1],
    })
    .unwrap();
    assert_eq!(
        p.level("crevasse").unwrap().void,
        rects(&[[2, 5, 4, 2], [18, 2, 4, 1]])
    );
    // Shrink from the bottom-right: the first rect is clipped, the second
    // goes.
    p.apply(Command::ResizeLevel {
        world: s("main"),
        level: s("crevasse"),
        size: [17, 6],
        anchor: [-1, -1],
    })
    .unwrap();
    assert_eq!(p.level("crevasse").unwrap().void, rects(&[[2, 5, 4, 1]]));
    // Undo is exact (the inverse puts the whole level back).
    let before = p.clone();
    let a = p
        .apply(Command::ResizeLevel {
            world: s("main"),
            level: s("crevasse"),
            size: [3, 3],
            anchor: [-1, -1],
        })
        .unwrap();
    assert!(p.level("crevasse").unwrap().void.is_empty());
    p.apply(a.inverse).unwrap();
    assert!(p == before);
}

#[test]
fn create_level_with_a_void_drops_content_there() {
    let mut p = common::fixture();
    let mut l = Level::new(s("ell"), "Ell", [4, 3]);
    l.void = rects(&[[2, 0, 2, 2]]);
    let content: command::LevelContent = serde_json::from_value(json!({
        "int_grids": [ { "layer": "collision", "rows": ["####", "#..#", "####"] } ]
    }))
    .unwrap();
    check(
        &mut p,
        Command::CreateLevel {
            world: s("main"),
            level: l.clone(),
            content: content.clone(),
            restore: false,
        },
    );
    assert_eq!(
        rows(&p, "ell", "collision").unwrap(),
        ["##..", "#...", "####"]
    );
    // A shape that doesn't fit is refused.
    let mut bad = l.clone();
    bad.slug = s("bad");
    bad.id = Id::generate();
    bad.void = rects(&[[3, 0, 2, 1]]);
    assert!(
        p.apply(Command::CreateLevel {
            world: s("main"),
            level: bad,
            content,
            restore: false,
        })
        .unwrap_err()
        .to_string()
        .contains("outside")
    );
}

#[test]
fn validation_reports_bad_shapes() {
    let mut p = common::fixture();
    let l = &mut p.worlds[0].levels[0];
    assert_eq!(l.slug.as_str(), "crevasse");
    // Splits the level in two, reaches outside, and holds the door.
    l.void = rects(&[[8, 0, 2, 5], [18, 3, 3, 1]]);
    let problems: Vec<String> = validate(&p).iter().map(|x| x.to_string()).collect();
    let has = |needle: &str| problems.iter().any(|x| x.contains(needle));
    assert!(has("splits the level into 2 pieces"), "{problems:?}");
    assert!(
        has("void[1]: [18, 3, 3, 1] reaches outside"),
        "{problems:?}"
    );
    assert!(
        has("entities[doorcrev22].cell: [18, 3] is in the level's void"),
        "{problems:?}"
    );
    assert!(has("collision.json: rows[0]"), "{problems:?}");
    let l = &mut p.worlds[0].levels[0];
    l.void = rects(&[[0, 0, 20, 5]]);
    assert!(
        validate(&p)
            .iter()
            .any(|x| x.message.contains("no cell is left"))
    );
}

#[test]
fn the_void_saves_loads_and_shows_in_the_text_view() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("p.worlds");
    let mut p = common::fixture();
    p.apply(set_void("crevasse", &[[0, 3, 4, 2]])).unwrap();
    save_all(&mut p, &path).unwrap();
    let text =
        std::fs::read_to_string(path.join("worlds/main/levels/crevasse/level.json")).unwrap();
    assert!(text.contains("\"void\": [[0, 3, 4, 2]]"), "{text}");
    let back = load(&path).unwrap();
    assert_eq!(back.level("crevasse").unwrap().void, rects(&[[0, 3, 4, 2]]));
    // A level without void keeps its file as it was.
    let shaft = std::fs::read_to_string(path.join("worlds/main/levels/shaft/level.json")).unwrap();
    assert!(!shaft.contains("void"));

    let t = text::render_level(&back, "crevasse", Some(&["collision"])).unwrap();
    assert!(
        t.contains("depth 0, bg #202020, void [[0, 3, 4, 2]]\n"),
        "{t}"
    );
    assert!(
        t.contains("legend: . empty, # solid, f fortified, o oneway, ~ water, L ramp_l, * void"),
        "{t}"
    );
    assert!(
        t.contains("3 ****~~~.......ff####\n4 ****################\n"),
        "{t}"
    );
}

fn exported(p: &Project) -> (Value, Value) {
    let world: Value = serde_json::from_str(&export::world_json(p)).unwrap();
    let l = p.level("crevasse").unwrap();
    let level: Value = serde_json::from_str(
        &export::level_json(p, l, &export::BakedLevel::default(), "..").unwrap(),
    )
    .unwrap();
    (world, level)
}

#[test]
fn the_export_carries_void_outline_and_seams_along_it() {
    let mut p = common::fixture();
    p.collision_layer = Some(s("collision"));
    // A rectangular level exports no void or outline, and no `at`.
    let (world, level) = exported(&p);
    assert!(level.get("void").is_none() && level.get("outline").is_none());
    let entry = &world["worlds"][0]["levels"][0];
    assert!(entry.get("outline").is_none());

    // Crevasse (20x5 at [0, 0], 16 px) loses its top-right corner: 4x2.
    // Shaft (6x8) moves into the notch, touching the notch's floor and the
    // wall to its left.
    p.apply(set_void("crevasse", &[[16, 0, 4, 2]])).unwrap();
    p.apply(Command::MoveLevel {
        world: s("main"),
        level: s("shaft"),
        world_px: [256, -96],
    })
    .unwrap();
    let (world, level) = exported(&p);
    assert_eq!(level["void"], json!([[16, 0, 4, 2]]));
    let outline = json!([[0, 0], [256, 0], [256, 32], [320, 32], [320, 80], [0, 80]]);
    assert_eq!(level["outline"], outline);
    let entry = &world["worlds"][0]["levels"][0];
    assert_eq!(entry["slug"], "crevasse");
    assert_eq!(entry["outline"], outline);
    assert_eq!(entry["void"], json!([[16, 0, 4, 2]]));
    // Shaft spans x 256..352, y -96..32: crevasse's notch floor (y 32, x
    // 256..320) and its wall at x 256 (y 0..32) both touch it.
    assert_eq!(
        entry["touching"],
        json!([
            { "level": "shaft", "edge": "right", "span": [0, 32], "at": 256 },
            { "level": "shaft", "edge": "top", "span": [256, 320], "at": 32 }
        ])
    );
    let shaft = &world["worlds"][0]["levels"][1];
    assert_eq!(
        shaft["touching"],
        json!([
            { "level": "crevasse", "edge": "left", "span": [0, 32], "at": 256 },
            { "level": "crevasse", "edge": "bottom", "span": [256, 320], "at": 32 }
        ])
    );
    // Open edges follow the outline: `[first, end, at]` in collision cells.
    // The box's top row is solid; the notch floor is row 2's cells 16..19
    // ("...#": the top edge at y 2 is open over 16..19); the notch's wall is
    // the right edge (x 16) of column 15, open in row 1.
    let edges = &entry["open_edges"];
    assert_eq!(
        *edges,
        json!({ "left": [], "right": [[1, 2, 16]], "top": [[16, 19, 2]], "bottom": [] })
    );
}

#[test]
fn a_centred_resize_moves_the_void_by_half() {
    let mut p = common::fixture();
    p.apply(set_void("crevasse", &[[16, 0, 4, 1]])).unwrap();
    // 20x5 to 24x9 about the centre: everything moves by [2, 2].
    undo_exact(
        &mut p,
        Command::ResizeLevel {
            world: s("main"),
            level: s("crevasse"),
            size: [24, 9],
            anchor: [0, 0],
        },
    );
    assert_eq!(p.level("crevasse").unwrap().void, rects(&[[18, 2, 4, 1]]));
}

/// Apply `cmd`; its undo must be exact. Leaves it applied. (Resizes undo
/// by `replace_level`, so their redo reports differently than `check` wants.)
fn undo_exact(p: &mut Project, cmd: Command) {
    let before = p.clone();
    let a = p.apply(cmd.clone()).unwrap();
    p.apply(a.inverse).unwrap();
    assert!(*p == before, "undo of {} isn't exact", cmd.label());
    p.apply(cmd).unwrap();
}

/// A level `size` big holding `rows` of collision and the `void` as given
/// (`restore`: content in the void is kept).
fn shaped_level(p: &mut Project, slug: &str, size: [u32; 2], void: &[[u32; 4]], rows: &[&str]) {
    let mut l = Level::new(s(slug), slug, size);
    l.world_px = [0, 400];
    l.void = rects(void);
    let content: command::LevelContent = serde_json::from_value(json!({
        "int_grids": [ { "layer": "collision", "rows": rows } ]
    }))
    .unwrap();
    p.apply(Command::CreateLevel {
        world: s("main"),
        level: l,
        content,
        restore: true,
    })
    .unwrap();
}

#[test]
fn crop_moves_and_clips_the_void_and_refuses_only_void() {
    let mut p = common::fixture();
    // Content in [2, 1] to [6, 5); a rect inside it and one outside it.
    shaped_level(
        &mut p,
        "ell",
        [8, 6],
        &[[4, 1, 2, 2], [0, 4, 2, 2]],
        &[
            "........", "..##....", "..##....", "..####..", "..####..", "........",
        ],
    );
    let crop = p.crop_level(&s("main"), &s("ell")).unwrap().unwrap();
    undo_exact(&mut p, crop);
    let l = p.level("ell").unwrap();
    assert_eq!(l.size, [4, 4]);
    assert_eq!(l.void, rects(&[[2, 0, 2, 2]]));
    assert_eq!(
        rows(&p, "ell", "collision").unwrap(),
        ["##..", "##..", "####", "####"]
    );
    // Content only in the void (a file put it there): cropping to it would
    // leave nothing but void.
    shaped_level(
        &mut p,
        "odd",
        [6, 4],
        &[[2, 1, 2, 2]],
        &["......", "..##..", "..##..", "......"],
    );
    let err = p.crop_level(&s("main"), &s("odd")).unwrap_err().to_string();
    assert!(err.contains("only void"), "{err}");
}

#[test]
fn duplicate_and_move_to_another_world_keep_the_void() {
    let mut p = common::fixture();
    p.apply(set_void("crevasse", &[[16, 0, 4, 1]])).unwrap();
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
    assert_eq!(copy.void, rects(&[[16, 0, 4, 1]]));
    assert_eq!(
        rows(&p, "crevasse_b", "collision"),
        rows(&p, "crevasse", "collision")
    );
    p.apply(Command::CreateWorld {
        slug: s("caves"),
        name: None,
    })
    .unwrap();
    check(
        &mut p,
        Command::MoveLevelToWorld {
            level: s("crevasse_b"),
            world: s("caves"),
            world_px: Some([0, 0]),
        },
    );
    let w = p.world("caves").unwrap();
    assert_eq!(w.levels[0].void, rects(&[[16, 0, 4, 1]]));
}

#[test]
fn paste_drops_cells_tiles_and_entities_in_the_void() {
    use std::collections::BTreeMap;
    use worlds_core::fragment::{self, CopySelection, Fragment, PasteOptions};
    let mut p = common::fixture();
    p.apply(set_void("crevasse", &[[16, 0, 4, 2]])).unwrap();
    let at = |origin| PasteOptions {
        level: s("crevasse"),
        origin,
        layer_map: BTreeMap::new(),
    };
    // Two 8 px decor stacks, pasted onto project cell [16, 0]: both void.
    let tiles = fragment::copy(
        &p,
        &CopySelection {
            level: Some(s("crevasse")),
            tiles: [(s("decor_back"), vec![[4, 2], [5, 2]])].into(),
            ..Default::default()
        },
    )
    .unwrap();
    let plan = fragment::paste(&p, &tiles, &at([16, 0])).unwrap();
    assert!(plan.commands.is_empty(), "{:?}", plan.commands);
    assert_eq!(
        plan.problems,
        ["2 cells fell in the void of level crevasse (cut out of its shape) and were dropped"]
    );
    // Four wall cells across the void's edge and a button in it.
    let frag: Fragment = serde_json::from_value(json!({
        "worlds_fragment": 1, "grid_size": 16, "size": [4, 1],
        "grids": { "collision": { "rows": ["oooo"] } },
        "entities": [ { "id": "aaaaaaaaaa", "type": "floor_button", "cell": [3, 0] } ]
    }))
    .unwrap();
    let plan = fragment::paste(&p, &frag, &at([14, 1])).unwrap();
    let msgs = plan.problems.join("\n");
    assert!(msgs.contains("2 cells fell in the void"), "{msgs}");
    assert!(
        msgs.contains(
            "entity aaaaaaaaaa (floor_button) would land in the void of level crevasse at [17, 1]"
        ),
        "{msgs}"
    );
    assert!(plan.entities.is_empty());
    let before = rows(&p, "crevasse", "collision").unwrap()[1].clone();
    check(&mut p, plan.command());
    let r = &rows(&p, "crevasse", "collision").unwrap()[1];
    assert_eq!(&r[14..16], "oo");
    assert_eq!(&r[16..], &before[16..], "the void stays empty");
    assert!(validate(&p).iter().all(|x| !x.message.contains("void")));
}
