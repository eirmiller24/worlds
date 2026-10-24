//! `level.*`. Levels are addressed by slug; the server fills in `world`.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::{Value, json};
use worlds_core::command::LevelContent;
use worlds_core::find::{self, FindKind};
use worlds_core::format::IntGridFile;
use worlds_core::text::{NoteFilter, render_level_baked_with};
use worlds_core::{
    Color, Command, CommandError, GroupMode, LayerState, Level, NoteStatus, Project, Slug,
};

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

/// `level.find { query?, tag?, fields?, world?, text? }`: the levels every
/// filter holds for ([`worlds_core::find`]), in `level.list` order, each
/// as `level.list` gives it plus its `tags` and stored `fields`.
pub(crate) fn find(project: &Project, method: &str, p: Value) -> Result<Value, RpcError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        query: Option<String>,
        tag: Option<String>,
        #[serde(default)]
        fields: BTreeMap<String, Value>,
        world: Option<Slug>,
        text: Option<String>,
    }
    let p: P = params::parse(method, p)?;
    if let Some(w) = &p.world
        && project.world(w.as_str()).is_none()
    {
        return Err(RpcError::command(&CommandError::UnknownWorld(w.clone())));
    }
    let mut q = params::query(method, p.query.as_deref(), FindKind::Levels)?;
    q.tags.extend(p.tag);
    q.fields.extend(params::field_tests(p.fields));
    q.worlds.extend(p.world.map(|w| w.to_string()));
    q.text.extend(p.text.map(|t| t.to_lowercase()));
    let levels: Vec<Value> = find::levels(project, &q)
        .map_err(|e| RpcError::invalid_params(format!("{method}: {e}")))?
        .into_iter()
        .map(|h| {
            let l = h.level;
            json!({ "slug": l.slug, "name": l.name, "id": l.id, "world": h.world.slug,
                "world_px": l.world_px, "size": l.size, "depth": l.depth,
                "tags": l.tags, "fields": l.fields })
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

/// `level.crop { level, world? }` → the `batch` of `resize_level`s that
/// shrinks the level to the bounding box of its content
/// (`Project::crop_level`); nothing when it already fits.
pub(crate) fn crop(project: &Project, method: &str, p: Value) -> Result<Write, RpcError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        level: Slug,
        world: Option<Slug>,
    }
    let p: P = params::parse(method, p)?;
    let (world, _) = find_level(project, p.level.as_str())?;
    if let Some(w) = p.world
        && w != world.slug
    {
        return Err(RpcError::command(&CommandError::LevelNotInWorld {
            world: w,
            level: p.level,
        }));
    }
    let cmd = project
        .crop_level(&world.slug, &p.level)
        .map_err(|e| RpcError::command(&e))?;
    Ok(Write::one(cmd.unwrap_or_else(Command::noop)))
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

/// `level.set_layer_state { level, layer, state }` → `set_layer_state`
/// (`state: null` removes the entry). The state is checked here, where the
/// command takes anything: its optional groups must be optional groups of
/// the layer's rules, its tilesets must exist.
pub(crate) fn set_layer_state(
    project: &Project,
    method: &str,
    p: Value,
) -> Result<Write, RpcError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        level: Slug,
        world: Option<Slug>,
        layer: Slug,
        #[serde(default, deserialize_with = "present")]
        state: Option<Option<LayerState>>,
    }
    let p: P = params::parse(method, p)?;
    let Some(state) = p.state else {
        return Err(RpcError::invalid_params(format!(
            "{method}: missing state; give the rule state, or null to remove it"
        )));
    };
    let (world, _) = find_level(project, p.level.as_str())?;
    check_world(&p.world, &world.slug, &p.level)?;
    params::layer_of_kind(project, p.layer.as_str(), "auto")?;
    if let Some(st) = &state {
        let groups = project
            .rules_for(p.layer.as_str())
            .map_or(&[][..], |r| &r.groups[..]);
        for g in &st.optional_groups {
            match groups.iter().find(|x| &x.slug == g) {
                None => {
                    return Err(RpcError::command(&CommandError::UnknownRuleGroup {
                        layer: p.layer.clone(),
                        group: g.clone(),
                    }));
                }
                Some(x) if x.mode != GroupMode::Optional => {
                    return Err(RpcError::command(&CommandError::Invalid(format!(
                        "rule group {g} of {} is not optional; only optional groups are switched on per level",
                        p.layer
                    ))));
                }
                Some(_) => {}
            }
        }
        for t in st.tilesets.iter().flat_map(|(a, b)| [a, b]) {
            if project.tileset(t.as_str()).is_none() {
                return Err(RpcError::command(&CommandError::UnknownTileset(t.clone())));
            }
        }
    }
    Ok(Write::one(Command::SetLayerState {
        level: p.level,
        layer: p.layer,
        state,
    }))
}

/// `level.replace { world?, level, content?, confirm? }` → `replace_level`:
/// the level with `level.slug`, metadata and content, wholesale. `level.id`
/// left out keeps the level's id. Needs `confirm` when the new content
/// leaves out something the level holds (a layer's content, an entity, a
/// note).
pub(crate) fn replace(project: &Project, method: &str, p: Value) -> Result<Write, RpcError> {
    let mut m = params::object(method, p)?;
    for k in m.keys() {
        if !["world", "level", "content"].contains(&k.as_str()) {
            return Err(RpcError::invalid_params(format!(
                "{method}: unknown key {k:?}; the keys are world, level, content"
            )));
        }
    }
    let mut level = m
        .remove("level")
        .ok_or_else(|| RpcError::invalid_params(format!("{method}: missing level")))?;
    let slug = level
        .get("slug")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            RpcError::invalid_params(format!("{method}: level.slug names the level to replace"))
        })?
        .to_string();
    let (world, old) = find_level(project, &slug)?;
    let given = m.get("world").and_then(Value::as_str).map(params::slug_of);
    check_world(&given, &world.slug, &old.slug)?;
    if let Value::Object(l) = &mut level
        && matches!(l.get("id"), None | Some(Value::Null))
    {
        l.insert("id".into(), json!(old.id));
    }
    let level: Level = params::parse(method, level)?;
    let content: LevelContent = match m.remove("content") {
        None | Some(Value::Null) => LevelContent::default(),
        Some(c) => params::parse(method, c)?,
    };
    let confirm = dropped(old, &content).map(|what| {
        Confirm::new(
            format!("drops {what}, which the new content leaves out"),
            format!(
                "replacing level {slug} drops {what}, which the new content leaves out. Send confirm: true to do it"
            ),
        )
    });
    Ok(Write::Commands {
        cmds: vec![Command::ReplaceLevel {
            world: world.slug.clone(),
            level,
            content,
            restore: false,
        }],
        id: None,
        confirm,
    })
}

/// What `level` holds that `content` leaves out, in words ("the collision
/// cells, 1 entity (doorcrev22)"), or `None` when nothing is dropped.
fn dropped(level: &Level, content: &LevelContent) -> Option<String> {
    let mut layers: Vec<String> = Vec::new();
    let grids = content.int_grids.iter().map(|g| &g.layer);
    let has_grid = |l: &Slug| grids.clone().any(|g| g == l);
    for g in level.int_grids.iter().filter(|g| !g.is_empty()) {
        if !has_grid(&g.layer) {
            layers.push(format!("the {} cells", g.layer));
        }
    }
    for o in level.overrides.iter().filter(|o| !o.is_empty()) {
        if !has_grid(&o.layer) {
            layers.push(format!("the {} overrides", o.layer));
        }
    }
    for t in level.tiles.iter().filter(|t| !t.tiles.is_empty()) {
        let kept = content
            .tiles
            .iter()
            .any(|n| n.layer == t.layer && !n.tiles.is_empty());
        if !kept {
            layers.push(format!("the {} tiles", t.layer));
        }
    }
    let ids = |list: Vec<&worlds_core::Id>| -> Option<(usize, String)> {
        let n = list.len();
        let mut s: Vec<String> = list.iter().take(5).map(|i| i.to_string()).collect();
        if n > 5 {
            s.push("...".into());
        }
        (n > 0).then(|| (n, s.join(", ")))
    };
    let entities = ids(level
        .all_entities()
        .filter(|e| {
            !content
                .entities
                .iter()
                .flat_map(|l| &l.entities)
                .any(|n| n.id == e.id)
        })
        .map(|e| &e.id)
        .collect());
    if let Some((n, s)) = entities {
        layers.push(format!(
            "{n} {} ({s})",
            if n == 1 { "entity" } else { "entities" }
        ));
    }
    let notes = ids(level
        .notes
        .iter()
        .filter(|e| !content.notes.iter().any(|n| n.id == e.id))
        .map(|e| &e.id)
        .collect());
    if let Some((n, s)) = notes {
        layers.push(format!(
            "{n} {} ({s})",
            if n == 1 { "note" } else { "notes" }
        ));
    }
    match layers.len() {
        0 => None,
        1 => layers.pop(),
        _ => {
            let last = layers.pop().unwrap_or_default();
            Some(format!("{} and {last}", layers.join(", ")))
        }
    }
}

/// `level.duplicate { level, slug, name?, world?, world_px? }` →
/// `duplicate_level`; the result adds the copy's `id` and `world_px`.
pub(crate) fn duplicate(project: &Project, method: &str, p: Value) -> Result<Write, RpcError> {
    let cmd = params::command(method, "duplicate_level", p)?;
    let Command::DuplicateLevel { slug, .. } = &cmd else {
        return Err(RpcError::internal("level.duplicate built another command"));
    };
    // The copy's id and position, worked out on a scratch copy (the ids
    // are derived, so applying it for real makes the same ones).
    let mut scratch = project.clone();
    scratch
        .apply(cmd.clone())
        .map_err(|e| RpcError::command(&e))?;
    let made = scratch
        .level(slug.as_str())
        .map(|l| json!({ "id": l.id, "world_px": l.world_px }));
    Ok(Write::Commands {
        cmds: vec![cmd],
        id: made,
        confirm: None,
    })
}
