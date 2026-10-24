//! Keyboard shortcuts: one table of every command a key can trigger, and
//! the user's bindings on top of it.
//!
//! The idea is that a key press goes through exactly one lookup. The app
//! asks the [`Keymap`] "which command does this press mean, given what's
//! active right now?", gets a [`Cmd`] back and turns it into the same
//! [`Action`] the toolbar and the command palette push. Going the other
//! way, tooltips and the palette ask "what's the chord for Undo?" so they
//! never print a stale `Ctrl+Z` after a remap.
//!
//! Each command has a stable id (`undo`, `tool.paint`, `view.fit_world`,
//! `bookmark.jump_3`), which is what the preferences file stores, a name for
//! the Preferences window, default chords and a [`Context`] saying when it
//! applies. Most commands have one default chord; a few have an alternate
//! (Redo is Ctrl+Shift+Z or Ctrl+Y, Reload is Ctrl+R or F5). The first
//! chord is the one tooltips show.
//!
//! Matching follows egui's "logical" rule: extra Shift or Alt held down
//! doesn't stop a chord from matching, so `+` matches whether or not your
//! keyboard needs Shift for it. When several bindings match one press the
//! best one wins: an exact modifier match beats a loose one (Ctrl+Shift+Z
//! is Redo, not Undo with Shift held), then the more specific context (with
//! a paste ghost to turn, R rotates it instead of picking the Rectangle
//! mode), then the chord with more modifiers.
//!
//! Not everything is remappable. These keys stay fixed:
//!
//! - Ctrl+C, Ctrl+X, Ctrl+V and Ctrl+Shift+V: the windowing layer turns
//!   them into clipboard events (reading the system clipboard for a paste)
//!   before the editor ever sees a key, so there's no key press to remap.
//! - Escape: cancels whatever is in progress (a prompt, a gesture, an armed
//!   paste, a canvas pick, a rebind in the Preferences window). One key
//!   that always backs out is worth more than the freedom to move it.
//! - Enter and Escape in prompts and dialogs, Enter in text fields: they
//!   belong to the widget that has focus.
//! - Space (hold to pan), and Shift, Ctrl and Alt as drag and click
//!   modifiers (Shift-drag outlines a rectangle, Ctrl-click adds to the
//!   selection, Alt-click picks): they change a gesture rather than run a
//!   command.
//! - Text editing keys inside text fields (arrows, Ctrl+A, Ctrl+Z in a
//!   field are the field's own), and Ctrl+Enter committing a multiline
//!   field.
//! - The command palette's arrows, Enter and Escape.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::LazyLock;

use egui::{Key, Modifiers};

use crate::app::actions::Action;
use crate::tools::Tool;

/// Print the Command modifier as "Cmd" (and Control as "Control")?
pub const IS_MAC: bool = cfg!(target_os = "macos");

/// The modifier half of a chord. `cmd` is the primary modifier, egui's
/// [`Modifiers::COMMAND`]: Ctrl on Linux and Windows, Cmd on macOS, so one
/// binding means the same thing everywhere. `ctrl` is the macOS Control key
/// as a separate modifier; elsewhere it is folded into `cmd`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Mods {
    pub cmd: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl Mods {
    fn egui(self) -> Modifiers {
        let mut m = Modifiers::NONE;
        if self.cmd {
            m |= Modifiers::COMMAND;
        }
        if self.ctrl {
            m |= Modifiers::CTRL;
        }
        if self.alt {
            m |= Modifiers::ALT;
        }
        if self.shift {
            m |= Modifiers::SHIFT;
        }
        m
    }

    fn count(self) -> u8 {
        self.cmd as u8 + self.ctrl as u8 + self.alt as u8 + self.shift as u8
    }
}

/// A key with its modifiers: `Ctrl+Shift+Z`, `F`, `PgDn`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Chord {
    pub mods: Mods,
    pub key: Key,
}

/// Short names for keys whose egui names are long or not what a keyboard
/// says on the cap. Parsing takes both these and egui's names.
const KEY_NAMES: &[(Key, &str)] = &[
    (Key::PageUp, "PgUp"),
    (Key::PageDown, "PgDn"),
    (Key::Delete, "Del"),
    (Key::Insert, "Ins"),
    (Key::Escape, "Esc"),
    (Key::Plus, "+"),
    (Key::Minus, "-"),
    (Key::Equals, "="),
    (Key::Comma, ","),
    (Key::Period, "."),
    (Key::Slash, "/"),
    (Key::Backslash, "\\"),
    (Key::Semicolon, ";"),
    (Key::Quote, "'"),
    (Key::Backtick, "`"),
    (Key::OpenBracket, "["),
    (Key::CloseBracket, "]"),
];

fn key_name(k: Key) -> &'static str {
    KEY_NAMES
        .iter()
        .find(|(key, _)| *key == k)
        .map_or(k.name(), |(_, n)| n)
}

fn parse_key(s: &str) -> Option<Key> {
    if let Some((k, _)) = KEY_NAMES.iter().find(|(_, n)| n.eq_ignore_ascii_case(s)) {
        return Some(*k);
    }
    Key::from_name(s).or_else(|| {
        Key::ALL
            .iter()
            .copied()
            .find(|k| k.name().eq_ignore_ascii_case(s))
    })
}

impl Chord {
    pub const fn new(mods: Mods, key: Key) -> Self {
        Chord { mods, key }
    }

    /// Parse `Ctrl+Shift+Z`, `Cmd+,`, `Shift+F`, `PgDn`, `+`, `Ctrl++`.
    /// Modifier names are case-insensitive: `Ctrl`, `Cmd` and `Command` are
    /// all the primary modifier (so a file written on Linux means the same
    /// on a Mac), `Control` is the Mac's Control key, `Alt` (or `Option`),
    /// `Shift`.
    pub fn parse(s: &str) -> Result<Chord, String> {
        Self::parse_for(s, IS_MAC)
    }

    /// [`Chord::parse`] as on a Mac (`mac`) or not.
    pub fn parse_for(s: &str, mac: bool) -> Result<Chord, String> {
        let s = s.trim();
        if s.is_empty() {
            return Err("an empty shortcut".into());
        }
        // The key is after the last `+`, except that the key may be `+`
        // itself (`+`, `Ctrl++`).
        let (mods_part, key_part) = if s == "+" {
            ("", "+")
        } else if let Some(rest) = s.strip_suffix("++") {
            (rest, "+")
        } else {
            match s.rfind('+') {
                Some(i) => (&s[..i], &s[i + 1..]),
                None => ("", s),
            }
        };
        let key = parse_key(key_part.trim())
            .ok_or_else(|| format!("{s:?}: unknown key {:?}", key_part.trim()))?;
        let mut mods = Mods::default();
        for m in mods_part
            .split('+')
            .map(str::trim)
            .filter(|m| !m.is_empty())
        {
            match m.to_ascii_lowercase().as_str() {
                "ctrl" | "cmd" | "command" => mods.cmd = true,
                "control" | "macctrl" => {
                    if mac {
                        mods.ctrl = true;
                    } else {
                        mods.cmd = true;
                    }
                }
                "alt" | "option" | "opt" => mods.alt = true,
                "shift" => mods.shift = true,
                _ => return Err(format!("{s:?}: unknown modifier {m:?}")),
            }
        }
        Ok(Chord { mods, key })
    }

    /// How this platform prints it: `Ctrl+Shift+Z` (`Cmd+Shift+Z` on a Mac).
    pub fn format_for(self, mac: bool) -> String {
        let mut s = String::new();
        if self.mods.cmd {
            s.push_str(if mac { "Cmd+" } else { "Ctrl+" });
        }
        if self.mods.ctrl {
            s.push_str(if mac { "Control+" } else { "Ctrl+" });
        }
        if self.mods.alt {
            s.push_str("Alt+");
        }
        if self.mods.shift {
            s.push_str("Shift+");
        }
        s.push_str(key_name(self.key));
        s
    }

    /// The chord a key press makes (the capture in the Preferences window).
    pub fn from_press(key: Key, m: Modifiers, mac: bool) -> Chord {
        Chord {
            mods: Mods {
                cmd: m.command || (!mac && m.ctrl),
                ctrl: mac && m.ctrl,
                alt: m.alt,
                shift: m.shift,
            },
            key,
        }
    }

    /// Whether a press with modifiers `m` sets this chord off: `Some(true)`
    /// for an exact match, `Some(false)` for one with extra Shift or Alt.
    fn matches(self, m: Modifiers) -> Option<bool> {
        let p = self.mods.egui();
        m.matches_logically(p).then(|| m.matches_exact(p))
    }
}

impl fmt::Display for Chord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.format_for(IS_MAC))
    }
}

/// When a command's keys are live. Every command but the global ones needs
/// the map to have the keyboard: no text field focused, the command
/// palette closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Context {
    /// Always, even while typing in a field (they all hold Ctrl).
    Global,
    /// The map has the keyboard.
    Map,
    /// The map, with a paste ghost or a tile stamp to turn.
    Turn,
    /// The map, with something selected.
    Selection,
    /// The map, with the Entity tool.
    EntityTool,
    /// The map, while a path, region or measurement is being drawn.
    Drawing,
}

impl Context {
    pub fn label(self) -> &'static str {
        match self {
            Context::Global => "everywhere",
            Context::Map => "on the map",
            Context::Turn => "with something to turn",
            Context::Selection => "with a selection",
            Context::EntityTool => "with the Entity tool",
            Context::Drawing => "while drawing",
        }
    }

    /// Who wins when two active bindings both match: the more specific.
    fn priority(self) -> u8 {
        match self {
            Context::Global => 0,
            Context::Map => 1,
            _ => 2,
        }
    }

    /// Whether a chord in both contexts is a conflict. A global key fires
    /// everywhere, so it clashes with anything. The narrower map contexts
    /// (Turn, Selection, ...) deliberately shadow plain map keys while they
    /// apply (R rotates a paste, otherwise it's the Rectangle mode), so
    /// they only clash with their own kind.
    pub fn overlaps(self, other: Context) -> bool {
        self == other || self == Context::Global || other == Context::Global
    }
}

/// Where a command sits in the Preferences window.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Group {
    File,
    Edit,
    Selection,
    View,
    Tools,
    Turn,
    Bookmarks,
}

impl Group {
    pub const ALL: [Group; 7] = [
        Group::File,
        Group::Edit,
        Group::Selection,
        Group::View,
        Group::Tools,
        Group::Turn,
        Group::Bookmarks,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Group::File => "File",
            Group::Edit => "Edit",
            Group::Selection => "Selection",
            Group::View => "View",
            Group::Tools => "Tools",
            Group::Turn => "Turn (a paste or a tile stamp)",
            Group::Bookmarks => "Bookmarks",
        }
    }
}

/// Every command a key can trigger.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Cmd {
    CommandPalette,
    Open,
    Reload,
    Save,
    Preferences,
    Undo,
    Redo,
    SelectAll,
    Duplicate,
    Delete,
    Group,
    Ungroup,
    SaveAssembly,
    /// Arrow `d`, by a pixel with `pixel`.
    Nudge {
        d: Dir,
        pixel: bool,
    },
    ToggleGrid,
    FitWorld,
    FitLevel,
    ZoomIn,
    ZoomOut,
    ZoomActual,
    NextLevel,
    PrevLevel,
    ShowFind,
    ShowLevels,
    /// The Paint button: the last Paint mode.
    PaintTool,
    Tool(Tool),
    PlaceEntity,
    FinishDrawing,
    DrawingBackspace,
    Turn(worlds_core::Flip),
    /// Bookmark `n` (0-based; the id and the name count from 1).
    JumpBookmark(u8),
}

/// An arrow direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

impl Dir {
    const ALL: [Dir; 4] = [Dir::Left, Dir::Right, Dir::Up, Dir::Down];

    fn delta(self) -> [i32; 2] {
        match self {
            Dir::Left => [-1, 0],
            Dir::Right => [1, 0],
            Dir::Up => [0, -1],
            Dir::Down => [0, 1],
        }
    }
}

/// Bookmarks with a key (Ctrl+Shift+1..9).
pub const BOOKMARK_KEYS: usize = 9;

static ALL: LazyLock<Vec<Cmd>> = LazyLock::new(|| {
    use Cmd::*;
    let mut v = vec![
        CommandPalette,
        Open,
        Reload,
        Save,
        Preferences,
        Undo,
        Redo,
        SelectAll,
        Duplicate,
        Delete,
        Group,
        Ungroup,
        SaveAssembly,
    ];
    for pixel in [false, true] {
        for d in Dir::ALL {
            v.push(Nudge { d, pixel });
        }
    }
    v.extend([
        ToggleGrid, FitWorld, FitLevel, ZoomIn, ZoomOut, ZoomActual, NextLevel, PrevLevel,
        ShowFind, ShowLevels, PaintTool,
    ]);
    v.extend(crate::tools::Tool::ALL.map(Cmd::Tool));
    v.extend([PlaceEntity, FinishDrawing, DrawingBackspace]);
    v.extend([
        Turn(worlds_core::Flip::X),
        Turn(worlds_core::Flip::Y),
        Turn(worlds_core::Flip::XT),
    ]);
    v.extend((0..BOOKMARK_KEYS as u8).map(JumpBookmark));
    v
});

impl Cmd {
    /// Every bindable command, in the Preferences window's order.
    pub fn all() -> &'static [Cmd] {
        &ALL
    }

    /// The stable id the preferences file uses.
    pub fn id(self) -> &'static str {
        use worlds_core::Flip;
        match self {
            Cmd::CommandPalette => "command_palette",
            Cmd::Open => "open",
            Cmd::Reload => "reload",
            Cmd::Save => "save",
            Cmd::Preferences => "preferences",
            Cmd::Undo => "undo",
            Cmd::Redo => "redo",
            Cmd::SelectAll => "select_all",
            Cmd::Duplicate => "duplicate",
            Cmd::Delete => "delete",
            Cmd::Group => "group",
            Cmd::Ungroup => "ungroup",
            Cmd::SaveAssembly => "save_assembly",
            Cmd::Nudge { d, pixel } => match (d, pixel) {
                (Dir::Left, false) => "nudge.left",
                (Dir::Right, false) => "nudge.right",
                (Dir::Up, false) => "nudge.up",
                (Dir::Down, false) => "nudge.down",
                (Dir::Left, true) => "nudge.left_pixel",
                (Dir::Right, true) => "nudge.right_pixel",
                (Dir::Up, true) => "nudge.up_pixel",
                (Dir::Down, true) => "nudge.down_pixel",
            },
            Cmd::ToggleGrid => "view.grid",
            Cmd::FitWorld => "view.fit_world",
            Cmd::FitLevel => "view.fit_level",
            Cmd::ZoomIn => "view.zoom_in",
            Cmd::ZoomOut => "view.zoom_out",
            Cmd::ZoomActual => "view.zoom_100",
            Cmd::NextLevel => "view.next_level",
            Cmd::PrevLevel => "view.prev_level",
            Cmd::ShowFind => "view.find",
            Cmd::ShowLevels => "view.levels",
            Cmd::PaintTool => "tool.paint",
            Cmd::Tool(t) => match t {
                Tool::Select => "tool.select",
                Tool::Brush => "tool.brush",
                Tool::Rect => "tool.rectangle",
                Tool::Line => "tool.line",
                Tool::Fill => "tool.fill",
                Tool::Eraser => "tool.eraser",
                Tool::Hand => "tool.hand",
                Tool::Entity => "tool.entity",
                Tool::Note => "tool.note",
                Tool::Measure => "tool.measure",
                Tool::Shape => "tool.shape",
            },
            Cmd::PlaceEntity => "place_entity",
            Cmd::FinishDrawing => "drawing.finish",
            Cmd::DrawingBackspace => "drawing.drop_point",
            Cmd::Turn(f) => match f {
                Flip::X => "turn.mirror_x",
                Flip::Y => "turn.mirror_y",
                _ => "turn.rotate",
            },
            Cmd::JumpBookmark(n) => {
                const IDS: [&str; BOOKMARK_KEYS] = [
                    "bookmark.jump_1",
                    "bookmark.jump_2",
                    "bookmark.jump_3",
                    "bookmark.jump_4",
                    "bookmark.jump_5",
                    "bookmark.jump_6",
                    "bookmark.jump_7",
                    "bookmark.jump_8",
                    "bookmark.jump_9",
                ];
                IDS.get(n as usize).copied().unwrap_or("bookmark.jump")
            }
        }
    }

    /// The command with this id.
    pub fn from_id(id: &str) -> Option<Cmd> {
        Cmd::all().iter().copied().find(|c| c.id() == id)
    }

    /// What the Preferences window calls it.
    pub fn name(self) -> String {
        use worlds_core::Flip;
        match self {
            Cmd::CommandPalette => "Command palette".into(),
            Cmd::Open => "Open project…".into(),
            Cmd::Reload => "Reload project from disk".into(),
            Cmd::Save => "Save".into(),
            Cmd::Preferences => "Preferences…".into(),
            Cmd::Undo => "Undo".into(),
            Cmd::Redo => "Redo".into(),
            Cmd::SelectAll => "Select all entities in the level".into(),
            Cmd::Duplicate => "Duplicate the selection".into(),
            Cmd::Delete => "Delete the selection (or the level)".into(),
            Cmd::Group => "Group the selection".into(),
            Cmd::Ungroup => "Ungroup the selection".into(),
            Cmd::SaveAssembly => "Save selection as assembly…".into(),
            Cmd::Nudge { d, pixel } => format!(
                "Nudge {} by a {}",
                match d {
                    Dir::Left => "left",
                    Dir::Right => "right",
                    Dir::Up => "up",
                    Dir::Down => "down",
                },
                if pixel { "pixel" } else { "cell" }
            ),
            Cmd::ToggleGrid => "Toggle grid".into(),
            Cmd::FitWorld => "Fit world".into(),
            Cmd::FitLevel => "Fit level".into(),
            Cmd::ZoomIn => "Zoom in".into(),
            Cmd::ZoomOut => "Zoom out".into(),
            Cmd::ZoomActual => "Zoom 100%".into(),
            Cmd::NextLevel => "Next level".into(),
            Cmd::PrevLevel => "Previous level".into(),
            Cmd::ShowFind => "Find: search the whole project".into(),
            Cmd::ShowLevels => "Levels: show the Levels pane".into(),
            Cmd::PaintTool => "Paint (last mode)".into(),
            Cmd::Tool(t) if t.is_paint_mode() => format!("Paint: {}", t.name()),
            Cmd::Tool(t) => t.name().into(),
            Cmd::PlaceEntity => "Place entity at the pointer".into(),
            Cmd::FinishDrawing => "Finish the path or polygon".into(),
            Cmd::DrawingBackspace => "Drop the last vertex".into(),
            Cmd::Turn(f) => match f {
                Flip::X => "Mirror left-right".into(),
                Flip::Y => "Mirror top-bottom".into(),
                _ => "Rotate 90°".into(),
            },
            Cmd::JumpBookmark(n) => format!("Jump to bookmark {}", n + 1),
        }
    }

    pub fn group(self) -> Group {
        match self {
            Cmd::CommandPalette | Cmd::Open | Cmd::Reload | Cmd::Save | Cmd::Preferences => {
                Group::File
            }
            Cmd::Undo | Cmd::Redo => Group::Edit,
            Cmd::SelectAll
            | Cmd::Duplicate
            | Cmd::Delete
            | Cmd::Group
            | Cmd::Ungroup
            | Cmd::SaveAssembly
            | Cmd::Nudge { .. } => Group::Selection,
            Cmd::ToggleGrid
            | Cmd::FitWorld
            | Cmd::FitLevel
            | Cmd::ZoomIn
            | Cmd::ZoomOut
            | Cmd::ZoomActual
            | Cmd::NextLevel
            | Cmd::PrevLevel
            | Cmd::ShowFind
            | Cmd::ShowLevels => Group::View,
            Cmd::PaintTool
            | Cmd::Tool(_)
            | Cmd::PlaceEntity
            | Cmd::FinishDrawing
            | Cmd::DrawingBackspace => Group::Tools,
            Cmd::Turn(_) => Group::Turn,
            Cmd::JumpBookmark(_) => Group::Bookmarks,
        }
    }

    /// When its keys are live (what the editor did before keys could be
    /// remapped: the Ctrl chords work while typing, the rest need the map).
    pub fn context(self) -> Context {
        match self {
            Cmd::CommandPalette
            | Cmd::Open
            | Cmd::Reload
            | Cmd::Save
            | Cmd::Preferences
            | Cmd::Undo
            | Cmd::Redo
            | Cmd::Group
            | Cmd::Ungroup
            | Cmd::ToggleGrid
            | Cmd::ShowFind
            | Cmd::ShowLevels
            | Cmd::JumpBookmark(_) => Context::Global,
            Cmd::Nudge { .. } => Context::Selection,
            Cmd::Turn(_) => Context::Turn,
            Cmd::PlaceEntity => Context::EntityTool,
            Cmd::FinishDrawing | Cmd::DrawingBackspace => Context::Drawing,
            _ => Context::Map,
        }
    }

    /// The default chords, the first one shown in tooltips.
    pub fn defaults(self) -> &'static [&'static str] {
        use worlds_core::Flip;
        match self {
            Cmd::CommandPalette => &["Ctrl+P"],
            Cmd::Open => &["Ctrl+O"],
            Cmd::Reload => &["Ctrl+R", "F5"],
            Cmd::Save => &["Ctrl+S"],
            Cmd::Preferences => &["Ctrl+,"],
            Cmd::Undo => &["Ctrl+Z"],
            Cmd::Redo => &["Ctrl+Shift+Z", "Ctrl+Y"],
            Cmd::SelectAll => &["Ctrl+A"],
            Cmd::Duplicate => &["Ctrl+D"],
            Cmd::Delete => &["Delete"],
            Cmd::Group => &["Ctrl+G"],
            Cmd::Ungroup => &["Ctrl+Shift+G"],
            Cmd::SaveAssembly => &["Ctrl+Shift+S"],
            Cmd::Nudge { d, pixel } => match (d, pixel) {
                (Dir::Left, false) => &["Left"],
                (Dir::Right, false) => &["Right"],
                (Dir::Up, false) => &["Up"],
                (Dir::Down, false) => &["Down"],
                (Dir::Left, true) => &["Shift+Left"],
                (Dir::Right, true) => &["Shift+Right"],
                (Dir::Up, true) => &["Shift+Up"],
                (Dir::Down, true) => &["Shift+Down"],
            },
            Cmd::ToggleGrid => &["Ctrl+M"],
            Cmd::FitWorld => &["Shift+F", "Home"],
            Cmd::FitLevel => &["F"],
            Cmd::ZoomIn => &["+", "="],
            Cmd::ZoomOut => &["-"],
            Cmd::ZoomActual => &["1"],
            Cmd::NextLevel => &["PgDn"],
            Cmd::PrevLevel => &["PgUp"],
            Cmd::ShowFind => &["Ctrl+F"],
            Cmd::ShowLevels => &["Ctrl+L"],
            Cmd::PaintTool => &["P"],
            Cmd::Tool(t) => match t {
                Tool::Select => &["V"],
                Tool::Brush => &["B"],
                Tool::Rect => &["R"],
                Tool::Line => &["L"],
                Tool::Fill => &["G"],
                Tool::Eraser => &["E"],
                Tool::Hand => &["H"],
                Tool::Entity => &["T"],
                Tool::Note => &["N"],
                Tool::Measure => &["M"],
                Tool::Shape => &[],
            },
            Cmd::PlaceEntity => &["Enter"],
            Cmd::FinishDrawing => &["Enter"],
            Cmd::DrawingBackspace => &["Backspace"],
            Cmd::Turn(f) => match f {
                Flip::X => &["X"],
                Flip::Y => &["Y"],
                _ => &["R"],
            },
            Cmd::JumpBookmark(n) => {
                const KEYS: [&str; BOOKMARK_KEYS] = [
                    "Ctrl+Shift+1",
                    "Ctrl+Shift+2",
                    "Ctrl+Shift+3",
                    "Ctrl+Shift+4",
                    "Ctrl+Shift+5",
                    "Ctrl+Shift+6",
                    "Ctrl+Shift+7",
                    "Ctrl+Shift+8",
                    "Ctrl+Shift+9",
                ];
                KEYS.get(n as usize).map_or(&[], std::slice::from_ref)
            }
        }
    }

    fn default_chords(self) -> Vec<Chord> {
        self.defaults()
            .iter()
            .map(|s| Chord::parse(s).expect("default shortcuts parse"))
            .collect()
    }

    /// The action this command runs. Delete depends on the selection, so
    /// it takes the view.
    pub fn action(self, view: &crate::view::ViewState) -> Action {
        match self {
            Cmd::CommandPalette => Action::CommandPalette,
            Cmd::Open => Action::OpenDialog,
            Cmd::Reload => Action::Reload,
            Cmd::Save => Action::Save,
            Cmd::Preferences => Action::OpenPreferences,
            Cmd::Undo => Action::Undo,
            Cmd::Redo => Action::Redo,
            Cmd::SelectAll => Action::SelectAll,
            Cmd::Duplicate => Action::DuplicateSelection,
            Cmd::Delete => crate::app::actions::delete_action(view),
            Cmd::Group => Action::GroupSelection,
            Cmd::Ungroup => Action::UngroupSelection,
            Cmd::SaveAssembly => Action::OpenSaveAssembly,
            Cmd::Nudge { d, pixel } => Action::Nudge {
                d: d.delta(),
                pixel,
            },
            Cmd::ToggleGrid => Action::ToggleGrid,
            Cmd::FitWorld => Action::FitWorld,
            Cmd::FitLevel => Action::FitLevel,
            Cmd::ZoomIn => Action::ZoomIn,
            Cmd::ZoomOut => Action::ZoomOut,
            Cmd::ZoomActual => Action::ZoomActual,
            Cmd::NextLevel => Action::NextLevel,
            Cmd::PrevLevel => Action::PrevLevel,
            Cmd::ShowFind => Action::ShowFind,
            Cmd::ShowLevels => Action::ShowLevels,
            Cmd::PaintTool => Action::PaintTool,
            Cmd::Tool(t) => Action::SetTool(t),
            Cmd::PlaceEntity => Action::PlaceEntity,
            Cmd::FinishDrawing => Action::FinishDrawing,
            Cmd::DrawingBackspace => Action::DrawingBackspace,
            Cmd::Turn(f) => Action::Turn(f),
            Cmd::JumpBookmark(n) => Action::JumpBookmark(n as usize),
        }
    }
}

/// Two commands sharing a chord where both can fire.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Conflict {
    pub a: Cmd,
    pub b: Cmd,
    pub chord: Chord,
}

/// The live bindings: the defaults with the user's changes on top.
#[derive(Clone, Debug, PartialEq)]
pub struct Keymap {
    binds: HashMap<Cmd, Vec<Chord>>,
}

impl Default for Keymap {
    fn default() -> Self {
        Keymap::defaults()
    }
}

impl Keymap {
    /// Every command on its default chords.
    pub fn defaults() -> Self {
        Keymap {
            binds: Cmd::all()
                .iter()
                .map(|&c| (c, c.default_chords()))
                .collect(),
        }
    }

    /// `c`'s chords (empty: unbound).
    pub fn chords(&self, c: Cmd) -> &[Chord] {
        self.binds.get(&c).map_or(&[], Vec::as_slice)
    }

    /// `c`'s first chord as text, `""` when unbound: what a tooltip shows.
    pub fn label(&self, c: Cmd) -> String {
        self.chords(c)
            .first()
            .map(Chord::to_string)
            .unwrap_or_default()
    }

    /// `text (chord)`, or just `text` when `c` is unbound.
    pub fn hint(&self, text: &str, c: Cmd) -> String {
        match self.label(c) {
            k if k.is_empty() => text.to_string(),
            k => format!("{text} ({k})"),
        }
    }

    /// Every chord of `c`, joined (`Ctrl+Shift+Z, Ctrl+Y`).
    pub fn labels(&self, c: Cmd) -> String {
        self.chords(c)
            .iter()
            .map(Chord::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Bind `c` to exactly these chords (empty: unbound).
    pub fn set(&mut self, c: Cmd, chords: Vec<Chord>) {
        let mut v: Vec<Chord> = Vec::new();
        for ch in chords {
            if !v.contains(&ch) {
                v.push(ch);
            }
        }
        self.binds.insert(c, v);
    }

    /// Add a chord to `c` (after the ones it has).
    pub fn add(&mut self, c: Cmd, chord: Chord) {
        let v = self.binds.entry(c).or_default();
        if !v.contains(&chord) {
            v.push(chord);
        }
    }

    /// Take one chord off `c`.
    pub fn remove(&mut self, c: Cmd, chord: Chord) {
        if let Some(v) = self.binds.get_mut(&c) {
            v.retain(|x| *x != chord);
        }
    }

    pub fn reset(&mut self, c: Cmd) {
        self.binds.insert(c, c.default_chords());
    }

    pub fn reset_all(&mut self) {
        *self = Keymap::defaults();
    }

    pub fn is_default(&self, c: Cmd) -> bool {
        self.chords(c) == c.default_chords().as_slice()
    }

    /// Every pair of commands that share a chord in overlapping contexts,
    /// each pair once.
    pub fn conflicts(&self) -> Vec<Conflict> {
        let mut out = Vec::new();
        let all = Cmd::all();
        for (i, &a) in all.iter().enumerate() {
            for &b in &all[i + 1..] {
                if !a.context().overlaps(b.context()) {
                    continue;
                }
                for &chord in self.chords(a) {
                    if self.chords(b).contains(&chord) {
                        out.push(Conflict { a, b, chord });
                    }
                }
            }
        }
        out
    }

    /// The commands `c` clashes with, and on which chord.
    pub fn conflicts_of(&self, c: Cmd) -> Vec<(Cmd, Chord)> {
        self.conflicts()
            .into_iter()
            .filter_map(|k| {
                if k.a == c {
                    Some((k.b, k.chord))
                } else if k.b == c {
                    Some((k.a, k.chord))
                } else {
                    None
                }
            })
            .collect()
    }

    /// The command a press of `key` with `m` held means, given which
    /// contexts are live; see the module docs for who wins a tie.
    pub fn lookup(&self, key: Key, m: Modifiers, active: impl Fn(Context) -> bool) -> Option<Cmd> {
        let mut best: Option<((bool, u8, u8), Cmd)> = None;
        for &c in Cmd::all() {
            let cx = c.context();
            for chord in self.chords(c) {
                if chord.key != key {
                    continue;
                }
                let Some(exact) = chord.matches(m) else {
                    continue;
                };
                if !active(cx) {
                    continue;
                }
                let score = (exact, cx.priority(), chord.mods.count());
                if best.is_none_or(|(s, _)| score > s) {
                    best = Some((score, c));
                }
            }
        }
        best.map(|(_, c)| c)
    }

    /// Take this frame's key presses that mean a command out of the input
    /// (so nothing else sees them) and return the commands, in order.
    pub fn take(&self, i: &mut egui::InputState, active: impl Fn(Context) -> bool) -> Vec<Cmd> {
        let mut out = Vec::new();
        i.events.retain(|e| {
            if let egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } = e
                && let Some(c) = self.lookup(*key, *modifiers, &active)
            {
                out.push(c);
                return false;
            }
            true
        });
        out
    }

    /// The bindings that differ from the defaults, by id, as text: what the
    /// preferences file stores (an empty list: unbound).
    pub fn overrides(&self) -> BTreeMap<String, Vec<String>> {
        Cmd::all()
            .iter()
            .filter(|&&c| !self.is_default(c))
            .map(|&c| {
                (
                    c.id().to_string(),
                    self.chords(c).iter().map(Chord::to_string).collect(),
                )
            })
            .collect()
    }

    /// The defaults with `over` on top. Returns what was wrong with it (an
    /// unknown id, a chord that doesn't parse): those entries are skipped,
    /// the rest apply.
    pub fn with_overrides(over: &BTreeMap<String, Vec<String>>) -> (Keymap, Vec<String>) {
        let mut km = Keymap::defaults();
        let mut problems = Vec::new();
        for (id, chords) in over {
            let Some(c) = Cmd::from_id(id) else {
                problems.push(format!("unknown shortcut command {id:?}"));
                continue;
            };
            let mut parsed = Vec::new();
            for s in chords {
                match Chord::parse(s) {
                    Ok(ch) => parsed.push(ch),
                    Err(e) => problems.push(format!("{id}: {e}")),
                }
            }
            km.set(c, parsed);
        }
        (km, problems)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(s: &str) -> Chord {
        Chord::parse_for(s, false).unwrap()
    }

    #[test]
    fn chords_round_trip() {
        for s in [
            "Ctrl+Shift+Z",
            "Ctrl+Z",
            "F",
            "Shift+F",
            "PgDn",
            "PgUp",
            "Del",
            "+",
            "Ctrl++",
            "-",
            "1",
            "Ctrl+Shift+1",
            "Ctrl+,",
            "Alt+Shift+Left",
            "F5",
            "Home",
            "Backspace",
            "Enter",
        ] {
            let c = ch(s);
            assert_eq!(c.format_for(false), s, "{s}");
            assert_eq!(
                Chord::parse_for(&c.format_for(true), true).unwrap(),
                c,
                "{s} on a Mac"
            );
        }
    }

    #[test]
    fn parsing_is_forgiving_and_says_what_is_wrong() {
        assert_eq!(ch("ctrl+shift+z"), ch("Ctrl+Shift+Z"));
        assert_eq!(ch("Cmd+Z"), ch("Ctrl+Z"));
        assert_eq!(ch("Command+Z"), ch("Ctrl+Z"));
        assert_eq!(ch("Delete"), ch("Del"));
        assert_eq!(ch("PageDown"), ch("PgDn"));
        assert_eq!(ch(" Shift + F "), ch("Shift+F"));
        assert_eq!(ch("Plus"), ch("+"));
        // A Mac prints the primary modifier as Cmd; its Control is apart.
        assert_eq!(ch("Ctrl+Z").format_for(true), "Cmd+Z");
        let mac_ctrl = Chord::parse_for("Control+Z", true).unwrap();
        assert!(mac_ctrl.mods.ctrl && !mac_ctrl.mods.cmd);
        assert_eq!(mac_ctrl.format_for(true), "Control+Z");
        // Elsewhere Control is just Ctrl.
        assert_eq!(ch("Control+Z"), ch("Ctrl+Z"));
        assert!(Chord::parse("").is_err());
        assert!(Chord::parse("Ctrl+").is_err());
        assert!(Chord::parse("Hyper+Z").unwrap_err().contains("Hyper"));
        assert!(Chord::parse("Ctrl+Banana").unwrap_err().contains("Banana"));
    }

    #[test]
    fn every_command_once_with_a_unique_id_and_parsing_defaults() {
        let all = Cmd::all();
        let mut ids: Vec<&str> = all.iter().map(|c| c.id()).collect();
        let n = ids.len();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), n, "ids are unique");
        let km = Keymap::defaults();
        for &c in all {
            assert_eq!(Cmd::from_id(c.id()), Some(c));
            assert_eq!(km.chords(c).len(), c.defaults().len(), "{}", c.id());
            assert!(!km.chords(c).is_empty(), "{} has a default key", c.id());
        }
        // Every tool in the toolbar has a command, and so does every
        // bookmark key.
        for t in Tool::ALL {
            assert!(all.contains(&Cmd::Tool(t)));
        }
        assert_eq!(Cmd::from_id("bookmark.jump_3"), Some(Cmd::JumpBookmark(2)));
        assert_eq!(Cmd::from_id("nope"), None);
    }

    #[test]
    fn defaults_have_no_conflicts() {
        assert_eq!(Keymap::defaults().conflicts(), vec![]);
    }

    #[test]
    fn conflicts_need_overlapping_contexts() {
        let mut km = Keymap::defaults();
        // Undo (everywhere) on F clashes with Fit level (on the map).
        km.set(Cmd::Undo, vec![ch("F")]);
        assert_eq!(
            km.conflicts(),
            vec![Conflict {
                a: Cmd::Undo,
                b: Cmd::FitLevel,
                chord: ch("F"),
            }]
        );
        assert_eq!(km.conflicts_of(Cmd::FitLevel), vec![(Cmd::Undo, ch("F"))]);
        // R both rotates (with something to turn) and picks the Rectangle
        // mode by default: a narrower context shadows, it doesn't clash.
        km.reset_all();
        assert!(
            km.chords(Cmd::Turn(worlds_core::Flip::XT))
                .contains(&ch("R"))
        );
        assert!(km.chords(Cmd::Tool(Tool::Rect)).contains(&ch("R")));
        assert!(km.conflicts().is_empty());
        // Two map keys on one chord do clash.
        km.set(Cmd::Tool(Tool::Hand), vec![ch("V")]);
        assert_eq!(km.conflicts_of(Cmd::Tool(Tool::Select)).len(), 1);
    }

    #[test]
    fn lookup_prefers_exact_then_specific() {
        let km = Keymap::defaults();
        let all = |_| true;
        let ctrl = Modifiers::COMMAND;
        let sh = Modifiers::SHIFT;
        assert_eq!(km.lookup(Key::Z, ctrl, all), Some(Cmd::Undo));
        assert_eq!(km.lookup(Key::Z, ctrl | sh, all), Some(Cmd::Redo));
        assert_eq!(km.lookup(Key::Y, ctrl, all), Some(Cmd::Redo));
        assert_eq!(km.lookup(Key::F, sh, all), Some(Cmd::FitWorld));
        assert_eq!(km.lookup(Key::F, Modifiers::NONE, all), Some(Cmd::FitLevel));
        // R rotates while there is something to turn, else is Rectangle.
        assert_eq!(
            km.lookup(Key::R, Modifiers::NONE, all),
            Some(Cmd::Turn(worlds_core::Flip::XT))
        );
        assert_eq!(
            km.lookup(Key::R, Modifiers::NONE, |c| c != Context::Turn),
            Some(Cmd::Tool(Tool::Rect))
        );
        // Shift+arrow is the pixel nudge, not the cell one.
        assert_eq!(
            km.lookup(Key::ArrowLeft, sh, all),
            Some(Cmd::Nudge {
                d: Dir::Left,
                pixel: true
            })
        );
        // Only the global keys work while a field has the keyboard; Ctrl+
        // Shift+S then falls back to Save, as a held Shift always did.
        let typing = |c| c == Context::Global;
        assert_eq!(km.lookup(Key::F, Modifiers::NONE, typing), None);
        assert_eq!(km.lookup(Key::S, ctrl | sh, typing), Some(Cmd::Save));
        assert_eq!(km.lookup(Key::S, ctrl | sh, all), Some(Cmd::SaveAssembly));
        // Extra Shift on a key that needs it still matches (+ on a US
        // keyboard), extra Ctrl doesn't.
        assert_eq!(km.lookup(Key::Plus, sh, all), Some(Cmd::ZoomIn));
        assert_eq!(km.lookup(Key::F, ctrl, all), Some(Cmd::ShowFind));
        assert_eq!(km.lookup(Key::Q, ctrl, all), None);
    }

    #[test]
    fn a_remap_moves_the_key() {
        let mut km = Keymap::defaults();
        km.set(Cmd::Undo, vec![ch("Ctrl+U")]);
        let all = |_| true;
        assert_eq!(km.lookup(Key::U, Modifiers::COMMAND, all), Some(Cmd::Undo));
        assert_eq!(km.lookup(Key::Z, Modifiers::COMMAND, all), None);
        assert_eq!(km.label(Cmd::Undo), ch("Ctrl+U").to_string());
        km.set(Cmd::Undo, vec![]);
        assert_eq!(km.label(Cmd::Undo), "");
        assert_eq!(km.hint("Undo", Cmd::Undo), "Undo");
        km.reset(Cmd::Undo);
        assert!(km.is_default(Cmd::Undo));
    }

    #[test]
    fn overrides_hold_only_what_changed() {
        let mut km = Keymap::defaults();
        assert!(km.overrides().is_empty());
        km.set(Cmd::Undo, vec![ch("Ctrl+U")]);
        km.set(Cmd::ToggleGrid, vec![]);
        km.add(Cmd::Save, ch("F2"));
        let over = km.overrides();
        assert_eq!(over.len(), 3, "{over:?}");
        assert_eq!(over["view.grid"], Vec::<String>::new());
        let (back, problems) = Keymap::with_overrides(&over);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(back, km);
        // Setting a command back to its defaults drops it from the file.
        km.set(Cmd::Undo, vec![ch("Ctrl+Z")]);
        assert!(!km.overrides().contains_key("undo"));
    }

    #[test]
    fn bad_overrides_are_skipped_not_fatal() {
        let over: BTreeMap<String, Vec<String>> = [
            ("undo".to_string(), vec!["Ctrl+U".to_string()]),
            ("no.such".to_string(), vec!["X".to_string()]),
            (
                "redo".to_string(),
                vec!["Ctrl+Nope".to_string(), "F9".to_string()],
            ),
        ]
        .into();
        let (km, problems) = Keymap::with_overrides(&over);
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert_eq!(km.chords(Cmd::Undo), &[Chord::parse("Ctrl+U").unwrap()]);
        assert_eq!(km.chords(Cmd::Redo), &[Chord::parse("F9").unwrap()]);
    }

    #[test]
    fn take_consumes_only_what_it_uses() {
        let km = Keymap::defaults();
        let mut i = egui::InputState::default();
        let key = |key, modifiers| egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        };
        i.events = vec![
            key(Key::Z, Modifiers::COMMAND),
            key(Key::Q, Modifiers::NONE),
            key(Key::F, Modifiers::NONE),
        ];
        let got = km.take(&mut i, |c| c == Context::Global);
        assert_eq!(got, vec![Cmd::Undo]);
        assert_eq!(i.events.len(), 2, "Q and F (map keys while typing) stay");
    }
}
