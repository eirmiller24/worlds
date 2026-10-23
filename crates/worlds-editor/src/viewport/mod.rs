//! The map viewport: one continuous world space with every level at its
//! world position.
//!
//! Drawing, back to front, within the viewport's egui painter:
//!
//! 1. Background, then one filled rect per visible level (its bg color).
//! 2. Zoomed out (a cell smaller than [`DETAIL_CELL_PX`] screen px): level
//!    names. Zoomed in: one paint callback that draws the visible chunks
//!    of every int_grid, tiles and auto layer with instanced quads
//!    ([`gpu`]), then entity boxes.
//! 3. Overlays through egui: grid of the active level, level outlines,
//!    the active level's highlight, the tool preview.
//!
//! Input: wheel zooms to the cursor in [`camera::ZOOM_STEPS`]; middle or
//! right drag, Space + left drag, or the hand tool pans; a right click
//! opens the context menu. Left button with another tool makes the level
//! under the pointer active and produces [`ToolEvent`]s for the app to
//! forward to the command layer. With Select ([`select_press`]), a press
//! on an entity selects it and grabs the selection (a drag moves it), on
//! empty space in the active level starts a marquee, on the active level's
//! name tab ([`level_tab`], drawn at every zoom) or another level's body
//! grabs that level (a drag moves it), on the active level's edges or
//! corners grabs a resize handle; a double click frames the level. An
//! armed canvas pick ([`crate::view::PickRequest`]) takes the next click
//! instead ([`pick_at`]), whatever the tool; Escape cancels it.

pub mod camera;
pub mod gpu;
pub mod notes;
pub mod scene;

use std::time::Duration;

use egui::{Align2, Color32, CursorIcon, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, pos2};
use worlds_core::{LayerKind, Level};

use crate::app::actions::Action;
use crate::doc::{Document, LevelIx};
use crate::tools::{
    Brush, CellHit, Grab, Invalidation, Tool, ToolEvent, ToolInput, cell_rect, line_cells,
    resize_level,
};
use crate::view::ViewState;
use camera::{Camera, WRect};
use gpu::{DrawCmd, Gpu, Uniforms, ViewportCallback, ViewportGpu};
use scene::{ChunkRef, Scene, parse_color, tileset_tex};

/// Below this many screen px per cell, levels draw as flat rects.
pub const DETAIL_CELL_PX: f64 = 4.0;
/// Grid lines show when a cell is at least this many screen px.
pub const GRID_CELL_PX: f64 = 8.0;

/// What to frame once the viewport knows its size.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Fit {
    World,
    Level(LevelIx),
    /// A rect of world pixels (`view.focus { world_rect }`).
    Rect(WRect),
}

/// Status bar readout for the pointer.
#[derive(Debug, Clone, Default)]
pub struct HoverInfo {
    pub world_px: [f64; 2],
    pub level: Option<String>,
    pub cell: Option<[i32; 2]>,
    /// Value, tile or entity under the pointer in the active layer.
    pub what: String,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ViewStats {
    pub detail: bool,
    pub chunks: usize,
    pub draws: usize,
    pub instances: usize,
    pub rebuilt: usize,
    pub rebuild_ms: f64,
    pub pending: usize,
    pub uploaded: usize,
}

pub struct ViewportCx<'a> {
    pub doc: Option<&'a Document>,
    pub gpu: &'a Gpu,
    pub view: &'a mut ViewState,
    /// This frame's tool events go here.
    pub events: &'a mut Vec<ToolEvent>,
    /// Context menu picks go here.
    pub actions: &'a mut Vec<Action>,
    pub accent: Color32,
    /// Pending agent batches to draw as ghosts (approval mode).
    pub ghosts: &'a [crate::ghost::Ghost],
    /// The Rules pane's selected rule: where it fired and where it matches
    /// but was blocked, in the active level.
    pub highlight: Option<&'a crate::app::rules::Highlight>,
    /// Tileset atlas textures by slug (the palette's cache), for entity
    /// icons; `None` (snapshots): boxes get no icon.
    pub atlas: Option<&'a Atlas>,
}

/// Tileset atlas textures by slug (`None`: the image failed to load).
pub type Atlas = std::collections::HashMap<String, Option<egui::TextureHandle>>;

pub struct Viewport {
    pub camera: Camera,
    scene: Option<Scene>,
    visible: Vec<ChunkRef>,
    line_buf: Vec<[i32; 2]>,
    wheel_acc: f32,
    pinch_acc: f32,
    gesture: Option<(CellHit, CellHit, Grab)>,
    /// A Note or Measure tool press: world px, screen point, level under.
    note_press: Option<([f64; 2], Pos2, Option<LevelIx>)>,
    /// The Note or Measure tool's readout (segment and total length), for
    /// the status bar.
    pub readout: Option<String>,
    /// Where the Paint mode strip was drawn this frame (`None`: it wasn't;
    /// the tool isn't a Paint mode).
    pub paint_strip: Option<Rect>,
    /// The other levels' name labels drawn this frame (screen points), by
    /// level (tests).
    pub level_labels: Vec<(LevelIx, Rect)>,
    /// Where the active level's name tab was drawn this frame (tests).
    pub level_tab: Option<Rect>,
    /// The scene was rebuilt: drop every GPU chunk buffer next frame.
    clear_gpu: bool,
    /// Where the context menu was opened: world px and the level there.
    context_at: Option<([f64; 2], Option<LevelIx>)>,
    pending_fit: Option<Fit>,
    /// Viewport size in physical px, as of the last frame.
    pub size_px: [f64; 2],
    pub hover: Option<HoverInfo>,
    pub tool_input: Option<ToolInput>,
    pub stats: ViewStats,
    /// Max time per frame spent rebuilding chunks; `None` = no limit
    /// (headless renders want the whole picture in one frame).
    pub rebuild_budget: Option<Duration>,
    /// Draw the map alone, for `view.snapshot` and `--snapshot`: no input,
    /// always the tile detail, no grid, outlines, highlight, ghosts or tool
    /// preview.
    pub map_only: bool,
    /// Draw entity boxes (`view.snapshot`'s `overlays`).
    pub draw_entities: bool,
}

impl Default for Viewport {
    fn default() -> Self {
        Self::new()
    }
}

impl Viewport {
    pub fn new() -> Self {
        Viewport {
            camera: Camera::default(),
            scene: None,
            visible: Vec::with_capacity(1024),
            line_buf: Vec::new(),
            wheel_acc: 0.0,
            pinch_acc: 0.0,
            gesture: None,
            note_press: None,
            readout: None,
            paint_strip: None,
            level_labels: Vec::new(),
            level_tab: None,
            context_at: None,
            clear_gpu: false,
            pending_fit: None,
            size_px: [0.0, 0.0],
            hover: None,
            tool_input: None,
            stats: ViewStats::default(),
            rebuild_budget: Some(Duration::from_millis(6)),
            map_only: false,
            draw_entities: true,
        }
    }

    pub fn scene(&self) -> Option<&Scene> {
        self.scene.as_ref()
    }

    /// Build the scene for a (re)loaded document and upload its tilesets.
    pub fn set_document(&mut self, doc: &Document, gpu: &Gpu) {
        self.scene = Some(Scene::new(doc));
        self.gesture = None;
        let mut r = gpu.renderer.write();
        if let Some(vg) = r.callback_resources.get_mut::<ViewportGpu>() {
            vg.clear_chunks();
            for i in 0..doc.project.tilesets.len() {
                upload_tileset(vg, gpu, doc, i);
            }
        }
    }

    /// Re-read one tileset's PNG (hot reload).
    pub fn reload_tileset(&mut self, doc: &Document, gpu: &Gpu, i: usize) {
        let mut r = gpu.renderer.write();
        if let Some(vg) = r.callback_resources.get_mut::<ViewportGpu>() {
            upload_tileset(vg, gpu, doc, i);
        }
    }

    pub fn invalidate(&mut self, doc: &Document, inv: &Invalidation) {
        let Some(scene) = &mut self.scene else {
            return;
        };
        match inv {
            Invalidation::Cells { level, layer, rect } => {
                scene.invalidate(*level, layer.as_str(), *rect)
            }
            Invalidation::Layer { level, layer } => scene.invalidate_layer(*level, layer.as_str()),
            Invalidation::Level(l) => scene.reset_level(doc, *l),
            // Textures are the app's part of `Tilesets` (it has the GPU).
            Invalidation::All | Invalidation::Tilesets => {
                *scene = Scene::new(doc);
                self.clear_gpu = true;
                // Level indices may have changed under a gesture.
                self.gesture = None;
            }
        }
    }

    /// A level's bake arrived.
    pub fn bake_arrived(&mut self, ix: LevelIx) {
        if let Some(s) = &mut self.scene {
            s.invalidate_auto(ix);
        }
    }

    /// Whether a left-button gesture (stroke, drag) is in progress: the
    /// live API waits until it ends.
    pub fn in_gesture(&self) -> bool {
        self.gesture.is_some() || self.note_press.is_some()
    }

    pub fn request_fit(&mut self, fit: Fit) {
        self.pending_fit = Some(fit);
    }

    /// Cancel the gesture in progress (tool switch, reload).
    pub fn cancel_gesture(&mut self, tool: Tool, events: &mut Vec<ToolEvent>) {
        self.note_press = None;
        if self.gesture.take().is_some() {
            events.push(ToolEvent::Cancel { tool });
        }
    }

    pub fn zoom_steps_centered(&mut self, steps: i32) {
        let c = [self.size_px[0] * 0.5, self.size_px[1] * 0.5];
        self.camera.zoom_steps(c, steps);
    }

    pub fn set_zoom_centered(&mut self, zoom: f64) {
        let c = [self.size_px[0] * 0.5, self.size_px[1] * 0.5];
        self.camera.zoom_at(c, zoom);
    }

    fn apply_fit(&mut self, doc: &Document, fit: Fit) {
        let rect = match fit {
            Fit::World => (0..doc.levels.len())
                .map(|i| level_wrect(doc, i))
                .fold(WRect::EMPTY, |a, b| a.union(&b)),
            Fit::Level(i) if i < doc.levels.len() => level_wrect(doc, i),
            Fit::Level(_) => return,
            Fit::Rect(r) => r,
        };
        self.camera.fit(rect, self.size_px, 24.0);
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, cx: &mut ViewportCx<'_>) {
        let (rect, resp) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        self.paint_strip = None;
        self.level_tab = None;
        self.level_labels.clear();
        let ppp = ui.ctx().pixels_per_point() as f64;
        let left = (rect.min.x as f64 * ppp).round();
        let top = (rect.min.y as f64 * ppp).round();
        let w = (rect.max.x as f64 * ppp).round() - left;
        let h = (rect.max.y as f64 * ppp).round() - top;
        self.size_px = [w.max(1.0), h.max(1.0)];
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, Color32::from_gray(24));

        let Some(doc) = cx.doc else {
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                "No project open. Ctrl-O to open one.",
                FontId::proportional(16.0),
                Color32::from_gray(140),
            );
            return;
        };
        if self.scene.is_none() {
            self.set_document(doc, cx.gpu);
        }
        if let Some(fit) = self.pending_fit.take() {
            self.apply_fit(doc, fit);
        }

        if self.map_only {
            self.map(ui, cx, rect, [left, top, w, h]);
            return;
        }

        // ---- input: pan and zoom ----
        let kb_free = !ui.ctx().egui_wants_keyboard_input();
        let space = kb_free && ui.input(|i| i.key_down(egui::Key::Space));
        let hand = space || cx.view.tool == Tool::Hand;
        let ppp32 = ppp as f32;
        if resp.dragged_by(egui::PointerButton::Middle)
            || resp.dragged_by(egui::PointerButton::Secondary)
            || (hand && resp.dragged_by(egui::PointerButton::Primary))
        {
            let d = resp.drag_delta() * ppp32;
            self.camera.pan_screen([d.x as f64, d.y as f64]);
        }
        let pointer_pts = ui.input(|i| i.pointer.latest_pos());
        let to_px = |p: Pos2| [p.x as f64 * ppp - left, p.y as f64 * ppp - top];
        if resp.hovered()
            && let Some(p) = pointer_pts
        {
            let (wheel, pinch) = ui.input(|i| {
                let mut wheel = 0.0f32;
                let mut pinch = 0.0f32;
                for e in &i.events {
                    match e {
                        egui::Event::MouseWheel { unit, delta, .. } => {
                            wheel += match unit {
                                egui::MouseWheelUnit::Line => delta.y,
                                egui::MouseWheelUnit::Point => delta.y / 50.0,
                                egui::MouseWheelUnit::Page => delta.y * 3.0,
                            }
                        }
                        egui::Event::Zoom(z) => pinch += z.ln(),
                        _ => {}
                    }
                }
                (wheel, pinch)
            });
            self.wheel_acc += wheel;
            self.pinch_acc += pinch;
            let mut steps = 0;
            while self.wheel_acc >= 1.0 {
                self.wheel_acc -= 1.0;
                steps += 1;
            }
            while self.wheel_acc <= -1.0 {
                self.wheel_acc += 1.0;
                steps -= 1;
            }
            let notch = 1.25f32.ln();
            while self.pinch_acc >= notch {
                self.pinch_acc -= notch;
                steps += 1;
            }
            while self.pinch_acc <= -notch {
                self.pinch_acc += notch;
                steps -= 1;
            }
            if steps != 0 {
                self.camera.zoom_steps(to_px(p), steps);
            }
        }

        let cam = self.camera;
        cx.view.zoom = cam.zoom;
        let view = cam.visible(self.size_px);
        let grid = doc.project.grid_size as f64;
        let detail = cam.zoom * grid >= DETAIL_CELL_PX;
        let to_screen = |wp: [f64; 2]| -> Pos2 {
            let s = cam.world_to_screen_snapped(wp);
            pos2(((left + s[0]) / ppp) as f32, ((top + s[1]) / ppp) as f32)
        };
        let to_srect = |r: &WRect| Rect::from_min_max(to_screen(r.min), to_screen(r.max));
        // Hit-test with the same snapped transform the tiles and overlays use,
        // so the cell under the cursor is the cell drawn there.
        let pointer_world = pointer_pts.map(|p| cam.screen_to_world_snapped(to_px(p)));

        // ---- levels: background fills and names ----
        let scene = self.scene.as_mut().expect("scene set above");
        let project_bg = doc
            .project
            .bg_color
            .as_ref()
            .map_or([0x40, 0x40, 0x48, 255], |c| parse_color(c.as_str()));
        for &ix in &scene.draw_order {
            let lr = scene.levels[ix].rect;
            if !lr.intersects(&view) {
                continue;
            }
            let level = doc.level(ix);
            let bg = level
                .bg_color
                .as_ref()
                .map_or(project_bg, |c| parse_color(c.as_str()));
            let mut fill = Color32::from_rgb(bg[0], bg[1], bg[2]);
            if !detail {
                // Flat mode: a bit lighter so small levels read on the dark bg.
                fill = fill.lerp_to_gamma(Color32::from_gray(120), 0.25);
            }
            painter.rect_filled(to_srect(&lr), 0.0, fill);
        }

        // ---- tiles via the GPU ----
        let mut stats = ViewStats {
            detail,
            ..Default::default()
        };
        if detail {
            let vs: &ViewState = cx.view;
            let cs = scene.collect_visible(
                doc,
                &view,
                |l| vs.layer_opacity(doc, l) > 0.0,
                self.rebuild_budget,
                &mut self.visible,
            );
            stats.rebuilt = cs.rebuilt;
            stats.rebuild_ms = cs.rebuild_time.as_secs_f64() * 1e3;
            stats.pending = cs.pending;
            stats.chunks = self.visible.len();
            if cs.pending > 0 {
                ui.ctx().request_repaint();
            }
            let mut r = cx.gpu.renderer.write();
            if let Some(vg) = r.callback_resources.get_mut::<ViewportGpu>() {
                if std::mem::take(&mut self.clear_gpu) {
                    vg.clear_chunks();
                    scene.retired.clear();
                }
                for id in scene.retired.drain(..) {
                    vg.free_chunk(id);
                }
                vg.draws.clear();
                for &cr in &self.visible {
                    let c = scene.chunk(cr);
                    vg.sync_chunk(&cx.gpu.device, &cx.gpu.queue, c);
                    stats.instances += c.instances.len();
                    for run in &c.runs {
                        vg.draws.push(DrawCmd {
                            chunk: c.id,
                            tex: run.tex,
                            start: run.start,
                            end: run.end,
                        });
                    }
                }
                stats.draws = vg.draws.len();
                stats.uploaded = vg.take_upload_count();
                vg.viewport_px = [left as f32, top as f32, w as f32, h as f32];
                let off = cam.snapped_offset();
                let mut u = Uniforms {
                    view: [off[0] as f32, off[1] as f32, cam.zoom as f32, 0.0],
                    size: [w as f32, h as f32, 0.0, 0.0],
                    opacity: [[1.0; 4]; gpu::MAX_LAYERS / 4],
                };
                for i in 0..doc.project.layers.len() {
                    u.set_opacity(i, cx.view.layer_opacity(doc, i));
                }
                vg.write_uniforms(&cx.gpu.queue, &u);
            }
            drop(r);
            painter.add(egui_wgpu::Callback::new_paint_callback(
                rect,
                ViewportCallback,
            ));
            if self.draw_entities {
                draw_entities(
                    &painter, doc, scene, cx.view, &view, cam.zoom, &to_screen, cx.accent, cx.atlas,
                );
            }
        } else {
            if self.draw_entities {
                draw_world_labels(&painter, doc, scene, cx.view, &view, &to_screen);
            }
            for &ix in &scene.draw_order {
                let lr = scene.levels[ix].rect;
                if !lr.intersects(&view) {
                    continue;
                }
                let sr = to_srect(&lr);
                let level = doc.level(ix);
                let name = if level.name.is_empty() {
                    level.slug.as_str()
                } else {
                    level.name.as_str()
                };
                if sr.width() > 7.0 * name.len().min(24) as f32 && sr.height() > 14.0 {
                    painter.with_clip_rect(sr.intersect(rect)).text(
                        sr.center(),
                        Align2::CENTER_CENTER,
                        name,
                        FontId::proportional(12.0),
                        Color32::from_gray(230),
                    );
                }
            }
        }
        self.stats = stats;
        notes::draw_annotations(
            &painter,
            &notes::NotesFrame {
                doc,
                view: cx.view,
                visible: &view,
                zoom: cam.zoom,
                to_screen: &to_screen,
                accent: cx.accent,
                snapshot: false,
            },
        );

        // ---- overlays ----
        let active = cx.view.active_level.filter(|&i| i < doc.levels.len());
        let active_layer = cx
            .view
            .active_layer
            .filter(|&i| i < doc.project.layers.len());
        if let (Some(ix), true) = (active, cx.view.show_grid) {
            let g = active_layer.map_or(grid, |l| {
                doc.project.layer_grid(&doc.project.layers[l]) as f64
            });
            if cam.zoom * g >= GRID_CELL_PX {
                draw_grid(&painter, &level_wrect(doc, ix), &view, g, &to_screen);
            }
        }
        let outline = Stroke::new(1.0, Color32::from_black_alpha(160));
        for &ix in &scene.draw_order {
            let lr = scene.levels[ix].rect;
            if lr.intersects(&view) && Some(ix) != active {
                painter.rect_stroke(to_srect(&lr), 0.0, outline, StrokeKind::Outside);
            }
        }
        if let Some(ix) = active {
            let lr = scene.levels[ix].rect;
            painter.rect_stroke(
                to_srect(&lr),
                0.0,
                Stroke::new(2.0, cx.accent),
                StrokeKind::Outside,
            );
        }
        // Selected levels: a thick accent rect; selected tile stacks: their
        // cell outlined.
        for id in cx.view.selection.levels() {
            if let Some(ix) = doc.level_ix_by_id(id) {
                painter.rect_stroke(
                    to_srect(&level_wrect(doc, ix)),
                    0.0,
                    Stroke::new(4.0, cx.accent),
                    StrokeKind::Outside,
                );
            }
        }
        for (lid, layer, cell) in cx.view.selection.tiles() {
            let (Some(ix), Some(def)) =
                (doc.level_ix_by_id(lid), doc.project.layer(layer.as_str()))
            else {
                continue;
            };
            let g = doc.project.layer_grid(def) as f64;
            let o = doc.level(ix).world_px;
            let r = WRect::from_pos_size(
                [
                    o[0] as f64 + cell[0] as f64 * g,
                    o[1] as f64 + cell[1] as f64 * g,
                ],
                [g, g],
            );
            if r.intersects(&view) {
                painter.rect_stroke(
                    to_srect(&r),
                    0.0,
                    Stroke::new(2.0, cx.accent),
                    StrokeKind::Inside,
                );
            }
        }

        if let Some(h) = cx.highlight.filter(|h| h.level < doc.levels.len())
            && detail
            && let Some(def) = doc.project.layer(h.layer.as_str())
        {
            let lr = level_wrect(doc, h.level);
            let g = doc.project.layer_grid(def) as f64;
            crate::app::rules::draw_highlight(&painter, h, cx.accent, |c| {
                to_srect(&WRect::from_pos_size(
                    [lr.min[0] + c[0] as f64 * g, lr.min[1] + c[1] as f64 * g],
                    [g, g],
                ))
            });
        }
        for g in cx.ghosts {
            g.draw(&painter, &to_screen, cx.accent);
        }
        if doc.levels.is_empty() && cx.view.drawing.is_none() {
            empty_map_hint(&painter, rect, doc.project.layers.is_empty(), cx.accent);
        }
        self.readout = None;
        if let Some(d) = &cx.view.drawing {
            let ctrl = crate::tools::pixel_mod(ui.input(|i| i.modifiers));
            let pointer = pointer_world
                .filter(|_| resp.hovered())
                .map(|wp| crate::annot::snap_point(doc, wp, ctrl));
            self.readout = draw_drawing(&painter, doc, d, pointer, &to_screen, cx.accent);
        }

        // ---- hover and tool gestures ----
        let level_under = pointer_world.and_then(|wp| {
            scene
                .draw_order
                .iter()
                .rev()
                .copied()
                .find(|&i| scene.levels[i].rect.contains(wp))
        });
        let hit_in = |ix: LevelIx, layer: usize, wp: [f64; 2]| -> CellHit {
            if ix >= doc.levels.len() {
                // A world gesture with no level at all ([`NO_LEVEL`]).
                return CellHit {
                    level: ix,
                    layer,
                    cell: [0, 0],
                    inside: false,
                    world_px: wp,
                };
            }
            let (g, cells) = crate::tools::hit_grid(doc, layer, ix);
            let g = g as f64;
            let lr = level_wrect(doc, ix);
            let cell = [
                ((wp[0] - lr.min[0]) / g).floor() as i32,
                ((wp[1] - lr.min[1]) / g).floor() as i32,
            ];
            CellHit {
                level: ix,
                layer,
                cell,
                inside: cell[0] >= 0
                    && cell[1] >= 0
                    && (cell[0] as u32) < cells[0]
                    && (cell[1] as u32) < cells[1],
                world_px: wp,
            }
        };
        let modifiers = ui.input(|i| i.modifiers);
        let tool = cx.view.tool;
        // The entity tool works on the active layer if it is an entities
        // layer, else on the first one.
        let entity_layer = if tool == Tool::Entity {
            active_layer
                .filter(|&l| doc.project.layers[l].is_entities())
                .or_else(|| crate::view::first_entities_layer(doc))
        } else {
            None
        };
        let tool_layer = if tool == Tool::Entity {
            entity_layer
        } else {
            active_layer
        };
        // The Select tool's cells: the active layer's, else the first
        // layer's; with no layer at all the project grid's, so levels are
        // still clicked, moved and resized ([`crate::tools::NO_LAYER`]).
        let hit_layer = active_layer
            .or_else(|| (!doc.project.layers.is_empty()).then_some(0))
            .unwrap_or(crate::tools::NO_LAYER);
        // Why the active tool can't paint on the active layer, if it can't.
        let refusal = if tool.paints() {
            active_layer.and_then(|l| {
                crate::edit::refusal(doc, cx.view, l).or_else(|| {
                    let def = &doc.project.layers[l];
                    match &def.kind {
                        LayerKind::Auto(a) => Some(format!(
                            "{} is an auto layer: paint its source layer {}",
                            def.name, a.source
                        )),
                        LayerKind::Entities => Some(format!(
                            "{} is an entities layer: place entities with the Entity tool (T)",
                            def.name
                        )),
                        _ => None,
                    }
                })
            })
        } else if tool == Tool::Entity {
            match entity_layer {
                Some(l) => crate::edit::refusal(doc, cx.view, l),
                None => Some("The project has no entities layer".into()),
            }
        } else {
            None
        };
        // The active level's name tab: drawn at every zoom; with Select,
        // dragging it moves the level (a drag on the active level's body is
        // the marquee).
        // The tool's strip at the top left of the map (Note options, Paint
        // modes) floats above the map: the tab steps aside from it.
        let strip = tool_strip_rect(ui.ctx(), rect, tool);
        let tab = active.map(|ix| {
            let l = doc.level(ix);
            let name = if l.name.is_empty() {
                l.slug.as_str()
            } else {
                l.name.as_str()
            };
            (
                level_tab(to_srect(&scene.levels[ix].rect), name, rect, strip),
                name.to_string(),
            )
        });
        let on_tab = tool == Tool::Select
            && !hand
            && tab
                .as_ref()
                .zip(pointer_pts)
                .is_some_and(|((r, _), p)| r.contains(p));
        // Resize handles of the active level (Select).
        let handle = match (tool, active, pointer_pts) {
            (Tool::Select, Some(ix), Some(p)) if !hand && !on_tab => {
                handle_at(to_srect(&scene.levels[ix].rect), p)
            }
            _ => None,
        };
        let (pressed, down, released) = ui.input(|i| {
            (
                i.pointer.primary_pressed(),
                i.pointer.primary_down(),
                i.pointer.primary_released(),
            )
        });
        let escape = kb_free && ui.input(|i| i.key_pressed(egui::Key::Escape));
        if cx.view.pick.is_some() && self.gesture.is_none() {
            // A canvas pick armed by an editor takes the next click, whatever
            // the tool; Escape cancels it.
            if escape {
                cx.view.pick = None;
                cx.view.set_status("Pick cancelled", false);
            } else if resp.hovered() && pressed && !hand {
                pick_at(doc, cx.view, level_under, pointer_world, cam.zoom);
            }
        } else if escape && self.gesture.is_none() && cx.view.drawing.is_some() {
            // Escape cancels the drawing (or measurement) first.
            crate::annot::cancel_drawing(cx.view);
            self.note_press = None;
        } else if matches!(tool, Tool::Note | Tool::Measure) && !hand {
            // The Note and Measure tools: a click adds (a note, a pin, a
            // vertex), a drag over 4 screen px with Region is a rect; a
            // double click finishes a path or polygon.
            if resp.hovered()
                && pressed
                && self.note_press.is_none()
                && let (Some(wp), Some(p)) = (pointer_world, pointer_pts)
            {
                self.note_press = Some((wp, p, level_under));
            }
            if let Some((from, p0, under)) = self.note_press
                && (released || !down)
            {
                self.note_press = None;
                let to = pointer_world.unwrap_or(from);
                let moved = pointer_pts.map_or(0.0, |p| (p - p0).length() as f64);
                let ctrl = crate::tools::pixel_mod(modifiers);
                let cmd = if moved > crate::annot::DRAG_PX {
                    crate::annot::tool_drag(doc, cx.view, from, to, under, ctrl)
                } else {
                    crate::annot::tool_click(doc, cx.view, from, under, ctrl)
                };
                if let Some(c) = cmd {
                    cx.actions.push(Action::Apply(Box::new(c)));
                }
                if resp.double_clicked() {
                    cx.actions.push(Action::FinishDrawing);
                }
            }
        } else if self.gesture.is_none() && resp.hovered() && pressed && !hand {
            let begin = |at: CellHit, grab: Grab, events: &mut Vec<ToolEvent>| {
                events.push(ToolEvent::Begin {
                    tool,
                    at,
                    modifiers,
                    grab,
                });
                (at, at, grab)
            };
            let add = crate::tools::pixel_mod(modifiers);
            if tool == Tool::Select {
                if let Some(wp) = pointer_world {
                    let layer = hit_layer;
                    self.gesture = select_press(
                        doc,
                        cx.view,
                        SelectPress {
                            active,
                            on_tab,
                            handle,
                            level_under,
                            wp,
                            add,
                            zoom: cam.zoom,
                        },
                    )
                    .map(|(ix, grab)| begin(hit_in(ix, layer, wp), grab, cx.events));
                }
            } else if let Some(ix) = level_under {
                cx.view.active_level = Some(ix);
                if tool == Tool::Entity {
                    match (&refusal, entity_layer, pointer_world) {
                        (Some(why), _, _) if !modifiers.alt => {
                            cx.view.set_status(why.clone(), true)
                        }
                        (_, Some(layer), Some(wp)) => {
                            cx.view.active_layer = Some(layer);
                            self.gesture =
                                Some(begin(hit_in(ix, layer, wp), Grab::None, cx.events));
                        }
                        _ => {}
                    }
                } else if tool.paints() {
                    match (&refusal, active_layer, pointer_world) {
                        (Some(why), _, _) if !modifiers.alt => {
                            cx.view.set_status(why.clone(), true)
                        }
                        (_, Some(layer), Some(wp)) => {
                            self.gesture =
                                Some(begin(hit_in(ix, layer, wp), Grab::None, cx.events));
                        }
                        _ => {}
                    }
                }
            }
        } else if let Some((start, last, grab)) = self.gesture {
            let at = pointer_world.map_or(last, |wp| hit_in(start.level, start.layer, wp));
            if escape {
                self.gesture = None;
                cx.events.push(ToolEvent::Cancel { tool });
            } else if released || !down {
                cx.events.push(ToolEvent::End {
                    tool,
                    start,
                    at,
                    modifiers,
                });
                self.gesture = None;
            } else if at.cell != last.cell
                || (matches!(tool, Tool::Select | Tool::Entity) && at.world_px != last.world_px)
            {
                cx.events.push(ToolEvent::Move {
                    tool,
                    start,
                    at,
                    modifiers,
                });
                self.gesture = Some((start, at, grab));
            }
        }
        if tool == Tool::Select
            && !hand
            && resp.double_clicked()
            && let Some(ix) = level_under
        {
            self.pending_fit = Some(Fit::Level(ix));
        }

        let hover_ix = active
            .filter(|&a| pointer_world.is_some_and(|wp| level_wrect(doc, a).contains(wp)))
            .or(level_under);
        let hover_hit = match (hover_ix, tool_layer, pointer_world) {
            (Some(ix), Some(l), Some(wp)) if resp.hovered() || self.gesture.is_some() => {
                Some(hit_in(ix, l, wp))
            }
            _ => None,
        };
        self.tool_input = Some(ToolInput {
            tool,
            grab: self.gesture.map_or(Grab::None, |g| g.2),
            hover: hover_hit,
            drag: self.gesture.map(|(a, b, _)| (a, b)),
            modifiers,
        });
        self.hover = pointer_world
            .filter(|_| resp.hovered())
            .map(|wp| HoverInfo {
                world_px: wp,
                level: hover_ix.map(|i| doc.level(i).slug.to_string()),
                cell: hover_hit.map(|h| h.cell),
                what: hover_hit.map(|h| describe_cell(doc, h)).unwrap_or_default(),
            });

        // Tool preview.
        if tool.paints() && detail {
            let color = if tool == Tool::Eraser {
                Color32::from_rgb(230, 80, 80)
            } else {
                cx.accent
            };
            let stroke = Stroke::new(1.5, color);
            let cell_srect = |h: &CellHit, c: [i32; 2], span: [i32; 2]| -> Rect {
                let lr = level_wrect(doc, h.level);
                let g = doc.project.layer_grid(&doc.project.layers[h.layer]) as f64;
                let min = [lr.min[0] + c[0] as f64 * g, lr.min[1] + c[1] as f64 * g];
                to_srect(&WRect::from_pos_size(
                    min,
                    [span[0] as f64 * g, span[1] as f64 * g],
                ))
            };
            let footprint = |h: &CellHit| brush_span(doc, cx.view, h.layer, tool);
            match (self.gesture, hover_hit) {
                (Some((start, at, _)), _) if tool == Tool::Rect => {
                    let r = cell_rect(start.cell, at.cell);
                    let outer = cell_srect(&start, [r[0], r[1]], [r[2] - r[0], r[3] - r[1]]);
                    painter.rect_stroke(outer, 0.0, stroke, StrokeKind::Inside);
                    if modifiers.shift {
                        // Outline: show the hollow inside.
                        if r[2] - r[0] > 2 && r[3] - r[1] > 2 {
                            let inner = cell_srect(
                                &start,
                                [r[0] + 1, r[1] + 1],
                                [r[2] - r[0] - 2, r[3] - r[1] - 2],
                            );
                            painter.rect_stroke(inner, 0.0, stroke, StrokeKind::Outside);
                        }
                    } else {
                        painter.rect_filled(outer, 0.0, color.gamma_multiply(0.25));
                    }
                }
                (Some((start, at, _)), _) if tool == Tool::Line => {
                    line_cells(start.cell, at.cell, &mut self.line_buf);
                    for &c in &self.line_buf {
                        painter.rect_filled(
                            cell_srect(&start, c, [1, 1]),
                            0.0,
                            color.gamma_multiply(0.35),
                        );
                    }
                    painter.rect_stroke(
                        cell_srect(&start, at.cell, [1, 1]),
                        0.0,
                        stroke,
                        StrokeKind::Inside,
                    );
                }
                (Some((_, at, _)), _) | (None, Some(at)) => {
                    let span = footprint(&at);
                    let r = cell_srect(&at, at.cell, span);
                    if tool == Tool::Fill {
                        painter.rect_filled(r, 0.0, color.gamma_multiply(0.25));
                    }
                    painter.rect_stroke(r, 0.0, stroke, StrokeKind::Inside);
                }
                _ => {}
            }
        }

        // Entity tool: the ghost of what a release would place.
        if tool == Tool::Entity
            && detail
            && let Some(brush) = &cx.view.entities.brush
            && let Some(def) = doc.project.entity_def(brush.ty.as_str())
        {
            let pair = match (self.gesture, hover_hit) {
                (Some((start, at, _)), _) => Some((start, at)),
                (None, Some(h)) if !modifiers.alt => Some((h, h)),
                _ => None,
            };
            if let Some((start, at)) = pair {
                let p = crate::tools::entity_placement(doc, def, &start, &at, modifiers);
                let g = doc.project.layer_grid(&doc.project.layers[at.layer]);
                let [x, y, w, h] = crate::tools::placement_box(def, &p, g, doc.project.grid_size);
                let lr = level_wrect(doc, at.level);
                let sr = to_srect(&WRect::from_pos_size(
                    [lr.min[0] + x, lr.min[1] + y],
                    [w, h],
                ));
                let c = parse_color(def.color.as_str());
                let col = Color32::from_rgb(c[0], c[1], c[2]);
                painter.rect_filled(sr, 1.0, col.gamma_multiply(0.35));
                painter.rect_stroke(sr, 1.0, Stroke::new(1.5, cx.accent), StrokeKind::Inside);
                if let Some(size) = p.size {
                    painter.text(
                        sr.center_bottom() + egui::vec2(0.0, 4.0),
                        Align2::CENTER_TOP,
                        format!("{} x {}", size[0], size[1]),
                        FontId::proportional(12.0),
                        Color32::WHITE,
                    );
                }
            }
        }

        // Cursor.
        if resp.hovered() || self.gesture.is_some() {
            let icon = if resp.dragged_by(egui::PointerButton::Middle)
                || resp.dragged_by(egui::PointerButton::Secondary)
                || (hand && resp.dragged())
            {
                CursorIcon::Grabbing
            } else if hand {
                CursorIcon::Grab
            } else if (tool.paints() || tool == Tool::Entity) && refusal.is_some() && !modifiers.alt
            {
                CursorIcon::NotAllowed
            } else if tool.paints()
                || matches!(tool, Tool::Entity | Tool::Note | Tool::Measure)
                || cx.view.pick.is_some()
            {
                CursorIcon::Crosshair
            } else if let Some(Grab::Handle(e)) = self.gesture.map(|g| g.2) {
                handle_cursor(e)
            } else if let Some(e) = handle {
                handle_cursor(e)
            } else if matches!(
                self.gesture,
                Some((_, _, Grab::Level | Grab::Selection { .. }))
            ) {
                CursorIcon::Grabbing
            } else if on_tab {
                CursorIcon::Grab
            } else {
                CursorIcon::Default
            };
            ui.ctx().set_cursor_icon(icon);
        }

        // Select: the selection dragged (where the release puts it), the
        // marquee.
        match self.gesture {
            Some((start, at, Grab::Selection { .. })) => {
                let delta = [
                    at.world_px[0] - start.world_px[0],
                    at.world_px[1] - start.world_px[1],
                ];
                draw_dragged_selection(
                    &painter,
                    doc,
                    cx.view,
                    delta,
                    crate::tools::pixel_mod(modifiers),
                    &to_screen,
                    cx.accent,
                );
            }
            Some((start, at, Grab::Marquee | Grab::WorldMarquee)) => {
                let r = Rect::from_two_pos(to_screen(start.world_px), to_screen(at.world_px));
                painter.rect_filled(r, 0.0, cx.accent.gamma_multiply(0.12));
                painter.rect_stroke(r, 0.0, Stroke::new(1.0, cx.accent), StrokeKind::Inside);
            }
            _ => {}
        }

        // The other levels' names (with tiles drawn; the flat view centres
        // every name): a quiet label in each one's top-left corner where it
        // fits inside the level and clears the active tab, the tool strip
        // and the labels already placed.
        self.level_labels.clear();
        self.level_tab = tab.as_ref().map(|(r, _)| *r);
        if detail {
            let mut taken: Vec<Rect> = tab.iter().map(|(r, _)| *r).chain(strip).collect();
            for &ix in &scene.draw_order {
                if Some(ix) == active {
                    continue;
                }
                let lr = scene.levels[ix].rect;
                if !lr.intersects(&view) {
                    continue;
                }
                let sr = to_srect(&lr).intersect(rect);
                let level = doc.level(ix);
                let name = if level.name.is_empty() {
                    level.slug.as_str()
                } else {
                    level.name.as_str()
                };
                let galley = painter.layout_no_wrap(
                    name.to_string(),
                    FontId::proportional(12.0),
                    Color32::from_gray(220),
                );
                let r = Rect::from_min_size(
                    sr.min + egui::vec2(3.0, 3.0),
                    galley.size() + egui::vec2(12.0, 4.0),
                );
                if !sr.shrink(2.0).contains_rect(r) || taken.iter().any(|t| t.intersects(r)) {
                    continue;
                }
                painter.rect_filled(r, 3.0, Color32::from_rgba_unmultiplied(20, 22, 30, 190));
                painter.galley(
                    r.min + egui::vec2(6.0, 2.0),
                    galley,
                    Color32::from_gray(220),
                );
                taken.push(r);
                self.level_labels.push((ix, r));
            }
        }

        // The active level's name tab (every tool, every zoom).
        if let Some((r, name)) = &tab {
            let hot = on_tab || matches!(self.gesture, Some((_, _, Grab::Level)));
            painter.rect_filled(
                *r,
                3.0,
                if hot {
                    cx.accent.gamma_multiply(0.55)
                } else {
                    Color32::from_rgba_unmultiplied(20, 22, 30, 230)
                },
            );
            painter.rect_stroke(*r, 3.0, Stroke::new(1.0, cx.accent), StrokeKind::Inside);
            painter.with_clip_rect(r.shrink(2.0)).text(
                r.left_center() + egui::vec2(7.0, 0.0),
                Align2::LEFT_CENTER,
                name,
                FontId::proportional(12.0),
                Color32::WHITE,
            );
        }

        // Select: resize handles on the active level, and the resize preview.
        if tool == Tool::Select
            && let Some(ix) = active
        {
            let lr = scene.levels[ix].rect;
            let sr = match self.gesture {
                Some((start, at, Grab::Handle(edges))) => {
                    let l = doc.level(ix);
                    let (px, size) = resize_level(
                        l.world_px,
                        l.size,
                        edges,
                        [
                            at.world_px[0] - start.world_px[0],
                            at.world_px[1] - start.world_px[1],
                        ],
                        doc.project.grid_size,
                    );
                    let r = to_srect(&WRect::from_pos_size(
                        [px[0] as f64, px[1] as f64],
                        [size[0] as f64 * grid, size[1] as f64 * grid],
                    ));
                    painter.rect_stroke(r, 0.0, Stroke::new(1.5, cx.accent), StrokeKind::Outside);
                    painter.text(
                        r.center_bottom() + egui::vec2(0.0, 4.0),
                        Align2::CENTER_TOP,
                        format!("{} x {}", size[0], size[1]),
                        FontId::proportional(12.0),
                        Color32::WHITE,
                    );
                    r
                }
                // A level drag only previews; the move is one command on release.
                Some((start, at, Grab::Level)) => {
                    let l = doc.level(ix);
                    let (d, snaps) = crate::annot::drag_delta(
                        doc,
                        cx.view,
                        &[ix],
                        [
                            at.world_px[0] - start.world_px[0],
                            at.world_px[1] - start.world_px[1],
                        ],
                        false,
                    );
                    let px = [l.world_px[0] + d[0], l.world_px[1] + d[1]];
                    draw_snaps(&painter, &snaps, &view, &to_screen);
                    let r = to_srect(&WRect::from_pos_size(
                        [px[0] as f64, px[1] as f64],
                        [lr.max[0] - lr.min[0], lr.max[1] - lr.min[1]],
                    ));
                    if px != l.world_px {
                        painter.rect_filled(r, 0.0, cx.accent.gamma_multiply(0.15));
                        painter.rect_stroke(
                            r,
                            0.0,
                            Stroke::new(1.5, cx.accent),
                            StrokeKind::Outside,
                        );
                        painter.text(
                            r.center_bottom() + egui::vec2(0.0, 4.0),
                            Align2::CENTER_TOP,
                            format!("{}, {}", px[0], px[1]),
                            FontId::proportional(12.0),
                            Color32::WHITE,
                        );
                    }
                    r
                }
                _ => to_srect(&lr),
            };
            if sr.width() >= 24.0 && sr.height() >= 24.0 {
                for (_, p) in handle_points(sr) {
                    let h = Rect::from_center_size(p, egui::vec2(7.0, 7.0));
                    painter.rect_filled(h, 0.0, Color32::from_gray(20));
                    painter.rect_stroke(h, 0.0, Stroke::new(1.0, cx.accent), StrokeKind::Inside);
                }
            }
        }

        // The Note tool's options strip, top left of the map; in its place
        // with a Paint mode, the Paint group's mode strip.
        if tool == Tool::Note {
            note_strip(ui.ctx(), rect, cx.view);
        }
        self.paint_strip = tool
            .is_paint_mode()
            .then(|| paint_strip(ui.ctx(), rect, tool, cx.actions));

        // Context menu (a right click; right drag pans).
        if resp.secondary_clicked() {
            self.context_at = pointer_world.map(|wp| (wp, level_under));
        }
        let context_at = self.context_at;
        resp.context_menu(|ui| {
            let Some((wp, lvl)) = context_at else {
                ui.close();
                return;
            };
            if ui.button("New level here…").clicked() {
                cx.actions.push(Action::NewLevel(Some(wp)));
                ui.close();
            }
            if let Some(ix) = lvl.filter(|&i| i < doc.levels.len()) {
                let slug = doc.level(ix).slug.as_str();
                ui.separator();
                if ui.button(format!("Frame {slug}")).clicked() {
                    cx.view.active_level = Some(ix);
                    cx.actions.push(Action::FitLevel);
                    ui.close();
                }
                if ui.button(format!("Delete {slug}…")).clicked() {
                    cx.view.active_level = Some(ix);
                    cx.actions.push(Action::DeleteLevel);
                    ui.close();
                }
            }
        });
    }
}

impl Viewport {
    /// The map alone ([`Viewport::map_only`]): level backgrounds, every
    /// visible layer's chunks at the camera's zoom, entity boxes if
    /// [`Viewport::draw_entities`]. `px` is the viewport in physical px
    /// (left, top, width, height).
    fn map(&mut self, ui: &mut egui::Ui, cx: &mut ViewportCx<'_>, rect: Rect, px: [f64; 4]) {
        let Some(doc) = cx.doc else {
            return;
        };
        let [left, top, w, h] = px;
        let ppp = ui.ctx().pixels_per_point() as f64;
        let painter = ui.painter_at(rect);
        let cam = self.camera;
        let view = cam.visible(self.size_px);
        let to_screen = |wp: [f64; 2]| -> Pos2 {
            let s = cam.world_to_screen_snapped(wp);
            pos2(((left + s[0]) / ppp) as f32, ((top + s[1]) / ppp) as f32)
        };
        let to_srect = |r: &WRect| Rect::from_min_max(to_screen(r.min), to_screen(r.max));
        let scene = self.scene.as_mut().expect("scene set before map()");
        let project_bg = doc
            .project
            .bg_color
            .as_ref()
            .map_or([0x40, 0x40, 0x48, 255], |c| parse_color(c.as_str()));
        for &ix in &scene.draw_order {
            let lr = scene.levels[ix].rect;
            if !lr.intersects(&view) {
                continue;
            }
            let bg = doc
                .level(ix)
                .bg_color
                .as_ref()
                .map_or(project_bg, |c| parse_color(c.as_str()));
            painter.rect_filled(to_srect(&lr), 0.0, Color32::from_rgb(bg[0], bg[1], bg[2]));
        }
        let vs: &ViewState = cx.view;
        scene.collect_visible(
            doc,
            &view,
            |l| vs.layer_opacity(doc, l) > 0.0,
            None,
            &mut self.visible,
        );
        let mut r = cx.gpu.renderer.write();
        if let Some(vg) = r.callback_resources.get_mut::<ViewportGpu>() {
            if std::mem::take(&mut self.clear_gpu) {
                vg.clear_chunks();
                scene.retired.clear();
            }
            for id in scene.retired.drain(..) {
                vg.free_chunk(id);
            }
            vg.draws.clear();
            for &cr in &self.visible {
                let c = scene.chunk(cr);
                vg.sync_chunk(&cx.gpu.device, &cx.gpu.queue, c);
                for run in &c.runs {
                    vg.draws.push(DrawCmd {
                        chunk: c.id,
                        tex: run.tex,
                        start: run.start,
                        end: run.end,
                    });
                }
            }
            vg.viewport_px = [left as f32, top as f32, w as f32, h as f32];
            let off = cam.snapped_offset();
            let mut u = Uniforms {
                view: [off[0] as f32, off[1] as f32, cam.zoom as f32, 0.0],
                size: [w as f32, h as f32, 0.0, 0.0],
                opacity: [[1.0; 4]; gpu::MAX_LAYERS / 4],
            };
            for i in 0..doc.project.layers.len() {
                u.set_opacity(i, cx.view.layer_opacity(doc, i));
            }
            vg.write_uniforms(&cx.gpu.queue, &u);
        }
        drop(r);
        painter.add(egui_wgpu::Callback::new_paint_callback(
            rect,
            ViewportCallback,
        ));
        if self.draw_entities {
            draw_entities(
                &painter, doc, scene, cx.view, &view, cam.zoom, &to_screen, cx.accent, cx.atlas,
            );
            // Snapshots with overlays draw annotations too: open notes, every
            // guide, no tag filter (a snapshot is the data, not the view).
            notes::draw_annotations(
                &painter,
                &notes::NotesFrame {
                    doc,
                    view: cx.view,
                    visible: &view,
                    zoom: cam.zoom,
                    to_screen: &to_screen,
                    accent: cx.accent,
                    snapshot: true,
                },
            );
        }
    }
}

/// Where a Select drag of the selection by `delta` world pixels puts
/// everything, drawn from the very commands the release applies
/// ([`crate::annot::plan_move`]): entity boxes, tile cells, annotation
/// outlines and level rects in the accent, plus the guide it snapped to.
fn draw_dragged_selection(
    painter: &egui::Painter,
    doc: &Document,
    view: &ViewState,
    delta: [f64; 2],
    pixel: bool,
    to_screen: &dyn Fn([f64; 2]) -> Pos2,
    accent: Color32,
) {
    let to_srect = |r: &WRect| Rect::from_min_max(to_screen(r.min), to_screen(r.max));
    let plan = match crate::annot::plan_move(doc, view, delta, pixel) {
        Ok(p) => p,
        Err(e) => {
            // Refused (tiles leaving a level): say so where the pointer is.
            let c = view
                .selection
                .tiles()
                .next()
                .and_then(|(l, _, _)| doc.level_ix_by_id(l))
                .map(|ix| level_wrect(doc, ix).center());
            if let Some(c) = c {
                painter.text(
                    to_screen(c),
                    Align2::CENTER_CENTER,
                    e,
                    FontId::proportional(12.0),
                    Color32::from_rgb(230, 120, 120),
                );
            }
            return;
        }
    };
    let pg = doc.project.grid_size;
    let stroke = Stroke::new(1.5, accent);
    let mut lines = egui::Mesh::default();
    for c in &plan.commands {
        match c {
            worlds_core::Command::MoveEntity {
                id,
                cell,
                offset_px,
                ..
            } => {
                let Some((ix, layer, e)) = doc.find_entity(id) else {
                    continue;
                };
                let (Some(def), Some(ldef)) = (
                    doc.project.entity_def(e.ty.as_str()),
                    doc.project.layer(layer.as_str()),
                ) else {
                    continue;
                };
                let moved = worlds_core::EntityInstance {
                    cell: *cell,
                    offset_px: *offset_px,
                    ..e.clone()
                };
                let r = entity_rect_of(def, &moved, doc.project.layer_grid(ldef), pg);
                let lr = level_wrect(doc, ix);
                let sr = to_srect(&WRect::new(
                    [lr.min[0] + r.min[0], lr.min[1] + r.min[1]],
                    [lr.min[0] + r.max[0], lr.min[1] + r.max[1]],
                ));
                painter.rect_filled(sr, 1.0, accent.gamma_multiply(0.25));
                painter.rect_stroke(sr, 1.0, stroke, StrokeKind::Inside);
            }
            worlds_core::Command::SetTiles {
                level,
                layer,
                tiles,
                ..
            } => {
                let (Some(ix), Some(def)) = (
                    doc.level_ix(level.as_str()),
                    doc.project.layer(layer.as_str()),
                ) else {
                    continue;
                };
                let g = doc.project.layer_grid(def) as f64;
                let o = doc.level(ix).world_px;
                for t in tiles {
                    let r = WRect::from_pos_size(
                        [
                            o[0] as f64 + t.cell[0] as f64 * g,
                            o[1] as f64 + t.cell[1] as f64 * g,
                        ],
                        [g, g],
                    );
                    painter.rect_filled(to_srect(&r), 0.0, accent.gamma_multiply(0.25));
                    painter.rect_stroke(to_srect(&r), 0.0, stroke, StrokeKind::Inside);
                }
            }
            worlds_core::Command::UpdateNote { note, .. } => {
                let Some(r) = crate::annot::find_note(doc, &note.id) else {
                    continue;
                };
                let moved = crate::annot::NoteRef { at: r.at, note };
                let pts: Vec<Pos2> = crate::annot::world_points(doc, moved)
                    .iter()
                    .map(|p| to_screen([p[0] as f64, p[1] as f64]))
                    .collect();
                if let [p] = pts.as_slice() {
                    notes::disk(&mut lines, *p, 5.0, accent);
                } else {
                    let closed = matches!(note.shape, worlds_core::Shape::Region { .. });
                    for k in 1..pts.len() {
                        notes::seg_quad(&mut lines, pts[k - 1], pts[k], 2.0, accent);
                    }
                    if closed && pts.len() > 2 {
                        notes::seg_quad(&mut lines, pts[pts.len() - 1], pts[0], 2.0, accent);
                    }
                }
            }
            worlds_core::Command::MoveLevel {
                level, world_px, ..
            } => {
                let Some(ix) = doc.level_ix(level.as_str()) else {
                    continue;
                };
                let lr = level_wrect(doc, ix);
                let r = to_srect(&WRect::from_pos_size(
                    [world_px[0] as f64, world_px[1] as f64],
                    [lr.width(), lr.height()],
                ));
                painter.rect_filled(r, 0.0, accent.gamma_multiply(0.15));
                painter.rect_stroke(r, 0.0, stroke, StrokeKind::Outside);
            }
            _ => {}
        }
    }
    painter.add(egui::Shape::mesh(lines));
    let vis = WRect::new(
        [f64::NEG_INFINITY, f64::NEG_INFINITY],
        [f64::INFINITY, f64::INFINITY],
    );
    draw_snaps(painter, &plan.snaps, &vis, to_screen);
    if plan.d != [0, 0]
        && view.selection.levels().next().is_some()
        && let Some(p) = painter.ctx().pointer_latest_pos()
    {
        painter.text(
            p + egui::vec2(14.0, 14.0),
            Align2::LEFT_TOP,
            format!("{}, {}", plan.d[0], plan.d[1]),
            FontId::proportional(12.0),
            Color32::WHITE,
        );
    }
}

/// The Paint group's modes as a segmented strip (Brush, Line, Rectangle,
/// Fill), where the Note tool's options go. A click is `SetTool`, exactly
/// as the mode's key. Returns where it was drawn.
fn paint_strip(ctx: &egui::Context, map: Rect, tool: Tool, actions: &mut Vec<Action>) -> Rect {
    egui::Area::new(egui::Id::new("paint tool strip"))
        .fixed_pos(map.min + STRIP_OFFSET)
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.weak("Paint");
                    for t in Tool::PAINT_MODES {
                        if ui
                            .add(egui::Button::selectable(tool == t, t.name()))
                            .on_hover_text(format!("{} ({})", t.name(), t.shortcut()))
                            .clicked()
                            && tool != t
                        {
                            actions.push(Action::SetTool(t));
                        }
                    }
                });
            });
        })
        .response
        .rect
}

/// The Note tool's options: kind, scope (Auto or World), the pin icon.
fn note_strip(ctx: &egui::Context, map: Rect, view: &mut ViewState) {
    use crate::view::NoteKind;
    egui::Area::new(egui::Id::new("note tool strip"))
        .fixed_pos(map.min + STRIP_OFFSET)
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.horizontal(|ui| {
                    for k in NoteKind::ALL {
                        let label = match k {
                            NoteKind::Note => "Note",
                            NoteKind::Pin => "Pin",
                            NoteKind::Path => "Path",
                            NoteKind::Region => "Region",
                        };
                        if ui
                            .add(egui::Button::selectable(view.note_tool.kind == k, label))
                            .clicked()
                            && view.note_tool.kind != k
                        {
                            view.note_tool.kind = k;
                            view.drawing = None;
                        }
                    }
                    ui.separator();
                    if ui
                        .add(egui::Button::selectable(!view.note_tool.world, "Auto"))
                        .on_hover_text("In the level under the pointer, else the world")
                        .clicked()
                    {
                        view.note_tool.world = false;
                    }
                    if ui
                        .add(egui::Button::selectable(view.note_tool.world, "World"))
                        .on_hover_text("Always world scope (world.json)")
                        .clicked()
                    {
                        view.note_tool.world = true;
                    }
                    if view.note_tool.kind == NoteKind::Pin {
                        ui.separator();
                        egui::ComboBox::from_id_salt("note tool icon")
                            .selected_text(view.note_tool.icon.clone())
                            .show_ui(ui, |ui| {
                                for name in crate::annot::PIN_ICONS {
                                    ui.selectable_value(
                                        &mut view.note_tool.icon,
                                        name.to_string(),
                                        name,
                                    );
                                }
                            });
                    }
                    if matches!(view.note_tool.kind, NoteKind::Path | NoteKind::Region) {
                        ui.weak(
                            "click points, Enter or double click finishes, Backspace drops one",
                        );
                    }
                });
            });
        });
}

/// The guide lines a level drag snapped to, highlighted.
fn draw_snaps(
    painter: &egui::Painter,
    snaps: &[crate::annot::SnapHit],
    view: &WRect,
    to_screen: &dyn Fn([f64; 2]) -> Pos2,
) {
    let hot = Color32::from_rgb(255, 240, 120);
    for s in snaps {
        let (a, b) = match s.segment {
            Some(seg) => seg,
            None => {
                let c = s.at as f64;
                let far = 1e7;
                let (lo, hi) = if s.axis == 0 {
                    (view.min[1].max(-far), view.max[1].min(far))
                } else {
                    (view.min[0].max(-far), view.max[0].min(far))
                };
                if s.axis == 0 {
                    ([c, lo], [c, hi])
                } else {
                    ([lo, c], [hi, c])
                }
            }
        };
        painter.line_segment([to_screen(a), to_screen(b)], Stroke::new(3.0, hot));
    }
}

/// The Note tool's drawing in progress (a polyline to the pointer) or a
/// measurement, with its readout by the pointer.
fn draw_drawing(
    painter: &egui::Painter,
    doc: &Document,
    d: &crate::annot::Drawing,
    pointer: Option<[i64; 2]>,
    to_screen: &dyn Fn([f64; 2]) -> Pos2,
    accent: Color32,
) -> Option<String> {
    let mut pts: Vec<[i64; 2]> = d.points.clone();
    let live = if d.done { None } else { pointer };
    if let Some(p) = live {
        pts.push(p);
    }
    let sp: Vec<Pos2> = pts
        .iter()
        .map(|p| to_screen([p[0] as f64, p[1] as f64]))
        .collect();
    let col = if d.measure {
        Color32::from_rgb(255, 220, 90)
    } else {
        accent
    };
    let mut mesh = egui::Mesh::default();
    for k in 1..sp.len() {
        notes::seg_quad(&mut mesh, sp[k - 1], sp[k], 2.0, col);
    }
    if d.kind == crate::view::NoteKind::Region && !d.measure && sp.len() > 2 {
        notes::dashed(
            &mut mesh,
            sp[sp.len() - 1],
            sp[0],
            1.0,
            5.0,
            4.0,
            col,
            painter.clip_rect(),
        );
    }
    for p in &sp {
        notes::disk(&mut mesh, *p, 3.0, col);
    }
    painter.add(egui::Shape::mesh(mesh));
    let text = crate::annot::readout(doc, &d.points, live)?;
    if let Some(last) = sp.last() {
        let r = painter.text(
            *last + egui::vec2(12.0, 10.0),
            Align2::LEFT_TOP,
            &text,
            FontId::proportional(12.0),
            Color32::WHITE,
        );
        painter.rect_filled(r.expand(3.0), 3.0, Color32::from_black_alpha(150));
        painter.text(
            *last + egui::vec2(12.0, 10.0),
            Align2::LEFT_TOP,
            &text,
            FontId::proportional(12.0),
            Color32::WHITE,
        );
    }
    Some(text)
}

/// Where a Select press landed, as the viewport worked it out on screen.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SelectPress {
    pub active: Option<LevelIx>,
    /// On the active level's name tab ([`level_tab`]).
    pub on_tab: bool,
    /// On a resize handle of the active level.
    pub handle: Option<[i8; 2]>,
    /// The topmost level under the pointer.
    pub level_under: Option<LevelIx>,
    /// The world pixel under the pointer.
    pub wp: [f64; 2],
    /// Ctrl (or Cmd): add to the selection.
    pub add: bool,
    /// Screen px per world px (annotation hit sizes are on screen).
    pub zoom: f64,
}

/// The topmost level whose rect holds world pixel `wp` (draw order:
/// depth, then index).
pub fn level_under(doc: &Document, wp: [f64; 2]) -> Option<LevelIx> {
    let mut order: Vec<LevelIx> = (0..doc.levels.len()).collect();
    order.sort_by_key(|&i| (doc.level(i).depth, i));
    order
        .into_iter()
        .rev()
        .find(|&i| level_wrect(doc, i).contains(wp))
}

/// What a Select click at `wp` picks, topmost first: a note or pin (card,
/// dot, icon), an entity of a visible, unlocked layer in the level under
/// the pointer, a stack of the active tiles layer (visible, unlocked)
/// there, a path, guide or region outline within 4 screen px. Levels are
/// not items here (their body is handled by [`select_press`]).
pub fn select_item_at(
    doc: &Document,
    view: &ViewState,
    level_under: Option<LevelIx>,
    wp: [f64; 2],
    zoom: f64,
) -> Option<crate::view::SelItem> {
    use crate::view::SelItem;
    if let Some(id) = crate::annot::point_note_at(doc, view, wp, zoom) {
        return Some(SelItem::Note(id));
    }
    if let Some(ix) = level_under {
        if let Some(id) = selectable_entity_at(doc, view, ix, wp) {
            return Some(SelItem::Entity(id));
        }
        if let Some(t) = crate::annot::tile_stack_at(doc, view, ix, wp) {
            return Some(t);
        }
    }
    crate::annot::line_note_at(doc, view, wp, zoom).map(SelItem::Note)
}

/// A map with no level says how to make one (and, with no layer either,
/// where layers come from): a quiet card in the middle of the map.
fn empty_map_hint(painter: &egui::Painter, rect: Rect, no_layers: bool, accent: Color32) {
    let wrap = (rect.width() - 48.0).clamp(120.0, 460.0);
    let title = painter.layout(
        "No levels yet".into(),
        FontId::proportional(18.0),
        Color32::from_gray(225),
        wrap,
    );
    let mut body = String::from(
        "Right-click the map and pick New level here…\nor press Ctrl+P and run New level…",
    );
    if no_layers {
        body.push_str(
            "\n\nLayers (what you paint on) and tilesets (your art)\nare set up in the Project tab.",
        );
    }
    let body = painter.layout(
        body,
        FontId::proportional(14.0),
        Color32::from_gray(165),
        wrap,
    );
    let gap = 8.0;
    let w = title.size().x.max(body.size().x);
    let h = title.size().y + gap + body.size().y;
    let card =
        Rect::from_center_size(rect.center(), egui::vec2(w, h)).expand2(egui::vec2(20.0, 16.0));
    painter.rect_filled(card, 6.0, Color32::from_black_alpha(140));
    painter.rect_stroke(
        card,
        6.0,
        Stroke::new(1.0, accent.gamma_multiply(0.5)),
        StrokeKind::Inside,
    );
    let top = card.min.y + 16.0;
    painter.galley(
        egui::pos2(rect.center().x - title.size().x / 2.0, top),
        title.clone(),
        Color32::from_gray(225),
    );
    painter.galley(
        egui::pos2(
            rect.center().x - body.size().x / 2.0,
            top + title.size().y + gap,
        ),
        body,
        Color32::from_gray(165),
    );
}

/// The level of a gesture made where the project has no level: only its
/// `world_px` means anything ([`select_press`]).
pub const NO_LEVEL: LevelIx = usize::MAX;

/// What a Select press grabs (`None`: nothing), in which level, and what
/// it does to the selection and the active level right away:
///
/// - the active level's name tab: the level (drag to move it), or, when
///   the level is selected along with other things, the whole selection;
/// - a resize handle of the active level: that handle;
/// - an item ([`select_item_at`]: a note or pin, an entity, a tile stack
///   of the active tiles layer, an outline): it is selected (Ctrl: added)
///   unless it already was, and the drag moves the whole selection; an
///   entity's, a tile stack's or a level note's level becomes active;
/// - empty space in the active level, selected or not: a marquee (as in
///   the level itself: its body is where you marquee; it moves by its tab);
/// - another selected level's body: the drag moves the whole selection;
/// - another level's body: it becomes active and selected (Ctrl: added)
///   and the drag moves it alone;
/// - outside every level: a world marquee (levels and world annotations).
///
/// The returned level is the one the gesture's `CellHit`s are made in;
/// for a world marquee or a press outside every level it is the active
/// level, else the first, else [`NO_LEVEL`] when the project has none
/// (only `world_px` counts there: world annotations are still selected,
/// dragged and marqueed).
pub fn select_press(
    doc: &Document,
    view: &mut ViewState,
    p: SelectPress,
) -> Option<(LevelIx, Grab)> {
    use crate::view::SelItem;
    if let (true, Some(ix)) = (p.on_tab, p.active) {
        let me = SelItem::Level(doc.ids[ix].clone());
        if view.selection.contains(&me) && view.selection.items.len() > 1 {
            return Some((ix, Grab::Selection { was_selected: true }));
        }
        return Some((ix, Grab::Level));
    }
    if let (Some(ix), Some(e)) = (p.active, p.handle) {
        return Some((ix, Grab::Handle(e)));
    }
    let carrier = p
        .level_under
        .or(p.active.filter(|&a| a < doc.levels.len()))
        .or((!doc.levels.is_empty()).then_some(0))
        .unwrap_or(NO_LEVEL);
    if let Some(item) = select_item_at(doc, view, p.level_under, p.wp, p.zoom) {
        let level = match &item {
            SelItem::Entity(id) => doc.find_entity(id).map(|f| f.0),
            SelItem::Tiles { level, .. } => doc.level_ix_by_id(level),
            SelItem::Note(id) => crate::annot::find_note(doc, id).and_then(|r| r.level()),
            SelItem::Level(id) => doc.level_ix_by_id(id),
        };
        if let Some(ix) = level {
            view.active_level = Some(ix);
        }
        let was_selected = view.selection.contains(&item);
        if !was_selected {
            if p.add {
                view.selection.add(item);
            } else {
                view.selection.set([item]);
            }
        }
        return Some((level.unwrap_or(carrier), Grab::Selection { was_selected }));
    }
    let Some(ix) = p.level_under else {
        return Some((carrier, Grab::WorldMarquee));
    };
    if Some(ix) == p.active {
        return Some((ix, Grab::Marquee));
    }
    let level_item = SelItem::Level(doc.ids[ix].clone());
    if view.selection.contains(&level_item) {
        return Some((ix, Grab::Selection { was_selected: true }));
    }
    view.active_level = Some(ix);
    if p.add {
        view.selection.add(level_item);
    } else {
        view.selection.set([level_item]);
    }
    Some((ix, Grab::Level))
}

/// The click that answers an armed [`crate::view::PickRequest`], by what
/// it takes, topmost first: an annotation (if it takes notes), an entity
/// under the pointer (any level), the point (cells), the level. A click on
/// nothing it takes leaves the pick armed, with a status message. `zoom`
/// is screen px per world px.
pub fn pick_at(
    doc: &Document,
    view: &mut ViewState,
    level_under: Option<LevelIx>,
    wp: Option<[f64; 2]>,
    zoom: f64,
) {
    use crate::view::{Picked, PickedThing};
    let Some(req) = view.pick.clone() else {
        return;
    };
    let Some(wp) = wp else {
        return;
    };
    let note = req
        .accept
        .note
        .then(|| crate::annot::note_at(doc, view, wp, zoom))
        .flatten()
        .map(|id| PickedThing::Note { id });
    let what = note.or_else(|| {
        let ix = level_under?;
        let entity = req
            .accept
            .entity
            .then(|| entity_at(doc, view, ix, wp))
            .flatten()
            .map(|id| PickedThing::Entity { id, level: ix });
        entity
            .or_else(|| {
                req.accept.cell.then_some(PickedThing::Cell {
                    level: ix,
                    world_px: wp,
                })
            })
            .or_else(|| req.accept.level.then_some(PickedThing::Level { level: ix }))
    });
    match what {
        Some(what) => {
            view.pick = None;
            view.status = None;
            view.picked = Some(Picked {
                target: req.target,
                what,
            });
        }
        None if level_under.is_none() && !req.accept.note => view.set_status(
            format!("{}: click inside a level (Esc cancels)", req.prompt),
            true,
        ),
        None => view.set_status(
            format!("{}: nothing to pick here (Esc cancels)", req.prompt),
            true,
        ),
    }
}

/// The 8 resize handles of a level's screen rect: (edges, center).
fn handle_points(r: Rect) -> impl Iterator<Item = ([i8; 2], Pos2)> {
    [-1i8, 0, 1].into_iter().flat_map(move |ey| {
        [-1i8, 0, 1].into_iter().filter_map(move |ex| {
            if ex == 0 && ey == 0 {
                return None;
            }
            let x = match ex {
                -1 => r.min.x,
                1 => r.max.x,
                _ => r.center().x,
            };
            let y = match ey {
                -1 => r.min.y,
                1 => r.max.y,
                _ => r.center().y,
            };
            Some(([ex, ey], pos2(x, y)))
        })
    })
}

/// The handle under `p` (screen points): a corner or edge midpoint square,
/// else anywhere along an edge.
fn handle_at(r: Rect, p: Pos2) -> Option<[i8; 2]> {
    if r.width() < 24.0 || r.height() < 24.0 {
        return None;
    }
    if let Some((e, _)) = handle_points(r).find(|(_, c)| (p - *c).abs().max_elem() <= 6.0) {
        return Some(e);
    }
    let near = |a: f32, b: f32| (a - b).abs() <= 4.0;
    let in_x = p.x > r.min.x && p.x < r.max.x;
    let in_y = p.y > r.min.y && p.y < r.max.y;
    match () {
        _ if near(p.x, r.min.x) && in_y => Some([-1, 0]),
        _ if near(p.x, r.max.x) && in_y => Some([1, 0]),
        _ if near(p.y, r.min.y) && in_x => Some([0, -1]),
        _ if near(p.y, r.max.y) && in_x => Some([0, 1]),
        _ => None,
    }
}

fn handle_cursor(e: [i8; 2]) -> CursorIcon {
    match e {
        [0, _] => CursorIcon::ResizeVertical,
        [_, 0] => CursorIcon::ResizeHorizontal,
        [a, b] if a == b => CursorIcon::ResizeNwSe,
        _ => CursorIcon::ResizeNeSw,
    }
}

/// The topmost entity of a visible entities layer of level `ix` under
/// world pixel `wp` (the entity tool's Alt-click sample).
pub fn entity_at(
    doc: &Document,
    view: &ViewState,
    ix: LevelIx,
    wp: [f64; 2],
) -> Option<worlds_core::Id> {
    entity_under(doc, ix, wp, |i| view.layer_visible(i))
}

/// What the Select tool can pick under world pixel `wp` in level `ix`: the
/// topmost entity of a visible, unlocked entities layer.
pub fn selectable_entity_at(
    doc: &Document,
    view: &ViewState,
    ix: LevelIx,
    wp: [f64; 2],
) -> Option<worlds_core::Id> {
    entity_under(doc, ix, wp, |i| {
        view.layer_visible(i) && !view.layer_locked(i)
    })
}

fn entity_under(
    doc: &Document,
    ix: LevelIx,
    wp: [f64; 2],
    layer_ok: impl Fn(usize) -> bool,
) -> Option<worlds_core::Id> {
    let level = doc.level(ix);
    let lp = [
        wp[0] - level.world_px[0] as f64,
        wp[1] - level.world_px[1] as f64,
    ];
    let defs = defs_by_slug(doc);
    doc.project
        .layers
        .iter()
        .enumerate()
        .rev()
        .filter(|(i, d)| d.is_entities() && layer_ok(*i))
        .find_map(|(_, def)| {
            let g = doc.project.layer_grid(def);
            level
                .entities_layer(def.slug.as_str())?
                .entities
                .iter()
                .rev()
                .find(|e| entity_rect_in(&defs, doc, e, g).is_some_and(|r| r.contains(lp)))
                .map(|e| e.id.clone())
        })
}

/// The entities of level `ix` on visible, unlocked layers whose box meets
/// world rect `r` (the Select tool's marquee), in file order.
pub fn entities_in_rect(
    doc: &Document,
    view: &ViewState,
    ix: LevelIx,
    r: &WRect,
) -> Vec<worlds_core::Id> {
    let level = doc.level(ix);
    let o = [level.world_px[0] as f64, level.world_px[1] as f64];
    let local = WRect::new(
        [r.min[0] - o[0], r.min[1] - o[1]],
        [r.max[0] - o[0], r.max[1] - o[1]],
    );
    let mut out = Vec::new();
    let defs = defs_by_slug(doc);
    for (i, def) in doc.project.layers.iter().enumerate() {
        if !def.is_entities() || !view.layer_visible(i) || view.layer_locked(i) {
            continue;
        }
        let g = doc.project.layer_grid(def);
        let Some(el) = level.entities_layer(def.slug.as_str()) else {
            continue;
        };
        for e in &el.entities {
            if entity_rect_in(&defs, doc, e, g).is_some_and(|b| b.intersects(&local)) {
                out.push(e.id.clone());
            }
        }
    }
    out
}

/// The active level's name tab (screen points): a label just above the
/// level's top-left corner, kept inside the map `clip`, drawn at every
/// zoom. With the Select tool, dragging it moves the level (its body is
/// the marquee's).
///
/// `avoid` is the tool's strip ([`tool_strip_rect`]), which floats over the
/// map's top left: a tab that would be under it moves to the strip's right
/// while it stays over its level's width there, else just below the strip.
pub fn level_tab(level: Rect, name: &str, clip: Rect, avoid: Option<Rect>) -> Rect {
    let w = (name.chars().count().min(32) as f32 * 7.0 + 14.0).max(28.0);
    let h = 18.0;
    let mut min = pos2(level.min.x, level.min.y - h - 3.0);
    // Keep it on screen: inside the map, and above the level when that fits.
    let keep = |min: &mut egui::Pos2| {
        min.x = min.x.clamp(
            clip.min.x + 2.0,
            (clip.max.x - w - 2.0).max(clip.min.x + 2.0),
        );
        min.y = min.y.clamp(
            clip.min.y + 2.0,
            (clip.max.y - h - 2.0).max(clip.min.y + 2.0),
        );
    };
    keep(&mut min);
    let r = Rect::from_min_size(min, egui::vec2(w, h));
    let Some(a) = avoid.filter(|a| a.expand(2.0).intersects(r)) else {
        return r;
    };
    let right = a.max.x + 4.0;
    if right + w <= level.max.x.min(clip.max.x - 2.0) {
        min.x = right;
    } else {
        min.y = a.max.y + 3.0;
    }
    keep(&mut min);
    Rect::from_min_size(min, egui::vec2(w, h))
}

/// Where the tool's options strip (Note) or mode strip (Paint) floats over
/// the top left of `map` this frame, if the tool has one: its size as
/// drawn last frame (an estimate the first time).
pub fn tool_strip_rect(ctx: &egui::Context, map: Rect, tool: Tool) -> Option<Rect> {
    let (id, guess) = if tool == Tool::Note {
        ("note tool strip", egui::vec2(420.0, 40.0))
    } else if tool.is_paint_mode() {
        ("paint tool strip", egui::vec2(280.0, 40.0))
    } else {
        return None;
    };
    let size = ctx
        .memory(|m| m.area_rect(egui::Id::new(id)))
        .map_or(guess, |r| r.size());
    Some(Rect::from_min_size(map.min + STRIP_OFFSET, size))
}

/// The tool strips' place: this far in from the map's top left.
const STRIP_OFFSET: egui::Vec2 = egui::vec2(8.0, 8.0);

/// World rect of level `ix`.
pub fn level_wrect(doc: &Document, ix: LevelIx) -> WRect {
    let [x, y, w, h] = doc.level_rect(ix);
    WRect::from_pos_size([x, y], [w, h])
}

fn upload_tileset(vg: &mut ViewportGpu, gpu: &Gpu, doc: &Document, i: usize) {
    let def = &doc.project.tilesets[i];
    let path = doc.dir.join(&def.image);
    match image::open(&path) {
        Ok(img) => {
            let img = img.to_rgba8();
            vg.set_texture(
                &gpu.device,
                &gpu.queue,
                tileset_tex(i),
                img.width(),
                img.height(),
                img.as_raw(),
            );
        }
        Err(e) => {
            tracing::error!(
                "tileset {:?}: cannot load {}: {e}",
                def.slug.as_str(),
                path.display()
            );
            vg.clear_texture(tileset_tex(i));
        }
    }
}

/// Cells the current brush covers, for the preview.
fn brush_span(doc: &Document, view: &ViewState, layer: usize, tool: Tool) -> [i32; 2] {
    if tool != Tool::Brush {
        return [1, 1];
    }
    let def = &doc.project.layers[layer];
    match view.palette.brush(def.slug.as_str()) {
        Some(Brush::Tiles { tileset, rect }) => {
            let g = doc.project.layer_grid(def).max(1);
            let ts = doc
                .project
                .tileset(tileset.as_str())
                .map_or([g, g], |t| t.tile_size);
            [
                (rect[2] * ts[0]).div_ceil(g).max(1) as i32,
                (rect[3] * ts[1]).div_ceil(g).max(1) as i32,
            ]
        }
        _ => [1, 1],
    }
}

/// A derived layer's cell for the status bar: `pillar_marks: pillar_base
/// (rules)`, `(override)`, or `suppressed (rules: pillar_base)`.
pub fn describe_derived(
    doc: &Document,
    ix: LevelIx,
    def: &worlds_core::LayerDef,
    x: u32,
    y: u32,
) -> String {
    let name = |i: u16| {
        def.values()
            .get((i as usize).wrapping_sub(1))
            .map_or_else(|| "empty".to_string(), |v| v.slug.to_string())
    };
    let computed = doc
        .derived_computed(ix, def.slug.as_str())
        .and_then(|c| c.index_at(x, y))
        .unwrap_or(0);
    let stored = doc
        .level(ix)
        .overrides(def.slug.as_str())
        .and_then(|o| o.get(x, y));
    match stored {
        Some(worlds_core::Override::Value(v)) => format!("{}: {} (override)", def.slug, name(v)),
        Some(worlds_core::Override::Suppress) => {
            format!("{}: suppressed (rules: {})", def.slug, name(computed))
        }
        _ => format!("{}: {} (rules)", def.slug, name(computed)),
    }
}

/// What's in the active layer at a hit, for the status bar.
pub fn describe_cell(doc: &Document, h: CellHit) -> String {
    let def = &doc.project.layers[h.layer];
    let level = doc.level(h.level);
    if !h.inside {
        return String::new();
    }
    let [x, y] = [h.cell[0] as u32, h.cell[1] as u32];
    match &def.kind {
        LayerKind::IntGrid(_) if def.is_derived() => describe_derived(doc, h.level, def, x, y),
        LayerKind::IntGrid(_) => match level
            .int_grid(def.slug.as_str())
            .and_then(|g| g.index_at(x, y))
        {
            Some(i) if i > 0 => match def.values().get(i as usize - 1) {
                Some(v) => format!("{} '{}' {}", def.name, v.glyph, v.name),
                None => format!("{} value #{i}?", def.name),
            },
            _ => format!("{} empty", def.name),
        },
        LayerKind::Tiles(_) => level
            .tiles_layer(def.slug.as_str())
            .and_then(|t| t.tiles.iter().rev().find(|p| p.cell == [x, y]))
            .map_or(format!("{} empty", def.name), |p| {
                format!(
                    "{} {} [{}, {}]",
                    def.name,
                    p.tileset.as_str(),
                    p.tile[0],
                    p.tile[1]
                )
            }),
        LayerKind::Auto(_) => {
            let lp = [
                h.world_px[0] - level.world_px[0] as f64,
                h.world_px[1] - level.world_px[1] as f64,
            ];
            let found = doc.bakes[h.level]
                .as_ref()
                .and_then(|b| b.get(def.slug.as_str()))
                .and_then(|b| {
                    b.placements.iter().rev().find(|p| {
                        let ts = doc
                            .project
                            .tileset(p.tileset.as_str())
                            .map_or([1, 1], |t| t.tile_size);
                        lp[0] >= p.px[0] as f64
                            && lp[1] >= p.px[1] as f64
                            && lp[0] < (p.px[0] + ts[0] as i32) as f64
                            && lp[1] < (p.px[1] + ts[1] as i32) as f64
                    })
                });
            match found {
                Some(p) => format!(
                    "{} {} [{}, {}] rule {}",
                    def.name,
                    p.tileset.as_str(),
                    p.tile[0],
                    p.tile[1],
                    p.rule.as_str()
                ),
                None if doc.bakes[h.level].is_none() => format!("{} (baking)", def.name),
                None => format!("{} empty", def.name),
            }
        }
        LayerKind::Entities => {
            let g = doc.project.layer_grid(def);
            let lp = [
                h.world_px[0] - level.world_px[0] as f64,
                h.world_px[1] - level.world_px[1] as f64,
            ];
            let defs = defs_by_slug(doc);
            level
                .entities_layer(def.slug.as_str())
                .and_then(|l| {
                    l.entities
                        .iter()
                        .rev()
                        .find(|e| entity_rect_in(&defs, doc, e, g).is_some_and(|r| r.contains(lp)))
                })
                .map_or(format!("{} none", def.name), |e| {
                    format!("{} {} ({})", def.name, e.ty.as_str(), e.id.as_str())
                })
        }
    }
}

/// Level-local pixel rect of an entity on a layer of `g` px cells: its
/// pivot point sits at `(cell + pivot) * g + offset_px`, the box is
/// `size * project grid` (sizes are in project cells, whatever the layer's
/// grid; `docs/format.md`, "Position math"). `None` for an unknown type.
pub(crate) fn entity_rect(
    project: &worlds_core::Project,
    e: &worlds_core::EntityInstance,
    g: u32,
) -> Option<WRect> {
    let def = project.entity_def(e.ty.as_str())?;
    Some(entity_rect_of(def, e, g, project.grid_size))
}

/// [`entity_rect`] with the types looked up in `defs` ([`defs_by_slug`],
/// built once per call rather than a linear search per entity).
fn entity_rect_in(
    defs: &std::collections::HashMap<&str, &worlds_core::EntityDef>,
    doc: &Document,
    e: &worlds_core::EntityInstance,
    g: u32,
) -> Option<WRect> {
    let def = defs.get(e.ty.as_str())?;
    Some(entity_rect_of(def, e, g, doc.project.grid_size))
}

/// [`entity_rect`] with the type already looked up
/// ([`crate::tools::entity_box`], i.e. `EntityDef::pixel_rect`).
pub(crate) fn entity_rect_of(
    def: &worlds_core::EntityDef,
    e: &worlds_core::EntityInstance,
    g: u32,
    project_grid: u32,
) -> WRect {
    let [x, y, w, h] = crate::tools::entity_box(def, e, g, project_grid);
    WRect::from_pos_size([x, y], [w, h])
}

/// Entity types by slug, for a frame's drawing (`Project::entity_def` is a
/// linear search, too slow per entity with hundreds of types).
fn defs_by_slug(doc: &Document) -> std::collections::HashMap<&str, &worlds_core::EntityDef> {
    doc.project
        .entities
        .iter()
        .map(|d| (d.slug.as_str(), d))
        .collect()
}

/// Canvas labels (the type name above the box, `display.canvas` fields
/// below it) show from this many screen px per cell of the entity's layer.
pub const LABEL_CELL_PX: f64 = 24.0;
/// At most this many `display.canvas` lines under a box.
pub const MAX_CANVAS_LINES: usize = 4;

/// A field value as one short line of canvas text.
pub fn value_text(v: &serde_json::Value) -> String {
    let s = match v {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    if s.chars().count() > 24 {
        let cut: String = s.chars().take(23).collect();
        format!("{cut}…")
    } else {
        s
    }
}

/// An instance's value of field `f`: stored, else the default.
fn field_value<'a>(
    e: &'a worlds_core::EntityInstance,
    f: &'a worlds_core::FieldDef,
) -> Option<&'a serde_json::Value> {
    e.fields.get(&f.slug).or(f.default.as_ref())
}

/// The UV rect of atlas tile `tile` in a texture of `size` px.
pub fn tile_uv(ts: &worlds_core::TilesetDef, tile: [u32; 2], size: [usize; 2]) -> Rect {
    let p = ts.tile_px(tile);
    let (w, h) = (size[0].max(1) as f32, size[1].max(1) as f32);
    Rect::from_min_max(
        pos2(p[0] as f32 / w, p[1] as f32 / h),
        pos2(
            (p[0] + ts.tile_size[0]) as f32 / w,
            (p[1] + ts.tile_size[1]) as f32 / h,
        ),
    )
}

/// Draw `icon` fitted (aspect kept) and centered in `r`.
pub fn draw_icon(
    painter: &egui::Painter,
    doc: &Document,
    atlas: &Atlas,
    icon: &worlds_core::TileRef,
    r: Rect,
) -> bool {
    let Some(ts) = doc.project.tileset(icon.tileset.as_str()) else {
        return false;
    };
    let Some(Some(tex)) = atlas.get(icon.tileset.as_str()) else {
        return false;
    };
    let uv = tile_uv(ts, icon.tile, tex.size());
    let (tw, th) = (ts.tile_size[0].max(1) as f32, ts.tile_size[1].max(1) as f32);
    let scale = (r.width() / tw).min(r.height() / th);
    if scale <= 0.0 {
        return false;
    }
    let ir = Rect::from_center_size(r.center(), egui::vec2(tw * scale, th * scale));
    painter.image(tex.id(), ir, uv, Color32::WHITE);
    true
}

/// Below this many screen px (the box's smaller side), an entity box is a
/// plain fill: no icon, outline or labels (a crowded zoomed-out map costs
/// one rect per entity).
pub const MIN_DETAIL_BOX_PX: f32 = 6.0;

/// Entity boxes: fill and outline in the type's color, the icon inside,
/// the type name above and the `display.canvas` fields under the box when
/// a cell is at least [`LABEL_CELL_PX`] on screen (else the
/// `display.world` fields, from [`WORLD_LABEL_CELL_PX`]), arrows for
/// `display.arrow` fields (entity references to their target's box, in any
/// level; points to their cell; from [`ARROW_CELL_PX`]), in the accent
/// color for selected entities. Boxes smaller than [`MIN_DETAIL_BOX_PX`]
/// are just their fill (selected ones keep the accent outline).
///
/// Cost is bounded for a crowded map: each layer's fills and outlines are
/// one mesh and its icons one mesh per atlas (not a shape per entity), and
/// every label is drawn after every box (so no box covers a label), each
/// only if it doesn't overlap one drawn before ([`LabelSpace`]).
#[allow(clippy::too_many_arguments)]
fn draw_entities(
    painter: &egui::Painter,
    doc: &Document,
    scene: &Scene,
    view: &ViewState,
    visible: &WRect,
    zoom: f64,
    to_screen: &dyn Fn([f64; 2]) -> Pos2,
    accent: Color32,
    atlas: Option<&Atlas>,
) {
    // World rect of every entity, by id; built on the first arrow that
    // needs a target.
    let mut targets: Option<std::collections::HashMap<&str, WRect>> = None;
    let font = FontId::proportional(10.0);
    let mut space = LabelSpace::default();
    let defs = defs_by_slug(doc);
    let mut colors: std::collections::HashMap<&str, Color32> = std::collections::HashMap::new();
    let pg = doc.project.grid_size;
    // Membership test per entity: a set once the selection is big (a
    // marquee over hundreds).
    let selected_set: Option<std::collections::HashSet<&worlds_core::Id>> =
        (view.selection.items.len() > 8).then(|| view.selection.entities().collect());
    let is_selected = |id: &worlds_core::Id| match &selected_set {
        Some(s) => s.contains(id),
        None => view.selection.contains_entity(id),
    };
    // Labels wait for the second pass.
    let mut labels: Vec<LabelJob<'_>> = Vec::new();
    for (li, def) in doc.project.layers.iter().enumerate() {
        if !def.is_entities() || view.layer_opacity(doc, li) <= 0.0 {
            continue;
        }
        let g = doc.project.layer_grid(def);
        let cell_px = zoom * g as f64;
        let canvas_labels = cell_px >= LABEL_CELL_PX;
        let world_labels = cell_px >= WORLD_LABEL_CELL_PX;
        let arrows = cell_px >= ARROW_CELL_PX;
        let mut boxes = egui::Mesh::default();
        let mut icons: std::collections::HashMap<egui::TextureId, egui::Mesh> =
            std::collections::HashMap::new();
        let mut on_top: Vec<egui::Shape> = Vec::new();
        for &ix in &scene.draw_order {
            if !scene.levels[ix].rect.intersects(visible) {
                continue;
            }
            let level: &Level = doc.level(ix);
            let Some(layer) = level.entities_layer(def.slug.as_str()) else {
                continue;
            };
            let o = [level.world_px[0] as f64, level.world_px[1] as f64];
            for e in &layer.entities {
                let Some(&edef) = defs.get(e.ty.as_str()) else {
                    continue;
                };
                let r = entity_rect_of(edef, e, g, pg);
                let wr = WRect::new(
                    [r.min[0] + o[0], r.min[1] + o[1]],
                    [r.max[0] + o[0], r.max[1] + o[1]],
                );
                let selected = is_selected(&e.id);
                let col = *colors.entry(edef.slug.as_str()).or_insert_with(|| {
                    let c = parse_color(edef.color.as_str());
                    Color32::from_rgb(c[0], c[1], c[2])
                });
                // Arrows first: they may come from an entity off screen.
                if arrows || selected {
                    let stroke = Stroke::new(1.5, if selected { accent } else { col });
                    for f in edef.fields.iter().filter(|f| f.display.arrow) {
                        let Some(v) = field_value(e, f) else { continue };
                        let vals: &[serde_json::Value] = match v {
                            serde_json::Value::Array(a) if f.array => a,
                            v => std::slice::from_ref(v),
                        };
                        for v in vals {
                            let to = match (&f.ty, v) {
                                (
                                    worlds_core::FieldType::EntityRef { .. },
                                    serde_json::Value::String(id),
                                ) => {
                                    let t = targets
                                        .get_or_insert_with(|| entity_world_rects(doc, &defs));
                                    t.get(id.as_str()).map(|r| r.center())
                                }
                                (worlds_core::FieldType::Point, serde_json::Value::Array(xy)) => {
                                    match (
                                        xy.first().and_then(|x| x.as_f64()),
                                        xy.get(1).and_then(|y| y.as_f64()),
                                    ) {
                                        (Some(x), Some(y)) => {
                                            let gf = g as f64;
                                            Some([o[0] + (x + 0.5) * gf, o[1] + (y + 0.5) * gf])
                                        }
                                        _ => None,
                                    }
                                }
                                _ => None,
                            };
                            let Some(to) = to else { continue };
                            let from = wr.center();
                            if !visible.intersects(&WRect::new(
                                [from[0].min(to[0]), from[1].min(to[1])],
                                [from[0].max(to[0]) + 1.0, from[1].max(to[1]) + 1.0],
                            )) {
                                continue;
                            }
                            let (a, b) = (to_screen(from), to_screen(to));
                            if (b - a).length() > 4.0 {
                                painter.arrow(a, b - a, stroke);
                            }
                        }
                    }
                }
                if !wr.intersects(visible) {
                    continue;
                }
                let sr = Rect::from_min_max(to_screen(wr.min), to_screen(wr.max));
                if sr.width().min(sr.height()) < MIN_DETAIL_BOX_PX {
                    boxes.add_colored_rect(sr, col.gamma_multiply(0.6));
                    if selected {
                        on_top.push(egui::Shape::rect_stroke(
                            sr,
                            0.0,
                            Stroke::new(2.0, accent),
                            StrokeKind::Outside,
                        ));
                    }
                    continue;
                }
                boxes.add_colored_rect(sr, col.gamma_multiply(0.3));
                outline(&mut boxes, sr, 1.0, col);
                if let (Some(icon), Some(atlas)) = (&edef.icon, atlas)
                    && let Some((tex, ir, uv)) = icon_quad(doc, atlas, icon, sr.shrink(1.0))
                {
                    icons
                        .entry(tex)
                        .or_insert_with(|| egui::Mesh::with_texture(tex))
                        .add_rect_with_uv(ir, uv, Color32::WHITE);
                }
                if selected {
                    on_top.push(egui::Shape::rect_stroke(
                        sr,
                        1.0,
                        Stroke::new(2.0, accent),
                        StrokeKind::Outside,
                    ));
                }
                if canvas_labels || world_labels {
                    labels.push(LabelJob {
                        e,
                        def: edef,
                        sr,
                        canvas: canvas_labels,
                    });
                }
            }
        }
        painter.add(egui::Shape::mesh(boxes));
        for (_, m) in icons {
            painter.add(egui::Shape::mesh(m));
        }
        painter.extend(on_top);
    }
    // Labels last, over every box; the selected entities' first so a
    // crowded map never hides theirs.
    labels.sort_by_key(|j| !is_selected(&j.e.id));
    for j in labels {
        let sr = j.sr;
        let clip = painter.with_clip_rect(
            sr.expand2(egui::vec2(160.0, 80.0))
                .intersect(painter.clip_rect()),
        );
        if j.canvas {
            label_line(
                &clip,
                &mut space,
                pos2(sr.center().x, sr.min.y - 1.0),
                j.e.ty.to_string(),
                &font,
                Color32::from_gray(230),
                true,
            );
            let mut y = sr.max.y + 1.0;
            for f in j
                .def
                .fields
                .iter()
                .filter(|f| f.display.canvas)
                .take(MAX_CANVAS_LINES)
            {
                let v = field_value(j.e, f).map_or("—".to_string(), value_text);
                label_line(
                    &clip,
                    &mut space,
                    pos2(sr.center().x, y),
                    format!("{}: {v}", f.slug),
                    &font,
                    Color32::from_gray(210),
                    false,
                );
                y += 11.0;
            }
        } else {
            world_label(&clip, &mut space, j.e, j.def, sr.center_bottom(), &font);
        }
    }
}

/// `display.world` labels show under boxes from this many screen px per
/// cell (below, a crowded zoomed-out map would spend its frame on text
/// nobody can read; the flat world view has its own labels).
pub const WORLD_LABEL_CELL_PX: f64 = 12.0;
/// `display.arrow` arrows show from this many screen px per cell (a
/// selected entity's always do).
pub const ARROW_CELL_PX: f64 = 8.0;

/// A label to draw once every box is down.
struct LabelJob<'a> {
    e: &'a worlds_core::EntityInstance,
    def: &'a worlds_core::EntityDef,
    /// The box on screen.
    sr: Rect,
    /// Canvas labels (type above, `display.canvas` below), else the
    /// `display.world` line.
    canvas: bool,
}

/// A `w` px outline inside `r`, as four quads of `mesh`.
fn outline(mesh: &mut egui::Mesh, r: Rect, w: f32, col: Color32) {
    let (x0, y0, x1, y1) = (r.min.x, r.min.y, r.max.x, r.max.y);
    for q in [
        Rect::from_min_max(pos2(x0, y0), pos2(x1, y0 + w)),
        Rect::from_min_max(pos2(x0, y1 - w), pos2(x1, y1)),
        Rect::from_min_max(pos2(x0, y0 + w), pos2(x0 + w, y1 - w)),
        Rect::from_min_max(pos2(x1 - w, y0 + w), pos2(x1, y1 - w)),
    ] {
        mesh.add_colored_rect(q, col);
    }
}

/// Where [`draw_icon`] would draw `icon` in `r`: (texture, rect, uv).
fn icon_quad(
    doc: &Document,
    atlas: &Atlas,
    icon: &worlds_core::TileRef,
    r: Rect,
) -> Option<(egui::TextureId, Rect, Rect)> {
    let ts = doc.project.tileset(icon.tileset.as_str())?;
    let tex = atlas.get(icon.tileset.as_str())?.as_ref()?;
    let uv = tile_uv(ts, icon.tile, tex.size());
    let (tw, th) = (ts.tile_size[0].max(1) as f32, ts.tile_size[1].max(1) as f32);
    let scale = (r.width() / tw).min(r.height() / th);
    if scale <= 0.0 {
        return None;
    }
    let ir = Rect::from_center_size(r.center(), egui::vec2(tw * scale, th * scale));
    Some((tex.id(), ir, uv))
}

/// Screen space already taken by canvas text this frame: labels that would
/// overlap one drawn before are skipped, so a crowded map stays readable
/// and its cost is bounded by the screen, not by the entity count.
#[derive(Default)]
pub struct LabelSpace {
    taken: std::collections::HashSet<(i32, i32)>,
}

impl LabelSpace {
    const CELL: f32 = 4.0;

    /// Take `r` if no part of it is taken yet. (Its edges are pulled in a
    /// little, so lines of text right under each other both fit.)
    pub fn claim(&mut self, r: Rect) -> bool {
        let r = r.shrink2(egui::vec2(1.0, 2.5));
        let c = Self::CELL;
        let (x0, x1) = ((r.min.x / c).floor() as i32, (r.max.x / c).ceil() as i32);
        let (y0, y1) = ((r.min.y / c).floor() as i32, (r.max.y / c).ceil() as i32);
        if (x0..x1).any(|x| (y0..y1).any(|y| self.taken.contains(&(x, y)))) {
            return false;
        }
        for x in x0..x1 {
            for y in y0..y1 {
                self.taken.insert((x, y));
            }
        }
        true
    }
}

/// Draw one line of 10 px canvas text centered under `at` (or, with
/// `above`, over it), unless it would overlap text drawn before.
fn label_line(
    painter: &egui::Painter,
    space: &mut LabelSpace,
    at: Pos2,
    text: String,
    font: &FontId,
    color: Color32,
    above: bool,
) -> bool {
    // Measured without laying out: about 5.5 px a character.
    let w = text.chars().count() as f32 * 5.5;
    let top = if above { at.y - 11.0 } else { at.y };
    let r = Rect::from_min_size(pos2(at.x - w / 2.0, top), egui::vec2(w, 11.0));
    if !painter.clip_rect().intersects(r) || !space.claim(r) {
        return false;
    }
    let align = if above {
        Align2::CENTER_BOTTOM
    } else {
        Align2::CENTER_TOP
    };
    painter.text(at, align, text, font.clone(), color);
    true
}

/// Whether a value says nothing worth a label (null, empty).
fn is_blank(v: &serde_json::Value) -> bool {
    match v {
        serde_json::Value::Null => true,
        serde_json::Value::String(s) => s.is_empty(),
        serde_json::Value::Array(a) => a.is_empty(),
        serde_json::Value::Object(o) => o.is_empty(),
        _ => false,
    }
}

/// The `display.world` fields' values of `e` as one line under `at`.
fn world_label(
    painter: &egui::Painter,
    space: &mut LabelSpace,
    e: &worlds_core::EntityInstance,
    def: &worlds_core::EntityDef,
    at: Pos2,
    font: &FontId,
) {
    let vals: Vec<String> = def
        .fields
        .iter()
        .filter(|f| f.display.world)
        .filter_map(|f| field_value(e, f).filter(|v| !is_blank(v)).map(value_text))
        .collect();
    if vals.is_empty() {
        return;
    }
    label_line(
        painter,
        space,
        at + egui::vec2(0.0, 1.0),
        vals.join(" · "),
        font,
        Color32::from_gray(225),
        false,
    );
}

/// Zoomed out to flat levels: a dot per entity whose type has
/// `display.world` fields, with their values, where the level is wide
/// enough on screen to read them.
fn draw_world_labels(
    painter: &egui::Painter,
    doc: &Document,
    scene: &Scene,
    view: &ViewState,
    visible: &WRect,
    to_screen: &dyn Fn([f64; 2]) -> Pos2,
) {
    let font = FontId::proportional(10.0);
    let mut space = LabelSpace::default();
    let defs = defs_by_slug(doc);
    let pg = doc.project.grid_size;
    for (li, def) in doc.project.layers.iter().enumerate() {
        if !def.is_entities() || view.layer_opacity(doc, li) <= 0.0 {
            continue;
        }
        let g = doc.project.layer_grid(def);
        for &ix in &scene.draw_order {
            let lr = scene.levels[ix].rect;
            if !lr.intersects(visible) {
                continue;
            }
            let sr = Rect::from_min_max(to_screen(lr.min), to_screen(lr.max));
            if sr.width() < 60.0 {
                continue;
            }
            let level = doc.level(ix);
            let Some(layer) = level.entities_layer(def.slug.as_str()) else {
                continue;
            };
            let clip = painter.with_clip_rect(sr.intersect(painter.clip_rect()));
            for e in &layer.entities {
                let Some(&edef) = defs.get(e.ty.as_str()) else {
                    continue;
                };
                if !edef.fields.iter().any(|f| f.display.world) {
                    continue;
                }
                let r = entity_rect_of(edef, e, g, pg);
                let c = to_screen([lr.min[0] + r.center()[0], lr.min[1] + r.center()[1]]);
                let col = parse_color(edef.color.as_str());
                clip.circle_filled(c, 2.5, Color32::from_rgb(col[0], col[1], col[2]));
                world_label(&clip, &mut space, e, edef, c + egui::vec2(0.0, 2.0), &font);
            }
        }
    }
}

/// Every entity's world rect by id (arrow targets, any level).
fn entity_world_rects<'a>(
    doc: &'a Document,
    defs: &std::collections::HashMap<&str, &worlds_core::EntityDef>,
) -> std::collections::HashMap<&'a str, WRect> {
    let pg = doc.project.grid_size;
    let mut out = std::collections::HashMap::new();
    for ix in 0..doc.levels.len() {
        let level = doc.level(ix);
        let o = [level.world_px[0] as f64, level.world_px[1] as f64];
        for el in &level.entities {
            let Some(def) = doc.project.layer(el.layer.as_str()) else {
                continue;
            };
            let g = doc.project.layer_grid(def);
            for e in &el.entities {
                if let Some(r) = defs.get(e.ty.as_str()).map(|d| entity_rect_of(d, e, g, pg)) {
                    out.insert(
                        e.id.as_str(),
                        WRect::new(
                            [r.min[0] + o[0], r.min[1] + o[1]],
                            [r.max[0] + o[0], r.max[1] + o[1]],
                        ),
                    );
                }
            }
        }
    }
    out
}

fn draw_grid(
    painter: &egui::Painter,
    level: &WRect,
    visible: &WRect,
    g: f64,
    to_screen: &dyn Fn([f64; 2]) -> Pos2,
) {
    let x0 = level.min[0].max(visible.min[0]);
    let x1 = level.max[0].min(visible.max[0]);
    let y0 = level.min[1].max(visible.min[1]);
    let y1 = level.max[1].min(visible.max[1]);
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    let stroke = Stroke::new(1.0, Color32::from_white_alpha(18));
    let first = |lo: f64, origin: f64| origin + ((lo - origin) / g).ceil() * g;
    let mut x = first(x0, level.min[0]);
    while x <= x1 {
        painter.line_segment([to_screen([x, y0]), to_screen([x, y1])], stroke);
        x += g;
    }
    let mut y = first(y0, level.min[1]);
    while y <= y1 {
        painter.line_segment([to_screen([x0, y]), to_screen([x1, y])], stroke);
        y += g;
    }
}

#[cfg(test)]
mod label_tests {
    use super::*;

    /// What `draw_entities` paints for `n` synthetic types (⌈n/4⌉
    /// instances in the level) at `zoom`: the shapes, in paint order.
    fn painted(n: usize, zoom: f64) -> Vec<egui::Shape> {
        let mut doc = scene::tests::small_doc();
        let mut p = doc.project.clone();
        p.layers.push(worlds_core::LayerDef::new(
            worlds_core::slug("ents"),
            "Ents",
            worlds_core::LayerKind::Entities,
        ));
        crate::doc::synthesize_entities(&mut p, n, None);
        doc = Document::from_project(doc.dir.clone(), p);
        let scene = Scene::new(&doc);
        let view = ViewState::new();
        let visible = WRect::new([-1e9, -1e9], [1e9, 1e9]);
        let o = doc.level(0).world_px;
        let to_screen = move |w: [f64; 2]| {
            pos2(
                ((w[0] - o[0] as f64) * zoom) as f32,
                ((w[1] - o[1] as f64) * zoom) as f32,
            )
        };
        let ctx = egui::Context::default();
        let raw = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                Pos2::ZERO,
                egui::vec2(40.0 * 16.0 * zoom as f32, 10.0 * 16.0 * zoom as f32),
            )),
            ..Default::default()
        };
        let mut out = ctx.run_ui(raw, |ui| {
            let painter = ui.painter().clone();
            draw_entities(
                &painter,
                &doc,
                &scene,
                &view,
                &visible,
                zoom,
                &to_screen,
                Color32::YELLOW,
                None,
            );
        });
        out.textures_delta.clear();
        std::mem::take(&mut out.shapes)
            .into_iter()
            .map(|c| c.shape)
            .collect()
    }

    #[test]
    fn boxes_are_one_mesh_and_labels_come_after_every_box() {
        // 2x zoom of a 16 px grid: 32 px cells, canvas labels on.
        for n in [8, 40] {
            let shapes = painted(n, 2.0);
            let meshes: Vec<usize> = (0..shapes.len())
                .filter(|&i| matches!(shapes[i], egui::Shape::Mesh(_)))
                .collect();
            let texts: Vec<usize> = (0..shapes.len())
                .filter(|&i| matches!(shapes[i], egui::Shape::Text(_)))
                .collect();
            // Every box's fill and outline in one mesh, whatever the count.
            assert_eq!(meshes.len(), 1, "{n} types: {meshes:?}");
            assert!(!texts.is_empty(), "{n} types: labels drawn");
            assert!(
                meshes.iter().max() < texts.iter().min(),
                "{n} types: a box drawn after a label ({meshes:?}, {texts:?})"
            );
        }
        // Zoomed out (8 px cells): boxes, no labels at all.
        let shapes = painted(40, 0.5);
        assert!(
            !shapes.iter().any(|s| matches!(s, egui::Shape::Text(_))),
            "{shapes:?}"
        );
    }

    #[test]
    fn lines_under_each_other_both_fit_and_overlaps_dont() {
        let mut s = LabelSpace::default();
        let line =
            |y: f32, w: f32| Rect::from_min_size(pos2(100.0 - w / 2.0, y), egui::vec2(w, 11.0));
        for y0 in [430.0, 431.3, 433.9, 7.0] {
            let mut s = LabelSpace::default();
            assert!(s.claim(line(y0, 44.0)), "{y0}");
            assert!(s.claim(line(y0 + 11.0, 66.0)), "{y0}");
            assert!(s.claim(line(y0 + 22.0, 66.0)), "{y0}");
        }
        assert!(s.claim(line(0.0, 40.0)));
        assert!(!s.claim(line(4.0, 40.0)), "overlapping lines");
        assert!(
            s.claim(line(0.0, 40.0).translate(egui::vec2(60.0, 0.0))),
            "side by side"
        );
    }
}
