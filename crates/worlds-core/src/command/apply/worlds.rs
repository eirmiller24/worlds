//! World commands: `create_world`, `rename_world`, `remove_world`,
//! `reorder_worlds` and `move_level_to_world`.
//!
//! The display order of the worlds is `project.json` `worlds`
//! ([`Project::world_order`]); [`Project::worlds`] is always sorted by it.
//! A project with one world and no list keeps having none (old projects
//! save byte for byte); as soon as a command leaves more than one world, or
//! the list is already there, creating or removing a world rewrites it to
//! the display order. Inverses put the list back exactly with a
//! `reorder_worlds` with `restore`.

use super::{Result, noop};
use crate::command::{Applied, Change, Command, CommandError, LevelPart, NoteScope, ProjectKey};
use crate::id::Slug;
use crate::model::{Project, World};

impl Project {
    /// After a world was added or removed: list the display order in
    /// `project.json` when there is more than one world or there was a list
    /// already. The change to report if the list changed.
    fn sync_world_order(&mut self, old: &[Slug]) -> Option<Change> {
        if !self.world_order.is_empty() || self.worlds.len() > 1 {
            self.world_order = self.worlds.iter().map(|w| w.slug.clone()).collect();
        }
        (self.world_order != old).then_some(Change::Project {
            key: ProjectKey::Worlds,
        })
    }

    /// `inverse`, followed by putting the world list back to `old` when the
    /// command changed it.
    fn with_order_restore(&self, inverse: Command, old: Vec<Slug>) -> Command {
        if self.world_order == old {
            return inverse;
        }
        Command::Batch {
            label: String::new(),
            commands: vec![
                inverse,
                Command::ReorderWorlds {
                    order: old,
                    restore: true,
                },
            ],
        }
    }

    pub(super) fn create_world(&mut self, slug: Slug, name: Option<String>) -> Result<Applied> {
        if self.world(slug.as_str()).is_some() {
            return Err(CommandError::DuplicateWorld(slug));
        }
        let old_order = self.world_order.clone();
        let mut world = World::new(slug.clone());
        world.name = name.filter(|n| !n.is_empty());
        // Last in the display order: after the listed worlds and after the
        // unlisted ones too, which the list then names.
        self.worlds.push(world);
        let mut changes = vec![Change::WorldCreated {
            world: slug.clone(),
        }];
        changes.extend(self.sync_world_order(&old_order));
        self.sort_worlds();
        let inverse = self.with_order_restore(
            Command::RemoveWorld {
                world: slug,
                confirm: false,
            },
            old_order,
        );
        Ok(Applied { inverse, changes })
    }

    pub(super) fn rename_world(
        &mut self,
        world: Slug,
        slug: Option<Slug>,
        name: Option<String>,
    ) -> Result<Applied> {
        let wi = self.find_world(&world)?;
        let to = slug.unwrap_or_else(|| world.clone());
        let old_name = self.worlds[wi].name.clone();
        let new_name = match name {
            None => old_name.clone(),
            Some(n) if n.is_empty() => None,
            Some(n) => Some(n),
        };
        if to == world && new_name == old_name {
            return Ok(noop());
        }
        let mut changes = Vec::new();
        if to != world {
            if self.world(to.as_str()).is_some() {
                return Err(CommandError::DuplicateWorld(to));
            }
            self.worlds[wi].slug = to.clone();
            let mut listed = false;
            for s in &mut self.world_order {
                if *s == world {
                    s.clone_from(&to);
                    listed = true;
                }
            }
            self.sort_worlds();
            changes.push(Change::WorldRenamed {
                from: world.clone(),
                to: to.clone(),
            });
            if listed {
                changes.push(Change::Project {
                    key: ProjectKey::Worlds,
                });
            }
        }
        let renamed_name = new_name != old_name;
        if renamed_name {
            let wi = self.find_world(&to)?;
            self.worlds[wi].name = new_name;
            changes.push(Change::WorldMeta { world: to.clone() });
        }
        let inverse = Command::RenameWorld {
            slug: (to != world).then(|| world.clone()),
            world: to,
            name: renamed_name.then(|| old_name.unwrap_or_default()),
        };
        Ok(Applied { inverse, changes })
    }

    pub(super) fn remove_world(&mut self, world: Slug, confirm: bool) -> Result<Applied> {
        let wi = self.find_world(&world)?;
        if self.worlds.len() == 1 {
            return Err(CommandError::Invalid(format!(
                "world {world} is the only world; a project keeps at least one"
            )));
        }
        let w = &self.worlds[wi];
        let (levels, notes, groups) = (w.levels.len(), w.notes.len(), w.guide_groups.len());
        if !confirm && levels + notes + groups > 0 {
            return Err(CommandError::Invalid(format!(
                "world {world} isn't empty ({levels} level(s), {notes} world note(s), {groups} \
                 guide group(s)); set confirm to remove it with everything in it"
            )));
        }
        // Build the inverse first: splitting a level can fail, and nothing
        // may change on failure.
        let mut undo = vec![Command::CreateWorld {
            slug: world.clone(),
            name: w.name.clone(),
        }];
        for g in &w.guide_groups {
            undo.push(Command::SetGuideGroup {
                world: world.clone(),
                group: g.clone(),
                index: None,
            });
        }
        for n in &w.notes {
            undo.push(Command::AddNote {
                scope: NoteScope::World(world.clone()),
                note: n.clone(),
                index: None,
            });
        }
        let mut changes = Vec::new();
        for l in &w.levels {
            let (meta, content) = self.split_level(l.clone())?;
            undo.push(Command::CreateLevel {
                world: world.clone(),
                level: meta,
                content,
                restore: true,
            });
            changes.push(Change::LevelDeleted {
                world: world.clone(),
                level: l.slug.clone(),
            });
        }
        for n in &w.notes {
            changes.push(Change::Note {
                world: world.clone(),
                level: None,
                id: n.id.clone(),
            });
        }
        let old_order = self.world_order.clone();
        undo.push(Command::ReorderWorlds {
            order: old_order.clone(),
            restore: true,
        });
        self.worlds.remove(wi);
        changes.push(Change::WorldRemoved {
            world: world.clone(),
        });
        changes.extend(self.sync_world_order(&old_order));
        Ok(Applied {
            inverse: Command::Batch {
                label: String::new(),
                commands: undo,
            },
            changes,
        })
    }

    pub(super) fn reorder_worlds(&mut self, order: Vec<Slug>, restore: bool) -> Result<Applied> {
        if !restore && !order.is_empty() {
            for (i, s) in order.iter().enumerate() {
                self.find_world(s)?;
                if order[..i].contains(s) {
                    return Err(CommandError::Invalid(format!(
                        "world {s} is listed twice in the order"
                    )));
                }
            }
            if let Some(w) = self.worlds.iter().find(|w| !order.contains(&w.slug)) {
                return Err(CommandError::Invalid(format!(
                    "the order leaves out world {}; list every world once",
                    w.slug
                )));
            }
        }
        if order == self.world_order {
            return Ok(noop());
        }
        let old = std::mem::replace(&mut self.world_order, order);
        self.sort_worlds();
        Ok(Applied {
            inverse: Command::ReorderWorlds {
                order: old,
                restore: true,
            },
            changes: vec![Change::Project {
                key: ProjectKey::Worlds,
            }],
        })
    }

    pub(super) fn move_level_to_world(
        &mut self,
        level: Slug,
        world: Slug,
        world_px: Option<[i64; 2]>,
    ) -> Result<Applied> {
        let (from, li) = self.find_level(&level)?;
        let to = self.find_world(&world)?;
        let old_px = self.worlds[from].levels[li].world_px;
        let px = world_px.unwrap_or(old_px);
        let from_slug = self.worlds[from].slug.clone();
        let inverse = Command::MoveLevelToWorld {
            level: level.clone(),
            world: from_slug.clone(),
            world_px: Some(old_px),
        };
        if from == to {
            if px == old_px {
                return Ok(noop());
            }
            self.worlds[from].levels[li].world_px = px;
            return Ok(Applied {
                inverse,
                changes: vec![Change::LevelMeta {
                    world,
                    level,
                    part: LevelPart::Position,
                }],
            });
        }
        let mut moved = self.worlds[from].levels.remove(li);
        moved.world_px = px;
        let levels = &mut self.worlds[to].levels;
        let i = levels.partition_point(|l| l.slug < level);
        levels.insert(i, moved);
        Ok(Applied {
            inverse,
            changes: vec![
                Change::LevelDeleted {
                    world: from_slug,
                    level: level.clone(),
                },
                Change::LevelCreated { world, level },
            ],
        })
    }
}
