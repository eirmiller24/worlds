//! `notes.*`. A note on the wire is `{ scope, note }`, the note in file form.
//!
//! Locked annotations: the commands ignore `locked` (it is the editor's and
//! this API's rule, not the model's). Moving one (its geometry or guide
//! group), or removing one, needs `confirm: true` here; editing its text,
//! tags, status, export flag or `locked` itself never does.

use serde::Deserialize;
use serde_json::{Map, Value, json};
use worlds_core::command::NoteScope;
use worlds_core::{
    Annotation, Command, CommandError, GuideGroup, Id, NoteStatus, Project, Shape, Slug,
};

use super::params::{self, default_world, ensure_id, find_level};
use super::{Confirm, Write};
use crate::rpc::RpcError;

/// The annotation kinds, as `kind` names them.
const KINDS: [&str; 5] = ["note", "pin", "path", "region", "guide"];

/// Every note with its scope: world notes first, then levels in
/// `level.list` order.
fn all(project: &Project) -> Vec<(NoteScope, &Annotation)> {
    let mut out = Vec::new();
    for w in &project.worlds {
        out.extend(
            w.notes
                .iter()
                .map(|n| (NoteScope::World(w.slug.clone()), n)),
        );
    }
    for w in &project.worlds {
        for l in &w.levels {
            out.extend(
                l.notes
                    .iter()
                    .map(|n| (NoteScope::Level(l.slug.clone()), n)),
            );
        }
    }
    out
}

/// A note and its scope, by id.
pub(crate) fn locate<'p>(project: &'p Project, id: &Id) -> Option<(NoteScope, &'p Annotation)> {
    project.find_note(id).map(|n| (n.scope(), n.note))
}

fn find(project: &Project, id: &Id) -> Result<Annotation, RpcError> {
    locate(project, id)
        .map(|(_, n)| n.clone())
        .ok_or_else(|| RpcError::command(&CommandError::UnknownNote(id.clone())))
}

fn check_kind(method: &str, k: &str) -> Result<(), RpcError> {
    if KINDS.contains(&k) {
        Ok(())
    } else {
        Err(RpcError::invalid_params(format!(
            "{method}: no kind {k:?}; the kinds are {}",
            KINDS.join(", ")
        )))
    }
}

/// What a move changes: the kind, every point, `closed` and the guide
/// group. Text, tags, status, color, links, export, `locked` and a pin's
/// icon and label are not geometry.
fn placement(n: &Annotation) -> (&'static str, Vec<[i64; 2]>, bool, Option<&Slug>) {
    let (closed, group) = match &n.shape {
        Shape::Guide { closed, group, .. } => (*closed, group.as_ref()),
        _ => (false, None),
    };
    (n.shape.name(), n.shape.points(), closed, group)
}

/// `needs_confirm` for touching locked notes: "note x is locked; moving it
/// needs confirm: true" (or "3 notes are locked (a, b, c); ... them ...").
fn locked_confirm(locked: &[&Id], verb: &str) -> Option<Confirm> {
    let reason = match locked {
        [] => return None,
        [one] => format!("note {one} is locked; {verb} it needs confirm: true"),
        many => {
            let mut list: Vec<String> = many.iter().take(5).map(|i| i.to_string()).collect();
            if many.len() > 5 {
                list.push("...".into());
            }
            format!(
                "{} notes are locked ({}); {verb} them needs confirm: true",
                many.len(),
                list.join(", ")
            )
        }
    };
    Some(Confirm::new(reason.clone(), reason))
}

#[derive(Deserialize, Clone, Copy, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
enum StatusFilter {
    Open,
    Done,
    #[default]
    All,
}

/// `notes.list { scope?, tag?, status?, kind?, kinds?, group?, export? }`.
pub(crate) fn list(project: &Project, method: &str, p: Value) -> Result<Value, RpcError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        scope: Option<NoteScope>,
        tag: Option<String>,
        #[serde(default)]
        status: StatusFilter,
        kind: Option<String>,
        kinds: Option<Vec<String>>,
        group: Option<Slug>,
        export: Option<bool>,
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
    for k in p.kind.iter().chain(p.kinds.iter().flatten()) {
        check_kind(method, k)?;
    }
    let notes: Vec<Value> = all(project)
        .into_iter()
        .filter(|(s, _)| p.scope.as_ref().is_none_or(|x| x == s))
        .filter(|(_, n)| p.tag.as_ref().is_none_or(|t| n.tags.contains(t)))
        .filter(|(_, n)| match p.status {
            StatusFilter::All => true,
            StatusFilter::Open => n.status == NoteStatus::Open,
            StatusFilter::Done => n.status == NoteStatus::Done,
        })
        .filter(|(_, n)| p.kind.as_ref().is_none_or(|k| n.shape.name() == k))
        .filter(|(_, n)| {
            p.kinds
                .as_ref()
                .is_none_or(|ks| ks.iter().any(|k| n.shape.name() == k))
        })
        .filter(|(_, n)| {
            p.group
                .as_ref()
                .is_none_or(|g| matches!(&n.shape, Shape::Guide { group: Some(x), .. } if x == g))
        })
        .filter(|(_, n)| p.export.is_none_or(|e| n.export == e))
        .map(|(s, n)| json!({ "scope": s, "note": n }))
        .collect();
    Ok(json!({ "notes": notes }))
}

/// `notes.add { scope, note, index? }` → `add_note`.
pub(crate) fn add(project: &Project, method: &str, p: Value) -> Result<Write, RpcError> {
    let mut m = params::object(method, p)?;
    let mut note = m
        .remove("note")
        .ok_or_else(|| RpcError::invalid_params(format!("{method}: missing note")))?;
    let id = ensure_id(project, method, "note", &mut note)?;
    m.insert("note".into(), note);
    let cmd = params::command(method, "add_note", Value::Object(m))?;
    Ok(Write::Commands {
        cmds: vec![cmd],
        id: Some(id),
        confirm: None,
    })
}

/// `notes.update { id, note }` (wholesale) or `{ id, patch }` (top-level
/// keys merged in, `null` removes a key, `kind` can't change). Moving a
/// locked note (geometry or `group`) needs `confirm`.
pub(crate) fn update(project: &Project, method: &str, p: Value) -> Result<Write, RpcError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        id: Id,
        note: Option<Value>,
        patch: Option<Map<String, Value>>,
    }
    let p: P = params::parse(method, p)?;
    let old = find(project, &p.id)?;
    let note = match (p.note, p.patch) {
        (Some(_), Some(_)) | (None, None) => {
            return Err(RpcError::invalid_params(format!(
                "{method}: give either note (the whole annotation) or patch (keys to change), not both"
            )));
        }
        (Some(mut note), None) => {
            if let Value::Object(m) = &mut note {
                m.entry("id").or_insert_with(|| p.id.as_str().into());
            }
            params::parse::<Annotation>(method, note)?
        }
        (None, Some(patch)) => {
            let mut v = serde_json::to_value(&old)
                .map_err(|e| RpcError::internal(format!("note {}: {e}", p.id)))?;
            let Value::Object(m) = &mut v else {
                return Err(RpcError::internal("a note did not serialize to an object"));
            };
            for (k, val) in patch {
                if k == "kind" && m.get("kind") != Some(&val) {
                    return Err(RpcError::invalid_params(format!(
                        "{method}: a note's kind can't change ({} to {val}); remove it and add a new one",
                        m.get("kind").cloned().unwrap_or(Value::Null)
                    )));
                }
                if k == "id" && val != Value::String(p.id.to_string()) {
                    return Err(RpcError::invalid_params(format!(
                        "{method}: a note's id can't change"
                    )));
                }
                if val.is_null() {
                    m.remove(&k);
                } else {
                    m.insert(k, val);
                }
            }
            params::parse::<Annotation>(method, v)?
        }
    };
    let confirm = if old.locked && placement(&old) != placement(&note) {
        locked_confirm(&[&p.id], "moving")
    } else {
        None
    };
    Ok(Write::Commands {
        cmds: vec![Command::UpdateNote { id: p.id, note }],
        id: None,
        confirm,
    })
}

/// `notes.move { ids, delta }` → one `batch` of `update_note`s, each
/// geometry moved by `delta` pixels (label "Move note x" / "Move 3 notes").
/// Locked notes need `confirm`.
pub(crate) fn move_notes(project: &Project, method: &str, p: Value) -> Result<Write, RpcError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        ids: Vec<Id>,
        delta: [i64; 2],
    }
    let p: P = params::parse(method, p)?;
    if p.ids.is_empty() {
        return Err(RpcError::invalid_params(format!("{method}: ids is empty")));
    }
    let mut commands = Vec::new();
    let mut locked = Vec::new();
    for (i, id) in p.ids.iter().enumerate() {
        if p.ids[..i].contains(id) {
            return Err(RpcError::invalid_params(format!(
                "{method}: note {id} is listed twice"
            )));
        }
        let mut note = find(project, id)?;
        if note.locked {
            locked.push(id);
        }
        note.shape.translate(p.delta);
        commands.push(Command::UpdateNote {
            id: id.clone(),
            note,
        });
    }
    let label = match p.ids.as_slice() {
        [one] => format!("Move note {one}"),
        many => format!("Move {} notes", many.len()),
    };
    Ok(Write::Commands {
        cmds: vec![Command::Batch { label, commands }],
        id: None,
        confirm: locked_confirm(&locked, "moving"),
    })
}

/// `notes.resolve { id, status? }` → `update_note` with the status.
pub(crate) fn resolve(project: &Project, method: &str, p: Value) -> Result<Write, RpcError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        id: Id,
        #[serde(default = "done")]
        status: NoteStatus,
    }
    fn done() -> NoteStatus {
        NoteStatus::Done
    }
    let p: P = params::parse(method, p)?;
    let mut note = find(project, &p.id)?;
    note.status = p.status;
    Ok(Write::one(Command::UpdateNote { id: p.id, note }))
}

/// `notes.remove { ids }` → `remove_note`s. Locked notes need `confirm`.
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
    // An unknown id is the command's error; only known locked ones count.
    // One index for every id (not a scan of the project per id).
    let index = project.note_index();
    let locked: Vec<&Id> = p
        .ids
        .iter()
        .filter(|id| index.get(id).is_some_and(|n| n.note.locked))
        .collect();
    let confirm = locked_confirm(&locked, "removing");
    Ok(Write::Commands {
        cmds: p
            .ids
            .iter()
            .map(|id| Command::RemoveNote { id: id.clone() })
            .collect(),
        id: None,
        confirm,
    })
}

/// `notes.set_guide_group { world?, group, index? }` → `set_guide_group`.
pub(crate) fn set_guide_group(
    project: &Project,
    method: &str,
    p: Value,
) -> Result<Write, RpcError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        world: Option<Slug>,
        group: GuideGroup,
        index: Option<usize>,
    }
    let p: P = params::parse(method, p)?;
    let world = default_world(project, method, p.world.as_ref().map(Slug::as_str))?;
    Ok(Write::one(Command::SetGuideGroup {
        world,
        group: p.group,
        index: p.index,
    }))
}

/// `notes.remove_guide_group { world?, slug }` → `remove_guide_group`.
pub(crate) fn remove_guide_group(
    project: &Project,
    method: &str,
    p: Value,
) -> Result<Write, RpcError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        world: Option<Slug>,
        slug: Slug,
    }
    let p: P = params::parse(method, p)?;
    let world = default_world(project, method, p.world.as_ref().map(Slug::as_str))?;
    Ok(Write::one(Command::RemoveGuideGroup {
        world,
        slug: p.slug,
    }))
}
