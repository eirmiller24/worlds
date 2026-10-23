//! Clipboard fragments: a piece of a level (grid cells, tiles, entities and
//! annotations) as JSON, cut loose from where it came from.
//!
//! [`copy`] builds a [`Fragment`] from a [`CopySelection`]; [`paste`] turns
//! a fragment into a [`PastePlan`]: one batch of existing commands
//! (`paint_cells`, `set_tiles`, `add_entity`, `add_note`), one undo entry.
//! Nothing here changes a project; the editor, the API and the MCP server
//! apply the plan's command like any other.
//!
//! Coordinates in a fragment are relative to its origin (its top-left
//! corner): grid and tile cells and entity cells in each layer's own grid,
//! annotation geometry in pixels. The fragment's `grid_size` and `size` say
//! how big it is in cells of the project grid. See `docs/format.md`
//! ("Clipboard fragments").

use std::collections::{BTreeMap, BTreeSet, HashSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::command::{CellValue, Command, CommandError, GridValue, NoteScope, Rect};
use crate::id::{Id, Slug};
use crate::model::{
    Annotation, EntityDef, EntityInstance, FieldType, LayerDef, Level, Link, Project, Shape,
    TilePlacement, parse_link,
};

/// The `worlds_fragment` version this build reads and writes.
pub const FRAGMENT_VERSION: u32 = 1;

/// A piece of a level, as copied to the clipboard. Serde-clean: this JSON
/// is what goes on the system clipboard, over the API and (later) into an
/// assembly file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Fragment {
    /// Format version: 1.
    pub worlds_fragment: u32,
    /// The project grid (pixels) of the project it was copied from. A
    /// paste into a project with another grid is refused.
    pub grid_size: u32,
    /// `[width, height]` in cells of that grid.
    pub size: [u32; 2],
    /// Where it was copied from, for paste in place.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<FragmentSource>,
    /// int_grid layers by slug: rows of glyphs from the origin.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub grids: BTreeMap<Slug, FragmentGrid>,
    /// tiles layers by slug: placements, cells from the origin.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tiles: BTreeMap<Slug, Vec<TilePlacement>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entities: Vec<FragmentEntity>,
    /// Annotations, geometry in pixels from the origin.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<Annotation>,
}

/// Where a fragment was copied from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FragmentSource {
    /// The level (none: only world annotations were copied).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<Slug>,
    /// The fragment's top-left corner in cells of the project grid, in the
    /// level (or, without a level, in the world).
    pub origin: [i64; 2],
}

/// One int_grid layer of a fragment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FragmentGrid {
    /// Glyph to value slug (`null`: empty). Glyphs not listed mean what the
    /// destination layer's definition says; `.` is empty.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub legend: BTreeMap<char, Option<Slug>>,
    /// One string per row from the origin, one glyph per cell of the
    /// layer's grid. A space is not part of the fragment (a paste leaves
    /// that cell alone); rows may be short.
    pub rows: Vec<String>,
}

/// One entity of a fragment: an instance as in `entities.json`, plus the
/// layer it sits on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FragmentEntity {
    /// Its id where it was copied from. A paste gives it a new one; the
    /// fragment's references to it follow.
    pub id: Id,
    #[serde(rename = "type")]
    pub ty: Slug,
    /// The entities layer (default: the destination's first).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer: Option<Slug>,
    /// Cell of the layer's grid, from the origin.
    pub cell: [i32; 2],
    #[serde(default, skip_serializing_if = "is_zero")]
    pub offset_px: [i32; 2],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<[u32; 2]>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fields: BTreeMap<Slug, Value>,
}

fn is_zero(v: &[i32; 2]) -> bool {
    *v == [0, 0]
}

/// What to copy. Everything is in one level (`level`, else the level of
/// the first entity or level annotation); world annotations of that level's
/// world may come along.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CopySelection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<Slug>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entities: Vec<Id>,
    /// Annotations: the level's, or world ones.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<Id>,
    /// Tile stacks: cells of a tiles layer, by layer.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tiles: BTreeMap<Slug, Vec<[u32; 2]>>,
    /// A rectangle in cells of the project grid: the cells of `layers`
    /// inside it are copied, empty cells included.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rect: Option<Rect>,
    /// With `rect`: the int_grid (not derived) and tiles layers to copy in
    /// it. Default: every int_grid layer that isn't derived.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub layers: Vec<Slug>,
}

/// Where and how to paste.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PasteOptions {
    pub level: Slug,
    /// Where the fragment's top-left corner goes, in cells of the project
    /// grid of `level` (may be negative: what falls outside is clipped).
    pub origin: [i64; 2],
    /// Fragment layer slug to destination layer slug.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub layer_map: BTreeMap<Slug, Slug>,
}

/// A paste, worked out: the commands and what they make.
#[derive(Debug, Clone, PartialEq)]
pub struct PastePlan {
    /// The steps of the batch ([`PastePlan::command`]).
    pub commands: Vec<Command>,
    /// `Paste 14 items`.
    pub label: String,
    /// Grid cells, tile stacks, entities and annotations pasted.
    pub items: usize,
    /// What was skipped, clipped, refused or changed, in words.
    pub problems: Vec<String>,
    /// Fragment id to new id, for every entity and annotation pasted.
    pub ids: BTreeMap<Id, Id>,
    /// The new entities, in fragment order.
    pub entities: Vec<Id>,
    /// The new annotations, in fragment order.
    pub notes: Vec<Id>,
    /// The tile stacks written, by destination layer.
    pub tiles: BTreeMap<Slug, Vec<[u32; 2]>>,
}

impl PastePlan {
    /// The paste as one command: a batch labelled [`PastePlan::label`].
    pub fn command(&self) -> Command {
        Command::Batch {
            label: self.label.clone(),
            commands: self.commands.clone(),
        }
    }
}

/// The JSON Schema of a fragment (MCP resource `worlds://schema/fragment.json`).
pub fn schema() -> Value {
    let mut v = schemars::schema_for!(Fragment).to_value();
    if let Some(o) = v.as_object_mut() {
        o.insert("title".into(), "Worlds clipboard fragment".into());
    }
    v
}

impl Fragment {
    /// Read a fragment from JSON text (the system clipboard): `None` when
    /// it isn't one. A fragment of another version is an error.
    pub fn from_text(text: &str) -> Option<Result<Fragment, String>> {
        let v: Value = serde_json::from_str(text.trim()).ok()?;
        let version = v.get("worlds_fragment")?.as_u64();
        if version != Some(FRAGMENT_VERSION as u64) {
            return Some(Err(format!(
                "this is a version {} fragment; this Worlds reads version {FRAGMENT_VERSION}",
                v["worlds_fragment"]
            )));
        }
        Some(serde_json::from_value(v).map_err(|e| format!("not a valid fragment: {e}")))
    }

    /// Nothing in it.
    pub fn is_empty(&self) -> bool {
        self.grids.is_empty()
            && self.tiles.is_empty()
            && self.entities.is_empty()
            && self.notes.is_empty()
    }

    /// Items in it: grid cells (not spaces), tile stacks, entities, notes.
    pub fn item_count(&self) -> usize {
        let cells: usize = self
            .grids
            .values()
            .flat_map(|g| &g.rows)
            .map(|r| r.chars().filter(|c| *c != ' ').count())
            .sum();
        let stacks: usize = self
            .tiles
            .values()
            .map(|t| t.iter().map(|p| p.cell).collect::<BTreeSet<_>>().len())
            .sum();
        cells + stacks + self.entities.len() + self.notes.len()
    }
}

fn invalid(msg: impl Into<String>) -> CommandError {
    CommandError::Invalid(msg.into())
}

fn level_of<'p>(p: &'p Project, slug: &Slug) -> Result<(usize, &'p Level), CommandError> {
    p.worlds
        .iter()
        .enumerate()
        .find_map(|(wi, w)| w.level(slug.as_str()).map(|l| (wi, l)))
        .ok_or_else(|| CommandError::UnknownLevel(slug.clone()))
}

fn layer_def<'p>(p: &'p Project, slug: &Slug) -> Result<&'p LayerDef, CommandError> {
    p.layer(slug.as_str())
        .ok_or_else(|| CommandError::UnknownLayer(slug.clone()))
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

/// Copy a selection into a fragment. Refused: unknown ids, levels or
/// layers, things from two levels, a rect outside the level, a derived or
/// auto layer in `layers`.
pub fn copy(project: &Project, sel: &CopySelection) -> Result<Fragment, CommandError> {
    // The level everything comes from.
    let mut entity_at: Vec<(Slug, &Slug, &EntityInstance)> = Vec::new();
    for id in &sel.entities {
        let found = project.levels().find_map(|l| {
            l.entities.iter().find_map(|el| {
                el.entities
                    .iter()
                    .find(|e| e.id == *id)
                    .map(|e| (l.slug.clone(), &el.layer, e))
            })
        });
        entity_at.push(found.ok_or_else(|| CommandError::UnknownEntity(id.clone()))?);
    }
    let mut notes = Vec::new();
    for id in &sel.notes {
        notes.push(
            project
                .find_note(id)
                .ok_or_else(|| CommandError::UnknownNote(id.clone()))?,
        );
    }
    let level_slug = sel
        .level
        .clone()
        .or_else(|| entity_at.first().map(|e| e.0.clone()))
        .or_else(|| notes.iter().find_map(|n| n.level.map(|l| l.slug.clone())));
    let level = match &level_slug {
        Some(s) => Some(level_of(project, s)?),
        None => None,
    };
    if level.is_none() && (sel.rect.is_some() || !sel.tiles.is_empty()) {
        return Err(invalid("copying cells or tiles needs level"));
    }
    if let Some((_, l)) = level {
        for (lv, _, e) in &entity_at {
            if *lv != l.slug {
                return Err(invalid(format!(
                    "entity {} is in level {lv}, not {}: a fragment comes from one level",
                    e.id, l.slug
                )));
            }
        }
    }
    let world_ix = level.map(|(w, _)| w);
    for n in &notes {
        match (n.level, level) {
            (Some(nl), Some((_, l))) if nl.slug != l.slug => {
                return Err(invalid(format!(
                    "note {} is in level {}, not {}: a fragment comes from one level",
                    n.note.id, nl.slug, l.slug
                )));
            }
            (None, _) if world_ix.is_some_and(|w| project.worlds[w].slug != n.world.slug) => {
                return Err(invalid(format!(
                    "note {} is in world {}, not the level's world",
                    n.note.id, n.world.slug
                )));
            }
            _ => {}
        }
    }
    if level.is_none()
        && let Some(w) = notes.first().map(|n| &n.world.slug)
        && let Some(n) = notes.iter().find(|n| n.world.slug != *w)
    {
        return Err(invalid(format!(
            "note {} is in world {}, not {w}: a fragment comes from one world",
            n.note.id, n.world.slug
        )));
    }
    let g = project.grid_size.max(1) as i64;
    // Pixel offset of the level in its world (world notes are moved by
    // it, so everything is in level pixels).
    let level_px = level.map_or([0, 0], |(_, l)| l.world_px);

    // What goes in, with bounds in level pixels.
    let mut bounds: Option<[i64; 4]> = None; // x0, y0, x1, y1
    let mut grow = |b: [i64; 4]| {
        bounds = Some(match bounds {
            None => b,
            Some(o) => [
                o[0].min(b[0]),
                o[1].min(b[1]),
                o[2].max(b[2]),
                o[3].max(b[3]),
            ],
        });
    };
    let mut align = g;

    // The rect, clipped to the level.
    let mut rect_px = None;
    let mut rect_layers: Vec<&LayerDef> = Vec::new();
    if let (Some(r), Some((_, l))) = (sel.rect, level) {
        let x1 = (r.x + r.w).min(l.size[0]);
        let y1 = (r.y + r.h).min(l.size[1]);
        if r.x >= x1 || r.y >= y1 {
            return Err(invalid(format!(
                "rect {:?} is outside level {} ({}x{} cells)",
                <[u32; 4]>::from(r),
                l.slug,
                l.size[0],
                l.size[1]
            )));
        }
        let px = [r.x as i64 * g, r.y as i64 * g, x1 as i64 * g, y1 as i64 * g];
        grow(px);
        rect_px = Some(px);
        if sel.layers.is_empty() {
            rect_layers.extend(
                project
                    .layers
                    .iter()
                    .filter(|d| d.is_int_grid() && !d.is_derived()),
            );
        }
        for s in &sel.layers {
            let def = layer_def(project, s)?;
            if def.is_derived() {
                return Err(invalid(format!(
                    "layer {s} is derived (the rules compute it); copy its source layer instead"
                )));
            }
            if !(def.is_int_grid() || def.is_tiles()) {
                return Err(CommandError::WrongLayerKind {
                    layer: s.clone(),
                    expected: "int_grid or tiles",
                    actual: def.kind.name(),
                });
            }
            rect_layers.push(def);
        }
        for d in &rect_layers {
            align = lcm(align, project.layer_grid(d) as i64);
        }
    } else if !sel.layers.is_empty() {
        return Err(invalid("layers only goes with rect"));
    }

    // Tile stacks: the selected cells, and the tiles layers' cells in the rect.
    let mut stacks: BTreeMap<Slug, BTreeSet<[u32; 2]>> = BTreeMap::new();
    if let Some((_, l)) = level {
        for (layer, cells) in &sel.tiles {
            let def = layer_def(project, layer)?;
            if !def.is_tiles() {
                return Err(CommandError::WrongLayerKind {
                    layer: layer.clone(),
                    expected: "tiles",
                    actual: def.kind.name(),
                });
            }
            let have: BTreeSet<[u32; 2]> = l
                .tiles_layer(layer.as_str())
                .map(|t| t.tiles.iter().map(|p| p.cell).collect())
                .unwrap_or_default();
            let set = stacks.entry(layer.clone()).or_default();
            set.extend(cells.iter().filter(|c| have.contains(*c)));
        }
        if let Some(px) = rect_px {
            for def in rect_layers.iter().filter(|d| d.is_tiles()) {
                let lg = project.layer_grid(def) as i64;
                if let Some(t) = l.tiles_layer(def.slug.as_str()) {
                    let set = stacks.entry(def.slug.clone()).or_default();
                    for p in &t.tiles {
                        let (cx, cy) = (p.cell[0] as i64 * lg, p.cell[1] as i64 * lg);
                        if cx < px[2] && cx + lg > px[0] && cy < px[3] && cy + lg > px[1] {
                            set.insert(p.cell);
                        }
                    }
                }
            }
        }
        stacks.retain(|_, s| !s.is_empty());
        for (layer, cells) in &stacks {
            let lg = project.layer_grid(layer_def(project, layer)?) as i64;
            align = lcm(align, lg);
            for c in cells {
                let (x, y) = (c[0] as i64 * lg, c[1] as i64 * lg);
                grow([x, y, x + lg, y + lg]);
            }
        }
    }
    for (_, layer, e) in &entity_at {
        let lg = project
            .layer(layer.as_str())
            .map_or(g, |d| project.layer_grid(d) as i64);
        align = lcm(align, lg);
        let (x, y) = (e.cell[0] as i64 * lg, e.cell[1] as i64 * lg);
        grow([x, y, x + lg, y + lg]);
    }
    let note_shift = |n: &crate::model::NoteAt<'_>| -> [i64; 2] {
        match n.level {
            Some(_) => [0, 0],
            None => [-level_px[0], -level_px[1]],
        }
    };
    for n in &notes {
        if let Some([x, y, w, h]) = n.note.bbox() {
            let d = note_shift(n);
            grow([x + d[0], y + d[1], x + d[0] + w, y + d[1] + h]);
        }
    }
    let Some(b) = bounds else {
        return Err(invalid("nothing to copy"));
    };
    let origin_px = [
        b[0].div_euclid(align) * align,
        b[1].div_euclid(align) * align,
    ];
    let end = [
        (b[2] + g - 1).div_euclid(g) * g,
        (b[3] + g - 1).div_euclid(g) * g,
    ];
    let size = [
        ((end[0] - origin_px[0]) / g).max(1) as u32,
        ((end[1] - origin_px[1]) / g).max(1) as u32,
    ];
    let mut frag = Fragment {
        worlds_fragment: FRAGMENT_VERSION,
        grid_size: g as u32,
        size,
        source: Some(FragmentSource {
            level: level.map(|(_, l)| l.slug.clone()),
            origin: [origin_px[0] / g, origin_px[1] / g],
        }),
        grids: BTreeMap::new(),
        tiles: BTreeMap::new(),
        entities: Vec::new(),
        notes: Vec::new(),
    };
    // Grid rows.
    if let (Some(px), Some((_, l))) = (rect_px, level) {
        for def in rect_layers.iter().filter(|d| d.is_int_grid()) {
            let lg = project.layer_grid(def) as i64;
            let cells = project.layer_cells(def, l);
            let o = [origin_px[0] / lg, origin_px[1] / lg];
            let x0 = px[0].div_euclid(lg);
            let y0 = px[1].div_euclid(lg);
            let x1 = ((px[2] + lg - 1).div_euclid(lg)).min(cells[0] as i64);
            let y1 = ((px[3] + lg - 1).div_euclid(lg)).min(cells[1] as i64);
            let grid = l.int_grid(def.slug.as_str());
            let mut legend = BTreeMap::new();
            let mut rows = Vec::new();
            for y in o[1]..y1 {
                let mut row = String::new();
                if y >= y0 {
                    for x in o[0]..x1 {
                        if x < x0 {
                            row.push(' ');
                            continue;
                        }
                        let v = grid.and_then(|gr| gr.value_at(def, x as u32, y as u32));
                        match v.and_then(|s| def.value(s.as_str())) {
                            Some(val) => {
                                legend.insert(val.glyph, Some(val.slug.clone()));
                                row.push(val.glyph);
                            }
                            None => row.push('.'),
                        }
                    }
                }
                rows.push(row);
            }
            frag.grids
                .insert(def.slug.clone(), FragmentGrid { legend, rows });
        }
    }
    // Tile stacks.
    if let Some((_, l)) = level {
        for (layer, cells) in &stacks {
            let lg = project.layer_grid(layer_def(project, layer)?) as i64;
            let o = [origin_px[0] / lg, origin_px[1] / lg];
            let mut out = Vec::new();
            if let Some(t) = l.tiles_layer(layer.as_str()) {
                for p in t.tiles.iter().filter(|p| cells.contains(&p.cell)) {
                    let mut p = p.clone();
                    p.cell = [
                        (p.cell[0] as i64 - o[0]) as u32,
                        (p.cell[1] as i64 - o[1]) as u32,
                    ];
                    out.push(p);
                }
            }
            frag.tiles.insert(layer.clone(), out);
        }
    }
    for (_, layer, e) in &entity_at {
        let lg = project
            .layer(layer.as_str())
            .map_or(g, |d| project.layer_grid(d) as i64);
        let o = [origin_px[0] / lg, origin_px[1] / lg];
        // `point` values are cells of the level: in the fragment they are
        // from the origin too.
        let mut fields = e.fields.clone();
        remap_fields(
            project.entity_def(e.ty.as_str()),
            &[],
            &mut fields,
            &BTreeMap::new(),
            &Outside::Keep,
            [-o[0], -o[1]],
            "",
            &mut Vec::new(),
        );
        frag.entities.push(FragmentEntity {
            id: e.id.clone(),
            ty: e.ty.clone(),
            layer: Some((*layer).clone()),
            cell: [
                (e.cell[0] as i64 - o[0]) as i32,
                (e.cell[1] as i64 - o[1]) as i32,
            ],
            offset_px: e.offset_px,
            size: e.size,
            fields,
        });
    }
    for n in &notes {
        let mut a = n.note.clone();
        let d = note_shift(n);
        a.shape
            .translate([d[0] - origin_px[0], d[1] - origin_px[1]]);
        frag.notes.push(a);
    }
    Ok(frag)
}

/// Fresh ids: none of `taken`, none handed out before.
fn fresh(taken: &mut HashSet<Id>) -> Id {
    loop {
        let id = Id::generate();
        if taken.insert(id.clone()) {
            return id;
        }
    }
}

/// How a reference that leaves the copied set is treated.
pub(crate) enum Outside<'a> {
    /// Kept as it is (a duplicated level: the target is still there).
    Keep,
    /// Kept when the destination project resolves it, else dropped with a
    /// problem; same-level-only `entity_ref` fields also need the target in
    /// `level`.
    Resolve {
        project: &'a Project,
        level: &'a Level,
    },
}

impl Outside<'_> {
    fn entity_ok(&self, id: &Id, cross_level: bool) -> bool {
        match self {
            Outside::Keep => true,
            Outside::Resolve { project, level } => {
                if level.all_entities().any(|e| e.id == *id) {
                    return true;
                }
                cross_level
                    && project
                        .levels()
                        .any(|l| l.all_entities().any(|e| e.id == *id))
            }
        }
    }

    fn link_ok(&self, link: &str) -> bool {
        match self {
            Outside::Keep => true,
            Outside::Resolve { project, .. } => match parse_link(link) {
                Link::Level(s) => project.level(s).is_some(),
                Link::Entity(s) => Id::new(s).is_ok_and(|id| {
                    project
                        .levels()
                        .any(|l| l.all_entities().any(|e| e.id == id))
                }),
                Link::Note(s) => Id::new(s).is_ok_and(|id| project.find_note(&id).is_some()),
                Link::Url(_) => true,
            },
        }
    }
}

/// Rewrite the `entity_ref` values of `fields` (of type `def`) through
/// `map`; references outside it follow `outside`. `point` values move by
/// `shift` cells. Problems are reported as "<who>: ...".
#[allow(clippy::too_many_arguments)]
pub(crate) fn remap_fields(
    def: Option<&EntityDef>,
    defs: &[crate::model::FieldDef],
    fields: &mut BTreeMap<Slug, Value>,
    map: &BTreeMap<Id, Id>,
    outside: &Outside<'_>,
    shift: [i64; 2],
    who: &str,
    problems: &mut Vec<String>,
) {
    let field_defs = def.map_or(defs, |d| d.fields.as_slice());
    for (slug, v) in fields.iter_mut() {
        let Some(fd) = field_defs.iter().find(|f| f.slug == *slug) else {
            continue;
        };
        match &fd.ty {
            FieldType::EntityRef { cross_level, .. } => {
                let one = |s: &str, problems: &mut Vec<String>| -> Option<Value> {
                    let Ok(id) = Id::new(s) else {
                        return Some(Value::String(s.to_string()));
                    };
                    if let Some(n) = map.get(&id) {
                        return Some(Value::String(n.to_string()));
                    }
                    if outside.entity_ok(&id, *cross_level) {
                        Some(Value::String(s.to_string()))
                    } else {
                        problems.push(format!(
                            "{who}: field {slug} pointed at entity {s}, which isn't in this level or this paste; set to null"
                        ));
                        None
                    }
                };
                match v {
                    Value::String(s) => {
                        let s = s.clone();
                        *v = one(&s, problems).unwrap_or(Value::Null);
                    }
                    Value::Array(items) => {
                        let mut out = Vec::new();
                        for it in items.iter() {
                            match it {
                                Value::String(s) => out.extend(one(s, problems)),
                                other => out.push(other.clone()),
                            }
                        }
                        *items = out;
                    }
                    _ => {}
                }
            }
            FieldType::Point if shift != [0, 0] => {
                let move_point = |p: &mut Value| {
                    if let Ok([x, y]) = serde_json::from_value::<[i64; 2]>(p.clone()) {
                        *p = serde_json::json!([x + shift[0], y + shift[1]]);
                    }
                };
                match v {
                    Value::Array(items) if fd.array => items.iter_mut().for_each(move_point),
                    other => move_point(other),
                }
            }
            _ => {}
        }
    }
}

/// Rewrite an annotation's `entity:` and `note:` links through the maps;
/// links outside follow `outside` (dropped with a problem when they don't
/// resolve).
pub(crate) fn remap_links(
    links: &mut Vec<String>,
    entities: &BTreeMap<Id, Id>,
    notes: &BTreeMap<Id, Id>,
    outside: &Outside<'_>,
    who: &str,
    problems: &mut Vec<String>,
) {
    let mut out = Vec::with_capacity(links.len());
    for link in links.drain(..) {
        let mapped = match parse_link(&link) {
            Link::Entity(s) => Id::new(s)
                .ok()
                .and_then(|id| entities.get(&id))
                .map(|n| format!("entity:{n}")),
            Link::Note(s) => Id::new(s)
                .ok()
                .and_then(|id| notes.get(&id))
                .map(|n| format!("note:{n}")),
            _ => None,
        };
        match mapped {
            Some(m) => out.push(m),
            None if outside.link_ok(&link) => out.push(link),
            None => problems.push(format!("{who}: link {link} doesn't resolve here; dropped")),
        }
    }
    *links = out;
}

/// Work out a paste of `frag` (nothing is applied). Refused: a fragment of
/// another version or project grid, an unknown level. Everything else
/// that can't be pasted is left out and reported in `problems`.
pub fn paste(
    project: &Project,
    frag: &Fragment,
    opts: &PasteOptions,
) -> Result<PastePlan, CommandError> {
    if frag.worlds_fragment != FRAGMENT_VERSION {
        return Err(invalid(format!(
            "this is a version {} fragment; this Worlds reads version {FRAGMENT_VERSION}",
            frag.worlds_fragment
        )));
    }
    if frag.grid_size != project.grid_size {
        return Err(invalid(format!(
            "the fragment was copied from a project with a {} px grid; this project's is {} px",
            frag.grid_size, project.grid_size
        )));
    }
    let (wi, level) = level_of(project, &opts.level)?;
    let world = project.worlds[wi].slug.clone();
    let g = project.grid_size.max(1) as i64;
    let origin_px = [opts.origin[0] * g, opts.origin[1] * g];
    let dst = |s: &Slug| opts.layer_map.get(s).cloned().unwrap_or_else(|| s.clone());
    let mut problems = Vec::new();
    let mut commands = Vec::new();
    let mut items = 0;
    let mut tiles_out: BTreeMap<Slug, Vec<[u32; 2]>> = BTreeMap::new();
    let mut clipped = 0usize;

    // ---- grids ----
    for (src, fg) in &frag.grids {
        let layer = dst(src);
        let Some(def) = project.layer(layer.as_str()) else {
            problems.push(format!(
                "layer {layer}: not in this project; its cells were skipped"
            ));
            continue;
        };
        if !def.is_int_grid() {
            problems.push(format!(
                "layer {layer} is a {} layer, not int_grid; its cells were skipped",
                def.kind.name()
            ));
            continue;
        }
        let lg = project.layer_grid(def) as i64;
        let size = project.layer_cells(def, level);
        let o = [origin_px[0].div_euclid(lg), origin_px[1].div_euclid(lg)];
        let mut cells = Vec::new();
        let mut unknown: BTreeSet<String> = BTreeSet::new();
        for (y, row) in fg.rows.iter().enumerate() {
            for (x, c) in row.chars().enumerate() {
                if c == ' ' {
                    continue;
                }
                let value = match fg.legend.get(&c) {
                    Some(v) => v.clone(),
                    None if c == '.' => None,
                    None => match def.value_by_glyph(c) {
                        Some(v) => Some(v.slug.clone()),
                        None => {
                            unknown.insert(format!("glyph {c:?}"));
                            continue;
                        }
                    },
                };
                if let Some(v) = &value
                    && def.value(v.as_str()).is_none()
                {
                    unknown.insert(format!("value {v}"));
                    continue;
                }
                let (cx, cy) = (o[0] + x as i64, o[1] + y as i64);
                if cx < 0 || cy < 0 || cx >= size[0] as i64 || cy >= size[1] as i64 {
                    clipped += 1;
                    continue;
                }
                cells.push(CellValue::new(cx as u32, cy as u32, GridValue::from(value)));
            }
        }
        if !unknown.is_empty() {
            problems.push(format!(
                "layer {layer} has no {}; those cells were skipped",
                unknown.into_iter().collect::<Vec<_>>().join(", ")
            ));
        }
        if !cells.is_empty() {
            items += cells.len();
            commands.push(Command::PaintCells {
                level: level.slug.clone(),
                layer,
                cells,
                keep_empty: false,
            });
        }
    }

    // ---- tiles ----
    for (src, placements) in &frag.tiles {
        let layer = dst(src);
        let Some(def) = project.layer(layer.as_str()) else {
            problems.push(format!(
                "layer {layer}: not in this project; its tiles were skipped"
            ));
            continue;
        };
        if !def.is_tiles() {
            problems.push(format!(
                "layer {layer} is a {} layer, not tiles; its tiles were skipped",
                def.kind.name()
            ));
            continue;
        }
        let lg = project.layer_grid(def) as i64;
        let size = project.layer_cells(def, level);
        let o = [origin_px[0].div_euclid(lg), origin_px[1].div_euclid(lg)];
        let mut out = Vec::new();
        let mut missing: BTreeSet<&Slug> = BTreeSet::new();
        let mut stacks: BTreeSet<[u32; 2]> = BTreeSet::new();
        let mut clipped_stacks: BTreeSet<[u32; 2]> = BTreeSet::new();
        for p in placements {
            if project.tileset(p.tileset.as_str()).is_none() {
                missing.insert(&p.tileset);
                continue;
            }
            let (cx, cy) = (o[0] + p.cell[0] as i64, o[1] + p.cell[1] as i64);
            if cx < 0 || cy < 0 || cx >= size[0] as i64 || cy >= size[1] as i64 {
                clipped_stacks.insert(p.cell);
                continue;
            }
            let mut p = p.clone();
            p.cell = [cx as u32, cy as u32];
            stacks.insert(p.cell);
            out.push(p);
        }
        clipped += clipped_stacks.len();
        if !missing.is_empty() {
            problems.push(format!(
                "layer {layer}: no tileset {}; those tiles were skipped",
                missing
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !out.is_empty() {
            items += stacks.len();
            tiles_out
                .entry(layer.clone())
                .or_default()
                .extend(stacks.iter().copied());
            commands.push(Command::SetTiles {
                level: level.slug.clone(),
                layer,
                tiles: out,
                clear: Vec::new(),
                keep_empty: false,
            });
        }
    }
    if clipped > 0 {
        problems.push(format!(
            "{clipped} cell{} fell outside level {} and {} clipped",
            if clipped == 1 { "" } else { "s" },
            level.slug,
            if clipped == 1 { "was" } else { "were" }
        ));
    }

    // ---- entities: decide which go in, then their ids ----
    let mut taken: HashSet<Id> = project
        .levels()
        .flat_map(|l| l.all_entities().map(|e| e.id.clone()))
        .collect();
    let first_entities = project.layers.iter().find(|d| d.is_entities());
    let mut placed: Vec<(&FragmentEntity, Slug, [i32; 2], [i64; 2])> = Vec::new();
    for e in &frag.entities {
        let layer = match &e.layer {
            Some(l) => dst(l),
            None => match first_entities {
                Some(d) => d.slug.clone(),
                None => {
                    problems.push(format!(
                        "entity {} ({}): the project has no entities layer; not pasted",
                        e.id, e.ty
                    ));
                    continue;
                }
            },
        };
        let Some(def) = project.layer(layer.as_str()) else {
            problems.push(format!(
                "entity {} ({}): no layer {layer} in this project; not pasted",
                e.id, e.ty
            ));
            continue;
        };
        if !def.is_entities() {
            problems.push(format!(
                "entity {} ({}): layer {layer} is a {} layer; not pasted",
                e.id,
                e.ty,
                def.kind.name()
            ));
            continue;
        }
        if project.entity_def(e.ty.as_str()).is_none() {
            problems.push(format!(
                "entity {}: no entity type {} in this project; not pasted",
                e.id, e.ty
            ));
            continue;
        }
        let lg = project.layer_grid(def) as i64;
        let size = project.layer_cells(def, level);
        let o = [origin_px[0].div_euclid(lg), origin_px[1].div_euclid(lg)];
        let cell = [o[0] + e.cell[0] as i64, o[1] + e.cell[1] as i64];
        if cell[0] < 0 || cell[1] < 0 || cell[0] >= size[0] as i64 || cell[1] >= size[1] as i64 {
            problems.push(format!(
                "entity {} ({}) would land outside level {} at [{}, {}]; not pasted",
                e.id, e.ty, level.slug, cell[0], cell[1]
            ));
            continue;
        }
        placed.push((e, layer, [cell[0] as i32, cell[1] as i32], o));
    }
    let mut ids = BTreeMap::new();
    for (e, ..) in &placed {
        ids.insert(e.id.clone(), fresh(&mut taken));
    }
    let mut note_taken: HashSet<Id> = project.notes().map(|n| n.note.id.clone()).collect();
    let mut note_ids = BTreeMap::new();
    for n in &frag.notes {
        note_ids.insert(n.id.clone(), fresh(&mut note_taken));
    }
    let outside = Outside::Resolve { project, level };
    let mut entities = Vec::new();
    for (e, layer, cell, o) in placed {
        let new_id = ids[&e.id].clone();
        let mut fields = e.fields.clone();
        // A point field is a cell of the level: it moves with the paste
        // (from the fragment's frame, whose origin is cell 0).
        remap_fields(
            project.entity_def(e.ty.as_str()),
            &[],
            &mut fields,
            &ids,
            &outside,
            o,
            &format!("entity {new_id} ({})", e.ty),
            &mut problems,
        );
        entities.push(new_id.clone());
        items += 1;
        commands.push(Command::AddEntity {
            level: level.slug.clone(),
            layer,
            entity: EntityInstance {
                id: new_id,
                ty: e.ty.clone(),
                cell,
                offset_px: e.offset_px,
                size: e.size,
                fields,
            },
            index: None,
            allow_outside: false,
        });
    }

    // ---- notes ----
    let mut notes = Vec::new();
    for n in &frag.notes {
        let mut a = n.clone();
        a.id = note_ids[&n.id].clone();
        let who = format!("note {}", a.id);
        remap_links(&mut a.links, &ids, &note_ids, &outside, &who, &mut problems);
        let scope = if let Shape::Guide { group, .. } = &mut a.shape {
            // Guides live at world scope.
            if let Some(gr) = group
                && project.worlds[wi].guide_group(gr.as_str()).is_none()
            {
                problems.push(format!(
                    "{who}: world {world} has no guide group {gr}; the guide was put in no group"
                ));
                *group = None;
            }
            a.shape.translate([
                level.world_px[0] + origin_px[0],
                level.world_px[1] + origin_px[1],
            ]);
            NoteScope::World(world.clone())
        } else {
            a.shape.translate(origin_px);
            NoteScope::Level(level.slug.clone())
        };
        notes.push(a.id.clone());
        items += 1;
        commands.push(Command::AddNote {
            scope,
            note: a,
            index: None,
        });
    }
    let mut all_ids = ids;
    all_ids.extend(note_ids);
    Ok(PastePlan {
        commands,
        label: format!("Paste {items} item{}", if items == 1 { "" } else { "s" }),
        items,
        problems,
        ids: all_ids,
        entities,
        notes,
        tiles: tiles_out,
    })
}
