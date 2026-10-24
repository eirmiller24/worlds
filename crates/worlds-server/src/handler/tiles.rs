//! `tiles.*`: `tiles` layers, placements as in the layer files.

use serde::Deserialize;
use serde_json::{Value, json};
use worlds_core::command::Rect;
use worlds_core::find::{self, FindKind};
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

/// How many placements `tiles.find` returns by default.
const FIND_LIMIT: usize = 1000;

/// `tiles.find { query?, tileset?, tile?, world?, level?, layer?, limit? }`:
/// hand-placed tiles across the project that every filter holds for
/// ([`worlds_core::find`]), level by level, each placement as in the layer
/// file plus its `world`, `level` and `layer`; at most `limit` (default
/// 1000), `total` counting them all.
pub(crate) fn find(project: &Project, method: &str, p: Value) -> Result<Value, RpcError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        query: Option<String>,
        tileset: Option<String>,
        tile: Option<[u32; 2]>,
        world: Option<String>,
        level: Option<Slug>,
        layer: Option<Slug>,
        limit: Option<usize>,
    }
    let p: P = params::parse(method, p)?;
    if let Some(l) = &p.level {
        find_level(project, l.as_str())?;
    }
    if let Some(l) = &p.layer {
        layer_of_kind(project, l.as_str(), "tiles")?;
    }
    let mut q = params::query(method, p.query.as_deref(), FindKind::Tiles)?;
    q.tilesets.extend(p.tileset);
    q.tiles.extend(p.tile);
    q.worlds.extend(p.world);
    q.levels.extend(p.level.map(|l| l.to_string()));
    q.layers.extend(p.layer.map(|l| l.to_string()));
    let hits =
        find::tiles(project, &q).map_err(|e| RpcError::invalid_params(format!("{method}: {e}")))?;
    let total = hits.len();
    let tiles: Vec<Value> = hits
        .into_iter()
        .take(p.limit.unwrap_or(FIND_LIMIT))
        .map(|h| {
            let mut v = json!({ "world": h.world.slug, "level": h.level.slug, "layer": h.layer });
            if let (Value::Object(m), Ok(Value::Object(t))) = (&mut v, serde_json::to_value(h.tile))
            {
                m.extend(t);
            }
            v
        })
        .collect();
    Ok(json!({ "tiles": tiles, "total": total }))
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
