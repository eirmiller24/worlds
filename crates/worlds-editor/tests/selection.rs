//! The selection and the Select tool on entities (no GPU): the press and
//! what it grabs (entity, marquee, the level's name tab), dragging the
//! selection, nudges that fold the offset into the cell, Ctrl-D, Delete,
//! pruning after undo, canvas picks, the field definition commands keeping
//! every bake, and live problems. Runs on a temp copy of `worlds-core`'s
//! `mini.worlds` fixture.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::json;
use worlds_core::{Command, EntityInstance, FieldDef, FieldType, Id, Slug};
use worlds_editor::app::problems::{LiveProblems, Poll};
use worlds_editor::doc::Document;
use worlds_editor::edit::{
    Editing, delete_commands, duplicate_commands, entity_move_commands, nudge_commands,
    unlocked_entities,
};
use worlds_editor::script::cell_hit;
use worlds_editor::tools::{CommandContext, EditorCommands, Grab, Invalidation, Tool, ToolEvent};
use worlds_editor::view::{PickAccept, PickedThing, SelItem, ViewState};
use worlds_editor::viewport::{SelectPress, pick_at, select_press};

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

fn slug(s: &str) -> Slug {
    Slug::new(s).unwrap()
}

fn id(s: &str) -> Id {
    Id::new(s).unwrap()
}

struct Rig {
    doc: Document,
    view: ViewState,
    ed: Editing,
    inv: Vec<Invalidation>,
    _dir: PathBuf,
    _tmp: tempfile::TempDir,
}

impl Rig {
    fn new() -> Self {
        Self::new_with(|_| {})
    }

    fn new_with(edit: impl FnOnce(&Path)) -> Self {
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
        edit(&dir);
        let mut doc = Document::open(&dir).unwrap();
        doc.bake_all_blocking();
        let mut view = ViewState::new();
        view.sync_layers(&doc, None);
        view.active_level = doc.level_ix("crevasse");
        view.active_layer = doc.project.layers.iter().position(|l| l.is_entities());
        // Screen px per world px for annotation hit sizes (a 16 px cell is
        // 64 screen px).
        view.zoom = 4.0;
        let mut r = Rig {
            doc,
            view,
            ed: Editing::new(),
            inv: Vec::new(),
            _dir: dir,
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

    fn batch(&mut self, label: &str, commands: Vec<Command>) -> Result<(), String> {
        self.apply(Command::Batch {
            label: label.into(),
            commands,
        })
    }

    fn undo(&mut self) {
        let mut ed = std::mem::take(&mut self.ed);
        ed.undo(&mut self.cx());
        self.ed = ed;
    }

    fn redo(&mut self) {
        let mut ed = std::mem::take(&mut self.ed);
        ed.redo(&mut self.cx());
        self.ed = ed;
    }

    fn entity(&self, i: &str) -> EntityInstance {
        self.doc.find_entity(&id(i)).unwrap().2.clone()
    }

    fn entries(&self) -> usize {
        self.ed.history().undo_entries().len()
    }

    fn level(&self, s: &str) -> usize {
        self.doc.level_ix(s).unwrap()
    }

    fn objects(&self) -> usize {
        self.doc
            .project
            .layers
            .iter()
            .position(|l| l.slug == "objects")
            .unwrap()
    }

    /// The world pixel at the middle of entity `i`'s box.
    fn center_of(&self, i: &str) -> [f64; 2] {
        let (ix, layer, e) = self.doc.find_entity(&id(i)).unwrap();
        let def = self.doc.project.entity_def(e.ty.as_str()).unwrap();
        let g = self
            .doc
            .project
            .layer_grid(self.doc.project.layer(layer.as_str()).unwrap());
        let [x, y, w, h] = def.pixel_rect(e, g, self.doc.project.grid_size);
        let o = self.doc.level(ix).world_px;
        [
            o[0] as f64 + x as f64 + w as f64 / 2.0,
            o[1] as f64 + y as f64 + h as f64 / 2.0,
        ]
    }

    /// A Select press at world pixel `wp`, as the viewport makes it (not on
    /// the tab or a handle), then a drag to `to` and the release: the
    /// press's `select_press`, then Begin / Move / End.
    fn select_drag(&mut self, wp: [f64; 2], to: [f64; 2], ctrl: bool) -> Option<Grab> {
        let level_under = (0..self.doc.levels.len()).find(|&i| {
            let r = self.doc.level_rect(i);
            wp[0] >= r[0] && wp[1] >= r[1] && wp[0] < r[0] + r[2] && wp[1] < r[1] + r[3]
        });
        let press = SelectPress {
            active: self.view.active_level,
            on_tab: false,
            handle: None,
            level_under,
            wp,
            add: ctrl,
            zoom: self.view.zoom,
        };
        let (ix, grab) = select_press(&self.doc, &mut self.view, press)?;
        let layer = self.objects();
        let m = egui::Modifiers {
            ctrl,
            command: ctrl,
            ..Default::default()
        };
        let mut start = cell_hit(&self.doc, ix, layer, [0, 0]);
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

    fn selected(&self) -> Vec<String> {
        self.view
            .selection
            .entities()
            .map(|i| i.to_string())
            .collect()
    }

    fn bake_ptrs(&self) -> Vec<(usize, String, usize)> {
        let mut v = Vec::new();
        for (ix, b) in self.doc.bakes.iter().enumerate() {
            for (l, bake) in b.as_ref().unwrap().iter() {
                v.push((ix, l.to_string(), Arc::as_ptr(bake) as usize));
            }
        }
        v
    }
}

// ---- the press: what it grabs ----

#[test]
fn a_press_grabs_the_entity_the_marquee_the_tab_or_another_level() {
    let mut r = Rig::new();
    let crev = r.level("crevasse");
    let shaft = r.level("shaft");
    let press = |r: &mut Rig, wp: [f64; 2], level_under, on_tab, add| {
        let active = r.view.active_level;
        select_press(
            &r.doc,
            &mut r.view,
            SelectPress {
                active,
                on_tab,
                handle: None,
                level_under,
                wp,
                add,
                zoom: 4.0,
            },
        )
    };
    // On an entity: it is selected, and the drag moves the selection.
    let door = r.center_of("doorcrev22");
    assert_eq!(
        press(&mut r, door, Some(crev), false, false),
        Some((
            crev,
            Grab::Selection {
                was_selected: false
            }
        ))
    );
    assert_eq!(r.selected(), ["doorcrev22"]);
    // Empty space in the active level: a marquee (the selection stays
    // until the release).
    assert_eq!(
        press(&mut r, [40.0, 20.0], Some(crev), false, false),
        Some((crev, Grab::Marquee))
    );
    assert_eq!(r.selected(), ["doorcrev22"]);
    // The active level's name tab moves the level, even over its body.
    assert_eq!(
        press(&mut r, [40.0, 20.0], Some(crev), true, false),
        Some((crev, Grab::Level))
    );
    // Another level's body: it becomes active, selected, and moves alone;
    // the rest of the selection goes (Ctrl keeps it).
    assert_eq!(
        press(&mut r, [330.0, 90.0], Some(shaft), false, true),
        Some((shaft, Grab::Level))
    );
    assert_eq!(r.view.active_level, Some(shaft));
    assert_eq!(r.selected(), ["doorcrev22"]);
    let shaft_item = SelItem::Level(r.doc.ids[shaft].clone());
    assert!(r.view.selection.contains(&shaft_item));
    // A press on a selected level's body grabs the whole selection.
    r.view.active_level = Some(crev);
    assert_eq!(
        press(&mut r, [330.0, 90.0], Some(shaft), false, false),
        Some((shaft, Grab::Selection { was_selected: true }))
    );
    r.view.selection.set_entities([id("doorcrev22")]);
    r.view.active_level = Some(crev);
    assert_eq!(
        press(&mut r, [330.0, 90.0], Some(shaft), false, false),
        Some((shaft, Grab::Level))
    );
    assert!(r.selected().is_empty());
    assert_eq!(r.view.selection.items, [shaft_item]);
    // Outside every level: a world marquee (the selection stays until the
    // release).
    r.view.active_level = Some(crev);
    r.view.selection.set_entities([id("doorcrev22")]);
    assert_eq!(
        press(&mut r, [-500.0, -500.0], None, false, false),
        Some((crev, Grab::WorldMarquee))
    );
    assert_eq!(r.selected(), ["doorcrev22"]);
    // A locked layer's entities aren't picked: the press is a marquee.
    let objects = r.objects();
    r.view.layers[objects].locked = true;
    r.view.active_level = Some(crev);
    assert_eq!(
        press(&mut r, door, Some(crev), false, false),
        Some((crev, Grab::Marquee))
    );
}

#[test]
fn the_tab_moves_the_level_and_the_body_is_a_marquee() {
    // The command layer is the same for a tab drag as for the old body
    // drag: one move_level on release.
    let mut r = Rig::new();
    let crev = r.level("crevasse");
    // Body drag in the active level: a marquee, no command, no move.
    let before = r.doc.level(crev).world_px;
    r.select_drag([4.0, 4.0], [310.0, 75.0], false);
    assert_eq!(r.doc.level(crev).world_px, before);
    assert_eq!(r.entries(), 0);
    // The marquee touched both entities of crevasse.
    let mut sel = r.selected();
    sel.sort();
    assert_eq!(sel, ["btncrev222", "doorcrev22"]);
    // A tab drag (Grab::Level) moves it.
    let layer = r.objects();
    let mut start = cell_hit(&r.doc, crev, layer, [1, 1]);
    let mut at = start;
    at.world_px = [start.world_px[0] + 32.0, start.world_px[1]];
    start.world_px = [start.world_px[0], start.world_px[1]];
    let m = egui::Modifiers::NONE;
    r.events(&[
        ToolEvent::Begin {
            tool: Tool::Select,
            at: start,
            modifiers: m,
            grab: Grab::Level,
        },
        ToolEvent::End {
            tool: Tool::Select,
            start,
            at,
            modifiers: m,
        },
    ]);
    assert_eq!(r.doc.level(crev).world_px, [before[0] + 32, before[1]]);
    assert_eq!(r.ed.undo_label().as_deref(), Some("Move level crevasse"));
}

#[test]
fn marquee_replaces_or_with_ctrl_adds_and_a_click_on_nothing_clears() {
    let mut r = Rig::new();
    // Around the button only.
    let b = r.center_of("btncrev222");
    // (From empty space up-left of it: a press on it would grab it.)
    r.select_drag([b[0] - 12.0, b[1] - 12.0], [b[0] + 2.0, b[1] + 2.0], false);
    assert_eq!(r.selected(), ["btncrev222"]);
    // Ctrl adds the door.
    let d = r.center_of("doorcrev22");
    r.select_drag([d[0] + 9.0, d[1]], [d[0] - 1.0, d[1] + 1.0], true);
    assert_eq!(r.selected(), ["btncrev222", "doorcrev22"]);
    // A click on empty space (a zero-size marquee) clears it.
    r.select_drag([40.0, 20.0], [40.0, 20.0], false);
    assert!(r.selected().is_empty());
    assert_eq!(r.entries(), 0, "selecting is not an edit");
}

// ---- dragging the selection ----

#[test]
fn dragging_a_selected_entity_moves_the_selection_as_one_entry() {
    let mut r = Rig::new();
    r.view
        .selection
        .set_entities([id("doorcrev22"), id("btncrev222")]);
    let door0 = r.entity("doorcrev22");
    let btn0 = r.entity("btncrev222");
    let c = r.center_of("doorcrev22");
    // 2 cells left, 0.4 cell down: whole cells (nearest).
    let grab = r.select_drag(c, [c[0] - 32.0, c[1] + 6.0], false);
    assert_eq!(grab, Some(Grab::Selection { was_selected: true }));
    assert_eq!(
        r.entity("doorcrev22").cell,
        [door0.cell[0] - 2, door0.cell[1]]
    );
    assert_eq!(
        r.entity("btncrev222").cell,
        [btn0.cell[0] - 2, btn0.cell[1]]
    );
    assert_eq!(
        r.entity("btncrev222").offset_px,
        btn0.offset_px,
        "offset kept"
    );
    assert_eq!(r.entries(), 1);
    assert_eq!(r.ed.undo_label().as_deref(), Some("Move 2 entities"));
    assert_eq!(r.selected(), ["doorcrev22", "btncrev222"]);
    // Ctrl: by pixels, folded into the cell.
    let c = r.center_of("btncrev222");
    r.select_drag(c, [c[0] + 21.0, c[1]], true);
    let b = r.entity("btncrev222");
    assert_eq!((b.cell[0], b.offset_px[0]), (btn0.cell[0] - 2 + 1, 5));
    // A click (no move) on a member narrows the selection to it.
    let c = r.center_of("doorcrev22");
    r.select_drag(c, c, false);
    assert_eq!(r.selected(), ["doorcrev22"]);
    // Undo puts both back.
    r.undo();
    r.undo();
    assert_eq!(r.entity("doorcrev22").cell, door0.cell);
    assert_eq!(r.entity("btncrev222"), btn0);
}

#[test]
fn ctrl_click_toggles_and_a_drag_off_the_level_is_refused() {
    let mut r = Rig::new();
    let d = r.center_of("doorcrev22");
    let b = r.center_of("btncrev222");
    r.select_drag(d, d, false);
    r.select_drag(b, b, true);
    assert_eq!(r.selected(), ["doorcrev22", "btncrev222"]);
    r.select_drag(d, d, true);
    assert_eq!(r.selected(), ["btncrev222"], "Ctrl+click takes it out");
    // Dragging out of the level: refused, nothing moved, no entry.
    let btn0 = r.entity("btncrev222");
    r.select_drag(b, [b[0] + 2000.0, b[1]], false);
    assert_eq!(r.entity("btncrev222"), btn0);
    assert_eq!(r.entries(), 0);
    assert!(r.view.status.as_ref().is_some_and(|s| s.error));
    assert!(!r.ed.in_gesture());
}

#[test]
fn move_commands_snap_per_layer_grid() {
    let r = Rig::new();
    let cmds = entity_move_commands(&r.doc, &[id("doorcrev22")], [7.0, 0.0], false);
    assert!(cmds.is_empty(), "under half a cell: no move");
    let cmds = entity_move_commands(&r.doc, &[id("doorcrev22")], [9.0, 0.0], false);
    assert!(matches!(
        &cmds[..],
        [Command::MoveEntity { cell: [19, 3], .. }]
    ));
}

// ---- keys: nudge, duplicate, delete ----

#[test]
fn nudges_fold_the_offset_into_the_cell() {
    let mut r = Rig::new();
    // btncrev222: cell [12, 3], offset [0, -2], 16 px cells.
    let ids = [id("btncrev222")];
    let up = nudge_commands(&r.doc, &ids, [0, -1], true);
    r.batch("Nudge", up).unwrap();
    assert_eq!(r.entity("btncrev222").offset_px, [0, -3]);
    for _ in 0..13 {
        let up = nudge_commands(&r.doc, &ids, [0, -1], true);
        r.batch("Nudge", up).unwrap();
    }
    // -16 px folded: one cell up, no offset.
    let b = r.entity("btncrev222");
    assert_eq!((b.cell, b.offset_px), ([12, 2], [0, 0]));
    // A cell nudge keeps the offset; each nudge is one entry.
    let right = nudge_commands(&r.doc, &ids, [1, 0], false);
    r.batch("Nudge", right).unwrap();
    assert_eq!(r.entity("btncrev222").cell, [13, 2]);
    assert_eq!(r.entries(), 15);
    // Out of the level: refused, nothing changes.
    let up: Vec<Command> = (0..3)
        .map(|k| Command::MoveEntity {
            id: id("btncrev222"),
            cell: [13, 1 - k],
            offset_px: [0, 0],
            allow_outside: false,
        })
        .collect();
    assert!(r.batch("Nudge", up).is_err());
    assert_eq!(r.entity("btncrev222").cell, [13, 2]);
    assert_eq!(r.entries(), 15);
}

#[test]
fn ctrl_d_copies_right_or_in_place_at_the_edge_keeping_fields() {
    let mut r = Rig::new();
    // The door to the last column of crevasse (20 wide): its copy stays
    // on its cell; the button's goes one right.
    r.apply(Command::MoveEntity {
        id: id("doorcrev22"),
        cell: [19, 3],
        offset_px: [0, 0],
        allow_outside: false,
    })
    .unwrap();
    let ids = [id("doorcrev22"), id("btncrev222")];
    let d = duplicate_commands(&r.doc, &ids);
    assert_eq!(d.overlapping, 1);
    assert_eq!(d.ids.len(), 2);
    assert!(d.ids.iter().all(|n| !ids.contains(n)), "new ids");
    assert_ne!(d.ids[0], d.ids[1]);
    r.batch("Duplicate 2 entities", d.commands).unwrap();
    assert_eq!(r.entries(), 2);
    let door = r.entity("doorcrev22");
    let copy = r.entity(d.ids[0].as_str());
    assert_eq!(copy.cell, door.cell, "the next cell is outside");
    assert_eq!(copy.fields, door.fields, "fields kept, references too");
    assert_eq!(copy.size, door.size);
    let btn = r.entity("btncrev222");
    let copy = r.entity(d.ids[1].as_str());
    assert_eq!(copy.cell, [btn.cell[0] + 1, btn.cell[1]]);
    assert_eq!(copy.fields["target"], json!("doorcrev22"));
    r.undo();
    assert!(r.doc.find_entity(&d.ids[0]).is_none());
}

/// The fixture with btncrev222 moved to a second entities layer, `props`.
fn two_entity_layers() -> Rig {
    Rig::new_with(|dir| {
        let path = dir.join("layers.json");
        let mut v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        v["layers"]
            .as_array_mut()
            .unwrap()
            .push(json!({ "slug": "props", "name": "Props", "kind": "entities" }));
        std::fs::write(&path, serde_json::to_string_pretty(&v).unwrap()).unwrap();
        let path = dir.join("worlds/main/levels/crevasse/entities.json");
        let mut v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let layers = v["layers"].as_array_mut().unwrap();
        let list = layers[0]["entities"].as_array_mut().unwrap();
        let i = list.iter().position(|e| e["id"] == "btncrev222").unwrap();
        let btn = list.remove(i);
        layers.push(json!({ "layer": "props", "entities": [btn] }));
        std::fs::write(&path, serde_json::to_string_pretty(&v).unwrap()).unwrap();
    })
}

#[test]
fn selection_edits_leave_entities_on_locked_layers_alone() {
    let mut r = two_entity_layers();
    let props = r
        .doc
        .project
        .layers
        .iter()
        .position(|l| l.slug == "props")
        .unwrap();
    let both = [id("doorcrev22"), id("btncrev222")];
    r.view.selection.set_entities(both.clone());
    r.view.layers[props].locked = true;
    // The keys (Delete, Ctrl-D, arrows) build their commands from what this
    // lets through.
    let (ok, why) = unlocked_entities(&r.doc, &r.view, &both);
    assert_eq!(ok, [id("doorcrev22")]);
    assert_eq!(
        why.as_deref(),
        Some("1 selected entity is on a locked layer: left alone")
    );
    let (ok, why) = unlocked_entities(&r.doc, &r.view, &[id("btncrev222")]);
    assert!(ok.is_empty());
    assert_eq!(why.as_deref(), Some("Layer Props is locked"));
    assert_eq!(delete_commands(&r.doc, &ok).len(), 0);
    // A drag of the selection moves the door only.
    let door0 = r.entity("doorcrev22");
    let btn0 = r.entity("btncrev222");
    let c = r.center_of("doorcrev22");
    r.select_drag(c, [c[0] - 32.0, c[1]], false);
    assert_eq!(
        r.entity("doorcrev22").cell,
        [door0.cell[0] - 2, door0.cell[1]]
    );
    assert_eq!(r.entity("btncrev222"), btn0, "locked: not moved");
    assert_eq!(r.entries(), 1);
    assert!(
        r.view
            .status
            .as_ref()
            .is_some_and(|m| m.text.contains("left alone")),
        "{:?}",
        r.view.status
    );
}

#[test]
fn delete_removes_the_selection_and_undo_brings_it_back_unselected() {
    let mut r = Rig::new();
    r.view
        .selection
        .set_entities([id("doorcrev22"), id("doorshaft2")]);
    let cmds = delete_commands(&r.doc, &r.view.selection.entity_ids());
    assert_eq!(cmds.len(), 2);
    r.batch("Delete 2 entities", cmds).unwrap();
    assert!(r.doc.find_entity(&id("doorshaft2")).is_none());
    assert!(r.selected().is_empty(), "pruned: they are gone");
    r.undo();
    assert!(r.doc.find_entity(&id("doorshaft2")).is_some());
    assert_eq!(r.entries(), 0);
}

#[test]
fn undo_prunes_entities_it_takes_away_from_the_selection() {
    let mut r = Rig::new();
    let mut e = r.entity("btncrev222");
    e.id = id("newbtn2222");
    e.cell = [2, 2];
    r.apply(Command::AddEntity {
        level: slug("crevasse"),
        layer: slug("objects"),
        entity: e,
        index: None,
        allow_outside: false,
    })
    .unwrap();
    r.view
        .selection
        .set_entities([id("doorcrev22"), id("newbtn2222")]);
    r.undo();
    assert_eq!(r.selected(), ["doorcrev22"]);
    // Redo brings the entity back, not its selection.
    r.redo();
    assert_eq!(r.selected(), ["doorcrev22"]);
    // Removing a type takes its instances out of the selection too.
    r.view
        .selection
        .set_entities([id("doorcrev22"), id("btncrev222")]);
    r.apply(Command::RemoveEntityDef {
        entity: slug("locked_door"),
        keep_instances: false,
    })
    .unwrap();
    assert_eq!(r.selected(), ["btncrev222"]);
}

// ---- canvas picks ----

#[test]
fn a_pick_takes_an_entity_in_another_level_or_a_cell() {
    let mut r = Rig::new();
    let shaft = r.level("shaft");
    let crev = r.level("crevasse");
    r.view.start_pick(
        PickAccept {
            entity: true,
            cell: false,
            ..Default::default()
        },
        "btncrev222:field:target".into(),
        "Pick the target".into(),
    );
    // A click on nothing it takes leaves it armed.
    pick_at(&r.doc, &mut r.view, Some(shaft), Some([321.0, 81.0]), 4.0);
    assert!(r.view.pick.is_some() && r.view.picked.is_none());
    // The door in the other level.
    let wp = r.center_of("doorshaft2");
    pick_at(&r.doc, &mut r.view, Some(shaft), Some(wp), 4.0);
    assert!(r.view.pick.is_none());
    assert_eq!(r.view.take_picked("someone else"), None);
    assert_eq!(
        r.view.take_picked("btncrev222:field:target"),
        Some(PickedThing::Entity {
            id: id("doorshaft2"),
            level: shaft
        })
    );
    // A cell pick answers with the level and the pixel.
    r.view.start_pick(
        PickAccept {
            entity: false,
            cell: true,
            ..Default::default()
        },
        "p".into(),
        "Pick a cell".into(),
    );
    pick_at(&r.doc, &mut r.view, Some(crev), Some([40.0, 20.0]), 4.0);
    assert_eq!(
        r.view.take_picked("p"),
        Some(PickedThing::Cell {
            level: crev,
            world_px: [40.0, 20.0]
        })
    );
}

// ---- field definition commands bake nothing ----

#[test]
fn field_definition_commands_keep_every_bake() {
    // Without a rule group reading a level field, none of the six field
    // commands (nor their undo and redo) rebakes anything or rebuilds the
    // scene.
    let mut r = Rig::new_with(|dir| {
        let p = dir.join("rules/main_tiling.json");
        let text = std::fs::read_to_string(&p).unwrap();
        let text: String = text
            .lines()
            .filter(|l| !l.contains("requires_level_field"))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&p, text).unwrap();
    });
    // A stroke first, so the cache holds crevasse's bakes too.
    let col = r
        .doc
        .project
        .layers
        .iter()
        .position(|l| l.slug == "collision")
        .unwrap();
    let crev = r.level("crevasse");
    let at = cell_hit(&r.doc, crev, col, [3, 3]);
    let m = egui::Modifiers::NONE;
    r.events(&[
        ToolEvent::Begin {
            tool: Tool::Brush,
            at,
            modifiers: m,
            grab: Grab::None,
        },
        ToolEvent::End {
            tool: Tool::Brush,
            start: at,
            at,
            modifiers: m,
        },
    ]);
    let before = r.bake_ptrs();
    assert!(!before.is_empty());
    let mut lever = FieldDef::new(slug("lever"), FieldType::Bool);
    lever.default = Some(json!(false));
    let mut channel = r
        .doc
        .project
        .entity_def("locked_door")
        .unwrap()
        .field("channel")
        .unwrap()
        .clone();
    channel.ty = FieldType::Int {
        min: Some(0.into()),
        max: Some(7.into()),
    };
    let weather = FieldDef::new(slug("weather"), FieldType::string());
    let cmds = vec![
        Command::SetEntityFieldDef {
            entity: slug("locked_door"),
            field: slug("lever"),
            def: lever,
            index: Some(0),
        },
        Command::SetEntityFieldDef {
            entity: slug("locked_door"),
            field: slug("channel"),
            def: channel,
            index: Some(3),
        },
        Command::RenameEntityField {
            entity: slug("locked_door"),
            from: slug("needs_item"),
            to: slug("key_item"),
        },
        Command::RemoveEntityFieldDef {
            entity: slug("locked_door"),
            field: slug("speed"),
            keep_values: false,
        },
        Command::SetLevelFieldDef {
            field: slug("weather"),
            def: weather,
            index: Some(0),
        },
        Command::RenameLevelField {
            from: slug("music"),
            to: slug("track"),
        },
        Command::RemoveLevelFieldDef {
            field: slug("track"),
            keep_values: false,
        },
    ];
    let n = cmds.len();
    for cmd in cmds {
        let label = cmd.label();
        r.inv.clear();
        r.apply(cmd).unwrap_or_else(|e| panic!("{label}: {e}"));
        assert!(r.inv.is_empty(), "{label}: {:?}", r.inv);
        assert_eq!(r.bake_ptrs(), before, "{label}");
        assert_eq!(r.doc.layer_jobs_pending, 0);
    }
    assert_eq!(r.entries(), 1 + n);
    // The rename carried the stored value; the remove took speed's.
    assert_eq!(
        r.entity("doorcrev22").fields["key_item"],
        json!("brass_key")
    );
    assert!(!r.entity("doorcrev22").fields.contains_key("speed"));
    for _ in 0..n {
        r.inv.clear();
        r.undo();
        assert!(r.inv.is_empty(), "{:?}", r.inv);
        assert_eq!(r.bake_ptrs(), before);
    }
    assert_eq!(r.entity("doorcrev22").fields["speed"], json!(2));
    for _ in 0..n {
        r.inv.clear();
        r.redo();
        assert!(!r.inv.contains(&Invalidation::All));
        assert_eq!(r.bake_ptrs(), before);
    }
    assert_eq!(r.entries(), 1 + n, "history kept");
}

// ---- live problems ----

#[test]
fn problems_follow_an_edit_after_a_quiet_moment_and_never_mid_gesture() {
    let mut r = Rig::new();
    let rev0 = r.ed.rev();
    let mut lp = LiveProblems::new(rev0, true);
    lp.quiet = Duration::from_millis(200);
    let t0 = Instant::now();
    assert_eq!(lp.poll(&r.doc, r.ed.rev(), false, t0), Poll::Idle);
    // Channel 99 on a button whose max is 15.
    r.apply(Command::SetEntityField {
        id: id("btncrev222"),
        field: slug("channel"),
        value: Some(json!(99)),
        allow_unknown: false,
    })
    .unwrap();
    // Too soon: wait.
    assert!(matches!(
        lp.poll(&r.doc, r.ed.rev(), false, t0),
        Poll::Wait(_)
    ));
    assert_eq!(lp.runs, 0);
    // A gesture is open (a stroke starts): still nothing, however long.
    let col = r
        .doc
        .project
        .layers
        .iter()
        .position(|l| l.slug == "collision")
        .unwrap();
    let crev = r.level("crevasse");
    let at = cell_hit(&r.doc, crev, col, [2, 2]);
    r.events(&[ToolEvent::Begin {
        tool: Tool::Brush,
        at,
        modifiers: egui::Modifiers::NONE,
        grab: Grab::None,
    }]);
    let later = t0 + Duration::from_millis(400);
    assert!(r.ed.in_gesture());
    assert!(matches!(
        lp.poll(&r.doc, r.ed.rev(), r.ed.in_gesture(), later),
        Poll::Wait(_)
    ));
    assert_eq!(lp.runs, 0, "never mid-gesture");
    r.events(&[ToolEvent::End {
        tool: Tool::Brush,
        start: at,
        at,
        modifiers: egui::Modifiers::NONE,
    }]);
    // Quiet again: one run, and the list names the entity.
    let t1 = later + Duration::from_millis(1);
    assert!(matches!(
        lp.poll(&r.doc, r.ed.rev(), false, t1),
        Poll::Wait(_)
    ));
    let fresh = match lp.poll(&r.doc, r.ed.rev(), false, t1 + Duration::from_millis(250)) {
        Poll::Fresh(list) => list,
        other => panic!("{other:?}"),
    };
    assert_eq!(lp.runs, 1);
    assert!(
        fresh
            .iter()
            .any(|p| p.path.contains("entities[btncrev222]") && p.path.ends_with("channel")),
        "{fresh:?}"
    );
    assert_eq!(
        worlds_editor::app::panels::problem_target(
            fresh
                .iter()
                .find(|p| p.path.contains("btncrev222"))
                .unwrap()
        ),
        Some(worlds_editor::app::panels::ProblemTarget::Entity(id(
            "btncrev222"
        )))
    );
    // Nothing changed since: no more runs.
    assert_eq!(
        lp.poll(&r.doc, r.ed.rev(), false, t1 + Duration::from_secs(5)),
        Poll::Idle
    );
    // Undo clears it again.
    r.undo();
    r.undo();
    let t2 = t1 + Duration::from_secs(6);
    let _ = lp.poll(&r.doc, r.ed.rev(), false, t2);
    match lp.poll(&r.doc, r.ed.rev(), false, t2 + Duration::from_millis(250)) {
        Poll::Fresh(list) => assert!(
            !list.iter().any(|p| p.path.contains("btncrev222")),
            "{list:?}"
        ),
        other => panic!("{other:?}"),
    }
}

#[test]
fn worker_results_for_an_old_revision_are_dropped() {
    let mut r = Rig::new();
    let mut lp = LiveProblems::new(r.ed.rev(), false);
    lp.quiet = Duration::ZERO;
    r.apply(Command::SetEntityField {
        id: id("btncrev222"),
        field: slug("channel"),
        value: Some(json!(99)),
        allow_unknown: false,
    })
    .unwrap();
    let t = Instant::now();
    // Starts a worker for this revision.
    assert_eq!(lp.poll(&r.doc, r.ed.rev(), false, t), Poll::Idle);
    assert_eq!(lp.runs, 1);
    // An edit lands before it reports: its result is stale.
    r.undo();
    let mut fresh = None;
    for _ in 0..500 {
        if let Poll::Fresh(l) = lp.poll(&r.doc, r.ed.rev(), false, Instant::now()) {
            fresh = Some(l);
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let fresh = fresh.expect("a result for the new revision");
    assert_eq!(lp.stale, 1, "the first worker's list was dropped");
    assert_eq!(lp.runs, 2);
    assert!(!fresh.iter().any(|p| p.path.contains("btncrev222")));
}

#[test]
fn problem_targets() {
    use worlds_editor::app::panels::{ProblemTarget, problem_target};
    let p = |file: &str, path: &str| worlds_core::Problem {
        file: file.into(),
        path: path.into(),
        message: "x".into(),
    };
    assert_eq!(
        problem_target(&p(
            "worlds/main/levels/crevasse/entities.json",
            "layers[objects].entities[doorcrev22].fields.channel"
        )),
        Some(ProblemTarget::Entity(id("doorcrev22")))
    );
    assert_eq!(
        problem_target(&p("entities/locked_door.json", "fields[2].regex")),
        Some(ProblemTarget::Type(slug("locked_door")))
    );
    assert_eq!(
        problem_target(&p("project.json", "level_fields[1].default")),
        Some(ProblemTarget::LevelFields)
    );
    assert_eq!(
        problem_target(&p("worlds/main/levels/shaft/collision.json", "rows[2]")),
        Some(ProblemTarget::Level("shaft".into()))
    );
    assert_eq!(problem_target(&p("layers.json", "layers[0]")), None);
}

/// A fresh project (`worlds-cli new`) has no layer: the Select tool still
/// selects, moves and resizes its levels, in cells of the project grid,
/// and a world marquee picks them. Painting tools still refuse.
#[test]
fn select_moves_and_resizes_levels_with_no_layers() {
    use worlds_editor::script::world_hit;
    use worlds_editor::tools::{CellHit, NO_LAYER};
    use worlds_editor::viewport::level_under;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("fresh.worlds");
    let mut p = worlds_core::Project::new("fresh");
    p.grid_size = 32;
    worlds_core::save(&mut p, &dir).unwrap();
    let mut doc = Document::open(&dir).unwrap();
    let mut view = ViewState::new();
    view.sync_layers(&doc, None);
    let mut inv = Vec::new();
    let mut ed = Editing::new();
    let mut gesture = |doc: &mut Document,
                       view: &mut ViewState,
                       tool: Tool,
                       grab: Grab,
                       start: CellHit,
                       at: CellHit| {
        let mut cx = CommandContext {
            doc,
            view,
            invalidate: &mut inv,
        };
        let m = egui::Modifiers::NONE;
        ed.tool_events(
            &mut cx,
            &[
                ToolEvent::Begin {
                    tool,
                    at: start,
                    modifiers: m,
                    grab,
                },
                ToolEvent::End {
                    tool,
                    start,
                    at,
                    modifiers: m,
                },
            ],
        );
        (ed.undo_label(), ed.history().undo_entries().len())
    };
    for (s, i, at) in [("a", "aaaaaaaaaa", [0, 0]), ("b", "bbbbbbbbbb", [640, 0])] {
        let cmd: Command = serde_json::from_value(json!({
            "op": "create_level", "world": "main",
            "level": { "id": i, "slug": s, "name": s, "world_px": at, "size": [10, 6], "depth": 0 }
        }))
        .unwrap();
        doc.project.apply(cmd).unwrap();
    }
    doc.reindex();
    assert!(doc.project.layers.is_empty());
    let a = doc.level_ix("a").unwrap();
    let b = doc.level_ix("b").unwrap();
    view.active_level = Some(a);
    // A press on b's body makes it active and grabs it.
    let press = |doc: &Document, view: &mut ViewState, active, wp: [f64; 2]| {
        select_press(
            doc,
            view,
            SelectPress {
                active,
                on_tab: false,
                handle: None,
                level_under: level_under(doc, wp),
                wp,
                add: false,
                zoom: 1.0,
            },
        )
    };
    let wp = [700.0, 50.0];
    assert_eq!(press(&doc, &mut view, Some(a), wp), Some((b, Grab::Level)));
    assert_eq!(view.active_level, Some(b));
    let start = world_hit(&doc, b, NO_LAYER, wp);
    assert_eq!(start.cell, [1, 1], "cells of the 32 px project grid");
    let to = world_hit(&doc, b, NO_LAYER, [wp[0] + 64.0, wp[1] + 32.0]);
    let (label, _) = gesture(&mut doc, &mut view, Tool::Select, Grab::Level, start, to);
    assert_eq!(doc.level(b).world_px, [704, 32]);
    assert_eq!(label.as_deref(), Some("Move level b"));
    // Its bottom-right handle resizes it by whole project cells.
    let corner = [704.0 + 320.0 - 4.0, 32.0 + 192.0 - 4.0];
    let start = world_hit(&doc, b, NO_LAYER, corner);
    let to = world_hit(&doc, b, NO_LAYER, [corner[0] + 64.0, corner[1] + 32.0]);
    gesture(
        &mut doc,
        &mut view,
        Tool::Select,
        Grab::Handle([1, 1]),
        start,
        to,
    );
    assert_eq!(doc.level(b).size, [12, 7]);
    // A world marquee around a picks it.
    let wp = [-50.0, -50.0];
    let (ix, grab) = press(&doc, &mut view, Some(b), wp).expect("a world marquee");
    assert_eq!(grab, Grab::WorldMarquee);
    let start = world_hit(&doc, ix, NO_LAYER, wp);
    let to = world_hit(&doc, ix, NO_LAYER, [100.0, 100.0]);
    let (_, entries) = gesture(&mut doc, &mut view, Tool::Select, grab, start, to);
    assert_eq!(view.selection.items, [SelItem::Level(doc.ids[a].clone())]);
    // Painting tools still refuse: there is no layer to paint on.
    let (_, after) = gesture(&mut doc, &mut view, Tool::Brush, Grab::None, start, to);
    assert_eq!(after, entries);
}
