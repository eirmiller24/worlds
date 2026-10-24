# worlds-rules

The autotile engine. Given a project, a level and an `auto` layer, it runs
`rules/<layer>.json` over the level's IntGrid cells and returns tile
placements, and the values its `write` rules compute for derived grids.

```rust
let images = worlds_rules::TilesetImages::new(project_dir, &project.tilesets);
let bake = worlds_rules::bake_layer(&project, level, &layer_slug, &images)?;
let all = worlds_rules::bake_level(&project, level, &images)?; // every auto layer
```

`LayerBake` has:

- `placements`, in draw order;
- `fired`: anchor cell to the rules that fired there (drew at least one
  tile or wrote a derived cell), in run order; the parity tests compare
  it;
- `derived`: the **computed** values of each derived grid the layer
  writes (0 where nothing was written; overrides not applied), in cells of
  the layer's grid;
- `occupied`: the occupancy grid after the last pass, row-major (empty when
  no rule checks or marks occupancy).

Rule slugs are the *expanded* ones: a template reference `walls` using
template rule `fill` bakes as rule `walls__fill`.

`bake_layer` of a layer that reads derived grids written by other auto
layers bakes those writers first, so it always equals `bake_level`'s entry.

## API

Baking:

- `bake_layer(&project, level, &layer, &images) -> Result<LayerBake>`,
  `bake_level(&project, level, &images) -> Result<BTreeMap<Slug,
  LayerBake>>` (dependency order; a cycle is `Error::DerivedCycle`).
- `bake_for_export(&project, level, &images) -> Result<BakedLevel>`: the
  tiles of every auto layer and the **effective** grid of every derived
  layer, the shape `worlds_core::export::write` takes (what `worlds-cli
  bake` runs).
- `effective_grid(&project, level, grid, &images) -> Result<IntGridLayer>`:
  one int_grid layer's effective values (for a derived layer, its
  overrides over what its writer computes; the writer is baked for it).
- `export_warnings(&project, &baked) -> Vec<String>`: what a game
  importer can't read faithfully, the `warnings` of `worlds-cli bake`,
  `project.bake` and MCP `bake_and_validate`. Currently always empty: the
  Godot addon reads transposed tiles, tiles off their layer's grid and
  several auto layers per level. `worlds-cli check-export` checks an export
  against the format (`docs/format.md`, "Checking an export").
- `BakeCache`: incremental baking (below), with `derived(level, layer) ->
  Option<Arc<IntGridLayer>>` (effective), `bake_derived(&project, level,
  layer, &images) -> Result<Option<Arc<IntGridLayer>>>` (the same, baking
  into the cache only the writer and the writers it depends on that aren't
  cached; what the server's derived reads use) and `occupied(level, layer)
  -> Option<&[bool]>`.

Where rules fire (the editor's highlight, the server's `rules.preview`):

- `rule_matches(&project, level, &layer, slug, &images) -> Result<Matches>`:
  every anchor where a rule could fire: a pattern rule's patterns (`match`,
  every `require`, modulo/checker) with the flips of the variants that
  match, ignoring chance, perlin, the break rule and occupancy; a region
  rule's candidates (`[None]` each). Grids are read as the rule's pass sees
  them (that is why it takes `images`: a pass reading what an earlier pass
  wrote needs the bake). `slug` may be an inline rule, an expanded rule or a
  template reference (the union of its expanded rules).
- `rule_matches_many(..., slug, &images)`: the same per expanded rule, and
  `rule_matches_all(&project, level, &layer, &images)`: every expanded rule
  of the layer from one compile, in run order (groups by `pass`).
- `preview_rules(&project, level, &layer, &images) -> Result<Vec<RulePreview>>`:
  every rule that runs in the level, in run order, from one full bake.
  `RulePreview { rule, group, pass, grid, fired, matches, occupied, regions,
  write }`: `grid` is `match`'s grid or `region.grid`; `fired` the anchors
  where it fired (row-major); `matches` as `rule_matches` (the anchors that
  didn't fire are `matches` minus `fired`); `occupied` the anchors where it
  matched, its gates passed and it wasn't locked, but every matching
  variant's footprint was occupied; `regions` (region rules only) every
  region of `region.cells` on its grid as `RegionPreview { id, bbox, area,
  selected, runs, candidates }` (`runs`: the rule's edge after `against`,
  before `margin` and `min_length`; `[]` for `across`); `write` the rule's
  `write`. This is what `rules.preview` with `pass`, `regions`,
  `occupied` and `write` is built from (capping lists at 1000 is the
  server's job).
- `preview_rules_for(&project, level, &layer, &images, &wanted, matches)`:
  the same for the rules `wanted(expanded slug)` keeps, with `matches`
  worked out only when `matches` is true (else empty): `rules.preview` asks
  for exactly what it returns.
- `region_preview(&project, level, &layer, rule, &images) ->
  Result<Option<Vec<RegionPreview>>>`: that one rule's `regions` (`None`
  for a pattern rule, or a rule that doesn't run in the level).

Regions of any grid (`rules.regions`, MCP `find_regions`):

- `regions(&project, grid, &values, &cells) -> Result<Regions>`: every
  4-connected region of `cells` (a `CellMatch`) on int_grid layer `grid`,
  read from `values` (the caller passes the **effective** grid:
  `BakeCache::derived`, `effective_grid`, or `level.int_grid` for a layer
  that isn't derived). `Regions { size, labels, regions }`: `labels` per
  cell (region id + 1, 0 outside), `regions` by id (row-major order of their
  first cell) as `RegionInfo { id, bbox: [x, y, w, h], area, edges: Edges {
  floor, ceiling, wall_left, wall_right } }`, each edge a list of runs `[x,
  y, len]` (no `against`). `Regions::picture()` gives the text rows (`a`..`z`
  by id, then `+`; `#` elsewhere), `Regions::at(x, y)` a cell's region.

## Pipeline

0. **Layer order.** Auto layers bake writers first: layer L depends on W
   when any of L's rules (templates expanded, active or not) reads
   (`match.grid`, `require[].grid`, `region.grid`, or L's `source`) a
   derived grid that W writes, W ≠ L (`Project::layer_dependencies`, shared
   with validation). Ties go in `layers.json` order, so without derived
   grids it is just `layers.json` order. Layers that read each other's
   derived grids (a cycle) fail with `Error::DerivedCycle`, as does a layer
   depending on them; more than one layer writing a grid fails with
   `Error::SecondWriter`. A reader sees each writer's final **effective**
   grid (overrides over its computed values).
1. **Expand.** `Project::expanded_rules(layer)` (in worlds-core, shared
   with validation, the server and the editor) turns the rule file into
   plain rules: each `{ "use": ... }` reference becomes its template's
   rules (slugs `<reference>__<rule>`, tiles shifted by `origin`, the
   reference's tileset, legends merged), and named rows and legends from
   `rules/macros.json` are looked up. A reference to an unknown template, or
   an unknown named pattern or legend, is a bake error (`Error::Expand`)
   naming it. Hash keys use the expanded slug and the template rule's own
   `salt`, so moving `origin` or switching tileset never re-rolls.
2. **Compile.** The layer bakes on its source's grid: anchors are cells of
   that grid, `ceil(level pixels / grid)` of them per axis. Groups that
   don't apply in this level are dropped (see [Groups](#groups)), as are
   inactive rules; the rest are sorted by `pass`, stably (the **run
   order**). Each grid a rule reads (`match.grid`, every `require[].grid`,
   `region.grid`, default the layer's `source`, on the same grid size) is
   copied once into a dense array of value indices: a derived grid another
   layer writes by its effective value, a derived grid nobody writes by its
   overrides, and a derived grid this layer writes by its overrides, with
   what earlier passes wrote laid in per pass (below). Glyphs resolve
   through the legend, then `* + . @`, then the grid's value and group
   glyphs. Each pattern becomes a list of `(dx, dy, test)` cells measured
   from its **anchor** (the `@` cell, else the center; `*` cells dropped,
   anchor first) with one lookup table per distinct test (two glyphs
   meaning the same test share one; a `[a|b]` / `![a|b]` legend value is
   one table, the union of its members, negated for `!`). Each `require`
   is another pattern on its own grid, anchored on the same cell: a rule
   matches where `match` and every `require` hold. Symmetry variants
   (below) are worked out once over all the patterns together, and a
   variant that samples exactly like an earlier one (same tests at the same
   offsets of the same grids) is dropped, so a symmetric pattern never
   stacks a turned copy of itself. A region rule compiles its `cells` (and
   `against`) tables and its placement; a `write` resolves its grid (a
   derived int_grid layer on the layer's grid, else `Error::BadWrite`) and
   value. The level's tileset substitutions are applied, and alternatives,
   weights and stamp offsets are precomputed.
3. **Sweep**, pass by pass (see [Passes](#passes)). At the start of a pass
   each region rule of the pass works out its candidates
   ([Region rules](#region-rules)). Then rule by rule, in run order, over
   every anchor cell (row major), skipping anchors an earlier rule of the
   pass locked: a pattern rule checks `modulo`/`checker`, the anchor's own
   test, then each variant's patterns; on the first matching variant the
   `chance` and `perlin` gates are rolled once for the cell; each matching
   variant places its tiles (only the first one placed if the rule has
   `break`), unless `avoid_overlap` finds its footprint occupied
   ([Occupancy](#occupancy)). A region rule fires at its candidates instead,
   as variant 0 with no gates. A `write` rule writes its value at the
   anchor (the first write to a cell in a pass wins).
4. **Finish.** Placements are emitted in draw order, `fired` is built, and
   the computed derived grids and the occupancy are handed over.

`sweep` takes a cell range, which is what the incremental bake below
re-sweeps.

**Non-rectangular levels.** A level with `void` ([format.md](../../docs/format.md#non-rectangular-levels-void))
bakes as if its void cells were past its edge, in cells of the layer's
grid (a cell is void when every project cell it covers is,
`Project::layer_void`): a pattern cell over the void reads the rule's
`out_of_bounds` value, or the variant doesn't match without one (the
matcher's slow path, which a level with void always takes); a void cell
is never an anchor, so nothing is drawn or written there and derived grids
compute nothing in it; no region holds a void cell, and an edge run stops
at one (the void is the border, not a wall: a cell next to the void is no
more on an edge than one next to the box). `regions_in` is `regions` with
a void mask. A level without void takes exactly the old paths, so its
bake is unchanged, which `tests/fixture_bakes.rs` holds it to; a
`level_meta` change of part `void` rebakes the level in full.

## Passes

A pass is every group with the same `pass` (default 0). Groups run sorted by
pass, stably, so file order within a pass.

- **Locks** (`break` and the opaque-tile lock) last for the rest of their
  pass: each pass starts with no locked anchor.
- **Writes**: each pass writes into its own buffer, 0 = nothing written;
  the first write to a cell in a pass wins (later writes there are dropped,
  but the rule still fired: it is in `fired`, and `break` locks). After the
  pass the buffer is laid over the computed grid, so a later pass may
  overwrite a cell but never erase it. A pass reads each derived grid as it
  was when the pass started (the overrides over what earlier passes
  computed), so inside a pass rule order never changes what a rule reads.
- **Occupancy** belongs to the (level, layer) and is kept across passes.
- **Draw order**: passes stack bottom to top. Pass 0's tiles come first
  (bottom), the last pass's on top; within a pass, as always, the last rule
  first (earlier rules on top), each rule's tiles by anchor row-major, then
  variant, then the alternative's tile order. A layer with one pass draws
  exactly as if passes didn't exist.

## Region rules

A region rule (`region` + `place`) fires at places inside regions of cells
instead of where a pattern holds. Per pass, for each region rule:

1. **Regions**: 4-connected components of the cells of `region.grid` (as
   the pass sees it) that pass `region.cells`; a flood fill over the whole
   level, row-major, so region ids are the row-major order of their first
   cells. bbox, width, height and area come with it. A region is
   **selected** when every given bound (`min_/max_width`, `_height`,
   `_area`, inclusive) holds.
2. **Edges** (for `along`): region cell `(x, y)` is on the `floor` when
   `(x, y+1)` is inside the level, isn't a region cell and passes
   `against` (if given); `ceiling` looks at `(x, y-1)`, the walls at
   `(x∓1, y)`. The level's border is never an edge. Floor and ceiling cells
   form runs along rows, wall cells down columns, listed row-major by first
   cell.
3. **Positions**: per run (with `along`; runs shorter than `min_length`
   skipped) or per bbox axis (with `across`, then kept where the cell is a
   region cell), the 1D strategy over `a..=b` with `lo = a + margin`,
   `hi = b - margin`, `U = hi - lo + 1` (nothing if `U < 1`) and
   `k = (U - 1) div every + 1`:
   - `even`: slack `s = (U - 1) - (k - 1) every`; offset 0 (`start`),
     `s div 2` (`center`; odd slack leans to the start), `s` (`end`);
     positions `lo + o + i every`.
   - `spread`: `k = 1`: `lo + (U - 1) div 2`; else
     `lo + floor((2 i (U - 1) + (k - 1)) / (2 (k - 1)))`: both ends hit,
     every gap at least `every`.
   - `random`: `p0 = lo + h(lo, REGION_PHASE) mod every`, then
     `p' = p + every + h(p, REGION_STEP) mod (jitter + 1)`, until past
     `hi`. `h` hashes the layer seed, the rule key and the cell whose run
     coordinate is `p` (the run's row or column; with `across`, x positions
     on row `bbox.y`, y positions on column `bbox.x`).
4. **Candidates**: the positions over every selected region, without
   repeats, row-major.

The rule then fires at each candidate in row-major order exactly as a
pattern rule that matched there as written (variant 0): skipped at a
locked anchor; with `out`, the alternative is picked with `ALT + 0`,
`random_offset_px` rolls with `OFFSET_X/Y + 0`, `avoid_overlap` and the
opaque-tile lock apply; with `write`, it writes; `break` locks the anchor.
Everything is keyed by cells, never by region ids or hash-map order, so the
same grid gives the same candidates. `format.md` has the worked examples;
the derived-grid and region-rule tests run every one of them through the
engine.

## Derived grids

A derived grid (an `int_grid` layer with `derived: true`) holds no painted
content: its level files are **overrides** (`DerivedOverrides`: a value, or
suppressed, per cell). What every reader gets is the **effective** grid,
built here (`effective_grid`, `BakeCache::derived`, `bake_for_export`): the
override where there is one (a suppressed cell is empty), else the value
the writer computed (`LayerBake::derived`), else empty. A suppressed cell
reads exactly like a cell nothing wrote: it passes `.` and `!x`, never `+`
or a value. Overrides never reach an `IntGridLayer` reader any other way.

A `write` draws nothing, so the opaque-tile lock and occupancy don't apply
to it; a pattern rule with `write` fires once per anchor whatever variants
match.

## Occupancy

Each (level, auto layer) has one occupancy grid, in cells of the layer's
grid, all free when the bake starts and kept across passes.

- The **footprint** of a placement is every in-level cell whose center
  lies inside the half-open pixel box `[x0, x0 + tw) × [y0, y0 + th)` of at
  least one of its tiles (`x0` the tile's final `px`, after pivot,
  `tile_pivot`, the variant's turn, `offset_px` and `random_offset_px`;
  `tw × th` the tileset's tile size, swapped by a transpose). On a `g` px
  grid, cell `c` is in it when `2 x0 <= 2 c g + g < 2 (x0 + tw)`, integer
  math (`footprint_axis` in `bake.rs`). A 64 px wide stamp with pivot
  `[0, 1]` on a 32 px grid covers `x, x+1`; with pivot `[0.5, 1]` it covers
  `x-1, x`; a 32 px tile on a 16 px grid covers 2×2 cells; a 16 px tile on
  a 32 px grid covers its cell only if it reaches the center.
- With `avoid_overlap` (any output), a variant whose footprint has an
  occupied cell is skipped (nothing drawn); the next matching variant is
  tried. A rule all of whose matching variants were skipped did not fire
  there: no lock, no write, not in `fired` (previews list the anchor under
  `occupied`).
- A placement of a stamp with `occupies` marks its footprint after it is
  drawn (with or without `avoid_overlap`).
- Order is the sweep order: run order of rules, anchors row-major, then
  variants. A locked anchor is skipped before occupancy is looked at, a
  skipped placement doesn't lock; locks never mark occupancy, occupancy
  never locks.

## Symmetry variants

A rule tries its pattern as written plus the orientations its `symmetry`
enables, in this frozen order (the index feeds the hash streams, so it never
changes; 0-3 are what mirrors always were):

| v | Variant | Enabled by | Pattern cell `(x, y)` read at | Flip bits |
| --- | --- | --- | --- | --- |
| 0 | as written | always | `(x, y)` | 0 |
| 1 | mirror x | `mirror_x`, or `rotate` + any mirror | `(-x, y)` | 1 (`x`) |
| 2 | mirror y | `mirror_y`, or `rotate` + any mirror | `(x, -y)` | 2 (`y`) |
| 3 | both = half turn | both mirrors, or `rotate` | `(-x, -y)` | 3 (`xy`) |
| 4 | quarter turn clockwise | `rotate` | `(-y, x)` | 5 (`xt`) |
| 5 | quarter turn counterclockwise | `rotate` | `(y, -x)` | 6 (`yt`) |
| 6 | transpose | `rotate` + any mirror | `(y, x)` | 4 (`t`) |
| 7 | anti-transpose | `rotate` + any mirror | `(-y, -x)` | 7 (`xyt`) |

Offsets are from the anchor, y down. Flip bits are the export's: 4 =
transpose (swap x and y) applied first, then 1 mirrors left-right and 2
top-bottom. The bits *are* the offset map: transposing, then negating x for
bit 1 and y for bit 2, gives the column above. So `rotate` alone gives the
four turns, and `rotate` with any mirror all eight orientations.

The drawn tiles turn with the variant. Stamp layouts, `offset_px` and
`random_offset_px` (drawn as written, then turned) turn about the anchor
cell's center: each tile's box `[o, o + t)` from the cell's top-left is
transposed (x and y swap) for bit 4, then mirrored on an axis to
`[g - o - t, g - o)`. With tiles the size of a cell that is just turning the
offset vector. `rotate` needs square tiles (a turned tile only fits its own
square): a rule with `rotate` drawing from a tileset with non-square tiles,
its own or a level's substitute, fails the bake with
`Error::RotateNeedsSquareTiles` (validation reports it first).

## Incremental baking

`BakeCache` keeps the last bake of each (level, auto layer) and updates it
from the `Change`s that `Project::apply` returns:

```rust
let mut cache = worlds_rules::BakeCache::new();
cache.bake_level(&project, "crevasse", &images)?;          // once, at open
let applied = project.apply(cmd)?;
for (layer, rect) in cache.apply_changes(&project, "crevasse", &images, &applied.changes)? {
    // redraw `layer` over `rect` (cells of the layer's grid, max exclusive)
}
let bake = cache.get("crevasse", "main_tiling");
```

Why re-sweeping part of a layer is exact: whether a rule fires at an anchor,
what it places and whether it locks the cell depend only on the grid within
the rule's pattern radius of that anchor, on the anchor itself (every random
roll is keyed on it) and on earlier rules at the same anchor. So after cells
in a rect change, only anchors within the layer's largest pattern radius of
that rect can change. Those anchors are re-swept with `bake_rect` into fresh
per-rule buckets and spliced over the cached placements anchored there, rule
by rule (each rule's run of placements is sorted by anchor, which also makes
`placements_at(level, layer, cell)` a binary search). `fired` loses the
re-swept anchors and gains the fresh ones.

What triggers what (auto layers are processed in dependency order, so a
writer is up to date before its readers):

- `grid` on a layer the auto layer's rules read (`match`, `require` or
  `region` of any expanded rule, or its `source`): a partial update over the
  changed cells (see [Multi-pass layers](#multi-pass-layers) for layers using
  passes, regions, writes or occupancy; otherwise one re-sweep of all grid
  changes of the call merged into one bounding rect, grown by the largest
  pattern radius over `match`, `require` and every enabled orientation);
- `grid` on a derived layer D is an **override edit**: D's effective grid is
  recomputed and the cells where it changed are reported as `(D, rect)` and
  are an input of every layer reading D; D's writer also gets the raw edit
  when it reads D itself (its passes read the overrides directly);
- when a writer's computed values change, the cells where a derived grid's
  effective values changed are reported as `(D, rect)` and are an input of
  the layers reading it;
- `layer_state` of that layer, `level_meta` with part `size` or `all`,
  `level_created`, or a project `seed` change: full rebake;
- `level_meta` part `fields` and project key `level_fields`: a full rebake
  of the layers whose groups require a field whose value or definition
  changed;
- `level_meta` part `tags`: a full rebake of the layers with a group that
  has `requires_level_tag`, and of every layer depending on them;
- definition changes, which have no level and so count for every level
  the cache is asked about, each a full rebake of the layers it names **and
  every layer reading their derived grids, transitively** (no stroke
  needed): `def` kind `rules` with slug L: layer L; `macros`: every layer
  whose rules use `rules/macros.json` (`RuleSet::uses_macros`: a template
  reference, a named pattern or legend); `tileset` T: every layer that can
  draw from T (`Project::auto_layers_drawing_from`: rule outputs, stamps,
  template references and fixed template rules, level substitutions);
  `layer` X: X itself, every layer reading X and, for a derived X, its
  writer. Dependencies are the new definitions', so the derived grids a
  rebaked layer's cached bake wrote (and those a removed layer wrote) are
  worked out again from their writer now, or their overrides alone, and
  count by where they changed for their readers: a writer whose `write`
  went away leaves no stale grid or reader. `def_renamed` isn't handled:
  call `invalidate`, `invalidate_layer` or `clear` after it;
- `level_renamed` moves the cache entries (the hash keys on the level id, not
  the slug), `level_deleted` drops them; everything else is ignored.

A layer that needs work and reads a derived grid whose writer isn't cached
bakes that writer first (and reports it). The result lists each auto layer
that was rebaked with its rect, then each derived layer whose effective
values changed with the rect of those cells.

The grids a layer reads are worked out from its expanded rules once per
**rules fingerprint** (`rules_fingerprint(&project, layer)`: FNV-1a over the
JSON of the layer's rule file, `rules/macros.json` if it uses macros, the
layer's own definition, the int_grid layers its rules name (`source` and
every `match.grid`/`require.grid`/`region.grid`, templates included) and the
derived layers it writes (with who else writes them), the tilesets it can
draw from (outputs, stamps, references, fixed template rules, both sides of
every level's substitution for the layer), the project grid size, the
level field definitions its groups require, and the fingerprints of the
writers of the derived grids it reads, worked out with a visited set so a
cycle never recurses: a layer on a cycle covers only its own inputs there,
and fails to bake anyway; within one cache call each layer's is hashed
once, off a cycle). Level tags, like level field values, are in the
entry's level-values hash when a group requires a tag. A definition the layer doesn't read or draw from
(another layer, an entity type, an unrelated tileset) leaves it alone, so the
next stroke still re-sweeps instead of rebaking. Every cache entry records the fingerprint it
was baked under, and a partial re-sweep under a different one becomes a full
rebake: a rule edit nobody reported can make a stroke slower, never wrong.
`seed` records the current fingerprint, so a seeded bake must come from the
project as it is now (the editor drops worker results from before a newer
edit). `seed` refuses (returns false for) a layer using passes, region
rules, writes or occupancy: a `LayerBake` doesn't carry the per-pass state
the update needs. For those, a worker runs `BakeCache::bake_detached(&project,
level, &images) -> LevelSeed` (every auto layer of the level, in dependency
order, with the state; `LevelSeed::bakes()` and `derived()` to show them)
and the UI thread hands it to `seed_level(&project, seed) -> bool`, which
takes it whole when every layer's fingerprint still matches (else nothing,
so the next update rebakes what it needs). The derived grids' effective
values are then worked out again from the overrides as they are now; if
one changed and some layer reads it, the seed is older than the grids and
the level is dropped (false).

Forgetting: `invalidate(level)` (every layer of a level),
`invalidate_layer(layer)` (one layer everywhere),
`invalidate_level_layer(level, layer)` (one entry), `clear()`.

A layer whose rebake fails (say, its rules reference a pattern that was just
removed) doesn't stop the others: every other layer is still brought up to
date, and the failed one is left uncached, never with its bake from before
the change. `apply_changes` and `bake_level` then return the first error;
`apply_changes_per_layer` gives each layer's own outcome (the rect to
redraw, or the error), which is what the editor uses.

`level` is the slug after the changes; changes made under an earlier slug
(before a rename in the same list, or a chain of them) still count. A layer
with nothing cached is baked in full only when some change can affect it, so
handing over an entity edit doesn't bake an uncached level.

The returned rect covers the re-swept anchors and every tile drawn or removed
there, so stamps and offsets that overhang are included (it can reach past
the level edge).

### Multi-pass layers

A layer using several passes, region rules, `write` or occupancy keeps, per
(level, layer), each pass's writes, each region rule's candidates, the
occupancy after each occupancy pass and each rule's firings. An update gets
the changed cells per grid it reads, then goes through the passes in order:

1. The pass's grids are the level's (and upstream effective grids), with
   the layer's own derived grids rebuilt from the overrides and the updated
   writes of the earlier passes.
2. **Dirty anchors**: the changed cells of the grids the pass reads (the
   call's inputs, plus the cells where an earlier pass's writes changed in
   this update), grown by the pass's largest pattern radius; plus, for each
   region rule whose grid has changed cells, the symmetric difference
   between its cached candidates and a fresh analysis of the whole level
   (O(cells); an untouched region gives the same candidates, merges and
   splits included).
3. A pass using occupancy (`avoid_overlap`, or a stamp that `occupies`) is
   re-swept **whole** when it has any dirty anchor or the occupancy entering
   it changed; any other pass re-sweeps the bounding rect of its dirty
   anchors, or nothing.
4. The pass's writes in the re-swept rect are diffed against the cache: the
   changed cells feed the later passes; a changed occupancy grid makes the
   later occupancy passes re-sweep whole.

Then placements and firings are spliced per re-swept pass, `fired` is
rebuilt over the re-swept rects from the firings, and `derived` and
`occupied` are replaced. Stamps that avoid each other belong in a small pass
of their own, since that pass re-sweeps whole.

`tests/incremental.rs` checks the cache against fresh bakes after every
step of seeded random edit sequences (paints, fills, flood fills, value
replacement, layer seeds, the project seed, moves) on all the sample
fixtures and a two-level project built from one, and that every changed tile lies inside the reported rect; on a
synthetic project with templates, `require` and `rotate`, after random rule
edits (tiles, add, remove, move), macro edits and tileset edits, that the
cache matches and reports exactly the layers the definition touches; and
with passes, region rules, writes and occupancy, on a synthetic project of
two layers using all of them (a reader listed before the writer):
collision strokes and fills, override paints
(values, `!`, `null`), level tags, rule edits on the writer with no stroke
after, tileset and derived-layer definition edits, layer and project seeds.
After every step each auto layer's `LayerBake` (placements, `fired`,
`derived`, `occupied`) equals a fresh `bake_level`, each derived layer's
effective grid equals `bake_for_export`'s, and every changed tile and
derived cell is inside the reported rect.

**Draw order.** First drawn is at the bottom. Passes stack bottom to top;
within a pass, rules earlier in the file end up on top: the list
runs from the pass's last rule up to its first rule. Each rule's tiles come
in anchor row-major order, then by variant (in the order of the table
above), then by the alternative's tile order.

## Groups

An `off` group never runs. Otherwise a group whose `requires_level_tag`
(non-empty `tags`) fails in the level (`any`: none of its tags on the
level; `all`: one missing) never runs. Otherwise a group with a non-empty
`requires_level_field`, in a level that has the field, runs exactly when the
requirement holds (`any` or `all`, scalar or array value, falling back to the
field default when the value is missing or null). That answer is final.
Otherwise a holding `requires_level_tag` runs the group (an `optional` one
too, without being listed). Otherwise an `always` group runs, and an
`optional` group runs only if the level lists it in `optional_groups`.

## Stamps

In stamp mode (`tile_mode: "stamp"`, or a named stamp) an alternative's tiles
keep their atlas layout. The layout steps by the tileset's `tile_size`; the
tileset's `spacing` only changes where tiles are cut from the image. For a
stamp whose atlas bounds are `left..=right`, each tile's offset from the
anchor cell's left edge is

```
(col - left) * tile_w + pivot_x * (grid - (right - left + 1) * tile_w) + tile_pivot_x * grid
```

floored, and the same for y. That is: the stamp's pixel box is placed so its
`pivot` point lands on the same point of the anchor cell, then the layer's
`tile_pivot` shifts it by a fraction of a cell. With `tile_w == grid` this
reduces to `((col - left) - pivot * (right - left) + tile_pivot) * grid`.

A mirrored or rotated variant turns the layout about the anchor cell's
center (see [Symmetry variants](#symmetry-variants)): mirrored, a tile
spanning `[o, o + tile_w)` from the cell's left edge lands on
`[grid - o - tile_w, grid - o)`, offsets included. With tiles the size of a
cell that is just `-o`.

## The break rule

Besides occupancy and writes read by a later pass, this is the only way
rules affect each other. At an anchor cell, walking the rules of a pass in
run order, a rule that fired there **locks** the cell for the rest of the
pass if either:

- it has `break`, even if the chosen alternative is empty (a `write` rule
  with `break` locks too); or
- it has no `offset_px`, no `random_offset_px`, `alpha >= 1`, and at least
  one of its tiles there is fully opaque in the tileset image.

Every later rule of the pass skips a locked anchor. Locks are by anchor, not
by where tiles land, and a pass starts with none.

## Randomness

Every random choice is `hash::hash(layer_seed, rule_key, x, y, stream)`:
splitmix64's mixer chained over the inputs. It's pure integer math, so it's
stable across platforms and Rust versions.

- `layer_seed` (`hash::layer_seed`) mixes `project.seed`, `level.id`, the
  layer slug and the level's optional `layers.<layer>.seed` (default 0).
- `rule_key` (`hash::rule_key`) mixes the rule's `slug` and its optional
  `salt` (default 0). Two rules never share rolls, and renaming a rule
  re-rolls it; `salt` exists only to re-roll without renaming (the LDtk
  importer fills it with the source rule's uid).

So neither per-level seeds nor salts are needed for distinct, stable
randomness. Only the anchor cell feeds the hash, so a one-cell edit can only
change anchors whose pattern window covers it.

| Stream | Id | Use |
| --- | --- | --- |
| `CHANCE` | `0x01` | `chance` roll, one per cell, shared by variants |
| `PERLIN` | `0x02` | Perlin permutation seed (rule key mixed with `perlin.seed`, x = y = 0) |
| `ALT` + v | `0x10`-`0x17` | weighted alternative pick for variant v |
| `OFFSET_X` + v | `0x20`-`0x27` | `random_offset_px.x` for variant v |
| `OFFSET_Y` + v | `0x30`-`0x37` | `random_offset_px.y` for variant v |
| `REGION_PHASE` | `0x40` | region rule, `random` spacing: the first position's phase, at the run's first cell after margin |
| `REGION_STEP` | `0x41` | region rule, `random` spacing: the jitter after position `p`, at `p`'s cell |

Variant v is the index in the [symmetry table](#symmetry-variants) (0 as
written, 1 mirror_x, 2 mirror_y, 3 both, 4-7 the rotations). Each base owns
16 ids, so the streams never collide. Perlin is an
in-crate 2D gradient noise (`perlin.rs`). The rule fires where the noise at
`(x * scale, y * scale)` is `>= 0`.

## Parity with imported LDtk projects

`tests/parity.rs` imports every LDtk sample fixture, bakes every auto layer
of every level and compares with the bake saved in the `.ldtk` file: same
`(anchor, rule)` pairs and exact tiles for deterministic rules, same pairs
and flips with each placement a valid alternative for multi-alternative
rules, and for chance/perlin rules every anchor LDtk fired on must match the
pattern here. Run with `--nocapture` for the summary table. The rule
semantics it checks are written up in
[docs/ldtk_parity.md](../../docs/ldtk_parity.md).

The fixtures include `Test_file_for_API_showing_all_features.ldtk`, whose
auto layers bake on 16 px grids inside an 8 px project.

Where an imported project can bake differently (none of these occur in the
fixtures):

- Randomness (alternative picks, random offsets, chance, perlin) uses
  Worlds' hash, not LDtk's.
- Stamp layouts step by the tileset's tile size, LDtk's by the layer grid
  (the same when they're equal, as in every fixture).
- Stamp offsets are floored, LDtk truncates toward zero. They differ only for
  a negative fractional offset (odd grid size, half-cell pivot), where
  truncation makes neighbouring stamp tiles overlap by a pixel.
- A variant that samples exactly like an earlier one is dropped, so a
  point-symmetric pattern with both mirrors doesn't stack a flipped copy
  (LDtk's `tidy()` only catches the X- and Y-symmetric cases).
- LDtk's `isRelevantInLayerAt` pruning, which can drop matches that rely on
  `out_of_bounds` when the value is absent from the layer, is not copied.

`tests/fixture_bakes.rs` holds every fixture (each LDtk sample and
worlds-core's `mini.worlds`) to a digest of its bake (`fixture_bakes.txt`;
imported projects get a fixed seed and level ids, since the importer draws
random ones), so an engine change that alters an existing bake fails it.

## Benchmarks

`cargo bench -p worlds-rules` (criterion, release, one thread; `benches/bake.rs`).
Measured on the development machine, 2026-10-22:

| Bench | What | Time |
| --- | --- | --- |
| `stroke_64x28_50_rules` | a 20-cell paint: `Project::apply` + `BakeCache::apply_changes`, 64x28 room, 50 rules (mirrors, some `rotate`, chance, two alternatives) | 305 µs |
| `blob47_64x28` | full bake of one `blob47` reference (47 rules) on a 64x28 mask | 1.12 ms |
| `rebake_500_levels` | full bake of 500 synthetic 64x28 levels, the 50 rules, sequential | 1.011 s |
| `stroke_64x28_50_rules_regions` | the same stroke with 46 of the rules in pass 0, two region rules (floor, interior) writing a derived grid, two pass-1 stamps with `occupies`/`avoid_overlap` reading it | 1.02 ms |

Against DESIGN's targets: a stroke is 0.31 ms of the 2 ms budget, 1.0 ms
with a region pass and an occupancy pass (the occupancy pass re-sweeps
whole, the region rules flood-fill the level). 500 levels take 1.01 s on
one thread, so about 0.13 s on an 8-thread worker pool (sequential / 8, an
estimate), inside the 1 s target.

## Not supported yet

Region rules and `write` in templates are not in format 1 (a load error in
`rules/macros.json`).
