//! Non-rectangular levels: `set_level_void`, and the void checks the level
//! and entity commands share.
//!
//! Void cells read as outside the level: no content belongs there. The
//! commands that write a value over an area (`fill_rect`, `flood_fill`,
//! `replace_value`) skip them, `add_entity` and `move_entity` refuse them,
//! and `set_level_void` clears what falls in the new void. `paint_cells` and
//! `set_tiles` write exactly the cells they name (they are what undo uses);
//! the editor's tools and the API's grid and tile writes drop void cells
//! before building them, as they clip at the level's edge.

use std::collections::BTreeSet;

use super::{LevelAt, Result, noop};
use crate::command::{
    Applied, CellValue, Change, Command, CommandError, GridValue, LevelPart, Rect,
};
use crate::id::Slug;
use crate::model::{LayerDef, Level, Project, cell_mask_of};

impl Project {
    /// Refuse cell `cell` of `def`'s grid when it is void in the level at
    /// `at` (an entity's cell).
    pub(super) fn check_not_void(
        &self,
        def: &LayerDef,
        at: LevelAt,
        layer: &Slug,
        cell: [i32; 2],
    ) -> Result<()> {
        let l = self.level_at(at);
        if self.layer_cell_void(def, l, cell[0] as i64, cell[1] as i64) {
            return Err(CommandError::Invalid(format!(
                "cell [{}, {}] of layer {layer} is in the void of level {} (cut out of its \
                 shape); pick a cell of the level",
                cell[0], cell[1], l.slug
            )));
        }
        Ok(())
    }

    /// Whether an entity of `layer` at `cell` sits in the void of the level
    /// at `at` (false for an unknown layer).
    pub(super) fn entity_cell_void(&self, layer: &Slug, at: LevelAt, cell: [i32; 2]) -> bool {
        self.layer(layer.as_str()).is_some_and(|d| {
            self.layer_cell_void(d, self.level_at(at), cell[0] as i64, cell[1] as i64)
        })
    }

    pub(super) fn set_level_void(
        &mut self,
        level: Slug,
        void: Vec<Rect>,
        restore: bool,
    ) -> Result<Applied> {
        let at = self.find_level(&level)?;
        let l = self.level_at(at);
        if l.void == void {
            return Ok(noop());
        }
        let mut clears = Vec::new();
        if !restore {
            check_void(l.size, &void)?;
            self.check_entities_out_of(l, &void)?;
            clears = self.clears_for(l, &void);
        }
        let old = std::mem::replace(&mut self.level_at_mut(at).void, void);
        let world = self.worlds[at.0].slug.clone();
        let mut changes = vec![Change::LevelMeta {
            world,
            level: level.clone(),
            part: LevelPart::Void,
        }];
        let mut inverses = Vec::new();
        for c in clears {
            match self.apply_inner(c) {
                Ok(a) => {
                    changes.extend(a.changes);
                    inverses.push(a.inverse);
                }
                Err(e) => {
                    for inv in inverses.into_iter().rev() {
                        let _ = self.apply_inner(inv);
                    }
                    self.level_at_mut(at).void = old;
                    return Err(e);
                }
            }
        }
        let back = Command::SetLevelVoid {
            level,
            void: old,
            restore: true,
        };
        // Content back first, then the old void: redoing that (its inverse)
        // sets the void, then clears, in the order this did.
        let inverse = if inverses.is_empty() {
            back
        } else {
            let mut commands: Vec<Command> = inverses.into_iter().rev().collect();
            commands.push(back);
            Command::Batch {
                label: String::new(),
                commands,
            }
        };
        Ok(Applied { inverse, changes })
    }

    /// Refuse a void that would hold one of the level's entities.
    pub(super) fn check_entities_out_of(&self, l: &Level, void: &[Rect]) -> Result<()> {
        for el in &l.entities {
            let Some(def) = self.layer(el.layer.as_str()) else {
                continue;
            };
            let lg = self.layer_grid(def);
            for e in &el.entities {
                if self.cell_void_of(lg, l.size, void, e.cell[0] as i64, e.cell[1] as i64) {
                    return Err(CommandError::Invalid(format!(
                        "entity {} ({}) at [{}, {}] of layer {} would be in the void of level \
                         {}; move or remove it first",
                        e.id, e.ty, e.cell[0], e.cell[1], el.layer, l.slug
                    )));
                }
            }
        }
        Ok(())
    }

    /// The commands clearing what of `l`'s grid cells, derived overrides
    /// and tiles lies in `void`: a `paint_cells` per int_grid layer, a
    /// `set_tiles` per tiles layer. Layers whose stored size doesn't fit
    /// the level are left alone (validation reports them).
    pub(super) fn clears_for(&self, l: &Level, void: &[Rect]) -> Vec<Command> {
        let mut out = Vec::new();
        let planes = l
            .int_grids
            .iter()
            .map(|g| (&g.layer, g.size(), g.cells().to_vec()))
            .chain(l.overrides.iter().map(|o| {
                let [w, h] = o.size();
                let codes = (0..h)
                    .flat_map(|y| (0..w).map(move |x| (x, y)))
                    .map(|(x, y)| o.code_at(x, y))
                    .collect();
                (&o.layer, o.size(), codes)
            }));
        for (layer, size, codes) in planes {
            let Some(def) = self.layer(layer.as_str()).filter(|d| d.is_int_grid()) else {
                continue;
            };
            if size != self.layer_cells(def, l) {
                continue;
            }
            let Some(mask) = self.grid_void(self.layer_grid(def), l.size, void) else {
                continue;
            };
            let cells: Vec<CellValue> = codes
                .iter()
                .enumerate()
                .filter(|&(i, &c)| c != 0 && mask[i])
                .map(|(i, _)| {
                    let i = i as u32;
                    CellValue::new(i % size[0], i / size[0], GridValue::Clear)
                })
                .collect();
            if !cells.is_empty() {
                out.push(Command::PaintCells {
                    level: l.slug.clone(),
                    layer: layer.clone(),
                    cells,
                    keep_empty: false,
                });
            }
        }
        for t in &l.tiles {
            let Some(def) = self.layer(t.layer.as_str()).filter(|d| d.is_tiles()) else {
                continue;
            };
            let size = self.layer_cells(def, l);
            let Some(mask) = self.grid_void(self.layer_grid(def), l.size, void) else {
                continue;
            };
            let clear: BTreeSet<[u32; 2]> = t
                .tiles
                .iter()
                .map(|p| p.cell)
                .filter(|c| c[0] < size[0] && c[1] < size[1])
                .filter(|c| mask[(c[1] * size[0] + c[0]) as usize])
                .collect();
            if !clear.is_empty() {
                out.push(Command::SetTiles {
                    level: l.slug.clone(),
                    layer: t.layer.clone(),
                    tiles: Vec::new(),
                    clear: clear.into_iter().collect(),
                    keep_empty: false,
                });
            }
        }
        out
    }
}

impl Project {
    /// Drop `level`'s grid cells, derived overrides and tiles that lie in its
    /// void (a new level's content, before it goes in); layers left empty go.
    pub(super) fn clip_to_shape(&self, level: &mut Level) {
        let void = level.void.clone();
        let mask_of = |slug: &Slug| {
            self.layer(slug.as_str())
                .and_then(|d| self.grid_void(self.layer_grid(d), level.size, &void))
        };
        let masks: Vec<_> = level.int_grids.iter().map(|g| mask_of(&g.layer)).collect();
        for (g, m) in level.int_grids.iter_mut().zip(masks) {
            let [w, h] = g.size();
            for (i, _) in m.iter().flatten().enumerate().filter(|(_, v)| **v) {
                let (x, y) = (i as u32 % w.max(1), i as u32 / w.max(1));
                if y < h {
                    g.set_index(x, y, 0);
                }
            }
        }
        level.int_grids.retain(|g| !g.is_empty());
        let masks: Vec<_> = level.overrides.iter().map(|o| mask_of(&o.layer)).collect();
        for (o, m) in level.overrides.iter_mut().zip(masks) {
            let [w, h] = o.size();
            for (i, _) in m.iter().flatten().enumerate().filter(|(_, v)| **v) {
                let (x, y) = (i as u32 % w.max(1), i as u32 / w.max(1));
                if y < h {
                    o.set_code(x, y, 0);
                }
            }
        }
        level.overrides.retain(|o| !o.is_empty());
        let masks: Vec<_> = level
            .tiles
            .iter()
            .map(|t| {
                let size = self
                    .layer(t.layer.as_str())
                    .map(|d| self.layer_cells(d, level));
                (mask_of(&t.layer), size)
            })
            .collect();
        for (t, (m, size)) in level.tiles.iter_mut().zip(masks) {
            if let (Some(m), Some([w, h])) = (m, size) {
                t.tiles.retain(|p| {
                    p.cell[0] >= w || p.cell[1] >= h || !m[(p.cell[1] * w + p.cell[0]) as usize]
                });
            }
        }
        level.tiles.retain(|t| !t.tiles.is_empty());
    }
}

/// Refuse a void list that doesn't fit a level of `size` cells: an empty
/// rect, one reaching outside the box, or no cell left.
pub(crate) fn check_void(size: [u32; 2], void: &[Rect]) -> Result<()> {
    for (i, r) in void.iter().enumerate() {
        let shown = [r.x, r.y, r.w, r.h];
        if r.is_empty() {
            return Err(CommandError::Invalid(format!(
                "void[{i}] {shown:?} is empty; a void rect is [x, y, width, height] with a width \
                 and height of at least 1"
            )));
        }
        if r.x as u64 + r.w as u64 > size[0] as u64 || r.y as u64 + r.h as u64 > size[1] as u64 {
            return Err(CommandError::Invalid(format!(
                "void[{i}] {shown:?} reaches outside the level's {}x{} cells",
                size[0], size[1]
            )));
        }
    }
    if !void.is_empty() && !cell_mask_of(size, void).contains(&true) {
        return Err(CommandError::Invalid(
            "the void covers the whole level; a level needs at least one cell".into(),
        ));
    }
    Ok(())
}
