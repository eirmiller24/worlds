//! Subscriptions and the events the server sends (`docs/api.md`, "events").
//!
//! The handler queues each notification as an [`Out::Notify`] for every
//! connection subscribed to it. The editor reports its own side (the
//! user's edits, selection, saves, problems, reloads) through the
//! `notify_*` methods here.

use std::collections::BTreeSet;

use serde_json::{Value, json};
use worlds_core::command::{Change, Origin, coalesce};
use worlds_core::{History, Problem};

use super::{Client, ConnId, Handler, Out, params};
use crate::rpc::{RpcError, notification};

/// Every event a connection can subscribe to.
pub const EVENTS: [&str; 7] = [
    "changed",
    "selection",
    "saved",
    "problems",
    "agent",
    "batch",
    "approval",
];

/// `kind` of a `changed` event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Edit,
    Undo,
    Redo,
    /// Re-read everything (`changes` and `files` are empty).
    Reload,
    /// A dropped connection's open batch was reverted.
    Cancel,
}

impl ChangeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ChangeKind::Edit => "edit",
            ChangeKind::Undo => "undo",
            ChangeKind::Redo => "redo",
            ChangeKind::Reload => "reload",
            ChangeKind::Cancel => "cancel",
        }
    }
}

/// A mutation the user made in the editor, for [`Handler::notify_changed`].
#[derive(Debug, Clone)]
pub struct UserChange<'a> {
    /// `Edit` for an entry (a gesture counts once, when it ends), `Undo`,
    /// `Redo`, or `Reload` (files changed on disk; use
    /// [`Handler::notify_reloaded`] for that).
    pub kind: ChangeKind,
    /// The entry's label.
    pub label: &'a str,
    /// For undo and redo: who made the entry.
    pub entry_origin: Option<Origin>,
    /// What `Project::apply` (or `History::undo`/`redo`) reported.
    pub changes: &'a [Change],
}

/// `changed` params.
pub(crate) fn changed(
    rev: u64,
    origin: &Origin,
    session: Option<&str>,
    entry_origin: Option<&Origin>,
    label: &str,
    kind: ChangeKind,
    changes: &[Change],
) -> Value {
    let mut files: Vec<String> = Vec::new();
    for c in changes {
        for f in c.files() {
            let f = params::rel_path(&f);
            if !files.contains(&f) {
                files.push(f);
            }
        }
    }
    let mut v = json!({ "rev": rev, "origin": origin, "label": label, "kind": kind.as_str(),
        "changes": changes, "files": files });
    if let Some(s) = session {
        v["session"] = s.into();
    }
    if let Some(e) = entry_origin {
        v["entry_origin"] = json!(e);
    }
    v
}

/// `events.subscribe` / `events.unsubscribe`.
pub(crate) fn subscribe(
    h: &mut Handler,
    client: &Client,
    method: &str,
    p: Value,
) -> Result<Value, RpcError> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        events: Vec<String>,
    }
    let p: P = params::parse(method, p)?;
    for e in &p.events {
        if !EVENTS.contains(&e.as_str()) {
            return Err(RpcError::invalid_params(format!(
                "{method}: no event {e:?}; the events are {}",
                EVENTS.join(", ")
            )));
        }
    }
    let subs = h.subs.entry(client.conn).or_default();
    if method == "events.subscribe" {
        subs.extend(p.events);
    } else {
        for e in &p.events {
            subs.remove(e);
        }
    }
    let list: Vec<&String> = subs.iter().collect();
    Ok(json!({ "rev": h.rev, "events": list }))
}

impl Handler {
    /// Queue `event` for every subscribed connection except `except`.
    pub(crate) fn emit(&mut self, event: &str, params: Value, except: Option<ConnId>) {
        let message = notification(event, params);
        let mut conns: Vec<ConnId> = self
            .subs
            .iter()
            .filter(|(c, s)| Some(**c) != except && s.contains(event))
            .map(|(c, _)| *c)
            .collect();
        conns.sort_unstable();
        for conn in conns {
            self.out.push(Out::Notify {
                conn,
                message: message.clone(),
            });
        }
    }

    /// The `agent` event for observers of `client`'s batch.
    pub(crate) fn agent_event(
        &mut self,
        client: &Client,
        batch_id: Option<&str>,
        label: &str,
        status: &str,
    ) {
        let mut v = json!({ "client": client.name, "session": client.session, "label": label, "status": status });
        if let Some(b) = batch_id {
            v["batch_id"] = b.into();
        }
        self.emit("agent", v, Some(client.conn));
    }

    /// Forget a connection's subscriptions.
    pub(crate) fn unsubscribe_all(&mut self, conn: ConnId) {
        self.subs.remove(&conn);
    }

    /// The subscriptions of a connection.
    pub fn subscriptions(&self, conn: ConnId) -> BTreeSet<String> {
        self.subs.get(&conn).cloned().unwrap_or_default()
    }

    /// Record the unrecorded part of every open batch as an entry of its
    /// own. The editor calls this (through `ServerHandle`) right before it
    /// records a user entry or steps the history, so the undo stack stays
    /// in the order things happened and a user stroke never joins an
    /// agent's batch. The handler calls it before its own undo and redo.
    pub fn flush_batches(&mut self, history: &mut History) {
        for b in self.batches.values_mut() {
            let inverses = std::mem::take(&mut b.inverses);
            if history.record(b.label.clone(), b.client.origin(), inverses) {
                b.entries += 1;
            }
        }
    }

    /// The user changed the project in the editor: bump `rev` and send
    /// `changed` with origin `user`. Call it once per recorded entry (a
    /// gesture when it ends), undo or redo step, after it applied. Returns
    /// the new `rev`. A change list that is empty (a no-op) changes nothing.
    pub fn notify_changed(&mut self, c: UserChange<'_>) -> u64 {
        if c.changes.is_empty() && c.kind != ChangeKind::Reload {
            return self.rev;
        }
        if c.kind == ChangeKind::Reload {
            return self.notify_reloaded();
        }
        self.rev += 1;
        let changes = coalesce(c.changes.to_vec());
        let params = changed(
            self.rev,
            &Origin::User,
            None,
            c.entry_origin.as_ref(),
            c.label,
            c.kind,
            &changes,
        );
        self.emit("changed", params, None);
        self.rev
    }

    /// The editor reloaded the project from disk (files changed, or the
    /// user asked): bump `rev`, close every open batch without recording
    /// it, reject every pending approval batch, send `changed` kind
    /// `reload`. The editor clears its own history. Returns the new `rev`.
    pub fn notify_reloaded(&mut self) -> u64 {
        self.reloaded(&Origin::User, None)
    }

    pub(crate) fn reloaded(&mut self, origin: &Origin, session: Option<&str>) -> u64 {
        self.rev += 1;
        self.batches.clear();
        self.reject_all("the project was reloaded");
        let params = changed(
            self.rev,
            origin,
            session,
            None,
            "Reload",
            ChangeKind::Reload,
            &[],
        );
        self.emit("changed", params, None);
        self.rev
    }

    /// The editor's selection or active level changed.
    pub fn notify_selection(
        &mut self,
        level: Option<&str>,
        entities: &[String],
        notes: &[String],
        levels: &[String],
    ) {
        self.emit(
            "selection",
            json!({ "level": level, "entities": entities, "notes": notes, "levels": levels }),
            None,
        );
    }

    /// A save wrote `files` (project-relative).
    pub fn notify_saved(&mut self, files: &[String]) {
        self.emit("saved", json!({ "rev": self.rev, "files": files }), None);
    }

    /// The problems list was recomputed.
    pub fn notify_problems(&mut self, problems: &[Problem]) {
        let list: Vec<Value> = problems.iter().map(problem_json).collect();
        self.emit(
            "problems",
            json!({ "rev": self.rev, "problems": list }),
            None,
        );
    }
}

/// A problem on the wire: `{ file, path, message }`.
pub(crate) fn problem_json(p: &Problem) -> Value {
    json!({ "file": p.file, "path": p.path, "message": p.message })
}
