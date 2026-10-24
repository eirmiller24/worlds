//! Per-session view state the panels and the viewport share: active level
//! and layer, per-layer visibility/lock/opacity, the tool and the palette,
//! the selection and a pending canvas pick. Partly saved to
//! `.worlds/ui.json` (not the selection).
//!
//! **Map tabs.** The editor has any number of map tabs ([`ViewportTab`]).
//! Per tab: the camera (and what the viewport keeps for it,
//! [`crate::viewport::TabCam`]), the world shown and the active level.
//! Everything else here is global: tool, brush, active layer, layer and
//! annotation visibility, selection. The *live* tab's per-tab state sits
//! in the usual places ([`ViewState::active_level`],
//! [`ViewState::active_world`], [`ViewState::zoom`] and the app's
//! [`crate::viewport::Viewport`]); the other tabs are parked in
//! [`ViewState::tabs`]. Outside the dock's drawing the live tab is the
//! focused one, so code that reads the active level reads the focused
//! tab's; drawing another tab swaps it in for its draw and back after
//! ([`swap_live`]).

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

/// A map tab's id: stable for the tab's life, saved in `ui.json` and
/// what the API's `viewport` argument names. The first tab is 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ViewportId(pub u32);

impl ViewportId {
    pub const FIRST: ViewportId = ViewportId(1);
}

impl std::fmt::Display for ViewportId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// One map tab. While the tab is live its fields here are stale (its
/// state is in the live slots, see the module docs); parked, they hold it.
#[derive(Debug, Clone)]
pub struct ViewportTab {
    pub id: ViewportId,
    pub active_level: Option<LevelIx>,
    pub active_world: Option<Slug>,
    pub zoom: f64,
    pub cam: crate::viewport::TabCam,
}

impl ViewportTab {
    pub fn new(id: ViewportId) -> Self {
        ViewportTab {
            id,
            active_level: None,
            active_world: None,
            zoom: 1.0,
            cam: Default::default(),
        }
    }
}

/// A saved view (`ui.json` `bookmarks`): a world and where the camera
/// looked, as its center and zoom so it frames the same spot in a tab of
/// any size.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Bookmark {
    pub name: String,
    pub world: String,
    pub camera: BookmarkCamera,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BookmarkCamera {
    /// The world pixel at the middle of the tab.
    pub center: [f64; 2],
    /// Physical screen px per world px.
    pub zoom: f64,
}

#[derive(Debug, Clone)]
pub struct ViewState {
    pub active_level: Option<LevelIx>,
    /// The world the map shows (`ui.json` `active_world`), by slug; see
    /// [`Self::world_ix`]. The active level's world wins over it.
    pub active_world: Option<Slug>,
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
    /// A paste waiting for its click: the fragment follows the pointer as
    /// a ghost; Escape cancels ([`crate::clipboard::PasteGhost`]).
    pub paste: Option<crate::clipboard::PasteGhost>,
    /// The last Select marquee in a level while an int_grid layer was
    /// active: the cells Copy takes from that layer.
    pub grid_marquee: Option<crate::clipboard::GridMarquee>,
    /// Reference images: the pseudo-layer, the selected one, a drag.
    pub references: crate::viewport::references::RefsView,
    /// Every map tab, in the order opened (see the module docs). Never
    /// empty.
    pub tabs: Vec<ViewportTab>,
    /// The focused map tab: keys, single-key tools, the toolbar's world
    /// switcher, the API's default `viewport`.
    pub focused: ViewportId,
    /// The tab whose state is in the live slots (the focused one, except
    /// while another tab draws).
    pub live: ViewportId,
    /// Two tabs whose cameras move together: same zoom, the same offset
    /// between their centers ("Link cameras").
    pub link: Option<[ViewportId; 2]>,
    /// Saved views, in order (Ctrl+Shift+1..9 jump to the first nine).
    pub bookmarks: Vec<Bookmark>,
}

impl Default for ViewState {
    fn default() -> Self {
        ViewState {
            active_level: None,
            active_world: None,
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
            paste: None,
            grid_marquee: None,
            references: Default::default(),
            tabs: vec![ViewportTab::new(ViewportId::FIRST)],
            focused: ViewportId::FIRST,
            live: ViewportId::FIRST,
            link: None,
            bookmarks: Vec::new(),
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

    /// The world the map shows, as an index into `project.worlds`: the
    /// active level's world, else [`Self::active_world`]'s, else the first.
    /// Only that world's levels and world-scope annotations are drawn and
    /// hit-tested; everything project-wide (validation, palette counts,
    /// the Notes and Problems panes) still covers every world.
    pub fn world_ix(&self, doc: &Document) -> usize {
        world_ix_of(doc, self.active_level, self.active_world.as_ref())
    }

    /// Show world `w`: it becomes [`Self::active_world`], and an active
    /// level in another world stops being active.
    pub fn set_world(&mut self, doc: &Document, w: usize) {
        let Some(world) = doc.project.worlds.get(w) else {
            return;
        };
        if self.world_ix(doc) != w {
            // What sits on the old world's map doesn't follow: an armed
            // paste ghost and the grid marquee are placed in its levels.
            self.paste = None;
            self.grid_marquee = None;
        }
        self.active_world = Some(world.slug.clone());
        if self
            .active_level
            .and_then(|ix| doc.levels.get(ix))
            .is_some_and(|r| r.world != w)
        {
            self.active_level = None;
        }
    }

    /// The world map tab `id` shows (index into `project.worlds`), as
    /// [`Self::world_ix`] for the live tab.
    pub fn tab_world_ix(&self, doc: &Document, id: ViewportId) -> usize {
        if id == self.live {
            return self.world_ix(doc);
        }
        match self.tabs.iter().find(|t| t.id == id) {
            Some(t) => world_ix_of(doc, t.active_level, t.active_world.as_ref()),
            None => self.world_ix(doc),
        }
    }

    /// Tab `id`'s active level ([`Self::active_level`] for the live tab).
    pub fn tab_active_level(&self, id: ViewportId) -> Option<LevelIx> {
        if id == self.live {
            return self.active_level;
        }
        self.tabs
            .iter()
            .find(|t| t.id == id)
            .and_then(|t| t.active_level)
    }

    pub fn has_tab(&self, id: ViewportId) -> bool {
        self.tabs.iter().any(|t| t.id == id)
    }

    /// An id no tab has: one more than the largest.
    pub fn fresh_tab_id(&self) -> ViewportId {
        ViewportId(self.tabs.iter().map(|t| t.id.0).max().unwrap_or(0) + 1)
    }

    /// Level indices changed (`map[old]` is the new index, `None`: gone):
    /// follow them in the live tab and every parked one.
    pub fn remap_levels(&mut self, map: &[Option<usize>]) {
        self.active_level = self
            .active_level
            .and_then(|i| map.get(i).copied().flatten());
        let live = self.live;
        for t in self.tabs.iter_mut().filter(|t| t.id != live) {
            t.active_level = t.active_level.and_then(|i| map.get(i).copied().flatten());
            t.cam.remap_levels(map);
        }
    }

    /// World `from` is now `to`: every tab showing it still does.
    pub fn rename_world(&mut self, from: &Slug, to: &Slug) {
        if self.active_world.as_ref() == Some(from) {
            self.active_world = Some(to.clone());
        }
        let live = self.live;
        for t in self.tabs.iter_mut().filter(|t| t.id != live) {
            if t.active_world.as_ref() == Some(from) {
                t.active_world = Some(to.clone());
            }
        }
    }

    /// Make [`Self::active_world`] name the world shown (the active
    /// level's, after something made a level in another world active).
    pub fn sync_world(&mut self, doc: &Document) {
        let w = self.world_ix(doc);
        if let Some(world) = doc.project.worlds.get(w)
            && self.active_world.as_ref() != Some(&world.slug)
        {
            self.active_world = Some(world.slug.clone());
        }
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

/// The world a tab with this active level and world shows (see
/// [`ViewState::world_ix`]).
fn world_ix_of(doc: &Document, level: Option<LevelIx>, world: Option<&Slug>) -> usize {
    if let Some(r) = level
        .and_then(|ix| doc.levels.get(ix))
        .filter(|r| r.world < doc.project.worlds.len())
    {
        return r.world;
    }
    world
        .and_then(|s| doc.project.worlds.iter().position(|w| w.slug == *s))
        .unwrap_or(0)
}

impl ViewState {
    /// Make tab `id` focused (and live). False if there is no such tab.
    pub fn focus_tab(&mut self, vp: &mut crate::viewport::Viewport, id: ViewportId) -> bool {
        if !self.has_tab(id) {
            return false;
        }
        swap_live(self, vp, id);
        self.focused = id;
        true
    }

    /// Tab `id`'s camera and size in physical px (the live tab's are the
    /// viewport's).
    pub fn tab_camera(
        &self,
        vp: &crate::viewport::Viewport,
        id: ViewportId,
    ) -> Option<(crate::viewport::camera::Camera, [f64; 2])> {
        if id == self.live {
            return Some((vp.camera, vp.size_px));
        }
        let t = self.tabs.iter().find(|t| t.id == id)?;
        Some((t.cam.camera, t.cam.size_px))
    }

    /// Set tab `id`'s camera.
    pub fn set_tab_camera(
        &mut self,
        vp: &mut crate::viewport::Viewport,
        id: ViewportId,
        cam: crate::viewport::camera::Camera,
    ) {
        if id == self.live {
            vp.camera = cam;
        } else if let Some(t) = self.tabs.iter_mut().find(|t| t.id == id) {
            t.cam.camera = cam;
        }
    }

    /// Add a map tab (parked; the app docks it): a copy of tab `copy_of`'s
    /// world, active level and camera, else world `world` (a slug; `None`:
    /// the focused tab's world), fitted when first drawn.
    pub fn new_tab(
        &mut self,
        doc: &Document,
        vp: &crate::viewport::Viewport,
        world: Option<Slug>,
        copy_of: Option<ViewportId>,
    ) -> ViewportId {
        let id = self.fresh_tab_id();
        let mut tab = ViewportTab::new(id);
        match copy_of.filter(|&c| self.has_tab(c)) {
            Some(src) => {
                let (cam, size) = self.tab_camera(vp, src).expect("has the tab");
                tab.active_level = self.tab_active_level(src);
                tab.active_world = doc
                    .project
                    .worlds
                    .get(self.tab_world_ix(doc, src))
                    .map(|w| w.slug.clone());
                tab.zoom = cam.zoom;
                tab.cam = crate::viewport::TabCam::new(cam, None);
                tab.cam.size_px = size;
            }
            None => {
                tab.active_world = world.or_else(|| {
                    doc.project
                        .worlds
                        .get(self.tab_world_ix(doc, self.focused))
                        .map(|w| w.slug.clone())
                });
                let (cam, size) = self.tab_camera(vp, self.focused).unwrap_or_default();
                tab.cam = crate::viewport::TabCam::new(cam, Some(crate::viewport::Fit::World));
                tab.cam.size_px = size;
            }
        }
        self.tabs.push(tab);
        id
    }

    /// Close tab `id`. The last tab can't be closed; closing the focused
    /// one focuses the first other tab.
    pub fn close_tab(
        &mut self,
        vp: &mut crate::viewport::Viewport,
        id: ViewportId,
    ) -> Result<(), String> {
        if !self.has_tab(id) {
            return Err(format!("There is no map tab {id}"));
        }
        if self.tabs.len() <= 1 {
            return Err("The last map tab stays open".into());
        }
        if self.focused == id || self.live == id {
            let other = self.tabs.iter().find(|t| t.id != id).expect("two tabs").id;
            self.focus_tab(vp, other);
        }
        self.tabs.retain(|t| t.id != id);
        if self.link.is_some_and(|l| l.contains(&id)) {
            self.link = None;
        }
        Ok(())
    }

    /// Save the focused tab's view as bookmark `name` (an old one with that
    /// name is replaced in place). Returns its index.
    pub fn save_bookmark(
        &mut self,
        doc: &Document,
        vp: &crate::viewport::Viewport,
        name: &str,
    ) -> Result<usize, String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("A bookmark needs a name".into());
        }
        let (cam, size) = self
            .tab_camera(vp, self.focused)
            .ok_or("no focused map tab")?;
        let world = doc
            .project
            .worlds
            .get(self.tab_world_ix(doc, self.focused))
            .map(|w| w.slug.to_string())
            .unwrap_or_default();
        let b = Bookmark {
            name: name.to_string(),
            world,
            camera: BookmarkCamera {
                center: cam.center(size),
                zoom: cam.zoom,
            },
        };
        Ok(match self.bookmarks.iter().position(|x| x.name == b.name) {
            Some(i) => {
                self.bookmarks[i] = b;
                i
            }
            None => {
                self.bookmarks.push(b);
                self.bookmarks.len() - 1
            }
        })
    }

    /// The bookmark named `name`.
    pub fn bookmark_ix(&self, name: &str) -> Option<usize> {
        self.bookmarks.iter().position(|b| b.name == name)
    }
}

/// Make map tab `id` the live one: park the live tab's per-tab state (the
/// view's active level, world and zoom, the viewport's [`TabCam`]) in its
/// entry and take `id`'s. Nothing if `id` is live or unknown. Swapping
/// back restores everything exactly.
///
/// [`TabCam`]: crate::viewport::TabCam
pub fn swap_live(view: &mut ViewState, vp: &mut crate::viewport::Viewport, id: ViewportId) {
    if id == view.live {
        return;
    }
    let (Some(a), Some(b)) = (
        view.tabs.iter().position(|t| t.id == view.live),
        view.tabs.iter().position(|t| t.id == id),
    ) else {
        return;
    };
    use std::mem::swap;
    // Park the live tab in its entry (a), then take b's.
    {
        let t = &mut view.tabs[a];
        swap(&mut view.active_level, &mut t.active_level);
        swap(&mut view.active_world, &mut t.active_world);
        swap(&mut view.zoom, &mut t.zoom);
        vp.swap_tab(&mut t.cam);
    }
    {
        let t = &mut view.tabs[b];
        swap(&mut view.active_level, &mut t.active_level);
        swap(&mut view.active_world, &mut t.active_world);
        swap(&mut view.zoom, &mut t.zoom);
        vp.swap_tab(&mut t.cam);
    }
    view.live = id;
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

    /// Swapping a parked tab in and back restores both exactly; per-tab
    /// state stays per tab while tool, layer and selection are shared;
    /// level indices follow a reindex in parked tabs; the last tab stays.
    #[test]
    fn map_tabs_swap_park_and_close() {
        use crate::viewport::camera::Camera;
        use crate::viewport::{Fit, Viewport};
        let mut v = ViewState::new();
        let mut vp = Viewport::new();
        let w = |s: &str| Some(Slug::new(s).unwrap());
        v.active_level = Some(0);
        v.active_world = w("main");
        vp.camera = Camera {
            origin: [1.0, 2.0],
            zoom: 2.0,
        };
        let mut t = ViewportTab::new(ViewportId(2));
        t.active_level = Some(3);
        t.active_world = w("dungeon");
        t.cam = crate::viewport::TabCam::new(
            Camera {
                origin: [9.0, 9.0],
                zoom: 4.0,
            },
            Some(Fit::Level(3)),
        );
        v.tabs.push(t);
        v.set_tool(Tool::Fill);
        swap_live(&mut v, &mut vp, ViewportId(2));
        assert_eq!(v.live, ViewportId(2));
        assert_eq!(v.active_level, Some(3));
        assert_eq!(v.active_world, w("dungeon"));
        assert_eq!(vp.camera.zoom, 4.0);
        assert_eq!(vp.pending_fit(), Some(Fit::Level(3)));
        assert_eq!(v.tool, Tool::Fill, "the tool is global");
        assert_eq!(v.tab_active_level(ViewportId::FIRST), Some(0));
        swap_live(&mut v, &mut vp, ViewportId::FIRST);
        assert_eq!(v.active_level, Some(0));
        assert_eq!(v.active_world, w("main"));
        assert_eq!(vp.camera.origin, [1.0, 2.0]);
        assert_eq!(vp.pending_fit(), None);
        assert_eq!(v.tab_active_level(ViewportId(2)), Some(3));
        // A reindex: level 3 is now 1, level 0 is gone.
        v.remap_levels(&[None, None, None, Some(1)]);
        assert_eq!(v.active_level, None);
        assert_eq!(v.tab_active_level(ViewportId(2)), Some(1));
        assert_eq!(v.tabs[1].cam.pending_fit(), Some(Fit::Level(1)));
        v.rename_world(&Slug::new("dungeon").unwrap(), &Slug::new("caves").unwrap());
        assert_eq!(v.tabs[1].active_world, w("caves"));
        // Closing the focused tab focuses the other; the last one stays.
        assert_eq!(v.fresh_tab_id(), ViewportId(3));
        v.link = Some([ViewportId::FIRST, ViewportId(2)]);
        v.close_tab(&mut vp, ViewportId::FIRST).unwrap();
        assert_eq!((v.focused, v.live), (ViewportId(2), ViewportId(2)));
        assert_eq!(v.active_level, Some(1));
        assert_eq!(v.link, None);
        assert!(v.close_tab(&mut vp, ViewportId(2)).is_err());
        assert_eq!(v.tabs.len(), 1);
    }

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
