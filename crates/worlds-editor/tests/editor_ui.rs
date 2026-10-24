//! The editor's own UI driven offscreen through the real app (keys, clicks,
//! frames): a note's title takes the keyboard when the Note tool adds it, so
//! typing never fires tool shortcuts; the Paint and Note strips never cover
//! the active level's name tab; zoomed out, every level shows its name;
//! the `vertex` and connector script gestures edit geometry.
//! Need a GPU (or a software driver), so they only run with
//! `WORLDS_GPU_TESTS=1`. They work on temp copies of the `game` fixture.

use std::path::{Path, PathBuf};

use worlds_editor::app::{AppOptions, EditorApp};
use worlds_editor::headless::Headless;
use worlds_editor::tools::Tool;
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

/// A temp copy of the `game` fixture, its tileset art (`../assets`, read
/// only) linked next to it.
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
    fn new(size: [u32; 2]) -> Self {
        let (tmp, dir) = game_copy();
        let hl = Headless::new(size).unwrap();
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

    /// `events` in one frame, then `more` empty frames.
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

    /// A key typed as a window sends it: the key, and its text.
    fn type_key(&mut self, key: egui::Key, text: &str) {
        let k = |pressed| egui::Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: Default::default(),
        };
        let mut events = vec![k(true)];
        if !text.is_empty() {
            events.push(egui::Event::Text(text.into()));
        }
        events.push(k(false));
        self.frames(events, 2);
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
}

/// The Note tool's click adds a note and gives its title in the Inspector
/// the keyboard, the text selected: typing replaces it, and the letters
/// that are tool shortcuts (E: Eraser, T: entity search) don't switch
/// tools. With the keyboard back on the map, E is the Eraser again.
#[test]
fn a_new_notes_title_takes_the_keyboard_from_the_shortcuts() {
    if !gpu_tests() {
        return;
    }
    let mut r = Rig::new([1600, 900]);
    r.steps(r#"[{ "ui": "focus", "level": "crevasse" }]"#);
    // The Types tab in front: the note brings the Inspector back.
    r.steps(r#"[{ "ui": "types", "entity": "torch" }]"#);
    r.type_key(egui::Key::N, "n");
    assert_eq!(r.app.st.view.tool, Tool::Note);
    let map = r.app.st.map_rect.expect("map drawn");
    r.click(map.center());
    let notes: Vec<_> = r.app.st.view.selection.notes().cloned().collect();
    let [note] = &notes[..] else {
        panic!("one new note selected: {notes:?}")
    };
    assert!(
        r.ctx.egui_wants_keyboard_input(),
        "the title has the keyboard"
    );
    for (k, c) in [
        (egui::Key::E, "e"),
        (egui::Key::T, "t"),
        (egui::Key::B, "b"),
    ] {
        r.type_key(k, c);
        assert_eq!(r.app.st.view.tool, Tool::Note, "{c} switched tools");
    }
    keep(&r.hl, "ui_note_title_focused.png");
    r.type_key(egui::Key::Enter, "");
    let doc = r.app.st.doc.as_ref().unwrap();
    let a = worlds_editor::annot::find_note(doc, note).expect("the note");
    assert_eq!(a.note.title.as_deref(), Some("etb"), "typed over \"Note\"");
    assert!(
        r.app
            .st
            .view
            .selection
            .contains(&SelItem::Note(note.clone()))
    );
    // Enter gave the keyboard back: E is a shortcut again.
    assert!(!r.ctx.egui_wants_keyboard_input());
    r.type_key(egui::Key::E, "e");
    assert_eq!(r.app.st.view.tool, Tool::Eraser);
}

/// The Paint mode strip and the Note options strip float over the map's
/// top left; the active level's name tab steps aside from them (the
/// tutorial's framed level put its tab right under the Paint strip), at a
/// wide and a narrow window, with the level framed and scrolled to the
/// map's corner.
#[test]
fn tool_strips_never_cover_the_level_tab() {
    if !gpu_tests() {
        return;
    }
    for size in [[1600, 900], [900, 560]] {
        let mut r = Rig::new(size);
        for focus in [
            r#"[{ "ui": "focus", "level": "crevasse" }]"#,
            r#"[{ "ui": "focus", "level": "level_1" }]"#,
            // Crevasse's top left corner (0, -1920) just inside the map's:
            // its tab where the strips are.
            r#"[{ "ui": "focus", "world_rect": [-20, -1960, 700, 400] }]"#,
            r#"[{ "ui": "focus", "world_rect": [0, -1990, 1200, 680] }]"#,
            r#"[{ "ui": "focus", "world_rect": [2000, -1330, 900, 500] }]"#,
        ] {
            r.steps(focus);
            for (key, text, strip) in [
                (egui::Key::B, "b", "paint tool strip"),
                (egui::Key::G, "g", "paint tool strip"),
                (egui::Key::N, "n", "note tool strip"),
            ] {
                r.type_key(key, text);
                r.frames(vec![], 20);
                let tab = r.app.st.viewport.level_tab.expect("the tab");
                let strip_rect = r
                    .ctx
                    .memory(|m| m.area_rect(egui::Id::new(strip)))
                    .expect("the strip");
                assert!(
                    !tab.intersects(strip_rect),
                    "{size:?} {focus} {text}: tab {tab:?} under {strip} {strip_rect:?}"
                );
                let map = r.app.st.map_rect.unwrap();
                assert!(map.contains_rect(tab), "{size:?} {focus}: {tab:?}");
            }
            r.type_key(egui::Key::V, "v");
        }
        r.steps(r#"[{ "ui": "focus", "level": "crevasse" }]"#);
        r.type_key(egui::Key::B, "b");
        r.frames(vec![], 20);
        keep(&r.hl, &format!("ui_paint_strip_{}.png", size[0]));
    }
}

/// Zoomed out on the world, with tiles drawn, every level shows its name,
/// not just the active one: a label in each other level's corner that fits,
/// none over another or over the active tab.
#[test]
fn zoomed_out_every_level_shows_its_name() {
    if !gpu_tests() {
        return;
    }
    let mut r = Rig::new([1600, 900]);
    r.steps(r#"[{ "ui": "focus", "level": "crevasse" }]"#);
    r.type_key(egui::Key::F, "F");
    // Shift+F: the whole world.
    let k = |pressed| egui::Event::Key {
        key: egui::Key::F,
        physical_key: None,
        pressed,
        repeat: false,
        modifiers: egui::Modifiers::SHIFT,
    };
    r.frames(vec![k(true), k(false)], 6);
    let doc = r.app.st.doc.as_ref().unwrap();
    let n = doc.levels.len();
    let active = r.app.st.view.active_level.expect("an active level");
    let labels = r.app.st.viewport.level_labels.clone();
    let tab = r.app.st.viewport.level_tab.expect("the tab");
    let mut shown: Vec<usize> = labels.iter().map(|(ix, _)| *ix).collect();
    shown.sort_unstable();
    let others: Vec<usize> = (0..n).filter(|&i| i != active).collect();
    assert_eq!(shown, others, "every other level labelled");
    for (i, (_, a)) in labels.iter().enumerate() {
        assert!(!a.intersects(tab), "{a:?} over the tab {tab:?}");
        for (_, b) in &labels[i + 1..] {
            assert!(!a.intersects(*b), "{a:?} over {b:?}");
        }
    }
    keep(&r.hl, "ui_world_level_names.png");
}

/// The Rules pane renames in place: a double click on a rule's slug opens
/// a box over it, typing and Enter rename it (one undo entry, the
/// selection following), and a slug another rule has is refused with the
/// reason in the box and the status bar. Undo puts the old slug back.
#[test]
fn rules_pane_renames_a_rule_inline() {
    if !gpu_tests() {
        return;
    }
    let mut r = Rig::new([1600, 900]);
    r.steps(
        r#"[{ "ui": "rules", "layer": "main_tiling", "level": "crevasse", "rule": "wall_fill" }]"#,
    );
    let slugs = |r: &Rig| -> Vec<String> {
        let doc = r.app.st.doc.as_ref().unwrap();
        doc.project.rules_for("main_tiling").unwrap().groups[0]
            .rules
            .iter()
            .map(|e| e.slug().to_string())
            .collect()
    };
    let before = slugs(&r);
    let at = |r: &Rig| {
        r.app.rules_state().slug_rects[&(false, worlds_core::Slug::new("wall_fill").unwrap())]
            .center()
    };
    let p = at(&r);
    r.click(p);
    r.click(p);
    r.frames(vec![], 3);
    assert!(
        r.app.rules_state().renaming.is_some(),
        "double click opens the box"
    );
    // Taken: refused, the box stays with the reason.
    r.frames(vec![egui::Event::Text("wall_top".into())], 1);
    r.type_key(egui::Key::Enter, "");
    let rn = r
        .app
        .rules_state()
        .renaming
        .clone()
        .expect("still renaming");
    assert!(
        rn.error
            .as_deref()
            .is_some_and(|e| e.contains("already has wall_top")),
        "{rn:?}"
    );
    assert_eq!(slugs(&r), before);
    // A free slug: renamed in place, one entry.
    let n = r.app.undo_entries();
    r.frames(vec![egui::Event::Text("wall_inner".into())], 1);
    keep(&r.hl, "ui_rules_rename.png");
    r.type_key(egui::Key::Enter, "");
    r.frames(vec![], 2);
    assert!(r.app.rules_state().renaming.is_none());
    let after = slugs(&r);
    let i = before.iter().position(|s| s == "wall_fill").unwrap();
    assert_eq!(after[i], "wall_inner");
    assert_eq!(after.len(), before.len());
    assert_eq!(r.app.undo_entries(), n + 1);
    assert_eq!(
        r.app.rules_state().selected.as_ref().map(|s| s.as_str()),
        Some("wall_inner")
    );
    keep(&r.hl, "ui_rules_renamed.png");
    r.steps(r#"[{ "undo": 1 }]"#);
    assert_eq!(slugs(&r), before);
}

/// Geometry editing and connectors through the real app and `--script`:
/// the `vertex` gesture drags a path's point, Alt+clicks a new one in and
/// deletes it again; a `note` gesture of kind
/// `connector` joins two rooms through a waypoint. Pictures (with
/// `WORLDS_TEST_OUT`): `geometry_handles.png` (the path selected, its
/// handles), `geometry_connector.png` (the connector selected).
#[test]
fn vertex_and_connector_script_gestures() {
    if !gpu_tests() {
        return;
    }
    let mut r = Rig::new([1280, 800]);
    r.steps(
        r#"[
          { "op": "add_note", "scope": { "world": "main" },
            "note": { "id": "pathgeoaaa", "kind": "path", "title": "Route",
                      "points": [[2100, -1200], [2400, -1000], [2600, -1200]] } },
          { "ui": "focus", "world_rect": [1900, -1500, 2700, 1100] }
        ]"#,
    );
    let points = |r: &Rig| {
        let doc = r.app.document().unwrap();
        let id = worlds_core::Id::new("pathgeoaaa").unwrap();
        worlds_editor::annot::find_note(doc, &id)
            .unwrap()
            .note
            .shape
            .points()
    };
    r.steps(r#"[{ "gesture": "vertex", "note": "pathgeoaaa", "vertex": 1, "world_px": [[2400, -900]] }]"#);
    assert_eq!(points(&r), [[2100, -1200], [2400, -896], [2600, -1200]]);
    r.steps(r#"[{ "gesture": "vertex", "note": "pathgeoaaa", "insert": [2250, -1048] }]"#);
    assert_eq!(
        points(&r),
        [[2100, -1200], [2256, -1056], [2400, -896], [2600, -1200]]
    );
    keep(&r.hl, "geometry_handles.png");
    r.steps(r#"[{ "gesture": "vertex", "note": "pathgeoaaa", "vertex": 1, "delete": true }]"#);
    assert_eq!(points(&r), [[2100, -1200], [2400, -896], [2600, -1200]]);
    r.steps(
        r#"[{ "gesture": "note", "kind": "connector",
              "world_px": [[2100, -900], [3136, -1400], [4400, -600]] }]"#,
    );
    let doc = r.app.document().unwrap();
    let c = worlds_editor::annot::all_notes(doc)
        .into_iter()
        .find(|n| matches!(n.note.shape, worlds_core::Shape::Connector { .. }))
        .expect("a connector");
    let worlds_core::Shape::Connector {
        from,
        to,
        waypoints,
        ..
    } = &c.note.shape
    else {
        unreachable!()
    };
    assert_eq!(*waypoints, [[3136, -1408]]);
    assert_eq!(
        worlds_editor::annot::end_level(doc, from),
        doc.level_ix("level_1")
    );
    assert_eq!(
        worlds_editor::annot::end_level(doc, to),
        doc.level_ix("level_4")
    );
    let id = c.note.id.to_string();
    r.steps(&format!(r#"[{{ "ui": "select", "notes": ["{id}"] }}]"#));
    keep(&r.hl, "geometry_connector.png");
}
