//! Assembly thumbnails: a small picture of an assembly's grid cells, tiles
//! and entity boxes, rendered on the CPU and cached as
//! `.worlds/thumbs/assembly-<slug>-<hash>.png` (`<hash>`: of the assembly's
//! content, so an edited assembly gets a new picture and the old file is
//! dropped). The Library draws them from [`Thumbs`]: a file already there
//! is loaded, else rendered once and written.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use image::{Rgba, RgbaImage};
use worlds_core::assembly::Assembly;
use worlds_core::{Flip, Project};

use crate::viewport::scene::parse_color;

/// The longest side of a thumbnail, in pixels.
pub const MAX_SIDE: u32 = 96;

/// The directory thumbnails go in.
pub fn dir(project_dir: &Path) -> PathBuf {
    project_dir.join(".worlds").join("thumbs")
}

/// The content hash of an assembly, as its file name carries it.
pub fn content_hash(a: &Assembly) -> String {
    let json = serde_json::to_vec(a).unwrap_or_default();
    worlds_core::autosave::hex(worlds_core::autosave::hash(&json))
}

/// The thumbnail file of an assembly.
pub fn file_of(project_dir: &Path, a: &Assembly) -> PathBuf {
    dir(project_dir).join(format!("assembly-{}-{}.png", a.slug, content_hash(a)))
}

/// Tileset images by slug, loaded once (`None`: it didn't load).
#[derive(Default)]
pub struct Images(HashMap<String, Option<RgbaImage>>);

impl Images {
    fn get(&mut self, project_dir: &Path, project: &Project, slug: &str) -> Option<&RgbaImage> {
        self.0
            .entry(slug.to_string())
            .or_insert_with(|| {
                let ts = project.tileset(slug)?;
                image::open(project_dir.join(&ts.image))
                    .map(|i| i.to_rgba8())
                    .ok()
            })
            .as_ref()
    }
}

/// Draw `a` (as stored, unturned) at most [`MAX_SIDE`] px on a side.
pub fn render(
    project_dir: &Path,
    project: &Project,
    a: &Assembly,
    images: &mut Images,
) -> RgbaImage {
    let g = a.grid_size.max(1) as f64;
    let w_px = (a.size[0].max(1) as f64 * g).max(1.0);
    let h_px = (a.size[1].max(1) as f64 * g).max(1.0);
    let scale = (MAX_SIDE as f64 / w_px)
        .min(MAX_SIDE as f64 / h_px)
        .min(4.0);
    let (w, h) = (
        ((w_px * scale).round() as u32).max(1),
        ((h_px * scale).round() as u32).max(1),
    );
    let bg = project
        .bg_color
        .as_ref()
        .map(|c| parse_color(c.as_str()))
        .unwrap_or([0x20, 0x22, 0x2c, 0xff]);
    let mut img = RgbaImage::from_pixel(w, h, Rgba([bg[0], bg[1], bg[2], 0xff]));
    let fill = |img: &mut RgbaImage, x0: f64, y0: f64, x1: f64, y1: f64, c: [u8; 4]| {
        let (x0, y0) = (
            (x0 * scale).floor().max(0.0) as u32,
            (y0 * scale).floor().max(0.0) as u32,
        );
        let (x1, y1) = (
            ((x1 * scale).ceil() as u32).min(w),
            ((y1 * scale).ceil() as u32).min(h),
        );
        for y in y0..y1 {
            for x in x0..x1 {
                blend(img.get_pixel_mut(x, y), c);
            }
        }
    };
    // Layers in draw order: grids and tiles as the project stacks them.
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
                        fill(&mut img, px, py, px + lg, py + lg, col);
                    }
                }
            }
        }
        if let Some(tiles) = a.tiles.get(&def.slug) {
            for t in tiles {
                let Some(ts) = project.tileset(t.tileset.as_str()) else {
                    continue;
                };
                let (tw, th) = (ts.tile_size[0], ts.tile_size[1]);
                let src = ts.tile_px(t.tile);
                let (px, py) = (t.cell[0] as f64 * lg, t.cell[1] as f64 * lg);
                let Some(atlas) = images.get(project_dir, project, t.tileset.as_str()) else {
                    fill(
                        &mut img,
                        px,
                        py,
                        px + tw as f64,
                        py + th as f64,
                        [0x80, 0x80, 0x90, 0xa0],
                    );
                    continue;
                };
                let bits = Flip::bits_of(t.flip);
                let (bw, bh) = if bits & 4 != 0 { (th, tw) } else { (tw, th) };
                // Each thumbnail pixel the tile box covers samples the
                // atlas (nearest), through the inverse of the flip.
                let x0 = (px * scale).floor() as i64;
                let y0 = (py * scale).floor() as i64;
                let x1 = ((px + bw as f64) * scale).ceil() as i64;
                let y1 = ((py + bh as f64) * scale).ceil() as i64;
                for y in y0.max(0)..y1.min(h as i64) {
                    for x in x0.max(0)..x1.min(w as i64) {
                        let mut u = ((x as f64 + 0.5) / scale - px).floor() as i64;
                        let mut v = ((y as f64 + 0.5) / scale - py).floor() as i64;
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
                        if sx < 0
                            || sy < 0
                            || sx >= atlas.width() as i64
                            || sy >= atlas.height() as i64
                        {
                            continue;
                        }
                        let mut c = atlas.get_pixel(sx as u32, sy as u32).0;
                        c[3] = (c[3] as f64 * t.alpha.clamp(0.0, 1.0)) as u8;
                        blend(img.get_pixel_mut(x as u32, y as u32), c);
                    }
                }
            }
        }
    }
    // Entity boxes on top, in the type's color.
    for e in &a.entities {
        let Some(def) = project.entity_def(e.ty.as_str()) else {
            continue;
        };
        let lg = e
            .layer
            .as_ref()
            .and_then(|l| project.layer(l.as_str()))
            .map_or(project.grid_size, |d| project.layer_grid(d));
        let inst = worlds_core::EntityInstance {
            id: e.id.clone(),
            ty: e.ty.clone(),
            cell: e.cell,
            offset_px: e.offset_px,
            size: e.size,
            fields: Default::default(),
        };
        let [x, y, bw, bh] = def.pixel_rect(&inst, lg, a.grid_size);
        let mut c = parse_color(def.color.as_str());
        c[3] = 0xb0;
        let (x0, y0, x1, y1) = (x as f64, y as f64, (x + bw) as f64, (y + bh) as f64);
        fill(&mut img, x0, y0, x1, y1, c);
        c[3] = 0xff;
        let edge = 1.0 / scale;
        fill(&mut img, x0, y0, x1, y0 + edge, c);
        fill(&mut img, x0, y1 - edge, x1, y1, c);
        fill(&mut img, x0, y0, x0 + edge, y1, c);
        fill(&mut img, x1 - edge, y0, x1, y1, c);
    }
    img
}

/// Alpha-blend `c` over `p`.
fn blend(p: &mut Rgba<u8>, c: [u8; 4]) {
    let a = c[3] as u32;
    for i in 0..3 {
        p.0[i] = ((c[i] as u32 * a + p.0[i] as u32 * (255 - a)) / 255) as u8;
    }
    p.0[3] = 0xff;
}

/// The thumbnail of `a`: read from the cache file when it is there, else
/// rendered and written (a write failure is logged; the picture is still
/// returned). Older files of the same assembly are removed.
pub fn load_or_render(
    project_dir: &Path,
    project: &Project,
    a: &Assembly,
    images: &mut Images,
) -> RgbaImage {
    let path = file_of(project_dir, a);
    if let Ok(img) = image::open(&path) {
        return img.to_rgba8();
    }
    let img = render(project_dir, project, a, images);
    let d = dir(project_dir);
    if let Err(e) = std::fs::create_dir_all(&d).and_then(|_| {
        img.save(&path)
            .map_err(|e| std::io::Error::other(e.to_string()))
    }) {
        tracing::warn!("cannot write {}: {e}", path.display());
    }
    let prefix = format!("assembly-{}-", a.slug);
    if let Ok(rd) = std::fs::read_dir(&d) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            // `assembly-door-<hash>.png`, not `assembly-door_b-<hash>.png`.
            let stale = name
                .strip_prefix(&prefix)
                .and_then(|rest| rest.strip_suffix(".png"))
                .is_some_and(|h| h.len() == 16 && !h.contains('-'));
            if stale && e.path() != path {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    img
}

/// The Library's thumbnail textures, by assembly slug and content hash.
#[derive(Default)]
pub struct Thumbs {
    textures: HashMap<String, (String, egui::TextureHandle)>,
    images: Images,
    /// How many thumbnails were rendered (not read from the cache; tests).
    pub rendered: usize,
}

impl Thumbs {
    /// Forget everything (a project was opened or reloaded).
    pub fn clear(&mut self) {
        self.textures.clear();
        self.images = Images::default();
    }

    /// The texture of `a`'s thumbnail, loaded or rendered on first use.
    pub fn texture(
        &mut self,
        ctx: &egui::Context,
        project_dir: &Path,
        project: &Project,
        a: &Assembly,
    ) -> egui::TextureHandle {
        let hash = content_hash(a);
        if let Some((h, t)) = self.textures.get(a.slug.as_str())
            && *h == hash
        {
            return t.clone();
        }
        let path = file_of(project_dir, a);
        if !path.exists() {
            self.rendered += 1;
        }
        let img = load_or_render(project_dir, project, a, &mut self.images);
        let ci = egui::ColorImage::from_rgba_unmultiplied(
            [img.width() as usize, img.height() as usize],
            img.as_raw(),
        );
        let t = ctx.load_texture(
            format!("assembly thumb {}", a.slug),
            ci,
            egui::TextureOptions::NEAREST,
        );
        self.textures.insert(a.slug.to_string(), (hash, t.clone()));
        t
    }
}
