//! Non-rectangular levels: `level.json` `void`, rectangles of project-grid
//! cells cut out of the level's box.
//!
//! A cell is **in the level** when it is inside `size` and in no void rect.
//! A layer with its own grid size has a cell void when every project cell it
//! covers (inside the box) is void, so a coarse layer keeps a cell the void
//! only nicks. Layer files stay dense; void cells are simply empty.
//!
//! Everything here is cheap to call on a level without void: the helpers
//! return `None` or the box itself, so callers keep their rectangle paths.

use crate::command::Rect;

use super::{LayerDef, Level, Project};

/// The glyph void cells show as in the text view (and that `grid.set_cells`
/// text blocks read as "leave this cell alone"): `*`, which no layer value
/// can use.
pub const VOID_GLYPH: char = '*';

impl Level {
    /// Whether any void rect is set.
    pub fn has_void(&self) -> bool {
        !self.void.is_empty()
    }

    /// Whether project-grid cell `[x, y]` lies in a void rect (the box is not
    /// checked: a cell outside the box is out of the level either way).
    pub fn is_void(&self, x: i64, y: i64) -> bool {
        self.void.iter().any(|r| rect_has(r, x, y))
    }

    /// Whether project-grid cell `[x, y]` is part of the level: inside the
    /// box and in no void rect.
    pub fn in_level(&self, x: i64, y: i64) -> bool {
        x >= 0
            && y >= 0
            && x < self.size[0] as i64
            && y < self.size[1] as i64
            && !self.is_void(x, y)
    }

    /// The level's cells of the project grid, row-major, `true` for a cell
    /// in the level.
    pub fn cell_mask(&self) -> Vec<bool> {
        cell_mask_of(self.size, &self.void)
    }

    /// How many project-grid cells the level has (its box minus the void).
    pub fn cell_count(&self) -> usize {
        self.cell_mask().iter().filter(|&&c| c).count()
    }

    /// The 4-connected pieces of the level's cells: each piece's cells as
    /// row-major indices into [`Self::cell_mask`], pieces in row-major order
    /// of their first cell. One piece for any valid level.
    pub fn pieces(&self) -> Vec<Vec<usize>> {
        components(
            &self.cell_mask(),
            self.size[0] as usize,
            self.size[1] as usize,
        )
    }

    /// The level's outline in world pixels: the outer boundary of its
    /// cells, clockwise (on screen, y down), starting at the left end of the
    /// topmost edge, one point per corner (no collinear points). A level
    /// without void gives its box's four corners. A void rect the level
    /// encloses (a hole) is not part of the outline; the cells of a level
    /// the void splits into pieces (a validation problem) give the outline
    /// of the first piece.
    pub fn outline(&self, grid: u32) -> Vec<[i64; 2]> {
        let g = grid as i64;
        let [ox, oy] = self.world_px;
        trace_outline(
            &self.cell_mask(),
            self.size[0] as usize,
            self.size[1] as usize,
        )
        .into_iter()
        .map(|[x, y]| [ox + x * g, oy + y * g])
        .collect()
    }
}

/// Whether a rect holds cell `[x, y]`.
fn rect_has(r: &Rect, x: i64, y: i64) -> bool {
    x >= r.x as i64 && y >= r.y as i64 && x < r.x as i64 + r.w as i64 && y < r.y as i64 + r.h as i64
}

impl Project {
    /// The void of `level` in cells of `def`'s grid, row-major
    /// ([`Project::layer_cells`]), `true` for a void cell; `None` when the
    /// level has no void (every cell is in the level). A layer cell is void
    /// when every project cell it covers inside the box is.
    pub fn layer_void(&self, def: &LayerDef, level: &Level) -> Option<Vec<bool>> {
        self.grid_void(self.layer_grid(def), level.size, &level.void)
    }

    /// [`Self::layer_void`] for a grid of `lg` pixels over a level of
    /// `size` project cells with `void` (which needn't be stored yet).
    pub fn grid_void(&self, lg: u32, size: [u32; 2], void: &[Rect]) -> Option<Vec<bool>> {
        if void.is_empty() {
            return None;
        }
        let g = self.grid_size.max(1) as u64;
        let lg = lg.max(1) as u64;
        let [pw, ph] = [size[0] as u64 * g, size[1] as u64 * g];
        let [w, h] = [pw.div_ceil(lg), ph.div_ceil(lg)];
        let mask = cell_mask_of(size, void);
        let pwc = size[0] as usize;
        // Project cells `[c0, c1)` a layer cell `i` covers on one axis.
        let span = |i: u64, px: u64| {
            let (a, b) = (i * lg, ((i + 1) * lg).min(px));
            ((a / g) as usize, b.div_ceil(g) as usize)
        };
        let mut out = Vec::with_capacity((w * h) as usize);
        for y in 0..h {
            let (r0, r1) = span(y, ph);
            for x in 0..w {
                let (c0, c1) = span(x, pw);
                out.push((r0..r1).all(|r| (c0..c1).all(|c| !mask[r * pwc + c])));
            }
        }
        Some(out)
    }

    /// Whether cell `[x, y]` of `def`'s grid is void in `level` (see
    /// [`Self::layer_void`]). Cells outside the layer are not void (they
    /// are out of bounds, which callers check on their own).
    pub fn layer_cell_void(&self, def: &LayerDef, level: &Level, x: i64, y: i64) -> bool {
        self.cell_void_of(self.layer_grid(def), level.size, &level.void, x, y)
    }

    /// [`Self::layer_cell_void`] for a grid of `lg` pixels over a level of
    /// `size` project cells with `void`.
    pub fn cell_void_of(&self, lg: u32, size: [u32; 2], void: &[Rect], x: i64, y: i64) -> bool {
        if void.is_empty() || x < 0 || y < 0 {
            return false;
        }
        let g = self.grid_size.max(1) as i64;
        let lg = lg.max(1) as i64;
        let [pw, ph] = [size[0] as i64 * g, size[1] as i64 * g];
        let (px0, py0) = (x * lg, y * lg);
        if px0 >= pw || py0 >= ph {
            return false;
        }
        let (px1, py1) = (((x + 1) * lg).min(pw), ((y + 1) * lg).min(ph));
        let (c0, c1) = (px0 / g, (px1 + g - 1) / g);
        let (r0, r1) = (py0 / g, (py1 + g - 1) / g);
        (r0..r1).all(|r| (c0..c1).all(|c| void.iter().any(|v| rect_has(v, c, r))))
    }
}

/// The cells of a `size` box (project grid), row-major, `true` for a cell
/// in no rect of `void`.
pub fn cell_mask_of(size: [u32; 2], void: &[Rect]) -> Vec<bool> {
    let [w, h] = [size[0] as usize, size[1] as usize];
    let mut m = vec![true; w * h];
    for r in void {
        let x0 = (r.x as usize).min(w);
        let x1 = (r.x as usize).saturating_add(r.w as usize).min(w);
        let y1 = (r.y as usize).saturating_add(r.h as usize).min(h);
        for y in (r.y as usize).min(h)..y1 {
            m[y * w + x0..y * w + x1].fill(false);
        }
    }
    m
}

/// 4-connected components of the `true` cells of a `w` x `h` mask.
pub(crate) fn components(mask: &[bool], w: usize, h: usize) -> Vec<Vec<usize>> {
    let mut seen = vec![false; mask.len()];
    let mut out = Vec::new();
    for start in 0..mask.len() {
        if !mask[start] || seen[start] {
            continue;
        }
        seen[start] = true;
        let mut piece = Vec::new();
        let mut stack = vec![start];
        while let Some(i) = stack.pop() {
            piece.push(i);
            let (x, y) = (i % w, i / w);
            let mut visit = |j: usize| {
                if mask[j] && !seen[j] {
                    seen[j] = true;
                    stack.push(j);
                }
            };
            if x > 0 {
                visit(i - 1);
            }
            if x + 1 < w {
                visit(i + 1);
            }
            if y > 0 {
                visit(i - w);
            }
            if y + 1 < h {
                visit(i + w);
            }
        }
        piece.sort_unstable();
        out.push(piece);
    }
    out
}

/// The outer boundary of the `true` cells of a `w` x `h` mask, in cell
/// corners: clockwise on screen (y down, the cells on the right of each
/// step), from the top-left corner of the first cell in row-major order,
/// corners only. Empty for an empty mask. Where two cells touch only at a
/// corner, the trace keeps them apart (turns right first).
pub(crate) fn trace_outline(mask: &[bool], w: usize, h: usize) -> Vec<[i64; 2]> {
    let Some(first) = mask.iter().position(|&c| c) else {
        return Vec::new();
    };
    let inside = |x: i64, y: i64| {
        x >= 0
            && y >= 0
            && (x as usize) < w
            && (y as usize) < h
            && mask[y as usize * w + x as usize]
    };
    // Directions: 0 east, 1 south, 2 west, 3 north (y down).
    const D: [[i64; 2]; 4] = [[1, 0], [0, 1], [-1, 0], [0, -1]];
    // Whether the unit edge leaving corner `p` in direction `d` is on the
    // boundary with the level's cells on its right.
    let edge = |p: [i64; 2], d: usize| -> bool {
        let [x, y] = p;
        // The cells left and right of the step, by the corner and direction.
        let (right, left) = match d {
            0 => ((x, y), (x, y - 1)),
            1 => ((x - 1, y), (x, y)),
            2 => ((x - 1, y - 1), (x - 1, y)),
            _ => ((x, y - 1), (x - 1, y - 1)),
        };
        inside(right.0, right.1) && !inside(left.0, left.1)
    };
    let start = [(first % w) as i64, (first / w) as i64];
    let mut out = vec![start];
    let (mut p, mut d) = (start, 0usize);
    let limit = 4 * (w + 1) * (h + 1) + 4;
    for _ in 0..limit {
        p = [p[0] + D[d][0], p[1] + D[d][1]];
        if p == start {
            break;
        }
        // Right turn first, then straight, then left.
        let next = [(d + 1) % 4, d, (d + 3) % 4]
            .into_iter()
            .find(|&nd| edge(p, nd))
            .unwrap_or(d);
        if next != d {
            out.push(p);
        }
        d = next;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::slug;

    fn level(size: [u32; 2], void: &[[u32; 4]]) -> Level {
        let mut l = Level::new(slug("a"), "A", size);
        l.void = void.iter().map(|&r| Rect::from(r)).collect();
        l
    }

    #[test]
    fn a_box_outline_is_its_corners() {
        let l = level([4, 3], &[]);
        assert_eq!(l.outline(16), [[0, 0], [64, 0], [64, 48], [0, 48]]);
    }

    #[test]
    fn an_l_outline_goes_round_the_notch() {
        // ##..
        // ##..
        // ####
        let mut l = level([4, 3], &[[2, 0, 2, 2]]);
        l.world_px = [100, 200];
        assert_eq!(
            l.outline(10),
            [
                [100, 200],
                [120, 200],
                [120, 220],
                [140, 220],
                [140, 230],
                [100, 230]
            ]
        );
        assert_eq!(l.cell_count(), 8);
        assert_eq!(l.pieces().len(), 1);
        assert!(!l.in_level(2, 0) && l.in_level(2, 2) && !l.in_level(4, 2));
    }

    #[test]
    fn the_outline_starts_at_the_top_edge_left_end() {
        // ..##
        // ####
        let l = level([4, 2], &[[0, 0, 2, 1]]);
        assert_eq!(
            l.outline(1),
            [[2, 0], [4, 0], [4, 2], [0, 2], [0, 1], [2, 1]]
        );
    }

    #[test]
    fn a_hole_is_not_in_the_outline_and_corner_contact_splits() {
        let l = level([3, 3], &[[1, 1, 1, 1]]);
        assert_eq!(l.outline(1), [[0, 0], [3, 0], [3, 3], [0, 3]]);
        assert_eq!(l.pieces().len(), 1);
        // #.
        // .#
        let l = level([2, 2], &[[1, 0, 1, 1], [0, 1, 1, 1]]);
        assert_eq!(l.pieces().len(), 2);
        assert_eq!(l.outline(1), [[0, 0], [1, 0], [1, 1], [0, 1]]);
    }

    #[test]
    fn layer_cells_are_void_when_all_their_pixels_are() {
        let mut p = Project::new("t");
        p.grid_size = 16;
        let l = level([4, 2], &[[0, 0, 1, 2], [2, 0, 1, 1]]);
        // A 32 px layer: cell [0, 0] covers project cells x 0..2, y 0..2.
        let coarse = p.grid_void(32, l.size, &l.void).unwrap();
        assert_eq!(coarse, [false, false]);
        let fine = p.grid_void(8, l.size, &l.void).unwrap();
        assert_eq!(fine.len(), 8 * 4);
        assert!(fine[0] && fine[1] && !fine[2] && fine[4] && !fine[6]);
        assert!(p.grid_void(16, [2, 2], &[]).is_none());
        let def = crate::model::LayerDef::new(
            slug("g"),
            "G",
            crate::model::LayerKind::IntGrid(Default::default()),
        );
        assert!(p.layer_cell_void(&def, &l, 0, 1));
        assert!(!p.layer_cell_void(&def, &l, 1, 1));
        assert!(p.layer_cell_void(&def, &l, 2, 0));
        assert!(!p.layer_cell_void(&def, &l, 2, 1));
    }
}
