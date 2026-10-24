//! Level thumbnails through the real app offscreen, headless exactly as
//! `--screenshot` runs it: zoomed out past `DETAIL_CELL_PX` every level on
//! the map is its thumbnail, the same texture the Levels pane shows (one
//! render per level however many panes and map tabs ask), a void shows the
//! map through, and an edit redraws only its level. Needs a GPU (or a
//! software driver), so it only runs with `WORLDS_GPU_TESTS=1`. Works on a
//! temp copy of the `game` fixture (grid 32, five levels in `main`).

use std::path::{Path, PathBuf};

use worlds_editor::app::{AppOptions, EditorApp};
use worlds_editor::headless::Headless;
use worlds_editor::view::ViewportId;
use worlds_editor::viewport::DETAIL_CELL_PX;

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

fn steps(app: &mut EditorApp, json: &str) {
    for s in worlds_editor::script::parse(json).unwrap() {
        app.run_step(s).unwrap();
    }
}

struct Rig {
    hl: Headless,
    ctx: egui::Context,
    app: EditorApp,
    t: f64,
}

impl Rig {
    fn new(dir: &Path) -> Self {
        let hl = Headless::new([1600, 900]).unwrap();
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
        app.open(dir);
        Rig {
            hl,
            ctx,
            app,
            t: 0.0,
        }
    }

    fn frames(&mut self, n: usize) -> image::RgbaImage {
        for _ in 0..n {
            self.hl.frame(&self.ctx, &mut self.app, self.t);
            self.t += 1.0 / 60.0;
        }
        let img = self.hl.read().unwrap();
        if let Some(d) = std::env::var_os("WORLDS_TEST_OUT") {
            std::fs::create_dir_all(&d).unwrap();
            img.save(PathBuf::from(d).join("thumbs_zoomed_out.png"))
                .unwrap();
        }
        img
    }

    /// Zoom the live tab out (about the middle) until levels draw flat.
    fn zoom_out(&mut self) {
        let grid = 32.0;
        while self.app.st.viewport.camera.zoom * grid >= DETAIL_CELL_PX {
            self.app.st.viewport.zoom_steps_centered(-1);
        }
    }

    /// Level `slug`'s box on screen in the live tab, `[x0, y0, x1, y1)` of
    /// project cells inside it.
    fn cells_on_screen(&self, slug: &str, c: [f64; 4]) -> egui::Rect {
        let doc = self.app.document().unwrap();
        let o = doc.project.level(slug).unwrap().world_px;
        let map = self.app.st.map_rect.unwrap();
        let cam = self.app.st.viewport.camera;
        let at = |x: f64, y: f64| {
            let s = cam.world_to_screen_snapped([o[0] as f64 + x * 32.0, o[1] as f64 + y * 32.0]);
            egui::pos2(map.min.x + s[0] as f32, map.min.y + s[1] as f32)
        };
        egui::Rect::from_min_max(at(c[0], c[1]), at(c[2], c[3]))
    }
}

/// How many pixels inside `r` pass `f` (and how many there are).
fn count(img: &image::RgbaImage, r: egui::Rect, f: impl Fn([u8; 4]) -> bool) -> (usize, usize) {
    let (mut n, mut all) = (0, 0);
    for y in r.min.y.max(0.0) as u32..(r.max.y as u32).min(img.height()) {
        for x in r.min.x.max(0.0) as u32..(r.max.x as u32).min(img.width()) {
            all += 1;
            n += f(img.get_pixel(x, y).0) as usize;
        }
    }
    (n, all)
}

/// The map's background (`viewport::Viewport::ui`'s fill).
const MAP_BG: [u8; 4] = [24, 24, 24, 255];

#[test]
fn zoomed_out_levels_are_their_thumbnails_in_every_tab() {
    if !gpu_tests() {
        return;
    }
    let (_tmp, dir) = game_copy();
    let mut r = Rig::new(&dir);
    // Framed on crevasse, the Levels pane up, level_2 active (so crevasse
    // is drawn plainly: no active hatching over its void below).
    steps(
        &mut r.app,
        r#"[{ "ui": "focus", "level": "crevasse" },
            { "ui": "levels", "select": "level_2" }]"#,
    );
    r.frames(3);
    // The pane's five rows made five pictures; zoomed in, the map drew none.
    assert_eq!(r.app.st.thumbs.rendered, 5);
    assert!(r.app.st.viewport.stats.detail);
    assert_eq!(r.app.st.viewport.stats.thumbs, 0);

    // Zoomed out past the switch: every level is its thumbnail, the same
    // five pictures (nothing drawn again).
    r.zoom_out();
    let with = r.frames(2);
    let st = &r.app.st.viewport.stats;
    assert!(!st.detail);
    assert_eq!(st.thumbs, 5, "{st:?}");
    assert_eq!(
        r.app.st.thumbs.rendered, 5,
        "the pane and the map share them"
    );

    // The thumbnail is the level's art, not the flat fill: with the
    // pictures gone (and none allowed) most of crevasse's box changes.
    r.app.st.thumbs.clear();
    r.app.st.thumbs.budget.pictures = Some(0);
    let flat = r.frames(2);
    assert_eq!(r.app.st.viewport.stats.thumbs, 0);
    let body = r.cells_on_screen("crevasse", [1.0, 1.0, 63.0, 27.0]);
    let (mut differ, mut all) = (0, 0);
    for y in body.min.y as u32..body.max.y as u32 {
        for x in body.min.x as u32..body.max.x as u32 {
            all += 1;
            differ += (with.get_pixel(x, y) != flat.get_pixel(x, y)) as usize;
        }
    }
    assert!(
        all > 1000 && differ * 10 > all * 3,
        "{differ} of {all} pixels"
    );
    r.app.st.thumbs.budget = worlds_editor::thumbs::Budget::UNLIMITED;
    r.frames(1);
    assert_eq!(r.app.st.thumbs.rendered, 10);

    // A void cut out of crevasse: only crevasse is drawn again, and the map
    // shows through its void.
    let void = r.cells_on_screen("crevasse", [1.0, 1.0, 15.0, 7.0]);
    let (bg_before, n) = count(&r.frames(1), void, |p| p == MAP_BG);
    assert!(bg_before * 20 < n, "{bg_before} of {n} were map before");
    steps(
        &mut r.app,
        r#"[{ "op": "set_level_void", "level": "crevasse", "void": [[0, 0, 16, 8]] }]"#,
    );
    let img = r.frames(2);
    assert_eq!(r.app.st.thumbs.rendered, 11, "crevasse only");
    let (bg, n) = count(&img, void, |p| p == MAP_BG);
    assert!(bg * 20 >= n * 19, "{bg} of {n} void pixels are map");
    // ... while the rest of it is still its picture.
    let (bg, n) = count(
        &img,
        body.intersect(r.cells_on_screen("crevasse", [20.0, 10.0, 63.0, 27.0])),
        |p| p == MAP_BG,
    );
    assert!(bg * 20 < n, "{bg} of {n}");

    // A second map tab on the same world, zoomed out too: it draws the
    // same textures, so nothing is rendered again.
    steps(&mut r.app, r#"[{ "ui": "viewport", "open": {} }]"#);
    r.frames(2);
    for id in [1, 2] {
        r.app.st.in_tab(ViewportId(id), |st| {
            while st.viewport.camera.zoom * 32.0 >= DETAIL_CELL_PX {
                st.viewport.zoom_steps_centered(-1);
            }
        });
    }
    r.frames(2);
    for id in [1, 2] {
        let s = r.app.st.in_tab(ViewportId(id), |st| st.viewport.stats);
        assert!(!s.detail && s.thumbs >= 1, "tab {id}: {s:?}");
    }
    assert_eq!(r.app.st.thumbs.rendered, 11, "the tabs share the pictures");
}
