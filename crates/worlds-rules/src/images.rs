//! Tileset images, loaded lazily, reduced to what the bake needs: which
//! tiles are fully opaque (the implicit lock of the break pass).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use worlds_core::{Slug, TilesetDef};

use crate::Error;

/// Per-tile opacity of one tileset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TileOpacity {
    cols: u32,
    rows: u32,
    opaque: Vec<bool>,
}

impl TileOpacity {
    /// From explicit flags, row-major, `cols * rows` long (e.g. LDtk's cached
    /// `opaqueTiles` string, which the importer or parity harness can pass in).
    pub fn from_flags(cols: u32, rows: u32, opaque: Vec<bool>) -> Self {
        assert_eq!(opaque.len(), (cols * rows) as usize, "opacity flag count");
        TileOpacity { cols, rows, opaque }
    }

    /// Scan an RGBA image laid out as `def` describes.
    ///
    /// The tile grid is `ceil((image - 2 * padding) / (tile + spacing))` per
    /// axis, so a partial last column or row is part of the grid. A tile whose
    /// rect runs past the image edge is **not scanned and counts as opaque**,
    /// as LDtk does. Every other tile is opaque iff all its pixels have alpha 255.
    pub fn from_image(img: &image::RgbaImage, def: &TilesetDef) -> Self {
        let (w, h) = img.dimensions();
        let [tw, th] = def.tile_size;
        let (cols, rows) = (def.columns(w), def.rows(h));
        let mut opaque = Vec::with_capacity((cols * rows) as usize);
        for row in 0..rows {
            for col in 0..cols {
                let [x0, y0] = def.tile_px([col, row]);
                let inside = x0 + tw <= w && y0 + th <= h;
                opaque.push(
                    !inside
                        || (y0..y0 + th)
                            .all(|y| (x0..x0 + tw).all(|x| img.get_pixel(x, y)[3] == 255)),
                );
            }
        }
        TileOpacity { cols, rows, opaque }
    }

    /// `[cols, rows]` of the tile grid.
    pub fn grid(&self) -> [u32; 2] {
        [self.cols, self.rows]
    }

    /// Whether the tile at `[col, row]` is fully opaque. Tiles outside the
    /// grid (entirely beyond the image) draw nothing, so they are not opaque.
    pub fn is_opaque(&self, col: u32, row: u32) -> bool {
        col < self.cols && row < self.rows && self.opaque[(row * self.cols + col) as usize]
    }
}

/// Lazily loaded tileset images for one project.
///
/// Each tileset's PNG is read on first use and cached. Thread-safe, so a
/// worker pool can share one instance across level bakes.
pub struct TilesetImages {
    root: Option<PathBuf>,
    defs: BTreeMap<Slug, TilesetDef>,
    cache: Mutex<BTreeMap<Slug, Arc<TileOpacity>>>,
}

impl TilesetImages {
    /// Images under `project_dir` (tileset `image` paths are relative to it).
    /// Takes a copy of the tileset definitions; build a new one (or call
    /// [`Self::invalidate`]) when tilesets change.
    pub fn new(project_dir: impl AsRef<Path>, tilesets: &[TilesetDef]) -> Self {
        TilesetImages {
            root: Some(project_dir.as_ref().to_path_buf()),
            defs: tilesets
                .iter()
                .map(|t| (t.slug.clone(), t.clone()))
                .collect(),
            cache: Mutex::new(BTreeMap::new()),
        }
    }

    /// No files at all: only tilesets given to [`Self::insert`] are known.
    /// For tests and for callers that already have opacity data.
    pub fn in_memory() -> Self {
        TilesetImages {
            root: None,
            defs: BTreeMap::new(),
            cache: Mutex::new(BTreeMap::new()),
        }
    }

    /// Provide (or replace) a tileset's opacity without reading its image.
    pub fn insert(&self, tileset: Slug, opacity: TileOpacity) {
        self.lock().insert(tileset, Arc::new(opacity));
    }

    /// Forget a cached tileset so the next use reloads its image.
    pub fn invalidate(&self, tileset: &str) {
        self.lock().remove(tileset);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<Slug, Arc<TileOpacity>>> {
        // A panic while holding the lock can't leave the map half-written.
        self.cache.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The opacity table of a tileset, loading its image on first use.
    pub fn opacity(&self, tileset: &str) -> Result<Arc<TileOpacity>, Error> {
        if let Some(o) = self.lock().get(tileset) {
            return Ok(o.clone());
        }
        let (Some(root), Some(def)) = (&self.root, self.defs.get(tileset)) else {
            return Err(Error::UnknownTileset(tileset.to_string()));
        };
        let path = root.join(&def.image);
        let img = image::open(&path)
            .map_err(|e| Error::Image {
                path: path.clone(),
                message: e.to_string(),
            })?
            .to_rgba8();
        let o = Arc::new(TileOpacity::from_image(&img, def));
        self.lock().insert(def.slug.clone(), o.clone());
        Ok(o)
    }

    /// Whether tile `[col, row]` of `tileset` is fully opaque (every pixel
    /// alpha 255). A tile whose rect extends past the image edge counts as
    /// opaque, as in LDtk; a tile entirely outside the image does not.
    /// Errors if the image can't be loaded (the error names the path).
    pub fn is_opaque(&self, tileset: &str, col: u32, row: u32) -> Result<bool, Error> {
        Ok(self.opacity(tileset)?.is_opaque(col, row))
    }
}
