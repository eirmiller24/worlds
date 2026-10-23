//! CPU side of the map renderer: per-chunk instance lists.
//!
//! Every drawable layer (`int_grid`, `tiles`, `auto`) of every level is cut
//! into chunks of [`CHUNK_CELLS`] x [`CHUNK_CELLS`] cells of that layer's
//! grid. A chunk holds the instances (one textured quad each) for what sits
//! in it, in draw order, split into *runs* of consecutive instances that
//! share a texture: one instanced draw per run. With one tileset per layer
//! that is one draw per chunk per tileset; a layer that interleaves tilesets
//! keeps its exact draw order at the cost of extra runs.
//!
//! Chunks start dirty and are rebuilt lazily, only when they are about to
//! be drawn ([`Scene::collect_visible`]). [`Scene::invalidate`] marks the
//! chunks a cell rect touches dirty again. Each rebuild bumps the chunk's
//! `version`, which the GPU side compares to decide whether to re-upload.
//!
//! Draw order across chunks: the chunks of a layer split the plane into
//! disjoint shares (each chunk's cells, with the outer chunks extended to
//! infinity past the level's edges). A tile or auto placement whose quad
//! overlaps several shares (a 32 px tile on a 16 px grid at a chunk edge, a
//! rule offset) is drawn in each of them, clipped to that share, and every
//! chunk keeps the layer's global placement order. Since no two chunks of a
//! layer draw the same pixel, the order chunks are drawn in can't change
//! the stacking: the layer looks exactly as if drawn in one list.
//!
//! Culling: each layer's extent (the union of its placements' quads, or
//! the level rect for int_grid) is computed from the actual placements
//! after every invalidation. A level is skipped when no visible layer's
//! extent meets the view, and a chunk when its share of that extent
//! doesn't; then each rebuilt chunk's instance bounds are tested exactly.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use bytemuck::{Pod, Zeroable};
use worlds_core::{Flip, LayerKind, Override, Slug, TilesetDef};

use super::camera::WRect;
use crate::doc::{Document, LevelIx};

static NEXT_VERSION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Chunk edge length in cells.
pub const CHUNK_CELLS: u32 = 32;

/// Stands in for infinity in level-local pixel rects.
const FAR: i64 = 1 << 40;

/// Texture slot of the 1x1 white texture (IntGrid quads, solid fills).
pub const TEX_WHITE: u32 = 0;
/// Texture slot drawn for tiles whose tileset is unknown or failed to load.
pub const TEX_MISSING: u32 = 1;

/// [`Instance::layer`] bit: the tile is drawn transposed (flip bit 4).
pub const TRANSPOSED: u32 = 1 << 31;

/// Texture slot of project tileset `i` (`project.tilesets[i]`).
pub fn tileset_tex(i: usize) -> u32 {
    i as u32 + 2
}

/// One quad. Positions are world pixels; `uv` is `[u0, v0, u1, v1]` in
/// texels of the run's texture (swapped for flips; the shader normalizes).
/// `color` is straight (not premultiplied) RGBA that multiplies the texel.
/// `layer` indexes the per-layer opacity table in the uniforms; bit 31
/// ([`TRANSPOSED`]) says the tile is transposed: the shader then reads `u`
/// along the quad's height and `v` along its width (see `tile_quad`).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Pod, Zeroable)]
pub struct Instance {
    pub pos: [f32; 2],
    pub size: [f32; 2],
    pub uv: [f32; 4],
    pub color: [u8; 4],
    pub layer: u32,
}

/// Consecutive instances `start..end` that draw with texture slot `tex`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Run {
    pub tex: u32,
    pub start: u32,
    pub end: u32,
}

#[derive(Debug)]
pub struct Chunk {
    /// Unique across the scene; the GPU buffer slot.
    pub id: u32,
    pub instances: Vec<Instance>,
    pub runs: Vec<Run>,
    /// World-pixel bounds of the instances (empty when there are none).
    pub bounds: WRect,
    pub dirty: bool,
    /// Bumped on every rebuild.
    pub version: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrawKind {
    IntGrid,
    Tiles,
    Auto,
}

#[derive(Debug)]
pub struct LayerChunks {
    /// Index in `project.layers`.
    pub layer: usize,
    pub slug: Slug,
    pub kind: DrawKind,
    /// A derived int_grid layer: drawn from its effective grid (see
    /// [`Scene::rebuild_chunk`]), so it follows its writer's bake.
    pub derived: bool,
    /// Cell size in pixels.
    pub grid: u32,
    /// Level size in cells of this layer's grid.
    pub cells: [u32; 2],
    pub chunks_x: u32,
    pub chunks_y: u32,
    pub chunks: Vec<Chunk>,
    /// Tiles/auto layers: placement indices per chunk (every chunk whose
    /// share the placement's quad overlaps, in placement order), built on
    /// first use and dropped whenever the layer is invalidated.
    buckets: Option<Vec<Vec<u32>>>,
    /// World rect the layer draws in (see the module docs); `None` until
    /// computed, and again after an invalidation.
    extent: Option<WRect>,
    /// How many cells right and down a tile anchored in a cell can reach
    /// (tiles larger than the cell).
    overhang: i32,
}

impl LayerChunks {
    /// The chunk holding cell `[x, y]` (clamped into the grid).
    pub fn chunk_of_cell(&self, x: i64, y: i64) -> usize {
        let cx = (x.div_euclid(CHUNK_CELLS as i64)).clamp(0, self.chunks_x as i64 - 1);
        let cy = (y.div_euclid(CHUNK_CELLS as i64)).clamp(0, self.chunks_y as i64 - 1);
        (cy * self.chunks_x as i64 + cx) as usize
    }

    fn chunk_xy(&self, px: i64, axis: usize) -> u32 {
        let span = (CHUNK_CELLS * self.grid.max(1)) as i64;
        let n = if axis == 0 {
            self.chunks_x
        } else {
            self.chunks_y
        };
        px.div_euclid(span).clamp(0, n as i64 - 1) as u32
    }

    /// Level-local pixel rect `[x0, y0, x1, y1)` of chunk `k`'s share of
    /// the plane: its cells, extended to infinity on the sides that are the
    /// level's edges. The shares of a layer's chunks don't overlap.
    pub fn chunk_share(&self, k: usize) -> [i64; 4] {
        let span = (CHUNK_CELLS * self.grid.max(1)) as i64;
        let cx = (k as u32 % self.chunks_x) as i64;
        let cy = (k as u32 / self.chunks_x) as i64;
        let lo = |c: i64| if c == 0 { -FAR } else { c * span };
        let hi = |c: i64, n: u32| {
            if c == n as i64 - 1 {
                FAR
            } else {
                (c + 1) * span
            }
        };
        [lo(cx), lo(cy), hi(cx, self.chunks_x), hi(cy, self.chunks_y)]
    }

    /// Chunks (inclusive `[cx0, cy0, cx1, cy1]`) whose share overlaps the
    /// level-local pixel rect `[x0, y0, x1, y1)`; `None` if it is empty.
    fn chunks_overlapping(&self, r: [i64; 4]) -> Option<[u32; 4]> {
        (r[0] < r[2] && r[1] < r[3]).then(|| {
            [
                self.chunk_xy(r[0], 0),
                self.chunk_xy(r[1], 1),
                self.chunk_xy(r[2] - 1, 0),
                self.chunk_xy(r[3] - 1, 1),
            ]
        })
    }

    fn key(&self) -> LayerKey {
        LayerKey {
            layer: self.layer,
            kind: self.kind,
            grid: self.grid,
        }
    }

    /// Cell range `[x0, y0, x1, y1)` of chunk `k`.
    pub fn chunk_cells(&self, k: usize) -> [u32; 4] {
        let cx = k as u32 % self.chunks_x;
        let cy = k as u32 / self.chunks_x;
        let x0 = cx * CHUNK_CELLS;
        let y0 = cy * CHUNK_CELLS;
        [
            x0,
            y0,
            (x0 + CHUNK_CELLS).min(self.cells[0]),
            (y0 + CHUNK_CELLS).min(self.cells[1]),
        ]
    }
}

#[derive(Debug)]
pub struct LevelScene {
    /// World-pixel rect of the level.
    pub rect: WRect,
    pub layers: Vec<LayerChunks>,
}

/// What [`Scene::quad`] needs of a layer.
#[derive(Debug, Clone, Copy)]
struct LayerKey {
    layer: usize,
    kind: DrawKind,
    grid: u32,
}

/// A quad in level-local pixels, before clipping.
#[derive(Debug, Clone, Copy)]
struct Quad {
    /// Level-local top-left pixel.
    at: [i64; 2],
    size: [f32; 2],
    uv: [f32; 4],
    tex: u32,
    alpha: f64,
    /// Transposed (flip bit 4): `u` runs down the quad, `v` across it.
    transposed: bool,
}

impl Quad {
    fn rect(&self) -> [i64; 4] {
        [
            self.at[0],
            self.at[1],
            self.at[0] + self.size[0] as i64,
            self.at[1] + self.size[1] as i64,
        ]
    }

    /// The part inside the level-local rect `clip`, as an instance at world
    /// offset `origin`. UVs are cut in proportion (they may be swapped for
    /// flips; interpolating from the first corner handles that). A
    /// transposed quad is cut in its own frame: its `u` follows the quad's
    /// y and its `v` the quad's x, as the shader reads them.
    fn clipped(&self, clip: [i64; 4], origin: [f32; 2], layer: u32) -> Option<Instance> {
        let r = self.rect();
        let (x0, y0) = (r[0].max(clip[0]), r[1].max(clip[1]));
        let (x1, y1) = (r[2].min(clip[2]), r[3].min(clip[3]));
        if x0 >= x1 || y0 >= y1 {
            return None;
        }
        let [u0, v0, u1, v1] = self.uv;
        let fx = |x: i64| (x - r[0]) as f32 / self.size[0];
        let fy = |y: i64| (y - r[1]) as f32 / self.size[1];
        // The fractions of the quad's width and height the clip keeps, in
        // the order u and v run along them.
        let (fu, fv) = if self.transposed {
            ([fy(y0), fy(y1)], [fx(x0), fx(x1)])
        } else {
            ([fx(x0), fx(x1)], [fy(y0), fy(y1)])
        };
        Some(Instance {
            pos: [origin[0] + x0 as f32, origin[1] + y0 as f32],
            size: [(x1 - x0) as f32, (y1 - y0) as f32],
            uv: [
                u0 + (u1 - u0) * fu[0],
                v0 + (v1 - v0) * fv[0],
                u0 + (u1 - u0) * fu[1],
                v0 + (v1 - v0) * fv[1],
            ],
            color: [255, 255, 255, alpha_u8(self.alpha)],
            layer: if self.transposed {
                layer | TRANSPOSED
            } else {
                layer
            },
        })
    }
}

/// A chunk to draw this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkRef {
    pub level: LevelIx,
    /// Index into `LevelScene::layers`.
    pub layer: usize,
    pub chunk: usize,
}

/// Counters from the last [`Scene::collect_visible`].
#[derive(Debug, Clone, Copy, Default)]
pub struct CollectStats {
    pub rebuilt: usize,
    pub rebuild_time: Duration,
    /// Dirty visible chunks left for a later frame (budget ran out).
    pub pending: usize,
}

#[derive(Debug, Clone)]
struct TilesetInfo {
    tex: u32,
    def: TilesetDef,
}

/// All chunks of a document.
#[derive(Debug)]
pub struct Scene {
    pub levels: Vec<LevelScene>,
    /// Level indices sorted by depth (back to front), stable.
    pub draw_order: Vec<LevelIx>,
    tilesets: HashMap<Slug, TilesetInfo>,
    /// Per project layer: RGBA of each IntGrid value (index - 1).
    value_colors: Vec<Vec<[u8; 4]>>,
    next_id: u32,
    /// Chunk ids dropped by [`Scene::reset_level`]; the viewport frees
    /// their GPU buffers.
    pub retired: Vec<u32>,
}

/// Parse `#rrggbb` / `#rrggbbaa`.
pub fn parse_color(s: &str) -> [u8; 4] {
    let hex = s.trim_start_matches('#');
    let byte = |i: usize| {
        hex.get(i..i + 2)
            .and_then(|h| u8::from_str_radix(h, 16).ok())
            .unwrap_or(255)
    };
    [
        byte(0),
        byte(2),
        byte(4),
        if hex.len() >= 8 { byte(6) } else { 255 },
    ]
}

impl Scene {
    pub fn new(doc: &Document) -> Self {
        let p = &doc.project;
        let tilesets: HashMap<Slug, TilesetInfo> = p
            .tilesets
            .iter()
            .enumerate()
            .map(|(i, t)| {
                (
                    t.slug.clone(),
                    TilesetInfo {
                        tex: tileset_tex(i),
                        def: t.clone(),
                    },
                )
            })
            .collect();
        let value_colors = p
            .layers
            .iter()
            .map(|l| {
                l.values()
                    .iter()
                    .map(|v| parse_color(v.color.as_str()))
                    .collect()
            })
            .collect();
        let mut scene = Scene {
            levels: Vec::with_capacity(doc.levels.len()),
            draw_order: Vec::new(),
            tilesets,
            value_colors,
            next_id: 0,
            retired: Vec::new(),
        };
        for ix in 0..doc.levels.len() {
            let ls = scene.level_scene(doc, ix);
            scene.levels.push(ls);
        }
        scene.sort_levels(doc);
        scene
    }

    fn sort_levels(&mut self, doc: &Document) {
        self.draw_order = (0..doc.levels.len()).collect();
        self.draw_order.sort_by_key(|&i| doc.level(i).depth);
    }

    fn level_scene(&mut self, doc: &Document, ix: LevelIx) -> LevelScene {
        self.level_scene_with(doc, ix, true)
    }

    /// The level's chunk grid; with `ids`, chunks get fresh ids (else 0).
    fn level_scene_with(&mut self, doc: &Document, ix: LevelIx, ids: bool) -> LevelScene {
        let p = &doc.project;
        let level = doc.level(ix);
        let [x, y, w, h] = doc.level_rect(ix);
        let mut layers = Vec::new();
        let max_tile = p
            .tilesets
            .iter()
            .map(|t| t.tile_size[0].max(t.tile_size[1]))
            .max()
            .unwrap_or(16)
            .max(16);
        for (li, def) in p.layers.iter().enumerate() {
            let kind = match def.kind {
                LayerKind::IntGrid(_) => DrawKind::IntGrid,
                LayerKind::Tiles(_) => DrawKind::Tiles,
                LayerKind::Auto(_) => DrawKind::Auto,
                LayerKind::Entities => continue,
            };
            let cells = p.layer_cells(def, level);
            let chunks_x = cells[0].div_ceil(CHUNK_CELLS).max(1);
            let chunks_y = cells[1].div_ceil(CHUNK_CELLS).max(1);
            let chunks = (0..chunks_x * chunks_y)
                .map(|_| {
                    let id = self.next_id;
                    if ids {
                        self.next_id += 1;
                    }
                    Chunk {
                        id,
                        instances: Vec::new(),
                        runs: Vec::new(),
                        bounds: WRect::EMPTY,
                        dirty: true,
                        version: 0,
                    }
                })
                .collect();
            let grid = p.layer_grid(def);
            layers.push(LayerChunks {
                layer: li,
                slug: def.slug.clone(),
                kind,
                derived: def.is_derived(),
                grid,
                cells,
                chunks_x,
                chunks_y,
                chunks,
                buckets: None,
                extent: None,
                overhang: (max_tile.div_ceil(grid.max(1)) as i32 - 1).max(0),
            });
        }
        LevelScene {
            rect: WRect::from_pos_size([x, y], [w, h]),
            layers,
        }
    }

    /// Total chunk ids handed out (GPU slot table size).
    pub fn chunk_id_count(&self) -> u32 {
        self.next_id
    }

    /// Rebuild a level's chunk structure (after a move, a resize, or a
    /// depth change); every chunk is dirty. When the chunk grid kept its
    /// shape (a move), the chunks keep their ids and GPU buffers; otherwise
    /// the old ids go to [`Scene::retired`].
    pub fn reset_level(&mut self, doc: &Document, ix: LevelIx) {
        let mut ls = self.level_scene_with(doc, ix, false);
        let old = &mut self.levels[ix];
        let same = old.layers.len() == ls.layers.len()
            && old
                .layers
                .iter()
                .zip(&ls.layers)
                .all(|(a, b)| a.slug == b.slug && a.chunks.len() == b.chunks.len());
        if same {
            for (a, b) in old.layers.iter().zip(&mut ls.layers) {
                for (ca, cb) in a.chunks.iter().zip(&mut b.chunks) {
                    cb.id = ca.id;
                }
            }
        } else {
            self.retired.extend(
                old.layers
                    .iter()
                    .flat_map(|l| l.chunks.iter().map(|c| c.id)),
            );
            for c in ls.layers.iter_mut().flat_map(|l| l.chunks.iter_mut()) {
                c.id = self.next_id;
                self.next_id += 1;
            }
        }
        self.levels[ix] = ls;
        self.sort_levels(doc);
    }

    /// Mark the chunks of `layer` in level `ix` that overlap the cell rect
    /// `[x0, y0, x1, y1)` (cells of that layer's grid) dirty. Unknown layers
    /// and empty rects do nothing.
    pub fn invalidate(&mut self, ix: LevelIx, layer: &str, rect: [i32; 4]) {
        let Some(lc) = self
            .levels
            .get_mut(ix)
            .and_then(|l| l.layers.iter_mut().find(|lc| lc.slug == layer))
        else {
            return;
        };
        if rect[0] >= rect[2] || rect[1] >= rect[3] {
            return;
        }
        lc.buckets = None;
        lc.extent = None;
        // A tile anchored in the rect may draw into chunks to its right and
        // below (see the module docs).
        let grow = if lc.kind == DrawKind::IntGrid {
            0
        } else {
            lc.overhang as i64
        };
        let a = lc.chunk_of_cell(rect[0] as i64, rect[1] as i64);
        let b = lc.chunk_of_cell(rect[2] as i64 - 1 + grow, rect[3] as i64 - 1 + grow);
        let (ax, ay) = (a as u32 % lc.chunks_x, a as u32 / lc.chunks_x);
        let (bx, by) = (b as u32 % lc.chunks_x, b as u32 / lc.chunks_x);
        for cy in ay..=by {
            for cx in ax..=bx {
                lc.chunks[(cy * lc.chunks_x + cx) as usize].dirty = true;
            }
        }
    }

    /// Mark every chunk of one layer in one level dirty. An auto layer's
    /// rebake may also change what the derived layers show (their computed
    /// values come from their writer's bake): they are redrawn too.
    pub fn invalidate_layer(&mut self, ix: LevelIx, layer: &str) {
        let Some(l) = self.levels.get_mut(ix) else {
            return;
        };
        let auto = l
            .layers
            .iter()
            .any(|lc| lc.slug == layer && lc.kind == DrawKind::Auto);
        for lc in l
            .layers
            .iter_mut()
            .filter(|lc| lc.slug == layer || (auto && lc.derived))
        {
            lc.buckets = None;
            lc.extent = None;
            lc.chunks.iter_mut().for_each(|c| c.dirty = true);
        }
    }

    /// Mark every chunk of a level dirty.
    pub fn invalidate_level(&mut self, ix: LevelIx) {
        if let Some(l) = self.levels.get_mut(ix) {
            for lc in &mut l.layers {
                lc.buckets = None;
                lc.extent = None;
                lc.chunks.iter_mut().for_each(|c| c.dirty = true);
            }
        }
    }

    /// Mark the `auto` layers of a level dirty (its bake changed), and the
    /// derived layers, whose computed values come with it.
    pub fn invalidate_auto(&mut self, ix: LevelIx) {
        if let Some(l) = self.levels.get_mut(ix) {
            for lc in l
                .layers
                .iter_mut()
                .filter(|lc| lc.kind == DrawKind::Auto || lc.derived)
            {
                lc.buckets = None;
                lc.extent = None;
                lc.chunks.iter_mut().for_each(|c| c.dirty = true);
            }
        }
    }

    pub fn chunk(&self, r: ChunkRef) -> &Chunk {
        &self.levels[r.level].layers[r.layer].chunks[r.chunk]
    }

    /// The world rect a chunk can draw in: its share of the plane cut to
    /// the layer's extent (empty if it draws nothing).
    pub fn chunk_area(&self, r: ChunkRef) -> WRect {
        let ls = &self.levels[r.level];
        let lc = &ls.layers[r.layer];
        let Some(ext) = lc.extent else {
            return WRect::EMPTY;
        };
        let s = lc.chunk_share(r.chunk);
        let o = ls.rect.min;
        WRect::new(
            [
                ext.min[0].max(o[0] + s[0] as f64),
                ext.min[1].max(o[1] + s[1] as f64),
            ],
            [
                ext.max[0].min(o[0] + s[2] as f64),
                ext.max[1].min(o[1] + s[3] as f64),
            ],
        )
    }

    /// How many placements a tiles or auto layer has (0 for int_grid).
    fn quad_count(doc: &Document, ix: LevelIx, lc: LayerKey) -> usize {
        let slug = doc.project.layers[lc.layer].slug.as_str();
        match lc.kind {
            DrawKind::Tiles => doc.level(ix).tiles_layer(slug).map_or(0, |t| t.tiles.len()),
            DrawKind::Auto => doc.bakes[ix]
                .as_ref()
                .and_then(|b| b.get(slug))
                .map_or(0, |b| b.placements.len()),
            DrawKind::IntGrid => 0,
        }
    }

    /// The quad of placement `i` of a tiles or auto layer.
    fn quad(
        tilesets: &HashMap<Slug, TilesetInfo>,
        doc: &Document,
        ix: LevelIx,
        lc: LayerKey,
        i: usize,
    ) -> Option<Quad> {
        let slug = doc.project.layers[lc.layer].slug.as_str();
        let grid = lc.grid as i64;
        match lc.kind {
            DrawKind::Tiles => {
                let tp = doc.level(ix).tiles_layer(slug)?.tiles.get(i)?;
                let (tex, size, uv, transposed) =
                    tile_quad(tilesets, &tp.tileset, tp.tile, tp.flip);
                Some(Quad {
                    at: [tp.cell[0] as i64 * grid, tp.cell[1] as i64 * grid],
                    size,
                    uv,
                    tex,
                    alpha: tp.alpha,
                    transposed,
                })
            }
            DrawKind::Auto => {
                let pl = doc.bakes[ix].as_ref()?.get(slug)?.placements.get(i)?;
                let (tex, size, uv, transposed) =
                    tile_quad(tilesets, &pl.tileset, pl.tile, pl.flip);
                Some(Quad {
                    at: [pl.px[0] as i64, pl.px[1] as i64],
                    size,
                    uv,
                    tex,
                    alpha: pl.alpha,
                    transposed,
                })
            }
            DrawKind::IntGrid => None,
        }
    }

    /// Compute the extent of every layer of a level that lost it; returns
    /// the union of the extents of the layers `layer_visible` accepts.
    fn level_extent(
        &mut self,
        doc: &Document,
        ix: LevelIx,
        layer_visible: &impl Fn(usize) -> bool,
    ) -> WRect {
        let ls = &mut self.levels[ix];
        let o = ls.rect.min;
        let mut out = WRect::EMPTY;
        for lc in &mut ls.layers {
            if lc.extent.is_none() {
                let ext = if lc.kind == DrawKind::IntGrid {
                    ls.rect
                } else {
                    let mut e = WRect::EMPTY;
                    for i in 0..Self::quad_count(doc, ix, lc.key()) {
                        let Some(q) = Self::quad(&self.tilesets, doc, ix, lc.key(), i) else {
                            continue;
                        };
                        let r = q.rect();
                        e = e.union(&WRect::new(
                            [o[0] + r[0] as f64, o[1] + r[1] as f64],
                            [o[0] + r[2] as f64, o[1] + r[3] as f64],
                        ));
                    }
                    e
                };
                lc.extent = Some(ext);
            }
            if layer_visible(lc.layer) {
                out = out.union(&lc.extent.unwrap_or(WRect::EMPTY));
            }
        }
        out
    }

    /// The chunks to draw for world rect `view`, in draw order (levels back
    /// to front, layers bottom to top), into `out` (cleared first). Dirty
    /// chunks among them are rebuilt until `budget` runs out; the rest stay
    /// dirty and are skipped this frame (counted in `pending`).
    pub fn collect_visible(
        &mut self,
        doc: &Document,
        view: &WRect,
        layer_visible: impl Fn(usize) -> bool,
        budget: Option<Duration>,
        out: &mut Vec<ChunkRef>,
    ) -> CollectStats {
        out.clear();
        let mut stats = CollectStats::default();
        let start = Instant::now();
        for oi in 0..self.draw_order.len() {
            let ix = self.draw_order[oi];
            if !self.level_extent(doc, ix, &layer_visible).intersects(view) {
                continue;
            }
            for j in 0..self.levels[ix].layers.len() {
                if !layer_visible(self.levels[ix].layers[j].layer) {
                    continue;
                }
                for k in 0..self.levels[ix].layers[j].chunks.len() {
                    let r = ChunkRef {
                        level: ix,
                        layer: j,
                        chunk: k,
                    };
                    if !self.chunk_area(r).intersects(view) {
                        continue;
                    }
                    if self.chunk(r).dirty {
                        // At least one rebuild per frame, so a tiny budget
                        // still makes progress.
                        if stats.rebuilt > 0 && budget.is_some_and(|b| start.elapsed() > b) {
                            stats.pending += 1;
                            continue;
                        }
                        let t = Instant::now();
                        self.rebuild_chunk(doc, r);
                        stats.rebuild_time += t.elapsed();
                        stats.rebuilt += 1;
                    }
                    let c = self.chunk(r);
                    if !c.instances.is_empty() && c.bounds.intersects(view) {
                        out.push(r);
                    }
                }
            }
        }
        stats
    }

    /// Regenerate one chunk's instances from the document.
    pub fn rebuild_chunk(&mut self, doc: &Document, r: ChunkRef) {
        let p = &doc.project;
        let level = doc.level(r.level);
        let origin = self.levels[r.level].rect.min;
        // Process-wide, so a rebuilt scene (same chunk ids) never matches
        // a version still cached on the GPU.
        let version = NEXT_VERSION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        // Split borrows: the layer's buckets may need (re)building first.
        let lc = &mut self.levels[r.level].layers[r.layer];
        let def = &p.layers[lc.layer];
        let layer_ix = lc.layer as u32;
        let grid = lc.grid as f32;
        let ox = origin[0] as f32;
        let oy = origin[1] as f32;
        let [x0, y0, x1, y1] = lc.chunk_cells(r.chunk);

        if lc.buckets.is_none() && lc.kind != DrawKind::IntGrid {
            let mut buckets = vec![Vec::new(); lc.chunks.len()];
            for i in 0..Self::quad_count(doc, r.level, lc.key()) {
                let Some(q) = Self::quad(&self.tilesets, doc, r.level, lc.key(), i) else {
                    continue;
                };
                if let Some([cx0, cy0, cx1, cy1]) = lc.chunks_overlapping(q.rect()) {
                    for cy in cy0..=cy1 {
                        for cx in cx0..=cx1 {
                            buckets[(cy * lc.chunks_x + cx) as usize].push(i as u32);
                        }
                    }
                }
            }
            lc.buckets = Some(buckets);
        }
        let share = lc.chunk_share(r.chunk);
        let key = lc.key();

        let chunk = &mut lc.chunks[r.chunk];
        chunk.instances.clear();
        chunk.runs.clear();
        chunk.bounds = WRect::EMPTY;
        let push = |chunk: &mut Chunk, inst: Instance, tex: u32| {
            let n = chunk.instances.len() as u32;
            match chunk.runs.last_mut() {
                Some(run) if run.tex == tex && run.end == n => run.end += 1,
                _ => chunk.runs.push(Run {
                    tex,
                    start: n,
                    end: n + 1,
                }),
            }
            chunk.bounds = chunk.bounds.union(&WRect::from_pos_size(
                [inst.pos[0] as f64, inst.pos[1] as f64],
                [inst.size[0] as f64, inst.size[1] as f64],
            ));
            chunk.instances.push(inst);
        };

        match lc.kind {
            DrawKind::IntGrid if lc.derived => {
                // The effective grid: computed cells faint, overrides solid
                // with an outline, suppressed cells a small grey cross.
                let colors = &self.value_colors[layer_ix as usize];
                let color_of = |v: u16| {
                    colors
                        .get(v as usize - 1)
                        .copied()
                        .unwrap_or([255, 0, 255, 255])
                };
                let ov = level.overrides(def.slug.as_str());
                let computed = doc.derived_computed(r.level, def.slug.as_str());
                for y in y0..y1 {
                    for x in x0..x1 {
                        let pos = [ox + x as f32 * grid, oy + y as f32 * grid];
                        for (inst, _) in derived_cell(
                            pos,
                            grid,
                            ov.and_then(|o| o.get(x, y)),
                            computed.and_then(|c| c.index_at(x, y)).unwrap_or(0),
                            &color_of,
                            layer_ix,
                        ) {
                            push(chunk, inst, TEX_WHITE);
                        }
                    }
                }
            }
            DrawKind::IntGrid => {
                if let Some(g) = level.int_grid(def.slug.as_str()) {
                    let colors = &self.value_colors[layer_ix as usize];
                    let [gw, gh] = g.size();
                    let cells = g.cells();
                    for y in y0..y1.min(gh) {
                        let row = &cells[(y * gw) as usize..((y + 1) * gw) as usize];
                        for x in x0..x1.min(gw) {
                            let v = row[x as usize];
                            if v == 0 {
                                continue;
                            }
                            let color = colors
                                .get(v as usize - 1)
                                .copied()
                                .unwrap_or([255, 0, 255, 255]);
                            push(
                                chunk,
                                Instance {
                                    pos: [ox + x as f32 * grid, oy + y as f32 * grid],
                                    size: [grid, grid],
                                    uv: [0.0, 0.0, 1.0, 1.0],
                                    color,
                                    layer: layer_ix,
                                },
                                TEX_WHITE,
                            );
                        }
                    }
                }
            }
            DrawKind::Tiles | DrawKind::Auto => {
                if let Some(b) = &lc.buckets {
                    for &i in &b[r.chunk] {
                        let Some(q) = Self::quad(&self.tilesets, doc, r.level, key, i as usize)
                        else {
                            continue;
                        };
                        if let Some(inst) = q.clipped(share, [ox, oy], layer_ix) {
                            push(chunk, inst, q.tex);
                        }
                    }
                }
            }
        }
        chunk.dirty = false;
        chunk.version = version;
    }
}

/// How much of the layer's opacity a derived layer's computed cells get.
pub const COMPUTED_ALPHA: f32 = 0.45;
/// The cross drawn on a suppressed derived cell.
pub const SUPPRESS_COLOR: [u8; 4] = [175, 175, 175, 235];

/// The quads of one derived layer cell at world `pos` (`grid` px): an
/// override (`Value`) full opacity with a 1 px dark outline, a suppressed
/// cell a small grey ×, else the `computed` value (0: nothing) at
/// [`COMPUTED_ALPHA`]. The second item says which of the three it is
/// (`'o'`, `'!'`, `'c'`), for tests.
fn derived_cell(
    pos: [f32; 2],
    grid: f32,
    stored: Option<Override>,
    computed: u16,
    color_of: &impl Fn(u16) -> [u8; 4],
    layer: u32,
) -> Vec<(Instance, char)> {
    let quad = |p: [f32; 2], size: [f32; 2], color: [u8; 4]| Instance {
        pos: p,
        size,
        uv: [0.0, 0.0, 1.0, 1.0],
        color,
        layer,
    };
    let mut out = Vec::new();
    match stored {
        Some(Override::Value(v)) => {
            let c = color_of(v);
            out.push((quad(pos, [grid, grid], c), 'o'));
            let dark = [c[0] / 3, c[1] / 3, c[2] / 3, 255];
            let [x, y] = pos;
            for (p, s) in [
                ([x, y], [grid, 1.0]),
                ([x, y + grid - 1.0], [grid, 1.0]),
                ([x, y + 1.0], [1.0, grid - 2.0]),
                ([x + grid - 1.0, y + 1.0], [1.0, grid - 2.0]),
            ] {
                out.push((quad(p, s, dark), 'o'));
            }
        }
        Some(Override::Suppress) => {
            // An × of small squares along both diagonals of the middle half.
            let steps = 5;
            let d = (grid * 0.5 / steps as f32).max(1.0);
            for k in 0..steps {
                let t = grid * 0.25 + k as f32 * d;
                let u = grid * 0.75 - (k + 1) as f32 * d;
                out.push((quad([pos[0] + t, pos[1] + t], [d, d], SUPPRESS_COLOR), '!'));
                out.push((quad([pos[0] + u, pos[1] + t], [d, d], SUPPRESS_COLOR), '!'));
            }
        }
        _ if computed > 0 => {
            let mut c = color_of(computed);
            c[3] = (c[3] as f32 * COMPUTED_ALPHA).round() as u8;
            out.push((quad(pos, [grid, grid], c), 'c'));
        }
        _ => {}
    }
    out
}

fn alpha_u8(a: f64) -> u8 {
    (a.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Texture slot, quad size, texel UVs and the transpose flag of atlas tile
/// `tile` of `tileset` drawn with `flip`.
///
/// Flips follow the export contract (`docs/format.md`, Godot's
/// `TileSetAtlasSource` transform): transpose (bit 4) first, then bit 1
/// mirrors the drawn tile left-right and bit 2 top-bottom, in the
/// destination. A mirror is a swap of the UV corners along the axis the
/// destination axis reads: without transpose x reads `u` and y reads `v`;
/// transposed, the shader reads `u` down the quad and `v` across it, so the
/// x mirror swaps `v` and the y mirror swaps `u`. A transposed quad is the
/// tile's height wide and its width tall.
fn tile_quad(
    tilesets: &HashMap<Slug, TilesetInfo>,
    tileset: &Slug,
    tile: [u32; 2],
    flip: Option<Flip>,
) -> (u32, [f32; 2], [f32; 4], bool) {
    let Some(ts) = tilesets.get(tileset) else {
        return (TEX_MISSING, [16.0, 16.0], [0.0, 0.0, 2.0, 2.0], false);
    };
    let [tx, ty] = ts.def.tile_px(tile);
    let [tw, th] = ts.def.tile_size;
    let (mut u0, mut v0, mut u1, mut v1) =
        (tx as f32, ty as f32, (tx + tw) as f32, (ty + th) as f32);
    let bits = Flip::bits_of(flip);
    let (mirror_x, mirror_y, transposed) = (bits & 1 != 0, bits & 2 != 0, bits & 4 != 0);
    let (swap_u, swap_v) = if transposed {
        (mirror_y, mirror_x)
    } else {
        (mirror_x, mirror_y)
    };
    if swap_u {
        std::mem::swap(&mut u0, &mut u1);
    }
    if swap_v {
        std::mem::swap(&mut v0, &mut v1);
    }
    let size = if transposed {
        [th as f32, tw as f32]
    } else {
        [tw as f32, th as f32]
    };
    (ts.tex, size, [u0, v0, u1, v1], transposed)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::path::PathBuf;
    use worlds_core::{
        Color, IntGridDef, IntGridLayer, IntGridValue, LayerDef, Level, Project, TilePlacement,
        TilesDef, TilesLayer, slug,
    };

    /// A 16 px project with one level 40x10 cells at world (100, -50):
    /// IntGrid `col` (values a, b), tiles `deco` from tileset `ts`.
    pub(crate) fn small_doc() -> Document {
        let mut p = Project::new("t");
        p.grid_size = 16;
        p.tilesets.push(TilesetDef {
            slug: slug("ts"),
            image: "ts.png".into(),
            tile_size: [16, 16],
            spacing: 0,
            padding: 0,
        });
        let col = LayerDef::new(
            slug("col"),
            "Col",
            LayerKind::IntGrid(IntGridDef {
                values: vec![
                    IntGridValue {
                        slug: slug("a"),
                        name: "A".into(),
                        color: Color::new("#ff0000").unwrap(),
                        glyph: '#',
                        group: None,
                    },
                    IntGridValue {
                        slug: slug("b"),
                        name: "B".into(),
                        color: Color::new("#00ff0080").unwrap(),
                        glyph: 'b',
                        group: None,
                    },
                ],
                ..Default::default()
            }),
        );
        let deco = LayerDef::new(
            slug("deco"),
            "Deco",
            LayerKind::Tiles(TilesDef {
                tileset: Some(slug("ts")),
            }),
        );
        let mut level = Level::new(slug("a"), "A", [40, 10]);
        level.world_px = [100, -50];
        let mut g = IntGridLayer::new_empty(&col, [40, 10]);
        g.set_index(0, 0, 1);
        g.set_index(5, 3, 2);
        g.set_index(33, 9, 1);
        level.int_grids.push(g);
        level.tiles.push(TilesLayer {
            layer: slug("deco"),
            tiles: vec![
                TilePlacement {
                    cell: [2, 1],
                    tileset: slug("ts"),
                    tile: [3, 2],
                    flip: Some(Flip::X),
                    alpha: 0.5,
                },
                TilePlacement {
                    cell: [35, 1],
                    tileset: slug("ts"),
                    tile: [0, 0],
                    flip: None,
                    alpha: 1.0,
                },
            ],
        });
        p.layers.push(col);
        p.layers.push(deco);
        p.worlds[0].levels.push(level);
        Document::from_project(PathBuf::from("/nonexistent"), p)
    }

    fn all(scene: &Scene) -> WRect {
        let _ = scene;
        WRect::new([-1e9, -1e9], [1e9, 1e9])
    }

    #[test]
    fn reset_level_keeps_ids_on_move_and_retires_on_resize() {
        let mut doc = small_doc();
        let mut scene = Scene::new(&doc);
        let ids = |s: &Scene| -> Vec<u32> {
            s.levels[0]
                .layers
                .iter()
                .flat_map(|l| l.chunks.iter().map(|c| c.id))
                .collect()
        };
        let before = ids(&scene);
        doc.project.worlds[0].levels[0].world_px = [500, 0];
        scene.reset_level(&doc, 0);
        assert_eq!(ids(&scene), before);
        assert!(scene.retired.is_empty());
        assert_eq!(scene.levels[0].rect.min, [500.0, 0.0]);
        assert!(
            scene.levels[0]
                .layers
                .iter()
                .all(|l| l.chunks.iter().all(|c| c.dirty))
        );
        // 40 -> 70 cells wide: two chunks per row become three.
        let lvl = &mut doc.project.worlds[0].levels[0];
        lvl.size = [70, 10];
        lvl.int_grids.clear();
        scene.reset_level(&doc, 0);
        let after = ids(&scene);
        assert_eq!(scene.retired.len(), before.len());
        assert!(after.iter().all(|i| !before.contains(i)));
    }

    #[test]
    fn chunk_grid_shape() {
        let doc = small_doc();
        let s = Scene::new(&doc);
        assert_eq!(s.levels.len(), 1);
        let l = &s.levels[0];
        assert_eq!(l.layers.len(), 2);
        assert_eq!((l.layers[0].chunks_x, l.layers[0].chunks_y), (2, 1));
        assert_eq!(l.layers[0].chunk_cells(1), [32, 0, 40, 10]);
        assert_eq!(l.rect, WRect::from_pos_size([100.0, -50.0], [640.0, 160.0]));
        // Every chunk has its own id.
        let mut ids: Vec<u32> = l
            .layers
            .iter()
            .flat_map(|lc| lc.chunks.iter().map(|c| c.id))
            .collect();
        ids.dedup();
        assert_eq!(ids.len() as u32, s.chunk_id_count());
    }

    #[test]
    fn intgrid_instances() {
        let doc = small_doc();
        let mut s = Scene::new(&doc);
        let r0 = ChunkRef {
            level: 0,
            layer: 0,
            chunk: 0,
        };
        s.rebuild_chunk(&doc, r0);
        let c = s.chunk(r0);
        assert_eq!(c.instances.len(), 2);
        assert_eq!(c.instances[0].pos, [100.0, -50.0]);
        assert_eq!(c.instances[0].color, [255, 0, 0, 255]);
        assert_eq!(c.instances[1].pos, [100.0 + 5.0 * 16.0, -50.0 + 3.0 * 16.0]);
        assert_eq!(c.instances[1].size, [16.0, 16.0]);
        assert_eq!(c.instances[1].color, [0, 255, 0, 128]);
        assert_eq!(
            c.runs,
            vec![Run {
                tex: TEX_WHITE,
                start: 0,
                end: 2
            }]
        );
        assert_eq!(c.bounds, WRect::new([100.0, -50.0], [196.0, 14.0]));
        let r1 = ChunkRef { chunk: 1, ..r0 };
        s.rebuild_chunk(&doc, r1);
        assert_eq!(s.chunk(r1).instances.len(), 1);
        assert_eq!(
            s.chunk(r1).instances[0].pos,
            [100.0 + 33.0 * 16.0, -50.0 + 9.0 * 16.0]
        );
    }

    #[test]
    fn tile_instances_flip_and_alpha() {
        let doc = small_doc();
        let mut s = Scene::new(&doc);
        let r = ChunkRef {
            level: 0,
            layer: 1,
            chunk: 0,
        };
        s.rebuild_chunk(&doc, r);
        let c = s.chunk(r);
        assert_eq!(c.instances.len(), 1);
        let i = c.instances[0];
        assert_eq!(i.pos, [100.0 + 32.0, -50.0 + 16.0]);
        // Tile [3, 2] at 16 px is texels 48..64 x 32..48, flipped in x.
        assert_eq!(i.uv, [64.0, 32.0, 48.0, 48.0]);
        assert_eq!(i.color[3], 128);
        assert_eq!(i.layer, 1);
        assert_eq!(
            c.runs,
            vec![Run {
                tex: tileset_tex(0),
                start: 0,
                end: 1
            }]
        );
        let r1 = ChunkRef { chunk: 1, ..r };
        s.rebuild_chunk(&doc, r1);
        assert_eq!(s.chunk(r1).instances.len(), 1);
    }

    #[test]
    fn dirty_tracking() {
        let doc = small_doc();
        let mut s = Scene::new(&doc);
        let mut out = Vec::new();
        let st = s.collect_visible(&doc, &all(&s), |_| true, None, &mut out);
        assert_eq!(st.rebuilt, 4);
        assert!(
            s.levels[0]
                .layers
                .iter()
                .all(|l| l.chunks.iter().all(|c| !c.dirty))
        );
        let v0 = s.levels[0].layers[0].chunks[1].version;

        // A rect inside chunk 1 of `col` only dirties that chunk.
        s.invalidate(0, "col", [33, 0, 34, 1]);
        let dirty: Vec<(usize, usize)> = s.levels[0]
            .layers
            .iter()
            .enumerate()
            .flat_map(|(j, l)| {
                l.chunks
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| c.dirty)
                    .map(move |(k, _)| (j, k))
            })
            .collect();
        assert_eq!(dirty, vec![(0, 1)]);
        // Straddling the chunk border dirties both; out-of-range clamps.
        s.invalidate(0, "deco", [31, 0, 33, 1]);
        assert!(s.levels[0].layers[1].chunks.iter().all(|c| c.dirty));
        s.invalidate(0, "col", [-100, -100, -99, -99]);
        assert!(s.levels[0].layers[0].chunks[0].dirty);
        // Empty rects and unknown layers are no-ops.
        s.invalidate(0, "nope", [0, 0, 5, 5]);
        s.invalidate(0, "col", [5, 5, 5, 9]);

        let st = s.collect_visible(&doc, &all(&s), |_| true, None, &mut out);
        assert_eq!(st.rebuilt, 4);
        assert!(s.levels[0].layers[0].chunks[1].version > v0);
        // Nothing dirty: nothing rebuilt.
        let st = s.collect_visible(&doc, &all(&s), |_| true, None, &mut out);
        assert_eq!(st.rebuilt, 0);
        assert_eq!(out.len(), 4);
    }

    #[test]
    fn culling_by_chunk() {
        let doc = small_doc();
        let mut s = Scene::new(&doc);
        let mut out = Vec::new();
        // Only the left 10 cells of the level: chunk 0 of each layer.
        let view = WRect::from_pos_size([100.0, -50.0], [160.0, 160.0]);
        s.collect_visible(&doc, &view, |_| true, None, &mut out);
        assert!(out.iter().all(|r| r.chunk == 0), "{out:?}");
        assert_eq!(out.len(), 2);
        // Hidden layers are skipped.
        s.collect_visible(&doc, &view, |l| l != 1, None, &mut out);
        assert_eq!(out.len(), 1);
        // Far away: nothing, and nothing rebuilt.
        let mut s = Scene::new(&doc);
        let far = WRect::from_pos_size([1e5, 1e5], [100.0, 100.0]);
        let st = s.collect_visible(&doc, &far, |_| true, None, &mut out);
        assert!(out.is_empty());
        assert_eq!(st.rebuilt, 0);
    }

    #[test]
    fn budget_defers_rebuilds() {
        let doc = small_doc();
        let mut s = Scene::new(&doc);
        let mut out = Vec::new();
        let st = s.collect_visible(&doc, &all(&s), |_| true, Some(Duration::ZERO), &mut out);
        // One rebuild always happens; with no budget left the rest wait.
        assert_eq!((st.rebuilt, st.pending), (1, 3));
        let st2 = s.collect_visible(&doc, &all(&s), |_| true, None, &mut out);
        assert_eq!(st2.rebuilt, st.pending);
    }

    /// The instance drawn on top at world point `p`: the last one covering
    /// it, walking the visible chunks in draw order as the GPU does.
    fn top_at(scene: &Scene, out: &[ChunkRef], p: [f64; 2]) -> Option<Instance> {
        let mut top = None;
        for &r in out {
            for i in &scene.chunk(r).instances {
                let rect = WRect::from_pos_size(
                    [i.pos[0] as f64, i.pos[1] as f64],
                    [i.size[0] as f64, i.size[1] as f64],
                );
                if rect.contains(p) {
                    top = Some(*i);
                }
            }
        }
        top
    }

    #[test]
    fn big_tiles_across_a_chunk_edge_keep_the_layer_order() {
        // 32 px tiles on the 16 px grid. A at cell (32, 4) is in chunk 1, B
        // at (31, 5) in chunk 0. Row-major order lists A first, so B is on
        // top where they overlap: cell (32, 5), which is in chunk 1. Drawing
        // whole chunks one after the other would put A on top there.
        let mut doc = small_doc();
        doc.project.tilesets[0].tile_size = [32, 32];
        let t = &mut doc.project.worlds[0].levels[0].tiles[0].tiles;
        let tile = |cell, tile| TilePlacement {
            cell,
            tileset: slug("ts"),
            tile,
            flip: None,
            alpha: 1.0,
        };
        *t = vec![tile([32, 4], [1, 0]), tile([31, 5], [2, 0])];
        let doc = Document::from_project(PathBuf::from("/nonexistent"), doc.project);
        let mut s = Scene::new(&doc);
        let mut out = Vec::new();
        s.collect_visible(&doc, &all(&s), |l| l == 1, None, &mut out);
        let [ox, oy] = [100.0, -50.0];
        let at = |cx: f64, cy: f64| [ox + (cx + 0.5) * 16.0, oy + (cy + 0.5) * 16.0];
        // B's tile [2, 0] is texels 64..96: its right half lands in chunk 1.
        let b = top_at(&s, &out, at(32.0, 5.0)).unwrap();
        assert_eq!(b.uv, [80.0, 0.0, 96.0, 32.0], "{b:?}");
        assert_eq!(b.pos, [ox as f32 + 32.0 * 16.0, oy as f32 + 5.0 * 16.0]);
        assert_eq!(b.size, [16.0, 32.0]);
        // Where only A is, A shows; B's left half stays in chunk 0.
        assert_eq!(top_at(&s, &out, at(32.0, 4.0)).unwrap().uv[0], 32.0);
        assert_eq!(
            top_at(&s, &out, at(31.0, 5.0)).unwrap().uv,
            [64.0, 0.0, 80.0, 32.0]
        );
        // Each chunk only draws inside its share: no pixel is drawn twice.
        let c0 = s.levels[0].layers[1].chunks[0].bounds;
        let c1 = s.levels[0].layers[1].chunks[1].bounds;
        assert!(!c0.intersects(&c1), "{c0:?} {c1:?}");
        // Editing B's cell redraws both chunks it reaches.
        s.collect_visible(&doc, &all(&s), |_| true, None, &mut out);
        s.invalidate(0, "deco", [31, 5, 32, 6]);
        assert!(s.levels[0].layers[1].chunks[0].dirty);
        assert!(s.levels[0].layers[1].chunks[1].dirty);
    }

    #[test]
    fn culling_uses_the_placements_actual_reach() {
        // An auto placement 200 px left of the level (a large rule offset),
        // far more than any fixed margin of a few tiles.
        let mut p = small_doc().project;
        p.layers.push(LayerDef::new(
            slug("auto"),
            "Auto",
            LayerKind::Auto(worlds_core::AutoDef::new(slug("col"))),
        ));
        let mut doc = Document::from_project(PathBuf::from("/nonexistent"), p);
        let bake = worlds_rules::LayerBake {
            placements: vec![worlds_rules::Placement {
                px: [-200, 16],
                tileset: slug("ts"),
                tile: [0, 0],
                flip: None,
                alpha: 1.0,
                rule: slug("r"),
                group: slug("g"),
                anchor: [0, 1],
            }],
            ..Default::default()
        };
        // `into`: works whether `LevelBakes` holds bakes or `Arc`s of them.
        #[allow(clippy::useless_conversion)]
        let bakes = [(slug("auto"), bake.into())].into_iter().collect();
        doc.bakes[0] = Some(std::sync::Arc::new(bakes));
        let mut s = Scene::new(&doc);
        let mut out = Vec::new();
        // A view around the placement only, nowhere near the level rect.
        let view = WRect::from_pos_size([100.0 - 210.0, -50.0], [40.0, 60.0]);
        s.collect_visible(&doc, &view, |_| true, None, &mut out);
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(s.levels[0].layers[out[0].layer].slug, "auto");
        // Once the bake is gone the level's extent shrinks back.
        doc.bakes[0] = None;
        s.invalidate_auto(0);
        s.collect_visible(&doc, &view, |_| true, None, &mut out);
        assert!(out.is_empty());
    }

    /// What the shader draws at every pixel of `out`'s chunks, emulated on
    /// the CPU (tiles.wgsl `vs_main`/`fs_main`): for each world pixel
    /// center, the atlas texel the topmost instance there samples.
    fn rasterize(scene: &Scene, out: &[ChunkRef]) -> HashMap<[i64; 2], [i64; 2]> {
        let mut px = HashMap::new();
        for &r in out {
            for i in &scene.chunk(r).instances {
                let t = i.layer & TRANSPOSED != 0;
                let [x0, y0] = [i.pos[0] as i64, i.pos[1] as i64];
                for y in y0..y0 + i.size[1] as i64 {
                    for x in x0..x0 + i.size[0] as i64 {
                        let c = [
                            (x as f32 + 0.5 - i.pos[0]) / i.size[0],
                            (y as f32 + 0.5 - i.pos[1]) / i.size[1],
                        ];
                        let c = if t { [c[1], c[0]] } else { c };
                        let uv = [
                            i.uv[0] + (i.uv[2] - i.uv[0]) * c[0],
                            i.uv[1] + (i.uv[3] - i.uv[1]) * c[1],
                        ];
                        let lo = [i.uv[0].min(i.uv[2]), i.uv[1].min(i.uv[3])];
                        let hi = [i.uv[0].max(i.uv[2]), i.uv[1].max(i.uv[3])];
                        let u = uv[0].clamp(lo[0] + 0.01, hi[0] - 0.01).floor() as i64;
                        let v = uv[1].clamp(lo[1] + 0.01, hi[1] - 0.01).floor() as i64;
                        px.insert([x, y], [u, v]);
                    }
                }
            }
        }
        px
    }

    /// The texel of a `w` x `h` source drawn at destination `(x, y)` with
    /// flip bits `b`: transpose first, then mirror x, then mirror y, in the
    /// destination (the export contract, `docs/format.md`).
    fn source_of(b: u8, [x, y]: [i64; 2], [w, h]: [i64; 2]) -> [i64; 2] {
        // Destination size: transposed tiles are h wide and w tall.
        let (dw, dh) = if b & 4 != 0 { (h, w) } else { (w, h) };
        let x = if b & 1 != 0 { dw - 1 - x } else { x };
        let y = if b & 2 != 0 { dh - 1 - y } else { y };
        if b & 4 != 0 { [y, x] } else { [x, y] }
    }

    #[test]
    fn transposed_tiles_draw_in_all_eight_orientations_across_a_chunk_edge() {
        // 32 px tiles on the 16 px grid, anchored at cell 31 so every tile
        // straddles the chunk edge at x = 512 (chunk 0 draws its left half,
        // chunk 1 its right half, each clipped in the tile's own frame).
        for bits in 0u8..8 {
            let mut doc = small_doc();
            doc.project.tilesets[0].tile_size = [32, 32];
            let t = &mut doc.project.worlds[0].levels[0].tiles[0].tiles;
            *t = vec![TilePlacement {
                cell: [31, 2],
                tileset: slug("ts"),
                tile: [1, 2],
                flip: Flip::from_bits(bits),
                alpha: 1.0,
            }];
            let doc = Document::from_project(PathBuf::from("/nonexistent"), doc.project);
            let mut s = Scene::new(&doc);
            let mut out = Vec::new();
            s.collect_visible(&doc, &all(&s), |l| l == 1, None, &mut out);
            assert_eq!(out.len(), 2, "bits {bits}: drawn in both chunks");
            let px = rasterize(&s, &out);
            let [ox, oy] = [100 + 31 * 16, -50 + 2 * 16];
            assert_eq!(px.len(), 32 * 32, "bits {bits}");
            for y in 0..32 {
                for x in 0..32 {
                    let [sx, sy] = source_of(bits, [x, y], [32, 32]);
                    assert_eq!(
                        px[&[ox + x, oy + y]],
                        [32 + sx, 64 + sy],
                        "bits {bits} at {x},{y}"
                    );
                }
            }
        }
    }

    #[test]
    fn rotated_stamp_across_a_chunk_edge() {
        // A 2x2 stamp of 32 px tiles (atlas tiles [0..2, 0..2]) turned 90
        // degrees clockwise, the way the engine lays a rotated stamp out:
        // each tile moves to its rotated slot and is drawn with flip `xt`
        // (bits 5). The block spans cells 30..34, across the chunk edge at
        // cell 32, and must look like the 64x64 atlas block rotated.
        let mut doc = small_doc();
        doc.project.tilesets[0].tile_size = [32, 32];
        let t = &mut doc.project.worlds[0].levels[0].tiles[0].tiles;
        // Source tile (c, r) goes to slot (1 - r, c) after a cw turn.
        *t = [[0u32, 0u32], [1, 0], [0, 1], [1, 1]]
            .iter()
            .map(|&[c, r]| TilePlacement {
                cell: [30 + 2 * (1 - r), 2 + 2 * c],
                tileset: slug("ts"),
                tile: [c, r],
                flip: Some(Flip::XT),
                alpha: 1.0,
            })
            .collect();
        let doc = Document::from_project(PathBuf::from("/nonexistent"), doc.project);
        let mut s = Scene::new(&doc);
        let mut out = Vec::new();
        s.collect_visible(&doc, &all(&s), |l| l == 1, None, &mut out);
        assert_eq!(out.len(), 2);
        let px = rasterize(&s, &out);
        let [ox, oy] = [100 + 30 * 16, -50 + 2 * 16];
        assert_eq!(px.len(), 64 * 64);
        for y in 0..64 {
            for x in 0..64 {
                // 90 degrees clockwise: destination (x, y) shows (y, 63 - x).
                assert_eq!(px[&[ox + x, oy + y]], [y, 63 - x], "at {x},{y}");
                assert_eq!(source_of(5, [x, y], [64, 64]), [y, 63 - x]);
            }
        }
        // Neither chunk draws outside its share.
        let c0 = s.levels[0].layers[1].chunks[0].bounds;
        let c1 = s.levels[0].layers[1].chunks[1].bounds;
        assert!(!c0.intersects(&c1), "{c0:?} {c1:?}");
    }

    #[test]
    fn derived_cells_computed_faint_overrides_solid_suppressed_crossed() {
        let color = |_: u16| [200, 100, 50, 255];
        let at = [10.0, 20.0];
        // Nothing computed, no override: nothing drawn.
        assert!(derived_cell(at, 32.0, None, 0, &color, 3).is_empty());
        assert!(derived_cell(at, 32.0, Some(Override::None), 0, &color, 3).is_empty());
        // Computed: one cell quad at 45 % alpha.
        let c = derived_cell(at, 32.0, Some(Override::None), 1, &color, 3);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].1, 'c');
        assert_eq!(c[0].0.color, [200, 100, 50, 115]);
        assert_eq!(
            (c[0].0.pos, c[0].0.size, c[0].0.layer),
            (at, [32.0, 32.0], 3)
        );
        // Override: the full color, then a 1 px outline in the dark shade
        // (four quads inside the cell), whatever was computed.
        let o = derived_cell(at, 32.0, Some(Override::Value(1)), 0, &color, 3);
        assert_eq!(o.len(), 5);
        assert_eq!(o[0].0.color, [200, 100, 50, 255]);
        assert!(o[1..].iter().all(|(i, k)| *k == 'o'
            && i.color == [66, 33, 16, 255]
            && (i.size[0] == 1.0 || i.size[1] == 1.0)));
        let total: f32 = o[1..].iter().map(|(i, _)| i.size[0] * i.size[1]).sum();
        assert_eq!(total, 4.0 * 32.0 - 4.0);
        // Suppressed: a small grey cross inside the cell, over a computed
        // value or not; never the value's color.
        for computed in [0, 1] {
            let x = derived_cell(at, 32.0, Some(Override::Suppress), computed, &color, 3);
            assert_eq!(x.len(), 10);
            for (i, k) in &x {
                assert_eq!((*k, i.color), ('!', SUPPRESS_COLOR));
                assert!(i.pos[0] >= at[0] + 8.0 && i.pos[0] + i.size[0] <= at[0] + 24.0);
                assert!(i.pos[1] >= at[1] + 8.0 && i.pos[1] + i.size[1] <= at[1] + 24.0);
            }
        }
    }

    #[test]
    fn colors() {
        assert_eq!(parse_color("#46464F"), [0x46, 0x46, 0x4f, 255]);
        assert_eq!(parse_color("#01020304"), [1, 2, 3, 4]);
    }
}
