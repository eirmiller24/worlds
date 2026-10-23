//! Derived grids across layers: which auto layer writes which derived grid,
//! what each auto layer reads, and the order auto layers bake in. Shared by
//! validation and the engine (`worlds-rules`), so both agree.
//!
//! Everything here looks at every rule in a rules file, active or not and
//! whatever its group's mode: who writes a grid, and so the bake order,
//! doesn't change when a rule is switched off.

use std::collections::{BTreeSet, VecDeque};
use std::fmt;

use super::rules::RuleEntry;
use super::{DerivedOverrides, IntGridLayer, LayerDef, Level, Project};
use crate::id::Slug;

/// Auto layers that read each other's derived grids, so no bake order
/// exists. `path` goes layer, grid it reads, the layer writing that grid,
/// and so on, back to the first layer: `[a, marks_b, b, marks_a, a]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivedCycle {
    pub path: Vec<Slug>,
}

impl DerivedCycle {
    /// The layers on the cycle, first one first (once each).
    pub fn layers(&self) -> Vec<&Slug> {
        let n = self.path.len().saturating_sub(1);
        self.path[..n].iter().step_by(2).collect()
    }
}

impl fmt::Display for DerivedCycle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<&str> = self.layers().into_iter().map(Slug::as_str).collect();
        let list = match names.as_slice() {
            [] => String::new(),
            [one] => (*one).to_string(),
            [init @ .., last] => format!("{} and {last}", init.join(", ")),
        };
        let path: Vec<&str> = self.path.iter().map(Slug::as_str).collect();
        write!(
            f,
            "auto layers {list} read each other's derived grids: {}",
            path.join(" → ")
        )
    }
}

impl std::error::Error for DerivedCycle {}

impl Project {
    /// The auto layers whose rules file has a `write` naming `grid`, in
    /// `layers.json` order. More than one is a validation problem (one
    /// writer per derived grid).
    pub fn derived_writers(&self, grid: &str) -> Vec<&Slug> {
        self.layers
            .iter()
            .filter(|l| l.is_auto())
            .filter(|l| {
                self.rules_for(l.slug.as_str()).is_some_and(|set| {
                    set.groups
                        .iter()
                        .flat_map(|g| g.inline_rules())
                        .any(|r| r.write().is_some_and(|w| w.grid == grid))
                })
            })
            .map(|l| &l.slug)
            .collect()
    }

    /// The auto layer that writes derived grid `grid`: the first of
    /// [`Project::derived_writers`].
    pub fn derived_writer(&self, grid: &str) -> Option<&Slug> {
        self.derived_writers(grid).into_iter().next()
    }

    /// The grids auto layer `layer`'s rules write (`write.grid`), sorted.
    pub fn layer_writes(&self, layer: &str) -> Vec<Slug> {
        let set: BTreeSet<Slug> = self
            .rules_for(layer)
            .into_iter()
            .flat_map(|s| s.groups.iter().flat_map(|g| g.inline_rules()))
            .filter_map(|r| r.write().map(|w| w.grid.clone()))
            .collect();
        set.into_iter().collect()
    }

    /// Every int_grid layer auto layer `layer` reads: its `source`, every
    /// pattern's `grid` (`match` and `require`, in its own rules and in the
    /// templates they use) and every region rule's `region.grid` (each
    /// defaulting to the source). Sorted, once each; empty for a layer that
    /// isn't auto.
    pub fn layer_reads(&self, layer: &str) -> Vec<Slug> {
        let Some(source) = self.layer(layer).and_then(|l| l.auto()).map(|a| &a.source) else {
            return Vec::new();
        };
        let mut out = BTreeSet::from([source.clone()]);
        for (_, entry) in self.rules_for(layer).into_iter().flat_map(|s| s.entries()) {
            match entry {
                RuleEntry::Rule(r) => {
                    out.extend(r.grids_read(source).into_iter().cloned());
                }
                RuleEntry::Use(u) => {
                    if let Some(t) = self.template(u.template.as_str()) {
                        for r in &t.rules {
                            out.extend(r.patterns().map(|p| p.grid_or(source).clone()));
                        }
                    }
                }
            }
        }
        out.into_iter().collect()
    }

    /// The **effective** grid of derived layer `def` in `level`: the
    /// level's override where there is one (a suppressed cell is empty),
    /// else `computed`'s value (what the writer's rules computed; empty
    /// where it is `None`). Sized in cells of the layer's grid. What the
    /// engine, the export and the editor all read.
    pub fn derived_effective(
        &self,
        level: &Level,
        def: &LayerDef,
        computed: Option<&IntGridLayer>,
    ) -> IntGridLayer {
        DerivedOverrides::effective_of(
            level.overrides(def.slug.as_str()),
            def.slug.clone(),
            self.layer_cells(def, level),
            computed,
        )
    }

    /// `layers` and every auto layer that reads (through
    /// [`Project::layer_dependencies`]) a derived grid one of them writes,
    /// transitively. The bake caches use it to find what a definition
    /// change reaches.
    pub fn with_dependents(&self, mut layers: BTreeSet<Slug>) -> BTreeSet<Slug> {
        if layers.is_empty() {
            return layers;
        }
        let deps: Vec<(&Slug, Vec<Slug>)> = self
            .layers
            .iter()
            .filter(|l| l.is_auto())
            .map(|l| {
                let ws = self
                    .layer_dependencies(l.slug.as_str())
                    .into_iter()
                    .map(|(_, w)| w)
                    .collect();
                (&l.slug, ws)
            })
            .collect();
        loop {
            let before = layers.len();
            for (l, ws) in &deps {
                if !layers.contains(*l) && ws.iter().any(|w| layers.contains(w)) {
                    layers.insert((*l).clone());
                }
            }
            if layers.len() == before {
                return layers;
            }
        }
    }

    /// What auto layer `layer` depends on: `(grid, writer)` for each derived
    /// grid it reads ([`Project::layer_reads`]) that another auto layer
    /// writes ([`Project::derived_writer`]). In grid order.
    pub fn layer_dependencies(&self, layer: &str) -> Vec<(Slug, Slug)> {
        self.layer_reads(layer)
            .into_iter()
            .filter(|g| self.layer(g.as_str()).is_some_and(|d| d.is_derived()))
            .filter_map(|g| {
                let w = self.derived_writer(g.as_str())?.clone();
                (w != layer).then_some((g, w))
            })
            .collect()
    }

    /// Every auto layer, in the order they bake: a layer after the writers
    /// of the derived grids it reads, ties in `layers.json` order (so
    /// without derived grids it is just `layers.json` order). A cycle is an
    /// error naming it, starting at its first layer in `layers.json` order.
    pub fn auto_layer_order(&self) -> Result<Vec<Slug>, DerivedCycle> {
        let auto: Vec<&Slug> = self
            .layers
            .iter()
            .filter(|l| l.is_auto())
            .map(|l| &l.slug)
            .collect();
        let index = |s: &Slug| auto.iter().position(|a| *a == s);
        // deps[i]: (grid, index of the writer) for each dependency of auto[i].
        let deps: Vec<Vec<(Slug, usize)>> = auto
            .iter()
            .map(|l| {
                self.layer_dependencies(l.as_str())
                    .into_iter()
                    .filter_map(|(g, w)| Some((g, index(&w)?)))
                    .collect()
            })
            .collect();
        let mut done = vec![false; auto.len()];
        let mut order = Vec::with_capacity(auto.len());
        while order.len() < auto.len() {
            let next = (0..auto.len()).find(|&i| !done[i] && deps[i].iter().all(|&(_, w)| done[w]));
            match next {
                Some(i) => {
                    done[i] = true;
                    order.push(auto[i].clone());
                }
                None => return Err(find_cycle(&auto, &deps, &done)),
            }
        }
        Ok(order)
    }
}

/// The shortest cycle through the first (in `layers.json` order) layer
/// that is on one, among the layers not yet ordered.
fn find_cycle(auto: &[&Slug], deps: &[Vec<(Slug, usize)>], done: &[bool]) -> DerivedCycle {
    for start in (0..auto.len()).filter(|&i| !done[i]) {
        // BFS from start's dependencies back to start.
        let mut prev: Vec<Option<(usize, Slug)>> = vec![None; auto.len()];
        let mut seen = vec![false; auto.len()];
        let mut queue = VecDeque::new();
        let mut found: Option<(usize, Slug)> = None;
        queue.push_back(start);
        seen[start] = true;
        'search: while let Some(at) = queue.pop_front() {
            for (g, w) in &deps[at] {
                if done[*w] {
                    continue;
                }
                if *w == start {
                    found = Some((at, g.clone()));
                    break 'search;
                }
                if !seen[*w] {
                    seen[*w] = true;
                    prev[*w] = Some((at, g.clone()));
                    queue.push_back(*w);
                }
            }
        }
        if let Some((last, grid)) = found {
            // Walk back from `last` to `start`: start → ... → last → grid → start.
            let mut rev = vec![auto[start].clone(), grid, auto[last].clone()];
            let mut at = last;
            while at != start {
                let (p, g) = prev[at].clone().expect("reached from start");
                rev.push(g);
                rev.push(auto[p].clone());
                at = p;
            }
            rev.reverse();
            return DerivedCycle { path: rev };
        }
    }
    unreachable!("a layer that can't be ordered is on a cycle or depends on one")
}
