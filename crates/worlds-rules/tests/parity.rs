//! LDtk structural parity: import each LDtk fixture, bake every auto layer of
//! every level, and compare against the `autoLayerTiles` LDtk saved.
//!
//! The gate is *structural* parity, not byte parity. Worlds uses its own
//! hash, so random rolls (which alternative, random offsets, chance and
//! perlin outcomes) are never compared. Per rule category:
//!
//! - **deterministic** (chance 1, no perlin, one alternative, no random
//!   offset): the `(anchor, rule)` pairs match exactly, and so does the
//!   multiset of `(px, tileset, tile, flip)` placed there;
//! - **alternatives** (several alternatives, or a random offset): the
//!   `(anchor, rule)` pairs and the flips fired there match, and every
//!   placement group (one per flip) is exactly one of the rule's
//!   alternatives, at a position consistent with the anchor;
//! - **random** (chance < 1 or perlin): no per-cell comparison, but every
//!   anchor LDtk fired on must match the rule's pattern in our engine.
//!
//! On both sides, for every rule: the flips fired at an anchor are the
//! pattern variants that match there (all of them, or the first one for a
//! `break` rule), and each flip's tiles form one valid alternative.
//!
//! The break rule couples rules at a cell. Once the two sides' lock state at
//! an anchor diverges because of a random roll (a random rule fired on one
//! side only, or an alternative with mixed opacity), later rules at that
//! anchor are not compared ("diverged" in the summary).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use worlds_core::ldtk::{self, RefTile};
use worlds_core::{Flip, Level, Project, Rule, RuleOutput, Slug, TileAlt, TileMode};
use worlds_rules::{LayerBake, Placement, TilesetImages};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

/// Every sample. `Test_file_for_API_showing_all_features.ldtk` mixes 8 px
/// and 16 px layers on an 8 px project grid, so its auto layers bake on
/// their own 16 px grid.
const SAMPLES: &[&str] = &[
    "Test_file_for_API_showing_all_features.ldtk",
    "AutoLayers_1_basic.ldtk",
    "AutoLayers_2_stamps.ldtk",
    "AutoLayers_3_Mosaic.ldtk",
    "AutoLayers_4_Assistant.ldtk",
    "AutoLayers_5_Advanced.ldtk",
    "AutoLayers_6_OptionalRules.ldtk",
    "AutoLayers_7_Biomes.ldtk",
    "Entities.ldtk",
];

/// Known differences where LDtk's *saved* tiles are wrong, not our bake.
/// `rule` "*" covers the whole layer. Every entry must still be hit, so a
/// fixed fixture can't leave a stale skip behind.
struct Skip {
    fixture: &'static str,
    level: &'static str,
    layer: &'static str,
    rule: &'static str,
    reason: &'static str,
}

const SKIPS: &[Skip] = &[];

// ---- one placed tile, from either side ----

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct T {
    px: [i32; 2],
    tileset: String,
    tile: [u32; 2],
    flip: u8,
}

fn flip_bits(f: Option<Flip>) -> u8 {
    Flip::bits_of(f)
}

fn flip_name(f: u8) -> &'static str {
    ["-", "x", "y", "xy"][f as usize & 3]
}

impl From<&RefTile> for T {
    fn from(t: &RefTile) -> Self {
        T {
            px: t.px,
            tileset: t.tileset.to_string(),
            tile: t.tile,
            flip: flip_bits(t.flip),
        }
    }
}

impl From<&Placement> for T {
    fn from(p: &Placement) -> Self {
        T {
            px: p.px,
            tileset: p.tileset.to_string(),
            tile: p.tile,
            flip: flip_bits(p.flip),
        }
    }
}

impl std::fmt::Display for T {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}@{},{}[{},{}]{}",
            self.tileset,
            self.px[0],
            self.px[1],
            self.tile[0],
            self.tile[1],
            if self.flip == 0 {
                String::new()
            } else {
                format!("/{}", flip_name(self.flip))
            }
        )
    }
}

// ---- rules as the harness sees them ----

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Cat {
    Deterministic,
    Alternatives,
    Random,
}

struct RuleInfo<'p> {
    rule: &'p Rule,
    cat: Cat,
    alts: Vec<TileAlt>,
    stamp: bool,
    pivot: [f64; 2],
    /// The tileset its tiles must come from in this level.
    tileset: String,
    /// That tileset's tile size in pixels.
    tile_size: [i32; 2],
    /// `[[x_min, x_max], [y_min, y_max]]`.
    random: [[i32; 2]; 2],
    /// No offsets and alpha >= 1: an opaque tile locks the anchor.
    opaque_locks: bool,
    /// Some alternative is empty: a variant can fire without tiles.
    has_empty_alt: bool,
}

/// The rules of `layer` in rule order (groups in file order, then rules),
/// keyed by id. Rules of groups that don't run here are included; they just
/// never fire.
fn rule_infos<'p>(project: &'p Project, level: &Level, layer: &str) -> Vec<RuleInfo<'p>> {
    let Some(set) = project.rules_for(layer) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for g in &set.groups {
        for r in g.inline_rules() {
            let (alts, stamp, pivot, ts) = match r.out().unwrap() {
                RuleOutput::Stamp(o) => {
                    let st = set.stamps.iter().find(|s| s.slug == o.stamp).unwrap();
                    let [c, row, w, h] = st.rect;
                    (
                        vec![TileAlt::rect(c, row, w, h)],
                        true,
                        st.pivot,
                        &st.tileset,
                    )
                }
                RuleOutput::Tiles(o) => (
                    o.tiles.clone(),
                    o.tile_mode == TileMode::Stamp,
                    o.pivot,
                    &o.tileset,
                ),
            };
            let o = r.out().unwrap().draw();
            let alts: Vec<TileAlt> = alts.into_iter().filter(|a| a.weight > 0).collect();
            let tileset = level.tileset_for(layer, ts);
            let tile_size = project
                .tileset(tileset.as_str())
                .map_or([0, 0], |t| [t.tile_size[0] as i32, t.tile_size[1] as i32]);
            let tileset = tileset.to_string();
            let random = o.random_offset_px.as_ref().map_or([[0, 0]; 2], |ro| {
                let s = |[a, b]: [i32; 2]| [a.min(b), a.max(b)];
                [s(ro.x), s(ro.y)]
            });
            let has_random_offset = random != [[0, 0]; 2];
            let m = r.as_match().unwrap();
            let gated = m.chance < 1.0 || m.perlin.as_ref().is_some_and(|p| p.active);
            let cat = if gated {
                Cat::Random
            } else if alts.len() > 1 || has_random_offset {
                Cat::Alternatives
            } else {
                Cat::Deterministic
            };
            out.push(RuleInfo {
                rule: r,
                cat,
                has_empty_alt: alts.iter().any(|a| a.tiles.is_empty()),
                alts,
                stamp,
                pivot,
                tileset,
                tile_size,
                random,
                opaque_locks: o.offset_px == [0, 0] && !has_random_offset && o.alpha >= 1.0,
            });
        }
    }
    out
}

/// The stamp offset of `tile` within `alt`, in pixels, unflipped
/// (`docs/ldtk_parity.md` §7.2), computed independently of the engine:
/// LDtk's formula, which scales the layout by the layer grid `g`. Worlds
/// scales by the tile size instead; the fixtures all have tiles the size of
/// the grid, where the two agree (asserted by the caller).
///
/// LDtk truncates toward zero; Worlds floors, so a stamp's tiles stay
/// exactly one cell apart. The two only differ for a fractional negative
/// offset (an odd grid size with a half-cell pivot), which no fixture has.
fn stamp_offset(
    alt: &TileAlt,
    tile: [u32; 2],
    pivot: [f64; 2],
    tile_pivot: [f64; 2],
    g: i32,
) -> [i32; 2] {
    let [l, t, r, b] = alt.bounds().expect("non-empty");
    let fx = (tile[0] as f64 - l as f64) - pivot[0] * (r - l) as f64 + tile_pivot[0];
    let fy = (tile[1] as f64 - t as f64) - pivot[1] * (b - t) as f64 + tile_pivot[1];
    [
        (fx * g as f64).floor() as i32,
        (fy * g as f64).floor() as i32,
    ]
}

/// Whether `tiles` (all with flip `flip`, anchored at `anchor`) are exactly
/// one alternative of `ri`, drawn where that alternative goes. Returns why not.
fn check_alternative(
    ri: &RuleInfo,
    tiles: &[T],
    flip: u8,
    anchor: [i32; 2],
    g: i32,
    tile_pivot: [f64; 2],
) -> Result<(), String> {
    let mut got: Vec<[u32; 2]> = tiles.iter().map(|t| t.tile).collect();
    got.sort_unstable();
    if let Some(t) = tiles.iter().find(|t| t.tileset != ri.tileset) {
        return Err(format!("tile {t} is not from tileset {}", ri.tileset));
    }
    let flipped = [flip & 1 != 0, flip & 2 != 0];
    let [ox, oy] = ri.rule.out().unwrap().draw().offset_px;
    if ri.stamp && ri.tile_size != [g, g] {
        return Err(format!(
            "stamp with {:?} px tiles on a {g} px grid: LDtk's layout differs from ours",
            ri.tile_size
        ));
    }
    let mut why = String::new();
    for alt in &ri.alts {
        let mut want = alt.tiles.clone();
        want.sort_unstable();
        if want != got {
            continue;
        }
        // Each tile's residual is the random offset; it must be in range and
        // shared by every tile of the group.
        let mut residual: Option<[i32; 2]> = None;
        let mut ok = true;
        for t in tiles {
            let s = if ri.stamp {
                stamp_offset(alt, t.tile, ri.pivot, tile_pivot, g)
            } else {
                [0, 0]
            };
            // Offset from the anchor cell's corner, unmirrored: a mirrored
            // tile spanning [o, o + size) sits at [g - o - size, g - o).
            let rel = |a: usize| {
                let r = t.px[a] - anchor[a] * g;
                if flipped[a] {
                    g - ri.tile_size[a] - r
                } else {
                    r
                }
            };
            let rnd = [rel(0) - s[0] - ox, rel(1) - s[1] - oy];
            let in_range = (0..2).all(|a| rnd[a] >= ri.random[a][0] && rnd[a] <= ri.random[a][1]);
            if !in_range || residual.is_some_and(|r| r != rnd) {
                ok = false;
                let _ = write!(why, "{t} off by {rnd:?}; ");
                break;
            }
            residual = Some(rnd);
        }
        if ok {
            return Ok(());
        }
    }
    if why.is_empty() {
        let shown: Vec<String> = tiles.iter().map(T::to_string).collect();
        Err(format!(
            "[{}] is none of the rule's alternatives",
            shown.join(" ")
        ))
    } else {
        Err(format!("misplaced: {why}"))
    }
}

// ---- report ----

#[derive(Default)]
struct Stats {
    layers: usize,
    ref_tiles: usize,
    our_tiles: usize,
    rules: BTreeMap<Cat, usize>,
    /// (anchor, rule) pairs compared per category.
    pairs: BTreeMap<Cat, usize>,
    /// (anchor, rule) pairs not compared because the anchor's lock state
    /// diverged after a random roll.
    diverged: usize,
    random_anchors_checked: usize,
}

#[derive(Default)]
struct Report {
    /// (fixture, level, layer, rule) -> examples.
    mismatches: BTreeMap<(String, String, String, String), Vec<String>>,
    skipped: BTreeMap<(String, String, String, String), usize>,
    stats: BTreeMap<String, Stats>,
    warnings: Vec<String>,
}

impl Report {
    fn mismatch(&mut self, fx: &str, level: &str, layer: &str, rule: &str, msg: String) {
        let skip = SKIPS.iter().find(|s| {
            s.fixture == fx
                && s.level == level
                && s.layer == layer
                && (s.rule == "*" || s.rule == rule)
        });
        let key = (
            fx.to_string(),
            level.to_string(),
            layer.to_string(),
            rule.to_string(),
        );
        if skip.is_some() {
            *self.skipped.entry(key).or_default() += 1;
        } else {
            self.mismatches.entry(key).or_default().push(msg);
        }
    }

    fn table(&self) -> String {
        let mut s = String::new();
        let _ = writeln!(
            s,
            "{:<34} {:>6} {:>6} {:>6} {:>13} {:>13} {:>6} {:>8} {:>7}",
            "fixture",
            "layers",
            "ref",
            "ours",
            "rules d/a/r",
            "pairs d/a",
            "random",
            "diverged",
            "issues"
        );
        for (fx, st) in &self.stats {
            let issues: usize = self
                .mismatches
                .iter()
                .filter(|(k, _)| k.0 == *fx)
                .map(|(_, v)| v.len())
                .sum();
            let r = |c| st.rules.get(&c).copied().unwrap_or(0);
            let p = |c| st.pairs.get(&c).copied().unwrap_or(0);
            let _ = writeln!(
                s,
                "{:<34} {:>6} {:>6} {:>6} {:>13} {:>13} {:>6} {:>8} {:>7}",
                fx,
                st.layers,
                st.ref_tiles,
                st.our_tiles,
                format!(
                    "{}/{}/{}",
                    r(Cat::Deterministic),
                    r(Cat::Alternatives),
                    r(Cat::Random)
                ),
                format!("{}/{}", p(Cat::Deterministic), p(Cat::Alternatives)),
                st.random_anchors_checked,
                st.diverged,
                issues
            );
        }
        for ((fx, lv, ly, rule), n) in &self.skipped {
            let reason = SKIPS
                .iter()
                .find(|k| {
                    k.fixture == fx
                        && k.level == lv
                        && k.layer == ly
                        && (k.rule == "*" || k.rule == rule)
                })
                .map_or("", |k| k.reason);
            let _ = writeln!(s, "skipped {n} in {fx} / {lv} / {ly} / {rule}: {reason}");
        }
        if !self.mismatches.is_empty() {
            let _ = writeln!(
                s,
                "\nMISMATCHES (fixture / level / layer / rule: count, up to 10 examples)"
            );
            for ((fx, lv, ly, rule), msgs) in &self.mismatches {
                let _ = writeln!(s, "{fx} / {lv} / {ly} / {rule}: {}", msgs.len());
                for m in msgs.iter().take(10) {
                    let _ = writeln!(s, "    {m}");
                }
            }
        }
        s
    }
}

// ---- the comparison ----

struct Side<'a> {
    name: &'static str,
    /// anchor -> rule index -> tiles.
    by_anchor: BTreeMap<[i32; 2], BTreeMap<usize, Vec<T>>>,
    opacity: &'a dyn Fn(&str, [u32; 2]) -> bool,
}

impl Side<'_> {
    /// Does rule `ri` with `tiles` at an anchor lock it?
    fn locks(&self, ri: &RuleInfo, tiles: &[T]) -> bool {
        ri.rule.break_on_match
            || (ri.opaque_locks && tiles.iter().any(|t| (self.opacity)(&t.tileset, t.tile)))
    }
}

#[allow(clippy::too_many_arguments)]
fn compare_layer(
    report: &mut Report,
    fx: &str,
    project: &Project,
    level: &Level,
    layer: &Slug,
    bake: &LayerBake,
    reference: &[RefTile],
    our_opacity: &dyn Fn(&str, [u32; 2]) -> bool,
    ref_opacity: &dyn Fn(&str, [u32; 2]) -> bool,
) {
    let lv = level.slug.as_str();
    let ly = layer.as_str();
    let def = project.layer(ly).unwrap();
    let g = project.layer_grid(def) as i32;
    let tile_pivot = def.auto().unwrap().tile_pivot;
    let infos = rule_infos(project, level, ly);
    let index: BTreeMap<&str, usize> = infos
        .iter()
        .enumerate()
        .map(|(i, r)| (r.rule.slug.as_str(), i))
        .collect();
    {
        let st = report.stats.entry(fx.to_string()).or_default();
        st.layers += 1;
        st.ref_tiles += reference.len();
        st.our_tiles += bake.placements.len();
    }

    // `fired` must be exactly what the placements say.
    let mut derived: BTreeMap<[i32; 2], Vec<Slug>> = BTreeMap::new();
    for p in &bake.placements {
        let e = derived.entry(p.anchor).or_default();
        if !e.contains(&p.rule) {
            e.push(p.rule.clone());
        }
    }
    for v in derived.values_mut() {
        v.sort_by_key(|r| index.get(r.as_str()).copied());
    }
    if derived != bake.fired {
        report.mismatch(
            fx,
            lv,
            ly,
            "*",
            "`fired` disagrees with `placements`".into(),
        );
    }

    let mut ours = Side {
        name: "ours",
        by_anchor: BTreeMap::new(),
        opacity: our_opacity,
    };
    for p in &bake.placements {
        let i = index[p.rule.as_str()];
        ours.by_anchor
            .entry(p.anchor)
            .or_default()
            .entry(i)
            .or_default()
            .push(p.into());
    }
    let mut ldtk = Side {
        name: "LDtk",
        by_anchor: BTreeMap::new(),
        opacity: ref_opacity,
    };
    for t in reference {
        match index.get(t.rule.as_str()) {
            Some(&i) => ldtk
                .by_anchor
                .entry(t.anchor_cell)
                .or_default()
                .entry(i)
                .or_default()
                .push(t.into()),
            None => report.mismatch(
                fx,
                lv,
                ly,
                t.rule.as_str(),
                format!(
                    "{:?}: LDtk tile from a rule that wasn't imported",
                    t.anchor_cell
                ),
            ),
        }
    }

    // Where each rule's pattern matches, for the flip checks.
    let mut matches: BTreeMap<usize, BTreeMap<[i32; 2], Vec<Option<Flip>>>> = BTreeMap::new();
    let mut used: BTreeSet<usize> = BTreeSet::new();
    for side in [&ours, &ldtk] {
        for rules in side.by_anchor.values() {
            used.extend(rules.keys().copied());
        }
    }
    for &i in &used {
        // LDtk layers have one pass and no region rules: matching never
        // bakes, so no images are needed.
        let m = worlds_rules::rule_matches(
            project,
            level,
            layer,
            infos[i].rule.slug.as_str(),
            &TilesetImages::in_memory(),
        )
        .unwrap_or_else(|e| panic!("{fx} {lv} {ly}: {e}"));
        matches.insert(i, m);
    }

    // Per side, per (anchor, rule): flips fired are the matching variants,
    // and each flip's tiles are one alternative.
    for side in [&ours, &ldtk] {
        for (&anchor, rules) in &side.by_anchor {
            for (&i, tiles) in rules {
                let ri = &infos[i];
                let id = ri.rule.slug.as_str();
                let mut by_flip: BTreeMap<u8, Vec<T>> = BTreeMap::new();
                for t in tiles {
                    by_flip.entry(t.flip).or_default().push(t.clone());
                }
                let matching: Vec<u8> = matches[&i]
                    .get(&anchor)
                    .map(|v| v.iter().map(|f| flip_bits(*f)).collect())
                    .unwrap_or_default();
                if ri.cat == Cat::Random && side.name == "LDtk" {
                    report
                        .stats
                        .entry(fx.to_string())
                        .or_default()
                        .random_anchors_checked += 1;
                }
                if matching.is_empty() {
                    report.mismatch(
                        fx,
                        lv,
                        ly,
                        id,
                        format!(
                            "{anchor:?}: {} fired but the pattern doesn't match here",
                            side.name
                        ),
                    );
                    continue;
                }
                let fired: Vec<u8> = by_flip.keys().copied().collect();
                let expected: Vec<u8> = if ri.rule.break_on_match {
                    vec![matching[0]]
                } else {
                    matching.clone()
                };
                let flips_ok = if ri.has_empty_alt {
                    fired.iter().all(|f| expected.contains(f))
                } else {
                    fired == expected
                };
                if !flips_ok {
                    report.mismatch(
                        fx,
                        lv,
                        ly,
                        id,
                        format!(
                            "{anchor:?}: {} fired flips {:?}, matching variants give {:?}",
                            side.name,
                            fired.iter().map(|f| flip_name(*f)).collect::<Vec<_>>(),
                            expected.iter().map(|f| flip_name(*f)).collect::<Vec<_>>(),
                        ),
                    );
                }
                for (&f, group) in &by_flip {
                    if let Err(why) = check_alternative(ri, group, f, anchor, g, tile_pivot) {
                        report.mismatch(fx, lv, ly, id, format!("{anchor:?} {}: {why}", side.name));
                    }
                }
            }
        }
    }

    // Walk each anchor's rules in order, following both sides' locks.
    let anchors: BTreeSet<[i32; 2]> = ours
        .by_anchor
        .keys()
        .chain(ldtk.by_anchor.keys())
        .copied()
        .collect();
    let empty = BTreeMap::new();
    for anchor in anchors {
        let o = ours.by_anchor.get(&anchor).unwrap_or(&empty);
        let l = ldtk.by_anchor.get(&anchor).unwrap_or(&empty);
        let rules: BTreeSet<usize> = o.keys().chain(l.keys()).copied().collect();
        let (mut lock_o, mut lock_l, mut diverged) = (false, false, false);
        for i in rules {
            let ri = &infos[i];
            let id = ri.rule.slug.as_str();
            let (ot, lt) = (o.get(&i), l.get(&i));
            if diverged {
                report.stats.entry(fx.to_string()).or_default().diverged += 1;
                continue;
            }
            if lock_o && ot.is_some() {
                report.mismatch(
                    fx,
                    lv,
                    ly,
                    id,
                    format!("{anchor:?}: ours placed tiles after the anchor locked"),
                );
            }
            if lock_l && lt.is_some() {
                report.mismatch(
                    fx,
                    lv,
                    ly,
                    id,
                    format!("{anchor:?}: LDtk placed tiles after the anchor locked"),
                );
            }
            if lock_o || lock_l {
                continue;
            }
            match ri.cat {
                Cat::Random => {}
                cat => {
                    *report
                        .stats
                        .entry(fx.to_string())
                        .or_default()
                        .pairs
                        .entry(cat)
                        .or_default() += 1;
                    match (ot, lt) {
                        (Some(_), None) => report.mismatch(
                            fx,
                            lv,
                            ly,
                            id,
                            format!("{anchor:?}: ours fired, LDtk didn't"),
                        ),
                        (None, Some(_)) => report.mismatch(
                            fx,
                            lv,
                            ly,
                            id,
                            format!("{anchor:?}: LDtk fired, ours didn't"),
                        ),
                        (Some(a), Some(b)) if cat == Cat::Deterministic => {
                            let (mut a, mut b) = (a.clone(), b.clone());
                            a.sort();
                            b.sort();
                            if a != b {
                                let s = |v: &[T]| {
                                    v.iter().map(T::to_string).collect::<Vec<_>>().join(" ")
                                };
                                report.mismatch(
                                    fx,
                                    lv,
                                    ly,
                                    id,
                                    format!("{anchor:?}: ours [{}] vs LDtk [{}]", s(&a), s(&b)),
                                );
                            }
                        }
                        (Some(a), Some(b)) => {
                            let fa: BTreeSet<u8> = a.iter().map(|t| t.flip).collect();
                            let fb: BTreeSet<u8> = b.iter().map(|t| t.flip).collect();
                            if fa != fb {
                                report.mismatch(
                                    fx,
                                    lv,
                                    ly,
                                    id,
                                    format!("{anchor:?}: flips ours {fa:?} vs LDtk {fb:?}"),
                                );
                            }
                        }
                        (None, None) => unreachable!(),
                    }
                }
            }
            let no = ot.is_some_and(|t| ours.locks(ri, t));
            let nl = lt.is_some_and(|t| ldtk.locks(ri, t));
            if no != nl {
                diverged = true;
                // A random roll explains it; a deterministic rule doesn't (its
                // fire mismatch is reported above, or the opacity tables differ).
                let rolled = ri.cat == Cat::Random
                    || (ri.cat == Cat::Alternatives && !ri.rule.break_on_match);
                if !rolled && ot.is_some() == lt.is_some() {
                    report.mismatch(fx, lv, ly, id, format!(
                        "{anchor:?}: lock differs (ours {no}, LDtk {nl}): opacity tables disagree"
                    ));
                }
            }
            lock_o |= no;
            lock_l |= nl;
        }
    }
}

// ---- fixtures ----

/// Compare PNG opacity with LDtk's cached table; differences are warnings.
fn cross_check_opacity(
    report: &mut Report,
    fx: &str,
    imported: &ldtk::Imported,
    images: &TilesetImages,
) {
    for (ts, cached) in &imported.reference.opaque_tiles {
        let png = match images.opacity(ts.as_str()) {
            Ok(o) => o,
            Err(e) => {
                report.warnings.push(format!("{fx}: {ts}: {e}"));
                continue;
            }
        };
        let [cols, rows] = png.grid();
        let mut diff = Vec::new();
        for row in 0..rows {
            for col in 0..cols {
                if png.is_opaque(col, row) != cached.contains(&[col, row]) {
                    diff.push([col, row]);
                }
            }
        }
        diff.extend(cached.iter().filter(|t| t[0] >= cols || t[1] >= rows));
        if !diff.is_empty() {
            report.warnings.push(format!(
                "{fx}: tileset {ts}: PNG opacity differs from LDtk's cached opaqueTiles on {} tiles, e.g. {:?}",
                diff.len(),
                &diff[..diff.len().min(10)]
            ));
        }
    }
}

fn run_fixture(
    report: &mut Report,
    fx: &str,
    path: &Path,
    out_dir: &Path,
    images: Option<TilesetImages>,
) {
    let imported = ldtk::import(path, out_dir).unwrap_or_else(|e| panic!("{fx}: {e}"));
    let project = &imported.project;
    let images = match images {
        Some(i) => i,
        None => TilesetImages::new(out_dir, &project.tilesets),
    };
    let cached: BTreeMap<String, BTreeSet<[u32; 2]>> = imported
        .reference
        .opaque_tiles
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect();
    let our_opacity = |ts: &str, t: [u32; 2]| {
        images
            .is_opaque(ts, t[0], t[1])
            .unwrap_or_else(|e| panic!("{fx}: {e}"))
    };
    // LDtk locked with its in-memory table, which is the cached one when present.
    let ref_opacity = |ts: &str, t: [u32; 2]| match cached.get(ts) {
        Some(set) => set.contains(&t),
        None => our_opacity(ts, t),
    };
    {
        // A rule's category doesn't depend on the level, so any level will do.
        let st = report.stats.entry(fx.to_string()).or_default();
        if let Some(level) = project.levels().next() {
            for l in project.layers.iter().filter(|l| l.is_auto()) {
                for ri in rule_infos(project, level, l.slug.as_str()) {
                    *st.rules.entry(ri.cat).or_default() += 1;
                }
            }
        }
    }
    let empty = Vec::new();
    for level in project.levels() {
        let bakes = worlds_rules::bake_level(project, level, &images)
            .unwrap_or_else(|e| panic!("{fx} {}: {e}", level.slug));
        for (layer, bake) in &bakes {
            let reference = imported
                .reference
                .autotiles
                .get(&level.slug)
                .and_then(|m| m.get(layer))
                .unwrap_or(&empty);
            compare_layer(
                report,
                fx,
                project,
                level,
                layer,
                bake,
                reference,
                &our_opacity,
                &ref_opacity,
            );
        }
    }
}

#[test]
fn ldtk_structural_parity() {
    let mut report = Report::default();
    let samples = fixtures().join("ldtk-samples");
    for name in SAMPLES {
        let path = samples.join(name);
        let imported = ldtk::import(&path, &samples).unwrap();
        let images = TilesetImages::new(&samples, &imported.project.tilesets);
        cross_check_opacity(&mut report, name, &imported, &images);
        run_fixture(&mut report, name, &path, &samples, Some(images));
    }

    let table = report.table();
    println!("{table}");
    for w in &report.warnings {
        println!("warning: {w}");
    }
    let unused: Vec<String> = SKIPS
        .iter()
        .filter(|s| {
            !report.skipped.keys().any(|k| {
                k.0 == s.fixture
                    && k.1 == s.level
                    && k.2 == s.layer
                    && (s.rule == "*" || k.3 == s.rule)
            })
        })
        .map(|s| format!("{} / {} / {} / {}", s.fixture, s.level, s.layer, s.rule))
        .collect();
    assert!(
        unused.is_empty(),
        "skip entries no longer needed: {unused:?}"
    );
    assert!(
        report.mismatches.is_empty(),
        "LDtk parity mismatches:\n{table}"
    );
}
