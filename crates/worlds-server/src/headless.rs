//! Headless mode: the same handler over a project loaded from disk, with
//! its own history and bake cache, no editor.
//!
//! - [`Session`] answers calls in-process (no runtime, no socket): what
//!   worlds-mcp uses when no editor is running, and what the handler tests
//!   use.
//! - [`run`] serves the project over the same WebSocket and discovery file
//!   as the editor, draining requests on a thread of its own as they come.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::Value;
use worlds_core::command::Change;
use worlds_core::{History, Project};
use worlds_rules::{BakeCache, TilesetImages};

use crate::handler::{Client, Handler, HandlerConfig, Out, Outcome};
use crate::host::{Doc, Host};
use crate::rpc::RpcError;
use crate::server::{Server, ServerConfig, ServerError, ServerHandle};

/// A [`Host`] that owns its document: a project loaded from `dir`, its
/// history, bake cache and tileset images.
pub struct HeadlessHost {
    pub project: Project,
    pub history: History,
    pub bakes: BakeCache,
    pub images: TilesetImages,
    pub dir: PathBuf,
}

impl HeadlessHost {
    /// Load the project in `dir`.
    pub fn open(dir: impl AsRef<Path>) -> Result<Self, worlds_core::Error> {
        let dir = std::fs::canonicalize(dir.as_ref()).map_err(|source| worlds_core::Error::Io {
            path: dir.as_ref().to_path_buf(),
            source,
        })?;
        let project = worlds_core::load(&dir)?;
        let images = TilesetImages::new(&dir, &project.tilesets);
        Ok(HeadlessHost {
            project,
            history: History::new(),
            bakes: BakeCache::new(),
            images,
            dir,
        })
    }
}

impl Host for HeadlessHost {
    fn doc(&mut self) -> Doc<'_> {
        Doc {
            project: &mut self.project,
            history: &mut self.history,
            bakes: &mut self.bakes,
            images: &self.images,
            dir: &self.dir,
        }
    }

    /// Keep the bake cache current, as the editor does: every cached level
    /// is brought up to date incrementally ([`BakeCache::apply_changes`],
    /// after rereading the tileset images on a `tileset` definition
    /// change); a definition rename or a project setting change drops
    /// everything. Levels not cached are baked when a read needs them.
    fn applied(&mut self, changes: &[Change]) {
        use worlds_core::command::DefKind;
        if changes.iter().any(|c| {
            matches!(
                c,
                Change::Def {
                    kind: DefKind::Tileset,
                    ..
                } | Change::DefRenamed {
                    kind: DefKind::Tileset,
                    ..
                }
            )
        }) {
            self.images = TilesetImages::new(&self.dir, &self.project.tilesets);
        }
        crate::handler::follow_changes(&mut self.bakes, &self.project, &self.images, changes);
    }

    fn rollback(&mut self) {
        self.bakes.clear();
    }

    fn reload(&mut self) -> Result<(), RpcError> {
        let project = worlds_core::load(&self.dir).map_err(|e| RpcError::from_format(&e))?;
        self.images = TilesetImages::new(&self.dir, &project.tilesets);
        self.project = project;
        self.bakes.clear();
        Ok(())
    }
}

/// An in-process headless session: one client calling the handler
/// directly, no socket and no runtime.
///
/// ```no_run
/// let mut s = worlds_server::headless::Session::open("mygame.worlds")?.client("claude-code");
/// let rev = s.call("level.list", serde_json::json!({}))?["rev"].clone();
/// s.call("grid.fill_rect", serde_json::json!({ "level": "crevasse", "layer": "collision",
///     "rect": [2, 2, 3, 1], "value": "water", "expect_rev": rev }))?;
/// s.call("project.save", serde_json::json!({}))?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub struct Session {
    handler: Handler,
    host: HeadlessHost,
    client: Client,
    events: Vec<Value>,
}

impl Session {
    /// Load the project in `dir`. The client is named `agent` until
    /// [`Session::client`] names it.
    pub fn open(dir: impl AsRef<Path>) -> Result<Self, worlds_core::Error> {
        Ok(Session {
            handler: Handler::new(HandlerConfig {
                headless: true,
                rev: 0,
            }),
            host: HeadlessHost::open(dir)?,
            client: Client::in_process("agent"),
            events: Vec::new(),
        })
    }

    /// Name the client (its `Origin::Agent` in the history and labels).
    pub fn client(mut self, name: &str) -> Self {
        self.client.name = name.to_string();
        self
    }

    /// Run one method. Notifications for this session (after
    /// `events.subscribe`) collect in [`Session::take_events`].
    pub fn call(&mut self, method: &str, params: Value) -> Result<Value, RpcError> {
        let outcome = self
            .handler
            .call(&mut self.host, &self.client, method, params);
        for o in self.handler.take_out() {
            if let Out::Notify { message, .. } = o {
                self.events.push(message);
            }
        }
        self.handler.take_applied();
        match outcome {
            Outcome::Reply(r) => r,
            Outcome::Pending { batch_id } => Err(RpcError::internal(format!(
                "batch {batch_id} is pending approval, which headless mode doesn't have"
            ))),
        }
    }

    /// Notifications sent to this session since the last call.
    pub fn take_events(&mut self) -> Vec<Value> {
        std::mem::take(&mut self.events)
    }

    /// The document.
    pub fn project(&self) -> &Project {
        &self.host.project
    }

    /// The history.
    pub fn history(&self) -> &History {
        &self.host.history
    }

    /// The project directory (canonical).
    pub fn dir(&self) -> &Path {
        &self.host.dir
    }

    /// The handler and host, for tests that act as the user or as several
    /// clients.
    pub fn parts(&mut self) -> (&mut Handler, &mut HeadlessHost, &Client) {
        (&mut self.handler, &mut self.host, &self.client)
    }
}

/// Options of [`run`].
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Save the dirty files whenever a request changed the project and no
    /// batch is open (worlds-mcp's "results written to disk").
    pub save_on_write: bool,
}

/// A running headless server.
pub struct HeadlessServer {
    port: u16,
    stop: Arc<AtomicBool>,
    wake: mpsc::Sender<()>,
    thread: Option<JoinHandle<()>>,
}

impl HeadlessServer {
    /// The WebSocket port.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Stop serving: close connections and remove `.worlds/server.json`.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.wake.send(());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for HeadlessServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Load the project in `dir` and serve it: the same server and discovery
/// file as the editor, with requests applied as soon as they arrive.
pub fn run(dir: impl AsRef<Path>, opts: Options) -> Result<HeadlessServer, ServerError> {
    let mut host = HeadlessHost::open(dir.as_ref()).map_err(ServerError::Load)?;
    let (wake_tx, wake_rx) = mpsc::channel::<()>();
    let waker = wake_tx.clone();
    let handle = Server::start(ServerConfig {
        project_dir: host.dir.clone(),
        project_name: host.project.name.clone(),
        headless: true,
        wake: Some(Arc::new(move || {
            let _ = waker.send(());
        })),
    })?;
    let port = handle.port();
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    let thread = std::thread::Builder::new()
        .name("worlds-headless".into())
        .spawn(move || serve(handle, &mut host, &wake_rx, &stop2, &opts))
        .map_err(ServerError::Io)?;
    Ok(HeadlessServer {
        port,
        stop,
        wake: wake_tx,
        thread: Some(thread),
    })
}

fn serve(
    mut handle: ServerHandle,
    host: &mut HeadlessHost,
    wake: &mpsc::Receiver<()>,
    stop: &AtomicBool,
    opts: &Options,
) {
    let mut saved_rev = handle.rev();
    while !stop.load(Ordering::SeqCst) {
        // A second wakes idle-batch expiry even without traffic.
        let _ = wake.recv_timeout(Duration::from_secs(1));
        handle.drain(host);
        // Save once the project moved on (writes, undo, a dropped
        // connection's revert) and no batch is open.
        if opts.save_on_write
            && handle.rev() != saved_rev
            && handle.open_batches() == 0
            && host.project.is_dirty()
        {
            match crate::save_project(&mut host.doc()) {
                Ok(files) => {
                    saved_rev = handle.rev();
                    handle.notify_saved(&files);
                }
                Err(e) => tracing::error!("saving after a write failed: {e}"),
            }
        }
    }
    handle.stop();
}
