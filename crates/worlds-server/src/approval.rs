//! Approval mode: agent writes wait as pending batches until the user
//! accepts or rejects them (`docs/api.md`, "Approval mode").
//!
//! The handler submits a [`PendingBatch`] after checking it on a scratch
//! copy; the editor lists them ([`crate::ServerHandle::pending`]), draws the
//! ghost from `commands`, and calls `apply_pending` or `reject_pending`.
//! The waiting itself happens in the transport, not here.

use std::collections::HashMap;
use std::time::Instant;

use serde_json::{Value, json};
use worlds_core::Command;
use worlds_core::command::{Change, coalesce};

use crate::handler::{ChangeKind, Client, ConnId, Handler, Out, events};
use crate::host::Host;
use crate::rpc::RpcError;

/// An agent batch waiting for the user.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingBatch {
    /// `b1`, `b2`, ...: what `batch` events and `history.batch_status` name.
    pub id: String,
    /// The client's name (its `Origin::Agent`).
    pub client: String,
    /// The client's connection session.
    pub session: String,
    pub conn: ConnId,
    /// The undo entry's label and the toast text.
    pub label: String,
    /// What accepting applies, as one `batch`, in order.
    pub commands: Vec<Command>,
    /// What applying it to the project at submission reported (coalesced):
    /// where to draw the ghost.
    pub changes: Vec<Change>,
    /// `rev` when it was submitted.
    pub rev: u64,
    pub submitted: Instant,
    /// The id a creating write reports (`entity.add`, ...).
    pub(crate) created_id: Option<Value>,
    /// Submitted by `end_batch` (its result has `entries`).
    pub(crate) is_batch: bool,
}

/// How a batch ended.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Decision {
    pub status: &'static str,
    pub rev_applied: Option<u64>,
    pub error: Option<Value>,
}

/// Pending batches in submission order, and the decided ones' outcomes.
#[derive(Debug, Default)]
pub(crate) struct Approvals {
    next: u64,
    pub pending: Vec<PendingBatch>,
    pub decided: HashMap<String, Decision>,
}

impl Approvals {
    #[allow(clippy::too_many_arguments)]
    pub fn submit(
        &mut self,
        client: &Client,
        label: String,
        commands: Vec<Command>,
        changes: Vec<Change>,
        created_id: Option<Value>,
        is_batch: bool,
        rev: u64,
    ) -> String {
        self.next += 1;
        let id = format!("b{}", self.next);
        self.pending.push(PendingBatch {
            id: id.clone(),
            client: client.name.clone(),
            session: client.session.clone(),
            conn: client.conn,
            label,
            commands,
            changes,
            rev,
            submitted: Instant::now(),
            created_id,
            is_batch,
        });
        id
    }
}

impl Handler {
    /// Turn approval mode on or off (the editor's toolbar toggle); sends
    /// `approval`. Pending batches stay either way. Headless refuses it.
    pub fn set_approval(&mut self, enabled: bool) {
        if self.headless || self.approval == enabled {
            return;
        }
        self.approval = enabled;
        self.emit("approval", json!({ "enabled": enabled }), None);
    }

    /// The batches waiting for the user, oldest first.
    pub fn pending(&self) -> &[PendingBatch] {
        &self.approvals.pending
    }

    /// Accept a pending batch: apply it as one `batch`, record one undo
    /// entry with the client's origin and the batch's label, bump `rev`
    /// once. Returns the changes, or the command error if it no longer
    /// applies (the batch is then `failed`). `Err` with `None` inside means
    /// there is no pending batch `id`. Batches are accepted oldest first: a
    /// later one is refused (`invalid`, nothing changes, it stays pending)
    /// while an older one waits.
    pub fn apply_pending(
        &mut self,
        host: &mut dyn Host,
        id: &str,
    ) -> Result<Vec<Change>, Option<RpcError>> {
        let Some(ix) = self.approvals.pending.iter().position(|p| p.id == id) else {
            return Err(None);
        };
        if ix > 0 {
            let first = &self.approvals.pending[0];
            return Err(Some(RpcError::invalid(format!(
                "batches are accepted oldest first: apply or reject {} ({}) before {id}",
                first.id, first.label
            ))));
        }
        let p = self.approvals.pending.remove(ix);
        self.flush_batches(host.doc().history);
        let applied = {
            let doc = host.doc();
            doc.project.apply(Command::Batch {
                label: p.label.clone(),
                commands: p.commands.clone(),
            })
        };
        let client = Client {
            conn: p.conn,
            name: p.client.clone(),
            version: String::new(),
            session: p.session.clone(),
            approval_wait: Default::default(),
        };
        match applied {
            Err(e) => {
                let err = self.failed(host, e);
                self.decide(&p, "failed", Err(err.clone()));
                self.agent_event(&client, Some(&p.id), &p.label, "failed");
                Err(Some(err))
            }
            Ok(a) => {
                let changes = coalesce(a.changes.clone());
                let recorded = if a.changes.is_empty() {
                    false
                } else {
                    self.mutated(host, &a.changes);
                    host.doc()
                        .history
                        .record(p.label.clone(), client.origin(), vec![a.inverse])
                };
                if recorded {
                    self.last_batch = Some(p.label.clone());
                    let params = events::changed(
                        self.rev,
                        &client.origin(),
                        Some(&client.session),
                        None,
                        &p.label,
                        ChangeKind::Edit,
                        &changes,
                    );
                    self.emit("changed", params, None);
                }
                let mut v = crate::handler::write_result(
                    self.rev,
                    "applied",
                    &p.label,
                    &changes,
                    p.created_id.as_ref(),
                );
                if p.is_batch {
                    v["entries"] = usize::from(recorded).into();
                }
                self.decide(&p, "applied", Ok(v));
                self.agent_event(&client, Some(&p.id), &p.label, "applied");
                Ok(changes)
            }
        }
    }

    /// Reject a pending batch and the later ones of the same connection
    /// (they may build on it). Returns the ids rejected.
    pub fn reject_pending(&mut self, id: &str) -> Vec<String> {
        let Some(ix) = self.approvals.pending.iter().position(|p| p.id == id) else {
            return Vec::new();
        };
        let session = self.approvals.pending[ix].session.clone();
        let mut out = Vec::new();
        let mut i = ix;
        while i < self.approvals.pending.len() {
            if self.approvals.pending[i].session == session {
                let p = self.approvals.pending.remove(i);
                self.reject_one(&p, "the user rejected this batch");
                out.push(p.id);
            } else {
                i += 1;
            }
        }
        out
    }

    /// Reject every pending batch (a reload).
    pub(crate) fn reject_all(&mut self, why: &str) {
        for p in std::mem::take(&mut self.approvals.pending) {
            self.reject_one(&p, why);
        }
    }

    fn reject_one(&mut self, p: &PendingBatch, why: &str) {
        let err = RpcError::rejected(format!("{why} ({}: {})", p.id, p.label));
        self.decide(p, "rejected", Err(err));
        let client = Client {
            conn: p.conn,
            name: p.client.clone(),
            version: String::new(),
            session: p.session.clone(),
            approval_wait: Default::default(),
        };
        self.agent_event(&client, Some(&p.id), &p.label, "rejected");
    }

    fn decide(&mut self, p: &PendingBatch, status: &'static str, result: Result<Value, RpcError>) {
        let rev_applied = result
            .as_ref()
            .ok()
            .and_then(|v| v.get("rev"))
            .and_then(Value::as_u64);
        let error = match &result {
            Err(e) if status == "failed" => Some(
                e.data
                    .clone()
                    .unwrap_or_else(|| json!({ "message": e.message })),
            ),
            _ => None,
        };
        let mut notification = json!({ "batch_id": p.id, "status": status });
        if let Some(r) = rev_applied {
            notification["rev"] = r.into();
        }
        if let Some(e) = &error {
            notification["error"] = e.clone();
        }
        self.approvals.decided.insert(
            p.id.clone(),
            Decision {
                status,
                rev_applied,
                error,
            },
        );
        self.out.push(Out::Decided {
            conn: p.conn,
            batch_id: p.id.clone(),
            result,
            notification: crate::rpc::notification("batch", notification),
        });
    }

    /// `history.batch_status`.
    pub(crate) fn batch_status(&self, batch_id: &str) -> Result<Value, RpcError> {
        if self.approvals.pending.iter().any(|p| p.id == batch_id) {
            return Ok(json!({ "rev": self.rev, "batch_id": batch_id, "status": "queued" }));
        }
        let Some(d) = self.approvals.decided.get(batch_id) else {
            return Err(RpcError::invalid(format!(
                "no approval batch {batch_id:?}: batch ids come from a queued result"
            )));
        };
        let mut v = json!({ "rev": self.rev, "batch_id": batch_id, "status": d.status });
        if let Some(r) = d.rev_applied {
            v["rev_applied"] = r.into();
        }
        if let Some(e) = &d.error {
            v["error"] = e.clone();
        }
        Ok(v)
    }
}
