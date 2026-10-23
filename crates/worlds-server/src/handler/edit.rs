//! `edit.*`: the clipboard. `edit.copy` reads a fragment out of the
//! project; `edit.paste` writes one in as a single batch
//! ([`worlds_core::fragment`] does the work).

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::{Value, json};
use worlds_core::fragment::{self, CopySelection, Fragment, PasteOptions};
use worlds_core::{Project, Slug};

use super::Write;
use super::params;
use crate::rpc::RpcError;

/// `edit.copy { selection }` → `{ fragment }`.
pub(crate) fn copy(project: &Project, method: &str, p: Value) -> Result<Value, RpcError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        selection: CopySelection,
    }
    let p: P = params::parse(method, p)?;
    let frag = fragment::copy(project, &p.selection).map_err(|e| RpcError::command(&e))?;
    Ok(json!({ "fragment": frag }))
}

/// `edit.paste { level, origin, fragment, layer_map? }` → one batch.
pub(crate) fn paste(project: &Project, method: &str, p: Value) -> Result<Write, RpcError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        level: Slug,
        origin: [i64; 2],
        fragment: Value,
        #[serde(default)]
        layer_map: BTreeMap<Slug, Slug>,
    }
    let p: P = params::parse(method, p)?;
    let frag: Fragment = match Fragment::from_text(&p.fragment.to_string()) {
        Some(Ok(f)) => f,
        Some(Err(m)) => return Err(RpcError::invalid_params(format!("{method}: {m}"))),
        None => {
            return Err(RpcError::invalid_params(format!(
                "{method}: fragment must be a fragment object ({{ \"worlds_fragment\": 1, ... }}, as edit.copy returns)"
            )));
        }
    };
    let plan = fragment::paste(
        project,
        &frag,
        &PasteOptions {
            level: p.level,
            origin: p.origin,
            layer_map: p.layer_map,
        },
    )
    .map_err(|e| RpcError::command(&e))?;
    if plan.commands.is_empty() {
        let why = if plan.problems.is_empty() {
            "the fragment is empty".to_string()
        } else {
            plan.problems.join("; ")
        };
        return Err(RpcError::invalid(format!("nothing to paste: {why}")));
    }
    let extra = json!({
        "items": plan.items,
        "ids": plan.ids,
        "entities": plan.entities,
        "notes": plan.notes,
        "problems": plan.problems,
    });
    Ok(Write::Commands {
        cmds: vec![plan.command()],
        id: Some(extra),
        confirm: None,
    })
}
