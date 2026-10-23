//! M6 engine: region rules and the spacing strategies, passes, `write`
//! outputs into derived grids (effective = overrides over computed), stamps
//! with occupancy and `avoid_overlap`, `requires_level_tag`, `require` as a
//! list, the cross-layer bake order, previews and export warnings.

mod common;

use std::collections::BTreeMap;

use serde_json::{Value, json};
use worlds_core::{
    AutoDef, Color, DerivedOverrides, Flip, IntGridDef, IntGridLayer, IntGridValue, LayerDef,
    LayerKind, Level, Override, Project, Rule, RuleGroup, RuleSet, Stamp, TilesetDef, slug,
};
use worlds_rules::{Error, LayerBake, TileOpacity, TilesetImages};

// ---- the test project ----

/// `common::project()` plus a derived layer `marks` (values `p`
/// pillar_base, `q` other; on the project grid), a 32 px tileset `big` and
/// an 8 px tileset `tiny`. Rules of `auto` come from `groups` (JSON).
fn project(groups: Value) -> Project {
    let mut p = common::project();
    p.layers.insert(1, marks_def("marks"));
    p.tilesets.push(TilesetDef {
        slug: slug("big"),
        image: "big.png".into(),
        tile_size: [32, 32],
        spacing: 0,
        padding: 0,
    });
    p.tilesets.push(TilesetDef {
        slug: slug("tiny"),
        image: "tiny.png".into(),
        tile_size: [8, 8],
        spacing: 0,
        padding: 0,
    });
    p.rules[0].groups = serde_json::from_value(groups).unwrap();
    p
}

fn marks_def(name: &str) -> LayerDef {
    let value = |s: &str, g: char| IntGridValue {
        slug: slug(s),
        name: s.into(),
        color: Color::new("#E0A040").unwrap(),
        glyph: g,
        group: None,
    };
    LayerDef::new(
        slug(name),
        name,
        LayerKind::IntGrid(IntGridDef {
            values: vec![value("pillar_base", 'p'), value("other", 'q')],
            groups: vec![],
            derived: true,
        }),
    )
}

fn images() -> TilesetImages {
    let imgs = common::images();
    for ts in ["big", "tiny"] {
        imgs.insert(slug(ts), TileOpacity::from_flags(8, 8, vec![false; 64]));
    }
    imgs
}

fn level(p: &Project, rows: &[&str]) -> Level {
    common::level(p, rows)
}

fn bake(p: &Project, l: &Level) -> LayerBake {
    worlds_rules::bake_layer(p, l, &slug("auto"), &images()).unwrap_or_else(|e| panic!("{e}"))
}

/// The cells of a derived grid holding a value, row-major (by `[x, y]`).
fn cells_of(g: &IntGridLayer) -> Vec<[i32; 2]> {
    let [w, h] = g.size();
    let mut out = Vec::new();
    for y in 0..h {
        for x in 0..w {
            if g.index_at(x, y).unwrap() != 0 {
                out.push([x as i32, y as i32]);
            }
        }
    }
    out
}

fn marks(b: &LayerBake) -> Vec<[i32; 2]> {
    cells_of(&b.derived[&slug("marks")])
}

fn fired_at(b: &LayerBake, rule: &str) -> Vec<[i32; 2]> {
    let mut v = common::fired_at(b, rule);
    v.sort_by_key(|c| (c[1], c[0]));
    v
}

/// A region rule writing `p` into `marks`.
fn region_write(region: Value, place: Value) -> Value {
    json!({ "slug": "r", "region": region, "place": place,
            "write": { "grid": "marks", "value": "pillar_base" } })
}

fn one_group(rule: Value) -> Value {
    json!([{ "slug": "g", "rules": [rule] }])
}

fn row(xs: impl IntoIterator<Item = i32>, y: i32) -> Vec<[i32; 2]> {
    xs.into_iter().map(|x| [x, y]).collect()
}

// ---- region rules: the worked examples ----

#[test]
fn crevasse_floor_every_6_margin_3() {
    let p = project(one_group(region_write(
        json!({ "cells": ".", "min_width": 15 }),
        json!({ "along": "floor", "every": 6, "margin": 3, "align": "center" }),
    )));
    let wide = ".".repeat(64);
    let solid = "#".repeat(64);
    let l = level(&p, &[&solid, &wide, &wide, &wide, &wide, &solid]);
    let b = bake(&p, &l);
    // 3..60, k 10, slack 3, offset 1: 4, 10, ..., 58.
    let want = row((0..10).map(|i| 4 + 6 * i), 4);
    assert_eq!(marks(&b), want);
    // A write firing is in `fired`; it draws nothing.
    assert_eq!(fired_at(&b, "r"), want);
    assert!(b.placements.is_empty());
}

#[test]
fn floor_broken_by_a_pit() {
    let p = project(one_group(region_write(
        json!({ "cells": "." }),
        json!({ "along": "floor", "every": 6, "margin": 3 }),
    )));
    let l = level(
        &p,
        &[
            "#################",
            "#...............#",
            "#######...#######",
            "#######...#######",
            "#################",
        ],
    );
    let b = bake(&p, &l);
    assert_eq!(
        marks(&b),
        Vec::<[i32; 2]>::new(),
        "every run shorter than 2m+1"
    );
    let regions = worlds_rules::region_preview(&p, &l, &slug("auto"), "r", &images())
        .unwrap()
        .unwrap();
    assert_eq!(regions.len(), 1);
    // Two floor runs either side of the pit, the pit's bottom a third.
    assert_eq!(regions[0].runs, vec![[1, 1, 6], [10, 1, 6], [7, 3, 3]]);
    assert!(regions[0].candidates.is_empty());
    // With margin 0 and every 2 each run is placed on its own.
    let p = project(one_group(region_write(
        json!({ "cells": "." }),
        json!({ "along": "floor", "every": 2, "align": "start" }),
    )));
    let b = bake(&p, &l);
    let mut want = row([1, 3, 5, 10, 12, 14], 1);
    want.extend(row([7, 9], 3));
    assert_eq!(marks(&b), want);
}

#[test]
fn several_floor_runs_in_one_region() {
    let p = project(one_group(region_write(
        json!({ "cells": "." }),
        json!({ "along": "floor", "every": 6, "margin": 3 }),
    )));
    let l = level(
        &p,
        &[
            "###################",
            "...................",
            "##########.........",
            "#..................",
            "#..................",
            "###################",
        ],
    );
    let b = bake(&p, &l);
    // Ledge 0..9: 4. Floor 1..18: lo 4, hi 15, k 2, slack 5 -> 6, 12.
    assert_eq!(marks(&b), vec![[4, 1], [6, 4], [12, 4]]);
    // `min_length` drops the ledge (10 cells) but keeps the floor (18).
    let p = project(one_group(region_write(
        json!({ "cells": "." }),
        json!({ "along": "floor", "every": 6, "margin": 3, "min_length": 11 }),
    )));
    assert_eq!(marks(&bake(&p, &l)), vec![[6, 4], [12, 4]]);
}

/// Level 30 wide, a wall column at 15: rooms 15 and 14 wide.
fn partitioned(p: &Project) -> Level {
    let solid = "#".repeat(30);
    let room = format!("{}#{}", ".".repeat(15), ".".repeat(14));
    level(p, &[&solid, &room, &room, &room, &solid])
}

#[test]
fn min_width_is_inclusive_and_odd_slack_leans_left() {
    let rule = |min: u32| {
        project(one_group(region_write(
            json!({ "cells": ".", "min_width": min }),
            json!({ "along": "floor", "every": 6, "margin": 3, "align": "center" }),
        )))
    };
    let p = rule(15);
    let l = partitioned(&p);
    assert_eq!(marks(&bake(&p, &l)), vec![[4, 3], [10, 3]]);
    let p = rule(14);
    // The 14-wide room: run 16..29 -> 19..26, k 2, slack 1 -> 19, 25.
    assert_eq!(
        marks(&bake(&p, &l)),
        vec![[4, 3], [10, 3], [19, 3], [25, 3]]
    );
    // Selection in the preview.
    let r = worlds_rules::region_preview(&p, &l, &slug("auto"), "r", &images())
        .unwrap()
        .unwrap();
    let p15 = rule(15);
    let r15 = worlds_rules::region_preview(&p15, &l, &slug("auto"), "r", &images())
        .unwrap()
        .unwrap();
    assert_eq!(r.len(), 2);
    assert_eq!(
        r15.iter()
            .map(|x| (x.bbox, x.area, x.selected))
            .collect::<Vec<_>>(),
        vec![([0, 1, 15, 3], 45, true), ([16, 1, 14, 3], 42, false)]
    );
    assert!(r15[1].candidates.is_empty());
    assert_eq!(
        r15[1].runs,
        vec![[16, 3, 14]],
        "runs listed even when not selected"
    );
}

#[test]
fn margin_exactly_2m_plus_1_and_larger_than_the_run() {
    let p = project(one_group(region_write(
        json!({ "cells": "." }),
        json!({ "along": "floor", "every": 6, "margin": 3 }),
    )));
    // Rooms of 7 (x1..7), 5 (x9..13) cells.
    let l = level(
        &p,
        &["###############", "#.......#.....#", "###############"],
    );
    assert_eq!(marks(&bake(&p, &l)), vec![[4, 1]]);
}

#[test]
fn spread_spacing() {
    let p = project(one_group(region_write(
        json!({ "cells": "." }),
        json!({ "along": "floor", "spacing": "spread", "every": 6, "margin": 3 }),
    )));
    let room = ".".repeat(30);
    let solid = "#".repeat(30);
    let l = level(&p, &[&solid, &room, &solid]);
    assert_eq!(marks(&bake(&p, &l)), row([3, 11, 18, 26], 1));
}

#[test]
fn random_spacing_is_seeded_and_bounded() {
    let rule = |salt: i64| {
        let mut r = region_write(
            json!({ "cells": "." }),
            json!({ "along": "floor", "spacing": "random", "every": 4, "jitter": 3, "margin": 2 }),
        );
        r["salt"] = json!(salt);
        project(one_group(r))
    };
    let room = ".".repeat(120);
    let solid = "#".repeat(120);
    let p = rule(0);
    let l = level(&p, &[&solid, &room, &room, &solid]);
    let a = marks(&bake(&p, &l));
    assert_eq!(a, marks(&bake(&p, &l)), "same project, same marks");
    assert!(a.len() >= 120 / 8, "{a:?}");
    assert!(a.iter().all(|c| c[1] == 2 && c[0] >= 2 && c[0] <= 117));
    assert!(a[0][0] < 2 + 4, "phase is below every");
    for w in a.windows(2) {
        assert!((4..=7).contains(&(w[1][0] - w[0][0])), "{a:?}");
    }
    assert_ne!(a, marks(&bake(&rule(1), &l)), "salt re-rolls");
}

#[test]
fn across_interior_lattice() {
    let p = project(one_group(region_write(
        json!({ "cells": "." }),
        json!({ "across": "interior", "every": [6, 3], "margin": [3, 1] }),
    )));
    let solid = "#".repeat(30);
    let room = ".".repeat(30);
    let mut rows = vec![solid.clone()];
    rows.extend(std::iter::repeat_n(room.clone(), 6));
    rows.push(solid);
    // A hole at (11, 5): that lattice point isn't a region cell.
    let mut hole: Vec<char> = rows[5].chars().collect();
    hole[11] = '#';
    rows[5] = hole.into_iter().collect();
    let rows: Vec<&str> = rows.iter().map(String::as_str).collect();
    let l = level(&p, &rows);
    // x: 5, 11, 17, 23; y: 2, 5. Eight pairs, minus the hole.
    let mut want = row([5, 11, 17, 23], 2);
    want.extend(row([5, 17, 23], 5));
    assert_eq!(marks(&bake(&p, &l)), want);
}

#[test]
fn walls_and_against() {
    // A left wall of `#` above and `f` below; `against: fortified` keeps
    // only the cells next to `f`.
    let rule = |against: Option<&str>| {
        let mut place = json!({ "along": "wall_left" });
        if let Some(a) = against {
            place["against"] = json!(a);
        }
        project(one_group(region_write(json!({ "cells": "." }), place)))
    };
    let rows = ["######", "#....#", "#....#", "f....#", "f....#", "######"];
    let p = rule(None);
    let l = level(&p, &rows);
    assert_eq!(marks(&bake(&p, &l)), vec![[1, 1], [1, 2], [1, 3], [1, 4]]);
    let p = rule(Some("fortified"));
    assert_eq!(marks(&bake(&p, &l)), vec![[1, 3], [1, 4]]);
    // The level's edge is no wall.
    let p = rule(None);
    let l = level(&p, &["......", "######"]);
    assert!(marks(&bake(&p, &l)).is_empty());
}

#[test]
fn regions_api() {
    let p = project(json!([]));
    let l = level(
        &p,
        &[
            "##########",
            "#....#...#",
            "#....#...#",
            "##...#####",
            "##########",
        ],
    );
    let values = l.int_grid("collision").unwrap();
    let r = worlds_rules::regions(
        &p,
        "collision",
        values,
        &serde_json::from_value(json!(".")).unwrap(),
    )
    .unwrap();
    assert_eq!(r.regions.len(), 2);
    let a = &r.regions[0];
    assert_eq!((a.id, a.bbox, a.area), (0, [1, 1, 4, 3], 11));
    assert_eq!(a.edges.floor, vec![[1, 2, 1], [2, 3, 3]]);
    assert_eq!(a.edges.ceiling, vec![[1, 1, 4]]);
    assert_eq!(a.edges.wall_left, vec![[1, 1, 2], [2, 3, 1]]);
    assert_eq!(a.edges.wall_right, vec![[4, 1, 3]]);
    assert_eq!(r.regions[1].bbox, [6, 1, 3, 2]);
    assert_eq!(
        r.picture(),
        vec![
            "##########",
            "#aaaa#bbb#",
            "#aaaa#bbb#",
            "##aaa#####",
            "##########"
        ]
    );
    // An unknown value in the test is an error.
    let bad = worlds_rules::regions(
        &p,
        "collision",
        values,
        &serde_json::from_value(json!("lava")).unwrap(),
    );
    assert!(matches!(bad, Err(Error::UnknownCellTest { .. })), "{bad:?}");
}

// ---- region rules that draw ----

#[test]
fn region_rule_out_fires_like_variant_0() {
    // Two alternatives and a random offset: the region rule's picks equal a
    // pattern rule's (same slug) as written at the same anchors.
    let out = json!({ "tileset": "main", "tiles": [[1, 0], { "stack": [[2, 0]], "weight": 3 }],
                      "random_offset_px": { "x": [-3, 3], "y": [0, 2] } });
    let region = project(one_group(json!({
        "slug": "r", "region": { "cells": "." }, "place": { "along": "floor" }, "out": out })));
    let pattern = project(one_group(json!({
        "slug": "r", "match": { "rows": ["@", "#"], "legend": { "@": "." } }, "out": out })));
    let rows = ["........", "...#....", "........", "########"];
    let l = level(&region, &rows);
    let a = bake(&region, &l);
    // The same level (its id is part of the layer seed).
    let b = bake(&pattern, &l);
    assert_eq!(a.placements.len(), 9);
    assert_eq!(a.placements, b.placements);
    assert_eq!(a.fired, b.fired);
}

#[test]
fn region_rule_opacity_lock_and_break() {
    // Tile [0, 0] of `main` is opaque: the region rule locks its anchors,
    // so the later rule of the pass skips them.
    let groups = json!([{ "slug": "g", "rules": [
        { "slug": "r", "region": { "cells": "." }, "place": { "along": "floor", "every": 2, "align": "start" },
          "out": { "tileset": "main", "tiles": [[0, 0]] } },
        { "slug": "after", "match": { "rows": ["@", "#"], "legend": { "@": "." } },
          "out": { "tileset": "main", "tiles": [[3, 0]] } }
    ] }]);
    let p = project(groups);
    let l = level(&p, &["......", "######"]);
    let b = bake(&p, &l);
    assert_eq!(fired_at(&b, "r"), row([0, 2, 4], 0));
    assert_eq!(fired_at(&b, "after"), row([1, 3, 5], 0));
}

// ---- footprints and occupancy ----

/// A project with grid `g`, one stamp `s` (tileset, rect, pivot) that
/// occupies, and a rule stamping it at every cell matching `rows`.
fn stamp_project(g: u32, tileset: &str, rect: [u32; 4], pivot: [f64; 2], rule: Value) -> Project {
    let mut p = project(json!([{ "slug": "g", "rules": [rule] }]));
    p.grid_size = g;
    p.rules[0].stamps = vec![Stamp {
        slug: slug("s"),
        tileset: slug(tileset),
        rect,
        pivot,
        occupies: true,
    }];
    p
}

fn occupied_cells(b: &LayerBake, w: i32) -> Vec<[i32; 2]> {
    b.occupied
        .iter()
        .enumerate()
        .filter(|(_, o)| **o)
        .map(|(i, _)| [i as i32 % w, i as i32 / w])
        .collect()
}

fn at_x() -> Value {
    // Fires at the one `~` cell.
    json!({ "slug": "st", "match": { "rows": ["~"] }, "out": { "stamp": "s" } })
}

#[test]
fn footprint_examples() {
    let rows = [
        "..........",
        "..........",
        ".....~....",
        "..........",
        "..........",
    ];
    // Gate pillar: 2x4 tiles of 32 px, pivot [0, 1], grid 32: x, x+1 by
    // y-3..y.
    let p = stamp_project(32, "big", [0, 0, 2, 4], [0.0, 1.0], at_x());
    let b = bake(&p, &level(&p, &rows));
    let mut want = Vec::new();
    for y in 0..=2 {
        want.extend(row([5, 6], y));
    }
    assert_eq!(occupied_cells(&b, 10), want, "clipped at the top");
    // Pivot [0.5, 1]: box [32x - 16, 32x + 48): x-1, x.
    let p = stamp_project(32, "big", [0, 0, 2, 1], [0.5, 1.0], at_x());
    let b = bake(&p, &level(&p, &rows));
    assert_eq!(occupied_cells(&b, 10), row([4, 5], 2));
    assert!(b.placements.iter().all(|t| t.px[0] % 32 == 16));
    // Pivot [1, 1]: box [32x - 32, 32x + 32): x-1, x.
    let p = stamp_project(32, "big", [0, 0, 2, 1], [1.0, 1.0], at_x());
    assert_eq!(
        occupied_cells(&bake(&p, &level(&p, &rows)), 10),
        row([4, 5], 2)
    );
    // Tile = grid, pivot [0, 0]: exactly the stamp's cells.
    let p = stamp_project(16, "main", [0, 2, 3, 2], [0.0, 0.0], at_x());
    let b = bake(&p, &level(&p, &rows));
    let mut want = row([5, 6, 7], 2);
    want.extend(row([5, 6, 7], 3));
    assert_eq!(occupied_cells(&b, 10), want);
    // A 64x32 px stamp (two 32 px tiles) on a 16 px grid, pivot [0, 0]:
    // 4x2 cells.
    let p = stamp_project(16, "big", [0, 0, 2, 1], [0.0, 0.0], at_x());
    let b = bake(&p, &level(&p, &rows));
    let mut want = row([5, 6, 7, 8], 2);
    want.extend(row([5, 6, 7, 8], 3));
    assert_eq!(occupied_cells(&b, 10), want);
    // A 32 px tile on a 16 px grid: 2x2 cells.
    let p = stamp_project(16, "big", [0, 0, 1, 1], [0.0, 0.0], at_x());
    let b = bake(&p, &level(&p, &rows));
    let mut want = row([5, 6], 2);
    want.extend(row([5, 6], 3));
    assert_eq!(occupied_cells(&b, 10), want);
    // A 16 px tile on a 32 px grid at the cell's corner misses its center.
    let p = stamp_project(32, "main", [0, 2, 1, 1], [0.0, 0.0], at_x());
    assert!(occupied_cells(&bake(&p, &level(&p, &rows)), 10).is_empty());
    // ... offset by 8 px it covers it.
    let mut r = at_x();
    r["out"]["offset_px"] = json!([8, 8]);
    let p = stamp_project(32, "main", [0, 2, 1, 1], [0.0, 0.0], r);
    assert_eq!(
        occupied_cells(&bake(&p, &level(&p, &rows)), 10),
        vec![[5, 2]]
    );
    // An 8 px tile on a 32 px grid with pivot 0.5 sits on the center.
    let p = stamp_project(32, "tiny", [0, 0, 1, 1], [0.5, 0.5], at_x());
    assert_eq!(
        occupied_cells(&bake(&p, &level(&p, &rows)), 10),
        vec![[5, 2]]
    );
}

#[test]
fn footprint_of_a_mirrored_variant() {
    // A 2x1 stamp, pivot 0: as written cells x, x+1. Only the mirrored
    // pattern matches (`#` to the left), and mirrored the layout lands on
    // x-1, x.
    let rule = json!({ "slug": "st", "match": { "rows": ["@#"], "legend": { "@": "." } }, "symmetry": ["mirror_x"],
                       "out": { "stamp": "s" } });
    let p = stamp_project(16, "main", [0, 2, 2, 1], [0.0, 0.0], rule);
    let l = level(&p, &["......", "...#..", "......"]);
    let b = bake(&p, &l);
    // As written at (2, 1) (`#` at x+1): cells 2, 3. Mirrored at (4, 1):
    // cells 3, 4.
    assert_eq!(b.placements.len(), 4);
    assert!(b.placements[2..].iter().all(|t| t.flip == Some(Flip::X)));
    assert_eq!(occupied_cells(&b, 6), row([2, 3, 4], 1));
    // Only the mirror: a `#` at the far left.
    let l = level(&p, &["#.....", "......"]);
    let b = bake(&p, &l);
    assert_eq!(fired_at(&b, "st"), vec![[1, 0]]);
    assert_eq!(occupied_cells(&b, 6), row([0, 1], 0));
}

/// A `place` rule stamping `s` (2 wide, pivot [0, 1] on a 16 px grid, so
/// cells x, x+1) at every `~`, avoiding overlap.
fn avoid_project(extra: Vec<Value>) -> Project {
    let mut rules = vec![json!({ "slug": "place", "match": { "rows": ["~"] },
                                 "out": { "stamp": "s", "avoid_overlap": true } })];
    rules.extend(extra);
    let mut p = project(json!([{ "slug": "g", "rules": rules }]));
    p.rules[0].stamps = vec![Stamp {
        slug: slug("s"),
        tileset: slug("main"),
        rect: [1, 2, 2, 1],
        pivot: [0.0, 1.0],
        occupies: true,
    }];
    p
}

#[test]
fn occupancy_order_touching_and_overlapping() {
    let p = avoid_project(vec![]);
    // Touching (2 apart) both stand; overlapping (1 apart): the first in
    // reading order wins.
    let l = level(&p, &["~.~~...", "......."]);
    let b = bake(&p, &l);
    assert_eq!(fired_at(&b, "place"), vec![[0, 0], [2, 0]]);
    assert_eq!(occupied_cells(&b, 7), row([0, 1, 2, 3], 0));
    // The blocked anchor shows in the preview's `occupied`, not in `fired`.
    let pv = worlds_rules::preview_rules(&p, &l, &slug("auto"), &images()).unwrap();
    assert_eq!(pv[0].occupied, vec![[3, 0]]);
    assert_eq!(pv[0].fired, vec![[0, 0], [2, 0]]);
    assert_eq!(pv[0].matches.len(), 3);
    // Row-major: (1, 1) is after (5, 0) in reading order.
    let l = level(&p, &[".....~", ".~...."]);
    assert_eq!(fired_at(&bake(&p, &l), "place"), vec![[5, 0], [1, 1]]);
}

/// `preview_rules_for` gives the rules asked for, with matches only when
/// asked: otherwise exactly `preview_rules`' entries.
#[test]
fn preview_for_computes_only_what_is_asked() {
    let p = project(json!([
        { "slug": "g", "rules": [
            { "slug": "write", "match": { "rows": ["#"] }, "write": { "grid": "marks", "value": "pillar_base" } },
            { "slug": "rooms", "region": { "cells": "." }, "place": { "along": "floor", "every": 2 },
              "out": { "tileset": "main", "tiles": [[1, 0]] } } ] },
        { "slug": "art", "pass": 1, "rules": [
            { "slug": "on_p", "match": { "grid": "marks", "rows": ["p"] },
              "out": { "tileset": "main", "tiles": [[2, 0]] } } ] }
    ]));
    let l = level(&p, &["#...#", "#####"]);
    let layer = slug("auto");
    let all = worlds_rules::preview_rules(&p, &l, &layer, &images()).unwrap();
    assert_eq!(all.len(), 3);
    assert_eq!(
        worlds_rules::preview_rules_for(&p, &l, &layer, &images(), &|_| true, true).unwrap(),
        all
    );
    for rule in ["on_p", "rooms"] {
        let one = worlds_rules::preview_rules_for(&p, &l, &layer, &images(), &|s| s == rule, false)
            .unwrap();
        let mut want = all.iter().find(|r| r.rule == rule).unwrap().clone();
        assert!(!want.matches.is_empty(), "{rule}");
        want.matches.clear();
        assert_eq!(one, vec![want], "{rule}");
    }
    let rooms = all.iter().find(|r| r.rule == "rooms").unwrap();
    assert_eq!(
        worlds_rules::region_preview(&p, &l, &layer, "rooms", &images()).unwrap(),
        rooms.regions
    );
}

#[test]
fn occupancy_and_locks() {
    // `first` has break and fires at (2, 0): the anchor is locked, `place`
    // skips it before looking at occupancy and marks nothing, so (1, 0) is
    // free... but `place` at (1, 0) runs first in reading order anyway.
    // Then a blocked placement doesn't lock: `last` fires at (2, 0) of a
    // second level where `place` was blocked there.
    let p = avoid_project(vec![json!({ "slug": "last", "match": { "rows": ["~"] },
        "out": { "tileset": "main", "tiles": [[5, 5]] } })]);
    let l = level(&p, &[".~~....", "......."]);
    let b = bake(&p, &l);
    assert_eq!(fired_at(&b, "place"), vec![[1, 0]]);
    // (2, 0) was blocked (cells 2, 3 overlap 1, 2): no lock, `last` fires.
    assert_eq!(fired_at(&b, "last"), vec![[1, 0], [2, 0]]);

    // A locked anchor is skipped before occupancy: no mark.
    let mut p = avoid_project(vec![]);
    let place = p.rules[0].groups[0].rules.remove(0);
    let first: Rule = serde_json::from_value(json!({ "slug": "first", "match": { "rows": ["~"] },
        "break": true, "out": { "tileset": "main", "tiles": [[5, 5]] } }))
    .unwrap();
    p.rules[0].groups[0].rules = vec![first.into(), place];
    let l = level(&p, &["~......", "......."]);
    let b = bake(&p, &l);
    assert_eq!(fired_at(&b, "first"), vec![[0, 0]]);
    assert!(fired_at(&b, "place").is_empty());
    assert!(
        occupied_cells(&b, 7).is_empty(),
        "locks never mark occupancy"
    );
}

#[test]
fn placements_without_avoid_overlap_draw_anyway() {
    // `mark` occupies but doesn't avoid: both its stamps draw, overlapping.
    let mut p = avoid_project(vec![]);
    p.rules[0].groups[0].rules[0] = serde_json::from_value::<Rule>(json!({
        "slug": "mark", "match": { "rows": ["~"] }, "out": { "stamp": "s" } }))
    .unwrap()
    .into();
    let l = level(&p, &["~~.....", "......."]);
    let b = bake(&p, &l);
    assert_eq!(fired_at(&b, "mark"), vec![[0, 0], [1, 0]]);
    assert_eq!(occupied_cells(&b, 7), row([0, 1, 2], 0));
    // A tiles output can avoid overlap too.
    let mut p = avoid_project(vec![json!({ "slug": "dot", "match": { "rows": ["+"] },
        "out": { "tileset": "main", "tiles": [[5, 5]], "avoid_overlap": true } })]);
    p.rules[0].groups[0].rules[0] = serde_json::from_value::<Rule>(json!({
        "slug": "mark", "match": { "rows": ["~"] }, "out": { "stamp": "s" } }))
    .unwrap()
    .into();
    let l = level(&p, &["~..#...", "......."]);
    let b = bake(&p, &l);
    // `dot` at (0, 0) and (1, 0)? only `+` cells: (0, 0) is `~` (blocked by
    // the stamp's cells 0, 1), (3, 0) is free.
    assert_eq!(fired_at(&b, "dot"), vec![[3, 0]]);
}

// ---- passes ----

#[test]
fn locks_do_not_cross_passes_and_later_passes_draw_on_top() {
    let groups = json!([
        { "slug": "top", "pass": 1, "rules": [
            { "slug": "b", "match": { "rows": ["#"] }, "out": { "tileset": "main", "tiles": [[2, 0]] } } ] },
        { "slug": "base", "rules": [
            { "slug": "a", "match": { "rows": ["#"] }, "break": true,
              "out": { "tileset": "main", "tiles": [[1, 0]] } },
            { "slug": "a2", "match": { "rows": ["#"] }, "out": { "tileset": "main", "tiles": [[3, 0]] } } ] }
    ]);
    let p = project(groups);
    let l = level(&p, &["#."]);
    let b = bake(&p, &l);
    // Pass 0 runs first although the file lists pass 1 first; `a` locks
    // (0, 0) for the rest of pass 0 only.
    assert_eq!(b.fired[&[0, 0]], vec![slug("a"), slug("b")]);
    let order: Vec<&str> = b.placements.iter().map(|t| t.rule.as_str()).collect();
    assert_eq!(order, vec!["a", "b"], "pass 0 at the bottom, pass 1 on top");
    // Within a pass, earlier rules on top (as today).
    let groups = json!([
        { "slug": "base", "rules": [
            { "slug": "a", "match": { "rows": ["#"] }, "out": { "tileset": "main", "tiles": [[1, 0]] } },
            { "slug": "a2", "match": { "rows": ["#"] }, "out": { "tileset": "main", "tiles": [[3, 0]] } } ] },
        { "slug": "top", "pass": 1, "rules": [
            { "slug": "b", "match": { "rows": ["#"] }, "out": { "tileset": "main", "tiles": [[2, 0]] } },
            { "slug": "b2", "match": { "rows": ["#"] }, "out": { "tileset": "main", "tiles": [[4, 0]] } } ] }
    ]);
    let p = project(groups);
    let b = bake(&p, &level(&p, &["#."]));
    let order: Vec<&str> = b.placements.iter().map(|t| t.rule.as_str()).collect();
    assert_eq!(order, vec!["a2", "a", "b2", "b"]);
}

#[test]
fn writes_are_seen_by_the_next_pass_only() {
    let groups = json!([
        { "slug": "w", "rules": [
            { "slug": "write", "match": { "rows": ["#"] }, "write": { "grid": "marks", "value": "pillar_base" } },
            { "slug": "same_pass", "match": { "grid": "marks", "rows": ["p"] },
              "out": { "tileset": "main", "tiles": [[2, 0]] } } ] },
        { "slug": "next", "pass": 1, "rules": [
            { "slug": "next_pass", "match": { "grid": "marks", "rows": ["p"] },
              "out": { "tileset": "main", "tiles": [[3, 0]] } } ] }
    ]);
    let p = project(groups);
    let l = level(&p, &["#.#"]);
    let b = bake(&p, &l);
    assert_eq!(marks(&b), vec![[0, 0], [2, 0]]);
    assert!(fired_at(&b, "same_pass").is_empty());
    assert_eq!(fired_at(&b, "next_pass"), vec![[0, 0], [2, 0]]);
    // rule_matches sees each rule's pass's view.
    let m = worlds_rules::rule_matches(&p, &l, &slug("auto"), "next_pass", &images()).unwrap();
    assert_eq!(m.keys().copied().collect::<Vec<_>>(), vec![[0, 0], [2, 0]]);
    let m = worlds_rules::rule_matches(&p, &l, &slug("auto"), "same_pass", &images()).unwrap();
    assert!(m.is_empty());
}

#[test]
fn first_write_wins_in_a_pass_and_a_later_pass_overwrites() {
    let groups = json!([
        { "slug": "w", "rules": [
            { "slug": "w1", "match": { "rows": ["#"] }, "write": { "grid": "marks", "value": "pillar_base" } },
            { "slug": "w2", "match": { "rows": ["+"] }, "write": { "grid": "marks", "value": "other" } } ] },
        { "slug": "later", "pass": 2, "rules": [
            { "slug": "w3", "match": { "rows": ["f"] }, "write": { "grid": "marks", "value": "other" } } ] }
    ]);
    let p = project(groups);
    let l = level(&p, &["#~f"]);
    let b = bake(&p, &l);
    let m = &b.derived[&slug("marks")];
    // (0, 0): w1 first, w2's write dropped but it still fired.
    assert_eq!(m.cells(), &[1, 2, 2]);
    assert_eq!(b.fired[&[0, 0]], vec![slug("w1"), slug("w2")]);
    // (2, 0): `f` is `+` too, so w2 wrote `other`; pass 2 rewrote it.
    assert_eq!(b.fired[&[2, 0]], vec![slug("w2"), slug("w3")]);
    // A write with break locks: a later write rule in the pass can't fire.
    let groups = json!([{ "slug": "w", "rules": [
        { "slug": "w1", "match": { "rows": ["#"] }, "break": true, "write": { "grid": "marks", "value": "pillar_base" } },
        { "slug": "w2", "match": { "rows": ["+"] }, "write": { "grid": "marks", "value": "other" } } ] }]);
    let p = project(groups);
    let b = bake(&p, &level(&p, &["#~"]));
    assert_eq!(b.fired[&[0, 0]], vec![slug("w1")]);
}

// ---- derived grids and overrides ----

fn with_overrides(p: &Project, l: &mut Level, cells: &[([u32; 2], Override)]) {
    let def = p.layer("marks").unwrap();
    let mut o = DerivedOverrides::new_empty(def, l.size);
    for (c, v) in cells {
        o.set(c[0], c[1], *v);
    }
    l.overrides = vec![o];
}

#[test]
fn overrides_over_computed_through_every_entry_point() {
    // Pass 0 writes `p` on every `#`; pass 1 draws where marks is `+`, and
    // a `!p` rule draws where it isn't `p`. The overrides: (1, 0) set to
    // `p` on an empty cell, (0, 0) suppressed on a computed `p`.
    let groups = json!([
        { "slug": "w", "rules": [
            { "slug": "write", "match": { "rows": ["#"] }, "write": { "grid": "marks", "value": "pillar_base" } } ] },
        { "slug": "art", "pass": 1, "rules": [
            { "slug": "plus", "match": { "grid": "marks", "rows": ["+"] },
              "out": { "tileset": "main", "tiles": [[2, 0]] } },
            { "slug": "not_p", "match": { "grid": "marks", "rows": ["N"], "legend": { "N": "!pillar_base" } },
              "out": { "tileset": "main", "tiles": [[3, 0]] } } ] }
    ]);
    let p = project(groups);
    let mut l = level(&p, &["#..#"]);
    with_overrides(
        &p,
        &mut l,
        &[([1, 0], Override::Value(1)), ([0, 0], Override::Suppress)],
    );
    let b = bake(&p, &l);
    // Computed marks: both `#` cells (overrides don't change what is
    // computed); effective: (1, 0) and (3, 0).
    assert_eq!(marks(&b), vec![[0, 0], [3, 0]]);
    assert_eq!(fired_at(&b, "plus"), vec![[1, 0], [3, 0]]);
    // A suppressed cell reads as empty: never `+`; `!p` matches it like any
    // empty cell (an empty cell isn't `pillar_base`).
    assert_eq!(fired_at(&b, "not_p"), vec![[0, 0], [2, 0]]);
    let dots = worlds_rules::rule_matches(&p, &l, &slug("auto"), "plus", &images()).unwrap();
    assert!(!dots.contains_key(&[0, 0]));
    // rule_matches_all, preview, BakeCache and the export agree.
    let all = worlds_rules::rule_matches_all(&p, &l, &slug("auto"), &images()).unwrap();
    let plus = &all.iter().find(|(s, _)| s == "plus").unwrap().1;
    assert_eq!(
        plus.keys().copied().collect::<Vec<_>>(),
        vec![[1, 0], [3, 0]]
    );
    let mut p2 = p.clone();
    p2.worlds[0].levels.push(l.clone());
    let mut cache = worlds_rules::BakeCache::new();
    cache.bake_level(&p2, "room", &images()).unwrap();
    assert_eq!(cache.get("room", "auto").unwrap(), &b);
    let eff = cache.derived("room", "marks").unwrap();
    assert_eq!(eff.cells(), &[0, 1, 0, 1]);
    let baked = worlds_rules::bake_for_export(&p2, &l, &images()).unwrap();
    assert_eq!(baked.derived[&slug("marks")].cells(), &[0, 1, 0, 1]);
    assert_eq!(
        worlds_rules::effective_grid(&p2, &l, "marks", &images())
            .unwrap()
            .cells(),
        &[0, 1, 0, 1]
    );
    // A region rule on the derived grid reads it by effective value.
    let p3 = project(json!([
        { "slug": "w", "rules": [
            { "slug": "write", "match": { "rows": ["#"] }, "write": { "grid": "marks", "value": "pillar_base" } } ] },
        { "slug": "reg", "pass": 1, "rules": [
            { "slug": "r", "region": { "grid": "marks", "cells": "+" }, "place": { "across": "interior" },
              "out": { "tileset": "main", "tiles": [[2, 0]] } } ] }
    ]));
    let b3 = bake(&p3, &l);
    assert_eq!(fired_at(&b3, "r"), vec![[1, 0], [3, 0]]);
    let rp = worlds_rules::region_preview(&p3, &l, &slug("auto"), "r", &images())
        .unwrap()
        .unwrap();
    assert_eq!(
        rp.iter().map(|r| r.bbox).collect::<Vec<_>>(),
        vec![[1, 0, 1, 1], [3, 0, 1, 1]]
    );
}

#[test]
fn derived_grid_nobody_writes_reads_its_overrides() {
    let groups = json!([{ "slug": "g", "rules": [
        { "slug": "on_p", "match": { "grid": "marks", "rows": ["p"] },
          "out": { "tileset": "main", "tiles": [[2, 0]] } } ] }]);
    let p = project(groups);
    let mut l = level(&p, &["...."]);
    with_overrides(&p, &mut l, &[([2, 0], Override::Value(1))]);
    assert_eq!(fired_at(&bake(&p, &l), "on_p"), vec![[2, 0]]);
    let mut p2 = p.clone();
    p2.worlds[0].levels.push(l.clone());
    let baked = worlds_rules::bake_for_export(&p2, &l, &images()).unwrap();
    assert_eq!(baked.derived[&slug("marks")].cells(), &[0, 0, 1, 0]);
}

/// Two auto layers: `reader` (listed first) reads `marks`, which `auto`
/// writes from collision.
fn two_layers() -> Project {
    let mut p = project(json!([{ "slug": "w", "rules": [
        { "slug": "write", "match": { "rows": ["#"] }, "write": { "grid": "marks", "value": "pillar_base" } } ] }]));
    p.layers.insert(
        1,
        LayerDef::new(
            slug("reader"),
            "Reader",
            LayerKind::Auto(AutoDef::new(slug("collision"))),
        ),
    );
    p.rules.push(RuleSet {
        layer: slug("reader"),
        stamps: vec![],
        groups: serde_json::from_value(json!([{ "slug": "g", "rules": [
            { "slug": "on_mark", "match": { "grid": "marks", "rows": ["p"] },
              "out": { "tileset": "main", "tiles": [[2, 0]] } } ] }]))
        .unwrap(),
    });
    p
}

#[test]
fn cross_layer_chain_bakes_writers_first() {
    let p = two_layers();
    let mut l = level(&p, &["#.#."]);
    with_overrides(
        &p,
        &mut l,
        &[([3, 0], Override::Value(1)), ([2, 0], Override::Suppress)],
    );
    let all = worlds_rules::bake_level(&p, &l, &images()).unwrap();
    assert_eq!(
        fired_at(&all[&slug("reader")], "on_mark"),
        vec![[0, 0], [3, 0]]
    );
    // bake_layer of the reader alone bakes the writer first.
    let alone = worlds_rules::bake_layer(&p, &l, &slug("reader"), &images()).unwrap();
    assert_eq!(alone, all[&slug("reader")]);
    // Previews too.
    let pv = worlds_rules::preview_rules(&p, &l, &slug("reader"), &images()).unwrap();
    assert_eq!(pv[0].fired, vec![[0, 0], [3, 0]]);
}

#[test]
fn derived_cycle_and_second_writer_are_errors() {
    // reader writes marks2, auto reads marks2 and writes marks: a cycle.
    let mut p = two_layers();
    p.layers.insert(2, marks_def("marks2"));
    p.rules[1].groups[0].rules.push(
        serde_json::from_value::<Rule>(json!({ "slug": "w2", "match": { "rows": ["#"] },
            "write": { "grid": "marks2", "value": "pillar_base" } }))
        .unwrap()
        .into(),
    );
    p.rules[0].groups[0].rules.push(
        serde_json::from_value::<Rule>(
            json!({ "slug": "r2", "match": { "grid": "marks2", "rows": ["p"] },
            "out": { "tileset": "main", "tiles": [[2, 0]] } }),
        )
        .unwrap()
        .into(),
    );
    let l = level(&p, &["#."]);
    let e = worlds_rules::bake_level(&p, &l, &images()).unwrap_err();
    assert!(matches!(e, Error::DerivedCycle(_)), "{e}");
    assert!(
        e.to_string().contains("read each other's derived grids"),
        "{e}"
    );
    let e = worlds_rules::bake_layer(&p, &l, &slug("reader"), &images()).unwrap_err();
    assert!(matches!(e, Error::DerivedCycle(_)), "{e}");

    // Two layers writing marks.
    let mut p = two_layers();
    p.rules[1].groups[0].rules.push(
        serde_json::from_value::<Rule>(json!({ "slug": "w2", "match": { "rows": ["#"] },
            "write": { "grid": "marks", "value": "pillar_base" } }))
        .unwrap()
        .into(),
    );
    let e = worlds_rules::bake_layer(&p, &l, &slug("auto"), &images()).unwrap_err();
    assert!(matches!(e, Error::SecondWriter { .. }), "{e}");

    // A write to a layer that isn't derived, or to an unknown value.
    let p = project(one_group(json!({ "slug": "w", "match": { "rows": ["#"] },
        "write": { "grid": "collision", "value": "solid" } })));
    let e = worlds_rules::bake_layer(&p, &l, &slug("auto"), &images()).unwrap_err();
    assert!(matches!(e, Error::BadWrite { .. }), "{e}");
    let p = project(one_group(json!({ "slug": "w", "match": { "rows": ["#"] },
        "write": { "grid": "marks", "value": "nope" } })));
    let e = worlds_rules::bake_layer(&p, &l, &slug("auto"), &images()).unwrap_err();
    assert!(matches!(e, Error::BadWrite { .. }), "{e}");
    // A region on a layer that isn't an int_grid.
    let p = project(one_group(
        json!({ "slug": "r", "region": { "grid": "auto", "cells": "." },
        "place": { "along": "floor" }, "write": { "grid": "marks", "value": "pillar_base" } }),
    ));
    let e = worlds_rules::bake_layer(&p, &l, &slug("auto"), &images()).unwrap_err();
    assert!(matches!(e, Error::BadRegion { .. }), "{e}");
}

// ---- requires_level_tag, require lists ----

#[test]
fn requires_level_tag() {
    let tagged = |tags: Value, mode: &str, gmode: &str| {
        project(json!([{ "slug": "g", "mode": gmode,
            "requires_level_tag": { "tags": tags, "mode": mode },
            "rules": [{ "slug": "r", "match": { "rows": ["#"] },
                        "out": { "tileset": "main", "tiles": [[2, 0]] } }] }]))
    };
    let runs = |p: &Project, tags: &[&str]| {
        let mut l = level(p, &["#"]);
        l.tags = tags.iter().map(|s| s.to_string()).collect();
        !bake(p, &l).placements.is_empty()
    };
    let any = tagged(json!(["ruined", "flood"]), "any", "always");
    assert!(runs(&any, &["flood"]));
    assert!(!runs(&any, &[]));
    let all = tagged(json!(["ruined", "flood"]), "all", "always");
    assert!(!runs(&all, &["flood"]));
    assert!(runs(&all, &["flood", "ruined", "x"]));
    // A holding tag runs an optional group without listing it; `off` never.
    let opt = tagged(json!(["ruined"]), "any", "optional");
    assert!(runs(&opt, &["ruined"]));
    assert!(!runs(&opt, &["other"]));
    let off = tagged(json!(["ruined"]), "any", "off");
    assert!(!runs(&off, &["ruined"]));
}

#[test]
fn require_list_all_must_hold() {
    let p = project(one_group(json!({ "slug": "r", "match": { "rows": ["#"] },
        "require": [ { "rows": [".", "@"] }, { "rows": ["@", "~"] } ],
        "symmetry": ["mirror_y"],
        "out": { "tileset": "main", "tiles": [[2, 0]] } })));
    // (0, 1): empty above, water below. (1, 1): empty above, nothing below.
    let l = level(&p, &["..", "##", "~."]);
    let b = bake(&p, &l);
    assert_eq!(fired_at(&b, "r"), vec![[0, 1]]);
    // Symmetry turns all of them: flipped, water above and empty below.
    let l = level(&p, &["~.", "##", ".."]);
    let b = bake(&p, &l);
    assert_eq!(fired_at(&b, "r"), vec![[0, 1]]);
    assert_eq!(b.placements[0].flip, Some(Flip::Y));
}

// ---- export ----

#[test]
fn export_warnings_and_derived_export() {
    use worlds_core::export::{BakedLevel, BakedTile};
    let p = two_layers();
    let mut p2 = p.clone();
    let l = level(&p, &["#.#."]);
    p2.worlds[0].levels.push(l.clone());
    let baked = worlds_rules::bake_for_export(&p2, &l, &images()).unwrap();
    assert_eq!(baked.derived[&slug("marks")].cells(), &[1, 0, 1, 0]);
    let mut all = BTreeMap::new();
    all.insert(slug("room"), baked.clone());
    // Only `reader` has tiles: no warning.
    assert!(worlds_rules::export_warnings(&p2, &all).is_empty());
    // Two auto layers with tiles, one off grid and one transposed: the M7
    // importer reads all three, so still no warning.
    let tile = |px: [i32; 2], flip| BakedTile {
        px,
        tileset: slug("main"),
        tile: [0, 0],
        flip,
        alpha: 1.0,
    };
    let mut b2: BakedLevel = baked;
    b2.tiles.insert(
        slug("auto"),
        vec![
            tile([8, 0], None),
            tile([16, 0], Some(Flip::T)),
            tile([3, 16], None),
        ],
    );
    all.insert(slug("room"), b2);
    assert_eq!(
        worlds_rules::export_warnings(&p2, &all),
        Vec::<String>::new()
    );
    // The export writes the effective grid.
    let dir = tempfile::tempdir().unwrap();
    let mut map = BTreeMap::new();
    map.insert(
        slug("room"),
        worlds_rules::bake_for_export(&p2, &l, &images()).unwrap(),
    );
    worlds_core::export::write(&p2, dir.path(), &dir.path().join("export"), &map, true).unwrap();
    let text = std::fs::read_to_string(dir.path().join("export/levels/room.json")).unwrap();
    let v: Value = serde_json::from_str(&text).unwrap();
    let marks = &v["int_grids"]["marks"];
    assert_eq!(marks["derived"], json!(true));
    assert_eq!(marks["rows"], json!(["p.p."]));
}

#[test]
fn group_entry_from_rule() {
    // RuleGroup JSON with pass round-trips (helper sanity).
    let g: RuleGroup =
        serde_json::from_value(json!({ "slug": "g", "pass": 3, "rules": [] })).unwrap();
    assert_eq!(g.pass, 3);
}
