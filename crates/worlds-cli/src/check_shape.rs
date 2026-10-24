//! `check-export`'s own reading of non-rectangular levels (`void`,
//! `outline`, and `touching` and `open_edges` along the outline), from the
//! plain JSON as the rest of the checker, never through `worlds_core`: the
//! outline is traced from a map of boundary edges and the seams are
//! intersected unit by unit, not the way the writer does it.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde_json::{Value, json};

/// A level's cells in world terms: its box, the project grid and which
/// project cells are in the level.
pub(crate) struct Shape {
    /// Size in project cells.
    pub w: i64,
    pub h: i64,
    /// The project grid in pixels.
    pub g: i64,
    /// Top-left corner in world pixels.
    pub origin: [i64; 2],
    /// Row-major, `true` for a cell in the level.
    inside: Vec<bool>,
    /// Whether any cell is void.
    pub shaped: bool,
}

/// Side names in the export's order, and the neighbour offset of each.
pub(crate) const SIDES: [(&str, [i64; 2]); 4] = [
    ("left", [-1, 0]),
    ("right", [1, 0]),
    ("top", [0, -1]),
    ("bottom", [0, 1]),
];

/// A `void` key: absent (`Ok(empty)`), or an array of `[x, y, w, h]`
/// integer arrays.
pub(crate) fn parse_void(v: Option<&Value>) -> Result<Vec<[i64; 4]>, String> {
    let Some(v) = v else {
        return Ok(Vec::new());
    };
    let a = v
        .as_array()
        .ok_or("expected an array of [x, y, width, height]")?;
    a.iter()
        .enumerate()
        .map(|(i, r)| {
            let r = r.as_array().filter(|r| r.len() == 4);
            let ints: Option<Vec<i64>> = r.map(|r| r.iter().filter_map(Value::as_i64).collect());
            match ints {
                Some(x) if x.len() == 4 => Ok([x[0], x[1], x[2], x[3]]),
                _ => Err(format!(
                    "void[{i}]: expected [x, y, width, height] (integers)"
                )),
            }
        })
        .collect()
}

/// The problems of a void list on a `w` x `h` box: rects that are empty or
/// reach outside, and a void that leaves no cell. As `(path, message)`.
pub(crate) fn void_problems(void: &[[i64; 4]], w: i64, h: i64) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (i, r) in void.iter().enumerate() {
        let [x, y, rw, rh] = *r;
        if rw <= 0 || rh <= 0 {
            out.push((format!("void[{i}]"), format!("{r:?} is empty")));
        } else if x < 0 || y < 0 || x + rw > w || y + rh > h {
            out.push((
                format!("void[{i}]"),
                format!("{r:?} reaches outside the level's {w}x{h} cells"),
            ));
        }
    }
    out
}

impl Shape {
    /// From a `world.json` rect (world pixels), the grid and the void.
    /// `None` when the rect isn't whole cells.
    pub fn new(rect: [i64; 4], g: i64, void: &[[i64; 4]]) -> Option<Shape> {
        let [x, y, pw, ph] = rect;
        if g <= 0 || pw <= 0 || ph <= 0 || pw % g != 0 || ph % g != 0 {
            return None;
        }
        let (w, h) = (pw / g, ph / g);
        let mut inside = vec![true; (w * h) as usize];
        for cy in 0..h {
            for cx in 0..w {
                let cut = void
                    .iter()
                    .any(|&[vx, vy, vw, vh]| cx >= vx && cx < vx + vw && cy >= vy && cy < vy + vh);
                if cut {
                    inside[(cy * w + cx) as usize] = false;
                }
            }
        }
        let shaped = inside.contains(&false);
        Some(Shape {
            w,
            h,
            g,
            origin: [x, y],
            inside,
            shaped,
        })
    }

    /// Whether project cell `[x, y]` is in the level.
    pub fn is_in(&self, x: i64, y: i64) -> bool {
        x >= 0 && y >= 0 && x < self.w && y < self.h && self.inside[(y * self.w + x) as usize]
    }

    /// Whether cell `[cx, cy]` of a `gs` px grid is void: every project cell
    /// it covers inside the box is out of the level. Cells past the box
    /// aren't void (they're out of bounds).
    pub fn void_on(&self, gs: i64, cx: i64, cy: i64) -> bool {
        let g = self.g;
        let (pw, ph) = (self.w * g, self.h * g);
        let (x0, y0) = (cx * gs, cy * gs);
        if cx < 0 || cy < 0 || x0 >= pw || y0 >= ph {
            return false;
        }
        let (x1, y1) = ((x0 + gs).min(pw), (y0 + gs).min(ph));
        let cover = |a: i64, b: i64| (a / g)..((b + g - 1) / g);
        cover(y0, y1).all(|y| cover(x0, x1).all(|x| !self.is_in(x, y)))
    }

    /// The outer boundary, clockwise on screen from the top-left-most
    /// corner, corners only, in world pixels.
    pub fn outline(&self) -> Vec<[i64; 2]> {
        // Directed unit edges with the level on their right, keyed by start.
        let mut out_edges: HashMap<[i64; 2], Vec<[i64; 2]>> = HashMap::new();
        for y in 0..self.h {
            for x in 0..self.w {
                if !self.is_in(x, y) {
                    continue;
                }
                let mut add = |a: [i64; 2], b: [i64; 2]| out_edges.entry(a).or_default().push(b);
                if !self.is_in(x, y - 1) {
                    add([x, y], [x + 1, y]);
                }
                if !self.is_in(x + 1, y) {
                    add([x + 1, y], [x + 1, y + 1]);
                }
                if !self.is_in(x, y + 1) {
                    add([x + 1, y + 1], [x, y + 1]);
                }
                if !self.is_in(x - 1, y) {
                    add([x, y + 1], [x, y]);
                }
            }
        }
        let Some(&start) = out_edges.keys().min_by_key(|p| (p[1], p[0])) else {
            return Vec::new();
        };
        // Walk, turning as far right as the edges allow where two leave one
        // corner (cells touching only at a corner stay apart).
        let dir = |a: [i64; 2], b: [i64; 2]| [b[0] - a[0], b[1] - a[1]];
        let right_of = |d: [i64; 2]| [-d[1], d[0]];
        let mut pts = vec![start];
        let mut cur = start;
        let mut d = [1, 0];
        let limit = 4 * (self.w + 1) * (self.h + 1) + 4;
        for _ in 0..limit {
            let next = cur;
            let cands = out_edges.get(&next).cloned().unwrap_or_default();
            let pick = [right_of(d), d, [-right_of(d)[0], -right_of(d)[1]]]
                .into_iter()
                .find_map(|want| cands.iter().copied().find(|&b| dir(next, b) == want));
            let Some(b) = pick else { break };
            let nd = dir(next, b);
            if nd != d && next != start {
                pts.push(next);
            }
            d = nd;
            cur = b;
            if cur == start {
                break;
            }
        }
        // A start whose first step isn't east (can't happen for the
        // top-left-most corner) would leave a collinear point; drop it.
        pts.iter()
            .map(|&[x, y]| [self.origin[0] + x * self.g, self.origin[1] + y * self.g])
            .collect()
    }

    /// Unit boundary segments per side: `(side index, line) -> starts`, in
    /// world pixels (the line is x for left/right, y for top/bottom; a
    /// segment covers `[start, start + g)` along it).
    fn units(&self) -> BTreeMap<(usize, i64), BTreeSet<i64>> {
        let mut out: BTreeMap<(usize, i64), BTreeSet<i64>> = BTreeMap::new();
        let g = self.g;
        let [ox, oy] = self.origin;
        for y in 0..self.h {
            for x in 0..self.w {
                if !self.is_in(x, y) {
                    continue;
                }
                for (s, (_, [dx, dy])) in SIDES.iter().enumerate() {
                    if self.is_in(x + dx, y + dy) {
                        continue;
                    }
                    let (line, start) = if *dy != 0 {
                        (oy + (y + i64::from(*dy > 0)) * g, ox + x * g)
                    } else {
                        (ox + (x + i64::from(*dx > 0)) * g, oy + y * g)
                    };
                    out.entry((s, line)).or_default().insert(start);
                }
            }
        }
        out
    }
}

/// `a`'s `touching` entries for `b` (`level` is `b`'s slug) when either is
/// non-rectangular: every straight stretch of `a`'s boundary lying on `b`'s
/// facing the other way, with `at`, in side order, then line, then start.
pub(crate) fn shaped_touching(a: &Shape, b: &Shape, level: &str) -> Vec<Value> {
    let (ua, ub) = (a.units(), b.units());
    let g = a.g;
    let mut out = Vec::new();
    for ((s, line), starts) in &ua {
        let Some(other) = ub.get(&(s ^ 1, *line)) else {
            continue;
        };
        let shared: Vec<i64> = starts.intersection(other).copied().collect();
        let mut i = 0;
        while i < shared.len() {
            let first = shared[i];
            let mut end = first + g;
            i += 1;
            while i < shared.len() && shared[i] == end {
                end += g;
                i += 1;
            }
            out.push(
                json!({ "level": level, "edge": SIDES[*s].0, "span": [first, end], "at": line }),
            );
        }
    }
    out
}

/// A shaped level's `open_edges`, from its collision cells (`cols` x `rows`
/// of `gs` px; `open(x, y)` for a cell holding no blocking value): runs of
/// open border cells `[first, end, at]` per side, along the outline.
pub(crate) fn shaped_open_edges(
    shape: &Shape,
    gs: i64,
    cols: i64,
    rows: i64,
    open: &dyn Fn(usize, usize) -> bool,
) -> Value {
    let inside =
        |x: i64, y: i64| x >= 0 && y >= 0 && x < cols && y < rows && !shape.void_on(gs, x, y);
    let mut out = serde_json::Map::new();
    for (name, [dx, dy]) in SIDES {
        let horizontal = dy != 0;
        let (outer, inner) = if horizontal {
            (rows, cols)
        } else {
            (cols, rows)
        };
        let mut runs = Vec::new();
        for a in 0..outer {
            let at = a + i64::from(dx > 0 || dy > 0);
            let border = |b: i64| {
                let (x, y) = if horizontal { (b, a) } else { (a, b) };
                inside(x, y) && !inside(x + dx, y + dy) && open(x as usize, y as usize)
            };
            let mut b = 0;
            while b < inner {
                if border(b) {
                    let s = b;
                    while b < inner && border(b) {
                        b += 1;
                    }
                    runs.push(json!([s, b, at]));
                } else {
                    b += 1;
                }
            }
        }
        out.insert(name.into(), Value::Array(runs));
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outline_of_an_l_and_a_box() {
        let s = Shape::new([100, 200, 40, 30], 10, &[[2, 0, 2, 2]]).unwrap();
        assert_eq!(
            s.outline(),
            [
                [100, 200],
                [120, 200],
                [120, 220],
                [140, 220],
                [140, 230],
                [100, 230]
            ]
        );
        let b = Shape::new([0, 0, 30, 20], 10, &[]).unwrap();
        assert!(!b.shaped);
        assert_eq!(b.outline(), [[0, 0], [30, 0], [30, 20], [0, 20]]);
    }

    #[test]
    fn touching_along_a_notch() {
        // A 4x2 box with its top-right cell cut; a 1x1 level in the notch.
        let a = Shape::new([0, 0, 40, 20], 10, &[[3, 0, 1, 1]]).unwrap();
        let b = Shape::new([30, 0, 10, 10], 10, &[]).unwrap();
        assert_eq!(
            shaped_touching(&a, &b, "b"),
            [
                json!({ "level": "b", "edge": "right", "span": [0, 10], "at": 30 }),
                json!({ "level": "b", "edge": "top", "span": [30, 40], "at": 10 }),
            ]
        );
    }
}
