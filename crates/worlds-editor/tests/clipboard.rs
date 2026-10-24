//! Copy, cut and paste in the editor, and Ctrl+D on a level, without a GPU:
//! through the same functions the keys and `--script` use
//! ([`worlds_editor::clipboard`], [`worlds_editor::annot::plan_duplicate`],
//! the Select marquee). On a temp copy of `worlds-core`'s `mini.worlds`
//! (crevasse at (0, 0), 20 x 5 cells of 16; shaft at (320, 80), 6 x 8).

use std::path::Path;

use worlds_core::{Command, Id};
use worlds_editor::clipboard::{self, PasteGhost};
use worlds_editor::doc::Document;
use worlds_editor::edit::Editing;
use worlds_editor::script::cell_hit;
use worlds_editor::tools::{CommandContext, EditorCommands, Invalidation, Tool, ToolEvent};
use worlds_editor::view::{SelItem, ViewState};
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
        let doc = Document::open(&dir).unwrap();
        let mut view = ViewState::new();
        view.sync_layers(&doc, None);
        view.active_level = doc.level_ix("crevasse");
        view.active_layer = doc.project.layers.iter().position(|l| l.is_entities());
        view.zoom = 4.0;
        Rig {
            doc,
            view,
            ed: Editing::new(),
            inv: Vec::new(),
            _tmp: tmp,
        }
    }

    fn cx(&mut self) -> CommandContext<'_> {
        CommandContext {
            doc: &mut self.doc,
            view: &mut self.view,
            invalidate: &mut self.inv,
        }
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

    fn label(&self) -> String {
        self.ed
            .history()
            .undo_label()
            .unwrap_or_default()
            .to_string()
    }

    fn rows(&self, level: &str) -> Vec<String> {
        let def = self.doc.project.layer("collision").unwrap();
        self.doc
            .project
            .level(level)
            .unwrap()
            .int_grid("collision")
            .unwrap()
            .to_rows(def)
            .unwrap()
    }

    /// A Select marquee in the active level from world px `a` to `b`.
    fn marquee(&mut self, a: [f64; 2], b: [f64; 2]) {
        let press = SelectPress {
            active: self.view.active_level,
            on_tab: false,
            handle: None,
            level_under: level_under(&self.doc, a),
            wp: a,
            add: false,
            zoom: self.view.zoom,
        };
        let (ix, grab) = select_press(&self.doc, &mut self.view, press).unwrap();
        let layer = self.view.active_layer.unwrap_or(0);
        let mut start = cell_hit(&self.doc, ix, layer, [0, 0]);
        start.world_px = a;
        let mut at = start;
        at.world_px = b;
        let m = egui::Modifiers::default();
        let events = [
            ToolEvent::Begin {
                tool: Tool::Select,
                at: start,
                modifiers: m,
                grab,
            },
            ToolEvent::End {
                tool: Tool::Select,
                start,
                at,
                modifiers: m,
            },
        ];
        let mut ed = std::mem::take(&mut self.ed);
        ed.tool_events(&mut self.cx(), &events);
        self.ed = ed;
    }
}

#[test]
fn copy_entities_and_paste_them_elsewhere() {
    let mut r = Rig::new();
    r.view.selection.set([
        SelItem::Entity(id("btncrev222")),
        SelItem::Entity(id("doorcrev22")),
    ]);
    let c = clipboard::copy_selection(&r.doc, &r.view).unwrap();
    assert_eq!(c.message, "Copied 2 items");
    // Through text, as the system clipboard carries it.
    let text = serde_json::to_string(&c.fragment).unwrap();
    let frag = clipboard::parse(&text).unwrap().unwrap();
    assert_eq!(clipboard::parse("hello").unwrap(), None);

    // The ghost follows the pointer: the fragment (7 x 1 cells) is centred
    // on the cell under it.
    let crev = r.doc.level_ix("crevasse").unwrap();
    let mut g = PasteGhost::new(frag.clone());
    let origin = g.origin_under(&r.doc, crev, [5.0 * 16.0 + 3.0, 2.0 * 16.0 + 3.0]);
    assert_eq!(origin, [2, 2]);
    g.at = Some((crev, origin));
    g.refresh(&r.doc);
    let (_, ghost) = g.ghost.as_ref().unwrap();
    assert_eq!(ghost.boxes.len(), 2, "both entities drawn");
    assert!(ghost.error.is_none());

    let plan = clipboard::paste_plan(&r.doc, &g.fragment(), crev, origin).unwrap();
    r.apply(plan.command()).unwrap();
    assert_eq!(r.label(), "Paste 2 items");
    let sel = clipboard::pasted_selection(&r.doc, &plan, crev);
    assert_eq!(sel.len(), 2);
    let SelItem::Entity(btn) = &sel[0] else {
        panic!("{sel:?}")
    };
    let SelItem::Entity(door) = &sel[1] else {
        panic!()
    };
    let (_, _, e) = r.doc.find_entity(btn).unwrap();
    assert_eq!(e.cell, [2, 2]);
    assert_eq!(e.fields["target"], serde_json::json!(door.as_str()));
    r.undo();
    assert!(r.doc.find_entity(btn).is_none());

    // Paste in place: the source level at the source cells.
    let (ix, at) = clipboard::in_place_target(&r.doc, &r.view, &frag).unwrap();
    assert_eq!((ix, at), (crev, [12, 3]));
}

#[test]
fn a_marquee_on_an_int_grid_layer_copies_and_cuts_its_cells() {
    let mut r = Rig::new();
    r.view.active_layer = r
        .doc
        .project
        .layers
        .iter()
        .position(|l| l.slug == "collision");
    // Cells (4, 3) to (6, 3): "~~~" in the bottom row of the room.
    r.marquee(
        [4.0 * 16.0 + 2.0, 3.0 * 16.0 + 2.0],
        [6.0 * 16.0 + 10.0, 3.0 * 16.0 + 10.0],
    );
    // The marquee also picked the world region it touches; leave just the
    // cells.
    r.view.selection.clear();
    let m = r.view.grid_marquee.clone().expect("a grid marquee");
    assert_eq!(<[u32; 4]>::from(m.rect), [4, 3, 3, 1]);
    let c = clipboard::copy_selection(&r.doc, &r.view).unwrap();
    assert_eq!(
        c.fragment.grids[&worlds_core::slug("collision")].rows,
        ["~~~"]
    );
    let (_, commands, label) = clipboard::cut_selection(&r.doc, &r.view).unwrap();
    assert_eq!(label, "Cut 3 items");
    r.apply(Command::Batch { label, commands }).unwrap();
    assert_eq!(&r.rows("crevasse")[3][4..7], "...");
    // Into shaft, then the cells are back where they were copied from.
    let shaft = r.doc.level_ix("shaft").unwrap();
    let plan = clipboard::paste_plan(&r.doc, &c.fragment, shaft, [1, 1]).unwrap();
    r.apply(plan.command()).unwrap();
    assert_eq!(&r.rows("shaft")[1][1..4], "~~~");
    // The marquee only counts while its layer is active.
    r.view.active_layer = r.doc.project.layers.iter().position(|l| l.is_entities());
    assert!(clipboard::copy_selection(&r.doc, &r.view).is_err());
}

#[test]
fn ctrl_d_on_a_level_duplicates_it() {
    let mut r = Rig::new();
    let shaft = r.doc.level_ix("shaft").unwrap();
    r.view
        .selection
        .set([SelItem::Level(r.doc.ids[shaft].clone())]);
    let d = worlds_editor::annot::plan_duplicate(&r.doc, &r.view);
    assert_eq!(d.label, "Duplicate level shaft");
    r.apply(Command::Batch {
        label: d.label,
        commands: d.commands,
    })
    .unwrap();
    let copy = r.doc.project.level("shaft_copy").unwrap();
    assert_eq!(copy.world_px, [416, 80]);
    assert_eq!(copy.name, "Shaft copy");
    assert_eq!(copy.all_entities().count(), 1);
    // A second copy gets the next free slug.
    let (cmd, slug) = clipboard::duplicate_level_command(&r.doc, shaft, &[]);
    assert_eq!(slug.as_str(), "shaft_copy2");
    r.apply(cmd).unwrap();
    assert_eq!(
        r.doc.project.level("shaft_copy2").unwrap().world_px,
        [512, 80]
    );
    // Copying a selected level says how to duplicate it.
    let e = clipboard::copy_selection(&r.doc, &r.view).unwrap_err();
    assert!(e.contains("Ctrl+D"), "{e}");
}

#[test]
fn paste_script_steps_parse() {
    let steps = worlds_editor::script::parse(
        r#"[ { "ui": "copy" }, { "ui": "cut" },
             { "ui": "paste", "level": "shaft", "origin": [1, 2], "place": true },
             { "ui": "paste", "in_place": true } ]"#,
    )
    .unwrap();
    assert_eq!(steps.len(), 4);
    assert!(worlds_editor::script::parse(r#"[ { "ui": "paste", "bogus": 1 } ]"#).is_err());
}
