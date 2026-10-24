//! Map tabs and camera bookmarks.
//!
//! Any number of map tabs (`Pane::Viewport(id)`) dock like any pane;
//! dragging one out of its tab bar splits the view. Each has its own
//! camera, world and active level ([`crate::view::ViewportTab`]); tool,
//! brush, layers and selection are shared. A press in a tab focuses it
//! before it draws, so keys and single-key tools always act in the tab
//! last clicked. "New viewport", "Duplicate viewport", "Link cameras" and
//! closing are in the command palette and the tab's context menu; the last
//! tab can't close.
//!
//! The scene (the CPU chunk cache) and the GPU chunk buffers are shared:
//! a second tab costs its draw calls, nothing is built twice.
//!
//! Bookmarks save the focused tab's world and camera (center and zoom)
//! under a name, in `ui.json`; the toolbar's ★ opens the list
//! ([`bookmarks_list`], a widget other panes can embed), the command
//! palette lists them, Ctrl+Shift+1..9 jump to the first nine.

use egui::RichText;
use egui_dock::DockState;
use worlds_core::Slug;

use super::actions::Action;
use super::{EditorApp, EditorState, Pane, show_pane};
use crate::view::{ViewportId, swap_live};
use crate::viewport::Fit;
use crate::viewport::camera::Camera;

/// The map tabs in the dock, in dock order.
pub fn dock_viewports(dock: &DockState<Pane>) -> Vec<ViewportId> {
    dock.iter_all_tabs()
        .filter_map(|(_, t)| match t {
            Pane::Viewport(id) => Some(*id),
            _ => None,
        })
        .collect()
}

/// Put tab `id` in the dock: a tab in the leaf of map tab `next_to`, or
/// with `split` a split to the right of it (main surface; elsewhere a tab).
/// No map tab to go next to: the first leaf.
pub fn dock_tab(dock: &mut DockState<Pane>, id: ViewportId, next_to: ViewportId, split: bool) {
    let pane = Pane::Viewport(id);
    if dock.find_tab(&pane).is_some() {
        return;
    }
    if split && let Some((node, _)) = dock.find_main_surface_tab(&Pane::Viewport(next_to)) {
        dock.main_surface_mut().split_right(node, 0.5, vec![pane]);
        return;
    }
    if let Some(p) = dock.find_tab(&Pane::Viewport(next_to))
        && let Ok(leaf) = dock.leaf_mut(p.node_path())
    {
        leaf.append_tab(pane);
        show_pane(dock, pane);
        return;
    }
    dock.push_to_first_leaf(pane);
}

/// The title of map tab `id`: "Map" while it is the only one, else
/// "Map <id>", with its world when the project has several.
pub fn tab_title(st: &EditorState, id: ViewportId) -> String {
    if st.view.tabs.len() <= 1 {
        return "Map".into();
    }
    match &st.doc {
        Some(doc) if doc.project.worlds.len() > 1 => {
            let w = &doc.project.worlds[st.view.tab_world_ix(doc, id)];
            format!("Map {id} · {}", super::worlds::world_label(w))
        }
        _ => format!("Map {id}"),
    }
}

/// The world center of a camera in a viewport of `size` physical px.
fn center(c: &Camera, size: [f64; 2]) -> [f64; 2] {
    c.center(size)
}

impl EditorState {
    /// Make tab `id` the live one ([`swap_live`]).
    pub fn swap_live(&mut self, id: ViewportId) {
        swap_live(&mut self.view, &mut self.viewport, id);
    }

    /// Focus tab `id` (it becomes live). False if there is no such tab.
    pub fn focus_tab(&mut self, id: ViewportId) -> bool {
        self.view.focus_tab(&mut self.viewport, id)
    }

    /// Run `f` with tab `id` live (as the focused tab is normally), then
    /// make the focused tab live again.
    pub fn in_tab<R>(&mut self, id: ViewportId, f: impl FnOnce(&mut EditorState) -> R) -> R {
        self.swap_live(id);
        let r = f(self);
        let focused = self.view.focused;
        self.swap_live(focused);
        r
    }

    /// The hover readout of the map tab under the pointer: the live tab's,
    /// else a parked one's that has it (status bar).
    pub fn pointer_hover(
        &self,
    ) -> (
        Option<&crate::viewport::HoverInfo>,
        Option<&String>,
        crate::viewport::ViewStats,
        f64,
    ) {
        let vp = &self.viewport;
        if vp.hover.is_none()
            && let Some(t) = self
                .view
                .tabs
                .iter()
                .find(|t| t.id != self.view.live && t.cam.hover.is_some())
        {
            return (
                t.cam.hover.as_ref(),
                t.cam.readout.as_ref(),
                t.cam.stats,
                t.cam.camera.zoom,
            );
        }
        (
            vp.hover.as_ref(),
            vp.readout.as_ref(),
            vp.stats,
            vp.camera.zoom,
        )
    }
}

/// The bookmark list: each bookmark jumps on a click (its world and key
/// beside it), × forgets it; a name field and Save view add the focused
/// tab's view. Reusable: the toolbar's ★ window shows it, a Levels pane can
/// embed it.
pub fn bookmarks_list(ui: &mut egui::Ui, st: &mut EditorState) {
    if st.view.bookmarks.is_empty() {
        ui.weak("No bookmarks yet: name this view and save it.");
    }
    let mut act = None;
    egui::Grid::new("bookmarks")
        .num_columns(3)
        .striped(true)
        .show(ui, |ui| {
            for (i, b) in st.view.bookmarks.iter().enumerate() {
                let key = super::actions::bookmark_key(i);
                let r = ui
                    .button(&b.name)
                    .on_hover_text(format!("Show {} in the focused map tab", b.name));
                super::fields::mark(ui, &format!("bookmark:{}", b.name), r.rect);
                if r.clicked() {
                    act = Some(Action::JumpBookmark(i));
                }
                ui.weak(if key.is_empty() {
                    b.world.clone()
                } else {
                    format!("{} · {key}", b.world)
                });
                if ui
                    .small_button("×")
                    .on_hover_text("Forget this bookmark")
                    .clicked()
                {
                    act = Some(Action::DeleteBookmark(i));
                }
                ui.end_row();
            }
        });
    ui.horizontal(|ui| {
        let r = ui.add(
            egui::TextEdit::singleline(&mut st.bookmark_name)
                .hint_text("Name")
                .desired_width(140.0),
        );
        if std::mem::take(&mut st.bookmark_focus) {
            r.request_focus();
        }
        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if ui
            .button("Save view")
            .on_hover_text("Bookmark the focused map tab's world and camera")
            .clicked()
            || enter
        {
            let name = std::mem::take(&mut st.bookmark_name);
            act = Some(Action::SaveBookmarkAs(name));
        }
    });
    if let Some(a) = act {
        st.actions.push(a);
    }
}

impl EditorApp {
    /// Keep the dock and the tab list in step: a tab the API opened gets a
    /// dock tab, a dock tab without an entry (an edited `ui.json`) gets
    /// one; the focused tab comes to the front of its tab group when the
    /// focus moved.
    pub(super) fn sync_viewport_tabs(&mut self) {
        let docked = dock_viewports(&self.dock);
        let focused = self.st.view.focused;
        let missing: Vec<ViewportId> = self
            .st
            .view
            .tabs
            .iter()
            .map(|t| t.id)
            .filter(|id| !docked.contains(id))
            .collect();
        for id in missing {
            dock_tab(&mut self.dock, id, focused, false);
        }
        if let Some(doc) = &self.st.doc {
            for id in docked {
                if !self.st.view.has_tab(id) {
                    let mut t = crate::view::ViewportTab::new(id);
                    t.cam.request_fit(Fit::World);
                    t.active_world = doc.project.worlds.first().map(|w| w.slug.clone());
                    self.st.view.tabs.push(t);
                }
            }
        }
        if self.shown_focus != Some(focused) {
            self.shown_focus = Some(focused);
            show_pane(&mut self.dock, Pane::Viewport(focused));
        }
    }

    /// Open a map tab on world `world` (`None`: the focused tab's), or a
    /// copy of tab `copy_of`, next to the focused tab (`split`: beside it),
    /// and focus it.
    pub(super) fn open_viewport(
        &mut self,
        world: Option<Slug>,
        copy_of: Option<ViewportId>,
        split: bool,
    ) -> Option<ViewportId> {
        let doc = self.st.doc.as_ref()?;
        let next_to = self.st.view.focused;
        let id = self.st.view.new_tab(doc, &self.st.viewport, world, copy_of);
        dock_tab(&mut self.dock, id, next_to, split);
        self.st.focus_tab(id);
        Some(id)
    }

    /// The `+` at the end of a tab bar: a new map tab in the leaf at
    /// `path`. Next to a map tab it is that tab's world (and that tab is
    /// focused first, so the new one lands beside it); on a pane's tab bar
    /// (Layers, Inspector, ...) it is the focused tab's world, appended
    /// there. Focused either way.
    pub(super) fn open_viewport_at(&mut self, path: egui_dock::NodePath) -> Option<ViewportId> {
        let here = self
            .dock
            .leaf(path)
            .ok()?
            .tabs()
            .iter()
            .find_map(|t| match t {
                Pane::Viewport(id) => Some(*id),
                _ => None,
            });
        if let Some(id) = here {
            self.st.focus_tab(id);
            return self.open_viewport(None, None, false);
        }
        let doc = self.st.doc.as_ref()?;
        let id = self.st.view.new_tab(doc, &self.st.viewport, None, None);
        let pane = Pane::Viewport(id);
        match self.dock.leaf_mut(path) {
            Ok(leaf) => {
                leaf.append_tab(pane);
                show_pane(&mut self.dock, pane);
            }
            Err(_) => dock_tab(&mut self.dock, id, self.st.view.focused, false),
        }
        self.st.focus_tab(id);
        Some(id)
    }

    /// Close map tab `id` (`None`: the focused one), unless it is the last.
    pub(super) fn close_viewport(&mut self, id: Option<ViewportId>) -> Result<(), String> {
        let id = id.unwrap_or(self.st.view.focused);
        self.st.view.close_tab(&mut self.st.viewport, id)?;
        if let Some(p) = self.dock.find_tab(&Pane::Viewport(id)) {
            self.dock.remove_tab(p);
        }
        Ok(())
    }

    /// Link `pair`'s cameras (`None`: the focused tab and the next one in
    /// the order opened); the same pair again unlinks. The second takes
    /// the first's zoom, keeping its own center.
    pub(super) fn link_cameras(&mut self, pair: Option<[ViewportId; 2]>) -> Result<(), String> {
        let pair = match pair {
            Some(p) => p,
            None => {
                let tabs = &self.st.view.tabs;
                let f = self.st.view.focused;
                let i = tabs.iter().position(|t| t.id == f).unwrap_or(0);
                let Some(other) = tabs
                    .iter()
                    .cycle()
                    .skip(i + 1)
                    .take(tabs.len())
                    .find(|t| t.id != f)
                else {
                    return Err("Link cameras needs a second map tab".into());
                };
                [f, other.id]
            }
        };
        let [a, b] = pair;
        if a == b {
            return Err("Link two different map tabs".into());
        }
        for id in pair {
            if !self.st.view.has_tab(id) {
                return Err(format!("There is no map tab {id}"));
            }
        }
        let same = |l: [ViewportId; 2]| l == [a, b] || l == [b, a];
        if self.st.view.link.is_some_and(same) {
            self.st.view.link = None;
            self.link_prev = None;
            self.st.view.set_status("Cameras unlinked", false);
            return Ok(());
        }
        self.st.view.link = Some(pair);
        let (ca, _) = self.st.view.tab_camera(&self.st.viewport, a).expect("tab");
        let (cb, sb) = self.st.view.tab_camera(&self.st.viewport, b).expect("tab");
        let nb = Camera::centered(center(&cb, sb), ca.zoom, sb);
        self.st.view.set_tab_camera(&mut self.st.viewport, b, nb);
        self.link_prev = None;
        self.st
            .view
            .set_status(format!("Cameras of maps {a} and {b} linked"), false);
        Ok(())
    }

    /// After the dock drew: a linked pair follows whichever camera moved
    /// (same zoom, the offset between their centers kept).
    pub(super) fn sync_linked_cameras(&mut self) {
        let Some([a, b]) = self.st.view.link else {
            self.link_prev = None;
            return;
        };
        let view = &self.st.view;
        let (Some((ca, sa)), Some((cb, sb))) = (
            view.tab_camera(&self.st.viewport, a),
            view.tab_camera(&self.st.viewport, b),
        ) else {
            self.st.view.link = None;
            self.link_prev = None;
            return;
        };
        if let Some([pa, pb]) = self.link_prev {
            let follow = |lead: Camera, old_lead: Camera, old: Camera, sl, so| {
                let offset = {
                    let (c0, c1) = (center(&old_lead, sl), center(&old, so));
                    [c1[0] - c0[0], c1[1] - c0[1]]
                };
                let c = center(&lead, sl);
                Camera::centered([c[0] + offset[0], c[1] + offset[1]], lead.zoom, so)
            };
            if ca != pa {
                let nb = follow(ca, pa, pb, sa, sb);
                self.st.view.set_tab_camera(&mut self.st.viewport, b, nb);
                self.link_prev = Some([ca, nb]);
                return;
            }
            if cb != pb {
                let na = follow(cb, pb, pa, sb, sa);
                self.st.view.set_tab_camera(&mut self.st.viewport, a, na);
                self.link_prev = Some([na, cb]);
                return;
            }
        }
        self.link_prev = Some([ca, cb]);
    }

    /// Show bookmark `i` in the focused tab: its world, its center at its
    /// zoom.
    pub(super) fn jump_bookmark(&mut self, i: usize) -> Result<(), String> {
        let Some(doc) = &self.st.doc else {
            return Err("no project open".into());
        };
        let Some(b) = self.st.view.bookmarks.get(i).cloned() else {
            return Err(format!("There is no bookmark {}", i + 1));
        };
        let Some(w) = doc
            .project
            .worlds
            .iter()
            .position(|w| w.slug == b.world.as_str())
        else {
            return Err(format!(
                "Bookmark {}: the world {} is gone",
                b.name, b.world
            ));
        };
        self.st.view.set_world(doc, w);
        self.st.viewport.request_fit(Fit::Center {
            center: b.camera.center,
            zoom: b.camera.zoom,
        });
        self.st
            .view
            .set_status(format!("Bookmark {}", b.name), false);
        Ok(())
    }

    /// Bookmark the focused tab's view as `name` (a fresh "View n" when
    /// empty).
    pub(super) fn save_bookmark(&mut self, name: &str) -> Result<(), String> {
        let Some(doc) = &self.st.doc else {
            return Err("no project open".into());
        };
        let name = if name.trim().is_empty() {
            let mut n = self.st.view.bookmarks.len() + 1;
            while self.st.view.bookmark_ix(&format!("View {n}")).is_some() {
                n += 1;
            }
            format!("View {n}")
        } else {
            name.trim().to_string()
        };
        let i = self.st.view.save_bookmark(doc, &self.st.viewport, &name)?;
        let key = super::actions::bookmark_key(i);
        self.st.view.set_status(
            if key.is_empty() {
                format!("Bookmarked {name}")
            } else {
                format!("Bookmarked {name} ({key})")
            },
            false,
        );
        Ok(())
    }

    /// The toolbar's ★: the bookmark list in a small window under it.
    pub(super) fn bookmarks_button(&mut self, ui: &mut egui::Ui) {
        if self.st.doc.is_none() {
            return;
        }
        let r = ui
            .add(egui::Button::selectable(self.st.bookmarks_open, "★"))
            .on_hover_text("Camera bookmarks (Ctrl+Shift+1..9)");
        super::fields::mark(ui, "bookmarks button", r.rect);
        if r.clicked() {
            self.st.bookmarks_open = !self.st.bookmarks_open;
        }
        self.bookmarks_anchor = Some(r.rect);
    }

    pub(super) fn bookmarks_window(&mut self, ctx: &egui::Context) {
        if !self.st.bookmarks_open || self.st.doc.is_none() {
            return;
        }
        let at = self.bookmarks_anchor.map_or(egui::pos2(200.0, 40.0), |r| {
            r.left_bottom() + egui::vec2(0.0, 4.0)
        });
        let mut open = true;
        egui::Window::new(RichText::new("Bookmarks"))
            .id(egui::Id::new("bookmarks window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .fixed_pos(at)
            .show(ctx, |ui| bookmarks_list(ui, &mut self.st));
        if !open {
            self.st.bookmarks_open = false;
        }
    }

    /// `{ "ui": "viewport", "open"?: { "world"? }, "focus"?, "close"?,
    /// "link"? }`, in that order: open a tab beside the focused one (it
    /// becomes focused), close one, focus one, link two (`[]` unlinks).
    pub(super) fn viewport_step(&mut self, v: &crate::script::ViewportStep) -> Result<(), String> {
        let doc = self.st.doc.as_ref().ok_or("no project open")?;
        if let Some(o) = &v.open {
            let world = match &o.world {
                Some(w) => Some(
                    doc.project
                        .world(w)
                        .map(|x| x.slug.clone())
                        .ok_or_else(|| format!("no world {w:?}"))?,
                ),
                None => None,
            };
            self.open_viewport(world, None, true);
        }
        if let Some(id) = v.close {
            self.close_viewport(Some(ViewportId(id)))?;
        }
        if let Some(id) = v.focus
            && !self.st.focus_tab(ViewportId(id))
        {
            return Err(format!("no viewport {id}"));
        }
        if let Some(l) = &v.link {
            match l.as_slice() {
                [] => {
                    self.st.view.link = None;
                    self.link_prev = None;
                }
                [a, b] => {
                    let pair = [ViewportId(*a), ViewportId(*b)];
                    if self.st.view.link != Some(pair) {
                        self.link_cameras(Some(pair))?;
                    }
                }
                _ => return Err("ui \"viewport\": link takes two ids, or [] to unlink".into()),
            }
        }
        Ok(())
    }

    /// `{ "ui": "bookmark", "save"?: name, "jump"?: name }`: save the
    /// focused tab's view, then jump to one.
    pub(super) fn bookmark_step(&mut self, b: &crate::script::BookmarkStep) -> Result<(), String> {
        if let Some(name) = &b.save {
            if name.trim().is_empty() {
                return Err("ui \"bookmark\": save needs a name".into());
            }
            self.save_bookmark(name)?;
        }
        if let Some(name) = &b.jump {
            let i = self
                .st
                .view
                .bookmark_ix(name)
                .ok_or_else(|| format!("no bookmark {name:?}"))?;
            self.jump_bookmark(i)?;
        }
        Ok(())
    }
}
