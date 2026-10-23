//! Rule-level, macro and stamp commands: `set_rule`, `remove_rule`,
//! `move_rule`, `set_macro`, `remove_macro`, `rename_macro`, `set_stamp`
//! and `remove_stamp`.
//!
//! They follow the definition commands' rules (see [`super::defs`]): a set
//! creates or replaces, a remove refuses while something uses what it
//! removes (unless `keep_uses`, which is what inverses of a creation over
//! dangling uses set), and a rename rewrites every reference and is refused
//! while the new name is already referred to. Only structure is checked:
//! an unknown template, pattern or legend named by a rule is a validation
//! problem, never a refusal.

use super::defs::{ListEdit, dangling, list_set, slug_mismatch};
use super::{Result, noop};
use crate::command::{
    Applied, Change, Command, CommandError, DefKind, MacroDef, MacroKind, coalesce,
};
use crate::id::Slug;
use crate::model::{
    LayerDef, Legend, Macros, Pattern, Project, Rows, RuleEntry, RuleOutput, RuleSet, Stamp,
    TemplateOutput,
};

/// How many uses an `in_use` error lists before "N more".
const SHOWN: usize = 5;

fn rules_change(layer: &Slug) -> Change {
    Change::Def {
        kind: DefKind::Rules,
        slug: layer.clone(),
    }
}

fn macros_change() -> Change {
    Change::Def {
        kind: DefKind::Macros,
        slug: crate::id::slug("macros"),
    }
}

/// Keep the first [`SHOWN`] distinct uses, then count the rest.
#[derive(Default)]
struct Uses {
    shown: Vec<String>,
    more: usize,
}

impl Uses {
    fn add(&mut self, what: String) {
        if self.shown.contains(&what) {
            return;
        }
        if self.shown.len() < SHOWN {
            self.shown.push(what);
        } else {
            self.more += 1;
        }
    }

    fn finish(mut self) -> Vec<String> {
        if self.more > 0 {
            self.shown.push(format!("{} more", self.more));
        }
        self.shown
    }
}

/// Where a macro name is referred to.
enum MacroSite {
    /// An inline rule's `match` or `require` (`rows` or `legend`).
    Rule { layer: Slug, rule: Slug },
    /// A `{ "use": ... }` entry.
    Reference { layer: Slug, reference: Slug },
    /// A template in `rules/macros.json` (its `legend`, or a rule's pattern).
    Template(Slug),
}

impl MacroSite {
    fn change(&self) -> Change {
        match self {
            MacroSite::Rule { layer, .. } | MacroSite::Reference { layer, .. } => {
                rules_change(layer)
            }
            MacroSite::Template(_) => macros_change(),
        }
    }

    fn describe(&self) -> String {
        match self {
            MacroSite::Rule { layer, rule } => format!("rule {rule} in rules/{layer}.json"),
            MacroSite::Reference { layer, reference } => {
                format!("reference {reference} in rules/{layer}.json")
            }
            MacroSite::Template(name) => format!("template {name}"),
        }
    }
}

/// Refuse an edit of `set` while two of its groups share a slug (a
/// validation problem the loader accepts): a group is found by its slug, so
/// which one is meant, and where an inverse puts things back, would be a
/// guess.
pub(super) fn unique_groups(set: &RuleSet) -> Result<()> {
    unique(
        set.groups.iter().map(|g| &g.slug),
        &format!("groups of rules/{}.json", set.layer),
    )
}

/// The same for stamps, which are found by slug too.
fn unique_stamps(set: &RuleSet) -> Result<()> {
    unique(
        set.stamps.iter().map(|s| &s.slug),
        &format!("stamps of rules/{}.json", set.layer),
    )
}

fn unique<'a>(slugs: impl Iterator<Item = &'a Slug>, what: &str) -> Result<()> {
    let mut seen = std::collections::BTreeSet::new();
    for s in slugs {
        if !seen.insert(s) {
            return Err(CommandError::Invalid(format!(
                "several {what} are slugged {s:?}; which one is meant would be a guess, so the \
                 file must be fixed by hand first (validation reports it)"
            )));
        }
    }
    Ok(())
}

/// A template name only the built-ins may have.
fn check_builtin(kind: MacroKind, name: &Slug) -> Result<()> {
    if kind == MacroKind::Template && Macros::is_builtin(name.as_str()) {
        return Err(CommandError::Invalid(format!(
            "{name:?} is a built-in template; it can't be defined, removed or renamed in \
             rules/macros.json (copy it under another name to change it)"
        )));
    }
    Ok(())
}

fn macros_empty(m: &Macros) -> bool {
    m.patterns.is_empty() && m.legends.is_empty() && m.templates.is_empty()
}

fn get_macro(m: Option<&Macros>, kind: MacroKind, name: &Slug) -> Option<MacroDef> {
    let m = m?;
    match kind {
        MacroKind::Pattern => m.patterns.get(name).cloned().map(MacroDef::Pattern),
        MacroKind::Legend => m.legends.get(name).cloned().map(MacroDef::Legend),
        MacroKind::Template => m.templates.get(name).cloned().map(MacroDef::Template),
    }
}

/// Take a macro out of the file, if it is there.
fn take_macro(m: &mut Macros, kind: MacroKind, name: &Slug) -> Option<MacroDef> {
    match kind {
        MacroKind::Pattern => m.patterns.remove(name).map(MacroDef::Pattern),
        MacroKind::Legend => m.legends.remove(name).map(MacroDef::Legend),
        MacroKind::Template => m.templates.remove(name).map(MacroDef::Template),
    }
}

fn put_macro(m: &mut Macros, name: Slug, def: MacroDef) {
    match def {
        MacroDef::Pattern(r) => {
            m.patterns.insert(name, r);
        }
        MacroDef::Legend(l) => {
            m.legends.insert(name, l);
        }
        MacroDef::Template(t) => {
            m.templates.insert(name, t);
        }
    }
}

/// Call `f` on the name in a pattern's `rows` or `legend` (per `kind`).
fn pattern_slot(
    p: &mut Pattern,
    kind: MacroKind,
    site: &MacroSite,
    f: &mut dyn FnMut(&MacroSite, &Slug) -> Option<Slug>,
) {
    let name = match (kind, &mut p.rows, &mut p.legend) {
        (MacroKind::Pattern, Rows::Named(n), _) => n,
        (MacroKind::Legend, _, Legend::Named(n)) => n,
        _ => return,
    };
    if let Some(new) = f(site, name) {
        *name = new;
    }
}

impl Project {
    // ---- rules ----

    /// Index of `layer`'s rule set; an unknown layer is `UnknownLayer`,
    /// a layer without rules is `missing`.
    fn rule_set_index(&self, layer: &Slug, missing: CommandError) -> Result<usize> {
        match self.rules.iter().position(|r| r.layer == *layer) {
            Some(k) => Ok(k),
            None if self.layer(layer.as_str()).is_none() => {
                Err(CommandError::UnknownLayer(layer.clone()))
            }
            None => Err(missing),
        }
    }

    /// `(group index, entry index)` of the one entry slugged `rule` in rule
    /// set `k`. Refused when the slug is on several entries (a validation
    /// problem): which one is meant, and where undo puts it back, would be
    /// a guess.
    fn find_entry(&self, k: usize, rule: &Slug) -> Result<(usize, usize)> {
        let set = &self.rules[k];
        unique_groups(set)?;
        let hits: Vec<(usize, usize)> = set
            .groups
            .iter()
            .enumerate()
            .flat_map(|(gi, g)| {
                g.rules
                    .iter()
                    .enumerate()
                    .filter(|(_, e)| e.slug() == rule)
                    .map(move |(ei, _)| (gi, ei))
            })
            .collect();
        match hits.as_slice() {
            [one] => Ok(*one),
            [] => Err(match self.expanded_from(set, rule) {
                Some(why) => CommandError::Invalid(why),
                None => CommandError::UnknownRule {
                    layer: set.layer.clone(),
                    rule: rule.clone(),
                },
            }),
            many => {
                let groups: Vec<&str> = many
                    .iter()
                    .map(|(gi, _)| set.groups[*gi].slug.as_str())
                    .collect();
                Err(CommandError::Invalid(format!(
                    "{} entries of rules/{}.json are slugged {rule:?} (in groups {}); give them \
                     distinct slugs first (set_rule_group)",
                    many.len(),
                    set.layer,
                    groups.join(", ")
                )))
            }
        }
    }

    /// If `rule` is a slug a reference in `set` expands to, why it can't be
    /// edited on its own.
    fn expanded_from(&self, set: &RuleSet, rule: &Slug) -> Option<String> {
        set.entries().find_map(|(_, e)| {
            let u = e.as_use()?;
            let rest = rule.as_str().strip_prefix(u.slug().as_str())?;
            let rest = rest.strip_prefix("__")?;
            let t = self.template(u.template.as_str())?;
            t.rules.iter().any(|r| r.slug == rest).then(|| {
                format!(
                    "rule {rule:?} comes from template reference {:?} (template {:?}) in \
                     rules/{}.json; add {rest:?} to that reference's \"skip\" (set_rule on {:?}) \
                     instead",
                    u.slug(),
                    u.template,
                    set.layer,
                    u.slug()
                )
            })
        })
    }

    pub(super) fn set_rule(
        &mut self,
        layer: Slug,
        group: Slug,
        rule: Slug,
        def: RuleEntry,
        index: Option<usize>,
    ) -> Result<Applied> {
        if *def.slug() != rule {
            return Err(slug_mismatch("set_rule", def.slug(), &rule));
        }
        let unknown_group = || CommandError::UnknownRuleGroup {
            layer: layer.clone(),
            group: group.clone(),
        };
        let k = self.rule_set_index(&layer, unknown_group())?;
        let set = &self.rules[k];
        unique_groups(set)?;
        let gi = set
            .groups
            .iter()
            .position(|g| g.slug == group)
            .ok_or_else(unknown_group)?;
        if let Some(other) = set
            .groups
            .iter()
            .enumerate()
            .find(|(i, g)| *i != gi && g.rules.iter().any(|e| *e.slug() == rule))
        {
            return Err(CommandError::Invalid(format!(
                "rules/{layer}.json already has {rule:?} in group {}; use move_rule to move it \
                 to group {group}, or set_rule on group {} to change it",
                other.1.slug, other.1.slug
            )));
        }
        let same = set.groups[gi]
            .rules
            .iter()
            .filter(|e| *e.slug() == rule)
            .count();
        if same > 1 {
            // Replacing "the" entry would be a guess, and so would its undo.
            return Err(CommandError::Invalid(format!(
                "{same} entries of group {group} of rules/{layer}.json are slugged {rule:?}; give \
                 them distinct slugs first (set_rule_group)"
            )));
        }
        let what = format!("group {group} of rules/{layer}.json");
        let g = &mut self.rules[k].groups[gi];
        let pos = g.rules.iter().position(|e| *e.slug() == rule);
        let inverse = match list_set(&mut g.rules, pos, def, index, &what)? {
            ListEdit::Noop => return Ok(noop()),
            ListEdit::Created => Command::RemoveRule {
                layer: layer.clone(),
                rule,
            },
            ListEdit::Replaced { old, moved_from } => Command::SetRule {
                layer: layer.clone(),
                group,
                rule,
                def: old,
                index: moved_from,
            },
        };
        Ok(Applied {
            inverse,
            changes: vec![rules_change(&layer)],
        })
    }

    pub(super) fn remove_rule(&mut self, layer: Slug, rule: Slug) -> Result<Applied> {
        let unknown = CommandError::UnknownRule {
            layer: layer.clone(),
            rule: rule.clone(),
        };
        let k = self.rule_set_index(&layer, unknown)?;
        let (gi, ei) = self.find_entry(k, &rule)?;
        let g = &mut self.rules[k].groups[gi];
        let def = g.rules.remove(ei);
        Ok(Applied {
            inverse: Command::SetRule {
                layer: layer.clone(),
                group: g.slug.clone(),
                rule,
                def,
                index: Some(ei),
            },
            changes: vec![rules_change(&layer)],
        })
    }

    pub(super) fn move_rule(
        &mut self,
        layer: Slug,
        rule: Slug,
        group: Slug,
        index: usize,
    ) -> Result<Applied> {
        let unknown = CommandError::UnknownRule {
            layer: layer.clone(),
            rule: rule.clone(),
        };
        let k = self.rule_set_index(&layer, unknown)?;
        let (gi, ei) = self.find_entry(k, &rule)?;
        let set = &mut self.rules[k];
        let to = set
            .groups
            .iter()
            .position(|g| g.slug == group)
            .ok_or_else(|| CommandError::UnknownRuleGroup {
                layer: layer.clone(),
                group: group.clone(),
            })?;
        let room = set.groups[to].rules.len() - usize::from(to == gi);
        if index > room {
            return Err(CommandError::Invalid(format!(
                "index {index} is past the end of group {group} of rules/{layer}.json ({room} \
                 entries besides {rule:?})"
            )));
        }
        if to == gi && index == ei {
            return Ok(noop());
        }
        let entry = set.groups[gi].rules.remove(ei);
        set.groups[to].rules.insert(index, entry);
        Ok(Applied {
            inverse: Command::MoveRule {
                layer: layer.clone(),
                rule,
                group: set.groups[gi].slug.clone(),
                index: ei,
            },
            changes: vec![rules_change(&layer)],
        })
    }

    // ---- macros ----

    /// Call `f` on every reference to a macro of `kind` (inline rules'
    /// `match`/`require` rows or legends, references' `use`, templates'
    /// legends and their rules' patterns), with where it is. When `f`
    /// returns a name, the reference becomes that name.
    fn visit_macro_refs(
        &mut self,
        kind: MacroKind,
        f: &mut dyn FnMut(&MacroSite, &Slug) -> Option<Slug>,
    ) {
        let Project { rules, macros, .. } = self;
        for set in rules.iter_mut() {
            let layer = set.layer.clone();
            for e in set.groups.iter_mut().flat_map(|g| &mut g.rules) {
                match e {
                    RuleEntry::Rule(r) => {
                        let site = MacroSite::Rule {
                            layer: layer.clone(),
                            rule: r.slug.clone(),
                        };
                        for p in r.patterns_mut() {
                            pattern_slot(p, kind, &site, f);
                        }
                    }
                    RuleEntry::Use(u) if kind == MacroKind::Template => {
                        let site = MacroSite::Reference {
                            layer: layer.clone(),
                            reference: u.slug().clone(),
                        };
                        if let Some(new) = f(&site, &u.template) {
                            u.template = new;
                        }
                    }
                    RuleEntry::Use(_) => {}
                }
            }
        }
        if let Some(m) = macros {
            for (name, t) in m.templates.iter_mut() {
                let site = MacroSite::Template(name.clone());
                if kind == MacroKind::Legend
                    && let Legend::Named(n) = &mut t.legend
                    && let Some(new) = f(&site, n)
                {
                    *n = new;
                }
                for r in &mut t.rules {
                    for p in r.patterns_mut() {
                        pattern_slot(p, kind, &site, f);
                    }
                }
            }
        }
    }

    /// Where macro `name` of `kind` is referred to, the first few.
    fn macro_uses(&mut self, kind: MacroKind, name: &Slug) -> Vec<String> {
        let mut uses = Uses::default();
        self.visit_macro_refs(kind, &mut |site, n| {
            if n == name {
                uses.add(site.describe());
            }
            None
        });
        uses.finish()
    }

    pub(super) fn set_macro(
        &mut self,
        kind: MacroKind,
        name: Slug,
        def: MacroDef,
    ) -> Result<Applied> {
        if def.kind() != kind {
            return Err(CommandError::Invalid(format!(
                "set_macro: kind is {kind} but def is a {} (a pattern is an array of rows, a \
                 legend an object of glyph to cell test, a template an object with \"rules\")",
                def.kind()
            )));
        }
        check_builtin(kind, &name)?;
        let old = get_macro(self.macros.as_ref(), kind, &name);
        if old.as_ref() == Some(&def) {
            return Ok(noop());
        }
        let inverse = match old {
            Some(old) => Command::SetMacro {
                kind,
                name: name.clone(),
                def: old,
            },
            // Uses that already name it (dangling until now) stay when the
            // creation is undone, and so does an empty macros file.
            None => Command::RemoveMacro {
                kind,
                keep_uses: !self.macro_uses(kind, &name).is_empty(),
                keep_empty: self.macros.as_ref().is_some_and(macros_empty),
                name: name.clone(),
            },
        };
        put_macro(self.macros.get_or_insert_with(Macros::default), name, def);
        Ok(Applied {
            inverse,
            changes: vec![macros_change()],
        })
    }

    pub(super) fn remove_macro(
        &mut self,
        kind: MacroKind,
        name: Slug,
        keep_uses: bool,
        keep_empty: bool,
    ) -> Result<Applied> {
        check_builtin(kind, &name)?;
        if get_macro(self.macros.as_ref(), kind, &name).is_none() {
            return Err(CommandError::UnknownMacro { kind, name });
        }
        let uses = self.macro_uses(kind, &name);
        if !uses.is_empty() && !keep_uses {
            return Err(CommandError::InUse {
                kind: DefKind::Macros,
                slug: name,
                uses,
            });
        }
        let m = self.macros.as_mut().expect("checked above");
        let def = take_macro(m, kind, &name).expect("checked above");
        if macros_empty(m) && !keep_empty {
            self.macros = None;
        }
        Ok(Applied {
            inverse: Command::SetMacro { kind, name, def },
            changes: vec![macros_change()],
        })
    }

    pub(super) fn rename_macro(
        &mut self,
        kind: MacroKind,
        from: Slug,
        to: Slug,
    ) -> Result<Applied> {
        check_builtin(kind, &from)?;
        check_builtin(kind, &to)?;
        if get_macro(self.macros.as_ref(), kind, &from).is_none() {
            return Err(CommandError::UnknownMacro { kind, name: from });
        }
        if from == to {
            return Ok(noop());
        }
        if get_macro(self.macros.as_ref(), kind, &to).is_some() {
            return Err(CommandError::DuplicateDef {
                kind: DefKind::Macros,
                slug: to,
            });
        }
        if let Some(what) = self.macro_uses(kind, &to).into_iter().next() {
            return Err(dangling(DefKind::Macros, &to, &what));
        }
        // A reference with no slug of its own is named by its template; it
        // keeps that name (`slug` pinned to the old one), so its expanded
        // slugs, and so their random rolls, don't change.
        let pins = if kind == MacroKind::Template {
            self.slugless_uses(&from)?
        } else {
            Vec::new()
        };
        let mut changes: Vec<Change> = pins.iter().map(|(k, ..)| rules_change(k)).collect();
        let mut unpin = Vec::new();
        for (layer, gi, ei) in pins {
            let k = self.rules.iter().position(|r| r.layer == layer);
            let set = &mut self.rules[k.expect("found above")];
            let group = set.groups[gi].slug.clone();
            let e = &mut set.groups[gi].rules[ei];
            let old = e.clone();
            if let RuleEntry::Use(u) = e {
                u.slug = Some(from.clone());
            }
            unpin.push(Command::SetRule {
                layer,
                group,
                rule: from.clone(),
                def: old,
                index: None,
            });
        }
        let m = self.macros.as_mut().expect("checked above");
        let def = take_macro(m, kind, &from).expect("checked above");
        put_macro(m, to.clone(), def);
        changes.push(macros_change());
        self.visit_macro_refs(kind, &mut |site, n| {
            (*n == from).then(|| {
                changes.push(site.change());
                to.clone()
            })
        });
        let back = Command::RenameMacro {
            kind,
            from: to,
            to: from,
        };
        // Undo renames back, then takes the pinned slugs out again (last
        // pinned first, so a redo pins in the same order).
        let inverse = if unpin.is_empty() {
            back
        } else {
            unpin.reverse();
            Command::Batch {
                label: String::new(),
                commands: std::iter::once(back).chain(unpin).collect(),
            }
        };
        Ok(Applied {
            inverse,
            changes: coalesce(changes),
        })
    }

    /// `(layer, group index, entry index)` of every reference to template
    /// `name` with no slug of its own, in file order. Refused when one of
    /// them couldn't be found again by its slug (a duplicate group, or
    /// another entry of the layer with the same slug): undo looks it up.
    fn slugless_uses(&self, name: &Slug) -> Result<Vec<(Slug, usize, usize)>> {
        let mut out = Vec::new();
        for (k, set) in self.rules.iter().enumerate() {
            for (gi, g) in set.groups.iter().enumerate() {
                for (ei, e) in g.rules.iter().enumerate() {
                    if let RuleEntry::Use(u) = e
                        && u.template == *name
                        && u.slug.is_none()
                    {
                        let at = self.find_entry(k, name)?;
                        debug_assert_eq!(at, (gi, ei));
                        out.push((set.layer.clone(), gi, ei));
                    }
                }
            }
        }
        Ok(out)
    }

    // ---- stamps ----

    /// Where stamp `stamp` of `layer` is placed: an inline rule's
    /// `out.stamp`, or a rule of a template the layer references.
    fn stamp_uses(&self, layer: &Slug, stamp: &Slug) -> Vec<String> {
        let mut uses = Uses::default();
        let Some(set) = self.rules_for(layer.as_str()) else {
            return Vec::new();
        };
        for (_, e) in set.entries() {
            match e {
                RuleEntry::Rule(r) => {
                    if matches!(r.out(), Some(RuleOutput::Stamp(s)) if s.stamp == *stamp) {
                        uses.add(format!("rule {}", r.slug));
                    }
                }
                RuleEntry::Use(u) => {
                    let Some(t) = self.template(u.template.as_str()) else {
                        continue;
                    };
                    if t.rules
                        .iter()
                        .any(|r| matches!(&r.out, TemplateOutput::Stamp(s) if s.stamp == *stamp))
                    {
                        uses.add(format!("template {} (reference {})", u.template, u.slug()));
                    }
                }
            }
        }
        uses.finish()
    }

    pub(super) fn set_stamp(
        &mut self,
        layer: Slug,
        stamp: Slug,
        def: Stamp,
        index: Option<usize>,
    ) -> Result<Applied> {
        if def.slug != stamp {
            return Err(slug_mismatch("set_stamp", &def.slug, &stamp));
        }
        let changes = vec![rules_change(&layer)];
        let Some(k) = self.rules.iter().position(|r| r.layer == layer) else {
            // A new rule set, for an auto layer.
            self.layer_kind(&layer, "auto", LayerDef::is_auto)?;
            if let Some(i) = index.filter(|&i| i > 0) {
                return Err(CommandError::Invalid(format!(
                    "index {i} is past the end of rules/{layer}.json stamps (0 entries)"
                )));
            }
            let k = self.rules.partition_point(|r| r.layer < layer);
            self.rules.insert(
                k,
                RuleSet {
                    layer: layer.clone(),
                    stamps: vec![def],
                    groups: Vec::new(),
                },
            );
            return Ok(Applied {
                inverse: Command::RemoveStamp {
                    layer,
                    stamp,
                    keep_uses: false,
                    keep_empty: false,
                },
                changes,
            });
        };
        // Uses that already name it (dangling until now) stay when the
        // creation is undone.
        unique_stamps(&self.rules[k])?;
        let keep_uses = !self.stamp_uses(&layer, &stamp).is_empty();
        let set = &mut self.rules[k];
        let was_empty = set.groups.is_empty() && set.stamps.is_empty();
        let pos = set.stamps.iter().position(|s| s.slug == stamp);
        let what = format!("rules/{layer}.json stamps");
        let inverse = match list_set(&mut set.stamps, pos, def, index, &what)? {
            ListEdit::Noop => return Ok(noop()),
            ListEdit::Created => Command::RemoveStamp {
                layer,
                stamp,
                keep_uses,
                keep_empty: was_empty,
            },
            ListEdit::Replaced { old, moved_from } => Command::SetStamp {
                layer,
                stamp,
                def: old,
                index: moved_from,
            },
        };
        Ok(Applied { inverse, changes })
    }

    pub(super) fn remove_stamp(
        &mut self,
        layer: Slug,
        stamp: Slug,
        keep_uses: bool,
        keep_empty: bool,
    ) -> Result<Applied> {
        let unknown = || CommandError::UnknownStamp {
            layer: layer.clone(),
            stamp: stamp.clone(),
        };
        let k = self.rule_set_index(&layer, unknown())?;
        unique_stamps(&self.rules[k])?;
        let i = self.rules[k]
            .stamps
            .iter()
            .position(|s| s.slug == stamp)
            .ok_or_else(unknown)?;
        let uses = self.stamp_uses(&layer, &stamp);
        if !uses.is_empty() && !keep_uses {
            return Err(CommandError::StampInUse { layer, stamp, uses });
        }
        let def = self.rules[k].stamps.remove(i);
        // As for `remove_rule_group`: a set not tied to an auto layer stays,
        // so that the inverse (which needs an auto layer to make one) works.
        let auto = self.layer(layer.as_str()).is_some_and(LayerDef::is_auto);
        let set = &self.rules[k];
        if set.groups.is_empty() && set.stamps.is_empty() && !keep_empty && auto {
            self.rules.remove(k);
        }
        Ok(Applied {
            inverse: Command::SetStamp {
                layer: layer.clone(),
                stamp,
                def,
                index: Some(i),
            },
            changes: vec![rules_change(&layer)],
        })
    }
}
