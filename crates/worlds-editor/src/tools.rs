//! The seam between the viewport and editing.
//!
//! The viewport turns pointer input into [`ToolEvent`]s (a gesture is
//! `Begin`, any number of `Move`s, then `End` or `Cancel`) and keeps a
//! [`ToolInput`] snapshot for previews. Once per frame the app hands the
//! frame's events to an [`EditorCommands`] implementation along with a
//! [`CommandContext`] (mutable document and view state, and a list to push
//! [`Invalidation`]s into). The app then applies the invalidations to the
//! viewport. The viewport never mutates the document itself.
//!
//! The real implementation is [`crate::edit::Editing`], on top of
//! `worlds_core::command`. [`NoCommands`] is a stand-in that edits nothing.
//!
//! The geometry helpers at the bottom (lines, rects, stamps, level resize)
//! are shared by the previews and the command layer, so what you see while
//! dragging is what gets applied.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use worlds_core::{Command, Slug};

use crate::doc::{Document, LevelIx};
use crate::view::ViewState;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum Tool {
    #[default]
    Select,
    Brush,
    Rect,
    Line,
    Fill,
    Eraser,
    Hand,
    /// Place entities of the palette's brush type (entities layers).
    Entity,
    /// Add annotations: notes, pins, paths, regions (N).
    Note,
    /// Measure distances, in tiles and pixels; never a command (M).
    Measure,
}

impl Tool {
    pub const ALL: [Tool; 10] = [
        Tool::Select,
        Tool::Brush,
        Tool::Rect,
        Tool::Line,
        Tool::Fill,
        Tool::Eraser,
        Tool::Entity,
        Tool::Note,
        Tool::Measure,
        Tool::Hand,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Tool::Select => "Select",
            Tool::Brush => "Brush",
            Tool::Rect => "Rectangle",
            Tool::Line => "Line",
            Tool::Fill => "Fill",
            Tool::Eraser => "Eraser",
            Tool::Hand => "Hand",
            Tool::Entity => "Entity",
            Tool::Note => "Note",
            Tool::Measure => "Measure",
        }
    }

    /// The single-key shortcut (holding Space also pans with any tool).
    pub fn shortcut(self) -> &'static str {
        match self {
            Tool::Select => "V",
            Tool::Brush => "B",
            Tool::Rect => "R",
            Tool::Line => "L",
            Tool::Fill => "G",
            Tool::Eraser => "E",
            Tool::Hand => "H",
            Tool::Entity => "T",
            Tool::Note => "N",
            Tool::Measure => "M",
        }
    }

    pub fn key(self) -> egui::Key {
        match self {
            Tool::Select => egui::Key::V,
            Tool::Brush => egui::Key::B,
            Tool::Rect => egui::Key::R,
            Tool::Line => egui::Key::L,
            Tool::Fill => egui::Key::G,
            Tool::Eraser => egui::Key::E,
            Tool::Hand => egui::Key::H,
            Tool::Entity => egui::Key::T,
            Tool::Note => egui::Key::N,
            Tool::Measure => egui::Key::M,
        }
    }

    /// The Paint group's modes, in the mode strip's order. The toolbar shows
    /// them as one button, Paint; each is still its own tool everywhere else
    /// (gestures, scripts, `ui.json`, undo labels).
    pub const PAINT_MODES: [Tool; 4] = [Tool::Brush, Tool::Line, Tool::Rect, Tool::Fill];

    /// Whether this tool is one of the Paint group's modes.
    pub fn is_paint_mode(self) -> bool {
        Tool::PAINT_MODES.contains(&self)
    }

    /// Tools that edit cells of the active layer.
    pub fn paints(self) -> bool {
        matches!(
            self,
            Tool::Brush | Tool::Rect | Tool::Line | Tool::Fill | Tool::Eraser
        )
    }
}

/// One button of the toolbar: a tool, or the Paint group (its button
/// selects the last Paint mode; the mode strip picks among them).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolButton {
    Tool(Tool),
    Paint,
}

impl ToolButton {
    /// The toolbar, in order: Paint sits where Brush was.
    pub const ALL: [ToolButton; 7] = [
        ToolButton::Tool(Tool::Select),
        ToolButton::Paint,
        ToolButton::Tool(Tool::Eraser),
        ToolButton::Tool(Tool::Entity),
        ToolButton::Tool(Tool::Note),
        ToolButton::Tool(Tool::Measure),
        ToolButton::Tool(Tool::Hand),
    ];

    /// Whether the button shows as selected with `tool` active.
    pub fn shows(self, tool: Tool) -> bool {
        match self {
            ToolButton::Tool(t) => t == tool,
            ToolButton::Paint => tool.is_paint_mode(),
        }
    }
}

/// A point under the pointer, resolved against a level and layer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CellHit {
    pub level: LevelIx,
    /// Index into `project.layers` (the active layer).
    pub layer: usize,
    /// Cell of that layer's grid, relative to the level. May be outside the
    /// level during a drag that leaves it.
    pub cell: [i32; 2],
    /// Whether `cell` is inside the level.
    pub inside: bool,
    /// World pixel under the pointer.
    pub world_px: [f64; 2],
}

/// What a Select gesture took hold of.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Grab {
    /// Not a select gesture (painting tools).
    #[default]
    None,
    /// The level body: dragging moves it.
    Level,
    /// A resize handle. Per axis, the edge that moves: `-1` left/top, `1`
    /// right/bottom, `0` neither. The opposite edge stays put, so the
    /// `resize_level` anchor is the negation.
    Handle([i8; 2]),
    /// The selection (the press was on a selected item, or on an item it
    /// selected: an entity, an annotation, a tile stack, a selected level):
    /// dragging moves everything selected by one delta
    /// ([`crate::annot::plan_move`]), applied on release as one entry. A
    /// release where the press was (a click) on an item that was already
    /// selected narrows the selection to it, or with Ctrl takes it out.
    Selection {
        /// The item under the press was selected before the press.
        was_selected: bool,
    },
    /// A marquee from empty space in the active level: on release, the
    /// visible, unlocked entities, the active tiles layer's stacks and the
    /// unlocked annotations the rectangle touches become the selection
    /// (with Ctrl, they are added).
    Marquee,
    /// A marquee from empty world space (outside every level): on release,
    /// the levels and the unlocked world annotations it touches become the
    /// selection (Ctrl adds). The gesture's `CellHit`s carry some level
    /// (the active one, else the first); only their `world_px` counts.
    WorldMarquee,
}

/// What the active tool sees this frame. Read by previews and from
/// `Viewport::tool_input`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ToolInput {
    pub tool: Tool,
    /// What the gesture in progress grabbed (Select).
    pub grab: Grab,
    /// The cell under the pointer in the active level (or the level under
    /// the pointer if none is active).
    pub hover: Option<CellHit>,
    /// The gesture in progress, if any: where it started and where it is now.
    pub drag: Option<(CellHit, CellHit)>,
    pub modifiers: egui::Modifiers,
}

/// Pointer gestures of the active tool, in order. Cells always belong to
/// the level the gesture started in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ToolEvent {
    /// Primary button went down over a level (or a resize handle).
    Begin {
        tool: Tool,
        at: CellHit,
        modifiers: egui::Modifiers,
        grab: Grab,
    },
    /// The pointer moved to a different cell while held (with Select: moved
    /// at all, so level drags can snap to the project grid).
    Move {
        tool: Tool,
        start: CellHit,
        at: CellHit,
        modifiers: egui::Modifiers,
    },
    /// Primary button released: commit.
    End {
        tool: Tool,
        start: CellHit,
        at: CellHit,
        modifiers: egui::Modifiers,
    },
    /// Escape, a tool switch, or a reload mid-gesture: discard.
    Cancel { tool: Tool },
}

/// What a palette click selected, per layer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Brush {
    /// IntGrid value index (1-based, as stored in cells).
    Value(u16),
    /// A rectangle of atlas tiles `[col, row, w, h]` from `tileset`.
    Tiles { tileset: Slug, rect: [u32; 4] },
    /// Paint empty (int_grid cells, or no tiles) with any painting tool. On
    /// a derived layer: remove overrides (back to the rules).
    Erase,
    /// Derived layers only: override cells to empty (`"!"`, suppress what
    /// the rules would put there).
    Suppress,
}

/// What the entity tool places: a type and the field values every new
/// instance gets (Alt-click copies them from an instance).
#[derive(Debug, Clone, PartialEq)]
pub struct EntityBrush {
    pub ty: Slug,
    pub fields: std::collections::BTreeMap<Slug, serde_json::Value>,
    /// The kind (`FieldType::name`) and `array` of each field in `fields`
    /// when the value was taken, so [`EntityBrush::refit`] can tell a
    /// retyped field.
    kinds: std::collections::BTreeMap<Slug, (&'static str, bool)>,
}

impl EntityBrush {
    pub fn of(ty: Slug) -> Self {
        EntityBrush {
            ty,
            fields: Default::default(),
            kinds: Default::default(),
        }
    }

    /// A brush of type `def` with `fields`, keeping only the keys `def`
    /// defines (an instance's unknown keys aren't copied to new ones).
    pub fn with_fields(
        def: &worlds_core::EntityDef,
        fields: std::collections::BTreeMap<Slug, serde_json::Value>,
    ) -> Self {
        let mut b = EntityBrush::of(def.slug.clone());
        for (k, v) in fields {
            if let Some(f) = def.field(k.as_str()) {
                b.kinds.insert(k.clone(), (f.ty.name(), f.array));
                b.fields.insert(k, v);
            }
        }
        b
    }

    /// Drop the stored values `def` (this brush's type, after a definition
    /// edit) no longer has a field of the same kind for: a removed,
    /// renamed or retyped field. Placing writes the rest.
    pub fn refit(&mut self, def: &worlds_core::EntityDef) {
        let kinds = &mut self.kinds;
        self.fields.retain(|k, _| {
            let fits = def
                .field(k.as_str())
                .is_some_and(|f| kinds.get(k) == Some(&(f.ty.name(), f.array)));
            if !fits {
                kinds.remove(k);
            }
            fits
        });
    }
}

/// Grid or list view of the palette's catalogue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaletteView {
    #[default]
    Grid,
    List,
}

/// The entity palette's remembered state (`.worlds/ui.json`
/// `entity_palette`; the brush is saved as its type slug). The command
/// layer reads the brush and adds placed types to `recent`; a renamed type
/// is followed everywhere ([`EntityPalette::renamed`]).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct EntityPalette {
    /// Starred type slugs, in the order they were starred.
    pub favorites: Vec<Slug>,
    /// The last [`RECENT_MAX`] types placed, most recent first.
    pub recent: Vec<Slug>,
    /// Collapsed parts (`/favorites`, `/recent`, `/level`, `/catalogue`)
    /// and catalogue category paths (`Puzzle`, `Puzzle/Doors`,
    /// `Uncategorized`).
    pub collapsed: std::collections::BTreeSet<String>,
    /// Tag chips: the catalogue shows types carrying every one of them.
    pub chips: Vec<String>,
    pub view: PaletteView,
    pub brush: Option<EntityBrush>,
}

/// How many types `recent` keeps.
pub const RECENT_MAX: usize = 8;

impl EntityPalette {
    /// A type was placed: it goes to the front of `recent`.
    pub fn placed(&mut self, ty: &Slug) {
        self.recent.retain(|s| s != ty);
        self.recent.insert(0, ty.clone());
        self.recent.truncate(RECENT_MAX);
    }

    /// Entity type `from` is now called `to` (a rename, or its undo).
    pub fn renamed(&mut self, from: &Slug, to: &Slug) {
        for s in self.favorites.iter_mut().chain(self.recent.iter_mut()) {
            if s == from {
                *s = to.clone();
            }
        }
        if let Some(b) = &mut self.brush
            && b.ty == *from
        {
            b.ty = to.clone();
        }
    }

    /// The project's definitions changed: a brush whose type is gone is
    /// dropped, and one whose fields changed keeps only the values that
    /// still fit ([`EntityBrush::refit`]).
    pub fn refit_brush(&mut self, project: &worlds_core::Project) {
        let Some(b) = &mut self.brush else {
            return;
        };
        match project.entity_def(b.ty.as_str()) {
            Some(def) => b.refit(def),
            None => self.brush = None,
        }
    }

    /// Drop slugs `project` has no type for (after loading `ui.json`).
    pub fn retain_known(&mut self, project: &worlds_core::Project) {
        let known = |s: &Slug| project.entity_def(s.as_str()).is_some();
        self.favorites.retain(known);
        self.recent.retain(known);
        let mut seen = std::collections::HashSet::new();
        self.favorites.retain(|s| seen.insert(s.clone()));
        let mut seen = std::collections::HashSet::new();
        self.recent.retain(|s| seen.insert(s.clone()));
        self.recent.truncate(RECENT_MAX);
        if self.brush.as_ref().is_some_and(|b| !known(&b.ty)) {
            self.brush = None;
        }
    }

    pub fn is_favorite(&self, ty: &Slug) -> bool {
        self.favorites.contains(ty)
    }

    pub fn toggle_favorite(&mut self, ty: &Slug) {
        if let Some(i) = self.favorites.iter().position(|s| s == ty) {
            self.favorites.remove(i);
        } else {
            self.favorites.push(ty.clone());
        }
    }
}

/// The current brush of each layer, by layer slug.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PaletteSelection {
    pub brushes: HashMap<String, Brush>,
}

impl PaletteSelection {
    pub fn brush(&self, layer: &str) -> Option<&Brush> {
        self.brushes.get(layer)
    }
}

/// Something the viewport must redraw because the document changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invalidation {
    /// Cells `[x0, y0, x1, y1)` (layer grid) of one layer in one level.
    Cells {
        level: LevelIx,
        layer: Slug,
        rect: [i32; 4],
    },
    /// A whole layer of one level (e.g. an auto layer after a re-bake).
    Layer { level: LevelIx, layer: Slug },
    /// A level's content and shape (moved, resized).
    Level(LevelIx),
    /// Levels were added or removed, or definitions changed: rebuild the
    /// whole scene.
    All,
    /// Tileset definitions changed (the document re-read its images): upload
    /// every tileset texture again, then rebuild the whole scene.
    Tilesets,
}

/// What an [`EditorCommands`] call may touch.
pub struct CommandContext<'a> {
    pub doc: &'a mut Document,
    /// Active level and layer, layer visibility and locks, the palette
    /// (alt-click sampling writes it), selection and the status message.
    pub view: &'a mut ViewState,
    /// Push what changed; the app forwards it to the viewport after the call.
    pub invalidate: &'a mut Vec<Invalidation>,
}

/// The editing backend. The app calls `tool_events` once per frame (even
/// with no events) and the rest from the toolbar, shortcuts, panels and the
/// command palette.
pub trait EditorCommands {
    fn tool_events(&mut self, cx: &mut CommandContext<'_>, events: &[ToolEvent]);
    /// Apply one command as its own undo entry (inspector edits, level
    /// operations, `--script`).
    fn apply(&mut self, _cx: &mut CommandContext<'_>, _cmd: Command) -> Result<(), String> {
        Err("editing is not available".into())
    }
    fn can_undo(&self) -> bool {
        false
    }
    fn can_redo(&self) -> bool {
        false
    }
    /// What undo would undo ("Paint collision").
    fn undo_label(&self) -> Option<String> {
        None
    }
    fn redo_label(&self) -> Option<String> {
        None
    }
    fn undo(&mut self, _cx: &mut CommandContext<'_>) {}
    fn redo(&mut self, _cx: &mut CommandContext<'_>) {}
    /// Edits not yet saved (the reload and close prompts, the title and the
    /// save button use it).
    fn has_unsaved(&self) -> bool {
        false
    }
    fn save(&mut self, _cx: &mut CommandContext<'_>) -> anyhow::Result<()> {
        Ok(())
    }
    /// The document was replaced (opened or reloaded from disk): drop
    /// history and caches that refer to the old one.
    fn reloaded(&mut self) {}
    /// A tileset image changed and these `auto` layers draw from it (see
    /// [`Document::tileset_changed`](crate::doc::Document::tileset_changed)):
    /// drop cached bakes of them, which were made against the old image.
    fn tileset_changed(&mut self, _layers: &[Slug]) {}
    /// The real command layer, when this is one: the live API borrows its
    /// history and bake cache ([`crate::live`]).
    fn editing(&mut self) -> Option<&mut crate::edit::Editing> {
        None
    }
    fn editing_ref(&self) -> Option<&crate::edit::Editing> {
        None
    }
}

/// A stand-in that edits nothing: tools show previews only.
#[derive(Debug, Default)]
pub struct NoCommands;

impl EditorCommands for NoCommands {
    fn tool_events(&mut self, _cx: &mut CommandContext<'_>, events: &[ToolEvent]) {
        for e in events {
            if let ToolEvent::End {
                tool, start, at, ..
            } = e
            {
                tracing::debug!(
                    "{} gesture {:?} -> {:?} (no command layer)",
                    tool.name(),
                    start.cell,
                    at.cell
                );
            }
        }
    }
}

/// Cells of a line (Bresenham, both ends included); lives in worlds-core so
/// the live API's `grid.line` draws exactly what the line tool does.
pub use worlds_core::command::line_cells;

/// `[x0, y0, x1, y1)` covering cells `a` and `b`.
pub fn cell_rect(a: [i32; 2], b: [i32; 2]) -> [i32; 4] {
    [
        a[0].min(b[0]),
        a[1].min(b[1]),
        a[0].max(b[0]) + 1,
        a[1].max(b[1]) + 1,
    ]
}

/// The cells of the rect `[x0, y0, x1, y1)`: all of them, or just the
/// border (`outline`). Row-major.
pub fn rect_cells(r: [i32; 4], outline: bool, out: &mut Vec<[i32; 2]>) {
    out.clear();
    for y in r[1]..r[3] {
        for x in r[0]..r[2] {
            if !outline || x == r[0] || y == r[1] || x == r[2] - 1 || y == r[3] - 1 {
                out.push([x, y]);
            }
        }
    }
}

/// Clip `[x0, y0, x1, y1)` to a layer of `size` cells.
pub fn clip_rect(r: [i32; 4], size: [u32; 2]) -> [i32; 4] {
    [
        r[0].max(0),
        r[1].max(0),
        r[2].min(size[0] as i32),
        r[3].min(size[1] as i32),
    ]
}

/// A [`CellHit`] layer index for a gesture made where the project has no
/// layer at all: the Select tool still clicks, moves and resizes levels
/// there, in cells of the project grid ([`hit_grid`]).
pub const NO_LAYER: usize = usize::MAX;

/// The cell size in pixels and the size in cells of project layer `layer`
/// in level `ix`; for [`NO_LAYER`] (or any index past the last layer) the
/// project grid and the level's own size.
pub fn hit_grid(doc: &Document, layer: usize, ix: LevelIx) -> (u32, [u32; 2]) {
    let level = doc.level(ix);
    match doc.project.layers.get(layer) {
        Some(def) => (
            doc.project.layer_grid(def).max(1),
            doc.project.layer_cells(def, level),
        ),
        None => (doc.project.grid_size.max(1), level.size),
    }
}

/// Whether a cell is inside a layer of `size` cells.
pub fn in_layer(c: [i32; 2], size: [u32; 2]) -> bool {
    c[0] >= 0 && c[1] >= 0 && (c[0] as u32) < size[0] && (c[1] as u32) < size[1]
}

/// A tile stamp: a `[col, row, w, h]` block of atlas tiles, laid out on a
/// layer whose cells may be smaller than the tiles. Each tile covers
/// `stride` cells per axis, the whole stamp `footprint()` cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stamp {
    pub rect: [u32; 4],
    pub stride: [u32; 2],
}

impl Stamp {
    /// `tile_size` in pixels, `grid` the layer's cell size.
    pub fn new(rect: [u32; 4], tile_size: [u32; 2], grid: u32) -> Self {
        let g = grid.max(1);
        Stamp {
            rect: [rect[0], rect[1], rect[2].max(1), rect[3].max(1)],
            stride: [(tile_size[0] / g).max(1), (tile_size[1] / g).max(1)],
        }
    }

    /// Cells the whole stamp covers.
    pub fn footprint(&self) -> [i32; 2] {
        [
            (self.rect[2] * self.stride[0]) as i32,
            (self.rect[3] * self.stride[1]) as i32,
        ]
    }

    /// The stamp origin (top-left cell) that `cell` falls in, when stamps
    /// tile the layer starting at `origin`. A stroke repeats the stamp on
    /// this lattice, so dragging along lays copies edge to edge.
    pub fn anchor(&self, origin: [i32; 2], cell: [i32; 2]) -> [i32; 2] {
        let f = self.footprint();
        [
            origin[0] + (cell[0] - origin[0]).div_euclid(f[0]) * f[0],
            origin[1] + (cell[1] - origin[1]).div_euclid(f[1]) * f[1],
        ]
    }

    /// The tile that goes at `cell` when the stamp tiles the layer from
    /// `origin`, or `None` if `cell` is covered by a tile placed on another
    /// cell (tiles bigger than a cell sit on every `stride`-th cell).
    pub fn tile_at(&self, origin: [i32; 2], cell: [i32; 2]) -> Option<[u32; 2]> {
        let d = [cell[0] - origin[0], cell[1] - origin[1]];
        let s = [self.stride[0] as i32, self.stride[1] as i32];
        if d[0].rem_euclid(s[0]) != 0 || d[1].rem_euclid(s[1]) != 0 {
            return None;
        }
        let i = (d[0].div_euclid(s[0])).rem_euclid(self.rect[2] as i32) as u32;
        let j = (d[1].div_euclid(s[1])).rem_euclid(self.rect[3] as i32) as u32;
        Some([self.rect[0] + i, self.rect[1] + j])
    }

    /// `(cell, tile)` for every tile of one stamp placed at `anchor`.
    pub fn tiles(&self, anchor: [i32; 2], out: &mut Vec<([i32; 2], [u32; 2])>) {
        for j in 0..self.rect[3] {
            for i in 0..self.rect[2] {
                out.push((
                    [
                        anchor[0] + (i * self.stride[0]) as i32,
                        anchor[1] + (j * self.stride[1]) as i32,
                    ],
                    [self.rect[0] + i, self.rect[1] + j],
                ));
            }
        }
    }
}

/// Where the entity tool puts an instance: what `add_entity` (or the
/// limit's `move_entity`) gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    /// Cell of the entities layer's grid holding the pivot.
    pub cell: [i32; 2],
    pub offset_px: [i32; 2],
    /// Per-instance size in project cells, when it differs from the type's.
    pub size: Option<[u32; 2]>,
}

/// Whether Ctrl (or Cmd) is held: pixel placement.
pub fn pixel_mod(m: egui::Modifiers) -> bool {
    m.ctrl || m.command || m.mac_cmd
}

/// The cell and offset that put an entity's pivot point at level pixel
/// `p` (one axis) on a layer of `g` px cells: `(cell + pivot) * g + offset
/// = p`, with the offset as small as it can be.
fn pivot_cell(p: f64, pivot: f64, g: f64) -> (i32, i32) {
    let c = (p / g - pivot).round();
    (c as i32, (p - (c + pivot) * g).round() as i32)
}

/// Where an entity-tool gesture from `start` to `at` (both on the same
/// entities layer of the same level) places an instance of `def`:
///
/// - a click or a drag: the pivot in the release cell;
/// - Ctrl: the pivot at the pointer pixel; `cell` is the cell containing
///   the pointer, the rest goes in `offset_px`;
/// - Shift, on the axes where the type is resizable: the box spans the
///   project cells from the start to the release (at least one); the other
///   axes keep the type's size, the pivot in the start cell.
pub fn entity_placement(
    doc: &Document,
    def: &worlds_core::EntityDef,
    start: &CellHit,
    at: &CellHit,
    m: egui::Modifiers,
) -> Placement {
    let ldef = &doc.project.layers[at.layer];
    let g = doc.project.layer_grid(ldef).max(1) as f64;
    let pg = doc.project.grid_size.max(1) as f64;
    let level = doc.level(at.level);
    let lp = |h: &CellHit| {
        [
            h.world_px[0] - level.world_px[0] as f64,
            h.world_px[1] - level.world_px[1] as f64,
        ]
    };
    if m.shift && (def.resizable[0] || def.resizable[1]) {
        let (s, e) = (lp(start), lp(at));
        let mut cell = [0; 2];
        let mut offset = [0; 2];
        let mut size = def.size;
        for a in 0..2 {
            let box_min = if def.resizable[a] {
                let ps = (s[a] / pg).floor();
                let pe = (e[a] / pg).floor();
                size[a] = (ps - pe).abs() as u32 + 1;
                ps.min(pe) * pg
            } else {
                let pivot_px = (start.cell[a] as f64 + def.pivot[a]) * g;
                pivot_px - def.pivot[a] * size[a] as f64 * pg
            };
            let p = box_min + def.pivot[a] * size[a] as f64 * pg;
            (cell[a], offset[a]) = pivot_cell(p, def.pivot[a], g);
        }
        return Placement {
            cell,
            offset_px: offset,
            size: (size != def.size).then_some(size),
        };
    }
    if pixel_mod(m) {
        let p = lp(at);
        let cell = [(p[0] / g).floor() as i32, (p[1] / g).floor() as i32];
        let off = |a: usize| (p[a] - (cell[a] as f64 + def.pivot[a]) * g).round() as i32;
        return Placement {
            cell,
            offset_px: [off(0), off(1)],
            size: None,
        };
    }
    Placement {
        cell: at.cell,
        offset_px: [0, 0],
        size: None,
    }
}

/// Level-local pixel rect `[x, y, w, h]` of instance `inst` of `def` on a
/// layer of `layer_grid` px cells: `worlds_core`'s
/// [`EntityDef::pixel_rect`](worlds_core::EntityDef::pixel_rect), the one
/// source of truth (the pivot point sits at `(cell + pivot) * layer_grid +
/// offset_px`, the box is `size * project_grid`; `docs/format.md`,
/// "Position math"). Drawing, hit-testing, the ghost, the marquee and
/// snapshots all go through it.
pub fn entity_box(
    def: &worlds_core::EntityDef,
    inst: &worlds_core::EntityInstance,
    layer_grid: u32,
    project_grid: u32,
) -> [f64; 4] {
    let [x, y, w, h] = def.pixel_rect(inst, layer_grid, project_grid);
    [x as f64, y as f64, w as f64, h as f64]
}

/// [`entity_box`] of where a [`Placement`] would put an instance of `def`.
pub fn placement_box(
    def: &worlds_core::EntityDef,
    p: &Placement,
    layer_grid: u32,
    project_grid: u32,
) -> [f64; 4] {
    let inst = worlds_core::EntityInstance {
        id: worlds_core::Id::new("ghostghost").expect("valid id"),
        ty: def.slug.clone(),
        cell: p.cell,
        offset_px: p.offset_px,
        size: p.size,
        fields: Default::default(),
    };
    entity_box(def, &inst, layer_grid, project_grid)
}

/// Keep `offset_px` under one cell of a layer of `g` px cells: whenever
/// `|offset|` reaches `g` on an axis, the whole cells go into `cell` (the
/// offset keeps its sign), so `cell` stays the cell the entity is really
/// in. The pixel position `cell * g + offset` doesn't change.
pub fn fold_offset(cell: [i32; 2], offset: [i32; 2], g: u32) -> ([i32; 2], [i32; 2]) {
    let g = g.max(1) as i32;
    let mut c = cell;
    let mut o = offset;
    for a in 0..2 {
        let q = o[a] / g;
        c[a] += q;
        o[a] -= q * g;
    }
    (c, o)
}

/// Where an entity at `cell` + `offset_px` on a layer of `g` px cells goes
/// when the Select tool drags it by `delta` world pixels: by whole cells
/// (the nearest), keeping its offset; with `pixel` (Ctrl) by exact pixels,
/// the offset folded into the cell ([`fold_offset`]).
pub fn dragged_position(
    cell: [i32; 2],
    offset: [i32; 2],
    g: u32,
    delta: [f64; 2],
    pixel: bool,
) -> ([i32; 2], [i32; 2]) {
    let gf = g.max(1) as f64;
    if pixel {
        let d = [delta[0].round() as i32, delta[1].round() as i32];
        fold_offset(cell, [offset[0] + d[0], offset[1] + d[1]], g)
    } else {
        let d = [
            (delta[0] / gf).round() as i32,
            (delta[1] / gf).round() as i32,
        ];
        ([cell[0] + d[0], cell[1] + d[1]], offset)
    }
}

/// An arrow-key nudge of `d` (each -1, 0 or 1): whole cells, or with
/// `pixel` (Shift) one pixel into `offset_px`, folded ([`fold_offset`]).
pub fn nudged(
    cell: [i32; 2],
    offset: [i32; 2],
    g: u32,
    d: [i32; 2],
    pixel: bool,
) -> ([i32; 2], [i32; 2]) {
    if pixel {
        fold_offset(cell, [offset[0] + d[0], offset[1] + d[1]], g)
    } else {
        ([cell[0] + d[0], cell[1] + d[1]], offset)
    }
}

/// Where a level moves when dragged by `delta` world pixels: snapped to
/// whole project cells.
pub fn snap_move(world_px: [i64; 2], delta: [f64; 2], grid: u32) -> [i64; 2] {
    let g = grid.max(1) as f64;
    [
        world_px[0] + ((delta[0] / g).round() * g) as i64,
        world_px[1] + ((delta[1] / g).round() * g) as i64,
    ]
}

/// A level resized by dragging the `edges` handle (see [`Grab::Handle`])
/// by `delta` world pixels: the new `(world_px, size)`, at least one cell
/// per axis. The anchor to send with `resize_level` is `-edges`.
pub fn resize_level(
    world_px: [i64; 2],
    size: [u32; 2],
    edges: [i8; 2],
    delta: [f64; 2],
    grid: u32,
) -> ([i64; 2], [u32; 2]) {
    let g = grid.max(1) as i64;
    let mut px = world_px;
    let mut sz = size;
    for a in 0..2 {
        let d = (delta[a] / g as f64).round() as i64;
        let old = size[a] as i64;
        let new = match edges[a] {
            1 => old + d,
            -1 => old - d,
            _ => old,
        }
        .max(1);
        if edges[a] == -1 {
            px[a] = world_px[a] + (old - new) * g;
        }
        sz[a] = new as u32;
    }
    (px, sz)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_endpoints_and_continuity() {
        let mut v = Vec::new();
        for (a, b) in [([0, 0], [5, 2]), ([3, 3], [-2, 7]), ([1, 1], [1, 1])] {
            line_cells(a, b, &mut v);
            assert_eq!(v[0], a);
            assert_eq!(*v.last().unwrap(), b);
            for w in v.windows(2) {
                assert!((w[0][0] - w[1][0]).abs() <= 1 && (w[0][1] - w[1][1]).abs() <= 1);
            }
        }
    }

    #[test]
    fn rect_is_inclusive() {
        assert_eq!(cell_rect([4, 1], [2, 3]), [2, 1, 5, 4]);
    }

    #[test]
    fn line_interpolates_fast_strokes() {
        // A fast mouse move jumps from (0, 0) to (6, 3): the stroke must
        // still be a connected run of exactly max(dx, dy) + 1 cells.
        let mut v = Vec::new();
        line_cells([0, 0], [6, 3], &mut v);
        assert_eq!(v.len(), 7);
        for (i, w) in v.windows(2).enumerate() {
            // One cell per column, never stepping back or skipping a row.
            assert_eq!(w[1][0], w[0][0] + 1, "{v:?} at {i}");
            assert!((0..=1).contains(&(w[1][1] - w[0][1])), "{v:?} at {i}");
        }
        // Each cell is within half a cell of the true line y = x / 2.
        for c in &v {
            assert!((c[1] as f64 - c[0] as f64 / 2.0).abs() <= 0.5, "{v:?}");
        }
        line_cells([2, 5], [2, 1], &mut v);
        assert_eq!(v, [[2, 5], [2, 4], [2, 3], [2, 2], [2, 1]]);
    }

    #[test]
    fn rect_fill_and_outline() {
        let mut v = Vec::new();
        rect_cells([1, 1, 4, 4], false, &mut v);
        assert_eq!(v.len(), 9);
        rect_cells([1, 1, 4, 4], true, &mut v);
        assert_eq!(v.len(), 8);
        assert!(!v.contains(&[2, 2]));
        // Thin rects: the outline is the whole thing.
        rect_cells([0, 0, 5, 1], true, &mut v);
        assert_eq!(v.len(), 5);
        rect_cells([0, 0, 2, 2], true, &mut v);
        assert_eq!(v.len(), 4);
        assert_eq!(clip_rect([-2, 3, 10, 9], [8, 6]), [0, 3, 8, 6]);
    }

    #[test]
    fn stamp_stride_and_lattice() {
        // 2x1 tiles of 16 px on an 8 px layer: each tile covers 2x2 cells,
        // the stamp 4x2.
        let s = Stamp::new([3, 4, 2, 1], [16, 16], 8);
        assert_eq!(s.stride, [2, 2]);
        assert_eq!(s.footprint(), [4, 2]);
        let mut t = Vec::new();
        s.tiles([10, 10], &mut t);
        assert_eq!(t, [([10, 10], [3, 4]), ([12, 10], [4, 4])]);
        // A stroke from (10, 10) repeats every 4 cells in x, 2 in y,
        // including to the left of and above the start.
        assert_eq!(s.anchor([10, 10], [13, 11]), [10, 10]);
        assert_eq!(s.anchor([10, 10], [14, 10]), [14, 10]);
        assert_eq!(s.anchor([10, 10], [9, 9]), [6, 8]);
        // Pattern fill: tiles land on the stride, cycling through the rect.
        assert_eq!(s.tile_at([0, 0], [0, 0]), Some([3, 4]));
        assert_eq!(s.tile_at([0, 0], [1, 0]), None);
        assert_eq!(s.tile_at([0, 0], [2, 0]), Some([4, 4]));
        assert_eq!(s.tile_at([0, 0], [4, 2]), Some([3, 4]));
        assert_eq!(s.tile_at([0, 0], [-2, 0]), Some([4, 4]));
        // Same-size tiles: stride 1.
        let s = Stamp::new([0, 0, 3, 2], [16, 16], 16);
        assert_eq!((s.stride, s.footprint()), ([1, 1], [3, 2]));
    }

    #[test]
    fn offsets_fold_into_the_cell() {
        // Reaching the grid on an axis moves a whole cell, either way.
        assert_eq!(fold_offset([3, 3], [15, -15], 16), ([3, 3], [15, -15]));
        assert_eq!(fold_offset([3, 3], [16, -16], 16), ([4, 2], [0, 0]));
        assert_eq!(fold_offset([3, 3], [35, -33], 16), ([5, 1], [3, -1]));
        // A nudge by one pixel past the edge.
        assert_eq!(nudged([2, 2], [7, 0], 8, [1, 0], true), ([3, 2], [0, 0]));
        assert_eq!(nudged([2, 2], [-7, 0], 8, [-1, 0], true), ([1, 2], [0, 0]));
        assert_eq!(nudged([2, 2], [5, 1], 8, [0, -1], false), ([2, 1], [5, 1]));
        // Drags snap to whole cells (nearest), or by pixel with Ctrl.
        assert_eq!(
            dragged_position([1, 1], [3, 0], 16, [23.0, -9.0], false),
            ([2, 0], [3, 0])
        );
        assert_eq!(
            dragged_position([1, 1], [3, 0], 16, [23.0, -9.0], true),
            ([2, 1], [10, -9])
        );
    }

    #[test]
    fn level_drag_geometry() {
        assert_eq!(snap_move([100, 50], [47.0, -15.0], 32), [132, 50]);
        assert_eq!(snap_move([0, 0], [-17.0, 15.9], 32), [-32, 0]);
        // Right edge out by two cells: left stays.
        assert_eq!(
            resize_level([0, 0], [10, 5], [1, 0], [64.0, 0.0], 32),
            ([0, 0], [12, 5])
        );
        // Top-left corner in by one cell each way: bottom-right stays.
        assert_eq!(
            resize_level([0, 0], [10, 5], [-1, -1], [32.0, 32.0], 32),
            ([32, 32], [9, 4])
        );
        // Never below one cell.
        assert_eq!(
            resize_level([0, 0], [2, 2], [1, 1], [-500.0, -500.0], 32),
            ([0, 0], [1, 1])
        );
        assert_eq!(
            resize_level([0, 0], [2, 2], [-1, 0], [500.0, 0.0], 32),
            ([32, 0], [1, 2])
        );
    }
}
