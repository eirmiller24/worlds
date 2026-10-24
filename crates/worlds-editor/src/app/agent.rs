//! The live API in the shell: starting and stopping the server, the drain
//! point, user-edit events, the agent indicator, agent toasts with one-click
//! undo, and approval mode (toggle, pending panel, ghosts).

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{Color32, RichText};
use worlds_core::command::Origin;
use worlds_server::{Host, Server, ServerConfig, ServerHandle, UserChange};

use super::EditorApp;
use crate::doc::Document;
use crate::live::{
    EditorHost, HostEffects, agent_entry_on_top, new_agent_entries, selection_of,
    sync_view_after_reload,
};
use crate::snapshot::SnapshotRenderer;

/// The editor's live API state.
#[derive(Default)]
pub struct Live {
    pub server: Option<ServerHandle>,
    /// Why the server isn't running (another process serves the project,
    /// or the socket failed).
    pub error: Option<String>,
    /// Offscreen renderer for `view.snapshot`, created on first use.
    pub snapshots: Option<SnapshotRenderer>,
    pub effects: HostEffects,
    /// The last `selection` sent: (level, entities, notes).
    #[allow(clippy::type_complexity)]
    last_selection: Option<(Option<String>, Vec<String>, Vec<String>, Vec<String>)>,
}

pub use crate::live::AgentUndo;

impl EditorApp {
    /// Start the live API for the open project (a failure is shown in the
    /// indicator and the log; the editor runs without it).
    pub(super) fn start_server(&mut self) {
        self.stop_server();
        let Some(doc) = &self.st.doc else {
            return;
        };
        let ctx = self.ctx.clone();
        let config = ServerConfig {
            project_dir: doc.dir.clone(),
            project_name: doc.project.name.clone(),
            headless: false,
            wake: Some(Arc::new(move || ctx.request_repaint())),
        };
        match Server::start(config) {
            Ok(s) => {
                tracing::info!("live API listening on 127.0.0.1:{}", s.port());
                self.live.server = Some(s);
                self.live.error = None;
            }
            Err(e) => {
                tracing::warn!("live API not started: {e}");
                self.live.error = Some(e.to_string());
            }
        }
    }

    /// Stop the server (closes connections, removes `server.json`).
    pub(super) fn stop_server(&mut self) {
        if let Some(s) = self.live.server.take() {
            s.stop();
        }
        self.live.last_selection = None;
        self.st.ghosts.clear();
    }

    /// The running server, if any.
    pub fn server(&self) -> Option<&ServerHandle> {
        self.live.server.as_ref()
    }

    /// Why the server isn't running, if it failed to start.
    pub fn server_error(&self) -> Option<&str> {
        self.live.error.as_deref()
    }

    /// Before the user records an entry or steps the history: record the
    /// open agent batches' parts so far, so the undo stack stays in order.
    pub(super) fn flush_agent_batches(&mut self) {
        if let (Some(s), Some(ed)) = (self.live.server.as_mut(), self.commands.editing()) {
            s.flush_batches(ed.history_mut());
        }
    }

    /// Send `changed` (origin user) for each entry and undo/redo step the
    /// command layer recorded since the last call.
    pub(super) fn report_user_edits(&mut self) {
        let Some(ed) = self.commands.editing() else {
            return;
        };
        let edits = ed.take_user_edits();
        let Some(s) = self.live.server.as_mut() else {
            return;
        };
        for e in &edits {
            s.notify_changed(UserChange {
                kind: e.kind,
                label: &e.label,
                entry_origin: e.entry_origin.clone(),
                changes: &e.changes,
            });
        }
    }

    /// The drain point, once per frame after the tool events: handle the
    /// queued requests unless a gesture is in progress, then refresh the
    /// ghosts and report a changed selection.
    pub(super) fn live_frame(&mut self) {
        if self.live.server.is_none() || self.st.doc.is_none() {
            return;
        }
        let busy = self.st.viewport.in_gesture()
            || self.commands.editing_ref().is_some_and(|e| e.in_gesture());
        if !busy {
            let handled = self.with_host(|s, h| s.drain(h)).unwrap_or_default();
            for h in &handled {
                match (h.error, &h.pending) {
                    (Some(code), _) => {
                        tracing::debug!("{} {}: error {code}", h.client, h.method)
                    }
                    (_, Some(id)) => tracing::info!("{} {}: pending as {id}", h.client, h.method),
                    _ => tracing::debug!("{} {}", h.client, h.method),
                }
            }
        }
        let (Some(s), Some(doc)) = (self.live.server.as_mut(), self.st.doc.as_ref()) else {
            return;
        };
        if !s.pending().is_empty() || !self.st.ghosts.is_empty() {
            crate::ghost::sync(&mut self.st.ghosts, s.pending(), &doc.project);
        }
        if s.open_batches() > 0 {
            // Idle batches end after 60 s even if nothing else repaints.
            self.ctx.request_repaint_after(Duration::from_secs(1));
        }
        let sel = selection_of(doc, &self.st.view);
        if self.live.last_selection.as_ref() != Some(&sel) {
            s.notify_selection(sel.0.as_deref(), &sel.1, &sel.2, &sel.3);
            self.live.last_selection = Some(sel);
        }
    }

    /// Lend the document to the server for one call, then finish what the
    /// call left to the app: a reload, a focus, problems, agent toasts and
    /// invalidations.
    fn with_host<R>(&mut self, f: impl FnOnce(&mut ServerHandle, &mut dyn Host) -> R) -> Option<R> {
        let synthetic = self.opts.is_synthetic();
        let recovered = self.recovered.clone();
        let server = self.live.server.as_mut()?;
        let doc = self.st.doc.as_mut()?;
        let editing = self.commands.editing()?;
        let before = editing.history().undo_entries().len();
        let mut host = EditorHost {
            doc,
            editing,
            view: &mut self.st.view,
            viewport: &mut self.st.viewport,
            invalidate: &mut self.invalidations,
            own_writes: &mut self.own_writes,
            snapshots: &mut self.live.snapshots,
            effects: &mut self.live.effects,
            synthetic,
            recovered,
        };
        let r = f(server, &mut host);
        self.after_host(before);
        Some(r)
    }

    fn after_host(&mut self, before: usize) {
        let effects = std::mem::take(&mut self.live.effects);
        if let Some(old) = effects.reloaded {
            self.finish_reload(&old);
            self.reset_problems();
        }
        if effects.problems
            && let (Some(s), Some(doc)) = (self.live.server.as_mut(), self.st.doc.as_ref())
        {
            s.notify_problems(&doc.problems);
        }
        let new: Vec<AgentUndo> = self
            .commands
            .editing_ref()
            .map(|ed| new_agent_entries(ed.history(), before))
            .unwrap_or_default();
        for u in new {
            let text = match &u.entry.origin {
                Origin::Agent(name) if !u.entry.label.starts_with(name.as_str()) => {
                    format!("{name}: {}", u.entry.label)
                }
                _ => u.entry.label.clone(),
            };
            tracing::info!("agent: {text}");
            self.st.push_toast(
                text,
                false,
                Instant::now() + Duration::from_secs(8),
                Some(u),
            );
        }
        self.apply_invalidations();
    }

    /// Whether `u`'s entry is still the top of the undo stack.
    pub fn agent_entry_on_top(&self, u: &AgentUndo) -> bool {
        self.commands
            .editing_ref()
            .is_some_and(|ed| agent_entry_on_top(ed.history(), u))
    }

    /// The toast's Undo: undo exactly that agent entry, if it is still on
    /// top of the undo stack.
    pub fn undo_agent_entry(&mut self, u: &AgentUndo) -> bool {
        self.flush_agent_batches();
        if !self.agent_entry_on_top(u) {
            self.st.toast(
                format!(
                    "\"{}\" is no longer the last edit; undo what came after it first",
                    u.entry.label
                ),
                true,
            );
            return false;
        }
        self.run(super::actions::Action::Undo);
        true
    }

    /// Approval mode on or off (the toolbar toggle; per session, not saved).
    pub fn set_approval(&mut self, on: bool) {
        if let Some(s) = self.live.server.as_mut() {
            s.set_approval(on);
        }
    }

    /// Accept pending batch `id`: it applies as one agent entry.
    pub fn apply_pending(&mut self, id: &str) -> Result<(), String> {
        let r = self
            .with_host(|s, h| s.apply_pending(h, id))
            .ok_or("no live API")?;
        let still_pending = self
            .live
            .server
            .as_ref()
            .is_some_and(|s| s.pending().iter().any(|p| p.id == id));
        match r {
            Ok(_) => Ok(()),
            Err(None) => Err(format!("no pending batch {id}")),
            // Refused, not tried: an older batch waits.
            Err(Some(e)) if still_pending => {
                self.st.toast(e.message.clone(), true);
                Err(e.message)
            }
            Err(Some(e)) => {
                let msg = format!("Batch {id} no longer applies: {}", e.message);
                self.st.toast(msg.clone(), true);
                Err(msg)
            }
        }
    }

    /// Reject pending batch `id` (and that client's later ones).
    pub fn reject_pending(&mut self, id: &str) -> Vec<String> {
        let ids = self
            .live
            .server
            .as_mut()
            .map(|s| s.reject_pending(id))
            .unwrap_or_default();
        if let (Some(s), Some(doc)) = (self.live.server.as_ref(), self.st.doc.as_ref()) {
            crate::ghost::sync(&mut self.st.ghosts, s.pending(), &doc.project);
        }
        ids
    }

    /// Re-read the document after the server replaced it (or the user
    /// reloaded): scene, bakes, watcher. The view was already carried over.
    pub(super) fn finish_reload(&mut self, old: &Document) {
        let tool = self.st.view.tool;
        self.st
            .viewport
            .cancel_gesture(tool, &mut self.st.tool_events);
        self.st.atlas.clear();
        self.st.edit_bufs.clear();
        self.st.pending.clear();
        self.st.ghosts.clear();
        let headless = self.opts.headless;
        let Some(doc) = self.st.doc.as_mut() else {
            return;
        };
        // Types the reloaded project no longer has leave the palette, and
        // what it cached from the old document goes. `defs_gen` carries on
        // from the old document, so nothing keyed on it matches again.
        self.st.view.entities.retain_known(&doc.project);
        self.st.entities.document_replaced();
        self.st.thumbs.clear();
        doc.defs_gen = old.defs_gen + 1;
        if headless {
            doc.inline_layer_bakes = true;
            doc.bake_all_blocking();
        } else {
            let ctx = self.ctx.clone();
            doc.wake = Some(std::sync::Arc::new(move || ctx.request_repaint()));
            let ctx = self.ctx.clone();
            doc.start_bakes(move || ctx.request_repaint());
        }
        self.st.viewport.set_document(doc, &self.gpu);
        let tilesets_changed = doc.project.tilesets != old.project.tilesets;
        let problems = doc.problems.len();
        tracing::info!("reloaded {}", doc.dir.display());
        if tilesets_changed && !headless {
            self.watcher = None;
            if let Some(doc) = self.st.doc.take() {
                self.start_watcher(&doc);
                self.st.doc = Some(doc);
            }
        }
        if problems > 0 {
            self.st.toast(
                format!(
                    "{problems} validation problem{} (see Problems)",
                    if problems == 1 { "" } else { "s" }
                ),
                true,
            );
        }
    }

    /// Reload from disk on the user's request or a file change.
    pub fn reload(&mut self) {
        let Some(old) = self.st.doc.take() else {
            return;
        };
        let doc = match self.load(&old.dir) {
            Ok(d) => d,
            Err(e) => {
                self.st
                    .toast(format!("Reload failed, keeping the open copy: {e}"), true);
                self.st.doc = Some(old);
                return;
            }
        };
        sync_view_after_reload(&mut self.st.view, &old, &doc);
        self.st.doc = Some(doc);
        self.finish_reload(&old);
        self.commands.reloaded();
        self.reset_problems();
        if let Some(s) = self.live.server.as_mut() {
            s.notify_reloaded();
        }
    }

    /// After the user saved `dirty`: send `saved` and `problems`.
    pub(super) fn report_save(&mut self, dirty: &[std::path::PathBuf]) {
        if let (Some(s), Some(doc)) = (self.live.server.as_mut(), self.st.doc.as_ref()) {
            s.notify_saved(&worlds_server::saved_paths(dirty));
            s.notify_problems(&doc.problems);
        }
    }

    /// The agent activity indicator at the right of the toolbar, drawn
    /// right to left in a right-to-left `ui` within `budget` points. Parts
    /// come in order of importance and each only if it fits: the status dot
    /// (its tooltip always has everything), the Approve toggle, the pending
    /// count, the client, the last batch's label (cut to the room left).
    pub(super) fn agent_indicator(&mut self, ui: &mut egui::Ui, budget: f32) {
        let gap = ui.spacing().item_spacing.x;
        let text_w = |ui: &egui::Ui, text: &str| {
            let font = egui::TextStyle::Body.resolve(ui.style());
            ui.painter()
                .layout_no_wrap(text.to_string(), font, Color32::WHITE)
                .size()
                .x
                + gap
        };
        let dot = |ui: &mut egui::Ui, c: Color32, hover: String| {
            let (r, resp) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
            ui.painter().circle_filled(r.center(), 4.0, c);
            resp.on_hover_text(hover);
        };
        let mut left = budget - 10.0 - gap;
        let mut approval = None;
        match (&self.live.server, &self.live.error) {
            (Some(s), _) => {
                let clients = s.clients();
                let names: Vec<String> = clients
                    .iter()
                    .map(|c| format!("{} {} ({})", c.name, c.version, c.session))
                    .collect();
                let pending = s.pending().len();
                let mut hover = if names.is_empty() {
                    format!("Live API on 127.0.0.1:{}; no agent connected", s.port())
                } else {
                    format!("Live API on 127.0.0.1:{}\n{}", s.port(), names.join("\n"))
                };
                if let Some(l) = s.last_batch() {
                    hover.push_str(&format!("\nLast agent batch: {l}"));
                }
                if pending > 0 {
                    hover.push_str(&format!("\n{pending} batch(es) waiting for approval"));
                }
                // What fits, in order of importance.
                let approve_w =
                    text_w(ui, "Approve") + ui.spacing().icon_width + ui.spacing().icon_spacing;
                let show_approve = approve_w <= left;
                if show_approve {
                    left -= approve_w;
                }
                let mut pending_text = None;
                if pending > 0 {
                    for t in [format!("{pending} pending"), pending.to_string()] {
                        let w = text_w(ui, &t);
                        if w <= left {
                            left -= w;
                            pending_text = Some(t);
                            break;
                        }
                    }
                }
                let who = match clients.len() {
                    0 => "Agent: listening".to_string(),
                    1 => format!("Agent: {}", clients[0].name),
                    n => format!("Agents: {n}"),
                };
                let w = text_w(ui, &who);
                let show_who = w <= left;
                let mut last = None;
                if show_who {
                    left -= w;
                    if let Some(l) = s.last_batch() {
                        let mut short: String = l.chars().take(40).collect();
                        let mut cut = short.len() < l.len();
                        while !short.is_empty() && text_w(ui, &format!("{short}…")) > left {
                            short.pop();
                            cut = true;
                        }
                        if short.chars().count() >= 4 {
                            last = Some(if cut { format!("{short}…") } else { short });
                        }
                    }
                }
                // Drawn right to left: Approve, pending, last batch, who, dot.
                if show_approve {
                    let mut on = s.approval();
                    if ui
                        .checkbox(&mut on, "Approve")
                        .on_hover_text(
                            "Approval mode: agent batches wait for Apply or Reject, \
                             drawn as ghosts on the map (this session only)",
                        )
                        .changed()
                    {
                        approval = Some(on);
                    }
                }
                if let Some(t) = pending_text {
                    ui.label(RichText::new(t).color(self.st.accent));
                }
                if let Some(l) = last {
                    ui.weak(l);
                }
                if show_who {
                    ui.label(who);
                }
                dot(
                    ui,
                    if clients.is_empty() {
                        Color32::from_gray(110)
                    } else {
                        Color32::from_rgb(90, 200, 120)
                    },
                    hover,
                );
            }
            (None, Some(err)) => {
                if text_w(ui, "Agent: off") <= left {
                    ui.weak("Agent: off").on_hover_text(err.as_str());
                }
                dot(ui, Color32::from_rgb(220, 90, 90), err.clone());
            }
            (None, None) => {
                if text_w(ui, "Agent: no project") <= left {
                    ui.weak("Agent: no project");
                }
                dot(ui, Color32::from_gray(90), "No project open".into());
            }
        }
        if let Some(on) = approval {
            self.set_approval(on);
        }
    }

    /// The pending batches of approval mode, with Apply and Reject.
    pub(super) fn pending_panel(&mut self, ctx: &egui::Context) {
        let Some(s) = &self.live.server else {
            return;
        };
        if s.pending().is_empty() {
            return;
        }
        let rows: Vec<(String, String, String, usize, Option<String>)> = s
            .pending()
            .iter()
            .map(|p| {
                let g = self.st.ghosts.iter().find(|g| g.batch == p.id);
                (
                    p.id.clone(),
                    p.label.clone(),
                    p.client.clone(),
                    g.map_or(0, |g| g.cells()),
                    g.and_then(|g| g.error.clone()),
                )
            })
            .collect();
        let mut apply = None;
        let mut reject = None;
        // Inside the Map pane's top-right corner, kept within the map, so it
        // never covers the inspector or other panes. Before the first frame
        // drew the map: below the toolbar at the right.
        let map = self.st.map_rect.unwrap_or_else(|| {
            let screen = ctx.content_rect();
            egui::Rect::from_min_max(screen.min + egui::vec2(0.0, 40.0), screen.max)
        });
        let margin = 12.0;
        egui::Window::new("Agent batches waiting")
            .id(egui::Id::new("agent batches panel"))
            .collapsible(true)
            .resizable(false)
            .pivot(egui::Align2::RIGHT_TOP)
            .fixed_pos(map.right_top() + egui::vec2(-margin, margin))
            .constrain_to(map.shrink(margin / 2.0))
            .show(ctx, |ui| {
                for (i, (id, label, client, cells, error)) in rows.iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(label).strong())
                            .on_hover_text(format!("{id} from {client}"));
                        if *cells > 0 {
                            ui.weak(format!("{cells} cells"));
                        }
                        if let Some(e) = error {
                            ui.colored_label(Color32::from_rgb(230, 150, 90), "stale")
                                .on_hover_text(e.as_str());
                        }
                        // Oldest first: later batches may build on it.
                        if ui
                            .add_enabled(i == 0, egui::Button::new("Apply"))
                            .on_disabled_hover_text("Apply or reject the batches above first")
                            .clicked()
                        {
                            apply = Some(id.clone());
                        }
                        if ui
                            .button("Reject")
                            .on_hover_text("Also rejects this agent's later batches")
                            .clicked()
                        {
                            reject = Some(id.clone());
                        }
                    });
                }
            });
        if let Some(id) = apply {
            let _ = self.apply_pending(&id);
        }
        if let Some(id) = reject {
            self.reject_pending(&id);
        }
    }
}
