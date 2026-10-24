//! Named editor actions: what the toolbar, shortcuts and the command
//! palette (Ctrl-P) trigger.

use crate::keymap::{Chord, Cmd, Keymap};
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
    /// Duplicate a level (its context menu): the copy goes just right of
    /// it and becomes the active level and the selection.
    DuplicateLevel(crate::doc::LevelIx),
    /// Ctrl+C: copy the selection (and the grid marquee) as a fragment.
    Copy,
    /// Ctrl+X: copy, then remove what was copied.
    Cut,
    /// Ctrl+V: paste the fragment in `text` (the system clipboard; `None`:
    /// the editor's own), as a ghost to place, or with `in_place`
    /// (Ctrl+Shift+V) at its original cells.
    Paste {
        text: Option<String>,
        in_place: bool,
    },
    /// The click that lands the armed paste ghost. `keep` (Shift) leaves
    /// it armed for the next click.
    PasteAt {
        level: crate::doc::LevelIx,
        origin: [i64; 2],
        keep: bool,
    },
    /// Crop the active level to the bounding box of its content.
    CropLevel,
    /// Show this world, fitted.
    SwitchWorld(worlds_core::Slug),
    /// Ask for a slug and a name, then create a world and show it.
    NewWorld,
    /// Rename a world (`None`: the one shown).
    RenameWorld(Option<worlds_core::Slug>),
    /// Delete a world (`None`: the one shown), after a confirmation.
    DeleteWorld(Option<worlds_core::Slug>),
    /// Move a level to another world and follow it there.
    MoveLevelToWorld {
        level: worlds_core::Slug,
        world: worlds_core::Slug,
    },
    /// Ctrl+F: the Find pane in front, its query box focused.
    ShowFind,
    /// Bring the History pane to the front.
    ShowHistory,
    /// Undo or redo until this many entries are done (a click in the
    /// History pane).
    HistoryJump(usize),
    /// Duplicate a layer (`None`: the active one) as `<slug>_copy`.
    DuplicateLayer(Option<worlds_core::Slug>),
    /// Merge a tiles layer (`None`: the active one) into the tiles layer
    /// below it.
    MergeLayerDown(Option<worlds_core::Slug>),
    /// Move the selected tile stacks or entities to a layer (`None`: the
    /// active one) of the same kind.
    MoveSelectionToLayer(Option<worlds_core::Slug>),
    /// Pick a PNG and add it as a reference image at this scope (`None`:
    /// the active level, else the world).
    AddReference(Option<worlds_core::command::NoteScope>),
    /// Bring the Inspector to the front.
    ShowInspector,
    /// Import from Worlds project…: pick a project, then the import dialog.
    ImportProject,
    /// A new map tab next to the focused one, on the world it shows,
    /// fitted, and focused.
    NewViewport,
    /// A new map tab with the same world, level and camera as this one
    /// (`None`: the focused one), focused.
    DuplicateViewport(Option<crate::view::ViewportId>),
    /// The `+` at the end of a tab bar: a new map tab in that leaf (on the
    /// world of the map tab there, else the focused one's).
    NewViewportAt(egui_dock::NodePath),
    /// Close a map tab (`None`: the focused one); the last one stays.
    CloseViewport(Option<crate::view::ViewportId>),
    /// Link two map tabs' cameras (`None`: the focused tab and the next
    /// one), or with the same pair linked, unlink them.
    LinkCameras(Option<[crate::view::ViewportId; 2]>),
    /// Open the bookmark list with its name field focused, to save the
    /// focused tab's view.
    SaveBookmark,
    /// Show bookmark `n` (its index) in the focused tab.
    JumpBookmark(usize),
    /// Forget bookmark `n`.
    DeleteBookmark(usize),
    /// Save the focused tab's view as this bookmark (empty: "View n").
    SaveBookmarkAs(String),
    /// X, Y, R: turn the armed paste ghost (or assembly), else the tile
    /// stamp, by `x` (mirror left-right), `y` (top-bottom) or `xt` (a
    /// quarter turn clockwise).
    Turn(worlds_core::Flip),
    /// Mirror or rotate the selection in place (one entry).
    TransformSelection(worlds_core::transform::TransformOp),
    /// Align, distribute or butt together the selected levels (one entry).
    ArrangeLevels(worlds_core::transform::ArrangeOp),
    /// Ctrl+Shift+S: the "Save selection as assembly…" dialog.
    OpenSaveAssembly,
    /// The dialog's Save: the selection as assembly `info` (one entry).
    SaveAssembly {
        info: worlds_core::assembly::AssemblyInfo,
        replace: bool,
    },
    /// A Library click: arm the assembly as a paste ghost to place.
    ArmAssembly(worlds_core::Slug),
    /// Show the Library in the Palette pane (or the layer's palette again).
    ToggleLibrary,
    /// Ctrl+L: the Levels pane in front, its search box focused.
    ShowLevels,
    /// Ctrl+G: one object group of the selected entities and annotations.
    GroupSelection,
    /// Ctrl+Shift+G: remove the groups of the selected members.
    UngroupSelection,
    /// The Preferences window (shortcuts).
    OpenPreferences,
}

impl Action {
    /// Everything the command palette lists: (name, shortcut, action). The
    /// shortcuts are read from `km`, so they follow a remap.
    pub fn catalog(km: &Keymap) -> Vec<(String, String, Action)> {
        let k = |c: Cmd| km.label(c);
        let none = String::new;
        let mut v: Vec<(String, String, Action)> = vec![
            ("Open project…".into(), k(Cmd::Open), Action::OpenDialog),
            (
                "Reload project from disk".into(),
                k(Cmd::Reload),
                Action::Reload,
            ),
            ("Save".into(), k(Cmd::Save), Action::Save),
            ("Undo".into(), k(Cmd::Undo), Action::Undo),
            ("Redo".into(), k(Cmd::Redo), Action::Redo),
            ("Fit world".into(), k(Cmd::FitWorld), Action::FitWorld),
            ("Fit level".into(), k(Cmd::FitLevel), Action::FitLevel),
            ("Zoom in".into(), k(Cmd::ZoomIn), Action::ZoomIn),
            ("Zoom out".into(), k(Cmd::ZoomOut), Action::ZoomOut),
            ("Zoom 100%".into(), k(Cmd::ZoomActual), Action::ZoomActual),
            ("Toggle grid".into(), k(Cmd::ToggleGrid), Action::ToggleGrid),
            (
                "Group the selection".into(),
                k(Cmd::Group),
                Action::GroupSelection,
            ),
            (
                "Ungroup the selection".into(),
                k(Cmd::Ungroup),
                Action::UngroupSelection,
            ),
            ("Next level".into(), k(Cmd::NextLevel), Action::NextLevel),
            (
                "Previous level".into(),
                k(Cmd::PrevLevel),
                Action::PrevLevel,
            ),
            ("New level…".into(), none(), Action::NewLevel(None)),
            ("Delete level…".into(), k(Cmd::Delete), Action::DeleteLevel),
            ("Crop level to content".into(), none(), Action::CropLevel),
            ("Reset panel layout".into(), none(), Action::ResetLayout),
            ("Clear log".into(), none(), Action::ClearLog),
            (
                "Place entity".into(),
                k(Cmd::PlaceEntity),
                Action::PlaceEntity,
            ),
            (
                "Duplicate selection".into(),
                k(Cmd::Duplicate),
                Action::DuplicateSelection,
            ),
            (
                "Delete selection".into(),
                k(Cmd::Delete),
                Action::DeleteSelection,
            ),
            ("Copy selection".into(), fixed("Ctrl+C"), Action::Copy),
            ("Cut selection".into(), fixed("Ctrl+X"), Action::Cut),
            (
                "Paste".into(),
                fixed("Ctrl+V"),
                Action::Paste {
                    text: None,
                    in_place: false,
                },
            ),
            (
                "Paste in place".into(),
                fixed("Ctrl+Shift+V"),
                Action::Paste {
                    text: None,
                    in_place: true,
                },
            ),
            (
                "Select all entities in the level".into(),
                k(Cmd::SelectAll),
                Action::SelectAll,
            ),
            ("Edit level fields".into(), none(), Action::OpenTypes(None)),
            (
                "Notes: show the Notes pane".into(),
                none(),
                Action::ShowNotes,
            ),
            ("New world…".into(), none(), Action::NewWorld),
            ("Rename world…".into(), none(), Action::RenameWorld(None)),
            ("Delete world…".into(), none(), Action::DeleteWorld(None)),
            (
                "Find: search the whole project".into(),
                k(Cmd::ShowFind),
                Action::ShowFind,
            ),
            (
                "History: show the History pane".into(),
                none(),
                Action::ShowHistory,
            ),
            (
                "Layer: duplicate the active layer".into(),
                none(),
                Action::DuplicateLayer(None),
            ),
            (
                "Layer: merge the active layer down".into(),
                none(),
                Action::MergeLayerDown(None),
            ),
            (
                "Selection: move to the active layer".into(),
                none(),
                Action::MoveSelectionToLayer(None),
            ),
            (
                "Add reference image…".into(),
                none(),
                Action::AddReference(None),
            ),
            (
                "Import from Worlds project…".into(),
                none(),
                Action::ImportProject,
            ),
            ("New viewport".into(), none(), Action::NewViewport),
            (
                "Duplicate viewport".into(),
                none(),
                Action::DuplicateViewport(None),
            ),
            ("Close viewport".into(), none(), Action::CloseViewport(None)),
            (
                "Link cameras (focused and next viewport)".into(),
                none(),
                Action::LinkCameras(None),
            ),
            ("Bookmark this view…".into(), none(), Action::SaveBookmark),
            (
                "Levels: show the Levels pane".into(),
                k(Cmd::ShowLevels),
                Action::ShowLevels,
            ),
            (
                "Preferences…".into(),
                k(Cmd::Preferences),
                Action::OpenPreferences,
            ),
        ];
        use super::project_pane::Section;
        for (name, s) in [
            ("Project: layers (add, edit, reorder)", Section::Layers),
            ("Project: tilesets (add an image)", Section::Tilesets),
            ("Project: settings (grid, collision)", Section::Settings),
        ] {
            v.push((name.into(), none(), Action::OpenProject(s)));
        }
        v.push((
            "Tool: Paint (last mode)".into(),
            k(Cmd::PaintTool),
            Action::PaintTool,
        ));
        for t in Tool::ALL {
            let name = if t.is_paint_mode() {
                format!("Tool: Paint: {}", t.name())
            } else {
                format!("Tool: {}", t.name())
            };
            v.push((name, k(Cmd::Tool(t)), Action::SetTool(t)));
        }
        v.push((
            "Save selection as assembly…".into(),
            k(Cmd::SaveAssembly),
            Action::OpenSaveAssembly,
        ));
        v.push((
            "Library: assemblies in the Palette".into(),
            none(),
            Action::ToggleLibrary,
        ));
        use worlds_core::transform::{ArrangeOp, TransformOp};
        for op in [
            TransformOp::MirrorX,
            TransformOp::MirrorY,
            TransformOp::Rotate90,
        ] {
            v.push((
                crate::arrange::op_title(op).into(),
                none(),
                Action::TransformSelection(op),
            ));
        }
        for (flip, what) in [
            (worlds_core::Flip::X, "mirror left-right"),
            (worlds_core::Flip::Y, "mirror top-bottom"),
            (worlds_core::Flip::XT, "rotate 90°"),
        ] {
            v.push((
                format!("Brush: {what} (the paste or the tile stamp)"),
                k(Cmd::Turn(flip)),
                Action::Turn(flip),
            ));
        }
        for op in ArrangeOp::ALL {
            v.push((
                format!("Levels: {}", op.title()),
                none(),
                Action::ArrangeLevels(op),
            ));
        }
        v
    }
}

/// A key that isn't remappable (the clipboard's), printed for this
/// platform (`Cmd+C` on a Mac).
pub fn fixed(chord: &str) -> String {
    Chord::parse(chord).map_or_else(|_| chord.to_string(), |c| c.to_string())
}

/// The shortcut of bookmark `n` (Ctrl+Shift+1..9 for the first nine by
/// default), `""` past those or when unbound.
pub fn bookmark_key(km: &Keymap, n: usize) -> String {
    if n < crate::keymap::BOOKMARK_KEYS {
        km.label(Cmd::JumpBookmark(n as u8))
    } else {
        String::new()
    }
}

/// The command palette's entries for the bookmarks: "Bookmark: <name>".
pub fn bookmark_entries(
    bookmarks: &[crate::view::Bookmark],
    km: &Keymap,
) -> Vec<(String, String, Action)> {
    bookmarks
        .iter()
        .enumerate()
        .map(|(i, b)| {
            (
                format!("Bookmark: {}", b.name),
                bookmark_key(km, i),
                Action::JumpBookmark(i),
            )
        })
        .collect()
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
        let c = Action::catalog(&Keymap::defaults());
        let mut names: Vec<&str> = c.iter().map(|(n, _, _)| n.as_str()).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), c.len());
    }
}
