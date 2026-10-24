//! Assemblies and transforms through the real app offscreen: Save
//! selection as assembly (the script step and Ctrl+Shift+S), the Library
//! with its thumbnails, arming and turning an assembly's ghost with the X
//! and R keys, placing it, the selection transforms and arranging levels.
//! Needs a GPU (or a software driver), so it only runs with
//! `WORLDS_GPU_TESTS=1`. Works on a temp copy of the `game` fixture.
//! `WORLDS_TEST_OUT=<dir>` keeps `library.png` and `rotated_ghost.png`.

use std::path::{Path, PathBuf};

use worlds_core::Flip;
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

fn key(k: egui::Key, modifiers: egui::Modifiers) -> Vec<egui::Event> {
    [true, false]
        .into_iter()
        .map(|pressed| egui::Event::Key {
            key: k,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers,
        })
        .collect()
}

fn steps(app: &mut EditorApp, json: &str) {
    for s in worlds_editor::script::parse(json).unwrap() {
        app.run_step(s).unwrap_or_else(|e| panic!("{e}\n{json}"));
    }
}

#[test]
fn save_an_assembly_then_place_it_turned_from_the_library() {
    if !gpu_tests() {
        return;
    }
    let (_tmp, dir) = game_copy();
    let hl = Headless::new([1400, 800]).unwrap();
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
    let mut t = 0.0;
    let mut frame = |app: &mut EditorApp, events: Vec<egui::Event>| {
        t += 1.0 / 60.0;
        hl.frame_with(&ctx, app, t, events);
    };
    for _ in 0..4 {
        frame(&mut app, vec![]);
    }
    let out = std::env::var_os("WORLDS_TEST_OUT").map(PathBuf::from);

    // Ctrl+Shift+S with nothing selected says so; with a selection it opens
    // the dialog (not Save).
    frame(
        &mut app,
        key(
            egui::Key::S,
            egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
        ),
    );
    assert!(app.st.library.dialog.is_none());
    steps(
        &mut app,
        r#"[{ "ui": "focus", "level": "crevasse" },
            { "ui": "select", "entities": ["entrymain2", "torchaaaa2"] }]"#,
    );
    frame(&mut app, vec![]);
    frame(
        &mut app,
        key(
            egui::Key::S,
            egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
        ),
    );
    assert!(app.st.library.dialog.is_some(), "the dialog is open");
    app.st.library.dialog = None;

    steps(
        &mut app,
        r#"[{ "ui": "save_assembly", "slug": "torch_pair", "name": "Torch and pickup",
              "category": "Lights/Wall", "tags": ["light"] }]"#,
    );
    let doc = app.st.doc.as_ref().unwrap();
    assert_eq!(
        doc.project.assembly("torch_pair").unwrap().entities.len(),
        2
    );
    assert!(app.st.view.library.open, "the Library shows the new one");
    for _ in 0..3 {
        frame(&mut app, vec![]);
    }
    // Its thumbnail was rendered (headless: not written into the project).
    assert_eq!(app.st.library.thumbs.rendered, 1);
    assert!(!dir.join(".worlds/thumbs").exists());
    assert!(app.st.library.rects.contains_key("torch_pair"));
    if let Some(o) = &out {
        std::fs::create_dir_all(o).unwrap();
        hl.read().unwrap().save(o.join("library.png")).unwrap();
    }

    // A click on it arms the ghost; X mirrors it, R turns it.
    let row = app.st.library.rects["torch_pair"];
    let click = |pressed| egui::Event::PointerButton {
        pos: row.center(),
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: Default::default(),
    };
    frame(
        &mut app,
        vec![egui::Event::PointerMoved(row.center()), click(true)],
    );
    frame(&mut app, vec![click(false)]);
    frame(&mut app, vec![]);
    let g = app.st.view.paste.as_ref().expect("armed");
    assert_eq!(g.assembly.as_ref().map(|s| s.as_str()), Some("torch_pair"));
    steps(
        &mut app,
        r#"[{ "ui": "library", "arm": "torch_pair", "level": "crevasse", "origin": [6, 10] }]"#,
    );
    let map = app.st.map_rect.unwrap();
    frame(&mut app, vec![egui::Event::PointerMoved(map.left_top())]);
    frame(&mut app, key(egui::Key::X, egui::Modifiers::NONE));
    frame(&mut app, key(egui::Key::R, egui::Modifiers::NONE));
    frame(&mut app, vec![]);
    let g = app.st.view.paste.as_ref().unwrap();
    assert_eq!(
        g.transform.flip,
        worlds_core::transform::then(Some(Flip::X), Flip::XT)
    );
    assert_eq!(
        app.st.view.tool,
        worlds_editor::tools::Tool::default(),
        "R turned the ghost, not the Rectangle mode"
    );
    if let Some(o) = &out {
        hl.read()
            .unwrap()
            .save(o.join("rotated_ghost.png"))
            .unwrap();
    }
    let count = |app: &EditorApp| {
        app.st
            .doc
            .as_ref()
            .unwrap()
            .project
            .level("crevasse")
            .unwrap()
            .all_entities()
            .count()
    };
    let before = count(&app);
    steps(
        &mut app,
        r#"[{ "ui": "library", "level": "crevasse", "origin": [6, 10], "place": true }]"#,
    );
    assert_eq!(count(&app), before + 2);
    assert!(app.st.view.paste.is_none());

    // The selection transforms and arranging levels, as script steps.
    steps(
        &mut app,
        r#"[{ "ui": "select", "entities": ["torchaaaa2", "pickupaaa2"] },
            { "ui": "transform", "op": "mirror_x" },
            { "ui": "arrange", "op": "align_top", "levels": ["level_1", "level_2"] }]"#,
    );
    let doc = app.st.doc.as_ref().unwrap();
    let y = |s: &str| doc.project.level(s).unwrap().world_px[1];
    assert_eq!(y("level_1"), y("level_2"));
    // Nothing to turn is an error.
    let e = app
        .run_step(
            worlds_editor::script::parse(r#"[{ "ui": "turn", "keys": "x" }]"#)
                .unwrap()
                .remove(0),
        )
        .unwrap_err();
    assert!(e.contains("nothing to turn"), "{e}");
}
