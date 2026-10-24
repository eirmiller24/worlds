//! `Project::apply`: one function per command, each computing its inverse
//! from the state it is about to overwrite. Definition commands live in
//! [`defs`], field definition commands in [`fields`], rule, macro and stamp
//! commands in [`rules`].

mod assembly;
mod defs;
mod duplicate;
mod fields;
mod layer_ops;
mod references;
mod resize;
mod rules;
mod worlds;

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use super::{
    Applied, CellValue, Change, Command, CommandError, GridValue, LevelContent, LevelPart,
    NoteScope, ProjectSetting, Rect, coalesce,
};
use crate::format::IntGridFile;
use crate::id::{Id, Slug};
use crate::model::{
    Annotation, DerivedOverrides, EntitiesLayer, EntityInstance, GuideGroup, IntGridLayer,
    LayerDef, Level, Project, Shape, TilePlacement, TilesLayer,
};

type Result<T> = std::result::Result<T, CommandError>;

/// Position of a level: (world index, level index).
type LevelAt = (usize, usize);

impl Project {
    /// Apply a command. On success the project has changed (unless it was a
    /// no-op), the changed files are marked dirty, and the result holds the
    /// exact inverse and the changes. On error nothing changed.
    pub fn apply(&mut self, cmd: Command) -> Result<Applied> {
        let applied = self.apply_inner(cmd)?;
        for c in &applied.changes {
            for f in c.files() {
                self.files.mark_dirty(f);
            }
        }
        Ok(applied)
    }

    fn apply_inner(&mut self, cmd: Command) -> Result<Applied> {
        use Command::*;
        match cmd {
            PaintCells {
                level,
                layer,
                cells,
                keep_empty,
            } => {
                let def = self.grid_def(&layer)?.clone();
                let mut writes = BTreeMap::new();
                for c in cells {
                    writes.insert(yx(c.cell), value_index(&def, &c.value)?);
                }
                self.write_cells(level, def, writes, keep_empty)
            }
            FillRect {
                level,
                layer,
                rect,
                value,
            } => {
                let def = self.grid_def(&layer)?.clone();
                let at = self.find_level(&level)?;
                let size = self.layer_cells(&def, self.level_at(at));
                check_rect(&def.slug, rect, size)?;
                let v = value_index(&def, &value)?;
                let mut writes = BTreeMap::new();
                for y in rect.y..rect.y + rect.h {
                    for x in rect.x..rect.x + rect.w {
                        writes.insert((y, x), v);
                    }
                }
                self.write_cells(level, def, writes, false)
            }
            FloodFill {
                level,
                layer,
                at,
                value,
            } => {
                let def = self.grid_def(&layer)?.clone();
                let v = value_index(&def, &value)?;
                let la = self.find_level(&level)?;
                let size = self.layer_cells(&def, self.level_at(la));
                check_cell(&def.slug, [at[0] as i64, at[1] as i64], size)?;
                let cells = self.grid_cells(la, &def, size)?;
                let writes = flood(&cells, size, at, v);
                self.write_cells(level, def, writes, false)
            }
            ReplaceValue {
                level,
                layer,
                from,
                to,
            } => {
                let def = self.grid_def(&layer)?.clone();
                let (f, t) = (value_index(&def, &from)?, value_index(&def, &to)?);
                let la = self.find_level(&level)?;
                let size = self.layer_cells(&def, self.level_at(la));
                let cells = self.grid_cells(la, &def, size)?;
                let mut writes = BTreeMap::new();
                if f != t {
                    for (i, &c) in cells.iter().enumerate() {
                        if c == f {
                            let (x, y) = (i as u32 % size[0], i as u32 / size[0]);
                            writes.insert((y, x), t);
                        }
                    }
                }
                self.write_cells(level, def, writes, false)
            }
            SetTiles {
                level,
                layer,
                tiles,
                clear,
                keep_empty,
            } => self.write_tiles(level, layer, tiles, clear, keep_empty),
            ClearTiles { level, layer, rect } => {
                let def = self.layer_kind(&layer, "tiles", LayerDef::is_tiles)?;
                let at = self.find_level(&level)?;
                let size = self.layer_cells(def, self.level_at(at));
                check_rect(&layer, rect, size)?;
                let clear: BTreeSet<[u32; 2]> = self
                    .level_at(at)
                    .tiles_layer(layer.as_str())
                    .map(|t| {
                        t.tiles
                            .iter()
                            .map(|p| p.cell)
                            .filter(|c| rect.contains(*c))
                            .collect()
                    })
                    .unwrap_or_default();
                self.write_tiles(level, layer, Vec::new(), clear.into_iter().collect(), false)
            }
            CreateLevel {
                world,
                level,
                content,
                restore,
            } => self.create_level(world, level, content, restore),
            DeleteLevel { world, level } => self.delete_level(world, level),
            ReplaceLevel {
                world,
                level,
                content,
                restore,
            } => self.replace_level(world, level, content, restore),
            MoveLevel {
                world,
                level,
                world_px,
            } => {
                let at = self.find_level_in(&world, &level)?;
                let l = self.level_at_mut(at);
                let old = std::mem::replace(&mut l.world_px, world_px);
                Ok(meta(
                    old != world_px,
                    MoveLevel {
                        world: world.clone(),
                        level: level.clone(),
                        world_px: old,
                    },
                    world,
                    level,
                    LevelPart::Position,
                ))
            }
            ResizeLevel {
                world,
                level,
                size,
                anchor,
            } => self.resize_level(world, level, size, anchor),
            RenameLevel {
                world,
                level,
                slug,
                name,
            } => self.rename_level(world, level, slug, name),
            SetLevelField {
                world,
                level,
                field,
                value,
                allow_unknown,
            } => {
                let at = self.find_level_in(&world, &level)?;
                let known = self.level_fields.iter().any(|f| f.slug == field);
                if value.is_some() && !known && !allow_unknown {
                    return Err(CommandError::UnknownField {
                        owner: "project level_fields".into(),
                        field,
                    });
                }
                let l = self.level_at_mut(at);
                let old = match &value {
                    Some(v) => l.fields.insert(field.clone(), v.clone()),
                    None => l.fields.remove(&field),
                };
                Ok(meta(
                    old != value,
                    SetLevelField {
                        world: world.clone(),
                        level: level.clone(),
                        allow_unknown: old.is_some() && !known,
                        field,
                        value: old,
                    },
                    world,
                    level,
                    LevelPart::Fields,
                ))
            }
            SetLevelDepth {
                world,
                level,
                depth,
            } => {
                let at = self.find_level_in(&world, &level)?;
                let old = std::mem::replace(&mut self.level_at_mut(at).depth, depth);
                Ok(meta(
                    old != depth,
                    SetLevelDepth {
                        world: world.clone(),
                        level: level.clone(),
                        depth: old,
                    },
                    world,
                    level,
                    LevelPart::Depth,
                ))
            }
            SetLevelBg {
                world,
                level,
                bg_color,
            } => {
                let at = self.find_level_in(&world, &level)?;
                let old = std::mem::replace(&mut self.level_at_mut(at).bg_color, bg_color.clone());
                Ok(meta(
                    old != bg_color,
                    SetLevelBg {
                        world: world.clone(),
                        level: level.clone(),
                        bg_color: old,
                    },
                    world,
                    level,
                    LevelPart::BgColor,
                ))
            }
            SetLevelTags { world, level, tags } => {
                let at = self.find_level_in(&world, &level)?;
                let old = std::mem::replace(&mut self.level_at_mut(at).tags, tags.clone());
                Ok(meta(
                    old != tags,
                    SetLevelTags {
                        world: world.clone(),
                        level: level.clone(),
                        tags: old,
                    },
                    world,
                    level,
                    LevelPart::Tags,
                ))
            }
            AddEntity {
                level,
                layer,
                entity,
                index,
                allow_outside,
            } => self.add_entity(level, layer, entity, index, allow_outside),
            RemoveEntity { id, keep_empty } => self.remove_entity(id, keep_empty),
            MoveEntity {
                id,
                cell,
                offset_px,
                allow_outside,
            } => {
                let (at, li, ei) = self.find_entity(&id)?;
                let layer = self.level_at(at).entities[li].layer.clone();
                let def = self.layer_kind(&layer, "entities", LayerDef::is_entities)?;
                let size = self.layer_cells(def, self.level_at(at));
                if !allow_outside {
                    check_cell(&layer, [cell[0] as i64, cell[1] as i64], size)?;
                }
                let e = &mut self.level_at_mut(at).entities[li].entities[ei];
                let old = (
                    std::mem::replace(&mut e.cell, cell),
                    std::mem::replace(&mut e.offset_px, offset_px),
                );
                let changed = old != (cell, offset_px);
                Ok(self.entity_change(
                    at,
                    changed,
                    MoveEntity {
                        id: id.clone(),
                        cell: old.0,
                        offset_px: old.1,
                        // Put back where it was, even if a resize left
                        // that outside.
                        allow_outside: !cell_inside(old.0, size),
                    },
                    id,
                ))
            }
            ResizeEntity { id, size } => {
                let (at, li, ei) = self.find_entity(&id)?;
                let e = &mut self.level_at_mut(at).entities[li].entities[ei];
                let old = std::mem::replace(&mut e.size, size);
                Ok(self.entity_change(
                    at,
                    old != size,
                    ResizeEntity {
                        id: id.clone(),
                        size: old,
                    },
                    id,
                ))
            }
            SetEntityField {
                id,
                field,
                value,
                allow_unknown,
            } => {
                let (at, li, ei) = self.find_entity(&id)?;
                let ty = self.level_at(at).entities[li].entities[ei].ty.clone();
                let def = self.entity_def(ty.as_str());
                let known = def.is_some_and(|d| d.field(field.as_str()).is_some());
                if value.is_some() && !allow_unknown {
                    if def.is_none() {
                        return Err(CommandError::UnknownEntityType(ty));
                    }
                    if !known {
                        return Err(CommandError::UnknownField {
                            owner: format!("entity type {ty:?}"),
                            field,
                        });
                    }
                }
                let e = &mut self.level_at_mut(at).entities[li].entities[ei];
                let old = match &value {
                    Some(v) => e.fields.insert(field.clone(), v.clone()),
                    None => e.fields.remove(&field),
                };
                Ok(self.entity_change(
                    at,
                    old != value,
                    SetEntityField {
                        id: id.clone(),
                        allow_unknown: old.is_some() && !known,
                        field,
                        value: old,
                    },
                    id,
                ))
            }
            AddNote { scope, note, index } => self.add_note(scope, note, index),
            RemoveNote { id } => {
                let (scope, i) = self.note_position(&id)?;
                let (world, level) = self.scope_names(&scope)?;
                let note = self.notes_mut(&scope)?.remove(i);
                Ok(Applied {
                    inverse: AddNote {
                        scope,
                        note,
                        index: Some(i),
                    },
                    changes: vec![Change::Note { world, level, id }],
                })
            }
            UpdateNote { id, note } => {
                if note.id != id {
                    return Err(CommandError::Invalid(format!(
                        "update_note: note.id {:?} is not {id:?}",
                        note.id
                    )));
                }
                let (scope, i) = self.note_position(&id)?;
                let (world, level) = self.scope_names(&scope)?;
                let slot = &mut self.notes_mut(&scope)?[i];
                if *slot == note {
                    return Ok(noop());
                }
                let old = std::mem::replace(slot, note);
                Ok(Applied {
                    inverse: UpdateNote {
                        id: id.clone(),
                        note: old,
                    },
                    changes: vec![Change::Note { world, level, id }],
                })
            }
            SetGuideGroup {
                world,
                group,
                index,
            } => self.set_guide_group(world, group, index),
            RemoveGuideGroup { world, slug } => self.remove_guide_group(world, slug),
            SetLayerState {
                level,
                layer,
                state,
            } => {
                self.layer_kind(&layer, "auto", LayerDef::is_auto)?;
                let at = self.find_level(&level)?;
                let world = self.worlds[at.0].slug.clone();
                let l = self.level_at_mut(at);
                let old = match &state {
                    Some(s) => l.layers.insert(layer.clone(), s.clone()),
                    None => l.layers.remove(&layer),
                };
                if old == state {
                    return Ok(noop());
                }
                Ok(Applied {
                    inverse: SetLayerState {
                        level: level.clone(),
                        layer: layer.clone(),
                        state: old,
                    },
                    changes: vec![Change::LayerState {
                        world,
                        level,
                        layer,
                    }],
                })
            }
            SetProjectSetting { setting } => self.set_setting(setting),
            SetLayerDef {
                layer,
                def,
                index,
                content,
                rules,
            } => self.set_layer_def(layer, def, index, content, rules),
            RemoveLayerDef { layer } => self.remove_layer_def(layer),
            RenameLayer { from, to } => self.rename_layer(from, to),
            SetEntityDef { entity, def } => self.set_entity_def(entity, def),
            RemoveEntityDef {
                entity,
                keep_instances,
            } => self.remove_entity_def(entity, keep_instances),
            RenameEntityType { from, to } => self.rename_entity_type(from, to),
            SetEntityFieldDef {
                entity,
                field,
                def,
                index,
            } => self.set_field_def(fields::Owner::Entity(&entity), field, def, index),
            RemoveEntityFieldDef {
                entity,
                field,
                keep_values,
            } => self.remove_field_def(fields::Owner::Entity(&entity), field, keep_values),
            RenameEntityField { entity, from, to } => {
                self.rename_field(fields::Owner::Entity(&entity), from, to)
            }
            SetLevelFieldDef { field, def, index } => {
                self.set_field_def(fields::Owner::Level, field, def, index)
            }
            RemoveLevelFieldDef { field, keep_values } => {
                self.remove_field_def(fields::Owner::Level, field, keep_values)
            }
            RenameLevelField { from, to } => self.rename_field(fields::Owner::Level, from, to),
            SetEnumDef { enum_, def, index } => self.set_enum_def(enum_, def, index),
            RemoveEnumDef { enum_ } => self.remove_enum_def(enum_),
            RenameEnum { from, to } => self.rename_enum(from, to),
            RenameEnumValue { enum_, from, to } => self.rename_enum_value(enum_, from, to),
            SetTilesetDef {
                tileset,
                def,
                index,
            } => self.set_tileset_def(tileset, def, index),
            RemoveTilesetDef { tileset, keep_uses } => self.remove_tileset_def(tileset, keep_uses),
            RenameTileset { from, to } => self.rename_tileset(from, to),
            SetRuleGroup {
                layer,
                group,
                def,
                index,
            } => self.set_rule_group(layer, group, def, index),
            RemoveRuleGroup {
                layer,
                group,
                keep_empty,
            } => self.remove_rule_group(layer, group, keep_empty),
            SetRule {
                layer,
                group,
                rule,
                def,
                index,
            } => self.set_rule(layer, group, rule, def, index),
            RemoveRule { layer, rule } => self.remove_rule(layer, rule),
            MoveRule {
                layer,
                rule,
                group,
                index,
            } => self.move_rule(layer, rule, group, index),
            SetMacro { kind, name, def } => self.set_macro(kind, name, def),
            RemoveMacro {
                kind,
                name,
                keep_uses,
                keep_empty,
            } => self.remove_macro(kind, name, keep_uses, keep_empty),
            RenameMacro { kind, from, to } => self.rename_macro(kind, from, to),
            SetStamp {
                layer,
                stamp,
                def,
                index,
            } => self.set_stamp(layer, stamp, def, index),
            RemoveStamp {
                layer,
                stamp,
                keep_uses,
                keep_empty,
            } => self.remove_stamp(layer, stamp, keep_uses, keep_empty),
            Batch { label, commands } => self.batch(label, commands),
            DuplicateLevel {
                level,
                slug,
                name,
                world,
                world_px,
            } => self.duplicate_level(level, slug, name, world, world_px),
            CreateWorld { slug, name } => self.create_world(slug, name),
            RenameWorld { world, slug, name } => self.rename_world(world, slug, name),
            RemoveWorld { world, confirm } => self.remove_world(world, confirm),
            ReorderWorlds { order, restore } => self.reorder_worlds(order, restore),
            MoveLevelToWorld {
                level,
                world,
                world_px,
            } => self.move_level_to_world(level, world, world_px),
            DuplicateLayer { layer, slug } => self.duplicate_layer(layer, slug),
            AddReference {
                scope,
                reference,
                index,
            } => self.add_reference(scope, reference, index),
            UpdateReference { id, reference } => self.update_reference(id, reference),
            RemoveReference { id } => self.remove_reference(id),
            SetAssembly { assembly } => self.set_assembly(assembly),
            RemoveAssembly { slug } => self.remove_assembly(slug),
            RenameAssembly { slug, new_slug } => self.rename_assembly(slug, new_slug),
        }
    }

    // ---- lookups ----

    fn level_at(&self, (w, l): LevelAt) -> &Level {
        &self.worlds[w].levels[l]
    }

    fn level_at_mut(&mut self, (w, l): LevelAt) -> &mut Level {
        &mut self.worlds[w].levels[l]
    }

    fn find_level(&self, slug: &Slug) -> Result<LevelAt> {
        for (wi, w) in self.worlds.iter().enumerate() {
            if let Some(li) = w.levels.iter().position(|l| l.slug == *slug) {
                return Ok((wi, li));
            }
        }
        Err(CommandError::UnknownLevel(slug.clone()))
    }

    fn find_world(&self, slug: &Slug) -> Result<usize> {
        self.worlds
            .iter()
            .position(|w| w.slug == *slug)
            .ok_or_else(|| CommandError::UnknownWorld(slug.clone()))
    }

    fn find_level_in(&self, world: &Slug, level: &Slug) -> Result<LevelAt> {
        let wi = self.find_world(world)?;
        match self.worlds[wi].levels.iter().position(|l| l.slug == *level) {
            Some(li) => Ok((wi, li)),
            None => match self.find_level(level) {
                Ok(_) => Err(CommandError::LevelNotInWorld {
                    world: world.clone(),
                    level: level.clone(),
                }),
                Err(e) => Err(e),
            },
        }
    }

    fn layer_kind(
        &self,
        slug: &Slug,
        expected: &'static str,
        is: fn(&LayerDef) -> bool,
    ) -> Result<&LayerDef> {
        let def = self
            .layer(slug.as_str())
            .ok_or_else(|| CommandError::UnknownLayer(slug.clone()))?;
        if is(def) {
            Ok(def)
        } else {
            Err(CommandError::WrongLayerKind {
                layer: slug.clone(),
                expected,
                actual: def.kind.name(),
            })
        }
    }

    fn grid_def(&self, slug: &Slug) -> Result<&LayerDef> {
        self.layer_kind(slug, "int_grid", LayerDef::is_int_grid)
    }

    /// (layer index, entity index) of an entity in a level.
    fn find_entity(&self, id: &Id) -> Result<(LevelAt, usize, usize)> {
        for (wi, w) in self.worlds.iter().enumerate() {
            for (li, l) in w.levels.iter().enumerate() {
                for (ai, layer) in l.entities.iter().enumerate() {
                    if let Some(ei) = layer.entities.iter().position(|e| e.id == *id) {
                        return Ok(((wi, li), ai, ei));
                    }
                }
            }
        }
        Err(CommandError::UnknownEntity(id.clone()))
    }

    fn note_position(&self, id: &Id) -> Result<(NoteScope, usize)> {
        for w in &self.worlds {
            if let Some(i) = w.notes.iter().position(|n| n.id == *id) {
                return Ok((NoteScope::World(w.slug.clone()), i));
            }
            for l in &w.levels {
                if let Some(i) = l.notes.iter().position(|n| n.id == *id) {
                    return Ok((NoteScope::Level(l.slug.clone()), i));
                }
            }
        }
        Err(CommandError::UnknownNote(id.clone()))
    }

    /// (world slug, level slug) of a note scope.
    fn scope_names(&self, scope: &NoteScope) -> Result<(Slug, Option<Slug>)> {
        match scope {
            NoteScope::World(w) => {
                self.find_world(w)?;
                Ok((w.clone(), None))
            }
            NoteScope::Level(l) => {
                let (wi, _) = self.find_level(l)?;
                Ok((self.worlds[wi].slug.clone(), Some(l.clone())))
            }
        }
    }

    fn notes_mut(&mut self, scope: &NoteScope) -> Result<&mut Vec<Annotation>> {
        match scope {
            NoteScope::World(w) => {
                let wi = self.find_world(w)?;
                Ok(&mut self.worlds[wi].notes)
            }
            NoteScope::Level(l) => {
                let at = self.find_level(l)?;
                Ok(&mut self.level_at_mut(at).notes)
            }
        }
    }

    fn entity_ids(&self) -> BTreeSet<&Id> {
        self.levels()
            .flat_map(|l| l.all_entities())
            .map(|e| &e.id)
            .collect()
    }

    fn note_ids(&self) -> BTreeSet<&Id> {
        self.worlds
            .iter()
            .flat_map(|w| w.notes.iter().chain(w.levels.iter().flat_map(|l| &l.notes)))
            .map(|n| &n.id)
            .collect()
    }

    fn entity_change(&self, at: LevelAt, changed: bool, inverse: Command, id: Id) -> Applied {
        if !changed {
            return noop();
        }
        Applied {
            inverse,
            changes: vec![Change::Entity {
                world: self.worlds[at.0].slug.clone(),
                level: self.level_at(at).slug.clone(),
                id,
            }],
        }
    }

    /// The grid's cells as stored state codes (value indices; on a derived
    /// layer its overrides, see [`Plane`]), all 0 if the level has no
    /// content for it. Errs if the stored grid has the wrong size.
    fn grid_cells(&self, at: LevelAt, def: &LayerDef, size: [u32; 2]) -> Result<Vec<u16>> {
        if def.is_derived() {
            plane_cells::<DerivedOverrides>(self.level_at(at), def, size)
        } else {
            plane_cells::<IntGridLayer>(self.level_at(at), def, size)
        }
    }

    // ---- grids ----

    /// Write state codes (keyed `(y, x)`) into an int_grid layer: its cells,
    /// or a derived layer's overrides. The inverse is a `paint_cells` of the
    /// old states of the cells that actually changed.
    fn write_cells(
        &mut self,
        level: Slug,
        def: LayerDef,
        writes: BTreeMap<(u32, u32), u16>,
        keep_empty: bool,
    ) -> Result<Applied> {
        if def.is_derived() {
            self.write_plane::<DerivedOverrides>(level, def, writes, keep_empty)
        } else {
            self.write_plane::<IntGridLayer>(level, def, writes, keep_empty)
        }
    }

    fn write_plane<P: Plane>(
        &mut self,
        level: Slug,
        def: LayerDef,
        writes: BTreeMap<(u32, u32), u16>,
        keep_empty: bool,
    ) -> Result<Applied> {
        let at = self.find_level(&level)?;
        let size = self.layer_cells(&def, self.level_at(at));
        for &(y, x) in writes.keys() {
            check_cell(&def.slug, [x as i64, y as i64], size)?;
        }
        let lvl = self.level_at(at);
        let list = P::list(lvl);
        let existing = list.iter().position(|g| *g.layer() == def.slug);
        if let Some(i) = existing
            && list[i].size() != size
        {
            return Err(CommandError::GridSize {
                layer: def.slug.clone(),
                actual: list[i].size(),
                expected: size,
            });
        }
        let read = |x: u32, y: u32| existing.map_or(0, |i| list[i].code(x, y));
        let mut prev = Vec::new();
        let mut changed = Vec::new();
        for (&(y, x), &v) in &writes {
            let old = read(x, y);
            if old != v {
                prev.push(CellValue::new(x, y, index_value(&def, x, y, old)?));
                changed.push((x, y, v));
            }
        }
        if changed.is_empty() {
            return Ok(noop());
        }
        let was_empty = existing.map(|i| list[i].is_empty());
        let rect = Rect::bounding(changed.iter().map(|&(x, y, _)| [x, y]));
        let world = self.worlds[at.0].slug.clone();

        let lvl = self.level_at_mut(at);
        let list = P::list_mut(lvl);
        let gi = match existing {
            Some(i) => i,
            None => {
                let i = list.partition_point(|g| *g.layer() < def.slug);
                list.insert(i, P::empty(&def, size));
                i
            }
        };
        let g = &mut list[gi];
        for (x, y, v) in changed {
            g.set_code(x, y, v);
        }
        if !keep_empty && g.is_empty() {
            list.remove(gi);
        }
        Ok(Applied {
            inverse: Command::PaintCells {
                level: level.clone(),
                layer: def.slug.clone(),
                cells: prev,
                keep_empty: was_empty == Some(true),
            },
            changes: vec![Change::Grid {
                world,
                level,
                layer: def.slug,
                rect,
            }],
        })
    }

    // ---- tiles ----

    /// Replace the tile stacks at the cells of `tiles` and `clear`. The
    /// inverse puts back the old stacks of the cells that changed.
    fn write_tiles(
        &mut self,
        level: Slug,
        layer: Slug,
        tiles: Vec<TilePlacement>,
        clear: Vec<[u32; 2]>,
        keep_empty: bool,
    ) -> Result<Applied> {
        let def = self.layer_kind(&layer, "tiles", LayerDef::is_tiles)?;
        let at = self.find_level(&level)?;
        let size = self.layer_cells(def, self.level_at(at));
        for c in clear.iter().chain(tiles.iter().map(|t| &t.cell)) {
            check_cell(&layer, [c[0] as i64, c[1] as i64], size)?;
        }
        for t in &tiles {
            if self.tileset(t.tileset.as_str()).is_none() {
                return Err(CommandError::UnknownTileset(t.tileset.clone()));
            }
        }
        // New stack per affected cell, keyed (y, x), in the given order.
        let mut new: BTreeMap<(u32, u32), Vec<TilePlacement>> = BTreeMap::new();
        for c in &clear {
            new.entry(yx(*c)).or_default();
        }
        for t in tiles {
            new.entry(yx(t.cell)).or_default().push(t);
        }
        let lvl = self.level_at(at);
        let existing = lvl.tiles.iter().position(|t| t.layer == layer);
        let old_tiles: &[TilePlacement] = existing.map_or(&[], |i| &lvl.tiles[i].tiles);
        let mut old: BTreeMap<(u32, u32), Vec<TilePlacement>> = BTreeMap::new();
        for t in old_tiles {
            if new.contains_key(&yx(t.cell)) {
                old.entry(yx(t.cell)).or_default().push(t.clone());
            }
        }
        let empty = Vec::new();
        new.retain(|k, stack| old.get(k).unwrap_or(&empty) != stack);
        if new.is_empty() {
            return Ok(noop());
        }
        let rect = Rect::bounding(new.keys().map(|&(y, x)| [x, y]));
        let was_empty = existing.map(|i| lvl.tiles[i].tiles.is_empty());
        let mut inv_tiles = Vec::new();
        let mut inv_clear = Vec::new();
        for &(y, x) in new.keys() {
            match old.remove(&(y, x)) {
                Some(stack) => inv_tiles.extend(stack),
                None => inv_clear.push([x, y]),
            }
        }

        // Merge: untouched placements (already sorted) with the new stacks.
        let mut merged = Vec::with_capacity(old_tiles.len() + new.len());
        let touched: BTreeSet<(u32, u32)> = new.keys().copied().collect();
        let mut rest = old_tiles
            .iter()
            .filter(|t| !touched.contains(&yx(t.cell)))
            .peekable();
        for (key, stack) in new {
            while let Some(t) = rest.next_if(|t| yx(t.cell) < key) {
                merged.push(t.clone());
            }
            merged.extend(stack);
        }
        merged.extend(rest.cloned());

        let world = self.worlds[at.0].slug.clone();
        let lvl = self.level_at_mut(at);
        match existing {
            Some(i) if merged.is_empty() && !keep_empty => {
                lvl.tiles.remove(i);
            }
            Some(i) => lvl.tiles[i].tiles = merged,
            None => {
                let i = lvl.tiles.partition_point(|t| t.layer < layer);
                lvl.tiles.insert(
                    i,
                    TilesLayer {
                        layer: layer.clone(),
                        tiles: merged,
                    },
                );
            }
        }
        Ok(Applied {
            inverse: Command::SetTiles {
                level: level.clone(),
                layer: layer.clone(),
                tiles: inv_tiles,
                clear: inv_clear,
                keep_empty: was_empty == Some(true),
            },
            changes: vec![Change::Tiles {
                world,
                level,
                layer,
                rect,
            }],
        })
    }

    // ---- levels ----

    /// A level from its `level.json` part and content, checked against the
    /// project. `ignore` is a level whose ids may be reused (the one being
    /// replaced).
    fn build_level(
        &self,
        mut level: Level,
        content: LevelContent,
        ignore: Option<LevelAt>,
        restore: bool,
    ) -> Result<Level> {
        if !(level.int_grids.is_empty()
            && level.overrides.is_empty()
            && level.tiles.is_empty()
            && level.entities.is_empty()
            && level.notes.is_empty())
        {
            return Err(CommandError::Invalid(
                "the level carries layer content in memory; put it in `content`".into(),
            ));
        }
        if !restore && level.size.contains(&0) {
            return Err(CommandError::Invalid(format!(
                "size {:?} must be at least 1x1",
                level.size
            )));
        }
        let skip = ignore.map(|at| self.level_at(at));
        let other_levels = || {
            self.levels()
                .filter(move |l| skip.is_none_or(|s| !std::ptr::eq(*l, s)))
        };
        if !restore && other_levels().any(|l| l.id == level.id) {
            return Err(CommandError::DuplicateId(level.id.clone()));
        }
        for f in content.int_grids {
            let def = self.grid_def(&f.layer)?;
            let bad = |(path, msg): (String, String)| {
                let at = if path.is_empty() {
                    String::new()
                } else {
                    format!(".{path}")
                };
                CommandError::Invalid(format!("int_grids[{}]{at}: {msg}", f.layer))
            };
            let expected = self.layer_cells(def, &level);
            let actual = if def.is_derived() {
                let o = f.to_overrides(def, false).map_err(bad)?;
                let size = o.size();
                if level.overrides(f.layer.as_str()).is_some() {
                    return Err(CommandError::Invalid(format!(
                        "layer {} given twice",
                        f.layer
                    )));
                }
                level.overrides.push(o);
                size
            } else {
                let grid = f.to_layer(def).map_err(bad)?;
                let size = grid.size();
                if level.int_grid(f.layer.as_str()).is_some() {
                    return Err(CommandError::Invalid(format!(
                        "layer {} given twice",
                        f.layer
                    )));
                }
                level.int_grids.push(grid);
                size
            };
            if !restore && actual != expected {
                return Err(CommandError::GridSize {
                    layer: f.layer,
                    actual,
                    expected,
                });
            }
        }
        for t in content.tiles {
            self.layer_kind(&t.layer, "tiles", LayerDef::is_tiles)?;
            if level.tiles_layer(t.layer.as_str()).is_some() {
                return Err(CommandError::Invalid(format!(
                    "layer {} given twice",
                    t.layer
                )));
            }
            level.tiles.push(t);
        }
        let taken: BTreeSet<Id> = other_levels()
            .flat_map(|l| l.all_entities())
            .map(|e| e.id.clone())
            .collect();
        let mut seen = BTreeSet::new();
        for e in content.entities {
            self.layer_kind(&e.layer, "entities", LayerDef::is_entities)?;
            if level.entities_layer(e.layer.as_str()).is_some() {
                return Err(CommandError::Invalid(format!(
                    "layer {} given twice",
                    e.layer
                )));
            }
            for i in &e.entities {
                if !restore && (taken.contains(&i.id) || !seen.insert(i.id.clone())) {
                    return Err(CommandError::DuplicateId(i.id.clone()));
                }
            }
            level.entities.push(e);
        }
        let taken: BTreeSet<Id> = self
            .worlds
            .iter()
            .flat_map(|w| w.notes.iter())
            .chain(other_levels().flat_map(|l| l.notes.iter()))
            .map(|n| n.id.clone())
            .collect();
        let mut seen = BTreeSet::new();
        for n in &content.notes {
            if !restore && (taken.contains(&n.id) || !seen.insert(n.id.clone())) {
                return Err(CommandError::DuplicateId(n.id.clone()));
            }
        }
        level.notes = content.notes;
        if !restore {
            let taken: BTreeSet<&Id> = self
                .worlds
                .iter()
                .flat_map(|w| w.references.iter())
                .chain(other_levels().flat_map(|l| l.references.iter()))
                .map(|r| &r.id)
                .collect();
            let mut seen = BTreeSet::new();
            for r in &level.references {
                if taken.contains(&r.id) || !seen.insert(&r.id) {
                    return Err(CommandError::DuplicateId(r.id.clone()));
                }
            }
        }
        level.normalize();
        Ok(level)
    }

    /// Split a level into its `level.json` part and its content (what
    /// `create_level` takes).
    pub fn split_level(&self, mut level: Level) -> Result<(Level, LevelContent)> {
        let mut int_grids = Vec::new();
        for g in std::mem::take(&mut level.int_grids) {
            let def = self.grid_def(&g.layer)?;
            let rows = g.to_rows(def).map_err(CommandError::Invalid)?;
            int_grids.push(IntGridFile {
                layer: g.layer,
                overrides: None,
                legend: None,
                rows,
            });
        }
        for o in std::mem::take(&mut level.overrides) {
            let def = self.grid_def(&o.layer)?;
            let rows = o.to_rows(def).map_err(CommandError::Invalid)?;
            int_grids.push(IntGridFile {
                layer: o.layer,
                overrides: Some(true),
                legend: None,
                rows,
            });
        }
        int_grids.sort_by(|a, b| a.layer.cmp(&b.layer));
        let content = LevelContent {
            int_grids,
            tiles: std::mem::take(&mut level.tiles),
            entities: std::mem::take(&mut level.entities),
            notes: std::mem::take(&mut level.notes),
        };
        Ok((level, content))
    }

    fn create_level(
        &mut self,
        world: Slug,
        level: Level,
        content: LevelContent,
        restore: bool,
    ) -> Result<Applied> {
        let wi = self.find_world(&world)?;
        if self.level(level.slug.as_str()).is_some() {
            return Err(CommandError::DuplicateLevel(level.slug.clone()));
        }
        let level = self.build_level(level, content, None, restore)?;
        let slug = level.slug.clone();
        let levels = &mut self.worlds[wi].levels;
        let i = levels.partition_point(|l| l.slug < slug);
        levels.insert(i, level);
        Ok(Applied {
            inverse: Command::DeleteLevel {
                world: world.clone(),
                level: slug.clone(),
            },
            changes: vec![Change::LevelCreated { world, level: slug }],
        })
    }

    fn delete_level(&mut self, world: Slug, level: Slug) -> Result<Applied> {
        let at = self.find_level_in(&world, &level)?;
        // Split a copy first: it can fail (an undefined value index), and
        // nothing may change on failure.
        let (meta, content) = self.split_level(self.level_at(at).clone())?;
        self.worlds[at.0].levels.remove(at.1);
        Ok(Applied {
            inverse: Command::CreateLevel {
                world: world.clone(),
                level: meta,
                content,
                restore: true,
            },
            changes: vec![Change::LevelDeleted { world, level }],
        })
    }

    fn replace_level(
        &mut self,
        world: Slug,
        level: Level,
        content: LevelContent,
        restore: bool,
    ) -> Result<Applied> {
        let at = self.find_level_in(&world, &level.slug)?;
        let new = self.build_level(level, content, Some(at), restore)?;
        let (meta, old_content) = self.split_level(self.level_at(at).clone())?;
        let old = std::mem::replace(self.level_at_mut(at), new);
        let changes = self.level_changes(&world, &old, self.level_at(at), LevelPart::All);
        Ok(Applied {
            inverse: Command::ReplaceLevel {
                world,
                level: meta,
                content: old_content,
                restore: true,
            },
            changes,
        })
    }

    /// Changes covering everything that differs between two versions of a
    /// level (same slug): metadata, then every layer, entity and note that
    /// is in either one.
    fn level_changes(
        &self,
        world: &Slug,
        old: &Level,
        new: &Level,
        part: LevelPart,
    ) -> Vec<Change> {
        let level = new.slug.clone();
        let mut out = vec![Change::LevelMeta {
            world: world.clone(),
            level: level.clone(),
            part,
        }];
        let mut grids: BTreeMap<&Slug, Rect> = BTreeMap::new();
        let sizes = old
            .int_grids
            .iter()
            .chain(&new.int_grids)
            .map(|g| (&g.layer, g.size()))
            .chain(
                old.overrides
                    .iter()
                    .chain(&new.overrides)
                    .map(|o| (&o.layer, o.size())),
            );
        for (layer, size) in sizes {
            let r = grids.entry(layer).or_insert(Rect::new(0, 0, 0, 0));
            *r = r.union(Rect::of_size(size));
        }
        for (layer, rect) in grids {
            out.push(Change::Grid {
                world: world.clone(),
                level: level.clone(),
                layer: layer.clone(),
                rect,
            });
        }
        let mut tiles: BTreeMap<&Slug, Rect> = BTreeMap::new();
        for (l, t) in [(old, &old.tiles), (new, &new.tiles)] {
            for t in t {
                let size = self
                    .layer(t.layer.as_str())
                    .map_or([0, 0], |d| self.layer_cells(d, l));
                let r = tiles.entry(&t.layer).or_insert(Rect::new(0, 0, 0, 0));
                *r = r.union(Rect::of_size(size));
            }
        }
        for (layer, rect) in tiles {
            out.push(Change::Tiles {
                world: world.clone(),
                level: level.clone(),
                layer: layer.clone(),
                rect,
            });
        }
        // Entity layer entries that appeared, went away or were emptied:
        // `entities.json` changes even when no entity id does.
        let entry = |l: &Level, slug: &Slug| {
            l.entities_layer(slug.as_str())
                .map(|e| e.entities.is_empty())
        };
        let layers: BTreeSet<&Slug> = old
            .entities
            .iter()
            .chain(&new.entities)
            .map(|e| &e.layer)
            .collect();
        for layer in layers {
            if entry(old, layer) != entry(new, layer) {
                out.push(Change::EntityLayer {
                    world: world.clone(),
                    level: level.clone(),
                    layer: layer.clone(),
                });
            }
        }
        let mut ids = BTreeSet::new();
        for e in old.all_entities().chain(new.all_entities()) {
            if ids.insert(&e.id) {
                out.push(Change::Entity {
                    world: world.clone(),
                    level: level.clone(),
                    id: e.id.clone(),
                });
            }
        }
        let mut ids = BTreeSet::new();
        for n in old.notes.iter().chain(&new.notes) {
            if ids.insert(&n.id) {
                out.push(Change::Note {
                    world: world.clone(),
                    level: Some(level.clone()),
                    id: n.id.clone(),
                });
            }
        }
        out
    }

    fn resize_level(
        &mut self,
        world: Slug,
        level: Slug,
        size: [u32; 2],
        anchor: [i8; 2],
    ) -> Result<Applied> {
        let at = self.find_level_in(&world, &level)?;
        if anchor.iter().any(|a| !(-1..=1).contains(a)) {
            return Err(CommandError::Invalid(format!(
                "anchor {anchor:?}: each axis must be -1, 0 or 1"
            )));
        }
        if size[0] == 0 || size[1] == 0 {
            return Err(CommandError::Invalid(format!(
                "size {size:?} must be at least 1x1"
            )));
        }
        let old = self.level_at(at);
        if old.size == size {
            return Ok(noop());
        }
        // Cells (of the project grid) the content moves right and down.
        let shift = resize::resize_shift(old.size, size, anchor);
        let g = self.grid_size as i64;
        let shift_px = [shift[0] * g, shift[1] * g];
        let mut new = old.clone();
        new.size = size;
        new.world_px = [old.world_px[0] - shift_px[0], old.world_px[1] - shift_px[1]];
        // Shift in a layer's own cells; must be whole cells.
        let layer_shift = |def: &LayerDef| -> Result<[i64; 2]> {
            let lg = self.layer_grid(def) as i64;
            if shift_px.iter().any(|p| p % lg != 0) {
                return Err(CommandError::Invalid(format!(
                    "this resize moves layer {} by {shift_px:?} px, not a whole number of its \
                     {lg} px cells; pick another anchor",
                    def.slug
                )));
            }
            Ok([shift_px[0] / lg, shift_px[1] / lg])
        };
        let mut grids = Vec::new();
        for gl in &old.int_grids {
            let def = self.grid_def(&gl.layer)?;
            let ns = self.layer_cells(def, &new);
            let grid = shift_plane(gl, def, layer_shift(def)?, ns);
            if !grid.is_empty() {
                grids.push(grid);
            }
        }
        new.int_grids = grids;
        let mut planes = Vec::new();
        for o in &old.overrides {
            let def = self.grid_def(&o.layer)?;
            let ns = self.layer_cells(def, &new);
            let plane = shift_plane(o, def, layer_shift(def)?, ns);
            if !plane.is_empty() {
                planes.push(plane);
            }
        }
        new.overrides = planes;
        let mut tiles = Vec::new();
        for tl in &old.tiles {
            let def = self.layer_kind(&tl.layer, "tiles", LayerDef::is_tiles)?;
            let [sx, sy] = layer_shift(def)?;
            let ns = self.layer_cells(def, &new);
            let kept: Vec<TilePlacement> = tl
                .tiles
                .iter()
                .filter_map(|t| {
                    let (nx, ny) = (t.cell[0] as i64 + sx, t.cell[1] as i64 + sy);
                    (nx >= 0 && ny >= 0 && nx < ns[0] as i64 && ny < ns[1] as i64).then(|| {
                        let mut t = t.clone();
                        t.cell = [nx as u32, ny as u32];
                        t
                    })
                })
                .collect();
            if !kept.is_empty() {
                tiles.push(TilesLayer {
                    layer: tl.layer.clone(),
                    tiles: kept,
                });
            }
        }
        new.tiles = tiles;
        let mut outside = Vec::new();
        let mut entity_layers = Vec::new();
        for el in &new.entities {
            let def = self.layer_kind(&el.layer, "entities", LayerDef::is_entities)?;
            entity_layers.push((layer_shift(def)?, self.layer_cells(def, &new)));
        }
        for (el, ([sx, sy], ns)) in new.entities.iter_mut().zip(entity_layers) {
            for e in &mut el.entities {
                e.cell = [e.cell[0] + sx as i32, e.cell[1] + sy as i32];
                if e.cell[0] < 0
                    || e.cell[1] < 0
                    || e.cell[0] as i64 >= ns[0] as i64
                    || e.cell[1] as i64 >= ns[1] as i64
                {
                    outside.push(e.id.clone());
                }
            }
        }
        for n in &mut new.notes {
            n.shape.translate(shift_px);
        }
        for r in &mut new.references {
            r.px = [r.px[0] + shift_px[0], r.px[1] + shift_px[1]];
        }
        let (meta, content) = self.split_level(self.level_at(at).clone())?;
        let old = std::mem::replace(self.level_at_mut(at), new);
        let new = self.level_at(at);
        let mut changes = self.level_changes(&world, &old, new, LevelPart::Size);
        if shift != [0, 0] {
            changes.insert(
                1,
                Change::LevelMeta {
                    world: world.clone(),
                    level: level.clone(),
                    part: LevelPart::Position,
                },
            );
        } else {
            // Entities and notes didn't move: only report those that did change.
            changes.retain(|c| !matches!(c, Change::Entity { .. } | Change::Note { .. }));
        }
        changes.extend(outside.into_iter().map(|id| Change::EntityOutside {
            world: world.clone(),
            level: level.clone(),
            id,
        }));
        Ok(Applied {
            inverse: Command::ReplaceLevel {
                world,
                level: meta,
                content,
                restore: true,
            },
            changes,
        })
    }

    fn rename_level(
        &mut self,
        world: Slug,
        level: Slug,
        slug: Slug,
        name: String,
    ) -> Result<Applied> {
        let at = self.find_level_in(&world, &level)?;
        let old_name = self.level_at(at).name.clone();
        if slug == level && name == old_name {
            return Ok(noop());
        }
        let mut changes = Vec::new();
        if slug != level {
            if self.level(slug.as_str()).is_some() {
                return Err(CommandError::DuplicateLevel(slug));
            }
            let (from, to) = (format!("level:{level}"), format!("level:{slug}"));
            // A link that already says `level:<new slug>` would be
            // indistinguishable from a rewritten one, so undo couldn't be exact.
            let all_notes = self
                .worlds
                .iter()
                .flat_map(|w| w.notes.iter().chain(w.levels.iter().flat_map(|l| &l.notes)));
            if let Some(n) = all_notes.clone().find(|n| n.links.contains(&to)) {
                return Err(CommandError::Invalid(format!(
                    "note {} already links to {to}, which doesn't exist; fix that link first",
                    n.id
                )));
            }
            let level_mut = self.level_at_mut(at);
            level_mut.slug = slug.clone();
            changes.push(Change::LevelRenamed {
                world: world.clone(),
                from: level.clone(),
                to: slug.clone(),
            });
            for w in &mut self.worlds {
                let ws = w.slug.clone();
                let scopes = std::iter::once((None, &mut w.notes)).chain(
                    w.levels
                        .iter_mut()
                        .map(|l| (Some(l.slug.clone()), &mut l.notes)),
                );
                for (lvl, notes) in scopes {
                    for n in notes {
                        let mut hit = false;
                        for link in &mut n.links {
                            if *link == from {
                                link.clone_from(&to);
                                hit = true;
                            }
                        }
                        if hit {
                            changes.push(Change::Note {
                                world: ws.clone(),
                                level: lvl.clone(),
                                id: n.id.clone(),
                            });
                        }
                    }
                }
            }
            self.worlds[at.0].levels.sort_by(|a, b| a.slug.cmp(&b.slug));
        }
        if name != old_name {
            let at = self.find_level(&slug)?;
            self.level_at_mut(at).name = name;
            changes.push(Change::LevelMeta {
                world: world.clone(),
                level: slug.clone(),
                part: LevelPart::Name,
            });
        }
        Ok(Applied {
            inverse: Command::RenameLevel {
                world,
                level: slug,
                slug: level,
                name: old_name,
            },
            changes,
        })
    }

    // ---- entities ----

    fn add_entity(
        &mut self,
        level: Slug,
        layer: Slug,
        entity: EntityInstance,
        index: Option<usize>,
        allow_outside: bool,
    ) -> Result<Applied> {
        let def = self.layer_kind(&layer, "entities", LayerDef::is_entities)?;
        let at = self.find_level(&level)?;
        let size = self.layer_cells(def, self.level_at(at));
        if !allow_outside {
            check_cell(&layer, [entity.cell[0] as i64, entity.cell[1] as i64], size)?;
        }
        if self.entity_def(entity.ty.as_str()).is_none() {
            return Err(CommandError::UnknownEntityType(entity.ty.clone()));
        }
        if self.entity_ids().contains(&entity.id) {
            return Err(CommandError::DuplicateId(entity.id.clone()));
        }
        let world = self.worlds[at.0].slug.clone();
        let lvl = self.level_at_mut(at);
        let existing = lvl.entities.iter().position(|e| e.layer == layer);
        let len = existing.map_or(0, |i| lvl.entities[i].entities.len());
        let i = index.unwrap_or(len);
        if i > len {
            return Err(CommandError::Invalid(format!(
                "index {i} is past the end of layer {layer} ({len} entities)"
            )));
        }
        let li = match existing {
            Some(li) => li,
            None => {
                let li = lvl.entities.partition_point(|e| e.layer < layer);
                lvl.entities.insert(
                    li,
                    EntitiesLayer {
                        layer: layer.clone(),
                        entities: Vec::new(),
                    },
                );
                li
            }
        };
        let id = entity.id.clone();
        lvl.entities[li].entities.insert(i, entity);
        Ok(Applied {
            inverse: Command::RemoveEntity {
                id: id.clone(),
                // An empty entry that was already there must survive undo.
                keep_empty: existing.is_some() && len == 0,
            },
            changes: vec![Change::Entity { world, level, id }],
        })
    }

    fn remove_entity(&mut self, id: Id, keep_empty: bool) -> Result<Applied> {
        let (at, li, ei) = self.find_entity(&id)?;
        let world = self.worlds[at.0].slug.clone();
        let layer = self.level_at(at).entities[li].layer.clone();
        let size = self
            .layer(layer.as_str())
            .map(|def| self.layer_cells(def, self.level_at(at)));
        let lvl = self.level_at_mut(at);
        let level = lvl.slug.clone();
        let entity = lvl.entities[li].entities.remove(ei);
        // An entity a resize left outside must still come back on undo.
        let allow_outside = size.is_none_or(|s| !cell_inside(entity.cell, s));
        if lvl.entities[li].entities.is_empty() && !keep_empty {
            lvl.entities.remove(li);
        }
        Ok(Applied {
            inverse: Command::AddEntity {
                level: level.clone(),
                layer,
                entity,
                index: Some(ei),
                allow_outside,
            },
            changes: vec![Change::Entity { world, level, id }],
        })
    }

    // ---- notes ----

    fn add_note(
        &mut self,
        scope: NoteScope,
        note: Annotation,
        index: Option<usize>,
    ) -> Result<Applied> {
        let (world, level) = self.scope_names(&scope)?;
        if self.note_ids().contains(&note.id) {
            return Err(CommandError::DuplicateId(note.id.clone()));
        }
        let notes = self.notes_mut(&scope)?;
        let i = index.unwrap_or(notes.len());
        if i > notes.len() {
            return Err(CommandError::Invalid(format!(
                "index {i} is past the end of the notes of {scope} ({})",
                notes.len()
            )));
        }
        let id = note.id.clone();
        notes.insert(i, note);
        Ok(Applied {
            inverse: Command::RemoveNote { id: id.clone() },
            changes: vec![Change::Note { world, level, id }],
        })
    }

    // ---- guide groups ----

    fn set_guide_group(
        &mut self,
        world: Slug,
        group: GuideGroup,
        index: Option<usize>,
    ) -> Result<Applied> {
        let wi = self.find_world(&world)?;
        let groups = &mut self.worlds[wi].guide_groups;
        let slug = group.slug.clone();
        let inverse = match groups.iter().position(|g| g.slug == slug) {
            Some(i) => {
                let to = index.unwrap_or(i);
                if to >= groups.len() {
                    return Err(CommandError::Invalid(format!(
                        "index {to} is past the end of the guide groups of world {world} ({})",
                        groups.len()
                    )));
                }
                if to == i && groups[i] == group {
                    return Ok(noop());
                }
                let old = groups.remove(i);
                groups.insert(to, group);
                Command::SetGuideGroup {
                    world: world.clone(),
                    group: old,
                    index: Some(i),
                }
            }
            None => {
                let to = index.unwrap_or(groups.len());
                if to > groups.len() {
                    return Err(CommandError::Invalid(format!(
                        "index {to} is past the end of the guide groups of world {world} ({})",
                        groups.len()
                    )));
                }
                groups.insert(to, group);
                Command::RemoveGuideGroup {
                    world: world.clone(),
                    slug: slug.clone(),
                }
            }
        };
        Ok(Applied {
            inverse,
            changes: vec![Change::GuideGroup { world, slug }],
        })
    }

    fn remove_guide_group(&mut self, world: Slug, slug: Slug) -> Result<Applied> {
        let wi = self.find_world(&world)?;
        let w = &self.worlds[wi];
        let i = w
            .guide_groups
            .iter()
            .position(|g| g.slug == slug)
            .ok_or_else(|| CommandError::UnknownGuideGroup {
                world: world.clone(),
                group: slug.clone(),
            })?;
        let users: Vec<&str> = w
            .notes
            .iter()
            .filter(|n| matches!(&n.shape, Shape::Guide { group: Some(g), .. } if *g == slug))
            .map(|n| n.id.as_str())
            .collect();
        if !users.is_empty() {
            let mut list = users.iter().take(3).copied().collect::<Vec<_>>().join(", ");
            if users.len() > 3 {
                list.push_str(", ...");
            }
            let n = users.len();
            return Err(CommandError::Invalid(format!(
                "guide group {slug} is used by {n} guide{} ({list}); move them out first",
                if n == 1 { "" } else { "s" }
            )));
        }
        let old = self.worlds[wi].guide_groups.remove(i);
        Ok(Applied {
            inverse: Command::SetGuideGroup {
                world: world.clone(),
                group: old,
                index: Some(i),
            },
            changes: vec![Change::GuideGroup { world, slug }],
        })
    }

    // ---- project ----

    fn set_setting(&mut self, setting: ProjectSetting) -> Result<Applied> {
        let key = setting.key();
        if let ProjectSetting::GridSize(g) = &setting
            && *g != self.grid_size
        {
            if *g == 0 {
                return Err(CommandError::Invalid("grid_size must be at least 1".into()));
            }
            let n = self.levels().count();
            if n > 0 {
                return Err(CommandError::Invalid(format!(
                    "grid_size can only change while the project has no levels (it has {n}): \
                     their sizes and positions are in cells of it"
                )));
            }
        }
        if let ProjectSetting::DefaultLevelSize(Some([w, h])) = &setting
            && (*w == 0 || *h == 0)
        {
            return Err(CommandError::Invalid(format!(
                "default_level_size must be at least 1x1 cells, not {w}x{h}"
            )));
        }
        let old = match setting {
            ProjectSetting::Name(v) => ProjectSetting::Name(std::mem::replace(&mut self.name, v)),
            ProjectSetting::BgColor(v) => {
                ProjectSetting::BgColor(std::mem::replace(&mut self.bg_color, v))
            }
            ProjectSetting::CollisionLayer(v) => {
                if let Some(l) = &v {
                    self.grid_def(l)?;
                }
                ProjectSetting::CollisionLayer(std::mem::replace(&mut self.collision_layer, v))
            }
            ProjectSetting::Seed(v) => ProjectSetting::Seed(std::mem::replace(&mut self.seed, v)),
            ProjectSetting::GridSize(v) => {
                ProjectSetting::GridSize(std::mem::replace(&mut self.grid_size, v))
            }
            ProjectSetting::BlockingValues(v) => {
                ProjectSetting::BlockingValues(std::mem::replace(&mut self.blocking_values, v))
            }
            ProjectSetting::DefaultLevelSize(v) => {
                ProjectSetting::DefaultLevelSize(std::mem::replace(&mut self.default_level_size, v))
            }
            ProjectSetting::ExportOnSave(v) => {
                ProjectSetting::ExportOnSave(std::mem::replace(&mut self.export_on_save, v))
            }
        };
        let unchanged = match &old {
            ProjectSetting::Name(v) => *v == self.name,
            ProjectSetting::BgColor(v) => *v == self.bg_color,
            ProjectSetting::CollisionLayer(v) => *v == self.collision_layer,
            ProjectSetting::Seed(v) => *v == self.seed,
            ProjectSetting::GridSize(v) => *v == self.grid_size,
            ProjectSetting::BlockingValues(v) => *v == self.blocking_values,
            ProjectSetting::DefaultLevelSize(v) => *v == self.default_level_size,
            ProjectSetting::ExportOnSave(v) => *v == self.export_on_save,
        };
        if unchanged {
            return Ok(noop());
        }
        Ok(Applied {
            inverse: Command::SetProjectSetting { setting: old },
            changes: vec![Change::Project { key }],
        })
    }

    // ---- batch ----

    fn batch(&mut self, label: String, commands: Vec<Command>) -> Result<Applied> {
        let mut inverses = Vec::with_capacity(commands.len());
        let mut changes = Vec::new();
        for (index, c) in commands.into_iter().enumerate() {
            match self.apply_inner(c) {
                Ok(a) => {
                    if !a.inverse.is_noop() || !a.changes.is_empty() {
                        inverses.push(a.inverse);
                    }
                    changes.extend(a.changes);
                }
                Err(source) => {
                    // Undo everything applied so far. If an inverse is
                    // refused, keep going with the rest: restore as much as
                    // possible, then say so loudly.
                    let nested = matches!(source, CommandError::Rollback { .. });
                    let mut failed = Vec::new();
                    let mut touched = changes;
                    for inv in inverses.into_iter().rev() {
                        match self.apply_inner(inv) {
                            Ok(a) => touched.extend(a.changes),
                            Err(e) => failed.push(e),
                        }
                    }
                    let err = CommandError::Batch {
                        label,
                        index,
                        source: Box::new(source),
                    };
                    if failed.is_empty() && !nested {
                        return Err(err);
                    }
                    // The project is not what it was: mark every file the
                    // batch touched dirty so a save writes what is in memory.
                    for c in &touched {
                        for f in c.files() {
                            self.files.mark_dirty(f);
                        }
                    }
                    return Err(CommandError::Rollback {
                        batch: Box::new(err),
                        failed,
                    });
                }
            }
        }
        inverses.reverse();
        Ok(Applied {
            inverse: Command::Batch {
                label,
                commands: inverses,
            },
            changes: coalesce(changes),
        })
    }
}

// ---- helpers ----

fn noop() -> Applied {
    Applied {
        inverse: Command::noop(),
        changes: Vec::new(),
    }
}

/// A `level.json` edit: no-op unless `changed`.
fn meta(changed: bool, inverse: Command, world: Slug, level: Slug, part: LevelPart) -> Applied {
    if !changed {
        return noop();
    }
    Applied {
        inverse,
        changes: vec![Change::LevelMeta { world, level, part }],
    }
}

/// Sort key for cells: row, then column.
fn yx(c: [u32; 2]) -> (u32, u32) {
    (c[1], c[0])
}

/// The stored state code a grid value writes on `def`: 0 empty (no
/// override), `i + 1` a value, [`DerivedOverrides::SUPPRESS_CODE`] for `"!"`
/// on a derived layer. `"!"` on any other layer is refused.
fn value_index(def: &LayerDef, value: &GridValue) -> Result<u16> {
    match value {
        GridValue::Clear => Ok(0),
        GridValue::Value(v) => {
            def.value_index(v.as_str())
                .ok_or_else(|| CommandError::UnknownValue {
                    layer: def.slug.clone(),
                    value: v.clone(),
                })
        }
        GridValue::Suppress if def.is_derived() => Ok(DerivedOverrides::SUPPRESS_CODE),
        GridValue::Suppress => Err(CommandError::Invalid(format!(
            "\"!\" (suppress) only applies to derived layers; {} isn't derived",
            def.slug
        ))),
    }
}

/// The grid value a stored state code stands for (see [`value_index`]).
fn index_value(def: &LayerDef, x: u32, y: u32, index: u16) -> Result<GridValue> {
    if index == 0 {
        return Ok(GridValue::Clear);
    }
    if def.is_derived() && index == DerivedOverrides::SUPPRESS_CODE {
        return Ok(GridValue::Suppress);
    }
    def.values()
        .get(index as usize - 1)
        .map(|v| GridValue::Value(v.slug.clone()))
        .ok_or_else(|| CommandError::UndefinedIndex {
            layer: def.slug.clone(),
            x,
            y,
            index,
        })
}

/// A level's per-cell content of one int_grid layer, as the grid commands
/// see it: state codes (0 empty, `i + 1` a value; a derived layer's
/// overrides add [`DerivedOverrides::SUPPRESS_CODE`]).
pub(super) trait Plane: Sized {
    fn layer(&self) -> &Slug;
    fn size(&self) -> [u32; 2];
    fn code(&self, x: u32, y: u32) -> u16;
    fn set_code(&mut self, x: u32, y: u32, code: u16);
    fn is_empty(&self) -> bool;
    fn empty(def: &LayerDef, size: [u32; 2]) -> Self;
    fn list(l: &Level) -> &Vec<Self>;
    fn list_mut(l: &mut Level) -> &mut Vec<Self>;
}

impl Plane for IntGridLayer {
    fn layer(&self) -> &Slug {
        &self.layer
    }
    fn size(&self) -> [u32; 2] {
        IntGridLayer::size(self)
    }
    fn code(&self, x: u32, y: u32) -> u16 {
        self.index_at(x, y).expect("cell inside the grid")
    }
    fn set_code(&mut self, x: u32, y: u32, code: u16) {
        self.set_index(x, y, code);
    }
    fn is_empty(&self) -> bool {
        IntGridLayer::is_empty(self)
    }
    fn empty(def: &LayerDef, size: [u32; 2]) -> Self {
        IntGridLayer::new_empty(def, size)
    }
    fn list(l: &Level) -> &Vec<Self> {
        &l.int_grids
    }
    fn list_mut(l: &mut Level) -> &mut Vec<Self> {
        &mut l.int_grids
    }
}

impl Plane for DerivedOverrides {
    fn layer(&self) -> &Slug {
        &self.layer
    }
    fn size(&self) -> [u32; 2] {
        DerivedOverrides::size(self)
    }
    fn code(&self, x: u32, y: u32) -> u16 {
        self.code_at(x, y)
    }
    fn set_code(&mut self, x: u32, y: u32, code: u16) {
        DerivedOverrides::set_code(self, x, y, code);
    }
    fn is_empty(&self) -> bool {
        DerivedOverrides::is_empty(self)
    }
    fn empty(def: &LayerDef, size: [u32; 2]) -> Self {
        DerivedOverrides::new_empty(def, size)
    }
    fn list(l: &Level) -> &Vec<Self> {
        &l.overrides
    }
    fn list_mut(l: &mut Level) -> &mut Vec<Self> {
        &mut l.overrides
    }
}

/// A level's plane of `def` as state codes, all 0 when the level has none.
fn plane_cells<P: Plane>(l: &Level, def: &LayerDef, size: [u32; 2]) -> Result<Vec<u16>> {
    match P::list(l).iter().find(|g| *g.layer() == def.slug) {
        Some(g) if g.size() != size => Err(CommandError::GridSize {
            layer: def.slug.clone(),
            actual: g.size(),
            expected: size,
        }),
        Some(g) => {
            let mut out = Vec::with_capacity(size[0] as usize * size[1] as usize);
            for y in 0..size[1] {
                for x in 0..size[0] {
                    out.push(g.code(x, y));
                }
            }
            Ok(out)
        }
        None => Ok(vec![0; size[0] as usize * size[1] as usize]),
    }
}

/// `p` moved by `[sx, sy]` cells onto a `ns` grid (cropped or extended).
fn shift_plane<P: Plane>(p: &P, def: &LayerDef, [sx, sy]: [i64; 2], ns: [u32; 2]) -> P {
    let mut out = P::empty(def, ns);
    let [ow, oh] = p.size();
    for y in 0..oh {
        for x in 0..ow {
            let (nx, ny) = (x as i64 + sx, y as i64 + sy);
            if nx >= 0 && ny >= 0 && nx < ns[0] as i64 && ny < ns[1] as i64 {
                out.set_code(nx as u32, ny as u32, p.code(x, y));
            }
        }
    }
    out
}

fn cell_inside(c: [i32; 2], size: [u32; 2]) -> bool {
    c[0] >= 0 && c[1] >= 0 && (c[0] as i64) < size[0] as i64 && (c[1] as i64) < size[1] as i64
}

fn check_cell(layer: &Slug, c: [i64; 2], size: [u32; 2]) -> Result<()> {
    if c[0] < 0 || c[1] < 0 || c[0] >= size[0] as i64 || c[1] >= size[1] as i64 {
        return Err(CommandError::OutOfBounds {
            layer: layer.clone(),
            x: c[0],
            y: c[1],
            w: size[0],
            h: size[1],
        });
    }
    Ok(())
}

fn check_rect(layer: &Slug, r: Rect, size: [u32; 2]) -> Result<()> {
    if r.is_empty() {
        return Ok(());
    }
    check_cell(layer, [r.x as i64, r.y as i64], size)?;
    check_cell(
        layer,
        [(r.x as i64) + r.w as i64 - 1, (r.y as i64) + r.h as i64 - 1],
        size,
    )
}

/// Writes that set the 4-connected region of `at`'s value to `v`.
fn flood(cells: &[u16], size: [u32; 2], at: [u32; 2], v: u16) -> BTreeMap<(u32, u32), u16> {
    let [w, h] = size;
    let idx = |x: u32, y: u32| (y * w + x) as usize;
    let from = cells[idx(at[0], at[1])];
    let mut out = BTreeMap::new();
    if from == v {
        return out;
    }
    let mut seen = vec![false; cells.len()];
    let mut queue = VecDeque::from([at]);
    seen[idx(at[0], at[1])] = true;
    while let Some([x, y]) = queue.pop_front() {
        out.insert((y, x), v);
        let mut visit = |nx: u32, ny: u32| {
            let i = idx(nx, ny);
            if !seen[i] && cells[i] == from {
                seen[i] = true;
                queue.push_back([nx, ny]);
            }
        };
        if x > 0 {
            visit(x - 1, y);
        }
        if y > 0 {
            visit(x, y - 1);
        }
        if x + 1 < w {
            visit(x + 1, y);
        }
        if y + 1 < h {
            visit(x, y + 1);
        }
    }
    out
}
