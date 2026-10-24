//! World layout modes (`world.json` `layout`): how the tools place a
//! world's levels. `world_px` stays the stored truth in every mode; the mode
//! only drives the commands and the editor.
//!
//! - `free`: levels go where they are put (the default).
//! - `horizontal`, `vertical`: a strip. Levels sit in the world's level
//!   order, left to right (top to bottom), from the world origin, `gap`
//!   pixels apart, top (left) edges at 0. The commands that change the
//!   order or a size recompute every `world_px`.
//! - `grid_vania`: levels snap to a lattice of `cell` pixels and must be
//!   whole cells (validation).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// `world.json` `layout`. Left out of the file at its default (free, no
/// gap, no cell).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorldLayout {
    pub mode: LayoutMode,
    /// Strip layouts: pixels between two neighbouring levels. Default 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gap: Option<u32>,
    /// Grid vania: the lattice cell `[w, h]` in pixels, a multiple of the
    /// project grid. Default 16 project cells each way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cell: Option<[u32; 2]>,
}

/// How a world places its levels.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LayoutMode {
    /// Levels go where they are put.
    #[default]
    Free,
    /// Left to right in level order.
    Horizontal,
    /// Top to bottom in level order.
    Vertical,
    /// Levels snap to a lattice and are whole lattice cells.
    GridVania,
}

impl LayoutMode {
    pub const ALL: [LayoutMode; 4] = [
        LayoutMode::Free,
        LayoutMode::Horizontal,
        LayoutMode::Vertical,
        LayoutMode::GridVania,
    ];

    /// The name in the file (`grid_vania`).
    pub fn name(self) -> &'static str {
        match self {
            LayoutMode::Free => "free",
            LayoutMode::Horizontal => "horizontal",
            LayoutMode::Vertical => "vertical",
            LayoutMode::GridVania => "grid_vania",
        }
    }

    /// A display name ("Grid vania").
    pub fn label(self) -> &'static str {
        match self {
            LayoutMode::Free => "Free",
            LayoutMode::Horizontal => "Horizontal",
            LayoutMode::Vertical => "Vertical",
            LayoutMode::GridVania => "Grid vania",
        }
    }
}

/// The lattice cell, in project cells each way, when `cell` is left out.
pub const DEFAULT_LATTICE_CELLS: u32 = 16;

impl WorldLayout {
    pub fn new(mode: LayoutMode) -> Self {
        WorldLayout {
            mode,
            gap: None,
            cell: None,
        }
    }

    /// Free with nothing set: what a missing `layout` means.
    pub fn is_default(&self) -> bool {
        *self == WorldLayout::default()
    }

    /// `horizontal` or `vertical`: positions follow the level order.
    pub fn is_strip(&self) -> bool {
        matches!(self.mode, LayoutMode::Horizontal | LayoutMode::Vertical)
    }

    /// The gap between strip neighbours in pixels.
    pub fn gap_px(&self) -> i64 {
        self.gap.unwrap_or(0) as i64
    }

    /// The lattice cell in pixels (`cell`, else
    /// [`DEFAULT_LATTICE_CELLS`] project cells), whatever the mode.
    pub fn cell_px(&self, grid: u32) -> [u32; 2] {
        self.cell.unwrap_or([grid * DEFAULT_LATTICE_CELLS; 2])
    }

    /// The lattice levels snap to: `Some` only in grid vania.
    pub fn lattice(&self, grid: u32) -> Option<[u32; 2]> {
        (self.mode == LayoutMode::GridVania).then(|| self.cell_px(grid))
    }

    /// Where a strip layout puts levels of these pixel sizes, in order:
    /// from the world origin, `gap` apart. `None` when not a strip.
    pub fn strip_positions(&self, sizes_px: &[[i64; 2]]) -> Option<Vec<[i64; 2]>> {
        let axis = match self.mode {
            LayoutMode::Horizontal => 0,
            LayoutMode::Vertical => 1,
            _ => return None,
        };
        let mut at = 0i64;
        let mut out = Vec::with_capacity(sizes_px.len());
        for s in sizes_px {
            let mut p = [0, 0];
            p[axis] = at;
            out.push(p);
            at += s[axis] + self.gap_px();
        }
        Some(out)
    }

    /// `px` moved to the nearest lattice point (grid vania), else as is.
    pub fn snap(&self, grid: u32, px: [i64; 2]) -> [i64; 2] {
        match self.lattice(grid) {
            Some(c) => [snap_to(px[0], c[0]), snap_to(px[1], c[1])],
            None => px,
        }
    }
}

/// `v` rounded to the nearest multiple of `step` (halves away from zero
/// round up).
fn snap_to(v: i64, step: u32) -> i64 {
    let s = step.max(1) as i64;
    (v + s / 2).div_euclid(s) * s
}

/// Why a rectangle `[x, y, w, h]` (pixels) isn't made of whole cells of the
/// lattice `cell`, or `None` when it is: its corner must be on a lattice
/// point and its size a whole number of cells. Validation calls it for each
/// level of a grid-vania world; it takes any rect so a level's cut-out
/// parts can be held to the same rule.
pub fn lattice_problem(cell: [u32; 2], rect: [i64; 4]) -> Option<String> {
    let [cw, ch] = [cell[0].max(1) as i64, cell[1].max(1) as i64];
    let [x, y, w, h] = rect;
    if x.rem_euclid(cw) != 0 || y.rem_euclid(ch) != 0 {
        return Some(format!(
            "[{x}, {y}] is not on the {cw}x{ch} px lattice (nearest point [{}, {}])",
            snap_to(x, cw as u32),
            snap_to(y, ch as u32)
        ));
    }
    if w.rem_euclid(cw) != 0 || h.rem_euclid(ch) != 0 {
        return Some(format!(
            "{w}x{h} px is not a whole number of {cw}x{ch} px lattice cells"
        ));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_follow_the_order_with_gaps() {
        let mut l = WorldLayout::new(LayoutMode::Horizontal);
        l.gap = Some(16);
        let p = l.strip_positions(&[[100, 50], [20, 80]]).unwrap();
        assert_eq!(p, [[0, 0], [116, 0]]);
        l.mode = LayoutMode::Vertical;
        let p = l.strip_positions(&[[100, 50], [20, 80]]).unwrap();
        assert_eq!(p, [[0, 0], [0, 66]]);
        l.mode = LayoutMode::Free;
        assert_eq!(l.strip_positions(&[[1, 1]]), None);
    }

    #[test]
    fn the_lattice_check_takes_any_rect() {
        assert_eq!(lattice_problem([256, 128], [-256, 128, 512, 128]), None);
        assert!(
            lattice_problem([256, 128], [10, 0, 256, 128])
                .unwrap()
                .contains("nearest point [0, 0]")
        );
        assert!(
            lattice_problem([256, 128], [0, 0, 250, 128])
                .unwrap()
                .contains("whole number")
        );
    }

    #[test]
    fn snapping_rounds_to_the_nearest_point() {
        let l = WorldLayout {
            mode: LayoutMode::GridVania,
            gap: None,
            cell: Some([100, 50]),
        };
        assert_eq!(l.snap(16, [149, -26]), [100, -50]);
        assert_eq!(l.snap(16, [150, 24]), [200, 0]);
        assert_eq!(WorldLayout::default().snap(16, [3, 4]), [3, 4]);
        assert_eq!(WorldLayout::default().cell_px(16), [256, 256]);
    }
}
