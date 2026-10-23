//! Parameter parsing and lookups shared by the method groups.

use std::path::Path;

use serde::Deserialize;
use serde::de::{DeserializeOwned, Deserializer};
use serde_json::{Map, Value};
use worlds_core::{Command, CommandError, Id, LayerDef, Level, Project, Slug, World};

use crate::rpc::RpcError;

/// Deserialize params into a method's parameter struct (which denies
/// unknown keys); the error names the method and what is wrong.
pub(crate) fn parse<T: DeserializeOwned>(method: &str, params: Value) -> Result<T, RpcError> {
    serde_json::from_value(params).map_err(|e| RpcError::invalid_params(format!("{method}: {e}")))
}

/// The params as an object.
pub(crate) fn object(method: &str, params: Value) -> Result<Map<String, Value>, RpcError> {
    match params {
        Value::Object(m) => Ok(m),
        _ => Err(RpcError::invalid_params(format!(
            "{method}: params must be an object"
        ))),
    }
}

/// Params that are a command's fields: add `op` and parse the command.
pub(crate) fn command(method: &str, op: &str, params: Value) -> Result<Command, RpcError> {
    let mut m = object(method, params)?;
    m.insert("op".into(), op.into());
    parse(method, Value::Object(m))
}

/// A key that is present is `Some`, even when it is `null` (for "`null`
/// resets it" parameters).
pub(crate) fn present<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(d).map(Some)
}

/// A level and its world, by slug.
pub(crate) fn find_level<'p>(
    project: &'p Project,
    level: &str,
) -> Result<(&'p World, &'p Level), RpcError> {
    project
        .worlds
        .iter()
        .find_map(|w| w.level(level).map(|l| (w, l)))
        .ok_or_else(|| RpcError::command(&CommandError::UnknownLevel(slug_of(level))))
}

/// A layer definition by slug.
pub(crate) fn find_layer<'p>(project: &'p Project, layer: &str) -> Result<&'p LayerDef, RpcError> {
    project
        .layer(layer)
        .ok_or_else(|| RpcError::command(&CommandError::UnknownLayer(slug_of(layer))))
}

/// A layer of the kind a method needs.
pub(crate) fn layer_of_kind<'p>(
    project: &'p Project,
    layer: &str,
    kind: &'static str,
) -> Result<&'p LayerDef, RpcError> {
    let def = find_layer(project, layer)?;
    if def.kind.name() != kind {
        return Err(RpcError::command(&CommandError::WrongLayerKind {
            layer: def.slug.clone(),
            expected: kind,
            actual: def.kind.name(),
        }));
    }
    Ok(def)
}

/// A slug for an error message (the text is already a slug when it came
/// from a `Slug` parameter; anything else is reported as is).
pub(crate) fn slug_of(s: &str) -> Slug {
    Slug::new(s).unwrap_or_else(|_| worlds_core::slug("invalid"))
}

/// Fill in `world` for a level command from the level's slug; a given
/// `world` must be the level's (`level_not_in_world` otherwise).
pub(crate) fn fill_world(
    project: &Project,
    method: &str,
    m: &mut Map<String, Value>,
) -> Result<(), RpcError> {
    let Some(level) = m.get("level").and_then(Value::as_str).map(str::to_string) else {
        return Ok(()); // the command parse reports the missing key
    };
    let (world, _) = find_level(project, &level)?;
    match m.get("world") {
        None | Some(Value::Null) => {
            m.insert("world".into(), world.slug.as_str().into());
        }
        Some(Value::String(w)) if w == world.slug.as_str() => {}
        Some(Value::String(w)) => {
            return Err(RpcError::command(&CommandError::LevelNotInWorld {
                world: slug_of(w),
                level: slug_of(&level),
            }));
        }
        Some(_) => {
            return Err(RpcError::invalid_params(format!(
                "{method}: world must be a world slug"
            )));
        }
    }
    Ok(())
}

/// The world a new level or world-scope item goes in: `world` when given,
/// else the only world (an error naming them when there are several).
pub(crate) fn default_world(
    project: &Project,
    method: &str,
    world: Option<&str>,
) -> Result<Slug, RpcError> {
    match world {
        Some(w) => project
            .world(w)
            .map(|w| w.slug.clone())
            .ok_or_else(|| RpcError::command(&CommandError::UnknownWorld(slug_of(w)))),
        None => match project.worlds.as_slice() {
            [w] => Ok(w.slug.clone()),
            ws => Err(RpcError::invalid_params(format!(
                "{method}: give world: the project has {} worlds ({})",
                ws.len(),
                ws.iter()
                    .map(|w| w.slug.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))),
        },
    }
}

/// A fresh id no level, entity or note of the project uses.
pub(crate) fn fresh_id(project: &Project) -> Id {
    loop {
        let id = Id::generate();
        let used = project.worlds.iter().any(|w| {
            w.notes.iter().any(|n| n.id == id)
                || w.levels.iter().any(|l| {
                    l.id == id
                        || l.all_entities().any(|e| e.id == id)
                        || l.notes.iter().any(|n| n.id == id)
                })
        });
        if !used {
            return id;
        }
    }
}

/// Put an `id` into an object that may lack one; returns the id.
pub(crate) fn ensure_id(
    project: &Project,
    method: &str,
    key: &str,
    v: &mut Value,
) -> Result<Value, RpcError> {
    let Value::Object(m) = v else {
        return Err(RpcError::invalid_params(format!(
            "{method}: {key} must be an object"
        )));
    };
    match m.get("id") {
        Some(id @ Value::String(_)) => Ok(id.clone()),
        None | Some(Value::Null) => {
            let id = Value::String(fresh_id(project).to_string());
            m.insert("id".into(), id.clone());
            Ok(id)
        }
        Some(_) => Err(RpcError::invalid_params(format!(
            "{method}: {key}.id must be a 10-character id string"
        ))),
    }
}

/// A project-relative path with `/` separators.
pub(crate) fn rel_path(p: &Path) -> String {
    p.iter()
        .map(|c| c.to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// The layer's size in its own cells in `level`.
pub(crate) fn layer_size(project: &Project, def: &LayerDef, level: &Level) -> [u32; 2] {
    project.layer_cells(def, level)
}

/// `[x, y, w, h]` must be inside a `[w, h]` layer.
pub(crate) fn check_rect(layer: &LayerDef, rect: [u32; 4], size: [u32; 2]) -> Result<(), RpcError> {
    let [x, y, w, h] = rect;
    let end = [x as u64 + w as u64, y as u64 + h as u64];
    if end[0] > size[0] as u64 || end[1] > size[1] as u64 {
        let (cx, cy) = if end[0] > size[0] as u64 {
            (end[0] as i64 - 1, y as i64)
        } else {
            (x as i64, end[1] as i64 - 1)
        };
        return Err(RpcError::command(&CommandError::OutOfBounds {
            layer: layer.slug.clone(),
            x: cx,
            y: cy,
            w: size[0],
            h: size[1],
        }));
    }
    Ok(())
}
