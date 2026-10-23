//! [`Server::start`] and the [`ServerHandle`] the document owner drives.
//!
//! The handle owns the tokio runtime (one worker thread: the listener,
//! the handshakes, the sockets) and the [`Handler`]. The owner of the
//! document calls [`ServerHandle::drain`] on its own thread, the editor once
//! per frame when no gesture is in progress, so the document has exactly
//! one writer and needs no lock.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant, SystemTime};

use serde_json::Value;
use tokio::sync::{oneshot, watch};
use worlds_core::command::Change;
use worlds_core::{History, Problem};

use crate::approval::PendingBatch;
use crate::discovery::{ServerInfo, new_token};
use crate::handler::{Client, ConnId, Handler, HandlerConfig, Out, Outcome, UserChange};
use crate::host::Host;
use crate::rpc::{RpcError, response};
use crate::transport::{self, HelloInfo, Shared, lock};

/// A batch idle this long is ended as if `end_batch` were called.
pub const BATCH_IDLE: Duration = Duration::from_secs(60);

/// What [`Server::start`] needs.
#[derive(Clone)]
pub struct ServerConfig {
    /// The project directory; `.worlds/server.json` goes in it.
    pub project_dir: PathBuf,
    /// The project's name, for `session.hello` (kept current by `drain`).
    pub project_name: String,
    /// Headless (no approval mode, `view.*` unsupported).
    pub headless: bool,
    /// Called from the server's thread whenever a request arrives or a
    /// connection closes or an approval wait ends: the editor passes
    /// `egui::Context::request_repaint`, so an idle editor still answers
    /// within a frame.
    pub wake: Option<Arc<dyn Fn() + Send + Sync>>,
}

/// Why the server didn't start.
#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    /// Another live process serves this project (one writer per project).
    /// `pid` and `port` are 0 when it hasn't published `server.json` yet.
    #[error(
        "another Worlds process (pid {pid}) is serving this project on port {port}; running without a server"
    )]
    AlreadyRunning { pid: u32, port: u16 },
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Load(worlds_core::Error),
}

/// A connected client, for the editor's agent indicator.
#[derive(Debug, Clone, PartialEq)]
pub struct ClientInfo {
    pub conn: ConnId,
    pub name: String,
    pub version: String,
    pub session: String,
    pub since: SystemTime,
}

/// One request [`ServerHandle::drain`] handled.
#[derive(Debug, Clone, PartialEq)]
pub struct Handled {
    pub client: String,
    pub session: String,
    pub method: String,
    /// What it applied (empty for reads and refused writes). The host saw
    /// these in [`Host::applied`] already.
    pub changes: Vec<Change>,
    /// The entry label of a write that applied (the toast text).
    pub label: Option<String>,
    /// The error code, if it failed.
    pub error: Option<i64>,
    /// It became a pending approval batch.
    pub pending: Option<String>,
}

/// Starts servers.
pub struct Server;

impl Server {
    /// Bind `127.0.0.1` on a free port, write `.worlds/server.json` (0600,
    /// temp file and rename) and start accepting connections. Refuses with
    /// [`ServerError::AlreadyRunning`] when the file belongs to another live
    /// process.
    pub fn start(config: ServerConfig) -> Result<ServerHandle, ServerError> {
        let dir = std::fs::canonicalize(&config.project_dir)?;
        let pid = std::process::id();
        // One server per project: the lock makes checking and publishing
        // server.json one step, so two processes starting at once can't both
        // pass the check below.
        let Some(lock) = crate::discovery::lock_project(&dir)? else {
            let (pid, port) = ServerInfo::read(&dir)
                .ok()
                .flatten()
                .map_or((0, 0), |i| (i.pid, i.port));
            return Err(ServerError::AlreadyRunning { pid, port });
        };
        if let Ok(Some(existing)) = ServerInfo::read(&dir)
            && existing.pid != pid
            && existing.is_live()
        {
            return Err(ServerError::AlreadyRunning {
                pid: existing.pid,
                port: existing.port,
            });
        }
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("worlds-server")
            .enable_all()
            .build()?;
        let std_listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
        std_listener.set_nonblocking(true)?;
        let port = std_listener.local_addr()?.port();
        let listener = {
            let _guard = runtime.enter();
            tokio::net::TcpListener::from_std(std_listener)?
        };
        let token = new_token();
        let info = ServerInfo {
            port,
            token: token.clone(),
            pid,
            project: dir.to_string_lossy().into_owned(),
        };
        info.write(&dir)?;
        let shared = Arc::new(Shared::new(
            token,
            HelloInfo {
                dir: info.project.clone(),
                name: config.project_name,
                headless: config.headless,
                approval: false,
            },
            config.wake,
        ));
        let (stop_tx, stop_rx) = watch::channel(false);
        runtime.spawn(transport::serve(listener, shared.clone(), stop_rx));
        Ok(ServerHandle {
            runtime: Some(runtime),
            shared,
            handler: Handler::new(HandlerConfig {
                headless: config.headless,
                rev: 0,
            }),
            info,
            dir,
            stop: stop_tx,
            _lock: lock,
        })
    }
}

/// A running server. Dropping it stops it and removes `server.json`.
pub struct ServerHandle {
    runtime: Option<tokio::runtime::Runtime>,
    shared: Arc<Shared>,
    handler: Handler,
    info: ServerInfo,
    dir: PathBuf,
    stop: watch::Sender<bool>,
    /// `.worlds/server.lock`, held while the handle lives.
    _lock: Option<std::fs::File>,
}

impl ServerHandle {
    /// The WebSocket port.
    pub fn port(&self) -> u16 {
        self.info.port
    }

    /// What `server.json` holds.
    pub fn info(&self) -> &ServerInfo {
        &self.info
    }

    /// The project directory (canonical).
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The current revision.
    pub fn rev(&self) -> u64 {
        self.handler.rev()
    }

    /// The handler (tests, and anything not wrapped here).
    pub fn handler(&mut self) -> &mut Handler {
        &mut self.handler
    }

    /// Whether requests are waiting (the editor can skip `drain` when not,
    /// though draining an empty queue is cheap).
    pub fn has_requests(&self) -> bool {
        let st = lock(&self.shared.state);
        !st.queues.is_empty() || !st.closed.is_empty()
    }

    /// Handle every ready request, in order, against the host's document,
    /// replying to each. Call it on the document's thread when no gesture
    /// is in progress (the editor: once per frame). Also cleans up after
    /// closed connections (reverting their open batches) and ends batches
    /// idle for 60 s. Returns what was handled, with the changes each
    /// applied.
    pub fn drain(&mut self, host: &mut dyn Host) -> Vec<Handled> {
        let mut handled = Vec::new();
        let closed = std::mem::take(&mut lock(&self.shared.state).closed);
        for conn in closed {
            self.handler.disconnect(host, conn);
        }
        let activity: HashMap<ConnId, Instant> = {
            let st = lock(&self.shared.state);
            st.conns
                .keys()
                .filter_map(|c| st.queues.last_activity(*c).map(|a| (*c, a)))
                .collect()
        };
        self.handler
            .expire_batches(host, Instant::now(), BATCH_IDLE, &|c| {
                activity.get(&c).copied()
            });
        loop {
            let open: BTreeSet<ConnId> = self.handler.open_batches().collect();
            let next = {
                let mut st = lock(&self.shared.state);
                let Some((conn, q)) = st.queues.pop_next(&open) else {
                    break;
                };
                st.conns.get(&conn).map(|c| (c.client.clone(), q))
            };
            let Some((client, q)) = next else {
                continue;
            };
            let outcome = self.handler.call(host, &client, &q.method, q.params);
            let changes = self.handler.take_applied();
            let mut h = Handled {
                client: client.name.clone(),
                session: client.session.clone(),
                method: q.method,
                changes,
                label: None,
                error: None,
                pending: None,
            };
            match outcome {
                Outcome::Reply(r) => {
                    h.error = r.as_ref().err().map(|e| e.code);
                    h.label = r
                        .as_ref()
                        .ok()
                        .filter(|v| v["status"] == "applied")
                        .and_then(|v| v["label"].as_str())
                        .map(str::to_string);
                    self.shared.send(client.conn, &response(&q.id, r));
                    lock(&self.shared.state).queues.set_idle(client.conn);
                }
                Outcome::Pending { batch_id } => {
                    h.pending = Some(batch_id.clone());
                    self.wait_for_decision(&client, q.id, batch_id);
                }
            }
            handled.push(h);
            self.flush();
        }
        self.flush();
        handled
    }

    fn wait_for_decision(&mut self, client: &Client, id: Value, batch_id: String) {
        let (tx, rx) = oneshot::channel();
        lock(&self.shared.waiters).insert(batch_id.clone(), tx);
        if let Some(rt) = &self.runtime {
            rt.spawn(transport::await_decision(
                self.shared.clone(),
                client.conn,
                id,
                batch_id,
                client.approval_wait,
                rx,
            ));
        }
    }

    /// Deliver what the handler queued, and publish `rev` and the approval
    /// flag for the next handshakes.
    fn flush(&mut self) {
        for o in self.handler.take_out() {
            match o {
                Out::Notify { conn, message } => self.shared.send(conn, &message),
                Out::Decided {
                    conn,
                    batch_id,
                    result,
                    notification,
                } => {
                    let waiter = lock(&self.shared.waiters).remove(&batch_id);
                    match waiter {
                        Some(tx) => {
                            let _ = tx.send(result);
                        }
                        None => self.shared.send(conn, &notification),
                    }
                }
            }
        }
        self.shared.rev.store(self.handler.rev(), Ordering::SeqCst);
        lock(&self.shared.hello).approval = self.handler.approval();
    }

    /// Keep the project name `session.hello` reports current.
    pub fn set_project_name(&mut self, name: &str) {
        lock(&self.shared.hello).name = name.to_string();
    }

    /// Connected clients (after their hello), oldest first.
    pub fn clients(&self) -> Vec<ClientInfo> {
        lock(&self.shared.state)
            .conns
            .values()
            .map(|c| ClientInfo {
                conn: c.client.conn,
                name: c.client.name.clone(),
                version: c.client.version.clone(),
                session: c.client.session.clone(),
                since: c.since,
            })
            .collect()
    }

    /// The label of the last agent entry recorded.
    pub fn last_batch(&self) -> Option<&str> {
        self.handler.last_batch()
    }

    /// How many connections have a batch open.
    pub fn open_batches(&self) -> usize {
        self.handler.open_batches().count()
    }

    /// Approval mode (the toolbar toggle). Sends `approval`.
    pub fn set_approval(&mut self, enabled: bool) {
        self.handler.set_approval(enabled);
        self.flush();
    }

    pub fn approval(&self) -> bool {
        self.handler.approval()
    }

    /// Batches waiting for the user, oldest first.
    pub fn pending(&self) -> &[PendingBatch] {
        self.handler.pending()
    }

    /// Accept a pending batch (see [`Handler::apply_pending`]); its
    /// blocked request gets the result, or its client a `batch` event.
    pub fn apply_pending(
        &mut self,
        host: &mut dyn Host,
        id: &str,
    ) -> Result<Vec<Change>, Option<RpcError>> {
        let r = self.handler.apply_pending(host, id);
        self.handler.take_applied();
        self.flush();
        r
    }

    /// Reject a pending batch and the later ones of the same client.
    pub fn reject_pending(&mut self, id: &str) -> Vec<String> {
        let r = self.handler.reject_pending(id);
        self.flush();
        r
    }

    /// Before the user records an entry or steps the history: record open
    /// agent batches' parts so far (see [`Handler::flush_batches`]).
    pub fn flush_batches(&mut self, history: &mut History) {
        self.handler.flush_batches(history);
    }

    /// The user changed the project (see [`Handler::notify_changed`]).
    pub fn notify_changed(&mut self, change: UserChange<'_>) -> u64 {
        let rev = self.handler.notify_changed(change);
        self.flush();
        rev
    }

    /// The editor reloaded the project (see [`Handler::notify_reloaded`]).
    pub fn notify_reloaded(&mut self) -> u64 {
        let rev = self.handler.notify_reloaded();
        self.flush();
        rev
    }

    /// The selection or active level changed.
    pub fn notify_selection(
        &mut self,
        level: Option<&str>,
        entities: &[String],
        notes: &[String],
        levels: &[String],
    ) {
        self.handler
            .notify_selection(level, entities, notes, levels);
        self.flush();
    }

    /// The user saved (`files` project-relative).
    pub fn notify_saved(&mut self, files: &[String]) {
        self.handler.notify_saved(files);
        self.flush();
    }

    /// The problems list was recomputed.
    pub fn notify_problems(&mut self, problems: &[Problem]) {
        self.handler.notify_problems(problems);
        self.flush();
    }

    /// Stop: close every connection, stop the runtime, remove
    /// `server.json` (if it is still ours).
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        let Some(rt) = self.runtime.take() else {
            return;
        };
        let _ = self.stop.send(true);
        // Let the connections send their close frames.
        let deadline = Instant::now() + Duration::from_millis(200);
        while Instant::now() < deadline && !lock(&self.shared.state).conns.is_empty() {
            std::thread::sleep(Duration::from_millis(5));
        }
        rt.shutdown_timeout(Duration::from_millis(200));
        self.info.remove_if_ours(&self.dir);
    }
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}
