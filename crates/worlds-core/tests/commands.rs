//! Commands: exact inverses for every variant, batches, history, dirty
//! tracking and the incremental save.

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use serde_json::json;
use worlds_core::command::{
    CellValue, Change, Command, CommandError, GridValue, History, LevelContent, LevelPart,
    MacroKind, NoteScope, Origin, ProjectKey, ProjectSetting, Rect,
};
use worlds_core::format::IntGridFile;
use worlds_core::*;

fn s(x: &str) -> Slug {
    slug(x)
}

fn id(x: &str) -> Id {
    Id::new(x).unwrap()
}

fn cell(x: u32, y: u32, v: Option<&str>) -> CellValue {
    CellValue::new(x, y, v.map(slug))
}

fn tile(x: u32, y: u32, ts: &str, t: [u32; 2]) -> TilePlacement {
    TilePlacement {
        cell: [x, y],
        tileset: s(ts),
        tile: t,
        flip: None,
        alpha: 1.0,
    }
}

fn rows(p: &Project, level: &str, layer: &str) -> Option<Vec<String>> {
    let def = p.layer(layer).unwrap();
    p.level(level)?
        .int_grid(layer)
        .map(|g| g.to_rows(def).unwrap())
}

/// Apply `cmd`, then check: the inverse restores the project exactly, the
/// inverse of the inverse (redo) gets back to the edited state, and every
/// command involved survives a JSON round trip. Leaves the edit applied and
/// returns its changes.
fn check(p: &mut Project, cmd: Command) -> Vec<Change> {
    let json = serde_json::to_string(&cmd).unwrap();
    let back: Command = serde_json::from_str(&json).unwrap_or_else(|e| panic!("{e}: {json}"));
    assert_eq!(back, cmd, "command JSON round trip: {json}");
    let before = p.clone();
    let a = p
        .apply(cmd.clone())
        .unwrap_or_else(|e| panic!("{}: {e}", cmd.label()));
    let after = p.clone();
    let inv_json = serde_json::to_string(&a.inverse).unwrap();
    let inverse: Command = serde_json::from_str(&inv_json).unwrap();
    assert_eq!(inverse, a.inverse, "inverse JSON round trip: {inv_json}");
    let b = p.apply(inverse).unwrap();
    assert!(*p == before, "undo of {} isn't exact", cmd.label());
    let c = p.apply(b.inverse).unwrap();
    assert!(*p == after, "redo of {} isn't exact", cmd.label());
    // Redo emits the same changes, except that redoing a resize is a
    // `replace_level`, which reports the whole level: it must still cover
    // every file the original touched.
    let files = |cs: &[Change]| -> std::collections::BTreeSet<PathBuf> {
        cs.iter().flat_map(|c| c.files()).collect()
    };
    if matches!(cmd, Command::ResizeLevel { .. }) {
        assert!(files(&c.changes).is_superset(&files(&a.changes)));
    } else {
        assert_eq!(c.changes, a.changes, "redo emits the same changes");
    }
    p.apply(c.inverse).unwrap();
    assert!(*p == before, "second undo of {}", cmd.label());
    let again = p.apply(cmd).unwrap();
    assert!(*p == after);
    again.changes
}

fn grid_change(level: &str, layer: &str, r: [u32; 4]) -> Change {
    Change::Grid {
        world: s("main"),
        level: s(level),
        layer: s(layer),
        rect: r.into(),
    }
}

#[test]
fn paint_cells_dedupes_and_reports_the_changed_rect() {
    let mut p = common::fixture();
    let changes = check(
        &mut p,
        Command::PaintCells {
            level: s("crevasse"),
            layer: s("collision"),
            cells: vec![
                cell(3, 1, Some("water")),
                cell(3, 1, Some("solid")), // last one wins
                cell(8, 2, None),
                cell(5, 2, None),          // already empty: not a change
                cell(0, 0, Some("solid")), // already solid: not a change
            ],
            keep_empty: false,
        },
    );
    assert_eq!(
        changes,
        [grid_change("crevasse", "collision", [3, 1, 6, 2])]
    );
    let r = rows(&p, "crevasse", "collision").unwrap();
    assert_eq!(&r[1][..6], "#..#..");
    assert_eq!(&r[2][..10], "#........o");
    // A command that changes nothing is a no-op.
    let a = p
        .apply(Command::PaintCells {
            level: s("crevasse"),
            layer: s("collision"),
            cells: vec![cell(3, 1, Some("solid"))],
            keep_empty: false,
        })
        .unwrap();
    assert!(a.changes.is_empty() && a.inverse.is_noop());
}

#[test]
fn paint_on_a_level_without_content_creates_and_undo_removes_it() {
    let mut p = common::fixture();
    assert!(p.level("shaft").unwrap().int_grid("collision").is_none());
    let before = p.clone();
    let a = p
        .apply(Command::PaintCells {
            level: s("shaft"),
            layer: s("collision"),
            cells: vec![cell(1, 1, Some("solid"))],
            keep_empty: false,
        })
        .unwrap();
    assert_eq!(rows(&p, "shaft", "collision").unwrap()[1], ".#....");
    p.apply(a.inverse).unwrap();
    assert!(p.level("shaft").unwrap().int_grid("collision").is_none());
    assert_eq!(p, before);
}

#[test]
fn an_existing_empty_grid_survives_undo() {
    let mut p = common::fixture();
    let def = p.layer("collision").unwrap().clone();
    p.worlds[0].levels[1]
        .int_grids
        .push(IntGridLayer::new_empty(&def, [6, 8]));
    check(
        &mut p,
        Command::PaintCells {
            level: s("shaft"),
            layer: s("collision"),
            cells: vec![cell(2, 2, Some("water"))],
            keep_empty: false,
        },
    );
    // Painting it back to empty without keep_empty drops the content.
    p.apply(Command::PaintCells {
        level: s("shaft"),
        layer: s("collision"),
        cells: vec![cell(2, 2, None)],
        keep_empty: false,
    })
    .unwrap();
    assert!(p.level("shaft").unwrap().int_grid("collision").is_none());
}

#[test]
fn fill_rect_replace_value_and_clearing_a_whole_grid() {
    let mut p = common::fixture();
    let changes = check(
        &mut p,
        Command::FillRect {
            level: s("crevasse"),
            layer: s("collision"),
            rect: Rect::new(2, 1, 3, 2),
            value: Some(s("water")).into(),
        },
    );
    assert_eq!(
        changes,
        [grid_change("crevasse", "collision", [2, 1, 3, 2])]
    );
    check(
        &mut p,
        Command::ReplaceValue {
            level: s("crevasse"),
            layer: s("collision"),
            from: Some(s("water")).into(),
            to: Some(s("fortified")).into(),
        },
    );
    let r = rows(&p, "crevasse", "collision").unwrap();
    assert_eq!(r[1], "#.fff..............#");
    assert_eq!(r[3], "#L..fff.......ff####");
    // Emptying everything drops the grid; undo brings it back.
    check(
        &mut p,
        Command::FillRect {
            level: s("crevasse"),
            layer: s("collision"),
            rect: Rect::new(0, 0, 20, 5),
            value: GridValue::Clear,
        },
    );
    assert!(p.level("crevasse").unwrap().int_grid("collision").is_none());
}

#[test]
fn flood_fill_is_4_connected_and_stops_at_other_values() {
    let mut p = common::fixture();
    // Interior of crevasse: the empty region is bounded by walls; the oneway
    // strip and the water pool are other values.
    let changes = check(
        &mut p,
        Command::FloodFill {
            level: s("crevasse"),
            layer: s("collision"),
            at: [1, 1],
            value: Some(s("water")).into(),
        },
    );
    let r = rows(&p, "crevasse", "collision").unwrap();
    assert_eq!(
        r,
        [
            "####################",
            "#~~~~~~~~~~~~~~~~~~#",
            "#~~~~~~~oooo~~~~~~~#",
            "#L~~~~~~~~~~~~ff####",
            "####################",
        ]
    );
    assert_eq!(
        changes,
        [grid_change("crevasse", "collision", [1, 1, 18, 3])]
    );

    // Diagonal neighbours don't connect.
    let mut p = Project::new("t");
    p.layers = common::fixture().layers;
    let def = p.layer("collision").unwrap().clone();
    let mut l = Level::new(s("room"), "Room", [3, 3]);
    l.int_grids
        .push(IntGridLayer::from_rows(&def, &["#..", ".#.", "..#"]).unwrap());
    p.worlds[0].levels.push(l);
    check(
        &mut p,
        Command::FloodFill {
            level: s("room"),
            layer: s("collision"),
            at: [0, 0],
            value: Some(s("water")).into(),
        },
    );
    assert_eq!(
        rows(&p, "room", "collision").unwrap(),
        ["~..", ".#.", "..#"]
    );
    // Filling a cell with its own value changes nothing.
    let a = p
        .apply(Command::FloodFill {
            level: s("room"),
            layer: s("collision"),
            at: [2, 0],
            value: GridValue::Clear,
        })
        .unwrap();
    assert!(a.changes.is_empty());
    // The top-right empty region reaches the bottom-left one? No: the
    // diagonal blocks it, so filling it leaves the other region alone.
    check(
        &mut p,
        Command::FloodFill {
            level: s("room"),
            layer: s("collision"),
            at: [2, 0],
            value: Some(s("solid")).into(),
        },
    );
    assert_eq!(
        rows(&p, "room", "collision").unwrap(),
        ["~##", ".##", "..#"]
    );
}

#[test]
fn grid_errors_change_nothing() {
    let mut p = common::fixture();
    let before = p.clone();
    let errs = [
        Command::PaintCells {
            level: s("crevasse"),
            layer: s("collision"),
            cells: vec![cell(1, 1, Some("solid")), cell(20, 0, Some("solid"))],
            keep_empty: false,
        },
        Command::PaintCells {
            level: s("crevasse"),
            layer: s("collision"),
            cells: vec![cell(1, 1, Some("lava"))],
            keep_empty: false,
        },
        Command::PaintCells {
            level: s("crevasse"),
            layer: s("decor_back"),
            cells: vec![],
            keep_empty: false,
        },
        Command::FillRect {
            level: s("crevasse"),
            layer: s("collision"),
            rect: Rect::new(18, 0, 3, 1),
            value: GridValue::Clear,
        },
        Command::FloodFill {
            level: s("nowhere"),
            layer: s("collision"),
            at: [0, 0],
            value: GridValue::Clear,
        },
    ];
    for c in errs {
        let e = p.apply(c).unwrap_err();
        assert!(!e.to_string().is_empty());
        assert_eq!(p, before);
        assert!(!p.is_dirty());
    }
}

#[test]
fn set_tiles_replaces_stacks_and_clears() {
    let mut p = common::fixture();
    let changes = check(
        &mut p,
        Command::SetTiles {
            level: s("crevasse"),
            layer: s("decor_back"),
            tiles: vec![
                tile(4, 2, "castle_main", [1, 1]), // replaces [5, 1]
                tile(9, 0, "castle_main", [2, 2]), // new cell, two tiles stacked
                tile(9, 0, "ssw_set5", [0, 0]),
            ],
            clear: vec![[5, 2], [30, 9]],
            keep_empty: false,
        },
    );
    assert_eq!(
        changes,
        [Change::Tiles {
            world: s("main"),
            level: s("crevasse"),
            layer: s("decor_back"),
            // [30, 9] had nothing to clear, so it isn't a change.
            rect: Rect::new(4, 0, 6, 3),
        }]
    );
    let t: Vec<([u32; 2], [u32; 2])> = p
        .level("crevasse")
        .unwrap()
        .tiles_layer("decor_back")
        .unwrap()
        .tiles
        .iter()
        .map(|t| (t.cell, t.tile))
        .collect();
    assert_eq!(
        t,
        [
            ([9, 0], [2, 2]),
            ([9, 0], [0, 0]),
            ([4, 1], [0, 3]),
            ([4, 2], [1, 1])
        ]
    );
    // On a level with no tiles content, then cleared again: no empty file left.
    check(
        &mut p,
        Command::SetTiles {
            level: s("shaft"),
            layer: s("decor_back"),
            tiles: vec![tile(0, 0, "castle_main", [0, 0])],
            clear: vec![],
            keep_empty: false,
        },
    );
    check(
        &mut p,
        Command::ClearTiles {
            level: s("shaft"),
            layer: s("decor_back"),
            rect: Rect::new(0, 0, 12, 16),
        },
    );
    assert!(
        p.level("shaft")
            .unwrap()
            .tiles_layer("decor_back")
            .is_none()
    );
    // An existing empty tiles layer survives an add and its undo.
    p.worlds[0].levels[1].tiles.push(TilesLayer {
        layer: s("decor_back"),
        tiles: vec![],
    });
    check(
        &mut p,
        Command::SetTiles {
            level: s("shaft"),
            layer: s("decor_back"),
            tiles: vec![tile(1, 1, "castle_main", [0, 0])],
            clear: vec![],
            keep_empty: false,
        },
    );
    let err = p
        .apply(Command::SetTiles {
            level: s("shaft"),
            layer: s("decor_back"),
            tiles: vec![tile(1, 1, "nope", [0, 0])],
            clear: vec![],
            keep_empty: false,
        })
        .unwrap_err();
    assert_eq!(err, CommandError::UnknownTileset(s("nope")));
}

#[test]
fn clear_tiles_rect() {
    let mut p = common::fixture();
    check(
        &mut p,
        Command::ClearTiles {
            level: s("crevasse"),
            layer: s("decor_back"),
            rect: Rect::new(4, 2, 2, 1),
        },
    );
    let left = &p
        .level("crevasse")
        .unwrap()
        .tiles_layer("decor_back")
        .unwrap()
        .tiles;
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].cell, [4, 1]);
}

fn new_level(slug_: &str, size: [u32; 2]) -> Level {
    let mut l = Level::new(s(slug_), "New", size);
    l.id = id("newlevel22");
    l
}

#[test]
fn create_delete_replace_level() {
    let mut p = common::fixture();
    let content = LevelContent {
        int_grids: vec![IntGridFile {
            overrides: None,
            layer: s("collision"),
            legend: None,
            rows: vec!["###".into(), "#.~".into()],
        }],
        tiles: vec![TilesLayer {
            layer: s("decor_back"),
            tiles: vec![tile(5, 3, "castle_main", [0, 0])],
        }],
        entities: vec![EntitiesLayer {
            layer: s("objects"),
            entities: vec![EntityInstance {
                id: id("newdoor222"),
                ty: s("locked_door"),
                cell: [1, 1],
                offset_px: [0, 0],
                size: None,
                fields: BTreeMap::new(),
            }],
        }],
        notes: vec![Annotation::new(Shape::Note { px: [3, 3] })],
    };
    let changes = check(
        &mut p,
        Command::CreateLevel {
            world: s("main"),
            level: new_level("annex", [3, 2]),
            content: content.clone(),
            restore: false,
        },
    );
    assert_eq!(
        changes,
        [Change::LevelCreated {
            world: s("main"),
            level: s("annex")
        }]
    );
    let slugs: Vec<&str> = p.worlds[0].levels.iter().map(|l| l.slug.as_str()).collect();
    assert_eq!(slugs, ["annex", "crevasse", "shaft"]);

    // Duplicates are refused.
    let mut dup = new_level("annex", [3, 2]);
    dup.id = id("otherid222");
    assert_eq!(
        p.apply(Command::CreateLevel {
            world: s("main"),
            level: dup,
            content: LevelContent::default(),
            restore: false,
        })
        .unwrap_err(),
        CommandError::DuplicateLevel(s("annex"))
    );
    let mut dup = new_level("annex2", [3, 2]);
    dup.id = id("otherid222");
    let mut c2 = content.clone();
    c2.int_grids.clear();
    c2.tiles.clear();
    c2.notes.clear();
    assert_eq!(
        p.apply(Command::CreateLevel {
            world: s("main"),
            level: dup,
            content: c2,
            restore: false,
        })
        .unwrap_err(),
        CommandError::DuplicateId(id("newdoor222"))
    );

    check(
        &mut p,
        Command::DeleteLevel {
            world: s("main"),
            level: s("crevasse"),
        },
    );
    assert!(p.level("crevasse").is_none());

    let mut meta = new_level("annex", [3, 2]);
    meta.name = "Annex, replaced".into();
    check(
        &mut p,
        Command::ReplaceLevel {
            world: s("main"),
            level: meta,
            content: LevelContent {
                int_grids: vec![IntGridFile {
                    overrides: None,
                    layer: s("collision"),
                    legend: None,
                    rows: vec!["...".into(), "..#".into()],
                }],
                ..Default::default()
            },
            restore: false,
        },
    );
    assert_eq!(rows(&p, "annex", "collision").unwrap(), ["...", "..#"]);
}

#[test]
fn level_metadata_commands() {
    let mut p = common::fixture();
    let w = || s("main");
    let c = check(
        &mut p,
        Command::MoveLevel {
            world: w(),
            level: s("shaft"),
            world_px: [-16, 32],
        },
    );
    assert_eq!(
        c,
        [Change::LevelMeta {
            world: w(),
            level: s("shaft"),
            part: LevelPart::Position
        }]
    );
    check(
        &mut p,
        Command::SetLevelDepth {
            world: w(),
            level: s("shaft"),
            depth: -3,
        },
    );
    check(
        &mut p,
        Command::SetLevelBg {
            world: w(),
            level: s("crevasse"),
            bg_color: None,
        },
    );
    check(
        &mut p,
        Command::SetLevelField {
            world: w(),
            level: s("crevasse"),
            field: s("music"),
            value: Some(json!("cave")),
            allow_unknown: false,
        },
    );
    // Removing a value vs storing null are different, and both invert.
    check(
        &mut p,
        Command::SetLevelField {
            world: w(),
            level: s("crevasse"),
            field: s("biome"),
            value: None,
            allow_unknown: false,
        },
    );
    check(
        &mut p,
        Command::SetLevelField {
            world: w(),
            level: s("shaft"),
            field: s("music"),
            value: Some(serde_json::Value::Null),
            allow_unknown: false,
        },
    );
    assert_eq!(
        p.level("shaft").unwrap().fields.get("music"),
        Some(&serde_json::Value::Null)
    );
    let json =
        r#"{"op":"set_level_field","world":"main","level":"shaft","field":"music","value":null}"#;
    let parsed: Command = serde_json::from_str(json).unwrap();
    assert!(matches!(
        parsed,
        Command::SetLevelField {
            value: Some(serde_json::Value::Null),
            ..
        }
    ));
    assert!(matches!(
        p.apply(Command::SetLevelField {
            world: w(),
            level: s("shaft"),
            field: s("nope"),
            value: Some(json!(1)),
            allow_unknown: false,
        }),
        Err(CommandError::UnknownField { .. })
    ));
    assert!(matches!(
        p.apply(Command::MoveLevel {
            world: s("other"),
            level: s("shaft"),
            world_px: [0, 0],
        }),
        Err(CommandError::UnknownWorld(_))
    ));
}

#[test]
fn rename_level_rewrites_note_links() {
    let mut p = common::fixture();
    let changes = check(
        &mut p,
        Command::RenameLevel {
            world: s("main"),
            level: s("crevasse"),
            slug: s("chasm"),
            name: "Chasm".into(),
        },
    );
    assert_eq!(
        changes,
        [
            Change::LevelRenamed {
                world: s("main"),
                from: s("crevasse"),
                to: s("chasm")
            },
            Change::Note {
                world: s("main"),
                level: None,
                id: id("pinstatue2")
            },
            Change::LevelMeta {
                world: s("main"),
                level: s("chasm"),
                part: LevelPart::Name
            },
        ]
    );
    assert_eq!(p.worlds[0].notes[1].links, ["level:chasm"]);
    assert_eq!(
        changes[0].files(),
        [
            PathBuf::from("worlds/main/levels/crevasse"),
            PathBuf::from("worlds/main/levels/chasm")
        ]
    );
    assert_eq!(
        p.apply(Command::RenameLevel {
            world: s("main"),
            level: s("chasm"),
            slug: s("shaft"),
            name: "x".into(),
        })
        .unwrap_err(),
        CommandError::DuplicateLevel(s("shaft"))
    );
}

/// A 4x3 level with a recognisable grid, a tile, an entity and a note.
fn resize_fixture() -> Project {
    let mut p = common::fixture();
    let def = p.layer("collision").unwrap().clone();
    let mut l = new_level("box", [4, 3]);
    l.world_px = [160, 160];
    l.int_grids
        .push(IntGridLayer::from_rows(&def, &["#~..", "..o.", "...f"]).unwrap());
    l.tiles.push(TilesLayer {
        layer: s("decor_back"),
        tiles: vec![tile(7, 5, "castle_main", [1, 1])],
    });
    l.entities.push(EntitiesLayer {
        layer: s("objects"),
        entities: vec![EntityInstance {
            id: id("boxdoor222"),
            ty: s("locked_door"),
            cell: [3, 2],
            offset_px: [0, 0],
            size: None,
            fields: BTreeMap::new(),
        }],
    });
    let mut n = Annotation::new(Shape::Note { px: [8, 8] });
    n.id = id("boxnote222");
    l.notes.push(n);
    p.worlds[0].levels.push(l);
    p.normalize();
    p
}

#[test]
fn resize_with_every_anchor() {
    // Grow 4x3 -> 6x5 (2 more cells each way) and shrink 4x3 -> 3x2.
    for ax in -1i8..=1 {
        for ay in -1i8..=1 {
            for (size, label) in [([6, 5], "grow"), ([3, 2], "shrink")] {
                let mut p = resize_fixture();
                let changes = check(
                    &mut p,
                    Command::ResizeLevel {
                        world: s("main"),
                        level: s("box"),
                        size,
                        anchor: [ax, ay],
                    },
                );
                let shift = |a: i8, d: i64| match a {
                    -1 => 0,
                    0 => d.div_euclid(2),
                    _ => d,
                };
                let d = [size[0] as i64 - 4, size[1] as i64 - 3];
                let (sx, sy) = (shift(ax, d[0]), shift(ay, d[1]));
                let l = p.level("box").unwrap();
                let ctx = format!("{label} anchor [{ax}, {ay}]");
                assert_eq!(l.size, size, "{ctx}");
                // Content stays put in the world.
                assert_eq!(l.world_px, [160 - sx * 16, 160 - sy * 16], "{ctx}");
                let def = p.layer("collision").unwrap();
                let g = l.int_grid("collision").unwrap();
                assert_eq!(g.size(), size, "{ctx}");
                for (y, row) in ["#~..", "..o.", "...f"].iter().enumerate() {
                    for (x, ch) in row.chars().enumerate() {
                        let (nx, ny) = (x as i64 + sx, y as i64 + sy);
                        if nx < 0 || ny < 0 || nx >= size[0] as i64 || ny >= size[1] as i64 {
                            continue;
                        }
                        let got = g
                            .value_at(def, nx as u32, ny as u32)
                            .map_or('.', |v| def.value(v.as_str()).unwrap().glyph);
                        assert_eq!(got, ch, "{ctx} cell [{x}, {y}]");
                    }
                }
                // The entity moves with the content and is kept even outside.
                let e = l.all_entities().next().unwrap();
                assert_eq!(e.cell, [3 + sx as i32, 2 + sy as i32], "{ctx}");
                let outside = e.cell[0] >= size[0] as i32 || e.cell[1] >= size[1] as i32;
                assert_eq!(
                    changes
                        .iter()
                        .any(|c| matches!(c, Change::EntityOutside { .. })),
                    outside,
                    "{ctx}"
                );
                // decor_back is on an 8 px grid: twice the shift, in its cells.
                let tiles = l.tiles_layer("decor_back").map_or(vec![], |t| {
                    t.tiles.iter().map(|t| t.cell).collect::<Vec<_>>()
                });
                let (tx, ty) = (7 + 2 * sx, 5 + 2 * sy);
                let inside =
                    tx >= 0 && ty >= 0 && tx < 2 * size[0] as i64 && ty < 2 * size[1] as i64;
                if inside {
                    assert_eq!(tiles, [[tx as u32, ty as u32]], "{ctx}");
                } else {
                    assert!(tiles.is_empty(), "{ctx}");
                }
                match &l.notes[0].shape {
                    Shape::Note { px } => assert_eq!(*px, [8 + sx * 16, 8 + sy * 16], "{ctx}"),
                    _ => unreachable!(),
                }
            }
        }
    }
}

/// `box` shrunk to 3x2 from the top-left: its door at [3, 2] is now outside.
fn stranded() -> Project {
    let mut p = resize_fixture();
    let a = p
        .apply(Command::ResizeLevel {
            world: s("main"),
            level: s("box"),
            size: [3, 2],
            anchor: [-1, -1],
        })
        .unwrap();
    assert!(
        a.changes
            .iter()
            .any(|c| matches!(c, Change::EntityOutside { .. }))
    );
    p
}

#[test]
fn an_entity_left_outside_by_a_resize_can_be_removed_moved_and_undone() {
    let mut p = stranded();
    // Remove: the inverse puts it back outside.
    check(
        &mut p,
        Command::RemoveEntity {
            id: id("boxdoor222"),
            keep_empty: false,
        },
    );
    // Move it inside: undo moves it back outside.
    let mut p = stranded();
    check(
        &mut p,
        Command::MoveEntity {
            id: id("boxdoor222"),
            cell: [1, 1],
            offset_px: [0, 0],
            allow_outside: false,
        },
    );
    // Through the history too: resize, remove, undo, undo, redo, redo.
    let mut p = resize_fixture();
    let before = p.clone();
    let mut h = History::new();
    h.push(
        &mut p,
        Command::ResizeLevel {
            world: s("main"),
            level: s("box"),
            size: [3, 2],
            anchor: [-1, -1],
        },
    )
    .unwrap();
    let shrunk = p.clone();
    h.push(
        &mut p,
        Command::RemoveEntity {
            id: id("boxdoor222"),
            keep_empty: false,
        },
    )
    .unwrap();
    let removed = p.clone();
    h.undo(&mut p).unwrap().unwrap();
    assert_eq!(p, shrunk);
    h.undo(&mut p).unwrap().unwrap();
    assert_eq!(p, before);
    h.redo(&mut p).unwrap().unwrap();
    h.redo(&mut p).unwrap().unwrap();
    assert_eq!(p, removed);
    // A user edit still can't put an entity outside.
    let mut p = stranded();
    assert!(matches!(
        p.apply(Command::MoveEntity {
            id: id("boxdoor222"),
            cell: [5, 5],
            offset_px: [0, 0],
            allow_outside: false,
        }),
        Err(CommandError::OutOfBounds { .. })
    ));
}

#[test]
fn a_failing_batch_rolls_back_a_stranded_entity_edit() {
    for first in [
        Command::RemoveEntity {
            id: id("boxdoor222"),
            keep_empty: false,
        },
        Command::MoveEntity {
            id: id("boxdoor222"),
            cell: [0, 0],
            offset_px: [2, 0],
            allow_outside: false,
        },
    ] {
        let mut p = stranded();
        let before = p.clone();
        let dirty = p.files.dirty().clone();
        let err = p
            .apply(Command::Batch {
                label: "half".into(),
                commands: vec![first, paint("box", 99, 0, Some("solid"))],
            })
            .unwrap_err();
        assert!(matches!(err, CommandError::Batch { index: 1, .. }), "{err}");
        assert_eq!(p, before);
        assert_eq!(*p.files.dirty(), dirty);
    }
}

#[test]
fn resize_refuses_bad_input() {
    let mut p = resize_fixture();
    for (size, anchor) in [([0, 3], [0, 0]), ([5, 5], [2, 0])] {
        assert!(matches!(
            p.apply(Command::ResizeLevel {
                world: s("main"),
                level: s("box"),
                size,
                anchor,
            }),
            Err(CommandError::Invalid(_))
        ));
    }
}

#[test]
fn resize_leaves_outside_predicts_entity_outside() {
    for ax in -1i8..=1 {
        for ay in -1i8..=1 {
            for size in [[6, 5], [3, 2], [4, 1], [1, 3]] {
                let mut p = resize_fixture();
                let predicted = p
                    .resize_leaves_outside(&s("main"), &s("box"), size, [ax, ay])
                    .unwrap();
                let a = p
                    .apply(Command::ResizeLevel {
                        world: s("main"),
                        level: s("box"),
                        size,
                        anchor: [ax, ay],
                    })
                    .unwrap();
                let reported: Vec<Id> = a
                    .changes
                    .iter()
                    .filter_map(|c| match c {
                        Change::EntityOutside { id, .. } => Some(id.clone()),
                        _ => None,
                    })
                    .collect();
                assert_eq!(predicted, reported, "{size:?} anchor [{ax}, {ay}]");
            }
        }
    }
    let p = resize_fixture();
    assert!(matches!(
        p.resize_leaves_outside(&s("main"), &s("nope"), [1, 1], [0, 0]),
        Err(CommandError::UnknownLevel(_))
    ));
}

#[test]
fn crop_level_undoes_a_grow_at_every_anchor() {
    let original = resize_fixture();
    // Already tight: nothing to do.
    assert_eq!(original.crop_level(&s("main"), &s("box")).unwrap(), None);
    for ax in -1i8..=1 {
        for ay in -1i8..=1 {
            let ctx = format!("anchor [{ax}, {ay}]");
            let mut p = original.clone();
            p.apply(Command::ResizeLevel {
                world: s("main"),
                level: s("box"),
                size: [11, 8],
                anchor: [ax, ay],
            })
            .unwrap();
            let grown = p.clone();
            let l = p.level("box").unwrap();
            let bounds = p.content_bounds(l).unwrap().unwrap();
            assert_eq!([bounds[2], bounds[3]], [4, 3], "{ctx}");
            let crop = p.crop_level(&s("main"), &s("box")).unwrap().unwrap();
            assert_eq!(crop.label(), "Crop level box", "{ctx}");
            let mut h = History::new();
            h.push(&mut p, crop).unwrap();
            // Back to the original level exactly: size, position, content.
            assert_eq!(p.level("box"), original.level("box"), "{ctx}");
            assert_eq!(p.crop_level(&s("main"), &s("box")).unwrap(), None);
            // One undo entry puts the grown level back.
            h.undo(&mut p).unwrap().unwrap();
            assert!(p == grown, "{ctx}: undo");
            h.redo(&mut p).unwrap().unwrap();
            assert_eq!(p.level("box"), original.level("box"), "{ctx}: redo");
        }
    }
}

#[test]
fn crop_level_keeps_layers_on_whole_cells_and_refuses_an_empty_level() {
    let mut p = common::fixture();
    p.worlds[0].levels.push(new_level("empty", [5, 5]));
    p.normalize();
    assert!(matches!(
        p.crop_level(&s("main"), &s("empty")),
        Err(CommandError::Invalid(_))
    ));
    // One door on a 32 px entities layer (the project grid is 16 px) at
    // cell [2, 1]: its cell covers project cells x 4..6, y 2..4, its box
    // (1x3 project cells, pivot bottom center) x 4.5..5.5, y 1..4. A crop
    // at the top by 1 project cell would move the layer half a cell, so the
    // top stays.
    let i = p.layers.iter().position(|l| l.slug == "objects").unwrap();
    p.layers[i].grid_size = Some(32);
    let mut l = new_level("wide", [10, 8]);
    l.entities.push(EntitiesLayer {
        layer: s("objects"),
        entities: vec![EntityInstance {
            id: id("widedoor22"),
            ty: s("locked_door"),
            cell: [2, 1],
            offset_px: [0, 0],
            size: None,
            fields: BTreeMap::new(),
        }],
    });
    p.worlds[0].levels.push(l);
    p.normalize();
    let bounds = p.content_bounds(p.level("wide").unwrap()).unwrap();
    assert_eq!(bounds, Some([4, 0, 2, 4]));
    let crop = p.crop_level(&s("main"), &s("wide")).unwrap().unwrap();
    p.apply(crop).unwrap();
    let l = p.level("wide").unwrap();
    assert_eq!(l.size, [2, 4]);
    assert_eq!(l.world_px, [64, 0]);
    assert_eq!(l.all_entities().next().unwrap().cell, [0, 1]);
}

#[test]
fn entity_commands() {
    let mut p = common::fixture();
    let door = || EntityInstance {
        id: id("newdoor222"),
        ty: s("locked_door"),
        cell: [2, 2],
        offset_px: [0, 0],
        size: None,
        fields: BTreeMap::from([(s("channel"), json!(4))]),
    };
    // Into an existing layer, at a position, and into a level without one.
    check(
        &mut p,
        Command::AddEntity {
            level: s("crevasse"),
            layer: s("objects"),
            entity: door(),
            index: Some(1),
            allow_outside: false,
        },
    );
    let ids: Vec<&str> = p
        .level("crevasse")
        .unwrap()
        .all_entities()
        .map(|e| e.id.as_str())
        .collect();
    assert_eq!(ids, ["doorcrev22", "newdoor222", "btncrev222"]);
    check(
        &mut p,
        Command::RemoveEntity {
            id: id("doorshaft2"),
            keep_empty: false,
        },
    );
    assert!(p.level("shaft").unwrap().entities.is_empty());
    let mut e = door();
    e.id = id("shaftdoor2");
    check(
        &mut p,
        Command::AddEntity {
            level: s("shaft"),
            layer: s("objects"),
            entity: e,
            index: None,
            allow_outside: false,
        },
    );
    check(
        &mut p,
        Command::MoveEntity {
            id: id("btncrev222"),
            cell: [5, 1],
            offset_px: [3, 0],
            allow_outside: false,
        },
    );
    check(
        &mut p,
        Command::ResizeEntity {
            id: id("doorcrev22"),
            size: None,
        },
    );
    check(
        &mut p,
        Command::SetEntityField {
            id: id("doorcrev22"),
            field: s("needs_item"),
            value: Some(json!("silver_key")),
            allow_unknown: false,
        },
    );
    check(
        &mut p,
        Command::SetEntityField {
            id: id("doorcrev22"),
            field: s("speed"),
            value: None,
            allow_unknown: false,
        },
    );
    // Errors.
    let before = p.clone();
    let bad = [
        Command::AddEntity {
            level: s("crevasse"),
            layer: s("objects"),
            entity: door(),
            index: None,
            allow_outside: false,
        },
        Command::AddEntity {
            level: s("crevasse"),
            layer: s("collision"),
            entity: EntityInstance {
                id: id("zzzzzzzzzz"),
                ..door()
            },
            index: None,
            allow_outside: false,
        },
        Command::MoveEntity {
            id: id("btncrev222"),
            cell: [-1, 0],
            offset_px: [0, 0],
            allow_outside: false,
        },
        Command::SetEntityField {
            id: id("btncrev222"),
            field: s("needs_item"),
            value: Some(json!(1)),
            allow_unknown: false,
        },
        Command::RemoveEntity {
            id: id("zzzzzzzzzz"),
            keep_empty: false,
        },
    ];
    for c in bad {
        assert!(p.apply(c).is_err());
        assert_eq!(p, before);
    }
}

#[test]
fn an_existing_empty_entities_layer_survives_undo() {
    let mut p = common::fixture();
    p.worlds[0].levels[1].entities[0].entities.clear();
    let mut e = p
        .level("crevasse")
        .unwrap()
        .all_entities()
        .next()
        .unwrap()
        .clone();
    e.id = id("xxxxxxxxxx");
    e.cell = [1, 1];
    check(
        &mut p,
        Command::AddEntity {
            level: s("shaft"),
            layer: s("objects"),
            entity: e,
            index: None,
            allow_outside: false,
        },
    );
}

#[test]
fn note_commands() {
    let mut p = common::fixture();
    let mut n = Annotation::new(Shape::Pin {
        px: [1, 2],
        icon: None,
        label: Some("x".into()),
    });
    n.id = id("newpin2222");
    let c = check(
        &mut p,
        Command::AddNote {
            scope: NoteScope::World(s("main")),
            note: n.clone(),
            index: Some(0),
        },
    );
    assert_eq!(c[0].file(), PathBuf::from("worlds/main/world.json"));
    assert_eq!(p.worlds[0].notes[0].id, n.id);
    let mut n2 = n.clone();
    n2.id = id("newnote222");
    let c = check(
        &mut p,
        Command::AddNote {
            scope: NoteScope::Level(s("shaft")),
            note: n2,
            index: None,
        },
    );
    assert_eq!(
        c[0].file(),
        PathBuf::from("worlds/main/levels/shaft/notes.json")
    );
    check(
        &mut p,
        Command::RemoveNote {
            id: id("notecrev22"),
        },
    );
    let mut upd = p.worlds[0].notes[2].clone();
    upd.status = NoteStatus::Done;
    upd.body = Some("moved".into());
    check(
        &mut p,
        Command::UpdateNote {
            id: upd.id.clone(),
            note: upd,
        },
    );
    assert!(matches!(
        p.apply(Command::AddNote {
            scope: NoteScope::Level(s("shaft")),
            note: n,
            index: None,
        }),
        Err(CommandError::DuplicateId(_))
    ));
}

/// The `guidestem2` guide of the fixture, set into `group`.
fn group_the_guide(p: &Project, group: Option<&str>) -> Command {
    let mut g = p.worlds[0]
        .notes
        .iter()
        .find(|n| n.id == id("guidestem2"))
        .unwrap()
        .clone();
    if let Shape::Guide { group: gr, .. } = &mut g.shape {
        *gr = group.map(s);
    }
    Command::UpdateNote {
        id: g.id.clone(),
        note: g,
    }
}

#[test]
fn level_tags_and_guide_groups() {
    let (tmp, mut p) = on_disk();
    let start = common::read_tree(tmp.path());
    let fixture = p.clone();
    // set_level_tags: level.json part `tags`.
    let c = check(
        &mut p,
        Command::SetLevelTags {
            world: s("main"),
            level: s("crevasse"),
            tags: vec!["forest".into(), "ruined".into()],
        },
    );
    assert_eq!(
        c,
        [Change::LevelMeta {
            world: s("main"),
            level: s("crevasse"),
            part: LevelPart::Tags,
        }]
    );
    assert_eq!(
        p.apply(Command::SetLevelTags {
            world: s("main"),
            level: s("crevasse"),
            tags: vec!["forest".into(), "ruined".into()],
        })
        .unwrap()
        .inverse,
        Command::noop()
    );
    assert!(matches!(
        p.apply(Command::SetLevelTags {
            world: s("main"),
            level: s("nowhere"),
            tags: vec![],
        }),
        Err(CommandError::UnknownLevel(_))
    ));
    assert!(matches!(
        p.apply(Command::SetLevelTags {
            world: s("other"),
            level: s("crevasse"),
            tags: vec![],
        }),
        Err(CommandError::UnknownWorld(_))
    ));

    // set_guide_group: new, then replace in place, then move.
    let ruined = GuideGroup {
        name: Some("Ruined wing".into()),
        offset: [0, 769],
        color: Some(Color::new("#c08040").unwrap()),
        ..GuideGroup::new(s("ruined"))
    };
    let c = check(
        &mut p,
        Command::SetGuideGroup {
            world: s("main"),
            group: ruined.clone(),
            index: None,
        },
    );
    assert_eq!(
        c,
        [Change::GuideGroup {
            world: s("main"),
            slug: s("ruined"),
        }]
    );
    assert_eq!(c[0].file(), PathBuf::from("worlds/main/world.json"));
    assert_eq!(c[0].level(), None);
    check(
        &mut p,
        Command::SetGuideGroup {
            world: s("main"),
            group: GuideGroup::new(s("spare")),
            index: Some(0),
        },
    );
    assert_eq!(p.worlds[0].guide_groups[0].slug, s("spare"));
    check(
        &mut p,
        Command::SetGuideGroup {
            world: s("main"),
            group: GuideGroup {
                offset: [0, 1],
                ..ruined.clone()
            },
            index: Some(0),
        },
    );
    assert_eq!(p.worlds[0].guide_groups[0].offset, [0, 1]);
    // Same value at the same place: a no-op.
    let a = p
        .apply(Command::SetGuideGroup {
            world: s("main"),
            group: p.worlds[0].guide_groups[1].clone(),
            index: None,
        })
        .unwrap();
    assert!(a.inverse.is_noop() && a.changes.is_empty());
    for index in [2, 3] {
        assert!(matches!(
            p.apply(Command::SetGuideGroup {
                world: s("main"),
                group: GuideGroup::new(s("spare")),
                index: Some(index),
            }),
            Err(CommandError::Invalid(_))
        ));
    }
    assert!(matches!(
        p.apply(Command::SetGuideGroup {
            world: s("main"),
            group: GuideGroup::new(s("third")),
            index: Some(3),
        }),
        Err(CommandError::Invalid(_))
    ));

    // remove_guide_group: refused while a guide names it.
    let c = group_the_guide(&p, Some("ruined"));
    check(&mut p, c);
    let e = p
        .apply(Command::RemoveGuideGroup {
            world: s("main"),
            slug: s("ruined"),
        })
        .unwrap_err();
    assert_eq!(
        e.to_string(),
        "guide group ruined is used by 1 guide (guidestem2); move them out first"
    );
    let c = group_the_guide(&p, None);
    check(&mut p, c);
    check(
        &mut p,
        Command::RemoveGuideGroup {
            world: s("main"),
            slug: s("ruined"),
        },
    );
    let e = p
        .apply(Command::RemoveGuideGroup {
            world: s("main"),
            slug: s("ruined"),
        })
        .unwrap_err();
    assert_eq!(
        e,
        CommandError::UnknownGuideGroup {
            world: s("main"),
            group: s("ruined"),
        }
    );
    assert_eq!(
        serde_json::to_value(&e).unwrap()["kind"],
        "unknown_guide_group"
    );
    // Labels.
    assert_eq!(
        Command::SetLevelTags {
            world: s("main"),
            level: s("crevasse"),
            tags: vec![]
        }
        .label(),
        "Tag level crevasse"
    );
    assert_eq!(
        Command::RemoveGuideGroup {
            world: s("main"),
            slug: s("ruined")
        }
        .label(),
        "Remove guide group ruined"
    );

    // Saved, the files say so; undone back to the start, the bytes match.
    save_dirty(&mut p, tmp.path()).unwrap();
    let world = fs::read_to_string(tmp.path().join("worlds/main/world.json")).unwrap();
    assert!(
        world.contains("\"name\": \"Main\",\n  \"guide_groups\": [\n    { \"slug\": \"spare\" }\n  ],\n  \"notes\""),
        "{world}"
    );
    let level =
        fs::read_to_string(tmp.path().join("worlds/main/levels/crevasse/level.json")).unwrap();
    assert!(
        level.contains(
            "\"bg_color\": \"#202020\",\n  \"tags\": [\"forest\", \"ruined\"],\n  \"fields\""
        ),
        "{level}"
    );
    assert_eq!(load(tmp.path()).unwrap(), p);
    for c in [
        Command::RemoveGuideGroup {
            world: s("main"),
            slug: s("spare"),
        },
        Command::SetLevelTags {
            world: s("main"),
            level: s("crevasse"),
            tags: vec![],
        },
    ] {
        p.apply(c).unwrap();
    }
    assert_eq!(p, fixture);
    save_dirty(&mut p, tmp.path()).unwrap();
    assert_eq!(common::read_tree(tmp.path()), start);
}

#[test]
fn layer_state_and_project_settings() {
    let mut p = common::fixture();
    check(
        &mut p,
        Command::SetLayerState {
            level: s("crevasse"),
            layer: s("main_tiling"),
            state: None,
        },
    );
    check(
        &mut p,
        Command::SetLayerState {
            level: s("crevasse"),
            layer: s("main_tiling"),
            state: Some(LayerState {
                seed: 9,
                ..Default::default()
            }),
        },
    );
    assert!(matches!(
        p.apply(Command::SetLayerState {
            level: s("crevasse"),
            layer: s("collision"),
            state: None,
        }),
        Err(CommandError::WrongLayerKind { .. })
    ));
    for setting in [
        ProjectSetting::Name("Renamed".into()),
        ProjectSetting::BgColor(None),
        ProjectSetting::BgColor(Some(Color::new("#ffffff").unwrap())),
        ProjectSetting::CollisionLayer(None),
        ProjectSetting::Seed(7),
        ProjectSetting::BlockingValues(Some(vec![s("solid")])),
        ProjectSetting::BlockingValues(Some(vec![])),
        ProjectSetting::BlockingValues(None),
        ProjectSetting::DefaultLevelSize(Some([40, 22])),
        ProjectSetting::DefaultLevelSize(None),
        ProjectSetting::ExportOnSave(true),
        ProjectSetting::ExportOnSave(false),
    ] {
        check(&mut p, Command::SetProjectSetting { setting });
    }
    // Each reports its own key, and the inverse puts the old value back.
    let a = p
        .apply(Command::SetProjectSetting {
            setting: ProjectSetting::DefaultLevelSize(Some([12, 8])),
        })
        .unwrap();
    assert_eq!(
        a.changes,
        vec![Change::Project {
            key: ProjectKey::DefaultLevelSize
        }]
    );
    assert_eq!(p.default_level_size, Some([12, 8]));
    p.apply(a.inverse).unwrap();
    assert_eq!(p.default_level_size, None);
    let a = p
        .apply(Command::SetProjectSetting {
            setting: ProjectSetting::ExportOnSave(true),
        })
        .unwrap();
    assert_eq!(
        a.changes,
        vec![Change::Project {
            key: ProjectKey::ExportOnSave
        }]
    );
    p.apply(a.inverse).unwrap();
    assert!(!p.export_on_save);
    // A zero-sized default level is refused.
    for bad in [[0, 10], [10, 0]] {
        let e = p
            .apply(Command::SetProjectSetting {
                setting: ProjectSetting::DefaultLevelSize(Some(bad)),
            })
            .unwrap_err();
        assert!(e.to_string().contains("at least 1x1"), "{e}");
    }
    // The grid can't change under levels; the same value is a no-op.
    let e = p
        .apply(Command::SetProjectSetting {
            setting: ProjectSetting::GridSize(32),
        })
        .unwrap_err();
    assert!(e.to_string().contains("no levels"), "{e}");
    let same = p
        .apply(Command::SetProjectSetting {
            setting: ProjectSetting::GridSize(p.grid_size),
        })
        .unwrap();
    assert!(same.changes.is_empty());
    let mut empty = Project::new("empty");
    check(
        &mut empty,
        Command::SetProjectSetting {
            setting: ProjectSetting::GridSize(32),
        },
    );
    assert!(
        empty
            .apply(Command::SetProjectSetting {
                setting: ProjectSetting::GridSize(0),
            })
            .is_err()
    );
    assert!(
        p.apply(Command::SetProjectSetting {
            setting: ProjectSetting::CollisionLayer(Some(s("decor_back")))
        })
        .is_err()
    );
    let json = r#"{"op":"set_project_setting","setting":{"key":"seed","value":42}}"#;
    let c: Command = serde_json::from_str(json).unwrap();
    assert_eq!(
        c,
        Command::SetProjectSetting {
            setting: ProjectSetting::Seed(42)
        }
    );
}

fn paint(level: &str, x: u32, y: u32, v: Option<&str>) -> Command {
    Command::PaintCells {
        level: s(level),
        layer: s("collision"),
        cells: vec![cell(x, y, v)],
        keep_empty: false,
    }
}

#[test]
fn batch_is_atomic_and_inverts_in_reverse() {
    let mut p = common::fixture();
    let changes = check(
        &mut p,
        Command::Batch {
            label: "stroke".into(),
            commands: vec![
                paint("crevasse", 1, 1, Some("solid")),
                paint("crevasse", 2, 1, Some("solid")),
                paint("crevasse", 1, 1, Some("water")),
                paint("crevasse", 3, 1, Some("solid")),
            ],
        },
    );
    // Grid changes on one layer coalesce into their bounding rect.
    assert_eq!(
        changes,
        [grid_change("crevasse", "collision", [1, 1, 3, 1])]
    );
    assert_eq!(&rows(&p, "crevasse", "collision").unwrap()[1][..5], "#~##.");

    let before = p.clone();
    let err = p
        .apply(Command::Batch {
            label: "half".into(),
            commands: vec![
                paint("crevasse", 5, 1, Some("solid")),
                Command::DeleteLevel {
                    world: s("main"),
                    level: s("shaft"),
                },
                paint("crevasse", 99, 1, Some("solid")),
                paint("crevasse", 6, 1, Some("solid")),
            ],
        })
        .unwrap_err();
    match err {
        CommandError::Batch { index, label, .. } => {
            assert_eq!((index, label.as_str()), (2, "half"))
        }
        e => panic!("{e}"),
    }
    assert_eq!(p, before);
    assert!(p.level("shaft").is_some());

    // Nested batches and an empty batch.
    check(
        &mut p,
        Command::Batch {
            label: String::new(),
            commands: vec![
                Command::Batch {
                    label: "inner".into(),
                    commands: vec![paint("shaft", 0, 0, Some("solid"))],
                },
                Command::noop(),
                Command::MoveLevel {
                    world: s("main"),
                    level: s("shaft"),
                    world_px: [0, 0],
                },
            ],
        },
    );
}

/// Every command kind once, each valid on the fixture after the ones
/// before it.
fn one_of_each() -> Vec<Command> {
    let mut n = Annotation::new(Shape::Note { px: [0, 0] });
    n.id = id("histnote22");
    let mut upd = n.clone();
    upd.title = Some("t".into());
    vec![
        paint("crevasse", 1, 1, Some("solid")),
        Command::FillRect {
            level: s("crevasse"),
            layer: s("collision"),
            rect: Rect::new(1, 1, 2, 2),
            value: Some(s("water")).into(),
        },
        Command::FloodFill {
            level: s("crevasse"),
            layer: s("collision"),
            at: [5, 1],
            value: Some(s("fortified")).into(),
        },
        Command::ReplaceValue {
            level: s("crevasse"),
            layer: s("collision"),
            from: Some(s("water")).into(),
            to: GridValue::Clear,
        },
        Command::SetTiles {
            level: s("crevasse"),
            layer: s("decor_back"),
            tiles: vec![tile(0, 0, "castle_main", [1, 0])],
            clear: vec![[4, 1]],
            keep_empty: false,
        },
        Command::ClearTiles {
            level: s("crevasse"),
            layer: s("decor_back"),
            rect: Rect::new(0, 0, 40, 10),
        },
        Command::CreateLevel {
            world: s("main"),
            level: new_level("annex", [2, 2]),
            content: LevelContent::default(),
            restore: false,
        },
        Command::ReplaceLevel {
            world: s("main"),
            level: {
                let mut l = new_level("annex", [3, 2]);
                l.depth = 4;
                l
            },
            content: LevelContent::default(),
            restore: false,
        },
        Command::MoveLevel {
            world: s("main"),
            level: s("annex"),
            world_px: [640, 0],
        },
        Command::ResizeLevel {
            world: s("main"),
            level: s("crevasse"),
            size: [22, 6],
            anchor: [0, 1],
        },
        Command::RenameLevel {
            world: s("main"),
            level: s("annex"),
            slug: s("wing"),
            name: "Wing".into(),
        },
        Command::SetLevelField {
            world: s("main"),
            level: s("wing"),
            field: s("music"),
            value: Some(json!("x")),
            allow_unknown: false,
        },
        Command::SetLevelDepth {
            world: s("main"),
            level: s("wing"),
            depth: 2,
        },
        Command::SetLevelBg {
            world: s("main"),
            level: s("wing"),
            bg_color: Some(Color::new("#000000").unwrap()),
        },
        Command::SetLevelTags {
            world: s("main"),
            level: s("wing"),
            tags: vec!["forest".into()],
        },
        Command::AddEntity {
            level: s("wing"),
            layer: s("objects"),
            entity: EntityInstance {
                id: id("histdoor22"),
                ty: s("floor_button"),
                cell: [0, 0],
                offset_px: [0, 0],
                size: None,
                fields: BTreeMap::new(),
            },
            index: None,
            allow_outside: false,
        },
        Command::MoveEntity {
            id: id("histdoor22"),
            cell: [1, 1],
            offset_px: [0, 0],
            allow_outside: false,
        },
        Command::ResizeEntity {
            id: id("histdoor22"),
            size: Some([2, 1]),
        },
        Command::SetEntityField {
            id: id("histdoor22"),
            field: s("channel"),
            value: Some(json!(2)),
            allow_unknown: false,
        },
        Command::RemoveEntity {
            id: id("btncrev222"),
            keep_empty: false,
        },
        Command::AddNote {
            scope: NoteScope::Level(s("wing")),
            note: n,
            index: None,
        },
        Command::UpdateNote {
            id: id("histnote22"),
            note: upd,
        },
        Command::RemoveNote {
            id: id("pathcrev22"),
        },
        Command::SetGuideGroup {
            world: s("main"),
            group: GuideGroup::new(s("spare")),
            index: None,
        },
        Command::SetGuideGroup {
            world: s("main"),
            group: GuideGroup {
                offset: [0, 32],
                ..GuideGroup::new(s("ruined"))
            },
            index: Some(0),
        },
        Command::RemoveGuideGroup {
            world: s("main"),
            slug: s("spare"),
        },
        Command::SetLayerState {
            level: s("wing"),
            layer: s("main_tiling"),
            state: Some(LayerState::default()),
        },
        Command::SetProjectSetting {
            setting: ProjectSetting::Seed(1),
        },
        Command::DeleteLevel {
            world: s("main"),
            level: s("shaft"),
        },
        Command::SetLayerDef {
            layer: s("hazards"),
            def: serde_json::from_value(json!({
                "slug": "hazards", "name": "Hazards", "kind": "int_grid",
                "values": [{ "slug": "spikes", "name": "Spikes", "color": "#c03030", "glyph": "^" }]
            }))
            .unwrap(),
            index: Some(1),
            content: Vec::new(),
            rules: None,
        },
        Command::RenameLayer {
            from: s("hazards"),
            to: s("traps"),
        },
        Command::SetEnumDef {
            enum_: s("weather"),
            def: serde_json::from_value(
                json!({ "slug": "weather", "values": [{ "slug": "rain" }] }),
            )
            .unwrap(),
            index: None,
        },
        Command::RenameEnum {
            from: s("facing"),
            to: s("direction"),
        },
        Command::RenameEnumValue {
            enum_: s("biome"),
            from: s("flooded"),
            to: s("wet"),
        },
        Command::RemoveEnumDef {
            enum_: s("weather"),
        },
        Command::SetTilesetDef {
            tileset: s("cave"),
            def: serde_json::from_value(
                json!({ "slug": "cave", "image": "art/cave.png", "tile_size": [8, 8] }),
            )
            .unwrap(),
            index: None,
        },
        Command::RenameTileset {
            from: s("castle_main"),
            to: s("castle"),
        },
        Command::RemoveTilesetDef {
            tileset: s("cave"),
            keep_uses: false,
        },
        Command::SetRuleGroup {
            layer: s("main_tiling"),
            group: s("vines"),
            def: RuleGroup::new(s("vines"), Vec::new()),
            index: Some(0),
        },
        Command::RemoveRuleGroup {
            layer: s("main_tiling"),
            group: s("retired"),
            keep_empty: false,
        },
        Command::SetEntityDef {
            entity: s("lever"),
            def: EntityDef {
                slug: s("lever"),
                ..common::fixture()
                    .entity_def("floor_button")
                    .unwrap()
                    .clone()
            },
        },
        Command::RenameEntityType {
            from: s("locked_door"),
            to: s("door"),
        },
        Command::SetEntityFieldDef {
            entity: s("door"),
            field: s("key_color"),
            def: serde_json::from_value(json!({ "slug": "key_color", "type": "color" })).unwrap(),
            index: Some(0),
        },
        Command::RenameEntityField {
            entity: s("door"),
            from: s("needs_item"),
            to: s("key_item"),
        },
        Command::RemoveEntityFieldDef {
            entity: s("door"),
            field: s("key_color"),
            keep_values: false,
        },
        Command::SetLevelFieldDef {
            field: s("ambience"),
            def: serde_json::from_value(json!({ "slug": "ambience", "type": "float" })).unwrap(),
            index: None,
        },
        Command::RenameLevelField {
            from: s("music"),
            to: s("soundtrack"),
        },
        Command::RemoveLevelFieldDef {
            field: s("ambience"),
            keep_values: false,
        },
        // floor_button: histdoor22 in wing goes with it.
        Command::RemoveEntityDef {
            entity: s("floor_button"),
            keep_instances: false,
        },
        Command::SetMacro {
            kind: MacroKind::Template,
            name: s("ledge"),
            def: serde_json::from_value(json!({ "legend": "walls", "rules": [
                { "slug": "top", "match": { "rows": "edge_top" }, "out": { "tiles": [[1, 0]] } }
            ] }))
            .unwrap(),
        },
        Command::RenameMacro {
            kind: MacroKind::Pattern,
            from: s("edge_top"),
            to: s("top_edge"),
        },
        Command::SetRule {
            layer: s("main_tiling"),
            group: s("walls"),
            rule: s("ledges"),
            def: serde_json::from_value(
                json!({ "use": "ledge", "slug": "ledges", "tileset": "castle" }),
            )
            .unwrap(),
            index: Some(1),
        },
        Command::MoveRule {
            layer: s("main_tiling"),
            rule: s("wall_fill"),
            group: s("pillars"),
            index: 0,
        },
        Command::RemoveRule {
            layer: s("main_tiling"),
            rule: s("ledges"),
        },
        Command::RemoveMacro {
            kind: MacroKind::Template,
            name: s("ledge"),
            keep_uses: false,
            keep_empty: false,
        },
        Command::SetStamp {
            layer: s("main_tiling"),
            stamp: s("arch"),
            def: serde_json::from_value(
                json!({ "slug": "arch", "tileset": "castle", "rect": [0, 4, 3, 2] }),
            )
            .unwrap(),
            index: None,
        },
        Command::RemoveStamp {
            layer: s("main_tiling"),
            stamp: s("arch"),
            keep_uses: false,
            keep_empty: false,
        },
        Command::RemoveLayerDef { layer: s("traps") },
    ]
}

#[test]
fn every_command_inverts_in_sequence() {
    let mut p = common::fixture();
    for c in one_of_each() {
        check(&mut p, c);
    }
}

#[test]
fn history_undo_redo_every_command() {
    let mut p = common::fixture();
    let mut h = History::new();
    let mut states = vec![p.clone()];
    for c in one_of_each() {
        let label = c.label();
        let changes = h.push(&mut p, c).unwrap();
        assert!(!changes.is_empty(), "{label}");
        assert_eq!(h.undo_label(), Some(label.as_str()));
        states.push(p.clone());
    }
    let n = states.len() - 1;
    assert_eq!(h.undo_entries().len(), n);
    for i in (0..n).rev() {
        assert!(h.undo(&mut p).unwrap().is_some());
        assert!(p == states[i], "undo to state {i}");
    }
    assert_eq!(h.undo(&mut p).unwrap(), None);
    for (i, state) in states.iter().enumerate().skip(1) {
        assert!(h.redo(&mut p).unwrap().is_some());
        assert!(p == *state, "redo to state {i}");
    }
    assert_eq!(h.redo(&mut p).unwrap(), None);
    // Undo half way, then a new edit drops the redo stack.
    h.undo(&mut p).unwrap();
    h.undo(&mut p).unwrap();
    assert!(h.can_redo());
    h.push(&mut p, paint("crevasse", 3, 3, Some("solid")))
        .unwrap();
    assert!(!h.can_redo());
}

#[test]
fn gestures_are_one_entry() {
    let mut p = common::fixture();
    let start = p.clone();
    let mut h = History::new();
    h.begin_gesture("Brush stroke", Origin::User);
    for x in 1..6 {
        h.push(&mut p, paint("crevasse", x, 1, Some("solid")))
            .unwrap();
    }
    // A no-op inside the gesture adds nothing.
    h.push(&mut p, paint("crevasse", 1, 1, Some("solid")))
        .unwrap();
    assert!(h.end_gesture());
    let stroked = p.clone();
    assert_eq!(h.undo_entries().len(), 1);
    assert_eq!(h.undo_label(), Some("Brush stroke"));
    h.undo(&mut p).unwrap();
    assert_eq!(p, start);
    assert_eq!(h.redo_label(), Some("Brush stroke"));
    h.redo(&mut p).unwrap();
    assert_eq!(p, stroked);

    // An agent batch is one entry with its origin.
    let agent = Origin::Agent("claude".into());
    h.push_as(
        &mut p,
        Command::Batch {
            label: "Agent: paint east wall".into(),
            commands: (0..5)
                .map(|y| paint("crevasse", 18, y, Some("solid")))
                .collect(),
        },
        agent.clone(),
    )
    .unwrap();
    let e = h.undo_entries().last().unwrap();
    assert_eq!(
        (e.label.as_str(), &e.origin),
        ("Agent: paint east wall", &agent)
    );
    h.undo(&mut p).unwrap();
    assert_eq!(p, stroked);

    // An empty gesture records nothing; a cancelled one reverts.
    h.begin_gesture("nothing", Origin::User);
    assert!(!h.end_gesture());
    h.begin_gesture("cancelled", Origin::User);
    h.push(&mut p, paint("crevasse", 7, 1, Some("water")))
        .unwrap();
    h.push(&mut p, paint("crevasse", 8, 1, Some("water")))
        .unwrap();
    let changes = h.cancel_gesture(&mut p).unwrap();
    assert_eq!(
        changes,
        [grid_change("crevasse", "collision", [7, 1, 2, 1])]
    );
    assert_eq!(p, stroked);
    assert_eq!(h.undo_entries().len(), 1);
    // Undo during a gesture closes it first.
    h.begin_gesture("open", Origin::User);
    h.push(&mut p, paint("crevasse", 9, 1, Some("water")))
        .unwrap();
    assert!(h.can_undo());
    h.undo(&mut p).unwrap();
    assert_eq!(p, stroked);
}

/// What the API server does for an agent batch: apply each write with
/// `Project::apply`, keep the inverses itself, record them with
/// `History::record`. A user gesture in the middle neither joins the batch
/// nor ends it; the server flushes the agent's part first.
#[test]
fn record_makes_one_entry_beside_an_open_gesture() {
    let mut p = common::fixture();
    let start = p.clone();
    let mut h = History::new();
    let agent = Origin::Agent("claude-code".into());
    let mut collected = Vec::new();
    let apply = |p: &mut Project, cmd: Command, into: &mut Vec<Command>| {
        into.push(p.apply(cmd).unwrap().inverse);
    };

    // Agent writes two cells (and one no-op, whose inverse is dropped).
    apply(
        &mut p,
        paint("crevasse", 1, 1, Some("solid")),
        &mut collected,
    );
    apply(
        &mut p,
        paint("crevasse", 2, 1, Some("solid")),
        &mut collected,
    );
    apply(
        &mut p,
        paint("crevasse", 2, 1, Some("solid")),
        &mut collected,
    );
    let agent_part = p.clone();

    // The user starts a stroke; before their entry lands, the server
    // records the agent's part so far.
    h.begin_gesture("Brush stroke", Origin::User);
    h.push(&mut p, paint("crevasse", 5, 1, Some("water")))
        .unwrap();
    assert!(h.record("Agent: wall", agent.clone(), std::mem::take(&mut collected)));
    assert!(h.in_gesture(), "record leaves the user's gesture open");
    assert!(h.end_gesture());

    // The batch goes on and ends: a second entry with the same label.
    let before_rest = p.clone();
    apply(
        &mut p,
        paint("crevasse", 3, 1, Some("solid")),
        &mut collected,
    );
    assert!(h.record("Agent: wall", agent.clone(), collected));
    let entries: Vec<_> = h
        .undo_entries()
        .iter()
        .map(|e| (e.label.as_str(), e.origin.clone()))
        .collect();
    assert_eq!(
        entries,
        [
            ("Agent: wall", agent.clone()),
            ("Brush stroke", Origin::User),
            ("Agent: wall", agent.clone()),
        ]
    );

    // Each entry undoes exactly its own part.
    h.undo(&mut p).unwrap();
    assert_eq!(p, before_rest);
    h.undo(&mut p).unwrap();
    assert_eq!(p, agent_part);
    h.undo(&mut p).unwrap();
    assert_eq!(p, start);
    h.redo(&mut p).unwrap();
    assert_eq!(p, agent_part);

    // Recording clears the redo stack; nothing (or only no-ops) records nothing.
    assert!(h.can_redo());
    assert!(!h.record("empty", agent.clone(), Vec::new()));
    assert!(!h.record("noop", agent.clone(), vec![Command::noop()]));
    assert!(h.can_redo());
    let inv = p
        .apply(paint("crevasse", 9, 1, Some("solid")))
        .unwrap()
        .inverse;
    assert!(h.record("Agent: one", agent, vec![inv]));
    assert!(!h.can_redo());
}

// ---- dirty tracking and incremental save ----

/// The fixture saved to a temp dir and loaded back from it, so it knows its files.
fn on_disk() -> (tempfile::TempDir, Project) {
    let tmp = tempfile::tempdir().unwrap();
    save_all(&mut common::fixture(), tmp.path()).unwrap();
    let p = load(tmp.path()).unwrap();
    (tmp, p)
}

fn backdate(dir: &std::path::Path) -> SystemTime {
    let old = SystemTime::now() - Duration::from_secs(3600);
    for f in common::read_tree(dir).keys() {
        fs::File::options()
            .write(true)
            .open(dir.join(f))
            .unwrap()
            .set_modified(old)
            .unwrap();
    }
    old
}

fn touched(dir: &std::path::Path, since: SystemTime) -> Vec<String> {
    common::read_tree(dir)
        .into_keys()
        .filter(|f| {
            fs::metadata(dir.join(f)).unwrap().modified().unwrap() > since + Duration::from_secs(1)
        })
        .collect()
}

#[test]
fn one_cell_paint_dirties_exactly_one_file() {
    let (tmp, mut p) = on_disk();
    assert!(!p.is_dirty());
    let old = backdate(tmp.path());
    p.apply(paint("crevasse", 3, 1, Some("solid"))).unwrap();
    assert!(p.is_dirty());
    let dirty: Vec<_> = p.files.dirty().iter().cloned().collect();
    assert_eq!(
        dirty,
        [PathBuf::from("worlds/main/levels/crevasse/collision.json")]
    );
    save_dirty(&mut p, tmp.path()).unwrap();
    assert!(!p.is_dirty());
    assert_eq!(
        touched(tmp.path(), old),
        ["worlds/main/levels/crevasse/collision.json"]
    );
    assert_eq!(load(tmp.path()).unwrap(), p);
    // A failed command dirties nothing.
    assert!(p.apply(paint("crevasse", 30, 1, Some("solid"))).is_err());
    assert!(!p.is_dirty());
}

#[test]
fn mark_clean_drops_one_file_from_the_dirty_set() {
    let (tmp, mut p) = on_disk();
    let mut door = p.entity_def("locked_door").unwrap().clone();
    door.name = "Door".into();
    p.apply(Command::SetEntityDef {
        entity: s("locked_door"),
        def: door,
    })
    .unwrap();
    p.apply(paint("crevasse", 3, 1, Some("solid"))).unwrap();
    // The watcher wrote entities/locked_door.json itself: only the paint is
    // left to save.
    p.mark_clean("entities/locked_door.json");
    let dirty: Vec<_> = p.files.dirty().iter().cloned().collect();
    assert_eq!(
        dirty,
        [PathBuf::from("worlds/main/levels/crevasse/collision.json")]
    );
    // A path that isn't dirty is ignored.
    p.mark_clean("entities/ghost.json");
    p.mark_clean(PathBuf::from("worlds/main/levels/crevasse/collision.json"));
    assert!(!p.is_dirty());
    // save_dirty then writes nothing.
    let old = backdate(tmp.path());
    save_dirty(&mut p, tmp.path()).unwrap();
    assert!(touched(tmp.path(), old).is_empty());
}

#[cfg(unix)]
#[test]
fn files_written_before_a_failed_save_can_still_be_deleted() {
    use std::os::unix::fs::PermissionsExt;
    let (tmp, mut p) = on_disk();
    // A new level (sorts first, so its files are written first), and an
    // edit to a level whose directory can't be written.
    p.apply(Command::CreateLevel {
        world: s("main"),
        level: new_level("aaa", [4, 2]),
        content: LevelContent {
            int_grids: vec![IntGridFile {
                overrides: None,
                layer: s("collision"),
                legend: None,
                rows: vec!["#..#".into(), "####".into()],
            }],
            ..Default::default()
        },
        restore: false,
    })
    .unwrap();
    p.apply(paint("crevasse", 3, 1, Some("solid"))).unwrap();
    let crevasse = tmp.path().join("worlds/main/levels/crevasse");
    let mode = |m| fs::set_permissions(&crevasse, fs::Permissions::from_mode(m)).unwrap();
    mode(0o555);
    let res = save_dirty(&mut p, tmp.path());
    mode(0o755);
    assert!(res.is_err());
    assert!(
        tmp.path()
            .join("worlds/main/levels/aaa/level.json")
            .is_file()
    );
    assert!(p.is_dirty());
    // Deleting the half-saved level removes its files on the next save.
    p.apply(Command::DeleteLevel {
        world: s("main"),
        level: s("aaa"),
    })
    .unwrap();
    save_dirty(&mut p, tmp.path()).unwrap();
    assert!(!tmp.path().join("worlds/main/levels/aaa").exists());
    assert_eq!(load(tmp.path()).unwrap(), p);
}

#[test]
fn replace_level_saves_empty_entity_layer_entries() {
    let (tmp, mut p) = on_disk();
    // shaft has one entities layer with one entity; drop the entity but keep
    // the (now empty) entry, then remove the entry with a replace_level.
    p.apply(Command::RemoveEntity {
        id: id("doorshaft2"),
        keep_empty: true,
    })
    .unwrap();
    save_dirty(&mut p, tmp.path()).unwrap();
    let file = tmp.path().join("worlds/main/levels/shaft/entities.json");
    assert!(file.is_file());
    let mut level = p.level("shaft").unwrap().clone();
    level.entities.clear();
    let replace = |level: Level, entities| Command::ReplaceLevel {
        world: s("main"),
        level: Level {
            entities: Vec::new(),
            ..level
        },
        content: LevelContent {
            entities,
            ..Default::default()
        },
        restore: false,
    };
    let changes = check(&mut p, replace(level.clone(), Vec::new()));
    assert!(changes.contains(&Change::EntityLayer {
        world: s("main"),
        level: s("shaft"),
        layer: s("objects"),
    }));
    save_dirty(&mut p, tmp.path()).unwrap();
    assert!(!file.exists());
    assert_eq!(load(tmp.path()).unwrap(), p);
    // And adding an empty entry back writes the file again.
    check(
        &mut p,
        replace(
            level,
            vec![EntitiesLayer {
                layer: s("objects"),
                entities: Vec::new(),
            }],
        ),
    );
    save_dirty(&mut p, tmp.path()).unwrap();
    assert!(file.is_file());
    assert_eq!(load(tmp.path()).unwrap(), p);
}

#[test]
fn incremental_save_matches_full_save() {
    let (tmp, mut p) = on_disk();
    for c in one_of_each() {
        p.apply(c).unwrap();
    }
    // A rename back and forth, and clearing notes, to exercise deletes.
    p.apply(Command::RemoveNote {
        id: id("notecrev22"),
    })
    .unwrap();
    save_dirty(&mut p, tmp.path()).unwrap();
    assert!(!p.is_dirty());
    let full = tempfile::tempdir().unwrap();
    save_all(&mut p.clone(), full.path()).unwrap();
    assert_eq!(
        common::read_tree(tmp.path()),
        common::read_tree(full.path())
    );
    assert!(!tmp.path().join("worlds/main/levels/shaft").exists());
    assert!(!tmp.path().join("worlds/main/levels/annex").exists());
    assert!(
        tmp.path()
            .join("worlds/main/levels/wing/level.json")
            .exists()
    );
    assert_eq!(load(tmp.path()).unwrap(), p);

    // Undo everything through inverses and save again: back to the fixture.
    let saved = p.clone();
    let mut inverses = Vec::new();
    for c in [
        paint("crevasse", 1, 1, None),
        Command::DeleteLevel {
            world: s("main"),
            level: s("wing"),
        },
    ] {
        inverses.push(p.apply(c).unwrap().inverse);
    }
    save_dirty(&mut p, tmp.path()).unwrap();
    for inv in inverses.into_iter().rev() {
        p.apply(inv).unwrap();
    }
    save_dirty(&mut p, tmp.path()).unwrap();
    assert_eq!(p, saved);
    assert_eq!(
        common::read_tree(tmp.path()),
        common::read_tree(full.path())
    );
}

#[test]
fn save_dirty_falls_back_to_a_full_save_elsewhere() {
    let mut p = common::fixture();
    p.apply(paint("crevasse", 3, 1, Some("solid"))).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    save_dirty(&mut p, tmp.path()).unwrap();
    let full = tempfile::tempdir().unwrap();
    save_all(&mut p.clone(), full.path()).unwrap();
    assert_eq!(
        common::read_tree(tmp.path()),
        common::read_tree(full.path())
    );
}

#[test]
fn commands_have_a_schema() {
    let schema = schemars::schema_for!(Command);
    let v = serde_json::to_value(&schema).unwrap();
    let validator = jsonschema::validator_for(&v).unwrap();
    for c in one_of_each() {
        let j = serde_json::to_value(&c).unwrap();
        assert!(validator.is_valid(&j), "{j}");
    }
    let schema = schemars::schema_for!(Change);
    assert!(
        serde_json::to_string(&schema)
            .unwrap()
            .contains("level_renamed")
    );
}

/// Random grid and tile edits (seeded), each recorded in a history: undoing
/// everything gets back the fixture exactly, redoing everything the end
/// state, and every intermediate state matches on the way.
#[test]
fn random_edit_sequences_undo_and_redo_exactly() {
    let mut rng = 0x1234_5678_9abc_def1u64;
    let mut next = move |n: u64| {
        rng ^= rng >> 12;
        rng ^= rng << 25;
        rng ^= rng >> 27;
        rng.wrapping_mul(0x2545_f491_4f6c_dd1d) % n.max(1)
    };
    let values = [
        None,
        Some("solid"),
        Some("water"),
        Some("oneway"),
        Some("ramp_l"),
    ];
    let mut p = common::fixture();
    let mut h = History::new();
    let mut states = vec![p.clone()];
    for step in 0..300 {
        let level = ["crevasse", "shaft"][next(2) as usize];
        let size = p.level(level).unwrap().size;
        let (x, y) = (next(size[0] as u64) as u32, next(size[1] as u64) as u32);
        let v = values[next(values.len() as u64) as usize];
        let cmd = match next(7) {
            0 | 1 => Command::PaintCells {
                level: s(level),
                layer: s("collision"),
                cells: (0..1 + next(4))
                    .map(|i| cell((x + i as u32).min(size[0] - 1), y, v))
                    .collect(),
                keep_empty: false,
            },
            2 => Command::FillRect {
                level: s(level),
                layer: s("collision"),
                rect: Rect::new(x, y, 1 + next((size[0] - x) as u64) as u32, 1),
                value: v.map(s).into(),
            },
            3 => Command::FloodFill {
                level: s(level),
                layer: s("collision"),
                at: [x, y],
                value: v.map(s).into(),
            },
            4 => Command::SetTiles {
                level: s(level),
                layer: s("decor_back"),
                tiles: (0..next(3))
                    .map(|i| tile(2 * x, 2 * y, "castle_main", [i as u32, 0]))
                    .collect(),
                clear: vec![[2 * x + 1, 2 * y]],
                keep_empty: false,
            },
            5 => Command::ClearTiles {
                level: s(level),
                layer: s("decor_back"),
                rect: Rect::new(2 * x, 2 * y, 2, 2),
            },
            _ => Command::ReplaceValue {
                level: s(level),
                layer: s("collision"),
                from: v.map(s).into(),
                to: values[next(5) as usize].map(s).into(),
            },
        };
        // A third of the edits are grouped in gestures of a few commands.
        if step % 3 == 0 {
            h.begin_gesture(format!("gesture {step}"), Origin::User);
        }
        let changes = h.push(&mut p, cmd).unwrap();
        if step % 3 == 2 && h.end_gesture() || !h.in_gesture() && !changes.is_empty() {
            states.push(p.clone());
        }
    }
    if h.end_gesture() {
        states.push(p.clone());
    }
    let end = p.clone();
    assert_eq!(h.undo_entries().len() + 1, states.len());
    for i in (0..states.len() - 1).rev() {
        h.undo(&mut p).unwrap().unwrap();
        assert!(p == states[i], "undo to {i}");
    }
    assert_eq!(p, common::fixture());
    while h.redo(&mut p).unwrap().is_some() {}
    assert_eq!(p, end);
}

#[test]
fn command_json_is_strict() {
    for bad in [
        r#"{"op":"remove_note","id":"pathcrev22","bogus":1}"#,
        r#"{"id":"pathcrev22"}"#,
        r#"{"op":"paint_cells","level":"crevasse","layer":"collision","cells":[[1,2]]}"#,
        r#"{"op":"fill_rect","level":"crevasse","layer":"collision","rect":[1,2,3],"value":null}"#,
    ] {
        assert!(serde_json::from_str::<Command>(bad).is_err(), "{bad}");
    }
}

#[test]
fn agent_batch_and_save_timing() {
    // DESIGN targets: a 1,000-cell agent batch applies within a frame, and
    // saving after a one-cell edit rewrites one file in under 5 ms.
    let (tmp, mut p) = on_disk();
    let values = [Some("solid"), None, Some("water")];
    let commands: Vec<Command> = (0..1000u32)
        .map(|i| {
            paint(
                "crevasse",
                1 + i % 18,
                1 + (i / 18) % 3,
                values[(i % 3) as usize],
            )
        })
        .collect();
    let t = std::time::Instant::now();
    let a = p
        .apply(Command::Batch {
            label: "agent".into(),
            commands,
        })
        .unwrap();
    let batch_ms = t.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(a.changes.len(), 1, "grid changes coalesce");
    save_dirty(&mut p, tmp.path()).unwrap();
    p.apply(paint("crevasse", 1, 1, Some("oneway"))).unwrap();
    let t = std::time::Instant::now();
    save_dirty(&mut p, tmp.path()).unwrap();
    let save_ms = t.elapsed().as_secs_f64() * 1000.0;
    eprintln!("1000-command batch: {batch_ms:.3} ms; one-cell save: {save_ms:.3} ms");
    let (b, s_) = if cfg!(debug_assertions) {
        (500.0, 200.0)
    } else {
        (16.0, 5.0)
    };
    assert!(batch_ms < b && save_ms < s_, "{batch_ms} ms, {save_ms} ms");
}

/// A level the loader accepts but a fresh `create_level` would refuse (a grid
/// whose size doesn't match the level, an entity id another level also uses:
/// both `validate` problems, not load errors) still comes back byte for byte
/// when its deletion, resize or replacement is undone.
#[test]
fn undo_restores_a_level_with_validation_problems_verbatim() {
    let (tmp, _) = on_disk();
    let dir = tmp.path().join("worlds/main/levels/crevasse");
    // 4 rows of 18 glyphs on a 20x5 level.
    fs::write(
        dir.join("collision.json"),
        "{ \"$schema\": \"../../../../schema/intgrid.json\", \"format\": 1, \
         \"layer\": \"collision\", \"rows\": [\
         \"##################\", \"#................#\", \"#.......oooo.....#\", \
         \"##################\"] }\n",
    )
    .unwrap();
    // The same door id as in shaft.
    let ents = fs::read_to_string(dir.join("entities.json")).unwrap();
    fs::write(
        dir.join("entities.json"),
        ents.replace("doorcrev22", "doorshaft2"),
    )
    .unwrap();
    let mut p = load(tmp.path()).unwrap();
    save_all(&mut p, tmp.path()).unwrap();
    let original = common::read_tree(tmp.path());
    let problems = validate(&p);
    assert!(
        problems
            .iter()
            .any(|pr| pr.file.ends_with("crevasse/collision.json"))
    );
    // A fresh create of the same level is refused: the commands are strict.
    let fresh = |p: &Project| {
        let mut l = p.level("crevasse").unwrap().clone();
        let content = LevelContent {
            int_grids: vec![IntGridFile {
                overrides: None,
                layer: s("collision"),
                legend: None,
                rows: rows(p, "crevasse", "collision").unwrap(),
            }],
            ..Default::default()
        };
        l.int_grids.clear();
        l.tiles.clear();
        l.entities.clear();
        l.notes.clear();
        (l, content)
    };
    let (meta, content) = fresh(&p);
    let mut q = p.clone();
    q.apply(Command::DeleteLevel {
        world: s("main"),
        level: s("crevasse"),
    })
    .unwrap();
    assert!(matches!(
        q.apply(Command::CreateLevel {
            world: s("main"),
            level: meta,
            content,
            restore: false,
        }),
        Err(CommandError::GridSize { .. })
    ));
    let (mut flat, _) = fresh(&p);
    flat.slug = s("flat");
    flat.id = id("flatlvl222");
    flat.size = [4, 0];
    assert!(matches!(
        q.apply(Command::CreateLevel {
            world: s("main"),
            level: flat,
            content: LevelContent::default(),
            restore: false,
        }),
        Err(CommandError::Invalid(m)) if m.contains("1x1")
    ));

    for cmd in [
        Command::DeleteLevel {
            world: s("main"),
            level: s("crevasse"),
        },
        Command::ResizeLevel {
            world: s("main"),
            level: s("crevasse"),
            size: [22, 6],
            anchor: [-1, -1],
        },
        Command::RenameLevel {
            world: s("main"),
            level: s("crevasse"),
            slug: s("crevasse"),
            name: "Crevasse!".into(),
        },
    ] {
        let label = cmd.label();
        let mut h = History::new();
        h.push(&mut p, cmd)
            .unwrap_or_else(|e| panic!("{label}: {e}"));
        save_dirty(&mut p, tmp.path()).unwrap();
        assert_ne!(common::read_tree(tmp.path()), original, "{label}");
        h.undo(&mut p)
            .unwrap_or_else(|e| panic!("undo {label}: {e}"))
            .unwrap();
        save_dirty(&mut p, tmp.path()).unwrap();
        assert_eq!(common::read_tree(tmp.path()), original, "undo {label}");
        // And redo, undo again.
        h.redo(&mut p).unwrap().unwrap();
        h.undo(&mut p).unwrap().unwrap();
        save_dirty(&mut p, tmp.path()).unwrap();
        assert_eq!(common::read_tree(tmp.path()), original, "redo+undo {label}");
    }

    // A replace (the user's own wholesale rewrite) undoes the same way.
    let (meta, content) = {
        let (mut m, mut c) = fresh(&p);
        m.size = [18, 4];
        c.entities.clear();
        (m, c)
    };
    let mut h = History::new();
    h.push(
        &mut p,
        Command::ReplaceLevel {
            world: s("main"),
            level: meta,
            content,
            restore: false,
        },
    )
    .unwrap();
    h.undo(&mut p).unwrap().unwrap();
    save_dirty(&mut p, tmp.path()).unwrap();
    assert_eq!(
        common::read_tree(tmp.path()),
        original,
        "undo replace_level"
    );
}

/// `Project::find_note`, `note_index` and `NoteAt::offset`: the one lookup
/// the editor's ghosts and the server build on.
#[test]
fn find_note_gives_scope_and_offset() {
    let mut p = common::fixture();
    let id = |s: &str| Id::new(s).unwrap();
    let pin = p.find_note(&id("pinstatue2")).unwrap();
    assert_eq!(pin.scope(), NoteScope::World(s("main")));
    assert_eq!(pin.offset(), [0, 0]);
    let crev = p.level("crevasse").unwrap().world_px;
    let note = p.find_note(&id("notecrev22")).unwrap();
    assert_eq!(note.scope(), NoteScope::Level(s("crevasse")));
    assert_eq!(note.offset(), crev);
    assert!(p.find_note(&id("gonegonego")).is_none());
    // A grouped guide is offset by its group, as built.
    p.worlds[0].guide_groups.push(GuideGroup {
        offset: [0, 64],
        ..GuideGroup::new(s("stem"))
    });
    if let Shape::Guide { group, .. } = &mut p.worlds[0].notes[0].shape {
        *group = Some(s("stem"));
    }
    assert_eq!(p.find_note(&id("guidestem2")).unwrap().offset(), [0, 64]);
    // The index holds every note once, by id.
    let index = p.note_index();
    assert_eq!(index.len(), p.notes().count());
    assert_eq!(
        index[&id("notecrev22")].scope(),
        NoteScope::Level(s("crevasse"))
    );
}
