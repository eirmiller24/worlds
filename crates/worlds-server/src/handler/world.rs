//! `world.*` and `level.move_to_world`: creating, renaming, reordering and
//! removing worlds, and moving a level between them.

use serde::Deserialize;
use serde_json::Value;
use worlds_core::{Command, CommandError, Project, Slug};

use super::params;
use super::{Confirm, Write};
use crate::rpc::RpcError;

/// `world.create { slug, name? }` → `create_world`.
pub(crate) fn create(_: &Project, method: &str, p: Value) -> Result<Write, RpcError> {
    Ok(Write::one(params::command(method, "create_world", p)?))
}

/// `world.rename { world, slug?, name? }` → `rename_world`.
pub(crate) fn rename(_: &Project, method: &str, p: Value) -> Result<Write, RpcError> {
    let cmd = params::command(method, "rename_world", p)?;
    if let Command::RenameWorld {
        slug: None,
        name: None,
        ..
    } = &cmd
    {
        return Err(RpcError::invalid_params(format!(
            "{method}: nothing to change; give slug or name"
        )));
    }
    Ok(Write::one(cmd))
}

/// `world.remove { world, confirm }` → `remove_world`. A world with
/// levels, world notes or guide groups needs `confirm: true`.
pub(crate) fn remove(project: &Project, method: &str, p: Value) -> Result<Write, RpcError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        world: Slug,
    }
    let p: P = params::parse(method, p)?;
    let w = project
        .world(p.world.as_str())
        .ok_or_else(|| RpcError::command(&CommandError::UnknownWorld(p.world.clone())))?;
    let held: Vec<String> = [
        (w.levels.len(), "level"),
        (w.notes.len(), "world note"),
        (w.guide_groups.len(), "guide group"),
    ]
    .into_iter()
    .filter(|(n, _)| *n > 0)
    .map(|(n, what)| format!("{n} {what}{}", if n == 1 { "" } else { "s" }))
    .collect();
    let confirm = (!held.is_empty()).then(|| {
        let held = held.join(", ");
        Confirm::new(
            format!("deletes world {} with {held}", p.world),
            format!(
                "deleting world {} needs confirm: true: it holds {held}",
                p.world
            ),
        )
    });
    Ok(Write::Commands {
        cmds: vec![Command::RemoveWorld {
            world: p.world,
            // The server asked for `confirm` above.
            confirm: true,
        }],
        id: None,
        confirm,
    })
}

/// `world.reorder { order }` → `reorder_worlds`.
pub(crate) fn reorder(_: &Project, method: &str, p: Value) -> Result<Write, RpcError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        order: Vec<Slug>,
    }
    let p: P = params::parse(method, p)?;
    Ok(Write::one(Command::ReorderWorlds {
        order: p.order,
        restore: false,
    }))
}

/// `level.move_to_world { level, world, world_px? }` → `move_level_to_world`.
pub(crate) fn move_level(_: &Project, method: &str, p: Value) -> Result<Write, RpcError> {
    Ok(Write::one(params::command(
        method,
        "move_level_to_world",
        p,
    )?))
}
