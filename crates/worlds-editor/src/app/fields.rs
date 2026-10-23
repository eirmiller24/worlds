//! Field value editors, shared by the entity inspector (one entity or
//! several), the level inspector and the Types pane (field defaults).
//!
//! One editor per field type, each committing once per edit (never per
//! keystroke or per frame of a drag), so an edit is one undo entry:
//!
//! - int, float, string, path: a text box, committed on Enter or when it
//!   loses focus; a value that doesn't parse is shown under it, nothing
//!   is sent;
//! - text and json: a multiline box, committed on Ctrl+Enter or when it
//!   loses focus (json is parsed then, with the error shown inline);
//! - bool: a checkbox; color: `panels::color_field` (committed when the picker
//!   closes); enum: a combo of the values with their color and icon;
//! - point: x and y, plus Pick (the next click on the map sets the cell;
//!   Escape cancels);
//! - entity_ref: the id, plus Pick (the next click on an entity, in any
//!   level, the world view too); `type @ level` beside it, red when no
//!   entity has that id;
//! - tile: the tileset and an atlas popup to click a tile in;
//! - arrays: an editor per element with ↑ ↓ ×, and + (for points and
//!   references + picks the new element); every change sends the whole
//!   array.
//!
//! [`field_row`] adds the nullable controls: `×` removes the stored value
//! (the default applies) and `null` stores `null` (nullable fields only). A
//! multi-select shows "—" for a value that differs between the selected
//! entities; an edit sets it on all of them.

use std::collections::HashMap;

use egui::{Color32, RichText, Sense, Vec2};
use serde_json::Value;
use worlds_core::{EnumSource, FieldDef, FieldType};

use crate::doc::{Document, LevelIx};
use crate::view::{PickAccept, PickedThing, ViewState};
use crate::viewport::Atlas;

/// What an editor asks for.
#[derive(Debug, Clone, PartialEq)]
pub enum FieldEdit {
    /// Store this value (`null` included: the `null` button).
    Set(Value),
    /// Remove the stored value (the `×` button): the default applies.
    Remove,
}

/// What the editors read and write besides the value.
pub struct FieldCx<'a> {
    pub doc: &'a Document,
    /// Text being edited, by editor key, until it is committed.
    pub bufs: &'a mut HashMap<String, String>,
    /// Tileset atlas textures (tile fields, enum icons).
    pub atlas: &'a mut Atlas,
    /// Canvas picks (point and entity_ref).
    pub view: &'a mut ViewState,
    /// The level the values belong to: a picked point must be in it.
    pub level: Option<LevelIx>,
    /// Points may be picked on the map. Off for a selection spanning
    /// levels: a cell is of one level, and would be written to all.
    pub pick_cells: bool,
    /// Pixels per cell of point values (the entity's layer grid; the
    /// project grid for level fields).
    pub point_grid: u32,
}

/// Red for errors and dangling references.
pub const ERROR: Color32 = Color32::from_rgb(230, 110, 110);

/// One field's editor and its `×` and `null` buttons. `stored` is the value
/// stored (`None`: the default applies), `mixed` that a multi-select's
/// values differ. `removable`: offer `×` when a value is stored.
pub fn field_row(
    ui: &mut egui::Ui,
    fx: &mut FieldCx<'_>,
    f: &FieldDef,
    stored: Option<&Value>,
    mixed: bool,
    key: &str,
    removable: bool,
) -> Option<FieldEdit> {
    let shown = if mixed {
        None
    } else {
        stored.or(f.default.as_ref())
    };
    // What a reference points at goes on its own line under the editor,
    // wrapped, so a narrow pane doesn't cut it (`room_entry @ crevasse`).
    if f.ty.is_entity_ref() && !f.array && !mixed && shown.and_then(Value::as_str).is_some() {
        return ui
            .vertical(|ui| {
                let out = field_row_inner(ui, fx, f, stored, shown, mixed, key, removable);
                ref_note(ui, fx.doc, shown);
                out
            })
            .inner;
    }
    field_row_inner(ui, fx, f, stored, shown, mixed, key, removable)
}

/// [`field_row`] without the reference readout.
#[allow(clippy::too_many_arguments)]
fn field_row_inner(
    ui: &mut egui::Ui,
    fx: &mut FieldCx<'_>,
    f: &FieldDef,
    stored: Option<&Value>,
    shown: Option<&Value>,
    mixed: bool,
    key: &str,
    removable: bool,
) -> Option<FieldEdit> {
    let mut out = None;
    let row = ui.horizontal(|ui| {
        let v = if f.array {
            array_editor(ui, fx, f, shown, mixed, key)
        } else {
            value_editor(ui, fx, f, shown, mixed, key)
        };
        if let Some(v) = v {
            out = Some(FieldEdit::Set(v));
        }
        if f.nullable
            && (mixed || stored != Some(&Value::Null))
            && ui
                .small_button("null")
                .on_hover_text("Store null")
                .clicked()
        {
            out = Some(FieldEdit::Set(Value::Null));
        }
        if removable
            && (stored.is_some() || mixed)
            && ui
                .small_button("×")
                .on_hover_text("Remove the stored value (the default applies)")
                .clicked()
        {
            out = Some(FieldEdit::Remove);
        }
        if stored.is_none() && !mixed && removable && f.default.is_some() {
            // Nothing stored, the default applies: say so, quietly.
            ui.weak("default");
        }
    });
    mark(ui, key, row.response.rect);
    out
}

/// The live problems about stored field values, by field slug: each
/// problem of a file ending in `file_suffix` whose path has `prefix`
/// (`entities[<id>].fields.` for an entity, `fields.` at the start for a
/// level), as its message (an array element's prefixed with its `[i]`).
pub fn field_problems<'a>(
    problems: &'a [worlds_core::Problem],
    file_suffix: &str,
    prefix: &str,
    at_start: bool,
) -> HashMap<&'a str, Vec<String>> {
    let mut out: HashMap<&str, Vec<String>> = HashMap::new();
    for p in problems {
        if !p.file.ends_with(file_suffix) {
            continue;
        }
        let at = if at_start {
            p.path.starts_with(prefix).then_some(0)
        } else {
            p.path.find(prefix)
        };
        let Some(at) = at else {
            continue;
        };
        let rest = &p.path[at + prefix.len()..];
        let end = rest.find(['[', '.']).unwrap_or(rest.len());
        let (slug, more) = rest.split_at(end);
        out.entry(slug).or_default().push(if more.is_empty() {
            p.message.clone()
        } else {
            format!("{more}: {}", p.message)
        });
    }
    out
}

/// A field's row ([`field_row`]) with its `problems` under it in red.
#[allow(clippy::too_many_arguments)]
pub fn field_row_noted(
    ui: &mut egui::Ui,
    fx: &mut FieldCx<'_>,
    f: &FieldDef,
    stored: Option<&Value>,
    key: &str,
    removable: bool,
    problems: Option<&Vec<String>>,
) -> Option<FieldEdit> {
    let Some(notes) = problems.filter(|n| !n.is_empty()) else {
        return field_row(ui, fx, f, stored, false, key, removable);
    };
    ui.vertical(|ui| {
        let out = field_row(ui, fx, f, stored, false, key, removable);
        for n in notes {
            let r = ui.add(egui::Label::new(RichText::new(n).color(ERROR)).wrap());
            mark(ui, &format!("{key}:problem"), r.rect);
        }
        out
    })
    .inner
}

/// Remember where an editor's part was drawn this frame (egui's temp
/// data, by key): tests find widgets with [`marked`].
pub(crate) fn mark(ui: &egui::Ui, key: &str, rect: egui::Rect) {
    ui.ctx()
        .data_mut(|d| d.insert_temp(egui::Id::new(("field rect", key)), rect));
}

/// Where the editor part `key` was drawn last (tests): a field's row by
/// its key, an array element's × by `<key>[i]:remove`, its + by
/// `<key>[+]`.
pub fn marked(ctx: &egui::Context, key: &str) -> Option<egui::Rect> {
    ctx.data(|d| d.get_temp(egui::Id::new(("field rect", key))))
}

/// The editor of one value of `f`'s type (an array's element too).
/// Returns a new value to store.
pub fn value_editor(
    ui: &mut egui::Ui,
    fx: &mut FieldCx<'_>,
    f: &FieldDef,
    shown: Option<&Value>,
    mixed: bool,
    key: &str,
) -> Option<Value> {
    match &f.ty {
        FieldType::Int { .. } => {
            let init = shown
                .and_then(Value::as_i64)
                .map_or(String::new(), |v| v.to_string());
            text_commit(ui, fx.bufs, key, &init, mixed, false, 70.0).and_then(|s| {
                parse_or_note(
                    fx.bufs,
                    key,
                    s.trim().parse::<i64>().map(Value::from),
                    "a whole number",
                )
            })
        }
        FieldType::Float { .. } => {
            let init = shown
                .and_then(Value::as_f64)
                .map_or(String::new(), |v| v.to_string());
            text_commit(ui, fx.bufs, key, &init, mixed, false, 70.0).and_then(|s| {
                parse_or_note(
                    fx.bufs,
                    key,
                    s.trim()
                        .parse::<f64>()
                        .ok()
                        .and_then(serde_json::Number::from_f64)
                        .map(Value::Number)
                        .ok_or(""),
                    "a number",
                )
            })
        }
        FieldType::String { .. } => {
            let init = shown
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            text_commit(ui, fx.bufs, key, &init, mixed, false, 140.0).map(Value::from)
        }
        FieldType::Path { extensions } => {
            let init = shown
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let r = text_commit(ui, fx.bufs, key, &init, mixed, false, 120.0).map(Value::from);
            if !extensions.is_empty() {
                ui.weak(
                    extensions
                        .iter()
                        .map(|e| format!(".{e}"))
                        .collect::<Vec<_>>()
                        .join(" "),
                )
                .on_hover_text("Allowed extensions");
            }
            r
        }
        FieldType::Text { .. } => {
            let init = shown
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            text_commit(ui, fx.bufs, key, &init, mixed, true, 160.0).map(Value::from)
        }
        FieldType::Json => {
            let init = shown
                .map(|v| serde_json::to_string_pretty(v).unwrap_or_default())
                .unwrap_or_default();
            text_commit(ui, fx.bufs, key, &init, mixed, true, 160.0).and_then(|s| {
                parse_or_note(
                    fx.bufs,
                    key,
                    serde_json::from_str::<Value>(&s).map_err(|e| e.to_string()),
                    "JSON",
                )
            })
        }
        FieldType::Bool => {
            let mut b = shown.and_then(Value::as_bool).unwrap_or(false);
            ui.add(egui::Checkbox::new(&mut b, "").indeterminate(mixed))
                .changed()
                .then(|| Value::from(b))
        }
        FieldType::Color => {
            let stored = if mixed {
                None
            } else {
                shown.and_then(Value::as_str)
            };
            let r = super::panels::color_field(ui, fx.bufs, key, stored).map(Value::from);
            if mixed {
                ui.weak("—");
            }
            r
        }
        FieldType::Enum(src) => enum_combo(ui, fx, f, src, shown, mixed, key),
        FieldType::Point => point_editor(ui, fx, f, shown, mixed, key),
        FieldType::EntityRef { .. } => ref_editor(ui, fx, f, shown, mixed, key),
        FieldType::Tile { tileset } => tile_editor(ui, fx, tileset.as_ref(), shown, mixed, key),
    }
}

/// A text box that commits once: on Enter or focus loss (singleline), on
/// Ctrl+Enter or focus loss (multiline). Returns the committed text when it
/// differs from `initial`. With `mixed`, the box starts empty with a "—"
/// hint and commits only what was typed.
pub fn text_commit(
    ui: &mut egui::Ui,
    bufs: &mut HashMap<String, String>,
    key: &str,
    initial: &str,
    mixed: bool,
    multiline: bool,
    width: f32,
) -> Option<String> {
    let start = if mixed { "" } else { initial };
    let mut s = bufs.get(key).cloned().unwrap_or_else(|| start.to_string());
    let edit = if multiline {
        egui::TextEdit::multiline(&mut s)
            .desired_rows(2)
            .desired_width(width)
    } else {
        egui::TextEdit::singleline(&mut s).desired_width(width)
    };
    let r = ui.add(edit.hint_text(if mixed { "—" } else { "" }));
    mark(ui, key, r.rect);
    if r.changed() {
        bufs.insert(key.to_string(), s.clone());
    }
    let ctrl_enter = multiline
        && r.has_focus()
        && ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter));
    let mut out = None;
    if r.lost_focus() || ctrl_enter {
        let typed = bufs.remove(key);
        if ctrl_enter {
            r.surrender_focus();
        }
        if let Some(t) = typed
            && (t != initial || mixed)
        {
            out = Some(t);
        }
    }
    if let Some(e) = bufs.get(&err_key(key)) {
        ui.colored_label(ERROR, e.clone());
    }
    out
}

fn err_key(key: &str) -> String {
    format!("{key}#error")
}

/// `parsed` or, when it failed, a note under the editor (cleared by the
/// next good value).
fn parse_or_note<E: std::fmt::Display>(
    bufs: &mut HashMap<String, String>,
    key: &str,
    parsed: Result<Value, E>,
    what: &str,
) -> Option<Value> {
    match parsed {
        Ok(v) => {
            bufs.remove(&err_key(key));
            Some(v)
        }
        Err(e) => {
            let e = e.to_string();
            let msg = if e.is_empty() || e == "()" {
                format!("Not {what}")
            } else {
                format!("Not {what}: {e}")
            };
            bufs.insert(err_key(key), msg);
            None
        }
    }
}

/// The values an enum field can take: (slug, label, color, icon).
type EnumChoice = (
    String,
    String,
    Option<Color32>,
    Option<worlds_core::TileRef>,
);

fn enum_choices(doc: &Document, src: &EnumSource) -> Vec<EnumChoice> {
    match src {
        EnumSource::Project(e) => doc
            .project
            .enum_def(e.as_str())
            .map(|d| {
                d.values
                    .iter()
                    .map(|v| {
                        let c = v.color.as_ref().map(|c| {
                            let p = crate::viewport::scene::parse_color(c.as_str());
                            Color32::from_rgb(p[0], p[1], p[2])
                        });
                        (
                            v.slug.to_string(),
                            v.name.clone().unwrap_or_else(|| v.slug.to_string()),
                            c,
                            v.icon.clone(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default(),
        EnumSource::Inline(v) => v
            .iter()
            .map(|s| (s.to_string(), s.to_string(), None, None))
            .collect(),
    }
}

/// A value's swatch: its icon, else its color, else nothing.
fn choice_swatch(ui: &mut egui::Ui, fx: &mut FieldCx<'_>, c: &EnumChoice) {
    if c.2.is_none() && c.3.is_none() {
        return;
    }
    let (r, _) = ui.allocate_exact_size(Vec2::splat(14.0), Sense::hover());
    let drawn = c.3.as_ref().is_some_and(|icon| {
        if let Some(ts) = fx.doc.project.tileset(icon.tileset.as_str()) {
            super::panels::atlas_texture(fx.atlas, ui.ctx(), fx.doc, ts);
        }
        crate::viewport::draw_icon(ui.painter(), fx.doc, fx.atlas, icon, r)
    });
    if !drawn && let Some(col) = c.2 {
        ui.painter().rect_filled(r.shrink(1.0), 2.0, col);
    }
}

fn enum_combo(
    ui: &mut egui::Ui,
    fx: &mut FieldCx<'_>,
    f: &FieldDef,
    src: &EnumSource,
    shown: Option<&Value>,
    mixed: bool,
    key: &str,
) -> Option<Value> {
    let choices = enum_choices(fx.doc, src);
    let current = shown.and_then(Value::as_str).map(str::to_owned);
    if let Some(c) = choices
        .iter()
        .find(|c| Some(&c.0) == current.as_ref())
        .cloned()
    {
        choice_swatch(ui, fx, &c);
    }
    let text = if mixed {
        "—".to_string()
    } else {
        match &current {
            Some(v) => choices
                .iter()
                .find(|c| c.0 == *v)
                .map_or(v.clone(), |c| c.1.clone()),
            None if shown.is_some_and(Value::is_null) => "null".into(),
            None => "—".into(),
        }
    };
    let mut out = None;
    egui::ComboBox::from_id_salt(("enum", key))
        .selected_text(text)
        .show_ui(ui, |ui| {
            if f.nullable && ui.selectable_label(false, "null").clicked() {
                out = Some(Value::Null);
            }
            for c in &choices {
                ui.horizontal(|ui| {
                    choice_swatch(ui, fx, c);
                    if ui
                        .selectable_label(current.as_deref() == Some(c.0.as_str()), &c.1)
                        .clicked()
                    {
                        out = Some(Value::from(c.0.clone()));
                    }
                });
            }
        });
    out.filter(|v| mixed || Some(v) != shown)
}

/// x and y of a point plus Pick.
fn point_editor(
    ui: &mut egui::Ui,
    fx: &mut FieldCx<'_>,
    f: &FieldDef,
    shown: Option<&Value>,
    mixed: bool,
    key: &str,
) -> Option<Value> {
    let xy = shown
        .and_then(Value::as_array)
        .and_then(|a| Some([a.first()?.as_i64()?, a.get(1)?.as_i64()?]));
    let mut out = None;
    let mut axis = |ui: &mut egui::Ui, i: usize, bufs: &mut HashMap<String, String>| {
        let init = xy.map_or(String::new(), |p| p[i].to_string());
        let k = format!("{key}.{}", ["x", "y"][i]);
        if let Some(s) = text_commit(ui, bufs, &k, &init, mixed, false, 36.0) {
            // The note goes under the axis it is about (where
            // `text_commit` shows it), and the next good value clears it.
            let parsed = s.trim().parse::<i64>().map(|n| {
                let mut p = xy.unwrap_or([0, 0]);
                p[i] = n;
                serde_json::json!([p[0], p[1]])
            });
            if let Some(v) = parse_or_note(bufs, &k, parsed, "a whole number") {
                out = Some(v);
            }
        }
    };
    ui.weak("x");
    axis(ui, 0, fx.bufs);
    ui.weak("y");
    axis(ui, 1, fx.bufs);
    ui.add_enabled_ui(fx.pick_cells, |ui| {
        pick_button(
            ui,
            fx,
            key,
            PickAccept {
                entity: false,
                cell: true,
                ..Default::default()
            },
            || format!("Click a cell for {}", f.slug),
        )
    });
    if let Some(PickedThing::Cell { level, world_px }) = fx.view.take_picked(key) {
        match picked_cell(fx, level, world_px) {
            Ok(c) => out = Some(serde_json::json!([c[0], c[1]])),
            Err(e) => fx.view.set_status(e, true),
        }
    }
    out
}

/// The cell of `point_grid` under `world_px` in `level`, which must be the
/// field's level.
fn picked_cell(fx: &FieldCx<'_>, level: LevelIx, world_px: [f64; 2]) -> Result<[i64; 2], String> {
    if !fx.pick_cells {
        return Err("The selection spans levels: a point can't be picked for all of them".into());
    }
    if fx.level.is_some_and(|l| l != level) {
        return Err(format!(
            "A point is a cell of this level, not of {}",
            fx.doc.level(level).slug
        ));
    }
    let o = fx.doc.level(level).world_px;
    let g = fx.point_grid.max(1) as f64;
    Ok([
        ((world_px[0] - o[0] as f64) / g).floor() as i64,
        ((world_px[1] - o[1] as f64) / g).floor() as i64,
    ])
}

/// A Pick button: arms a canvas pick for `key` (or cancels the one in
/// progress). Returns whether it is armed.
fn pick_button(
    ui: &mut egui::Ui,
    fx: &mut FieldCx<'_>,
    key: &str,
    accept: PickAccept,
    prompt: impl Fn() -> String,
) -> bool {
    let armed = fx.view.pick.as_ref().is_some_and(|p| p.target == key);
    let r = ui
        .add(egui::Button::selectable(armed, "Pick"))
        .on_hover_text(if accept.entity {
            "Click the target on the map, in any level (Esc cancels)"
        } else {
            "Click a cell on the map (Esc cancels)"
        });
    if r.clicked() {
        if armed {
            fx.view.pick = None;
        } else {
            fx.view.start_pick(accept, key.to_string(), prompt());
        }
    }
    armed
}

/// `type @ level` of the entity with id `id`, or `None` if none has it.
pub fn ref_label(doc: &Document, id: &str) -> Option<String> {
    let id = worlds_core::Id::new(id).ok()?;
    let (ix, _, e) = doc.find_entity(&id)?;
    Some(format!("{} @ {}", e.ty, doc.level(ix).slug))
}

/// An entity reference: its id and Pick (what it points at is shown under
/// the row, [`ref_note`]).
fn ref_editor(
    ui: &mut egui::Ui,
    fx: &mut FieldCx<'_>,
    f: &FieldDef,
    shown: Option<&Value>,
    mixed: bool,
    key: &str,
) -> Option<Value> {
    let id = shown
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let mut out = text_commit(ui, fx.bufs, key, &id, mixed, false, 84.0).map(|s| {
        let s = s.trim().to_string();
        if s.is_empty() && f.nullable {
            Value::Null
        } else {
            Value::from(s)
        }
    });
    pick_button(
        ui,
        fx,
        key,
        PickAccept {
            entity: true,
            cell: false,
            ..Default::default()
        },
        || format!("Click the entity {} points to", f.slug),
    );
    if let Some(PickedThing::Entity { id, .. }) = fx.view.take_picked(key) {
        out = Some(Value::from(id.to_string()));
    }
    out
}

/// What reference `v` points at, `type @ level` (wrapped, never cut), or
/// "missing" in red when no entity has that id.
fn ref_note(ui: &mut egui::Ui, doc: &Document, v: Option<&Value>) {
    let Some(id) = v.and_then(Value::as_str).filter(|s| !s.is_empty()) else {
        return;
    };
    match ref_label(doc, id) {
        Some(l) => {
            let r = ui.add(egui::Label::new(RichText::new(&l).weak()).wrap());
            mark(ui, &format!("ref note:{l}"), r.rect);
        }
        None => {
            ui.colored_label(ERROR, "missing")
                .on_hover_text(format!("No entity has id {id}"));
        }
    }
}

/// A tile: its icon, the tileset (fixed when the field names one) and an
/// atlas to click a tile in.
fn tile_editor(
    ui: &mut egui::Ui,
    fx: &mut FieldCx<'_>,
    fixed: Option<&worlds_core::Slug>,
    shown: Option<&Value>,
    mixed: bool,
    key: &str,
) -> Option<Value> {
    let cur: Option<worlds_core::TileRef> =
        shown.and_then(|v| serde_json::from_value(v.clone()).ok());
    if let Some(t) = &cur
        && !mixed
    {
        let (r, _) = ui.allocate_exact_size(Vec2::splat(18.0), Sense::hover());
        if let Some(ts) = fx.doc.project.tileset(t.tileset.as_str()) {
            super::panels::atlas_texture(fx.atlas, ui.ctx(), fx.doc, ts);
        }
        crate::viewport::draw_icon(ui.painter(), fx.doc, fx.atlas, t, r);
    }
    let buf_ts = format!("{key}.tileset");
    let mut ts_slug = fixed
        .map(|s| s.to_string())
        .or_else(|| fx.bufs.get(&buf_ts).cloned())
        .or_else(|| cur.as_ref().map(|t| t.tileset.to_string()))
        .or_else(|| fx.doc.project.tilesets.first().map(|t| t.slug.to_string()))
        .unwrap_or_default();
    if fixed.is_none() {
        egui::ComboBox::from_id_salt(("tileset", key))
            .selected_text(ts_slug.clone())
            .width(90.0)
            .show_ui(ui, |ui| {
                for t in &fx.doc.project.tilesets {
                    if ui
                        .selectable_label(ts_slug == t.slug.as_str(), t.slug.as_str())
                        .clicked()
                    {
                        ts_slug = t.slug.to_string();
                        fx.bufs.insert(buf_ts.clone(), ts_slug.clone());
                    }
                }
            });
    }
    let text = if mixed {
        "—".to_string()
    } else {
        cur.as_ref()
            .map_or("none".into(), |t| format!("[{}, {}]", t.tile[0], t.tile[1]))
    };
    let mut out = None;
    if let Some(ts) = fx.doc.project.tileset(&ts_slug) {
        let tex = super::panels::atlas_texture(fx.atlas, ui.ctx(), fx.doc, ts);
        ui.menu_button(text, |ui| match &tex {
            Some(tex) => {
                if let Some(tile) = atlas_picker(ui, ts, tex, cur.as_ref().map(|t| t.tile)) {
                    out = Some(serde_json::json!({ "tileset": ts.slug.as_str(), "tile": tile }));
                    ui.close();
                }
            }
            None => {
                ui.colored_label(ERROR, "The tileset image didn't load");
            }
        });
    } else {
        ui.weak(text);
    }
    out
}

/// A tileset atlas to click a tile in (scaled to fit about 320 px wide);
/// `current` is outlined. Returns the tile clicked.
pub fn atlas_picker(
    ui: &mut egui::Ui,
    ts: &worlds_core::TilesetDef,
    tex: &egui::TextureHandle,
    current: Option<[u32; 2]>,
) -> Option<[u32; 2]> {
    let img = tex.size_vec2();
    let scale = (320.0 / img.x).clamp(0.25, 4.0);
    let size = img * scale;
    let mut out = None;
    egui::ScrollArea::both()
        .id_salt("atlas picker")
        .max_height(360.0)
        .show(ui, |ui| {
            let resp = ui.add(egui::Image::new((tex.id(), size)).sense(Sense::click()));
            let to_tile = |p: egui::Pos2| {
                let local = (p - resp.rect.min) / scale;
                ts.tile_at_px([local.x.max(0.0) as u32, local.y.max(0.0) as u32])
            };
            let outline = |t: [u32; 2], col: Color32| {
                let a = ts.tile_px(t);
                let r = egui::Rect::from_min_size(
                    resp.rect.min + Vec2::new(a[0] as f32, a[1] as f32) * scale,
                    Vec2::new(ts.tile_size[0] as f32, ts.tile_size[1] as f32) * scale,
                );
                ui.painter().rect_stroke(
                    r,
                    0.0,
                    egui::Stroke::new(2.0, col),
                    egui::StrokeKind::Outside,
                );
            };
            if let Some(t) = current {
                outline(t, super::theme::ACCENT);
            }
            if let Some(h) = resp.hover_pos() {
                outline(to_tile(h), Color32::WHITE);
            }
            if resp.clicked()
                && let Some(p) = resp.interact_pointer_pos()
            {
                out = Some(to_tile(p));
            }
        });
    out
}

/// A new element for an array of `f`'s type (+): its default's first
/// element if it has one, else a plain value of the type.
fn new_element(doc: &Document, f: &FieldDef) -> Value {
    if let Some(Value::Array(a)) = &f.default
        && let Some(v) = a.first()
    {
        return v.clone();
    }
    match &f.ty {
        FieldType::Int { min, .. } => min.clone().map_or(Value::from(0), Value::Number),
        FieldType::Float { min, .. } => min.clone().map_or(Value::from(0.0), Value::Number),
        FieldType::Bool => Value::from(false),
        FieldType::String { .. } | FieldType::Text { .. } | FieldType::Path { .. } => {
            Value::from("")
        }
        FieldType::Color => Value::from("#ffffff"),
        FieldType::Enum(src) => enum_choices(doc, src)
            .first()
            .map_or(Value::Null, |c| Value::from(c.0.clone())),
        FieldType::Point => serde_json::json!([0, 0]),
        FieldType::EntityRef { .. } => Value::from(""),
        FieldType::Tile { tileset } => tileset
            .as_ref()
            .or(doc.project.tilesets.first().map(|t| &t.slug))
            .map_or(
                Value::Null,
                |t| serde_json::json!({ "tileset": t.as_str(), "tile": [0, 0] }),
            ),
        FieldType::Json => Value::Null,
    }
}

/// An array: each element's editor with ↑ ↓ ×, then +. Every change
/// returns the whole new array.
fn array_editor(
    ui: &mut egui::Ui,
    fx: &mut FieldCx<'_>,
    f: &FieldDef,
    shown: Option<&Value>,
    mixed: bool,
    key: &str,
) -> Option<Value> {
    let items: Vec<Value> = if mixed {
        Vec::new()
    } else {
        shown.and_then(Value::as_array).cloned().unwrap_or_default()
    };
    // Element edits (text committed when focus moves, e.g. onto ↑ or +)
    // and the frame's one structural change are collected, then applied
    // in that order to one array, so neither discards the other.
    let mut edits: Vec<(usize, Value)> = Vec::new();
    let mut op: Option<ArrayOp> = None;
    let elem = FieldDef {
        array: false,
        nullable: false,
        default: None,
        ..f.clone()
    };
    ui.vertical(|ui| {
        if mixed {
            ui.weak("— (differs; + starts a new list for all)");
        }
        let n = items.len();
        for (i, v) in items.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.weak(format!("{i}"));
                if let Some(nv) =
                    value_editor(ui, fx, &elem, Some(v), false, &format!("{key}[{i}]"))
                {
                    edits.push((i, nv));
                }
                if ui
                    .add_enabled(i > 0, egui::Button::new("↑").small())
                    .clicked()
                {
                    op = Some(ArrayOp::Swap(i, i - 1));
                }
                if ui
                    .add_enabled(i + 1 < n, egui::Button::new("↓").small())
                    .clicked()
                {
                    op = Some(ArrayOp::Swap(i, i + 1));
                }
                let x = ui.small_button("×").on_hover_text("Remove this element");
                mark(ui, &format!("{key}[{i}]:remove"), x.rect);
                if x.clicked() {
                    op = Some(ArrayOp::Remove(i));
                }
                if f.ty.is_entity_ref() {
                    ref_note(ui, fx.doc, Some(v));
                }
            });
        }
        // + : points and references pick the new element on the map.
        let add_key = format!("{key}[+]");
        match &f.ty {
            FieldType::Point | FieldType::EntityRef { .. } => {
                let accept = PickAccept {
                    entity: f.ty.is_entity_ref(),
                    cell: !f.ty.is_entity_ref(),
                    ..Default::default()
                };
                let armed = fx.view.pick.as_ref().is_some_and(|p| p.target == add_key);
                let b = ui
                    .add_enabled(
                        accept.entity || fx.pick_cells,
                        egui::Button::selectable(armed, "+ Pick"),
                    )
                    .on_hover_text("Add an element: click it on the map (Esc cancels)");
                mark(ui, &add_key, b.rect);
                if b.clicked() {
                    if armed {
                        fx.view.pick = None;
                    } else {
                        fx.view.start_pick(
                            accept,
                            add_key.clone(),
                            format!("Click to add to {}", f.slug),
                        );
                    }
                }
                match fx.view.take_picked(&add_key) {
                    Some(PickedThing::Entity { id, .. }) => {
                        op = Some(ArrayOp::Push(Value::from(id.to_string())));
                    }
                    Some(PickedThing::Cell { level, world_px }) => {
                        match picked_cell(fx, level, world_px) {
                            Ok(c) => op = Some(ArrayOp::Push(serde_json::json!([c[0], c[1]]))),
                            Err(e) => fx.view.set_status(e, true),
                        }
                    }
                    Some(PickedThing::Level { .. } | PickedThing::Note { .. }) | None => {}
                }
            }
            _ => {
                let b = ui.small_button("+").on_hover_text("Add an element");
                mark(ui, &add_key, b.rect);
                if b.clicked() {
                    op = Some(ArrayOp::Push(new_element(fx.doc, f)));
                }
            }
        }
    });
    if edits.is_empty() && op.is_none() {
        return None;
    }
    let mut a = items;
    for (i, v) in edits {
        a[i] = v;
    }
    match op {
        Some(ArrayOp::Swap(i, j)) => a.swap(i, j),
        Some(ArrayOp::Remove(i)) => {
            a.remove(i);
        }
        Some(ArrayOp::Push(v)) => a.push(v),
        None => {}
    }
    Some(Value::Array(a))
}

/// The one structural change an array editor's buttons make in a frame.
enum ArrayOp {
    Swap(usize, usize),
    Remove(usize),
    Push(Value),
}

/// A field's label: its name (else slug), the doc on hover.
pub fn field_label(ui: &mut egui::Ui, f: &FieldDef) {
    let name = f.name.as_deref().unwrap_or(f.slug.as_str());
    let text = format!("{name}{}", if f.array { " []" } else { "" });
    let mut hover = format!(
        "{} ({}{})",
        f.slug,
        f.ty.name(),
        if f.array { " array" } else { "" }
    );
    if let Some(d) = &f.doc {
        hover.push('\n');
        hover.push_str(d);
    }
    ui.label(RichText::new(text).weak()).on_hover_text(hover);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_elements_fit_the_type() {
        let doc = crate::viewport::scene::tests::small_doc();
        let slug = |s: &str| worlds_core::Slug::new(s).unwrap();
        let mut f = FieldDef::new(slug("n"), FieldType::int());
        f.array = true;
        assert_eq!(new_element(&doc, &f), Value::from(0));
        f.default = Some(serde_json::json!([7, 8]));
        assert_eq!(new_element(&doc, &f), Value::from(7));
        let f = FieldDef::new(slug("p"), FieldType::Point);
        assert_eq!(new_element(&doc, &f), serde_json::json!([0, 0]));
        let f = FieldDef::new(
            slug("e"),
            FieldType::Enum(EnumSource::Inline(vec![slug("wood"), slug("iron")])),
        );
        assert_eq!(new_element(&doc, &f), Value::from("wood"));
    }
}
