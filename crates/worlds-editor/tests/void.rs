//! Non-rectangular levels in the editor (no GPU): the Shape tool's cut and
//! fill (one `set_level_void` each), painting and the entity tool clipped
//! at the void, hit tests and fills that follow the outline, and the
//! rebake. On a temp copy of `worlds-core`'s `mini.worlds`.

use std::path::Path;

use worlds_core::Slug;
use worlds_core::command::Rect;
use worlds_editor::doc::Document;
use worlds_editor::edit::Editing;
use worlds_editor::script::{GestureStep, gesture_events};
use worlds_editor::tools::{CommandContext, EditorCommands, Invalidation};
use worlds_editor::view::ViewState;

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
        view.active_layer = doc
            .project
            .layers
            .iter()
            .position(|l| l.slug == "collision");
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

    /// Replay a gesture as the viewport (and `--script`) sends it.
    fn gesture(&mut self, json: &str) {
        let g: GestureStep = serde_json::from_str(json).unwrap();
        let plan = gesture_events(
            &self.doc,
            &g,
            self.view.active_layer,
            self.view.entities.brush.as_ref(),
        )
        .unwrap();
        if let Some(b) = plan.brush {
            let l = self.doc.project.layers[plan.layer].slug.to_string();
            self.view.palette.brushes.insert(l, b);
        }
        if let Some(b) = plan.entity_brush {
            self.view.entities.brush = Some(b);
        }
        self.view.tool = worlds_editor::script::parse_tool(&g.gesture).unwrap();
        self.view.active_level = Some(plan.level);
        if plan.layer < self.doc.project.layers.len() {
            self.view.active_layer = Some(plan.layer);
        }
        let mut ed = std::mem::take(&mut self.ed);
        ed.tool_events(&mut self.cx(), &plan.events);
        self.ed = ed;
    }

    fn undo(&mut self) {
        let mut ed = std::mem::take(&mut self.ed);
        ed.undo(&mut self.cx());
        self.ed = ed;
    }

    fn void(&self, level: &str) -> Vec<Rect> {
        self.doc.project.level(level).unwrap().void.clone()
    }

    fn rows(&self, level: &str) -> Vec<String> {
        let def = self.doc.project.layer("collision").unwrap();
        let l = self.doc.project.level(level).unwrap();
        l.int_grid("collision").unwrap().to_rows(def).unwrap()
    }

    fn fresh(&self, level: &str, layer: &str) -> worlds_rules::LayerBake {
        let ix = self.doc.level_ix(level).unwrap();
        worlds_rules::bake_layer(
            &self.doc.project,
            self.doc.level(ix),
            &Slug::new(layer).unwrap(),
            &self.doc.images,
        )
        .unwrap()
    }

    fn shown(&self, level: &str, layer: &str) -> worlds_rules::LayerBake {
        let ix = self.doc.level_ix(level).unwrap();
        (*self.doc.bakes[ix].as_ref().unwrap()[layer]).clone()
    }
}

#[test]
fn shape_cuts_and_fills_one_entry_each() {
    let mut r = Rig::new();
    // Cut the top-right corner (cells 16..19 x 0..1): the drag's two
    // corners, in project cells.
    r.gesture(r#"{ "gesture": "shape", "level": "crevasse", "path": [[16, 0], [19, 1]] }"#);
    assert_eq!(r.void("crevasse"), [Rect::new(16, 0, 4, 2)]);
    assert_eq!(r.ed.history().undo_label(), Some("Shape level crevasse"));
    assert_eq!(
        &r.rows("crevasse")[0][14..],
        "##....",
        "the walls there are cleared"
    );
    // The auto layer rebaked with the void as out of bounds.
    assert_eq!(
        r.shown("crevasse", "main_tiling"),
        r.fresh("crevasse", "main_tiling")
    );
    assert!(
        r.inv.iter().any(
            |i| matches!(i, Invalidation::Level(ix) if Some(*ix) == r.doc.level_ix("crevasse"))
        )
    );
    // Cutting inside the void again changes nothing (no entry).
    let before = r.ed.history().undo_entries().len();
    r.gesture(r#"{ "gesture": "shape", "level": "crevasse", "path": [[17, 0], [18, 1]] }"#);
    assert_eq!(r.ed.history().undo_entries().len(), before);
    // Shift-drag fills the middle of it back in: the rect splits.
    r.gesture(
        r#"{ "gesture": "shape", "level": "crevasse", "path": [[17, 0], [18, 1]], "shift": true }"#,
    );
    assert_eq!(
        r.void("crevasse"),
        [Rect::new(16, 0, 1, 2), Rect::new(19, 0, 1, 2)]
    );
    // A cut over the button's cell is refused, with no entry.
    let before = r.ed.history().undo_entries().len();
    r.gesture(r#"{ "gesture": "shape", "level": "crevasse", "path": [[11, 2], [13, 4]] }"#);
    assert_eq!(r.ed.history().undo_entries().len(), before);
    assert!(
        r.view
            .status
            .as_ref()
            .is_some_and(|s| s.error && s.text.contains("btncrev222"))
    );
    // Undo, twice: back to a rectangle with its walls.
    r.undo();
    r.undo();
    assert!(r.void("crevasse").is_empty());
    assert_eq!(r.rows("crevasse")[0], "####################");
    // A shape gesture takes no layer.
    let g: GestureStep = serde_json::from_str(
        r#"{ "gesture": "shape", "level": "crevasse", "layer": "collision", "path": [[0, 0]] }"#,
    )
    .unwrap();
    assert!(gesture_events(&r.doc, &g, None, None).is_err());
}

#[test]
fn painting_and_placing_clip_at_the_void() {
    let mut r = Rig::new();
    r.gesture(r#"{ "gesture": "shape", "level": "crevasse", "path": [[4, 1], [7, 2]] }"#);
    // A brush stroke across the void paints only the level's cells.
    r.gesture(
        r#"{ "gesture": "brush", "level": "crevasse", "layer": "collision", "path": [[2, 1], [9, 1]], "brush": "water" }"#,
    );
    assert_eq!(&r.rows("crevasse")[1][..11], "#.~~....~~.");
    // A rect over it fills around it.
    r.gesture(
        r#"{ "gesture": "rect", "level": "crevasse", "layer": "collision", "path": [[3, 2], [8, 2]], "brush": "solid" }"#,
    );
    assert_eq!(&r.rows("crevasse")[2][..10], "#..#....#o");
    // A flood from the left doesn't run through the void.
    r.gesture(
        r#"{ "gesture": "fill", "level": "crevasse", "layer": "decor_back", "path": [[0, 0]], "brush": { "tileset": "castle_main", "tile": [1, 1] } }"#,
    );
    let ix = r.doc.level_ix("crevasse").unwrap();
    let def = r.doc.project.layer("decor_back").unwrap();
    let l = r.doc.level(ix);
    let tiles = &l.tiles_layer("decor_back").unwrap().tiles;
    assert!(!tiles.is_empty());
    assert!(
        tiles.iter().all(|t| !r.doc.project.layer_cell_void(
            def,
            l,
            t.cell[0] as i64,
            t.cell[1] as i64
        )),
        "no tile in the void"
    );
    // The entity tool refuses a void cell, with a status and no entry.
    let before = r.ed.history().undo_entries().len();
    r.gesture(
        r#"{ "gesture": "entity", "level": "crevasse", "path": [[5, 1]], "type": "floor_button" }"#,
    );
    assert_eq!(r.ed.history().undo_entries().len(), before);
    assert!(
        r.view
            .status
            .as_ref()
            .is_some_and(|s| s.error && s.text.contains("cut out")),
        "{:?}",
        r.view.status
    );
}

#[test]
fn hit_tests_and_fills_follow_the_outline() {
    let mut r = Rig::new();
    // An L: crevasse (20x5 at [0, 0]) loses its top-right 4x2; shaft moves
    // into the notch.
    r.gesture(r#"{ "gesture": "shape", "level": "crevasse", "path": [[16, 0], [19, 1]] }"#);
    let mut ed = std::mem::take(&mut r.ed);
    ed.apply(
        &mut r.cx(),
        worlds_core::Command::MoveLevel {
            world: Slug::new("main").unwrap(),
            level: Slug::new("shaft").unwrap(),
            world_px: [256, -96],
        },
    )
    .unwrap();
    r.ed = ed;
    let crevasse = r.doc.level_ix("crevasse").unwrap();
    let shaft = r.doc.level_ix("shaft").unwrap();
    // The notch (cell [17, 1] of crevasse) is shaft's, a cell of the L's
    // lower arm is crevasse's.
    assert_eq!(
        worlds_editor::viewport::level_under(&r.doc, [17.0 * 16.0 + 4.0, 20.0]),
        Some(shaft)
    );
    assert_eq!(
        worlds_editor::viewport::level_under(&r.doc, [17.0 * 16.0 + 4.0, 40.0]),
        Some(crevasse)
    );
    assert!(!worlds_editor::viewport::in_level_px(
        &r.doc,
        crevasse,
        [300.0, 4.0]
    ));
    // The background fill covers exactly the cells.
    let rects = worlds_editor::viewport::level_cell_rects(&r.doc, crevasse);
    let area: f64 = rects
        .iter()
        .map(|w| (w.max[0] - w.min[0]) * (w.max[1] - w.min[1]))
        .sum();
    assert_eq!(area, (20.0 * 5.0 - 8.0) * 256.0);
    assert_eq!(rects.len(), 2);
    assert_eq!(
        worlds_editor::viewport::level_outline(&r.doc, crevasse),
        [
            [0.0, 0.0],
            [256.0, 0.0],
            [256.0, 32.0],
            [320.0, 32.0],
            [320.0, 80.0],
            [0.0, 80.0]
        ]
    );
}

#[test]
fn shape_void_cuts_and_fills_rects() {
    use worlds_editor::tools::shape_void;
    let size = [10, 6];
    // A cut over cells that are all void already changes nothing.
    let v = vec![Rect::new(0, 0, 4, 4)];
    assert_eq!(shape_void(&v, size, [1, 1, 3, 3], false), None);
    // A bigger cut swallows the rects it covers.
    assert_eq!(
        shape_void(&v, size, [0, 0, 5, 5], false),
        Some(vec![Rect::new(0, 0, 5, 5)])
    );
    // Clipped to the box; nothing left after clipping is nothing.
    assert_eq!(
        shape_void(&[], size, [8, -3, 14, 2], false),
        Some(vec![Rect::new(8, 0, 2, 2)])
    );
    assert_eq!(shape_void(&[], size, [12, 0, 14, 2], false), None);
    // A fill in the middle of a rect leaves its four sides.
    let v = vec![Rect::new(2, 1, 5, 4)];
    assert_eq!(
        shape_void(&v, size, [3, 2, 5, 4], true),
        Some(vec![
            Rect::new(2, 1, 5, 1),
            Rect::new(2, 4, 5, 1),
            Rect::new(2, 2, 1, 2),
            Rect::new(5, 2, 2, 2),
        ])
    );
    // A fill outside every rect changes nothing.
    assert_eq!(shape_void(&v, size, [8, 0, 9, 1], true), None);
}
