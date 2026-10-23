//! Named editor actions: what the toolbar, shortcuts and the command
//! palette (Ctrl-P) trigger.

use crate::tools::Tool;

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    OpenDialog,
    Reload,
    Save,
    Undo,
    Redo,
    FitWorld,
    FitLevel,
    ZoomIn,
    ZoomOut,
    ZoomActual,
    SetTool(Tool),
    /// The Paint button and P: the Paint group's last mode
    /// (`ViewState::paint_tool`).
    PaintTool,
    ToggleGrid,
    NextLevel,
    PrevLevel,
    ResetLayout,
    ClearLog,
    CommandPalette,
    /// Ask for a slug, then create a level at this world pixel (default:
    /// the middle of the viewport).
    NewLevel(Option<[f64; 2]>),
    /// Delete the active level, after a confirmation.
    DeleteLevel,
    /// Entity tool: place the brush at the hovered cell (or the middle of
    /// the active level).
    PlaceEntity,
    /// Delete the selected entities (Delete with a selection).
    DeleteSelection,
    /// Ctrl-D: duplicate the selected entities one cell to the right.
    DuplicateSelection,
    /// Arrow keys: move the selected entities by a cell, or with `pixel`
    /// (Shift) by a pixel.
    Nudge {
        d: [i32; 2],
        pixel: bool,
    },
    /// Open the Types pane on an entity type (`None`: the level fields).
    OpenTypes(Option<worlds_core::Slug>),
    /// Duplicate an entity type under a new slug and open the copy.
    DuplicateType(worlds_core::Slug),
    /// Select every entity of the active level.
    SelectAll,
    /// Apply one command as its own undo entry (the Note tool's add).
    Apply(Box<worlds_core::Command>),
    /// Enter or a double click: finish the path or polygon being drawn
    /// (or the measurement).
    FinishDrawing,
    /// Backspace while drawing: drop the last vertex.
    DrawingBackspace,
    /// Bring the Notes pane to the front.
    ShowNotes,
    /// The Project pane on a section (layers, tilesets, settings).
    OpenProject(super::project_pane::Section),
    /// Crop the active level to the bounding box of its content.
    CropLevel,
}

impl Action {
    /// Everything the command palette lists: (name, shortcut, action).
    pub fn catalog() -> Vec<(String, &'static str, Action)> {
        let mut v: Vec<(String, &'static str, Action)> = vec![
            ("Open project…".into(), "Ctrl+O", Action::OpenDialog),
            ("Reload project from disk".into(), "Ctrl+R", Action::Reload),
            ("Save".into(), "Ctrl+S", Action::Save),
            ("Undo".into(), "Ctrl+Z", Action::Undo),
            ("Redo".into(), "Ctrl+Shift+Z", Action::Redo),
            ("Fit world".into(), "Shift+F", Action::FitWorld),
            ("Fit level".into(), "F", Action::FitLevel),
            ("Zoom in".into(), "+", Action::ZoomIn),
            ("Zoom out".into(), "-", Action::ZoomOut),
            ("Zoom 100%".into(), "1", Action::ZoomActual),
            ("Toggle grid".into(), "Ctrl+G", Action::ToggleGrid),
            ("Next level".into(), "PgDn", Action::NextLevel),
            ("Previous level".into(), "PgUp", Action::PrevLevel),
            ("New level…".into(), "", Action::NewLevel(None)),
            ("Delete level…".into(), "Del", Action::DeleteLevel),
            ("Crop level to content".into(), "", Action::CropLevel),
            ("Reset panel layout".into(), "", Action::ResetLayout),
            ("Clear log".into(), "", Action::ClearLog),
            ("Place entity".into(), "Enter", Action::PlaceEntity),
            (
                "Duplicate selection".into(),
                "Ctrl+D",
                Action::DuplicateSelection,
            ),
            ("Delete selection".into(), "Del", Action::DeleteSelection),
            (
                "Select all entities in the level".into(),
                "Ctrl+A",
                Action::SelectAll,
            ),
            ("Edit level fields".into(), "", Action::OpenTypes(None)),
            ("Notes: show the Notes pane".into(), "", Action::ShowNotes),
        ];
        use super::project_pane::Section;
        for (name, s) in [
            ("Project: layers (add, edit, reorder)", Section::Layers),
            ("Project: tilesets (add an image)", Section::Tilesets),
            ("Project: settings (grid, collision)", Section::Settings),
        ] {
            v.push((name.into(), "", Action::OpenProject(s)));
        }
        v.push(("Tool: Paint (last mode)".into(), "P", Action::PaintTool));
        for t in Tool::ALL {
            let name = if t.is_paint_mode() {
                format!("Tool: Paint: {}", t.name())
            } else {
                format!("Tool: {}", t.name())
            };
            v.push((name, t.shortcut(), Action::SetTool(t)));
        }
        v
    }
}

/// What Delete does: remove the selected entities when there are any,
/// else ask to delete the active level.
pub fn delete_action(view: &crate::view::ViewState) -> Action {
    if view.selection.is_empty() {
        Action::DeleteLevel
    } else {
        Action::DeleteSelection
    }
}

/// Case-insensitive subsequence match of `query` in `text`. Higher is
/// better; `None` if some query character doesn't appear in order.
/// Consecutive matches and matches at word starts score extra, so "fw"
/// prefers "Fit world" over "Next level from wherever".
pub fn fuzzy_score(query: &str, text: &str) -> Option<i32> {
    fuzzy_score_chars(&fuzzy_query(query), text)
}

/// A query prepared for [`fuzzy_score_chars`]: lowercase, no whitespace.
pub fn fuzzy_query(query: &str) -> Vec<char> {
    query
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}

/// [`fuzzy_score`] with the query prepared once ([`fuzzy_query`]), for
/// scoring many texts; allocates nothing.
pub fn fuzzy_score_chars(q: &[char], text: &str) -> Option<i32> {
    if q.is_empty() {
        return Some(0);
    }
    let mut qi = 0;
    let mut score = 0;
    let mut prev_match: Option<usize> = None;
    let mut prev_char: Option<char> = None;
    let mut len = 0usize;
    for (i, c) in text.chars().enumerate() {
        len = i + 1;
        if qi < q.len() && c.to_lowercase().eq(std::iter::once(q[qi])) {
            let word_start = prev_char.is_none_or(|p| !p.is_alphanumeric());
            score += 1;
            if word_start {
                score += 8;
            }
            if prev_match == Some(i.wrapping_sub(1)) {
                score += 5;
            }
            if let Some(p) = prev_match {
                score -= (i - p - 1).min(5) as i32;
            }
            prev_match = Some(i);
            qi += 1;
        }
        prev_char = Some(c);
    }
    (qi == q.len()).then_some(score - len as i32 / 8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy() {
        assert!(fuzzy_score("fw", "Fit world").is_some());
        assert!(fuzzy_score("wf", "Fit world").is_none());
        assert!(fuzzy_score("", "anything") == Some(0));
        assert!(fuzzy_score("BRUSH", "Tool: Brush").is_some());
        let a = fuzzy_score("fl", "Fit level").unwrap();
        let b = fuzzy_score("fl", "Tool: Fill").unwrap();
        assert!(a > b, "{a} vs {b}");
        let a = fuzzy_score("reload", "Reload project from disk").unwrap();
        let b = fuzzy_score("reload", "Reset panel layout").unwrap_or(i32::MIN);
        assert!(a > b);
    }

    #[test]
    fn delete_takes_the_selection_first() {
        let mut v = crate::view::ViewState::new();
        assert_eq!(delete_action(&v), Action::DeleteLevel);
        v.selection
            .set_entities([worlds_core::Id::new("doorcrev22").unwrap()]);
        assert_eq!(delete_action(&v), Action::DeleteSelection);
        v.selection.clear();
        assert_eq!(delete_action(&v), Action::DeleteLevel);
    }

    #[test]
    fn catalog_names_unique() {
        let c = Action::catalog();
        let mut names: Vec<&str> = c.iter().map(|(n, _, _)| n.as_str()).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), c.len());
    }
}
