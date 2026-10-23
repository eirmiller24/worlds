//! The command layer driven the way the viewport drives it: tool events in,
//! commands, undo entries, invalidations and saved files out. Runs on a temp
//! copy of `worlds-core`'s `mini.worlds` fixture (no GPU).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use worlds_core::Slug;
use worlds_editor::app::watch::OwnWrites;
use worlds_editor::doc::Document;
use worlds_editor::edit::Editing;
use worlds_editor::script::{GestureStep, cell_hit, gesture_events};
use worlds_editor::tools::{
    Brush, CommandContext, EditorCommands, Grab, Invalidation, Tool, ToolEvent,
};
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

fn files(dir: &Path) -> BTreeMap<PathBuf, (Vec<u8>, SystemTime)> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else {
                let m = std::fs::metadata(&p).unwrap().modified().unwrap();
                out.insert(
                    p.strip_prefix(dir).unwrap().to_path_buf(),
                    (std::fs::read(&p).unwrap(), m),
                );
            }
        }
    }
    out
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
        // The fixture has no art: opaque placeholder atlases (the rules
        // check tile opacity).
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
        view.active_layer = Some(layer(&doc, "collision"));
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

    fn events(&mut self, events: &[ToolEvent]) {
        let mut ed = std::mem::take(&mut self.ed);
        ed.tool_events(&mut self.cx(), events);
        self.ed = ed;
    }

    /// Replay a gesture as the viewport would send it.
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
        self.view.active_layer = Some(plan.layer);
        self.events(&plan.events);
    }

    /// Apply a command in the wire format as its own undo entry.
    fn apply(&mut self, cmd: serde_json::Value) {
        self.apply_cmd(serde_json::from_value(cmd).unwrap());
    }

    fn apply_cmd(&mut self, cmd: worlds_core::Command) {
        let mut ed = std::mem::take(&mut self.ed);
        let r = ed.apply(&mut self.cx(), cmd);
        self.ed = ed;
        r.unwrap();
    }

    /// A fresh full bake of one layer of a level.
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

    /// The document's bake of one layer of a level.
    fn shown(&self, level: &str, layer: &str) -> worlds_rules::LayerBake {
        let ix = self.doc.level_ix(level).unwrap();
        (*self.doc.bakes[ix].as_ref().unwrap()[layer]).clone()
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

    fn save(&mut self) {
        let mut ed = std::mem::take(&mut self.ed);
        ed.save(&mut self.cx()).unwrap();
        self.ed = ed;
    }

    fn rows(&self, level: &str, layer: &str) -> Vec<String> {
        let def = self.doc.project.layer(layer).unwrap();
        self.doc
            .project
            .level(level)
            .unwrap()
            .int_grid(layer)
            .unwrap()
            .to_rows(def)
            .unwrap()
    }

    /// The cells (and rects) invalidated per layer, merged.
    fn invalidated(&self) -> BTreeMap<String, [i32; 4]> {
        let mut m: BTreeMap<String, [i32; 4]> = BTreeMap::new();
        for i in &self.inv {
            if let Invalidation::Cells { layer, rect, .. } = i {
                let e = m.entry(layer.to_string()).or_insert(*rect);
                *e = [
                    e[0].min(rect[0]),
                    e[1].min(rect[1]),
                    e[2].max(rect[2]),
                    e[3].max(rect[3]),
                ];
            }
        }
        m
    }
}

fn layer(doc: &Document, slug: &str) -> usize {
    doc.project
        .layers
        .iter()
        .position(|l| l.slug == slug)
        .unwrap()
}

#[test]
fn stroke_is_one_undo_entry_and_interpolates() {
    let mut r = Rig::new();
    let before = r.doc.project.clone();
    // A fast stroke: three pointer samples, far apart.
    r.gesture(
        r#"{ "gesture": "brush", "level": "crevasse", "layer": "collision",
             "path": [[2, 1], [7, 1], [7, 2]], "brush": "water" }"#,
    );
    let rows = r.rows("crevasse", "collision");
    assert_eq!(rows[1], "#.~~~~~~...........#");
    assert_eq!(rows[2], "#......~oooo.......#");
    assert_eq!(r.ed.history().undo_entries().len(), 1);
    assert_eq!(r.ed.undo_label().as_deref(), Some("Paint collision"));
    assert!(r.ed.has_unsaved());
    r.undo();
    assert_eq!(r.doc.project, before);
    assert!(!r.ed.can_undo() && r.ed.can_redo());
    r.redo();
    assert_eq!(r.rows("crevasse", "collision")[1], "#.~~~~~~...........#");
}

#[test]
fn undo_invalidates_what_the_edit_did() {
    let mut r = Rig::new();
    let ix = r.doc.level_ix("crevasse").unwrap();
    let fresh = |doc: &Document| {
        worlds_rules::bake_level(&doc.project, doc.level(ix), &doc.images).unwrap()
    };
    let original_bake = r.doc.bakes[ix].clone().unwrap();
    r.gesture(
        r#"{ "gesture": "rect", "level": "crevasse", "layer": "collision",
             "path": [[5, 1], [9, 2]], "brush": "solid" }"#,
    );
    let edit = r.invalidated();
    assert!(r.view.status.is_none(), "{:?}", r.view.status);
    assert_eq!(edit["collision"], [5, 1, 10, 3]);
    // The auto layer reading collision was rebaked around the edit, and the
    // incremental bake is the full bake.
    assert!(edit.contains_key("main_tiling"), "{edit:?}");
    let unshared = |doc: &Document| -> BTreeMap<Slug, worlds_rules::LayerBake> {
        doc.bakes[ix]
            .as_ref()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), (**v).clone()))
            .collect()
    };
    assert_eq!(unshared(&r.doc), fresh(&r.doc));
    assert_ne!(r.doc.bakes[ix].as_ref().unwrap(), &original_bake);

    r.inv.clear();
    r.undo();
    let undo = r.invalidated();
    assert_eq!(undo["collision"], edit["collision"]);
    assert!(undo.contains_key("main_tiling"));
    assert_eq!(r.doc.bakes[ix].as_ref().unwrap(), &original_bake);
}

#[test]
fn escape_cancels_the_gesture() {
    let mut r = Rig::new();
    let before = r.doc.project.clone();
    r.gesture(
        r#"{ "gesture": "brush", "level": "crevasse", "layer": "collision",
             "path": [[2, 1], [10, 1]], "cancel": true }"#,
    );
    assert_eq!(r.doc.project, before);
    assert!(!r.ed.can_undo() && !r.ed.in_gesture());
    // The revert redraws the cells the stroke had painted.
    assert_eq!(r.invalidated()["collision"], [2, 1, 11, 2]);
    // Rect and line only preview until release, so a cancel changes nothing.
    r.inv.clear();
    r.gesture(
        r#"{ "gesture": "rect", "level": "crevasse", "layer": "collision",
             "path": [[2, 1], [10, 2]], "cancel": true }"#,
    );
    assert_eq!(r.doc.project, before);
    assert!(r.inv.is_empty());
}

#[test]
fn rect_outline_line_and_fill() {
    let mut r = Rig::new();
    // Shift: outline only. Dragging past the level's edge is clipped.
    r.gesture(
        r#"{ "gesture": "rect", "level": "crevasse", "layer": "collision",
             "path": [[1, 1], [6, 3]], "shift": true, "brush": "oneway" }"#,
    );
    let rows = r.rows("crevasse", "collision");
    assert_eq!(&rows[1][..8], "#oooooo.");
    assert_eq!(&rows[2][..8], "#o....o.");
    assert_eq!(&rows[3][..8], "#oooooo.");
    r.gesture(
        r#"{ "gesture": "line", "level": "crevasse", "layer": "collision",
             "path": [[10, 1], [14, 3], [30, 3]] }"#,
    );
    let rows = r.rows("crevasse", "collision");
    // Bresenham from (10, 1) to (30, 3): five cells on row 1, then row 2
    // from x = 15, clipped at the right wall (x = 19 is inside the level).
    assert_eq!(&rows[1][10..], "ooooo....#");
    assert_eq!(&rows[2][10..], "oo...ooooo");
    // Flood fill the open air with water. It's 4-connected, so the inside
    // of the outline stays dry.
    r.gesture(
        r#"{ "gesture": "fill", "level": "crevasse", "layer": "collision",
             "path": [[8, 1]], "brush": "water" }"#,
    );
    let rows = r.rows("crevasse", "collision");
    // The pocket at x 15..18 is sealed off by the line.
    assert_eq!(rows[1], "#oooooo~~~ooooo....#");
    assert_eq!(&rows[2][..8], "#o....o~");
    assert_eq!(r.ed.history().undo_entries().len(), 3);
    assert_eq!(r.ed.undo_label().as_deref(), Some("Fill collision"));
}

#[test]
fn tile_stamps_repeat_on_their_stride() {
    let mut r = Rig::new();
    // decor_back has 8 px cells and castle_main 16 px tiles: each tile
    // covers 2x2 cells, a 2x1 stamp 4x2. A stroke from x=0 to x=9 on row 6
    // lays stamps at x = 0, 4, 8 (the last one clipped at the level edge,
    // which is 40 cells wide).
    r.gesture(
        r#"{ "gesture": "brush", "level": "crevasse", "layer": "decor_back",
             "path": [[0, 6], [9, 6]],
             "brush": { "tileset": "castle_main", "rect": [2, 3, 2, 1] } }"#,
    );
    let t = r
        .doc
        .project
        .level("crevasse")
        .unwrap()
        .tiles_layer("decor_back")
        .unwrap();
    let placed: Vec<([u32; 2], [u32; 2])> = t
        .tiles
        .iter()
        .filter(|p| p.cell[1] == 6)
        .map(|p| (p.cell, p.tile))
        .collect();
    assert_eq!(
        placed,
        [
            ([0, 6], [2, 3]),
            ([2, 6], [3, 3]),
            ([4, 6], [2, 3]),
            ([6, 6], [3, 3]),
            ([8, 6], [2, 3]),
            ([10, 6], [3, 3]),
        ]
    );
    assert_eq!(r.ed.history().undo_entries().len(), 1);
    // Eraser on a tiles layer clears stacks.
    r.gesture(r#"{ "gesture": "eraser", "level": "crevasse", "layer": "decor_back", "path": [[2, 6], [4, 6]] }"#);
    let t = r
        .doc
        .project
        .level("crevasse")
        .unwrap()
        .tiles_layer("decor_back")
        .unwrap();
    assert!(!t.tiles.iter().any(|p| p.cell == [2, 6] || p.cell == [4, 6]));
    // Alt-click samples the tile back into the palette.
    r.gesture(r#"{ "gesture": "brush", "level": "crevasse", "layer": "decor_back", "path": [[5, 2]], "alt": true }"#);
    assert_eq!(
        r.view.palette.brush("decor_back"),
        Some(&Brush::Tiles {
            tileset: Slug::new("castle_main").unwrap(),
            rect: [6, 1, 1, 1]
        })
    );
    assert_eq!(r.ed.history().undo_entries().len(), 2);
}

#[test]
fn locked_and_hidden_layers_refuse_paint() {
    let mut r = Rig::new();
    let before = r.doc.project.clone();
    let l = layer(&r.doc, "collision");
    r.view.layers[l].locked = true;
    r.gesture(
        r#"{ "gesture": "brush", "level": "crevasse", "layer": "collision", "path": [[2, 1]] }"#,
    );
    assert!(r.view.status.as_ref().unwrap().text.contains("locked"));
    r.view.layers[l].locked = false;
    r.view.layers[l].visible = false;
    r.gesture(
        r#"{ "gesture": "brush", "level": "crevasse", "layer": "collision", "path": [[2, 1]] }"#,
    );
    assert!(r.view.status.as_ref().unwrap().text.contains("hidden"));
    // Auto layers can't be painted.
    r.gesture(
        r#"{ "gesture": "brush", "level": "crevasse", "layer": "main_tiling", "path": [[2, 1]] }"#,
    );
    assert!(r.view.status.as_ref().unwrap().text.contains("auto layer"));
    assert_eq!(r.doc.project, before);
    assert!(!r.ed.can_undo());
}

#[test]
fn select_moves_and_resizes_levels() {
    let mut r = Rig::new();
    let ix = r.doc.level_ix("shaft").unwrap();
    assert_eq!(r.doc.level(ix).world_px, [320, 80]);
    // Drag the level by 3 cells right, 1 down, in several moves: one entry.
    r.gesture(r#"{ "gesture": "select", "level": "shaft", "layer": "collision", "path": [[1, 1], [2, 1], [4, 2]] }"#);
    assert_eq!(r.doc.level(ix).world_px, [368, 96]);
    assert_eq!(r.ed.history().undo_entries().len(), 1);
    assert_eq!(r.ed.undo_label().as_deref(), Some("Move level shaft"));
    assert!(r.inv.contains(&Invalidation::Level(ix)));
    // Drag the left edge 2 cells left: wider, the right edge stays.
    r.gesture(
        r#"{ "gesture": "select", "level": "shaft", "layer": "collision", "path": [[0, 3], [-2, 3]], "handle": [-1, 0] }"#,
    );
    let l = r.doc.level(ix);
    assert_eq!((l.world_px, l.size), ([336, 96], [8, 8]));
    assert_eq!(r.ed.undo_label().as_deref(), Some("Resize level shaft"));
    r.undo();
    r.undo();
    let l = r.doc.level(ix);
    assert_eq!((l.world_px, l.size), ([320, 80], [6, 8]));
}

#[test]
fn a_resize_that_would_strand_an_entity_is_refused_and_crop_fits_the_content() {
    let mut r = Rig::new();
    let ix = r.doc.level_ix("shaft").unwrap();
    let before = r.doc.project.clone();
    // shaft's door stands on [2, 7]: pulling the bottom edge up 2 cells
    // would leave it outside.
    r.gesture(
        r#"{ "gesture": "select", "level": "shaft", "layer": "collision", "path": [[3, 8], [3, 6]], "handle": [0, 1] }"#,
    );
    let status = r.view.status.as_ref().unwrap().text.clone();
    assert!(
        status.contains("Not resized") && status.contains("doorshaft2"),
        "{status}"
    );
    assert_eq!(r.doc.project, before);
    assert!(!r.ed.can_undo());
    // The right edge in by 3 keeps it inside.
    r.gesture(
        r#"{ "gesture": "select", "level": "shaft", "layer": "collision", "path": [[6, 3], [3, 3]], "handle": [1, 0] }"#,
    );
    assert_eq!(r.doc.level(ix).size, [3, 8]);
    // Crop to content: one entry down to the door's 1x3.
    let world = Slug::new("main").unwrap();
    let shaft = Slug::new("shaft").unwrap();
    let crop = worlds_editor::tools::crop_command(&r.doc.project, &world, &shaft).unwrap();
    r.apply_cmd(crop);
    let l = r.doc.level(ix);
    assert_eq!((l.size, l.world_px), ([1, 3], [352, 160]));
    assert_eq!(r.ed.undo_label().as_deref(), Some("Crop level shaft"));
    assert!(r.inv.contains(&Invalidation::Level(ix)));
    let again = worlds_editor::tools::crop_command(&r.doc.project, &world, &shaft).unwrap_err();
    assert!(again.contains("already fits"), "{again}");
    r.undo();
    assert_eq!(r.doc.level(ix).size, [3, 8]);
    // The Inspector's path: a resize with an anchor that strands nothing.
    let cmd = worlds_editor::tools::resize_command(&r.doc.project, &world, &shaft, [5, 10], [1, 1])
        .unwrap();
    r.apply_cmd(cmd);
    let l = r.doc.level(ix);
    assert_eq!((l.size, l.world_px), ([5, 10], [288, 48]));
    assert_eq!(l.all_entities().next().unwrap().cell, [4, 9]);
    let e = worlds_editor::tools::resize_command(&r.doc.project, &world, &shaft, [2, 2], [-1, -1])
        .unwrap_err();
    assert!(e.contains("doorshaft2"), "{e}");
}

#[test]
fn level_drag_previews_and_commits_one_move() {
    let mut r = Rig::new();
    let ix = r.doc.level_ix("shaft").unwrap();
    let g: GestureStep = serde_json::from_str(
        r#"{ "gesture": "select", "level": "shaft", "layer": "collision",
             "path": [[1, 1], [2, 1], [3, 2], [4, 2]] }"#,
    )
    .unwrap();
    let plan = gesture_events(&r.doc, &g, None, None).unwrap();
    r.view.active_level = Some(plan.level);
    r.view.active_layer = Some(plan.layer);
    let (last, moves) = plan.events.split_last().unwrap();
    // While dragging, nothing is applied and nothing is invalidated.
    r.events(moves);
    assert_eq!(r.doc.level(ix).world_px, [320, 80]);
    assert!(r.inv.is_empty(), "{:?}", r.inv);
    assert!(!r.doc.project.is_dirty());
    // Release: one move_level, one undo entry that is a single command.
    r.events(std::slice::from_ref(last));
    assert_eq!(r.doc.level(ix).world_px, [368, 96]);
    let entries = r.ed.history().undo_entries();
    assert_eq!(entries.len(), 1);
    assert!(
        matches!(
            entries[0].command,
            worlds_core::Command::MoveLevel {
                world_px: [320, 80],
                ..
            }
        ),
        "{:?}",
        entries[0].command
    );
    assert_eq!(
        r.inv
            .iter()
            .filter(|i| **i == Invalidation::Level(ix))
            .count(),
        1
    );
}

#[test]
fn save_writes_only_touched_files() {
    let mut r = Rig::new();
    let dir = r.doc.dir.clone();
    let before = files(&dir);
    r.gesture(r#"{ "gesture": "brush", "level": "crevasse", "layer": "collision", "path": [[3, 1]], "brush": "solid" }"#);
    r.save();
    assert!(!r.ed.has_unsaved() && !r.doc.project.is_dirty());
    let after = files(&dir);
    let changed: Vec<&PathBuf> = after
        .iter()
        .filter(|(p, v)| before.get(*p) != Some(v))
        .map(|(p, _)| p)
        .collect();
    assert_eq!(
        changed,
        [&PathBuf::from("worlds/main/levels/crevasse/collision.json")]
    );
    // Undo is still possible after a save, and dirties the file again.
    r.undo();
    assert!(r.ed.has_unsaved());
    // The saved files load back to what is in memory before the undo.
    r.redo();
    r.save();
    assert_eq!(worlds_core::load(&dir).unwrap(), r.doc.project);
    // Also after edits that create layer content a level didn't have (shaft
    // has no collision grid or tiles) and a level made from scratch. The
    // file watcher relies on this to ignore our own saves.
    r.gesture(r#"{ "gesture": "rect", "level": "shaft", "layer": "collision", "path": [[0, 0], [2, 2]], "brush": "fortified" }"#);
    r.gesture(
        r#"{ "gesture": "brush", "level": "shaft", "layer": "decor_back", "path": [[1, 1]],
             "brush": { "tileset": "castle_main", "rect": [0, 0, 1, 1] } }"#,
    );
    let mut ed = std::mem::take(&mut r.ed);
    let mut level = worlds_core::Level::new(Slug::new("annex").unwrap(), "Annex", [4, 4]);
    level.world_px = [-200, 0];
    ed.apply(
        &mut r.cx(),
        worlds_core::Command::CreateLevel {
            world: Slug::new("main").unwrap(),
            level,
            content: Default::default(),
            restore: false,
        },
    )
    .unwrap();
    r.ed = ed;
    r.gesture(r#"{ "gesture": "line", "level": "annex", "layer": "collision", "path": [[0, 3], [3, 0]], "brush": "solid" }"#);
    r.save();
    assert_eq!(worlds_core::load(&dir).unwrap(), r.doc.project);
}

#[test]
fn own_save_then_undo_is_not_an_external_change() {
    // The watcher fires ~100 ms after a save. If the user undoes in that
    // window, memory no longer matches the files, but the files still hold
    // exactly what we wrote: no reload prompt.
    let mut r = Rig::new();
    let dir = r.doc.dir.clone();
    r.gesture(r#"{ "gesture": "rect", "level": "crevasse", "layer": "collision", "path": [[5, 1], [9, 2]], "brush": "solid" }"#);
    let mut level = worlds_core::Level::new(Slug::new("annex").unwrap(), "Annex", [4, 4]);
    level.world_px = [-200, 0];
    let mut ed = std::mem::take(&mut r.ed);
    ed.apply(
        &mut r.cx(),
        worlds_core::Command::CreateLevel {
            world: Slug::new("main").unwrap(),
            level,
            content: Default::default(),
            restore: false,
        },
    )
    .unwrap();
    r.ed = ed;
    let dirty: Vec<PathBuf> = r.doc.project.files.dirty().iter().cloned().collect();
    r.save();
    let mut own = OwnWrites::default();
    own.record(&dir, &dirty);
    r.undo();
    r.undo();
    assert!(r.ed.has_unsaved());
    assert_ne!(worlds_core::load(&dir).unwrap(), r.doc.project);
    // What the watcher would report: every file the save touched.
    let changed: Vec<PathBuf> = vec![
        dir.join("worlds/main/levels/crevasse/collision.json"),
        dir.join("worlds/main/levels/annex/level.json"),
    ];
    assert!(changed.iter().all(|p| own.explains(p)));
    // An external edit to one of them is not ours.
    std::fs::write(&changed[0], "{}").unwrap();
    assert!(!own.explains(&changed[0]));
}

#[test]
fn tileset_hot_reload_rebakes_and_drops_stale_cache() {
    let mut r = Rig::new();
    let ix = r.doc.level_ix("crevasse").unwrap();
    let fresh = |doc: &Document| {
        worlds_rules::bake_level(&doc.project, doc.level(ix), &doc.images).unwrap()
    };
    // Warm the cache with an edit, so it holds bakes made from the old image.
    r.gesture(r#"{ "gesture": "rect", "level": "crevasse", "layer": "collision", "path": [[5, 1], [9, 2]], "brush": "solid" }"#);
    assert!(r.doc.bake_owned[ix]);
    let before = r.doc.bakes[ix].clone().unwrap();
    // The art changes on disk: every other tile column goes transparent,
    // which changes which tiles are opaque (cell locks) and so the bake.
    let ts = r
        .doc
        .project
        .tilesets
        .iter()
        .position(|t| t.slug == "castle_main")
        .unwrap();
    let png = r.doc.dir.join(&r.doc.project.tilesets[ts].image);
    let img = image::RgbaImage::from_fn(256, 256, |x, _| {
        image::Rgba([90, 80, 70, if (x / 16) % 2 == 0 { 255 } else { 0 }])
    });
    img.save(&png).unwrap();
    // What the app does on the watcher's Tileset event.
    let layers = r.doc.tileset_changed(ts);
    assert!(layers.iter().any(|l| l == "main_tiling"), "{layers:?}");
    r.ed.tileset_changed(&layers);
    assert!(!r.doc.bake_owned[ix], "the next worker bake must land");
    assert!(r.doc.stale_bakes[ix].contains("main_tiling"));
    // An edit before the rebake lands must not start from the stale bake
    // still on screen (it isn't handed to the cache).
    r.gesture(r#"{ "gesture": "rect", "level": "crevasse", "layer": "collision", "path": [[16, 1], [17, 1]], "brush": "solid" }"#);
    assert_eq!(unshare(r.doc.bakes[ix].as_ref().unwrap()), fresh(&r.doc));
    assert!(!r.doc.stale_bakes[ix].contains("main_tiling"));
    r.doc.bake_all_blocking();
    let after = fresh(&r.doc);
    assert_ne!(unshare(&before), after, "the new image changes the bake");
    assert_eq!(unshare(r.doc.bakes[ix].as_ref().unwrap()), after);
    // The next edit's incremental rebake starts from the new image, not
    // from the stale cached bake: it equals a full bake.
    r.gesture(r#"{ "gesture": "rect", "level": "crevasse", "layer": "collision", "path": [[12, 1], [14, 2]], "brush": "solid" }"#);
    assert_eq!(unshare(r.doc.bakes[ix].as_ref().unwrap()), fresh(&r.doc));
}

/// `set_rule` on `main_tiling`'s `wall_fill`, drawing `tile` instead.
fn set_wall_fill(tile: [u32; 2]) -> serde_json::Value {
    serde_json::json!({ "op": "set_rule", "layer": "main_tiling", "group": "walls", "rule": "wall_fill",
        "def": { "slug": "wall_fill", "match": { "rows": ["+"] },
                 "out": { "tileset": "castle_main", "tiles": [tile] }, "out_of_bounds": "solid" } })
}

/// A rig with walls in shaft too (it has no collision file in the fixture)
/// and a second auto layer, `glow`, reading the same grid.
fn rule_rig() -> Rig {
    let mut r = Rig::new();
    r.gesture(r#"{ "gesture": "rect", "level": "shaft", "layer": "collision", "path": [[1, 1], [4, 3]], "brush": "solid" }"#);
    r.apply(serde_json::json!({ "op": "set_layer_def", "layer": "glow",
        "def": { "slug": "glow", "name": "Glow", "kind": "auto", "source": "collision" } }));
    r.apply(
        serde_json::json!({ "op": "set_rule_group", "layer": "glow", "group": "g",
        "def": { "slug": "g", "rules": [ { "slug": "lit", "match": { "rows": ["#"] },
            "out": { "tileset": "castle_main", "tiles": [[3, 3]] } } ] } }),
    );
    r.doc.finish_layer_bakes();
    r.view.active_level = r.doc.level_ix("crevasse");
    r.inv.clear();
    r
}

/// Only Layer/Cells invalidations of `layer`, and a Layer one per level.
fn assert_only_layer_invalidated(r: &Rig, layer: &str) {
    for i in &r.inv {
        match i {
            Invalidation::Layer { layer: l, .. } | Invalidation::Cells { layer: l, .. } => {
                assert_eq!(l.as_str(), layer, "{:?}", r.inv)
            }
            other => panic!("unexpected {other:?} in {:?}", r.inv),
        }
    }
    for ix in 0..r.doc.levels.len() {
        assert!(
            r.inv.contains(&Invalidation::Layer {
                level: ix,
                layer: Slug::new(layer).unwrap()
            }),
            "level {ix}: {:?}",
            r.inv
        );
    }
}

#[test]
fn rule_edit_rebakes_only_that_layer_active_level_at_once() {
    let mut r = rule_rig();
    let (crev, shaft) = (
        r.doc.level_ix("crevasse").unwrap(),
        r.doc.level_ix("shaft").unwrap(),
    );
    let glow = |r: &Rig, ix: usize| Arc::as_ptr(&r.doc.bakes[ix].as_ref().unwrap()["glow"]);
    let glow_before = [glow(&r, crev), glow(&r, shaft)];
    let before = [
        r.shown("crevasse", "main_tiling"),
        r.shown("shaft", "main_tiling"),
    ];
    for step in ["edit", "undo", "redo"] {
        r.inv.clear();
        match step {
            "edit" => r.apply(set_wall_fill([2, 2])),
            "undo" => r.undo(),
            _ => r.redo(),
        }
        assert_only_layer_invalidated(&r, "main_tiling");
        // The active level is rebaked in the same `after`.
        assert_eq!(
            r.shown("crevasse", "main_tiling"),
            r.fresh("crevasse", "main_tiling")
        );
        assert!(r.doc.stale_bakes[crev].is_empty());
        assert_eq!(
            r.ed.cache().get("crevasse", "main_tiling"),
            Some(&r.fresh("crevasse", "main_tiling"))
        );
        // The other level keeps its old bake on screen until its job lands.
        assert!(r.doc.stale_bakes[shaft].contains("main_tiling"), "{step}");
        assert!(r.doc.layer_jobs_pending > 0);
        assert_eq!(r.ed.cache().get("shaft", "main_tiling"), None);
        let arrived = r.doc.finish_layer_bakes();
        assert_eq!(arrived, vec![(shaft, Slug::new("main_tiling").unwrap())]);
        r.ed.adopt_layer_bakes(&r.doc, &arrived);
        assert_eq!(
            r.shown("shaft", "main_tiling"),
            r.fresh("shaft", "main_tiling")
        );
        assert!(r.doc.stale_bakes[shaft].is_empty());
        // The owned level's cache took the worker's bake as is.
        assert!(r.doc.bake_owned[shaft]);
        assert!(Arc::ptr_eq(
            r.ed.cache().get_shared("shaft", "main_tiling").unwrap(),
            &r.doc.bakes[shaft].as_ref().unwrap()["main_tiling"]
        ));
        // The other auto layer was not touched in any level.
        assert_eq!([glow(&r, crev), glow(&r, shaft)], glow_before, "{step}");
        if step == "undo" {
            assert_eq!(r.shown("crevasse", "main_tiling"), before[0]);
            assert_eq!(r.shown("shaft", "main_tiling"), before[1]);
        } else {
            assert_ne!(
                r.shown("crevasse", "main_tiling"),
                before[0],
                "the edit shows"
            );
        }
    }
}

#[test]
fn paint_during_a_layer_job_leaves_no_half_baked_layer() {
    let mut r = rule_rig();
    let shaft = r.doc.level_ix("shaft").unwrap();
    // The rule edit sends shaft's main_tiling to a worker, which bakes the
    // project as it is now...
    r.apply(set_wall_fill([2, 2]));
    assert!(r.doc.stale_bakes[shaft].contains("main_tiling"));
    // ...then the user paints in shaft before the job is back: the command
    // layer bakes the layer itself (the cache has nothing for it).
    r.gesture(r#"{ "gesture": "rect", "level": "shaft", "layer": "collision", "path": [[5, 4], [5, 6]], "brush": "solid" }"#);
    assert!(r.view.status.is_none(), "{:?}", r.view.status);
    assert_eq!(r.rows("shaft", "collision")[5], ".....#");
    assert_eq!(
        r.shown("shaft", "main_tiling"),
        r.fresh("shaft", "main_tiling")
    );
    let dropped = r.doc.dropped_results;
    // The job's result (made before the paint) arrives and is dropped.
    let arrived = r.doc.finish_layer_bakes();
    assert!(arrived.is_empty(), "{arrived:?}");
    assert_eq!(r.doc.dropped_results, dropped + 1);
    r.ed.adopt_layer_bakes(&r.doc, &arrived);
    // Document and cache both hold the bake of the painted grid...
    assert_eq!(
        r.shown("shaft", "main_tiling"),
        r.fresh("shaft", "main_tiling")
    );
    assert_eq!(
        r.ed.cache().get("shaft", "main_tiling"),
        Some(&r.fresh("shaft", "main_tiling"))
    );
    // ...and the next stroke's incremental re-sweep starts from it.
    r.gesture(r#"{ "gesture": "rect", "level": "shaft", "layer": "collision", "path": [[1, 6], [3, 6]], "brush": "solid" }"#);
    assert_eq!(r.rows("shaft", "collision")[6], ".###.#");
    assert_eq!(
        r.shown("shaft", "main_tiling"),
        r.fresh("shaft", "main_tiling")
    );
    assert_eq!(
        r.ed.cache().get("shaft", "main_tiling"),
        Some(&r.fresh("shaft", "main_tiling"))
    );
}

#[test]
fn results_of_an_older_rule_edit_are_dropped() {
    let mut r = rule_rig();
    let shaft = r.doc.level_ix("shaft").unwrap();
    let dropped = r.doc.dropped_results;
    // Two edits before any job is back: the first edit's job is obsolete.
    r.apply(set_wall_fill([2, 2]));
    r.apply(set_wall_fill([6, 6]));
    let arrived = r.doc.finish_layer_bakes();
    assert_eq!(arrived, vec![(shaft, Slug::new("main_tiling").unwrap())]);
    assert_eq!(r.doc.dropped_results, dropped + 1);
    assert_eq!(
        r.shown("shaft", "main_tiling"),
        r.fresh("shaft", "main_tiling")
    );
    // Nothing is left stale.
    assert!(r.doc.stale_bakes.iter().all(|s| s.is_empty()));
}

#[test]
fn macro_edits_rebake_the_layers_using_macros() {
    let mut r = rule_rig();
    // Make main_tiling use a named pattern from macros.json.
    r.apply(
        serde_json::json!({ "op": "set_macro", "kind": "pattern", "name": "solo", "def": ["+"] }),
    );
    r.apply(serde_json::json!({ "op": "set_rule", "layer": "main_tiling", "group": "walls", "rule": "wall_fill",
        "def": { "slug": "wall_fill", "match": { "rows": "solo" },
                 "out": { "tileset": "castle_main", "tiles": [[2, 2]] }, "out_of_bounds": "solid" } }));
    r.doc.finish_layer_bakes();
    r.inv.clear();
    r.apply(
        serde_json::json!({ "op": "set_macro", "kind": "pattern", "name": "solo", "def": ["#"] }),
    );
    assert_only_layer_invalidated(&r, "main_tiling");
    assert_eq!(
        r.shown("crevasse", "main_tiling"),
        r.fresh("crevasse", "main_tiling")
    );
    r.doc.finish_layer_bakes();
    assert_eq!(
        r.shown("shaft", "main_tiling"),
        r.fresh("shaft", "main_tiling")
    );
}

#[test]
fn tileset_def_change_reuploads_and_rebakes_its_layers() {
    let mut r = rule_rig();
    let mut def = r.doc.project.tileset("castle_main").unwrap().clone();
    def.spacing = 1;
    let gen0 = r.doc.images_gen;
    r.apply(serde_json::json!({ "op": "set_tileset_def", "tileset": "castle_main", "def": def }));
    // Textures go up again (the scene is rebuilt with them)...
    assert!(r.inv.contains(&Invalidation::Tilesets));
    assert_ne!(r.doc.images_gen, gen0);
    // ...and both auto layers draw from castle_main: rebaked, nothing else.
    assert!(!r.inv.contains(&Invalidation::All));
    for l in ["main_tiling", "glow"] {
        assert_eq!(r.shown("crevasse", l), r.fresh("crevasse", l));
    }
    r.doc.finish_layer_bakes();
    for l in ["main_tiling", "glow"] {
        assert_eq!(r.shown("shaft", l), r.fresh("shaft", l));
    }
}

#[test]
fn export_after_a_rule_edit_never_writes_a_stale_layer() {
    let mut r = rule_rig();
    r.doc.project.export_on_save = true;
    r.apply(set_wall_fill([2, 2]));
    let shaft = r.doc.level_ix("shaft").unwrap();
    assert!(r.doc.stale_bakes[shaft].contains("main_tiling"));
    // Saved before shaft's job is back: shaft is baked for the export.
    r.save();
    let dir = r.doc.dir.clone();
    let ours = std::fs::read_to_string(dir.join("export/levels/shaft.json")).unwrap();
    let p = worlds_core::load(&dir).unwrap();
    let images = worlds_rules::TilesetImages::new(&dir, &p.tilesets);
    let fresh = tempfile::tempdir().unwrap();
    let baked = p
        .levels()
        .map(|l| {
            (
                l.slug.clone(),
                worlds_rules::bake_for_export(&p, l, &images).unwrap(),
            )
        })
        .collect();
    worlds_core::export::write(&p, &dir, fresh.path(), &baked, true).unwrap();
    let theirs = std::fs::read_to_string(fresh.path().join("levels/shaft.json")).unwrap();
    let strip = |s: &str| {
        s.lines()
            .filter(|l| !l.contains("\"image\""))
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_eq!(strip(&ours), strip(&theirs));
}

#[test]
fn detach_and_save_as_template_keep_the_bake() {
    use worlds_editor::app::rules::{detach_commands, save_template_commands};
    let mut r = rule_rig();
    let layer = Slug::new("main_tiling").unwrap();
    r.apply(
        serde_json::json!({ "op": "set_rule", "layer": "main_tiling", "group": "walls",
        "rule": "moss", "index": 0,
        "def": { "use": "blob47", "slug": "moss", "tileset": "castle_main", "origin": [0, 0] } }),
    );
    r.doc.finish_layer_bakes();
    let bakes = |r: &Rig| {
        (
            r.fresh("crevasse", "main_tiling"),
            r.fresh("shaft", "main_tiling"),
        )
    };
    let before = bakes(&r);
    assert!(
        before
            .0
            .fired
            .values()
            .flatten()
            .any(|s| s.as_str().starts_with("moss__")),
        "the reference fires"
    );
    let entries = r.ed.history().undo_entries().len();
    // Detach: the reference becomes its rules, same slugs, same bake.
    let cmds = detach_commands(&r.doc.project, &layer, &Slug::new("moss").unwrap()).unwrap();
    r.apply_cmd(worlds_core::Command::Batch {
        label: "Detach moss".into(),
        commands: cmds,
    });
    assert_eq!(r.ed.history().undo_entries().len(), entries + 1);
    let set = r.doc.project.rules_for("main_tiling").unwrap();
    assert!(set.entries().all(|(_, e)| e.as_use().is_none()));
    let detached: Vec<Slug> = set
        .entries()
        .filter(|(_, e)| e.slug().as_str().starts_with("moss__"))
        .map(|(_, e)| e.slug().clone())
        .collect();
    assert!(detached.len() > 10, "{detached:?}");
    assert_eq!(bakes(&r), before);
    assert_eq!(r.shown("crevasse", "main_tiling"), before.0);
    // Save them as a template, replaced by a reference named like their
    // prefix: the template's rules lose `moss__`, the reference expands to
    // the same slugs again, and nothing re-rolls.
    let (label, cmds) = save_template_commands(
        &r.doc.project,
        &layer,
        &detached,
        "moss_t",
        "moss",
        [0, 0],
        true,
    )
    .unwrap();
    r.apply_cmd(worlds_core::Command::Batch {
        label,
        commands: cmds,
    });
    let set = r.doc.project.rules_for("main_tiling").unwrap();
    let u = set
        .entries()
        .find_map(|(_, e)| e.as_use())
        .expect("a reference");
    assert_eq!((u.slug().as_str(), u.template.as_str()), ("moss", "moss_t"));
    let t = r.doc.project.template("moss_t").unwrap();
    assert!(t.rules.iter().all(|x| !x.slug.as_str().contains("__")));
    assert_eq!(bakes(&r), before);
    r.doc.finish_layer_bakes();
    assert_eq!(r.shown("shaft", "main_tiling"), before.1);
    // Two undo entries back to the blob47 reference.
    r.undo();
    r.undo();
    let set = r.doc.project.rules_for("main_tiling").unwrap();
    assert_eq!(
        set.entries()
            .find_map(|(_, e)| e.as_use())
            .map(|u| u.template.as_str()),
        Some("blob47")
    );
    assert!(r.doc.project.template("moss_t").is_none());
}

/// A level's bakes as plain values, to compare with a fresh bake.
fn unshare(b: &worlds_editor::doc::LevelBakes) -> BTreeMap<Slug, worlds_rules::LayerBake> {
    b.iter().map(|(k, v)| (k.clone(), (**v).clone())).collect()
}

/// The snapshot renderer keys its uploaded textures on `images_gen`: it
/// changes on a tileset hot reload, on a tileset definition change (which
/// also asks the app to upload textures again), and is new for every
/// opened document, whatever address its images land at.
#[test]
fn image_generation_follows_the_images() {
    let mut r = Rig::new();
    let g0 = r.doc.images_gen;
    r.doc.tileset_changed(0);
    let g1 = r.doc.images_gen;
    assert_ne!(g1, g0);
    let reopened = Document::open(&r.doc.dir).unwrap();
    assert!(reopened.images_gen != g0 && reopened.images_gen != g1);
    drop(reopened);
    let mut def = r.doc.project.tilesets[0].clone();
    def.image = "art/other.png".into();
    let cmd = worlds_core::Command::SetTilesetDef {
        tileset: def.slug.clone(),
        def,
        index: None,
    };
    let mut ed = std::mem::take(&mut r.ed);
    ed.apply(&mut r.cx(), cmd).unwrap();
    r.ed = ed;
    assert_ne!(r.doc.images_gen, g1);
    assert!(r.inv.contains(&Invalidation::Tilesets));
}

#[test]
fn level_operations_reindex_and_undo() {
    let mut r = Rig::new();
    let n = r.doc.levels.len();
    let shaft = r.doc.level_ix("shaft").unwrap();
    r.view.active_level = Some(shaft);
    let mut level = worlds_core::Level::new(Slug::new("annex").unwrap(), "Annex", [5, 3]);
    level.world_px = [-160, 0];
    let mut ed = std::mem::take(&mut r.ed);
    ed.apply(
        &mut r.cx(),
        worlds_core::Command::CreateLevel {
            world: Slug::new("main").unwrap(),
            level,
            content: Default::default(),
            restore: false,
        },
    )
    .unwrap();
    assert_eq!(r.doc.levels.len(), n + 1);
    assert!(r.inv.contains(&Invalidation::All));
    // The active level follows its id through the re-index.
    assert_eq!(
        r.doc.level(r.view.active_level.unwrap()).slug.as_str(),
        "shaft"
    );
    // A new level gets a bake.
    let annex = r.doc.level_ix("annex").unwrap();
    assert!(r.doc.bakes[annex].is_some());
    ed.apply(
        &mut r.cx(),
        worlds_core::Command::RenameLevel {
            world: Slug::new("main").unwrap(),
            level: Slug::new("shaft").unwrap(),
            slug: Slug::new("well").unwrap(),
            name: "Well".into(),
        },
    )
    .unwrap();
    assert_eq!(
        r.doc.level(r.view.active_level.unwrap()).slug.as_str(),
        "well"
    );
    ed.apply(
        &mut r.cx(),
        worlds_core::Command::DeleteLevel {
            world: Slug::new("main").unwrap(),
            level: Slug::new("well").unwrap(),
        },
    )
    .unwrap();
    assert_eq!(r.view.active_level, None);
    assert_eq!(r.doc.levels.len(), n);
    r.ed = ed;
    r.undo();
    r.undo();
    r.undo();
    assert_eq!(r.doc.levels.len(), n);
    assert!(r.doc.level_ix("shaft").is_some() && r.doc.level_ix("annex").is_none());
}

#[test]
fn begin_carries_the_grab() {
    // The select tool's Begin says what it grabbed; painting tools grab nothing.
    let r = Rig::new();
    let ix = r.doc.level_ix("crevasse").unwrap();
    let g: GestureStep = serde_json::from_str(
        r#"{ "gesture": "select", "level": "crevasse", "path": [[0, 0]], "handle": [1, 1] }"#,
    )
    .unwrap();
    let ev = gesture_events(&r.doc, &g, Some(0), None).unwrap().events;
    assert!(matches!(
        ev[0],
        ToolEvent::Begin {
            tool: Tool::Select,
            grab: Grab::Handle([1, 1]),
            ..
        }
    ));
    assert_eq!(cell_hit(&r.doc, ix, 0, [0, 0]).world_px, [8.0, 8.0]);
}

#[test]
fn export_on_save_uses_the_cached_bakes() {
    let mut r = Rig::new();
    r.doc.project.export_on_save = true;
    r.gesture(r#"{ "gesture": "rect", "level": "crevasse", "layer": "collision", "path": [[5, 1], [9, 2]], "brush": "solid" }"#);
    r.save();
    let dir = r.doc.dir.clone();
    let ours = std::fs::read_to_string(dir.join("export/levels/crevasse.json")).unwrap();
    // A full export from a fresh bake of the saved project gives the same bytes.
    let fresh = tempfile::tempdir().unwrap();
    let p = worlds_core::load(&dir).unwrap();
    let images = worlds_rules::TilesetImages::new(&dir, &p.tilesets);
    let baked = p
        .levels()
        .map(|l| {
            (
                l.slug.clone(),
                worlds_rules::bake_for_export(&p, l, &images).unwrap(),
            )
        })
        .collect();
    worlds_core::export::write(&p, &dir, fresh.path(), &baked, true).unwrap();
    let theirs = std::fs::read_to_string(fresh.path().join("levels/crevasse.json")).unwrap();
    // Tileset image paths are relative to the export dir, which differs.
    let strip = |s: &str| {
        s.lines()
            .filter(|l| !l.contains("\"image\""))
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_eq!(strip(&ours), strip(&theirs));
    assert!(dir.join("export/world.json").is_file());
}

#[test]
fn strokes_reuse_the_worker_bake_and_edit_it_in_place() {
    // The first stroke on a level takes the worker's bake into the cache as
    // is (no re-sweep on the UI thread), and every brush move edits that
    // same bake in place: no copy of the layer per move.
    let mut r = Rig::new();
    let ix = r.doc.level_ix("crevasse").unwrap();
    let col = layer(&r.doc, "collision");
    let ptr = |r: &Rig| Arc::as_ptr(&r.doc.bakes[ix].as_ref().unwrap()["main_tiling"]);
    let worker = ptr(&r);
    let m = egui::Modifiers::NONE;
    let hit = |r: &Rig, x: i32| cell_hit(&r.doc, ix, col, [x, 1]);
    let start = hit(&r, 2);
    r.events(&[ToolEvent::Begin {
        tool: Tool::Brush,
        at: start,
        modifiers: m,
        grab: Grab::None,
    }]);
    assert!(r.view.status.is_none(), "{:?}", r.view.status);
    assert!(Arc::ptr_eq(
        r.ed.cache().get_shared("crevasse", "main_tiling").unwrap(),
        &r.doc.bakes[ix].as_ref().unwrap()["main_tiling"]
    ));
    assert_eq!(ptr(&r), worker, "seeded, not re-baked, and not copied");
    for x in 3..9 {
        let at = hit(&r, x);
        r.events(&[ToolEvent::Move {
            tool: Tool::Brush,
            start,
            at,
            modifiers: m,
        }]);
        assert_eq!(ptr(&r), worker, "move to {x} copied the bake");
    }
    let at = hit(&r, 9);
    r.events(&[ToolEvent::End {
        tool: Tool::Brush,
        start,
        at,
        modifiers: m,
    }]);
    assert_eq!(r.rows("crevasse", "collision")[1][2..10], *"########");
    let fresh = worlds_rules::bake_level(&r.doc.project, r.doc.level(ix), &r.doc.images).unwrap();
    assert_eq!(
        *r.doc.bakes[ix].as_ref().unwrap()["main_tiling"],
        fresh["main_tiling"]
    );
}

#[test]
fn script_brushes_by_slug_erase_and_tile() {
    let mut r = Rig::new();
    r.gesture(
        r#"{ "gesture": "rect", "level": "crevasse", "layer": "collision",
             "path": [[2, 1], [5, 1]], "brush": "water" }"#,
    );
    assert_eq!(&r.rows("crevasse", "collision")[1][..7], "#.~~~~.");
    // "erase" paints empty with any painting tool, not only the eraser.
    r.gesture(
        r#"{ "gesture": "rect", "level": "crevasse", "layer": "collision",
             "path": [[3, 1], [4, 1]], "brush": "erase" }"#,
    );
    assert_eq!(&r.rows("crevasse", "collision")[1][..7], "#.~..~.");
    // One tile by [col, row], then erase it with the brush.
    r.gesture(
        r#"{ "gesture": "brush", "level": "crevasse", "layer": "decor_back",
             "path": [[0, 6]], "brush": { "tileset": "castle_main", "tile": [4, 5] } }"#,
    );
    let tiles = |r: &Rig| -> Vec<[u32; 2]> {
        r.doc
            .project
            .level("crevasse")
            .unwrap()
            .tiles_layer("decor_back")
            .map(|t| {
                t.tiles
                    .iter()
                    .filter(|p| p.cell == [0, 6])
                    .map(|p| p.tile)
                    .collect()
            })
            .unwrap_or_default()
    };
    assert_eq!(tiles(&r), [[4, 5]]);
    r.gesture(
        r#"{ "gesture": "brush", "level": "crevasse", "layer": "decor_back",
             "path": [[0, 6]], "brush": "erase" }"#,
    );
    assert!(tiles(&r).is_empty());
    // Unknown slugs and forms that don't fit the layer are errors naming
    // the problem.
    let err = |json: &str| {
        let g: GestureStep = serde_json::from_str(json).unwrap();
        format!("{:#}", gesture_events(&r.doc, &g, None, None).unwrap_err())
    };
    let e = err(
        r#"{ "gesture": "brush", "level": "crevasse", "layer": "collision", "path": [[0, 0]], "brush": "lava" }"#,
    );
    assert!(e.contains("lava") && e.contains("solid"), "{e}");
    let e = err(
        r#"{ "gesture": "brush", "level": "crevasse", "layer": "decor_back", "path": [[0, 0]], "brush": "solid" }"#,
    );
    assert!(e.contains("int_grid"), "{e}");
    let e = err(
        r#"{ "gesture": "brush", "level": "crevasse", "layer": "collision", "path": [[0, 0]],
             "brush": { "tileset": "castle_main", "tile": [0, 0] } }"#,
    );
    assert!(e.contains("tiles layer"), "{e}");
    let e = err(
        r#"{ "gesture": "brush", "level": "crevasse", "layer": "decor_back", "path": [[0, 0]],
             "brush": { "tileset": "nope", "tile": [0, 0] } }"#,
    );
    assert!(e.contains("nope"), "{e}");
}

// ---- the live API hosted by the editor (worlds-server through EditorHost) ----

mod live {
    use std::net::TcpStream;
    use std::time::{Duration, Instant};

    use serde_json::{Value, json};
    use tokio_tungstenite::tungstenite::stream::MaybeTlsStream;
    use tokio_tungstenite::tungstenite::{self, Message, WebSocket};
    use worlds_core::command::{Origin, Rect};
    use worlds_core::{Command, Slug};
    use worlds_editor::app::watch::OwnWrites;
    use worlds_editor::ghost::Ghost;
    use worlds_editor::live::{
        EditorHost, HostEffects, agent_entry_on_top, new_agent_entries, selection_of,
    };
    use worlds_editor::snapshot::SnapshotRenderer;
    use worlds_editor::tools::EditorCommands;
    use worlds_server::{ChangeKind, Handled, Server, ServerConfig, ServerHandle, UserChange};

    use super::{Rig, unshare};

    /// A rig whose project is served by a real `worlds-server` on a socket.
    struct LiveRig {
        rig: Rig,
        server: ServerHandle,
        own: OwnWrites,
        snaps: Option<SnapshotRenderer>,
        fx: HostEffects,
    }

    impl LiveRig {
        fn new() -> Self {
            let rig = Rig::new();
            let server = Server::start(ServerConfig {
                project_dir: rig.doc.dir.clone(),
                project_name: rig.doc.project.name.clone(),
                headless: false,
                wake: None,
            })
            .unwrap();
            LiveRig {
                rig,
                server,
                own: OwnWrites::default(),
                snaps: None,
                fx: HostEffects::default(),
            }
        }

        /// Apply pending batch `id` (the Apply button).
        fn apply_pending(&mut self, id: &str) -> Vec<worlds_core::command::Change> {
            let mut host = EditorHost {
                doc: &mut self.rig.doc,
                editing: &mut self.rig.ed,
                view: &mut self.rig.view,
                invalidate: &mut self.rig.inv,
                own_writes: &mut self.own,
                snapshots: &mut self.snaps,
                effects: &mut self.fx,
                synthetic: false,
                recovered: None,
            };
            self.server.apply_pending(&mut host, id).unwrap()
        }

        /// Drain (as the editor does once per frame) until a request was
        /// handled.
        fn drain(&mut self) -> Vec<Handled> {
            let t = Instant::now();
            loop {
                let mut host = EditorHost {
                    doc: &mut self.rig.doc,
                    editing: &mut self.rig.ed,
                    view: &mut self.rig.view,
                    invalidate: &mut self.rig.inv,
                    own_writes: &mut self.own,
                    snapshots: &mut self.snaps,
                    effects: &mut self.fx,
                    synthetic: false,
                    recovered: None,
                };
                let h = self.server.drain(&mut host);
                if !h.is_empty() {
                    return h;
                }
                assert!(t.elapsed() < Duration::from_secs(5), "nothing to drain");
                std::thread::sleep(Duration::from_millis(2));
            }
        }

        /// What the app does around a user command: flush agent batches,
        /// apply, report the entry as `changed`.
        fn user_apply(&mut self, cmd: Command) {
            self.server.flush_batches(self.rig.ed.history_mut());
            let mut ed = std::mem::take(&mut self.rig.ed);
            ed.apply(&mut self.rig.cx(), cmd).unwrap();
            self.rig.ed = ed;
            self.report();
        }

        fn report(&mut self) {
            for e in self.rig.ed.take_user_edits() {
                self.server.notify_changed(UserChange {
                    kind: e.kind,
                    label: &e.label,
                    entry_origin: e.entry_origin.clone(),
                    changes: &e.changes,
                });
            }
        }

        /// A request from `agent`, drained and answered.
        fn call(&mut self, agent: &mut Agent, method: &str, params: Value) -> Value {
            let id = agent.send(method, params);
            self.drain();
            agent.response(id)
        }
    }

    /// A blocking WebSocket client (the editor drains on the same thread).
    struct Agent {
        ws: WebSocket<MaybeTlsStream<TcpStream>>,
        next: u64,
        notes: Vec<Value>,
    }

    impl Agent {
        fn connect(server: &ServerHandle, name: &str) -> Agent {
            let (ws, _) =
                tungstenite::connect(format!("ws://127.0.0.1:{}/", server.port())).unwrap();
            if let MaybeTlsStream::Plain(s) = ws.get_ref() {
                s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            }
            let mut a = Agent {
                ws,
                next: 1,
                notes: Vec::new(),
            };
            let token = server.info().token.clone();
            a.ws.send(Message::Text(
                json!({ "jsonrpc": "2.0", "id": 0, "method": "session.hello",
                        "params": { "token": token, "client": { "name": name, "version": "1" } } })
                .to_string()
                .into(),
            ))
            .unwrap();
            let r = a.response(0);
            assert!(r.get("result").is_some(), "{r}");
            a
        }

        fn send(&mut self, method: &str, params: Value) -> u64 {
            let id = self.next;
            self.next += 1;
            self.ws
                .send(Message::Text(
                    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
                        .to_string()
                        .into(),
                ))
                .unwrap();
            id
        }

        fn read(&mut self) -> Value {
            loop {
                match self.ws.read().expect("no message within 5 s") {
                    Message::Text(t) => return serde_json::from_str(&t).unwrap(),
                    Message::Close(_) => panic!("closed"),
                    _ => continue,
                }
            }
        }

        fn response(&mut self, id: u64) -> Value {
            loop {
                let v = self.read();
                if v.get("id") == Some(&json!(id)) {
                    return v;
                }
                self.notes.push(v);
            }
        }

        /// The next notification `method` (already received or arriving).
        fn notification(&mut self, method: &str) -> Value {
            if let Some(i) = self.notes.iter().position(|n| n["method"] == method) {
                return self.notes.remove(i)["params"].clone();
            }
            loop {
                let v = self.read();
                if v["method"] == method {
                    return v["params"].clone();
                }
                self.notes.push(v);
            }
        }
    }

    fn fill(rect: [u32; 4], value: &str) -> Command {
        Command::FillRect {
            level: Slug::new("crevasse").unwrap(),
            layer: Slug::new("collision").unwrap(),
            rect: Rect::new(rect[0], rect[1], rect[2], rect[3]),
            value: Some(Slug::new(value).unwrap()).into(),
        }
    }

    fn fill_params(rect: [u32; 4], value: &str) -> Value {
        json!({ "level": "crevasse", "layer": "collision", "rect": rect, "value": value })
    }

    /// An agent's write drained through `EditorHost` takes the user's path:
    /// the same invalidations and the same incremental rebake as the same
    /// edit made in the editor.
    #[test]
    fn agent_write_routes_like_a_user_edit() {
        let mut a = LiveRig::new();
        let mut agent = Agent::connect(&a.server, "claude-code");
        let r = a.call(
            &mut agent,
            "grid.fill_rect",
            fill_params([3, 2, 4, 2], "solid"),
        );
        assert_eq!(r["result"]["status"], "applied", "{r}");
        assert_eq!(r["result"]["rev"], 1);

        let mut u = Rig::new();
        let mut ed = std::mem::take(&mut u.ed);
        ed.apply(&mut u.cx(), fill([3, 2, 4, 2], "solid")).unwrap();
        u.ed = ed;

        assert!(!a.rig.inv.is_empty());
        assert_eq!(a.rig.inv, u.inv, "invalidations");
        assert_eq!(a.rig.invalidated(), u.invalidated());
        assert!(
            a.rig.invalidated().contains_key("main_tiling"),
            "the auto layer was rebaked: {:?}",
            a.rig.invalidated()
        );
        assert_eq!(
            a.rig.rows("crevasse", "collision"),
            u.rows("crevasse", "collision")
        );
        let ix = a.rig.doc.level_ix("crevasse").unwrap();
        assert_eq!(
            unshare(a.rig.doc.bakes[ix].as_ref().unwrap()),
            unshare(u.doc.bakes[ix].as_ref().unwrap())
        );
        assert!(a.rig.doc.bake_owned[ix]);
        assert!(a.rig.ed.has_unsaved());
        let e = a.rig.ed.history().undo_entries();
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].origin, Origin::Agent("claude-code".into()));
        // The agent's write isn't reported as a user edit.
        assert!(a.rig.ed.take_user_edits().is_empty());
    }

    /// An agent's definition commands drained through `EditorHost`: a new
    /// layer resyncs the per-layer view by slug and rebuilds the scene; a
    /// rule group edit rebakes that layer in every level (the active one at
    /// once, the rest on the workers) without rebuilding the scene, so no
    /// stale bake stays on the canvas or in `project.bake`; undo does the
    /// same in reverse.
    #[test]
    fn agent_def_changes_resync_layers_and_rebake_every_level() {
        use worlds_editor::tools::Invalidation;
        let mut a = LiveRig::new();
        let mut agent = Agent::connect(&a.server, "claude-code");
        let fresh = |doc: &worlds_editor::doc::Document, ix: usize| {
            worlds_rules::bake_level(&doc.project, doc.level(ix), &doc.images).unwrap()
        };
        let n = a.rig.doc.levels.len();
        assert!(n > 1);
        let coll = super::layer(&a.rig.doc, "collision");
        a.rig.view.layers[coll].visible = false;
        a.rig.view.active_layer = Some(coll);

        // A new layer on top: the table grows, settings follow their slug.
        let r = a.call(
            &mut agent,
            "defs.set",
            json!({ "kind": "layer", "index": 0, "def": { "slug": "hazards", "name": "Hazards",
                "kind": "int_grid", "values": [{ "slug": "spikes", "name": "Spikes", "color": "#c03030", "glyph": "^" }] } }),
        );
        assert_eq!(r["result"]["status"], "applied", "{r}");
        let doc = &a.rig.doc;
        assert_eq!(a.rig.view.layers.len(), doc.project.layers.len());
        let coll2 = super::layer(doc, "collision");
        assert_eq!(coll2, coll + 1);
        assert!(!a.rig.view.layers[coll2].visible, "collision stays hidden");
        assert!(a.rig.view.layers[0].visible, "the new layer is shown");
        assert_eq!(a.rig.view.active_layer, Some(coll2));
        assert!(a.rig.inv.contains(&Invalidation::All));

        // A rule group that fires on every solid cell: every level's bake
        // is the fresh one, in the document and in the cache.
        let before: Vec<_> = (0..n)
            .map(|ix| unshare(a.rig.doc.bakes[ix].as_ref().unwrap()))
            .collect();
        a.rig.inv.clear();
        let r = a.call(
            &mut agent,
            "rules.set_group",
            json!({ "layer": "main_tiling", "index": 0, "group": { "slug": "everywhere", "rules": [
                { "slug": "solid_tile", "match": { "rows": ["#"] }, "out": { "tileset": "castle_main", "tiles": [[3, 3]] } } ] } }),
        );
        assert_eq!(r["result"]["status"], "applied", "{r}");
        assert!(!a.rig.inv.contains(&Invalidation::All));
        for ix in 0..n {
            assert!(a.rig.inv.contains(&Invalidation::Layer {
                level: ix,
                layer: Slug::new("main_tiling").unwrap()
            }));
        }
        a.rig.doc.finish_layer_bakes();
        for ix in 0..n {
            let now = unshare(a.rig.doc.bakes[ix].as_ref().unwrap());
            assert_eq!(now, fresh(&a.rig.doc, ix), "level {ix}");
            assert!(a.rig.doc.stale_bakes[ix].is_empty());
        }
        let crev = a.rig.doc.level_ix("crevasse").unwrap();
        assert_ne!(
            unshare(a.rig.doc.bakes[crev].as_ref().unwrap()),
            before[crev],
            "the new group fires in crevasse"
        );

        // Undo through the API: back to the old bakes.
        let r = a.call(&mut agent, "history.undo", json!({}));
        assert_eq!(r["result"]["steps"].as_array().unwrap().len(), 1, "{r}");
        a.rig.doc.finish_layer_bakes();
        for (ix, old) in before.iter().enumerate() {
            assert_eq!(&unshare(a.rig.doc.bakes[ix].as_ref().unwrap()), old);
        }

        // A renamed layer keeps its settings.
        let r = a.call(
            &mut agent,
            "defs.rename",
            json!({ "kind": "layer", "from": "collision", "to": "walls" }),
        );
        assert_eq!(r["result"]["status"], "applied", "{r}");
        let walls = super::layer(&a.rig.doc, "walls");
        assert!(!a.rig.view.layers[walls].visible);
        assert_eq!(a.rig.view.active_layer, Some(walls));
    }

    /// A user entry during an open agent batch: the editor flushes first,
    /// so the batch ends as two entries around the user's.
    #[test]
    fn user_edit_mid_batch_splits_the_batch() {
        let mut a = LiveRig::new();
        let mut agent = Agent::connect(&a.server, "claude-code");
        a.call(
            &mut agent,
            "history.begin_batch",
            json!({ "label": "Agent: wall" }),
        );
        a.call(
            &mut agent,
            "grid.fill_rect",
            fill_params([3, 2, 1, 1], "solid"),
        );
        a.user_apply(fill([10, 1, 1, 1], "water"));
        a.call(
            &mut agent,
            "grid.fill_rect",
            fill_params([4, 2, 1, 1], "solid"),
        );
        let r = a.call(&mut agent, "history.end_batch", json!({}));
        assert_eq!(r["result"]["entries"], 2, "{r}");
        let origins: Vec<(String, Origin)> = a
            .rig
            .ed
            .history()
            .undo_entries()
            .iter()
            .map(|e| (e.label.clone(), e.origin.clone()))
            .collect();
        let agent_o = Origin::Agent("claude-code".into());
        assert_eq!(origins[0], ("Agent: wall".into(), agent_o.clone()));
        assert_eq!(origins[1].1, Origin::User);
        assert_eq!(origins[2], ("Agent: wall".into(), agent_o));
        // The user entry bumped rev once: 2 agent writes + 1 user edit.
        assert_eq!(a.server.rev(), 3);
    }

    /// The toast's undo targets exactly its entry, and only while that entry
    /// is on top of the stack.
    #[test]
    fn toast_undo_undoes_that_entry() {
        let mut a = LiveRig::new();
        let before_rows = a.rig.rows("crevasse", "collision");
        let mut agent = Agent::connect(&a.server, "claude-code");
        let depth = a.rig.ed.history().undo_entries().len();
        a.call(
            &mut agent,
            "grid.fill_rect",
            fill_params([3, 2, 2, 1], "solid"),
        );
        let new = new_agent_entries(a.rig.ed.history(), depth);
        assert_eq!(new.len(), 1);
        let u = new[0].clone();
        assert!(agent_entry_on_top(a.rig.ed.history(), &u));
        // A user edit on top disables it ...
        a.user_apply(fill([10, 1, 1, 1], "water"));
        assert!(!agent_entry_on_top(a.rig.ed.history(), &u));
        // ... until it is undone.
        a.rig.undo();
        a.report();
        assert!(agent_entry_on_top(a.rig.ed.history(), &u));
        a.rig.undo();
        let edits = a.rig.ed.take_user_edits();
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].kind, ChangeKind::Undo);
        assert_eq!(
            edits[0].entry_origin,
            Some(Origin::Agent("claude-code".into()))
        );
        assert_eq!(a.rig.rows("crevasse", "collision"), before_rows);
        assert!(a.rig.ed.history().undo_entries().is_empty());
    }

    /// Approval mode: a write waits as a pending batch with a ghost; Apply
    /// goes through the host, Reject leaves the project and `rev` alone.
    #[test]
    fn approval_queue_ghost_apply_and_reject() {
        let mut a = LiveRig::new();
        a.server.set_approval(true);
        let before_rows = a.rig.rows("crevasse", "collision");
        let mut agent = Agent::connect(&a.server, "claude-code");
        let id = agent.send("grid.fill_rect", fill_params([3, 2, 2, 1], "solid"));
        let h = a.drain();
        let batch = h[0].pending.clone().expect("pending");
        assert_eq!(a.server.rev(), 0);
        assert_eq!(a.rig.rows("crevasse", "collision"), before_rows);
        assert!(a.rig.inv.is_empty());

        // The ghost: one run of two solid cells, outlined.
        let g = Ghost::compute(&a.rig.doc.project, &a.server.pending()[0]);
        assert_eq!(g.batch, batch);
        assert_eq!(g.error, None);
        assert_eq!(g.cells(), 2);
        let grid = a.rig.doc.project.grid_size as f64;
        let l = a.rig.doc.project.level("crevasse").unwrap().world_px;
        assert_eq!(g.fills.len(), 1);
        let (r, c) = g.fills[0];
        assert_eq!(r.min, [l[0] as f64 + 3.0 * grid, l[1] as f64 + 2.0 * grid]);
        assert_eq!(r.max, [l[0] as f64 + 5.0 * grid, l[1] as f64 + 3.0 * grid]);
        assert_eq!(c, egui::Color32::from_rgb(0x40, 0x40, 0x40));
        assert_eq!(g.outlines.len(), 1);

        // Apply: through Host::applied, one agent entry, rev + 1.
        let changes = a.apply_pending(&batch);
        assert!(!changes.is_empty());
        assert_eq!(a.server.rev(), 1);
        assert!(!a.rig.inv.is_empty());
        assert_eq!(&a.rig.rows("crevasse", "collision")[2][3..5], "##");
        let r = agent.response(id);
        assert_eq!(r["result"]["status"], "applied", "{r}");

        // Reject: nothing changes, the request gets `rejected`.
        let rows = a.rig.rows("crevasse", "collision");
        a.rig.inv.clear();
        let id = agent.send("grid.fill_rect", fill_params([6, 2, 2, 1], "water"));
        let b2 = a.drain()[0].pending.clone().unwrap();
        assert_eq!(a.server.reject_pending(&b2), vec![b2.clone()]);
        assert_eq!(a.server.rev(), 1);
        assert_eq!(a.rig.rows("crevasse", "collision"), rows);
        assert!(a.rig.inv.is_empty());
        assert_eq!(agent.response(id)["error"]["code"], -32003);
        assert!(a.server.pending().is_empty());
    }

    /// The editor's events: a user edit is `changed` with origin user, a
    /// user save is `saved` with the files, `view.select` moves the view
    /// and the selection is reported; an agent's `project.save` goes
    /// through the editor's save.
    #[test]
    fn events_for_user_edits_saves_and_selection() {
        let mut a = LiveRig::new();
        let mut agent = Agent::connect(&a.server, "observer");
        a.call(
            &mut agent,
            "events.subscribe",
            json!({ "events": ["changed", "saved", "problems", "selection"] }),
        );
        a.user_apply(fill([10, 1, 2, 1], "water"));
        let c = agent.notification("changed");
        assert_eq!(c["origin"], "user");
        assert_eq!(c["kind"], "edit");
        assert_eq!(c["rev"], 1);

        let dirty: Vec<std::path::PathBuf> =
            a.rig.doc.project.files.dirty().iter().cloned().collect();
        a.rig.save();
        let files = worlds_server::saved_paths(&dirty);
        assert_eq!(files, ["worlds/main/levels/crevasse/collision.json"]);
        a.server.notify_saved(&files);
        a.server.notify_problems(&a.rig.doc.problems);
        assert_eq!(agent.notification("saved")["files"], json!(files));
        assert!(agent.notification("problems")["problems"].is_array());

        let r = a.call(&mut agent, "view.select", json!({ "level": "shaft" }));
        assert!(r.get("result").is_some(), "{r}");
        let sel = selection_of(&a.rig.doc, &a.rig.view);
        assert_eq!(sel.0.as_deref(), Some("shaft"));
        a.server
            .notify_selection(sel.0.as_deref(), &sel.1, &sel.2, &sel.3);
        assert_eq!(agent.notification("selection")["level"], "shaft");

        // Unknown ids are refused, the view stays.
        let r = a.call(
            &mut agent,
            "view.select",
            json!({ "entities": ["nope000000"] }),
        );
        assert!(r.get("error").is_some(), "{r}");
        assert_eq!(
            selection_of(&a.rig.doc, &a.rig.view).0.as_deref(),
            Some("shaft")
        );

        // Notes and levels add up; the first level note's level becomes
        // active; the event carries both.
        let r = a.call(
            &mut agent,
            "view.select",
            json!({ "notes": ["notecrev22", "pinstatue2"], "levels": ["shaft"] }),
        );
        assert!(r.get("result").is_some(), "{r}");
        let sel = selection_of(&a.rig.doc, &a.rig.view);
        assert_eq!(sel.0.as_deref(), Some("crevasse"));
        assert_eq!(sel.2, ["notecrev22", "pinstatue2"]);
        assert_eq!(sel.3, ["shaft"]);
        a.server
            .notify_selection(sel.0.as_deref(), &sel.1, &sel.2, &sel.3);
        let ev = agent.notification("selection");
        assert_eq!(ev["notes"], json!(["notecrev22", "pinstatue2"]), "{ev}");
        assert_eq!(ev["levels"], json!(["shaft"]), "{ev}");
        // view.focus on a note frames it and makes its level active.
        a.rig.view.active_level = a.rig.doc.level_ix("shaft");
        let r = a.call(&mut agent, "view.focus", json!({ "note": "notecrev22" }));
        assert!(r.get("result").is_some(), "{r}");
        assert_eq!(
            selection_of(&a.rig.doc, &a.rig.view).0.as_deref(),
            Some("crevasse")
        );
        assert!(matches!(
            a.fx.focus,
            Some(worlds_editor::viewport::Fit::Rect(_))
        ));

        // An agent save: the editor's save, recorded as our own write.
        a.call(
            &mut agent,
            "grid.fill_rect",
            fill_params([3, 2, 1, 1], "solid"),
        );
        let r = a.call(&mut agent, "project.save", json!({}));
        assert_eq!(
            r["result"]["files"],
            json!(["worlds/main/levels/crevasse/collision.json"]),
            "{r}"
        );
        assert!(a.fx.problems);
        assert!(!a.rig.ed.has_unsaved());
        let path = a
            .rig
            .doc
            .dir
            .join("worlds/main/levels/crevasse/collision.json");
        assert!(a.own.explains(&path));
        assert_eq!(agent.notification("saved")["files"], r["result"]["files"]);
    }
}

/// "Save as template" with "Replace by a reference" only takes rules the
/// reference can stand in for exactly: one run of one group (so priority and
/// the group's conditions stay), and a reference slug no other entry has
/// (`set_rule` would replace it).
#[test]
fn save_as_template_replace_refuses_what_would_reorder_or_overwrite() {
    use worlds_editor::app::rules::save_template_commands;
    let r = Rig::new();
    let layer = Slug::new("main_tiling").unwrap();
    let save = |rules: &[&str], reference: &str| {
        let rules: Vec<Slug> = rules.iter().map(|s| Slug::new(*s).unwrap()).collect();
        save_template_commands(
            &r.doc.project,
            &layer,
            &rules,
            "tpl",
            reference,
            [0, 0],
            true,
        )
    };
    // A and C of [A, B, C]: C would jump ahead of B.
    let e = save(&["wall_top_edge", "any_wall_group"], "edges").unwrap_err();
    assert!(
        e.contains("wall_fill") && e.contains("next to each other"),
        "{e}"
    );
    // Rules of two groups: the second group's mode and requirement would go.
    let e = save(&["any_wall_group", "drips"], "mixed").unwrap_err();
    assert!(e.contains("one group"), "{e}");
    // The slug of a rule left out of the selection.
    let e = save(&["wall_top_edge"], "wall_fill").unwrap_err();
    assert!(e.contains("wall_fill") && e.contains("already"), "{e}");
    let e = save(&["drips"], "pillar_on_floor").unwrap_err();
    assert!(e.contains("group pillars"), "{e}");
    // One run of one group, a free slug (or one of the rules' own): fine,
    // and the reference lands where the run was.
    let mut r = Rig::new();
    let (label, cmds) = save_template_commands(
        &r.doc.project,
        &layer,
        &[
            Slug::new("wall_fill").unwrap(),
            Slug::new("any_wall_group").unwrap(),
        ],
        "tpl",
        "wall_fill",
        [0, 0],
        true,
    )
    .unwrap();
    r.apply_cmd(worlds_core::Command::Batch {
        label,
        commands: cmds,
    });
    let walls: Vec<&str> = r.doc.project.rules_for("main_tiling").unwrap().groups[0]
        .rules
        .iter()
        .map(|e| e.slug().as_str())
        .collect();
    assert_eq!(walls, ["wall_top_edge", "wall_fill"]);
}

/// One edit breaks `main_tiling` (its named pattern goes) and changes
/// `glow` (a legend only it reads): in the active level `glow` still shows
/// its new bake and `main_tiling` shows nothing, neither keeps an old bake,
/// and the other level's jobs agree.
#[test]
fn a_rule_edit_that_breaks_one_layer_still_rebakes_the_others() {
    let mut r = rule_rig();
    let (crev, shaft) = (
        r.doc.level_ix("crevasse").unwrap(),
        r.doc.level_ix("shaft").unwrap(),
    );
    for cmd in [
        serde_json::json!({ "op": "set_macro", "kind": "pattern", "name": "solo", "def": ["+"] }),
        serde_json::json!({ "op": "set_macro", "kind": "legend", "name": "lg", "def": { "x": "solid" } }),
        serde_json::json!({ "op": "set_rule", "layer": "main_tiling", "group": "walls", "rule": "wall_fill",
            "def": { "slug": "wall_fill", "match": { "rows": "solo" },
                     "out": { "tileset": "castle_main", "tiles": [[2, 2]] }, "out_of_bounds": "solid" } }),
        serde_json::json!({ "op": "set_rule", "layer": "glow", "group": "g", "rule": "lit",
            "def": { "slug": "lit", "match": { "legend": "lg", "rows": ["x"] },
                     "out": { "tileset": "castle_main", "tiles": [[3, 3]] } } }),
    ] {
        r.apply(cmd);
    }
    r.doc.finish_layer_bakes();
    let glow_before = r.shown("crevasse", "glow");
    r.inv.clear();
    r.apply(
        serde_json::json!({ "op": "batch", "label": "break one", "commands": [
        { "op": "remove_macro", "kind": "pattern", "name": "solo", "keep_uses": true },
        { "op": "set_macro", "kind": "legend", "name": "lg", "def": { "x": "water" } } ] }),
    );
    let glow_now = r.fresh("crevasse", "glow");
    assert_ne!(glow_now, glow_before, "the edit must change glow");
    assert_eq!(r.shown("crevasse", "glow"), glow_now);
    assert_eq!(r.ed.cache().get("crevasse", "glow"), Some(&glow_now));
    assert!(r.shown("crevasse", "main_tiling").placements.is_empty());
    assert!(r.ed.cache().get("crevasse", "main_tiling").is_none());
    assert!(
        r.doc.stale_bakes[crev].is_empty(),
        "{:?}",
        r.doc.stale_bakes[crev]
    );
    for l in ["main_tiling", "glow"] {
        assert!(
            r.inv.contains(&Invalidation::Layer {
                level: crev,
                layer: Slug::new(l).unwrap()
            }),
            "{l}: {:?}",
            r.inv
        );
    }
    let status = r
        .view
        .status
        .as_ref()
        .map(|s| s.text.clone())
        .unwrap_or_default();
    assert!(status.contains("main_tiling"), "{status}");
    r.doc.finish_layer_bakes();
    assert_eq!(r.shown("shaft", "glow"), r.fresh("shaft", "glow"));
    assert!(r.shown("shaft", "main_tiling").placements.is_empty());
    assert!(r.doc.stale_bakes[shaft].is_empty());
}

/// A whole-level worker bake bakes each layer on its own: a layer whose
/// rules don't bake shows nothing (not its old tiles) and isn't left stale
/// for every export to retry, and the level's other layers still land.
#[test]
fn a_failed_layer_in_a_level_bake_shows_nothing_and_isnt_stale() {
    let mut r = rule_rig();
    assert!(!r.shown("crevasse", "main_tiling").placements.is_empty());
    // Rules that stopped baking behind the document's back, then a tileset
    // image reload: every level is stale and rebaked whole.
    let k = r
        .doc
        .project
        .rules
        .iter()
        .position(|set| set.layer == "main_tiling")
        .unwrap();
    r.doc.project.rules[k].groups[0].rules[1] = serde_json::from_value(serde_json::json!(
        { "slug": "wall_fill", "match": { "rows": "missing" },
          "out": { "tileset": "castle_main", "tiles": [[2, 2]] } }))
    .unwrap();
    let ts = r
        .doc
        .project
        .tilesets
        .iter()
        .position(|t| t.slug == "castle_main")
        .unwrap();
    let stale = r.doc.tileset_changed(ts);
    assert!(stale.iter().any(|l| l == "main_tiling"), "{stale:?}");
    r.doc.bake_all_blocking();
    assert!(
        r.doc.stale_bakes.iter().all(|s| s.is_empty()),
        "{:?}",
        r.doc.stale_bakes
    );
    assert!(r.shown("crevasse", "main_tiling").placements.is_empty());
    assert_eq!(r.shown("crevasse", "glow"), r.fresh("crevasse", "glow"));
    assert!(!r.shown("crevasse", "glow").placements.is_empty());
}

/// Layer jobs a newer request superseded before they started are skipped,
/// not baked; and the jobs share one queue instead of a thread batch each.
#[test]
fn superseded_layer_jobs_are_skipped_not_baked() {
    let mut r = rule_rig();
    let shaft = r.doc.level_ix("shaft").unwrap();
    // No workers: the first edit's job waits in the queue...
    r.doc.max_layer_workers = 0;
    r.apply(set_wall_fill([2, 2]));
    assert_eq!(r.doc.layer_jobs_pending, 1);
    // ...until a second edit, whose batch starts a worker for both.
    r.doc.max_layer_workers = 1;
    r.apply(set_wall_fill([6, 6]));
    let (dropped, skipped) = (r.doc.dropped_results, r.doc.skipped_layer_jobs);
    let arrived = r.doc.finish_layer_bakes();
    assert_eq!(arrived, vec![(shaft, Slug::new("main_tiling").unwrap())]);
    assert_eq!(
        r.doc.skipped_layer_jobs,
        skipped + 1,
        "the first job never baked"
    );
    assert_eq!(r.doc.dropped_results, dropped + 1);
    assert_eq!(
        r.shown("shaft", "main_tiling"),
        r.fresh("shaft", "main_tiling")
    );
    assert!(r.doc.stale_bakes.iter().all(|s| s.is_empty()));
}

// ---------------------------------------------------------------------
// Derived layers, passes, region rules
// ---------------------------------------------------------------------

/// mini.worlds plus a derived-layer setup as one entry: a derived layer `marks`
/// (value `pillar_base`, glyph `p`) and an auto layer `pillars` whose pass 0
/// region rule writes marks along every floor run (every 4, margin 1) and
/// whose pass 1 rule draws a tile on each mark. In crevasse the marks land
/// at (9, 1), (5, 2), (16, 2), (8, 3), (12, 3).
fn derived_setup() -> serde_json::Value {
    serde_json::json!({ "op": "batch", "label": "Derived layer setup", "commands": [
        { "op": "set_layer_def", "layer": "marks", "index": 1,
          "def": { "slug": "marks", "name": "Marks", "kind": "int_grid", "derived": true,
                   "values": [{ "slug": "pillar_base", "name": "Pillar base", "color": "#e0a040", "glyph": "p" }] } },
        { "op": "set_layer_def", "layer": "pillars", "index": 2,
          "def": { "slug": "pillars", "name": "Pillars", "kind": "auto", "source": "collision" } },
        { "op": "set_rule_group", "layer": "pillars", "group": "find", "index": 0,
          "def": { "slug": "find", "rules": [
            { "slug": "floor_marks",
              "region": { "cells": ".", "min_width": 5 },
              "place": { "along": "floor", "every": 4, "margin": 1 },
              "write": { "grid": "marks", "value": "pillar_base" } } ] } },
        { "op": "set_rule_group", "layer": "pillars", "group": "art", "index": 1,
          "def": { "slug": "art", "pass": 1, "rules": [
            { "slug": "on_mark", "match": { "grid": "marks", "rows": ["p"] },
              "out": { "tileset": "castle_main", "tiles": [[1, 1]] } } ] } }
    ] })
}

const MARKS: [[i32; 2]; 5] = [[9, 1], [5, 2], [16, 2], [8, 3], [12, 3]];

/// A rig with the derived-layer setup applied and saved.
fn derived_rig() -> Rig {
    let mut r = Rig::new();
    r.apply(derived_setup());
    r.save();
    r
}

/// The document's effective `marks` grid in crevasse, as row strings
/// (`p` / `.`).
fn marks_rows(r: &Rig) -> Vec<String> {
    let ix = r.doc.level_ix("crevasse").unwrap();
    let def = r.doc.project.layer("marks").unwrap().clone();
    r.doc.derived_effective(ix, &def).to_rows(&def).unwrap()
}

/// Every check that the shown state is the fresh state: the `pillars` bake
/// and the effective `marks` grid equal a fresh bake (and the cache's).
fn assert_fresh(r: &Rig) {
    let ix = r.doc.level_ix("crevasse").unwrap();
    assert_eq!(
        r.shown("crevasse", "pillars"),
        r.fresh("crevasse", "pillars")
    );
    let def = r.doc.project.layer("marks").unwrap().clone();
    let fresh =
        worlds_rules::effective_grid(&r.doc.project, r.doc.level(ix), "marks", &r.doc.images)
            .unwrap();
    assert_eq!(r.doc.derived_effective(ix, &def), fresh);
    if let Some(c) = r.ed.cache().derived("crevasse", "marks") {
        assert_eq!(*c, fresh, "the cache's effective grid");
    }
}

#[test]
fn derived_layer_bakes_and_draws_its_marks() {
    let r = derived_rig();
    let rows = marks_rows(&r);
    for [x, y] in MARKS {
        assert_eq!(rows[y as usize].as_bytes()[x as usize], b'p', "{rows:?}");
    }
    assert_eq!(rows.concat().matches('p').count(), MARKS.len(), "{rows:?}");
    // Pass 1 drew a tile on every mark.
    let b = r.shown("crevasse", "pillars");
    let mut anchors: Vec<[i32; 2]> = b.placements.iter().map(|p| p.anchor).collect();
    anchors.sort_by_key(|a| (a[1], a[0]));
    assert_eq!(anchors, MARKS);
    assert_fresh(&r);
}

/// Brush, Suppress, Eraser and Fill on a derived layer: each gesture is one
/// entry, saves exactly the layer's overrides file, and undo brings every
/// file back byte for byte (the file goes away with the last override).
#[test]
fn derived_layer_gestures_save_one_file_and_undo_byte_exact() {
    let mut r = derived_rig();
    let dir = r.doc.dir.clone();
    let bytes = |d: &Path| -> BTreeMap<PathBuf, Vec<u8>> {
        files(d).into_iter().map(|(p, (b, _))| (p, b)).collect()
    };
    let start = bytes(&dir);
    let file = PathBuf::from("worlds/main/levels/crevasse/marks.json");
    assert!(!start.contains_key(&file));
    let undo0 = r.ed.history().undo_entries().len();
    let step = |r: &mut Rig, json: &str, want: &[(usize, &str)]| {
        let before = bytes(&dir);
        let n = r.ed.history().undo_entries().len();
        r.gesture(json);
        assert!(
            r.view.status.as_ref().is_none_or(|s| !s.error),
            "{:?}",
            r.view.status
        );
        assert_eq!(
            r.ed.history().undo_entries().len(),
            n + 1,
            "one entry: {json}"
        );
        r.save();
        let after = bytes(&dir);
        let changed: Vec<&PathBuf> = after
            .keys()
            .chain(before.keys())
            .filter(|p| before.get(*p) != after.get(*p))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        assert_eq!(changed, [&file], "{json}");
        let saved: serde_json::Value = serde_json::from_slice(&after[&file]).unwrap();
        assert_eq!(saved["overrides"], true);
        for (y, row) in want {
            assert_eq!(saved["rows"][*y], *row, "{json}: {saved}");
        }
        assert_fresh(r);
    };
    // Brush: two overrides.
    step(
        &mut r,
        r#"{ "gesture": "brush", "level": "crevasse", "layer": "marks", "path": [[2, 1], [3, 1]], "brush": "pillar_base" }"#,
        &[(1, "..pp................")],
    );
    let rows = marks_rows(&r);
    assert_eq!(&rows[1][..4], "..pp");
    // Suppress the computed mark at (9, 1).
    step(
        &mut r,
        r#"{ "gesture": "brush", "level": "crevasse", "layer": "marks", "path": [[9, 1]], "brush": "suppress" }"#,
        &[(1, "..pp.....!..........")],
    );
    assert_eq!(&marks_rows(&r)[1][9..10], ".");
    // The pass 1 rule no longer draws there.
    assert!(
        !r.shown("crevasse", "pillars")
            .placements
            .iter()
            .any(|p| p.anchor == [9, 1])
    );
    // The eraser takes an override away (back to the rules).
    step(
        &mut r,
        r#"{ "gesture": "eraser", "level": "crevasse", "layer": "marks", "path": [[2, 1]] }"#,
        &[(1, "...p.....!..........")],
    );
    // Fill floods the effective region (what the map shows), not the
    // stored plane: from (0, 4), the 4-connected cells that are empty in
    // the effective grid, which stops at the marks.
    let ix = r.doc.level_ix("crevasse").unwrap();
    let def = r.doc.project.layer("marks").unwrap().clone();
    let eff = r.doc.derived_effective(ix, &def);
    let region = worlds_editor::edit::value_region(&eff, [20, 5], [0, 4]);
    let marked = eff.cells().iter().filter(|&&c| c != 0).count();
    assert_eq!(
        region.len(),
        100 - marked,
        "isolated marks: everything else"
    );
    r.gesture(
        r#"{ "gesture": "fill", "level": "crevasse", "layer": "marks", "path": [[0, 4]], "brush": "pillar_base" }"#,
    );
    let ov = r
        .doc
        .project
        .level("crevasse")
        .unwrap()
        .overrides("marks")
        .unwrap();
    for y in 0..5u32 {
        for x in 0..20u32 {
            let in_region = region.contains(&[x as i32, y as i32]);
            let o = ov.get(x, y).unwrap();
            if in_region {
                assert_eq!(o, worlds_core::Override::Value(1), "({x}, {y})");
            } else {
                assert!(
                    o != worlds_core::Override::Value(1) || [x, y] == [3, 1],
                    "({x}, {y}) {o:?}"
                );
            }
        }
    }
    // The suppressed cell was empty in the effective grid, so the fill
    // took it too; the computed marks stay computed.
    assert_eq!(ov.get(9, 1), Some(worlds_core::Override::Value(1)));
    assert_eq!(ov.get(8, 3), Some(worlds_core::Override::None));
    assert_fresh(&r);
    r.save();
    // Undo all four: the overrides file goes away, every file as it was.
    while r.ed.history().undo_entries().len() > undo0 {
        r.undo();
        assert_fresh(&r);
    }
    r.save();
    assert_eq!(bytes(&dir), start);
}

/// Alt+click on a derived layer picks the stored state: an override's
/// value, Suppress, or Erase where the rules decide.
#[test]
fn derived_layer_alt_click_samples_the_stored_state() {
    let mut r = derived_rig();
    r.apply(
        serde_json::json!({ "op": "paint_cells", "level": "crevasse", "layer": "marks",
        "cells": [[2, 1, "pillar_base"], [9, 1, "!"]] }),
    );
    let pick = |r: &mut Rig, x: i32| {
        r.gesture(&format!(
            r#"{{ "gesture": "brush", "level": "crevasse", "layer": "marks", "path": [[{x}, 1]], "alt": true }}"#
        ));
        r.view.palette.brush("marks").cloned()
    };
    assert_eq!(pick(&mut r, 2), Some(Brush::Value(1)));
    assert_eq!(pick(&mut r, 9), Some(Brush::Suppress));
    assert_eq!(pick(&mut r, 5), Some(Brush::Erase));
    // Suppress is refused on a layer that isn't derived.
    let g: GestureStep = serde_json::from_str(
        r#"{ "gesture": "brush", "level": "crevasse", "layer": "collision", "path": [[2, 1]], "brush": "suppress" }"#,
    )
    .unwrap();
    let e = gesture_events(&r.doc, &g, None, None)
        .unwrap_err()
        .to_string();
    assert!(e.contains("derived"), "{e}");
}

/// The status bar names a derived cell's state.
#[test]
fn derived_cells_describe_their_state() {
    let mut r = derived_rig();
    r.apply(
        serde_json::json!({ "op": "paint_cells", "level": "crevasse", "layer": "marks",
        "cells": [[2, 1, "pillar_base"], [9, 1, "!"]] }),
    );
    let ix = r.doc.level_ix("crevasse").unwrap();
    let def = r.doc.project.layer("marks").unwrap().clone();
    let d = |x, y| worlds_editor::viewport::describe_derived(&r.doc, ix, &def, x, y);
    assert_eq!(d(2, 1), "marks: pillar_base (override)");
    assert_eq!(d(9, 1), "marks: suppressed (rules: pillar_base)");
    assert_eq!(d(8, 3), "marks: pillar_base (rules)");
    assert_eq!(d(0, 0), "marks: empty (rules)");
}

/// The editor's export on save writes derived layers' effective grids, and
/// every level file equals `worlds_rules::bake_for_export` (what `worlds-cli
/// bake` writes) byte for byte.
#[test]
fn export_on_save_with_a_derived_layer_equals_bake_for_export() {
    let mut r = derived_rig();
    r.doc.project.export_on_save = true;
    r.apply(
        serde_json::json!({ "op": "paint_cells", "level": "crevasse", "layer": "marks",
        "cells": [[2, 1, "pillar_base"], [9, 1, "!"]] }),
    );
    r.gesture(r#"{ "gesture": "rect", "level": "crevasse", "layer": "collision", "path": [[14, 2], [15, 2]], "brush": "solid" }"#);
    r.save();
    let dir = r.doc.dir.clone();
    let fresh = tempfile::tempdir().unwrap();
    let p = worlds_core::load(&dir).unwrap();
    let images = worlds_rules::TilesetImages::new(&dir, &p.tilesets);
    let baked = p
        .levels()
        .map(|l| {
            (
                l.slug.clone(),
                worlds_rules::bake_for_export(&p, l, &images).unwrap(),
            )
        })
        .collect();
    worlds_core::export::write(&p, &dir, fresh.path(), &baked, true).unwrap();
    let strip = |s: &str| {
        s.lines()
            .filter(|l| !l.contains("\"image\""))
            .collect::<Vec<_>>()
            .join("\n")
    };
    for level in ["crevasse", "shaft"] {
        let ours =
            std::fs::read_to_string(dir.join(format!("export/levels/{level}.json"))).unwrap();
        let theirs =
            std::fs::read_to_string(fresh.path().join(format!("levels/{level}.json"))).unwrap();
        assert_eq!(strip(&ours), strip(&theirs), "{level}");
    }
    // The level file carries the derived grid, effective.
    let v: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(dir.join("export/levels/crevasse.json")).unwrap(),
    )
    .unwrap();
    let marks = &v["int_grids"]["marks"];
    assert_eq!(marks["derived"], true);
    let text = marks.to_string();
    assert!(
        !text.contains('!'),
        "effective, never the override plane: {text}"
    );
}

/// A rule edit on the writer rebakes its readers too, in the active level
/// at once and on the workers elsewhere.
#[test]
fn a_writer_rule_edit_rebakes_its_readers() {
    let mut r = derived_rig();
    // A reader in another layer: `deco` draws on the marks.
    r.apply(
        serde_json::json!({ "op": "batch", "label": "reader", "commands": [
        { "op": "set_layer_def", "layer": "deco", "index": 3,
          "def": { "slug": "deco", "name": "Deco", "kind": "auto", "source": "marks" } },
        { "op": "set_rule_group", "layer": "deco", "group": "g", "index": 0,
          "def": { "slug": "g", "rules": [
            { "slug": "dot", "match": { "rows": ["p"] },
              "out": { "tileset": "castle_main", "tiles": [[2, 2]] } } ] } } ] }),
    );
    let deco = |r: &Rig, level: &str| r.shown(level, "deco").placements.len();
    assert_eq!(deco(&r, "crevasse"), MARKS.len());
    // Turn the writer off: no marks, nothing for either reader.
    r.apply(serde_json::json!({ "op": "set_rule", "layer": "pillars", "group": "find", "rule": "floor_marks",
        "def": { "slug": "floor_marks", "active": false,
                 "region": { "cells": ".", "min_width": 5 },
                 "place": { "along": "floor", "every": 4, "margin": 1 },
                 "write": { "grid": "marks", "value": "pillar_base" } } }));
    r.doc.finish_layer_bakes();
    for level in ["crevasse", "shaft"] {
        assert_eq!(deco(&r, level), 0, "{level}");
        assert_eq!(r.shown(level, "deco"), r.fresh(level, "deco"), "{level}");
        assert_eq!(
            r.shown(level, "pillars"),
            r.fresh(level, "pillars"),
            "{level}"
        );
    }
}

/// A rule edit that makes the writer stop writing the derived grid (its
/// `write` becomes an `out`) leaves the grid with no writer: its readers
/// rebake in every level, though under the new rules they depend on
/// nobody, and the effective grid is the overrides alone.
#[test]
fn a_writer_that_stops_writing_rebakes_the_readers() {
    let mut r = derived_rig();
    r.apply(
        serde_json::json!({ "op": "batch", "label": "reader", "commands": [
        { "op": "set_layer_def", "layer": "deco", "index": 3,
          "def": { "slug": "deco", "name": "Deco", "kind": "auto", "source": "marks" } },
        { "op": "set_rule_group", "layer": "deco", "group": "g", "index": 0,
          "def": { "slug": "g", "rules": [
            { "slug": "dot", "match": { "rows": ["p"] },
              "out": { "tileset": "castle_main", "tiles": [[2, 2]] } } ] } } ] }),
    );
    let deco = |r: &Rig, level: &str| r.shown(level, "deco").placements.len();
    assert_eq!(deco(&r, "crevasse"), MARKS.len());
    r.apply(serde_json::json!({ "op": "set_rule", "layer": "pillars", "group": "find", "rule": "floor_marks",
        "def": { "slug": "floor_marks",
                 "region": { "cells": ".", "min_width": 5 },
                 "place": { "along": "floor", "every": 4, "margin": 1 },
                 "out": { "tileset": "castle_main", "tiles": [[3, 3]] } } }));
    assert!(r.doc.project.derived_writer("marks").is_none());
    r.doc.finish_layer_bakes();
    for level in ["crevasse", "shaft"] {
        assert_eq!(deco(&r, level), 0, "{level}");
        assert_eq!(r.shown(level, "deco"), r.fresh(level, "deco"), "{level}");
    }
    assert!(marks_rows(&r).iter().all(|row| !row.contains('p')));
    assert_fresh(&r);
}

/// Worker bakes run in dependency order and seed the command layer's cache
/// whole: the first stroke in a level re-sweeps from the worker's bake.
#[test]
fn worker_level_bakes_seed_the_cache_in_dependency_order() {
    let mut r = derived_rig();
    // Rebake everything as at open (on this thread, through the same path).
    r.doc.bake_all_blocking();
    let ix = r.doc.level_ix("crevasse").unwrap();
    let ptr = |r: &Rig| Arc::as_ptr(&r.doc.bakes[ix].as_ref().unwrap()["pillars"]);
    let worker = ptr(&r);
    assert_eq!(
        r.shown("crevasse", "pillars"),
        r.fresh("crevasse", "pillars")
    );
    // A fresh command layer, as after open: the first stroke seeds the
    // whole level from the worker's bake (no bake on the UI thread) and
    // re-sweeps that same bake in place.
    r.ed = Editing::new();
    assert!(r.ed.cache().cached_levels().is_empty());
    r.gesture(r#"{ "gesture": "brush", "level": "crevasse", "layer": "collision", "path": [[1, 1]], "brush": "solid" }"#);
    assert!(r.ed.cache().derived("crevasse", "marks").is_some());
    assert_eq!(
        ptr(&r),
        worker,
        "seeded from the worker's bake, edited in place"
    );
    assert_fresh(&r);
}
