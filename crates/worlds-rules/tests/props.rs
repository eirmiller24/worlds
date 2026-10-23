//! Property tests for the M3 engine features: each new construct bakes
//! exactly like an equivalent written the old way, and `rotate` commutes
//! with turning the level.

mod common;

use std::collections::BTreeMap;

use common::*;
use proptest::prelude::*;
use proptest::sample::select;
use serde_json::{Value, json};
use worlds_core::{Flip, Level, Project, Rule, RuleGroup};
use worlds_rules::LayerBake;

fn rule_json(v: Value) -> Rule {
    serde_json::from_value(v).unwrap()
}

/// A collision grid of `#` `f` `~` `.` (solid and empty more often).
fn grid() -> impl Strategy<Value = Vec<String>> {
    (3..9usize, 3..8usize).prop_flat_map(|(w, h)| {
        prop::collection::vec(select(vec!['#', '#', '.', '.', 'f', '~']), w * h)
            .prop_map(move |c| c.chunks(w).map(|r| r.iter().collect()).collect())
    })
}

/// 3x3 rows of pattern glyphs, `*` often.
fn rows3(glyphs: &'static [char]) -> impl Strategy<Value = Vec<String>> {
    prop::collection::vec(select(glyphs.to_vec()), 9)
        .prop_map(|c| c.chunks(3).map(|r| r.iter().collect()).collect())
}

const GLYPHS: &[char] = &['*', '*', '*', '#', '.', 'f', '~', '+'];

fn symmetry() -> impl Strategy<Value = Vec<&'static str>> {
    select(vec![
        vec![],
        vec!["mirror_x"],
        vec!["mirror_x", "mirror_y"],
        vec!["rotate"],
        vec!["rotate", "mirror_y"],
    ])
}

fn oob() -> impl Strategy<Value = Option<&'static str>> {
    select(vec![None, Some("."), Some("solid")])
}

fn lvl(p: &Project, rows: &[String]) -> Level {
    let rows: Vec<&str> = rows.iter().map(String::as_str).collect();
    level(p, &rows)
}

fn with_rules(rules: Vec<Rule>) -> Project {
    let mut p = project();
    set_groups(&mut p, vec![group("g", rules)]);
    p
}

fn same_bake(a: &LayerBake, b: &LayerBake) -> Result<(), TestCaseError> {
    prop_assert_eq!(&a.fired, &b.fired);
    prop_assert_eq!(&a.placements, &b.placements);
    Ok(())
}

/// A rule's common keys: random alternatives, chance and break, so the
/// hash streams and the break rule take part.
fn out_json(alts: usize, chance: f64, brk: bool) -> (Value, f64, bool) {
    let tiles: Vec<Value> = (0..alts).map(|i| json!([1 + i as u32, 1])).collect();
    (json!({ "tileset": "main", "tiles": tiles }), chance, brk)
}

/// `rows` turned a quarter clockwise: cell `(x, y)` moves to `(h-1-y, x)`.
fn rotate_cw(rows: &[String]) -> Vec<String> {
    let g: Vec<Vec<char>> = rows.iter().map(|r| r.chars().collect()).collect();
    let (w, h) = (g[0].len(), g.len());
    (0..w)
        .map(|y| (0..h).map(|x| g[h - 1 - x][y]).collect())
        .collect()
}

/// The 8 orientations' offset maps, by flip bits.
fn turn(bits: u8, (x, y): (i32, i32)) -> (i32, i32) {
    let (mut x, mut y) = if bits & 4 != 0 { (y, x) } else { (x, y) };
    if bits & 1 != 0 {
        x = -x;
    }
    if bits & 2 != 0 {
        y = -y;
    }
    (x, y)
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 96, ..ProptestConfig::default() })]

    /// Baking a level turned 90 degrees clockwise gives the original
    /// placements turned: anchor `(x, y)` at `(h-1-y, x)`, and each flip
    /// followed by the quarter turn (transpose, then mirror x).
    #[test]
    fn rotate_commutes_with_turning_the_level(
        rows in grid(),
        pat in rows3(GLYPHS),
        mirror in any::<bool>(),
        oob in oob(),
    ) {
        // Only patterns whose enabled orientations all read different
        // cells: otherwise which copy of a symmetric pattern is kept
        // depends on the variant order, which a turn doesn't preserve.
        let cells: Vec<((i32, i32), char)> = pat.iter().enumerate().flat_map(|(y, r)| {
            r.chars().enumerate().filter(|(_, c)| *c != '*')
                .map(move |(x, c)| ((x as i32 - 1, y as i32 - 1), c))
        }).collect();
        let bits: &[u8] = if mirror { &[0, 1, 2, 3, 5, 6, 4, 7] } else { &[0, 3, 5, 6] };
        let mut keys: Vec<Vec<((i32, i32), char)>> = bits.iter().map(|&b| {
            let mut k: Vec<_> = cells.iter().map(|&(d, c)| (turn(b, d), c)).collect();
            k.sort();
            k
        }).collect();
        keys.sort();
        keys.dedup();
        prop_assume!(keys.len() == bits.len());

        let symmetry: &[&str] = if mirror { &["rotate", "mirror_x"] } else { &["rotate"] };
        let mut r = json!({
            "slug": "r", "match": { "rows": pat }, "symmetry": symmetry,
            "out": { "tileset": "main", "tiles": [[1, 1]] }
        });
        if let Some(o) = oob { r["out_of_bounds"] = json!(o); }
        let p = with_rules(vec![rule_json(r)]);
        let a = bake(&p, &lvl(&p, &rows));
        let b = bake(&p, &lvl(&p, &rotate_cw(&rows)));
        let h = rows.len() as i32;
        let quarter = |f: Option<Flip>| Flip::mirror_x(Flip::transpose(f));
        let mut want: Vec<([i32; 2], [u32; 2], Option<Flip>)> = a.placements.iter()
            .map(|t| ([h - 1 - t.anchor[1], t.anchor[0]], t.tile, quarter(t.flip)))
            .collect();
        let mut got: Vec<_> = b.placements.iter().map(|t| (t.anchor, t.tile, t.flip)).collect();
        for t in &b.placements {
            prop_assert_eq!(t.px, [t.anchor[0] * 16, t.anchor[1] * 16]);
        }
        want.sort_by_key(|t| (t.0, Flip::bits_of(t.2)));
        got.sort_by_key(|t| (t.0, Flip::bits_of(t.2)));
        prop_assert_eq!(got, want);
    }

    /// A template reference bakes byte-identically to its rules written
    /// inline with the expanded slugs, legends, tileset and shifted tiles.
    #[test]
    fn template_expansion_equals_rules_written_inline(
        rows in grid(),
        pats in prop::collection::vec(rows3(&['*', '*', '#', 'S', '.', '+']), 1..4),
        alts in prop::collection::vec(1..3usize, 3),
        chance in select(vec![1.0, 0.5]),
        brk in prop::collection::vec(any::<bool>(), 3),
        sym in symmetry(),
        origin in (0..4u32, 0..4u32),
        remap in any::<bool>(),
        skip_first in any::<bool>(),
        override_last in any::<bool>(),
    ) {
        let n = pats.len();
        let template_rules: Vec<Value> = (0..n).map(|i| {
            let (out, chance, brk) = out_json(alts[i], chance, brk[i]);
            json!({ "slug": format!("t{i}"), "match": { "rows": pats[i] },
                    "symmetry": sym, "out": { "tiles": out["tiles"] },
                    "chance": chance, "break": brk, "salt": i })
        }).collect();
        let mut tp = project();
        tp.macros = Some(serde_json::from_value(json!({
            "templates": { "tpl": { "legend": { "S": "!walls" }, "rules": template_rules } }
        })).unwrap());
        let mut reference = json!({ "use": "tpl", "slug": "ref", "tileset": "alt",
                                    "origin": [origin.0, origin.1] });
        if remap { reference["legend"] = json!({ "#": "fortified" }); }
        if skip_first && n > 1 { reference["skip"] = json!(["t0"]); }
        if override_last { reference["tiles"] = json!({ format!("t{}", n - 1): [[5, 2], [6, 2]] }); }
        tp.rules[0].groups = vec![serde_json::from_value::<RuleGroup>(
            json!({ "slug": "g", "rules": [reference] })).unwrap()];

        let inline: Vec<Rule> = (0..n).filter(|&i| !(skip_first && n > 1 && i == 0)).map(|i| {
            let (out, chance, brk) = out_json(alts[i], chance, brk[i]);
            let mut tiles: Vec<[u32; 2]> = out["tiles"].as_array().unwrap().iter()
                .map(|t| [t[0].as_u64().unwrap() as u32, t[1].as_u64().unwrap() as u32])
                .collect();
            if override_last && i == n - 1 { tiles = vec![[5, 2], [6, 2]]; }
            let tiles: Vec<Value> = tiles.iter()
                .map(|t| json!([t[0] + origin.0, t[1] + origin.1])).collect();
            let mut legend = json!({ "S": "!walls" });
            if remap { legend["#"] = json!("fortified"); }
            rule_json(json!({
                "slug": format!("ref__t{i}"), "match": { "rows": pats[i], "legend": legend },
                "symmetry": sym, "out": { "tileset": "alt", "tiles": tiles },
                "chance": chance, "break": brk, "salt": i
            }))
        }).collect();
        let ip = with_rules(inline);
        let l = lvl(&tp, &rows);
        same_bake(&bake(&tp, &l), &bake(&ip, &l))?;
    }

    /// `require` on the same grid is the conjunction: a rule with `match` A
    /// and `require` B bakes like one pattern holding both (B only where A
    /// is `*`), under every symmetry.
    #[test]
    fn require_is_the_conjunction_of_two_patterns(
        rows in grid(),
        a in rows3(GLYPHS),
        b in rows3(GLYPHS),
        sym in symmetry(),
        alts in 1..3usize,
        chance in select(vec![1.0, 0.6]),
        brk in any::<bool>(),
        oob in oob(),
    ) {
        let b: Vec<String> = a.iter().zip(&b).map(|(ra, rb)| {
            ra.chars().zip(rb.chars()).map(|(ca, cb)| if ca == '*' { cb } else { '*' }).collect()
        }).collect();
        let merged: Vec<String> = a.iter().zip(&b).map(|(ra, rb)| {
            ra.chars().zip(rb.chars()).map(|(ca, cb)| if ca == '*' { cb } else { ca }).collect()
        }).collect();
        let (out, chance, brk) = out_json(alts, chance, brk);
        let mut split = json!({ "slug": "r", "match": { "rows": a }, "require": { "rows": b },
                                "symmetry": sym, "out": out, "chance": chance, "break": brk });
        let mut one = json!({ "slug": "r", "match": { "rows": merged },
                              "symmetry": sym, "out": out, "chance": chance, "break": brk });
        if let Some(o) = oob {
            split["out_of_bounds"] = json!(o);
            one["out_of_bounds"] = json!(o);
        }
        // A second rule after it, so the break rule's locks are compared too.
        let after = rule("after", &["+"], vec![worlds_core::TileAlt::single(2, 2)]);
        let ps = with_rules(vec![rule_json(split), after.clone()]);
        let po = with_rules(vec![rule_json(one), after]);
        let l = lvl(&ps, &rows);
        same_bake(&bake(&ps, &l), &bake(&po, &l))?;
    }

    /// A pattern anchored with `@` (any size, even included) bakes like the
    /// same cells in an odd, `*`-padded pattern centered on the anchor.
    #[test]
    fn at_anchor_equals_a_padded_centered_pattern(
        rows in grid(),
        (w, h) in (1..5usize, 1..5usize),
        cells in prop::collection::vec(select(GLYPHS.to_vec()), 16),
        anchor in (0..4usize, 0..4usize),
        at_test in select(vec![None, Some("solid"), Some("!water"), Some("."), Some("[fortified|.]")]),
        sym in symmetry(),
        chance in select(vec![1.0, 0.7]),
    ) {
        let (ax, ay) = (anchor.0 % w, anchor.1 % h);
        let glyph = |x: usize, y: usize| cells[y * 4 + x];
        let at_rows: Vec<String> = (0..h).map(|y| (0..w).map(|x| {
            if (x, y) == (ax, ay) { '@' } else { glyph(x, y) }
        }).collect()).collect();
        let (rx, ry) = (ax.max(w - 1 - ax), ay.max(h - 1 - ay));
        let mut padded = vec![vec!['*'; 2 * rx + 1]; 2 * ry + 1];
        for y in 0..h {
            for x in 0..w {
                let c = if (x, y) == (ax, ay) {
                    if at_test.is_some() { 'A' } else { '*' }
                } else {
                    glyph(x, y)
                };
                padded[y + ry - ay][x + rx - ax] = c;
            }
        }
        let padded: Vec<String> = padded.into_iter().map(|r| r.into_iter().collect()).collect();
        let (out, chance, _) = out_json(2, chance, true);
        let mut at = json!({ "slug": "r", "match": { "rows": at_rows }, "symmetry": sym,
                             "out": out, "chance": chance, "break": true });
        let mut pad = json!({ "slug": "r", "match": { "rows": padded }, "symmetry": sym,
                              "out": out, "chance": chance, "break": true });
        if let Some(t) = at_test {
            at["match"]["legend"] = json!({ "@": t });
            pad["match"]["legend"] = json!({ "A": t });
        }
        let pa = with_rules(vec![rule_json(at)]);
        let pp = with_rules(vec![rule_json(pad)]);
        let l = lvl(&pa, &rows);
        same_bake(&bake(&pa, &l), &bake(&pp, &l))?;
    }

    /// `[solid|fortified]` is the `walls` group, and `![...]` its negation.
    #[test]
    fn one_of_equals_a_group_with_those_values(
        rows in grid(),
        pat in rows3(&['*', '*', 'w', 'w', 'n', '.', '#']),
        sym in symmetry(),
    ) {
        let make = |w: &str, n: &str| {
            let r = json!({ "slug": "r", "match": { "rows": pat, "legend": { "w": w, "n": n } },
                            "symmetry": sym, "out": { "tileset": "main", "tiles": [[1, 1]] } });
            with_rules(vec![rule_json(r)])
        };
        let one = make("[solid|fortified]", "![fortified|solid]");
        let grp = make("walls", "!walls");
        // One level for both: the hash keys on the level's (random) id.
        let l = lvl(&one, &rows);
        same_bake(&bake(&one, &l), &bake(&grp, &l))?;
        // And with empty as a member: `[.|water]` is "not walls" here.
        let one = make("[.|water]", "![water|.]");
        let grp = make("!walls", "walls");
        same_bake(&bake(&one, &l), &bake(&grp, &l))?;
    }
}

/// The turn helper agrees with the design table (variant index order).
#[test]
fn offset_maps_match_the_design_table() {
    let d = (2, 1);
    let want: BTreeMap<u8, (i32, i32)> = [
        (0, (2, 1)),
        (1, (-2, 1)),
        (2, (2, -1)),
        (3, (-2, -1)),
        (5, (-1, 2)),
        (6, (1, -2)),
        (4, (1, 2)),
        (7, (-1, -2)),
    ]
    .into_iter()
    .collect();
    for (bits, w) in want {
        assert_eq!(turn(bits, d), w, "bits {bits}");
    }
}
