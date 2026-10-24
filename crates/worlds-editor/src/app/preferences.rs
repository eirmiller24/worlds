//! The Preferences window (Ctrl+, or "Preferences…" in the command
//! palette). For now it has one section, Shortcuts: every command in
//! [`crate::keymap`], grouped, with its keys and buttons to change them.
//!
//! Rebind captures the next key press and makes it the command's only
//! chord; `+` captures one more (Redo has two by default). Escape cancels a
//! capture, so Escape itself can't be bound (it's one of the fixed keys).
//! Clear unbinds, Reset puts the defaults back. Two commands on one chord
//! where both can fire get a ⚠ with the other's name and a button that
//! unbinds the other one, so fixing a clash is one click.
//!
//! Every change applies at once and is saved to the preferences file
//! ([`crate::prefs`]) right away.

use egui::{Color32, RichText};

use super::{EditorApp, fields};
use crate::keymap::{Chord, Cmd, Group, IS_MAC, Keymap};

/// The window's state.
#[derive(Default)]
pub struct PrefsWindow {
    pub open: bool,
    /// The search box: words that must all appear in a row's name, id or
    /// keys.
    pub search: String,
    /// Waiting for a key press for this command.
    pub capture: Option<Capture>,
    /// Focus the search box next frame.
    pub just_opened: bool,
}

/// A rebind in progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capture {
    pub cmd: Cmd,
    /// Add the key to the ones it has (`+`) rather than replace them.
    pub add: bool,
}

/// The keys that stay as they are, for the window's list (see the
/// `keymap` module docs for why).
const FIXED: &[(&str, &str)] = &[
    (
        "Ctrl+C, Ctrl+X, Ctrl+V, Ctrl+Shift+V",
        "copy, cut, paste, paste in place (the system clipboard's keys)",
    ),
    (
        "Escape",
        "cancel: a prompt, a gesture, an armed paste, a pick, a rebind",
    ),
    ("Enter", "confirm a prompt or a dialog, commit a field"),
    ("Space (held)", "pan with any tool"),
    (
        "Shift, Ctrl, Alt with the mouse",
        "outline, add to the selection, pick, pixel moves",
    ),
    (
        "Arrows, Enter, Escape in the command palette",
        "move, run, close",
    ),
];

impl PrefsWindow {
    pub fn show(&mut self) {
        self.open = true;
        self.just_opened = true;
    }

    /// While capturing, take this frame's key presses before anything
    /// else sees them: the first real key becomes the binding, Escape
    /// cancels. Typed text and clipboard events are swallowed too, so the
    /// press never also types or pastes. Returns whether `km` changed.
    pub fn take_capture(&mut self, ctx: &egui::Context, km: &mut Keymap) -> bool {
        let Some(cap) = self.capture else {
            return false;
        };
        if !self.open {
            self.capture = None;
            return false;
        }
        let mut got: Option<Result<Chord, ()>> = None;
        ctx.input_mut(|i| {
            i.events.retain(|e| match e {
                egui::Event::Key {
                    key,
                    pressed,
                    modifiers,
                    ..
                } => {
                    if *pressed && got.is_none() && !is_modifier(*key) {
                        got = Some(if *key == egui::Key::Escape {
                            Err(())
                        } else {
                            Ok(Chord::from_press(*key, *modifiers, IS_MAC))
                        });
                    }
                    false
                }
                egui::Event::Text(_)
                | egui::Event::Copy
                | egui::Event::Cut
                | egui::Event::Paste(_) => false,
                _ => true,
            });
        });
        match got {
            None => false,
            Some(Err(())) => {
                self.capture = None;
                false
            }
            Some(Ok(chord)) => {
                self.capture = None;
                if cap.add {
                    km.add(cap.cmd, chord);
                } else {
                    km.set(cap.cmd, vec![chord]);
                }
                true
            }
        }
    }
}

/// Keys that only ever modify (egui reports some as keys of their own).
fn is_modifier(k: egui::Key) -> bool {
    use egui::Key;
    matches!(
        k,
        Key::ShiftLeft
            | Key::ShiftRight
            | Key::ControlLeft
            | Key::ControlRight
            | Key::AltLeft
            | Key::AltRight
            | Key::SuperLeft
            | Key::SuperRight
    )
}

/// Whether `cmd` matches every word of `query` (in its name, id or keys).
fn matches(km: &Keymap, cmd: Cmd, query: &str) -> bool {
    let hay = format!("{} {} {}", cmd.name(), cmd.id(), km.labels(cmd)).to_lowercase();
    query
        .split_whitespace()
        .all(|w| hay.contains(&w.to_lowercase()))
}

impl EditorApp {
    /// Draw the window and apply what it changed (saved at once).
    pub(super) fn preferences_window(&mut self, ctx: &egui::Context) {
        if !self.prefs_win.open {
            return;
        }
        let mut open = true;
        let mut changed = false;
        let km = &mut self.st.keymap;
        let win = &mut self.prefs_win;
        let store = &self.prefs;
        let conflicts = km.conflicts();
        egui::Window::new("Preferences")
            .id(egui::Id::new("preferences window"))
            .open(&mut open)
            .collapsible(false)
            .fade_in(false)
            .default_size([640.0, 560.0])
            .default_pos(ctx.content_rect().center() - egui::vec2(310.0, 280.0))
            .show(ctx, |ui| {
                ui.heading("Shortcuts");
                ui.horizontal(|ui| {
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut win.search)
                            .hint_text("Search commands or keys…")
                            .desired_width(260.0),
                    );
                    fields::mark(ui, "prefs:search", r.rect);
                    if std::mem::take(&mut win.just_opened) {
                        r.request_focus();
                    }
                    let any_changed = Cmd::all().iter().any(|&c| !km.is_default(c));
                    let r = ui
                        .add_enabled(any_changed, egui::Button::new("Reset all"))
                        .on_hover_text("Every command back on its default keys");
                    fields::mark(ui, "prefs:reset_all", r.rect);
                    if r.clicked() {
                        km.reset_all();
                        win.capture = None;
                        changed = true;
                    }
                });
                ui.weak(match (&store.path, store.read_only) {
                    (Some(p), false) => format!("Saved to {}", p.display()),
                    (Some(p), true) => {
                        format!("Read from {} (this run saves nothing)", p.display())
                    }
                    (None, _) => "Not saved: no preferences file in this run".to_string(),
                });
                if let Some(e) = &store.last_error {
                    ui.colored_label(Color32::from_rgb(230, 120, 90), e);
                }
                if let Some(cap) = win.capture {
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(format!(
                            "Press the {} for {}… (Escape cancels)",
                            if cap.add { "extra key" } else { "new key" },
                            cap.cmd.name()
                        ))
                        .strong()
                        .color(ui.visuals().selection.stroke.color),
                    );
                }
                ui.separator();
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let mut shown = 0;
                        // One grid for every group, so the columns line up
                        // down the whole list.
                        egui::Grid::new("prefs shortcuts")
                            .num_columns(3)
                            .striped(true)
                            .min_col_width(80.0)
                            .show(ui, |ui| {
                                for g in Group::ALL {
                                    let rows: Vec<Cmd> = Cmd::all()
                                        .iter()
                                        .copied()
                                        .filter(|&c| c.group() == g && matches(km, c, &win.search))
                                        .collect();
                                    if rows.is_empty() {
                                        continue;
                                    }
                                    shown += rows.len();
                                    ui.strong(g.name());
                                    ui.end_row();
                                    for c in rows {
                                        changed |= row(ui, km, win, &conflicts, c);
                                        ui.end_row();
                                    }
                                }
                            });
                        if shown == 0 {
                            ui.weak("No command matches");
                        }
                        ui.add_space(10.0);
                        egui::CollapsingHeader::new("Keys that can't be changed")
                            .id_salt("prefs fixed keys")
                            .show(ui, |ui| {
                                egui::Grid::new("prefs fixed")
                                    .num_columns(2)
                                    .show(ui, |ui| {
                                        for (keys, what) in FIXED {
                                            ui.monospace(fixed_keys(keys));
                                            ui.label(*what);
                                            ui.end_row();
                                        }
                                    });
                            });
                    });
            });
        if !open {
            self.prefs_win.open = false;
            self.prefs_win.capture = None;
        }
        if changed {
            self.save_preferences();
        }
    }

    /// Write the keymap's changes to the preferences file; a failure is a
    /// toast (once, until a save works again).
    pub(super) fn save_preferences(&mut self) {
        let before = self.prefs.last_error.clone();
        if let Err(e) = self.prefs.save(&self.st.keymap)
            && before.as_deref() != Some(e.as_str())
        {
            self.st.toast(format!("Preferences not saved: {e}"), true);
        }
    }

    /// `"ui": "preferences"`: see [`crate::script::PreferencesStep`].
    pub(super) fn preferences_step(
        &mut self,
        p: &crate::script::PreferencesStep,
    ) -> Result<(), String> {
        let capture = match &p.capture {
            Some(id) => {
                Some(Cmd::from_id(id).ok_or_else(|| format!("no shortcut command {id:?}"))?)
            }
            None => None,
        };
        self.prefs_win.show();
        if let Some(s) = &p.search {
            self.prefs_win.search = s.clone();
        }
        self.prefs_win.capture = capture.map(|cmd| Capture { cmd, add: false });
        Ok(())
    }

    /// The Preferences window's state (tests).
    pub fn preferences_state(&self) -> &PrefsWindow {
        &self.prefs_win
    }
}

/// `Ctrl+C, Ctrl+X` with each chord printed for this platform.
fn fixed_keys(keys: &str) -> String {
    keys.split(", ")
        .map(|k| match k.split_once(' ') {
            None => super::actions::fixed(k),
            Some(_) => k.to_string(),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// One command's row: name, keys (with a conflict marker), buttons.
/// Returns whether the keymap changed.
fn row(
    ui: &mut egui::Ui,
    km: &mut Keymap,
    win: &mut PrefsWindow,
    conflicts: &[crate::keymap::Conflict],
    c: Cmd,
) -> bool {
    let id = c.id();
    let mut changed = false;
    ui.label(c.name())
        .on_hover_text(format!("{id}: works {}", c.context().label()));
    ui.horizontal(|ui| {
        let capturing = win.capture.is_some_and(|k| k.cmd == c);
        if capturing {
            ui.label(RichText::new("press a key…").italics());
        } else if km.chords(c).is_empty() {
            ui.weak("—");
        } else {
            let r = ui.monospace(km.labels(c));
            fields::mark(ui, &format!("prefs:keys:{id}"), r.rect);
        }
        if !km.is_default(c) {
            ui.weak("(changed)").on_hover_text(format!(
                "Default: {}",
                match c.defaults() {
                    [] => "none".to_string(),
                    d => d
                        .iter()
                        .map(|s| Chord::parse(s).map(|c| c.to_string()).unwrap_or_default())
                        .collect::<Vec<_>>()
                        .join(", "),
                }
            ));
        }
        for k in conflicts.iter().filter(|k| k.a == c || k.b == c) {
            let other = if k.a == c { k.b } else { k.a };
            let r = ui
                .label(RichText::new("⚠").color(Color32::from_rgb(230, 150, 90)))
                .on_hover_text(format!(
                    "{} is also {} ({})",
                    k.chord,
                    other.name(),
                    other.context().label()
                ));
            fields::mark(ui, &format!("prefs:conflict:{id}"), r.rect);
            let r = ui
                .small_button(format!("Unbind {}", other.name()))
                .on_hover_text(format!("Take {} off {}", k.chord, other.name()));
            fields::mark(ui, &format!("prefs:unbind_other:{id}"), r.rect);
            if r.clicked() {
                km.remove(other, k.chord);
                changed = true;
            }
        }
    });
    ui.horizontal(|ui| {
        let r = ui
            .small_button("Rebind")
            .on_hover_text("Press the new key next (Escape cancels)");
        fields::mark(ui, &format!("prefs:rebind:{id}"), r.rect);
        if r.clicked() {
            win.capture = Some(Capture { cmd: c, add: false });
        }
        let r = ui.small_button("+").on_hover_text("Add another key for it");
        fields::mark(ui, &format!("prefs:add:{id}"), r.rect);
        if r.clicked() {
            win.capture = Some(Capture { cmd: c, add: true });
        }
        let r = ui
            .add_enabled(!km.chords(c).is_empty(), egui::Button::new("Clear").small())
            .on_hover_text("No key for it");
        fields::mark(ui, &format!("prefs:clear:{id}"), r.rect);
        if r.clicked() {
            km.set(c, vec![]);
            changed = true;
        }
        let r = ui
            .add_enabled(!km.is_default(c), egui::Button::new("Reset").small())
            .on_hover_text("Back to the default keys");
        fields::mark(ui, &format!("prefs:reset:{id}"), r.rect);
        if r.clicked() {
            km.reset(c);
            changed = true;
        }
    });
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_matches_names_ids_and_keys() {
        let km = Keymap::defaults();
        assert!(matches(&km, Cmd::Undo, "undo"));
        assert!(matches(&km, Cmd::FitWorld, "fit world"));
        assert!(matches(&km, Cmd::FitWorld, "view.fit"));
        assert!(matches(&km, Cmd::Redo, "ctrl+y"));
        assert!(!matches(&km, Cmd::Undo, "redo"));
        assert!(matches(&km, Cmd::Undo, ""));
    }

    fn press(key: egui::Key, modifiers: egui::Modifiers) -> Vec<egui::Event> {
        vec![
            egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers,
            },
            egui::Event::Text("x".into()),
        ]
    }

    fn run(ctx: &egui::Context, events: Vec<egui::Event>, f: impl FnMut(&egui::Context)) {
        let mut f = f;
        let mut out = ctx.run_ui(
            egui::RawInput {
                events,
                ..Default::default()
            },
            |ui| f(ui.ctx()),
        );
        out.textures_delta.clear();
    }

    #[test]
    fn capture_takes_the_next_key_and_escape_cancels() {
        let ctx = egui::Context::default();
        let mut km = Keymap::defaults();
        let mut w = PrefsWindow {
            open: true,
            capture: Some(Capture {
                cmd: Cmd::Undo,
                add: false,
            }),
            ..Default::default()
        };
        // Escape cancels and changes nothing.
        run(&ctx, press(egui::Key::Escape, Default::default()), |ctx| {
            assert!(!w.take_capture(ctx, &mut km));
        });
        assert!(w.capture.is_none());
        assert!(km.is_default(Cmd::Undo));
        // A key replaces, and nothing else sees the press (nor its text).
        w.capture = Some(Capture {
            cmd: Cmd::Undo,
            add: false,
        });
        run(&ctx, press(egui::Key::U, egui::Modifiers::COMMAND), |ctx| {
            assert!(w.take_capture(ctx, &mut km));
            assert!(ctx.input(|i| i.events.is_empty()));
        });
        assert_eq!(km.chords(Cmd::Undo), &[Chord::parse("Ctrl+U").unwrap()]);
        // `+` adds.
        w.capture = Some(Capture {
            cmd: Cmd::Undo,
            add: true,
        });
        run(&ctx, press(egui::Key::F9, Default::default()), |ctx| {
            w.take_capture(ctx, &mut km);
        });
        assert_eq!(km.chords(Cmd::Undo).len(), 2);
    }
}
