//! Several worlds in the editor, headless: the map shows one world at a
//! time (its levels, labels and Fit world), the toolbar's switcher and
//! the Switch world action change it, a level made active in another world
//! brings its world along, and moving a level follows it. Needs a GPU (or
//! a software driver), so it only runs with `WORLDS_GPU_TESTS=1`.

use std::path::{Path, PathBuf};

use worlds_core::Slug;
use worlds_editor::app::actions::Action;
use worlds_editor::app::{AppOptions, EditorApp};
use worlds_editor::headless::Headless;

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

    fn shift_f(&mut self) {
        let k = |pressed| egui::Event::Key {
            key: egui::Key::F,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: egui::Modifiers::SHIFT,
        };
        self.frames(vec![k(true), k(false)], 6);
    }

    fn click(&mut self, p: egui::Pos2) {
        let b = |pressed| egui::Event::PointerButton {
            pos: p,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        self.frames(vec![egui::Event::PointerMoved(p), b(true)], 0);
        self.frames(vec![b(false)], 3);
    }

    fn world(&self) -> usize {
        let doc = self.app.st.doc.as_ref().unwrap();
        self.app.st.view.world_ix(doc)
    }

    /// The levels the map draws, by slug, sorted.
    fn drawn(&self) -> Vec<String> {
        let doc = self.app.st.doc.as_ref().unwrap();
        let scene = self.app.st.viewport.scene().unwrap();
        let mut v: Vec<String> = scene
            .draw_order
            .iter()
            .map(|&ix| doc.level(ix).slug.to_string())
            .collect();
        v.sort();
        v
    }
}

#[test]
fn the_map_shows_one_world_and_the_switcher_changes_it() {
    if !gpu_tests() {
        return;
    }
    let mut r = Rig::new();
    let all = r.drawn();
    assert!(all.contains(&"level_1".to_string()), "{all:?}");
    r.steps(
        r#"[
          { "op": "create_world", "slug": "dungeon", "name": "Dungeon" },
          { "op": "move_level_to_world", "level": "level_1", "world": "dungeon", "world_px": [0, 0] },
          { "ui": "focus", "level": "crevasse" }
        ]"#,
    );
    assert_eq!(r.world(), 0);
    let main: Vec<String> = all.iter().filter(|s| *s != "level_1").cloned().collect();
    assert_eq!(r.drawn(), main);
    // Fit world: every level of main gets a label (but the active one),
    // none of the dungeon.
    r.shift_f();
    let doc = r.app.st.doc.as_ref().unwrap();
    let labelled: Vec<&str> = r
        .app
        .st
        .viewport
        .level_labels
        .iter()
        .map(|(ix, _)| doc.level(*ix).slug.as_str())
        .collect();
    assert!(!labelled.contains(&"level_1"), "{labelled:?}");
    assert_eq!(labelled.len(), main.len() - 1, "{labelled:?}");

    // The toolbar's switcher: open it, the dungeon is listed.
    let combo = worlds_editor::app::fields::marked(&r.ctx, "world switcher").expect("drawn");
    r.click(combo.center());
    keep(&r.hl, "ui_world_switcher.png");
    r.frames(
        vec![egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Default::default(),
        }],
        2,
    );

    // Switching: the dungeon alone, no active level, fitted.
    r.app
        .run(Action::SwitchWorld(Slug::new("dungeon").unwrap()));
    r.frames(vec![], 4);
    assert_eq!(r.world(), 1);
    assert_eq!(r.app.st.view.active_level, None);
    assert_eq!(r.drawn(), ["level_1"]);
    let doc = r.app.st.doc.as_ref().unwrap();
    let l1 = doc.level_ix("level_1").unwrap();
    let lr = worlds_editor::viewport::level_wrect(doc, l1);
    let size = r.app.st.viewport.size_px;
    let seen = r.app.st.viewport.camera.visible(size);
    assert!(
        seen.min[0] <= lr.min[0] && seen.max[0] >= lr.max[0],
        "{seen:?} fits {lr:?}"
    );
    keep(&r.hl, "ui_world_dungeon.png");

    // A level of main made active (a problem, the API) brings main back.
    r.steps(r#"[{ "ui": "focus", "level": "crevasse" }]"#);
    assert_eq!(r.world(), 0);
    assert_eq!(
        r.app.st.view.active_world.as_ref().map(|s| s.as_str()),
        Some("main")
    );

    // Moving a level follows it to its new world.
    r.app.run(Action::MoveLevelToWorld {
        level: Slug::new("level_2").unwrap(),
        world: Slug::new("dungeon").unwrap(),
    });
    r.frames(vec![], 4);
    assert_eq!(r.world(), 1);
    assert_eq!(r.drawn(), ["level_1", "level_2"]);
    // One undo entry puts it back in main.
    r.steps(r#"[{ "undo": 1 }]"#);
    let doc = r.app.st.doc.as_ref().unwrap();
    assert!(
        doc.project
            .world("main")
            .unwrap()
            .level("level_2")
            .is_some()
    );
}
