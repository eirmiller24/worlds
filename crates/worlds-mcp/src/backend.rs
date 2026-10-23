//! The two ways to reach a project, behind one [`Backend`] trait so every
//! tool is written once:
//!
//! - [`EditorBackend`]: the editor's live API over its localhost WebSocket
//!   (found through `.worlds/server.json`).
//! - [`HeadlessBackend`]: the same API handler in-process on the project
//!   loaded from disk (`worlds_server::headless::Session`).
//!
//! [`Connector`] picks one before every tool call (`docs/mcp.md`, "Editor or
//! headless").

use std::collections::BTreeMap;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde_json::{Value, json};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};
use worlds_server::headless::Session;
use worlds_server::rpc::code;
use worlds_server::{RpcError, ServerInfo};

/// Which backend a call ran on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Editor,
    Headless,
}

impl Mode {
    /// `"editor"` or `"headless"`, as tool results report it.
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Editor => "editor",
            Mode::Headless => "headless",
        }
    }
}

/// A connection to the project's live API.
pub trait Backend {
    /// Run one API method (`docs/api.md`).
    fn call(&mut self, method: &str, params: Value) -> Result<Value, RpcError>;
    /// Notifications received since the last call (`changed`, `batch`, ...),
    /// each a JSON-RPC notification object.
    fn take_events(&mut self) -> Vec<Value>;
    /// This connection's `session` id (from `session.hello`).
    fn session(&self) -> &str;
    fn mode(&self) -> Mode;
    /// The project directory (canonical).
    fn dir(&self) -> &Path;
    /// Headless: whether a project file changed on disk since the last load
    /// or save (the editor watches its files itself).
    fn changed_on_disk(&self) -> bool {
        false
    }
}

/// An error that isn't the API's: the connection broke.
pub(crate) fn transport_error(message: impl Into<String>) -> RpcError {
    RpcError::new(code::BUSY, message, None)
}

// --- headless ------------------------------------------------------------

/// The API handler in-process over the project on disk.
pub struct HeadlessBackend {
    session: Session,
    session_id: String,
    dir: PathBuf,
    /// Project files as of the last load or save.
    stamp: FileStamp,
    /// An editor session's autosave that nobody recovered (the editor
    /// crashed or was killed with unsaved edits), as of opening: opening
    /// gives a notice. `describe_project` looks again each time.
    pub autosave: Option<worlds_core::autosave::Summary>,
}

impl HeadlessBackend {
    /// Load the project in `dir` as client `name`, subscribed to `changed`.
    pub fn open(dir: &Path, name: &str) -> Result<Self, String> {
        let stamp = FileStamp::of(dir);
        // A live editor's autosave is its own; only a crashed session's is
        // news (`worlds_core::autosave::pending` tells them apart).
        let autosave = match worlds_core::autosave::pending(dir) {
            worlds_core::autosave::Pending::Crashed(s) => Some(s),
            _ => None,
        };
        let mut session = Session::open(dir)
            .map_err(|e| format!("can't load the project {}: {e}", dir.display()))?
            .client(name);
        let session_id = session.parts().2.session.clone();
        let dir = session.dir().to_path_buf();
        session
            .call("events.subscribe", json!({ "events": ["changed"] }))
            .map_err(|e| e.message)?;
        Ok(HeadlessBackend {
            session,
            session_id,
            dir,
            stamp,
            autosave,
        })
    }
}

impl Backend for HeadlessBackend {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, RpcError> {
        let mut r = self.session.call(method, params);
        // Looked up per describe: an editor may have crashed, recovered or
        // discarded since this backend opened.
        if method == "project.describe"
            && let Ok(v) = &mut r
            && let worlds_core::autosave::Pending::Crashed(a) =
                worlds_core::autosave::pending(&self.dir)
        {
            v["autosave"] = a.to_json();
        }
        if method == "project.save" && r.is_ok() {
            self.stamp = FileStamp::of(&self.dir);
        }
        r
    }

    fn take_events(&mut self) -> Vec<Value> {
        self.session.take_events()
    }

    fn session(&self) -> &str {
        &self.session_id
    }

    fn mode(&self) -> Mode {
        Mode::Headless
    }

    fn dir(&self) -> &Path {
        &self.dir
    }

    fn changed_on_disk(&self) -> bool {
        FileStamp::of(&self.dir) != self.stamp
    }
}

/// Size and modification time of every project file (not `.worlds/`,
/// `export/`, `schema/` or hidden directories), to notice edits made on
/// disk by someone else.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FileStamp(BTreeMap<PathBuf, (u64, Option<SystemTime>)>);

impl FileStamp {
    pub fn of(dir: &Path) -> Self {
        let mut out = BTreeMap::new();
        walk(dir, dir, &mut out);
        FileStamp(out)
    }
}

fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, (u64, Option<SystemTime>)>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.filter_map(|e| e.ok()) {
        let path = e.path();
        let name = e.file_name();
        let name = name.to_string_lossy();
        let Ok(meta) = e.metadata() else { continue };
        if meta.is_dir() {
            let skip =
                name.starts_with('.') || (dir == root && (name == "export" || name == "schema"));
            if !skip {
                walk(root, &path, out);
            }
        } else if !name.starts_with('.') {
            let rel = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
            out.insert(rel, (meta.len(), meta.modified().ok()));
        }
    }
}

// --- editor --------------------------------------------------------------

/// How long one request may take over the socket. Approval waits are 20 s
/// by default; snapshots of big worlds take a few frames.
const SOCKET_TIMEOUT: Duration = Duration::from_secs(150);

/// A synchronous JSON-RPC client of the editor's WebSocket.
pub struct EditorBackend {
    ws: WebSocket<MaybeTlsStream<TcpStream>>,
    next_id: u64,
    events: Vec<Value>,
    session: String,
    dir: PathBuf,
    /// Which server this is (port, token): a new editor is a new connection.
    pub info: ServerInfo,
}

impl EditorBackend {
    /// Connect to the server `info` describes, say hello as `name` and
    /// subscribe to `changed`.
    pub fn connect(info: &ServerInfo, dir: &Path, name: &str) -> Result<Self, String> {
        let url = format!("ws://127.0.0.1:{}/", info.port);
        let stream = TcpStream::connect_timeout(
            &std::net::SocketAddr::from(([127, 0, 0, 1], info.port)),
            Duration::from_secs(2),
        )
        .map_err(|e| format!("can't connect to the editor on port {}: {e}", info.port))?;
        stream
            .set_read_timeout(Some(SOCKET_TIMEOUT))
            .map_err(|e| e.to_string())?;
        let (ws, _) = tungstenite::client::client(url, MaybeTlsStream::Plain(stream))
            .map_err(|e| format!("the editor refused the WebSocket handshake: {e}"))?;
        let mut b = EditorBackend {
            ws,
            next_id: 1,
            events: Vec::new(),
            session: String::new(),
            dir: dir.to_path_buf(),
            info: info.clone(),
        };
        let hello = b
            .call(
                "session.hello",
                json!({ "token": info.token,
                        "client": { "name": name, "version": env!("CARGO_PKG_VERSION") } }),
            )
            .map_err(|e| format!("the editor refused session.hello: {}", e.message))?;
        b.session = hello["session"].as_str().unwrap_or_default().to_string();
        b.call("events.subscribe", json!({ "events": ["changed"] }))
            .map_err(|e| e.message)?;
        Ok(b)
    }

    fn send(&mut self, v: &Value) -> Result<(), RpcError> {
        self.ws
            .send(Message::Text(v.to_string().into()))
            .map_err(|e| transport_error(format!("lost the connection to the editor: {e}")))
    }
}

impl Backend for EditorBackend {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, RpcError> {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))?;
        loop {
            let msg = self
                .ws
                .read()
                .map_err(|e| transport_error(format!("lost the connection to the editor: {e}")))?;
            let text = match msg {
                Message::Text(t) => t,
                Message::Close(_) => {
                    return Err(transport_error("the editor closed the connection"));
                }
                _ => continue,
            };
            let v: Value = serde_json::from_str(&text).map_err(|e| {
                transport_error(format!("the editor sent something that isn't JSON: {e}"))
            })?;
            if v.get("id") == Some(&json!(id)) {
                if let Some(err) = v.get("error") {
                    return Err(RpcError::from_value(err).unwrap_or_else(|| {
                        RpcError::internal(format!("malformed error from the editor: {err}"))
                    }));
                }
                return Ok(v.get("result").cloned().unwrap_or(Value::Null));
            }
            if v.get("method").is_some() {
                self.events.push(v);
            }
        }
    }

    fn take_events(&mut self) -> Vec<Value> {
        std::mem::take(&mut self.events)
    }

    fn session(&self) -> &str {
        &self.session
    }

    fn mode(&self) -> Mode {
        Mode::Editor
    }

    fn dir(&self) -> &Path {
        &self.dir
    }
}

impl Drop for EditorBackend {
    fn drop(&mut self) {
        let _ = self.ws.close(None);
        let _ = self.ws.flush();
    }
}

// --- choosing one ----------------------------------------------------------

/// The live editor server for `dir`, if `.worlds/server.json` is live: the
/// pid runs, the port accepts, and `project` is this directory.
pub fn live_server(dir: &Path) -> Option<ServerInfo> {
    let info = ServerInfo::read(dir).ok()??;
    let same = std::fs::canonicalize(&info.project).is_ok_and(|p| p == dir);
    (same && info.is_live()).then_some(info)
}

/// Picks and keeps the backend: the editor when its server is live, else
/// the project in-process.
pub struct Connector {
    dir: PathBuf,
    force_headless: bool,
    name: String,
    backend: Option<Box<dyn Backend>>,
    /// The editor this backend is connected to, if any.
    editor: Option<ServerInfo>,
}

/// What [`Connector::ensure`] did, for notices.
#[derive(Debug, Default)]
pub struct Switch {
    pub notices: Vec<String>,
}

impl Connector {
    pub fn new(dir: PathBuf, force_headless: bool, name: String) -> Self {
        Connector {
            dir,
            force_headless,
            name,
            backend: None,
            editor: None,
        }
    }

    /// The project directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The client name sent in `session.hello`.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Change the client name; the next call reconnects under it.
    pub fn set_name(&mut self, name: String) {
        if name != self.name {
            self.name = name;
            self.backend = None;
            self.editor = None;
        }
    }

    /// Forget the backend (after a broken connection).
    pub fn drop_backend(&mut self) {
        self.backend = None;
        self.editor = None;
    }

    /// The current backend, if one is open.
    pub fn current(&mut self) -> Option<&mut Box<dyn Backend>> {
        self.backend.as_mut()
    }

    /// Make sure the right backend is open, switching between editor and
    /// headless as `server.json` says. Returns notices about switches.
    pub fn ensure(&mut self) -> Result<Switch, String> {
        let mut sw = Switch::default();
        let live = live_server(&self.dir);
        if self.force_headless {
            if let Some(info) = live {
                return Err(format!(
                    "the Worlds editor (pid {}) has this project open, and worlds-mcp runs with --headless, so writing would clobber its edits; restart worlds-mcp without --headless to work through the editor",
                    info.pid
                ));
            }
        } else if let Some(info) = live {
            let same = self
                .editor
                .as_ref()
                .is_some_and(|e| e.port == info.port && e.token == info.token);
            if same && self.backend.is_some() {
                return Ok(sw);
            }
            let was_headless = self
                .backend
                .as_ref()
                .is_some_and(|b| b.mode() == Mode::Headless);
            match EditorBackend::connect(&info, &self.dir, &self.name) {
                Ok(b) => {
                    tracing::info!(port = info.port, pid = info.pid, "connected to the editor");
                    if was_headless {
                        sw.notices.push("the Worlds editor opened the project: from now on your edits show live on the user's canvas and are saved when they save".into());
                    }
                    self.backend = Some(Box::new(b));
                    self.editor = Some(info);
                    return Ok(sw);
                }
                Err(e) => {
                    // The editor has the project open: editing the files
                    // too would make two writers. Fail this call; the next
                    // one tries again.
                    tracing::warn!("{e}");
                    self.backend = None;
                    self.editor = None;
                    return Err(format!(
                        "the Worlds editor (pid {}) has this project open, but worlds-mcp couldn't connect to it: {e}. Nothing was changed; try again in a moment",
                        info.pid
                    ));
                }
            }
        }
        // Headless.
        if self.editor.take().is_some() {
            self.backend = None;
            sw.notices.push("the Worlds editor closed the project: from now on edits are written to disk after every call".into());
        }
        if let Some(b) = &self.backend
            && b.mode() == Mode::Headless
        {
            if !b.changed_on_disk() {
                return Ok(sw);
            }
            sw.notices.push("project files changed on disk since the last call, so the project was reloaded from disk (and undo history cleared); re-read what you rely on".into());
        }
        let b = HeadlessBackend::open(&self.dir, &self.name)?;
        tracing::info!(dir = %self.dir.display(), "headless: project loaded from disk");
        if let Some(a) = &b.autosave {
            sw.notices.push(a.warning());
        }
        self.backend = Some(Box::new(b));
        Ok(sw)
    }
}
