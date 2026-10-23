//! Setting up a fresh project (`worlds-cli new`) in the editor, headless:
//! the empty map's hint, levels moved with no layer, and the Project
//! pane's + Layer, + Tileset and settings, saved to the files a newcomer
//! would otherwise write by hand. Needs a GPU (or a software driver), so it
//! only runs with `WORLDS_GPU_TESTS=1`.

use std::path::{Path, PathBuf};

use worlds_editor::app::{AppOptions, EditorApp, Pane};

fn gpu_tests() -> bool {
    let on = std::env::var("WORLDS_GPU_TESTS").as_deref() == Ok("1");
    if !on {
        eprintln!("skipped: set WORLDS_GPU_TESTS=1 to run");
    }
    on
}

/// Where these tests leave their PNGs: `WORLDS_TEST_OUT`, else nowhere.
fn keep(png: &Path) {
    if let Some(dir) = std::env::var_os("WORLDS_TEST_OUT") {
        let dir = PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::copy(png, dir.join(png.file_name().unwrap())).unwrap();
    }
}

fn run(app: &mut EditorApp, json: serde_json::Value) -> Result<(), String> {
    let steps = worlds_editor::script::parse(&json.to_string()).map_err(|e| format!("{e:#}"))?;
    for s in steps {
        app.run_step(s)?;
    }
    Ok(())
}

/// Pixels of `img` inside `r` that aren't `bg`.
fn not_bg(img: &image::RgbaImage, r: egui::Rect, bg: [u8; 3]) -> usize {
    let mut n = 0;
    for y in r.min.y.max(0.0) as u32..(r.max.y as u32).min(img.height()) {
        for x in r.min.x.max(0.0) as u32..(r.max.x as u32).min(img.width()) {
            let p = img.get_pixel(x, y);
            if [p[0], p[1], p[2]] != bg {
                n += 1;
            }
        }
    }
    n
}

#[test]
fn a_fresh_project_is_set_up_in_the_project_pane() {
    if !gpu_tests() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("dungeon.worlds");
    let mut p = worlds_core::Project::new("dungeon");
    p.grid_size = 32;
    worlds_core::save(&mut p, &dir).unwrap();
    // The art lives outside the project, as a download would.
    let art = tmp.path().join("tiles.png");
    image::RgbaImage::from_fn(128, 64, |x, y| {
        image::Rgba([(x * 2) as u8, (y * 4) as u8, 90, 255])
    })
    .save(&art)
    .unwrap();

    let hl = worlds_editor::headless::Headless::new([1600, 900]).unwrap();
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
    let mut frames = |app: &mut EditorApp| {
        for _ in 0..4 {
            t += 1.0 / 60.0;
            hl.frame(&ctx, app, t);
        }
        hl.read().unwrap()
    };

    // The empty map says how to make a level: a card in its middle.
    let img = frames(&mut app);
    let out = tmp.path().join("setup_empty_map.png");
    img.save(&out).unwrap();
    keep(&out);
    let map = app.st.pane_rects[&Pane::Viewport];
    let mid = egui::Rect::from_center_size(map.center(), egui::vec2(300.0, 100.0));
    assert!(
        not_bg(&img, mid, [24, 24, 24]) > 2000,
        "the empty map's hint is drawn"
    );

    // With no layer at all, the Select tool still moves a level.
    run(
        &mut app,
        serde_json::json!([
            { "op": "create_level", "world": "main",
              "level": { "id": "entrance22", "slug": "entrance", "name": "Entrance",
                         "world_px": [0, 0], "size": [32, 18], "depth": 0 } },
            { "gesture": "select", "world_px": [[100, 100], [164, 132]], "press": true }
        ]),
    )
    .unwrap();
    let doc = app.st.doc.as_ref().unwrap();
    assert_eq!(doc.level(0).world_px, [64, 32]);
    let img = frames(&mut app);
    assert!(
        not_bg(&img, mid, [24, 24, 24]) < not_bg(&img, map, [24, 24, 24]),
        "with a level the hint goes"
    );

    // + Layer: int_grid (collision, with `solid`), auto (reads it),
    // entities; + Tileset… from outside the project (copied into art/).
    run(
        &mut app,
        serde_json::json!([
            { "ui": "project", "add_layer": "int_grid" },
            { "ui": "project", "add_layer": "auto" },
            { "ui": "project", "add_layer": "entities" },
            { "ui": "project", "add_tileset": art }
        ]),
    )
    .unwrap();
    assert_eq!(
        app.st.project.tileset.as_ref().map(|s| s.as_str()),
        Some("tiles")
    );
    assert!(
        app.st
            .project
            .notice
            .as_deref()
            .is_some_and(|n| n.contains("art/tiles.png"))
    );
    // The auto layer added on top made it the active one, then the
    // entities layer.
    let doc = app.st.doc.as_ref().unwrap();
    let slugs: Vec<&str> = doc.project.layers.iter().map(|l| l.slug.as_str()).collect();
    assert_eq!(slugs, ["collision", "walls", "objects"]);
    assert_eq!(app.st.view.active_layer, Some(2));
    let img = frames(&mut app);
    let out = tmp.path().join("setup_tileset.png");
    img.save(&out).unwrap();
    keep(&out);
    let pane = app.st.pane_rects[&Pane::Project];
    let preview = app.st.project.rects["tileset preview"];
    assert!(pane.contains_rect(preview.intersect(pane)));
    assert!(
        not_bg(&img, preview.intersect(pane), [24, 24, 24]) > 1000,
        "the atlas preview is drawn"
    );

    // Settings: the collision layer (its `solid` value blocks); the value
    // the tutorial adds; then save.
    run(
        &mut app,
        serde_json::json!([
            { "op": "set_project_setting", "setting": { "key": "collision_layer", "value": "collision" } },
            { "op": "set_project_setting", "setting": { "key": "blocking_values", "value": ["solid"] } },
            { "ui": "project", "section": "settings" },
            { "ui": "project", "layer": "collision" }
        ]),
    )
    .unwrap();
    for bad in [
        serde_json::json!([{ "ui": "project", "add_layer": "nope" }]),
        serde_json::json!([{ "ui": "project", "section": "nope" }]),
        serde_json::json!([{ "ui": "project", "layer": "nope" }]),
        serde_json::json!([{ "ui": "project", "tileset": "nope" }]),
        serde_json::json!([{ "ui": "project", "add_tileset": "/no/such.png" }]),
        serde_json::json!([{ "ui": "project", "bogus": 1 }]),
        // Grid size can't change under a level.
        serde_json::json!([{ "op": "set_project_setting", "setting": { "key": "grid_size", "value": 16 } }]),
    ] {
        assert!(run(&mut app, bad.clone()).is_err(), "{bad}");
    }
    frames(&mut app);
    app.save().unwrap();
    let read = |f: &str| std::fs::read_to_string(dir.join(f)).unwrap();
    let layers: serde_json::Value = serde_json::from_str(&read("layers.json")).unwrap();
    assert_eq!(layers["layers"][0]["values"][0]["slug"], "solid");
    assert_eq!(layers["layers"][1]["source"], "collision");
    assert_eq!(layers["layers"][2]["kind"], "entities");
    let tilesets: serde_json::Value = serde_json::from_str(&read("tilesets.json")).unwrap();
    assert_eq!(tilesets["tilesets"][0]["image"], "art/tiles.png");
    assert_eq!(
        tilesets["tilesets"][0]["tile_size"],
        serde_json::json!([32, 32])
    );
    assert!(dir.join("art/tiles.png").exists());
    let project: serde_json::Value = serde_json::from_str(&read("project.json")).unwrap();
    assert_eq!(project["collision_layer"], "collision");
    assert_eq!(project["blocking_values"], serde_json::json!(["solid"]));
    assert_eq!(project["grid_size"], 32);
    // What was saved loads and validates clean (the auto layer has no
    // rules yet, which isn't a problem).
    let loaded = worlds_core::load(&dir).unwrap();
    let problems = worlds_core::validate(&loaded);
    assert!(problems.is_empty(), "{problems:#?}");
}
