//! The write path: one request's commands applied, recorded and announced
//! (or, in approval mode, collected or submitted as a pending batch).

use std::time::Instant;

use serde_json::{Map, Value, json};
use worlds_core::command::{Change, coalesce};
use worlds_core::{Command, CommandError};

use super::{ChangeKind, Client, Handler, Outcome, events};
use crate::host::Host;
use crate::rpc::RpcError;

/// Why a destructive write needs `confirm: true`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Confirm {
    /// `data.reason`.
    pub reason: String,
    /// The error message.
    pub message: String,
}

impl Confirm {
    /// "<what> needs confirm: true" with `reason` as the reason.
    pub fn new(reason: impl Into<String>, message: impl Into<String>) -> Self {
        Confirm {
            reason: reason.into(),
            message: message.into(),
        }
    }
}

/// What a write request asks for, built from its params.
#[derive(Debug, Clone)]
pub(crate) enum Write {
    /// Apply commands (several become one `batch`).
    Commands {
        cmds: Vec<Command>,
        /// What the result reports besides the write result: the id of
        /// what it creates (a string, under `id`), or an object whose keys
        /// are added to the result.
        id: Option<Value>,
        /// Set when the write needs `confirm: true`.
        confirm: Option<Confirm>,
    },
    /// `history.undo` / `history.redo`.
    Step { redo: bool, n: usize, any: bool },
    /// `history.begin_batch`.
    BeginBatch { label: String },
    /// `project.reload`.
    Reload { confirm: Option<Confirm> },
}

impl Write {
    /// One or more commands with nothing else to report.
    pub fn cmds(cmds: Vec<Command>) -> Self {
        Write::Commands {
            cmds,
            id: None,
            confirm: None,
        }
    }

    /// A single command.
    pub fn one(cmd: Command) -> Self {
        Write::cmds(vec![cmd])
    }
}

/// The keys every write takes besides its command's fields.
#[derive(Debug, Clone, Default)]
pub(crate) struct Opts {
    pub expect_rev: Option<u64>,
    pub label: Option<String>,
    pub confirm: bool,
}

impl Opts {
    /// Take `expect_rev`, `label` and `confirm` out of `params`
    /// (`history.begin_batch` keeps `label`: it is the batch's).
    pub fn take(method: &str, params: &mut Map<String, Value>) -> Result<Opts, RpcError> {
        let bad = |key: &str, what: &str| {
            RpcError::invalid_params(format!("{method}: {key} must be {what}"))
        };
        let expect_rev = match params.remove("expect_rev") {
            None | Some(Value::Null) => None,
            Some(v) => Some(
                v.as_u64()
                    .ok_or_else(|| bad("expect_rev", "a revision number"))?,
            ),
        };
        let label = if method == "history.begin_batch" {
            None
        } else {
            match params.remove("label") {
                None | Some(Value::Null) => None,
                Some(Value::String(s)) => Some(s),
                Some(_) => return Err(bad("label", "a string")),
            }
        };
        let confirm = match params.remove("confirm") {
            None | Some(Value::Null) => false,
            Some(Value::Bool(b)) => b,
            Some(_) => return Err(bad("confirm", "true or false")),
        };
        Ok(Opts {
            expect_rev,
            label,
            confirm,
        })
    }
}

/// Several commands as one: the command itself, or a `batch` labelled like
/// its first command.
pub(crate) fn combine(mut cmds: Vec<Command>) -> Command {
    if cmds.len() == 1 {
        return cmds.remove(0);
    }
    let label = cmds.first().map(Command::label).unwrap_or_default();
    Command::Batch {
        label,
        commands: cmds,
    }
}

/// A write result.
pub(crate) fn result(
    rev: u64,
    status: &str,
    label: &str,
    changes: &[Change],
    id: Option<&Value>,
) -> Value {
    let mut v = json!({ "rev": rev, "status": status, "label": label, "changes": changes });
    add_extra(&mut v, id);
    v
}

/// Put a write's extra result (see [`Write::Commands`] `id`) into `v`: an
/// object's keys are added, anything else goes under `id`.
fn add_extra(v: &mut Value, extra: Option<&Value>) {
    match extra {
        Some(Value::Object(m)) => {
            for (k, x) in m {
                v[k.as_str()] = x.clone();
            }
        }
        Some(id) => v["id"] = id.clone(),
        None => {}
    }
}

impl Handler {
    pub(crate) fn write(
        &mut self,
        host: &mut dyn Host,
        client: &Client,
        opts: &Opts,
        w: Write,
    ) -> Outcome {
        match w {
            Write::Commands { cmds, id, confirm } => {
                if let Some(c) = confirm
                    && !opts.confirm
                {
                    return Outcome::Reply(Err(RpcError::needs_confirm(c.reason, c.message)));
                }
                if cmds.is_empty() {
                    return Outcome::Reply(Err(RpcError::invalid_params("nothing to do")));
                }
                self.apply_commands(host, client, opts, cmds, id)
            }
            Write::Step { redo, n, any } => {
                Outcome::Reply(super::history::step(self, host, client, redo, n, any))
            }
            Write::BeginBatch { label } => {
                Outcome::Reply(super::history::begin_batch(self, host, client, label))
            }
            Write::Reload { confirm } => {
                if self.approval {
                    return Outcome::Reply(Err(RpcError::unsupported(
                        "project.reload is refused in approval mode; ask the user to reload",
                    )));
                }
                if let Some(c) = confirm
                    && !opts.confirm
                {
                    return Outcome::Reply(Err(RpcError::needs_confirm(c.reason, c.message)));
                }
                Outcome::Reply(super::project::reload(self, host, client))
            }
        }
    }

    fn apply_commands(
        &mut self,
        host: &mut dyn Host,
        client: &Client,
        opts: &Opts,
        cmds: Vec<Command>,
        id: Option<Value>,
    ) -> Outcome {
        let cmd = combine(cmds);
        let in_batch = self.batches.contains_key(&client.conn);
        let label = match self.batches.get(&client.conn) {
            Some(b) => b.label.clone(),
            None => opts
                .label
                .clone()
                .unwrap_or_else(|| format!("{}: {}", client.name, cmd.label())),
        };

        // Approval mode, inside a batch: collect against the scratch copy.
        if let Some(c) = self
            .batches
            .get_mut(&client.conn)
            .and_then(|b| b.collect.as_mut())
        {
            if let Err(e) = c.scratch.apply(cmd.clone()) {
                return Outcome::Reply(Err(RpcError::command(&e)));
            }
            c.commands.push(cmd);
            let mut v = json!({ "rev": self.rev, "status": "collected" });
            add_extra(&mut v, id.as_ref());
            return Outcome::Reply(Ok(v));
        }
        // Approval mode, outside a batch: one pending batch.
        if self.approval && !in_batch {
            return self.submit(host, client, label, vec![cmd], id, false);
        }

        let applied = {
            let doc = host.doc();
            doc.project.apply(cmd)
        };
        let applied = match applied {
            Ok(a) => a,
            Err(e) => return Outcome::Reply(Err(self.failed(host, e))),
        };
        if applied.changes.is_empty() {
            return Outcome::Reply(Ok(result(self.rev, "applied", &label, &[], id.as_ref())));
        }
        self.mutated(host, &applied.changes);
        let changes = coalesce(applied.changes);
        match self.batches.get_mut(&client.conn) {
            Some(b) => {
                b.inverses.push(applied.inverse);
                b.changes.extend(changes.iter().cloned());
                b.last_activity = Instant::now();
            }
            None => {
                host.doc()
                    .history
                    .record(label.clone(), client.origin(), vec![applied.inverse]);
                self.agent_entry(client, &label, &changes, None);
            }
        }
        Outcome::Reply(Ok(result(
            self.rev,
            "applied",
            &label,
            &changes,
            id.as_ref(),
        )))
    }

    /// Count a mutation: bump `rev`, tell the host, remember the changes.
    pub(crate) fn mutated(&mut self, host: &mut dyn Host, changes: &[Change]) {
        self.rev += 1;
        host.applied(changes);
        self.applied.extend_from_slice(changes);
    }

    /// A command was refused. After a failed rollback the project did
    /// change: count it and let the host drop its caches.
    pub(crate) fn failed(&mut self, host: &mut dyn Host, e: CommandError) -> RpcError {
        if matches!(e, CommandError::Rollback { .. }) {
            self.rev += 1;
            host.rollback();
        }
        RpcError::command(&e)
    }

    /// An agent entry was recorded: the `changed` event (own connection
    /// included) and the `agent` event for observers.
    pub(crate) fn agent_entry(
        &mut self,
        client: &Client,
        label: &str,
        changes: &[Change],
        batch_id: Option<&str>,
    ) {
        self.last_batch = Some(label.to_string());
        let params = events::changed(
            self.rev,
            &client.origin(),
            Some(&client.session),
            None,
            label,
            ChangeKind::Edit,
            changes,
        );
        self.emit("changed", params, None);
        self.agent_event(client, batch_id, label, "applied");
    }

    /// Submit a pending approval batch: check it on a scratch copy first.
    pub(crate) fn submit(
        &mut self,
        host: &mut dyn Host,
        client: &Client,
        label: String,
        commands: Vec<Command>,
        id: Option<Value>,
        is_batch: bool,
    ) -> Outcome {
        let checked = {
            let doc = host.doc();
            let mut scratch = doc.project.clone();
            scratch.apply(Command::Batch {
                label: label.clone(),
                commands: commands.clone(),
            })
        };
        let changes = match checked {
            Ok(a) => coalesce(a.changes),
            Err(e) => return Outcome::Reply(Err(RpcError::command(&e))),
        };
        let batch_id = self.approvals.submit(
            client,
            label.clone(),
            commands,
            changes,
            id,
            is_batch,
            self.rev,
        );
        self.agent_event(client, Some(&batch_id), &label, "queued");
        Outcome::Pending { batch_id }
    }
}
