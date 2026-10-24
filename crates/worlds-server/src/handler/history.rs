//! `history.*`: undo and redo of the shared stack, and agent batches.
//!
//! A batch doesn't use the history's gesture slot (that belongs to the
//! user). Its writes apply at once; the handler keeps their inverses and
//! records them with `History::record` at `end_batch`, or earlier, as a
//! part of its own, when the user records an entry or the history is
//! stepped ([`Handler::flush_batches`]).

use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Value, json};
use worlds_core::command::{Change, coalesce};
use worlds_core::{Command, Project};

use super::events::{ChangeKind, changed};
use super::params;
use super::{Client, Collect, ConnId, Handler, OpenBatch, Outcome, Write};
use crate::host::Host;
use crate::rpc::RpcError;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StepParams {
    n: Option<usize>,
    #[serde(default)]
    any: bool,
}

fn step_builder(method: &str, p: Value, redo: bool) -> Result<Write, RpcError> {
    let p: StepParams = params::parse(method, p)?;
    Ok(Write::Step {
        redo,
        n: p.n.unwrap_or(1),
        any: p.any,
    })
}

/// `history.undo { n?, any?, expect_rev? }`.
pub(crate) fn undo_builder(_: &Project, method: &str, p: Value) -> Result<Write, RpcError> {
    step_builder(method, p, false)
}

/// `history.redo { n?, any?, expect_rev? }`.
pub(crate) fn redo_builder(_: &Project, method: &str, p: Value) -> Result<Write, RpcError> {
    step_builder(method, p, true)
}

/// `history.begin_batch { label }`.
pub(crate) fn begin_builder(_: &Project, method: &str, p: Value) -> Result<Write, RpcError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        label: String,
    }
    let p: P = params::parse(method, p)?;
    Ok(Write::BeginBatch { label: p.label })
}

/// Step the history up to `n` entries, stopping at the first entry this
/// client didn't make unless `any`.
pub(crate) fn step(
    h: &mut Handler,
    host: &mut dyn Host,
    client: &Client,
    redo: bool,
    n: usize,
    any: bool,
) -> Result<Value, RpcError> {
    let what = if redo { "history.redo" } else { "history.undo" };
    if any && h.approval {
        return Err(RpcError::unsupported(format!(
            "{what} with any: true is refused in approval mode; only your own entries can be stepped"
        )));
    }
    h.flush_batches(host.doc().history);
    let origin = client.origin();
    let mut steps = Vec::new();
    let mut all: Vec<Change> = Vec::new();
    let mut stopped = None;
    for _ in 0..n {
        let next = {
            let doc = host.doc();
            let stack = if redo {
                doc.history.redo_entries()
            } else {
                doc.history.undo_entries()
            };
            stack.last().map(|e| (e.label.clone(), e.origin.clone()))
        };
        let Some((label, entry_origin)) = next else {
            break;
        };
        if !any && entry_origin != origin {
            stopped = Some(json!({ "label": label, "origin": entry_origin }));
            break;
        }
        let res = {
            let doc = host.doc();
            if redo {
                doc.history.redo(doc.project)
            } else {
                doc.history.undo(doc.project)
            }
        };
        let changes = match res {
            Ok(Some(c)) => c,
            Ok(None) => break,
            Err(e) => {
                if steps.is_empty() {
                    return Err(h.failed(host, e));
                }
                // Report the steps that did apply; the failing entry stays.
                stopped = Some(
                    json!({ "label": label, "origin": entry_origin, "error": RpcError::command(&e).data }),
                );
                break;
            }
        };
        h.mutated(host, &changes);
        let kind = if redo {
            ChangeKind::Redo
        } else {
            ChangeKind::Undo
        };
        let changes = coalesce(changes);
        let params = changed(
            h.rev,
            &origin,
            Some(&client.session),
            Some(&entry_origin),
            &label,
            kind,
            &changes,
        );
        h.emit("changed", params, None);
        all.extend(changes);
        steps.push(json!({ "label": label, "origin": entry_origin }));
    }
    let mut v = json!({ "rev": h.rev, "steps": steps, "changes": coalesce(all) });
    if let Some(s) = stopped {
        v["stopped"] = s;
    }
    Ok(v)
}

/// `history.begin_batch`: open a batch on this connection.
pub(crate) fn begin_batch(
    h: &mut Handler,
    host: &mut dyn Host,
    client: &Client,
    label: String,
) -> Result<Value, RpcError> {
    if let Some(b) = h.batches.get(&client.conn) {
        return Err(RpcError::batch_state(format!(
            "a batch ({:?}) is already open on this connection; end it with history.end_batch first (batches don't nest)",
            b.label
        )));
    }
    let collect = h.approval.then(|| Collect {
        commands: Vec::new(),
        scratch: host.doc().project.clone(),
    });
    h.batches.insert(
        client.conn,
        OpenBatch {
            label,
            client: client.clone(),
            inverses: Vec::new(),
            parts: Vec::new(),
            recorded_changes: 0,
            changes: Vec::new(),
            entries: 0,
            last_activity: Instant::now(),
            collect,
        },
    );
    Ok(json!({ "rev": h.rev }))
}

/// `history.end_batch {}`. In approval mode the collected writes become a
/// pending batch and the request waits for the decision like a write
/// ([`Outcome::Pending`]).
pub(crate) fn end_batch(
    h: &mut Handler,
    host: &mut dyn Host,
    client: &Client,
    method: &str,
    p: Value,
) -> Outcome {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {}
    if let Err(e) = params::parse::<P>(method, p) {
        return Outcome::Reply(Err(e));
    }
    h.finish_batch(host, client.conn).unwrap_or_else(|| {
        Outcome::Reply(Err(RpcError::batch_state(
            "no batch is open on this connection (it was never begun, it timed out, or a reload closed it)",
        )))
    })
}

/// `history.cancel_batch {}`: end the open batch without recording or
/// submitting it. What isn't recorded yet is reverted (as for a dropped
/// connection); in approval mode the collected writes are dropped.
pub(crate) fn cancel_batch(
    h: &mut Handler,
    host: &mut dyn Host,
    client: &Client,
    method: &str,
    p: Value,
) -> Result<Value, RpcError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {}
    let P {} = params::parse(method, p)?;
    if !h.batches.contains_key(&client.conn) {
        return Err(RpcError::batch_state(
            "no batch is open on this connection (it was never begun, it timed out, or a reload closed it)",
        ));
    }
    h.revert_batch(host, client.conn);
    Ok(json!({ "rev": h.rev }))
}

/// `history.batch_status { batch_id }`.
pub(crate) fn batch_status(h: &mut Handler, method: &str, p: Value) -> Result<Value, RpcError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        batch_id: String,
    }
    let p: P = params::parse(method, p)?;
    h.batch_status(&p.batch_id)
}

impl Handler {
    /// End `conn`'s batch: record what is left as an entry and announce
    /// the batch; in approval mode, submit its collected writes. `None`
    /// when no batch is open.
    pub(crate) fn finish_batch(&mut self, host: &mut dyn Host, conn: ConnId) -> Option<Outcome> {
        let mut b = self.batches.remove(&conn)?;
        if let Some(c) = b.collect.take() {
            if c.commands.is_empty() {
                return Some(Outcome::Reply(Ok(
                    json!({ "rev": self.rev, "status": "applied",
                    "label": b.label, "changes": [], "entries": 0 }),
                )));
            }
            return Some(self.submit(host, &b.client, b.label, c.commands, None, true));
        }
        let inverses = std::mem::take(&mut b.inverses);
        let parts = std::mem::take(&mut b.parts);
        if host.doc().history.record_with(
            b.label.clone(),
            b.client.origin(),
            inverses,
            parts,
            &b.changes[b.recorded_changes..],
        ) {
            b.entries += 1;
        }
        let changes = coalesce(b.changes);
        if b.entries > 0 {
            self.agent_entry(&b.client, &b.label, &changes, None);
        }
        Some(Outcome::Reply(Ok(
            json!({ "rev": self.rev, "status": "applied",
            "label": b.label, "changes": changes, "entries": b.entries }),
        )))
    }

    /// A connection closed: revert the unrecorded part of its open batch
    /// (sending `changed` kind `cancel`) and forget its subscriptions. Its
    /// pending approval batches stay for the user to decide.
    pub fn disconnect(&mut self, host: &mut dyn Host, conn: ConnId) {
        self.unsubscribe_all(conn);
        self.revert_batch(host, conn);
    }

    /// Close `conn`'s open batch without recording it: revert its
    /// unrecorded writes (sending `changed` kind `cancel`), or drop its
    /// collected ones in approval mode.
    pub(crate) fn revert_batch(&mut self, host: &mut dyn Host, conn: ConnId) {
        let Some(b) = self.batches.remove(&conn) else {
            return;
        };
        if b.collect.is_some() || b.inverses.is_empty() {
            return;
        }
        let mut inverses = b.inverses;
        inverses.reverse();
        let res = host.doc().project.apply(Command::Batch {
            label: b.label.clone(),
            commands: inverses,
        });
        match res {
            Ok(a) => {
                if !a.changes.is_empty() {
                    self.mutated(host, &a.changes);
                    let changes = coalesce(a.changes);
                    let params = changed(
                        self.rev,
                        &b.client.origin(),
                        Some(&b.client.session),
                        None,
                        &b.label,
                        ChangeKind::Cancel,
                        &changes,
                    );
                    self.emit("changed", params, None);
                }
                self.agent_event(&b.client, None, &b.label, "cancelled");
            }
            Err(e) => {
                let _ = self.failed(host, e);
            }
        }
    }

    /// End batches idle for longer than `idle` (60 s in the server) as if
    /// `end_batch` were called. `activity` gives a connection's latest
    /// queued request, which counts as activity too.
    pub fn expire_batches(
        &mut self,
        host: &mut dyn Host,
        now: Instant,
        idle: Duration,
        activity: &dyn Fn(ConnId) -> Option<Instant>,
    ) -> Vec<ConnId> {
        let stale: Vec<ConnId> = self
            .batches
            .iter()
            .filter(|(c, b)| {
                let last = activity(**c).map_or(b.last_activity, |a| a.max(b.last_activity));
                now.saturating_duration_since(last) > idle
            })
            .map(|(c, _)| *c)
            .collect();
        for c in &stale {
            let _ = self.finish_batch(host, *c);
        }
        stale
    }
}
