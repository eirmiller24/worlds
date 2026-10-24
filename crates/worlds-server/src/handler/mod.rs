//! The method handler: one JSON-RPC request against a borrowed document.
//!
//! [`Handler`] is transport-free. It holds what outlives a request (the
//! project revision, open batches, approval batches, subscriptions) and runs
//! each request against the document a [`Host`] lends it. Notifications and
//! approval decisions it produces are queued as [`Out`] items; the caller
//! (the socket server, or the in-process [`crate::headless::Session`])
//! delivers them.
//!
//! Methods live in one module per group (`project.rs`, `grid.rs`, ...).
//! Reads return a value; writes build [`Write`]s (commands plus what the
//! result reports) that [`Handler::write`] applies, records and announces.

mod baked;
mod defs;
mod edit;
mod entity;
pub(crate) mod events;
mod grid;
mod history;
mod level;
mod notes;
pub(crate) mod params;
mod project;
mod rules;
mod tiles;
mod view;
mod write;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};
use worlds_core::command::{Change, Origin};
use worlds_core::{Command, Project};

use crate::approval::Approvals;
use crate::host::Host;
use crate::rpc::RpcError;

pub(crate) use baked::follow_changes;
pub use events::{ChangeKind, UserChange};
pub(crate) use project::export_levels;
pub(crate) use write::{Confirm, Write, result as write_result};

/// A connection, as the server numbers them.
pub type ConnId = u64;

/// The client a request comes from: what `session.hello` established.
#[derive(Debug, Clone, PartialEq)]
pub struct Client {
    pub conn: ConnId,
    /// `client.name` from the hello: the agent's name in the history.
    pub name: String,
    pub version: String,
    /// 8 random characters naming this connection.
    pub session: String,
    /// How long a write may block waiting for approval.
    pub approval_wait: Duration,
}

impl Client {
    /// A client for in-process use (no socket): connection 0.
    pub fn in_process(name: &str) -> Client {
        Client {
            conn: 0,
            name: name.to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            session: crate::random_session(),
            approval_wait: Duration::ZERO,
        }
    }

    /// `Origin::Agent(name)`.
    pub fn origin(&self) -> Origin {
        Origin::Agent(self.name.clone())
    }
}

/// What a request produced.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// Send this response now.
    Reply(Result<Value, RpcError>),
    /// Approval mode: the write became pending batch `batch_id`. The
    /// transport waits up to the client's `approval_wait` for an
    /// [`Out::Decided`] with this id, else replies `queued`.
    Pending { batch_id: String },
}

/// Something the handler wants delivered.
#[derive(Debug, Clone, PartialEq)]
pub enum Out {
    /// A notification for one connection.
    Notify { conn: ConnId, message: Value },
    /// The user decided approval batch `batch_id` of `conn`: the result the
    /// blocked request should get (`applied` result, `rejected` or the
    /// command error). If the request already returned `queued`, the
    /// transport sends `notification` (a `batch` event) instead.
    Decided {
        conn: ConnId,
        batch_id: String,
        result: Result<Value, RpcError>,
        notification: Value,
    },
}

/// A batch opened with `history.begin_batch`.
#[derive(Debug, Clone)]
pub(crate) struct OpenBatch {
    pub label: String,
    pub client: Client,
    /// Inverses of the writes not recorded in the history yet.
    pub inverses: Vec<Command>,
    /// Every change of the batch so far (for `end_batch` and its event).
    pub changes: Vec<Change>,
    /// Entries recorded so far (parts flushed before a user edit or undo).
    pub entries: usize,
    pub last_activity: Instant,
    /// Approval mode: writes are collected here, not applied.
    pub collect: Option<Collect>,
}

/// An approval-mode batch's collected writes.
#[derive(Debug, Clone)]
pub(crate) struct Collect {
    pub commands: Vec<Command>,
    /// The project with the collected writes applied (checks each write).
    pub scratch: Project,
}

/// Settings of a [`Handler`].
#[derive(Debug, Clone, Default)]
pub struct HandlerConfig {
    /// Headless: no approval mode, `view.*` unsupported.
    pub headless: bool,
    /// The revision to start at (0 for a freshly loaded project).
    pub rev: u64,
}

/// The method handler. See the module docs.
pub struct Handler {
    pub(crate) rev: u64,
    pub(crate) headless: bool,
    pub(crate) approval: bool,
    pub(crate) batches: BTreeMap<ConnId, OpenBatch>,
    pub(crate) approvals: Approvals,
    pub(crate) subs: HashMap<ConnId, BTreeSet<String>>,
    pub(crate) out: Vec<Out>,
    pub(crate) last_batch: Option<String>,
    /// Changes applied since the last [`Handler::take_applied`].
    pub(crate) applied: Vec<Change>,
}

/// Methods that change the project (or step the history). While one
/// connection has a batch open, the queue holds back other connections'
/// writes.
pub fn is_write(method: &str) -> bool {
    matches!(
        method,
        "project.reload"
            | "project.update"
            | "defs.set"
            | "defs.rename"
            | "defs.remove"
            | "level.create"
            | "level.delete"
            | "level.move"
            | "level.resize"
            | "level.update"
            | "level.set_fields"
            | "level.set_layer_state"
            | "level.replace"
            | "grid.set_cells"
            | "grid.fill_rect"
            | "grid.flood_fill"
            | "grid.line"
            | "grid.replace_value"
            | "tiles.set"
            | "tiles.clear_rect"
            | "tiles.stamp"
            | "entity.add"
            | "entity.update"
            | "entity.move"
            | "entity.remove"
            | "notes.add"
            | "notes.update"
            | "notes.resolve"
            | "notes.remove"
            | "notes.move"
            | "notes.set_guide_group"
            | "notes.remove_guide_group"
            | "rules.set_group"
            | "rules.remove_group"
            | "rules.set_rule"
            | "rules.remove_rule"
            | "rules.move_rule"
            | "rules.set_macro"
            | "rules.remove_macro"
            | "rules.rename_macro"
            | "rules.set_stamp"
            | "rules.remove_stamp"
            | "history.undo"
            | "history.redo"
            | "history.begin_batch"
            | "edit.paste"
            | "level.duplicate"
    )
}

impl Handler {
    pub fn new(config: HandlerConfig) -> Self {
        Handler {
            rev: config.rev,
            headless: config.headless,
            approval: false,
            batches: BTreeMap::new(),
            approvals: Approvals::default(),
            subs: HashMap::new(),
            out: Vec::new(),
            last_batch: None,
            applied: Vec::new(),
        }
    }

    /// The current project revision.
    pub fn rev(&self) -> u64 {
        self.rev
    }

    /// Whether approval mode is on.
    pub fn approval(&self) -> bool {
        self.approval
    }

    /// The label of the last agent entry recorded (a batch or a write).
    pub fn last_batch(&self) -> Option<&str> {
        self.last_batch.as_deref()
    }

    /// Connections with an open batch.
    pub fn open_batches(&self) -> impl Iterator<Item = ConnId> + '_ {
        self.batches.keys().copied()
    }

    /// Notifications and decisions to deliver, oldest first.
    pub fn take_out(&mut self) -> Vec<Out> {
        std::mem::take(&mut self.out)
    }

    /// Every change applied since the last call, in order (writes, undo
    /// and redo steps, accepted batches, reverts). The host already saw
    /// each in [`Host::applied`]; this is for the caller's own bookkeeping.
    pub fn take_applied(&mut self) -> Vec<Change> {
        std::mem::take(&mut self.applied)
    }

    /// Run one request. `session.hello` is the transport's job and is
    /// answered here only for in-process callers.
    pub fn call(
        &mut self,
        host: &mut dyn Host,
        client: &Client,
        method: &str,
        params: Value,
    ) -> Outcome {
        let mut params = match params {
            Value::Object(m) => m,
            Value::Null => Map::new(),
            _ => {
                return Outcome::Reply(Err(RpcError::invalid_params(
                    "params must be an object of named parameters",
                )));
            }
        };
        if let Some(b) = self.batches.get_mut(&client.conn) {
            b.last_activity = Instant::now();
        }
        if let Some(builder) = write_builder(method) {
            return self.run_write(host, client, method, &mut params, builder);
        }
        if method == "history.end_batch" {
            return history::end_batch(self, host, client, method, Value::Object(params));
        }
        Outcome::Reply(self.read_or_control(host, client, method, Value::Object(params)))
    }

    fn run_write(
        &mut self,
        host: &mut dyn Host,
        client: &Client,
        method: &str,
        params: &mut Map<String, Value>,
        builder: Builder,
    ) -> Outcome {
        let opts = match write::Opts::take(method, params) {
            Ok(o) => o,
            Err(e) => return Outcome::Reply(Err(e)),
        };
        if let Some(expected) = opts.expect_rev
            && expected != self.rev
        {
            return Outcome::Reply(Err(RpcError::stale_rev(expected, self.rev)));
        }
        // Inside an approval-mode batch the request builds against the
        // batch's scratch copy: earlier collected writes count (a layer it
        // defined, a level it created), though none of them applied yet.
        let params = Value::Object(std::mem::take(params));
        let built = match self
            .batches
            .get(&client.conn)
            .and_then(|b| b.collect.as_ref())
        {
            Some(c) => builder(&c.scratch, method, params),
            None => builder(host.doc().project, method, params),
        };
        match built {
            Ok(w) => self.write(host, client, &opts, w),
            Err(e) => Outcome::Reply(Err(e)),
        }
    }

    fn read_or_control(
        &mut self,
        host: &mut dyn Host,
        client: &Client,
        method: &str,
        params: Value,
    ) -> Result<Value, RpcError> {
        let rev = self.rev;
        let read = |host: &mut dyn Host, f: ReadFn| -> Result<Value, RpcError> {
            let doc = host.doc();
            with_rev(rev, f(doc.project, method, params.clone())?)
        };
        match method {
            "session.hello" => Ok(json!({
                "protocol": crate::PROTOCOL, "server": crate::server_info(),
                "project": { "dir": host.doc().dir.to_string_lossy(), "name": host.doc().project.name },
                "rev": self.rev, "headless": self.headless, "approval": self.approval,
                "session": client.session,
            })),
            "project.describe" => project::describe(self, host, method, params),
            "project.validate" => project::validate(self, host, method, params),
            "project.bake" => project::bake(self, host, method, params),
            "project.export" => project::export(self, host, method, params),
            "project.save" => project::save(self, host, method, params),
            "project.read_file" => read(host, project::read_file),
            "project.reload" | "project.update" => unreachable_write(method),
            "defs.get" => read(host, defs::get),
            "level.list" => read(host, level::list),
            "level.get" => level::get(self, host, method, params),
            "grid.get_cells" => grid::get_cells(self, host, method, params),
            "tiles.get" => read(host, tiles::get),
            "entity.list" => read(host, entity::list),
            "entity.get" => read(host, entity::get),
            "entity.find" => read(host, entity::find),
            "notes.list" => read(host, notes::list),
            "level.find" => read(host, level::find),
            "tiles.find" => read(host, tiles::find),
            "rules.list" => read(host, rules::list),
            "rules.get_group" => read(host, rules::get_group),
            "rules.get_macros" => read(host, rules::get_macros),
            "rules.expand" => read(host, rules::expand),
            "rules.preview" => rules::preview(self, host, method, params),
            "rules.regions" => rules::regions(self, host, method, params),
            "history.cancel_batch" => history::cancel_batch(self, host, client, method, params),
            "history.batch_status" => history::batch_status(self, method, params),
            "view.focus" | "view.select" | "view.snapshot" => {
                view::call(self, host, method, params)
            }
            "events.subscribe" | "events.unsubscribe" => {
                events::subscribe(self, client, method, params)
            }
            "edit.copy" => read(host, edit::copy),
            _ => Err(RpcError::method_not_found(method)),
        }
    }
}

fn unreachable_write(method: &str) -> Result<Value, RpcError> {
    Err(RpcError::internal(format!(
        "{method} was not routed as a write"
    )))
}

/// A read: the project and the params (an object) to a result without `rev`.
pub(crate) type ReadFn = fn(&Project, &str, Value) -> Result<Value, RpcError>;

/// A write builder: params (an object, without `expect_rev`, `label` and
/// `confirm`) to the commands to apply.
pub(crate) type Builder = fn(&Project, &str, Value) -> Result<Write, RpcError>;

/// Add `rev` to a result object.
pub(crate) fn with_rev(rev: u64, v: Value) -> Result<Value, RpcError> {
    match v {
        Value::Object(mut m) => {
            let mut out = Map::new();
            out.insert("rev".into(), rev.into());
            out.append(&mut m);
            Ok(Value::Object(out))
        }
        other => Ok(json!({ "rev": rev, "value": other })),
    }
}

fn write_builder(method: &str) -> Option<Builder> {
    Some(match method {
        "project.reload" => project::reload_builder,
        "project.update" => project::update,
        "defs.set" => defs::set,
        "defs.rename" => defs::rename,
        "defs.remove" => defs::remove,
        "level.create" => level::create,
        "level.delete" => level::delete,
        "level.move" => level::move_level,
        "level.resize" => level::resize,
        "level.update" => level::update,
        "level.set_fields" => level::set_fields,
        "level.set_layer_state" => level::set_layer_state,
        "level.replace" => level::replace,
        "grid.set_cells" => grid::set_cells,
        "grid.fill_rect" => grid::fill_rect,
        "grid.flood_fill" => grid::flood_fill,
        "grid.line" => grid::line,
        "grid.replace_value" => grid::replace_value,
        "tiles.set" => tiles::set,
        "tiles.clear_rect" => tiles::clear_rect,
        "tiles.stamp" => tiles::stamp,
        "entity.add" => entity::add,
        "entity.update" => entity::update,
        "entity.move" => entity::move_entity,
        "entity.remove" => entity::remove,
        "notes.add" => notes::add,
        "notes.update" => notes::update,
        "notes.resolve" => notes::resolve,
        "notes.remove" => notes::remove,
        "notes.move" => notes::move_notes,
        "notes.set_guide_group" => notes::set_guide_group,
        "notes.remove_guide_group" => notes::remove_guide_group,
        "rules.set_group" => rules::set_group,
        "rules.remove_group" => rules::remove_group,
        "rules.set_rule" => rules::set_rule,
        "rules.remove_rule" => rules::remove_rule,
        "rules.move_rule" => rules::move_rule,
        "rules.set_macro" => rules::set_macro,
        "rules.remove_macro" => rules::remove_macro,
        "rules.rename_macro" => rules::rename_macro,
        "rules.set_stamp" => rules::set_stamp,
        "rules.remove_stamp" => rules::remove_stamp,
        "history.undo" => history::undo_builder,
        "history.redo" => history::redo_builder,
        "history.begin_batch" => history::begin_builder,
        "edit.paste" => edit::paste,
        "level.duplicate" => level::duplicate,
        _ => return None,
    })
}
