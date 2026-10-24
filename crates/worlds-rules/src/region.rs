//! Region analysis and the 1D spacing strategies behind region rules.
//!
//! A **region** is a 4-connected component of cells passing a cell test on
//! one int_grid layer (by effective value). Its bounding box, cell count and
//! edge runs are what region rules select on and place along. Everything is
//! keyed by cells: ids are the row-major order of each region's first cell,
//! runs are listed row-major by first cell, so the same grid always gives
//! the same answer.

use worlds_core::{Align, CellMatch, Edge, IntGridLayer, PlaceAt, Project, Spacing};

use crate::hash::{hash, stream};
use crate::{Error, Result};

/// One run of edge cells: `[x, y, len]`, from its first cell (leftmost for
/// floor and ceiling runs, topmost for wall runs).
pub type EdgeRun = [i32; 3];

/// The edge runs of one region, per edge kind.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Edges {
    pub floor: Vec<EdgeRun>,
    pub ceiling: Vec<EdgeRun>,
    pub wall_left: Vec<EdgeRun>,
    pub wall_right: Vec<EdgeRun>,
}

impl Edges {
    pub fn get(&self, e: Edge) -> &[EdgeRun] {
        match e {
            Edge::Floor => &self.floor,
            Edge::Ceiling => &self.ceiling,
            Edge::WallLeft => &self.wall_left,
            Edge::WallRight => &self.wall_right,
        }
    }

    fn get_mut(&mut self, e: Edge) -> &mut Vec<EdgeRun> {
        match e {
            Edge::Floor => &mut self.floor,
            Edge::Ceiling => &mut self.ceiling,
            Edge::WallLeft => &mut self.wall_left,
            Edge::WallRight => &mut self.wall_right,
        }
    }
}

/// One region of a grid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegionInfo {
    /// Row-major order of the region's first cell, from 0.
    pub id: u32,
    /// `[x, y, w, h]` in cells.
    pub bbox: [i32; 4],
    /// Number of cells.
    pub area: u32,
    /// Edge runs; only the kinds that were asked for are filled.
    pub edges: Edges,
}

impl RegionInfo {
    pub fn width(&self) -> u32 {
        self.bbox[2] as u32
    }

    pub fn height(&self) -> u32 {
        self.bbox[3] as u32
    }
}

/// Every region of a grid for one cell test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Regions {
    /// `[w, h]` of the grid in cells.
    pub size: [i32; 2],
    /// Per cell, row-major: region id + 1, or 0 for a cell that isn't in a
    /// region.
    pub labels: Vec<u32>,
    /// By id.
    pub regions: Vec<RegionInfo>,
}

impl Regions {
    /// The region a cell belongs to.
    pub fn at(&self, x: i32, y: i32) -> Option<&RegionInfo> {
        if x < 0 || y < 0 || x >= self.size[0] || y >= self.size[1] {
            return None;
        }
        let l = self.labels[(y * self.size[0] + x) as usize];
        (l > 0).then(|| &self.regions[l as usize - 1])
    }

    /// The grid as text rows: a region cell is its region's letter (`a` to
    /// `z` by id, then `+`), every other cell `#`.
    pub fn picture(&self) -> Vec<String> {
        let [w, h] = self.size;
        (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| match self.labels[(y * w + x) as usize] {
                        0 => '#',
                        l if l <= 26 => (b'a' + (l - 1) as u8) as char,
                        _ => '+',
                    })
                    .collect()
            })
            .collect()
    }
}

/// Every region of int_grid layer `grid` for cell test `cells`, with all
/// four edge kinds, read from `values` (the grid's **effective** values:
/// for a derived layer, overrides over what the rules computed, see
/// [`crate::effective_grid`]). `values` may be smaller than the level
/// (missing cells are empty) and is read on its own size.
///
/// Errors: `grid` isn't an int_grid layer, or `cells` names a value or
/// group it doesn't have.
pub fn regions(
    project: &Project,
    grid: &str,
    values: &IntGridLayer,
    cells: &CellMatch,
) -> Result<Regions> {
    regions_in(project, grid, values, cells, None)
}

/// [`regions`] in a non-rectangular level: `void` marks the cells (of the
/// grid, row-major) cut out of the level, which no region holds and which
/// count as the level's border (an edge run stops at them), as for the
/// region rules. `None` is a rectangular level.
pub fn regions_in(
    project: &Project,
    grid: &str,
    values: &IntGridLayer,
    cells: &CellMatch,
    void: Option<&[bool]>,
) -> Result<Regions> {
    let def = project
        .layer(grid)
        .filter(|l| l.is_int_grid())
        .ok_or_else(|| Error::BadRegion {
            rule: String::new(),
            grid: grid.to_string(),
            reason: "not an int_grid layer".into(),
        })?;
    let table = crate::bake::test_table(def, cells, "regions")?;
    let [w, h] = values.size();
    let unknown = def.values().len() as u16 + 1;
    let vals: Vec<u16> = values.cells().iter().map(|&c| c.min(unknown)).collect();
    let void = void.filter(|v| v.len() == vals.len());
    let mut r = label(&vals, w as i32, h as i32, &table, void);
    for e in [Edge::Floor, Edge::Ceiling, Edge::WallLeft, Edge::WallRight] {
        edge_runs(&mut r, &vals, e, None, void);
    }
    Ok(r)
}

/// Components of cells whose value passes `table`, edges left empty. Cells
/// `void` marks (a non-rectangular level's) are in no region.
pub(crate) fn label(
    values: &[u16],
    w: i32,
    h: i32,
    table: &[bool],
    void: Option<&[bool]>,
) -> Regions {
    let in_void = |i: usize| void.is_some_and(|v| v[i]);
    let n = (w * h).max(0) as usize;
    let mut labels = vec![0u32; n];
    let mut regions = Vec::new();
    let mut stack: Vec<usize> = Vec::new();
    for start in 0..n {
        if labels[start] != 0 || !table[values[start] as usize] || in_void(start) {
            continue;
        }
        let id = regions.len() as u32;
        let l = id + 1;
        labels[start] = l;
        stack.push(start);
        let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
        let mut area = 0u32;
        while let Some(i) = stack.pop() {
            let (x, y) = ((i as i32) % w, (i as i32) / w);
            area += 1;
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
            let mut visit = |j: usize| {
                if labels[j] == 0 && table[values[j] as usize] && !in_void(j) {
                    labels[j] = l;
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
                visit(i - w as usize);
            }
            if y + 1 < h {
                visit(i + w as usize);
            }
        }
        regions.push(RegionInfo {
            id,
            bbox: [x0, y0, x1 - x0 + 1, y1 - y0 + 1],
            area,
            edges: Edges::default(),
        });
    }
    Regions {
        size: [w, h],
        labels,
        regions,
    }
}

/// The neighbour offset of an edge kind.
fn edge_offset(e: Edge) -> (i32, i32) {
    match e {
        Edge::Floor => (0, 1),
        Edge::Ceiling => (0, -1),
        Edge::WallLeft => (-1, 0),
        Edge::WallRight => (1, 0),
    }
}

/// Fill one edge kind's runs of every region. A region cell is on the edge
/// when its neighbour on that side is inside the grid, isn't a region cell,
/// isn't marked in `void` (the void is the level's border, like the grid's
/// edge) and (with `against`) passes `against`.
pub(crate) fn edge_runs(
    r: &mut Regions,
    values: &[u16],
    e: Edge,
    against: Option<&[bool]>,
    void: Option<&[bool]>,
) {
    let [w, h] = r.size;
    let (dx, dy) = edge_offset(e);
    let on = |x: i32, y: i32| -> bool {
        let i = (y * w + x) as usize;
        if r.labels[i] == 0 {
            return false;
        }
        let (nx, ny) = (x + dx, y + dy);
        if nx < 0 || ny < 0 || nx >= w || ny >= h {
            return false;
        }
        let j = (ny * w + nx) as usize;
        r.labels[j] == 0
            && !void.is_some_and(|v| v[j])
            && against.is_none_or(|t| t[values[j] as usize])
    };
    let mut runs: Vec<(u32, EdgeRun)> = Vec::new();
    match e {
        Edge::Floor | Edge::Ceiling => {
            for y in 0..h {
                let mut x = 0;
                while x < w {
                    if !on(x, y) {
                        x += 1;
                        continue;
                    }
                    let start = x;
                    while x < w && on(x, y) {
                        x += 1;
                    }
                    let l = r.labels[(y * w + start) as usize];
                    runs.push((l, [start, y, x - start]));
                }
            }
        }
        Edge::WallLeft | Edge::WallRight => {
            for x in 0..w {
                let mut y = 0;
                while y < h {
                    if !on(x, y) {
                        y += 1;
                        continue;
                    }
                    let start = y;
                    while y < h && on(x, y) {
                        y += 1;
                    }
                    let l = r.labels[(start * w + x) as usize];
                    runs.push((l, [x, start, y - start]));
                }
            }
            // Row-major by first cell.
            runs.sort_by_key(|(_, run)| (run[1], run[0]));
        }
    }
    for reg in &mut r.regions {
        reg.edges.get_mut(e).clear();
    }
    for (l, run) in runs {
        r.regions[l as usize - 1].edges.get_mut(e).push(run);
    }
}

/// The parameters of one 1D placement, from a rule's `place` on one axis.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Axis {
    pub spacing: Spacing,
    pub every: i32,
    pub margin: i32,
    pub align: Align,
    pub jitter: i32,
}

/// Positions on `a..=b` (see the crate README, "Region rules"). `h(p, s)` is
/// the hash at the cell whose run coordinate is `p`, on stream `s` (only
/// `random` calls it).
pub(crate) fn positions(ax: Axis, a: i32, b: i32, h: &dyn Fn(i32, u64) -> u64) -> Vec<i32> {
    let lo = a + ax.margin;
    let hi = b - ax.margin;
    let u = hi - lo + 1;
    if u < 1 {
        return Vec::new();
    }
    let every = ax.every.max(1);
    match ax.spacing {
        Spacing::Even => {
            let k = (u - 1) / every + 1;
            let slack = (u - 1) - (k - 1) * every;
            let o = match ax.align {
                Align::Start => 0,
                Align::Center => slack / 2,
                Align::End => slack,
            };
            (0..k).map(|i| lo + o + i * every).collect()
        }
        Spacing::Spread => {
            let k = (u - 1) / every + 1;
            if k == 1 {
                return vec![lo + (u - 1) / 2];
            }
            let (k, u) = (k as i64, u as i64);
            (0..k)
                .map(|i| lo + ((2 * i * (u - 1) + (k - 1)) / (2 * (k - 1))) as i32)
                .collect()
        }
        Spacing::Random => {
            // In i64: `every` and `jitter` can be near `i32::MAX`, and a
            // step past `hi` must end the loop, never wrap.
            let span = (ax.jitter.max(0) as u64) + 1;
            let (hi, every) = (hi as i64, every as i64);
            let mut out = Vec::new();
            let mut p = lo as i64 + (h(lo, stream::REGION_PHASE) % every as u64) as i64;
            while p <= hi {
                let at = p as i32;
                out.push(at);
                p += every + (h(at, stream::REGION_STEP) % span) as i64;
            }
            out
        }
    }
}

/// What a region rule needs from its `region` and `place`, compiled.
pub(crate) struct RegionPlan {
    pub at: PlaceAt,
    pub x: Axis,
    pub y: Axis,
    pub min_length: i32,
    pub select: worlds_core::Region,
}

/// Per region (by id): whether it is selected, the runs of the rule's edge
/// (before margin; empty for `across`), and its candidates.
pub(crate) struct RegionCandidates {
    pub selected: bool,
    pub runs: Vec<EdgeRun>,
    pub candidates: Vec<[i32; 2]>,
}

/// Candidates of a region rule over analysed regions (edges of the rule's
/// kind already filled). `seed`/`key`: the layer seed and the rule key, for
/// `random`.
pub(crate) fn place(plan: &RegionPlan, r: &Regions, seed: u64, key: u64) -> Vec<RegionCandidates> {
    let [w, _] = r.size;
    r.regions
        .iter()
        .map(|reg| {
            let selected = plan.select.selects(reg.width(), reg.height(), reg.area);
            let runs: Vec<EdgeRun> = match plan.at {
                PlaceAt::Along(e) => reg.edges.get(e).to_vec(),
                PlaceAt::Across => Vec::new(),
            };
            let mut candidates = Vec::new();
            if selected {
                match plan.at {
                    PlaceAt::Along(e) => {
                        let vertical = matches!(e, Edge::WallLeft | Edge::WallRight);
                        for &[x, y, len] in &runs {
                            if len < plan.min_length {
                                continue;
                            }
                            if vertical {
                                let h = |p: i32, s: u64| hash(seed, key, x, p, s);
                                for p in positions(plan.y, y, y + len - 1, &h) {
                                    candidates.push([x, p]);
                                }
                            } else {
                                let h = |p: i32, s: u64| hash(seed, key, p, y, s);
                                for p in positions(plan.x, x, x + len - 1, &h) {
                                    candidates.push([p, y]);
                                }
                            }
                        }
                    }
                    PlaceAt::Across => {
                        let [bx, by, bw, bh] = reg.bbox;
                        let hx = |p: i32, s: u64| hash(seed, key, p, by, s);
                        let hy = |p: i32, s: u64| hash(seed, key, bx, p, s);
                        let xs = positions(plan.x, bx, bx + bw - 1, &hx);
                        let ys = positions(plan.y, by, by + bh - 1, &hy);
                        let l = reg.id + 1;
                        for &y in &ys {
                            for &x in &xs {
                                if r.labels[(y * w + x) as usize] == l {
                                    candidates.push([x, y]);
                                }
                            }
                        }
                    }
                }
                candidates.sort_by_key(|c| (c[1], c[0]));
                candidates.dedup();
            }
            RegionCandidates {
                selected,
                runs,
                candidates,
            }
        })
        .collect()
}

/// Every candidate of a rule, union over selected regions, row-major.
pub(crate) fn flatten(per_region: &[RegionCandidates]) -> Vec<[i32; 2]> {
    let mut all: Vec<[i32; 2]> = per_region
        .iter()
        .flat_map(|r| r.candidates.iter().copied())
        .collect();
    all.sort_by_key(|c| (c[1], c[0]));
    all.dedup();
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    fn even(every: i32, margin: i32, align: Align) -> Axis {
        Axis {
            spacing: Spacing::Even,
            every,
            margin,
            align,
            jitter: 0,
        }
    }

    fn no_hash(_: i32, _: u64) -> u64 {
        unreachable!()
    }

    /// `random` with `every` and `jitter` as large as a rule can make them
    /// (clamped from u32): the steps don't overflow (a debug panic, or
    /// endless wrapping in release); at most one position fits.
    #[test]
    fn random_spacing_with_huge_steps_ends() {
        let ax = Axis {
            spacing: Spacing::Random,
            every: i32::MAX,
            margin: 0,
            align: Align::Center,
            jitter: i32::MAX / 2,
        };
        for h in [0, 7, u64::MAX - 1] {
            let p = positions(ax, 5, 100, &move |_, _| h);
            assert!(
                p.len() <= 1 && p.iter().all(|&x| (5..=100).contains(&x)),
                "{h}: {p:?}"
            );
        }
        // A small one steps as before: phase 7 % 3 = 1 from 5 gives 6, then
        // 6 + 3 + 7 % 3 = 10.
        let small = Axis {
            every: 3,
            jitter: 2,
            ..ax
        };
        assert_eq!(positions(small, 5, 12, &|_, _| 7), vec![6, 10]);
    }

    #[test]
    fn even_worked_examples() {
        let c = even(6, 3, Align::Center);
        // crevasse floor: 3..60, k 10, slack 3, offset 1.
        let crev = positions(c, 0, 63, &no_hash);
        assert_eq!(crev, (0..10).map(|i| 4 + 6 * i).collect::<Vec<_>>());
        // level_4 floor 1..25: 4, 10, 16, 22.
        assert_eq!(positions(c, 1, 25, &no_hash), vec![4, 10, 16, 22]);
        // level_1 ledge 0..9: 4.
        assert_eq!(positions(c, 0, 9, &no_hash), vec![4]);
        // level_2 left room 0..14: 4, 10.
        assert_eq!(positions(c, 0, 14, &no_hash), vec![4, 10]);
        // the 14-wide room's run 16..29: 19, 25 (odd slack leans left).
        assert_eq!(positions(c, 16, 29, &no_hash), vec![19, 25]);
        // margin larger than the run.
        assert_eq!(positions(c, 0, 4, &no_hash), Vec::<i32>::new());
        // exactly 2m + 1.
        assert_eq!(positions(c, 0, 6, &no_hash), vec![3]);
        // start and end put the slack on one side.
        assert_eq!(positions(even(6, 3, Align::Start), 0, 63, &no_hash)[0], 3);
        assert_eq!(positions(even(6, 3, Align::End), 0, 63, &no_hash)[9], 60);
    }

    #[test]
    fn spread_worked_example_and_gaps() {
        let s = Axis {
            spacing: Spacing::Spread,
            every: 6,
            margin: 3,
            align: Align::Center,
            jitter: 0,
        };
        assert_eq!(positions(s, 0, 29, &no_hash), vec![3, 11, 18, 26]);
        for len in 1..80 {
            for every in 1..9 {
                let s = Axis { every, ..s };
                let p = positions(s, 0, len - 1, &no_hash);
                let (lo, hi) = (3, len - 4);
                if hi < lo {
                    assert!(p.is_empty());
                    continue;
                }
                assert_eq!(p[0], if p.len() == 1 { lo + (hi - lo) / 2 } else { lo });
                if p.len() > 1 {
                    assert_eq!(*p.last().unwrap(), hi);
                }
                for g in p.windows(2) {
                    assert!(g[1] - g[0] >= every, "len {len} every {every}: {p:?}");
                }
                // As many as fit.
                assert_eq!(p.len() as i32, (hi - lo) / every + 1);
            }
        }
    }

    #[test]
    fn random_is_deterministic_and_bounded() {
        let r = Axis {
            spacing: Spacing::Random,
            every: 4,
            margin: 2,
            align: Align::Center,
            jitter: 3,
        };
        let h = |p: i32, s: u64| hash(77, 99, p, 5, s);
        let a = positions(r, 0, 200, &h);
        assert_eq!(a, positions(r, 0, 200, &h));
        assert!(a.len() > 10);
        assert!(a[0] >= 2 && a[0] < 2 + 4);
        assert!(*a.last().unwrap() <= 198);
        for g in a.windows(2) {
            assert!((4..=7).contains(&(g[1] - g[0])), "{a:?}");
        }
        // Another seed (salt) re-rolls.
        let h2 = |p: i32, s: u64| hash(78, 99, p, 5, s);
        assert_ne!(a, positions(r, 0, 200, &h2));
        // A run shorter than the margins: nothing.
        assert!(positions(r, 0, 3, &h).is_empty());
    }

    #[test]
    fn labels_and_runs() {
        // 0 = room, 1 = wall. Two rooms: a pit splits the floor of the left one.
        let rows = [
            "1111111111",
            "1000010001",
            "1000010001",
            "1100011111",
            "1111111111",
        ];
        let w = 10;
        let vals: Vec<u16> = rows
            .iter()
            .flat_map(|r| r.bytes().map(|b| (b - b'0') as u16))
            .collect();
        let table = vec![true, false, false];
        let mut r = label(&vals, w, 5, &table, None);
        edge_runs(&mut r, &vals, Edge::Floor, None, None);
        edge_runs(&mut r, &vals, Edge::WallLeft, None, None);
        assert_eq!(r.regions.len(), 2);
        assert_eq!(r.regions[0].bbox, [1, 1, 4, 3]);
        assert_eq!(r.regions[0].area, 11);
        assert_eq!(r.regions[0].edges.floor, vec![[1, 2, 1], [2, 3, 3]]);
        assert_eq!(r.regions[0].edges.wall_left, vec![[1, 1, 2], [2, 3, 1]]);
        assert_eq!(r.regions[1].bbox, [6, 1, 3, 2]);
        assert_eq!(r.picture()[1], "#aaaa#bbb#");
    }
}
