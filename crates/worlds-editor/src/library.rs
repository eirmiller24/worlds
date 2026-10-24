//! The Library: the project's assemblies (`assemblies/<slug>.json`) as a
//! part of the Palette pane, and the "Save selection as assembly…" dialog
//! (Ctrl+Shift+S).
//!
//! The Library works like the entity palette: a search box (fuzzy over
//! name, slug, category and tags, ranked in that order), **Favorites** (★
//! on any row), and the **catalogue** as a tree by `category`
//! (`Uncategorized` last); a search shows the matches instead, best first.
//! Each row has the assembly's thumbnail ([`crate::thumbs`]), name, slug
//! and size. A click arms it: a paste ghost of the assembly follows the
//! pointer ([`crate::clipboard::PasteGhost::assembly`]); X, Y and R turn it,
//! a click on the map places it (one entry, `Place assembly <slug>`),
//! Escape cancels. Right click: Favorite, Remove assembly.

use std::collections::{BTreeMap, BTreeSet};

use egui::{Align2, FontId, Rect, Sense, Vec2, pos2};
use serde::{Deserialize, Serialize};
use worlds_core::assembly::{Assembly, AssemblyInfo};
use worlds_core::{Command, Project, Slug};

use crate::app::EditorState;
use crate::app::actions::{Action, fuzzy_query, fuzzy_score_chars};

/// The category of assemblies without one.
pub const UNCATEGORIZED: &str = "Uncategorized";
/// The collapsible parts' keys in [`LibraryView::collapsed`].
pub const PART_FAVORITES: &str = "/favorites";
pub const PART_CATALOGUE: &str = "/catalogue";
/// Height of a Library row.
pub const ROW_H: f32 = 44.0;

/// The Library's remembered state (`.worlds/ui.json` `library`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LibraryView {
    /// The Palette pane shows the Library instead of the active layer's
    /// palette.
    pub open: bool,
    /// Starred assembly slugs, in the order they were starred.
    pub favorites: Vec<Slug>,
    /// Collapsed parts (`/favorites`, `/catalogue`) and category paths.
    pub collapsed: BTreeSet<String>,
}

impl LibraryView {
    pub fn toggle_favorite(&mut self, s: &Slug) {
        if let Some(i) = self.favorites.iter().position(|f| f == s) {
            self.favorites.remove(i);
        } else {
            self.favorites.push(s.clone());
        }
    }

    /// Assembly `from` is now `to`.
    pub fn renamed(&mut self, from: &Slug, to: &Slug) {
        for f in &mut self.favorites {
            if f == from {
                *f = to.clone();
            }
        }
    }
}

/// The "Save selection as assembly…" dialog's fields.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SaveDialog {
    pub slug: String,
    pub name: String,
    pub category: String,
    /// Comma-separated.
    pub tags: String,
    pub doc: String,
    /// Replace an assembly with this slug.
    pub replace: bool,
    /// Why Save was refused.
    pub error: Option<String>,
}

impl SaveDialog {
    /// What Save would save, or why it can't.
    pub fn info(&self, project: &Project) -> Result<(AssemblyInfo, bool), String> {
        let slug = Slug::new(self.slug.trim()).map_err(|_| {
            format!(
                "{:?} isn't a slug: lowercase letters, digits and _, starting with a letter",
                self.slug.trim()
            )
        })?;
        if self.name.trim().is_empty() {
            return Err("Give it a name".into());
        }
        if !self.replace && project.assembly(slug.as_str()).is_some() {
            return Err(format!(
                "An assembly {slug} exists already: tick Replace, or pick another slug"
            ));
        }
        let tags: Vec<String> = self
            .tags
            .split(',')
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect();
        let opt = |s: &str| Some(s.trim().to_string()).filter(|s| !s.is_empty());
        Ok((
            AssemblyInfo {
                slug,
                name: self.name.trim().to_string(),
                category: opt(&self.category),
                tags,
                doc: opt(&self.doc),
            },
            self.replace,
        ))
    }
}

/// What isn't saved: the search, the thumbnails, the dialog.
#[derive(Default)]
pub struct LibraryState {
    pub query: String,
    pub thumbs: crate::thumbs::Thumbs,
    /// The save dialog, while open.
    pub dialog: Option<SaveDialog>,
    /// Where each row and header was drawn last frame, by slug or key
    /// (tests).
    #[doc(hidden)]
    pub rects: BTreeMap<String, Rect>,
}

/// How well `query` matches an assembly: (tier, score), tier 3 name, 2
/// slug, 1 category, 0 a tag. An empty query matches everything.
pub fn rank(a: &Assembly, q: &[char]) -> Option<(u8, i32)> {
    if q.is_empty() {
        return Some((0, 0));
    }
    if let Some(s) = fuzzy_score_chars(q, &a.name) {
        return Some((3, s));
    }
    if let Some(s) = fuzzy_score_chars(q, a.slug.as_str()) {
        return Some((2, s));
    }
    if let Some(s) = a.category.as_deref().and_then(|c| fuzzy_score_chars(q, c)) {
        return Some((1, s));
    }
    a.tags
        .iter()
        .filter_map(|t| fuzzy_score_chars(q, t))
        .max()
        .map(|s| (0, s))
}

/// One Library row.
#[derive(Debug, Clone, PartialEq)]
pub enum Row {
    Part {
        key: &'static str,
        title: String,
        collapsed: bool,
    },
    Category {
        path: String,
        depth: usize,
        count: usize,
        collapsed: bool,
    },
    Item {
        slug: Slug,
        depth: usize,
    },
    Note(String),
}

/// The rows the Library shows for `query` (see the module docs).
pub fn rows(project: &Project, lib: &LibraryView, query: &str) -> Vec<Row> {
    let q = fuzzy_query(query);
    let searching = !q.is_empty();
    let mut hits: Vec<(u8, i32, &Assembly)> = project
        .assemblies
        .iter()
        .filter_map(|a| rank(a, &q).map(|(t, s)| (t, s, a)))
        .collect();
    hits.sort_by(|a, b| {
        (b.0, b.1)
            .cmp(&(a.0, a.1))
            .then_with(|| a.2.name.to_lowercase().cmp(&b.2.name.to_lowercase()))
            .then_with(|| a.2.slug.cmp(&b.2.slug))
    });
    let matching: BTreeSet<&str> = hits.iter().map(|h| h.2.slug.as_str()).collect();
    let mut out = Vec::new();
    let c = lib.collapsed.contains(PART_FAVORITES);
    let favs: Vec<&Slug> = lib
        .favorites
        .iter()
        .filter(|s| matching.contains(s.as_str()))
        .collect();
    if !(searching && favs.is_empty()) {
        out.push(Row::Part {
            key: PART_FAVORITES,
            title: format!("Favorites ({})", favs.len()),
            collapsed: c,
        });
        if !c {
            if favs.is_empty() {
                out.push(Row::Note("Star an assembly (★) to keep it here".into()));
            }
            out.extend(favs.into_iter().map(|s| Row::Item {
                slug: s.clone(),
                depth: 0,
            }));
        }
    }
    let c = lib.collapsed.contains(PART_CATALOGUE);
    out.push(Row::Part {
        key: PART_CATALOGUE,
        title: if searching {
            format!("Matches ({})", hits.len())
        } else {
            format!("Library ({})", hits.len())
        },
        collapsed: c,
    });
    if c {
        return out;
    }
    if hits.is_empty() {
        out.push(Row::Note(if searching {
            "No matching assemblies".into()
        } else {
            "No assemblies yet: select something on the map and press Ctrl+Shift+S".into()
        }));
        return out;
    }
    if searching {
        out.extend(hits.iter().map(|h| Row::Item {
            slug: h.2.slug.clone(),
            depth: 0,
        }));
        return out;
    }
    #[derive(Default)]
    struct Node<'a> {
        children: BTreeMap<String, Node<'a>>,
        items: Vec<&'a Assembly>,
        count: usize,
    }
    let mut root = Node::default();
    for (_, _, a) in &hits {
        let mut node = &mut root;
        let cat = a
            .category
            .as_deref()
            .filter(|c| !c.trim().is_empty())
            .unwrap_or(UNCATEGORIZED);
        for seg in cat.split('/').map(str::trim).filter(|s| !s.is_empty()) {
            node = node.children.entry(seg.to_string()).or_default();
            node.count += 1;
        }
        node.items.push(a);
    }
    fn walk(out: &mut Vec<Row>, node: &Node<'_>, path: &str, depth: usize, lib: &LibraryView) {
        let mut kids: Vec<(&String, &Node<'_>)> = node.children.iter().collect();
        kids.sort_by_key(|(k, _)| (depth == 0 && k.as_str() == UNCATEGORIZED, k.to_lowercase()));
        for (name, child) in kids {
            let p = if path.is_empty() {
                name.clone()
            } else {
                format!("{path}/{name}")
            };
            let c = lib.collapsed.contains(&p);
            out.push(Row::Category {
                path: p.clone(),
                depth,
                count: child.count,
                collapsed: c,
            });
            if !c {
                walk(out, child, &p, depth + 1, lib);
                out.extend(child.items.iter().map(|a| Row::Item {
                    slug: a.slug.clone(),
                    depth: depth + 1,
                }));
            }
        }
    }
    walk(&mut out, &root, "", 0, lib);
    out
}

/// The Palette pane's Layer | Library switch. Returns whether the Library
/// is showing.
pub fn switch(ui: &mut egui::Ui, view: &mut crate::view::ViewState) -> bool {
    ui.horizontal(|ui| {
        if ui
            .add(egui::Button::selectable(!view.library.open, "Layer"))
            .on_hover_text("What the active layer can hold")
            .clicked()
        {
            view.library.open = false;
        }
        if ui
            .add(egui::Button::selectable(view.library.open, "Library"))
            .on_hover_text("Assemblies: saved pieces of levels to place (Ctrl+Shift+S saves one)")
            .clicked()
        {
            view.library.open = true;
        }
    });
    view.library.open
}

/// The hover card of an assembly.
fn hover_card(ui: &mut egui::Ui, a: &Assembly) {
    ui.strong(&a.name);
    ui.weak(format!(
        "{} · {}",
        a.slug,
        a.category.as_deref().unwrap_or(UNCATEGORIZED)
    ));
    if !a.tags.is_empty() {
        ui.label(format!("Tags: {}", a.tags.join(", ")));
    }
    if let Some(d) = &a.doc {
        ui.label(d);
    }
    ui.label(format!(
        "{} x {} cells, {} items",
        a.size[0],
        a.size[1],
        a.item_count()
    ));
    ui.weak("Click to place; X, Y and R turn it");
}

/// The Library part of the Palette pane.
pub fn library(ui: &mut egui::Ui, st: &mut EditorState) {
    let EditorState {
        doc,
        view,
        library: lib,
        actions,
        pending,
        accent,
        ..
    } = st;
    let Some(doc) = doc.as_ref() else {
        ui.weak("No project");
        return;
    };
    lib.rects.clear();
    ui.add(
        egui::TextEdit::singleline(&mut lib.query)
            .hint_text("Search assemblies…")
            .desired_width(f32::INFINITY),
    );
    let armed = view.paste.as_ref().and_then(|p| p.assembly.clone());
    ui.separator();
    let rows = rows(&doc.project, &view.library, &lib.query);
    let mut toggle: Option<String> = None;
    let mut fav: Option<Slug> = None;
    let mut arm: Option<Slug> = None;
    let mut remove: Option<Slug> = None;
    let ctx = ui.ctx().clone();
    egui::ScrollArea::vertical()
        .id_salt("library rows")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for row in &rows {
                let h = if matches!(row, Row::Item { .. }) {
                    ROW_H
                } else {
                    24.0
                };
                let (rect, resp) =
                    ui.allocate_exact_size(Vec2::new(ui.available_width(), h), Sense::click());
                let p = ui.painter_at(rect.expand(1.0));
                let text = ui.visuals().text_color();
                match row {
                    Row::Part {
                        key,
                        title,
                        collapsed,
                    } => {
                        lib.rects.insert((*key).into(), rect);
                        if resp.hovered() {
                            p.rect_filled(rect, 3.0, ui.visuals().widgets.hovered.bg_fill);
                        }
                        p.text(
                            rect.left_center() + Vec2::new(4.0, 0.0),
                            Align2::LEFT_CENTER,
                            format!("{} {title}", if *collapsed { "⏵" } else { "⏷" }),
                            FontId::proportional(13.0),
                            ui.visuals().strong_text_color(),
                        );
                        if resp.clicked() {
                            toggle = Some((*key).into());
                        }
                    }
                    Row::Category {
                        path,
                        depth,
                        count,
                        collapsed,
                    } => {
                        lib.rects.insert(path.clone(), rect);
                        if resp.hovered() {
                            p.rect_filled(rect, 3.0, ui.visuals().widgets.hovered.bg_fill);
                        }
                        let name = path.rsplit('/').next().unwrap_or(path);
                        p.text(
                            rect.left_center() + Vec2::new(8.0 + 12.0 * *depth as f32, 0.0),
                            Align2::LEFT_CENTER,
                            format!("{} {name}", if *collapsed { "⏵" } else { "⏷" }),
                            FontId::proportional(13.0),
                            text,
                        );
                        p.text(
                            rect.right_center() - Vec2::new(6.0, 0.0),
                            Align2::RIGHT_CENTER,
                            count.to_string(),
                            FontId::proportional(11.0),
                            ui.visuals().weak_text_color(),
                        );
                        if resp.clicked() {
                            toggle = Some(path.clone());
                        }
                    }
                    Row::Note(s) => {
                        p.text(
                            rect.left_center() + Vec2::new(12.0, 0.0),
                            Align2::LEFT_CENTER,
                            s,
                            FontId::proportional(12.0),
                            ui.visuals().weak_text_color(),
                        );
                    }
                    Row::Item { slug, depth } => {
                        let Some(a) = doc.project.assembly(slug.as_str()) else {
                            continue;
                        };
                        lib.rects.entry(slug.to_string()).or_insert(rect);
                        if armed.as_ref() == Some(slug) {
                            p.rect_filled(rect, 3.0, ui.visuals().selection.bg_fill);
                        } else if resp.hovered() {
                            p.rect_filled(rect, 3.0, ui.visuals().widgets.hovered.bg_fill);
                        }
                        let x0 = rect.min.x + 8.0 + 12.0 * *depth as f32;
                        let thumb_box = Rect::from_min_size(
                            pos2(x0, rect.min.y + 2.0),
                            Vec2::splat(ROW_H - 4.0),
                        );
                        let tex = lib.thumbs.texture(&ctx, &doc.dir, &doc.project, a);
                        let [tw, th] = tex.size();
                        let s = (thumb_box.width() / tw as f32).min(thumb_box.height() / th as f32);
                        let r = Rect::from_center_size(
                            thumb_box.center(),
                            Vec2::new(tw as f32 * s, th as f32 * s),
                        );
                        p.image(
                            tex.id(),
                            r,
                            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                            egui::Color32::WHITE,
                        );
                        let star = Rect::from_min_max(
                            pos2(rect.max.x - 22.0, rect.min.y),
                            pos2(rect.max.x - 4.0, rect.max.y),
                        );
                        let sr = ui.interact(star, resp.id.with("star"), Sense::click());
                        let is_fav = view.library.favorites.contains(slug);
                        p.text(
                            star.center(),
                            Align2::CENTER_CENTER,
                            if is_fav { "★" } else { "☆" },
                            FontId::proportional(14.0),
                            if is_fav {
                                *accent
                            } else {
                                ui.visuals().weak_text_color()
                            },
                        );
                        if sr
                            .on_hover_text(if is_fav { "Unfavorite" } else { "Favorite" })
                            .clicked()
                        {
                            fav = Some(slug.clone());
                        }
                        let name_x = thumb_box.max.x + 8.0;
                        let pc = p.with_clip_rect(Rect::from_min_max(
                            pos2(name_x, rect.min.y),
                            pos2(star.min.x - 2.0, rect.max.y),
                        ));
                        pc.text(
                            pos2(name_x, rect.center().y - 8.0),
                            Align2::LEFT_CENTER,
                            &a.name,
                            FontId::proportional(13.0),
                            text,
                        );
                        pc.text(
                            pos2(name_x, rect.center().y + 9.0),
                            Align2::LEFT_CENTER,
                            format!("{} · {}x{}", a.slug, a.size[0], a.size[1]),
                            FontId::proportional(11.0),
                            ui.visuals().weak_text_color(),
                        );
                        if resp.clicked() {
                            arm = Some(slug.clone());
                        }
                        let resp = resp.on_hover_ui(|ui| hover_card(ui, a));
                        resp.context_menu(|ui| {
                            if ui
                                .button(if is_fav { "Unfavorite" } else { "Favorite" })
                                .clicked()
                            {
                                fav = Some(slug.clone());
                                ui.close();
                            }
                            if ui.button("Place…").clicked() {
                                arm = Some(slug.clone());
                                ui.close();
                            }
                            if ui.button("Remove assembly").clicked() {
                                remove = Some(slug.clone());
                                ui.close();
                            }
                        });
                    }
                }
            }
        });
    if let Some(k) = toggle
        && !view.library.collapsed.remove(&k)
    {
        view.library.collapsed.insert(k);
    }
    if let Some(s) = fav {
        view.library.toggle_favorite(&s);
    }
    if let Some(s) = arm {
        actions.push(Action::ArmAssembly(s));
    }
    if let Some(slug) = remove {
        pending.push(Command::RemoveAssembly { slug });
    }
}

/// The "Save selection as assembly…" dialog, while open.
pub fn save_dialog(ctx: &egui::Context, st: &mut EditorState) {
    let EditorState {
        doc,
        library: lib,
        actions,
        ..
    } = st;
    let (Some(doc), Some(d)) = (doc.as_ref(), lib.dialog.as_mut()) else {
        return;
    };
    let mut open = true;
    let mut close = false;
    egui::Window::new("Save selection as assembly")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
        .show(ctx, |ui| {
            egui::Grid::new("save assembly form")
                .num_columns(2)
                .spacing([8.0, 6.0])
                .show(ui, |ui| {
                    ui.label("Slug");
                    ui.add(egui::TextEdit::singleline(&mut d.slug).hint_text("doorway"));
                    ui.end_row();
                    ui.label("Name");
                    ui.add(egui::TextEdit::singleline(&mut d.name).hint_text("Doorway"));
                    ui.end_row();
                    ui.label("Category");
                    ui.add(egui::TextEdit::singleline(&mut d.category).hint_text("Puzzle/Doors"));
                    ui.end_row();
                    ui.label("Tags");
                    ui.add(egui::TextEdit::singleline(&mut d.tags).hint_text("door, puzzle"));
                    ui.end_row();
                    ui.label("Doc");
                    ui.add(egui::TextEdit::multiline(&mut d.doc).desired_rows(2));
                    ui.end_row();
                });
            if doc.project.assembly(d.slug.trim()).is_some() {
                ui.checkbox(&mut d.replace, "Replace the assembly with this slug");
            }
            if let Some(e) = &d.error {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
            ui.horizontal(|ui| {
                if ui.button("Save").clicked() {
                    match d.info(&doc.project) {
                        Ok((info, replace)) => {
                            actions.push(Action::SaveAssembly { info, replace });
                            close = true;
                        }
                        Err(e) => d.error = Some(e),
                    }
                }
                if ui.button("Cancel").clicked() {
                    close = true;
                }
            });
        });
    if !open || close {
        lib.dialog = None;
    }
}
