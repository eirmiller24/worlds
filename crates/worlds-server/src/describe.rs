//! `project.describe`: the agent's map of the project (`docs/api.md`,
//! "project"). Compact on purpose: counts instead of content, definitions
//! without editor-only keys.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{Map, Value, json};
use worlds_core::{Annotation, FieldDef, LayerKind, NoteStatus, Project, Shape};

/// The mode flags `describe` reports.
#[derive(Debug, Clone, Copy)]
pub struct Mode {
    pub headless: bool,
    pub approval: bool,
    /// Approval batches waiting for the user.
    pub pending: usize,
}

/// A field definition as in the files, without `name` and `display`.
pub fn field_json(f: &FieldDef) -> Value {
    let mut v = serde_json::to_value(f).unwrap_or(Value::Null);
    if let Value::Object(m) = &mut v {
        m.remove("name");
        m.remove("display");
    }
    v
}

/// `(open, done)` of the annotations that aren't guides: guides are
/// reference geometry, not work items, and are counted apart.
fn notes_count<'a>(notes: impl Iterator<Item = &'a Annotation>) -> (usize, usize) {
    notes
        .filter(|n| !is_guide(n))
        .fold((0, 0), |(o, d), n| match n.status {
            NoteStatus::Open => (o + 1, d),
            NoteStatus::Done => (o, d + 1),
        })
}

fn is_guide(n: &Annotation) -> bool {
    matches!(n.shape, Shape::Guide { .. })
}

/// The whole `project.describe` result.
pub fn describe(project: &Project, dir: &Path, rev: u64, mode: Mode) -> Value {
    let mut out = Map::new();
    out.insert("rev".into(), rev.into());
    out.insert("name".into(), project.name.clone().into());
    out.insert("dir".into(), dir.to_string_lossy().into_owned().into());
    out.insert("grid_size".into(), project.grid_size.into());
    out.insert("bg_color".into(), json!(project.bg_color));
    out.insert("collision_layer".into(), json!(project.collision_layer));
    out.insert("seed".into(), project.seed.into());
    out.insert("blocking_values".into(), json!(project.blocking_values));
    out.insert(
        "default_level_size".into(),
        json!(project.default_level_size),
    );
    out.insert("export_on_save".into(), project.export_on_save.into());
    out.insert(
        "mode".into(),
        json!({ "headless": mode.headless, "approval": mode.approval, "pending": mode.pending }),
    );
    out.insert(
        "level_fields".into(),
        project.level_fields.iter().map(field_json).collect(),
    );
    out.insert("worlds".into(), worlds(project));
    out.insert("levels".into(), levels(project));
    out.insert("layers".into(), layers(project));
    out.insert("entity_types".into(), entity_types(project));
    out.insert(
        "enums".into(),
        project
            .enums
            .iter()
            .map(|e| json!({ "slug": e.slug, "values": e.values.iter().map(|v| &v.slug).collect::<Vec<_>>() }))
            .collect(),
    );
    out.insert("tilesets".into(), tilesets(project, dir));
    out.insert(
        "assemblies".into(),
        project
            .assemblies
            .iter()
            .map(|a| {
                let mut o = json!({ "slug": a.slug, "name": a.name });
                if let Some(c) = &a.category {
                    o["category"] = json!(c);
                }
                o["tags"] = json!(a.tags);
                if let Some(d) = &a.doc {
                    o["doc"] = json!(d);
                }
                o["size"] = json!(a.size);
                o["items"] = json!(a.item_count());
                o
            })
            .collect(),
    );
    let (open, done) = notes_count(
        project
            .worlds
            .iter()
            .flat_map(|w| w.notes.iter().chain(w.levels.iter().flat_map(|l| &l.notes))),
    );
    out.insert("notes".into(), json!({ "open": open, "done": done }));
    Value::Object(out)
}

fn worlds(project: &Project) -> Value {
    project
        .worlds
        .iter()
        .map(|w| {
            let guides_in = |slug: &worlds_core::Slug| {
                w.notes
                    .iter()
                    .filter(
                        |n| matches!(&n.shape, Shape::Guide { group: Some(g), .. } if g == slug),
                    )
                    .count()
            };
            let groups: Vec<Value> = w
                .guide_groups
                .iter()
                .map(|g| {
                    let mut o = json!({ "slug": g.slug });
                    if let Some(name) = &g.name {
                        o["name"] = json!(name);
                    }
                    o["offset"] = json!(g.offset);
                    if let Some(c) = &g.color {
                        o["color"] = json!(c);
                    }
                    o["guides"] = guides_in(&g.slug).into();
                    o
                })
                .collect();
            json!({
                "slug": w.slug,
                "name": w.name.clone().unwrap_or_else(|| title_case(w.slug.as_str())),
                "levels": w.levels.iter().map(|l| &l.slug).collect::<Vec<_>>(),
                "level_count": w.levels.len(),
                "bbox": world_bbox(project, w),
                "notes": w.notes.iter().filter(|n| !is_guide(n)).count(),
                "guides": w.notes.iter().filter(|n| is_guide(n)).count(),
                "guide_groups": groups,
            })
        })
        .collect()
}

/// `[x, y, w, h]` in world pixels around every level of a world, `null`
/// when it has none.
pub fn world_bbox(project: &Project, w: &worlds_core::World) -> Value {
    let g = project.grid_size as i64;
    let mut it = w.levels.iter().map(|l| {
        let [x, y] = l.world_px;
        [x, y, x + l.size[0] as i64 * g, y + l.size[1] as i64 * g]
    });
    let Some(first) = it.next() else {
        return Value::Null;
    };
    let [x0, y0, x1, y1] = it.fold(first, |a, b| {
        [
            a[0].min(b[0]),
            a[1].min(b[1]),
            a[2].max(b[2]),
            a[3].max(b[3]),
        ]
    });
    json!([x0, y0, x1 - x0, y1 - y0])
}

/// `main` -> `Main`, `west_wing` -> `West wing`: the display name of a
/// world without one.
fn title_case(slug: &str) -> String {
    let s = slug.replace('_', " ");
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => s,
    }
}

fn levels(project: &Project) -> Value {
    let g = project.grid_size;
    let mut out = Vec::new();
    for w in &project.worlds {
        for l in &w.levels {
            let mut content = Map::new();
            for def in &project.layers {
                let slug = def.slug.as_str();
                let item = match &def.kind {
                    LayerKind::IntGrid(ig) if ig.derived => l
                        .overrides(slug)
                        .map(|o| {
                            let (set, sup) = o.counts();
                            set + sup
                        })
                        .filter(|n| *n > 0)
                        .map(|n| json!({ "overrides": n })),
                    LayerKind::IntGrid(_) => l
                        .int_grid(slug)
                        .map(|grid| grid.cells().iter().filter(|c| **c != 0).count())
                        .filter(|n| *n > 0)
                        .map(|n| json!({ "cells": n })),
                    LayerKind::Tiles(_) => l
                        .tiles_layer(slug)
                        .map(|t| t.tiles.len())
                        .filter(|n| *n > 0)
                        .map(|n| json!({ "tiles": n })),
                    LayerKind::Entities => l
                        .entities_layer(slug)
                        .map(|e| e.entities.len())
                        .filter(|n| *n > 0)
                        .map(|n| json!({ "entities": n })),
                    LayerKind::Auto(_) => None,
                };
                if let Some(item) = item {
                    content.insert(slug.to_string(), item);
                }
            }
            let (open, done) = notes_count(l.notes.iter());
            out.push(json!({
                "slug": l.slug, "name": l.name, "id": l.id, "world": w.slug,
                "world_px": l.world_px, "size": l.size,
                "size_px": [l.size[0] as u64 * g as u64, l.size[1] as u64 * g as u64],
                "depth": l.depth, "tags": l.tags, "content": content,
                "notes": { "open": open, "done": done },
            }));
        }
    }
    Value::Array(out)
}

fn layers(project: &Project) -> Value {
    project
        .layers
        .iter()
        .map(|def| {
            let mut m = Map::new();
            m.insert("slug".into(), json!(def.slug));
            m.insert("name".into(), json!(def.name));
            m.insert("kind".into(), def.kind.name().into());
            m.insert("grid_size".into(), project.layer_grid(def).into());
            match &def.kind {
                LayerKind::IntGrid(ig) => {
                    let values: Vec<Value> = ig
                        .values
                        .iter()
                        .map(|v| {
                            let mut o = json!({ "slug": v.slug, "glyph": v.glyph.to_string(), "color": v.color });
                            if let Some(g) = &v.group {
                                o["group"] = json!(g);
                            }
                            o
                        })
                        .collect();
                    let groups: Vec<Value> = ig
                        .groups
                        .iter()
                        .map(|g| {
                            let mut o = json!({ "slug": g.slug });
                            if let Some(c) = g.glyph {
                                o["glyph"] = c.to_string().into();
                            }
                            o
                        })
                        .collect();
                    m.insert("values".into(), values.into());
                    m.insert("groups".into(), groups.into());
                    if ig.derived {
                        m.insert("derived".into(), true.into());
                        m.insert(
                            "writer".into(),
                            json!(project.derived_writer(def.slug.as_str())),
                        );
                    }
                }
                LayerKind::Tiles(t) => {
                    m.insert("tileset".into(), json!(t.tileset));
                }
                LayerKind::Auto(a) => {
                    m.insert("source".into(), json!(a.source));
                    let n = project
                        .rules_for(def.slug.as_str())
                        .map_or(0, |r| r.groups.len());
                    m.insert("rule_groups".into(), n.into());
                    let passes: std::collections::BTreeSet<u32> = project
                        .rules_for(def.slug.as_str())
                        .into_iter()
                        .flat_map(|r| r.groups.iter().map(|g| g.pass))
                        .collect();
                    m.insert("passes".into(), json!(passes));
                }
                LayerKind::Entities => {}
            }
            Value::Object(m)
        })
        .collect()
}

fn entity_types(project: &Project) -> Value {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for l in project.levels() {
        for e in l.all_entities() {
            *counts.entry(e.ty.as_str()).or_default() += 1;
        }
    }
    project
        .entities
        .iter()
        .map(|d| {
            let mut m = Map::new();
            m.insert("slug".into(), json!(d.slug));
            m.insert("name".into(), json!(d.name));
            if let Some(doc) = &d.doc {
                m.insert("doc".into(), json!(doc));
            }
            if let Some(c) = &d.category {
                m.insert("category".into(), json!(c));
            }
            m.insert("tags".into(), json!(d.tags));
            m.insert("size".into(), json!(d.size));
            m.insert("pivot".into(), json!(d.pivot));
            m.insert("resizable".into(), json!(d.resizable));
            m.insert("color".into(), json!(d.color));
            if let Some(icon) = &d.icon {
                m.insert("icon".into(), json!(icon));
            }
            if let Some(limit) = &d.limit {
                // Spelled out, defaults included (the file leaves them out).
                m.insert(
                    "limit".into(),
                    json!({ "max": limit.max, "scope": limit.scope, "behavior": limit.behavior }),
                );
            }
            m.insert(
                "fields".into(),
                d.fields.iter().map(field_json).collect::<Vec<_>>().into(),
            );
            m.insert(
                "count".into(),
                counts.get(d.slug.as_str()).copied().unwrap_or(0).into(),
            );
            Value::Object(m)
        })
        .collect()
}

fn tilesets(project: &Project, dir: &Path) -> Value {
    project
        .tilesets
        .iter()
        .map(|t| {
            let mut m = Map::new();
            m.insert("slug".into(), json!(t.slug));
            m.insert("image".into(), json!(t.image));
            m.insert("tile_size".into(), json!(t.tile_size));
            if t.spacing != 0 {
                m.insert("spacing".into(), t.spacing.into());
            }
            if t.padding != 0 {
                m.insert("padding".into(), t.padding.into());
            }
            let atlas = image::image_dimensions(dir.join(&t.image))
                .ok()
                .map(|(w, h)| json!([t.columns(w), t.rows(h)]))
                .unwrap_or(Value::Null);
            m.insert("atlas".into(), atlas);
            Value::Object(m)
        })
        .collect()
}
