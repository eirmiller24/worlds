//! The Inspector pane: the selected entities (one entity's form, or the
//! fields several have in common), then the active level (rename, delete,
//! level fields), or the project when no level is active.
//!
//! Every edit is queued on [`EditorState::pending`] as one labelled batch,
//! so it is one undo entry and goes through the same path as any other
//! edit. Field editors come from [`super::fields`].

use std::collections::HashMap;

use egui::{Color32, RichText, Sense, Vec2};
use serde_json::Value;
use worlds_core::{Command, EntityDef, EntityInstance, FieldDef, FieldType, Id, Slug};

use super::EditorState;
use super::actions::Action;
use super::fields::{FieldCx, FieldEdit, field_label, field_row, text_commit};
use crate::doc::{Document, LevelIx};

/// Queue `cmds` as one undo entry labelled `label`.
pub fn push(pending: &mut Vec<Command>, label: impl Into<String>, cmds: Vec<Command>) {
    if cmds.is_empty() {
        return;
    }
    pending.push(Command::Batch {
        label: label.into(),
        commands: cmds,
    });
}

/// The Inspector pane.
pub fn inspector(ui: &mut egui::Ui, st: &mut EditorState) {
    if st.doc.is_none() {
        ui.weak("No project");
        return;
    }
    egui::ScrollArea::vertical()
        .auto_shrink([false, true])
        .show(ui, |ui| {
            let mut n = selection_section(ui, st);
            for part in [
                super::note_inspector::notes_section,
                super::note_inspector::levels_section,
            ] {
                if n > 0 {
                    ui.add_space(8.0);
                    ui.separator();
                }
                let k = part(ui, st);
                n += k;
            }
            // The selected reference image, if any.
            let k = super::references::inspector_section(ui, st);
            if k > 0 {
                ui.add_space(8.0);
                n += k;
            }
            if let Some(ts) = tile_count(st)
                && ts > 0
            {
                ui.separator();
                ui.label(format!(
                    "{ts} tile stack{} selected (drag, arrows, Ctrl+D, Delete)",
                    if ts == 1 { "" } else { "s" }
                ));
                n += ts;
            }
            if n > 0 {
                ui.add_space(8.0);
                ui.separator();
            }
            super::panels::level_inspector(ui, st);
        });
}

fn tile_count(st: &EditorState) -> Option<usize> {
    Some(st.view.selection.tiles().count())
}

/// One selected entity, found: its level, layer and definition.
struct Sel<'a> {
    level: LevelIx,
    layer: &'a Slug,
    e: &'a EntityInstance,
    def: Option<&'a EntityDef>,
}

/// The selected entities' part. Returns how many were shown.
fn selection_section(ui: &mut egui::Ui, st: &mut EditorState) -> usize {
    let EditorState {
        doc,
        view,
        pending,
        edit_bufs,
        atlas,
        actions,
        ..
    } = st;
    let Some(doc) = doc.as_ref() else {
        return 0;
    };
    let ids = view.selection.entity_ids();
    let sel: Vec<Sel<'_>> = ids
        .iter()
        .filter_map(|id| {
            let (level, layer, e) = doc.find_entity(id)?;
            Some(Sel {
                level,
                layer,
                e,
                def: doc.project.entity_def(e.ty.as_str()),
            })
        })
        .collect();
    if sel.is_empty() {
        return 0;
    }
    ui.horizontal(|ui| {
        if ui
            .small_button("Duplicate")
            .on_hover_text("Ctrl+D: copies one cell to the right, fields kept")
            .clicked()
        {
            actions.push(Action::DuplicateSelection);
        }
        if ui
            .small_button("Delete")
            .on_hover_text("Delete (undo brings them back)")
            .clicked()
        {
            actions.push(Action::DeleteSelection);
        }
        if ui.small_button("Deselect").clicked() {
            view.selection.clear();
        }
    });
    let n = sel.len();
    if n == 1 {
        single(ui, doc, view, pending, edit_bufs, atlas, &sel[0]);
    } else {
        multi(ui, doc, view, pending, edit_bufs, atlas, &sel);
    }
    n
}

/// The type's icon and name, with the slug and doc.
fn type_header(
    ui: &mut egui::Ui,
    doc: &Document,
    atlas: &mut crate::viewport::Atlas,
    def: Option<&EntityDef>,
    ty: &Slug,
    extra: &str,
) {
    ui.horizontal(|ui| {
        if let Some(def) = def {
            let (r, _) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::hover());
            if let Some(icon) = &def.icon
                && let Some(ts) = doc.project.tileset(icon.tileset.as_str())
            {
                super::panels::atlas_texture(atlas, ui.ctx(), doc, ts);
            }
            super::entities::type_swatch(ui.painter(), doc, atlas, def, r);
            ui.heading(&def.name);
        } else {
            ui.heading(ty.as_str());
        }
        ui.weak(format!("{ty}{extra}"));
    });
    match def {
        Some(def) => {
            if let Some(d) = &def.doc {
                ui.label(RichText::new(d).italics());
            }
        }
        None => {
            ui.colored_label(
                super::fields::ERROR,
                format!("No entity type {ty}: its fields can't be edited"),
            );
        }
    }
}

/// Two whole numbers committed together (cell, offset, size): returns the
/// pair when one of them was committed.
fn pair_editor(
    ui: &mut egui::Ui,
    bufs: &mut HashMap<String, String>,
    key: &str,
    v: [i64; 2],
    enabled: [bool; 2],
) -> Option<[i64; 2]> {
    pair_editor_named(ui, bufs, key, v, enabled, ["x", "y"])
}

/// [`pair_editor`] with other axis names ("w", "h").
fn pair_editor_named(
    ui: &mut egui::Ui,
    bufs: &mut HashMap<String, String>,
    key: &str,
    v: [i64; 2],
    enabled: [bool; 2],
    names: [&str; 2],
) -> Option<[i64; 2]> {
    let mut out = None;
    for (i, axis) in names.into_iter().enumerate() {
        ui.weak(axis);
        let k = format!("{key}.{axis}");
        ui.add_enabled_ui(enabled[i], |ui| {
            if let Some(s) = text_commit(ui, bufs, &k, &v[i].to_string(), false, false, 44.0)
                && let Ok(n) = s.trim().parse::<i64>()
            {
                let mut p = v;
                p[i] = n;
                out = Some(p);
            }
        });
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn single(
    ui: &mut egui::Ui,
    doc: &Document,
    view: &mut crate::view::ViewState,
    pending: &mut Vec<Command>,
    bufs: &mut HashMap<String, String>,
    atlas: &mut crate::viewport::Atlas,
    s: &Sel<'_>,
) {
    let e = s.e;
    type_header(ui, doc, atlas, s.def, &e.ty, "");
    let ldef = doc.project.layer(s.layer.as_str());
    let g = ldef.map_or(doc.project.grid_size, |d| doc.project.layer_grid(d));
    let id = e.id.clone();
    egui::Grid::new("entity")
        .num_columns(2)
        .striped(true)
        .show(ui, |ui| {
            ui.weak("Id");
            ui.monospace(e.id.as_str());
            ui.end_row();
            ui.weak("Level · layer");
            ui.label(format!("{} · {}", doc.level(s.level).slug, s.layer));
            ui.end_row();
            ui.weak("Cell");
            ui.horizontal(|ui| {
                let k = format!("{id}:cell");
                if let Some(c) = pair_editor(
                    ui,
                    bufs,
                    &k,
                    [e.cell[0] as i64, e.cell[1] as i64],
                    [true, true],
                ) {
                    push(
                        pending,
                        format!("Move entity {id}"),
                        vec![Command::MoveEntity {
                            id: id.clone(),
                            cell: [c[0] as i32, c[1] as i32],
                            offset_px: e.offset_px,
                            allow_outside: false,
                        }],
                    );
                }
            });
            ui.end_row();
            ui.weak("Offset px");
            ui.horizontal(|ui| {
                let k = format!("{id}:offset");
                if let Some(o) = pair_editor(
                    ui,
                    bufs,
                    &k,
                    [e.offset_px[0] as i64, e.offset_px[1] as i64],
                    [true, true],
                ) {
                    // Whole cells of offset go into the cell.
                    let (cell, offset_px) =
                        crate::tools::fold_offset(e.cell, [o[0] as i32, o[1] as i32], g);
                    push(
                        pending,
                        format!("Move entity {id}"),
                        vec![Command::MoveEntity {
                            id: id.clone(),
                            cell,
                            offset_px,
                            allow_outside: false,
                        }],
                    );
                }
            });
            ui.end_row();
            if let Some(def) = s.def {
                ui.weak("Size");
                ui.horizontal(|ui| {
                    let size = e.size.unwrap_or(def.size);
                    let k = format!("{id}:size");
                    if let Some(sz) = pair_editor_named(
                        ui,
                        bufs,
                        &k,
                        [size[0] as i64, size[1] as i64],
                        def.resizable,
                        ["w", "h"],
                    ) {
                        let new = [sz[0].max(1) as u32, sz[1].max(1) as u32];
                        push(
                            pending,
                            format!("Resize entity {id}"),
                            vec![Command::ResizeEntity {
                                id: id.clone(),
                                size: (new != def.size).then_some(new),
                            }],
                        );
                    }
                    if e.size.is_some()
                        && ui
                            .small_button("×")
                            .on_hover_text("Back to the type's size")
                            .clicked()
                    {
                        push(
                            pending,
                            format!("Resize entity {id}"),
                            vec![Command::ResizeEntity {
                                id: id.clone(),
                                size: None,
                            }],
                        );
                    }
                    if def.resizable == [false, false] {
                        ui.weak("(not resizable)");
                    }
                });
                ui.end_row();
            }
        });
    let Some(def) = s.def else {
        stored_values(ui, e);
        return;
    };
    if !def.fields.is_empty() {
        ui.add_space(6.0);
        ui.strong("Fields");
    }
    let mut fx = FieldCx {
        doc,
        bufs,
        atlas,
        view,
        level: Some(s.level),
        pick_cells: true,
        point_grid: g,
    };
    // What validation says about each value (live problems), shown in red
    // under it.
    let problems = super::fields::field_problems(
        &doc.problems,
        &format!("levels/{}/entities.json", doc.level(s.level).slug),
        &format!("entities[{id}].fields."),
        false,
    );
    egui::Grid::new("entity fields")
        .num_columns(2)
        .striped(true)
        .show(ui, |ui| {
            for f in &def.fields {
                field_label(ui, f);
                let key = format!("{id}:field:{}", f.slug);
                if let Some(edit) = super::fields::field_row_noted(
                    ui,
                    &mut fx,
                    f,
                    e.fields.get(&f.slug),
                    &key,
                    true,
                    problems.get(f.slug.as_str()),
                ) {
                    let value = match edit {
                        FieldEdit::Set(v) => Some(v),
                        FieldEdit::Remove => None,
                    };
                    push(
                        pending,
                        format!("Set {} on {id}", f.slug),
                        vec![Command::SetEntityField {
                            id: id.clone(),
                            field: f.slug.clone(),
                            value,
                            allow_unknown: false,
                        }],
                    );
                }
                ui.end_row();
            }
            // Stored keys the type has no field for (validation reports
            // them): shown, and removable.
            for (k, v) in &e.fields {
                if def.field(k.as_str()).is_some() {
                    continue;
                }
                ui.label(RichText::new(k.as_str()).color(super::fields::ERROR))
                    .on_hover_text("The type has no such field");
                ui.horizontal(|ui| {
                    ui.label(short(v));
                    if ui.small_button("×").on_hover_text("Remove it").clicked() {
                        push(
                            pending,
                            format!("Set {k} on {id}"),
                            vec![Command::SetEntityField {
                                id: id.clone(),
                                field: k.clone(),
                                value: None,
                                allow_unknown: false,
                            }],
                        );
                    }
                });
                ui.end_row();
            }
        });
}

/// Stored values, read only (an entity whose type is gone).
fn stored_values(ui: &mut egui::Ui, e: &EntityInstance) {
    egui::Grid::new("entity values")
        .num_columns(2)
        .striped(true)
        .show(ui, |ui| {
            for (k, v) in &e.fields {
                ui.weak(k.as_str());
                ui.label(short(v));
                ui.end_row();
            }
        });
}

fn short(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => {
            let s = other.to_string();
            if s.len() > 60 {
                format!("{}…", &s[..s.floor_char_boundary(60)])
            } else {
                s
            }
        }
    }
}

/// Whether two fields are "the same field" for a multi-select: same slug,
/// type, array-ness and enum source.
pub fn same_field(a: &FieldDef, b: &FieldDef) -> bool {
    a.slug == b.slug
        && a.array == b.array
        && match (&a.ty, &b.ty) {
            (FieldType::Enum(x), FieldType::Enum(y)) => x == y,
            (x, y) => x.name() == y.name(),
        }
}

/// The fields every selected type has (by [`same_field`]), in the first
/// type's order. Empty when a selected entity's type is unknown.
pub fn common_fields<'a>(defs: &[Option<&'a EntityDef>]) -> Vec<&'a FieldDef> {
    let Some(Some(first)) = defs.first() else {
        return Vec::new();
    };
    if defs.iter().any(Option::is_none) {
        return Vec::new();
    }
    first
        .fields
        .iter()
        .filter(|f| {
            defs.iter()
                .flatten()
                .all(|d| d.fields.iter().any(|g| same_field(f, g)))
        })
        .collect()
}

/// The value a multi-select shows for field `f`: `(stored, mixed)`. When
/// every entity holds the same effective value (stored, else its type's
/// default) it is shown; `stored` is the common stored value when every one
/// stores it.
pub fn common_value<'a>(sel: &[(&'a EntityInstance, &'a FieldDef)]) -> (Option<&'a Value>, bool) {
    let eff =
        |(e, f): &(&'a EntityInstance, &'a FieldDef)| e.fields.get(&f.slug).or(f.default.as_ref());
    let first = sel.first().and_then(eff);
    let mixed = sel.iter().any(|s| eff(s) != first);
    if mixed {
        return (None, true);
    }
    let stored: Vec<Option<&Value>> = sel.iter().map(|(e, f)| e.fields.get(&f.slug)).collect();
    if stored.iter().all(Option::is_some) {
        (stored[0], false)
    } else if stored.iter().all(Option::is_none) {
        (None, false)
    } else {
        // Same effective value, some stored and some from the default:
        // show it as stored so × is offered.
        (stored.iter().flatten().next().copied(), false)
    }
}

#[allow(clippy::too_many_arguments)]
fn multi(
    ui: &mut egui::Ui,
    doc: &Document,
    view: &mut crate::view::ViewState,
    pending: &mut Vec<Command>,
    bufs: &mut HashMap<String, String>,
    atlas: &mut crate::viewport::Atlas,
    sel: &[Sel<'_>],
) {
    let n = sel.len();
    let mut by_type: Vec<(&Slug, usize)> = Vec::new();
    for s in sel {
        match by_type.iter_mut().find(|(t, _)| *t == &s.e.ty) {
            Some((_, c)) => *c += 1,
            None => by_type.push((&s.e.ty, 1)),
        }
    }
    let first = sel[0].def;
    if by_type.len() == 1 {
        type_header(
            ui,
            doc,
            atlas,
            first,
            &sel[0].e.ty,
            &format!(" · {n} selected"),
        );
    } else {
        ui.heading(format!("{n} entities"));
        ui.weak(
            by_type
                .iter()
                .map(|(t, c)| format!("{c} × {t}"))
                .collect::<Vec<_>>()
                .join(", "),
        );
    }
    let defs: Vec<Option<&EntityDef>> = sel.iter().map(|s| s.def).collect();
    let common = common_fields(&defs);
    ui.add_space(6.0);
    if common.is_empty() {
        ui.weak("No field common to every selected entity.");
        return;
    }
    ui.strong("Common fields");
    ui.weak("— marks a value that differs; an edit sets it on all of them.");
    // The key covers the whole selection, so editors don't carry text from
    // one selection to another.
    let sel_key = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for s in sel {
            s.e.id.hash(&mut h);
        }
        format!("multi{:x}", h.finish())
    };
    let one_level = sel.iter().all(|s| s.level == sel[0].level);
    let g = doc
        .project
        .layer(sel[0].layer.as_str())
        .map_or(doc.project.grid_size, |d| doc.project.layer_grid(d));
    let mut fx = FieldCx {
        doc,
        bufs,
        atlas,
        view,
        level: one_level.then_some(sel[0].level),
        pick_cells: one_level,
        point_grid: g,
    };
    let ids: Vec<Id> = sel.iter().map(|s| s.e.id.clone()).collect();
    egui::Grid::new("common fields")
        .num_columns(2)
        .striped(true)
        .show(ui, |ui| {
            for f in common {
                field_label(ui, f);
                let pairs: Vec<(&EntityInstance, &FieldDef)> = sel
                    .iter()
                    .filter_map(|s| {
                        let d = s.def?;
                        let own = d.fields.iter().find(|g| same_field(f, g))?;
                        Some((s.e, own))
                    })
                    .collect();
                let (stored, mixed) = common_value(&pairs);
                let key = format!("{sel_key}:field:{}", f.slug);
                if let Some(edit) = field_row(ui, &mut fx, f, stored, mixed, &key, true) {
                    let (value, verb) = match edit {
                        FieldEdit::Set(v) => (Some(v), "Set"),
                        FieldEdit::Remove => (None, "Clear"),
                    };
                    let cmds = ids
                        .iter()
                        .zip(&pairs)
                        .filter(|(_, (e, _))| value.is_some() || e.fields.contains_key(&f.slug))
                        .map(|(id, _)| Command::SetEntityField {
                            id: id.clone(),
                            field: f.slug.clone(),
                            value: value.clone(),
                            allow_unknown: false,
                        })
                        .collect();
                    push(pending, format!("{verb} {} on {n} entities", f.slug), cmds);
                }
                ui.end_row();
            }
        });
    if !one_level {
        ui.colored_label(
            Color32::from_rgb(230, 190, 90),
            "The selection spans levels: point fields can't be picked.",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use worlds_core::Slug;

    fn def(fields: Value) -> EntityDef {
        serde_json::from_value(json!({ "slug": "t", "name": "T", "size": [1, 1],
            "color": "#808080", "fields": fields }))
        .unwrap()
    }

    #[test]
    fn common_fields_match_slug_type_array_and_enum_source() {
        let a = def(json!([
            { "slug": "channel", "type": "int" },
            { "slug": "style", "type": "enum", "values": ["a", "b"] },
            { "slug": "points", "type": "point", "array": true },
            { "slug": "only_a", "type": "bool" },
            { "slug": "kind", "type": "enum", "enum": "facing" }
        ]));
        let b = def(json!([
            { "slug": "points", "type": "point", "array": true },
            { "slug": "channel", "type": "int", "min": 3 },
            { "slug": "style", "type": "enum", "values": ["a", "c"] },
            { "slug": "kind", "type": "enum", "enum": "facing" }
        ]));
        let got: Vec<&str> = common_fields(&[Some(&a), Some(&b)])
            .iter()
            .map(|f| f.slug.as_str())
            .collect();
        // Bounds may differ; inline values that differ are another source.
        assert_eq!(got, ["channel", "points", "kind"]);
        assert!(common_fields(&[Some(&a), None]).is_empty());
    }

    #[test]
    fn mixed_values_show_as_mixed() {
        let d = def(json!([{ "slug": "channel", "type": "int", "default": 0 }]));
        let f = &d.fields[0];
        let inst = |v: Option<i64>| {
            let mut e: EntityInstance =
                serde_json::from_value(json!({ "id": "aaaaaaaaaa", "type": "t", "cell": [0, 0] }))
                    .unwrap();
            if let Some(v) = v {
                e.fields.insert(f.slug.clone(), json!(v));
            }
            e
        };
        let (a, b, c) = (inst(Some(3)), inst(Some(3)), inst(None));
        assert_eq!(common_value(&[(&a, f), (&b, f)]), (Some(&json!(3)), false));
        assert_eq!(common_value(&[(&a, f), (&c, f)]), (None, true));
        // Stored 0 and the default 0 are the same value.
        let z = inst(Some(0));
        assert_eq!(common_value(&[(&z, f), (&c, f)]), (Some(&json!(0)), false));
        assert_eq!(common_value(&[(&c, f), (&c, f)]), (None, false));
    }

    /// The mini fixture in an `EditorState`.
    fn state() -> (tempfile::TempDir, EditorState) {
        fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
            std::fs::create_dir_all(to).unwrap();
            for e in std::fs::read_dir(from).unwrap() {
                let p = e.unwrap().path();
                let to = to.join(p.file_name().unwrap());
                if p.is_dir() {
                    copy_dir(&p, &to);
                } else {
                    std::fs::copy(&p, &to).unwrap();
                }
            }
        }
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("mini.worlds");
        copy_dir(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../worlds-core/tests/fixtures/mini.worlds"),
            &dir,
        );
        let doc = Document::open(&dir).unwrap();
        let mut view = crate::view::ViewState::new();
        view.sync_layers(&doc, None);
        view.active_level = doc.level_ix("crevasse");
        let st = EditorState {
            doc: Some(doc),
            view,
            ..Default::default()
        };
        (tmp, st)
    }

    struct Driver {
        ctx: egui::Context,
        t: f64,
        /// The pane's width (the screen's).
        width: f32,
    }

    impl Driver {
        fn new() -> Self {
            Driver {
                ctx: egui::Context::default(),
                t: 0.0,
                width: 900.0,
            }
        }

        fn frame(&mut self, st: &mut EditorState, events: Vec<egui::Event>) {
            self.t += 0.1;
            let raw = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(self.width, 1400.0),
                )),
                time: Some(self.t),
                focused: true,
                events,
                ..Default::default()
            };
            let mut out = self.ctx.run_ui(raw, |ui| inspector(ui, st));
            out.textures_delta.clear();
        }

        fn click(&mut self, st: &mut EditorState, p: egui::Pos2) {
            for pressed in [true, false] {
                self.frame(
                    st,
                    vec![
                        egui::Event::PointerMoved(p),
                        egui::Event::PointerButton {
                            pos: p,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: Default::default(),
                        },
                    ],
                );
            }
        }

        fn rect(&self, key: &str) -> egui::Rect {
            super::super::fields::marked(&self.ctx, key)
                .unwrap_or_else(|| panic!("no editor drawn for {key}"))
        }
    }

    fn id(s: &str) -> Id {
        Id::new(s).unwrap()
    }

    /// The one queued batch's commands.
    fn batch(st: &mut EditorState) -> (String, Vec<Command>) {
        assert_eq!(st.pending.len(), 1, "{:?}", st.pending);
        match st.pending.pop().unwrap() {
            Command::Batch { label, commands } => (label, commands),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_multi_select_edit_is_one_batch_and_one_entry() {
        let (_tmp, mut st) = state();
        st.view
            .selection
            .set_entities([id("doorcrev22"), id("doorshaft2")]);
        let mut d = Driver::new();
        d.frame(&mut st, vec![]);
        d.frame(&mut st, vec![]);
        // Both are locked doors: every field is common. `consumes` is a
        // checkbox, first in its row.
        let key = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            id("doorcrev22").hash(&mut h);
            id("doorshaft2").hash(&mut h);
            format!("multi{:x}:field:consumes", h.finish())
        };
        let r = d.rect(&key);
        d.click(&mut st, r.left_center() + egui::vec2(8.0, 0.0));
        let (label, cmds) = batch(&mut st);
        assert_eq!(label, "Set consumes on 2 entities");
        assert_eq!(
            cmds,
            ["doorcrev22", "doorshaft2"].map(|e| Command::SetEntityField {
                id: id(e),
                field: Slug::new("consumes").unwrap(),
                value: Some(json!(true)),
                allow_unknown: false,
            })
        );
        // Applied, it is one undo entry setting both.
        let mut ed = crate::edit::Editing::new();
        let mut inv = Vec::new();
        let EditorState { doc, view, .. } = &mut st;
        let mut cx = crate::tools::CommandContext {
            doc: doc.as_mut().unwrap(),
            view,
            invalidate: &mut inv,
        };
        use crate::tools::EditorCommands;
        ed.apply(
            &mut cx,
            Command::Batch {
                label,
                commands: cmds,
            },
        )
        .unwrap();
        assert_eq!(ed.history().undo_entries().len(), 1);
        for e in ["doorcrev22", "doorshaft2"] {
            let (_, _, inst) = cx.doc.find_entity(&id(e)).unwrap();
            assert_eq!(inst.fields["consumes"], json!(true));
        }
    }

    #[test]
    fn a_reference_picked_in_another_level_is_set() {
        let (_tmp, mut st) = state();
        st.view.selection.set_entities([id("btncrev222")]);
        let mut d = Driver::new();
        d.frame(&mut st, vec![]);
        // Its Pick button arms the pick (the next map click answers it).
        let key = "btncrev222:field:target";
        let row = d.rect(key);
        assert!(row.width() > 0.0);
        let shaft = st.doc.as_ref().unwrap().level_ix("shaft").unwrap();
        st.view.picked = Some(crate::view::Picked {
            target: key.into(),
            what: crate::view::PickedThing::Entity {
                id: id("doorshaft2"),
                level: shaft,
            },
        });
        d.frame(&mut st, vec![]);
        let (label, cmds) = batch(&mut st);
        assert_eq!(label, "Set target on btncrev222");
        assert_eq!(
            cmds,
            [Command::SetEntityField {
                id: id("btncrev222"),
                field: Slug::new("target").unwrap(),
                value: Some(json!("doorshaft2")),
                allow_unknown: false,
            }]
        );
        assert_eq!(
            super::super::fields::ref_label(st.doc.as_ref().unwrap(), "doorshaft2").as_deref(),
            Some("locked_door @ shaft")
        );
        assert_eq!(
            super::super::fields::ref_label(st.doc.as_ref().unwrap(), "gonegonego"),
            None
        );
    }

    #[test]
    fn array_edits_send_the_whole_array() {
        let (_tmp, mut st) = state();
        st.view.selection.set_entities([id("doorcrev22")]);
        let mut d = Driver::new();
        d.frame(&mut st, vec![]);
        d.frame(&mut st, vec![]);
        // waypoints: [[1, 1], [2, 1]]. × on the first element.
        let key = "doorcrev22:field:waypoints";
        let x = d.rect(&format!("{key}[0]:remove"));
        d.click(&mut st, x.center());
        let (_, cmds) = batch(&mut st);
        assert_eq!(
            cmds,
            [Command::SetEntityField {
                id: id("doorcrev22"),
                field: Slug::new("waypoints").unwrap(),
                value: Some(json!([[2, 1]])),
                allow_unknown: false,
            }]
        );
        // + Pick: a cell clicked in this level is appended (16 px cells).
        let crev = st.doc.as_ref().unwrap().level_ix("crevasse").unwrap();
        st.view.picked = Some(crate::view::Picked {
            target: format!("{key}[+]"),
            what: crate::view::PickedThing::Cell {
                level: crev,
                world_px: [5.0 * 16.0 + 3.0, 2.0 * 16.0 + 9.0],
            },
        });
        d.frame(&mut st, vec![]);
        let (_, cmds) = batch(&mut st);
        assert!(
            matches!(&cmds[..], [Command::SetEntityField { value: Some(v), .. }]
                if *v == json!([[1, 1], [2, 1], [5, 2]])),
            "{cmds:?}"
        );
        // A cell of another level isn't a point of this one.
        let shaft = st.doc.as_ref().unwrap().level_ix("shaft").unwrap();
        st.view.picked = Some(crate::view::Picked {
            target: format!("{key}[+]"),
            what: crate::view::PickedThing::Cell {
                level: shaft,
                world_px: [330.0, 90.0],
            },
        });
        d.frame(&mut st, vec![]);
        assert!(st.pending.is_empty());
        assert!(st.view.status.as_ref().is_some_and(|s| s.error));
    }

    #[test]
    fn a_bad_point_axis_is_noted_under_that_axis_and_cleared_by_a_good_one() {
        let (_tmp, mut st) = state();
        st.view.selection.set_entities([id("doorcrev22")]);
        let mut d = Driver::new();
        d.frame(&mut st, vec![]);
        d.frame(&mut st, vec![]);
        let enter = |pressed| egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: Default::default(),
        };
        // anchor: a nullable point with no value. Type "q" into x.
        let x = "doorcrev22:field:anchor.x";
        let note = format!("{x}#error");
        let r = d.rect(x);
        d.click(&mut st, r.center());
        d.frame(&mut st, vec![egui::Event::Text("q".into())]);
        d.frame(&mut st, vec![enter(true), enter(false)]);
        assert!(st.pending.is_empty());
        assert!(
            st.edit_bufs
                .get(&note)
                .is_some_and(|n| n.starts_with("Not a whole number")),
            "{:?}",
            st.edit_bufs
        );
        // A whole number: stored, and the note goes.
        d.click(&mut st, r.center());
        d.frame(&mut st, vec![egui::Event::Text("4".into())]);
        d.frame(&mut st, vec![enter(true), enter(false)]);
        let (_, cmds) = batch(&mut st);
        assert!(
            matches!(&cmds[..], [Command::SetEntityField { value: Some(v), .. }]
                if *v == json!([4, 0])),
            "{cmds:?}"
        );
        assert!(!st.edit_bufs.contains_key(&note));
    }

    #[test]
    fn a_selection_spanning_levels_picks_no_points() {
        let (_tmp, mut st) = state();
        st.view
            .selection
            .set_entities([id("doorcrev22"), id("doorshaft2")]);
        let mut d = Driver::new();
        d.frame(&mut st, vec![]);
        let key = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            id("doorcrev22").hash(&mut h);
            id("doorshaft2").hash(&mut h);
            format!("multi{:x}:field:", h.finish())
        };
        // A cell picked (an armed pick from before, or a script) isn't
        // written to both: it is a cell of one level.
        let crev = st.doc.as_ref().unwrap().level_ix("crevasse").unwrap();
        for target in [format!("{key}anchor"), format!("{key}waypoints[+]")] {
            st.view.picked = Some(crate::view::Picked {
                target,
                what: crate::view::PickedThing::Cell {
                    level: crev,
                    world_px: [40.0, 40.0],
                },
            });
            d.frame(&mut st, vec![]);
            assert!(st.pending.is_empty(), "{:?}", st.pending);
            assert!(st.view.status.as_ref().is_some_and(|s| s.error));
            st.view.status = None;
        }
    }

    #[test]
    fn an_element_edit_committed_by_a_click_on_remove_is_kept() {
        let (_tmp, mut st) = state();
        st.view.selection.set_entities([id("doorcrev22")]);
        let mut d = Driver::new();
        d.frame(&mut st, vec![]);
        d.frame(&mut st, vec![]);
        // waypoints: [[1, 1], [2, 1]]. Type a 9 after element 0's x...
        let key = "doorcrev22:field:waypoints";
        let x0 = d.rect(&format!("{key}[0].x"));
        d.click(&mut st, x0.center());
        d.frame(&mut st, vec![egui::Event::Text("9".into())]);
        // ...then click × on element 1 (one frame: the text commits as it
        // loses focus, and the button is clicked).
        let rm = d.rect(&format!("{key}[1]:remove")).center();
        let button = |pressed| egui::Event::PointerButton {
            pos: rm,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        d.frame(
            &mut st,
            vec![egui::Event::PointerMoved(rm), button(true), button(false)],
        );
        d.frame(&mut st, vec![]);
        let all: Vec<Command> = std::mem::take(&mut st.pending);
        let last = all.last().expect("an edit");
        let Command::Batch { commands, .. } = last else {
            panic!("{last:?}")
        };
        assert!(
            matches!(&commands[..], [Command::SetEntityField { value: Some(v), .. }]
                if *v == json!([[19, 1]])),
            "{all:?}"
        );
    }

    #[test]
    fn a_problem_with_a_value_is_noted_under_it() {
        let (_tmp, mut st) = state();
        st.view.selection.set_entities([id("doorcrev22")]);
        // channel (min 0) at -3, and a waypoint that isn't a point; the
        // live problems list says so.
        {
            let doc = st.doc.as_mut().unwrap();
            let e = doc
                .project
                .worlds
                .iter_mut()
                .flat_map(|w| &mut w.levels)
                .flat_map(|l| &mut l.entities)
                .flat_map(|x| &mut x.entities)
                .find(|e| e.id == id("doorcrev22"))
                .unwrap();
            e.fields.insert(Slug::new("channel").unwrap(), json!(-3));
            e.fields
                .insert(Slug::new("waypoints").unwrap(), json!([[1, 1], "x"]));
            doc.problems = worlds_core::validate(&doc.project);
        }
        let notes = super::super::fields::field_problems(
            &st.doc.as_ref().unwrap().problems,
            "levels/crevasse/entities.json",
            "entities[doorcrev22].fields.",
            false,
        );
        assert_eq!(notes["channel"].len(), 1, "{notes:?}");
        assert!(notes["channel"][0].contains("below min 0"), "{notes:?}");
        assert!(notes["waypoints"][0].starts_with("[1]: "), "{notes:?}");
        assert!(!notes.contains_key("needs_item"));
        let mut d = Driver::new();
        d.frame(&mut st, vec![]);
        d.frame(&mut st, vec![]);
        // Under the value, in its column.
        let row = d.rect("doorcrev22:field:channel");
        let note = d.rect("doorcrev22:field:channel:problem");
        assert!(note.top() >= row.bottom() - 1.0, "{note:?} under {row:?}");
        assert!((note.left() - row.left()).abs() < 2.0, "{note:?} {row:?}");
        assert!(super::super::fields::marked(&d.ctx, "doorcrev22:field:speed:problem").is_none());
    }

    #[test]
    fn a_reference_readout_has_its_own_line() {
        let (_tmp, mut st) = state();
        st.view.selection.set_entities([id("btncrev222")]);
        let mut d = Driver::new();
        d.frame(&mut st, vec![]);
        d.frame(&mut st, vec![]);
        // target = doorcrev22: "locked_door @ crevasse" under the id and
        // Pick, wrapped rather than cut at the end of the row.
        let row = d.rect("btncrev222:field:target");
        let note = d.rect("ref note:locked_door @ crevasse");
        assert!(note.top() >= row.bottom() - 1.0, "{note:?} under {row:?}");
    }

    /// Array rows lay out cleanly at a wide and a narrow pane: each
    /// element's ⏶ ⏷ × (glyphs the fonts have) inside the pane and apart,
    /// `+` and the field's own × under the elements, and nothing over the
    /// next field's row. An Array<enum> like LDtk's chest `content`, and an
    /// array of points.
    #[test]
    fn array_rows_stay_inside_the_pane_at_any_width() {
        use super::super::fields::{ARRAY_DOWN, ARRAY_REMOVE, ARRAY_UP};
        let (_tmp, mut st) = state();
        let dir = st.doc.as_ref().unwrap().dir.clone();
        let edit = |rel: &str, from: &str, to: &str| {
            let p = dir.join(rel);
            let s = std::fs::read_to_string(&p).unwrap();
            assert!(s.contains(from), "{rel}");
            std::fs::write(&p, s.replace(from, to)).unwrap();
        };
        let waypoints =
            r#"{ "slug": "waypoints", "type": "point", "array": true, "default": [] },"#;
        edit(
            "entities/locked_door.json",
            waypoints,
            &format!(
                "{waypoints}\n    {}",
                r#"{ "slug": "content", "type": "enum", "values": ["sword", "bow", "healing_potion"], "array": true },"#
            ),
        );
        edit(
            "worlds/main/levels/crevasse/entities.json",
            r#""waypoints": [[1, 1], [2, 1]],"#,
            r#""waypoints": [[1, 1], [2, 1]], "content": ["sword", "healing_potion"],"#,
        );
        st.doc = Some(Document::open(&dir).unwrap());
        st.view.selection.set_entities([id("doorcrev22")]);
        for width in [900.0, 330.0, 250.0] {
            let mut d = Driver::new();
            d.width = width;
            for _ in 0..3 {
                d.frame(&mut st, vec![]);
            }
            let glyphs = format!("{ARRAY_UP}{ARRAY_DOWN}{ARRAY_REMOVE}");
            assert!(
                d.ctx
                    .fonts_mut(|f| f.has_glyphs(&egui::FontId::proportional(14.0), &glyphs)),
                "{glyphs} not in the fonts"
            );
            for (field, next) in [("waypoints", "content"), ("content", "exit_to")] {
                let key = format!("doorcrev22:field:{field}");
                let at = |s: &str| d.rect(&format!("{key}{s}"));
                let inside = |r: egui::Rect, what: &str| {
                    assert!(
                        r.left() >= 0.0 && r.right() <= width,
                        "{width}: {field}{what} {r:?} past the pane"
                    );
                };
                let next_row = d.rect(&format!("doorcrev22:field:{next}"));
                assert!(
                    next_row.top() >= at("").bottom() - 0.5,
                    "{width}: {field} {:?} runs into {next} {next_row:?}",
                    at("")
                );
                let mut last = None;
                for i in 0..2 {
                    let row = at(&format!("[{i}]:row"));
                    inside(row, &format!("[{i}]"));
                    let parts: Vec<egui::Rect> = ["up", "down", "remove"]
                        .iter()
                        .map(|p| at(&format!("[{i}]:{p}")))
                        .collect();
                    for (k, r) in parts.iter().enumerate() {
                        inside(*r, &format!("[{i}] button {k}"));
                        assert!(row.contains_rect(*r), "{width}: {field}[{i}] {r:?}");
                        for o in &parts[k + 1..] {
                            assert!(!r.intersects(*o), "{width}: {field}[{i}] {r:?} {o:?}");
                        }
                    }
                    if let Some(prev) = last {
                        assert!(row.top() >= prev - 0.5, "{width}: {field}[{i}] overlaps");
                    }
                    last = Some(row.bottom());
                }
                let plus = at("[+]");
                inside(plus, "[+]");
                assert!(plus.top() >= last.unwrap() - 0.5, "{width}: {field} +");
                assert!(
                    plus.bottom() <= next_row.top() + 0.5,
                    "{width}: {field} + over {next}"
                );
                if field == "content" {
                    // Stored: the field's own ×, on +'s line.
                    let x = at(":remove");
                    inside(x, ":remove");
                    assert!((x.center().y - plus.center().y).abs() < 4.0, "{width}");
                }
            }
        }
    }

    #[test]
    fn level_array_fields_are_editable() {
        let (_tmp, mut st) = state();
        let mut d = Driver::new();
        d.frame(&mut st, vec![]);
        d.frame(&mut st, vec![]);
        // crevasse's biome: ["flooded"]; + adds the enum's first value.
        let plus = d.rect("crevasse22:field:biome[+]");
        d.click(&mut st, plus.center());
        let (label, cmds) = batch(&mut st);
        assert_eq!(label, "Set biome on crevasse");
        assert!(
            matches!(&cmds[..], [Command::SetLevelField { value: Some(v), .. }]
                if *v == json!(["flooded", "dry"])),
            "{cmds:?}"
        );
    }
}
