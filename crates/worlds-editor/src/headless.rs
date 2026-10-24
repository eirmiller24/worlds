//! Render the editor without a window: `worlds --screenshot out.png`.
//!
//! Creates its own wgpu device (any adapter, else the software fallback),
//! runs a few egui frames of the real [`EditorApp`] into an offscreen
//! texture, and writes the last one as PNG. The whole UI is in the picture,
//! viewport included, because the viewport's paint callback runs inside
//! egui-wgpu's render pass exactly as in the window.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context as _, anyhow};
use egui::mutex::RwLock;
use egui_wgpu::wgpu;
use std::sync::Arc;

use crate::app::{AppOptions, EditorApp};
use crate::viewport::gpu::Gpu;

#[derive(Debug, Clone)]
pub struct ScreenshotOptions {
    pub project: PathBuf,
    pub out: PathBuf,
    pub level: Option<String>,
    pub size: [u32; 2],
    pub synthetic: Option<usize>,
    /// `--synthetic-entities N`.
    pub synthetic_entities: Option<usize>,
    /// `--synthetic-notes N`.
    pub synthetic_notes: Option<usize>,
    /// `--synthetic-guides N`.
    pub synthetic_guides: Option<usize>,
    /// Extra frames to render (camera panning) and time, 0 for none.
    pub bench_frames: usize,
    /// `--bench-palette QUERY`: the bench frames keep the camera still,
    /// show the entity palette, and frame `i` types the first `i % len + 1`
    /// characters of `QUERY` into its search.
    pub bench_palette: Option<String>,
    /// Zoom steps to apply after fitting (positive = in).
    pub zoom_steps: i32,
    /// Put the pointer here (screen points) for the hover readout and the
    /// tool preview.
    pub hover: Option<[f32; 2]>,
    /// Select this tool before rendering.
    pub tool: Option<crate::tools::Tool>,
    /// A script to run before rendering (see [`crate::script`]): commands,
    /// each its own undo entry, and replayed tool gestures.
    pub script: Option<PathBuf>,
    /// Save the project after the script (with `serve`: after serving).
    pub save: bool,
    /// Host the live API while rendering (`--serve`).
    pub serve: bool,
    /// `--rule <layer>/<rule>`: after the script, open the Rules pane on
    /// that rule (in `level`), as a script `ui` step would.
    pub rule: Option<String>,
    /// With `serve`: keep rendering frames (draining requests) for this
    /// long, then take the screenshot. `None`: until stdin closes.
    pub wait_for: Option<Duration>,
    /// `--autosave <secs>` (0: off; unset: `WORLDS_AUTOSAVE_SECS`, else
    /// off).
    pub autosave: Option<f64>,
    /// `--recover keep|apply|discard`.
    pub recover: Option<crate::app::recovery::RecoverMode>,
    /// `--recover-conflicts`.
    pub recover_conflicts: bool,
    /// `--recover-report <file>`.
    pub recover_report: Option<PathBuf>,
    /// The user's preferences file, read but never written (`None`, the
    /// default and what tests use: the default shortcuts).
    pub preferences: Option<PathBuf>,
}

impl ScreenshotOptions {
    /// Defaults for `project` rendered to `out` at 1600x900.
    pub fn new(project: impl Into<PathBuf>, out: impl Into<PathBuf>) -> Self {
        ScreenshotOptions {
            project: project.into(),
            out: out.into(),
            level: None,
            size: [1600, 900],
            synthetic: None,
            synthetic_entities: None,
            synthetic_notes: None,
            synthetic_guides: None,
            bench_frames: 0,
            bench_palette: None,
            zoom_steps: 0,
            hover: None,
            tool: None,
            script: None,
            save: false,
            serve: false,
            wait_for: None,
            rule: None,
            autosave: None,
            recover: None,
            recover_conflicts: false,
            recover_report: None,
            preferences: None,
        }
    }
}

/// Timings from [`screenshot`].
#[derive(Debug, Clone, Default)]
pub struct Report {
    pub adapter: String,
    pub levels: usize,
    /// Per bench frame: CPU time of the editor's `frame()` (UI + viewport).
    pub ui_ms: Vec<f64>,
    /// Per bench frame: the whole frame including tessellation, GPU
    /// submission and waiting for the GPU.
    pub total_ms: Vec<f64>,
    /// Single-chunk rebuild time: (chunks, mean µs, max µs).
    pub chunk_rebuild: (usize, f64, f64),
    pub draws: usize,
    pub chunks: usize,
    pub instances: usize,
    /// Script items run.
    pub applied: usize,
    /// Where the Map pane and the Rules pane were in the last frame
    /// (screen points; pixels, at the headless 1 pixel per point), if drawn.
    pub map_rect: Option<egui::Rect>,
    pub rules_rect: Option<egui::Rect>,
    /// The pointer readout the picture's status bar shows (the hover as of
    /// the frame before the last), and the map's hover after the last
    /// frame: equal once the picture has settled (`--hover` with `--zoom`).
    pub hover_shown: Option<crate::viewport::HoverInfo>,
    pub hover_last: Option<crate::viewport::HoverInfo>,
    /// UI-thread render time of each autosave the run wrote, in ms.
    pub autosave_ms: Vec<f64>,
}

pub struct Headless {
    pub gpu: Gpu,
    pub adapter: String,
    target: wgpu::Texture,
    size: [u32; 2],
    /// Pointer position sent with every frame.
    pub pointer: Option<egui::Pos2>,
}

impl Headless {
    pub fn new(size: [u32; 2]) -> anyhow::Result<Self> {
        // One wgpu instance per process, and one adapter and device request
        // at a time: creating and tearing down instances on several threads
        // at once (the GPU tests run in parallel) crashed inside the GL/EGL
        // driver now and then (SIGSEGV after "failed to create dri2 screen").
        static INSTANCE: std::sync::OnceLock<wgpu::Instance> = std::sync::OnceLock::new();
        static INIT: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _init = INIT.lock().unwrap_or_else(|e| e.into_inner());
        let instance = INSTANCE.get_or_init(|| {
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env())
        });
        let force = std::env::var_os("WORLDS_FALLBACK_ADAPTER").is_some();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::None,
            force_fallback_adapter: force,
            compatible_surface: None,
            ..Default::default()
        }))
        .or_else(|_| {
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                force_fallback_adapter: true,
                ..Default::default()
            }))
        })
        .context("no wgpu adapter (not even the software fallback)")?;
        let info = adapter.get_info();
        let adapter_name = format!("{} ({:?})", info.name, info.backend);
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("worlds headless"),
            required_limits: wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits()),
            ..Default::default()
        }))
        .context("request wgpu device")?;
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let renderer = egui_wgpu::Renderer::new(
            &device,
            format,
            egui_wgpu::RendererOptions {
                dithering: false,
                ..Default::default()
            },
        );
        let target = target_texture(&device, format, size);
        Ok(Headless {
            gpu: Gpu {
                device,
                queue,
                renderer: Arc::new(RwLock::new(renderer)),
                format,
            },
            adapter: adapter_name,
            target,
            size,
            pointer: None,
        })
    }

    fn raw_input(&self, t: f64, extra: Vec<egui::Event>) -> egui::RawInput {
        let mut events: Vec<egui::Event> = self
            .pointer
            .map(|p| vec![egui::Event::PointerMoved(p)])
            .unwrap_or_default();
        events.extend(extra);
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(self.size[0] as f32, self.size[1] as f32),
            )),
            time: Some(t),
            focused: true,
            events,
            ..Default::default()
        }
    }

    /// The target size in pixels.
    pub fn size(&self) -> [u32; 2] {
        self.size
    }

    /// Render into a target of this size from now on (a new texture when it
    /// differs).
    pub fn set_size(&mut self, size: [u32; 2]) {
        if size == self.size {
            return;
        }
        self.target = target_texture(&self.gpu.device, self.gpu.format, size);
        self.size = size;
    }

    /// Run one egui frame of `app` and render it into the target. Returns
    /// (time in `app.frame`, total time including the GPU).
    pub fn frame(&self, ctx: &egui::Context, app: &mut EditorApp, t: f64) -> (Duration, Duration) {
        self.run(ctx, t, |ui| app.frame(ui))
    }

    /// [`Self::frame`] with these input events too (clicks, keys; tests).
    pub fn frame_with(
        &self,
        ctx: &egui::Context,
        app: &mut EditorApp,
        t: f64,
        events: Vec<egui::Event>,
    ) -> (Duration, Duration) {
        self.run_with(ctx, t, events, |ui| app.frame(ui))
    }

    /// Run one egui frame of `f` and render it into the target. Returns
    /// (time in `f`, total time including the GPU).
    pub fn run(
        &self,
        ctx: &egui::Context,
        t: f64,
        f: impl FnMut(&mut egui::Ui),
    ) -> (Duration, Duration) {
        self.run_with(ctx, t, Vec::new(), f)
    }

    /// [`Self::run`] with these input events after the pointer's move.
    pub fn run_with(
        &self,
        ctx: &egui::Context,
        t: f64,
        events: Vec<egui::Event>,
        mut f: impl FnMut(&mut egui::Ui),
    ) -> (Duration, Duration) {
        let t0 = Instant::now();
        let mut ui_time = Duration::ZERO;
        let mut out = ctx.run_ui(self.raw_input(t, events), |ui| {
            let t = Instant::now();
            f(ui);
            ui_time = t.elapsed();
        });
        let prims = ctx.tessellate(std::mem::take(&mut out.shapes), out.pixels_per_point);
        let sd = egui_wgpu::ScreenDescriptor {
            size_in_pixels: self.size,
            pixels_per_point: out.pixels_per_point,
        };
        let g = &self.gpu;
        let mut r = g.renderer.write();
        for (id, deltas) in &out.textures_delta.set {
            for delta in deltas {
                r.update_texture(&g.device, &g.queue, *id, delta);
            }
        }
        let mut enc = g
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("worlds headless frame"),
            });
        let mut cmds = r.update_buffers(&g.device, &g.queue, &mut enc, &prims, &sd);
        let view = self
            .target
            .create_view(&wgpu::TextureViewDescriptor::default());
        {
            let pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("worlds headless"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            let mut pass = pass.forget_lifetime();
            r.render(&mut pass, &prims, &sd);
        }
        for id in &out.textures_delta.free {
            r.free_texture(id);
        }
        out.textures_delta.clear();
        drop(r);
        cmds.push(enc.finish());
        g.queue.submit(cmds);
        let _ = g.device.poll(wgpu::PollType::wait_indefinitely());
        (ui_time, t0.elapsed())
    }

    /// Read the target back as an RGBA image.
    pub fn read(&self) -> anyhow::Result<image::RgbaImage> {
        let g = &self.gpu;
        let [w, h] = self.size;
        let row = (4 * w).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let buf = g.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("worlds readback"),
            size: (row * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = g.device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        g.queue.submit([enc.finish()]);
        let (tx, rx) = std::sync::mpsc::channel();
        buf.slice(..).map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        let _ = g.device.poll(wgpu::PollType::wait_indefinitely());
        rx.recv()?
            .map_err(|e| anyhow!("map readback buffer: {e}"))?;
        let data = buf
            .slice(..)
            .get_mapped_range()
            .map_err(|e| anyhow!("read mapped buffer: {e:?}"))?;
        let mut img = image::RgbaImage::new(w, h);
        for y in 0..h {
            let src = &data[(y * row) as usize..(y * row + 4 * w) as usize];
            img.as_mut()[(y * 4 * w) as usize..((y + 1) * 4 * w) as usize].copy_from_slice(src);
        }
        // egui leaves alpha < 255 where it blended over the clear; the
        // picture is opaque.
        for p in img.pixels_mut() {
            p[3] = 255;
        }
        Ok(img)
    }
}

fn target_texture(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    size: [u32; 2],
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("worlds screenshot"),
        size: wgpu::Extent3d {
            width: size[0],
            height: size[1],
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}

/// An error the process exits with a code of its own for (`worlds` exits 2
/// for these: the project is open in another Worlds, or `--recover apply`
/// found an autosave it can't recover).
#[derive(Debug)]
pub struct Exit2(pub String);

impl std::fmt::Display for Exit2 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Exit2 {}

/// Open `opts.project`, render, write `opts.out`.
///
/// A run that edits the project (`--serve`, `--save`, `--recover`,
/// `--autosave`) takes its session lock: held by another Worlds, this
/// fails with [`Exit2`].
pub fn screenshot(opts: &ScreenshotOptions) -> anyhow::Result<Report> {
    let log = crate::log::init();
    let mut hl = Headless::new(opts.size)?;
    hl.pointer = opts.hover.map(|[x, y]| egui::pos2(x, y));
    tracing::info!("headless adapter: {}", hl.adapter);
    let ctx = egui::Context::default();
    let mut app = EditorApp::new(
        &ctx,
        hl.gpu.clone(),
        log,
        AppOptions {
            headless: true,
            synthetic: opts.synthetic,
            synthetic_entities: opts.synthetic_entities,
            synthetic_notes: opts.synthetic_notes,
            synthetic_guides: opts.synthetic_guides,
            level: opts.level.clone(),
            serve: opts.serve,
            edits: opts.serve
                || opts.save
                || opts.recover.is_some()
                || opts.autosave.is_some_and(|s| s > 0.0)
                || (opts.script.is_some() && crate::autosave::interval(None, true).is_some()),
            autosave: crate::autosave::interval(opts.autosave, true),
            recover: opts.recover,
            recover_conflicts: opts.recover_conflicts,
            recover_report: opts.recover_report.clone(),
            preferences: opts.preferences.clone(),
            preferences_read_only: true,
        },
    );
    app.set_commands(Box::new(crate::edit::Editing::new()));
    app.open(&opts.project);
    if let Some(msg) = &app.locked_out {
        return Err(Exit2(format!("{}: {msg}", opts.project.display())).into());
    }
    if let Some(msg) = &app.recovery_failed {
        return Err(Exit2(format!("--recover apply: {msg}")).into());
    }
    let mut applied = 0;
    let mut t = 0.0;
    if let Some(path) = &opts.script {
        let steps = crate::script::read(path)?;
        // A script runs in the view it is drawn in, as a user works: frames
        // first (the dock lays out, the opening fit applies), then one after
        // each step (a `focus` or a pane it shows takes effect), so what
        // depends on the zoom ("within 8 screen px" for guide snapping, hit
        // tolerances) uses the zoom of the picture, not a default of 1.
        for _ in 0..3 {
            hl.frame(&ctx, &mut app, t);
            t += 1.0 / 60.0;
        }
        for (i, step) in steps.into_iter().enumerate() {
            app.run_step(step)
                .map_err(|e| anyhow!("{}: item {i}: {e}", path.display()))?;
            applied += 1;
            hl.frame(&ctx, &mut app, t);
            t += 1.0 / 60.0;
        }
    }
    if let Some(spec) = &opts.rule {
        let step = crate::script::UiStep::rule(spec, opts.level.clone())?;
        app.ui_step(&step)
            .map_err(|e| anyhow!("--rule {spec}: {e}"))?;
    }
    if opts.bench_palette.is_some() {
        app.ui_step(&crate::script::UiStep::Palette(Default::default()))
            .map_err(|e| anyhow!("--bench-palette: {e}"))?;
    }
    if opts.serve {
        let signalled = serve(&hl, &ctx, &mut app, opts.wait_for, &mut t)?;
        if signalled {
            // As the window does: unsaved edits are autosaved before the
            // run goes on (to --save, if given, which removes it again).
            app.autosave_sync();
        }
    }
    if opts.save {
        app.save()?;
    }
    if let Some(t) = opts.tool {
        app.run(crate::app::actions::Action::SetTool(t));
    }
    let doc = app
        .document()
        .ok_or_else(|| anyhow!("could not open {}", opts.project.display()))?;
    let mut report = Report {
        adapter: hl.adapter.clone(),
        levels: doc.levels.len(),
        applied,
        ..Default::default()
    };
    report.chunk_rebuild = chunk_rebuild_stats(doc);

    // A few frames for egui layout and the dock to settle, then the fit.
    for _ in 0..3 {
        hl.frame(&ctx, &mut app, t);
        t += 1.0 / 60.0;
    }
    if opts.zoom_steps != 0 {
        app.st.viewport.zoom_steps_centered(opts.zoom_steps);
        // The status bar is drawn before the map in a frame, so it shows
        // the hover the map worked out the frame before: one frame at the
        // new zoom first, so the picture's readout is the zoomed one.
        hl.frame(&ctx, &mut app, t);
        t += 1.0 / 60.0;
    }
    report.hover_shown = app.st.viewport.hover.clone();
    hl.frame(&ctx, &mut app, t);
    t += 1.0 / 60.0;
    report.hover_last = app.st.viewport.hover.clone();
    let img = hl.read()?;
    let s = app.st.viewport.stats;
    (report.draws, report.chunks, report.instances) = (s.draws, s.chunks, s.instances);
    report.map_rect = app.st.map_rect;
    report.rules_rect = app.st.rules.pane_rect;
    write_png(&opts.out, &img)?;

    // Bench the palette: the camera stays, the search grows a character
    // per frame (and starts over).
    if let (true, Some(q)) = (opts.bench_frames > 0, &opts.bench_palette) {
        let chars: Vec<char> = q.chars().collect();
        for i in 0..opts.bench_frames {
            let n = if chars.is_empty() {
                0
            } else {
                i % chars.len() + 1
            };
            app.st.entities.query = chars[..n].iter().collect();
            let (ui, total) = hl.frame(&ctx, &mut app, t);
            t += 1.0 / 60.0;
            report.ui_ms.push(ui.as_secs_f64() * 1e3);
            report.total_ms.push(total.as_secs_f64() * 1e3);
        }
    } else if opts.bench_frames > 0 {
        // Bench: pan in a circle so chunks enter and leave the view.
        let start_cam = app.st.viewport.camera;
        let size = app.st.viewport.size_px;
        let radius = size[0].min(size[1]) * 0.25;
        for i in 0..opts.bench_frames {
            let a = i as f64 / opts.bench_frames as f64 * std::f64::consts::TAU;
            app.st.viewport.camera = start_cam;
            app.st
                .viewport
                .camera
                .pan_screen([radius * a.cos(), radius * a.sin()]);
            let (ui, total) = hl.frame(&ctx, &mut app, t);
            t += 1.0 / 60.0;
            report.ui_ms.push(ui.as_secs_f64() * 1e3);
            report.total_ms.push(total.as_secs_f64() * 1e3);
        }
    }
    report.autosave_ms = app.autosaver.render_ms.clone();
    app.shutdown();
    Ok(report)
}

/// `--serve`: print `{ port, pid, project }` as one JSON line, then render
/// frames (each drains the live API's queues) until `wait` has passed, or
/// without `wait` until stdin closes. Returns whether SIGINT/SIGTERM ended
/// it.
fn serve(
    hl: &Headless,
    ctx: &egui::Context,
    app: &mut EditorApp,
    wait: Option<Duration>,
    t: &mut f64,
) -> anyhow::Result<bool> {
    let Some(server) = app.server() else {
        anyhow::bail!(
            "--serve: the live API didn't start: {}",
            app.server_error().unwrap_or("no project open")
        );
    };
    let info = server.info();
    // SIGINT/SIGTERM end the serving like the end of the wait: the run
    // goes on to save (with --save), screenshot and stop the server. The
    // handlers go in before the port is printed, so a client may signal
    // as soon as it reads it.
    let signalled = crate::signals::watch(|| {}, crate::signals::GRACE);
    println!(
        "{}",
        serde_json::json!({ "port": info.port, "pid": info.pid, "project": info.project })
    );
    use std::io::Write as _;
    let _ = std::io::stdout().flush();
    let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    if wait.is_none() {
        let closed = closed.clone();
        std::thread::spawn(move || {
            let _ = std::io::copy(&mut std::io::stdin().lock(), &mut std::io::sink());
            closed.store(true, std::sync::atomic::Ordering::SeqCst);
        });
    }
    let start = Instant::now();
    let tick = Duration::from_millis(16);
    let signal = || signalled.load(std::sync::atomic::Ordering::SeqCst);
    loop {
        let done = signal()
            || match wait {
                Some(w) => start.elapsed() >= w,
                None => closed.load(std::sync::atomic::Ordering::SeqCst),
            };
        if done {
            break;
        }
        let f0 = Instant::now();
        hl.frame(ctx, app, *t);
        *t += 1.0 / 60.0;
        if let Some(rest) = tick.checked_sub(f0.elapsed()) {
            std::thread::sleep(rest);
        }
    }
    Ok(signal())
}

/// What `worlds --snapshot` renders.
#[derive(Debug, Clone)]
pub struct MapSnapshotOptions {
    pub project: PathBuf,
    pub out: PathBuf,
    pub target: worlds_server::ViewTarget,
    pub max_size: [u32; 2],
    pub layers: Option<Vec<worlds_core::Slug>>,
}

/// `worlds --screenshot out.png --snapshot`: the map alone, through
/// [`crate::snapshot::SnapshotRenderer::render`] (the function
/// `view.snapshot` uses), written as PNG. Returns the snapshot.
pub fn map_snapshot(opts: &MapSnapshotOptions) -> anyhow::Result<worlds_server::Snapshot> {
    crate::log::init();
    let mut doc = crate::doc::Document::open(&opts.project)
        .map_err(|e| anyhow!("cannot open {}: {e}", opts.project.display()))?;
    doc.bake_all_blocking();
    let mut view = crate::view::ViewState::new();
    view.sync_layers(&doc, None);
    let mut r = crate::snapshot::SnapshotRenderer::new()?;
    let shot = r
        .render(
            &doc,
            &view,
            &worlds_server::SnapshotRequest {
                target: opts.target.clone(),
                size: opts.max_size,
                layers: opts.layers.clone(),
                overlays: true,
            },
        )
        .map_err(|e| anyhow!("{}", e.message))?;
    if let Some(dir) = opts.out.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&opts.out, &shot.png)
        .with_context(|| format!("write {}", opts.out.display()))?;
    Ok(shot)
}

/// Rebuild every chunk once and time each (after a warm-up pass).
fn chunk_rebuild_stats(doc: &crate::doc::Document) -> (usize, f64, f64) {
    use crate::viewport::scene::{ChunkRef, Scene};
    let mut scene = Scene::new(doc);
    let mut refs = Vec::new();
    for (l, ls) in scene.levels.iter().enumerate() {
        for (j, lc) in ls.layers.iter().enumerate() {
            for k in 0..lc.chunks.len() {
                refs.push(ChunkRef {
                    level: l,
                    layer: j,
                    chunk: k,
                });
            }
        }
    }
    refs.truncate(4000);
    for &r in &refs {
        scene.rebuild_chunk(doc, r);
    }
    let mut total = 0.0;
    let mut max: f64 = 0.0;
    for &r in &refs {
        scene.invalidate_layer(
            r.level,
            scene.levels[r.level].layers[r.layer].slug.clone().as_str(),
        );
        let t = Instant::now();
        scene.rebuild_chunk(doc, r);
        let us = t.elapsed().as_secs_f64() * 1e6;
        total += us;
        max = max.max(us);
    }
    (refs.len(), total / refs.len().max(1) as f64, max)
}

fn write_png(path: &Path, img: &image::RgbaImage) -> anyhow::Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    img.save(path)
        .with_context(|| format!("write {}", path.display()))?;
    tracing::info!("wrote {}", path.display());
    Ok(())
}

/// Fraction of pixels that differ from the most common color: a cheap
/// "is anything drawn" check for smoke tests.
pub fn non_blank_fraction(img: &image::RgbaImage) -> f64 {
    let mut counts = std::collections::HashMap::new();
    for p in img.pixels() {
        *counts.entry(p.0).or_insert(0usize) += 1;
    }
    let max = counts.values().copied().max().unwrap_or(0);
    1.0 - max as f64 / (img.width() * img.height()).max(1) as f64
}
