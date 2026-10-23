//! Autotile engine for Worlds: turns IntGrid cells into tile placements by
//! running an `auto` layer's rules (`rules/<layer>.json`).
//!
//! ```no_run
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let project = worlds_core::load("mygame.worlds")?;
//! let images = worlds_rules::TilesetImages::new("mygame.worlds", &project.tilesets);
//! let level = project.level("crevasse").unwrap();
//! for (layer, bake) in worlds_rules::bake_level(&project, level, &images)? {
//!     println!("{layer}: {} tiles", bake.placements.len());
//! }
//! # Ok(()) }
//! ```
//!
//! See the crate README for the pipeline, the hash streams and the break rule.

mod bake;
mod cache;
mod derived;
pub mod hash;
mod images;
mod perlin;
mod preview;
mod region;
mod warnings;

use std::collections::BTreeMap;
use std::path::PathBuf;

use worlds_core::{DerivedCycle, Flip, IntGridLayer, Level, Project, Slug};

pub use bake::{
    Matches, bake_layer, bake_level, group_applies, rule_matches, rule_matches_all,
    rule_matches_many,
};
pub use cache::{BakeCache, DirtyRect, LevelSeed, rules_fingerprint};
pub use derived::effective_grid;
pub use hash::layer_seed;
pub use images::{TileOpacity, TilesetImages};
pub use preview::{RegionPreview, RulePreview, preview_rules, preview_rules_for, region_preview};
pub use region::{EdgeRun, Edges, RegionInfo, Regions, regions};
pub use warnings::export_warnings;

/// One baked tile.
#[derive(Debug, Clone, PartialEq)]
pub struct Placement {
    /// Top-left of the tile in level-local pixels.
    pub px: [i32; 2],
    pub tileset: Slug,
    /// `[column, row]` in the tileset atlas.
    pub tile: [u32; 2],
    /// How the tile is turned; set when a mirrored or rotated variant of
    /// the pattern fired (bit 4, transpose, only from `rotate`).
    pub flip: Option<Flip>,
    /// Opacity, 0 to 1 (the rule's `out.alpha`).
    pub alpha: f64,
    /// The rule that placed it.
    pub rule: Slug,
    /// That rule's group.
    pub group: Slug,
    /// The cell the rule matched on. Stamp tiles and offset tiles can be drawn
    /// elsewhere; this is still the cell that decided them.
    pub anchor: [i32; 2],
}

/// The result of baking one `auto` layer in one level.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LayerBake {
    /// Every tile, in **draw order**: draw index 0 first (bottom), the last
    /// one on top. Passes stack bottom to top: pass 0's tiles first, the
    /// last pass's on top. Within a pass, like LDtk, rules earlier in the
    /// file end up on top: the list runs from the pass's last rule to its
    /// first rule, each rule's tiles by anchor cell in row-major order, and
    /// within one anchor by variant (as written, mirror_x, mirror_y, both,
    /// then the rotations) and then by the alternative's tile order.
    pub placements: Vec<Placement>,
    /// For each anchor cell, the rules that fired there (drew at least one
    /// tile or wrote a derived cell) after the break rule and occupancy, in
    /// run order. The LDtk parity harness compares this map.
    pub fired: BTreeMap<[i32; 2], Vec<Slug>>,
    /// The **computed** values of each derived grid this layer writes (what
    /// its `write` outputs produced, every pass laid over the earlier ones;
    /// 0 where nothing was written), by layer slug, in cells of the layer's
    /// grid. Overrides are not applied here: see [`effective_grid`] and
    /// [`BakeCache::derived`]. Empty for a layer that writes nothing.
    pub derived: BTreeMap<Slug, IntGridLayer>,
    /// The occupancy grid after the last pass, row-major in cells of the
    /// layer's grid. Empty when no rule of the layer checks
    /// (`avoid_overlap`) or marks (a stamp with `occupies`) occupancy.
    pub occupied: Vec<bool>,
}

/// Errors from baking. Most mean the project would also fail `validate`.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("unknown layer {0:?}")]
    UnknownLayer(String),
    #[error("unknown level {0:?}")]
    UnknownLevel(String),
    #[error("layer {0:?} is not an auto layer")]
    NotAutoLayer(String),
    #[error("rule {rule}: pattern grid {grid:?} is not an int_grid layer on this layer's grid")]
    BadGrid { rule: String, grid: String },
    #[error("rule {rule}: glyph {glyph:?} is not a built-in and not in the legend")]
    UnknownGlyph { rule: String, glyph: char },
    #[error("rule {rule}: {test:?} is not a value or group of layer {grid:?}")]
    UnknownCellTest {
        rule: String,
        test: String,
        grid: String,
    },
    #[error("rule {rule}: out_of_bounds {value:?} is not a value of layer {grid:?}")]
    UnknownOutOfBounds {
        rule: String,
        value: String,
        grid: String,
    },
    #[error("rule {0}: output names no tileset")]
    NoTileset(String),
    #[error("unknown tileset {0:?}")]
    UnknownTileset(String),
    #[error("rule {rule}: unknown stamp {stamp:?}")]
    UnknownStamp { rule: String, stamp: String },
    /// The layer's rules can't be expanded (an unknown template, named
    /// pattern or legend; the message names it).
    #[error("layer {layer}: {source}")]
    Expand {
        layer: String,
        source: worlds_core::ExpandError,
    },
    /// A `rotate` rule draws from a tileset whose tiles aren't square
    /// (validation reports it too).
    #[error(
        "rule {rule}: rotate needs square tiles, but tileset {tileset:?} has {}x{} tiles",
        size[0],
        size[1]
    )]
    RotateNeedsSquareTiles {
        rule: String,
        tileset: String,
        size: [u32; 2],
    },
    #[error("cannot load tileset image {}: {message}", path.display())]
    Image { path: PathBuf, message: String },
    /// More than one auto layer writes a derived grid (one writer per
    /// derived grid; validation reports it too).
    #[error("derived grid {grid} is written by more than one auto layer: {}", layers.join(", "))]
    SecondWriter { grid: String, layers: Vec<String> },
    /// Auto layers read each other's derived grids, so no bake order exists.
    #[error("{0}")]
    DerivedCycle(DerivedCycle),
    /// A `write` names a grid that isn't a derived int_grid layer on the
    /// auto layer's grid, or a value that layer doesn't have.
    #[error("rule {rule}: write to {grid:?}: {reason}")]
    BadWrite {
        rule: String,
        grid: String,
        reason: String,
    },
    /// A region rule's `region.grid` isn't an int_grid layer on the auto
    /// layer's grid.
    #[error("rule {rule}: region grid {grid:?}: {reason}")]
    BadRegion {
        rule: String,
        grid: String,
        reason: String,
    },
    /// The layer reads a derived grid whose writer failed to bake.
    #[error(
        "layer {layer} reads a derived grid written by {upstream}, which failed to bake: {reason}"
    )]
    UpstreamFailed {
        layer: String,
        upstream: String,
        reason: String,
    },
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Bake every `auto` layer of `level` into the shape the game export takes
/// ([`worlds_core::export::write`]): the tiles of every auto layer and the
/// **effective** grid of every derived layer (overrides over what its
/// writer computed; overrides alone when no layer writes it).
pub fn bake_for_export(
    project: &Project,
    level: &Level,
    images: &TilesetImages,
) -> Result<worlds_core::export::BakedLevel> {
    let bakes = bake_level(project, level, images)?;
    let mut derived = BTreeMap::new();
    for def in project.layers.iter().filter(|l| l.is_derived()) {
        let computed = project
            .derived_writer(def.slug.as_str())
            .and_then(|w| bakes.get(w))
            .and_then(|b| b.derived.get(&def.slug));
        derived.insert(
            def.slug.clone(),
            derived::effective(project, level, def, computed),
        );
    }
    let tiles: worlds_core::export::BakedLayers = bakes
        .into_iter()
        .map(|(layer, bake)| {
            let tiles = bake
                .placements
                .into_iter()
                .map(|p| worlds_core::export::BakedTile {
                    px: p.px,
                    tileset: p.tileset,
                    tile: p.tile,
                    flip: p.flip,
                    alpha: p.alpha,
                })
                .collect();
            (layer, tiles)
        })
        .collect();
    Ok(worlds_core::export::BakedLevel { tiles, derived })
}

/// The rule groups of `layer` that run in `level`, in file order
/// (see [`group_applies`]).
pub fn applied_groups<'a>(project: &'a Project, level: &Level, layer: &str) -> Vec<&'a Slug> {
    project
        .rules_for(layer)
        .map(|set| {
            set.groups
                .iter()
                .filter(|g| group_applies(project, level, layer, g))
                .map(|g| &g.slug)
                .collect()
        })
        .unwrap_or_default()
}
