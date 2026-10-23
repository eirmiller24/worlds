//! Headless render tests. Need a GPU (or a software Vulkan/GL driver), so
//! they only run with `WORLDS_GPU_TESTS=1`. They work on temp copies of the
//! `game` fixture (`tests/fixtures/game/`: a small platformer project and
//! its tileset art).

use std::path::PathBuf;

use worlds_editor::headless::{ScreenshotOptions, non_blank_fraction, screenshot};

#[test]
fn crevasse_renders() {
    if std::env::var("WORLDS_GPU_TESTS").as_deref() != Ok("1") {
        eprintln!("skipped: set WORLDS_GPU_TESTS=1 to run");
        return;
    }
    // A copy, not the fixture itself: no test writes into the repo.
    let (tmp, dir) = temp_copy(&game());
    let out = tmp.path().join("crevasse.png");
    let report = screenshot(&ScreenshotOptions {
        level: Some("crevasse".into()),
        size: [800, 500],
        ..ScreenshotOptions::new(&dir, &out)
    })
    .unwrap();
    let img = image::open(&out).unwrap().to_rgba8();
    assert_eq!(img.dimensions(), (800, 500));
    // The whole frame has UI on it ...
    assert!(
        non_blank_fraction(&img) > 0.2,
        "{}",
        non_blank_fraction(&img)
    );
    // ... and the viewport drew tiles: crevasse's chunks were drawn, and
    // the middle of the map shows many distinct colors (tile art), not a
    // flat fill.
    assert!(report.draws > 0 && report.instances > 100, "{report:?}");
    let mut colors = std::collections::HashSet::new();
    for y in 150..350 {
        for x in 250..550 {
            colors.insert(img.get_pixel(x, y).0);
        }
    }
    assert!(
        colors.len() > 30,
        "only {} colors in the map area",
        colors.len()
    );
}

/// Siblings of the project that a temp copy links to (read only: the
/// tileset images live in `../assets`). Nothing a test or the editor writes
/// may live here: a write through the link lands in the source tree.
const LINKED_SIBLINGS: &[&str] = &["assets"];
/// Siblings a temp copy copies as real directories, because tests read and
/// write them.
const COPIED_SIBLINGS: &[&str] = &["data"];

fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
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

/// Copy a project directory into a tempdir, link only `LINKED_SIBLINGS`
/// next to it (so relative image paths still work) and copy
/// `COPIED_SIBLINGS`. Never a link to a directory anything writes into.
fn temp_copy(project: &std::path::Path) -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let name = project.file_name().unwrap();
    let dir = tmp.path().join(name);
    copy_dir(project, &dir);
    if let Some(parent) = project.parent() {
        for sib in LINKED_SIBLINGS {
            let src = parent.join(sib);
            if src.is_dir() {
                #[cfg(unix)]
                std::os::unix::fs::symlink(&src, tmp.path().join(sib)).unwrap();
                #[cfg(not(unix))]
                copy_dir(&src, &tmp.path().join(sib));
            }
        }
        for sib in COPIED_SIBLINGS {
            let src = parent.join(sib);
            if src.is_dir() {
                copy_dir(&src, &tmp.path().join(sib));
            }
        }
    }
    (tmp, dir)
}

/// The guard for every write next to a temp copy: `target` (which need not
/// exist yet) resolves, through any symlinks, to a path inside `tmp`. Panics
/// otherwise, before anything is written.
fn assert_write_inside(tmp: &std::path::Path, target: &std::path::Path) {
    let root = tmp.canonicalize().unwrap();
    // Canonicalize the deepest existing ancestor, then re-append the rest
    // (refusing `..` in the part that doesn't exist yet).
    let mut existing = target.to_path_buf();
    let mut rest = Vec::new();
    while !existing.exists() {
        let name = existing.file_name().unwrap_or_else(|| {
            panic!("write target {} has no existing ancestor", target.display())
        });
        rest.push(name.to_os_string());
        existing.pop();
    }
    let mut resolved = existing.canonicalize().unwrap();
    for n in rest.iter().rev() {
        resolved.push(n);
    }
    assert!(
        resolved.starts_with(&root),
        "write target {} resolves to {}, outside the tempdir {}",
        target.display(),
        resolved.display(),
        root.display()
    );
}

/// `temp_copy` links no sibling a test writes into. A fake game repo
/// (project, `assets/`, `data/rooms.json`): in the copy `data` is a real
/// directory, `../data/rooms.json` from the project resolves inside the
/// tempdir, writing it leaves the "game" file alone, and the guard refuses a
/// write through the `assets` link. Runs without a GPU.
#[test]
fn temp_copy_never_links_a_written_sibling() {
    let game = tempfile::tempdir().unwrap();
    let project = game.path().join("game.worlds");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("project.json"), "{}").unwrap();
    std::fs::create_dir_all(game.path().join("assets")).unwrap();
    std::fs::write(game.path().join("assets/a.png"), "png").unwrap();
    std::fs::create_dir_all(game.path().join("data")).unwrap();
    std::fs::write(game.path().join("data/rooms.json"), "game").unwrap();
    std::fs::create_dir_all(game.path().join("tools")).unwrap();

    let (tmp, dir) = temp_copy(&project);
    for (sib, linked) in [("assets", true), ("data", false)] {
        let ft = std::fs::symlink_metadata(tmp.path().join(sib))
            .unwrap()
            .file_type();
        assert_eq!(ft.is_symlink(), linked, "{sib}");
    }
    // Unlisted siblings are neither linked nor copied.
    assert!(!tmp.path().join("tools").exists());
    assert_eq!(
        std::fs::read_to_string(dir.join("../data/rooms.json")).unwrap(),
        "game"
    );
    let target = dir.join("../data/rooms.json");
    assert_write_inside(tmp.path(), &target);
    std::fs::write(&target, "test").unwrap();
    assert_eq!(
        std::fs::read_to_string(game.path().join("data/rooms.json")).unwrap(),
        "game"
    );
    // The guard catches a write through a link, existing file or not.
    let through = dir.join("../assets/new.json");
    let caught = std::panic::catch_unwind(|| assert_write_inside(tmp.path(), &through));
    assert!(caught.is_err(), "a write through ../assets must be refused");
    assert!(!game.path().join("assets/new.json").exists());
}

fn collision_rows(dir: &std::path::Path) -> Vec<String> {
    let path = dir.join("worlds/main/levels/crevasse/collision.json");
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    v["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r.as_str().unwrap().to_string())
        .collect()
}

/// `--script` end to end: paint a wall into a copy of crevasse (a command
/// and a brush gesture), render, save, and check the saved collision grid
/// changed in exactly the painted cells and no other file changed.
#[test]
fn script_paints_saves_and_renders() {
    if std::env::var("WORLDS_GPU_TESTS").as_deref() != Ok("1") {
        eprintln!("skipped: set WORLDS_GPU_TESTS=1 to run");
        return;
    }
    let (tmp, dir) = temp_copy(&game());
    let before = collision_rows(&dir);
    let snapshot = |d: &std::path::Path| {
        let mut m = std::collections::BTreeMap::new();
        let mut stack = vec![d.to_path_buf()];
        while let Some(x) = stack.pop() {
            for e in std::fs::read_dir(&x).unwrap() {
                let p = e.unwrap().path();
                // `.worlds/` is the session's (lock, autosave), not the project.
                if p.file_name() == Some(std::ffi::OsStr::new(".worlds")) {
                    continue;
                }
                if p.is_dir() {
                    stack.push(p);
                } else {
                    m.insert(p.clone(), std::fs::read(&p).unwrap());
                }
            }
        }
        m
    };
    let files_before = snapshot(&dir);
    let script = tmp.path().join("wall.json");
    std::fs::write(
        &script,
        r#"[
          { "op": "fill_rect", "level": "crevasse", "layer": "collision", "rect": [28, 12, 4, 13], "value": "solid" },
          { "gesture": "brush", "level": "crevasse", "layer": "collision",
            "path": [[10, 15], [15, 15]], "brush": "solid" }
        ]"#,
    )
    .unwrap();
    let out = tmp.path().join("wall.png");
    let report = screenshot(&ScreenshotOptions {
        level: Some("crevasse".into()),
        size: [800, 500],
        script: Some(script),
        save: true,
        ..ScreenshotOptions::new(&dir, &out)
    })
    .unwrap();
    assert_eq!(report.applied, 2);
    let img = image::open(&out).unwrap().to_rgba8();
    assert!(non_blank_fraction(&img) > 0.2);

    let after = collision_rows(&dir);
    let mut changed = Vec::new();
    for (y, (a, b)) in before.iter().zip(&after).enumerate() {
        for (x, (ca, cb)) in a.chars().zip(b.chars()).enumerate() {
            if ca != cb {
                assert_eq!(cb, '#', "cell {x}, {y}");
                changed.push([x, y]);
            }
        }
    }
    let mut expected: Vec<[usize; 2]> = (12..25)
        .flat_map(|y| (28..32).map(move |x| [x, y]))
        .chain((10..16).map(|x| [x, 15]))
        .collect();
    expected.sort_by_key(|c| (c[1], c[0]));
    changed.sort_by_key(|c| (c[1], c[0]));
    assert_eq!(changed, expected);
    let files_after = snapshot(&dir);
    let differ: Vec<_> = files_after
        .iter()
        .filter(|(p, b)| files_before.get(*p) != Some(b))
        .map(|(p, _)| p.strip_prefix(&dir).unwrap().to_path_buf())
        .collect();
    assert_eq!(
        differ,
        [PathBuf::from("worlds/main/levels/crevasse/collision.json")]
    );
}

fn gpu_tests() -> bool {
    let on = std::env::var("WORLDS_GPU_TESTS").as_deref() == Ok("1");
    if !on {
        eprintln!("skipped: set WORLDS_GPU_TESTS=1 to run");
    }
    on
}

/// The `game` fixture project (read only: tests work on `temp_copy`s).
fn game() -> PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/game/game.worlds")
}

/// Run the `worlds` binary; (exit code, stdout, stderr).
fn worlds(args: &[&std::ffi::OsStr]) -> (i32, String, String) {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_worlds"))
        .args(args)
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn os(s: &str) -> &std::ffi::OsStr {
    std::ffi::OsStr::new(s)
}

/// Where these tests leave their PNGs: `WORLDS_TEST_OUT`, else the temp dir.
fn keep(png: &std::path::Path) {
    if let Some(dir) = std::env::var_os("WORLDS_TEST_OUT") {
        let dir = PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::copy(png, dir.join(png.file_name().unwrap())).unwrap();
    }
}

/// `--snapshot`: the map alone, fitted into `--max-size`; `--layers`
/// changes the picture; a bad rect or an unknown level exits 1.
#[test]
fn snapshot_cli_flags() {
    if !gpu_tests() {
        return;
    }
    let (tmp, dir) = temp_copy(&game());
    let full = tmp.path().join("snap_full.png");
    let (code, _, err) = worlds(&[
        os("--screenshot"),
        full.as_os_str(),
        os("--snapshot"),
        os("--level"),
        os("crevasse"),
        os("--max-size"),
        os("600x400"),
        dir.as_os_str(),
    ]);
    assert_eq!(code, 0, "{err}");
    keep(&full);
    let img = image::open(&full).unwrap().to_rgba8();
    assert!(
        img.width() <= 600 && img.height() <= 400,
        "{:?}",
        img.dimensions()
    );
    assert!(
        img.width() == 600 || img.height() == 400,
        "{:?}",
        img.dimensions()
    );
    assert!(non_blank_fraction(&img) > 0.05);

    let col = tmp.path().join("snap_collision.png");
    let (code, _, err) = worlds(&[
        os("--screenshot"),
        col.as_os_str(),
        os("--snapshot"),
        os("--level"),
        os("crevasse"),
        os("--max-size"),
        os("600x400"),
        os("--layers"),
        os("collision"),
        dir.as_os_str(),
    ]);
    assert_eq!(code, 0, "{err}");
    keep(&col);
    let c = image::open(&col).unwrap().to_rgba8();
    assert_eq!(c.dimensions(), img.dimensions());
    assert_ne!(c, img);

    for bad in [
        vec![os("--world-rect"), os("0,0,0,10")],
        vec![os("--world-rect"), os("1,2,3")],
        vec![os("--level"), os("no_such_level")],
        vec![os("--level"), os("crevasse"), os("--layers"), os("nope")],
    ] {
        let mut args = vec![
            os("--screenshot"),
            tmp.path().join("bad.png").into_os_string().leak(),
            os("--snapshot"),
        ];
        args.extend(bad.iter().copied());
        args.push(dir.as_os_str());
        let (code, _, err) = worlds(&args);
        assert_eq!(code, 1, "{bad:?}: {err}");
        assert!(err.contains("Error"), "{bad:?}: {err}");
    }
}

/// The headless agent path: `worlds --screenshot --serve --wait-for` on a
/// copy of the game fixture while an agent connects over the socket, paints a wall
/// with `grid.fill_rect` and takes `view.snapshot`s. The final screenshot
/// shows the wall, the snapshot equals `--snapshot` of the same level, and
/// `--save` writes exactly those cells.
#[test]
fn serve_lets_an_agent_paint() {
    use futures_util::{SinkExt, StreamExt};
    use serde_json::{Value, json};
    use std::io::BufRead as _;
    use tokio_tungstenite::tungstenite::Message;

    if !gpu_tests() {
        return;
    }
    let (tmp, dir) = temp_copy(&game());
    let before_rows = collision_rows(&dir);
    // Map-only snapshot and full-UI screenshot before the edit.
    let cli_snap = tmp.path().join("serve_cli_snapshot.png");
    let (code, _, err) = worlds(&[
        os("--screenshot"),
        cli_snap.as_os_str(),
        os("--snapshot"),
        os("--level"),
        os("crevasse"),
        os("--max-size"),
        os("1024x1024"),
        dir.as_os_str(),
    ]);
    assert_eq!(code, 0, "{err}");
    let before_ui = tmp.path().join("serve_before.png");
    let (code, _, err) = worlds(&[
        os("--screenshot"),
        before_ui.as_os_str(),
        os("--level"),
        os("crevasse"),
        os("--size"),
        os("800x500"),
        dir.as_os_str(),
    ]);
    assert_eq!(code, 0, "{err}");

    let after_ui = tmp.path().join("serve_after.png");
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_worlds"))
        .args([
            os("--screenshot"),
            after_ui.as_os_str(),
            os("--level"),
            os("crevasse"),
            os("--size"),
            os("800x500"),
            os("--serve"),
            os("--wait-for"),
            os("4"),
            os("--save"),
            dir.as_os_str(),
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .unwrap();
    let mut line = String::new();
    // Keep reading stdout to the end so the editor can keep printing.
    let mut stdout = std::io::BufReader::new(child.stdout.take().unwrap());
    stdout.read_line(&mut line).unwrap();
    let rest = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = std::io::Read::read_to_string(&mut stdout, &mut s);
        s
    });
    let info: Value = serde_json::from_str(&line).unwrap_or_else(|e| panic!("{line:?}: {e}"));
    let port = info["port"].as_u64().unwrap();
    let disc: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join(".worlds/server.json")).unwrap())
            .unwrap();
    assert_eq!(disc["port"], info["port"]);

    let decode = |v: &Value| {
        use base64::Engine as _;
        let png = base64::engine::general_purpose::STANDARD
            .decode(v["result"]["png"].as_str().expect("png"))
            .unwrap();
        image::load_from_memory(&png).unwrap().to_rgba8()
    };
    let (snap_before, fill, snap_after) = tokio::runtime::Runtime::new().unwrap().block_on(async {
        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/"))
            .await
            .unwrap();
        let mut id = 0;
        let mut call = async |ws: &mut tokio_tungstenite::WebSocketStream<_>, method: &str, params: Value| {
            id += 1;
            ws.send(Message::Text(
                json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
            loop {
                let m = tokio::time::timeout(std::time::Duration::from_secs(10), ws.next())
                    .await
                    .expect("no answer within 10 s")
                    .unwrap()
                    .unwrap();
                if let Message::Text(t) = m {
                    let v: Value = serde_json::from_str(&t).unwrap();
                    if v["id"] == id {
                        return v;
                    }
                }
            }
        };
        let r = call(
            &mut ws,
            "session.hello",
            json!({ "token": disc["token"], "client": { "name": "gpu-test", "version": "1" } }),
        )
        .await;
        assert!(r.get("result").is_some(), "{r}");
        let snap = json!({ "level": "crevasse", "size": [1024, 1024] });
        let before = call(&mut ws, "view.snapshot", snap.clone()).await;
        let fill = call(
            &mut ws,
            "grid.fill_rect",
            json!({ "level": "crevasse", "layer": "collision", "rect": [28, 12, 4, 13], "value": "solid" }),
        )
        .await;
        let after = call(&mut ws, "view.snapshot", snap).await;
        (before, fill, after)
    });
    assert_eq!(fill["result"]["status"], "applied", "{fill}");
    let status = child.wait().unwrap();
    let rest = rest.join().unwrap();
    assert!(status.success(), "{status}: {rest}");

    // view.snapshot and --snapshot are the same function: same pixels.
    let cli = image::open(&cli_snap).unwrap().to_rgba8();
    let before = decode(&snap_before);
    let after = decode(&snap_after);
    let scale = snap_after["result"]["scale"].as_f64().unwrap();
    assert_eq!(before.dimensions(), cli.dimensions());
    assert!(before == cli, "view.snapshot differs from --snapshot");
    let snap_png = tmp.path().join("serve_snapshot_after.png");
    after.save(&snap_png).unwrap();
    keep(&cli_snap);
    keep(&snap_png);

    // The wall's cells changed in the snapshot; nothing two cells away did
    // (the auto layer's rebake reaches a little past the painted cells).
    let cell = 32.0 * scale;
    let px = |c: f64| (c * cell) as u32;
    let (mut inside, mut changed_inside) = (0, 0);
    for y in px(12.0)..px(25.0) {
        for x in px(28.0)..px(32.0) {
            inside += 1;
            if before.get_pixel(x, y) != after.get_pixel(x, y) {
                changed_inside += 1;
            }
        }
    }
    assert!(changed_inside * 2 > inside, "{changed_inside} of {inside}");
    for (x, y, p) in after.enumerate_pixels() {
        let (cx, cy) = (x as f64 / cell, y as f64 / cell);
        let near = (26.0..34.0).contains(&cx) && (10.0..27.0).contains(&cy);
        if !near {
            assert_eq!(before.get_pixel(x, y), p, "pixel {x}, {y} changed");
        }
    }

    // The full UI screenshot shows a change of the wall's shape: taller
    // than wide.
    keep(&before_ui);
    keep(&after_ui);
    let a = image::open(&before_ui).unwrap().to_rgba8();
    let b = image::open(&after_ui).unwrap().to_rgba8();
    let (mut x0, mut y0, mut x1, mut y1, mut n) = (u32::MAX, u32::MAX, 0, 0, 0);
    for (x, y, p) in b.enumerate_pixels() {
        // Only the map pane (the log, toast and status bar change too).
        if y > 60 && y < 290 && x > 180 && x < 620 && a.get_pixel(x, y) != p {
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
            n += 1;
        }
    }
    assert!(n > 50, "only {n} pixels changed in the map");
    assert!(
        y1 - y0 > 2 * (x1 - x0),
        "changed area {x0},{y0}..{x1},{y1} isn't a wall"
    );

    // --save after serving: collision.json changed in exactly the wall.
    let after_rows = collision_rows(&dir);
    let mut changed = Vec::new();
    for (y, (a, b)) in before_rows.iter().zip(&after_rows).enumerate() {
        for (x, (ca, cb)) in a.chars().zip(b.chars()).enumerate() {
            if ca != cb {
                changed.push([x, y]);
            }
        }
    }
    let expected: Vec<[usize; 2]> = (12..25)
        .flat_map(|y| (28..32).map(move |x| [x, y]))
        .filter(|&[x, y]| before_rows[y].as_bytes()[x] != b'#')
        .collect();
    assert_eq!(changed, expected);
    assert!(
        !dir.join(".worlds/server.json").exists(),
        "server.json left behind"
    );
}

/// Approval mode in the real app (offscreen): an agent's write waits as a
/// pending batch, the canvas draws its ghost and the panel lists it; Apply
/// answers the waiting request with `applied`.
#[test]
fn approval_ghost_renders_and_applies() {
    use serde_json::{Value, json};
    use tokio_tungstenite::tungstenite::{self, Message};
    use worlds_editor::app::{AppOptions, EditorApp};
    use worlds_editor::headless::Headless;

    if !gpu_tests() {
        return;
    }
    let (tmp, dir) = temp_copy(&game());
    let mut hl = Headless::new([1600, 900]).unwrap();
    let ctx = egui::Context::default();
    let mut app = EditorApp::new(
        &ctx,
        hl.gpu.clone(),
        worlds_editor::log::init(),
        AppOptions {
            headless: true,
            level: Some("crevasse".into()),
            serve: true,
            ..Default::default()
        },
    );
    app.set_commands(Box::new(worlds_editor::edit::Editing::new()));
    app.open(&dir);
    app.set_approval(true);
    let info = app.server().expect("server").info().clone();
    let agent = std::thread::spawn(move || {
        let (mut ws, _) = tungstenite::connect(format!("ws://127.0.0.1:{}/", info.port)).unwrap();
        let mut call = |id: u64, method: &str, params: Value| {
            ws.send(Message::Text(
                json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
                    .to_string()
                    .into(),
            ))
            .unwrap();
            loop {
                if let Message::Text(t) = ws.read().unwrap() {
                    let v: Value = serde_json::from_str(&t).unwrap();
                    if v["id"] == id {
                        return v;
                    }
                }
            }
        };
        call(
            0,
            "session.hello",
            json!({ "token": info.token, "client": { "name": "claude-code", "version": "1" } }),
        );
        call(
            1,
            "grid.fill_rect",
            json!({ "level": "crevasse", "layer": "collision", "rect": [28, 12, 4, 13],
                    "value": "solid", "label": "claude-code: wall" }),
        )
    });
    let mut t = 0.0;
    let start = std::time::Instant::now();
    while app.server().unwrap().pending().is_empty() {
        assert!(start.elapsed().as_secs() < 10, "no pending batch");
        hl.frame(&ctx, &mut app, t);
        t += 1.0 / 60.0;
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    for _ in 0..3 {
        hl.frame(&ctx, &mut app, t);
        t += 1.0 / 60.0;
    }
    assert_eq!(app.st.ghosts.len(), 1);
    assert_eq!(app.st.ghosts[0].cells(), 52);
    // The panel sits inside the Map pane, not over the inspector.
    let map = app.st.map_rect.expect("map drawn");
    let panel = ctx
        .memory(|m| m.area_rect(egui::Id::new("agent batches panel")))
        .expect("panel shown");
    assert!(
        map.contains_rect(panel),
        "panel {panel:?} outside map {map:?}"
    );
    let png = tmp.path().join("approval_ghost.png");
    hl.read().unwrap().save(&png).unwrap();
    keep(&png);
    // A narrow window: the indicator collapses instead of drawing over the
    // tools, and still has its Approve toggle.
    let (tools, indicator) = app.toolbar_split().unwrap();
    assert!(tools.right() < indicator.left(), "{tools:?} {indicator:?}");
    hl.set_size([800, 500]);
    for _ in 0..3 {
        hl.frame(&ctx, &mut app, t);
        t += 1.0 / 60.0;
    }
    let (tools, indicator) = app.toolbar_split().unwrap();
    assert!(tools.right() < indicator.left(), "{tools:?} {indicator:?}");
    assert!(
        indicator.right() <= 800.0 && indicator.width() >= 100.0,
        "{indicator:?}"
    );
    let png = tmp.path().join("approval_narrow.png");
    hl.read().unwrap().save(&png).unwrap();
    keep(&png);
    let map = app.st.map_rect.expect("map drawn");
    let panel = ctx
        .memory(|m| m.area_rect(egui::Id::new("agent batches panel")))
        .expect("panel shown");
    assert!(
        map.contains_rect(panel),
        "panel {panel:?} outside map {map:?}"
    );
    let id = app.server().unwrap().pending()[0].id.clone();
    app.apply_pending(&id).unwrap();
    let r = agent.join().unwrap();
    assert_eq!(r["result"]["status"], "applied", "{r}");
    for _ in 0..2 {
        hl.frame(&ctx, &mut app, t);
        t += 1.0 / 60.0;
    }
    assert!(app.st.ghosts.is_empty());
    let png = tmp.path().join("approval_applied.png");
    hl.read().unwrap().save(&png).unwrap();
    keep(&png);
    assert!(app.has_unsaved());
}

/// `--serve` with no `--wait-for` (serving until stdin closes) stops
/// cleanly on SIGTERM: it exits 0 after its screenshot, and
/// `.worlds/server.json` is gone.
#[cfg(unix)]
#[test]
fn serve_stops_cleanly_on_sigterm() {
    if !gpu_tests() {
        return;
    }
    let (tmp, dir) = temp_copy(&game());
    let out = tmp.path().join("sigterm.png");
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_worlds"))
        .args([
            os("--screenshot"),
            out.as_os_str(),
            os("--size"),
            os("400x300"),
            os("--serve"),
            dir.as_os_str(),
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let mut line = String::new();
    let mut stdout = std::io::BufReader::new(child.stdout.take().unwrap());
    std::io::BufRead::read_line(&mut stdout, &mut line).unwrap();
    assert!(line.contains("\"port\""), "{line:?}");
    // Keep reading, so the report it prints at the end has somewhere to go.
    let rest = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = std::io::Read::read_to_string(&mut stdout, &mut s);
        s
    });
    assert!(dir.join(".worlds/server.json").exists());
    let killed = std::process::Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    let start = std::time::Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        assert!(
            start.elapsed().as_secs() < 30,
            "still running after SIGTERM"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    assert!(status.success(), "{status:?}");
    assert!(
        !dir.join(".worlds/server.json").exists(),
        "server.json left behind"
    );
    assert!(out.exists(), "the screenshot was still taken");
    assert!(rest.join().unwrap().contains("adapter:"));
}

/// Agent toasts stack upward from just above the status bar, newest at the
/// bottom, and are never drawn over the status bar or the toolbar: checked
/// on every frame from the one where each toast first appears (so a frame
/// drawn from a stale stack size can't slip through), at 1600x900 with
/// three agent batches, then with more toasts than are shown ("+N more"),
/// then in a small window.
#[test]
fn agent_toasts_stack_above_the_status_bar() {
    use serde_json::{Value, json};
    use tokio_tungstenite::tungstenite::{self, Message};
    use worlds_editor::app::{AppOptions, EditorApp, SHOWN_TOASTS};
    use worlds_editor::headless::Headless;

    if !gpu_tests() {
        return;
    }
    let (tmp, dir) = temp_copy(&game());
    let mut hl = Headless::new([1600, 900]).unwrap();
    let ctx = egui::Context::default();
    let mut app = EditorApp::new(
        &ctx,
        hl.gpu.clone(),
        worlds_editor::log::init(),
        AppOptions {
            headless: true,
            level: Some("crevasse".into()),
            serve: true,
            ..Default::default()
        },
    );
    app.set_commands(Box::new(worlds_editor::edit::Editing::new()));
    app.open(&dir);
    let info = app.server().expect("server").info().clone();
    let (go, wait) = std::sync::mpsc::channel::<()>();
    let agent = std::thread::spawn(move || {
        let (mut ws, _) = tungstenite::connect(format!("ws://127.0.0.1:{}/", info.port)).unwrap();
        let mut call = |id: u64, method: &str, params: Value| {
            ws.send(Message::Text(
                json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
                    .to_string()
                    .into(),
            ))
            .unwrap();
            loop {
                if let Message::Text(t) = ws.read().unwrap() {
                    let v: Value = serde_json::from_str(&t).unwrap();
                    if v["id"] == id {
                        return v;
                    }
                }
            }
        };
        call(
            0,
            "session.hello",
            json!({ "token": info.token, "client": { "name": "claude-code", "version": "1" } }),
        );
        for (i, (x, label)) in [
            (28, "Agent: paint vertical wall x28-29"),
            (31, "Agent: paint vertical wall x31-32"),
            (34, "Agent: paint vertical wall x34-35"),
        ]
        .into_iter()
        .enumerate()
        {
            wait.recv().unwrap();
            let r = call(
                1 + i as u64,
                "grid.fill_rect",
                json!({ "level": "crevasse", "layer": "collision", "rect": [x, 12, 2, 13],
                        "value": "solid", "label": label }),
            );
            assert_eq!(r["result"]["status"], "applied", "{r}");
        }
    });
    let mut t = 0.0;
    let check = |app: &EditorApp, what: &str| {
        let (rects, bounds) = app.toast_rects();
        let bounds = bounds.expect("panels drawn");
        for r in &rects {
            assert!(
                bounds.contains_rect(*r),
                "{what}: toast {r:?} outside {bounds:?} (status bar at {})",
                bounds.bottom()
            );
        }
        for w in rects.windows(2) {
            assert!(
                w[1].bottom() <= w[0].top(),
                "{what}: toasts overlap or are out of order: {rects:?}"
            );
        }
        rects
    };
    for n in 1..=3 {
        go.send(()).unwrap();
        let start = std::time::Instant::now();
        while app.st.toasts.len() < n {
            assert!(start.elapsed().as_secs() < 10, "no toast {n}");
            hl.frame(&ctx, &mut app, t);
            t += 1.0 / 60.0;
            check(&app, &format!("waiting for toast {n}"));
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        for f in 0..3 {
            hl.frame(&ctx, &mut app, t);
            t += 1.0 / 60.0;
            check(&app, &format!("toast {n}, frame {f}"));
        }
    }
    agent.join().unwrap();
    let rects = check(&app, "three toasts");
    assert_eq!(rects.len(), 3, "{rects:?}");
    // Newest at the bottom, close above the status bar.
    let (_, bounds) = app.toast_rects();
    assert!(
        bounds.unwrap().bottom() - rects[0].bottom() <= 24.0,
        "{rects:?}"
    );
    let png = tmp.path().join("m2_toasts_stacked.png");
    hl.read().unwrap().save(&png).unwrap();
    keep(&png);

    // More than fit: the newest SHOWN_TOASTS and a "+N more" line.
    for i in 0..5 {
        app.st.toast(format!("note {i}"), false);
    }
    for f in 0..3 {
        hl.frame(&ctx, &mut app, t);
        t += 1.0 / 60.0;
        check(&app, &format!("eight toasts, frame {f}"));
    }
    let rects = check(&app, "eight toasts");
    assert_eq!(rects.len(), SHOWN_TOASTS + 1, "{rects:?}");
    let png = tmp.path().join("toasts_more.png");
    hl.read().unwrap().save(&png).unwrap();
    keep(&png);

    // A short window: whatever doesn't fit is folded into "+N more".
    hl.set_size([800, 300]);
    for f in 0..4 {
        hl.frame(&ctx, &mut app, t);
        t += 1.0 / 60.0;
        check(&app, &format!("800x300, frame {f}"));
    }
    assert!(!check(&app, "800x300").is_empty());
    let png = tmp.path().join("toasts_short.png");
    hl.read().unwrap().save(&png).unwrap();
    keep(&png);
}

/// The real window under Xvfb exits with status 0 after SIGTERM and after
/// SIGINT, with `.worlds/server.json` gone: the process leaves through
/// `signals::exit_now` once the app has shut down, so winit's X11 teardown
/// (which segfaulted in its keymap drop at process exit) never runs.
/// Needs `xvfb-run`; winit needs `libxkbcommon-x11.so`: when the system has
/// none, point `WORLDS_XKB_DIR` at a directory holding it (see the README).
#[cfg(unix)]
#[test]
fn window_exits_cleanly_on_signals_under_xvfb() {
    if !gpu_tests() {
        return;
    }
    let have = |cmd: &str| {
        std::process::Command::new("sh")
            .args(["-c", &format!("command -v {cmd}")])
            .stdout(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    };
    assert!(have("xvfb-run"), "xvfb-run is needed for this test");
    let ld = std::env::var_os("WORLDS_XKB_DIR").map(PathBuf::from);
    for (signal, name) in [("-TERM", "SIGTERM"), ("-INT", "SIGINT")] {
        let (_tmp, dir) = temp_copy(&game());
        let mut cmd = std::process::Command::new("xvfb-run");
        cmd.args(["-a", "-s", "-screen 0 1280x800x24"])
            .arg(env!("CARGO_BIN_EXE_worlds"))
            .arg(&dir)
            .env_remove("WAYLAND_DISPLAY")
            // xvfb-run sends the command's stderr (the log) to its stdout.
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        if let Some(ld) = &ld {
            cmd.env("LD_LIBRARY_PATH", ld);
        }
        let mut child = cmd.spawn().unwrap();
        let mut out = child.stdout.take().unwrap();
        let log = std::thread::spawn(move || {
            let mut s = String::new();
            let _ = std::io::Read::read_to_string(&mut out, &mut s);
            s
        });
        let info = dir.join(".worlds/server.json");
        let start = std::time::Instant::now();
        let pid = loop {
            if let Ok(s) = std::fs::read_to_string(&info)
                && let Ok(v) = serde_json::from_str::<serde_json::Value>(&s)
                && let Some(pid) = v["pid"].as_u64()
            {
                break pid;
            }
            if let Some(s) = child.try_wait().unwrap() {
                panic!(
                    "{name}: the window exited early ({s:?}):\n{}",
                    log.join().unwrap()
                );
            }
            assert!(start.elapsed().as_secs() < 90, "{name}: no server.json");
            std::thread::sleep(std::time::Duration::from_millis(100));
        };
        // A few frames in, so the keyboard state winit tears down exists.
        std::thread::sleep(std::time::Duration::from_secs(2));
        let killed = std::process::Command::new("kill")
            .args([signal, &pid.to_string()])
            .status()
            .unwrap();
        assert!(killed.success());
        let start = std::time::Instant::now();
        let status = loop {
            if let Some(s) = child.try_wait().unwrap() {
                break s;
            }
            assert!(start.elapsed().as_secs() < 30, "{name}: still running");
            std::thread::sleep(std::time::Duration::from_millis(50));
        };
        let log = log.join().unwrap();
        assert_eq!(status.code(), Some(0), "{name}: {status:?}\n{log}");
        assert!(!info.exists(), "{name}: server.json left behind");
        assert!(log.contains("signal received"), "{name}: {log}");
    }
}

/// The asymmetric 16 px test tile: a white "F" on a gradient where every
/// pixel has its own color, so any wrong turn or mirror shows.
fn test_tile() -> image::RgbaImage {
    image::RgbaImage::from_fn(16, 16, |x, y| {
        let f = (3..6).contains(&x) && (2..14).contains(&y)
            || (2..5).contains(&y) && (3..13).contains(&x)
            || (7..10).contains(&y) && (3..10).contains(&x);
        if f {
            image::Rgba([250, 250, 240, 255])
        } else {
            image::Rgba([
                (x * 9 + 20) as u8,
                (y * 9 + 20) as u8,
                (x * 3 + y * 5 + 60) as u8,
                255,
            ])
        }
    })
}

/// `tile` drawn with flip bits `b`, worked out with the `image` crate alone
/// (not the renderer): transpose first, then mirror left-right (bit 1),
/// then top-bottom (bit 2), the export contract in `docs/format.md`.
fn oriented(tile: &image::RgbaImage, b: u8) -> image::RgbaImage {
    use image::imageops;
    let mut t = tile.clone();
    if b & 4 != 0 {
        // Transpose = a quarter turn clockwise, then a left-right mirror.
        t = imageops::flip_horizontal(&imageops::rotate90(&t));
    }
    if b & 1 != 0 {
        t = imageops::flip_horizontal(&t);
    }
    if b & 2 != 0 {
        t = imageops::flip_vertical(&t);
    }
    t
}

/// The renderer draws a tile in all 8 orientations (flips `x`, `y`, `xy`
/// and the transposed `t`, `xt`, `yt`, `xyt`) exactly as an independently
/// rotated and flipped copy of the tile: pixel for pixel at 4x, through
/// `--snapshot`. The PNG `m3_orientations.png` is the render (top) over the
/// expected picture (bottom).
#[test]
fn eight_orientations_match_the_image_crate() {
    if !gpu_tests() {
        return;
    }
    use worlds_core::{Flip, LayerDef, LayerKind, Level, TilePlacement, TilesDef, TilesLayer};
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("orient.worlds");
    std::fs::create_dir_all(&dir).unwrap();
    let tile = test_tile();
    tile.save(dir.join("tile.png")).unwrap();
    let mut p = worlds_core::Project::new("orient");
    p.grid_size = 16;
    p.bg_color = Some(worlds_core::Color::new("#202020").unwrap());
    p.tilesets.push(worlds_core::TilesetDef {
        slug: worlds_core::slug("t"),
        image: "tile.png".into(),
        tile_size: [16, 16],
        spacing: 0,
        padding: 0,
    });
    p.layers.push(LayerDef::new(
        worlds_core::slug("deco"),
        "Deco",
        LayerKind::Tiles(TilesDef {
            tileset: Some(worlds_core::slug("t")),
        }),
    ));
    let mut level = Level::new(worlds_core::slug("row"), "Row", [17, 3]);
    level.tiles.push(TilesLayer {
        layer: worlds_core::slug("deco"),
        tiles: (0u8..8)
            .map(|b| TilePlacement {
                cell: [1 + 2 * b as u32, 1],
                tileset: worlds_core::slug("t"),
                tile: [0, 0],
                flip: Flip::from_bits(b),
                alpha: 1.0,
            })
            .collect(),
    });
    p.worlds[0].levels.push(level);
    worlds_core::save(&mut p, &dir).unwrap();
    let out = tmp.path().join("m3_orientations_render.png");
    let (code, _, err) = worlds(&[
        os("--screenshot"),
        out.as_os_str(),
        os("--snapshot"),
        os("--level"),
        os("row"),
        os("--max-size"),
        os("1088x192"),
        dir.as_os_str(),
    ]);
    assert_eq!(code, 0, "{err}");
    let img = image::open(&out).unwrap().to_rgba8();
    assert_eq!(img.dimensions(), (17 * 16 * 4, 3 * 16 * 4), "4x");
    let scale = 4;
    let mut expected = image::RgbaImage::from_pixel(
        img.width(),
        img.height(),
        image::Rgba([0x20, 0x20, 0x20, 255]),
    );
    for b in 0u8..8 {
        let o = image::imageops::resize(
            &oriented(&tile, b),
            16 * scale,
            16 * scale,
            image::imageops::FilterType::Nearest,
        );
        let (x0, y0) = ((1 + 2 * b as u32) * 16 * scale, 16 * scale);
        image::imageops::replace(&mut expected, &o, x0 as i64, y0 as i64);
        let mut wrong = 0;
        for y in 0..16 * scale {
            for x in 0..16 * scale {
                if img.get_pixel(x0 + x, y0 + y) != o.get_pixel(x, y) {
                    wrong += 1;
                }
            }
        }
        assert_eq!(
            wrong,
            0,
            "flip bits {b} ({:?}): {wrong} pixels differ",
            Flip::from_bits(b)
        );
        // Nothing drawn outside the tile's cell (the cells around it are
        // background).
        for (x, y) in [
            (x0 - 1, y0 + 8),
            (x0 + 16 * scale, y0 + 8),
            (x0 + 8, y0 - 1),
            (x0 + 8, y0 + 16 * scale),
        ] {
            assert_eq!(
                img.get_pixel(x, y).0,
                [0x20, 0x20, 0x20, 255],
                "bits {b} spills at {x},{y}"
            );
        }
    }
    // The eyeball picture: render on top, expected below.
    let mut both = image::RgbaImage::new(img.width(), img.height() * 2);
    image::imageops::replace(&mut both, &img, 0, 0);
    image::imageops::replace(&mut both, &expected, 0, img.height() as i64);
    let pair = tmp.path().join("m3_orientations.png");
    both.save(&pair).unwrap();
    keep(&pair);
}

/// The Rules pane on level_4 with a rule selected: the pane is
/// drawn, and the selection's highlight changes the map compared with the
/// same view and no selected rule.
#[test]
fn rules_pane_highlights_the_selected_rule() {
    if !gpu_tests() {
        return;
    }
    let (tmp, dir) = temp_copy(&game());
    let shot = |name: &str, script: &str| {
        let path = tmp.path().join(format!("{name}.json"));
        std::fs::write(&path, script).unwrap();
        let out = tmp.path().join(format!("{name}.png"));
        let r = screenshot(&ScreenshotOptions {
            level: Some("level_4".into()),
            size: [1600, 900],
            script: Some(path),
            ..ScreenshotOptions::new(&dir, &out)
        })
        .unwrap();
        keep(&out);
        (image::open(&out).unwrap().to_rgba8(), r)
    };
    let (plain, r0) = shot(
        "m3_rules_none",
        r#"[{ "ui": "rules", "layer": "main_tiling", "level": "level_4" }]"#,
    );
    let (lit, r1) = shot(
        "m3_rules_selected",
        r#"[{ "ui": "rules", "layer": "main_tiling", "level": "level_4", "rule": "wall_fill" }]"#,
    );
    let map = r1.map_rect.expect("map drawn");
    let pane = r1.rules_rect.expect("rules pane drawn");
    assert_eq!(r0.map_rect, Some(map));
    assert!(!map.intersects(pane), "{map:?} {pane:?}");
    let px = |img: &image::RgbaImage, r: egui::Rect| {
        let (x0, y0) = (r.min.x.max(0.0) as u32, r.min.y.max(0.0) as u32);
        let (x1, y1) = (
            (r.max.x as u32).min(img.width()),
            (r.max.y as u32).min(img.height()),
        );
        (x0, y0, x1, y1)
    };
    // The highlight: many map pixels differ, and they carry the accent.
    let (x0, y0, x1, y1) = px(&lit, map);
    let mut changed = 0;
    for y in y0..y1 {
        for x in x0..x1 {
            if lit.get_pixel(x, y) != plain.get_pixel(x, y) {
                changed += 1;
            }
        }
    }
    assert!(changed > 2000, "only {changed} map pixels changed");
    // The pane has content: text, rows, the pattern grid.
    let (x0, y0, x1, y1) = px(&lit, pane);
    let crop = image::imageops::crop_imm(&lit, x0, y0, x1 - x0, y1 - y0).to_image();
    assert!(
        non_blank_fraction(&crop) > 0.05,
        "{}",
        non_blank_fraction(&crop)
    );
    let mut colors = std::collections::HashSet::new();
    for p in crop.pixels() {
        colors.insert(p.0);
    }
    assert!(
        colors.len() > 50,
        "only {} colors in the pane",
        colors.len()
    );
}

/// `--script` with a `set_rule` and a `ui` step renders the pane on the new
/// rule, and `--save` writes exactly `rules/main_tiling.json`.
#[test]
fn script_set_rule_and_ui_step_save_only_the_rules_file() {
    if !gpu_tests() {
        return;
    }
    let (tmp, dir) = temp_copy(&game());
    let files = |d: &std::path::Path| {
        let mut m = std::collections::BTreeMap::new();
        let mut stack = vec![d.to_path_buf()];
        while let Some(x) = stack.pop() {
            for e in std::fs::read_dir(&x).unwrap() {
                let p = e.unwrap().path();
                // `.worlds/` is the session's (lock, autosave), not the project.
                if p.file_name() == Some(std::ffi::OsStr::new(".worlds")) {
                    continue;
                }
                if p.is_dir() {
                    stack.push(p);
                } else {
                    m.insert(p.clone(), std::fs::read(&p).unwrap());
                }
            }
        }
        m
    };
    let before = files(&dir);
    let script = tmp.path().join("rule.json");
    std::fs::write(
        &script,
        r#"[
          { "op": "set_rule", "layer": "main_tiling", "group": "base_tile", "rule": "m3_probe", "index": 0,
            "def": { "slug": "m3_probe", "match": { "rows": ["*.*", ".#.", "*#*"] },
                     "out": { "tileset": "props", "tiles": [[1, 0]] }, "break": true } },
          { "ui": "rules", "layer": "main_tiling", "level": "level_4", "rule": "m3_probe" }
        ]"#,
    )
    .unwrap();
    let out = tmp.path().join("m3_rules_script.png");
    let r = screenshot(&ScreenshotOptions {
        level: Some("level_4".into()),
        size: [1600, 900],
        script: Some(script),
        save: true,
        ..ScreenshotOptions::new(&dir, &out)
    })
    .unwrap();
    keep(&out);
    assert_eq!(r.applied, 2);
    assert!(r.rules_rect.is_some(), "the Rules pane is shown");
    let img = image::open(&out).unwrap().to_rgba8();
    assert!(non_blank_fraction(&img) > 0.2);
    let after = files(&dir);
    let differ: Vec<_> = after
        .iter()
        .filter(|(p, b)| before.get(*p) != Some(b))
        .map(|(p, _)| p.strip_prefix(&dir).unwrap().to_path_buf())
        .collect();
    assert_eq!(differ, [PathBuf::from("rules/main_tiling.json")]);
    let saved: serde_json::Value =
        serde_json::from_slice(&after[&dir.join("rules/main_tiling.json")]).unwrap();
    assert_eq!(saved["groups"][0]["rules"][0]["slug"], "m3_probe");
}

/// The entity palette over 100 synthetic types: searching "door" with the
/// entity tool shows different rows than searching nothing, the map shows
/// entity boxes, and `--bench-palette` times one frame per keystroke.
#[test]
fn entity_palette_search_renders_and_benches() {
    if !gpu_tests() {
        return;
    }
    let (tmp, dir) = temp_copy(&game());
    let shot = |name: &str, search: &str, bench: usize| {
        let script = tmp.path().join(format!("{name}.json"));
        std::fs::write(
            &script,
            serde_json::json!([{ "ui": "palette", "search": search, "tool": true }]).to_string(),
        )
        .unwrap();
        let out = tmp.path().join(format!("{name}.png"));
        let report = screenshot(&ScreenshotOptions {
            level: Some("crevasse".into()),
            synthetic_entities: Some(100),
            script: Some(script),
            bench_frames: bench,
            bench_palette: (bench > 0).then(|| "door".to_string()),
            ..ScreenshotOptions::new(&dir, &out)
        })
        .unwrap();
        keep(&out);
        (image::open(&out).unwrap().to_rgba8(), report)
    };
    let (all, _) = shot("m4_palette_all", "", 0);
    let (door, report) = shot("m4_palette_door", "door", 6);
    // The palette (bottom left) differs; the map shows entities either way.
    let mut diff = 0;
    for y in 440..860 {
        for x in 0..310 {
            if all.get_pixel(x, y) != door.get_pixel(x, y) {
                diff += 1;
            }
        }
    }
    assert!(diff > 2000, "palette area changed in {diff} pixels");
    assert!(non_blank_fraction(&door) > 0.2);
    assert_eq!(report.ui_ms.len(), 6);
    assert!(report.ui_ms.iter().all(|t| t.is_finite() && *t > 0.0));
    // A generated project can't be saved.
    let r = screenshot(&ScreenshotOptions {
        synthetic_entities: Some(4),
        save: true,
        ..ScreenshotOptions::new(&dir, tmp.path().join("nosave.png"))
    });
    assert!(format!("{:#}", r.unwrap_err()).contains("can't be saved"));
}

/// An `entity` gesture places a room_entry in crevasse: the picture shows
/// it, and `--save` writes only that level's `entities.json`.
#[test]
fn entity_gesture_places_and_saves_only_entities_json() {
    if !gpu_tests() {
        return;
    }
    let (tmp, dir) = temp_copy(&game());
    let before = files_of(&dir);
    let script = tmp.path().join("place.json");
    std::fs::write(
        &script,
        serde_json::json!([
            { "gesture": "entity", "level": "crevasse", "path": [[30, 20]], "type": "room_entry",
              "fields": { "id": "west" } }
        ])
        .to_string(),
    )
    .unwrap();
    let out = tmp.path().join("m4_placed.png");
    let r = screenshot(&ScreenshotOptions {
        level: Some("crevasse".into()),
        script: Some(script),
        save: true,
        ..ScreenshotOptions::new(&dir, &out)
    })
    .unwrap();
    keep(&out);
    assert_eq!(r.applied, 1);
    let after = files_of(&dir);
    let changed: Vec<&PathBuf> = after
        .iter()
        .filter(|(p, b)| before.get(*p) != Some(*b))
        .map(|(p, _)| p)
        .collect();
    assert_eq!(
        changed,
        [&PathBuf::from("worlds/main/levels/crevasse/entities.json")],
        "{changed:?}"
    );
    let text =
        std::fs::read_to_string(dir.join("worlds/main/levels/crevasse/entities.json")).unwrap();
    assert!(text.contains("\"west\""), "{text}");
}

fn files_of(dir: &std::path::Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
    let mut out = std::collections::BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            // `.worlds/` is the session's (lock, autosave), not the project.
            if p.file_name() == Some(std::ffi::OsStr::new(".worlds")) {
                continue;
            }
            if p.is_dir() {
                stack.push(p);
            } else {
                out.insert(
                    p.strip_prefix(dir).unwrap().to_path_buf(),
                    std::fs::read(&p).unwrap(),
                );
            }
        }
    }
    out
}

/// A changed `entities/*.json` waits for the open gesture to end, then
/// applies as one "Disk:" entry with the file clean; undo dirties it.
#[test]
fn disk_entity_change_waits_for_the_gesture() {
    use worlds_editor::app::{AppOptions, EditorApp};
    use worlds_editor::headless::Headless;
    use worlds_editor::tools::{Grab, Tool, ToolEvent};

    if !gpu_tests() {
        return;
    }
    let (_tmp, dir) = temp_copy(&game());
    let hl = Headless::new([800, 500]).unwrap();
    let ctx = egui::Context::default();
    let mut app = EditorApp::new(
        &ctx,
        hl.gpu.clone(),
        worlds_editor::log::init(),
        AppOptions {
            headless: true,
            level: Some("crevasse".into()),
            ..Default::default()
        },
    );
    app.set_commands(Box::new(worlds_editor::edit::Editing::new()));
    app.open(&dir);
    let mut t = 0.0;
    let mut frame = |app: &mut EditorApp| {
        hl.frame(&ctx, app, t);
        t += 1.0 / 60.0;
    };
    frame(&mut app);
    // A brush stroke is open (Begin, no End yet).
    let doc = app.document().unwrap();
    let ix = doc.level_ix("crevasse").unwrap();
    let layer = doc
        .project
        .layers
        .iter()
        .position(|l| l.slug == "collision")
        .unwrap();
    let at = worlds_editor::script::cell_hit(doc, ix, layer, [3, 3]);
    app.st.view.active_level = Some(ix);
    app.st.view.active_layer = Some(layer);
    app.st.tool_events.push(ToolEvent::Begin {
        tool: Tool::Brush,
        at,
        modifiers: Default::default(),
        grab: Grab::None,
    });
    frame(&mut app);
    let path = dir.join("entities/torch.json");
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(
        &path,
        text.replace("\"name\": \"Torch\"", "\"name\": \"Brazier\""),
    )
    .unwrap();
    app.queue_disk_entities(vec![PathBuf::from("entities/torch.json")]);
    frame(&mut app);
    let name = |app: &EditorApp| {
        app.document()
            .unwrap()
            .project
            .entity_def("torch")
            .unwrap()
            .name
            .clone()
    };
    assert_eq!(name(&app), "Torch", "waits while the stroke is open");
    app.st.tool_events.push(ToolEvent::End {
        tool: Tool::Brush,
        start: at,
        at,
        modifiers: Default::default(),
    });
    frame(&mut app);
    assert_eq!(name(&app), "Brazier");
    let dirty = |app: &EditorApp| {
        app.document()
            .unwrap()
            .project
            .files
            .dirty()
            .contains(std::path::Path::new("entities/torch.json"))
    };
    assert!(!dirty(&app), "disk already holds it");
    app.run(worlds_editor::app::actions::Action::Undo);
    assert_eq!(name(&app), "Torch");
    assert!(dirty(&app), "undo: a save writes the old definition back");
}

/// An editor on a copy of the game fixture, offscreen, with the command layer.
fn app_on(
    dir: &std::path::Path,
    size: [u32; 2],
) -> (
    worlds_editor::headless::Headless,
    egui::Context,
    worlds_editor::app::EditorApp,
) {
    use worlds_editor::app::{AppOptions, EditorApp};
    let hl = worlds_editor::headless::Headless::new(size).unwrap();
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
    app.open(dir);
    (hl, ctx, app)
}

/// Two room exits in crevasse, the second with a size and a lock.
fn two_exits() -> Vec<worlds_editor::script::ScriptStep> {
    worlds_editor::script::parse(
        &serde_json::json!([
            { "op": "add_entity", "level": "crevasse", "layer": "entities",
              "entity": { "id": "exitwesta2", "type": "room_exit", "cell": [2, 22],
                          "fields": { "target": "entrymain2", "direction": "left" } } },
            { "op": "add_entity", "level": "crevasse", "layer": "entities",
              "entity": { "id": "exiteasta3", "type": "room_exit", "cell": [60, 22], "size": [1, 4],
                          "fields": { "target": "entrymain2", "direction": "right",
                                      "locked_by": "brass_key" } } },
            { "ui": "select", "entities": ["exitwesta2", "exiteasta3"] }
        ])
        .to_string(),
    )
    .unwrap()
}

/// Pixels that differ between two images inside `r` (screen points =
/// pixels headless).
fn diff_in(a: &image::RgbaImage, b: &image::RgbaImage, r: egui::Rect) -> usize {
    let mut n = 0;
    for y in r.min.y.max(0.0) as u32..(r.max.y as u32).min(a.height()) {
        for x in r.min.x.max(0.0) as u32..(r.max.x as u32).min(a.width()) {
            if a.get_pixel(x, y) != b.get_pixel(x, y) {
                n += 1;
            }
        }
    }
    n
}

/// D13: the `duplicate` and `delete` script steps are Ctrl-D and Delete:
/// one entry each, the copies selected; Delete with a level selected asks
/// first, which the step refuses without `confirm` (nothing deleted) and
/// answers yes with it.
#[test]
fn script_duplicate_and_delete_steps() {
    if !gpu_tests() {
        return;
    }
    let (_tmp, dir) = temp_copy(&game());
    let (hl, ctx, mut app) = app_on(&dir, [800, 600]);
    hl.frame(&ctx, &mut app, 0.0);
    let step = |app: &mut worlds_editor::app::EditorApp, s: &str| {
        let s = worlds_editor::script::parse(&format!("[{s}]")).unwrap();
        app.run_step(s[0].clone())
    };
    let e = step(&mut app, r#"{ "ui": "duplicate" }"#).unwrap_err();
    assert!(e.contains("Nothing selected"), "{e}");
    for s in two_exits() {
        app.run_step(s).unwrap();
    }
    let entries = |app: &mut worlds_editor::app::EditorApp| app.undo_entries();
    let n0 = entries(&mut app);
    step(&mut app, r#"{ "ui": "duplicate" }"#).unwrap();
    let copies = app.st.view.selection.entity_ids();
    assert_eq!(copies.len(), 2);
    assert!(app.document().unwrap().find_entity(&copies[0]).is_some());
    step(&mut app, r#"{ "ui": "delete" }"#).unwrap();
    assert!(app.document().unwrap().find_entity(&copies[0]).is_none());
    assert_eq!(entries(&mut app), n0 + 2);
    // A level: asks first.
    step(&mut app, r#"{ "ui": "select", "levels": ["crevasse"] }"#).unwrap();
    let e = step(&mut app, r#"{ "ui": "delete" }"#).unwrap_err();
    assert!(
        e.contains("asks first") && e.contains("Delete 1 level?"),
        "{e}"
    );
    assert!(app.document().unwrap().level_ix("crevasse").is_some());
    assert_eq!(entries(&mut app), n0 + 2);
    step(&mut app, r#"{ "ui": "delete", "confirm": true }"#).unwrap();
    assert!(app.document().unwrap().level_ix("crevasse").is_none());
    assert_eq!(entries(&mut app), n0 + 3);
}

/// `ui: select` of two room exits: both selected (the first primary), the
/// Inspector shows their common fields (one value mixed); Ctrl-D copies
/// them as one entry and selects the copies; Delete with a selection
/// removes it, and asks for the level only without one.
#[test]
fn multi_select_inspector_duplicate_and_delete() {
    use worlds_editor::app::Pane;
    use worlds_editor::app::actions::{Action, delete_action};
    if !gpu_tests() {
        return;
    }
    let (tmp, dir) = temp_copy(&game());
    let (hl, ctx, mut app) = app_on(&dir, [1600, 900]);
    let mut t = 0.0;
    let mut frames = |app: &mut worlds_editor::app::EditorApp, n: usize| {
        for _ in 0..n {
            hl.frame(&ctx, app, t);
            t += 1.0 / 60.0;
        }
    };
    frames(&mut app, 2);
    let before = hl.read().unwrap();
    for step in two_exits() {
        app.run_step(step).unwrap();
    }
    frames(&mut app, 3);
    let img = hl.read().unwrap();
    let out = tmp.path().join("m4_inspector_multi.png");
    img.save(&out).unwrap();
    keep(&out);
    let sel: Vec<String> = app
        .st
        .view
        .selection
        .entities()
        .map(|i| i.to_string())
        .collect();
    assert_eq!(sel, ["exitwesta2", "exiteasta3"]);
    let doc = app.document().unwrap();
    assert_eq!(app.st.view.active_level, doc.level_ix("crevasse"));
    // The Inspector drew the form: its area changed a lot.
    let insp = app.st.pane_rects[&Pane::Inspector];
    assert!(diff_in(&before, &img, insp) > 3000, "inspector area");
    // Unknown ids are an error naming them.
    let bad = worlds_editor::script::parse(r#"[{ "ui": "select", "entities": ["nopenopeno"] }]"#)
        .unwrap();
    let e = app.run_step(bad[0].clone()).unwrap_err();
    assert!(e.contains("nopenopeno"), "{e}");
    // A script select gesture with `press` grabs as the mouse does: on a
    // selected exit, the drag moves both, two cells right, one entry.
    let drag = worlds_editor::script::parse(
        r#"[{ "gesture": "select", "level": "crevasse", "layer": "entities",
              "path": [[2, 23], [3, 23], [4, 23]], "press": true }]"#,
    )
    .unwrap();
    app.run_step(drag[0].clone()).unwrap();
    let doc = app.document().unwrap();
    let cell = |s: &str| {
        doc.find_entity(&worlds_core::Id::new(s).unwrap())
            .unwrap()
            .2
            .cell
    };
    assert_eq!(
        (cell("exitwesta2"), cell("exiteasta3")),
        ([4, 22], [62, 22])
    );
    assert_eq!(app.st.view.selection.entity_ids().len(), 2);
    // Ctrl-D: the copies selected, fields kept.
    app.run(Action::DuplicateSelection);
    let copies = app.st.view.selection.entity_ids();
    assert_eq!(copies.len(), 2);
    assert!(!copies.iter().any(|c| c.as_str() == "exitwesta2"));
    let doc = app.document().unwrap();
    let (_, _, c0) = doc.find_entity(&copies[0]).unwrap();
    assert_eq!(c0.cell, [5, 22]);
    assert_eq!(c0.fields["target"], serde_json::json!("entrymain2"));
    // Delete with a selection removes it (not the level).
    assert_eq!(delete_action(&app.st.view), Action::DeleteSelection);
    app.run(Action::DeleteSelection);
    let doc = app.document().unwrap();
    assert!(doc.find_entity(&copies[0]).is_none());
    assert!(app.st.view.selection.is_empty());
    assert_eq!(delete_action(&app.st.view), Action::DeleteLevel);
    // Undo (the delete), undo (the copies): back to the two exits.
    app.run(Action::Undo);
    app.run(Action::Undo);
    let doc = app.document().unwrap();
    assert!(doc.find_entity(&copies[0]).is_none());
    assert!(
        doc.find_entity(&worlds_core::Id::new("exiteasta3").unwrap())
            .is_some()
    );
}

/// `ui: types` opens the Types pane on room_exit with a field's form; a
/// type or field it doesn't know is an error.
#[test]
fn types_pane_opens_on_a_type_and_field() {
    use worlds_editor::app::Pane;
    use worlds_editor::app::defs::TypesSel;
    if !gpu_tests() {
        return;
    }
    let (tmp, dir) = temp_copy(&game());
    let (hl, ctx, mut app) = app_on(&dir, [1600, 900]);
    let mut t = 0.0;
    hl.frame(&ctx, &mut app, t);
    let before = hl.read().unwrap();
    let steps = worlds_editor::script::parse(
        r#"[{ "ui": "types", "entity": "room_exit", "field": "target" }]"#,
    )
    .unwrap();
    for s in steps {
        app.run_step(s).unwrap();
    }
    for _ in 0..3 {
        t += 1.0 / 60.0;
        hl.frame(&ctx, &mut app, t);
    }
    let img = hl.read().unwrap();
    let out = tmp.path().join("m4_types_pane.png");
    img.save(&out).unwrap();
    keep(&out);
    assert_eq!(
        app.st.types.open,
        Some(TypesSel::Entity(
            worlds_core::Slug::new("room_exit").unwrap()
        ))
    );
    assert_eq!(
        app.st.types.field.as_ref().map(|s| s.as_str()),
        Some("target")
    );
    let pane = app.st.pane_rects[&Pane::EntityDefs];
    assert!(
        diff_in(&before, &img, pane) > 3000,
        "the Types pane is in front"
    );
    for bad in [
        r#"{ "ui": "types", "entity": "nope" }"#,
        r#"{ "ui": "types", "entity": "room_exit", "field": "nope" }"#,
        r#"{ "ui": "types" }"#,
        r#"{ "ui": "types", "entity": "room_exit", "level_fields": true }"#,
    ] {
        let s = worlds_editor::script::parse(&format!("[{bad}]")).unwrap();
        assert!(app.run_step(s[0].clone()).is_err(), "{bad}");
    }
    let s = worlds_editor::script::parse(r#"[{ "ui": "types", "level_fields": true }]"#).unwrap();
    app.run_step(s[0].clone()).unwrap();
    assert_eq!(app.st.types.open, Some(TypesSel::LevelFields));
}

/// Opening a type far down the Types list scrolls the list to it, and the
/// filter row stays at the top of the pane while the pane is scrolled to
/// the field form.
#[test]
fn types_pane_scrolls_to_the_opened_type_and_keeps_the_filter() {
    use worlds_editor::app::Pane;
    if !gpu_tests() {
        return;
    }
    let (_tmp, dir) = temp_copy(&game());
    let (hl, ctx, mut app) = app_on(&dir, [1600, 900]);
    let mut t = 0.0;
    hl.frame(&ctx, &mut app, t);
    // The last type by name, and its last field (the form is low down).
    let (ty, field) = {
        let doc = app.document().unwrap();
        let d = doc
            .project
            .entities
            .iter()
            .max_by_key(|d| d.name.to_lowercase())
            .unwrap();
        (
            d.slug.to_string(),
            d.fields.last().unwrap().slug.to_string(),
        )
    };
    let step = format!(r#"[{{ "ui": "types", "entity": "{ty}", "field": "{field}" }}]"#);
    let steps = worlds_editor::script::parse(&step).unwrap();
    app.run_step(steps[0].clone()).unwrap();
    for _ in 0..4 {
        t += 1.0 / 60.0;
        hl.frame(&ctx, &mut app, t);
    }
    let rects = &app.st.types.rects;
    let list = rects["types list"];
    let row = rects[&format!("type:{ty}")];
    assert!(
        list.contains(row.center()),
        "the open type's row {row:?} is in the list's view {list:?}"
    );
    let pane = app.st.pane_rects[&Pane::EntityDefs];
    let filter = rects["types filter"];
    assert!(
        pane.contains(filter.min) && pane.contains(filter.max),
        "the filter {filter:?} is in the pane {pane:?}"
    );
}

/// Keyboard-first placement as a script: `"enter": true` on a palette step
/// picks the search's first hit (Enter in the search box), and an entity
/// gesture without `type` places that brush. A search with no hit is an
/// error naming it.
#[test]
fn palette_enter_step_picks_the_first_hit_for_the_gesture() {
    use worlds_editor::tools::Tool;
    if !gpu_tests() {
        return;
    }
    let (_tmp, dir) = temp_copy(&game());
    let (hl, ctx, mut app) = app_on(&dir, [1600, 900]);
    hl.frame(&ctx, &mut app, 0.0);
    let steps = worlds_editor::script::parse(
        r#"[
          { "ui": "palette", "search": "leve", "enter": true },
          { "gesture": "entity", "level": "crevasse", "path": [[30, 20]] }
        ]"#,
    )
    .unwrap();
    let mut steps = steps.into_iter();
    assert_eq!(app.st.view.tool, Tool::Select);
    app.run_step(steps.next().unwrap()).unwrap();
    assert_eq!(app.st.view.tool, Tool::Entity);
    assert_eq!(
        app.st.view.entities.brush.as_ref().map(|b| b.ty.as_str()),
        Some("lever")
    );
    // Enter switched from Select to the entity tool, and the switch left
    // the keyboard on the map (it used to hand it back to the search box,
    // so the next Enter picked again instead of placing).
    assert!(!app.st.entities.focus);
    app.run_step(steps.next().unwrap()).unwrap();
    let doc = app.document().unwrap();
    let crevasse = doc.level(doc.level_ix("crevasse").unwrap());
    let levers: Vec<_> = crevasse
        .entities
        .iter()
        .flat_map(|l| &l.entities)
        .filter(|e| e.ty.as_str() == "lever")
        .collect();
    assert_eq!(levers.len(), 1);
    assert_eq!(levers[0].cell, [30, 20]);
    let s =
        worlds_editor::script::parse(r#"[{ "ui": "palette", "search": "qqzx", "enter": true }]"#)
            .unwrap();
    let e = app.run_step(s[0].clone()).unwrap_err();
    assert!(e.contains("qqzx"), "{e}");
}

/// A field set over its max shows up in the Problems pane without a save
/// (live problems); clicking it selects the entity and makes its level
/// active.
#[test]
fn problems_pane_lists_a_live_problem_and_a_click_selects_it() {
    use worlds_editor::app::Pane;
    if !gpu_tests() {
        return;
    }
    let (tmp, dir) = temp_copy(&game());
    let (hl, ctx, mut app) = app_on(&dir, [1600, 900]);
    let mut t = 0.0;
    hl.frame(&ctx, &mut app, t);
    assert!(app.document().unwrap().problems.is_empty());
    let steps = worlds_editor::script::parse(
        r#"[{ "op": "set_entity_field", "id": "buttonaaa2", "field": "channel", "value": 99 }]"#,
    )
    .unwrap();
    for s in steps {
        app.run_step(s).unwrap();
    }
    t += 1.0 / 60.0;
    hl.frame(&ctx, &mut app, t);
    let problems = app.document().unwrap().problems.clone();
    assert!(
        problems
            .iter()
            .any(|p| p.path.contains("entities[buttonaaa2]")),
        "{problems:?}"
    );
    assert!(app.live_problems_state().runs >= 1);
    // Click the first problem.
    t += 1.0 / 60.0;
    hl.frame(&ctx, &mut app, t);
    let pane = app.st.pane_rects[&Pane::Problems];
    let p = pane.min + egui::vec2(40.0, 10.0);
    for pressed in [true, false] {
        t += 1.0 / 60.0;
        hl.frame_with(
            &ctx,
            &mut app,
            t,
            vec![
                egui::Event::PointerMoved(p),
                egui::Event::PointerButton {
                    pos: p,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: Default::default(),
                },
            ],
        );
    }
    for _ in 0..2 {
        t += 1.0 / 60.0;
        hl.frame(&ctx, &mut app, t);
    }
    let doc = app.document().unwrap();
    assert_eq!(
        app.st.view.selection.entity_ids(),
        [worlds_core::Id::new("buttonaaa2").unwrap()]
    );
    assert_eq!(app.st.view.active_level, doc.level_ix("level_2"));
    let img = hl.read().unwrap();
    let out = tmp.path().join("m4_problems.png");
    img.save(&out).unwrap();
    keep(&out);
    // Undo: the problem goes again.
    app.run(worlds_editor::app::actions::Action::Undo);
    t += 1.0 / 60.0;
    hl.frame(&ctx, &mut app, t);
    assert!(app.document().unwrap().problems.is_empty());
}

/// `--hover` with `--zoom`: the picture's status bar reads the zoomed
/// cell, not the one before the zoom (it shows the hover the map worked
/// out the frame before).
#[test]
fn zoomed_hover_readout_is_current() {
    if !gpu_tests() {
        return;
    }
    let (tmp, dir) = temp_copy(&game());
    let r = screenshot(&ScreenshotOptions {
        level: Some("crevasse".into()),
        zoom_steps: 3,
        hover: Some([700.0, 300.0]),
        ..ScreenshotOptions::new(&dir, tmp.path().join("zoom.png"))
    })
    .unwrap();
    let shown = r.hover_shown.expect("a readout");
    let last = r.hover_last.expect("a hover");
    assert_eq!(shown.cell, last.cell);
    assert_eq!(shown.world_px, last.world_px);
}

/// A reload starts the document's counters over (`defs_gen`, the edit
/// revision): the palette must not keep rows built from the old document
/// under a key that matches again. A type added on disk is found by the
/// same search after the reload.
#[test]
fn the_palette_forgets_the_old_document_on_reload() {
    if !gpu_tests() {
        return;
    }
    let (_tmp, dir) = temp_copy(&game());
    let (hl, ctx, mut app) = app_on(&dir, [1600, 900]);
    hl.frame(&ctx, &mut app, 0.0);
    let search =
        worlds_editor::script::parse(r#"[{ "ui": "palette", "search": "zqlamp", "enter": true }]"#)
            .unwrap();
    assert!(app.run_step(search[0].clone()).is_err(), "no such type yet");
    let torch = std::fs::read_to_string(dir.join("entities/torch.json")).unwrap();
    std::fs::write(
        dir.join("entities/zqlamp.json"),
        torch
            .replace("\"slug\": \"torch\"", "\"slug\": \"zqlamp\"")
            .replace("\"name\": \"Torch\"", "\"name\": \"Zqlamp\""),
    )
    .unwrap();
    app.reload();
    hl.frame(&ctx, &mut app, 0.1);
    assert!(
        app.document()
            .unwrap()
            .project
            .entity_def("zqlamp")
            .is_some()
    );
    app.run_step(search[0].clone()).unwrap();
    assert_eq!(
        app.st.view.entities.brush.as_ref().map(|b| b.ty.as_str()),
        Some("zqlamp")
    );
}

/// A queued disk change to an entity file that an edit made dirty before
/// the drain (a panel command or agent request runs first in the frame)
/// isn't applied over that edit: the reload prompt asks instead.
#[test]
fn a_queued_disk_change_never_overwrites_a_newer_edit() {
    if !gpu_tests() {
        return;
    }
    let (_tmp, dir) = temp_copy(&game());
    let (hl, ctx, mut app) = app_on(&dir, [800, 500]);
    let mut t = 0.0;
    let mut frame = |app: &mut worlds_editor::app::EditorApp| {
        hl.frame(&ctx, app, t);
        t += 1.0 / 60.0;
    };
    frame(&mut app);
    let path = dir.join("entities/torch.json");
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(
        &path,
        text.replace("\"name\": \"Torch\"", "\"name\": \"Brazier\""),
    )
    .unwrap();
    // Queued while clean (as the watcher does), then edited.
    app.queue_disk_entities(vec![PathBuf::from("entities/torch.json")]);
    let steps = worlds_editor::script::parse(
        r#"[{ "op": "set_entity_field_def", "entity": "torch", "field": "glow",
              "def": { "slug": "glow", "type": "bool" } }]"#,
    )
    .unwrap();
    app.run_step(steps[0].clone()).unwrap();
    frame(&mut app);
    let def = app.document().unwrap().project.entity_def("torch").unwrap();
    assert_eq!(def.name, "Torch", "the disk version wasn't applied");
    assert!(def.field("glow").is_some(), "the edit is kept");
    assert!(
        app.document()
            .unwrap()
            .project
            .files
            .dirty()
            .contains(std::path::Path::new("entities/torch.json")),
        "still unsaved"
    );
    assert!(app.reload_prompt_pending(), "the conflict is asked about");
}

/// An entity file that doesn't load is a problem until it loads again,
/// and it stays listed through edits and saves (live problems revalidate
/// the project; the file's problem is kept beside them).
#[test]
fn a_broken_entity_file_stays_a_problem_until_it_loads() {
    if !gpu_tests() {
        return;
    }
    let (_tmp, dir) = temp_copy(&game());
    let (hl, ctx, mut app) = app_on(&dir, [800, 500]);
    let mut t = 0.0;
    let mut frame = |app: &mut worlds_editor::app::EditorApp| {
        hl.frame(&ctx, app, t);
        t += 1.0 / 60.0;
    };
    frame(&mut app);
    let path = dir.join("entities/torch.json");
    let good = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, "{ not json").unwrap();
    app.queue_disk_entities(vec![PathBuf::from("entities/torch.json")]);
    frame(&mut app);
    let listed = |app: &worlds_editor::app::EditorApp| {
        app.document()
            .unwrap()
            .problems
            .iter()
            .filter(|p| p.file == "entities/torch.json")
            .count()
    };
    assert_eq!(listed(&app), 1);
    // An edit revalidates: the file's problem is still there.
    let steps = worlds_editor::script::parse(
        r#"[{ "op": "set_entity_field", "id": "buttonaaa2", "field": "channel", "value": 2 }]"#,
    )
    .unwrap();
    app.run_step(steps[0].clone()).unwrap();
    frame(&mut app);
    assert!(app.live_problems_state().runs >= 1);
    assert_eq!(listed(&app), 1, "kept through a revalidation");
    app.save().unwrap();
    assert_eq!(listed(&app), 1, "and through a save");
    // The file is fixed: its problem goes.
    std::fs::write(&path, good.replace("\"Torch\"", "\"Lamp\"")).unwrap();
    app.queue_disk_entities(vec![PathBuf::from("entities/torch.json")]);
    frame(&mut app);
    assert_eq!(listed(&app), 0);
    assert_eq!(
        app.document()
            .unwrap()
            .project
            .entity_def("torch")
            .unwrap()
            .name,
        "Lamp"
    );
}

// ---------------------------------------------------------------- notes and guides

/// The sides the layout guides are tagged with, one box each.
const SIDES: [&str; 4] = ["north", "east", "south", "west"];

/// A script step adding layout guides to world `main` as one entry: per
/// side a locked box and a crossbar, tagged `layout` and the side, the
/// first side's in the guide group `shifted` (offset 769 px down).
fn layout_guides() -> String {
    let mut cmds = vec![
        r#"{ "op": "set_guide_group", "world": "main", "group": { "slug": "shifted", "offset": [0, 769] } }"#
            .to_string(),
    ];
    let mut n = 0;
    for (i, side) in SIDES.iter().enumerate() {
        let x0 = -224 + 1200 * i as i64;
        let x1 = x0 + 800;
        let group = if i == 0 {
            r#", "group": "shifted""#
        } else {
            ""
        };
        for points in [
            format!("[[{x0}, -1600], [{x0}, -900], [{x1}, -900], [{x1}, -1600]]"),
            format!("[[{x0}, -1250], [{x1}, -1250]]"),
        ] {
            let id = format!(
                "guidelay{}{}",
                (b'a' + n / 26) as char,
                (b'a' + n % 26) as char
            );
            n += 1;
            cmds.push(format!(
                r#"{{ "op": "add_note", "scope": {{ "world": "main" }}, "note": {{ "id": "{id}", "kind": "guide",
                  "locked": true, "points": {points}, "tags": ["layout", "{side}"]{group} }} }}"#
            ));
        }
    }
    format!(
        r#"{{ "op": "batch", "label": "Add layout guides", "commands": [{}] }}"#,
        cmds.join(", ")
    )
}

/// Whether a pixel is the guide cyan (#4fc3f7), allowing for blending at
/// line edges.
fn cyan(p: &image::Rgba<u8>) -> bool {
    (p[0] as i32 - 0x4f).abs() < 24
        && (p[1] as i32 - 0xc3).abs() < 24
        && (p[2] as i32 - 0xf7).abs() < 24
}

fn count_in(img: &image::RgbaImage, r: egui::Rect, f: impl Fn(&image::Rgba<u8>) -> bool) -> usize {
    let mut n = 0;
    for y in r.min.y.max(0.0) as u32..(r.max.y.max(0.0) as u32).min(img.height()) {
        for x in r.min.x.max(0.0) as u32..(r.max.x.max(0.0) as u32).min(img.width()) {
            if f(img.get_pixel(x, y)) {
                n += 1;
            }
        }
    }
    n
}

/// Run script steps (JSON text of one array) on the app.
fn steps(app: &mut worlds_editor::app::EditorApp, json: &str) {
    for s in worlds_editor::script::parse(json).unwrap() {
        app.run_step(s).unwrap();
    }
}

/// Screen rect (points = pixels headless) of world rect `[x0, y0, x1, y1]`
/// in the app's map as of the last frame.
fn screen_of(app: &worlds_editor::app::EditorApp, r: [f64; 4]) -> egui::Rect {
    let map = app.st.map_rect.unwrap();
    let cam = app.st.viewport.camera;
    let a = cam.world_to_screen_snapped([r[0], r[1]]);
    let b = cam.world_to_screen_snapped([r[2], r[3]]);
    egui::Rect::from_min_max(
        egui::pos2(map.min.x + a[0] as f32, map.min.y + a[1] as f32),
        egui::pos2(map.min.x + b[0] as f32, map.min.y + b[1] as f32),
    )
}

/// The world box of the guides with `tag` (as built, or with `designed`
/// without group offsets).
fn guides_box(app: &worlds_editor::app::EditorApp, tag: &str, designed: bool) -> [f64; 4] {
    let doc = app.document().unwrap();
    let w = &doc.project.worlds[0];
    let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    for a in &w.notes {
        if !a.tags.iter().any(|t| t == tag) {
            continue;
        }
        let off = match &a.shape {
            worlds_core::Shape::Guide { group: Some(g), .. } if !designed => {
                w.guide_group(g.as_str()).unwrap().offset
            }
            _ => [0, 0],
        };
        for p in a.shape.points() {
            b[0] = b[0].min((p[0] + off[0]) as f64);
            b[1] = b[1].min((p[1] + off[1]) as f64);
            b[2] = b[2].max((p[0] + off[0]) as f64);
            b[3] = b[3].max((p[1] + off[1]) as f64);
        }
    }
    b
}

/// Layout guides on the game fixture: guide pixels inside the projected box of each
/// side, none once `layout` is hidden; "as designed" adds dashes only where
/// the grouped guides were laid out.
#[test]
fn layout_guides_render_and_hide_with_their_tag() {
    if !gpu_tests() {
        return;
    }
    let (tmp, dir) = temp_copy(&game());
    let (hl, ctx, mut app) = app_on(&dir, [1600, 900]);
    steps(
        &mut app,
        &format!(
            "[{}, {{ \"ui\": \"focus\", \"world_rect\": [-700, -2000, 5600, 2700] }}]",
            layout_guides()
        ),
    );
    let mut t = 0.0;
    let mut frames = |app: &mut worlds_editor::app::EditorApp, n: usize| {
        for _ in 0..n {
            hl.frame(&ctx, app, t);
            t += 1.0 / 60.0;
        }
    };
    frames(&mut app, 3);
    let built = hl.read().unwrap();
    let out = tmp.path().join("guides_world.png");
    built.save(&out).unwrap();
    keep(&out);
    for side in SIDES {
        let r = screen_of(&app, guides_box(&app, side, false)).expand(2.0);
        let n = count_in(&built, r, cyan);
        assert!(n > 100, "{side}: {n} guide pixels in {r:?}");
    }
    let map = app.st.map_rect.unwrap();
    // As designed: dashes where the grouped guides were laid out, nothing
    // else changes.
    steps(&mut app, r#"[{ "ui": "notes", "show_designed": true }]"#);
    frames(&mut app, 2);
    let designed = hl.read().unwrap();
    let out = tmp.path().join("guides_designed.png");
    designed.save(&out).unwrap();
    keep(&out);
    let grouped = {
        let doc = app.document().unwrap();
        let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
        for a in &doc.project.worlds[0].notes {
            if matches!(&a.shape, worlds_core::Shape::Guide { group: Some(_), .. }) {
                for p in a.shape.points() {
                    b = [
                        b[0].min(p[0] as f64),
                        b[1].min(p[1] as f64),
                        b[2].max(p[0] as f64),
                        b[3].max(p[1] as f64),
                    ];
                }
            }
        }
        screen_of(&app, b).expand(3.0)
    };
    let mut inside = 0;
    for y in map.min.y as u32..map.max.y as u32 {
        for x in map.min.x as u32..map.max.x as u32 {
            if built.get_pixel(x, y) != designed.get_pixel(x, y) {
                assert!(
                    grouped.contains(egui::pos2(x as f32 + 0.5, y as f32 + 0.5)),
                    "({x}, {y}) differs outside the grouped guides' designed box {grouped:?}"
                );
                inside += 1;
            }
        }
    }
    assert!(inside > 50, "{inside} designed pixels");
    // Hidden by its tag: no guide pixel anywhere on the map.
    steps(
        &mut app,
        r#"[{ "ui": "notes", "show_designed": false, "hidden_tags": ["layout"] }]"#,
    );
    frames(&mut app, 2);
    let hidden = hl.read().unwrap();
    let out = tmp.path().join("guides_hidden.png");
    hidden.save(&out).unwrap();
    keep(&out);
    assert_eq!(count_in(&hidden, map, cyan), 0);
}

/// `--snapshot` with overlays draws annotations (every guide; open notes),
/// whatever the view hides.
#[test]
fn snapshots_draw_annotations() {
    if !gpu_tests() {
        return;
    }
    let (tmp, dir) = temp_copy(&game());
    let shot = |name: &str| {
        let out = tmp.path().join(name);
        let (code, _, err) = worlds(&[
            os("--screenshot"),
            out.as_os_str(),
            os("--snapshot"),
            os("--world-rect=-700,-2000,5600,2700"),
            dir.as_os_str(),
        ]);
        assert_eq!(code, 0, "{err}");
        keep(&out);
        image::open(&out).unwrap().to_rgba8()
    };
    let before = shot("m5_snapshot_before.png");
    let script = tmp.path().join("guides.json");
    std::fs::write(&script, format!("[{}]", layout_guides())).unwrap();
    let (code, _, err) = worlds(&[
        os("--screenshot"),
        tmp.path().join("x.png").as_os_str(),
        os("--size"),
        os("320x200"),
        os("--script"),
        script.as_os_str(),
        os("--save"),
        dir.as_os_str(),
    ]);
    assert_eq!(code, 0, "{err}");
    let after = shot("m5_snapshot_guides.png");
    let all = egui::Rect::from_min_size(
        egui::Pos2::ZERO,
        egui::vec2(after.width() as f32, after.height() as f32),
    );
    assert_eq!(count_in(&before, all, cyan), 0);
    assert!(
        count_in(&after, all, cyan) > 500,
        "{}",
        count_in(&after, all, cyan)
    );
}

/// A note is a card from 12 screen px per cell, a dot below.
#[test]
fn notes_are_cards_zoomed_in_and_dots_zoomed_out() {
    if !gpu_tests() {
        return;
    }
    let (tmp, dir) = temp_copy(&game());
    let note = r#"{ "op": "add_note", "scope": { "level": "crevasse" }, "note": { "id": "cardnote22",
        "kind": "note", "title": "Pin the old statue", "body": "In the forest.", "px": [640, 320],
        "tags": ["todo"] } }"#;
    let render = |zoom: i32, with: bool, name: &str| {
        let (hl, ctx, mut app) = app_on(&dir, [1200, 800]);
        let mut s = String::from("[");
        if with {
            s.push_str(note);
            s.push(',');
        }
        s.push_str(r#"{ "ui": "focus", "level": "crevasse" }]"#);
        steps(&mut app, &s);
        for i in 0..3 {
            hl.frame(&ctx, &mut app, i as f64 / 60.0);
        }
        app.st.viewport.zoom_steps_centered(zoom);
        for i in 3..6 {
            hl.frame(&ctx, &mut app, i as f64 / 60.0);
        }
        let img = hl.read().unwrap();
        let out = tmp.path().join(name);
        img.save(&out).unwrap();
        keep(&out);
        let crev = app.document().unwrap().level_ix("crevasse").unwrap();
        let o = app.document().unwrap().level(crev).world_px;
        let p = [(o[0] + 640) as f64, (o[1] + 320) as f64];
        let at = screen_of(&app, [p[0], p[1], p[0], p[1]]).min;
        (img, at, app.st.viewport.camera.zoom)
    };
    let (with, at, z) = render(3, true, "m5_cards.png");
    let (without, _, _) = render(3, false, "m5_cards_without.png");
    assert!(z * 32.0 >= 12.0, "zoom {z}");
    let r = egui::Rect::from_min_size(at - egui::vec2(4.0, 4.0), egui::vec2(200.0, 60.0));
    let card = diff_in(&with, &without, r);
    assert!(card > 2500, "card: {card} pixels");
    let (with, at, z) = render(-4, true, "m5_dots.png");
    let (without, _, _) = render(-4, false, "m5_dots_without.png");
    assert!(z * 32.0 < 12.0, "zoom {z}");
    let r = egui::Rect::from_min_size(at - egui::vec2(10.0, 10.0), egui::vec2(200.0, 60.0));
    let dot = diff_in(&with, &without, r);
    assert!(dot > 0 && dot < 120, "dot: {dot} pixels");
}

/// The annotation editing path end to end: layout guides added, a world marquee
/// over two levels and their drag snapping to a guide (one "Move 2
/// levels"), tile stacks marqueed, dragged, duplicated and deleted, a pin
/// and a polygon from the Note tool; then `undo` back to the start and
/// `--save`: every file as it was.
#[test]
fn script_steps_drive_annotations_and_undo_restores_every_file() {
    use worlds_editor::app::actions::Action;
    if !gpu_tests() {
        return;
    }
    let (_tmp, dir) = temp_copy(&game());
    let before = files_of(&dir);
    let (hl, ctx, mut app) = app_on(&dir, [1600, 900]);
    steps(&mut app, &format!("[{}]", layout_guides()));
    // level_1 at (2048, -1280), level_2 at (2656, -1120): a marquee from
    // empty world space above-left of level_1 over both.
    let doc = app.document().unwrap();
    let l1 = doc.level(doc.level_ix("level_1").unwrap()).world_px;
    // The vertical guide segments' x, as built.
    let mut xs: Vec<i64> = Vec::new();
    for a in &doc.project.worlds[0].notes {
        // Ungrouped guides: their x is the same as built.
        if let worlds_core::Shape::Guide {
            points,
            group: None,
            ..
        } = &a.shape
        {
            for w in points.windows(2) {
                if w[0][0] == w[1][0] {
                    xs.push(w[0][0]);
                }
            }
        }
    }
    // The vertical segment nearest level_1's left edge.
    let target = xs
        .iter()
        .copied()
        .min_by_key(|&x| (x - l1[0]).abs())
        .expect("a segment");
    steps(
        &mut app,
        r#"[{ "gesture": "select", "world_px": [[2052, -1292], [2700, -1100]], "press": true }]"#,
    );
    let sel = worlds_editor::live::selection_of(app.document().unwrap(), &app.st.view);
    assert_eq!(sel.3, ["level_1", "level_2"]);
    // Drag level_1's body so its left edge lands 5 px right of the segment
    // (at 1x before any frame: within 8 screen px).
    let d = (target + 5 - l1[0]) as f64;
    steps(
        &mut app,
        &format!(
            r#"[{{ "gesture": "select", "world_px": [[2100, -1200], [{}, -1200]], "press": true }}]"#,
            2100.0 + d
        ),
    );
    let doc = app.document().unwrap();
    let l1b = doc.level(doc.level_ix("level_1").unwrap()).world_px;
    assert_eq!(l1b[0], target, "the left edge on the segment");
    let l2a = [2656, -1120];
    let l2b = doc.level(doc.level_ix("level_2").unwrap()).world_px;
    assert_eq!(l2b[0] - l2a[0], l1b[0] - l1[0], "one delta");
    // Tiles on crevasse's decor_back: a marquee from an empty cell.
    let doc = app.document().unwrap();
    let crev = doc.level(doc.level_ix("crevasse").unwrap());
    let stacks: Vec<[u32; 2]> = crev
        .tiles_layer("decor_back")
        .map(|t| t.tiles.iter().map(|p| p.cell).collect())
        .unwrap_or_default();
    assert!(!stacks.is_empty());
    let c0 = stacks[0];
    steps(
        &mut app,
        &format!(
            r#"[{{ "ui": "select", "tiles": {{ "level": "crevasse", "layer": "decor_back", "cells": [[{}, {}]] }} }}]"#,
            c0[0], c0[1]
        ),
    );
    app.run(Action::DuplicateSelection);
    app.run(Action::DeleteSelection);
    // Note tool: a pin and a polygon region, world scope.
    steps(
        &mut app,
        r#"[
          { "gesture": "note", "kind": "pin", "world_px": [[256, -1700]], "icon": "statue", "scope": "world" },
          { "gesture": "note", "kind": "region", "polygon": true,
            "world_px": [[0, -1900], [600, -1900], [600, -1500]] },
          { "gesture": "measure", "world_px": [[0, 0], [320, 0]] }
        ]"#,
    );
    // import, move levels, dup, delete, pin, region: 6 entries.
    for i in 0..3 {
        hl.frame(&ctx, &mut app, i as f64 / 60.0);
    }
    steps(&mut app, r#"[{ "undo": 6 }]"#);
    assert!(steps_err(&mut app, r#"[{ "undo": 1 }]"#).contains("undo"));
    app.save().unwrap();
    let after = files_of(&dir);
    for (p, b) in &before {
        if p.components().any(|c| c.as_os_str() == ".worlds") {
            continue;
        }
        assert_eq!(after.get(p), Some(b), "{}", p.display());
    }
}

fn steps_err(app: &mut worlds_editor::app::EditorApp, json: &str) -> String {
    let mut err = String::new();
    for s in worlds_editor::script::parse(json).unwrap() {
        if let Err(e) = app.run_step(s) {
            err = e;
        }
    }
    err
}

/// A short bench over synthetic guides and notes renders and times frames,
/// and the synthetic project refuses to be saved.
#[test]
fn synthetic_guides_and_notes_bench() {
    if !gpu_tests() {
        return;
    }
    let (tmp, dir) = temp_copy(&game());
    let out = tmp.path().join("m5_bench.png");
    let r = screenshot(&ScreenshotOptions {
        size: [800, 450],
        synthetic_guides: Some(200),
        synthetic_notes: Some(50),
        bench_frames: 6,
        ..ScreenshotOptions::new(&dir, &out)
    })
    .unwrap();
    keep(&out);
    assert_eq!(r.ui_ms.len(), 6);
    let img = image::open(&out).unwrap().to_rgba8();
    let all = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 450.0));
    assert!(count_in(&img, all, cyan) > 200);
    let e = screenshot(&ScreenshotOptions {
        size: [320, 200],
        synthetic_notes: Some(4),
        save: true,
        ..ScreenshotOptions::new(&dir, tmp.path().join("x.png"))
    });
    assert!(e.is_err());
    // Deterministic: the same N gives the same annotations.
    let mut a = worlds_core::load(&dir).unwrap();
    let mut b = a.clone();
    worlds_editor::doc::synthesize_notes(&mut a, 20);
    worlds_editor::doc::synthesize_guides(&mut a, 20);
    worlds_editor::doc::synthesize_notes(&mut b, 20);
    worlds_editor::doc::synthesize_guides(&mut b, 20);
    assert_eq!(a.worlds[0].notes, b.worlds[0].notes);
    assert_eq!(a.worlds[0].notes.len(), 20 + 2 + 20);
}

/// D11: in a `--screenshot --script` run, "within 8 screen px" is worked
/// out at the zoom of the picture: a drag that leaves level_1's left edge
/// 12 world px short of a vertical guide (x 2212) snaps at the
/// world fit (17%: 8 screen px is about 47 world px), and doesn't once a
/// `focus` step has zoomed in on it (8 screen px is then 2 or 3 world px).
#[test]
fn guide_snapping_uses_the_scripts_view() {
    if !gpu_tests() {
        return;
    }
    let run = |focus: Option<&str>| {
        let (tmp, dir) = temp_copy(&game());
        let script = tmp.path().join("far.json");
        std::fs::write(
            &script,
            format!(
                r#"[
                  {{"op": "add_note", "scope": {{"world": "main"}}, "note": {{"id": "guidestem7", "kind": "guide",
                    "locked": true, "points": [[2212, -1600], [2212, -900]], "tags": ["layout"]}}}},
                  {{"gesture": "select", "world_px": [[2100, -1350], [2700, -1000]], "press": true}},
                  {}
                  {{"gesture": "select", "world_px": [[2500, -950], [2652, -950]], "press": true}}
                ]"#,
                focus.map_or(String::new(), |f| format!("{f},"))
            ),
        )
        .unwrap();
        let out = tmp.path().join("far.png");
        screenshot(&ScreenshotOptions {
            size: [1600, 900],
            script: Some(script),
            save: true,
            ..ScreenshotOptions::new(&dir, &out)
        })
        .unwrap();
        let lv: serde_json::Value = serde_json::from_slice(
            &std::fs::read(dir.join("worlds/main/levels/level_1/level.json")).unwrap(),
        )
        .unwrap();
        lv["world_px"][0].as_i64().unwrap()
    };
    assert_eq!(run(None), 2212, "snapped at the world fit");
    assert_eq!(
        run(Some(
            r#"{"ui": "focus", "world_rect": [2100, -1400, 300, 200]}"#
        )),
        2208,
        "12 px is too far once zoomed in: the grid snap (152 px rounds to 160)"
    );
}

// ---------------------------------------------------------------------
// Derived layers, region rules, occupancy (the game fixture plus two
// rules that place pillars)
// ---------------------------------------------------------------------

/// An authoring batch: `pillar_marks` (derived) and
/// `pillars` (auto) with the region rule writing marks along the floor of
/// rooms 15+ wide and the pass 1 stamp rule drawing a pillar on each.
const M6_AUTHOR: &str = r##"
  { "op": "batch", "label": "Pillars from two rules", "commands": [
    { "op": "set_layer_def", "layer": "pillar_marks", "index": 2,
      "def": { "slug": "pillar_marks", "name": "Pillar marks", "kind": "int_grid", "derived": true,
               "values": [{ "slug": "pillar_base", "name": "Pillar base", "color": "#E0A040", "glyph": "p" }] } },
    { "op": "set_layer_def", "layer": "pillars", "index": 1,
      "def": { "slug": "pillars", "name": "Pillars", "kind": "auto", "source": "collision" } },
    { "op": "set_stamp", "layer": "pillars", "stamp": "pillar",
      "def": { "slug": "pillar", "tileset": "tiles", "rect": [34, 18, 2, 4], "pivot": [0, 1.0], "occupies": true } },
    { "op": "set_rule_group", "layer": "pillars", "group": "marks", "index": 0,
      "def": { "slug": "marks", "rules": [
        { "slug": "big_room_pillars",
          "region": { "cells": ".", "min_width": 15 },
          "place": { "along": "floor", "every": 6, "margin": 3, "align": "center" },
          "write": { "grid": "pillar_marks", "value": "pillar_base" } } ] } },
    { "op": "set_rule_group", "layer": "pillars", "group": "art", "index": 1,
      "def": { "slug": "art", "pass": 1, "rules": [
        { "slug": "pillar_stamp",
          "match": { "grid": "pillar_marks", "rows": ["p"] },
          "require": { "legend": { ".": "[.|oneway]" }, "rows": ["..", "..", "..", "@."] },
          "out": { "stamp": "pillar", "avoid_overlap": true } } ] } }
  ] }"##;

/// An override paint: two marks by hand in crevasse (the second
/// one blocked by the first's footprint) and one computed mark suppressed.
const M6_OVERRIDES: &str = r#"
  { "op": "paint_cells", "level": "crevasse", "layer": "pillar_marks",
    "cells": [[6, 24, "pillar_base"], [7, 24, "pillar_base"], [58, 24, "!"]] }"#;

/// Render `steps` (JSON items, after the authoring batch unless they
/// start with `!`, which is dropped) at 1600x900.
fn m6_shot(
    tmp: &std::path::Path,
    dir: &std::path::Path,
    name: &str,
    level: Option<&str>,
    steps: &str,
    save: bool,
) -> (image::RgbaImage, worlds_editor::headless::Report) {
    let script = tmp.join(format!("{name}.json"));
    let body = if let Some(own) = steps.trim().strip_prefix('!') {
        format!("[{own}]")
    } else if steps.trim().is_empty() {
        format!("[{M6_AUTHOR}]")
    } else {
        format!("[{M6_AUTHOR}, {steps}]")
    };
    std::fs::write(&script, body).unwrap();
    let out = tmp.join(format!("{name}.png"));
    let r = screenshot(&ScreenshotOptions {
        level: level.map(str::to_string),
        size: [1600, 900],
        script: Some(script),
        save,
        ..ScreenshotOptions::new(dir, &out)
    })
    .unwrap();
    keep(&out);
    (image::open(&out).unwrap().to_rgba8(), r)
}

/// The derived overlay in `--snapshot` (the same renderer as `view.snapshot`
/// and the window): a computed mark is the value's color, faint; an override
/// is the full color; a suppressed cell a grey cross; other cells nothing.
#[test]
fn m6_derived_overlay_computed_faint_override_solid_suppressed_crossed() {
    if !gpu_tests() {
        return;
    }
    let (tmp, dir) = temp_copy(&game());
    m6_shot(
        tmp.path(),
        &dir,
        "m6_author_overrides",
        Some("crevasse"),
        M6_OVERRIDES,
        true,
    );
    let snap = tmp.path().join("m6_snapshot_marks.png");
    let (code, _, err) = worlds(&[
        os("--screenshot"),
        snap.as_os_str(),
        os("--snapshot"),
        os("--level"),
        os("crevasse"),
        os("--max-size"),
        os("2048x896"),
        os("--layers"),
        os("pillar_marks"),
        dir.as_os_str(),
    ]);
    assert_eq!(code, 0, "{err}");
    keep(&snap);
    let img = image::open(&snap).unwrap().to_rgba8();
    assert_eq!(img.dimensions(), (2048, 896), "one pixel per level pixel");
    let center = |x: u32, y: u32| img.get_pixel(x * 32 + 16, y * 32 + 16).0;
    let near = |x: u32, y: u32| img.get_pixel(x * 32 + 5, y * 32 + 16).0;
    let empty = center(20, 10);
    let override_ = center(6, 24);
    let computed = center(10, 24);
    let suppressed = center(58, 24);
    assert_eq!(
        &override_[..3],
        &[0xE0, 0xA0, 0x40],
        "override: the full color"
    );
    // Faint: between the empty background and the full color, nearer the
    // background than an opaque fill would be.
    for c in 0..3 {
        let (lo, hi) = (empty[c].min(override_[c]), empty[c].max(override_[c]));
        assert!(
            computed[c] >= lo && computed[c] <= hi,
            "{computed:?} {empty:?}"
        );
    }
    assert_ne!(computed, empty);
    assert_ne!(computed, override_);
    // The 1 px outline of an override is darker than its fill.
    let edge = img.get_pixel(6 * 32, 24 * 32 + 16).0;
    assert!(edge[0] < override_[0] / 2, "{edge:?}");
    // Suppressed: grey in the middle of the cross, background beside it.
    assert!(
        suppressed[0] > 140 && (suppressed[0] as i32 - suppressed[2] as i32).abs() < 12,
        "{suppressed:?}"
    );
    assert_eq!(near(58, 24), empty);
    // A cell with no mark and no override draws nothing.
    assert_eq!(center(30, 24), empty);
}

/// The Rules pane on the region rule outlines its regions (the 27 x 18
/// room in the accent, the closet dim) and runs; "Show occupancy" hatches
/// the stamps' footprints.
#[test]
fn m6_region_highlight_and_occupancy_render() {
    if !gpu_tests() {
        return;
    }
    let (tmp, dir) = temp_copy(&game());
    let shot =
        |name: &str, steps: &str| m6_shot(tmp.path(), &dir, name, Some("level_4"), steps, false);
    let (plain, r) = shot(
        "m6_regions_off",
        r#"{ "ui": "rules", "layer": "pillars", "level": "level_4", "rule": "big_room_pillars", "show_regions": false }"#,
    );
    let (lit, _) = shot(
        "m6_regions_on",
        r#"{ "ui": "rules", "layer": "pillars", "level": "level_4", "rule": "big_room_pillars", "show_regions": true }"#,
    );
    let map = r.map_rect.expect("map drawn");
    let changed = diff_in(&plain, &lit, map);
    assert!(changed > 1500, "only {changed} map pixels changed");
    // The "27 × 18" and "3 × 3" boxes: the accent (#4a9eff-ish blue) on
    // the outline of the big room, grey on the closet's.
    let accent = |p: &image::Rgba<u8>| p[2] > 200 && p[0] < 120 && p[1] > 120;
    assert!(count_in(&lit, map, accent) > count_in(&plain, map, accent) + 500);

    let (no_occ, _) = shot(
        "m6_occupancy_off",
        r#"{ "ui": "rules", "layer": "pillars", "level": "level_4", "rule": "pillar_stamp" }"#,
    );
    let (occ, _) = shot(
        "m6_occupancy_on",
        r#"{ "ui": "rules", "layer": "pillars", "level": "level_4", "rule": "pillar_stamp", "show_occupancy": true }"#,
    );
    let hatch = |p: &image::Rgba<u8>| p[0] > 180 && p[1] < 140 && p[2] > 150;
    let (h0, h1) = (count_in(&no_occ, map, hatch), count_in(&occ, map, hatch));
    // 4 pillars x 8 cells hatched.
    assert!(h1 > h0 + 1000, "hatch pixels {h0} -> {h1}");
    assert!(diff_in(&no_occ, &occ, map) > 1500);
}

/// Painting a derived layer from a script: the brush with `"suppress"`,
/// and `"erase"`, save exactly that level's overrides file; undone, the
/// tree is as before.
#[test]
fn m6_suppress_gesture_saves_one_file_and_undo_restores() {
    if !gpu_tests() {
        return;
    }
    let (tmp, dir) = temp_copy(&game());
    m6_shot(tmp.path(), &dir, "m6_authored", None, "", true);
    let before = files_of(&dir);
    let (_, r) = m6_shot(
        tmp.path(),
        &dir,
        "m6_suppress",
        Some("crevasse"),
        r#"!{ "gesture": "brush", "level": "crevasse", "layer": "pillar_marks", "path": [[10, 24], [16, 24]], "brush": "suppress" },
           { "gesture": "brush", "level": "crevasse", "layer": "pillar_marks", "path": [[16, 24]], "brush": "erase" }"#,
        true,
    );
    assert_eq!(r.applied, 2);
    let after = files_of(&dir);
    let differ: Vec<_> = after
        .keys()
        .chain(before.keys())
        .filter(|p| before.get(*p) != after.get(*p))
        .map(|p| p.strip_prefix(&dir).unwrap_or(p).to_path_buf())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    assert_eq!(
        differ,
        [PathBuf::from(
            "worlds/main/levels/crevasse/pillar_marks.json"
        )]
    );
    let file = dir.join("worlds/main/levels/crevasse/pillar_marks.json");
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(saved["overrides"], true);
    let row = saved["rows"][24].as_str().unwrap();
    assert_eq!(&row[10..17], "!!!!!!.", "{row}");
    // Clearing the overrides by hand, then the clear undone and redone:
    // the file comes back byte for byte, then goes away and the tree is
    // the authored one again.
    let with_file = files_of(&dir);
    m6_shot(
        tmp.path(),
        &dir,
        "m6_suppress_undone",
        None,
        r#"!{ "gesture": "rect", "level": "crevasse", "layer": "pillar_marks", "path": [[10, 24], [15, 24]], "brush": "erase" },
           { "undo": 1 }"#,
        true,
    );
    assert_eq!(files_of(&dir), with_file);
    m6_shot(
        tmp.path(),
        &dir,
        "m6_suppress_cleared",
        None,
        r#"!{ "gesture": "rect", "level": "crevasse", "layer": "pillar_marks", "path": [[10, 24], [15, 24]], "brush": "erase" }"#,
        true,
    );
    assert!(!file.exists());
    assert_eq!(files_of(&dir), before);
}

/// `--synthetic 500 --bench` with the pillar layers: every level bakes the two
/// passes, and frames stay timed.
#[test]
fn m6_synthetic_500_bench_smoke() {
    if !gpu_tests() {
        return;
    }
    let (tmp, dir) = temp_copy(&game());
    let script = tmp.path().join("m6_author.json");
    std::fs::write(&script, format!("[{M6_AUTHOR}]")).unwrap();
    let out = tmp.path().join("m6_synthetic500.png");
    let r = screenshot(&ScreenshotOptions {
        size: [1600, 900],
        synthetic: Some(500),
        script: Some(script),
        bench_frames: 20,
        ..ScreenshotOptions::new(&dir, &out)
    })
    .unwrap();
    keep(&out);
    assert_eq!(r.levels, 500);
    assert_eq!(r.ui_ms.len(), 20);
}

// ---------------------------------------------------------------------
// The Paint group: one toolbar button, a mode strip
// ---------------------------------------------------------------------

/// B, L, R and G pick their Paint mode, P the last one; the mode strip is
/// drawn exactly while a Paint mode is the tool (never with V or E), and a
/// click on one of its modes picks it as the key does. Pressing B on Brush
/// changes nothing.
#[test]
fn paint_modes_by_shortcut_and_the_mode_strip() {
    use egui::Key;
    use worlds_editor::tools::Tool;
    if !gpu_tests() {
        return;
    }
    let (tmp, dir) = temp_copy(&game());
    let (hl, ctx, mut app) = app_on(&dir, [1600, 900]);
    steps(&mut app, r#"[{ "ui": "focus", "level": "crevasse" }]"#);
    let mut t = 0.0;
    // `events` in the first frame, then a third of a second of frames: the
    // shortcut is read in one frame and its action run, the strip follows
    // the tool in the next and fades in (an egui area) over the rest.
    let mut frames = |app: &mut worlds_editor::app::EditorApp, events: Vec<egui::Event>| {
        hl.frame_with(&ctx, app, t, events);
        for _ in 0..20 {
            t += 1.0 / 60.0;
            hl.frame(&ctx, app, t);
        }
    };
    let key = |k: Key| {
        [true, false]
            .map(|pressed| egui::Event::Key {
                key: k,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers: Default::default(),
            })
            .to_vec()
    };
    frames(&mut app, vec![]);
    assert_eq!(app.st.view.tool, Tool::Select);
    assert!(
        app.st.viewport.paint_strip.is_none(),
        "no strip with Select"
    );
    let select = hl.read().unwrap();
    for (k, mode) in [
        (Key::B, Tool::Brush),
        (Key::L, Tool::Line),
        (Key::R, Tool::Rect),
        (Key::G, Tool::Fill),
    ] {
        frames(&mut app, key(k));
        assert_eq!(app.st.view.tool, mode, "{k:?}");
        assert_eq!(app.st.view.paint_tool(), mode);
        let strip = app
            .st
            .viewport
            .paint_strip
            .expect("the strip with a Paint mode");
        assert!(app.st.map_rect.unwrap().contains_rect(strip), "{strip:?}");
        // Drawn: those pixels differ from the Select frame's.
        let img = hl.read().unwrap();
        let n = diff_in(&select, &img, strip);
        assert!(
            n > 500,
            "{k:?}: strip not drawn ({n} pixels differ in {strip:?})"
        );
    }
    // Away and back: P restores Fill; V and E have no strip.
    for (k, tool) in [(Key::V, Tool::Select), (Key::E, Tool::Eraser)] {
        frames(&mut app, key(k));
        assert_eq!(app.st.view.tool, tool);
        assert!(app.st.viewport.paint_strip.is_none(), "{k:?}");
    }
    frames(&mut app, key(Key::P));
    assert_eq!(app.st.view.tool, Tool::Fill, "P: the last mode");
    // B on Brush: still Brush, the same undo stack, the strip still there.
    frames(&mut app, key(Key::B));
    let n = app.undo_entries();
    frames(&mut app, key(Key::B));
    assert_eq!(app.st.view.tool, Tool::Brush);
    assert_eq!(app.undo_entries(), n);
    // A click on the strip's Line: the click lands in the right fifth
    // ("Paint", Brush, Line, Rectangle, Fill: Line is the third label).
    let strip = app.st.viewport.paint_strip.unwrap();
    let out = tmp.path().join("ui_paint_brush.png");
    hl.read().unwrap().save(&out).unwrap();
    keep(&out);
    let mut hit = None;
    for i in 0..40 {
        let p = egui::pos2(
            strip.left() + strip.width() * (i as f32 + 0.5) / 40.0,
            strip.center().y,
        );
        let press = |pressed| egui::Event::PointerButton {
            pos: p,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        frames(
            &mut app,
            vec![egui::Event::PointerMoved(p), press(true), press(false)],
        );
        if app.st.view.tool == Tool::Line {
            hit = Some(i);
            break;
        }
        assert_eq!(app.st.view.tool, Tool::Brush, "a click at {p:?}");
    }
    assert!(hit.is_some(), "no click on the strip picked Line");
    assert_eq!(app.st.view.paint_tool(), Tool::Line);
    assert_eq!(app.undo_entries(), n, "a mode click edits nothing");
}
