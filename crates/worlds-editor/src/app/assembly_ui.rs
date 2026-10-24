//! The editor's assemblies and transforms: X, Y and R on the paste ghost
//! and the tile stamp, the selection transforms, arranging levels, Save
//! selection as assembly and placing from the Library, and their
//! `--script` steps. The work is in [`crate::arrange`],
//! [`crate::clipboard`] and worlds-core.

use worlds_core::assembly::{Assembly, AssemblyInfo};
use worlds_core::transform::{ArrangeOp, TransformOp};
use worlds_core::{Command, Flip, Slug};

use super::actions::Action;
use super::{EditorApp, Pane, show_pane};
use crate::tools::Brush;

/// What X, Y or R would turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Turnable {
    /// The armed paste ghost (or assembly).
    Ghost,
    /// The tile stamp of the active tiles layer, in a Paint mode.
    Stamp,
}

impl EditorApp {
    /// What X, Y and R act on right now, if anything (the keys are left to
    /// the tools otherwise: R is the Rectangle mode).
    pub fn turnable(&self) -> Option<Turnable> {
        if self.st.view.paste.is_some() {
            return Some(Turnable::Ghost);
        }
        let doc = self.st.doc.as_ref()?;
        let def = self
            .st
            .view
            .active_layer
            .and_then(|l| doc.project.layers.get(l))?;
        (def.is_tiles()
            && self.st.view.tool.is_paint_mode()
            && matches!(
                self.st.view.palette.brush(def.slug.as_str()),
                Some(Brush::Tiles { .. } | Brush::Random { .. })
            ))
        .then_some(Turnable::Stamp)
    }

    /// X, Y, R: turn the paste ghost, else the tile stamp, by `op`.
    pub(super) fn turn_brush(&mut self, op: Flip) {
        let Some(doc) = self.st.doc.as_ref() else {
            return;
        };
        match self.turnable() {
            Some(Turnable::Ghost) => {
                let Some(p) = self.st.view.paste.as_mut() else {
                    return;
                };
                match p.turn(&doc.project, op) {
                    Ok(()) => {
                        let what = p.transform.describe();
                        let it = match &p.assembly {
                            Some(s) => format!("Assembly {s}"),
                            None => "Paste".into(),
                        };
                        self.st.view.set_status(
                            format!("{it} {what}: click to place, X/Y/R turn it again"),
                            false,
                        );
                    }
                    Err(e) => self.st.view.set_status(e, true),
                }
            }
            Some(Turnable::Stamp) => {
                let def = &doc.project.layers[self.st.view.active_layer.unwrap_or_default()];
                if let Some(Brush::Tiles { tileset, .. } | Brush::Random { tileset, .. }) =
                    self.st.view.palette.brush(def.slug.as_str())
                    && let Some(ts) = doc.project.tileset(tileset.as_str())
                {
                    let next = worlds_core::transform::then(self.st.view.brush_flip, op);
                    if Flip::bits_of(next) & 4 != 0 && ts.tile_size[0] != ts.tile_size[1] {
                        self.st
                            .view
                            .set_status(crate::clipboard::not_square(ts), true);
                        return;
                    }
                    self.st.view.brush_flip = next;
                    self.st.view.set_status(
                        format!("Tile stamp {}", crate::clipboard::describe_flip(next)),
                        false,
                    );
                }
            }
            None => self.st.view.set_status(
                "X, Y and R turn a paste (Ctrl+V, or an assembly from the Library) or a tile \
                 stamp (a tiles layer in a Paint mode); to turn the selection, use Mirror or \
                 Rotate in its context menu",
                true,
            ),
        }
    }

    /// Mirror X, Mirror Y, Rotate 90 on the selection: one entry; the
    /// status bar says what was left alone and which fields look
    /// directional.
    pub(super) fn transform_selection(&mut self, op: TransformOp) {
        let Some(doc) = self.st.doc.as_ref() else {
            return;
        };
        let t = match crate::arrange::plan_transform(doc, &self.st.view, op) {
            Ok(t) => t,
            Err(e) => {
                self.st.view.set_status(e, true);
                return;
            }
        };
        match self.apply_command(t.command) {
            Ok(()) => {
                self.st.view.selection.set(t.selection);
                if self.st.view.grid_marquee.is_some() {
                    self.st.view.grid_marquee = t.marquee;
                }
                self.st.view.set_status(t.message, false);
            }
            Err(e) => self.st.view.set_status(format!("Can't turn: {e}"), true),
        }
    }

    /// Align, distribute or butt together the selected levels: one entry.
    pub(super) fn arrange_levels(&mut self, op: ArrangeOp) {
        let Some(doc) = self.st.doc.as_ref() else {
            return;
        };
        let cmd = match crate::arrange::plan_arrange(doc, &self.st.view, op) {
            Ok(c) => c,
            Err(e) => {
                self.st.view.set_status(e, true);
                return;
            }
        };
        if cmd.is_noop() {
            self.st
                .view
                .set_status(format!("{}: already so", op.title()), false);
            return;
        }
        let label = cmd.label();
        match self.apply_command(cmd) {
            Ok(()) => self.st.view.set_status(label, false),
            Err(e) => self.st.view.set_status(format!("Can't arrange: {e}"), true),
        }
    }

    /// Ctrl+Shift+S: the dialog, when there is something to save, with a
    /// slug, name, category and tags suggested from the selection.
    pub(super) fn open_save_assembly(&mut self) {
        let Some(doc) = self.st.doc.as_ref() else {
            return;
        };
        match crate::clipboard::copy_selection(doc, &self.st.view) {
            Ok(copied) => {
                self.st.library.dialog = Some(crate::library::SaveDialog::suggest(
                    &doc.project,
                    &copied.fragment,
                ));
            }
            Err(e) => self
                .st
                .view
                .set_status(e.replacen("Nothing to copy", "Nothing to save", 1), true),
        }
    }

    /// The dialog's Save: what Copy would take, as assembly `info`.
    pub(super) fn save_assembly(&mut self, info: AssemblyInfo, replace: bool) {
        let Some(doc) = self.st.doc.as_ref() else {
            return;
        };
        if !replace && doc.project.assembly(info.slug.as_str()).is_some() {
            self.st
                .view
                .set_status(format!("An assembly {} exists already", info.slug), true);
            return;
        }
        let copied = match crate::clipboard::copy_selection(doc, &self.st.view) {
            Ok(c) => c,
            Err(e) => {
                self.st
                    .view
                    .set_status(e.replacen("Nothing to copy", "Nothing to save", 1), true);
                return;
            }
        };
        let n = copied.fragment.item_count();
        let slug = info.slug.clone();
        let assembly = Assembly::new(info, copied.fragment);
        match self.apply_command(Command::SetAssembly { assembly }) {
            Ok(()) => {
                self.st.view.library.open = true;
                show_pane(&mut self.dock, Pane::Palette);
                self.st.view.set_status(
                    format!(
                        "Saved assembly {slug} ({n} item{}); it is in the Library",
                        if n == 1 { "" } else { "s" }
                    ),
                    false,
                );
            }
            Err(e) => self.st.view.set_status(format!("Can't save: {e}"), true),
        }
    }

    /// A Library click: the assembly follows the pointer as a paste ghost.
    pub(super) fn arm_assembly(&mut self, slug: &Slug) {
        let Some(doc) = self.st.doc.as_ref() else {
            return;
        };
        let Some(a) = doc.project.assembly(slug.as_str()) else {
            self.st.view.set_status(format!("No assembly {slug}"), true);
            return;
        };
        let frag = a.fragment();
        self.st
            .viewport
            .cancel_gesture(self.st.view.tool, &mut self.st.tool_events);
        self.st.view.paste = Some(crate::clipboard::PasteGhost::assembly(slug.clone(), frag));
        self.st.view.set_status(
            format!(
                "Place assembly {slug}: click on the map (Shift+click places more); X, Y and R \
                 turn it, Escape cancels"
            ),
            false,
        );
    }

    pub(super) fn toggle_library(&mut self) {
        self.st.view.library.open = !self.st.view.library.open;
        show_pane(&mut self.dock, Pane::Palette);
    }

    // ---- --script steps ----

    /// `"ui": "turn"`.
    pub(super) fn turn_step(&mut self, t: &crate::script::TurnStep) -> Result<(), String> {
        for k in t.keys.chars() {
            let op = match k.to_ascii_lowercase() {
                'x' => Flip::X,
                'y' => Flip::Y,
                'r' => Flip::XT,
                other => return Err(format!("key {other:?}: expected x, y or r")),
            };
            if self.turnable().is_none() {
                return Err(format!(
                    "{}: nothing to turn (arm a paste or an assembly, or pick a tile stamp on a tiles layer in a Paint mode)",
                    k.to_ascii_uppercase()
                ));
            }
            self.status_step(Action::Turn(op))?;
        }
        Ok(())
    }

    /// `"ui": "transform"`.
    pub(super) fn transform_step(
        &mut self,
        t: &crate::script::TransformStep,
    ) -> Result<(), String> {
        self.status_step(Action::TransformSelection(t.op))
    }

    /// `"ui": "arrange"`.
    pub(super) fn arrange_step(&mut self, a: &crate::script::ArrangeStep) -> Result<(), String> {
        if let Some(levels) = &a.levels {
            let doc = self.st.doc.as_ref().ok_or("no project open")?;
            let mut items = Vec::new();
            for s in levels {
                let ix = doc.level_ix(s).ok_or_else(|| format!("no level {s:?}"))?;
                items.push(crate::view::SelItem::Level(doc.ids[ix].clone()));
            }
            self.st.view.selection.set(items);
        }
        self.status_step(Action::ArrangeLevels(a.op))
    }

    /// `"ui": "save_assembly"`.
    pub(super) fn save_assembly_step(
        &mut self,
        s: &crate::script::SaveAssemblyStep,
    ) -> Result<(), String> {
        self.status_step(Action::OpenSaveAssembly)?;
        let doc = self.st.doc.as_ref().ok_or("no project open")?;
        let d = self
            .st
            .library
            .dialog
            .as_mut()
            .ok_or("the dialog didn't open")?;
        d.slug = s.slug.clone();
        d.name = s.name.clone();
        d.category = s.category.clone().unwrap_or_default();
        d.tags = s.tags.join(", ");
        d.doc = s.doc.clone().unwrap_or_default();
        d.replace = s.replace;
        if s.open {
            return Ok(());
        }
        let (info, replace) = d.info(&doc.project)?;
        self.st.library.dialog = None;
        self.status_step(Action::SaveAssembly { info, replace })
    }

    /// `"ui": "library"`.
    pub(super) fn library_step(&mut self, l: &crate::script::LibraryStep) -> Result<(), String> {
        let doc = self.st.doc.as_ref().ok_or("no project open")?;
        self.st.view.library.open = true;
        show_pane(&mut self.dock, Pane::Palette);
        if let Some(q) = &l.search {
            self.st.library.query = q.clone();
        }
        let known = |s: &String| -> Result<Slug, String> {
            doc.project
                .assembly(s)
                .map(|a| a.slug.clone())
                .ok_or_else(|| format!("no assembly {s:?}"))
        };
        if let Some(f) = &l.favorites {
            self.st.view.library.favorites = f.iter().map(known).collect::<Result<_, _>>()?;
        }
        if let Some(c) = &l.collapsed {
            self.st.view.library.collapsed = c.iter().cloned().collect();
        }
        if let Some(s) = &l.arm {
            let slug = known(s)?;
            self.status_step(Action::ArmAssembly(slug))?;
        }
        if let Some(level) = &l.level {
            let doc = self.st.doc.as_ref().ok_or("no project open")?;
            let ix = doc
                .level_ix(level)
                .ok_or_else(|| format!("no level {level:?}"))?;
            let origin = l.origin.ok_or("level goes with origin")?;
            let g = self.st.view.paste.as_mut().ok_or("level needs arm")?;
            g.at = Some((ix, origin));
            g.pinned = true;
        }
        if l.place {
            let (ix, origin) = self
                .st
                .view
                .paste
                .as_ref()
                .and_then(|g| g.at)
                .ok_or("place needs arm, level and origin")?;
            return self.status_step(Action::PasteAt {
                level: ix,
                origin,
                keep: false,
            });
        }
        Ok(())
    }
}
