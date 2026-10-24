//! The editor's side of layer operations (duplicate, merge down, move the
//! selection to a layer), reference images (add one with the file dialog)
//! and the History pane's jump: what their [`Action`]s and `--script`
//! steps do. The commands come from worlds-core
//! (`worlds_core::command::layer_ops`, `duplicate_layer`,
//! `add_reference`); each action is one undo entry.

use std::collections::BTreeMap;

use worlds_core::command::{NoteScope, layer_ops};
use worlds_core::{Command, Id, Slug};

use super::actions::Action;
use super::{EditorApp, Pane, references, show_pane};
use crate::view::SelItem;

impl EditorApp {
    /// The undo history (what the History pane lists), with a project open.
    pub fn history(&self) -> Option<&worlds_core::History> {
        self.commands.editing_ref().map(|e| e.history())
    }

    /// The layer an action names, else the active one.
    fn action_layer(&self, layer: Option<Slug>) -> Result<Slug, String> {
        let doc = self.st.doc.as_ref().ok_or("no project open")?;
        match layer {
            Some(l) => doc
                .project
                .layer(l.as_str())
                .map(|d| d.slug.clone())
                .ok_or_else(|| format!("no layer {l}")),
            None => self
                .st
                .view
                .active_layer
                .and_then(|i| doc.project.layers.get(i))
                .map(|d| d.slug.clone())
                .ok_or_else(|| "pick a layer first (the Layers pane)".to_string()),
        }
    }

    fn report(&mut self, r: Result<String, String>) {
        match r {
            Ok(msg) => self.st.view.set_status(msg, false),
            Err(e) => {
                self.st.view.set_status(e.clone(), true);
                self.st.toast(e, true);
            }
        }
    }

    /// Duplicate a layer as `<slug>_copy` (`_copy_2`, ... when taken),
    /// named "<name> copy", right above it; the copy becomes active. One
    /// entry: `duplicate_layer`, then the new name.
    pub(super) fn duplicate_layer(&mut self, layer: Option<Slug>) {
        let r = self.try_duplicate_layer(layer);
        self.report(r);
    }

    fn try_duplicate_layer(&mut self, layer: Option<Slug>) -> Result<String, String> {
        let slug = self.action_layer(layer)?;
        let doc = self.st.doc.as_ref().ok_or("no project open")?;
        let def = doc.project.layer(slug.as_str()).ok_or("no such layer")?;
        let new =
            super::defs::unique_slug(&format!("{slug}_copy"), |s| doc.project.layer(s).is_some());
        let mut renamed = def.clone();
        renamed.slug = new.clone();
        renamed.name = format!("{} copy", def.name);
        self.apply_command(Command::Batch {
            label: format!("Duplicate layer {slug}"),
            commands: vec![
                Command::DuplicateLayer {
                    layer: slug.clone(),
                    slug: new.clone(),
                },
                Command::SetLayerDef {
                    layer: new.clone(),
                    def: renamed,
                    index: None,
                    content: Vec::new(),
                    rules: None,
                },
            ],
        })?;
        if let Some(doc) = &self.st.doc {
            self.st.view.active_layer = doc.project.layers.iter().position(|l| l.slug == new);
            self.st.view.references.active = false;
        }
        Ok(format!("Duplicated {slug} as {new}"))
    }

    /// Merge a tiles layer into the tiles layer below it (one entry).
    pub(super) fn merge_layer_down(&mut self, layer: Option<Slug>) {
        let r = self.try_merge_layer_down(layer);
        self.report(r);
    }

    fn try_merge_layer_down(&mut self, layer: Option<Slug>) -> Result<String, String> {
        let slug = self.action_layer(layer)?;
        let doc = self.st.doc.as_ref().ok_or("no project open")?;
        let lower = layer_ops::merge_target(&doc.project, &slug)
            .map_err(|e| format!("Can't merge {slug} down: {e}"))?
            .slug
            .clone();
        let cmd = layer_ops::merge_down(&doc.project, &slug).map_err(|e| e.to_string())?;
        self.apply_command(cmd)?;
        if let Some(doc) = &self.st.doc {
            self.st.view.active_layer = doc.project.layers.iter().position(|l| l.slug == lower);
        }
        Ok(format!("Merged {slug} down into {lower}"))
    }

    /// Move the selected tile stacks (to a tiles layer) or entities (to an
    /// entities layer) to `layer` (one entry). Tile stacks stay selected
    /// on their new layer.
    pub(super) fn move_selection_to_layer(&mut self, layer: Option<Slug>) {
        let r = self.try_move_selection(layer);
        self.report(r);
    }

    fn try_move_selection(&mut self, layer: Option<Slug>) -> Result<String, String> {
        let to = self.action_layer(layer)?;
        let doc = self.st.doc.as_ref().ok_or("no project open")?;
        let def = doc.project.layer(to.as_str()).ok_or("no such layer")?;
        let sel = &self.st.view.selection;
        if def.is_tiles() {
            // Per level and source layer, in selection order.
            let mut groups: BTreeMap<(Id, Slug), Vec<[u32; 2]>> = BTreeMap::new();
            for (level, from, cell) in sel.tiles() {
                if *from != to {
                    groups
                        .entry((level.clone(), from.clone()))
                        .or_default()
                        .push(cell);
                }
            }
            if groups.is_empty() {
                return Err(format!(
                    "Select tile stacks on another tiles layer to move them to {to}"
                ));
            }
            let mut commands = Vec::new();
            let mut moved = Vec::new();
            for ((level_id, from), cells) in &groups {
                let ix = doc
                    .level_ix_by_id(level_id)
                    .ok_or("a selected level is gone")?;
                let level = doc.level(ix).slug.clone();
                let c = layer_ops::move_tiles(&doc.project, &level, from, &to, cells)
                    .map_err(|e| e.to_string())?;
                if !c.is_noop() {
                    commands.push(c);
                    moved.extend(cells.iter().map(|&c| (level_id.clone(), c)));
                }
            }
            let n = moved.len();
            if n == 0 {
                return Err("The selected cells hold no tiles".into());
            }
            let keep: Vec<SelItem> = sel
                .items
                .iter()
                .filter(|i| !matches!(i, SelItem::Tiles { .. }))
                .cloned()
                .chain(moved.into_iter().map(|(level, cell)| SelItem::Tiles {
                    level,
                    layer: to.clone(),
                    cell,
                }))
                .collect();
            self.apply_command(Command::Batch {
                label: format!(
                    "Move {n} tile stack{} to {to}",
                    if n == 1 { "" } else { "s" }
                ),
                commands,
            })?;
            self.st.view.selection.set(keep);
            Ok(format!(
                "Moved {n} tile stack{} to {to}",
                if n == 1 { "" } else { "s" }
            ))
        } else if def.is_entities() {
            let ids: Vec<Id> = sel.entities().cloned().collect();
            if ids.is_empty() {
                return Err(format!("Select entities to move them to {to}"));
            }
            let cmd =
                layer_ops::move_entities(&doc.project, &ids, &to).map_err(|e| e.to_string())?;
            if cmd.is_noop() {
                return Err(format!("The selected entities are already on {to}"));
            }
            let label = cmd.label();
            self.apply_command(cmd)?;
            Ok(label)
        } else {
            Err(format!(
                "{to} is a {} layer: tile stacks move to a tiles layer, entities to an entities \
                 layer",
                def.kind.name()
            ))
        }
    }

    /// "Add reference image…": the file dialog, for `scope` (default: the
    /// active level, else the world).
    pub(super) fn add_reference_dialog(&mut self, scope: Option<NoteScope>) {
        let Some(doc) = &self.st.doc else {
            return;
        };
        if self.st.ref_picker.is_some() {
            return;
        }
        let Some(world) = doc.project.worlds.get(self.st.view.world_ix(doc)) else {
            return;
        };
        let scope = scope.unwrap_or_else(|| {
            match self.st.view.active_level.filter(|&i| i < doc.levels.len()) {
                Some(ix) => NoteScope::Level(doc.level(ix).slug.clone()),
                None => NoteScope::World(world.slug.clone()),
            }
        });
        self.st.ref_picker = Some(references::start_picker(&self.ctx, &doc.dir, scope));
    }

    /// The file dialog answered: add the image (one entry), select it and
    /// show it in the Inspector.
    pub(super) fn poll_reference_picker(&mut self) {
        let Some(doc) = &self.st.doc else {
            return;
        };
        let Some(res) = references::poll_picker(doc, &self.st.view, &mut self.st.ref_picker) else {
            return;
        };
        let r = res.and_then(|(cmd, id, copied)| self.added_reference(cmd, id, copied));
        self.report(r);
    }

    fn added_reference(
        &mut self,
        cmd: Command,
        id: Id,
        copied: Option<String>,
    ) -> Result<String, String> {
        self.apply_command(cmd)?;
        let rv = &mut self.st.view.references;
        rv.selected = Some(id);
        rv.active = true;
        rv.expanded = true;
        rv.ui.visible = true;
        show_pane(&mut self.dock, Pane::Inspector);
        Ok(copied.unwrap_or_else(|| "Added a reference image".into()))
    }

    /// `{ "ui": "reference", ... }` (see [`crate::script::ReferenceStep`]).
    pub(super) fn reference_step(
        &mut self,
        s: &crate::script::ReferenceStep,
    ) -> Result<(), String> {
        let doc = self.st.doc.as_ref().ok_or("no project open")?;
        let scope = match (&s.level, &s.world) {
            (Some(l), None) => {
                let ix = doc.level_ix(l).ok_or_else(|| format!("no level {l:?}"))?;
                NoteScope::Level(doc.level(ix).slug.clone())
            }
            (None, Some(w)) => NoteScope::World(
                doc.project
                    .world(w)
                    .ok_or_else(|| format!("no world {w:?}"))?
                    .slug
                    .clone(),
            ),
            (None, None) => match self.st.view.active_level.filter(|&i| i < doc.levels.len()) {
                Some(ix) => NoteScope::Level(doc.level(ix).slug.clone()),
                None => NoteScope::World(
                    doc.project
                        .worlds
                        .get(self.st.view.world_ix(doc))
                        .ok_or("the project has no world")?
                        .slug
                        .clone(),
                ),
            },
            (Some(_), Some(_)) => return Err("give level or world, not both".into()),
        };
        let px =
            s.px.unwrap_or_else(|| references::default_px(doc, &self.st.view, &scope));
        let (mut cmd, id, copied) =
            references::new_reference(doc, std::path::Path::new(&s.add), scope, px)?;
        if let Command::AddReference { reference, .. } = &mut cmd {
            if let Some(k) = s.scale {
                reference.scale = k;
            }
            if let Some(o) = s.opacity {
                reference.opacity = o;
            }
            reference.locked = s.locked;
        }
        self.added_reference(cmd, id, copied).map(drop)
    }

    /// Undo or redo until `to` entries are done (the History pane's click),
    /// one step at a time as Ctrl+Z / Ctrl+Shift+Z would.
    pub(super) fn history_jump(&mut self, to: usize) {
        let done = |app: &Self| {
            app.commands
                .editing_ref()
                .map(|e| e.history().undo_entries().len())
        };
        let Some(mut now) = done(self) else {
            return;
        };
        while now != to {
            let undo = now > to;
            let can = if undo {
                self.commands.can_undo()
            } else {
                self.commands.can_redo()
            };
            if !can {
                break;
            }
            self.run(if undo { Action::Undo } else { Action::Redo });
            let Some(next) = done(self) else {
                return;
            };
            if next == now {
                // A step that failed (its status says why): stop there.
                break;
            }
            now = next;
        }
    }

    /// `{ "ui": "history", "expand"?, "jump"? }`.
    pub(super) fn history_step(&mut self, s: &crate::script::HistoryStep) -> Result<(), String> {
        self.st.history.expand_all = s.expand;
        if let Some(to) = s.jump {
            let have = self
                .commands
                .editing_ref()
                .map(|e| e.history())
                .map_or(0, |h| h.undo_entries().len() + h.redo_entries().len());
            if to > have {
                return Err(format!(
                    "jump {to}: the history has {have} entr{}",
                    if have == 1 { "y" } else { "ies" }
                ));
            }
            self.history_jump(to);
        }
        show_pane(&mut self.dock, Pane::History);
        Ok(())
    }

    /// A PNG in the project changed on disk: a reference image using it
    /// reloads.
    pub(super) fn reference_image_changed(&mut self, path: &std::path::Path) {
        let Some(doc) = &self.st.doc else {
            return;
        };
        let root = doc.dir.canonicalize().unwrap_or_else(|_| doc.dir.clone());
        let Ok(rel) = path
            .strip_prefix(&root)
            .or_else(|_| path.strip_prefix(&doc.dir))
        else {
            return;
        };
        let rel = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        if crate::viewport::references::reload_image(&self.gpu, &rel) {
            tracing::info!("reference image {rel} changed, reloading it");
            self.ctx.request_repaint();
        }
    }
}
