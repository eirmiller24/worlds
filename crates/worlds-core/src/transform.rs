//! Mirror and rotate on the grid, and arranging levels.
//!
//! A turn is a [`Flip`] in the tile notation of `docs/format.md`: `t`
//! (transpose, swap x and y) first, then `x` mirrors left-right and `y`
//! top-bottom. A quarter turn clockwise is `xt`, a half turn `xy`. The same
//! value turns a single tile and a whole piece of a level:
//!
//! - [`fragment`] turns a [`Fragment`] (the paste ghost, an assembly before
//!   it is placed): grid cells and tile stacks move, tiles turn with their
//!   flip bits, entities move to the turned cell keeping their size and
//!   fields, annotation geometry turns.
//! - [`selection`] turns what is selected in a level in place, about its
//!   bounding box: one batch of `paint_cells`, `set_tiles`, `move_entity`
//!   and `update_note`, one undo entry. Entities keep their ids, size and
//!   field values (a transform never changes a field; [`SelectionPlan`]'s
//!   `notes` say when an enum field looks directional).
//! - [`arrange_levels`] aligns, distributes and butts levels together: one
//!   batch of `move_level`.
//!
//! A turn with `t` needs square tiles: refused when a tile in it comes from
//! a tileset whose tiles aren't square, as rules require for `rotate`.

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::command::{CellValue, Command, CommandError, GridValue};
use crate::fragment::{self, CopySelection, Fragment, FragmentGrid};
use crate::id::Slug;
use crate::model::{EntityDef, EnumSource, FieldType, Flip, Level, Project, Shape, TilePlacement};

/// A selection transform, as the editor's menus name them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TransformOp {
    /// Left-right (flip `x`).
    MirrorX,
    /// Top-bottom (flip `y`).
    MirrorY,
    /// A quarter turn clockwise (flip `xt`).
    Rotate90,
}

impl TransformOp {
    /// The turn as a flip.
    pub fn flip(self) -> Flip {
        match self {
            TransformOp::MirrorX => Flip::X,
            TransformOp::MirrorY => Flip::Y,
            TransformOp::Rotate90 => Flip::XT,
        }
    }
}

/// `f`, then turned by `op` (both in flip notation).
pub fn then(f: Option<Flip>, op: Flip) -> Option<Flip> {
    let b = op.bits();
    let mut f = f;
    if b & 4 != 0 {
        f = Flip::transpose(f);
    }
    if b & 1 != 0 {
        f = Flip::mirror_x(f);
    }
    if b & 2 != 0 {
        f = Flip::mirror_y(f);
    }
    f
}

/// Words that make an enum value look like a direction.
const DIRECTIONS: [&str; 20] = [
    "left",
    "right",
    "up",
    "down",
    "north",
    "south",
    "east",
    "west",
    "top",
    "bottom",
    "cw",
    "ccw",
    "clockwise",
    "counterclockwise",
    "ne",
    "nw",
    "se",
    "sw",
    "horizontal",
    "vertical",
];

/// Whether an enum value slug looks like a direction (`left`,
/// `facing_north`, `ne`...).
pub fn looks_directional(value: &str) -> bool {
    let parts: Vec<&str> = value.split('_').collect();
    parts.iter().any(|p| DIRECTIONS.contains(p))
}

/// The enum fields of `def` whose values look directional (more than one
/// value does), by field slug.
pub fn directional_fields(project: &Project, def: &EntityDef) -> Vec<Slug> {
    def.fields
        .iter()
        .filter(|f| match &f.ty {
            FieldType::Enum(src) => {
                let values: Vec<&str> = match src {
                    EnumSource::Inline(v) => v.iter().map(|s| s.as_str()).collect(),
                    EnumSource::Project(e) => project
                        .enum_def(e.as_str())
                        .map(|e| e.values.iter().map(|v| v.slug.as_str()).collect())
                        .unwrap_or_default(),
                };
                values.iter().filter(|v| looks_directional(v)).count() >= 2
            }
            _ => false,
        })
        .map(|f| f.slug.clone())
        .collect()
}

/// A turn of a box on the plane: points of the box `src` (`[x, y, w, h]`)
/// land in the box at `dst` (top-left) whose size is `src`'s, swapped when
/// the turn transposes.
#[derive(Debug, Clone, Copy)]
struct BoxMap {
    bits: u8,
    src: [i64; 4],
    dst: [i64; 2],
}

impl BoxMap {
    fn new(flip: Flip, src: [i64; 4], dst: [i64; 2]) -> Self {
        BoxMap {
            bits: flip.bits(),
            src,
            dst,
        }
    }

    fn transposes(&self) -> bool {
        self.bits & 4 != 0
    }

    /// The turned box's size.
    fn size(&self) -> [i64; 2] {
        if self.transposes() {
            [self.src[3], self.src[2]]
        } else {
            [self.src[2], self.src[3]]
        }
    }

    fn point_f(&self, p: [f64; 2]) -> [f64; 2] {
        let mut u = [p[0] - self.src[0] as f64, p[1] - self.src[1] as f64];
        if self.transposes() {
            u = [u[1], u[0]];
        }
        let s = self.size();
        if self.bits & 1 != 0 {
            u[0] = s[0] as f64 - u[0];
        }
        if self.bits & 2 != 0 {
            u[1] = s[1] as f64 - u[1];
        }
        [u[0] + self.dst[0] as f64, u[1] + self.dst[1] as f64]
    }

    fn point(&self, p: [i64; 2]) -> [i64; 2] {
        let mut u = [p[0] - self.src[0], p[1] - self.src[1]];
        if self.transposes() {
            u = [u[1], u[0]];
        }
        let s = self.size();
        if self.bits & 1 != 0 {
            u[0] = s[0] - u[0];
        }
        if self.bits & 2 != 0 {
            u[1] = s[1] - u[1];
        }
        [u[0] + self.dst[0], u[1] + self.dst[1]]
    }

    /// A rect `[x, y, w, h]`, turned (as a rect again).
    fn rect(&self, r: [i64; 4]) -> [i64; 4] {
        let a = self.point([r[0], r[1]]);
        let b = self.point([r[0] + r[2], r[1] + r[3]]);
        [
            a[0].min(b[0]),
            a[1].min(b[1]),
            (a[0] - b[0]).abs(),
            (a[1] - b[1]).abs(),
        ]
    }

    /// The cell of grid `lg` holding the turned top-left corner of a `w` x
    /// `h` px box whose top-left is at cell `c`.
    fn cell(&self, c: [i64; 2], lg: i64, w: i64, h: i64) -> [i64; 2] {
        let r = self.rect([c[0] * lg, c[1] * lg, w, h]);
        [r[0].div_euclid(lg), r[1].div_euclid(lg)]
    }

    fn shape(&self, s: &mut Shape) {
        let pt = |p: &mut [i64; 2]| *p = self.point(*p);
        match s {
            Shape::Note { px } | Shape::Pin { px, .. } => pt(px),
            Shape::Path { points } | Shape::Guide { points, .. } => points.iter_mut().for_each(pt),
            Shape::Region { points, rect } => {
                points.iter_mut().for_each(pt);
                if let Some(r) = rect {
                    *r = self.rect(*r);
                }
            }
            Shape::Connector { waypoints, .. } => waypoints.iter_mut().for_each(pt),
        }
    }
}

/// Refuse a transposing turn of tiles that aren't square.
fn check_square<'a>(
    project: &Project,
    flip: Flip,
    tilesets: impl IntoIterator<Item = &'a Slug>,
) -> Result<(), CommandError> {
    if flip.bits() & 4 == 0 {
        return Ok(());
    }
    for ts in tilesets {
        if let Some(d) = project.tileset(ts.as_str())
            && d.tile_size[0] != d.tile_size[1]
        {
            return Err(CommandError::Invalid(format!(
                "rotating needs square tiles, but tileset {:?} has {}x{} tiles",
                ts.as_str(),
                d.tile_size[0],
                d.tile_size[1]
            )));
        }
    }
    Ok(())
}

/// The pixel size of a placement's tile (its tileset's tile size; the
/// layer's cell for an unknown tileset).
fn tile_px(project: &Project, p: &TilePlacement, lg: i64) -> [i64; 2] {
    project
        .tileset(p.tileset.as_str())
        .map_or([lg, lg], |d| [d.tile_size[0] as i64, d.tile_size[1] as i64])
}

/// The grid of a layer (the project's for one it doesn't have).
fn grid_of(project: &Project, layer: &Slug) -> i64 {
    project
        .layer(layer.as_str())
        .map_or(project.grid_size.max(1), |d| project.layer_grid(d)) as i64
}

/// Turn one placement: its tile's box moves, its flip composes with `flip`.
fn turn_tile(
    project: &Project,
    m: &BoxMap,
    flip: Flip,
    p: &TilePlacement,
    lg: i64,
) -> ([i64; 2], TilePlacement) {
    let [tw, th] = tile_px(project, p, lg);
    let cell = m.cell([p.cell[0] as i64, p.cell[1] as i64], lg, tw, th);
    let mut out = p.clone();
    out.flip = then(p.flip, flip);
    (cell, out)
}

/// Where an entity's pivot cell and offset go: its box's center turns, its
/// size stays.
fn turn_entity(
    m: &BoxMap,
    def: Option<&EntityDef>,
    cell: [i32; 2],
    offset: [i32; 2],
    size: Option<[u32; 2]>,
    lg: i64,
    g: i64,
) -> ([i32; 2], [i32; 2]) {
    let pivot = def.map_or([0.0, 0.0], |d| d.pivot);
    let size = size.or(def.map(|d| d.size)).unwrap_or([1, 1]);
    let lg = lg as f64;
    let len = [size[0] as f64 * g as f64, size[1] as f64 * g as f64];
    let center =
        |i: usize| (cell[i] as f64 + pivot[i]) * lg + offset[i] as f64 + (0.5 - pivot[i]) * len[i];
    let c = m.point_f([center(0), center(1)]);
    let mut out_cell = [0i32; 2];
    let mut out_off = [0i32; 2];
    for i in 0..2 {
        let p = c[i] + (pivot[i] - 0.5) * len[i];
        // The cell that needs the smallest offset.
        let q = ((p - pivot[i] * lg) / lg).round();
        out_cell[i] = q as i32;
        out_off[i] = (p - (q + pivot[i]) * lg).round() as i32;
    }
    (out_cell, out_off)
}

/// Turn the rows of a fragment grid inside a box map whose frame is the
/// fragment's (pixels from its origin): the cells that land at negative
/// coordinates are dropped.
fn turn_rows(m: &BoxMap, fg: &FragmentGrid, lg: i64) -> FragmentGrid {
    let mut cells: BTreeMap<(i64, i64), char> = BTreeMap::new();
    for (y, row) in fg.rows.iter().enumerate() {
        for (x, c) in row.chars().enumerate() {
            if c == ' ' {
                continue;
            }
            let n = m.cell([x as i64, y as i64], lg, lg, lg);
            if n[0] >= 0 && n[1] >= 0 {
                cells.insert((n[1], n[0]), c);
            }
        }
    }
    let h = cells.keys().map(|k| k.0 + 1).max().unwrap_or(0);
    let mut rows = vec![String::new(); h as usize];
    for ((y, x), c) in cells {
        let row = &mut rows[y as usize];
        while (row.chars().count() as i64) < x {
            row.push(' ');
        }
        row.push(c);
    }
    FragmentGrid {
        legend: fg.legend.clone(),
        rows,
    }
}

/// Turn a fragment by `flip` about its own box: what lands at negative
/// cells (only possible for tiles and entities bigger than their cell) is
/// pulled back to 0 for tiles and kept for entities. Refused: a transposing
/// turn of tiles that aren't square.
pub fn fragment(project: &Project, frag: &Fragment, flip: Flip) -> Result<Fragment, CommandError> {
    check_square(
        project,
        flip,
        frag.tiles.values().flatten().map(|p| &p.tileset),
    )?;
    let g = frag.grid_size.max(1) as i64;
    let m = BoxMap::new(
        flip,
        [0, 0, frag.size[0] as i64 * g, frag.size[1] as i64 * g],
        [0, 0],
    );
    let size = m.size();
    let mut out = frag.clone();
    out.size = [(size[0] / g) as u32, (size[1] / g) as u32];
    for (layer, fg) in &frag.grids {
        let lg = grid_of(project, layer);
        out.grids.insert(layer.clone(), turn_rows(&m, fg, lg));
    }
    for (layer, placements) in &frag.tiles {
        let lg = grid_of(project, layer);
        let mut v: Vec<TilePlacement> = placements
            .iter()
            .map(|p| {
                let (c, mut t) = turn_tile(project, &m, flip, p, lg);
                t.cell = [c[0].max(0) as u32, c[1].max(0) as u32];
                t
            })
            .collect();
        // Stacks keep their order; the list is by row then column.
        v.sort_by_key(|p| (p.cell[1], p.cell[0]));
        out.tiles.insert(layer.clone(), v);
    }
    for e in &mut out.entities {
        let lg = e.layer.as_ref().map_or(g, |l| grid_of(project, l));
        let (cell, off) = turn_entity(
            &m,
            project.entity_def(e.ty.as_str()),
            e.cell,
            e.offset_px,
            e.size,
            lg,
            g,
        );
        e.cell = cell;
        e.offset_px = off;
    }
    for n in &mut out.notes {
        m.shape(&mut n.shape);
    }
    Ok(out)
}

/// A selection transform, worked out: one batch, nothing applied yet.
#[derive(Debug, Clone, PartialEq)]
pub struct SelectionPlan {
    /// `paint_cells`, `set_tiles`, `move_entity` and `update_note` steps.
    pub commands: Vec<Command>,
    /// "Mirror 4 items left-right".
    pub label: String,
    /// What the status bar should say besides: clipped cells, enum fields
    /// that look directional (a transform never changes field values).
    pub notes: Vec<String>,
    /// The turned selection's bounding box, `[x, y, w, h]` in cells of the
    /// project grid of the level.
    pub rect: [i64; 4],
}

impl SelectionPlan {
    pub fn command(&self) -> Command {
        Command::Batch {
            label: self.label.clone(),
            commands: self.commands.clone(),
        }
    }
}

fn lcm(a: i64, b: i64) -> i64 {
    fn gcd(a: i64, b: i64) -> i64 {
        if b == 0 { a } else { gcd(b, a % b) }
    }
    if a == 0 || b == 0 {
        a.max(b)
    } else {
        a / gcd(a, b) * b
    }
}

fn label(flip: Flip, n: usize) -> String {
    let items = format!("{n} item{}", if n == 1 { "" } else { "s" });
    match flip {
        Flip::X => format!("Mirror {items} left-right"),
        Flip::Y => format!("Mirror {items} top-bottom"),
        Flip::XT => format!("Rotate {items} a quarter turn"),
        Flip::XY => format!("Rotate {items} a half turn"),
        Flip::YT => format!("Rotate {items} three quarters"),
        f => format!("Turn {items} ({})", f.as_str()),
    }
}

/// Turn a selection in place about its bounding box: around the grid
/// cells, tile cells, entity boxes and annotation geometry that
/// [`fragment::copy`] takes, on whole cells of the grid and tiles layers
/// involved. A rotation keeps the box's center.
/// Grid cells and tile stacks that would leave the level are dropped (a
/// note says how many); an entity that would leave it refuses the whole
/// transform. Annotations go wherever they land.
pub fn selection(
    project: &Project,
    sel: &CopySelection,
    flip: Flip,
) -> Result<SelectionPlan, CommandError> {
    let frag = fragment::copy(project, sel)?;
    check_square(
        project,
        flip,
        frag.tiles.values().flatten().map(|p| &p.tileset),
    )?;
    let g = project.grid_size.max(1) as i64;
    let src = frag.source.clone().unwrap_or(fragment::FragmentSource {
        level: None,
        origin: [0, 0],
    });
    let level: Option<&Level> = src.level.as_ref().and_then(|s| project.level(s.as_str()));
    let origin_px = [src.origin[0] * g, src.origin[1] * g];
    let level_px = level.map_or([0, 0], |l| l.world_px);

    // The box, in level pixels, around what is turned: grid cells and
    // tile cells, entity boxes (not their pivot cells), annotation
    // geometry. Grid and tile layers need it on whole cells of theirs;
    // entities and annotations alone turn about their exact box.
    let mut align = 1;
    for l in frag.grids.keys().chain(frag.tiles.keys()) {
        align = lcm(align, grid_of(project, l));
    }
    let mut bounds: Option<[i64; 4]> = None;
    let mut grow = |r: [i64; 4]| {
        bounds = Some(match bounds {
            None => r,
            Some(o) => [
                o[0].min(r[0]),
                o[1].min(r[1]),
                o[2].max(r[2]),
                o[3].max(r[3]),
            ],
        });
    };
    for (layer, fg) in &frag.grids {
        let lg = grid_of(project, layer);
        for (y, row) in fg.rows.iter().enumerate() {
            for (x, c) in row.chars().enumerate() {
                if c != ' ' {
                    let px = origin_px[0] + x as i64 * lg;
                    let py = origin_px[1] + y as i64 * lg;
                    grow([px, py, px + lg, py + lg]);
                }
            }
        }
    }
    for (layer, placements) in &frag.tiles {
        let lg = grid_of(project, layer);
        for p in placements {
            let px = origin_px[0] + p.cell[0] as i64 * lg;
            let py = origin_px[1] + p.cell[1] as i64 * lg;
            // A tile can be bigger than its cell.
            let [tw, th] = tile_px(project, p, lg);
            grow([px, py, px + tw.max(lg), py + th.max(lg)]);
        }
    }
    for e in &frag.entities {
        let lg = e.layer.as_ref().map_or(g, |l| grid_of(project, l));
        let o = [origin_px[0] / lg, origin_px[1] / lg];
        let inst = crate::model::EntityInstance {
            id: e.id.clone(),
            ty: e.ty.clone(),
            cell: [
                (e.cell[0] as i64 + o[0]) as i32,
                (e.cell[1] as i64 + o[1]) as i32,
            ],
            offset_px: e.offset_px,
            size: e.size,
            fields: Default::default(),
        };
        let r = match project.entity_def(e.ty.as_str()) {
            Some(def) => def.pixel_rect(&inst, lg as u32, g as u32),
            None => [
                inst.cell[0] * lg as i32 + inst.offset_px[0],
                inst.cell[1] * lg as i32 + inst.offset_px[1],
                lg as i32,
                lg as i32,
            ],
        };
        grow([
            r[0] as i64,
            r[1] as i64,
            (r[0] + r[2]) as i64,
            (r[1] + r[3]) as i64,
        ]);
    }
    for n in &frag.notes {
        if let Some([x, y, w, h]) = n.bbox() {
            let (x, y) = (origin_px[0] + x, origin_px[1] + y);
            grow([x, y, x + w, y + h]);
        }
    }
    let b = bounds.unwrap_or([origin_px[0], origin_px[1], origin_px[0], origin_px[1]]);
    let down = |v: i64| v.div_euclid(align) * align;
    let up = |v: i64| (v + align - 1).div_euclid(align) * align;
    let bx = [down(b[0]), down(b[1]), up(b[2]), up(b[3])];
    let srect = [bx[0], bx[1], bx[2] - bx[0], bx[3] - bx[1]];
    let dst = if flip.bits() & 4 != 0 {
        // Keep the center.
        [
            down((2 * srect[0] + srect[2] - srect[3]).div_euclid(2) + align / 2),
            down((2 * srect[1] + srect[3] - srect[2]).div_euclid(2) + align / 2),
        ]
    } else {
        [srect[0], srect[1]]
    };
    // Level pixels to level pixels.
    let m = BoxMap::new(flip, srect, dst);
    let msize = m.size();

    let mut commands = Vec::new();
    let mut notes = Vec::new();
    let mut clipped = 0usize;
    let mut items = 0usize;

    if let Some(l) = level {
        // ---- grids: clear the old cells, then write the turned ones ----
        for (layer, fg) in &frag.grids {
            let Some(def) = project.layer(layer.as_str()) else {
                continue;
            };
            let lg = project.layer_grid(def) as i64;
            let size = project.layer_cells(def, l);
            let o = [origin_px[0] / lg, origin_px[1] / lg];
            let inside = |c: [i64; 2]| {
                c[0] >= 0 && c[1] >= 0 && c[0] < size[0] as i64 && c[1] < size[1] as i64
            };
            let mut clear = Vec::new();
            let mut set = Vec::new();
            for (y, row) in fg.rows.iter().enumerate() {
                for (x, ch) in row.chars().enumerate() {
                    if ch == ' ' {
                        continue;
                    }
                    let c = [o[0] + x as i64, o[1] + y as i64];
                    let value = match fg.legend.get(&ch) {
                        Some(v) => v.clone(),
                        None if ch == '.' => None,
                        None => def.value_by_glyph(ch).map(|v| v.slug.clone()),
                    };
                    if inside(c) {
                        clear.push(CellValue::new(c[0] as u32, c[1] as u32, GridValue::Clear));
                    }
                    let n = m.cell(c, lg, lg, lg);
                    if inside(n) {
                        set.push(CellValue::new(
                            n[0] as u32,
                            n[1] as u32,
                            GridValue::from(value),
                        ));
                        items += 1;
                    } else {
                        clipped += 1;
                    }
                }
            }
            clear.extend(set);
            if !clear.is_empty() {
                commands.push(Command::PaintCells {
                    level: l.slug.clone(),
                    layer: layer.clone(),
                    cells: clear,
                    keep_empty: false,
                });
            }
        }
        // ---- tiles: empty the old stacks, write the turned ones ----
        for (layer, placements) in &frag.tiles {
            let Some(def) = project.layer(layer.as_str()) else {
                continue;
            };
            let lg = project.layer_grid(def) as i64;
            let size = project.layer_cells(def, l);
            let o = [origin_px[0] / lg, origin_px[1] / lg];
            let mut clear: BTreeSet<[u32; 2]> = BTreeSet::new();
            let mut stacks: BTreeSet<[u32; 2]> = BTreeSet::new();
            let mut lost: BTreeSet<[u32; 2]> = BTreeSet::new();
            let mut tiles = Vec::new();
            for p in placements {
                let c = [o[0] + p.cell[0] as i64, o[1] + p.cell[1] as i64];
                clear.insert([c[0] as u32, c[1] as u32]);
                let mut lp = p.clone();
                lp.cell = [c[0] as u32, c[1] as u32];
                let (n, mut t) = turn_tile(project, &m, flip, &lp, lg);
                if n[0] < 0 || n[1] < 0 || n[0] >= size[0] as i64 || n[1] >= size[1] as i64 {
                    lost.insert(lp.cell);
                    continue;
                }
                t.cell = [n[0] as u32, n[1] as u32];
                stacks.insert(t.cell);
                tiles.push(t);
            }
            clipped += lost.len();
            items += stacks.len();
            tiles.sort_by_key(|p| (p.cell[1], p.cell[0]));
            commands.push(Command::SetTiles {
                level: l.slug.clone(),
                layer: layer.clone(),
                tiles,
                clear: clear.into_iter().collect(),
                keep_empty: false,
            });
        }
        // ---- entities: move, keeping id, size and fields ----
        let mut directional: BTreeMap<(Slug, Slug), usize> = BTreeMap::new();
        for e in &frag.entities {
            let layer = e.layer.clone();
            let lg = layer.as_ref().map_or(g, |l| grid_of(project, l));
            let o = [origin_px[0] / lg, origin_px[1] / lg];
            let cell = [
                (e.cell[0] as i64 + o[0]) as i32,
                (e.cell[1] as i64 + o[1]) as i32,
            ];
            let def = project.entity_def(e.ty.as_str());
            let (ncell, noff) = turn_entity(&m, def, cell, e.offset_px, e.size, lg, g);
            let lsize = layer
                .as_ref()
                .and_then(|s| project.layer(s.as_str()))
                .map_or(l.size, |d| project.layer_cells(d, l));
            if ncell[0] < 0
                || ncell[1] < 0
                || ncell[0] as i64 >= lsize[0] as i64
                || ncell[1] as i64 >= lsize[1] as i64
            {
                return Err(CommandError::Invalid(format!(
                    "entity {} ({}) would land outside level {} at [{}, {}]; nothing was turned",
                    e.id, e.ty, l.slug, ncell[0], ncell[1]
                )));
            }
            items += 1;
            if let Some(d) = def {
                for f in directional_fields(project, d) {
                    *directional.entry((e.ty.clone(), f)).or_default() += 1;
                }
            }
            if ncell != cell || noff != e.offset_px {
                commands.push(Command::MoveEntity {
                    id: e.id.clone(),
                    cell: ncell,
                    offset_px: noff,
                    allow_outside: false,
                });
            }
        }
        if !directional.is_empty() {
            let list: Vec<String> = directional
                .keys()
                .map(|(t, f)| format!("{t}.{f}"))
                .collect();
            notes.push(format!(
                "Fields aren't changed by a transform: check {} (looks like a direction)",
                list.join(", ")
            ));
        }
    }
    // ---- annotations: the geometry turns ----
    for n in &frag.notes {
        let Some(at) = project.find_note(&n.id) else {
            continue;
        };
        let mut a = at.note.clone();
        // The fragment's copy is in fragment pixels; back to level pixels,
        // turned, then to the note's own frame.
        let mut shape = n.shape.clone();
        shape.translate(origin_px);
        m.shape(&mut shape);
        if at.level.is_none() {
            shape.translate(level_px);
        }
        a.shape = shape;
        items += 1;
        if a != *at.note {
            commands.push(Command::UpdateNote {
                id: a.id.clone(),
                note: a,
            });
        }
    }
    if clipped > 0 {
        notes.push(format!(
            "{clipped} cell{} would leave the level and {} dropped",
            if clipped == 1 { "" } else { "s" },
            if clipped == 1 { "was" } else { "were" }
        ));
    }
    Ok(SelectionPlan {
        commands,
        label: label(flip, items),
        notes,
        rect: [dst[0] / g, dst[1] / g, msize[0] / g, msize[1] / g],
    })
}

/// A way to arrange levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ArrangeOp {
    /// Left edges on the leftmost one's.
    AlignLeft,
    AlignRight,
    AlignTop,
    AlignBottom,
    /// Horizontal centers (x) on the first level's.
    AlignCenterX,
    /// Vertical centers (y) on the first level's.
    AlignCenterY,
    /// Equal gaps between neighbours left to right; the outer two stay.
    DistributeX,
    /// Equal gaps top to bottom; the outer two stay.
    DistributeY,
    /// Close the gaps leftwards: the leftmost stays, each next one's left
    /// edge goes on the previous one's right edge.
    ButtLeft,
    ButtRight,
    ButtUp,
    ButtDown,
}

impl ArrangeOp {
    pub const ALL: [ArrangeOp; 12] = [
        ArrangeOp::AlignLeft,
        ArrangeOp::AlignRight,
        ArrangeOp::AlignTop,
        ArrangeOp::AlignBottom,
        ArrangeOp::AlignCenterX,
        ArrangeOp::AlignCenterY,
        ArrangeOp::DistributeX,
        ArrangeOp::DistributeY,
        ArrangeOp::ButtLeft,
        ArrangeOp::ButtRight,
        ArrangeOp::ButtUp,
        ArrangeOp::ButtDown,
    ];

    /// The name on the wire (`align_left`).
    pub fn name(self) -> &'static str {
        match self {
            ArrangeOp::AlignLeft => "align_left",
            ArrangeOp::AlignRight => "align_right",
            ArrangeOp::AlignTop => "align_top",
            ArrangeOp::AlignBottom => "align_bottom",
            ArrangeOp::AlignCenterX => "align_center_x",
            ArrangeOp::AlignCenterY => "align_center_y",
            ArrangeOp::DistributeX => "distribute_x",
            ArrangeOp::DistributeY => "distribute_y",
            ArrangeOp::ButtLeft => "butt_left",
            ArrangeOp::ButtRight => "butt_right",
            ArrangeOp::ButtUp => "butt_up",
            ArrangeOp::ButtDown => "butt_down",
        }
    }

    /// The words in a menu ("Align left edges").
    pub fn title(self) -> &'static str {
        match self {
            ArrangeOp::AlignLeft => "Align left edges",
            ArrangeOp::AlignRight => "Align right edges",
            ArrangeOp::AlignTop => "Align top edges",
            ArrangeOp::AlignBottom => "Align bottom edges",
            ArrangeOp::AlignCenterX => "Align horizontal centers",
            ArrangeOp::AlignCenterY => "Align vertical centers",
            ArrangeOp::DistributeX => "Distribute horizontally",
            ArrangeOp::DistributeY => "Distribute vertically",
            ArrangeOp::ButtLeft => "Butt together leftwards",
            ArrangeOp::ButtRight => "Butt together rightwards",
            ArrangeOp::ButtUp => "Butt together upwards",
            ArrangeOp::ButtDown => "Butt together downwards",
        }
    }
}

/// Arrange levels of one world: one batch of `move_level` labelled like
/// "Align 3 levels left" (empty when nothing moves). Positions stay on the
/// project grid. Refused: fewer than two levels, an unknown level, levels
/// of different worlds.
pub fn arrange_levels(
    project: &Project,
    levels: &[Slug],
    op: ArrangeOp,
) -> Result<Command, CommandError> {
    let mut found: Vec<(usize, &Level)> = Vec::new();
    for s in levels {
        let at = project
            .worlds
            .iter()
            .enumerate()
            .find_map(|(wi, w)| w.level(s.as_str()).map(|l| (wi, l)))
            .ok_or_else(|| CommandError::UnknownLevel(s.clone()))?;
        if !found.iter().any(|f| f.1.slug == at.1.slug) {
            found.push(at);
        }
    }
    if found.len() < 2 {
        return Err(CommandError::Invalid(format!(
            "{} needs at least two levels",
            op.name()
        )));
    }
    let wi = found[0].0;
    if let Some(f) = found.iter().find(|f| f.0 != wi) {
        return Err(CommandError::Invalid(format!(
            "level {} is in world {}, not {}: arrange levels of one world",
            f.1.slug, project.worlds[f.0].slug, project.worlds[wi].slug
        )));
    }
    let g = project.grid_size.max(1) as i64;
    let snap = |v: i64| (v as f64 / g as f64).round() as i64 * g;
    // [x, y, w, h] in world pixels.
    let rect = |l: &Level| {
        [
            l.world_px[0],
            l.world_px[1],
            l.size[0] as i64 * g,
            l.size[1] as i64 * g,
        ]
    };
    let rects: Vec<[i64; 4]> = found.iter().map(|f| rect(f.1)).collect();
    let mut pos: Vec<[i64; 2]> = rects.iter().map(|r| [r[0], r[1]]).collect();
    let n = rects.len();
    // Indices sorted by an edge.
    let sorted = |key: &dyn Fn(&[i64; 4]) -> i64| {
        let mut ix: Vec<usize> = (0..n).collect();
        ix.sort_by_key(|&i| (key(&rects[i]), i));
        ix
    };
    match op {
        ArrangeOp::AlignLeft => {
            let x = rects.iter().map(|r| r[0]).min().unwrap_or(0);
            pos.iter_mut().for_each(|p| p[0] = x);
        }
        ArrangeOp::AlignRight => {
            let x = rects.iter().map(|r| r[0] + r[2]).max().unwrap_or(0);
            for (p, r) in pos.iter_mut().zip(&rects) {
                p[0] = x - r[2];
            }
        }
        ArrangeOp::AlignTop => {
            let y = rects.iter().map(|r| r[1]).min().unwrap_or(0);
            pos.iter_mut().for_each(|p| p[1] = y);
        }
        ArrangeOp::AlignBottom => {
            let y = rects.iter().map(|r| r[1] + r[3]).max().unwrap_or(0);
            for (p, r) in pos.iter_mut().zip(&rects) {
                p[1] = y - r[3];
            }
        }
        ArrangeOp::AlignCenterX => {
            let c = 2 * rects[0][0] + rects[0][2];
            for (p, r) in pos.iter_mut().zip(&rects) {
                p[0] = snap((c - r[2]) / 2);
            }
        }
        ArrangeOp::AlignCenterY => {
            let c = 2 * rects[0][1] + rects[0][3];
            for (p, r) in pos.iter_mut().zip(&rects) {
                p[1] = snap((c - r[3]) / 2);
            }
        }
        ArrangeOp::DistributeX | ArrangeOp::DistributeY => {
            let a = if op == ArrangeOp::DistributeX { 0 } else { 1 };
            let ix = sorted(&|r| r[a]);
            let first = rects[ix[0]][a];
            let last = rects[ix[n - 1]];
            let span = last[a] + last[a + 2] - first;
            let total: i64 = ix.iter().map(|&i| rects[i][a + 2]).sum();
            let gaps = span - total;
            let mut before = 0;
            for (k, &i) in ix.iter().enumerate() {
                if k > 0 && k < n - 1 {
                    let gap = (gaps * k as i64).div_euclid(n as i64 - 1);
                    pos[i][a] = snap(first + before + gap);
                }
                before += rects[i][a + 2];
            }
        }
        ArrangeOp::ButtLeft | ArrangeOp::ButtUp => {
            let a = if op == ArrangeOp::ButtLeft { 0 } else { 1 };
            let ix = sorted(&|r| r[a]);
            let mut edge = rects[ix[0]][a] + rects[ix[0]][a + 2];
            for &i in &ix[1..] {
                pos[i][a] = edge;
                edge += rects[i][a + 2];
            }
        }
        ArrangeOp::ButtRight | ArrangeOp::ButtDown => {
            let a = if op == ArrangeOp::ButtRight { 0 } else { 1 };
            let ix = sorted(&|r| -(r[a] + r[a + 2]));
            let mut edge = rects[ix[0]][a];
            for &i in &ix[1..] {
                edge -= rects[i][a + 2];
                pos[i][a] = edge;
            }
        }
    }
    let world = project.worlds[wi].slug.clone();
    let commands: Vec<Command> = found
        .iter()
        .zip(&pos)
        .filter(|((_, l), p)| l.world_px != **p)
        .map(|((_, l), p)| Command::MoveLevel {
            world: world.clone(),
            level: l.slug.clone(),
            world_px: *p,
        })
        .collect();
    let what = match op {
        ArrangeOp::AlignLeft => "left",
        ArrangeOp::AlignRight => "right",
        ArrangeOp::AlignTop => "top",
        ArrangeOp::AlignBottom => "bottom",
        ArrangeOp::AlignCenterX => "on their horizontal centers",
        ArrangeOp::AlignCenterY => "on their vertical centers",
        ArrangeOp::DistributeX => "horizontally",
        ArrangeOp::DistributeY => "vertically",
        ArrangeOp::ButtLeft => "together leftwards",
        ArrangeOp::ButtRight => "together rightwards",
        ArrangeOp::ButtUp => "together upwards",
        ArrangeOp::ButtDown => "together downwards",
    };
    let verb = match op {
        ArrangeOp::DistributeX | ArrangeOp::DistributeY => "Distribute",
        ArrangeOp::ButtLeft | ArrangeOp::ButtRight | ArrangeOp::ButtUp | ArrangeOp::ButtDown => {
            "Butt"
        }
        _ => "Align",
    };
    Ok(Command::Batch {
        label: format!("{verb} {n} levels {what}"),
        commands,
    })
}
