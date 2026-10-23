//! Crash recovery in the shell: the session lock's "open in another
//! Worlds" message, and the recovery prompt (Recover / Discard / Later)
//! for a previous session's autosave, with its headless counterpart
//! (`--recover keep|apply|discard`). See `worlds_core::autosave`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use egui::{Color32, RichText};
use serde_json::{Value, json};
use worlds_core::autosave::{self, FileState, Found, Inspect};

use super::EditorApp;
use crate::autosave::{Blocked, Due};

/// How long closing or switching projects waits for an autosave write.
pub(super) const FLUSH_TIMEOUT: Duration = Duration::from_secs(5);

/// Whether this session may autosave and recover: only with the session
/// lock really held (a filesystem that can't lock gives a lock that holds
/// nothing, and two editors would overwrite each other's autosave).
pub(super) fn session_locked(lock: Option<&autosave::EditorLock>) -> bool {
    lock.is_some_and(autosave::EditorLock::is_locked)
}

/// What to do with a previous session's autosave without asking
/// (`--recover`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoverMode {
    /// Open from disk, leave the autosave for later (headless default).
    Keep,
    /// Recover it (conflicting files only with `--recover-conflicts`).
    Apply,
    /// Move it to `.worlds/autosave.discarded/`.
    Discard,
    /// Show the recovery prompt, as the window does by default (headless:
    /// it is in the screenshot, and tests answer it).
    Ask,
}

impl std::str::FromStr for RecoverMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "keep" => Ok(RecoverMode::Keep),
            "apply" => Ok(RecoverMode::Apply),
            "discard" => Ok(RecoverMode::Discard),
            "ask" => Ok(RecoverMode::Ask),
            _ => Err(format!("{s:?}: expected keep, apply, discard or ask")),
        }
    }
}

/// The prompt's buttons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    Recover,
    Discard,
    Later,
}

/// The recovery prompt's state.
pub struct RecoveryPrompt {
    pub found: Found,
    /// "Recover conflicting files too".
    pub conflicts: bool,
    /// Each row's diff, made when it is first expanded.
    diffs: Vec<Option<String>>,
    pub error: Option<String>,
}

impl RecoveryPrompt {
    fn new(found: Found) -> Self {
        let n = found.entries.len();
        RecoveryPrompt {
            found,
            conflicts: false,
            diffs: vec![None; n],
            error: None,
        }
    }
}

impl EditorApp {
    /// After a project opened with the session lock held: look for a
    /// previous session's autosave and act on it (`--recover`), or ask.
    pub(super) fn check_recovery(&mut self, dir: &Path) {
        let mode = self.opts.recover;
        match autosave::inspect(dir) {
            Inspect::None => self.write_recover_report(|| json!({ "found": false })),
            Inspect::DiskUnreadable(msg) => {
                // The autosave is fine; the project can't be compared with
                // it now. Keep it (autosave stays off) until it can.
                let text = format!(
                    "The last session's autosave can't be compared with the project ({msg}); it is kept, and this session isn't autosaved until it is recovered or discarded"
                );
                eprintln!("autosave: {text}");
                self.st.toast(text, true);
                self.write_recover_report(|| json!({ "found": true, "unreadable": msg }));
                self.autosaver.blocked = Some(Blocked::Later);
                if mode == Some(RecoverMode::Apply) {
                    self.recovery_failed = Some(msg);
                }
            }
            Inspect::Broken(msg) => {
                let moved = autosave::discard(dir);
                let text = format!(
                    "The last session's autosave can't be read and was set aside ({}): {msg}",
                    match &moved {
                        Ok(d) => d.display().to_string(),
                        Err(e) => format!("moving it failed: {e}"),
                    }
                );
                eprintln!("autosave: {text}");
                self.st.toast(text, true);
                self.write_recover_report(|| json!({ "found": true, "broken": msg }));
                if mode == Some(RecoverMode::Apply) {
                    self.recovery_failed = Some(msg);
                }
            }
            Inspect::Found(f) if f.entries.is_empty() => {
                // Everything in it is on disk already (killed between a
                // save and the autosave's removal).
                tracing::info!("the last session's autosave matches the disk; removed");
                let _ = autosave::clear(dir);
                self.write_recover_report(|| f.report(&[], true));
            }
            Inspect::Found(f) => {
                let headless = self.opts.headless;
                match mode {
                    Some(RecoverMode::Apply) => {
                        let conflicts = self.opts.recover_conflicts;
                        if let Err(e) = self.recover(f, conflicts) {
                            eprintln!("autosave: recovery failed: {e}");
                            self.recovery_failed = Some(e);
                        }
                    }
                    Some(RecoverMode::Discard) => {
                        self.write_recover_report(|| f.report(&[], true));
                        self.discard_autosave(dir);
                    }
                    Some(RecoverMode::Keep) | None if headless => {
                        eprintln!("autosave: {}; not applied", f.summary());
                        self.write_recover_report(|| f.report(&[], true));
                        self.autosaver.blocked = Some(Blocked::Later);
                    }
                    Some(RecoverMode::Keep) => {
                        self.write_recover_report(|| f.report(&[], true));
                        self.autosaver.blocked = Some(Blocked::Later);
                    }
                    Some(RecoverMode::Ask) | None => {
                        tracing::info!("autosave found: {}", f.summary());
                        self.write_recover_report(|| f.report(&[], true));
                        self.autosaver.blocked = Some(Blocked::Undecided);
                        self.recovery = Some(RecoveryPrompt::new(f));
                    }
                }
            }
        }
    }

    /// Write `--recover-report`, if asked for; `v` (which may hold every
    /// file's diff) is only made then.
    fn write_recover_report(&mut self, v: impl FnOnce() -> Value) {
        let Some(path) = self.opts.recover_report.clone() else {
            return;
        };
        self.recover_reports += 1;
        let text = serde_json::to_string_pretty(&v()).expect("serializes") + "\n";
        if let Err(e) = std::fs::write(&path, text) {
            eprintln!("--recover-report {}: {e}", path.display());
        }
    }

    /// How many `--recover-report` bodies this session made (tests: none
    /// without the flag).
    pub fn recover_reports_made(&self) -> usize {
        self.recover_reports
    }

    /// Whether the recovery prompt is up.
    pub fn recovery_pending(&self) -> bool {
        self.recovery.is_some()
    }

    /// The recovery prompt, if up (tests).
    pub fn recovery_prompt_state(&mut self) -> Option<&mut RecoveryPrompt> {
        self.recovery.as_mut()
    }

    /// What `project.describe` reports as `recovered` when this session
    /// started from a recovery.
    pub fn recovered(&self) -> Option<&Value> {
        self.recovered.as_ref()
    }

    /// Answer the recovery prompt (its buttons; tests).
    pub fn recovery_choose(&mut self, c: Choice) -> Result<(), String> {
        let Some(p) = self.recovery.take() else {
            return Err("no recovery prompt".into());
        };
        let Some(dir) = self.st.doc.as_ref().map(|d| d.dir.clone()) else {
            return Err("no project open".into());
        };
        match c {
            Choice::Recover if self.has_unsaved() => {
                // Recovering replaces the open document: edits made since
                // it opened (yours or an agent's) would be lost silently.
                let e = "You edited the project since it opened; save or undo those edits first (recovering replaces the open document)".to_string();
                self.recovery = Some(RecoveryPrompt {
                    error: Some(e.clone()),
                    ..p
                });
                return Err(e);
            }
            Choice::Recover => {
                // The prompt shows what the disk held when it came up; the
                // disk may have changed since (git, an agent). Look again,
                // and if anything differs show the new state instead of
                // applying decisions made on the old one.
                let found = match autosave::inspect(&dir) {
                    Inspect::Found(f) if f.entries.is_empty() => {
                        let _ = autosave::clear(&dir);
                        self.autosaver.blocked = None;
                        self.st
                            .view
                            .set_status("The last session's edits are all on disk now", false);
                        return Ok(());
                    }
                    Inspect::Found(f) => f,
                    Inspect::None => {
                        self.autosaver.blocked = None;
                        return Err("the last session's autosave is gone".into());
                    }
                    Inspect::Broken(_) | Inspect::DiskUnreadable(_) => {
                        self.reask_recovery();
                        return Err("the last session's autosave changed; see the message".into());
                    }
                };
                if found.manifest != p.found.manifest || found.entries != p.found.entries {
                    let e = "Files changed on disk while this was open; check the list again before recovering".to_string();
                    let mut fresh = RecoveryPrompt::new(found);
                    fresh.conflicts = p.conflicts && fresh.found.has_conflicts();
                    fresh.error = Some(e.clone());
                    self.recovery = Some(fresh);
                    return Err(e);
                }
                let conflicts = p.conflicts;
                if let Err(e) = self.recover(found, conflicts) {
                    self.recovery = Some(RecoveryPrompt {
                        error: Some(e.clone()),
                        ..p
                    });
                    return Err(e);
                }
            }
            Choice::Discard => self.discard_autosave(&dir),
            Choice::Later => {
                self.autosaver.blocked = Some(Blocked::Later);
                self.st.view.set_status(
                    "Autosave is off until you recover or discard the last session's edits",
                    false,
                );
            }
        }
        Ok(())
    }

    /// Ask again (the status bar's indicator, or a save while Later).
    pub fn reask_recovery(&mut self) {
        let Some(dir) = self.st.doc.as_ref().map(|d| d.dir.clone()) else {
            return;
        };
        match autosave::inspect(&dir) {
            Inspect::Found(f) if !f.entries.is_empty() => {
                self.autosaver.blocked = Some(Blocked::Undecided);
                self.recovery = Some(RecoveryPrompt::new(f));
            }
            Inspect::Found(_) | Inspect::None => {
                let _ = autosave::clear(&dir);
                self.autosaver.blocked = None;
            }
            Inspect::DiskUnreadable(msg) => {
                self.st.toast(
                    format!("The last session's autosave can't be compared with the project ({msg}); it is kept"),
                    true,
                );
                self.autosaver.blocked = Some(Blocked::Later);
            }
            Inspect::Broken(msg) => {
                let _ = autosave::discard(&dir);
                self.st.toast(
                    format!("The last session's autosave can't be read and was set aside: {msg}"),
                    true,
                );
                self.autosaver.blocked = None;
            }
        }
    }

    fn discard_autosave(&mut self, dir: &Path) {
        match autosave::discard(dir) {
            Ok(d) => {
                let msg = format!(
                    "Discarded the last session's edits (kept in {})",
                    d.display()
                );
                tracing::info!("{msg}");
                self.st.view.set_status(msg, false);
            }
            Err(e) => self
                .st
                .toast(format!("Couldn't move the autosave aside: {e}"), true),
        }
        self.autosaver.blocked = None;
    }

    /// Recover: load the disk with the autosave on top, its files dirty
    /// (nothing written until a save), undo history empty. Conflicting
    /// entries are skipped unless `conflicts`; skipped ones aren't lost:
    /// the old autosave goes to `.worlds/autosave.discarded/` first.
    fn recover(&mut self, found: Found, conflicts: bool) -> Result<(), String> {
        let Some(dir) = self.st.doc.as_ref().map(|d| d.dir.clone()) else {
            return Err("no project open".into());
        };
        let plan = found.plan(conflicts);
        let project =
            worlds_core::format::load_with_overlay(&dir, &plan.overlay, plan.dirty.clone())
                .map_err(|e| format!("the autosave doesn't load over the files on disk: {e}"))?;
        // The load records the disk it read as each file's base. If a
        // recovered file changed after `found` was made, its base isn't the
        // disk the decision was about: saving would overwrite that change
        // unasked. Refuse; the caller inspects again.
        for e in found
            .entries
            .iter()
            .filter(|e| plan.applied.contains(&e.path))
        {
            if project.files.base_hash(&e.path) != e.disk.as_deref().map(autosave::hash) {
                return Err(format!(
                    "{} changed on disk during recovery; try again",
                    autosave::slash(&e.path)
                ));
            }
        }
        let mut kept = None;
        if !plan.skipped.is_empty() {
            kept = Some(
                autosave::discard(&dir)
                    .map_err(|e| format!("couldn't keep the skipped files' autosave: {e}"))?,
            );
        }
        let doc = crate::doc::Document::from_project(dir.clone(), project);
        self.open_document(doc);
        if let (Some(doc), Some(e)) = (self.st.doc.as_ref(), self.commands.editing()) {
            e.sync_unsaved(&doc.project);
        }
        let rev = self.commands.editing_ref().map_or(0, |e| e.rev());
        // Without skips the old autosave is this session's now; with them
        // it was moved aside.
        self.autosaver.adopt(rev, kept.is_none());
        self.autosaver.blocked = None;
        let n = plan.applied.len();
        let mut msg = format!(
            "Recovered {n} file{} from autosave; Ctrl+S to keep them",
            if n == 1 { "" } else { "s" }
        );
        if let Some(d) = &kept {
            msg.push_str(&format!(
                "; {} conflicting file{} skipped (kept in {})",
                plan.skipped.len(),
                if plan.skipped.len() == 1 { "" } else { "s" },
                d.display()
            ));
        }
        tracing::info!("{msg}");
        eprintln!("autosave: {msg}");
        self.st.view.set_status(msg.clone(), false);
        self.st.toast(msg, false);
        let slash = |p: &PathBuf| autosave::slash(p);
        self.recovered = Some(json!({
            "files": plan.applied.iter().map(slash).collect::<Vec<_>>(),
            "skipped": plan.skipped.iter().map(slash).collect::<Vec<_>>(),
            "session": { "pid": found.manifest.pid, "written": found.manifest.written },
        }));
        self.write_recover_report(|| found.report(&plan.applied, true));
        Ok(())
    }

    /// The recovery prompt (modal).
    pub(super) fn recovery_window(&mut self, ctx: &egui::Context) {
        let Some(p) = self.recovery.as_mut() else {
            return;
        };
        let accent = self.st.accent;
        let warn = Color32::from_rgb(230, 150, 90);
        let mut choice = None;
        let m = &p.found.manifest;
        let n = p.found.entries.len();
        let conflicts = p.found.entries.iter().filter(|e| e.conflict).count();
        let (plus, minus) = p
            .found
            .entries
            .iter()
            .fold((0, 0), |(a, b), e| (a + e.plus, b + e.minus));
        let screen = ctx.content_rect();
        egui::Window::new("Recover unsaved edits?")
            .collapsible(false)
            .resizable(false)
            .fade_in(false)
            .fade_out(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .max_width((screen.width() - 80.0).clamp(360.0, 760.0))
            .show(ctx, |ui| {
                ui.label(format!(
                    "Worlds didn't close cleanly (session pid {}, last autosave {}{}).",
                    m.pid,
                    autosave::describe_time(&m.written),
                    m.last_label
                        .as_deref()
                        .map(|l| format!(", after \u{2018}{l}\u{2019}"))
                        .unwrap_or_default()
                ));
                ui.label(format!(
                    "Unsaved edits to {n} file{}: +{plus} \u{2212}{minus} lines{}",
                    if n == 1 { "" } else { "s" },
                    if conflicts > 0 {
                        format!(", {conflicts} changed on disk since")
                    } else {
                        String::new()
                    }
                ));
                ui.add_space(4.0);
                let list_h = (screen.height() * 0.5).clamp(120.0, 420.0);
                let first_modified = p
                    .found
                    .entries
                    .iter()
                    .position(|e| e.state == FileState::Modified);
                egui::ScrollArea::vertical()
                    .max_height(list_h)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for (i, e) in p.found.entries.iter().enumerate() {
                            let path = autosave::slash(&e.path);
                            let mut head = egui::text::LayoutJob::default();
                            let mono = egui::TextFormat {
                                font_id: egui::FontId::monospace(12.0),
                                color: ui.visuals().text_color(),
                                ..Default::default()
                            };
                            head.append(&path, 0.0, mono.clone());
                            head.append(
                                &format!("  {}  ", e.state.name()),
                                0.0,
                                egui::TextFormat {
                                    color: ui.visuals().weak_text_color(),
                                    ..mono.clone()
                                },
                            );
                            head.append(
                                &format!("+{}", e.plus),
                                0.0,
                                egui::TextFormat {
                                    color: Color32::from_rgb(110, 190, 110),
                                    ..mono.clone()
                                },
                            );
                            head.append(
                                &format!(" \u{2212}{}", e.minus),
                                0.0,
                                egui::TextFormat {
                                    color: Color32::from_rgb(220, 110, 110),
                                    ..mono.clone()
                                },
                            );
                            if e.conflict {
                                head.append(
                                    "  conflict: disk changed since",
                                    0.0,
                                    egui::TextFormat {
                                        color: warn,
                                        ..mono.clone()
                                    },
                                );
                            }
                            let r = egui::CollapsingHeader::new(head)
                                .id_salt(("recovery row", i))
                                .default_open(Some(i) == first_modified)
                                .show(ui, |ui| {
                                    let diff = p.diffs[i].get_or_insert_with(|| e.diff());
                                    egui::ScrollArea::both()
                                        .id_salt(("recovery diff", i))
                                        .max_height(240.0)
                                        .show(ui, |ui| diff_view(ui, diff));
                                });
                            let _ = r;
                            if e.state == FileState::Deleted && e.disk.is_none() {
                                ui.weak("  (already gone on disk)");
                            }
                        }
                    });
                ui.add_space(4.0);
                if conflicts > 0 {
                    ui.checkbox(
                        &mut p.conflicts,
                        "Recover conflicting files too (they overwrite the change on disk when you save)",
                    );
                }
                if let Some(e) = &p.error {
                    ui.label(RichText::new(e).color(warn));
                }
                ui.label(
                    RichText::new(
                        "Recover loads these edits unsaved (Ctrl+S keeps them). Discard keeps a copy in .worlds/autosave.discarded/.",
                    )
                    .weak(),
                );
                ui.horizontal(|ui| {
                    let rec = ui.button(RichText::new("Recover").color(accent));
                    if rec.clicked() {
                        choice = Some(Choice::Recover);
                    }
                    if ui.button("Discard").clicked() {
                        choice = Some(Choice::Discard);
                    }
                    if ui.button("Later").clicked() {
                        choice = Some(Choice::Later);
                    }
                });
            });
        if let Some(c) = choice
            && let Err(e) = self.recovery_choose(c)
        {
            tracing::warn!("recovery: {e}");
        }
    }

    /// "Open in another Worlds": shown instead of the project, Quit closes.
    pub(super) fn locked_window(&mut self, ctx: &egui::Context) {
        let Some(msg) = self.locked_out.clone() else {
            return;
        };
        egui::Window::new("Project already open")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                let mut c = msg.chars();
                let msg: String = c
                    .next()
                    .map(|f| f.to_uppercase().chain(c).collect())
                    .unwrap_or_default();
                ui.label(format!("{msg}."));
                ui.label("Close it there first, or switch to that window.");
                if ui.button("Quit").clicked() {
                    self.allow_close = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
    }
}

impl EditorApp {
    /// Once per frame, after the drain point: write an autosave when one is
    /// due (never inside a gesture or agent batch), or remove this
    /// session's once the document is clean again (saved, reloaded).
    pub(super) fn autosave_tick(&mut self, ctx: &egui::Context) {
        let focused = ctx.input(|i| i.viewport().focused);
        let focus_lost = self.was_focused == Some(true) && focused == Some(false);
        self.was_focused = focused;
        let Some(doc) = &self.st.doc else {
            return;
        };
        let rev = self.commands.editing_ref().map_or(0, |e| e.rev());
        let tick = crate::autosave::Tick {
            rev,
            dirty: doc.project.is_dirty(),
            busy: self.busy(),
            focus_lost,
        };
        let now = Instant::now();
        for e in self.autosaver.poll(now) {
            self.st.toast(e, true);
        }
        let (due, reask) = self.autosaver.due(&tick, now);
        match due {
            Due::Nothing => {}
            Due::Clear => self.autosaver.clear(),
            Due::After(d) => ctx.request_repaint_after(d),
            Due::Write => self.autosave_now(),
        }
        if reask {
            self.reask_recovery();
        }
    }

    /// Autosave the document now (the dirty files on the UI thread, the
    /// write on the writer thread).
    pub fn autosave_now(&mut self) {
        let Some(doc) = &self.st.doc else {
            return;
        };
        let rev = self.commands.editing_ref().map_or(0, |e| e.rev());
        let label = self
            .commands
            .editing_ref()
            .and_then(|e| e.history().undo_entries().last().map(|x| x.label.clone()));
        match self.autosaver.write_now(&doc.project, rev, label) {
            Ok(t) => tracing::debug!("autosave queued ({:.2} ms render)", t.as_secs_f64() * 1e3),
            Err(e) => {
                tracing::warn!("{e}");
                self.st.view.set_status(e, true);
            }
        }
    }

    /// SIGINT/SIGTERM: autosave unsaved edits (if this session autosaves)
    /// and wait for the write, at most 5 s (the signal's grace is 10 s).
    pub fn autosave_sync(&mut self) {
        let Some(doc) = &self.st.doc else {
            return;
        };
        let rev = self.commands.editing_ref().map_or(0, |e| e.rev());
        let dirty = doc.project.is_dirty();
        let _ = self.autosaver.poll(Instant::now());
        if dirty && self.autosaver.active() && !self.autosaver.current(rev) {
            self.autosave_now();
        }
        if !self.autosaver.flush(FLUSH_TIMEOUT) {
            tracing::warn!("autosave still writing after 5 s; leaving anyway");
        }
        for e in self.autosaver.poll(Instant::now()) {
            eprintln!("autosave: {e}");
        }
        if dirty && self.autosaver.active() && !self.autosaver.current(rev) {
            eprintln!("autosave: the unsaved edits could not be autosaved before exiting");
        }
    }

    /// The end of the session: a clean document's autosave goes, queued
    /// writes finish, the session lock is released (`editor.json` removed).
    /// An unsaved document keeps its autosave (a signal, or Discard that
    /// already removed it).
    ///
    /// A write still running after the wait keeps the lock: releasing it
    /// would let another editor take the project while this writer still
    /// replaces its manifest. The process ends right after, and the OS
    /// releases the lock when the writer is gone too.
    pub fn shutdown(&mut self) {
        if self.st.doc.as_ref().is_some_and(|d| !d.project.is_dirty()) {
            self.autosaver.clear();
        }
        let idle = self.autosaver.flush(FLUSH_TIMEOUT);
        if let Some(l) = self.lock.take() {
            if idle {
                l.release();
            } else {
                tracing::warn!(
                    "autosave still writing after 5 s; keeping the session lock until exit"
                );
                std::mem::forget(l);
            }
        }
    }

    /// The status bar's note while autosave is off because the last
    /// session's autosave waits (Later): click to decide.
    pub(super) fn autosave_indicator(&mut self, ui: &mut egui::Ui) {
        if self.autosaver.blocked != Some(Blocked::Later) || self.recovery.is_some() {
            return;
        }
        let r = ui
            .add(
                egui::Label::new(
                    RichText::new("Autosave off: last session's edits waiting")
                        .color(Color32::from_rgb(230, 150, 90)),
                )
                .sense(egui::Sense::click()),
            )
            .on_hover_text("An earlier Worlds session didn't close cleanly. Click to recover or discard its edits; until then this session isn't autosaved.");
        if r.clicked() {
            self.reask_recovery();
        }
        ui.separator();
    }
}

/// A unified diff, colored per line.
fn diff_view(ui: &mut egui::Ui, diff: &str) {
    let mut job = egui::text::LayoutJob::default();
    for line in diff.lines() {
        let color = match line.as_bytes().first() {
            Some(b'+') if !line.starts_with("+++") => Color32::from_rgb(110, 190, 110),
            Some(b'-') if !line.starts_with("---") => Color32::from_rgb(220, 110, 110),
            Some(b'@') => Color32::from_rgb(120, 160, 220),
            _ => ui.visuals().weak_text_color(),
        };
        job.append(
            &format!("{line}\n"),
            0.0,
            egui::TextFormat {
                font_id: egui::FontId::monospace(11.0),
                color,
                ..Default::default()
            },
        );
    }
    ui.label(job);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_held_lock_lets_the_session_autosave() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!session_locked(None));
        let none = autosave::EditorLock::unlocked(tmp.path());
        assert!(!session_locked(Some(&none)));
        let held = autosave::EditorLock::acquire(tmp.path(), "test").unwrap();
        assert!(session_locked(Some(&held)));
    }
}
