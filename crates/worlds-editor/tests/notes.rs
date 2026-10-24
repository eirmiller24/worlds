//! Annotations on the canvas, without a GPU: annotations and the mixed selection
//! (a click on a note, Ctrl toggles, locked ones stay put, the marquee in a
//! level and on the world, a mixed drag as one entry with each kind's exact
//! delta, levels snapping to guides within 8 screen px and not beyond, tile
//! stacks dragged, duplicated, deleted and nudged), the Note and Measure
//! tools per kind. Everything goes through the same
//! functions the mouse and `--script` use ([`worlds_editor::annot`],
//! [`worlds_editor::viewport::select_press`], the command layer). Runs on
//! a temp copy of `worlds-core`'s `mini.worlds`.
//!
//! mini.worlds, world px: crevasse at (0, 0), 20 x 5 cells of 16; shaft at
//! (320, 80), 6 x 8. World notes: a locked guide `guidestem2` (0,0)-(0,480)
//! -(320,480), a pin `pinstatue2` at (96, 64), a rect region `regioneast`
//! [0, 0, 640, 480]. Crevasse notes: `notecrev22` at (160, 48), a done path
//! `pathcrev22` (16, 64)-(300, 64). Tiles on `decor_back` (8 px cells) at
//! (5, 2), (4, 2), (4, 1).

use std::path::Path;

use worlds_core::{Command, Id, Shape, Slug};
use worlds_editor::annot;
use worlds_editor::doc::Document;
use worlds_editor::edit::Editing;
use worlds_editor::script::cell_hit;
use worlds_editor::tools::{CommandContext, EditorCommands, Grab, Invalidation, Tool, ToolEvent};
use worlds_editor::view::{NoteKind, SelItem, ViewState};
use worlds_editor::viewport::{SelectPress, level_under, select_press};

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

    fn batch(&mut self, label: String, commands: Vec<Command>) -> Result<(), String> {
        self.apply(Command::Batch { label, commands })
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

    fn level(&self, s: &str) -> usize {
        self.doc.level_ix(s).unwrap()
    }

    fn layer(&self, s: &str) -> usize {
        self.doc
            .project
            .layers
            .iter()
            .position(|l| l.slug == s)
            .unwrap()
    }

    fn note(&self, i: &str) -> worlds_core::Annotation {
        annot::find_note(&self.doc, &id(i)).unwrap().note.clone()
    }

    fn press(&mut self, wp: [f64; 2], ctrl: bool) -> Option<(usize, Grab)> {
        let press = SelectPress {
            active: self.view.active_level,
            on_tab: false,
            handle: None,
            level_under: level_under(&self.doc, wp),
            wp,
            add: ctrl,
            zoom: self.view.zoom,
        };
        select_press(&self.doc, &mut self.view, press)
    }

    /// A Select press at `wp` as the mouse makes it, a drag to `to`, the
    /// release.
    fn drag(&mut self, wp: [f64; 2], to: [f64; 2], ctrl: bool) -> Option<Grab> {
        let (ix, grab) = self.press(wp, ctrl)?;
        let layer = self.view.active_layer.unwrap_or(0);
        let m = egui::Modifiers {
            ctrl,
            command: ctrl,
            ..Default::default()
        };
        let mut start = if ix < self.doc.levels.len() {
            cell_hit(&self.doc, ix, layer, [0, 0])
        } else {
            worlds_editor::script::world_hit(&self.doc, ix, layer, wp)
        };
        start.world_px = wp;
        let mut at = start;
        at.world_px = to;
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

    fn tiles(&self, level: &str) -> Vec<([u32; 2], [u32; 2])> {
        let ix = self.level(level);
        let mut v: Vec<([u32; 2], [u32; 2])> = self
            .doc
            .level(ix)
            .tiles_layer("decor_back")
            .map(|t| t.tiles.iter().map(|p| (p.cell, p.tile)).collect())
            .unwrap_or_default();
        v.sort();
        v
    }

    fn stack(&self, level: &str, cell: [u32; 2]) -> SelItem {
        SelItem::Tiles {
            level: self.doc.ids[self.level(level)].clone(),
            layer: Slug::new("decor_back").unwrap(),
            cell,
        }
    }
}

// ---- the selection ----

#[test]
fn a_click_selects_a_note_and_ctrl_toggles() {
    let mut r = Rig::new();
    // On the pin's icon: it is selected, the drag would move the selection.
    assert_eq!(
        r.press([97.0, 63.0], false),
        Some((
            r.level("crevasse"),
            Grab::Selection {
                was_selected: false
            }
        ))
    );
    assert_eq!(r.view.selection.items, [SelItem::Note(id("pinstatue2"))]);
    // Ctrl on the note's card (at 4x, the card hangs right and down of its
    // point): added.
    r.drag([162.0, 50.0], [162.0, 50.0], true);
    assert_eq!(
        r.view.selection.items,
        [
            SelItem::Note(id("pinstatue2")),
            SelItem::Note(id("notecrev22"))
        ]
    );
    // A Ctrl click on a selected one takes it out; no entry for any of it.
    r.drag([96.0, 64.0], [96.0, 64.0], true);
    assert_eq!(r.view.selection.items, [SelItem::Note(id("notecrev22"))]);
    // A plain click on a note of a bigger selection narrows to it.
    r.view.selection.add(SelItem::Note(id("pinstatue2")));
    r.drag([96.0, 64.0], [96.0, 64.0], false);
    assert_eq!(r.view.selection.items, [SelItem::Note(id("pinstatue2"))]);
    // The done path is picked by its line (within 4 screen px = 1 px).
    r.drag([200.0, 64.5], [200.0, 64.5], false);
    assert_eq!(r.view.selection.items, [SelItem::Note(id("pathcrev22"))]);
    // Hidden by its tag: not hit.
    r.view.annotations.hidden_tags.insert("landmark".into());
    r.press([96.0, 64.0], false);
    assert!(!r.view.selection.contains(&SelItem::Note(id("pinstatue2"))));
    assert_eq!(r.entries(), 0);
}

#[test]
fn a_locked_guide_is_selected_by_a_click_but_never_moved() {
    let mut r = Rig::new();
    let before = r.note("guidestem2");
    // On the guide's stem, outside every level.
    let g = r.drag([0.5, 200.0], [64.5, 232.0], false);
    assert_eq!(
        g,
        Some(Grab::Selection {
            was_selected: false
        })
    );
    assert_eq!(r.view.selection.items, [SelItem::Note(id("guidestem2"))]);
    assert_eq!(r.note("guidestem2"), before);
    assert_eq!(r.entries(), 0);
    let s = r.view.status.as_ref().unwrap();
    assert!(s.text.contains("1 locked note left alone"), "{}", s.text);
    // Nudge, Delete: left alone too.
    let n = annot::plan_nudge(&r.doc, &r.view, [1, 0], false).unwrap();
    assert!(n.commands.is_empty() && n.left_alone.unwrap().contains("locked"));
    assert!(annot::plan_delete(&r.doc, &r.view).commands.is_empty());
    // Ctrl-D copies it unlocked, one cell right.
    let d = annot::plan_duplicate(&r.doc, &r.view);
    let Command::AddNote { note, .. } = &d.commands[0] else {
        panic!("{:?}", d.commands)
    };
    assert!(!note.locked);
    assert_eq!(note.shape.points()[0], [16, 0]);
}

#[test]
fn the_marquee_picks_entities_tiles_and_notes_in_the_level() {
    let mut r = Rig::new();
    r.view.active_layer = Some(r.layer("decor_back"));
    let g = r.drag([2.0, 2.0], [319.0, 79.0], false);
    assert_eq!(g, Some(Grab::Marquee));
    let sel = &r.view.selection;
    for item in [
        SelItem::Entity(id("doorcrev22")),
        SelItem::Entity(id("btncrev222")),
        SelItem::Note(id("notecrev22")),
        SelItem::Note(id("pathcrev22")),
        SelItem::Note(id("pinstatue2")),
        SelItem::Note(id("regioneast")),
    ] {
        assert!(sel.contains(&item), "{item:?} in {sel:?}");
    }
    for c in [[5, 2], [4, 2], [4, 1]] {
        assert!(sel.contains(&r.stack("crevasse", c)), "{c:?}");
    }
    // Never the locked guide.
    assert!(!sel.contains(&SelItem::Note(id("guidestem2"))));
    assert_eq!(sel.items.len(), 9);
    // A small marquee touching only the pin's point; Ctrl adds.
    r.view.selection.clear();
    // (The region's box and the path's line meet it too.)
    r.drag([90.0, 58.0], [100.0, 70.0], false);
    assert_eq!(
        r.view.selection.items,
        [
            SelItem::Note(id("pinstatue2")),
            SelItem::Note(id("regioneast")),
            SelItem::Note(id("pathcrev22"))
        ]
    );
    r.drag([2.0, 2.0], [60.0, 30.0], true);
    assert!(r.view.selection.contains(&SelItem::Note(id("pinstatue2"))));
    assert!(r.view.selection.contains(&r.stack("crevasse", [4, 1])));
    // An entities layer active: no tile stacks.
    r.view.active_layer = Some(r.layer("objects"));
    r.drag([2.0, 2.0], [319.0, 79.0], false);
    assert!(r.view.selection.tiles().next().is_none());
    assert_eq!(r.entries(), 0);
}

#[test]
fn the_world_marquee_picks_levels_and_world_notes() {
    let mut r = Rig::new();
    let g = r.drag([-50.0, -40.0], [700.0, 500.0], false);
    assert_eq!(g, Some(Grab::WorldMarquee));
    let crev = SelItem::Level(r.doc.ids[r.level("crevasse")].clone());
    let shaft = SelItem::Level(r.doc.ids[r.level("shaft")].clone());
    assert_eq!(
        r.view.selection.items,
        [
            crev.clone(),
            shaft.clone(),
            SelItem::Note(id("pinstatue2")),
            SelItem::Note(id("regioneast")),
        ]
    );
    // Only shaft; Ctrl adds the pin's area.
    r.drag([-50.0, -40.0], [-49.0, -39.0], false);
    assert!(
        r.view.selection.is_empty(),
        "a tiny marquee touches nothing"
    );
    r.drag([700.0, 300.0], [400.0, 200.0], false);
    assert_eq!(
        r.view.selection.items,
        [shaft.clone(), SelItem::Note(id("regioneast"))]
    );
    // A click (zero size) out there clears it.
    r.drag([700.0, 300.0], [700.0, 300.0], false);
    assert!(r.view.selection.is_empty());
    // The selection event reports levels by slug.
    r.view
        .selection
        .set([shaft, SelItem::Note(id("pinstatue2"))]);
    let sel = worlds_editor::live::selection_of(&r.doc, &r.view);
    assert_eq!(sel.2, ["pinstatue2"]);
    assert_eq!(sel.3, ["shaft"]);
    assert_eq!(r.entries(), 0);
}

#[test]
fn a_mixed_drag_is_one_entry_with_each_kinds_delta() {
    let mut r = Rig::new();
    r.view.active_layer = Some(r.layer("decor_back"));
    let btn_before = r.doc.find_entity(&id("btncrev222")).unwrap().2.clone();
    r.view.selection.set([
        SelItem::Note(id("pinstatue2")),
        SelItem::Entity(id("btncrev222")),
        SelItem::Note(id("notecrev22")),
        r.stack("crevasse", [4, 1]),
    ]);
    // Grab the pin and drag by (35, 17): notes by the project grid (16):
    // (32, 16); the button by whole 16 px cells: (2, 1); the tile stack by
    // whole 8 px cells: (4, 2).
    let g = r.drag([96.0, 64.0], [131.0, 81.0], false);
    assert_eq!(g, Some(Grab::Selection { was_selected: true }));
    assert_eq!(r.entries(), 1, "{:?}", r.view.status);
    assert_eq!(r.label(), "Move 4 items");
    assert_eq!(r.note("pinstatue2").shape.points(), [[128, 80]]);
    assert_eq!(r.note("notecrev22").shape.points(), [[192, 64]]);
    let btn = r.doc.find_entity(&id("btncrev222")).unwrap().2.clone();
    assert_eq!(btn.cell, [btn_before.cell[0] + 2, btn_before.cell[1] + 1]);
    let tiles = r.tiles("crevasse");
    assert!(
        tiles.iter().any(|(c, t)| *c == [8, 3] && *t == [0, 3]),
        "{tiles:?}"
    );
    assert!(!tiles.iter().any(|(c, _)| *c == [4, 1]), "{tiles:?}");
    // The moved stack stays selected where it went.
    assert!(r.view.selection.contains(&r.stack("crevasse", [8, 3])));
    // Undo puts all of it back.
    r.undo();
    assert_eq!(r.note("pinstatue2").shape.points(), [[96, 64]]);
    assert_eq!(
        r.doc.find_entity(&id("btncrev222")).unwrap().2.cell,
        btn_before.cell
    );
    assert!(r.tiles("crevasse").iter().any(|(c, _)| *c == [4, 1]));
    // Ctrl: notes by the pixel, the entity by the pixel (offset folded).
    r.view.selection.set([SelItem::Note(id("pinstatue2"))]);
    r.drag([96.0, 64.0], [101.0, 61.0], true);
    assert_eq!(r.note("pinstatue2").shape.points(), [[101, 61]]);
    assert_eq!(r.label(), "Move note pinstatue2");
    // Levels: a selected level and a world note move by one delta, the
    // level's own things ride along (no command of their own).
    r.view.selection.set([
        SelItem::Level(r.doc.ids[r.level("shaft")].clone()),
        SelItem::Note(id("pinstatue2")),
    ]);
    r.view.annotations.snap_to_guides = false;
    let before = r.entries();
    let g = r.drag([330.0, 90.0], [362.0, 58.0], false);
    assert_eq!(g, Some(Grab::Selection { was_selected: true }));
    assert_eq!(r.entries(), before + 1);
    assert_eq!(r.doc.level(r.level("shaft")).world_px, [352, 48]);
    assert_eq!(r.note("pinstatue2").shape.points(), [[133, 29]]);
    assert_eq!(r.label(), "Move 2 items");
    // Two levels alone: "Move 2 levels", both by the same delta.
    r.view.selection.set([
        SelItem::Level(r.doc.ids[r.level("shaft")].clone()),
        SelItem::Level(r.doc.ids[r.level("crevasse")].clone()),
    ]);
    r.drag([360.0, 60.0], [328.0, 60.0], false);
    assert_eq!(r.label(), "Move 2 levels");
    assert_eq!(r.doc.level(r.level("shaft")).world_px, [320, 48]);
    assert_eq!(r.doc.level(r.level("crevasse")).world_px, [-32, 0]);
}

#[test]
fn levels_snap_to_guides_within_8_screen_px_and_not_beyond() {
    let mut r = Rig::new();
    // A vertical guide at x = 500 (and the stem at x = 0).
    let mut g = worlds_core::Annotation::new(Shape::Guide {
        points: vec![[500, -100], [500, 300]],
        closed: false,
        group: None,
    });
    g.id = id("guidefivex");
    g.locked = true;
    r.apply(Command::AddNote {
        scope: worlds_core::command::NoteScope::World(Slug::new("main").unwrap()),
        note: g,
        index: None,
    })
    .unwrap();
    let shaft = r.level("shaft");
    let item = SelItem::Level(r.doc.ids[shaft].clone());
    // At 1x, 8 screen px are 8 world px. Shaft spans x 320..416: dragged by
    // 79 its right edge would be at 495, 5 px from the guide: it lands on
    // it exactly (x = 500 - 96 = 404), not on the grid (400).
    r.view.zoom = 1.0;
    r.view.selection.set([item.clone()]);
    r.drag([330.0, 90.0], [409.0, 90.0], false);
    assert_eq!(r.doc.level(shaft).world_px, [404, 80]);
    assert_eq!(r.label(), "Move level shaft");
    r.undo();
    // 14 px away: the grid.
    r.drag([330.0, 90.0], [400.0, 90.0], false);
    assert_eq!(r.doc.level(shaft).world_px, [384, 80]);
    r.undo();
    // At 4x, 8 screen px are 2 world px: 5 px away no longer snaps.
    r.view.zoom = 4.0;
    r.drag([330.0, 90.0], [409.0, 90.0], false);
    assert_eq!(r.doc.level(shaft).world_px, [400, 80]);
    r.undo();
    // Snapping off, or the guide hidden: the grid.
    r.view.zoom = 1.0;
    r.view.annotations.snap_to_guides = false;
    r.drag([330.0, 90.0], [409.0, 90.0], false);
    assert_eq!(r.doc.level(shaft).world_px, [400, 80]);
    r.undo();
    r.view.annotations.snap_to_guides = true;
    r.view.annotations.hidden_kinds.insert("guide".into());
    let (d, snaps) = annot::drag_delta(&r.doc, &r.view, &[shaft], [79.0, 0.0], false);
    assert_eq!((d, snaps.len()), ([80, 0], 0));
    r.view.annotations.hidden_kinds.clear();
    // A guide group's offset counts (as built): moved 20 px right, the
    // guide is at 520, which a drag by 99 reaches (right edge 515) and a
    // drag by 79 no longer does.
    let (d, snaps) = annot::drag_delta(&r.doc, &r.view, &[shaft], [79.0, 0.0], false);
    assert_eq!(d, [84, 0]);
    assert_eq!(snaps[0].at, 500);
    r.apply(Command::SetGuideGroup {
        world: Slug::new("main").unwrap(),
        group: worlds_core::GuideGroup {
            offset: [20, 0],
            ..worlds_core::GuideGroup::new(Slug::new("g").unwrap())
        },
        index: None,
    })
    .unwrap();
    let mut gn = r.note("guidefivex");
    if let Shape::Guide { group, .. } = &mut gn.shape {
        *group = Some(Slug::new("g").unwrap());
    }
    r.apply(Command::UpdateNote {
        id: id("guidefivex"),
        note: gn,
    })
    .unwrap();
    let (d, _) = annot::drag_delta(&r.doc, &r.view, &[shaft], [99.0, 0.0], false);
    assert_eq!(d, [104, 0]);
    let (d, _) = annot::drag_delta(&r.doc, &r.view, &[shaft], [79.0, 0.0], false);
    assert_eq!(d, [80, 0]);
    // The level's own drag (its body, unselected) snaps the same way.
    r.view.selection.clear();
    r.view.active_level = r.doc.level_ix("crevasse");
    let g = r.drag([330.0, 90.0], [429.0, 90.0], false);
    assert_eq!(g, Some(Grab::Level));
    assert_eq!(r.doc.level(shaft).world_px, [424, 80]);
}

#[test]
fn tile_stacks_drag_duplicate_delete_and_nudge_one_entry_each() {
    let mut r = Rig::new();
    let crev = r.level("crevasse");
    r.view.active_layer = Some(r.layer("decor_back"));
    let orig = r.tiles("crevasse");
    // A click on a stack selects it (then Ctrl adds another).
    r.drag([33.0, 17.0], [33.0, 17.0], false);
    assert_eq!(r.view.selection.items, [r.stack("crevasse", [4, 2])]);
    r.drag([41.0, 17.0], [41.0, 17.0], true);
    assert_eq!(r.view.selection.items.len(), 2);
    // Drag two cells right: one set_tiles, one entry.
    r.drag([33.0, 17.0], [50.0, 18.0], false);
    assert_eq!(r.entries(), 1);
    assert_eq!(r.label(), "Move 2 tile stacks");
    let t = r.tiles("crevasse");
    assert!(
        t.contains(&([6, 2], [5, 1])) && t.contains(&([7, 2], [6, 1])),
        "{t:?}"
    );
    assert!(
        !t.iter().any(|(c, _)| *c == [4, 2] || *c == [5, 2]),
        "{t:?}"
    );
    assert!(t.contains(&([4, 1], [0, 3])));
    r.undo();
    assert_eq!(r.tiles("crevasse"), orig);
    // A move off the level is refused, with no entry.
    r.view
        .selection
        .set([r.stack("crevasse", [4, 2]), r.stack("crevasse", [5, 2])]);
    let plan = annot::plan_move(&r.doc, &r.view, [-400.0, 0.0], false);
    assert!(plan.is_err());
    // Ctrl-D: copies one cell right, they become the selection.
    r.view.selection.set([r.stack("crevasse", [4, 1])]);
    let d = annot::plan_duplicate(&r.doc, &r.view);
    r.batch(d.label.clone(), d.commands).unwrap();
    assert_eq!(d.label, "Duplicate tile stack");
    r.view.selection.set(d.selection);
    let t = r.tiles("crevasse");
    assert!(
        t.contains(&([4, 1], [0, 3])) && t.contains(&([5, 1], [0, 3])),
        "{t:?}"
    );
    assert_eq!(r.view.selection.items, [r.stack("crevasse", [5, 1])]);
    // Delete: the copy goes; nudge: one cell down.
    let p = annot::plan_delete(&r.doc, &r.view);
    r.batch(p.label, p.commands).unwrap();
    assert_eq!(r.tiles("crevasse"), orig);
    assert!(r.view.selection.is_empty(), "pruned");
    r.view.selection.set([r.stack("crevasse", [4, 1])]);
    let n = annot::plan_nudge(&r.doc, &r.view, [0, 1], false).unwrap();
    assert_eq!(n.label, "Nudge tile stack");
    assert_eq!(n.selection, [r.stack("crevasse", [4, 2])]);
    r.batch(n.label, n.commands).unwrap();
    let t = r.tiles("crevasse");
    // Onto (4, 2): that stack is replaced by the moved one.
    assert!(
        t.contains(&([4, 2], [0, 3])) && !t.iter().any(|(c, _)| *c == [4, 1]),
        "{t:?}"
    );
    // Duplicate, delete, nudge: one entry each; undoing them restores it.
    assert_eq!(r.entries(), 3);
    for _ in 0..3 {
        r.undo();
    }
    assert_eq!(r.entries(), 0);
    assert_eq!(r.tiles("crevasse"), orig);
    let _ = crev;
}

#[test]
fn undo_prunes_the_selection_of_every_kind() {
    let mut r = Rig::new();
    r.view.tool = Tool::Note;
    r.view.note_tool.kind = NoteKind::Note;
    let wp = [101.0, 37.0];
    let under = level_under(&r.doc, wp);
    let cmd = annot::tool_click(&r.doc, &mut r.view, wp, under, false).unwrap();
    r.apply(cmd).unwrap();
    assert_eq!(r.view.selection.notes().count(), 1);
    r.view
        .selection
        .add(SelItem::Level(r.doc.ids[r.level("shaft")].clone()));
    r.undo();
    // The note is gone and left the selection; the level stays.
    assert_eq!(r.view.selection.notes().count(), 0);
    assert_eq!(r.view.selection.levels().count(), 1);
    // A deleted level leaves it too.
    let shaft = Slug::new("shaft").unwrap();
    r.apply(Command::DeleteLevel {
        world: Slug::new("main").unwrap(),
        level: shaft,
    })
    .unwrap();
    assert!(r.view.selection.is_empty());
}

// ---- the Note and Measure tools ----

#[test]
fn the_note_tool_adds_each_kind() {
    let mut r = Rig::new();
    r.view.tool = Tool::Note;
    let click = |r: &mut Rig, wp: [f64; 2]| {
        let under = level_under(&r.doc, wp);
        annot::tool_click(&r.doc, &mut r.view, wp, under, false)
    };
    // A note in crevasse: level scope, level px, half-grid snapped (8 px).
    r.view.note_tool.kind = NoteKind::Note;
    let c = click(&mut r, [101.0, 37.0]).unwrap();
    let Command::AddNote { scope, note, .. } = &c else {
        panic!("{c:?}")
    };
    assert_eq!(
        *scope,
        worlds_core::command::NoteScope::Level(Slug::new("crevasse").unwrap())
    );
    assert_eq!(note.shape, Shape::Note { px: [104, 40] });
    assert_eq!(note.title.as_deref(), Some("Note"));
    assert_eq!(r.view.focus_title.as_ref(), Some(&note.id));
    r.apply(c).unwrap();
    assert_eq!(r.entries(), 1);
    // A pin outside every level: world scope, the strip's icon.
    r.view.note_tool.kind = NoteKind::Pin;
    r.view.note_tool.icon = "statue".into();
    let c = click(&mut r, [-100.0, -51.0]).unwrap();
    let Command::AddNote { scope, note, .. } = &c else {
        panic!()
    };
    assert_eq!(
        *scope,
        worlds_core::command::NoteScope::World(Slug::new("main").unwrap())
    );
    assert_eq!(
        note.shape,
        Shape::Pin {
            px: [-104, -48],
            icon: Some("statue".into()),
            label: None
        }
    );
    // World forced, inside a level; Ctrl: the pixel.
    r.view.note_tool.world = true;
    let c = annot::tool_click(&r.doc, &mut r.view, [101.0, 37.0], Some(0), true).unwrap();
    assert!(
        matches!(&c, Command::AddNote { scope: worlds_core::command::NoteScope::World(_), note, .. }
        if note.shape.points() == [[101, 37]])
    );
    r.view.note_tool.world = false;
    // A path: clicks, Backspace drops the last, Enter finishes: one add.
    r.view.note_tool.kind = NoteKind::Path;
    for wp in [[16.0, 16.0], [64.0, 16.0], [64.0, 64.0], [200.0, 70.0]] {
        assert!(click(&mut r, wp).is_none());
    }
    assert!(annot::drawing_backspace(&mut r.view));
    let c = annot::finish_drawing(&r.doc, &mut r.view).unwrap();
    let Command::AddNote { note, .. } = &c else {
        panic!()
    };
    assert_eq!(
        note.shape,
        Shape::Path {
            points: vec![[16, 16], [64, 16], [64, 64]]
        }
    );
    assert!(r.view.drawing.is_none());
    let n = r.entries();
    r.apply(c).unwrap();
    assert_eq!(r.entries(), n + 1);
    assert_eq!(r.label(), "Add path");
    // One point isn't a path: refused, still drawing; Escape drops it.
    click(&mut r, [16.0, 16.0]);
    assert!(annot::finish_drawing(&r.doc, &mut r.view).is_none());
    assert!(r.view.status.as_ref().unwrap().error);
    assert!(r.view.drawing.is_some());
    assert!(annot::cancel_drawing(&mut r.view));
    assert!(r.view.drawing.is_none());
    // A double click's second click repeats the point: counted once.
    for wp in [[16.0, 16.0], [64.0, 16.0], [64.0, 16.0]] {
        click(&mut r, wp);
    }
    let c = annot::finish_drawing(&r.doc, &mut r.view).unwrap();
    assert!(matches!(&c, Command::AddNote { note, .. } if note.shape.points().len() == 2));
    // A region: a drag is a rect, clicks + Enter a polygon.
    r.view.note_tool.kind = NoteKind::Region;
    let c = annot::tool_drag(
        &r.doc,
        &mut r.view,
        [20.0, 10.0],
        [90.0, 50.0],
        Some(0),
        false,
    )
    .unwrap();
    let Command::AddNote { note, .. } = &c else {
        panic!()
    };
    assert_eq!(
        note.shape,
        Shape::Region {
            points: vec![],
            rect: Some([24, 8, 64, 40])
        }
    );
    for wp in [[16.0, 16.0], [64.0, 16.0]] {
        click(&mut r, wp);
    }
    assert!(
        annot::finish_drawing(&r.doc, &mut r.view).is_none(),
        "2 points"
    );
    click(&mut r, [64.0, 64.0]);
    let c = annot::finish_drawing(&r.doc, &mut r.view).unwrap();
    assert!(matches!(&c, Command::AddNote { note, .. }
        if note.shape == Shape::Region { points: vec![[16, 16], [64, 16], [64, 64]], rect: None }));
}

#[test]
fn the_measure_tool_reads_out_and_makes_no_entry() {
    let mut r = Rig::new();
    r.view.tool = Tool::Measure;
    for wp in [[0.0, 0.0], [48.0, 0.0], [48.0, 64.0]] {
        assert!(annot::tool_click(&r.doc, &mut r.view, wp, Some(0), false).is_none());
    }
    assert!(annot::finish_drawing(&r.doc, &mut r.view).is_none());
    let d = r.view.drawing.clone().unwrap();
    assert!(d.measure && d.done);
    assert_eq!(
        annot::readout(&r.doc, &d.points, None).unwrap(),
        "segment 64 px (4.0 tiles) · total 112 px (7.0 tiles) · dx 0 dy 64"
    );
    assert_eq!(r.entries(), 0);
    // It stays until Escape (or another tool).
    assert!(annot::cancel_drawing(&mut r.view));
}

#[test]
fn notes_drawing_shows_and_hides_by_tag_kind_and_status() {
    let r = Rig::new();
    let mut v = r.view.clone();
    let pin = r.note("pinstatue2");
    let path = r.note("pathcrev22");
    let region = r.note("regioneast");
    assert!(v.annotations.shows(&pin) && v.annotations.shows(&path));
    v.annotations.hidden_tags.insert("landmark".into());
    assert!(!v.annotations.shows(&pin));
    v.annotations
        .hidden_tags
        .insert(worlds_editor::view::UNTAGGED.into());
    assert!(!v.annotations.shows(&region), "untagged");
    v.annotations.hide_done = true;
    assert!(!v.annotations.shows(&path));
    // Snapshots: no filter, open only, every guide.
    assert!(annot::shown(&v, &pin, true));
    assert!(!annot::shown(&v, &path, true));
    assert!(annot::shown(&v, &r.note("guidestem2"), true));
}

// ---- nudging, duplicating and selecting across kinds ----

/// Shift+arrows nudge every movable kind: entities, notes and levels
/// by a pixel, tile stacks by a cell of their layer (a stack has nothing
/// finer). One entry; undo restores it exactly.
#[test]
fn shift_nudge_moves_every_kind() {
    let mut r = Rig::new();
    let shaft = r.level("shaft");
    let shaft_px = r.doc.level(shaft).world_px;
    let pin = r.note("pinstatue2");
    let orig = r.tiles("crevasse");
    r.view.selection.set([
        SelItem::Level(r.doc.ids[shaft].clone()),
        SelItem::Note(id("pinstatue2")),
        r.stack("crevasse", [4, 1]),
    ]);
    let n = annot::plan_nudge(&r.doc, &r.view, [1, 0], true).unwrap();
    assert_eq!(n.label, "Nudge 3 items");
    assert_eq!(n.left_alone, None);
    assert!(n.selection.contains(&r.stack("crevasse", [5, 1])));
    r.batch(n.label, n.commands).unwrap();
    r.view.selection.set(n.selection);
    assert_eq!(r.entries(), 1);
    assert_eq!(r.doc.level(shaft).world_px, [shaft_px[0] + 1, shaft_px[1]]);
    assert_eq!(r.note("pinstatue2").shape.points(), vec![[97, 64]]);
    let t = r.tiles("crevasse");
    assert!(t.contains(&([5, 1], [0, 3])) && !t.iter().any(|(c, _)| *c == [4, 1]));
    // Without Shift: a project cell for the level and the note.
    let n = annot::plan_nudge(&r.doc, &r.view, [0, 1], false).unwrap();
    r.batch(n.label, n.commands).unwrap();
    assert_eq!(
        r.doc.level(shaft).world_px,
        [shaft_px[0] + 1, shaft_px[1] + 16]
    );
    r.undo();
    r.undo();
    assert_eq!(r.doc.level(shaft).world_px, shaft_px);
    assert_eq!(r.note("pinstatue2"), pin);
    assert_eq!(r.tiles("crevasse"), orig);
}

/// C3: what is selected inside a selected level rides along when the level
/// moves (nudge) and when it is duplicated (Ctrl-D copies the level with
/// everything in it), so it isn't copied twice.
#[test]
fn items_in_a_selected_level_ride_along_or_are_copied() {
    let mut r = Rig::new();
    let crev = r.level("crevasse");
    r.view.selection.set([
        SelItem::Level(r.doc.ids[crev].clone()),
        SelItem::Note(id("notecrev22")),
        r.stack("crevasse", [4, 1]),
    ]);
    // Shift-nudge: the level moves a pixel; the note and the stack go with it.
    let n = annot::plan_nudge(&r.doc, &r.view, [1, 0], true).unwrap();
    assert_eq!(n.label, "Nudge level crevasse");
    assert!(matches!(n.commands.as_slice(), [Command::MoveLevel { .. }]));
    // A3: the stack inside the moved level keeps its (level-local) cell.
    assert!(n.selection.contains(&r.stack("crevasse", [4, 1])));
    // Ctrl-D: the level is duplicated, the note and the stack with it.
    let d = annot::plan_duplicate(&r.doc, &r.view);
    assert_eq!(d.label, "Duplicate level crevasse");
    assert_eq!(d.message, None);
    assert!(d.selection.is_empty());
    assert_eq!(d.levels, [worlds_core::slug("crevasse_copy")]);
    assert!(matches!(
        d.commands.as_slice(),
        [Command::DuplicateLevel { .. }]
    ));
    // A drag that rounds to no level movement moves the inside items
    // instead of dropping them: 6 px moves the 8 px stack a cell while the
    // level (16 px grid) stays.
    let p = annot::plan_move(&r.doc, &r.view, [6.0, 0.0], false).unwrap();
    assert_eq!(p.d, [0, 0]);
    assert_eq!(p.label, "Move tile stack");
}

/// A3: after a move or nudge, only stacks that actually moved follow in
/// the selection; a stack on a locked layer keeps its cell.
#[test]
fn the_selection_follows_only_the_stacks_that_moved() {
    let mut r = Rig::new();
    let decor = r.layer("decor_back");
    r.view
        .selection
        .set([r.stack("crevasse", [4, 1]), SelItem::Note(id("pinstatue2"))]);
    r.view.layers[decor].locked = true;
    let n = annot::plan_nudge(&r.doc, &r.view, [1, 0], false).unwrap();
    assert_eq!(n.label, "Nudge note pinstatue2");
    assert_eq!(
        n.selection,
        [r.stack("crevasse", [4, 1]), SelItem::Note(id("pinstatue2"))]
    );
    let p = annot::plan_move(&r.doc, &r.view, [16.0, 0.0], false).unwrap();
    assert_eq!(p.selection[0], r.stack("crevasse", [4, 1]));
}

/// C9: Ctrl-D of many notes gives every copy its own new id (the used ids
/// are gathered once, not per copy).
#[test]
fn duplicating_many_notes_gives_distinct_new_ids() {
    let mut r = Rig::new();
    let w = Slug::new("main").unwrap();
    let mut cmds = Vec::new();
    for i in 0..300 {
        let mut n = worlds_core::Annotation::new(Shape::Note { px: [i, 0] });
        n.id = annot::fresh_note_id(&r.doc);
        r.view.selection.add(SelItem::Note(n.id.clone()));
        cmds.push(Command::AddNote {
            scope: worlds_core::command::NoteScope::World(w.clone()),
            note: n,
            index: None,
        });
        if i % 50 == 49 {
            r.batch("add".into(), std::mem::take(&mut cmds)).unwrap();
        }
    }
    let d = annot::plan_duplicate(&r.doc, &r.view);
    let used = annot::used_note_ids(&r.doc);
    let new: std::collections::HashSet<Id> = d
        .commands
        .iter()
        .map(|c| match c {
            Command::AddNote { note, .. } => note.id.clone(),
            c => panic!("{c:?}"),
        })
        .collect();
    assert_eq!(new.len(), 300);
    assert!(new.iter().all(|i| !used.contains(i)));
}

/// In the active level, a press on no item is a marquee even when the
/// level is selected (as in the level itself); the level moves by its tab,
/// which drags the whole selection when the level is selected with others;
/// another selected level's body still drags the selection.
#[test]
fn a_marquee_starts_inside_the_selected_active_level() {
    let mut r = Rig::new();
    let crev = r.level("crevasse");
    let shaft = r.level("shaft");
    let crev_item = SelItem::Level(r.doc.ids[crev].clone());
    let shaft_item = SelItem::Level(r.doc.ids[shaft].clone());
    r.view.selection.set([crev_item.clone()]);
    assert_eq!(r.press([250.0, 8.0], false), Some((crev, Grab::Marquee)));
    // The tab: the level alone, or the selection it is part of.
    let tab = |r: &mut Rig| {
        let press = SelectPress {
            active: r.view.active_level,
            on_tab: true,
            handle: None,
            level_under: None,
            wp: [4.0, -10.0],
            add: false,
            zoom: r.view.zoom,
        };
        select_press(&r.doc, &mut r.view, press)
    };
    assert_eq!(tab(&mut r), Some((crev, Grab::Level)));
    r.view.selection.set([crev_item, shaft_item]);
    assert_eq!(
        tab(&mut r),
        Some((crev, Grab::Selection { was_selected: true }))
    );
    // Shaft is selected but not active: its body drags the selection.
    assert_eq!(
        r.press([400.0, 150.0], false),
        Some((shaft, Grab::Selection { was_selected: true }))
    );
    // A marquee drag in crevasse replaces the selection with what it touches.
    r.drag([250.0, 8.0], [319.0, 79.0], false);
    assert!(r.view.selection.levels().next().is_none());
    assert_eq!(r.entries(), 0);
}

/// A6: with no level left, world annotations are still selected, dragged
/// and marqueed on the canvas.
#[test]
fn world_notes_are_selectable_with_no_level() {
    let mut r = Rig::new();
    let w = Slug::new("main").unwrap();
    for l in ["crevasse", "shaft"] {
        r.apply(Command::DeleteLevel {
            world: w.clone(),
            level: Slug::new(l).unwrap(),
        })
        .unwrap();
    }
    assert!(r.doc.levels.is_empty());
    r.view.active_level = None;
    r.view.selection.clear();
    let before = r.entries();
    // A drag of the pin by two cells.
    let g = r.drag([97.0, 63.0], [129.0, 63.0], false);
    assert_eq!(
        g,
        Some(Grab::Selection {
            was_selected: false
        })
    );
    assert_eq!(r.note("pinstatue2").shape.points(), vec![[128, 64]]);
    assert_eq!(r.entries(), before + 1);
    // A world marquee picks the unlocked world notes it touches.
    assert_eq!(
        r.drag([-50.0, -50.0], [700.0, 500.0], false),
        Some(Grab::WorldMarquee)
    );
    let mut got: Vec<SelItem> = r.view.selection.items.clone();
    got.sort_by_key(|i| format!("{i:?}"));
    assert_eq!(
        got,
        [
            SelItem::Note(id("pinstatue2")),
            SelItem::Note(id("regioneast"))
        ]
    );
}
