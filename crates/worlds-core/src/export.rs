//! The game export: `export/levels/<slug>.json` per level,
//! `export/world.json` as the index and `export/enums.json`. The shapes are documented in
//! `docs/format.md` ("export/").
//!
//! The export is derived data. Auto layers have to be baked first, which is
//! `worlds-rules`' job (it depends on this crate, not the other way round), so
//! callers pass each level's bake in as a [`BakedLevel`]: the baked tiles,
//! and the effective grid of every derived layer:
//!
//! ```no_run
//! # use std::collections::BTreeMap;
//! # fn bake(_: &worlds_core::Project, _: &worlds_core::Level) -> worlds_core::export::BakedLevel { todo!() }
//! let project = worlds_core::load("mygame.worlds")?;
//! let baked: BTreeMap<_, _> = project.levels().map(|l| (l.slug.clone(), bake(&project, l))).collect();
//! worlds_core::export::write(&project, "mygame.worlds".as_ref(), "mygame.worlds/export".as_ref(), &baked, true)?;
//! # Ok::<(), worlds_core::Error>(())
//! ```
//!
//! Output is deterministic: the same project and bake give byte-identical
//! files, and files whose bytes didn't change aren't rewritten.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{Value, json};

use crate::format::{Error, Result, json as pretty_json, sync_dir, write_atomic};
use crate::id::Slug;
use crate::model::{
    Annotation, FieldDef, Flip, IntGridLayer, LayerKind, Level, Project, Shape, World,
};

/// Version of the export files' shape.
pub const EXPORT_FORMAT: u32 = 1;

/// One baked autotile.
#[derive(Debug, Clone, PartialEq)]
pub struct BakedTile {
    /// Top-left of the tile in level pixels.
    pub px: [i32; 2],
    pub tileset: Slug,
    pub tile: [u32; 2],
    pub flip: Option<Flip>,
    pub alpha: f64,
}

/// One level's baked auto layers: layer slug -> tiles in draw order (first
/// drawn first). A missing layer exports with no tiles.
pub type BakedLayers = BTreeMap<Slug, Vec<BakedTile>>;

/// One level's bake, what the export needs beyond the project: the tiles of
/// its auto layers and the **effective** grid of every derived layer
/// (overrides applied over what the rules computed).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BakedLevel {
    pub tiles: BakedLayers,
    /// Derived layer slug -> effective grid. Every derived layer of the
    /// project must be here: a missing one is an export error, never a
    /// fallback to the stored overrides.
    pub derived: BTreeMap<Slug, IntGridLayer>,
}

impl From<BakedLayers> for BakedLevel {
    /// Tiles only (no derived grids).
    fn from(tiles: BakedLayers) -> Self {
        BakedLevel {
            tiles,
            derived: BTreeMap::new(),
        }
    }
}

// ---- file shapes (field order is output order) ----

#[derive(Serialize)]
struct LevelFile<'a> {
    format: u32,
    slug: &'a Slug,
    id: &'a str,
    name: &'a str,
    world: &'a Slug,
    /// Top-left corner in world pixels.
    world_px: [i64; 2],
    /// In cells of the project grid.
    size: [u32; 2],
    /// The project grid, in pixels.
    grid_size: u32,
    depth: i32,
    /// The level's tags; left out when there are none.
    #[serde(skip_serializing_if = "<[String]>::is_empty")]
    tags: &'a [String],
    #[serde(skip_serializing_if = "Option::is_none")]
    bg_color: Option<&'a str>,
    /// Every level field, defaults filled in.
    fields: BTreeMap<Slug, Value>,
    /// The level fields `level.json` stores a value for.
    fields_set: Vec<&'a Slug>,
    /// Every int_grid layer, by slug.
    int_grids: BTreeMap<Slug, GridOut<'a>>,
    /// Every tiles and auto layer, bottom first.
    tile_layers: Vec<TileLayerOut<'a>>,
    entities: Vec<EntityOut<'a>>,
    /// Level notes flagged `export`, in world pixels.
    notes: Vec<Annotation>,
}

#[derive(Serialize)]
struct GridOut<'a> {
    /// The layer's display name.
    name: &'a str,
    /// This layer's cell size in pixels.
    grid_size: u32,
    /// A derived layer: `rows` are its effective values.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    derived: bool,
    legend: BTreeMap<char, Option<Slug>>,
    rows: Vec<String>,
}

#[derive(Serialize)]
struct TileLayerOut<'a> {
    layer: &'a Slug,
    /// The layer's display name.
    name: &'a str,
    kind: &'static str,
    /// This layer's cell size in pixels.
    grid_size: u32,
    #[serde(skip_serializing_if = "is_one")]
    opacity: f64,
    tilesets: Vec<TilesetGroup<'a>>,
}

/// The tiles of one layer that come from one tileset at one opacity.
#[derive(Serialize)]
struct TilesetGroup<'a> {
    tileset: &'a Slug,
    /// Relative to the export directory, `/`-separated.
    image: String,
    tile_size: [u32; 2],
    #[serde(skip_serializing_if = "is_zero")]
    spacing: u32,
    #[serde(skip_serializing_if = "is_zero")]
    padding: u32,
    #[serde(skip_serializing_if = "is_one")]
    alpha: f64,
    /// `[x, y, col, row, flip]`: level pixels of the tile's top-left, atlas
    /// tile, flip bits (1 = x, 2 = y). Draw order.
    tiles: Vec<[i64; 5]>,
}

#[derive(Serialize)]
struct EntityOut<'a> {
    id: &'a str,
    #[serde(rename = "type")]
    ty: &'a Slug,
    layer: &'a Slug,
    /// In cells of the entities layer's grid.
    cell: [i32; 2],
    /// The pivot point in level pixels: `(cell + pivot) * layer grid + offset_px`.
    px: [Value; 2],
    /// In cells of the project grid.
    size: [u32; 2],
    pivot: [f64; 2],
    /// Every field of the type, defaults filled in.
    fields: BTreeMap<Slug, Value>,
    /// The fields the instance stores a value for.
    fields_set: Vec<&'a Slug>,
}

#[derive(Serialize)]
struct WorldFile<'a> {
    format: u32,
    name: &'a str,
    grid_size: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    bg_color: Option<&'a str>,
    /// The int_grid layer `open_edges` are read from (`project.json`
    /// `collision_layer`). Absent when unset.
    #[serde(skip_serializing_if = "Option::is_none")]
    collision_layer: Option<&'a Slug>,
    /// `project.json` `blocking_values`; present with `collision_layer`.
    #[serde(skip_serializing_if = "Option::is_none")]
    blocking_values: Option<&'a [Slug]>,
    /// The export's other files, relative to `export/`.
    files: Files,
    entity_types: Vec<EntityType<'a>>,
    worlds: Vec<WorldOut<'a>>,
}

#[derive(Serialize)]
struct Files {
    enums: &'static str,
}

#[derive(Serialize)]
struct EntityType<'a> {
    slug: &'a Slug,
    name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    category: Option<&'a str>,
    tags: &'a [String],
}

#[derive(Serialize)]
struct EnumsFile<'a> {
    format: u32,
    enums: Vec<EnumOut<'a>>,
}

/// An enum or enum value: `name` falls back to the slug.
#[derive(Serialize)]
struct EnumOut<'a> {
    slug: &'a Slug,
    name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    values: Option<Vec<EnumOut<'a>>>,
}

#[derive(Serialize)]
struct WorldOut<'a> {
    slug: &'a Slug,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<&'a str>,
    /// World notes flagged `export`.
    notes: Vec<Annotation>,
    levels: Vec<LevelIndex<'a>>,
}

#[derive(Serialize)]
struct LevelIndex<'a> {
    slug: &'a Slug,
    id: &'a str,
    name: &'a str,
    /// Relative to `export/`.
    file: String,
    /// `[x, y, w, h]` in world pixels.
    rect: [i64; 4],
    depth: i32,
    /// The level's tags; left out when there are none.
    #[serde(skip_serializing_if = "<[String]>::is_empty")]
    tags: &'a [String],
    /// Levels of the same world sharing a stretch of this level's border.
    touching: Vec<Touch<'a>>,
    /// Per edge, runs of border cells that aren't blocking in the collision grid,
    /// as `[first, end)` cell indices along the edge. Absent when the
    /// project has no `collision_layer`; `null` when this level's collision
    /// grid is absent or all empty (the export doesn't describe its seams).
    #[serde(skip_serializing_if = "Option::is_none")]
    open_edges: Option<Option<Edges>>,
}

#[derive(Serialize)]
struct Touch<'a> {
    level: &'a Slug,
    /// This level's edge: `left`, `right`, `top` or `bottom`.
    edge: &'static str,
    /// The shared stretch along that edge, `[start, end)` in world pixels.
    span: [i64; 2],
}

#[derive(Serialize)]
struct Edges {
    left: Vec<[u32; 2]>,
    right: Vec<[u32; 2]>,
    top: Vec<[u32; 2]>,
    bottom: Vec<[u32; 2]>,
}

fn is_one(x: &f64) -> bool {
    *x == 1.0
}

fn is_zero(x: &u32) -> bool {
    *x == 0
}

fn render<T: Serialize>(what: &T) -> String {
    let compact = serde_json::to_string(what).expect("export types always serialize");
    pretty_json::pretty(&compact)
}

// ---- levels ----

/// `export/levels/<slug>.json` for `level`. `project_dir` is the project
/// directory as seen from the export directory (`..` for the usual
/// `<project>/export`), `/`-separated; tileset images are written relative
/// to the export directory through it. Errs when `baked` lacks the grid of
/// a derived layer.
pub fn level_json(
    project: &Project,
    level: &Level,
    baked: &BakedLevel,
    project_dir: &str,
) -> Result<String> {
    level_json_with(project, level, baked, &entity_levels(project), project_dir)
}

/// [`level_json`] with the entity index built once by the caller.
fn level_json_with(
    project: &Project,
    level: &Level,
    baked: &BakedLevel,
    entity_levels: &HashMap<&str, &Slug>,
    project_dir: &str,
) -> Result<String> {
    let world = project
        .worlds
        .iter()
        .find(|w| w.levels.iter().any(|l| l.slug == level.slug))
        .map(|w| &w.slug);
    let fallback = Slug::new("main").expect("valid");
    let g = project.grid_size;

    let mut int_grids = BTreeMap::new();
    for def in project.layers.iter().filter(|l| l.is_int_grid()) {
        let empty = || {
            let [w, h] = project.layer_cells(def, level);
            vec![".".repeat(w as usize); h as usize]
        };
        let grid = if def.is_derived() {
            let Some(g) = baked.derived.get(&def.slug) else {
                return Err(Error::Layout {
                    path: PathBuf::from(format!("levels/{}.json", level.slug)),
                    message: format!(
                        "level {}: derived layer {} has no baked grid (bake the auto layer that \
                         writes it, or pass its effective grid)",
                        level.slug, def.slug
                    ),
                });
            };
            Some(g)
        } else {
            level.int_grid(def.slug.as_str())
        };
        // Validation reports grids that don't fit their definition.
        let rows = grid
            .and_then(|grid| grid.to_rows(def).ok())
            .unwrap_or_else(empty);
        int_grids.insert(
            def.slug.clone(),
            GridOut {
                name: &def.name,
                grid_size: project.layer_grid(def),
                derived: def.is_derived(),
                legend: def.legend(),
                rows,
            },
        );
    }

    let mut tile_layers = Vec::new();
    for def in &project.layers {
        let lg = project.layer_grid(def);
        let tiles: Vec<BakedTile> = match def.kind {
            LayerKind::Tiles(_) => level
                .tiles_layer(def.slug.as_str())
                .map(|t| {
                    t.tiles
                        .iter()
                        .map(|p| BakedTile {
                            px: [(p.cell[0] * lg) as i32, (p.cell[1] * lg) as i32],
                            tileset: p.tileset.clone(),
                            tile: p.tile,
                            flip: p.flip,
                            alpha: p.alpha,
                        })
                        .collect()
                })
                .unwrap_or_default(),
            LayerKind::Auto(_) => baked.tiles.get(&def.slug).cloned().unwrap_or_default(),
            _ => continue,
        };
        tile_layers.push(TileLayerOut {
            layer: &def.slug,
            name: &def.name,
            kind: def.kind.name(),
            grid_size: lg,
            opacity: def.opacity,
            tilesets: group_tiles(project, tiles, project_dir),
        });
    }

    let mut entities = Vec::new();
    for layer in &level.entities {
        let lg = project
            .layer(layer.layer.as_str())
            .map_or(g, |d| project.layer_grid(d));
        for e in &layer.entities {
            let def = project.entity_def(e.ty.as_str());
            let pivot = def.map_or([0.0, 0.0], |d| d.pivot);
            let size = e.size.or(def.map(|d| d.size)).unwrap_or([1, 1]);
            let px = |a: usize| {
                number((e.cell[a] as f64 + pivot[a]) * lg as f64 + e.offset_px[a] as f64)
            };
            let (fields, fields_set) = fields(
                def.map_or(&[][..], |d| &d.fields[..]),
                &e.fields,
                entity_levels,
            );
            entities.push(EntityOut {
                id: e.id.as_str(),
                ty: &e.ty,
                layer: &layer.layer,
                cell: e.cell,
                px: [px(0), px(1)],
                size,
                pivot,
                fields,
                fields_set,
            });
        }
    }

    let notes = level
        .notes
        .iter()
        .filter(|n| is_exported(n))
        .map(|n| exported_note(n, level.world_px))
        .collect();

    let (fields, fields_set) = fields(&project.level_fields, &level.fields, entity_levels);
    Ok(render(&LevelFile {
        format: EXPORT_FORMAT,
        slug: &level.slug,
        id: level.id.as_str(),
        name: &level.name,
        world: world.unwrap_or(&fallback),
        world_px: level.world_px,
        size: level.size,
        grid_size: g,
        depth: level.depth,
        tags: &level.tags,
        bg_color: level
            .bg_color
            .as_ref()
            .or(project.bg_color.as_ref())
            .map(|c| c.as_str()),
        fields,
        fields_set,
        int_grids,
        tile_layers,
        entities,
        notes,
    }))
}

/// Split tiles (in draw order) into runs of consecutive tiles sharing a
/// tileset and alpha. Drawing the groups in order, each group's tiles in
/// order, is exactly the original draw order; a tileset that comes back
/// after another starts a new group.
fn group_tiles<'a>(
    project: &'a Project,
    tiles: Vec<BakedTile>,
    project_dir: &str,
) -> Vec<TilesetGroup<'a>> {
    let mut groups: Vec<TilesetGroup> = Vec::new();
    for t in tiles {
        let Some(def) = project.tileset(t.tileset.as_str()) else {
            continue; // validation reports unknown tilesets
        };
        let i = match groups
            .last()
            .filter(|gr| *gr.tileset == t.tileset && gr.alpha == t.alpha)
        {
            Some(_) => groups.len() - 1,
            None => {
                groups.push(TilesetGroup {
                    tileset: &def.slug,
                    image: join_path(project_dir, &def.image),
                    tile_size: def.tile_size,
                    spacing: def.spacing,
                    padding: def.padding,
                    alpha: t.alpha,
                    tiles: Vec::new(),
                });
                groups.len() - 1
            }
        };
        let flip = Flip::bits_of(t.flip) as i64;
        groups[i].tiles.push([
            t.px[0] as i64,
            t.px[1] as i64,
            t.tile[0] as i64,
            t.tile[1] as i64,
            flip,
        ]);
    }
    groups
}

/// An integer-valued float as a JSON integer, so `16.0` exports as `16`.
fn number(x: f64) -> Value {
    if x.fract() == 0.0 && x.abs() < 9.0e15 {
        json!(x as i64)
    } else {
        json!(x)
    }
}

/// Entity id -> level slug, for resolving `entity_ref` fields.
fn entity_levels(project: &Project) -> HashMap<&str, &Slug> {
    project
        .levels()
        .flat_map(|l| l.all_entities().map(move |e| (e.id.as_str(), &l.slug)))
        .collect()
}

/// Every field of `defs` with its value or default (`null` if none, `[]`
/// for arrays), and the fields `values` stores, sorted. `entity_ref` values
/// become `{ "level", "id" }`, `level` being the target's level (`null`
/// only when no entity has that id).
fn fields<'d>(
    defs: &'d [FieldDef],
    values: &BTreeMap<Slug, Value>,
    entity_levels: &HashMap<&str, &Slug>,
) -> (BTreeMap<Slug, Value>, Vec<&'d Slug>) {
    let mut set: Vec<&Slug> = defs
        .iter()
        .filter(|d| values.contains_key(&d.slug))
        .map(|d| &d.slug)
        .collect();
    set.sort();
    set.dedup();
    let all = defs
        .iter()
        .map(|d| {
            let v = values
                .get(&d.slug)
                .or(d.default.as_ref())
                .cloned()
                .unwrap_or(if d.array { json!([]) } else { Value::Null });
            let v = if d.ty.is_entity_ref() {
                let resolve = |v: &Value| match v.as_str() {
                    Some(id) => json!({ "level": entity_levels.get(id), "id": id }),
                    None => v.clone(),
                };
                match &v {
                    Value::Array(a) => Value::Array(a.iter().map(resolve).collect()),
                    other => resolve(other),
                }
            } else {
                v
            };
            (d.slug.clone(), v)
        })
        .collect();
    (all, set)
}

/// Whether an annotation goes into the export: flagged `export` and not a
/// guide (guides never export; validation says so when one is flagged).
fn is_exported(n: &Annotation) -> bool {
    n.export && !matches!(n.shape, Shape::Guide { .. })
}

/// A note as exported: world pixels, editor-only flags dropped.
fn exported_note(n: &Annotation, origin: [i64; 2]) -> Annotation {
    let mut n = n.clone();
    n.shape.translate(origin);
    n.export = false;
    n.locked = false;
    n
}

// ---- world index ----

/// `export/world.json`.
pub fn world_json(project: &Project) -> String {
    let g = project.grid_size as i64;
    // Validation reports a collision_layer that isn't an int_grid layer.
    let collision = project
        .collision_layer
        .as_ref()
        .and_then(|s| project.layer(s.as_str()))
        .filter(|l| l.is_int_grid());
    let blocking_values = project.effective_blocking_values();
    let blocking: HashSet<u16> = collision
        .map(|c| {
            blocking_values
                .iter()
                .filter_map(|v| c.value_index(v.as_str()))
                .collect()
        })
        .unwrap_or_default();
    let rect = |l: &Level| {
        [
            l.world_px[0],
            l.world_px[1],
            l.size[0] as i64 * g,
            l.size[1] as i64 * g,
        ]
    };
    let worlds = project
        .worlds
        .iter()
        .map(|w: &World| WorldOut {
            slug: &w.slug,
            name: w.name.as_deref(),
            notes: w
                .notes
                .iter()
                .filter(|n| is_exported(n))
                .map(|n| exported_note(n, [0, 0]))
                .collect(),
            levels: w
                .levels
                .iter()
                .map(|l| LevelIndex {
                    slug: &l.slug,
                    id: l.id.as_str(),
                    name: &l.name,
                    file: format!("levels/{}.json", l.slug),
                    rect: rect(l),
                    depth: l.depth,
                    tags: &l.tags,
                    touching: w
                        .levels
                        .iter()
                        .filter(|o| o.slug != l.slug)
                        .filter_map(|o| {
                            touch(rect(l), rect(o)).map(|(edge, span)| Touch {
                                level: &o.slug,
                                edge,
                                span,
                            })
                        })
                        .collect(),
                    open_edges: collision.map(|c| open_edges(project, l, c, &blocking)),
                })
                .collect(),
        })
        .collect();
    render(&WorldFile {
        format: EXPORT_FORMAT,
        name: &project.name,
        grid_size: project.grid_size,
        bg_color: project.bg_color.as_ref().map(|c| c.as_str()),
        collision_layer: collision.map(|l| &l.slug),
        blocking_values: collision.map(|_| &blocking_values[..]),
        files: Files {
            enums: "enums.json",
        },
        entity_types: project
            .entities
            .iter()
            .map(|d| EntityType {
                slug: &d.slug,
                name: &d.name,
                category: d.category.as_deref(),
                tags: &d.tags,
            })
            .collect(),
        worlds,
    })
}

/// `export/enums.json`: every project enum and its values, with display
/// names (the slug where none is set).
pub fn enums_json(project: &Project) -> String {
    let enums = project
        .enums
        .iter()
        .map(|e| EnumOut {
            slug: &e.slug,
            name: e.name.as_deref().unwrap_or(e.slug.as_str()),
            values: Some(
                e.values
                    .iter()
                    .map(|v| EnumOut {
                        slug: &v.slug,
                        name: v.name.as_deref().unwrap_or(v.slug.as_str()),
                        values: None,
                    })
                    .collect(),
            ),
        })
        .collect();
    render(&EnumsFile {
        format: EXPORT_FORMAT,
        enums,
    })
}

/// Which edge of `a` touches `b`, and the shared span, if they share a
/// stretch of border of positive length.
fn touch(a: [i64; 4], b: [i64; 4]) -> Option<(&'static str, [i64; 2])> {
    let [ax, ay, aw, ah] = a;
    let [bx, by, bw, bh] = b;
    let overlap = |a0: i64, a1: i64, b0: i64, b1: i64| {
        let (s, e) = (a0.max(b0), a1.min(b1));
        (s < e).then_some([s, e])
    };
    if ax == bx + bw {
        overlap(ay, ay + ah, by, by + bh).map(|s| ("left", s))
    } else if ax + aw == bx {
        overlap(ay, ay + ah, by, by + bh).map(|s| ("right", s))
    } else if ay == by + bh {
        overlap(ax, ax + aw, bx, bx + bw).map(|s| ("top", s))
    } else if ay + ah == by {
        overlap(ax, ax + aw, bx, bx + bw).map(|s| ("bottom", s))
    } else {
        None
    }
}

/// Runs of border cells whose value isn't blocking (empty included), in
/// cells of `layer`'s grid. `blocking` holds value indices. `None` when the
/// level has no collision content at all: an unpainted level says nothing
/// about its seams.
fn open_edges(
    project: &Project,
    level: &Level,
    layer: &crate::model::LayerDef,
    blocking: &HashSet<u16>,
) -> Option<Edges> {
    let [w, h] = project.layer_cells(layer, level);
    let grid = level
        .int_grid(layer.slug.as_str())
        .filter(|g| !g.is_empty())?;
    let empty = |x: u32, y: u32| !blocking.contains(&grid.index_at(x, y).unwrap_or(0));
    let runs = |n: u32, at: &dyn Fn(u32) -> bool| {
        let mut out = Vec::new();
        let mut start = None;
        for i in 0..=n {
            match (i < n && at(i), start) {
                (true, None) => start = Some(i),
                (false, Some(s)) => {
                    out.push([s, i]);
                    start = None;
                }
                _ => {}
            }
        }
        out
    };
    Some(Edges {
        left: runs(h, &|y| empty(0, y)),
        right: runs(h, &|y| empty(w.saturating_sub(1), y)),
        top: runs(w, &|x| empty(x, 0)),
        bottom: runs(w, &|x| empty(x, h.saturating_sub(1))),
    })
}

// ---- writing ----

/// The export files for the levels in `baked`, rendered in memory exactly
/// as [`write`] would write them into `export_dir`: `world.json`,
/// `enums.json`, then `levels/<slug>.json` in project order, each with its
/// path relative to `export_dir`. Tileset image paths are made relative to
/// `export_dir` (as for [`write`]), so `worlds-cli check-export --fresh` can
/// compare a fresh bake with an export directory byte for byte without
/// writing anything. Nothing is read or written but the two directories'
/// paths (resolved when they exist). Errs when a level's bake lacks a
/// derived layer's grid.
pub fn render_files(
    project: &Project,
    project_dir: &Path,
    export_dir: &Path,
    baked: &BTreeMap<Slug, BakedLevel>,
) -> Result<Vec<(PathBuf, String)>> {
    let from_export = relative_dir(export_dir, project_dir);
    let entity_levels = entity_levels(project);
    let mut out = vec![
        (PathBuf::from("world.json"), world_json(project)),
        (PathBuf::from("enums.json"), enums_json(project)),
    ];
    for level in project.levels() {
        let Some(b) = baked.get(&level.slug) else {
            continue;
        };
        out.push((
            PathBuf::from(format!("levels/{}.json", level.slug)),
            level_json_with(project, level, b, &entity_levels, &from_export)?,
        ));
    }
    Ok(out)
}

/// What [`write_counted`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Written {
    /// Every file written or kept, relative to the export directory.
    pub files: Vec<PathBuf>,
    /// How many of `files` had different bytes on disk (or were missing) and
    /// were written.
    pub changed: usize,
    /// Level files the prune removed.
    pub removed: usize,
}

/// Write `world.json`, `enums.json` and `levels/<slug>.json` into
/// `export_dir` (normally `<project_dir>/export`) for every level in `baked`
/// (keyed by level slug). Tileset image paths are rewritten to be relative
/// to `export_dir`, so `project_dir` must be where `project` was loaded from.
/// With `prune`, level files for levels not in `baked` are removed (use it
/// for a full export). Files whose bytes didn't change are left untouched.
/// Returns the files written or kept, relative to `export_dir`. A level
/// whose bake lacks a derived layer's grid is an error, before any file is
/// written.
pub fn write(
    project: &Project,
    project_dir: &Path,
    export_dir: &Path,
    baked: &BTreeMap<Slug, BakedLevel>,
    prune: bool,
) -> Result<Vec<PathBuf>> {
    Ok(write_counted(project, project_dir, export_dir, baked, prune)?.files)
}

/// [`write`], also counting the files it changed and the level files it
/// pruned (what `worlds-cli bake` reports).
pub fn write_counted(
    project: &Project,
    project_dir: &Path,
    export_dir: &Path,
    baked: &BTreeMap<Slug, BakedLevel>,
    prune: bool,
) -> Result<Written> {
    fs::create_dir_all(export_dir).map_err(|source| Error::Io {
        path: export_dir.to_path_buf(),
        source,
    })?;
    // Rendered first, so a bake that lacks a derived grid writes nothing.
    let files = render_files(project, project_dir, export_dir, baked)?;
    let mut out = Written::default();
    for (rel, text) in files {
        let path = export_dir.join(&rel);
        if !fs::read(&path).is_ok_and(|old| old == text.as_bytes()) {
            write_atomic(&path, text.as_bytes())?;
            out.changed += 1;
        }
        out.files.push(rel);
    }
    let io = |path: &Path| {
        let path = path.to_path_buf();
        move |source| Error::Io { path, source }
    };
    // Each write syncs its own directory; also make `levels/` itself (when
    // just created) durable in the export directory.
    sync_dir(export_dir).map_err(io(export_dir))?;
    if prune {
        let levels = export_dir.join("levels");
        if let Ok(rd) = fs::read_dir(&levels) {
            for entry in rd {
                let path = entry.map_err(io(&levels))?.path();
                let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned());
                let keep = stem.is_some_and(|s| baked.keys().any(|k| k.as_str() == s));
                if path.extension().is_some_and(|e| e == "json") && !keep {
                    fs::remove_file(&path).map_err(io(&path))?;
                    out.removed += 1;
                }
            }
        }
        // Make the deletes durable, as `save` does.
        if out.removed > 0 {
            sync_dir(&levels).map_err(io(&levels))?;
        }
    }
    Ok(out)
}

/// `to` as seen from `from`, `/`-separated (`..` for `<project>/export` ->
/// `<project>`). Both are resolved through the file system when they
/// exist; with no common root (another drive) it is `to`, absolute.
fn relative_dir(from: &Path, to: &Path) -> String {
    let (from, to) = (resolve(from), resolve(to));
    let (a, b): (Vec<_>, Vec<_>) = (from.components().collect(), to.components().collect());
    let common = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    if common == 0 {
        return to.to_string_lossy().replace('\\', "/");
    }
    let mut parts: Vec<String> = vec!["..".into(); a.len() - common];
    parts.extend(
        b[common..]
            .iter()
            .map(|c| c.as_os_str().to_string_lossy().into_owned()),
    );
    if parts.is_empty() {
        ".".into()
    } else {
        parts.join("/")
    }
}

/// `p` made absolute, with symlinks resolved as far as it exists: the
/// deepest existing ancestor is canonicalized and the rest appended. So a
/// directory not created yet still compares equal to its existing parents
/// when they sit behind a symlink (macOS's `/var` is `/private/var`).
fn resolve(p: &Path) -> std::path::PathBuf {
    let abs = std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
    let mut rest = Vec::new();
    let mut cur = abs.as_path();
    loop {
        if let Ok(real) = fs::canonicalize(cur) {
            return rest.iter().rev().fold(real, |acc, part| acc.join(part));
        }
        match (cur.parent(), cur.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_os_string());
                cur = parent;
            }
            _ => return abs,
        }
    }
}

/// `path` (relative, `/`-separated) joined onto `dir`, with `.` and `..`
/// folded where they can be: `join_path("..", "../art/a.png")` is
/// `../../art/a.png`, `join_path("..", "art/a.png")` is `../art/a.png`. An
/// absolute `path` is kept as it is.
fn join_path(dir: &str, path: &str) -> String {
    if path.starts_with('/') || Path::new(path).is_absolute() {
        return path.to_string();
    }
    let mut parts: Vec<&str> = Vec::new();
    for (i, seg) in dir.split('/').chain(path.split('/')).enumerate() {
        match seg {
            "." => {}
            "" if i > 0 => {}
            ".." => match parts.last() {
                Some(&"..") | None => parts.push(".."),
                Some(&"") => {} // the parent of / is /
                Some(_) => {
                    parts.pop();
                }
            },
            s => parts.push(s),
        }
    }
    match parts.as_slice() {
        [] => ".".into(),
        [""] => "/".into(),
        _ => parts.join("/"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn touching_edges() {
        let a = [0, 0, 100, 50];
        assert_eq!(touch(a, [100, 20, 10, 100]), Some(("right", [20, 50])));
        assert_eq!(touch(a, [-10, 0, 10, 10]), Some(("left", [0, 10])));
        assert_eq!(touch(a, [50, 50, 100, 10]), Some(("bottom", [50, 100])));
        assert_eq!(touch(a, [0, -5, 1, 5]), Some(("top", [0, 1])));
        // Corner contact only.
        assert_eq!(touch(a, [100, 50, 10, 10]), None);
        assert_eq!(touch(a, [200, 0, 10, 10]), None);
    }

    use crate::id::slug;
    use crate::model::{IntGridDef, LayerDef, TilesetDef};

    fn tileset(s: &str) -> TilesetDef {
        TilesetDef {
            slug: slug(s),
            image: format!("{s}.png"),
            tile_size: [16, 16],
            spacing: 0,
            padding: 0,
        }
    }

    #[test]
    fn tileset_groups_keep_draw_order() {
        let mut p = Project::new("t");
        p.tilesets = vec![tileset("a"), tileset("b")];
        let tile = |ts: &str, x: i32| BakedTile {
            px: [x, 0],
            tileset: slug(ts),
            tile: [0, 0],
            flip: None,
            alpha: 1.0,
        };
        // a, a, b, a: the last `a` is drawn over `b`, so it can't join the first group.
        let groups = group_tiles(
            &p,
            vec![tile("a", 0), tile("a", 1), tile("b", 2), tile("a", 3)],
            "..",
        );
        let shape: Vec<(&str, Vec<i64>)> = groups
            .iter()
            .map(|g| (g.tileset.as_str(), g.tiles.iter().map(|t| t[0]).collect()))
            .collect();
        assert_eq!(
            shape,
            [("a", vec![0, 1]), ("b", vec![2]), ("a", vec![3])],
            "groups are consecutive runs"
        );
    }

    #[test]
    fn open_edges_come_from_the_collision_layer() {
        let mut p = Project::new("t");
        p.grid_size = 16;
        p.layers = vec![LayerDef::new(
            slug("walls"),
            "Walls",
            LayerKind::IntGrid(IntGridDef::default()),
        )];
        p.worlds[0].levels.push(Level::new(slug("a"), "A", [4, 3]));
        // No collision_layer: no open_edges, and no guessing from slugs.
        let v: Value = serde_json::from_str(&world_json(&p)).unwrap();
        assert!(v.get("collision_layer").is_none());
        assert!(v["worlds"][0]["levels"][0].get("open_edges").is_none());

        p.collision_layer = Some(slug("walls"));
        let v: Value = serde_json::from_str(&world_json(&p)).unwrap();
        assert_eq!(v["collision_layer"], "walls");
        // No collision content in the level: null, not "all open".
        assert_eq!(v["worlds"][0]["levels"][0]["open_edges"], Value::Null);
        assert!(v["worlds"][0]["levels"][0].get("open_edges").is_some());
        // An all-empty grid is the same as none.
        let def = p.layers[0].clone();
        p.worlds[0].levels[0]
            .int_grids
            .push(crate::model::IntGridLayer::new_empty(&def, [4, 3]));
        let v: Value = serde_json::from_str(&world_json(&p)).unwrap();
        assert_eq!(v["worlds"][0]["levels"][0]["open_edges"], Value::Null);
        // Any content: edges are described (nothing blocks here).
        p.worlds[0].levels[0].int_grids[0].set_index(1, 1, 1);
        let v: Value = serde_json::from_str(&world_json(&p)).unwrap();
        assert_eq!(
            v["worlds"][0]["levels"][0]["open_edges"]["top"],
            json!([[0, 4]])
        );
    }

    #[test]
    fn paths_from_the_export_root() {
        assert_eq!(join_path("..", "art/a.png"), "../art/a.png");
        assert_eq!(join_path("..", "../art/a.png"), "../../art/a.png");
        assert_eq!(join_path(".", "./art/../b.png"), "b.png");
        assert_eq!(join_path("../x/p", "../../art/a.png"), "../art/a.png");
        assert_eq!(join_path("/abs/p", "../../../a.png"), "/a.png");
        assert_eq!(join_path("..", "/abs/a.png"), "/abs/a.png");

        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("a/b/p.worlds");
        let export = proj.join("export");
        std::fs::create_dir_all(&export).unwrap();
        assert_eq!(relative_dir(&export, &proj), "..");
        assert_eq!(relative_dir(&proj, &proj), ".");
        assert_eq!(relative_dir(tmp.path(), &proj), "a/b/p.worlds");
        // Not created yet: still resolved lexically.
        assert_eq!(
            relative_dir(&tmp.path().join("a/out/x"), &proj),
            "../../b/p.worlds"
        );
        // Behind a symlink (as macOS's /var is), the part that exists is
        // resolved and the rest kept.
        #[cfg(unix)]
        {
            let link = tmp.path().join("link");
            std::os::unix::fs::symlink(tmp.path().join("a"), &link).unwrap();
            assert_eq!(relative_dir(&link.join("out/x"), &proj), "../../b/p.worlds");
            assert_eq!(relative_dir(&export, &link.join("b/p.worlds")), "..");
        }
    }

    #[test]
    fn numbers_and_notes() {
        assert_eq!(number(16.0), json!(16));
        assert_eq!(number(-8.5), json!(-8.5));
        let mut n = Annotation::new(crate::model::Shape::Region {
            points: vec![],
            rect: Some([1, 2, 3, 4]),
        });
        n.export = true;
        let e = exported_note(&n, [100, 200]);
        assert_eq!(
            e.shape,
            crate::model::Shape::Region {
                points: vec![],
                rect: Some([101, 202, 3, 4])
            }
        );
        assert!(!e.export);
    }
}
