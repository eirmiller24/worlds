//! Incremental baking: keep the last bake of every `auto` layer and redo
//! only the anchors an edit can have changed.
//!
//! Why a partial re-sweep is exact: whether a rule fires at an anchor, what
//! it places and whether it locks the cell depend only on the grid cells
//! within the rule's pattern radius of that anchor, on the anchor itself
//! (modulo, chance, perlin and every random roll are keyed on it), and on
//! earlier rules of the same pass at the same anchor (the break rule; the
//! first write wins). So after an edit to a rect of cells, only anchors
//! within the largest pattern radius of it can change. Those are re-swept
//! into fresh buckets with [`Baker::sweep`] and spliced over the cached
//! placements anchored there, rule by rule; the rest of the bake is reused
//! as is. `tests/incremental.rs` checks this against full bakes over random
//! edit sequences.
//!
//! Layers using passes, regions or occupancy (several passes, region rules, `write`,
//! occupancy) go pass by pass ([`BakeCache`]'s docs and the crate README
//! have the rules): a region rule's candidates come from a flood fill of the
//! whole level, recomputed when its grid changed and diffed; a pass using
//! occupancy is re-swept whole when anything in it may change; writes that
//! changed are inputs to later passes and, through the effective derived
//! grids, to the layers reading them. Layers without them keep the plain
//! re-sweep, bit for bit.
//!
//! A partial re-sweep is only exact if the compiled rules are the ones the
//! cached bake came from. Every entry keeps the [`rules_fingerprint`] it was
//! baked under; a re-sweep under a different one becomes a full rebake, so
//! even a rule edit nobody reported can't leave a stale half-bake behind.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use worlds_core::command::{Change, DefKind, LevelPart, ProjectKey};
use worlds_core::{IntGridLayer, Level, Project, Slug};

use crate::bake::{Baker, auto_def, overlay};
use crate::derived::{self, Effective};
use crate::{Error, LayerBake, Placement, Result, TilesetImages, region};

/// Cells of a layer's grid that may look different after a rebake:
/// `[min, max)`. Covers every re-swept anchor and every tile drawn or
/// removed there, so it can reach past the level's edges when stamps or
/// offsets overhang. For a derived layer: the cells whose effective value
/// may have changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirtyRect {
    pub min: [i32; 2],
    /// Exclusive.
    pub max: [i32; 2],
}

impl DirtyRect {
    pub fn is_empty(&self) -> bool {
        self.max[0] <= self.min[0] || self.max[1] <= self.min[1]
    }

    fn union(self, o: DirtyRect) -> DirtyRect {
        if self.is_empty() {
            return o;
        }
        if o.is_empty() {
            return self;
        }
        DirtyRect {
            min: [self.min[0].min(o.min[0]), self.min[1].min(o.min[1])],
            max: [self.max[0].max(o.max[0]), self.max[1].max(o.max[1])],
        }
    }

    const EMPTY: DirtyRect = DirtyRect {
        min: [0, 0],
        max: [0, 0],
    };

    fn of_size(size: [i32; 2]) -> DirtyRect {
        DirtyRect {
            min: [0, 0],
            max: size,
        }
    }

    fn cell(x: i32, y: i32) -> DirtyRect {
        DirtyRect {
            min: [x, y],
            max: [x + 1, y + 1],
        }
    }

    fn contains(&self, a: [i32; 2]) -> bool {
        a[0] >= self.min[0] && a[0] < self.max[0] && a[1] >= self.min[1] && a[1] < self.max[1]
    }

    /// Grown by `r` cells each side, clipped to `[0, size)`.
    fn grow(self, r: [i32; 2], size: [i32; 2]) -> DirtyRect {
        DirtyRect {
            min: [(self.min[0] - r[0]).max(0), (self.min[1] - r[1]).max(0)],
            max: [
                (self.max[0] + r[0]).min(size[0]),
                (self.max[1] + r[1]).min(size[1]),
            ],
        }
    }
}

fn union_opt(a: Option<DirtyRect>, b: Option<DirtyRect>) -> Option<DirtyRect> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.union(b)),
        (a, None) => a,
        (None, b) => b,
    }
}

/// Per-pass state of a layer using passes, regions or occupancy, for its incremental bake.
#[derive(Clone)]
struct Staged {
    /// Per rule: the anchors where it fired, row-major.
    firings: Vec<Vec<[i32; 2]>>,
    /// Per pass, per written grid: what the pass wrote (0 = nothing).
    writes: Vec<Vec<Vec<u16>>>,
    /// Per rule: a region rule's candidates, row-major.
    candidates: Vec<Vec<[i32; 2]>>,
    /// Per pass: the occupancy after it (occupancy passes only).
    occ_after: Vec<Option<Vec<bool>>>,
}

/// One cached layer bake.
#[derive(Clone)]
struct Cached {
    /// Shared with whoever displays it ([`BakeCache::get_shared`]); a
    /// re-sweep copies it only if someone still holds the old one.
    bake: Arc<LayerBake>,
    /// Placements per compiled rule, in rule order (0 for a `write` rule).
    /// The placements list is those runs in draw order, each sorted by
    /// anchor row-major: that is the per-cell index.
    lens: Vec<usize>,
    /// Rule indices in draw order (passes bottom to top, within a pass the
    /// last rule first).
    draw: Vec<usize>,
    /// `(group, expanded rule slug)` of the compiled rules the bake came
    /// from, in rule order: the runs of `lens`.
    rules: Vec<(Slug, Slug)>,
    /// [`rules_fingerprint`] of the layer when it was baked.
    fingerprint: u64,
    /// [`level_values_hash`] of the level when it was baked.
    values: u64,
    size: [i32; 2],
    /// For a layer using passes, regions or occupancy.
    staged: Option<Box<Staged>>,
}

/// Everything cached for one level.
#[derive(Default, Clone)]
struct LevelCache {
    layers: BTreeMap<Slug, Cached>,
    /// Effective grids of the derived layers (overrides over the writer's
    /// computed values).
    derived: Effective,
}

/// A hash of what a layer's bake reads from the level besides its grids:
/// the stored values of the level fields its groups require, and the
/// level's tags when a group has `requires_level_tag`. With the
/// fingerprint (which covers the definitions), everything about level
/// fields and tags a bake reads.
fn level_values_hash(project: &Project, level: &Level, layer: &str) -> u64 {
    let mut h = Fnv(0xcbf2_9ce4_8422_2325);
    if let Some(set) = project.rules_for(layer) {
        for f in level_fields_read(set) {
            h.put(f);
            h.put(&level.fields.get(f));
        }
        if uses_tags(set) {
            h.put(&level.tags);
        }
    }
    h.0
}

/// Whether some group of the rule set has a `requires_level_tag`.
fn uses_tags(set: &worlds_core::RuleSet) -> bool {
    set.groups.iter().any(|g| g.requires_level_tag.is_some())
}

/// A hash of everything a layer's bake depends on besides the level: its
/// rules file, `rules/macros.json` if the rules use it, the layer's own
/// definition, the int_grid layers its rules read (values, groups; derived
/// ones included) and write, the tilesets it can draw from (tile sizes;
/// with any level's substitution for the layer), the project grid size, the
/// definitions of the level fields its groups' `requires_level_field` name
/// (a requirement falls back to the field's default), and the fingerprints
/// of the auto layers writing the derived grids it reads (worked out with a
/// visited set, so a cycle never recurses: a layer on a cycle covers only
/// its own inputs there, and it fails to bake anyway). Other level fields
/// don't change it.
/// Equal fingerprints mean the same compiled rules, so a cached bake can be
/// patched instead of rebaked; a definition the layer doesn't read or draw
/// from doesn't change it. It is FNV-1a over those definitions' JSON, a few
/// microseconds for a 50-rule layer.
pub fn rules_fingerprint(project: &Project, layer: &str) -> u64 {
    // One layer: nothing to keep (no memo, so no cycle check either).
    fingerprint_in(
        project,
        layer,
        &mut BTreeSet::new(),
        &mut HashMap::new(),
        None,
    )
}

/// [`rules_fingerprint`]s worked out at most once per layer within one
/// cache call (a reader's fingerprint covers its writers', so without this
/// a chain of readers hashes the first writer's definitions once per layer
/// below it). Layers on a derived-grid cycle (or depending on one) aren't
/// kept: theirs depend on where the walk entered the cycle.
struct Fingerprints<'p> {
    project: &'p Project,
    memo: HashMap<Slug, u64>,
    cyclic: BTreeSet<Slug>,
}

impl<'p> Fingerprints<'p> {
    fn new(project: &'p Project) -> Self {
        let cyclic = if project.layers.iter().any(|l| l.is_derived()) {
            derived::order(project)
                .1
                .into_iter()
                .map(|(l, _)| l)
                .collect()
        } else {
            BTreeSet::new()
        };
        Self::with_cyclic(project, cyclic)
    }

    fn with_cyclic(project: &'p Project, cyclic: BTreeSet<Slug>) -> Self {
        Fingerprints {
            project,
            memo: HashMap::new(),
            cyclic,
        }
    }

    fn get(&mut self, layer: &str) -> u64 {
        fingerprint_in(
            self.project,
            layer,
            &mut BTreeSet::new(),
            &mut self.memo,
            Some(&self.cyclic),
        )
    }
}

#[cfg(test)]
thread_local! {
    /// How many fingerprints were worked out (not taken from a memo).
    static FINGERPRINTS_HASHED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn fingerprint_in(
    project: &Project,
    layer: &str,
    visited: &mut BTreeSet<String>,
    memo: &mut HashMap<Slug, u64>,
    // `None`: keep nothing in `memo`.
    cyclic: Option<&BTreeSet<Slug>>,
) -> u64 {
    if let Some(&f) = memo.get(layer) {
        return f;
    }
    #[cfg(test)]
    FINGERPRINTS_HASHED.with(|n| n.set(n.get() + 1));
    visited.insert(layer.to_string());
    let mut h = Fnv(0xcbf2_9ce4_8422_2325);
    h.put(&project.grid_size);
    let def = project.layer(layer);
    h.put(&def);
    match project.rules_for(layer) {
        Some(set) => {
            h.put(set);
            for f in level_fields_read(set) {
                h.put(&f);
                // The one the bake looks up: the first with that slug.
                h.put(&project.level_fields.iter().find(|d| d.slug == *f));
            }
            if set.uses_macros() {
                h.put(&project.macros);
            }
            let source = def.and_then(|d| d.auto()).map(|a| &a.source);
            let (grids, tilesets) = reads_and_draws(project, set, source);
            for g in grids {
                h.put(&g);
                h.put(&project.layer(g.as_str()));
            }
            for t in tilesets {
                h.put(&t);
                h.put(&project.tileset(t.as_str()));
            }
            let any_derived = project.layers.iter().any(|l| l.is_derived());
            for g in any_derived
                .then(|| project.layer_writes(layer))
                .into_iter()
                .flatten()
            {
                h.put(&g);
                h.put(&project.layer(g.as_str()));
                // Who else writes it (a second writer fails the bake).
                h.put(&project.derived_writers(g.as_str()));
            }
            let deps = any_derived.then(|| project.layer_dependencies(layer));
            for (g, w) in deps.into_iter().flatten() {
                h.put(&g);
                h.put(&w);
                if !visited.contains(w.as_str()) {
                    h.put(&fingerprint_in(project, w.as_str(), visited, memo, cyclic));
                }
            }
        }
        None => h.put(&()),
    }
    let f = crate::hash::mix(h.0);
    // Off a cycle, nothing upstream of `layer` is in `visited` (that would
    // be a cycle through it), so the value doesn't depend on the walk.
    if let Some(cyclic) = cyclic
        && let Ok(slug) = Slug::new(layer)
        && !cyclic.contains(&slug)
    {
        memo.insert(slug, f);
    }
    f
}

/// The level fields a rule set's groups require (`requires_level_field`).
fn level_fields_read(set: &worlds_core::RuleSet) -> BTreeSet<&Slug> {
    set.groups
        .iter()
        .filter_map(|g| g.requires_level_field.as_ref())
        .map(|r| &r.field)
        .collect()
}

/// Whether an auto layer's bake can depend on a level field: some group of
/// its rule set has a `requires_level_field`.
fn reads_level_fields(project: &Project, layer: &str) -> bool {
    project
        .rules_for(layer)
        .is_some_and(|set| !level_fields_read(set).is_empty())
}

/// The grids a rule set's patterns and regions name (and the layer's
/// `source`) and the tilesets it names (outputs, stamps, references, fixed
/// template rules, and both sides of every level's substitution for the
/// layer), without expanding anything.
fn reads_and_draws(
    project: &Project,
    set: &worlds_core::RuleSet,
    source: Option<&Slug>,
) -> (BTreeSet<Slug>, BTreeSet<Slug>) {
    use worlds_core::{RuleEntry, TemplateOutput};
    let mut grids: BTreeSet<Slug> = source.into_iter().cloned().collect();
    let mut tilesets: BTreeSet<Slug> = set.stamps.iter().map(|s| s.tileset.clone()).collect();
    let mut pattern = |p: &worlds_core::Pattern| {
        if let Some(g) = &p.grid {
            grids.insert(g.clone());
        }
    };
    for (_, e) in set.entries() {
        match e {
            RuleEntry::Rule(r) => {
                r.patterns().for_each(&mut pattern);
                if let Some(g) = r.as_region() {
                    pattern(&worlds_core::Pattern {
                        grid: g.region.grid.clone(),
                        ..worlds_core::Pattern::new(&[])
                    });
                }
                if let Some(t) = r.out().and_then(|o| o.as_tiles()) {
                    tilesets.insert(t.tileset.clone());
                }
            }
            RuleEntry::Use(u) => {
                tilesets.extend(u.tileset.iter().cloned());
                let Some(t) = project.template(u.template.as_str()) else {
                    continue;
                };
                for r in &t.rules {
                    r.patterns().for_each(&mut pattern);
                    if let TemplateOutput::Tiles(o) = &r.out {
                        tilesets.extend(o.tileset.iter().cloned());
                    }
                }
            }
        }
    }
    for l in project.levels() {
        if let Some(st) = l.layers.get(set.layer.as_str()) {
            for (a, b) in &st.tilesets {
                tilesets.insert(a.clone());
                tilesets.insert(b.clone());
            }
        }
    }
    (grids, tilesets)
}

/// FNV-1a as an `io::Write`, so serde_json can stream into it.
struct Fnv(u64);

impl std::io::Write for Fnv {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        for &b in buf {
            self.0 ^= b as u64;
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Fnv {
    /// Hash `v`'s JSON and a separator. A value that can't serialize
    /// still hashes what was written before the error.
    fn put(&mut self, v: &impl serde::Serialize) {
        let _ = serde_json::to_writer(&mut *self, v);
        let _ = std::io::Write::write(self, &[0xff]);
    }
}

/// One level's bakes made away from the cache (a worker thread's
/// [`BakeCache::bake_detached`]), to hand to [`BakeCache::seed_level`].
pub struct LevelSeed {
    level: Slug,
    cache: LevelCache,
    results: Vec<(Slug, Result<DirtyRect>)>,
}

impl LevelSeed {
    /// The level it is for.
    pub fn level(&self) -> &Slug {
        &self.level
    }

    /// Each auto layer's bake (layers that failed are missing).
    pub fn bakes(&self) -> impl Iterator<Item = (&Slug, &Arc<LayerBake>)> {
        self.cache.layers.iter().map(|(l, c)| (l, &c.bake))
    }

    /// The effective grid of each derived layer.
    pub fn derived(&self) -> impl Iterator<Item = (&Slug, &Arc<IntGridLayer>)> {
        self.cache.derived.iter()
    }

    /// Each auto layer's outcome, in bake order; take them before seeding.
    pub fn take_results(&mut self) -> Vec<(Slug, Result<DirtyRect>)> {
        std::mem::take(&mut self.results)
    }
}

/// The last bake of each (level, auto layer), updated from [`Change`]s.
///
/// ```no_run
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// # let mut project = worlds_core::load("mygame.worlds")?;
/// # let images = worlds_rules::TilesetImages::new("mygame.worlds", &project.tilesets);
/// # let cmd = worlds_core::Command::noop();
/// let mut cache = worlds_rules::BakeCache::new();
/// cache.bake_level(&project, "crevasse", &images)?;
/// let applied = project.apply(cmd)?;
/// for (layer, rect) in cache.apply_changes(&project, "crevasse", &images, &applied.changes)? {
///     // re-render `layer` over `rect` (an auto layer, or a derived layer
///     // whose effective values changed)
/// #   let _ = (layer, rect);
/// }
/// # Ok(()) }
/// ```
#[derive(Default)]
pub struct BakeCache {
    levels: HashMap<Slug, LevelCache>,
    /// Per auto layer: the grids its rules read (`match`, `require` and
    /// `region` of every expanded rule, and its `source`), for the
    /// fingerprint they were worked out under. `None`: the rules don't
    /// expand, so any grid counts.
    reads: HashMap<Slug, (u64, Option<BTreeSet<Slug>>)>,
}

/// What a layer needs after a list of changes.
enum Need {
    Full,
    /// Changed cells per grid it reads.
    Cells(BTreeMap<Slug, DirtyRect>),
}

impl BakeCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// The cached bake of a layer.
    pub fn get(&self, level: &str, layer: &str) -> Option<&LayerBake> {
        self.get_shared(level, layer).map(|b| &**b)
    }

    /// The cached bake of a layer, to share without copying. Drop the clone
    /// before the next [`Self::apply_changes`] that re-sweeps the layer, or
    /// that re-sweep has to copy the whole bake first.
    pub fn get_shared(&self, level: &str, layer: &str) -> Option<&Arc<LayerBake>> {
        self.levels.get(level)?.layers.get(layer).map(|c| &c.bake)
    }

    /// The **effective** grid of derived layer `layer` in `level` (overrides
    /// over what its writer computed; the overrides alone when no layer
    /// writes it), as of the last bake or update of the level. `None` when
    /// the level isn't cached or the writer's bake failed.
    pub fn derived(&self, level: &str, layer: &str) -> Option<Arc<IntGridLayer>> {
        self.levels.get(level)?.derived.get(layer).cloned()
    }

    /// The occupancy grid of auto layer `layer` after its last pass,
    /// row-major in cells of its grid ([`LayerBake::occupied`]; empty when
    /// the layer uses no occupancy).
    pub fn occupied(&self, level: &str, layer: &str) -> Option<&[bool]> {
        self.get(level, layer).map(|b| &b.occupied[..])
    }

    /// The effective grid of derived layer `layer` in `level`
    /// ([`Self::derived`]), baking into the cache only what it needs when
    /// it isn't there: its writer and the writers that one depends on, those
    /// not cached yet. `Ok(None)` when `layer` isn't a derived layer; an
    /// error when its writer (or one it depends on) fails to bake, which
    /// is left uncached, as by [`Self::bake_level`].
    pub fn bake_derived(
        &mut self,
        project: &Project,
        level: &str,
        layer: &str,
        images: &TilesetImages,
    ) -> Result<Option<Arc<IntGridLayer>>> {
        if let Some(g) = self.derived(level, layer) {
            return Ok(Some(g));
        }
        let lvl = project
            .level(level)
            .ok_or_else(|| Error::UnknownLevel(level.to_string()))?;
        let Some(def) = project.layer(layer).filter(|l| l.is_derived()) else {
            return Ok(None);
        };
        let mut first_err = None;
        if let Some(w) = project.derived_writer(layer).cloned() {
            let mut need = derived::upstream_layers(project, w.as_str())?;
            need.push(w);
            for l in need {
                let cached = self
                    .levels
                    .get(level)
                    .is_some_and(|c| c.layers.contains_key(&l));
                if cached {
                    continue;
                }
                let fp = rules_fingerprint(project, l.as_str());
                let r = self.full(project, lvl, &l, images, fp);
                if let Err(e) = self.settle(level, &l, r) {
                    first_err.get_or_insert(e);
                }
                for g in project.layer_writes(l.as_str()) {
                    self.refresh_effective(project, lvl, &g);
                }
            }
        }
        self.refresh_effective(project, lvl, &def.slug);
        match (self.derived(level, layer), first_err) {
            (Some(g), _) => Ok(Some(g)),
            (None, Some(e)) => Err(e),
            (None, None) => Err(Error::UpstreamFailed {
                layer: layer.to_string(),
                upstream: project
                    .derived_writer(layer)
                    .map_or_else(String::new, |w| w.to_string()),
                reason: "its writer didn't bake".into(),
            }),
        }
    }

    /// Cache a full bake of `layer` made elsewhere (a worker thread's
    /// [`crate::bake_layer`] of the same project state), instead of baking
    /// it again. Returns false, caching nothing, if `bake` doesn't fit the
    /// layer's current compiled rules (by expanded slug) and size. For a
    /// layer using passes, regions or occupancy (passes, region rules, writes, occupancy)
    /// the per-pass state is rebuilt from the bake (its `fired`, `derived`,
    /// `occupied` and placements; no tile is baked again), and a bake that
    /// doesn't fit it is refused too. A layer reading derived grids needs
    /// their writers cached first (else false). The bake must come from the
    /// project as it is now: it is recorded under the current
    /// [`rules_fingerprint`].
    pub fn seed(
        &mut self,
        project: &Project,
        level: &str,
        layer: &str,
        bake: Arc<LayerBake>,
    ) -> Result<bool> {
        let lvl = project
            .level(level)
            .ok_or_else(|| Error::UnknownLevel(level.to_string()))?;
        let def = auto_def(project, layer)?;
        let fingerprint = rules_fingerprint(project, layer);
        let values = level_values_hash(project, lvl, layer);
        let cached = if project.rules_for(layer).is_none() {
            if !bake.placements.is_empty() {
                return Ok(false);
            }
            let size = project.layer_cells(def, lvl);
            Cached {
                bake,
                lens: Vec::new(),
                draw: Vec::new(),
                rules: Vec::new(),
                fingerprint,
                values,
                size: [size[0] as i32, size[1] as i32],
                staged: None,
            }
        } else {
            let upstream = self
                .levels
                .get(level)
                .map(|l| l.derived.clone())
                .unwrap_or_default();
            let baker = match Baker::new(project, lvl, def, &upstream) {
                Ok(b) => b,
                Err(Error::UpstreamFailed { .. }) => return Ok(false),
                Err(e) => return Err(e),
            };
            let staged = if baker.is_staged() {
                match baker.rebuild(&bake) {
                    Some(r) => Some(Box::new(Staged {
                        firings: r.firings,
                        writes: r.writes,
                        candidates: r.candidates,
                        occ_after: r.occ_after,
                    })),
                    None => return Ok(false),
                }
            } else {
                None
            };
            let rules = baker.rule_keys();
            let draw = baker.draw_order();
            // The placements are each rule's run, in draw order.
            let mut lens = vec![0; rules.len()];
            let mut rest = &bake.placements[..];
            for &ri in &draw {
                let (group, rule) = &rules[ri];
                let n = rest
                    .iter()
                    .take_while(|p| p.rule == *rule && p.group == *group)
                    .count();
                lens[ri] = n;
                rest = &rest[n..];
            }
            if !rest.is_empty() {
                return Ok(false);
            }
            Cached {
                bake,
                lens,
                draw,
                rules,
                fingerprint,
                values,
                size: baker.size(),
                staged,
            }
        };
        self.levels
            .entry(lvl.slug.clone())
            .or_default()
            .layers
            .insert(def.slug.clone(), cached);
        // The derived grids it writes (and those nobody writes) now have
        // an effective grid, for the layers reading them.
        for d in project.layers.iter().filter(|l| l.is_derived()) {
            let writer = project.derived_writer(d.slug.as_str());
            if writer.is_none() || writer == Some(&def.slug) {
                self.refresh_effective(project, lvl, &d.slug);
            }
        }
        Ok(true)
    }

    /// Bake every auto layer of a level from scratch, away from any cache
    /// (on a worker thread), for [`Self::seed_level`].
    pub fn bake_detached(
        project: &Project,
        level: &str,
        images: &TilesetImages,
    ) -> Result<LevelSeed> {
        let mut c = BakeCache::new();
        let results = c.bake_level_each(project, level, images)?;
        let lvl = project.level(level).expect("baked above");
        Ok(LevelSeed {
            level: lvl.slug.clone(),
            cache: c.levels.remove(level).unwrap_or_default(),
            results,
        })
    }

    /// Cache a level baked by [`Self::bake_detached`] from the project as it
    /// is now, replacing whatever was cached for it. Layers whose rules
    /// changed since (by [`rules_fingerprint`], level fields and tags) are
    /// left out; returns true when every layer of the seed was taken.
    ///
    /// The effective derived grids are worked out again from the overrides
    /// as they are now (an override painted after the worker baked, on a
    /// grid no layer reads, still shows). When one of them changed and some
    /// auto layer reads it, the seed's bakes are older than the grids and
    /// the level is dropped instead (false).
    pub fn seed_level(&mut self, project: &Project, seed: LevelSeed) -> bool {
        let Some(lvl) = project.level(seed.level.as_str()) else {
            return false;
        };
        let mut all = true;
        let mut cache = seed.cache;
        let mut fps = Fingerprints::new(project);
        cache.layers.retain(|layer, c| {
            let fits = c.fingerprint == fps.get(layer.as_str())
                && c.values == level_values_hash(project, lvl, layer.as_str());
            all &= fits;
            fits
        });
        if !all {
            // A layer left out may feed derived grids: drop them all so
            // the next update rebakes what it needs.
            cache.derived.clear();
            cache.layers.clear();
        }
        self.levels.insert(lvl.slug.clone(), cache);
        if !all {
            return false;
        }
        let mut stale = false;
        for d in project.layers.iter().filter(|l| l.is_derived()) {
            let changed = self.refresh_effective(project, lvl, &d.slug).is_some();
            stale |= changed
                && project
                    .layers
                    .iter()
                    .filter(|l| l.is_auto())
                    .any(|l| project.layer_reads(l.slug.as_str()).contains(&d.slug));
        }
        if stale {
            self.levels.remove(lvl.slug.as_str());
            return false;
        }
        true
    }

    /// The levels with at least one cached layer or effective derived
    /// grid, by slug: what [`Self::apply_changes`] has to keep current.
    pub fn cached_levels(&self) -> Vec<Slug> {
        let mut out: Vec<Slug> = self
            .levels
            .iter()
            .filter(|(_, l)| !l.layers.is_empty() || !l.derived.is_empty())
            .map(|(k, _)| k.clone())
            .collect();
        out.sort();
        out
    }

    /// The cached placements anchored at a cell, in draw order.
    pub fn placements_at(&self, level: &str, layer: &str, anchor: [i32; 2]) -> Vec<&Placement> {
        let Some(c) = self.levels.get(level).and_then(|l| l.layers.get(layer)) else {
            return Vec::new();
        };
        let key = |a: [i32; 2]| (a[1], a[0]);
        let mut out = Vec::new();
        let mut start = 0;
        for &ri in &c.draw {
            let len = c.lens[ri];
            let run = &c.bake.placements[start..start + len];
            let lo = run.partition_point(|p| key(p.anchor) < key(anchor));
            let hi = run.partition_point(|p| key(p.anchor) <= key(anchor));
            out.extend(&run[lo..hi]);
            start += len;
        }
        out
    }

    /// Forget every layer of a level.
    pub fn invalidate(&mut self, level: &str) {
        self.levels.remove(level);
    }

    /// Forget one layer of every level (and the derived grids it writes).
    pub fn invalidate_layer(&mut self, layer: &str) {
        for l in self.levels.values_mut() {
            drop_layer(l, layer);
        }
    }

    /// Forget one layer of one level (say, before handing its rebake to a
    /// worker and [`Self::seed`]ing the result back).
    pub fn invalidate_level_layer(&mut self, level: &str, layer: &str) {
        if let Some(l) = self.levels.get_mut(level) {
            drop_layer(l, layer);
        }
    }

    pub fn clear(&mut self) {
        self.levels.clear();
        self.reads.clear();
    }

    /// Bake every `auto` layer of a level from scratch and cache it, in
    /// dependency order. A layer that fails to bake doesn't stop the others
    /// (except the layers reading its derived grids, which fail too): it is
    /// left uncached, and the first such error is returned once every layer
    /// was tried.
    pub fn bake_level(
        &mut self,
        project: &Project,
        level: &str,
        images: &TilesetImages,
    ) -> Result<Vec<(Slug, DirtyRect)>> {
        first_error(self.bake_level_each(project, level, images)?)
    }

    fn bake_level_each(
        &mut self,
        project: &Project,
        level: &str,
        images: &TilesetImages,
    ) -> Result<Vec<(Slug, Result<DirtyRect>)>> {
        let lvl = project
            .level(level)
            .ok_or_else(|| Error::UnknownLevel(level.to_string()))?;
        let (order, cyclic) = derived::order(project);
        let mut fps =
            Fingerprints::with_cyclic(project, cyclic.iter().map(|(l, _)| l.clone()).collect());
        let mut out = Vec::new();
        for layer in order {
            let fp = fps.get(layer.as_str());
            let r = self.full(project, lvl, &layer, images, fp);
            let r = self.settle(level, &layer, r);
            out.push((layer.clone(), r.map(|x| x.rect)));
            for g in project.layer_writes(layer.as_str()) {
                self.refresh_effective(project, lvl, &g);
            }
        }
        for (layer, cycle) in cyclic {
            self.invalidate_level_layer(level, layer.as_str());
            out.push((layer, Err(Error::DerivedCycle(cycle))));
        }
        for def in project.layers.iter().filter(|l| l.is_derived()) {
            self.refresh_effective(project, lvl, &def.slug);
        }
        Ok(out)
    }

    /// A failed rebake leaves nothing cached for the layer (never the bake
    /// from before the change).
    fn settle<T>(&mut self, level: &str, layer: &Slug, r: Result<T>) -> Result<T> {
        if r.is_err() {
            self.invalidate_level_layer(level, layer.as_str());
        }
        r
    }

    /// Recompute derived layer `d`'s effective grid in `level` from the
    /// cache, and say where it changed (`None`: nowhere). A derived layer
    /// whose writer isn't cached has no effective grid.
    fn refresh_effective(
        &mut self,
        project: &Project,
        level: &Level,
        d: &Slug,
    ) -> Option<DirtyRect> {
        let def = project.layer(d.as_str()).filter(|l| l.is_derived())?;
        let lc = self.levels.entry(level.slug.clone()).or_default();
        let new = match project.derived_writer(d.as_str()) {
            None => Some(derived::effective(project, level, def, None)),
            Some(w) => lc
                .layers
                .get(w.as_str())
                .map(|c| derived::effective(project, level, def, c.bake.derived.get(d))),
        };
        let old = lc.derived.get(d).cloned();
        match new {
            None => {
                lc.derived.remove(d);
                old.map(|o| grid_rect(&o))
            }
            Some(new) => {
                let rect = match &old {
                    Some(o) => diff_grids(o, &new),
                    None => Some(grid_rect(&new)),
                };
                if rect.is_some() || old.is_none() {
                    lc.derived.insert(d.clone(), Arc::new(new));
                }
                rect
            }
        }
    }

    /// Bring a level's cached bakes up to date after `changes` (from one or
    /// more [`Project::apply`] calls, in order) and say what to redraw.
    ///
    /// Auto layers are processed in dependency order (writers of derived
    /// grids before their readers).
    ///
    /// - `Grid` on a layer some auto layer's rules read (`match`, `require`
    ///   or `region`, templates expanded): that auto layer is re-swept over
    ///   the changed rect grown by its largest pattern radius (all grid
    ///   changes of one layer are merged into their bounding rect). A layer
    ///   using passes, regions or occupancy goes pass by pass (crate README, "Incremental
    ///   baking"). On a derived layer a `grid` change is an override edit:
    ///   it changes the effective grid where it changed (reported as that
    ///   derived layer's rect), which its readers re-sweep, and the writer
    ///   re-sweeps it when it reads the grid itself;
    /// - when a writer's computed values change, the derived grid's
    ///   effective cells that changed are reported and are an input of the
    ///   layers reading it;
    /// - `LayerState` of an auto layer, a `LevelMeta` part that affects
    ///   bakes (size, everything), a new level, or a new project seed: that
    ///   layer, or all of them, are rebaked in full;
    /// - `LevelMeta` part `fields` (the level's values) and `project` key
    ///   `level_fields` (the definitions, for every level): a layer with a
    ///   group that has a `requires_level_field` is rebaked in full when the
    ///   value or the definition of a field it requires changed; other
    ///   layers can't read level fields and are left alone;
    /// - `LevelMeta` part `tags`: the layers with a group that has a
    ///   `requires_level_tag`, and the layers depending on them, are
    ///   rebaked in full;
    /// - definitions (no level of their own, so they count for every
    ///   level), each rebaking in full the layers named here **and every
    ///   layer reading their derived grids, transitively**: `def` kind
    ///   `rules` with slug L: layer L; `macros`: every layer whose rules use
    ///   `rules/macros.json` (a template reference or a named pattern or
    ///   legend); `tileset` T: every layer that can draw from T
    ///   ([`Project::auto_layers_drawing_from`]); `layer` X: X itself, the
    ///   layers reading X and, for a derived X, its writer. The derived
    ///   grids the cached bakes of those layers (or of a removed layer)
    ///   wrote are worked out again from their writer now (or their
    ///   overrides alone), so a writer that stopped writing a grid leaves
    ///   no stale reader. `def_renamed`
    ///   isn't handled here (call [`Self::invalidate`] or [`Self::clear`]),
    ///   but a re-sweep never patches a bake made under other definitions:
    ///   see [`rules_fingerprint`];
    /// - `LevelRenamed` moves the cache entries (the bake keys on the
    ///   level's id, not its slug); `LevelDeleted` drops them;
    /// - anything else (tiles layers, entities, notes, position) can't
    ///   change a bake and is ignored.
    ///
    /// `level` is the slug after the changes. Changes made while the level
    /// had an earlier slug (before a rename in `changes`) count as its own.
    /// A layer with nothing cached is baked in full, but only if some change
    /// can affect it; otherwise it stays uncached and isn't reported (a
    /// layer reading derived grids first bakes uncached writers it needs).
    ///
    /// A layer whose rebake fails doesn't stop the others, and is left
    /// uncached (never with its bake from before the changes); the first
    /// error is returned once every layer was brought up to date. Use
    /// [`Self::apply_changes_per_layer`] to get each layer's outcome.
    ///
    /// The result lists each auto layer that was rebaked with the rect to
    /// redraw, then each derived layer whose effective values changed with
    /// the rect of those cells.
    pub fn apply_changes(
        &mut self,
        project: &Project,
        level: &str,
        images: &TilesetImages,
        changes: &[Change],
    ) -> Result<Vec<(Slug, DirtyRect)>> {
        first_error(self.apply_changes_per_layer(project, level, images, changes))
    }

    /// [`Self::apply_changes`], with the outcome of every layer it rebaked:
    /// the rect to redraw, or why the rebake failed (the layer is then
    /// uncached).
    pub fn apply_changes_per_layer(
        &mut self,
        project: &Project,
        level: &str,
        images: &TilesetImages,
        changes: &[Change],
    ) -> Vec<(Slug, Result<DirtyRect>)> {
        let ours = ours(changes, level);
        for (c, _) in changes.iter().zip(&ours).filter(|(_, o)| **o) {
            match c {
                Change::LevelRenamed { from, to, .. } => {
                    if let Some(e) = self.levels.remove(from.as_str()) {
                        self.levels.insert(to.clone(), e);
                    }
                }
                Change::LevelDeleted { level: l, .. } => {
                    self.levels.remove(l.as_str());
                }
                _ => {}
            }
        }
        let Some(lvl) = project.level(level) else {
            self.levels.remove(level);
            return Vec::new();
        };
        let all = changes.iter().zip(&ours).any(|(c, &o)| match c {
            Change::LevelCreated { .. } => o,
            Change::LevelMeta { part, .. } => {
                o && part.affects_bake() && !matches!(part, LevelPart::Fields | LevelPart::Tags)
            }
            Change::Project { key } => *key == ProjectKey::Seed,
            _ => false,
        });
        let field_values = changes.iter().zip(&ours).any(|(c, &o)| {
            o && matches!(
                c,
                Change::LevelMeta {
                    part: LevelPart::Fields,
                    ..
                }
            )
        });
        let field_defs = changes.iter().any(|c| {
            matches!(
                c,
                Change::Project {
                    key: ProjectKey::LevelFields
                }
            )
        });
        let tags = changes.iter().zip(&ours).any(|(c, &o)| {
            o && matches!(
                c,
                Change::LevelMeta {
                    part: LevelPart::Tags,
                    ..
                }
            )
        });
        let mut redefined = redefined(project, changes);
        if tags {
            redefined.extend(
                project
                    .rules
                    .iter()
                    .filter(|set| uses_tags(set))
                    .map(|set| set.layer.clone()),
            );
        }
        let redefined = project.with_dependents(redefined);

        // Grid edits of this level, per layer.
        let mut grid_changes: BTreeMap<Slug, DirtyRect> = BTreeMap::new();
        for (c, _) in changes.iter().zip(&ours).filter(|(_, o)| **o) {
            if let Change::Grid { layer, rect, .. } = c {
                let [ex, ey] = rect.end();
                let r = DirtyRect {
                    min: [rect.x as i32, rect.y as i32],
                    max: [ex as i32, ey as i32],
                };
                let e = grid_changes.entry(layer.clone()).or_insert(r);
                *e = e.union(r);
            }
        }
        // Effective changes of derived grids (override edits now, writers'
        // updates as they come), the input of the layers reading them.
        let mut eff_changes: BTreeMap<Slug, DirtyRect> = BTreeMap::new();
        let mut derived_out: BTreeMap<Slug, DirtyRect> = BTreeMap::new();
        let mut touched: BTreeSet<Slug> = grid_changes
            .keys()
            .filter(|g| project.layer(g.as_str()).is_some_and(|d| d.is_derived()))
            .cloned()
            .collect();
        for c in changes {
            if let Change::Def {
                kind: DefKind::Layer,
                slug,
            } = c
                && project.layer(slug.as_str()).is_some_and(|d| d.is_derived())
            {
                touched.insert(slug.clone());
            }
        }
        // The dependencies above come from the project after the changes: a
        // redefined writer may have stopped writing a derived grid (a rule
        // edit dropped or retargeted its `write`), or the writer may be gone
        // (`remove_layers`). The grids their cached bakes wrote are touched
        // too, so each is worked out again from its writer now (or its
        // overrides alone) and its readers see where it changed.
        if let Some(lc) = self.levels.get_mut(lvl.slug.as_str()) {
            let gone: Vec<Slug> = lc
                .layers
                .keys()
                .filter(|l| project.layer(l.as_str()).is_none_or(|d| !d.is_auto()))
                .cloned()
                .collect();
            for l in gone {
                if let Some(c) = lc.layers.remove(&l) {
                    touched.extend(c.bake.derived.keys().cloned());
                }
            }
            for l in &redefined {
                if let Some(c) = lc.layers.get(l) {
                    touched.extend(c.bake.derived.keys().cloned());
                }
            }
            // A layer that is no longer derived has no effective grid.
            lc.derived
                .retain(|d, _| project.layer(d.as_str()).is_some_and(|x| x.is_derived()));
        }
        touched.retain(|d| project.layer(d.as_str()).is_some_and(|x| x.is_derived()));
        if all {
            touched.extend(
                project
                    .layers
                    .iter()
                    .filter(|l| l.is_derived())
                    .map(|l| l.slug.clone()),
            );
        }
        for d in &touched {
            self.note_effective(project, lvl, d, &mut eff_changes, &mut derived_out);
            // An override edit can only change the effective grid where it
            // painted: count those cells for the readers even when the
            // effective grid can't be worked out yet (its writer isn't
            // cached; a reader that needs work bakes it first).
            if let Some(r) = grid_changes.get(d) {
                let e = eff_changes.entry(d.clone()).or_insert(*r);
                *e = e.union(*r);
            }
        }

        let any_derived = project.layers.iter().any(|l| l.is_derived());
        let (order, cyclic) = derived::order(project);
        let mut fps =
            Fingerprints::with_cyclic(project, cyclic.iter().map(|(l, _)| l.clone()).collect());
        let mut out = Vec::new();
        for layer in &order {
            // Worked out at most once per layer, and only when needed.
            let mut fp = None;
            // A level field change matters only if it changed a field the
            // layer reads: its definition (the fingerprint says) or the
            // level's value of it.
            let by_fields =
                (field_values || field_defs) && reads_level_fields(project, layer.as_str()) && {
                    let now = *fp.get_or_insert_with(|| fps.get(layer.as_str()));
                    let values = level_values_hash(project, lvl, layer.as_str());
                    self.levels
                        .get(level)
                        .and_then(|l| l.layers.get(layer))
                        .is_none_or(|c| c.fingerprint != now || c.values != values)
                };
            let need = if all || by_fields || redefined.contains(layer) {
                Some(Need::Full)
            } else {
                self.need(
                    project,
                    layer,
                    changes,
                    &ours,
                    &grid_changes,
                    &eff_changes,
                    &mut fp,
                    &mut fps,
                )
            };
            let Some(need) = need else { continue };
            if any_derived {
                self.ensure_upstream(
                    project,
                    lvl,
                    layer,
                    images,
                    &mut out,
                    &mut eff_changes,
                    &mut derived_out,
                    &mut fps,
                );
            }
            let cached = self
                .levels
                .get(level)
                .is_some_and(|l| l.layers.contains_key(layer));
            let fp = *fp.get_or_insert_with(|| fps.get(layer.as_str()));
            let r = match need {
                Need::Cells(cells) if cached => {
                    self.partial_any(project, lvl, layer, images, &cells, fp)
                }
                _ => self.full(project, lvl, layer, images, fp),
            };
            let r = self.settle(level, layer, r);
            out.push((layer.clone(), r.map(|x| x.rect)));
            if any_derived {
                for g in project.layer_writes(layer.as_str()) {
                    self.note_effective(project, lvl, &g, &mut eff_changes, &mut derived_out);
                }
            }
        }
        for (layer, cycle) in cyclic {
            let affected = self
                .levels
                .get(level)
                .is_some_and(|l| l.layers.contains_key(&layer))
                || redefined.contains(&layer)
                || all
                || self
                    .need(
                        project,
                        &layer,
                        changes,
                        &ours,
                        &grid_changes,
                        &eff_changes,
                        &mut None,
                        &mut fps,
                    )
                    .is_some();
            if affected {
                self.invalidate_level_layer(level, layer.as_str());
                out.push((layer, Err(Error::DerivedCycle(cycle))));
            }
        }
        out.extend(derived_out.into_iter().map(|(d, r)| (d, Ok(r))));
        out
    }

    /// Refresh `d`'s effective grid and record where it changed.
    fn note_effective(
        &mut self,
        project: &Project,
        lvl: &Level,
        d: &Slug,
        eff_changes: &mut BTreeMap<Slug, DirtyRect>,
        derived_out: &mut BTreeMap<Slug, DirtyRect>,
    ) {
        if let Some(r) = self.refresh_effective(project, lvl, d) {
            for m in [&mut *eff_changes, &mut *derived_out] {
                let e = m.entry(d.clone()).or_insert(r);
                *e = e.union(r);
            }
        }
    }

    /// Bake in full the uncached writers `layer` depends on (and theirs).
    #[allow(clippy::too_many_arguments)]
    fn ensure_upstream(
        &mut self,
        project: &Project,
        lvl: &Level,
        layer: &Slug,
        images: &TilesetImages,
        out: &mut Vec<(Slug, Result<DirtyRect>)>,
        eff_changes: &mut BTreeMap<Slug, DirtyRect>,
        derived_out: &mut BTreeMap<Slug, DirtyRect>,
        fps: &mut Fingerprints<'_>,
    ) {
        let Ok(ups) = derived::upstream_layers(project, layer.as_str()) else {
            return;
        };
        for w in ups {
            let cached = self
                .levels
                .get(lvl.slug.as_str())
                .is_some_and(|l| l.layers.contains_key(&w));
            if cached {
                continue;
            }
            let fp = fps.get(w.as_str());
            let r = self.full(project, lvl, &w, images, fp);
            let r = self.settle(lvl.slug.as_str(), &w, r);
            out.push((w.clone(), r.map(|x| x.rect)));
            for g in project.layer_writes(w.as_str()) {
                self.note_effective(project, lvl, &g, eff_changes, derived_out);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn need(
        &mut self,
        project: &Project,
        layer: &Slug,
        changes: &[Change],
        ours: &[bool],
        grid_changes: &BTreeMap<Slug, DirtyRect>,
        eff_changes: &BTreeMap<Slug, DirtyRect>,
        fp: &mut Option<u64>,
        fps: &mut Fingerprints<'_>,
    ) -> Option<Need> {
        let def = project.layer(layer.as_str())?;
        let source = &def.auto()?.source;
        for (c, _) in changes.iter().zip(ours).filter(|(_, o)| **o) {
            if let Change::LayerState { layer: a, .. } = c
                && a == layer
            {
                return Some(Need::Full);
            }
        }
        let mut cells: BTreeMap<Slug, DirtyRect> = BTreeMap::new();
        let derived_of_other = |g: &Slug| {
            project.layer(g.as_str()).is_some_and(|d| d.is_derived())
                && project.derived_writer(g.as_str()) != Some(layer)
        };
        for (g, r) in grid_changes {
            // Another layer's derived grid counts by its effective change.
            if derived_of_other(g) {
                continue;
            }
            let fp = *fp.get_or_insert_with(|| fps.get(layer.as_str()));
            if self.reads(project, layer, source, fp, g) {
                cells.insert(g.clone(), *r);
            }
        }
        for (g, r) in eff_changes {
            if !derived_of_other(g) {
                continue;
            }
            let fp = *fp.get_or_insert_with(|| fps.get(layer.as_str()));
            if self.reads(project, layer, source, fp, g) {
                cells.insert(g.clone(), *r);
            }
        }
        (!cells.is_empty()).then_some(Need::Cells(cells))
    }

    /// Whether `layer`'s rules read `grid`, from the per-layer grid set
    /// (worked out again only when the fingerprint changes).
    fn reads(
        &mut self,
        project: &Project,
        layer: &Slug,
        source: &Slug,
        fp: u64,
        grid: &Slug,
    ) -> bool {
        let fresh = self.reads.get(layer).is_some_and(|(f, _)| *f == fp);
        if !fresh {
            let set = project.expanded_rules(layer.as_str()).ok().map(|groups| {
                groups
                    .iter()
                    .flat_map(|g| &g.rules)
                    .flat_map(|r| r.rule.grids_read(source))
                    .cloned()
                    .collect()
            });
            self.reads.insert(layer.clone(), (fp, set));
        }
        match &self.reads[layer].1 {
            Some(set) => set.contains(grid),
            // Rules that don't expand fail to bake; let the rebake say so.
            None => project.rules_for(layer.as_str()).is_some(),
        }
    }

    /// The effective grids of the derived grids other layers write, as
    /// cached for `level`.
    fn upstream(&self, level: &str) -> Effective {
        self.levels
            .get(level)
            .map(|l| l.derived.clone())
            .unwrap_or_default()
    }

    /// Bake one layer from scratch into the cache.
    fn full(
        &mut self,
        project: &Project,
        level: &Level,
        layer: &Slug,
        images: &TilesetImages,
        fingerprint: u64,
    ) -> Result<Rebaked> {
        let def = auto_def(project, layer.as_str())?;
        let values = level_values_hash(project, level, layer.as_str());
        let upstream = self.upstream(level.slug.as_str());
        let old = self
            .levels
            .get_mut(level.slug.as_str())
            .and_then(|l| l.layers.remove(layer));
        let (new, g) = if project.rules_for(layer.as_str()).is_none() {
            let size = project.layer_cells(def, level);
            let cached = Cached {
                bake: Arc::default(),
                lens: Vec::new(),
                draw: Vec::new(),
                rules: Vec::new(),
                fingerprint,
                values,
                size: [size[0] as i32, size[1] as i32],
                staged: None,
            };
            (cached, project.layer_grid(def) as i32)
        } else {
            let baker = Baker::new(project, level, def, &upstream)?;
            let crate::bake::Run {
                out,
                writes,
                computed,
                candidates,
                occ_after,
                ..
            } = baker.run(images, false)?;
            let lens = (0..baker.rule_count())
                .map(|ri| {
                    if baker.is_write(ri) {
                        0
                    } else {
                        out.buckets[ri].len()
                    }
                })
                .collect();
            let staged = baker.is_staged().then(|| {
                Box::new(Staged {
                    firings: out.buckets.iter().map(|b| anchors(b)).collect(),
                    writes,
                    candidates,
                    occ_after,
                })
            });
            let cached = Cached {
                bake: Arc::new(baker.finish(out, &computed)),
                lens,
                draw: baker.draw_order(),
                rules: baker.rule_keys(),
                fingerprint,
                values,
                size: baker.size(),
                staged,
            };
            (cached, baker.grid_px())
        };
        let mut rect = DirtyRect::of_size(new.size);
        for p in old.iter().chain([&new]).flat_map(|c| &c.bake.placements) {
            rect = rect.union(drawn(project, p, g));
        }
        self.levels
            .entry(level.slug.clone())
            .or_default()
            .layers
            .insert(layer.clone(), new);
        Ok(Rebaked { rect })
    }

    /// A partial update: the plain re-sweep, or pass by pass for a layer
    /// using passes, regions or occupancy.
    fn partial_any(
        &mut self,
        project: &Project,
        level: &Level,
        layer: &Slug,
        images: &TilesetImages,
        cells: &BTreeMap<Slug, DirtyRect>,
        fingerprint: u64,
    ) -> Result<Rebaked> {
        let staged = self
            .levels
            .get(level.slug.as_str())
            .and_then(|l| l.layers.get(layer))
            .is_some_and(|c| c.staged.is_some());
        if staged {
            return self.partial_staged(project, level, layer, images, cells, fingerprint);
        }
        let merged = cells.values().fold(DirtyRect::EMPTY, |a, &b| a.union(b));
        self.partial(project, level, layer, images, merged, fingerprint)
    }

    /// Whether the cached entry can be patched under the current rules and
    /// level values; `None` when it can't.
    fn fresh_entry(
        &self,
        project: &Project,
        level: &Level,
        layer: &Slug,
        fingerprint: u64,
    ) -> bool {
        self.levels
            .get(level.slug.as_str())
            .and_then(|l| l.layers.get(layer))
            .is_some_and(|e| {
                e.fingerprint == fingerprint
                    && e.values == level_values_hash(project, level, layer.as_str())
            })
    }

    /// Re-sweep the anchors within reach of `cells` and splice them in.
    fn partial(
        &mut self,
        project: &Project,
        level: &Level,
        layer: &Slug,
        images: &TilesetImages,
        cells: DirtyRect,
        fingerprint: u64,
    ) -> Result<Rebaked> {
        let def = auto_def(project, layer.as_str())?;
        if project.rules_for(layer.as_str()).is_none()
            || !self.fresh_entry(project, level, layer, fingerprint)
        {
            // Other rules (or required level field values) than the cached
            // bake's: patching would mix them.
            return self.full(project, level, layer, images, fingerprint);
        }
        let upstream = self.upstream(level.slug.as_str());
        let baker = Baker::new(project, level, def, &upstream)?;
        let entry = self
            .levels
            .get_mut(level.slug.as_str())
            .and_then(|l| l.layers.get_mut(layer))
            .expect("checked above");
        if entry.size != baker.size() || entry.rules != baker.rule_keys() || baker.is_staged() {
            return self.full(project, level, layer, images, fingerprint);
        }
        let [w, h] = baker.size();
        let [rx, ry] = baker.max_radius();
        let min = [(cells.min[0] - rx).max(0), (cells.min[1] - ry).max(0)];
        let max = [(cells.max[0] + rx).min(w), (cells.max[1] + ry).min(h)];
        let mut rect = DirtyRect { min, max };
        if rect.is_empty() {
            return Ok(Rebaked {
                rect: DirtyRect::EMPTY,
            });
        }
        let mut out = baker.output();
        baker.bake_rect(&mut out, images, min, max)?;
        let fresh = out.buckets;

        let inside =
            |a: [i32; 2]| a[0] >= min[0] && a[0] < max[0] && a[1] >= min[1] && a[1] < max[1];
        let key = |a: [i32; 2]| (a[1], a[0]);
        let g = baker.grid_px();
        let bake = Arc::make_mut(&mut entry.bake);
        let old = std::mem::take(&mut bake.placements);
        let mut old = old.into_iter();
        let mut placements = Vec::with_capacity(old.len());
        let n = entry.lens.len();
        let mut lens = vec![0; n];
        for ri in (0..n).rev() {
            let start = placements.len();
            let mut fi = fresh[ri].iter().peekable();
            for p in old.by_ref().take(entry.lens[ri]) {
                if inside(p.anchor) {
                    rect = rect.union(drawn(project, &p, g));
                    continue;
                }
                while let Some(f) = fi.next_if(|f| key(f.anchor) < key(p.anchor)) {
                    let np = baker.placement(ri, f);
                    rect = rect.union(drawn_sized(&np, baker.tile_size(ri), g));
                    placements.push(np);
                }
                placements.push(p);
            }
            for f in fi {
                let np = baker.placement(ri, f);
                rect = rect.union(drawn_sized(&np, baker.tile_size(ri), g));
                placements.push(np);
            }
            lens[ri] = placements.len() - start;
        }
        bake.placements = placements;
        entry.lens = lens;

        // `fired` is keyed [x, y]: drop the re-swept anchors column by
        // column, then add the fresh ones in rule order.
        let fired = &mut bake.fired;
        for x in min[0]..max[0] {
            let keys: Vec<[i32; 2]> = fired
                .range([x, min[1]]..[x, max[1]])
                .map(|(k, _)| *k)
                .collect();
            for k in keys {
                fired.remove(&k);
            }
        }
        for (ri, bucket) in fresh.iter().enumerate() {
            let mut prev = None;
            for p in bucket {
                if prev != Some(p.anchor) {
                    fired
                        .entry(p.anchor)
                        .or_default()
                        .push(baker.rule_slug(ri).clone());
                    prev = Some(p.anchor);
                }
            }
        }
        Ok(Rebaked { rect })
    }

    /// The pass-by-pass update of a layer using passes, regions or occupancy (crate README,
    /// "Incremental baking"): for each pass in order, the anchors whose
    /// inputs changed (pattern rules: changed cells of the grids the pass
    /// reads, grown by the pass's pattern radius; region rules: the
    /// symmetric difference of their candidates, recomputed when their grid
    /// changed), the whole pass when it uses occupancy and anything in it
    /// may change; the pass's changed writes are inputs of the later passes.
    fn partial_staged(
        &mut self,
        project: &Project,
        level: &Level,
        layer: &Slug,
        images: &TilesetImages,
        cells: &BTreeMap<Slug, DirtyRect>,
        fingerprint: u64,
    ) -> Result<Rebaked> {
        let def = auto_def(project, layer.as_str())?;
        if project.rules_for(layer.as_str()).is_none()
            || !self.fresh_entry(project, level, layer, fingerprint)
        {
            return self.full(project, level, layer, images, fingerprint);
        }
        let upstream = self.upstream(level.slug.as_str());
        let baker = Baker::new(project, level, def, &upstream)?;
        let entry = self
            .levels
            .get_mut(level.slug.as_str())
            .and_then(|l| l.layers.get_mut(layer))
            .expect("checked above");
        if entry.size != baker.size()
            || entry.rules != baker.rule_keys()
            || !baker.is_staged()
            || entry.staged.is_none()
        {
            return self.full(project, level, layer, images, fingerprint);
        }
        let size = baker.size();
        let [w, h] = size;
        let n = (w * h) as usize;
        let nw = baker.writes.len();
        let st = entry.staged.as_mut().expect("checked above");
        // Changed cells per sampled grid, and per written grid this call's
        // changed writes (inputs of the later passes reading it).
        let gdirty: Vec<Option<DirtyRect>> = baker
            .grid_slugs
            .iter()
            .map(|s| cells.get(s).copied())
            .collect();
        let read_of: Vec<Option<usize>> = (0..baker.grids.len())
            .map(|gi| baker.writes.iter().position(|g| g.read_index() == Some(gi)))
            .collect();
        let mut self_dirty: Vec<Option<DirtyRect>> = vec![None; nw];
        let mut computed: Vec<Vec<u16>> = vec![vec![0; n]; nw];
        let mut occ: Vec<bool> = if baker.uses_occupancy() {
            vec![false; n]
        } else {
            Vec::new()
        };
        let mut occ_changed = false;
        let mut out = baker.output();
        let mut swept: Vec<Option<DirtyRect>> = vec![None; baker.passes.len()];
        for (p, pd) in baker.passes.iter().enumerate() {
            let ov = baker.pass_overlay(p, &computed);
            let grids = baker.grid_view(&ov);
            let dirty_of = |gi: usize, self_dirty: &[Option<DirtyRect>]| {
                union_opt(gdirty[gi], read_of[gi].and_then(|wi| self_dirty[wi]))
            };
            let mut anchors: Option<DirtyRect> = None;
            let has_patterns = pd.rules.clone().any(|ri| !baker.is_region(ri));
            if has_patterns {
                let d = pd
                    .reads
                    .iter()
                    .fold(None, |a, &gi| union_opt(a, dirty_of(gi, &self_dirty)));
                anchors = d.map(|d| d.grow(pd.radius, size));
            }
            for ri in pd.rules.clone().filter(|&ri| baker.is_region(ri)) {
                let gi = baker
                    .grid_slugs
                    .iter()
                    .position(|s| s == baker.rule_grid(ri))
                    .expect("sampled");
                if dirty_of(gi, &self_dirty).is_none() {
                    continue;
                }
                let new = region::flatten(&baker.region_candidates(ri, &grids).1);
                let diff = sym_diff_rect(&st.candidates[ri], &new);
                anchors = union_opt(anchors, diff);
                st.candidates[ri] = new;
            }
            let rect = if pd.occupancy && (anchors.is_some() || occ_changed) {
                Some(DirtyRect::of_size(size))
            } else {
                anchors.filter(|r| !r.is_empty())
            };
            if pd.occupancy {
                out.occupied.clone_from(&occ);
            }
            if let Some(r) = rect {
                let mut writes = st.writes[p].clone();
                baker.sweep(
                    p,
                    &grids,
                    &st.candidates,
                    &mut out,
                    &mut writes,
                    images,
                    r.min,
                    r.max,
                )?;
                for wi in 0..nw {
                    for y in r.min[1]..r.max[1] {
                        for x in r.min[0]..r.max[0] {
                            let i = (y * w + x) as usize;
                            if writes[wi][i] != st.writes[p][wi][i] {
                                self_dirty[wi] =
                                    union_opt(self_dirty[wi], Some(DirtyRect::cell(x, y)));
                            }
                        }
                    }
                }
                st.writes[p] = writes;
                swept[p] = Some(r);
            }
            overlay(&mut computed, &st.writes[p]);
            if pd.occupancy {
                let new_occ = if swept[p].is_some() {
                    out.occupied.clone()
                } else {
                    st.occ_after[p].clone().unwrap_or_else(|| occ.clone())
                };
                occ_changed = st.occ_after[p].as_ref() != Some(&new_occ);
                st.occ_after[p] = Some(new_occ.clone());
                occ = new_occ;
            }
        }

        // Splice the fresh placements and firings of the re-swept passes.
        let pass_of: Vec<usize> = (0..baker.rule_count())
            .map(|ri| baker.pass_of(ri))
            .collect();
        let g = baker.grid_px();
        let key = |a: [i32; 2]| (a[1], a[0]);
        let mut rect = DirtyRect::EMPTY;
        for r in swept.iter().flatten() {
            rect = rect.union(*r);
        }
        let bake = Arc::make_mut(&mut entry.bake);
        let old = std::mem::take(&mut bake.placements);
        let mut old = old.into_iter();
        let mut placements = Vec::with_capacity(old.len());
        let mut lens = vec![0; entry.lens.len()];
        for &ri in &entry.draw {
            let start = placements.len();
            let Some(r) = swept[pass_of[ri]] else {
                placements.extend(old.by_ref().take(entry.lens[ri]));
                lens[ri] = placements.len() - start;
                continue;
            };
            if baker.is_write(ri) {
                continue;
            }
            let fresh = &out.buckets[ri];
            let mut fi = fresh.iter().peekable();
            for p in old.by_ref().take(entry.lens[ri]) {
                if r.contains(p.anchor) {
                    rect = rect.union(drawn(project, &p, g));
                    continue;
                }
                while let Some(f) = fi.next_if(|f| key(f.anchor) < key(p.anchor)) {
                    let np = baker.placement(ri, f);
                    rect = rect.union(drawn_sized(&np, baker.tile_size(ri), g));
                    placements.push(np);
                }
                placements.push(p);
            }
            for f in fi {
                let np = baker.placement(ri, f);
                rect = rect.union(drawn_sized(&np, baker.tile_size(ri), g));
                placements.push(np);
            }
            lens[ri] = placements.len() - start;
        }
        bake.placements = placements;
        entry.lens = lens;
        for (ri, firings) in st.firings.iter_mut().enumerate() {
            let Some(r) = swept[pass_of[ri]] else {
                continue;
            };
            let fresh = anchors(&out.buckets[ri]);
            let mut merged = Vec::with_capacity(firings.len() + fresh.len());
            let mut fi = fresh.into_iter().peekable();
            for a in firings.drain(..) {
                if r.contains(a) {
                    continue;
                }
                while let Some(f) = fi.next_if(|f| key(*f) < key(a)) {
                    merged.push(f);
                }
                merged.push(a);
            }
            merged.extend(fi);
            *firings = merged;
        }

        // `fired`: rebuilt over every re-swept rect from the firings.
        if !rect.is_empty() {
            let u = swept
                .iter()
                .flatten()
                .fold(DirtyRect::EMPTY, |a, &b| a.union(b));
            let fired = &mut bake.fired;
            for x in u.min[0]..u.max[0] {
                let keys: Vec<[i32; 2]> = fired
                    .range([x, u.min[1]]..[x, u.max[1]])
                    .map(|(k, _)| *k)
                    .collect();
                for k in keys {
                    fired.remove(&k);
                }
            }
            for (ri, firings) in st.firings.iter().enumerate() {
                for y in u.min[1]..u.max[1] {
                    let lo = firings.partition_point(|a| key(*a) < (y, u.min[0]));
                    for a in &firings[lo..] {
                        if a[1] != y || a[0] >= u.max[0] {
                            break;
                        }
                        fired
                            .entry(*a)
                            .or_default()
                            .push(baker.rule_slug(ri).clone());
                    }
                }
            }
        }
        bake.derived = baker.derived_layers(&computed);
        bake.occupied = occ;
        Ok(Rebaked { rect })
    }
}

/// What a layer's rebake changed.
struct Rebaked {
    /// Where to redraw the layer.
    rect: DirtyRect,
}

/// The distinct anchors of a bucket (sorted by anchor), in order.
fn anchors(bucket: &[crate::bake::Raw]) -> Vec<[i32; 2]> {
    let mut out: Vec<[i32; 2]> = Vec::new();
    for p in bucket {
        if out.last() != Some(&p.anchor) {
            out.push(p.anchor);
        }
    }
    out
}

/// The bounding rect of the cells in exactly one of two row-major sorted
/// lists.
fn sym_diff_rect(a: &[[i32; 2]], b: &[[i32; 2]]) -> Option<DirtyRect> {
    let key = |c: &[i32; 2]| (c[1], c[0]);
    let (mut i, mut j) = (0, 0);
    let mut r: Option<DirtyRect> = None;
    let mut add = |c: [i32; 2]| r = union_opt(r, Some(DirtyRect::cell(c[0], c[1])));
    while i < a.len() || j < b.len() {
        match (a.get(i), b.get(j)) {
            (Some(x), Some(y)) if x == y => {
                i += 1;
                j += 1;
            }
            (Some(x), Some(y)) if key(x) < key(y) => {
                add(*x);
                i += 1;
            }
            (Some(_), Some(y)) => {
                add(*y);
                j += 1;
            }
            (Some(x), None) => {
                add(*x);
                i += 1;
            }
            (None, Some(y)) => {
                add(*y);
                j += 1;
            }
            (None, None) => break,
        }
    }
    r
}

/// The whole of a grid as a rect.
fn grid_rect(g: &IntGridLayer) -> DirtyRect {
    let [w, h] = g.size();
    DirtyRect::of_size([w as i32, h as i32])
}

/// The bounding rect of the cells where two grids differ (`None`: equal).
/// Grids of different sizes differ everywhere.
fn diff_grids(a: &IntGridLayer, b: &IntGridLayer) -> Option<DirtyRect> {
    if a.size() != b.size() {
        return Some(grid_rect(a).union(grid_rect(b)));
    }
    let w = a.size()[0] as usize;
    let mut r: Option<DirtyRect> = None;
    for (i, (x, y)) in a.cells().iter().zip(b.cells()).enumerate() {
        if x != y {
            r = union_opt(r, Some(DirtyRect::cell((i % w) as i32, (i / w) as i32)));
        }
    }
    r
}

/// Forget `layer` in one level, and the effective grids it feeds.
fn drop_layer(l: &mut LevelCache, layer: &str) {
    if let Some(c) = l.layers.remove(layer) {
        for d in c.bake.derived.keys() {
            l.derived.remove(d);
        }
    }
}

/// Every layer's rect, or the first layer's error.
fn first_error(out: Vec<(Slug, Result<DirtyRect>)>) -> Result<Vec<(Slug, DirtyRect)>> {
    out.into_iter().map(|(l, r)| Ok((l, r?))).collect()
}

/// The auto layers whose definitions `changes` touch (see
/// [`BakeCache::apply_changes`]): they rebake in full in every level.
fn redefined(project: &Project, changes: &[Change]) -> BTreeSet<Slug> {
    let mut out = BTreeSet::new();
    for c in changes {
        let Change::Def { kind, slug } = c else {
            continue;
        };
        match kind {
            DefKind::Rules => {
                out.insert(slug.clone());
            }
            DefKind::Macros => out.extend(
                project
                    .rules
                    .iter()
                    .filter(|set| set.uses_macros())
                    .map(|set| set.layer.clone()),
            ),
            DefKind::Tileset => out.extend(project.auto_layers_drawing_from(slug.as_str())),
            DefKind::Layer => {
                for l in project.layers.iter().filter(|l| l.is_auto()) {
                    if l.slug == *slug || project.layer_reads(l.slug.as_str()).contains(slug) {
                        out.insert(l.slug.clone());
                    }
                }
                if let Some(w) = project.derived_writer(slug.as_str()) {
                    out.insert(w.clone());
                }
            }
            _ => {}
        }
    }
    out
}

/// For each change, whether it is about the level that is called `level`
/// after all of them: walking back from the end, a rename to the name we
/// follow means earlier changes know the level by the old name.
fn ours(changes: &[Change], level: &str) -> Vec<bool> {
    let mut name = level;
    let mut out = vec![false; changes.len()];
    for (i, c) in changes.iter().enumerate().rev() {
        out[i] = c.level().is_some_and(|l| l == name);
        if let Change::LevelRenamed { from, to, .. } = c
            && to == name
        {
            name = from.as_str();
        }
    }
    out
}

/// The cells a placement draws over, from its tileset's tile size.
fn drawn(project: &Project, p: &Placement, g: i32) -> DirtyRect {
    let ts = project
        .tileset(p.tileset.as_str())
        .map_or([g, g], |t| [t.tile_size[0] as i32, t.tile_size[1] as i32]);
    drawn_sized(p, ts, g)
}

fn drawn_sized(p: &Placement, size: [i32; 2], g: i32) -> DirtyRect {
    let g = g.max(1);
    DirtyRect {
        min: [p.px[0].div_euclid(g), p.px[1].div_euclid(g)],
        max: [
            (p.px[0] + size[0] + g - 1).div_euclid(g),
            (p.px[1] + size[1] + g - 1).div_euclid(g),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::{FINGERPRINTS_HASHED, Fingerprints, rules_fingerprint};

    /// `a` writes `d1`, `b` reads it and writes `d2`, `c` reads `d2`.
    fn chain() -> worlds_core::Project {
        use serde_json::json;
        let mut p = worlds_core::Project::new("t");
        p.grid_size = 16;
        let marks = |s: &str| {
            json!({ "slug": s, "name": s, "kind": "int_grid", "derived": true,
                "values": [{ "slug": "m", "name": "M", "color": "#808080", "glyph": "m" }] })
        };
        p.layers = serde_json::from_value(json!([
            { "slug": "collision", "name": "C", "kind": "int_grid",
              "values": [{ "slug": "solid", "name": "S", "color": "#808080", "glyph": "#" }] },
            marks("d1"), marks("d2"),
            { "slug": "a", "name": "A", "kind": "auto", "source": "collision" },
            { "slug": "b", "name": "B", "kind": "auto", "source": "d1" },
            { "slug": "c", "name": "C", "kind": "auto", "source": "d2" }
        ]))
        .unwrap();
        let rules = |layer: &str, write: Option<&str>| {
            let r = match write {
                Some(g) => {
                    json!({ "slug": "r", "match": { "rows": ["*"] }, "write": { "grid": g, "value": "m" } })
                }
                None => {
                    json!({ "slug": "r", "match": { "rows": ["*"] }, "out": { "tileset": "t", "tiles": [[0, 0]] } })
                }
            };
            serde_json::from_value(
                json!({ "layer": layer, "groups": [{ "slug": "g", "rules": [r] }] }),
            )
            .unwrap()
        };
        p.rules = vec![
            rules("a", Some("d1")),
            rules("b", Some("d2")),
            rules("c", None),
        ];
        p
    }

    fn hashed(f: impl FnOnce()) -> usize {
        FINGERPRINTS_HASHED.with(|n| n.set(0));
        f();
        FINGERPRINTS_HASHED.with(|n| n.get())
    }

    /// Within one call each layer's fingerprint is hashed once, whatever
    /// reads it; the values are `rules_fingerprint`'s.
    #[test]
    fn fingerprints_are_hashed_once_per_call() {
        let p = chain();
        assert_eq!(p.layer_dependencies("c").len(), 1);
        let each: Vec<u64> = ["c", "b", "a"]
            .iter()
            .map(|l| rules_fingerprint(&p, l))
            .collect();
        // Alone, c covers b and a, b covers a: 3 + 2 + 1.
        let alone = hashed(|| {
            for l in ["c", "b", "a"] {
                rules_fingerprint(&p, l);
            }
        });
        assert_eq!(alone, 6);
        let mut fps = Fingerprints::new(&p);
        let mut got = Vec::new();
        let once = hashed(|| got = ["c", "b", "a"].iter().map(|l| fps.get(l)).collect());
        assert_eq!(once, 3);
        assert_eq!(got, each);
    }

    /// `Project::with_dependents` (shared by this cache and the editor's
    /// command layer) follows readers transitively.
    #[test]
    fn with_dependents_follows_readers_transitively() {
        let p = chain();
        let set = |l: &[&str]| -> std::collections::BTreeSet<worlds_core::Slug> {
            l.iter().map(|s| worlds_core::slug(s)).collect()
        };
        assert_eq!(p.with_dependents(set(&["a"])), set(&["a", "b", "c"]));
        assert_eq!(p.with_dependents(set(&["b"])), set(&["b", "c"]));
        assert_eq!(p.with_dependents(set(&["c"])), set(&["c"]));
        assert_eq!(p.with_dependents(set(&[])), set(&[]));
    }
}
