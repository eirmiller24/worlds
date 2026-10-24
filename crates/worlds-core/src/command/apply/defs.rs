//! Definition commands: layers, entity types, enums, tilesets and rule
//! groups.
//!
//! A `set_*` creates or replaces one definition; its inverse removes it or
//! puts the old one back. A `remove_*` takes the definition away (with the
//! content that can't exist without it: a layer's per-level content, an
//! entity type's instances); its inverse puts everything back. A rename
//! rewrites every reference, so its inverse is the reverse rename. Because
//! of that, a rename is refused while something already refers to the new
//! slug (a dangling reference): undo couldn't tell it from a rewritten one.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use super::{Result, noop};
use crate::command::{
    Applied, Change, Command, CommandError, DefKind, LayerContent, LevelPart, ProjectKey, Rect,
    coalesce,
};
use crate::format::{IntGridFile, RESERVED_LAYER_SLUGS, RESERVED_RULES_SLUG};
use crate::id::{Id, Slug};
use crate::model::{
    DerivedOverrides, EntitiesLayer, EntityDef, EnumDef, EnumSource, FieldDef, FieldType,
    IntGridLayer, LayerDef, LayerKind, Level, Override, Project, RuleEntry, RuleGroup, RuleSet,
    TemplateOutput, TilesLayer, TilesetDef,
};

/// What [`list_set`] did.
pub(super) enum ListEdit<T> {
    Noop,
    Created,
    /// The old entry, and its old index if it moved.
    Replaced {
        old: T,
        moved_from: Option<usize>,
    },
}

/// Put `def` in an ordered list: replace the entry at `pos` (moving it to
/// `index` if given), or insert it at `index` (default: the end). Fails
/// before changing anything.
pub(super) fn list_set<T: PartialEq>(
    list: &mut Vec<T>,
    pos: Option<usize>,
    def: T,
    index: Option<usize>,
    what: &str,
) -> Result<ListEdit<T>> {
    let past_end = |i: usize, len: usize| {
        CommandError::Invalid(format!(
            "index {i} is past the end of {what} ({len} entries)"
        ))
    };
    match pos {
        None => {
            let i = index.unwrap_or(list.len());
            if i > list.len() {
                return Err(past_end(i, list.len()));
            }
            list.insert(i, def);
            Ok(ListEdit::Created)
        }
        Some(p) => {
            let j = index.unwrap_or(p);
            if j >= list.len() {
                return Err(past_end(j, list.len()));
            }
            if list[p] == def && p == j {
                return Ok(ListEdit::Noop);
            }
            let old = list.remove(p);
            list.insert(j, def);
            Ok(ListEdit::Replaced {
                old,
                moved_from: (p != j).then_some(p),
            })
        }
    }
}

pub(super) fn slug_mismatch(op: &str, given: &Slug, key: &Slug) -> CommandError {
    CommandError::Invalid(format!("{op}: def.slug {given:?} is not {key:?}"))
}

/// A slug already referred to by something, so a rename to it is refused.
pub(super) fn dangling(kind: DefKind, to: &Slug, what: &str) -> CommandError {
    CommandError::Invalid(format!(
        "{what} already refers to {kind} {to:?}, which doesn't exist; fix that reference \
         first (undo couldn't tell it from a renamed one)"
    ))
}

/// Refuse a layer slug that would collide with a fixed file name.
fn check_layer_slug(slug: &Slug, kind: &LayerKind) -> Result<()> {
    let has_file = matches!(kind, LayerKind::IntGrid(_) | LayerKind::Tiles(_));
    if has_file && RESERVED_LAYER_SLUGS.contains(&slug.as_str()) {
        return Err(CommandError::Invalid(format!(
            "an {} layer can't be named {slug:?}: its level files would clash with \
             level.json, entities.json or notes.json",
            kind.name()
        )));
    }
    if matches!(kind, LayerKind::Auto(_)) && slug == RESERVED_RULES_SLUG {
        return Err(CommandError::Invalid(format!(
            "an auto layer can't be named {slug:?}: its rules would be rules/macros.json"
        )));
    }
    Ok(())
}

/// Whether a field holds values of project enum `e`.
fn is_enum_field(f: &FieldDef, e: &Slug) -> bool {
    matches!(&f.ty, FieldType::Enum(EnumSource::Project(s)) if s == e)
}

/// Whether a stored enum value (a slug, or an array of them) holds `s`.
fn holds(v: &Value, s: &Slug) -> bool {
    match v {
        Value::String(x) => x == s.as_str(),
        Value::Array(a) => a.iter().any(|x| x.as_str() == Some(s.as_str())),
        _ => false,
    }
}

/// Rewrite `from` to `to` in a stored enum value. True if anything changed.
fn rename_in(v: &mut Value, from: &Slug, to: &Slug) -> bool {
    let mut hit = false;
    let mut one = |x: &mut Value| {
        if x.as_str() == Some(from.as_str()) {
            *x = Value::String(to.to_string());
            hit = true;
        }
    };
    match v {
        Value::Array(a) => a.iter_mut().for_each(&mut one),
        x => one(x),
    }
    hit
}

/// Where a tileset is referenced, for [`Project::visit_tilesets`].
enum Site {
    /// In a definition (a tiles layer's palette tileset, an icon, a `tile`
    /// field type or default, a rule output or stamp).
    Def(DefKind, Slug),
    /// A template in `rules/macros.json` (a fixed rule's own tileset).
    Template(Slug),
    /// A template reference's `tileset` in `rules/<layer>.json`.
    Reference { layer: Slug, reference: Slug },
    /// A `tile` level field's type or default in `project.json`.
    LevelFields,
    Placement {
        world: Slug,
        level: Slug,
        layer: Slug,
        cell: [u32; 2],
    },
    /// A tileset substitution in a level's rule state.
    State {
        world: Slug,
        level: Slug,
        layer: Slug,
    },
    /// A `tile` field value of an entity.
    Instance { world: Slug, level: Slug, id: Id },
    /// A `tile` level field value.
    LevelField { world: Slug, level: Slug },
}

impl Site {
    fn change(&self) -> Change {
        match self {
            Site::Def(kind, slug) => Change::Def {
                kind: *kind,
                slug: slug.clone(),
            },
            Site::Template(_) => Change::Def {
                kind: DefKind::Macros,
                slug: crate::id::slug("macros"),
            },
            Site::Reference { layer, .. } => Change::Def {
                kind: DefKind::Rules,
                slug: layer.clone(),
            },
            Site::LevelFields => Change::Project {
                key: ProjectKey::LevelFields,
            },
            Site::Placement {
                world,
                level,
                layer,
                cell,
            } => Change::Tiles {
                world: world.clone(),
                level: level.clone(),
                layer: layer.clone(),
                rect: Rect::cell(*cell),
            },
            Site::State {
                world,
                level,
                layer,
            } => Change::LayerState {
                world: world.clone(),
                level: level.clone(),
                layer: layer.clone(),
            },
            Site::Instance { world, level, id } => Change::Entity {
                world: world.clone(),
                level: level.clone(),
                id: id.clone(),
            },
            Site::LevelField { world, level } => Change::LevelMeta {
                world: world.clone(),
                level: level.clone(),
                part: LevelPart::Fields,
            },
        }
    }

    fn describe(&self) -> String {
        match self {
            Site::Def(kind, slug) => format!("{kind} {slug}"),
            Site::Template(name) => format!("template {name}"),
            Site::Reference { layer, reference } => {
                format!("rule set {layer} reference {reference}")
            }
            Site::LevelFields => "project level_fields".into(),
            Site::Placement { level, layer, .. } => {
                format!("tiles of layer {layer} in level {level}")
            }
            Site::State { level, layer, .. } => {
                format!("the {layer} rule state of level {level}")
            }
            Site::Instance { level, id, .. } => format!("entity {id} in level {level}"),
            Site::LevelField { level, .. } => format!("the fields of level {level}"),
        }
    }
}

/// Call `f` on the tileset slug of a `tile` field value (a tile ref, or an
/// array of them); a returned slug replaces it.
fn tile_value(v: &mut Value, site: &Site, f: &mut dyn FnMut(&Site, &Slug) -> Option<Slug>) {
    let mut one = |x: &mut Value| {
        if let Some(Value::String(s)) = x.get_mut("tileset")
            && let Ok(slug) = Slug::new(s.as_str())
            && let Some(n) = f(site, &slug)
        {
            *s = n.to_string();
        }
    };
    match v {
        Value::Array(a) => a.iter_mut().for_each(&mut one),
        x => one(x),
    }
}

/// Call `f` on every tileset reference in field definitions (type and default).
fn field_tilesets(
    fields: &mut [FieldDef],
    site: &Site,
    f: &mut dyn FnMut(&Site, &Slug) -> Option<Slug>,
) {
    for d in fields {
        if let FieldType::Tile { tileset } = &mut d.ty {
            if let Some(ts) = tileset
                && let Some(n) = f(site, ts)
            {
                *ts = n;
            }
            if let Some(v) = &mut d.default {
                tile_value(v, site, f);
            }
        }
    }
}

/// The slugs of `tile` fields.
fn tile_fields(fields: &[FieldDef]) -> Vec<Slug> {
    fields
        .iter()
        .filter(|f| matches!(f.ty, FieldType::Tile { .. }))
        .map(|f| f.slug.clone())
        .collect()
}

/// Rewrite `from` to `to` in `entity_ref` `allowed_types`. True if any changed.
fn rename_allowed_types(fields: &mut [FieldDef], from: &Slug, to: &Slug) -> bool {
    let mut hit = false;
    for f in fields {
        if let FieldType::EntityRef { allowed_types, .. } = &mut f.ty {
            for t in allowed_types.iter_mut().filter(|t| *t == from) {
                t.clone_from(to);
                hit = true;
            }
        }
    }
    hit
}

/// Rewrite `enum` field types naming `from` to `to`. True if any changed.
fn rename_enum_type(fields: &mut [FieldDef], from: &Slug, to: &Slug) -> bool {
    let mut hit = false;
    for f in fields {
        if let FieldType::Enum(EnumSource::Project(e)) = &mut f.ty
            && e == from
        {
            e.clone_from(to);
            hit = true;
        }
    }
    hit
}

/// Sort a level's per-layer lists by layer slug (after a layer rename).
fn sort_layers(l: &mut Level) {
    l.int_grids.sort_by(|a, b| a.layer.cmp(&b.layer));
    l.overrides.sort_by(|a, b| a.layer.cmp(&b.layer));
    l.tiles.sort_by(|a, b| a.layer.cmp(&b.layer));
    l.entities.sort_by(|a, b| a.layer.cmp(&b.layer));
}

impl Project {
    // ---- layers ----

    pub(super) fn set_layer_def(
        &mut self,
        layer: Slug,
        def: LayerDef,
        index: Option<usize>,
        content: Vec<LayerContent>,
        rules: Option<RuleSet>,
    ) -> Result<Applied> {
        if def.slug != layer {
            return Err(slug_mismatch("set_layer_def", &def.slug, &layer));
        }
        check_layer_slug(&layer, &def.kind)?;
        match self.layers.iter().position(|l| l.slug == layer) {
            None => self.create_layer_def(def, index, content, rules),
            Some(i) => {
                if !content.is_empty() || rules.is_some() {
                    return Err(CommandError::Invalid(format!(
                        "set_layer_def: `content` and `rules` only come with a new layer, and \
                         layer {layer:?} exists"
                    )));
                }
                self.replace_layer_def(i, def, index)
            }
        }
    }

    /// The changes of creating or removing layer `slug` as it is now: the
    /// definition, its rule set, then every level's content, entities and
    /// rule state for it.
    fn layer_def_changes(&self, slug: &Slug) -> Vec<Change> {
        let mut out = vec![Change::Def {
            kind: DefKind::Layer,
            slug: slug.clone(),
        }];
        if self.rules_for(slug.as_str()).is_some() {
            out.push(Change::Def {
                kind: DefKind::Rules,
                slug: slug.clone(),
            });
        }
        let def = self.layer(slug.as_str());
        for w in &self.worlds {
            for l in &w.levels {
                let (world, level, layer) = (w.slug.clone(), l.slug.clone(), slug.clone());
                let grid = l
                    .int_grid(slug.as_str())
                    .map(|g| g.size())
                    .or(l.overrides(slug.as_str()).map(|o| o.size()));
                if let Some(size) = grid {
                    out.push(Change::Grid {
                        world: world.clone(),
                        level: level.clone(),
                        layer: layer.clone(),
                        rect: Rect::of_size(size),
                    });
                }
                if l.tiles_layer(slug.as_str()).is_some() {
                    let size = def.map_or([0, 0], |d| self.layer_cells(d, l));
                    out.push(Change::Tiles {
                        world: world.clone(),
                        level: level.clone(),
                        layer: layer.clone(),
                        rect: Rect::of_size(size),
                    });
                }
                if let Some(e) = l.entities_layer(slug.as_str()) {
                    out.push(Change::EntityLayer {
                        world: world.clone(),
                        level: level.clone(),
                        layer: layer.clone(),
                    });
                    out.extend(e.entities.iter().map(|e| Change::Entity {
                        world: world.clone(),
                        level: level.clone(),
                        id: e.id.clone(),
                    }));
                }
                if l.layers.contains_key(slug) {
                    out.push(Change::LayerState {
                        world,
                        level,
                        layer,
                    });
                }
            }
        }
        out
    }

    /// Something in a level that names layer `slug` (content, an entities
    /// entry, rule state), or its rule set: what `remove_layer_def` takes.
    fn layer_leftover(&self, slug: &Slug) -> Option<String> {
        if self.rules_for(slug.as_str()).is_some() {
            return Some(format!("rules/{slug}.json"));
        }
        self.levels()
            .find(|l| {
                l.int_grid(slug.as_str()).is_some()
                    || l.overrides(slug.as_str()).is_some()
                    || l.tiles_layer(slug.as_str()).is_some()
                    || l.entities_layer(slug.as_str()).is_some()
                    || l.layers.contains_key(slug)
            })
            .map(|l| format!("level {}", l.slug))
    }

    fn create_layer_def(
        &mut self,
        def: LayerDef,
        index: Option<usize>,
        content: Vec<LayerContent>,
        rules: Option<RuleSet>,
    ) -> Result<Applied> {
        let slug = def.slug.clone();
        if let Some(what) = self.layer_leftover(&slug) {
            return Err(CommandError::Invalid(format!(
                "{what} still holds content of an old layer {slug:?}; remove it (or restore the \
                 layer with that content as `content`) first"
            )));
        }
        if let Some(r) = &rules
            && r.layer != slug
        {
            return Err(CommandError::Invalid(format!(
                "set_layer_def: rules.layer {:?} is not {slug:?}",
                r.layer
            )));
        }
        // Check and build all content before changing anything.
        type Built = (
            (usize, usize),
            Option<std::result::Result<IntGridLayer, DerivedOverrides>>,
            Option<TilesLayer>,
            Option<EntitiesLayer>,
            Option<crate::model::LayerState>,
        );
        let mut built: Vec<Built> = Vec::new();
        let mut ids: BTreeSet<Id> = self.entity_ids().into_iter().cloned().collect();
        let mut levels = BTreeSet::new();
        for c in content {
            if c.is_empty() {
                return Err(CommandError::Invalid(format!(
                    "content for level {} is empty",
                    c.level
                )));
            }
            let at = self.find_level(&c.level)?;
            if !levels.insert(at) {
                return Err(CommandError::Invalid(format!(
                    "content for level {} given twice",
                    c.level
                )));
            }
            let wrong = |expected| CommandError::WrongLayerKind {
                layer: slug.clone(),
                expected,
                actual: def.kind.name(),
            };
            let grid = match c.rows {
                Some(_) if !def.is_int_grid() => return Err(wrong("int_grid")),
                Some(rows) => {
                    let file = IntGridFile {
                        layer: slug.clone(),
                        overrides: None,
                        legend: None,
                        rows,
                    };
                    let bad = |(path, msg): (String, String)| {
                        CommandError::Invalid(format!(
                            "content for level {}: {path}: {msg}",
                            c.level
                        ))
                    };
                    Some(if def.is_derived() {
                        Err(file.to_overrides(&def, false).map_err(bad)?)
                    } else {
                        Ok(file.to_layer(&def).map_err(bad)?)
                    })
                }
                None => None,
            };
            let tiles = match c.tiles {
                Some(_) if !def.is_tiles() => return Err(wrong("tiles")),
                Some(tiles) => {
                    let mut t = TilesLayer {
                        layer: slug.clone(),
                        tiles,
                    };
                    t.sort();
                    Some(t)
                }
                None => None,
            };
            let entities = match c.entities {
                Some(entities) => {
                    for e in &entities {
                        if !ids.insert(e.id.clone()) {
                            return Err(CommandError::DuplicateId(e.id.clone()));
                        }
                    }
                    Some(EntitiesLayer {
                        layer: slug.clone(),
                        entities,
                    })
                }
                None => None,
            };
            built.push((at, grid, tiles, entities, c.state));
        }
        list_set(&mut self.layers, None, def, index, "layers.json")?;
        if let Some(r) = rules {
            let k = self.rules.partition_point(|x| x.layer < r.layer);
            self.rules.insert(k, r);
        }
        for (at, grid, tiles, entities, state) in built {
            let l = self.level_at_mut(at);
            match grid {
                Some(Ok(g)) => {
                    let i = l.int_grids.partition_point(|x| x.layer < slug);
                    l.int_grids.insert(i, g);
                }
                Some(Err(o)) => {
                    let i = l.overrides.partition_point(|x| x.layer < slug);
                    l.overrides.insert(i, o);
                }
                None => {}
            }
            if let Some(t) = tiles {
                let i = l.tiles.partition_point(|x| x.layer < slug);
                l.tiles.insert(i, t);
            }
            if let Some(e) = entities {
                let i = l.entities.partition_point(|x| x.layer < slug);
                l.entities.insert(i, e);
            }
            if let Some(s) = state {
                l.layers.insert(slug.clone(), s);
            }
        }
        Ok(Applied {
            changes: self.layer_def_changes(&slug),
            inverse: Command::RemoveLayerDef { layer: slug },
        })
    }

    fn replace_layer_def(
        &mut self,
        i: usize,
        def: LayerDef,
        index: Option<usize>,
    ) -> Result<Applied> {
        let old = &self.layers[i];
        let slug = def.slug.clone();
        let s = slug.as_str();
        let has_content = |l: &Level| {
            l.int_grid(s).is_some()
                || l.overrides(s).is_some()
                || l.tiles_layer(s).is_some()
                || l.entities_layer(s).is_some()
        };
        // A derived layer's files hold overrides, any other int_grid layer's
        // hold cells: flipping `derived` would change what they mean.
        if old.is_int_grid()
            && def.is_int_grid()
            && old.is_derived() != def.is_derived()
            && let Some(l) = self.levels().find(|l| has_content(l))
        {
            return Err(CommandError::Invalid(if old.is_derived() {
                format!("{slug} has overrides in {}; clear them first", l.slug)
            } else {
                format!(
                    "{slug} has content in {}; clear it first (a derived layer's files hold \
                     overrides, not cells)",
                    l.slug
                )
            }));
        }
        if std::mem::discriminant(&old.kind) != std::mem::discriminant(&def.kind)
            && let Some(l) = self
                .levels()
                .find(|l| has_content(l) || l.layers.contains_key(&slug))
        {
            return Err(CommandError::Invalid(format!(
                "layer {slug:?} has content in level {}, so it can't change from {} to {}; \
                 remove the layer and create it again instead",
                l.slug,
                old.kind.name(),
                def.kind.name()
            )));
        }
        if self.layer_grid(old) != self.layer_grid(&def)
            && let Some(l) = self.levels().find(|l| has_content(l))
        {
            return Err(CommandError::Invalid(format!(
                "changing the grid of layer {slug:?} from {} to {} px would misplace its content \
                 in level {}",
                self.layer_grid(old),
                self.layer_grid(&def),
                l.slug
            )));
        }
        // Stored cells are indices into the values: keep each cell's value
        // by slug.
        let mut regrids = Vec::new();
        let mut replanes = Vec::new();
        if old.values() != def.values() {
            let map: Vec<Option<u16>> = old
                .values()
                .iter()
                .map(|v| def.value_index(v.slug.as_str()))
                .collect();
            for (wi, w) in self.worlds.iter().enumerate() {
                for (li, l) in w.levels.iter().enumerate() {
                    if let Some(o) = l.overrides(s) {
                        let mut cells: Vec<Override> = o.cells().collect();
                        for (k, c) in cells.iter_mut().enumerate() {
                            let Override::Value(v) = *c else { continue };
                            let (x, y) = (k as u32 % o.size()[0], k as u32 / o.size()[0]);
                            match map.get(v as usize - 1) {
                                Some(Some(n)) => *c = Override::Value(*n),
                                Some(None) => {
                                    return Err(CommandError::Invalid(format!(
                                        "value {:?} of layer {slug:?} is still used by an \
                                         override in level {} (cell [{x}, {y}]); replace it \
                                         (replace_value) before taking it out of the definition",
                                        old.values()[v as usize - 1].slug,
                                        l.slug
                                    )));
                                }
                                None => {
                                    return Err(CommandError::UndefinedIndex {
                                        layer: slug.clone(),
                                        x,
                                        y,
                                        index: v,
                                    });
                                }
                            }
                        }
                        replanes.push(((wi, li), o.size(), cells));
                    }
                    let Some(g) = l.int_grid(s) else { continue };
                    let mut cells = g.cells().to_vec();
                    for (k, c) in cells.iter_mut().enumerate() {
                        if *c == 0 {
                            continue;
                        }
                        let (x, y) = (k as u32 % g.size()[0], k as u32 / g.size()[0]);
                        match map.get(*c as usize - 1) {
                            Some(Some(n)) => *c = *n,
                            Some(None) => {
                                return Err(CommandError::Invalid(format!(
                                    "value {:?} of layer {slug:?} is still used in level {} \
                                     (cell [{x}, {y}]); replace it (replace_value) before taking \
                                     it out of the definition",
                                    old.values()[*c as usize - 1].slug,
                                    l.slug
                                )));
                            }
                            None => {
                                return Err(CommandError::UndefinedIndex {
                                    layer: slug.clone(),
                                    x,
                                    y,
                                    index: *c,
                                });
                            }
                        }
                    }
                    regrids.push(((wi, li), g.size(), cells));
                }
            }
        }
        let edit = list_set(&mut self.layers, Some(i), def, index, "layers.json")?;
        let (old, moved_from) = match edit {
            ListEdit::Noop => return Ok(noop()),
            ListEdit::Created => unreachable!("replacing an existing layer"),
            ListEdit::Replaced { old, moved_from } => (old, moved_from),
        };
        let mut changes = vec![Change::Def {
            kind: DefKind::Layer,
            slug: slug.clone(),
        }];
        for (at, size, cells) in regrids {
            let world = self.worlds[at.0].slug.clone();
            let l = self.level_at_mut(at);
            let g = l.int_grids.iter_mut().find(|g| g.layer == slug).unwrap();
            *g = IntGridLayer::from_cells(slug.clone(), size, cells);
            changes.push(Change::Grid {
                world,
                level: l.slug.clone(),
                layer: slug.clone(),
                rect: Rect::of_size(size),
            });
        }
        for (at, size, cells) in replanes {
            let world = self.worlds[at.0].slug.clone();
            let l = self.level_at_mut(at);
            let o = l.overrides.iter_mut().find(|o| o.layer == slug).unwrap();
            *o = DerivedOverrides::from_cells(slug.clone(), size, &cells);
            changes.push(Change::Grid {
                world,
                level: l.slug.clone(),
                layer: slug.clone(),
                rect: Rect::of_size(size),
            });
        }
        Ok(Applied {
            inverse: Command::SetLayerDef {
                layer: slug,
                def: old,
                index: moved_from,
                content: Vec::new(),
                rules: None,
            },
            changes,
        })
    }

    pub(super) fn remove_layer_def(&mut self, layer: Slug) -> Result<Applied> {
        let i = self
            .layers
            .iter()
            .position(|l| l.slug == layer)
            .ok_or_else(|| CommandError::UnknownLayer(layer.clone()))?;
        let def = self.layers[i].clone();
        let s = layer.as_str();
        let mut content = Vec::new();
        for l in self.levels() {
            let rows = match (l.int_grid(s), l.overrides(s)) {
                (Some(g), _) => Some(g.to_rows(&def).map_err(CommandError::Invalid)?),
                (None, Some(o)) => Some(o.to_rows(&def).map_err(CommandError::Invalid)?),
                (None, None) => None,
            };
            let c = LayerContent {
                level: l.slug.clone(),
                rows,
                tiles: l.tiles_layer(s).map(|t| t.tiles.clone()),
                entities: l.entities_layer(s).map(|e| e.entities.clone()),
                state: l.layers.get(&layer).cloned(),
            };
            if !c.is_empty() {
                content.push(c);
            }
        }
        let changes = self.layer_def_changes(&layer);
        self.layers.remove(i);
        let rules = self
            .rules
            .iter()
            .position(|r| r.layer == layer)
            .map(|k| self.rules.remove(k));
        for w in &mut self.worlds {
            for l in &mut w.levels {
                l.int_grids.retain(|g| g.layer != layer);
                l.overrides.retain(|o| o.layer != layer);
                l.tiles.retain(|t| t.layer != layer);
                l.entities.retain(|e| e.layer != layer);
                l.layers.remove(&layer);
            }
        }
        Ok(Applied {
            inverse: Command::SetLayerDef {
                layer,
                def,
                index: Some(i),
                content,
                rules,
            },
            changes,
        })
    }

    /// Something other than the layer itself that names layer `slug`.
    fn layer_ref(&self, slug: &Slug) -> Option<String> {
        if self.collision_layer.as_ref() == Some(slug) {
            return Some("project collision_layer".into());
        }
        if let Some(d) = self
            .layers
            .iter()
            .find(|d| d.auto().is_some_and(|a| a.source == *slug))
        {
            return Some(format!("the source of layer {}", d.slug));
        }
        for set in &self.rules {
            if set.layer == *slug {
                return Some(format!("rules/{slug}.json"));
            }
            for g in &set.groups {
                if let Some(r) = g.inline_rules().find(|r| {
                    r.patterns().any(|p| p.grid.as_ref() == Some(slug))
                        || r.as_region()
                            .is_some_and(|g| g.region.grid.as_ref() == Some(slug))
                        || r.write().is_some_and(|w| w.grid == *slug)
                }) {
                    return Some(format!("rule {} in rules/{}.json", r.slug, set.layer));
                }
            }
        }
        if let Some(m) = &self.macros {
            for (name, t) in &m.templates {
                if t.rules
                    .iter()
                    .any(|r| r.patterns().any(|p| p.grid.as_ref() == Some(slug)))
                {
                    return Some(format!("template {name} in rules/macros.json"));
                }
            }
        }
        self.layer_leftover(slug)
    }

    pub(super) fn rename_layer(&mut self, from: Slug, to: Slug) -> Result<Applied> {
        let i = self
            .layers
            .iter()
            .position(|l| l.slug == from)
            .ok_or_else(|| CommandError::UnknownLayer(from.clone()))?;
        if from == to {
            return Ok(noop());
        }
        if self.layer(to.as_str()).is_some() {
            return Err(CommandError::DuplicateDef {
                kind: DefKind::Layer,
                slug: to,
            });
        }
        check_layer_slug(&to, &self.layers[i].kind)?;
        if let Some(what) = self.layer_ref(&to) {
            return Err(dangling(DefKind::Layer, &to, &what));
        }
        let mut changes = vec![Change::DefRenamed {
            kind: DefKind::Layer,
            from: from.clone(),
            to: to.clone(),
        }];
        self.layers[i].slug = to.clone();
        for d in &mut self.layers {
            if d.auto().is_some_and(|a| a.source == from) {
                d.auto_mut().expect("auto layer").source = to.clone();
                changes.push(Change::Def {
                    kind: DefKind::Layer,
                    slug: d.slug.clone(),
                });
            }
        }
        if let Some(set) = self.rules.iter_mut().find(|r| r.layer == from) {
            set.layer = to.clone();
            changes.push(Change::DefRenamed {
                kind: DefKind::Rules,
                from: from.clone(),
                to: to.clone(),
            });
            self.rules.sort_by(|a, b| a.layer.cmp(&b.layer));
        }
        let regrid = |p: &mut crate::model::Pattern| {
            let hit = p.grid.as_ref() == Some(&from);
            if hit {
                p.grid = Some(to.clone());
            }
            hit
        };
        for set in &mut self.rules {
            let mut hit = false;
            for g in &mut set.groups {
                for r in g.inline_rules_mut() {
                    for p in r.patterns_mut() {
                        hit |= regrid(p);
                    }
                    if let Some(g) = r.as_region_mut()
                        && g.region.grid.as_ref() == Some(&from)
                    {
                        g.region.grid = Some(to.clone());
                        hit = true;
                    }
                    if let Some(w) = r.write_mut()
                        && w.grid == from
                    {
                        w.grid = to.clone();
                        hit = true;
                    }
                }
            }
            // The renamed set's own file is already reported.
            if hit && set.layer != to {
                changes.push(Change::Def {
                    kind: DefKind::Rules,
                    slug: set.layer.clone(),
                });
            }
        }
        if let Some(m) = &mut self.macros {
            let mut hit = false;
            for r in m.templates.values_mut().flat_map(|t| &mut t.rules) {
                for p in r.patterns_mut() {
                    hit |= regrid(p);
                }
            }
            if hit {
                changes.push(Change::Def {
                    kind: DefKind::Macros,
                    slug: crate::id::slug("macros"),
                });
            }
        }
        if self.collision_layer.as_ref() == Some(&from) {
            self.collision_layer = Some(to.clone());
            changes.push(Change::Project {
                key: ProjectKey::CollisionLayer,
            });
        }
        for w in &mut self.worlds {
            for l in &mut w.levels {
                let (world, level) = (w.slug.clone(), l.slug.clone());
                let file = |changes: &mut Vec<Change>| {
                    changes.push(Change::LayerFileRenamed {
                        world: world.clone(),
                        level: level.clone(),
                        from: from.clone(),
                        to: to.clone(),
                    })
                };
                if let Some(g) = l.int_grids.iter_mut().find(|g| g.layer == from) {
                    g.layer = to.clone();
                    file(&mut changes);
                }
                if let Some(o) = l.overrides.iter_mut().find(|o| o.layer == from) {
                    o.layer = to.clone();
                    file(&mut changes);
                }
                if let Some(t) = l.tiles.iter_mut().find(|t| t.layer == from) {
                    t.layer = to.clone();
                    file(&mut changes);
                }
                if let Some(e) = l.entities.iter_mut().find(|e| e.layer == from) {
                    e.layer = to.clone();
                    changes.push(Change::EntityLayer {
                        world: world.clone(),
                        level: level.clone(),
                        layer: to.clone(),
                    });
                }
                if let Some(st) = l.layers.remove(&from) {
                    l.layers.insert(to.clone(), st);
                    changes.push(Change::LayerState {
                        world: world.clone(),
                        level: level.clone(),
                        layer: to.clone(),
                    });
                }
                sort_layers(l);
            }
        }
        Ok(Applied {
            inverse: Command::RenameLayer { from: to, to: from },
            changes,
        })
    }

    // ---- entity types ----

    pub(super) fn set_entity_def(&mut self, entity: Slug, def: EntityDef) -> Result<Applied> {
        if def.slug != entity {
            return Err(slug_mismatch("set_entity_def", &def.slug, &entity));
        }
        let changes = vec![Change::Def {
            kind: DefKind::Entity,
            slug: entity.clone(),
        }];
        let inverse = match self.entities.iter().position(|e| e.slug == entity) {
            Some(k) if self.entities[k] == def => return Ok(noop()),
            Some(k) => Command::SetEntityDef {
                entity,
                def: std::mem::replace(&mut self.entities[k], def),
            },
            None => {
                // Instances that already name the type (dangling until now)
                // stay when the creation is undone.
                let keep_instances = self
                    .levels()
                    .flat_map(|l| l.all_entities())
                    .any(|e| e.ty == entity);
                let k = self.entities.partition_point(|e| e.slug < entity);
                self.entities.insert(k, def);
                Command::RemoveEntityDef {
                    entity,
                    keep_instances,
                }
            }
        };
        Ok(Applied { inverse, changes })
    }

    pub(super) fn remove_entity_def(
        &mut self,
        entity: Slug,
        keep_instances: bool,
    ) -> Result<Applied> {
        let k = self
            .entities
            .iter()
            .position(|e| e.slug == entity)
            .ok_or_else(|| CommandError::UnknownEntityType(entity.clone()))?;
        let ids: Vec<Id> = self
            .levels()
            .flat_map(|l| l.all_entities())
            .filter(|e| e.ty == entity)
            .map(|e| e.id.clone())
            .collect();
        if !ids.is_empty() && !keep_instances {
            // Remove the instances one by one, then the (unused) type: the
            // inverse is a batch that puts the type back, then each instance
            // at its old index.
            let mut commands: Vec<Command> = ids
                .into_iter()
                .map(|id| Command::RemoveEntity {
                    id,
                    keep_empty: false,
                })
                .collect();
            let label = format!("Remove entity type {entity}");
            commands.push(Command::RemoveEntityDef {
                entity,
                keep_instances: false,
            });
            return self.batch(label, commands);
        }
        let def = self.entities.remove(k);
        Ok(Applied {
            inverse: Command::SetEntityDef {
                entity: entity.clone(),
                def,
            },
            changes: vec![Change::Def {
                kind: DefKind::Entity,
                slug: entity,
            }],
        })
    }

    pub(super) fn rename_entity_type(&mut self, from: Slug, to: Slug) -> Result<Applied> {
        let k = self
            .entities
            .iter()
            .position(|e| e.slug == from)
            .ok_or_else(|| CommandError::UnknownEntityType(from.clone()))?;
        if from == to {
            return Ok(noop());
        }
        if self.entity_def(to.as_str()).is_some() {
            return Err(CommandError::DuplicateDef {
                kind: DefKind::Entity,
                slug: to,
            });
        }
        let allows = |fields: &[FieldDef]| {
            fields.iter().any(|f| {
                matches!(&f.ty, FieldType::EntityRef { allowed_types, .. } if allowed_types.contains(&to))
            })
        };
        if let Some(e) = self
            .levels()
            .flat_map(|l| l.all_entities())
            .find(|e| e.ty == to)
        {
            return Err(dangling(DefKind::Entity, &to, &format!("entity {}", e.id)));
        }
        if let Some(d) = self.entities.iter().find(|d| allows(&d.fields)) {
            return Err(dangling(
                DefKind::Entity,
                &to,
                &format!("a field of entity type {}", d.slug),
            ));
        }
        if allows(&self.level_fields) {
            return Err(dangling(DefKind::Entity, &to, "a level field"));
        }
        let mut changes = vec![Change::DefRenamed {
            kind: DefKind::Entity,
            from: from.clone(),
            to: to.clone(),
        }];
        self.entities[k].slug = to.clone();
        self.entities.sort_by(|a, b| a.slug.cmp(&b.slug));
        for d in &mut self.entities {
            // The renamed type's own file is already reported.
            if rename_allowed_types(&mut d.fields, &from, &to) && d.slug != to {
                changes.push(Change::Def {
                    kind: DefKind::Entity,
                    slug: d.slug.clone(),
                });
            }
        }
        if rename_allowed_types(&mut self.level_fields, &from, &to) {
            changes.push(Change::Project {
                key: ProjectKey::LevelFields,
            });
        }
        for w in &mut self.worlds {
            for l in &mut w.levels {
                for e in l.entities.iter_mut().flat_map(|x| &mut x.entities) {
                    if e.ty == from {
                        e.ty = to.clone();
                        changes.push(Change::Entity {
                            world: w.slug.clone(),
                            level: l.slug.clone(),
                            id: e.id.clone(),
                        });
                    }
                }
            }
        }
        Ok(Applied {
            inverse: Command::RenameEntityType { from: to, to: from },
            changes,
        })
    }

    // ---- enums ----

    pub(super) fn set_enum_def(
        &mut self,
        enum_: Slug,
        def: EnumDef,
        index: Option<usize>,
    ) -> Result<Applied> {
        if def.slug != enum_ {
            return Err(slug_mismatch("set_enum_def", &def.slug, &enum_));
        }
        let pos = self.enums.iter().position(|e| e.slug == enum_);
        let inverse = match list_set(&mut self.enums, pos, def, index, "enums.json")? {
            ListEdit::Noop => return Ok(noop()),
            ListEdit::Created => Command::RemoveEnumDef {
                enum_: enum_.clone(),
            },
            ListEdit::Replaced { old, moved_from } => Command::SetEnumDef {
                enum_: enum_.clone(),
                def: old,
                index: moved_from,
            },
        };
        Ok(Applied {
            inverse,
            changes: vec![Change::Def {
                kind: DefKind::Enum,
                slug: enum_,
            }],
        })
    }

    pub(super) fn remove_enum_def(&mut self, enum_: Slug) -> Result<Applied> {
        let i = self
            .enums
            .iter()
            .position(|e| e.slug == enum_)
            .ok_or_else(|| CommandError::UnknownEnum(enum_.clone()))?;
        let def = self.enums.remove(i);
        Ok(Applied {
            inverse: Command::SetEnumDef {
                enum_: enum_.clone(),
                def,
                index: Some(i),
            },
            changes: vec![Change::Def {
                kind: DefKind::Enum,
                slug: enum_,
            }],
        })
    }

    pub(super) fn rename_enum(&mut self, from: Slug, to: Slug) -> Result<Applied> {
        let i = self
            .enums
            .iter()
            .position(|e| e.slug == from)
            .ok_or_else(|| CommandError::UnknownEnum(from.clone()))?;
        if from == to {
            return Ok(noop());
        }
        if self.enum_def(to.as_str()).is_some() {
            return Err(CommandError::DuplicateDef {
                kind: DefKind::Enum,
                slug: to,
            });
        }
        let uses = |fields: &[FieldDef]| fields.iter().any(|f| is_enum_field(f, &to));
        if let Some(d) = self.entities.iter().find(|d| uses(&d.fields)) {
            return Err(dangling(
                DefKind::Enum,
                &to,
                &format!("a field of entity type {}", d.slug),
            ));
        }
        if uses(&self.level_fields) {
            return Err(dangling(DefKind::Enum, &to, "a level field"));
        }
        self.enums[i].slug = to.clone();
        let mut changes = vec![Change::DefRenamed {
            kind: DefKind::Enum,
            from: from.clone(),
            to: to.clone(),
        }];
        for d in &mut self.entities {
            if rename_enum_type(&mut d.fields, &from, &to) {
                changes.push(Change::Def {
                    kind: DefKind::Entity,
                    slug: d.slug.clone(),
                });
            }
        }
        if rename_enum_type(&mut self.level_fields, &from, &to) {
            changes.push(Change::Project {
                key: ProjectKey::LevelFields,
            });
        }
        Ok(Applied {
            inverse: Command::RenameEnum { from: to, to: from },
            changes,
        })
    }

    pub(super) fn rename_enum_value(
        &mut self,
        enum_: Slug,
        from: Slug,
        to: Slug,
    ) -> Result<Applied> {
        let i = self
            .enums
            .iter()
            .position(|e| e.slug == enum_)
            .ok_or_else(|| CommandError::UnknownEnum(enum_.clone()))?;
        let vi = self.enums[i]
            .values
            .iter()
            .position(|v| v.slug == from)
            .ok_or_else(|| CommandError::UnknownEnumValue {
                enum_: enum_.clone(),
                value: from.clone(),
            })?;
        if from == to {
            return Ok(noop());
        }
        if self.enums[i].has_value(to.as_str()) {
            return Err(CommandError::Invalid(format!(
                "enum {enum_:?} already has a value {to:?}"
            )));
        }
        // The fields that hold this enum's values.
        let fields_of = |fields: &[FieldDef]| -> Vec<Slug> {
            fields
                .iter()
                .filter(|f| is_enum_field(f, &enum_))
                .map(|f| f.slug.clone())
                .collect()
        };
        let entity_fields: BTreeMap<Slug, Vec<Slug>> = self
            .entities
            .iter()
            .map(|d| (d.slug.clone(), fields_of(&d.fields)))
            .filter(|(_, f)| !f.is_empty())
            .collect();
        let level_fields = fields_of(&self.level_fields);
        let default_holds = |fields: &[FieldDef], s: &Slug| {
            fields.iter().any(|f| {
                is_enum_field(f, &enum_) && f.default.as_ref().is_some_and(|v| holds(v, s))
            })
        };
        // Refuse while anything already holds `to` (see the module docs).
        let clash = || -> Option<String> {
            if let Some(d) = self.entities.iter().find(|d| default_holds(&d.fields, &to)) {
                return Some(format!("a field default of entity type {}", d.slug));
            }
            if default_holds(&self.level_fields, &to) {
                return Some("a level field default".into());
            }
            for l in self.levels() {
                if level_fields
                    .iter()
                    .any(|f| l.fields.get(f).is_some_and(|v| holds(v, &to)))
                {
                    return Some(format!("a field of level {}", l.slug));
                }
                for e in l.all_entities() {
                    let fs = entity_fields.get(&e.ty).map_or(&[][..], |f| f.as_slice());
                    if fs
                        .iter()
                        .any(|f| e.fields.get(f).is_some_and(|v| holds(v, &to)))
                    {
                        return Some(format!("entity {}", e.id));
                    }
                }
            }
            for set in &self.rules {
                for g in &set.groups {
                    if let Some(req) = &g.requires_level_field
                        && level_fields.contains(&req.field)
                        && req.values.contains(&to)
                    {
                        return Some(format!("rule group {} in rules/{}.json", g.slug, set.layer));
                    }
                }
            }
            None
        };
        if let Some(what) = clash() {
            return Err(CommandError::Invalid(format!(
                "{what} already holds {to:?}, which isn't a value of enum {enum_:?}; fix that \
                 first (undo couldn't tell it from a renamed value)"
            )));
        }
        self.enums[i].values[vi].slug = to.clone();
        let mut changes = vec![Change::Def {
            kind: DefKind::Enum,
            slug: enum_.clone(),
        }];
        let rename_defaults = |fields: &mut [FieldDef]| {
            let mut hit = false;
            for f in fields.iter_mut().filter(|f| is_enum_field(f, &enum_)) {
                if let Some(v) = &mut f.default {
                    hit |= rename_in(v, &from, &to);
                }
            }
            hit
        };
        for d in &mut self.entities {
            if rename_defaults(&mut d.fields) {
                changes.push(Change::Def {
                    kind: DefKind::Entity,
                    slug: d.slug.clone(),
                });
            }
        }
        if rename_defaults(&mut self.level_fields) {
            changes.push(Change::Project {
                key: ProjectKey::LevelFields,
            });
        }
        for w in &mut self.worlds {
            for l in &mut w.levels {
                let mut hit = false;
                for f in &level_fields {
                    if let Some(v) = l.fields.get_mut(f) {
                        hit |= rename_in(v, &from, &to);
                    }
                }
                if hit {
                    changes.push(Change::LevelMeta {
                        world: w.slug.clone(),
                        level: l.slug.clone(),
                        part: LevelPart::Fields,
                    });
                }
                for e in l.entities.iter_mut().flat_map(|x| &mut x.entities) {
                    let Some(fs) = entity_fields.get(&e.ty) else {
                        continue;
                    };
                    let mut hit = false;
                    for f in fs {
                        if let Some(v) = e.fields.get_mut(f) {
                            hit |= rename_in(v, &from, &to);
                        }
                    }
                    if hit {
                        changes.push(Change::Entity {
                            world: w.slug.clone(),
                            level: l.slug.clone(),
                            id: e.id.clone(),
                        });
                    }
                }
            }
        }
        for set in &mut self.rules {
            let mut hit = false;
            for g in &mut set.groups {
                if let Some(req) = &mut g.requires_level_field
                    && level_fields.contains(&req.field)
                {
                    for v in req.values.iter_mut().filter(|v| **v == from) {
                        v.clone_from(&to);
                        hit = true;
                    }
                }
            }
            if hit {
                changes.push(Change::Def {
                    kind: DefKind::Rules,
                    slug: set.layer.clone(),
                });
            }
        }
        Ok(Applied {
            inverse: Command::RenameEnumValue {
                enum_,
                from: to,
                to: from,
            },
            changes,
        })
    }

    // ---- tilesets ----

    pub(super) fn set_tileset_def(
        &mut self,
        tileset: Slug,
        def: TilesetDef,
        index: Option<usize>,
    ) -> Result<Applied> {
        if def.slug != tileset {
            return Err(slug_mismatch("set_tileset_def", &def.slug, &tileset));
        }
        let pos = self.tilesets.iter().position(|t| t.slug == tileset);
        let inverse = match list_set(&mut self.tilesets, pos, def, index, "tilesets.json")? {
            ListEdit::Noop => return Ok(noop()),
            // Uses that already name it (dangling until now) stay when the
            // creation is undone.
            ListEdit::Created => Command::RemoveTilesetDef {
                keep_uses: !self.tileset_uses(&tileset).is_empty(),
                tileset: tileset.clone(),
            },
            ListEdit::Replaced { old, moved_from } => Command::SetTilesetDef {
                tileset: tileset.clone(),
                def: old,
                index: moved_from,
            },
        };
        Ok(Applied {
            inverse,
            changes: vec![Change::Def {
                kind: DefKind::Tileset,
                slug: tileset,
            }],
        })
    }

    /// Where tileset `slug` is used, the first few, described for an agent.
    fn tileset_uses(&mut self, slug: &Slug) -> Vec<String> {
        const SHOWN: usize = 5;
        let mut uses: Vec<String> = Vec::new();
        let mut more = 0;
        self.visit_tilesets(&mut |site, s| {
            if s == slug {
                let d = site.describe();
                if !uses.contains(&d) {
                    if uses.len() < SHOWN {
                        uses.push(d);
                    } else {
                        more += 1;
                    }
                }
            }
            None
        });
        if more > 0 {
            uses.push(format!("{more} more"));
        }
        uses
    }

    pub(super) fn remove_tileset_def(&mut self, tileset: Slug, keep_uses: bool) -> Result<Applied> {
        let i = self
            .tilesets
            .iter()
            .position(|t| t.slug == tileset)
            .ok_or_else(|| CommandError::UnknownTileset(tileset.clone()))?;
        let uses = self.tileset_uses(&tileset);
        if !uses.is_empty() && !keep_uses {
            return Err(CommandError::InUse {
                kind: DefKind::Tileset,
                slug: tileset,
                uses,
            });
        }
        let def = self.tilesets.remove(i);
        Ok(Applied {
            inverse: Command::SetTilesetDef {
                tileset: tileset.clone(),
                def,
                index: Some(i),
            },
            changes: vec![Change::Def {
                kind: DefKind::Tileset,
                slug: tileset,
            }],
        })
    }

    pub(super) fn rename_tileset(&mut self, from: Slug, to: Slug) -> Result<Applied> {
        let i = self
            .tilesets
            .iter()
            .position(|t| t.slug == from)
            .ok_or_else(|| CommandError::UnknownTileset(from.clone()))?;
        if from == to {
            return Ok(noop());
        }
        if self.tileset(to.as_str()).is_some() {
            return Err(CommandError::DuplicateDef {
                kind: DefKind::Tileset,
                slug: to,
            });
        }
        if let Some(what) = self.tileset_uses(&to).into_iter().next() {
            return Err(dangling(DefKind::Tileset, &to, &what));
        }
        self.tilesets[i].slug = to.clone();
        let mut changes = vec![Change::DefRenamed {
            kind: DefKind::Tileset,
            from: from.clone(),
            to: to.clone(),
        }];
        self.visit_tilesets(&mut |site, s| {
            (*s == from).then(|| {
                changes.push(site.change());
                to.clone()
            })
        });
        Ok(Applied {
            inverse: Command::RenameTileset { from: to, to: from },
            changes: coalesce(changes),
        })
    }

    /// Call `f` on every tileset reference in the project (not the
    /// tileset definitions' own slugs), with where it is. When `f` returns
    /// a slug, the reference becomes that slug.
    fn visit_tilesets(&mut self, f: &mut dyn FnMut(&Site, &Slug) -> Option<Slug>) {
        let Project {
            layers,
            enums,
            entities,
            rules,
            macros,
            worlds,
            level_fields,
            ..
        } = self;
        fn slot(site: &Site, s: &mut Slug, f: &mut dyn FnMut(&Site, &Slug) -> Option<Slug>) {
            if let Some(n) = f(site, s) {
                *s = n;
            }
        }
        for d in layers.iter_mut() {
            let site = Site::Def(DefKind::Layer, d.slug.clone());
            if let LayerKind::Tiles(t) = &mut d.kind
                && let Some(ts) = &mut t.tileset
            {
                slot(&site, ts, f);
            }
        }
        for e in enums.iter_mut() {
            let site = Site::Def(DefKind::Enum, e.slug.clone());
            for icon in e.values.iter_mut().filter_map(|v| v.icon.as_mut()) {
                slot(&site, &mut icon.tileset, f);
            }
        }
        for d in entities.iter_mut() {
            let site = Site::Def(DefKind::Entity, d.slug.clone());
            if let Some(icon) = &mut d.icon {
                slot(&site, &mut icon.tileset, f);
            }
            field_tilesets(&mut d.fields, &site, f);
        }
        field_tilesets(level_fields, &Site::LevelFields, f);
        for set in rules.iter_mut() {
            let site = Site::Def(DefKind::Rules, set.layer.clone());
            for st in &mut set.stamps {
                slot(&site, &mut st.tileset, f);
            }
            for g in &mut set.groups {
                if let Some(icon) = &mut g.icon {
                    slot(&site, &mut icon.tileset, f);
                }
                for e in &mut g.rules {
                    match e {
                        RuleEntry::Rule(r) => {
                            if let Some(out) = r.out_mut().and_then(|o| o.as_tiles_mut()) {
                                slot(&site, &mut out.tileset, f);
                            }
                        }
                        RuleEntry::Use(u) => {
                            let site = Site::Reference {
                                layer: set.layer.clone(),
                                reference: u.slug().clone(),
                            };
                            if let Some(ts) = &mut u.tileset {
                                slot(&site, ts, f);
                            }
                        }
                    }
                }
            }
        }
        if let Some(m) = macros {
            for (name, t) in m.templates.iter_mut() {
                let site = Site::Template(name.clone());
                for r in &mut t.rules {
                    if let TemplateOutput::Tiles(o) = &mut r.out
                        && let Some(ts) = &mut o.tileset
                    {
                        slot(&site, ts, f);
                    }
                }
            }
        }
        let entity_tiles: BTreeMap<Slug, Vec<Slug>> = entities
            .iter()
            .map(|d| (d.slug.clone(), tile_fields(&d.fields)))
            .filter(|(_, f)| !f.is_empty())
            .collect();
        let level_tiles = tile_fields(level_fields);
        for w in worlds.iter_mut() {
            for l in &mut w.levels {
                let (world, level) = (w.slug.clone(), l.slug.clone());
                for t in &mut l.tiles {
                    for p in &mut t.tiles {
                        let site = Site::Placement {
                            world: world.clone(),
                            level: level.clone(),
                            layer: t.layer.clone(),
                            cell: p.cell,
                        };
                        slot(&site, &mut p.tileset, f);
                    }
                }
                for (layer, st) in &mut l.layers {
                    if st.tilesets.is_empty() {
                        continue;
                    }
                    let site = Site::State {
                        world: world.clone(),
                        level: level.clone(),
                        layer: layer.clone(),
                    };
                    st.tilesets = std::mem::take(&mut st.tilesets)
                        .into_iter()
                        .map(|(mut k, mut v)| {
                            slot(&site, &mut k, f);
                            slot(&site, &mut v, f);
                            (k, v)
                        })
                        .collect();
                }
                let site = Site::LevelField {
                    world: world.clone(),
                    level: level.clone(),
                };
                for field in &level_tiles {
                    if let Some(v) = l.fields.get_mut(field) {
                        tile_value(v, &site, f);
                    }
                }
                for e in l.entities.iter_mut().flat_map(|x| &mut x.entities) {
                    let Some(fs) = entity_tiles.get(&e.ty) else {
                        continue;
                    };
                    let site = Site::Instance {
                        world: world.clone(),
                        level: level.clone(),
                        id: e.id.clone(),
                    };
                    for field in fs {
                        if let Some(v) = e.fields.get_mut(field) {
                            tile_value(v, &site, f);
                        }
                    }
                }
            }
        }
    }

    // ---- rule groups ----

    pub(super) fn set_rule_group(
        &mut self,
        layer: Slug,
        group: Slug,
        def: RuleGroup,
        index: Option<usize>,
    ) -> Result<Applied> {
        if def.slug != group {
            return Err(slug_mismatch("set_rule_group", &def.slug, &group));
        }
        let changes = vec![Change::Def {
            kind: DefKind::Rules,
            slug: layer.clone(),
        }];
        let Some(k) = self.rules.iter().position(|r| r.layer == layer) else {
            // A new rule set, for an auto layer.
            self.layer_kind(&layer, "auto", LayerDef::is_auto)?;
            if let Some(i) = index.filter(|&i| i > 0) {
                return Err(CommandError::Invalid(format!(
                    "index {i} is past the end of rules/{layer}.json groups (0 entries)"
                )));
            }
            let k = self.rules.partition_point(|r| r.layer < layer);
            self.rules.insert(
                k,
                RuleSet {
                    layer: layer.clone(),
                    stamps: Vec::new(),
                    groups: vec![def],
                },
            );
            return Ok(Applied {
                inverse: Command::RemoveRuleGroup {
                    layer,
                    group,
                    keep_empty: false,
                },
                changes,
            });
        };
        super::rules::unique_groups(&self.rules[k])?;
        let set = &mut self.rules[k];
        let was_empty = set.groups.is_empty() && set.stamps.is_empty();
        let pos = set.groups.iter().position(|g| g.slug == group);
        let what = format!("rules/{layer}.json groups");
        let inverse = match list_set(&mut set.groups, pos, def, index, &what)? {
            ListEdit::Noop => return Ok(noop()),
            ListEdit::Created => Command::RemoveRuleGroup {
                layer,
                group,
                keep_empty: was_empty,
            },
            ListEdit::Replaced { old, moved_from } => Command::SetRuleGroup {
                layer,
                group,
                def: old,
                index: moved_from,
            },
        };
        Ok(Applied { inverse, changes })
    }

    pub(super) fn remove_rule_group(
        &mut self,
        layer: Slug,
        group: Slug,
        keep_empty: bool,
    ) -> Result<Applied> {
        let unknown = || CommandError::UnknownRuleGroup {
            layer: layer.clone(),
            group: group.clone(),
        };
        let k = self
            .rules
            .iter()
            .position(|r| r.layer == layer)
            .ok_or_else(unknown)?;
        super::rules::unique_groups(&self.rules[k])?;
        let gi = self.rules[k]
            .groups
            .iter()
            .position(|g| g.slug == group)
            .ok_or_else(unknown)?;
        let def = self.rules[k].groups.remove(gi);
        // A rule set not tied to an auto layer is kept, so that putting the
        // group back (which needs an auto layer to make a new set) works.
        let auto = self.layer(layer.as_str()).is_some_and(LayerDef::is_auto);
        let set = &self.rules[k];
        if set.groups.is_empty() && set.stamps.is_empty() && !keep_empty && auto {
            self.rules.remove(k);
        }
        Ok(Applied {
            inverse: Command::SetRuleGroup {
                layer: layer.clone(),
                group,
                def,
                index: Some(gi),
            },
            changes: vec![Change::Def {
                kind: DefKind::Rules,
                slug: layer,
            }],
        })
    }
}

impl Project {
    /// Every tileset reference with what holds it, for
    /// [`crate::merge`]'s dependencies. Level fields' types and defaults
    /// (one site for all of them) are left out: the merge reads those
    /// itself.
    pub(crate) fn tileset_refs(&mut self) -> Vec<(crate::merge::Owner, Slug)> {
        use crate::merge::Owner;
        let mut out = Vec::new();
        self.visit_tilesets(&mut |site, s| {
            let owner = match site {
                Site::Def(DefKind::Rules, layer) => Some(Owner::Layer(layer.clone())),
                Site::Def(kind, slug) => Some(Owner::Def(*kind, slug.clone())),
                Site::Template(name) => Some(Owner::Template(name.clone())),
                Site::Reference { layer, .. } => Some(Owner::Layer(layer.clone())),
                Site::LevelFields => None,
                Site::Placement { level, .. }
                | Site::State { level, .. }
                | Site::Instance { level, .. }
                | Site::LevelField { level, .. } => Some(Owner::Level(level.clone())),
            };
            if let Some(o) = owner {
                out.push((o, s.clone()));
            }
            None
        });
        out
    }
}
