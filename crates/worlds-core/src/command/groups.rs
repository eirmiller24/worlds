//! Object group edits built from `set_group` and `remove_group`: grouping a
//! selection, ungrouping, and dropping deleted objects from their groups.
//! The editor, the live API and the MCP server all call these, so they
//! build the same commands.

use std::collections::BTreeSet;

use super::{Command, CommandError};
use crate::id::{Id, Slug};
use crate::model::{ObjectGroup, Project};

type Result<T> = std::result::Result<T, CommandError>;

/// The `set_group`s (and `remove_group`s, for a group left without
/// members) that take `ids` out of every group that lists them. Add them to
/// the batch that deletes those objects, so undo puts the membership back
/// with the objects.
pub fn drop_members(project: &Project, ids: &[Id]) -> Vec<Command> {
    let gone: BTreeSet<&Id> = ids.iter().collect();
    let mut out = Vec::new();
    for l in project.levels() {
        for g in &l.groups {
            if !g.members.iter().any(|m| gone.contains(m)) {
                continue;
            }
            let members: Vec<Id> = g
                .members
                .iter()
                .filter(|m| !gone.contains(m))
                .cloned()
                .collect();
            out.push(if members.is_empty() {
                Command::RemoveGroup {
                    level: l.slug.clone(),
                    id: g.id.clone(),
                }
            } else {
                Command::SetGroup {
                    level: l.slug.clone(),
                    group: ObjectGroup {
                        members,
                        ..g.clone()
                    },
                    index: None,
                }
            });
        }
    }
    out
}

/// A fresh group id for `level`: random, unused by its groups.
pub fn new_group_id(project: &Project, level: &Slug) -> Id {
    let taken: BTreeSet<&Id> = project
        .level(level.as_str())
        .map(|l| l.groups.iter().map(|g| &g.id).collect())
        .unwrap_or_default();
    loop {
        let id = Id::generate();
        if !taken.contains(&id) {
            return id;
        }
    }
}

/// Group `members` (entities and annotations of `level`) as a new group
/// named `name` (default "Group <n>"), with `id` (default a fresh one). A
/// member already in a group leaves it first (an object is in one group).
/// One `batch` labelled "Group <n> objects".
pub fn group(
    project: &Project,
    level: &Slug,
    members: &[Id],
    name: Option<String>,
    id: Option<Id>,
) -> Result<Command> {
    let l = project
        .level(level.as_str())
        .ok_or_else(|| CommandError::UnknownLevel(level.clone()))?;
    if members.is_empty() {
        return Err(CommandError::Invalid(
            "a group needs at least one member".into(),
        ));
    }
    let mut seen = BTreeSet::new();
    for m in members {
        let known = l.all_entities().any(|e| e.id == *m) || l.notes.iter().any(|n| n.id == *m);
        if !known {
            return Err(CommandError::Invalid(format!(
                "{m} is not an entity or annotation of level {level}"
            )));
        }
        if !seen.insert(m) {
            return Err(CommandError::Invalid(format!("{m} is listed twice")));
        }
    }
    let id = id.unwrap_or_else(|| new_group_id(project, level));
    if l.group(&id).is_some() {
        return Err(CommandError::DuplicateId(id));
    }
    let name = name
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| format!("Group {}", l.groups.len() + 1));
    let mut commands = drop_members(project, members);
    commands.push(Command::SetGroup {
        level: level.clone(),
        group: ObjectGroup::new(id, name, members.to_vec()),
        index: None,
    });
    let label = match members.len() {
        1 => "Group 1 object".to_string(),
        n => format!("Group {n} objects"),
    };
    Ok(Command::Batch { label, commands })
}

/// Remove group `id` of `level`; its members stay. Labelled "Ungroup <name>".
pub fn ungroup(project: &Project, level: &Slug, id: &Id) -> Result<Command> {
    let l = project
        .level(level.as_str())
        .ok_or_else(|| CommandError::UnknownLevel(level.clone()))?;
    let g = l
        .group(id)
        .ok_or_else(|| CommandError::Invalid(format!("level {level} has no group {id:?}")))?;
    Ok(Command::Batch {
        label: format!("Ungroup {}", g.name),
        commands: vec![Command::RemoveGroup {
            level: level.clone(),
            id: id.clone(),
        }],
    })
}
