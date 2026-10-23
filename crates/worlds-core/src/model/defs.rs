//! Tilesets (`tilesets.json`) and enums (`enums.json`).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::{Color, TileRef, is_default};
use crate::id::Slug;

/// One entry of `tilesets.json`. Image dimensions are read from the PNG, not stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TilesetDef {
    pub slug: Slug,
    /// Path to the PNG, relative to the project directory, `/`-separated.
    pub image: String,
    /// `[width, height]` of one tile in pixels. Stamp layouts step by this
    /// size; it may differ from any layer's grid.
    pub tile_size: [u32; 2],
    /// Pixels between adjacent tiles. Default 0.
    #[serde(default, skip_serializing_if = "is_default")]
    pub spacing: u32,
    /// Pixels between the image edge and the first tile. Default 0.
    #[serde(default, skip_serializing_if = "is_default")]
    pub padding: u32,
}

impl TilesetDef {
    /// Tile columns in an image `image_width` px wide, as LDtk counts them:
    /// `ceil((width - 2 * padding) / (tile_w + spacing))`, so a partial last
    /// column counts. 0 for a zero-sized tile step.
    pub fn columns(&self, image_width: u32) -> u32 {
        grid_count(image_width, self.tile_size[0], self.padding, self.spacing)
    }

    /// Tile rows in an image `image_height` px tall (see [`Self::columns`]).
    pub fn rows(&self, image_height: u32) -> u32 {
        grid_count(image_height, self.tile_size[1], self.padding, self.spacing)
    }

    /// Pixel position of the top-left corner of atlas tile `[col, row]`:
    /// `padding + col * (tile + spacing)` per axis.
    pub fn tile_px(&self, tile: [u32; 2]) -> [u32; 2] {
        let at = |a: usize| self.padding + tile[a] * (self.tile_size[a] + self.spacing);
        [at(0), at(1)]
    }

    /// The atlas tile whose cell holds pixel `px` (the inverse of
    /// [`Self::tile_px`]): `(px - padding) / (tile + spacing)` per axis,
    /// clamped at 0 inside the padding.
    pub fn tile_at_px(&self, px: [u32; 2]) -> [u32; 2] {
        let at = |a: usize| {
            let step = self.tile_size[a] + self.spacing;
            px[a]
                .saturating_sub(self.padding)
                .checked_div(step)
                .unwrap_or(0)
        };
        [at(0), at(1)]
    }
}

fn grid_count(size: u32, tile: u32, padding: u32, spacing: u32) -> u32 {
    let step = tile + spacing;
    if step == 0 {
        0
    } else {
        size.saturating_sub(2 * padding).div_ceil(step)
    }
}

/// One entry of `enums.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EnumDef {
    pub slug: Slug,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub values: Vec<EnumValue>,
}

impl EnumDef {
    pub fn has_value(&self, value: &str) -> bool {
        self.values.iter().any(|v| v.slug == value)
    }
}

/// One value of an enum.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EnumValue {
    pub slug: Slug,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<Color>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<TileRef>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tile_grid_arithmetic() {
        let t = TilesetDef {
            slug: crate::id::slug("t"),
            image: "t.png".into(),
            tile_size: [16, 8],
            spacing: 4,
            padding: 8,
        };
        // (100 - 16) / 20 = 4.2: the partial fifth column counts, as in LDtk.
        assert_eq!(t.columns(100), 5);
        assert_eq!(t.rows(16 + 3 * 12), 3);
        assert_eq!(t.columns(10), 0);
        assert_eq!(t.tile_px([5, 3]), [108, 44]);
        for tile in [[0, 0], [5, 3], [2, 7]] {
            assert_eq!(t.tile_at_px(t.tile_px(tile)), tile);
        }
        // Anywhere inside a tile (or in the padding before the first) maps to it.
        assert_eq!(t.tile_at_px([108 + 15, 44 + 7]), [5, 3]);
        assert_eq!(t.tile_at_px([0, 0]), [0, 0]);
        let zero = TilesetDef {
            tile_size: [0, 0],
            spacing: 0,
            ..t
        };
        assert_eq!((zero.columns(64), zero.tile_at_px([5, 5])), (0, [0, 0]));
    }
}
