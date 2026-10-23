//! Helpers that plan `resize_level` commands for clients: which entities a
//! resize would leave outside the level (tools refuse that), and the
//! resizes that crop a level to its content.

use super::{Plane, Result};
use crate::command::{Command, CommandError};
use crate::id::{Id, Slug};
use crate::model::{LayerDef, Level, Project};

/// Cells of the project grid the content of a level `old` cells big moves
/// right and down when it is resized to `size` with `anchor` (see
/// [`Command::ResizeLevel`]).
pub(super) fn resize_shift(old: [u32; 2], size: [u32; 2], anchor: [i8; 2]) -> [i64; 2] {
    std::array::from_fn(|a| {
        let diff = size[a] as i64 - old[a] as i64;
        match anchor[a] {
            -1 => 0,
            0 => diff.div_euclid(2),
            _ => diff,
        }
    })
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

impl Project {
    /// The ids of the entities that `resize_level { world, level, size,
    /// anchor }` would leave outside the level, in layer then list order.
    /// The command keeps them (and reports `entity_outside`); the editor's
    /// tools refuse such a resize, as they refuse moving an entity out.
    pub fn resize_leaves_outside(
        &self,
        world: &Slug,
        level: &Slug,
        size: [u32; 2],
        anchor: [i8; 2],
    ) -> Result<Vec<Id>> {
        let old = self.level_at(self.find_level_in(world, level)?);
        let shift = resize_shift(old.size, size, anchor);
        let g = self.grid_size.max(1) as i64;
        let mut out = Vec::new();
        for el in &old.entities {
            let def = self.layer_kind(&el.layer, "entities", LayerDef::is_entities)?;
            let lg = self.layer_grid(def) as i64;
            let ns = size.map(|n| (n as u64 * g as u64).div_ceil(lg as u64) as i64);
            // A shift that isn't whole cells is refused by the command anyway.
            let s = [shift[0] * g / lg, shift[1] * g / lg];
            for e in &el.entities {
                let c = [e.cell[0] as i64 + s[0], e.cell[1] as i64 + s[1]];
                if c[0] < 0 || c[1] < 0 || c[0] >= ns[0] || c[1] >= ns[1] {
                    out.push(e.id.clone());
                }
            }
        }
        Ok(out)
    }

    /// The bounding box of a level's content in cells of the project grid,
    /// `[x, y, w, h]`: non-empty int_grid cells, derived overrides, tile
    /// stacks and entities (each entity's cell and its box, as far as they
    /// lie inside the level). Notes don't count. `None` when there is none.
    /// The left and top edges are rounded down so that cropping there moves
    /// every layer of the level by whole cells of its own grid.
    pub fn content_bounds(&self, level: &Level) -> Result<Option<[u32; 4]>> {
        let g = self.grid_size.max(1) as i64;
        let level_px = [level.size[0] as i64 * g, level.size[1] as i64 * g];
        // min x, min y, max x, max y in level pixels.
        let mut b = [i64::MAX, i64::MAX, i64::MIN, i64::MIN];
        let mut add = |x0: i64, y0: i64, x1: i64, y1: i64| {
            let (x0, y0) = (x0.max(0), y0.max(0));
            let (x1, y1) = (x1.min(level_px[0]), y1.min(level_px[1]));
            if x0 < x1 && y0 < y1 {
                b = [b[0].min(x0), b[1].min(y0), b[2].max(x1), b[3].max(y1)];
            }
        };
        // Project cells a crop at the left or top must be a multiple of.
        let mut step = 1u64;
        let mut grid_of = |def: &LayerDef| {
            let lg = self.layer_grid(def) as u64;
            let need = lg / gcd(lg, g as u64);
            step = step / gcd(step, need) * need;
            lg as i64
        };
        fn plane_cells<P: Plane>(p: &P, lg: i64, add: &mut impl FnMut(i64, i64, i64, i64)) {
            let [w, h] = p.size();
            for y in 0..h {
                for x in 0..w {
                    if p.code(x, y) != 0 {
                        let (x, y) = (x as i64 * lg, y as i64 * lg);
                        add(x, y, x + lg, y + lg);
                    }
                }
            }
        }
        for gl in &level.int_grids {
            let lg = grid_of(self.grid_def(&gl.layer)?);
            plane_cells(gl, lg, &mut add);
        }
        for o in &level.overrides {
            let lg = grid_of(self.grid_def(&o.layer)?);
            plane_cells(o, lg, &mut add);
        }
        for tl in &level.tiles {
            let lg = grid_of(self.layer_kind(&tl.layer, "tiles", LayerDef::is_tiles)?);
            for t in &tl.tiles {
                let (x, y) = (t.cell[0] as i64 * lg, t.cell[1] as i64 * lg);
                add(x, y, x + lg, y + lg);
            }
        }
        for el in &level.entities {
            let def = self.layer_kind(&el.layer, "entities", LayerDef::is_entities)?;
            let lg = grid_of(def);
            for e in &el.entities {
                let (x, y) = (e.cell[0] as i64 * lg, e.cell[1] as i64 * lg);
                add(x, y, x + lg, y + lg);
                if let Some(ed) = self.entity_def(e.ty.as_str()) {
                    let [x, y, w, h] = ed.pixel_rect(e, lg as u32, g as u32);
                    let (x, y) = (x as i64, y as i64);
                    add(x, y, x + w as i64, y + h as i64);
                }
            }
        }
        if b[0] > b[2] {
            return Ok(None);
        }
        let step = step as i64;
        let x0 = (b[0] / g) / step * step;
        let y0 = (b[1] / g) / step * step;
        let x1 = (b[2] + g - 1) / g;
        let y1 = (b[3] + g - 1) / g;
        Ok(Some([x0 as u32, y0 as u32, (x1 - x0) as u32, (y1 - y0) as u32]))
    }

    /// The edit that crops a level to [`Self::content_bounds`]: a `batch`
    /// labelled `Crop level <slug>` of one or two `resize_level`s (the
    /// first trims the left and top with the far edges anchored, the second
    /// the right and bottom with the near edges anchored). `None` when the
    /// level already fits its content. A level with no content is an
    /// error.
    pub fn crop_level(&self, world: &Slug, level: &Slug) -> Result<Option<Command>> {
        let l = self.level_at(self.find_level_in(world, level)?);
        let Some([x, y, w, h]) = self.content_bounds(l)? else {
            return Err(CommandError::Invalid(format!(
                "level {level} has no content to crop to"
            )));
        };
        let mut commands = Vec::new();
        let mut size = l.size;
        if x > 0 || y > 0 {
            size = [size[0] - x, size[1] - y];
            commands.push(Command::ResizeLevel {
                world: world.clone(),
                level: level.clone(),
                size,
                anchor: [if x > 0 { 1 } else { -1 }, if y > 0 { 1 } else { -1 }],
            });
        }
        if size != [w, h] {
            commands.push(Command::ResizeLevel {
                world: world.clone(),
                level: level.clone(),
                size: [w, h],
                anchor: [-1, -1],
            });
        }
        Ok((!commands.is_empty()).then(|| Command::Batch {
            label: format!("Crop level {level}"),
            commands,
        }))
    }
}
