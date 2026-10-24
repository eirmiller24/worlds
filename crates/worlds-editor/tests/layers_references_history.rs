//! Layer operations, reference images and the History pane through the
//! real app (actions, `--script` steps, frames): a reference image draws
//! under the layers and goes with undo; duplicate, merge down and moving
//! selected tile stacks are one entry each; the History pane jumps.
//! Need a GPU (or a software driver), so they only run with
//! `WORLDS_GPU_TESTS=1`. They work on temp copies of the `game` fixture;
//! `WORLDS_TEST_OUT` keeps the pictures.

use std::path::{Path, PathBuf};

use worlds_editor::app::actions::Action;
use worlds_editor::app::{AppOptions, EditorApp};
use worlds_editor::headless::Headless;
use worlds_editor::view::SelItem;

fn gpu_tests() -> bool {
    let on = std::env::var("WORLDS_GPU_TESTS").as_deref() == Ok("1");
    if !on {
        eprintln!("skipped: set WORLDS_GPU_TESTS=1 to run");
    }
    on
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let p = e.path();
        if e.file_type().unwrap().is_dir() {
            copy_dir(&p, &to.join(e.file_name()));
        } else {
            std::fs::copy(&p, to.join(e.file_name())).unwrap();
        }
    }
}

fn game_copy() -> (tempfile::TempDir, PathBuf) {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/game/game.worlds");
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("game.worlds");
    copy_dir(&src, &dir);
    let assets = src.parent().unwrap().join("assets");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&assets, tmp.path().join("assets")).unwrap();
    #[cfg(not(unix))]
    copy_dir(&assets, &tmp.path().join("assets"));
    (tmp, dir)
}

fn keep(hl: &Headless, name: &str) {
    if let Some(dir) = std::env::var_os("WORLDS_TEST_OUT") {
        let dir = PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        hl.read().unwrap().save(dir.join(name)).unwrap();
    }
}

struct Rig {
    hl: Headless,
    ctx: egui::Context,
    app: EditorApp,
    t: f64,
    tmp: tempfile::TempDir,
}

impl Rig {
    fn new(size: [u32; 2]) -> Self {
        let (tmp, dir) = game_copy();
        let hl = Headless::new(size).unwrap();
        let ctx = egui::Context::default();
        let mut app = EditorApp::new(
            &ctx,
            hl.gpu.clone(),
            worlds_editor::log::init(),
            AppOptions {
                headless: true,
                ..Default::default()
            },
        );
        app.set_commands(Box::new(worlds_editor::edit::Editing::new()));
        app.open(&dir);
        let mut r = Rig {
            hl,
            ctx,
            app,
            t: 0.0,
            tmp,
        };
        r.frames(4);
        r
    }

    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            self.t += 1.0 / 60.0;
            self.hl.frame(&self.ctx, &mut self.app, self.t);
        }
    }

    fn steps(&mut self, json: &str) {
        for s in worlds_editor::script::parse(json).unwrap() {
            self.app.run_step(s).unwrap();
        }
        self.frames(4);
    }

    fn doc(&self) -> &worlds_editor::doc::Document {
        self.app.st.doc.as_ref().unwrap()
    }

    fn undo_len(&self) -> usize {
        self.app.history().unwrap().undo_entries().len()
    }
}

/// How many pixels of `img` inside `r` (screen points) are close to `c`.
fn count_near(img: &image::RgbaImage, r: egui::Rect, c: [u8; 3]) -> usize {
    let mut n = 0;
    for y in r.min.y.max(0.0) as u32..(r.max.y as u32).min(img.height()) {
        for x in r.min.x.max(0.0) as u32..(r.max.x as u32).min(img.width()) {
            let p = img.get_pixel(x, y);
            if (0..3).all(|k| (p[k] as i32 - c[k] as i32).abs() < 40) {
                n += 1;
            }
        }
    }
    n
}

#[test]
fn a_reference_image_draws_under_the_layers_and_undo_takes_it() {
    if !gpu_tests() {
        return;
    }
    let mut r = Rig::new([1600, 900]);
    // A bright magenta sketch outside the project: copied into art/.
    let png = r.tmp.path().join("sketch.png");
    image::RgbaImage::from_pixel(64, 32, image::Rgba([250, 0, 250, 255]))
        .save(&png)
        .unwrap();
    // decor_back's backdrop tiles cover the whole level (the sketch is
    // under them): hide that layer.
    r.steps(
        r#"[{ "ui": "focus", "level": "crevasse" }, { "ui": "layers", "visible": { "decor_back": false } }]"#,
    );
    let before = r.undo_len();
    r.steps(&format!(
        r#"[{{ "ui": "reference", "add": {:?}, "level": "crevasse", "px": [16, 16], "scale": 8, "opacity": 1 }}]"#,
        png.to_str().unwrap()
    ));
    assert_eq!(r.undo_len(), before + 1, "one entry");
    assert!(r.doc().dir.join("art/sketch.png").is_file());
    let id = r.app.st.view.references.selected.clone().expect("selected");
    let l = &r.doc().level(r.doc().level_ix("crevasse").unwrap());
    assert_eq!(l.references.len(), 1);
    assert_eq!(l.references[0].id, id);
    r.frames(4);
    let map = r.app.st.map_rect.expect("map drawn");
    let img = r.hl.read().unwrap();
    let magenta = count_near(&img, map, [250, 0, 250]);
    keep(&r.hl, "reference_behind_level.png");
    assert!(magenta > 500, "the sketch shows: {magenta} px");
    // Hidden by the pseudo-layer's eye: gone from the map, still in the file.
    r.app.st.view.references.ui.visible = false;
    r.frames(4);
    let img = r.hl.read().unwrap();
    assert!(count_near(&img, map, [250, 0, 250]) < magenta / 10);
    r.app.st.view.references.ui.visible = true;
    // Undo takes it away.
    r.app
        .run_step(
            worlds_editor::script::parse(r#"[{ "undo": 1 }]"#)
                .unwrap()
                .remove(0),
        )
        .unwrap();
    r.frames(4);
    let img = r.hl.read().unwrap();
    assert!(count_near(&img, map, [250, 0, 250]) < magenta / 10);
}

#[test]
fn layer_operations_are_one_entry_each() {
    if !gpu_tests() {
        return;
    }
    let mut r = Rig::new([1400, 800]);
    let n0 = r.undo_len();
    r.app.run(Action::DuplicateLayer(Some(worlds_core::slug(
        "decor_back",
    ))));
    r.frames(2);
    assert_eq!(r.undo_len(), n0 + 1);
    let doc = r.doc();
    let i = doc
        .project
        .layers
        .iter()
        .position(|l| l.slug == "decor_back_copy")
        .expect("the copy");
    assert_eq!(doc.project.layers[i - 1].slug, "decor_back");
    assert_eq!(
        doc.project.layers[i].name,
        format!("{} copy", doc.project.layers[i - 1].name)
    );
    assert_eq!(r.app.st.view.active_layer, Some(i));
    // Select a tile stack of decor_back in crevasse, move it to the copy.
    let ix = doc.level_ix("crevasse").unwrap();
    let level = doc.level(ix);
    let cell = level.tiles_layer("decor_back").expect("tiles").tiles[0].cell;
    let lid = level.id.clone();
    r.app.st.view.selection.set([SelItem::Tiles {
        level: lid.clone(),
        layer: worlds_core::slug("decor_back"),
        cell,
    }]);
    r.app.run(Action::MoveSelectionToLayer(None));
    r.frames(2);
    assert_eq!(r.undo_len(), n0 + 2);
    assert!(r.app.st.view.selection.contains(&SelItem::Tiles {
        level: lid,
        layer: worlds_core::slug("decor_back_copy"),
        cell,
    }));
    // Merge the copy back down.
    r.app.run(Action::MergeLayerDown(None));
    r.frames(2);
    assert_eq!(r.undo_len(), n0 + 3);
    assert!(r.doc().project.layer("decor_back_copy").is_none());
    // decor_front sits on collision: refused, nothing recorded.
    r.app.run(Action::MergeLayerDown(Some(worlds_core::slug(
        "decor_front",
    ))));
    assert_eq!(r.undo_len(), n0 + 3);
    let s = r.app.st.view.status.as_ref().expect("a status");
    assert!(s.text.contains("int_grid"), "{}", s.text);
}

#[test]
fn the_history_pane_jumps_back_and_forth() {
    if !gpu_tests() {
        return;
    }
    let mut r = Rig::new([1600, 900]);
    r.steps(
        r#"[
        { "op": "fill_rect", "level": "crevasse", "layer": "collision", "rect": [2, 2, 3, 1], "value": "solid" },
        { "op": "fill_rect", "level": "crevasse", "layer": "collision", "rect": [2, 3, 3, 1], "value": "solid" },
        { "op": "fill_rect", "level": "crevasse", "layer": "collision", "rect": [2, 4, 3, 1], "value": "solid" },
        { "ui": "focus", "level": "crevasse" },
        { "ui": "history", "expand": true }
    ]"#,
    );
    let n = r.undo_len();
    assert!(n >= 3);
    keep(&r.hl, "history_pane.png");
    assert!(
        r.app
            .st
            .pane_rects
            .contains_key(&worlds_editor::app::Pane::History)
    );
    assert_eq!(r.app.st.history.rows.len(), n);
    r.steps(&format!(r#"[{{ "ui": "history", "jump": {} }}]"#, n - 3));
    assert_eq!(r.undo_len(), n - 3);
    assert_eq!(r.app.st.history.rows.len(), n, "undone entries stay listed");
    assert_eq!(r.app.st.history.done, n - 3);
    r.app.run(Action::HistoryJump(n));
    r.frames(2);
    assert_eq!(r.undo_len(), n);
    assert!(
        worlds_editor::script::parse(r#"[{ "ui": "history", "jump": 999 }]"#)
            .map(|mut s| r.app.run_step(s.remove(0)))
            .unwrap()
            .is_err()
    );
}
