//! The Notes pane: every annotation of the project, filterable by
//! search, kind, scope, status and tags (all of them); a click selects one
//! and frames it (its level becomes active); a checkbox marks it done (one
//! `update_note`). Guides are a collapsed "Guides (n)" group at the end.
//! Above the list: per-tag visibility (eye toggles, with counts), "hide
//! done", the guide groups (offset, color: one `set_guide_group` each),
//! "Show guides as designed" and "Snap to guides".
//!
//! The rows are one flat list rebuilt only when the project or the filter
//! changes (the entity palette's pattern) and drawn with `show_rows`.

use egui::{Color32, RichText, Sense};
use worlds_core::{Command, Id, NoteStatus, Shape, Slug};

use super::EditorState;
use super::fields::mark;
use crate::annot::{self, At};
use crate::view::SelItem;
use crate::viewport::Fit;

/// The status filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StatusFilter {
    #[default]
    Open,
    Done,
    All,
}

impl StatusFilter {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "open" => Some(StatusFilter::Open),
            "done" => Some(StatusFilter::Done),
            "all" => Some(StatusFilter::All),
            _ => None,
        }
    }
}

/// The scope filter: every annotation, world ones, the active level's,
/// or one level's (by slug).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ScopeFilter {
    #[default]
    All,
    World,
    ThisLevel,
    Level(Slug),
}

/// One row of the list.
#[derive(Debug, Clone, PartialEq)]
pub enum Row {
    Note {
        id: Id,
        kind: &'static str,
        label: String,
        scope: String,
        tags: Vec<String>,
        done: bool,
        color: Color32,
    },
    /// "Guides (n)", a toggle.
    GuidesHeader(usize),
}

/// The pane's state.
#[derive(Default)]
pub struct NotesState {
    pub search: String,
    /// `None`: every kind but guides (guides have their group).
    pub kind: Option<String>,
    pub scope: ScopeFilter,
    pub status: StatusFilter,
    /// Tag chips: annotations carrying every one of them.
    pub tags: Vec<String>,
    /// The Guides group is open.
    pub guides_open: bool,
    /// The "Tags, visibility and guide groups" section is open.
    pub view_open: bool,
    /// The edit revision the rows were built at (`EditorApp::frame` sets
    /// `rev` each frame).
    pub rev: u64,
    /// Shared with the frame that draws them (a clone is a pointer copy).
    rows: std::sync::Arc<Vec<Row>>,
    built: Option<(u64, String)>,
    /// Tag counts ([`annot::tag_counts`]) and the revision they were
    /// counted at.
    counts: std::sync::Arc<Vec<(String, usize)>>,
    counted: Option<u64>,
    /// How many times rows and counts were built (tests: bounded work).
    pub builds: (usize, usize),
    /// Bring the pane to the front.
    pub focus: bool,
    /// Where the pane drew last (tests).
    pub pane_rect: Option<egui::Rect>,
}

impl NotesState {
    /// The rows as of the last build (tests).
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    fn key(&self, active: Option<usize>) -> String {
        format!(
            "{}|{:?}|{:?}|{:?}|{:?}|{}|{:?}",
            self.search, self.kind, self.scope, self.status, self.tags, self.guides_open, active
        )
    }

    /// Rebuild the rows when the project or the filter changed.
    pub fn refresh(&mut self, doc: &crate::doc::Document, active: Option<usize>) {
        let key = self.key(active);
        if self.built.as_ref() == Some(&(self.rev, key.clone())) {
            return;
        }
        self.rows = std::sync::Arc::new(build_rows(self, doc, active));
        self.built = Some((self.rev, key));
        self.builds.0 += 1;
    }

    /// Every tag with its count, counted again only when the project
    /// changed.
    pub fn tag_counts(
        &mut self,
        doc: &crate::doc::Document,
    ) -> std::sync::Arc<Vec<(String, usize)>> {
        if self.counted != Some(self.rev) {
            self.counts = std::sync::Arc::new(annot::tag_counts(doc));
            self.counted = Some(self.rev);
            self.builds.1 += 1;
        }
        self.counts.clone()
    }
}

/// The rows the filter gives: matching annotations (no guides), then the
/// Guides header and, when open, the matching guides.
pub fn build_rows(f: &NotesState, doc: &crate::doc::Document, active: Option<usize>) -> Vec<Row> {
    let q = f.search.to_lowercase();
    let mut notes = Vec::new();
    let mut guides = Vec::new();
    for r in annot::all_notes(doc) {
        let a = r.note;
        let scope_ok = match (&f.scope, r.at) {
            (ScopeFilter::All, _) => true,
            (ScopeFilter::World, At::World(_)) => true,
            (ScopeFilter::ThisLevel, At::Level(ix)) => Some(ix) == active,
            (ScopeFilter::Level(s), At::Level(ix)) => doc.level(ix).slug == *s,
            _ => false,
        };
        let status_ok = match f.status {
            StatusFilter::All => true,
            StatusFilter::Open => a.status == NoteStatus::Open,
            StatusFilter::Done => a.status == NoteStatus::Done,
        };
        let is_guide = matches!(a.shape, Shape::Guide { .. });
        let kind_ok = match &f.kind {
            None => true,
            Some(k) => a.shape.name() == k,
        };
        let tags_ok = f.tags.iter().all(|t| a.tags.contains(t));
        let search_ok = q.is_empty()
            || a.id.as_str().contains(&q)
            || a.title
                .as_deref()
                .is_some_and(|t| t.to_lowercase().contains(&q))
            || a.body
                .as_deref()
                .is_some_and(|t| t.to_lowercase().contains(&q));
        if !(scope_ok && (status_ok || is_guide) && kind_ok && tags_ok && search_ok) {
            continue;
        }
        let row = Row::Note {
            id: a.id.clone(),
            kind: a.shape.name(),
            label: a
                .title
                .clone()
                .unwrap_or_else(|| format!("{} {}", a.shape.name(), a.id)),
            scope: match r.at {
                At::World(w) => format!("world {}", doc.project.worlds[w].slug),
                At::Level(ix) => doc.level(ix).slug.to_string(),
            },
            tags: a.tags.clone(),
            done: a.status == NoteStatus::Done,
            color: crate::viewport::notes::note_color(doc, r),
        };
        if is_guide {
            guides.push(row);
        } else {
            notes.push(row);
        }
    }
    if !guides.is_empty() {
        notes.push(Row::GuidesHeader(guides.len()));
        if f.guides_open {
            notes.extend(guides);
        }
    }
    notes
}

/// The Notes pane.
pub fn pane(ui: &mut egui::Ui, st: &mut EditorState) {
    st.notes.pane_rect = Some(ui.max_rect());
    let Some(doc) = st.doc.as_ref() else {
        ui.weak("No project");
        return;
    };
    let active = st.view.active_level.filter(|&i| i < doc.levels.len());
    // ---- filter row ----
    ui.horizontal_wrapped(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut st.notes.search)
                .hint_text("Search title, body, id")
                .desired_width(140.0),
        );
        egui::ComboBox::from_id_salt("notes kind")
            .selected_text(st.notes.kind.clone().unwrap_or_else(|| "any kind".into()))
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut st.notes.kind, None, "any kind");
                for k in ["note", "pin", "path", "region", "guide"] {
                    ui.selectable_value(&mut st.notes.kind, Some(k.to_string()), k);
                }
            });
        let scope_text = match &st.notes.scope {
            ScopeFilter::All => "All".to_string(),
            ScopeFilter::World => "World".to_string(),
            ScopeFilter::ThisLevel => "This level".to_string(),
            ScopeFilter::Level(s) => s.to_string(),
        };
        egui::ComboBox::from_id_salt("notes scope")
            .selected_text(scope_text)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut st.notes.scope, ScopeFilter::All, "All");
                ui.selectable_value(&mut st.notes.scope, ScopeFilter::World, "World");
                ui.selectable_value(&mut st.notes.scope, ScopeFilter::ThisLevel, "This level");
                for ix in 0..doc.levels.len() {
                    let s = doc.level(ix).slug.clone();
                    let t = s.to_string();
                    ui.selectable_value(&mut st.notes.scope, ScopeFilter::Level(s), t);
                }
            });
        egui::ComboBox::from_id_salt("notes status")
            .selected_text(format!("{:?}", st.notes.status))
            .show_ui(ui, |ui| {
                for s in [StatusFilter::Open, StatusFilter::Done, StatusFilter::All] {
                    ui.selectable_value(&mut st.notes.status, s, format!("{s:?}"));
                }
            });
    });
    // Tag chips (AND).
    let counts = st.notes.tag_counts(doc);
    ui.horizontal_wrapped(|ui| {
        let mut drop = None;
        for (i, t) in st.notes.tags.iter().enumerate() {
            if ui
                .add(egui::Button::selectable(true, format!("{t} ×")))
                .on_hover_text("Remove this tag filter")
                .clicked()
            {
                drop = Some(i);
            }
        }
        if let Some(i) = drop {
            st.notes.tags.remove(i);
        }
        egui::ComboBox::from_id_salt("notes add tag")
            .selected_text("+ tag")
            .show_ui(ui, |ui| {
                for (t, n) in counts.iter() {
                    if t == crate::view::UNTAGGED || st.notes.tags.contains(t) {
                        continue;
                    }
                    if ui.selectable_label(false, format!("{t} ({n})")).clicked() {
                        st.notes.tags.push(t.clone());
                    }
                }
            });
    });
    // ---- visibility and guides (folded by default, so the list shows) ----
    let r = egui::CollapsingHeader::new("Tags, visibility and guide groups")
        .id_salt("notes view")
        .open(Some(st.notes.view_open))
        .show(ui, |ui| view_section(ui, st, &counts));
    mark(ui, "notes:view", r.header_response.rect);
    if r.header_response.clicked() {
        st.notes.view_open = !st.notes.view_open;
    }
    ui.separator();
    // ---- the list ----
    let Some(doc) = st.doc.as_ref() else {
        return;
    };
    st.notes.refresh(doc, active);
    let rows = std::sync::Arc::clone(&st.notes.rows);
    if rows.is_empty() {
        ui.weak("No annotation matches.");
        return;
    }
    let row_h = ui.spacing().interact_size.y + 2.0;
    let mut clicked: Option<Id> = None;
    let mut toggle_guides = false;
    let mut done_set: Option<(Id, bool)> = None;
    let selected: std::collections::HashSet<Id> = st.view.selection.notes().cloned().collect();
    egui::ScrollArea::vertical()
        .id_salt("notes rows")
        .auto_shrink([false, false])
        .show_rows(ui, row_h, rows.len(), |ui, range| {
            for row in &rows[range] {
                match row {
                    Row::GuidesHeader(n) => {
                        let open = st.notes.guides_open;
                        let r = ui.add(egui::Button::selectable(
                            false,
                            format!("{} Guides ({n})", if open { "⏷" } else { "⏵" }),
                        ));
                        mark(ui, "notes:guides", r.rect);
                        if r.clicked() {
                            toggle_guides = true;
                        }
                    }
                    Row::Note {
                        id,
                        kind,
                        label,
                        scope,
                        tags,
                        done,
                        color,
                    } => {
                        ui.horizontal(|ui| {
                            let mut d = *done;
                            let cb = ui.checkbox(&mut d, "").on_hover_text("Done");
                            mark(ui, &format!("notes:done:{id}"), cb.rect);
                            if cb.changed() {
                                done_set = Some((id.clone(), d));
                            }
                            let (dot, _) =
                                ui.allocate_exact_size(egui::vec2(10.0, 10.0), Sense::hover());
                            ui.painter().circle_filled(dot.center(), 4.0, *color);
                            let text = RichText::new(label.as_str());
                            let r = ui
                                .add(
                                    egui::Button::selectable(selected.contains(id), text)
                                        .sense(Sense::click()),
                                )
                                .on_hover_text(format!("{kind} {id}"));
                            mark(ui, &format!("notes:row:{id}"), r.rect);
                            if r.clicked() {
                                clicked = Some(id.clone());
                            }
                            ui.weak(scope);
                            for t in tags.iter().take(3) {
                                ui.label(RichText::new(t).small().color(Color32::from_gray(200)));
                            }
                        });
                    }
                }
            }
        });
    if toggle_guides {
        st.notes.guides_open = !st.notes.guides_open;
    }
    if let Some((id, d)) = done_set
        && let Some(r) = annot::find_note(doc, &id)
    {
        let mut note = r.note.clone();
        note.status = if d {
            NoteStatus::Done
        } else {
            NoteStatus::Open
        };
        st.pending.push(Command::Batch {
            label: format!("Edit note {id}: status"),
            commands: vec![Command::UpdateNote { id, note }],
        });
    }
    if let Some(id) = clicked {
        jump(st, &id);
    }
}

/// Per-tag visibility, hide done, the guide groups and the guide toggles.
fn view_section(ui: &mut egui::Ui, st: &mut EditorState, counts: &[(String, usize)]) {
    let Some(doc) = st.doc.as_ref() else {
        return;
    };
    egui::CollapsingHeader::new("Tags")
        .id_salt("notes tags")
        .default_open(true)
        .show(ui, |ui| {
            let hidden = &mut st.view.annotations.hidden_tags;
            for (t, n) in counts {
                ui.horizontal(|ui| {
                    let shown = !hidden.contains(t);
                    let r = ui
                        .add(egui::Button::selectable(
                            shown,
                            if shown { "👁" } else { "—" },
                        ))
                        .on_hover_text(if shown {
                            "Shown on the map: click to hide every annotation with this tag"
                        } else {
                            "Hidden on the map: click to show"
                        });
                    mark(ui, &format!("notes:tag:{t}"), r.rect);
                    if r.clicked() {
                        if shown {
                            hidden.insert(t.clone());
                        } else {
                            hidden.remove(t);
                        }
                    }
                    ui.label(t);
                    ui.weak(n.to_string());
                });
            }
            ui.checkbox(&mut st.view.annotations.hide_done, "Hide done");
        });
    // ---- guide groups ----
    let mut set_group: Vec<Command> = Vec::new();
    for (w, world) in doc.project.worlds.iter().enumerate() {
        if world.guide_groups.is_empty() {
            continue;
        }
        egui::CollapsingHeader::new(format!("Guide groups ({})", world.slug))
            .id_salt(("notes groups", w))
            .default_open(true)
            .show(ui, |ui| {
                for g in &world.guide_groups {
                    let n = world
                        .notes
                        .iter()
                        .filter(|a| matches!(&a.shape, Shape::Guide { group: Some(s), .. } if *s == g.slug))
                        .count();
                    ui.horizontal(|ui| {
                        ui.strong(g.slug.as_str());
                        if let Some(name) = &g.name {
                            ui.label(name);
                        }
                        ui.weak(format!("{n} guides"));
                    });
                    ui.horizontal(|ui| {
                        ui.weak("offset");
                        for axis in 0..2 {
                            let key = format!("group:{}:{}:{axis}", world.slug, g.slug);
                            let id = egui::Id::new(&key);
                            let mut v: i64 = ui
                                .data(|d| d.get_temp(id))
                                .unwrap_or(g.offset[axis]);
                            let r = ui.add(egui::DragValue::new(&mut v).speed(1.0));
                            mark(ui, &format!("notes:{key}"), r.rect);
                            if r.dragged() || r.has_focus() || r.changed() {
                                ui.data_mut(|d| d.insert_temp(id, v));
                            }
                            let done = r.drag_stopped() || r.lost_focus();
                            if done {
                                ui.data_mut(|d| d.remove::<i64>(id));
                                if v != g.offset[axis] {
                                    let mut ng = g.clone();
                                    ng.offset[axis] = v;
                                    set_group.push(Command::SetGuideGroup {
                                        world: world.slug.clone(),
                                        group: ng,
                                        index: None,
                                    });
                                }
                            }
                        }
                        ui.weak("color");
                        let key = format!("group-color:{}:{}", world.slug, g.slug);
                        if let Some(hex) = super::panels::color_field(
                            ui,
                            &mut st.edit_bufs,
                            &key,
                            g.color.as_ref().map(|c| c.as_str()),
                        ) && let Ok(c) = worlds_core::Color::new(hex)
                        {
                            let mut ng = g.clone();
                            ng.color = Some(c);
                            set_group.push(Command::SetGuideGroup {
                                world: world.slug.clone(),
                                group: ng,
                                index: None,
                            });
                        }
                        if g.color.is_some() && ui.small_button("×").clicked() {
                            let mut ng = g.clone();
                            ng.color = None;
                            set_group.push(Command::SetGuideGroup {
                                world: world.slug.clone(),
                                group: ng,
                                index: None,
                            });
                        }
                    });
                }
            });
    }
    for c in set_group {
        st.pending.push(c);
    }
    ui.horizontal_wrapped(|ui| {
        ui.checkbox(
            &mut st.view.annotations.show_designed,
            "Show guides as designed",
        )
        .on_hover_text("Also draw grouped guides where they were laid out (dashed)");
        ui.checkbox(&mut st.view.annotations.snap_to_guides, "Snap to guides")
            .on_hover_text("Dragged levels snap to guide lines within 8 screen px");
    });
}

/// Select an annotation and frame it (`view.focus { note }` semantics:
/// its box with a margin, its level active).
pub fn jump(st: &mut EditorState, id: &Id) {
    let Some(doc) = st.doc.as_ref() else {
        return;
    };
    let Some((rect, level)) = annot::focus_rect(doc, id) else {
        return;
    };
    if let Some(ix) = level {
        st.view.active_level = Some(ix);
    }
    st.view.selection.set([SelItem::Note(id.clone())]);
    st.viewport.request_fit(Fit::Rect(rect));
}

#[cfg(test)]
mod tests {
    use super::super::testkit::{Driver, batches, state};
    use super::*;

    fn labels(rows: &[Row]) -> Vec<String> {
        rows.iter()
            .map(|r| match r {
                Row::Note { id, .. } => id.to_string(),
                Row::GuidesHeader(n) => format!("guides({n})"),
            })
            .collect()
    }

    #[test]
    fn the_filters_narrow_the_rows_and_guides_have_their_group() {
        let (_tmp, st) = state();
        let doc = st.doc.as_ref().unwrap();
        let crev = doc.level_ix("crevasse");
        let mut f = NotesState::default();
        // Open by default: the done path is out; guides at the end, closed.
        assert_eq!(
            labels(&build_rows(&f, doc, crev)),
            ["pinstatue2", "regioneast", "notecrev22", "guides(1)"]
        );
        f.guides_open = true;
        f.status = StatusFilter::All;
        assert_eq!(
            labels(&build_rows(&f, doc, crev)),
            [
                "pinstatue2",
                "regioneast",
                "notecrev22",
                "pathcrev22",
                "guides(1)",
                "guidestem2"
            ]
        );
        f.status = StatusFilter::Done;
        assert_eq!(
            labels(&build_rows(&f, doc, crev)),
            ["pathcrev22", "guides(1)", "guidestem2"]
        );
        f.status = StatusFilter::All;
        f.scope = ScopeFilter::ThisLevel;
        assert_eq!(
            labels(&build_rows(&f, doc, crev)),
            ["notecrev22", "pathcrev22"]
        );
        f.scope = ScopeFilter::World;
        f.kind = Some("pin".into());
        assert_eq!(labels(&build_rows(&f, doc, crev)), ["pinstatue2"]);
        f.kind = None;
        f.tags = vec!["layout".into()];
        assert_eq!(
            labels(&build_rows(&f, doc, crev)),
            ["guides(1)", "guidestem2"]
        );
        f.tags.clear();
        f.scope = ScopeFilter::All;
        f.search = "too easy".into();
        assert_eq!(labels(&build_rows(&f, doc, crev)), ["notecrev22"]);
        f.search = "STATUE".into();
        assert_eq!(labels(&build_rows(&f, doc, crev)), ["pinstatue2"]);
    }

    /// C5: frames with no edit and no filter change neither rebuild the
    /// rows nor recount the tags, and share the rows instead of copying
    /// them; an edit (a new rev) does both once.
    #[test]
    fn idle_frames_do_no_per_annotation_work() {
        let (_tmp, mut st) = state();
        let mut d = Driver::new(pane);
        d.frame(&mut st, vec![]);
        let rows = std::sync::Arc::clone(&st.notes.rows);
        let built = st.notes.builds;
        assert_eq!(built, (1, 1));
        for _ in 0..10 {
            d.frame(&mut st, vec![]);
        }
        assert_eq!(st.notes.builds, built);
        assert!(std::sync::Arc::ptr_eq(&rows, &st.notes.rows));
        st.notes.rev += 1;
        d.frame(&mut st, vec![]);
        d.frame(&mut st, vec![]);
        assert_eq!(st.notes.builds, (2, 2));
    }

    #[test]
    fn the_pane_hides_a_tag_selects_a_row_and_marks_one_done() {
        let (_tmp, mut st) = state();
        st.view.active_level = st.doc.as_ref().unwrap().level_ix("shaft");
        let mut d = Driver::new(pane);
        d.frame(&mut st, vec![]);
        d.frame(&mut st, vec![]);
        d.click(&mut st, d.rect("notes:view").center());
        assert!(st.notes.view_open);
        for _ in 0..4 {
            d.frame(&mut st, vec![]);
        }
        // The eye hides the tag on the map.
        d.click(&mut st, d.rect("notes:tag:landmark").center());
        assert!(st.view.annotations.hidden_tags.contains("landmark"));
        d.click(&mut st, d.rect("notes:tag:landmark").center());
        assert!(st.view.annotations.hidden_tags.is_empty());
        // A row click selects the note and makes its level active.
        d.click(&mut st, d.rect("notes:row:notecrev22").center());
        assert_eq!(
            st.view.selection.items,
            [SelItem::Note(Id::new("notecrev22").unwrap())]
        );
        assert_eq!(
            st.view.active_level,
            st.doc.as_ref().unwrap().level_ix("crevasse")
        );
        // The done box: one update_note of its status.
        d.click(&mut st, d.rect("notes:done:notecrev22").center());
        let b = batches(&mut st);
        assert_eq!(b.len(), 1, "{b:?}");
        assert_eq!(b[0].0, "Edit note notecrev22: status");
        assert!(
            matches!(&b[0].1[..], [Command::UpdateNote { note, .. }] if note.status == NoteStatus::Done)
        );
        // The guides group opens.
        d.click(&mut st, d.rect("notes:guides").center());
        assert!(st.notes.guides_open);
        d.frame(&mut st, vec![]);
        assert!(
            st.notes
                .rows()
                .iter()
                .any(|r| matches!(r, Row::Note { kind: "guide", .. }))
        );
    }
}
