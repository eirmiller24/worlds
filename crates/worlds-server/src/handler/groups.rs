//! `group.*`: object groups, named sets of a level's entities and
//! annotations (`level.json` `groups`). The grouping helpers are
//! `worlds_core::command::groups`, shared with the editor and MCP.

use serde::Deserialize;
use serde_json::{Value, json};
use worlds_core::command::groups;
use worlds_core::{Command, CommandError, Id, ObjectGroup, Project, Slug};

use super::Write;
use super::params::{self, find_level};
use crate::rpc::RpcError;

/// `group.list { level? }`: `{ groups: [{ level, id, name, members }] }`,
/// levels in `level.list` order.
pub(crate) fn list(project: &Project, method: &str, p: Value) -> Result<Value, RpcError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        level: Option<Slug>,
    }
    let p: P = params::parse(method, p)?;
    if let Some(l) = &p.level {
        find_level(project, l.as_str())?;
    }
    let groups: Vec<Value> = project
        .levels()
        .filter(|l| p.level.as_ref().is_none_or(|s| *s == l.slug))
        .flat_map(|l| {
            l.groups.iter().map(move |g| {
                json!({ "level": l.slug, "id": g.id, "name": g.name, "members": g.members })
            })
        })
        .collect();
    Ok(json!({ "groups": groups }))
}

/// `group.create { level, members, name?, id? }`: the members leave any
/// group they were in. Result has `id`.
pub(crate) fn create(project: &Project, method: &str, p: Value) -> Result<Write, RpcError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        level: Slug,
        members: Vec<Id>,
        name: Option<String>,
        id: Option<Id>,
    }
    let p: P = params::parse(method, p)?;
    find_level(project, p.level.as_str())?;
    let id =
        p.id.unwrap_or_else(|| groups::new_group_id(project, &p.level));
    let cmd = groups::group(project, &p.level, &p.members, p.name, Some(id.clone()))
        .map_err(|e| RpcError::command(&e))?;
    Ok(Write::Commands {
        cmds: vec![cmd],
        id: Some(json!(id)),
        confirm: None,
    })
}

/// The group `id` of `level`.
fn find<'p>(project: &'p Project, level: &Slug, id: &Id) -> Result<&'p ObjectGroup, RpcError> {
    let (_, l) = find_level(project, level.as_str())?;
    l.group(id).ok_or_else(|| {
        RpcError::command(&CommandError::Invalid(format!(
            "level {level} has no group {id:?}"
        )))
    })
}

/// `group.update { level, id, name?, members? }` → `set_group` (members
/// replace the list as given).
pub(crate) fn update(project: &Project, method: &str, p: Value) -> Result<Write, RpcError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        level: Slug,
        id: Id,
        name: Option<String>,
        members: Option<Vec<Id>>,
    }
    let p: P = params::parse(method, p)?;
    let mut g = find(project, &p.level, &p.id)?.clone();
    if p.name.is_none() && p.members.is_none() {
        return Err(RpcError::invalid_params(format!(
            "{method}: nothing to change; give name or members"
        )));
    }
    if let Some(n) = p.name {
        g.name = n;
    }
    if let Some(m) = p.members {
        g.members = m;
    }
    Ok(Write::one(Command::SetGroup {
        level: p.level,
        group: g,
        index: None,
    }))
}

/// `group.remove { level, ids }` → `remove_group`s (ungroup; the members
/// stay).
pub(crate) fn remove(project: &Project, method: &str, p: Value) -> Result<Write, RpcError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        level: Slug,
        ids: Vec<Id>,
    }
    let p: P = params::parse(method, p)?;
    if p.ids.is_empty() {
        return Err(RpcError::invalid_params(format!("{method}: ids is empty")));
    }
    let mut cmds = Vec::new();
    for id in &p.ids {
        find(project, &p.level, id)?;
        cmds.push(Command::RemoveGroup {
            level: p.level.clone(),
            id: id.clone(),
        });
    }
    Ok(Write::cmds(cmds))
}
