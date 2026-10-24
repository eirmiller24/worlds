//! `edit.*`: the clipboard, assemblies and transforms. `edit.copy` reads a
//! fragment out of the project; `edit.paste` writes one in as a single
//! batch ([`worlds_core::fragment`] does the work); `edit.place_assembly`
//! pastes an assembly, `edit.save_assembly` makes one, `edit.transform`
//! mirrors or rotates a selection and `level.arrange` aligns levels
//! ([`worlds_core::assembly`], [`worlds_core::transform`]).

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::{Value, json};
use worlds_core::assembly::{self, Assembly, AssemblyInfo};
use worlds_core::command::{Command, CommandError, DefKind};
use worlds_core::fragment::{self, CopySelection, Fragment, PasteOptions, PastePlan};
use worlds_core::transform::{self, ArrangeOp, TransformOp};
use worlds_core::{Flip, Project, Slug};

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
        flip: Option<Flip>,
    }
    let p: P = params::parse(method, p)?;
    let mut frag = parse_fragment(method, &p.fragment)?;
    if let Some(f) = p.flip {
        frag = transform::fragment(project, &frag, f).map_err(|e| RpcError::command(&e))?;
    }
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
    plan_write(plan, "paste")
}

/// A fragment given as JSON: one `edit.copy` returns, or one written by hand.
fn parse_fragment(method: &str, v: &Value) -> Result<Fragment, RpcError> {
    match Fragment::from_text(&v.to_string()) {
        Some(Ok(f)) => Ok(f),
        Some(Err(m)) => Err(RpcError::invalid_params(format!("{method}: {m}"))),
        None => Err(RpcError::invalid_params(format!(
            "{method}: fragment must be a fragment object ({{ \"worlds_fragment\": 1, ... }}, as edit.copy returns)"
        ))),
    }
}

/// The write of a paste plan, with what it made in the result.
fn plan_write(plan: PastePlan, what: &str) -> Result<Write, RpcError> {
    if plan.commands.is_empty() {
        let why = if plan.problems.is_empty() {
            "the fragment is empty".to_string()
        } else {
            plan.problems.join("; ")
        };
        return Err(RpcError::invalid(format!("nothing to {what}: {why}")));
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

/// `edit.place_assembly { level, slug, origin, flip? }` → one batch,
/// `Place assembly <slug>`.
pub(crate) fn place_assembly(project: &Project, method: &str, p: Value) -> Result<Write, RpcError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        level: Slug,
        slug: Slug,
        origin: [i64; 2],
        flip: Option<Flip>,
    }
    let p: P = params::parse(method, p)?;
    let plan = assembly::place(project, &p.slug, &p.level, p.origin, p.flip)
        .map_err(|e| RpcError::command(&e))?;
    plan_write(plan, "place")
}

/// `edit.save_assembly { selection | fragment, slug, name, category?, tags?,
/// doc?, replace? }` → `set_assembly`.
pub(crate) fn save_assembly(project: &Project, method: &str, p: Value) -> Result<Write, RpcError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        selection: Option<CopySelection>,
        fragment: Option<Value>,
        slug: Slug,
        name: String,
        category: Option<String>,
        #[serde(default)]
        tags: Vec<String>,
        doc: Option<String>,
        #[serde(default)]
        replace: bool,
    }
    let p: P = params::parse(method, p)?;
    let frag = match (&p.selection, &p.fragment) {
        (Some(sel), None) => fragment::copy(project, sel).map_err(|e| RpcError::command(&e))?,
        (None, Some(f)) => parse_fragment(method, f)?,
        _ => {
            return Err(RpcError::invalid_params(format!(
                "{method}: give either selection (what edit.copy takes) or fragment"
            )));
        }
    };
    if frag.is_empty() {
        return Err(RpcError::invalid("nothing to save: the fragment is empty"));
    }
    if !p.replace && project.assembly(p.slug.as_str()).is_some() {
        return Err(RpcError::command(&CommandError::DuplicateDef {
            kind: DefKind::Assembly,
            slug: p.slug,
        }));
    }
    let items = frag.item_count();
    let assembly = Assembly::new(
        AssemblyInfo {
            slug: p.slug,
            name: p.name,
            category: p.category,
            tags: p.tags,
            doc: p.doc,
        },
        frag,
    );
    let size = assembly.size;
    Ok(Write::Commands {
        cmds: vec![Command::SetAssembly { assembly }],
        id: Some(json!({ "items": items, "size": size })),
        confirm: None,
    })
}

/// `edit.transform { selection, op }` → one batch turning it in place.
pub(crate) fn transform(project: &Project, method: &str, p: Value) -> Result<Write, RpcError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        selection: CopySelection,
        op: TransformOp,
    }
    let p: P = params::parse(method, p)?;
    let plan = transform::selection(project, &p.selection, p.op.flip())
        .map_err(|e| RpcError::command(&e))?;
    if plan.commands.is_empty() {
        return Err(RpcError::invalid(
            "nothing to turn: the selection is the same turned".to_string(),
        ));
    }
    Ok(Write::Commands {
        cmds: vec![plan.command()],
        id: Some(json!({ "notes": plan.notes, "rect": plan.rect })),
        confirm: None,
    })
}

/// `level.arrange { levels, op }` → one batch of `move_level`.
pub(crate) fn arrange(project: &Project, method: &str, p: Value) -> Result<Write, RpcError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        levels: Vec<Slug>,
        op: ArrangeOp,
    }
    let p: P = params::parse(method, p)?;
    let cmd =
        transform::arrange_levels(project, &p.levels, p.op).map_err(|e| RpcError::command(&e))?;
    Ok(Write::one(cmd))
}
