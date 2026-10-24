//! `view.*`: what the editor shows. Never changes the project or `rev`;
//! headless hosts answer `unsupported`.

use base64::Engine as _;
use serde::Deserialize;
use serde_json::{Value, json};
use worlds_core::{CommandError, Id, Project, Slug};

use super::params::{self, find_level};
use super::{Handler, notes, with_rev};
use crate::host::{
    Host, Selection, SnapshotRequest, ViewTarget, ViewportInfo, needs_editor, unknown_viewport,
};
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
                viewport: Option<u32>,
            }
            let p: P = params::parse(method, p)?;
            let vp = p.viewport;
            if let (Some(w), None, None, None) = (&p.world, &p.level, &p.world_rect, &p.note) {
                // A world alone: switch to it and fit it.
                if host.doc().project.world(w.as_str()).is_none() {
                    return Err(RpcError::command(&CommandError::UnknownWorld(w.clone())));
                }
                host.focus(&ViewTarget::World(w.clone()), vp)?;
                return Ok(json!({ "rev": h.rev }));
            }
            let Some(id) = p.note else {
                let t = Target {
                    level: p.level,
                    world_rect: p.world_rect,
                    world: p.world,
                };
                host.focus(&target(method, t)?, vp)?;
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
            host.focus(&ViewTarget::Note(id), vp)?;
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
                viewport: Option<u32>,
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
            host.select(&sel, p.viewport)?;
            Ok(json!({ "rev": h.rev }))
        }
        "view.list_viewports" => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct P {}
            let _: P = params::parse(method, p)?;
            let tabs = host.viewports()?;
            let focused = tabs.iter().find(|t| t.focused).map(|t| t.id);
            let list: Vec<Value> = tabs
                .iter()
                .map(|t| {
                    json!({ "id": t.id, "world": t.world, "level": t.level, "focused": t.focused,
                        "world_rect": t.world_rect, "zoom": t.zoom })
                })
                .collect();
            Ok(json!({ "rev": h.rev, "focused": focused, "viewports": list }))
        }
        "view.open_viewport" => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct P {
                world: Slug,
                focus: Option<bool>,
            }
            let p: P = params::parse(method, p)?;
            if host.doc().project.world(p.world.as_str()).is_none() {
                return Err(RpcError::command(&CommandError::UnknownWorld(p.world)));
            }
            let id = host.open_viewport(&p.world, p.focus.unwrap_or(false))?;
            Ok(json!({ "rev": h.rev, "viewport": id }))
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
                viewport: Option<u32>,
            }
            let p: P = params::parse(method, p)?;
            let t = match (&p.level, p.world_rect, &p.world) {
                // Nothing to frame: what the tab shows, in its world.
                (None, None, None) => {
                    let tab = viewport(host, p.viewport)?;
                    ViewTarget::WorldRect {
                        world: Some(tab.world),
                        rect: tab.world_rect,
                    }
                }
                // A rect in the tab's world.
                (None, Some(rect), None) if p.viewport.is_some() => ViewTarget::WorldRect {
                    world: Some(viewport(host, p.viewport)?.world),
                    rect,
                },
                _ => target(
                    method,
                    Target {
                        level: p.level,
                        world_rect: p.world_rect,
                        world: p.world,
                    },
                )?,
            };
            let req = SnapshotRequest {
                target: t,
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

/// Map tab `id` (`None`: the focused one); an unknown id is `invalid params`.
fn viewport(host: &mut dyn Host, id: Option<u32>) -> Result<ViewportInfo, RpcError> {
    let tabs = host.viewports()?;
    let found = match id {
        Some(id) => tabs.into_iter().find(|t| t.id == id),
        None => tabs.into_iter().find(|t| t.focused),
    };
    found.ok_or_else(|| unknown_viewport(id.unwrap_or(0)))
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
