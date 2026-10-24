//! Thumbnails: small CPU pictures of assemblies (the Palette's Library) and
//! levels (the Levels pane, and the map zoomed out past
//! [`crate::viewport::DETAIL_CELL_PX`]), behind one contract and one cache.
//!
//! **The contract.** Anything with a thumbnail is a [`Source`]: it says
//! which picture it is ([`ThumbId`]), what the picture depends on (a
//! [`Source::key`]: equal keys, same picture), whether the picture may be
//! kept on disk ([`Source::file`]), and how to draw itself
//! ([`Source::render`]). [`AssemblyThumb`] and [`LevelThumb`] are the two
//! sources; [`render`] holds their drawing, which shares one
//! [`render::Canvas`] (background, int_grid cells, tiles, entity boxes).
//!
//! **The cache** ([`Thumbs`], one per editor, in `EditorState::thumbs`)
//! owns the egui textures, one per id, replaced in place when the key moves.
//! Every consumer asks it for the same id and gets the same texture, so the
//! Levels pane and every map tab share one render of a level: a second
//! viewport tab costs nothing.
//!
//! **Disk or memory, per kind.**
//! - An assembly changes rarely and its key is a hash of its JSON, so its
//!   picture is kept as `.worlds/thumbs/assembly-<slug>-<hash>.png`: the next
//!   session reads it instead of drawing it. Writing a new one removes the
//!   older files of that slug.
//! - A level changes on every brush stroke, and its key is a stamp of the
//!   edits that reached it ([`crate::doc::Document::look_stamp`]), which
//!   means nothing to the next session. A file per stroke would be pure
//!   churn, so level pictures live in memory only.
//! - Headless (`--screenshot`) reads files but never writes one: nothing
//!   lands in the project unless `--save`.
//!
//! **Size.** A level is drawn at one pixel per project cell (each tile as
//! its average color), capped at [`render::LEVEL_MAX_SIDE`]. That is the
//! most the map needs (zoomed out a cell is under 4 screen px, so the
//! texture is magnified at most 4x, nearest) and the Levels pane's 60x36
//! box shrinks it (linear). One texture for both, so the size isn't in
//! the key. An assembly is at most [`render::ASSEMBLY_MAX_SIDE`] px, up to 4
//! px per pixel of art, as the Library shows it.
//!
//! **Budget, not workers.** A level picture costs about one write per cell
//! per layer plus one per tile, and the bake already did the expensive part
//! (the rules). In a release build the game fixture's crevasse (64x28
//! cells, ~300 baked tiles) takes 0.2 ms, its 14x7 level_3 5 µs; a
//! 256x256-cell level would be some 7 ms. So the cache draws on the UI
//! thread within [`Budget::FRAME`] (8 pictures or 4 ms a frame, at least
//! one), and only what is asked for: the rows and levels on screen, so a
//! thousand-level world fills in over a few frames while staying
//! responsive, and levels off screen cost nothing. Until a new picture is
//! drawn the old one stays (no flicker), a turned-away request asks for
//! another frame, and a picture redrawn less than 250 ms ago waits (a
//! brush stroke moves its level's key every frame; the thumbnail catches
//! up four times a second instead of costing every frame). The worker pool
//! would need a copy of the level and a hold on its bakes, and holding a
//! bake from another thread would stop the command layer editing it in
//! place (its `Arc` would be shared): every brush move would copy whole
//! layers, costing far more than the picture. Headless renders take
//! [`Budget::UNLIMITED`]: a screenshot has every picture, current, in its
//! first frame.

pub mod cache;
pub mod render;

use std::path::{Path, PathBuf};

use image::RgbaImage;
use worlds_core::assembly::Assembly;
use worlds_core::{Id, Slug};

use crate::doc::{Document, LevelIx};
use crate::view::ViewState;

pub use cache::{Budget, Thumbs};
pub use render::{ASSEMBLY_MAX_SIDE, Images, LEVEL_MAX_SIDE};

/// Which thumbnail: one texture each.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ThumbId {
    Assembly(Slug),
    /// By id, so a renamed level keeps its picture.
    Level(Id),
}

/// A picture kept on disk: `<dir>/<prefix><hash>.png`. Other files of the
/// same prefix and a 16-digit hash are older pictures of the same thing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheFile {
    pub dir: PathBuf,
    /// `assembly-<slug>-`.
    pub prefix: String,
    /// 16 hex digits.
    pub hash: String,
}

impl CacheFile {
    pub fn path(&self) -> PathBuf {
        self.dir.join(format!("{}{}.png", self.prefix, self.hash))
    }

    /// Whether file name `name` is an older picture of the same thing:
    /// `assembly-door-<hash>.png`, but not `assembly-door_b-<hash>.png` or
    /// `assembly-door-b-<hash>.png`.
    pub fn is_older(&self, name: &str) -> bool {
        name.strip_prefix(&self.prefix)
            .and_then(|rest| rest.strip_suffix(".png"))
            .is_some_and(|h| h.len() == 16 && !h.contains('-') && h != self.hash)
    }
}

/// Something with a thumbnail.
pub trait Source {
    /// Which picture this is.
    fn id(&self) -> ThumbId;
    /// Everything the picture depends on, hashed: the cache keeps a picture
    /// while the key stays the same, and draws it again when it moves.
    fn key(&self) -> u64;
    /// Where the picture is kept between sessions; `None`: memory only.
    fn file(&self) -> Option<CacheFile>;
    /// Draw the picture.
    fn render(&self, images: &mut Images) -> RgbaImage;
}

/// Draw `tex` as large as fits in `bx`, centred, keeping its shape (the
/// Library's and the Levels pane's rows); returns where it went.
pub fn draw_fitted(p: &egui::Painter, tex: &egui::TextureHandle, bx: egui::Rect) -> egui::Rect {
    let [tw, th] = tex.size();
    let s = (bx.width() / tw.max(1) as f32).min(bx.height() / th.max(1) as f32);
    let r = egui::Rect::from_center_size(bx.center(), egui::vec2(tw as f32 * s, th as f32 * s));
    let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
    p.image(tex.id(), r, uv, egui::Color32::WHITE);
    r
}

/// The directory thumbnails are kept in.
pub fn dir(project_dir: &Path) -> PathBuf {
    project_dir.join(".worlds").join("thumbs")
}

/// The content hash of an assembly, as its file name carries it.
pub fn content_hash(a: &Assembly) -> u64 {
    let json = serde_json::to_vec(a).unwrap_or_default();
    worlds_core::autosave::hash(&json)
}

/// An assembly's thumbnail (the Library's rows).
pub struct AssemblyThumb<'a> {
    pub doc: &'a Document,
    pub assembly: &'a Assembly,
}

impl Source for AssemblyThumb<'_> {
    fn id(&self) -> ThumbId {
        ThumbId::Assembly(self.assembly.slug.clone())
    }

    fn key(&self) -> u64 {
        content_hash(self.assembly)
    }

    fn file(&self) -> Option<CacheFile> {
        Some(CacheFile {
            dir: dir(&self.doc.dir),
            prefix: format!("assembly-{}-", self.assembly.slug),
            hash: worlds_core::autosave::hex(self.key()),
        })
    }

    fn render(&self, images: &mut Images) -> RgbaImage {
        render::assembly(self.doc, self.assembly, images)
    }
}

/// How each layer shows (its effective opacity, 0 when hidden), as level
/// thumbnails draw it: the view's layer visibility and opacity, which every
/// map tab shares. Make one per frame and hand it to every [`LevelThumb`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LayerLook {
    pub opacity: Vec<f32>,
    hash: u64,
}

impl LayerLook {
    pub fn of(doc: &Document, view: &ViewState) -> Self {
        let opacity: Vec<f32> = (0..doc.project.layers.len())
            .map(|i| view.layer_opacity(doc, i))
            .collect();
        Self::new(opacity)
    }

    /// Every layer fully shown (tests, tools without a view).
    pub fn all(doc: &Document) -> Self {
        Self::new(vec![1.0; doc.project.layers.len()])
    }

    fn new(opacity: Vec<f32>) -> Self {
        let bytes: Vec<u8> = opacity.iter().flat_map(|o| o.to_le_bytes()).collect();
        LayerLook {
            hash: worlds_core::autosave::hash(&bytes),
            opacity,
        }
    }
}

/// A level's thumbnail (the Levels pane's rows, the zoomed-out map).
pub struct LevelThumb<'a> {
    pub doc: &'a Document,
    pub ix: LevelIx,
    pub look: &'a LayerLook,
}

impl Source for LevelThumb<'_> {
    fn id(&self) -> ThumbId {
        ThumbId::Level(self.doc.ids[self.ix].clone())
    }

    /// The level's look stamp (edits, bakes), the tileset images and the
    /// layer opacities. The stamp and `images_gen` are unique across
    /// documents, so a reload never matches an old picture.
    fn key(&self) -> u64 {
        let mut b = Vec::with_capacity(24);
        b.extend(self.doc.look_stamp(self.ix).to_le_bytes());
        b.extend(self.doc.images_gen.to_le_bytes());
        b.extend(self.look.hash.to_le_bytes());
        worlds_core::autosave::hash(&b)
    }

    fn file(&self) -> Option<CacheFile> {
        None
    }

    fn render(&self, images: &mut Images) -> RgbaImage {
        render::level(self.doc, self.ix, &self.look.opacity, images)
    }
}
