//! The Find pane (`Pane::Find`, tab "Find", next to Problems and Notes):
//! project-wide search with the query language of [`worlds_core::find`]
//! (the API's `entity.find`, `level.find`, `tiles.find`, `notes.list` and
//! the MCP `find` tool read the same queries).
//!
//! One query box and a kind picker (All, Entities, Levels, Notes, Tiles).
//! "All" runs every kind the query can match (and nothing for an empty
//! query, which would list every tile); one kind with an empty query lists
//! all of that kind. Each result is a row with its world and level and a
//! one-line preview; a click selects it and frames its level (which, in a
//! project with several worlds, also switches to that level's world).
//! **Select all** puts every result in the selection, so an Inspector
//! edit or Delete applies to all of them. **Replace** sets one field to one
//! value on every entity result whose type has it, as one undo entry
//! ("Replace channel in 12 entities", [`worlds_core::find::replace_field`]).
//!
//! The rows are rebuilt only when the project or the query changes (the
//! Notes pane's pattern) and drawn with `show_rows`.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use egui::{Color32, RichText};
use worlds_core::find::{self, FindKind, Query};
use worlds_core::{Id, Shape, Slug};

use super::EditorState;
use super::fields::mark;
use crate::doc::{Document, LevelIx};
use crate::view::SelItem;
use crate::viewport::Fit;

/// Rows built at most (a query matching more says how many it left out).
pub const MAX_ROWS: usize = 10_000;

/// What a row selects.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Target {
    Entity(Id),
    Level(Id),
    Note(Id),
    Tiles {
        level: Id,
        layer: Slug,
        cell: [u32; 2],
    },
}

impl Target {
    pub fn sel_item(&self) -> SelItem {
        match self {
            Target::Entity(id) => SelItem::Entity(id.clone()),
            Target::Level(id) => SelItem::Level(id.clone()),
            Target::Note(id) => SelItem::Note(id.clone()),
            Target::Tiles { level, layer, cell } => SelItem::Tiles {
                level: level.clone(),
                layer: layer.clone(),
                cell: *cell,
            },
        }
    }
}

/// One result.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub kind: FindKind,
    pub target: Target,
    /// `world / level`, or `world` for a world-scope annotation.
    pub place: String,
    pub preview: String,
    /// The level it is in.
    pub level: Option<LevelIx>,
}

/// What the last build found.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Found {
    pub rows: Vec<Row>,
    /// Matches in all (rows stop at [`MAX_ROWS`]).
    pub total: usize,
    /// The query didn't parse, or the kind doesn't take it.
    pub error: Option<String>,
}

/// The pane's state.
#[derive(Default)]
pub struct FindState {
    pub query: String,
    /// `None`: every kind the query applies to.
    pub kind: Option<FindKind>,
    /// Replace: the field and the value text.
    pub replace_field: Option<String>,
    pub replace_value: String,
    /// What the last Replace or Select all did, or why it couldn't.
    pub notice: Option<(String, bool)>,
    /// The edit revision (`EditorApp::frame` sets it each frame).
    pub rev: u64,
    found: Arc<Found>,
    built: Option<(u64, String, Option<FindKind>)>,
    /// Rebuilds so far (tests: bounded work).
    pub builds: usize,
    /// Give the query box the keyboard (Ctrl+F).
    pub focus_query: bool,
    /// Bring the pane to the front.
    pub focus: bool,
}

impl FindState {
    /// The results as of the last build.
    pub fn found(&self) -> &Found {
        &self.found
    }

    /// Rebuild the results when the project or the query changed.
    pub fn refresh(&mut self, doc: &Document) {
        let key = (self.rev, self.query.clone(), self.kind);
        if self.built.as_ref() == Some(&key) {
            return;
        }
        self.found = Arc::new(search(doc, &self.query, self.kind));
        self.built = Some(key);
        self.builds += 1;
    }
}

/// Run `query` over the project: one kind, or (`None`) every kind it
/// applies to, entities first.
pub fn search(doc: &Document, query: &str, kind: Option<FindKind>) -> Found {
    let q = match Query::parse(query) {
        Ok(q) => q,
        Err(e) => {
            return Found {
                error: Some(e),
                ..Default::default()
            };
        }
    };
    let kinds: Vec<FindKind> = match kind {
        Some(k) => {
            if let Err(e) = q.check(k) {
                return Found {
                    error: Some(e),
                    ..Default::default()
                };
            }
            vec![k]
        }
        None if q.is_empty() => return Found::default(),
        None => FindKind::ALL
            .into_iter()
            .filter(|k| q.applies(*k))
            .filter(|k| *k != FindKind::Tiles || !q.tilesets.is_empty() || !q.tiles.is_empty())
            .collect(),
    };
    let mut out = Found::default();
    for k in kinds {
        rows_of(doc, &q, k, &mut out);
    }
    out
}

fn push(out: &mut Found, row: impl FnOnce() -> Row) {
    out.total += 1;
    if out.rows.len() < MAX_ROWS {
        out.rows.push(row());
    }
}

/// Up to three `field=value` pairs, stored ones only.
fn fields_text(fields: &BTreeMap<Slug, serde_json::Value>) -> String {
    let mut parts: Vec<String> = fields
        .iter()
        .take(3)
        .map(|(k, v)| {
            let v = match v {
                serde_json::Value::String(s) => s.clone(),
                o => o.to_string(),
            };
            let v = if v.chars().count() > 24 {
                format!("{}…", v.chars().take(23).collect::<String>())
            } else {
                v
            };
            format!("{k}={v}")
        })
        .collect();
    if fields.len() > 3 {
        parts.push("…".into());
    }
    parts.join(" ")
}

fn rows_of(doc: &Document, q: &Query, kind: FindKind, out: &mut Found) {
    let p = &doc.project;
    let place = |w: &worlds_core::World, l: Option<&worlds_core::Level>| match l {
        Some(l) => format!("{} / {}", w.slug, l.slug),
        None => w.slug.to_string(),
    };
    match kind {
        FindKind::Entities => {
            for h in find::entities(p, q).unwrap_or_default() {
                push(out, || {
                    let e = h.entity;
                    let mut preview =
                        format!("{} {} at [{}, {}]", e.ty, e.id, e.cell[0], e.cell[1]);
                    let f = fields_text(&e.fields);
                    if !f.is_empty() {
                        preview.push_str("  ");
                        preview.push_str(&f);
                    }
                    Row {
                        kind,
                        target: Target::Entity(e.id.clone()),
                        place: place(h.world, Some(h.level)),
                        preview,
                        level: doc.level_ix_by_id(&h.level.id),
                    }
                });
            }
        }
        FindKind::Levels => {
            for h in find::levels(p, q).unwrap_or_default() {
                push(out, || {
                    let l = h.level;
                    let mut preview =
                        format!("{} ({}) {}x{}", l.name, l.slug, l.size[0], l.size[1]);
                    if !l.tags.is_empty() {
                        preview.push_str(&format!("  tags {}", l.tags.join(", ")));
                    }
                    let f = fields_text(&l.fields);
                    if !f.is_empty() {
                        preview.push_str("  ");
                        preview.push_str(&f);
                    }
                    Row {
                        kind,
                        target: Target::Level(l.id.clone()),
                        place: place(h.world, Some(l)),
                        preview,
                        level: doc.level_ix_by_id(&l.id),
                    }
                });
            }
        }
        FindKind::Notes => {
            for h in find::notes(p, q).unwrap_or_default() {
                push(out, || {
                    let n = h.note;
                    let label = match &n.shape {
                        Shape::Pin { label, .. } | Shape::Connector { label, .. } => {
                            label.as_deref()
                        }
                        _ => None,
                    };
                    let text = n
                        .title
                        .as_deref()
                        .or(label)
                        .or_else(|| n.body.as_deref().and_then(|b| b.lines().next()))
                        .unwrap_or("");
                    let mut preview = format!("{} {}  {text}", n.shape.name(), n.id);
                    if n.status == worlds_core::NoteStatus::Done {
                        preview.push_str("  (done)");
                    }
                    Row {
                        kind,
                        target: Target::Note(n.id.clone()),
                        place: place(h.world, h.level),
                        preview,
                        level: h.level.and_then(|l| doc.level_ix_by_id(&l.id)),
                    }
                });
            }
        }
        FindKind::Tiles => {
            for h in find::tiles(p, q).unwrap_or_default() {
                push(out, || {
                    let t = h.tile;
                    Row {
                        kind,
                        target: Target::Tiles {
                            level: h.level.id.clone(),
                            layer: h.layer.clone(),
                            cell: t.cell,
                        },
                        place: place(h.world, Some(h.level)),
                        preview: format!(
                            "{} [{}, {}] at [{}, {}] in {}",
                            t.tileset, t.tile[0], t.tile[1], t.cell[0], t.cell[1], h.layer
                        ),
                        level: doc.level_ix_by_id(&h.level.id),
                    }
                });
            }
        }
    }
}

/// Select one result and frame its level (an annotation: its own box).
pub fn jump(st: &mut EditorState, row: &Row) {
    if let Target::Note(id) = &row.target {
        super::notes::jump(st, id);
        return;
    }
    st.view.selection.set([row.target.sel_item()]);
    if let Some(ix) = row.level {
        st.view.active_level = Some(ix);
        st.viewport.request_fit(Fit::Level(ix));
    }
}

/// Put every result in the selection (the first primary); the first
/// result's level becomes active. Returns how many items it selected.
pub fn select_all(st: &mut EditorState) -> usize {
    let found = Arc::clone(&st.find.found);
    let mut seen = HashSet::new();
    let items: Vec<SelItem> = found
        .rows
        .iter()
        .filter(|r| seen.insert(r.target.clone()))
        .map(|r| r.target.sel_item())
        .collect();
    let n = items.len();
    st.view.selection.items = items;
    if let Some(ix) = found.rows.iter().find_map(|r| r.level) {
        st.view.active_level = Some(ix);
    }
    n
}

/// The fields the entity results' types have, each with how many results
/// have it, by slug.
pub fn replace_fields(doc: &Document, found: &Found) -> Vec<(String, usize)> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for r in &found.rows {
        let Target::Entity(id) = &r.target else {
            continue;
        };
        let Some((_, _, e)) = doc.find_entity(id) else {
            continue;
        };
        if let Some(def) = doc.project.entity_def(e.ty.as_str()) {
            for f in &def.fields {
                *counts.entry(f.slug.to_string()).or_default() += 1;
            }
        }
    }
    counts.into_iter().collect()
}

/// Queue Replace: `field` set to `text` (read for the field's type,
/// [`find::replace_value`]) on every entity result that has it, one batch.
pub fn replace(st: &mut EditorState, field: &str, text: &str) -> Result<usize, String> {
    let doc = st.doc.as_ref().ok_or("no project open")?;
    let field = Slug::new(field).map_err(|e| format!("field {field:?}: {e}"))?;
    let ids: Vec<Id> = st
        .find
        .found
        .rows
        .iter()
        .filter_map(|r| match &r.target {
            Target::Entity(id) => Some(id.clone()),
            _ => None,
        })
        .collect();
    if ids.is_empty() {
        return Err("Replace works on entity results; there are none".into());
    }
    // The value is read for the first result's type that has the field.
    let def = ids
        .iter()
        .filter_map(|id| doc.find_entity(id))
        .filter_map(|(_, _, e)| doc.project.entity_def(e.ty.as_str()))
        .find_map(|d| d.field(field.as_str()))
        .ok_or_else(|| format!("no result's type has a field {field}"))?;
    let value = find::replace_value(def, text);
    let (cmd, n) = find::replace_field(&doc.project, &ids, &field, &value)
        .ok_or_else(|| format!("no result's type has a field {field}"))?;
    st.pending.push(cmd);
    Ok(n)
}

const KINDS: [(Option<FindKind>, &str); 5] = [
    (None, "All"),
    (Some(FindKind::Entities), "Entities"),
    (Some(FindKind::Levels), "Levels"),
    (Some(FindKind::Notes), "Notes"),
    (Some(FindKind::Tiles), "Tiles"),
];

fn kind_label(k: FindKind) -> &'static str {
    match k {
        FindKind::Entities => "entity",
        FindKind::Levels => "level",
        FindKind::Notes => "note",
        FindKind::Tiles => "tile",
    }
}

/// The Find pane.
pub fn pane(ui: &mut egui::Ui, st: &mut EditorState) {
    let Some(doc) = st.doc.as_ref() else {
        ui.weak("No project");
        return;
    };
    // ---- query row ----
    ui.horizontal(|ui| {
        let r = ui.add(
            egui::TextEdit::singleline(&mut st.find.query)
                .hint_text("type:door channel:3, tag:puzzle, field:target = null, text")
                .desired_width((ui.available_width() - 110.0).max(120.0)),
        );
        mark(ui, "find:query", r.rect);
        if std::mem::take(&mut st.find.focus_query) {
            r.request_focus();
        }
        let label = KINDS
            .iter()
            .find(|(k, _)| *k == st.find.kind)
            .map_or("All", |(_, l)| l);
        let c = egui::ComboBox::from_id_salt("find kind")
            .selected_text(label)
            .show_ui(ui, |ui| {
                for (k, l) in KINDS {
                    ui.selectable_value(&mut st.find.kind, k, l);
                }
            });
        mark(ui, "find:kind", c.response.rect);
    });
    st.find.refresh(doc);
    let found = Arc::clone(&st.find.found);
    if let Some(e) = &found.error {
        ui.colored_label(Color32::from_rgb(230, 110, 100), e);
        return;
    }
    if st.find.query.trim().is_empty() && st.find.kind.is_none() {
        ui.weak("Search every world: type:door channel:3, tag:puzzle, field:target = null, kind:pin, tileset:castle tile:4,2, or plain words. Pick a kind to list all of it.");
        return;
    }
    // ---- summary and actions ----
    let mut select = false;
    ui.horizontal_wrapped(|ui| {
        let shown = if found.total > found.rows.len() {
            format!("{} results (first {} shown)", found.total, found.rows.len())
        } else if found.total == 1 {
            "1 result".to_string()
        } else {
            format!("{} results", found.total)
        };
        ui.label(shown);
        let r = ui
            .add_enabled(!found.rows.is_empty(), egui::Button::new("Select all"))
            .on_hover_text("Select every result, so an Inspector edit or Delete applies to all");
        mark(ui, "find:select_all", r.rect);
        select = r.clicked();
    });
    // ---- replace (entity results) ----
    let fields = replace_fields(doc, &found);
    let mut do_replace = None;
    if !fields.is_empty() {
        ui.horizontal_wrapped(|ui| {
            ui.label("Replace");
            if st
                .find
                .replace_field
                .as_ref()
                .is_none_or(|f| !fields.iter().any(|(s, _)| s == f))
            {
                st.find.replace_field = None;
            }
            let c = egui::ComboBox::from_id_salt("find replace field")
                .selected_text(st.find.replace_field.as_deref().unwrap_or("field…"))
                .show_ui(ui, |ui| {
                    for (f, n) in &fields {
                        ui.selectable_value(
                            &mut st.find.replace_field,
                            Some(f.clone()),
                            format!("{f} ({n})"),
                        );
                    }
                });
            mark(ui, "find:replace_field", c.response.rect);
            ui.label("with");
            let r = ui.add(
                egui::TextEdit::singleline(&mut st.find.replace_value)
                    .hint_text("value")
                    .desired_width(90.0),
            );
            mark(ui, "find:replace_value", r.rect);
            let n = st
                .find
                .replace_field
                .as_ref()
                .and_then(|f| fields.iter().find(|(s, _)| s == f))
                .map_or(0, |(_, n)| *n);
            let b = ui
                .add_enabled(n > 0, egui::Button::new(format!("Replace in {n}")))
                .on_hover_text("Set this field on every entity result that has it: one undo entry");
            mark(ui, "find:replace", b.rect);
            if b.clicked()
                && let Some(f) = st.find.replace_field.clone()
            {
                do_replace = Some(f);
            }
        });
    }
    if let Some((msg, error)) = &st.find.notice {
        let c = if *error {
            Color32::from_rgb(230, 110, 100)
        } else {
            Color32::from_gray(170)
        };
        ui.colored_label(c, msg);
    }
    ui.separator();
    // ---- results ----
    let row_h = ui.spacing().interact_size.y + 2.0;
    let mut clicked: Option<usize> = None;
    let selected: HashSet<SelItem> = st.view.selection.items.iter().cloned().collect();
    egui::ScrollArea::vertical()
        .id_salt("find rows")
        .auto_shrink([false, false])
        .show_rows(ui, row_h, found.rows.len(), |ui, range| {
            for i in range {
                let row = &found.rows[i];
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(kind_label(row.kind))
                            .small()
                            .color(Color32::from_gray(150)),
                    );
                    ui.weak(&row.place);
                    let on = selected.contains(&row.target.sel_item());
                    let r = ui.add(egui::Button::selectable(on, row.preview.as_str()));
                    mark(ui, &format!("find:row:{i}"), r.rect);
                    if r.clicked() {
                        clicked = Some(i);
                    }
                });
            }
        });
    if let Some(i) = clicked {
        let row = found.rows[i].clone();
        jump(st, &row);
    }
    if select {
        let n = select_all(st);
        st.find.notice = Some((format!("Selected {n}"), false));
    }
    if let Some(f) = do_replace {
        let text = st.find.replace_value.clone();
        st.find.notice = Some(match replace(st, &f, &text) {
            Ok(n) => (
                format!(
                    "Replaced {f} in {n} {}",
                    if n == 1 { "entity" } else { "entities" }
                ),
                false,
            ),
            Err(e) => (e, true),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::super::testkit::{Driver, batches, state};
    use super::*;
    use worlds_core::Command;

    fn targets(f: &Found) -> Vec<String> {
        f.rows
            .iter()
            .map(|r| match &r.target {
                Target::Entity(id) | Target::Level(id) | Target::Note(id) => id.to_string(),
                Target::Tiles { cell, .. } => format!("tile{cell:?}"),
            })
            .collect()
    }

    #[test]
    fn search_runs_each_kind_the_query_applies_to() {
        let (_tmp, st) = state();
        let doc = st.doc.as_ref().unwrap();
        // All, empty: nothing (it would list every tile).
        assert_eq!(search(doc, "", None), Found::default());
        // All, plain text: entities, levels and notes, not tiles.
        let f = search(doc, "shaft", None);
        assert_eq!(targets(&f), ["doorshaft2", "shaft22222"]);
        assert_eq!(f.rows[0].place, "main / shaft");
        assert!(
            f.rows[0]
                .preview
                .starts_with("locked_door doorshaft2 at [2, 7]")
        );
        // A tag: entities (by type), levels and notes.
        let f = search(doc, "tag:todo", None);
        assert_eq!(targets(&f), ["notecrev22"]);
        // tileset: only tiles.
        let f = search(doc, "tileset:castle_main", None);
        assert_eq!(targets(&f), ["tile[4, 2]", "tile[5, 2]"]);
        assert_eq!(
            f.rows[1].preview,
            "castle_main [6, 1] at [5, 2] in decor_back"
        );
        // One kind, empty: all of it.
        assert_eq!(search(doc, "", Some(FindKind::Levels)).total, 2);
        assert_eq!(search(doc, "", Some(FindKind::Tiles)).total, 3);
        // A word the kind doesn't take, or a bad query: the error.
        let f = search(doc, "type:door", Some(FindKind::Notes));
        assert_eq!(f.error.as_deref(), Some("type: is for entities, not notes"));
        assert!(search(doc, "kind:door", None).error.is_some());
        // A world note: its place is the world.
        let f = search(doc, "kind:pin", None);
        assert_eq!(f.rows[0].place, "main");
        assert_eq!(f.rows[0].level, None);
    }

    #[test]
    fn idle_frames_do_not_search_again() {
        let (_tmp, mut st) = state();
        st.find.query = "tag:puzzle".into();
        let mut d = Driver::new(pane);
        d.frame(&mut st, vec![]);
        d.frame(&mut st, vec![]);
        assert_eq!(st.find.builds, 1);
        st.find.rev += 1;
        d.frame(&mut st, vec![]);
        assert_eq!(st.find.builds, 2);
    }

    #[test]
    fn click_jumps_select_all_selects_and_replace_is_one_batch() {
        let (_tmp, mut st) = state();
        st.find.query = "type:*door".into();
        let mut d = Driver::new(pane);
        d.frame(&mut st, vec![]);
        d.frame(&mut st, vec![]);
        // The second door is in shaft: a click selects it there.
        d.click(&mut st, d.rect("find:row:1").center());
        assert_eq!(
            st.view.selection.items,
            [SelItem::Entity(Id::new("doorshaft2").unwrap())]
        );
        assert_eq!(
            st.view.active_level,
            st.doc.as_ref().unwrap().level_ix("shaft")
        );
        d.click(&mut st, d.rect("find:select_all").center());
        assert_eq!(st.view.selection.entity_ids().len(), 2);
        assert_eq!(
            st.view.active_level,
            st.doc.as_ref().unwrap().level_ix("crevasse")
        );
        // Replace channel with 5 on both doors: one entry.
        st.find.replace_field = Some("channel".into());
        st.find.replace_value = "5".into();
        d.frame(&mut st, vec![]);
        d.click(&mut st, d.rect("find:replace").center());
        let b = batches(&mut st);
        assert_eq!(b.len(), 1, "{b:?}");
        assert_eq!(b[0].0, "Replace channel in 2 entities");
        assert!(b[0].1.iter().all(|c| matches!(c,
            Command::SetEntityField { field, value: Some(v), .. }
                if field.as_str() == "channel" && *v == serde_json::json!(5))));
    }

    #[test]
    fn replace_reads_the_value_for_the_field_type() {
        let (_tmp, mut st) = state();
        st.find.query = "type:locked_door".into();
        let doc = st.doc.as_ref().unwrap();
        st.find.refresh(doc);
        assert_eq!(replace(&mut st, "needs_item", "3"), Ok(2));
        let b = batches(&mut st);
        assert!(matches!(&b[0].1[0],
            Command::SetEntityField { value: Some(v), .. } if *v == serde_json::json!("3")));
        assert!(replace(&mut st, "latching", "true").is_err());
        st.find.query = "kind:pin".into();
        let doc = st.doc.as_ref().unwrap();
        st.find.refresh(doc);
        assert!(replace(&mut st, "channel", "1").is_err());
    }
}
