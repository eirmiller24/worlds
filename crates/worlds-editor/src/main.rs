//! `worlds`: the Worlds level editor.

use std::path::PathBuf;

use std::time::Duration;

use anyhow::bail;
use clap::Parser;
use worlds_core::Slug;
use worlds_editor::app::{AppOptions, EditorApp};
use worlds_editor::edit::Editing;
use worlds_editor::headless::{self, ScreenshotOptions};
use worlds_editor::tools::Tool;
use worlds_editor::viewport::gpu::Gpu;
use worlds_server::ViewTarget;

#[derive(Parser, Debug)]
#[command(name = "worlds", version, about = "The Worlds level editor")]
struct Cli {
    /// Project directory (`*.worlds`). Without one, use File > Open (Ctrl+O).
    project: Option<PathBuf>,
    /// Render one frame without a window to this PNG and exit.
    #[arg(long, value_name = "PNG")]
    screenshot: Option<PathBuf>,
    /// Level to make active and frame (default: fit the whole world).
    #[arg(long, value_name = "SLUG")]
    level: Option<String>,
    /// Screenshot size in pixels.
    #[arg(long, value_name = "WxH", default_value = "1600x900", value_parser = parse_size)]
    size: [u32; 2],
    /// Zoom this many steps in (or out, negative) after fitting (screenshots).
    #[arg(long, default_value_t = 0, allow_hyphen_values = true)]
    zoom: i32,
    /// Dev: grow the project to N levels by copying its levels into a grid.
    #[arg(long, value_name = "N")]
    synthetic: Option<usize>,
    /// Dev: add N generated entity types (a category tree, tags, icons,
    /// every field type) and ceil(N/4) instances of them per level. Combines
    /// with --synthetic; the project can't be saved.
    #[arg(long, value_name = "N")]
    synthetic_entities: Option<usize>,
    /// Dev: add N world pins, ceil(N/4) notes per level and N/10 world
    /// paths (deterministic); the project can't be saved.
    #[arg(long, value_name = "N")]
    synthetic_notes: Option<usize>,
    /// Dev: add N four-point guide polylines over the world, half of them in
    /// a guide group with an offset (deterministic); the project can't be
    /// saved.
    #[arg(long, value_name = "N")]
    synthetic_guides: Option<usize>,
    /// Screenshots: put the pointer at X,Y (points) to show the hover readout
    /// and the tool preview.
    #[arg(long, value_name = "X,Y", value_parser = parse_point)]
    hover: Option<[f32; 2]>,
    /// Screenshots: select this tool (select, brush, rect, line, fill, eraser, entity, hand).
    #[arg(long, value_parser = parse_tool)]
    tool: Option<Tool>,
    /// Dev, with --screenshot: render and time N more frames while panning.
    #[arg(long, value_name = "N", default_value_t = 0)]
    bench: usize,
    /// Dev, with --bench: keep the camera still and the entity palette
    /// shown; frame i types the first (i % len + 1) characters of QUERY
    /// into its search.
    #[arg(long, value_name = "QUERY", requires = "bench")]
    bench_palette: Option<String>,
    /// Screenshots: before rendering, run this JSON file: a list of commands
    /// in the `docs/commands.md` wire format and tool gestures (see README).
    #[arg(long, value_name = "JSON")]
    script: Option<PathBuf>,
    /// Screenshots: save the project after applying `--script` (with
    /// `--serve`, after serving).
    #[arg(long)]
    save: bool,
    /// Screenshots: host the live API (prints `{"port","pid","project"}` as
    /// one JSON line) and keep rendering frames, draining agent requests,
    /// until `--wait-for` seconds pass or, without it, stdin closes.
    #[arg(long)]
    serve: bool,
    /// Open the Rules pane on this rule (`<layer>/<rule>`, in `--level`):
    /// with `--screenshot`, after `--script`.
    #[arg(long, value_name = "LAYER/RULE")]
    rule: Option<String>,
    /// With `--serve`: how long to serve before the screenshot, in seconds.
    #[arg(long, value_name = "SECONDS", requires = "serve")]
    wait_for: Option<f64>,
    /// Screenshots: render the map alone, exactly as `view.snapshot` does
    /// (needs `--level` or `--world-rect`).
    #[arg(long, conflicts_with_all = ["serve", "script", "bench", "hover", "tool", "synthetic", "synthetic_entities", "synthetic_notes", "synthetic_guides", "rule"])]
    snapshot: bool,
    /// With `--snapshot`: frame this world-pixel rect.
    #[arg(
        long,
        value_name = "X,Y,W,H",
        requires = "snapshot",
        conflicts_with = "level"
    )]
    world_rect: Option<String>,
    /// With `--snapshot`: the largest image allowed.
    #[arg(long, value_name = "WxH", default_value = "1024x1024", value_parser = parse_size)]
    max_size: [u32; 2],
    /// With `--snapshot`: draw only these layers (slugs, comma separated).
    #[arg(long, value_name = "A,B", value_delimiter = ',', requires = "snapshot")]
    layers: Option<Vec<String>>,
    /// Autosave unsaved edits to `.worlds/autosave/` every SECS seconds
    /// (0: off). Default: `WORLDS_AUTOSAVE_SECS`, else 30 in the window and
    /// off with `--screenshot`.
    #[arg(long, value_name = "SECS", conflicts_with = "snapshot")]
    autosave: Option<f64>,
    /// What to do with an autosave a previous session left (it didn't
    /// close cleanly): keep it for later (the default with --screenshot),
    /// apply it (recover; conflicting files only with --recover-conflicts)
    /// or discard it (moved to .worlds/autosave.discarded/), or ask (the
    /// recovery prompt; the window's default, and with --screenshot it is in
    /// the picture).
    #[arg(long, value_name = "MODE", conflicts_with = "snapshot")]
    recover: Option<worlds_editor::app::recovery::RecoverMode>,
    /// With `--recover apply`: also recover files changed on disk since the
    /// autosave was taken (they overwrite that change on the next save).
    #[arg(long, requires = "recover")]
    recover_conflicts: bool,
    /// Write what was found in the autosave (and what was applied) to this
    /// JSON file.
    #[arg(long, value_name = "JSON", conflicts_with = "snapshot")]
    recover_report: Option<PathBuf>,
}

fn parse_rect(s: &str) -> Result<[i64; 4], String> {
    let v: Vec<i64> = s
        .split(',')
        .map(|p| p.trim().parse::<i64>().map_err(|e| format!("{p:?}: {e}")))
        .collect::<Result<_, _>>()?;
    let [x, y, w, h] = v[..] else {
        return Err("expected X,Y,W,H (world pixels)".into());
    };
    if w <= 0 || h <= 0 {
        return Err(format!("width and height must be positive, got {w}x{h}"));
    }
    Ok([x, y, w, h])
}

fn parse_size(s: &str) -> Result<[u32; 2], String> {
    let (w, h) = s
        .split_once(['x', 'X'])
        .ok_or("expected WxH, e.g. 1600x900")?;
    let w: u32 = w.trim().parse().map_err(|e| format!("width: {e}"))?;
    let h: u32 = h.trim().parse().map_err(|e| format!("height: {e}"))?;
    if w == 0 || h == 0 || w > 8192 || h > 8192 {
        return Err("size must be 1..=8192 in each axis".into());
    }
    Ok([w, h])
}

fn parse_point(s: &str) -> Result<[f32; 2], String> {
    let (x, y) = s.split_once(',').ok_or("expected X,Y")?;
    Ok([
        x.trim().parse().map_err(|e| format!("x: {e}"))?,
        y.trim().parse().map_err(|e| format!("y: {e}"))?,
    ])
}

fn parse_tool(s: &str) -> Result<Tool, String> {
    worlds_editor::script::parse_tool(s).ok_or_else(|| format!("unknown tool {s:?}"))
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    if let Some(out) = cli.screenshot {
        let Some(project) = cli.project else {
            bail!("--screenshot needs a project directory");
        };
        if cli.snapshot {
            let target = match (cli.level, cli.world_rect) {
                (Some(l), None) => ViewTarget::Level(
                    Slug::new(&l).map_err(|e| anyhow::anyhow!("--level {l:?}: {e}"))?,
                ),
                (None, Some(rect)) => ViewTarget::WorldRect {
                    world: None,
                    rect: parse_rect(&rect).map_err(|e| anyhow::anyhow!("--world-rect: {e}"))?,
                },
                _ => bail!("--snapshot needs --level <slug> or --world-rect x,y,w,h"),
            };
            let layers = cli
                .layers
                .map(|ls| {
                    ls.iter()
                        .map(|l| {
                            Slug::new(l.trim()).map_err(|e| anyhow::anyhow!("--layers {l:?}: {e}"))
                        })
                        .collect::<anyhow::Result<Vec<_>>>()
                })
                .transpose()?;
            let shot = headless::map_snapshot(&headless::MapSnapshotOptions {
                project,
                out,
                target,
                max_size: cli.max_size,
                layers,
            })?;
            println!(
                "snapshot: {} x {} at scale {}",
                shot.width, shot.height, shot.scale
            );
            return Ok(());
        }
        let r = headless::screenshot(&ScreenshotOptions {
            project,
            out,
            level: cli.level,
            size: cli.size,
            synthetic: cli.synthetic,
            synthetic_entities: cli.synthetic_entities,
            synthetic_notes: cli.synthetic_notes,
            synthetic_guides: cli.synthetic_guides,
            bench_frames: cli.bench,
            bench_palette: cli.bench_palette,
            zoom_steps: cli.zoom,
            hover: cli.hover,
            tool: cli.tool,
            script: cli.script,
            save: cli.save,
            serve: cli.serve,
            wait_for: cli.wait_for.map(Duration::from_secs_f64),
            rule: cli.rule,
            autosave: cli.autosave,
            recover: cli.recover,
            recover_conflicts: cli.recover_conflicts,
            recover_report: cli.recover_report,
        });
        let r = match r {
            Ok(r) => r,
            // Open in another Worlds, or --recover apply couldn't: exit 2.
            Err(e) if e.downcast_ref::<headless::Exit2>().is_some() => {
                eprintln!("Error: {e}");
                std::process::exit(2);
            }
            Err(e) => return Err(e),
        };
        if r.applied > 0 {
            println!("script: {} items applied", r.applied);
        }
        println!("adapter: {}", r.adapter);
        println!(
            "levels: {}  visible chunks: {}  draws: {}  instances: {}",
            r.levels, r.chunks, r.draws, r.instances
        );
        println!(
            "chunk rebuild (with re-bucketing): {} chunks, mean {:.1} us, max {:.1} us",
            r.chunk_rebuild.0, r.chunk_rebuild.1, r.chunk_rebuild.2
        );
        if !r.total_ms.is_empty() {
            let stats = |v: &[f64]| {
                let mut s = v.to_vec();
                s.sort_by(f64::total_cmp);
                let mean = s.iter().sum::<f64>() / s.len() as f64;
                (mean, s[s.len() / 2], s[s.len() * 95 / 100], s[s.len() - 1])
            };
            let (m, p50, p95, max) = stats(&r.ui_ms);
            println!(
                "editor frame() CPU (ui_ms): mean {m:.2} ms  p50 {p50:.2}  p95 {p95:.2}  max {max:.2}"
            );
            let (m, p50, p95, max) = stats(&r.total_ms);
            println!(
                "full frame incl. GPU wait (total_ms): mean {m:.2} ms  p50 {p50:.2}  p95 {p95:.2}  max {max:.2}"
            );
        }
        return Ok(());
    }

    let log = worlds_editor::log::init();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Worlds")
            .with_app_id("worlds")
            .with_icon(window_icon())
            .with_inner_size([1600.0, 960.0]),
        ..Default::default()
    };
    let project = cli.project;
    let rule = match &cli.rule {
        Some(spec) => Some(worlds_editor::script::UiStep::rule(
            spec,
            cli.level.clone(),
        )?),
        None => None,
    };
    let opts = AppOptions {
        headless: false,
        synthetic: cli.synthetic,
        synthetic_entities: cli.synthetic_entities,
        synthetic_notes: cli.synthetic_notes,
        synthetic_guides: cli.synthetic_guides,
        level: cli.level,
        serve: true,
        edits: true,
        autosave: worlds_editor::autosave::interval(cli.autosave, false),
        recover: cli.recover,
        recover_conflicts: cli.recover_conflicts,
        recover_report: cli.recover_report,
    };
    let result = eframe::run_native(
        "Worlds",
        options,
        Box::new(move |cc| {
            let rs = cc
                .wgpu_render_state
                .as_ref()
                .ok_or("Worlds needs the wgpu renderer")?;
            let mut app = EditorApp::new(&cc.egui_ctx, Gpu::from_render_state(rs), log, opts);
            app.set_commands(Box::new(Editing::new()));
            let ctx = cc.egui_ctx.clone();
            app.set_terminate_flag(worlds_editor::signals::watch(
                move || ctx.request_repaint(),
                worlds_editor::signals::GRACE,
            ));
            if let Some(p) = project {
                app.open(&p);
                if let Some(step) = &rule
                    && let Err(e) = app.ui_step(step)
                {
                    app.st.toast(format!("--rule: {e}"), true);
                }
            }
            Ok(Box::new(app))
        }),
    );
    // The app has shut down (`on_exit`: server stopped, server.json gone,
    // ui.json written). Leave now rather than through toolkit teardown,
    // which can crash after our state is safe (see `signals::exit_now`).
    let code = match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("Error: {e}");
            1
        }
    };
    worlds_editor::signals::exit_now(code)
}

/// The window and taskbar icon (`assets/icon/`, drawn by its `make_icon.py`).
fn window_icon() -> egui::IconData {
    let png = include_bytes!("../../../assets/icon/icon-256.png");
    let img = image::load_from_memory(png)
        .expect("the bundled icon is a valid PNG")
        .into_rgba8();
    egui::IconData {
        width: img.width(),
        height: img.height(),
        rgba: img.into_raw(),
    }
}
