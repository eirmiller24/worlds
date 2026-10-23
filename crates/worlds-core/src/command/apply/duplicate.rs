//! `duplicate_level`: a copy of a level under a new slug, applied as the
//! `create_level` it builds, so its inverse is `delete_level`.
//!
//! The copy's ids are derived from the copy's level id and the original
//! ids ([`Id::derived`]), so the same duplicate of the same project makes
//! the same ids (the API can report them before an approval-mode batch is
//! applied, and redo is exact anyway: it is the inverse's `create_level`).

use std::collections::{BTreeMap, HashSet};

use super::Result;
use crate::command::{Applied, CommandError};
use crate::fragment::{Outside, derived_free, remap_fields, remap_links};
use crate::id::{Id, Slug};
use crate::model::Project;

impl Project {
    pub(super) fn duplicate_level(
        &mut self,
        level: Slug,
        slug: Slug,
        name: Option<String>,
        world: Option<Slug>,
        world_px: Option<[i64; 2]>,
    ) -> Result<Applied> {
        let at = self.find_level(&level)?;
        let world = match world {
            Some(w) => {
                self.find_world(&w)?;
                w
            }
            None => self.worlds[at.0].slug.clone(),
        };
        if self.level(slug.as_str()).is_some() {
            return Err(CommandError::DuplicateLevel(slug));
        }
        let src = self.level_at(at).clone();
        let (mut meta, mut content) = self.split_level(src.clone())?;

        let mut level_ids: HashSet<Id> = self.levels().map(|l| l.id.clone()).collect();
        meta.id = derived_free(
            &format!("duplicate_level:{}:{slug}", src.id),
            &mut level_ids,
        );
        let new_level_id = meta.id.clone();
        meta.slug = slug.clone();
        meta.name = name.unwrap_or_else(|| {
            if src.name.is_empty() {
                String::new()
            } else {
                format!("{} copy", src.name)
            }
        });
        meta.world_px = match world_px {
            Some(p) => p,
            None => self.right_of(&world, &src),
        };

        // New ids for the entities and notes, and every reference between
        // them rewritten.
        let mut taken: HashSet<Id> = self
            .levels()
            .flat_map(|l| l.all_entities().map(|e| e.id.clone()))
            .collect();
        let mut entity_map = BTreeMap::new();
        for e in content.entities.iter().flat_map(|l| &l.entities) {
            let id = derived_free(&format!("{new_level_id}:{}", e.id), &mut taken);
            entity_map.insert(e.id.clone(), id);
        }
        let mut taken: HashSet<Id> = self.notes().map(|n| n.note.id.clone()).collect();
        let mut note_map = BTreeMap::new();
        for n in &content.notes {
            let id = derived_free(&format!("{new_level_id}:{}", n.id), &mut taken);
            note_map.insert(n.id.clone(), id);
        }
        let mut problems = Vec::new();
        for el in &mut content.entities {
            for e in &mut el.entities {
                e.id = entity_map[&e.id].clone();
                remap_fields(
                    self.entity_def(e.ty.as_str()),
                    &[],
                    &mut e.fields,
                    &entity_map,
                    &Outside::Keep,
                    [0, 0],
                    "",
                    &mut problems,
                );
            }
        }
        for n in &mut content.notes {
            n.id = note_map[&n.id].clone();
            remap_links(
                &mut n.links,
                &entity_map,
                &note_map,
                &Outside::Keep,
                "",
                &mut problems,
            );
        }
        let level_fields = self.level_fields.clone();
        remap_fields(
            None,
            &level_fields,
            &mut meta.fields,
            &entity_map,
            &Outside::Keep,
            [0, 0],
            "",
            &mut problems,
        );
        self.create_level(world, meta, content, false)
    }

    /// Where a copy of `src` goes in `world` by default: at `src`'s top,
    /// starting at its right edge, moved right past every level of `world`
    /// it would overlap.
    fn right_of(&self, world: &Slug, src: &crate::model::Level) -> [i64; 2] {
        let g = self.grid_size as i64;
        let w = src.size[0] as i64 * g;
        let h = src.size[1] as i64 * g;
        let mut x = src.world_px[0] + w;
        let y = src.world_px[1];
        let Some(wd) = self.world(world.as_str()) else {
            return [x, y];
        };
        for _ in 0..=wd.levels.len() {
            let hit = wd.levels.iter().find(|l| {
                let (lx, ly) = (l.world_px[0], l.world_px[1]);
                let (lw, lh) = (l.size[0] as i64 * g, l.size[1] as i64 * g);
                x < lx + lw && lx < x + w && y < ly + lh && ly < y + h
            });
            match hit {
                Some(l) => x = l.world_px[0] + l.size[0] as i64 * g,
                None => break,
            }
        }
        [x, y]
    }
}
