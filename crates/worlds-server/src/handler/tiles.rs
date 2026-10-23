//! `tiles.*`: `tiles` layers, placements as in the layer files.

use serde::Deserialize;
use serde_json::{Value, json};
use worlds_core::command::Rect;
use worlds_core::{Command, CommandError, Project, Slug, TilePlacement};

use super::Write;
use super::params::{self, find_level, layer_of_kind};
use crate::rpc::RpcError;

/// `tiles.get { level, layer, rect? }`: placements whose cell is in `rect`,
/// in file order.
pub(crate) fn get(project: &Project, method: &str, p: Value) -> Result<Value, RpcError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        level: Slug,
        layer: Slug,
        rect: Option<[u32; 4]>,
    }
    let p: P = params::parse(method, p)?;
    let (_, level) = find_level(project, p.level.as_str())?;
    layer_of_kind(project, p.layer.as_str(), "tiles")?;
    let rect = p.rect.map(Rect::from);
    let tiles: Vec<&TilePlacement> = level
        .tiles_layer(p.layer.as_str())
        .map(|t| {
            t.tiles
                .iter()
                .filter(|t| rect.is_none_or(|r| r.contains(t.cell)))
                .collect()
        })
        .unwrap_or_default();
    Ok(json!({ "tiles": tiles }))
}

/// `tiles.set` → `set_tiles`.
pub(crate) fn set(_: &Project, method: &str, p: Value) -> Result<Write, RpcError> {
    Ok(Write::one(params::command(method, "set_tiles", p)?))
}

/// `tiles.clear_rect` → `clear_tiles`.
pub(crate) fn clear_rect(_: &Project, method: &str, p: Value) -> Result<Write, RpcError> {
    Ok(Write::one(params::command(method, "clear_tiles", p)?))
}

/// `tiles.stamp { level, layer, at, tileset, rect }`: the atlas rect laid
/// down whole, one placement per atlas tile, stepping
/// `max(1, tile size / layer grid)` cells per tile.
pub(crate) fn stamp(project: &Project, method: &str, p: Value) -> Result<Write, RpcError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        level: Slug,
        layer: Slug,
        at: [u32; 2],
        tileset: Slug,
        rect: [u32; 4],
    }
    let p: P = params::parse(method, p)?;
    let def = layer_of_kind(project, p.layer.as_str(), "tiles")?;
    let ts = project
        .tileset(p.tileset.as_str())
        .ok_or_else(|| RpcError::command(&CommandError::UnknownTileset(p.tileset.clone())))?;
    let grid = project.layer_grid(def);
    let step = |a: usize| (ts.tile_size[a] / grid).max(1);
    let [col, row, w, h] = p.rect;
    if w == 0 || h == 0 {
        return Err(RpcError::invalid_params(format!(
            "{method}: rect [{col}, {row}, {w}, {h}] has no tiles"
        )));
    }
    let mut tiles = Vec::new();
    for j in 0..h {
        for i in 0..w {
            tiles.push(TilePlacement {
                cell: [
                    p.at[0].saturating_add(i * step(0)),
                    p.at[1].saturating_add(j * step(1)),
                ],
                tileset: p.tileset.clone(),
                tile: [col + i, row + j],
                flip: None,
                alpha: 1.0,
            });
        }
    }
    Ok(Write::one(Command::SetTiles {
        level: p.level,
        layer: p.layer,
        tiles,
        clear: Vec::new(),
        keep_empty: false,
    }))
}
