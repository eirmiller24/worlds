//! Several worlds per project: the toolbar's world switcher, the New world
//! and Rename world dialog, the Delete world confirmation, moving a level
//! to another world, and the Project pane's Worlds section.
//!
//! The map shows one world at a time ([`ViewState::world_ix`], saved as
//! `ui.json` `active_world`). Switching worlds fits the new one; a level
//! made active in another world (a problem, a note, the API) brings its
//! world along. Every change is a world command, so undo works.

use egui::{Color32, RichText};
use worlds_core::{Command, Slug, World};

use super::actions::Action;
use super::project_pane::ProjectState;
use super::{Confirm, EditorApp, Modal};
use crate::doc::Document;
use crate::view::ViewState;
use crate::viewport::Fit;

/// The New world (`rename: None`) or Rename world dialog.
pub struct WorldDialog {
    /// The world being renamed.
    rename: Option<Slug>,
    slug: String,
    name: String,
    error: Option<String>,
    just_opened: bool,
}

/// How a world is named in menus: its name, else its slug.
pub fn world_label(w: &World) -> &str {
    w.name.as_deref().unwrap_or(w.slug.as_str())
}

/// A slug for a new world that no world has: `world_<n>`.
fn fresh_world_slug(doc: &Document) -> String {
    let mut n = doc.project.worlds.len() + 1;
    loop {
        let s = format!("world_{n}");
        if doc.project.world(&s).is_none() {
            return s;
        }
        n += 1;
    }
}

/// The levels of world `w` (indices into [`Document::levels`]).
pub fn world_levels(doc: &Document, w: usize) -> usize {
    doc.levels.iter().filter(|r| r.world == w).count()
}

impl EditorApp {
    /// The toolbar's world switcher: a combo of the worlds (in display
    /// order) and New world…. Returns whether it drew anything.
    pub(super) fn world_switcher(&mut self, ui: &mut egui::Ui) -> bool {
        let Some(doc) = &self.st.doc else {
            return false;
        };
        let Some(cur) = doc.project.worlds.get(self.st.view.world_ix(doc)) else {
            return false;
        };
        let mut act = None;
        let r = egui::ComboBox::from_id_salt("world switcher")
            .selected_text(world_label(cur))
            .width(110.0)
            .show_ui(ui, |ui| {
                for w in &doc.project.worlds {
                    let n = w.levels.len();
                    if ui
                        .selectable_label(w.slug == cur.slug, world_label(w))
                        .on_hover_text(format!("{} ({n} levels)", w.slug))
                        .clicked()
                    {
                        act = Some(Action::SwitchWorld(w.slug.clone()));
                    }
                }
                ui.separator();
                if ui.button("New world…").clicked() {
                    act = Some(Action::NewWorld);
                }
            });
        super::fields::mark(ui, "world switcher", r.response.rect);
        r.response
            .on_hover_text("The world the map shows (a project can hold several)");
        if let Some(a) = act {
            self.st.actions.push(a);
        }
        true
    }

    /// Show world `slug` and fit it.
    pub(super) fn switch_world(&mut self, slug: &Slug) {
        let Some(doc) = &self.st.doc else {
            return;
        };
        let Some(w) = doc.project.worlds.iter().position(|w| w.slug == *slug) else {
            self.st.toast(format!("No world {slug}"), true);
            return;
        };
        self.st.view.set_world(doc, w);
        self.st.viewport.request_fit(Fit::World);
    }

    /// Open the New world dialog (`rename: None`), or the Rename world one
    /// for `Some(world)` (`Some(None)`: the world shown).
    pub(super) fn open_world_dialog(&mut self, rename: Option<Option<Slug>>) {
        let Some(doc) = &self.st.doc else {
            return;
        };
        self.world_dialog = Some(match rename {
            None => WorldDialog {
                rename: None,
                slug: fresh_world_slug(doc),
                name: String::new(),
                error: None,
                just_opened: true,
            },
            Some(w) => {
                let w = w
                    .unwrap_or_else(|| doc.project.worlds[self.st.view.world_ix(doc)].slug.clone());
                let Some(world) = doc.project.world(w.as_str()) else {
                    self.st.toast(format!("No world {w}"), true);
                    return;
                };
                WorldDialog {
                    rename: Some(w.clone()),
                    slug: w.to_string(),
                    name: world.name.clone().unwrap_or_default(),
                    error: None,
                    just_opened: true,
                }
            }
        });
    }

    pub(super) fn world_dialog(&mut self, ctx: &egui::Context) {
        let Some(d) = &mut self.world_dialog else {
            return;
        };
        let Some(doc) = &self.st.doc else {
            self.world_dialog = None;
            return;
        };
        let (enter, escape) = self.modal_keys.for_modal(Modal::World);
        let mut ok = enter;
        let mut cancel = escape;
        let title = match &d.rename {
            None => "New world".to_string(),
            Some(w) => format!("Rename world {w}"),
        };
        egui::Window::new(title)
            .id(egui::Id::new("world dialog"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_TOP, [0.0, 64.0])
            .show(ctx, |ui| {
                egui::Grid::new("world dialog grid")
                    .num_columns(2)
                    .show(ui, |ui| {
                        ui.label("Slug");
                        let r = ui.text_edit_singleline(&mut d.slug);
                        if d.just_opened {
                            r.request_focus();
                            d.just_opened = false;
                        }
                        if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            ok = true;
                        }
                        ui.end_row();
                        ui.label("Name");
                        let r =
                            ui.add(egui::TextEdit::singleline(&mut d.name).hint_text("(optional)"));
                        if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            ok = true;
                        }
                        ui.end_row();
                    });
                ui.weak(match &d.rename {
                    None => "An empty map of its own; layers, tilesets and types are shared.",
                    Some(_) => "A new slug moves worlds/<slug>/ on the next save.",
                });
                if let Some(e) = &d.error {
                    ui.colored_label(Color32::from_rgb(230, 120, 120), e);
                }
                ui.horizontal(|ui| {
                    let label = if d.rename.is_some() {
                        "Rename"
                    } else {
                        "Create"
                    };
                    ok |= ui.button(label).clicked();
                    cancel |= ui.button("Cancel").clicked();
                });
            });
        if cancel {
            self.world_dialog = None;
            return;
        }
        if !ok {
            return;
        }
        let slug = match Slug::new(d.slug.trim()) {
            Ok(s) => s,
            Err(e) => {
                d.error = Some(e.to_string());
                return;
            }
        };
        let taken = doc.project.world(slug.as_str()).is_some();
        let name = d.name.trim().to_string();
        let cmd = match &d.rename {
            None if taken => {
                d.error = Some(format!("A world {slug:?} already exists"));
                return;
            }
            None => Command::CreateWorld {
                slug: slug.clone(),
                name: (!name.is_empty()).then_some(name),
            },
            Some(w) if *w != slug && taken => {
                d.error = Some(format!("A world {slug:?} already exists"));
                return;
            }
            Some(w) => Command::RenameWorld {
                world: w.clone(),
                slug: (*w != slug).then(|| slug.clone()),
                name: Some(name),
            },
        };
        let new = d.rename.is_none();
        match self.apply_command(cmd) {
            Ok(()) => {
                self.world_dialog = None;
                if new {
                    self.switch_world(&slug);
                }
            }
            Err(e) => {
                if let Some(d) = &mut self.world_dialog {
                    d.error = Some(e);
                }
            }
        }
    }

    /// Ask before deleting world `w` (`None`: the world shown); an empty
    /// world goes right away (undo brings it back).
    pub(super) fn ask_delete_world(&mut self, w: Option<Slug>) {
        let Some(doc) = &self.st.doc else {
            return;
        };
        if doc.project.worlds.len() < 2 {
            self.st
                .toast("This is the only world; a project keeps at least one", true);
            return;
        }
        let wi = match &w {
            Some(s) => doc.project.worlds.iter().position(|x| x.slug == *s),
            None => Some(self.st.view.world_ix(doc)),
        };
        let Some(world) = wi.and_then(|i| doc.project.worlds.get(i)) else {
            return;
        };
        let (levels, notes) = (world.levels.len(), world.notes.len());
        let mut parts = Vec::new();
        if levels > 0 {
            parts.push(format!(
                "{levels} level{}",
                if levels == 1 { "" } else { "s" }
            ));
        }
        if notes > 0 {
            parts.push(format!(
                "{notes} world note{}",
                if notes == 1 { "" } else { "s" }
            ));
        }
        let summary = if parts.is_empty() {
            String::new()
        } else {
            format!(" and its {}", parts.join(" and "))
        };
        self.confirm = Some(Confirm::RemoveWorld {
            world: world.slug.clone(),
            summary,
        });
    }

    /// Move a level to another world (same position) and follow it there.
    pub(super) fn move_level_to_world(&mut self, level: Slug, world: Slug) {
        let cmd = Command::MoveLevelToWorld {
            level: level.clone(),
            world,
            world_px: None,
        };
        if let Err(e) = self.apply_command(cmd) {
            self.st.toast(e, true);
            return;
        }
        if let Some(doc) = &self.st.doc
            && let Some(ix) = doc.level_ix(level.as_str())
        {
            self.st.view.active_level = Some(ix);
            self.st.view.sync_world(doc);
            self.st.viewport.request_fit(Fit::Level(ix));
        }
    }
}

/// The Project pane's Worlds section: the worlds in display order with
/// their level counts; click one to show it; ⏶ ⏷ reorder
/// (`reorder_worlds`), Rename…, × (asks first); + World.
pub fn section(
    ui: &mut egui::Ui,
    doc: &Document,
    view: &ViewState,
    ps: &mut ProjectState,
    actions: &mut Vec<Action>,
) {
    ui.horizontal(|ui| {
        let r = ui.button("+ World");
        ps.rects.insert("add world".into(), r.rect);
        if r.clicked() {
            actions.push(Action::NewWorld);
        }
        ui.weak("each is its own map: worlds/<slug>/");
    });
    ui.add_space(4.0);
    let shown = view.world_ix(doc);
    let n = doc.project.worlds.len();
    let order: Vec<Slug> = doc.project.worlds.iter().map(|w| w.slug.clone()).collect();
    egui::Grid::new("project worlds")
        .num_columns(3)
        .striped(true)
        .show(ui, |ui| {
            for (i, w) in doc.project.worlds.iter().enumerate() {
                let levels = super::worlds::world_levels(doc, i);
                let r = ui
                    .selectable_label(i == shown, RichText::new(world_label(w)))
                    .on_hover_text(format!("worlds/{}/: click to show it", w.slug));
                ps.rects.insert(format!("world:{}", w.slug), r.rect);
                if r.clicked() {
                    actions.push(Action::SwitchWorld(w.slug.clone()));
                }
                ui.weak(format!(
                    "{levels} level{}",
                    if levels == 1 { "" } else { "s" }
                ));
                ui.horizontal(|ui| {
                    let mv = |to: usize| {
                        let mut o = order.clone();
                        let s = o.remove(i);
                        o.insert(to, s);
                        Command::ReorderWorlds {
                            order: o,
                            restore: false,
                        }
                    };
                    let up = ui
                        .add_enabled(i > 0, egui::Button::new("⏶").small())
                        .on_hover_text("Earlier in the world list");
                    ps.rects.insert(format!("world:{}:up", w.slug), up.rect);
                    if up.clicked() {
                        ps.pending.push(mv(i - 1));
                    }
                    let down = ui
                        .add_enabled(i + 1 < n, egui::Button::new("⏷").small())
                        .on_hover_text("Later in the world list");
                    ps.rects.insert(format!("world:{}:down", w.slug), down.rect);
                    if down.clicked() {
                        ps.pending.push(mv(i + 1));
                    }
                    let rn = ui.small_button("Rename…");
                    ps.rects.insert(format!("world:{}:rename", w.slug), rn.rect);
                    if rn.clicked() {
                        actions.push(Action::RenameWorld(Some(w.slug.clone())));
                    }
                    let del = ui
                        .add_enabled(n > 1, egui::Button::new("×").small())
                        .on_hover_text("Delete this world (asks first)")
                        .on_disabled_hover_text("A project keeps at least one world");
                    ps.rects
                        .insert(format!("world:{}:delete", w.slug), del.rect);
                    if del.clicked() {
                        actions.push(Action::DeleteWorld(Some(w.slug.clone())));
                    }
                });
                ui.end_row();
            }
        });
}

#[cfg(test)]
mod tests {
    use worlds_core::Command;

    use super::*;
    use crate::app::project_pane::Section;
    use crate::app::testkit::{Driver, state};
    use crate::viewport::scene::Scene;

    fn s(x: &str) -> Slug {
        Slug::new(x).unwrap()
    }

    /// The mini fixture with a second world `dungeon` holding `shaft` and
    /// a world note of its own.
    fn two_worlds() -> (tempfile::TempDir, super::super::EditorState) {
        let (tmp, mut st) = state();
        let doc = st.doc.as_mut().unwrap();
        for c in [
            Command::CreateWorld {
                slug: s("dungeon"),
                name: Some("Dungeon".into()),
            },
            Command::MoveLevelToWorld {
                level: s("shaft"),
                world: s("dungeon"),
                world_px: Some([0, 0]),
            },
            Command::AddNote {
                scope: worlds_core::command::NoteScope::World(s("dungeon")),
                note: worlds_core::Annotation::new(worlds_core::Shape::Note { px: [8, 8] }),
                index: None,
            },
        ] {
            doc.project.apply(c).unwrap();
        }
        doc.reindex();
        st.view.active_level = doc.level_ix("crevasse");
        (tmp, st)
    }

    #[test]
    fn the_active_level_decides_the_world_then_the_stored_one() {
        let (_tmp, mut st) = two_worlds();
        let doc = st.doc.as_ref().unwrap();
        assert_eq!(st.view.world_ix(doc), 0);
        // Showing the dungeon drops the active level (it is in main).
        st.view.set_world(doc, 1);
        assert_eq!(st.view.active_level, None);
        assert_eq!(st.view.world_ix(doc), 1);
        assert_eq!(st.view.active_world, Some(s("dungeon")));
        // A level made active elsewhere brings its world along.
        st.view.active_level = doc.level_ix("crevasse");
        assert_eq!(st.view.world_ix(doc), 0);
        st.view.sync_world(doc);
        assert_eq!(st.view.active_world, Some(s("main")));
        // An unknown stored world falls back to the first.
        st.view.active_level = None;
        st.view.active_world = Some(s("gone"));
        assert_eq!(st.view.world_ix(doc), 0);
    }

    #[test]
    fn the_map_draws_and_hits_only_the_world_shown() {
        let (_tmp, mut st) = two_worlds();
        let doc = st.doc.as_ref().unwrap();
        let crevasse = doc.level_ix("crevasse").unwrap();
        let shaft = doc.level_ix("shaft").unwrap();
        let mut scene = Scene::new(doc);
        scene.set_world(doc, 0);
        assert_eq!(scene.draw_order, [crevasse]);
        scene.set_world(doc, 1);
        assert_eq!(scene.draw_order, [shaft]);
        // Both levels sit at [0, 0]: what is under the pointer depends on
        // the world.
        let wp = [4.0, 4.0];
        assert_eq!(crate::viewport::level_under_in(doc, 0, wp), Some(crevasse));
        assert_eq!(crate::viewport::level_under_in(doc, 1, wp), Some(shaft));
        // Annotations: main's, or the dungeon's one world note.
        let main = crate::annot::shown_notes(doc, &st.view);
        assert!(!main.is_empty());
        assert!(
            main.iter()
                .all(|r| crate::annot::note_world(doc, r.at) == 0)
        );
        st.view.set_world(doc, 1);
        let shown = crate::annot::shown_notes(doc, &st.view);
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].at, crate::annot::At::World(1));
        // Fit world frames the world shown.
        let r = crate::viewport::world_wrect(doc, 1);
        assert_eq!(r, crate::viewport::level_wrect(doc, shaft));
    }

    #[test]
    fn project_pane_lists_switches_and_reorders_worlds() {
        let (_tmp, mut st) = two_worlds();
        st.project.show(Section::Worlds);
        let mut d = Driver::new(crate::app::project_pane::pane);
        d.frame(&mut st, vec![]);
        let r = st.project.rects["world:dungeon"];
        d.click(&mut st, r.center());
        assert_eq!(st.actions, [Action::SwitchWorld(s("dungeon"))]);
        st.actions.clear();
        d.frame(&mut st, vec![]);
        let up = st.project.rects["world:dungeon:up"];
        d.click(&mut st, up.center());
        assert_eq!(
            st.project.pending,
            [Command::ReorderWorlds {
                order: vec![s("dungeon"), s("main")],
                restore: false
            }]
        );
        st.project.pending.clear();
        d.frame(&mut st, vec![]);
        let x = st.project.rects["world:main:delete"];
        d.click(&mut st, x.center());
        assert_eq!(st.actions, [Action::DeleteWorld(Some(s("main")))]);
    }

    /// A connector of main whose far end names a level of the dungeon:
    /// that end is left off main's map (validation reports it), and with
    /// the dungeon shown the connector gets no handles.
    #[test]
    fn connectors_stay_on_their_world() {
        use worlds_core::{Annotation, ConnectorStyle, Endpoint, Shape};
        let (_tmp, mut st) = two_worlds();
        let doc = st.doc.as_mut().unwrap();
        let mut n = Annotation::new(Shape::Connector {
            from: Endpoint::Level(s("crevasse")),
            to: Endpoint::Level(s("shaft")),
            waypoints: vec![[40, 40]],
            label: None,
            style: ConnectorStyle::default(),
        });
        let id = n.id.clone();
        n.title = Some("across".into());
        doc.project
            .apply(Command::AddNote {
                scope: worlds_core::command::NoteScope::World(s("main")),
                note: n,
                index: None,
            })
            .unwrap();
        doc.reindex();
        let doc = st.doc.as_ref().unwrap();
        let r = crate::annot::find_note(doc, &id).unwrap();
        // The crevasse end and the waypoint; no end in the dungeon.
        assert_eq!(crate::annot::connector_line(doc, r).len(), 2);
        st.view
            .selection
            .set([crate::view::SelItem::Note(id.clone())]);
        assert!(crate::annot::edited_note(doc, &st.view).is_some());
        st.view.set_world(doc, 1);
        st.view
            .selection
            .set([crate::view::SelItem::Note(id.clone())]);
        assert!(crate::annot::edited_note(doc, &st.view).is_none());
        assert!(crate::annot::handle_at(doc, &st.view, [40.0, 40.0], 1.0).is_none());
    }
}
