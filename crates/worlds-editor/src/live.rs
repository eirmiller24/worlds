//! The live API inside the editor: [`EditorHost`] lends the editor's
//! document to `worlds-server`'s handler.
//!
//! The editor's UI thread owns the document. [`EditorApp`](crate::app::EditorApp)
//! starts a server on project open and drains its request queues once per
//! frame, after the tool events, when no gesture is in progress (see the
//! README, "Live API"). Each drain borrows the document, the command layer
//! ([`Editing`]: history and bake cache), the view state and the
//! invalidation list through an `EditorHost`:
//!
//! - [`Host::applied`] is [`Editing::after`], the path every user edit takes
//!   (chunk invalidations, incremental rebake, re-index on level create,
//!   delete and rename), so an agent edit is on the canvas next frame;
//! - `save` is the editor's own save (dirty files, `export_on_save`,
//!   problems rerun, own-write bookkeeping for the file watcher);
//! - `reload` re-reads the project and fixes up the view; the app finishes
//!   it after the drain (scene, bakes, watcher) from [`HostEffects`];
//! - `focus`, `select` and `snapshot` act on the view; `snapshot` renders
//!   through [`SnapshotRenderer`].

use std::path::PathBuf;

use worlds_core::command::{Change, CommandError, Entry, Origin};
use worlds_core::{History, Problem};
use worlds_server::{Doc, Host, RpcError, Selection, Snapshot, SnapshotRequest, ViewTarget};

use crate::app::watch::OwnWrites;
use crate::doc::Document;
use crate::edit::Editing;
use crate::snapshot::SnapshotRenderer;
use crate::tools::{CommandContext, EditorCommands, Invalidation};
use crate::view::{SelItem, ViewState};
use crate::viewport::Fit;
use crate::viewport::camera::WRect;

/// What a drain did that the app finishes afterwards (it needs the
/// viewport, the GPU or the server, which the host doesn't hold).
#[derive(Default)]
pub struct HostEffects {
    /// `view.focus`: frame this.
    pub focus: Option<Fit>,
    /// `project.reload` replaced the document; this is the old one.
    pub reloaded: Option<Document>,
    /// A save recomputed the problems: send `problems`.
    pub problems: bool,
}

/// The editor's side of the live API seam (see the module docs).
pub struct EditorHost<'a> {
    pub doc: &'a mut Document,
    pub editing: &'a mut Editing,
    pub view: &'a mut ViewState,
    pub invalidate: &'a mut Vec<Invalidation>,
    pub own_writes: &'a mut OwnWrites,
    /// Created on the first `view.snapshot`.
    pub snapshots: &'a mut Option<SnapshotRenderer>,
    pub effects: &'a mut HostEffects,
    /// A `--synthetic` world: saving and reloading are refused.
    pub synthetic: bool,
    /// `project.describe`'s `recovered` (see [`Host::recovered`]).
    pub recovered: Option<serde_json::Value>,
}

impl Host for EditorHost<'_> {
    fn recovered(&mut self) -> Option<serde_json::Value> {
        self.recovered.clone()
    }

    fn doc(&mut self) -> Doc<'_> {
        let (history, bakes) = self.editing.parts_mut();
        Doc {
            project: &mut self.doc.project,
            history,
            bakes,
            images: &self.doc.images,
            dir: &self.doc.dir,
        }
    }

    fn applied(&mut self, changes: &[Change]) {
        let editing = &mut *self.editing;
        let mut cx = CommandContext {
            doc: self.doc,
            view: self.view,
            invalidate: self.invalidate,
        };
        editing.after(&mut cx, changes);
    }

    fn rollback(&mut self) {
        let editing = &mut *self.editing;
        let mut cx = CommandContext {
            doc: self.doc,
            view: self.view,
            invalidate: self.invalidate,
        };
        editing.reset_caches(&mut cx);
    }

    fn reload(&mut self) -> Result<(), RpcError> {
        if self.synthetic {
            return Err(RpcError::unsupported(
                "this editor shows a --synthetic world, which can't be reloaded",
            ));
        }
        let fresh = Document::open(&self.doc.dir).map_err(|e| RpcError::from_format(&e))?;
        let old = std::mem::replace(self.doc, fresh);
        sync_view_after_reload(self.view, &old, self.doc);
        self.editing.reloaded();
        self.invalidate.push(Invalidation::All);
        // Keep the first replaced document if two reloads share a drain.
        self.effects.reloaded.get_or_insert(old);
        Ok(())
    }

    fn save(&mut self) -> Result<Vec<String>, RpcError> {
        if self.synthetic {
            return Err(RpcError::unsupported(
                "this editor shows a --synthetic world, which can't be saved",
            ));
        }
        let dir = self.doc.dir.clone();
        let dirty: Vec<PathBuf> = self.doc.project.files.dirty().iter().cloned().collect();
        let files = worlds_server::saved_paths(&dirty);
        let editing = &mut *self.editing;
        let mut cx = CommandContext {
            doc: self.doc,
            view: self.view,
            invalidate: self.invalidate,
        };
        let res = editing.save(&mut cx);
        // Also after a failed save: whatever it did write is ours.
        self.own_writes.record(&dir, &dirty);
        res.map_err(|e| RpcError::io(dir.display().to_string(), format!("save failed: {e:#}")))?;
        self.effects.problems = true;
        Ok(files)
    }

    fn problems(&mut self, problems: &[Problem]) {
        self.doc.problems = problems.to_vec();
    }

    fn focus(&mut self, target: &ViewTarget) -> Result<(), RpcError> {
        match target {
            ViewTarget::Level(slug) => {
                let ix = self
                    .doc
                    .level_ix(slug.as_str())
                    .ok_or_else(|| RpcError::command(&CommandError::UnknownLevel(slug.clone())))?;
                self.view.active_level = Some(ix);
                self.view.sync_world(self.doc);
                self.effects.focus = Some(Fit::Level(ix));
            }
            ViewTarget::WorldRect { world, rect } => {
                if let Some(w) = world
                    && self.doc.project.world(w.as_str()).is_none()
                {
                    return Err(RpcError::command(&CommandError::UnknownWorld(w.clone())));
                }
                if rect[2] <= 0 || rect[3] <= 0 {
                    return Err(RpcError::invalid_params(format!(
                        "world_rect {rect:?} needs a positive width and height"
                    )));
                }
                if let Some(w) = crate::snapshot::target_world(self.doc, target) {
                    self.view.set_world(self.doc, w);
                }
                self.effects.focus = Some(Fit::Rect(WRect::from_pos_size(
                    [rect[0] as f64, rect[1] as f64],
                    [rect[2] as f64, rect[3] as f64],
                )));
            }
            // An annotation: its box as built, grown by 15 % (at least 8
            // cells each way); a level note's level becomes active.
            ViewTarget::Note(id) => {
                let (rect, level) = crate::annot::focus_rect(self.doc, id)
                    .ok_or_else(|| RpcError::command(&CommandError::UnknownNote(id.clone())))?;
                if let Some(w) = crate::snapshot::target_world(self.doc, target) {
                    self.view.set_world(self.doc, w);
                }
                if let Some(ix) = level {
                    self.view.active_level = Some(ix);
                }
                self.effects.focus = Some(Fit::Rect(rect));
            }
            // A world: it is shown, fitted.
            ViewTarget::World(w) => {
                let wi = crate::snapshot::target_world(self.doc, target)
                    .ok_or_else(|| RpcError::command(&CommandError::UnknownWorld(w.clone())))?;
                self.view.set_world(self.doc, wi);
                self.effects.focus = Some(Fit::World);
            }
        }
        Ok(())
    }

    /// `view.select`: `level` alone makes that level active and clears
    /// the selection; otherwise every listed entity, annotation and level
    /// is selected (in that order: entities, notes, levels; the first item
    /// is primary). The active level becomes the first entity's level, else
    /// the first level-scope note's level, else the first listed level,
    /// else stays. Every id is checked before the view changes.
    fn select(&mut self, selection: &Selection) -> Result<(), RpcError> {
        if let Some(slug) = &selection.level {
            let ix = self
                .doc
                .level_ix(slug.as_str())
                .ok_or_else(|| RpcError::command(&CommandError::UnknownLevel(slug.clone())))?;
            self.view.active_level = Some(ix);
            self.view.selection.clear();
            return Ok(());
        }
        let mut items = Vec::new();
        let mut level = None;
        for id in &selection.entities {
            let ix = self
                .doc
                .find_entity(id)
                .map(|f| f.0)
                .ok_or_else(|| RpcError::command(&CommandError::UnknownEntity(id.clone())))?;
            level.get_or_insert(ix);
            items.push(SelItem::Entity(id.clone()));
        }
        for id in &selection.notes {
            let r = crate::annot::find_note(self.doc, id)
                .ok_or_else(|| RpcError::command(&CommandError::UnknownNote(id.clone())))?;
            if level.is_none() {
                level = r.level();
            }
            items.push(SelItem::Note(id.clone()));
        }
        for slug in &selection.levels {
            let ix = self
                .doc
                .level_ix(slug.as_str())
                .ok_or_else(|| RpcError::command(&CommandError::UnknownLevel(slug.clone())))?;
            level.get_or_insert(ix);
            items.push(SelItem::Level(self.doc.ids[ix].clone()));
        }
        if let Some(ix) = level {
            self.view.active_level = Some(ix);
        }
        self.view.selection.set(items);
        Ok(())
    }

    fn snapshot(&mut self, request: &SnapshotRequest) -> Result<Snapshot, RpcError> {
        if self.snapshots.is_none() {
            let r = SnapshotRenderer::new().map_err(|e| {
                RpcError::unsupported(format!(
                    "view.snapshot: this editor has no GPU adapter for offscreen rendering: {e:#}"
                ))
            })?;
            *self.snapshots = Some(r);
        }
        // Never show an agent a layer older than its last edit: rebake the
        // layers whose worker job hasn't landed yet (after a rule edit).
        for (ix, layer) in self.doc.refresh_stale() {
            self.invalidate
                .push(Invalidation::Layer { level: ix, layer });
        }
        let r = self.snapshots.as_mut().expect("created above");
        r.render(self.doc, self.view, request)
    }
}

/// Carry the view over to a reloaded document: layer settings and the
/// active layer by slug, the active level by slug, the selected entities
/// the new document still has (entities, annotations and levels by id,
/// tile stacks still there), no pick or drawing in progress.
pub fn sync_view_after_reload(view: &mut ViewState, old: &Document, new: &Document) {
    let old_active = view.active_layer;
    view.sync_layers(new, Some((old, old_active)));
    view.active_level = view
        .active_level
        .filter(|&i| i < old.levels.len())
        .and_then(|i| new.level_ix(old.level(i).slug.as_str()));
    view.selection.prune(&new.project);
    view.pick = None;
    view.drawing = None;
    view.picked = None;
}

/// What the `selection` event reports for a view: the active level's slug
/// (`level` is the active level, whatever is selected), the selected
/// entities (primary first), annotations and levels (slugs), each in
/// selection order. Tile stacks are not reported (they have no id).
#[allow(clippy::type_complexity)]
pub fn selection_of(
    doc: &Document,
    view: &ViewState,
) -> (Option<String>, Vec<String>, Vec<String>, Vec<String>) {
    let level = view
        .active_level
        .filter(|&i| i < doc.levels.len())
        .map(|i| doc.level(i).slug.to_string());
    let entities = view.selection.entities().map(|i| i.to_string()).collect();
    let notes = view.selection.notes().map(|i| i.to_string()).collect();
    let levels = view
        .selection
        .levels()
        .filter_map(|id| doc.level_ix_by_id(id))
        .map(|ix| doc.level(ix).slug.to_string())
        .collect();
    (level, entities, notes, levels)
}

/// An agent entry a toast can undo: its depth in the undo stack and the
/// entry itself.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentUndo {
    pub depth: usize,
    pub entry: Entry,
}

/// The agent entries recorded on top of the first `before` entries of the
/// undo stack (one toast each).
pub fn new_agent_entries(history: &History, before: usize) -> Vec<AgentUndo> {
    history
        .undo_entries()
        .iter()
        .enumerate()
        .skip(before)
        .filter(|(_, e)| matches!(e.origin, Origin::Agent(_)))
        .map(|(depth, e)| AgentUndo {
            depth,
            entry: e.clone(),
        })
        .collect()
}

/// Whether `u`'s entry is still exactly the top of the undo stack (the
/// toast's Undo works only then).
pub fn agent_entry_on_top(history: &History, u: &AgentUndo) -> bool {
    let e = history.undo_entries();
    e.len() == u.depth + 1 && e.last() == Some(&u.entry)
}
