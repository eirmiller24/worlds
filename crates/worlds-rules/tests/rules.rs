mod common;

use common::*;
use worlds_core::{
    Checker, EnumSource, FieldDef, FieldType, Flip, GroupMode, LayerState, LevelFieldRequirement,
    Modulo, OutOfBounds, RandomOffset, RequirementMode, Symmetry, TileAlt, TileMode, slug,
};

fn one(id: &str, rows: &[&str]) -> worlds_core::Rule {
    rule(id, rows, vec![TileAlt::single(1, 1)])
}

#[test]
fn legend_tests() {
    let mut p = project();
    set_groups(
        &mut p,
        vec![group(
            "g",
            vec![
                one("any", &["*"]),
                one("nonempty", &["+"]),
                one("empty", &["."]),
                legend(one("value", &["s"]), 's', "solid"),
                legend(one("in_group", &["w"]), 'w', "walls"),
                legend(one("not_solid", &["n"]), 'n', "!solid"),
                legend(one("not_walls", &["n"]), 'n', "!walls"),
                // The legend can override a built-in glyph.
                legend(one("dot_is_water", &["."]), '.', "water"),
            ],
        )],
    );
    let l = level(&p, &["#f~."]);
    let b = bake(&p, &l);
    let xs = |r: &str| fired_at(&b, r).iter().map(|c| c[0]).collect::<Vec<_>>();
    assert_eq!(xs("any"), [0, 1, 2, 3]);
    assert_eq!(xs("nonempty"), [0, 1, 2]);
    assert_eq!(xs("empty"), [3]);
    assert_eq!(xs("value"), [0]);
    assert_eq!(xs("in_group"), [0, 1]);
    assert_eq!(xs("not_solid"), [1, 2, 3]);
    assert_eq!(xs("not_walls"), [2, 3]);
    assert_eq!(xs("dot_is_water"), [2]);
}

#[test]
fn value_and_group_glyphs_need_no_legend() {
    let mut p = project();
    p.layers[0].int_grid_mut().unwrap().groups[0].glyph = Some('w');
    set_groups(
        &mut p,
        vec![group(
            "g",
            vec![
                one("solid", &["#"]),
                one("water", &["~"]),
                one("walls", &["w"]),
            ],
        )],
    );
    let b = bake(&p, &level(&p, &["#f~."]));
    let xs = |r: &str| fired_at(&b, r).iter().map(|c| c[0]).collect::<Vec<_>>();
    assert_eq!(xs("solid"), [0]);
    assert_eq!(xs("water"), [2]);
    assert_eq!(xs("walls"), [0, 1]);
}

#[test]
fn repeated_glyph_uses_its_own_test() {
    // A glyph seen again after another glyph must reuse its own lookup
    // table, not the first glyph's (it used to read `#`'s table, shifted).
    // Column: solid above, water at the anchor, water below.
    let mut p = project();
    set_groups(
        &mut p,
        vec![group(
            "g",
            vec![legend(one("r", &["#", "n", "n"]), 'n', "!solid")],
        )],
    );
    let l = level(&p, &["#", "~", "~"]);
    let b = bake(&p, &l);
    assert_eq!(fired_at(&b, "r"), [[0, 1]]);
}

#[test]
fn unknown_test_is_an_error() {
    let mut p = project();
    set_groups(
        &mut p,
        vec![group("g", vec![legend(one("r", &["x"]), 'x', "lava")])],
    );
    let l = level(&p, &["#"]);
    let e = worlds_rules::bake_layer(&p, &l, &slug("auto"), &images()).unwrap_err();
    assert!(e.to_string().contains("lava"), "{e}");
}

#[test]
fn mirror_x_and_mirror_y() {
    // Anchor is empty with solid on the left; mirror_x also accepts solid on the right.
    let mut r = one("edge", &["#.*"]);
    r.as_match_mut().unwrap().symmetry = vec![Symmetry::MirrorX];
    let mut p = project();
    set_groups(&mut p, vec![group("g", vec![r])]);
    let b = bake(&p, &level(&p, &["#..##"]));
    let got: Vec<_> = by_rule(&b, "edge")
        .iter()
        .map(|t| (t.anchor, t.flip, t.px))
        .collect();
    assert_eq!(
        got,
        [([1, 0], None, [16, 0]), ([2, 0], Some(Flip::X), [32, 0])]
    );

    let mut r = one("top", &["#", ".", "*"]);
    r.as_match_mut().unwrap().symmetry = vec![Symmetry::MirrorY];
    set_groups(&mut p, vec![group("g", vec![r])]);
    let b = bake(&p, &level(&p, &["#", ".", ".", "#", "#"]));
    let got: Vec<_> = by_rule(&b, "top")
        .iter()
        .map(|t| (t.anchor, t.flip))
        .collect();
    assert_eq!(got, [([0, 1], None), ([0, 2], Some(Flip::Y))]);
}

#[test]
fn mirror_both_axes_and_break_between_variants() {
    // Solid diagonal up-left of the anchor; with both mirrors, all four diagonals.
    let mut r = one("corner", &["#**", "*.*", "***"]);
    r.as_match_mut().unwrap().symmetry = vec![Symmetry::MirrorX, Symmetry::MirrorY];
    r.as_match_mut().unwrap().out_of_bounds = Some(OutOfBounds::Empty);
    let mut p = project();
    set_groups(&mut p, vec![group("g", vec![r.clone()])]);
    let l = level(&p, &["#.#", "...", "#.#"]);
    let b = bake(&p, &l);
    // Center cell sees all four corners: every variant places a tile (no break).
    let flips: Vec<_> = by_rule(&b, "corner")
        .iter()
        .filter(|t| t.anchor == [1, 1])
        .map(|t| t.flip)
        .collect();
    assert_eq!(flips, [None, Some(Flip::X), Some(Flip::Y), Some(Flip::XY)]);

    // With break, only the first matching variant draws.
    r.break_on_match = true;
    set_groups(&mut p, vec![group("g", vec![r])]);
    let b = bake(&p, &l);
    let flips: Vec<_> = by_rule(&b, "corner")
        .iter()
        .filter(|t| t.anchor == [1, 1])
        .map(|t| t.flip)
        .collect();
    assert_eq!(flips, [None]);
}

#[test]
fn symmetric_pattern_does_not_stack_mirrored_copies() {
    let mut r = one("between", &["#.#"]);
    r.as_match_mut().unwrap().symmetry = vec![Symmetry::MirrorX];
    let mut p = project();
    set_groups(&mut p, vec![group("g", vec![r])]);
    let b = bake(&p, &level(&p, &["#.#"]));
    assert_eq!(by_rule(&b, "between").len(), 1);

    // Symmetric in meaning though not in glyphs: `S` is a legend alias for
    // solid, so `#.S` mirrors onto itself and must not stack either.
    let mut r = legend(one("alias", &["#.S"]), 'S', "solid");
    r.as_match_mut().unwrap().symmetry = vec![Symmetry::MirrorX];
    set_groups(&mut p, vec![group("g", vec![r])]);
    let b = bake(&p, &level(&p, &["#.#"]));
    assert_eq!(by_rule(&b, "alias").len(), 1);
}

#[test]
fn break_versus_stacking_and_draw_order() {
    // Tile [1, 1] is transparent, so without break both rules draw.
    let a = one("a", &["+"]);
    let mut b_rule = one("b", &["+"]);
    tiles(&mut b_rule).tiles = vec![TileAlt::single(2, 2)];
    let mut p = project();
    set_groups(
        &mut p,
        vec![
            group("g1", vec![a.clone()]),
            group("g2", vec![b_rule.clone()]),
        ],
    );
    let l = level(&p, &["##"]);
    let bake1 = bake(&p, &l);
    assert_eq!(bake1.fired[&[0, 0]], [slug("a"), slug("b")]);
    // Draw order: later group/rule first (bottom), first rule last (top).
    let order: Vec<_> = bake1
        .placements
        .iter()
        .map(|t| (t.rule.as_str(), t.anchor))
        .collect();
    assert_eq!(
        order,
        [("b", [0, 0]), ("b", [1, 0]), ("a", [0, 0]), ("a", [1, 0])]
    );
    assert_eq!(bake1.placements[3].group, "g1");

    // break on `a` locks the cell across groups.
    let mut a = a;
    a.break_on_match = true;
    set_groups(
        &mut p,
        vec![group("g1", vec![a]), group("g2", vec![b_rule])],
    );
    let bake2 = bake(&p, &l);
    assert_eq!(bake2.fired[&[0, 0]], [slug("a")]);
    assert!(by_rule(&bake2, "b").is_empty());
}

#[test]
fn break_with_empty_alternative_still_locks() {
    let mut a = rule(
        "a",
        &["+"],
        vec![TileAlt {
            tiles: vec![],
            weight: 1,
        }],
    );
    a.break_on_match = true;
    let mut p = project();
    set_groups(&mut p, vec![group("g", vec![a, one("b", &["+"])])]);
    let b = bake(&p, &level(&p, &["#"]));
    assert!(b.placements.is_empty());
    assert!(
        b.fired.is_empty(),
        "an empty alternative contributes no tiles"
    );
}

#[test]
fn opaque_tile_locks_implicitly() {
    let mut p = project();
    let opaque = rule("opaque", &["+"], vec![TileAlt::single(0, 0)]);
    let clear = rule("clear", &["+"], vec![TileAlt::single(3, 3)]);
    let later = one("later", &["+"]);
    let l = level(&p, &["#"]);

    set_groups(
        &mut p,
        vec![group("g", vec![opaque.clone(), later.clone()])],
    );
    assert_eq!(bake(&p, &l).fired[&[0, 0]], [slug("opaque")]);

    set_groups(&mut p, vec![group("g", vec![clear, later.clone()])]);
    assert_eq!(bake(&p, &l).fired[&[0, 0]], [slug("clear"), slug("later")]);

    // Not eligible: translucent, or offset.
    let mut faded = opaque.clone();
    faded.out_mut().unwrap().draw_mut().alpha = 0.5;
    set_groups(&mut p, vec![group("g", vec![faded, later.clone()])]);
    assert_eq!(bake(&p, &l).fired[&[0, 0]].len(), 2);
    let mut shifted = opaque.clone();
    shifted.out_mut().unwrap().draw_mut().offset_px = [0, 2];
    set_groups(&mut p, vec![group("g", vec![shifted, later.clone()])]);
    assert_eq!(bake(&p, &l).fired[&[0, 0]].len(), 2);
    let mut jitter = opaque;
    jitter.out_mut().unwrap().draw_mut().random_offset_px = Some(RandomOffset {
        x: [-1, 1],
        y: [0, 0],
    });
    set_groups(&mut p, vec![group("g", vec![jitter, later])]);
    assert_eq!(bake(&p, &l).fired[&[0, 0]].len(), 2);
}

#[test]
fn modulo_and_checker() {
    let p = project();
    let l = level(&p, &["######"; 4]);
    let cells = |p: &worlds_core::Project, r: worlds_core::Rule| {
        let mut p = p.clone();
        set_groups(&mut p, vec![group("g", vec![r])]);
        fired_at(&bake(&p, &l), "m")
    };

    let mut r = one("m", &["+"]);
    r.as_match_mut().unwrap().modulo = Some(Modulo {
        x: 2,
        y: 3,
        x_offset: 0,
        y_offset: 0,
    });
    assert_eq!(
        cells(&p, r.clone()),
        [[0, 0], [0, 3], [2, 0], [2, 3], [4, 0], [4, 3]]
    );

    // An offset larger than the coordinate still behaves (Euclidean remainder).
    r.as_match_mut().unwrap().modulo = Some(Modulo {
        x: 2,
        y: 3,
        x_offset: 3,
        y_offset: 4,
    });
    assert_eq!(cells(&p, r.clone()), [[1, 1], [3, 1], [5, 1]]);

    // Horizontal checker on a 2x2 modulo: row pairs alternate even/odd columns.
    r.as_match_mut().unwrap().modulo = Some(Modulo {
        x: 2,
        y: 2,
        x_offset: 0,
        y_offset: 0,
    });
    r.as_match_mut().unwrap().checker = Checker::Horizontal;
    let got = cells(&p, r.clone());
    let want: Vec<[i32; 2]> = (0..6)
        .flat_map(|x| (0..4).map(move |y| [x, y]))
        .filter(|&[x, y]| y % 2 == 0 && (x + (y / 2) % 2) % 2 == 0)
        .collect();
    assert_eq!(got, want);
    assert!(got.contains(&[0, 0]) && got.contains(&[1, 2]) && !got.contains(&[0, 2]));

    r.as_match_mut().unwrap().checker = Checker::Vertical;
    let got = cells(&p, r.clone());
    let want: Vec<[i32; 2]> = (0..6)
        .flat_map(|x| (0..4).map(move |y| [x, y]))
        .filter(|&[x, y]| x % 2 == 0 && (y + (x / 2) % 2) % 2 == 0)
        .collect();
    assert_eq!(got, want);

    // modulo 0 never matches.
    r.as_match_mut().unwrap().checker = Checker::None;
    r.as_match_mut().unwrap().modulo = Some(Modulo {
        x: 0,
        y: 1,
        x_offset: 0,
        y_offset: 0,
    });
    assert!(cells(&p, r).is_empty());
}

#[test]
fn out_of_bounds() {
    let mut p = project();
    let l = level(&p, &["###"]);
    let mut r = legend(one("r", &["s+s"]), 's', "solid");
    let mut run = |r: &worlds_core::Rule| {
        set_groups(&mut p, vec![group("g", vec![r.clone()])]);
        fired_at(&bake(&p, &l), "r")
            .iter()
            .map(|c| c[0])
            .collect::<Vec<_>>()
    };
    assert_eq!(
        run(&r),
        [1],
        "None: a pattern cell off the level fails the match"
    );
    r.as_match_mut().unwrap().out_of_bounds = Some(OutOfBounds::Value(slug("solid")));
    assert_eq!(run(&r), [0, 1, 2]);
    r.as_match_mut().unwrap().out_of_bounds = Some(OutOfBounds::Empty);
    assert_eq!(run(&r), [1]);
    // `*` cells never sample, so they don't care about the edge.
    let star = one("r", &["*+*"]);
    assert_eq!(run(&star), [0, 1, 2]);
    // A negated test off the level still fails with None (LDtk semantics).
    let neg = legend(one("r", &["n+n"]), 'n', "!water");
    assert_eq!(run(&neg), [1]);
}

fn stamp_rule(pivot: [f64; 2]) -> worlds_core::Rule {
    let mut r = rule("stamp", &["+"], vec![TileAlt::rect(4, 0, 2, 2)]);
    tiles(&mut r).tile_mode = TileMode::Stamp;
    tiles(&mut r).pivot = pivot;
    r
}

fn layout(b: &worlds_rules::LayerBake) -> Vec<([u32; 2], [i32; 2], Option<Flip>)> {
    by_rule(b, "stamp")
        .iter()
        .map(|t| (t.tile, t.px, t.flip))
        .collect()
}

#[test]
fn stamp_placement() {
    let mut p = project();
    let l = level(&p, &["...", ".#.", "..."]);
    // Pivot 0,0: the anchor cell is the stamp's top-left.
    set_groups(&mut p, vec![group("g", vec![stamp_rule([0.0, 0.0])])]);
    assert_eq!(
        layout(&bake(&p, &l)),
        [
            ([4, 0], [16, 16], None),
            ([5, 0], [32, 16], None),
            ([4, 1], [16, 32], None),
            ([5, 1], [32, 32], None),
        ]
    );
    // Pivot 0.5,1: bottom-center sits on the anchor.
    set_groups(&mut p, vec![group("g", vec![stamp_rule([0.5, 1.0])])]);
    assert_eq!(
        layout(&bake(&p, &l)),
        [
            ([4, 0], [8, 0], None),
            ([5, 0], [24, 0], None),
            ([4, 1], [8, 16], None),
            ([5, 1], [24, 16], None),
        ]
    );
    // Single mode stacks every tile of the alternative on the anchor.
    let mut single = stamp_rule([0.5, 1.0]);
    tiles(&mut single).tile_mode = TileMode::Stack;
    set_groups(&mut p, vec![group("g", vec![single])]);
    assert!(layout(&bake(&p, &l)).iter().all(|t| t.1 == [16, 16]));
    // Layer tile_pivot shifts stamps by a fraction of a cell.
    p.layers[1].auto_mut().unwrap().tile_pivot = [0.5, 0.0];
    set_groups(&mut p, vec![group("g", vec![stamp_rule([0.0, 0.0])])]);
    assert_eq!(layout(&bake(&p, &l))[0].1, [24, 16]);
}

#[test]
fn flipped_stamp_mirrors_layout() {
    let mut p = project();
    // Fires only through the mirror_x variant (solid on the right of the anchor).
    let mut r = stamp_rule([0.0, 1.0]);
    r.as_match_mut().unwrap().pattern.rows = worlds_core::Rows::Inline(vec!["#.*".into()]);
    r.as_match_mut().unwrap().symmetry = vec![Symmetry::MirrorX];
    r.out_mut().unwrap().draw_mut().offset_px = [3, 0];
    set_groups(&mut p, vec![group("g", vec![r])]);
    let l = level(&p, &["...", "..#"]);
    // Anchor (1,1): right neighbour (2,1) is solid, left (0,1) empty: mirror_x only.
    let b = bake(&p, &l);
    let got = layout(&b);
    // Unflipped would be [4,0]@(16+3,0) [5,0]@(32+3,0) ... ; mirrored about the anchor cell:
    assert_eq!(
        got,
        [
            ([4, 0], [16 - 3, 0], Some(Flip::X)),
            ([5, 0], [0 - 3, 0], Some(Flip::X)),
            ([4, 1], [16 - 3, 16], Some(Flip::X)),
            ([5, 1], [0 - 3, 16], Some(Flip::X)),
        ]
    );
}

#[test]
fn named_stamp() {
    let mut p = project();
    let mut r = rule("stamp", &["+"], vec![]);
    r.output = worlds_core::Output::Draw(worlds_core::RuleOutput::stamp(slug("pillar")));
    set_groups(&mut p, vec![group("g", vec![r])]);
    let b = bake(&p, &level(&p, &[".#."]));
    // pillar: rect [4,0,2,2], pivot 0.5,1.
    assert_eq!(layout(&b)[0], ([4, 0], [8, -16], None));
    assert_eq!(layout(&b).len(), 4);
}

#[test]
fn weighted_alternatives() {
    let mut p = project();
    let r = rule(
        "w",
        &["+"],
        vec![
            TileAlt {
                tiles: vec![[1, 0]],
                weight: 1,
            },
            TileAlt {
                tiles: vec![[2, 0]],
                weight: 3,
            },
            TileAlt {
                tiles: vec![[3, 0]],
                weight: 0,
            },
        ],
    );
    set_groups(&mut p, vec![group("g", vec![r])]);
    let rows = vec!["#".repeat(100); 100];
    let rows: Vec<&str> = rows.iter().map(String::as_str).collect();
    let l = level(&p, &rows);
    let a = bake(&p, &l);
    assert_eq!(a, bake(&p, &l), "deterministic");
    let n = a.placements.len() as f64;
    let ones = a.placements.iter().filter(|t| t.tile == [1, 0]).count() as f64;
    assert!(
        a.placements.iter().all(|t| t.tile != [3, 0]),
        "weight 0 never picked"
    );
    assert!(
        (ones / n - 0.25).abs() < 0.02,
        "share of weight-1 alt: {}",
        ones / n
    );

    // A different level seed re-rolls.
    let mut l2 = l.clone();
    l2.layers.insert(
        slug("auto"),
        LayerState {
            seed: 99,
            ..Default::default()
        },
    );
    assert_eq!(l.layer_seed("auto"), 0, "seed is optional");
    assert_ne!(a, bake(&p, &l2));
}

#[test]
fn one_cell_edit_only_changes_nearby_tiles() {
    let mut p = project();
    let mut r = rule(
        "w",
        &["*+*", "+++", "*+*"],
        vec![TileAlt::single(1, 0), TileAlt::single(2, 0)],
    );
    r.as_match_mut().unwrap().chance = 0.7;
    set_groups(&mut p, vec![group("g", vec![r])]);
    let rows = vec!["#".repeat(30); 30];
    let rows: Vec<&str> = rows.iter().map(String::as_str).collect();
    let l = level(&p, &rows);
    let mut l2 = l.clone();
    l2.int_grids[0].set_index(15, 15, 0);
    let (a, b) = (bake(&p, &l), bake(&p, &l2));
    let key = |t: &worlds_rules::Placement| (t.anchor, t.tile);
    let sa: std::collections::BTreeSet<_> = a.placements.iter().map(key).collect();
    let sb: std::collections::BTreeSet<_> = b.placements.iter().map(key).collect();
    let diff: Vec<_> = sa.symmetric_difference(&sb).collect();
    assert!(!diff.is_empty());
    for (c, _) in diff {
        assert!((c[0] - 15).abs() <= 1 && (c[1] - 15).abs() <= 1, "{c:?}");
    }
}

#[test]
fn chance_and_perlin_gates() {
    let mut p = project();
    let mut r = one("c", &["+"]);
    r.as_match_mut().unwrap().chance = 0.3;
    set_groups(&mut p, vec![group("g", vec![r.clone()])]);
    let rows = vec!["#".repeat(100); 50];
    let rows: Vec<&str> = rows.iter().map(String::as_str).collect();
    let l = level(&p, &rows);
    let share = bake(&p, &l).placements.len() as f64 / 5000.0;
    assert!((share - 0.3).abs() < 0.03, "{share}");

    r.as_match_mut().unwrap().chance = 1.0;
    r.as_match_mut().unwrap().perlin = Some(worlds_core::Perlin {
        active: true,
        seed: 7,
        scale: 0.15,
        octaves: 2,
    });
    set_groups(&mut p, vec![group("g", vec![r.clone()])]);
    let n = bake(&p, &l).placements.len();
    assert!(
        (1000..4000).contains(&n),
        "perlin keeps a patchy subset: {n}"
    );
    r.as_match_mut().unwrap().perlin.as_mut().unwrap().active = false;
    set_groups(&mut p, vec![group("g", vec![r])]);
    assert_eq!(bake(&p, &l).placements.len(), 5000);
}

#[test]
fn random_offset_within_range_and_mirrored() {
    let mut p = project();
    let mut r = one("j", &["+"]);
    r.out_mut().unwrap().draw_mut().offset_px = [10, 0];
    r.out_mut().unwrap().draw_mut().random_offset_px = Some(RandomOffset {
        x: [-2, 2],
        y: [0, 3],
    });
    set_groups(&mut p, vec![group("g", vec![r])]);
    let rows = vec!["#".repeat(40); 10];
    let rows: Vec<&str> = rows.iter().map(String::as_str).collect();
    let b = bake(&p, &level(&p, &rows));
    let mut seen_x = std::collections::BTreeSet::new();
    for t in &b.placements {
        let dx = t.px[0] - t.anchor[0] * 16;
        let dy = t.px[1] - t.anchor[1] * 16;
        assert!((8..=12).contains(&dx) && (0..=3).contains(&dy), "{dx} {dy}");
        seen_x.insert(dx);
    }
    assert_eq!(seen_x.len(), 5);
}

#[test]
fn optional_and_inactive_groups() {
    let mut p = project();
    let mut opt = group("opt", vec![one("o", &["+"])]);
    opt.mode = GroupMode::Optional;
    let mut off = group("off", vec![one("i", &["+"])]);
    off.mode = GroupMode::Off;
    let mut inactive_rule = one("x", &["+"]);
    inactive_rule.active = false;
    set_groups(
        &mut p,
        vec![
            opt,
            off,
            group("base", vec![inactive_rule, one("b", &["+"])]),
        ],
    );
    let mut l = level(&p, &["#"]);
    assert_eq!(bake(&p, &l).fired[&[0, 0]], [slug("b")]);
    l.layers.insert(
        slug("auto"),
        LayerState {
            optional_groups: vec![slug("opt"), slug("off")],
            ..Default::default()
        },
    );
    // An `off` group stays off even when listed (validation flags the listing).
    assert_eq!(bake(&p, &l).fired[&[0, 0]], [slug("o"), slug("b")]);
    let names: Vec<_> = worlds_rules::applied_groups(&p, &l, "auto");
    assert_eq!(names, [&slug("opt"), &slug("base")]);
}

#[test]
fn level_field_requirement() {
    let mut p = project();
    let mut field = FieldDef::new(
        slug("biome"),
        FieldType::Enum(EnumSource::Inline(vec![slug("flooded"), slug("dark")])),
    );
    field.array = true;
    p.level_fields = vec![field];
    let req = |mode| LevelFieldRequirement {
        field: slug("biome"),
        values: vec![slug("flooded"), slug("dark")],
        mode,
    };
    let mut any = group("any", vec![one("any", &["+"])]);
    any.requires_level_field = Some(req(RequirementMode::Any));
    let mut all = group("all", vec![one("all", &["+"])]);
    all.requires_level_field = Some(req(RequirementMode::All));
    // Optional, but the requirement result is final when the level has the field.
    all.mode = GroupMode::Optional;
    set_groups(&mut p, vec![any, all]);

    let with = |v: serde_json::Value| {
        let mut l = level(&p, &["#"]);
        l.fields.insert(slug("biome"), v);
        bake(&p, &l).fired.get(&[0, 0]).cloned().unwrap_or_default()
    };
    assert_eq!(with(serde_json::json!(["flooded"])), [slug("any")]);
    assert_eq!(
        with(serde_json::json!(["dark", "flooded"])),
        [slug("any"), slug("all")]
    );
    assert_eq!(with(serde_json::json!([])), Vec::<worlds_core::Slug>::new());
    assert_eq!(
        with(serde_json::json!("dark")),
        [slug("any")],
        "scalar field"
    );

    // The field's default stands in for a missing value.
    p.level_fields[0].default = Some(serde_json::json!(["flooded", "dark"]));
    let l = level(&p, &["#"]);
    assert_eq!(bake(&p, &l).fired[&[0, 0]], [slug("any"), slug("all")]);

    // No such field anywhere: fall back to active/optional.
    p.level_fields.clear();
    let l = level(&p, &["#"]);
    assert_eq!(bake(&p, &l).fired[&[0, 0]], [slug("any")]);
}

#[test]
fn tileset_substitution() {
    let mut p = project();
    let mut from_alt = one("a", &["+"]);
    tiles(&mut from_alt).tileset = slug("alt");
    set_groups(&mut p, vec![group("g", vec![one("r", &["+"]), from_alt])]);
    let mut l = level(&p, &["#"]);
    let sets = |p: &worlds_core::Project, l: &worlds_core::Level| -> Vec<String> {
        bake(p, l)
            .placements
            .iter()
            .map(|t| t.tileset.to_string())
            .collect()
    };
    assert_eq!(sets(&p, &l), ["alt", "main"]);
    // Only rules drawing from `main` switch; `alt` rules keep theirs.
    l.layers.insert(
        slug("auto"),
        LayerState {
            tilesets: [(slug("main"), slug("alt"))].into(),
            ..Default::default()
        },
    );
    assert_eq!(sets(&p, &l), ["alt", "alt"]);
    l.layers.get_mut("auto").unwrap().tilesets = [(slug("alt"), slug("main"))].into();
    assert_eq!(sets(&p, &l), ["main", "main"]);
}

#[test]
fn stamp_layout_steps_by_tile_size() {
    // 32 px tiles on a 16 px layer: a 2x2 stamp covers 64x64 px.
    let mut p = project();
    p.tilesets[0].tile_size = [32, 32];
    let l = level(&p, &["...", ".#.", "..."]);
    set_groups(&mut p, vec![group("g", vec![stamp_rule([0.0, 0.0])])]);
    assert_eq!(
        layout(&bake(&p, &l)),
        [
            ([4, 0], [16, 16], None),
            ([5, 0], [48, 16], None),
            ([4, 1], [16, 48], None),
            ([5, 1], [48, 48], None),
        ]
    );
    // Pivot on the pixel box: bottom-center of the 64 px box on the
    // bottom-center of the 16 px cell.
    set_groups(&mut p, vec![group("g", vec![stamp_rule([0.5, 1.0])])]);
    let got = layout(&bake(&p, &l));
    assert_eq!(got[0].1, [16 + 8 - 32, 16 + 16 - 64]);
    assert_eq!(got[3].1, [16 + 8, 16 + 16 - 32]);
    // Spacing only moves where tiles are cut, not where they go.
    p.tilesets[0].spacing = 2;
    assert_eq!(layout(&bake(&p, &l)), got);

    // A mirrored 32 px tile on a 16 px cell mirrors about the cell center.
    let mut r = one("m", &["#.*"]);
    r.as_match_mut().unwrap().symmetry = vec![Symmetry::MirrorX];
    set_groups(&mut p, vec![group("g", vec![r])]);
    let b = bake(&p, &level(&p, &[".#"]));
    let got: Vec<_> = by_rule(&b, "m").iter().map(|t| (t.px, t.flip)).collect();
    assert_eq!(got, [([16 - 32, 0], Some(Flip::X))]);
}

#[test]
fn rules_hash_by_slug() {
    // Same pattern and tiles, different slugs: different picks somewhere.
    let mut p = project();
    let alts = vec![TileAlt::single(1, 0), TileAlt::single(2, 0)];
    set_groups(
        &mut p,
        vec![group("g", vec![rule("a", &["+"], alts.clone())])],
    );
    let rows = vec!["#".repeat(40); 10];
    let rows: Vec<&str> = rows.iter().map(String::as_str).collect();
    let l = level(&p, &rows);
    let picks =
        |b: worlds_rules::LayerBake| b.placements.iter().map(|t| t.tile).collect::<Vec<_>>();
    let a = picks(bake(&p, &l));
    set_groups(
        &mut p,
        vec![group("g", vec![rule("b", &["+"], alts.clone())])],
    );
    assert_ne!(a, picks(bake(&p, &l)));
    // A salt re-rolls without renaming; salt 0 is the default.
    let mut salted = rule("a", &["+"], alts);
    salted.salt = 7;
    set_groups(&mut p, vec![group("g", vec![salted])]);
    assert_ne!(a, picks(bake(&p, &l)));
}

#[test]
fn bake_level_covers_auto_layers_only() {
    let mut p = project();
    set_groups(&mut p, vec![group("g", vec![one("r", &["+"])])]);
    let l = level(&p, &["#."]);
    let all = worlds_rules::bake_level(&p, &l, &images()).unwrap();
    assert_eq!(all.keys().collect::<Vec<_>>(), [&slug("auto")]);
    let e = worlds_rules::bake_layer(&p, &l, &slug("collision"), &images()).unwrap_err();
    assert!(matches!(e, worlds_rules::Error::NotAutoLayer(_)));
}

#[test]
fn full_layer_timing_sanity() {
    // 64x28 room, 50 rules of 3x3 patterns with mirrors. Release target is
    // well under 10 ms; debug builds get a loose bound.
    let mut p = project();
    let rules: Vec<_> = (0..50)
        .map(|i| {
            let rows: &[&str] = match i % 3 {
                0 => &["...", ".#.", "###"],
                1 => &["#..", "##.", "###"],
                _ => &["*+*", "+.+", "*+*"],
            };
            let mut r = rule(
                &format!("r{i}"),
                rows,
                vec![TileAlt::single(1, 1), TileAlt::single(2, 1)],
            );
            r.as_match_mut().unwrap().symmetry = vec![Symmetry::MirrorX, Symmetry::MirrorY];
            r.as_match_mut().unwrap().chance = 0.9;
            r
        })
        .collect();
    set_groups(&mut p, vec![group("g", rules)]);
    let rows: Vec<String> = (0..28)
        .map(|y| {
            (0..64)
                .map(|x| {
                    if (x * 7 + y * 3) % 5 < 2 || y > 24 {
                        '#'
                    } else {
                        '.'
                    }
                })
                .collect()
        })
        .collect();
    let rows: Vec<&str> = rows.iter().map(String::as_str).collect();
    let l = level(&p, &rows);
    let imgs = images();
    let _ = worlds_rules::bake_layer(&p, &l, &slug("auto"), &imgs).unwrap();
    let t = std::time::Instant::now();
    let b = worlds_rules::bake_layer(&p, &l, &slug("auto"), &imgs).unwrap();
    let ms = t.elapsed().as_secs_f64() * 1000.0;
    eprintln!("64x28, 50 rules: {ms:.3} ms, {} tiles", b.placements.len());
    assert!(!b.placements.is_empty());
    let limit = if cfg!(debug_assertions) { 500.0 } else { 10.0 };
    assert!(ms < limit, "bake took {ms:.2} ms");
}
