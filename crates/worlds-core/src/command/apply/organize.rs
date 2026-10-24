//! Level order, world layouts and object groups: `reorder_levels`,
//! `set_world_layout`, `set_group` and `remove_group`, and the strip
//! relayout the level commands run.
//!
//! The level order is `world.json` `levels` ([`World::level_order`]);
//! [`World::levels`] is always sorted by it. A world without a list keeps
//! having none (old projects save byte for byte) unless it has a strip
//! layout; once there is a list, creating, deleting, renaming and moving
//! levels keep it up to date. Inverses put a list back exactly with a
//! `reorder_levels` with `restore`.
//!
//! In a strip layout (`horizontal`, `vertical`) the positions follow the
//! order: `reorder_levels`, `create_level`, `duplicate_level`,
//! `resize_level`, `delete_level` and `move_level_to_world` end with the
//! `move_level`s that put every level of the world back in its place, in
//! the same [`Applied`] (one undo entry). Their inverses undo those moves
//! and carry `restore`/`keep_layout`, so undo moves nothing else.

use super::{Result, noop};
use crate::command::{Applied, Change, Command, CommandError, LevelPart, coalesce};
use crate::id::{Id, Slug};
use crate::model::{ObjectGroup, Project, WorldLayout};

impl Project {
    /// After a level was added to or removed from world `wi`: rewrite its
    /// level order to the current one when it keeps a list (it has one, or
    /// a strip layout), else sort by slug. The change to report if the list
    /// changed.
    pub(super) fn sync_level_order(&mut self, wi: usize) -> Option<Change> {
        let w = &mut self.worlds[wi];
        let old = w.level_order.clone();
        if !w.level_order.is_empty() || w.layout.is_strip() {
            w.level_order = w.levels.iter().map(|l| l.slug.clone()).collect();
        }
        w.sort_levels();
        (w.level_order != old).then(|| Change::WorldMeta {
            world: w.slug.clone(),
        })
    }

    /// `inverse`, followed by putting world `wi`'s level order back to
    /// `old` when the command changed it.
    pub(super) fn with_level_order_restore(
        &self,
        wi: usize,
        inverse: Command,
        old: Vec<Slug>,
    ) -> Command {
        let w = &self.worlds[wi];
        if w.level_order == old {
            return inverse;
        }
        Command::Batch {
            label: String::new(),
            commands: vec![
                inverse,
                Command::ReorderLevels {
                    world: w.slug.clone(),
                    order: old,
                    restore: true,
                },
            ],
        }
    }

    /// After `applied` changed `world`'s levels: in a strip layout, move
    /// every level that isn't where the strip puts it. The moves join
    /// `applied`'s changes; their inverses run before its inverse. A no-op
    /// stays a no-op, and so does any other layout.
    pub(super) fn relayout(&mut self, world: &Slug, applied: Applied) -> Result<Applied> {
        if applied.changes.is_empty() {
            return Ok(applied);
        }
        let Some(wi) = self.worlds.iter().position(|w| w.slug == *world) else {
            return Ok(applied);
        };
        let w = &self.worlds[wi];
        let Some(places) = w.strip_positions(self.grid_size) else {
            return Ok(applied);
        };
        let moves: Vec<Command> = w
            .levels
            .iter()
            .zip(places)
            .filter(|(l, p)| l.world_px != *p)
            .map(|(l, p)| Command::MoveLevel {
                world: world.clone(),
                level: l.slug.clone(),
                world_px: p,
            })
            .collect();
        if moves.is_empty() {
            return Ok(applied);
        }
        let mut inverses = Vec::with_capacity(moves.len() + 1);
        let mut changes = applied.changes;
        for m in moves {
            let a = self.apply_inner(m)?;
            inverses.push(a.inverse);
            changes.extend(a.changes);
        }
        inverses.reverse();
        inverses.push(applied.inverse);
        Ok(Applied {
            inverse: Command::Batch {
                label: String::new(),
                commands: inverses,
            },
            changes: coalesce(changes),
        })
    }

    pub(super) fn reorder_levels(
        &mut self,
        world: Slug,
        order: Vec<Slug>,
        restore: bool,
    ) -> Result<Applied> {
        let wi = self.find_world(&world)?;
        if !restore && !order.is_empty() {
            for (i, s) in order.iter().enumerate() {
                self.find_level_in(&world, s)?;
                if order[..i].contains(s) {
                    return Err(CommandError::Invalid(format!(
                        "level {s} is listed twice in the order"
                    )));
                }
            }
            if let Some(l) = self.worlds[wi]
                .levels
                .iter()
                .find(|l| !order.contains(&l.slug))
            {
                return Err(CommandError::Invalid(format!(
                    "the order leaves out level {}; list every level of world {world} once",
                    l.slug
                )));
            }
        }
        let w = &mut self.worlds[wi];
        if order == w.level_order {
            return Ok(noop());
        }
        let old = std::mem::replace(&mut w.level_order, order);
        w.sort_levels();
        let applied = Applied {
            inverse: Command::ReorderLevels {
                world: world.clone(),
                order: old,
                restore: true,
            },
            changes: vec![Change::WorldMeta {
                world: world.clone(),
            }],
        };
        if restore {
            Ok(applied)
        } else {
            self.relayout(&world, applied)
        }
    }

    pub(super) fn set_world_layout(
        &mut self,
        world: Slug,
        layout: WorldLayout,
        restore: bool,
    ) -> Result<Applied> {
        let wi = self.find_world(&world)?;
        if let Some(c) = layout.cell
            && (c[0] == 0 || c[1] == 0)
        {
            return Err(CommandError::Invalid(format!(
                "layout cell {c:?} must be at least 1x1 px"
            )));
        }
        let w = &mut self.worlds[wi];
        if w.layout == layout {
            return Ok(noop());
        }
        let old = std::mem::replace(&mut w.layout, layout);
        let old_order = w.level_order.clone();
        let mut changes = vec![Change::WorldMeta {
            world: world.clone(),
        }];
        if !restore {
            // A strip follows the level order; write it down so renames
            // don't reorder the strip.
            changes.extend(self.sync_level_order(wi));
        }
        let inverse = self.with_level_order_restore(
            wi,
            Command::SetWorldLayout {
                world: world.clone(),
                layout: old,
                restore: true,
            },
            old_order,
        );
        let applied = Applied {
            inverse,
            changes: coalesce(changes),
        };
        if restore {
            Ok(applied)
        } else {
            self.relayout(&world, applied)
        }
    }

    pub(super) fn set_group(
        &mut self,
        level: Slug,
        group: ObjectGroup,
        index: Option<usize>,
    ) -> Result<Applied> {
        let at = self.find_level(&level)?;
        let world = self.worlds[at.0].slug.clone();
        let groups = &mut self.level_at_mut(at).groups;
        let inverse = match groups.iter().position(|g| g.id == group.id) {
            Some(i) => {
                let to = index.unwrap_or(i);
                if to >= groups.len() {
                    return Err(CommandError::Invalid(format!(
                        "index {to} is past the end of the groups of level {level} ({})",
                        groups.len()
                    )));
                }
                if to == i && groups[i] == group {
                    return Ok(noop());
                }
                let old = groups.remove(i);
                groups.insert(to, group);
                Command::SetGroup {
                    level: level.clone(),
                    group: old,
                    index: Some(i),
                }
            }
            None => {
                let to = index.unwrap_or(groups.len());
                if to > groups.len() {
                    return Err(CommandError::Invalid(format!(
                        "index {to} is past the end of the groups of level {level} ({})",
                        groups.len()
                    )));
                }
                let id = group.id.clone();
                groups.insert(to, group);
                Command::RemoveGroup {
                    level: level.clone(),
                    id,
                }
            }
        };
        Ok(Applied {
            inverse,
            changes: vec![Change::LevelMeta {
                world,
                level,
                part: LevelPart::Groups,
            }],
        })
    }

    pub(super) fn remove_group(&mut self, level: Slug, id: Id) -> Result<Applied> {
        let at = self.find_level(&level)?;
        let world = self.worlds[at.0].slug.clone();
        let groups = &mut self.level_at_mut(at).groups;
        let Some(i) = groups.iter().position(|g| g.id == id) else {
            return Err(CommandError::Invalid(format!(
                "level {level} has no group {id:?}"
            )));
        };
        let old = groups.remove(i);
        Ok(Applied {
            inverse: Command::SetGroup {
                level: level.clone(),
                group: old,
                index: Some(i),
            },
            changes: vec![Change::LevelMeta {
                world,
                level,
                part: LevelPart::Groups,
            }],
        })
    }
}
