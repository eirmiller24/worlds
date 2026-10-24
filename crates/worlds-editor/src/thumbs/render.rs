//! Drawing thumbnails on the CPU. Both kinds go through one [`Canvas`]: a
//! picture of some source rect (an assembly's box, a level's box) at
//! `scale` thumbnail pixels per source pixel, with the same few strokes:
//! fill a rect, draw a tile, draw an entity box.
//!
//! A tile is drawn by sampling the atlas (nearest, through its flip) when
//! it covers more than 2 thumbnail pixels a side, else as one rect of its
//! average color: at a pixel per cell (a level) a 16 px tile is one pixel,
//! and its average is what a pixel of it should be.

use std::collections::HashMap;

use image::{Rgba, RgbaImage};
use worlds_core::assembly::Assembly;
use worlds_core::{EntityDef, EntityInstance, Flip, Slug, TilesetDef};

use crate::doc::{Document, LevelIx};
use crate::viewport::scene::parse_color;

/// The longest side of an assembly thumbnail, in pixels.
pub const ASSEMBLY_MAX_SIDE: u32 = 96;
/// The longest side of a level thumbnail, in pixels (below it: one pixel
/// per project cell).
pub const LEVEL_MAX_SIDE: u32 = 256;

/// One tileset image, with each tile's average color once a picture needs
/// it.
pub struct Atlas {
    pub image: RgbaImage,
    /// By `row * columns + column`.
    averages: Option<(u32, Vec<[u8; 4]>)>,
}

impl Atlas {
    /// The average color of tile `[col, row]` (alpha: its mean coverage).
    fn average(&mut self, ts: &TilesetDef, tile: [u32; 2]) -> Option<[u8; 4]> {
        let img = &self.image;
        let (cols, colors) = self.averages.get_or_insert_with(|| {
            let (cols, rows) = (ts.columns(img.width()), ts.rows(img.height()));
            let [tw, th] = ts.tile_size;
            let mut colors = Vec::with_capacity((cols * rows) as usize);
            for r in 0..rows {
                for c in 0..cols {
                    let [x0, y0] = ts.tile_px([c, r]);
                    let mut sum = [0u64; 4];
                    for y in y0..(y0 + th).min(img.height()) {
                        for x in x0..(x0 + tw).min(img.width()) {
                            let p = img.get_pixel(x, y).0;
                            let a = p[3] as u64;
                            for k in 0..3 {
                                sum[k] += p[k] as u64 * a;
                            }
                            sum[3] += a;
                        }
                    }
                    let n = (tw as u64 * th as u64).max(1);
                    let avg = |k: usize| sum[k].checked_div(sum[3]).unwrap_or(0) as u8;
                    colors.push([avg(0), avg(1), avg(2), (sum[3] / n) as u8]);
                }
            }
            (cols, colors)
        });
        colors.get((tile[1] * *cols + tile[0]) as usize).copied()
    }
}

/// Tileset images by slug, loaded once per [`Document::images_gen`]
/// (`None`: it didn't load).
#[derive(Default)]
pub struct Images {
    images_gen: u64,
    by_slug: HashMap<Slug, Option<Atlas>>,
}

impl Images {
    fn get(&mut self, doc: &Document, slug: &Slug) -> Option<&mut Atlas> {
        if self.images_gen != doc.images_gen {
            self.images_gen = doc.images_gen;
            self.by_slug.clear();
        }
        self.by_slug
            .entry(slug.clone())
            .or_insert_with(|| {
                let ts = doc.project.tileset(slug.as_str())?;
                let image = image::open(doc.dir.join(&ts.image)).ok()?.to_rgba8();
                Some(Atlas {
                    image,
                    averages: None,
                })
            })
            .as_mut()
    }
}

/// A picture of a source rect `[0, w) x [0, h)` (source pixels: an
/// assembly's or a level's) at `scale` thumbnail pixels per source pixel.
pub struct Canvas {
    pub img: RgbaImage,
    pub scale: f64,
}

impl Canvas {
    /// A canvas for a `w` x `h` source, at most `max_side` px a side and at
    /// most `max_scale`, filled with `bg`.
    pub fn new(w: f64, h: f64, max_side: u32, max_scale: f64, bg: [u8; 4]) -> Self {
        let (w, h) = (w.max(1.0), h.max(1.0));
        let scale = (max_side as f64 / w)
            .min(max_side as f64 / h)
            .min(max_scale);
        let size = |v: f64| ((v * scale).round() as u32).clamp(1, max_side);
        Canvas {
            img: RgbaImage::from_pixel(size(w), size(h), Rgba(bg)),
            scale,
        }
    }

    fn w(&self) -> u32 {
        self.img.width()
    }

    fn h(&self) -> u32 {
        self.img.height()
    }

    /// The thumbnail pixels a source rect covers: at least one, when it
    /// meets the picture at all.
    fn span(&self, x0: f64, y0: f64, x1: f64, y1: f64) -> Option<(u32, u32, u32, u32)> {
        let s = self.scale;
        let (w, h) = (self.w() as f64, self.h() as f64);
        let (a, b) = ((x0 * s).floor().max(0.0), (y0 * s).floor().max(0.0));
        let (c, d) = ((x1 * s).ceil().min(w), (y1 * s).ceil().min(h));
        if x1 <= 0.0 || y1 <= 0.0 || a >= w || b >= h {
            return None;
        }
        let (c, d) = (c.max(a + 1.0), d.max(b + 1.0));
        Some((a as u32, b as u32, c as u32, d as u32))
    }

    /// Blend `c` over the source rect `[x0, x1) x [y0, y1)`.
    pub fn fill(&mut self, x0: f64, y0: f64, x1: f64, y1: f64, c: [u8; 4]) {
        let Some((a, b, cx, d)) = self.span(x0, y0, x1, y1) else {
            return;
        };
        for y in b..d {
            for x in a..cx {
                blend(self.img.get_pixel_mut(x, y), c);
            }
        }
    }

    /// Tile `tile` of `ts` with its top-left at source pixel `[px, py]`.
    #[allow(clippy::too_many_arguments)]
    pub fn tile(
        &mut self,
        ts: &TilesetDef,
        atlas: Option<&mut Atlas>,
        tile: [u32; 2],
        flip: Option<Flip>,
        alpha: f64,
        px: f64,
        py: f64,
    ) {
        let (tw, th) = (ts.tile_size[0], ts.tile_size[1]);
        let bits = Flip::bits_of(flip);
        let (bw, bh) = if bits & 4 != 0 { (th, tw) } else { (tw, th) };
        let alpha = alpha.clamp(0.0, 1.0);
        let Some(atlas) = atlas else {
            // No image: a grey box where the tile is.
            let c = [0x80, 0x80, 0x90, (0xa0 as f64 * alpha) as u8];
            self.fill(px, py, px + bw as f64, py + bh as f64, c);
            return;
        };
        let s = self.scale;
        if bw.max(bh) as f64 * s <= 2.0 {
            if let Some(mut c) = atlas.average(ts, tile) {
                c[3] = (c[3] as f64 * alpha) as u8;
                self.fill(px, py, px + bw as f64, py + bh as f64, c);
            }
            return;
        }
        let src = ts.tile_px(tile);
        let (w, h) = (self.w() as i64, self.h() as i64);
        let x0 = (px * s).floor() as i64;
        let y0 = (py * s).floor() as i64;
        let x1 = ((px + bw as f64) * s).ceil() as i64;
        let y1 = ((py + bh as f64) * s).ceil() as i64;
        let atlas = &atlas.image;
        // Each thumbnail pixel the tile box covers samples the atlas
        // (nearest), through the inverse of the flip.
        for y in y0.max(0)..y1.min(h) {
            for x in x0.max(0)..x1.min(w) {
                let mut u = ((x as f64 + 0.5) / s - px).floor() as i64;
                let mut v = ((y as f64 + 0.5) / s - py).floor() as i64;
                if u < 0 || v < 0 || u >= bw as i64 || v >= bh as i64 {
                    continue;
                }
                if bits & 2 != 0 {
                    v = bh as i64 - 1 - v;
                }
                if bits & 1 != 0 {
                    u = bw as i64 - 1 - u;
                }
                if bits & 4 != 0 {
                    std::mem::swap(&mut u, &mut v);
                }
                let (sx, sy) = (src[0] as i64 + u, src[1] as i64 + v);
                if sx < 0 || sy < 0 || sx >= atlas.width() as i64 || sy >= atlas.height() as i64 {
                    continue;
                }
                let mut c = atlas.get_pixel(sx as u32, sy as u32).0;
                c[3] = (c[3] as f64 * alpha) as u8;
                blend(self.img.get_pixel_mut(x as u32, y as u32), c);
            }
        }
    }

    /// An entity's box in its type's color, outlined when it is at least 3
    /// thumbnail pixels a side.
    pub fn entity(
        &mut self,
        def: &EntityDef,
        inst: &EntityInstance,
        layer_grid: u32,
        grid: u32,
        opacity: f64,
    ) {
        let [x, y, bw, bh] = def.pixel_rect(inst, layer_grid, grid);
        let mut c = parse_color(def.color.as_str());
        let (x0, y0, x1, y1) = (x as f64, y as f64, (x + bw) as f64, (y + bh) as f64);
        c[3] = (0xb0 as f64 * opacity) as u8;
        self.fill(x0, y0, x1, y1, c);
        if (bw.min(bh) as f64) * self.scale >= 3.0 {
            c[3] = (0xff as f64 * opacity) as u8;
            let e = 1.0 / self.scale;
            self.fill(x0, y0, x1, y0 + e, c);
            self.fill(x0, y1 - e, x1, y1, c);
            self.fill(x0, y0, x0 + e, y1, c);
            self.fill(x1 - e, y0, x1, y1, c);
        }
    }
}

/// Alpha-blend `c` over `p` (the canvas is opaque until the void is cut).
fn blend(p: &mut Rgba<u8>, c: [u8; 4]) {
    let a = c[3] as u32;
    for (d, s) in p.0.iter_mut().zip(c).take(3) {
        *d = ((s as u32 * a + *d as u32 * (255 - a)) / 255) as u8;
    }
    p.0[3] = 0xff;
}

/// Assembly `a` (as stored, unturned), at most [`ASSEMBLY_MAX_SIDE`] px a
/// side: on a light neutral ground (so dark wall colors and empty cells
/// read apart) with a faint line every cell, its grid cells and tiles in
/// the project's layer order, entity boxes on top.
pub fn assembly(doc: &Document, a: &Assembly, images: &mut Images) -> RgbaImage {
    let project = &doc.project;
    let g = a.grid_size.max(1) as f64;
    let (w_px, h_px) = (a.size[0].max(1) as f64 * g, a.size[1].max(1) as f64 * g);
    let mut cv = Canvas::new(w_px, h_px, ASSEMBLY_MAX_SIDE, 4.0, [0x7a, 0x7e, 0x8c, 0xff]);
    let scale = cv.scale;
    if g * scale >= 4.0 {
        let on = |v: u32| ((v as f64 / scale) % g) < 1.0 / scale;
        for y in 0..cv.h() {
            for x in 0..cv.w() {
                if on(x) || on(y) {
                    blend(cv.img.get_pixel_mut(x, y), [0x60, 0x64, 0x70, 0x60]);
                }
            }
        }
    }
    for def in &project.layers {
        let lg = project.layer_grid(def) as f64;
        if let Some(fg) = a.grids.get(&def.slug) {
            for (y, row) in fg.rows.iter().enumerate() {
                for (x, c) in row.chars().enumerate() {
                    let value = match fg.legend.get(&c) {
                        Some(v) => v.as_ref().and_then(|s| def.value(s.as_str())),
                        None => def.value_by_glyph(c),
                    };
                    if let Some(v) = value {
                        let mut col = parse_color(v.color.as_str());
                        col[3] = 0xe0;
                        let (px, py) = (x as f64 * lg, y as f64 * lg);
                        cv.fill(px, py, px + lg, py + lg, col);
                    }
                }
            }
        }
        for t in a.tiles.get(&def.slug).into_iter().flatten() {
            let Some(ts) = project.tileset(t.tileset.as_str()) else {
                continue;
            };
            let (px, py) = (t.cell[0] as f64 * lg, t.cell[1] as f64 * lg);
            let atlas = images.get(doc, &t.tileset);
            cv.tile(ts, atlas, t.tile, t.flip, t.alpha, px, py);
        }
    }
    for e in &a.entities {
        let Some(def) = project.entity_def(e.ty.as_str()) else {
            continue;
        };
        let lg = e
            .layer
            .as_ref()
            .and_then(|l| project.layer(l.as_str()))
            .map_or(project.grid_size, |d| project.layer_grid(d));
        let inst = EntityInstance {
            id: e.id.clone(),
            ty: e.ty.clone(),
            cell: e.cell,
            offset_px: e.offset_px,
            size: e.size,
            fields: Default::default(),
        };
        cv.entity(def, &inst, lg, a.grid_size, 1.0);
    }
    cv.img
}

/// Level `ix`, one pixel per project cell (at most [`LEVEL_MAX_SIDE`] a
/// side): its background, then each layer bottom first at `opacity[layer]`
/// (int_grid values in their colors, hand-placed and baked auto tiles,
/// entity boxes), then its void cut out (transparent), so a
/// non-rectangular level is its own shape.
pub fn level(doc: &Document, ix: LevelIx, opacity: &[f32], images: &mut Images) -> RgbaImage {
    let project = &doc.project;
    let l = doc.level(ix);
    let g = project.grid_size.max(1) as f64;
    let (wpx, hpx) = (l.size[0].max(1) as f64 * g, l.size[1].max(1) as f64 * g);
    let bg = l
        .bg_color
        .as_ref()
        .or(project.bg_color.as_ref())
        .map_or([0x40, 0x40, 0x48, 0xff], |c| parse_color(c.as_str()));
    let mut cv = Canvas::new(
        wpx,
        hpx,
        LEVEL_MAX_SIDE,
        1.0 / g,
        [bg[0], bg[1], bg[2], 0xff],
    );
    let s = cv.scale;
    let bakes = doc.bakes.get(ix).and_then(|b| b.as_ref());
    for (i, def) in project.layers.iter().enumerate() {
        let op = opacity.get(i).copied().unwrap_or(1.0) as f64;
        if op <= 0.0 {
            continue;
        }
        let lg = project.layer_grid(def) as f64;
        if let Some(grid) = l.int_grid(def.slug.as_str()) {
            // One sample per thumbnail pixel, at its middle: bounded by the
            // picture, however many cells the layer has.
            let values = def.values();
            for y in 0..cv.h() {
                let cy = ((y as f64 + 0.5) / s / lg) as u32;
                for x in 0..cv.w() {
                    let cx = ((x as f64 + 0.5) / s / lg) as u32;
                    if let Some(v) = grid.index_at(cx, cy).filter(|&v| v > 0)
                        && let Some(val) = values.get(v as usize - 1)
                    {
                        let mut c = parse_color(val.color.as_str());
                        c[3] = (0xe0 as f64 * op) as u8;
                        blend(cv.img.get_pixel_mut(x, y), c);
                    }
                }
            }
        }
        if let Some(t) = l.tiles_layer(def.slug.as_str()) {
            for p in &t.tiles {
                let Some(ts) = project.tileset(p.tileset.as_str()) else {
                    continue;
                };
                let (px, py) = (p.cell[0] as f64 * lg, p.cell[1] as f64 * lg);
                let atlas = images.get(doc, &p.tileset);
                cv.tile(ts, atlas, p.tile, p.flip, p.alpha * op, px, py);
            }
        }
        if def.is_auto()
            && let Some(b) = bakes.and_then(|b| b.get(&def.slug))
        {
            for p in &b.placements {
                let Some(ts) = project.tileset(p.tileset.as_str()) else {
                    continue;
                };
                let atlas = images.get(doc, &p.tileset);
                let (px, py) = (p.px[0] as f64, p.px[1] as f64);
                cv.tile(ts, atlas, p.tile, p.flip, p.alpha * op, px, py);
            }
        }
        if let Some(el) = l.entities_layer(def.slug.as_str()) {
            for e in &el.entities {
                if let Some(ed) = project.entity_def(e.ty.as_str()) {
                    cv.entity(ed, e, lg as u32, project.grid_size, op);
                }
            }
        }
    }
    if l.has_void() {
        for y in 0..cv.h() {
            let cy = ((y as f64 + 0.5) / s / g) as i64;
            for x in 0..cv.w() {
                let cx = ((x as f64 + 0.5) / s / g) as i64;
                if !l.in_level(cx, cy) {
                    cv.img.put_pixel(x, y, Rgba([0, 0, 0, 0]));
                }
            }
        }
    }
    cv.img
}
