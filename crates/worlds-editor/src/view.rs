//! Per-session view state the panels and the viewport share: active level
//! and layer, per-layer visibility/lock/opacity, the tool and the palette,
//! the selection and a pending canvas pick. Partly saved to
//! `.worlds/ui.json` (not the selection).

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use worlds_core::{Id, Slug};

use crate::doc::{Document, LevelIx};
use crate::tools::{EntityPalette, PaletteSelection, Tool};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LayerView {
    pub visible: bool,
    pub locked: bool,
    /// View opacity, multiplied with the layer definition's opacity.
    pub opacity: f32,
}

impl Default for LayerView {
    fn default() -> Self {
        LayerView {
            visible: true,
            locked: false,
            opacity: 1.0,
        }
    }
}

/// A short message for the status bar ("Layer Collision is locked").
#[derive(Debug, Clone)]
pub struct StatusMsg {
    pub text: String,
    pub error: bool,
    pub until: Instant,
}

#[derive(Debug, Clone)]
pub struct ViewState {
    pub active_level: Option<LevelIx>,
    /// Index into `project.layers`.
    pub active_layer: Option<usize>,
    /// By index into `project.layers`.
    pub layers: Vec<LayerView>,
    /// The layer slugs `layers` was last synced to (same order), so a
    /// definition change can carry the settings over by slug.
    pub layer_slugs: Vec<Slug>,
    pub tool: Tool,
    /// The Paint group's mode: the last of [`Tool::PAINT_MODES`] that was
    /// the tool (Brush at first). The toolbar's Paint button and P go back
    /// to it. Always a Paint mode; change the tool with [`Self::set_tool`].
    pub paint_mode: Tool,
    pub palette: PaletteSelection,
    /// The entity palette: brush, favorites, recent, collapsed parts.
    pub entities: EntityPalette,
    pub show_grid: bool,
    /// What the Select tool, the palette's "In this level" counts, the
    /// Problems pane, the Notes pane and `view.select` selected (see
    /// [`Selection`]).
    pub selection: Selection,
    /// A canvas pick waiting for its click (an inspector's Pick button):
    /// the next click on the map goes to it, Escape cancels.
    pub pick: Option<PickRequest>,
    /// What the last pick picked, until the editor that asked takes it.
    pub picked: Option<Picked>,
    pub status: Option<StatusMsg>,
    /// Annotation visibility and guide options (`ui.json` `annotations`).
    pub annotations: AnnotationsView,
    /// The Note tool's options strip: kind, scope, pin icon.
    pub note_tool: NoteToolOptions,
    /// A path or polygon being drawn with the Note tool, or a measurement
    /// with the Measure tool (finished measurements stay until Escape or
    /// another tool).
    pub drawing: Option<crate::annot::Drawing>,
    /// Screen px per world px, as of the map's last frame (1 before any
    /// frame, e.g. in a headless `--script`): what "within 8 screen px"
    /// and "4 screen px" mean for the command layer.
    pub zoom: f64,
    /// A newly added note whose title the Inspector should focus.
    pub focus_title: Option<Id>,
    /// The highlighted geometry handle of the one selected annotation (the
    /// last one pressed with Select): Delete removes that vertex.
    pub vertex: Option<(Id, crate::annot::Handle)>,
}

impl Default for ViewState {
    fn default() -> Self {
        ViewState {
            active_level: None,
            active_layer: None,
            layers: Vec::new(),
            layer_slugs: Vec::new(),
            tool: Tool::default(),
            paint_mode: Tool::Brush,
            palette: PaletteSelection::default(),
            entities: EntityPalette::default(),
            show_grid: false,
            selection: Selection::default(),
            pick: None,
            picked: None,
            status: None,
            annotations: AnnotationsView::default(),
            note_tool: NoteToolOptions::default(),
            drawing: None,
            zoom: 1.0,
            focus_title: None,
            vertex: None,
        }
    }
}

/// Which annotations the map draws, and the guide options. Saved in
/// `.worlds/ui.json` as `annotations` (editor state, not project data).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AnnotationsView {
    /// An annotation with any of these tags is hidden (`(untagged)` stands
    /// for annotations with no tag).
    pub hidden_tags: std::collections::BTreeSet<String>,
    /// Hide annotations whose status is done.
    pub hide_done: bool,
    /// Hidden kinds (`note`, `pin`, `path`, `region`, `guide`, `connector`).
    pub hidden_kinds: std::collections::BTreeSet<String>,
    /// Also draw grouped guides where they were designed (dashed).
    pub show_designed: bool,
    /// Dragged levels snap to guides (as built) within 8 screen px.
    pub snap_to_guides: bool,
}

impl Default for AnnotationsView {
    fn default() -> Self {
        AnnotationsView {
            hidden_tags: Default::default(),
            hide_done: false,
            hidden_kinds: Default::default(),
            show_designed: false,
            snap_to_guides: true,
        }
    }
}

/// The pseudo-tag of annotations without tags (per-tag visibility).
pub const UNTAGGED: &str = "(untagged)";

impl AnnotationsView {
    /// Whether the map draws `a` (an annotation is hidden if any of its
    /// tags is hidden).
    pub fn shows(&self, a: &worlds_core::Annotation) -> bool {
        if self.hidden_kinds.contains(a.shape.name()) {
            return false;
        }
        if self.hide_done && a.status == worlds_core::NoteStatus::Done {
            return false;
        }
        if self.hidden_tags.is_empty() {
            return true;
        }
        if a.tags.is_empty() {
            return !self.hidden_tags.contains(UNTAGGED);
        }
        !a.tags.iter().any(|t| self.hidden_tags.contains(t))
    }
}

/// What the Note tool adds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoteKind {
    #[default]
    Note,
    Pin,
    Path,
    Region,
    /// A line from a room or entity to another.
    Connector,
}

impl NoteKind {
    pub const ALL: [NoteKind; 5] = [
        NoteKind::Note,
        NoteKind::Pin,
        NoteKind::Path,
        NoteKind::Region,
        NoteKind::Connector,
    ];

    pub fn name(self) -> &'static str {
        match self {
            NoteKind::Note => "note",
            NoteKind::Pin => "pin",
            NoteKind::Path => "path",
            NoteKind::Region => "region",
            NoteKind::Connector => "connector",
        }
    }

    pub fn parse(s: &str) -> Option<NoteKind> {
        NoteKind::ALL.into_iter().find(|k| k.name() == s)
    }
}

/// The Note tool's options strip.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NoteToolOptions {
    pub kind: NoteKind,
    /// Force world scope (else: the level under the press, or the world
    /// outside every level).
    pub world: bool,
    /// The icon of new pins.
    pub icon: String,
}

impl Default for NoteToolOptions {
    fn default() -> Self {
        NoteToolOptions {
            kind: NoteKind::Note,
            world: false,
            icon: "pin".into(),
        }
    }
}

/// One selected thing. Code that walks a selection matches on the kind.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SelItem {
    Entity(Id),
    /// An annotation, world or level scope.
    Note(Id),
    /// A level, by its stable `level.json` id (LevelIx changes on create/delete).
    Level(Id),
    /// The whole stack of placements at one cell of a tiles layer.
    Tiles {
        level: Id,
        layer: Slug,
        cell: [u32; 2],
    },
}

/// The selection: an ordered list of items, the first one primary (the
/// inspector shows it alone, or what several have in common). Items are
/// pruned when an edit, undo, redo or reload removes them
/// ([`Selection::prune`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Selection {
    pub items: Vec<SelItem>,
}

impl Selection {
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }

    /// The selected entities, in order.
    pub fn entities(&self) -> impl Iterator<Item = &Id> {
        self.items.iter().filter_map(|i| match i {
            SelItem::Entity(id) => Some(id),
            _ => None,
        })
    }

    /// The selected annotations, in order.
    pub fn notes(&self) -> impl Iterator<Item = &Id> {
        self.items.iter().filter_map(|i| match i {
            SelItem::Note(id) => Some(id),
            _ => None,
        })
    }

    /// The selected levels (`level.json` ids), in order.
    pub fn levels(&self) -> impl Iterator<Item = &Id> {
        self.items.iter().filter_map(|i| match i {
            SelItem::Level(id) => Some(id),
            _ => None,
        })
    }

    /// The selected tile stacks: (level id, layer, cell).
    pub fn tiles(&self) -> impl Iterator<Item = (&Id, &Slug, [u32; 2])> {
        self.items.iter().filter_map(|i| match i {
            SelItem::Tiles { level, layer, cell } => Some((level, layer, *cell)),
            _ => None,
        })
    }

    /// The selected entity ids, cloned.
    pub fn entity_ids(&self) -> Vec<Id> {
        self.entities().cloned().collect()
    }

    /// The selected annotation ids, cloned.
    pub fn note_ids(&self) -> Vec<Id> {
        self.notes().cloned().collect()
    }

    /// The first selected entity.
    pub fn primary_entity(&self) -> Option<&Id> {
        self.entities().next()
    }

    pub fn contains(&self, item: &SelItem) -> bool {
        self.items.contains(item)
    }

    pub fn contains_entity(&self, id: &Id) -> bool {
        self.items
            .iter()
            .any(|i| matches!(i, SelItem::Entity(e) if e == id))
    }

    /// Select exactly these items (duplicates dropped, order kept).
    pub fn set(&mut self, items: impl IntoIterator<Item = SelItem>) {
        self.items.clear();
        for i in items {
            self.add(i);
        }
    }

    /// Add an item at the end (no-op if selected).
    pub fn add(&mut self, item: SelItem) {
        if !self.items.contains(&item) {
            self.items.push(item);
        }
    }

    /// Add the item if it isn't selected, else take it out (Ctrl+click).
    pub fn toggle(&mut self, item: SelItem) {
        if let Some(i) = self.items.iter().position(|x| *x == item) {
            self.items.remove(i);
        } else {
            self.items.push(item);
        }
    }

    /// Select exactly these entities (duplicates dropped, order kept).
    pub fn set_entities(&mut self, ids: impl IntoIterator<Item = Id>) {
        self.set(ids.into_iter().map(SelItem::Entity));
    }

    /// Add an entity at the end (no-op if selected).
    pub fn add_entity(&mut self, id: Id) {
        self.add(SelItem::Entity(id));
    }

    /// Add the entity if it isn't selected, else take it out (Ctrl+click).
    pub fn toggle_entity(&mut self, id: Id) {
        self.toggle(SelItem::Entity(id));
    }

    /// Drop items `project` no longer has: entities and annotations by id
    /// (world and level notes), levels by id, tile stacks that are empty
    /// now or whose level or layer is gone. Returns whether anything was
    /// dropped.
    pub fn prune(&mut self, project: &worlds_core::Project) -> bool {
        if self.items.is_empty() {
            return false;
        }
        use std::collections::HashSet;
        let mut entities: HashSet<&Id> = HashSet::new();
        let mut notes: HashSet<&Id> = HashSet::new();
        let mut levels: HashSet<&Id> = HashSet::new();
        for w in &project.worlds {
            notes.extend(w.notes.iter().map(|n| &n.id));
            for l in &w.levels {
                levels.insert(&l.id);
                notes.extend(l.notes.iter().map(|n| &n.id));
                entities.extend(l.all_entities().map(|e| &e.id));
            }
        }
        let tile_alive = |level: &Id, layer: &Slug, cell: [u32; 2]| {
            project
                .levels()
                .find(|l| l.id == *level)
                .and_then(|l| l.tiles_layer(layer.as_str()))
                .is_some_and(|t| t.tiles.iter().any(|p| p.cell == cell))
        };
        let before = self.items.len();
        self.items.retain(|i| match i {
            SelItem::Entity(id) => entities.contains(id),
            SelItem::Note(id) => notes.contains(id),
            SelItem::Level(id) => levels.contains(id),
            SelItem::Tiles { level, layer, cell } => tile_alive(level, layer, *cell),
        });
        self.items.len() != before
    }
}

/// What a canvas pick accepts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PickAccept {
    /// A click on an entity, in any level (the world view too).
    pub entity: bool,
    /// A click on a cell of a level.
    pub cell: bool,
    /// A click on a level (anywhere in it, when nothing it takes first is
    /// there).
    pub level: bool,
    /// A click on an annotation (note card or dot, pin, a path, guide or
    /// region outline).
    pub note: bool,
}

/// A canvas pick an editor armed (a field's Pick button): the next left
/// click on the map is routed to it instead of the active tool, Escape
/// cancels it. The result goes to [`ViewState::picked`] with the same
/// `target`, for the editor that asked to take.
#[derive(Debug, Clone, PartialEq)]
pub struct PickRequest {
    pub accept: PickAccept,
    /// Who asked: the editor's key (an inspector field, an array element).
    pub target: String,
    /// What the status bar says while waiting ("Pick the target of exit_to").
    pub prompt: String,
}

/// The answer to a [`PickRequest`].
#[derive(Debug, Clone, PartialEq)]
pub struct Picked {
    pub target: String,
    pub what: PickedThing,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PickedThing {
    /// An entity, and the level holding it.
    Entity { id: Id, level: LevelIx },
    /// A point in a level: the level and the world pixel clicked (the
    /// editor turns it into a cell of the grid it needs).
    Cell { level: LevelIx, world_px: [f64; 2] },
    /// A level.
    Level { level: LevelIx },
    /// An annotation.
    Note { id: Id },
}

impl ViewState {
    /// Arm a canvas pick (replacing one in progress).
    pub fn start_pick(&mut self, accept: PickAccept, target: String, prompt: String) {
        self.picked = None;
        self.set_status(format!("{prompt} (Esc cancels)"), false);
        self.pick = Some(PickRequest {
            accept,
            target,
            prompt,
        });
    }

    /// Take the pick result meant for `target`, if it arrived.
    pub fn take_picked(&mut self, target: &str) -> Option<PickedThing> {
        if self.picked.as_ref().is_some_and(|p| p.target == target) {
            return self.picked.take().map(|p| p.what);
        }
        None
    }
}

impl ViewState {
    pub fn new() -> Self {
        ViewState {
            show_grid: true,
            ..Default::default()
        }
    }

    /// Make `t` the tool. A Paint mode becomes the remembered
    /// [`Self::paint_mode`]; a drawing or a measurement ends with its tool.
    /// (The caller cancels a gesture in progress first.)
    pub fn set_tool(&mut self, t: Tool) {
        if t.is_paint_mode() {
            self.paint_mode = t;
        }
        if self.tool != t {
            self.drawing = None;
        }
        self.tool = t;
    }

    /// The tool the Paint button (and P) selects: the last Paint mode.
    pub fn paint_tool(&self) -> Tool {
        if self.paint_mode.is_paint_mode() {
            self.paint_mode
        } else {
            Tool::Brush
        }
    }

    /// Make the per-layer table match the document, keeping settings of
    /// layers that still exist (matched by slug through `old`).
    pub fn sync_layers(&mut self, doc: &Document, old: Option<(&Document, Option<usize>)>) {
        let layers = &doc.project.layers;
        let mut fresh = vec![LayerView::default(); layers.len()];
        let mut active = None;
        if let Some((old_doc, old_active)) = old {
            for (i, l) in layers.iter().enumerate() {
                if let Some(j) = old_doc.project.layers.iter().position(|o| o.slug == l.slug) {
                    if let Some(v) = self.layers.get(j) {
                        fresh[i] = *v;
                    }
                    if old_active == Some(j) {
                        active = Some(i);
                    }
                }
            }
        }
        self.layers = fresh;
        self.layer_slugs = layers.iter().map(|l| l.slug.clone()).collect();
        self.active_layer = active.or_else(|| default_active_layer(doc));
    }

    /// After definition commands changed the layer list: make the table
    /// match it again, keeping each layer's settings (and the active layer)
    /// by slug, following `renamed` layers (`(from, to)`, in order).
    pub fn resync_layers(&mut self, doc: &Document, renamed: &[(Slug, Slug)]) {
        let layers = &doc.project.layers;
        let old_active = self
            .active_layer
            .and_then(|i| self.layer_slugs.get(i).cloned());
        let mut fresh = vec![LayerView::default(); layers.len()];
        let mut active = None;
        for (i, l) in layers.iter().enumerate() {
            let was = renamed
                .iter()
                .rev()
                .fold(
                    l.slug.clone(),
                    |cur, (from, to)| {
                        if cur == *to { from.clone() } else { cur }
                    },
                );
            if let Some(v) = self
                .layer_slugs
                .iter()
                .position(|s| *s == was)
                .and_then(|j| self.layers.get(j))
            {
                fresh[i] = *v;
            }
            if old_active.as_ref() == Some(&was) {
                active = Some(i);
            }
        }
        self.layers = fresh;
        self.layer_slugs = layers.iter().map(|l| l.slug.clone()).collect();
        self.active_layer = active.or_else(|| default_active_layer(doc));
    }

    /// Show a message in the status bar for a few seconds.
    pub fn set_status(&mut self, text: impl Into<String>, error: bool) {
        let text = text.into();
        if error {
            tracing::warn!("{text}");
        } else {
            tracing::debug!("{text}");
        }
        self.status = Some(StatusMsg {
            text,
            error,
            until: Instant::now() + Duration::from_secs(if error { 5 } else { 3 }),
        });
    }

    pub fn layer_locked(&self, i: usize) -> bool {
        self.layers.get(i).is_some_and(|l| l.locked)
    }

    pub fn layer_visible(&self, i: usize) -> bool {
        self.layers.get(i).is_none_or(|l| l.visible)
    }

    /// Effective opacity of project layer `i`.
    pub fn layer_opacity(&self, doc: &Document, i: usize) -> f32 {
        let def = doc.project.layers.get(i).map_or(1.0, |d| d.opacity as f32);
        let v = self.layers.get(i).copied().unwrap_or_default();
        if v.visible { def * v.opacity } else { 0.0 }
    }
}

/// The topmost int_grid layer, else the topmost tiles layer, else the top.
pub fn default_active_layer(doc: &Document) -> Option<usize> {
    let layers = &doc.project.layers;
    let top = |f: &dyn Fn(&worlds_core::LayerDef) -> bool| layers.iter().rposition(f);
    top(&|l| l.is_int_grid())
        .or_else(|| top(&|l| l.is_tiles()))
        .or_else(|| layers.len().checked_sub(1))
}

/// The first entities layer (the entity tool's default layer).
pub fn first_entities_layer(doc: &Document) -> Option<usize> {
    doc.project.layers.iter().position(|l| l.is_entities())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Paint group remembers its mode across other tools: Select and
    /// back to Paint (its button, P) restores the last mode, and a fresh
    /// view starts on Brush.
    #[test]
    fn paint_mode_is_remembered_across_tool_switches() {
        let mut v = ViewState::new();
        assert_eq!(v.paint_tool(), Tool::Brush);
        for mode in [Tool::Line, Tool::Rect, Tool::Fill, Tool::Brush] {
            v.set_tool(mode);
            assert_eq!(v.tool, mode);
            for other in [Tool::Select, Tool::Eraser, Tool::Entity, Tool::Hand] {
                v.set_tool(other);
                assert_eq!(v.tool, other);
                assert_eq!(v.paint_tool(), mode, "after {other:?}");
            }
            v.set_tool(v.paint_tool());
            assert_eq!(v.tool, mode);
        }
        // The same mode again changes nothing, a drawing included.
        v.set_tool(Tool::Fill);
        v.drawing = Some(crate::annot::Drawing {
            measure: true,
            kind: NoteKind::Path,
            level: None,
            points: vec![[0, 0]],
            done: true,
            connector: None,
        });
        v.set_tool(Tool::Fill);
        assert!(v.drawing.is_some());
        v.set_tool(Tool::Line);
        assert!(v.drawing.is_none());
        // Never anything but a Paint mode.
        v.paint_mode = Tool::Select;
        assert_eq!(v.paint_tool(), Tool::Brush);
    }
}
