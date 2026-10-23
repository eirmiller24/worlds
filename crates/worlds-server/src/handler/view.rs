//! `view.*`: what the editor shows. Never changes the project or `rev`;
//! headless hosts answer `unsupported`.

use base64::Engine as _;
use serde::Deserialize;
use serde_json::{Value, json};
use worlds_core::{CommandError, Id, Project, Slug};

use super::params::{self, find_level};
use super::{Handler, notes, with_rev};
use crate::host::{Host, Selection, SnapshotRequest, ViewTarget, needs_editor};
use crate::rpc::RpcError;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Target {
    level: Option<Slug>,
    world_rect: Option<[i64; 4]>,
    world: Option<Slug>,
}

fn target(method: &str, t: Target) -> Result<ViewTarget, RpcError> {
    match (t.level, t.world_rect) {
        (Some(l), None) if t.world.is_none() => Ok(ViewTarget::Level(l)),
        (None, Some(rect)) => Ok(ViewTarget::WorldRect {
            world: t.world,
            rect,
        }),
        _ => Err(RpcError::invalid_params(format!(
            "{method}: give either level or world_rect (with world when there are several)"
        ))),
    }
}

pub(crate) fn call(
    h: &mut Handler,
    host: &mut dyn Host,
    method: &str,
    p: Value,
) -> Result<Value, RpcError> {
    if h.headless {
        return Err(needs_editor(method));
    }
    match method {
        "view.focus" => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct P {
                level: Option<Slug>,
                world_rect: Option<[i64; 4]>,
                world: Option<Slug>,
                note: Option<Id>,
            }
            let p: P = params::parse(method, p)?;
            if let (Some(w), None, None, None) = (&p.world, &p.level, &p.world_rect, &p.note) {
                // A world alone: switch to it and fit it.
                if host.doc().project.world(w.as_str()).is_none() {
                    return Err(RpcError::command(&CommandError::UnknownWorld(w.clone())));
                }
                host.focus(&ViewTarget::World(w.clone()))?;
                return Ok(json!({ "rev": h.rev }));
            }
            let Some(id) = p.note else {
                let t = Target {
                    level: p.level,
                    world_rect: p.world_rect,
                    world: p.world,
                };
                host.focus(&target(method, t)?)?;
                return Ok(json!({ "rev": h.rev }));
            };
            if p.level.is_some() || p.world_rect.is_some() || p.world.is_some() {
                return Err(RpcError::invalid_params(format!(
                    "{method}: give note alone, or level, or world_rect (with world when there are several)"
                )));
            }
            // The editor frames it (bounding box as built, a 15 % margin,
            // at least 8 project cells each way) and makes a level note's
            // level active; an unknown id is refused here first.
            if notes::locate(host.doc().project, &id).is_none() {
                return Err(RpcError::command(&CommandError::UnknownNote(id)));
            }
            host.focus(&ViewTarget::Note(id))?;
            Ok(json!({ "rev": h.rev }))
        }
        "view.select" => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct P {
                entities: Option<Vec<Id>>,
                notes: Option<Vec<Id>>,
                levels: Option<Vec<Slug>>,
                level: Option<Slug>,
            }
            let p: P = params::parse(method, p)?;
            let lists = p.entities.is_some() || p.notes.is_some() || p.levels.is_some();
            if lists == p.level.is_some() {
                return Err(RpcError::invalid_params(format!(
                    "{method}: give level alone, or any of entities, notes and levels"
                )));
            }
            let sel = Selection {
                level: p.level,
                entities: p.entities.unwrap_or_default(),
                notes: p.notes.unwrap_or_default(),
                levels: p.levels.unwrap_or_default(),
            };
            check_selection(host.doc().project, &sel)?;
            host.select(&sel)?;
            Ok(json!({ "rev": h.rev }))
        }
        _ => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct P {
                level: Option<Slug>,
                world_rect: Option<[i64; 4]>,
                world: Option<Slug>,
                size: Option<[u32; 2]>,
                layers: Option<Vec<Slug>>,
                overlays: Option<bool>,
            }
            let p: P = params::parse(method, p)?;
            let req = SnapshotRequest {
                target: target(
                    method,
                    Target {
                        level: p.level,
                        world_rect: p.world_rect,
                        world: p.world,
                    },
                )?,
                size: p.size.unwrap_or([1024, 1024]),
                layers: p.layers,
                overlays: p.overlays.unwrap_or(true),
            };
            let shot = host.snapshot(&req)?;
            let png = base64::engine::general_purpose::STANDARD.encode(&shot.png);
            with_rev(
                h.rev,
                json!({ "png": png, "width": shot.width, "height": shot.height, "scale": shot.scale }),
            )
        }
    }
}

/// Every id and slug of a `view.select` must exist (checked here, so the
/// editor never sees a half-valid selection): `unknown_entity`,
/// `unknown_note`, `unknown_level`.
fn check_selection(project: &Project, sel: &Selection) -> Result<(), RpcError> {
    for l in sel.level.iter().chain(&sel.levels) {
        find_level(project, l.as_str())?;
    }
    // One set per kind, built once (not a scan of the project per id).
    let entities: std::collections::HashSet<&worlds_core::Id> = if sel.entities.is_empty() {
        Default::default()
    } else {
        project
            .levels()
            .flat_map(|l| l.all_entities().map(|e| &e.id))
            .collect()
    };
    for id in &sel.entities {
        if !entities.contains(id) {
            return Err(RpcError::command(&CommandError::UnknownEntity(id.clone())));
        }
    }
    let index = (!sel.notes.is_empty()).then(|| project.note_index());
    for id in &sel.notes {
        if !index.as_ref().is_some_and(|i| i.contains_key(id)) {
            return Err(RpcError::command(&CommandError::UnknownNote(id.clone())));
        }
    }
    Ok(())
}
