//! Derived grids across auto layers: the bake order, baking the writers a
//! layer depends on, and effective grids (overrides over computed values).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use worlds_core::{DerivedCycle, IntGridLayer, LayerDef, Level, Project, Slug};

use crate::bake::{auto_def, bake_with};
use crate::{Error, LayerBake, Result, TilesetImages};

/// Effective grids of derived layers, by slug.
pub(crate) type Effective = BTreeMap<Slug, Arc<IntGridLayer>>;

/// The auto layers in bake order (writers before readers, ties in
/// `layers.json` order), and the ones that can't be ordered (on a cycle or
/// depending on one) with the cycle.
pub(crate) fn order(project: &Project) -> (Vec<Slug>, Vec<(Slug, DerivedCycle)>) {
    if !project.layers.iter().any(|l| l.is_derived()) {
        // No derived grids, no dependencies: `layers.json` order.
        let auto = project.layers.iter().filter(|l| l.is_auto());
        return (auto.map(|l| l.slug.clone()).collect(), Vec::new());
    }
    let auto: Vec<&Slug> = project
        .layers
        .iter()
        .filter(|l| l.is_auto())
        .map(|l| &l.slug)
        .collect();
    let deps: Vec<Vec<usize>> = auto
        .iter()
        .map(|l| {
            project
                .layer_dependencies(l.as_str())
                .into_iter()
                .filter_map(|(_, w)| auto.iter().position(|a| **a == w))
                .collect()
        })
        .collect();
    let mut done = vec![false; auto.len()];
    let mut out = Vec::with_capacity(auto.len());
    while let Some(i) = (0..auto.len()).find(|&i| !done[i] && deps[i].iter().all(|&w| done[w])) {
        done[i] = true;
        out.push(auto[i].clone());
    }
    let mut cyclic = Vec::new();
    if out.len() < auto.len() {
        let cycle = project
            .auto_layer_order()
            .expect_err("some layer couldn't be ordered");
        for (i, l) in auto.iter().enumerate() {
            if !done[i] {
                cyclic.push(((*l).clone(), cycle.clone()));
            }
        }
    }
    (out, cyclic)
}

/// Every writer `layer` depends on, transitively, in bake order. A cycle
/// among them is an error.
pub(crate) fn upstream_layers(project: &Project, layer: &str) -> Result<Vec<Slug>> {
    if !project.layers.iter().any(|l| l.is_derived()) {
        return Ok(Vec::new());
    }
    let mut need: BTreeSet<Slug> = BTreeSet::new();
    let mut stack: Vec<Slug> = project
        .layer_dependencies(layer)
        .into_iter()
        .map(|(_, w)| w)
        .collect();
    while let Some(w) = stack.pop() {
        if w == layer {
            // `layer` reads (through others) a grid it writes itself.
            let (_, cyclic) = order(project);
            let cycle = cyclic.into_iter().next().map(|(_, c)| c);
            return Err(match cycle {
                Some(c) => Error::DerivedCycle(c),
                None => Error::UnknownLayer(layer.to_string()),
            });
        }
        if need.insert(w.clone()) {
            stack.extend(
                project
                    .layer_dependencies(w.as_str())
                    .into_iter()
                    .map(|(_, w)| w),
            );
        }
    }
    if need.is_empty() {
        return Ok(Vec::new());
    }
    let (ordered, cyclic) = order(project);
    if let Some((_, c)) = cyclic
        .into_iter()
        .find(|(l, _)| need.contains(l) || l == layer)
    {
        return Err(Error::DerivedCycle(c));
    }
    Ok(ordered.into_iter().filter(|l| need.contains(l)).collect())
}

/// Bake the writers `layer` depends on and return the effective grids of
/// every derived grid they write.
pub(crate) fn upstream(
    project: &Project,
    level: &Level,
    layer: &str,
    images: &TilesetImages,
) -> Result<Effective> {
    let mut eff = Effective::new();
    for w in upstream_layers(project, layer)? {
        let def = auto_def(project, w.as_str())?;
        let bake = bake_with(project, level, def, images, &eff).map_err(|e| match e {
            // The first layer that failed says why.
            Error::UpstreamFailed { .. } | Error::DerivedCycle(_) => e,
            other => Error::UpstreamFailed {
                layer: layer.to_string(),
                upstream: w.to_string(),
                reason: other.to_string(),
            },
        })?;
        publish(project, level, &bake, &mut eff);
    }
    Ok(eff)
}

/// Add the effective grids of the derived grids `bake` computed.
pub(crate) fn publish(project: &Project, level: &Level, bake: &LayerBake, eff: &mut Effective) {
    for (slug, computed) in &bake.derived {
        if let Some(def) = project.layer(slug.as_str()) {
            eff.insert(
                slug.clone(),
                Arc::new(effective(project, level, def, Some(computed))),
            );
        }
    }
}

/// A derived layer's effective grid in `level`
/// ([`Project::derived_effective`]).
pub(crate) fn effective(
    project: &Project,
    level: &Level,
    def: &LayerDef,
    computed: Option<&IntGridLayer>,
) -> IntGridLayer {
    project.derived_effective(level, def, computed)
}

/// The **effective** values of int_grid layer `grid` in `level`: for a
/// derived layer, its overrides over what its writer computes (the writer
/// and what it depends on are baked for this); for any other int_grid
/// layer, its stored cells (an empty grid when the level has none). Sized
/// in cells of the layer's grid.
pub fn effective_grid(
    project: &Project,
    level: &Level,
    grid: &str,
    images: &TilesetImages,
) -> Result<IntGridLayer> {
    let def = project
        .layer(grid)
        .filter(|l| l.is_int_grid())
        .ok_or_else(|| Error::UnknownLayer(grid.to_string()))?;
    if !def.is_derived() {
        let [w, h] = project.layer_cells(def, level);
        return Ok(match level.int_grid(grid) {
            Some(g) => g.clone(),
            None => IntGridLayer::new_empty(def, [w, h]),
        });
    }
    let Some(writer) = project.derived_writer(grid) else {
        return Ok(effective(project, level, def, None));
    };
    let writer = writer.clone();
    let bake = crate::bake_layer(project, level, &writer, images)?;
    Ok(effective(project, level, def, bake.derived.get(grid)))
}
