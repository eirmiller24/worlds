//! `project.import_plan` and `project.import`: importing from another
//! Worlds project ([`worlds_core::merge`]). The plan is a read and is kept
//! under a `plan_id`; the import applies that plan as one batch, refused
//! with `stale_rev` once the project changed since it was planned.

use std::path::PathBuf;

use serde::Deserialize;
use serde_json::{Map, Value, json};
use worlds_core::merge::{self, Plan, Policy, Selection};

use super::write::Opts;
use super::{Client, Handler, Outcome, Write, params, with_rev};
use crate::host::Host;
use crate::rpc::RpcError;

/// How many plans are kept (the oldest goes first).
const KEPT: usize = 16;

/// A plan waiting for `project.import`.
#[derive(Debug, Clone)]
pub(crate) struct StoredPlan {
    pub id: String,
    /// The rev it was planned at.
    pub rev: u64,
    pub plan: Plan,
    /// The source project's directory.
    pub source: PathBuf,
}

/// `project.import_plan { source, selection?, policy? }` → the plan with a
/// `plan_id`. `source` is a project directory, absolute or relative to
/// this project's.
pub(crate) fn plan(
    h: &mut Handler,
    host: &mut dyn Host,
    method: &str,
    p: Value,
) -> Result<Value, RpcError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        source: String,
        #[serde(default)]
        selection: Selection,
        #[serde(default)]
        policy: Policy,
    }
    let p: P = params::parse(method, p)?;
    let doc = host.doc();
    let source = doc.dir.join(&p.source);
    let src = worlds_core::load(&source)
        .map_err(|e| RpcError::invalid_params(format!("{method}: can't load {}: {e}", p.source)))?;
    let plan = merge::plan(&src, doc.project, &p.selection, &p.policy)
        .map_err(|e| RpcError::command(&e))?;
    h.import_seq += 1;
    let id = format!("plan_{}", h.import_seq);
    let mut v = serde_json::to_value(&plan).map_err(|e| RpcError::internal(e.to_string()))?;
    v["plan_id"] = json!(id);
    v["empty"] = json!(plan.is_empty());
    h.import_plans.push(StoredPlan {
        id,
        rev: h.rev,
        plan,
        source,
    });
    if h.import_plans.len() > KEPT {
        h.import_plans.remove(0);
    }
    with_rev(h.rev, v)
}

/// `project.import { plan_id }`: copy the plan's files, then apply it as
/// one batch (one undo entry). The result adds `plan_id`, `summary` and
/// `copied` (the files written under the project).
pub(crate) fn apply(
    h: &mut Handler,
    host: &mut dyn Host,
    client: &Client,
    method: &str,
    params: &mut Map<String, Value>,
) -> Outcome {
    let opts = match Opts::take(method, params) {
        Ok(o) => o,
        Err(e) => return Outcome::Reply(Err(e)),
    };
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        plan_id: String,
    }
    let p: P = match params::parse(method, Value::Object(std::mem::take(params))) {
        Ok(p) => p,
        Err(e) => return Outcome::Reply(Err(e)),
    };
    if let Some(expected) = opts.expect_rev
        && expected != h.rev
    {
        return Outcome::Reply(Err(RpcError::stale_rev(expected, h.rev)));
    }
    let Some(at) = h.import_plans.iter().position(|s| s.id == p.plan_id) else {
        return Outcome::Reply(Err(RpcError::invalid_params(format!(
            "{method}: no plan {} (plans are kept until used, and only the last {KEPT}); call \
             project.import_plan again",
            p.plan_id
        ))));
    };
    let stored = &h.import_plans[at];
    if stored.rev != h.rev {
        let mut e = RpcError::stale_rev(stored.rev, h.rev);
        e.message = format!(
            "the project changed since plan {} was made (rev {} then, {} now): call \
             project.import_plan again",
            stored.id, stored.rev, h.rev
        );
        return Outcome::Reply(Err(e));
    }
    if stored.plan.is_empty() {
        return Outcome::Reply(Err(RpcError::invalid(format!(
            "nothing to import: {}",
            stored.plan.summary
        ))));
    }
    let stored = h.import_plans.remove(at);
    let copied = {
        let doc = host.doc();
        match merge::copy_assets(&stored.plan, &stored.source, doc.dir) {
            Ok(c) => c,
            Err(e) => return Outcome::Reply(Err(RpcError::io("art/", e.to_string()))),
        }
    };
    let extra = json!({
        "plan_id": stored.id,
        "summary": stored.plan.summary,
        "copied": copied,
    });
    let mut opts = opts;
    if opts.label.is_none() {
        opts.label = Some(format!("{}: {}", client.name, stored.plan.label));
    }
    h.write(
        host,
        client,
        &opts,
        Write::Commands {
            cmds: vec![stored.plan.command()],
            id: Some(extra),
            confirm: None,
        },
    )
}
