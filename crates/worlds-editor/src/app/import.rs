//! Import from Worlds project…: the dialog over [`worlds_core::merge`].
//!
//! A checkbox tree of the source project's definitions, worlds and levels
//! (with a search box and the presets *Definitions only*, *Everything* and
//! *Pick*), the collision policy (a default and one per collision), where
//! imported levels go, and the plan's summary line. Items a checked item
//! needs show checked and greyed out. Every change re-plans; Apply copies
//! the plan's files and applies its batch as one undo entry, `Import from
//! <name>`.

use std::path::{Path, PathBuf};

use egui::{Color32, RichText};
use worlds_core::merge::{
    self, Action as ImportAction, Conflict, Kind, PlacementMode, Plan, PlanItem, Policy, Preset,
    Selection,
};
use worlds_core::{Project, Slug};

use super::actions::Action;
use super::{EditorApp, Modal};

/// The open dialog.
pub struct ImportDialog {
    /// The source project's directory.
    pub dir: PathBuf,
    pub source: Project,
    pub selection: Selection,
    pub policy: Policy,
    pub search: String,
    /// The new world's slug as typed (placement `new_world`).
    new_world: String,
    /// The plan for the current choices; `None` until re-planned.
    plan: Option<Result<Plan, String>>,
    error: Option<String>,
}

impl ImportDialog {
    /// The dialog on the project in `dir`, everything unchecked.
    pub fn open(dir: &Path) -> Result<Self, String> {
        let source = worlds_core::load(dir).map_err(|e| format!("can't import from it: {e}"))?;
        let new_world = merge::default_world(&source).to_string();
        Ok(ImportDialog {
            dir: dir.to_path_buf(),
            source,
            selection: Selection::default(),
            policy: Policy::default(),
            search: String::new(),
            new_world,
            plan: None,
            error: None,
        })
    }

    /// The current plan against `dst` (made when the choices changed).
    pub fn plan(&mut self, dst: &Project) -> &Result<Plan, String> {
        if self.plan.is_none() {
            self.sync_placement();
            self.plan = Some(
                merge::plan(&self.source, dst, &self.selection, &self.policy)
                    .map_err(|e| e.to_string()),
            );
        }
        self.plan.as_ref().expect("made above")
    }

    /// Forget the plan: the next frame makes it again.
    pub fn replan(&mut self) {
        self.plan = None;
        self.error = None;
    }

    fn sync_placement(&mut self) {
        if self.policy.placement.mode == PlacementMode::NewWorld {
            let w = self.new_world.trim();
            self.policy.placement.world = if w.is_empty() || merge::default_world(&self.source) == w
            {
                None
            } else {
                Slug::new(w).ok()
            };
        }
    }

    /// The selection with its preset spelled out as lists (preset `pick`).
    fn explicit(&self) -> Selection {
        let mut s = self.selection.clone();
        let p = s.preset;
        s.preset = Preset::Pick;
        if p == Preset::Pick {
            return s;
        }
        for (kind, slug) in merge::catalog(&self.source) {
            if kind.is_definition()
                && let Some(list) = s.list_mut(kind)
                && !list.contains(&slug)
            {
                list.push(slug);
            }
        }
        if p == Preset::Everything {
            for w in &self.source.worlds {
                if !s.worlds.contains(&w.slug) {
                    s.worlds.push(w.slug.clone());
                }
            }
        }
        s
    }

    /// Whether the user checked `kind` `slug` (a level: or its world).
    pub fn checked(&self, kind: Kind, slug: &Slug) -> bool {
        let mut s = self.explicit();
        if kind == Kind::Level
            && let Some(w) = self.source.world_of_level(slug.as_str())
            && s.worlds.contains(&w.slug)
        {
            return true;
        }
        s.list_mut(kind).is_some_and(|l| l.contains(slug))
    }

    /// Check or uncheck `kind` `slug`.
    pub fn set_checked(&mut self, kind: Kind, slug: &Slug, on: bool) {
        let mut s = self.explicit();
        if kind == Kind::Level
            && !on
            && let Some(w) = self.source.world_of_level(slug.as_str())
            && let Some(i) = s.worlds.iter().position(|x| *x == w.slug)
        {
            // Unchecking one level of a whole world: the rest stay.
            s.worlds.remove(i);
            for l in &w.levels {
                if l.slug != *slug && !s.levels.contains(&l.slug) {
                    s.levels.push(l.slug.clone());
                }
            }
        }
        if let Some(list) = s.list_mut(kind) {
            list.retain(|x| x != slug);
            if on {
                list.push(slug.clone());
            }
        }
        self.selection = s;
        self.replan();
    }

    /// Check or uncheck a whole source world.
    pub fn set_world(&mut self, world: &Slug, on: bool) {
        let mut s = self.explicit();
        s.worlds.retain(|x| x != world);
        if let Some(w) = self.source.world(world.as_str()) {
            s.levels.retain(|l| w.level(l.as_str()).is_none());
        }
        if on {
            s.worlds.push(world.clone());
        }
        self.selection = s;
        self.replan();
    }

    pub fn set_preset(&mut self, p: Preset) {
        self.selection = match p {
            Preset::Pick => self.explicit(),
            p => Selection::preset(p),
        };
        self.replan();
    }

    /// The policy for the collision `key` (its own, else the default).
    fn conflict(&self, key: &str) -> Conflict {
        self.policy
            .conflicts
            .get(key)
            .copied()
            .unwrap_or(self.policy.on_conflict)
    }
}

fn conflict_name(c: Conflict) -> &'static str {
    match c {
        Conflict::Skip => "Skip (keep ours)",
        Conflict::Overwrite => "Overwrite (take theirs)",
        Conflict::Rename => "Rename (take as _2)",
    }
}

const CONFLICTS: [Conflict; 3] = [Conflict::Skip, Conflict::Overwrite, Conflict::Rename];

/// The badge an item's action shows.
fn badge(i: &PlanItem) -> (String, Color32) {
    match i.action {
        ImportAction::Add => ("add".into(), Color32::from_rgb(120, 200, 120)),
        ImportAction::Same => ("same".into(), Color32::from_gray(140)),
        ImportAction::Skip => ("skip: ours kept".into(), Color32::from_rgb(220, 180, 90)),
        ImportAction::Overwrite => ("overwrite".into(), Color32::from_rgb(230, 130, 100)),
        ImportAction::Rename => (
            format!(
                "rename -> {}",
                i.to.as_ref().map(Slug::as_str).unwrap_or("?")
            ),
            Color32::from_rgb(120, 170, 240),
        ),
        ImportAction::Refused => ("refused".into(), Color32::from_rgb(230, 100, 100)),
    }
}

impl EditorApp {
    /// File > Import from Worlds project…: pick the source's directory.
    pub(super) fn pick_import_source(&mut self) {
        if self.import_pick.is_some() || self.opts.headless || self.st.doc.is_none() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let start = self.st.doc.as_ref().map(|d| d.dir.clone());
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let mut d = rfd::FileDialog::new().set_title("Import from a .worlds project directory");
            if let Some(s) = start.as_ref().and_then(|s| s.parent()) {
                d = d.set_directory(s);
            }
            let _ = tx.send(d.pick_folder());
            ctx.request_repaint();
        });
        self.import_pick = Some(rx);
    }

    /// The picked directory arrived.
    pub(super) fn poll_import_pick(&mut self) {
        if let Some(rx) = &self.import_pick
            && let Ok(res) = rx.try_recv()
        {
            self.import_pick = None;
            if let Some(dir) = res
                && let Err(e) = self.open_import(&dir)
            {
                self.st.toast(e, true);
            }
        }
    }

    /// Open the import dialog on the project in `dir`.
    pub fn open_import(&mut self, dir: &Path) -> Result<(), String> {
        let Some(doc) = &self.st.doc else {
            return Err("no project open".into());
        };
        if std::fs::canonicalize(dir).ok() == std::fs::canonicalize(&doc.dir).ok() {
            return Err("that is this project; pick another one".into());
        }
        let dir = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
        self.import = Some(ImportDialog::open(&dir)?);
        Ok(())
    }

    /// The import dialog, if open.
    pub fn import_dialog(&mut self) -> Option<&mut ImportDialog> {
        self.import.as_mut()
    }

    pub(super) fn import_window(&mut self, ctx: &egui::Context) {
        let Some(doc) = &self.st.doc else {
            self.import = None;
            return;
        };
        let Some(d) = &mut self.import else {
            return;
        };
        let (_, escape) = self.modal_keys.for_modal(Modal::Import);
        let mut apply = false;
        let mut cancel = escape;
        let dst_worlds: Vec<Slug> = doc.project.worlds.iter().map(|w| w.slug.clone()).collect();
        d.plan(&doc.project);
        let title = format!("Import from {}", d.source.name);
        egui::Window::new(title)
            .id(egui::Id::new("import dialog"))
            // Drawn whole from its first frame (a four-frame screenshot).
            .fade_in(false)
            .collapsible(false)
            .resizable(true)
            .default_width(560.0)
            .anchor(egui::Align2::CENTER_TOP, [0.0, 48.0])
            .show(ctx, |ui| {
                ui.weak(d.dir.display().to_string());
                ui.horizontal(|ui| {
                    for (p, name) in [
                        (Preset::DefsOnly, "Definitions only"),
                        (Preset::Everything, "Everything"),
                        (Preset::Pick, "Pick"),
                    ] {
                        if ui
                            .add(egui::Button::selectable(d.selection.preset == p, name))
                            .clicked()
                        {
                            d.set_preset(p);
                        }
                    }
                    ui.separator();
                    ui.add(
                        egui::TextEdit::singleline(&mut d.search)
                            .hint_text("Search…")
                            .desired_width(160.0),
                    );
                });
                ui.separator();
                let plan = match d.plan.as_ref() {
                    Some(Ok(p)) => Some(p.clone()),
                    _ => None,
                };
                let q = d.search.trim().to_lowercase();
                let catalog = merge::catalog(&d.source);
                let mut toggles: Vec<(Kind, Slug, bool)> = Vec::new();
                let mut world_toggles: Vec<(Slug, bool)> = Vec::new();
                let mut conflicts: Vec<(String, Conflict)> = Vec::new();
                egui::ScrollArea::vertical()
                    .max_height(320.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        let mut row = |ui: &mut egui::Ui, kind: Kind, slug: &Slug| {
                            let mine = d.checked(kind, slug);
                            let item = plan.as_ref().and_then(|p| p.item(kind, slug.as_str()));
                            let pulled = !mine && item.is_some();
                            ui.horizontal(|ui| {
                                let mut on = mine || pulled;
                                let r = ui.add_enabled(!pulled, egui::Checkbox::new(&mut on, ""));
                                if r.changed() {
                                    toggles.push((kind, slug.clone(), on));
                                }
                                let text = RichText::new(slug.as_str());
                                if pulled {
                                    ui.weak(text);
                                } else {
                                    ui.label(text);
                                }
                                if let Some(i) = item {
                                    let (b, c) = badge(i);
                                    let r = ui.label(RichText::new(b).small().color(c));
                                    let mut tip = Vec::new();
                                    if !i.required_by.is_empty() {
                                        tip.push(format!("Needed by {}", i.required_by.join(", ")));
                                    }
                                    if let Some(w) = &i.world {
                                        tip.push(format!("Goes into world {w}"));
                                    }
                                    if let Some(why) = &i.reason {
                                        tip.push(why.clone());
                                    }
                                    if !tip.is_empty() {
                                        r.on_hover_text(tip.join("\n"));
                                    }
                                    if i.collided() && kind != Kind::Setting {
                                        let key = i.key();
                                        let cur = d.conflict(&key);
                                        egui::ComboBox::from_id_salt(("import conflict", &key))
                                            .selected_text(conflict_name(cur))
                                            .width(150.0)
                                            .show_ui(ui, |ui| {
                                                for c in CONFLICTS {
                                                    if ui
                                                        .selectable_label(
                                                            c == cur,
                                                            conflict_name(c),
                                                        )
                                                        .clicked()
                                                    {
                                                        conflicts.push((key.clone(), c));
                                                    }
                                                }
                                            });
                                    }
                                }
                            });
                        };
                        for kind in Kind::ALL {
                            if !kind.is_definition() {
                                continue;
                            }
                            let items: Vec<&Slug> = catalog
                                .iter()
                                .filter(|(k, s)| *k == kind && s.as_str().contains(&q))
                                .map(|(_, s)| s)
                                .collect();
                            if items.is_empty() {
                                continue;
                            }
                            egui::CollapsingHeader::new(format!(
                                "{} ({})",
                                kind.plural(),
                                items.len()
                            ))
                            .id_salt(("import kind", kind.name()))
                            .default_open(true)
                            .show(ui, |ui| {
                                for s in items {
                                    row(ui, kind, s);
                                }
                            });
                        }
                        for w in &d.source.worlds {
                            let levels: Vec<&Slug> = w
                                .levels
                                .iter()
                                .map(|l| &l.slug)
                                .filter(|s| s.as_str().contains(&q) || w.slug.as_str().contains(&q))
                                .collect();
                            if levels.is_empty() {
                                continue;
                            }
                            egui::CollapsingHeader::new(format!(
                                "World {} ({} levels)",
                                w.slug,
                                w.levels.len()
                            ))
                            .id_salt(("import world", w.slug.as_str()))
                            .default_open(true)
                            .show(ui, |ui| {
                                let mut whole = d.explicit().worlds.contains(&w.slug);
                                if ui
                                    .checkbox(
                                        &mut whole,
                                        "The whole world (its guides and notes too)",
                                    )
                                    .changed()
                                {
                                    world_toggles.push((w.slug.clone(), whole));
                                }
                                for s in levels {
                                    row(ui, Kind::Level, s);
                                }
                            });
                        }
                    });
                ui.separator();
                egui::Grid::new("import options")
                    .num_columns(2)
                    .show(ui, |ui| {
                        ui.label("On a collision");
                        let cur = d.policy.on_conflict;
                        egui::ComboBox::from_id_salt("import on_conflict")
                            .selected_text(conflict_name(cur))
                            .width(180.0)
                            .show_ui(ui, |ui| {
                                for c in CONFLICTS {
                                    if ui.selectable_label(c == cur, conflict_name(c)).clicked() {
                                        d.policy.on_conflict = c;
                                        d.replan();
                                    }
                                }
                            });
                        ui.end_row();
                        ui.label("Levels go");
                        ui.horizontal(|ui| {
                            let pl = &mut d.policy.placement;
                            let mut changed = false;
                            changed |= ui
                                .radio_value(
                                    &mut pl.mode,
                                    PlacementMode::NewWorld,
                                    "into a new world",
                                )
                                .changed();
                            if pl.mode == PlacementMode::NewWorld {
                                changed |= ui
                                    .add(
                                        egui::TextEdit::singleline(&mut d.new_world)
                                            .desired_width(90.0),
                                    )
                                    .changed();
                            }
                            changed |= ui
                                .radio_value(&mut pl.mode, PlacementMode::World, "into world")
                                .changed();
                            if pl.mode == PlacementMode::World {
                                if pl.world.as_ref().is_none_or(|w| !dst_worlds.contains(w)) {
                                    pl.world = dst_worlds.first().cloned();
                                    changed = true;
                                }
                                let cur = pl
                                    .world
                                    .clone()
                                    .unwrap_or_else(|| worlds_core::slug("main"));
                                egui::ComboBox::from_id_salt("import world")
                                    .selected_text(cur.as_str())
                                    .show_ui(ui, |ui| {
                                        for w in &dst_worlds {
                                            if ui.selectable_label(*w == cur, w.as_str()).clicked()
                                            {
                                                pl.world = Some(w.clone());
                                                changed = true;
                                            }
                                        }
                                    });
                            }
                            changed |= ui
                                .radio_value(&mut pl.mode, PlacementMode::Keep, "keep their worlds")
                                .changed();
                            if changed {
                                if pl.mode == PlacementMode::Keep {
                                    pl.world = None;
                                }
                                d.plan = None;
                            }
                        });
                        ui.end_row();
                        ui.label("Offset (px)");
                        ui.horizontal(|ui| {
                            let o = &mut d.policy.placement.offset;
                            let a = ui.add(egui::DragValue::new(&mut o[0]).prefix("x ").speed(16));
                            let b = ui.add(egui::DragValue::new(&mut o[1]).prefix("y ").speed(16));
                            if a.changed() || b.changed() {
                                d.plan = None;
                            }
                        });
                        ui.end_row();
                    });
                ui.separator();
                match &plan {
                    Some(p) => {
                        ui.strong(&p.summary);
                        let files = p.assets.iter().filter(|a| a.action.imports()).count();
                        let extra = [
                            (p.ids.len(), "id re-rolled", "ids re-rolled"),
                            (p.dropped.len(), "reference dropped", "references dropped"),
                            (files, "file copied", "files copied"),
                            (p.problems.len(), "problem", "problems"),
                        ];
                        if extra.iter().any(|(n, ..)| *n > 0) {
                            egui::CollapsingHeader::new(
                                extra
                                    .iter()
                                    .filter(|(n, ..)| *n > 0)
                                    .map(|(n, one, many)| {
                                        format!("{n} {}", if *n == 1 { one } else { many })
                                    })
                                    .collect::<Vec<_>>()
                                    .join(", "),
                            )
                            .id_salt("import details")
                            .show(ui, |ui| {
                                for c in &p.ids {
                                    ui.label(format!("{} {} -> {}", c.kind, c.from, c.to));
                                }
                                for s in &p.dropped {
                                    ui.label(format!("dropped: {s}"));
                                }
                                for a in &p.assets {
                                    ui.label(format!(
                                        "{}: {} -> {}",
                                        a.action.name(),
                                        a.from,
                                        a.to
                                    ));
                                }
                                for s in &p.problems {
                                    ui.label(s);
                                }
                            });
                        }
                    }
                    None => {
                        if let Some(Err(e)) = &d.plan {
                            ui.colored_label(Color32::from_rgb(230, 120, 120), e);
                        }
                    }
                }
                if let Some(e) = &d.error {
                    ui.colored_label(Color32::from_rgb(230, 120, 120), e);
                }
                ui.horizontal(|ui| {
                    let can = plan.as_ref().is_some_and(|p| !p.is_empty());
                    apply |= ui
                        .add_enabled(can, egui::Button::new("Apply"))
                        .on_disabled_hover_text("Nothing to import")
                        .clicked();
                    cancel |= ui.button("Cancel").clicked();
                });
                for (k, s, on) in toggles {
                    d.set_checked(k, &s, on);
                }
                for (w, on) in world_toggles {
                    d.set_world(&w, on);
                }
                for (key, c) in conflicts {
                    d.policy.conflicts.insert(key, c);
                    d.replan();
                }
            });
        if cancel {
            self.import = None;
            return;
        }
        if apply
            && let Err(e) = self.apply_import()
            && let Some(d) = &mut self.import
        {
            d.error = Some(e);
        }
    }

    /// Apply the open dialog's plan (made again against the project as it
    /// is now): copy its files, then its batch, one undo entry.
    pub fn apply_import(&mut self) -> Result<String, String> {
        let (Some(d), Some(doc)) = (&mut self.import, &self.st.doc) else {
            return Err("no import open".into());
        };
        d.replan();
        let plan = d.plan(&doc.project).clone()?;
        if plan.is_empty() {
            return Err(format!("Nothing to import ({})", plan.summary));
        }
        let copied = merge::copy_assets(&plan, &d.dir, &doc.dir)
            .map_err(|e| format!("copying files into the project: {e}"))?;
        let new_world = plan
            .items
            .iter()
            .find(|i| i.kind == Kind::World && i.action == ImportAction::Add)
            .map(|i| i.slug.clone());
        self.apply_command(plan.command())?;
        self.import = None;
        let files = match copied.len() {
            0 => String::new(),
            1 => " (1 file copied)".into(),
            n => format!(" ({n} files copied)"),
        };
        let msg = format!("{}: {}{files}", plan.label, plan.summary);
        self.st.toast(msg.clone(), false);
        if let Some(w) = new_world {
            self.st.actions.push(Action::SwitchWorld(w));
        }
        Ok(msg)
    }

    /// `"ui": "import_project"`: see [`crate::script::ImportStep`].
    pub(super) fn import_step(&mut self, s: &crate::script::ImportStep) -> Result<(), String> {
        let dir = std::path::absolute(&s.source).map_err(|e| e.to_string())?;
        self.open_import(&dir)?;
        let d = self.import.as_mut().expect("opened above");
        if let Some(sel) = &s.selection {
            d.selection = sel.clone();
        }
        if let Some(p) = &s.policy {
            d.policy = p.clone();
            if let (PlacementMode::NewWorld, Some(w)) = (p.placement.mode, &p.placement.world) {
                d.new_world = w.to_string();
            }
        }
        if let Some(q) = &s.search {
            d.search = q.clone();
        }
        d.replan();
        let doc = self.st.doc.as_ref().ok_or("no project open")?;
        let d = self.import.as_mut().expect("opened above");
        d.plan(&doc.project).clone()?;
        if s.apply {
            self.apply_import()?;
            for a in std::mem::take(&mut self.st.actions) {
                self.run(a);
            }
        }
        Ok(())
    }
}
