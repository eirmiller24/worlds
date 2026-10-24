//! `reference.*`: reference images. A reference on the wire is
//! `{ scope, reference }`, the reference in file form (`level.json` or
//! `world.json` `references`).
//!
//! Locked references: as with notes, the commands ignore `locked`; moving
//! or scaling a locked one (`px`, `scale`) with `reference.update`, or
//! removing one, needs `confirm: true` here.

use serde::Deserialize;
use serde_json::{Map, Value, json};
use worlds_core::command::NoteScope;
use worlds_core::{Command, CommandError, Id, Project, Reference};

use super::params::{self, find_level};
use super::{Confirm, Write};
use crate::rpc::RpcError;

/// Every reference with its scope: world references first, then levels in
/// `level.list` order; each list bottom first.
fn all(project: &Project) -> Vec<(NoteScope, &Reference)> {
    let mut out = Vec::new();
    for w in &project.worlds {
        out.extend(
            w.references
                .iter()
                .map(|r| (NoteScope::World(w.slug.clone()), r)),
        );
    }
    for w in &project.worlds {
        for l in &w.levels {
            out.extend(
                l.references
                    .iter()
                    .map(|r| (NoteScope::Level(l.slug.clone()), r)),
            );
        }
    }
    out
}

fn find(project: &Project, id: &Id) -> Result<Reference, RpcError> {
    all(project)
        .into_iter()
        .find(|(_, r)| r.id == *id)
        .map(|(_, r)| r.clone())
        .ok_or_else(|| RpcError::command(&CommandError::UnknownReference(id.clone())))
}

/// A fresh id no level, entity, note or reference uses.
fn fresh_id(project: &Project) -> Id {
    loop {
        let id = params::fresh_id(project);
        if all(project).iter().all(|(_, r)| r.id != id) {
            return id;
        }
    }
}

fn locked_confirm(locked: &[&Id], verb: &str) -> Option<Confirm> {
    let reason = match locked {
        [] => return None,
        [one] => format!("reference image {one} is locked; {verb} it needs confirm: true"),
        many => format!(
            "{} reference images are locked ({}); {verb} them needs confirm: true",
            many.len(),
            many.iter()
                .map(|i| i.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    };
    Some(Confirm::new(reason.clone(), reason))
}

/// `reference.list { scope? }`.
pub(crate) fn list(project: &Project, method: &str, p: Value) -> Result<Value, RpcError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        scope: Option<NoteScope>,
    }
    let p: P = params::parse(method, p)?;
    match &p.scope {
        Some(NoteScope::World(w)) if project.world(w.as_str()).is_none() => {
            return Err(RpcError::command(&CommandError::UnknownWorld(w.clone())));
        }
        Some(NoteScope::Level(l)) => {
            find_level(project, l.as_str())?;
        }
        _ => {}
    }
    let references: Vec<Value> = all(project)
        .into_iter()
        .filter(|(s, _)| p.scope.as_ref().is_none_or(|x| x == s))
        .map(|(s, r)| json!({ "scope": s, "reference": r }))
        .collect();
    Ok(json!({ "references": references }))
}

/// `reference.add { scope, reference, index? }` → `add_reference`;
/// `reference.id` is generated when left out.
pub(crate) fn add(project: &Project, method: &str, p: Value) -> Result<Write, RpcError> {
    let mut m = params::object(method, p)?;
    let mut r = m
        .remove("reference")
        .ok_or_else(|| RpcError::invalid_params(format!("{method}: missing reference")))?;
    let Value::Object(rm) = &mut r else {
        return Err(RpcError::invalid_params(format!(
            "{method}: reference must be an object"
        )));
    };
    let id = match rm.get("id") {
        Some(id @ Value::String(_)) => id.clone(),
        None | Some(Value::Null) => {
            let id = Value::String(fresh_id(project).to_string());
            rm.insert("id".into(), id.clone());
            id
        }
        Some(_) => {
            return Err(RpcError::invalid_params(format!(
                "{method}: reference.id must be a 10-character id string"
            )));
        }
    };
    m.insert("reference".into(), r);
    let cmd = params::command(method, "add_reference", Value::Object(m))?;
    Ok(Write::Commands {
        cmds: vec![cmd],
        id: Some(id),
        confirm: None,
    })
}

/// `reference.update { id, reference }` (wholesale) or `{ id, patch }`
/// (top-level keys merged in; `null` puts a key back to its default).
/// Moving or scaling a locked reference needs `confirm`.
pub(crate) fn update(project: &Project, method: &str, p: Value) -> Result<Write, RpcError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        id: Id,
        reference: Option<Value>,
        patch: Option<Map<String, Value>>,
    }
    let p: P = params::parse(method, p)?;
    let old = find(project, &p.id)?;
    let reference = match (p.reference, p.patch) {
        (Some(_), Some(_)) | (None, None) => {
            return Err(RpcError::invalid_params(format!(
                "{method}: give either reference (the whole entry) or patch (keys to change), not both"
            )));
        }
        (Some(mut r), None) => {
            if let Value::Object(m) = &mut r {
                m.entry("id").or_insert_with(|| p.id.as_str().into());
            }
            params::parse::<Reference>(method, r)?
        }
        (None, Some(patch)) => {
            let mut v = serde_json::to_value(&old)
                .map_err(|e| RpcError::internal(format!("reference {}: {e}", p.id)))?;
            let Value::Object(m) = &mut v else {
                return Err(RpcError::internal(
                    "a reference did not serialize to an object",
                ));
            };
            for (k, val) in patch {
                if k == "id" && val != Value::String(p.id.to_string()) {
                    return Err(RpcError::invalid_params(format!(
                        "{method}: a reference's id can't change"
                    )));
                }
                if val.is_null() {
                    m.remove(&k);
                } else {
                    m.insert(k, val);
                }
            }
            params::parse::<Reference>(method, v)?
        }
    };
    let moved = old.px != reference.px || old.scale != reference.scale;
    let confirm = if old.locked && moved {
        locked_confirm(&[&p.id], "moving or scaling")
    } else {
        None
    };
    Ok(Write::Commands {
        cmds: vec![Command::UpdateReference {
            id: p.id,
            reference,
        }],
        id: None,
        confirm,
    })
}

/// `reference.remove { ids }` → `remove_reference`s. Locked ones need
/// `confirm`.
pub(crate) fn remove(project: &Project, method: &str, p: Value) -> Result<Write, RpcError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        ids: Vec<Id>,
    }
    let p: P = params::parse(method, p)?;
    if p.ids.is_empty() {
        return Err(RpcError::invalid_params(format!("{method}: ids is empty")));
    }
    let refs = all(project);
    let locked: Vec<&Id> = p
        .ids
        .iter()
        .filter(|id| refs.iter().any(|(_, r)| r.id == **id && r.locked))
        .collect();
    let confirm = locked_confirm(&locked, "removing");
    Ok(Write::Commands {
        cmds: p
            .ids
            .iter()
            .map(|id| Command::RemoveReference { id: id.clone() })
            .collect(),
        id: None,
        confirm,
    })
}
