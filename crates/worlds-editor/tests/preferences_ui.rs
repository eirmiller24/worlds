//! The Preferences window through the real app offscreen: Ctrl+, (and the
//! Worlds menu) open it, Rebind captures a key, the old chord stops working and the new one
//! works, the command palette and tooltips print the new chord, a clash
//! shows a marker with a one-click fix, and the file lands in the test's
//! config dir (never the real one). Needs a GPU (or a software driver), so
//! it only runs with `WORLDS_GPU_TESTS=1`. Works on a temp copy of the
//! `game` fixture. `WORLDS_TEST_OUT=<dir>` keeps `preferences.png`.

use std::path::{Path, PathBuf};

use egui::{Key, Modifiers};
use worlds_editor::app::actions::Action;
use worlds_editor::app::fields::marked;
use worlds_editor::app::{AppOptions, EditorApp};
use worlds_editor::headless::Headless;
use worlds_editor::keymap::{Chord, Cmd};

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

fn game_copy(tmp: &Path) -> PathBuf {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/game/game.worlds");
    let dir = tmp.join("game.worlds");
    copy_dir(&src, &dir);
    let assets = src.parent().unwrap().join("assets");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&assets, tmp.join("assets")).unwrap();
    #[cfg(not(unix))]
    copy_dir(&assets, &tmp.join("assets"));
    dir
}

struct Rig {
    hl: Headless,
    ctx: egui::Context,
    app: EditorApp,
    t: f64,
}

impl Rig {
    fn frames(&mut self, events: Vec<egui::Event>, more: usize) {
        self.t += 1.0 / 60.0;
        self.hl.frame_with(&self.ctx, &mut self.app, self.t, events);
        for _ in 0..more {
            self.t += 1.0 / 60.0;
            self.hl.frame(&self.ctx, &mut self.app, self.t);
        }
    }

    fn press(&mut self, key: Key, modifiers: Modifiers) {
        let k = |pressed| egui::Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers,
        };
        self.frames(vec![k(true), k(false)], 1);
    }

    fn click(&mut self, what: &str) {
        let p = marked(&self.ctx, what)
            .unwrap_or_else(|| panic!("{what} not drawn"))
            .center();
        let b = |pressed| egui::Event::PointerButton {
            pos: p,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        self.frames(vec![egui::Event::PointerMoved(p), b(true)], 0);
        self.frames(vec![b(false)], 2);
    }

    fn steps(&mut self, json: &str) {
        for s in worlds_editor::script::parse(json).unwrap() {
            self.app.run_step(s).unwrap();
        }
        self.frames(vec![], 2);
    }
}

fn opts(prefs: &Path) -> AppOptions {
    AppOptions {
        headless: true,
        preferences: Some(prefs.to_path_buf()),
        ..Default::default()
    }
}

fn ch(s: &str) -> Chord {
    Chord::parse(s).unwrap()
}

#[test]
fn remap_undo_in_the_preferences_window() {
    if !gpu_tests() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let dir = game_copy(tmp.path());
    let prefs = tmp.path().join("config").join("preferences.json");
    let hl = Headless::new([1400, 900]).unwrap();
    let ctx = egui::Context::default();
    let mut app = EditorApp::new(
        &ctx,
        hl.gpu.clone(),
        worlds_editor::log::init(),
        opts(&prefs),
    );
    app.set_commands(Box::new(worlds_editor::edit::Editing::new()));
    app.open(&dir);
    let mut r = Rig {
        hl,
        ctx,
        app,
        t: 0.0,
    };
    r.frames(vec![], 3);
    assert!(!prefs.exists(), "nothing is written before a change");

    // Ctrl+, opens the window, its search box takes the keyboard.
    r.press(Key::Comma, Modifiers::COMMAND);
    assert!(r.app.preferences_state().open);
    r.frames(vec![egui::Event::Text("undo".into())], 2);
    assert_eq!(r.app.preferences_state().search, "undo");

    // Rebind, then Ctrl+U: the next press is the new chord.
    r.click("prefs:rebind:undo");
    assert!(r.app.preferences_state().capture.is_some());
    if let Some(dir) = std::env::var_os("WORLDS_TEST_OUT") {
        let dir = PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        r.hl.read()
            .unwrap()
            .save(dir.join("preferences.png"))
            .unwrap();
    }
    r.press(Key::U, Modifiers::COMMAND);
    assert!(r.app.preferences_state().capture.is_none());
    assert_eq!(r.app.st.keymap.chords(Cmd::Undo), &[ch("Ctrl+U")]);

    // Saved at once, under the test's config dir, only the change.
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&prefs).unwrap()).unwrap();
    assert_eq!(
        v,
        serde_json::json!({ "shortcuts": { "undo": [ch("Ctrl+U").to_string()] } })
    );

    // The palette and the tooltips print the new chord.
    let cat = Action::catalog(&r.app.st.keymap);
    let undo = cat.iter().find(|(_, _, a)| *a == Action::Undo).unwrap();
    assert_eq!(undo.1, ch("Ctrl+U").to_string());
    assert!(
        r.app
            .st
            .keymap
            .hint("Undo Paint", Cmd::Undo)
            .ends_with(&format!("({})", ch("Ctrl+U")))
    );

    // Ctrl+Z does nothing now; Ctrl+U undoes.
    r.steps(
        r#"[{ "op": "reorder_levels", "world": "main",
              "order": ["level_4", "level_3", "level_2", "level_1", "crevasse"] }]"#,
    );
    let n = r.app.undo_entries();
    assert!(n > 0);
    r.press(Key::Z, Modifiers::COMMAND);
    assert_eq!(r.app.undo_entries(), n, "the old chord is free");
    r.press(Key::U, Modifiers::COMMAND);
    assert_eq!(r.app.undo_entries(), n - 1, "the new chord undoes");

    // A clash: Redo gets Ctrl+U as well (+ adds a key). Both rows say so,
    // and one click takes Ctrl+U off the other command.
    r.steps(r#"[{ "ui": "preferences", "search": "do" }]"#);
    r.click("prefs:add:redo");
    r.press(Key::U, Modifiers::COMMAND);
    assert!(r.app.st.keymap.chords(Cmd::Redo).contains(&ch("Ctrl+U")));
    assert_eq!(r.app.st.keymap.conflicts().len(), 1);
    r.frames(vec![], 1);
    assert!(marked(&r.ctx, "prefs:conflict:undo").is_some());
    r.click("prefs:unbind_other:redo");
    assert!(r.app.st.keymap.conflicts().is_empty());
    assert_eq!(r.app.st.keymap.chords(Cmd::Undo), &[] as &[Chord]);
    assert_eq!(r.app.st.keymap.chords(Cmd::Redo).len(), 3);

    // A new session reads the file back.
    let ctx2 = egui::Context::default();
    let app2 = EditorApp::new(
        &ctx2,
        r.hl.gpu.clone(),
        worlds_editor::log::init(),
        opts(&prefs),
    );
    assert_eq!(app2.st.keymap, r.app.st.keymap);

    // Reset all: the defaults, and a file with nothing in it.
    r.click("prefs:reset_all");
    assert_eq!(r.app.st.keymap, worlds_editor::keymap::Keymap::defaults());
    assert_eq!(std::fs::read_to_string(&prefs).unwrap().trim(), "{}");
    r.press(Key::Z, Modifiers::COMMAND | Modifiers::SHIFT);
    assert_eq!(r.app.undo_entries(), n, "Ctrl+Shift+Z redoes");
    r.press(Key::Z, Modifiers::COMMAND);
    assert_eq!(r.app.undo_entries(), n - 1, "Ctrl+Z is Undo again");

    // An unknown command in a script step is an error naming it.
    let s = worlds_editor::script::parse(r#"[{ "ui": "preferences", "capture": "nope" }]"#)
        .unwrap()
        .remove(0);
    assert!(r.app.run_step(s).unwrap_err().contains("nope"));
}

#[test]
fn headless_runs_read_preferences_but_never_write_them() {
    if !gpu_tests() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let prefs = tmp.path().join("preferences.json");
    std::fs::write(&prefs, r#"{ "shortcuts": { "view.grid": ["F2"] } }"#).unwrap();
    let hl = Headless::new([1000, 700]).unwrap();
    let ctx = egui::Context::default();
    let app = EditorApp::new(
        &ctx,
        hl.gpu.clone(),
        worlds_editor::log::init(),
        AppOptions {
            preferences_read_only: true,
            ..opts(&prefs)
        },
    );
    let mut r = Rig {
        hl,
        ctx,
        app,
        t: 0.0,
    };
    assert_eq!(r.app.st.keymap.chords(Cmd::ToggleGrid), &[ch("F2")]);
    r.frames(vec![], 2);
    // The Worlds menu's last entry opens the window.
    r.click("menu:worlds");
    r.click("menu:preferences");
    assert!(r.app.preferences_state().open);
    // A change applies for the session, the file stays as it was.
    r.frames(vec![egui::Event::Text("grid".into())], 2);
    r.click("prefs:reset:view.grid");
    assert!(r.app.st.keymap.is_default(Cmd::ToggleGrid));
    assert_eq!(
        std::fs::read_to_string(&prefs).unwrap(),
        r#"{ "shortcuts": { "view.grid": ["F2"] } }"#
    );
}
