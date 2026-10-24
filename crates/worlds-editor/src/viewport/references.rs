//! Reference images on the map: drawn under every layer through the tile
//! pipeline ([`super::gpu`]: one textured quad each, texture slots from
//! [`REF_TEX_BASE`]), world references first, then each level's in level
//! draw order, each list bottom first. Never in `view.snapshot` renders
//! (they aren't part of the game).
//!
//! The "References" pseudo-layer at the bottom of the Layers pane has an
//! eye (draw them at all) and a lock (the canvas doesn't move them), and
//! when it is the active one, the Select tool works on references: a click
//! selects the topmost one under the pointer, a drag moves it, the selected
//! one's corner handles scale it (the opposite corner stays). A release is
//! one `update_reference`. Locked references are selected but not moved.
//!
//! Images load on first use and hot-reload like tilesets
//! ([`RefImages::reload`]); an image that doesn't load draws as the missing
//! texture at [`MISSING_SIZE`].

use std::collections::HashMap;

use egui::{Color32, Pos2, Rect, Stroke, StrokeKind};
use serde::{Deserialize, Serialize};
use worlds_core::command::NoteScope;
use worlds_core::{Command, Id, Reference};

use super::camera::WRect;
use super::gpu::{self, DrawCmd, Gpu, REF_TEX_BASE, ViewportGpu};
use super::scene::{Instance, TEX_MISSING};
use crate::doc::{Document, LevelIx};
use crate::tools::Tool;

/// Drawn size in image px of a reference whose image doesn't load.
pub const MISSING_SIZE: [u32; 2] = [128, 128];
/// Screen px around a corner that grabs its handle.
const HANDLE_PX: f32 = 7.0;

/// The pseudo-layer's eye and lock, saved in `.worlds/ui.json` as
/// `references` (editor state, not project data).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RefsUi {
    pub visible: bool,
    pub locked: bool,
}

impl Default for RefsUi {
    fn default() -> Self {
        RefsUi {
            visible: true,
            locked: false,
        }
    }
}

/// What the editor knows about reference images besides the project.
#[derive(Debug, Clone, Default)]
pub struct RefsView {
    pub ui: RefsUi,
    /// The References pseudo-layer is the active layer: the Select tool
    /// picks and drags references.
    pub active: bool,
    /// The selected reference (the Inspector shows it).
    pub selected: Option<Id>,
    /// A canvas drag in progress.
    pub drag: Option<RefDrag>,
    /// Image size in px by image path, as loaded (for hit tests).
    pub sizes: HashMap<String, [u32; 2]>,
    /// The Inspector's copy while a value is being dragged (committed as
    /// one entry when the drag ends).
    pub edit: Option<Reference>,
    /// The Layers pane lists the references under the pseudo-layer.
    pub expanded: bool,
}

/// A drag of a reference on the canvas.
#[derive(Debug, Clone, PartialEq)]
pub struct RefDrag {
    pub id: Id,
    pub kind: DragKind,
    /// The reference when the drag started.
    pub start: Reference,
    /// World px where the press was, and where the pointer is now.
    pub from: [f64; 2],
    pub to: [f64; 2],
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DragKind {
    Move,
    /// A corner: `[0 | 1, 0 | 1]`, 1 being the right / bottom edge.
    Scale([u8; 2]),
}

/// A reference and where it is: world-pixel origin of its coordinates.
#[derive(Debug, Clone, Copy)]
pub struct RefAt<'a> {
    pub reference: &'a Reference,
    /// World px of its coordinates' origin: `[0, 0]` for a world reference,
    /// the level's `world_px` for a level one.
    pub origin: [i64; 2],
    /// The level it belongs to, if any.
    pub level: Option<LevelIx>,
    /// The world it belongs to (index into `project.worlds`).
    pub world: usize,
}

impl RefAt<'_> {
    pub fn scope(&self, doc: &Document) -> NoteScope {
        match self.level {
            Some(ix) => NoteScope::Level(doc.level(ix).slug.clone()),
            None => NoteScope::World(doc.project.worlds[self.world].slug.clone()),
        }
    }
}

/// Every reference in drawing order: the worlds' first, then each level's
/// in `order` (level draw order), each list bottom first.
pub fn all<'a>(doc: &'a Document, order: &[LevelIx]) -> Vec<RefAt<'a>> {
    let mut out = Vec::new();
    for (w, world) in doc.project.worlds.iter().enumerate() {
        out.extend(world.references.iter().map(|r| RefAt {
            reference: r,
            origin: [0, 0],
            level: None,
            world: w,
        }));
    }
    for &ix in order {
        if ix >= doc.levels.len() {
            continue;
        }
        let l = doc.level(ix);
        out.extend(l.references.iter().map(|r| RefAt {
            reference: r,
            origin: l.world_px,
            level: Some(ix),
            world: doc.levels[ix].world,
        }));
    }
    out
}

/// The references the map shows in world `world`: its world references,
/// then those of the levels in `order` that belong to it.
pub fn shown<'a>(doc: &'a Document, world: usize, order: &[LevelIx]) -> Vec<RefAt<'a>> {
    let mut out = all(doc, order);
    out.retain(|r| r.world == world);
    out
}

/// The reference with this id.
pub fn find<'a>(doc: &'a Document, id: &Id) -> Option<RefAt<'a>> {
    let order: Vec<LevelIx> = (0..doc.levels.len()).collect();
    all(doc, &order).into_iter().find(|r| r.reference.id == *id)
}

/// The image size to draw a reference at: its image's, or [`MISSING_SIZE`].
pub fn image_size(view: &RefsView, r: &Reference) -> [u32; 2] {
    view.sizes.get(&r.image).copied().unwrap_or(MISSING_SIZE)
}

/// World rect of a reference as it would be drawn.
pub fn world_rect(view: &RefsView, r: RefAt<'_>) -> WRect {
    rect_of(view, r.reference, r.origin)
}

fn rect_of(view: &RefsView, r: &Reference, origin: [i64; 2]) -> WRect {
    let [w, h] = image_size(view, r);
    WRect::from_pos_size(
        [(origin[0] + r.px[0]) as f64, (origin[1] + r.px[1]) as f64],
        [w as f64 * r.scale, h as f64 * r.scale],
    )
}

/// Whether the map draws references at all in world `world`.
pub fn drawn(doc: &Document, view: &RefsView, world: usize) -> bool {
    view.ui.visible
        && (doc
            .project
            .worlds
            .get(world)
            .is_some_and(|w| !w.references.is_empty())
            || (0..doc.levels.len())
                .any(|i| doc.levels[i].world == world && !doc.level(i).references.is_empty()))
}

/// One loaded image.
#[derive(Debug, Clone, Copy)]
struct RefTex {
    slot: u32,
    /// The image's size in px (the texture may be smaller: a GPU limit).
    size: [u32; 2],
    /// The texture's size.
    tex: [u32; 2],
}

/// Reference image textures by image path (in [`ViewportGpu`]).
#[derive(Debug, Default)]
pub struct RefImages {
    /// `None`: it didn't load (drawn as missing until reloaded).
    by_path: HashMap<String, Option<RefTex>>,
    next_slot: u32,
    free: Vec<u32>,
}

impl RefImages {
    /// Forget `path` so it is read again next frame (hot reload). Returns
    /// whether it was in use.
    pub fn reload(&mut self, path: &str) -> bool {
        match self.by_path.remove(path) {
            Some(Some(t)) => {
                self.free.push(t.slot);
                true
            }
            Some(None) => true,
            None => false,
        }
    }

    /// Forget everything (a reload of the project; the slots stay).
    pub fn clear(&mut self) {
        let slots: Vec<u32> = self
            .by_path
            .drain()
            .filter_map(|(_, t)| t)
            .map(|t| t.slot)
            .collect();
        self.free.extend(slots);
    }

    fn slot(&mut self) -> u32 {
        self.free.pop().unwrap_or_else(|| {
            self.next_slot += 1;
            REF_TEX_BASE + self.next_slot - 1
        })
    }
}

/// Image `path` (project-relative) changed on disk: read it again before
/// it is next drawn. Returns whether a reference uses it.
pub fn reload_image(gpu: &Gpu, path: &str) -> bool {
    let mut r = gpu.renderer.write();
    r.callback_resources
        .get_mut::<ViewportGpu>()
        .is_some_and(|vg| vg.ref_images.reload(path))
}

/// Read `path` (project-relative) for a texture at most `max` px a side.
fn load(doc: &Document, path: &str, max: u32) -> Option<image::RgbaImage> {
    let p = std::path::Path::new(path);
    if path.is_empty()
        || p.is_absolute()
        || p.components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        tracing::warn!("reference image {path:?} isn't a path inside the project");
        return None;
    }
    match image::open(doc.dir.join(p)) {
        Ok(img) => {
            let mut img = img.to_rgba8();
            if img.width() > max || img.height() > max {
                let k = max as f64 / img.width().max(img.height()) as f64;
                let (w, h) = (
                    ((img.width() as f64 * k) as u32).max(1),
                    ((img.height() as f64 * k) as u32).max(1),
                );
                img = image::imageops::resize(&img, w, h, image::imageops::FilterType::Triangle);
            }
            Some(img)
        }
        Err(e) => {
            tracing::warn!("reference image {path:?}: {e}");
            None
        }
    }
}

/// Make sure every visible reference's image is loaded, write this frame's
/// quads and draws into `vg`'s slot `slot` (this tab's), and note image sizes in `view` (for hit
/// tests). With the pseudo-layer hidden there are no draws.
pub fn sync(
    vg: &mut ViewportGpu,
    gpu: &Gpu,
    doc: &Document,
    world: usize,
    view: &mut RefsView,
    order: &[LevelIx],
    visible: &WRect,
    slot: usize,
) {
    vg.slot(&gpu.device, slot).ref_draws.clear();
    if !view.ui.visible {
        return;
    }
    let refs = shown(doc, world, order);
    let mut images = std::mem::take(&mut vg.ref_images);
    let max = gpu.device.limits().max_texture_dimension_2d.max(1);
    for r in &refs {
        let path = &r.reference.image;
        if images.by_path.contains_key(path) {
            continue;
        }
        let t = load(doc, path, max).map(|img| {
            let slot = images.slot();
            let size = image::image_dimensions(doc.dir.join(path))
                .map(|(w, h)| [w, h])
                .unwrap_or([img.width(), img.height()]);
            vg.set_texture(
                &gpu.device,
                &gpu.queue,
                slot,
                img.width(),
                img.height(),
                img.as_raw(),
            );
            RefTex {
                slot,
                size,
                tex: [img.width(), img.height()],
            }
        });
        images.by_path.insert(path.clone(), t);
    }
    view.sizes = images
        .by_path
        .iter()
        .filter_map(|(p, t)| t.map(|t| (p.clone(), t.size)))
        .collect();
    let drag = view.drag.as_ref().map(|d| (d.id.clone(), dragged(d, view)));
    let mut inst = Vec::new();
    for r in &refs {
        let mut reference = r.reference;
        if let Some((id, moved)) = &drag
            && *id == reference.id
        {
            reference = moved;
        }
        if !reference.visible || reference.opacity <= 0.0 {
            continue;
        }
        let rect = rect_of(view, reference, r.origin);
        if !rect.intersects(visible) {
            continue;
        }
        let (tex, uv) = match images.by_path.get(&reference.image).copied().flatten() {
            Some(t) => (t.slot, [0.0, 0.0, t.tex[0] as f32, t.tex[1] as f32]),
            None => (TEX_MISSING, [0.0, 0.0, 2.0, 2.0]),
        };
        let start = inst.len() as u32;
        inst.push(Instance {
            pos: [rect.min[0] as f32, rect.min[1] as f32],
            size: [rect.width() as f32, rect.height() as f32],
            uv,
            color: [
                255,
                255,
                255,
                (reference.opacity.clamp(0.0, 1.0) * 255.0) as u8,
            ],
            // A slot of the opacity table the layers don't use (1.0).
            layer: (gpu::MAX_LAYERS - 1) as u32,
        });
        vg.slot(&gpu.device, slot).ref_draws.push(DrawCmd {
            chunk: 0,
            tex,
            start,
            end: start + 1,
        });
    }
    vg.set_refs(&gpu.device, &gpu.queue, slot, &inst);
    vg.ref_images = images;
}

/// Where a drag puts its reference.
pub fn dragged(d: &RefDrag, view: &RefsView) -> Reference {
    let mut r = d.start.clone();
    match d.kind {
        DragKind::Move => {
            r.px = [
                d.start.px[0] + (d.to[0] - d.from[0]).round() as i64,
                d.start.px[1] + (d.to[1] - d.from[1]).round() as i64,
            ];
        }
        DragKind::Scale(corner) => {
            let [w, h] = image_size(view, &d.start);
            let (w, h) = (w.max(1) as f64, h.max(1) as f64);
            let s0 = d.start.scale;
            // The opposite corner stays, in the reference's own pixels.
            let anchor = [
                d.start.px[0] as f64 + if corner[0] == 1 { 0.0 } else { w * s0 },
                d.start.px[1] as f64 + if corner[1] == 1 { 0.0 } else { h * s0 },
            ];
            let delta = [d.to[0] - d.from[0], d.to[1] - d.from[1]];
            let grab = [
                d.start.px[0] as f64 + if corner[0] == 1 { w * s0 } else { 0.0 } + delta[0],
                d.start.px[1] as f64 + if corner[1] == 1 { h * s0 } else { 0.0 } + delta[1],
            ];
            let s = ((grab[0] - anchor[0]).abs() / w)
                .max((grab[1] - anchor[1]).abs() / h)
                .max(0.01);
            let s = (s * 1000.0).round() / 1000.0;
            r.scale = s;
            r.px = [
                if corner[0] == 1 {
                    anchor[0]
                } else {
                    anchor[0] - w * s
                }
                .round() as i64,
                if corner[1] == 1 {
                    anchor[1]
                } else {
                    anchor[1] - h * s
                }
                .round() as i64,
            ];
        }
    }
    r
}

/// The four corners of a screen rect with their `[x, y]` edge flags.
fn corners(r: Rect) -> [([u8; 2], Pos2); 4] {
    [
        ([0, 0], r.left_top()),
        ([1, 0], r.right_top()),
        ([0, 1], r.left_bottom()),
        ([1, 1], r.right_bottom()),
    ]
}

/// The pointer and buttons, as the viewport read them this frame.
pub struct Input<'a> {
    pub tool: Tool,
    pub hand: bool,
    pub hovered: bool,
    pub pressed: bool,
    pub down: bool,
    pub released: bool,
    pub escape: bool,
    /// Nothing else holds the pointer (no viewport gesture, no pick, no
    /// drawing).
    pub free: bool,
    pub pointer_world: Option<[f64; 2]>,
    pub pointer: Option<Pos2>,
    pub to_screen: &'a dyn Fn([f64; 2]) -> Pos2,
}

/// The Select tool on references, when the pseudo-layer is active: press,
/// drag and release (see the module docs). Returns true when it took the
/// press (or holds a drag), so the viewport's own Select gesture doesn't
/// start. A finished drag that moved the reference queues one
/// `update_reference` in `out`.
pub fn interact(
    doc: &Document,
    world: usize,
    view: &mut RefsView,
    order: &[LevelIx],
    i: &Input<'_>,
    out: &mut Vec<Command>,
) -> bool {
    if let Some(d) = &mut view.drag {
        if i.escape {
            view.drag = None;
            return true;
        }
        if let Some(wp) = i.pointer_world {
            d.to = wp;
        }
        if i.released || !i.down {
            let d = view.drag.take().expect("checked above");
            let r = dragged(&d, view);
            if r != d.start {
                out.push(Command::UpdateReference {
                    id: d.id,
                    reference: r,
                });
            }
        }
        return true;
    }
    let usable = view.active && view.ui.visible && i.tool == Tool::Select && !i.hand && i.free;
    if !(usable && i.hovered && i.pressed) {
        return false;
    }
    let (Some(wp), Some(p)) = (i.pointer_world, i.pointer) else {
        return false;
    };
    let refs = shown(doc, world, order);
    // The selected one's corner handles first.
    if !view.ui.locked
        && let Some(sel) = &view.selected
        && let Some(r) = refs.iter().find(|r| r.reference.id == *sel)
        && !r.reference.locked
    {
        let wr = world_rect(view, *r);
        let sr = Rect::from_two_pos((i.to_screen)(wr.min), (i.to_screen)(wr.max));
        if let Some((corner, _)) = corners(sr)
            .into_iter()
            .find(|(_, c)| c.distance(p) <= HANDLE_PX)
        {
            view.drag = Some(RefDrag {
                id: r.reference.id.clone(),
                kind: DragKind::Scale(corner),
                start: r.reference.clone(),
                from: wp,
                to: wp,
            });
            return true;
        }
    }
    let hit = refs
        .iter()
        .rev()
        .find(|r| r.reference.visible && world_rect(view, **r).contains(wp));
    let Some(r) = hit else {
        view.selected = None;
        return false;
    };
    view.selected = Some(r.reference.id.clone());
    if !view.ui.locked && !r.reference.locked {
        view.drag = Some(RefDrag {
            id: r.reference.id.clone(),
            kind: DragKind::Move,
            start: r.reference.clone(),
            from: wp,
            to: wp,
        });
    }
    true
}

/// The selected reference's outline and corner handles (where a drag puts
/// it, during one).
pub fn draw_overlay(
    painter: &egui::Painter,
    doc: &Document,
    view: &RefsView,
    world: usize,
    to_screen: &dyn Fn([f64; 2]) -> Pos2,
    accent: Color32,
) {
    let Some(sel) = &view.selected else {
        return;
    };
    let Some(at) = find(doc, sel).filter(|at| at.world == world) else {
        return;
    };
    let r = match &view.drag {
        Some(d) if d.id == *sel => dragged(d, view),
        _ => at.reference.clone(),
    };
    let wr = rect_of(view, &r, at.origin);
    let sr = Rect::from_two_pos(to_screen(wr.min), to_screen(wr.max));
    painter.rect_stroke(sr, 0.0, Stroke::new(1.5, accent), StrokeKind::Outside);
    if r.locked || view.ui.locked || !view.active {
        return;
    }
    for (_, c) in corners(sr) {
        let h = Rect::from_center_size(c, egui::vec2(7.0, 7.0));
        painter.rect_filled(h, 0.0, Color32::from_gray(20));
        painter.rect_stroke(h, 0.0, Stroke::new(1.0, accent), StrokeKind::Inside);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sketch() -> Reference {
        Reference::new(Id::new("refsketch2").unwrap(), "art/sketch.png", [10, 20])
    }

    fn view() -> RefsView {
        let mut v = RefsView::default();
        v.sizes.insert("art/sketch.png".into(), [100, 50]);
        v
    }

    #[test]
    fn a_move_drag_moves_by_whole_pixels() {
        let d = RefDrag {
            id: sketch().id,
            kind: DragKind::Move,
            start: sketch(),
            from: [0.0, 0.0],
            to: [5.4, -3.6],
        };
        assert_eq!(dragged(&d, &view()).px, [15, 16]);
    }

    #[test]
    fn a_corner_drag_scales_about_the_opposite_corner() {
        let v = view();
        // Bottom right out by 100 px: twice the size, top left stays.
        let d = RefDrag {
            id: sketch().id,
            kind: DragKind::Scale([1, 1]),
            start: sketch(),
            from: [110.0, 70.0],
            to: [210.0, 70.0],
        };
        let r = dragged(&d, &v);
        assert_eq!((r.scale, r.px), (2.0, [10, 20]));
        // Top left in by 50 px: half the size, bottom right stays at
        // [110, 70].
        let d = RefDrag {
            kind: DragKind::Scale([0, 0]),
            from: [10.0, 20.0],
            to: [60.0, 45.0],
            ..d
        };
        let r = dragged(&d, &v);
        assert_eq!((r.scale, r.px), (0.5, [60, 45]));
    }
}
