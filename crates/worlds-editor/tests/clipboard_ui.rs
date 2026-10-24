//! Copy and paste through the real app offscreen: the clipboard events
//! the windowing layer sends (Copy, Paste with the clipboard's text), the
//! ghost following the pointer, a click landing it, Escape cancelling it.
//! Needs a GPU (or a software driver), so it only runs with
//! `WORLDS_GPU_TESTS=1`. Works on a temp copy of the `game` fixture.

use std::path::{Path, PathBuf};

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

#[test]
fn copy_then_paste_follows_the_pointer_and_lands_on_a_click() {
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
    for s in worlds_editor::script::parse(
        r#"[{ "ui": "focus", "level": "crevasse" }, { "ui": "select", "entities": ["torchaaaa2"] }]"#,
    )
    .unwrap()
    {
        app.run_step(s).unwrap();
    }
    for _ in 0..3 {
        frame(&mut app, vec![]);
    }
    let torches = |app: &EditorApp| {
        let doc = app.st.doc.as_ref().unwrap();
        doc.project
            .levels()
            .flat_map(|l| l.all_entities())
            .filter(|e| e.ty == "torch")
            .count()
    };
    let before = torches(&app);

    // Ctrl+C arrives as a Copy event; what it copies is what a paste of
    // the clipboard's text brings back.
    frame(&mut app, vec![egui::Event::Copy]);
    let text = {
        let doc = app.st.doc.as_ref().unwrap();
        let c = worlds_editor::clipboard::copy_selection(doc, &app.st.view).unwrap();
        serde_json::to_string(&c.fragment).unwrap()
    };
    frame(&mut app, vec![egui::Event::Paste(text.clone())]);
    assert!(app.st.view.paste.is_some(), "the ghost is armed");

    // The ghost follows the pointer over the level.
    let map = app.st.map_rect.expect("map drawn");
    let p = map.center();
    frame(&mut app, vec![egui::Event::PointerMoved(p)]);
    frame(&mut app, vec![]);
    let at = app
        .st
        .view
        .paste
        .as_ref()
        .unwrap()
        .at
        .expect("over a level");
    assert!(app.st.view.paste.as_ref().unwrap().ghost.is_some());
    // A click lands it: one more torch, selected.
    let b = |pressed| egui::Event::PointerButton {
        pos: p,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: Default::default(),
    };
    frame(&mut app, vec![b(true)]);
    frame(&mut app, vec![b(false)]);
    frame(&mut app, vec![]);
    assert!(app.st.view.paste.is_none(), "landed");
    assert_eq!(torches(&app), before + 1);
    let sel: Vec<_> = app.st.view.selection.entities().cloned().collect();
    assert_eq!(sel.len(), 1);
    assert!(!app.st.view.selection.contains(&SelItem::Entity(
        worlds_core::Id::new("torchaaaa2").unwrap()
    )));
    assert_eq!(app.st.view.active_level, Some(at.0));

    // Paste again, then Escape: nothing more.
    frame(&mut app, vec![egui::Event::Paste(text)]);
    assert!(app.st.view.paste.is_some());
    let esc = |pressed| egui::Event::Key {
        key: egui::Key::Escape,
        physical_key: None,
        pressed,
        repeat: false,
        modifiers: Default::default(),
    };
    frame(
        &mut app,
        vec![egui::Event::PointerMoved(p), esc(true), esc(false)],
    );
    frame(&mut app, vec![]);
    assert!(app.st.view.paste.is_none(), "cancelled");
    assert_eq!(torches(&app), before + 1);
    // Text that isn't a fragment says so.
    frame(&mut app, vec![egui::Event::Paste("hello".into())]);
    assert!(app.st.view.paste.is_none());
}
