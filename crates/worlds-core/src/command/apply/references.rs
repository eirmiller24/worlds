//! Reference image commands: `add_reference`, `update_reference`,
//! `remove_reference`. A reference lives in a world's `world.json` or a
//! level's `level.json` (`references`, bottom first); ids are unique across
//! the project's references.

use super::{Result, noop};
use crate::command::{Applied, Change, Command, CommandError, NoteScope};
use crate::id::{Id, Slug};
use crate::model::{Project, Reference};

impl Project {
    /// Where reference `id` is: its scope and index.
    fn reference_position(&self, id: &Id) -> Result<(NoteScope, usize)> {
        for w in &self.worlds {
            if let Some(i) = w.references.iter().position(|r| r.id == *id) {
                return Ok((NoteScope::World(w.slug.clone()), i));
            }
            for l in &w.levels {
                if let Some(i) = l.references.iter().position(|r| r.id == *id) {
                    return Ok((NoteScope::Level(l.slug.clone()), i));
                }
            }
        }
        Err(CommandError::UnknownReference(id.clone()))
    }

    fn references_mut(&mut self, scope: &NoteScope) -> Result<(Slug, &mut Vec<Reference>)> {
        match scope {
            NoteScope::World(w) => {
                let wi = self.find_world(w)?;
                let world = &mut self.worlds[wi];
                Ok((world.slug.clone(), &mut world.references))
            }
            NoteScope::Level(l) => {
                let (wi, li) = self.find_level(l)?;
                let world = &mut self.worlds[wi];
                Ok((world.slug.clone(), &mut world.levels[li].references))
            }
        }
    }

    pub(super) fn add_reference(
        &mut self,
        scope: NoteScope,
        reference: Reference,
        index: Option<usize>,
    ) -> Result<Applied> {
        if self.reference_position(&reference.id).is_ok() {
            return Err(CommandError::DuplicateId(reference.id));
        }
        let (world, list) = self.references_mut(&scope)?;
        let i = index.unwrap_or(list.len());
        if i > list.len() {
            return Err(CommandError::Invalid(format!(
                "index {i} is past the end of the reference images of {scope} ({})",
                list.len()
            )));
        }
        let id = reference.id.clone();
        list.insert(i, reference);
        Ok(Applied {
            inverse: Command::RemoveReference { id: id.clone() },
            changes: vec![Change::Reference {
                world,
                level: scope_level(&scope),
                id,
            }],
        })
    }

    pub(super) fn update_reference(&mut self, id: Id, reference: Reference) -> Result<Applied> {
        if reference.id != id {
            return Err(CommandError::Invalid(format!(
                "update_reference: reference.id {:?} is not {id:?}",
                reference.id
            )));
        }
        let (scope, i) = self.reference_position(&id)?;
        let (world, list) = self.references_mut(&scope)?;
        if list[i] == reference {
            return Ok(noop());
        }
        let old = std::mem::replace(&mut list[i], reference);
        Ok(Applied {
            inverse: Command::UpdateReference {
                id: id.clone(),
                reference: old,
            },
            changes: vec![Change::Reference {
                world,
                level: scope_level(&scope),
                id,
            }],
        })
    }

    pub(super) fn remove_reference(&mut self, id: Id) -> Result<Applied> {
        let (scope, i) = self.reference_position(&id)?;
        let (world, list) = self.references_mut(&scope)?;
        let reference = list.remove(i);
        let level = scope_level(&scope);
        Ok(Applied {
            inverse: Command::AddReference {
                scope,
                reference,
                index: Some(i),
            },
            changes: vec![Change::Reference { world, level, id }],
        })
    }
}

fn scope_level(scope: &NoteScope) -> Option<Slug> {
    match scope {
        NoteScope::World(_) => None,
        NoteScope::Level(l) => Some(l.clone()),
    }
}
