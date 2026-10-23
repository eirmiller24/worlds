//! M3 engine features: `@` anchors, `require`, `rotate` (8 orientations),
//! templates and named macros, `rule_matches_many`, the built-in `blob47`.

mod common;

use common::*;
use serde_json::json;
use worlds_core::{
    Flip, IntGridDef, IntGridLayer, IntGridValue, LayerDef, LayerKind, Level, Macros, Project,
    Rule, RuleEntry, RuleGroup, Slug, TilesetDef, slug,
};

fn rule_json(v: serde_json::Value) -> Rule {
    serde_json::from_value(v).unwrap()
}

fn group_of(entries: Vec<serde_json::Value>) -> RuleGroup {
    serde_json::from_value(json!({ "slug": "g", "rules": entries })).unwrap()
}

/// `(tile, px, flip)` of every placement of `rule`, in draw order.
fn layout(b: &worlds_rules::LayerBake, rule: &str) -> Vec<([u32; 2], [i32; 2], Option<Flip>)> {
    by_rule(b, rule)
        .iter()
        .map(|t| (t.tile, t.px, t.flip))
        .collect()
}

/// The common project plus an int_grid layer `decor` (`t` torch, `b` banner).
fn with_decor() -> Project {
    let mut p = project();
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
    p
}

fn level_with_decor(p: &Project, col: &[&str], decor: &[&str]) -> Level {
    let mut l = level(p, col);
    l.int_grids
        .push(IntGridLayer::from_rows(p.layer("decor").unwrap(), decor).unwrap());
    l
}

// ---- @ anchors ----

#[test]
fn at_anchor_allows_even_sizes_and_takes_a_legend_test() {
    let mut p = project();
    // 2x2 block: solid right of, below and below-right of an empty anchor.
    let r = rule_json(json!({
        "slug": "corner",
        "match": { "legend": { "@": "." }, "rows": ["@#", "##"] },
        "out": { "tileset": "main", "tiles": [[1, 1]] }
    }));
    set_groups(&mut p, vec![group("g", vec![r])]);
    let l = level(&p, &[".##", "###", "..."]);
    assert_eq!(fired_at(&bake(&p, &l), "corner"), [[0, 0]]);
}

#[test]
fn at_anchor_without_a_legend_entry_is_dont_care() {
    let mut p = project();
    // `@` below a solid cell, anywhere: rows ["#", "@"] anchored on the bottom.
    let r = rule_json(json!({
        "slug": "under",
        "match": { "rows": ["#", "@"] },
        "out": { "tileset": "main", "tiles": [[1, 1]] }
    }));
    set_groups(&mut p, vec![group("g", vec![r])]);
    let l = level(&p, &["#.#", "#..", "..."]);
    assert_eq!(fired_at(&bake(&p, &l), "under"), [[0, 1], [0, 2], [2, 1]]);
}

// ---- require ----

#[test]
fn require_reads_its_own_grid_on_the_same_anchor() {
    let mut p = with_decor();
    let r = rule_json(json!({
        "slug": "torch",
        "match": { "legend": { "@": "." }, "rows": ["#", "@"] },
        "require": { "grid": "decor", "rows": ["t"] },
        "out": { "tileset": "main", "tiles": [[3, 3]] }
    }));
    set_groups(&mut p, vec![group("g", vec![r])]);
    let l = level_with_decor(&p, &["####", "....", "...."], &["....", "tb.t", "t..."]);
    // (0,1) and (3,1): empty, solid above, torch on the cell. (1,1) has a
    // banner, (0,2) has a torch but no solid above.
    assert_eq!(fired_at(&bake(&p, &l), "torch"), [[0, 1], [3, 1]]);
    let m = worlds_rules::rule_matches(&p, &l, &slug("auto"), "torch", &images()).unwrap();
    assert_eq!(m.keys().copied().collect::<Vec<_>>(), [[0, 1], [3, 1]]);
}

#[test]
fn require_has_its_own_anchor_and_shares_out_of_bounds() {
    let mut p = with_decor();
    // require: a torch one cell to the right of the anchor (`@` at left).
    let mut r = rule_json(json!({
        "slug": "r",
        "match": { "rows": ["#"] },
        "require": { "grid": "decor", "rows": ["@t"] },
        "out": { "tileset": "main", "tiles": [[3, 3]] }
    }));
    set_groups(&mut p, vec![group("g", vec![r.clone()])]);
    let l = level_with_decor(&p, &["###"], &[".t."]);
    assert_eq!(fired_at(&bake(&p, &l), "r"), [[0, 0]]);
    // Off the level the require cell reads as out_of_bounds (here: torch).
    r.as_match_mut().unwrap().out_of_bounds = Some(serde_json::from_value(json!("torch")).unwrap());
    set_groups(&mut p, vec![group("g", vec![r])]);
    // `torch` isn't a collision value: out_of_bounds must name a value of
    // both grids, so this is a bake error, not a silent miss.
    let e = worlds_rules::bake_layer(&p, &l, &slug("auto"), &images()).unwrap_err();
    assert!(
        matches!(e, worlds_rules::Error::UnknownOutOfBounds { .. }),
        "{e}"
    );
}

// ---- rotate ----

/// The 3x3-free pattern used below: anchor `#`, water east, fortified
/// south-east. No two of its 8 orientations read the same cells.
fn turning_rule(symmetry: &[&str]) -> Rule {
    rule_json(json!({
        "slug": "turn",
        "match": { "legend": { "@": "solid" }, "rows": ["@~", "*f"] },
        "symmetry": symmetry,
        "out": { "tileset": "main", "tiles": [[4, 0, 2, 1]], "tile_mode": "stamp", "pivot": [0, 0] }
    }))
}

/// A 5x5 level with solid at the center and the pattern's two marks turned
/// by `bits` (transpose first, then mirror x, then mirror y).
fn turned_level(p: &Project, bits: u8) -> Level {
    let turn = |(x, y): (i32, i32)| {
        let (mut x, mut y) = if bits & 4 != 0 { (y, x) } else { (x, y) };
        if bits & 1 != 0 {
            x = -x;
        }
        if bits & 2 != 0 {
            y = -y;
        }
        ((2 + x) as usize, (2 + y) as usize)
    };
    let mut rows = vec![vec!['.'; 5]; 5];
    rows[2][2] = '#';
    let (wx, wy) = turn((1, 0));
    rows[wy][wx] = '~';
    let (fx, fy) = turn((1, 1));
    rows[fy][fx] = 'f';
    let rows: Vec<String> = rows.into_iter().map(|r| r.into_iter().collect()).collect();
    let rows: Vec<&str> = rows.iter().map(String::as_str).collect();
    level(p, &rows)
}

/// Variant index to its flip bits (design table: 4 = cw, 5 = ccw, 6 = t, 7).
const BITS: [u8; 8] = [0, 1, 2, 3, 5, 6, 4, 7];

#[test]
fn rotate_with_a_mirror_gives_all_eight_orientations_with_exact_stamp_pixels() {
    let mut p = project();
    let mut r = turning_rule(&["rotate", "mirror_x"]);
    r.out_mut().unwrap().draw_mut().offset_px = [3, 1];
    set_groups(&mut p, vec![group("g", vec![r.clone()])]);
    // 2x1 stamp (tiles [4,0] [5,0]) on 16 px cells, anchor (2,2) = (32,32),
    // offset (3,1): unturned the tiles' top-lefts are (3,1) and (19,1) from
    // the anchor cell. Each orientation turns those boxes about the cell's
    // center (8,8); with tiles the size of a cell that turns the vectors.
    let expect: [(Option<Flip>, [[i32; 2]; 2]); 8] = [
        (None, [[3, 1], [19, 1]]),
        (Some(Flip::X), [[-3, 1], [-19, 1]]),
        (Some(Flip::Y), [[3, -1], [19, -1]]),
        (Some(Flip::XY), [[-3, -1], [-19, -1]]),
        (Some(Flip::XT), [[-1, 3], [-1, 19]]),
        (Some(Flip::YT), [[1, -3], [1, -19]]),
        (Some(Flip::T), [[1, 3], [1, 19]]),
        (Some(Flip::XYT), [[-1, -3], [-1, -19]]),
    ];
    // The same through random_offset_px with one-value ranges.
    let mut random = r.clone();
    random.out_mut().unwrap().draw_mut().offset_px = [0, 0];
    random.out_mut().unwrap().draw_mut().random_offset_px = Some(worlds_core::RandomOffset {
        x: [3, 3],
        y: [1, 1],
    });
    for rule in [r, random] {
        set_groups(&mut p, vec![group("g", vec![rule])]);
        for (v, (flip, px)) in expect.iter().enumerate() {
            let l = turned_level(&p, BITS[v]);
            let got = layout(&bake(&p, &l), "turn");
            let want: Vec<_> = [[4u32, 0], [5, 0]]
                .into_iter()
                .zip(px)
                .map(|(t, o)| (t, [32 + o[0], 32 + o[1]], *flip))
                .collect();
            assert_eq!(got, want, "variant {v}");
            let flips =
                worlds_rules::rule_matches(&p, &l, &slug("auto"), "turn", &images()).unwrap();
            assert_eq!(flips.get(&[2, 2]), Some(&vec![*flip]), "variant {v}");
        }
    }
}

#[test]
fn rotate_alone_gives_the_four_turns() {
    let mut p = project();
    set_groups(&mut p, vec![group("g", vec![turning_rule(&["rotate"])])]);
    let fires: Vec<bool> = BITS
        .iter()
        .map(|&b| !bake(&p, &turned_level(&p, b)).placements.is_empty())
        .collect();
    // as written, half turn, quarter turns; not the mirrors or transposes.
    assert_eq!(fires, [true, false, false, true, true, true, false, false]);
    // mirror_x and mirror_y alone keep their old meaning.
    set_groups(&mut p, vec![group("g", vec![turning_rule(&["mirror_x"])])]);
    let fires: Vec<bool> = BITS
        .iter()
        .map(|&b| !bake(&p, &turned_level(&p, b)).placements.is_empty())
        .collect();
    assert_eq!(
        fires,
        [true, true, false, false, false, false, false, false]
    );
}

#[test]
fn rotate_never_stacks_orientations_that_read_the_same_cells() {
    let mut p = project();
    // A plus shape is symmetric under all 8: one placement per anchor.
    let r = rule_json(json!({
        "slug": "plus",
        "match": { "rows": ["*#*", "###", "*#*"] },
        "symmetry": ["rotate", "mirror_x", "mirror_y"],
        "out": { "tileset": "main", "tiles": [[1, 1]] }
    }));
    set_groups(&mut p, vec![group("g", vec![r])]);
    let b = bake(&p, &level(&p, &[".#.", "###", ".#."]));
    assert_eq!(layout(&b, "plus"), [([1, 1], [16, 16], None)]);
    // An L shape under rotate: four distinct orientations, each fires once.
    let r = rule_json(json!({
        "slug": "ell",
        "match": { "rows": ["*#*", "*##", "***"] },
        "symmetry": ["rotate"],
        "out": { "tileset": "main", "tiles": [[1, 1]] }
    }));
    set_groups(&mut p, vec![group("g", vec![r])]);
    let b = bake(&p, &level(&p, &[".#.", "###", ".#."]));
    let flips: Vec<_> = layout(&b, "ell")
        .into_iter()
        .filter(|t| t.1 == [16, 16])
        .map(|t| t.2)
        .collect();
    assert_eq!(
        flips,
        [None, Some(Flip::XY), Some(Flip::XT), Some(Flip::YT)]
    );
}

#[test]
fn rotate_on_non_square_tiles_is_an_error_not_a_panic() {
    let mut p = project();
    p.tilesets.push(TilesetDef {
        slug: slug("tall"),
        image: "tall.png".into(),
        tile_size: [16, 32],
        spacing: 0,
        padding: 0,
    });
    let r = rule_json(json!({
        "slug": "r",
        "match": { "rows": ["#~"] , "legend": {} },
        "symmetry": ["rotate"],
        "out": { "tileset": "tall", "tiles": [[0, 0]] }
    }));
    set_groups(&mut p, vec![group("g", vec![r])]);
    let l = level(&p, &["#~."]);
    let e = worlds_rules::bake_layer(&p, &l, &slug("auto"), &images()).unwrap_err();
    assert!(
        matches!(e, worlds_rules::Error::RotateNeedsSquareTiles { .. }),
        "{e}"
    );
    assert!(e.to_string().contains("16x32"), "{e}");
    // Same through a level's tileset substitution.
    let r = rule_json(json!({
        "slug": "r",
        "match": { "rows": ["#~"] },
        "symmetry": ["rotate"],
        "out": { "tileset": "main", "tiles": [[0, 0]] }
    }));
    set_groups(&mut p, vec![group("g", vec![r])]);
    let mut l = level(&p, &["#~."]);
    l.layers.entry(slug("auto")).or_default().tilesets =
        [(slug("main"), slug("tall"))].into_iter().collect();
    assert!(worlds_rules::bake_layer(&p, &l, &slug("auto"), &images()).is_err());
}

// ---- templates and macros ----

fn frame_macros() -> Macros {
    serde_json::from_value(json!({
        "patterns": { "top": ["*.*", "*#*", "***"] },
        "legends": { "walls": { "#": "walls", "S": "!walls" } },
        "templates": {
            "frame": {
                "legend": "walls",
                "rules": [
                    { "slug": "top", "match": { "rows": "top", "legend": { ".": "!walls" } },
                      "out": { "tiles": [[1, 0], [2, 0]] }, "break": true },
                    { "slug": "fill", "match": { "rows": ["#"] },
                      "out": { "tiles": [[1, 1]] }, "break": true },
                    { "slug": "fixed", "match": { "rows": ["~"] },
                      "out": { "tileset": "main", "tiles": [[7, 7]] } }
                ]
            }
        }
    }))
    .unwrap()
}

#[test]
fn a_template_reference_bakes_its_expanded_rules() {
    let mut p = project();
    p.macros = Some(frame_macros());
    p.rules[0].groups = vec![group_of(vec![json!({
        "use": "frame", "slug": "walls", "tileset": "alt", "origin": [2, 3],
        "tiles": { "fill": [[0, 5]] }
    })])];
    let l = level(&p, &["...", "#f#", "##~"]);
    let b = bake(&p, &l);
    // The template rule's own legend makes `.` mean !walls (over the
    // template legend), so the row above only has to be open.
    assert_eq!(fired_at(&b, "walls__top"), [[0, 1], [1, 1], [2, 1]]);
    assert_eq!(fired_at(&b, "walls__fill"), [[0, 2], [1, 2]]);
    assert_eq!(fired_at(&b, "walls__fixed"), [[2, 2]]);
    for t in by_rule(&b, "walls__top") {
        assert_eq!(t.tileset, "alt");
        assert!(t.tile == [3, 3] || t.tile == [4, 3], "{:?}", t.tile);
    }
    // The override is relative to origin too.
    assert!(by_rule(&b, "walls__fill").iter().all(|t| t.tile == [2, 8]));
    // A fixed rule keeps its own tileset and absolute tiles.
    let f = by_rule(&b, "walls__fixed");
    assert_eq!((f[0].tileset.as_str(), f[0].tile), ("main", [7, 7]));
    assert!(b.placements.iter().all(|t| t.group == "g"));

    // skip, a legend remap, active.
    p.rules[0].groups = vec![group_of(vec![json!({
        "use": "frame", "slug": "walls", "tileset": "alt",
        "skip": ["fixed"], "legend": { "#": "fortified", "S": "!fortified" }
    })])];
    let b = bake(&p, &l);
    assert_eq!(fired_at(&b, "walls__top"), [[1, 1]]);
    assert!(fired_at(&b, "walls__fixed").is_empty());
    p.rules[0].groups = vec![group_of(vec![json!({
        "use": "frame", "slug": "walls", "tileset": "alt", "active": false
    })])];
    assert!(bake(&p, &l).placements.is_empty());
}

#[test]
fn named_rows_and_legends_in_an_inline_rule() {
    let mut p = project();
    p.macros = Some(frame_macros());
    let r = rule_json(json!({
        "slug": "top",
        "match": { "rows": "top", "legend": "walls" },
        "out": { "tileset": "main", "tiles": [[1, 1]] }
    }));
    set_groups(&mut p, vec![group("g", vec![r])]);
    // The named legend doesn't define `.`, so it means empty.
    let b = bake(&p, &level(&p, &[".~.", "#f#"]));
    assert_eq!(fired_at(&b, "top"), [[0, 1], [2, 1]]);
}

#[test]
fn an_unknown_template_is_a_bake_error_naming_it() {
    let mut p = project();
    p.rules[0].groups = vec![group_of(vec![json!({ "use": "nope", "tileset": "main" })])];
    let l = level(&p, &["#"]);
    let e = worlds_rules::bake_layer(&p, &l, &slug("auto"), &images()).unwrap_err();
    assert!(matches!(e, worlds_rules::Error::Expand { .. }), "{e}");
    assert!(e.to_string().contains("\"nope\""), "{e}");
    let e = worlds_rules::rule_matches(&p, &l, &slug("auto"), "nope", &images()).unwrap_err();
    assert!(matches!(e, worlds_rules::Error::Expand { .. }), "{e}");
}

#[test]
fn rule_matches_many_takes_a_reference_slug() {
    let mut p = project();
    p.macros = Some(frame_macros());
    p.rules[0].groups = vec![group_of(vec![
        json!({ "use": "frame", "slug": "walls", "tileset": "alt", "skip": ["fixed"] }),
        json!({ "slug": "solo", "match": { "rows": ["~"] },
                "out": { "tileset": "main", "tiles": [[1, 1]] } }),
    ])];
    let l = level(&p, &["...", "#f#", "##~"]);
    let auto = slug("auto");
    let many = worlds_rules::rule_matches_many(&p, &l, &auto, "walls", &images()).unwrap();
    let names: Vec<&str> = many.iter().map(|(s, _)| s.as_str()).collect();
    assert_eq!(names, ["walls__top", "walls__fill"]);
    // Matches ignore break: `fill` (`#`) matches on the top row too.
    assert_eq!(many[1].1.len(), 5);
    let one = worlds_rules::rule_matches_many(&p, &l, &auto, "walls__fill", &images()).unwrap();
    assert_eq!(one, many[1..]);
    let union = worlds_rules::rule_matches(&p, &l, &auto, "walls", &images()).unwrap();
    assert_eq!(union.len(), 5);
    assert_eq!(union[&[0, 1]], [None, None]);
    assert!(
        worlds_rules::rule_matches_many(&p, &l, &auto, "nothing", &images())
            .unwrap()
            .is_empty()
    );
    // Every rule at once (one compile) is every entry's list, in order.
    let all = worlds_rules::rule_matches_all(&p, &l, &auto, &images()).unwrap();
    let mut each = many.clone();
    each.extend(worlds_rules::rule_matches_many(&p, &l, &auto, "solo", &images()).unwrap());
    assert_eq!(all, each);
    // A skipped rule doesn't exist; an inactive group's rules match nowhere.
    p.rules[0].groups[0].mode = worlds_core::GroupMode::Off;
    let many = worlds_rules::rule_matches_many(&p, &l, &auto, "walls", &images()).unwrap();
    assert_eq!(many.len(), 2);
    assert!(many.iter().all(|(_, m)| m.is_empty()));
}

// ---- blob47 ----

/// Neighbour bits of the blob47 template's `m<mask>` names.
const DIRS: [(i32, i32, u32); 8] = [
    (0, -1, 1),
    (1, -1, 2),
    (1, 0, 4),
    (1, 1, 8),
    (0, 1, 16),
    (-1, 1, 32),
    (-1, 0, 64),
    (-1, -1, 128),
];

#[test]
fn blob47_draws_the_piece_of_every_neighbourhood_and_nothing_else() {
    let mut p = project();
    // Every 3x3 neighbourhood once: 512 blocks side by side, so each
    // block's center cell sees exactly its block. Filled cells are `#` or
    // `f` (walls), open ones `.` or `~`, mixed so the remap is exercised.
    let (bw, bh) = (32usize, 16usize);
    let (w, h) = (bw * 3, bh * 3);
    let mut grid = vec!['.'; w * h];
    for n in 0..512usize {
        let (bx, by) = (n % bw * 3, n / bw * 3);
        for i in 0..9 {
            let filled = n >> i & 1 == 1;
            let alt = (n + i) % 3 == 0;
            grid[(by + i / 3) * w + bx + i % 3] = match (filled, alt) {
                (true, false) => '#',
                (true, true) => 'f',
                (false, false) => '.',
                (false, true) => '~',
            };
        }
    }
    let rows: Vec<String> = grid.chunks(w).map(|r| r.iter().collect()).collect();
    let rows: Vec<&str> = rows.iter().map(String::as_str).collect();
    p.rules[0].groups = vec![group_of(vec![json!({
        "use": "blob47", "slug": "moss", "tileset": "main", "origin": [0, 8],
        "legend": { "#": "walls", "S": "!walls" }
    })])];
    let l = level(&p, &rows);
    let b = bake(&p, &l);

    // The template's own tile for each mask (12x4 block at origin [0, 8]).
    let blob = Macros::builtin_templates().get("blob47").unwrap();
    let tile_of: std::collections::BTreeMap<u32, [u32; 2]> = blob
        .rules
        .iter()
        .map(|r| {
            let mask: u32 = r.slug.as_str()[1..].parse().unwrap();
            let worlds_core::TemplateOutput::Tiles(o) = &r.out else {
                panic!("tiles")
            };
            let t = o.tiles[0].tiles[0];
            (mask, [t[0], t[1] + 8])
        })
        .collect();
    assert_eq!(tile_of.len(), 47);
    let filled = |x: i32, y: i32| {
        x >= 0
            && y >= 0
            && (x as usize) < w
            && (y as usize) < h
            && matches!(grid[y as usize * w + x as usize], '#' | 'f')
    };
    let mut seen = std::collections::BTreeSet::new();
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            let fired = b.fired.get(&[x, y]);
            if !filled(x, y) {
                assert_eq!(fired, None, "open cell ({x},{y})");
                continue;
            }
            let mut mask = 0;
            for (dx, dy, bit) in DIRS {
                let corner = dx != 0 && dy != 0;
                if filled(x + dx, y + dy) && (!corner || (filled(x + dx, y) && filled(x, y + dy))) {
                    mask |= bit;
                }
            }
            let name = Slug::new(format!("moss__m{mask}")).unwrap();
            assert_eq!(
                fired,
                Some(&vec![name.clone()]),
                "cell ({x},{y}) mask {mask}"
            );
            let at: Vec<_> = b.placements.iter().filter(|t| t.anchor == [x, y]).collect();
            assert_eq!(at.len(), 1);
            assert_eq!(at[0].tile, tile_of[&mask], "cell ({x},{y}) mask {mask}");
            seen.insert(mask);
        }
    }
    assert_eq!(seen.len(), 47, "every piece fires somewhere");
}

#[test]
fn group_of_inline_entries_still_bakes_like_before() {
    // RuleEntry::Rule entries next to a reference keep their order.
    let mut p = project();
    p.macros = Some(frame_macros());
    let mut g = group_of(vec![
        json!({ "use": "frame", "slug": "w", "tileset": "alt" }),
    ]);
    g.rules.insert(
        0,
        RuleEntry::Rule(rule(
            "first",
            &["#"],
            vec![worlds_core::TileAlt::single(0, 0)],
        )),
    );
    p.rules[0].groups = vec![g];
    let b = bake(&p, &level(&p, &["#"]));
    // `first` has an opaque tile and comes first: it locks the cell.
    assert_eq!(b.fired[&[0, 0]], [slug("first")]);
}
