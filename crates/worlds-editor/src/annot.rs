//! Annotations in the editor: finding them, hit-testing them, the
//! Note and Measure tools, the selection edits that work across kinds
//! (move, nudge, duplicate, delete of entities, annotations, tile stacks
//! and levels), and the Select tool's geometry handles on one selected
//! annotation (drag a vertex, Alt+click a segment to add one, Delete the
//! highlighted one).
//!
//! Everything here is plain functions of the document and the view, shared
//! by the viewport (mouse), the app (keys, the Notes pane) and `--script`
//! (tests), so a script drives exactly what the mouse does. The plans
//! return commands; the caller applies them as one undo entry. Previews draw
//! from the same plan's commands, so a drag shows exactly what its release
//! applies.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use worlds_core::command::NoteScope;
use worlds_core::{
    Annotation, Command, Endpoint, Id, NoteStatus, Shape, Slug, TilePlacement, bbox_of,
};

use crate::doc::{Document, LevelIx};
use crate::tools::in_layer;
use crate::view::{NoteKind, SelItem, ViewState};
use crate::viewport::camera::WRect;

/// Below this many screen px per project cell, notes are dots, pins are
/// small and no labels or titles are drawn.
pub const CARD_CELL_PX: f64 = 12.0;
/// Largest note card, screen points.
pub const CARD_MAX: [f32; 2] = [180.0, 52.0];
/// Hit distance to a path, guide or region outline, screen px.
pub const LINE_HIT_PX: f64 = 4.0;
/// Levels snap to guides within this many screen px.
pub const SNAP_PX: f64 = 8.0;
/// A Note-tool press that moves more than this many screen px is a drag
/// (a region rect); less is a click.
pub const DRAG_PX: f64 = 4.0;

/// The built-in pin icons. Anything else draws as `pin`.
pub const PIN_ICONS: [&str; 10] = [
    "pin", "statue", "pool", "door", "star", "chest", "skull", "eye", "flag", "key",
];

/// Fixed guide color when neither the group nor the guide has one.
pub const GUIDE_CYAN: [u8; 3] = [0x4f, 0xc3, 0xf7];

/// Where an annotation lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum At {
    /// `world.json` of world `w` (index into `project.worlds`).
    World(usize),
    /// A level's `notes.json`.
    Level(LevelIx),
}

/// An annotation found in the document.
#[derive(Debug, Clone, Copy)]
pub struct NoteRef<'a> {
    pub at: At,
    pub note: &'a Annotation,
}

impl NoteRef<'_> {
    /// The level of a level-scope note.
    pub fn level(&self) -> Option<LevelIx> {
        match self.at {
            At::Level(ix) => Some(ix),
            At::World(_) => None,
        }
    }
}

/// The world index a level belongs to.
pub fn world_of(doc: &Document, ix: LevelIx) -> usize {
    doc.levels[ix].world
}

/// The world index an annotation is in.
pub fn note_world(doc: &Document, at: At) -> usize {
    match at {
        At::World(w) => w,
        At::Level(ix) => world_of(doc, ix),
    }
}

/// The annotations of the world the map shows ([`ViewState::world_ix`]):
/// what it draws and hit-tests, in [`all_notes`] order.
pub fn shown_notes<'a>(doc: &'a Document, view: &ViewState) -> Vec<NoteRef<'a>> {
    let w = view.world_ix(doc);
    let mut out = all_notes(doc);
    out.retain(|r| note_world(doc, r.at) == w);
    out
}

/// What to add to an annotation's own pixels to get world pixels, as
/// built: the level's position for level notes, a guide's group offset.
pub fn offset(doc: &Document, at: At, a: &Annotation) -> [i64; 2] {
    match at {
        At::Level(ix) => doc.level(ix).world_px,
        At::World(w) => group_offset(doc, w, a),
    }
}

/// A grouped guide's offset (`[0, 0]` for anything else).
pub fn group_offset(doc: &Document, w: usize, a: &Annotation) -> [i64; 2] {
    match &a.shape {
        Shape::Guide { group: Some(g), .. } => doc.project.worlds[w]
            .guide_group(g.as_str())
            .map_or([0, 0], |g| g.offset),
        _ => [0, 0],
    }
}

/// Every annotation: each world's notes, then each level's (index order).
pub fn all_notes(doc: &Document) -> Vec<NoteRef<'_>> {
    let mut out = Vec::new();
    for (w, world) in doc.project.worlds.iter().enumerate() {
        out.extend(world.notes.iter().map(|n| NoteRef {
            at: At::World(w),
            note: n,
        }));
    }
    for ix in 0..doc.levels.len() {
        out.extend(doc.level(ix).notes.iter().map(|n| NoteRef {
            at: At::Level(ix),
            note: n,
        }));
    }
    out
}

/// The annotation with this id.
pub fn find_note<'a>(doc: &'a Document, id: &Id) -> Option<NoteRef<'a>> {
    for (w, world) in doc.project.worlds.iter().enumerate() {
        if let Some(n) = world.notes.iter().find(|n| n.id == *id) {
            return Some(NoteRef {
                at: At::World(w),
                note: n,
            });
        }
    }
    (0..doc.levels.len()).find_map(|ix| {
        doc.level(ix)
            .notes
            .iter()
            .find(|n| n.id == *id)
            .map(|n| NoteRef {
                at: At::Level(ix),
                note: n,
            })
    })
}

/// The points of an annotation in world px, as built. A connector's are
/// its line ([`connector_line`]): the computed ends with the waypoints.
pub fn world_points(doc: &Document, r: NoteRef<'_>) -> Vec<[i64; 2]> {
    if matches!(r.note.shape, Shape::Connector { .. }) {
        return connector_line(doc, r);
    }
    let o = offset(doc, r.at, r.note);
    r.note
        .shape
        .points()
        .into_iter()
        .map(|p| [p[0] + o[0], p[1] + o[1]])
        .collect()
}

/// Where a connector end is, in world px (`None`: it names nothing).
pub fn end_anchor(doc: &Document, e: &Endpoint) -> Option<worlds_core::Anchor> {
    doc.project.endpoint_anchor(e).map(|a| a.0)
}

/// Where a connector end is, in world px, if it is in world `w` (index
/// into `project.worlds`): an end in another world has no place on this
/// world's map.
fn end_anchor_in(doc: &Document, e: &Endpoint, w: usize) -> Option<worlds_core::Anchor> {
    let (a, world, _) = doc.project.endpoint_anchor(e)?;
    (doc.project.worlds.get(w).map(|x| &x.slug) == Some(&world.slug)).then_some(a)
}

/// A connector's line in world px: its `from` end, the waypoints, its `to`
/// end ([`worlds_core::connector_polyline`]), so it follows the rooms and
/// entities it joins. An end that names nothing, or something in another
/// world than the connector's, is left out (validation reports both).
pub fn connector_line(doc: &Document, r: NoteRef<'_>) -> Vec<[i64; 2]> {
    let Shape::Connector {
        from,
        to,
        waypoints,
        ..
    } = &r.note.shape
    else {
        return Vec::new();
    };
    let o = offset(doc, r.at, r.note);
    let wp: Vec<[i64; 2]> = waypoints
        .iter()
        .map(|p| [p[0] + o[0], p[1] + o[1]])
        .collect();
    let w = note_world(doc, r.at);
    match (end_anchor_in(doc, from, w), end_anchor_in(doc, to, w)) {
        (Some(a), Some(b)) => worlds_core::connector_polyline(a, b, &wp),
        (a, b) => {
            let mut out = Vec::new();
            if let Some(a) = a {
                out.push(a.end_toward(wp.first().copied().unwrap_or(a.point)));
            }
            out.extend_from_slice(&wp);
            if let Some(b) = b {
                out.push(b.end_toward(wp.last().copied().unwrap_or(b.point)));
            }
            out
        }
    }
}

/// The bounding rect of an annotation in world px, as built.
pub fn world_bbox(doc: &Document, r: NoteRef<'_>) -> Option<WRect> {
    if matches!(r.note.shape, Shape::Connector { .. }) {
        let [x, y, w, h] = bbox_of(&connector_line(doc, r))?;
        return Some(WRect::from_pos_size(
            [x as f64, y as f64],
            [w as f64, h as f64],
        ));
    }
    let o = offset(doc, r.at, r.note);
    let [x, y, w, h] = r.note.bbox()?;
    Some(WRect::from_pos_size(
        [(x + o[0]) as f64, (y + o[1]) as f64],
        [w as f64, h as f64],
    ))
}

/// Whether a rect (possibly zero-sized, a point or a line) touches `r`.
pub fn touches(b: &WRect, r: &WRect) -> bool {
    b.min[0] <= r.max[0] && r.min[0] <= b.max[0] && b.min[1] <= r.max[1] && r.min[1] <= b.max[1]
}

/// The total length of a polyline, px.
pub fn path_length(points: &[[i64; 2]]) -> f64 {
    points
        .windows(2)
        .map(|w| {
            let d = [(w[1][0] - w[0][0]) as f64, (w[1][1] - w[0][1]) as f64];
            d[0].hypot(d[1])
        })
        .sum()
}

/// The segments of an annotation's outline in world px (paths and guides
/// as drawn, region outlines closed; notes and pins have none).
pub fn segments(doc: &Document, r: NoteRef<'_>) -> Vec<([f64; 2], [f64; 2])> {
    let closed = match &r.note.shape {
        Shape::Note { .. } | Shape::Pin { .. } => return Vec::new(),
        Shape::Path { .. } | Shape::Connector { .. } => false,
        Shape::Region { .. } => true,
        Shape::Guide { closed, .. } => *closed,
    };
    let pts: Vec<[f64; 2]> = world_points(doc, r)
        .into_iter()
        .map(|p| [p[0] as f64, p[1] as f64])
        .collect();
    let mut out: Vec<([f64; 2], [f64; 2])> = pts.windows(2).map(|w| (w[0], w[1])).collect();
    if closed && pts.len() > 2 {
        out.push((pts[pts.len() - 1], pts[0]));
    }
    out
}

/// Distance from `p` to the segment `a`-`b`.
pub fn seg_dist(p: [f64; 2], a: [f64; 2], b: [f64; 2]) -> f64 {
    let d = [b[0] - a[0], b[1] - a[1]];
    let l2 = d[0] * d[0] + d[1] * d[1];
    let t = if l2 == 0.0 {
        0.0
    } else {
        (((p[0] - a[0]) * d[0] + (p[1] - a[1]) * d[1]) / l2).clamp(0.0, 1.0)
    };
    let q = [a[0] + t * d[0], a[1] + t * d[1]];
    (p[0] - q[0]).hypot(p[1] - q[1])
}

/// The screen size (points) of a note's card.
pub fn card_size(a: &Annotation) -> [f32; 2] {
    let title = a.title.as_deref().unwrap_or("Note");
    let body = first_line(a.body.as_deref());
    let chars = title
        .chars()
        .count()
        .max(body.map_or(0, |b| b.chars().count()));
    let w = (chars as f32 * 6.2 + 14.0).clamp(64.0, CARD_MAX[0]);
    let mut h = 6.0 + 15.0;
    if body.is_some() {
        h += 13.0;
    }
    if !a.tags.is_empty() {
        h += 13.0;
    }
    [w, (h as f32).min(CARD_MAX[1])]
}

/// The first non-empty line of a body.
pub fn first_line(body: Option<&str>) -> Option<&str> {
    body.and_then(|b| b.lines().map(str::trim).find(|l| !l.is_empty()))
}

/// Whether the map shows cards, big pins and labels at this zoom.
pub fn card_zoom(doc: &Document, zoom: f64) -> bool {
    zoom * doc.project.grid_size as f64 >= CARD_CELL_PX
}

/// The pin icon size in screen px at this zoom.
pub fn pin_size(doc: &Document, zoom: f64) -> f64 {
    if card_zoom(doc, zoom) { 16.0 } else { 8.0 }
}

/// Whether the map shows this annotation (the view's per-tag, per-kind
/// and done filters; `snapshot`: no filter except that only open notes
/// are drawn, and every guide).
pub fn shown(view: &ViewState, a: &Annotation, snapshot: bool) -> bool {
    if snapshot {
        return matches!(a.shape, Shape::Guide { .. }) || a.status == NoteStatus::Open;
    }
    view.annotations.shows(a)
}

/// The note or pin (a point annotation) under world pixel `wp`: a card,
/// a dot or a pin's icon, topmost (drawn last) first. `zoom` is screen px
/// per world px.
pub fn point_note_at(doc: &Document, view: &ViewState, wp: [f64; 2], zoom: f64) -> Option<Id> {
    let card = card_zoom(doc, zoom);
    let pin_r = (pin_size(doc, zoom) / 2.0).max(5.0) / zoom;
    shown_notes(doc, view).into_iter().rev().find_map(|r| {
        if !shown(view, r.note, false) {
            return None;
        }
        let o = offset(doc, r.at, r.note);
        let (p, pin) = match &r.note.shape {
            Shape::Note { px } => (*px, false),
            Shape::Pin { px, .. } => (*px, true),
            _ => return None,
        };
        let c = [(p[0] + o[0]) as f64, (p[1] + o[1]) as f64];
        let hit = if pin {
            (wp[0] - c[0]).abs() <= pin_r && (wp[1] - c[1]).abs() <= pin_r
        } else if card {
            let s = card_size(r.note);
            wp[0] >= c[0] - 3.0 / zoom
                && wp[1] >= c[1] - 3.0 / zoom
                && wp[0] <= c[0] + s[0] as f64 / zoom
                && wp[1] <= c[1] + s[1] as f64 / zoom
        } else {
            (wp[0] - c[0]).hypot(wp[1] - c[1]) <= 6.0 / zoom
        };
        hit.then(|| r.note.id.clone())
    })
}

/// The connector, path, guide or region outline within [`LINE_HIT_PX`] of
/// `wp`, topmost first.
pub fn line_note_at(doc: &Document, view: &ViewState, wp: [f64; 2], zoom: f64) -> Option<Id> {
    let tol = LINE_HIT_PX / zoom;
    let notes = shown_notes(doc, view);
    // Drawn order: regions, guides, paths, connectors; so connectors are
    // hit first.
    for kind in ["connector", "path", "guide", "region"] {
        for r in notes.iter().rev() {
            if r.note.shape.name() != kind || !shown(view, r.note, false) {
                continue;
            }
            if let Some(b) = world_bbox(doc, *r)
                && !touches(&b.expand(tol), &WRect::new(wp, wp))
            {
                continue;
            }
            if segments(doc, *r)
                .iter()
                .any(|(a, b)| seg_dist(wp, *a, *b) <= tol)
            {
                return Some(r.note.id.clone());
            }
        }
    }
    None
}

/// Any annotation under `wp` (points first, then outlines).
pub fn note_at(doc: &Document, view: &ViewState, wp: [f64; 2], zoom: f64) -> Option<Id> {
    point_note_at(doc, view, wp, zoom).or_else(|| line_note_at(doc, view, wp, zoom))
}

/// The active layer, when it is a visible, unlocked tiles layer.
pub fn active_tiles_layer(doc: &Document, view: &ViewState) -> Option<usize> {
    view.active_layer.filter(|&l| {
        doc.project.layers.get(l).is_some_and(|d| d.is_tiles())
            && view.layer_visible(l)
            && !view.layer_locked(l)
    })
}

/// The tile stack of the active tiles layer at world pixel `wp` in level
/// `ix` (a stack placed at that cell), if any.
pub fn tile_stack_at(
    doc: &Document,
    view: &ViewState,
    ix: LevelIx,
    wp: [f64; 2],
) -> Option<SelItem> {
    let l = active_tiles_layer(doc, view)?;
    let def = &doc.project.layers[l];
    let g = doc.project.layer_grid(def).max(1) as f64;
    let level = doc.level(ix);
    let c = [
        ((wp[0] - level.world_px[0] as f64) / g).floor(),
        ((wp[1] - level.world_px[1] as f64) / g).floor(),
    ];
    if c[0] < 0.0 || c[1] < 0.0 {
        return None;
    }
    let cell = [c[0] as u32, c[1] as u32];
    level
        .tiles_layer(def.slug.as_str())?
        .tiles
        .iter()
        .any(|p| p.cell == cell)
        .then(|| SelItem::Tiles {
            level: level.id.clone(),
            layer: def.slug.clone(),
            cell,
        })
}

/// What a Select marquee over world rect `r` picks. In a level (`level`):
/// the entities on visible, unlocked layers it touches, the stacks of the
/// active tiles layer (visible, unlocked) whose cell it touches, and the
/// visible, unlocked annotations whose box it touches (that level's and
/// world ones). On the world (`None`): the levels it touches and the
/// visible, unlocked world annotations.
pub fn marquee_items(
    doc: &Document,
    view: &ViewState,
    level: Option<LevelIx>,
    r: &WRect,
) -> Vec<SelItem> {
    let mut out = Vec::new();
    match level {
        Some(ix) => {
            out.extend(
                crate::viewport::entities_in_rect(doc, view, ix, r)
                    .into_iter()
                    .map(SelItem::Entity),
            );
            if let Some(l) = active_tiles_layer(doc, view) {
                let def = &doc.project.layers[l];
                let g = doc.project.layer_grid(def).max(1) as f64;
                let lv = doc.level(ix);
                let o = [lv.world_px[0] as f64, lv.world_px[1] as f64];
                if let Some(t) = lv.tiles_layer(def.slug.as_str()) {
                    let mut seen = BTreeSet::new();
                    for p in &t.tiles {
                        let cr = WRect::from_pos_size(
                            [o[0] + p.cell[0] as f64 * g, o[1] + p.cell[1] as f64 * g],
                            [g, g],
                        );
                        if cr.intersects(r) && seen.insert(p.cell) {
                            out.push(SelItem::Tiles {
                                level: lv.id.clone(),
                                layer: def.slug.clone(),
                                cell: p.cell,
                            });
                        }
                    }
                }
            }
        }
        None => {
            let w = view.world_ix(doc);
            for ix in (0..doc.levels.len()).filter(|&ix| world_of(doc, ix) == w) {
                if crate::viewport::level_wrect(doc, ix).intersects(r) {
                    out.push(SelItem::Level(doc.ids[ix].clone()));
                }
            }
        }
    }
    for n in shown_notes(doc, view) {
        let in_scope = match n.at {
            At::World(_) => true,
            At::Level(lx) => level == Some(lx),
        };
        if !in_scope || n.note.locked || !shown(view, n.note, false) {
            continue;
        }
        if world_bbox(doc, n).is_some_and(|b| touches(&b, r)) {
            out.push(SelItem::Note(n.note.id.clone()));
        }
    }
    out
}

// ---------------------------------------------------------------- tools

/// A path or polygon being drawn (Note tool), or a measurement (Measure
/// tool). Points are world px.
#[derive(Debug, Clone, PartialEq)]
pub struct Drawing {
    /// A measurement: never a command.
    pub measure: bool,
    pub kind: NoteKind,
    /// The level a level-scope annotation goes in (`None`: the world).
    pub level: Option<LevelIx>,
    pub points: Vec<[i64; 2]>,
    /// A measurement that was finished: it stays drawn until Escape or
    /// another tool.
    pub done: bool,
    /// A connector being drawn: its ends (`points` are its waypoints).
    pub connector: Option<ConnectorDraft>,
}

/// The ends of a connector being drawn with the Note tool: the first click
/// set `from`; a click on another room or an entity sets `to` (Enter
/// finishes).
#[derive(Debug, Clone, PartialEq)]
pub struct ConnectorDraft {
    pub from: Endpoint,
    pub to: Option<Endpoint>,
}

/// A Note-tool point: half the project grid, or with Ctrl the pixel.
pub fn snap_point(doc: &Document, wp: [f64; 2], ctrl: bool) -> [i64; 2] {
    if ctrl {
        return [wp[0].round() as i64, wp[1].round() as i64];
    }
    let h = (doc.project.grid_size as f64 / 2.0).max(1.0);
    [
        ((wp[0] / h).round() * h) as i64,
        ((wp[1] / h).round() * h) as i64,
    ]
}

/// The level a Note-tool press at `wp` puts its annotation in (`None`:
/// world scope): with World forced, none; else the active level when the
/// press is in it, else the level under the pointer (which becomes
/// active), else none.
pub fn note_scope(
    doc: &Document,
    view: &mut ViewState,
    wp: [f64; 2],
    level_under: Option<LevelIx>,
) -> Option<LevelIx> {
    if view.note_tool.world {
        return None;
    }
    if let Some(a) = view.active_level.filter(|&a| a < doc.levels.len())
        && crate::viewport::level_wrect(doc, a).contains(wp)
    {
        return Some(a);
    }
    let ix = level_under?;
    view.active_level = Some(ix);
    Some(ix)
}

/// The world a new annotation goes in: its level's, else the one shown.
fn new_note_world(doc: &Document, level: Option<LevelIx>, view: &ViewState) -> Option<usize> {
    match level {
        Some(ix) => Some(world_of(doc, ix)),
        None => (!doc.project.worlds.is_empty()).then(|| view.world_ix(doc)),
    }
}

/// An id no annotation in the project has.
pub fn fresh_note_id(doc: &Document) -> Id {
    fresh_note_id_in(&mut used_note_ids(doc))
}

/// Every annotation id in the project, for [`fresh_note_id_in`].
pub fn used_note_ids(doc: &Document) -> HashSet<Id> {
    all_notes(doc).iter().map(|r| r.note.id.clone()).collect()
}

/// An id not in `used`, which then holds it too: build `used` once
/// ([`used_note_ids`]) for any number of new ids.
pub fn fresh_note_id_in(used: &mut HashSet<Id>) -> Id {
    loop {
        let id = Id::generate();
        if used.insert(id.clone()) {
            return id;
        }
    }
}

/// `add_note` of `shape` (world px in `points`, turned into level px for a
/// level-scope note) with a fresh id, and select it.
fn add_note(
    doc: &Document,
    view: &mut ViewState,
    level: Option<LevelIx>,
    shape: Shape,
    title: &str,
) -> Option<Command> {
    let w = new_note_world(doc, level, view)?;
    let mut shape = shape;
    let scope = match level {
        Some(ix) => {
            let o = doc.level(ix).world_px;
            shape.translate([-o[0], -o[1]]);
            NoteScope::Level(doc.level(ix).slug.clone())
        }
        None => NoteScope::World(doc.project.worlds[w].slug.clone()),
    };
    let mut note = Annotation::new(shape);
    note.id = fresh_note_id(doc);
    note.title = Some(title.to_string());
    view.selection.set([SelItem::Note(note.id.clone())]);
    view.focus_title = Some(note.id.clone());
    Some(Command::AddNote {
        scope,
        note,
        index: None,
    })
}

/// A Note- or Measure-tool click at world pixel `wp`: a note or pin is
/// added at once; a path or polygon gets a vertex (the first one starts
/// the drawing and decides its scope); a measurement gets a point; a
/// connector is [`tool_click_shift`] without Shift.
pub fn tool_click(
    doc: &Document,
    view: &mut ViewState,
    wp: [f64; 2],
    level_under: Option<LevelIx>,
    ctrl: bool,
) -> Option<Command> {
    tool_click_shift(doc, view, wp, level_under, ctrl, false)
}

/// What a Connector click at `wp` lands on: an entity of a visible layer
/// in the level under it, else that level.
pub fn connector_target(
    doc: &Document,
    view: &ViewState,
    wp: [f64; 2],
    level_under: Option<LevelIx>,
) -> Option<Endpoint> {
    let ix = level_under?;
    match crate::viewport::entity_at(doc, view, ix, wp) {
        Some(id) => Some(Endpoint::Entity(id)),
        None => Some(Endpoint::Level(doc.level(ix).slug.clone())),
    }
}

/// The level a connector end is in (an entity's level).
pub fn end_level(doc: &Document, e: &Endpoint) -> Option<LevelIx> {
    match e {
        Endpoint::Level(s) => doc.level_ix(s.as_str()),
        Endpoint::Entity(id) => doc.find_entity(id).map(|f| f.0),
    }
}

/// [`tool_click`] with Shift held. With the Connector kind: the first
/// click on a room or an entity starts the connector there (`from`); after
/// it, a click on an entity, or in a level other than the one `from` is
/// in, sets `to` (a later one replaces it); any other click, and any click
/// with Shift, adds a waypoint. Enter finishes.
pub fn tool_click_shift(
    doc: &Document,
    view: &mut ViewState,
    wp: [f64; 2],
    level_under: Option<LevelIx>,
    ctrl: bool,
    shift: bool,
) -> Option<Command> {
    let p = snap_point(doc, wp, ctrl);
    if view.tool == crate::tools::Tool::Note
        && let Some(d) = &view.drawing
        && let Some(c) = &d.connector
    {
        let target = connector_target(doc, view, wp, level_under).filter(|t| {
            !shift
                && match t {
                    Endpoint::Entity(_) => *t != c.from,
                    Endpoint::Level(_) => end_level(doc, t) != end_level(doc, &c.from),
                }
        });
        let d = view.drawing.as_mut()?;
        let c = d.connector.as_mut()?;
        match target {
            Some(t) => c.to = Some(t),
            None => d.points.push(p),
        }
        return None;
    }
    if view.tool == crate::tools::Tool::Measure {
        match &mut view.drawing {
            Some(d) if d.measure && !d.done => d.points.push(p),
            _ => {
                view.drawing = Some(Drawing {
                    measure: true,
                    kind: NoteKind::Path,
                    level: None,
                    points: vec![p],
                    done: false,
                    connector: None,
                })
            }
        }
        return None;
    }
    if let Some(d) = &mut view.drawing
        && !d.measure
    {
        d.points.push(p);
        return None;
    }
    let kind = view.note_tool.kind;
    let level = note_scope(doc, view, wp, level_under);
    match kind {
        NoteKind::Note => add_note(doc, view, level, Shape::Note { px: p }, "Note"),
        NoteKind::Pin => {
            let icon = Some(view.note_tool.icon.clone()).filter(|i| !i.is_empty());
            add_note(
                doc,
                view,
                level,
                Shape::Pin {
                    px: p,
                    icon,
                    label: None,
                },
                "Pin",
            )
        }
        NoteKind::Path | NoteKind::Region => {
            view.drawing = Some(Drawing {
                measure: false,
                kind,
                level,
                points: vec![p],
                done: false,
                connector: None,
            });
            None
        }
        NoteKind::Connector => {
            let Some(from) = connector_target(doc, view, wp, level_under) else {
                view.set_status("A connector starts on a room or an entity: click one", true);
                return None;
            };
            view.active_level = end_level(doc, &from).or(view.active_level);
            view.drawing = Some(Drawing {
                measure: false,
                kind,
                level: None,
                points: Vec::new(),
                done: false,
                connector: Some(ConnectorDraft { from, to: None }),
            });
            None
        }
    }
}

/// A connector being drawn, in world px: its `from` end, the waypoints,
/// then its `to` end, or with no `to` yet the pointer (when given).
pub fn draft_line(doc: &Document, d: &Drawing, pointer: Option<[i64; 2]>) -> Vec<[i64; 2]> {
    let Some(c) = &d.connector else {
        return Vec::new();
    };
    let mut mid = d.points.clone();
    let to = c.to.as_ref().and_then(|t| end_anchor(doc, t));
    if to.is_none() {
        mid.extend(pointer);
    }
    let Some(a) = end_anchor(doc, &c.from) else {
        return mid;
    };
    match to {
        Some(b) => worlds_core::connector_polyline(a, b, &mid),
        None => {
            let mut out = vec![a.end_toward(mid.first().copied().unwrap_or(a.point))];
            out.extend(mid);
            out
        }
    }
}

/// A Note-tool drag from `from` to `to` (more than [`DRAG_PX`] on screen):
/// with the Region kind, a rect region; anything else is a click at the
/// release.
pub fn tool_drag(
    doc: &Document,
    view: &mut ViewState,
    from: [f64; 2],
    to: [f64; 2],
    level_under: Option<LevelIx>,
    ctrl: bool,
) -> Option<Command> {
    if view.tool != crate::tools::Tool::Note
        || view.note_tool.kind != NoteKind::Region
        || view.drawing.is_some()
    {
        return tool_click(doc, view, to, level_under, ctrl);
    }
    let a = snap_point(doc, from, ctrl);
    let b = snap_point(doc, to, ctrl);
    let rect = [
        a[0].min(b[0]),
        a[1].min(b[1]),
        (a[0] - b[0]).abs(),
        (a[1] - b[1]).abs(),
    ];
    if rect[2] <= 0 || rect[3] <= 0 {
        view.set_status("A region needs a width and a height", true);
        return None;
    }
    let level = note_scope(doc, view, from, level_under);
    add_note(
        doc,
        view,
        level,
        Shape::Region {
            points: Vec::new(),
            rect: Some(rect),
        },
        "Region",
    )
}

/// Enter or a double click: finish the path (2+ points) or polygon (3+)
/// as one `add_note`, or the measurement (kept on screen). Consecutive
/// repeated points (a double click's second click) count once.
pub fn finish_drawing(doc: &Document, view: &mut ViewState) -> Option<Command> {
    let d = view.drawing.take()?;
    let mut pts = d.points.clone();
    pts.dedup();
    if d.measure {
        view.drawing = Some(Drawing {
            points: pts,
            done: true,
            ..d
        });
        return None;
    }
    if let Some(c) = &d.connector {
        let Some(to) = c.to.clone() else {
            view.set_status(
                "Click a second room or entity for the connector's end, then Enter",
                true,
            );
            view.drawing = Some(Drawing { points: pts, ..d });
            return None;
        };
        // Level scope when both ends are in one level (and World isn't
        // forced), else the world of its start.
        let (la, lb) = (end_level(doc, &c.from), end_level(doc, &to));
        let level = la.filter(|_| la == lb && !view.note_tool.world);
        if let Some(ix) = la {
            view.active_level = Some(ix);
        }
        let shape = Shape::Connector {
            from: c.from.clone(),
            to,
            waypoints: pts,
            label: None,
            style: Default::default(),
        };
        return add_note(doc, view, level, shape, "Connector");
    }
    let (need, shape, title) = match d.kind {
        NoteKind::Region => (
            3,
            Shape::Region {
                points: pts.clone(),
                rect: None,
            },
            "Region",
        ),
        _ => (
            2,
            Shape::Path {
                points: pts.clone(),
            },
            "Path",
        ),
    };
    if pts.len() < need {
        view.set_status(
            format!("A {} needs at least {need} points", d.kind.name()),
            true,
        );
        view.drawing = Some(Drawing { points: pts, ..d });
        return None;
    }
    add_note(doc, view, d.level, shape, title)
}

/// Backspace while drawing: drop the last vertex (the drawing ends with
/// its last one). Returns whether a drawing was in progress.
pub fn drawing_backspace(view: &mut ViewState) -> bool {
    let Some(d) = &mut view.drawing else {
        return false;
    };
    if d.done {
        return false;
    }
    if let Some(c) = &mut d.connector {
        // A connector: the last waypoint, then its end, then the drawing.
        if d.points.pop().is_none() && c.to.take().is_none() {
            view.drawing = None;
        }
        return true;
    }
    d.points.pop();
    if d.points.is_empty() {
        view.drawing = None;
    }
    true
}

/// Escape: drop the drawing in progress (or the finished measurement).
/// Returns whether there was one.
pub fn cancel_drawing(view: &mut ViewState) -> bool {
    view.drawing.take().is_some()
}

/// The readout of a path or measurement: last segment and total length
/// in px and tiles (px / project grid, 1 decimal), dx, dy of the last
/// segment. `pointer` extends it to where the pointer is.
pub fn readout(doc: &Document, points: &[[i64; 2]], pointer: Option<[i64; 2]>) -> Option<String> {
    let mut pts = points.to_vec();
    if let Some(p) = pointer {
        pts.push(p);
    }
    if pts.len() < 2 {
        return None;
    }
    let g = doc.project.grid_size.max(1) as f64;
    let last = [&pts[pts.len() - 2], &pts[pts.len() - 1]];
    let (dx, dy) = (last[1][0] - last[0][0], last[1][1] - last[0][1]);
    let seg = (dx as f64).hypot(dy as f64);
    let total = path_length(&pts);
    Some(format!(
        "segment {seg:.0} px ({:.1} tiles) · total {total:.0} px ({:.1} tiles) · dx {dx} dy {dy}",
        seg / g,
        total / g
    ))
}

// ---------------------------------------------------------- selection edits

/// The selected items, resolved, with what can be edited.
struct Resolved<'a> {
    entities: Vec<Id>,
    notes: Vec<NoteRef<'a>>,
    locked_notes: usize,
    levels: Vec<LevelIx>,
    /// (level, layer slug, cells).
    tiles: BTreeMap<(LevelIx, Slug), Vec<[u32; 2]>>,
    /// Entities on locked layers.
    locked_entities: Option<String>,
}

/// The selected levels.
fn selected_levels(doc: &Document, view: &ViewState) -> Vec<LevelIx> {
    view.selection
        .levels()
        .filter_map(|id| doc.level_ix_by_id(id))
        .collect()
}

/// The selection, resolved. With `levels_go` (the edit moves or deletes
/// the selected levels) what is inside a selected level goes with it and
/// gets no command of its own; without it (a duplicate, which leaves
/// levels, or a move that doesn't move them) those items are edited like
/// any other.
fn resolve<'a>(doc: &'a Document, view: &ViewState, levels_go: bool) -> Resolved<'a> {
    let levels = selected_levels(doc, view);
    let in_moved = |ix: LevelIx| levels_go && levels.contains(&ix);
    let ents: Vec<Id> = view
        .selection
        .entities()
        .filter(|id| doc.find_entity(id).is_none_or(|(ix, _, _)| !in_moved(ix)))
        .cloned()
        .collect();
    let (entities, locked_entities) = crate::edit::unlocked_entities(doc, view, &ents);
    let mut notes = Vec::new();
    let mut locked_notes = 0;
    for id in view.selection.notes() {
        let Some(r) = find_note(doc, id) else {
            continue;
        };
        if r.level().is_some_and(in_moved) {
            continue;
        }
        if r.note.locked {
            locked_notes += 1;
        } else {
            notes.push(r);
        }
    }
    let mut tiles: BTreeMap<(LevelIx, Slug), Vec<[u32; 2]>> = BTreeMap::new();
    for (lid, layer, cell) in view.selection.tiles() {
        let Some(ix) = doc.level_ix_by_id(lid) else {
            continue;
        };
        if in_moved(ix) {
            continue;
        }
        let li = doc.project.layers.iter().position(|l| l.slug == *layer);
        if li.is_some_and(|i| view.layer_locked(i)) {
            continue;
        }
        tiles.entry((ix, layer.clone())).or_default().push(cell);
    }
    Resolved {
        entities,
        notes,
        locked_notes,
        levels,
        tiles,
        locked_entities,
    }
}

/// "2 locked notes left alone" and the locked-layer message, joined.
fn left_alone(r: &Resolved<'_>) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(m) = &r.locked_entities {
        parts.push(m.clone());
    }
    match r.locked_notes {
        0 => {}
        1 => parts.push("1 locked note left alone".into()),
        n => parts.push(format!("{n} locked notes left alone")),
    }
    (!parts.is_empty()).then(|| parts.join("; "))
}

/// A label for an edit of `counts` (kind name, number, single-item label).
fn label_for(verb: &str, counts: &[(&str, usize, Option<String>)]) -> String {
    let kinds: Vec<&(&str, usize, Option<String>)> =
        counts.iter().filter(|(_, n, _)| *n > 0).collect();
    let total: usize = kinds.iter().map(|k| k.1).sum();
    match kinds.as_slice() {
        [(kind, 1, Some(one))] => format!("{verb} {kind} {one}"),
        [(kind, 1, None)] => format!("{verb} {kind}"),
        [(kind, n, _)] => format!("{verb} {n} {}", plural(kind)),
        _ => format!("{verb} {total} items"),
    }
}

fn plural(kind: &str) -> String {
    match kind {
        "entity" => "entities".into(),
        "tile stack" => "tile stacks".into(),
        k => format!("{k}s"),
    }
}

/// A segment in world px.
pub type Seg = ([f64; 2], [f64; 2]);

/// A guide edge the moving levels snapped to, for the drag preview.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SnapHit {
    /// 0: x (a vertical line), 1: y.
    pub axis: usize,
    /// The world coordinate on that axis.
    pub at: i64,
    /// The guide segment along it, when the snap is to a segment (else a
    /// vertex).
    pub segment: Option<Seg>,
}

/// The delta (world px) a Select drag by `d_raw` applies to annotations
/// and levels: the project grid unless `pixel` (Ctrl); with levels moving
/// and "Snap to guides" on, each axis snaps the moving levels' bounding
/// edges to the x (or y) of vertical (horizontal) guide segments and of
/// guide vertices within [`SNAP_PX`] screen px, as built, visible guides
/// only, and (with guide snapping on or off) to the edges of the other
/// levels of their world ([`crate::arrange::level_edges`]). Also returns
/// what it snapped to.
pub fn drag_delta(
    doc: &Document,
    view: &ViewState,
    levels: &[LevelIx],
    d_raw: [f64; 2],
    pixel: bool,
) -> ([i64; 2], Vec<SnapHit>) {
    let g = doc.project.grid_size.max(1) as f64;
    let mut d = if pixel {
        [d_raw[0].round() as i64, d_raw[1].round() as i64]
    } else {
        [
            ((d_raw[0] / g).round() * g) as i64,
            ((d_raw[1] / g).round() * g) as i64,
        ]
    };
    let mut hits = Vec::new();
    if levels.is_empty() {
        return (d, hits);
    }
    let bbox = levels
        .iter()
        .map(|&ix| crate::viewport::level_wrect(doc, ix))
        .fold(WRect::EMPTY, |a, b| a.union(&b));
    let tol = SNAP_PX / view.zoom.max(1e-9);
    // Candidates per axis: (coordinate, segment along it if any).
    let mut cand: [Vec<(i64, Option<Seg>)>; 2] = crate::arrange::level_edges(doc, levels);
    for r in shown_notes(doc, view)
        .into_iter()
        .filter(|_| view.annotations.snap_to_guides)
    {
        if !matches!(r.note.shape, Shape::Guide { .. }) || !shown(view, r.note, false) {
            continue;
        }
        let pts = world_points(doc, r);
        for p in &pts {
            cand[0].push((p[0], None));
            cand[1].push((p[1], None));
        }
        for s in segments(doc, r) {
            for (axis, c) in cand.iter_mut().enumerate() {
                if (s.0[axis] - s.1[axis]).abs() < 0.5 {
                    c.push((s.0[axis].round() as i64, Some(s)));
                }
            }
        }
    }
    for axis in 0..2 {
        // The bounding box's edges, and a non-rectangular level's inner
        // edges too (the x of each vertical edge of its outline, the y of
        // each horizontal one): the notch of an L snaps like its sides.
        let mut edges = vec![bbox.min[axis], bbox.max[axis]];
        for &ix in levels {
            if doc.level(ix).has_void() {
                edges.extend(
                    crate::viewport::level_outline(doc, ix)
                        .iter()
                        .map(|p| p[axis]),
                );
            }
        }
        edges.sort_by(f64::total_cmp);
        edges.dedup();
        let mut best: Option<(f64, i64, Option<Seg>, f64)> = None;
        for &(c, seg) in &cand[axis] {
            for &e in &edges {
                let dist = (e + d_raw[axis] - c as f64).abs();
                // Prefer a segment over a lone vertex at the same distance.
                let better = match &best {
                    None => true,
                    Some((bd, _, bs, _)) => {
                        dist < *bd - 1e-9 || (dist <= *bd + 1e-9 && bs.is_none() && seg.is_some())
                    }
                };
                if dist <= tol && better {
                    best = Some((dist, c, seg, e));
                }
            }
        }
        if let Some((_, c, seg, e)) = best {
            d[axis] = c - e.round() as i64;
            hits.push(SnapHit {
                axis,
                at: c,
                segment: seg,
            });
        }
    }
    (d, hits)
}

/// What a Select drag of the selection by `d_raw` world px applies on
/// release ([`plan_move`]).
#[derive(Debug, Clone, PartialEq)]
pub struct MovePlan {
    pub commands: Vec<Command>,
    pub label: String,
    /// Locked notes or layers left alone.
    pub left_alone: Option<String>,
    /// The delta annotations and levels move by.
    pub d: [i64; 2],
    pub snaps: Vec<SnapHit>,
    /// The selection after the release: moved tile stacks at their new
    /// cells, everything else as it was.
    pub selection: Vec<SelItem>,
}

/// The commands that drag the selection by `d_raw` world px, as one
/// gesture: entities by whole cells of their layer (Ctrl: pixels, folded
/// into the cell), tile stacks by whole cells of their layer (Ctrl
/// ignored; one `set_tiles` per layer; refused if any would leave its
/// level), annotations by the [`drag_delta`] px (`update_note`), levels by
/// it (`move_level`, guide snapping). Items inside a moved level ride
/// along with it and get no command of their own; locked annotations and
/// entities on locked layers are left alone.
pub fn plan_move(
    doc: &Document,
    view: &ViewState,
    d_raw: [f64; 2],
    pixel: bool,
) -> Result<MovePlan, String> {
    let (d, snaps) = drag_delta(doc, view, &selected_levels(doc, view), d_raw, pixel);
    let r = resolve(doc, view, d != [0, 0]);
    let mut commands = crate::edit::entity_move_commands(doc, &r.entities, d_raw, pixel);
    let n_entities = commands.len();
    let mut n_tiles = 0;
    let mut moved: Vec<(LevelIx, Slug, [i64; 2])> = Vec::new();
    for ((ix, layer), cells) in &r.tiles {
        let def = doc
            .project
            .layer(layer.as_str())
            .ok_or_else(|| format!("No layer {layer}"))?;
        let g = doc.project.layer_grid(def).max(1) as f64;
        let dc = [(d_raw[0] / g).round() as i64, (d_raw[1] / g).round() as i64];
        if dc == [0, 0] {
            continue;
        }
        let cmd = tiles_command(doc, *ix, layer, cells, dc, false)?;
        n_tiles += cells.len();
        if cmd.is_some() {
            moved.push((*ix, layer.clone(), dc));
        }
        commands.extend(cmd);
    }
    let mut n_notes = 0;
    if d != [0, 0] {
        for n in &r.notes {
            let mut note = n.note.clone();
            note.shape.translate(d);
            n_notes += 1;
            commands.push(Command::UpdateNote {
                id: note.id.clone(),
                note,
            });
        }
        for &ix in &r.levels {
            let l = doc.level(ix);
            commands.push(Command::MoveLevel {
                world: doc.project.worlds[world_of(doc, ix)].slug.clone(),
                level: l.slug.clone(),
                world_px: [l.world_px[0] + d[0], l.world_px[1] + d[1]],
            });
        }
    }
    let n_levels = if d != [0, 0] { r.levels.len() } else { 0 };
    let one_entity = r.entities.first().map(|i| i.to_string());
    let one_note = r.notes.first().map(|n| n.note.id.to_string());
    let one_level = r.levels.first().map(|&ix| doc.level(ix).slug.to_string());
    let label = label_for(
        "Move",
        &[
            ("entity", n_entities, one_entity),
            ("tile stack", n_tiles, None),
            ("note", n_notes, one_note),
            ("level", n_levels, one_level),
        ],
    );
    Ok(MovePlan {
        commands,
        label,
        left_alone: left_alone(&r),
        d,
        snaps,
        selection: shift_moved_tiles(doc, &view.selection.items, &moved),
    })
}

/// One `set_tiles` moving (or with `copy` copying) the stacks at `cells`
/// of `layer` in level `ix` by `dc` cells: the stacks at their new cells,
/// the old cells not overwritten cleared (kept with `copy`). Refused when a
/// stack would leave the level.
pub fn tiles_command(
    doc: &Document,
    ix: LevelIx,
    layer: &Slug,
    cells: &[[u32; 2]],
    dc: [i64; 2],
    copy: bool,
) -> Result<Option<Command>, String> {
    let level = doc.level(ix);
    let def = doc
        .project
        .layer(layer.as_str())
        .ok_or_else(|| format!("No layer {layer}"))?;
    let size = doc.project.layer_cells(def, level);
    let Some(t) = level.tiles_layer(layer.as_str()) else {
        return Ok(None);
    };
    let picked: BTreeSet<[u32; 2]> = cells.iter().copied().collect();
    let mut tiles: Vec<TilePlacement> = Vec::new();
    let mut dest: BTreeSet<[u32; 2]> = BTreeSet::new();
    for p in t.tiles.iter().filter(|p| picked.contains(&p.cell)) {
        let c = [p.cell[0] as i64 + dc[0], p.cell[1] as i64 + dc[1]];
        if !in_layer([c[0] as i32, c[1] as i32], size) || c[0] > i32::MAX as i64 {
            return Err(format!(
                "Can't move tiles there: cell {}, {} is outside level {}",
                c[0], c[1], level.slug
            ));
        }
        let cell = [c[0] as u32, c[1] as u32];
        dest.insert(cell);
        tiles.push(TilePlacement { cell, ..p.clone() });
    }
    if tiles.is_empty() {
        return Ok(None);
    }
    let clear: Vec<[u32; 2]> = if copy {
        Vec::new()
    } else {
        picked
            .iter()
            .filter(|c| !dest.contains(*c))
            .copied()
            .collect()
    };
    Ok(Some(Command::SetTiles {
        level: level.slug.clone(),
        layer: layer.clone(),
        tiles,
        clear,
        keep_empty: false,
    }))
}

/// What an arrow key does to the selection ([`plan_nudge`]).
#[derive(Debug, Clone, PartialEq)]
pub struct NudgePlan {
    pub commands: Vec<Command>,
    pub label: String,
    /// Locked notes or layers left alone.
    pub left_alone: Option<String>,
    /// The selection after it: nudged tile stacks at their new cells.
    pub selection: Vec<SelItem>,
}

/// An arrow-key nudge of the selection by `d` (each -1, 0 or 1), every
/// movable kind: entities and tile stacks by a cell of their layer, notes
/// and levels by a project cell; with `pixel` (Shift) entities, notes and
/// levels by one pixel (an entity's into `offset_px`), and tile stacks
/// still by one cell of their layer, since a stack has no position finer
/// than its cell (as a Ctrl drag moves them). Things inside a nudged level
/// ride along with it. One entry.
pub fn plan_nudge(
    doc: &Document,
    view: &ViewState,
    d: [i32; 2],
    pixel: bool,
) -> Result<NudgePlan, String> {
    let r = resolve(doc, view, true);
    let mut commands = crate::edit::nudge_commands(doc, &r.entities, d, pixel);
    let n_entities = commands.len();
    let mut n_tiles = 0;
    let dc = [d[0] as i64, d[1] as i64];
    let mut moved: Vec<(LevelIx, Slug, [i64; 2])> = Vec::new();
    for ((ix, layer), cells) in &r.tiles {
        if let Some(c) = tiles_command(doc, *ix, layer, cells, dc, false)? {
            commands.push(c);
            n_tiles += cells.len();
            moved.push((*ix, layer.clone(), dc));
        }
    }
    let step = if pixel {
        1
    } else {
        doc.project.grid_size as i64
    };
    let dp = [d[0] as i64 * step, d[1] as i64 * step];
    for &ix in &r.levels {
        let l = doc.level(ix);
        commands.push(Command::MoveLevel {
            world: doc.project.worlds[world_of(doc, ix)].slug.clone(),
            level: l.slug.clone(),
            world_px: [l.world_px[0] + dp[0], l.world_px[1] + dp[1]],
        });
    }
    for n in &r.notes {
        let mut note = n.note.clone();
        note.shape.translate(dp);
        commands.push(Command::UpdateNote {
            id: note.id.clone(),
            note,
        });
    }
    let label = label_for(
        "Nudge",
        &[
            (
                "entity",
                n_entities,
                r.entities.first().map(|i| i.to_string()),
            ),
            ("tile stack", n_tiles, None),
            (
                "note",
                r.notes.len(),
                r.notes.first().map(|n| n.note.id.to_string()),
            ),
            (
                "level",
                r.levels.len(),
                r.levels.first().map(|&i| doc.level(i).slug.to_string()),
            ),
        ],
    );
    Ok(NudgePlan {
        commands,
        label,
        left_alone: left_alone(&r),
        selection: shift_moved_tiles(doc, &view.selection.items, &moved),
    })
}

/// What Delete does to the selection ([`plan_delete`]).
#[derive(Debug, Clone, PartialEq)]
pub struct DeletePlan {
    pub commands: Vec<Command>,
    pub label: String,
    pub left_alone: Option<String>,
    /// Selected levels it deletes: Delete asks first when there are any.
    pub levels: Vec<Slug>,
    /// "2 levels and 3 notes" for the prompt.
    pub summary: String,
}

/// Delete: `remove_entity` per entity, `remove_note` per unlocked
/// annotation, one `set_tiles` clearing the stacks per layer, `delete_level`
/// per level (things inside a deleted level go with it).
pub fn plan_delete(doc: &Document, view: &ViewState) -> DeletePlan {
    let r = resolve(doc, view, true);
    let mut commands = crate::edit::delete_commands(doc, &r.entities);
    let n_entities = commands.len();
    let mut n_tiles = 0;
    for ((ix, layer), cells) in &r.tiles {
        commands.push(Command::SetTiles {
            level: doc.level(*ix).slug.clone(),
            layer: layer.clone(),
            tiles: Vec::new(),
            clear: cells.clone(),
            keep_empty: false,
        });
        n_tiles += cells.len();
    }
    for n in &r.notes {
        commands.push(Command::RemoveNote {
            id: n.note.id.clone(),
        });
    }
    let mut levels = Vec::new();
    for &ix in &r.levels {
        let l = doc.level(ix);
        levels.push(l.slug.clone());
        commands.push(Command::DeleteLevel {
            world: doc.project.worlds[world_of(doc, ix)].slug.clone(),
            level: l.slug.clone(),
            keep_layout: false,
        });
    }
    let counts = [
        (
            "entity",
            n_entities,
            r.entities.first().map(|i| i.to_string()),
        ),
        ("tile stack", n_tiles, None),
        (
            "note",
            r.notes.len(),
            r.notes.first().map(|n| n.note.id.to_string()),
        ),
        (
            "level",
            r.levels.len(),
            r.levels.first().map(|&i| doc.level(i).slug.to_string()),
        ),
    ];
    let label = label_for("Delete", &counts);
    let parts: Vec<String> = counts
        .iter()
        .filter(|c| c.1 > 0)
        .map(|c| {
            if c.1 == 1 {
                format!("1 {}", c.0)
            } else {
                format!("{} {}", c.1, plural(c.0))
            }
        })
        .collect();
    let summary = match parts.as_slice() {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    };
    DeletePlan {
        commands,
        label,
        left_alone: left_alone(&r),
        levels,
        summary,
    }
}

/// What Ctrl-D does to the selection ([`plan_duplicate`]).
#[derive(Debug, Clone, PartialEq)]
pub struct DupPlan {
    pub commands: Vec<Command>,
    pub label: String,
    /// The copies: they become the selection.
    pub selection: Vec<SelItem>,
    pub message: Option<String>,
    /// The slugs of the level copies (`duplicate_level`), in selection
    /// order: once applied they join the selection and the first becomes
    /// the active level.
    pub levels: Vec<Slug>,
}

/// Ctrl-D: entities as before ([`crate::edit::duplicate_commands`]); each
/// annotation copied one project cell to the right with a new id, not
/// locked, links kept (locked ones too: the copy is unlocked); tile stacks
/// copied one cell of their layer to the right (stacks that would leave
/// the level are left out); each selected level gets a `duplicate_level`
/// (a copy just right of it, [`crate::clipboard::duplicate_level_command`]),
/// and what is selected inside it goes with it.
pub fn plan_duplicate(doc: &Document, view: &ViewState) -> DupPlan {
    let r = resolve(doc, view, true);
    let d = crate::edit::duplicate_commands(doc, &r.entities);
    let mut commands = d.commands;
    let mut selection: Vec<SelItem> = d.ids.iter().cloned().map(SelItem::Entity).collect();
    let mut msgs = Vec::new();
    if let Some(m) = &r.locked_entities {
        msgs.push(m.clone());
    }
    if d.overlapping > 0 {
        msgs.push(format!(
            "{} entit{} on the original's cell: the next cell is outside the level",
            d.overlapping,
            if d.overlapping == 1 {
                "y sits"
            } else {
                "ies sit"
            }
        ));
    }
    let mut n_tiles = 0;
    let mut outside = 0;
    for ((ix, layer), cells) in &r.tiles {
        let level = doc.level(*ix);
        let Some(def) = doc.project.layer(layer.as_str()) else {
            continue;
        };
        let size = doc.project.layer_cells(def, level);
        let fits: Vec<[u32; 2]> = cells
            .iter()
            .copied()
            .filter(|c| in_layer([c[0] as i32 + 1, c[1] as i32], size))
            .collect();
        outside += cells.len() - fits.len();
        if let Ok(Some(c)) = tiles_command(doc, *ix, layer, &fits, [1, 0], true) {
            commands.push(c);
            n_tiles += fits.len();
            selection.extend(fits.iter().map(|c| SelItem::Tiles {
                level: level.id.clone(),
                layer: layer.clone(),
                cell: [c[0] + 1, c[1]],
            }));
        }
    }
    if outside > 0 {
        msgs.push(format!(
            "{outside} tile stack{} at the level's right edge not copied",
            if outside == 1 { "" } else { "s" }
        ));
    }
    // Locked notes are copied too (the copy is unlocked).
    let mut all_notes: Vec<NoteRef<'_>> = Vec::new();
    for id in view.selection.notes() {
        if let Some(n) = find_note(doc, id) {
            all_notes.push(n);
        }
    }
    // Every id in use, gathered once for all the copies.
    let mut used = if all_notes.is_empty() {
        HashSet::new()
    } else {
        used_note_ids(doc)
    };
    let g = doc.project.grid_size as i64;
    // Notes inside a duplicated level go with it.
    all_notes.retain(|n| n.level().is_none_or(|ix| !r.levels.contains(&ix)));
    for n in &all_notes {
        let mut note = n.note.clone();
        note.id = fresh_note_id_in(&mut used);
        note.locked = false;
        note.shape.translate([g, 0]);
        let scope = match n.at {
            At::World(w) => NoteScope::World(doc.project.worlds[w].slug.clone()),
            At::Level(ix) => NoteScope::Level(doc.level(ix).slug.clone()),
        };
        selection.push(SelItem::Note(note.id.clone()));
        commands.push(Command::AddNote {
            scope,
            note,
            index: None,
        });
    }
    let mut levels: Vec<Slug> = Vec::new();
    for &ix in &r.levels {
        let (cmd, slug) = crate::clipboard::duplicate_level_command(doc, ix, &levels);
        commands.push(cmd);
        levels.push(slug);
    }
    let label = label_for(
        "Duplicate",
        &[
            (
                "entity",
                d.ids.len(),
                r.entities.first().map(|i| i.to_string()),
            ),
            ("tile stack", n_tiles, None),
            (
                "note",
                all_notes.len(),
                all_notes.first().map(|n| n.note.id.to_string()),
            ),
            (
                "level",
                r.levels.len(),
                r.levels.first().map(|&i| doc.level(i).slug.to_string()),
            ),
        ],
    );
    DupPlan {
        commands,
        label,
        selection,
        message: (!msgs.is_empty()).then(|| msgs.join("; ")),
        levels,
    }
}

/// The world rect `view.focus { note }` frames: the annotation's box as
/// built (a grouped guide moved by its group's offset, a level note in
/// world px), grown by 15 % of its size on each side and to at least 8
/// project cells each way (a pin is a point); and its level for a level
/// note.
pub fn focus_rect(doc: &Document, id: &Id) -> Option<(WRect, Option<LevelIx>)> {
    let r = find_note(doc, id)?;
    let b = world_bbox(doc, r)?;
    let min = 8.0 * doc.project.grid_size.max(1) as f64;
    let c = b.center();
    let w = (b.width() * 1.3).max(min);
    let h = (b.height() * 1.3).max(min);
    Some((
        WRect::new(
            [c[0] - w / 2.0, c[1] - h / 2.0],
            [c[0] + w / 2.0, c[1] + h / 2.0],
        ),
        r.level(),
    ))
}

/// Every tag annotations use, with its count (guides included), sorted;
/// `(untagged)` counts the annotations without one.
pub fn tag_counts(doc: &Document) -> Vec<(String, usize)> {
    let mut m: HashMap<String, usize> = HashMap::new();
    for r in all_notes(doc) {
        if r.note.tags.is_empty() {
            *m.entry(crate::view::UNTAGGED.to_string()).or_default() += 1;
        }
        for t in &r.note.tags {
            *m.entry(t.clone()).or_default() += 1;
        }
    }
    let mut v: Vec<(String, usize)> = m.into_iter().collect();
    v.sort();
    v
}

/// The selection `items` after a move: each tile stack of a (level,
/// layer) that `moved` moved by its cells delta goes with it; every other
/// item, a stack left alone (a locked layer, inside a moved level, a zero
/// delta) included, stays as it was.
fn shift_moved_tiles(
    doc: &Document,
    items: &[SelItem],
    moved: &[(LevelIx, Slug, [i64; 2])],
) -> Vec<SelItem> {
    items
        .iter()
        .map(|i| match i {
            SelItem::Tiles { level, layer, cell } => {
                let Some(dc) = moved
                    .iter()
                    .find(|(ix, l, _)| doc.ids[*ix] == *level && l == layer)
                    .map(|m| m.2)
                else {
                    return i.clone();
                };
                let c = [cell[0] as i64 + dc[0], cell[1] as i64 + dc[1]];
                if c[0] < 0 || c[1] < 0 {
                    return i.clone();
                }
                SelItem::Tiles {
                    level: level.clone(),
                    layer: layer.clone(),
                    cell: [c[0] as u32, c[1] as u32],
                }
            }
            other => other.clone(),
        })
        .collect()
}

// ---------------------------------------------------------- geometry handles

/// Hit distance to a geometry handle, screen px.
pub const HANDLE_PX: f64 = 6.0;

/// A geometry handle of the one selected annotation ([`handles`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Handle {
    /// Point `i`: of a path, guide or polygon region's `points`, of a
    /// connector's `waypoints`.
    Vertex(usize),
    /// A rect region's corner or edge middle: per axis the edge that moves
    /// (`-1` left/top, `1` right/bottom, `0` neither), as a level's resize
    /// handles.
    Rect([i8; 2]),
}

/// The annotation the Select tool shows handles on: the selection is
/// exactly one annotation, and it is visible.
pub fn edited_note<'a>(doc: &'a Document, view: &ViewState) -> Option<NoteRef<'a>> {
    let [crate::view::SelItem::Note(id)] = view.selection.items.as_slice() else {
        return None;
    };
    find_note(doc, id)
        .filter(|r| shown(view, r.note, false) && note_world(doc, r.at) == view.world_ix(doc))
}

/// The handles of an annotation, world px as built: each vertex of a path,
/// guide or polygon region, each waypoint of a connector (its ends follow
/// what they name and aren't handles), the corners and edge middles of a
/// rect region. Notes and pins have none: they are dragged whole.
pub fn handles(doc: &Document, r: NoteRef<'_>) -> Vec<(Handle, [f64; 2])> {
    let o = offset(doc, r.at, r.note);
    let w = |p: &[i64; 2]| [(p[0] + o[0]) as f64, (p[1] + o[1]) as f64];
    let vertices = |points: &[[i64; 2]]| -> Vec<(Handle, [f64; 2])> {
        points
            .iter()
            .enumerate()
            .map(|(i, p)| (Handle::Vertex(i), w(p)))
            .collect()
    };
    match &r.note.shape {
        Shape::Note { .. } | Shape::Pin { .. } => Vec::new(),
        Shape::Region {
            rect: Some([x, y, rw, rh]),
            ..
        } => {
            let mut out = Vec::new();
            for ey in [-1i8, 0, 1] {
                for ex in [-1i8, 0, 1] {
                    if ex == 0 && ey == 0 {
                        continue;
                    }
                    let px = [
                        (x + o[0]) as f64 + (ex + 1) as f64 / 2.0 * *rw as f64,
                        (y + o[1]) as f64 + (ey + 1) as f64 / 2.0 * *rh as f64,
                    ];
                    out.push((Handle::Rect([ex, ey]), px));
                }
            }
            out
        }
        Shape::Path { points } | Shape::Guide { points, .. } | Shape::Region { points, .. } => {
            vertices(points)
        }
        Shape::Connector { waypoints, .. } => vertices(waypoints),
    }
}

/// The handle of the [`edited_note`] within [`HANDLE_PX`] screen px of
/// `wp` (the nearest), with the annotation's id.
pub fn handle_at(
    doc: &Document,
    view: &ViewState,
    wp: [f64; 2],
    zoom: f64,
) -> Option<(Id, Handle)> {
    let r = edited_note(doc, view)?;
    let tol = HANDLE_PX / zoom.max(1e-9);
    handles(doc, r)
        .into_iter()
        .map(|(h, p)| (h, (p[0] - wp[0]).hypot(p[1] - wp[1])))
        .filter(|(_, d)| *d <= tol)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(h, _)| (r.note.id.clone(), h))
}

/// "path x is locked ..." for an edit the handles refuse.
fn locked_refusal(a: &Annotation) -> String {
    format!(
        "{} {} is locked: untick Locked in the Inspector to edit it",
        a.shape.name(),
        a.id
    )
}

/// Where a dragged vertex lands, world px: on half the project grid, or
/// with `pixel` (Ctrl) on the pixel; with "Snap to guides" on, each axis
/// snaps instead to the x (y) of a vertical (horizontal) guide segment or
/// a guide vertex within [`SNAP_PX`] screen px (visible guides as built,
/// not the one being edited). Also returns what it snapped to.
pub fn snap_vertex(
    doc: &Document,
    view: &ViewState,
    editing: &Id,
    target: [f64; 2],
    pixel: bool,
) -> ([i64; 2], Vec<SnapHit>) {
    let mut p = snap_point(doc, target, pixel);
    let mut hits = Vec::new();
    if !view.annotations.snap_to_guides {
        return (p, hits);
    }
    let tol = SNAP_PX / view.zoom.max(1e-9);
    let mut cand: [Vec<(i64, Option<Seg>)>; 2] = [Vec::new(), Vec::new()];
    for r in shown_notes(doc, view) {
        if !matches!(r.note.shape, Shape::Guide { .. })
            || r.note.id == *editing
            || !shown(view, r.note, false)
        {
            continue;
        }
        for q in world_points(doc, r) {
            cand[0].push((q[0], None));
            cand[1].push((q[1], None));
        }
        for s in segments(doc, r) {
            for (axis, c) in cand.iter_mut().enumerate() {
                if (s.0[axis] - s.1[axis]).abs() < 0.5 {
                    c.push((s.0[axis].round() as i64, Some(s)));
                }
            }
        }
    }
    for (axis, cands) in cand.iter().enumerate() {
        // The nearest; a segment over a lone vertex at the same distance.
        let best = cands
            .iter()
            .map(|&(c, seg)| ((target[axis] - c as f64).abs(), c, seg))
            .filter(|(d, _, _)| *d <= tol)
            .min_by(|a, b| a.0.total_cmp(&b.0).then(b.2.is_some().cmp(&a.2.is_some())));
        if let Some((_, c, seg)) = best {
            p[axis] = c;
            hits.push(SnapHit {
                axis,
                at: c,
                segment: seg,
            });
        }
    }
    (p, hits)
}

/// What dragging a handle applies on release ([`plan_vertex_move`]).
#[derive(Debug, Clone, PartialEq)]
pub struct VertexPlan {
    /// One `update_note`, or `None` when the drag changes nothing.
    pub command: Option<Command>,
    pub label: String,
    /// The annotation as the release leaves it (for the preview).
    pub note: Annotation,
    pub snaps: Vec<SnapHit>,
}

/// A handle `h` of the [`edited_note`] dragged by `d_raw` world px: the
/// vertex (or waypoint) moves to where [`snap_vertex`] puts it; a rect
/// region's handle moves its edges there (the rect is kept the right way
/// round; one with no width or height is refused). Refused for a locked
/// annotation (its handles show, but don't edit).
pub fn plan_vertex_move(
    doc: &Document,
    view: &ViewState,
    h: Handle,
    d_raw: [f64; 2],
    pixel: bool,
) -> Result<VertexPlan, String> {
    let r = edited_note(doc, view).ok_or("Select one annotation to edit its points")?;
    let start = handles(doc, r)
        .into_iter()
        .find(|(x, _)| *x == h)
        .map(|(_, p)| p)
        .ok_or("That point is gone")?;
    let a = r.note;
    let target = [start[0] + d_raw[0], start[1] + d_raw[1]];
    let (p, snaps) = snap_vertex(doc, view, &a.id, target, pixel);
    let o = offset(doc, r.at, a);
    let p = [p[0] - o[0], p[1] - o[1]];
    let mut note = a.clone();
    match (&mut note.shape, h) {
        (
            Shape::Path { points }
            | Shape::Guide { points, .. }
            | Shape::Region {
                points, rect: None, ..
            }
            | Shape::Connector {
                waypoints: points, ..
            },
            Handle::Vertex(i),
        ) if i < points.len() => points[i] = p,
        (
            Shape::Region {
                rect: Some(rect), ..
            },
            Handle::Rect(e),
        ) => {
            let [x, y, w, hh] = *rect;
            let (mut x0, mut y0, mut x1, mut y1) = (x, y, x + w, y + hh);
            match e[0] {
                -1 => x0 = p[0],
                1 => x1 = p[0],
                _ => {}
            }
            match e[1] {
                -1 => y0 = p[1],
                1 => y1 = p[1],
                _ => {}
            }
            let nr = [x0.min(x1), y0.min(y1), (x1 - x0).abs(), (y1 - y0).abs()];
            if nr[2] <= 0 || nr[3] <= 0 {
                return Err("A region needs a width and a height".into());
            }
            *rect = nr;
        }
        _ => return Err("That point is gone".into()),
    }
    let label = match h {
        Handle::Vertex(i) => format!("Move point {i} of {}", a.id),
        Handle::Rect(_) => format!("Resize region {}", a.id),
    };
    if note == *a {
        return Ok(VertexPlan {
            command: None,
            label,
            note,
            snaps,
        });
    }
    if a.locked {
        return Err(locked_refusal(a));
    }
    Ok(VertexPlan {
        command: Some(Command::UpdateNote {
            id: a.id.clone(),
            note: note.clone(),
        }),
        label,
        note,
        snaps,
    })
}

/// Where an Alt+click at `wp` adds a point to the [`edited_note`]: the
/// segment within [`LINE_HIT_PX`] screen px of it (`None` when there is
/// none, so the click selects as usual). Returns the `update_note` with
/// the new point (on half the project grid, Ctrl: the pixel) inserted
/// between the segment's ends, and its handle. A rect region has no
/// points to add to, and a locked annotation is refused.
pub fn plan_vertex_insert(
    doc: &Document,
    view: &ViewState,
    wp: [f64; 2],
    zoom: f64,
    pixel: bool,
) -> Option<Result<(Command, Handle), String>> {
    let r = edited_note(doc, view)?;
    let tol = LINE_HIT_PX / zoom.max(1e-9);
    let k = segments(doc, r)
        .iter()
        .enumerate()
        .map(|(k, (a, b))| (k, seg_dist(wp, *a, *b)))
        .filter(|(_, d)| *d <= tol)
        .min_by(|a, b| a.1.total_cmp(&b.1))?
        .0;
    let a = r.note;
    if a.locked {
        return Some(Err(locked_refusal(a)));
    }
    let o = offset(doc, r.at, a);
    let p = snap_point(doc, wp, pixel);
    let p = [p[0] - o[0], p[1] - o[1]];
    let mut note = a.clone();
    // Segment k joins point k and k + 1 (the last of a closed outline joins
    // the last point back to the first: the new one goes at the end). A
    // connector's line starts at its `from` end, so segment k ends at
    // waypoint k.
    let i = match &mut note.shape {
        Shape::Region { rect: Some(_), .. } => {
            return Some(Err(
                "A rect region has no points to add to: drag its edges and corners".into(),
            ));
        }
        Shape::Path { points } | Shape::Guide { points, .. } | Shape::Region { points, .. } => {
            let i = (k + 1).min(points.len());
            points.insert(i, p);
            i
        }
        Shape::Connector { waypoints, .. } => {
            let i = k.min(waypoints.len());
            waypoints.insert(i, p);
            i
        }
        Shape::Note { .. } | Shape::Pin { .. } => return None,
    };
    Some(Ok((
        Command::UpdateNote {
            id: a.id.clone(),
            note,
        },
        Handle::Vertex(i),
    )))
}

/// Delete with a highlighted point ([`ViewState::vertex`]) of the
/// [`edited_note`]: the `update_note` without it, and its label. A path
/// (and an open guide) keeps at least 2 points, a polygon (a closed guide)
/// 3; a rect's handles and a locked annotation are refused. `None` when no
/// point of the edited annotation is highlighted (Delete deletes the
/// selection).
pub fn plan_vertex_delete(
    doc: &Document,
    view: &ViewState,
) -> Option<Result<(Command, String), String>> {
    let r = edited_note(doc, view)?;
    let (id, h) = view.vertex.as_ref()?;
    if *id != r.note.id {
        return None;
    }
    let a = r.note;
    let Handle::Vertex(i) = *h else {
        return Some(Err(
            "A rect region's corners and edges can't be removed".into()
        ));
    };
    if a.locked {
        return Some(Err(locked_refusal(a)));
    }
    let mut note = a.clone();
    let (points, min, what) = match &mut note.shape {
        Shape::Path { points } => (points, 2, "A path"),
        Shape::Guide {
            points,
            closed: true,
            ..
        } => (points, 3, "A closed guide"),
        Shape::Guide { points, .. } => (points, 2, "A guide"),
        Shape::Region { points, .. } => (points, 3, "A polygon region"),
        Shape::Connector { waypoints, .. } => (waypoints, 0, "A connector"),
        Shape::Note { .. } | Shape::Pin { .. } => return None,
    };
    if i >= points.len() {
        return None;
    }
    if points.len() <= min {
        return Some(Err(format!("{what} keeps at least {min} points")));
    }
    points.remove(i);
    Some(Ok((
        Command::UpdateNote {
            id: a.id.clone(),
            note,
        },
        format!("Remove point {i} of {}", a.id),
    )))
}
