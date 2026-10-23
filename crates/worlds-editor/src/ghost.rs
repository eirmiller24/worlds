//! Approval mode's ghost overlay: what a pending agent batch would change,
//! drawn semi-transparent on the canvas until the user applies or rejects
//! it.
//!
//! [`Ghost::compute`] applies the batch's commands to a scratch clone of
//! the project (the server checked them on its own scratch copy at
//! submission, so this normally succeeds) and reads the new state where the
//! batch's `changes` say something changed: int_grid cells in their value
//! color, tiles cells in a tile tint, emptied cells dark, entities as boxes,
//! created, moved or resized levels as their new rect. Every changed rect
//! also gets a dashed outline. If the batch no longer applies to the
//! project as it is now, the ghost is only the outlines and says why.

use std::collections::HashSet;

use egui::{Color32, Pos2, Rect, Shape, Stroke};
use worlds_core::command::Change;
use worlds_core::{Command, LayerKind, Project};
use worlds_server::PendingBatch;

use crate::viewport::camera::WRect;
use crate::viewport::entity_rect;
use crate::viewport::scene::parse_color;

/// Tint of cells a batch puts tiles in (the ghost doesn't draw tile art).
pub const TILE_TINT: Color32 = Color32::from_rgb(110, 170, 255);
/// Fill of cells a batch empties.
pub const EMPTY_TINT: Color32 = Color32::from_rgb(16, 16, 20);
/// Alpha of ghost fills.
pub const GHOST_ALPHA: f32 = 0.5;

/// One pending batch, ready to draw (world pixels).
#[derive(Debug, Clone, PartialEq)]
pub struct Ghost {
    /// The pending batch id (`b1`, ...).
    pub batch: String,
    pub label: String,
    pub client: String,
    /// Changed cells, merged into horizontal runs of one color.
    pub fills: Vec<(WRect, Color32)>,
    /// Dashed outlines: each changed cell rect, each touched level rect.
    pub outlines: Vec<WRect>,
    /// Entity boxes as the batch leaves them (removed ones in red).
    pub boxes: Vec<(WRect, Color32)>,
    /// Why the batch no longer applies to the current project, if it
    /// doesn't (then only the outlines are drawn, from `changes`).
    pub error: Option<String>,
    /// Cells the batch changes (grid and tiles).
    pub cell_count: usize,
}

impl Ghost {
    /// Compute the ghost of `p` against `project` (the document now).
    pub fn compute(project: &Project, p: &PendingBatch) -> Ghost {
        let mut scratch = project.clone();
        let applied = scratch.apply(Command::Batch {
            label: p.label.clone(),
            commands: p.commands.clone(),
        });
        let mut g = Ghost {
            batch: p.id.clone(),
            label: p.label.clone(),
            client: p.client.clone(),
            fills: Vec::new(),
            outlines: Vec::new(),
            boxes: Vec::new(),
            error: None,
            cell_count: 0,
        };
        match applied {
            Ok(a) => {
                let changes = if a.changes.is_empty() {
                    p.changes.clone()
                } else {
                    a.changes
                };
                for c in &changes {
                    g.add(project, &scratch, c, true);
                }
            }
            Err(e) => {
                g.error = Some(e.to_string());
                for c in &p.changes {
                    g.add(project, project, c, false);
                }
            }
        }
        g
    }

    /// Add what change `c` looks like in `after` (`before` is the project
    /// without the batch). `fill`: draw contents, not only outlines.
    fn add(&mut self, before: &Project, after: &Project, c: &Change, fill: bool) {
        let grid = after.grid_size as f64;
        let level_rect = |p: &Project, slug: &str| {
            p.level(slug).map(|l| {
                WRect::from_pos_size(
                    [l.world_px[0] as f64, l.world_px[1] as f64],
                    [l.size[0] as f64 * grid, l.size[1] as f64 * grid],
                )
            })
        };
        match c {
            Change::Grid {
                level, layer, rect, ..
            }
            | Change::Tiles {
                level, layer, rect, ..
            } => {
                let (Some(l), Some(def)) =
                    (after.level(level.as_str()), after.layer(layer.as_str()))
                else {
                    return;
                };
                let g = after.layer_grid(def) as f64;
                let o = [l.world_px[0] as f64, l.world_px[1] as f64];
                let cell = |x: u32, y: u32, w: u32| {
                    WRect::from_pos_size(
                        [o[0] + x as f64 * g, o[1] + y as f64 * g],
                        [w as f64 * g, g],
                    )
                };
                self.cell_count += (rect.w * rect.h) as usize;
                self.outlines.push(WRect::from_pos_size(
                    [o[0] + rect.x as f64 * g, o[1] + rect.y as f64 * g],
                    [rect.w as f64 * g, rect.h as f64 * g],
                ));
                if !fill {
                    return;
                }
                let color_at: Box<dyn Fn(u32, u32) -> Color32> = match &def.kind {
                    // A derived layer: the overrides it would store (grey
                    // for a suppressed cell, empty where the rules decide).
                    LayerKind::IntGrid(_) if def.is_derived() => {
                        let ov = l.overrides(layer.as_str());
                        let values = def.values();
                        Box::new(move |x, y| match ov.and_then(|o| o.get(x, y)) {
                            Some(worlds_core::Override::Value(i)) => {
                                values.get(i as usize - 1).map_or(EMPTY_TINT, |v| {
                                    let c = parse_color(v.color.as_str());
                                    Color32::from_rgb(c[0], c[1], c[2])
                                })
                            }
                            Some(worlds_core::Override::Suppress) => Color32::from_gray(150),
                            _ => EMPTY_TINT,
                        })
                    }
                    LayerKind::IntGrid(_) => {
                        let grid = l.int_grid(layer.as_str());
                        let values = def.values();
                        Box::new(move |x, y| {
                            match grid.and_then(|gr| gr.index_at(x, y)).filter(|&i| i > 0) {
                                Some(i) => values.get(i as usize - 1).map_or(EMPTY_TINT, |v| {
                                    let c = parse_color(v.color.as_str());
                                    Color32::from_rgb(c[0], c[1], c[2])
                                }),
                                None => EMPTY_TINT,
                            }
                        })
                    }
                    LayerKind::Tiles(_) => {
                        let filled: HashSet<[u32; 2]> = l
                            .tiles_layer(layer.as_str())
                            .map(|t| t.tiles.iter().map(|p| p.cell).collect())
                            .unwrap_or_default();
                        Box::new(move |x, y| {
                            if filled.contains(&[x, y]) {
                                TILE_TINT
                            } else {
                                EMPTY_TINT
                            }
                        })
                    }
                    _ => return,
                };
                for y in rect.y..rect.y + rect.h {
                    let mut run: Option<(u32, Color32)> = None;
                    for x in rect.x..=rect.x + rect.w {
                        let c = (x < rect.x + rect.w).then(|| color_at(x, y));
                        match (run, c) {
                            (Some((_, rc)), Some(c)) if rc == c => {}
                            (r, c) => {
                                if let Some((x0, rc)) = r {
                                    self.fills.push((cell(x0, y, x - x0), rc));
                                }
                                run = c.map(|c| (x, c));
                            }
                        }
                    }
                }
            }
            Change::Entity { level, id, .. } => {
                let find = |p: &Project| {
                    let l = p.level(level.as_str())?;
                    p.layers.iter().filter(|d| d.is_entities()).find_map(|d| {
                        let e = l
                            .entities_layer(d.slug.as_str())?
                            .entities
                            .iter()
                            .find(|e| e.id == *id)?;
                        let r = entity_rect(p, e, p.layer_grid(d))?;
                        let o = [l.world_px[0] as f64, l.world_px[1] as f64];
                        let c = p
                            .entity_def(e.ty.as_str())
                            .map_or([255; 4], |d| parse_color(d.color.as_str()));
                        Some((
                            WRect::new(
                                [r.min[0] + o[0], r.min[1] + o[1]],
                                [r.max[0] + o[0], r.max[1] + o[1]],
                            ),
                            Color32::from_rgb(c[0], c[1], c[2]),
                        ))
                    })
                };
                match find(after) {
                    Some(b) => self.boxes.push(b),
                    None => {
                        if let Some((r, _)) = find(before) {
                            self.boxes.push((r, Color32::from_rgb(230, 80, 80)));
                        }
                    }
                }
            }
            Change::LevelMeta { level, .. } | Change::LevelCreated { level, .. } => {
                self.outlines.extend(level_rect(after, level.as_str()));
            }
            Change::LevelDeleted { level, .. } => {
                self.outlines.extend(level_rect(before, level.as_str()));
            }
            // Annotations: the box of the annotation as the batch leaves it
            // (or as it was, when removed), as built.
            Change::Note { id, .. } => {
                let bbox = |p: &Project| -> Option<WRect> {
                    let (a, o) = find_note(p, id)?;
                    let [x, y, w, h] = a.bbox()?;
                    Some(WRect::from_pos_size(
                        [(x + o[0]) as f64 - 2.0, (y + o[1]) as f64 - 2.0],
                        [w as f64 + 4.0, h as f64 + 4.0],
                    ))
                };
                self.outlines.extend(bbox(after).or_else(|| bbox(before)));
            }
            Change::GuideGroup { world, slug } => {
                let Some(w) = after.world(world.as_str()) else {
                    return;
                };
                let off = w.guide_group(slug.as_str()).map_or([0, 0], |g| g.offset);
                let r = w
                    .notes
                    .iter()
                    .filter(|a| {
                        matches!(&a.shape, worlds_core::Shape::Guide { group: Some(g), .. } if g == slug)
                    })
                    .filter_map(|a| a.bbox())
                    .map(|[x, y, w, h]| {
                        WRect::from_pos_size(
                            [(x + off[0]) as f64, (y + off[1]) as f64],
                            [w as f64, h as f64],
                        )
                    })
                    .fold(WRect::EMPTY, |a, b| a.union(&b));
                if r.min[0].is_finite() {
                    self.outlines.push(r);
                }
            }
            _ => {}
        }
    }

    /// Cells the batch changes (the pending panel shows it).
    pub fn cells(&self) -> usize {
        self.cell_count
    }

    /// Draw onto the canvas. `to_screen` maps world pixels to points.
    pub fn draw(
        &self,
        painter: &egui::Painter,
        to_screen: &dyn Fn([f64; 2]) -> Pos2,
        accent: Color32,
    ) {
        let srect = |r: &WRect| Rect::from_min_max(to_screen(r.min), to_screen(r.max));
        for (r, c) in &self.fills {
            painter.rect_filled(srect(r), 0.0, c.gamma_multiply(GHOST_ALPHA));
        }
        for (r, c) in &self.boxes {
            let s = srect(r);
            painter.rect_filled(s, 1.0, c.gamma_multiply(0.25));
            dashed_rect(painter, s, Stroke::new(1.5, *c));
        }
        let stroke = Stroke::new(1.5, accent);
        for r in &self.outlines {
            dashed_rect(painter, srect(r), stroke);
        }
    }
}

/// An annotation by id and the offset to world px (its level's position,
/// or a guide's group offset): [`worlds_core::Project::find_note`].
fn find_note<'a>(
    p: &'a Project,
    id: &worlds_core::Id,
) -> Option<(&'a worlds_core::Annotation, [i64; 2])> {
    p.find_note(id).map(|n| (n.note, n.offset()))
}

/// A dashed rectangle outline.
pub fn dashed_rect(painter: &egui::Painter, r: Rect, stroke: Stroke) {
    let pts = [
        r.left_top(),
        r.right_top(),
        r.right_bottom(),
        r.left_bottom(),
        r.left_top(),
    ];
    painter.extend(Shape::dashed_line(&pts, stroke, 6.0, 4.0));
}

/// Keep `ghosts` in step with the server's pending batches: drop decided
/// ones, compute new ones (each once; the project they were computed on is
/// the one at submission or at the last reload).
pub fn sync(ghosts: &mut Vec<Ghost>, pending: &[PendingBatch], project: &Project) {
    ghosts.retain(|g| pending.iter().any(|p| p.id == g.batch));
    for p in pending {
        if !ghosts.iter().any(|g| g.batch == p.id) {
            ghosts.push(Ghost::compute(project, p));
        }
    }
    ghosts.sort_by_key(|g| pending.iter().position(|p| p.id == g.batch));
}
