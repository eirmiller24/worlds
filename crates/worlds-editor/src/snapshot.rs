//! Map-only PNG rendering: `view.snapshot` and `worlds --snapshot` both go
//! through [`SnapshotRenderer::render`], so the same arguments give the
//! same pixels.
//!
//! The renderer owns its own offscreen wgpu device ([`Headless`]), egui
//! context and [`Viewport`] in [`Viewport::map_only`] mode. That works the
//! same in the window (eframe's device and the main viewport's GPU state are
//! left alone) and in `--screenshot --serve`. It is created on the first
//! snapshot and kept: tileset textures are uploaded again only when the
//! document or its images change; the chunk scene is rebuilt per snapshot
//! (only the chunks in the frame are built).

use std::path::PathBuf;

use worlds_core::Slug;
use worlds_core::command::CommandError;
use worlds_server::{RpcError, Snapshot, SnapshotRequest, ViewTarget};

use crate::doc::Document;
use crate::headless::Headless;
use crate::tools::Invalidation;
use crate::view::ViewState;
use crate::viewport::camera::{Camera, WRect};
use crate::viewport::{Viewport, ViewportCx, level_wrect};

/// The default and largest `size` (`docs/api.md`, `view.snapshot`).
pub const DEFAULT_SIZE: [u32; 2] = [1024, 1024];
/// Images are capped at this many pixels per axis.
pub const MAX_SIZE: u32 = 4096;

/// Where and how big: the world rect, screen px per world px, image size.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Plan {
    pub rect: WRect,
    pub scale: f64,
    pub size: [u32; 2],
}

/// Work out the frame of a request: the target's world rect at the largest
/// scale that fits `req.size` (whole when it is 1 or more).
pub fn plan(doc: &Document, req: &SnapshotRequest) -> Result<Plan, RpcError> {
    let rect = match &req.target {
        ViewTarget::Level(slug) => {
            let ix = doc
                .level_ix(slug.as_str())
                .ok_or_else(|| RpcError::command(&CommandError::UnknownLevel(slug.clone())))?;
            level_wrect(doc, ix)
        }
        ViewTarget::Note(id) => crate::annot::focus_rect(doc, id)
            .map(|(r, _)| r)
            .ok_or_else(|| RpcError::command(&CommandError::UnknownNote(id.clone())))?,
        ViewTarget::WorldRect { world, rect } => {
            check_world(doc, world.as_ref())?;
            if rect[2] <= 0 || rect[3] <= 0 {
                return Err(RpcError::invalid_params(format!(
                    "world_rect {rect:?} needs a positive width and height: [x, y, w, h] in world pixels"
                )));
            }
            WRect::from_pos_size(
                [rect[0] as f64, rect[1] as f64],
                [rect[2] as f64, rect[3] as f64],
            )
        }
        ViewTarget::World(w) => {
            let wi = world_index(doc, w)?;
            let r = crate::viewport::world_wrect(doc, wi);
            if r.is_empty() {
                return Err(RpcError::invalid_params(format!(
                    "world {w} has no levels or annotations to show"
                )));
            }
            r
        }
    };
    if req.size[0] == 0 || req.size[1] == 0 {
        return Err(RpcError::invalid_params(format!(
            "size {:?} must be at least 1 x 1",
            req.size
        )));
    }
    let max = [req.size[0].min(MAX_SIZE), req.size[1].min(MAX_SIZE)];
    let mut scale = (max[0] as f64 / rect.width()).min(max[1] as f64 / rect.height());
    if scale >= 1.0 {
        scale = scale.floor();
    }
    let size = [
        ((rect.width() * scale).round() as u32).clamp(1, max[0]),
        ((rect.height() * scale).round() as u32).clamp(1, max[1]),
    ];
    Ok(Plan { rect, scale, size })
}

/// The index of world `w` in `project.worlds`.
fn world_index(doc: &Document, w: &Slug) -> Result<usize, RpcError> {
    doc.project
        .worlds
        .iter()
        .position(|x| x.slug == *w)
        .ok_or_else(|| RpcError::command(&CommandError::UnknownWorld(w.clone())))
}

/// The world a target is in (index into `project.worlds`): a level's or
/// an annotation's world, the named one, else `None` (the view's).
pub fn target_world(doc: &Document, target: &ViewTarget) -> Option<usize> {
    match target {
        ViewTarget::Level(slug) => doc.level_ix(slug.as_str()).map(|ix| doc.levels[ix].world),
        ViewTarget::Note(id) => {
            crate::annot::find_note(doc, id).map(|r| crate::annot::note_world(doc, r.at))
        }
        ViewTarget::WorldRect { world: Some(w), .. } | ViewTarget::World(w) => {
            world_index(doc, w).ok()
        }
        ViewTarget::WorldRect { world: None, .. } => None,
    }
}

fn check_world(doc: &Document, world: Option<&Slug>) -> Result<(), RpcError> {
    match world {
        Some(w) if doc.project.world(w.as_str()).is_none() => {
            Err(RpcError::command(&CommandError::UnknownWorld(w.clone())))
        }
        _ => Ok(()),
    }
}

/// The view a snapshot draws with: `base`'s layer visibility and opacity
/// (or only `layers`), no selection.
pub fn snapshot_view(
    doc: &Document,
    base: &ViewState,
    layers: Option<&[Slug]>,
) -> Result<ViewState, RpcError> {
    let mut view = base.clone();
    if view.layers.len() != doc.project.layers.len() {
        view.sync_layers(doc, None);
    }
    view.selection.clear();
    view.pick = None;
    view.active_level = None;
    if let Some(only) = layers {
        for l in only {
            if doc.project.layer(l.as_str()).is_none() {
                return Err(RpcError::command(&CommandError::UnknownLayer(l.clone())));
            }
        }
        for (i, def) in doc.project.layers.iter().enumerate() {
            view.layers[i].visible = only.contains(&def.slug);
            if view.layers[i].visible && view.layers[i].opacity <= 0.0 {
                view.layers[i].opacity = 1.0;
            }
        }
    }
    Ok(view)
}

/// The offscreen map renderer (see the module docs).
pub struct SnapshotRenderer {
    hl: Headless,
    ctx: egui::Context,
    viewport: Viewport,
    /// What the uploaded textures belong to: project dir and
    /// [`Document::images_gen`].
    uploaded: Option<(PathBuf, u64)>,
    t: f64,
}

impl SnapshotRenderer {
    /// Create the offscreen device (any adapter, else the software one).
    pub fn new() -> anyhow::Result<Self> {
        let hl = Headless::new([1, 1])?;
        hl.gpu.install_viewport();
        tracing::info!("snapshot renderer on {}", hl.adapter);
        let mut viewport = Viewport::new();
        viewport.rebuild_budget = None;
        viewport.map_only = true;
        Ok(SnapshotRenderer {
            hl,
            ctx: egui::Context::default(),
            viewport,
            uploaded: None,
            t: 0.0,
        })
    }

    /// The adapter it renders on.
    pub fn adapter(&self) -> &str {
        &self.hl.adapter
    }

    /// Render `req` of `doc` as `view` shows it (layer visibility and
    /// opacity) to a PNG. Shared by `view.snapshot` and `worlds --snapshot`.
    pub fn render(
        &mut self,
        doc: &Document,
        view: &ViewState,
        req: &SnapshotRequest,
    ) -> Result<Snapshot, RpcError> {
        let plan = plan(doc, req)?;
        let mut view = snapshot_view(doc, view, req.layers.as_deref())?;
        // Only the target's world is drawn.
        if let Some(w) = target_world(doc, &req.target) {
            view.set_world(doc, w);
        }
        let img = self.render_rgba(doc, &mut view, &plan, req.overlays)?;
        let mut png = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .map_err(|e| RpcError::internal(format!("encode the snapshot PNG: {e}")))?;
        Ok(Snapshot {
            png,
            width: plan.size[0],
            height: plan.size[1],
            scale: plan.scale,
        })
    }

    fn render_rgba(
        &mut self,
        doc: &Document,
        view: &mut ViewState,
        plan: &Plan,
        entities: bool,
    ) -> Result<image::RgbaImage, RpcError> {
        let key = (doc.dir.clone(), doc.images_gen);
        if self.uploaded.as_ref() != Some(&key) {
            self.viewport.set_document(doc, &self.hl.gpu);
            self.uploaded = Some(key);
        } else {
            self.viewport.invalidate(doc, &Invalidation::All);
        }
        self.viewport.camera = Camera {
            origin: plan.rect.min,
            zoom: plan.scale,
        };
        self.viewport.draw_entities = entities;
        self.hl.set_size(plan.size);
        let (mut events, mut actions) = (Vec::new(), Vec::new());
        let gpu = self.hl.gpu.clone();
        let vp = &mut self.viewport;
        self.hl.run(&self.ctx, self.t, |ui| {
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show(ui, |ui| {
                    let mut cx = ViewportCx {
                        doc: Some(doc),
                        gpu: &gpu,
                        view: &mut *view,
                        events: &mut events,
                        actions: &mut actions,
                        accent: crate::app::theme::ACCENT,
                        ghosts: &[],
                        highlight: None,
                        atlas: None,
                    };
                    vp.ui(ui, &mut cx);
                });
        });
        self.t += 1.0 / 60.0;
        self.hl
            .read()
            .map_err(|e| RpcError::internal(format!("read the snapshot back: {e:#}")))
    }
}
