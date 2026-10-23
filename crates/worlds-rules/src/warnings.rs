//! Warnings about an export that the game's importer can't read faithfully.

use std::collections::BTreeMap;

use worlds_core::export::BakedLevel;
use worlds_core::{Project, Slug};

/// What a game importer would get wrong in an export of `baked` (level
/// slug → that level's bake, as [`crate::bake_for_export`] makes it), as
/// sentences. `worlds-cli bake` prints them; the server's `project.bake`
/// and `project.export` and MCP `bake_and_validate` return them as
/// `warnings`.
///
/// Empty today, for every export. It used to list things an older Godot
/// importer couldn't read (transposed tiles, tiles off their layer's grid,
/// several auto layers with tiles in one level). The mechanism stays so a future limitation of the
/// export's readers has one place to go. Game-specific rules (layer names,
/// grid sizes, required layers) are not checked here: they belong to that
/// game's own import step.
pub fn export_warnings(_project: &Project, _baked: &BTreeMap<Slug, BakedLevel>) -> Vec<String> {
    Vec::new()
}
