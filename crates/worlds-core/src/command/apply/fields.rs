//! Field definition commands: `set_entity_field_def`,
//! `remove_entity_field_def`, `rename_entity_field`, and the same three for
//! the project's `level_fields`.
//!
//! They follow the definition commands' rules (see [`super::defs`]). A set
//! creates or replaces one field (and moves it with `index`) and never
//! touches stored values: retyping a field leaves them for validation to
//! report. A remove takes the stored values with it, so its inverse puts
//! them back. A rename rewrites every stored key (and, for level fields,
//! every rule group's `requires_level_field`), so it is refused while
//! something already stores a value under the new slug: undo couldn't tell
//! it from a renamed one. Fields are found by slug, so while two fields of
//! an owner share the slug a command names, it is refused: which one is
//! meant would be a guess.

use super::defs::{ListEdit, list_set, slug_mismatch};
use super::{Result, noop};
use crate::command::{Applied, Change, Command, CommandError, DefKind, LevelPart, ProjectKey};
use crate::id::{Id, Slug};
use crate::model::{FieldDef, Project};

/// Whose field list a command edits.
#[derive(Clone, Copy)]
pub(super) enum Owner<'a> {
    /// An entity type's `fields`.
    Entity(&'a Slug),
    /// `project.json` `level_fields`.
    Level,
}

impl Owner<'_> {
    /// As in [`CommandError::UnknownField`]: `entity type "door"`, `project
    /// level_fields`.
    fn describe(self) -> String {
        match self {
            Owner::Entity(e) => format!("entity type {e:?}"),
            Owner::Level => "project level_fields".into(),
        }
    }

    /// The change for the file holding the definitions.
    fn change(self) -> Change {
        match self {
            Owner::Entity(e) => Change::Def {
                kind: DefKind::Entity,
                slug: e.clone(),
            },
            Owner::Level => Change::Project {
                key: ProjectKey::LevelFields,
            },
        }
    }
}

/// Where a stored value of a field is: an entity instance or a level.
enum Holder {
    Entity { id: Id },
    Level { world: Slug, level: Slug },
}

impl Holder {
    fn describe(&self) -> String {
        match self {
            Holder::Entity { id } => format!("entity {id}"),
            Holder::Level { level, .. } => format!("level {level}"),
        }
    }

    /// The command that removes this holder's stored value of `field`.
    fn unset(self, field: &Slug) -> Command {
        match self {
            Holder::Entity { id } => Command::SetEntityField {
                id,
                field: field.clone(),
                value: None,
                allow_unknown: false,
            },
            Holder::Level { world, level } => Command::SetLevelField {
                world,
                level,
                field: field.clone(),
                value: None,
                allow_unknown: false,
            },
        }
    }
}

/// Refuse while several fields of the list are slugged `field`.
fn unique_field(list: &[FieldDef], field: &Slug, owner: Owner) -> Result<()> {
    let n = list.iter().filter(|f| f.slug == *field).count();
    if n > 1 {
        return Err(CommandError::Invalid(format!(
            "{n} fields of {} are slugged {field:?}; which one is meant would be a guess, so the \
             file must be fixed by hand first (validation reports it)",
            owner.describe()
        )));
    }
    Ok(())
}

impl Project {
    /// The field list of `owner`: an entity type's `fields` (refused for an
    /// unknown type) or the project's `level_fields`.
    pub(super) fn field_list_mut(&mut self, owner: Owner) -> Result<&mut Vec<FieldDef>> {
        match owner {
            Owner::Entity(e) => self
                .entities
                .iter_mut()
                .find(|d| d.slug == *e)
                .map(|d| &mut d.fields)
                .ok_or_else(|| CommandError::UnknownEntityType(e.clone())),
            Owner::Level => Ok(&mut self.level_fields),
        }
    }

    /// How many instances of entity type `ty` store a value under key
    /// `field` (what removing or retyping that field would touch).
    pub fn entity_field_holders(&self, ty: &Slug, field: &Slug) -> usize {
        self.field_holders(Owner::Entity(ty), field).len()
    }

    /// How many levels store a value under level field key `field`.
    pub fn level_field_holders(&self, field: &Slug) -> usize {
        self.field_holders(Owner::Level, field).len()
    }

    /// Every entity of the owner's type, or every level, that stores a value
    /// under key `field`, in file order.
    fn field_holders(&self, owner: Owner, field: &Slug) -> Vec<Holder> {
        let mut out = Vec::new();
        for w in &self.worlds {
            for l in &w.levels {
                match owner {
                    Owner::Entity(ty) => {
                        for e in l.all_entities() {
                            if e.ty == *ty && e.fields.contains_key(field) {
                                out.push(Holder::Entity { id: e.id.clone() });
                            }
                        }
                    }
                    Owner::Level => {
                        if l.fields.contains_key(field) {
                            out.push(Holder::Level {
                                world: w.slug.clone(),
                                level: l.slug.clone(),
                            });
                        }
                    }
                }
            }
        }
        out
    }

    /// The first entity holder whose id another instance in the project
    /// also has.
    fn shared_holder_id<'h>(&self, holders: &'h [Holder]) -> Option<&'h Id> {
        let ids: Vec<&Id> = holders
            .iter()
            .filter_map(|h| match h {
                Holder::Entity { id } => Some(id),
                Holder::Level { .. } => None,
            })
            .collect();
        if ids.is_empty() {
            return None;
        }
        let mut seen = std::collections::BTreeMap::<&Id, usize>::new();
        for e in self.levels().flat_map(|l| l.all_entities()) {
            *seen.entry(&e.id).or_default() += 1;
        }
        ids.into_iter()
            .find(|id| seen.get(id).is_some_and(|&n| n > 1))
    }

    /// The command that sets a field definition of `owner`.
    fn set_field_command(
        owner: Owner,
        field: Slug,
        def: FieldDef,
        index: Option<usize>,
    ) -> Command {
        match owner {
            Owner::Entity(e) => Command::SetEntityFieldDef {
                entity: e.clone(),
                field,
                def,
                index,
            },
            Owner::Level => Command::SetLevelFieldDef { field, def, index },
        }
    }

    /// The command that removes a field definition of `owner`.
    fn remove_field_command(owner: Owner, field: Slug, keep_values: bool) -> Command {
        match owner {
            Owner::Entity(e) => Command::RemoveEntityFieldDef {
                entity: e.clone(),
                field,
                keep_values,
            },
            Owner::Level => Command::RemoveLevelFieldDef { field, keep_values },
        }
    }

    pub(super) fn set_field_def(
        &mut self,
        owner: Owner,
        field: Slug,
        def: FieldDef,
        index: Option<usize>,
    ) -> Result<Applied> {
        if def.slug != field {
            let op = match owner {
                Owner::Entity(_) => "set_entity_field_def",
                Owner::Level => "set_level_field_def",
            };
            return Err(slug_mismatch(op, &def.slug, &field));
        }
        // Values already stored under the slug (unknown keys until now) stay
        // when the creation is undone.
        let keep_values = !self.field_holders(owner, &field).is_empty();
        let what = match owner {
            Owner::Entity(e) => format!("fields of entities/{e}.json"),
            Owner::Level => "project.json level_fields".into(),
        };
        let list = self.field_list_mut(owner)?;
        unique_field(list, &field, owner)?;
        let pos = list.iter().position(|f| f.slug == field);
        let inverse = match list_set(list, pos, def, index, &what)? {
            ListEdit::Noop => return Ok(noop()),
            ListEdit::Created => Self::remove_field_command(owner, field, keep_values),
            ListEdit::Replaced { old, moved_from } => {
                Self::set_field_command(owner, field, old, moved_from)
            }
        };
        Ok(Applied {
            inverse,
            changes: vec![owner.change()],
        })
    }

    pub(super) fn remove_field_def(
        &mut self,
        owner: Owner,
        field: Slug,
        keep_values: bool,
    ) -> Result<Applied> {
        let list = self.field_list_mut(owner)?;
        unique_field(list, &field, owner)?;
        let Some(i) = list.iter().position(|f| f.slug == field) else {
            return Err(CommandError::UnknownField {
                owner: owner.describe(),
                field,
            });
        };
        let holders = self.field_holders(owner, &field);
        if !holders.is_empty() && !keep_values {
            // Each value is taken out by entity id, which finds the first
            // instance with that id: while a holder's id is shared, the
            // unset (and its undo) could hit another instance.
            if let Some(id) = self.shared_holder_id(&holders) {
                return Err(CommandError::Invalid(format!(
                    "several entities have id {id:?}, and one of them stores a value for \
                     {field:?}; which instance an unset means would be a guess, so give them \
                     unique ids first (validation reports it), or remove the field with \
                     keep_values"
                )));
            }
            // Take the values out one by one, then the (now unused) field:
            // the inverse is a batch that puts the field back, then each
            // value.
            let mut commands: Vec<Command> = holders.into_iter().map(|h| h.unset(&field)).collect();
            let label = Self::remove_field_command(owner, field.clone(), false).label();
            commands.push(Self::remove_field_command(owner, field, true));
            return self.batch(label, commands);
        }
        let def = self.field_list_mut(owner)?.remove(i);
        Ok(Applied {
            inverse: Self::set_field_command(owner, field, def, Some(i)),
            changes: vec![owner.change()],
        })
    }

    pub(super) fn rename_field(&mut self, owner: Owner, from: Slug, to: Slug) -> Result<Applied> {
        let list = self.field_list_mut(owner)?;
        unique_field(list, &from, owner)?;
        let Some(i) = list.iter().position(|f| f.slug == from) else {
            return Err(CommandError::UnknownField {
                owner: owner.describe(),
                field: from,
            });
        };
        if from == to {
            return Ok(noop());
        }
        if list.iter().any(|f| f.slug == to) {
            return Err(CommandError::DuplicateField {
                owner: owner.describe(),
                field: to,
            });
        }
        // Refuse while something already stores or names `to` (see the
        // module docs).
        let pinned = match self.field_holders(owner, &to).first() {
            Some(h) => Some(format!(
                "{} already stores a value under {to:?}",
                h.describe()
            )),
            None if matches!(owner, Owner::Level) => self
                .requirement_on(&to)
                .map(|g| format!("{g} already requires level field {to:?}")),
            None => None,
        };
        if let Some(what) = pinned {
            return Err(CommandError::Invalid(format!(
                "{what}, which isn't a field of {}; fix that first (undo couldn't tell it from \
                 a renamed field)",
                owner.describe()
            )));
        }
        self.field_list_mut(owner)?[i].slug = to.clone();
        let mut changes = vec![owner.change()];
        let rekey = |fields: &mut std::collections::BTreeMap<Slug, serde_json::Value>| {
            let v = fields.remove(&from);
            v.map(|v| fields.insert(to.clone(), v)).is_some()
        };
        // In file order, as `field_holders` lists them.
        for w in &mut self.worlds {
            for l in &mut w.levels {
                match owner {
                    Owner::Entity(ty) => {
                        for e in l.entities.iter_mut().flat_map(|x| &mut x.entities) {
                            if e.ty == *ty && rekey(&mut e.fields) {
                                changes.push(Change::Entity {
                                    world: w.slug.clone(),
                                    level: l.slug.clone(),
                                    id: e.id.clone(),
                                });
                            }
                        }
                    }
                    Owner::Level => {
                        if rekey(&mut l.fields) {
                            changes.push(Change::LevelMeta {
                                world: w.slug.clone(),
                                level: l.slug.clone(),
                                part: LevelPart::Fields,
                            });
                        }
                    }
                }
            }
        }
        if matches!(owner, Owner::Level) {
            for set in &mut self.rules {
                let mut hit = false;
                for g in &mut set.groups {
                    if let Some(req) = &mut g.requires_level_field
                        && req.field == from
                    {
                        req.field = to.clone();
                        hit = true;
                    }
                }
                if hit {
                    changes.push(Change::Def {
                        kind: DefKind::Rules,
                        slug: set.layer.clone(),
                    });
                }
            }
        }
        let inverse = match owner {
            Owner::Entity(e) => Command::RenameEntityField {
                entity: e.clone(),
                from: to,
                to: from,
            },
            Owner::Level => Command::RenameLevelField { from: to, to: from },
        };
        Ok(Applied { inverse, changes })
    }

    /// The first rule group whose `requires_level_field` names `field`.
    fn requirement_on(&self, field: &Slug) -> Option<String> {
        self.rules.iter().find_map(|set| {
            set.groups
                .iter()
                .find(|g| {
                    g.requires_level_field
                        .as_ref()
                        .is_some_and(|r| r.field == *field)
                })
                .map(|g| format!("rule group {} in rules/{}.json", g.slug, set.layer))
        })
    }
}
