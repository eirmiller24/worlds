//! Map tabs and camera bookmarks in the editor, headless: two tabs side by
//! side on two worlds, each drawing its own world from the one shared
//! scene; a click focuses a tab, keys act in the focused one, the status
//! bar reads the tab under the pointer; focus steps aim at a tab; linked
//! cameras zoom together; the last tab can't close; bookmarks save and
//! jump (script, Ctrl+Shift+1). Needs a GPU (or a software driver), so it
//! only runs with `WORLDS_GPU_TESTS=1`.

use std::path::{Path, PathBuf};

use worlds_editor::app::actions::Action;
use worlds_editor::app::{AppOptions, EditorApp, Pane};
use worlds_editor::headless::Headless;
use worlds_editor::view::ViewportId;

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
        let p = e.unwrap().path();
        let to = to.join(p.file_name().unwrap());
        if p.is_dir() {
            copy_dir(&p, &to);
        } else {
            std::fs::copy(&p, &to).unwrap();
        }
    }
}

/// Where the PNGs go: `WORLDS_TEST_OUT`, else nowhere.
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
    _tmp: tempfile::TempDir,
}

impl Rig {
    fn new() -> Self {
        let src =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/game/game.worlds");
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("game.worlds");
        copy_dir(&src, &dir);
        let assets = src.parent().unwrap().join("assets");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&assets, tmp.path().join("assets")).unwrap();
        #[cfg(not(unix))]
        copy_dir(&assets, &tmp.path().join("assets"));
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
        app.open(&dir);
        let mut r = Rig {
            hl,
            ctx,
            app,
            t: 0.0,
            _tmp: tmp,
        };
        r.frames(vec![], 4);
        r
    }

    fn frames(&mut self, events: Vec<egui::Event>, more: usize) {
        self.t += 1.0 / 60.0;
        self.hl.frame_with(&self.ctx, &mut self.app, self.t, events);
        for _ in 0..more {
            self.t += 1.0 / 60.0;
            self.hl.frame(&self.ctx, &mut self.app, self.t);
        }
    }

    fn steps(&mut self, json: &str) {
        for s in worlds_editor::script::parse(json).unwrap() {
            self.app.run_step(s).unwrap();
        }
        self.frames(vec![], 4);
    }

    fn key(&mut self, key: egui::Key, modifiers: egui::Modifiers) {
        let k = |pressed| egui::Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers,
        };
        self.frames(vec![k(true), k(false)], 4);
    }

    fn click(&mut self, p: egui::Pos2) {
        let b = |pressed| egui::Event::PointerButton {
            pos: p,
            button: egui::PointerButton::Secondary,
            pressed,
            modifiers: Default::default(),
        };
        // A right click (it pans on a drag, else opens the menu): it
        // focuses the tab without painting anything.
        self.frames(vec![egui::Event::PointerMoved(p), b(true)], 0);
        self.frames(vec![b(false)], 0);
        self.key(egui::Key::Escape, Default::default());
    }

    fn map(&self, id: u32) -> egui::Rect {
        self.app.st.pane_rects[&Pane::Viewport(ViewportId(id))]
    }

    /// Tab `id`'s camera zoom.
    fn zoom(&self, id: u32) -> f64 {
        let st = &self.app.st;
        st.view
            .tab_camera(&st.viewport, ViewportId(id))
            .unwrap()
            .0
            .zoom
    }

    fn world_of(&self, id: u32) -> String {
        let st = &self.app.st;
        let doc = st.doc.as_ref().unwrap();
        doc.project.worlds[st.view.tab_world_ix(doc, ViewportId(id))]
            .slug
            .to_string()
    }
}

#[test]
fn two_map_tabs_side_by_side_on_two_worlds() {
    if !gpu_tests() {
        return;
    }
    let mut r = Rig::new();
    r.steps(
        r#"[
          { "op": "create_world", "slug": "dungeon", "name": "Dungeon" },
          { "op": "move_level_to_world", "level": "level_1", "world": "dungeon", "world_px": [0, 0] },
          { "ui": "focus", "level": "crevasse" },
          { "ui": "viewport", "open": { "world": "dungeon" } }
        ]"#,
    );
    // Two tabs beside each other, the new one focused, each on its world.
    let (a, b) = (r.map(1), r.map(2));
    assert!(a.max.x <= b.min.x + 1.0, "{a:?} left of {b:?}");
    assert!(a.width() > 200.0 && b.width() > 200.0, "{a:?} {b:?}");
    assert_eq!(r.app.st.view.focused, ViewportId(2));
    assert_eq!(r.world_of(1), "main");
    assert_eq!(r.world_of(2), "dungeon");
    // The scene is shared: the stats of both tabs come from one cache,
    // and both drew tiles.
    let st = &r.app.st;
    assert!(st.viewport.stats.draws > 0 || !st.viewport.stats.detail);
    keep(&r.hl, "ui_two_map_tabs.png");

    // A focus step aimed at tab 1 changes tab 1 only.
    r.steps(r#"[{ "ui": "focus", "level": "level_2", "viewport": 1 }]"#);
    let doc = r.app.st.doc.as_ref().unwrap();
    assert_eq!(
        r.app.st.view.tab_active_level(ViewportId(1)),
        doc.level_ix("level_2")
    );
    assert_eq!(r.app.st.view.focused, ViewportId(2));
    assert_eq!(r.world_of(2), "dungeon");

    // A click in tab 1 focuses it; then keys act there: Zoom in (+) zooms
    // tab 1, not tab 2.
    let z2 = r.zoom(2);
    r.click(r.map(1).center());
    assert_eq!(r.app.st.view.focused, ViewportId(1));
    let z1 = r.zoom(1);
    r.key(egui::Key::Plus, Default::default());
    assert!(r.zoom(1) > z1, "{} > {z1}", r.zoom(1));
    assert_eq!(r.zoom(2), z2);

    // The status bar reads the tab under the pointer, focused or not.
    r.frames(vec![egui::Event::PointerMoved(r.map(2).center())], 2);
    let hover = r.app.st.pointer_hover().0.cloned();
    assert!(hover.is_some(), "a hover over the unfocused tab");

    // Linked cameras zoom together.
    r.steps(r#"[{ "ui": "viewport", "link": [1, 2] }]"#);
    assert_eq!(r.zoom(1), r.zoom(2));
    r.key(egui::Key::Minus, Default::default());
    assert_eq!(r.zoom(1), r.zoom(2), "linked");
    r.steps(r#"[{ "ui": "viewport", "link": [] }]"#);
    r.key(egui::Key::Minus, Default::default());
    assert_ne!(r.zoom(1), r.zoom(2), "unlinked");

    // Bookmarks: save tab 1's view, move away, jump back (script and
    // Ctrl+Shift+1), in whatever tab is focused.
    r.steps(r#"[{ "ui": "bookmark", "save": "hall" }]"#);
    let saved = r.app.st.view.bookmarks[0].clone();
    assert_eq!(saved.world, "main");
    r.steps(r#"[{ "ui": "focus", "world": "dungeon" }]"#);
    assert_eq!(r.world_of(1), "dungeon");
    r.key(
        egui::Key::Num1,
        egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
    );
    assert_eq!(r.world_of(1), "main");
    let (cam, size) = {
        let st = &r.app.st;
        st.view.tab_camera(&st.viewport, ViewportId(1)).unwrap()
    };
    let c = cam.center(size);
    assert!(
        (c[0] - saved.camera.center[0]).abs() < 1.0 && (c[1] - saved.camera.center[1]).abs() < 1.0,
        "{c:?} vs {:?}",
        saved.camera.center
    );
    assert_eq!(cam.zoom, saved.camera.zoom);

    // Closing: tab 2 goes, the last one is refused.
    r.app.run(Action::CloseViewport(Some(ViewportId(2))));
    r.frames(vec![], 2);
    assert_eq!(r.app.st.view.tabs.len(), 1);
    r.app.run(Action::CloseViewport(None));
    assert_eq!(r.app.st.view.tabs.len(), 1);
    assert!(
        r.app.st.view.status.as_ref().is_some_and(|m| m.error),
        "refused"
    );

    // New and Duplicate from the palette's actions: a copy keeps the
    // camera and world.
    r.app.run(Action::DuplicateViewport(None));
    r.frames(vec![], 3);
    assert_eq!(r.app.st.view.tabs.len(), 2);
    let id = r.app.st.view.focused;
    assert_ne!(id, ViewportId::FIRST);
    assert_eq!(r.world_of(id.0), "main");
    assert_eq!(r.zoom(id.0), r.zoom(1));
}

/// Pixels in `r` within 40 of color `c` per channel.
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

/// Reference images draw in every tab, each tab its own world's; a
/// reference drag reads the focused tab's camera only; a Find result in
/// another world moves the focused tab there and leaves the other alone.
#[test]
fn references_and_find_follow_each_tab() {
    if !gpu_tests() {
        return;
    }
    let mut r = Rig::new();
    let magenta = r._tmp.path().join("magenta.png");
    image::RgbaImage::from_pixel(64, 32, image::Rgba([250, 0, 250, 255]))
        .save(&magenta)
        .unwrap();
    let cyan = r._tmp.path().join("cyan.png");
    image::RgbaImage::from_pixel(64, 32, image::Rgba([0, 250, 250, 255]))
        .save(&cyan)
        .unwrap();
    // Every layer hidden: the references (under the layers) show.
    r.steps(&format!(
        r#"[
          {{ "op": "create_world", "slug": "dungeon", "name": "Dungeon" }},
          {{ "op": "move_level_to_world", "level": "level_1", "world": "dungeon", "world_px": [0, 0] }},
          {{ "ui": "layers", "visible": {{ "main_tiling": false, "entities": false, "decor_front": false,
             "collision": false, "decor_back": false }} }},
          {{ "ui": "reference", "add": {:?}, "world": "main", "px": [-4000, -4000], "scale": 4, "opacity": 1 }},
          {{ "ui": "reference", "add": {:?}, "world": "dungeon", "px": [-4000, -4000], "scale": 4, "opacity": 1 }},
          {{ "ui": "focus", "world": "main", "world_rect": [-4000, -4000, 256, 128] }},
          {{ "ui": "viewport", "open": {{ "world": "dungeon" }} }},
          {{ "ui": "focus", "world": "dungeon", "world_rect": [-4000, -4000, 256, 128] }}
        ]"#,
        magenta.to_str().unwrap(),
        cyan.to_str().unwrap()
    ));
    r.frames(vec![], 4);
    let img = r.hl.read().unwrap();
    keep(&r.hl, "ui_two_tabs_references.png");
    let (a, b) = (r.map(1), r.map(2));
    let (am, ac) = (
        count_near(&img, a, [250, 0, 250]),
        count_near(&img, a, [0, 250, 250]),
    );
    let (bm, bc) = (
        count_near(&img, b, [250, 0, 250]),
        count_near(&img, b, [0, 250, 250]),
    );
    assert!(
        am > 1000 && ac == 0,
        "tab 1 shows main's reference: {am} {ac}"
    );
    assert!(bc > 1000 && bm == 0, "tab 2 shows the dungeon's: {bm} {bc}");

    // Drag main's reference in tab 1 (focused by the press) by 40 screen
    // px to the right; tab 2 (another camera) must not steer it.
    r.app
        .run(Action::SetTool(worlds_editor::tools::Tool::Select));
    r.app.st.view.references.active = true;
    r.frames(vec![], 1);
    let (cam, _) = {
        let st = &r.app.st;
        st.view.tab_camera(&st.viewport, ViewportId(1)).unwrap()
    };
    let ppp = r.ctx.pixels_per_point() as f64;
    let to_screen = |w: [f64; 2]| {
        let s = cam.world_to_screen(w);
        egui::pos2(a.min.x + (s[0] / ppp) as f32, a.min.y + (s[1] / ppp) as f32)
    };
    let start = to_screen([-3900.0, -3950.0]);
    let end = start + egui::vec2(40.0, 0.0);
    let b = |pos, pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: Default::default(),
    };
    r.frames(vec![egui::Event::PointerMoved(start), b(start, true)], 0);
    assert_eq!(r.app.st.view.focused, ViewportId(1));
    for k in 1..=4 {
        let p = start + egui::vec2(10.0 * k as f32, 0.0);
        r.frames(vec![egui::Event::PointerMoved(p)], 0);
    }
    // Mid-drag the preview follows tab 1's camera (tab 2 draws after it
    // in the same frame and must not move it).
    let d = r.app.st.view.references.drag.clone().expect("dragging");
    let want_to = -3900.0 + 40.0 * ppp / cam.zoom;
    assert!(
        (d.to[0] - want_to).abs() < 1.0,
        "preview at {:?}, want x {want_to}",
        d.to
    );
    r.frames(vec![b(end, false)], 3);
    let doc = r.app.st.doc.as_ref().unwrap();
    let px = doc.project.world("main").unwrap().references[0].px;
    let want = -4000 + (40.0 * ppp / cam.zoom).round() as i64;
    assert!(
        (px[0] - want).abs() <= 1 && px[1] == -4000,
        "{px:?}, want x {want} (zoom {})",
        cam.zoom
    );
    r.app.st.view.references.active = false;

    // Tab 2 on main too; a Find hit in the dungeon moves the focused tab
    // (1) there, not tab 2.
    r.steps(r#"[{ "ui": "focus", "world": "main", "viewport": 2 }, { "ui": "find", "query": "type:pickup" }]"#);
    let doc = r.app.st.doc.as_ref().unwrap();
    let row = r
        .app
        .st
        .find
        .found()
        .rows
        .iter()
        .find(|row| row.level.is_some_and(|ix| doc.levels[ix].world == 1))
        .cloned()
        .expect("a pickup in the dungeon (level_1)");
    worlds_editor::app::find::jump(&mut r.app.st, &row);
    r.frames(vec![], 3);
    assert_eq!(r.app.st.view.focused, ViewportId(1));
    assert_eq!(r.world_of(1), "dungeon");
    assert_eq!(r.world_of(2), "main");
}
