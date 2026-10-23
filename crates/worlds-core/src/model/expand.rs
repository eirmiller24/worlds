//! Template expansion: what a rule set actually runs.
//!
//! [`Project::expanded_rules`] is the one place that turns a rule file's
//! entries (inline rules and `{ "use": ... }` references) into plain rules.
//! Validation, the baker, the cache, the server and the editor all go
//! through it, so they agree on slugs, legends and tiles.

use std::borrow::Cow;
use std::collections::BTreeMap;

use super::Project;
use super::rules::{
    CellMatch, Legend, Macros, MatchSelect, Output, Pattern, Rows, Rule, RuleEntry, RuleGroup,
    RuleOutput, Select, Template, TemplateOutput, TemplateRef, TemplateRule, TileAlt, TilesOutput,
};
use crate::id::Slug;

/// One group of a rule set with its entries expanded, in file order.
#[derive(Debug, Clone, PartialEq)]
pub struct ExpandedGroup<'p> {
    /// The group as written (slug, mode, `requires_level_field`, ...).
    pub group: &'p RuleGroup,
    /// Its rules in evaluation order: inline rules as they are, each
    /// reference replaced by its template's rules.
    pub rules: Vec<ExpandedRule<'p>>,
}

/// One rule as the engine runs it: rows and legends inline (no names left),
/// tiles and tileset final (before a level's tileset substitutions).
#[derive(Debug, Clone, PartialEq)]
pub struct ExpandedRule<'p> {
    /// Borrowed for an inline rule with nothing to resolve, owned otherwise.
    /// For a template rule the slug is `<reference slug>__<template rule slug>`.
    pub rule: Cow<'p, Rule>,
    /// The reference this rule came from (its slug), `None` for an inline rule.
    pub from: Option<Slug>,
    /// Index of the on-disk entry in the group's `rules`.
    pub entry: usize,
}

/// Why a rule set can't be expanded. Also reported by `validate`; never a
/// load error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExpandError {
    #[error("rule {rule}: no pattern {name:?} in rules/macros.json patterns")]
    UnknownPattern { rule: Slug, name: Slug },
    #[error("rule {rule}: no legend {name:?} in rules/macros.json legends")]
    UnknownLegend { rule: Slug, name: Slug },
    #[error(
        "reference {reference}: no template {template:?} (not in rules/macros.json templates, \
         not a built-in)"
    )]
    UnknownTemplate { reference: Slug, template: Slug },
    #[error(
        "reference {reference}: template rule {rule} draws from the reference's tileset, but the \
         reference names no \"tileset\""
    )]
    NoTileset { reference: Slug, rule: Slug },
}

impl Project {
    /// The template `use: name` means (a built-in first, then
    /// `rules/macros.json`).
    pub fn template(&self, name: &str) -> Option<&Template> {
        Macros::template(self.macros.as_ref(), name)
    }

    /// The rules of auto layer `layer` as they run: every group in file
    /// order, each reference expanded in place, named rows and legends
    /// resolved. A layer without a rule set has no groups.
    ///
    /// A template rule `r` of reference `u` becomes a rule with
    /// - slug `<u.slug>__<r.slug>`;
    /// - rows resolved; legend = template `legend`, then `r`'s own, then
    ///   `u.legend` (later entries win), for `match` and `require` alike;
    /// - `out.tileset` = `u.tileset` and tiles (`u.tiles[r.slug]`, else
    ///   `r`'s) shifted by `u.origin`, unless `r` names its own tileset
    ///   (fixed: its tileset, tiles absolute, overrides absolute);
    /// - `active` = `r.active && u.active`; everything else as in `r`.
    ///
    /// Rules in `u.skip` are left out.
    pub fn expanded_rules(&self, layer: &str) -> Result<Vec<ExpandedGroup<'_>>, ExpandError> {
        let Some(set) = self.rules_for(layer) else {
            return Ok(Vec::new());
        };
        set.groups
            .iter()
            .map(|group| {
                let mut rules = Vec::new();
                for (i, entry) in group.rules.iter().enumerate() {
                    rules.extend(self.expand_entry(entry, i)?);
                }
                Ok(ExpandedGroup { group, rules })
            })
            .collect()
    }

    /// One entry of a group expanded (see [`Project::expanded_rules`]);
    /// `index` is its position in the group, recorded in each result.
    pub fn expand_entry<'p>(
        &'p self,
        entry: &'p RuleEntry,
        index: usize,
    ) -> Result<Vec<ExpandedRule<'p>>, ExpandError> {
        match entry {
            RuleEntry::Rule(r) => Ok(vec![ExpandedRule {
                rule: self.resolve_rule(r)?,
                from: None,
                entry: index,
            }]),
            RuleEntry::Use(u) => {
                let t = self.template(u.template.as_str()).ok_or_else(|| {
                    ExpandError::UnknownTemplate {
                        reference: u.slug().clone(),
                        template: u.template.clone(),
                    }
                })?;
                t.rules
                    .iter()
                    .filter(|r| !u.skip.contains(&r.slug))
                    .map(|r| {
                        Ok(ExpandedRule {
                            rule: Cow::Owned(self.expand_template_rule(u, t, r)?),
                            from: Some(u.slug().clone()),
                            entry: index,
                        })
                    })
                    .collect()
            }
        }
    }

    /// An inline rule with its named rows and legends resolved (borrowed
    /// when there is nothing to resolve).
    pub fn resolve_rule<'p>(&self, r: &'p Rule) -> Result<Cow<'p, Rule>, ExpandError> {
        if !r.patterns().any(Pattern::is_named) {
            return Ok(Cow::Borrowed(r));
        }
        let mut out = r.clone();
        for p in out.patterns_mut() {
            *p = self.resolve_pattern(p, &r.slug, &[])?;
        }
        Ok(Cow::Owned(out))
    }

    /// The named rows of `macros.json` `patterns`.
    pub fn named_pattern(&self, name: &str) -> Option<&Vec<String>> {
        self.macros.as_ref()?.patterns.get(name)
    }

    /// The named legend of `macros.json` `legends`.
    pub fn named_legend(&self, name: &str) -> Option<&BTreeMap<char, CellMatch>> {
        self.macros.as_ref()?.legends.get(name)
    }

    /// A legend as a map, resolving a name. `rule` is for the error.
    pub fn resolve_legend(
        &self,
        legend: &Legend,
        rule: &Slug,
    ) -> Result<BTreeMap<char, CellMatch>, ExpandError> {
        match legend {
            Legend::Inline(m) => Ok(m.clone()),
            Legend::Named(n) => {
                self.named_legend(n.as_str())
                    .cloned()
                    .ok_or_else(|| ExpandError::UnknownLegend {
                        rule: rule.clone(),
                        name: n.clone(),
                    })
            }
        }
    }

    /// `p` with inline rows and legend. `under` are legends merged in
    /// order *under* the pattern's own (earlier = weaker); used for a
    /// template's legend. Nothing is merged over it here.
    fn resolve_pattern(
        &self,
        p: &Pattern,
        rule: &Slug,
        under: &[&BTreeMap<char, CellMatch>],
    ) -> Result<Pattern, ExpandError> {
        let rows = match &p.rows {
            Rows::Inline(r) => r.clone(),
            Rows::Named(n) => self.named_pattern(n.as_str()).cloned().ok_or_else(|| {
                ExpandError::UnknownPattern {
                    rule: rule.clone(),
                    name: n.clone(),
                }
            })?,
        };
        let mut legend = BTreeMap::new();
        for l in under {
            legend.extend(l.iter().map(|(k, v)| (*k, v.clone())));
        }
        legend.extend(self.resolve_legend(&p.legend, rule)?);
        Ok(Pattern {
            grid: p.grid.clone(),
            legend: Legend::Inline(legend),
            rows: Rows::Inline(rows),
        })
    }

    fn expand_template_rule(
        &self,
        u: &TemplateRef,
        t: &Template,
        r: &TemplateRule,
    ) -> Result<Rule, ExpandError> {
        let slug = Slug::new(format!("{}__{}", u.slug(), r.slug))
            .expect("two slugs joined by __ are a slug");
        let base = self.resolve_legend(&t.legend, &slug)?;
        let pattern_for = |p: &Pattern| -> Result<Pattern, ExpandError> {
            let mut p = self.resolve_pattern(p, &slug, &[&base])?;
            if let Legend::Inline(m) = &mut p.legend {
                m.extend(u.legend.iter().map(|(k, v)| (*k, v.clone())));
            }
            Ok(p)
        };
        let pattern = pattern_for(&r.pattern)?;
        let require = r
            .require
            .iter()
            .map(pattern_for)
            .collect::<Result<Vec<_>, _>>()?;
        let out = match &r.out {
            TemplateOutput::Stamp(s) => RuleOutput::Stamp(s.clone()),
            TemplateOutput::Tiles(o) => {
                let given = u.tiles.get(&r.slug).unwrap_or(&o.tiles);
                let (tileset, tiles) = match &o.tileset {
                    Some(ts) => (ts.clone(), given.clone()),
                    None => {
                        let ts = u.tileset.clone().ok_or_else(|| ExpandError::NoTileset {
                            reference: u.slug().clone(),
                            rule: r.slug.clone(),
                        })?;
                        (ts, given.iter().map(|a| shift(a, u.origin)).collect())
                    }
                };
                RuleOutput::Tiles(TilesOutput {
                    tileset,
                    tiles,
                    tile_mode: o.tile_mode,
                    pivot: o.pivot,
                    avoid_overlap: o.avoid_overlap,
                    draw: o.draw.clone(),
                })
            }
        };
        Ok(Rule {
            slug,
            active: r.active && u.active,
            select: Select::Match(MatchSelect {
                pattern,
                require,
                symmetry: r.symmetry.clone(),
                chance: r.chance,
                modulo: r.modulo.clone(),
                checker: r.checker,
                perlin: r.perlin.clone(),
                out_of_bounds: r.out_of_bounds.clone(),
            }),
            output: Output::Draw(out),
            break_on_match: r.break_on_match,
            salt: r.salt,
        })
    }

    /// The auto layers whose rules can draw from tileset `slug`: an inline
    /// or template rule output, a stamp, a template reference's tileset, or
    /// a level's tileset substitution for the layer (either side). Sorted.
    pub fn auto_layers_drawing_from(&self, slug: &str) -> Vec<Slug> {
        let mut out = Vec::new();
        for set in &self.rules {
            let layer = set.layer.as_str();
            let hit = set.stamps.iter().any(|s| s.tileset == slug)
                || set.entries().any(|(_, e)| match e {
                    RuleEntry::Rule(r) => r
                        .out()
                        .and_then(RuleOutput::as_tiles)
                        .is_some_and(|t| t.tileset == slug),
                    RuleEntry::Use(u) => {
                        u.tileset.as_ref().is_some_and(|t| t == slug)
                            || self.template(u.template.as_str()).is_some_and(|t| {
                                t.rules.iter().any(|r| match &r.out {
                                    TemplateOutput::Tiles(o) => {
                                        o.tileset.as_ref().is_some_and(|t| t == slug)
                                    }
                                    TemplateOutput::Stamp(_) => false,
                                })
                            })
                    }
                })
                || self.levels().any(|l| {
                    l.layers
                        .get(layer)
                        .is_some_and(|st| st.tilesets.iter().any(|(a, b)| a == slug || b == slug))
                });
            if hit {
                out.push(set.layer.clone());
            }
        }
        out.sort();
        out
    }
}

/// `a` moved by `origin` (every tile, so rectangles stay rectangles).
fn shift(a: &TileAlt, origin: [u32; 2]) -> TileAlt {
    TileAlt {
        tiles: a
            .tiles
            .iter()
            .map(|t| {
                [
                    t[0].saturating_add(origin[0]),
                    t[1].saturating_add(origin[1]),
                ]
            })
            .collect(),
        weight: a.weight,
    }
}
