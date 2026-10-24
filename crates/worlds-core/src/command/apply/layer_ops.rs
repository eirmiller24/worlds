//! `duplicate_layer`: a layer's definition, content, rule state and rule set
//! copied under a new slug, applied as the `set_layer_def` that creates it,
//! so its inverse is that layer's `remove_layer_def`.

use std::collections::BTreeSet;

use super::Result;
use crate::command::{Applied, CommandError, DefKind, LayerContent};
use crate::id::{Id, Slug};
use crate::model::Project;

impl Project {
    pub(super) fn duplicate_layer(&mut self, layer: Slug, slug: Slug) -> Result<Applied> {
        let i = self
            .layers
            .iter()
            .position(|l| l.slug == layer)
            .ok_or_else(|| CommandError::UnknownLayer(layer.clone()))?;
        if self.layer(slug.as_str()).is_some() {
            return Err(CommandError::DuplicateDef {
                kind: DefKind::Layer,
                slug,
            });
        }
        let src = self.layers[i].clone();
        let mut def = src.clone();
        def.slug = slug.clone();
        let s = layer.as_str();
        // Copied entities get ids derived from the original's and the new
        // slug (the same every time), skipping any that is taken.
        let mut taken: BTreeSet<Id> = self.entity_ids().into_iter().cloned().collect();
        let mut fresh = |old: &Id| {
            let mut n = 0u32;
            loop {
                let seed = if n == 0 {
                    format!("{old}/{slug}")
                } else {
                    format!("{old}/{slug}/{n}")
                };
                let id = Id::derived(&seed);
                if taken.insert(id.clone()) {
                    return id;
                }
                n += 1;
            }
        };
        let mut content = Vec::new();
        for l in self.levels() {
            let rows = match (l.int_grid(s), l.overrides(s)) {
                (Some(g), _) => Some(g.to_rows(&src).map_err(CommandError::Invalid)?),
                (None, Some(o)) => Some(o.to_rows(&src).map_err(CommandError::Invalid)?),
                (None, None) => None,
            };
            let entities = l.entities_layer(s).map(|e| {
                e.entities
                    .iter()
                    .map(|e| {
                        let mut e = e.clone();
                        e.id = fresh(&e.id);
                        e
                    })
                    .collect()
            });
            let c = LayerContent {
                level: l.slug.clone(),
                rows,
                tiles: l.tiles_layer(s).map(|t| t.tiles.clone()),
                entities,
                state: l.layers.get(&layer).cloned(),
            };
            if c.rows.is_some() || c.tiles.is_some() || c.entities.is_some() || c.state.is_some() {
                content.push(c);
            }
        }
        let rules = self.rules_for(s).map(|r| {
            let mut r = r.clone();
            r.layer = slug.clone();
            r
        });
        self.set_layer_def(slug, def, Some(i + 1), content, rules)
    }
}
