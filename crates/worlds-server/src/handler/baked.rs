//! Reads that need bake results: the **effective** grid of a derived layer
//! (its overrides over what its writer's rules computed), taken from the
//! host's [`worlds_rules::BakeCache`]; when it isn't there, only its writer
//! and what that depends on are baked into the cache
//! ([`BakeCache::bake_derived`]), as the editor does for its views.

use std::collections::BTreeMap;
use std::sync::Arc;

use worlds_core::{IntGridLayer, LayerDef, Project, Slug};
use worlds_rules::BakeCache;

use crate::host::Doc;
use crate::rpc::RpcError;

/// The effective grid of derived layer `layer` in `level`, from the cache
/// (the level is baked into it first when it isn't there). An error says
/// why it can't be computed (the writer's bake failed).
pub(crate) fn derived_grid(
    doc: &mut Doc<'_>,
    level: &str,
    layer: &str,
) -> Result<Arc<IntGridLayer>, RpcError> {
    let r = doc
        .bakes
        .bake_derived(doc.project, level, layer, doc.images);
    match r {
        Ok(Some(g)) => Ok(g),
        Ok(None) => Err(RpcError::invalid(format!(
            "layer {layer} isn't a derived layer"
        ))),
        Err(e) => Err(RpcError::invalid(format!(
            "the effective grid of derived layer {layer} in level {level} can't be computed: {e}"
        ))),
    }
}

/// The effective grid of every derived layer of `level` that can be
/// computed (those whose writer bakes), by layer slug, for the text view.
/// `shown` limits it to some layers.
pub(crate) fn derived_grids(
    doc: &mut Doc<'_>,
    level: &str,
    shown: impl Fn(&Slug) -> bool,
) -> BTreeMap<Slug, IntGridLayer> {
    let derived: Vec<Slug> = doc
        .project
        .layers
        .iter()
        .filter(|l| l.is_derived() && shown(&l.slug))
        .map(|l| l.slug.clone())
        .collect();
    // Each one's writers, only those not cached yet.
    derived
        .into_iter()
        .filter_map(|d| {
            let g = doc
                .bakes
                .bake_derived(doc.project, level, d.as_str(), doc.images)
                .ok()??;
            Some((d, (*g).clone()))
        })
        .collect()
}

/// The values of int_grid layer `def` in `level` as rules read them: the
/// effective grid of a derived layer, the stored cells of any other (an
/// empty grid when the level has none).
pub(crate) fn effective_values(
    doc: &mut Doc<'_>,
    level: &str,
    def: &LayerDef,
) -> Result<Arc<IntGridLayer>, RpcError> {
    if def.is_derived() {
        return derived_grid(doc, level, def.slug.as_str());
    }
    let project: &Project = doc.project;
    let (_, lvl) = super::params::find_level(project, level)?;
    Ok(Arc::new(match lvl.int_grid(def.slug.as_str()) {
        Some(g) => g.clone(),
        None => IntGridLayer::new_empty(def, project.layer_cells(def, lvl)),
    }))
}

/// Keep `bakes` current after `changes` the way the editor does: cached
/// levels are brought up to date incrementally
/// ([`BakeCache::apply_changes`]); what the cache can't follow (a
/// definition rename, a project setting) drops everything, to be rebaked
/// when next read; a deleted level is dropped.
pub(crate) fn follow_changes(
    bakes: &mut BakeCache,
    project: &Project,
    images: &worlds_rules::TilesetImages,
    changes: &[worlds_core::command::Change],
) {
    use worlds_core::command::Change;
    if changes
        .iter()
        .any(|c| matches!(c, Change::DefRenamed { .. } | Change::Project { .. }))
    {
        bakes.clear();
        return;
    }
    for cached in bakes.cached_levels() {
        // The slug after the changes (`apply_changes` moves a renamed
        // level's entry itself).
        let mut level = cached.clone();
        for c in changes {
            if let Change::LevelRenamed { from, to, .. } = c
                && *from == level
            {
                level = to.clone();
            }
        }
        if project.level(level.as_str()).is_none() {
            bakes.invalidate(cached.as_str());
            continue;
        }
        // A layer that fails to rebake is left uncached and baked again
        // when next read, which reports its error.
        if let Err(e) = bakes.apply_changes(project, level.as_str(), images, changes) {
            tracing::debug!("cache update of {level}: {e}");
        }
    }
}
