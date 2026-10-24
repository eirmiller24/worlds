//! The tiles palette's tileset picker through the real app offscreen: the
//! button names the layer's tileset, its popup lists the tilesets with a
//! search box, a click or Enter picks one (remembered per layer), and Edit…
//! opens the Project pane on that tileset. Needs a GPU (or a software
//! driver), so it only runs with `WORLDS_GPU_TESTS=1`. Works on a temp copy
//! of the `game` fixture. `WORLDS_TEST_OUT=<dir>` keeps `tileset_picker.png`.

use std::path::{Path, PathBuf};

use worlds_editor::app::project_pane::Section;
use worlds_editor::app::{AppOptions, EditorApp, Pane};
use worlds_editor::headless::Headless;

fn gpu_tests() -> bool {
    let on = std::env::var_os("WORLDS_GPU_TESTS").is_some();
    if !on {
        eprintln!("skipped: set WORLDS_GPU_TESTS=1");
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

fn click(pos: egui::Pos2) -> Vec<egui::Event> {
    [true, false]
        .into_iter()
        .map(|pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        })
        .collect()
}

fn key(k: egui::Key) -> Vec<egui::Event> {
    [true, false]
        .into_iter()
        .map(|pressed| egui::Event::Key {
            key: k,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: Default::default(),
        })
        .collect()
}

fn steps(app: &mut EditorApp, json: &str) {
    for s in worlds_editor::script::parse(json).unwrap() {
        app.run_step(s).unwrap_or_else(|e| panic!("{e}\n{json}"));
    }
}

#[test]
fn the_tileset_picker_lists_searches_picks_and_edits() {
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
    // A tiles layer active: the palette shows its tileset button.
    steps(&mut app, r#"[{ "ui": "layers", "active": "decor_back" }]"#);
    for _ in 0..4 {
        frame(&mut app, vec![]);
    }
    let button = app.st.palette_rects["tileset button"];
    assert_eq!(
        app.st.palette_tab.get("decor_back").map(String::as_str),
        Some("tiles"),
        "the layer's default tileset"
    );
    assert!(
        !app.st.palette_rects.contains_key("tileset row:props"),
        "the list is in the popup, not the pane"
    );

    // Click: the popup lists every tileset; a click on a row picks it.
    frame(&mut app, click(button.center()));
    for _ in 0..8 {
        frame(&mut app, vec![]);
    }
    let props = app.st.palette_rects["tileset row:props"];
    assert!(app.st.palette_rects.contains_key("tileset row:tiles"));
    if let Some(o) = std::env::var_os("WORLDS_TEST_OUT").map(PathBuf::from) {
        std::fs::create_dir_all(&o).unwrap();
        hl.read()
            .unwrap()
            .save(o.join("tileset_picker.png"))
            .unwrap();
    }
    frame(&mut app, click(props.center()));
    frame(&mut app, vec![]);
    assert_eq!(
        app.st.palette_tab.get("decor_back").map(String::as_str),
        Some("props")
    );

    // The popup closed with the pick: no rows last frame.
    assert!(!app.st.palette_rects.contains_key("tileset row:props"));

    // Type to search, Enter takes the first match.
    let button = app.st.palette_rects["tileset button"];
    frame(&mut app, click(button.center()));
    frame(&mut app, vec![]);
    frame(&mut app, vec![egui::Event::Text("til".into())]);
    frame(&mut app, vec![]);
    assert!(app.st.palette_rects.contains_key("tileset row:tiles"));
    assert!(
        !app.st.palette_rects.contains_key("tileset row:props"),
        "props doesn't match 'til'"
    );
    frame(&mut app, key(egui::Key::Enter));
    frame(&mut app, vec![]);
    assert_eq!(
        app.st.palette_tab.get("decor_back").map(String::as_str),
        Some("tiles")
    );

    // Edit…: the Project pane opens on that tileset.
    let button = app.st.palette_rects["tileset button"];
    frame(&mut app, click(button.center()));
    frame(&mut app, vec![]);
    let edit = app.st.palette_rects["tileset edit"];
    frame(&mut app, click(edit.center()));
    frame(&mut app, vec![]);
    assert_eq!(app.st.project.section, Section::Tilesets);
    assert_eq!(
        app.st.project.tileset.as_ref().map(|s| s.as_str()),
        Some("tiles")
    );
    assert!(app.st.pane_rects.contains_key(&Pane::Project));
}
