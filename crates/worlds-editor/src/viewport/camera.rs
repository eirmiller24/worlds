//! Camera: world pixels <-> physical screen pixels.
//!
//! The camera keeps `origin`, the world point at the viewport's top-left
//! corner, and `zoom`, physical pixels per world pixel. Screen positions here
//! are physical pixels relative to the viewport's top-left. The renderer
//! uses [`Camera::snapped_offset`], which rounds the origin to a whole
//! physical pixel, so at integer zoom every world pixel lands on exactly
//! `zoom` screen pixels and pixel art stays crisp.

use serde::{Deserialize, Serialize};

/// Zoom levels the wheel steps through (physical px per world px). Above 1
/// they are integers, so pixel art is never resampled unevenly.
pub const ZOOM_STEPS: &[f64] = &[
    1.0 / 256.0,
    1.0 / 128.0,
    1.0 / 64.0,
    1.0 / 48.0,
    1.0 / 32.0,
    1.0 / 24.0,
    1.0 / 16.0,
    1.0 / 12.0,
    1.0 / 8.0,
    1.0 / 6.0,
    1.0 / 4.0,
    1.0 / 3.0,
    1.0 / 2.0,
    2.0 / 3.0,
    1.0,
    2.0,
    3.0,
    4.0,
    5.0,
    6.0,
    8.0,
    10.0,
    12.0,
    16.0,
];

/// The smallest zoom a fit may pick below [`ZOOM_STEPS`]: a world a
/// million times wider than the viewport still fits.
pub const MIN_ZOOM: f64 = 1e-6;

/// A 2D axis-aligned rectangle in world pixels (or any f64 space).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WRect {
    pub min: [f64; 2],
    pub max: [f64; 2],
}

impl WRect {
    pub fn new(min: [f64; 2], max: [f64; 2]) -> Self {
        WRect { min, max }
    }

    pub fn from_pos_size(pos: [f64; 2], size: [f64; 2]) -> Self {
        WRect {
            min: pos,
            max: [pos[0] + size[0], pos[1] + size[1]],
        }
    }

    /// An empty rect that any `union` replaces.
    pub const EMPTY: WRect = WRect {
        min: [f64::INFINITY, f64::INFINITY],
        max: [f64::NEG_INFINITY, f64::NEG_INFINITY],
    };

    pub fn is_empty(&self) -> bool {
        self.min[0] >= self.max[0] || self.min[1] >= self.max[1]
    }

    pub fn width(&self) -> f64 {
        self.max[0] - self.min[0]
    }

    pub fn height(&self) -> f64 {
        self.max[1] - self.min[1]
    }

    pub fn center(&self) -> [f64; 2] {
        [
            (self.min[0] + self.max[0]) * 0.5,
            (self.min[1] + self.max[1]) * 0.5,
        ]
    }

    /// Open-interval overlap test: rects that only touch don't intersect.
    pub fn intersects(&self, o: &WRect) -> bool {
        self.min[0] < o.max[0]
            && o.min[0] < self.max[0]
            && self.min[1] < o.max[1]
            && o.min[1] < self.max[1]
    }

    pub fn contains(&self, p: [f64; 2]) -> bool {
        p[0] >= self.min[0] && p[0] < self.max[0] && p[1] >= self.min[1] && p[1] < self.max[1]
    }

    pub fn union(&self, o: &WRect) -> WRect {
        WRect {
            min: [self.min[0].min(o.min[0]), self.min[1].min(o.min[1])],
            max: [self.max[0].max(o.max[0]), self.max[1].max(o.max[1])],
        }
    }

    pub fn expand(&self, by: f64) -> WRect {
        WRect {
            min: [self.min[0] - by, self.min[1] - by],
            max: [self.max[0] + by, self.max[1] + by],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Camera {
    /// World pixel at the viewport's top-left corner.
    pub origin: [f64; 2],
    /// Physical screen pixels per world pixel.
    pub zoom: f64,
}

impl Default for Camera {
    fn default() -> Self {
        Camera {
            origin: [0.0, 0.0],
            zoom: 1.0,
        }
    }
}

impl Camera {
    pub fn world_to_screen(&self, w: [f64; 2]) -> [f64; 2] {
        [
            (w[0] - self.origin[0]) * self.zoom,
            (w[1] - self.origin[1]) * self.zoom,
        ]
    }

    pub fn screen_to_world(&self, s: [f64; 2]) -> [f64; 2] {
        [
            s[0] / self.zoom + self.origin[0],
            s[1] / self.zoom + self.origin[1],
        ]
    }

    /// The origin in screen pixels, rounded to a whole physical pixel. The
    /// renderer draws world point `w` at `w * zoom - snapped_offset()`.
    pub fn snapped_offset(&self) -> [f64; 2] {
        [
            (self.origin[0] * self.zoom).round(),
            (self.origin[1] * self.zoom).round(),
        ]
    }

    /// Like [`Self::world_to_screen`], but with the same pixel snapping the
    /// renderer uses, so overlays line up with tiles exactly.
    pub fn world_to_screen_snapped(&self, w: [f64; 2]) -> [f64; 2] {
        let o = self.snapped_offset();
        [w[0] * self.zoom - o[0], w[1] * self.zoom - o[1]]
    }

    /// The inverse of [`Self::world_to_screen_snapped`]: the world point
    /// drawn at screen point `s`. Pointer hit tests use this, so the cell
    /// under the cursor is the cell drawn under it at any zoom.
    pub fn screen_to_world_snapped(&self, s: [f64; 2]) -> [f64; 2] {
        let o = self.snapped_offset();
        [(s[0] + o[0]) / self.zoom, (s[1] + o[1]) / self.zoom]
    }

    /// The world rect visible in a viewport `size` physical pixels big.
    pub fn visible(&self, size: [f64; 2]) -> WRect {
        WRect::new(self.origin, self.screen_to_world(size))
    }

    /// Move by a screen-pixel delta (content follows the pointer).
    pub fn pan_screen(&mut self, delta: [f64; 2]) {
        self.origin[0] -= delta[0] / self.zoom;
        self.origin[1] -= delta[1] / self.zoom;
    }

    /// Set the zoom, keeping the world point under screen point `anchor`
    /// fixed. Off-grid zooms are kept (down to [`MIN_ZOOM`]): only a fit
    /// makes one, and the wheel steps back onto the grid from there.
    pub fn zoom_at(&mut self, anchor: [f64; 2], zoom: f64) {
        let w = self.screen_to_world(anchor);
        self.zoom = zoom.clamp(MIN_ZOOM, ZOOM_STEPS[ZOOM_STEPS.len() - 1]);
        self.origin = [w[0] - anchor[0] / self.zoom, w[1] - anchor[1] / self.zoom];
    }

    /// Step the zoom `steps` notches (positive = in) through [`ZOOM_STEPS`],
    /// keeping the point under `anchor` fixed. A zoom between two steps goes
    /// to the next one in that direction.
    pub fn zoom_steps(&mut self, anchor: [f64; 2], steps: i32) {
        let z = step_zoom(self.zoom, steps);
        self.zoom_at(anchor, z);
    }

    /// Frame `rect` in a viewport of `size` physical px with `margin` px
    /// around it, at the largest zoom step that fits, centered. A rect too
    /// big for the smallest step gets the exact zoom that fits it (an
    /// off-grid zoom, at least [`MIN_ZOOM`]).
    pub fn fit(&mut self, rect: WRect, size: [f64; 2], margin: f64) {
        if rect.is_empty() || size[0] <= 0.0 || size[1] <= 0.0 {
            return;
        }
        let avail = [
            (size[0] - 2.0 * margin).max(1.0),
            (size[1] - 2.0 * margin).max(1.0),
        ];
        let z = (avail[0] / rect.width()).min(avail[1] / rect.height());
        self.zoom = if z < ZOOM_STEPS[0] {
            z.max(MIN_ZOOM)
        } else {
            snap_down(z)
        };
        let c = rect.center();
        self.origin = [
            c[0] - size[0] * 0.5 / self.zoom,
            c[1] - size[1] * 0.5 / self.zoom,
        ];
    }
}

/// The largest zoom step at or below `z` (the smallest step if none).
pub fn snap_down(z: f64) -> f64 {
    ZOOM_STEPS
        .iter()
        .rev()
        .copied()
        .find(|&s| s <= z * (1.0 + 1e-9))
        .unwrap_or(ZOOM_STEPS[0])
}

/// The zoom `steps` notches away from `z`. From an off-grid zoom the first
/// notch lands on the nearest step in that direction; zooming out below
/// the smallest step (only a fit gets there) keeps the zoom.
pub fn step_zoom(z: f64, steps: i32) -> f64 {
    let eps = 1e-9;
    if steps < 0 && z < ZOOM_STEPS[0] * (1.0 - eps) {
        return z;
    }
    let mut i = if steps > 0 {
        // Last step <= z: one notch in from there.
        ZOOM_STEPS
            .iter()
            .rposition(|&s| s <= z * (1.0 + eps))
            .map_or(-1, |i| i as i64)
    } else {
        // First step >= z.
        ZOOM_STEPS
            .iter()
            .position(|&s| s >= z * (1.0 - eps))
            .map_or(ZOOM_STEPS.len() as i64, |i| i as i64)
    };
    i += steps as i64;
    ZOOM_STEPS[i.clamp(0, ZOOM_STEPS.len() as i64 - 1) as usize]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: [f64; 2], b: [f64; 2]) -> bool {
        (a[0] - b[0]).abs() < 1e-6 && (a[1] - b[1]).abs() < 1e-6
    }

    #[test]
    fn screen_world_roundtrip() {
        let cam = Camera {
            origin: [-123.5, 900.25],
            zoom: 3.0,
        };
        for p in [[0.0, 0.0], [10.0, -4.0], [1e5, 3.25]] {
            assert!(close(cam.screen_to_world(cam.world_to_screen(p)), p));
            assert!(close(cam.world_to_screen(cam.screen_to_world(p)), p));
        }
        assert_eq!(cam.world_to_screen(cam.origin), [0.0, 0.0]);
    }

    #[test]
    fn zoom_keeps_point_under_cursor() {
        let mut cam = Camera {
            origin: [100.0, 50.0],
            zoom: 1.0,
        };
        let anchor = [317.0, 211.0];
        let before = cam.screen_to_world(anchor);
        for steps in [1, 1, 3, -2, -5, -7, 9] {
            cam.zoom_steps(anchor, steps);
            assert!(close(cam.screen_to_world(anchor), before), "{steps}");
            assert!(ZOOM_STEPS.contains(&cam.zoom));
        }
    }

    #[test]
    fn steps_walk_the_table() {
        assert_eq!(step_zoom(1.0, 1), 2.0);
        assert_eq!(step_zoom(1.0, -1), 2.0 / 3.0);
        // Between steps: in goes to the next step up, out to the next down.
        assert_eq!(step_zoom(2.5, 1), 3.0);
        assert_eq!(step_zoom(2.5, -1), 2.0);
        // Clamped at both ends.
        assert_eq!(step_zoom(16.0, 5), 16.0);
        assert_eq!(step_zoom(ZOOM_STEPS[0], -3), ZOOM_STEPS[0]);
        assert_eq!(ZOOM_STEPS[0], 1.0 / 256.0);
        assert_eq!(ZOOM_STEPS[1], 1.0 / 128.0);
    }

    #[test]
    fn below_the_smallest_step() {
        // In from an off-grid zoom: the smallest step, then on up the table.
        let z = 1.0 / 1000.0;
        assert_eq!(step_zoom(z, 1), 1.0 / 256.0);
        assert_eq!(step_zoom(z, 2), 1.0 / 128.0);
        // Out: there is no step below, so the zoom stays.
        assert_eq!(step_zoom(z, -1), z);
        let mut cam = Camera {
            origin: [0.0, 0.0],
            zoom: z,
        };
        let anchor = [400.0, 300.0];
        let before = cam.screen_to_world(anchor);
        cam.zoom_steps(anchor, -1);
        assert_eq!(cam.zoom, z);
        cam.zoom_steps(anchor, 1);
        assert_eq!(cam.zoom, 1.0 / 256.0);
        assert!(close(cam.screen_to_world(anchor), before));
    }

    #[test]
    fn fit_goes_below_the_smallest_step() {
        // 1000 viewports wide: no step fits, so the fit's zoom is exact.
        let mut cam = Camera::default();
        let size = [1000.0, 800.0];
        let r = WRect::from_pos_size([-5e5, 0.0], [1e6, 1e3]);
        cam.fit(r, size, 0.0);
        assert!((cam.zoom - 1e-3).abs() < 1e-12, "{}", cam.zoom);
        assert!(!ZOOM_STEPS.contains(&cam.zoom));
        let v = cam.visible(size);
        assert!(v.min[0] <= r.min[0] + 1e-6 && v.max[0] >= r.max[0] - 1e-6);
        assert!(close(v.center(), r.center()));
        // Just under the smallest step's reach still snaps to a step.
        cam.fit(WRect::from_pos_size([0.0, 0.0], [1000.0 * 200.0, 10.0]), size, 0.0);
        assert_eq!(cam.zoom, 1.0 / 256.0);
        // Absurd sizes stop at MIN_ZOOM.
        cam.fit(WRect::from_pos_size([0.0, 0.0], [1e12, 1.0]), size, 0.0);
        assert_eq!(cam.zoom, MIN_ZOOM);
    }

    #[test]
    fn integer_zoom_snaps_to_whole_pixels() {
        let cam = Camera {
            origin: [10.3, -7.6],
            zoom: 2.0,
        };
        let o = cam.snapped_offset();
        assert_eq!(o, [21.0, -15.0]);
        // Every integer world pixel lands on an integer screen pixel.
        for w in [[0.0, 0.0], [17.0, -3.0], [1024.0, 4096.0]] {
            let s = cam.world_to_screen_snapped(w);
            assert_eq!(s, [s[0].round(), s[1].round()]);
        }
    }

    #[test]
    fn pointer_hits_the_cell_drawn_under_it() {
        // Fractional zoom and origin: 16 px cells are 10.67 screen px wide
        // and the origin snaps by up to half a screen pixel.
        let cam = Camera {
            origin: [3.4, -5.3],
            zoom: 2.0 / 3.0,
        };
        let g = 16.0;
        let cell = |w: [f64; 2]| [(w[0] / g).floor() as i64, (w[1] / g).floor() as i64];
        let mut unsnapped_misses = 0;
        // Pointer positions a hair inside each side of every cell edge on
        // screen, along both axes.
        for i in 1..40 {
            let edge = cam.world_to_screen_snapped([i as f64 * g, i as f64 * g]);
            for d in [-0.05, 0.05] {
                let s = [edge[0] + d, edge[1] + d];
                // The cell whose drawn (snapped) rect contains `s`.
                let drawn = [i - (d < 0.0) as i64, i - (d < 0.0) as i64];
                assert_eq!(cell(cam.screen_to_world_snapped(s)), drawn, "{s:?}");
                unsnapped_misses += (cell(cam.screen_to_world(s)) != drawn) as usize;
            }
        }
        // The plain inverse gets some of these wrong (the bug this fixes).
        assert!(unsnapped_misses > 0);
        // And the snapped pair round-trips.
        for p in [[0.0, 0.0], [17.5, -3.25], [1024.0, 4096.0]] {
            assert!(close(
                cam.screen_to_world_snapped(cam.world_to_screen_snapped(p)),
                p
            ));
        }
    }

    #[test]
    fn fit_frames_rect_centered() {
        let mut cam = Camera::default();
        let r = WRect::from_pos_size([0.0, -1920.0], [2048.0, 896.0]);
        cam.fit(r, [1600.0, 900.0], 16.0);
        assert!(ZOOM_STEPS.contains(&cam.zoom));
        let v = cam.visible([1600.0, 900.0]);
        assert!(v.min[0] <= r.min[0] && v.max[0] >= r.max[0]);
        assert!(v.min[1] <= r.min[1] && v.max[1] >= r.max[1]);
        assert!(close(v.center(), r.center()));
        // The next step up would not fit.
        assert!(
            r.width() * step_zoom(cam.zoom, 1) > 1600.0 - 32.0
                || r.height() * step_zoom(cam.zoom, 1) > 900.0 - 32.0
        );
    }

    #[test]
    fn rect_ops() {
        let a = WRect::from_pos_size([0.0, 0.0], [10.0, 10.0]);
        let b = WRect::from_pos_size([10.0, 0.0], [5.0, 5.0]);
        assert!(!a.intersects(&b), "touching edges don't intersect");
        assert!(a.expand(0.5).intersects(&b));
        assert_eq!(WRect::EMPTY.union(&a), a);
        assert!(WRect::EMPTY.is_empty());
    }
}
