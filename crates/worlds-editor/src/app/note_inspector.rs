//! The Inspector's annotation and level-tag parts.
//!
//! - One annotation: kind and scope (read only), title, body, tags (comma
//!   separated), color (picker, ×), done, export (disabled for guides:
//!   "guides are never exported"), locked; links, one row each (a badge,
//!   "missing" in red when it doesn't resolve, ×), "+ Link" (a URL or
//!   `kind:id`) and Pick (an entity, a level or an annotation on the map);
//!   a pin's icon and label, a guide's group and point count, a path's
//!   length; the points as JSON (vertex editing). Each change is one
//!   `update_note` labelled "Edit note <id>: <field>".
//! - Several annotations: their common tags (add, ×), done, export, locked,
//!   color; mixed values shown as such. One batch each.
//! - Levels (selected, or the active level): `tags` (comma separated,
//!   `set_level_tags`); several selected levels: their tags in common (add,
//!   ×).

use std::collections::HashMap;

use egui::{Color32, RichText};
use worlds_core::{Annotation, Command, Link, NoteStatus, Shape, Slug, parse_link};

use super::EditorState;
use super::fields::{ERROR, mark, text_commit};
use super::inspector::push;
use crate::annot::{self, At, NoteRef};
use crate::doc::Document;
use crate::view::{PickAccept, PickedThing, ViewState};

/// "a, b ,c" into tags (trimmed, empty ones dropped, repeats dropped).
pub fn parse_tags(s: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in s.split(',').map(str::trim).filter(|t| !t.is_empty()) {
        if !out.iter().any(|x| x == t) {
            out.push(t.to_string());
        }
    }
    out
}

/// Whether a link resolves in the document (URLs always do).
pub fn link_resolves(doc: &Document, s: &str) -> bool {
    match parse_link(s) {
        Link::Level(l) => doc.level_ix(l).is_some(),
        Link::Entity(e) => worlds_core::Id::new(e).is_ok_and(|id| doc.find_entity(&id).is_some()),
        Link::Note(n) => {
            worlds_core::Id::new(n).is_ok_and(|id| annot::find_note(doc, &id).is_some())
        }
        Link::Url(_) => true,
    }
}

fn badge(s: &str) -> &'static str {
    match parse_link(s) {
        Link::Level(_) => "level",
        Link::Entity(_) => "entity",
        Link::Note(_) => "note",
        Link::Url(_) => "url",
    }
}

/// The selected annotations' part of the Inspector. Returns how many were
/// shown.
pub fn notes_section(ui: &mut egui::Ui, st: &mut EditorState) -> usize {
    let EditorState {
        doc,
        view,
        pending,
        edit_bufs,
        ..
    } = st;
    let Some(doc) = doc.as_ref() else {
        return 0;
    };
    let sel: Vec<NoteRef<'_>> = view
        .selection
        .notes()
        .filter_map(|id| annot::find_note(doc, id))
        .collect();
    match sel.as_slice() {
        [] => 0,
        [one] => {
            single(ui, doc, view, pending, edit_bufs, *one);
            1
        }
        many => {
            multi(ui, pending, edit_bufs, many);
            many.len()
        }
    }
}

fn update(
    pending: &mut Vec<Command>,
    a: &Annotation,
    field: &str,
    f: impl FnOnce(&mut Annotation),
) {
    let mut note = a.clone();
    f(&mut note);
    if note == *a {
        return;
    }
    push(
        pending,
        format!("Edit note {}: {field}", a.id),
        vec![Command::UpdateNote {
            id: a.id.clone(),
            note,
        }],
    );
}

fn single(
    ui: &mut egui::Ui,
    doc: &Document,
    view: &mut ViewState,
    pending: &mut Vec<Command>,
    bufs: &mut HashMap<String, String>,
    r: NoteRef<'_>,
) {
    let a = r.note;
    let at = r.at;
    let id = a.id.clone();
    let k = |f: &str| format!("note:{id}:{f}");
    ui.horizontal(|ui| {
        ui.heading(a.title.as_deref().unwrap_or(a.shape.name()));
        ui.weak(format!("{} · {id}", a.shape.name()));
    });
    let guide = matches!(a.shape, Shape::Guide { .. });
    if a.locked {
        ui.colored_label(
            Color32::from_rgb(230, 190, 90),
            "Locked: the tools don't move, nudge or delete it (untick Locked to)",
        );
    }
    egui::Grid::new(("note", id.as_str()))
        .num_columns(2)
        .striped(true)
        .show(ui, |ui| {
            ui.weak("Scope");
            ui.label(match at {
                At::World(w) => format!("world {}", doc.project.worlds[w].slug),
                At::Level(ix) => format!("level {}", doc.level(ix).slug),
            });
            ui.end_row();
            ui.weak("Title");
            let focus_key = k("title");
            let focus = view.focus_title.as_ref() == Some(&id);
            if focus && !bufs.contains_key(&focus_key) {
                let t = a.title.clone().unwrap_or_default();
                bufs.insert(focus_key.clone(), t);
            }
            let (edited, focused) = title_edit(
                ui,
                bufs,
                &focus_key,
                a.title.as_deref().unwrap_or(""),
                &id,
                focus,
            );
            if focused {
                view.focus_title = None;
            }
            if let Some(t) = edited {
                update(pending, a, "title", |n| {
                    n.title = Some(t.clone()).filter(|t| !t.is_empty())
                });
            }
            ui.end_row();
            ui.weak("Body");
            if let Some(t) = text_commit(
                ui,
                bufs,
                &k("body"),
                a.body.as_deref().unwrap_or(""),
                false,
                true,
                220.0,
            ) {
                update(pending, a, "body", |n| {
                    n.body = Some(t.clone()).filter(|t| !t.is_empty())
                });
            }
            ui.end_row();
            ui.weak("Tags");
            if let Some(t) = text_commit(
                ui,
                bufs,
                &k("tags"),
                &a.tags.join(", "),
                false,
                false,
                220.0,
            ) {
                update(pending, a, "tags", |n| n.tags = parse_tags(&t));
            }
            ui.end_row();
            ui.weak("Color");
            ui.horizontal(|ui| {
                if let Some(hex) = super::panels::color_field(
                    ui,
                    bufs,
                    &k("color"),
                    a.color.as_ref().map(|c| c.as_str()),
                ) && let Ok(c) = worlds_core::Color::new(hex)
                {
                    update(pending, a, "color", |n| n.color = Some(c));
                }
                if a.color.is_some()
                    && ui
                        .small_button("×")
                        .on_hover_text("Default color")
                        .clicked()
                {
                    update(pending, a, "color", |n| n.color = None);
                }
            });
            ui.end_row();
            ui.weak("Done");
            let mut done = a.status == NoteStatus::Done;
            let r = ui.checkbox(&mut done, "");
            mark(ui, &k("status"), r.rect);
            if r.changed() {
                update(pending, a, "status", |n| {
                    n.status = if done {
                        NoteStatus::Done
                    } else {
                        NoteStatus::Open
                    }
                });
            }
            ui.end_row();
            ui.weak("Export");
            let mut ex = a.export;
            let r = ui
                .add_enabled(!guide, egui::Checkbox::new(&mut ex, ""))
                .on_hover_text("Included in export/ for the game")
                .on_disabled_hover_text("guides are never exported");
            mark(ui, &k("export"), r.rect);
            if r.changed() && !guide {
                update(pending, a, "export", |n| n.export = ex);
            }
            ui.end_row();
            ui.weak("Locked");
            let mut lk = a.locked;
            let r = ui.checkbox(&mut lk, "");
            mark(ui, &k("locked"), r.rect);
            if r.changed() {
                update(pending, a, "locked", |n| n.locked = lk);
            }
            ui.end_row();
            match &a.shape {
                Shape::Pin { icon, label, .. } => {
                    ui.weak("Icon");
                    ui.vertical(|ui| {
                        let cur = icon.clone().unwrap_or_else(|| "pin".into());
                        egui::ComboBox::from_id_salt(k("icon"))
                            .selected_text(&cur)
                            .show_ui(ui, |ui| {
                                for name in annot::PIN_ICONS {
                                    if ui.selectable_label(cur == name, name).clicked()
                                        && cur != name
                                    {
                                        update(pending, a, "icon", |n| {
                                            if let Shape::Pin { icon, .. } = &mut n.shape {
                                                *icon = Some(name.to_string());
                                            }
                                        });
                                    }
                                }
                            });
                        if !annot::PIN_ICONS.contains(&cur.as_str()) {
                            ui.colored_label(ERROR, "unknown icon (drawn as pin)");
                        }
                    });
                    ui.end_row();
                    ui.weak("Label");
                    if let Some(t) = text_commit(
                        ui,
                        bufs,
                        &k("label"),
                        label.as_deref().unwrap_or(""),
                        false,
                        false,
                        140.0,
                    ) {
                        update(pending, a, "label", |n| {
                            if let Shape::Pin { label, .. } = &mut n.shape {
                                *label = Some(t.clone()).filter(|t| !t.is_empty());
                            }
                        });
                    }
                    ui.end_row();
                }
                Shape::Guide { group, points, .. } => {
                    ui.weak("Group");
                    let groups: Vec<Slug> = match at {
                        At::World(w) => doc.project.worlds[w]
                            .guide_groups
                            .iter()
                            .map(|g| g.slug.clone())
                            .collect(),
                        At::Level(_) => Vec::new(),
                    };
                    let cur = group.as_ref().map_or("none".to_string(), |g| g.to_string());
                    // The group moves a guide: not while it is locked.
                    let locked = a.locked;
                    ui.add_enabled_ui(!locked, |ui| {
                        let r = egui::ComboBox::from_id_salt(k("group"))
                            .selected_text(&cur)
                            .show_ui(ui, |ui| {
                                let mut pick: Option<Option<Slug>> = None;
                                if ui.selectable_label(group.is_none(), "none").clicked() {
                                    pick = Some(None);
                                }
                                for g in &groups {
                                    if ui
                                        .selectable_label(group.as_ref() == Some(g), g.as_str())
                                        .clicked()
                                    {
                                        pick = Some(Some(g.clone()));
                                    }
                                }
                                if let Some(p) = pick.filter(|_| !locked) {
                                    update(pending, a, "group", |n| {
                                        if let Shape::Guide { group, .. } = &mut n.shape {
                                            *group = p;
                                        }
                                    });
                                }
                            });
                        mark(ui, &k("group"), r.response.rect);
                    });
                    ui.end_row();
                    ui.weak("Points");
                    ui.label(points.len().to_string());
                    ui.end_row();
                }
                Shape::Path { points } => {
                    let len = annot::path_length(points);
                    ui.weak("Length");
                    ui.label(format!(
                        "{len:.0} px · {:.1} tiles",
                        len / doc.project.grid_size.max(1) as f64
                    ));
                    ui.end_row();
                }
                _ => {}
            }
            // Geometry as JSON (vertex editing).
            ui.weak("Geometry");
            let geo = geometry_json(a);
            // Geometry moves it: read only while locked, as the tools and
            // the API (which needs confirm) treat it.
            let typed = ui.add_enabled_ui(!a.locked, |ui| {
                text_commit(ui, bufs, &k("geometry"), &geo, false, false, 220.0)
            });
            typed
                .response
                .on_disabled_hover_text("Locked: untick Locked to move it");
            if let Some(t) = typed.inner.filter(|_| !a.locked) {
                match set_geometry(a, &t) {
                    Ok(n) => update(pending, a, "geometry", |x| *x = n),
                    Err(e) => view.set_status(format!("Geometry: {e}"), true),
                }
            }
            ui.end_row();
        });
    // ---- links ----
    ui.add_space(4.0);
    ui.strong("Links");
    let mut remove = None;
    for (i, l) in a.links.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(badge(l))
                    .small()
                    .background_color(Color32::from_gray(60)),
            );
            ui.label(l);
            if !link_resolves(doc, l) {
                ui.colored_label(ERROR, "missing");
            }
            let r = ui.small_button("×").on_hover_text("Remove the link");
            mark(ui, &format!("{}[{i}]:remove", k("links")), r.rect);
            if r.clicked() {
                remove = Some(i);
            }
        });
    }
    if let Some(i) = remove {
        update(pending, a, "links", |n| {
            n.links.remove(i);
        });
    }
    ui.horizontal(|ui| {
        ui.weak("+ Link");
        if let Some(t) = text_commit(ui, bufs, &k("links:add"), "", false, false, 160.0)
            && !t.trim().is_empty()
        {
            let t = t.trim().to_string();
            update(pending, a, "links", |n| n.links.push(t));
        }
        let pick_key = k("links:pick");
        let armed = view.pick.as_ref().is_some_and(|p| p.target == pick_key);
        let b = ui
            .add(egui::Button::selectable(armed, "Pick"))
            .on_hover_text("Click an entity, an annotation or a level on the map");
        mark(ui, &pick_key, b.rect);
        if b.clicked() {
            if armed {
                view.pick = None;
            } else {
                view.start_pick(
                    PickAccept {
                        entity: true,
                        level: true,
                        note: true,
                        cell: false,
                    },
                    pick_key.clone(),
                    "Click an entity, an annotation or a level to link".into(),
                );
            }
        }
        if let Some(p) = view.take_picked(&pick_key) {
            let link = match p {
                PickedThing::Entity { id, .. } => Some(format!("entity:{id}")),
                PickedThing::Note { id } => Some(format!("note:{id}")),
                PickedThing::Level { level } => Some(format!("level:{}", doc.level(level).slug)),
                PickedThing::Cell { level, .. } => Some(format!("level:{}", doc.level(level).slug)),
            };
            if let Some(l) = link {
                if a.links.contains(&l) {
                    view.set_status(format!("Already linked: {l}"), false);
                } else {
                    update(pending, a, "links", |n| n.links.push(l));
                }
            }
        }
    });
}

/// The title box: a single line that commits on Enter or focus loss, with
/// a stable id so a new note can focus it.
fn title_edit(
    ui: &mut egui::Ui,
    bufs: &mut HashMap<String, String>,
    key: &str,
    initial: &str,
    id: &worlds_core::Id,
    focus: bool,
) -> (Option<String>, bool) {
    let mut s = bufs
        .get(key)
        .cloned()
        .unwrap_or_else(|| initial.to_string());
    let r = ui.add(
        egui::TextEdit::singleline(&mut s)
            .id(egui::Id::new(("focus title", id.as_str())))
            .desired_width(220.0),
    );
    mark(ui, key, r.rect);
    // Asked to take the keyboard (a note just added): after the field is
    // added, and only once it is really there. A new Grid's first frame is
    // an invisible sizing pass whose widgets are disabled, and a disabled
    // widget gives the focus straight back: asking before adding it (as this
    // did) lost the request there, so typing went to the shortcuts.
    let focused = focus && ui.is_enabled() && ui.is_visible() && !ui.is_sizing_pass();
    if focused {
        r.request_focus();
        // All of it selected, so typing replaces the placeholder title.
        let mut state = egui::text_edit::TextEditState::load(ui.ctx(), r.id).unwrap_or_default();
        state
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::two(
                egui::text::CCursor::new(0),
                egui::text::CCursor::new(s.chars().count()),
            )));
        state.store(ui.ctx(), r.id);
    }
    if r.changed() {
        bufs.insert(key.to_string(), s);
    }
    if r.lost_focus() {
        let t = bufs.remove(key);
        if let Some(t) = t
            && t != initial
        {
            return (Some(t), focused);
        }
    }
    (None, focused)
}

/// The geometry keys as JSON (`{"px": [x, y]}`, `{"points": [...]}`,
/// `{"rect": [x, y, w, h]}`).
pub fn geometry_json(a: &Annotation) -> String {
    match &a.shape {
        Shape::Note { px } | Shape::Pin { px, .. } => serde_json::json!({ "px": px }).to_string(),
        Shape::Region { rect: Some(r), .. } => serde_json::json!({ "rect": r }).to_string(),
        Shape::Region { points, .. } | Shape::Path { points } | Shape::Guide { points, .. } => {
            serde_json::json!({ "points": points }).to_string()
        }
    }
}

/// `a` with the geometry of a [`geometry_json`] text.
pub fn set_geometry(a: &Annotation, text: &str) -> Result<Annotation, String> {
    let v: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let mut n = a.clone();
    let get = |k: &str| v.get(k).cloned();
    match &mut n.shape {
        Shape::Note { px } | Shape::Pin { px, .. } => {
            *px = serde_json::from_value(get("px").ok_or("expected {\"px\": [x, y]}")?)
                .map_err(|e| e.to_string())?;
        }
        Shape::Region { points, rect } => {
            if let Some(r) = get("rect") {
                *rect = Some(serde_json::from_value(r).map_err(|e| e.to_string())?);
                points.clear();
            } else {
                *points = serde_json::from_value(get("points").ok_or("expected rect or points")?)
                    .map_err(|e| e.to_string())?;
                *rect = None;
            }
        }
        Shape::Path { points } | Shape::Guide { points, .. } => {
            *points = serde_json::from_value(get("points").ok_or("expected {\"points\": [...]}")?)
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(n)
}

fn multi(
    ui: &mut egui::Ui,
    pending: &mut Vec<Command>,
    bufs: &mut HashMap<String, String>,
    sel: &[NoteRef<'_>],
) {
    let n = sel.len();
    ui.heading(format!("{n} annotations"));
    let all = |f: &dyn Fn(&Annotation) -> bool| sel.iter().all(|r| f(r.note));
    let any = |f: &dyn Fn(&Annotation) -> bool| sel.iter().any(|r| f(r.note));
    let batch = |pending: &mut Vec<Command>, field: &str, f: &dyn Fn(&mut Annotation)| {
        let cmds: Vec<Command> = sel
            .iter()
            .filter_map(|r| {
                let mut note = r.note.clone();
                f(&mut note);
                (note != *r.note).then(|| Command::UpdateNote {
                    id: note.id.clone(),
                    note,
                })
            })
            .collect();
        push(pending, format!("Edit {n} notes: {field}"), cmds);
    };
    let key = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for r in sel {
            r.note.id.hash(&mut h);
        }
        format!("notes{:x}", h.finish())
    };
    let common: Vec<String> = sel[0]
        .note
        .tags
        .iter()
        .filter(|t| sel.iter().all(|r| r.note.tags.contains(t)))
        .cloned()
        .collect();
    egui::Grid::new("notes multi")
        .num_columns(2)
        .striped(true)
        .show(ui, |ui| {
            ui.weak("Tags in common");
            ui.horizontal_wrapped(|ui| {
                for t in &common {
                    let r = ui
                        .small_button(format!("{t} ×"))
                        .on_hover_text("Remove from all of them");
                    mark(ui, &format!("{key}:tag:{t}"), r.rect);
                    if r.clicked() {
                        batch(pending, "tags", &|a| a.tags.retain(|x| x != t));
                    }
                }
                if let Some(t) =
                    text_commit(ui, bufs, &format!("{key}:tags:add"), "", false, false, 90.0)
                {
                    let add = parse_tags(&t);
                    batch(pending, "tags", &|a| {
                        for t in &add {
                            if !a.tags.contains(t) {
                                a.tags.push(t.clone());
                            }
                        }
                    });
                }
            });
            ui.end_row();
            for (field, label) in [
                ("status", "Done"),
                ("export", "Export"),
                ("locked", "Locked"),
            ] {
                let get = |a: &Annotation| match field {
                    "status" => a.status == NoteStatus::Done,
                    "export" => a.export,
                    _ => a.locked,
                };
                ui.weak(label);
                let all_on = all(&get);
                let mixed = !all_on && any(&get);
                let mut v = all_on;
                let r = ui.add(egui::Checkbox::new(&mut v, "").indeterminate(mixed));
                mark(ui, &format!("{key}:{field}"), r.rect);
                if r.changed() {
                    batch(pending, field, &|a| match field {
                        "status" => {
                            a.status = if v {
                                NoteStatus::Done
                            } else {
                                NoteStatus::Open
                            }
                        }
                        "export" => {
                            if !matches!(a.shape, Shape::Guide { .. }) {
                                a.export = v
                            }
                        }
                        _ => a.locked = v,
                    });
                }
                ui.end_row();
            }
            ui.weak("Color");
            ui.horizontal(|ui| {
                let first = sel[0].note.color.as_ref().map(|c| c.as_str());
                let same = sel
                    .iter()
                    .all(|r| r.note.color.as_ref().map(|c| c.as_str()) == first);
                if let Some(hex) = super::panels::color_field(
                    ui,
                    bufs,
                    &format!("{key}:color"),
                    if same { first } else { None },
                ) && let Ok(c) = worlds_core::Color::new(hex)
                {
                    batch(pending, "color", &|a| a.color = Some(c.clone()));
                }
                if !same {
                    ui.weak("—");
                }
            });
            ui.end_row();
        });
}

/// The selected levels' tags (one: its tags; several: the tags they have
/// in common, add / ×). Returns how many levels were shown.
pub fn levels_section(ui: &mut egui::Ui, st: &mut EditorState) -> usize {
    let EditorState {
        doc,
        view,
        pending,
        edit_bufs,
        ..
    } = st;
    let Some(doc) = doc.as_ref() else {
        return 0;
    };
    let levels: Vec<usize> = view
        .selection
        .levels()
        .filter_map(|id| doc.level_ix_by_id(id))
        .collect();
    match levels.as_slice() {
        [] => 0,
        [one] => {
            ui.heading(format!("Level {}", doc.level(*one).slug));
            level_tags_row(ui, doc, pending, edit_bufs, *one);
            1
        }
        many => {
            ui.heading(format!("{} levels", many.len()));
            ui.weak(
                many.iter()
                    .map(|&i| doc.level(i).slug.to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
            );
            let common: Vec<String> = doc
                .level(many[0])
                .tags
                .iter()
                .filter(|t| many.iter().all(|&i| doc.level(i).tags.contains(t)))
                .cloned()
                .collect();
            let cmds = |f: &dyn Fn(&mut Vec<String>)| -> Vec<Command> {
                many.iter()
                    .filter_map(|&i| {
                        let l = doc.level(i);
                        let mut tags = l.tags.clone();
                        f(&mut tags);
                        (tags != l.tags).then(|| Command::SetLevelTags {
                            world: doc.project.worlds[doc.levels[i].world].slug.clone(),
                            level: l.slug.clone(),
                            tags,
                        })
                    })
                    .collect()
            };
            ui.horizontal_wrapped(|ui| {
                ui.weak("Tags in common");
                for t in &common {
                    let r = ui.small_button(format!("{t} ×"));
                    mark(ui, &format!("levels:tag:{t}"), r.rect);
                    if r.clicked() {
                        let c = cmds(&|v| v.retain(|x| x != t));
                        push(pending, format!("Tag {} levels", many.len()), c);
                    }
                }
                if let Some(t) =
                    text_commit(ui, edit_bufs, "levels:tags:add", "", false, false, 90.0)
                {
                    let add = parse_tags(&t);
                    let c = cmds(&|v| {
                        for t in &add {
                            if !v.contains(t) {
                                v.push(t.clone());
                            }
                        }
                    });
                    push(pending, format!("Tag {} levels", many.len()), c);
                }
            });
            many.len()
        }
    }
}

/// A level's `tags` row (comma separated; `set_level_tags`).
pub fn level_tags_row(
    ui: &mut egui::Ui,
    doc: &Document,
    pending: &mut Vec<Command>,
    bufs: &mut HashMap<String, String>,
    ix: usize,
) {
    let l = doc.level(ix);
    ui.horizontal(|ui| {
        ui.weak("Tags");
        let key = format!("{}:tags", l.id);
        if let Some(t) = text_commit(ui, bufs, &key, &l.tags.join(", "), false, false, 200.0) {
            let tags = parse_tags(&t);
            if tags != l.tags {
                push(
                    pending,
                    format!("Tag level {}", l.slug),
                    vec![Command::SetLevelTags {
                        world: doc.project.worlds[doc.levels[ix].world].slug.clone(),
                        level: l.slug.clone(),
                        tags,
                    }],
                );
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::super::testkit::{Driver, batches, state};
    use crate::view::{PickedThing, SelItem};
    use worlds_core::{Command, Id, Shape};

    fn id(s: &str) -> Id {
        Id::new(s).unwrap()
    }

    fn driver() -> Driver {
        Driver::new(super::super::inspector::inspector)
    }

    #[test]
    fn a_title_edit_is_one_update_and_one_entry() {
        let (_tmp, mut st) = state();
        st.view.selection.set([SelItem::Note(id("notecrev22"))]);
        let mut d = driver();
        d.frame(&mut st, vec![]);
        d.frame(&mut st, vec![]);
        let r = d.rect("note:notecrev22:title");
        d.click(&mut st, r.center());
        // Select all, then type over it.
        let sel_all = egui::Event::Key {
            key: egui::Key::A,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::COMMAND,
        };
        d.frame(&mut st, vec![sel_all]);
        d.type_enter(&mut st, "Wider gap");
        let b = batches(&mut st);
        assert_eq!(b.len(), 1, "{b:?}");
        assert_eq!(b[0].0, "Edit note notecrev22: title");
        let [Command::UpdateNote { note, .. }] = &b[0].1[..] else {
            panic!("{b:?}")
        };
        assert_eq!(note.title.as_deref(), Some("Wider gap"));
        // Applied: one undo entry.
        let mut ed = crate::edit::Editing::new();
        let mut inv = Vec::new();
        let super::super::EditorState { doc, view, .. } = &mut st;
        let mut cx = crate::tools::CommandContext {
            doc: doc.as_mut().unwrap(),
            view,
            invalidate: &mut inv,
        };
        use crate::tools::EditorCommands;
        let (label, commands) = b.into_iter().next().unwrap();
        ed.apply(&mut cx, Command::Batch { label, commands })
            .unwrap();
        assert_eq!(ed.history().undo_entries().len(), 1);
        let n = crate::annot::find_note(cx.doc, &id("notecrev22")).unwrap();
        assert_eq!(n.note.title.as_deref(), Some("Wider gap"));
    }

    /// A note just added (`focus_title`, as the Note tool sets it): its
    /// title takes the keyboard, asked once, with the text selected, so
    /// typing replaces it. (`tests/editor_ui.rs` checks it in the real app,
    /// shortcuts included.)
    #[test]
    fn a_new_notes_title_takes_the_keyboard() {
        let (_tmp, mut st) = state();
        let note = id("notecrev22");
        st.view.selection.set([SelItem::Note(note.clone())]);
        st.view.focus_title = Some(note.clone());
        let mut d = driver();
        d.frame(&mut st, vec![]);
        d.frame(&mut st, vec![]);
        let field = egui::Id::new(("focus title", "notecrev22"));
        assert_eq!(d.ctx.memory(|m| m.focused()), Some(field));
        assert!(d.ctx.egui_wants_keyboard_input());
        assert_eq!(st.view.focus_title, None, "asked once");
        d.type_enter(&mut st, "Gap");
        let b = batches(&mut st);
        let [(_, cmds)] = &b[..] else { panic!("{b:?}") };
        let [Command::UpdateNote { note, .. }] = &cmds[..] else {
            panic!("{b:?}")
        };
        assert_eq!(note.title.as_deref(), Some("Gap"), "typed over");
    }

    #[test]
    fn a_link_is_picked_on_a_note_and_on_a_level() {
        let (_tmp, mut st) = state();
        st.view.selection.set([SelItem::Note(id("pinstatue2"))]);
        let mut d = driver();
        d.frame(&mut st, vec![]);
        // Pick arms a pick taking entities, levels and notes.
        let pick = d.rect("note:pinstatue2:links:pick");
        d.click(&mut st, pick.center());
        let req = st.view.pick.clone().expect("armed");
        assert!(req.accept.entity && req.accept.level && req.accept.note && !req.accept.cell);
        // The map click answers it: on the note's card, a note; inside a
        // level away from annotations, the level.
        let doc = st.doc.as_ref().unwrap();
        let shaft = doc.level_ix("shaft").unwrap();
        crate::viewport::pick_at(doc, &mut st.view, Some(0), Some([162.0, 50.0]), 4.0);
        assert_eq!(
            st.view.picked.as_ref().map(|p| p.what.clone()),
            Some(PickedThing::Note {
                id: id("notecrev22")
            })
        );
        d.frame(&mut st, vec![]);
        let b = batches(&mut st);
        let [Command::UpdateNote { note, .. }] = &b[0].1[..] else {
            panic!("{b:?}")
        };
        assert_eq!(note.links, ["level:crevasse", "note:notecrev22"]);
        assert_eq!(b[0].0, "Edit note pinstatue2: links");
        st.view.pick = Some(req);
        let doc = st.doc.as_ref().unwrap();
        crate::viewport::pick_at(doc, &mut st.view, Some(shaft), Some([380.0, 180.0]), 4.0);
        assert_eq!(
            st.view.picked.as_ref().map(|p| p.what.clone()),
            Some(PickedThing::Level { level: shaft })
        );
        d.frame(&mut st, vec![]);
        let b = batches(&mut st);
        let [Command::UpdateNote { note, .. }] = &b[0].1[..] else {
            panic!("{b:?}")
        };
        assert_eq!(note.links, ["level:crevasse", "level:shaft"]);
        // A link that doesn't resolve is missing; a URL never is.
        let doc = st.doc.as_ref().unwrap();
        assert!(!super::link_resolves(doc, "note:gonegonego"));
        assert!(super::link_resolves(doc, "entity:btncrev222"));
        assert!(super::link_resolves(doc, "https://example.com"));
    }

    #[test]
    fn a_guides_export_box_is_disabled() {
        let (_tmp, mut st) = state();
        st.view.selection.set([SelItem::Note(id("guidestem2"))]);
        let mut d = driver();
        d.frame(&mut st, vec![]);
        d.frame(&mut st, vec![]);
        d.click(&mut st, d.rect("note:guidestem2:export").center());
        assert!(st.pending.is_empty(), "{:?}", st.pending);
        // Locked can be unticked (the inspector unlocks).
        d.click(&mut st, d.rect("note:guidestem2:locked").center());
        let b = batches(&mut st);
        let [Command::UpdateNote { note, .. }] = &b[0].1[..] else {
            panic!("{b:?}")
        };
        assert!(!note.locked && matches!(note.shape, Shape::Guide { .. }));
        assert_eq!(b[0].0, "Edit note guidestem2: locked");
    }

    /// A2: a locked guide's Geometry and Group can't be edited (they move
    /// it); once unlocked they can.
    #[test]
    fn a_locked_guides_placement_is_read_only() {
        let (_tmp, mut st) = state();
        st.view.selection.set([SelItem::Note(id("guidestem2"))]);
        let mut d = driver();
        d.frame(&mut st, vec![]);
        d.frame(&mut st, vec![]);
        let sel_all = || egui::Event::Key {
            key: egui::Key::A,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::COMMAND,
        };
        d.click(&mut st, d.rect("note:guidestem2:geometry").center());
        d.frame(&mut st, vec![sel_all()]);
        d.type_enter(&mut st, r#"{"kind": "guide", "points": [[5, 5], [9, 9]]}"#);
        d.click(&mut st, d.rect("note:guidestem2:group").center());
        d.frame(&mut st, vec![]);
        assert!(st.pending.is_empty(), "{:?}", st.pending);
        // Unlocked: the same typing is one update.
        let doc = st.doc.as_mut().unwrap();
        doc.project.worlds[0].notes[0].locked = false;
        d.frame(&mut st, vec![]);
        d.click(&mut st, d.rect("note:guidestem2:geometry").center());
        d.frame(&mut st, vec![sel_all()]);
        d.type_enter(&mut st, r#"{"kind": "guide", "points": [[5, 5], [9, 9]]}"#);
        let b = batches(&mut st);
        assert_eq!(b.len(), 1, "{b:?}");
        assert_eq!(b[0].0, "Edit note guidestem2: geometry");
    }

    #[test]
    fn level_tags_are_set_from_the_level_part() {
        let (_tmp, mut st) = state();
        let mut d = driver();
        d.frame(&mut st, vec![]);
        d.frame(&mut st, vec![]);
        d.click(&mut st, d.rect("crevasse22:tags").center());
        d.type_enter(&mut st, "forest, ruined, forest");
        let b = batches(&mut st);
        assert_eq!(b[0].0, "Tag level crevasse");
        assert_eq!(
            b[0].1,
            [Command::SetLevelTags {
                world: worlds_core::Slug::new("main").unwrap(),
                level: worlds_core::Slug::new("crevasse").unwrap(),
                tags: vec!["forest".into(), "ruined".into()],
            }]
        );
        // Two selected levels: their tags in common, added to both.
        let doc = st.doc.as_ref().unwrap();
        let ids = [doc.ids[0].clone(), doc.ids[1].clone()];
        st.view.selection.set(ids.map(SelItem::Level));
        d.frame(&mut st, vec![]);
        d.click(&mut st, d.rect("levels:tags:add").center());
        d.type_enter(&mut st, "caves");
        let b = batches(&mut st);
        assert_eq!(b[0].1.len(), 2, "{b:?}");
        assert!(b[0].1.iter().all(
            |c| matches!(c, Command::SetLevelTags { tags, .. } if tags == &["caves".to_string()])
        ));
    }
}
