//! `BakeCache` against full bakes: random edit sequences, applied through
//! real commands, on the LDtk samples and a two-level game built from one.
//! After every update the cache must equal `bake_layer` exactly, and every placement
//! that changed must lie inside the reported dirty rect.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use worlds_core::command::{CellValue, Change, Command, ProjectSetting, Rect};
use worlds_core::{LayerState, Project, Slug, ldtk};
use worlds_rules::{BakeCache, DirtyRect, LayerBake, Placement, TileOpacity, TilesetImages};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

/// xorshift64*: deterministic, no dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next() % n }
    }
    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len() as u64) as usize]
    }
}

fn sample(name: &str) -> (Project, TilesetImages) {
    let samples = fixtures().join("ldtk-samples");
    let imported = ldtk::import(&samples.join(name), &samples).unwrap();
    let images = TilesetImages::new(&samples, &imported.project.tilesets);
    (imported.project, images)
}

/// A two-level platformer built from `AutoLayers_1_basic`: its IntGrid
/// layer renamed `Collision` (so its rules land on `collision_tiles`) and
/// its level copied as `Crevasse` and `Level_1`.
fn game() -> (Project, TilesetImages) {
    let samples = fixtures().join("ldtk-samples");
    let src = samples.join("AutoLayers_1_basic.ldtk");
    let mut v: serde_json::Value = serde_json::from_slice(&std::fs::read(&src).unwrap()).unwrap();
    for l in v["defs"]["layers"].as_array_mut().unwrap() {
        if l["identifier"] == "IntGrid_layer" {
            l["identifier"] = "Collision".into();
        }
    }
    let level = v["levels"][0].clone();
    let levels = v["levels"].as_array_mut().unwrap();
    levels.clear();
    for (i, name) in ["Crevasse", "Level_1"].into_iter().enumerate() {
        let mut l = level.clone();
        let uid = 90_000 + i as u64;
        l["identifier"] = name.into();
        l["uid"] = uid.into();
        l["iid"] = format!("level-{i}").into();
        for li in l["layerInstances"].as_array_mut().unwrap() {
            li["levelId"] = uid.into();
            li["iid"] = format!("layer-{i}").into();
        }
        levels.push(l);
    }
    // Next to the samples, so the atlas paths resolve.
    let tmp = tempfile::Builder::new()
        .prefix(".game")
        .suffix(".ldtk")
        .tempfile_in(&samples)
        .unwrap();
    std::fs::write(tmp.path(), serde_json::to_vec(&v).unwrap()).unwrap();
    let imported = ldtk::import(tmp.path(), &samples).unwrap();
    let images = TilesetImages::new(&samples, &imported.project.tilesets);
    let mut p = imported.project;
    p.seed = 42;
    (p, images)
}

fn auto_layers(p: &Project) -> Vec<Slug> {
    p.layers
        .iter()
        .filter(|l| l.is_auto())
        .map(|l| l.slug.clone())
        .collect()
}

fn full(p: &Project, level: &str, images: &TilesetImages) -> BTreeMap<Slug, LayerBake> {
    let l = p.level(level).unwrap();
    worlds_rules::bake_level(p, l, images).unwrap()
}

/// The cells a placement draws over (as the cache computes them).
fn drawn(p: &Project, pl: &Placement, g: i32) -> DirtyRect {
    let ts = p.tileset(pl.tileset.as_str()).unwrap().tile_size;
    DirtyRect {
        min: [pl.px[0].div_euclid(g), pl.px[1].div_euclid(g)],
        max: [
            (pl.px[0] + ts[0] as i32 + g - 1).div_euclid(g),
            (pl.px[1] + ts[1] as i32 + g - 1).div_euclid(g),
        ],
    }
}

fn within(inner: DirtyRect, outer: DirtyRect) -> bool {
    inner.min[0] >= outer.min[0]
        && inner.min[1] >= outer.min[1]
        && inner.max[0] <= outer.max[0]
        && inner.max[1] <= outer.max[1]
}

/// Placements in `a` but not in `b`, as multisets.
fn missing<'a>(a: &'a [Placement], b: &[Placement]) -> Vec<&'a Placement> {
    let mut left: Vec<Option<&Placement>> = b.iter().map(Some).collect();
    let mut out = Vec::new();
    for p in a {
        match left.iter_mut().find(|x| x.is_some_and(|x| x == p)) {
            Some(slot) => *slot = None,
            None => out.push(p),
        }
    }
    out
}

/// One random edit as a command.
fn random_edit(rng: &mut Rng, p: &Project, level: &str) -> Command {
    let l = p.level(level).unwrap();
    let grids: Vec<&worlds_core::LayerDef> = p.layers.iter().filter(|d| d.is_int_grid()).collect();
    let def = *rng.pick(&grids);
    let [w, h] = p.layer_cells(def, l);
    let mut values: Vec<Option<Slug>> = def.values().iter().map(|v| Some(v.slug.clone())).collect();
    values.push(None);
    values.push(None);
    let value = |rng: &mut Rng| rng.pick(&values).clone();
    let (lv, layer) = (Slug::new(level).unwrap(), def.slug.clone());
    match rng.below(20) {
        0..=10 => {
            // A short stroke: a few cells near each other.
            let (x0, y0) = (rng.below(w as u64) as u32, rng.below(h as u64) as u32);
            let n = 1 + rng.below(6);
            let v = value(rng);
            let cells = (0..n)
                .map(|_| {
                    let x = (x0 + rng.below(4) as u32).min(w - 1);
                    let y = (y0 + rng.below(3) as u32).min(h - 1);
                    CellValue::new(x, y, v.clone())
                })
                .collect();
            Command::PaintCells {
                level: lv,
                layer,
                cells,
                keep_empty: false,
            }
        }
        11..=13 => {
            let (x, y) = (rng.below(w as u64) as u32, rng.below(h as u64) as u32);
            let rw = 1 + rng.below((w - x).min(8) as u64) as u32;
            let rh = 1 + rng.below((h - y).min(5) as u64) as u32;
            Command::FillRect {
                level: lv,
                layer,
                rect: Rect::new(x, y, rw, rh),
                value: value(rng).into(),
            }
        }
        14 | 15 => Command::FloodFill {
            level: lv,
            layer,
            at: [rng.below(w as u64) as u32, rng.below(h as u64) as u32],
            value: value(rng).into(),
        },
        16 => Command::ReplaceValue {
            level: lv,
            layer,
            from: value(rng).into(),
            to: value(rng).into(),
        },
        17 => match auto_layers(p).first() {
            Some(a) => Command::SetLayerState {
                level: lv,
                layer: a.clone(),
                state: Some(LayerState {
                    seed: rng.below(1000) as i64,
                    ..l.layers.get(a).cloned().unwrap_or_default()
                }),
            },
            None => Command::noop(),
        },
        18 => Command::SetProjectSetting {
            setting: ProjectSetting::Seed(rng.below(1 << 20) as i64),
        },
        _ => Command::MoveLevel {
            world: p.worlds[0].slug.clone(),
            level: lv,
            world_px: [rng.below(100) as i64, 0],
        },
    }
}

fn run(name: &str, p: &mut Project, images: &TilesetImages, steps: usize, seed: u64) {
    let mut rng = Rng(seed);
    let levels: Vec<String> = p.levels().map(|l| l.slug.to_string()).collect();
    for level in &levels {
        let mut cache = BakeCache::new();
        cache.bake_level(p, level, images).unwrap();
        let mut prev = full(p, level, images);
        for (layer, bake) in &prev {
            assert_eq!(
                cache.get(level, layer.as_str()),
                Some(bake),
                "{name} {level} initial"
            );
        }
        let mut step = 0;
        while step < steps {
            // Sometimes several edits land before the cache sees them.
            let mut changes: Vec<Change> = Vec::new();
            for _ in 0..1 + rng.below(3) {
                let cmd = random_edit(&mut rng, p, level);
                if let Ok(a) = p.apply(cmd) {
                    changes.extend(a.changes);
                }
                step += 1;
            }
            let dirty: BTreeMap<Slug, DirtyRect> = cache
                .apply_changes(p, level, images, &changes)
                .unwrap()
                .into_iter()
                .collect();
            let now = full(p, level, images);
            for (layer, bake) in &now {
                let ctx = format!("{name} {level} {layer} step {step}");
                let got = cache.get(level, layer.as_str()).unwrap();
                if got != bake {
                    let keys: BTreeSet<&[i32; 2]> =
                        got.fired.keys().chain(bake.fired.keys()).collect();
                    let diff: Vec<String> = keys
                        .into_iter()
                        .filter(|k| got.fired.get(*k) != bake.fired.get(*k))
                        .take(8)
                        .map(|k| {
                            format!("{k:?}: {:?} vs {:?}", got.fired.get(k), bake.fired.get(k))
                        })
                        .collect();
                    panic!(
                        "{ctx}: cache differs from a full bake ({} vs {} placements); \
                         fired differs at {diff:?}",
                        got.placements.len(),
                        bake.placements.len()
                    );
                }
                let g = p.layer_grid(p.layer(layer.as_str()).unwrap()) as i32;
                let before = &prev[layer].placements;
                let changed: Vec<&Placement> = missing(before, &bake.placements)
                    .into_iter()
                    .chain(missing(&bake.placements, before))
                    .collect();
                if !changed.is_empty() {
                    let r = dirty
                        .get(layer)
                        .unwrap_or_else(|| panic!("{ctx}: no dirty rect"));
                    for pl in changed {
                        assert!(within(drawn(p, pl, g), *r), "{ctx}: {pl:?} outside {r:?}");
                    }
                }
            }
            prev = now;
        }
    }
}

#[test]
fn incremental_matches_full_on_ldtk_samples() {
    for (i, name) in [
        "AutoLayers_1_basic.ldtk",
        "AutoLayers_2_stamps.ldtk",
        "AutoLayers_3_Mosaic.ldtk",
        "AutoLayers_4_Assistant.ldtk",
        "AutoLayers_5_Advanced.ldtk",
        "AutoLayers_6_OptionalRules.ldtk",
        "AutoLayers_7_Biomes.ldtk",
        "Test_file_for_API_showing_all_features.ldtk",
    ]
    .iter()
    .enumerate()
    {
        let (mut p, images) = sample(name);
        run(name, &mut p, &images, 40, 0x9e37_79b9 + i as u64);
    }
}

#[test]
fn incremental_matches_full_on_a_two_level_game() {
    let (mut p, images) = game();
    run("game", &mut p, &images, 40, 7);
}

#[test]
fn renames_and_deletes_follow_the_level() {
    let (mut p, images) = game();
    let mut cache = BakeCache::new();
    cache.bake_level(&p, "level_1", &images).unwrap();
    let before = cache.get("level_1", "collision_tiles").unwrap().clone();
    let a = p
        .apply(Command::RenameLevel {
            world: p.worlds[0].slug.clone(),
            level: Slug::new("level_1").unwrap(),
            slug: Slug::new("hall").unwrap(),
            name: "Hall".into(),
        })
        .unwrap();
    let dirty = cache
        .apply_changes(&p, "hall", &images, &a.changes)
        .unwrap();
    assert!(dirty.is_empty(), "a rename changes no tiles");
    assert_eq!(cache.get("hall", "collision_tiles"), Some(&before));
    assert!(cache.get("level_1", "collision_tiles").is_none());
    let a = p
        .apply(Command::DeleteLevel {
            world: p.worlds[0].slug.clone(),
            level: Slug::new("hall").unwrap(),
        })
        .unwrap();
    assert!(
        cache
            .apply_changes(&p, "hall", &images, &a.changes)
            .unwrap()
            .is_empty()
    );
    assert!(cache.get("hall", "collision_tiles").is_none());
}

#[test]
fn edits_before_a_rename_in_the_same_batch_of_changes_count() {
    // Paint on level_1, rename it to hall, then rename hall to atrium, and
    // hand all the changes over at once under the final slug.
    let (mut p, images) = game();
    let mut cache = BakeCache::new();
    cache.bake_level(&p, "level_1", &images).unwrap();
    let world = p.worlds[0].slug.clone();
    let solid = p.layer("collision").unwrap().values()[0].slug.clone();
    let mut changes = Vec::new();
    let mut apply = |p: &mut Project, c: Command| changes.extend(p.apply(c).unwrap().changes);
    apply(
        &mut p,
        Command::PaintCells {
            level: Slug::new("level_1").unwrap(),
            layer: Slug::new("collision").unwrap(),
            cells: (2..8)
                .map(|x| CellValue::new(x, 3, Some(solid.clone())))
                .collect(),
            keep_empty: false,
        },
    );
    for (from, to) in [("level_1", "hall"), ("hall", "atrium")] {
        apply(
            &mut p,
            Command::RenameLevel {
                world: world.clone(),
                level: Slug::new(from).unwrap(),
                slug: Slug::new(to).unwrap(),
                name: to.into(),
            },
        );
    }
    let dirty = cache
        .apply_changes(&p, "atrium", &images, &changes)
        .unwrap();
    assert!(!dirty.is_empty(), "the paint before the renames is seen");
    let expected = full(&p, "atrium", &images);
    for layer in auto_layers(&p) {
        assert_eq!(cache.get("atrium", layer.as_str()), expected.get(&layer));
    }
    assert!(cache.get("level_1", "collision_tiles").is_none());
    assert!(cache.get("hall", "collision_tiles").is_none());
}

#[test]
fn an_uncached_level_is_only_baked_when_a_change_can_affect_it() {
    let (mut p, images) = game();
    let mut cache = BakeCache::new();
    // A level position change can't affect a bake: nothing is baked.
    let a = p
        .apply(Command::MoveLevel {
            world: p.worlds[0].slug.clone(),
            level: Slug::new("crevasse").unwrap(),
            world_px: [-4096, -4096],
        })
        .unwrap();
    let dirty = cache
        .apply_changes(&p, "crevasse", &images, &a.changes)
        .unwrap();
    assert!(dirty.is_empty());
    assert!(cache.get("crevasse", "collision_tiles").is_none());
    // A paint on the source layer bakes the uncached layer in full.
    let solid = p.layer("collision").unwrap().values()[0].slug.clone();
    let a = p
        .apply(Command::PaintCells {
            level: Slug::new("crevasse").unwrap(),
            layer: Slug::new("collision").unwrap(),
            cells: vec![CellValue::new(5, 5, Some(solid))],
            keep_empty: false,
        })
        .unwrap();
    let dirty = cache
        .apply_changes(&p, "crevasse", &images, &a.changes)
        .unwrap();
    assert!(!dirty.is_empty());
    assert_eq!(
        cache.get("crevasse", "collision_tiles"),
        full(&p, "crevasse", &images).get("collision_tiles")
    );
}

#[test]
fn a_seeded_cache_updates_like_a_baked_one() {
    // Seed from a bake made elsewhere (as the editor's worker threads do),
    // then edit: the result must match a full bake, and the seed must be
    // the same Arc (no copy).
    for (name, (mut p, images)) in [
        ("game", game()),
        ("AutoLayers_1_basic", sample("AutoLayers_1_basic.ldtk")),
    ] {
        let level = p.levels().next().unwrap().slug.to_string();
        let mut cache = BakeCache::new();
        for (layer, bake) in full(&p, &level, &images) {
            let bake = std::sync::Arc::new(bake);
            assert!(
                cache
                    .seed(&p, &level, layer.as_str(), bake.clone())
                    .unwrap(),
                "{name} {layer}"
            );
            assert!(std::sync::Arc::ptr_eq(
                cache.get_shared(&level, layer.as_str()).unwrap(),
                &bake
            ));
        }
        let mut rng = Rng(11);
        for _ in 0..15 {
            let cmd = random_edit(&mut rng, &p, &level);
            let a = p.apply(cmd).unwrap();
            cache
                .apply_changes(&p, &level, &images, &a.changes)
                .unwrap();
        }
        let expected = full(&p, &level, &images);
        for layer in auto_layers(&p) {
            assert_eq!(
                cache.get(&level, layer.as_str()),
                expected.get(&layer),
                "{name} {layer}"
            );
        }
        // A bake that doesn't fit (a tile from a rule the layer doesn't
        // have) is refused.
        let (layer, mut bad) = full(&p, &level, &images).into_iter().next().unwrap();
        let mut stray = bad.placements[0].clone();
        stray.rule = Slug::new("no_such_rule").unwrap();
        bad.placements.push(stray);
        let mut fresh = BakeCache::new();
        assert!(
            !fresh
                .seed(&p, &level, layer.as_str(), std::sync::Arc::new(bad))
                .unwrap()
        );
        assert!(fresh.get(&level, layer.as_str()).is_none());
    }
}

#[test]
fn placements_at_indexes_by_anchor() {
    let (p, images) = game();
    let mut cache = BakeCache::new();
    cache.bake_level(&p, "crevasse", &images).unwrap();
    let bake = cache.get("crevasse", "collision_tiles").unwrap();
    let anchor = bake.placements[bake.placements.len() / 2].anchor;
    let expected: Vec<&Placement> = bake
        .placements
        .iter()
        .filter(|q| q.anchor == anchor)
        .collect();
    assert!(!expected.is_empty());
    assert_eq!(
        cache.placements_at("crevasse", "collision_tiles", anchor),
        expected
    );
}

#[test]
fn stroke_rebake_timing() {
    // A 20-cell stroke on crevasse (37x26, 11 rules). Release
    // target: under 1 ms per update; debug builds get a loose bound.
    let (mut p, images) = game();
    let mut cache = BakeCache::new();
    cache.bake_level(&p, "crevasse", &images).unwrap();
    let solid = p.layer("collision").unwrap().values()[0].slug.clone();
    let mut best = f64::MAX;
    for round in 0..10u32 {
        let v = (round % 2 == 0).then(|| solid.clone());
        let cells = (0..20)
            .map(|i| CellValue::new(8 + i, 10 + (i / 7), v.clone()))
            .collect();
        let a = p
            .apply(Command::PaintCells {
                level: Slug::new("crevasse").unwrap(),
                layer: Slug::new("collision").unwrap(),
                cells,
                keep_empty: false,
            })
            .unwrap();
        let t = std::time::Instant::now();
        let dirty = cache
            .apply_changes(&p, "crevasse", &images, &a.changes)
            .unwrap();
        best = best.min(t.elapsed().as_secs_f64() * 1000.0);
        assert_eq!(dirty.len(), 1);
    }
    let t = std::time::Instant::now();
    let full_bake = full(&p, "crevasse", &images);
    let full_ms = t.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(
        cache.get("crevasse", "collision_tiles"),
        full_bake.get("collision_tiles")
    );
    eprintln!("20-cell stroke on crevasse: {best:.3} ms (full bake {full_ms:.3} ms)");
    let limit = if cfg!(debug_assertions) { 100.0 } else { 1.0 };
    assert!(best < limit, "incremental rebake took {best:.3} ms");
}

// ---- rule, macro and tileset edits ----

mod common;

use worlds_core::{AutoDef, IntGridDef, IntGridValue, LayerDef, LayerKind, RuleEntry, slug};

/// `common::project()` plus an int_grid `decor` (`t`, `b`), a second auto
/// layer `auto2` drawing only from `alt`, macros (a named pattern and
/// legend, a template) and rules using all of it: a template reference,
/// `require` on decor, `rotate`, chance with alternatives, a named stamp.
fn rules_project() -> (Project, TilesetImages) {
    let mut p = common::project();
    let value = |s: &str, g: char| IntGridValue {
        slug: slug(s),
        name: s.into(),
        color: worlds_core::Color::new("#808080").unwrap(),
        glyph: g,
        group: None,
    };
    p.layers.insert(
        1,
        LayerDef::new(
            slug("decor"),
            "Decor",
            LayerKind::IntGrid(IntGridDef {
                values: vec![value("torch", 't'), value("banner", 'b')],
                groups: vec![],
                derived: false,
            }),
        ),
    );
    p.layers.push(LayerDef::new(
        slug("auto2"),
        "Auto 2",
        LayerKind::Auto(AutoDef::new(slug("collision"))),
    ));
    p.macros = Some(
        serde_json::from_value(serde_json::json!({
            "patterns": { "edge": ["*.*", "*#*", "***"] },
            "legends": { "walls": { "#": "walls", "S": "!walls" } },
            "templates": { "frame": { "legend": "walls", "rules": [
                { "slug": "top", "match": { "rows": "edge", "legend": { ".": "!walls" } },
                  "out": { "tiles": [[1, 0], [2, 0]] }, "break": true },
                { "slug": "corner", "match": { "rows": ["S#*", "##*", "***"] },
                  "symmetry": ["rotate"], "out": { "tiles": [[3, 0]] } },
                { "slug": "fill", "match": { "rows": ["#"] }, "out": { "tiles": [[1, 1]] },
                  "chance": 0.8 }
            ] } }
        }))
        .unwrap(),
    );
    let group =
        |v: serde_json::Value| -> worlds_core::RuleGroup { serde_json::from_value(v).unwrap() };
    p.rules[0].groups = vec![
        group(serde_json::json!({ "slug": "a", "rules": [
            { "slug": "torch", "match": { "legend": { "@": "." }, "rows": ["#", "@"] },
              "require": { "grid": "decor", "rows": ["t"] },
              "out": { "tileset": "main", "tiles": [[6, 6]] }, "break": true },
            { "use": "frame", "slug": "w", "tileset": "main", "origin": [0, 2] },
            { "slug": "rot", "match": { "rows": ["@~", "*f"], "legend": { "@": "+" } },
              "symmetry": ["rotate", "mirror_x"],
              "out": { "tileset": "main", "tiles": [[4, 0, 2, 1]], "tile_mode": "stamp" } }
        ] })),
        group(serde_json::json!({ "slug": "b", "rules": [
            { "slug": "pillars", "match": { "rows": ["...", ".+.", "*+*"] },
              "out": { "stamp": "pillar" }, "chance": 0.5 }
        ] })),
    ];
    p.rules.push(worlds_core::RuleSet {
        layer: slug("auto2"),
        stamps: vec![],
        groups: vec![group(serde_json::json!({ "slug": "c", "rules": [
            { "slug": "edge2", "match": { "rows": ["*.*", "*#*", "***"] },
              "symmetry": ["mirror_y"], "out": { "tileset": "alt", "tiles": [[2, 2], [3, 2]] } },
            { "slug": "fill2", "match": { "rows": ["+"] },
              "out": { "tileset": "alt", "tiles": [[0, 0]] } }
        ] }))],
    });
    let mut rng = Rng(5);
    let mut grid_rows = |chars: &[char], w: usize, h: usize| -> Vec<String> {
        (0..h)
            .map(|_| (0..w).map(|_| *rng.pick(chars)).collect())
            .collect()
    };
    let col = grid_rows(&['#', '#', '.', '.', 'f', '~'], 24, 16);
    let dec = grid_rows(&['.', '.', '.', 't', 'b'], 24, 16);
    let col: Vec<&str> = col.iter().map(String::as_str).collect();
    let dec: Vec<&str> = dec.iter().map(String::as_str).collect();
    let mut l = common::level(&p, &col);
    l.int_grids
        .push(worlds_core::IntGridLayer::from_rows(p.layer("decor").unwrap(), &dec).unwrap());
    p.worlds[0].levels.push(l);
    (p, common::images())
}

/// One random definition edit (or a paint), with the auto layers it must
/// make `apply_changes` report.
fn random_rule_edit(rng: &mut Rng, p: &mut Project, n: &mut u32) -> (Vec<Change>, BTreeSet<Slug>) {
    let level = p.levels().next().unwrap().slug.clone();
    let layers = [slug("auto"), slug("auto2")];
    let layer = rng.pick(&layers).clone();
    let groups = p.rules_for(layer.as_str()).unwrap().groups.clone();
    let gi = rng.below(groups.len() as u64) as usize;
    let mut g = groups[gi].clone();
    let only = |l: &Slug| BTreeSet::from([l.clone()]);
    let set_group = |p: &mut Project, g: worlds_core::RuleGroup| {
        p.apply(Command::SetRuleGroup {
            layer: layer.clone(),
            group: g.slug.clone(),
            def: g,
            index: None,
        })
        .unwrap()
        .changes
    };
    match rng.below(12) {
        0 => {
            // Set a rule's tiles (or move a reference's origin).
            let i = rng.below(g.rules.len() as u64) as usize;
            let t = [rng.below(6) as u32, rng.below(6) as u32];
            match &mut g.rules[i] {
                RuleEntry::Rule(r) => {
                    if let Some(o) = r.out_mut().unwrap().as_tiles_mut() {
                        o.tiles = vec![worlds_core::TileAlt::single(t[0], t[1])];
                    }
                }
                RuleEntry::Use(u) => u.origin = t,
            }
            (set_group(p, g), only(&layer))
        }
        1 => {
            // Add a rule somewhere.
            *n += 1;
            let rows = rng.pick(&[
                ["*#*", ".#.", "*#*"],
                ["...", "*+*", "*+*"],
                ["*~*", "*#*", "***"],
            ]);
            let mut r = common::rule(
                &format!("new{n}"),
                rows,
                vec![worlds_core::TileAlt::single(5, 5)],
            );
            r.break_on_match = rng.below(2) == 0;
            if rng.below(2) == 0 {
                r.as_match_mut().unwrap().symmetry = vec![worlds_core::Symmetry::Rotate];
            }
            let at = rng.below(g.rules.len() as u64 + 1) as usize;
            g.rules.insert(at, RuleEntry::Rule(r));
            (set_group(p, g), only(&layer))
        }
        2 if g.rules.len() > 1 => {
            let i = rng.below(g.rules.len() as u64) as usize;
            g.rules.remove(i);
            (set_group(p, g), only(&layer))
        }
        3 if g.rules.len() > 1 => {
            let i = rng.below(g.rules.len() as u64) as usize;
            let e = g.rules.remove(i);
            let j = rng.below(g.rules.len() as u64 + 1) as usize;
            g.rules.insert(j, e);
            (set_group(p, g), only(&layer))
        }
        4 => {
            // A macros edit (no command for it yet: change the model and
            // report what `set_macro` will).
            let m = p.macros.as_mut().unwrap();
            let rows = m.patterns.get_mut("edge").unwrap();
            rows[2] = if rows[2] == "***" {
                "*#*".into()
            } else {
                "***".into()
            };
            let fill = m
                .templates
                .get_mut("frame")
                .unwrap()
                .rules
                .last_mut()
                .unwrap();
            fill.chance = if fill.chance < 0.7 { 0.8 } else { 0.6 };
            // `auto` while it still has the reference (an edit may remove it).
            let expect = p
                .rules
                .iter()
                .filter(|s| s.uses_macros())
                .map(|s| s.layer.clone())
                .collect();
            (
                vec![Change::Def {
                    kind: worlds_core::command::DefKind::Macros,
                    slug: slug("macros"),
                }],
                expect,
            )
        }
        5 => {
            // A tileset edit: only the layers drawing from it rebake.
            let ts = rng.pick(&["main", "alt"]);
            let mut def = p.tileset(ts).unwrap().clone();
            def.tile_size = if def.tile_size[0] == 16 {
                [32, 32]
            } else {
                [16, 16]
            };
            let a = p
                .apply(Command::SetTilesetDef {
                    tileset: slug(ts),
                    def,
                    index: None,
                })
                .unwrap();
            let expect = p.auto_layers_drawing_from(ts).into_iter().collect();
            (a.changes, expect)
        }
        _ => {
            // A paint on collision or decor.
            let grid = rng.pick(&[slug("collision"), slug("decor")]).clone();
            let def = p.layer(grid.as_str()).unwrap();
            let mut values: Vec<Option<Slug>> =
                def.values().iter().map(|v| Some(v.slug.clone())).collect();
            values.push(None);
            let v = rng.pick(&values).clone();
            let (x, y) = (rng.below(22) as u32, rng.below(14) as u32);
            let cells = (0..4)
                .map(|i| CellValue::new(x + i % 2, y + i / 2, v.clone()))
                .collect();
            let a = p
                .apply(Command::PaintCells {
                    level,
                    layer: grid.clone(),
                    cells,
                    keep_empty: false,
                })
                .unwrap();
            // Which auto layers read the grid now (decor: only through
            // `torch`'s `require`, unless an edit removed it).
            let expect = layers
                .into_iter()
                .filter(|l| {
                    p.expanded_rules(l.as_str()).unwrap().iter().any(|g| {
                        g.rules.iter().any(|r| {
                            r.rule
                                .patterns()
                                .any(|pat| pat.grid_or(&slug("collision")) == &grid)
                        })
                    })
                })
                .collect();
            (a.changes, expect)
        }
    }
}

#[test]
fn rule_macro_and_tileset_edits_keep_the_cache_exact() {
    for seed in [1u64, 2, 3, 4] {
        let (mut p, images) = rules_project();
        let level = p.levels().next().unwrap().slug.to_string();
        let mut cache = BakeCache::new();
        cache.bake_level(&p, &level, &images).unwrap();
        let mut rng = Rng(seed);
        let mut n = 0;
        for step in 0..80 {
            let (changes, mut must) = random_rule_edit(&mut rng, &mut p, &mut n);
            if changes.is_empty() {
                // A paint of the values already there, or an identical group.
                must.clear();
            }
            let reported: BTreeSet<Slug> = cache
                .apply_changes(&p, &level, &images, &changes)
                .unwrap()
                .into_iter()
                .map(|(l, _)| l)
                .collect();
            let ctx = format!("seed {seed} step {step} {changes:?}");
            assert!(
                reported.is_superset(&must),
                "{ctx}: reported {reported:?}, must {must:?}"
            );
            // Definition edits rebake exactly the layers they touch.
            if changes.iter().any(|c| matches!(c, Change::Def { .. })) {
                assert_eq!(reported, must, "{ctx}");
            }
            for (layer, bake) in full(&p, &level, &images) {
                assert_eq!(
                    cache.get(&level, layer.as_str()),
                    Some(&bake),
                    "{ctx}: {layer} differs from a full bake"
                );
            }
        }
    }
}

#[test]
fn a_paint_after_an_unreported_rule_edit_rebakes_in_full() {
    let (mut p, images) = rules_project();
    let level = p.levels().next().unwrap().slug.to_string();
    let mut cache = BakeCache::new();
    cache.bake_level(&p, &level, &images).unwrap();
    // Edit a rule behind the cache's back (no change handed over), then
    // paint: the fingerprint differs, so no stale partial patch.
    for r in p.rules[0].groups[0].inline_rules_mut() {
        if let Some(o) = r.out_mut().unwrap().as_tiles_mut() {
            o.tiles = vec![worlds_core::TileAlt::single(7, 7)];
        }
    }
    let a = p
        .apply(Command::PaintCells {
            level: Slug::new(&level).unwrap(),
            layer: slug("collision"),
            cells: vec![CellValue::new(3, 3, Some(slug("solid")))],
            keep_empty: false,
        })
        .unwrap();
    cache
        .apply_changes(&p, &level, &images, &a.changes)
        .unwrap();
    let expect = full(&p, &level, &images);
    assert_eq!(cache.get(&level, "auto"), expect.get("auto"));
}

#[test]
fn require_grids_count_as_read_and_seed_uses_expanded_slugs() {
    let (mut p, images) = rules_project();
    let level = p.levels().next().unwrap().slug.to_string();
    let mut cache = BakeCache::new();
    // Seeding takes a bake with expanded slugs (`w__top`, ...).
    for (layer, bake) in full(&p, &level, &images) {
        assert!(
            bake.placements
                .iter()
                .any(|t| t.rule.as_str().starts_with("w__"))
                || layer != "auto"
        );
        assert!(
            cache
                .seed(&p, &level, layer.as_str(), std::sync::Arc::new(bake))
                .unwrap()
        );
    }
    // A decor paint touches `auto` (its `require` reads decor), not `auto2`.
    let a = p
        .apply(Command::PaintCells {
            level: Slug::new(&level).unwrap(),
            layer: slug("decor"),
            cells: vec![CellValue::new(5, 5, Some(slug("torch")))],
            keep_empty: false,
        })
        .unwrap();
    let got = cache
        .apply_changes(&p, &level, &images, &a.changes)
        .unwrap();
    assert_eq!(
        got.iter().map(|(l, _)| l.as_str()).collect::<Vec<_>>(),
        ["auto"]
    );
    // Partial: the rect is the paint grown by the radius, not the level.
    let r = got[0].1;
    assert!(r.max[0] - r.min[0] < 10, "{r:?}");
    assert_eq!(
        cache.get(&level, "auto"),
        full(&p, &level, &images).get("auto")
    );
    // invalidate_level_layer forgets one layer of one level.
    cache.invalidate_level_layer(&level, "auto");
    assert!(cache.get(&level, "auto").is_none());
    assert!(cache.get(&level, "auto2").is_some());
}

/// One edit breaks `auto` (its template reads a pattern that goes away) and
/// changes `auto2` (a legend only it reads): `auto2` is still rebaked, and
/// `auto` is left uncached instead of keeping its bake from before the edit.
#[test]
fn a_layer_that_fails_to_rebake_leaves_the_others_current() {
    let (mut p, images) = rules_project();
    let level = p.levels().next().unwrap().slug.to_string();
    let m = p.macros.as_mut().unwrap();
    m.legends.insert(
        slug("two"),
        serde_json::from_value(serde_json::json!({ "x": "water" })).unwrap(),
    );
    p.rules[1].groups[0].rules[1] = serde_json::from_value(serde_json::json!(
        { "slug": "fill2", "match": { "legend": "two", "rows": ["x"] },
          "out": { "tileset": "alt", "tiles": [[0, 0]] } }))
    .unwrap();
    let auto_first = p.layers.iter().position(|l| l.slug == "auto").unwrap()
        < p.layers.iter().position(|l| l.slug == "auto2").unwrap();
    assert!(auto_first, "the failing layer must come first");
    let mut cache = BakeCache::new();
    cache.bake_level(&p, &level, &images).unwrap();
    let before = cache.get(&level, "auto2").unwrap().clone();

    let m = p.macros.as_mut().unwrap();
    m.patterns.remove("edge");
    m.legends.insert(
        slug("two"),
        serde_json::from_value(serde_json::json!({ "x": "solid" })).unwrap(),
    );
    let changes = [Change::Def {
        kind: worlds_core::command::DefKind::Macros,
        slug: slug("macros"),
    }];
    let each = cache.apply_changes_per_layer(&p, &level, &images, &changes);
    let outcome: Vec<(&str, bool)> = each.iter().map(|(l, r)| (l.as_str(), r.is_ok())).collect();
    assert_eq!(outcome, [("auto", false), ("auto2", true)]);
    assert!(
        cache.get(&level, "auto").is_none(),
        "a failed layer keeps no bake"
    );
    let now =
        worlds_rules::bake_layer(&p, p.level(&level).unwrap(), &slug("auto2"), &images).unwrap();
    assert_ne!(now, before, "the edit must change auto2");
    assert_eq!(cache.get(&level, "auto2"), Some(&now));

    // The same through `apply_changes` and `bake_level`: the error comes
    // back, after the other layers were brought up to date.
    let mut cache = BakeCache::new();
    let e = cache.bake_level(&p, &level, &images).unwrap_err();
    assert!(e.to_string().contains("edge"), "{e}");
    assert!(cache.get(&level, "auto").is_none());
    assert_eq!(cache.get(&level, "auto2"), Some(&now));
}

/// The fingerprint covers what the layer reads and draws from, not every
/// definition in the project: an unrelated edit leaves cached bakes
/// patchable.
#[test]
fn the_rules_fingerprint_ignores_definitions_the_layer_doesnt_use() {
    let (p, _) = rules_project();
    let fp = |p: &Project| {
        (
            worlds_rules::rules_fingerprint(p, "auto"),
            worlds_rules::rules_fingerprint(p, "auto2"),
        )
    };
    let base = fp(&p);
    // A tiles layer, and a tileset nothing draws from.
    let mut q = p.clone();
    q.layers.push(LayerDef::new(
        slug("deco_tiles"),
        "Deco",
        LayerKind::Tiles(worlds_core::TilesDef {
            tileset: Some(slug("main")),
        }),
    ));
    let mut extra = q.tilesets[0].clone();
    extra.slug = slug("unused");
    q.tilesets.push(extra);
    assert_eq!(fp(&q), base);
    // `decor` is read by `auto` (`require`) only.
    let mut q = p.clone();
    if let LayerKind::IntGrid(d) = &mut q.layers[1].kind {
        d.values[0].glyph = 'T';
    }
    let now = fp(&q);
    assert_ne!(now.0, base.0);
    assert_eq!(now.1, base.1);
    // `alt` is drawn by `auto2` only.
    let mut q = p.clone();
    q.tilesets
        .iter_mut()
        .find(|t| t.slug == "alt")
        .unwrap()
        .tile_size = [32, 32];
    let now = fp(&q);
    assert_eq!(now.0, base.0);
    assert_ne!(now.1, base.1);
    // A level field no group requires counts for no layer.
    let mut q = p.clone();
    q.level_fields.push(
        serde_json::from_value(serde_json::json!({ "slug": "biome", "type": "string" })).unwrap(),
    );
    assert_eq!(fp(&q), base);
    // Once `auto2` has a group requiring it, its definition counts for
    // `auto2` only.
    let mut q = with_biome_group(p.clone());
    let base = fp(&q);
    q.level_fields[0].default = Some(serde_json::json!("dry"));
    let now = fp(&q);
    assert_eq!(now.0, base.0);
    assert_ne!(now.1, base.1);
}

/// `rules_project` with a `biome` level field (default `flooded`) and a
/// group of `auto2` that requires it.
fn with_biome_group(mut p: Project) -> Project {
    p.level_fields = vec![
        serde_json::from_value(serde_json::json!(
            { "slug": "biome", "type": "enum", "values": ["flooded", "dry"], "default": "flooded" }
        ))
        .unwrap(),
    ];
    p.rules[1].groups.push(
        serde_json::from_value(serde_json::json!({ "slug": "wet",
            "requires_level_field": { "field": "biome", "values": ["flooded"] },
            "rules": [ { "slug": "drip", "match": { "rows": ["#", "."] },
                         "out": { "tileset": "alt", "tiles": [[1, 1]] } } ] }))
        .unwrap(),
    );
    p
}

fn level_field_def(slug_: &str, v: serde_json::Value) -> Command {
    Command::SetLevelFieldDef {
        field: slug(slug_),
        def: serde_json::from_value(v).unwrap(),
        index: None,
    }
}

/// Apply `cmd` and update the cache; check it against full bakes and
/// return which layers were rebaked, with their rects.
fn step(
    p: &mut Project,
    cache: &mut BakeCache,
    level: &str,
    images: &TilesetImages,
    cmd: Command,
) -> BTreeMap<String, DirtyRect> {
    let a = p.apply(cmd).unwrap();
    let out = cache.apply_changes(p, level, images, &a.changes).unwrap();
    let expect = full(p, level, images);
    for layer in auto_layers(p) {
        assert_eq!(
            cache.get(level, layer.as_str()),
            expect.get(&layer),
            "{layer}"
        );
    }
    out.into_iter().map(|(l, r)| (l.to_string(), r)).collect()
}

#[test]
fn level_field_edits_rebake_only_the_layers_that_read_them() {
    let (p, images) = rules_project();
    let mut p = with_biome_group(p);
    let level = p.levels().next().unwrap().slug.to_string();
    let world = p.worlds[0].slug.clone();
    let size = {
        let l = p.level(&level).unwrap();
        [l.size[0] as i32, l.size[1] as i32]
    };
    let whole = DirtyRect {
        min: [0, 0],
        max: size,
    };
    let mut cache = BakeCache::new();
    cache.bake_level(&p, &level, &images).unwrap();
    let paint = |x: u32, y: u32, v: &str| Command::PaintCells {
        level: Slug::new(&level).unwrap(),
        layer: slug("collision"),
        cells: vec![CellValue::new(x, y, Some(slug(v)))],
        keep_empty: false,
    };

    // A new level field nothing requires: no layer rebakes, and the next
    // stroke is still a partial re-sweep of both layers.
    let music = serde_json::json!({ "slug": "music", "type": "string", "nullable": true });
    let out = step(
        &mut p,
        &mut cache,
        &level,
        &images,
        level_field_def("music", music),
    );
    assert!(out.is_empty(), "{out:?}");
    let other = |p: &Project, x: usize, y: usize| {
        let def = p.layer("collision").unwrap();
        let grid = p.level(&level).unwrap().int_grid("collision").unwrap();
        let rows = grid.to_rows(def).unwrap();
        if rows[y].as_bytes()[x] == b'#' {
            "water"
        } else {
            "solid"
        }
    };
    let v = other(&p, 12, 8);
    let out = step(&mut p, &mut cache, &level, &images, paint(12, 8, v));
    assert_eq!(out.len(), 2, "{out:?}");
    for (layer, r) in &out {
        assert!(
            r.is_empty() || !within(whole, *r),
            "{layer}: full rebake {r:?}"
        );
    }

    // Its value in the level: nothing reads it.
    let set_music = Command::SetLevelField {
        world: world.clone(),
        level: Slug::new(&level).unwrap(),
        field: slug("music"),
        value: Some(serde_json::json!("cave")),
        allow_unknown: false,
    };
    let out = step(&mut p, &mut cache, &level, &images, set_music);
    assert!(out.is_empty(), "{out:?}");

    // `biome` is read by `auto2` only: a value or a definition change
    // rebakes `auto2` in full and leaves `auto` alone.
    let set_biome = Command::SetLevelField {
        world: world.clone(),
        level: Slug::new(&level).unwrap(),
        field: slug("biome"),
        value: Some(serde_json::json!("dry")),
        allow_unknown: false,
    };
    let out = step(&mut p, &mut cache, &level, &images, set_biome);
    assert_eq!(out.keys().collect::<Vec<_>>(), ["auto2"]);
    assert!(within(whole, out["auto2"]));
    let biome = serde_json::json!(
        { "slug": "biome", "type": "enum", "values": ["flooded", "dry"], "default": "dry" }
    );
    let remove_value = Command::SetLevelField {
        world,
        level: Slug::new(&level).unwrap(),
        field: slug("biome"),
        value: None,
        allow_unknown: false,
    };
    step(&mut p, &mut cache, &level, &images, remove_value);
    let out = step(
        &mut p,
        &mut cache,
        &level,
        &images,
        level_field_def("biome", biome),
    );
    assert_eq!(out.keys().collect::<Vec<_>>(), ["auto2"]);
    // Renaming it rewrites the requirement: `auto2`'s rules changed.
    let out = step(
        &mut p,
        &mut cache,
        &level,
        &images,
        Command::RenameLevelField {
            from: slug("biome"),
            to: slug("zone"),
        },
    );
    assert_eq!(out.keys().collect::<Vec<_>>(), ["auto2"]);
    // And a stroke afterwards is partial again.
    let v = other(&p, 5, 5);
    let out = step(&mut p, &mut cache, &level, &images, paint(5, 5, v));
    assert_eq!(out.len(), 2, "{out:?}");
    for (layer, r) in &out {
        assert!(
            r.is_empty() || !within(whole, *r),
            "{layer}: full rebake {r:?}"
        );
    }
}

#[test]
fn a_level_field_value_rebakes_nothing_without_a_requirement() {
    let (mut p, images) = rules_project();
    p.level_fields = vec![
        serde_json::from_value(serde_json::json!({ "slug": "music", "type": "string" })).unwrap(),
    ];
    let level = p.levels().next().unwrap().slug.to_string();
    let mut cache = BakeCache::new();
    cache.bake_level(&p, &level, &images).unwrap();
    let cmd = Command::SetLevelField {
        world: p.worlds[0].slug.clone(),
        level: Slug::new(&level).unwrap(),
        field: slug("music"),
        value: Some(serde_json::json!("cave")),
        allow_unknown: false,
    };
    let out = step(&mut p, &mut cache, &level, &images, cmd);
    assert!(out.is_empty(), "{out:?}");
}

// ---- passes, region rules, writes, occupancy, derived grids ----

use worlds_core::command::GridValue;

/// Two auto layers using passes, region rules, writes, occupancy and
/// derived grids. `reader` (listed first) reads
/// the derived grid `marks` that `writer` writes:
///
/// - writer pass 0: walls with two alternatives (one opaque, so locks),
///   floor marks from a region rule, water marks (chance), a speckle region
///   rule `across` with random spacing;
/// - writer pass 1: a stamp that occupies and avoids overlap on `p` marks
///   (with a `require` list), a 32 px dot avoiding overlap on `q` marks,
///   and a group requiring the level tag `lush`;
/// - writer pass 2: writes `q` over fortified cells (a later pass
///   overwrites);
/// - reader: tiles on `p`, a region rule over the marks, a stamp avoiding
///   overlap.
fn derived_project() -> (Project, TilesetImages) {
    let mut p = common::project();
    p.seed = 99;
    let marks: worlds_core::LayerDef = serde_json::from_value(serde_json::json!({
        "slug": "marks", "name": "Marks", "kind": "int_grid", "derived": true,
        "values": [
            { "slug": "pillar_base", "name": "Pillar base", "color": "#E0A040", "glyph": "p" },
            { "slug": "other", "name": "Other", "color": "#40A0E0", "glyph": "q" } ]
    }))
    .unwrap();
    let writer: worlds_core::LayerDef = serde_json::from_value(
        serde_json::json!({ "slug": "writer", "name": "Writer", "kind": "auto", "source": "collision" }),
    )
    .unwrap();
    let reader: worlds_core::LayerDef = serde_json::from_value(
        serde_json::json!({ "slug": "reader", "name": "Reader", "kind": "auto", "source": "collision" }),
    )
    .unwrap();
    p.layers = vec![p.layers[0].clone(), marks, reader, writer];
    p.tilesets.push(worlds_core::TilesetDef {
        slug: Slug::new("big").unwrap(),
        image: "big.png".into(),
        tile_size: [32, 32],
        spacing: 0,
        padding: 0,
    });
    p.rules = vec![
        serde_json::from_value(serde_json::json!({
            "layer": "writer",
            "stamps": [
                { "slug": "pil", "tileset": "main", "rect": [1, 2, 2, 3], "pivot": [0, 1], "occupies": true } ],
            "groups": [
                { "slug": "base", "rules": [
                    { "slug": "walls", "match": { "rows": ["#"] },
                      "out": { "tileset": "main", "tiles": [[0, 0], { "stack": [[1, 0]], "weight": 2 }] } },
                    { "slug": "floor_marks", "region": { "cells": ".", "min_width": 4 },
                      "place": { "along": "floor", "every": 3, "margin": 1 },
                      "write": { "grid": "marks", "value": "pillar_base" } },
                    { "slug": "water_marks", "match": { "rows": ["~"] }, "chance": 0.7,
                      "write": { "grid": "marks", "value": "other" } },
                    { "slug": "speckle", "region": { "cells": ".", "min_area": 6 },
                      "place": { "across": "interior", "spacing": "random", "every": [3, 2], "jitter": 2 },
                      "out": { "tileset": "alt", "tiles": [[5, 5]] } } ] },
                { "slug": "art", "pass": 1, "rules": [
                    { "slug": "pillar", "match": { "grid": "marks", "rows": ["p"] },
                      "require": [ { "rows": ["@", "+"] }, { "legend": { "x": "!water" }, "rows": ["x", "@"] } ],
                      "out": { "stamp": "pil", "avoid_overlap": true } },
                    { "slug": "q_dot", "match": { "grid": "marks", "rows": ["q"] },
                      "out": { "tileset": "big", "tiles": [[0, 0]], "avoid_overlap": true } } ] },
                { "slug": "lush", "pass": 1, "requires_level_tag": { "tags": ["lush"] }, "rules": [
                    { "slug": "moss", "match": { "rows": [".", "@"], "legend": { "@": "solid" } }, "chance": 0.5,
                      "out": { "tileset": "alt", "tiles": [[3, 3]] } } ] },
                { "slug": "late", "pass": 2, "rules": [
                    { "slug": "fort_q", "match": { "rows": ["f"] },
                      "write": { "grid": "marks", "value": "other" } } ] }
            ]
        }))
        .unwrap(),
        serde_json::from_value(serde_json::json!({
            "layer": "reader",
            "stamps": [
                { "slug": "box", "tileset": "main", "rect": [4, 4, 2, 2], "pivot": [0.5, 1], "occupies": true } ],
            "groups": [
                { "slug": "g", "rules": [
                    { "slug": "on_p", "match": { "grid": "marks", "rows": ["p"] }, "symmetry": ["mirror_x"],
                      "out": { "tileset": "main", "tiles": [[6, 0]] } },
                    { "slug": "mark_rooms", "region": { "grid": "marks", "cells": "+" },
                      "place": { "across": "interior", "every": 2 },
                      "out": { "tileset": "main", "tiles": [[7, 0]] } } ] },
                { "slug": "boxes", "pass": 1, "rules": [
                    { "slug": "box_q", "match": { "grid": "marks", "rows": ["q"] },
                      "out": { "stamp": "box", "avoid_overlap": true } } ] }
            ]
        }))
        .unwrap(),
    ];
    let imgs = common::images();
    imgs.insert(
        Slug::new("big").unwrap(),
        TileOpacity::from_flags(8, 8, vec![false; 64]),
    );
    (p, imgs)
}

/// A level of rooms: solid border, a few platforms, water and fortified.
fn derived_level(p: &Project, rng: &mut Rng, name: &str, w: usize, h: usize) -> worlds_core::Level {
    let mut rows: Vec<Vec<char>> = (0..h)
        .map(|y| {
            (0..w)
                .map(|x| {
                    if x == 0 || y == 0 || x == w - 1 || y == h - 1 {
                        '#'
                    } else {
                        '.'
                    }
                })
                .collect()
        })
        .collect();
    for _ in 0..(w * h / 40) {
        let (x, y) = (
            1 + rng.below(w as u64 - 2) as usize,
            1 + rng.below(h as u64 - 2) as usize,
        );
        let len = 1 + rng.below(6) as usize;
        let g = *rng.pick(&['#', '#', '#', 'f', '~']);
        for i in 0..len {
            if x + i < w - 1 {
                rows[y][x + i] = g;
            }
        }
    }
    let rows: Vec<String> = rows.into_iter().map(|r| r.into_iter().collect()).collect();
    let rows: Vec<&str> = rows.iter().map(String::as_str).collect();
    let mut l = common::level(p, &rows);
    l.slug = Slug::new(name).unwrap();
    l
}

/// One random edit of the kinds those features read: collision strokes and fills, override
/// paints (values, `!`, `null`), level tags, rule edits on the writer (no
/// stroke after), a tileset and a derived layer definition, layer seeds.
fn derived_edit(rng: &mut Rng, p: &Project, level: &str, writer: &str, marks: &str) -> Command {
    let l = p.level(level).unwrap();
    let lv = Slug::new(level).unwrap();
    let col = p.layer("collision").unwrap();
    let [w, h] = p.layer_cells(col, l);
    let at = |rng: &mut Rng| (rng.below(w as u64) as u32, rng.below(h as u64) as u32);
    match rng.below(40) {
        0..=13 => {
            let mut values: Vec<Option<Slug>> =
                col.values().iter().map(|v| Some(v.slug.clone())).collect();
            values.push(None);
            values.push(None);
            let v = rng.pick(&values).clone();
            let (x0, y0) = at(rng);
            let n = 1 + rng.below(6);
            let cells = (0..n)
                .map(|_| {
                    let x = (x0 + rng.below(4) as u32).min(w - 1);
                    let y = (y0 + rng.below(3) as u32).min(h - 1);
                    CellValue::new(x, y, v.clone())
                })
                .collect();
            Command::PaintCells {
                level: lv,
                layer: Slug::new("collision").unwrap(),
                cells,
                keep_empty: false,
            }
        }
        14..=16 => {
            let (x, y) = at(rng);
            let rw = 1 + rng.below((w - x).min(4) as u64) as u32;
            let rh = 1 + rng.below((h - y).min(8) as u64) as u32;
            let v = rng
                .pick(&[None, Some(col.values()[0].slug.clone())])
                .clone();
            Command::FillRect {
                level: lv,
                layer: Slug::new("collision").unwrap(),
                rect: Rect::new(x, y, rw, rh),
                value: v.into(),
            }
        }
        17..=26 => {
            let md = p.layer(marks).unwrap();
            let mut values: Vec<GridValue> = md
                .values()
                .iter()
                .map(|v| GridValue::Value(v.slug.clone()))
                .collect();
            values.push(GridValue::Suppress);
            values.push(GridValue::Suppress);
            values.push(GridValue::Clear);
            values.push(GridValue::Clear);
            let (x0, y0) = at(rng);
            let cells = (0..1 + rng.below(4))
                .map(|_| {
                    let x = (x0 + rng.below(3) as u32).min(w - 1);
                    let y = (y0 + rng.below(2) as u32).min(h - 1);
                    CellValue::new(x, y, rng.pick(&values).clone())
                })
                .collect();
            Command::PaintCells {
                level: lv,
                layer: Slug::new(marks).unwrap(),
                cells,
                keep_empty: false,
            }
        }
        27 | 28 => {
            let tags = if l.tags.is_empty() {
                vec!["lush".to_string()]
            } else {
                vec![]
            };
            Command::SetLevelTags {
                world: p.worlds[0].slug.clone(),
                level: lv,
                tags,
            }
        }
        29..=32 => {
            // A rule edit on the writer: switch a rule, or move its marks.
            let set = p.rules_for(writer).unwrap();
            let (group, rule) = {
                let all: Vec<(&worlds_core::RuleGroup, &worlds_core::Rule)> = set
                    .groups
                    .iter()
                    .flat_map(|g| g.inline_rules().map(move |r| (g, r)))
                    .collect();
                let (g, r) = *rng.pick(&all);
                (g.slug.clone(), r.clone())
            };
            let mut r = rule;
            if let Some(rs) = r.as_region_mut()
                && rng.below(2) == 0
            {
                let e = 1 + rng.below(5) as u32;
                rs.place.every = worlds_core::PerAxis::Same(std::num::NonZeroU32::new(e).unwrap());
            } else {
                r.active = !r.active;
            }
            Command::SetRule {
                layer: Slug::new(writer).unwrap(),
                group,
                rule: r.slug.clone(),
                def: r.into(),
                index: None,
            }
        }
        33 => match p.tileset("big").or_else(|| p.tilesets.first()) {
            Some(t) => {
                let mut t = t.clone();
                let s = *rng.pick(&[16u32, 24, 32]);
                t.tile_size = [s, s];
                Command::SetTilesetDef {
                    tileset: t.slug.clone(),
                    def: t,
                    index: None,
                }
            }
            None => Command::noop(),
        },
        34 => {
            let mut def = p.layer(marks).unwrap().clone();
            def.name = format!("Marks {}", rng.below(100));
            Command::SetLayerDef {
                layer: def.slug.clone(),
                def,
                index: None,
                content: Vec::new(),
                rules: None,
            }
        }
        35 | 36 => Command::SetLayerState {
            level: lv,
            layer: Slug::new(writer).unwrap(),
            state: Some(LayerState {
                seed: rng.below(1000) as i64,
                ..l.layers.get(writer).cloned().unwrap_or_default()
            }),
        },
        37 => Command::SetProjectSetting {
            setting: ProjectSetting::Seed(rng.below(1 << 20) as i64),
        },
        _ => Command::FillRect {
            level: lv,
            layer: Slug::new(marks).unwrap(),
            rect: Rect::new(0, 0, w, h),
            value: GridValue::Clear,
        },
    }
}

/// Every cell where two grids differ.
fn grid_diff(
    a: Option<&worlds_core::IntGridLayer>,
    b: Option<&worlds_core::IntGridLayer>,
) -> Vec<[i32; 2]> {
    let (Some(a), Some(b)) = (a, b) else {
        return Vec::new();
    };
    let w = a.size()[0] as usize;
    a.cells()
        .iter()
        .zip(b.cells())
        .enumerate()
        .filter(|(_, (x, y))| x != y)
        .map(|(i, _)| [(i % w) as i32, (i / w) as i32])
        .collect()
}

/// Random edits of those kinds on every level: after each batch the cache equals a
/// fresh bake (every auto layer's `LayerBake`, placements, `fired`, the
/// computed grids and occupancy; every derived layer's effective grid),
/// and every changed tile and derived cell is inside the reported rect.
fn run_derived(
    name: &str,
    p: &mut Project,
    images: &TilesetImages,
    steps: usize,
    seed: u64,
    writer: &str,
    marks: &str,
) {
    let mut rng = Rng(seed);
    let levels: Vec<String> = p.levels().map(|l| l.slug.to_string()).collect();
    let derived: Vec<Slug> = p
        .layers
        .iter()
        .filter(|l| l.is_derived())
        .map(|l| l.slug.clone())
        .collect();
    let export = |p: &Project, level: &str| {
        worlds_rules::bake_for_export(p, p.level(level).unwrap(), images).unwrap()
    };
    for level in &levels {
        let mut cache = BakeCache::new();
        cache.bake_level(p, level, images).unwrap();
        let mut prev = full(p, level, images);
        let mut prev_eff = export(p, level).derived;
        let mut step = 0;
        while step < steps {
            let mut changes: Vec<Change> = Vec::new();
            for _ in 0..1 + rng.below(2) {
                let cmd = derived_edit(&mut rng, p, level, writer, marks);
                if let Ok(a) = p.apply(cmd) {
                    changes.extend(a.changes);
                }
                step += 1;
            }
            let dirty: BTreeMap<Slug, DirtyRect> = cache
                .apply_changes(p, level, images, &changes)
                .unwrap_or_else(|e| panic!("{name} {level} step {step}: {e}"))
                .into_iter()
                .collect();
            let now = full(p, level, images);
            let eff = export(p, level).derived;
            for (layer, bake) in &now {
                let ctx = format!("{name} {level} {layer} step {step} after {changes:?}");
                let got = cache
                    .get(level, layer.as_str())
                    .unwrap_or_else(|| panic!("{ctx}: uncached"));
                if got != bake {
                    let keys: BTreeSet<&[i32; 2]> =
                        got.fired.keys().chain(bake.fired.keys()).collect();
                    let diff: Vec<String> = keys
                        .into_iter()
                        .filter(|k| got.fired.get(*k) != bake.fired.get(*k))
                        .take(8)
                        .map(|k| {
                            format!("{k:?}: {:?} vs {:?}", got.fired.get(k), bake.fired.get(k))
                        })
                        .collect();
                    panic!(
                        "{ctx}: cache differs from a full bake ({} vs {} placements, derived \
                         equal {}, occupied equal {}); fired differs at {diff:?}",
                        got.placements.len(),
                        bake.placements.len(),
                        got.derived == bake.derived,
                        got.occupied == bake.occupied,
                    );
                }
                let g = p.layer_grid(p.layer(layer.as_str()).unwrap()) as i32;
                let before = &prev[layer].placements;
                let changed: Vec<&Placement> = missing(before, &bake.placements)
                    .into_iter()
                    .chain(missing(&bake.placements, before))
                    .collect();
                if !changed.is_empty() {
                    let r = dirty
                        .get(layer)
                        .unwrap_or_else(|| panic!("{ctx}: no dirty rect"));
                    for pl in changed {
                        assert!(within(drawn(p, pl, g), *r), "{ctx}: {pl:?} outside {r:?}");
                    }
                }
            }
            for d in &derived {
                let ctx = format!("{name} {level} {d} step {step}");
                let got = cache
                    .derived(level, d.as_str())
                    .unwrap_or_else(|| panic!("{ctx}: none"));
                assert_eq!(Some(&*got), eff.get(d), "{ctx}: effective grid");
                for c in grid_diff(prev_eff.get(d), eff.get(d)) {
                    let r = dirty
                        .get(d)
                        .unwrap_or_else(|| panic!("{ctx}: {c:?} changed, no rect"));
                    assert!(
                        within(
                            DirtyRect {
                                min: c,
                                max: [c[0] + 1, c[1] + 1]
                            },
                            *r
                        ),
                        "{ctx}: {c:?} outside {r:?}"
                    );
                }
            }
            prev = now;
            prev_eff = eff;
        }
    }
}

#[test]
fn incremental_matches_full_with_derived_grids_across_two_layers() {
    let (mut p, images) = derived_project();
    let mut rng = Rng(5);
    for (i, (w, h)) in [(40, 18), (24, 12), (64, 28)].into_iter().enumerate() {
        let l = derived_level(&p, &mut rng, &format!("room{i}"), w, h);
        p.worlds[0].levels.push(l);
    }
    // Sanity: the project exercises what it should.
    let l = p.level("room2").unwrap();
    let b = worlds_rules::bake_level(&p, l, &images).unwrap();
    let writer = &b[&Slug::new("writer").unwrap()];
    assert!(writer.fired.values().flatten().any(|r| r == "floor_marks"));
    assert!(writer.fired.values().flatten().any(|r| r == "pillar"));
    assert!(writer.occupied.iter().any(|o| *o));
    assert!(
        b[&Slug::new("reader").unwrap()]
            .fired
            .values()
            .flatten()
            .any(|r| r == "on_p")
    );
    run_derived("derived", &mut p, &images, 160, 0x5eed, "writer", "marks");
}

#[test]
fn a_rule_edit_on_the_writer_rebakes_its_readers_with_no_stroke() {
    let (mut p, images) = derived_project();
    let mut rng = Rng(11);
    let l = derived_level(&p, &mut rng, "room", 40, 18);
    p.worlds[0].levels.push(l);
    let mut cache = BakeCache::new();
    cache.bake_level(&p, "room", &images).unwrap();
    // Switch the floor marks off: only a `def` change, no grid edit.
    let set = p.rules_for("writer").unwrap();
    let mut r = set.groups[0].inline_rules().nth(1).unwrap().clone();
    assert_eq!(r.slug, "floor_marks");
    r.active = false;
    let a = p
        .apply(Command::SetRule {
            layer: Slug::new("writer").unwrap(),
            group: Slug::new("base").unwrap(),
            rule: r.slug.clone(),
            def: r.into(),
            index: None,
        })
        .unwrap();
    let dirty: BTreeMap<Slug, DirtyRect> = cache
        .apply_changes(&p, "room", &images, &a.changes)
        .unwrap()
        .into_iter()
        .collect();
    assert!(
        dirty.contains_key("reader"),
        "the reader is rebaked: {dirty:?}"
    );
    assert!(dirty.contains_key("marks"), "the marks changed: {dirty:?}");
    let now = full(&p, "room", &images);
    for (layer, bake) in &now {
        assert_eq!(cache.get("room", layer.as_str()), Some(bake), "{layer}");
    }
}

/// The cache against fresh bakes after `changes`: every auto layer's bake
/// and the effective `marks` grid.
fn assert_cache_fresh(p: &Project, cache: &BakeCache, images: &TilesetImages, what: &str) {
    let now = full(p, "room", images);
    for (layer, bake) in &now {
        assert_eq!(
            cache.get("room", layer.as_str()),
            Some(bake),
            "{what}: {layer}"
        );
    }
    let l = p.level("room").unwrap();
    let fresh = worlds_rules::effective_grid(p, l, "marks", images).unwrap();
    assert_eq!(
        cache.derived("room", "marks").as_deref(),
        Some(&fresh),
        "{what}: effective marks"
    );
}

/// When the writer stops writing the derived grid (a rule edit removes its
/// `write` rules, or the writer layer goes), the grid's effective values
/// fall back to the overrides and its readers rebake, though under the new
/// definitions nothing depends on that writer any more.
#[test]
fn a_writer_that_stops_writing_leaves_no_stale_grid_or_reader() {
    let (mut p, images) = derived_project();
    let mut rng = Rng(5);
    let l = derived_level(&p, &mut rng, "room", 40, 18);
    p.worlds[0].levels.push(l);
    // An override, so the grid isn't simply empty afterwards.
    p.apply(Command::PaintCells {
        level: Slug::new("room").unwrap(),
        layer: Slug::new("marks").unwrap(),
        cells: vec![CellValue::new(
            3,
            3,
            GridValue::Value(Slug::new("other").unwrap()),
        )],
        keep_empty: false,
    })
    .unwrap();
    let start = p.clone();

    // A rule edit: every `write` rule of the writer removed.
    let mut cache = BakeCache::new();
    cache.bake_level(&p, "room", &images).unwrap();
    let mut changes = Vec::new();
    for rule in ["floor_marks", "water_marks", "fort_q"] {
        let a = p
            .apply(Command::RemoveRule {
                layer: Slug::new("writer").unwrap(),
                rule: Slug::new(rule).unwrap(),
            })
            .unwrap();
        changes.extend(a.changes);
    }
    assert!(p.derived_writer("marks").is_none());
    let dirty: BTreeMap<Slug, DirtyRect> = cache
        .apply_changes(&p, "room", &images, &changes)
        .unwrap()
        .into_iter()
        .collect();
    assert!(dirty.contains_key("reader"), "{dirty:?}");
    assert!(dirty.contains_key("marks"), "{dirty:?}");
    assert_cache_fresh(&p, &cache, &images, "write rules removed");

    // The writer layer removed.
    let mut p = start;
    let mut cache = BakeCache::new();
    cache.bake_level(&p, "room", &images).unwrap();
    let a = p
        .apply(Command::RemoveLayerDef {
            layer: Slug::new("writer").unwrap(),
        })
        .unwrap();
    cache
        .apply_changes(&p, "room", &images, &a.changes)
        .unwrap();
    assert!(cache.get("room", "writer").is_none(), "the removed layer");
    assert_cache_fresh(&p, &cache, &images, "writer removed");
}

/// A level seed baked before an override paint: the override shows in the
/// cache's effective grid once seeded when no layer reads the grid; when
/// one does, the seed is refused (its bakes are older than the grid).
#[test]
fn a_level_seed_never_hides_a_later_override() {
    let (mut p, images) = derived_project();
    let mut rng = Rng(9);
    let l = derived_level(&p, &mut rng, "room", 30, 14);
    p.worlds[0].levels.push(l);
    let paint = |p: &mut Project| {
        p.apply(Command::PaintCells {
            level: Slug::new("room").unwrap(),
            layer: Slug::new("marks").unwrap(),
            cells: vec![CellValue::new(
                2,
                2,
                GridValue::Value(Slug::new("other").unwrap()),
            )],
            keep_empty: false,
        })
        .unwrap()
    };
    let fresh_marks = |p: &Project| {
        worlds_rules::effective_grid(p, p.level("room").unwrap(), "marks", &images).unwrap()
    };

    // Read by both layers: refused, nothing stale left behind.
    let mut q = p.clone();
    let seed = BakeCache::bake_detached(&q, "room", &images).unwrap();
    let a = paint(&mut q);
    let mut cache = BakeCache::new();
    cache
        .apply_changes(&q, "room", &images, &a.changes)
        .unwrap();
    assert!(!cache.seed_level(&q, seed));
    assert!(cache.get("room", "reader").is_none());
    cache.bake_level(&q, "room", &images).unwrap();
    assert_cache_fresh(&q, &cache, &images, "after a refused seed");

    // Read by nobody (no reader, the writer's pass 1 gone): taken, and the
    // override shows.
    for cmd in [
        Command::RemoveLayerDef {
            layer: Slug::new("reader").unwrap(),
        },
        Command::RemoveRuleGroup {
            layer: Slug::new("writer").unwrap(),
            group: Slug::new("art").unwrap(),
            keep_empty: false,
        },
    ] {
        p.apply(cmd).unwrap();
    }
    let seed = BakeCache::bake_detached(&p, "room", &images).unwrap();
    let a = paint(&mut p);
    let mut cache = BakeCache::new();
    cache
        .apply_changes(&p, "room", &images, &a.changes)
        .unwrap();
    assert!(cache.seed_level(&p, seed));
    assert_eq!(
        cache.derived("room", "marks").as_deref(),
        Some(&fresh_marks(&p))
    );
    assert_cache_fresh(&p, &cache, &images, "seeded");
}

#[test]
fn derived_layers_seed_from_a_level_seed_or_layer_by_layer() {
    let (mut p, images) = derived_project();
    let mut rng = Rng(3);
    let l = derived_level(&p, &mut rng, "room", 30, 14);
    p.worlds[0].levels.push(l);
    let level_seed = BakeCache::bake_detached(&p, "room", &images).unwrap();
    assert_eq!(level_seed.bakes().count(), 2);
    let mut a = BakeCache::new();
    assert!(a.seed_level(&p, level_seed));
    // Layer by layer, writer first (as worker bakes arrive): the per-pass
    // state is rebuilt from each bake, and the seeded Arc is kept as is.
    let fresh = full(&p, "room", &images);
    let mut b = BakeCache::new();
    assert!(
        !b.seed(
            &p,
            "room",
            "reader",
            std::sync::Arc::new(fresh[&Slug::new("reader").unwrap()].clone())
        )
        .unwrap(),
        "the reader needs its writer's grid first"
    );
    for layer in ["writer", "reader"] {
        let bake = std::sync::Arc::new(fresh[&Slug::new(layer).unwrap()].clone());
        assert!(b.seed(&p, "room", layer, bake.clone()).unwrap(), "{layer}");
        assert!(std::sync::Arc::ptr_eq(
            b.get_shared("room", layer).unwrap(),
            &bake
        ));
    }
    // A bake that doesn't fit is refused.
    let mut wrong = fresh[&Slug::new("writer").unwrap()].clone();
    wrong.occupied.iter_mut().for_each(|o| *o = !*o);
    assert!(
        !BakeCache::new()
            .seed(&p, "room", "writer", std::sync::Arc::new(wrong))
            .unwrap()
    );
    // Then edits update both like a baked cache.
    let mut rng = Rng(77);
    for step in 0..40 {
        let cmd = derived_edit(&mut rng, &p, "room", "writer", "marks");
        let Ok(applied) = p.apply(cmd) else { continue };
        for c in [&mut a, &mut b] {
            c.apply_changes(&p, "room", &images, &applied.changes)
                .unwrap();
        }
        let now = full(&p, "room", &images);
        for (layer, bake) in &now {
            assert_eq!(
                a.get("room", layer.as_str()),
                Some(bake),
                "{layer} step {step}"
            );
            assert_eq!(
                b.get("room", layer.as_str()),
                Some(bake),
                "{layer} step {step}"
            );
        }
    }
}
