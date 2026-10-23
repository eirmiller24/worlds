//! The Types pane (`Pane::EntityDefs`, tab "Types", next to the
//! Inspector): entity types and the level fields, edited with the same
//! field editors as the Inspector.
//!
//! - **List**: a filter, every entity type (name, slug, instance count) and
//!   a "Level fields" entry; New type.
//! - **A type**: Duplicate (a `set_entity_def` under a new slug), Rename
//!   (`rename_entity_type`, instances and references follow), Delete (asks
//!   first and says how many instances go; `remove_entity_def`); the form:
//!   name, category, tags, doc, size, resizable, pivot, icon (an atlas
//!   pick), color, limit, each change one `set_entity_def`.
//! - **Fields** (of a type, or the level fields): drag `☰` to reorder
//!   (`set_*_field_def` with `index`), duplicate (a new slug right after),
//!   remove (says how many stored values go with it; undo brings them
//!   back), + Field; the selected field's form: slug (Rename:
//!   `rename_*_field`), name, doc, type, array, nullable, default (the
//!   Inspector's editors), and the type's options (min/max, regex, enum,
//!   values, allowed types and tags, cross level, extensions, tileset),
//!   display. Each change is one `set_*_field_def`.
//!
//! Every edit is one labelled undo entry. The pane's commands go through
//! [`TypesState::pending`], so a refusal (a rename onto a slug that is
//! taken, or that an instance already stores a value under) comes back to
//! the pane and is shown there.

use std::collections::HashMap;

use egui::{Color32, Rect, RichText, Sense, Stroke};
use worlds_core::{
    Command, EntityDef, EnumSource, FieldDef, FieldType, Limit, LimitBehavior, LimitScope, Slug,
    TileRef,
};

use super::EditorState;
use super::fields::{ERROR, FieldCx, FieldEdit, field_row, text_commit};
use super::inspector::push;
use crate::doc::Document;

/// What the pane shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypesSel {
    Entity(Slug),
    LevelFields,
}

/// A delete waiting for its confirmation.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Confirm {
    DeleteType(Slug),
    RemoveField(Slug),
}

/// The Types pane's state.
#[derive(Default)]
pub struct TypesState {
    pub open: Option<TypesSel>,
    /// The field whose form shows.
    pub field: Option<Slug>,
    /// Bring the pane to the front next frame.
    pub focus: bool,
    /// The last refusal of a command the pane sent.
    pub error: Option<String>,
    /// The pane's commands, applied after the frame's panels like
    /// [`EditorState::pending`]; a failure lands in [`Self::error`].
    pub pending: Vec<Command>,
    confirm: Option<Confirm>,
    filter: String,
    /// Open (or close) the type form's header next frame.
    pub force_type_open: Option<bool>,
    /// Scroll the field form into view next frame.
    pub reveal_field: bool,
    /// Scroll the list to the open type next frame.
    reveal_type: bool,
    /// Field rows as drawn last frame, by slug (tests).
    #[doc(hidden)]
    pub rects: HashMap<String, Rect>,
    /// What the pane follows once its pending commands all apply (a
    /// rename of the open type or field); dropped when one is refused, so
    /// the form stays on what is still there.
    pub follow: Option<Follow>,
}

/// A rename the Types pane follows after it applies ([`TypesState::follow`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Follow {
    /// The open type is now called this.
    Type(Slug),
    /// The selected field is now called this.
    Field(Slug),
}

impl TypesState {
    /// The frame's pending commands were applied: `ok` if none was refused.
    /// Follows a rename the pane asked for only when it went through.
    pub fn applied(&mut self, ok: bool) {
        let Some(f) = self.follow.take() else {
            return;
        };
        if !ok {
            return;
        }
        match f {
            Follow::Type(to) => {
                self.open = Some(TypesSel::Entity(to));
                self.reveal_type = true;
            }
            Follow::Field(to) => self.field = Some(to),
        }
    }

    /// Show `sel` (and no field form).
    pub fn show(&mut self, sel: TypesSel) {
        if self.open.as_ref() != Some(&sel) {
            self.field = None;
            self.confirm = None;
        }
        self.open = Some(sel);
        self.focus = true;
        self.error = None;
        self.reveal_type = true;
    }
}

/// A slug not taken yet: `base`, else `base_2`, `base_3`, ...
pub fn unique_slug(base: &str, taken: impl Fn(&str) -> bool) -> Slug {
    let mut n = 1;
    loop {
        let s = if n == 1 {
            base.to_string()
        } else {
            format!("{base}_{n}")
        };
        if !taken(&s)
            && let Ok(slug) = Slug::new(&s)
        {
            return slug;
        }
        n += 1;
    }
}

/// Duplicate type `ty`: a copy under a new slug (`<slug>_copy`, ...) named
/// "`<name> copy`". Returns the command and the new slug.
pub fn duplicate_type(doc: &Document, ty: &str) -> Option<(Command, Slug)> {
    let def = doc.project.entity_def(ty)?;
    let slug = unique_slug(&format!("{ty}_copy"), |s| {
        doc.project.entity_def(s).is_some()
    });
    let copy = EntityDef {
        slug: slug.clone(),
        name: format!("{} copy", def.name),
        ..def.clone()
    };
    Some((
        Command::Batch {
            label: format!("Duplicate type {ty} as {slug}"),
            commands: vec![Command::SetEntityDef {
                entity: slug.clone(),
                def: copy,
            }],
        },
        slug,
    ))
}

/// How many instances of `ty` the project has.
fn instance_count(doc: &Document, ty: &Slug) -> usize {
    doc.project
        .levels()
        .flat_map(|l| l.all_entities())
        .filter(|e| e.ty == *ty)
        .count()
}

/// How many holders (instances of `owner`, or levels) store field `field`.
pub fn stored_count(doc: &Document, owner: &TypesSel, field: &Slug) -> usize {
    match owner {
        TypesSel::Entity(ty) => doc.project.entity_field_holders(ty, field),
        TypesSel::LevelFields => doc.project.level_field_holders(field),
    }
}

/// The final position for a field dragged from `from` onto row `target`
/// (`after`: its lower half), as `set_*_field_def`'s `index` means it
/// (the position in the list once it moved). `None`: it stays.
pub fn drop_index(from: usize, target: usize, after: bool) -> Option<usize> {
    let mut to = target + usize::from(after);
    if from < to {
        to -= 1;
    }
    (to != from).then_some(to)
}

/// The field definition commands of the shown owner.
struct Owner<'a> {
    sel: TypesSel,
    fields: &'a [FieldDef],
}

impl Owner<'_> {
    fn name(&self) -> String {
        match &self.sel {
            TypesSel::Entity(t) => t.to_string(),
            TypesSel::LevelFields => "level fields".into(),
        }
    }

    fn set(&self, def: FieldDef, index: Option<usize>) -> Command {
        match &self.sel {
            TypesSel::Entity(t) => Command::SetEntityFieldDef {
                entity: t.clone(),
                field: def.slug.clone(),
                def,
                index,
            },
            TypesSel::LevelFields => Command::SetLevelFieldDef {
                field: def.slug.clone(),
                def,
                index,
            },
        }
    }

    fn remove(&self, field: &Slug) -> Command {
        match &self.sel {
            TypesSel::Entity(t) => Command::RemoveEntityFieldDef {
                entity: t.clone(),
                field: field.clone(),
                keep_values: false,
            },
            TypesSel::LevelFields => Command::RemoveLevelFieldDef {
                field: field.clone(),
                keep_values: false,
            },
        }
    }

    fn rename(&self, from: &Slug, to: Slug) -> Command {
        match &self.sel {
            TypesSel::Entity(t) => Command::RenameEntityField {
                entity: t.clone(),
                from: from.clone(),
                to,
            },
            TypesSel::LevelFields => Command::RenameLevelField {
                from: from.clone(),
                to,
            },
        }
    }

    /// The definition field `base` of this owner has after queued command
    /// `c`, if `c` touches it: a replacement, or a rename to a new slug.
    fn queued(&self, c: &Command, base: &FieldDef) -> Option<FieldDef> {
        let slug = &base.slug;
        let renamed = |to: &Slug| FieldDef {
            slug: to.clone(),
            ..base.clone()
        };
        match (&self.sel, c) {
            (
                TypesSel::Entity(t),
                Command::SetEntityFieldDef {
                    entity, field, def, ..
                },
            ) if entity == t && field == slug => Some(def.clone()),
            (TypesSel::LevelFields, Command::SetLevelFieldDef { field, def, .. })
                if field == slug =>
            {
                Some(def.clone())
            }
            (TypesSel::Entity(t), Command::RenameEntityField { entity, from, to })
                if entity == t && from == slug =>
            {
                Some(renamed(to))
            }
            (TypesSel::LevelFields, Command::RenameLevelField { from, to }) if from == slug => {
                Some(renamed(to))
            }
            _ => None,
        }
    }

    /// What one stored value lives in ("entities", "levels").
    fn holders(&self) -> &'static str {
        match self.sel {
            TypesSel::Entity(_) => "entities",
            TypesSel::LevelFields => "levels",
        }
    }
}

/// The Types pane.
pub fn pane(ui: &mut egui::Ui, st: &mut EditorState) {
    let EditorState {
        doc,
        view,
        edit_bufs: bufs,
        atlas,
        types: ts,
        accent,
        ..
    } = st;
    let Some(doc) = doc.as_ref() else {
        ui.weak("No project");
        return;
    };
    ts.rects.clear();
    // A type that went away (undo, a rename): back to the list.
    if let Some(TypesSel::Entity(t)) = &ts.open
        && doc.project.entity_def(t.as_str()).is_none()
    {
        ts.open = None;
        ts.field = None;
    }
    list_header(ui, doc, ts);
    egui::ScrollArea::vertical()
        .id_salt("types pane")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            list(ui, doc, ts);
            if let Some(e) = ts.error.clone() {
                ui.horizontal(|ui| {
                    ui.colored_label(ERROR, e);
                    if ui.small_button("×").clicked() {
                        ts.error = None;
                    }
                });
            }
            ui.separator();
            let open = ts.open.clone();
            let (sel, fields): (TypesSel, &[FieldDef]) = match &open {
                Some(TypesSel::Entity(t)) => match doc.project.entity_def(t.as_str()) {
                    Some(def) => {
                        type_form(ui, doc, bufs, atlas, ts, def);
                        (TypesSel::Entity(t.clone()), &def.fields)
                    }
                    None => return,
                },
                Some(TypesSel::LevelFields) => {
                    ui.heading("Level fields");
                    ui.weak("Fields every level has (project.json level_fields).");
                    (TypesSel::LevelFields, &doc.project.level_fields)
                }
                None => {
                    ui.weak("Pick a type above, or the level fields.");
                    return;
                }
            };
            let owner = Owner { sel, fields };
            ui.add_space(8.0);
            field_list(ui, doc, ts, &owner, *accent);
            if let Some(f) = ts
                .field
                .clone()
                .and_then(|s| owner.fields.iter().find(|f| f.slug == s))
            {
                ui.separator();
                let mut fx = FieldCx {
                    doc,
                    bufs,
                    atlas,
                    view,
                    level: None,
                    pick_cells: true,
                    point_grid: doc.project.grid_size,
                };
                let r = ui
                    .scope(|ui| field_form(ui, &mut fx, ts, &owner, f))
                    .response;
                if std::mem::take(&mut ts.reveal_field) {
                    r.scroll_to_me(None);
                }
            }
        });
    ts.reveal_type = false;
}

/// The list of types, Level fields, and New type.
/// The filter and New type, above the pane's scroll area so they stay put
/// when it scrolls.
fn list_header(ui: &mut egui::Ui, doc: &Document, ts: &mut TypesState) {
    ui.horizontal(|ui| {
        let r = ui.add(
            egui::TextEdit::singleline(&mut ts.filter)
                .hint_text("Filter types…")
                .desired_width(140.0),
        );
        ts.rects.insert("types filter".into(), r.rect);
        if ui.button("New type").clicked() {
            let slug = unique_slug("new_type", |s| doc.project.entity_def(s).is_some());
            let def: EntityDef = serde_json::from_value(serde_json::json!({
                "slug": slug.as_str(), "name": "New type", "size": [1, 1], "color": "#8080c0"
            }))
            .expect("a valid type");
            push(
                &mut ts.pending,
                format!("New type {slug}"),
                vec![Command::SetEntityDef {
                    entity: slug.clone(),
                    def,
                }],
            );
            ts.show(TypesSel::Entity(slug));
        }
    });
}

/// The list of types (filtered) and Level fields; the open one is scrolled
/// into view when it was just opened.
fn list(ui: &mut egui::Ui, doc: &Document, ts: &mut TypesState) {
    let q = super::actions::fuzzy_query(&ts.filter);
    let mut types: Vec<&EntityDef> = doc
        .project
        .entities
        .iter()
        .filter(|d| {
            q.is_empty()
                || super::actions::fuzzy_score_chars(&q, &d.name).is_some()
                || super::actions::fuzzy_score_chars(&q, d.slug.as_str()).is_some()
        })
        .collect();
    types.sort_by_key(|d| d.name.to_lowercase());
    egui::ScrollArea::vertical()
        .id_salt("types list")
        .max_height(if ts.open.is_some() { 96.0 } else { 220.0 })
        .auto_shrink([false, true])
        .show(ui, |ui| {
            ts.rects.insert("types list".into(), ui.clip_rect());
            let lf = ts.open == Some(TypesSel::LevelFields);
            if ui
                .selectable_label(
                    lf,
                    RichText::new(format!("Level fields ({})", doc.project.level_fields.len()))
                        .strong(),
                )
                .clicked()
            {
                ts.show(TypesSel::LevelFields);
            }
            for d in types {
                let on = ts.open.as_ref() == Some(&TypesSel::Entity(d.slug.clone()));
                let r = ui
                    .selectable_label(on, format!("{}  ·  {}", d.name, d.slug))
                    .on_hover_text(d.doc.as_deref().unwrap_or(""));
                if on && ts.reveal_type {
                    // At once: an animated scroll can be cut short (the
                    // field form's reveal, a screenshot taken after a few
                    // frames) and leave the row out of view.
                    r.scroll_to_me_animation(
                        Some(egui::Align::Center),
                        egui::style::ScrollAnimation::none(),
                    );
                }
                if on {
                    ts.rects.insert(format!("type:{}", d.slug), r.rect);
                }
                if r.clicked() {
                    ts.show(TypesSel::Entity(d.slug.clone()));
                }
            }
        });
}

/// A copy of `def` changed by `f`, as one labelled `set_entity_def`.
fn set_type(ts: &mut TypesState, def: &EntityDef, what: &str, f: impl FnOnce(&mut EntityDef)) {
    // Several edits can commit in one frame (a text field loses focus to a
    // checkbox click): each builds on the last one queued for this type,
    // so applying them in order keeps them all.
    // A rename queued before applies first, so the edit follows it.
    let mut base = def.clone();
    for c in ts.pending.iter().flat_map(batch_commands) {
        match c {
            Command::SetEntityDef { entity, def: d } if *entity == base.slug => {
                base = d.clone();
            }
            Command::RenameEntityType { from, to } if *from == base.slug => {
                base.slug = to.clone();
            }
            _ => {}
        }
    }
    let mut d = base.clone();
    f(&mut d);
    if d != base {
        push(
            &mut ts.pending,
            format!("Edit type {}: {what}", base.slug),
            vec![Command::SetEntityDef {
                entity: base.slug.clone(),
                def: d,
            }],
        );
    }
}

/// Comma-separated words.
fn words(s: &str) -> Vec<String> {
    s.split(',')
        .map(|w| w.trim().to_string())
        .filter(|w| !w.is_empty())
        .collect()
}

/// The type's header, Duplicate / Rename / Delete, and its form.
fn type_form(
    ui: &mut egui::Ui,
    doc: &Document,
    bufs: &mut HashMap<String, String>,
    atlas: &mut crate::viewport::Atlas,
    ts: &mut TypesState,
    def: &EntityDef,
) {
    let slug = &def.slug;
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(egui::Vec2::splat(22.0), Sense::hover());
        if let Some(icon) = &def.icon
            && let Some(t) = doc.project.tileset(icon.tileset.as_str())
        {
            super::panels::atlas_texture(atlas, ui.ctx(), doc, t);
        }
        super::entities::type_swatch(ui.painter(), doc, atlas, def, r);
        ui.heading(&def.name);
        ui.weak(format!("{} instances", instance_count(doc, slug)));
    });
    ui.horizontal(|ui| {
        if ui
            .button("Duplicate")
            .on_hover_text("A copy of this type under a new slug")
            .clicked()
            && let Some((cmd, new)) = duplicate_type(doc, slug.as_str())
        {
            ts.pending.push(cmd);
            ts.show(TypesSel::Entity(new));
        }
        if ui.button("Delete…").clicked() {
            ts.confirm = Some(Confirm::DeleteType(slug.clone()));
        }
    });
    if ts.confirm == Some(Confirm::DeleteType(slug.clone())) {
        let n = instance_count(doc, slug);
        egui::Frame::group(ui.style())
            .stroke(Stroke::new(1.0, ERROR))
            .show(ui, |ui| {
                ui.label(format!(
                    "Delete type {slug} and its {n} instance{}? Undo brings them back.",
                    if n == 1 { "" } else { "s" }
                ));
                ui.horizontal(|ui| {
                    if ui.button("Delete").clicked() {
                        push(
                            &mut ts.pending,
                            format!("Delete type {slug}"),
                            vec![Command::RemoveEntityDef {
                                entity: slug.clone(),
                                keep_instances: false,
                            }],
                        );
                        ts.confirm = None;
                    }
                    if ui.button("Cancel").clicked() {
                        ts.confirm = None;
                    }
                });
            });
    }
    let mut header = egui::CollapsingHeader::new("Type")
        .id_salt(("type form header", slug.as_str()))
        .default_open(true);
    if let Some(open) = ts.force_type_open.take() {
        header = header.open(Some(open));
    }
    header.show(ui, |ui| type_grid(ui, doc, bufs, atlas, ts, def));
}

/// The type form's rows.
fn type_grid(
    ui: &mut egui::Ui,
    doc: &Document,
    bufs: &mut HashMap<String, String>,
    atlas: &mut crate::viewport::Atlas,
    ts: &mut TypesState,
    def: &EntityDef,
) {
    let slug = &def.slug;
    let k = |what: &str| format!("type:{slug}:{what}");
    egui::Grid::new(("type form", slug.as_str()))
        .num_columns(2)
        .striped(true)
        .show(ui, |ui| {
            ui.weak("Slug");
            ui.horizontal(|ui| {
                if let Some(s) =
                    text_commit(ui, bufs, &k("slug"), slug.as_str(), false, false, 110.0)
                {
                    match Slug::new(s.trim()) {
                        Ok(to) => {
                            push(
                                &mut ts.pending,
                                format!("Rename type {slug} to {to}"),
                                vec![Command::RenameEntityType {
                                    from: slug.clone(),
                                    to: to.clone(),
                                }],
                            );
                            ts.follow = Some(Follow::Type(to));
                        }
                        Err(e) => ts.error = Some(format!("Bad slug: {e}")),
                    }
                }
                ui.weak("(Enter renames)")
                    .on_hover_text("Instances and entity_ref allowed_types follow the rename");
            });
            ui.end_row();
            ui.weak("Name");
            if let Some(s) = text_commit(ui, bufs, &k("name"), &def.name, false, false, 160.0) {
                set_type(ts, def, "name", |d| d.name = s);
            }
            ui.end_row();
            ui.weak("Category");
            let cat = def.category.clone().unwrap_or_default();
            if let Some(s) = text_commit(ui, bufs, &k("category"), &cat, false, false, 160.0) {
                set_type(ts, def, "category", |d| {
                    d.category = (!s.trim().is_empty()).then(|| s.trim().to_string())
                });
            }
            ui.end_row();
            ui.weak("Tags");
            if let Some(s) = text_commit(
                ui,
                bufs,
                &k("tags"),
                &def.tags.join(", "),
                false,
                false,
                160.0,
            ) {
                set_type(ts, def, "tags", |d| d.tags = words(&s));
            }
            ui.end_row();
            ui.weak("Doc");
            let doc_text = def.doc.clone().unwrap_or_default();
            if let Some(s) = text_commit(ui, bufs, &k("doc"), &doc_text, false, true, 180.0) {
                set_type(ts, def, "doc", |d| {
                    d.doc = (!s.trim().is_empty()).then_some(s)
                });
            }
            ui.end_row();
            ui.weak("Size (cells)");
            ui.horizontal(|ui| {
                for (i, axis) in ["w", "h"].into_iter().enumerate() {
                    ui.weak(axis);
                    if let Some(s) = text_commit(
                        ui,
                        bufs,
                        &k(&format!("size.{axis}")),
                        &def.size[i].to_string(),
                        false,
                        false,
                        36.0,
                    ) && let Ok(n) = s.trim().parse::<u32>()
                    {
                        set_type(ts, def, "size", |d| d.size[i] = n.max(1));
                    }
                }
            });
            ui.end_row();
            ui.weak("Resizable");
            ui.horizontal(|ui| {
                let mut r = def.resizable;
                let a = ui.checkbox(&mut r[0], "width").changed();
                let b = ui.checkbox(&mut r[1], "height").changed();
                if a || b {
                    set_type(ts, def, "resizable", |d| d.resizable = r);
                }
            });
            ui.end_row();
            ui.weak("Pivot");
            ui.horizontal(|ui| {
                for (i, axis) in ["x", "y"].into_iter().enumerate() {
                    ui.weak(axis);
                    if let Some(s) = text_commit(
                        ui,
                        bufs,
                        &k(&format!("pivot.{axis}")),
                        &def.pivot[i].to_string(),
                        false,
                        false,
                        36.0,
                    ) && let Ok(v) = s.trim().parse::<f64>()
                    {
                        set_type(ts, def, "pivot", |d| d.pivot[i] = v.clamp(0.0, 1.0));
                    }
                }
                egui::ComboBox::from_id_salt(("pivot presets", slug.as_str()))
                    .selected_text("…")
                    .width(30.0)
                    .show_ui(ui, |ui| {
                        for (name, p) in [
                            ("top left", [0.0, 0.0]),
                            ("center", [0.5, 0.5]),
                            ("bottom center", [0.5, 1.0]),
                            ("bottom left", [0.0, 1.0]),
                        ] {
                            if ui.selectable_label(def.pivot == p, name).clicked() {
                                set_type(ts, def, "pivot", |d| d.pivot = p);
                            }
                        }
                    });
            });
            ui.end_row();
            ui.weak("Icon");
            ui.horizontal(|ui| icon_editor(ui, doc, bufs, atlas, ts, def));
            ui.end_row();
            ui.weak("Color");
            if let Some(hex) =
                super::panels::color_field(ui, bufs, &k("color"), Some(def.color.as_str()))
                && let Ok(c) = worlds_core::Color::new(hex)
            {
                set_type(ts, def, "color", |d| d.color = c);
            }
            ui.end_row();
            ui.weak("Limit");
            ui.horizontal(|ui| limit_editor(ui, bufs, ts, def));
            ui.end_row();
        });
}

/// The icon: a tileset and a tile clicked in its atlas, or none.
fn icon_editor(
    ui: &mut egui::Ui,
    doc: &Document,
    bufs: &mut HashMap<String, String>,
    atlas: &mut crate::viewport::Atlas,
    ts: &mut TypesState,
    def: &EntityDef,
) {
    let key = format!("type:{}:icon.tileset", def.slug);
    let mut tileset = bufs
        .get(&key)
        .cloned()
        .or_else(|| def.icon.as_ref().map(|i| i.tileset.to_string()))
        .or_else(|| doc.project.tilesets.first().map(|t| t.slug.to_string()))
        .unwrap_or_default();
    egui::ComboBox::from_id_salt(("icon tileset", def.slug.as_str()))
        .selected_text(tileset.clone())
        .width(100.0)
        .show_ui(ui, |ui| {
            for t in &doc.project.tilesets {
                if ui
                    .selectable_label(tileset == t.slug.as_str(), t.slug.as_str())
                    .clicked()
                {
                    tileset = t.slug.to_string();
                    bufs.insert(key.clone(), tileset.clone());
                }
            }
        });
    let text = def
        .icon
        .as_ref()
        .map_or("none".into(), |i| format!("[{}, {}]", i.tile[0], i.tile[1]));
    if let Some(t) = doc.project.tileset(&tileset) {
        let tex = super::panels::atlas_texture(atlas, ui.ctx(), doc, t);
        let current = def
            .icon
            .as_ref()
            .filter(|i| i.tileset == t.slug)
            .map(|i| i.tile);
        ui.menu_button(text, |ui| match &tex {
            Some(tex) => {
                if let Some(tile) = super::fields::atlas_picker(ui, t, tex, current) {
                    set_type(ts, def, "icon", |d| {
                        d.icon = Some(TileRef {
                            tileset: t.slug.clone(),
                            tile,
                        })
                    });
                    ui.close();
                }
            }
            None => {
                ui.colored_label(ERROR, "The tileset image didn't load");
            }
        });
    }
    if def.icon.is_some() && ui.small_button("×").on_hover_text("No icon").clicked() {
        set_type(ts, def, "icon", |d| d.icon = None);
    }
}

/// `limit`: on/off, max, scope, behavior.
fn limit_editor(
    ui: &mut egui::Ui,
    bufs: &mut HashMap<String, String>,
    ts: &mut TypesState,
    def: &EntityDef,
) {
    let mut on = def.limit.is_some();
    if ui.checkbox(&mut on, "").changed() {
        set_type(ts, def, "limit", |d| {
            d.limit = on.then(|| Limit {
                max: 1,
                scope: LimitScope::default(),
                behavior: LimitBehavior::default(),
            })
        });
    }
    let Some(l) = &def.limit else {
        ui.weak("unlimited");
        return;
    };
    ui.weak("max");
    let key = format!("type:{}:limit.max", def.slug);
    if let Some(s) = text_commit(ui, bufs, &key, &l.max.to_string(), false, false, 32.0)
        && let Ok(n) = s.trim().parse::<u32>()
    {
        set_type(ts, def, "limit", |d| {
            if let Some(l) = &mut d.limit {
                l.max = n;
            }
        });
    }
    egui::ComboBox::from_id_salt(("limit scope", def.slug.as_str()))
        .selected_text(format!("per {}", scope_name(l.scope)))
        .show_ui(ui, |ui| {
            for s in [LimitScope::Layer, LimitScope::Level, LimitScope::World] {
                if ui
                    .selectable_label(l.scope == s, format!("per {}", scope_name(s)))
                    .clicked()
                {
                    set_type(ts, def, "limit", |d| {
                        if let Some(l) = &mut d.limit {
                            l.scope = s;
                        }
                    });
                }
            }
        });
    egui::ComboBox::from_id_salt(("limit behavior", def.slug.as_str()))
        .selected_text(behavior_name(l.behavior))
        .show_ui(ui, |ui| {
            for b in [
                LimitBehavior::PreventAdding,
                LimitBehavior::DiscardOldest,
                LimitBehavior::MoveLastOne,
            ] {
                if ui
                    .selectable_label(l.behavior == b, behavior_name(b))
                    .clicked()
                {
                    set_type(ts, def, "limit", |d| {
                        if let Some(l) = &mut d.limit {
                            l.behavior = b;
                        }
                    });
                }
            }
        });
}

fn scope_name(s: LimitScope) -> &'static str {
    match s {
        LimitScope::Layer => "layer",
        LimitScope::Level => "level",
        LimitScope::World => "project",
    }
}

fn behavior_name(b: LimitBehavior) -> &'static str {
    match b {
        LimitBehavior::PreventAdding => "prevent adding",
        LimitBehavior::DiscardOldest => "discard oldest",
        LimitBehavior::MoveLastOne => "move last one",
    }
}

/// Drag payload: a field row's slug.
#[derive(Debug, Clone)]
struct FieldDrag(Slug);

/// The field rows: drag handle, slug and type, duplicate, remove; + Field.
fn field_list(
    ui: &mut egui::Ui,
    doc: &Document,
    ts: &mut TypesState,
    owner: &Owner<'_>,
    accent: Color32,
) {
    ui.strong(format!("Fields ({})", owner.fields.len()));
    for (i, f) in owner.fields.iter().enumerate() {
        let row = ui.horizontal(|ui| {
            let h = ui
                .add(egui::Label::new(RichText::new("☰").weak()).sense(Sense::drag()))
                .on_hover_text("Drag to reorder");
            super::fields::mark(ui, &format!("types:{}:handle", f.slug), h.rect);
            if h.dragged() {
                h.dnd_set_drag_payload(FieldDrag(f.slug.clone()));
            }
            let on = ts.field.as_ref() == Some(&f.slug);
            if ui
                .selectable_label(
                    on,
                    format!(
                        "{}  {}{}",
                        f.slug,
                        f.ty.name(),
                        if f.array { "[]" } else { "" }
                    ),
                )
                .clicked()
            {
                ts.field = (!on).then(|| f.slug.clone());
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let x = ui.small_button("×").on_hover_text("Remove this field");
                super::fields::mark(ui, &format!("types:{}:remove", f.slug), x.rect);
                if x.clicked() {
                    ts.confirm = Some(Confirm::RemoveField(f.slug.clone()));
                }
                let dup = ui
                    .small_button("Dup")
                    .on_hover_text("Duplicate this field (a new slug, right after it)");
                super::fields::mark(ui, &format!("types:{}:dup", f.slug), dup.rect);
                if dup.clicked() {
                    let slug = unique_slug(&format!("{}_copy", f.slug), |s| {
                        owner.fields.iter().any(|g| g.slug == s)
                    });
                    let copy = FieldDef {
                        slug: slug.clone(),
                        ..f.clone()
                    };
                    push(
                        &mut ts.pending,
                        format!("Duplicate field {} of {}", f.slug, owner.name()),
                        vec![owner.set(copy, Some(i + 1))],
                    );
                    ts.field = Some(slug);
                }
            });
        });
        let resp = row.response;
        ts.rects.insert(f.slug.to_string(), resp.rect);
        if let Some(p) = resp.dnd_hover_payload::<FieldDrag>() {
            let after = ui
                .ctx()
                .pointer_hover_pos()
                .is_some_and(|p| p.y > resp.rect.center().y);
            if p.0 != f.slug {
                let y = if after {
                    resp.rect.bottom()
                } else {
                    resp.rect.top()
                };
                ui.painter()
                    .hline(resp.rect.x_range(), y, Stroke::new(2.0, accent));
            }
        }
        if let Some(p) = resp.dnd_release_payload::<FieldDrag>() {
            let after = ui
                .ctx()
                .pointer_interact_pos()
                .is_some_and(|p| p.y > resp.rect.center().y);
            if let Some(from) = owner.fields.iter().position(|g| g.slug == p.0)
                && let Some(to) = drop_index(from, i, after)
            {
                push(
                    &mut ts.pending,
                    format!("Move field {} of {}", p.0, owner.name()),
                    vec![owner.set(owner.fields[from].clone(), Some(to))],
                );
            }
        }
        if ts.confirm == Some(Confirm::RemoveField(f.slug.clone())) {
            let n = stored_count(doc, &owner.sel, &f.slug);
            egui::Frame::group(ui.style())
                .stroke(Stroke::new(1.0, ERROR))
                .show(ui, |ui| {
                    ui.label(if n == 0 {
                        format!(
                            "Remove field {}? No {} store a value.",
                            f.slug,
                            owner.holders()
                        )
                    } else {
                        format!(
                            "Remove field {} and the values {n} {} store? Undo brings them back.",
                            f.slug,
                            owner.holders()
                        )
                    });
                    ui.horizontal(|ui| {
                        let b = ui.button("Remove");
                        super::fields::mark(ui, "types:confirm remove", b.rect);
                        if b.clicked() {
                            push(
                                &mut ts.pending,
                                format!("Remove field {} of {}", f.slug, owner.name()),
                                vec![owner.remove(&f.slug)],
                            );
                            ts.confirm = None;
                            if ts.field.as_ref() == Some(&f.slug) {
                                ts.field = None;
                            }
                        }
                        if ui.button("Cancel").clicked() {
                            ts.confirm = None;
                        }
                    });
                });
        }
    }
    let add = ui.button("+ Field");
    super::fields::mark(ui, "types:add field", add.rect);
    if add.clicked() {
        let slug = unique_slug("field", |s| owner.fields.iter().any(|g| g.slug == s));
        let def = FieldDef::new(slug.clone(), FieldType::int());
        push(
            &mut ts.pending,
            format!("Add field {slug} to {}", owner.name()),
            vec![owner.set(def, None)],
        );
        ts.field = Some(slug);
    }
}

/// The types a field can have, by name.
const TYPE_NAMES: [&str; 12] = [
    "int",
    "float",
    "bool",
    "string",
    "text",
    "color",
    "enum",
    "point",
    "entity_ref",
    "path",
    "tile",
    "json",
];

/// A fresh field type named `name` (no options), for a type change.
fn type_named(doc: &Document, name: &str) -> FieldType {
    match name {
        "int" => FieldType::int(),
        "float" => FieldType::float(),
        "bool" => FieldType::Bool,
        "string" => FieldType::string(),
        "text" => FieldType::Text { regex: None },
        "color" => FieldType::Color,
        "enum" => match doc.project.enums.first() {
            Some(e) => FieldType::Enum(EnumSource::Project(e.slug.clone())),
            None => FieldType::Enum(EnumSource::Inline(vec![
                Slug::new("a").expect("valid"),
                Slug::new("b").expect("valid"),
            ])),
        },
        "point" => FieldType::Point,
        "entity_ref" => FieldType::entity_ref(),
        "path" => FieldType::Path {
            extensions: Vec::new(),
        },
        "tile" => FieldType::Tile { tileset: None },
        _ => FieldType::Json,
    }
}

/// A copy of field `f` changed by `edit`, as one `set_*_field_def`.
fn set_field(
    ts: &mut TypesState,
    owner: &Owner<'_>,
    f: &FieldDef,
    what: &str,
    edit: impl FnOnce(&mut FieldDef),
) {
    // As in `set_type`: build on what this frame already queued for the
    // field (an earlier edit, or a rename, which applies first).
    let mut base = f.clone();
    for c in ts.pending.iter().flat_map(batch_commands) {
        if let Some(d) = owner.queued(c, &base) {
            base = d;
        }
    }
    let mut d = base.clone();
    edit(&mut d);
    if d != base {
        push(
            &mut ts.pending,
            format!("Edit field {} of {}: {what}", base.slug, owner.name()),
            vec![owner.set(d, None)],
        );
    }
}

/// The commands of a queued pane edit (a batch, or one command).
fn batch_commands(c: &Command) -> &[Command] {
    match c {
        Command::Batch { commands, .. } => commands,
        other => std::slice::from_ref(other),
    }
}

/// A JSON number from text (`None` when empty: no bound).
fn number(s: &str, int: bool) -> Result<Option<serde_json::Number>, String> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(None);
    }
    if int {
        s.parse::<i64>()
            .map(|n| Some(n.into()))
            .map_err(|_| format!("{s:?} is not a whole number"))
    } else {
        s.parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map(Some)
            .ok_or_else(|| format!("{s:?} is not a number"))
    }
}

/// The selected field's form.
fn field_form(
    ui: &mut egui::Ui,
    fx: &mut FieldCx<'_>,
    ts: &mut TypesState,
    owner: &Owner<'_>,
    f: &FieldDef,
) {
    let base = format!("fdef:{}:{}", owner.name(), f.slug);
    let k = |what: &str| format!("{base}:{what}");
    ui.strong(format!("Field {}", f.slug));
    egui::Grid::new(("field form", base.as_str()))
        .num_columns(2)
        .striped(true)
        .show(ui, |ui| {
            ui.weak("Slug");
            ui.horizontal(|ui| {
                if let Some(s) = text_commit(
                    ui,
                    fx.bufs,
                    &k("slug"),
                    f.slug.as_str(),
                    false,
                    false,
                    110.0,
                ) {
                    match Slug::new(s.trim()) {
                        Ok(to) => {
                            push(
                                &mut ts.pending,
                                format!("Rename field {} of {} to {to}", f.slug, owner.name()),
                                vec![owner.rename(&f.slug, to.clone())],
                            );
                            ts.follow = Some(Follow::Field(to));
                        }
                        Err(e) => ts.error = Some(format!("Bad slug: {e}")),
                    }
                }
                ui.weak("(Enter renames)")
                    .on_hover_text("Stored values follow the rename");
            });
            ui.end_row();
            ui.weak("Name");
            let name = f.name.clone().unwrap_or_default();
            if let Some(s) = text_commit(ui, fx.bufs, &k("name"), &name, false, false, 150.0) {
                set_field(ts, owner, f, "name", |d| {
                    d.name = (!s.trim().is_empty()).then_some(s)
                });
            }
            ui.end_row();
            ui.weak("Doc");
            let doc_text = f.doc.clone().unwrap_or_default();
            if let Some(s) = text_commit(ui, fx.bufs, &k("doc"), &doc_text, false, true, 170.0) {
                set_field(ts, owner, f, "doc", |d| {
                    d.doc = (!s.trim().is_empty()).then_some(s)
                });
            }
            ui.end_row();
            ui.weak("Type");
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt(("field type", base.as_str()))
                    .selected_text(f.ty.name())
                    .show_ui(ui, |ui| {
                        for name in TYPE_NAMES {
                            if ui.selectable_label(f.ty.name() == name, name).clicked()
                                && f.ty.name() != name
                            {
                                let ty = type_named(fx.doc, name);
                                // The old default doesn't fit the new type;
                                // stored values stay (validation reports them).
                                set_field(ts, owner, f, "type", |d| {
                                    d.ty = ty;
                                    d.default = None;
                                });
                            }
                        }
                    });
                let mut array = f.array;
                if ui.checkbox(&mut array, "array").changed() {
                    set_field(ts, owner, f, "array", |d| {
                        d.array = array;
                        d.default = None;
                    });
                }
                let mut nullable = f.nullable;
                let nb = ui.checkbox(&mut nullable, "nullable");
                super::fields::mark(ui, &k("nullable"), nb.rect);
                if nb.changed() {
                    set_field(ts, owner, f, "nullable", |d| d.nullable = nullable);
                }
            });
            ui.end_row();
            ui.weak("Default");
            if let Some(edit) = field_row(ui, fx, f, f.default.as_ref(), false, &k("default"), true)
            {
                set_field(ts, owner, f, "default", |d| {
                    d.default = match edit {
                        FieldEdit::Set(v) => Some(v),
                        FieldEdit::Remove => None,
                    }
                });
            }
            ui.end_row();
            type_options(ui, fx, ts, owner, f, &k);
            ui.weak("Display");
            ui.horizontal(|ui| {
                let mut d = f.display.clone();
                let a = ui
                    .checkbox(&mut d.canvas, "canvas")
                    .on_hover_text("Show the value under the box on the canvas")
                    .changed();
                let b = ui
                    .checkbox(&mut d.arrow, "arrow")
                    .on_hover_text("entity_ref and point: an arrow to the target")
                    .changed();
                let c = ui
                    .checkbox(&mut d.world, "world")
                    .on_hover_text("Show the value in the zoomed-out world view")
                    .changed();
                if a || b || c {
                    set_field(ts, owner, f, "display", move |x| x.display = d);
                }
            });
            ui.end_row();
        });
}

/// The rows of the options the field's type has.
fn type_options(
    ui: &mut egui::Ui,
    fx: &mut FieldCx<'_>,
    ts: &mut TypesState,
    owner: &Owner<'_>,
    f: &FieldDef,
    k: &dyn Fn(&str) -> String,
) {
    let bufs = &mut *fx.bufs;
    match &f.ty {
        FieldType::Int { min, max } | FieldType::Float { min, max } => {
            let int = matches!(f.ty, FieldType::Int { .. });
            for (what, cur) in [("min", min), ("max", max)] {
                ui.weak(what);
                let init = cur.as_ref().map_or(String::new(), |n| n.to_string());
                if let Some(s) = text_commit(ui, bufs, &k(what), &init, false, false, 70.0) {
                    match number(&s, int) {
                        Ok(n) => set_field(ts, owner, f, what, |d| {
                            if let FieldType::Int { min, max } | FieldType::Float { min, max } =
                                &mut d.ty
                            {
                                if what == "min" {
                                    *min = n;
                                } else {
                                    *max = n;
                                }
                            }
                        }),
                        Err(e) => ts.error = Some(e),
                    }
                }
                ui.end_row();
            }
        }
        FieldType::String { regex } | FieldType::Text { regex } => {
            ui.weak("Regex");
            let init = regex.clone().unwrap_or_default();
            if let Some(s) = text_commit(ui, bufs, &k("regex"), &init, false, false, 150.0) {
                set_field(ts, owner, f, "regex", |d| {
                    if let FieldType::String { regex } | FieldType::Text { regex } = &mut d.ty {
                        *regex = (!s.is_empty()).then_some(s);
                    }
                });
            }
            ui.end_row();
        }
        FieldType::Enum(src) => {
            ui.weak("Values from");
            let inline = matches!(src, EnumSource::Inline(_));
            egui::ComboBox::from_id_salt(k("enum"))
                .selected_text(match src {
                    EnumSource::Project(e) => format!("enum {e}"),
                    EnumSource::Inline(_) => "inline values".into(),
                })
                .show_ui(ui, |ui| {
                    for e in &fx.doc.project.enums {
                        let on = *src == EnumSource::Project(e.slug.clone());
                        if ui
                            .selectable_label(on, format!("enum {}", e.slug))
                            .clicked()
                            && !on
                        {
                            let s = e.slug.clone();
                            set_field(ts, owner, f, "enum", |d| {
                                d.ty = FieldType::Enum(EnumSource::Project(s));
                                d.default = None;
                            });
                        }
                    }
                    if ui.selectable_label(inline, "inline values").clicked() && !inline {
                        set_field(ts, owner, f, "enum", |d| {
                            d.ty = FieldType::Enum(EnumSource::Inline(vec![
                                Slug::new("a").expect("valid"),
                            ]));
                            d.default = None;
                        });
                    }
                });
            ui.end_row();
            if let EnumSource::Inline(vals) = src {
                ui.weak("Values");
                let init = vals
                    .iter()
                    .map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                if let Some(s) = text_commit(ui, bufs, &k("values"), &init, false, false, 150.0) {
                    match words(&s)
                        .into_iter()
                        .map(Slug::new)
                        .collect::<Result<Vec<_>, _>>()
                    {
                        Ok(v) => set_field(ts, owner, f, "values", |d| {
                            d.ty = FieldType::Enum(EnumSource::Inline(v))
                        }),
                        Err(e) => ts.error = Some(format!("Bad value: {e}")),
                    }
                }
                ui.end_row();
            }
        }
        FieldType::EntityRef {
            allowed_types,
            allowed_tags,
            cross_level,
        } => {
            ui.weak("Allowed types");
            let init = allowed_types
                .iter()
                .map(|t| t.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            if let Some(s) = text_commit(ui, bufs, &k("allowed_types"), &init, false, false, 150.0)
            {
                match words(&s)
                    .into_iter()
                    .map(Slug::new)
                    .collect::<Result<Vec<_>, _>>()
                {
                    Ok(v) => set_field(ts, owner, f, "allowed types", |d| {
                        if let FieldType::EntityRef { allowed_types, .. } = &mut d.ty {
                            *allowed_types = v;
                        }
                    }),
                    Err(e) => ts.error = Some(format!("Bad type slug: {e}")),
                }
            }
            ui.end_row();
            ui.weak("Allowed tags");
            if let Some(s) = text_commit(
                ui,
                bufs,
                &k("allowed_tags"),
                &allowed_tags.join(", "),
                false,
                false,
                150.0,
            ) {
                set_field(ts, owner, f, "allowed tags", |d| {
                    if let FieldType::EntityRef { allowed_tags, .. } = &mut d.ty {
                        *allowed_tags = words(&s);
                    }
                });
            }
            ui.end_row();
            ui.weak("Cross level");
            let mut c = *cross_level;
            if ui
                .checkbox(&mut c, "the target may be in another level")
                .changed()
            {
                set_field(ts, owner, f, "cross level", |d| {
                    if let FieldType::EntityRef { cross_level, .. } = &mut d.ty {
                        *cross_level = c;
                    }
                });
            }
            ui.end_row();
        }
        FieldType::Path { extensions } => {
            ui.weak("Extensions");
            if let Some(s) = text_commit(
                ui,
                bufs,
                &k("extensions"),
                &extensions.join(", "),
                false,
                false,
                150.0,
            ) {
                set_field(ts, owner, f, "extensions", |d| {
                    if let FieldType::Path { extensions } = &mut d.ty {
                        *extensions = words(&s)
                            .into_iter()
                            .map(|e| e.trim_start_matches('.').to_string())
                            .collect();
                    }
                });
            }
            ui.end_row();
        }
        FieldType::Tile { tileset } => {
            ui.weak("Tileset");
            egui::ComboBox::from_id_salt(k("tileset"))
                .selected_text(tileset.as_ref().map_or("any", |t| t.as_str()))
                .show_ui(ui, |ui| {
                    if ui.selectable_label(tileset.is_none(), "any").clicked() {
                        set_field(ts, owner, f, "tileset", |d| {
                            d.ty = FieldType::Tile { tileset: None }
                        });
                    }
                    for t in &fx.doc.project.tilesets {
                        let on = tileset.as_ref() == Some(&t.slug);
                        if ui.selectable_label(on, t.slug.as_str()).clicked() {
                            let s = t.slug.clone();
                            set_field(ts, owner, f, "tileset", |d| {
                                d.ty = FieldType::Tile { tileset: Some(s) }
                            });
                        }
                    }
                });
            ui.end_row();
        }
        FieldType::Bool | FieldType::Color | FieldType::Point | FieldType::Json => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drop_positions() {
        // Fields a b c d: dragging a onto the lower half of c puts it after
        // c (index 2 once a is out of the way).
        assert_eq!(drop_index(0, 2, true), Some(2));
        assert_eq!(drop_index(0, 2, false), Some(1));
        assert_eq!(drop_index(3, 0, false), Some(0));
        assert_eq!(drop_index(3, 1, true), Some(2));
        // Onto itself, or right where it is: nothing.
        assert_eq!(drop_index(1, 1, false), None);
        assert_eq!(drop_index(1, 0, true), None);
        assert_eq!(drop_index(1, 1, true), None);
    }

    #[test]
    fn unique_slugs() {
        let taken = ["door_copy", "door_copy_2"];
        assert_eq!(
            unique_slug("door_copy", |s| taken.contains(&s)),
            "door_copy_3"
        );
        assert_eq!(unique_slug("field", |_| false), "field");
    }

    /// The mini fixture in an `EditorState` with the Types pane on
    /// `locked_door`.
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
        let mut st = EditorState {
            doc: Some(doc),
            ..Default::default()
        };
        st.types
            .show(TypesSel::Entity(Slug::new("locked_door").unwrap()));
        // The field list matters here, not the type's form.
        st.types.force_type_open = Some(false);
        (tmp, st)
    }

    struct Driver {
        ctx: egui::Context,
        t: f64,
    }

    impl Driver {
        fn frame(&mut self, st: &mut EditorState, events: Vec<egui::Event>) {
            self.t += 0.1;
            let raw = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(700.0, 2000.0),
                )),
                time: Some(self.t),
                focused: true,
                events,
                ..Default::default()
            };
            let mut out = self.ctx.run_ui(raw, |ui| pane(ui, st));
            out.textures_delta.clear();
        }

        fn pointer(&mut self, st: &mut EditorState, p: egui::Pos2, pressed: Option<bool>) {
            let mut ev = vec![egui::Event::PointerMoved(p)];
            if let Some(pressed) = pressed {
                ev.push(egui::Event::PointerButton {
                    pos: p,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: Default::default(),
                });
            }
            self.frame(st, ev);
        }

        fn click(&mut self, st: &mut EditorState, p: egui::Pos2) {
            self.pointer(st, p, Some(true));
            self.pointer(st, p, Some(false));
        }

        fn rect(&self, key: &str) -> egui::Rect {
            super::super::fields::marked(&self.ctx, key)
                .unwrap_or_else(|| panic!("nothing drawn for {key}"))
        }
    }

    fn one(st: &mut EditorState) -> (String, Vec<Command>) {
        assert_eq!(st.types.pending.len(), 1, "{:?}", st.types.pending);
        match st.types.pending.pop().unwrap() {
            Command::Batch { label, commands } => (label, commands),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn duplicate_and_remove_a_field() {
        let (_tmp, mut st) = state();
        let mut d = Driver {
            ctx: egui::Context::default(),
            t: 0.0,
        };
        d.frame(&mut st, vec![]);
        d.frame(&mut st, vec![]);
        // Dup on `channel` (first): a copy under a new slug right after it.
        let r = d.rect("types:channel:dup");
        d.click(&mut st, r.center());
        let (label, cmds) = one(&mut st);
        assert_eq!(label, "Duplicate field channel of locked_door");
        match &cmds[..] {
            [
                Command::SetEntityFieldDef {
                    entity,
                    field,
                    def,
                    index: Some(1),
                },
            ] => {
                assert_eq!(
                    (entity.as_str(), field.as_str()),
                    ("locked_door", "channel_copy")
                );
                assert_eq!(def.slug, "channel_copy");
                assert_eq!(def.ty.name(), "int");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            st.types.field.as_ref().map(|s| s.as_str()),
            Some("channel_copy")
        );
        // × on needs_item asks first (doorcrev22 stores a value), then
        // removes it with its values.
        let r = d.rect("types:needs_item:remove");
        d.click(&mut st, r.center());
        assert!(st.types.pending.is_empty(), "asks first");
        let doc = st.doc.as_ref().unwrap();
        let owner = TypesSel::Entity(Slug::new("locked_door").unwrap());
        assert_eq!(
            stored_count(doc, &owner, &Slug::new("needs_item").unwrap()),
            1
        );
        d.frame(&mut st, vec![]);
        let r = d.rect("types:confirm remove");
        d.click(&mut st, r.center());
        let (_, cmds) = one(&mut st);
        assert_eq!(
            cmds,
            [Command::RemoveEntityFieldDef {
                entity: Slug::new("locked_door").unwrap(),
                field: Slug::new("needs_item").unwrap(),
                keep_values: false,
            }]
        );
    }

    #[test]
    fn dragging_a_field_reorders_it() {
        let (_tmp, mut st) = state();
        let mut d = Driver {
            ctx: egui::Context::default(),
            t: 0.0,
        };
        d.frame(&mut st, vec![]);
        d.frame(&mut st, vec![]);
        // channel (0) onto the lower half of consumes (2): index 2.
        let h = d.rect("types:channel:handle").center();
        let target = st.types.rects["consumes"];
        let to = egui::pos2(target.center().x, target.center().y + target.height() * 0.3);
        d.pointer(&mut st, h, Some(true));
        for k in 1..=8 {
            let p = h + (to - h) * (k as f32 / 8.0);
            d.pointer(&mut st, p, None);
        }
        d.pointer(&mut st, to, Some(false));
        d.frame(&mut st, vec![]);
        let (label, cmds) = one(&mut st);
        assert_eq!(label, "Move field channel of locked_door");
        assert!(
            matches!(&cmds[..], [Command::SetEntityFieldDef { field, index: Some(2), .. }]
                if field == "channel"),
            "{cmds:?}"
        );
    }

    #[test]
    fn a_rename_is_followed_only_once_it_applied() {
        let (_tmp, mut st) = state();
        st.types.field = Some(Slug::new("channel").unwrap());
        let mut d = Driver {
            ctx: egui::Context::default(),
            t: 0.0,
        };
        d.frame(&mut st, vec![]);
        d.frame(&mut st, vec![]);
        // Type "x" after the field's slug and press Enter.
        let r = d.rect("fdef:locked_door:channel:slug");
        d.click(&mut st, r.center());
        d.frame(&mut st, vec![egui::Event::Text("x".into())]);
        let enter = |pressed| egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: Default::default(),
        };
        d.frame(&mut st, vec![enter(true), enter(false)]);
        let (label, _) = one(&mut st);
        assert_eq!(label, "Rename field channel of locked_door to channelx");
        // Not yet: the form stays on the field until the rename applied.
        assert_eq!(st.types.field.as_ref().unwrap(), "channel");
        assert_eq!(
            st.types.follow,
            Some(Follow::Field(Slug::new("channelx").unwrap()))
        );
        // Refused: the form stays where it was.
        let follow = st.types.follow.clone();
        st.types.applied(false);
        assert_eq!(st.types.field.as_ref().unwrap(), "channel");
        assert_eq!(st.types.follow, None);
        // Applied: it follows.
        st.types.follow = follow;
        st.types.applied(true);
        assert_eq!(st.types.field.as_ref().unwrap(), "channelx");
        // A type rename moves the open type the same way, so the pane
        // doesn't fall back to the list.
        st.types.follow = Some(Follow::Type(Slug::new("door").unwrap()));
        st.types.applied(true);
        assert_eq!(
            st.types.open,
            Some(TypesSel::Entity(Slug::new("door").unwrap()))
        );
    }

    #[test]
    fn edits_committed_in_one_frame_build_on_each_other() {
        let (_tmp, mut st) = state();
        st.types.field = Some(Slug::new("channel").unwrap());
        let mut d = Driver {
            ctx: egui::Context::default(),
            t: 0.0,
        };
        d.frame(&mut st, vec![]);
        d.frame(&mut st, vec![]);
        // Type a name, then click "nullable": in that frame the name
        // commits (focus leaves it) and the checkbox changes.
        let r = d.rect("fdef:locked_door:channel:name");
        d.click(&mut st, r.center());
        d.frame(&mut st, vec![egui::Event::Text("Chan".into())]);
        let nb = d.rect("fdef:locked_door:channel:nullable").left_center() + egui::vec2(6.0, 0.0);
        let button = |pressed| egui::Event::PointerButton {
            pos: nb,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        d.frame(
            &mut st,
            vec![egui::Event::PointerMoved(nb), button(true), button(false)],
        );
        // Applied in order, the last one carries both edits.
        let last = st
            .types
            .pending
            .iter()
            .flat_map(batch_commands)
            .last()
            .cloned();
        match last {
            Some(Command::SetEntityFieldDef { def, .. }) => {
                assert_eq!(def.name.as_deref(), Some("Chan"));
                assert!(def.nullable);
            }
            other => panic!("{other:?} of {:?}", st.types.pending),
        }
        assert_eq!(st.types.pending.len(), 2, "{:?}", st.types.pending);
    }

    #[test]
    fn level_fields_get_a_new_field_and_refusals_are_shown() {
        let (_tmp, mut st) = state();
        st.types.show(TypesSel::LevelFields);
        let mut d = Driver {
            ctx: egui::Context::default(),
            t: 0.0,
        };
        d.frame(&mut st, vec![]);
        d.frame(&mut st, vec![]);
        let r = d.rect("types:add field");
        d.click(&mut st, r.center());
        let (label, cmds) = one(&mut st);
        assert_eq!(label, "Add field field to level fields");
        assert!(matches!(
            &cmds[..],
            [Command::SetLevelFieldDef { field, index: None, .. }] if field == "field"
        ));
        // A rename onto a slug that's taken comes back refused; the app
        // puts the message in the pane, which shows it.
        let mut ed = crate::edit::Editing::new();
        let mut inv = Vec::new();
        let EditorState { doc, view, .. } = &mut st;
        let mut cx = crate::tools::CommandContext {
            doc: doc.as_mut().unwrap(),
            view,
            invalidate: &mut inv,
        };
        use crate::tools::EditorCommands;
        let e = ed
            .apply(
                &mut cx,
                Command::RenameLevelField {
                    from: Slug::new("music").unwrap(),
                    to: Slug::new("biome").unwrap(),
                },
            )
            .unwrap_err();
        assert!(e.contains("biome"), "{e}");
        st.types.error = Some(e);
        d.frame(&mut st, vec![]);
        assert!(
            st.types.error.is_some(),
            "kept until dismissed or the next edit"
        );
    }
}
