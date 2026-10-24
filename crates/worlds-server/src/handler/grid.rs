//! `grid.*`: `int_grid` layers. Cells are `[x, y]` in the layer's grid,
//! values are value slugs, `null` is empty. On a derived layer reads give
//! the effective values (or, with `stored`, the override plane) and writes
//! edit the overrides, where `"!"` suppresses a cell.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::{Value, json};
use worlds_core::command::{CellValue, GridValue, line_cells};
use worlds_core::{Command, DerivedOverrides, Override, Project, Slug};

use super::params::{self, check_rect, find_level, layer_of_kind, layer_size};
use super::{Handler, Write, with_rev};
use crate::host::Host;
use crate::rpc::RpcError;

/// `grid.get_cells { level, layer, rect?, stored? }`.
pub(crate) fn get_cells(
    h: &mut Handler,
    host: &mut dyn Host,
    method: &str,
    p: Value,
) -> Result<Value, RpcError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        level: Slug,
        layer: Slug,
        rect: Option<[u32; 4]>,
        #[serde(default)]
        stored: bool,
    }
    let p: P = params::parse(method, p)?;
    let mut doc = host.doc();
    let project: &Project = doc.project;
    let (_, level) = find_level(project, p.level.as_str())?;
    let def = layer_of_kind(project, p.layer.as_str(), "int_grid")?.clone();
    let size = layer_size(project, &def, level);
    let rect = p.rect.unwrap_or([0, 0, size[0], size[1]]);
    check_rect(&def, rect, size)?;
    let glyph = |i: u16| -> char {
        match i {
            0 => '.',
            i => def.values().get(i as usize - 1).map_or('.', |v| v.glyph),
        }
    };
    let [x0, y0, w, hh] = rect;
    let cell_rows = |f: &dyn Fn(u32, u32) -> char| -> Vec<String> {
        (y0..y0 + hh)
            .map(|y| (x0..x0 + w).map(|x| f(x, y)).collect())
            .collect()
    };
    let mut legend: BTreeMap<String, Value> = std::iter::once((".".to_string(), Value::Null))
        .chain(
            def.values()
                .iter()
                .map(|v| (v.glyph.to_string(), json!(v.slug))),
        )
        .collect();
    let rows = if !def.is_derived() {
        let grid = level.int_grid(p.layer.as_str());
        cell_rows(&|x, y| glyph(grid.and_then(|g| g.index_at(x, y)).unwrap_or(0)))
    } else if p.stored {
        let ov: Option<&DerivedOverrides> = level.overrides(p.layer.as_str());
        // As in the layer's file: `.` (no override) and `!` (suppressed)
        // are built in; `.` isn't "empty" here, so it isn't listed.
        legend.remove(".");
        legend.insert(
            DerivedOverrides::SUPPRESS.to_string(),
            json!(DerivedOverrides::SUPPRESS.to_string()),
        );
        cell_rows(&|x, y| match ov.and_then(|o| o.get(x, y)) {
            Some(Override::Value(i)) => glyph(i),
            Some(Override::Suppress) => DerivedOverrides::SUPPRESS,
            _ => '.',
        })
    } else {
        let grid = super::baked::derived_grid(&mut doc, p.level.as_str(), p.layer.as_str())?;
        cell_rows(&|x, y| glyph(grid.index_at(x, y).unwrap_or(0)))
    };
    with_rev(
        h.rev,
        json!({ "rect": rect, "legend": legend, "rows": rows }),
    )
}

/// `grid.set_cells`: `cells` triples, or a `text` block at `origin`.
pub(crate) fn set_cells(project: &Project, method: &str, p: Value) -> Result<Write, RpcError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        level: Slug,
        layer: Slug,
        cells: Option<Vec<CellValue>>,
        text: Option<String>,
        origin: Option<[u32; 2]>,
        legend: Option<BTreeMap<String, GridValue>>,
        #[serde(default)]
        keep_empty: bool,
    }
    let p: P = params::parse(method, p)?;
    let cells = match (p.cells, p.text) {
        (Some(_), Some(_)) => {
            return Err(RpcError::invalid_params(
                "give either cells or text with origin, not both",
            ));
        }
        (None, None) => {
            return Err(RpcError::invalid_params(
                "give either cells ([x, y, value] triples) or text with origin",
            ));
        }
        (Some(cells), None) => {
            if p.origin.is_some() || p.legend.is_some() {
                return Err(RpcError::invalid_params(
                    "origin and legend go with text, not with cells",
                ));
            }
            cells
        }
        (None, Some(text)) => {
            let origin = p.origin.ok_or_else(|| {
                RpcError::invalid_params("text needs origin: the [x, y] of its first character")
            })?;
            let def = layer_of_kind(project, p.layer.as_str(), "int_grid")?;
            text_cells(def, &text, origin, p.legend.unwrap_or_default())?
        }
    };
    let cells = clip_void(project, &p.level, &p.layer, cells);
    Ok(Write::one(Command::PaintCells {
        level: p.level,
        layer: p.layer,
        cells,
        keep_empty: p.keep_empty,
    }))
}

/// `cells` without the writes of a value (or `"!"`) into the level's void
/// (a non-rectangular level's cut-out cells): grid writes clip at the void
/// as the editor's tools do. Clearing a void cell is kept. Unknown levels
/// and layers are left for the command to refuse.
pub(crate) fn clip_void(
    project: &Project,
    level: &Slug,
    layer: &Slug,
    mut cells: Vec<CellValue>,
) -> Vec<CellValue> {
    if let (Some(l), Some(def)) = (project.level(level.as_str()), project.layer(layer.as_str()))
        && l.has_void()
    {
        cells.retain(|c| {
            c.value == GridValue::Clear
                || !project.layer_cell_void(def, l, c.cell[0] as i64, c.cell[1] as i64)
        });
    }
    cells
}

/// The cells a text block paints (see `docs/api.md`, `grid.set_cells`):
/// `.` empty (on a derived layer: no override), `!` suppressed (derived
/// layers only; the command refuses it elsewhere).
fn text_cells(
    def: &worlds_core::LayerDef,
    text: &str,
    origin: [u32; 2],
    legend: BTreeMap<String, GridValue>,
) -> Result<Vec<CellValue>, RpcError> {
    let mut glyphs: BTreeMap<char, GridValue> = BTreeMap::new();
    for (k, v) in legend {
        let mut chars = k.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) => {
                if let GridValue::Value(v) = &v
                    && def.value(v.as_str()).is_none()
                {
                    return Err(RpcError::command(
                        &worlds_core::CommandError::UnknownValue {
                            layer: def.slug.clone(),
                            value: v.clone(),
                        },
                    ));
                }
                glyphs.insert(c, v);
            }
            _ => {
                return Err(RpcError::invalid_params(format!(
                    "legend keys are single glyphs, not {k:?}"
                )));
            }
        }
    }
    let text = text.strip_suffix('\n').unwrap_or(text);
    let mut out = Vec::new();
    for (dy, row) in text.split('\n').enumerate() {
        for (dx, c) in row.chars().enumerate() {
            let value = match glyphs.get(&c) {
                Some(v) => v.clone(),
                None => match c {
                    ' ' | '*' => continue,
                    '.' => GridValue::Clear,
                    DerivedOverrides::SUPPRESS => GridValue::Suppress,
                    c => match def.value_by_glyph(c) {
                        Some(v) => GridValue::Value(v.slug.clone()),
                        None => {
                            return Err(RpcError::invalid_params(format!(
                                "glyph {c:?} (row {dy}, column {dx} of text) is not a glyph of layer {} and not in legend; its glyphs are {}, {}",
                                def.slug,
                                if def.is_derived() {
                                    ". (no override), ! (suppressed)"
                                } else {
                                    ". (empty)"
                                },
                                def.values()
                                    .iter()
                                    .map(|v| format!("{} ({})", v.glyph, v.slug))
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            )));
                        }
                    },
                },
            };
            out.push(CellValue::new(
                origin[0].saturating_add(dx as u32),
                origin[1].saturating_add(dy as u32),
                value,
            ));
        }
    }
    Ok(out)
}

/// `grid.fill_rect` → `fill_rect`.
pub(crate) fn fill_rect(_: &Project, method: &str, p: Value) -> Result<Write, RpcError> {
    Ok(Write::one(params::command(method, "fill_rect", p)?))
}

/// `grid.flood_fill` → `flood_fill`.
pub(crate) fn flood_fill(_: &Project, method: &str, p: Value) -> Result<Write, RpcError> {
    Ok(Write::one(params::command(method, "flood_fill", p)?))
}

/// `grid.replace_value` → `replace_value`.
pub(crate) fn replace_value(_: &Project, method: &str, p: Value) -> Result<Write, RpcError> {
    Ok(Write::one(params::command(method, "replace_value", p)?))
}

/// `grid.line { level, layer, from, to, value }` → `paint_cells` with the
/// line tool's cells.
pub(crate) fn line(project: &Project, method: &str, p: Value) -> Result<Write, RpcError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        level: Slug,
        layer: Slug,
        from: [u32; 2],
        to: [u32; 2],
        value: GridValue,
    }
    let p: P = params::parse(method, p)?;
    // Both ends inside the layer before the line is drawn: the cells between
    // are then inside too, and there are at most width + height of them.
    let (_, level) = find_level(project, p.level.as_str())?;
    let def = layer_of_kind(project, p.layer.as_str(), "int_grid")?;
    let size = layer_size(project, def, level);
    for [x, y] in [p.from, p.to] {
        check_rect(def, [x, y, 1, 1], size)?;
    }
    let mut pts = Vec::new();
    line_cells(
        [p.from[0] as i32, p.from[1] as i32],
        [p.to[0] as i32, p.to[1] as i32],
        &mut pts,
    );
    let cells = pts
        .into_iter()
        .map(|[x, y]| CellValue::new(x as u32, y as u32, p.value.clone()))
        .collect();
    let cells = clip_void(project, &p.level, &p.layer, cells);
    Ok(Write::one(Command::PaintCells {
        level: p.level,
        layer: p.layer,
        cells,
        keep_empty: false,
    }))
}
