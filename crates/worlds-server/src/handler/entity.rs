//! `entity.*`. Entities are addressed by id anywhere in the project.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::{Map, Value, json};
use worlds_core::command::Rect;
use worlds_core::find::{self, FindKind};
use worlds_core::{Command, CommandError, EntityInstance, Id, Level, Project, Slug, World};

use super::Write;
use super::params::{self, ensure_id, find_level, present};
use crate::rpc::RpcError;

/// Where an entity lives.
struct Found<'p> {
    world: &'p World,
    level: &'p Level,
    layer: &'p Slug,
    index: usize,
    entity: &'p EntityInstance,
}

fn all(project: &Project) -> impl Iterator<Item = Found<'_>> {
    project.worlds.iter().flat_map(|w| {
        w.levels.iter().flat_map(move |l| {
            l.entities.iter().flat_map(move |el| {
                el.entities
                    .iter()
                    .enumerate()
                    .map(move |(index, entity)| Found {
                        world: w,
                        level: l,
                        layer: &el.layer,
                        index,
                        entity,
                    })
            })
        })
    })
}

/// An entity on the wire.
fn wire(f: &Found<'_>) -> Value {
    let e = f.entity;
    json!({ "id": e.id, "type": e.ty, "level": f.level.slug, "world": f.world.slug,
        "layer": f.layer, "cell": e.cell, "offset_px": e.offset_px, "size": e.size,
        "fields": e.fields })
}

/// Every field of the entity's type, stored value or default (`null` when
/// it has neither), plus stored values of fields the type doesn't define.
fn effective(project: &Project, e: &EntityInstance) -> Map<String, Value> {
    let mut out = Map::new();
    if let Some(def) = project.entity_def(e.ty.as_str()) {
        for f in &def.fields {
            let v = e
                .fields
                .get(&f.slug)
                .cloned()
                .or_else(|| f.default.clone())
                .unwrap_or(Value::Null);
            out.insert(f.slug.to_string(), v);
        }
    }
    for (k, v) in &e.fields {
        out.entry(k.to_string()).or_insert_with(|| v.clone());
    }
    out
}

/// `entity.list { level?, layer?, type? }`, in file order.
pub(crate) fn list(project: &Project, method: &str, p: Value) -> Result<Value, RpcError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        level: Option<Slug>,
        layer: Option<Slug>,
        #[serde(rename = "type")]
        ty: Option<Slug>,
    }
    let p: P = params::parse(method, p)?;
    if let Some(l) = &p.level {
        find_level(project, l.as_str())?;
    }
    if let Some(l) = &p.layer {
        params::layer_of_kind(project, l.as_str(), "entities")?;
    }
    let entities: Vec<Value> = all(project)
        .filter(|f| p.level.as_ref().is_none_or(|l| *l == f.level.slug))
        .filter(|f| p.layer.as_ref().is_none_or(|l| l == f.layer))
        .filter(|f| p.ty.as_ref().is_none_or(|t| *t == f.entity.ty))
        .map(|f| wire(&f))
        .collect();
    Ok(json!({ "entities": entities }))
}

/// `entity.get { id }` → `{ entity, index, effective_fields }`.
pub(crate) fn get(project: &Project, method: &str, p: Value) -> Result<Value, RpcError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        id: Id,
    }
    let p: P = params::parse(method, p)?;
    let f = all(project)
        .find(|f| f.entity.id == p.id)
        .ok_or_else(|| RpcError::command(&CommandError::UnknownEntity(p.id.clone())))?;
    Ok(json!({ "entity": wire(&f), "index": f.index,
        "effective_fields": effective(project, f.entity) }))
}

/// `entity.find { query?, type?, tag?, fields?, world?, level?, layer?,
/// text?, rect? }`: every filter must hold ([`worlds_core::find`]).
pub(crate) fn find(project: &Project, method: &str, p: Value) -> Result<Value, RpcError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        query: Option<String>,
        #[serde(rename = "type")]
        ty: Option<String>,
        tag: Option<String>,
        #[serde(default)]
        fields: BTreeMap<String, Value>,
        world: Option<String>,
        level: Option<Slug>,
        layer: Option<String>,
        text: Option<String>,
        rect: Option<[u32; 4]>,
    }
    let p: P = params::parse(method, p)?;
    if p.rect.is_some() && p.level.is_none() {
        return Err(RpcError::invalid_params(format!(
            "{method}: rect needs level (it is in the entities layer's cells of that level)"
        )));
    }
    if let Some(l) = &p.level {
        find_level(project, l.as_str())?;
    }
    let mut q = params::query(method, p.query.as_deref(), FindKind::Entities)?;
    q.types.extend(p.ty);
    q.tags.extend(p.tag);
    q.fields.extend(params::field_tests(p.fields));
    q.worlds.extend(p.world);
    q.levels.extend(p.level.map(|l| l.to_string()));
    q.layers.extend(p.layer);
    q.text.extend(p.text.map(|t| t.to_lowercase()));
    let rect = p.rect.map(Rect::from);
    let entities: Vec<Value> = find::entities(project, &q)
        .map_err(|e| RpcError::invalid_params(format!("{method}: {e}")))?
        .into_iter()
        .filter(|h| {
            rect.is_none_or(|r| {
                let [x, y] = h.entity.cell;
                x >= 0 && y >= 0 && r.contains([x as u32, y as u32])
            })
        })
        .map(|h| {
            wire(&Found {
                world: h.world,
                level: h.level,
                layer: h.layer,
                index: h.index,
                entity: h.entity,
            })
        })
        .collect();
    Ok(json!({ "entities": entities }))
}

/// `entity.add { level, layer?, entity, index? }` → `add_entity`.
pub(crate) fn add(project: &Project, method: &str, p: Value) -> Result<Write, RpcError> {
    let mut m = params::object(method, p)?;
    for k in m.keys() {
        if !["level", "layer", "entity", "index"].contains(&k.as_str()) {
            return Err(RpcError::invalid_params(format!(
                "{method}: unknown key {k:?}; the keys are level, layer, entity, index"
            )));
        }
    }
    if !m.contains_key("layer") {
        let layers: Vec<&Slug> = project
            .layers
            .iter()
            .filter(|l| l.is_entities())
            .map(|l| &l.slug)
            .collect();
        match layers.as_slice() {
            [one] => {
                m.insert("layer".into(), one.as_str().into());
            }
            many => {
                return Err(RpcError::invalid_params(format!(
                    "{method}: give layer: the project has {} entities layers ({})",
                    many.len(),
                    many.iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )));
            }
        }
    }
    let mut entity = m
        .remove("entity")
        .ok_or_else(|| RpcError::invalid_params(format!("{method}: missing entity")))?;
    let id = ensure_id(project, method, "entity", &mut entity)?;
    m.insert("entity".into(), entity);
    let cmd = params::command(method, "add_entity", Value::Object(m))?;
    Ok(Write::Commands {
        cmds: vec![cmd],
        id: Some(id),
        confirm: None,
    })
}

/// `entity.update { id, fields?, unset?, size? }` → `set_entity_field`s and
/// `resize_entity` (`size: null` resets it).
pub(crate) fn update(_: &Project, method: &str, p: Value) -> Result<Write, RpcError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        id: Id,
        #[serde(default)]
        fields: BTreeMap<Slug, Value>,
        #[serde(default)]
        unset: Vec<Slug>,
        #[serde(default, deserialize_with = "present")]
        size: Option<Option<[u32; 2]>>,
    }
    let p: P = params::parse(method, p)?;
    let mut cmds: Vec<Command> = p
        .fields
        .into_iter()
        .map(|(field, value)| Command::SetEntityField {
            id: p.id.clone(),
            field,
            value: Some(value),
            allow_unknown: false,
        })
        .collect();
    cmds.extend(p.unset.into_iter().map(|field| Command::SetEntityField {
        id: p.id.clone(),
        field,
        value: None,
        allow_unknown: false,
    }));
    if let Some(size) = p.size {
        cmds.push(Command::ResizeEntity {
            id: p.id.clone(),
            size,
        });
    }
    if cmds.is_empty() {
        return Err(RpcError::invalid_params(format!(
            "{method}: nothing to update; give fields, unset or size"
        )));
    }
    Ok(Write::cmds(cmds))
}

/// `entity.move { id, cell, offset_px? }` → `move_entity`.
pub(crate) fn move_entity(_: &Project, method: &str, p: Value) -> Result<Write, RpcError> {
    Ok(Write::one(params::command(method, "move_entity", p)?))
}

/// `entity.remove { ids }` → `remove_entity`s.
pub(crate) fn remove(_: &Project, method: &str, p: Value) -> Result<Write, RpcError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        ids: Vec<Id>,
    }
    let p: P = params::parse(method, p)?;
    if p.ids.is_empty() {
        return Err(RpcError::invalid_params(format!("{method}: ids is empty")));
    }
    Ok(Write::cmds(
        p.ids
            .into_iter()
            .map(|id| Command::RemoveEntity {
                id,
                keep_empty: false,
            })
            .collect(),
    ))
}
