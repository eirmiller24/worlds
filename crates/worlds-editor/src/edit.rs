//! The command layer: [`Editing`] implements [`EditorCommands`] on top of
//! `worlds_core::command`.
//!
//! The document's `Project` is only ever changed through [`History`], so
//! every edit has an exact inverse and marks the files it touches dirty.
//! A gesture (one brush stroke, one level drag) is one history gesture and
//! so one undo entry. After every apply, undo or redo, the returned
//! [`Change`]s go through one path, [`Editing::after`]:
//!
//! - `grid` and `tiles` changes become [`Invalidation::Cells`] of that layer;
//! - level moves, resizes and depth changes become [`Invalidation::Level`];
//!   created, deleted and renamed levels re-index the document and become
//!   [`Invalidation::All`];
//! - rule edits (`def` kind `rules`, `macros`, or `tileset` for a tileset
//!   some auto layer draws from) rebake only the layers whose rules
//!   changed: the active level at once through the cache (so the canvas
//!   and the rules panel update the same frame), every other level on the
//!   workers ([`Document::start_layer_bakes`], old bakes on screen until
//!   the new ones land, out-of-date results dropped by generation); each
//!   level gets an [`Invalidation::Layer`] (plus [`Invalidation::Tilesets`]
//!   for a tileset definition, after the images are re-read);
//! - entity types and enums (`def` and `def_renamed` of kind `entity` or
//!   `enum`) and the project key `collision_layer` bake nothing: they only
//!   bump [`Document::defs_gen`] (what the entity palette's rows are keyed
//!   on), and an entity type rename is followed in the palette's brush,
//!   favorites and recent;
//! - `level_fields` rebakes only the auto layers with a group reading a
//!   level field (`requires_level_field`), like a rule edit of those layers
//!   (none in a project without such groups);
//! - other definition changes (`def` of layers, a tileset no auto layer
//!   draws from; `def_renamed` of layers and tilesets) resync the per-layer
//!   view by slug, become [`Invalidation::All`] (plus
//!   [`Invalidation::Tilesets`] when tilesets changed), and rebake every
//!   level from scratch;
//! - the Select tool's drag of the selection (entities, annotations, tile
//!   stacks, levels) is one batch on release ([`crate::annot::plan_move`]:
//!   entities by whole cells or pixels with Ctrl, as [`entity_move_commands`];
//!   tile stacks by cells of their layer; annotations and levels by one
//!   delta, levels snapping to guides); a marquee (in a level, or on the
//!   world) only changes the selection; a level's own drag snaps to guides
//!   too;
//!   nudges, Ctrl-D and Delete build their batches here too
//!   ([`nudge_commands`], [`duplicate_commands`], [`delete_commands`]);
//!   whatever an edit, undo or redo takes away (entities, annotations,
//!   tile stacks, levels) leaves the selection;
//! - the entity tool places on release: `add_entity` with the palette's
//!   brush at the [`crate::tools::entity_placement`] of the gesture, plus
//!   what the type's `limit` asks for ([`limit_commands`]), all as the
//!   gesture's one entry;
//! - the same changes go to [`BakeCache::apply_changes`], which re-sweeps
//!   only the anchors the edit can reach. `Document::bakes` shares each
//!   layer bake with the cache (`Arc`): it lets go before the re-sweep, so
//!   the cache edits in place, and takes the result back. Each returned
//!   dirty rect becomes an [`Invalidation::Cells`] of the auto layer.
//!
//! Undo and redo are the same operation (apply the entry's command), so
//! they invalidate exactly what the edit did.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Instant;

use worlds_core::command::{
    CellValue, Change, CommandError, DefKind, GridValue, LevelPart, Origin, ProjectKey, Rect,
};
use worlds_core::export::{BakedLayers, BakedLevel, BakedTile};
use worlds_core::{Command, History, Id, LayerKind, Override, Slug, TilePlacement};
use worlds_rules::{BakeCache, LayerBake};
use worlds_server::ChangeKind;

use crate::doc::{Document, LevelBakes, LevelIx};
use crate::tools::{
    Brush, CellHit, CommandContext, EditorCommands, Grab, Invalidation, Stamp, Tool, ToolEvent,
    cell_rect, clip_rect, in_layer, line_cells, rect_cells, resize_level, snap_move,
};
use crate::view::ViewState;

/// What a painting gesture puts down.
#[derive(Debug, Clone, PartialEq)]
pub enum Paint {
    /// An int_grid cell value: a value slug, empty (`Clear`; on a derived
    /// layer, no override), or `Suppress` (derived layers only).
    Value(GridValue),
    /// A stamp of tiles from one tileset.
    Tiles { tileset: Slug, stamp: Stamp },
    /// Empty the cells of a tiles layer (the eraser).
    ClearTiles,
}

#[derive(Debug, Clone)]
enum Kind {
    Paint(Paint),
    MoveLevel {
        px: [i64; 2],
        start: [f64; 2],
    },
    Resize {
        px: [i64; 2],
        size: [u32; 2],
        edges: [i8; 2],
        start: [f64; 2],
    },
    /// The entity tool: place the brush on release.
    Place {
        start: CellHit,
    },
    /// Select: drag the selection, applied on release as one entry.
    MoveSelection {
        start: CellHit,
        was_selected: bool,
    },
    /// Select: a marquee from `start` (in its level, or on the world);
    /// selects on release, no command.
    Marquee {
        start: CellHit,
        world: bool,
    },
    /// Select: drag a geometry handle of the one selected annotation,
    /// applied on release as one entry.
    MoveVertex {
        start: CellHit,
        handle: crate::annot::Handle,
    },
    /// Select: Alt+click on a segment of the one selected annotation adds a
    /// point there on release, one entry.
    InsertVertex {
        start: CellHit,
    },
}

#[derive(Debug, Clone)]
struct Gesture {
    tool: Tool,
    world: Slug,
    level: Slug,
    layer: Slug,
    /// Layer size in its own cells.
    size: [u32; 2],
    /// Stroke start: the origin of the stamp lattice and of rects and lines.
    origin: [i32; 2],
    /// Last cell a brush stroke reached.
    last: [i32; 2],
    kind: Kind,
}

/// One entry the user recorded, or one undo or redo step: what the live
/// API reports as a `changed` event with origin `user`
/// ([`Editing::take_user_edits`]).
#[derive(Debug, Clone, PartialEq)]
pub struct UserEdit {
    pub kind: ChangeKind,
    pub label: String,
    /// For undo and redo: who made the entry.
    pub entry_origin: Option<Origin>,
    pub changes: Vec<Change>,
}

/// What definition changes ask of the bakes ([`Editing::after`]).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Redefined {
    /// No definition changed.
    None,
    /// Only these auto layers' rules (or the tiles they draw from) changed:
    /// rebake those layers, nothing else.
    Layers(Vec<Slug>),
    /// Anything else: rebuild the scene and rebake everything.
    All,
}

/// The editor's command layer: history, incremental bakes, gestures.
#[derive(Default)]
pub struct Editing {
    /// Bumped by every [`Editing::after`] with changes: what views computed
    /// from the project (the rules panel's match counts) are keyed on.
    rev: u64,
    history: History,
    cache: BakeCache,
    /// Changes of the entry being built (a gesture, or one command).
    entry_changes: Vec<Change>,
    /// Entries recorded and history steps taken since the last
    /// [`Editing::take_user_edits`].
    user_edits: Vec<UserEdit>,
    /// Layer bakes (level id, layer) the cache refused to take as a seed;
    /// not retried.
    unseedable: HashSet<(Id, Slug)>,
    gesture: Option<Gesture>,
    unsaved: bool,
    cells: Vec<[i32; 2]>,
}

impl Editing {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn history(&self) -> &History {
        &self.history
    }

    /// Changes whenever the project does (see the field).
    pub fn rev(&self) -> u64 {
        self.rev
    }

    /// Layer jobs came back ([`Document::poll_layer_bakes`]): a level whose
    /// bakes this cache maintains takes the new bake as is (no rebake on
    /// the UI thread), unless it already holds a newer one of its own.
    pub fn adopt_layer_bakes(&mut self, doc: &Document, arrived: &[(LevelIx, Slug)]) {
        for (ix, layer) in arrived {
            if *ix >= doc.levels.len() || !doc.bake_owned[*ix] {
                continue;
            }
            let slug = doc.level(*ix).slug.as_str();
            if self.cache.get_shared(slug, layer.as_str()).is_some() {
                continue;
            }
            let Some(bake) = doc.bakes[*ix].as_ref().and_then(|b| b.get(layer)) else {
                continue;
            };
            match self
                .cache
                .seed(&doc.project, slug, layer.as_str(), bake.clone())
            {
                Ok(true) => {}
                Ok(false) => tracing::debug!("layer bake {slug}/{layer} doesn't fit; not cached"),
                Err(e) => tracing::debug!("layer bake {slug}/{layer}: {e}"),
            }
        }
    }

    /// The incremental bake cache (tests).
    pub fn cache(&self) -> &BakeCache {
        &self.cache
    }

    /// The history, for the live API: agent entries are recorded here.
    pub fn history_mut(&mut self) -> &mut History {
        &mut self.history
    }

    /// The bake cache, for the live API (`project.bake` reads and fills it).
    pub fn cache_mut(&mut self) -> &mut BakeCache {
        &mut self.cache
    }

    /// The history and the bake cache at once (the live API's `Doc`).
    pub fn parts_mut(&mut self) -> (&mut History, &mut BakeCache) {
        (&mut self.history, &mut self.cache)
    }

    /// The project changed in unknown ways (a failed rollback): drop every
    /// cache and redraw everything.
    pub fn reset_caches(&mut self, cx: &mut CommandContext<'_>) {
        let map = cx.doc.reindex();
        cx.view.active_level = cx
            .view
            .active_level
            .and_then(|i| map.get(i).copied().flatten());
        self.cache.clear();
        self.unsaved = cx.doc.project.is_dirty();
        cx.invalidate.push(Invalidation::All);
    }

    /// The user's entries and undo/redo steps since the last call, oldest
    /// first: the editor reports each as a `changed` event.
    pub fn take_user_edits(&mut self) -> Vec<UserEdit> {
        std::mem::take(&mut self.user_edits)
    }

    /// Close the open gesture; if that recorded an entry, remember it as a
    /// user edit.
    fn close(&mut self) {
        let recorded = self.history.end_gesture();
        self.entry_done(recorded);
    }

    /// The entry being built is finished: recorded (`true`) or dropped.
    fn entry_done(&mut self, recorded: bool) {
        let changes = std::mem::take(&mut self.entry_changes);
        if recorded && !changes.is_empty() {
            self.user_edits.push(UserEdit {
                kind: ChangeKind::Edit,
                label: self.history.undo_label().unwrap_or_default().to_string(),
                entry_origin: None,
                changes,
            });
        }
    }

    /// Hand the active level's bakes to the cache once they're in, so the
    /// first stroke there only re-sweeps what it touches. The worker's
    /// bakes are shared with the cache as they are (no re-sweep, no copy).
    fn warm(&mut self, cx: &mut CommandContext<'_>) {
        let Some(ix) = cx.view.active_level.filter(|&i| i < cx.doc.levels.len()) else {
            return;
        };
        // A whole-level worker bake the document still shows as it is
        // seeds every layer at once, derived grids and pass state included
        // (no re-sweep, no copy).
        let level_slug = cx.doc.level(ix).slug.clone();
        let nothing_cached = cx
            .doc
            .project
            .layers
            .iter()
            .filter(|l| l.is_auto())
            .all(|l| {
                self.cache
                    .get(level_slug.as_str(), l.slug.as_str())
                    .is_none()
            });
        if nothing_cached && let Some(seed) = cx.doc.take_seed(ix) {
            if self.cache.seed_level(&cx.doc.project, seed) {
                tracing::debug!("bake cache seeded with level {level_slug}");
                return;
            }
            tracing::debug!("level seed {level_slug} doesn't fit; seeding layer by layer");
        }
        let doc = &*cx.doc;
        let Some(bakes) = &doc.bakes[ix] else {
            return;
        };
        let slug = doc.level(ix).slug.as_str();
        // Writers of derived grids before their readers: a reader's seed
        // needs its writers cached.
        let order = doc.project.auto_layer_order().unwrap_or_else(|_| {
            doc.project
                .layers
                .iter()
                .filter(|l| l.is_auto())
                .map(|l| l.slug.clone())
                .collect()
        });
        for (layer, bake) in order.iter().filter_map(|l| bakes.get_key_value(l)) {
            if self.cache.get_shared(slug, layer.as_str()).is_some()
                || doc.stale_bakes[ix].contains(layer)
            {
                continue;
            }
            let key = (doc.ids[ix].clone(), layer.clone());
            if self.unseedable.contains(&key) {
                continue;
            }
            match self
                .cache
                .seed(&doc.project, slug, layer.as_str(), bake.clone())
            {
                Ok(true) => tracing::debug!("bake cache seeded with {slug}/{layer}"),
                Ok(false) => {
                    tracing::warn!("bake of {slug}/{layer} doesn't fit its rules; not cached");
                    self.unseedable.insert(key);
                }
                Err(e) => {
                    tracing::warn!("bake cache for {slug}/{layer}: {e}");
                    self.unseedable.insert(key);
                }
            }
        }
    }

    /// Apply through the history and route the changes. Inside a gesture
    /// the command joins it.
    fn push(&mut self, cx: &mut CommandContext<'_>, cmd: Command) -> Result<(), String> {
        let t = Instant::now();
        let changes = match self.history.push(&mut cx.doc.project, cmd) {
            Ok(c) => c,
            Err(e) => return Err(self.failed(cx, e)),
        };
        self.after(cx, &changes);
        self.entry_changes.extend(changes);
        tracing::trace!("applied in {:.2} ms", t.elapsed().as_secs_f64() * 1e3);
        Ok(())
    }

    /// A command failed. Normally nothing changed; but after a batch whose
    /// rollback failed ([`CommandError::Rollback`]) the project did change in
    /// unknown ways, so drop every cache and redraw everything. Returns the
    /// message.
    fn failed(&mut self, cx: &mut CommandContext<'_>, e: CommandError) -> String {
        if matches!(e, CommandError::Rollback { .. }) {
            tracing::error!("{e}");
            let doc = &mut *cx.doc;
            let map = doc.reindex();
            cx.view.active_level = cx
                .view
                .active_level
                .and_then(|i| map.get(i).copied().flatten());
            self.cache.clear();
            self.unsaved = doc.project.is_dirty();
            cx.invalidate.push(Invalidation::All);
            return format!("{e}. Save, then reload the project to be safe.");
        }
        e.to_string()
    }

    fn push_or_status(&mut self, cx: &mut CommandContext<'_>, cmd: Command) {
        if let Err(e) = self.push(cx, cmd) {
            cx.view.set_status(e, true);
        }
    }

    /// Turn changes into invalidations and rebakes (see the module docs).
    pub fn after(&mut self, cx: &mut CommandContext<'_>, changes: &[Change]) {
        self.unsaved = cx.doc.project.is_dirty();
        if changes.is_empty() {
            return;
        }
        self.rev += 1;
        // Entities, annotations, tile stacks and levels an edit, undo or
        // redo took away leave the selection.
        if !cx.view.selection.is_empty()
            && changes.iter().any(|c| {
                matches!(
                    c,
                    Change::Entity { .. }
                        | Change::EntityLayer { .. }
                        | Change::LevelDeleted { .. }
                        | Change::Note { .. }
                        | Change::Tiles { .. }
                )
            })
        {
            cx.view.selection.prune(&cx.doc.project);
        }
        let redefined = self.defs_changed(cx, changes);
        let doc = &mut *cx.doc;
        if changes.iter().any(|c| {
            matches!(
                c,
                Change::LevelCreated { .. }
                    | Change::LevelDeleted { .. }
                    | Change::LevelRenamed { .. }
            )
        }) {
            let map = doc.reindex();
            cx.view.active_level = cx
                .view
                .active_level
                .and_then(|i| map.get(i).copied().flatten());
            cx.invalidate.push(Invalidation::All);
        }
        let mut levels: Vec<Slug> = Vec::new();
        for c in changes {
            match c {
                Change::Grid {
                    level, layer, rect, ..
                }
                | Change::Tiles {
                    level, layer, rect, ..
                } => {
                    if let Some(ix) = doc.level_ix(level.as_str()) {
                        cx.invalidate.push(Invalidation::Cells {
                            level: ix,
                            layer: layer.clone(),
                            rect: [
                                rect.x as i32,
                                rect.y as i32,
                                (rect.x + rect.w) as i32,
                                (rect.y + rect.h) as i32,
                            ],
                        });
                    }
                }
                Change::LevelMeta { level, part, .. } => {
                    if matches!(
                        part,
                        LevelPart::Position | LevelPart::Size | LevelPart::All | LevelPart::Depth
                    ) && let Some(ix) = doc.level_ix(level.as_str())
                    {
                        cx.invalidate.push(Invalidation::Level(ix));
                    }
                }
                Change::EntityOutside { id, level, .. } => {
                    cx.view
                        .set_status(format!("Entity {id} is now outside level {level}"), true);
                }
                _ => {}
            }
            if let Some(l) = c.level()
                && !levels.contains(l)
            {
                levels.push(l.clone());
            }
            if let Change::LevelRenamed { from, .. } = c
                && !levels.contains(from)
            {
                levels.push(from.clone());
            }
        }
        let reseeded = changes.iter().any(|c| {
            matches!(
                c,
                Change::Project {
                    key: ProjectKey::Seed
                }
            )
        });
        if redefined == Redefined::All || reseeded {
            levels = doc.project.levels().map(|l| l.slug.clone()).collect();
        } else if let Redefined::Layers(_) = &redefined
            && let Some(ix) = cx.view.active_level.filter(|&i| i < doc.levels.len())
        {
            // The active level rebakes the edited layers right away; every
            // other level not touched otherwise gets a worker job below.
            let active = doc.level(ix).slug.clone();
            if !levels.contains(&active) {
                levels.push(active);
            }
        }
        if !doc.project.layers.iter().any(|l| l.is_auto()) {
            return;
        }
        let auto: Vec<Slug> = doc
            .project
            .layers
            .iter()
            .filter(|l| l.is_auto())
            .map(|l| l.slug.clone())
            .collect();
        // (level, layer) pairs the cache rebaked here.
        let mut rebaked: HashSet<(Slug, Slug)> = HashSet::new();
        for slug in &levels {
            let ix = doc.level_ix(slug.as_str());
            // Give the cache the only reference to the bakes it shares with
            // the document, so a re-sweep edits them in place instead of
            // copying whole layers on every brush move. They come back from
            // the cache below.
            if let Some(b) = ix.and_then(|ix| doc.bakes[ix].as_mut()) {
                let b = Arc::make_mut(b);
                let shared: Vec<Slug> = b
                    .iter()
                    .filter(|(layer, bake)| {
                        self.cache
                            .get_shared(slug.as_str(), layer.as_str())
                            .is_some_and(|c| Arc::ptr_eq(c, bake))
                    })
                    .map(|(layer, _)| layer.clone())
                    .collect();
                for layer in shared {
                    b.remove(&layer);
                }
            }
            // After a definition change every auto layer is baked afresh
            // (the cache was cleared); otherwise only what the edit reaches
            // (for a rule edit: the edited layers, in full). Each layer
            // succeeds or fails on its own: a failed one is left uncached.
            // (The whole-level bake after other definition changes reports
            // its first error only; every view is redrawn anyway.)
            let (res, level_error): (Vec<(Slug, Result<_, String>)>, _) =
                if redefined == Redefined::All {
                    match self
                        .cache
                        .bake_level(&doc.project, slug.as_str(), &doc.images)
                    {
                        Ok(d) => (d.into_iter().map(|(l, r)| (l, Ok(r))).collect(), None),
                        Err(e) => (Vec::new(), Some(e.to_string())),
                    }
                } else {
                    let each = self.cache.apply_changes_per_layer(
                        &doc.project,
                        slug.as_str(),
                        &doc.images,
                        changes,
                    );
                    let each = each
                        .into_iter()
                        .map(|(l, r)| (l, r.map_err(|e| e.to_string())))
                        .collect();
                    (each, None)
                };
            let Some(ix) = ix.or_else(|| doc.level_ix(slug.as_str())) else {
                continue;
            };
            // Take the cache's bakes back (current, and still shared).
            let mut fresh: Vec<(Slug, Arc<LayerBake>)> = auto
                .iter()
                .filter_map(|l| {
                    self.cache
                        .get_shared(slug.as_str(), l.as_str())
                        .map(|b| (l.clone(), b.clone()))
                })
                .collect();
            // A layer whose rules don't bake (say, a half-written rule)
            // shows nothing rather than its old tiles, as in the other
            // levels (`Document::start_layer_bakes`).
            let failed: Vec<(&Slug, &String)> = res
                .iter()
                .filter_map(|(l, r)| r.as_ref().err().map(|e| (l, e)))
                .filter(|(l, _)| auto.contains(l))
                .collect();
            for (l, _) in &failed {
                if !fresh.iter().any(|(f, _)| f == *l) {
                    fresh.push(((*l).clone(), Arc::default()));
                }
                cx.invalidate.push(Invalidation::Layer {
                    level: ix,
                    layer: (*l).clone(),
                });
            }
            if !fresh.is_empty() {
                let names: Vec<Slug> = fresh.iter().map(|(l, _)| l.clone()).collect();
                let bakes: &mut LevelBakes =
                    Arc::make_mut(doc.bakes[ix].get_or_insert_with(Default::default));
                bakes.extend(fresh);
                doc.bakes_refreshed(ix, &names);
            }
            if let Some((l, e)) = failed.first() {
                cx.view
                    .set_status(format!("Rebake of {slug}/{l} failed: {e}"), true);
            } else if let Some(e) = &level_error {
                cx.view
                    .set_status(format!("Rebake of {slug} failed: {e}"), true);
            }
            let mut any = false;
            for (layer, r) in res {
                rebaked.insert((slug.clone(), layer.clone()));
                let Ok(r) = r else { continue };
                any = true;
                if !r.is_empty() {
                    cx.invalidate.push(Invalidation::Cells {
                        level: ix,
                        layer,
                        rect: [r.min[0], r.min[1], r.max[0], r.max[1]],
                    });
                }
            }
            if any {
                doc.bake_owned[ix] = true;
            }
        }
        if let Redefined::Layers(layers) = redefined {
            let mut jobs = Vec::new();
            for ix in 0..doc.levels.len() {
                let slug = doc.level(ix).slug.clone();
                for l in &layers {
                    cx.invalidate.push(Invalidation::Layer {
                        level: ix,
                        layer: l.clone(),
                    });
                    // Levels handled above whose cache didn't rebake the
                    // layer (a change it doesn't track) get a job too.
                    if !levels.contains(&slug) || !rebaked.contains(&(slug.clone(), l.clone())) {
                        self.cache.invalidate_level_layer(slug.as_str(), l.as_str());
                        jobs.push((ix, l.clone()));
                    }
                }
            }
            doc.start_layer_bakes(jobs);
        }
    }

    /// Definitions changed (layers, entity types, enums, tilesets, rule
    /// sets, macros, or the project keys rules read): sort out what the
    /// bakes need ([`Redefined`]).
    ///
    /// - Only rules changed (`def` kind `rules` for a layer; `macros`, which
    ///   counts for every layer whose rules use `rules/macros.json`; or
    ///   `tileset` for a tileset some auto layer draws from):
    ///   [`Redefined::Layers`]. The layer list and the scene stay.
    /// - Anything else: carry the per-layer view settings over by slug,
    ///   rebuild the scene and drop every cached bake, so [`Self::after`]
    ///   bakes every level afresh ([`Redefined::All`]).
    ///
    /// Either way a tileset definition change re-reads the images and asks
    /// the app to upload the textures again ([`Invalidation::Tilesets`]).
    fn defs_changed(&mut self, cx: &mut CommandContext<'_>, changes: &[Change]) -> Redefined {
        let doc = &mut *cx.doc;
        let mut all = false;
        let mut layers: BTreeSet<Slug> = BTreeSet::new();
        let mut tilesets = false;
        let is_auto = |doc: &Document, l: &str| doc.project.layer(l).is_some_and(|d| d.is_auto());
        for c in changes {
            match c {
                // Rules of a layer that isn't auto bake nothing.
                Change::Def {
                    kind: DefKind::Rules,
                    slug,
                } if !is_auto(doc, slug.as_str()) => {}
                Change::Def {
                    kind: DefKind::Rules,
                    slug,
                } => {
                    layers.insert(slug.clone());
                }
                Change::Def {
                    kind: DefKind::Macros,
                    ..
                } => layers.extend(
                    doc.project
                        .rules
                        .iter()
                        .filter(|set| set.uses_macros() && is_auto(doc, set.layer.as_str()))
                        .map(|set| set.layer.clone()),
                ),
                Change::Def {
                    kind: DefKind::Tileset,
                    slug,
                } => {
                    tilesets = true;
                    let using = doc.project.auto_layers_drawing_from(slug.as_str());
                    if using.is_empty() {
                        all = true;
                    }
                    layers.extend(using);
                }
                // Entity types and enums bake nothing; `collision_layer`
                // only matters to the export.
                Change::Def {
                    kind: DefKind::Entity | DefKind::Enum,
                    ..
                }
                | Change::Project {
                    key: ProjectKey::CollisionLayer,
                } => {}
                Change::DefRenamed {
                    kind: DefKind::Entity,
                    from,
                    to,
                } => cx.view.entities.renamed(from, to),
                Change::DefRenamed {
                    kind: DefKind::Enum,
                    ..
                } => {}
                // Level field definitions: only the auto layers with a group
                // reading a level field (`requires_level_field`) can change.
                Change::Project {
                    key: ProjectKey::LevelFields,
                } => layers.extend(
                    doc.project
                        .rules
                        .iter()
                        .filter(|set| {
                            is_auto(doc, set.layer.as_str())
                                && set.groups.iter().any(|g| g.requires_level_field.is_some())
                        })
                        .map(|set| set.layer.clone()),
                ),
                // The project grid: every layer's cells (only with no levels).
                Change::Def { .. }
                | Change::DefRenamed { .. }
                | Change::Project {
                    key: ProjectKey::GridSize,
                } => all = true,
                _ => {}
            }
        }
        if changes.iter().any(|c| {
            matches!(
                c,
                Change::Def { .. }
                    | Change::DefRenamed { .. }
                    | Change::Project {
                        key: ProjectKey::LevelFields
                    }
            )
        }) {
            doc.defs_gen += 1;
            // The brush's stored values follow its type's fields (renames of
            // the type itself were followed above).
            cx.view.entities.refit_brush(&doc.project);
        }
        if tilesets
            || changes.iter().any(|c| {
                matches!(
                    c,
                    Change::DefRenamed {
                        kind: DefKind::Tileset,
                        ..
                    }
                )
            })
        {
            doc.tileset_defs_changed();
            cx.invalidate.push(Invalidation::Tilesets);
        }
        if !all {
            if layers.is_empty() {
                return Redefined::None;
            }
            // Seeds refused before may fit the new rules.
            self.unseedable.clear();
            // Dependencies are the new rules': a writer whose rule edit
            // dropped or retargeted a `write` no longer counts as the writer
            // of what it wrote. The readers of what the shown bakes of these
            // layers wrote rebake too (their grid is now the overrides
            // alone, or another writer's).
            let wrote: BTreeSet<Slug> = doc
                .bakes
                .iter()
                .flatten()
                .flat_map(|b| layers.iter().filter_map(|l| b.get(l)))
                .flat_map(|b| b.derived.keys().cloned())
                .collect();
            if !wrote.is_empty() {
                let readers: Vec<Slug> = doc
                    .project
                    .layers
                    .iter()
                    .filter(|l| l.is_auto())
                    .filter(|l| {
                        doc.project
                            .layer_reads(l.slug.as_str())
                            .iter()
                            .any(|g| wrote.contains(g))
                    })
                    .map(|l| l.slug.clone())
                    .collect();
                layers.extend(readers);
            }
            // Layers reading the derived grids these write rebake too,
            // transitively, in dependency order.
            return Redefined::Layers(with_dependents(&doc.project, layers));
        }
        let renamed: Vec<(Slug, Slug)> = changes
            .iter()
            .filter_map(|c| match c {
                Change::DefRenamed {
                    kind: DefKind::Layer,
                    from,
                    to,
                } => Some((from.clone(), to.clone())),
                _ => None,
            })
            .collect();
        cx.view.resync_layers(doc, &renamed);
        self.cache.clear();
        self.unseedable.clear();
        for ix in 0..doc.levels.len() {
            doc.bakes[ix] = None;
            doc.stale_bakes[ix].clear();
        }
        cx.invalidate.push(Invalidation::All);
        Redefined::All
    }

    fn begin(
        &mut self,
        cx: &mut CommandContext<'_>,
        tool: Tool,
        at: CellHit,
        modifiers: egui::Modifiers,
        grab: Grab,
    ) {
        self.close();
        self.gesture = None;
        let doc = &*cx.doc;
        // A world marquee or a drag of the selection needs no level (only
        // world px count): they work in a project with none.
        let worldly = tool == Tool::Select
            && matches!(
                grab,
                Grab::WorldMarquee | Grab::Selection { .. } | Grab::Vertex(_) | Grab::InsertVertex
            );
        // Without a layer ([`crate::tools::NO_LAYER`]: a project with none)
        // only the Select tool works, in cells of the project grid.
        let no_layer = at.layer >= doc.project.layers.len();
        if no_layer && tool != Tool::Select {
            return;
        }
        if at.level >= doc.levels.len() {
            if !worldly {
                return;
            }
            let none = Slug::new("none").expect("a valid slug");
            self.gesture = Some(Gesture {
                tool,
                world: none.clone(),
                level: none.clone(),
                layer: none,
                size: [0, 0],
                origin: at.cell,
                last: at.cell,
                kind: match grab {
                    Grab::Selection { was_selected } => Kind::MoveSelection {
                        start: at,
                        was_selected,
                    },
                    Grab::Vertex(handle) => Kind::MoveVertex { start: at, handle },
                    Grab::InsertVertex => Kind::InsertVertex { start: at },
                    _ => Kind::Marquee {
                        start: at,
                        world: true,
                    },
                },
            });
            return;
        }
        let level = doc.level(at.level);
        let world = doc.project.worlds[doc.levels[at.level].world].slug.clone();
        let (layer, size) = match doc.project.layers.get(at.layer) {
            Some(def) => (def.slug.clone(), doc.project.layer_cells(def, level)),
            None => (Slug::new("none").expect("a valid slug"), level.size),
        };
        let mut g = Gesture {
            tool,
            world,
            level: level.slug.clone(),
            layer,
            size,
            origin: at.cell,
            last: at.cell,
            kind: Kind::Paint(Paint::Value(GridValue::Clear)),
        };
        match tool {
            Tool::Hand => {}
            Tool::Select if matches!(grab, Grab::Marquee | Grab::WorldMarquee) => {
                // A marquee changes the selection, not the project: no
                // history gesture.
                g.kind = Kind::Marquee {
                    start: at,
                    world: grab == Grab::WorldMarquee,
                };
                self.gesture = Some(g);
            }
            Tool::Select if matches!(grab, Grab::Selection { .. }) => {
                // Applied on release as one batch (its label says what
                // moved), so no history gesture is opened here.
                let Grab::Selection { was_selected } = grab else {
                    return;
                };
                g.kind = Kind::MoveSelection {
                    start: at,
                    was_selected,
                };
                self.gesture = Some(g);
            }
            Tool::Select if matches!(grab, Grab::Vertex(_) | Grab::InsertVertex) => {
                // Applied on release as one entry.
                g.kind = match grab {
                    Grab::Vertex(handle) => Kind::MoveVertex { start: at, handle },
                    _ => Kind::InsertVertex { start: at },
                };
                self.gesture = Some(g);
            }
            Tool::Select => {
                let (kind, label) = match grab {
                    Grab::None
                    | Grab::Marquee
                    | Grab::WorldMarquee
                    | Grab::Selection { .. }
                    | Grab::Vertex(_)
                    | Grab::InsertVertex => {
                        return;
                    }
                    Grab::Level => (
                        Kind::MoveLevel {
                            px: level.world_px,
                            start: at.world_px,
                        },
                        format!("Move level {}", level.slug),
                    ),
                    Grab::Handle(edges) => (
                        Kind::Resize {
                            px: level.world_px,
                            size: level.size,
                            edges,
                            start: at.world_px,
                        },
                        format!("Resize level {}", level.slug),
                    ),
                };
                g.kind = kind;
                self.history.begin_gesture(label, Origin::User);
                self.gesture = Some(g);
            }
            Tool::Entity => {
                if modifiers.alt {
                    sample_entity(cx.doc, cx.view, at);
                    return;
                }
                if let Some(why) = entity_refusal(doc, cx.view, at.layer) {
                    cx.view.set_status(why, true);
                    return;
                }
                let Some(brush) = &cx.view.entities.brush else {
                    cx.view
                        .set_status("Pick an entity type in the palette first", true);
                    return;
                };
                if doc.project.entity_def(brush.ty.as_str()).is_none() {
                    let why = format!("No entity type {}", brush.ty);
                    cx.view.set_status(why, true);
                    return;
                }
                g.kind = Kind::Place { start: at };
                self.history
                    .begin_gesture(format!("Place {}", brush.ty), Origin::User);
                self.gesture = Some(g);
            }
            _ => {
                if modifiers.alt {
                    sample(cx.doc, cx.view, at);
                    return;
                }
                if let Some(why) = refusal(doc, cx.view, at.layer) {
                    cx.view.set_status(why, true);
                    return;
                }
                let paint = match resolve_paint(doc, cx.view, at.layer, tool) {
                    Ok(p) => p,
                    Err(why) => {
                        cx.view.set_status(why, true);
                        return;
                    }
                };
                let label = gesture_label(tool, &g.layer);
                g.kind = Kind::Paint(paint);
                self.history.begin_gesture(label, Origin::User);
                match tool {
                    Tool::Brush | Tool::Eraser => {
                        let cmd = paint_command(&g, &[at.cell], false);
                        self.gesture = Some(g);
                        if let Some(cmd) = cmd {
                            self.push_or_status(cx, cmd);
                        }
                    }
                    Tool::Fill => {
                        if let Some(cmd) = fill_command(cx.doc, &g, at.level, at.cell) {
                            self.push_or_status(cx, cmd);
                        }
                        self.close();
                    }
                    _ => self.gesture = Some(g),
                }
            }
        }
    }

    fn moved(&mut self, cx: &mut CommandContext<'_>, at: CellHit) {
        let Some(g) = &mut self.gesture else {
            return;
        };
        match &g.kind {
            Kind::Paint(_) if matches!(g.tool, Tool::Brush | Tool::Eraser) => {
                line_cells(g.last, at.cell, &mut self.cells);
                g.last = at.cell;
                let cmd = paint_command(g, &self.cells, false);
                if let Some(cmd) = cmd {
                    self.push_or_status(cx, cmd);
                }
            }
            // Level moves and resizes only preview (the viewport draws the
            // outline) and commit one command on release.
            _ => {}
        }
    }

    fn end(&mut self, cx: &mut CommandContext<'_>, at: CellHit, modifiers: egui::Modifiers) {
        let Some(g) = self.gesture.take() else {
            return;
        };
        let cmd = match &g.kind {
            Kind::Paint(_) => match g.tool {
                Tool::Brush | Tool::Eraser if at.cell != g.last => {
                    line_cells(g.last, at.cell, &mut self.cells);
                    paint_command(&g, &self.cells, false)
                }
                Tool::Rect => rect_command(&g, cell_rect(g.origin, at.cell), modifiers.shift),
                Tool::Line => {
                    line_cells(g.origin, at.cell, &mut self.cells);
                    paint_command(&g, &self.cells, false)
                }
                _ => None,
            },
            Kind::MoveLevel { px, start } => {
                let d_raw = [at.world_px[0] - start[0], at.world_px[1] - start[1]];
                let to = match cx.doc.level_ix(g.level.as_str()) {
                    Some(ix) => {
                        let (d, _) = crate::annot::drag_delta(cx.doc, cx.view, &[ix], d_raw, false);
                        [px[0] + d[0], px[1] + d[1]]
                    }
                    None => snap_move(*px, d_raw, cx.doc.project.grid_size),
                };
                Some(Command::MoveLevel {
                    world: g.world.clone(),
                    level: g.level.clone(),
                    world_px: to,
                })
            }
            Kind::Resize {
                px,
                size,
                edges,
                start,
            } => {
                let (_, new) = resize_level(
                    *px,
                    *size,
                    *edges,
                    [at.world_px[0] - start[0], at.world_px[1] - start[1]],
                    cx.doc.project.grid_size,
                );
                (new != *size).then(|| Command::ResizeLevel {
                    world: g.world.clone(),
                    level: g.level.clone(),
                    size: new,
                    anchor: [-edges[0], -edges[1]],
                })
            }
            Kind::Place { start } => {
                self.place(cx, &g, start, &at, modifiers);
                return;
            }
            Kind::MoveSelection {
                start,
                was_selected,
            } => {
                self.move_selection(cx, start, &at, *was_selected, modifiers);
                return;
            }
            Kind::Marquee { start, world } => {
                marquee_select(cx, start, &at, *world, crate::tools::pixel_mod(modifiers));
                self.close();
                return;
            }
            Kind::MoveVertex { start, handle } => {
                self.move_vertex(cx, start, &at, *handle, modifiers);
                return;
            }
            Kind::InsertVertex { start } => {
                self.insert_vertex(cx, start, modifiers);
                return;
            }
        };
        if let Some(cmd) = cmd {
            self.push_or_status(cx, cmd);
        }
        self.close();
    }

    /// The entity tool's release: add the brush's type at the placement,
    /// honouring the type's `limit` within its scope, as the gesture's one
    /// entry.
    fn place(
        &mut self,
        cx: &mut CommandContext<'_>,
        g: &Gesture,
        start: &CellHit,
        at: &CellHit,
        modifiers: egui::Modifiers,
    ) {
        let doc = &*cx.doc;
        let planned = (|| {
            let brush = cx.view.entities.brush.clone().ok_or("No entity brush")?;
            let def = doc
                .project
                .entity_def(brush.ty.as_str())
                .ok_or_else(|| format!("No entity type {}", brush.ty))?;
            let p = crate::tools::entity_placement(doc, def, start, at, modifiers);
            if !in_layer(p.cell, g.size) {
                return Err(format!(
                    "Cell {}, {} is outside level {}",
                    p.cell[0], p.cell[1], g.level
                ));
            }
            let entity = worlds_core::EntityInstance {
                id: fresh_entity_id(&doc.project),
                ty: brush.ty.clone(),
                cell: p.cell,
                offset_px: p.offset_px,
                size: p.size,
                fields: brush.fields.clone(),
            };
            let cmds = limit_commands(doc, def, &g.level, &g.layer, entity, p)?;
            Ok::<_, String>((brush.ty, cmds))
        })();
        let (ty, cmds) = match planned {
            Ok(x) => x,
            Err(why) => {
                cx.view.set_status(why, true);
                self.close();
                return;
            }
        };
        for cmd in cmds {
            if let Err(e) = self.push(cx, cmd) {
                // Take back what this placement already did.
                self.cancel(cx);
                cx.view.set_status(e, true);
                return;
            }
        }
        cx.view.entities.placed(&ty);
        self.close();
    }

    /// The release of a Select drag of the selection: its
    /// [`crate::annot::plan_move`] as one batch (one entry). A release that
    /// moves nothing is a click: on an item that was already selected it
    /// narrows the selection to that item (Ctrl: takes it out).
    fn move_selection(
        &mut self,
        cx: &mut CommandContext<'_>,
        start: &CellHit,
        at: &CellHit,
        was_selected: bool,
        modifiers: egui::Modifiers,
    ) {
        let pixel = crate::tools::pixel_mod(modifiers);
        let d_raw = [
            at.world_px[0] - start.world_px[0],
            at.world_px[1] - start.world_px[1],
        ];
        let plan = match crate::annot::plan_move(cx.doc, cx.view, d_raw, pixel) {
            Ok(p) => p,
            Err(e) => {
                cx.view.set_status(e, true);
                return;
            }
        };
        if let Some(why) = plan.left_alone.as_ref().filter(|_| d_raw != [0.0, 0.0]) {
            cx.view.set_status(why.clone(), plan.commands.is_empty());
        }
        if plan.commands.is_empty() {
            if was_selected
                && let Some(item) = crate::viewport::select_item_at(
                    cx.doc,
                    cx.view,
                    Some(start.level).filter(|&ix| {
                        ix < cx.doc.levels.len()
                            && crate::viewport::level_wrect(cx.doc, ix).contains(start.world_px)
                    }),
                    start.world_px,
                    cx.view.zoom,
                )
                .or_else(|| {
                    crate::viewport::level_under(cx.doc, start.world_px)
                        .map(|ix| crate::view::SelItem::Level(cx.doc.ids[ix].clone()))
                        .filter(|i| cx.view.selection.contains(i))
                })
            {
                if pixel {
                    cx.view.selection.toggle(item);
                } else {
                    cx.view.selection.set([item]);
                }
            }
            return;
        }
        let moved_tiles = plan.selection;
        let r = self.push(
            cx,
            Command::Batch {
                label: plan.label,
                commands: plan.commands,
            },
        );
        self.entry_done(r.is_ok());
        match r {
            Ok(()) => cx.view.selection.set(moved_tiles),
            Err(e) => cx.view.set_status(format!("Can't move there: {e}"), true),
        }
    }

    /// The release of a handle drag: [`crate::annot::plan_vertex_move`] as
    /// one entry. A release where the press was (a click) only highlights
    /// the handle (done at the press).
    fn move_vertex(
        &mut self,
        cx: &mut CommandContext<'_>,
        start: &CellHit,
        at: &CellHit,
        handle: crate::annot::Handle,
        modifiers: egui::Modifiers,
    ) {
        let d_raw = [
            at.world_px[0] - start.world_px[0],
            at.world_px[1] - start.world_px[1],
        ];
        if d_raw == [0.0, 0.0] {
            return;
        }
        let pixel = crate::tools::pixel_mod(modifiers);
        let plan = match crate::annot::plan_vertex_move(cx.doc, cx.view, handle, d_raw, pixel) {
            Ok(p) => p,
            Err(e) => {
                cx.view.set_status(e, true);
                return;
            }
        };
        let Some(cmd) = plan.command else {
            return;
        };
        let r = self.push(
            cx,
            Command::Batch {
                label: plan.label,
                commands: vec![cmd],
            },
        );
        self.entry_done(r.is_ok());
        if let Err(e) = r {
            cx.view.set_status(e, true);
        }
    }

    /// The release of an Alt+click on a segment of the one selected
    /// annotation: [`crate::annot::plan_vertex_insert`] at the press, one
    /// entry; the new point is highlighted.
    fn insert_vertex(
        &mut self,
        cx: &mut CommandContext<'_>,
        start: &CellHit,
        modifiers: egui::Modifiers,
    ) {
        let pixel = crate::tools::pixel_mod(modifiers);
        let zoom = cx.view.zoom;
        let Some(planned) =
            crate::annot::plan_vertex_insert(cx.doc, cx.view, start.world_px, zoom, pixel)
        else {
            return;
        };
        let (cmd, handle) = match planned {
            Ok(x) => x,
            Err(e) => {
                cx.view.set_status(e, true);
                return;
            }
        };
        let id = match &cmd {
            Command::UpdateNote { id, .. } => id.clone(),
            _ => return,
        };
        let r = self.push(
            cx,
            Command::Batch {
                label: format!("Add a point to {id}"),
                commands: vec![cmd],
            },
        );
        self.entry_done(r.is_ok());
        match r {
            Ok(()) => cx.view.vertex = Some((id, handle)),
            Err(e) => cx.view.set_status(e, true),
        }
    }

    fn cancel(&mut self, cx: &mut CommandContext<'_>) {
        self.gesture = None;
        self.entry_changes.clear();
        match self.history.cancel_gesture(&mut cx.doc.project) {
            Ok(changes) => self.after(cx, &changes),
            Err(e) => {
                let msg = self.failed(cx, e);
                cx.view.set_status(format!("Cancel failed: {msg}"), true)
            }
        }
    }

    /// The project's files were marked clean outside a command (the
    /// watcher applied a file from disk): refresh [`EditorCommands::has_unsaved`].
    pub fn sync_unsaved(&mut self, project: &worlds_core::Project) {
        self.unsaved = project.is_dirty();
    }

    /// Whether a gesture is open (tests).
    pub fn in_gesture(&self) -> bool {
        self.gesture.is_some()
    }
}

impl EditorCommands for Editing {
    fn tool_events(&mut self, cx: &mut CommandContext<'_>, events: &[ToolEvent]) {
        self.warm(cx);
        for e in events {
            match *e {
                ToolEvent::Begin {
                    tool,
                    at,
                    modifiers,
                    grab,
                } => self.begin(cx, tool, at, modifiers, grab),
                ToolEvent::Move { at, .. } => self.moved(cx, at),
                ToolEvent::End { at, modifiers, .. } => self.end(cx, at, modifiers),
                ToolEvent::Cancel { .. } => self.cancel(cx),
            }
        }
    }

    fn apply(&mut self, cx: &mut CommandContext<'_>, cmd: Command) -> Result<(), String> {
        self.gesture = None;
        self.close();
        let r = self.push(cx, cmd);
        // Outside a gesture the history recorded the command as an entry.
        self.entry_done(r.is_ok());
        r
    }

    fn can_undo(&self) -> bool {
        self.history.can_undo()
    }

    fn can_redo(&self) -> bool {
        self.history.can_redo()
    }

    fn undo_label(&self) -> Option<String> {
        self.history.undo_label().map(str::to_owned)
    }

    fn redo_label(&self) -> Option<String> {
        self.history.redo_label().map(str::to_owned)
    }

    fn undo(&mut self, cx: &mut CommandContext<'_>) {
        self.gesture = None;
        self.close();
        let label = self.undo_label();
        let origin = self.history.undo_entries().last().map(|e| e.origin.clone());
        match self.history.undo(&mut cx.doc.project) {
            Ok(Some(changes)) => {
                self.after(cx, &changes);
                self.user_edits.push(UserEdit {
                    kind: ChangeKind::Undo,
                    label: label.clone().unwrap_or_default(),
                    entry_origin: origin,
                    changes,
                });
                cx.view
                    .set_status(format!("Undid {}", label.unwrap_or_default()), false);
            }
            Ok(None) => {}
            Err(e) => {
                let msg = self.failed(cx, e);
                cx.view.set_status(format!("Undo failed: {msg}"), true)
            }
        }
    }

    fn redo(&mut self, cx: &mut CommandContext<'_>) {
        self.gesture = None;
        self.close();
        let label = self.redo_label();
        let origin = self.history.redo_entries().last().map(|e| e.origin.clone());
        match self.history.redo(&mut cx.doc.project) {
            Ok(Some(changes)) => {
                self.after(cx, &changes);
                self.user_edits.push(UserEdit {
                    kind: ChangeKind::Redo,
                    label: label.clone().unwrap_or_default(),
                    entry_origin: origin,
                    changes,
                });
                cx.view
                    .set_status(format!("Redid {}", label.unwrap_or_default()), false);
            }
            Ok(None) => {}
            Err(e) => {
                let msg = self.failed(cx, e);
                cx.view.set_status(format!("Redo failed: {msg}"), true)
            }
        }
    }

    fn has_unsaved(&self) -> bool {
        self.unsaved
    }

    fn save(&mut self, cx: &mut CommandContext<'_>) -> anyhow::Result<()> {
        let t = Instant::now();
        let doc = &mut *cx.doc;
        let n = doc.project.files.dirty().len();
        worlds_core::save_dirty(&mut doc.project, &doc.dir)?;
        self.unsaved = false;
        doc.problems = worlds_core::validate(&doc.project);
        let mut msg = format!(
            "Saved {n} change{} in {:.1} ms",
            if n == 1 { "" } else { "s" },
            t.elapsed().as_secs_f64() * 1e3
        );
        if doc.project.export_on_save {
            let files = export(doc)?;
            msg.push_str(&format!(", exported {files} files"));
        }
        if !doc.problems.is_empty() {
            msg.push_str(&format!(" ({} problems)", doc.problems.len()));
        }
        cx.view.set_status(msg, false);
        Ok(())
    }

    fn reloaded(&mut self) {
        *self = Editing::default();
    }

    fn editing(&mut self) -> Option<&mut Editing> {
        Some(self)
    }

    fn editing_ref(&self) -> Option<&Editing> {
        Some(self)
    }

    fn tileset_changed(&mut self, layers: &[Slug]) {
        for l in layers {
            self.cache.invalidate_layer(l.as_str());
        }
        // Seeds refused before may fit the fresh bakes.
        self.unseedable.clear();
    }
}

/// Write `export/` from the document's bakes (the cache keeps the edited
/// levels' bakes current; a level whose bake hasn't arrived, or has a stale
/// layer, is baked here).
/// Returns the number of files written or kept.
///
/// Each derived layer goes out with its effective grid
/// ([`Document::derived_effective`]: overrides over what its writer's
/// current bake computed, the bake the cache keeps current), so the bytes
/// equal `worlds_rules::bake_for_export`'s (`worlds-cli bake`).
pub fn export(doc: &Document) -> anyhow::Result<usize> {
    let mut baked: BTreeMap<Slug, BakedLevel> = BTreeMap::new();
    for ix in 0..doc.levels.len() {
        let level = doc.level(ix);
        let layers = match &doc.bakes[ix] {
            // A level with a layer still waiting for its rebake (after a
            // rule edit) is baked here instead.
            Some(b) if doc.stale_bakes[ix].is_empty() => {
                let tiles = BakedLayers::from_iter(b.iter().map(|(layer, bake)| {
                    let tiles = bake
                        .placements
                        .iter()
                        .map(|p| BakedTile {
                            px: p.px,
                            tileset: p.tileset.clone(),
                            tile: p.tile,
                            flip: p.flip,
                            alpha: p.alpha,
                        })
                        .collect();
                    (layer.clone(), tiles)
                }));
                let derived = doc
                    .project
                    .layers
                    .iter()
                    .filter(|d| d.is_derived())
                    .map(|d| (d.slug.clone(), doc.derived_effective(ix, d)))
                    .collect();
                BakedLevel { tiles, derived }
            }
            _ => worlds_rules::bake_for_export(&doc.project, level, &doc.images)?,
        };
        baked.insert(level.slug.clone(), layers);
    }
    let files = worlds_core::export::write(
        &doc.project,
        &doc.dir,
        &doc.dir.join("export"),
        &baked,
        true,
    )?;
    Ok(files.len())
}

/// Undo label of a painting gesture: "Paint collision".
fn gesture_label(tool: Tool, layer: &Slug) -> String {
    match tool {
        Tool::Eraser => format!("Erase {layer}"),
        Tool::Rect => format!("Rectangle on {layer}"),
        Tool::Line => format!("Line on {layer}"),
        Tool::Fill => format!("Fill {layer}"),
        _ => format!("Paint {layer}"),
    }
}

/// Why painting on this layer isn't allowed right now, if it isn't.
pub fn refusal(doc: &Document, view: &ViewState, layer: usize) -> Option<String> {
    let def = doc.project.layers.get(layer)?;
    if view.layer_locked(layer) {
        return Some(format!("Layer {} is locked", def.name));
    }
    if !view.layer_visible(layer) {
        return Some(format!("Layer {} is hidden", def.name));
    }
    None
}

/// What `tool` puts down on project layer `layer`, from the palette.
pub fn resolve_paint(
    doc: &Document,
    view: &ViewState,
    layer: usize,
    tool: Tool,
) -> Result<Paint, String> {
    let def = &doc.project.layers[layer];
    let brush = view.palette.brush(def.slug.as_str());
    match &def.kind {
        LayerKind::IntGrid(_) if tool == Tool::Eraser || brush == Some(&Brush::Erase) => {
            Ok(Paint::Value(GridValue::Clear))
        }
        LayerKind::IntGrid(_) if brush == Some(&Brush::Suppress) => {
            if def.is_derived() {
                Ok(Paint::Value(GridValue::Suppress))
            } else {
                Err(format!(
                    "Suppress (!) only applies to derived layers; {} isn't derived",
                    def.name
                ))
            }
        }
        LayerKind::IntGrid(_) => {
            let ix = match brush {
                Some(Brush::Value(i)) => *i,
                _ => 1,
            };
            def.values()
                .get((ix as usize).wrapping_sub(1))
                .map(|v| Paint::Value(GridValue::Value(v.slug.clone())))
                .ok_or_else(|| format!("Layer {} has no value #{ix}", def.name))
        }
        LayerKind::Tiles(_) if tool == Tool::Eraser || brush == Some(&Brush::Erase) => {
            Ok(Paint::ClearTiles)
        }
        LayerKind::Tiles(_) => match brush {
            Some(Brush::Tiles { tileset, rect }) => {
                let ts = doc
                    .project
                    .tileset(tileset.as_str())
                    .ok_or_else(|| format!("No tileset {tileset}"))?;
                Ok(Paint::Tiles {
                    tileset: tileset.clone(),
                    stamp: Stamp::new(*rect, ts.tile_size, doc.project.layer_grid(def)),
                })
            }
            _ => Err("Pick a tile in the palette first".into()),
        },
        LayerKind::Auto(a) => Err(format!(
            "{} is an auto layer: paint its source layer {}",
            def.name, a.source
        )),
        LayerKind::Entities => Err(format!(
            "{} is an entities layer: place entities with the Entity tool (T)",
            def.name
        )),
    }
}

/// The command painting `cells` (layer cells, may be outside: clipped).
/// Tiles go down as stamps on the stroke's lattice, or with `pattern` as
/// the stamp tiled over exactly those cells (rect fill, flood fill).
fn paint_command(g: &Gesture, cells: &[[i32; 2]], pattern: bool) -> Option<Command> {
    let Kind::Paint(paint) = &g.kind else {
        return None;
    };
    let inside = |c: &&[i32; 2]| in_layer(**c, g.size);
    let u = |c: [i32; 2]| [c[0] as u32, c[1] as u32];
    match paint {
        Paint::Value(v) => {
            let cells: Vec<CellValue> = cells
                .iter()
                .filter(inside)
                .map(|c| CellValue::new(c[0] as u32, c[1] as u32, v.clone()))
                .collect();
            (!cells.is_empty()).then(|| Command::PaintCells {
                level: g.level.clone(),
                layer: g.layer.clone(),
                cells,
                keep_empty: false,
            })
        }
        Paint::ClearTiles => {
            let clear: BTreeSet<[u32; 2]> = cells.iter().filter(inside).map(|c| u(*c)).collect();
            (!clear.is_empty()).then(|| Command::SetTiles {
                level: g.level.clone(),
                layer: g.layer.clone(),
                tiles: Vec::new(),
                clear: clear.into_iter().collect(),
                keep_empty: false,
            })
        }
        Paint::Tiles { tileset, stamp } => {
            let mut out: BTreeMap<[u32; 2], [u32; 2]> = BTreeMap::new();
            if pattern {
                for c in cells.iter().filter(inside) {
                    if let Some(t) = stamp.tile_at(g.origin, *c) {
                        out.insert(u(*c), t);
                    }
                }
            } else {
                let mut seen = HashSet::new();
                let mut tiles = Vec::new();
                for c in cells {
                    let a = stamp.anchor(g.origin, *c);
                    if seen.insert(a) {
                        stamp.tiles(a, &mut tiles);
                    }
                }
                for (c, t) in tiles {
                    if in_layer(c, g.size) {
                        out.insert(u(c), t);
                    }
                }
            }
            (!out.is_empty()).then(|| Command::SetTiles {
                level: g.level.clone(),
                layer: g.layer.clone(),
                tiles: out
                    .into_iter()
                    .map(|(cell, tile)| TilePlacement {
                        cell,
                        tileset: tileset.clone(),
                        tile,
                        flip: None,
                        alpha: 1.0,
                    })
                    .collect(),
                clear: Vec::new(),
                keep_empty: false,
            })
        }
    }
}

/// Fill (or outline) the cell rect `[x0, y0, x1, y1)`, clipped to the layer.
fn rect_command(g: &Gesture, r: [i32; 4], outline: bool) -> Option<Command> {
    let r = clip_rect(r, g.size);
    if r[0] >= r[2] || r[1] >= r[3] {
        return None;
    }
    if let (Kind::Paint(Paint::Value(v)), false) = (&g.kind, outline) {
        return Some(Command::FillRect {
            level: g.level.clone(),
            layer: g.layer.clone(),
            rect: Rect::new(
                r[0] as u32,
                r[1] as u32,
                (r[2] - r[0]) as u32,
                (r[3] - r[1]) as u32,
            ),
            value: v.clone(),
        });
    }
    // Outlines, and tiles: lay the stamp as a pattern from the rect's corner.
    let mut cells = Vec::new();
    rect_cells(r, outline, &mut cells);
    let g = Gesture {
        origin: [r[0], r[1]],
        ..g.clone()
    };
    paint_command(&g, &cells, true)
}

/// Flood fill at `cell`: `flood_fill` for int_grid layers; for tiles
/// layers, the 4-connected region of cells holding the same stack as
/// `cell` gets the stamp as a pattern.
fn fill_command(doc: &Document, g: &Gesture, ix: LevelIx, cell: [i32; 2]) -> Option<Command> {
    if !in_layer(cell, g.size) {
        return None;
    }
    let Kind::Paint(paint) = &g.kind else {
        return None;
    };
    // A derived layer floods its effective region (what the map shows:
    // overrides over what the rules computed), not the stored overrides:
    // one `paint_cells` with the brush over it.
    if let Paint::Value(_) = paint
        && let Some(def) = doc
            .project
            .layer(g.layer.as_str())
            .filter(|d| d.is_derived())
    {
        let grid = doc.derived_effective(ix, def);
        let region = value_region(&grid, g.size, cell);
        return paint_command(g, &region, true);
    }
    if let Paint::Value(v) = paint {
        return Some(Command::FloodFill {
            level: g.level.clone(),
            layer: g.layer.clone(),
            at: [cell[0] as u32, cell[1] as u32],
            value: v.clone(),
        });
    }
    let region = tile_region(doc, ix, &g.layer, g.size, cell);
    paint_command(g, &region, true)
}

/// The 4-connected cells around `start` holding the same value as it in
/// `grid`, row-major.
pub fn value_region(
    grid: &worlds_core::IntGridLayer,
    size: [u32; 2],
    start: [i32; 2],
) -> Vec<[i32; 2]> {
    let at = |c: [i32; 2]| grid.index_at(c[0] as u32, c[1] as u32).unwrap_or(0);
    let target = at(start);
    let mut seen = HashSet::from([start]);
    let mut queue = VecDeque::from([start]);
    let mut out = Vec::new();
    while let Some(c) = queue.pop_front() {
        out.push(c);
        for d in [[1, 0], [-1, 0], [0, 1], [0, -1]] {
            let n = [c[0] + d[0], c[1] + d[1]];
            if in_layer(n, size) && !seen.contains(&n) && at(n) == target {
                seen.insert(n);
                queue.push_back(n);
            }
        }
    }
    out.sort_by_key(|c| (c[1], c[0]));
    out
}

/// `layers` and every auto layer reading a derived grid one of them
/// writes, transitively ([`worlds_core::Project::with_dependents`]), in
/// bake order (writers first).
fn with_dependents(project: &worlds_core::Project, layers: BTreeSet<Slug>) -> Vec<Slug> {
    let set = project.with_dependents(layers);
    match project.auto_layer_order() {
        Ok(order) => order.into_iter().filter(|l| set.contains(l)).collect(),
        Err(_) => set.into_iter().collect(),
    }
}

/// The 4-connected cells around `start` whose tile stack equals its stack.
pub fn tile_region(
    doc: &Document,
    ix: LevelIx,
    layer: &Slug,
    size: [u32; 2],
    start: [i32; 2],
) -> Vec<[i32; 2]> {
    type Stack<'a> = Vec<(&'a Slug, [u32; 2], Option<worlds_core::Flip>, u64)>;
    let mut stacks: HashMap<[i32; 2], Stack<'_>> = HashMap::new();
    if let Some(t) = doc.level(ix).tiles_layer(layer.as_str()) {
        for p in &t.tiles {
            stacks
                .entry([p.cell[0] as i32, p.cell[1] as i32])
                .or_default()
                .push((&p.tileset, p.tile, p.flip, p.alpha.to_bits()));
        }
    }
    let empty = Stack::new();
    let stack = |c: &[i32; 2]| stacks.get(c).unwrap_or(&empty);
    let target = stack(&start).clone();
    let mut seen = HashSet::from([start]);
    let mut queue = VecDeque::from([start]);
    let mut out = Vec::new();
    while let Some(c) = queue.pop_front() {
        out.push(c);
        for d in [[1, 0], [-1, 0], [0, 1], [0, -1]] {
            let n = [c[0] + d[0], c[1] + d[1]];
            if in_layer(n, size) && !seen.contains(&n) && *stack(&n) == target {
                seen.insert(n);
                queue.push_back(n);
            }
        }
    }
    out.sort_by_key(|c| (c[1], c[0]));
    out
}

/// Alt-click: put the value or tile under the cursor in the palette.
fn sample(doc: &Document, view: &mut ViewState, at: CellHit) {
    let def = &doc.project.layers[at.layer];
    let level = doc.level(at.level);
    if !at.inside {
        return;
    }
    let [x, y] = [at.cell[0] as u32, at.cell[1] as u32];
    let slug = def.slug.to_string();
    match &def.kind {
        // A derived layer: the stored state (the override plane).
        LayerKind::IntGrid(_) if def.is_derived() => {
            let (brush, msg) = match level.overrides(def.slug.as_str()).and_then(|o| o.get(x, y)) {
                Some(Override::Value(i)) => {
                    let name = def
                        .values()
                        .get(i as usize - 1)
                        .map_or("?", |v| v.name.as_str());
                    (Brush::Value(i), format!("Picked override {name}"))
                }
                Some(Override::Suppress) => (Brush::Suppress, "Picked Suppress (!)".to_string()),
                _ => (
                    Brush::Erase,
                    "No override here: picked Erase (back to the rules)".to_string(),
                ),
            };
            view.palette.brushes.insert(slug, brush);
            view.set_status(msg, false);
        }
        LayerKind::IntGrid(_) => {
            match level
                .int_grid(def.slug.as_str())
                .and_then(|g| g.index_at(x, y))
                .filter(|&i| i > 0)
            {
                Some(i) => {
                    let name = def
                        .values()
                        .get(i as usize - 1)
                        .map_or("?", |v| v.name.as_str());
                    view.palette.brushes.insert(slug, Brush::Value(i));
                    view.set_status(format!("Picked {name}"), false);
                }
                None => view.set_status("Empty cell: nothing to pick", false),
            }
        }
        LayerKind::Tiles(_) => {
            let g = doc.project.layer_grid(def).max(1);
            let found = level.tiles_layer(def.slug.as_str()).and_then(|t| {
                t.tiles.iter().rev().find(|p| {
                    let ts = doc
                        .project
                        .tileset(p.tileset.as_str())
                        .map_or([g, g], |t| t.tile_size);
                    // A transposed tile is drawn its height wide.
                    let ts = if worlds_core::Flip::bits_of(p.flip) & 4 != 0 {
                        [ts[1], ts[0]]
                    } else {
                        ts
                    };
                    let span = [(ts[0] / g).max(1), (ts[1] / g).max(1)];
                    x >= p.cell[0]
                        && y >= p.cell[1]
                        && x < p.cell[0] + span[0]
                        && y < p.cell[1] + span[1]
                })
            });
            match found {
                Some(p) => {
                    view.palette.brushes.insert(
                        slug,
                        Brush::Tiles {
                            tileset: p.tileset.clone(),
                            rect: [p.tile[0], p.tile[1], 1, 1],
                        },
                    );
                    view.set_status(
                        format!("Picked {} [{}, {}]", p.tileset, p.tile[0], p.tile[1]),
                        false,
                    );
                }
                None => view.set_status("Empty cell: nothing to pick", false),
            }
        }
        _ => view.set_status(
            format!("Nothing to pick on {} layers", def.kind.name()),
            false,
        ),
    }
}

/// Why the entity tool can't place on project layer `layer`, if it can't.
pub fn entity_refusal(doc: &Document, view: &ViewState, layer: usize) -> Option<String> {
    let def = doc.project.layers.get(layer)?;
    if !def.is_entities() {
        return Some(format!(
            "{} is a {} layer: the Entity tool places on entities layers",
            def.name,
            def.kind.name()
        ));
    }
    refusal(doc, view, layer)
}

/// An entity id no instance in the project has yet.
pub fn fresh_entity_id(project: &worlds_core::Project) -> Id {
    loop {
        let id = Id::generate();
        if !project
            .levels()
            .any(|l| l.all_entities().any(|e| e.id == id))
        {
            return id;
        }
    }
}

/// The instances of `ty` within a limit's scope, oldest first (file
/// order: layers as stored in each level, levels in project order), with
/// the level and layer each is in.
fn in_scope<'a>(
    project: &'a worlds_core::Project,
    ty: &Slug,
    scope: worlds_core::LimitScope,
    level: &Slug,
    layer: &Slug,
) -> Vec<(&'a Slug, &'a Slug, &'a worlds_core::EntityInstance)> {
    use worlds_core::LimitScope;
    let mut out = Vec::new();
    for l in project.levels() {
        if scope != LimitScope::World && l.slug != *level {
            continue;
        }
        for el in &l.entities {
            if scope == LimitScope::Layer && el.layer != *layer {
                continue;
            }
            for e in el.entities.iter().filter(|e| e.ty == *ty) {
                out.push((&l.slug, &el.layer, e));
            }
        }
    }
    out
}

/// The commands that place `entity` in `level`/`layer`, honouring the
/// type's `limit` (`limit.behavior` within `limit.scope`):
///
/// - under the limit: `add_entity`;
/// - `prevent_adding` (or `max` 0): refused, with the reason;
/// - `discard_oldest`: `remove_entity` of the oldest instances in scope
///   (enough to make room), then `add_entity`;
/// - `move_last_one`: the newest instance in scope moves here
///   (`move_entity`, plus `resize_entity` when the size changes); when it
///   is on another layer or in another level it is removed and added here
///   with its id and fields, since `move_entity` stays on its layer.
///
/// Commands and the API stay structural (they never apply limits);
/// validation reports any overrun.
pub fn limit_commands(
    doc: &Document,
    def: &worlds_core::EntityDef,
    level: &Slug,
    layer: &Slug,
    entity: worlds_core::EntityInstance,
    p: crate::tools::Placement,
) -> Result<Vec<Command>, String> {
    use worlds_core::{LimitBehavior, LimitScope};
    let add = |entity: worlds_core::EntityInstance| Command::AddEntity {
        level: level.clone(),
        layer: layer.clone(),
        entity,
        index: None,
        allow_outside: false,
    };
    let Some(limit) = &def.limit else {
        return Ok(vec![add(entity)]);
    };
    let found = in_scope(&doc.project, &def.slug, limit.scope, level, layer);
    let max = limit.max as usize;
    if found.len() < max {
        return Ok(vec![add(entity)]);
    }
    let scope = match limit.scope {
        LimitScope::Layer => "layer",
        LimitScope::Level => "level",
        LimitScope::World => "project",
    };
    if limit.behavior == LimitBehavior::PreventAdding || max == 0 {
        return Err(format!(
            "{} allows at most {max} per {scope} ({} placed)",
            def.name,
            found.len()
        ));
    }
    match limit.behavior {
        LimitBehavior::DiscardOldest => {
            let mut cmds: Vec<Command> = found[..found.len() + 1 - max]
                .iter()
                .map(|(_, _, e)| Command::RemoveEntity {
                    id: e.id.clone(),
                    keep_empty: false,
                })
                .collect();
            cmds.push(add(entity));
            Ok(cmds)
        }
        _ => {
            let (l, el, newest) = found[found.len() - 1];
            if l == level && el == layer {
                let mut cmds = vec![Command::MoveEntity {
                    id: newest.id.clone(),
                    cell: p.cell,
                    offset_px: p.offset_px,
                    allow_outside: false,
                }];
                if newest.size != p.size {
                    cmds.push(Command::ResizeEntity {
                        id: newest.id.clone(),
                        size: p.size,
                    });
                }
                Ok(cmds)
            } else {
                let moved = worlds_core::EntityInstance {
                    id: newest.id.clone(),
                    fields: newest.fields.clone(),
                    ..entity
                };
                Ok(vec![
                    Command::RemoveEntity {
                        id: newest.id.clone(),
                        keep_empty: false,
                    },
                    add(moved),
                ])
            }
        }
    }
}

/// Alt-click with the entity tool: the type and fields of the entity under
/// the pointer become the brush.
fn sample_entity(doc: &Document, view: &mut ViewState, at: CellHit) {
    let found = crate::viewport::entity_at(doc, view, at.level, at.world_px).and_then(|id| {
        doc.level(at.level)
            .all_entities()
            .find(|e| e.id == id)
            .cloned()
    });
    match found {
        Some(e) => match doc.project.entity_def(e.ty.as_str()) {
            Some(def) => {
                view.set_status(format!("Picked {} with its fields", e.ty), false);
                view.entities.brush = Some(crate::tools::EntityBrush::with_fields(def, e.fields));
            }
            None => view.set_status(format!("No entity type {}: nothing to pick", e.ty), false),
        },
        None => view.set_status("No entity here: nothing to pick", false),
    }
}

/// Split the selected entities `ids` into those that may be edited and why
/// the others may not: an entity on a locked layer is left alone by every
/// selection edit (drag, nudge, duplicate, delete), as by the tools.
/// Unknown ids are dropped silently.
pub fn unlocked_entities(
    doc: &Document,
    view: &ViewState,
    ids: &[Id],
) -> (Vec<Id>, Option<String>) {
    let mut ok = Vec::new();
    let mut locked: Vec<&str> = Vec::new();
    for id in ids {
        let Some((_, layer, _)) = doc.find_entity(id) else {
            continue;
        };
        let ix = doc.project.layers.iter().position(|l| l.slug == *layer);
        match ix.filter(|&i| view.layer_locked(i)) {
            Some(i) => locked.push(doc.project.layers[i].name.as_str()),
            None => ok.push(id.clone()),
        }
    }
    let why = match locked.as_slice() {
        [] => None,
        [name] if ok.is_empty() => Some(format!("Layer {name} is locked")),
        [_] => Some("1 selected entity is on a locked layer: left alone".into()),
        many => Some(format!(
            "{} selected entities are on locked layers: left alone",
            many.len()
        )),
    };
    (ok, why)
}

/// The `move_entity` commands that drag entities `ids` by `delta` world
/// pixels ([`crate::tools::dragged_position`]: whole cells of each one's
/// layer, or exact pixels with `pixel`); entities that don't move are left
/// out.
pub fn entity_move_commands(
    doc: &Document,
    ids: &[Id],
    delta: [f64; 2],
    pixel: bool,
) -> Vec<Command> {
    ids.iter()
        .filter_map(|id| {
            let (_, layer, e) = doc.find_entity(id)?;
            let g = doc
                .project
                .layer(layer.as_str())
                .map_or(doc.project.grid_size, |d| doc.project.layer_grid(d));
            let (cell, offset_px) =
                crate::tools::dragged_position(e.cell, e.offset_px, g, delta, pixel);
            ((cell, offset_px) != (e.cell, e.offset_px)).then(|| Command::MoveEntity {
                id: id.clone(),
                cell,
                offset_px,
                allow_outside: false,
            })
        })
        .collect()
}

/// The arrow-key nudge of entities `ids` by `d`: a cell, or with `pixel`
/// one pixel into `offset_px` (folded into `cell` whenever it reaches the
/// layer grid; [`crate::tools::nudged`]).
pub fn nudge_commands(doc: &Document, ids: &[Id], d: [i32; 2], pixel: bool) -> Vec<Command> {
    ids.iter()
        .filter_map(|id| {
            let (_, layer, e) = doc.find_entity(id)?;
            let g = doc
                .project
                .layer(layer.as_str())
                .map_or(doc.project.grid_size, |d| doc.project.layer_grid(d));
            let (cell, offset_px) = crate::tools::nudged(e.cell, e.offset_px, g, d, pixel);
            Some(Command::MoveEntity {
                id: id.clone(),
                cell,
                offset_px,
                allow_outside: false,
            })
        })
        .collect()
}

/// `remove_entity` of each of `ids` the project has.
pub fn delete_commands(doc: &Document, ids: &[Id]) -> Vec<Command> {
    ids.iter()
        .filter(|id| doc.find_entity(id).is_some())
        .map(|id| Command::RemoveEntity {
            id: id.clone(),
            keep_empty: false,
        })
        .collect()
}

/// What Ctrl-D does to the selected entities ([`duplicate_commands`]).
#[derive(Debug, Clone, PartialEq)]
pub struct Duplicates {
    /// One `add_entity` per copy.
    pub commands: Vec<Command>,
    /// The copies' ids, in the selection's order: they become the
    /// selection.
    pub ids: Vec<Id>,
    /// How many copies sit on their original's cell (the cell to the right
    /// is outside the level).
    pub overlapping: usize,
}

/// Ctrl-D: a copy of each of `ids` one cell to the right (of its layer's
/// grid) in the same level and layer, or on the same cell when that one is
/// outside the level. Field values are kept as they are, references
/// included; each copy gets a new id.
pub fn duplicate_commands(doc: &Document, ids: &[Id]) -> Duplicates {
    let mut out = Duplicates {
        commands: Vec::new(),
        ids: Vec::new(),
        overlapping: 0,
    };
    for id in ids {
        let Some((ix, layer, e)) = doc.find_entity(id) else {
            continue;
        };
        let level = doc.level(ix);
        let size = doc
            .project
            .layer(layer.as_str())
            .map_or(level.size, |d| doc.project.layer_cells(d, level));
        let right = [e.cell[0] + 1, e.cell[1]];
        let cell = if in_layer(right, size) {
            right
        } else {
            out.overlapping += 1;
            e.cell
        };
        let new_id = loop {
            let n = fresh_entity_id(&doc.project);
            if !out.ids.contains(&n) {
                break n;
            }
        };
        out.ids.push(new_id.clone());
        out.commands.push(Command::AddEntity {
            level: level.slug.clone(),
            layer: layer.clone(),
            entity: worlds_core::EntityInstance {
                id: new_id,
                cell,
                ..e.clone()
            },
            index: None,
            allow_outside: false,
        });
    }
    out
}

/// The end of a marquee from `start` to `at`: what it touches
/// ([`crate::annot::marquee_items`]: in the start's level, or on the world)
/// becomes the selection, or with `add` joins it.
fn marquee_select(
    cx: &mut CommandContext<'_>,
    start: &CellHit,
    at: &CellHit,
    world: bool,
    add: bool,
) {
    if !world && start.level >= cx.doc.levels.len() {
        return;
    }
    let r = crate::viewport::camera::WRect::new(
        [
            start.world_px[0].min(at.world_px[0]),
            start.world_px[1].min(at.world_px[1]),
        ],
        [
            start.world_px[0].max(at.world_px[0]),
            start.world_px[1].max(at.world_px[1]),
        ],
    );
    let level = (!world).then_some(start.level);
    let hits = if r.width() == 0.0 && r.height() == 0.0 {
        Vec::new()
    } else {
        crate::annot::marquee_items(cx.doc, cx.view, level, &r)
    };
    // With an int_grid layer active, the marquee's cells are what Copy
    // takes from it.
    let cells = level.and_then(|ix| crate::clipboard::grid_marquee(cx.doc, cx.view, ix, &r));
    if cells.is_some() || !add {
        cx.view.grid_marquee = cells;
    }
    if add {
        for i in hits {
            cx.view.selection.add(i);
        }
    } else {
        cx.view.selection.set(hits);
    }
}
