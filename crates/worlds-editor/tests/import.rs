//! Import from Worlds project… in the editor, headless: the dialog on the
//! game fixture over a fresh project (a `"ui": "import_project"` step, as a
//! screenshot shows it), presets and checkboxes, Apply as one undo entry
//! `Import from game` that brings levels into a new world and copies the
//! tileset images. Needs a GPU (or a software driver), so it only runs
//! with `WORLDS_GPU_TESTS=1`.

use std::path::{Path, PathBuf};

use worlds_core::merge::{Action as ImportAction, Kind, Preset};
use worlds_core::{Project, Slug};
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

/// Where the PNGs go: `WORLDS_TEST_OUT`, else nowhere.
fn keep(hl: &Headless, name: &str) {
    if let Some(dir) = std::env::var_os("WORLDS_TEST_OUT") {
        let dir = PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        hl.read().unwrap().save(dir.join(name)).unwrap();
    }
}

fn game() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/game/game.worlds")
}

struct Rig {
    hl: Headless,
    ctx: egui::Context,
    app: EditorApp,
    t: f64,
    dir: PathBuf,
    _tmp: tempfile::TempDir,
}

impl Rig {
    /// A fresh 32 px project with one empty world.
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("sequel.worlds");
        let mut p = Project::new("Sequel");
        p.grid_size = 32;
        p.seed = 7;
        worlds_core::save(&mut p, &dir).unwrap();
        let hl = Headless::new([1400, 900]).unwrap();
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
            dir,
            _tmp: tmp,
        };
        r.frames(4);
        r
    }

    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            self.t += 1.0 / 60.0;
            self.hl.frame(&self.ctx, &mut self.app, self.t);
        }
    }

    fn step(&mut self, json: &str) -> Result<(), String> {
        for s in worlds_editor::script::parse(json).map_err(|e| e.to_string())? {
            self.app.run_step(s)?;
        }
        self.frames(4);
        Ok(())
    }
}

#[test]
fn the_import_dialog_plans_and_applies_one_entry() {
    if !gpu_tests() {
        return;
    }
    let mut r = Rig::new();
    let src = game().to_string_lossy().replace('\\', "/");
    r.step(&format!(
        r#"[{{ "ui": "import_project", "source": "{src}",
               "selection": {{ "levels": ["level_1", "crevasse"] }} }}]"#
    ))
    .unwrap();
    // Past the window's fade-in.
    r.frames(20);
    keep(&r.hl, "ui_import_dialog.png");
    let d = r.app.import_dialog().expect("the dialog is open");
    assert!(d.checked(Kind::Level, &Slug::new("level_1").unwrap()));
    assert!(!d.checked(Kind::Layer, &Slug::new("collision").unwrap()));
    let doc = r.app.st.doc.as_ref().unwrap();
    let project = doc.project.clone();
    let d = r.app.import_dialog().unwrap();
    let plan = d.plan(&project).clone().unwrap();
    // The levels pulled in their layers; nothing is applied yet.
    let c = plan.item(Kind::Layer, "collision").unwrap();
    assert_eq!(c.action, ImportAction::Add);
    assert!(!c.required_by.is_empty());
    assert!(plan.summary.contains("2 levels"), "{}", plan.summary);
    assert!(r.app.st.doc.as_ref().unwrap().project.layers.is_empty());

    // Presets: Definitions only drops the levels; Pick spells it out, so
    // unchecking one layer leaves the rest.
    let d = r.app.import_dialog().unwrap();
    d.set_preset(Preset::DefsOnly);
    let plan = d.plan(&project).clone().unwrap();
    assert!(plan.item(Kind::Level, "level_1").is_none());
    d.set_checked(Kind::Layer, &Slug::new("decor_front").unwrap(), false);
    assert_eq!(d.selection.preset, Preset::Pick);
    assert!(d.checked(Kind::Layer, &Slug::new("collision").unwrap()));
    assert!(!d.checked(Kind::Layer, &Slug::new("decor_front").unwrap()));

    // Apply everything: one entry, a new world, the images copied.
    d.set_preset(Preset::Everything);
    let msg = r.app.apply_import().unwrap();
    assert!(msg.starts_with("Import from game: "), "{msg}");
    r.frames(6);
    assert!(r.app.import_dialog().is_none());
    let doc = r.app.st.doc.as_ref().unwrap();
    assert_eq!(doc.project.world("game").unwrap().levels.len(), 5);
    assert!(r.dir.join("art/tiles.png").is_file());
    assert_eq!(r.app.st.view.world_ix(doc), 1, "the new world is shown");
    keep(&r.hl, "ui_import_applied.png");
    r.app.run(Action::Undo);
    r.frames(4);
    let doc = r.app.st.doc.as_ref().unwrap();
    assert!(doc.project.world("game").is_none());
    assert!(doc.project.layers.is_empty());

    // Bad steps: this project itself, a source that isn't one, nothing
    // to apply.
    let me = r.dir.to_string_lossy().replace('\\', "/");
    let e = r
        .step(&format!(
            r#"[{{ "ui": "import_project", "source": "{me}" }}]"#
        ))
        .unwrap_err();
    assert!(e.contains("this project"), "{e}");
    let e = r
        .step(r#"[{ "ui": "import_project", "source": "/nowhere/at/all" }]"#)
        .unwrap_err();
    assert!(e.contains("can't import from it"), "{e}");
    let e = r
        .step(&format!(
            r#"[{{ "ui": "import_project", "source": "{src}", "apply": true }}]"#
        ))
        .unwrap_err();
    assert!(e.starts_with("Nothing to import"), "{e}");
}
