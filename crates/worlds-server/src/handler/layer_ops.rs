//! Layer operations: `defs.duplicate` and `defs.merge_down` (layers), and
//! moving tile stacks (`tiles.move_to_layer`) or entities
//! (`entity.move_to_layer`) to another layer of the same kind. All but the
//! first are batches `worlds_core::command::layer_ops` builds.

use serde::Deserialize;
use serde_json::Value;
use worlds_core::command::layer_ops;
use worlds_core::{Command, Id, Project, Slug};

use super::Write;
use super::params;
use crate::rpc::RpcError;

fn layer_kind(method: &str, kind: &str) -> Result<(), RpcError> {
    if kind == "layer" {
        Ok(())
    } else {
        Err(RpcError::invalid_params(format!(
            "{method}: only kind \"layer\" can be {}; got {kind:?}",
            if method == "defs.duplicate" {
                "duplicated"
            } else {
                "merged down"
            }
        )))
    }
}

/// `defs.duplicate { kind: "layer", slug, to? }` → `duplicate_layer`
/// (`to` defaults to `<slug>_copy`).
pub(crate) fn duplicate(_: &Project, method: &str, p: Value) -> Result<Write, RpcError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        kind: String,
        slug: Slug,
        to: Option<Slug>,
    }
    let p: P = params::parse(method, p)?;
    layer_kind(method, &p.kind)?;
    let to = match p.to {
        Some(t) => t,
        None => Slug::new(format!("{}_copy", p.slug))
            .map_err(|e| RpcError::invalid_params(format!("{method}: {e}")))?,
    };
    Ok(Write::one(Command::DuplicateLayer {
        layer: p.slug,
        slug: to,
    }))
}

/// `defs.merge_down { kind: "layer", slug }`: the tiles layer into the
/// tiles layer right below it ([`layer_ops::merge_down`]).
pub(crate) fn merge_down(project: &Project, method: &str, p: Value) -> Result<Write, RpcError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        kind: String,
        slug: Slug,
    }
    let p: P = params::parse(method, p)?;
    layer_kind(method, &p.kind)?;
    let cmd = layer_ops::merge_down(project, &p.slug).map_err(|e| RpcError::command(&e))?;
    Ok(Write::one(cmd))
}

/// `tiles.move_to_layer { level, layer, to, cells }`
/// ([`layer_ops::move_tiles`]).
pub(crate) fn move_tiles(project: &Project, method: &str, p: Value) -> Result<Write, RpcError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        level: Slug,
        layer: Slug,
        to: Slug,
        cells: Vec<[u32; 2]>,
    }
    let p: P = params::parse(method, p)?;
    if p.cells.is_empty() {
        return Err(RpcError::invalid_params(format!(
            "{method}: cells is empty"
        )));
    }
    let cmd = layer_ops::move_tiles(project, &p.level, &p.layer, &p.to, &p.cells)
        .map_err(|e| RpcError::command(&e))?;
    Ok(Write::one(cmd))
}

/// `entity.move_to_layer { ids, layer }` ([`layer_ops::move_entities`]).
pub(crate) fn move_entities(project: &Project, method: &str, p: Value) -> Result<Write, RpcError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        ids: Vec<Id>,
        layer: Slug,
    }
    let p: P = params::parse(method, p)?;
    if p.ids.is_empty() {
        return Err(RpcError::invalid_params(format!("{method}: ids is empty")));
    }
    let cmd =
        layer_ops::move_entities(project, &p.ids, &p.layer).map_err(|e| RpcError::command(&e))?;
    Ok(Write::one(cmd))
}
