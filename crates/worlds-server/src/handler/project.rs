//! `project.*`: describe, validate, bake, export, save, reload, read_file,
//! update.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::{Value, json};
use worlds_core::command::ProjectSetting;
use worlds_core::{Color, Command, Project, Slug};

use super::events::problem_json;
use super::params::{self, find_level, present, rel_path};
use super::{Client, Confirm, Handler, Write, with_rev};
use crate::describe::{Mode, describe as describe_project};
use crate::host::Host;
use crate::rpc::RpcError;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Levels {
    levels: Option<Vec<Slug>>,
}

/// `project.describe {}`.
pub(crate) fn describe(
    h: &mut Handler,
    host: &mut dyn Host,
    method: &str,
    p: Value,
) -> Result<Value, RpcError> {
    let Empty {} = params::parse(method, p)?;
    let mode = Mode {
        headless: h.headless,
        approval: h.approval,
        pending: h.approvals.pending.len(),
    };
    let recovered = host.recovered();
    let doc = host.doc();
    let mut out = describe_project(doc.project, doc.dir, h.rev, mode);
    if let Some(r) = recovered {
        out["recovered"] = r;
    }
    Ok(out)
}

/// `project.validate {}` → `{ problems }`; refreshes the host's panel and
/// sends `problems`.
pub(crate) fn validate(
    h: &mut Handler,
    host: &mut dyn Host,
    method: &str,
    p: Value,
) -> Result<Value, RpcError> {
    let Empty {} = params::parse(method, p)?;
    let problems = worlds_core::validate(host.doc().project);
    host.problems(&problems);
    h.notify_problems(&problems);
    let list: Vec<Value> = problems.iter().map(problem_json).collect();
    with_rev(h.rev, json!({ "problems": list }))
}

/// The levels a `levels?` param names (all of them when absent).
fn levels_of(project: &Project, levels: Option<Vec<Slug>>) -> Result<Vec<Slug>, RpcError> {
    match levels {
        Some(ls) => {
            for l in &ls {
                find_level(project, l.as_str())?;
            }
            Ok(ls)
        }
        None => Ok(project.levels().map(|l| l.slug.clone()).collect()),
    }
}

/// `project.bake { levels? }`: tile counts of the auto layers, from the
/// host's bake cache (baking what isn't cached). Writes nothing.
pub(crate) fn bake(
    h: &mut Handler,
    host: &mut dyn Host,
    method: &str,
    p: Value,
) -> Result<Value, RpcError> {
    let p: Levels = params::parse(method, p)?;
    let doc = host.doc();
    let project = &*doc.project;
    let levels = levels_of(project, p.levels)?;
    let autos: Vec<&Slug> = project
        .layers
        .iter()
        .filter(|l| l.is_auto())
        .map(|l| &l.slug)
        .collect();
    let mut out = Vec::new();
    // What baked, in the export's shape, for the export warnings.
    let mut exported: BTreeMap<Slug, worlds_core::export::BakedLevel> = BTreeMap::new();
    for level in levels {
        let cached = autos
            .iter()
            .all(|a| doc.bakes.get(level.as_str(), a.as_str()).is_some());
        let mut entry = json!({ "level": level });
        let baked = if cached {
            Ok(())
        } else {
            doc.bakes
                .bake_level(project, level.as_str(), doc.images)
                .map(drop)
        };
        match baked {
            Ok(()) => {
                let layers: Vec<Value> = autos
                    .iter()
                    .map(|a| {
                        let n = doc
                            .bakes
                            .get(level.as_str(), a.as_str())
                            .map_or(0, |b| b.placements.len());
                        json!({ "layer": a, "tiles": n })
                    })
                    .collect();
                entry["layers"] = layers.into();
                exported.insert(
                    level.clone(),
                    cached_level(project, doc.bakes, level.as_str()),
                );
            }
            Err(e) => {
                entry["layers"] = json!([]);
                entry["error"] = e.to_string().into();
            }
        }
        out.push(entry);
    }
    let warnings = worlds_rules::export_warnings(project, &exported);
    with_rev(h.rev, json!({ "levels": out, "warnings": warnings }))
}

/// A cached level's bake in the export's shape: its auto layers' tiles and
/// its derived layers' effective grids (what the cache has of them).
fn cached_level(
    project: &Project,
    bakes: &worlds_rules::BakeCache,
    level: &str,
) -> worlds_core::export::BakedLevel {
    let tiles = project
        .layers
        .iter()
        .filter(|l| l.is_auto())
        .filter_map(|l| {
            let b = bakes.get(level, l.slug.as_str())?;
            let tiles = b
                .placements
                .iter()
                .map(|p| worlds_core::export::BakedTile {
                    px: p.px,
                    tileset: p.tileset.clone(),
                    tile: p.tile,
                    flip: p.flip,
                    alpha: p.alpha,
                })
                .collect();
            Some((l.slug.clone(), tiles))
        })
        .collect();
    let derived = project
        .layers
        .iter()
        .filter(|l| l.is_derived())
        .filter_map(|l| {
            let g = bakes.derived(level, l.slug.as_str())?;
            Some((l.slug.clone(), (*g).clone()))
        })
        .collect();
    worlds_core::export::BakedLevel { tiles, derived }
}

/// `project.export { levels? }`: bake and write `export/`.
pub(crate) fn export(
    h: &mut Handler,
    host: &mut dyn Host,
    method: &str,
    p: Value,
) -> Result<Value, RpcError> {
    let p: Levels = params::parse(method, p)?;
    let full = p.levels.is_none();
    let doc = host.doc();
    let levels = levels_of(doc.project, p.levels)?;
    let (files, warnings) = export_levels(doc.project, doc.dir, doc.images, &levels, full)?;
    with_rev(
        h.rev,
        json!({ "dir": "export", "files": files, "warnings": warnings }),
    )
}

/// Bake `levels` and write them (plus `world.json` and `enums.json`) to
/// `<dir>/export`; `full` prunes level files of other levels. Returns the
/// files relative to the export directory, and the export warnings of what
/// was written (as `project.bake` reports them).
pub fn export_levels(
    project: &Project,
    dir: &std::path::Path,
    images: &worlds_rules::TilesetImages,
    levels: &[Slug],
    full: bool,
) -> Result<(Vec<String>, Vec<String>), RpcError> {
    let mut baked = BTreeMap::new();
    for slug in levels {
        let (_, level) = find_level(project, slug.as_str())?;
        let b = worlds_rules::bake_for_export(project, level, images)
            .map_err(|e| RpcError::invalid(format!("baking level {slug}: {e}")))?;
        baked.insert(slug.clone(), b);
    }
    let files = worlds_core::export::write(project, dir, &dir.join("export"), &baked, full)
        .map_err(|e| RpcError::from_format(&e))?;
    let warnings = worlds_rules::export_warnings(project, &baked);
    Ok((files.iter().map(|f| rel_path(f)).collect(), warnings))
}

/// `project.save {}`: the host saves; sends `saved`.
pub(crate) fn save(
    h: &mut Handler,
    host: &mut dyn Host,
    method: &str,
    p: Value,
) -> Result<Value, RpcError> {
    let Empty {} = params::parse(method, p)?;
    let files = host.save()?;
    h.notify_saved(&files);
    with_rev(h.rev, json!({ "files": files }))
}

/// `project.read_file { path }`: a file as saving would write it now.
pub(crate) fn read_file(project: &Project, method: &str, p: Value) -> Result<Value, RpcError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        path: String,
    }
    let p: P = params::parse(method, p)?;
    // Only `/`-separated relative paths name a file; `..`, `.` and empty
    // parts never do.
    let parts: Vec<&str> = p.path.split('/').collect();
    let plain = parts
        .iter()
        .all(|c| !c.is_empty() && *c != "." && *c != "..");
    let rendered = if plain {
        let rel: std::path::PathBuf = parts.iter().collect();
        worlds_core::format::render_file(project, &rel)
            .map_err(|e| RpcError::invalid(format!("{} can't be rendered: {e}", p.path)))?
    } else {
        None
    };
    match rendered {
        Some(bytes) => Ok(json!({ "path": p.path, "text": String::from_utf8_lossy(&bytes) })),
        None => Err(RpcError::invalid(format!(
            "{:?} is not a project file; paths are relative to the project directory, like project.json, layers.json, entities/<type>.json or worlds/<world>/levels/<level>/<layer>.json",
            p.path
        ))),
    }
}

/// `project.reload { confirm? }`: built as a write so `confirm` and the
/// queue apply.
pub(crate) fn reload_builder(project: &Project, method: &str, p: Value) -> Result<Write, RpcError> {
    let Empty {} = params::parse(method, p)?;
    let dirty = project.files.dirty().len();
    let confirm = (dirty > 0).then(|| {
        Confirm::new(
            format!("discards unsaved edits to {dirty} file(s)"),
            format!(
                "reloading discards unsaved edits to {dirty} file(s); save first, or send confirm: true"
            ),
        )
    });
    Ok(Write::Reload { confirm })
}

/// Run `project.reload`.
pub(crate) fn reload(
    h: &mut Handler,
    host: &mut dyn Host,
    client: &Client,
) -> Result<Value, RpcError> {
    host.reload()?;
    host.doc().history.clear();
    let rev = h.reloaded(&client.origin(), Some(&client.session));
    Ok(json!({ "rev": rev }))
}

/// `project.update { name?, bg_color?, collision_layer?, seed? }` → one
/// `set_project_setting` per key given (a `batch` when several).
pub(crate) fn update(_project: &Project, method: &str, p: Value) -> Result<Write, RpcError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        name: Option<String>,
        #[serde(default, deserialize_with = "present")]
        bg_color: Option<Option<Color>>,
        #[serde(default, deserialize_with = "present")]
        collision_layer: Option<Option<Slug>>,
        seed: Option<i64>,
    }
    let p: P = params::parse(method, p)?;
    let mut settings = Vec::new();
    if let Some(name) = p.name {
        settings.push(ProjectSetting::Name(name));
    }
    if let Some(c) = p.bg_color {
        settings.push(ProjectSetting::BgColor(c));
    }
    if let Some(l) = p.collision_layer {
        settings.push(ProjectSetting::CollisionLayer(l));
    }
    if let Some(s) = p.seed {
        settings.push(ProjectSetting::Seed(s));
    }
    if settings.is_empty() {
        return Err(RpcError::invalid_params(format!(
            "{method}: nothing to update; give name, bg_color, collision_layer or seed"
        )));
    }
    Ok(Write::cmds(
        settings
            .into_iter()
            .map(|setting| Command::SetProjectSetting { setting })
            .collect(),
    ))
}
