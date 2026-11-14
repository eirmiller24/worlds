//! The History pane: the undo log as a list, oldest first. Each entry has
//! its label, who made it (you, an agent, or a file changed on disk), the
//! time it was recorded and the files it touched; an agent's batch made of
//! several writes expands to list them. Undone entries (the redo stack)
//! follow, faded. Clicking an entry undoes or redoes to just after it
//! ("Opened" goes back before every entry); that is
//! [`Action::HistoryJump`], the same steps as Ctrl+Z and Ctrl+Shift+Z.
//!
//! Each write of an expanded batch is a link: clicking it centers the map
//! on where that write landed ([`part_region`]; zooming out only if the
//! region doesn't fit), without stepping the history.
//!
//! The pane reads a snapshot of `worlds_core::History` that [`refresh`]
//! rebuilds when the history changed (the command layer's revision or a
//! stack length moved), so drawing it costs nothing per frame.

use std::collections::BTreeSet;
use std::time::SystemTime;

use egui::{Color32, RichText};
use worlds_core::History;
use worlds_core::command::{Change, Entry, Origin};

use super::EditorState;
use super::actions::Action;
use crate::doc::{Document, LevelIx};
use crate::viewport::Fit;
use crate::viewport::camera::WRect;

/// One entry, as the pane shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub label: String,
    pub origin: Origin,
    pub time: SystemTime,
    /// Project-relative, `/`-separated.
    pub files: Vec<String>,
    pub parts: Vec<String>,
    /// What each part changed, parallel to `parts` (empty when unknown).
    pub part_changes: Vec<Vec<Change>>,
    /// On the undo stack (done), else undone.
    pub done: bool,
}

impl Row {
    fn of(e: &Entry, done: bool) -> Self {
        Row {
            label: e.label.clone(),
            origin: e.origin.clone(),
            time: e.time,
            files: e
                .files
                .iter()
                .map(|f| {
                    f.iter()
                        .map(|c| c.to_string_lossy())
                        .collect::<Vec<_>>()
                        .join("/")
                })
                .collect(),
            parts: e.parts.clone(),
            part_changes: e.part_changes.clone(),
            done,
        }
    }

    /// Who made it, for the origin column.
    pub fn who(&self) -> String {
        match &self.origin {
            Origin::User => "you".into(),
            Origin::Agent(name) => name.clone(),
            Origin::Disk => "disk".into(),
        }
    }

    /// An agent's batch of several writes: it expands to list them.
    pub fn expandable(&self) -> bool {
        matches!(self.origin, Origin::Agent(_)) && self.parts.len() > 1
    }
}

/// The History pane's state.
#[derive(Debug, Default)]
pub struct HistoryState {
    /// Done entries oldest first, then undone ones in redo order.
    pub rows: Vec<Row>,
    /// How many of `rows` are done.
    pub done: usize,
    /// What `rows` was built from: (command layer revision, undo length,
    /// redo length).
    key: Option<(u64, usize, usize)>,
    /// Expanded agent batches, by row index (reset when the rows change).
    pub expanded: BTreeSet<usize>,
    /// Expand every agent batch (the `--script` step's `expand`).
    pub expand_all: bool,
}

/// Rebuild the snapshot if the history changed.
pub fn refresh(hs: &mut HistoryState, rev: u64, history: Option<&History>) {
    let Some(h) = history else {
        if !hs.rows.is_empty() {
            *hs = HistoryState::default();
        }
        return;
    };
    let key = (rev, h.undo_entries().len(), h.redo_entries().len());
    if hs.key == Some(key) {
        return;
    }
    hs.key = Some(key);
    let mut rows: Vec<Row> = h.undo_entries().iter().map(|e| Row::of(e, true)).collect();
    hs.done = rows.len();
    rows.extend(h.redo_entries().iter().rev().map(|e| Row::of(e, false)));
    if rows != hs.rows {
        hs.expanded.clear();
    }
    hs.rows = rows;
}

/// `HH:MM:SS` (UTC, as the Log pane shows times).
pub fn clock(t: SystemTime) -> String {
    let secs = t
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
        % 86400;
    format!("{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
}

/// How long ago, roughly ("12 s ago", "3 min ago", "2 h ago").
fn ago(t: SystemTime) -> String {
    let s = SystemTime::now()
        .duration_since(t)
        .map_or(0, |d| d.as_secs());
    if s < 60 {
        format!("{s} s ago")
    } else if s < 3600 {
        format!("{} min ago", s / 60)
    } else {
        format!("{} h ago", s / 3600)
    }
}

fn origin_color(o: &Origin, accent: Color32) -> Color32 {
    match o {
        Origin::User => Color32::from_gray(150),
        Origin::Agent(_) => accent,
        Origin::Disk => Color32::from_rgb(200, 170, 90),
    }
}

/// Where on the map a set of changes happened: the world (index into
/// `project.worlds`), the level when they are in one, and a rect of world
/// px around them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Region {
    pub world: usize,
    pub level: Option<LevelIx>,
    pub rect: WRect,
}

/// The level `level` of world `world`, by slug.
fn level_in(doc: &Document, world: &str, level: &str) -> Option<LevelIx> {
    (0..doc.levels.len()).find(|&ix| {
        let r = doc.levels[ix];
        doc.project.worlds[r.world].slug == world && doc.level(ix).slug == level
    })
}

fn level_wrect(doc: &Document, ix: LevelIx) -> WRect {
    let [x, y, w, h] = doc.level_rect(ix);
    WRect::from_pos_size([x, y], [w, h])
}

/// Where one change happened, as the document is now: painted cells, an
/// entity's box (its level when it is gone), an annotation's box, a level
/// (its metadata, layers or reference images). Definitions, settings and
/// worlds have no place on the map: `None`, as for a deleted level.
fn change_region(doc: &Document, c: &Change) -> Option<Region> {
    let in_level = |ix: LevelIx, rect: WRect| Region {
        world: doc.levels[ix].world,
        level: Some(ix),
        rect,
    };
    let whole = |world: &str, level: &str| {
        level_in(doc, world, level).map(|ix| in_level(ix, level_wrect(doc, ix)))
    };
    match c {
        Change::Grid {
            world,
            level,
            layer,
            rect,
        }
        | Change::Tiles {
            world,
            level,
            layer,
            rect,
        } => {
            let ix = level_in(doc, world.as_str(), level.as_str())?;
            let g = doc
                .project
                .layer(layer.as_str())
                .map_or(doc.project.grid_size.max(1), |d| doc.project.layer_grid(d))
                as f64;
            let o = doc.level(ix).world_px;
            Some(in_level(
                ix,
                WRect::from_pos_size(
                    [
                        o[0] as f64 + rect.x as f64 * g,
                        o[1] as f64 + rect.y as f64 * g,
                    ],
                    [rect.w as f64 * g, rect.h as f64 * g],
                ),
            ))
        }
        Change::Entity { world, level, id } | Change::EntityOutside { world, level, id } => {
            if let Some((ix, layer, inst)) = doc.find_entity(id)
                && let Some(def) = doc.project.entity_def(inst.ty.as_str())
            {
                let lg = doc
                    .project
                    .layer(layer.as_str())
                    .map_or(doc.project.grid_size.max(1), |d| doc.project.layer_grid(d));
                let [x, y, w, h] =
                    crate::tools::entity_box(def, inst, lg, doc.project.grid_size.max(1));
                let o = doc.level(ix).world_px;
                return Some(in_level(
                    ix,
                    WRect::from_pos_size(
                        [o[0] as f64 + x, o[1] as f64 + y],
                        [w.max(1.0), h.max(1.0)],
                    ),
                ));
            }
            whole(world.as_str(), level.as_str())
        }
        Change::Note { world, level, id } => {
            if let Some(r) = crate::annot::find_note(doc, id)
                && let Some(b) = crate::annot::world_bbox(doc, r)
            {
                return Some(Region {
                    world: crate::annot::note_world(doc, r.at),
                    level: r.level(),
                    rect: b,
                });
            }
            level
                .as_ref()
                .and_then(|l| whole(world.as_str(), l.as_str()))
        }
        Change::Reference {
            world,
            level: Some(level),
            ..
        } => whole(world.as_str(), level.as_str()),
        Change::LevelMeta { world, level, .. }
        | Change::LevelCreated { world, level }
        | Change::LayerState { world, level, .. }
        | Change::EntityLayer { world, level, .. }
        | Change::LevelRenamed {
            world, to: level, ..
        }
        | Change::LayerFileRenamed { world, level, .. } => whole(world.as_str(), level.as_str()),
        _ => None,
    }
}

/// Where a part (one write of a batch) happened: [`change_region`] of each
/// of its changes, joined (those in the first one's world), with some room
/// around it (as an annotation is framed: 30 % more, at least 8 cells).
/// `None` when nothing it changed is on the map.
pub fn part_region(doc: &Document, changes: &[Change]) -> Option<Region> {
    let mut out: Option<Region> = None;
    for r in changes.iter().filter_map(|c| change_region(doc, c)) {
        match &mut out {
            None => out = Some(r),
            Some(o) if o.world == r.world => {
                o.rect = o.rect.union(&r.rect);
                if o.level != r.level {
                    o.level = o.level.or(r.level);
                }
            }
            Some(_) => {}
        }
    }
    let mut out = out?;
    let min = 8.0 * doc.project.grid_size.max(1) as f64;
    let c = out.rect.center();
    let w = (out.rect.width() * 1.3).max(min);
    let h = (out.rect.height() * 1.3).max(min);
    out.rect = WRect::new(
        [c[0] - w / 2.0, c[1] - h / 2.0],
        [c[0] + w / 2.0, c[1] + h / 2.0],
    );
    Some(out)
}

/// Center the map on `r` (its world shown, its level active).
pub fn show_region(st: &mut EditorState, r: Region) {
    let Some(doc) = st.doc.as_ref() else {
        return;
    };
    st.view.set_world(doc, r.world);
    if let Some(ix) = r.level {
        st.view.active_level = Some(ix);
    }
    st.viewport.request_fit(Fit::Show(r.rect));
}

/// The History pane.
pub fn pane(ui: &mut egui::Ui, st: &mut EditorState) {
    if st.doc.is_none() {
        ui.weak("No project");
        return;
    }
    let accent = st.accent;
    let mut show: Option<Region> = None;
    let doc = st.doc.as_ref().expect("checked above");
    let hs = &mut st.history;
    let n = hs.rows.len();
    ui.horizontal(|ui| {
        ui.label(format!("{} done, {} undone", hs.done, n - hs.done));
        ui.weak("· click an entry to go back (or forward) to just after it");
    });
    ui.separator();
    let mut jump: Option<usize> = None;
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .stick_to_bottom(true)
        .show(ui, |ui| {
            // Before every entry: the project as opened (or reloaded).
            let r = ui
                .selectable_label(hs.done == 0, RichText::new("Opened").italics())
                .on_hover_text("Undo everything in the list");
            super::fields::mark(ui, "history:0", r.rect);
            if r.clicked() && hs.done != 0 {
                jump = Some(0);
            }
            for i in 0..n {
                let row = &hs.rows[i];
                let current = i + 1 == hs.done;
                let mut label = RichText::new(&row.label);
                if !row.done {
                    label = label.weak().italics();
                }
                let open = hs.expand_all || hs.expanded.contains(&i);
                let mut toggle = false;
                let resp = ui
                    .horizontal(|ui| {
                        if row.expandable() {
                            let arrow = if open { "⏷" } else { "⏵" };
                            let a = ui
                                .small_button(arrow)
                                .on_hover_text("Show the batch's writes");
                            super::fields::mark(ui, &format!("history:{}:expand", i + 1), a.rect);
                            if a.clicked() {
                                toggle = true;
                            }
                        } else {
                            ui.add_space(ui.spacing().interact_size.y);
                        }
                        ui.label(
                            RichText::new(clock(row.time))
                                .monospace()
                                .color(ui.visuals().weak_text_color()),
                        )
                        .on_hover_text(ago(row.time));
                        ui.label(
                            RichText::new(row.who())
                                .small()
                                .color(origin_color(&row.origin, accent)),
                        );
                        let r = ui.selectable_label(current, label);
                        if !row.files.is_empty() {
                            ui.weak(if row.files.len() == 1 {
                                "1 file".to_string()
                            } else {
                                format!("{} files", row.files.len())
                            });
                        }
                        r
                    })
                    .inner;
                let tip = if row.files.is_empty() {
                    String::new()
                } else {
                    format!("\n\nFiles:\n{}", row.files.join("\n"))
                };
                let resp = resp.on_hover_text(format!(
                    "{} by {}{tip}\n\nClick to {} to just after it",
                    row.label,
                    row.who(),
                    if row.done { "undo" } else { "redo" }
                ));
                super::fields::mark(ui, &format!("history:{}", i + 1), resp.rect);
                if resp.clicked() && !current {
                    jump = Some(i + 1);
                }
                if toggle && !hs.expanded.remove(&i) {
                    hs.expanded.insert(i);
                }
                if open && row.expandable() {
                    ui.indent(("history parts", i), |ui| {
                        for (j, p) in row.parts.iter().enumerate() {
                            let region = row.part_changes.get(j).and_then(|c| part_region(doc, c));
                            let text = RichText::new(format!("· {p}"));
                            let r = match region {
                                Some(_) => ui
                                    .link(text)
                                    .on_hover_text("Center the map on where this happened"),
                                None => ui.weak(format!("· {p}")).on_hover_text(
                                    "Nothing on the map to show (a definition, a setting, \
                                     or something no longer there)",
                                ),
                            };
                            super::fields::mark(ui, &format!("history:{}:part:{j}", i + 1), r.rect);
                            if r.clicked()
                                && let Some(g) = region
                            {
                                show = Some(g);
                            }
                        }
                        for f in &row.files {
                            ui.label(RichText::new(f).small().monospace());
                        }
                    });
                }
            }
        });
    if let Some(to) = jump {
        st.actions.push(Action::HistoryJump(to));
    }
    if let Some(r) = show {
        show_region(st, r);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use worlds_core::command::NoteScope;
    use worlds_core::{Command, Reference};

    #[test]
    fn rows_follow_the_history_and_jump_targets() {
        let mut p = worlds_core::load(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../worlds-core/tests/fixtures/mini.worlds"),
        )
        .unwrap();
        let mut h = History::new();
        let add = |x: &str| Command::AddReference {
            scope: NoteScope::Level(worlds_core::slug("crevasse")),
            reference: Reference::new(worlds_core::Id::new(x).unwrap(), "a.png", [0, 0]),
            index: None,
        };
        h.push(&mut p, add("refaaaaaa2")).unwrap();
        h.push(&mut p, add("refbbbbbb2")).unwrap();
        h.undo(&mut p).unwrap();
        let mut hs = HistoryState::default();
        refresh(&mut hs, 1, Some(&h));
        assert_eq!(hs.rows.len(), 2);
        assert_eq!(hs.done, 1);
        assert!(hs.rows[0].done && !hs.rows[1].done);
        assert_eq!(hs.rows[0].who(), "you");
        assert_eq!(hs.rows[0].files, ["worlds/main/levels/crevasse/level.json"]);
        assert!(!hs.rows[0].expandable());
        assert_eq!(clock(SystemTime::UNIX_EPOCH), "00:00:00");
        // A level copy lists the new level's directory.
        h.push(
            &mut p,
            Command::DuplicateLevel {
                level: worlds_core::slug("crevasse"),
                slug: worlds_core::slug("crevasse_b"),
                name: None,
                world: None,
                world_px: None,
            },
        )
        .unwrap();
        refresh(&mut hs, 2, Some(&h));
        assert_eq!(hs.rows.len(), 2, "the new entry dropped the undone one");
        assert_eq!(hs.rows[1].label, "Duplicate level crevasse as crevasse_b");
        assert_eq!(hs.rows[1].files, ["worlds/main/levels/crevasse_b"]);
    }

    #[test]
    fn clicking_an_entry_jumps_to_it_and_agent_batches_expand() {
        use super::super::testkit::{Driver, state};
        let (_t, mut st) = state();
        let mut h = History::new();
        let doc = st.doc.as_mut().unwrap();
        let add = |x: &str| Command::AddReference {
            scope: NoteScope::Level(worlds_core::slug("crevasse")),
            reference: Reference::new(worlds_core::Id::new(x).unwrap(), "a.png", [0, 0]),
            index: None,
        };
        h.push(&mut doc.project, add("refaaaaaa2")).unwrap();
        // An agent's batch of two writes, recorded as the server does.
        let a = doc.project.apply(add("refbbbbbb2")).unwrap();
        let b = doc.project.apply(add("refcccccc2")).unwrap();
        let changes: Vec<_> = a.changes.iter().chain(&b.changes).cloned().collect();
        h.record_with(
            "claude: two sketches",
            Origin::Agent("claude".into()),
            vec![a.inverse, b.inverse],
            vec!["Add reference image".into(), "Add reference image".into()],
            &changes,
        );
        refresh(&mut st.history, 7, Some(&h));
        assert!(st.history.rows[1].expandable());
        assert_eq!(st.history.rows[1].who(), "claude");
        let mut d = Driver::new(pane);
        d.frame(&mut st, vec![]);
        d.frame(&mut st, vec![]);
        // The first entry: undo the batch.
        d.click(&mut st, d.rect("history:1").center());
        assert_eq!(st.actions, [Action::HistoryJump(1)]);
        st.actions.clear();
        // "Opened": undo everything.
        d.click(&mut st, d.rect("history:0").center());
        assert_eq!(st.actions, [Action::HistoryJump(0)]);
        st.actions.clear();
        // The current entry: nothing to do.
        d.click(&mut st, d.rect("history:2").center());
        assert!(st.actions.is_empty());
        // The batch opens to list its writes.
        d.click(&mut st, d.rect("history:2:expand").center());
        assert!(st.history.expanded.contains(&1));
    }

    #[test]
    fn clicking_a_batch_write_centers_the_map_on_it() {
        use super::super::testkit::{Driver, state};
        use worlds_core::command::{DefKind, Rect};
        let (_t, mut st) = state();
        st.view.active_level = None;
        let doc = st.doc.as_mut().unwrap();
        let mut h = History::new();
        let a = doc.project.apply(add_ref("refaaaaaa2")).unwrap();
        let b = doc.project.apply(add_ref("refbbbbbb2")).unwrap();
        // What the server keeps per write: a painted rect in crevasse, and
        // a definition (nothing on the map).
        let painted = Change::Grid {
            world: worlds_core::slug("main"),
            level: worlds_core::slug("crevasse"),
            layer: worlds_core::slug("collision"),
            rect: Rect::new(2, 1, 3, 2),
        };
        let def = Change::Def {
            kind: DefKind::Enum,
            slug: worlds_core::slug("biome"),
        };
        assert!(h.record_parts(
            "claude: paint and define",
            Origin::Agent("claude".into()),
            vec![a.inverse, b.inverse],
            vec![
                ("Paint collision".into(), vec![painted.clone()]),
                ("Set enum biome".into(), vec![def.clone()]),
            ],
        ));
        assert_eq!(h.undo_entries()[0].part_changes.len(), 2);
        // Cells 2..5 x 1..3 of a 16 px grid in a level at the origin.
        let doc = st.doc.as_ref().unwrap();
        let r = change_region(doc, &painted).unwrap();
        assert_eq!(r.level, doc.level_ix("crevasse"));
        assert_eq!(r.rect, WRect::from_pos_size([32.0, 16.0], [48.0, 32.0]));
        assert_eq!(part_region(doc, &[def]), None);
        refresh(&mut st.history, 1, Some(&h));
        st.history.expand_all = true;
        let mut d = Driver::new(pane);
        d.frame(&mut st, vec![]);
        d.frame(&mut st, vec![]);
        d.click(&mut st, d.rect("history:1:part:0").center());
        assert!(
            st.actions.is_empty(),
            "showing a write doesn't step the history"
        );
        assert_eq!(
            st.view.active_level,
            st.doc.as_ref().unwrap().level_ix("crevasse")
        );
        let Some(Fit::Show(shown)) = st.viewport.pending_fit() else {
            panic!("no Show fit: {:?}", st.viewport.pending_fit());
        };
        assert_eq!(shown.center(), [56.0, 32.0]);
        assert!(shown.width() >= 8.0 * 16.0);
        // The definition has nowhere to go.
        st.viewport.request_fit(Fit::World);
        d.click(&mut st, d.rect("history:1:part:1").center());
        assert_eq!(st.viewport.pending_fit(), Some(Fit::World));
    }

    fn add_ref(x: &str) -> Command {
        Command::AddReference {
            scope: NoteScope::Level(worlds_core::slug("crevasse")),
            reference: Reference::new(worlds_core::Id::new(x).unwrap(), "a.png", [0, 0]),
            index: None,
        }
    }

    #[test]
    fn old_layouts_get_the_history_pane_next_to_the_log() {
        use super::super::{Pane, add_history_pane, default_dock};
        use egui_dock::{DockState, NodeIndex};
        let mut old = DockState::new(vec![Pane::Viewport(crate::view::ViewportId::FIRST)]);
        let s = old.main_surface_mut();
        let [center, _] = s.split_left(NodeIndex::root(), 0.2, vec![Pane::Layers]);
        s.split_below(center, 0.8, vec![Pane::Log, Pane::Rules]);
        add_history_pane(&mut old);
        add_history_pane(&mut old);
        let log = old.find_tab(&Pane::Log).unwrap();
        let h = old.find_tab(&Pane::History).unwrap();
        assert_eq!(log.node_path(), h.node_path());
        assert_eq!(
            old.iter_all_tabs()
                .filter(|(_, t)| **t == Pane::History)
                .count(),
            1
        );
        // No Log or Rules: a split under the map.
        let mut bare = DockState::new(vec![Pane::Viewport(crate::view::ViewportId::FIRST)]);
        add_history_pane(&mut bare);
        assert!(bare.find_tab(&Pane::History).is_some());
        assert!(default_dock().find_tab(&Pane::History).is_some());
    }
}
