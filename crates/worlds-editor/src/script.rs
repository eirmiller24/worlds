//! `--script` files: a JSON array whose items are either commands in the
//! `docs/commands.md` wire format, applied as they are (one undo entry
//! each), or tool gestures replayed through the same path as the mouse:
//!
//! ```json
//! [
//!   { "op": "fill_rect", "level": "crevasse", "layer": "collision", "rect": [28, 12, 4, 13], "value": "solid" },
//!   { "gesture": "brush", "level": "crevasse", "layer": "collision", "path": [[3, 20], [12, 18]],
//!     "brush": "solid" },
//!   { "gesture": "brush", "level": "crevasse", "layer": "decor_back", "path": [[6, 13]],
//!     "brush": { "tileset": "castle_main", "rect": [20, 23, 3, 6] } },
//!   { "gesture": "rect", "level": "crevasse", "layer": "collision", "path": [[40, 10], [50, 16]], "shift": true },
//!   { "gesture": "select", "level": "shaft", "path": [[1, 1], [4, 1]] }
//! ]
//! ```
//!
//! A UI step sets editor state for the picture, no command:
//!
//! ```json
//! { "ui": "rules", "layer": "main_tiling", "level": "level_4", "rule": "walls__fill",
//!   "group_collapsed": ["decor"] }
//! ```
//!
//! opens the Rules pane on auto layer `layer`, makes `level` active (and
//! frames it) and selects `rule` (an inline rule, a template reference, or
//! an expanded `<reference>__<rule>`), so the canvas shows its highlight.
//! `level`, `rule` and `group_collapsed` are optional.
//!
//! ```json
//! { "ui": "palette", "search": "door", "favorites": ["locked_door"], "recent": [],
//!   "collapsed": ["/recent", "Puzzle"], "chips": ["object"], "view": "list",
//!   "brush": "locked_door", "tool": true }
//! ```
//!
//! shows the entity palette (the entities layer becomes active) and sets
//! the given parts of its state ([`PaletteStep`]).
//!
//! ```json
//! { "ui": "select", "entities": ["doorcrev22", "doorshaft2"] }
//! { "ui": "types", "entity": "locked_door", "field": "channel" }
//! { "ui": "types", "level_fields": true }
//! ```
//!
//! selects entities (the Inspector shows them; [`SelectStep`]) and opens the
//! Types pane ([`TypesStep`]). Each `ui` kind refuses unknown keys.
//!
//! ```json
//! { "ui": "reference", "add": "sketches/crevasse.png", "level": "crevasse", "opacity": 0.6 }
//! { "ui": "history", "expand": true }
//! ```
//!
//! adds a reference image as "Add reference image…" does
//! ([`ReferenceStep`]) and shows the History pane ([`HistoryStep`]).
//!
//! An `entity` gesture places with the entity tool: `type` and `fields` set
//! the brush first, `ctrl` holds Ctrl (pixel placement), `shift` resizes,
//! `alt` samples:
//!
//! ```json
//! { "gesture": "entity", "level": "crevasse", "path": [[10, 20]], "type": "room_entry",
//!   "fields": { "id": "west" } }
//! ```
//!
//! A select gesture with `"press": true` lets the press decide what it
//! grabs, as the mouse does: on an entity it selects it (Ctrl adds) and
//! the drag moves the selection; on empty space in the active level it is
//! a marquee (Ctrl adds what it touches):
//!
//! ```json
//! { "gesture": "select", "level": "crevasse", "layer": "objects", "path": [[18, 2], [16, 2]], "press": true }
//! ```
//!
//! A gesture makes `level` and `layer` active, sets the layer's palette
//! brush if `brush` is given (see [`ScriptBrush`]; leave it out to keep the
//! current brush), then sends `Begin` at the first cell of
//! `path`, a `Move` for each later cell and `End` at the last (or `Cancel`
//! with `"cancel": true`). Cells are in the layer's grid; the pointer sits
//! in the middle of each cell. `shift` and `alt` are held throughout.
//! `handle` (`[-1..1, -1..1]`, the edges that move) makes a select gesture
//! a resize instead of a move.
//!
//! A `vertex` gesture edits one annotation's geometry with the Select
//! tool's handles ([`VertexStep`]):
//!
//! ```json
//! { "gesture": "vertex", "note": "pathcrev22", "vertex": 1, "world_px": [[300, 96]] }
//! { "gesture": "vertex", "note": "pathcrev22", "insert": [150, 64] }
//! { "gesture": "vertex", "note": "pathcrev22", "vertex": 1, "delete": true }
//! ```

use std::path::Path;

use anyhow::{Context as _, bail};
use serde::Deserialize;
use worlds_core::{Command, LayerKind};

use crate::doc::{Document, LevelIx};
use crate::tools::{Brush, CellHit, Grab, Tool, ToolEvent};

#[derive(Debug, Clone, PartialEq)]
pub enum ScriptStep {
    Command(Box<Command>),
    Gesture(GestureStep),
    Ui(UiStep),
    /// `{ "gesture": "note" | "measure", "world_px": [[x, y], ...], ... }`.
    NoteGesture(NoteGestureStep),
    /// `{ "gesture": "select", "world_px": [[x, y], ...], "press": true }`.
    WorldSelect(WorldSelectStep),
    /// `{ "undo": n }` or `{ "redo": n }`: step the history.
    History {
        redo: bool,
        n: usize,
    },
    /// `{ "gesture": "vertex", "note": id, ... }`: the Select tool on one
    /// annotation's geometry handles.
    Vertex(VertexStep),
}

/// The Select tool's geometry handles on one annotation, through the same
/// press and drag as the mouse (`note` becomes the selection first):
///
/// - `"vertex": i` (a point of a path, guide or polygon, a connector's
///   waypoint) or `"handle": [ex, ey]` (a rect region's corner or edge,
///   `-1..1` per axis) with `world_px`: pressed on that handle and dragged
///   through the points, released at the last (one `update_note`);
/// - the same with `"delete": true` and no `world_px`: the handle clicked
///   (highlighted), then the Delete key;
/// - `"insert": [x, y]`: an Alt+click there, on a segment, adds a point.
///
/// `ctrl` holds Ctrl (pixels instead of half the project grid).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VertexStep {
    pub gesture: String,
    pub note: String,
    #[serde(default)]
    pub vertex: Option<usize>,
    #[serde(default)]
    pub handle: Option<[i8; 2]>,
    #[serde(default)]
    pub world_px: Vec<[f64; 2]>,
    #[serde(default)]
    pub insert: Option<[f64; 2]>,
    #[serde(default)]
    pub delete: bool,
    #[serde(default)]
    pub ctrl: bool,
}

/// The Note tool (or the Measure tool) through the real path, in world px:
/// note and pin are a click at the first point; a path, or a region with
/// `polygon`, is a click per point then `end` (Enter by default); a region
/// without `polygon` is a drag from the first point to the second (a
/// rect). `keys` are pressed after the points, before `end`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoteGestureStep {
    /// `note` or `measure`.
    pub gesture: String,
    /// `note`, `pin`, `path`, `region` or `connector` (Note tool; default
    /// `note`). A connector is a click per point (the first on a room or
    /// entity, then waypoints and the other end), then `end`.
    #[serde(default)]
    pub kind: Option<String>,
    pub world_px: Vec<[f64; 2]>,
    /// `"world"`: world scope (else the active level, the level under the
    /// first point, or the world outside every level).
    #[serde(default)]
    pub scope: Option<String>,
    /// A pin's icon.
    #[serde(default)]
    pub icon: Option<String>,
    /// A region drawn as a polygon (clicks) instead of a rect (a drag).
    #[serde(default)]
    pub polygon: bool,
    /// Ctrl: points by the pixel instead of half the project grid.
    #[serde(default)]
    pub ctrl: bool,
    /// Keys after the points: `"backspace"`.
    #[serde(default)]
    pub keys: Vec<String>,
    /// How a path or polygon ends: `"enter"` (default), `"double_click"`,
    /// `"escape"` or `"none"` (left in progress).
    #[serde(default)]
    pub end: Option<String>,
    /// The clicks (indexes into `world_px`) made with Shift held: a
    /// connector's waypoint even on a room or entity.
    #[serde(default)]
    pub shift: Vec<usize>,
}

/// A Select press and drag in world px, exactly as the mouse does it
/// ([`crate::viewport::select_press`] at the first point, then the drag
/// through every later point, release at the last): a world marquee from
/// empty world space, dragging selected levels (with guide snapping), a
/// note or pin. `press` must be `true`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorldSelectStep {
    pub gesture: String,
    pub world_px: Vec<[f64; 2]>,
    pub press: bool,
    #[serde(default)]
    pub ctrl: bool,
}

/// A UI-state step (see the module docs), tagged on `ui`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "ui", rename_all = "snake_case")]
pub enum UiStep {
    /// `"ui": "rules"`: the Rules pane on an auto layer.
    Rules(RulesStep),
    /// `"ui": "palette"`: the entity palette.
    Palette(PaletteStep),
    /// `"ui": "select"`: select entities (the multi-select inspector).
    Select(SelectStep),
    /// `"ui": "types"`: the Types pane on an entity type or the level
    /// fields.
    Types(TypesStep),
    /// `"ui": "notes"`: the Notes pane with that state.
    Notes(NotesStep),
    /// `"ui": "focus"`: frame a level, a world rect or an annotation.
    Focus(FocusStep),
    /// `"ui": "duplicate"`: Ctrl-D on the selection (one entry).
    Duplicate(DuplicateStep),
    /// `"ui": "delete"`: the Delete key (one entry; `confirm` answers the
    /// prompt Delete raises with levels selected).
    Delete(DeleteStep),
    /// `"ui": "layers"`: the Layers pane's state (active layer, visibility,
    /// opacity).
    Layers(LayersStep),
    /// `"ui": "project"`: the Project pane (layers, tilesets, settings),
    /// and its + Layer and + Tileset buttons.
    Project(ProjectStep),
    /// `"ui": "copy"`: Ctrl+C on the selection and the grid marquee.
    Copy(CopyStep),
    /// `"ui": "cut"`: Ctrl+X.
    Cut(CopyStep),
    /// `"ui": "paste"`: Ctrl+V (the ghost, pinned where the step says, and
    /// optionally placed) or Ctrl+Shift+V.
    Paste(PasteStep),
    /// `"ui": "find"`: the Find pane with a query, optionally Select all
    /// and Replace.
    Find(FindStep),
    /// `"ui": "history"`: the History pane in front, its agent batches
    /// expanded, a jump.
    History(HistoryStep),
    /// `"ui": "reference"`: "Add reference image…" with a given PNG.
    Reference(ReferenceStep),
    /// `"ui": "import_project"`: the Import from Worlds project dialog.
    ImportProject(Box<ImportStep>),
    /// `"ui": "viewport"`: open, close, focus or link map tabs.
    Viewport(ViewportStep),
    /// `"ui": "bookmark"`: save or jump to a camera bookmark.
    Bookmark(BookmarkStep),
}

/// `{ "ui": "import_project", "source": dir, "selection"?, "policy"?,
/// "search"?, "apply"?: bool }`: the Import from Worlds project dialog on
/// the project in `source` (relative to the current directory), with the
/// `selection` and `policy` of `project.import_plan` and the search text,
/// so a `--screenshot` shows it; `apply: true` then clicks Apply (one
/// entry, `Import from <name>`; the plan's files are copied first). A
/// source that doesn't load, this project itself, a refused plan or
/// nothing to apply is an error.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportStep {
    pub source: std::path::PathBuf,
    #[serde(default)]
    pub selection: Option<worlds_core::merge::Selection>,
    #[serde(default)]
    pub policy: Option<worlds_core::merge::Policy>,
    #[serde(default)]
    pub search: Option<String>,
    #[serde(default)]
    pub apply: bool,
}

/// `{ "ui": "find", "query": "...", "kind"?: "all" | "entities" |
/// "levels" | "notes" | "tiles", "select_all"?: true, "replace"?: {
/// "field", "value" } }`: the Find pane in front with that query (and
/// kind, default all) and its results. `select_all` presses Select all;
/// `replace` presses Replace with that field and value text (one entry).
/// A query that doesn't parse, or that the kind doesn't take, is an error;
/// so are Select all with no result and a refused Replace.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FindStep {
    pub query: String,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub select_all: bool,
    #[serde(default)]
    pub replace: Option<ReplaceStep>,
}

/// `"replace": { "field": slug, "value": text }` of a [`FindStep`].
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplaceStep {
    pub field: String,
    pub value: String,
}

/// `{ "ui": "viewport", "open"?: { "world"? }, "focus"?: id, "close"?: id,
/// "link"?: [a, b] }`, applied in that order: `open` adds a map tab beside
/// the focused one (on `world`, default the focused tab's), fitted, and
/// focuses it; `focus` makes tab `id` the one later steps act in; `link`
/// links two tabs' cameras (`[]` unlinks).
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViewportStep {
    #[serde(default)]
    pub open: Option<OpenViewport>,
    #[serde(default)]
    pub focus: Option<u32>,
    #[serde(default)]
    pub close: Option<u32>,
    #[serde(default)]
    pub link: Option<Vec<u32>>,
}

/// A viewport step's `open`.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenViewport {
    #[serde(default)]
    pub world: Option<String>,
}

/// `{ "ui": "bookmark", "save"?: name, "jump"?: name }`: save the focused
/// tab's view (world, camera) as `save`, then show bookmark `jump` in it.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BookmarkStep {
    #[serde(default)]
    pub save: Option<String>,
    #[serde(default)]
    pub jump: Option<String>,
}

/// `{ "ui": "copy" }` / `{ "ui": "cut" }`: the key, exactly as pressed
/// ([`crate::clipboard::copy_selection`]); the fragment becomes the
/// editor's clipboard. Nothing to copy is an error.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CopyStep {}

/// `{ "ui": "paste", "fragment"?: {...}, "level"?: slug, "origin"?: [x, y],
/// "place"?: bool, "in_place"?: bool }`: Ctrl+V with `fragment` (default:
/// the editor's clipboard, from the last copy). The ghost is armed and,
/// with `level` and `origin` (the fragment's top-left corner in cells of
/// the project grid), pinned there so the picture shows it; `place: true`
/// then lands it as a click does. `in_place: true` is Ctrl+Shift+V instead.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PasteStep {
    #[serde(default)]
    pub fragment: Option<serde_json::Value>,
    #[serde(default)]
    pub level: Option<String>,
    #[serde(default)]
    pub origin: Option<[i64; 2]>,
    #[serde(default)]
    pub place: bool,
    #[serde(default)]
    pub in_place: bool,
}

/// `{ "ui": "history", "expand"?: true, "jump"?: n }`: the History pane in
/// front; `expand` opens every agent batch's list of writes; `jump` undoes
/// or redoes until `n` entries are done, as clicking an entry does (`0`:
/// the "Opened" row). A `jump` past the history's length is an error.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryStep {
    #[serde(default)]
    pub expand: bool,
    #[serde(default)]
    pub jump: Option<usize>,
}

/// `{ "ui": "reference", "add": path, "level"?: slug, "world"?: slug,
/// "px"?: [x, y], "scale"?, "opacity"?, "locked"? }`: what "Add reference
/// image…" does with that PNG (relative to the current directory; an image
/// outside the project is copied into `art/`): one `add_reference` entry at
/// the level (default: the active level, else the world) with its corner at
/// `px` (default the level's corner), opacity 0.5 unless given; the new
/// reference is selected, the References pseudo-layer active and the
/// Inspector in front. An image that doesn't read is an error.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceStep {
    pub add: String,
    #[serde(default)]
    pub level: Option<String>,
    #[serde(default)]
    pub world: Option<String>,
    #[serde(default)]
    pub px: Option<[i64; 2]>,
    #[serde(default)]
    pub scale: Option<f64>,
    #[serde(default)]
    pub opacity: Option<f64>,
    #[serde(default)]
    pub locked: bool,
}

/// `{ "ui": "project", "add_layer"?: kind, "add_tileset"?: path,
/// "section"?: "layers" | "tilesets" | "settings", "layer"?: slug,
/// "tileset"?: slug }`: the Project pane in front. `add_layer` does what
/// its + Layer menu does for that kind (`int_grid`, `tiles`, `auto`,
/// `entities`: one entry, the new layer active and its form open);
/// `add_tileset` what + Tileset… does with that PNG (relative to the
/// current directory; an image outside the project is copied into `art/`;
/// one entry, its form open). Then `section` shows that part, `layer` /
/// `tileset` open that form (and its section). An unknown kind, section,
/// layer or tileset, or a refused add, is an error.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectStep {
    #[serde(default)]
    pub add_layer: Option<String>,
    #[serde(default)]
    pub add_tileset: Option<std::path::PathBuf>,
    #[serde(default)]
    pub section: Option<String>,
    #[serde(default)]
    pub layer: Option<String>,
    #[serde(default)]
    pub tileset: Option<String>,
}

/// `{ "ui": "layers", "active"?: slug, "visible"?: { slug: bool },
/// "opacity"?: { slug: number } }`: make a layer active, show or hide
/// layers, set their view opacity (0 to 1), as the Layers pane does. An
/// unknown layer, or an opacity outside 0..=1, is an error naming it.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LayersStep {
    #[serde(default)]
    pub active: Option<String>,
    #[serde(default)]
    pub visible: Option<std::collections::BTreeMap<String, bool>>,
    #[serde(default)]
    pub opacity: Option<std::collections::BTreeMap<String, f32>>,
}

/// `{ "ui": "duplicate" }`: Ctrl-D, exactly as the key
/// ([`crate::annot::plan_duplicate`]); the copies become the selection.
/// Nothing selected, or nothing that can be copied, is an error.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DuplicateStep {}

/// `{ "ui": "delete", "confirm"?: true }`: the Delete key, exactly as
/// pressed on the map ([`crate::app::actions::delete_action`]: the
/// selection, or with none the active level). When it asks first (levels
/// in it), `confirm: true` answers yes; without it the step is an error
/// naming the question and nothing is deleted.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeleteStep {
    #[serde(default)]
    pub confirm: bool,
}

/// `{ "ui": "notes", ... }`: the Notes pane in front, each given key
/// replacing that part of its state (filters) or of the map's annotation
/// view (visibility, guide options).
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotesStep {
    #[serde(default)]
    pub search: Option<String>,
    /// A kind (`note`, `pin`, `path`, `region`, `guide`), or `"any"`.
    #[serde(default)]
    pub kind: Option<String>,
    /// `"all"`, `"world"`, `"this_level"` or a level slug.
    #[serde(default)]
    pub scope: Option<String>,
    /// `"open"`, `"done"` or `"all"`.
    #[serde(default)]
    pub status: Option<String>,
    /// Tag chips (AND).
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    #[serde(default)]
    pub hidden_tags: Option<Vec<String>>,
    #[serde(default)]
    pub hide_done: Option<bool>,
    #[serde(default)]
    pub hidden_kinds: Option<Vec<String>>,
    #[serde(default)]
    pub show_designed: Option<bool>,
    #[serde(default)]
    pub snap_to_guides: Option<bool>,
    #[serde(default)]
    pub guides_open: Option<bool>,
    /// The pane's "Tags, visibility and guide groups" section open.
    #[serde(default)]
    pub view_open: Option<bool>,
}

/// `{ "ui": "focus", "level" | "world_rect": [x, y, w, h] | "note" }`
/// (exactly one): frame it as `view.focus` does. `world` alone shows that
/// world, fitted; with `world_rect` it says which world the rect is in.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FocusStep {
    #[serde(default)]
    pub level: Option<String>,
    #[serde(default)]
    pub world_rect: Option<[i64; 4]>,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub world: Option<String>,
    /// The map tab to frame it in (default: the focused one).
    #[serde(default)]
    pub viewport: Option<u32>,
}

/// `"tiles"` of a select step: stacks of one tiles layer of one level.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TilesSel {
    pub level: String,
    pub layer: String,
    pub cells: Vec<[u32; 2]>,
}

/// `{ "ui": "rules", "layer", "level"?, "rule"?, "group_collapsed"?,
/// "show_regions"?, "show_occupancy"? }`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RulesStep {
    /// The auto layer.
    pub layer: String,
    /// Level to make active and frame (default: keep the active level).
    #[serde(default)]
    pub level: Option<String>,
    /// Rule, reference or expanded rule slug to select.
    #[serde(default)]
    pub rule: Option<String>,
    /// Groups to show collapsed.
    #[serde(default)]
    pub group_collapsed: Vec<String>,
    /// The pane's "Show regions" toggle: a selected region rule outlines
    /// the regions it looks at (default on).
    #[serde(default)]
    pub show_regions: Option<bool>,
    /// The pane's "Show occupancy" toggle: hatch the layer's occupied cells
    /// (default off).
    #[serde(default)]
    pub show_occupancy: Option<bool>,
}

/// `{ "ui": "palette", ... }`: the Palette pane on the entities layer (the
/// active one, else the first), then each given key replaces that part of
/// the palette's state. Slugs must name entity types.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaletteStep {
    /// The search text.
    #[serde(default)]
    pub search: Option<String>,
    #[serde(default)]
    pub favorites: Option<Vec<String>>,
    /// Most recent first (at most 8 are kept).
    #[serde(default)]
    pub recent: Option<Vec<String>>,
    /// Collapsed parts (`/favorites`, `/recent`, `/level`, `/catalogue`)
    /// and catalogue category paths.
    #[serde(default)]
    pub collapsed: Option<Vec<String>>,
    /// Tag chips (AND filter of the catalogue).
    #[serde(default)]
    pub chips: Option<Vec<String>>,
    /// `"grid"` or `"list"`.
    #[serde(default)]
    pub view: Option<crate::tools::PaletteView>,
    /// The brush's type (no field values).
    #[serde(default)]
    pub brush: Option<String>,
    /// `true`: switch to the entity tool (the search box gets the
    /// keyboard).
    #[serde(default)]
    pub tool: Option<bool>,
    /// `true`: Enter in the search box, after the other keys: the first
    /// hit for `search` becomes the brush and the entity tool is on. No
    /// hit is an error.
    #[serde(default)]
    pub enter: Option<bool>,
}

/// `{ "ui": "select", "entities"?: [ids], "notes"?: [ids], "levels"?:
/// [slugs], "tiles"?: { "level", "layer", "cells": [[x, y]] } }`: select
/// these (they add up, in that order; the first is primary; the first
/// entity's level, else the first level note's, else the first level,
/// else the tiles' level becomes active and is framed) and bring the
/// Inspector to the front. An unknown id, slug or empty cell is an error.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectStep {
    #[serde(default)]
    pub entities: Vec<String>,
    #[serde(default)]
    pub notes: Vec<String>,
    #[serde(default)]
    pub levels: Vec<String>,
    #[serde(default)]
    pub tiles: Option<TilesSel>,
}

/// `{ "ui": "types", "entity": slug }` or `{ "ui": "types",
/// "level_fields": true }`, optionally with `"field": slug`: the Types pane
/// on that type (or the level fields), with that field's form open.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TypesStep {
    #[serde(default)]
    pub entity: Option<String>,
    #[serde(default)]
    pub level_fields: Option<bool>,
    #[serde(default)]
    pub field: Option<String>,
}

impl UiStep {
    /// `--rule <layer>/<rule>`: the Rules pane on that rule, in `level`.
    pub fn rule(spec: &str, level: Option<String>) -> anyhow::Result<UiStep> {
        let Some((layer, rule)) = spec.split_once('/') else {
            bail!("--rule {spec:?}: expected <layer>/<rule>, e.g. main_tiling/walls__fill");
        };
        Ok(UiStep::Rules(RulesStep {
            layer: layer.into(),
            level,
            rule: (!rule.is_empty()).then(|| rule.to_string()),
            group_collapsed: Vec::new(),
            show_regions: None,
            show_occupancy: None,
        }))
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GestureStep {
    /// Tool name: select, brush, rect, line, fill, eraser.
    pub gesture: String,
    pub level: String,
    /// Layer slug (default: the active layer, else the first).
    #[serde(default)]
    pub layer: Option<String>,
    /// Cells of the layer's grid, at least one.
    pub path: Vec<[i32; 2]>,
    #[serde(default)]
    pub shift: bool,
    #[serde(default)]
    pub alt: bool,
    /// Palette brush for the layer before the gesture; leave it out to keep
    /// the current one (`null` is an error).
    #[serde(default, deserialize_with = "present_brush")]
    pub brush: Option<ScriptBrush>,
    /// Select: grab this resize handle instead of the level body.
    #[serde(default)]
    pub handle: Option<[i8; 2]>,
    /// End with `Cancel` (Escape) instead of `End`.
    #[serde(default)]
    pub cancel: bool,
    /// Entity and select gestures: Ctrl held (pixel placement or moves;
    /// a select press adds to the selection).
    #[serde(default)]
    pub ctrl: bool,
    /// Select gestures: the press decides what it grabs, as a mouse press
    /// does ([`crate::viewport::select_press`]): an entity selects it and
    /// the drag moves the selection, empty space in the active level is a
    /// marquee, another level's body moves that level. Without it a select
    /// gesture grabs the level body (or `handle`).
    #[serde(default)]
    pub press: bool,
    /// Entity gestures: the brush's type (default: keep the brush).
    #[serde(default, rename = "type")]
    pub ty: Option<String>,
    /// Entity gestures: the brush's field values (with `type`, or for the
    /// current brush's type).
    #[serde(default)]
    pub fields: Option<std::collections::BTreeMap<String, serde_json::Value>>,
}

/// A brush as a script writes it:
///
/// - `"solid"`: an int_grid value, by slug;
/// - `"erase"`: empty (int_grid cells, or no tiles; on a derived layer,
///   remove overrides);
/// - `"suppress"`: on a derived layer, override to empty (`"!"`);
/// - `{ "tileset": "castle_main", "tile": [col, row] }`: one atlas tile;
/// - `{ "tileset": "castle_main", "rect": [col, row, w, h] }`: a stamp;
/// - `{ "tileset": "castle_main", "tiles": [alternatives] }`: a random
///   brush, the alternatives in the forms of a rule's `out.tiles`.
///
/// A tile form may add `"density": 1..100` (the Paint strip's density)
/// and `"seed": n` (a fixed seed for the stroke's randomness, so a test
/// gets the same picks every run): a [`ScriptBrush::Scatter`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScriptBrush {
    Value(String),
    Erase,
    Suppress,
    Tiles {
        tileset: String,
        rect: [u32; 4],
    },
    Random {
        tileset: String,
        alternatives: Vec<worlds_core::TileAlt>,
    },
    /// A tile form with `density` and/or `seed`.
    Scatter {
        brush: Box<ScriptBrush>,
        density: Option<u8>,
        seed: Option<u64>,
    },
}

impl ScriptBrush {
    /// The `density` and `seed` a tile form gave.
    pub fn scatter(&self) -> (Option<u8>, Option<u64>) {
        match self {
            ScriptBrush::Scatter { density, seed, .. } => (*density, *seed),
            _ => (None, None),
        }
    }
}

const BRUSH_FORMS: &str = r#"a value slug, "erase", "suppress", {"tileset", "tile": [c, r]}, {"tileset", "rect": [c, r, w, h]} or {"tileset", "tiles": [alternatives]}, the last three with "density"? and "seed"?"#;

impl<'de> Deserialize<'de> for ScriptBrush {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct TileForm {
            tileset: String,
            #[serde(default)]
            tile: Option<[u32; 2]>,
            #[serde(default)]
            rect: Option<[u32; 4]>,
            #[serde(default)]
            tiles: Option<Vec<serde_json::Value>>,
            #[serde(default)]
            density: Option<u8>,
            #[serde(default)]
            seed: Option<u64>,
        }
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Wire {
            Name(String),
            Tiles(TileForm),
        }
        let wire = Wire::deserialize(d)
            .map_err(|_| D::Error::custom(format!("brush: expected {BRUSH_FORMS}")))?;
        match wire {
            Wire::Name(s) if s == "erase" => Ok(ScriptBrush::Erase),
            Wire::Name(s) if s == "suppress" => Ok(ScriptBrush::Suppress),
            Wire::Name(s) => Ok(ScriptBrush::Value(s)),
            Wire::Tiles(t) => {
                let rect = match (t.tile, t.rect, &t.tiles) {
                    (Some([c, r]), None, None) => [c, r, 1, 1],
                    (None, Some(r), None) if r[2] > 0 && r[3] > 0 => r,
                    (None, Some(_), None) => {
                        return Err(D::Error::custom(
                            "brush: rect width and height must be at least 1",
                        ));
                    }
                    (None, None, Some(_)) => [0, 0, 0, 0],
                    _ => {
                        return Err(D::Error::custom(
                            "brush: give exactly one of `tile`, `rect` and `tiles`",
                        ));
                    }
                };
                let brush = match t.tiles {
                    Some(alts) => {
                        if alts.is_empty() {
                            return Err(D::Error::custom(
                                "brush: `tiles` needs at least one alternative",
                            ));
                        }
                        let alternatives = alts
                            .into_iter()
                            .enumerate()
                            .map(|(i, v)| {
                                serde_json::from_value::<worlds_core::TileAlt>(v).map_err(|e| {
                                    D::Error::custom(format!("brush: tiles[{i}]: {e}"))
                                })
                            })
                            .collect::<Result<Vec<_>, _>>()?;
                        ScriptBrush::Random {
                            tileset: t.tileset,
                            alternatives,
                        }
                    }
                    None => ScriptBrush::Tiles {
                        tileset: t.tileset,
                        rect,
                    },
                };
                if let Some(d) = t.density
                    && !(1..=100).contains(&d)
                {
                    return Err(D::Error::custom(format!(
                        "brush: density {d} must be 1 to 100"
                    )));
                }
                Ok(if t.density.is_some() || t.seed.is_some() {
                    ScriptBrush::Scatter {
                        brush: Box::new(brush),
                        density: t.density,
                        seed: t.seed,
                    }
                } else {
                    brush
                })
            }
        }
    }
}

/// `brush` given: it must be a brush, not `null`.
fn present_brush<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<ScriptBrush>, D::Error> {
    use serde::de::Error;
    let v = serde_json::Value::deserialize(d)?;
    if v.is_null() {
        return Err(D::Error::custom(
            "brush: null is not a brush; leave `brush` out to keep the current one, or use \"erase\"",
        ));
    }
    ScriptBrush::deserialize(v)
        .map(Some)
        .map_err(D::Error::custom)
}

/// The palette brush a script brush means on project layer `layer`.
pub fn resolve_brush(doc: &Document, layer: usize, b: &ScriptBrush) -> anyhow::Result<Brush> {
    let def = &doc.project.layers[layer];
    match (b, &def.kind) {
        (ScriptBrush::Erase, LayerKind::IntGrid(_) | LayerKind::Tiles(_)) => Ok(Brush::Erase),
        (ScriptBrush::Suppress, LayerKind::IntGrid(_)) if def.is_derived() => Ok(Brush::Suppress),
        (ScriptBrush::Suppress, _) => bail!(
            "brush \"suppress\" only applies to derived layers; {} isn't derived",
            def.slug
        ),
        (ScriptBrush::Value(v), LayerKind::IntGrid(_)) => {
            def.value_index(v).map(Brush::Value).with_context(|| {
                let known: Vec<&str> = def.values().iter().map(|v| v.slug.as_str()).collect();
                format!(
                    "layer {} has no value {v:?} (values: {})",
                    def.slug,
                    known.join(", ")
                )
            })
        }
        (ScriptBrush::Tiles { tileset, rect }, LayerKind::Tiles(_)) => {
            let ts = doc
                .project
                .tileset(tileset)
                .with_context(|| format!("no tileset {tileset:?}"))?;
            Ok(Brush::Tiles {
                tileset: ts.slug.clone(),
                rect: *rect,
            })
        }
        (
            ScriptBrush::Random {
                tileset,
                alternatives,
            },
            LayerKind::Tiles(_),
        ) => {
            let ts = doc
                .project
                .tileset(tileset)
                .with_context(|| format!("no tileset {tileset:?}"))?;
            Ok(Brush::Random {
                tileset: ts.slug.clone(),
                alternatives: alternatives.clone(),
            })
        }
        (ScriptBrush::Scatter { brush, .. }, _) => resolve_brush(doc, layer, brush),
        (ScriptBrush::Value(_), _) => bail!(
            "layer {} is a {} layer: a value brush needs an int_grid layer",
            def.slug,
            def.kind.name()
        ),
        (ScriptBrush::Tiles { .. } | ScriptBrush::Random { .. }, _) => bail!(
            "layer {} is a {} layer: a tile brush needs a tiles layer",
            def.slug,
            def.kind.name()
        ),
        (ScriptBrush::Erase, _) => {
            bail!("nothing to erase on {} layer {}", def.kind.name(), def.slug)
        }
    }
}

/// What a gesture step sends, resolved against the document.
#[derive(Debug, Clone)]
pub struct GesturePlan {
    pub events: Vec<ToolEvent>,
    pub level: LevelIx,
    /// Project layer index.
    pub layer: usize,
    /// The palette brush to set on the layer first, if the step gave one.
    pub brush: Option<Brush>,
    /// Entity gestures: the entity brush to set first, if the step gave a
    /// type or fields.
    pub entity_brush: Option<crate::tools::EntityBrush>,
    /// The brush's `density` and `seed`, to set first if given.
    pub density: Option<u8>,
    pub seed: Option<u64>,
}

/// Parse a script: an item with a `gesture` key is a gesture, anything
/// else must be a command.
pub fn parse(text: &str) -> anyhow::Result<Vec<ScriptStep>> {
    let items: Vec<serde_json::Value> = serde_json::from_str(text)?;
    items
        .into_iter()
        .enumerate()
        .map(|(i, v)| parse_step(v).with_context(|| format!("item {i}")))
        .collect()
}

/// One script item (see the module docs for the forms).
fn parse_step(v: serde_json::Value) -> anyhow::Result<ScriptStep> {
    let gesture = v
        .get("gesture")
        .and_then(|g| g.as_str())
        .map(str::to_string);
    if let Some(g) = gesture {
        return Ok(match g.as_str() {
            "note" | "measure" => ScriptStep::NoteGesture(serde_json::from_value(v)?),
            "vertex" => ScriptStep::Vertex(serde_json::from_value(v)?),
            "select" if v.get("world_px").is_some() => {
                let s: WorldSelectStep = serde_json::from_value(v)?;
                if !s.press {
                    bail!("a world_px select gesture needs \"press\": true");
                }
                ScriptStep::WorldSelect(s)
            }
            _ => ScriptStep::Gesture(serde_json::from_value(v)?),
        });
    }
    if v.get("ui").is_some() {
        return Ok(ScriptStep::Ui(serde_json::from_value(v)?));
    }
    if v.get("op").is_none()
        && let Some(obj) = v.as_object()
        && obj.len() == 1
        && let Some((k, n)) = obj.iter().next()
        && (k == "undo" || k == "redo")
    {
        let n = n
            .as_u64()
            .with_context(|| format!("{k}: expected a number of steps"))?;
        return Ok(ScriptStep::History {
            redo: k == "redo",
            n: n as usize,
        });
    }
    Ok(ScriptStep::Command(Box::new(serde_json::from_value(v)?)))
}

pub fn read(path: &Path) -> anyhow::Result<Vec<ScriptStep>> {
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    parse(&text).with_context(|| format!("parse {}", path.display()))
}

pub fn parse_tool(s: &str) -> Option<Tool> {
    Tool::ALL.into_iter().find(|t| {
        t.name().eq_ignore_ascii_case(s) || (s.eq_ignore_ascii_case("rect") && *t == Tool::Rect)
    })
}

/// The pointer over cell `cell` of project layer `layer` in level `ix`.
pub fn cell_hit(doc: &Document, ix: LevelIx, layer: usize, cell: [i32; 2]) -> CellHit {
    let level = doc.level(ix);
    let (g, size) = crate::tools::hit_grid(doc, layer, ix);
    let g = g as f64;
    CellHit {
        level: ix,
        layer,
        cell,
        inside: crate::tools::in_layer(cell, size),
        world_px: [
            level.world_px[0] as f64 + (cell[0] as f64 + 0.5) * g,
            level.world_px[1] as f64 + (cell[1] as f64 + 0.5) * g,
        ],
    }
}

/// The pointer at world pixel `wp`, resolved in level `ix` on project
/// layer `layer` (cells may be outside the level).
pub fn world_hit(doc: &Document, ix: LevelIx, layer: usize, wp: [f64; 2]) -> CellHit {
    if ix >= doc.levels.len() {
        // No level ([`crate::viewport::NO_LEVEL`]): only `world_px` counts.
        return CellHit {
            level: ix,
            layer,
            cell: [0, 0],
            inside: false,
            world_px: wp,
        };
    }
    let level = doc.level(ix);
    let (g, size) = crate::tools::hit_grid(doc, layer, ix);
    let g = g as f64;
    let cell = [
        ((wp[0] - level.world_px[0] as f64) / g).floor() as i32,
        ((wp[1] - level.world_px[1] as f64) / g).floor() as i32,
    ];
    CellHit {
        level: ix,
        layer,
        cell,
        inside: crate::tools::in_layer(cell, size),
        world_px: wp,
    }
}

/// The events a gesture sends, with its level, layer and brush.
pub fn gesture_events(
    doc: &Document,
    step: &GestureStep,
    active_layer: Option<usize>,
    current_entity_brush: Option<&crate::tools::EntityBrush>,
) -> anyhow::Result<GesturePlan> {
    let Some(tool) = parse_tool(&step.gesture) else {
        bail!("unknown tool {:?}", step.gesture);
    };
    let Some(ix) = doc.level_ix(&step.level) else {
        bail!("no level {:?}", step.level);
    };
    let layer = match &step.layer {
        Some(s) => doc
            .project
            .layers
            .iter()
            .position(|l| l.slug == s.as_str())
            .with_context(|| format!("no layer {s:?}"))?,
        // The entity tool's layer: the active one if it holds entities,
        // else the first entities layer.
        None if tool == Tool::Entity => active_layer
            .filter(|&l| doc.project.layers.get(l).is_some_and(|d| d.is_entities()))
            .or_else(|| crate::view::first_entities_layer(doc))
            .context("the project has no entities layer")?,
        // Select works with no layer at all, in project cells.
        None if tool == Tool::Select && doc.project.layers.is_empty() => crate::tools::NO_LAYER,
        None => active_layer.unwrap_or(0),
    };
    if layer >= doc.project.layers.len() && layer != crate::tools::NO_LAYER {
        bail!("the project has no layers");
    }
    if layer == crate::tools::NO_LAYER && step.brush.is_some() {
        bail!("`brush` needs a layer");
    }
    if tool != Tool::Entity && (step.ty.is_some() || step.fields.is_some()) {
        bail!("`type` and `fields` are for entity gestures");
    }
    if !matches!(tool, Tool::Entity | Tool::Select) && step.ctrl {
        bail!("`ctrl` is for entity gestures and select gestures");
    }
    if step.press && (tool != Tool::Select || step.handle.is_some()) {
        bail!("`press` is for select gestures without `handle`");
    }
    let brush = step
        .brush
        .as_ref()
        .map(|b| resolve_brush(doc, layer, b))
        .transpose()?;
    let entity_brush = if tool == Tool::Entity {
        entity_brush(doc, step, current_entity_brush)?
    } else {
        None
    };
    let Some((&first, rest)) = step.path.split_first() else {
        bail!("empty path");
    };
    let modifiers = egui::Modifiers {
        shift: step.shift,
        alt: step.alt,
        ctrl: step.ctrl,
        command: step.ctrl,
        ..Default::default()
    };
    let grab = match (tool, step.handle) {
        (Tool::Select, Some(e)) => Grab::Handle(e),
        (Tool::Select, None) => Grab::Level,
        _ => Grab::None,
    };
    let start = cell_hit(doc, ix, layer, first);
    let mut events = vec![ToolEvent::Begin {
        tool,
        at: start,
        modifiers,
        grab,
    }];
    let mut at = start;
    for &c in rest {
        at = cell_hit(doc, ix, layer, c);
        events.push(ToolEvent::Move {
            tool,
            start,
            at,
            modifiers,
        });
    }
    events.push(if step.cancel {
        ToolEvent::Cancel { tool }
    } else {
        ToolEvent::End {
            tool,
            start,
            at,
            modifiers,
        }
    });
    let (density, seed) = step
        .brush
        .as_ref()
        .map_or((None, None), ScriptBrush::scatter);
    Ok(GesturePlan {
        events,
        level: ix,
        layer,
        brush,
        entity_brush,
        density,
        seed,
    })
}

/// An entity gesture's brush: `type` (else the current brush's type) with
/// `fields` (each must be a field of the type); `None` when the step gives
/// neither.
fn entity_brush(
    doc: &Document,
    step: &GestureStep,
    current: Option<&crate::tools::EntityBrush>,
) -> anyhow::Result<Option<crate::tools::EntityBrush>> {
    if step.ty.is_none() && step.fields.is_none() {
        return Ok(None);
    }
    let ty = match &step.ty {
        Some(t) => t.clone(),
        None => current
            .map(|b| b.ty.to_string())
            .context("`fields` without `type` needs a current brush")?,
    };
    let def = doc
        .project
        .entity_def(&ty)
        .with_context(|| format!("no entity type {ty:?}"))?;
    let mut fields = std::collections::BTreeMap::new();
    for (k, v) in step.fields.clone().unwrap_or_default() {
        let f = def
            .field(&k)
            .with_context(|| format!("entity type {ty} has no field {k:?}"))?;
        fields.insert(f.slug.clone(), v);
    }
    Ok(Some(crate::tools::EntityBrush::with_fields(def, fields)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derived_layer_ui_steps_and_the_suppress_brush() {
        let s = parse(
            r#"[
              { "ui": "rules", "layer": "pillars", "rule": "big_room_pillars",
                "show_regions": true, "show_occupancy": false },
              { "ui": "layers", "active": "pillar_marks", "visible": { "pillars": false },
                "opacity": { "collision": 0.5 } },
              { "ui": "layers" },
              { "gesture": "brush", "level": "a", "layer": "pillar_marks", "path": [[0, 0]],
                "brush": "suppress" }
            ]"#,
        )
        .unwrap();
        let ScriptStep::Ui(UiStep::Rules(r)) = &s[0] else {
            panic!("{:?}", s[0])
        };
        assert_eq!(
            (r.show_regions, r.show_occupancy),
            (Some(true), Some(false))
        );
        let ScriptStep::Ui(UiStep::Layers(l)) = &s[1] else {
            panic!("{:?}", s[1])
        };
        assert_eq!(l.active.as_deref(), Some("pillar_marks"));
        assert!(!l.visible.as_ref().unwrap()["pillars"]);
        assert_eq!(l.opacity.as_ref().unwrap()["collision"], 0.5);
        assert_eq!(s[2], ScriptStep::Ui(UiStep::Layers(LayersStep::default())));
        let ScriptStep::Gesture(g) = &s[3] else {
            panic!()
        };
        assert_eq!(g.brush, Some(ScriptBrush::Suppress));
        // Unknown keys are refused, naming them.
        for (bad, key) in [
            (r#"{ "ui": "layers", "hidden": ["a"] }"#, "hidden"),
            (
                r#"{ "ui": "layers", "active": "a", "locked": { "a": true } }"#,
                "locked",
            ),
            (
                r#"{ "ui": "rules", "layer": "p", "show_region": true }"#,
                "show_region",
            ),
        ] {
            let e = format!("{:#}", parse(&format!("[{bad}]")).unwrap_err());
            assert!(e.contains(key), "{bad}: {e}");
        }
        // Wrong types too.
        assert!(parse(r#"[{ "ui": "layers", "opacity": { "a": "x" } }]"#).is_err());
        assert!(parse(r#"[{ "ui": "rules", "layer": "p", "show_occupancy": 1 }]"#).is_err());
    }

    #[test]
    fn parses_commands_and_gestures() {
        let s = parse(
            r#"[
              { "op": "paint_cells", "level": "a", "layer": "collision", "cells": [[1, 2, "solid"]] },
              { "gesture": "rect", "level": "a", "path": [[0, 0], [3, 3]], "shift": true,
                "brush": "water" },
              { "gesture": "brush", "level": "a", "path": [[0, 0]], "brush": "erase" },
              { "gesture": "brush", "level": "a", "path": [[0, 0]],
                "brush": { "tileset": "t", "tile": [3, 4] } },
              { "gesture": "brush", "level": "a", "path": [[0, 0]],
                "brush": { "tileset": "t", "rect": [1, 2, 3, 4] } },
              { "gesture": "brush", "level": "a", "path": [[0, 0]] }
            ]"#,
        )
        .unwrap();
        assert!(
            matches!(&s[0], ScriptStep::Command(c) if matches!(**c, Command::PaintCells { .. }))
        );
        let ScriptStep::Gesture(g) = &s[1] else {
            panic!()
        };
        assert!(g.shift && g.brush == Some(ScriptBrush::Value("water".into())));
        let brush = |i: usize| match &s[i] {
            ScriptStep::Gesture(g) => g.brush.clone(),
            _ => panic!(),
        };
        assert_eq!(brush(2), Some(ScriptBrush::Erase));
        let tiles = |rect| {
            Some(ScriptBrush::Tiles {
                tileset: "t".into(),
                rect,
            })
        };
        assert_eq!(brush(3), tiles([3, 4, 1, 1]));
        assert_eq!(brush(4), tiles([1, 2, 3, 4]));
        assert_eq!(brush(5), None);
        for bad in [
            "null",
            r#"{ "Value": 1 }"#,
            r#"{ "tileset": "t" }"#,
            r#"{ "tileset": "t", "tile": [0, 0], "rect": [0, 0, 1, 1] }"#,
            r#"{ "tileset": "t", "rect": [0, 0, 0, 1] }"#,
            "3",
        ] {
            let text = format!(
                r#"[{{ "gesture": "brush", "level": "a", "path": [[0, 0]], "brush": {bad} }}]"#
            );
            let e = parse(&text).unwrap_err();
            assert!(format!("{e:#}").contains("brush"), "{bad}: {e:#}");
        }
        let s = parse(
            r#"[{ "ui": "rules", "layer": "main_tiling", "level": "a", "rule": "walls__fill" }]"#,
        )
        .unwrap();
        assert!(
            matches!(&s[0], ScriptStep::Ui(UiStep::Rules(u)) if u.rule.as_deref() == Some("walls__fill"))
        );
        assert!(parse(r#"[{ "ui": "rules", "layer": "l", "colour": 1 }]"#).is_err());
        let UiStep::Rules(u) = UiStep::rule("main_tiling/r_146", None).unwrap() else {
            panic!()
        };
        assert_eq!(
            (u.layer.as_str(), u.rule.as_deref()),
            ("main_tiling", Some("r_146"))
        );
        assert!(UiStep::rule("main_tiling", None).is_err());
        let e = parse(r#"[{ "op": "paint_cels" }]"#).unwrap_err();
        assert!(format!("{e:#}").contains("item 0"), "{e:#}");
    }

    #[test]
    fn random_and_scatter_brush_forms() {
        let brush = |b: &str| -> anyhow::Result<Option<ScriptBrush>> {
            let s = parse(&format!(
                r#"[{{ "gesture": "brush", "level": "a", "path": [[0, 0]], "brush": {b} }}]"#
            ))?;
            match s.into_iter().next() {
                Some(ScriptStep::Gesture(g)) => Ok(g.brush),
                _ => panic!(),
            }
        };
        let alts = |v: &str| -> Vec<worlds_core::TileAlt> { serde_json::from_str(v).unwrap() };
        let random = brush(
            r#"{ "tileset": "t", "tiles": [[1, 2], [0, 0, 2, 2], { "rect": [3, 3, 1, 1], "weight": 4 }, { "stack": [[5, 5], [4, 4]] }] }"#,
        )
        .unwrap();
        assert_eq!(
            random,
            Some(ScriptBrush::Random {
                tileset: "t".into(),
                alternatives: alts(
                    r#"[[1, 2], [0, 0, 2, 2], { "rect": [3, 3, 1, 1], "weight": 4 }, { "stack": [[5, 5], [4, 4]] }]"#
                ),
            })
        );
        assert_eq!(random.unwrap().scatter(), (None, None));
        let s = brush(r#"{ "tileset": "t", "tile": [1, 1], "density": 25, "seed": 9 }"#)
            .unwrap()
            .unwrap();
        assert_eq!(s.scatter(), (Some(25), Some(9)));
        assert!(matches!(&s, ScriptBrush::Scatter { brush, .. }
            if **brush == ScriptBrush::Tiles { tileset: "t".into(), rect: [1, 1, 1, 1] }));
        // Existing forms are unchanged.
        assert_eq!(
            brush(r#"{ "tileset": "t", "rect": [0, 0, 2, 1] }"#).unwrap(),
            Some(ScriptBrush::Tiles {
                tileset: "t".into(),
                rect: [0, 0, 2, 1]
            })
        );
        for (bad, says) in [
            (r#"{ "tileset": "t", "tiles": [] }"#, "at least one"),
            (r#"{ "tileset": "t", "tiles": [[1]] }"#, "tiles[0]"),
            (
                r#"{ "tileset": "t", "tiles": [[1, 1]], "tile": [0, 0] }"#,
                "exactly one",
            ),
            (
                r#"{ "tileset": "t", "tile": [0, 0], "density": 0 }"#,
                "density",
            ),
            (
                r#"{ "tileset": "t", "tile": [0, 0], "density": 101 }"#,
                "density",
            ),
        ] {
            let e = brush(bad).unwrap_err();
            assert!(format!("{e:#}").contains(says), "{bad}: {e:#}");
        }
    }

    #[test]
    fn ui_steps_are_tagged_on_ui() {
        let s = parse(
            r#"[
              { "ui": "palette", "search": "door", "favorites": ["a"], "recent": [],
                "collapsed": ["/recent", "Puzzle"], "chips": ["object"], "view": "list",
                "brush": "a", "tool": true, "enter": true },
              { "ui": "palette" },
              { "ui": "select", "entities": ["abcdefghij"] },
              { "ui": "types", "entity": "door" },
              { "ui": "types", "level_fields": true },
              { "gesture": "entity", "level": "a", "path": [[1, 2]], "type": "door",
                "fields": { "channel": 2 }, "ctrl": true }
            ]"#,
        )
        .unwrap();
        let ScriptStep::Ui(UiStep::Palette(p)) = &s[0] else {
            panic!("{:?}", s[0])
        };
        assert_eq!(p.search.as_deref(), Some("door"));
        assert_eq!(p.view, Some(crate::tools::PaletteView::List));
        assert_eq!(p.tool, Some(true));
        assert_eq!(p.enter, Some(true));
        assert!(
            matches!(&s[1], ScriptStep::Ui(UiStep::Palette(p)) if *p == PaletteStep::default())
        );
        assert!(matches!(&s[2], ScriptStep::Ui(UiStep::Select(s)) if s.entities.len() == 1));
        assert!(
            matches!(&s[3], ScriptStep::Ui(UiStep::Types(t)) if t.entity.as_deref() == Some("door"))
        );
        assert!(matches!(&s[4], ScriptStep::Ui(UiStep::Types(t)) if t.level_fields == Some(true)));
        let ScriptStep::Gesture(g) = &s[5] else {
            panic!()
        };
        assert_eq!(parse_tool(&g.gesture), Some(Tool::Entity));
        assert!(g.ctrl && g.ty.as_deref() == Some("door"));
        // Unknown keys, per variant, and unknown uis are errors.
        for bad in [
            r#"{ "ui": "palette", "serch": "x" }"#,
            r#"{ "ui": "palette", "view": "tiles" }"#,
            r#"{ "ui": "select", "entities": [], "items": [] }"#,
            r#"{ "ui": "types", "entity": "a", "x": 1 }"#,
            r#"{ "ui": "nope" }"#,
            r#"{ "ui": "rules", "layer": "l", "search": "x" }"#,
        ] {
            assert!(parse(&format!("[{bad}]")).is_err(), "{bad}");
        }
    }

    #[test]
    fn annotation_steps_parse() {
        let s = parse(
            r#"[
              { "undo": 2 },
              { "redo": 1 },
              { "ui": "select", "notes": ["pinstatue2"], "levels": ["shaft"],
                "tiles": { "level": "crevasse", "layer": "decor_back", "cells": [[4, 1]] } },
              { "ui": "notes", "hidden_tags": ["layout"], "show_designed": true, "guides_open": true },
              { "ui": "focus", "note": "pinstatue2" },
              { "gesture": "note", "kind": "path", "world_px": [[0, 0], [64, 0]], "keys": ["backspace"],
                "end": "double_click" },
              { "gesture": "measure", "world_px": [[0, 0], [64, 0]] },
              { "gesture": "select", "world_px": [[-50, -50], [700, 500]], "press": true, "ctrl": true },
              { "ui": "duplicate" },
              { "ui": "delete" },
              { "ui": "delete", "confirm": true }
            ]"#,
        )
        .unwrap();
        assert_eq!(s[0], ScriptStep::History { redo: false, n: 2 });
        assert_eq!(s[1], ScriptStep::History { redo: true, n: 1 });
        let ScriptStep::Ui(UiStep::Select(sel)) = &s[2] else {
            panic!()
        };
        assert_eq!(sel.tiles.as_ref().unwrap().cells, [[4, 1]]);
        assert!(matches!(&s[3], ScriptStep::Ui(UiStep::Notes(n)) if n.show_designed == Some(true)));
        assert!(matches!(&s[4], ScriptStep::Ui(UiStep::Focus(f)) if f.note.is_some()));
        assert!(matches!(&s[5], ScriptStep::NoteGesture(g) if g.keys == ["backspace"]));
        assert!(matches!(&s[6], ScriptStep::NoteGesture(g) if g.gesture == "measure"));
        assert!(matches!(&s[7], ScriptStep::WorldSelect(w) if w.ctrl && w.world_px.len() == 2));
        assert_eq!(s[8], ScriptStep::Ui(UiStep::Duplicate(DuplicateStep {})));
        assert_eq!(
            s[9],
            ScriptStep::Ui(UiStep::Delete(DeleteStep { confirm: false }))
        );
        assert_eq!(
            s[10],
            ScriptStep::Ui(UiStep::Delete(DeleteStep { confirm: true }))
        );
        for bad in [
            r#"{ "undo": "two" }"#,
            r#"{ "gesture": "select", "world_px": [[0, 0]] }"#,
            r#"{ "gesture": "note", "world_px": [[0, 0]], "level": "a" }"#,
            r#"{ "ui": "notes", "hiden_tags": [] }"#,
            r#"{ "ui": "duplicate", "n": 2 }"#,
            r#"{ "ui": "delete", "confirm": "yes" }"#,
        ] {
            assert!(parse(&format!("[{bad}]")).is_err(), "{bad}");
        }
    }
}
