//! The Project pane (`Pane::Project`, tab "Project", next to the Types
//! tab): what a fresh project needs before anything can be painted.
//!
//! - **Layers** (`layers.json`): the layers top first, as in the Layers
//!   pane; + Layer (int_grid, tiles, auto, entities), ↑ ↓ to reorder
//!   (`set_layer_def` with `index`), × (asks first, saying in how many
//!   levels it has content; `remove_layer_def`, undo brings it all back).
//!   The open layer's form: slug (Enter renames: `rename_layer`), name,
//!   doc, kind, opacity, grid; an int_grid layer's values (color, glyph,
//!   slug, name, group; ↑ ↓ ×, + Value), its groups and `derived`; an auto
//!   layer's source and tile pivot (and its rules, a click away in the
//!   Rules pane); a tiles layer's default tileset. Each change is one
//!   `set_layer_def`.
//! - **Tilesets** (`tilesets.json`): + Tileset… picks a PNG (a native file
//!   dialog; an image outside the project is copied into `art/` first, so
//!   the project stays self-contained) and adds it with the project grid
//!   as its tile size; the form: slug (`rename_tileset`), image (Browse…),
//!   tile size, spacing, padding, and a preview of the atlas with the
//!   tiles as the editor cuts them. × is `remove_tileset_def`, refused
//!   (with the uses listed) while anything draws from it.
//! - **Settings** (`project.json`): name, grid size (only while there are
//!   no levels), background, collision layer and its blocking values,
//!   seed, new level size, export on save, each one `set_project_setting`.
//!
//! Every edit is one labelled undo entry. The pane's commands go through
//! [`ProjectState::pending`], so a refusal (changing the kind of a layer
//! with content, taking out a value cells still hold, removing a tileset
//! in use) comes back to the pane and is shown there, in red.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;

use egui::{Color32, RichText, Stroke, StrokeKind, Vec2};
use worlds_core::command::ProjectSetting;
use worlds_core::{
    AutoDef, Color, Command, IntGridDef, IntGridGroup, IntGridValue, LayerDef, LayerKind, Slug,
    TilesDef, TilesetDef,
};

use super::EditorState;
use super::fields::{ERROR, mark, text_commit};
use super::inspector::push;
use crate::doc::Document;

/// Which part of the pane shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Section {
    #[default]
    Layers,
    Tilesets,
    Settings,
}

impl Section {
    pub const ALL: [Section; 3] = [Section::Layers, Section::Tilesets, Section::Settings];

    pub fn name(self) -> &'static str {
        match self {
            Section::Layers => "layers",
            Section::Tilesets => "tilesets",
            Section::Settings => "settings",
        }
    }

    pub fn parse(s: &str) -> Option<Section> {
        Section::ALL.into_iter().find(|x| x.name() == s)
    }
}

/// A delete waiting for its confirmation.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Confirm {
    RemoveLayer(Slug),
}

/// What the file dialog's answer is for.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PickFor {
    /// A new tileset.
    New,
    /// The image of this tileset.
    Image(Slug),
}

/// A rename or a new definition the pane follows once its pending
/// commands all apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Follow {
    /// Open this layer's form; `activate`: also make it the active layer
    /// (a layer the pane just added).
    Layer {
        slug: Slug,
        activate: bool,
    },
    Tileset(Slug),
}

/// The Project pane's state.
#[derive(Default)]
pub struct ProjectState {
    pub section: Section,
    /// The layer whose form shows.
    pub layer: Option<Slug>,
    /// The tileset whose form shows.
    pub tileset: Option<Slug>,
    /// Bring the pane to the front next frame.
    pub focus: bool,
    /// The last refusal of a command the pane sent (or a file it couldn't
    /// use).
    pub error: Option<String>,
    /// What the pane did besides commands (an image copied into `art/`).
    pub notice: Option<String>,
    /// The pane's commands, applied after the frame's panels like
    /// [`EditorState::pending`]; a failure lands in [`Self::error`].
    pub pending: Vec<Command>,
    /// Followed once the pending commands applied ([`Self::applied`]).
    pub follow: Option<Follow>,
    confirm: Option<Confirm>,
    picker: Option<(Receiver<Option<PathBuf>>, PickFor)>,
    /// Where parts of the pane were drawn last frame (tests), by key.
    #[doc(hidden)]
    pub rects: HashMap<String, egui::Rect>,
}

impl ProjectState {
    /// Show `section` (and bring the pane to the front).
    pub fn show(&mut self, section: Section) {
        self.section = section;
        self.focus = true;
    }

    /// Show a layer's form.
    pub fn show_layer(&mut self, slug: Slug) {
        self.layer = Some(slug);
        self.show(Section::Layers);
    }

    /// The frame's pending commands were applied: `ok` if none was refused.
    /// Returns a layer to make active (one the pane just added).
    pub fn applied(&mut self, ok: bool) -> Option<Slug> {
        let f = self.follow.take()?;
        if !ok {
            return None;
        }
        match f {
            Follow::Layer { slug, activate } => {
                self.layer = Some(slug.clone());
                activate.then_some(slug)
            }
            Follow::Tileset(t) => {
                self.tileset = Some(t);
                None
            }
        }
    }
}

/// Colors for new values and groups, in turn.
const COLORS: [&str; 8] = [
    "#8c93a8", "#d8b45c", "#5c9ad8", "#d86b5c", "#6bc46b", "#b07cd8", "#5cd8c8", "#d8d05c",
];

/// Glyphs for new values and groups, first free one first.
const GLYPHS: &str =
    "#-~x=o^%&$/:;<>?ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwyz0123456789";

/// The first glyph no value or group of `def` uses.
fn free_glyph(def: &IntGridDef) -> char {
    GLYPHS
        .chars()
        .find(|c| {
            !def.values.iter().any(|v| v.glyph == *c)
                && !def.groups.iter().any(|g| g.glyph == Some(*c))
        })
        .unwrap_or('#')
}

fn color(s: &str) -> Color {
    Color::new(s).expect("a valid color")
}

/// `My tiles-2.png` → `my_tiles_2`: a slug from free text (`fallback` when
/// nothing is left).
pub fn slugify(s: &str, fallback: &str) -> String {
    let mut out = String::new();
    for c in s.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('_') && !out.is_empty() {
            out.push('_');
        }
    }
    let out = out.trim_end_matches('_').to_string();
    match out.chars().next() {
        Some('a'..='z') => out,
        Some(_) => format!("{fallback}_{out}"),
        None => fallback.to_string(),
    }
}

/// "collision" → "Collision", "decor_back" → "Decor back".
fn title(slug: &str) -> String {
    let s = slug.replace('_', " ");
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => s,
    }
}

/// The layer kinds + Layer offers: (kind, label, what it is).
pub const KINDS: [(&str, &str, &str); 4] = [
    (
        "int_grid",
        "Int grid",
        "A named value (or nothing) per cell: collision, water, zones. Rules read it.",
    ),
    (
        "tiles",
        "Tiles",
        "Tiles you place by hand from a tileset: decoration.",
    ),
    (
        "auto",
        "Auto",
        "Tiles drawn by rules from an int_grid layer: walls that tile themselves.",
    ),
    (
        "entities",
        "Entities",
        "Doors, pickups, spawn points: instances of the entity types.",
    ),
];

/// A new layer of `kind` (`int_grid`, `tiles`, `auto`, `entities`) on top
/// of the others, with a fresh slug: an int_grid layer starts with one
/// value (`solid`), an auto layer reads the topmost int_grid layer, a tiles
/// layer defaults to the first tileset. Refused (with the reason) for an
/// auto layer when there is no int_grid layer to read.
pub fn new_layer(doc: &Document, kind: &str) -> Result<(Command, Slug), String> {
    let p = &doc.project;
    let taken = |s: &str| p.layer(s).is_some();
    let (base, kind) = match kind {
        "int_grid" => {
            let base = if p.layers.iter().any(|l| l.is_int_grid()) {
                "grid"
            } else {
                "collision"
            };
            let values = vec![IntGridValue {
                slug: Slug::new("solid").expect("a valid slug"),
                name: "Solid".into(),
                color: color(COLORS[0]),
                glyph: '#',
                group: None,
            }];
            (
                base,
                LayerKind::IntGrid(IntGridDef {
                    values,
                    ..Default::default()
                }),
            )
        }
        "tiles" => (
            "decor",
            LayerKind::Tiles(TilesDef {
                tileset: p.tilesets.first().map(|t| t.slug.clone()),
            }),
        ),
        "auto" => {
            let Some(source) = p.layers.iter().rev().find(|l| l.is_int_grid()) else {
                return Err(
                    "An auto layer draws tiles from an int_grid layer's cells: add an int_grid \
                     layer first"
                        .into(),
                );
            };
            (
                "walls",
                LayerKind::Auto(AutoDef {
                    source: source.slug.clone(),
                    tile_pivot: [0.0, 0.0],
                }),
            )
        }
        "entities" => ("objects", LayerKind::Entities),
        other => return Err(format!("unknown layer kind {other:?}")),
    };
    let slug = super::defs::unique_slug(base, taken);
    let def = LayerDef::new(slug.clone(), title(slug.as_str()), kind);
    let cmd = Command::Batch {
        label: format!("New {} layer {slug}", def.kind.name()),
        commands: vec![Command::SetLayerDef {
            layer: slug.clone(),
            def,
            index: None,
            content: Vec::new(),
            rules: None,
        }],
    };
    Ok((cmd, slug))
}

/// How many levels hold content of layer `slug` (cells, overrides, tiles,
/// entities, rule state).
pub fn content_levels(doc: &Document, slug: &str) -> usize {
    doc.project
        .levels()
        .filter(|l| {
            l.int_grid(slug).is_some()
                || l.overrides(slug).is_some()
                || l.tiles_layer(slug).is_some()
                || l.entities_layer(slug).is_some()
                || l.layers.contains_key(slug)
        })
        .count()
}

/// A tileset for the PNG at `path`: its slug from the file name, the
/// project grid as its tile size. The image path is relative to the
/// project; an image outside the project directory is copied into `art/`
/// first (an identical file there is reused, a different one gets a new
/// name). Returns the command, the new slug and what was copied, if
/// anything.
pub fn tileset_from_file(
    doc: &Document,
    path: &Path,
) -> Result<(Command, Slug, Option<String>), String> {
    let (rel, copied) = project_image(&doc.dir, path)?;
    let stem = Path::new(&rel)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let slug = super::defs::unique_slug(&slugify(&stem, "tileset"), |s| {
        doc.project.tileset(s).is_some()
    });
    let g = doc.project.grid_size.max(1);
    let def = TilesetDef {
        slug: slug.clone(),
        image: rel,
        tile_size: [g, g],
        spacing: 0,
        padding: 0,
    };
    Ok((
        Command::Batch {
            label: format!("New tileset {slug}"),
            commands: vec![Command::SetTilesetDef {
                tileset: slug.clone(),
                def,
                index: None,
            }],
        },
        slug,
        copied,
    ))
}

/// The project-relative, `/`-separated path of the image at `path`,
/// copying it into `<dir>/art/` when it is outside the project. Refused
/// when it isn't a readable image.
pub fn project_image(dir: &Path, path: &Path) -> Result<(String, Option<String>), String> {
    let shown = path.display();
    let abs = path
        .canonicalize()
        .map_err(|e| format!("Can't open {shown}: {e}"))?;
    image::image_dimensions(&abs)
        .map_err(|e| format!("{shown} isn't an image we can read: {e}"))?;
    let root = dir
        .canonicalize()
        .map_err(|e| format!("Can't open the project directory: {e}"))?;
    let rel_of = |p: &Path| {
        p.components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/")
    };
    if let Ok(rel) = abs.strip_prefix(&root) {
        return Ok((rel_of(rel), None));
    }
    let name = abs
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| format!("{shown} has no file name"))?;
    let art = root.join("art");
    std::fs::create_dir_all(&art).map_err(|e| format!("Can't create art/: {e}"))?;
    let bytes = std::fs::read(&abs).map_err(|e| format!("Can't read {shown}: {e}"))?;
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) => (s.to_string(), format!(".{e}")),
        None => (name.clone(), String::new()),
    };
    for n in 1.. {
        let file = if n == 1 {
            name.clone()
        } else {
            format!("{stem}_{n}{ext}")
        };
        let dest = art.join(&file);
        match std::fs::read(&dest) {
            Ok(old) if old == bytes => return Ok((format!("art/{file}"), None)),
            Ok(_) => continue,
            Err(_) => {
                std::fs::write(&dest, &bytes)
                    .map_err(|e| format!("Can't write art/{file}: {e}"))?;
                return Ok((
                    format!("art/{file}"),
                    Some(format!(
                        "Copied {name} into art/{file}: a project keeps its images in its folder"
                    )),
                ));
            }
        }
    }
    unreachable!("an unbounded loop returns")
}

/// Start the native file dialog for a PNG (on its own thread; the answer
/// comes back through the receiver).
fn start_picker(ctx: &egui::Context, dir: &Path) -> Receiver<Option<PathBuf>> {
    let (tx, rx) = std::sync::mpsc::channel();
    let ctx = ctx.clone();
    let dir = dir.to_path_buf();
    std::thread::spawn(move || {
        let picked = rfd::FileDialog::new()
            .set_title("Pick a tileset image")
            .add_filter("PNG image", &["png"])
            .set_directory(&dir)
            .pick_file();
        let _ = tx.send(picked);
        ctx.request_repaint();
    });
    rx
}

/// The Project pane.
pub fn pane(ui: &mut egui::Ui, st: &mut EditorState) {
    let EditorState {
        doc,
        edit_bufs: bufs,
        atlas,
        project: ps,
        rules,
        types,
        ..
    } = st;
    let Some(doc) = doc.as_ref() else {
        ui.weak("No project");
        return;
    };
    ps.rects.clear();
    poll_picker(doc, ps);
    // A layer or tileset that went away (undo, a rename): back to the list.
    if ps
        .layer
        .as_ref()
        .is_some_and(|l| doc.project.layer(l.as_str()).is_none())
    {
        ps.layer = None;
    }
    if ps
        .tileset
        .as_ref()
        .is_some_and(|t| doc.project.tileset(t.as_str()).is_none())
    {
        ps.tileset = None;
    }
    ui.horizontal(|ui| {
        for s in Section::ALL {
            let label = match s {
                Section::Layers => format!("Layers ({})", doc.project.layers.len()),
                Section::Tilesets => format!("Tilesets ({})", doc.project.tilesets.len()),
                Section::Settings => "Settings".into(),
            };
            let r = ui.selectable_label(ps.section == s, RichText::new(label).strong());
            ps.rects.insert(format!("section:{}", s.name()), r.rect);
            if r.clicked() {
                ps.section = s;
            }
        }
    });
    if let Some(e) = ps.error.clone() {
        ui.horizontal_wrapped(|ui| {
            ui.colored_label(ERROR, e);
            if ui.small_button("×").clicked() {
                ps.error = None;
            }
        });
    }
    if let Some(n) = ps.notice.clone() {
        ui.horizontal_wrapped(|ui| {
            ui.weak(n);
            if ui.small_button("×").clicked() {
                ps.notice = None;
            }
        });
    }
    ui.separator();
    egui::ScrollArea::vertical()
        .id_salt("project pane")
        .auto_shrink([false, false])
        .show(ui, |ui| match ps.section {
            Section::Layers => layers_section(ui, doc, bufs, ps, rules, types),
            Section::Tilesets => tilesets_section(ui, doc, bufs, atlas, ps),
            Section::Settings => settings_section(ui, doc, bufs, ps),
        });
}

/// The file dialog's answer, if it came.
fn poll_picker(doc: &Document, ps: &mut ProjectState) {
    let Some((rx, _)) = &ps.picker else {
        return;
    };
    let Ok(res) = rx.try_recv() else {
        return;
    };
    let (_, what) = ps.picker.take().expect("checked above");
    if let Some(path) = res {
        pick_image(doc, ps, what_slug(&what), &path);
    }
}

fn what_slug(p: &PickFor) -> Option<Slug> {
    match p {
        PickFor::New => None,
        PickFor::Image(s) => Some(s.clone()),
    }
}

/// Use the image at `path`: a new tileset (`for_tileset` `None`), or the
/// image of that tileset. Errors go to the pane.
pub fn pick_image(doc: &Document, ps: &mut ProjectState, for_tileset: Option<Slug>, path: &Path) {
    match for_tileset {
        None => match tileset_from_file(doc, path) {
            Ok((cmd, slug, copied)) => {
                ps.pending.push(cmd);
                ps.follow = Some(Follow::Tileset(slug));
                ps.notice = copied;
                ps.section = Section::Tilesets;
            }
            Err(e) => ps.error = Some(e),
        },
        Some(t) => {
            let Some(def) = doc.project.tileset(t.as_str()) else {
                return;
            };
            match project_image(&doc.dir, path) {
                Ok((rel, copied)) => {
                    ps.notice = copied;
                    set_tileset(ps, def, "image", |d| d.image = rel);
                }
                Err(e) => ps.error = Some(e),
            }
        }
    }
}

// ---- layers ----

fn layers_section(
    ui: &mut egui::Ui,
    doc: &Document,
    bufs: &mut HashMap<String, String>,
    ps: &mut ProjectState,
    rules: &mut super::rules::RulesState,
    types: &mut super::defs::TypesState,
) {
    ui.horizontal(|ui| {
        let r = ui.menu_button("+ Layer", |ui| {
            for (kind, label, what) in KINDS {
                let refusal = (kind == "auto"
                    && !doc.project.layers.iter().any(|l| l.is_int_grid()))
                .then_some("Add an int_grid layer first: an auto layer reads one");
                let r = ui
                    .add_enabled(refusal.is_none(), egui::Button::new(label))
                    .on_hover_text(what)
                    .on_disabled_hover_text(refusal.unwrap_or_default());
                if r.clicked() {
                    match new_layer(doc, kind) {
                        Ok((cmd, slug)) => {
                            ps.pending.push(cmd);
                            ps.follow = Some(Follow::Layer {
                                slug,
                                activate: true,
                            });
                        }
                        Err(e) => ps.error = Some(e),
                    }
                    ui.close();
                }
            }
        });
        ps.rects.insert("add layer".into(), r.response.rect);
        ui.weak("saved in layers.json");
    });
    if doc.project.layers.is_empty() {
        ui.add_space(4.0);
        ui.label(
            "No layers yet. Start with an int_grid layer (collision): a value per cell, \
             which you paint and rules read.",
        );
    }
    ui.add_space(4.0);
    let n = doc.project.layers.len();
    egui::Grid::new("project layers")
        .num_columns(3)
        .striped(true)
        .show(ui, |ui| {
            for i in (0..n).rev() {
                let def = &doc.project.layers[i];
                let open = ps.layer.as_ref() == Some(&def.slug);
                ui.weak(if def.is_derived() {
                    "derived"
                } else {
                    def.kind.name()
                });
                let r = ui
                    .selectable_label(open, format!("{}  ·  {}", def.name, def.slug))
                    .on_hover_text(def.doc.as_deref().unwrap_or("Click to edit"));
                ps.rects.insert(format!("layer:{}", def.slug), r.rect);
                if r.clicked() {
                    ps.layer = if open { None } else { Some(def.slug.clone()) };
                }
                ui.horizontal(|ui| {
                    let mv = |to: usize| Command::Batch {
                        label: format!("Move layer {}", def.slug),
                        commands: vec![Command::SetLayerDef {
                            layer: def.slug.clone(),
                            def: def.clone(),
                            index: Some(to),
                            content: Vec::new(),
                            rules: None,
                        }],
                    };
                    let up = ui
                        .add_enabled(i + 1 < n, egui::Button::new("⏶").small())
                        .on_hover_text("Draw above the layer over it");
                    ps.rects.insert(format!("layer:{}:up", def.slug), up.rect);
                    if up.clicked() {
                        ps.pending.push(mv(i + 1));
                    }
                    let down = ui
                        .add_enabled(i > 0, egui::Button::new("⏷").small())
                        .on_hover_text("Draw below the layer under it");
                    ps.rects
                        .insert(format!("layer:{}:down", def.slug), down.rect);
                    if down.clicked() {
                        ps.pending.push(mv(i - 1));
                    }
                    let x = ui
                        .small_button("×")
                        .on_hover_text("Delete the layer (asks first)");
                    ps.rects
                        .insert(format!("layer:{}:remove", def.slug), x.rect);
                    if x.clicked() {
                        ps.confirm = Some(Confirm::RemoveLayer(def.slug.clone()));
                    }
                });
                ui.end_row();
            }
        });
    if let Some(Confirm::RemoveLayer(slug)) = ps.confirm.clone() {
        let n = content_levels(doc, slug.as_str());
        egui::Frame::group(ui.style())
            .stroke(Stroke::new(1.0, ERROR))
            .show(ui, |ui| {
                ui.label(if n == 0 {
                    format!("Delete layer {slug}? Undo brings it back.")
                } else {
                    format!(
                        "Delete layer {slug} and its content in {n} level{}? Undo brings it \
                         all back.",
                        if n == 1 { "" } else { "s" }
                    )
                });
                ui.horizontal(|ui| {
                    let r = ui.button("Delete");
                    ps.rects.insert("confirm delete".into(), r.rect);
                    if r.clicked() {
                        push(
                            &mut ps.pending,
                            format!("Delete layer {slug}"),
                            vec![Command::RemoveLayerDef {
                                layer: slug.clone(),
                            }],
                        );
                        ps.confirm = None;
                    }
                    if ui.button("Cancel").clicked() {
                        ps.confirm = None;
                    }
                });
            });
    }
    let Some(def) = ps
        .layer
        .as_ref()
        .and_then(|l| doc.project.layer(l.as_str()))
    else {
        if !doc.project.layers.is_empty() {
            ui.add_space(6.0);
            ui.weak("Click a layer to edit it.");
        }
        return;
    };
    ui.separator();
    layer_form(ui, doc, bufs, ps, rules, types, def);
}

/// A copy of `def` changed by `f`, as one labelled `set_layer_def`. Edits
/// committed in one frame build on each other (and on a queued rename).
fn set_layer(ps: &mut ProjectState, def: &LayerDef, what: &str, f: impl FnOnce(&mut LayerDef)) {
    let mut base = def.clone();
    for c in ps.pending.iter().flat_map(batch_commands) {
        match c {
            Command::SetLayerDef { layer, def: d, .. } if *layer == base.slug => base = d.clone(),
            Command::RenameLayer { from, to } if *from == base.slug => base.slug = to.clone(),
            _ => {}
        }
    }
    let mut d = base.clone();
    f(&mut d);
    if d != base {
        push(
            &mut ps.pending,
            format!("Edit layer {}: {what}", base.slug),
            vec![Command::SetLayerDef {
                layer: base.slug.clone(),
                def: d,
                index: None,
                content: Vec::new(),
                rules: None,
            }],
        );
    }
}

fn batch_commands(c: &Command) -> &[Command] {
    match c {
        Command::Batch { commands, .. } => commands,
        other => std::slice::from_ref(other),
    }
}

/// The open layer's form.
fn layer_form(
    ui: &mut egui::Ui,
    doc: &Document,
    bufs: &mut HashMap<String, String>,
    ps: &mut ProjectState,
    rules: &mut super::rules::RulesState,
    types: &mut super::defs::TypesState,
    def: &LayerDef,
) {
    let slug = &def.slug;
    let k = |what: &str| format!("layer:{slug}:{what}");
    ui.heading(&def.name);
    egui::Grid::new(("layer form", slug.as_str()))
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
                                &mut ps.pending,
                                format!("Rename layer {slug} to {to}"),
                                vec![Command::RenameLayer {
                                    from: slug.clone(),
                                    to: to.clone(),
                                }],
                            );
                            ps.follow = Some(Follow::Layer {
                                slug: to,
                                activate: false,
                            });
                        }
                        Err(e) => ps.error = Some(format!("Bad slug: {e}")),
                    }
                }
                ui.weak("(Enter renames)").on_hover_text(
                    "Level files, rules and the collision layer setting follow the rename",
                );
            });
            ui.end_row();
            ui.weak("Name");
            if let Some(s) = text_commit(ui, bufs, &k("name"), &def.name, false, false, 160.0) {
                set_layer(ps, def, "name", |d| d.name = s);
            }
            ui.end_row();
            ui.weak("Kind");
            kind_combo(ui, doc, ps, def);
            ui.end_row();
            ui.weak("Doc");
            let text = def.doc.clone().unwrap_or_default();
            if let Some(s) = text_commit(ui, bufs, &k("doc"), &text, false, true, 180.0) {
                set_layer(ps, def, "doc", |d| {
                    d.doc = (!s.trim().is_empty()).then_some(s)
                });
            }
            ui.end_row();
            ui.weak("Opacity");
            if let Some(v) = slider01(ui, bufs, &k("opacity"), def.opacity) {
                set_layer(ps, def, "opacity", |d| d.opacity = v);
            }
            ui.end_row();
            if !def.is_auto() {
                ui.weak("Grid (px)");
                ui.horizontal(|ui| {
                    let cur = def.grid_size.map_or(String::new(), |g| g.to_string());
                    let r = text_commit(ui, bufs, &k("grid"), &cur, false, false, 40.0);
                    ui.weak(format!("empty: the project's {} px", doc.project.grid_size));
                    if let Some(s) = r {
                        match s.trim() {
                            "" => set_layer(ps, def, "grid", |d| d.grid_size = None),
                            t => match t.parse::<u32>() {
                                Ok(g) if g > 0 => {
                                    set_layer(ps, def, "grid", |d| d.grid_size = Some(g))
                                }
                                _ => ps.error = Some(format!("Grid: {t:?} isn't a size in px")),
                            },
                        }
                    }
                });
                ui.end_row();
            }
            match &def.kind {
                LayerKind::IntGrid(g) => {
                    ui.weak("Derived");
                    let mut d = g.derived;
                    if ui
                        .checkbox(&mut d, "computed by rules (write outputs)")
                        .on_hover_text(
                            "A derived layer holds what an auto layer's write rules compute; \
                             painting sets overrides",
                        )
                        .changed()
                    {
                        set_layer(ps, def, "derived", |l| {
                            if let Some(g) = l.int_grid_mut() {
                                g.derived = d;
                            }
                        });
                    }
                    ui.end_row();
                }
                LayerKind::Auto(a) => {
                    ui.weak("Source");
                    egui::ComboBox::from_id_salt(("auto source", slug.as_str()))
                        .selected_text(a.source.as_str())
                        .show_ui(ui, |ui| {
                            for l in doc.project.layers.iter().filter(|l| l.is_int_grid()) {
                                if ui
                                    .selectable_label(l.slug == a.source, l.slug.as_str())
                                    .clicked()
                                {
                                    let s = l.slug.clone();
                                    set_layer(ps, def, "source", |d| {
                                        if let Some(a) = d.auto_mut() {
                                            a.source = s;
                                        }
                                    });
                                }
                            }
                        })
                        .response
                        .on_hover_text("The int_grid layer the rules read by default");
                    ui.end_row();
                    ui.weak("Tile pivot");
                    ui.horizontal(|ui| {
                        for (i, axis) in ["x", "y"].into_iter().enumerate() {
                            ui.weak(axis);
                            if let Some(s) = text_commit(
                                ui,
                                bufs,
                                &k(&format!("pivot.{axis}")),
                                &a.tile_pivot[i].to_string(),
                                false,
                                false,
                                36.0,
                            ) && let Ok(v) = s.trim().parse::<f64>()
                            {
                                set_layer(ps, def, "tile pivot", |d| {
                                    if let Some(a) = d.auto_mut() {
                                        a.tile_pivot[i] = v.clamp(0.0, 1.0);
                                    }
                                });
                            }
                        }
                    })
                    .response
                    .on_hover_text("Shifts stamp-mode rule output, in cells (0 to 1)");
                    ui.end_row();
                    ui.weak("Rules");
                    ui.horizontal(|ui| {
                        let (g, r) = doc.project.rules_for(slug.as_str()).map_or((0, 0), |s| {
                            (
                                s.groups.len(),
                                s.groups.iter().map(|g| g.rules.len()).sum::<usize>(),
                            )
                        });
                        ui.label(format!(
                            "{g} group{}, {r} rule{}",
                            if g == 1 { "" } else { "s" },
                            if r == 1 { "" } else { "s" }
                        ));
                        if ui
                            .button("Open in Rules")
                            .on_hover_text(format!("rules/{slug}.json"))
                            .clicked()
                        {
                            rules.layer = Some(slug.clone());
                            rules.focus = true;
                        }
                    });
                    ui.end_row();
                }
                LayerKind::Tiles(t) => {
                    ui.weak("Tileset");
                    tileset_combo(ui, doc, ps, def, t);
                    ui.end_row();
                }
                LayerKind::Entities => {
                    ui.weak("Types");
                    ui.horizontal(|ui| {
                        ui.label(format!("{} entity types", doc.project.entities.len()));
                        if ui
                            .button("Open Types")
                            .on_hover_text("Entity types are project-wide: the Types tab")
                            .clicked()
                        {
                            types.focus = true;
                        }
                    });
                    ui.end_row();
                }
            }
        });
    if let LayerKind::IntGrid(g) = &def.kind {
        ui.add_space(8.0);
        values_editor(ui, bufs, ps, def, g);
        ui.add_space(8.0);
        groups_editor(ui, bufs, ps, def, g);
    }
}

/// The kind combo: a new kind keeps slug, name, doc and opacity. The
/// command layer refuses a change while the layer has content.
fn kind_combo(ui: &mut egui::Ui, doc: &Document, ps: &mut ProjectState, def: &LayerDef) {
    egui::ComboBox::from_id_salt(("layer kind", def.slug.as_str()))
        .selected_text(def.kind.name())
        .show_ui(ui, |ui| {
            for (kind, label, what) in KINDS {
                let on = def.kind.name() == kind;
                if ui.selectable_label(on, label).on_hover_text(what).clicked() && !on {
                    let new = match kind {
                        "int_grid" => Some(LayerKind::IntGrid(IntGridDef::default())),
                        "tiles" => Some(LayerKind::Tiles(TilesDef::default())),
                        "entities" => Some(LayerKind::Entities),
                        _ => doc
                            .project
                            .layers
                            .iter()
                            .rev()
                            .find(|l| l.is_int_grid() && l.slug != def.slug)
                            .map(|s| {
                                LayerKind::Auto(AutoDef {
                                    source: s.slug.clone(),
                                    tile_pivot: [0.0, 0.0],
                                })
                            }),
                    };
                    match new {
                        Some(k) => set_layer(ps, def, "kind", |d| {
                            if matches!(k, LayerKind::Auto(_)) {
                                d.grid_size = None;
                            }
                            d.kind = k;
                        }),
                        None => {
                            ps.error = Some(
                                "An auto layer reads an int_grid layer: there is no other one"
                                    .into(),
                            )
                        }
                    }
                }
            }
        })
        .response
        .on_hover_text("Changing the kind needs a layer with no content in any level");
}

fn tileset_combo(
    ui: &mut egui::Ui,
    doc: &Document,
    ps: &mut ProjectState,
    def: &LayerDef,
    t: &TilesDef,
) {
    let shown = t.tileset.as_ref().map_or("none", |s| s.as_str());
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt(("tiles tileset", def.slug.as_str()))
            .selected_text(shown)
            .show_ui(ui, |ui| {
                if ui.selectable_label(t.tileset.is_none(), "none").clicked() {
                    set_layer(ps, def, "tileset", |d| {
                        d.kind = LayerKind::Tiles(TilesDef::default())
                    });
                }
                for ts in &doc.project.tilesets {
                    if ui
                        .selectable_label(t.tileset.as_ref() == Some(&ts.slug), ts.slug.as_str())
                        .clicked()
                    {
                        let s = ts.slug.clone();
                        set_layer(ps, def, "tileset", |d| {
                            d.kind = LayerKind::Tiles(TilesDef { tileset: Some(s) })
                        });
                    }
                }
            })
            .response
            .on_hover_text("The tileset the Palette opens on for this layer");
        if doc.project.tilesets.is_empty() && ui.button("Add a tileset…").clicked() {
            ps.section = Section::Tilesets;
        }
    });
}

/// A 0..1 slider committed on release (the value waits in `bufs`).
fn slider01(
    ui: &mut egui::Ui,
    bufs: &mut HashMap<String, String>,
    key: &str,
    cur: f64,
) -> Option<f64> {
    let mut v = bufs.get(key).and_then(|s| s.parse().ok()).unwrap_or(cur);
    let r = ui.add(egui::Slider::new(&mut v, 0.0..=1.0).max_decimals(2));
    if r.changed() {
        bufs.insert(key.to_string(), v.to_string());
    }
    let done =
        r.drag_stopped() || r.lost_focus() || (r.changed() && !r.dragged() && !r.has_focus());
    if done && let Some(v) = bufs.remove(key).and_then(|s| s.parse::<f64>().ok()) {
        return (v != cur).then_some(v);
    }
    None
}

/// One glyph typed into a box: the first character, if it can be a glyph.
fn parse_glyph(s: &str) -> Result<char, String> {
    let mut chars = s.trim().chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => match worlds_core::glyph_problem(c) {
            Some(p) => Err(p),
            None => Ok(c),
        },
        (None, _) => Err("a glyph is one character".into()),
        _ => Err(format!("{:?}: a glyph is one character", s.trim())),
    }
}

/// A header row of a grid, each column at least its width (a cell of a
/// grid column other than the last gets the column's width as it was laid
/// out, so a text box would never grow past its header otherwise).
fn grid_headers(ui: &mut egui::Ui, cols: &[(&str, f32)]) {
    let h = ui.text_style_height(&egui::TextStyle::Body);
    let color = ui.visuals().weak_text_color();
    for (text, w) in cols {
        let (r, _) = ui.allocate_exact_size(Vec2::new(*w, h), egui::Sense::hover());
        ui.painter().text(
            r.left_center(),
            egui::Align2::LEFT_CENTER,
            *text,
            egui::TextStyle::Body.resolve(ui.style()),
            color,
        );
    }
    ui.end_row();
}

/// An int_grid layer's values: color, glyph, slug, name, group; ⏶ ⏷ ×;
/// + Value.
fn values_editor(
    ui: &mut egui::Ui,
    bufs: &mut HashMap<String, String>,
    ps: &mut ProjectState,
    def: &LayerDef,
    g: &IntGridDef,
) {
    let slug = &def.slug;
    ui.strong("Values");
    ui.weak(
        "What a cell can hold. The glyph stands for the value in level files and rule patterns.",
    );
    let set = |ps: &mut ProjectState, what: &str, f: &dyn Fn(&mut IntGridDef)| {
        set_layer(ps, def, what, |d| {
            if let Some(g) = d.int_grid_mut() {
                f(g);
            }
        })
    };
    let n = g.values.len();
    egui::Grid::new(("values", slug.as_str()))
        .num_columns(6)
        .spacing([6.0, 4.0])
        .show(ui, |ui| {
            grid_headers(
                ui,
                &[
                    ("", 40.0),
                    ("glyph", 18.0),
                    ("slug", 64.0),
                    ("name", 72.0),
                    ("", 66.0),
                    (if g.groups.is_empty() { "" } else { "group" }, 10.0),
                ],
            );
            for (i, v) in g.values.iter().enumerate() {
                let k = |what: &str| format!("layer:{slug}:value:{i}:{what}");
                if let Some(hex) =
                    super::panels::color_field(ui, bufs, &k("color"), Some(v.color.as_str()))
                    && let Ok(c) = Color::new(hex)
                {
                    set(ps, "value color", &|g| g.values[i].color = c.clone());
                }
                if let Some(s) = text_commit(
                    ui,
                    bufs,
                    &k("glyph"),
                    &v.glyph.to_string(),
                    false,
                    false,
                    18.0,
                ) {
                    match parse_glyph(&s) {
                        Ok(c) => set(ps, "value glyph", &|g| g.values[i].glyph = c),
                        Err(e) => ps.error = Some(format!("Value {}: {e}", v.slug)),
                    }
                }
                if let Some(s) =
                    text_commit(ui, bufs, &k("slug"), v.slug.as_str(), false, false, 64.0)
                {
                    match Slug::new(s.trim()) {
                        Ok(to) => set(ps, "value slug", &|g| g.values[i].slug = to.clone()),
                        Err(e) => ps.error = Some(format!("Bad slug: {e}")),
                    }
                }
                if let Some(s) = text_commit(ui, bufs, &k("name"), &v.name, false, false, 72.0) {
                    set(ps, "value name", &|g| g.values[i].name = s.clone());
                }
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(i > 0, egui::Button::new("⏶").small())
                        .clicked()
                    {
                        set(ps, "value order", &|g| g.values.swap(i, i - 1));
                    }
                    if ui
                        .add_enabled(i + 1 < n, egui::Button::new("⏷").small())
                        .clicked()
                    {
                        set(ps, "value order", &|g| g.values.swap(i, i + 1));
                    }
                    let x = ui.small_button("×").on_hover_text(
                        "Remove the value (refused while cells hold it: erase them first)",
                    );
                    mark(ui, &k("remove"), x.rect);
                    if x.clicked() {
                        set(ps, "remove value", &|g| {
                            g.values.remove(i);
                        });
                    }
                });
                if !g.groups.is_empty() {
                    egui::ComboBox::from_id_salt(k("group"))
                        .width(70.0)
                        .selected_text(v.group.as_ref().map_or("—", |s| s.as_str()))
                        .show_ui(ui, |ui| {
                            if ui.selectable_label(v.group.is_none(), "—").clicked() {
                                set(ps, "value group", &|g| g.values[i].group = None);
                            }
                            for gr in &g.groups {
                                if ui
                                    .selectable_label(
                                        v.group.as_ref() == Some(&gr.slug),
                                        gr.slug.as_str(),
                                    )
                                    .clicked()
                                {
                                    let s = gr.slug.clone();
                                    set(ps, "value group", &|g| {
                                        g.values[i].group = Some(s.clone())
                                    });
                                }
                            }
                        });
                }
                ui.end_row();
            }
        });
    let r = ui.button("+ Value");
    mark(ui, &format!("layer:{slug}:add value"), r.rect);
    if r.clicked() {
        set(ps, "add value", &|g| {
            let taken = |s: &str| {
                g.values.iter().any(|v| v.slug == s) || g.groups.iter().any(|v| v.slug == s)
            };
            let s = super::defs::unique_slug("value", taken);
            let c = COLORS[g.values.len() % COLORS.len()];
            g.values.push(IntGridValue {
                name: title(s.as_str()),
                slug: s,
                color: color(c),
                glyph: free_glyph(g),
                group: None,
            });
        });
    }
}

/// An int_grid layer's groups (rule patterns can test "any value in this
/// group"): color, glyph, slug, name, ×; + Group.
fn groups_editor(
    ui: &mut egui::Ui,
    bufs: &mut HashMap<String, String>,
    ps: &mut ProjectState,
    def: &LayerDef,
    g: &IntGridDef,
) {
    let slug = &def.slug;
    ui.strong("Groups");
    ui.weak("A rule pattern can match any value of a group with the group's glyph.");
    let set = |ps: &mut ProjectState, what: &str, f: &dyn Fn(&mut IntGridDef)| {
        set_layer(ps, def, what, |d| {
            if let Some(g) = d.int_grid_mut() {
                f(g);
            }
        })
    };
    if !g.groups.is_empty() {
        egui::Grid::new(("groups", slug.as_str()))
            .num_columns(5)
            .spacing([6.0, 4.0])
            .show(ui, |ui| {
                grid_headers(
                    ui,
                    &[
                        ("", 40.0),
                        ("glyph", 18.0),
                        ("slug", 64.0),
                        ("name", 72.0),
                        ("", 10.0),
                    ],
                );
                for (i, gr) in g.groups.iter().enumerate() {
                    let k = |what: &str| format!("layer:{slug}:group:{i}:{what}");
                    let stored = gr.color.as_ref().map(|c| c.as_str());
                    if let Some(hex) = super::panels::color_field(ui, bufs, &k("color"), stored)
                        && let Ok(c) = Color::new(hex)
                    {
                        set(ps, "group color", &|g| g.groups[i].color = Some(c.clone()));
                    }
                    let glyph = gr.glyph.map_or(String::new(), |c| c.to_string());
                    if let Some(s) = text_commit(ui, bufs, &k("glyph"), &glyph, false, false, 18.0)
                    {
                        if s.trim().is_empty() {
                            set(ps, "group glyph", &|g| g.groups[i].glyph = None);
                        } else {
                            match parse_glyph(&s) {
                                Ok(c) => set(ps, "group glyph", &|g| g.groups[i].glyph = Some(c)),
                                Err(e) => ps.error = Some(format!("Group {}: {e}", gr.slug)),
                            }
                        }
                    }
                    if let Some(s) =
                        text_commit(ui, bufs, &k("slug"), gr.slug.as_str(), false, false, 64.0)
                    {
                        match Slug::new(s.trim()) {
                            Ok(to) => {
                                let from = gr.slug.clone();
                                set(ps, "group slug", &|g| {
                                    g.groups[i].slug = to.clone();
                                    for v in &mut g.values {
                                        if v.group.as_ref() == Some(&from) {
                                            v.group = Some(to.clone());
                                        }
                                    }
                                })
                            }
                            Err(e) => ps.error = Some(format!("Bad slug: {e}")),
                        }
                    }
                    if let Some(s) = text_commit(ui, bufs, &k("name"), &gr.name, false, false, 72.0)
                    {
                        set(ps, "group name", &|g| g.groups[i].name = s.clone());
                    }
                    if ui
                        .small_button("×")
                        .on_hover_text("Remove the group (its values stay, ungrouped)")
                        .clicked()
                    {
                        let gone = gr.slug.clone();
                        set(ps, "remove group", &|g| {
                            g.groups.remove(i);
                            for v in &mut g.values {
                                if v.group.as_ref() == Some(&gone) {
                                    v.group = None;
                                }
                            }
                        });
                    }
                    ui.end_row();
                }
            });
    }
    if ui.button("+ Group").clicked() {
        set(ps, "add group", &|g| {
            let taken = |s: &str| {
                g.values.iter().any(|v| v.slug == s) || g.groups.iter().any(|v| v.slug == s)
            };
            let s = super::defs::unique_slug("group", taken);
            g.groups.push(IntGridGroup {
                name: title(s.as_str()),
                slug: s,
                color: None,
                glyph: Some(free_glyph(g)),
            });
        });
    }
}

// ---- tilesets ----

/// A copy of tileset `def` changed by `f`, as one `set_tileset_def`.
fn set_tileset(
    ps: &mut ProjectState,
    def: &TilesetDef,
    what: &str,
    f: impl FnOnce(&mut TilesetDef),
) {
    let mut base = def.clone();
    for c in ps.pending.iter().flat_map(batch_commands) {
        match c {
            Command::SetTilesetDef {
                tileset, def: d, ..
            } if *tileset == base.slug => base = d.clone(),
            Command::RenameTileset { from, to } if *from == base.slug => base.slug = to.clone(),
            _ => {}
        }
    }
    let mut d = base.clone();
    f(&mut d);
    if d != base {
        push(
            &mut ps.pending,
            format!("Edit tileset {}: {what}", base.slug),
            vec![Command::SetTilesetDef {
                tileset: base.slug.clone(),
                def: d,
                index: None,
            }],
        );
    }
}

fn tilesets_section(
    ui: &mut egui::Ui,
    doc: &Document,
    bufs: &mut HashMap<String, String>,
    atlas: &mut crate::viewport::Atlas,
    ps: &mut ProjectState,
) {
    ui.horizontal(|ui| {
        let busy = ps.picker.is_some();
        let r = ui
            .add_enabled(!busy, egui::Button::new("+ Tileset…"))
            .on_hover_text(
                "Pick a PNG. One outside the project is copied into art/ first; the tile size \
                 starts at the project grid",
            );
        ps.rects.insert("add tileset".into(), r.rect);
        if r.clicked() {
            ps.picker = Some((start_picker(ui.ctx(), &doc.dir), PickFor::New));
        }
        ui.weak("saved in tilesets.json");
    });
    if doc.project.tilesets.is_empty() {
        ui.add_space(4.0);
        ui.label(
            "No tilesets yet. A tileset is one PNG cut into a grid of tiles: tiles layers \
             place them by hand, auto layers' rules draw them.",
        );
    }
    ui.add_space(4.0);
    egui::Grid::new("project tilesets")
        .num_columns(3)
        .striped(true)
        .show(ui, |ui| {
            for t in &doc.project.tilesets {
                let open = ps.tileset.as_ref() == Some(&t.slug);
                let r = ui.selectable_label(open, t.slug.as_str());
                ps.rects.insert(format!("tileset:{}", t.slug), r.rect);
                if r.clicked() {
                    ps.tileset = if open { None } else { Some(t.slug.clone()) };
                }
                ui.weak(format!(
                    "{}  ·  {}×{}",
                    t.image, t.tile_size[0], t.tile_size[1]
                ));
                let x = ui.small_button("×").on_hover_text(
                    "Remove the tileset (refused while a layer, rule or tile uses it)",
                );
                ps.rects
                    .insert(format!("tileset:{}:remove", t.slug), x.rect);
                if x.clicked() {
                    push(
                        &mut ps.pending,
                        format!("Delete tileset {}", t.slug),
                        vec![Command::RemoveTilesetDef {
                            tileset: t.slug.clone(),
                            keep_uses: false,
                        }],
                    );
                }
                ui.end_row();
            }
        });
    let Some(def) = ps
        .tileset
        .as_ref()
        .and_then(|t| doc.project.tileset(t.as_str()))
    else {
        if !doc.project.tilesets.is_empty() {
            ui.add_space(6.0);
            ui.weak("Click a tileset to edit it.");
        }
        return;
    };
    ui.separator();
    tileset_form(ui, doc, bufs, atlas, ps, def);
}

fn tileset_form(
    ui: &mut egui::Ui,
    doc: &Document,
    bufs: &mut HashMap<String, String>,
    atlas: &mut crate::viewport::Atlas,
    ps: &mut ProjectState,
    def: &TilesetDef,
) {
    let slug = &def.slug;
    let k = |what: &str| format!("tileset:{slug}:{what}");
    ui.heading(slug.as_str());
    egui::Grid::new(("tileset form", slug.as_str()))
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
                                &mut ps.pending,
                                format!("Rename tileset {slug} to {to}"),
                                vec![Command::RenameTileset {
                                    from: slug.clone(),
                                    to: to.clone(),
                                }],
                            );
                            ps.follow = Some(Follow::Tileset(to));
                        }
                        Err(e) => ps.error = Some(format!("Bad slug: {e}")),
                    }
                }
                ui.weak("(Enter renames)")
                    .on_hover_text("Tiles, rules, icons and fields that use it follow");
            });
            ui.end_row();
            ui.weak("Image");
            ui.horizontal(|ui| {
                if let Some(s) = text_commit(ui, bufs, &k("image"), &def.image, false, false, 150.0)
                {
                    let s = s.trim().replace('\\', "/");
                    if !s.is_empty() {
                        set_tileset(ps, def, "image", |d| d.image = s);
                    }
                }
                let busy = ps.picker.is_some();
                if ui
                    .add_enabled(!busy, egui::Button::new("Browse…"))
                    .on_hover_text("Pick another PNG (copied into art/ when outside the project)")
                    .clicked()
                {
                    ps.picker = Some((
                        start_picker(ui.ctx(), &doc.dir),
                        PickFor::Image(slug.clone()),
                    ));
                }
            });
            ui.end_row();
            ui.weak("Tile size (px)");
            ui.horizontal(|ui| {
                for (i, axis) in ["w", "h"].into_iter().enumerate() {
                    ui.weak(axis);
                    if let Some(s) = text_commit(
                        ui,
                        bufs,
                        &k(&format!("tile.{axis}")),
                        &def.tile_size[i].to_string(),
                        false,
                        false,
                        36.0,
                    ) {
                        match s.trim().parse::<u32>() {
                            Ok(n) if n > 0 => {
                                set_tileset(ps, def, "tile size", |d| d.tile_size[i] = n)
                            }
                            _ => {
                                ps.error =
                                    Some(format!("Tile size: {:?} isn't a size in px", s.trim()))
                            }
                        }
                    }
                }
            });
            ui.end_row();
            for (what, cur, hint) in [
                ("spacing", def.spacing, "Pixels between tiles in the image"),
                (
                    "padding",
                    def.padding,
                    "Pixels between the image edge and the first tile",
                ),
            ] {
                ui.weak(title(what)).on_hover_text(hint);
                if let Some(s) =
                    text_commit(ui, bufs, &k(what), &cur.to_string(), false, false, 36.0)
                {
                    match s.trim().parse::<u32>() {
                        Ok(n) => set_tileset(ps, def, what, |d| {
                            if what == "spacing" {
                                d.spacing = n;
                            } else {
                                d.padding = n;
                            }
                        }),
                        Err(_) => {
                            ps.error = Some(format!(
                                "{}: {:?} isn't a number of px",
                                title(what),
                                s.trim()
                            ))
                        }
                    }
                }
                ui.end_row();
            }
        });
    ui.add_space(6.0);
    let Some(tex) = super::panels::atlas_texture(atlas, ui.ctx(), doc, def) else {
        ui.colored_label(
            ERROR,
            format!(
                "Can't read {} (relative to the project folder; see the log).",
                def.image
            ),
        );
        return;
    };
    let [iw, ih] = tex.size();
    let (cols, rows) = (def.columns(iw as u32), def.rows(ih as u32));
    ui.weak(format!(
        "{iw} × {ih} px  ·  {cols} × {rows} tiles of {} × {} px, cut as outlined",
        def.tile_size[0], def.tile_size[1]
    ));
    // At its own size (twice that for a small image), scrolled: shrunk to
    // the pane, a big atlas's tiles and the cut lines would be unreadable.
    let img = tex.size_vec2();
    let scale = if img.x * 2.0 <= ui.available_width() {
        2.0
    } else {
        1.0
    };
    egui::ScrollArea::both()
        .id_salt(("tileset preview", slug.as_str()))
        .max_height(360.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            let r = ui.add(egui::Image::new((tex.id(), img * scale)));
            ps.rects.insert("tileset preview".into(), r.rect);
            let p = ui.painter_at(r.rect);
            let clip = ui.clip_rect();
            let stroke = Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 200, 80, 150));
            for row in 0..rows {
                for col in 0..cols {
                    let at = def.tile_px([col, row]);
                    let tr = egui::Rect::from_min_size(
                        r.rect.min + Vec2::new(at[0] as f32, at[1] as f32) * scale,
                        Vec2::new(def.tile_size[0] as f32, def.tile_size[1] as f32) * scale,
                    );
                    if tr.intersects(clip) {
                        p.rect_stroke(tr, 0.0, stroke, StrokeKind::Inside);
                    }
                }
            }
            if let Some(h) = r.hover_pos() {
                let local = (h - r.rect.min) / scale;
                let t = def.tile_at_px([local.x.max(0.0) as u32, local.y.max(0.0) as u32]);
                r.on_hover_text(format!("tile [{}, {}]", t[0], t[1]));
            }
        });
}

// ---- settings ----

fn settings_section(
    ui: &mut egui::Ui,
    doc: &Document,
    bufs: &mut HashMap<String, String>,
    ps: &mut ProjectState,
) {
    let p = &doc.project;
    let set = |ps: &mut ProjectState, label: String, settings: Vec<ProjectSetting>| {
        push(
            &mut ps.pending,
            label,
            settings
                .into_iter()
                .map(|setting| Command::SetProjectSetting { setting })
                .collect(),
        )
    };
    ui.weak("saved in project.json");
    egui::Grid::new("project settings")
        .num_columns(2)
        .striped(true)
        .show(ui, |ui| {
            ui.weak("Name");
            if let Some(s) = text_commit(ui, bufs, "project:name", &p.name, false, false, 160.0) {
                set(ps, "Project name".into(), vec![ProjectSetting::Name(s)]);
            }
            ui.end_row();
            ui.weak("Grid (px)").on_hover_text(
                "The size of a cell. Level sizes and positions are in these cells; layers use \
                 it unless they set their own",
            );
            let levels = doc.levels.len();
            ui.horizontal(|ui| {
                if levels == 0 {
                    if let Some(s) = text_commit(
                        ui,
                        bufs,
                        "project:grid",
                        &p.grid_size.to_string(),
                        false,
                        false,
                        40.0,
                    ) {
                        match s.trim().parse::<u32>() {
                            Ok(g) if g > 0 => set(
                                ps,
                                format!("Project grid {g} px"),
                                vec![ProjectSetting::GridSize(g)],
                            ),
                            _ => {
                                ps.error = Some(format!("Grid: {:?} isn't a size in px", s.trim()))
                            }
                        }
                    }
                    ui.weak("match your art's tile size");
                } else {
                    ui.label(format!("{} px", p.grid_size));
                    ui.weak("(fixed: the project has levels)").on_hover_text(
                        "Level sizes and positions are in cells of the grid, so it can only \
                         change while there are no levels",
                    );
                }
            });
            ui.end_row();
            ui.weak("Background");
            ui.horizontal(|ui| {
                let stored = p.bg_color.as_ref().map(|c| c.as_str());
                if let Some(hex) =
                    super::panels::color_field(ui, bufs, "project:bg", stored.or(Some("#1a1c2c")))
                    && let Ok(c) = Color::new(hex)
                {
                    set(
                        ps,
                        "Project background".into(),
                        vec![ProjectSetting::BgColor(Some(c))],
                    );
                }
                if p.bg_color.is_some() {
                    if ui
                        .small_button("×")
                        .on_hover_text("Back to the default")
                        .clicked()
                    {
                        set(
                            ps,
                            "Project background".into(),
                            vec![ProjectSetting::BgColor(None)],
                        );
                    }
                } else {
                    ui.weak("default");
                }
                ui.weak("behind every level without its own");
            });
            ui.end_row();
            ui.weak("Collision layer").on_hover_text(
                "The int_grid layer whose values block movement. The export lists each level's \
                 open edges from it",
            );
            collision_combo(ui, doc, ps);
            ui.end_row();
            if let Some(layer) = p
                .collision_layer
                .as_ref()
                .and_then(|c| p.layer(c.as_str()))
                .filter(|l| l.is_int_grid())
            {
                ui.weak("Blocking values").on_hover_text(
                    "A border cell holding anything else (empty included) is open in the export",
                );
                ui.horizontal_wrapped(|ui| {
                    let current = p.blocking_values.clone().unwrap_or_default();
                    for v in layer.values() {
                        let mut on = current.contains(&v.slug);
                        let r = ui.checkbox(&mut on, v.name.as_str());
                        mark(ui, &format!("project:blocking:{}", v.slug), r.rect);
                        if r.changed() {
                            let list = layer
                                .values()
                                .iter()
                                .filter(|x| {
                                    if x.slug == v.slug {
                                        on
                                    } else {
                                        current.contains(&x.slug)
                                    }
                                })
                                .map(|x| x.slug.clone())
                                .collect();
                            set(
                                ps,
                                "Blocking values".into(),
                                vec![ProjectSetting::BlockingValues(Some(list))],
                            );
                        }
                    }
                    if layer.values().is_empty() {
                        ui.weak("the layer has no values yet");
                    }
                });
                ui.end_row();
            }
            ui.weak("Seed")
                .on_hover_text("Mixed into every random choice the rules make");
            if let Some(s) = text_commit(
                ui,
                bufs,
                "project:seed",
                &p.seed.to_string(),
                false,
                false,
                160.0,
            ) {
                match s.trim().parse::<i64>() {
                    Ok(n) => set(ps, "Project seed".into(), vec![ProjectSetting::Seed(n)]),
                    Err(_) => ps.error = Some(format!("Seed: {:?} isn't a whole number", s.trim())),
                }
            }
            ui.end_row();
            ui.weak("New level size")
                .on_hover_text("Size in cells of a level made with New Level");
            ui.horizontal(|ui| {
                let shown = p
                    .default_level_size
                    .map_or(String::new(), |[w, h]| format!("{w}x{h}"));
                if let Some(s) =
                    text_commit(ui, bufs, "project:level_size", &shown, false, false, 60.0)
                {
                    match parse_level_size(&s) {
                        Ok(size) => set(
                            ps,
                            "New level size".into(),
                            vec![ProjectSetting::DefaultLevelSize(size)],
                        ),
                        Err(e) => ps.error = Some(format!("New level size: {e}")),
                    }
                }
                if p.default_level_size.is_none() {
                    let [w, h] = super::default_level_size(p);
                    ui.weak(format!("most common ({w}x{h})"));
                } else if ui
                    .small_button("×")
                    .on_hover_text("Back to the most common level size")
                    .clicked()
                {
                    set(
                        ps,
                        "New level size".into(),
                        vec![ProjectSetting::DefaultLevelSize(None)],
                    );
                }
            });
            ui.end_row();
            ui.weak("Export on save")
                .on_hover_text("Write export/ every time the project is saved");
            let mut on = p.export_on_save;
            let r = ui.checkbox(&mut on, "write export/ on every save");
            mark(ui, "project:export_on_save", r.rect);
            if r.changed() {
                set(
                    ps,
                    format!("Export on save {}", if on { "on" } else { "off" }),
                    vec![ProjectSetting::ExportOnSave(on)],
                );
            }
            ui.end_row();
        });
}

/// `"40x22"` (or `"40 22"`, `"40,22"`) as a level size; empty is none.
fn parse_level_size(s: &str) -> Result<Option<[u32; 2]>, String> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(None);
    }
    let parts: Vec<&str> = s
        .split(|c: char| c == 'x' || c == 'X' || c == ',' || c.is_whitespace())
        .filter(|p| !p.is_empty())
        .collect();
    match parts[..] {
        [w, h] => match (w.parse::<u32>(), h.parse::<u32>()) {
            (Ok(w), Ok(h)) if w > 0 && h > 0 => Ok(Some([w, h])),
            _ => Err(format!("{s:?} isn't a size in cells, like 40x22")),
        },
        _ => Err(format!("{s:?} isn't a size in cells, like 40x22")),
    }
}

/// The collision layer: none or an int_grid layer. Choosing one also sets
/// its blocking values (its `solid`, `fortified`, ... values, as loading
/// would; none of them: nothing blocks until you tick some).
fn collision_combo(ui: &mut egui::Ui, doc: &Document, ps: &mut ProjectState) {
    let p = &doc.project;
    let shown = p.collision_layer.as_ref().map_or("none", |s| s.as_str());
    egui::ComboBox::from_id_salt("collision layer")
        .selected_text(shown)
        .show_ui(ui, |ui| {
            if ui
                .selectable_label(p.collision_layer.is_none(), "none")
                .clicked()
                && p.collision_layer.is_some()
            {
                push(
                    &mut ps.pending,
                    "Collision layer: none",
                    vec![
                        Command::SetProjectSetting {
                            setting: ProjectSetting::BlockingValues(None),
                        },
                        Command::SetProjectSetting {
                            setting: ProjectSetting::CollisionLayer(None),
                        },
                    ],
                );
            }
            for l in p.layers.iter().filter(|l| l.is_int_grid()) {
                let on = p.collision_layer.as_ref() == Some(&l.slug);
                if ui.selectable_label(on, l.slug.as_str()).clicked() && !on {
                    let blocking: Vec<Slug> = l
                        .values()
                        .iter()
                        .filter(|v| worlds_core::DEFAULT_BLOCKING_VALUES.contains(&v.slug.as_str()))
                        .map(|v| v.slug.clone())
                        .collect();
                    push(
                        &mut ps.pending,
                        format!("Collision layer: {}", l.slug),
                        vec![
                            Command::SetProjectSetting {
                                setting: ProjectSetting::CollisionLayer(Some(l.slug.clone())),
                            },
                            Command::SetProjectSetting {
                                setting: ProjectSetting::BlockingValues(Some(blocking)),
                            },
                        ],
                    );
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testkit::{Driver, state};

    fn pane_frame(ui: &mut egui::Ui, st: &mut EditorState) {
        pane(ui, st);
    }

    /// The pane's queued batches: (label, commands).
    fn queued(st: &mut EditorState) -> Vec<(String, Vec<Command>)> {
        std::mem::take(&mut st.project.pending)
            .into_iter()
            .map(|c| match c {
                Command::Batch { label, commands } => (label, commands),
                other => (other.label(), vec![other]),
            })
            .collect()
    }

    /// Apply the pane's queued commands to the document as the app does
    /// (without the command layer: the project and the index only).
    fn apply(st: &mut EditorState) -> Result<(), String> {
        let doc = st.doc.as_mut().unwrap();
        let mut ok = Ok(());
        for c in std::mem::take(&mut st.project.pending) {
            if let Err(e) = doc.project.apply(c) {
                ok = Err(e.to_string());
            }
        }
        doc.reindex();
        if let Some(s) = st.project.applied(ok.is_ok()) {
            st.view.sync_layers(doc, None);
            st.view.active_layer = doc.project.layers.iter().position(|l| l.slug == s);
        }
        ok
    }

    #[test]
    fn slugs_and_titles() {
        assert_eq!(slugify("My tiles-2", "tileset"), "my_tiles_2");
        assert_eq!(slugify("2bit", "tileset"), "tileset_2bit");
        assert_eq!(slugify("--", "tileset"), "tileset");
        assert_eq!(title("decor_back"), "Decor back");
        assert_eq!(Section::parse("tilesets"), Some(Section::Tilesets));
    }

    #[test]
    fn new_layers_get_useful_defaults() {
        let (_tmp, st) = state();
        let doc = st.doc.as_ref().unwrap();
        let (cmd, slug) = new_layer(doc, "int_grid").unwrap();
        // mini has a `collision` layer already.
        assert_eq!(slug.as_str(), "grid");
        let Command::Batch { commands, .. } = &cmd else {
            panic!()
        };
        let Command::SetLayerDef { def, index, .. } = &commands[0] else {
            panic!()
        };
        assert_eq!(*index, None, "on top");
        assert_eq!(def.values().len(), 1);
        assert_eq!(def.values()[0].glyph, '#');
        let (_, slug) = new_layer(doc, "auto").unwrap();
        assert_eq!(slug.as_str(), "walls");
        assert!(new_layer(doc, "nope").is_err());
        // With no int_grid layer an auto layer is refused, with the reason.
        let mut empty = worlds_core::Project::new("fresh");
        let dir = tempfile::tempdir().unwrap();
        worlds_core::save(&mut empty, dir.path()).unwrap();
        let fresh = Document::open(dir.path()).unwrap();
        let e = new_layer(&fresh, "auto").unwrap_err();
        assert!(e.contains("int_grid"), "{e}");
        assert_eq!(
            new_layer(&fresh, "int_grid").unwrap().1.as_str(),
            "collision"
        );
    }

    #[test]
    fn add_a_layer_and_a_value_then_remove_the_layer() {
        let (_tmp, mut st) = state();
        st.project.show(Section::Layers);
        let mut d = Driver::new(pane_frame);
        d.frame(&mut st, vec![]);
        let add = st.project.rects["add layer"];
        d.click(&mut st, add.center());
        // The menu: "Int grid" is its first item, under the button.
        d.frame(&mut st, vec![]);
        let item = egui::pos2(add.min.x + 30.0, add.max.y + 14.0);
        d.click(&mut st, item);
        let q = queued(&mut st);
        assert_eq!(q.len(), 1, "{q:?}");
        assert_eq!(q[0].0, "New int_grid layer grid");
        st.project.pending = q
            .into_iter()
            .map(|(label, commands)| Command::Batch { label, commands })
            .collect();
        apply(&mut st).unwrap();
        assert_eq!(st.project.layer.as_ref().unwrap().as_str(), "grid");
        let doc = st.doc.as_ref().unwrap();
        assert_eq!(doc.project.layers.last().unwrap().slug.as_str(), "grid");
        assert_eq!(
            st.view.active_layer,
            Some(doc.project.layers.len() - 1),
            "the new layer is active"
        );
        // + Value adds a second value with a free glyph.
        d.frame(&mut st, vec![]);
        d.frame(&mut st, vec![]);
        let r = d.rect("layer:grid:add value");
        d.click(&mut st, r.center());
        let q = queued(&mut st);
        assert_eq!(q[0].0, "Edit layer grid: add value");
        let Command::SetLayerDef { def, .. } = &q[0].1[0] else {
            panic!()
        };
        assert_eq!(def.values().len(), 2);
        assert_eq!(def.values()[1].slug.as_str(), "value");
        assert_eq!(def.values()[1].glyph, '-');
        // × on a layer asks first, then removes it.
        d.frame(&mut st, vec![]);
        let x = st.project.rects["layer:grid:remove"];
        d.click(&mut st, x.center());
        assert!(st.project.pending.is_empty());
        d.frame(&mut st, vec![]);
        let yes = st.project.rects["confirm delete"];
        d.click(&mut st, yes.center());
        let q = queued(&mut st);
        assert_eq!(q[0].0, "Delete layer grid");
        assert!(matches!(q[0].1[0], Command::RemoveLayerDef { .. }));
    }

    #[test]
    fn reorder_and_refusals_are_shown() {
        let (_tmp, mut st) = state();
        st.project.show(Section::Layers);
        let mut d = Driver::new(pane_frame);
        d.frame(&mut st, vec![]);
        // ↓ on the top layer moves it one down (index n - 2).
        let n = st.doc.as_ref().unwrap().project.layers.len();
        let top = st.doc.as_ref().unwrap().project.layers[n - 1].slug.clone();
        let r = st.project.rects[&format!("layer:{top}:down")];
        d.click(&mut st, r.center());
        let q = queued(&mut st);
        let Command::SetLayerDef { index, .. } = &q[0].1[0] else {
            panic!()
        };
        assert_eq!(*index, Some(n - 2));
        // Removing a value cells hold is refused; the message comes back.
        st.project.show_layer(Slug::new("collision").unwrap());
        d.frame(&mut st, vec![]);
        d.frame(&mut st, vec![]);
        let x = d.rect("layer:collision:value:0:remove");
        d.click(&mut st, x.center());
        let e = apply(&mut st).unwrap_err();
        assert!(e.contains("solid"), "{e}");
    }

    #[test]
    fn blocking_values_toggle_and_collision_layer() {
        let (_tmp, mut st) = state();
        st.project.show(Section::Settings);
        let mut d = Driver::new(pane_frame);
        d.frame(&mut st, vec![]);
        d.frame(&mut st, vec![]);
        // mini: collision, blocking solid, fortified, ramp_l (loaded).
        let r = d.rect("project:blocking:oneway");
        d.click(&mut st, r.center());
        let q = queued(&mut st);
        assert_eq!(q.len(), 1);
        let Command::SetProjectSetting {
            setting: ProjectSetting::BlockingValues(Some(list)),
        } = &q[0].1[0]
        else {
            panic!("{q:?}")
        };
        let list: Vec<&str> = list.iter().map(|s| s.as_str()).collect();
        assert_eq!(list, ["solid", "fortified", "oneway", "ramp_l"]);
        // Grid size is fixed with levels.
        d.frame(&mut st, vec![]);
        assert!(super::super::fields::marked(&d.ctx, "project:grid").is_none());
    }

    #[test]
    fn export_on_save_and_new_level_size() {
        let (_tmp, mut st) = state();
        st.project.show(Section::Settings);
        let mut d = Driver::new(pane_frame);
        d.frame(&mut st, vec![]);
        d.frame(&mut st, vec![]);
        let r = d.rect("project:export_on_save");
        d.click(&mut st, r.center());
        let q = queued(&mut st);
        assert_eq!(
            q,
            vec![(
                "Export on save on".to_string(),
                vec![Command::SetProjectSetting {
                    setting: ProjectSetting::ExportOnSave(true)
                }]
            )]
        );
        assert!(super::super::fields::marked(&d.ctx, "project:level_size").is_some());
        assert_eq!(parse_level_size(" 40x22 "), Ok(Some([40, 22])));
        assert_eq!(parse_level_size("40, 22"), Ok(Some([40, 22])));
        assert_eq!(parse_level_size(""), Ok(None));
        assert!(parse_level_size("0x5").is_err());
        assert!(parse_level_size("40").is_err());
        assert!(parse_level_size("ax5").is_err());
    }

    #[test]
    fn a_tileset_from_outside_is_copied_into_art() {
        let (_tmp, st) = state();
        let doc = st.doc.as_ref().unwrap();
        let other = tempfile::tempdir().unwrap();
        let png = other.path().join("My Tiles.png");
        image::RgbaImage::from_pixel(64, 32, image::Rgba([200, 100, 50, 255]))
            .save(&png)
            .unwrap();
        let (cmd, slug, copied) = tileset_from_file(doc, &png).unwrap();
        assert_eq!(slug.as_str(), "my_tiles");
        assert!(copied.unwrap().contains("art/My Tiles.png"));
        assert!(doc.dir.join("art/My Tiles.png").exists());
        let Command::Batch { commands, .. } = cmd else {
            panic!()
        };
        let Command::SetTilesetDef { def, .. } = &commands[0] else {
            panic!()
        };
        assert_eq!(def.image, "art/My Tiles.png");
        assert_eq!(def.tile_size, [16, 16]);
        // Again: the same file is reused, nothing copied.
        let (_, _, copied) = tileset_from_file(doc, &png).unwrap();
        assert!(copied.is_none());
        // A different file of the same name gets a new one.
        image::RgbaImage::from_pixel(64, 32, image::Rgba([1, 2, 3, 255]))
            .save(&png)
            .unwrap();
        let (cmd, _, _) = tileset_from_file(doc, &png).unwrap();
        let Command::Batch { commands, .. } = cmd else {
            panic!()
        };
        let Command::SetTilesetDef { def, .. } = &commands[0] else {
            panic!()
        };
        assert_eq!(def.image, "art/My Tiles_2.png");
        // Inside the project: the relative path, no copy.
        let (rel, copied) = project_image(&doc.dir, &doc.dir.join("art/My Tiles.png")).unwrap();
        assert_eq!(rel, "art/My Tiles.png");
        assert!(copied.is_none());
        // Not an image: refused.
        let txt = other.path().join("notes.txt");
        std::fs::write(&txt, "hi").unwrap();
        assert!(project_image(&doc.dir, &txt).is_err());
    }
}
