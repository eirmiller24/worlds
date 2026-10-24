//! The editor shell: toolbar, dock layout, status bar, command palette,
//! toasts, project lifecycle (open, reload on change, tileset hot reload,
//! save, the unsaved-changes prompts) and the per-project UI file
//! `.worlds/ui.json`.
//!
//! [`EditorApp::frame`] draws one frame into a root `Ui`. The window
//! (`eframe::App`) and the headless screenshot path both call it.

pub mod actions;
pub mod agent;
mod assembly_ui;
pub mod defs;
pub mod entities;
pub mod fields;
pub mod find;
pub mod history;
pub mod import;
pub mod inspector;
mod layer_actions;
pub mod levels;
pub mod note_inspector;
pub mod notes;
pub mod panels;
pub mod problems;
pub mod project_pane;
pub mod recovery;
pub mod references;
pub mod rules;
pub mod shape_inspector;
#[cfg(test)]
pub(crate) mod testkit;
pub mod theme;
pub mod viewports;
pub mod watch;
pub mod worlds;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use egui::{Color32, RichText};
use egui_dock::{DockArea, DockState, NodeIndex, TabViewer};
use serde::{Deserialize, Serialize};

use crate::doc::{Document, synthesize};
use crate::log::LogBuffer;
use crate::tools::{
    CommandContext, EditorCommands, EntityBrush, EntityPalette, Invalidation, NoCommands,
    PaletteSelection, PaletteView, Tool, ToolButton, ToolEvent,
};
use crate::view::{LayerView, ViewState, ViewportId};
use crate::viewport::camera::Camera;
use crate::viewport::gpu::Gpu;
use crate::viewport::{Fit, Viewport, ViewportCx};
use actions::{Action, fuzzy_score};
use watch::{OwnWrites, ProjectWatcher, WatchChange};
use worlds_core::{Command, Level, Slug};

/// Dock tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Pane {
    /// A map tab ([`viewports`]); a `ui.json` from before map tabs has a
    /// bare `"Viewport"`, read as tab 1 ([`legacy_dock`]).
    Viewport(ViewportId),
    Layers,
    Palette,
    Inspector,
    Problems,
    Log,
    /// The rule editor for an auto layer ([`rules`]).
    Rules,
    /// Entity types and level fields ([`defs`]), tab "Types".
    EntityDefs,
    /// Annotations ([`notes`]): list, filters, tags, guide groups.
    Notes,
    /// Layers, tilesets and project settings ([`project_pane`]), tab
    /// "Project".
    Project,
    /// Project-wide find and replace ([`find`]), tab "Find".
    Find,
    /// The undo log ([`history`]): click an entry to go back to it.
    History,
    /// The levels of the world shown ([`levels`]): thumbnails, search,
    /// sort, drag to reorder. Tab "Levels", next to the Layers pane.
    Levels,
}

/// Default layout. Split fractions are the share of the first (left or
/// top) child. The bottom node holds the Log and the Rules pane as tabs;
/// the Types and Project panes are tabs next to the Inspector, Notes and
/// Find next to the Problems.
pub fn default_dock() -> DockState<Pane> {
    let mut dock = DockState::new(vec![Pane::Viewport(ViewportId::FIRST)]);
    let s = dock.main_surface_mut();
    let [center, _left] = s.split_left(NodeIndex::root(), 0.2, vec![Pane::Layers, Pane::Levels]);
    let [_layers, _palette] = s.split_below(_left, 0.45, vec![Pane::Palette]);
    let [center, right] = s.split_right(
        center,
        0.72,
        vec![Pane::Inspector, Pane::EntityDefs, Pane::Project],
    );
    let [_inspector, _problems] =
        s.split_below(right, 0.7, vec![Pane::Problems, Pane::Notes, Pane::Find]);
    let [_center, _log] = s.split_below(center, 0.62, vec![Pane::Log, Pane::Rules, Pane::History]);
    dock
}

/// Give a layout saved before the Rules pane existed one: a tab next to the
/// Log, or (no Log) a bottom split of the viewport.
pub fn add_rules_pane(dock: &mut DockState<Pane>) {
    if dock.find_tab(&Pane::Rules).is_some() {
        return;
    }
    if let Some(p) = dock.find_tab(&Pane::Log)
        && let Ok(leaf) = dock.leaf_mut(p.node_path())
    {
        leaf.append_tab(Pane::Rules);
        return;
    }
    if let Some((node, _)) = main_viewport(dock) {
        dock.main_surface_mut()
            .split_below(node, 0.62, vec![Pane::Rules]);
    }
}

/// Give a layout saved before the Types pane existed one: a tab next to
/// the Inspector, else next to the Problems, else a split right of the map.
pub fn add_types_pane(dock: &mut DockState<Pane>) {
    if dock.find_tab(&Pane::EntityDefs).is_some() {
        return;
    }
    for next_to in [Pane::Inspector, Pane::Problems] {
        if let Some(p) = dock.find_tab(&next_to)
            && let Ok(leaf) = dock.leaf_mut(p.node_path())
        {
            leaf.append_tab(Pane::EntityDefs);
            return;
        }
    }
    if let Some((node, _)) = main_viewport(dock) {
        dock.main_surface_mut()
            .split_right(node, 0.75, vec![Pane::EntityDefs]);
    }
}

/// Give a layout saved before the Notes pane existed one: a tab next to
/// the Problems, else next to the Inspector, else a split right of the map.
pub fn add_notes_pane(dock: &mut DockState<Pane>) {
    if dock.find_tab(&Pane::Notes).is_some() {
        return;
    }
    for next_to in [Pane::Problems, Pane::Inspector] {
        if let Some(p) = dock.find_tab(&next_to)
            && let Ok(leaf) = dock.leaf_mut(p.node_path())
        {
            leaf.append_tab(Pane::Notes);
            return;
        }
    }
    if let Some((node, _)) = main_viewport(dock) {
        dock.main_surface_mut()
            .split_right(node, 0.75, vec![Pane::Notes]);
    }
}

/// Give a layout saved before the Project pane existed one: a tab next to
/// the Types pane, else the Inspector, else a split right of the map.
pub fn add_project_pane(dock: &mut DockState<Pane>) {
    if dock.find_tab(&Pane::Project).is_some() {
        return;
    }
    for next_to in [Pane::EntityDefs, Pane::Inspector] {
        if let Some(p) = dock.find_tab(&next_to)
            && let Ok(leaf) = dock.leaf_mut(p.node_path())
        {
            leaf.append_tab(Pane::Project);
            return;
        }
    }
    if let Some((node, _)) = main_viewport(dock) {
        dock.main_surface_mut()
            .split_right(node, 0.75, vec![Pane::Project]);
    }
}

/// Give a layout saved before the Find pane existed one: a tab next to the
/// Notes, else the Problems, else the Inspector, else a split right of the
/// map.
pub fn add_find_pane(dock: &mut DockState<Pane>) {
    if dock.find_tab(&Pane::Find).is_some() {
        return;
    }
    for next_to in [Pane::Notes, Pane::Problems, Pane::Inspector] {
        if let Some(p) = dock.find_tab(&next_to)
            && let Ok(leaf) = dock.leaf_mut(p.node_path())
        {
            leaf.append_tab(Pane::Find);
            return;
        }
    }
    if let Some((node, _)) = main_viewport(dock) {
        dock.main_surface_mut()
            .split_right(node, 0.75, vec![Pane::Find]);
    }
}

/// Give a layout saved before the History pane existed one: a tab next to
/// the Log, else next to the Rules pane, else a bottom split of the map.
pub fn add_history_pane(dock: &mut DockState<Pane>) {
    if dock.find_tab(&Pane::History).is_some() {
        return;
    }
    for next_to in [Pane::Log, Pane::Rules] {
        if let Some(p) = dock.find_tab(&next_to)
            && let Ok(leaf) = dock.leaf_mut(p.node_path())
        {
            leaf.append_tab(Pane::History);
            return;
        }
    }
    if let Some((node, _)) = main_viewport(dock) {
        dock.main_surface_mut()
            .split_below(node, 0.62, vec![Pane::History]);
    }
}

/// The first map tab on the main surface.
fn main_viewport(dock: &DockState<Pane>) -> Option<(NodeIndex, egui_dock::TabIndex)> {
    dock.main_surface()
        .find_tab_from(|t| matches!(t, Pane::Viewport(_)))
}

/// A `ui.json` dock from before map tabs had ids: every bare
/// `"Viewport"` tab becomes `{ "Viewport": 1 }`.
fn legacy_dock(v: &mut serde_json::Value) {
    match v {
        serde_json::Value::String(s) if s == "Viewport" => {
            *v = serde_json::json!({ "Viewport": ViewportId::FIRST.0 });
        }
        serde_json::Value::Array(a) => a.iter_mut().for_each(legacy_dock),
        serde_json::Value::Object(o) => o.values_mut().for_each(legacy_dock),
        _ => {}
    }
}

/// Read a `ui.json` (an old dock's bare `"Viewport"` tabs included).
fn parse_ui_file(text: &str) -> serde_json::Result<UiFile> {
    let mut v: serde_json::Value = serde_json::from_str(text)?;
    if let Some(d) = v.get_mut("dock") {
        legacy_dock(d);
    }
    serde_json::from_value(v)
}

/// Give a layout saved before the Levels pane existed one: a tab next to
/// the Layers pane, else the Palette, else a split left of the map.
pub fn add_levels_pane(dock: &mut DockState<Pane>) {
    if dock.find_tab(&Pane::Levels).is_some() {
        return;
    }
    for next_to in [Pane::Layers, Pane::Palette] {
        if let Some(p) = dock.find_tab(&next_to)
            && let Ok(leaf) = dock.leaf_mut(p.node_path())
        {
            leaf.append_tab(Pane::Levels);
            return;
        }
    }
    if let Some((node, _)) = main_viewport(dock) {
        dock.main_surface_mut()
            .split_left(node, 0.2, vec![Pane::Levels]);
    }
}

/// Bring `pane` to the front of its tab group.
pub fn show_pane(dock: &mut DockState<Pane>, pane: Pane) {
    if let Some(p) = dock.find_tab(&pane) {
        let _ = dock.set_active_tab(p);
    }
}

/// `.worlds/ui.json`.
#[derive(Serialize, Deserialize)]
struct UiFile {
    dock: DockState<Pane>,
    /// The focused map tab's camera (what an older editor reads; see
    /// `viewports`).
    #[serde(default)]
    camera: Camera,
    #[serde(default)]
    active_level: Option<String>,
    #[serde(default)]
    active_layer: Option<String>,
    #[serde(default)]
    layers: BTreeMap<String, LayerView>,
    #[serde(default)]
    show_grid: bool,
    #[serde(default)]
    tool: Tool,
    /// The Paint group's remembered mode (`Brush`, `Line`, `Rect`, `Fill`).
    #[serde(default = "default_paint_mode")]
    paint_mode: Tool,
    #[serde(default)]
    palette: PaletteSelection,
    #[serde(default)]
    entity_palette: UiPalette,
    /// Annotation visibility and guide options.
    #[serde(default)]
    annotations: crate::view::AnnotationsView,
    /// The Note tool's options strip.
    #[serde(default)]
    note_tool: crate::view::NoteToolOptions,
    /// The world the map shows (by slug).
    #[serde(default)]
    active_world: Option<String>,
    /// The References pseudo-layer's eye and lock.
    #[serde(default)]
    references: crate::viewport::references::RefsUi,
    /// Every map tab. A file without them (from before map tabs) opens as
    /// one tab holding `camera`, `active_world` and `active_level`.
    #[serde(default)]
    viewports: Vec<UiViewport>,
    /// The focused map tab.
    #[serde(default)]
    focused_viewport: Option<ViewportId>,
    /// Two map tabs whose cameras are linked.
    #[serde(default)]
    linked_viewports: Option<[ViewportId; 2]>,
    /// Camera bookmarks, in order.
    #[serde(default)]
    bookmarks: Vec<crate::view::Bookmark>,
    /// The Palette's Library: shown or not, favorites, collapsed parts.
    #[serde(default)]
    library: crate::library::LibraryView,
}

/// The map tabs of `f`: one per map tab in the dock (`docked`), with the
/// saved world, camera and active level (an old file without `viewports`:
/// its single camera, world and level for tab 1; a tab the file doesn't
/// know: the first world, fitted), parked; and the focused one.
fn ui_viewports(
    doc: &Document,
    docked: &[ViewportId],
    f: &UiFile,
) -> (Vec<crate::view::ViewportTab>, ViewportId) {
    let world_slug = |s: &Option<String>| {
        s.as_ref()
            .and_then(|s| Slug::new(s).ok())
            .filter(|s| doc.project.world(s.as_str()).is_some())
    };
    let legacy = [UiViewport {
        id: ViewportId::FIRST,
        world: f.active_world.clone(),
        camera: f.camera,
        active_level: f.active_level.clone(),
    }];
    let saved: &[UiViewport] = if f.viewports.is_empty() {
        &legacy
    } else {
        &f.viewports
    };
    let mut tabs = Vec::new();
    for id in docked {
        let mut t = crate::view::ViewportTab::new(*id);
        match saved.iter().find(|v| v.id == *id) {
            Some(v) => {
                t.active_world = world_slug(&v.world);
                t.active_level = v.active_level.as_deref().and_then(|s| doc.level_ix(s));
                t.zoom = v.camera.zoom;
                t.cam = crate::viewport::TabCam::new(v.camera, None);
            }
            None => t.cam.request_fit(Fit::World),
        }
        tabs.push(t);
    }
    let focused = f
        .focused_viewport
        .filter(|id| docked.contains(id))
        .unwrap_or(docked[0]);
    (tabs, focused)
}

/// `ui.json`'s `viewports`: one map tab.
#[derive(Serialize, Deserialize)]
struct UiViewport {
    id: ViewportId,
    #[serde(default)]
    world: Option<String>,
    #[serde(default)]
    camera: Camera,
    #[serde(default)]
    active_level: Option<String>,
}

fn default_paint_mode() -> Tool {
    Tool::Brush
}

/// `ui.json`'s `entity_palette`: the entity palette's remembered state.
/// Every key is optional, so older files load; slugs the project doesn't
/// have (any more) are dropped on restore.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiPalette {
    pub favorites: Vec<String>,
    pub recent: Vec<String>,
    pub collapsed: Vec<String>,
    pub chips: Vec<String>,
    pub view: PaletteView,
    pub brush: Option<String>,
}

impl UiPalette {
    pub fn from_palette(p: &EntityPalette) -> Self {
        let strs = |v: &[Slug]| v.iter().map(|s| s.to_string()).collect();
        UiPalette {
            favorites: strs(&p.favorites),
            recent: strs(&p.recent),
            collapsed: p.collapsed.iter().cloned().collect(),
            chips: p.chips.clone(),
            view: p.view,
            brush: p.brush.as_ref().map(|b| b.ty.to_string()),
        }
    }

    /// The palette state for `project`: unknown or malformed slugs dropped.
    pub fn to_palette(&self, project: &worlds_core::Project) -> EntityPalette {
        let slugs = |v: &[String]| {
            v.iter()
                .filter_map(|s| Slug::new(s).ok())
                .collect::<Vec<_>>()
        };
        let mut p = EntityPalette {
            favorites: slugs(&self.favorites),
            recent: slugs(&self.recent),
            collapsed: self.collapsed.iter().cloned().collect(),
            chips: self.chips.clone(),
            view: self.view,
            brush: self
                .brush
                .as_deref()
                .and_then(|s| Slug::new(s).ok())
                .map(EntityBrush::of),
        };
        p.retain_known(project);
        p
    }
}

pub struct Toast {
    /// Unique per toast (its egui area id).
    pub id: u64,
    pub text: String,
    pub error: bool,
    pub until: Instant,
    /// An agent batch's toast: its one-click Undo.
    pub undo: Option<agent::AgentUndo>,
}

/// Everything the panels read and write.
pub struct EditorState {
    pub doc: Option<Document>,
    pub view: ViewState,
    pub viewport: Viewport,
    pub log: LogBuffer,
    pub log_level: tracing::Level,
    pub toasts: Vec<Toast>,
    /// The next [`Toast::id`].
    pub next_toast: u64,
    /// Actions queued by panels this frame.
    pub actions: Vec<Action>,
    /// Palette atlas textures by tileset slug (`None`: failed to load).
    pub atlas: HashMap<String, Option<egui::TextureHandle>>,
    /// Palette tileset tab per tiles layer slug.
    pub palette_tab: HashMap<String, String>,
    pub atlas_drag: Option<[u32; 2]>,
    /// The random brush as it was when the current atlas drag started (a
    /// Ctrl or Shift drag adds to it).
    pub random_base: Option<crate::tools::Brush>,
    pub tool_events: Vec<ToolEvent>,
    /// Commands queued by panels this frame (inspector edits), applied as
    /// one undo entry each.
    pub pending: Vec<Command>,
    /// Inspector text being edited, by field key, until it is committed.
    pub edit_bufs: HashMap<String, String>,
    pub accent: Color32,
    /// Approval mode: the pending agent batches' ghosts.
    pub ghosts: Vec<crate::ghost::Ghost>,
    /// Where the Map pane was drawn last frame (screen points): the
    /// approval panel sits inside it, over the map, not over other panes.
    pub map_rect: Option<egui::Rect>,
    /// The Rules pane.
    pub rules: rules::RulesState,
    /// The entity palette (what isn't saved; see `view.entities`).
    pub entities: entities::PaletteState,
    /// The Types pane.
    pub types: defs::TypesState,
    /// The Notes pane.
    pub notes: notes::NotesState,
    /// The Project pane.
    pub project: project_pane::ProjectState,
    /// Where each pane drew last frame (screen points; tests).
    pub pane_rects: HashMap<Pane, egui::Rect>,
    /// The Find pane.
    pub find: find::FindState,
    /// The History pane.
    pub history: history::HistoryState,
    /// "Add reference image…": the file dialog, while it is open.
    pub ref_picker: Option<references::Picker>,
    /// The bookmark window is open (the toolbar's ★).
    pub bookmarks_open: bool,
    /// The bookmark list's name field.
    pub bookmark_name: String,
    /// Focus that name field next frame.
    pub bookmark_focus: bool,
    /// The Library (assemblies) and the Save as assembly dialog.
    pub library: crate::library::LibraryState,
    /// The Levels pane.
    pub levels: levels::LevelsState,
}

impl Default for EditorState {
    fn default() -> Self {
        EditorState {
            doc: None,
            view: ViewState::new(),
            viewport: Viewport::new(),
            log: LogBuffer::default(),
            log_level: tracing::Level::INFO,
            toasts: Vec::new(),
            next_toast: 0,
            actions: Vec::new(),
            atlas: HashMap::new(),
            palette_tab: HashMap::new(),
            atlas_drag: None,
            random_base: None,
            tool_events: Vec::new(),
            pending: Vec::new(),
            edit_bufs: HashMap::new(),
            accent: theme::ACCENT,
            ghosts: Vec::new(),
            map_rect: None,
            rules: rules::RulesState::default(),
            entities: entities::PaletteState::default(),
            types: defs::TypesState::default(),
            notes: notes::NotesState::default(),
            project: project_pane::ProjectState::default(),
            pane_rects: HashMap::new(),
            find: find::FindState::default(),
            history: history::HistoryState::default(),
            ref_picker: None,
            bookmarks_open: false,
            bookmark_name: String::new(),
            bookmark_focus: false,
            library: crate::library::LibraryState::default(),
            levels: levels::LevelsState::default(),
        }
    }
}

impl EditorState {
    pub fn toast(&mut self, text: impl Into<String>, error: bool) {
        let text = text.into();
        if error {
            tracing::error!("{text}");
        } else {
            tracing::info!("{text}");
        }
        let until = Instant::now() + Duration::from_secs(if error { 8 } else { 4 });
        self.push_toast(text, error, until, None);
    }

    /// Add a toast. At most [`MAX_TOASTS`] are kept; the oldest go first.
    pub fn push_toast(
        &mut self,
        text: String,
        error: bool,
        until: Instant,
        undo: Option<agent::AgentUndo>,
    ) {
        self.next_toast += 1;
        self.toasts.push(Toast {
            id: self.next_toast,
            text,
            error,
            until,
            undo,
        });
        if self.toasts.len() > MAX_TOASTS {
            self.toasts.drain(..self.toasts.len() - MAX_TOASTS);
        }
    }
}

/// Toasts kept at once (older ones are dropped).
pub const MAX_TOASTS: usize = 32;
/// Toasts drawn at once, newest at the bottom; older ones collapse into a
/// "+N more" line above them.
pub const SHOWN_TOASTS: usize = 4;

#[derive(Debug, Clone, Default)]
pub struct AppOptions {
    /// Render frames without a window: no watcher, no ui.json, synchronous
    /// bakes, no rebuild budget.
    pub headless: bool,
    /// Grow the project to this many levels after loading (perf testing).
    pub synthetic: Option<usize>,
    /// Add this many synthetic entity types (and instances) after loading
    /// (perf testing; [`crate::doc::synthesize_entities`]).
    pub synthetic_entities: Option<usize>,
    /// Add N world pins, ⌈N/4⌉ notes per level and N/10 world paths
    /// ([`crate::doc::synthesize_notes`]).
    pub synthetic_notes: Option<usize>,
    /// Add N guide polylines ([`crate::doc::synthesize_guides`]).
    pub synthetic_guides: Option<usize>,
    /// Make this level active and frame it on open.
    pub level: Option<String>,
    /// Host the live API (`worlds-server`) for the open project. The window
    /// always does; `--screenshot` only with `--serve`.
    pub serve: bool,
    /// This session edits the project: take the session lock
    /// (`.worlds/editor.lock`) and deal with a previous session's autosave.
    /// The window always does; headless with `--serve`, `--save`,
    /// `--recover` or `--autosave`.
    pub edits: bool,
    /// Autosave interval (`None`: off); see [`crate::autosave::interval`].
    pub autosave: Option<Duration>,
    /// `--recover`: what to do with a previous session's autosave without
    /// asking.
    pub recover: Option<recovery::RecoverMode>,
    /// `--recover-conflicts`: `apply` recovers conflicting files too.
    pub recover_conflicts: bool,
    /// `--recover-report <file>`: write what was found as JSON.
    pub recover_report: Option<PathBuf>,
}

impl AppOptions {
    /// A generated (`--synthetic`, `--synthetic-entities`) project: saving
    /// and reloading are refused.
    pub fn is_synthetic(&self) -> bool {
        self.synthetic.is_some()
            || self.synthetic_entities.is_some()
            || self.synthetic_notes.is_some()
            || self.synthetic_guides.is_some()
    }
}

/// The "save before opening another project" prompt's buttons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenChoice {
    Save,
    Discard,
    Cancel,
}

/// The New Level dialog.
struct NewLevelDialog {
    world: Slug,
    at: [i64; 2],
    slug: String,
    error: Option<String>,
    just_opened: bool,
}

/// A question waiting in the corner.
enum Confirm {
    DeleteLevel {
        world: Slug,
        level: Slug,
    },
    /// Delete with levels selected: the whole selection's batch.
    DeleteSelection {
        summary: String,
        label: String,
        commands: Vec<Command>,
    },
    /// Delete a world and what it holds (`summary`: " and its 3 levels").
    RemoveWorld {
        world: Slug,
        summary: String,
    },
}

impl Confirm {
    /// What the prompt asks and the command a yes applies.
    fn question_and_command(&self) -> (String, Command) {
        match self {
            Confirm::DeleteLevel { world, level } => (
                format!("Delete level {level}? Undo brings it back."),
                Command::DeleteLevel {
                    world: world.clone(),
                    level: level.clone(),
                    keep_layout: false,
                },
            ),
            Confirm::DeleteSelection {
                summary,
                label,
                commands,
            } => (
                format!("Delete {summary}? Undo brings them back."),
                Command::Batch {
                    label: label.clone(),
                    commands: commands.clone(),
                },
            ),
            Confirm::RemoveWorld { world, summary } => (
                format!("Delete world {world}{summary}? Undo brings it back."),
                Command::RemoveWorld {
                    world: world.clone(),
                    confirm: true,
                },
            ),
        }
    }
}

/// The modal prompts, topmost first: only the topmost one gets Enter and
/// Escape (see [`modal_keys`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Modal {
    Close,
    Open,
    Reload,
    Confirm,
    NewLevel,
    Recovery,
    World,
    Import,
}

/// Enter and Escape taken from this frame's input for the topmost modal.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct ModalKeys {
    modal: Option<Modal>,
    enter: bool,
    escape: bool,
}

impl ModalKeys {
    /// The keys, if `m` is the modal they were taken for.
    fn for_modal(self, m: Modal) -> (bool, bool) {
        if self.modal == Some(m) {
            (self.enter, self.escape)
        } else {
            (false, false)
        }
    }
}

/// Take Enter and Escape out of the input for the topmost open modal, before
/// anything else (shortcuts, the viewport's Escape-cancels-gesture, panels)
/// sees them. Escape always goes to the modal. Enter goes to it only when no
/// widget has keyboard focus: a focused text field or button keeps its Enter
/// (committing a rename in the inspector never answers a prompt).
fn modal_keys(ctx: &egui::Context, modal: Option<Modal>) -> ModalKeys {
    use egui::{Key, Modifiers};
    let Some(m) = modal else {
        return ModalKeys::default();
    };
    let focused = ctx.egui_wants_keyboard_input();
    ctx.input_mut(|i| ModalKeys {
        modal: Some(m),
        escape: i.consume_key(Modifiers::NONE, Key::Escape),
        enter: !focused && i.consume_key(Modifiers::NONE, Key::Enter),
    })
}

#[derive(Default)]
struct CommandPalette {
    open: bool,
    query: String,
    selected: usize,
    just_opened: bool,
}

pub struct EditorApp {
    pub st: EditorState,
    dock: DockState<Pane>,
    gpu: Gpu,
    ctx: egui::Context,
    commands: Box<dyn EditorCommands>,
    watcher: Option<ProjectWatcher>,
    /// What our saves wrote, to recognize their watcher events.
    own_writes: OwnWrites,
    palette: CommandPalette,
    reload_prompt: bool,
    /// The window was asked to close with unsaved edits.
    close_prompt: bool,
    /// File > Open picked this project while there are unsaved edits.
    open_prompt: Option<PathBuf>,
    /// Close without asking (after save or discard in the prompt).
    allow_close: bool,
    /// Raised by SIGINT/SIGTERM ([`crate::signals`]): stop the server and
    /// close.
    terminate: Option<Arc<AtomicBool>>,
    /// The toolbar's tools as wide as they were drawn last frame (0 before
    /// the first): what the agent indicator leaves them.
    tools_w: f32,
    /// See [`Self::toolbar_split`].
    toolbar_split: Option<(egui::Rect, egui::Rect)>,
    /// Where toasts may go (between the toolbar and the status bar, screen
    /// points), as of this frame's panels.
    toast_bounds: Option<egui::Rect>,
    /// See [`Self::toast_rects`].
    toast_rects: Vec<egui::Rect>,
    new_level: Option<NewLevelDialog>,
    confirm: Option<Confirm>,
    /// This frame's Enter/Escape for the topmost modal.
    modal_keys: ModalKeys,
    title: String,
    opts: AppOptions,
    last_ui_save: Instant,
    last_ui_json: String,
    invalidations: Vec<Invalidation>,
    /// CPU time of the last frame's `frame()` call, smoothed.
    pub frame_ms: f32,
    dialog: Option<std::sync::mpsc::Receiver<Option<PathBuf>>>,
    /// Set by `open`, cleared (and logged) at the end of the next frame.
    opened_at: Option<Instant>,
    /// The live API.
    live: agent::Live,
    /// The active layer last frame: switching to an auto layer brings the
    /// Rules pane to the front.
    last_active_layer: Option<usize>,
    /// Entity definition files that changed on disk, waiting for the drain
    /// point ([`Self::apply_disk_entities`]).
    disk_entities: Vec<PathBuf>,
    /// Live validation ([`problems`]).
    problems: problems::LiveProblems,
    /// The session lock of the open project (`opts.edits`).
    lock: Option<worlds_core::autosave::EditorLock>,
    /// Autosave ([`crate::autosave`]).
    pub autosaver: crate::autosave::Autosaver,
    /// A previous session's autosave waiting for an answer.
    recovery: Option<recovery::RecoveryPrompt>,
    /// `project.describe`'s `recovered`: this session started from a
    /// recovery.
    recovered: Option<serde_json::Value>,
    /// `--recover apply` couldn't recover (headless exits 2).
    pub recovery_failed: Option<String>,
    /// The project is open in another Worlds (the session lock is held).
    pub locked_out: Option<String>,
    /// `--recover-report` bodies made (see `recover_reports_made`).
    recover_reports: usize,
    /// Window focus last frame, to notice a focus loss.
    was_focused: Option<bool>,
    /// The last fragment copied or pasted: what Ctrl+V pastes when the
    /// system clipboard can't be read (and `--script`'s paste).
    clipboard: Option<worlds_core::fragment::Fragment>,
    /// The New world / Rename world dialog.
    world_dialog: Option<worlds::WorldDialog>,
    /// Import from Worlds project…: the folder picker's answer.
    import_pick: Option<std::sync::mpsc::Receiver<Option<PathBuf>>>,
    /// The Import from Worlds project dialog.
    import: Option<import::ImportDialog>,
    /// The focused map tab last brought to the front of its tab group.
    shown_focus: Option<ViewportId>,
    /// The linked cameras as of the last frame ([`Self::sync_linked_cameras`]).
    link_prev: Option<[Camera; 2]>,
    /// Where the toolbar's ★ was drawn (the bookmark window hangs from it).
    bookmarks_anchor: Option<egui::Rect>,
}

impl EditorApp {
    pub fn new(ctx: &egui::Context, gpu: Gpu, log: LogBuffer, opts: AppOptions) -> Self {
        theme::apply(ctx);
        gpu.install_viewport();
        let mut viewport = Viewport::new();
        if opts.headless {
            viewport.rebuild_budget = None;
        }
        let problems = problems::LiveProblems::new(0, opts.headless);
        let autosaver = crate::autosave::Autosaver::new(opts.autosave);
        let mut library = crate::library::LibraryState::default();
        library.thumbs.headless = opts.headless;
        EditorApp {
            st: EditorState {
                viewport,
                log,
                library,
                ..Default::default()
            },
            problems,
            dock: default_dock(),
            gpu,
            ctx: ctx.clone(),
            commands: Box::new(NoCommands),
            watcher: None,
            own_writes: OwnWrites::default(),
            palette: CommandPalette::default(),
            reload_prompt: false,
            close_prompt: false,
            open_prompt: None,
            allow_close: false,
            terminate: None,
            tools_w: 0.0,
            toolbar_split: None,
            toast_bounds: None,
            toast_rects: Vec::new(),
            new_level: None,
            confirm: None,
            modal_keys: ModalKeys::default(),
            title: String::new(),
            opts,
            last_ui_save: Instant::now(),
            last_ui_json: String::new(),
            invalidations: Vec::new(),
            frame_ms: 0.0,
            dialog: None,
            opened_at: None,
            live: agent::Live::default(),
            last_active_layer: None,
            disk_entities: Vec::new(),
            lock: None,
            autosaver,
            recovery: None,
            recovered: None,
            recovery_failed: None,
            locked_out: None,
            recover_reports: 0,
            was_focused: None,
            clipboard: None,
            world_dialog: None,
            import_pick: None,
            import: None,
            shown_focus: None,
            link_prev: None,
            bookmarks_anchor: None,
        }
    }

    /// Swap in the editing backend (normally [`crate::edit::Editing`]).
    pub fn set_commands(&mut self, commands: Box<dyn EditorCommands>) {
        self.commands = commands;
    }

    /// Apply one command as its own undo entry and update the viewport
    /// (`--script`, tests).
    pub fn apply_command(&mut self, cmd: Command) -> Result<(), String> {
        let mut res = Err("no project open".to_string());
        self.flush_agent_batches();
        self.with_commands(|c, cx| res = c.apply(cx, cmd));
        self.apply_invalidations();
        self.report_user_edits();
        res
    }

    /// Run one `--script` item: apply a command, or replay a tool gesture
    /// through the command layer exactly as the viewport would send it. An
    /// error status the gesture leaves (a refused paint) is an error.
    pub fn run_step(&mut self, step: crate::script::ScriptStep) -> Result<(), String> {
        use crate::script::ScriptStep;
        let g = match step {
            ScriptStep::Command(c) => return self.apply_command(*c),
            ScriptStep::Gesture(g) => g,
            ScriptStep::Ui(u) => return self.ui_step(&u),
            ScriptStep::NoteGesture(n) => return self.note_gesture(&n),
            ScriptStep::WorldSelect(w) => return self.world_select(&w),
            ScriptStep::Vertex(v) => return self.vertex_gesture(&v),
            ScriptStep::History { redo, n } => {
                for k in 0..n {
                    let can = if redo {
                        self.commands.can_redo()
                    } else {
                        self.commands.can_undo()
                    };
                    if !can {
                        return Err(format!(
                            "{}: only {k} of {n} steps to take",
                            if redo { "redo" } else { "undo" }
                        ));
                    }
                    self.run(if redo { Action::Redo } else { Action::Undo });
                }
                return Ok(());
            }
        };
        let doc = self.st.doc.as_ref().ok_or("no project open")?;
        let plan = crate::script::gesture_events(
            doc,
            &g,
            self.st.view.active_layer,
            self.st.view.entities.brush.as_ref(),
        )
        .map_err(|e| format!("{e:#}"))?;
        let (mut events, level, layer) = (plan.events, plan.level, plan.layer);
        // A select gesture in a project with no layer has none
        // ([`crate::tools::NO_LAYER`]).
        let slug = doc
            .project
            .layers
            .get(layer)
            .map(|l| l.slug.to_string())
            .unwrap_or_default();
        let has_layer = layer < doc.project.layers.len();
        let tool = crate::script::parse_tool(&g.gesture).unwrap_or_default();
        self.run(Action::SetTool(tool));
        self.st.view.active_level = Some(level);
        if has_layer {
            self.st.view.active_layer = Some(layer);
        }
        if g.press
            && let Some(doc) = &self.st.doc
            && let Some(ToolEvent::Begin { at, grab, .. }) = events.first_mut()
        {
            // As a mouse press there would: the press picks the grab.
            let press = crate::viewport::SelectPress {
                active: Some(level),
                on_tab: false,
                handle: None,
                level_under: Some(level),
                wp: at.world_px,
                add: g.ctrl,
                zoom: self.st.view.zoom,
            };
            match crate::viewport::select_press(doc, &mut self.st.view, press) {
                Some((_, gr)) => *grab = gr,
                None => events.clear(),
            }
        }
        if let Some(b) = plan.brush {
            self.st.view.palette.brushes.insert(slug, b);
        }
        if let Some(d) = plan.density {
            self.st.view.palette.density = d;
        }
        if let Some(s) = plan.seed {
            self.st.view.palette.seed = Some(s);
        }
        if let Some(b) = plan.entity_brush {
            self.st.view.entities.brush = Some(b);
        }
        self.st.view.status = None;
        self.flush_agent_batches();
        self.with_commands(|c, cx| c.tool_events(cx, &events));
        self.apply_invalidations();
        self.report_user_edits();
        match &self.st.view.status {
            Some(m) if m.error => Err(m.text.clone()),
            _ => Ok(()),
        }
    }

    /// A script `note` or `measure` gesture: the Note or Measure tool
    /// through the same functions the mouse uses ([`crate::annot`]).
    fn note_gesture(&mut self, g: &crate::script::NoteGestureStep) -> Result<(), String> {
        use crate::view::NoteKind;
        let measure = g.gesture == "measure";
        self.run(Action::SetTool(if measure {
            Tool::Measure
        } else {
            Tool::Note
        }));
        let kind = match g.kind.as_deref() {
            None => NoteKind::Note,
            Some(k) => NoteKind::parse(k).ok_or_else(|| {
                format!("kind {k:?}: expected note, pin, path, region or connector")
            })?,
        };
        if measure && g.kind.is_some() {
            return Err("`kind` is for note gestures".into());
        }
        self.st.view.note_tool.kind = kind;
        self.st.view.note_tool.world = match g.scope.as_deref() {
            None => false,
            Some("world") => true,
            Some(o) => return Err(format!("scope {o:?}: only \"world\" can be given")),
        };
        if let Some(i) = &g.icon {
            self.st.view.note_tool.icon = i.clone();
        }
        if g.world_px.is_empty() {
            return Err("world_px: at least one point".into());
        }
        self.st.view.status = None;
        let clicks = measure
            || matches!(kind, NoteKind::Path | NoteKind::Connector)
            || (kind == NoteKind::Region && g.polygon);
        let doc = self.st.doc.as_ref().ok_or("no project open")?;
        let world = self.st.view.world_ix(doc);
        let under = |wp: [f64; 2]| crate::viewport::level_under_in(doc, world, wp);
        let mut cmds = Vec::new();
        if !clicks {
            let p0 = g.world_px[0];
            let cmd = match (kind, g.world_px.as_slice()) {
                (NoteKind::Region, [_, p1]) => {
                    crate::annot::tool_drag(doc, &mut self.st.view, p0, *p1, under(p0), g.ctrl)
                }
                (NoteKind::Region, _) => {
                    return Err(
                        "a rect region is a drag: give exactly 2 points (or \"polygon\": true)"
                            .into(),
                    );
                }
                (_, [_]) => crate::annot::tool_click(doc, &mut self.st.view, p0, under(p0), g.ctrl),
                _ => return Err(format!("a {} is one click: give 1 point", kind.name())),
            };
            cmds.extend(cmd);
        } else {
            for (i, &p) in g.world_px.iter().enumerate() {
                cmds.extend(crate::annot::tool_click_shift(
                    doc,
                    &mut self.st.view,
                    p,
                    under(p),
                    g.ctrl,
                    g.shift.contains(&i),
                ));
            }
            for k in &g.keys {
                match k.as_str() {
                    "backspace" => {
                        crate::annot::drawing_backspace(&mut self.st.view);
                    }
                    o => return Err(format!("key {o:?}: only \"backspace\"")),
                }
            }
            match g.end.as_deref().unwrap_or("enter") {
                "enter" => cmds.extend(crate::annot::finish_drawing(doc, &mut self.st.view)),
                "double_click" => {
                    let last = *g.world_px.last().expect("checked above");
                    cmds.extend(crate::annot::tool_click(
                        doc,
                        &mut self.st.view,
                        last,
                        under(last),
                        g.ctrl,
                    ));
                    cmds.extend(crate::annot::finish_drawing(doc, &mut self.st.view));
                }
                "escape" => {
                    crate::annot::cancel_drawing(&mut self.st.view);
                }
                "none" => {}
                o => return Err(format!("end {o:?}: enter, double_click, escape or none")),
            }
        }
        if let Some(m) = self.st.view.status.as_ref().filter(|m| m.error) {
            return Err(m.text.clone());
        }
        for c in cmds {
            self.apply_command(c)?;
        }
        Ok(())
    }

    /// A script `vertex` gesture: the one annotation `note` selected, then
    /// the Select tool on its geometry exactly as the mouse
    /// ([`crate::viewport::select_press_alt`]): a press on handle `vertex`
    /// (or a rect region's `handle`) dragged through `world_px`, or with
    /// `delete` clicked (highlighted) and the Delete key pressed; or an
    /// Alt+click at `insert`.
    fn vertex_gesture(&mut self, g: &crate::script::VertexStep) -> Result<(), String> {
        use crate::annot::Handle;
        let doc = self.st.doc.as_ref().ok_or("no project open")?;
        let id = worlds_core::Id::new(&g.note).map_err(|e| format!("note: {e}"))?;
        let r = crate::annot::find_note(doc, &id).ok_or(format!("no annotation {id}"))?;
        if let Some(ix) = r.level() {
            self.st.view.active_level = Some(ix);
        }
        self.run(Action::SetTool(Tool::Select));
        self.st
            .view
            .selection
            .set([crate::view::SelItem::Note(id.clone())]);
        let doc = self.st.doc.as_ref().ok_or("no project open")?;
        let r = crate::annot::find_note(doc, &id).ok_or(format!("no annotation {id}"))?;
        let handle = match (g.vertex, g.handle) {
            (Some(i), None) => Some(Handle::Vertex(i)),
            (None, Some(e)) => Some(Handle::Rect(e)),
            (None, None) => None,
            (Some(_), Some(_)) => return Err("give vertex or handle, not both".into()),
        };
        let path: Vec<[f64; 2]> = match (handle, g.insert) {
            (Some(h), None) => {
                let start = crate::annot::handles(doc, r)
                    .into_iter()
                    .find(|(x, _)| *x == h)
                    .map(|(_, p)| p)
                    .ok_or(format!("{id} has no handle {h:?}"))?;
                if g.delete {
                    if !g.world_px.is_empty() {
                        return Err("delete takes no world_px".into());
                    }
                    vec![start]
                } else {
                    if g.world_px.is_empty() {
                        return Err("world_px: where to drag the handle".into());
                    }
                    std::iter::once(start)
                        .chain(g.world_px.iter().copied())
                        .collect()
                }
            }
            (None, Some(p)) => {
                if g.delete || !g.world_px.is_empty() {
                    return Err("insert is one Alt+click: no world_px or delete".into());
                }
                vec![p]
            }
            (None, None) => return Err("give vertex, handle or insert".into()),
            (Some(_), Some(_)) => return Err("give vertex or handle, or insert, not both".into()),
        };
        self.world_select_alt(
            &crate::script::WorldSelectStep {
                gesture: "select".into(),
                world_px: path,
                press: true,
                ctrl: g.ctrl,
            },
            g.insert.is_some(),
        )?;
        if g.delete {
            self.st.view.status = None;
            self.run(crate::app::actions::delete_action(&self.st.view));
            if let Some(m) = self.st.view.status.as_ref().filter(|m| m.error) {
                return Err(m.text.clone());
            }
        }
        Ok(())
    }

    /// A script select gesture in world px: the press as the mouse makes
    /// it ([`crate::viewport::select_press`]), then the drag and release
    /// through the command layer.
    fn world_select(&mut self, g: &crate::script::WorldSelectStep) -> Result<(), String> {
        self.world_select_alt(g, false)
    }

    /// [`Self::world_select`] with Alt held at the press.
    fn world_select_alt(
        &mut self,
        g: &crate::script::WorldSelectStep,
        alt: bool,
    ) -> Result<(), String> {
        let Some((&p0, rest)) = g.world_px.split_first() else {
            return Err("world_px: at least one point".into());
        };
        self.run(Action::SetTool(Tool::Select));
        let doc = self.st.doc.as_ref().ok_or("no project open")?;
        let press = crate::viewport::SelectPress {
            active: self.st.view.active_level.filter(|&a| a < doc.levels.len()),
            on_tab: false,
            handle: None,
            level_under: crate::viewport::level_under_in(doc, self.st.view.world_ix(doc), p0),
            wp: p0,
            add: g.ctrl,
            zoom: self.st.view.zoom,
        };
        let Some((ix, grab)) =
            crate::viewport::select_press_alt(doc, &mut self.st.view, press, alt)
        else {
            return Ok(());
        };
        let layer = self
            .st
            .view
            .active_layer
            .filter(|&l| l < doc.project.layers.len())
            .or_else(|| (!doc.project.layers.is_empty()).then_some(0))
            .unwrap_or(crate::tools::NO_LAYER);
        let m = egui::Modifiers {
            ctrl: g.ctrl,
            command: g.ctrl,
            ..Default::default()
        };
        let start = crate::script::world_hit(doc, ix, layer, p0);
        let mut events = vec![ToolEvent::Begin {
            tool: Tool::Select,
            at: start,
            modifiers: m,
            grab,
        }];
        let mut at = start;
        for &p in rest {
            at = crate::script::world_hit(doc, ix, layer, p);
            events.push(ToolEvent::Move {
                tool: Tool::Select,
                start,
                at,
                modifiers: m,
            });
        }
        events.push(ToolEvent::End {
            tool: Tool::Select,
            start,
            at,
            modifiers: m,
        });
        self.st.view.status = None;
        self.flush_agent_batches();
        self.with_commands(|c, cx| c.tool_events(cx, &events));
        self.apply_invalidations();
        self.report_user_edits();
        match &self.st.view.status {
            Some(m) if m.error => Err(m.text.clone()),
            _ => Ok(()),
        }
    }

    /// A `--script` UI step (and `--rule`): see [`crate::script::UiStep`].
    pub fn ui_step(&mut self, u: &crate::script::UiStep) -> Result<(), String> {
        use crate::script::UiStep;
        match u {
            UiStep::Rules(r) => self.rules_step(r),
            UiStep::Palette(p) => self.palette_step(p),
            UiStep::Select(sel) => self.select_step(sel),
            UiStep::Types(t) => self.types_step(t),
            UiStep::Notes(n) => self.notes_step(n),
            UiStep::Focus(f) => self.focus_step(f),
            UiStep::Duplicate(_) => self.duplicate_step(),
            UiStep::Delete(d) => self.delete_step(d.confirm),
            UiStep::Layers(l) => self.layers_step(l),
            UiStep::Project(p) => self.project_step(p),
            UiStep::Copy(_) => self.status_step(Action::Copy),
            UiStep::Cut(_) => self.status_step(Action::Cut),
            UiStep::Paste(p) => self.paste_step(p),
            UiStep::Find(f) => self.find_step(f),
            UiStep::History(h) => self.history_step(h),
            UiStep::Reference(r) => self.reference_step(r),
            UiStep::ImportProject(s) => self.import_step(s),
            UiStep::Viewport(v) => self.viewport_step(v),
            UiStep::Bookmark(b) => self.bookmark_step(b),
            UiStep::Turn(t) => self.turn_step(t),
            UiStep::Transform(t) => self.transform_step(t),
            UiStep::Arrange(a) => self.arrange_step(a),
            UiStep::SaveAssembly(s) => self.save_assembly_step(s),
            UiStep::Library(l) => self.library_step(l),
            UiStep::Levels(l) => self.levels_step(l),
        }
    }

    /// `"ui": "levels"`: see [`crate::script::LevelsStep`].
    fn levels_step(&mut self, l: &crate::script::LevelsStep) -> Result<(), String> {
        let sort = match l.sort.as_deref() {
            None => levels::Sort::Order,
            Some(s) => levels::Sort::parse(s)
                .ok_or_else(|| format!("sort {s:?}: order, name or position"))?,
        };
        self.st.levels.search = l.search.clone().unwrap_or_default();
        self.st.levels.sort = sort;
        self.st.levels.all_worlds = l.all_worlds.unwrap_or(false);
        show_pane(&mut self.dock, Pane::Levels);
        if let Some(slug) = &l.select {
            let doc = self.st.doc.as_ref().ok_or("no project open")?;
            let ix = doc
                .level_ix(slug)
                .ok_or_else(|| format!("no level {slug:?}"))?;
            levels::select(&mut self.st.view, doc, ix);
        }
        Ok(())
    }

    /// `"ui": "find"`: see [`crate::script::FindStep`].
    fn find_step(&mut self, f: &crate::script::FindStep) -> Result<(), String> {
        use worlds_core::find::FindKind;
        let kind = match f.kind.as_deref() {
            None | Some("all") => None,
            Some(k) => Some(
                FindKind::parse(k)
                    .ok_or_else(|| format!("kind {k:?}: all, entities, levels, notes or tiles"))?,
            ),
        };
        self.st.find.query = f.query.clone();
        self.st.find.kind = kind;
        self.st.find.notice = None;
        self.st.find.rev = self.commands.editing_ref().map_or(0, |e| e.rev());
        let doc = self.st.doc.as_ref().ok_or("no project open")?;
        self.st.find.refresh(doc);
        if let Some(e) = &self.st.find.found().error {
            return Err(format!("query {:?}: {e}", f.query));
        }
        show_pane(&mut self.dock, Pane::Find);
        if f.select_all {
            if find::select_all(&mut self.st) == 0 {
                return Err(format!("query {:?} found nothing to select", f.query));
            }
            self.st.find.notice = Some((
                format!("Selected {}", self.st.view.selection.items.len()),
                false,
            ));
        }
        if let Some(r) = &f.replace {
            let n = find::replace(&mut self.st, &r.field, &r.value)?;
            self.st.find.replace_field = Some(r.field.clone());
            self.st.find.replace_value = r.value.clone();
            for cmd in std::mem::take(&mut self.st.pending) {
                self.apply_command(cmd)?;
            }
            self.st.find.notice = Some((
                format!(
                    "Replaced {} in {n} {}",
                    r.field,
                    if n == 1 { "entity" } else { "entities" }
                ),
                false,
            ));
        }
        Ok(())
    }

    /// Run an action as a script step: an error status it leaves is an
    /// error.
    fn status_step(&mut self, a: Action) -> Result<(), String> {
        self.st.view.status = None;
        self.run(a);
        match &self.st.view.status {
            Some(m) if m.error => Err(m.text.clone()),
            _ => Ok(()),
        }
    }

    /// `"ui": "paste"`: see [`crate::script::PasteStep`].
    fn paste_step(&mut self, p: &crate::script::PasteStep) -> Result<(), String> {
        let text = p.fragment.as_ref().map(|f| f.to_string());
        if p.fragment.is_none() && self.clipboard.is_none() {
            return Err("nothing copied yet: give fragment, or copy first".into());
        }
        self.status_step(Action::Paste {
            text,
            in_place: p.in_place,
        })?;
        if p.in_place {
            return Ok(());
        }
        if let Some(level) = &p.level {
            let doc = self.st.doc.as_ref().ok_or("no project open")?;
            let ix = doc
                .level_ix(level)
                .ok_or_else(|| format!("no level {level:?}"))?;
            let origin = p.origin.ok_or("level goes with origin")?;
            if let Some(g) = self.st.view.paste.as_mut() {
                g.at = Some((ix, origin));
                g.pinned = true;
            }
        }
        if p.place {
            let (ix, origin) = self
                .st
                .view
                .paste
                .as_ref()
                .and_then(|g| g.at)
                .ok_or("place needs level and origin")?;
            return self.status_step(Action::PasteAt { level: ix, origin });
        }
        Ok(())
    }

    /// `"ui": "project"`: the Project pane, through the functions its
    /// + Layer and + Tileset buttons use ([`project_pane`]).
    fn project_step(&mut self, p: &crate::script::ProjectStep) -> Result<(), String> {
        use project_pane::{Follow, Section};
        let section = p
            .section
            .as_deref()
            .map(|s| {
                Section::parse(s)
                    .ok_or_else(|| format!("section {s:?}: expected layers, tilesets or settings"))
            })
            .transpose()?;
        if let Some(kind) = &p.add_layer {
            let doc = self.st.doc.as_ref().ok_or("no project open")?;
            let (cmd, slug) = project_pane::new_layer(doc, kind)?;
            self.st.project.pending.push(cmd);
            self.st.project.follow = Some(Follow::Layer {
                slug,
                activate: true,
            });
            self.st.project.section = Section::Layers;
            self.project_pane_commands();
            if let Some(e) = self.st.project.error.take() {
                return Err(e);
            }
        }
        if let Some(path) = &p.add_tileset {
            let doc = self.st.doc.as_ref().ok_or("no project open")?;
            let (cmd, slug, copied) = project_pane::tileset_from_file(doc, path)?;
            self.st.project.pending.push(cmd);
            self.st.project.follow = Some(Follow::Tileset(slug));
            self.st.project.notice = copied;
            self.st.project.section = Section::Tilesets;
            self.project_pane_commands();
            if let Some(e) = self.st.project.error.take() {
                return Err(e);
            }
        }
        let doc = self.st.doc.as_ref().ok_or("no project open")?;
        if let Some(l) = &p.layer {
            let def = doc
                .project
                .layer(l)
                .ok_or_else(|| format!("no layer {l:?}"))?;
            self.st.project.layer = Some(def.slug.clone());
            self.st.project.section = Section::Layers;
        }
        if let Some(t) = &p.tileset {
            let def = doc
                .project
                .tileset(t)
                .ok_or_else(|| format!("no tileset {t:?}"))?;
            self.st.project.tileset = Some(def.slug.clone());
            self.st.project.section = Section::Tilesets;
        }
        if let Some(s) = section {
            self.st.project.section = s;
        }
        show_pane(&mut self.dock, Pane::Project);
        Ok(())
    }

    /// `"ui": "layers"`: the Layers pane's active layer, visibility and
    /// view opacity. Checked whole before anything changes.
    fn layers_step(&mut self, l: &crate::script::LayersStep) -> Result<(), String> {
        let doc = self.st.doc.as_ref().ok_or("no project open")?;
        let index = |s: &str| {
            doc.project
                .layers
                .iter()
                .position(|d| d.slug == s)
                .ok_or_else(|| format!("no layer {s:?}"))
        };
        let active = l.active.as_deref().map(index).transpose()?;
        let mut visible = Vec::new();
        for (s, v) in l.visible.iter().flatten() {
            visible.push((index(s)?, *v));
        }
        let mut opacity = Vec::new();
        for (s, o) in l.opacity.iter().flatten() {
            if !(0.0..=1.0).contains(o) {
                return Err(format!("opacity of {s}: {o} is not between 0 and 1"));
            }
            opacity.push((index(s)?, *o));
        }
        let n = doc.project.layers.len();
        if self.st.view.layers.len() != n {
            self.st.view.sync_layers(doc, None);
        }
        for (i, v) in visible {
            self.st.view.layers[i].visible = v;
        }
        for (i, o) in opacity {
            self.st.view.layers[i].opacity = o;
        }
        if let Some(i) = active {
            self.st.view.active_layer = Some(i);
            self.last_active_layer = Some(i);
        }
        Ok(())
    }

    /// `"ui": "duplicate"`: Ctrl-D. An error status it leaves (nothing
    /// selected, nothing copyable) is an error.
    fn duplicate_step(&mut self) -> Result<(), String> {
        if self.st.view.selection.is_empty() {
            return Err("Nothing selected to duplicate".into());
        }
        self.st.view.status = None;
        self.run(Action::DuplicateSelection);
        match &self.st.view.status {
            Some(m) if m.error => Err(m.text.clone()),
            _ => Ok(()),
        }
    }

    /// `"ui": "delete"`: the Delete key. A prompt it raises is answered
    /// yes with `confirm`, else dropped and reported as the error.
    fn delete_step(&mut self, confirm: bool) -> Result<(), String> {
        self.st.view.status = None;
        self.run(actions::delete_action(&self.st.view));
        if let Some(m) = self.st.view.status.as_ref().filter(|m| m.error) {
            return Err(m.text.clone());
        }
        if let Some(c) = self.confirm.take() {
            let (question, cmd) = c.question_and_command();
            if !confirm {
                return Err(format!(
                    "Delete asks first: {question:?}; add \"confirm\": true to answer yes"
                ));
            }
            self.apply_command(cmd)?;
        }
        Ok(())
    }

    /// `"ui": "notes"`: the Notes pane in front, with that state.
    fn notes_step(&mut self, n: &crate::script::NotesStep) -> Result<(), String> {
        let doc = self.st.doc.as_ref().ok_or("no project open")?;
        let ns = &mut self.st.notes;
        if let Some(s) = &n.search {
            ns.search = s.clone();
        }
        if let Some(k) = &n.kind {
            ns.kind = match k.as_str() {
                "any" => None,
                "note" | "pin" | "path" | "region" | "guide" | "connector" => Some(k.clone()),
                o => {
                    return Err(format!(
                        "kind {o:?}: note, pin, path, region, guide, connector or any"
                    ));
                }
            };
        }
        if let Some(sc) = &n.scope {
            ns.scope = match sc.as_str() {
                "all" => notes::ScopeFilter::All,
                "world" => notes::ScopeFilter::World,
                "this_level" => notes::ScopeFilter::ThisLevel,
                l => notes::ScopeFilter::Level(
                    doc.level_ix(l)
                        .map(|ix| doc.level(ix).slug.clone())
                        .ok_or_else(|| format!("scope: no level {l:?}"))?,
                ),
            };
        }
        if let Some(s) = &n.status {
            ns.status = notes::StatusFilter::parse(s)
                .ok_or_else(|| format!("status {s:?}: open, done or all"))?;
        }
        if let Some(t) = &n.tags {
            ns.tags = t.clone();
        }
        if let Some(g) = n.guides_open {
            ns.guides_open = g;
        }
        if let Some(v) = n.view_open {
            ns.view_open = v;
        }
        let a = &mut self.st.view.annotations;
        if let Some(t) = &n.hidden_tags {
            a.hidden_tags = t.iter().cloned().collect();
        }
        if let Some(h) = n.hide_done {
            a.hide_done = h;
        }
        if let Some(k) = &n.hidden_kinds {
            for x in k {
                if !["note", "pin", "path", "region", "guide", "connector"].contains(&x.as_str()) {
                    return Err(format!("hidden_kinds: no kind {x:?}"));
                }
            }
            a.hidden_kinds = k.iter().cloned().collect();
        }
        if let Some(v) = n.show_designed {
            a.show_designed = v;
        }
        if let Some(v) = n.snap_to_guides {
            a.snap_to_guides = v;
        }
        show_pane(&mut self.dock, Pane::Notes);
        Ok(())
    }

    /// `"ui": "focus"`: frame a level, a world rect or an annotation
    /// (exactly one), as `view.focus` does.
    fn focus_step(&mut self, f: &crate::script::FocusStep) -> Result<(), String> {
        let Some(id) = f.viewport.map(ViewportId) else {
            return self.focus_step_here(f);
        };
        if !self.st.view.has_tab(id) {
            return Err(format!("no viewport {id}"));
        }
        self.st.swap_live(id);
        let r = self.focus_step_here(f);
        let focused = self.st.view.focused;
        self.st.swap_live(focused);
        r
    }

    /// A focus step in the live map tab.
    fn focus_step_here(&mut self, f: &crate::script::FocusStep) -> Result<(), String> {
        let doc = self.st.doc.as_ref().ok_or("no project open")?;
        if let Some(w) = &f.world {
            let wi = doc
                .project
                .worlds
                .iter()
                .position(|x| x.slug == w.as_str())
                .ok_or_else(|| format!("no world {w:?}"))?;
            if f.level.is_some() || f.note.is_some() {
                return Err("ui \"focus\": world goes alone or with world_rect".into());
            }
            self.st.view.set_world(doc, wi);
            if f.world_rect.is_none() {
                self.st.viewport.request_fit(Fit::World);
                return Ok(());
            }
        }
        match (&f.level, &f.world_rect, &f.note) {
            (Some(l), None, None) => {
                let ix = doc.level_ix(l).ok_or_else(|| format!("no level {l:?}"))?;
                self.st.view.active_level = Some(ix);
                self.st.viewport.request_fit(Fit::Level(ix));
            }
            (None, Some(r), None) => {
                if r[2] <= 0 || r[3] <= 0 {
                    return Err(format!(
                        "world_rect {r:?} needs a positive width and height"
                    ));
                }
                self.st.viewport.request_fit(Fit::Rect(
                    crate::viewport::camera::WRect::from_pos_size(
                        [r[0] as f64, r[1] as f64],
                        [r[2] as f64, r[3] as f64],
                    ),
                ));
            }
            (None, None, Some(n)) => {
                let id = worlds_core::Id::new(n).map_err(|e| format!("note id {n:?}: {e}"))?;
                let (rect, level) =
                    crate::annot::focus_rect(doc, &id).ok_or_else(|| format!("no note {n:?}"))?;
                if let Some(r) = crate::annot::find_note(doc, &id) {
                    let w = crate::annot::note_world(doc, r.at);
                    self.st.view.set_world(doc, w);
                }
                if let Some(ix) = level {
                    self.st.view.active_level = Some(ix);
                }
                self.st.viewport.request_fit(Fit::Rect(rect));
            }
            _ => return Err("ui \"focus\" needs exactly one of level, world_rect and note".into()),
        }
        Ok(())
    }

    /// `"ui": "select"`: select entities, annotations, levels and tile
    /// stacks as `view.select` does (they add up; the first is primary;
    /// the active level follows the first entity, else the first level
    /// note, else the first level, else the tiles), frame that level and
    /// bring the Inspector to the front.
    fn select_step(&mut self, sel: &crate::script::SelectStep) -> Result<(), String> {
        use crate::view::SelItem;
        let doc = self.st.doc.as_ref().ok_or("no project open")?;
        let mut items = Vec::new();
        let mut level = None;
        for s in &sel.entities {
            let id = worlds_core::Id::new(s).map_err(|e| format!("entity id {s:?}: {e}"))?;
            let (ix, _, _) = doc
                .find_entity(&id)
                .ok_or_else(|| format!("no entity {s:?}"))?;
            level.get_or_insert(ix);
            items.push(SelItem::Entity(id));
        }
        for s in &sel.notes {
            let id = worlds_core::Id::new(s).map_err(|e| format!("note id {s:?}: {e}"))?;
            let r = crate::annot::find_note(doc, &id).ok_or_else(|| format!("no note {s:?}"))?;
            if level.is_none() {
                level = r.level();
            }
            items.push(SelItem::Note(id));
        }
        for s in &sel.levels {
            let ix = doc.level_ix(s).ok_or_else(|| format!("no level {s:?}"))?;
            level.get_or_insert(ix);
            items.push(SelItem::Level(doc.ids[ix].clone()));
        }
        if let Some(t) = &sel.tiles {
            let ix = doc
                .level_ix(&t.level)
                .ok_or_else(|| format!("no level {:?}", t.level))?;
            let def = doc
                .project
                .layer(&t.layer)
                .filter(|d| d.is_tiles())
                .ok_or_else(|| format!("no tiles layer {:?}", t.layer))?;
            let tl = doc.level(ix).tiles_layer(def.slug.as_str());
            for c in &t.cells {
                if !tl.is_some_and(|l| l.tiles.iter().any(|p| p.cell == *c)) {
                    return Err(format!("no tiles at {c:?} in {}/{}", t.level, t.layer));
                }
                items.push(SelItem::Tiles {
                    level: doc.ids[ix].clone(),
                    layer: def.slug.clone(),
                    cell: *c,
                });
            }
            level.get_or_insert(ix);
        }
        self.st.view.selection.set(items);
        if let Some(ix) = level {
            self.st.view.active_level = Some(ix);
            self.st.viewport.request_fit(Fit::Level(ix));
        }
        show_pane(&mut self.dock, Pane::Inspector);
        Ok(())
    }

    /// `"ui": "types"`: the Types pane on an entity type (`entity`) or the
    /// level fields (`level_fields: true`), with `field`'s form open.
    fn types_step(&mut self, t: &crate::script::TypesStep) -> Result<(), String> {
        let doc = self.st.doc.as_ref().ok_or("no project open")?;
        let (sel, fields) = match (&t.entity, t.level_fields) {
            (Some(e), None | Some(false)) => {
                let def = doc
                    .project
                    .entity_def(e)
                    .ok_or_else(|| format!("no entity type {e:?}"))?;
                (defs::TypesSel::Entity(def.slug.clone()), &def.fields)
            }
            (None, Some(true)) => (defs::TypesSel::LevelFields, &doc.project.level_fields),
            _ => {
                return Err(
                    "ui \"types\" needs exactly one of `entity` and `\"level_fields\": true`"
                        .into(),
                );
            }
        };
        let field = match &t.field {
            Some(f) => Some(
                fields
                    .iter()
                    .find(|d| d.slug == f.as_str())
                    .map(|d| d.slug.clone())
                    .ok_or_else(|| format!("no field {f:?} there"))?,
            ),
            None => None,
        };
        self.st.types.show(sel);
        // With a field, its form is what the step is about: the type's
        // own form folds away and the field form scrolls into view.
        if field.is_some() {
            self.st.types.force_type_open = Some(false);
            self.st.types.reveal_field = true;
        }
        self.st.types.field = field;
        show_pane(&mut self.dock, Pane::EntityDefs);
        Ok(())
    }

    /// `"ui": "palette"`: the Palette pane on an entities layer, then the
    /// given parts of its state.
    fn palette_step(&mut self, p: &crate::script::PaletteStep) -> Result<(), String> {
        let doc = self.st.doc.as_ref().ok_or("no project open")?;
        let layer = self
            .st
            .view
            .active_layer
            .filter(|&l| doc.project.layers.get(l).is_some_and(|d| d.is_entities()))
            .or_else(|| crate::view::first_entities_layer(doc))
            .ok_or("the project has no entities layer")?;
        let ty = |s: &String| -> Result<Slug, String> {
            doc.project
                .entity_def(s)
                .map(|d| d.slug.clone())
                .ok_or_else(|| format!("no entity type {s:?}"))
        };
        let types = |v: &Vec<String>| v.iter().map(ty).collect::<Result<Vec<Slug>, String>>();
        let mut pal = self.st.view.entities.clone();
        if let Some(v) = &p.favorites {
            pal.favorites = types(v)?;
        }
        if let Some(v) = &p.recent {
            pal.recent = types(v)?;
            pal.recent.truncate(crate::tools::RECENT_MAX);
        }
        if let Some(v) = &p.collapsed {
            pal.collapsed = v.iter().cloned().collect();
        }
        if let Some(v) = &p.chips {
            pal.chips = v.clone();
        }
        if let Some(v) = p.view {
            pal.view = v;
        }
        if let Some(b) = &p.brush {
            pal.brush = Some(EntityBrush::of(ty(b)?));
        }
        self.st.view.entities = pal;
        self.st.view.active_layer = Some(layer);
        show_pane(&mut self.dock, Pane::Palette);
        if p.tool == Some(true) {
            self.run(Action::SetTool(Tool::Entity));
        }
        if let Some(q) = &p.search {
            self.st.entities.query = q.clone();
        }
        if p.enter == Some(true) {
            let doc = self.st.doc.as_ref().ok_or("no project open")?;
            let mut acts = Vec::new();
            entities::enter(&mut self.st.entities, doc, &mut self.st.view, &mut acts)?;
            for a in acts {
                self.run(a);
            }
        }
        Ok(())
    }

    /// `"ui": "rules"` (and `--rule`): open the Rules pane on a layer, make
    /// a level active and frame it, select a rule.
    fn rules_step(&mut self, u: &crate::script::RulesStep) -> Result<(), String> {
        let doc = self.st.doc.as_ref().ok_or("no project open")?;
        let li = doc
            .project
            .layers
            .iter()
            .position(|l| l.slug == u.layer.as_str())
            .ok_or_else(|| format!("no layer {:?}", u.layer))?;
        let def = &doc.project.layers[li];
        if !def.is_auto() {
            return Err(format!(
                "layer {} is a {} layer, not auto",
                def.slug,
                def.kind.name()
            ));
        }
        let level = match &u.level {
            Some(l) => Some(doc.level_ix(l).ok_or_else(|| format!("no level {l:?}"))?),
            None => None,
        };
        let rule = match &u.rule {
            Some(r) => {
                let expanded = doc
                    .project
                    .expanded_rules(def.slug.as_str())
                    .map_err(|e| e.to_string())?;
                let known = expanded.iter().any(|g| {
                    g.group.rules.iter().any(|e| e.slug() == r.as_str())
                        || g.rules.iter().any(|x| x.rule.slug == r.as_str())
                });
                if !known {
                    return Err(format!("layer {} has no rule {r:?}", def.slug));
                }
                // Show an expanded rule's reference opened in the list.
                let from = expanded
                    .iter()
                    .flat_map(|g| &g.rules)
                    .find(|x| x.rule.slug == r.as_str())
                    .and_then(|x| x.from.clone());
                Some((Slug::new(r).map_err(|e| e.to_string())?, from))
            }
            None => None,
        };
        let mut collapsed = std::collections::BTreeSet::new();
        for g in &u.group_collapsed {
            collapsed.insert(Slug::new(g).map_err(|e| format!("group_collapsed: {e}"))?);
        }
        let slug = def.slug.clone();
        if let Some(ix) = level {
            self.st.view.active_level = Some(ix);
            self.st.viewport.request_fit(Fit::Level(ix));
        }
        self.st.view.active_layer = Some(li);
        self.last_active_layer = Some(li);
        let r = &mut self.st.rules;
        r.layer = Some(slug);
        r.collapsed = collapsed;
        r.multi.clear();
        if let Some(v) = u.show_regions {
            r.hide_regions = !v;
        }
        if let Some(v) = u.show_occupancy {
            r.show_occupancy = v;
        }
        if let Some((rule, from)) = rule {
            if let Some(f) = from {
                r.expanded.insert(f);
            }
            r.selected = Some(rule);
            r.reveal = true;
        }
        r.focus = true;
        Ok(())
    }

    /// Unsaved edits?
    pub fn has_unsaved(&self) -> bool {
        self.commands.has_unsaved()
    }

    /// Save (Ctrl+S). Errors are also shown as a toast.
    pub fn save(&mut self) -> anyhow::Result<()> {
        if self.opts.is_synthetic() {
            let e = anyhow::anyhow!("a --synthetic world is generated and can't be saved");
            self.st.toast(format!("Save refused: {e}"), true);
            return Err(e);
        }
        let dirty: Vec<PathBuf> = self
            .st
            .doc
            .as_ref()
            .map(|d| d.project.files.dirty().iter().cloned().collect())
            .unwrap_or_default();
        let mut res = Ok(());
        self.with_commands(|c, cx| res = c.save(cx));
        // Also after a failed save: whatever it did write is ours.
        if let Some(doc) = &self.st.doc {
            self.own_writes.record(&doc.dir, &dirty);
        }
        match &res {
            Ok(()) => {
                // Saved: this session's autosave has nothing left to keep.
                self.autosaver.clear();
                // The save revalidated; the disk problems stay listed.
                let rev = self.commands.editing_ref().map_or(0, |e| e.rev());
                self.problems.validated(rev);
                if let Some(doc) = &mut self.st.doc {
                    doc.problems
                        .extend(self.problems.disk.values().flatten().cloned());
                }
                self.report_save(&dirty);
                if let Some(doc) = &self.st.doc
                    && !doc.problems.is_empty()
                {
                    tracing::info!("{} validation problems after save", doc.problems.len());
                }
            }
            Err(e) => self.st.toast(format!("Save failed: {e:#}"), true),
        }
        res
    }

    /// How many undo entries the history holds (tests).
    pub fn undo_entries(&mut self) -> usize {
        self.commands
            .editing()
            .map_or(0, |e| e.history().undo_entries().len())
    }

    pub fn document(&self) -> Option<&Document> {
        self.st.doc.as_ref()
    }

    fn load(&self, dir: &Path) -> Result<Document, String> {
        let mut doc = Document::open(dir).map_err(|e| e.to_string())?;
        if let Some(n) = self.opts.synthetic {
            let mut p = doc.project;
            synthesize(&mut p, n);
            doc = Document::from_project(doc.dir, p);
            tracing::info!("synthetic world: {} levels", doc.levels.len());
        }
        if let Some(n) = self.opts.synthetic_entities {
            let mut p = doc.project;
            // Icons only from tiles that draw something, when the image reads.
            let atlas =
                p.tilesets
                    .first()
                    .and_then(|t| match image::open(doc.dir.join(&t.image)) {
                        Ok(img) => Some(crate::doc::SynthAtlas::from_image(t, &img.to_rgba8())),
                        Err(_) => image::image_dimensions(doc.dir.join(&t.image))
                            .ok()
                            .map(|(w, h)| crate::doc::SynthAtlas::of_size([w, h])),
                    });
            crate::doc::synthesize_entities(&mut p, n, atlas.as_ref());
            doc = Document::from_project(doc.dir, p);
            tracing::info!("synthetic entities: {n} types");
        }
        if self.opts.synthetic_notes.is_some() || self.opts.synthetic_guides.is_some() {
            let mut p = doc.project;
            if let Some(n) = self.opts.synthetic_notes {
                crate::doc::synthesize_notes(&mut p, n);
            }
            if let Some(n) = self.opts.synthetic_guides {
                crate::doc::synthesize_guides(&mut p, n);
            }
            doc = Document::from_project(doc.dir, p);
        }
        Ok(doc)
    }

    /// File > Open: open `dir`, asking first (Save / Discard / Cancel) when
    /// the open document has unsaved edits.
    pub fn request_open(&mut self, dir: &Path) {
        if self.st.doc.is_some() && self.has_unsaved() {
            self.open_prompt = Some(dir.to_path_buf());
        } else {
            self.open(dir);
        }
    }

    /// Whether the "save before opening" prompt is up.
    pub fn open_prompt_pending(&self) -> bool {
        self.open_prompt.is_some()
    }

    /// Answer the "save before opening" prompt (its buttons; tests).
    pub fn open_prompt_choose(&mut self, c: OpenChoice) {
        let Some(dir) = self.open_prompt.take() else {
            return;
        };
        match c {
            OpenChoice::Save => {
                if self.save().is_ok() {
                    self.open(&dir);
                }
            }
            OpenChoice::Discard => self.open_inner(&dir, true),
            OpenChoice::Cancel => {}
        }
    }

    /// Open a project directory, replacing the current one. An editing
    /// session (`opts.edits`) takes the project's session lock first (held
    /// by another Worlds: nothing opens, see [`Self::locked_out`]) and
    /// afterwards looks for a previous session's autosave.
    ///
    /// The open document's unsaved edits are autosaved first (when this
    /// session autosaves), so opening another project never loses them:
    /// they are offered back when that project opens again. Its autosave
    /// writes must finish before its lock goes; if they don't, nothing
    /// changes.
    pub fn open(&mut self, dir: &Path) {
        self.open_inner(dir, false);
    }

    /// [`Self::open`]; `discard`: the open document's unsaved edits are
    /// thrown away on purpose (the open prompt's Discard), with their
    /// autosave.
    fn open_inner(&mut self, dir: &Path, discard: bool) {
        // Not a project: nothing is created in it (no `.worlds/`, lock or
        // editor.json in a folder picked by mistake).
        if !dir.join("project.json").is_file() {
            let msg = format!(
                "Cannot open project: {} has no project.json (pick the project directory, e.g. mygame.worlds)",
                dir.display()
            );
            tracing::warn!("{msg}");
            self.st.toast(msg, true);
            return;
        }
        self.save_ui_file();
        self.opened_at = Some(Instant::now());
        let wants_lock = self.opts.edits && !self.opts.is_synthetic();
        let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        let same = self
            .lock
            .as_ref()
            .is_some_and(|l| canon(l.dir()) == canon(dir));
        let mut new_lock = None;
        if wants_lock && !same {
            match worlds_core::autosave::EditorLock::acquire(dir, env!("CARGO_PKG_VERSION")) {
                Ok(l) => new_lock = Some(l),
                Err(e) => {
                    let msg = e.to_string();
                    tracing::warn!("{msg}");
                    if self.st.doc.is_none() {
                        self.locked_out = Some(msg);
                    } else {
                        self.st.toast(format!("Cannot open project: {msg}"), true);
                    }
                    return;
                }
            }
        }
        let doc = match self.load(dir) {
            Ok(d) => d,
            Err(e) => {
                if let Some(l) = new_lock {
                    l.release();
                }
                self.st.toast(format!("Cannot open project: {e}"), true);
                return;
            }
        };
        // The open document's autosave: brought up to date (unsaved edits)
        // or removed (clean, or discarded), and every write finished before
        // its lock goes or the same project is inspected for a crashed
        // session's autosave (it would find this session's own).
        if let Some(old) = &self.st.doc {
            let dirty = old.project.is_dirty();
            let _ = self.autosaver.poll(Instant::now());
            let rev = self.commands.editing_ref().map_or(0, |e| e.rev());
            if dirty && !discard {
                if self.autosaver.active() && !self.autosaver.current(rev) {
                    self.autosave_now();
                }
            } else {
                self.autosaver.clear();
            }
            if !self.autosaver.flush(recovery::FLUSH_TIMEOUT) {
                if let Some(l) = new_lock {
                    l.release();
                }
                self.st.toast(
                    "Cannot open project yet: the open project's autosave is still being written; try again",
                    true,
                );
                return;
            }
            for e in self.autosaver.poll(Instant::now()) {
                self.st.toast(e, true);
            }
        }
        if new_lock.is_some() || !wants_lock {
            // Another project: its lock goes (an unsaved autosave of it
            // stays, to recover next time).
            if let Some(old) = self.lock.take() {
                old.release();
            }
            self.lock = new_lock;
        }
        self.recovery = None;
        self.recovered = None;
        self.open_document(doc);
        let rev = self.commands.editing_ref().map_or(0, |e| e.rev());
        let locked = recovery::session_locked(self.lock.as_ref());
        if wants_lock && !locked {
            let msg = "This filesystem can't lock the project: autosave and crash recovery are off for this session";
            tracing::warn!("{msg}");
            self.st.toast(msg, true);
            if self.opts.recover == Some(recovery::RecoverMode::Apply) {
                self.recovery_failed = Some(msg.into());
            }
        }
        self.autosaver.reset(dir, rev, locked);
        if locked {
            self.check_recovery(dir);
        }
    }

    /// Show `doc` as the open document (a fresh view, scene, bakes,
    /// watcher, server).
    pub(super) fn open_document(&mut self, mut doc: Document) {
        self.st.view = ViewState::new();
        self.st.view.sync_layers(&doc, None);
        self.st.atlas.clear();
        self.st.palette_tab.clear();
        self.st.edit_bufs.clear();
        self.st.pending.clear();
        self.st.rules = rules::RulesState::default();
        self.st.entities = entities::PaletteState::default();
        self.st.types = defs::TypesState::default();
        self.st.notes = notes::NotesState::default();
        self.st.find = find::FindState::default();
        self.dock = default_dock();
        self.shown_focus = None;
        self.link_prev = None;
        self.st.bookmarks_open = false;
        let restored = if self.opts.headless {
            false
        } else {
            self.restore_ui_file(&doc)
        };
        if let Some(slug) = &self.opts.level {
            match doc.level_ix(slug) {
                Some(ix) => self.st.view.active_level = Some(ix),
                None => self
                    .st
                    .toast(format!("No level {slug:?} in the project"), true),
            }
        }
        if self.opts.headless {
            doc.inline_layer_bakes = true;
            doc.bake_all_blocking();
        } else {
            let ctx = self.ctx.clone();
            doc.wake = Some(Arc::new(move || ctx.request_repaint()));
            let ctx = self.ctx.clone();
            doc.start_bakes(move || ctx.request_repaint());
        }
        self.problems_toast(&doc);
        self.st.viewport.set_document(&doc, &self.gpu);
        if !restored || self.opts.level.is_some() {
            self.st.viewport.request_fit(
                match (self.opts.level.is_some(), self.st.view.active_level) {
                    (true, Some(ix)) => Fit::Level(ix),
                    _ => Fit::World,
                },
            );
        }
        self.ctx
            .send_viewport_cmd(egui::ViewportCommand::Title(format!(
                "Worlds — {}",
                doc.project.name
            )));
        self.watcher = None;
        self.own_writes.clear();
        if !self.opts.headless {
            self.start_watcher(&doc);
        }
        self.st.doc = Some(doc);
        self.commands.reloaded();
        self.reset_problems();
        if self.opts.serve {
            self.start_server();
        } else {
            self.stop_server();
        }
    }

    fn problems_toast(&mut self, doc: &Document) {
        if !doc.problems.is_empty() {
            self.st.toast(
                format!(
                    "{} validation problem{} (see Problems)",
                    doc.problems.len(),
                    if doc.problems.len() == 1 { "" } else { "s" }
                ),
                true,
            );
        }
    }

    fn start_watcher(&mut self, doc: &Document) {
        let ctx = self.ctx.clone();
        match ProjectWatcher::new(doc, move || ctx.request_repaint_after(watch::DEBOUNCE)) {
            Ok(w) => self.watcher = Some(w),
            Err(e) => tracing::warn!("cannot watch {}: {e}", doc.dir.display()),
        }
    }

    fn ui_file_path(doc: &Document) -> PathBuf {
        doc.dir.join(".worlds").join("ui.json")
    }

    fn restore_ui_file(&mut self, doc: &Document) -> bool {
        let path = Self::ui_file_path(doc);
        let Ok(text) = std::fs::read_to_string(&path) else {
            return false;
        };
        let f: UiFile = match parse_ui_file(&text) {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!("ignoring {}: {e}", path.display());
                return false;
            }
        };
        self.dock = f.dock.clone();
        // A layout without a map would be unusable.
        if viewports::dock_viewports(&self.dock).is_empty() {
            self.dock = default_dock();
        }
        add_rules_pane(&mut self.dock);
        add_types_pane(&mut self.dock);
        add_notes_pane(&mut self.dock);
        add_project_pane(&mut self.dock);
        add_find_pane(&mut self.dock);
        add_history_pane(&mut self.dock);
        add_levels_pane(&mut self.dock);
        self.restore_viewports(doc, &f);
        if let Some(i) = f
            .active_layer
            .and_then(|s| doc.project.layers.iter().position(|l| l.slug == s.as_str()))
        {
            self.st.view.active_layer = Some(i);
        }
        for (i, l) in doc.project.layers.iter().enumerate() {
            if let Some(v) = f.layers.get(l.slug.as_str()) {
                self.st.view.layers[i] = *v;
            }
        }
        self.st.view.show_grid = f.show_grid;
        // A stored mode that isn't a Paint mode falls back to Brush
        // (`paint_tool`); the stored tool, if it is one, wins.
        self.st.view.paint_mode = f.paint_mode;
        self.st.view.paint_mode = self.st.view.paint_tool();
        self.st.view.set_tool(f.tool);
        self.st.view.palette = f.palette;
        self.st.view.entities = f.entity_palette.to_palette(&doc.project);
        self.st.view.annotations = f.annotations;
        self.st.view.note_tool = f.note_tool;
        self.st.view.references.ui = f.references;
        self.st.view.bookmarks = f.bookmarks;
        self.st.view.library = f.library;
        self.last_ui_json = text;
        true
    }

    /// The map tabs of `f` (see [`ui_viewports`]), the focused one live.
    fn restore_viewports(&mut self, doc: &Document, f: &UiFile) {
        let docked = viewports::dock_viewports(&self.dock);
        let (mut tabs, focused) = ui_viewports(doc, &docked, f);
        // The focused tab goes live: its state into the live slots.
        let live = tabs
            .iter_mut()
            .find(|t| t.id == focused)
            .expect("focused is docked");
        let view = &mut self.st.view;
        view.active_level = live.active_level.take();
        view.active_world = live.active_world.take();
        view.zoom = live.zoom;
        self.st.viewport.swap_tab(&mut live.cam);
        view.tabs = tabs;
        view.focused = focused;
        view.live = focused;
        view.link = f
            .linked_viewports
            .filter(|l| l[0] != l[1] && l.iter().all(|id| docked.contains(id)));
    }

    /// Write `.worlds/ui.json` if it changed.
    /// The flag [`crate::signals::watch`] raises on SIGINT/SIGTERM.
    pub fn set_terminate_flag(&mut self, flag: Arc<AtomicBool>) {
        self.terminate = Some(flag);
    }

    /// Whether a signal asked the editor to shut down.
    pub fn terminate_requested(&self) -> bool {
        self.terminate
            .as_ref()
            .is_some_and(|f| f.load(std::sync::atomic::Ordering::SeqCst))
    }

    pub fn save_ui_file(&mut self) {
        if self.opts.headless {
            return;
        }
        let Some(doc) = &self.st.doc else {
            return;
        };
        let view = &self.st.view;
        let slug_of_world = |w: usize| doc.project.worlds.get(w).map(|w| w.slug.to_string());
        let viewports = view
            .tabs
            .iter()
            .map(|t| UiViewport {
                id: t.id,
                world: slug_of_world(view.tab_world_ix(doc, t.id)),
                camera: view
                    .tab_camera(&self.st.viewport, t.id)
                    .map_or_else(Camera::default, |c| c.0),
                active_level: view
                    .tab_active_level(t.id)
                    .filter(|&i| i < doc.levels.len())
                    .map(|i| doc.level(i).slug.to_string()),
            })
            .collect();
        let f = UiFile {
            dock: self.dock.clone(),
            camera: self.st.viewport.camera,
            active_level: self
                .st
                .view
                .active_level
                .map(|i| doc.level(i).slug.to_string()),
            active_layer: self
                .st
                .view
                .active_layer
                .map(|i| doc.project.layers[i].slug.to_string()),
            layers: doc
                .project
                .layers
                .iter()
                .zip(&self.st.view.layers)
                .map(|(l, v)| (l.slug.to_string(), *v))
                .collect(),
            show_grid: self.st.view.show_grid,
            tool: self.st.view.tool,
            paint_mode: self.st.view.paint_tool(),
            palette: self.st.view.palette.clone(),
            entity_palette: UiPalette::from_palette(&self.st.view.entities),
            annotations: self.st.view.annotations.clone(),
            note_tool: self.st.view.note_tool.clone(),
            active_world: doc
                .project
                .worlds
                .get(self.st.view.world_ix(doc))
                .map(|w| w.slug.to_string()),
            references: self.st.view.references.ui,
            viewports,
            focused_viewport: Some(view.focused),
            linked_viewports: view.link,
            bookmarks: view.bookmarks.clone(),
            library: self.st.view.library.clone(),
        };
        let Ok(json) = serde_json::to_string_pretty(&f) else {
            return;
        };
        if json == self.last_ui_json {
            return;
        }
        let path = Self::ui_file_path(doc);
        let res = std::fs::create_dir_all(path.parent().expect("has parent"))
            .and_then(|_| std::fs::write(&path, &json));
        match res {
            Ok(()) => self.last_ui_json = json,
            Err(e) => tracing::warn!("cannot write {}: {e}", path.display()),
        }
    }

    // ---- per frame ----

    /// Draw one frame.
    pub fn frame(&mut self, ui: &mut egui::Ui) {
        let t0 = Instant::now();
        let ctx = ui.ctx().clone();
        self.poll_external(&ctx);
        // A level made active in another world (a problem, a note, the
        // API) brings its world along.
        if let Some(doc) = &self.st.doc {
            self.st.view.sync_world(doc);
        }
        self.modal_keys = modal_keys(&ctx, self.top_modal());
        self.shortcuts(&ctx);

        let top = egui::Panel::top("toolbar")
            .show(ui, |ui| self.toolbar(ui))
            .response
            .rect;
        let bottom = egui::Panel::bottom("status")
            .show(ui, |ui| self.status_bar(ui))
            .response
            .rect;
        self.toast_bounds = Some(egui::Rect::from_x_y_ranges(
            top.left()..=top.right(),
            top.bottom()..=bottom.top(),
        ));
        self.rules_frame();
        // A note just added: its title takes the keyboard in the Inspector,
        // so the Inspector comes to the front until it has (unless the
        // note is no longer what is selected).
        if let Some(id) = &self.st.view.focus_title {
            if self
                .st
                .view
                .selection
                .contains(&crate::view::SelItem::Note(id.clone()))
            {
                show_pane(&mut self.dock, Pane::Inspector);
            } else {
                self.st.view.focus_title = None;
            }
        }
        let rev = self.commands.editing_ref().map_or(0, |e| e.rev());
        entities::refresh(&mut self.st, rev, &ctx);
        self.st.notes.rev = rev;
        self.st.find.rev = rev;
        self.st.levels.rev = rev;
        if std::mem::take(&mut self.st.find.focus) {
            show_pane(&mut self.dock, Pane::Find);
        }
        if std::mem::take(&mut self.st.levels.focus) {
            show_pane(&mut self.dock, Pane::Levels);
        }
        history::refresh(
            &mut self.st.history,
            rev,
            self.commands.editing_ref().map(|e| e.history()),
        );
        self.sync_viewport_tabs();
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                let mut tabs = Tabs {
                    st: &mut self.st,
                    gpu: &self.gpu,
                };
                // Close buttons show on map tabs only (the last one has
                // none: `is_closeable`).
                DockArea::new(&mut self.dock)
                    .id(egui::Id::new("worlds dock"))
                    .show_close_buttons(true)
                    .show_leaf_close_all_buttons(false)
                    .show_inside(ui, &mut tabs);
            });
        let focused = self.st.view.focused;
        self.st.swap_live(focused);
        self.sync_linked_cameras();
        self.bookmarks_window(&ctx);
        self.command_palette(&ctx);
        self.recovery_window(&ctx);
        self.locked_window(&ctx);
        self.reload_prompt(&ctx);
        self.close_prompt(&ctx);
        self.open_prompt(&ctx);
        self.new_level_dialog(&ctx);
        self.world_dialog(&ctx);
        self.import_window(&ctx);
        crate::library::save_dialog(&ctx, &mut self.st);
        self.confirm(&ctx);
        self.toasts(&ctx);
        self.pending_panel(&ctx);
        if std::mem::take(&mut self.st.notes.focus) {
            show_pane(&mut self.dock, Pane::Notes);
        }

        // Tool events and queued invalidations go to the command layer.
        let events = std::mem::take(&mut self.st.tool_events);
        if events.iter().any(|e| matches!(e, ToolEvent::Begin { .. })) {
            self.flush_agent_batches();
        }
        self.with_commands(|c, cx| c.tool_events(cx, &events));
        self.st.tool_events = events;
        self.st.tool_events.clear();
        for cmd in std::mem::take(&mut self.st.pending) {
            if let Err(e) = self.apply_command(cmd) {
                self.st.toast(e, true);
            }
        }
        // The Types pane's commands: a refusal is shown in the pane.
        let types_pending = std::mem::take(&mut self.st.types.pending);
        let mut types_ok = true;
        for cmd in types_pending {
            match self.apply_command(cmd) {
                Ok(()) => self.st.types.error = None,
                Err(e) => {
                    types_ok = false;
                    self.st.types.error = Some(e);
                }
            }
        }
        // A rename the pane asked for: the form follows it if it applied.
        self.st.types.applied(types_ok);
        if std::mem::take(&mut self.st.types.focus) {
            show_pane(&mut self.dock, Pane::EntityDefs);
        }
        self.project_pane_commands();
        self.poll_reference_picker();
        self.apply_invalidations();
        self.report_user_edits();
        // The drain point: agent requests, unless a gesture is in progress.
        self.live_frame();
        self.apply_disk_entities();
        self.live_problems(&ctx);
        self.update_title(&ctx);

        for a in std::mem::take(&mut self.st.actions) {
            self.run(a);
        }
        self.autosave_tick(&ctx);
        if self.last_ui_save.elapsed() > Duration::from_secs(2) {
            self.last_ui_save = Instant::now();
            self.save_ui_file();
        }
        if let Some(t) = self.opened_at.take() {
            tracing::info!(
                "first frame {:.1} ms after open",
                t.elapsed().as_secs_f64() * 1e3
            );
        }
        let ms = t0.elapsed().as_secs_f32() * 1e3;
        self.frame_ms = if self.frame_ms == 0.0 {
            ms
        } else {
            self.frame_ms * 0.9 + ms * 0.1
        };
    }

    /// The Project pane's commands, each its own undo entry: a refusal is
    /// shown in the pane; a layer the pane added becomes the active layer.
    fn project_pane_commands(&mut self) {
        let pending = std::mem::take(&mut self.st.project.pending);
        let mut ok = true;
        for cmd in pending {
            match self.apply_command(cmd) {
                Ok(()) => self.st.project.error = None,
                Err(e) => {
                    ok = false;
                    self.st.project.error = Some(e);
                }
            }
        }
        if let Some(slug) = self.st.project.applied(ok)
            && let Some(doc) = &self.st.doc
        {
            self.st.view.active_layer = doc.project.layers.iter().position(|l| l.slug == slug);
        }
        if std::mem::take(&mut self.st.project.focus) {
            show_pane(&mut self.dock, Pane::Project);
        }
    }

    /// Fresh live validation for a (re)opened document (after the command
    /// layer was reset).
    pub(super) fn reset_problems(&mut self) {
        let rev = self.commands.editing_ref().map_or(0, |e| e.rev());
        let mut p = problems::LiveProblems::new(rev, self.opts.headless);
        let ctx = self.ctx.clone();
        p.wake = Some(Arc::new(move || ctx.request_repaint()));
        self.problems = p;
    }

    /// Live problems (see [`problems`]): revalidate once the project has
    /// been quiet for a moment and no gesture or batch is open; send
    /// `problems` when the list changed.
    fn live_problems(&mut self, ctx: &egui::Context) {
        let rev = self.commands.editing_ref().map_or(0, |e| e.rev());
        let busy = self.busy();
        let Some(doc) = self.st.doc.as_mut() else {
            return;
        };
        match self.problems.poll(doc, rev, busy, Instant::now()) {
            problems::Poll::Idle => {}
            problems::Poll::Wait(d) => ctx.request_repaint_after(d),
            problems::Poll::Fresh(list) => {
                if list != doc.problems {
                    doc.problems = list;
                    if let Some(s) = self.live.server.as_mut() {
                        s.notify_problems(&doc.problems);
                    }
                }
            }
        }
    }

    /// The live problems' state (tests).
    pub fn live_problems_state(&self) -> &problems::LiveProblems {
        &self.problems
    }

    /// Before the dock: the Rules pane's counts and the canvas highlight,
    /// and bring the pane to the front when asked or when the active layer
    /// became an auto layer.
    fn rules_frame(&mut self) {
        let active = self.st.view.active_layer;
        if active != self.last_active_layer {
            self.last_active_layer = active;
            if let (Some(doc), Some(i)) = (&self.st.doc, active)
                && doc.project.layers.get(i).is_some_and(|l| l.is_auto())
            {
                self.st.rules.layer = Some(doc.project.layers[i].slug.clone());
                self.st.rules.focus = true;
            }
        }
        if std::mem::take(&mut self.st.rules.focus) {
            show_pane(&mut self.dock, Pane::Rules);
        }
        let (rev, in_gesture) = self
            .commands
            .editing_ref()
            .map_or((0, false), |e| (e.rev(), e.in_gesture()));
        let in_gesture = in_gesture || self.st.viewport.in_gesture();
        rules::refresh(&mut self.st, rev, in_gesture);
    }

    /// The Rules pane's state (tests, `--rule`).
    pub fn rules_state(&self) -> &rules::RulesState {
        &self.st.rules
    }

    /// Whether a gesture or an agent batch is open: what the drain point
    /// waits for.
    fn busy(&self) -> bool {
        self.st.viewport.in_gesture()
            || self.commands.editing_ref().is_some_and(|e| e.in_gesture())
            || self
                .live
                .server
                .as_ref()
                .is_some_and(|s| s.open_batches() > 0)
    }

    /// Whether the "changed on disk, reload?" prompt is up (tests).
    pub fn reload_prompt_pending(&self) -> bool {
        self.reload_prompt
    }

    /// Queue entity definition files that changed on disk (as the watcher
    /// does; tests).
    pub fn queue_disk_entities(&mut self, rels: Vec<PathBuf>) {
        self.disk_entities.extend(rels);
    }

    /// At the drain point, unless a gesture or an agent batch is open: apply
    /// the entity definition files that changed on disk as one entry
    /// ("Disk: entities/door.json", origin disk, reported as `changed`),
    /// then mark those files clean, since disk already holds them. Undoing
    /// it marks them dirty again (a save then writes the old definition
    /// back). A file that doesn't load keeps the old definition and is
    /// reported in the status bar and the Problems pane.
    pub fn apply_disk_entities(&mut self) {
        if self.disk_entities.is_empty() || self.busy() {
            return;
        }
        let rels = std::mem::take(&mut self.disk_entities);
        let Some(doc) = &self.st.doc else {
            return;
        };
        // A queued file may have been edited since it was queued (a panel
        // edit or an agent request ran first this frame, or a gesture held
        // the queue): replacing it would lose that edit and `mark_clean`
        // would hide it. As at queueing, that is a conflict for the reload
        // prompt.
        if rels.iter().any(|r| doc.project.files.dirty().contains(r)) {
            tracing::info!("changed entity files have unsaved edits: asking to reload");
            self.reload_prompt = true;
            return;
        }
        let d = watch::disk_entity_command(doc, &rels);
        if let Some(p) = d.problems.first() {
            let msg = format!("{p}");
            self.st.view.set_status(msg.clone(), true);
            self.st.toast(msg, true);
        }
        // A file's load problems last until it loads; the live list keeps
        // them after its own.
        let before = self.problems.disk.clone();
        for r in &rels {
            self.problems
                .disk
                .remove(&r.to_string_lossy().replace('\\', "/"));
        }
        for p in &d.problems {
            self.problems
                .disk
                .entry(p.file.clone())
                .or_default()
                .push(p.clone());
        }
        if self.problems.disk != before
            && let Some(doc) = &mut self.st.doc
        {
            let disk: Vec<&worlds_core::Problem> = before.values().flatten().collect();
            doc.problems.retain(|p| !disk.contains(&p));
            doc.problems
                .extend(self.problems.disk.values().flatten().cloned());
            if let Some(s) = self.live.server.as_mut() {
                s.notify_problems(&doc.problems);
            }
        }
        let Some(cmd) = d.command else {
            return;
        };
        // The entry says it came from disk (the History pane, `changed`
        // events' `entry_origin` on undo).
        if let Some(e) = self.commands.editing() {
            e.set_next_origin(worlds_core::command::Origin::Disk);
        }
        match self.apply_command(cmd) {
            Ok(()) => {
                if let Some(doc) = &mut self.st.doc {
                    for f in &d.files {
                        doc.project.mark_clean(f);
                        // Disk holds this file now: the base an autosave
                        // compares against.
                        let h = std::fs::read(doc.dir.join(f))
                            .ok()
                            .map(|b| worlds_core::autosave::hash(&b));
                        doc.project.files.set_base(f.clone(), h);
                    }
                    if let Some(e) = self.commands.editing() {
                        e.sync_unsaved(&doc.project);
                    }
                }
                tracing::info!("applied {} changed entity files from disk", d.files.len());
            }
            Err(e) => self
                .st
                .toast(format!("Changed entity files didn't apply: {e}"), true),
        }
    }

    /// The entity tool was chosen (or T pressed again): the search box gets
    /// the keyboard, the Palette comes to the front, and a non-entities
    /// active layer gives way to the first entities layer.
    fn enter_entity_tool(&mut self) {
        // Not after Enter picked the brush: the keyboard stays on the map.
        self.st.entities.focus = !std::mem::take(&mut self.st.entities.keep_map_keys);
        show_pane(&mut self.dock, Pane::Palette);
        let Some(doc) = &self.st.doc else {
            return;
        };
        let on_entities = self
            .st
            .view
            .active_layer
            .and_then(|l| doc.project.layers.get(l))
            .is_some_and(|l| l.is_entities());
        if !on_entities {
            match crate::view::first_entities_layer(doc) {
                Some(l) => self.st.view.active_layer = Some(l),
                None => self
                    .st
                    .view
                    .set_status("The project has no entities layer to place on", true),
            }
        }
    }

    /// Enter with the entity tool (and the keyboard on the map): place the
    /// brush at the hovered cell, or in the middle of the active level when
    /// the pointer isn't over the map. The same events a click sends.
    fn place_entity_by_key(&mut self) {
        let Some(doc) = &self.st.doc else {
            return;
        };
        let Some(layer) = self
            .st
            .view
            .active_layer
            .filter(|&l| doc.project.layers.get(l).is_some_and(|d| d.is_entities()))
            .or_else(|| crate::view::first_entities_layer(doc))
        else {
            self.st
                .view
                .set_status("The project has no entities layer to place on", true);
            return;
        };
        let hover = self
            .st
            .viewport
            .tool_input
            .and_then(|t| t.hover)
            .filter(|h| h.layer == layer && self.st.viewport.hover.is_some());
        let at = match (hover, self.st.view.active_level) {
            (Some(h), _) => h,
            (None, Some(ix)) if ix < doc.levels.len() => {
                let def = &doc.project.layers[layer];
                let cells = doc.project.layer_cells(def, doc.level(ix));
                crate::script::cell_hit(
                    doc,
                    ix,
                    layer,
                    [(cells[0] / 2) as i32, (cells[1] / 2) as i32],
                )
            }
            _ => {
                self.st.view.set_status("Click a level first", true);
                return;
            }
        };
        self.st.view.active_level = Some(at.level);
        self.st.view.active_layer = Some(layer);
        let m = egui::Modifiers::NONE;
        self.st.tool_events.push(ToolEvent::Begin {
            tool: Tool::Entity,
            at,
            modifiers: m,
            grab: crate::tools::Grab::None,
        });
        self.st.tool_events.push(ToolEvent::End {
            tool: Tool::Entity,
            start: at,
            at,
            modifiers: m,
        });
    }

    /// Delete with a selection: remove what is selected
    /// ([`crate::annot::plan_delete`]), one entry. With levels among it,
    /// ask first ("Delete 2 levels and 3 notes?").
    fn delete_selection(&mut self) {
        let Some(doc) = &self.st.doc else {
            return;
        };
        // A highlighted point of the one selected annotation goes alone.
        if let Some(planned) = crate::annot::plan_vertex_delete(doc, &self.st.view) {
            match planned {
                Ok((cmd, label)) => {
                    self.st.view.vertex = None;
                    if let Err(e) = self.apply_command(Command::Batch {
                        label,
                        commands: vec![cmd],
                    }) {
                        self.st.view.set_status(e, true);
                    }
                }
                Err(e) => self.st.view.set_status(e, true),
            }
            return;
        }
        let plan = crate::annot::plan_delete(doc, &self.st.view);
        if let Some(why) = &plan.left_alone {
            self.st
                .view
                .set_status(why.clone(), plan.commands.is_empty());
        }
        if plan.commands.is_empty() {
            return;
        }
        if !plan.levels.is_empty() {
            self.confirm = Some(Confirm::DeleteSelection {
                summary: plan.summary,
                label: plan.label,
                commands: plan.commands,
            });
            return;
        }
        if let Err(e) = self.apply_command(Command::Batch {
            label: plan.label,
            commands: plan.commands,
        }) {
            self.st.view.set_status(e, true);
        }
    }

    /// Ctrl-D: copy the selection ([`crate::annot::plan_duplicate`]):
    /// entities and tile stacks one cell to the right, annotations one
    /// project cell, new ids; the copies become the selection. One entry.
    fn duplicate_selection(&mut self) {
        let Some(doc) = &self.st.doc else {
            return;
        };
        if self.st.view.selection.is_empty() {
            self.st
                .view
                .set_status("Nothing selected to duplicate", false);
            return;
        }
        let plan = crate::annot::plan_duplicate(doc, &self.st.view);
        if plan.commands.is_empty() {
            let msg = plan
                .message
                .unwrap_or_else(|| "Nothing to duplicate".into());
            self.st.view.set_status(msg, true);
            return;
        }
        match self.apply_command(Command::Batch {
            label: plan.label,
            commands: plan.commands,
        }) {
            Ok(()) => {
                let mut sel = plan.selection;
                // Level copies: selected, the first one active.
                if let Some(doc) = &self.st.doc {
                    let copies: Vec<crate::doc::LevelIx> = plan
                        .levels
                        .iter()
                        .filter_map(|s| doc.level_ix(s.as_str()))
                        .collect();
                    sel.extend(
                        copies
                            .iter()
                            .map(|&ix| crate::view::SelItem::Level(doc.ids[ix].clone())),
                    );
                    if let Some(&first) = copies.first() {
                        self.st.view.active_level = Some(first);
                    }
                }
                self.st.view.selection.set(sel);
                if let Some(m) = plan.message {
                    self.st.view.set_status(m, false);
                }
            }
            Err(e) => self.st.view.set_status(e, true),
        }
    }

    /// A level's Duplicate (its context menu, the level inspector): a copy
    /// just right of it under a fresh slug, which becomes the active level
    /// and the selection. One entry.
    fn duplicate_level(&mut self, ix: crate::doc::LevelIx) {
        let Some(doc) = self.st.doc.as_ref().filter(|d| ix < d.levels.len()) else {
            return;
        };
        let (cmd, slug) = crate::clipboard::duplicate_level_command(doc, ix, &[]);
        if let Err(e) = self.apply_command(cmd) {
            self.st.view.set_status(e, true);
            return;
        }
        if let Some(doc) = &self.st.doc
            && let Some(new) = doc.level_ix(slug.as_str())
        {
            self.st.view.active_level = Some(new);
            self.st
                .view
                .selection
                .set([crate::view::SelItem::Level(doc.ids[new].clone())]);
            self.st
                .view
                .set_status(format!("Duplicated as {slug}"), false);
        }
    }

    /// Ctrl+C (and the first half of Ctrl+X): the selection and the grid
    /// marquee as a fragment, to the system clipboard (as JSON text) and to
    /// the editor's own.
    fn copy(&mut self, ctx: Option<&egui::Context>) -> Option<crate::clipboard::Copied> {
        let doc = self.st.doc.as_ref()?;
        match crate::clipboard::copy_selection(doc, &self.st.view) {
            Ok(c) => {
                if let (Some(ctx), Ok(text)) = (ctx, serde_json::to_string(&c.fragment)) {
                    ctx.copy_text(text);
                }
                self.clipboard = Some(c.fragment.clone());
                self.st.view.set_status(c.message.clone(), false);
                Some(c)
            }
            Err(e) => {
                self.st.view.set_status(e, true);
                None
            }
        }
    }

    /// Ctrl+X: copy, then remove what was copied (one entry).
    fn cut(&mut self, ctx: Option<&egui::Context>) {
        let Some(doc) = self.st.doc.as_ref() else {
            return;
        };
        let (copied, commands, label) = match crate::clipboard::cut_selection(doc, &self.st.view) {
            Ok(x) => x,
            Err(e) => {
                self.st.view.set_status(e, true);
                return;
            }
        };
        if let (Some(ctx), Ok(text)) = (ctx, serde_json::to_string(&copied.fragment)) {
            ctx.copy_text(text);
        }
        self.clipboard = Some(copied.fragment);
        if commands.is_empty() {
            self.st.view.set_status(copied.message, false);
            return;
        }
        match self.apply_command(Command::Batch { label, commands }) {
            Ok(()) => {
                self.st.view.selection.clear();
                self.st.view.grid_marquee = None;
                self.st
                    .view
                    .set_status(copied.message.replacen("Copied", "Cut", 1), false);
            }
            Err(e) => self.st.view.set_status(e, true),
        }
    }

    /// Ctrl+V (or Ctrl+Shift+V, `in_place`): the fragment in `text` (the
    /// system clipboard), else the editor's own clipboard. A paste follows
    /// the pointer as a ghost until a click; in place it lands at once on
    /// its original cells.
    fn paste(&mut self, text: Option<String>, in_place: bool) {
        let frag = match text.as_deref().map(crate::clipboard::parse) {
            Some(Ok(Some(f))) => f,
            Some(Err(e)) => {
                self.st.view.set_status(e, true);
                return;
            }
            Some(Ok(None)) | None => match &self.clipboard {
                Some(f) if text.is_none() => f.clone(),
                _ => {
                    self.st
                        .view
                        .set_status("The clipboard holds no Worlds fragment", true);
                    return;
                }
            },
        };
        self.clipboard = Some(frag.clone());
        if in_place {
            let target = self
                .st
                .doc
                .as_ref()
                .and_then(|d| crate::clipboard::in_place_target(d, &self.st.view, &frag));
            match target {
                Some((ix, origin)) => self.paste_at(Some(frag), ix, origin),
                None => self.st.view.set_status("No level to paste in", true),
            }
            return;
        }
        self.st
            .viewport
            .cancel_gesture(self.st.view.tool, &mut self.st.tool_events);
        let n = frag.item_count();
        self.st.view.paste = Some(crate::clipboard::PasteGhost::new(frag));
        self.st.view.set_status(
            format!(
                "Paste {n} item{}: click to place; X, Y mirror and R rotates it; Escape cancels",
                if n == 1 { "" } else { "s" }
            ),
            false,
        );
    }

    /// Land a paste: `frag`, or the armed ghost's fragment, with its origin
    /// at `origin` in level `ix`. One entry; what it made becomes the
    /// selection and `ix` the active level; problems go to the status bar.
    fn paste_at(
        &mut self,
        frag: Option<worlds_core::fragment::Fragment>,
        ix: crate::doc::LevelIx,
        origin: [i64; 2],
    ) {
        let Some(doc) = self.st.doc.as_ref().filter(|d| ix < d.levels.len()) else {
            return;
        };
        let planned = match (&frag, &self.st.view.paste) {
            (Some(f), _) => crate::clipboard::paste_plan(doc, f, ix, origin),
            // The armed ghost, turned as it is (an assembly's is labelled
            // "Place assembly <slug>").
            (None, Some(p)) => p.plan(doc, ix, origin),
            (None, None) => return,
        };
        let plan = match planned {
            Ok(p) => p,
            Err(e) => {
                self.st.view.set_status(e, true);
                return;
            }
        };
        self.st.view.paste = None;
        if plan.commands.is_empty() {
            let why = plan.problems.join("; ");
            self.st
                .view
                .set_status(format!("Nothing pasted: {why}"), true);
            return;
        }
        let label = plan.label.clone();
        match self.apply_command(plan.command()) {
            Ok(()) => {
                if let Some(doc) = &self.st.doc {
                    let sel = crate::clipboard::pasted_selection(doc, &plan, ix);
                    self.st.view.selection.set(sel);
                }
                self.st.view.active_level = Some(ix);
                let msg = match plan.problems.as_slice() {
                    [] => label,
                    ps => format!("{label}; {}", ps.join("; ")),
                };
                self.st.view.set_status(msg, false);
            }
            Err(e) => self.st.view.set_status(e, true),
        }
    }

    /// Arrow keys: move the selection by a cell (entities and tile stacks
    /// of their layer, notes and levels of the project grid), or (Shift)
    /// entities, notes and levels by a pixel (tile stacks still a cell).
    /// One entry ([`crate::annot::plan_nudge`]).
    fn nudge_selection(&mut self, d: [i32; 2], pixel: bool) {
        let Some(doc) = &self.st.doc else {
            return;
        };
        let plan = match crate::annot::plan_nudge(doc, &self.st.view, d, pixel) {
            Ok(x) => x,
            Err(e) => {
                self.st.view.set_status(e, true);
                return;
            }
        };
        if let Some(why) = plan.left_alone {
            self.st.view.set_status(why, plan.commands.is_empty());
        }
        if plan.commands.is_empty() {
            return;
        }
        match self.apply_command(Command::Batch {
            label: plan.label,
            commands: plan.commands,
        }) {
            Ok(()) => self.st.view.selection.set(plan.selection),
            Err(e) => self.st.view.set_status(format!("Can't nudge: {e}"), true),
        }
    }

    fn with_commands(&mut self, f: impl FnOnce(&mut dyn EditorCommands, &mut CommandContext<'_>)) {
        let Some(doc) = self.st.doc.as_mut() else {
            return;
        };
        let mut cx = CommandContext {
            doc,
            view: &mut self.st.view,
            invalidate: &mut self.invalidations,
        };
        f(self.commands.as_mut(), &mut cx);
    }

    fn update_title(&mut self, ctx: &egui::Context) {
        let Some(doc) = &self.st.doc else {
            return;
        };
        let title = format!(
            "Worlds — {}{}",
            doc.project.name,
            if self.commands.has_unsaved() {
                " •"
            } else {
                ""
            }
        );
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }
    }

    fn apply_invalidations(&mut self) {
        if self.invalidations.is_empty() {
            return;
        }
        let tilesets = self.invalidations.contains(&Invalidation::Tilesets);
        if let Some(doc) = &self.st.doc {
            if tilesets {
                for i in 0..doc.project.tilesets.len() {
                    self.st.viewport.reload_tileset(doc, &self.gpu, i);
                }
                self.st.atlas.clear();
            }
            for inv in self.invalidations.drain(..) {
                self.st.viewport.invalidate(doc, &inv);
            }
        }
        self.invalidations.clear();
        // Tileset images may live in other directories now.
        if tilesets
            && !self.opts.headless
            && let Some(doc) = self.st.doc.take()
        {
            self.watcher = None;
            self.start_watcher(&doc);
            self.st.doc = Some(doc);
        }
    }

    fn poll_external(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.dialog
            && let Ok(res) = rx.try_recv()
        {
            self.dialog = None;
            if let Some(dir) = res {
                self.request_open(&dir);
            }
        }
        self.poll_import_pick();
        if let Some(doc) = &mut self.st.doc {
            for ix in doc.poll_bakes() {
                self.st.viewport.bake_arrived(ix);
            }
            let layers = doc.poll_layer_bakes();
            for (ix, layer) in &layers {
                self.st.viewport.invalidate(
                    doc,
                    &Invalidation::Layer {
                        level: *ix,
                        layer: layer.clone(),
                    },
                );
            }
            if let Some(e) = self.commands.editing() {
                e.adopt_layer_bakes(doc, &layers);
            }
        }
        let Some(w) = &mut self.watcher else {
            return;
        };
        let (changes, again) = w.poll();
        if let Some(d) = again {
            ctx.request_repaint_after(d);
        }
        for ch in changes {
            match ch {
                WatchChange::Project(paths) => {
                    // Our own save also lands here: nothing to do when every
                    // changed file holds what we wrote.
                    let foreign: Vec<PathBuf> = paths
                        .iter()
                        .filter(|p| !self.own_writes.explains(p))
                        .cloned()
                        .collect();
                    if foreign.is_empty() {
                        tracing::debug!("{} changed files are our own save", paths.len());
                        continue;
                    }
                    // Only entity definition files, none with unsaved edits:
                    // applied as an undoable edit at the drain point.
                    if let Some(doc) = &self.st.doc
                        && let Some(rels) = watch::entity_files(&doc.dir, &foreign)
                        && rels.iter().all(|r| !doc.project.files.dirty().contains(r))
                    {
                        for r in rels {
                            if !self.disk_entities.contains(&r) {
                                self.disk_entities.push(r);
                            }
                        }
                        continue;
                    }
                    if self.commands.has_unsaved() {
                        self.reload_prompt = true;
                    } else {
                        tracing::info!("project changed on disk, reloading");
                        self.reload();
                    }
                }
                WatchChange::Tileset(i) => self.tileset_changed(i),
                WatchChange::Image(p) => self.reference_image_changed(&p),
            }
        }
    }

    /// Tileset `i`'s PNG changed: re-upload the texture, and since the
    /// rules read tile opacity, rebake the auto layers that draw from it.
    /// The old bakes stay on screen until the new ones arrive; worker
    /// results from before the change are dropped.
    fn tileset_changed(&mut self, i: usize) {
        let Some(doc) = &mut self.st.doc else {
            return;
        };
        let Some(t) = doc.project.tilesets.get(i) else {
            return;
        };
        tracing::info!("tileset {} changed, reloading its image", t.slug.as_str());
        self.st.atlas.remove(t.slug.as_str());
        self.st.viewport.reload_tileset(doc, &self.gpu, i);
        let layers = doc.tileset_changed(i);
        if layers.is_empty() {
            return;
        }
        self.commands.tileset_changed(&layers);
        let ctx = self.ctx.clone();
        doc.start_bakes(move || ctx.request_repaint());
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        use egui::{Key, Modifiers};
        let mut acts = Vec::new();
        let on_map = !ctx.egui_wants_keyboard_input() && !self.palette.open;
        let turnable = self.turnable().is_some();
        ctx.input_mut(|i| {
            let cmd = Modifiers::COMMAND;
            if i.consume_key(cmd, Key::P) {
                acts.push(Action::CommandPalette);
            }
            if i.consume_key(cmd, Key::O) {
                acts.push(Action::OpenDialog);
            }
            if i.consume_key(cmd, Key::R) || i.consume_key(Modifiers::NONE, Key::F5) {
                acts.push(Action::Reload);
            }
            // Before Ctrl+S, which a held Shift would also match.
            if on_map && i.consume_key(cmd | Modifiers::SHIFT, Key::S) {
                acts.push(Action::OpenSaveAssembly);
            }
            if i.consume_key(cmd, Key::S) {
                acts.push(Action::Save);
            }
            if i.consume_key(cmd | Modifiers::SHIFT, Key::Z) || i.consume_key(cmd, Key::Y) {
                acts.push(Action::Redo);
            }
            if i.consume_key(cmd, Key::Z) {
                acts.push(Action::Undo);
            }
            if i.consume_key(cmd, Key::G) {
                acts.push(Action::ToggleGrid);
            }
            if i.consume_key(cmd, Key::F) {
                acts.push(Action::ShowFind);
            }
            for (n, k) in [
                Key::Num1,
                Key::Num2,
                Key::Num3,
                Key::Num4,
                Key::Num5,
                Key::Num6,
                Key::Num7,
                Key::Num8,
                Key::Num9,
            ]
            .into_iter()
            .enumerate()
            {
                if i.consume_key(cmd | Modifiers::SHIFT, k) {
                    acts.push(Action::JumpBookmark(n));
                }
            }
            if i.consume_key(cmd, Key::L) {
                acts.push(Action::ShowLevels);
            }
        });
        if !ctx.egui_wants_keyboard_input() && !self.palette.open {
            ctx.input_mut(|i| {
                use egui::Modifiers as M;
                // With a paste ghost or a tile stamp to turn, X and Y
                // mirror it and R rotates it (instead of the Rectangle
                // mode).
                if turnable {
                    for (key, op) in [
                        (Key::X, worlds_core::Flip::X),
                        (Key::Y, worlds_core::Flip::Y),
                        (Key::R, worlds_core::Flip::XT),
                    ] {
                        if i.consume_key(M::NONE, key) {
                            acts.push(Action::Turn(op));
                        }
                    }
                }
                for t in Tool::ALL {
                    if let Some(k) = t.key()
                        && i.consume_key(M::NONE, k)
                    {
                        acts.push(Action::SetTool(t));
                    }
                }
                if i.consume_key(M::NONE, Key::P) {
                    acts.push(Action::PaintTool);
                }
                if i.consume_key(M::SHIFT, Key::F) || i.consume_key(M::NONE, Key::Home) {
                    acts.push(Action::FitWorld);
                }
                if i.consume_key(M::NONE, Key::F) {
                    acts.push(Action::FitLevel);
                }
                if i.consume_key(M::NONE, Key::PageDown) {
                    acts.push(Action::NextLevel);
                }
                if i.consume_key(M::NONE, Key::PageUp) {
                    acts.push(Action::PrevLevel);
                }
                if i.consume_key(M::NONE, Key::Plus) || i.consume_key(M::NONE, Key::Equals) {
                    acts.push(Action::ZoomIn);
                }
                if i.consume_key(M::NONE, Key::Minus) {
                    acts.push(Action::ZoomOut);
                }
                if i.consume_key(M::NONE, Key::Num1) {
                    acts.push(Action::ZoomActual);
                }
                if i.consume_key(M::NONE, Key::Delete) {
                    acts.push(actions::delete_action(&self.st.view));
                }
                if i.consume_key(M::COMMAND, Key::D) {
                    acts.push(Action::DuplicateSelection);
                }
                if i.consume_key(M::COMMAND, Key::A) {
                    acts.push(Action::SelectAll);
                }
                // The clipboard keys arrive as events, not keys (the
                // windowing layer reads the system clipboard for a paste).
                for e in &i.events {
                    match e {
                        egui::Event::Copy => acts.push(Action::Copy),
                        egui::Event::Cut => acts.push(Action::Cut),
                        egui::Event::Paste(text) => acts.push(Action::Paste {
                            text: Some(text.clone()),
                            in_place: i.modifiers.shift,
                        }),
                        _ => {}
                    }
                }
                if !self.st.view.selection.is_empty() {
                    for (key, d) in [
                        (Key::ArrowLeft, [-1, 0]),
                        (Key::ArrowRight, [1, 0]),
                        (Key::ArrowUp, [0, -1]),
                        (Key::ArrowDown, [0, 1]),
                    ] {
                        if i.consume_key(M::NONE, key) {
                            acts.push(Action::Nudge { d, pixel: false });
                        }
                        if i.consume_key(M::SHIFT, key) {
                            acts.push(Action::Nudge { d, pixel: true });
                        }
                    }
                }
                if self.st.view.tool == Tool::Entity && i.consume_key(M::NONE, Key::Enter) {
                    acts.push(Action::PlaceEntity);
                }
                if self.st.view.drawing.as_ref().is_some_and(|d| !d.done) {
                    if i.consume_key(M::NONE, Key::Enter) {
                        acts.push(Action::FinishDrawing);
                    }
                    if i.consume_key(M::NONE, Key::Backspace) {
                        acts.push(Action::DrawingBackspace);
                    }
                }
            });
        }
        self.st.actions.extend(acts);
    }

    /// Perform an action.
    pub fn run(&mut self, a: Action) {
        match a {
            Action::OpenDialog => {
                if self.dialog.is_some() || self.opts.headless {
                    return;
                }
                let (tx, rx) = std::sync::mpsc::channel();
                let start = self.st.doc.as_ref().map(|d| d.dir.clone());
                let ctx = self.ctx.clone();
                std::thread::spawn(move || {
                    let mut d =
                        rfd::FileDialog::new().set_title("Open a .worlds project directory");
                    if let Some(s) = start.as_ref().and_then(|s| s.parent()) {
                        d = d.set_directory(s);
                    }
                    let _ = tx.send(d.pick_folder());
                    ctx.request_repaint();
                });
                self.dialog = Some(rx);
            }
            Action::Reload => self.reload(),
            Action::Save => {
                let _ = self.save();
            }
            Action::NewLevel(at) => self.open_new_level(at),
            Action::DeleteLevel => {
                let Some(doc) = &self.st.doc else {
                    return;
                };
                if let Some(ix) = self.st.view.active_level.filter(|&i| i < doc.levels.len()) {
                    self.confirm = Some(Confirm::DeleteLevel {
                        world: doc.project.worlds[doc.levels[ix].world].slug.clone(),
                        level: doc.level(ix).slug.clone(),
                    });
                }
            }
            Action::Undo => {
                self.flush_agent_batches();
                self.with_commands(|c, cx| c.undo(cx));
                self.apply_invalidations();
                self.report_user_edits();
            }
            Action::Redo => {
                self.flush_agent_batches();
                self.with_commands(|c, cx| c.redo(cx));
                self.apply_invalidations();
                self.report_user_edits();
            }
            Action::FitWorld => self.st.viewport.request_fit(Fit::World),
            Action::FitLevel => {
                if let Some(ix) = self.st.view.active_level {
                    self.st.viewport.request_fit(Fit::Level(ix));
                }
            }
            Action::ZoomIn => self.st.viewport.zoom_steps_centered(1),
            Action::ZoomOut => self.st.viewport.zoom_steps_centered(-1),
            Action::ZoomActual => self.st.viewport.set_zoom_centered(1.0),
            Action::SetTool(t) => {
                let old = self.st.view.tool;
                if old != t {
                    self.st
                        .viewport
                        .cancel_gesture(old, &mut self.st.tool_events);
                }
                self.st.view.set_tool(t);
                if t == Tool::Entity {
                    self.enter_entity_tool();
                }
            }
            Action::PaintTool => self.run(Action::SetTool(self.st.view.paint_tool())),
            Action::PlaceEntity => self.place_entity_by_key(),
            Action::Apply(cmd) => {
                if let Err(e) = self.apply_command(*cmd) {
                    self.st.view.set_status(e, true);
                }
            }
            Action::FinishDrawing => {
                let cmd = self
                    .st
                    .doc
                    .as_ref()
                    .and_then(|doc| crate::annot::finish_drawing(doc, &mut self.st.view));
                if let Some(c) = cmd
                    && let Err(e) = self.apply_command(c)
                {
                    self.st.view.set_status(e, true);
                }
            }
            Action::DrawingBackspace => {
                crate::annot::drawing_backspace(&mut self.st.view);
            }
            Action::ShowNotes => show_pane(&mut self.dock, Pane::Notes),
            Action::ShowFind => {
                show_pane(&mut self.dock, Pane::Find);
                self.st.find.focus_query = true;
            }
            Action::OpenProject(s) => {
                self.st.project.section = s;
                show_pane(&mut self.dock, Pane::Project);
            }
            Action::SwitchWorld(w) => self.switch_world(&w),
            Action::NewWorld => self.open_world_dialog(None),
            Action::ImportProject => self.pick_import_source(),
            Action::RenameWorld(w) => self.open_world_dialog(Some(w)),
            Action::DeleteWorld(w) => self.ask_delete_world(w),
            Action::MoveLevelToWorld { level, world } => self.move_level_to_world(level, world),
            Action::ShowHistory => show_pane(&mut self.dock, Pane::History),
            Action::HistoryJump(to) => self.history_jump(to),
            Action::DuplicateLayer(l) => self.duplicate_layer(l),
            Action::MergeLayerDown(l) => self.merge_layer_down(l),
            Action::MoveSelectionToLayer(l) => self.move_selection_to_layer(l),
            Action::AddReference(scope) => self.add_reference_dialog(scope),
            Action::ShowInspector => show_pane(&mut self.dock, Pane::Inspector),
            Action::NewViewport => {
                self.open_viewport(None, None, false);
            }
            Action::DuplicateViewport(id) => {
                let src = id.unwrap_or(self.st.view.focused);
                self.open_viewport(None, Some(src), false);
            }
            Action::CloseViewport(id) => {
                if let Err(e) = self.close_viewport(id) {
                    self.st.view.set_status(e, true);
                }
            }
            Action::LinkCameras(pair) => {
                if let Err(e) = self.link_cameras(pair) {
                    self.st.view.set_status(e, true);
                }
            }
            Action::SaveBookmark => {
                self.st.bookmarks_open = true;
                self.st.bookmark_focus = true;
            }
            Action::SaveBookmarkAs(name) => {
                if let Err(e) = self.save_bookmark(&name) {
                    self.st.view.set_status(e, true);
                }
            }
            Action::JumpBookmark(i) => {
                if let Err(e) = self.jump_bookmark(i) {
                    self.st.view.set_status(e, true);
                }
            }
            Action::DeleteBookmark(i) => {
                if i < self.st.view.bookmarks.len() {
                    let b = self.st.view.bookmarks.remove(i);
                    self.st
                        .view
                        .set_status(format!("Forgot bookmark {}", b.name), false);
                }
            }
            Action::ShowLevels => {
                show_pane(&mut self.dock, Pane::Levels);
                self.st.levels.focus_search = true;
            }
            Action::DeleteSelection => self.delete_selection(),
            Action::DuplicateSelection => self.duplicate_selection(),
            Action::DuplicateLevel(ix) => self.duplicate_level(ix),
            Action::Copy => {
                let ctx = self.ctx.clone();
                self.copy(Some(&ctx));
            }
            Action::Cut => {
                let ctx = self.ctx.clone();
                self.cut(Some(&ctx));
            }
            Action::Paste { text, in_place } => self.paste(text, in_place),
            Action::PasteAt { level, origin } => self.paste_at(None, level, origin),
            Action::Turn(op) => self.turn_brush(op),
            Action::TransformSelection(op) => self.transform_selection(op),
            Action::ArrangeLevels(op) => self.arrange_levels(op),
            Action::OpenSaveAssembly => self.open_save_assembly(),
            Action::SaveAssembly { info, replace } => self.save_assembly(info, replace),
            Action::ArmAssembly(slug) => self.arm_assembly(&slug),
            Action::ToggleLibrary => self.toggle_library(),
            Action::Nudge { d, pixel } => self.nudge_selection(d, pixel),
            Action::SelectAll => {
                if let (Some(doc), Some(ix)) = (&self.st.doc, self.st.view.active_level)
                    && ix < doc.levels.len()
                {
                    let ids = (0..doc.project.layers.len())
                        .filter(|&i| {
                            doc.project.layers[i].is_entities()
                                && self.st.view.layer_visible(i)
                                && !self.st.view.layer_locked(i)
                        })
                        .filter_map(|i| {
                            doc.level(ix)
                                .entities_layer(doc.project.layers[i].slug.as_str())
                        })
                        .flat_map(|l| l.entities.iter().map(|e| e.id.clone()))
                        .collect::<Vec<_>>();
                    self.st.view.selection.set_entities(ids);
                }
            }
            Action::OpenTypes(t) => {
                self.st.types.show(match t {
                    Some(t) => defs::TypesSel::Entity(t),
                    None => defs::TypesSel::LevelFields,
                });
                show_pane(&mut self.dock, Pane::EntityDefs);
            }
            Action::DuplicateType(t) => {
                if let Some((cmd, new)) = self
                    .st
                    .doc
                    .as_ref()
                    .and_then(|d| defs::duplicate_type(d, t.as_str()))
                {
                    match self.apply_command(cmd) {
                        Ok(()) => {
                            self.st.types.show(defs::TypesSel::Entity(new));
                            show_pane(&mut self.dock, Pane::EntityDefs);
                        }
                        Err(e) => self.st.toast(e, true),
                    }
                }
            }
            Action::ToggleGrid => self.st.view.show_grid = !self.st.view.show_grid,
            Action::NextLevel | Action::PrevLevel => {
                let Some(doc) = &self.st.doc else {
                    return;
                };
                // The levels of the world shown.
                let w = self.st.view.world_ix(doc);
                let shown: Vec<usize> = (0..doc.levels.len())
                    .filter(|&i| doc.levels[i].world == w)
                    .collect();
                let n = shown.len();
                if n == 0 {
                    return;
                }
                let cur = self
                    .st
                    .view
                    .active_level
                    .and_then(|ix| shown.iter().position(|&i| i == ix));
                let next = shown[match (cur, a == Action::NextLevel) {
                    (None, true) => 0,
                    (None, false) => n - 1,
                    (Some(i), true) => (i + 1) % n,
                    (Some(i), false) => (i + n - 1) % n,
                }];
                self.st.view.active_level = Some(next);
                self.st.viewport.request_fit(Fit::Level(next));
            }
            Action::ResetLayout => self.dock = default_dock(),
            Action::ClearLog => self.st.log.clear(),
            Action::CropLevel => {
                let Some(doc) = &self.st.doc else {
                    return;
                };
                let Some(ix) = self.st.view.active_level.filter(|&i| i < doc.levels.len()) else {
                    self.st.view.set_status("No active level to crop", true);
                    return;
                };
                let world = &doc.project.worlds[doc.levels[ix].world].slug;
                match crate::tools::crop_command(&doc.project, world, &doc.level(ix).slug) {
                    Ok(cmd) => {
                        if let Err(e) = self.apply_command(cmd) {
                            self.st.view.set_status(e, true);
                        }
                    }
                    Err(e) => self.st.view.set_status(e, true),
                }
            }
            Action::CommandPalette => {
                self.palette.open = !self.palette.open;
                self.palette.query.clear();
                self.palette.selected = 0;
                self.palette.just_opened = true;
            }
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        /// The agent indicator's least room: its dot and Approve toggle.
        const INDICATOR_MIN_W: f32 = 110.0;
        // The agent indicator first, at the right end, in the room the tools
        // leave (as wide as they were last frame), but always room for its
        // dot and the Approve toggle; it collapses to fit. The tools get the
        // rest, clipped there, so a narrow window never draws one over the
        // other.
        let row = {
            let r = ui.available_rect_before_wrap();
            egui::Rect::from_min_size(r.min, egui::vec2(r.width(), ui.spacing().interact_size.y))
        };
        let gap = ui.spacing().item_spacing.x * 2.0;
        let budget = if self.tools_w > 0.0 {
            row.width() - self.tools_w - gap
        } else {
            row.width() * 0.45
        }
        .max(INDICATOR_MIN_W)
        .min(row.width());
        let right = egui::Rect::from_min_max(egui::pos2(row.right() - budget, row.top()), row.max);
        let indicator = ui
            .scope_builder(
                egui::UiBuilder::new()
                    .max_rect(right)
                    .layout(egui::Layout::right_to_left(egui::Align::Center)),
                |ui| self.agent_indicator(ui, budget),
            )
            .response
            .rect;
        let used = indicator.width();
        let left = egui::Rect::from_min_max(
            row.min,
            egui::pos2((row.right() - used - gap).max(row.left()), row.bottom()),
        );
        self.tools_w = ui
            .scope_builder(
                egui::UiBuilder::new()
                    .max_rect(left)
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
                |ui| {
                    ui.set_clip_rect(left.intersect(ui.clip_rect()));
                    self.toolbar_tools(ui);
                },
            )
            .response
            .rect
            .width();
        self.toolbar_split = Some((left, indicator));
        ui.advance_cursor_after_rect(row);
    }

    /// Last frame's toolbar split: the tools' (clipped) area and the agent
    /// indicator's rect. They never overlap.
    pub fn toolbar_split(&self) -> Option<(egui::Rect, egui::Rect)> {
        self.toolbar_split
    }

    /// The left part of the toolbar: tools, undo/redo/save, open, commands.
    fn toolbar_tools(&mut self, ui: &mut egui::Ui) {
        {
            ui.strong("Worlds");
            ui.separator();
            if self.world_switcher(ui) {
                self.bookmarks_button(ui);
                ui.separator();
            }
            for b in ToolButton::ALL {
                let sel = b.shows(self.st.view.tool);
                let (name, tip, act) = match b {
                    ToolButton::Paint => (
                        "Paint",
                        format!(
                            "Paint (P): {} now; Brush (B), Line (L), Rectangle (R), Fill (G)",
                            self.st.view.paint_tool().name()
                        ),
                        Action::PaintTool,
                    ),
                    ToolButton::Tool(Tool::Hand) => (
                        "Hand",
                        "Hand: drag to pan (hold Space for it with any tool)".into(),
                        Action::SetTool(Tool::Hand),
                    ),
                    ToolButton::Tool(t) => (
                        t.name(),
                        format!("{} ({})", t.name(), t.shortcut()),
                        Action::SetTool(t),
                    ),
                };
                if ui
                    .add(egui::Button::selectable(sel, name))
                    .on_hover_text(tip)
                    .clicked()
                {
                    self.st.actions.push(act);
                }
            }
            ui.separator();
            let undo = self.commands.undo_label();
            let redo = self.commands.redo_label();
            if ui
                .add_enabled(self.commands.can_undo(), egui::Button::new("Undo"))
                .on_hover_text(format!(
                    "Undo {} (Ctrl+Z)",
                    undo.as_deref().unwrap_or("the stroke in progress")
                ))
                .on_disabled_hover_text("Nothing to undo")
                .clicked()
            {
                self.st.actions.push(Action::Undo);
            }
            if ui
                .add_enabled(self.commands.can_redo(), egui::Button::new("Redo"))
                .on_hover_text(format!(
                    "Redo {} (Ctrl+Shift+Z)",
                    redo.as_deref().unwrap_or_default()
                ))
                .on_disabled_hover_text("Nothing to redo")
                .clicked()
            {
                self.st.actions.push(Action::Redo);
            }
            let dirty = self.commands.has_unsaved();
            if ui
                .add_enabled(
                    dirty,
                    egui::Button::new(if dirty { "Save •" } else { "Save" }),
                )
                .on_hover_text("Unsaved changes (Ctrl+S)")
                .on_disabled_hover_text("No unsaved changes")
                .clicked()
            {
                self.st.actions.push(Action::Save);
            }
            ui.separator();
            if ui.button("Open…").on_hover_text("Ctrl+O").clicked() {
                self.st.actions.push(Action::OpenDialog);
            }
            if ui
                .add_enabled(self.st.doc.is_some(), egui::Button::new("Import…"))
                .on_hover_text(
                    "Import from Worlds project…: definitions, worlds or levels of another project",
                )
                .clicked()
            {
                self.st.actions.push(Action::ImportProject);
            }
            if ui.button("Commands").on_hover_text("Ctrl+P").clicked() {
                self.st.actions.push(Action::CommandPalette);
            }
        }
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        let now = Instant::now();
        if self.st.view.status.as_ref().is_some_and(|m| m.until <= now) {
            self.st.view.status = None;
        }
        if let Some(m) = &self.st.view.status {
            ui.ctx()
                .request_repaint_after(m.until.saturating_duration_since(now));
        }
        ui.horizontal(|ui| {
            self.autosave_indicator(ui);
            if let Some(m) = &self.st.view.status {
                let c = if m.error {
                    Color32::from_rgb(230, 150, 90)
                } else {
                    self.st.accent
                };
                ui.label(RichText::new(&m.text).color(c));
                ui.separator();
            }
            // The tab under the pointer's readout.
            let (hover, readout, stats, zoom) = self.st.pointer_hover();
            match hover {
                Some(h) => {
                    if let Some(l) = &h.level {
                        ui.strong(l);
                    }
                    if let Some(c) = h.cell {
                        ui.monospace(format!("cell {}, {}", c[0], c[1]));
                    }
                    ui.weak(format!("px {:.0}, {:.0}", h.world_px[0], h.world_px[1]));
                    if !h.what.is_empty() {
                        ui.label(&h.what);
                    }
                    if let Some(r) = readout {
                        ui.label(RichText::new(r).color(self.st.accent));
                    }
                    if let Some(fired) = self.fired_under_pointer() {
                        ui.label(RichText::new(fired).color(self.st.accent));
                    }
                }
                None => {
                    ui.weak("—");
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let s = stats;
                ui.weak(format!(
                    "{:.1} ms · {} chunks · {} draws · {} tiles",
                    self.frame_ms, s.chunks, s.draws, s.instances
                ));
                ui.label(crate::viewport::camera::zoom_label(zoom));
                if let Some(doc) = &self.st.doc
                    && doc.bakes_pending > 0
                {
                    ui.label(
                        RichText::new(format!(
                            "baking {}/{}",
                            doc.levels.len() - doc.bakes_pending,
                            doc.levels.len()
                        ))
                        .color(self.st.accent),
                    );
                } else if let Some(doc) = &self.st.doc
                    && doc.layer_jobs_pending > 0
                {
                    ui.label(
                        RichText::new(format!("rebaking {} layers", doc.layer_jobs_pending))
                            .color(self.st.accent),
                    );
                }
            });
        });
    }

    /// On an auto layer: the rules that fired at the hovered cell of the
    /// active level, from the command layer's cache (`placements_at`), else
    /// the level's bake.
    fn fired_under_pointer(&self) -> Option<String> {
        let doc = self.st.doc.as_ref()?;
        let layer = self.st.view.active_layer?;
        let def = doc.project.layers.get(layer).filter(|d| d.is_auto())?;
        let h = self.st.pointer_hover().0?;
        let cell = h.cell?;
        let ix = doc.level_ix(h.level.as_deref()?)?;
        let level = doc.level(ix).slug.as_str();
        let mut rules: Vec<String> = Vec::new();
        let cached = self.commands.editing_ref().and_then(|e| {
            let c = e.cache();
            c.get(level, def.slug.as_str())?;
            Some(c.placements_at(level, def.slug.as_str(), cell))
        });
        match cached {
            Some(ps) => {
                for p in ps {
                    if !rules.iter().any(|r| *r == p.rule.as_str()) {
                        rules.push(p.rule.to_string());
                    }
                }
            }
            None => {
                let bake = doc.bakes[ix].as_ref()?.get(&def.slug)?;
                rules.extend(bake.fired.get(&cell)?.iter().map(|s| s.to_string()));
            }
        }
        (!rules.is_empty()).then(|| format!("fired: {}", rules.join(", ")))
    }

    fn command_palette(&mut self, ctx: &egui::Context) {
        if !self.palette.open {
            return;
        }
        let mut catalog = Action::catalog();
        catalog.extend(actions::bookmark_entries(&self.st.view.bookmarks));
        let mut hits: Vec<(i32, usize)> = catalog
            .iter()
            .enumerate()
            .filter_map(|(i, (name, _, _))| fuzzy_score(&self.palette.query, name).map(|s| (s, i)))
            .collect();
        if !self.palette.query.is_empty() {
            hits.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        }
        let (up, down, enter, esc) = ctx.input_mut(|i| {
            (
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
                i.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
                i.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
            )
        });
        if down {
            self.palette.selected = (self.palette.selected + 1).min(hits.len().saturating_sub(1));
        }
        if up {
            self.palette.selected = self.palette.selected.saturating_sub(1);
        }
        let mut chosen = None;
        egui::Window::new("Commands")
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .fixed_size([420.0, 0.0])
            .anchor(egui::Align2::CENTER_TOP, [0.0, 64.0])
            .show(ctx, |ui| {
                let r = ui.add(
                    egui::TextEdit::singleline(&mut self.palette.query)
                        .hint_text("Type a command…")
                        .desired_width(f32::INFINITY),
                );
                if self.palette.just_opened {
                    r.request_focus();
                    self.palette.just_opened = false;
                }
                if r.changed() {
                    self.palette.selected = 0;
                }
                ui.separator();
                egui::ScrollArea::vertical()
                    .max_height(320.0)
                    .show(ui, |ui| {
                        for (row, &(_, i)) in hits.iter().enumerate() {
                            let (name, key, _) = &catalog[i];
                            let sel = row == self.palette.selected;
                            let resp = ui.horizontal(|ui| {
                                let r = ui.selectable_label(sel, name);
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        ui.weak(*key);
                                    },
                                );
                                r
                            });
                            if sel && (up || down) {
                                resp.inner.scroll_to_me(None);
                            }
                            if resp.inner.clicked() {
                                chosen = Some(i);
                            }
                        }
                        if hits.is_empty() {
                            ui.weak("No matching command");
                        }
                    });
            });
        if enter && let Some(&(_, i)) = hits.get(self.palette.selected) {
            chosen = Some(i);
        }
        if let Some(i) = chosen {
            self.palette.open = false;
            let a = catalog[i].2.clone();
            if a != Action::CommandPalette {
                self.st.actions.push(a);
            }
        } else if esc {
            self.palette.open = false;
        }
    }

    /// The topmost open modal prompt.
    fn top_modal(&self) -> Option<Modal> {
        if self.recovery.is_some() {
            Some(Modal::Recovery)
        } else if self.close_prompt {
            Some(Modal::Close)
        } else if self.open_prompt.is_some() {
            Some(Modal::Open)
        } else if self.reload_prompt {
            Some(Modal::Reload)
        } else if self.confirm.is_some() {
            Some(Modal::Confirm)
        } else if self.new_level.is_some() {
            Some(Modal::NewLevel)
        } else if self.world_dialog.is_some() {
            Some(Modal::World)
        } else if self.import.is_some() {
            Some(Modal::Import)
        } else {
            None
        }
    }

    fn reload_prompt(&mut self, ctx: &egui::Context) {
        if !self.reload_prompt {
            return;
        }
        let (_, escape) = self.modal_keys.for_modal(Modal::Reload);
        egui::Window::new("Project changed on disk")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label("Files changed outside the editor, and you have unsaved edits.");
                ui.label("Reloading discards your edits. Keeping them means your next save overwrites the files that changed.");
                ui.horizontal(|ui| {
                    let keep = ui.button("Keep mine");
                    if !keep.has_focus() && !ui.memory(|m| m.focused().is_some()) {
                        keep.request_focus();
                    }
                    if keep.clicked() || escape {
                        self.reload_prompt = false;
                    }
                    if ui.button("Reload (discard my edits)").clicked() {
                        self.reload_prompt = false;
                        self.reload();
                    }
                });
            });
    }

    fn close_prompt(&mut self, ctx: &egui::Context) {
        if !self.close_prompt {
            return;
        }
        let (_, escape) = self.modal_keys.for_modal(Modal::Close);
        egui::Window::new("Unsaved changes")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label("Save your edits before closing?");
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() {
                        self.close_prompt = false;
                        if self.save().is_ok() {
                            self.allow_close = true;
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    }
                    if ui.button("Discard").clicked() {
                        self.discard_and_close();
                    }
                    if ui.button("Cancel").clicked() || escape {
                        self.close_prompt = false;
                    }
                });
            });
    }

    fn open_prompt(&mut self, ctx: &egui::Context) {
        let Some(dir) = self.open_prompt.clone() else {
            return;
        };
        let (_, escape) = self.modal_keys.for_modal(Modal::Open);
        let mut choice = None;
        egui::Window::new("Unsaved changes")
            .id(egui::Id::new("open prompt"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(format!("Save your edits before opening {}?", dir.display()));
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() {
                        choice = Some(OpenChoice::Save);
                    }
                    if ui.button("Discard").clicked() {
                        choice = Some(OpenChoice::Discard);
                    }
                    if ui.button("Cancel").clicked() || escape {
                        choice = Some(OpenChoice::Cancel);
                    }
                });
            });
        if let Some(c) = choice {
            self.open_prompt_choose(c);
        }
    }

    /// The close prompt's Discard: close without saving. The edits are
    /// thrown away on purpose, so their autosave goes too.
    pub fn discard_and_close(&mut self) {
        self.close_prompt = false;
        self.allow_close = true;
        self.autosaver.clear();
        self.ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    /// Open the New Level dialog for a level at world pixel `at` (default:
    /// the middle of the viewport), snapped to the grid.
    fn open_new_level(&mut self, at: Option<[f64; 2]>) {
        let Some(doc) = &self.st.doc else {
            return;
        };
        let Some(world) = doc
            .project
            .worlds
            .get(self.st.view.world_ix(doc))
            .map(|w| w.slug.clone())
        else {
            self.st
                .toast("The project has no world to add a level to", true);
            return;
        };
        let vp = &self.st.viewport;
        let at = at.unwrap_or_else(|| {
            vp.camera
                .screen_to_world([vp.size_px[0] * 0.5, vp.size_px[1] * 0.5])
        });
        // On the project grid, or in a grid-vania world on the lattice
        // cell under the point.
        let w = &doc.project.worlds[self.st.view.world_ix(doc)];
        let lattice = w.layout.lattice(doc.project.grid_size);
        let gx = lattice.map_or(doc.project.grid_size, |c| c[0]).max(1) as f64;
        let gy = lattice.map_or(doc.project.grid_size, |c| c[1]).max(1) as f64;
        let snap = |v: f64, g: f64| ((v / g).floor() * g) as i64;
        let mut n = doc.levels.len() + 1;
        let slug = loop {
            let s = format!("level_{n}");
            if doc.level_ix(&s).is_none() {
                break s;
            }
            n += 1;
        };
        self.new_level = Some(NewLevelDialog {
            world,
            at: [snap(at[0], gx), snap(at[1], gy)],
            slug,
            error: None,
            just_opened: true,
        });
    }

    fn new_level_dialog(&mut self, ctx: &egui::Context) {
        let Some(d) = &mut self.new_level else {
            return;
        };
        let Some(doc) = &self.st.doc else {
            self.new_level = None;
            return;
        };
        let size = default_level_size(&doc.project);
        let (enter, escape) = self.modal_keys.for_modal(Modal::NewLevel);
        let mut create = enter;
        let mut cancel = escape;
        egui::Window::new("New level")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_TOP, [0.0, 64.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Slug");
                    let r = ui.text_edit_singleline(&mut d.slug);
                    if d.just_opened {
                        r.request_focus();
                        d.just_opened = false;
                    }
                    if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        create = true;
                    }
                });
                ui.weak(format!(
                    "{} x {} cells at {}, {} in world {}",
                    size[0], size[1], d.at[0], d.at[1], d.world
                ));
                if let Some(e) = &d.error {
                    ui.colored_label(Color32::from_rgb(230, 120, 120), e);
                }
                ui.horizontal(|ui| {
                    create |= ui.button("Create").clicked();
                    cancel |= ui.button("Cancel").clicked();
                });
            });
        if cancel {
            self.new_level = None;
            return;
        }
        if !create {
            return;
        }
        let slug = match Slug::new(d.slug.trim()) {
            Ok(s) if doc.level_ix(s.as_str()).is_none() => s,
            Ok(_) => {
                d.error = Some(format!("A level {:?} already exists", d.slug.trim()));
                return;
            }
            Err(e) => {
                d.error = Some(e.to_string());
                return;
            }
        };
        // A grid-vania world's levels are whole lattice cells.
        let size = match doc
            .project
            .world(d.world.as_str())
            .and_then(|w| w.layout.lattice(doc.project.grid_size))
        {
            Some(c) if c[0] % doc.project.grid_size == 0 && c[1] % doc.project.grid_size == 0 => {
                let g = doc.project.grid_size.max(1);
                let (cw, ch) = (c[0] / g, c[1] / g);
                [size[0].div_ceil(cw) * cw, size[1].div_ceil(ch) * ch]
            }
            _ => size,
        };
        let mut level = Level::new(slug.clone(), slug.as_str(), size);
        level.world_px = d.at;
        let cmd = Command::CreateLevel {
            world: d.world.clone(),
            level,
            content: Default::default(),
            restore: false,
        };
        match self.apply_command(cmd) {
            Ok(()) => {
                self.new_level = None;
                if let Some(doc) = &self.st.doc {
                    self.st.view.active_level = doc.level_ix(slug.as_str());
                }
            }
            Err(e) => {
                if let Some(d) = &mut self.new_level {
                    d.error = Some(e);
                }
            }
        }
    }

    fn confirm(&mut self, ctx: &egui::Context) {
        let Some(c) = &self.confirm else {
            return;
        };
        let (question, cmd) = c.question_and_command();
        let (enter, escape) = self.modal_keys.for_modal(Modal::Confirm);
        let mut answer = match (enter, escape) {
            (_, true) => Some(false),
            (true, _) => Some(true),
            _ => None,
        };
        egui::Area::new(egui::Id::new("confirm"))
            .anchor(egui::Align2::RIGHT_BOTTOM, [-16.0, -40.0])
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style())
                    .stroke(egui::Stroke::new(1.0, Color32::from_rgb(220, 90, 90)))
                    .show(ui, |ui| {
                        ui.label(&question);
                        ui.horizontal(|ui| {
                            if ui.button("Delete").clicked() {
                                answer = Some(true);
                            }
                            if ui.button("Cancel").clicked() {
                                answer = Some(false);
                            }
                        });
                    });
            });
        match answer {
            Some(true) => {
                self.confirm = None;
                if let Err(e) = self.apply_command(cmd) {
                    self.st.toast(e, true);
                }
            }
            Some(false) => self.confirm = None,
            None => {}
        }
    }

    /// The toasts, stacked upward from just above the status bar, newest at
    /// the bottom. Every toast is one line of the same height (long text is
    /// cut, the whole of it on hover and in the log), so the stack is laid
    /// out from the style alone: no position depends on a size measured
    /// last frame, and a toast is never drawn where the status bar or the
    /// toolbar is. At most [`SHOWN_TOASTS`] are drawn, fewer when the window
    /// is short; older ones are summed up in a "+N more" line on top.
    fn toasts(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        self.st.toasts.retain(|t| t.until > now);
        self.toast_rects.clear();
        if self.st.toasts.is_empty() {
            return;
        }
        let bounds = self
            .toast_bounds
            .unwrap_or_else(|| ctx.content_rect())
            .shrink(8.0);
        let style = ctx.global_style();
        let base = egui::Frame::popup(&style);
        let margin = base.total_margin();
        let font = egui::TextStyle::Body.resolve(&style);
        let text_h = ctx.fonts_mut(|f| f.row_height(&font));
        let row_h = style
            .spacing
            .interact_size
            .y
            .max(text_h + 2.0 * style.spacing.button_padding.y)
            .ceil();
        let h = row_h + margin.sum().y;
        let w = 440.0_f32.min(bounds.width());
        let gap = 6.0;
        // Rows that fit between the toolbar and the status bar; with more
        // toasts than rows (or than SHOWN_TOASTS), the top row is "+N more".
        let rows = ((bounds.height() + gap) / (h + gap)).floor().max(0.0) as usize;
        let total = self.st.toasts.len();
        let shown = if total <= rows.min(SHOWN_TOASTS) {
            total
        } else {
            rows.saturating_sub(1).min(SHOWN_TOASTS)
        };
        let hidden = total - shown;
        let mut bottom = bounds.bottom();
        let mut undo = None;
        for t in self.st.toasts.iter().rev().take(shown) {
            let on_top = t.undo.as_ref().is_some_and(|u| self.agent_entry_on_top(u));
            let frame = base.stroke(egui::Stroke::new(
                1.0,
                if t.error {
                    Color32::from_rgb(220, 90, 90)
                } else {
                    self.st.accent
                },
            ));
            let r = self.toast_area(
                ctx,
                egui::Id::new(("toast", t.id)),
                frame,
                [bounds.right() - w, bottom - h],
                [w, row_h],
                |ui| {
                    if let Some(u) = &t.undo {
                        let r = ui
                            .add_enabled(on_top, egui::Button::new("Undo"))
                            .on_hover_text("Undo this agent batch")
                            .on_disabled_hover_text(
                                "Not the last edit any more: undo what came after it first",
                            );
                        if r.clicked() {
                            undo = Some(u.clone());
                        }
                    }
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.add(egui::Label::new(&t.text).truncate());
                    });
                },
            );
            self.toast_rects.extend(r);
            bottom -= h + gap;
        }
        if hidden > 0 && rows > 0 {
            let r = self.toast_area(
                ctx,
                egui::Id::new("toast more"),
                base,
                [bounds.right() - w, bottom - h],
                [w, row_h],
                |ui| {
                    ui.weak(format!("+{hidden} more"));
                },
            );
            self.toast_rects.extend(r);
        }
        if let Some(u) = undo {
            self.undo_agent_entry(&u);
        }
        if let Some(next) = self.st.toasts.iter().map(|t| t.until).min() {
            ctx.request_repaint_after(next.saturating_duration_since(now));
        }
    }

    /// One toast row: an area at `pos` (top left) holding `frame` with an
    /// inner size of exactly `[w - margins, row_h]`, laid out right to left.
    /// Returns where it was drawn; `None` the first time, when egui lays a
    /// new area out invisibly (and repaints at once to show it).
    fn toast_area(
        &self,
        ctx: &egui::Context,
        id: egui::Id,
        frame: egui::Frame,
        pos: [f32; 2],
        [w, row_h]: [f32; 2],
        add: impl FnOnce(&mut egui::Ui),
    ) -> Option<egui::Rect> {
        let inner_w = w - frame.total_margin().sum().x;
        let seen = ctx.memory(|m| m.area_rect(id)).is_some();
        if !seen {
            ctx.request_repaint();
        }
        let rect = egui::Area::new(id)
            .fixed_pos(pos)
            .order(egui::Order::Foreground)
            .fade_in(false)
            .show(ctx, |ui| {
                frame.show(ui, |ui| {
                    ui.set_min_size(egui::vec2(inner_w, row_h));
                    ui.set_max_size(egui::vec2(inner_w, row_h));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), add);
                });
            })
            .response
            .rect;
        seen.then_some(rect)
    }

    /// The toasts drawn last frame (screen points), newest first, then the
    /// "+N more" line if any; and the rect they must stay in (between the
    /// toolbar and the status bar).
    pub fn toast_rects(&self) -> (Vec<egui::Rect>, Option<egui::Rect>) {
        (self.toast_rects.clone(), self.toast_bounds)
    }
}

/// Size of a new level: the project's `default_level_size`, else the most
/// common level size, else 32 x 18.
pub fn default_level_size(p: &worlds_core::Project) -> [u32; 2] {
    if let Some(s) = p.default_level_size {
        return s;
    }
    let mut counts: BTreeMap<[u32; 2], usize> = BTreeMap::new();
    for l in p.levels() {
        *counts.entry(l.size).or_default() += 1;
    }
    counts
        .into_iter()
        .max_by_key(|&(s, n)| (n, std::cmp::Reverse(s)))
        .map_or([32, 18], |(s, _)| s)
}

struct Tabs<'a> {
    st: &'a mut EditorState,
    gpu: &'a Gpu,
}

impl TabViewer for Tabs<'_> {
    type Tab = Pane;

    fn id(&mut self, tab: &mut Pane) -> egui::Id {
        egui::Id::new(("worlds pane", *tab))
    }

    fn title(&mut self, tab: &mut Pane) -> egui::WidgetText {
        match tab {
            Pane::Viewport(id) => viewports::tab_title(self.st, *id).into(),
            Pane::Layers => "Layers".into(),
            Pane::Palette => "Palette".into(),
            Pane::Inspector => "Inspector".into(),
            Pane::Problems => {
                let n = self.st.doc.as_ref().map_or(0, |d| d.problems.len());
                if n == 0 {
                    "Problems".into()
                } else {
                    format!("Problems ({n})").into()
                }
            }
            Pane::Log => "Log".into(),
            Pane::Rules => "Rules".into(),
            Pane::EntityDefs => "Types".into(),
            Pane::Notes => "Notes".into(),
            Pane::Project => "Project".into(),
            Pane::Find => "Find".into(),
            Pane::History => "History".into(),
            Pane::Levels => "Levels".into(),
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut Pane) {
        self.st.pane_rects.insert(*tab, ui.max_rect());
        match tab {
            Pane::Viewport(id) => {
                let id = *id;
                let st = &mut *self.st;
                let rect = ui.max_rect();
                // A press in a tab focuses it before it draws, so the
                // gesture and the keys after it are this tab's.
                let pressed_here = ui.input(|i| {
                    i.pointer.any_pressed()
                        && i.pointer.press_origin().is_some_and(|p| rect.contains(p))
                }) && ui
                    .input(|i| i.pointer.press_origin())
                    .and_then(|p| ui.ctx().layer_id_at(p))
                    == Some(ui.layer_id());
                if pressed_here && st.view.focused != id {
                    st.focus_tab(id);
                }
                let focused = st.view.focused == id;
                st.swap_live(id);
                if focused {
                    st.map_rect = Some(rect);
                }
                let mut cx = ViewportCx {
                    doc: st.doc.as_ref(),
                    gpu: self.gpu,
                    view: &mut st.view,
                    events: &mut st.tool_events,
                    actions: &mut st.actions,
                    accent: st.accent,
                    ghosts: &st.ghosts,
                    highlight: st.rules.highlight.as_ref(),
                    atlas: Some(&st.atlas),
                    focused,
                };
                st.viewport.ui(ui, &mut cx);
                let f = st.view.focused;
                st.swap_live(f);
            }
            Pane::Layers => panels::layers(ui, self.st),
            Pane::Palette => panels::palette(ui, self.st),
            Pane::Inspector => inspector::inspector(ui, self.st),
            Pane::Problems => panels::problems(ui, self.st),
            Pane::Log => panels::log(ui, self.st),
            Pane::Rules => rules::pane(ui, self.st),
            Pane::EntityDefs => defs::pane(ui, self.st),
            Pane::Notes => notes::pane(ui, self.st),
            Pane::Project => project_pane::pane(ui, self.st),
            Pane::Find => find::pane(ui, self.st),
            Pane::History => history::pane(ui, self.st),
            Pane::Levels => levels::pane(ui, self.st),
        }
    }

    fn tab_style_override(
        &self,
        tab: &Pane,
        global: &egui_dock::TabStyle,
    ) -> Option<egui_dock::TabStyle> {
        matches!(tab, Pane::Viewport(_)).then(|| {
            let mut s = global.clone();
            s.tab_body.inner_margin = egui::Margin::ZERO;
            s
        })
    }

    /// Map tabs close while there is another; other panes never.
    fn is_closeable(&self, tab: &Pane) -> bool {
        matches!(tab, Pane::Viewport(_)) && self.st.view.tabs.len() > 1
    }

    fn on_close(&mut self, tab: &mut Pane) -> egui_dock::widgets::tab_viewer::OnCloseResponse {
        use egui_dock::widgets::tab_viewer::OnCloseResponse;
        match tab {
            Pane::Viewport(id) => match self.st.view.close_tab(&mut self.st.viewport, *id) {
                Ok(()) => OnCloseResponse::Close,
                Err(e) => {
                    self.st.view.set_status(e, true);
                    OnCloseResponse::Ignore
                }
            },
            _ => OnCloseResponse::Ignore,
        }
    }

    fn on_tab_button(&mut self, tab: &mut Pane, response: &egui::Response) {
        if let Pane::Viewport(id) = tab
            && response.clicked()
        {
            self.st.focus_tab(*id);
        }
    }

    /// A map tab's right-click menu (egui_dock adds Eject and Close).
    fn context_menu(&mut self, ui: &mut egui::Ui, tab: &mut Pane, _path: egui_dock::NodePath) {
        let Pane::Viewport(id) = *tab else {
            return;
        };
        let st = &mut *self.st;
        if ui.button("New viewport").clicked() {
            st.focus_tab(id);
            st.actions.push(Action::NewViewport);
            ui.close();
        }
        if ui.button("Duplicate viewport").clicked() {
            st.actions.push(Action::DuplicateViewport(Some(id)));
            ui.close();
        }
        let others: Vec<ViewportId> = st
            .view
            .tabs
            .iter()
            .map(|t| t.id)
            .filter(|&o| o != id)
            .collect();
        if !others.is_empty() {
            ui.menu_button("Link cameras with", |ui| {
                for o in others {
                    let linked = st.view.link.is_some_and(|l| l == [id, o] || l == [o, id]);
                    if ui
                        .selectable_label(linked, viewports::tab_title(st, o))
                        .clicked()
                    {
                        st.actions.push(Action::LinkCameras(Some([id, o])));
                        ui.close();
                    }
                }
            });
        }
    }

    fn clear_background(&self, tab: &Pane) -> bool {
        !matches!(tab, Pane::Viewport(_))
    }

    fn scroll_bars(&self, _tab: &Pane) -> [bool; 2] {
        [false, false]
    }
}

impl eframe::App for EditorApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if self.terminate_requested() && !self.allow_close {
            // SIGINT/SIGTERM: no prompt, but unsaved edits are autosaved
            // first (synchronously, well inside the signal's grace) so the
            // next start offers them back; the server stops and
            // server.json goes.
            tracing::info!("terminating: autosaving, stopping the live API and closing");
            self.autosave_sync();
            self.stop_server();
            self.allow_close = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if ctx.input(|i| i.viewport().close_requested())
            && self.commands.has_unsaved()
            && !self.allow_close
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.close_prompt = true;
        }
        self.frame(ui);
    }

    /// The last thing the window does: the process ends right after with
    /// `signals::exit_now`, so nothing may be left to a destructor.
    fn on_exit(&mut self) {
        self.stop_server();
        self.save_ui_file();
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(key: egui::Key) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Default::default(),
        }
    }

    /// One frame: `modal_keys` first (as `frame` does), then a text field
    /// (focused if `focus`), then what a later reader (the viewport) still
    /// sees of Enter and Escape.
    fn frame(
        ctx: &egui::Context,
        modal: Option<Modal>,
        focus: bool,
        events: Vec<egui::Event>,
    ) -> (ModalKeys, bool, bool) {
        let raw = egui::RawInput {
            events,
            focused: true,
            ..Default::default()
        };
        let mut out = (ModalKeys::default(), false, false);
        let mut full = ctx.run_ui(raw, |ui| {
            out.0 = modal_keys(ui.ctx(), modal);
            let mut s = String::new();
            let r = ui.text_edit_singleline(&mut s);
            if focus {
                r.request_focus();
            } else {
                r.surrender_focus();
            }
            out.1 = ui.input(|i| i.key_pressed(egui::Key::Enter));
            out.2 = ui.input(|i| i.key_pressed(egui::Key::Escape));
        });
        full.textures_delta.clear();
        out
    }

    #[test]
    fn old_layouts_get_the_rules_pane() {
        // A layout saved before the Rules pane: Log under the viewport, no Rules.
        let mut old = DockState::new(vec![Pane::Viewport(ViewportId::FIRST)]);
        let s = old.main_surface_mut();
        let [center, _] = s.split_left(NodeIndex::root(), 0.2, vec![Pane::Layers]);
        s.split_below(center, 0.8, vec![Pane::Log]);
        add_rules_pane(&mut old);
        let log = old.find_tab(&Pane::Log).unwrap();
        let rules = old.find_tab(&Pane::Rules).unwrap();
        assert_eq!(log.node_path(), rules.node_path(), "a tab next to the Log");
        // Once is enough.
        add_rules_pane(&mut old);
        assert_eq!(
            old.iter_all_tabs()
                .filter(|(_, t)| **t == Pane::Rules)
                .count(),
            1
        );
        // No Log: a split under the viewport.
        let mut bare = DockState::new(vec![Pane::Viewport(ViewportId::FIRST)]);
        add_rules_pane(&mut bare);
        let v = bare.find_tab(&Pane::Viewport(ViewportId::FIRST)).unwrap();
        let r = bare.find_tab(&Pane::Rules).unwrap();
        assert_ne!(v.node_path(), r.node_path());
        // The default layout has it, and showing it makes it the active tab.
        let mut d = default_dock();
        show_pane(&mut d, Pane::Rules);
        let p = d.find_tab(&Pane::Rules).unwrap();
        let leaf = d.leaf(p.node_path()).unwrap();
        assert_eq!(leaf.tabs()[leaf.active.0], Pane::Rules);
    }

    /// A `ui.json` from before map tabs (a bare `"Viewport"` tab, one
    /// camera, world and level) opens as tab 1 holding them; a new one
    /// round-trips every tab, the focus and the bookmarks.
    #[test]
    fn old_ui_files_open_as_one_map_tab() {
        let (_tmp, st) = testkit::state();
        let doc = st.doc.as_ref().unwrap();
        let mut old = DockState::new(vec![Pane::Viewport(ViewportId::FIRST)]);
        old.main_surface_mut()
            .split_left(NodeIndex::root(), 0.2, vec![Pane::Layers]);
        // A dock never drawn has infinite rects (null in JSON); a drawn
        // one has numbers.
        fn finite(v: &mut serde_json::Value) {
            match v {
                serde_json::Value::Array(a) => a.iter_mut().for_each(finite),
                serde_json::Value::Object(o) => {
                    for (k, x) in o.iter_mut() {
                        if (k == "x" || k == "y") && x.is_null() {
                            *x = serde_json::json!(0.0);
                        }
                        finite(x);
                    }
                }
                _ => {}
            }
        }
        let mut dock = serde_json::to_value(&old).unwrap();
        finite(&mut dock);
        // How the old editor wrote the tab: a unit variant.
        fn unit(v: &mut serde_json::Value) {
            match v {
                serde_json::Value::Object(o) if o.len() == 1 && o.contains_key("Viewport") => {
                    *v = serde_json::json!("Viewport");
                }
                serde_json::Value::Array(a) => a.iter_mut().for_each(unit),
                serde_json::Value::Object(o) => o.values_mut().for_each(unit),
                _ => {}
            }
        }
        unit(&mut dock);
        assert!(dock.to_string().contains("\"Viewport\""), "{dock}");
        let text = serde_json::json!({
            "dock": dock,
            "camera": { "origin": [100.0, 50.0], "zoom": 3.0 },
            "active_level": "shaft",
            "active_world": "main",
        })
        .to_string();
        let f = parse_ui_file(&text).unwrap();
        let docked = viewports::dock_viewports(&f.dock);
        assert_eq!(docked, [ViewportId::FIRST]);
        // The pane migrations still find the (rewritten) map to split.
        let mut migrated = f.dock.clone();
        add_rules_pane(&mut migrated);
        add_types_pane(&mut migrated);
        add_notes_pane(&mut migrated);
        add_project_pane(&mut migrated);
        add_find_pane(&mut migrated);
        add_history_pane(&mut migrated);
        for p in [Pane::Rules, Pane::Find, Pane::History, Pane::Notes] {
            assert!(migrated.find_tab(&p).is_some(), "{p:?}");
        }
        assert_eq!(viewports::dock_viewports(&migrated), [ViewportId::FIRST]);
        let (tabs, focused) = ui_viewports(doc, &docked, &f);
        assert_eq!(focused, ViewportId::FIRST);
        assert_eq!(tabs.len(), 1);
        assert_eq!(tabs[0].active_level, doc.level_ix("shaft"));
        assert_eq!(
            tabs[0].active_world.as_ref().map(|s| s.as_str()),
            Some("main")
        );
        assert_eq!(tabs[0].cam.camera.origin, [100.0, 50.0]);
        assert_eq!(tabs[0].cam.camera.zoom, 3.0);

        // A file with tabs: each keeps its own; a docked tab the file
        // doesn't list gets fitted; the focus is kept.
        let mut d = default_dock();
        viewports::dock_tab(&mut d, ViewportId(2), ViewportId::FIRST, true);
        viewports::dock_tab(&mut d, ViewportId(5), ViewportId(2), false);
        let mut d = serde_json::to_value(&d).unwrap();
        finite(&mut d);
        let text = serde_json::json!({
            "dock": d,
            "viewports": [
                { "id": 1, "world": "main", "camera": { "origin": [0.0, 0.0], "zoom": 1.0 }, "active_level": "crevasse" },
                { "id": 2, "world": "main", "camera": { "origin": [7.0, 8.0], "zoom": 4.0 }, "active_level": "shaft" }
            ],
            "focused_viewport": 2,
            "linked_viewports": [1, 2],
            "bookmarks": [ { "name": "hall", "world": "main", "camera": { "center": [10.0, 20.0], "zoom": 2.0 } } ]
        })
        .to_string();
        let f = parse_ui_file(&text).unwrap();
        let docked = viewports::dock_viewports(&f.dock);
        assert_eq!(docked.len(), 3);
        let (tabs, focused) = ui_viewports(doc, &docked, &f);
        assert_eq!(focused, ViewportId(2));
        let two = tabs.iter().find(|t| t.id == ViewportId(2)).unwrap();
        assert_eq!(two.active_level, doc.level_ix("shaft"));
        assert_eq!(two.cam.camera.zoom, 4.0);
        let five = tabs.iter().find(|t| t.id == ViewportId(5)).unwrap();
        assert_eq!(five.cam.pending_fit(), Some(Fit::World));
        assert_eq!(f.linked_viewports, Some([ViewportId::FIRST, ViewportId(2)]));
        assert_eq!(f.bookmarks[0].camera.center, [10.0, 20.0]);
    }

    #[test]
    fn old_layouts_get_the_find_pane_next_to_the_notes() {
        // The default layout before Find: Problems and Notes as tabs.
        let mut old = DockState::new(vec![Pane::Viewport(crate::view::ViewportId::FIRST)]);
        let s = old.main_surface_mut();
        let [_, right] = s.split_right(NodeIndex::root(), 0.7, vec![Pane::Inspector]);
        s.split_below(right, 0.7, vec![Pane::Problems, Pane::Notes]);
        add_find_pane(&mut old);
        let notes = old.find_tab(&Pane::Notes).unwrap();
        let find = old.find_tab(&Pane::Find).unwrap();
        assert_eq!(notes.node_path(), find.node_path(), "a tab next to Notes");
        add_find_pane(&mut old);
        assert_eq!(
            old.iter_all_tabs()
                .filter(|(_, t)| **t == Pane::Find)
                .count(),
            1
        );
        // No Notes, Problems or Inspector: a split right of the map.
        let mut bare = DockState::new(vec![Pane::Viewport(crate::view::ViewportId::FIRST)]);
        add_find_pane(&mut bare);
        assert!(bare.find_tab(&Pane::Find).is_some());
        // The default layout has it beside the Notes.
        let d = default_dock();
        assert_eq!(
            d.find_tab(&Pane::Find).unwrap().node_path(),
            d.find_tab(&Pane::Notes).unwrap().node_path()
        );
    }

    #[test]
    fn old_layouts_get_the_levels_pane_next_to_the_layers() {
        let mut old = DockState::new(vec![Pane::Viewport(crate::view::ViewportId::FIRST)]);
        let s = old.main_surface_mut();
        let [_, left] = s.split_left(NodeIndex::root(), 0.2, vec![Pane::Layers]);
        s.split_below(left, 0.45, vec![Pane::Palette]);
        add_levels_pane(&mut old);
        let layers = old.find_tab(&Pane::Layers).unwrap();
        let levels = old.find_tab(&Pane::Levels).unwrap();
        assert_eq!(
            layers.node_path(),
            levels.node_path(),
            "a tab next to Layers"
        );
        add_levels_pane(&mut old);
        assert_eq!(
            old.iter_all_tabs()
                .filter(|(_, t)| **t == Pane::Levels)
                .count(),
            1
        );
        let mut bare = DockState::new(vec![Pane::Viewport(crate::view::ViewportId::FIRST)]);
        add_levels_pane(&mut bare);
        assert!(bare.find_tab(&Pane::Levels).is_some());
        let d = default_dock();
        assert_eq!(
            d.find_tab(&Pane::Levels).unwrap().node_path(),
            d.find_tab(&Pane::Layers).unwrap().node_path()
        );
    }

    #[test]
    fn modal_prompts_take_their_keys_first() {
        let ctx = egui::Context::default();
        let keys = || vec![key(egui::Key::Enter), key(egui::Key::Escape)];
        // No modal: nothing is taken, the viewport sees Escape.
        let (k, enter, esc) = frame(&ctx, None, false, keys());
        assert_eq!(k, ModalKeys::default());
        assert!(enter && esc);
        // A modal takes Escape and (nothing focused) Enter; nobody after it
        // sees them, so Escape doesn't also cancel a viewport gesture.
        frame(&ctx, None, false, vec![]);
        let (k, enter, esc) = frame(&ctx, Some(Modal::Confirm), false, keys());
        assert_eq!(k.for_modal(Modal::Confirm), (true, true));
        assert_eq!(k.for_modal(Modal::NewLevel), (false, false));
        assert!(!enter && !esc);
        // With a text field focused (say a rename in the inspector), Enter
        // belongs to the field: the delete prompt doesn't see it.
        frame(&ctx, None, true, vec![]);
        let (k, _, _) = frame(
            &ctx,
            Some(Modal::Confirm),
            true,
            vec![key(egui::Key::Enter)],
        );
        assert_eq!(k.for_modal(Modal::Confirm), (false, false));
    }
}
