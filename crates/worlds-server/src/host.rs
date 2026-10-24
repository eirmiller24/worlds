//! The seam between the method handler and whoever owns the document.
//!
//! The handler never owns a project. Each request borrows one through
//! [`Host::doc`]: the editor hands out its `Document`'s project, its
//! `Editing`'s history and bake cache; headless mode
//! ([`crate::headless::HeadlessHost`]) owns its own. After every mutation
//! the handler makes, it calls [`Host::applied`] with the changes, so the
//! editor can invalidate chunks and rebake before the next frame.
//!
//! The `view.*` methods go through [`Host::focus`], [`Host::select`],
//! [`Host::snapshot`], [`Host::viewports`] and [`Host::open_viewport`];
//! their defaults refuse with `unsupported`, which is what headless mode
//! answers. `focus` and `select` take the map tab (`viewport` id) to act
//! in, `None` for the focused one.

use std::path::Path;

use worlds_core::command::Change;
use worlds_core::{History, Id, Project, Slug};
use worlds_rules::{BakeCache, TilesetImages};

use crate::rpc::RpcError;

/// The document a request runs against, borrowed for the request.
pub struct Doc<'a> {
    pub project: &'a mut Project,
    /// The shared undo history: agent entries land here with
    /// `Origin::Agent(name)`.
    pub history: &'a mut History,
    /// Cached auto layer bakes. `project.bake` reads (and fills) it; the
    /// host keeps it current in [`Host::applied`].
    pub bakes: &'a mut BakeCache,
    /// Tileset images for bakes (`project.bake`, `project.export`,
    /// `rules.preview`).
    pub images: &'a TilesetImages,
    /// The project directory (absolute).
    pub dir: &'a Path,
}

/// Where `view.focus` and `view.snapshot` look.
#[derive(Debug, Clone, PartialEq)]
pub enum ViewTarget {
    /// A level, by slug.
    Level(Slug),
    /// A rect of world pixels `[x, y, w, h]` in `world` (the only world
    /// when `None`).
    WorldRect { world: Option<Slug>, rect: [i64; 4] },
    /// An annotation, by id: framed as built
    Note(Id),
    /// A whole world, by slug: it becomes the active world, fitted.
    World(Slug),
}

/// What `view.select` asks for: `level` alone (make it active, clear the selection),
/// or any of the lists (they add up; the first item decides the active level).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Selection {
    pub level: Option<Slug>,
    pub entities: Vec<Id>,
    pub notes: Vec<Id>,
    pub levels: Vec<Slug>,
}

/// A `view.snapshot` request.
#[derive(Debug, Clone, PartialEq)]
pub struct SnapshotRequest {
    pub target: ViewTarget,
    /// The largest image allowed, `[w, h]` (default `[1024, 1024]`).
    pub size: [u32; 2],
    /// Layers to draw; `None` is every visible layer.
    pub layers: Option<Vec<Slug>>,
    /// Entity boxes and open notes (default true).
    pub overlays: bool,
}

/// One of the editor's map tabs (`view.list_viewports`).
#[derive(Debug, Clone, PartialEq)]
pub struct ViewportInfo {
    pub id: u32,
    /// The world it shows.
    pub world: Slug,
    /// Its active level.
    pub level: Option<Slug>,
    /// It is the focused tab.
    pub focused: bool,
    /// What it shows, in world pixels `[x, y, w, h]` (rounded out).
    pub world_rect: [i64; 4],
    /// Screen pixels per world pixel.
    pub zoom: f64,
}

/// A rendered snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    /// PNG bytes (the server base64-encodes them).
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// Screen pixels per world pixel.
    pub scale: f64,
}

/// The document owner: the editor, or [`crate::headless::HeadlessHost`].
///
/// Only [`Host::doc`], [`Host::applied`] and [`Host::reload`] are
/// required; the rest have defaults that suit headless mode.
pub trait Host {
    /// Borrow the document for one request.
    fn doc(&mut self) -> Doc<'_>;

    /// The handler applied a mutation (a write, an undo or redo step, an
    /// accepted approval batch, a dropped connection's revert). `changes`
    /// are what `Project::apply` reported. Called before the response is
    /// sent; the editor invalidates and rebakes here.
    fn applied(&mut self, changes: &[Change]);

    /// A batch failed and so did its rollback (`CommandError::Rollback`):
    /// the project changed in unknown ways. Drop caches, redraw everything.
    fn rollback(&mut self) {}

    /// `project.reload`: load the project again from disk, replacing the
    /// document and dropping caches. The handler clears the history and
    /// bumps `rev` itself.
    fn reload(&mut self) -> Result<(), RpcError>;

    /// `project.save`: write the dirty files (and `export/` with
    /// `export_on_save`). Returns the project-relative paths saved. The
    /// default is [`crate::save_project`]; the editor overrides it to go
    /// through its own save (file watcher bookkeeping, title bar).
    fn save(&mut self) -> Result<Vec<String>, RpcError> {
        crate::save_project(&mut self.doc())
    }

    /// `project.validate` recomputed the problems (refresh the panel).
    fn problems(&mut self, _problems: &[worlds_core::Problem]) {}

    /// `view.focus`, in map tab `viewport` (`None`: the focused one).
    fn focus(&mut self, _target: &ViewTarget, _viewport: Option<u32>) -> Result<(), RpcError> {
        Err(needs_editor("view.focus"))
    }

    /// `view.select`; `viewport` is the tab whose active level follows
    /// (`None`: the focused one). The editor sends the `selection` event
    /// itself (through `ServerHandle::notify_selection`) when its selection
    /// changes.
    fn select(&mut self, _selection: &Selection, _viewport: Option<u32>) -> Result<(), RpcError> {
        Err(needs_editor("view.select"))
    }

    /// `view.list_viewports`: every map tab, in the order opened.
    fn viewports(&mut self) -> Result<Vec<ViewportInfo>, RpcError> {
        Err(needs_editor("view.list_viewports"))
    }

    /// `view.open_viewport`: a new map tab on `world` (it exists), focused
    /// with `focus`. Returns its id.
    fn open_viewport(&mut self, _world: &Slug, _focus: bool) -> Result<u32, RpcError> {
        Err(needs_editor("view.open_viewport"))
    }

    /// `view.snapshot`.
    fn snapshot(&mut self, _request: &SnapshotRequest) -> Result<Snapshot, RpcError> {
        Err(needs_editor("view.snapshot"))
    }

    /// `project.describe`'s `recovered`: what this editor session recovered
    /// from a previous session's autosave when it started, if it did.
    fn recovered(&mut self) -> Option<serde_json::Value> {
        None
    }
}

/// The `unsupported` error of a `view.*` method without an editor.
pub fn needs_editor(method: &str) -> RpcError {
    RpcError::unsupported(format!(
        "{method} needs the editor; open the project with `worlds <dir>`"
    ))
}

/// The `invalid params` error of a `viewport` id no map tab has.
pub fn unknown_viewport(id: u32) -> RpcError {
    RpcError::invalid_params(format!(
        "there is no viewport {id} (view.list_viewports lists them)"
    ))
}
