//! Graphical geometry editing and connectors, without a GPU: the Select
//! tool's handles on one selected annotation (drag a vertex with grid,
//! pixel and guide snapping, Alt+click a segment to add one, Delete the
//! highlighted one, rect regions' edges and corners, locked annotations
//! refusing), and the Note tool's Connector kind (ends on rooms and
//! entities, waypoints, scope, a line that follows its rooms). Everything
//! goes through the functions the mouse and `--script` use
//! ([`worlds_editor::viewport::select_press_alt`], [`worlds_editor::annot`],
//! the command layer). Runs on a temp copy of `worlds-core`'s
//! `mini.worlds`.
//!
//! mini.worlds, world px: crevasse at (0, 0), 20 x 5 cells of 16 (320 x
//! 80); shaft at (320, 80), 6 x 8 (96 x 128), its door `doorshaft2` (1 x 3
//! cells, pivot bottom middle) at cell (2, 7), pivot (360, 208). World
//! notes: a locked guide `guidestem2` (0,0)-(0,480)-(320,480), a rect
//! region `regioneast` [0, 0, 640, 480]. Crevasse: a path `pathcrev22`
//! (16, 64)-(300, 64).

use std::path::Path;

use worlds_core::{Command, Endpoint, Id, Shape, Slug};
use worlds_editor::annot::{self, Handle};
use worlds_editor::doc::Document;
use worlds_editor::edit::Editing;
use worlds_editor::tools::{CommandContext, EditorCommands, Grab, Invalidation, Tool, ToolEvent};
use worlds_editor::view::{NoteKind, SelItem, ViewState};
use worlds_editor::viewport::{SelectPress, level_under, select_press_alt};

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let p = e.path();
        if p.is_dir() {
            copy_dir(&p, &to.join(e.file_name()));
        } else {
            std::fs::copy(&p, to.join(e.file_name())).unwrap();
        }
    }
}

fn id(s: &str) -> Id {
    Id::new(s).unwrap()
}

struct Rig {
    doc: Document,
    view: ViewState,
    ed: Editing,
    inv: Vec<Invalidation>,
    _tmp: tempfile::TempDir,
}

impl Rig {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let src =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../worlds-core/tests/fixtures/mini.worlds");
        let dir = tmp.path().join("mini.worlds");
        copy_dir(&src, &dir);
        std::fs::create_dir_all(dir.join("art")).unwrap();
        for (name, px) in [("castle_main", 256), ("ssw_set5", 2 + 8 * 33)] {
            image::RgbaImage::from_pixel(px, px, image::Rgba([90, 80, 70, 255]))
                .save(dir.join(format!("art/{name}.png")))
                .unwrap();
        }
        let mut doc = Document::open(&dir).unwrap();
        doc.bake_all_blocking();
        let mut view = ViewState::new();
        view.sync_layers(&doc, None);
        view.active_level = doc.level_ix("crevasse");
        view.active_layer = doc.project.layers.iter().position(|l| l.is_entities());
        view.zoom = 4.0;
        view.tool = Tool::Select;
        let mut r = Rig {
            doc,
            view,
            ed: Editing::new(),
            inv: Vec::new(),
            _tmp: tmp,
        };
        r.events(&[]);
        r
    }

    fn cx(&mut self) -> CommandContext<'_> {
        CommandContext {
            doc: &mut self.doc,
            view: &mut self.view,
            invalidate: &mut self.inv,
        }
    }

    fn events(&mut self, events: &[ToolEvent]) {
        let mut ed = std::mem::take(&mut self.ed);
        ed.tool_events(&mut self.cx(), events);
        self.ed = ed;
    }

    fn apply(&mut self, cmd: Command) -> Result<(), String> {
        let mut ed = std::mem::take(&mut self.ed);
        let r = ed.apply(&mut self.cx(), cmd);
        self.ed = ed;
        r
    }

    fn undo(&mut self) {
        let mut ed = std::mem::take(&mut self.ed);
        ed.undo(&mut self.cx());
        self.ed = ed;
    }

    fn entries(&self) -> usize {
        self.ed.history().undo_entries().len()
    }

    fn label(&self) -> String {
        self.ed
            .history()
            .undo_label()
            .unwrap_or_default()
            .to_string()
    }

    fn note(&self, i: &str) -> worlds_core::Annotation {
        annot::find_note(&self.doc, &id(i)).unwrap().note.clone()
    }

    fn select(&mut self, i: &str) {
        self.view.selection.set([SelItem::Note(id(i))]);
    }

    fn press(&mut self, wp: [f64; 2], alt: bool) -> Option<(usize, Grab)> {
        let press = SelectPress {
            active: self.view.active_level,
            on_tab: false,
            handle: None,
            level_under: level_under(&self.doc, wp),
            wp,
            add: false,
            zoom: self.view.zoom,
        };
        select_press_alt(&self.doc, &mut self.view, press, alt)
    }

    /// A Select press at `wp` (Alt with `alt`), a drag to `to` (Ctrl with
    /// `ctrl`), the release, as the mouse sends them.
    fn drag(&mut self, wp: [f64; 2], to: [f64; 2], ctrl: bool, alt: bool) -> Option<Grab> {
        let (ix, grab) = self.press(wp, alt)?;
        let layer = self.view.active_layer.unwrap_or(0);
        let m = egui::Modifiers {
            ctrl,
            command: ctrl,
            alt,
            ..Default::default()
        };
        let start = worlds_editor::script::world_hit(&self.doc, ix, layer, wp);
        let at = worlds_editor::script::world_hit(&self.doc, ix, layer, to);
        self.events(&[
            ToolEvent::Begin {
                tool: Tool::Select,
                at: start,
                modifiers: m,
                grab,
            },
            ToolEvent::Move {
                tool: Tool::Select,
                start,
                at,
                modifiers: m,
            },
            ToolEvent::End {
                tool: Tool::Select,
                start,
                at,
                modifiers: m,
            },
        ]);
        Some(grab)
    }

    /// Delete with the highlighted point, as the Delete key: the one
    /// `update_note` [`annot::plan_vertex_delete`] gives, applied.
    fn delete_vertex(&mut self) -> Result<(), String> {
        let (cmd, label) =
            annot::plan_vertex_delete(&self.doc, &self.view).expect("a point is highlighted")?;
        self.apply(Command::Batch {
            label,
            commands: vec![cmd],
        })
    }

    fn points(&self, i: &str) -> Vec<[i64; 2]> {
        self.note(i).shape.points()
    }
}

/// A vertex is dragged on half the project grid (8 px), or with Ctrl on
/// the pixel; each drag is one `update_note`, undone in one step.
#[test]
fn a_dragged_vertex_snaps_to_half_the_grid_or_the_pixel() {
    let mut r = Rig::new();
    r.select("pathcrev22");
    let path = annot::find_note(&r.doc, &id("pathcrev22")).unwrap();
    assert_eq!(
        annot::handles(&r.doc, path),
        [
            (Handle::Vertex(0), [16.0, 64.0]),
            (Handle::Vertex(1), [300.0, 64.0])
        ]
    );
    let before = r.entries();
    let grab = r.drag([300.5, 64.5], [310.5, 90.5], false, false);
    assert_eq!(grab, Some(Grab::Vertex(Handle::Vertex(1))));
    assert_eq!(r.points("pathcrev22"), [[16, 64], [312, 88]]);
    assert_eq!(r.entries(), before + 1);
    assert_eq!(r.label(), "Move point 1 of pathcrev22");
    assert_eq!(r.view.vertex, Some((id("pathcrev22"), Handle::Vertex(1))));
    // Still selected alone: the handles stay.
    assert_eq!(r.view.selection.items, [SelItem::Note(id("pathcrev22"))]);
    r.drag([16.0, 64.0], [21.0, 67.0], true, false);
    assert_eq!(r.points("pathcrev22"), [[21, 67], [312, 88]]);
    r.undo();
    r.undo();
    assert_eq!(r.points("pathcrev22"), [[16, 64], [300, 64]]);
    // A click on a handle only highlights it.
    let n = r.entries();
    r.drag([16.0, 64.0], [16.0, 64.0], false, false);
    assert_eq!(r.entries(), n);
    assert_eq!(r.view.vertex, Some((id("pathcrev22"), Handle::Vertex(0))));
    // A press elsewhere takes the highlight away.
    r.press([150.0, 30.0], false);
    assert_eq!(r.view.vertex, None);
}

/// Alt+click on a segment adds a point there (one entry, highlighted);
/// Delete then removes it; a path keeps 2 points.
#[test]
fn alt_click_adds_a_point_and_delete_removes_the_highlighted_one() {
    let mut r = Rig::new();
    r.select("pathcrev22");
    let n = r.entries();
    let grab = r.drag([150.0, 64.0], [150.0, 64.0], false, true);
    assert_eq!(grab, Some(Grab::InsertVertex));
    assert_eq!(r.points("pathcrev22"), [[16, 64], [152, 64], [300, 64]]);
    assert_eq!(r.entries(), n + 1);
    assert_eq!(r.label(), "Add a point to pathcrev22");
    assert_eq!(r.view.vertex, Some((id("pathcrev22"), Handle::Vertex(1))));
    r.delete_vertex().unwrap();
    assert_eq!(r.points("pathcrev22"), [[16, 64], [300, 64]]);
    assert_eq!(r.label(), "Remove point 1 of pathcrev22");
    r.drag([300.0, 64.0], [300.0, 64.0], false, false);
    assert_eq!(
        r.delete_vertex().unwrap_err(),
        "A path keeps at least 2 points"
    );
    // Alt off a segment is an ordinary press.
    r.select("pathcrev22");
    assert_ne!(
        r.press([150.0, 20.0], true).map(|g| g.1),
        Some(Grab::InsertVertex)
    );
    // Nothing highlighted: Delete deletes the selection as before.
    r.select("pathcrev22");
    r.view.vertex = None;
    assert!(annot::plan_vertex_delete(&r.doc, &r.view).is_none());
}

/// A rect region's corners and edges move its edges; it is kept the right
/// way round, and a rect with no width is refused.
#[test]
fn rect_regions_have_edge_and_corner_handles() {
    let mut r = Rig::new();
    r.view.zoom = 1.0;
    r.select("regioneast");
    let region = annot::find_note(&r.doc, &id("regioneast")).unwrap();
    let hs = annot::handles(&r.doc, region);
    assert_eq!(hs.len(), 8);
    assert!(hs.contains(&(Handle::Rect([1, 1]), [640.0, 480.0])));
    assert!(hs.contains(&(Handle::Rect([0, -1]), [320.0, 0.0])));
    r.drag([640.0, 480.0], [600.0, 400.0], false, false);
    let rect = |r: &Rig| match r.note("regioneast").shape {
        Shape::Region { rect, .. } => rect.unwrap(),
        s => panic!("{s:?}"),
    };
    assert_eq!(rect(&r), [0, 0, 600, 400]);
    assert_eq!(r.label(), "Resize region regioneast");
    // The left edge dragged past the right one: flipped, not negative.
    r.drag([0.0, 200.0], [700.0, 200.0], false, false);
    assert_eq!(rect(&r), [600, 0, 104, 400]);
    // No width: refused, nothing changes.
    let n = r.entries();
    r.drag([600.0, 200.0], [704.0, 200.0], false, false);
    assert_eq!(rect(&r), [600, 0, 104, 400]);
    assert_eq!(r.entries(), n);
    assert!(
        r.view
            .status
            .as_ref()
            .unwrap()
            .text
            .contains("width and a height")
    );
    // A rect has no points to add or remove.
    let e = annot::plan_vertex_insert(&r.doc, &r.view, [650.0, 0.0], 1.0, false)
        .unwrap()
        .unwrap_err();
    assert!(e.contains("rect region"), "{e}");
}

/// A locked annotation shows its handles, but a drag, an Alt+click and
/// Delete are refused with a status message.
#[test]
fn locked_annotations_show_handles_but_refuse_edits() {
    let mut r = Rig::new();
    r.view.zoom = 1.0;
    r.select("guidestem2");
    let guide = annot::find_note(&r.doc, &id("guidestem2")).unwrap();
    assert_eq!(annot::handles(&r.doc, guide).len(), 3);
    let before = r.note("guidestem2");
    let n = r.entries();
    assert_eq!(
        r.drag([0.0, 480.0], [32.0, 480.0], false, false),
        Some(Grab::Vertex(Handle::Vertex(1)))
    );
    assert_eq!(r.note("guidestem2"), before);
    assert_eq!(r.entries(), n);
    let msg = r.view.status.as_ref().unwrap().text.clone();
    assert!(msg.contains("guidestem2 is locked"), "{msg}");
    r.view.status = None;
    r.drag([0.0, 240.0], [0.0, 240.0], false, true);
    assert_eq!(r.note("guidestem2"), before);
    assert!(r.view.status.as_ref().unwrap().text.contains("locked"));
    r.drag([0.0, 480.0], [0.0, 480.0], false, false);
    assert!(r.delete_vertex().unwrap_err().contains("locked"));
    assert_eq!(r.entries(), n);
}

/// A dragged vertex snaps to a guide within 8 screen px (as built), over
/// the grid, when "Snap to guides" is on.
#[test]
fn a_dragged_vertex_snaps_to_guides() {
    let mut r = Rig::new();
    r.view.zoom = 1.0;
    r.select("pathcrev22");
    // x 5 is 5 px from the guide's vertical line at x 0: snapped there
    // (the grid alone gives 8).
    r.drag([16.0, 64.0], [5.0, 30.0], false, false);
    assert_eq!(r.points("pathcrev22")[0], [0, 32]);
    r.undo();
    r.view.annotations.snap_to_guides = false;
    r.drag([16.0, 64.0], [5.0, 30.0], false, false);
    assert_eq!(r.points("pathcrev22")[0], [8, 32]);
}

/// The Note tool's Connector: a click on a room starts it, a click
/// outside every level adds a waypoint, a click on an entity sets its end,
/// Enter adds it (world scope, one entry). Its line follows the rooms.
#[test]
fn the_connector_tool_joins_a_room_to_an_entity() {
    let mut r = Rig::new();
    r.view.tool = Tool::Note;
    r.view.note_tool.kind = NoteKind::Connector;
    let under = |r: &Rig, wp: [f64; 2]| level_under(&r.doc, wp);
    // Nothing under the first click: refused.
    let wp = [-100.0, -100.0];
    let u = under(&r, wp);
    assert!(annot::tool_click(&r.doc, &mut r.view, wp, u, false).is_none());
    assert!(r.view.drawing.is_none());
    for wp in [[20.0, 20.0], [361.0, 41.0], [360.0, 190.0]] {
        let u = under(&r, wp);
        assert!(annot::tool_click(&r.doc, &mut r.view, wp, u, false).is_none());
    }
    let d = r.view.drawing.clone().unwrap();
    let c = d.connector.as_ref().unwrap();
    assert_eq!(c.from, Endpoint::Level(Slug::new("crevasse").unwrap()));
    assert_eq!(c.to, Some(Endpoint::Entity(id("doorshaft2"))));
    assert_eq!(d.points, [[360, 40]]);
    // The preview is the line the release makes.
    assert_eq!(
        annot::draft_line(&r.doc, &d, None),
        [[320, 40], [360, 40], [360, 208]]
    );
    let n = r.entries();
    let cmd = annot::finish_drawing(&r.doc, &mut r.view).unwrap();
    let Command::AddNote { scope, note, .. } = &cmd else {
        panic!("{cmd:?}")
    };
    assert_eq!(
        *scope,
        worlds_core::command::NoteScope::World(Slug::new("main").unwrap())
    );
    let cid = note.id.clone();
    r.apply(cmd.clone()).unwrap();
    assert_eq!(r.entries(), n + 1);
    assert_eq!(r.view.selection.items, [SelItem::Note(cid.clone())]);
    let line = |r: &Rig| annot::world_points(&r.doc, annot::find_note(&r.doc, &cid).unwrap());
    assert_eq!(line(&r), [[320, 40], [360, 40], [360, 208]]);
    // Moving the shaft moves the line's end; undo moves it back.
    r.apply(Command::MoveLevel {
        world: Slug::new("main").unwrap(),
        level: Slug::new("shaft").unwrap(),
        world_px: [400, 80],
    })
    .unwrap();
    assert_eq!(line(&r), [[320, 40], [360, 40], [440, 208]]);
    r.undo();
    assert_eq!(line(&r), [[320, 40], [360, 40], [360, 208]]);
    // It is hit along its line, and its waypoint is a handle.
    r.view.zoom = 1.0;
    assert_eq!(
        annot::note_at(&r.doc, &r.view, [340.0, 41.0], 1.0),
        Some(cid.clone())
    );
    r.view.tool = Tool::Select;
    r.drag([360.0, 40.0], [376.0, 24.0], false, false);
    assert_eq!(line(&r), [[320, 24], [376, 24], [360, 208]]);
    // Alt+click on its first segment adds a waypoint before it.
    r.drag([330.0, 24.0], [330.0, 24.0], false, true);
    assert_eq!(r.note(cid.as_str()).shape.points(), [[328, 24], [376, 24]]);
    // Waypoints can all go.
    r.drag([328.0, 24.0], [328.0, 24.0], false, false);
    r.delete_vertex().unwrap();
    r.drag([376.0, 24.0], [376.0, 24.0], false, false);
    r.delete_vertex().unwrap();
    assert!(r.note(cid.as_str()).shape.points().is_empty());
    assert_eq!(line(&r), [[320, 80], [360, 208]]);
}

/// Shift adds a waypoint even on a room; Enter without an end keeps
/// drawing; Backspace takes the last waypoint, then the end, then the
/// drawing; both ends in one level make a level-scope connector.
#[test]
fn connector_drawing_details() {
    let mut r = Rig::new();
    r.view.tool = Tool::Note;
    r.view.note_tool.kind = NoteKind::Connector;
    let click = |r: &mut Rig, wp: [f64; 2], shift: bool| {
        let u = level_under(&r.doc, wp);
        annot::tool_click_shift(&r.doc, &mut r.view, wp, u, false, shift)
    };
    click(&mut r, [360.0, 190.0], false);
    // In the door's own level, not on it: a waypoint.
    click(&mut r, [336.0, 98.0], false);
    assert!(annot::finish_drawing(&r.doc, &mut r.view).is_none());
    assert!(r.view.status.as_ref().unwrap().error);
    assert!(r.view.drawing.is_some());
    // On crevasse with Shift: a waypoint too.
    click(&mut r, [100.0, 40.0], true);
    let d = r.view.drawing.clone().unwrap();
    assert_eq!(d.points, [[336, 96], [104, 40]]);
    assert_eq!(d.connector.as_ref().unwrap().to, None);
    // Without Shift: its end.
    click(&mut r, [100.0, 40.0], false);
    assert!(annot::drawing_backspace(&mut r.view));
    assert_eq!(r.view.drawing.as_ref().unwrap().points, [[336, 96]]);
    assert!(annot::drawing_backspace(&mut r.view));
    assert_eq!(
        r.view
            .drawing
            .as_ref()
            .unwrap()
            .connector
            .as_ref()
            .unwrap()
            .to,
        Some(Endpoint::Level(Slug::new("crevasse").unwrap()))
    );
    assert!(annot::drawing_backspace(&mut r.view));
    assert!(r.view.drawing.is_some());
    assert!(annot::drawing_backspace(&mut r.view));
    assert!(r.view.drawing.is_none());
    // The door to its own room: level scope, waypoints in level pixels.
    click(&mut r, [360.0, 190.0], false);
    click(&mut r, [336.0, 64.0], true);
    r.view
        .drawing
        .as_mut()
        .unwrap()
        .connector
        .as_mut()
        .unwrap()
        .to = Some(Endpoint::Level(Slug::new("shaft").unwrap()));
    let cmd = annot::finish_drawing(&r.doc, &mut r.view).unwrap();
    let Command::AddNote { scope, note, .. } = &cmd else {
        panic!("{cmd:?}")
    };
    assert_eq!(
        *scope,
        worlds_core::command::NoteScope::Level(Slug::new("shaft").unwrap())
    );
    assert_eq!(note.shape.points(), [[16, -16]]);
}
