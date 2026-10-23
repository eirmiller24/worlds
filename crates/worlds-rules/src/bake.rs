//! The bake: compile a layer's rules once, then sweep cells, pass by pass.
//!
//! Pipeline for one `auto` layer in one level (the crate README has the
//! long form):
//!
//! 1. **Compile** ([`Baker::new`]). Expand the rule set
//!    ([`Project::expanded_rules`]), drop groups that don't apply in this
//!    level and inactive rules, and sort the groups by `pass` (stable: file
//!    order within a pass). Sample every int_grid a rule reads into a dense
//!    `Vec<u16>` of value indices (a derived grid by its effective value:
//!    the writer's result from `upstream` with the overrides on top, or, for
//!    a grid this layer writes itself, the overrides alone; earlier passes'
//!    writes are laid in per pass). Turn each pattern into a flat list of
//!    `(dx, dy, test)` cells with one lookup table per distinct test, work
//!    out the symmetry variants once, resolve the tileset, alternatives and
//!    stamp offsets; compile a region rule's selector and placement, and a
//!    `write` output's grid and value.
//! 2. **Sweep** ([`Baker::sweep`]), per pass: each rule of the pass sweeps
//!    the anchor cells row-major (a region rule only its candidates),
//!    skipping anchors an earlier rule of the pass locked. Hits append raw
//!    placements (or a write firing) to the rule's bucket; occupancy is
//!    checked and marked inline.
//! 3. **Finish** ([`Baker::finish`]). Concatenate buckets in draw order
//!    (passes bottom to top; within a pass, last rule first), attach slugs,
//!    build `fired`, the computed derived grids and the occupancy.
//!
//! **The break rule**, within a pass: at each anchor cell, walking the
//! pass's rules in order, a rule that fired there locks the cell if it has
//! `break`, or if it has no offsets, `alpha >= 1` and at least one of its
//! tiles there is opaque. Later rules of the same pass skip a locked anchor.
//! A pass starts with no locks. Because the lock decision at a cell only
//! depends on the earlier rules at that same cell, doing it inline (rules in
//! order, each one skipping already-locked anchors) is the same as LDtk's
//! post-pass.
//!
//! Incremental bakes ([`crate::BakeCache`]) plug in at `sweep`: it only
//! touches anchors in the given rect, so a dirty rect grown by the largest
//! pattern radius is re-swept into fresh buckets and spliced over the cached
//! placements of those anchors.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::Arc;

use worlds_core::{
    CellMatch, CellTest, Checker, Flip, GroupMode, IntGridLayer, LayerDef, Level, MatchSelect,
    OutOfBounds, Override, PlaceAt, Project, RequirementMode, Rule, RuleGroup, RuleOutput, Slug,
    Symmetry, TileAlt, TileMode,
};

use crate::derived::{self, Effective};
use crate::hash::{self, hash, stream, unit};
use crate::images::{TileOpacity, TilesetImages};
use crate::perlin::Perlin;
use crate::region::{self, Axis, RegionCandidates, RegionPlan};
use crate::{Error, LayerBake, Placement, Result};

/// Bake one `auto` layer of `level`. A layer with no rules file bakes empty.
///
/// A layer that reads derived grids written by other auto layers bakes
/// those writers first (transitively), so the result is always the one
/// [`bake_level`] gives.
///
/// Only `auto` layers carry rules in format 1 (`rules/<layer>.json` must name
/// an `auto` layer), so any other kind is an error.
pub fn bake_layer(
    project: &Project,
    level: &Level,
    layer: &Slug,
    images: &TilesetImages,
) -> Result<LayerBake> {
    let def = auto_def(project, layer.as_str())?;
    if project.rules_for(layer.as_str()).is_none() {
        return Ok(LayerBake::default());
    }
    let upstream = derived::upstream(project, level, layer.as_str(), images)?;
    bake_with(project, level, def, images, &upstream)
}

/// Bake one layer given the effective grids of the derived grids other
/// layers write.
pub(crate) fn bake_with(
    project: &Project,
    level: &Level,
    def: &LayerDef,
    images: &TilesetImages,
    upstream: &Effective,
) -> Result<LayerBake> {
    if project.rules_for(def.slug.as_str()).is_none() {
        return Ok(LayerBake::default());
    }
    let baker = Baker::new(project, level, def, upstream)?;
    let run = baker.run(images, false)?;
    Ok(baker.finish(run.out, &run.computed))
}

/// Bake every `auto` layer of `level`, keyed by layer slug. Layers bake in
/// dependency order (writers of derived grids before their readers, ties
/// in `layers.json` order); a cycle is [`Error::DerivedCycle`].
pub fn bake_level(
    project: &Project,
    level: &Level,
    images: &TilesetImages,
) -> Result<BTreeMap<Slug, LayerBake>> {
    let (order, cyclic) = derived::order(project);
    if let Some((_, cycle)) = cyclic.into_iter().next() {
        return Err(Error::DerivedCycle(cycle));
    }
    let mut eff = Effective::new();
    let mut out = BTreeMap::new();
    for layer in order {
        let def = auto_def(project, layer.as_str())?;
        let bake = bake_with(project, level, def, images, &eff)?;
        derived::publish(project, level, &bake, &mut eff);
        out.insert(layer, bake);
    }
    Ok(out)
}

pub(crate) fn auto_def<'p>(project: &'p Project, layer: &str) -> Result<&'p LayerDef> {
    let def = project
        .layer(layer)
        .ok_or_else(|| Error::UnknownLayer(layer.to_string()))?;
    if !def.is_auto() {
        return Err(Error::NotAutoLayer(layer.to_string()));
    }
    Ok(def)
}

/// Where one rule matches: anchor cell to the flips of the pattern variants
/// that match there (`None` = as written), in variant order.
pub type Matches = BTreeMap<[i32; 2], Vec<Option<Flip>>>;

/// Every anchor cell where rule `rule` of `layer` matches in `level`, with
/// the pattern variants that match there as the flip each would draw with
/// (`None` = as written), in variant order.
///
/// `rule` is an inline rule's slug, an expanded template rule's slug
/// (`<reference>__<rule>`), or a template reference's slug, which means the
/// union of its expanded rules (flips concatenated in rule order; use
/// [`rule_matches_many`] to keep them apart).
///
/// For a pattern rule only the patterns (`match` and `require`),
/// `out_of_bounds` and the `modulo`/`checker` gate are checked: `chance`,
/// `perlin`, the break rule and occupancy are ignored, so this says where
/// the rule *could* fire. Grids are read as the rule's pass sees them (a
/// derived grid this layer writes includes the earlier passes' writes, which
/// is why `images` is needed). For a region rule the matches are its
/// candidates (`[None]` each). A rule that doesn't run in this level (its
/// group doesn't apply, it is inactive, or it has no alternatives) matches
/// nowhere. Used by the LDtk parity harness for random rules, and by the
/// editor's "where does this rule fire" highlight.
pub fn rule_matches(
    project: &Project,
    level: &Level,
    layer: &Slug,
    rule: &str,
    images: &TilesetImages,
) -> Result<Matches> {
    let mut out = Matches::new();
    for (_, m) in rule_matches_many(project, level, layer, rule, images)? {
        for (cell, flips) in m {
            out.entry(cell).or_default().extend(flips);
        }
    }
    Ok(out)
}

/// [`rule_matches`] per expanded rule: for an inline or expanded slug one
/// entry, for a template reference's slug one entry per rule it expands to
/// (in run order, skipped rules left out), keyed by expanded slug. A rule
/// that doesn't run in this level is listed with no matches. An unknown slug
/// gives an empty list.
pub fn rule_matches_many(
    project: &Project,
    level: &Level,
    layer: &Slug,
    rule: &str,
    images: &TilesetImages,
) -> Result<Vec<(Slug, Matches)>> {
    matches_where(project, level, layer, images, &|slug, from| {
        slug == rule || from.is_some_and(|f| f == rule)
    })
}

/// [`rule_matches`] of every expanded rule of `layer`, in run order (groups
/// sorted by pass), keyed by expanded slug: one compile and one sweep, where
/// calling [`rule_matches_many`] per entry compiles the whole layer each
/// time.
pub fn rule_matches_all(
    project: &Project,
    level: &Level,
    layer: &Slug,
    images: &TilesetImages,
) -> Result<Vec<(Slug, Matches)>> {
    matches_where(project, level, layer, images, &|_, _| true)
}

/// Matches of the expanded rules `keep(slug, reference)` selects.
fn matches_where(
    project: &Project,
    level: &Level,
    layer: &Slug,
    images: &TilesetImages,
    keep: &dyn Fn(&Slug, Option<&Slug>) -> bool,
) -> Result<Vec<(Slug, Matches)>> {
    let def = auto_def(project, layer.as_str())?;
    if project.rules_for(layer.as_str()).is_none() {
        return Ok(Vec::new());
    }
    let expanded = project
        .expanded_rules(layer.as_str())
        .map_err(|source| Error::Expand {
            layer: layer.to_string(),
            source,
        })?;
    let mut wanted: Vec<(u32, Slug)> = expanded
        .iter()
        .flat_map(|g| g.rules.iter().map(move |r| (g.group.pass, r)))
        .filter(|(_, r)| keep(&r.rule.slug, r.from.as_ref()))
        .map(|(p, r)| (p, r.rule.slug.clone()))
        .collect();
    if wanted.is_empty() {
        return Ok(Vec::new());
    }
    // Run order: stable by pass.
    wanted.sort_by_key(|(p, _)| *p);
    let upstream = derived::upstream(project, level, layer.as_str(), images)?;
    let baker = Baker::new(project, level, def, &upstream)?;
    // Region candidates and later passes' view of the layer's own writes
    // need the bake; plain pattern layers don't.
    let run = if baker.needs_run_for_matches() {
        Some(baker.run(images, true)?)
    } else {
        None
    };
    let mut out = Vec::new();
    for (_, slug) in wanted {
        let mut m = Matches::new();
        // Expanded slugs are unique in a valid layer; with a duplicate the
        // first rule of that slug answers, as before.
        if let Some(ri) = baker.rules.iter().position(|r| r.rule.slug == slug) {
            let r = &baker.rules[ri];
            if r.region.is_some() {
                let cands = &run.as_ref().expect("region rules run").candidates[ri];
                for c in cands {
                    m.insert(*c, vec![None]);
                }
            } else {
                let none = Overlay::new();
                let ov = run
                    .as_ref()
                    .map_or(&none, |run| &run.snapshots[baker.pass_of(ri)]);
                m = baker.pattern_matches(ri, &baker.grid_view(ov));
            }
        }
        out.push((slug, m));
    }
    Ok(out)
}

/// Whether `group` of `layer` runs in `level`:
///
/// - An `off` group never runs.
/// - A group whose `requires_level_tag` (non-empty `tags`) fails in this
///   level never runs.
/// - Otherwise, a group with a non-empty `requires_level_field`, in a level
///   that has that field (the project defines it as a level field, or the
///   level stores a value), runs iff the requirement holds, and that is
///   final: `mode` and `optional_groups` are ignored. `any`: the field holds
///   at least one of `values`; `all`: it holds every one. Scalar and array
///   fields both work; a missing or `null` value falls back to the field's
///   default.
/// - Otherwise a holding `requires_level_tag` runs the group (an `optional`
///   one too, without being listed).
/// - Otherwise an `always` group runs, and an `optional` one runs only if
///   the level lists it in `layers.<layer>.optional_groups`.
pub fn group_applies(project: &Project, level: &Level, layer: &str, group: &RuleGroup) -> bool {
    if group.mode == GroupMode::Off {
        return false;
    }
    let tag = group
        .requires_level_tag
        .as_ref()
        .filter(|t| !t.tags.is_empty());
    if let Some(t) = tag
        && !t.holds(&level.tags)
    {
        return false;
    }
    if let Some(req) = &group.requires_level_field
        && !req.values.is_empty()
        && let Some(held) = level_field_values(project, level, req.field.as_str())
    {
        let has = |v: &Slug| held.contains(&v.as_str());
        return match req.mode {
            RequirementMode::Any => req.values.iter().any(has),
            RequirementMode::All => req.values.iter().all(has),
        };
    }
    if tag.is_some() {
        return true;
    }
    match group.mode {
        GroupMode::Always => true,
        GroupMode::Optional => level.optional_groups(layer).contains(&group.slug),
        GroupMode::Off => false,
    }
}

/// The string values a level field holds, or None if the level doesn't have
/// the field at all.
fn level_field_values<'a>(
    project: &'a Project,
    level: &'a Level,
    field: &str,
) -> Option<Vec<&'a str>> {
    let def = project.level_fields.iter().find(|f| f.slug == field);
    let stored = level.fields.get(field);
    if def.is_none() && stored.is_none() {
        return None;
    }
    let value = match stored {
        Some(v) if !v.is_null() => Some(v),
        _ => def.and_then(|d| d.default.as_ref()),
    };
    Some(match value {
        Some(serde_json::Value::String(s)) => vec![s.as_str()],
        Some(serde_json::Value::Array(a)) => a.iter().filter_map(|v| v.as_str()).collect(),
        _ => Vec::new(),
    })
}

// ---- compiled form ----

/// One non-`*` pattern cell: offset from the anchor and its test table.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct PatternCell {
    dx: i32,
    dy: i32,
    table: u32,
}

/// One pattern of a rule (`match` is part 0, each `require` one more), on
/// its own sampled grid.
struct Part {
    /// Index into [`Baker::grids`].
    grid: usize,
    /// Anchor first, then outwards.
    cells: Vec<PatternCell>,
    /// What a cell off the level reads as on this grid (`None`: no match).
    oob: Option<u16>,
}

/// One way of reading the patterns: an orientation from [`VARIANTS`].
struct Variant {
    /// 0..8, see [`VARIANTS`]. Feeds the hash streams.
    index: u64,
    /// Flip bits: 4 transpose (first), 1 mirror x, 2 mirror y. Both the
    /// pattern offsets and the drawn tiles are turned by them.
    bits: u8,
    flip: Option<Flip>,
    /// Per part, per cell (same order as the part's `cells`): linear grid
    /// offset and table offset, for anchors far enough from the edge that
    /// no bounds check is needed.
    linear: Vec<Vec<(isize, u32)>>,
}

/// A region rule fires as variant 0, as written.
static AS_WRITTEN: Variant = Variant {
    index: 0,
    bits: 0,
    flip: None,
    linear: Vec::new(),
};

/// The orientations a rule's symmetry can enable, in frozen order (the
/// index feeds the hash streams): `(index, flip bits)`. Variants 0-3 are the
/// mirrors (as written, x, y, both); 4 = quarter turn clockwise (bits 5),
/// 5 = counterclockwise (6), 6 = transpose (4), 7 = anti-transpose (7).
const VARIANTS: [(u64, u8); 8] = [
    (0, 0),
    (1, 1),
    (2, 2),
    (3, 3),
    (4, 5),
    (5, 6),
    (6, 4),
    (7, 7),
];

/// An offset from the anchor turned by flip bits: transpose first, then
/// mirror x, then mirror y (y grows down). Bits 5 map `(x, y)` to
/// `(-y, x)`, a quarter turn clockwise.
#[inline]
fn turn(bits: u8, dx: i32, dy: i32) -> (i32, i32) {
    let (mut x, mut y) = if bits & 4 != 0 { (dy, dx) } else { (dx, dy) };
    if bits & 1 != 0 {
        x = -x;
    }
    if bits & 2 != 0 {
        y = -y;
    }
    (x, y)
}

struct Alt {
    tiles: Vec<[u32; 2]>,
    /// Unflipped pixel offset of each tile from the anchor (stamp mode), or zeros.
    stamp_px: Vec<[i32; 2]>,
    weight: u64,
}

/// A region rule's selector and placement, compiled.
struct CompiledRegion {
    /// Index into [`Baker::grids`].
    grid: usize,
    /// `region.cells` over the grid's value indices.
    cells: Vec<bool>,
    /// `place.against`, if any.
    against: Option<Vec<bool>>,
    plan: RegionPlan,
}

struct CompiledRule<'p> {
    /// As expanded: an inline rule borrowed, a template rule owned. Its
    /// slug is the expanded one.
    rule: Cow<'p, Rule>,
    group: &'p RuleGroup,
    /// The rule's part of the hash (slug and salt).
    key: u64,
    /// `match`, then each `require`; empty for a region rule.
    parts: Vec<Part>,
    /// Max `|dx|`, `|dy|` the rule reads at, over every part and variant.
    radius: [i32; 2],
    /// Table of the anchor cell's own `match` test, if it has one (it is
    /// then `parts[0].cells[0]`).
    anchor_table: Option<u32>,
    /// `tables[cell.table + value index]`: does the value pass the cell's test.
    tables: Vec<bool>,
    variants: Vec<Variant>,
    /// `[x, y, x_offset, y_offset]`; a modulo of 0 never matches.
    modulo: [i32; 4],
    checker: Checker,
    chance: f64,
    perlin: Option<(Perlin, f64, u32)>,
    /// A region rule's selector and placement.
    region: Option<CompiledRegion>,
    /// A `write` output: index into [`Baker::writes`] and the value index.
    write: Option<(usize, u16)>,
    tileset: usize,
    /// Pixel size of one tile of `tileset`.
    tile_size: [i32; 2],
    alts: Vec<Alt>,
    total_weight: u64,
    random_offset: Option<[[i32; 2]; 2]>,
    offset_px: [i32; 2],
    alpha: f64,
    /// No offsets and alpha >= 1: an opaque tile locks the cell.
    opaque_locks: bool,
    /// `out.avoid_overlap`.
    avoid_overlap: bool,
    /// Its stamp `occupies`.
    occupies: bool,
}

/// Raw placement inside a rule's bucket; the rule supplies the rest. A
/// `write` rule's bucket holds one entry per firing (only `anchor` means
/// anything).
#[derive(Clone, Copy)]
pub(crate) struct Raw {
    px: [i32; 2],
    tile: [u32; 2],
    flip: Option<Flip>,
    pub(crate) anchor: [i32; 2],
}

/// Mutable bake state, separate from the compiled rules so an incremental
/// bake can keep it between strokes.
pub(crate) struct Output {
    pub(crate) buckets: Vec<Vec<Raw>>,
    opacity: Vec<Option<Arc<TileOpacity>>>,
    /// Anchors locked by the break rule in the current pass, row-major.
    locked: Vec<bool>,
    /// Occupied cells, row-major; empty when no rule uses occupancy.
    pub(crate) occupied: Vec<bool>,
    /// Per rule: anchors where it matched (gates passed, not locked) but
    /// every matching variant's footprint was occupied.
    pub(crate) blocked: Vec<Vec<[i32; 2]>>,
}

/// One pass of the compiled rules.
pub(crate) struct PassDef {
    /// Its rules: a range of [`Baker::rules`].
    pub(crate) rules: Range<usize>,
    /// Some rule checks or marks occupancy.
    pub(crate) occupancy: bool,
    /// The largest pattern reach of its pattern rules.
    pub(crate) radius: [i32; 2],
    /// Grids its rules read (indices into [`Baker::grids`]), sorted.
    pub(crate) reads: Vec<usize>,
}

/// A derived grid this layer writes.
pub(crate) struct WriteGrid {
    pub(crate) slug: Slug,
    /// Its index in [`Baker::grids`] when some rule of the layer reads it.
    read: Option<usize>,
    /// Per cell (when read): the override, `Some(0)` suppressed.
    ov: Vec<Option<u16>>,
}

pub(crate) struct Baker<'p> {
    w: i32,
    h: i32,
    grid_px: i32,
    seed: u64,
    pub(crate) grids: Vec<Vec<u16>>,
    /// Slug of each of `grids`.
    pub(crate) grid_slugs: Vec<Slug>,
    tilesets: Vec<Slug>,
    rules: Vec<CompiledRule<'p>>,
    pub(crate) passes: Vec<PassDef>,
    /// Sorted by slug.
    pub(crate) writes: Vec<WriteGrid>,
}

/// Per grid index, the cells a pass reads instead of the level's
/// ([`Baker::pass_overlay`]).
pub(crate) type Overlay = Vec<(usize, Vec<u16>)>;

/// What a full run of every pass produced.
pub(crate) struct Run {
    pub(crate) out: Output,
    /// Per pass, per written grid: what the pass wrote (0 = nothing).
    pub(crate) writes: Vec<Vec<Vec<u16>>>,
    /// Per written grid: every pass's writes laid over each other.
    pub(crate) computed: Vec<Vec<u16>>,
    /// Per rule: a region rule's candidates (row-major), else empty.
    pub(crate) candidates: Vec<Vec<[i32; 2]>>,
    /// Per pass: the occupancy after it (occupancy passes only).
    pub(crate) occ_after: Vec<Option<Vec<bool>>>,
    /// With `keep`: per pass, how the grids it read differ from the
    /// level's ([`Baker::pass_overlay`]); per rule, a region rule's
    /// per-region analysis.
    pub(crate) snapshots: Vec<Overlay>,
    pub(crate) regions: Vec<Option<(Vec<region::RegionInfo>, Vec<RegionCandidates>)>>,
}

/// The grid as a `w` x `h` array of value indices: 0 empty, `1..=n` the
/// layer's values in order, `n + 1` an index the layer doesn't define
/// (non-empty, matches no slug). Cells the stored grid doesn't cover are empty.
fn sample_grid(def: &LayerDef, data: Option<&IntGridLayer>, w: usize, h: usize) -> Vec<u16> {
    let unknown = def.values().len() as u16 + 1;
    let Some(data) = data else {
        return vec![0u16; w * h];
    };
    if data.size() == [w as u32, h as u32] && data.cells().iter().all(|&c| c <= unknown) {
        return data.cells().to_vec();
    }
    let mut cells = vec![0u16; w * h];
    let [dw, dh] = data.size();
    for y in 0..h.min(dh as usize) {
        for x in 0..w.min(dw as usize) {
            cells[y * w + x] = data.cells()[y * dw as usize + x].min(unknown);
        }
    }
    cells
}

/// Lookup table for one cell test over a grid's value indices.
pub(crate) fn test_table(def: &LayerDef, test: &CellMatch, rule: &str) -> Result<Vec<bool>> {
    let n = def.values().len();
    let in_set = |slug: &Slug| -> Result<Vec<bool>> {
        let mut t = vec![false; n + 2];
        if let Some(i) = def.values().iter().position(|v| v.slug == *slug) {
            t[i + 1] = true;
        } else if def.groups().iter().any(|g| g.slug == *slug) {
            for (i, v) in def.values().iter().enumerate() {
                t[i + 1] = v.group.as_ref() == Some(slug);
            }
        } else {
            return Err(Error::UnknownCellTest {
                rule: rule.to_string(),
                test: test.to_string(),
                grid: def.slug.to_string(),
            });
        }
        Ok(t)
    };
    let t = match test {
        CellMatch::Any => vec![true; n + 2],
        CellMatch::NonEmpty => (0..n + 2).map(|i| i != 0).collect(),
        CellMatch::Empty => (0..n + 2).map(|i| i == 0).collect(),
        CellMatch::Is(s) => in_set(s)?,
        CellMatch::Not(s) => in_set(s)?.into_iter().map(|b| !b).collect(),
        CellMatch::OneOf(m) | CellMatch::NotOneOf(m) => {
            let mut t = vec![false; n + 2];
            for member in m {
                let mt = match member {
                    CellTest::Empty => (0..n + 2).map(|i| i == 0).collect(),
                    CellTest::NonEmpty => (0..n + 2).map(|i| i != 0).collect(),
                    CellTest::Is(s) => in_set(s)?,
                };
                t.iter_mut().zip(mt).for_each(|(a, b)| *a |= b);
            }
            if matches!(test, CellMatch::NotOneOf(_)) {
                t.iter_mut().for_each(|a| *a = !*a);
            }
            t
        }
    };
    Ok(t)
}

impl WriteGrid {
    /// Its index in [`Baker::grids`] when some rule of the layer reads it.
    pub(crate) fn read_index(&self) -> Option<usize> {
        self.read
    }
}

impl<'p> Baker<'p> {
    /// Compile `def`'s rules for `level`. `upstream` holds the effective
    /// grids of the derived grids other auto layers write (every one this
    /// layer reads must be there).
    pub(crate) fn new(
        project: &'p Project,
        level: &'p Level,
        def: &'p LayerDef,
        upstream: &Effective,
    ) -> Result<Self> {
        let layer = def.slug.as_str();
        let set = project.rules_for(layer).expect("caller checked");
        let auto = def.auto().expect("caller checked");
        // The layer bakes on its source's grid, in cells of that grid.
        let [w, h] = project.layer_cells(def, level);
        let (w, h) = (w as usize, h as usize);
        let tile_pivot = auto.tile_pivot;
        let grid_px = project.layer_grid(def) as i32;
        let seed = hash::layer_seed(project, level, layer);
        // Templates expanded, named rows and legends resolved: what runs.
        let mut groups = project
            .expanded_rules(layer)
            .map_err(|source| Error::Expand {
                layer: layer.to_string(),
                source,
            })?;
        // Run order: by pass, file order within a pass.
        groups.sort_by_key(|g| g.group.pass);

        let mut baker = Baker {
            w: w as i32,
            h: h as i32,
            grid_px,
            seed,
            grids: Vec::new(),
            grid_slugs: Vec::new(),
            tilesets: Vec::new(),
            rules: Vec::new(),
            passes: Vec::new(),
            writes: Vec::new(),
        };

        // The derived grids this layer writes (every rule, whatever its
        // group or `active`: the writer doesn't change when a rule is off).
        let any_derived = project.layers.iter().any(|l| l.is_derived());
        for g in any_derived
            .then(|| project.layer_writes(layer))
            .into_iter()
            .flatten()
        {
            if !project.layer(g.as_str()).is_some_and(|l| l.is_derived()) {
                // The rule fails to compile with `BadWrite` if it runs.
                continue;
            }
            let writers = project.derived_writers(g.as_str());
            if writers.len() > 1 {
                return Err(Error::SecondWriter {
                    grid: g.to_string(),
                    layers: writers.iter().map(|s| s.to_string()).collect(),
                });
            }
            baker.writes.push(WriteGrid {
                slug: g,
                read: None,
                ov: Vec::new(),
            });
        }

        let tileset_index = |tilesets: &mut Vec<Slug>, slug: &Slug| -> Result<usize> {
            if project.tileset(slug.as_str()).is_none() {
                return Err(Error::UnknownTileset(slug.to_string()));
            }
            Ok(match tilesets.iter().position(|t| t == slug) {
                Some(i) => i,
                None => {
                    tilesets.push(slug.clone());
                    tilesets.len() - 1
                }
            })
        };

        for eg in groups {
            let group = eg.group;
            if !group_applies(project, level, layer, group) {
                continue;
            }
            for er in eg.rules.into_iter().filter(|r| r.rule.active) {
                let rule = er.rule;
                let label = format!("{}/{}", group.slug, rule.slug);
                let key = hash::rule_key(rule.slug.as_str(), rule.salt);

                // Pattern cells, one table per distinct test, flattened: a
                // cell's `table` is the offset of its test in `tables`.
                // Tables are interned by content, so two glyphs that mean
                // the same test (`#` and a legend `S: solid`) share one
                // offset, and the variant dedup below sees them as equal.
                let mut tables: Vec<bool> = Vec::new();
                let mut interned: Vec<(usize, usize)> = Vec::new();
                let mut parts: Vec<Part> = Vec::new();
                let mut region_c: Option<CompiledRegion> = None;
                if let Some(sel) = rule.as_match() {
                    for pat in sel.patterns() {
                        let grid_slug: &'p Slug = match &pat.grid {
                            // Borrow the slug from the project, not the
                            // (possibly owned) expanded rule.
                            Some(g) => {
                                project.layer(g.as_str()).map(|l| &l.slug).ok_or_else(|| {
                                    Error::BadGrid {
                                        rule: label.clone(),
                                        grid: g.to_string(),
                                    }
                                })?
                            }
                            None => &auto.source,
                        };
                        let gdef = project
                            .layer(grid_slug.as_str())
                            .filter(|l| l.is_int_grid() && project.layer_grid(l) == grid_px as u32)
                            .ok_or_else(|| Error::BadGrid {
                                rule: label.clone(),
                                grid: grid_slug.to_string(),
                            })?;
                        let grid = baker.grid_index(project, level, layer, gdef, upstream, w, h)?;
                        // Offsets are from the anchor: the `@` cell, else the
                        // center (an even size without `@` is a validation
                        // problem; it bakes anchored on the cell below-right of
                        // the middle).
                        let (ax, ay) = pat.anchor();
                        let mut glyphs: Vec<(char, u32)> = Vec::new();
                        let mut cells = Vec::new();
                        for (y, row) in pat.inline_rows().iter().enumerate() {
                            for (x, glyph) in row.chars().enumerate() {
                                let test =
                                    pat.resolve(glyph, Some(gdef)).ok_or(Error::UnknownGlyph {
                                        rule: label.clone(),
                                        glyph,
                                    })?;
                                if test == CellMatch::Any {
                                    continue;
                                }
                                let table = match glyphs.iter().find(|(g, _)| *g == glyph) {
                                    Some(&(_, t)) => t,
                                    None => {
                                        let t = test_table(gdef, &test, &label)?;
                                        let at = match interned
                                            .iter()
                                            .find(|&&(s, n)| tables[s..s + n] == t[..])
                                        {
                                            Some(&(s, _)) => s,
                                            None => {
                                                interned.push((tables.len(), t.len()));
                                                tables.extend_from_slice(&t);
                                                tables.len() - t.len()
                                            }
                                        };
                                        glyphs.push((glyph, at as u32));
                                        at as u32
                                    }
                                };
                                cells.push(PatternCell {
                                    dx: x as i32 - ax as i32,
                                    dy: y as i32 - ay as i32,
                                    table,
                                });
                            }
                        }
                        // Anchor first, then outwards: most rules fail on the anchor.
                        cells.sort_by_key(|c| (c.dx.abs() + c.dy.abs(), c.dy, c.dx));
                        // `out_of_bounds` applies to every pattern, each on its grid.
                        let oob = match &sel.out_of_bounds {
                            None => None,
                            Some(OutOfBounds::Empty) => Some(0),
                            Some(OutOfBounds::Value(v)) => {
                                Some(gdef.value_index(v.as_str()).ok_or_else(|| {
                                    Error::UnknownOutOfBounds {
                                        rule: label.clone(),
                                        value: v.to_string(),
                                        grid: gdef.slug.to_string(),
                                    }
                                })?)
                            }
                        };
                        parts.push(Part { grid, cells, oob });
                    }
                } else if let Some(rs) = rule.as_region() {
                    let g = rs.region.grid_or(&auto.source);
                    let gdef = project
                        .layer(g.as_str())
                        .filter(|l| l.is_int_grid() && project.layer_grid(l) == grid_px as u32)
                        .ok_or_else(|| Error::BadRegion {
                            rule: label.clone(),
                            grid: g.to_string(),
                            reason: "not an int_grid layer on this layer's grid".into(),
                        })?;
                    let grid = baker.grid_index(project, level, layer, gdef, upstream, w, h)?;
                    let cells = test_table(gdef, &rs.region.cells, &label)?;
                    let against = match &rs.place.against {
                        Some(a) => Some(test_table(gdef, a, &label)?),
                        None => None,
                    };
                    let p = &rs.place;
                    let [ex, ey] = p.every_xy();
                    let [mx, my] = p.margin_xy();
                    let axis = |every: u32, margin: u32| Axis {
                        spacing: p.spacing,
                        every: every.min(i32::MAX as u32) as i32,
                        margin: margin.min(i32::MAX as u32 / 2) as i32,
                        align: p.align,
                        jitter: p.jitter.min(i32::MAX as u32 / 2) as i32,
                    };
                    region_c = Some(CompiledRegion {
                        grid,
                        cells,
                        against,
                        plan: RegionPlan {
                            at: p.at,
                            x: axis(ex, mx),
                            y: axis(ey, my),
                            min_length: p.min_length.get().min(i32::MAX as u32) as i32,
                            select: rs.region.clone(),
                        },
                    });
                }

                let (variants, radius) = match rule.as_match() {
                    Some(sel) => {
                        let variants = variants(sel, &parts, w as i32);
                        let radius = variants.iter().fold([0, 0], |r, v| {
                            parts
                                .iter()
                                .flat_map(|p| &p.cells)
                                .map(|c| turn(v.bits, c.dx, c.dy))
                                .fold(r, |r, (x, y)| [r[0].max(x.abs()), r[1].max(y.abs())])
                        });
                        (variants, radius)
                    }
                    None => (Vec::new(), [0, 0]),
                };

                // Output: a write, or tileset, alternatives and stamp layout.
                let mut write = None;
                let mut tileset = 0;
                let mut tile_size = [0, 0];
                let mut alts: Vec<Alt> = Vec::new();
                let mut random_offset = None;
                let mut offset_px = [0, 0];
                let mut alpha = 1.0;
                let mut opaque_locks = false;
                let mut avoid_overlap = false;
                let mut occupies = false;
                if let Some(wr) = rule.write() {
                    let bad = |reason: &str| Error::BadWrite {
                        rule: label.clone(),
                        grid: wr.grid.to_string(),
                        reason: reason.to_string(),
                    };
                    let wdef = project
                        .layer(wr.grid.as_str())
                        .filter(|l| l.is_derived())
                        .ok_or_else(|| bad("not a derived int_grid layer"))?;
                    if project.layer_grid(wdef) != grid_px as u32 {
                        return Err(bad("not on this layer's grid"));
                    }
                    let value = wdef
                        .value_index(wr.value.as_str())
                        .ok_or_else(|| bad(&format!("{} is not a value of it", wr.value)))?;
                    let wi = baker
                        .writes
                        .iter()
                        .position(|g| g.slug == wr.grid)
                        .expect("layer_writes lists every derived write");
                    write = Some((wi, value));
                } else if let Some(rule_out) = rule.out() {
                    let (alts_src, stamp_mode, pivot, out_tileset): (
                        Vec<TileAlt>,
                        bool,
                        [f64; 2],
                        &Slug,
                    ) =
                        match rule_out {
                            RuleOutput::Stamp(o) => {
                                let st = set.stamps.iter().find(|s| s.slug == o.stamp).ok_or_else(
                                    || Error::UnknownStamp {
                                        rule: label.clone(),
                                        stamp: o.stamp.to_string(),
                                    },
                                )?;
                                occupies = st.occupies;
                                let [c, r, sw, sh] = st.rect;
                                (
                                    vec![TileAlt::rect(c, r, sw, sh)],
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
                    avoid_overlap = rule_out.avoid_overlap();
                    let draw = rule_out.draw();

                    // The level's substitution beats the rule's own tileset.
                    let ts = level.tileset_for(layer, out_tileset);
                    let ts_def = project
                        .tileset(ts.as_str())
                        .ok_or_else(|| Error::UnknownTileset(ts.to_string()))?;
                    tileset = tileset_index(&mut baker.tilesets, ts)?;
                    tile_size = [ts_def.tile_size[0] as i32, ts_def.tile_size[1] as i32];
                    // A turned tile only fits its own square.
                    if rule
                        .as_match()
                        .is_some_and(|s| s.symmetry.contains(&Symmetry::Rotate))
                        && tile_size[0] != tile_size[1]
                    {
                        return Err(Error::RotateNeedsSquareTiles {
                            rule: label,
                            tileset: ts.to_string(),
                            size: ts_def.tile_size,
                        });
                    }

                    // An empty alternative list, or all weights 0, never fires (as in LDtk).
                    alts = alts_src
                        .into_iter()
                        .filter(|a| a.weight > 0)
                        .map(|a| {
                            let stamp_px = match (stamp_mode, a.bounds()) {
                                (true, Some(b)) => a
                                    .tiles
                                    .iter()
                                    .map(|&t| {
                                        stamp_offset(t, b, pivot, tile_pivot, tile_size, grid_px)
                                    })
                                    .collect(),
                                _ => vec![[0, 0]; a.tiles.len()],
                            };
                            Alt {
                                stamp_px,
                                weight: a.weight as u64,
                                tiles: a.tiles,
                            }
                        })
                        .collect();
                    if alts.is_empty() {
                        continue;
                    }
                    random_offset = draw
                        .random_offset_px
                        .as_ref()
                        .filter(|r| r.x != [0, 0] || r.y != [0, 0])
                        .map(|r| {
                            let s = |[a, b]: [i32; 2]| [a.min(b), a.max(b)];
                            [s(r.x), s(r.y)]
                        });
                    opaque_locks =
                        draw.offset_px == [0, 0] && random_offset.is_none() && draw.alpha >= 1.0;
                    (offset_px, alpha) = (draw.offset_px, draw.alpha);
                }
                let total_weight = alts.iter().map(|a| a.weight).sum();

                let (modulo, checker, chance, perlin) = match rule.as_match() {
                    Some(sel) => (
                        match &sel.modulo {
                            Some(m) => [
                                i32::try_from(m.x).unwrap_or(0),
                                i32::try_from(m.y).unwrap_or(0),
                                m.x_offset,
                                m.y_offset,
                            ],
                            None => [1, 1, 0, 0],
                        },
                        sel.checker,
                        sel.chance,
                        sel.perlin.as_ref().filter(|p| p.active).map(|p| {
                            let pkey = hash::mix(key ^ p.seed as u64);
                            let pseed = hash(seed, pkey, 0, 0, stream::PERLIN);
                            (Perlin::new(pseed), p.scale, p.octaves)
                        }),
                    ),
                    None => ([1, 1, 0, 0], Checker::None, 1.0, None),
                };

                let anchor_table = parts
                    .first()
                    .and_then(|p| p.cells.first())
                    .filter(|c| c.dx == 0 && c.dy == 0)
                    .map(|c| c.table);
                baker.rules.push(CompiledRule {
                    rule,
                    group,
                    key,
                    parts,
                    radius,
                    anchor_table,
                    tables,
                    variants,
                    modulo,
                    checker,
                    chance,
                    perlin,
                    region: region_c,
                    write,
                    tileset,
                    tile_size,
                    alts,
                    total_weight,
                    random_offset,
                    offset_px,
                    alpha,
                    opaque_locks,
                    avoid_overlap,
                    occupies,
                });
            }
        }

        // Passes: runs of rules with the same `pass`.
        let mut start = 0;
        while start < baker.rules.len() {
            let pass = baker.rules[start].group.pass;
            let mut end = start;
            while end < baker.rules.len() && baker.rules[end].group.pass == pass {
                end += 1;
            }
            let rs = &baker.rules[start..end];
            let mut reads: Vec<usize> = rs
                .iter()
                .flat_map(|r| {
                    r.parts
                        .iter()
                        .map(|p| p.grid)
                        .chain(r.region.as_ref().map(|g| g.grid))
                })
                .collect();
            reads.sort_unstable();
            reads.dedup();
            baker.passes.push(PassDef {
                rules: start..end,
                occupancy: rs.iter().any(|r| r.avoid_overlap || r.occupies),
                radius: rs.iter().fold([0, 0], |a, r| {
                    [a[0].max(r.radius[0]), a[1].max(r.radius[1])]
                }),
                reads,
            });
            start = end;
        }
        Ok(baker)
    }

    /// The index of `gdef`'s sampled grid in [`Self::grids`], sampling it
    /// the first time.
    #[allow(clippy::too_many_arguments)]
    fn grid_index(
        &mut self,
        project: &Project,
        level: &Level,
        layer: &str,
        gdef: &LayerDef,
        upstream: &Effective,
        w: usize,
        h: usize,
    ) -> Result<usize> {
        if let Some(i) = self.grid_slugs.iter().position(|s| *s == gdef.slug) {
            return Ok(i);
        }
        let values = if !gdef.is_derived() {
            sample_grid(gdef, level.int_grid(gdef.slug.as_str()), w, h)
        } else if let Some(wi) = self.writes.iter().position(|g| g.slug == gdef.slug) {
            // Written here: the overrides, and per pass what earlier passes
            // wrote under them.
            let unknown = gdef.values().len() as u16 + 1;
            let mut ov = vec![None; w * h];
            if let Some(o) = level.overrides(gdef.slug.as_str()) {
                let [ow, oh] = o.size();
                for y in 0..h.min(oh as usize) {
                    for x in 0..w.min(ow as usize) {
                        ov[y * w + x] = match o.get(x as u32, y as u32) {
                            Some(Override::Value(v)) => Some(v.min(unknown)),
                            Some(Override::Suppress) => Some(0),
                            _ => None,
                        };
                    }
                }
            }
            let values = ov.iter().map(|o| o.unwrap_or(0)).collect();
            self.writes[wi].ov = ov;
            self.writes[wi].read = Some(self.grids.len());
            values
        } else if let Some(writer) = project.derived_writer(gdef.slug.as_str()) {
            let eff = upstream
                .get(&gdef.slug)
                .ok_or_else(|| Error::UpstreamFailed {
                    layer: layer.to_string(),
                    upstream: writer.to_string(),
                    reason: format!("no baked grid for {}", gdef.slug),
                })?;
            sample_grid(gdef, Some(eff), w, h)
        } else {
            // Nobody writes it: the overrides are all there is.
            let eff = derived::effective(project, level, gdef, None);
            sample_grid(gdef, Some(&eff), w, h)
        };
        self.grids.push(values);
        self.grid_slugs.push(gdef.slug.clone());
        Ok(self.grids.len() - 1)
    }

    pub(crate) fn output(&self) -> Output {
        let n = (self.w * self.h) as usize;
        Output {
            buckets: self.rules.iter().map(|_| Vec::new()).collect(),
            opacity: vec![None; self.tilesets.len()],
            locked: vec![false; n],
            occupied: if self.uses_occupancy() {
                vec![false; n]
            } else {
                Vec::new()
            },
            blocked: self.rules.iter().map(|_| Vec::new()).collect(),
        }
    }

    pub(crate) fn uses_occupancy(&self) -> bool {
        self.passes.iter().any(|p| p.occupancy)
    }

    /// Whether the layer uses anything that the plain re-sweep of the
    /// incremental bake can't patch: several passes, region rules, writes or
    /// occupancy.
    pub(crate) fn is_staged(&self) -> bool {
        self.passes.len() > 1
            || !self.writes.is_empty()
            || self.uses_occupancy()
            || self.rules.iter().any(|r| r.region.is_some())
    }

    /// Whether [`rule_matches`] needs a bake for this layer: region
    /// candidates, or a pass reading what an earlier one wrote.
    fn needs_run_for_matches(&self) -> bool {
        self.rules.iter().any(|r| r.region.is_some())
            || (self.passes.len() > 1 && self.writes.iter().any(|g| g.read.is_some()))
    }

    /// The pass (index into [`Self::passes`]) rule `ri` runs in.
    pub(crate) fn pass_of(&self, ri: usize) -> usize {
        self.passes
            .iter()
            .position(|p| p.rules.contains(&ri))
            .expect("every rule is in a pass")
    }

    /// What pass `p` reads differently from [`Self::grids`]: each of the
    /// layer's own written grids it reads, as the overrides over `computed`
    /// (what the earlier passes wrote), by grid index. Empty when it reads
    /// none, or nothing was written yet. Only those grids are built; the
    /// others are read in place ([`Self::grid_view`]).
    pub(crate) fn pass_overlay(&self, p: usize, computed: &[Vec<u16>]) -> Overlay {
        let own: Vec<(usize, usize)> = self
            .writes
            .iter()
            .enumerate()
            .filter_map(|(wi, g)| g.read.map(|gi| (wi, gi)))
            .filter(|(_, gi)| self.passes[p].reads.contains(gi))
            .collect();
        if own.is_empty() || computed.iter().all(|c| c.iter().all(|&v| v == 0)) {
            return Vec::new();
        }
        own.into_iter()
            .map(|(wi, gi)| {
                let ov = &self.writes[wi].ov;
                let cells = (0..self.grids[gi].len())
                    .map(|i| ov[i].unwrap_or(computed[wi][i]))
                    .collect();
                (gi, cells)
            })
            .collect()
    }

    /// The grids a pass reads: [`Self::grids`], each replaced by its
    /// version in `overlay` ([`Self::pass_overlay`]) when it has one.
    pub(crate) fn grid_view<'a>(&'a self, overlay: &'a Overlay) -> Vec<&'a [u16]> {
        self.grids
            .iter()
            .enumerate()
            .map(|(gi, g)| match overlay.iter().find(|(o, _)| *o == gi) {
                Some((_, cells)) => &cells[..],
                None => &g[..],
            })
            .collect()
    }

    /// Candidates of region rule `ri` on `grids`, with the per-region
    /// analysis.
    pub(crate) fn region_candidates(
        &self,
        ri: usize,
        grids: &[&[u16]],
    ) -> (region::Regions, Vec<RegionCandidates>) {
        let r = &self.rules[ri];
        let reg = r.region.as_ref().expect("a region rule");
        let values = &grids[reg.grid];
        let mut regions = region::label(values, self.w, self.h, &reg.cells);
        if let PlaceAt::Along(e) = reg.plan.at {
            region::edge_runs(&mut regions, values, e, reg.against.as_deref());
        }
        let per = region::place(&reg.plan, &regions, self.seed, r.key);
        (regions, per)
    }

    /// Run every pass over the whole level. With `keep`, also keep each
    /// pass's grids and each region rule's analysis (previews).
    pub(crate) fn run(&self, images: &TilesetImages, keep: bool) -> Result<Run> {
        let n = (self.w * self.h) as usize;
        let mut out = self.output();
        let mut computed: Vec<Vec<u16>> = self.writes.iter().map(|_| vec![0; n]).collect();
        let mut candidates: Vec<Vec<[i32; 2]>> = self.rules.iter().map(|_| Vec::new()).collect();
        let mut regions: Vec<_> = self.rules.iter().map(|_| None).collect();
        let mut pass_writes = Vec::with_capacity(self.passes.len());
        let mut occ_after = Vec::with_capacity(self.passes.len());
        let mut snapshots = Vec::new();
        for p in 0..self.passes.len() {
            let ov = self.pass_overlay(p, &computed);
            let grids = self.grid_view(&ov);
            for ri in self.passes[p].rules.clone() {
                if self.rules[ri].region.is_some() {
                    let (found, per) = self.region_candidates(ri, &grids);
                    candidates[ri] = region::flatten(&per);
                    if keep {
                        regions[ri] = Some((found.regions, per));
                    }
                }
            }
            let mut writes: Vec<Vec<u16>> = self.writes.iter().map(|_| vec![0; n]).collect();
            self.sweep(
                p,
                &grids,
                &candidates,
                &mut out,
                &mut writes,
                images,
                [0, 0],
                [self.w, self.h],
            )?;
            drop(grids);
            overlay(&mut computed, &writes);
            if keep {
                snapshots.push(ov);
            }
            pass_writes.push(writes);
            occ_after.push(self.passes[p].occupancy.then(|| out.occupied.clone()));
        }
        Ok(Run {
            out,
            writes: pass_writes,
            computed,
            candidates,
            occ_after,
            snapshots,
            regions,
        })
    }

    /// Evaluate every anchor in `[min, max)` (cells) for a layer of one
    /// pass that reads no grid it writes (the plain incremental re-sweep).
    pub(crate) fn bake_rect(
        &self,
        out: &mut Output,
        images: &TilesetImages,
        min: [i32; 2],
        max: [i32; 2],
    ) -> Result<()> {
        debug_assert!(!self.is_staged());
        for p in 0..self.passes.len() {
            let grids: Vec<&[u16]> = self.grids.iter().map(|g| &g[..]).collect();
            self.sweep(p, &grids, &[], out, &mut [], images, min, max)?;
        }
        Ok(())
    }

    /// Evaluate pass `p`'s rules at every anchor in `[min, max)` (cells),
    /// reading `grids`, firing region rules at their `candidates` (indexed
    /// by rule). Locks and this pass's `writes` are reset in the rect first;
    /// occupancy is read and marked in `out.occupied`.
    ///
    /// Rule-major: each rule sweeps the rect row-major (so its bucket stays
    /// sorted by anchor), skipping anchors an earlier rule locked. That is
    /// the same result as walking rules per cell, because a cell's lock
    /// only depends on the earlier rules of the pass at that cell; so is
    /// "the first write to a cell in a pass wins", writes being at the
    /// anchor.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn sweep(
        &self,
        p: usize,
        grids: &[&[u16]],
        candidates: &[Vec<[i32; 2]>],
        out: &mut Output,
        writes: &mut [Vec<u16>],
        images: &TilesetImages,
        min: [i32; 2],
        max: [i32; 2],
    ) -> Result<()> {
        let (x0, y0) = (min[0].max(0), min[1].max(0));
        let (x1, y1) = (max[0].min(self.w), max[1].min(self.h));
        if x0 >= x1 || y0 >= y1 {
            return Ok(());
        }
        for y in y0..y1 {
            let row = (y * self.w) as usize;
            out.locked[row + x0 as usize..row + x1 as usize].fill(false);
            for w in writes.iter_mut() {
                w[row + x0 as usize..row + x1 as usize].fill(0);
            }
        }
        for ri in self.passes[p].rules.clone() {
            let r = &self.rules[ri];
            if r.region.is_some() {
                let cands = &candidates[ri];
                let lo = cands.partition_point(|c| (c[1], c[0]) < (y0, x0));
                for &[x, y] in &cands[lo..] {
                    if y >= y1 {
                        break;
                    }
                    if x < x0 || x >= x1 || out.locked[(y * self.w + x) as usize] {
                        continue;
                    }
                    self.fire(ri, x, y, std::iter::once(&AS_WRITTEN), out, writes, images)?;
                }
                continue;
            }
            let grid = &grids[r.parts[0].grid];
            if r.write.is_none() && !r.avoid_overlap && !r.occupies {
                // A rule that only draws: the hot loop, kept tight.
                self.sweep_plain(ri, grids, out, images, [x0, y0], [x1, y1])?;
                continue;
            }
            for y in y0..y1 {
                for x in x0..x1 {
                    let cell = (y * self.w + x) as usize;
                    if out.locked[cell] || !r.modulo_ok(x, y) {
                        continue;
                    }
                    // The anchor's own test is the same in every variant.
                    if let Some(t) = r.anchor_table
                        && !r.tables[t as usize + grid[cell] as usize]
                    {
                        continue;
                    }
                    let (w, h) = (self.w, self.h);
                    let mut matching = r
                        .variants
                        .iter()
                        .filter(|v| r.matches(grids, w, h, x, y, v));
                    let Some(first) = matching.next() else {
                        continue;
                    };
                    // chance and perlin don't depend on the variant: rolled
                    // once, on the first matching variant.
                    if !r.gate(self.seed, x, y) {
                        continue;
                    }
                    self.fire(
                        ri,
                        x,
                        y,
                        std::iter::once(first).chain(matching),
                        out,
                        writes,
                        images,
                    )?;
                }
            }
        }
        Ok(())
    }

    /// Pattern rule `ri`, which only draws (no write, no occupancy), over
    /// `[min, max)` (already clipped).
    fn sweep_plain(
        &self,
        ri: usize,
        grids: &[&[u16]],
        out: &mut Output,
        images: &TilesetImages,
        min: [i32; 2],
        max: [i32; 2],
    ) -> Result<()> {
        let r = &self.rules[ri];
        let grid = &grids[r.parts[0].grid];
        let bucket = &mut out.buckets[ri];
        for y in min[1]..max[1] {
            for x in min[0]..max[0] {
                let cell = (y * self.w + x) as usize;
                if out.locked[cell] || !r.modulo_ok(x, y) {
                    continue;
                }
                // The anchor's own test is the same in every variant.
                if let Some(t) = r.anchor_table
                    && !r.tables[t as usize + grid[cell] as usize]
                {
                    continue;
                }
                let start = bucket.len();
                let mut hit = false;
                let mut gate: Option<bool> = None;
                for v in &r.variants {
                    if !r.matches(grids, self.w, self.h, x, y, v) {
                        continue;
                    }
                    // chance and perlin don't depend on the variant.
                    if !*gate.get_or_insert_with(|| r.gate(self.seed, x, y)) {
                        break;
                    }
                    hit = true;
                    r.place(self.seed, self.grid_px, x, y, v, bucket);
                    if r.rule.break_on_match {
                        break;
                    }
                }
                if !hit {
                    continue;
                }
                // The break rule (see module docs).
                if r.rule.break_on_match {
                    out.locked[cell] = true;
                } else if r.opaque_locks && bucket.len() > start {
                    let op = match &out.opacity[r.tileset] {
                        Some(o) => o.clone(),
                        None => {
                            let o = images.opacity(self.tilesets[r.tileset].as_str())?;
                            out.opacity[r.tileset] = Some(o.clone());
                            o
                        }
                    };
                    if bucket[start..]
                        .iter()
                        .any(|p| op.is_opaque(p.tile[0], p.tile[1]))
                    {
                        out.locked[cell] = true;
                    }
                }
            }
        }
        Ok(())
    }

    /// Rule `ri` at anchor `(x, y)`, given the variants that match there
    /// (gates passed): draw or write, check and mark occupancy, lock.
    #[allow(clippy::too_many_arguments)]
    #[inline]
    fn fire<'v>(
        &self,
        ri: usize,
        x: i32,
        y: i32,
        matching: impl Iterator<Item = &'v Variant>,
        out: &mut Output,
        writes: &mut [Vec<u16>],
        images: &TilesetImages,
    ) -> Result<()> {
        let r = &self.rules[ri];
        let Output {
            buckets,
            opacity,
            locked,
            occupied,
            blocked,
        } = out;
        let bucket = &mut buckets[ri];
        let start = bucket.len();
        let mut hit = false;
        let mut skipped = false;
        for v in matching {
            if r.write.is_some() {
                // A write fires once per anchor, whatever the variant.
                hit = true;
                break;
            }
            if r.avoid_overlap || r.occupies {
                let before = bucket.len();
                r.place(self.seed, self.grid_px, x, y, v, bucket);
                if r.avoid_overlap
                    && self.footprint(r, v.bits, &bucket[before..], &mut |c| occupied[c])
                {
                    bucket.truncate(before);
                    skipped = true;
                    continue;
                }
                if r.occupies {
                    self.footprint(r, v.bits, &bucket[before..], &mut |c| {
                        occupied[c] = true;
                        false
                    });
                }
            } else {
                r.place(self.seed, self.grid_px, x, y, v, bucket);
            }
            hit = true;
            if r.rule.break_on_match {
                break;
            }
        }
        if !hit {
            if skipped {
                blocked[ri].push([x, y]);
            }
            return Ok(());
        }
        let cell = (y * self.w + x) as usize;
        if let Some((wi, value)) = r.write {
            bucket.push(Raw {
                px: [0, 0],
                tile: [0, 0],
                flip: None,
                anchor: [x, y],
            });
            // The first write to a cell in a pass wins.
            let c = &mut writes[wi][cell];
            if *c == 0 {
                *c = value;
            }
        }
        // The break rule (see module docs).
        if r.rule.break_on_match {
            locked[cell] = true;
        } else if r.opaque_locks && bucket.len() > start {
            let op = match &opacity[r.tileset] {
                Some(o) => o.clone(),
                None => {
                    let o = images.opacity(self.tilesets[r.tileset].as_str())?;
                    opacity[r.tileset] = Some(o.clone());
                    o
                }
            };
            if bucket[start..]
                .iter()
                .any(|p| op.is_opaque(p.tile[0], p.tile[1]))
            {
                locked[cell] = true;
            }
        }
        Ok(())
    }

    /// Visit the footprint of `raws` (tiles of rule `r` drawn as variant
    /// `v`): every in-level cell whose center lies inside at least one
    /// tile's half-open pixel box. Stops and returns true as soon as `f`
    /// does.
    fn footprint(
        &self,
        r: &CompiledRule,
        bits: u8,
        raws: &[Raw],
        f: &mut dyn FnMut(usize) -> bool,
    ) -> bool {
        let g = self.grid_px;
        let [mut tw, mut th] = r.tile_size;
        if bits & 4 != 0 {
            std::mem::swap(&mut tw, &mut th);
        }
        for raw in raws {
            let [cx0, cx1] = footprint_axis(raw.px[0], tw, g, self.w);
            let [cy0, cy1] = footprint_axis(raw.px[1], th, g, self.h);
            for cy in cy0..cy1 {
                for cx in cx0..cx1 {
                    if f((cy * self.w + cx) as usize) {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// The per-pass state of a full run, worked out from its result instead
    /// of baking again (for [`crate::BakeCache::seed`]): each rule's
    /// firings from `fired`, each pass's writes (the first write rule of
    /// the pass listed at an anchor wins), the region candidates from the
    /// grids each pass reads, and the occupancy from the footprints of the
    /// occupying rules' placements. `None` when `bake` doesn't fit (rule
    /// slugs repeat, or the computed grids or occupancy don't come out the
    /// same).
    pub(crate) fn rebuild(&self, bake: &LayerBake) -> Option<Rebuilt> {
        let n = (self.w * self.h) as usize;
        let mut index: BTreeMap<&Slug, usize> = BTreeMap::new();
        for (ri, r) in self.rules.iter().enumerate() {
            if index.insert(&r.rule.slug, ri).is_some() {
                return None;
            }
        }
        let pass_of: Vec<usize> = (0..self.rules.len()).map(|ri| self.pass_of(ri)).collect();
        let mut firings: Vec<Vec<[i32; 2]>> = self.rules.iter().map(|_| Vec::new()).collect();
        let mut writes: Vec<Vec<Vec<u16>>> = self
            .passes
            .iter()
            .map(|_| self.writes.iter().map(|_| vec![0; n]).collect())
            .collect();
        for (a, rules) in &bake.fired {
            if a[0] < 0 || a[1] < 0 || a[0] >= self.w || a[1] >= self.h {
                return None;
            }
            let cell = (a[1] * self.w + a[0]) as usize;
            for slug in rules {
                let ri = *index.get(slug)?;
                firings[ri].push(*a);
                if let Some((wi, value)) = self.rules[ri].write {
                    let c = &mut writes[pass_of[ri]][wi][cell];
                    if *c == 0 {
                        *c = value;
                    }
                }
            }
        }
        for f in &mut firings {
            f.sort_by_key(|c| (c[1], c[0]));
        }
        let mut computed: Vec<Vec<u16>> = self.writes.iter().map(|_| vec![0; n]).collect();
        let mut candidates: Vec<Vec<[i32; 2]>> = self.rules.iter().map(|_| Vec::new()).collect();
        let mut occ = if self.uses_occupancy() {
            vec![false; n]
        } else {
            Vec::new()
        };
        let mut occ_after = Vec::with_capacity(self.passes.len());
        for (p, pd) in self.passes.iter().enumerate() {
            let ov = self.pass_overlay(p, &computed);
            let grids = self.grid_view(&ov);
            for ri in pd.rules.clone() {
                if self.rules[ri].region.is_some() {
                    candidates[ri] = region::flatten(&self.region_candidates(ri, &grids).1);
                }
            }
            drop(grids);
            overlay(&mut computed, &writes[p]);
            if pd.occupancy {
                for t in &bake.placements {
                    let &ri = index.get(&t.rule)?;
                    if pass_of[ri] != p || !self.rules[ri].occupies {
                        continue;
                    }
                    let raw = Raw {
                        px: t.px,
                        tile: t.tile,
                        flip: t.flip,
                        anchor: t.anchor,
                    };
                    self.footprint(&self.rules[ri], Flip::bits_of(t.flip), &[raw], &mut |c| {
                        occ[c] = true;
                        false
                    });
                }
            }
            occ_after.push(pd.occupancy.then(|| occ.clone()));
        }
        if self.derived_layers(&computed) != bake.derived || occ != bake.occupied {
            return None;
        }
        Some(Rebuilt {
            firings,
            writes,
            candidates,
            occ_after,
        })
    }

    /// Where pattern rule `ri` matches on `grids` (see [`rule_matches`]).
    pub(crate) fn pattern_matches(&self, ri: usize, grids: &[&[u16]]) -> Matches {
        let r = &self.rules[ri];
        let mut m = Matches::new();
        for y in 0..self.h {
            for x in 0..self.w {
                if !r.modulo_ok(x, y) {
                    continue;
                }
                let flips: Vec<Option<Flip>> = r
                    .variants
                    .iter()
                    .filter(|v| r.matches(grids, self.w, self.h, x, y, v))
                    .map(|v| v.flip)
                    .collect();
                if !flips.is_empty() {
                    m.insert([x, y], flips);
                }
            }
        }
        m
    }

    /// `[w, h]` in cells of the layer's grid.
    pub(crate) fn size(&self) -> [i32; 2] {
        [self.w, self.h]
    }

    pub(crate) fn grid_px(&self) -> i32 {
        self.grid_px
    }

    /// The largest pattern reach from the anchor over every compiled rule:
    /// an edit can only change anchors this close to it.
    pub(crate) fn max_radius(&self) -> [i32; 2] {
        self.rules.iter().fold([0, 0], |r, c| {
            [r[0].max(c.radius[0]), r[1].max(c.radius[1])]
        })
    }

    /// `group/rule` of every compiled rule, in rule order.
    pub(crate) fn rule_keys(&self) -> Vec<(Slug, Slug)> {
        self.rules
            .iter()
            .map(|r| (r.group.slug.clone(), r.rule.slug.clone()))
            .collect()
    }

    pub(crate) fn rule_slug(&self, ri: usize) -> &Slug {
        &self.rules[ri].rule.slug
    }

    pub(crate) fn rule_count(&self) -> usize {
        self.rules.len()
    }

    /// Whether rule `ri` writes (its bucket holds firings, not tiles).
    pub(crate) fn is_write(&self, ri: usize) -> bool {
        self.rules[ri].write.is_some()
    }

    pub(crate) fn is_region(&self, ri: usize) -> bool {
        self.rules[ri].region.is_some()
    }

    /// Rule `ri`'s rule and group, for previews.
    pub(crate) fn rule_info(&self, ri: usize) -> (&Rule, &RuleGroup) {
        (&self.rules[ri].rule, self.rules[ri].group)
    }

    /// The grid rule `ri` reads first: `match`'s, or the region's.
    pub(crate) fn rule_grid(&self, ri: usize) -> &Slug {
        let r = &self.rules[ri];
        let gi = match &r.region {
            Some(g) => g.grid,
            None => r.parts[0].grid,
        };
        &self.grid_slugs[gi]
    }

    /// Rule indices in draw order: passes bottom to top, within a pass the
    /// last rule first.
    pub(crate) fn draw_order(&self) -> Vec<usize> {
        self.passes
            .iter()
            .flat_map(|p| p.rules.clone().rev())
            .collect()
    }

    /// Pixel size of rule `ri`'s tiles.
    pub(crate) fn tile_size(&self, ri: usize) -> [i32; 2] {
        self.rules[ri].tile_size
    }

    /// The full placement for a raw one of rule `ri`.
    pub(crate) fn placement(&self, ri: usize, p: &Raw) -> Placement {
        let r = &self.rules[ri];
        Placement {
            px: p.px,
            tileset: self.tilesets[r.tileset].clone(),
            tile: p.tile,
            flip: p.flip,
            alpha: r.alpha,
            rule: r.rule.slug.clone(),
            group: r.group.slug.clone(),
            anchor: p.anchor,
        }
    }

    /// The computed grids as layers, by slug.
    pub(crate) fn derived_layers(&self, computed: &[Vec<u16>]) -> BTreeMap<Slug, IntGridLayer> {
        self.writes
            .iter()
            .zip(computed)
            .map(|(g, c)| {
                (
                    g.slug.clone(),
                    IntGridLayer::from_cells(
                        g.slug.clone(),
                        [self.w as u32, self.h as u32],
                        c.clone(),
                    ),
                )
            })
            .collect()
    }

    pub(crate) fn finish(&self, out: Output, computed: &[Vec<u16>]) -> LayerBake {
        let mut fired: BTreeMap<[i32; 2], Vec<Slug>> = BTreeMap::new();
        for (r, bucket) in self.rules.iter().zip(&out.buckets) {
            let mut prev = None;
            for p in bucket {
                if prev != Some(p.anchor) {
                    fired.entry(p.anchor).or_default().push(r.rule.slug.clone());
                    prev = Some(p.anchor);
                }
            }
        }
        let total = out
            .buckets
            .iter()
            .zip(&self.rules)
            .filter(|(_, r)| r.write.is_none())
            .map(|(b, _)| b.len())
            .sum();
        let mut placements = Vec::with_capacity(total);
        for ri in self.draw_order() {
            if self.rules[ri].write.is_some() {
                continue;
            }
            placements.extend(out.buckets[ri].iter().map(|p| self.placement(ri, p)));
        }
        LayerBake {
            placements,
            fired,
            derived: self.derived_layers(computed),
            occupied: out.occupied,
        }
    }
}

/// Per-pass state rebuilt from a bake ([`Baker::rebuild`]).
pub(crate) struct Rebuilt {
    pub(crate) firings: Vec<Vec<[i32; 2]>>,
    pub(crate) writes: Vec<Vec<Vec<u16>>>,
    pub(crate) candidates: Vec<Vec<[i32; 2]>>,
    pub(crate) occ_after: Vec<Option<Vec<bool>>>,
}

/// Lay `writes` (0 = nothing written) over `computed`, grid by grid.
pub(crate) fn overlay(computed: &mut [Vec<u16>], writes: &[Vec<u16>]) {
    for (c, w) in computed.iter_mut().zip(writes) {
        for (a, &b) in c.iter_mut().zip(w) {
            if b != 0 {
                *a = b;
            }
        }
    }
}

/// The cells `[lo, hi)` on one axis whose center lies in the half-open
/// pixel box `[x0, x0 + size)`, clipped to `0..n`: cell `c` is in it iff
/// `2 x0 <= 2 c g + g < 2 (x0 + size)`.
pub(crate) fn footprint_axis(x0: i32, size: i32, g: i32, n: i32) -> [i32; 2] {
    let ceil_div = |a: i64, b: i64| -> i64 { a.div_euclid(b) + i64::from(a.rem_euclid(b) != 0) };
    let (x0, size, g) = (x0 as i64, size as i64, g.max(1) as i64);
    let lo = ceil_div(2 * x0 - g, 2 * g).max(0);
    let hi = ceil_div(2 * (x0 + size) - g, 2 * g).min(n as i64);
    [lo as i32, hi.max(lo) as i32]
}

/// Unflipped pixel offset of `tile` from the anchor cell's top-left in a
/// stamp whose atlas bounds are `[left, top, right, bottom]`.
///
/// The layout steps by the tileset's tile size `t` (spacing only matters for
/// where tiles are cut from the image). The stamp's pixel box is
/// `(right - left + 1) * t` wide; `pivot` picks the point of that box that
/// sits on the same point of the anchor cell (`g` px), so the box's left edge
/// is at `pivot * (g - box)`. The layer's `tile_pivot` shifts by a fraction
/// of a cell. Floored to whole pixels. With `t == g` this is LDtk's
/// `((col - left) - pivot * (right - left) + tile_pivot) * g`.
fn stamp_offset(
    tile: [u32; 2],
    bounds: [u32; 4],
    pivot: [f64; 2],
    tile_pivot: [f64; 2],
    t: [i32; 2],
    g: i32,
) -> [i32; 2] {
    let [l, top, r, b] = bounds;
    let axis = |a: usize, lo: u32, hi: u32| {
        let span = (hi - lo + 1) as f64 * t[a] as f64;
        let f = (tile[a] - lo) as f64 * t[a] as f64
            + pivot[a] * (g as f64 - span)
            + tile_pivot[a] * g as f64;
        f.floor() as i32
    };
    [axis(0, l, r), axis(1, top, b)]
}

/// The pattern variants to try, in [`VARIANTS`] order. `mirror_x` enables
/// 1, `mirror_y` 2, both mirrors 3; `rotate` enables 3, 4 and 5 (the turns),
/// and with any mirror all eight. A variant that samples exactly like an
/// earlier one (the same tests at the same offsets of the same grids, over
/// `match` and `require` together) is dropped, so symmetric patterns never
/// stack turned copies.
fn variants(rule: &MatchSelect, parts: &[Part], grid_w: i32) -> Vec<Variant> {
    let mx = rule.symmetry.contains(&Symmetry::MirrorX);
    let my = rule.symmetry.contains(&Symmetry::MirrorY);
    let rot = rule.symmetry.contains(&Symmetry::Rotate);
    let mirror = mx || my;
    let enabled = [
        true,
        mx || (rot && mirror),
        my || (rot && mirror),
        (mx && my) || rot,
        rot,
        rot,
        rot && mirror,
        rot && mirror,
    ];
    let mut seen: Vec<Vec<(usize, i32, i32, u32)>> = Vec::new();
    let mut out = Vec::new();
    for ((index, bits), on) in VARIANTS.into_iter().zip(enabled) {
        if !on {
            continue;
        }
        let mut key: Vec<(usize, i32, i32, u32)> = parts
            .iter()
            .flat_map(|p| {
                p.cells.iter().map(move |c| {
                    let (x, y) = turn(bits, c.dx, c.dy);
                    (p.grid, x, y, c.table)
                })
            })
            .collect();
        key.sort_unstable();
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        out.push(Variant {
            index,
            bits,
            flip: Flip::from_bits(bits),
            linear: parts
                .iter()
                .map(|p| {
                    p.cells
                        .iter()
                        .map(|c| {
                            let (x, y) = turn(bits, c.dx, c.dy);
                            ((y * grid_w + x) as isize, c.table)
                        })
                        .collect()
                })
                .collect(),
        });
    }
    out
}

impl CompiledRule<'_> {
    /// `modulo` and `checker`, on Euclidean remainders so negative
    /// coordinates (and offsets past the cell) behave like any other. As in
    /// LDtk, a checker axis ignores its own offset: vertical checker staggers
    /// rows by `x / modulo.x` and tests `(y + stagger) % modulo.y`.
    #[inline]
    fn modulo_ok(&self, x: i32, y: i32) -> bool {
        let [mx, my, ox, oy] = self.modulo;
        if mx == 1 && my == 1 {
            // Every cell passes; skip the divisions (the common case).
            return true;
        }
        if mx <= 0 || my <= 0 {
            return false;
        }
        let ok_y = match self.checker {
            Checker::Vertical => (y + x.div_euclid(mx).rem_euclid(2)).rem_euclid(my) == 0,
            _ => (y - oy).rem_euclid(my) == 0,
        };
        let ok_x = match self.checker {
            Checker::Horizontal => (x + y.div_euclid(my).rem_euclid(2)).rem_euclid(mx) == 0,
            _ => (x - ox).rem_euclid(mx) == 0,
        };
        ok_x && ok_y
    }

    #[inline]
    fn matches(&self, grids: &[&[u16]], w: i32, h: i32, x: i32, y: i32, v: &Variant) -> bool {
        let [rx, ry] = self.radius;
        if x >= rx && y >= ry && x + rx < w && y + ry < h {
            let base = (y * w + x) as isize;
            if let [linear] = &v.linear[..] {
                // No `require` (the common case): one grid, one loop.
                let grid = &grids[self.parts[0].grid];
                return linear
                    .iter()
                    .all(|&(o, t)| self.tables[t as usize + grid[(base + o) as usize] as usize]);
            }
            return self.parts.iter().zip(&v.linear).all(|(p, linear)| {
                let grid = &grids[p.grid];
                linear
                    .iter()
                    .all(|&(o, t)| self.tables[t as usize + grid[(base + o) as usize] as usize])
            });
        }
        self.parts.iter().all(|p| {
            let grid = &grids[p.grid];
            p.cells.iter().all(|c| {
                let (dx, dy) = turn(v.bits, c.dx, c.dy);
                let (sx, sy) = (x + dx, y + dy);
                let value = if sx >= 0 && sy >= 0 && sx < w && sy < h {
                    grid[(sy * w + sx) as usize]
                } else {
                    match p.oob {
                        Some(o) => o,
                        None => return false,
                    }
                };
                self.tables[c.table as usize + value as usize]
            })
        })
    }

    /// `chance` then `perlin`, both keyed on the anchor only.
    fn gate(&self, seed: u64, x: i32, y: i32) -> bool {
        let chance = self.chance;
        if chance < 1.0
            && (chance <= 0.0 || unit(hash(seed, self.key, x, y, stream::CHANCE)) >= chance)
        {
            return false;
        }
        match &self.perlin {
            Some((p, scale, octaves)) => {
                p.fractal(x as f64 * scale, y as f64 * scale, *octaves) >= 0.0
            }
            None => true,
        }
    }

    fn place(&self, seed: u64, g: i32, x: i32, y: i32, v: &Variant, bucket: &mut Vec<Raw>) {
        let key = self.key;
        let alt = if self.alts.len() == 1 {
            &self.alts[0]
        } else {
            let mut roll = hash(seed, key, x, y, stream::ALT + v.index) % self.total_weight;
            let mut pick = &self.alts[0];
            for a in &self.alts {
                if roll < a.weight {
                    pick = a;
                    break;
                }
                roll -= a.weight;
            }
            pick
        };
        let mut off = self.offset_px;
        if let Some([rx, ry]) = self.random_offset {
            let draw = |[lo, hi]: [i32; 2], s: u64| {
                let span = (hi as i64 - lo as i64 + 1) as u64;
                (lo as i64 + (hash(seed, key, x, y, s + v.index) % span) as i64) as i32
            };
            off[0] += draw(rx, stream::OFFSET_X);
            off[1] += draw(ry, stream::OFFSET_Y);
        }
        // A turned variant turns the whole layout about the anchor cell's
        // center: each tile's box `[o, o + t)` from the cell's top-left is
        // transposed (x and y swap, the cell is square), then mirrored: on
        // an axis, `[o, o + t)` lands on `[g - o - t, g - o)`. With tiles
        // the size of a cell a mirror is just `-o`, so stamp offsets and
        // pixel offsets flip sign. `rotate` needs square tiles, so a
        // transposed box keeps its size.
        let [tw, th] = self.tile_size;
        let at = |s: [i32; 2]| {
            let (mut ox, mut oy) = (s[0] + off[0], s[1] + off[1]);
            let (mut bw, mut bh) = (tw, th);
            if v.bits & 4 != 0 {
                std::mem::swap(&mut ox, &mut oy);
                std::mem::swap(&mut bw, &mut bh);
            }
            if v.bits & 1 != 0 {
                ox = g - ox - bw;
            }
            if v.bits & 2 != 0 {
                oy = g - oy - bh;
            }
            [x * g + ox, y * g + oy]
        };
        for (tile, s) in alt.tiles.iter().zip(&alt.stamp_px) {
            bucket.push(Raw {
                px: at(*s),
                tile: *tile,
                flip: v.flip,
                anchor: [x, y],
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Baker, auto_def, footprint_axis};

    /// A pass reading the layer's own derived grid gets a copy of that
    /// grid only (with what earlier passes wrote); every other grid it
    /// reads is the level's, in place. A pass reading no own grid copies
    /// nothing.
    #[test]
    fn a_pass_copies_only_its_own_written_grids() {
        use serde_json::json;
        let mut p = worlds_core::Project::new("t");
        p.grid_size = 16;
        p.layers = serde_json::from_value(json!([
            { "slug": "collision", "name": "C", "kind": "int_grid",
              "values": [{ "slug": "solid", "name": "S", "color": "#808080", "glyph": "#" }] },
            { "slug": "deco", "name": "D", "kind": "int_grid",
              "values": [{ "slug": "moss", "name": "M", "color": "#808080", "glyph": "o" }] },
            { "slug": "marks", "name": "M", "kind": "int_grid", "derived": true,
              "values": [{ "slug": "m", "name": "M", "color": "#808080", "glyph": "m" }] },
            { "slug": "auto", "name": "A", "kind": "auto", "source": "collision" }
        ]))
        .unwrap();
        p.tilesets = serde_json::from_value(json!([
            { "slug": "main", "image": "main.png", "tile_size": [16, 16] }
        ]))
        .unwrap();
        p.rules = vec![
            serde_json::from_value(json!({ "layer": "auto", "groups": [
                { "slug": "w", "rules": [
                    { "slug": "w", "match": { "rows": ["#"] }, "write": { "grid": "marks", "value": "m" } } ] },
                { "slug": "d", "pass": 1, "rules": [
                    { "slug": "d", "match": { "grid": "marks", "rows": ["m"] },
                      "require": [{ "grid": "deco", "rows": ["*"] }],
                      "out": { "tileset": "main", "tiles": [[0, 0]] } } ] } ] }))
            .unwrap(),
        ];
        let def = p.layer("collision").unwrap();
        let g = worlds_core::IntGridLayer::from_rows(def, &["#.#"]).unwrap();
        let mut l = worlds_core::Level::new(worlds_core::slug("room"), "Room", g.size());
        l.int_grids = vec![g];
        let baker = Baker::new(&p, &l, auto_def(&p, "auto").unwrap(), &Default::default()).unwrap();
        let marks = baker.grid_slugs.iter().position(|s| s == "marks").unwrap();
        let computed = vec![vec![1, 0, 1]];
        assert!(baker.pass_overlay(0, &computed).is_empty());
        let ov = baker.pass_overlay(1, &computed);
        assert_eq!(ov, vec![(marks, vec![1, 0, 1])]);
        let view = baker.grid_view(&ov);
        assert_eq!(view.len(), baker.grids.len());
        assert!(baker.grids.len() >= 3, "collision, deco and marks");
        for (gi, g) in view.iter().enumerate() {
            if gi == marks {
                assert_eq!(*g, &[1, 0, 1][..]);
            } else {
                assert!(
                    std::ptr::eq(g.as_ptr(), baker.grids[gi].as_ptr()),
                    "grid {gi} copied"
                );
            }
        }
    }

    #[test]
    fn footprint_formula() {
        // 64 px stamp, pivot [0, 1] at anchor x on a 32 px grid: x, x+1.
        assert_eq!(footprint_axis(32 * 5, 64, 32, 100), [5, 7]);
        // Pivot 0.5: box [32x - 16, 32x + 48): x-1, x.
        assert_eq!(footprint_axis(32 * 5 - 16, 64, 32, 100), [4, 6]);
        // 32 px tile on a 16 px grid at 0: centers 8 and 24.
        assert_eq!(footprint_axis(0, 32, 16, 100), [0, 2]);
        // 16 px tile on a 32 px grid at 0: misses the center 16.
        assert_eq!(footprint_axis(0, 16, 32, 100), [0, 0]);
        // ... at 8: covers it.
        assert_eq!(footprint_axis(8, 16, 32, 100), [0, 1]);
        // Clipped to the level.
        assert_eq!(footprint_axis(-64, 128, 32, 100), [0, 2]);
        assert_eq!(footprint_axis(32 * 99, 64, 32, 100), [99, 100]);
        // Odd grid: exact.
        assert_eq!(footprint_axis(0, 7, 7, 10), [0, 1]);
        assert_eq!(footprint_axis(4, 7, 7, 10), [1, 2]);
    }
}
