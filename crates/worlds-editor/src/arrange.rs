//! Selection transforms (Mirror X, Mirror Y, Rotate 90), arranging levels
//! (align, distribute, butt together) and level-edge snapping, for the
//! editor. worlds-core does the work ([`worlds_core::transform`]); this
//! module turns the selection and the grid marquee into its input and its
//! output back into a selection and a status message. Plain functions of
//! the document and the view, so `--script` drives exactly what the menus
//! do.

use worlds_core::Command;
use worlds_core::transform::{self, ArrangeOp, TransformOp};

use crate::annot::{Seg, find_note};
use crate::clipboard::{self, GridMarquee};
use crate::doc::{Document, LevelIx};
use crate::view::{SelItem, ViewState};

/// A selection transform, worked out.
#[derive(Debug, Clone, PartialEq)]
pub struct Turned {
    /// One batch: one undo entry.
    pub command: Command,
    /// For the status bar: the label, then what was left alone, clipped,
    /// or looks directional.
    pub message: String,
    /// The selection afterwards (entities and notes keep their ids; tile
    /// stacks are where they went).
    pub selection: Vec<SelItem>,
    /// The grid marquee afterwards (the turned cells).
    pub marquee: Option<GridMarquee>,
}

/// The menu name of a selection transform.
pub fn op_title(op: TransformOp) -> &'static str {
    match op {
        TransformOp::MirrorX => "Mirror selection left-right (X)",
        TransformOp::MirrorY => "Mirror selection top-bottom (Y)",
        TransformOp::Rotate90 => "Rotate selection 90° clockwise",
    }
}

/// Mirror or rotate the selection (and the grid marquee) in place about its
/// bounding box: what Copy would take, from one level; locked annotations
/// are left alone. Entities keep their ids, size and field values.
pub fn plan_transform(doc: &Document, view: &ViewState, op: TransformOp) -> Result<Turned, String> {
    let req = clipboard::copy_request(doc, view).map_err(|e| {
        e.replacen("Nothing to copy", "Nothing to turn", 1)
            .replacen(
                "Levels aren't copied: Ctrl+D duplicates a level",
                "Levels don't turn: arrange them instead",
                1,
            )
    })?;
    let mut sel = req.sel;
    let before = sel.notes.len();
    sel.notes
        .retain(|id| find_note(doc, id).is_some_and(|n| !n.note.locked));
    let locked = before - sel.notes.len();
    if sel.entities.is_empty() && sel.notes.is_empty() && sel.tiles.is_empty() && sel.rect.is_none()
    {
        return Err("Nothing to turn: the selected annotations are locked".into());
    }
    let plan = transform::selection(&doc.project, &sel, op.flip())
        .map_err(|e| format!("Can't turn the selection: {e}"))?;
    if plan.commands.is_empty() {
        return Err("Nothing moved: the selection is the same turned".into());
    }
    let mut message = plan.label.clone();
    for n in &plan.notes {
        message.push_str("; ");
        message.push_str(n);
    }
    if locked > 0 {
        message.push_str(&format!(
            "; {locked} locked annotation{} left alone",
            if locked == 1 { "" } else { "s" }
        ));
    }
    if req.elsewhere > 0 {
        message.push_str(&format!(
            "; {} selected in other levels left alone",
            req.elsewhere
        ));
    }
    // The selection afterwards: the same entities and notes, the tile
    // stacks where they went, levels as they were.
    let level_id = sel
        .level
        .as_ref()
        .and_then(|s| doc.level_ix(s.as_str()))
        .map(|ix| doc.ids[ix].clone());
    let mut selection: Vec<SelItem> = view
        .selection
        .items
        .iter()
        .filter(|i| matches!(i, SelItem::Entity(_) | SelItem::Note(_) | SelItem::Level(_)))
        .cloned()
        .collect();
    if let Some(level) = &level_id {
        for c in &plan.commands {
            if let Command::SetTiles { layer, tiles, .. } = c
                && sel.tiles.contains_key(layer)
            {
                let mut cells: Vec<[u32; 2]> = tiles.iter().map(|t| t.cell).collect();
                cells.dedup();
                selection.extend(cells.into_iter().map(|cell| SelItem::Tiles {
                    level: level.clone(),
                    layer: layer.clone(),
                    cell,
                }));
            }
        }
    }
    let marquee = req.marquee.and_then(|(ix, m)| {
        let l = doc.level(ix);
        let [x, y, w, h] = plan.rect;
        let x0 = x.clamp(0, l.size[0] as i64);
        let y0 = y.clamp(0, l.size[1] as i64);
        let x1 = (x + w).clamp(0, l.size[0] as i64);
        let y1 = (y + h).clamp(0, l.size[1] as i64);
        (x1 > x0 && y1 > y0).then(|| GridMarquee {
            rect: worlds_core::command::Rect::new(
                x0 as u32,
                y0 as u32,
                (x1 - x0) as u32,
                (y1 - y0) as u32,
            ),
            ..m
        })
    });
    Ok(Turned {
        command: plan.command(),
        message,
        selection,
        marquee,
    })
}

/// Align, distribute or butt together the selected levels: one batch of
/// `move_level` (empty when nothing moves). Fewer than two selected
/// levels, or levels of different worlds, is an error.
pub fn plan_arrange(doc: &Document, view: &ViewState, op: ArrangeOp) -> Result<Command, String> {
    let slugs: Vec<worlds_core::Slug> = view
        .selection
        .levels()
        .filter_map(|id| doc.level_ix_by_id(id))
        .map(|ix| doc.level(ix).slug.clone())
        .collect();
    if slugs.len() < 2 {
        return Err(format!(
            "{}: select two or more levels first (Shift+click or a marquee in the world)",
            op.title()
        ));
    }
    transform::arrange_levels(&doc.project, &slugs, op).map_err(|e| format!("Can't arrange: {e}"))
}

/// The tile stamp's X, Y, R in the Paint strip (a tiles layer): lit when
/// that part of the turn is on, a click is the key. A separate function so
/// the strip's own layout stays the strip's.
pub fn turn_buttons(
    ui: &mut egui::Ui,
    flip: Option<worlds_core::Flip>,
    actions: &mut Vec<crate::app::actions::Action>,
) {
    use crate::app::actions::Action;
    use worlds_core::Flip;
    let b = Flip::bits_of(flip);
    ui.separator();
    for (label, on, op, tip) in [
        (
            "⇆ X",
            b & 1 != 0,
            Flip::X,
            "Mirror the tile stamp left-right (X)",
        ),
        (
            "⇅ Y",
            b & 2 != 0,
            Flip::Y,
            "Mirror the tile stamp top-bottom (Y)",
        ),
        (
            "⟳ R",
            b & 4 != 0,
            Flip::XT,
            "Rotate the tile stamp 90° clockwise (R; square tiles)",
        ),
    ] {
        if ui
            .add(egui::Button::selectable(on, label).small())
            .on_hover_text(format!(
                "{tip}. Now: {}",
                crate::clipboard::describe_flip(flip)
            ))
            .clicked()
        {
            actions.push(Action::Turn(op));
        }
    }
}

/// The paste ghost's turn in words, under its bottom-left corner.
pub fn ghost_badge(painter: &egui::Painter, at: egui::Pos2, what: &str, accent: egui::Color32) {
    let text = format!("⟳ {what} (X, Y, R)");
    let g = painter.layout_no_wrap(text, egui::FontId::proportional(12.0), egui::Color32::WHITE);
    let r = egui::Rect::from_min_size(at + egui::vec2(0.0, 4.0), g.size() + egui::vec2(8.0, 4.0));
    painter.rect_filled(r, 3.0, accent.gamma_multiply(0.85));
    painter.galley(r.min + egui::vec2(4.0, 2.0), g, egui::Color32::WHITE);
}

/// The map's context menu entries for the selection: Save as assembly,
/// Mirror and Rotate (anything but levels selected, or a grid marquee),
/// Arrange levels (two or more levels selected).
pub fn context_menu(
    ui: &mut egui::Ui,
    doc: &Document,
    view: &ViewState,
    actions: &mut Vec<crate::app::actions::Action>,
) {
    use crate::app::actions::Action;
    let items = view
        .selection
        .items
        .iter()
        .any(|i| !matches!(i, SelItem::Level(_)))
        || view.grid_marquee.is_some();
    if items {
        ui.separator();
        if ui
            .button("Save selection as assembly…  Ctrl+Shift+S")
            .clicked()
        {
            actions.push(Action::OpenSaveAssembly);
            ui.close();
        }
        for (label, op) in [
            ("Mirror X (left-right)", TransformOp::MirrorX),
            ("Mirror Y (top-bottom)", TransformOp::MirrorY),
            ("Rotate 90° clockwise", TransformOp::Rotate90),
        ] {
            if ui.button(label).clicked() {
                actions.push(Action::TransformSelection(op));
                ui.close();
            }
        }
    }
    let levels = view
        .selection
        .levels()
        .filter(|id| doc.level_ix_by_id(id).is_some())
        .count();
    if levels >= 2 {
        ui.separator();
        ui.menu_button(format!("Arrange {levels} levels"), |ui| {
            for (i, op) in ArrangeOp::ALL.into_iter().enumerate() {
                if i == 6 || i == 8 {
                    ui.separator();
                }
                if ui.button(op.title()).clicked() {
                    actions.push(Action::ArrangeLevels(op));
                    ui.close();
                }
            }
        });
    }
}

/// Where a level drag can snap to other levels' edges: per axis, the x (or
/// y) of every left and right (top and bottom) edge of the levels that sit
/// in the same world as the moving ones and aren't moving, each with the
/// edge as a segment (what the map highlights). The guide snapping of
/// [`crate::annot::drag_delta`] takes these as more candidates, within the
/// same 8 screen px.
pub fn level_edges(doc: &Document, moving: &[LevelIx]) -> [Vec<(i64, Option<Seg>)>; 2] {
    let mut out: [Vec<(i64, Option<Seg>)>; 2] = [Vec::new(), Vec::new()];
    let worlds: Vec<usize> = moving
        .iter()
        .filter(|&&ix| ix < doc.levels.len())
        .map(|&ix| doc.levels[ix].world)
        .collect();
    for ix in 0..doc.levels.len() {
        if moving.contains(&ix) || !worlds.contains(&doc.levels[ix].world) {
            continue;
        }
        let r = crate::viewport::level_wrect(doc, ix);
        for x in [r.min[0], r.max[0]] {
            out[0].push((x.round() as i64, Some(([x, r.min[1]], [x, r.max[1]]))));
        }
        for y in [r.min[1], r.max[1]] {
            out[1].push((y.round() as i64, Some(([r.min[0], y], [r.max[0], y]))));
        }
    }
    out
}
