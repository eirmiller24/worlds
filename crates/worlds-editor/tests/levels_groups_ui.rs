//! The Levels pane, world layouts and object groups through the real app
//! offscreen: `"ui": "levels"` steps, PgDn following a reorder, a level
//! drag snapped to a grid-vania lattice and refused in a strip, a click on
//! a group member selecting the group, Ctrl+G. Needs a GPU (or a software
//! driver), so it only runs with `WORLDS_GPU_TESTS=1`. Works on a temp copy
//! of the `game` fixture.

use std::path::{Path, PathBuf};

use worlds_editor::app::actions::Action;
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

fn id(x: &str) -> worlds_core::Id {
    worlds_core::Id::new(x).unwrap()
}

fn px(app: &EditorApp, level: &str) -> [i64; 2] {
    app.document()
        .unwrap()
        .project
        .level(level)
        .unwrap()
        .world_px
}

#[test]
fn levels_layouts_and_groups_in_the_app() {
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

    // The Levels pane: a search, a click on a row.
    steps(
        &mut app,
        r#"[{ "ui": "levels", "search": "level", "sort": "name", "select": "level_2" }]"#,
    )
    .unwrap();
    for _ in 0..3 {
        frame(&mut app);
    }
    assert!(app.st.pane_rects.contains_key(&Pane::Levels));
    let doc = app.document().unwrap();
    assert_eq!(app.st.view.active_level, doc.level_ix("level_2"));
    assert_eq!(app.st.levels.rows().len(), 4);
    let e = steps(&mut app, r#"[{ "ui": "levels", "sort": "size" }]"#).unwrap_err();
    assert!(e.contains("order, name or position"), "{e}");

    // PgDn follows the level order.
    steps(
        &mut app,
        r#"[{ "op": "reorder_levels", "world": "main",
              "order": ["level_4", "level_3", "level_2", "level_1", "crevasse"] }]"#,
    )
    .unwrap();
    frame(&mut app);
    app.run(Action::NextLevel);
    let doc = app.document().unwrap();
    assert_eq!(app.st.view.active_level, doc.level_ix("level_1"));

    // Grid vania: a level drag lands on the lattice.
    steps(
        &mut app,
        r#"[{ "op": "set_world_layout", "world": "main", "layout": { "mode": "grid_vania", "cell": [320, 256] } }]"#,
    )
    .unwrap();
    frame(&mut app);
    steps(
        &mut app,
        r#"[{ "gesture": "select", "level": "level_1", "path": [[1, 1], [5, 3]] }]"#,
    )
    .unwrap();
    let p = px(&app, "level_1");
    assert_eq!([p[0] % 320, p[1] % 256], [0, 0], "{p:?}");

    // A strip: the drag is refused with a message, the strip stays.
    steps(
        &mut app,
        r#"[{ "op": "set_world_layout", "world": "main", "layout": { "mode": "horizontal" } }]"#,
    )
    .unwrap();
    frame(&mut app);
    let before = px(&app, "level_1");
    let e = steps(
        &mut app,
        r#"[{ "gesture": "select", "level": "level_1", "path": [[1, 1], [9, 3]] }]"#,
    )
    .unwrap_err();
    assert!(e.contains("horizontal layout"), "{e}");
    assert_eq!(px(&app, "level_1"), before);
    // The strip follows the order: level_4 first, at the origin.
    assert_eq!(px(&app, "level_4"), [0, 0]);

    // Groups: a press on a member selects the group; Ctrl+Shift+G
    // ungroups, Ctrl+G groups again (one entry each).
    steps(
        &mut app,
        r#"[{ "op": "set_group", "level": "level_1",
              "group": { "id": "levergrp22", "name": "Lever", "members": ["leveraaaa2", "pickupbbb2"] } }]"#,
    )
    .unwrap();
    frame(&mut app);
    steps(
        &mut app,
        r#"[{ "gesture": "select", "level": "level_1", "layer": "entities", "path": [[9, 7]], "press": true }]"#,
    )
    .unwrap();
    let sel = app.st.view.selection.items.clone();
    assert_eq!(
        sel,
        [
            SelItem::Entity(id("leveraaaa2")),
            SelItem::Entity(id("pickupbbb2"))
        ]
    );
    let n = app.undo_entries();
    app.run(Action::UngroupSelection);
    assert!(
        app.document()
            .unwrap()
            .project
            .level("level_1")
            .unwrap()
            .groups
            .is_empty()
    );
    app.run(Action::GroupSelection);
    assert_eq!(app.undo_entries(), n + 2);
    let g = &app
        .document()
        .unwrap()
        .project
        .level("level_1")
        .unwrap()
        .groups;
    assert_eq!(g.len(), 1);
    assert_eq!(g[0].members, [id("leveraaaa2"), id("pickupbbb2")]);
    // Deleting a member takes it out of the group in the same entry.
    app.st
        .view
        .selection
        .set([SelItem::Entity(id("pickupbbb2"))]);
    app.run(Action::DeleteSelection);
    let g = &app
        .document()
        .unwrap()
        .project
        .level("level_1")
        .unwrap()
        .groups;
    assert_eq!(g[0].members, [id("leveraaaa2")]);
    steps(&mut app, r#"[{ "undo": 1 }]"#).unwrap();
    let g = &app
        .document()
        .unwrap()
        .project
        .level("level_1")
        .unwrap()
        .groups;
    assert_eq!(g[0].members.len(), 2);
    for _ in 0..2 {
        frame(&mut app);
    }
}
