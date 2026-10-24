//! The Inspector's Shape section for the active level (non-rectangular
//! levels): the void rects as exact values, each edit one `set_level_void`,
//! and the way into the map's Shape tool.

use std::collections::HashMap;

use worlds_core::Command;
use worlds_core::command::Rect;

use crate::app::actions::Action;
use crate::doc::{Document, LevelIx};
use crate::tools::Tool;
use crate::view::ViewState;

/// `[x, y, w, h]` typed as `x, y, w, h` (commas or spaces).
fn parse_rect(s: &str) -> Option<Rect> {
    let n: Vec<u32> = s
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|p| !p.is_empty())
        .map(|p| p.parse().ok())
        .collect::<Option<_>>()?;
    match n[..] {
        [x, y, w, h] => Some(Rect::new(x, y, w, h)),
        _ => None,
    }
}

fn show(r: &Rect) -> String {
    format!("{}, {}, {}, {}", r.x, r.y, r.w, r.h)
}

/// The Shape section: "Rectangle" or the void rects (each editable as
/// `x, y, w, h`, × removes it), a row to cut another, Edit shape (the map's
/// Shape tool) and Make rectangular.
pub fn shape_section(
    ui: &mut egui::Ui,
    doc: &Document,
    view: &mut ViewState,
    pending: &mut Vec<Command>,
    bufs: &mut HashMap<String, String>,
    actions: &mut Vec<Action>,
    ix: LevelIx,
) {
    let l = doc.level(ix);
    ui.add_space(8.0);
    ui.strong("Shape");
    let set = |pending: &mut Vec<Command>, void: Vec<Rect>| {
        pending.push(Command::SetLevelVoid {
            level: l.slug.clone(),
            void,
            restore: false,
        });
    };
    if l.void.is_empty() {
        ui.weak("Rectangle: no cells cut out");
    } else {
        let cells = l.cell_count();
        let pieces = l.pieces().len();
        ui.weak(format!(
            "{cells} of {} cells{}; void rects [x, y, w, h] in project cells:",
            l.size[0] as usize * l.size[1] as usize,
            if pieces > 1 {
                format!(", split into {pieces} pieces")
            } else {
                String::new()
            }
        ));
        egui::Grid::new("level void")
            .num_columns(3)
            .striped(true)
            .show(ui, |ui| {
                for (i, r) in l.void.iter().enumerate() {
                    // Keyed by the rect too, so an undo shows the new value.
                    let key = format!("{}:void:{i}:{}", l.id, show(r));
                    let b = bufs.entry(key.clone()).or_insert_with(|| show(r));
                    let resp = ui.add(egui::TextEdit::singleline(b).desired_width(130.0));
                    let typed = parse_rect(b);
                    let commit = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let clicked = ui
                        .add_enabled(typed.is_some_and(|nr| nr != *r), egui::Button::new("Set"))
                        .on_hover_text("x, y, w, h in project cells")
                        .clicked();
                    if (commit || clicked)
                        && let Some(nr) = typed.filter(|nr| nr != r)
                    {
                        let mut void = l.void.clone();
                        void[i] = nr;
                        set(pending, void);
                        bufs.remove(&key);
                    }
                    if ui
                        .small_button("×")
                        .on_hover_text("Fill it back in")
                        .clicked()
                    {
                        let mut void = l.void.clone();
                        void.remove(i);
                        set(pending, void);
                        bufs.retain(|k, _| !k.starts_with(&format!("{}:void:", l.id)));
                    }
                    ui.end_row();
                }
            });
    }
    ui.horizontal(|ui| {
        let key = format!("{}:void:new", l.id);
        let b = bufs.entry(key.clone()).or_default();
        ui.add(
            egui::TextEdit::singleline(b)
                .hint_text("x, y, w, h")
                .desired_width(130.0),
        );
        let typed = parse_rect(b);
        if ui
            .add_enabled(typed.is_some(), egui::Button::new("Cut out"))
            .on_hover_text("Add this rect of project cells to the void")
            .clicked()
            && let Some(r) = typed
        {
            let mut void = l.void.clone();
            void.push(r);
            set(pending, void);
            bufs.remove(&key);
        }
    });
    ui.horizontal(|ui| {
        if ui
            .button("Edit shape")
            .on_hover_text("On the map: drag to cut cells out, Shift-drag to fill them back in")
            .clicked()
        {
            actions.push(Action::SetTool(Tool::Shape));
            view.set_status(
                format!(
                    "Shape {}: drag to cut cells out, Shift-drag to fill them back in; Escape \
                     when done",
                    l.slug
                ),
                false,
            );
        }
        if !l.void.is_empty() && ui.button("Make rectangular").clicked() {
            set(pending, Vec::new());
            bufs.retain(|k, _| !k.starts_with(&format!("{}:void:", l.id)));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rects_parse_with_commas_or_spaces() {
        assert_eq!(parse_rect("1, 2, 3, 4"), Some(Rect::new(1, 2, 3, 4)));
        assert_eq!(parse_rect("1 2 3 4"), Some(Rect::new(1, 2, 3, 4)));
        assert_eq!(parse_rect("1, 2, 3"), None);
        assert_eq!(parse_rect("a, 2, 3, 4"), None);
        assert_eq!(show(&Rect::new(5, 0, 2, 9)), "5, 0, 2, 9");
    }
}
