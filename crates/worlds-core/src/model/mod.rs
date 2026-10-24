//! The in-memory project document. Plain data, no interior mutability.
//!
//! Each struct that maps to a file derives serde for that file's body; fields
//! loaded from other files are `#[serde(skip)]`. Directory-backed lists
//! (entity defs, rule sets, worlds, levels, per-level layers) are kept sorted
//! by slug, which is the order `load` produces. Entity instances and notes
//! keep insertion order; tile placements are sorted by row then column.

mod annotation;
mod common;
mod connector;
mod defs;
mod derived;
mod entity;
mod expand;
mod layer;
mod reference;
mod rules;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub use annotation::{
    Annotation, ConnectorStyle, Endpoint, GuideGroup, Link, NoteStatus, Shape, bbox_of, parse_link,
};
pub use common::{Color, Flip, TileRef};
pub use connector::{Anchor, connector_polyline, nearest_edge_point};
pub use defs::{EnumDef, EnumValue, TilesetDef};
pub use derived::DerivedCycle;
pub use entity::{
    EntitiesLayer, EntityDef, EntityInstance, EnumSource, FieldDef, FieldDisplay, FieldType, Limit,
    LimitBehavior, LimitScope,
};
pub use expand::{ExpandError, ExpandedGroup, ExpandedRule};
pub use layer::{
    AutoDef, DerivedOverrides, IntGridDef, IntGridGroup, IntGridLayer, IntGridValue, LayerDef,
    LayerKind, LayerState, Override, RESERVED_GLYPHS, TilePlacement, TilesDef, TilesLayer,
    glyph_problem,
};
pub use reference::Reference;
pub use rules::{
    ANCHOR_GLYPH, Align, CellMatch, CellTest, Checker, Draw, Edge, GroupMode, Legend,
    LevelFieldRequirement, LevelTagRequirement, Macros, MatchSelect, Modulo, OutOfBounds, Output,
    Pattern, PerAxis, Perlin, Place, PlaceAt, RandomOffset, Region, RegionSelect, RequirementMode,
    Rows, Rule, RuleEntry, RuleGroup, RuleOutput, RuleSet, Select, Spacing, Stamp, StampOutput,
    Symmetry, Template, TemplateOutput, TemplateRef, TemplateRule, TemplateTilesOutput, TileAlt,
    TileMode, TilesOutput, Write,
};

use crate::id::{Id, Slug};

/// A whole project. The serialized part is `project.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Project {
    pub name: String,
    /// Cell size in pixels. Level sizes and cell coordinates are in these cells.
    pub grid_size: u32,
    /// Project-wide seed mixed into every rule's random choices.
    pub seed: i64,
    /// Default level background.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bg_color: Option<Color>,
    /// The `int_grid` layer whose values are the collision kinds. The game
    /// export reads level `open_edges` from it; unset, there are none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collision_layer: Option<Slug>,
    /// Values of the collision layer that block movement. The export's
    /// `open_edges` are the border cells holding anything else (empty
    /// included). `None` (left out of the file) is not the same as `[]`:
    /// loading fills an absent list from [`DEFAULT_BLOCKING_VALUES`] (see
    /// [`Project::default_blocking_values`]); `[]` means nothing blocks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocking_values: Option<Vec<Slug>>,
    /// Fields every level has (values in each `level.json`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub level_fields: Vec<FieldDef>,
    /// Size in cells of a level made with the editor's New Level command.
    /// Unset, the editor uses the most common level size in the project.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_level_size: Option<[u32; 2]>,
    /// The editor writes `export/` on every save (as `worlds-cli bake` does).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub export_on_save: bool,
    /// `worlds`: the display order of the worlds, by slug. Optional: left
    /// out, worlds are in slug order. A world it doesn't list comes after
    /// the listed ones (in slug order), and validation reports it; so does
    /// a listed slug with no world. The world commands write it as soon as
    /// there is more than one world.
    #[serde(rename = "worlds", default, skip_serializing_if = "Vec::is_empty")]
    pub world_order: Vec<Slug>,

    /// `layers.json`, in draw order: first is drawn first (bottom).
    #[serde(skip)]
    pub layers: Vec<LayerDef>,
    /// `tilesets.json`.
    #[serde(skip)]
    pub tilesets: Vec<TilesetDef>,
    /// `enums.json`.
    #[serde(skip)]
    pub enums: Vec<EnumDef>,
    /// `entities/<slug>.json`, sorted by slug.
    #[serde(skip)]
    pub entities: Vec<EntityDef>,
    /// `rules/<layer>.json`, sorted by layer slug.
    #[serde(skip)]
    pub rules: Vec<RuleSet>,
    /// `rules/macros.json`, if present.
    #[serde(skip)]
    pub macros: Option<Macros>,
    /// `worlds/<slug>/`, in display order: the order of
    /// [`Self::world_order`], then unlisted worlds by slug.
    #[serde(skip)]
    pub worlds: Vec<World>,

    /// The project files this document knows exist on disk (loaded, or
    /// written by a save). `save` only deletes files listed here. Not part
    /// of the document: ignored by `==`.
    #[serde(skip)]
    pub files: ProjectFiles,
}

/// The managed files of a project directory that a [`Project`] was loaded
/// from or saved to, relative to that directory. `save` deletes a file only
/// if it is listed here and the project no longer produces it, so files it
/// never saw (an agent's new level written while the editor was open, a
/// stray backup) are never removed.
///
/// Also holds the dirty set: files (or level directories) that commands
/// changed since the last load or save, fed by [`Project::apply`] and
/// consumed by [`crate::format::save_dirty`].
///
/// Also remembers, for every known file, a hash of its bytes on disk as of
/// the load or the last save that wrote it ([`Self::base_hash`]): what the
/// autosave records as each dirty file's `base`, so recovery can tell a file
/// that changed on disk since (a conflict) from one that didn't.
///
/// Always compares equal, so it doesn't affect `Project` equality.
#[derive(Debug, Clone, Default)]
pub struct ProjectFiles {
    root: Option<PathBuf>,
    paths: BTreeSet<PathBuf>,
    dirty: BTreeSet<PathBuf>,
    bases: BTreeMap<PathBuf, u64>,
}

impl PartialEq for ProjectFiles {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl ProjectFiles {
    /// Files under `root` (paths relative to it).
    pub fn new(root: &Path, paths: impl IntoIterator<Item = PathBuf>) -> Self {
        ProjectFiles {
            root: Some(canonical(root)),
            paths: paths.into_iter().collect(),
            dirty: BTreeSet::new(),
            bases: BTreeMap::new(),
        }
    }

    /// Files under `root` with the hash of each one's bytes on disk.
    pub fn with_bases(root: &Path, files: impl IntoIterator<Item = (PathBuf, u64)>) -> Self {
        let bases: BTreeMap<PathBuf, u64> = files.into_iter().collect();
        ProjectFiles {
            root: Some(canonical(root)),
            paths: bases.keys().cloned().collect(),
            dirty: BTreeSet::new(),
            bases,
        }
    }

    /// The known files if they belong to `dir`, else none.
    pub fn known_in(&self, dir: &Path) -> Option<&BTreeSet<PathBuf>> {
        (self.root.as_deref() == Some(canonical(dir).as_path())).then_some(&self.paths)
    }

    /// The directory the known files are relative to, if any.
    pub fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }

    /// The known files (relative to [`Self::root`]).
    pub fn known(&self) -> &BTreeSet<PathBuf> {
        &self.paths
    }

    /// The hash ([`crate::autosave::hash`]) of `rel`'s bytes on disk as of
    /// the load or the last save that wrote it; `None` for a file that
    /// wasn't there (or isn't known).
    pub fn base_hash(&self, rel: &Path) -> Option<u64> {
        self.bases.get(rel).copied()
    }

    /// Record what disk now holds for `rel` (its hash, or `None` if it
    /// doesn't exist), for a caller that read the file itself (the editor's
    /// watcher applying a changed file).
    pub fn set_base(&mut self, rel: impl Into<PathBuf>, hash: Option<u64>) {
        let rel = rel.into();
        match hash {
            Some(h) => {
                self.bases.insert(rel.clone(), h);
                self.paths.insert(rel);
            }
            None => {
                self.bases.remove(&rel);
                self.paths.remove(&rel);
            }
        }
    }

    /// Project-relative paths changed by commands since the last load or
    /// save. A path without an extension is a whole level directory.
    pub fn dirty(&self) -> &BTreeSet<PathBuf> {
        &self.dirty
    }

    pub(crate) fn mark_dirty(&mut self, rel: PathBuf) {
        self.dirty.insert(rel);
    }

    pub(crate) fn take_dirty(&mut self) -> BTreeSet<PathBuf> {
        std::mem::take(&mut self.dirty)
    }

    pub(crate) fn restore_dirty(&mut self, dirty: BTreeSet<PathBuf>) {
        self.dirty.extend(dirty);
    }

    /// After an incremental save: `written` (with the hash of the bytes
    /// written) now exist, `removed` don't.
    pub(crate) fn saved(
        &mut self,
        written: impl IntoIterator<Item = (PathBuf, u64)>,
        removed: &BTreeSet<PathBuf>,
    ) {
        for (p, h) in written {
            self.paths.insert(p.clone());
            self.bases.insert(p, h);
        }
        self.paths.retain(|p| !removed.contains(p));
        self.bases.retain(|p, _| !removed.contains(p));
    }
}

fn canonical(p: &Path) -> PathBuf {
    std::fs::canonicalize(p)
        .or_else(|_| std::path::absolute(p))
        .unwrap_or_else(|_| p.to_path_buf())
}

/// Collision value slugs that block movement when a project doesn't say:
/// just `solid`. Used for an absent `blocking_values` (on load and LDtk
/// import); a project with other wall kinds lists them itself.
pub const DEFAULT_BLOCKING_VALUES: [&str; 1] = ["solid"];

impl Project {
    /// The values of the collision layer named in [`DEFAULT_BLOCKING_VALUES`],
    /// in the layer's order; `None` if there is no `int_grid` collision
    /// layer or it has none of them.
    pub fn default_blocking_values(&self) -> Option<Vec<Slug>> {
        let layer = self
            .collision_layer
            .as_ref()
            .and_then(|c| self.layer(c.as_str()))
            .filter(|l| l.is_int_grid())?;
        let found: Vec<Slug> = layer
            .values()
            .iter()
            .filter(|v| DEFAULT_BLOCKING_VALUES.contains(&v.slug.as_str()))
            .map(|v| v.slug.clone())
            .collect();
        (!found.is_empty()).then_some(found)
    }

    /// The blocking values the export uses: the stored list, or for an
    /// absent one, [`Self::default_blocking_values`] (else none).
    pub fn effective_blocking_values(&self) -> Vec<Slug> {
        match &self.blocking_values {
            Some(v) => v.clone(),
            None => self.default_blocking_values().unwrap_or_default(),
        }
    }
    /// An empty project with a 16 px grid, a random seed and one empty world `main`.
    pub fn new(name: impl Into<String>) -> Self {
        Project {
            name: name.into(),
            grid_size: 16,
            seed: rand::random::<i64>(),
            bg_color: None,
            collision_layer: None,
            blocking_values: None,
            level_fields: Vec::new(),
            default_level_size: None,
            export_on_save: false,
            world_order: Vec::new(),
            layers: Vec::new(),
            tilesets: Vec::new(),
            enums: Vec::new(),
            entities: Vec::new(),
            rules: Vec::new(),
            macros: None,
            worlds: vec![World::new(crate::id::slug("main"))],
            files: ProjectFiles::default(),
        }
    }

    /// Cell size in pixels of a layer: its `grid_size`, else the project's.
    /// An `auto` layer uses its source layer's grid.
    pub fn layer_grid(&self, def: &LayerDef) -> u32 {
        if let Some(a) = def.auto()
            && let Some(src) = self.layer(a.source.as_str())
            && !src.is_auto()
        {
            return self.layer_grid(src);
        }
        def.grid_size.unwrap_or(self.grid_size).max(1)
    }

    /// A level's size in cells of `def`'s grid: the level's pixel size
    /// divided by the layer grid, rounded up.
    pub fn layer_cells(&self, def: &LayerDef, level: &Level) -> [u32; 2] {
        let g = self.layer_grid(def) as u64;
        let px = |n: u32| (n as u64 * self.grid_size as u64).div_ceil(g) as u32;
        [px(level.size[0]), px(level.size[1])]
    }

    pub fn layer(&self, slug: &str) -> Option<&LayerDef> {
        self.layers.iter().find(|l| l.slug == slug)
    }

    pub fn tileset(&self, slug: &str) -> Option<&TilesetDef> {
        self.tilesets.iter().find(|t| t.slug == slug)
    }

    pub fn enum_def(&self, slug: &str) -> Option<&EnumDef> {
        self.enums.iter().find(|e| e.slug == slug)
    }

    pub fn entity_def(&self, slug: &str) -> Option<&EntityDef> {
        self.entities.iter().find(|e| e.slug == slug)
    }

    /// The rule set of an `auto` layer.
    pub fn rules_for(&self, layer: &str) -> Option<&RuleSet> {
        self.rules.iter().find(|r| r.layer == layer)
    }

    pub fn world(&self, slug: &str) -> Option<&World> {
        self.worlds.iter().find(|w| w.slug == slug)
    }

    /// The world holding the level `slug`, if any.
    pub fn world_of_level(&self, slug: &str) -> Option<&World> {
        self.worlds.iter().find(|w| w.level(slug).is_some())
    }

    /// Where a world sorts in the display order: its position in
    /// [`Self::world_order`], or after every listed one.
    fn world_rank(&self, slug: &Slug) -> (usize, Option<Slug>) {
        match self.world_order.iter().position(|s| s == slug) {
            Some(i) => (i, None),
            None => (usize::MAX, Some(slug.clone())),
        }
    }

    /// Put [`Self::worlds`] in display order (see [`Self::world_order`]).
    pub fn sort_worlds(&mut self) {
        let mut worlds = std::mem::take(&mut self.worlds);
        worlds.sort_by_cached_key(|w| self.world_rank(&w.slug));
        self.worlds = worlds;
    }

    /// Find a level by slug in any world.
    pub fn level(&self, slug: &str) -> Option<&Level> {
        self.levels().find(|l| l.slug == slug)
    }

    pub fn levels(&self) -> impl Iterator<Item = &Level> {
        self.worlds.iter().flat_map(|w| w.levels.iter())
    }

    /// Every annotation with where it lives: each world's notes, then its
    /// levels' (in order), world by world.
    pub fn notes(&self) -> impl Iterator<Item = NoteAt<'_>> {
        self.worlds.iter().flat_map(|w| {
            w.notes
                .iter()
                .map(move |n| NoteAt {
                    world: w,
                    level: None,
                    note: n,
                })
                .chain(w.levels.iter().flat_map(move |l| {
                    l.notes.iter().map(move |n| NoteAt {
                        world: w,
                        level: Some(l),
                        note: n,
                    })
                }))
        })
    }

    /// The annotation with this id, and where it lives.
    pub fn find_note(&self, id: &crate::Id) -> Option<NoteAt<'_>> {
        self.notes().find(|n| n.note.id == *id)
    }

    /// Every annotation by id (the first of a repeated id), for looking up
    /// many ids at once.
    pub fn note_index(&self) -> std::collections::HashMap<&crate::Id, NoteAt<'_>> {
        let mut m = std::collections::HashMap::new();
        for n in self.notes() {
            m.entry(&n.note.id).or_insert(n);
        }
        m
    }
}

/// An annotation and where it lives ([`Project::find_note`]).
#[derive(Debug, Clone, Copy)]
pub struct NoteAt<'a> {
    pub world: &'a World,
    /// `None`: in `world.json`.
    pub level: Option<&'a Level>,
    pub note: &'a Annotation,
}

impl NoteAt<'_> {
    /// The scope as commands name it.
    pub fn scope(&self) -> crate::command::NoteScope {
        match self.level {
            Some(l) => crate::command::NoteScope::Level(l.slug.clone()),
            None => crate::command::NoteScope::World(self.world.slug.clone()),
        }
    }

    /// What to add to the annotation's own pixels to get world pixels, as
    /// built: the level's position for a level note, a grouped guide's
    /// group offset, else nothing.
    pub fn offset(&self) -> [i64; 2] {
        match (self.level, &self.note.shape) {
            (Some(l), _) => l.world_px,
            (None, Shape::Guide { group: Some(g), .. }) => self
                .world
                .guide_group(g.as_str())
                .map_or([0, 0], |g| g.offset),
            _ => [0, 0],
        }
    }
}

impl Project {
    /// True when commands changed the project since it was last loaded or
    /// saved. Direct edits to the model don't count (use
    /// [`crate::format::save_all`] after those).
    pub fn is_dirty(&self) -> bool {
        !self.files.dirty.is_empty()
    }

    /// Drop one project-relative file (`"entities/door.json"`) from the
    /// dirty set: the file on disk already matches the project. For a
    /// caller that applied a change it read from that file (the editor's
    /// watcher), so the file doesn't show as unsaved. Other dirty files
    /// stay; a path that isn't dirty is ignored.
    pub fn mark_clean(&mut self, path: impl AsRef<Path>) {
        self.files.dirty.remove(path.as_ref());
    }

    /// Sort every list that has a canonical order (what `load` returns).
    pub fn normalize(&mut self) {
        self.entities.sort_by(|a, b| a.slug.cmp(&b.slug));
        self.rules.sort_by(|a, b| a.layer.cmp(&b.layer));
        self.sort_worlds();
        for w in &mut self.worlds {
            w.normalize();
        }
    }
}

/// A world: `worlds/<slug>/world.json` plus its levels.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct World {
    /// Same as the directory name.
    pub slug: Slug,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Named sets of guides, each with an offset, in insertion order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub guide_groups: Vec<GuideGroup>,
    /// World-scope annotations (world pixel coordinates), in insertion order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<Annotation>,
    /// World-scope reference images (world pixels), bottom first. Never
    /// exported.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<Reference>,
    /// `levels/<slug>/`, sorted by slug.
    #[serde(skip)]
    pub levels: Vec<Level>,
}

impl World {
    pub fn new(slug: Slug) -> Self {
        World {
            slug,
            name: None,
            guide_groups: Vec::new(),
            notes: Vec::new(),
            references: Vec::new(),
            levels: Vec::new(),
        }
    }

    pub fn level(&self, slug: &str) -> Option<&Level> {
        self.levels.iter().find(|l| l.slug == slug)
    }

    pub fn guide_group(&self, slug: &str) -> Option<&GuideGroup> {
        self.guide_groups.iter().find(|g| g.slug == slug)
    }

    pub fn normalize(&mut self) {
        self.levels.sort_by(|a, b| a.slug.cmp(&b.slug));
        for l in &mut self.levels {
            l.normalize();
        }
    }
}

/// A level: `level.json` plus one file per stored layer, `entities.json` and `notes.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Level {
    pub id: Id,
    /// Same as the directory name; also the Godot room name.
    pub slug: Slug,
    pub name: String,
    /// Top-left corner in world pixels.
    pub world_px: [i64; 2],
    /// `[width, height]` in cells of the project grid. Layers with their own
    /// grid size cover the same pixels.
    pub size: [u32; 2],
    /// Draw order in the world view; higher is in front.
    pub depth: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bg_color: Option<Color>,
    /// Free tags (`forest`), the same vocabulary as annotation tags.
    /// Non-empty, no repeats (validation).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Values for the project's `level_fields`, by field slug.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fields: BTreeMap<Slug, serde_json::Value>,
    /// Per-layer rule state by `auto` layer slug. A layer without an entry
    /// has seed 0, no optional groups and no tileset substitutions.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub layers: BTreeMap<Slug, LayerState>,
    /// Reference images under the layer stack (level pixels), bottom
    /// first. Never exported.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<Reference>,

    /// `<layer>.json` for (non-derived) int_grid layers, sorted by layer
    /// slug. A layer with no entry is all empty.
    #[serde(skip)]
    pub int_grids: Vec<IntGridLayer>,
    /// `<layer>.json` for derived int_grid layers: their hand-painted
    /// overrides, sorted by layer slug. A layer with no entry has none.
    #[serde(skip)]
    pub overrides: Vec<DerivedOverrides>,
    /// `<layer>.json` for tiles layers, sorted by layer slug.
    #[serde(skip)]
    pub tiles: Vec<TilesLayer>,
    /// `entities.json`, one entry per entities layer, sorted by layer slug.
    #[serde(skip)]
    pub entities: Vec<EntitiesLayer>,
    /// `notes.json`, in insertion order.
    #[serde(skip)]
    pub notes: Vec<Annotation>,
}

impl Level {
    /// An empty level with a fresh id.
    pub fn new(slug: Slug, name: impl Into<String>, size: [u32; 2]) -> Self {
        Level {
            id: Id::generate(),
            slug,
            name: name.into(),
            world_px: [0, 0],
            size,
            depth: 0,
            bg_color: None,
            tags: Vec::new(),
            fields: BTreeMap::new(),
            layers: BTreeMap::new(),
            references: Vec::new(),
            int_grids: Vec::new(),
            overrides: Vec::new(),
            tiles: Vec::new(),
            entities: Vec::new(),
            notes: Vec::new(),
        }
    }

    /// A non-derived int_grid layer's content. A derived layer's content is
    /// its [`Level::overrides`], never returned here.
    pub fn int_grid(&self, layer: &str) -> Option<&IntGridLayer> {
        self.int_grids.iter().find(|g| g.layer == layer)
    }

    /// A derived int_grid layer's hand-painted overrides in this level.
    pub fn overrides(&self, layer: &str) -> Option<&DerivedOverrides> {
        self.overrides.iter().find(|g| g.layer == layer)
    }

    pub fn tiles_layer(&self, layer: &str) -> Option<&TilesLayer> {
        self.tiles.iter().find(|t| t.layer == layer)
    }

    pub fn entities_layer(&self, layer: &str) -> Option<&EntitiesLayer> {
        self.entities.iter().find(|e| e.layer == layer)
    }

    /// Every entity instance in the level, across entities layers.
    pub fn all_entities(&self) -> impl Iterator<Item = &EntityInstance> {
        self.entities.iter().flat_map(|l| l.entities.iter())
    }

    /// Rule seed for a layer in this level (0 if unset).
    pub fn layer_seed(&self, layer: &str) -> i64 {
        self.layers.get(layer).map_or(0, |s| s.seed)
    }

    /// Optional rule groups switched on for a layer in this level.
    pub fn optional_groups(&self, layer: &str) -> &[Slug] {
        self.layers
            .get(layer)
            .map_or(&[], |s| s.optional_groups.as_slice())
    }

    /// The tileset a rule of `layer` that names `tileset` draws from in this
    /// level, after the level's substitutions.
    pub fn tileset_for<'a>(&'a self, layer: &str, tileset: &'a Slug) -> &'a Slug {
        self.layers
            .get(layer)
            .and_then(|s| s.tilesets.get(tileset))
            .unwrap_or(tileset)
    }

    /// A copy with only the per-level files `keep` (by file name:
    /// `collision.json`, `entities.json`, `notes.json`) accepts; `level.json`
    /// fields are always copied. What an incremental save clones to render a
    /// few files of a big level.
    pub(crate) fn copy_with(&self, keep: impl Fn(&str) -> bool) -> Level {
        let Level {
            id,
            slug,
            name,
            world_px,
            size,
            depth,
            bg_color,
            tags,
            fields,
            layers,
            references,
            int_grids,
            overrides,
            tiles,
            entities,
            notes,
        } = self;
        let file = |layer: &Slug| keep(&format!("{layer}.json"));
        Level {
            id: id.clone(),
            slug: slug.clone(),
            name: name.clone(),
            world_px: *world_px,
            size: *size,
            depth: *depth,
            bg_color: bg_color.clone(),
            tags: tags.clone(),
            fields: fields.clone(),
            layers: layers.clone(),
            references: references.clone(),
            int_grids: int_grids
                .iter()
                .filter(|g| file(&g.layer))
                .cloned()
                .collect(),
            overrides: overrides
                .iter()
                .filter(|g| file(&g.layer))
                .cloned()
                .collect(),
            tiles: tiles.iter().filter(|g| file(&g.layer)).cloned().collect(),
            entities: if keep("entities.json") {
                entities.clone()
            } else {
                Vec::new()
            },
            notes: if keep("notes.json") {
                notes.clone()
            } else {
                Vec::new()
            },
        }
    }

    /// Sort what has a canonical order: layers by slug, tile placements by
    /// row then column. Entities and notes keep their order.
    pub fn normalize(&mut self) {
        self.int_grids.sort_by(|a, b| a.layer.cmp(&b.layer));
        self.overrides.sort_by(|a, b| a.layer.cmp(&b.layer));
        self.tiles.sort_by(|a, b| a.layer.cmp(&b.layer));
        for t in &mut self.tiles {
            t.sort();
        }
        self.entities.sort_by(|a, b| a.layer.cmp(&b.layer));
    }
}
