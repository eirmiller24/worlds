//! Bake benchmarks (`cargo bench -p worlds-rules`). Numbers go in the
//! README's "Benchmarks" section.
//!
//! - `stroke_64x28_50_rules`: a 20-cell paint stroke through
//!   `Project::apply` and `BakeCache::apply_changes` on a 64x28 room with 50
//!   rules (the DESIGN target: under 2 ms).
//! - `blob47_64x28`: a full bake of a `blob47` reference on a 64x28 mask.
//! - `rebake_500_levels`: full bake of 500 synthetic 64x28 levels with the
//!   50 rules, sequentially on one thread (DESIGN: under 1 s for all of them
//!   on the worker pool).
//! - `stroke_64x28_50_rules_m6`: `stroke_64x28_50_rules` with 46 of the 50
//!   rules in pass 0, two region rules (floor, interior) writing a derived
//!   grid, and two pass-1 stamp rules with `occupies`/`avoid_overlap`
//!   reading it (target: under 2 ms).

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use worlds_core::command::{CellValue, Command};
use worlds_core::{
    AutoDef, Color, IntGridDef, IntGridGroup, IntGridLayer, IntGridValue, LayerDef, LayerKind,
    Level, Project, RuleSet, TilesetDef, slug,
};
use worlds_rules::{BakeCache, TileOpacity, TilesetImages};

/// xorshift64*, so the synthetic levels are the same every run.
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d) % n
    }
}

/// `collision` (`#` solid, `f` fortified in group `walls`, `~` water), an
/// auto layer `auto` on it, tileset `main` (16 px), with `rules` as its
/// only group.
fn synthetic(rules: serde_json::Value) -> Project {
    let mut p = Project::new("bench");
    p.seed = 7;
    let value = |s: &str, g: char, group: Option<&str>| IntGridValue {
        slug: slug(s),
        name: s.into(),
        color: Color::new("#808080").unwrap(),
        glyph: g,
        group: group.map(slug),
    };
    p.layers = vec![
        LayerDef::new(
            slug("collision"),
            "Collision",
            LayerKind::IntGrid(IntGridDef {
                values: vec![
                    value("solid", '#', Some("walls")),
                    value("fortified", 'f', Some("walls")),
                    value("water", '~', None),
                ],
                groups: vec![IntGridGroup {
                    slug: slug("walls"),
                    name: "Walls".into(),
                    color: None,
                    glyph: None,
                }],
                derived: false,
            }),
        ),
        LayerDef::new(
            slug("auto"),
            "Auto",
            LayerKind::Auto(AutoDef::new(slug("collision"))),
        ),
    ];
    p.tilesets = vec![TilesetDef {
        slug: slug("main"),
        image: "main.png".into(),
        tile_size: [16, 16],
        spacing: 0,
        padding: 0,
    }];
    p.rules = vec![RuleSet {
        layer: slug("auto"),
        stamps: vec![],
        groups: vec![
            serde_json::from_value(serde_json::json!({ "slug": "g", "rules": rules })).unwrap(),
        ],
    }];
    p
}

/// 50 rules of 3x3 patterns with both mirrors, two alternatives, chance
/// 0.9 (as `tests/rules.rs`' timing check), a few with `rotate`.
fn fifty_rules() -> serde_json::Value {
    let rules: Vec<serde_json::Value> = (0..50)
        .map(|i| {
            let rows = match i % 3 {
                0 => ["...", ".#.", "###"],
                1 => ["#..", "##.", "###"],
                _ => ["*+*", "+.+", "*+*"],
            };
            let symmetry = if i % 10 == 9 {
                vec!["rotate", "mirror_x"]
            } else {
                vec!["mirror_x", "mirror_y"]
            };
            serde_json::json!({
                "slug": format!("r{i}"), "match": { "rows": rows }, "symmetry": symmetry,
                "out": { "tileset": "main", "tiles": [[1, 1], [2, 1]] }, "chance": 0.9
            })
        })
        .collect();
    serde_json::Value::Array(rules)
}

/// A `w` x `h` room: random walls, a solid floor.
fn room(p: &Project, name: &str, rng: &mut Rng, w: usize, h: usize) -> Level {
    let rows: Vec<String> = (0..h)
        .map(|y| {
            (0..w)
                .map(|_| match rng.below(10) {
                    _ if y + 3 >= h => '#',
                    0..=2 => '#',
                    3 => 'f',
                    4 => '~',
                    _ => '.',
                })
                .collect()
        })
        .collect();
    let rows: Vec<&str> = rows.iter().map(String::as_str).collect();
    let g = IntGridLayer::from_rows(p.layer("collision").unwrap(), &rows).unwrap();
    let mut l = Level::new(slug(name), name, g.size());
    l.int_grids = vec![g];
    l
}

fn images() -> TilesetImages {
    let imgs = TilesetImages::in_memory();
    let mut flags = vec![false; 64 * 64];
    flags[0] = true;
    imgs.insert(slug("main"), TileOpacity::from_flags(64, 64, flags));
    imgs
}

fn stroke(c: &mut Criterion) {
    let mut p = synthetic(fifty_rules());
    let mut rng = Rng(1);
    let l = room(&p, "room", &mut rng, 64, 28);
    p.worlds[0].levels.push(l);
    let images = images();
    let mut cache = BakeCache::new();
    cache.bake_level(&p, "room", &images).unwrap();
    let mut on = false;
    c.bench_function("stroke_64x28_50_rules", |b| {
        b.iter(|| {
            // A 20-cell diagonal-ish stroke, alternately painted and erased
            // so every iteration changes the grid.
            on = !on;
            let v = on.then(|| slug("solid"));
            let cells = (0..20)
                .map(|i| CellValue::new(20 + i, 10 + i / 7, v.clone()))
                .collect();
            let a = p
                .apply(Command::PaintCells {
                    level: slug("room"),
                    layer: slug("collision"),
                    cells,
                    keep_empty: false,
                })
                .unwrap();
            black_box(
                cache
                    .apply_changes(&p, "room", &images, &a.changes)
                    .unwrap(),
            )
        })
    });
}

fn blob47(c: &mut Criterion) {
    let p = synthetic(serde_json::json!([
        { "use": "blob47", "slug": "moss", "tileset": "main", "origin": [0, 8],
          "legend": { "#": "walls", "S": "!walls" } }
    ]));
    let mut rng = Rng(2);
    let l = room(&p, "room", &mut rng, 64, 28);
    let images = images();
    let layer = slug("auto");
    c.bench_function("blob47_64x28", |b| {
        b.iter(|| black_box(worlds_rules::bake_layer(&p, &l, &layer, &images).unwrap()))
    });
}

fn rebake_500(c: &mut Criterion) {
    let p = synthetic(fifty_rules());
    let mut rng = Rng(3);
    let levels: Vec<Level> = (0..500)
        .map(|i| room(&p, &format!("l{i}"), &mut rng, 64, 28))
        .collect();
    let images = images();
    let layer = slug("auto");
    let mut g = c.benchmark_group("rebake");
    g.sample_size(10);
    g.bench_function("rebake_500_levels", |b| {
        b.iter(|| {
            for l in &levels {
                black_box(worlds_rules::bake_layer(&p, l, &layer, &images).unwrap());
            }
        })
    });
    g.finish();
}

/// The synthetic project of `stroke` with M6 features: 46 of the 50 rules
/// in pass 0, two region rules writing derived grid `marks`, two pass-1
/// stamps reading it.
fn synthetic_m6() -> Project {
    let mut rules = fifty_rules();
    rules.as_array_mut().unwrap().truncate(46);
    let mut p = synthetic(rules);
    p.layers.insert(
        1,
        serde_json::from_value(serde_json::json!({
            "slug": "marks", "name": "Marks", "kind": "int_grid", "derived": true,
            "values": [
                { "slug": "a", "name": "A", "color": "#E0A040", "glyph": "a" },
                { "slug": "b", "name": "B", "color": "#40A0E0", "glyph": "b" } ]
        }))
        .unwrap(),
    );
    let set = &mut p.rules[0];
    set.stamps = serde_json::from_value(serde_json::json!([
        { "slug": "big", "tileset": "main", "rect": [3, 3, 2, 3], "pivot": [0, 1], "occupies": true },
        { "slug": "small", "tileset": "main", "rect": [6, 6, 1, 1], "occupies": true }
    ]))
    .unwrap();
    let extra: Vec<worlds_core::RuleGroup> = serde_json::from_value(serde_json::json!([
        { "slug": "marks", "rules": [
            { "slug": "floor_marks", "region": { "cells": "." },
              "place": { "along": "floor", "every": 4, "margin": 1 },
              "write": { "grid": "marks", "value": "a" } },
            { "slug": "inner_marks", "region": { "cells": ".", "min_area": 4 },
              "place": { "across": "interior", "every": [5, 3] },
              "write": { "grid": "marks", "value": "b" } } ] },
        { "slug": "stamps", "pass": 1, "rules": [
            { "slug": "stamp_a", "match": { "grid": "marks", "rows": ["a"] },
              "out": { "stamp": "big", "avoid_overlap": true } },
            { "slug": "stamp_b", "match": { "grid": "marks", "rows": ["b"] },
              "out": { "stamp": "small", "avoid_overlap": true } } ] }
    ]))
    .unwrap();
    set.groups.extend(extra);
    p
}

fn stroke_m6(c: &mut Criterion) {
    let mut p = synthetic_m6();
    let mut rng = Rng(1);
    let l = room(&p, "room", &mut rng, 64, 28);
    p.worlds[0].levels.push(l);
    let images = images();
    let mut cache = BakeCache::new();
    cache.bake_level(&p, "room", &images).unwrap();
    let mut on = false;
    c.bench_function("stroke_64x28_50_rules_m6", |b| {
        b.iter(|| {
            on = !on;
            let v = on.then(|| slug("solid"));
            let cells = (0..20)
                .map(|i| CellValue::new(20 + i, 10 + i / 7, v.clone()))
                .collect();
            let a = p
                .apply(Command::PaintCells {
                    level: slug("room"),
                    layer: slug("collision"),
                    cells,
                    keep_empty: false,
                })
                .unwrap();
            black_box(
                cache
                    .apply_changes(&p, "room", &images, &a.changes)
                    .unwrap(),
            )
        })
    });
}

criterion_group!(benches, stroke, blob47, rebake_500, stroke_m6);
criterion_main!(benches);
