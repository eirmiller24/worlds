//! The Levels pane (`Pane::Levels`, tab "Levels", next to the Layers
//! pane; Ctrl+L or "Levels: show the Levels pane"): the focused map tab's
//! world's levels, or every world's grouped by world, each row a
//! thumbnail, the name and slug, the size in cells, the tags and the
//! problem count. Under the rows, the camera bookmarks (the toolbar ★
//! window's list).
//!
//! - **Search** by name, slug and tag: every word of the box must be in one
//!   of them (`forest boss`).
//! - **Sort** by the level order (the default, `world.json` `levels`), by
//!   name, or by position (top to bottom, then left to right).
//! - **Reorder**: drag a row (in the level order, with no search), or the
//!   menu's Move earlier / Move later. One `reorder_levels`, one undo entry;
//!   PgUp/PgDn and the export follow the order, and a strip layout moves its
//!   levels to their new places in the same entry.
//! - A click makes the level active (and selects it), in the focused map
//!   tab (a level of another world takes that tab there); a double click
//!   also frames it in that tab (the Map's Fit level). Right click: Frame, Rename,
//!   Duplicate, Crop to content, Move earlier/later, Move to world, Delete.
//!
//! Each row's thumbnail comes from the editor's one thumbnail cache
//! ([`crate::thumbs`]): the same texture the zoomed-out map draws, asked for
//! only for the rows on screen (`show_rows`), so the budget there is spent
//! on what is visible.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use egui::{Color32, Sense, Stroke, Vec2};
use worlds_core::{Command, Id, Slug};

use crate::thumbs::{LayerLook, LevelThumb};

use super::EditorState;
use super::actions::Action;
use super::fields::mark;
use crate::doc::{Document, LevelIx};
use crate::view::{SelItem, ViewState};

/// Row height in points.
const ROW_H: f32 = 40.0;

/// How the rows are ordered.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Sort {
    /// The world's level order (`world.json` `levels`).
    #[default]
    Order,
    /// By display name, then slug.
    Name,
    /// Top to bottom, then left to right.
    Position,
}

impl Sort {
    pub const ALL: [Sort; 3] = [Sort::Order, Sort::Name, Sort::Position];

    pub fn label(self) -> &'static str {
        match self {
            Sort::Order => "Order",
            Sort::Name => "Name",
            Sort::Position => "Position",
        }
    }

    /// The script's names: `order`, `name`, `position`.
    pub fn parse(s: &str) -> Option<Sort> {
        match s {
            "order" => Some(Sort::Order),
            "name" => Some(Sort::Name),
            "position" => Some(Sort::Position),
            _ => None,
        }
    }
}

/// One row: a world header or a level.
#[derive(Debug, Clone, PartialEq)]
pub enum Row {
    World { world: usize, levels: usize },
    Level(LevelIx),
}

/// The rename editor of one row.
#[derive(Debug, Clone, PartialEq)]
pub struct Rename {
    pub level: Id,
    pub name: String,
    pub slug: String,
    pub focus: bool,
}

/// The pane's state.
#[derive(Default)]
pub struct LevelsState {
    pub search: String,
    pub sort: Sort,
    /// Every world, grouped; else the world shown.
    pub all_worlds: bool,
    /// The edit revision (`EditorApp::frame` sets it each frame).
    pub rev: u64,
    pub rename: Option<Rename>,
    /// What went wrong with the last rename.
    pub error: Option<String>,
    /// The level being dragged to a new place.
    pub drag: Option<Id>,
    /// Bring the pane to the front.
    pub focus: bool,
    /// Give the search box the keyboard.
    pub focus_search: bool,
    rows: Arc<Vec<Row>>,
    built: Option<(u64, String, Sort, bool, usize, usize)>,
    /// Row rebuilds so far (tests: bounded work).
    pub builds: usize,
}

impl LevelsState {
    /// The rows as of the last build.
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// Rebuild the rows when the project, the world shown or the filters
    /// changed.
    pub fn refresh(&mut self, doc: &Document, view: &ViewState) {
        let key = (
            self.rev,
            self.search.clone(),
            self.sort,
            self.all_worlds,
            view.world_ix(doc),
            doc.levels.len(),
        );
        if self.built.as_ref() == Some(&key) {
            return;
        }
        self.rows = Arc::new(rows(doc, view, &self.search, self.sort, self.all_worlds));
        self.built = Some(key);
        self.builds += 1;
    }

    /// Whether rows can be dragged to reorder: the level order, unfiltered.
    pub fn can_drag(&self) -> bool {
        self.sort == Sort::Order && self.search.trim().is_empty()
    }
}

/// Whether level `ix` matches every word of `search` (in its name, slug
/// or a tag, ignoring case).
pub fn matches(doc: &Document, ix: LevelIx, search: &str) -> bool {
    let l = doc.level(ix);
    let name = l.name.to_lowercase();
    search.split_whitespace().all(|w| {
        let w = w.to_lowercase();
        let w = w.strip_prefix("tag:").unwrap_or(&w);
        name.contains(w)
            || l.slug.as_str().contains(w)
            || l.tags.iter().any(|t| t.to_lowercase().contains(w))
    })
}

/// The rows: per world (the one shown, or every one with a header) its
/// matching levels in `sort` order.
pub fn rows(doc: &Document, view: &ViewState, search: &str, sort: Sort, all: bool) -> Vec<Row> {
    let shown = view.world_ix(doc);
    let mut out = Vec::new();
    for w in 0..doc.project.worlds.len() {
        if !all && w != shown {
            continue;
        }
        let mut ixs: Vec<LevelIx> = (0..doc.levels.len())
            .filter(|&i| doc.levels[i].world == w && matches(doc, i, search))
            .collect();
        match sort {
            Sort::Order => {}
            Sort::Name => ixs.sort_by_cached_key(|&i| {
                let l = doc.level(i);
                (l.name.to_lowercase(), l.slug.clone())
            }),
            Sort::Position => ixs.sort_by_key(|&i| {
                let l = doc.level(i);
                (l.world_px[1], l.world_px[0], l.slug.clone())
            }),
        }
        if all {
            out.push(Row::World {
                world: w,
                levels: ixs.len(),
            });
        }
        out.extend(ixs.into_iter().map(Row::Level));
    }
    out
}

/// Problems per level slug (those whose file or path is in a level's
/// directory).
pub fn problem_counts(doc: &Document) -> HashMap<String, usize> {
    let mut out = HashMap::new();
    for p in &doc.problems {
        let full = format!("{}/{}", p.file, p.path);
        if let Some(slug) = full
            .split("levels/")
            .nth(1)
            .and_then(|r| r.split('/').next())
        {
            *out.entry(slug.to_string()).or_default() += 1;
        }
    }
    out
}

/// `world`'s level order with `level` moved to position `to` (counted
/// after taking it out), as a `reorder_levels`; `None` when nothing moves.
pub fn move_to(doc: &Document, level: &Slug, to: usize) -> Option<Command> {
    let w = doc.project.world_of_level(level.as_str())?;
    let mut order: Vec<Slug> = w.levels.iter().map(|l| l.slug.clone()).collect();
    let from = order.iter().position(|s| s == level)?;
    let s = order.remove(from);
    let to = to.min(order.len());
    if to == from {
        return None;
    }
    order.insert(to, s);
    Some(Command::ReorderLevels {
        world: w.slug.clone(),
        order,
        restore: false,
    })
}

/// Make level `ix` active and the selection (a click on its row).
pub fn select(view: &mut ViewState, doc: &Document, ix: LevelIx) {
    // A level of another world (All worlds) takes the focused tab there.
    view.set_world(doc, doc.levels[ix].world);
    view.active_level = Some(ix);
    view.selection.set([SelItem::Level(doc.ids[ix].clone())]);
}

/// A level's place in its world's level order.
fn order_index(doc: &Document, ix: LevelIx) -> usize {
    doc.levels[ix].level
}

// ---- the pane ----

/// The camera bookmarks under the levels: the toolbar ★ window's list
/// ([`super::viewports::bookmarks_list`]), folding away. A request to focus
/// the name field (the ★ window opening) is left for the window.
fn bookmarks(ui: &mut egui::Ui, st: &mut EditorState) {
    let n = st.view.bookmarks.len();
    let r = egui::CollapsingHeader::new(format!("Bookmarks ({n})"))
        .id_salt("levels bookmarks")
        .default_open(true)
        .show(ui, |ui| {
            let focus = std::mem::take(&mut st.bookmark_focus);
            super::viewports::bookmarks_list(ui, st);
            st.bookmark_focus |= focus;
        });
    mark(ui, "levels:bookmarks", r.header_response.rect);
}

/// The Levels pane.
pub fn pane(ui: &mut egui::Ui, st: &mut EditorState) {
    if st.doc.is_none() {
        ui.weak("No project");
        return;
    }
    // ---- filter row ----
    ui.horizontal(|ui| {
        let r = ui.add(
            egui::TextEdit::singleline(&mut st.levels.search)
                .hint_text("name, slug or tag")
                .desired_width((ui.available_width() - 180.0).max(80.0)),
        );
        mark(ui, "levels:search", r.rect);
        if std::mem::take(&mut st.levels.focus_search) {
            r.request_focus();
        }
        let c = egui::ComboBox::from_id_salt("levels sort")
            .selected_text(st.levels.sort.label())
            .width(76.0)
            .show_ui(ui, |ui| {
                for s in Sort::ALL {
                    ui.selectable_value(&mut st.levels.sort, s, s.label());
                }
            });
        mark(ui, "levels:sort", c.response.rect);
        let r = ui
            .checkbox(&mut st.levels.all_worlds, "All worlds")
            .on_hover_text("Every world's levels, grouped by world");
        mark(ui, "levels:all_worlds", r.rect);
    });
    // The bookmarks take the bottom; the rows scroll in the rest.
    egui::Panel::bottom("levels bookmarks")
        .resizable(false)
        .show(ui, |ui| bookmarks(ui, st));
    let EditorState {
        doc,
        view,
        levels: ls,
        thumbs,
        pending,
        actions,
        ..
    } = st;
    let doc = doc.as_ref().expect("checked");
    ls.refresh(doc, view);
    let rows = Arc::clone(&ls.rows);
    if let Some(e) = &ls.error {
        ui.colored_label(Color32::from_rgb(230, 110, 100), e);
    }
    if rows.iter().all(|r| matches!(r, Row::World { .. })) {
        ui.weak(if ls.search.trim().is_empty() {
            "No levels in this world yet: New level… in the map's right-click menu."
        } else {
            "No level matches."
        });
    }
    let problems = problem_counts(doc);
    let ctx = ui.ctx().clone();
    let look = LayerLook::of(doc, view);
    let can_drag = ls.can_drag();
    let mut drawn: Vec<(LevelIx, egui::Rect)> = Vec::new();
    let mut clicked: Option<LevelIx> = None;
    let mut framed: Option<LevelIx> = None;
    let mut menu: Option<(LevelIx, MenuPick)> = None;
    let mut commit_rename = false;
    let mut cancel_rename = false;
    let selected: HashSet<Id> = view.selection.levels().cloned().collect();
    let accent = ui.visuals().selection.bg_fill;
    egui::ScrollArea::vertical()
        .id_salt("levels rows")
        .auto_shrink([false, false])
        .show_rows(ui, ROW_H, rows.len(), |ui, range| {
            for i in range {
                match &rows[i] {
                    Row::World { world, levels } => {
                        let w = &doc.project.worlds[*world];
                        let (rect, _) = ui.allocate_exact_size(
                            Vec2::new(ui.available_width(), ROW_H),
                            Sense::hover(),
                        );
                        ui.painter().text(
                            rect.left_bottom() + Vec2::new(2.0, -6.0),
                            egui::Align2::LEFT_BOTTOM,
                            format!(
                                "{}  ({levels} level{})",
                                super::worlds::world_label(w),
                                if *levels == 1 { "" } else { "s" }
                            ),
                            egui::FontId::proportional(14.0),
                            ui.visuals().strong_text_color(),
                        );
                        mark(ui, &format!("levels:world:{}", w.slug), rect);
                    }
                    Row::Level(ix) => {
                        let ix = *ix;
                        let l = doc.level(ix);
                        let renaming = ls.rename.as_ref().filter(|r| r.level == l.id);
                        if let Some(r) = renaming {
                            let mut r = r.clone();
                            let (ok, cancel) = rename_row(ui, &mut r);
                            commit_rename |= ok;
                            cancel_rename |= cancel;
                            ls.rename = Some(r);
                            continue;
                        }
                        let (rect, resp) = ui.allocate_exact_size(
                            Vec2::new(ui.available_width(), ROW_H),
                            if can_drag {
                                Sense::click_and_drag()
                            } else {
                                Sense::click()
                            },
                        );
                        mark(ui, &format!("levels:row:{}", l.slug), rect);
                        drawn.push((ix, rect));
                        let on = view.active_level == Some(ix) || selected.contains(&l.id);
                        let p = ui.painter();
                        if on {
                            p.rect_filled(rect, 3.0, accent.gamma_multiply(0.6));
                        } else if resp.hovered() {
                            p.rect_filled(rect, 3.0, ui.visuals().widgets.hovered.bg_fill);
                        }
                        // Thumbnail.
                        let thumb = egui::Rect::from_min_size(
                            rect.min + Vec2::new(2.0, 2.0),
                            Vec2::new(ROW_H * 1.5, ROW_H - 4.0),
                        );
                        let src = LevelThumb {
                            doc,
                            ix,
                            look: &look,
                        };
                        if let Some(tex) = thumbs.get(&ctx, &src) {
                            let r = crate::thumbs::draw_fitted(p, &tex, thumb);
                            p.rect_stroke(
                                r,
                                0.0,
                                Stroke::new(1.0, Color32::from_gray(90)),
                                egui::StrokeKind::Outside,
                            );
                        } else {
                            p.rect_filled(thumb, 0.0, Color32::from_gray(50));
                        }
                        // Name, slug, size and tags; problems on the right.
                        let x = thumb.right() + 8.0;
                        let name = if l.name.is_empty() {
                            l.slug.as_str()
                        } else {
                            l.name.as_str()
                        };
                        let text = ui.visuals().text_color();
                        p.text(
                            egui::pos2(x, rect.top() + 4.0),
                            egui::Align2::LEFT_TOP,
                            name,
                            egui::FontId::proportional(14.0),
                            text,
                        );
                        let mut sub = format!("{}  {}x{}", l.slug, l.size[0], l.size[1]);
                        if !l.tags.is_empty() {
                            sub.push_str("  #");
                            sub.push_str(&l.tags.join(" #"));
                        }
                        p.text(
                            egui::pos2(x, rect.bottom() - 4.0),
                            egui::Align2::LEFT_BOTTOM,
                            sub,
                            egui::FontId::proportional(11.0),
                            Color32::from_gray(150),
                        );
                        if let Some(n) = problems.get(l.slug.as_str()) {
                            p.text(
                                rect.right_center() - Vec2::new(6.0, 0.0),
                                egui::Align2::RIGHT_CENTER,
                                format!("⚠ {n}"),
                                egui::FontId::proportional(13.0),
                                Color32::from_rgb(230, 170, 80),
                            );
                        }
                        let resp = resp.on_hover_text(format!(
                            "{} at [{}, {}]. Double click to frame it; drag to reorder",
                            l.slug, l.world_px[0], l.world_px[1]
                        ));
                        if resp.double_clicked() {
                            framed = Some(ix);
                        } else if resp.clicked() {
                            clicked = Some(ix);
                        }
                        if resp.drag_started() {
                            ls.drag = Some(l.id.clone());
                        }
                        resp.context_menu(|ui| {
                            if let Some(pick) = level_menu(ui, doc, ix) {
                                menu = Some((ix, pick));
                                ui.close();
                            }
                        });
                    }
                }
            }
        });
    // ---- drag to reorder ----
    if let Some(id) = ls.drag.clone() {
        let pointer = ui.input(|i| i.pointer.interact_pos());
        let released = ui.input(|i| i.pointer.any_released() || !i.pointer.any_down());
        let target = pointer.and_then(|p| drop_target(doc, &drawn, &id, p));
        if let Some((_, line_y, _)) = target
            && let Some((_, r)) = drawn.first()
        {
            ui.painter().hline(
                r.left()..=r.right(),
                line_y,
                Stroke::new(2.0, ui.visuals().selection.stroke.color),
            );
        }
        if released {
            ls.drag = None;
            if let Some((ix, _, to)) = target {
                let slug = doc.level(ix).slug.clone();
                pending.extend(move_to(doc, &slug, to));
            }
        }
    }
    // ---- what the rows asked for ----
    if cancel_rename {
        ls.rename = None;
        ls.error = None;
    }
    if commit_rename && let Some(r) = ls.rename.take() {
        match rename_command(doc, &r) {
            Ok(Some(cmd)) => {
                ls.error = None;
                pending.push(cmd);
            }
            Ok(None) => ls.error = None,
            Err(e) => {
                ls.error = Some(e);
                ls.rename = Some(r);
            }
        }
    }
    if let Some(ix) = clicked {
        select(view, doc, ix);
    }
    if let Some(ix) = framed {
        select(view, doc, ix);
        actions.push(Action::FitLevel);
    }
    if let Some((ix, pick)) = menu {
        let l = doc.level(ix);
        match pick {
            MenuPick::Frame => {
                select(view, doc, ix);
                actions.push(Action::FitLevel);
            }
            MenuPick::Rename => {
                ls.error = None;
                ls.rename = Some(Rename {
                    level: l.id.clone(),
                    name: l.name.clone(),
                    slug: l.slug.to_string(),
                    focus: true,
                });
            }
            MenuPick::Duplicate => actions.push(Action::DuplicateLevel(ix)),
            MenuPick::Crop => {
                view.active_level = Some(ix);
                actions.push(Action::CropLevel);
            }
            MenuPick::Earlier => {
                let i = order_index(doc, ix);
                if i > 0 {
                    pending.extend(move_to(doc, &l.slug, i - 1));
                }
            }
            MenuPick::Later => {
                pending.extend(move_to(doc, &l.slug, order_index(doc, ix) + 1));
            }
            MenuPick::ToWorld(w) => actions.push(Action::MoveLevelToWorld {
                level: l.slug.clone(),
                world: w,
            }),
            MenuPick::Delete => {
                view.active_level = Some(ix);
                actions.push(Action::DeleteLevel);
            }
        }
    }
}

/// Where a dragged level would go with the pointer at `p`: (the dragged
/// level, the y of the insertion line, its new index in the level order).
/// Only between rows of its own world.
fn drop_target(
    doc: &Document,
    drawn: &[(LevelIx, egui::Rect)],
    id: &Id,
    p: egui::Pos2,
) -> Option<(LevelIx, f32, usize)> {
    let dragged = doc.level_ix_by_id(id)?;
    let world = doc.levels[dragged].world;
    let rows: Vec<&(LevelIx, egui::Rect)> = drawn
        .iter()
        .filter(|(ix, _)| doc.levels[*ix].world == world)
        .collect();
    rows.first()?;
    // The first row whose middle is below the pointer: insert before it.
    let (line, before) = match rows.iter().find(|(_, r)| p.y < r.center().y) {
        Some((ix, r)) => (r.top(), order_index(doc, *ix)),
        None => {
            let (ix, r) = rows.last()?;
            (r.bottom(), order_index(doc, *ix) + 1)
        }
    };
    let from = order_index(doc, dragged);
    let to = if before > from { before - 1 } else { before };
    (to != from).then_some((dragged, line, to))
}

/// What a row's menu asked for.
#[derive(Debug, Clone, PartialEq)]
enum MenuPick {
    Frame,
    Rename,
    Duplicate,
    Crop,
    Earlier,
    Later,
    ToWorld(Slug),
    Delete,
}

fn level_menu(ui: &mut egui::Ui, doc: &Document, ix: LevelIx) -> Option<MenuPick> {
    let mut pick = None;
    let slug = doc.level(ix).slug.as_str();
    if ui.button(format!("Frame {slug}")).clicked() {
        pick = Some(MenuPick::Frame);
    }
    if ui.button("Rename…").clicked() {
        pick = Some(MenuPick::Rename);
    }
    if ui.button("Duplicate").clicked() {
        pick = Some(MenuPick::Duplicate);
    }
    if ui.button("Crop to content").clicked() {
        pick = Some(MenuPick::Crop);
    }
    ui.separator();
    let i = order_index(doc, ix);
    let n = doc.project.worlds[doc.levels[ix].world].levels.len();
    if ui
        .add_enabled(i > 0, egui::Button::new("Move earlier"))
        .clicked()
    {
        pick = Some(MenuPick::Earlier);
    }
    if ui
        .add_enabled(i + 1 < n, egui::Button::new("Move later"))
        .clicked()
    {
        pick = Some(MenuPick::Later);
    }
    let here = doc.levels[ix].world;
    if doc.project.worlds.len() > 1 {
        ui.menu_button("Move to world", |ui| {
            for (w, world) in doc.project.worlds.iter().enumerate() {
                if w != here && ui.button(super::worlds::world_label(world)).clicked() {
                    pick = Some(MenuPick::ToWorld(world.slug.clone()));
                }
            }
        });
    }
    ui.separator();
    if ui.button("Delete…").clicked() {
        pick = Some(MenuPick::Delete);
    }
    pick
}

/// A row being renamed: name and slug fields. Returns (commit, cancel).
fn rename_row(ui: &mut egui::Ui, r: &mut Rename) -> (bool, bool) {
    let mut ok = false;
    let mut cancel = false;
    ui.horizontal(|ui| {
        ui.set_min_height(ROW_H);
        let n = ui.add(
            egui::TextEdit::singleline(&mut r.name)
                .hint_text("name")
                .desired_width(120.0),
        );
        mark(ui, "levels:rename:name", n.rect);
        if std::mem::take(&mut r.focus) {
            n.request_focus();
        }
        let s = ui.add(
            egui::TextEdit::singleline(&mut r.slug)
                .hint_text("slug")
                .desired_width(100.0),
        );
        mark(ui, "levels:rename:slug", s.rect);
        let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
        if (n.lost_focus() || s.lost_focus()) && enter {
            ok = true;
        }
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            cancel = true;
        }
        let b = ui.small_button("OK");
        mark(ui, "levels:rename:ok", b.rect);
        ok |= b.clicked();
        cancel |= ui.small_button("Cancel").clicked();
    });
    (ok, cancel)
}

/// The `rename_level` a rename editor asks for (`None`: nothing changed).
pub fn rename_command(doc: &Document, r: &Rename) -> Result<Option<Command>, String> {
    let ix = doc.level_ix_by_id(&r.level).ok_or("That level is gone")?;
    let l = doc.level(ix);
    let slug = Slug::new(r.slug.trim()).map_err(|e| e.to_string())?;
    let name = r.name.trim().to_string();
    if slug == l.slug && name == l.name {
        return Ok(None);
    }
    if slug != l.slug && doc.project.level(slug.as_str()).is_some() {
        return Err(format!("A level {slug} already exists"));
    }
    Ok(Some(Command::RenameLevel {
        world: doc.project.worlds[doc.levels[ix].world].slug.clone(),
        level: l.slug.clone(),
        slug,
        name,
    }))
}

#[cfg(test)]
mod tests {
    use super::super::testkit::{Driver, batches, state};
    use super::*;

    fn s(x: &str) -> Slug {
        Slug::new(x).unwrap()
    }

    fn slugs(doc: &Document, rows: &[Row]) -> Vec<String> {
        rows.iter()
            .map(|r| match r {
                Row::World { world, .. } => format!("[{}]", doc.project.worlds[*world].slug),
                Row::Level(ix) => doc.level(*ix).slug.to_string(),
            })
            .collect()
    }

    #[test]
    fn rows_search_sort_and_group() {
        let (_tmp, mut st) = state();
        let doc = st.doc.as_mut().unwrap();
        doc.project
            .apply(Command::SetLevelTags {
                world: s("main"),
                level: s("shaft"),
                tags: vec!["Forest".into()],
            })
            .unwrap();
        let doc = st.doc.as_ref().unwrap();
        let v = &st.view;
        assert_eq!(
            slugs(doc, &rows(doc, v, "", Sort::Order, false)),
            ["crevasse", "shaft"]
        );
        // Shaft (6x8 at [320, 80]) is below crevasse ([0, 0]).
        assert_eq!(
            slugs(doc, &rows(doc, v, "", Sort::Position, false)),
            ["crevasse", "shaft"]
        );
        assert_eq!(
            slugs(doc, &rows(doc, v, "forest", Sort::Order, false)),
            ["shaft"]
        );
        assert_eq!(
            slugs(doc, &rows(doc, v, "tag:for", Sort::Name, false)),
            ["shaft"]
        );
        assert_eq!(
            slugs(doc, &rows(doc, v, "crev", Sort::Order, true)),
            ["[main]", "crevasse"]
        );
        assert!(rows(doc, v, "nothing", Sort::Order, false).is_empty());
    }

    #[test]
    fn move_to_builds_one_reorder() {
        let (_tmp, st) = state();
        let doc = st.doc.as_ref().unwrap();
        assert_eq!(
            move_to(doc, &s("crevasse"), 1),
            Some(Command::ReorderLevels {
                world: s("main"),
                order: vec![s("shaft"), s("crevasse")],
                restore: false,
            })
        );
        assert_eq!(move_to(doc, &s("crevasse"), 0), None);
    }

    #[test]
    fn click_selects_double_click_frames_and_menu_reorders() {
        let (_tmp, mut st) = state();
        let mut d = Driver::new(pane);
        d.frame(&mut st, vec![]);
        d.frame(&mut st, vec![]);
        let r = d.rect("levels:row:shaft");
        d.click(&mut st, r.center());
        let doc = st.doc.as_ref().unwrap();
        assert_eq!(st.view.active_level, doc.level_ix("shaft"));
        assert_eq!(
            st.view.selection.items,
            [SelItem::Level(Id::new("shaft22222").unwrap())]
        );
        // Thumbnails were made for the rows on screen.
        assert_eq!(st.thumbs.rendered, 2);
        // Idle frames make none and rebuild no rows.
        let builds = st.levels.builds;
        d.frame(&mut st, vec![]);
        assert_eq!(st.thumbs.rendered, 2);
        assert_eq!(st.levels.builds, builds);
        // Rename by hand: the editor and its command.
        let doc = st.doc.as_ref().unwrap();
        let r = Rename {
            level: Id::new("shaft22222").unwrap(),
            name: "The shaft".into(),
            slug: "deep_shaft".into(),
            focus: false,
        };
        assert!(matches!(
            rename_command(doc, &r),
            Ok(Some(Command::RenameLevel { slug, .. })) if slug == "deep_shaft"
        ));
        let taken = Rename {
            slug: "crevasse".into(),
            ..r.clone()
        };
        assert!(rename_command(doc, &taken).is_err());
        assert!(batches(&mut st).is_empty());
    }
}
