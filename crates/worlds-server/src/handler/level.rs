//! `level.*`. Levels are addressed by slug; the server fills in `world`.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::{Value, json};
use worlds_core::command::LevelContent;
use worlds_core::format::IntGridFile;
use worlds_core::text::{NoteFilter, render_level_baked_with};
use worlds_core::{Color, Command, CommandError, Level, NoteStatus, Project, Slug};

use super::params::{self, default_world, ensure_id, fill_world, find_level, present};
use super::{Confirm, Handler, Write, with_rev};
use crate::host::Host;
use crate::rpc::RpcError;

/// `level.list { world? }`.
pub(crate) fn list(project: &Project, method: &str, p: Value) -> Result<Value, RpcError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        world: Option<Slug>,
    }
    let p: P = params::parse(method, p)?;
    if let Some(w) = &p.world
        && project.world(w.as_str()).is_none()
    {
        return Err(RpcError::command(&CommandError::UnknownWorld(w.clone())));
    }
    let levels: Vec<Value> = project
        .worlds
        .iter()
        .filter(|w| p.world.as_ref().is_none_or(|x| *x == w.slug))
        .flat_map(|w| {
            w.levels.iter().map(move |l| {
                json!({ "slug": l.slug, "name": l.name, "id": l.id, "world": w.slug,
                    "world_px": l.world_px, "size": l.size, "depth": l.depth })
            })
        })
        .collect();
    Ok(json!({ "levels": levels }))
}

/// `level.get { level, layers?, format?, notes? }`. Derived layers show
/// their effective grids (from the host's bake cache).
pub(crate) fn get(
    h: &mut Handler,
    host: &mut dyn Host,
    method: &str,
    p: Value,
) -> Result<Value, RpcError> {
    #[derive(Deserialize, Default, Clone, Copy, PartialEq)]
    #[serde(rename_all = "snake_case")]
    enum Format {
        #[default]
        Json,
        Text,
    }
    #[derive(Deserialize, Default, Clone, Copy)]
    #[serde(rename_all = "snake_case")]
    enum Notes {
        #[default]
        All,
        Open,
        None,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        level: Slug,
        layers: Option<Vec<Slug>>,
        #[serde(default)]
        format: Format,
        #[serde(default)]
        notes: Notes,
    }
    let p: P = params::parse(method, p)?;
    let mut doc = host.doc();
    find_level(doc.project, p.level.as_str())?;
    if let Some(ls) = &p.layers {
        for l in ls {
            params::find_layer(doc.project, l.as_str())?;
        }
    }
    let shown = |layer: &Slug| p.layers.as_ref().is_none_or(|ls| ls.contains(layer));
    // Effective grids of the derived layers shown (those that bake).
    let derived = super::baked::derived_grids(&mut doc, p.level.as_str(), shown);
    let project: &Project = doc.project;
    let (world, level) = find_level(project, p.level.as_str())?;
    if p.format == Format::Text {
        let names: Option<Vec<&str>> = p
            .layers
            .as_ref()
            .map(|ls| ls.iter().map(Slug::as_str).collect());
        let filter = match p.notes {
            Notes::All => NoteFilter::All,
            Notes::Open => NoteFilter::Open,
            Notes::None => NoteFilter::None,
        };
        let text = render_level_baked_with(
            project,
            p.level.as_str(),
            names.as_deref(),
            filter,
            &derived,
        )?;
        return with_rev(h.rev, json!({ "text": text }));
    }
    let mut int_grids = Vec::new();
    for g in &level.int_grids {
        if !shown(&g.layer) {
            continue;
        }
        let Some(def) = project.layer(g.layer.as_str()) else {
            continue;
        };
        let file = IntGridFile::from_layer(g, def).map_err(|m| {
            RpcError::invalid(format!("layer {} of level {}: {m}", g.layer, level.slug))
        })?;
        int_grids.push(file);
    }
    let content = LevelContent {
        int_grids,
        tiles: level
            .tiles
            .iter()
            .filter(|t| shown(&t.layer))
            .cloned()
            .collect(),
        entities: level.entities.clone(),
        notes: level
            .notes
            .iter()
            .filter(|n| match p.notes {
                Notes::All => true,
                Notes::Open => n.status == NoteStatus::Open,
                Notes::None => false,
            })
            .cloned()
            .collect(),
    };
    let mut out = json!({ "world": world.slug, "level": level, "content": content });
    let mut derived_json = serde_json::Map::new();
    for def in project
        .layers
        .iter()
        .filter(|d| d.is_derived() && shown(&d.slug))
    {
        let mut e = json!({ "writer": project.derived_writer(def.slug.as_str()) });
        match derived.get(&def.slug) {
            Some(g) => e["rows"] = json!(g.to_rows(def).map_err(RpcError::invalid)?),
            None => {
                e["rows"] = Value::Null;
                e["error"] = json!(format!(
                    "the rules of {} didn't bake, so the effective grid is unknown",
                    project
                        .derived_writer(def.slug.as_str())
                        .map_or("its writer", |w| w.as_str())
                ));
            }
        }
        if let Some(o) = level.overrides(def.slug.as_str()).filter(|o| !o.is_empty()) {
            e["overrides"] = json!(o.to_rows(def).map_err(RpcError::invalid)?);
        }
        derived_json.insert(def.slug.to_string(), e);
    }
    if !derived_json.is_empty() {
        out["derived"] = Value::Object(derived_json);
    }
    with_rev(h.rev, out)
}

/// `level.create { world?, level, content? }` → `create_level`.
pub(crate) fn create(project: &Project, method: &str, p: Value) -> Result<Write, RpcError> {
    let mut m = params::object(method, p)?;
    for k in m.keys() {
        if !["world", "level", "content"].contains(&k.as_str()) {
            return Err(RpcError::invalid_params(format!(
                "{method}: unknown key {k:?}; the keys are world, level, content"
            )));
        }
    }
    let world = default_world(project, method, m.get("world").and_then(Value::as_str))?;
    let mut level = m
        .remove("level")
        .ok_or_else(|| RpcError::invalid_params(format!("{method}: missing level")))?;
    let id = ensure_id(project, method, "level", &mut level)?;
    let level: Level = params::parse(method, level)?;
    let content: LevelContent = match m.remove("content") {
        None | Some(Value::Null) => LevelContent::default(),
        Some(c) => params::parse(method, c)?,
    };
    Ok(Write::Commands {
        cmds: vec![Command::CreateLevel {
            world,
            level,
            content,
            restore: false,
        }],
        id: Some(id),
        confirm: None,
    })
}

fn level_command(project: &Project, method: &str, op: &str, p: Value) -> Result<Command, RpcError> {
    let mut m = params::object(method, p)?;
    fill_world(project, method, &mut m)?;
    params::command(method, op, Value::Object(m))
}

/// `level.delete { level, confirm: true }` → `delete_level`.
pub(crate) fn delete(project: &Project, method: &str, p: Value) -> Result<Write, RpcError> {
    let cmd = level_command(project, method, "delete_level", p)?;
    let confirm = match &cmd {
        Command::DeleteLevel { level, .. } => Some(Confirm::new(
            format!("deletes level {level}"),
            format!("deleting level {level} needs confirm: true"),
        )),
        _ => None,
    };
    Ok(Write::Commands {
        cmds: vec![cmd],
        id: None,
        confirm,
    })
}

/// `level.move { level, world_px }` → `move_level`.
pub(crate) fn move_level(project: &Project, method: &str, p: Value) -> Result<Write, RpcError> {
    Ok(Write::one(level_command(project, method, "move_level", p)?))
}

/// `level.resize { level, size, anchor? }` → `resize_level`.
pub(crate) fn resize(project: &Project, method: &str, p: Value) -> Result<Write, RpcError> {
    Ok(Write::one(level_command(
        project,
        method,
        "resize_level",
        p,
    )?))
}

/// `level.update { level, slug?, name?, depth?, bg_color?, tags? }`.
pub(crate) fn update(project: &Project, method: &str, p: Value) -> Result<Write, RpcError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        level: Slug,
        world: Option<Slug>,
        slug: Option<Slug>,
        name: Option<String>,
        depth: Option<i32>,
        #[serde(default, deserialize_with = "present")]
        bg_color: Option<Option<Color>>,
        tags: Option<Vec<String>>,
    }
    let p: P = params::parse(method, p)?;
    let (world, level) = find_level(project, p.level.as_str())?;
    check_world(&p.world, &world.slug, &p.level)?;
    let world = world.slug.clone();
    let mut cmds = Vec::new();
    let mut slug = p.level.clone();
    if p.slug.is_some() || p.name.is_some() {
        let new = p.slug.unwrap_or_else(|| p.level.clone());
        cmds.push(Command::RenameLevel {
            world: world.clone(),
            level: p.level.clone(),
            slug: new.clone(),
            name: p.name.unwrap_or_else(|| level.name.clone()),
        });
        slug = new;
    }
    if let Some(depth) = p.depth {
        cmds.push(Command::SetLevelDepth {
            world: world.clone(),
            level: slug.clone(),
            depth,
        });
    }
    if let Some(bg_color) = p.bg_color {
        cmds.push(Command::SetLevelBg {
            world: world.clone(),
            level: slug.clone(),
            bg_color,
        });
    }
    if let Some(tags) = p.tags {
        cmds.push(Command::SetLevelTags {
            world,
            level: slug,
            tags,
        });
    }
    if cmds.is_empty() {
        return Err(RpcError::invalid_params(format!(
            "{method}: nothing to update; give slug, name, depth, bg_color or tags"
        )));
    }
    Ok(Write::cmds(cmds))
}

fn check_world(given: &Option<Slug>, actual: &Slug, level: &Slug) -> Result<(), RpcError> {
    match given {
        Some(w) if w != actual => Err(RpcError::command(&CommandError::LevelNotInWorld {
            world: w.clone(),
            level: level.clone(),
        })),
        _ => Ok(()),
    }
}

/// `level.set_fields { level, fields?, unset? }` → `set_level_field`s.
pub(crate) fn set_fields(project: &Project, method: &str, p: Value) -> Result<Write, RpcError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        level: Slug,
        world: Option<Slug>,
        #[serde(default)]
        fields: BTreeMap<Slug, Value>,
        #[serde(default)]
        unset: Vec<Slug>,
    }
    let p: P = params::parse(method, p)?;
    let (world, _) = find_level(project, p.level.as_str())?;
    check_world(&p.world, &world.slug, &p.level)?;
    let mut cmds: Vec<Command> = p
        .fields
        .into_iter()
        .map(|(field, value)| Command::SetLevelField {
            world: world.slug.clone(),
            level: p.level.clone(),
            field,
            value: Some(value),
            allow_unknown: false,
        })
        .collect();
    cmds.extend(p.unset.into_iter().map(|field| Command::SetLevelField {
        world: world.slug.clone(),
        level: p.level.clone(),
        field,
        value: None,
        allow_unknown: false,
    }));
    if cmds.is_empty() {
        return Err(RpcError::invalid_params(format!(
            "{method}: nothing to set; give fields or unset"
        )));
    }
    Ok(Write::cmds(cmds))
}
