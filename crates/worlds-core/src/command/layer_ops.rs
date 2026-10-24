//! Layer operations built from existing commands: merging a tiles layer
//! down into the one below it, and moving tile stacks or entities to
//! another layer of the same kind. Each returns one `batch` (one undo
//! entry); the editor, the live API and the MCP server all call these, so
//! they build the same commands. `duplicate_layer` is a command of its own.

use std::collections::BTreeMap;

use super::{Command, CommandError};
use crate::id::{Id, Slug};
use crate::model::{LayerDef, Project, TilePlacement};

type Result<T> = std::result::Result<T, CommandError>;

fn tiles_layer<'p>(p: &'p Project, slug: &Slug) -> Result<&'p LayerDef> {
    let def = p
        .layer(slug.as_str())
        .ok_or_else(|| CommandError::UnknownLayer(slug.clone()))?;
    if !def.is_tiles() {
        return Err(CommandError::WrongLayerKind {
            layer: slug.clone(),
            expected: "tiles",
            actual: def.kind.name(),
        });
    }
    Ok(def)
}

/// The layer right below `layer` in draw order, if it is a tiles layer on
/// the same grid: what [`merge_down`] merges into. The error says why not.
pub fn merge_target<'p>(p: &'p Project, layer: &Slug) -> Result<&'p LayerDef> {
    let upper = tiles_layer(p, layer)?;
    let i = p.layers.iter().position(|l| l.slug == *layer).unwrap_or(0);
    let Some(lower) = i.checked_sub(1).map(|j| &p.layers[j]) else {
        return Err(CommandError::Invalid(format!(
            "layer {layer} is the bottom layer; there is nothing to merge it down into"
        )));
    };
    if !lower.is_tiles() {
        let kind = lower.kind.name();
        let a = if kind.starts_with(['a', 'e', 'i', 'o', 'u']) {
            "an"
        } else {
            "a"
        };
        return Err(CommandError::Invalid(format!(
            "the layer below {layer} is {}, {a} {kind} layer; only a tiles layer merges into a \
             tiles layer",
            lower.slug,
        )));
    }
    if p.layer_grid(upper) != p.layer_grid(lower) {
        return Err(CommandError::Invalid(format!(
            "layer {layer} is on a {} px grid and {} on a {} px one; they can only merge on the \
             same grid",
            p.layer_grid(upper),
            lower.slug,
            p.layer_grid(lower)
        )));
    }
    Ok(lower)
}

/// Each cell's stack in a level's tiles layer.
fn stacks(p: &Project, level: &Slug, layer: &Slug) -> BTreeMap<[u32; 2], Vec<TilePlacement>> {
    let mut out: BTreeMap<[u32; 2], Vec<TilePlacement>> = BTreeMap::new();
    if let Some(t) = p
        .level(level.as_str())
        .and_then(|l| l.tiles_layer(layer.as_str()))
    {
        for t in &t.tiles {
            out.entry(t.cell).or_default().push(t.clone());
        }
    }
    out
}

/// Merge tiles layer `layer` down into the tiles layer right below it:
/// in every level, each of its stacks goes on top of the lower layer's
/// stack at that cell (`set_tiles`), then the layer goes
/// (`remove_layer_def`). Refused unless the layer below is a tiles layer on
/// the same grid. One `batch`, labelled "Merge <layer> down into <lower>".
pub fn merge_down(p: &Project, layer: &Slug) -> Result<Command> {
    let lower = merge_target(p, layer)?.slug.clone();
    let mut commands = Vec::new();
    for l in p.levels() {
        let upper = stacks(p, &l.slug, layer);
        if upper.is_empty() {
            continue;
        }
        let mut below = stacks(p, &l.slug, &lower);
        let mut tiles = Vec::new();
        for (cell, stack) in upper {
            tiles.extend(below.remove(&cell).unwrap_or_default());
            tiles.extend(stack);
        }
        commands.push(Command::SetTiles {
            level: l.slug.clone(),
            layer: lower.clone(),
            tiles,
            clear: Vec::new(),
            keep_empty: false,
        });
    }
    commands.push(Command::RemoveLayerDef {
        layer: layer.clone(),
    });
    Ok(Command::Batch {
        label: format!("Merge {layer} down into {lower}"),
        commands,
    })
}

/// Move the tile stacks at `cells` of tiles layer `from` in `level` to
/// tiles layer `to` (same grid), on top of what `to` holds there. Cells
/// with no tiles are skipped. One `batch` of two `set_tiles`, labelled
/// "Move <n> tile stacks to <to>"; a no-op batch when no cell has tiles.
pub fn move_tiles(
    p: &Project,
    level: &Slug,
    from: &Slug,
    to: &Slug,
    cells: &[[u32; 2]],
) -> Result<Command> {
    let (a, b) = (tiles_layer(p, from)?, tiles_layer(p, to)?);
    if p.level(level.as_str()).is_none() {
        return Err(CommandError::UnknownLevel(level.clone()));
    }
    if from == to {
        return Err(CommandError::Invalid(format!(
            "the tiles are already on layer {to}"
        )));
    }
    if p.layer_grid(a) != p.layer_grid(b) {
        return Err(CommandError::Invalid(format!(
            "layer {from} is on a {} px grid and {to} on a {} px one; tiles move only between \
             layers on the same grid",
            p.layer_grid(a),
            p.layer_grid(b)
        )));
    }
    let mut src = stacks(p, level, from);
    let mut dst = stacks(p, level, to);
    let mut tiles = Vec::new();
    let mut clear = Vec::new();
    for c in cells {
        let Some(stack) = src.remove(c) else { continue };
        tiles.extend(dst.remove(c).unwrap_or_default());
        tiles.extend(stack);
        clear.push(*c);
    }
    if clear.is_empty() {
        return Ok(Command::noop());
    }
    let n = clear.len();
    Ok(Command::Batch {
        label: if n == 1 {
            format!("Move 1 tile stack to {to}")
        } else {
            format!("Move {n} tile stacks to {to}")
        },
        commands: vec![
            Command::SetTiles {
                level: level.clone(),
                layer: to.clone(),
                tiles,
                clear: Vec::new(),
                keep_empty: false,
            },
            Command::SetTiles {
                level: level.clone(),
                layer: from.clone(),
                tiles: Vec::new(),
                clear,
                keep_empty: false,
            },
        ],
    })
}

/// Move entities to entities layer `to` of their level (same grid as the
/// layer each is on), keeping id, cell and fields: per entity a
/// `remove_entity` and an `add_entity` (at the end of `to`'s list). Ones
/// already on `to` are skipped. One `batch`, labelled "Move <id> to <to>"
/// or "Move <n> entities to <to>".
pub fn move_entities(p: &Project, ids: &[Id], to: &Slug) -> Result<Command> {
    let target = p
        .layer(to.as_str())
        .ok_or_else(|| CommandError::UnknownLayer(to.clone()))?;
    if !target.is_entities() {
        return Err(CommandError::WrongLayerKind {
            layer: to.clone(),
            expected: "entities",
            actual: target.kind.name(),
        });
    }
    let mut commands = Vec::new();
    let mut moved = Vec::new();
    for id in ids {
        let found = p.levels().find_map(|l| {
            l.entities.iter().find_map(|el| {
                el.entities
                    .iter()
                    .find(|e| e.id == *id)
                    .map(|e| (l, &el.layer, e))
            })
        });
        let Some((level, layer, e)) = found else {
            return Err(CommandError::UnknownEntity(id.clone()));
        };
        if layer == to || moved.contains(&id) {
            continue;
        }
        let src = p
            .layer(layer.as_str())
            .ok_or_else(|| CommandError::UnknownLayer(layer.clone()))?;
        if p.layer_grid(src) != p.layer_grid(target) {
            return Err(CommandError::Invalid(format!(
                "entity {id} is on layer {layer}, a {} px grid, and {to} is on a {} px one; \
                 entities move only between layers on the same grid",
                p.layer_grid(src),
                p.layer_grid(target)
            )));
        }
        let size = p.layer_cells(target, level);
        let inside = e.cell[0] >= 0
            && e.cell[1] >= 0
            && (e.cell[0] as u32) < size[0]
            && (e.cell[1] as u32) < size[1];
        // One a file put in a non-rectangular level's void moves as it is,
        // like one a resize left outside.
        let in_void = p.layer_cell_void(target, level, e.cell[0] as i64, e.cell[1] as i64);
        commands.push(Command::RemoveEntity {
            id: id.clone(),
            keep_empty: false,
        });
        commands.push(Command::AddEntity {
            level: level.slug.clone(),
            layer: to.clone(),
            entity: e.clone(),
            index: None,
            allow_outside: !inside || in_void,
        });
        moved.push(id);
    }
    let label = match moved.as_slice() {
        [] => return Ok(Command::noop()),
        [one] => format!("Move {one} to {to}"),
        many => format!("Move {} entities to {to}", many.len()),
    };
    Ok(Command::Batch { label, commands })
}
