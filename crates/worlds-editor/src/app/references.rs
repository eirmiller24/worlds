//! Reference images in the panes: the "References" pseudo-layer at the
//! bottom of the Layers pane (eye, lock, the list of the active level's and
//! the world's references, "Add reference image…"), and the Inspector part
//! for the selected one (path, position, scale, opacity, locked, visible).
//! The map side is [`crate::viewport::references`].

use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;

use egui::RichText;
use worlds_core::command::NoteScope;
use worlds_core::{Command, Id, Reference};

use super::EditorState;
use super::actions::Action;
use crate::doc::Document;
use crate::viewport::references::{self as refs, RefAt};

/// The native file dialog for "Add reference image…", running on its own
/// thread, and where the image goes.
pub struct Picker {
    pub rx: Receiver<Option<PathBuf>>,
    pub scope: NoteScope,
}

/// Start the file dialog for a PNG to add at `scope`.
pub fn start_picker(ctx: &egui::Context, dir: &Path, scope: NoteScope) -> Picker {
    let (tx, rx) = std::sync::mpsc::channel();
    let ctx = ctx.clone();
    let dir = dir.to_path_buf();
    std::thread::spawn(move || {
        let picked = rfd::FileDialog::new()
            .set_title("Pick a reference image")
            .add_filter("PNG image", &["png"])
            .set_directory(&dir)
            .pick_file();
        let _ = tx.send(picked);
        ctx.request_repaint();
    });
    Picker { rx, scope }
}

/// An id no reference, level, entity or note uses.
pub fn fresh_id(doc: &Document) -> Id {
    loop {
        let id = Id::generate();
        let p = &doc.project;
        let used = p.worlds.iter().any(|w| {
            w.references.iter().any(|r| r.id == id)
                || w.notes.iter().any(|n| n.id == id)
                || w.levels.iter().any(|l| {
                    l.id == id
                        || l.references.iter().any(|r| r.id == id)
                        || l.notes.iter().any(|n| n.id == id)
                        || l.all_entities().any(|e| e.id == id)
                })
        });
        if !used {
            return id;
        }
    }
}

/// What adding a reference image makes: the command, the new id, and what
/// was copied into `art/`, if anything.
pub type NewReference = (Command, Id, Option<String>);

/// The `add_reference` for the image at `path` (an image outside the
/// project is copied into `art/` first, as the Project pane does for
/// tilesets) at `scope`, its top-left corner at `px`, half opaque so the
/// grid shows through. Returns the command, the new id and what was
/// copied, if anything.
pub fn new_reference(
    doc: &Document,
    path: &Path,
    scope: NoteScope,
    px: [i64; 2],
) -> Result<NewReference, String> {
    let (rel, copied) = super::project_pane::project_image(&doc.dir, path)?;
    let id = fresh_id(doc);
    let mut r = Reference::new(id.clone(), rel, px);
    r.opacity = 0.5;
    Ok((
        Command::AddReference {
            scope,
            reference: r,
            index: None,
        },
        id,
        copied,
    ))
}

/// Where "Add reference image…" puts the image: the top-left corner of
/// the level (level scope), or of the active level in the world (world
/// scope; `[0, 0]` with no level).
pub fn default_px(doc: &Document, view: &crate::view::ViewState, scope: &NoteScope) -> [i64; 2] {
    match scope {
        NoteScope::Level(_) => [0, 0],
        NoteScope::World(_) => view
            .active_level
            .filter(|&i| i < doc.levels.len())
            .map_or([0, 0], |i| doc.level(i).world_px),
    }
}

/// The references the Layers pane lists: the active level's, then those
/// of the world shown.
fn listed<'a>(doc: &'a Document, view: &crate::view::ViewState) -> Vec<RefAt<'a>> {
    let world = view.world_ix(doc);
    let active = view
        .active_level
        .filter(|&i| i < doc.levels.len() && doc.levels[i].world == world);
    let order: Vec<usize> = active.into_iter().collect();
    let mut out: Vec<RefAt<'a>> = refs::shown(doc, world, &order)
        .into_iter()
        .filter(|r| r.level.is_some())
        .collect();
    out.extend(refs::shown(doc, world, &[]));
    out
}

/// The References pseudo-layer, the last row of the Layers pane.
pub fn layers_row(ui: &mut egui::Ui, st: &mut EditorState) {
    let EditorState {
        doc,
        view,
        pending,
        actions,
        ..
    } = st;
    let Some(doc) = doc.as_ref() else {
        return;
    };
    let shown = listed(doc, view);
    let world = view.world_ix(doc);
    let total = refs::shown(doc, world, &(0..doc.levels.len()).collect::<Vec<_>>()).len();
    ui.separator();
    let level = view_level(doc, view);
    let rv = &mut view.references;
    ui.horizontal(|ui| {
        let eye = RichText::new(if rv.ui.visible { "👁" } else { "–" }).size(15.0);
        ui.toggle_value(&mut rv.ui.visible, eye)
            .on_hover_text("Show reference images");
        let lock = if rv.ui.locked {
            RichText::new("🔒").size(15.0)
        } else {
            RichText::new("🔓").size(15.0).weak()
        };
        ui.toggle_value(&mut rv.ui.locked, lock)
            .on_hover_text("Locked: the map doesn't move or scale reference images");
        let arrow = if rv.expanded { "⏷" } else { "⏵" };
        if ui.small_button(arrow).on_hover_text("List them").clicked() {
            rv.expanded = !rv.expanded;
        }
        let name = RichText::new(format!("References ({total})")).italics();
        let r = ui
            .selectable_label(rv.active, name)
            .on_hover_text(
                "Images drawn under every layer, never exported. Make this the active layer to \
                 select, drag and scale them on the map with the Select tool. Right click to add one",
            );
        super::fields::mark(ui, "references", r.rect);
        if r.clicked() {
            rv.active = true;
            rv.expanded = true;
        }
        r.context_menu(|ui| add_menu(ui, doc, level, world, actions));
    });
    let rv = &mut view.references;
    if !rv.expanded {
        return;
    }
    ui.indent("references list", |ui| {
        if shown.is_empty() {
            ui.weak("None here. Right click References to add an image.");
        }
        for r in &shown {
            let id = &r.reference.id;
            ui.horizontal(|ui| {
                let mut visible = r.reference.visible;
                let eye = RichText::new(if visible { "👁" } else { "–" }).size(13.0);
                let e = ui
                    .toggle_value(&mut visible, eye)
                    .on_hover_text("Visible (saved in the file)");
                super::fields::mark(ui, &format!("references:{id}:eye"), e.rect);
                if e.changed() {
                    let mut new = r.reference.clone();
                    new.visible = visible;
                    pending.push(Command::UpdateReference {
                        id: id.clone(),
                        reference: new,
                    });
                }
                let where_ = match r.level {
                    Some(_) => "level",
                    None => "world",
                };
                let file = Path::new(&r.reference.image)
                    .file_name()
                    .map_or(r.reference.image.clone(), |f| {
                        f.to_string_lossy().into_owned()
                    });
                let mut text = RichText::new(format!("{file} ({where_})"));
                if r.reference.locked {
                    text = text.weak();
                }
                let sel = rv.selected.as_ref() == Some(id);
                let item = ui
                    .selectable_label(sel, text)
                    .on_hover_text(format!("{} at {:?}", r.reference.image, r.reference.px));
                super::fields::mark(ui, &format!("references:{id}"), item.rect);
                if item.clicked() {
                    rv.selected = Some(id.clone());
                    rv.active = true;
                    actions.push(Action::ShowInspector);
                }
            });
        }
    });
}

/// The level "Add reference image…" targets: the active one.
fn view_level(doc: &Document, view: &crate::view::ViewState) -> Option<usize> {
    view.active_level.filter(|&i| i < doc.levels.len())
}

/// "Add reference image to <level>…" and "… to the world…" (world
/// `world`, the one shown).
pub fn add_menu(
    ui: &mut egui::Ui,
    doc: &Document,
    level: Option<usize>,
    world: usize,
    actions: &mut Vec<Action>,
) {
    if let Some(ix) = level {
        let slug = doc.level(ix).slug.clone();
        if ui
            .button(format!("Add reference image to {slug}…"))
            .clicked()
        {
            actions.push(Action::AddReference(Some(NoteScope::Level(slug))));
            ui.close();
        }
    }
    if let Some(w) = doc.project.worlds.get(world) {
        let label = if doc.project.worlds.len() == 1 {
            "Add reference image to the world…".to_string()
        } else {
            format!("Add reference image to world {}…", w.slug)
        };
        if ui.button(label).clicked() {
            actions.push(Action::AddReference(Some(NoteScope::World(w.slug.clone()))));
            ui.close();
        }
    }
}

/// The Inspector part for the selected reference. Returns how many it
/// showed (0 or 1). Edits are `update_reference`s, one per change (a
/// dragged value commits when the drag ends).
pub fn inspector_section(ui: &mut egui::Ui, st: &mut EditorState) -> usize {
    let EditorState {
        doc, view, pending, ..
    } = st;
    let Some(doc) = doc.as_ref() else {
        return 0;
    };
    let rv = &mut view.references;
    let Some(sel) = rv.selected.clone() else {
        return 0;
    };
    let Some(at) = refs::find(doc, &sel) else {
        // Removed (undo, an agent): nothing selected.
        rv.selected = None;
        rv.edit = None;
        return 0;
    };
    let stored = at.reference.clone();
    if rv.edit.as_ref().is_some_and(|e| e.id != stored.id) {
        rv.edit = None;
    }
    let mut r = rv.edit.clone().unwrap_or_else(|| stored.clone());
    let size = refs::image_size(rv, &stored);
    let scope = match at.level {
        Some(ix) => format!("level {}", doc.level(ix).slug),
        None => format!("world {}", doc.project.worlds[at.world].slug),
    };
    ui.heading("Reference image");
    ui.weak(format!("{} · {scope}", stored.id));
    let mut dragging = false;
    let mut commit = false;
    egui::Grid::new("reference")
        .num_columns(2)
        .striped(true)
        .show(ui, |ui| {
            ui.weak("Image");
            let resp = ui.text_edit_singleline(&mut r.image);
            dragging |= resp.has_focus();
            commit |= resp.lost_focus();
            ui.end_row();
            ui.weak("Size");
            ui.label(if rv.sizes.contains_key(&stored.image) {
                format!("{} x {} px", size[0], size[1])
            } else {
                "not loaded (missing or not a PNG)".into()
            });
            ui.end_row();
            ui.weak("Position");
            ui.horizontal(|ui| {
                for k in 0..2 {
                    let resp = ui.add(egui::DragValue::new(&mut r.px[k]).speed(1.0));
                    dragging |= resp.dragged() || resp.has_focus();
                    commit |= resp.drag_stopped() || resp.lost_focus();
                }
            });
            ui.end_row();
            ui.weak("Scale");
            let resp = ui.add(
                egui::DragValue::new(&mut r.scale)
                    .speed(0.01)
                    .range(0.01..=100.0),
            );
            dragging |= resp.dragged() || resp.has_focus();
            commit |= resp.drag_stopped() || resp.lost_focus();
            ui.end_row();
            ui.weak("Opacity");
            let resp = ui.add(egui::Slider::new(&mut r.opacity, 0.0..=1.0));
            dragging |= resp.dragged();
            commit |= resp.drag_stopped() || (resp.changed() && !resp.dragged());
            ui.end_row();
            ui.weak("Locked");
            let c = ui
                .checkbox(&mut r.locked, "")
                .on_hover_text("The map doesn't move or scale it");
            super::fields::mark(ui, "reference:locked", c.rect);
            commit |= c.changed();
            ui.end_row();
            ui.weak("Visible");
            commit |= ui.checkbox(&mut r.visible, "").changed();
            ui.end_row();
        });
    if commit {
        rv.edit = None;
        if r != stored {
            pending.push(Command::UpdateReference {
                id: sel.clone(),
                reference: r,
            });
        }
    } else if dragging {
        rv.edit = Some(r);
    } else {
        rv.edit = None;
    }
    ui.horizontal(|ui| {
        let b = ui.button("Remove");
        super::fields::mark(ui, "reference:remove", b.rect);
        if b.clicked() {
            pending.push(Command::RemoveReference { id: sel.clone() });
            rv.selected = None;
        }
        if ui
            .button("Reset scale")
            .on_hover_text("Draw the image at its own size")
            .clicked()
            && stored.scale != 1.0
        {
            let mut new = stored.clone();
            new.scale = 1.0;
            pending.push(Command::UpdateReference {
                id: sel.clone(),
                reference: new,
            });
        }
    });
    1
}

/// Poll the file dialog; when it answered, the command that adds the
/// image (and the new id), or why not.
pub fn poll_picker(
    doc: &Document,
    view: &crate::view::ViewState,
    picker: &mut Option<Picker>,
) -> Option<Result<NewReference, String>> {
    let p = picker.as_ref()?;
    let res = match p.rx.try_recv() {
        Ok(r) => r,
        Err(std::sync::mpsc::TryRecvError::Empty) => return None,
        Err(std::sync::mpsc::TryRecvError::Disconnected) => None,
    };
    let p = picker.take().expect("checked above");
    let path = res?;
    let px = default_px(doc, view, &p.scope);
    Some(new_reference(doc, &path, p.scope, px))
}

#[cfg(test)]
mod tests {
    use super::super::testkit::{Driver, batches, state};
    use super::*;

    fn with_sketch(st: &mut EditorState) -> Id {
        let id = Id::new("refsketch2").unwrap();
        let doc = st.doc.as_mut().unwrap();
        doc.project
            .apply(Command::AddReference {
                scope: NoteScope::Level(worlds_core::slug("crevasse")),
                reference: Reference::new(id.clone(), "art/sketch.png", [0, 0]),
                index: None,
            })
            .unwrap();
        id
    }

    fn layers(ui: &mut egui::Ui, st: &mut EditorState) {
        layers_row(ui, st);
    }

    fn inspector(ui: &mut egui::Ui, st: &mut EditorState) {
        inspector_section(ui, st);
    }

    #[test]
    fn the_pseudo_layer_lists_and_selects_references() {
        let (_t, mut st) = state();
        let id = with_sketch(&mut st);
        let mut d = Driver::new(layers);
        d.frame(&mut st, vec![]);
        d.click(&mut st, d.rect("references").center());
        assert!(st.view.references.active && st.view.references.expanded);
        d.frame(&mut st, vec![]);
        d.click(&mut st, d.rect("references:refsketch2").center());
        assert_eq!(st.view.references.selected, Some(id.clone()));
        assert_eq!(st.actions, [Action::ShowInspector]);
        // The eye hides it, in the file.
        d.click(&mut st, d.rect("references:refsketch2:eye").center());
        let b = batches(&mut st);
        assert_eq!(b.len(), 1);
        match &b[0].1[0] {
            Command::UpdateReference { reference, .. } => assert!(!reference.visible),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_inspector_edits_and_removes_the_selected_reference() {
        let (_t, mut st) = state();
        let id = with_sketch(&mut st);
        st.view.references.selected = Some(id.clone());
        let mut d = Driver::new(inspector);
        d.frame(&mut st, vec![]);
        d.click(&mut st, d.rect("reference:locked").center());
        let b = batches(&mut st);
        match &b[0].1[0] {
            Command::UpdateReference { reference, .. } => assert!(reference.locked),
            other => panic!("{other:?}"),
        }
        d.click(&mut st, d.rect("reference:remove").center());
        assert_eq!(
            batches(&mut st)[0].1,
            [Command::RemoveReference { id: id.clone() }]
        );
        assert_eq!(st.view.references.selected, None);
    }

    #[test]
    fn only_the_world_shown_has_its_references_listed_and_drawn() {
        let (_t, mut st) = state();
        let main_level = with_sketch(&mut st);
        let doc = st.doc.as_mut().unwrap();
        let s = |x: &str| worlds_core::slug(x);
        let world_ref = |id: &str| Reference::new(Id::new(id).unwrap(), "art/sketch.png", [0, 0]);
        for c in [
            Command::CreateWorld {
                slug: s("dungeon"),
                name: None,
            },
            Command::AddReference {
                scope: NoteScope::World(s("main")),
                reference: world_ref("mainrefsaa"),
                index: None,
            },
            Command::AddReference {
                scope: NoteScope::World(s("dungeon")),
                reference: world_ref("dungrefsaa"),
                index: None,
            },
        ] {
            doc.project.apply(c).unwrap();
        }
        doc.reindex();
        let doc = st.doc.as_ref().unwrap();
        let ids = |v: Vec<RefAt<'_>>| -> Vec<String> {
            v.iter().map(|r| r.reference.id.to_string()).collect()
        };
        let every: Vec<usize> = (0..doc.levels.len()).collect();
        st.view.active_level = doc.level_ix("crevasse");
        assert_eq!(st.view.world_ix(doc), 0);
        assert_eq!(
            ids(listed(doc, &st.view)),
            [main_level.to_string(), "mainrefsaa".into()]
        );
        assert_eq!(ids(refs::shown(doc, 0, &every)).len(), 2);
        // Show the empty world: only its own reference.
        st.view.active_level = None;
        st.view.active_world = Some(s("dungeon"));
        assert_eq!(st.view.world_ix(doc), 1);
        assert_eq!(ids(listed(doc, &st.view)), ["dungrefsaa"]);
        assert_eq!(ids(refs::shown(doc, 1, &every)), ["dungrefsaa"]);
        assert!(refs::drawn(doc, &st.view.references, 1));
    }

    #[test]
    fn adding_copies_an_outside_image_into_art() {
        let (_t, st) = state();
        let doc = st.doc.as_ref().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let png = outside.path().join("sketch.png");
        image::RgbaImage::new(4, 2).save(&png).unwrap();
        let (cmd, id, copied) = new_reference(
            doc,
            &png,
            NoteScope::Level(worlds_core::slug("crevasse")),
            [0, 0],
        )
        .unwrap();
        assert!(copied.is_some());
        assert!(doc.dir.join("art/sketch.png").is_file());
        match cmd {
            Command::AddReference { reference, .. } => {
                assert_eq!(reference.id, id);
                assert_eq!(reference.image, "art/sketch.png");
                assert_eq!(reference.opacity, 0.5);
            }
            other => panic!("{other:?}"),
        }
        assert!(
            new_reference(
                doc,
                &outside.path().join("nope.png"),
                NoteScope::World(worlds_core::slug("main")),
                [0, 0]
            )
            .is_err()
        );
    }
}
