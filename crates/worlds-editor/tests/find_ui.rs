//! The Find pane through the real app offscreen: `"ui": "find"` steps
//! select every result across levels and Replace as one undo entry, the
//! pane draws, a bad query is an error. Needs a GPU (or a software
//! driver), so it only runs with `WORLDS_GPU_TESTS=1`. Works on a temp
//! copy of the `game` fixture.

use std::path::{Path, PathBuf};

use worlds_editor::app::{AppOptions, EditorApp, Pane};
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

fn steps(app: &mut EditorApp, json: &str) -> Result<(), String> {
    for s in worlds_editor::script::parse(json).map_err(|e| format!("{e:#}"))? {
        app.run_step(s)?;
    }
    Ok(())
}

fn item(app: &EditorApp, id: &str) -> Option<String> {
    let doc = app.document().unwrap();
    let id = worlds_core::Id::new(id).unwrap();
    let (_, _, e) = doc.find_entity(&id).unwrap();
    e.fields
        .get("item")
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

#[test]
fn find_selects_across_levels_and_replaces_in_one_entry() {
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
    let mut frame = |app: &mut EditorApp| {
        t += 1.0 / 60.0;
        hl.frame_with(&ctx, app, t, vec![]);
    };
    for _ in 0..3 {
        frame(&mut app);
    }
    // Two pickups, in crevasse and level_1: both selected.
    steps(
        &mut app,
        r#"[{ "ui": "find", "query": "type:pickup", "select_all": true }]"#,
    )
    .unwrap();
    let sel = &app.st.view.selection.items;
    assert_eq!(sel.len(), 2, "{sel:?}");
    assert!(sel.contains(&SelItem::Entity(
        worlds_core::Id::new("pickupaaa2").unwrap()
    )));
    assert!(sel.contains(&SelItem::Entity(
        worlds_core::Id::new("pickupbbb2").unwrap()
    )));
    for _ in 0..3 {
        frame(&mut app);
    }
    assert!(app.st.pane_rects.contains_key(&Pane::Find));
    assert_eq!(app.st.find.found().total, 2);

    // Replace: one entry for both; undo puts both back.
    let before = app.undo_entries();
    steps(
        &mut app,
        r#"[{ "ui": "find", "query": "type:pickup", "replace": { "field": "item", "value": "key" } }]"#,
    )
    .unwrap();
    assert_eq!(app.undo_entries(), before + 1);
    assert_eq!(item(&app, "pickupaaa2").as_deref(), Some("key"));
    assert_eq!(item(&app, "pickupbbb2").as_deref(), Some("key"));
    frame(&mut app);
    // The rows follow the edit.
    steps(&mut app, r#"[{ "ui": "find", "query": "item:key" }]"#).unwrap();
    assert_eq!(app.st.find.found().total, 2);
    steps(&mut app, r#"[{ "undo": 1 }]"#).unwrap();
    assert_ne!(item(&app, "pickupaaa2").as_deref(), Some("key"));

    // Errors.
    let e = steps(&mut app, r#"[{ "ui": "find", "query": "kind:door" }]"#).unwrap_err();
    assert!(e.contains("annotation kinds"), "{e}");
    let e = steps(
        &mut app,
        r#"[{ "ui": "find", "query": "type:door", "kind": "levels" }]"#,
    )
    .unwrap_err();
    assert!(e.contains("type: is for entities"), "{e}");
    let e = steps(
        &mut app,
        r#"[{ "ui": "find", "query": "type:nothing_like_it", "select_all": true }]"#,
    )
    .unwrap_err();
    assert!(e.contains("nothing to select"), "{e}");
    let e = steps(
        &mut app,
        r#"[{ "ui": "find", "query": "type:pickup", "replace": { "field": "channel", "value": "1" } }]"#,
    )
    .unwrap_err();
    assert!(e.contains("channel"), "{e}");
}
