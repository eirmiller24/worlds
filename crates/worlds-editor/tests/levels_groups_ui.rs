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

/// The Levels pane lists the focused map tab's world and frames in that
/// tab; All worlds lists every world whatever tab is focused; the
/// bookmarks list sits under the rows.
#[test]
fn the_levels_pane_follows_the_focused_tab() {
    use worlds_editor::app::levels::Row;
    use worlds_editor::view::ViewportId;
    if !gpu_tests() {
        return;
    }
    let (_tmp, dir) = game_copy();
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
    let mut t = 0.0;
    let mut frames = |app: &mut EditorApp, n: usize| {
        for _ in 0..n {
            t += 1.0 / 60.0;
            hl.frame_with(&ctx, app, t, vec![]);
        }
    };
    frames(&mut app, 3);
    steps(
        &mut app,
        r#"[
          { "op": "create_world", "slug": "dungeon", "name": "Dungeon" },
          { "op": "move_level_to_world", "level": "level_1", "world": "dungeon", "world_px": [0, 0] },
          { "ui": "focus", "level": "crevasse" },
          { "ui": "viewport", "open": { "world": "dungeon" } },
          { "ui": "focus", "world": "dungeon", "world_rect": [-4000, -4000, 256, 128] },
          { "ui": "levels" }
        ]"#,
    )
    .unwrap();
    frames(&mut app, 4);
    assert_eq!(app.st.view.focused, ViewportId(2));
    let listed = |app: &EditorApp| -> Vec<String> {
        let doc = app.document().unwrap();
        app.st
            .levels
            .rows()
            .iter()
            .filter_map(|r| match r {
                Row::Level(ix) => Some(doc.level(*ix).slug.to_string()),
                Row::World { .. } => None,
            })
            .collect()
    };
    assert_eq!(listed(&app), ["level_1"]);
    // Framing from the pane moves the focused tab (2) only.
    let cam = |app: &EditorApp, id| {
        let st = &app.st;
        let (c, size) = st.view.tab_camera(&st.viewport, ViewportId(id)).unwrap();
        c.center(size)
    };
    let tab1 = cam(&app, 1);
    steps(&mut app, r#"[{ "ui": "levels", "frame": "level_1" }]"#).unwrap();
    frames(&mut app, 4);
    let (want, tab2) = {
        let doc = app.document().unwrap();
        let l = doc.project.level("level_1").unwrap();
        let g = doc.project.grid_size as f64;
        (
            [
                l.world_px[0] as f64 + l.size[0] as f64 * g / 2.0,
                l.world_px[1] as f64 + l.size[1] as f64 * g / 2.0,
            ],
            cam(&app, 2),
        )
    };
    assert!(
        (tab2[0] - want[0]).abs() < 2.0 && (tab2[1] - want[1]).abs() < 2.0,
        "{tab2:?} vs {want:?}"
    );
    assert_eq!(cam(&app, 1), tab1, "tab 1 didn't move");
    // Focus tab 1: the pane lists main's levels.
    steps(&mut app, r#"[{ "ui": "viewport", "focus": 1 }]"#).unwrap();
    frames(&mut app, 3);
    let main = listed(&app);
    assert!(main.contains(&"crevasse".to_string()), "{main:?}");
    assert!(!main.contains(&"level_1".to_string()), "{main:?}");
    // All worlds: both worlds, every level, focus or not.
    steps(&mut app, r#"[{ "ui": "levels", "all_worlds": true }]"#).unwrap();
    frames(&mut app, 3);
    let worlds = app
        .st
        .levels
        .rows()
        .iter()
        .filter(|r| matches!(r, Row::World { .. }))
        .count();
    assert_eq!(worlds, 2);
    assert_eq!(
        listed(&app).len(),
        app.document().unwrap().project.levels().count()
    );
    // A level of the other world from All worlds takes the focused tab there.
    steps(
        &mut app,
        r#"[{ "ui": "levels", "all_worlds": true, "select": "level_1" }]"#,
    )
    .unwrap();
    frames(&mut app, 2);
    {
        let st = &app.st;
        let doc = st.doc.as_ref().unwrap();
        assert_eq!(
            doc.project.worlds[st.view.tab_world_ix(doc, ViewportId(1))].slug,
            "dungeon"
        );
    }
    // The bookmarks list is under the rows, in the pane.
    steps(
        &mut app,
        r#"[{ "ui": "bookmark", "save": "hall" }, { "ui": "levels" }]"#,
    )
    .unwrap();
    frames(&mut app, 3);
    let pane = app.st.pane_rects[&Pane::Levels];
    let marked = |k: &str| worlds_editor::app::fields::marked(&ctx, k);
    let head = marked("levels:bookmarks").expect("drawn");
    let hall = marked("bookmark:hall").expect("listed");
    assert!(
        pane.contains(head.center()) && pane.contains(hall.center()),
        "{pane:?} {head:?} {hall:?}"
    );
    let row = marked("levels:row:level_1").expect("row");
    assert!(row.bottom() <= head.top() + 1.0, "{row:?} above {head:?}");
}
