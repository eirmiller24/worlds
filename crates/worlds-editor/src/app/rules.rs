//! The Rules pane: edit an `auto` layer's rule groups, rules, template
//! references and templates, with live "where does this rule fire"
//! highlighting on the canvas.
//!
//! Left, the list: group headers (slug, name, mode, color, level-field
//! requirement), rule rows (drag handle, active, slug, mini pattern, first
//! tile, "fires N · matches M" in the active level) and reference rows
//! (`use frame6 as walls`, its expansion with skip toggles, Detach); below,
//! the template library. Right, the selected entry: the pattern as a grid
//! you click and as text, `require`, the options, and the output with an
//! atlas picker. Every edit is a command queued on [`EditorState::pending`]
//! (one labelled undo entry each), so the pane itself only reads the
//! document.
//!
//! Highlight data ([`RulesState::highlight`]) is worked out by [`refresh`]
//! once per frame, before the dock: fired cells from the active level's
//! current bake (`LayerBake::fired`), "matches but blocked" from
//! [`worlds_rules::rule_matches_all`] (one compile) minus fired, recomputed only when the
//! project, the level or the layer changed and no gesture is in progress.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use egui::{Color32, RichText, Sense, Stroke, StrokeKind, Vec2};
use worlds_core::command::Command;
use worlds_core::{
    Align, CellMatch, Checker, Color, Edge, GroupMode, Id, LayerDef, Legend, LevelFieldRequirement,
    LevelTagRequirement, MatchSelect, Modulo, OutOfBounds, Output, Pattern, PerAxis, Perlin, Place,
    PlaceAt, RandomOffset, Region, RequirementMode, Rows, Rule, RuleEntry, RuleGroup, RuleOutput,
    RuleSet, Slug, Spacing, Symmetry, Template, TemplateOutput, TemplateRef, TemplateRule, TileAlt,
    TileMode, TilesetDef, Write,
};
use worlds_rules::RegionPreview;

use super::EditorState;
use crate::doc::{Document, LevelIx};
use crate::viewport::scene::parse_color;

/// The pane's own state (not saved).
#[derive(Default)]
pub struct RulesState {
    /// The auto layer shown; `None`: the active layer if it is an auto
    /// layer, else the first auto layer.
    pub layer: Option<Slug>,
    /// The selected rule, reference or expanded template rule (by slug).
    pub selected: Option<Slug>,
    /// More selected inline rules (Ctrl+click), for "Save as template".
    pub multi: BTreeSet<Slug>,
    /// Collapsed group slugs.
    pub collapsed: BTreeSet<Slug>,
    /// References whose expansion is open in the list.
    pub expanded: BTreeSet<Slug>,
    /// Bring the Rules tab to the front next frame.
    pub focus: bool,
    /// Scroll the list to the selected row when it is next drawn, at once
    /// (no animation: a headless screenshot renders only a few frames). Set
    /// when something other than a click in the list selects a rule: a `ui`
    /// script step, `--rule`.
    pub reveal: bool,
    /// What the canvas highlights (see [`refresh`]).
    pub highlight: Option<Highlight>,
    /// The glyph Shift+click paints in a pattern grid.
    pub brush: Option<char>,
    text: Option<TextBuf>,
    /// Numbers being dragged or typed, by field key, until committed.
    nums: HashMap<String, f64>,
    /// Single-line texts being typed, by field key, until committed.
    strs: HashMap<String, String>,
    stats: Stats,
    /// Atlas picker drag: the first tile and whether Shift was held.
    atlas_drag: Option<([u32; 2], bool)>,
    apply: Option<ApplyDialog>,
    save: Option<SaveDialog>,
    /// Where each pattern cell was drawn last frame, by (pattern key, x,
    /// y): for tests that click them.
    #[doc(hidden)]
    pub cell_rects: HashMap<(String, u32, u32), egui::Rect>,
    /// Where each list row and its drag handle were drawn last frame, by
    /// entry slug; an expanded rule's row (no handle) twice (tests).
    #[doc(hidden)]
    pub row_rects: HashMap<Slug, (egui::Rect, egui::Rect)>,
    /// Where the pane was drawn last frame (screen points).
    pub pane_rect: Option<egui::Rect>,
    /// Where each group's drag handle and each `Pass n` header were drawn
    /// last frame (tests).
    #[doc(hidden)]
    pub group_rects: HashMap<Slug, egui::Rect>,
    #[doc(hidden)]
    pub pass_rects: HashMap<u32, egui::Rect>,
    /// "Show regions" is off (it is on by default): a selected region rule
    /// then highlights only its firings and blocked candidates.
    pub hide_regions: bool,
    /// "Show occupancy": hatch the layer's occupied cells in the active
    /// level (`LayerBake::occupied`).
    pub show_occupancy: bool,
    /// The selected region rule's regions, and what they were worked out
    /// for (edit revision, images, level, layer, rule).
    region_cache: Option<(RegionKey, Option<Vec<RegionPreview>>)>,
}

/// What a region preview was worked out for.
type RegionKey = (u64, u64, Id, Slug, Slug);

/// A pattern's text box while it is being edited.
struct TextBuf {
    key: String,
    text: String,
}

/// Fires and matches per expanded rule in the active level.
#[derive(Default)]
struct Stats {
    fired_key: Option<(u64, Id, Slug, usize)>,
    matched_key: Option<(u64, u64, Id, Slug)>,
    /// Expanded rule slug -> anchors it fired at (after break, chance...).
    fired: HashMap<Slug, Vec<[i32; 2]>>,
    /// Expanded rule slug -> anchors its pattern matches at.
    matched: HashMap<Slug, BTreeSet<[i32; 2]>>,
    /// Entry slug (inline rule or reference) and expanded slug -> the
    /// expanded rules it stands for.
    members: HashMap<Slug, Vec<Slug>>,
    /// Why the rules don't expand or bake, if they don't.
    error: Option<String>,
}

impl Stats {
    fn count(&self, slugs: &[Slug]) -> (usize, Option<usize>) {
        let mut fired: BTreeSet<[i32; 2]> = BTreeSet::new();
        let mut matched: BTreeSet<[i32; 2]> = BTreeSet::new();
        let mut known = self.matched_key.is_some();
        for s in slugs {
            if let Some(f) = self.fired.get(s) {
                fired.extend(f.iter().copied());
            }
            match self.matched.get(s) {
                Some(m) => matched.extend(m.iter().copied()),
                None => known &= self.members.contains_key(s),
            }
        }
        (fired.len(), known.then_some(matched.len()))
    }

    fn members_of(&self, slug: &Slug) -> Vec<Slug> {
        self.members
            .get(slug)
            .cloned()
            .unwrap_or_else(|| vec![slug.clone()])
    }
}

/// What the canvas draws for the selected rule (in the active level).
#[derive(Debug, Clone, PartialEq)]
pub struct Highlight {
    pub level: LevelIx,
    pub layer: Slug,
    /// Anchors where the rule fired: filled.
    pub fired: Vec<[i32; 2]>,
    /// Anchors where its pattern matches but it didn't fire (chance,
    /// perlin, an earlier rule's break, an occupied footprint; a region
    /// rule's candidates that didn't fire): outlined.
    pub blocked: Vec<[i32; 2]>,
    /// A selected region rule's regions ("Show regions"): each bbox
    /// outlined with a `w × h` label (selected ones in the accent), the
    /// runs of its edge as 2 px lines.
    pub regions: Vec<RegionPreview>,
    /// The edge the runs lie on (`place.along`); `None` for `across`.
    pub edge: Option<Edge>,
    /// Occupied cells of the layer ("Show occupancy"): hatched.
    pub occupied: Vec<[i32; 2]>,
}

/// The Apply… dialog of the template library.
struct ApplyDialog {
    template: Slug,
    slug: String,
    tileset: Option<Slug>,
    origin: [u32; 2],
    group: Option<Slug>,
    index: Option<usize>,
    error: Option<String>,
}

/// The "Save selection as template" dialog.
struct SaveDialog {
    rules: Vec<Slug>,
    name: String,
    reference: String,
    origin: [u32; 2],
    replace: bool,
    error: Option<String>,
}

/// What a list row drag carries.
#[derive(Debug, Clone, PartialEq)]
enum Drag {
    Entry(Slug),
    Group(Slug),
}

// ---------------------------------------------------------------------
// Pure helpers (unit tested below)
// ---------------------------------------------------------------------

/// The auto layer the pane shows: `wanted` if it is one, else the active
/// layer if it is one, else the first auto layer.
pub fn pane_layer(doc: &Document, wanted: Option<&Slug>, active: Option<usize>) -> Option<Slug> {
    let is_auto = |s: &str| doc.project.layer(s).is_some_and(|d| d.is_auto());
    if let Some(w) = wanted.filter(|w| is_auto(w.as_str())) {
        return Some(w.clone());
    }
    if let Some(def) = active.and_then(|i| doc.project.layers.get(i))
        && def.is_auto()
    {
        return Some(def.slug.clone());
    }
    doc.project
        .layers
        .iter()
        .find(|l| l.is_auto())
        .map(|l| l.slug.clone())
}

/// Where an entry with this slug is: (group index, entry index).
pub fn find_entry(set: &RuleSet, slug: &str) -> Option<(usize, usize)> {
    set.groups.iter().enumerate().find_map(|(gi, g)| {
        g.rules
            .iter()
            .position(|e| e.slug() == slug)
            .map(|ei| (gi, ei))
    })
}

/// A pattern as the text box shows it: the rows, a blank line, then one
/// `glyph = test` line per legend entry. Names from `rules/macros.json`
/// read `rows = name` and `legend = name`.
pub fn pattern_text(p: &Pattern) -> String {
    let mut out = String::new();
    match &p.rows {
        Rows::Inline(rows) => {
            for r in rows {
                out.push_str(r);
                out.push('\n');
            }
        }
        Rows::Named(n) => out.push_str(&format!("rows = {n}\n")),
    }
    match &p.legend {
        Legend::Inline(m) if m.is_empty() => {}
        Legend::Inline(m) => {
            out.push('\n');
            for (g, t) in m {
                out.push_str(&format!("{g} = {t}\n"));
            }
        }
        Legend::Named(n) => out.push_str(&format!("\nlegend = {n}\n")),
    }
    out.trim_end().to_string()
}

/// Parse [`pattern_text`] back; `grid` is kept from the old pattern.
/// Errors name the line.
pub fn parse_pattern_text(text: &str, grid: Option<Slug>) -> Result<Pattern, String> {
    let mut rows: Option<Rows> = None;
    let mut inline: Vec<String> = Vec::new();
    let mut legend: Option<Legend> = None;
    let mut map: BTreeMap<char, CellMatch> = BTreeMap::new();
    let mut in_legend = false;
    for (n, raw) in text.lines().enumerate() {
        let line = raw.trim();
        let at = |m: String| format!("line {}: {m}", n + 1);
        if line.is_empty() {
            if !inline.is_empty() || rows.is_some() {
                in_legend = true;
            }
            continue;
        }
        if let Some(name) = line.strip_prefix("rows =") {
            if !inline.is_empty() || rows.is_some() {
                return Err(at("rows given twice".into()));
            }
            rows = Some(Rows::Named(
                Slug::new(name.trim()).map_err(|e| at(e.to_string()))?,
            ));
            continue;
        }
        if let Some(name) = line.strip_prefix("legend =") {
            if !map.is_empty() || legend.is_some() {
                return Err(at("legend given twice".into()));
            }
            legend = Some(Legend::Named(
                Slug::new(name.trim()).map_err(|e| at(e.to_string()))?,
            ));
            in_legend = true;
            continue;
        }
        if in_legend {
            let mut chars = line.chars();
            let glyph = chars.next().expect("not empty");
            let rest = chars.as_str().trim_start();
            let Some(test) = rest.strip_prefix('=') else {
                return Err(at(format!(
                    "expected `{glyph} = test` (a legend entry), got {line:?}"
                )));
            };
            if legend.is_some() {
                return Err(at("a named legend can't have entries too".into()));
            }
            let m = CellMatch::try_from(test.trim().to_string()).map_err(at)?;
            if map.insert(glyph, m).is_some() {
                return Err(at(format!("glyph {glyph:?} is in the legend twice")));
            }
        } else {
            if rows.is_some() {
                return Err(at("rows = name can't be followed by rows".into()));
            }
            if let Some(first) = inline.first()
                && first.chars().count() != line.chars().count()
            {
                return Err(at(format!(
                    "row {:?} is {} wide, the first row is {}",
                    line,
                    line.chars().count(),
                    first.chars().count()
                )));
            }
            inline.push(line.to_string());
        }
    }
    let rows = match rows {
        Some(r) => r,
        None if inline.is_empty() => return Err("no rows".into()),
        None => Rows::Inline(inline),
    };
    Ok(Pattern {
        grid,
        legend: legend.unwrap_or(Legend::Inline(map)),
        rows,
    })
}

/// Glyphs a pattern cell click cycles through: `*`, the grid's value and
/// group glyphs, `.`, `+`, then the pattern's own legend glyphs.
pub fn glyph_cycle(p: &Pattern, grid: Option<&LayerDef>) -> Vec<char> {
    let mut out = vec!['*'];
    let mut push = |c: char| {
        if c != '@' && !out.contains(&c) {
            out.push(c);
        }
    };
    if let Some(g) = grid {
        for v in g.values() {
            push(v.glyph);
        }
        for gr in g.groups() {
            if let Some(c) = gr.glyph {
                push(c);
            }
        }
    }
    push('.');
    push('+');
    if let Some(m) = p.inline_legend() {
        for c in m.keys() {
            push(*c);
        }
    }
    out
}

/// The rows with cell `(x, y)` set to `glyph`. Setting `@` moves the
/// anchor: any other `@` becomes `*` (a pattern has at most one).
pub fn set_cell(rows: &[String], x: usize, y: usize, glyph: char) -> Vec<String> {
    rows.iter()
        .enumerate()
        .map(|(ry, r)| {
            r.chars()
                .enumerate()
                .map(|(rx, c)| {
                    if (rx, ry) == (x, y) {
                        glyph
                    } else if glyph == '@' && c == '@' {
                        '*'
                    } else {
                        c
                    }
                })
                .collect()
        })
        .collect()
}

/// The `move_rule` that drops entry `rule` before (or `after`) entry
/// `target` of group `group` (`None`: at its end): the group slug and the
/// index counted after taking `rule` out of its old place. `None` when it
/// would not move.
pub fn drop_target(
    set: &RuleSet,
    rule: &Slug,
    group: usize,
    target: Option<usize>,
    after: bool,
) -> Option<(Slug, usize)> {
    let g = set.groups.get(group)?;
    let rest: Vec<&Slug> = g
        .rules
        .iter()
        .map(|e| e.slug())
        .filter(|s| *s != rule)
        .collect();
    let index = match target {
        None => rest.len(),
        Some(ei) => {
            let t = g.rules.get(ei)?.slug();
            if t == rule {
                return None;
            }
            rest.iter().position(|s| *s == t)? + after as usize
        }
    };
    if let Some((gi, ei)) = find_entry(set, rule.as_str()) {
        // Where it is now, counted the same way.
        let now = set.groups[gi].rules[..ei]
            .iter()
            .filter(|e| e.slug() != rule)
            .count();
        if gi == group && now == index {
            return None;
        }
    }
    Some((g.slug.clone(), index))
}

/// The `set_rule_group` index that drops group `moved` before (or `after`)
/// group `target`, counted without `moved`; `None` when it would not move.
pub fn group_drop_index(set: &RuleSet, moved: &Slug, target: usize, after: bool) -> Option<usize> {
    let t = &set.groups.get(target)?.slug;
    if t == moved {
        return None;
    }
    let rest: Vec<&Slug> = set
        .groups
        .iter()
        .map(|g| &g.slug)
        .filter(|s| *s != moved)
        .collect();
    let index = rest.iter().position(|s| *s == t)? + after as usize;
    let now = set.groups.iter().position(|g| &g.slug == moved)?;
    (now != index).then_some(index)
}

/// Where group `moved` goes in the file when it joins pass `pass`: the
/// `set_rule_group` index (counted without it) right after every other
/// group of a lower pass (`last`: also after those of pass `pass`).
pub fn pass_index(set: &RuleSet, moved: &Slug, pass: u32, last: bool) -> usize {
    set.groups
        .iter()
        .filter(|g| &g.slug != moved)
        .filter(|g| g.pass < pass || (last && g.pass == pass))
        .count()
}

/// The `set_rule_group` (group with its new `pass`, index) that drops group
/// `moved` under the `Pass n` header: first in that pass. `None` when it
/// is already there.
pub fn group_into_pass(set: &RuleSet, moved: &Slug, pass: u32) -> Option<(RuleGroup, usize)> {
    let now = set.groups.iter().position(|g| &g.slug == moved)?;
    let mut def = set.groups[now].clone();
    let index = pass_index(set, moved, pass, false);
    if def.pass == pass && index == now {
        return None;
    }
    def.pass = pass;
    Some((def, index))
}

/// A group header's `pass` edit: the group with pass `pass`, and an index
/// only when the file would otherwise stop being in pass order (then it
/// goes to the nearest place that keeps it: first in its new pass when
/// the pass grew, last when it shrank).
pub fn set_pass(set: &RuleSet, moved: &Slug, pass: u32) -> Option<(RuleGroup, Option<usize>)> {
    let now = set.groups.iter().position(|g| &g.slug == moved)?;
    let old = set.groups[now].pass;
    if old == pass {
        return None;
    }
    let mut def = set.groups[now].clone();
    def.pass = pass;
    let fits = set.groups[..now].iter().all(|g| g.pass <= pass)
        && set.groups[now + 1..].iter().all(|g| g.pass >= pass);
    let index = (!fits).then(|| pass_index(set, moved, pass, pass < old));
    Some((def, index))
}

/// The groups of a rule set in run order (by `pass`, stably), as indices.
pub fn run_order(set: &RuleSet) -> Vec<usize> {
    let mut order: Vec<usize> = (0..set.groups.len()).collect();
    order.sort_by_key(|&gi| set.groups[gi].pass);
    order
}

/// A one-line summary of a region rule: `. w≥15 · floor every 6`.
pub fn region_summary(r: &worlds_core::RegionSelect) -> String {
    let g = &r.region;
    let mut out = g.cells.to_string();
    for (name, lo, hi) in [
        ("w", g.min_width, g.max_width),
        ("h", g.min_height, g.max_height),
        ("area", g.min_area, g.max_area),
    ] {
        match (lo, hi) {
            (Some(a), Some(b)) => out.push_str(&format!(" {name} {a}..{b}")),
            (Some(a), None) => out.push_str(&format!(" {name}≥{a}")),
            (None, Some(b)) => out.push_str(&format!(" {name}≤{b}")),
            (None, None) => {}
        }
    }
    let at = match r.place.at {
        PlaceAt::Along(e) => edge_name(e),
        PlaceAt::Across => "interior",
    };
    let every = match r.place.every {
        PerAxis::Same(v) => v.to_string(),
        PerAxis::Each([x, y]) => format!("[{x}, {y}]"),
    };
    format!("{out} · {at} every {every}")
}

fn edge_name(e: Edge) -> &'static str {
    match e {
        Edge::Floor => "floor",
        Edge::Ceiling => "ceiling",
        Edge::WallLeft => "wall_left",
        Edge::WallRight => "wall_right",
    }
}

/// "Detach" a reference: remove it and put its expanded rules in its place,
/// keeping their expanded slugs (so their random choices don't re-roll).
pub fn detach_commands(
    project: &worlds_core::Project,
    layer: &Slug,
    reference: &Slug,
) -> Result<Vec<Command>, String> {
    let set = project
        .rules_for(layer.as_str())
        .ok_or_else(|| format!("layer {layer} has no rules"))?;
    let (gi, ei) = find_entry(set, reference.as_str())
        .ok_or_else(|| format!("no reference {reference} in {layer}"))?;
    let group = &set.groups[gi];
    if group.rules[ei].as_use().is_none() {
        return Err(format!("{reference} is a rule, not a template reference"));
    }
    let expanded = project
        .expanded_rules(layer.as_str())
        .map_err(|e| e.to_string())?;
    let rules: Vec<Rule> = expanded[gi]
        .rules
        .iter()
        .filter(|r| r.entry == ei)
        .map(|r| r.rule.clone().into_owned())
        .collect();
    let mut cmds = vec![Command::RemoveRule {
        layer: layer.clone(),
        rule: reference.clone(),
    }];
    for (i, r) in rules.into_iter().enumerate() {
        cmds.push(Command::SetRule {
            layer: layer.clone(),
            group: group.slug.clone(),
            rule: r.slug.clone(),
            def: RuleEntry::Rule(r),
            index: Some(ei + i),
        });
    }
    Ok(cmds)
}

/// The tileset every rule draws from, if they all draw tiles from one.
fn common_tileset<'a>(rules: &[&'a Rule]) -> Option<&'a Slug> {
    let tiles = |r: &'a Rule| r.out().and_then(RuleOutput::as_tiles);
    let first = tiles(rules.first()?)?.tileset.clone();
    rules
        .iter()
        .all(|r| tiles(r).is_some_and(|t| t.tileset == first))
        .then(|| &tiles(rules[0]).expect("checked").tileset)
}

/// The top-left tile any of these rules draws (the default origin).
fn min_tile(rules: &[&Rule]) -> [u32; 2] {
    let mut m: Option<[u32; 2]> = None;
    for r in rules {
        if let Some(t) = r.out().and_then(RuleOutput::as_tiles) {
            for alt in &t.tiles {
                for c in &alt.tiles {
                    m = Some(m.map_or(*c, |m| [m[0].min(c[0]), m[1].min(c[1])]));
                }
            }
        }
    }
    m.unwrap_or([0, 0])
}

/// The common `<prefix>__` of the slugs, if they all have one (rules a
/// Detach made): a template saved from them keeps their expanded slugs.
fn common_prefix(slugs: &[&Slug]) -> Option<String> {
    let first = slugs.first()?.as_str().split_once("__")?.0.to_string();
    slugs
        .iter()
        .all(|s| {
            s.as_str()
                .split_once("__")
                .is_some_and(|(p, r)| p == first && !r.is_empty())
        })
        .then_some(first)
}

/// A template from inline rules. Rules that all draw from one tileset get
/// tiles relative to `origin` and no tileset of their own (the reference
/// gives it); otherwise each rule is fixed (keeps its tileset and absolute
/// tiles). Slugs lose `strip` + `__` when given.
pub fn template_from_rules(
    rules: &[&Rule],
    origin: [u32; 2],
    strip: Option<&str>,
) -> Result<(Template, Option<Slug>), String> {
    let tileset = common_tileset(rules).cloned();
    let mut out = Vec::new();
    for r in rules {
        let mut t = TemplateRule::try_from((*r).clone())?;
        if let Some(p) = strip
            && let Some(rest) = r.slug.as_str().strip_prefix(&format!("{p}__"))
        {
            t.slug = Slug::new(rest).map_err(|e| e.to_string())?;
        }
        if tileset.is_some()
            && let TemplateOutput::Tiles(o) = &mut t.out
        {
            o.tileset = None;
            for alt in &mut o.tiles {
                for c in &mut alt.tiles {
                    if c[0] < origin[0] || c[1] < origin[1] {
                        return Err(format!(
                            "rule {} draws tile [{}, {}], left of or above the origin [{}, {}]",
                            r.slug, c[0], c[1], origin[0], origin[1]
                        ));
                    }
                    *c = [c[0] - origin[0], c[1] - origin[1]];
                }
            }
        }
        out.push(t);
    }
    Ok((
        Template {
            name: None,
            doc: None,
            legend: Legend::default(),
            rules: out,
        },
        tileset,
    ))
}

/// A fresh slug `base`, `base_2`, ... not in `taken`.
fn fresh_slug(base: &str, taken: &dyn Fn(&str) -> bool) -> Slug {
    let mut n = 1;
    loop {
        let s = if n == 1 {
            base.to_string()
        } else {
            format!("{base}_{n}")
        };
        if !taken(&s)
            && let Ok(slug) = Slug::new(&s)
        {
            return slug;
        }
        n += 1;
    }
}

// ---------------------------------------------------------------------
// Per-frame data: counts and the highlight
// ---------------------------------------------------------------------

/// Bring the pane's counts and the canvas highlight up to date (once per
/// frame, before the dock). `rev` changes with every edit; matches are only
/// recomputed when it (or the level or layer) changed and no gesture is in
/// progress, so a brush stroke doesn't pay for them on every move.
pub fn refresh(st: &mut EditorState, rev: u64, in_gesture: bool) {
    let EditorState {
        doc, view, rules, ..
    } = st;
    rules.pane_rect = None;
    let Some(doc) = doc.as_ref() else {
        rules.highlight = None;
        return;
    };
    rules.layer = pane_layer(doc, rules.layer.as_ref(), view.active_layer);
    let (Some(layer), Some(ix)) = (
        rules.layer.clone(),
        view.active_level.filter(|&i| i < doc.levels.len()),
    ) else {
        rules.highlight = None;
        rules.stats = Stats::default();
        return;
    };
    let id = doc.ids[ix].clone();
    let bake = doc.bakes[ix].as_ref().and_then(|b| b.get(&layer)).cloned();
    let ptr = bake.as_ref().map_or(0, |b| Arc::as_ptr(b) as usize);
    let s = &mut rules.stats;
    let fired_key = (rev, id.clone(), layer.clone(), ptr);
    if s.fired_key.as_ref() != Some(&fired_key) {
        s.fired_key = Some(fired_key);
        s.members.clear();
        s.error = None;
        match doc.project.expanded_rules(layer.as_str()) {
            Ok(groups) => {
                for g in &groups {
                    for r in &g.rules {
                        let entry = g.group.rules[r.entry].slug().clone();
                        s.members
                            .entry(entry)
                            .or_default()
                            .push(r.rule.slug.clone());
                        s.members
                            .entry(r.rule.slug.clone())
                            .or_insert_with(|| vec![r.rule.slug.clone()]);
                    }
                }
            }
            Err(e) => s.error = Some(e.to_string()),
        }
        s.fired.clear();
        if let Some(b) = &bake {
            for (cell, slugs) in &b.fired {
                for slug in slugs {
                    s.fired.entry(slug.clone()).or_default().push(*cell);
                }
            }
        }
    }
    let matched_key = (rev, doc.images_gen, id, layer.clone());
    if s.matched_key.as_ref() != Some(&matched_key) && !in_gesture {
        s.matched_key = Some(matched_key);
        s.matched.clear();
        let t = std::time::Instant::now();
        let level = doc.level(ix);
        // Every rule from one compile of the layer.
        match worlds_rules::rule_matches_all(&doc.project, level, &layer, &doc.images) {
            Ok(list) => {
                for (slug, m) in list {
                    s.matched
                        .entry(slug)
                        .or_insert_with(|| m.into_keys().collect());
                }
            }
            Err(e) => {
                s.error.get_or_insert(e.to_string());
            }
        }
        tracing::debug!(
            "rule matches for {layer} in {}: {:.2} ms",
            level.slug,
            t.elapsed().as_secs_f64() * 1e3
        );
    }
    // A selected region rule's regions, worked out (one bake of the layer)
    // only when what they depend on changed and no gesture is running.
    let region_rule = rules.selected.as_ref().and_then(|sel| {
        let set = doc.project.rules_for(layer.as_str())?;
        let (gi, ei) = find_entry(set, sel.as_str())?;
        let r = set.groups[gi].rules[ei].as_rule()?;
        r.as_region().map(|x| (sel.clone(), x.place.at))
    });
    let mut regions = Vec::new();
    let mut edge = None;
    if let Some((rule, at)) = region_rule.filter(|_| !rules.hide_regions) {
        let key: RegionKey = (
            rev,
            doc.images_gen,
            doc.ids[ix].clone(),
            layer.clone(),
            rule.clone(),
        );
        let stale = rules.region_cache.as_ref().is_none_or(|(k, _)| *k != key);
        if stale && !in_gesture {
            let level = doc.level(ix);
            let found = worlds_rules::region_preview(
                &doc.project,
                level,
                &layer,
                rule.as_str(),
                &doc.images,
            )
            .unwrap_or_else(|e| {
                tracing::debug!("region preview of {rule}: {e}");
                None
            });
            rules.region_cache = Some((key, found));
        }
        if let Some((_, Some(r))) = &rules.region_cache {
            regions = r.clone();
        }
        edge = match at {
            PlaceAt::Along(e) => Some(e),
            PlaceAt::Across => None,
        };
    }
    let occupied: Vec<[i32; 2]> = match (&bake, rules.show_occupancy) {
        (Some(b), true) if !b.occupied.is_empty() => {
            let w = doc
                .project
                .layer(layer.as_str())
                .map_or(0, |d| doc.project.layer_cells(d, doc.level(ix))[0] as usize)
                .max(1);
            b.occupied
                .iter()
                .enumerate()
                .filter(|(_, o)| **o)
                .map(|(i, _)| [(i % w) as i32, (i / w) as i32])
                .collect()
        }
        _ => Vec::new(),
    };
    if rules.selected.is_none() && occupied.is_empty() {
        rules.highlight = None;
        return;
    }
    let mut fired: BTreeSet<[i32; 2]> = BTreeSet::new();
    let mut matched: BTreeSet<[i32; 2]> = BTreeSet::new();
    if let Some(sel) = &rules.selected {
        for m in &s.members_of(sel) {
            if let Some(f) = s.fired.get(m) {
                fired.extend(f.iter().copied());
            }
            if let Some(x) = s.matched.get(m) {
                matched.extend(x.iter().copied());
            }
        }
    }
    rules.highlight = Some(Highlight {
        level: ix,
        layer: layer.clone(),
        blocked: matched.difference(&fired).copied().collect(),
        fired: fired.into_iter().collect(),
        regions,
        edge,
        occupied,
    });
}

/// "fires N · matches M" for an entry or expanded slug (the active level).
pub fn counts_text(st: &RulesState, slug: &Slug) -> String {
    let (f, m) = st.stats.count(&st.stats.members_of(slug));
    match m {
        Some(m) => format!("fires {f} · matches {m}"),
        None => format!("fires {f}"),
    }
}

// ---------------------------------------------------------------------
// The pane
// ---------------------------------------------------------------------

/// Queue `cmds` as one undo entry labelled `label`.
fn push(pending: &mut Vec<Command>, label: impl Into<String>, cmds: Vec<Command>) {
    if cmds.is_empty() {
        return;
    }
    pending.push(Command::Batch {
        label: label.into(),
        commands: cmds,
    });
}

fn set_entry(layer: &Slug, group: &Slug, e: RuleEntry) -> Command {
    Command::SetRule {
        layer: layer.clone(),
        group: group.clone(),
        rule: e.slug().clone(),
        def: e,
        index: None,
    }
}

fn set_group(layer: &Slug, g: RuleGroup) -> Command {
    Command::SetRuleGroup {
        layer: layer.clone(),
        group: g.slug.clone(),
        def: g,
        index: None,
    }
}

/// Everything the pane's parts read and write, borrowed apart.
struct Cx<'a> {
    doc: &'a Document,
    layer: Slug,
    r: &'a mut RulesState,
    pending: &'a mut Vec<Command>,
    atlas: &'a mut HashMap<String, Option<egui::TextureHandle>>,
    accent: Color32,
    status: Option<String>,
}

/// The Rules pane.
pub fn pane(ui: &mut egui::Ui, st: &mut EditorState) {
    let EditorState {
        doc,
        view,
        rules,
        pending,
        atlas,
        accent,
        ..
    } = st;
    rules.pane_rect = Some(ui.max_rect());
    let Some(doc) = doc.as_ref() else {
        ui.weak("No project");
        return;
    };
    let auto: Vec<Slug> = doc
        .project
        .layers
        .iter()
        .filter(|l| l.is_auto())
        .map(|l| l.slug.clone())
        .collect();
    let Some(layer) = pane_layer(doc, rules.layer.as_ref(), view.active_layer) else {
        ui.weak("The project has no auto layers. Add one (kind \"auto\") to write rules for it.");
        return;
    };
    rules.layer = Some(layer.clone());
    let level = view.active_level.filter(|&i| i < doc.levels.len());
    let mut cx = Cx {
        doc,
        layer: layer.clone(),
        r: rules,
        pending,
        atlas,
        accent: *accent,
        status: None,
    };
    // Header: layer, level, selection counts, actions.
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt("rules layer")
            .selected_text(layer.as_str())
            .show_ui(ui, |ui| {
                for l in &auto {
                    if ui.selectable_label(*l == layer, l.as_str()).clicked() {
                        cx.r.layer = Some(l.clone());
                        cx.r.selected = None;
                        cx.r.multi.clear();
                    }
                }
            });
        match level {
            Some(ix) => ui.label(format!("in {}", doc.level(ix).slug)),
            None => ui.weak("no active level: click a level to see counts"),
        };
        if let Some(sel) = cx.r.selected.clone() {
            ui.separator();
            ui.strong(sel.as_str());
            ui.label(RichText::new(counts_text(cx.r, &sel)).color(cx.accent));
        }
        if let Some(e) = &cx.r.stats.error {
            ui.colored_label(Color32::from_rgb(230, 150, 90), e);
        }
        ui.separator();
        let mut show = !cx.r.hide_regions;
        if ui
            .checkbox(&mut show, "Show regions")
            .on_hover_text(
                "A selected region rule outlines each region it looks at (selected ones in the \
                 accent, with w × h) and draws the runs of its edge",
            )
            .changed()
        {
            cx.r.hide_regions = !show;
        }
        ui.checkbox(&mut cx.r.show_occupancy, "Show occupancy")
            .on_hover_text(
                "Hatch the cells the layer's stamps occupy (occupies) in the active level; \
                 avoid_overlap placements skip them",
            );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let set = doc.project.rules_for(layer.as_str());
            let chosen = selected_inline(&cx, set);
            if ui
                .add_enabled(
                    !chosen.is_empty(),
                    egui::Button::new("Save as template…"),
                )
                .on_hover_text(
                    "Make a template in rules/macros.json from the selected rules (Ctrl+click to select several)",
                )
                .on_disabled_hover_text("Select inline rules first (Ctrl+click for several)")
                .clicked()
            {
                cx.r.save = Some(save_dialog(&chosen));
            }
            if ui.button("+ Group").clicked() {
                let taken = |s: &str| set.is_some_and(|set| set.groups.iter().any(|g| g.slug == s));
                let slug = fresh_slug("group", &taken);
                push(
                    cx.pending,
                    format!("Add rule group {slug}"),
                    vec![set_group(&layer, RuleGroup::new(slug, Vec::new()))],
                );
            }
        });
    });
    ui.separator();
    let avail = ui.available_size();
    let left_w = (avail.x * 0.45).clamp(240.0, 560.0);
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            Vec2::new(left_w, avail.y),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("rules list")
                    .auto_shrink([false, false])
                    .show(ui, |ui| list(ui, &mut cx));
            },
        );
        ui.separator();
        ui.allocate_ui_with_layout(
            Vec2::new(ui.available_width(), avail.y),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("rule editor")
                    .auto_shrink([false, false])
                    .show(ui, |ui| editor(ui, &mut cx));
            },
        );
    });
    apply_dialog(ui.ctx(), &mut cx);
    save_template_dialog(ui.ctx(), &mut cx);
    if let Some(s) = cx.status.take() {
        view.set_status(s, true);
    }
}

/// The selected inline rules (the selection plus the Ctrl+click ones), in
/// file order.
fn selected_inline<'a>(cx: &Cx<'_>, set: Option<&'a RuleSet>) -> Vec<&'a Rule> {
    let Some(set) = set else {
        return Vec::new();
    };
    set.entries()
        .filter_map(|(_, e)| e.as_rule())
        .filter(|r| cx.r.selected.as_ref() == Some(&r.slug) || cx.r.multi.contains(&r.slug))
        .collect()
}

fn save_dialog(rules: &[&Rule]) -> SaveDialog {
    let slugs: Vec<&Slug> = rules.iter().map(|r| &r.slug).collect();
    let prefix = common_prefix(&slugs);
    SaveDialog {
        rules: rules.iter().map(|r| r.slug.clone()).collect(),
        name: prefix.clone().unwrap_or_else(|| "my_template".into()),
        reference: prefix.unwrap_or_else(|| "my_template".into()),
        origin: min_tile(rules),
        replace: true,
        error: None,
    }
}

// ---- the list ----

fn list(ui: &mut egui::Ui, cx: &mut Cx<'_>) {
    let doc = cx.doc;
    let layer = cx.layer.clone();
    let Some(set) = doc.project.rules_for(layer.as_str()) else {
        ui.weak(format!(
            "No rules for {layer} yet: add a group (+ Group), then rules in it."
        ));
        templates(ui, cx);
        return;
    };
    // Run order: the groups by pass (stably), each pass under a header.
    let mut last_pass = None;
    for gi in run_order(set) {
        let g = &set.groups[gi];
        if last_pass != Some(g.pass) {
            pass_header(ui, cx, set, g.pass, false);
            last_pass = Some(g.pass);
        }
        group_header(ui, cx, set, gi, g);
        if cx.r.collapsed.contains(&g.slug) {
            continue;
        }
        for (ei, e) in g.rules.iter().enumerate() {
            entry_row(ui, cx, set, gi, ei, e);
        }
        // Drop here to append to the group.
        let (rect, resp) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), 6.0), Sense::hover());
        if let Some(p) = resp.dnd_hover_payload::<Drag>()
            && matches!(*p, Drag::Entry(_))
        {
            ui.painter()
                .hline(rect.x_range(), rect.center().y, Stroke::new(2.0, cx.accent));
        }
        if let Some(p) = resp.dnd_release_payload::<Drag>()
            && let Drag::Entry(rule) = &*p
            && let Some((group, index)) = drop_target(set, rule, gi, None, false)
        {
            move_rule(cx, rule, group, index);
        }
    }
    // The next pass, empty: drop a group here to start it.
    let next = set.groups.iter().map(|g| g.pass).max().map_or(0, |m| m + 1);
    pass_header(ui, cx, set, next, true);
    ui.add_space(8.0);
    templates(ui, cx);
}

/// A `Pass n` header: a group dropped on it joins pass `n`, first in it
/// (one `set_rule_group` setting `pass` and the index); a rule dropped on
/// it goes first in the pass's first group (`move_rule`). `empty`: the
/// next pass, which has no group yet.
fn pass_header(ui: &mut egui::Ui, cx: &mut Cx<'_>, set: &RuleSet, pass: u32, empty: bool) {
    let text = if empty {
        RichText::new(format!("Pass {pass}: drag a group here to start it")).weak()
    } else {
        RichText::new(format!("Pass {pass}"))
            .strong()
            .color(cx.accent)
    };
    let resp = ui
        .horizontal(|ui| {
            ui.add_space(2.0);
            ui.label(text);
        })
        .response
        .on_hover_text(
            "Passes run in order: a pass sees the derived grids earlier passes wrote, its tiles \
             draw above theirs, and break locks end with the pass",
        );
    cx.r.pass_rects.insert(pass, resp.rect);
    let line = |ui: &egui::Ui| {
        ui.painter().hline(
            resp.rect.x_range(),
            resp.rect.bottom(),
            Stroke::new(2.0, cx.accent),
        );
    };
    if resp.dnd_hover_payload::<Drag>().is_some() {
        line(ui);
    }
    if let Some(p) = resp.dnd_release_payload::<Drag>() {
        match &*p {
            Drag::Group(moved) => {
                if let Some((def, index)) = group_into_pass(set, moved, pass) {
                    push(
                        cx.pending,
                        format!("Move rule group {moved} to pass {pass}"),
                        vec![Command::SetRuleGroup {
                            layer: cx.layer.clone(),
                            group: moved.clone(),
                            def,
                            index: Some(index),
                        }],
                    );
                }
            }
            Drag::Entry(rule) => {
                let first = run_order(set)
                    .into_iter()
                    .find(|&gi| set.groups[gi].pass == pass);
                if let Some(gi) = first {
                    let at = (!set.groups[gi].rules.is_empty()).then_some(0);
                    if let Some((group, index)) = drop_target(set, rule, gi, at, false) {
                        move_rule(cx, rule, group, index);
                    }
                }
            }
        }
    }
}

fn move_rule(cx: &mut Cx<'_>, rule: &Slug, group: Slug, index: usize) {
    push(
        cx.pending,
        format!("Move rule {rule} to {group}"),
        vec![Command::MoveRule {
            layer: cx.layer.clone(),
            rule: rule.clone(),
            group,
            index,
        }],
    );
}

fn group_header(ui: &mut egui::Ui, cx: &mut Cx<'_>, set: &RuleSet, gi: usize, g: &RuleGroup) {
    let layer = cx.layer.clone();
    let collapsed = cx.r.collapsed.contains(&g.slug);
    let mut members = Vec::new();
    for e in &g.rules {
        members.extend(cx.r.stats.members_of(e.slug()));
    }
    let (fires, matches) = cx.r.stats.count(&members);
    let frame = egui::Frame::NONE
        .fill(ui.visuals().faint_bg_color)
        .inner_margin(egui::Margin::symmetric(4, 2));
    let resp = frame
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                if ui
                    .small_button(if collapsed { "⏵" } else { "⏷" })
                    .on_hover_text("Collapse or expand the group")
                    .clicked()
                {
                    if collapsed {
                        cx.r.collapsed.remove(&g.slug);
                    } else {
                        cx.r.collapsed.insert(g.slug.clone());
                    }
                }
                let h = ui
                    .add(egui::Label::new(RichText::new("☰").weak()).sense(Sense::drag()))
                    .on_hover_text("Drag to reorder groups");
                if h.dragged() {
                    h.dnd_set_drag_payload(Drag::Group(g.slug.clone()));
                }
                cx.r.group_rects.insert(g.slug.clone(), h.rect);
                if let Some(c) = &g.color {
                    let c = parse_color(c.as_str());
                    let (r, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
                    ui.painter()
                        .rect_filled(r, 2.0, Color32::from_rgb(c[0], c[1], c[2]));
                }
                ui.strong(g.slug.as_str());
                let text = match matches {
                    Some(m) => format!("{fires} · {m}"),
                    None => format!("{fires}"),
                };
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .small_button("×")
                        .on_hover_text("Remove the group and its rules (undo brings it back)")
                        .clicked()
                    {
                        push(
                            cx.pending,
                            format!("Remove rule group {}", g.slug),
                            vec![Command::RemoveRuleGroup {
                                layer: layer.clone(),
                                group: g.slug.clone(),
                                keep_empty: true,
                            }],
                        );
                    }
                    if ui
                        .small_button("+ Region")
                        .on_hover_text(
                            "Add a region rule (fires at places in regions of cells) at the end of the group",
                        )
                        .clicked()
                    {
                        new_region_rule(cx, set, g);
                    }
                    if ui
                        .small_button("+ Rule")
                        .on_hover_text("Add a rule at the end of the group")
                        .clicked()
                    {
                        new_rule(cx, set, g);
                    }
                    ui.weak(text).on_hover_text(
                        "Cells where the group's rules fired · where their patterns match, in the active level",
                    );
                });
            });
            ui.horizontal(|ui| {
                ui.add_space(24.0);
                // Name: commit on Enter or focus loss.
                let key = format!("{layer}/{}:name", g.slug);
                if let Some(name) = text_field(
                    ui,
                    &mut cx.r.strs,
                    &key,
                    g.name.as_deref().unwrap_or(""),
                    80.0,
                    "name",
                ) {
                    let mut ng = g.clone();
                    ng.name = (!name.trim().is_empty()).then(|| name.trim().to_string());
                    push(
                        cx.pending,
                        format!("Group {}: name", g.slug),
                        vec![set_group(&layer, ng)],
                    );
                }
                let mut mode = g.mode;
                egui::ComboBox::from_id_salt(("group mode", &g.slug))
                    .width(70.0)
                    .selected_text(mode_name(mode))
                    .show_ui(ui, |ui| {
                        for m in [GroupMode::Always, GroupMode::Optional, GroupMode::Off] {
                            ui.selectable_value(&mut mode, m, mode_name(m));
                        }
                    });
                if mode != g.mode {
                    let mut ng = g.clone();
                    ng.mode = mode;
                    push(
                        cx.pending,
                        format!("Group {}: {}", g.slug, mode_name(mode)),
                        vec![set_group(&layer, ng)],
                    );
                }
                let key = format!("{layer}/{}:color", g.slug);
                if let Some(hex) = super::panels::color_field(
                    ui,
                    &mut cx.r.strs,
                    &key,
                    // No color yet: a neutral gray rather than black.
                    Some(g.color.as_ref().map_or("#707070", |c| c.as_str())),
                ) && let Ok(c) = Color::new(hex)
                {
                    let mut ng = g.clone();
                    ng.color = Some(c);
                    push(
                        cx.pending,
                        format!("Group {}: color", g.slug),
                        vec![set_group(&layer, ng)],
                    );
                }
                requirement_button(ui, cx, g);
                tag_button(ui, cx, g);
                let key = format!("{layer}/{}:pass", g.slug);
                if let Some(v) = num(
                    ui,
                    &mut cx.r.nums,
                    &key,
                    g.pass as f64,
                    0.05,
                    0.0..=99.0,
                    0,
                    "pass ",
                ) && let Some((ng, index)) = set_pass(set, &g.slug, v as u32)
                {
                    push(
                        cx.pending,
                        format!("Group {}: pass {}", g.slug, v as u32),
                        vec![Command::SetRuleGroup {
                            layer: layer.clone(),
                            group: g.slug.clone(),
                            def: ng,
                            index,
                        }],
                    );
                }
            });
        })
        .response;
    if let Some(p) = resp.dnd_hover_payload::<Drag>() {
        let after = ui
            .ctx()
            .pointer_hover_pos()
            .is_some_and(|p| p.y > resp.rect.center().y);
        let y = if matches!(*p, Drag::Entry(_)) || after {
            resp.rect.bottom()
        } else {
            resp.rect.top()
        };
        ui.painter()
            .hline(resp.rect.x_range(), y, Stroke::new(2.0, cx.accent));
    }
    if let Some(p) = resp.dnd_release_payload::<Drag>() {
        let after = ui
            .ctx()
            .pointer_interact_pos()
            .is_some_and(|p| p.y > resp.rect.center().y);
        match &*p {
            // A rule dropped on a header goes first in that group.
            Drag::Entry(rule) => {
                let first = (!g.rules.is_empty()).then_some(0);
                if let Some((group, index)) = drop_target(set, rule, gi, first, false) {
                    move_rule(cx, rule, group, index);
                }
            }
            // A group dropped next to a group of another pass joins that
            // pass: one `set_rule_group` with the pass and the place.
            Drag::Group(moved) if moved != &g.slug => {
                let now = set.groups.iter().position(|x| &x.slug == moved);
                let index = group_drop_index(set, moved, gi, after).or(now);
                if let (Some(index), Some(def)) =
                    (index, set.groups.iter().find(|x| &x.slug == moved))
                    && (Some(index) != now || def.pass != g.pass)
                {
                    let mut def = def.clone();
                    def.pass = g.pass;
                    push(
                        cx.pending,
                        format!("Move rule group {moved}"),
                        vec![Command::SetRuleGroup {
                            layer: layer.clone(),
                            group: moved.clone(),
                            def,
                            index: Some(index),
                        }],
                    );
                }
            }
            Drag::Group(_) => {}
        }
    }
}

fn mode_name(m: GroupMode) -> &'static str {
    match m {
        GroupMode::Always => "always",
        GroupMode::Optional => "optional",
        GroupMode::Off => "off",
    }
}

/// The group's `requires_level_field`, as a button with a popup editor.
fn requirement_button(ui: &mut egui::Ui, cx: &mut Cx<'_>, g: &RuleGroup) {
    let layer = cx.layer.clone();
    let label = match &g.requires_level_field {
        Some(r) => format!(
            "needs {} {} {}",
            r.field,
            if r.mode == RequirementMode::All {
                "all of"
            } else {
                "∈"
            },
            r.values
                .iter()
                .map(|v| v.as_str())
                .collect::<Vec<_>>()
                .join("|")
        ),
        None => "any level".into(),
    };
    let resp = ui
        .small_button(label)
        .on_hover_text("requires_level_field: run only in levels whose field holds these values");
    egui::Popup::from_toggle_button_response(&resp)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            ui.set_min_width(220.0);
            let fields = &cx.doc.project.level_fields;
            let cur = g.requires_level_field.clone();
            let mut field = cur.as_ref().map(|r| r.field.clone());
            egui::ComboBox::from_id_salt(("req field", &g.slug))
                .selected_text(field.as_ref().map_or("(none)", |f| f.as_str()))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut field, None, "(none)");
                    for f in fields {
                        ui.selectable_value(&mut field, Some(f.slug.clone()), f.slug.as_str());
                    }
                });
            let mut mode = cur.as_ref().map_or(RequirementMode::Any, |r| r.mode);
            ui.horizontal(|ui| {
                ui.selectable_value(&mut mode, RequirementMode::Any, "any");
                ui.selectable_value(&mut mode, RequirementMode::All, "all");
            });
            let key = format!("{layer}/{}:req values", g.slug);
            let shown = cur.as_ref().map_or(String::new(), |r| {
                r.values
                    .iter()
                    .map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            });
            ui.label("values (comma separated slugs):");
            let typed = text_field(ui, &mut cx.r.strs, &key, &shown, 200.0, "flooded, dry");
            let values: Vec<Slug> = typed
                .clone()
                .unwrap_or(shown)
                .split(',')
                .filter_map(|v| Slug::new(v.trim()).ok())
                .collect();
            let next = field.map(|field| LevelFieldRequirement {
                field,
                values,
                mode,
            });
            if next != cur {
                let mut ng = g.clone();
                ng.requires_level_field = next;
                push(
                    cx.pending,
                    format!("Group {}: level field requirement", g.slug),
                    vec![set_group(&layer, ng)],
                );
            }
        });
}

/// The group's `requires_level_tag`, as a button with a popup editor.
fn tag_button(ui: &mut egui::Ui, cx: &mut Cx<'_>, g: &RuleGroup) {
    let layer = cx.layer.clone();
    let label = match &g.requires_level_tag {
        Some(r) => format!(
            "tags {} {}",
            if r.mode == RequirementMode::All {
                "all of"
            } else {
                "any of"
            },
            r.tags.join("|")
        ),
        None => "any tags".into(),
    };
    let resp = ui
        .small_button(label)
        .on_hover_text("requires_level_tag: run only in levels with these tags");
    egui::Popup::from_toggle_button_response(&resp)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            ui.set_min_width(220.0);
            let cur = g.requires_level_tag.clone();
            let mut mode = cur.as_ref().map_or(RequirementMode::Any, |r| r.mode);
            ui.horizontal(|ui| {
                ui.selectable_value(&mut mode, RequirementMode::Any, "any");
                ui.selectable_value(&mut mode, RequirementMode::All, "all");
            });
            let key = format!("{layer}/{}:req tags", g.slug);
            let shown = cur.as_ref().map_or(String::new(), |r| r.tags.join(", "));
            ui.label("level tags (comma separated; empty: no requirement):");
            let typed = text_field(ui, &mut cx.r.strs, &key, &shown, 200.0, "ruined, flooded");
            let mut tags: Vec<String> = Vec::new();
            for t in typed.clone().unwrap_or(shown).split(',') {
                let t = t.trim();
                if !t.is_empty() && !tags.iter().any(|x| x == t) {
                    tags.push(t.to_string());
                }
            }
            let next = (!tags.is_empty()).then_some(LevelTagRequirement { tags, mode });
            if next != cur {
                let mut ng = g.clone();
                ng.requires_level_tag = next;
                push(
                    cx.pending,
                    format!("Group {}: level tag requirement", g.slug),
                    vec![set_group(&layer, ng)],
                );
            }
        });
}

/// Add a region rule at the end of group `g`: regions of empty cells,
/// placed along the floor, drawing the first tile of the tileset the layer
/// uses.
fn new_region_rule(cx: &mut Cx<'_>, set: &RuleSet, g: &RuleGroup) {
    let taken = |s: &str| find_entry(set, s).is_some() || cx.r.stats.members.contains_key(s);
    let slug = fresh_slug("region", &taken);
    let tileset = set
        .entries()
        .filter_map(|(_, e)| e.as_rule())
        .find_map(|r| r.out()?.as_tiles().map(|t| t.tileset.clone()))
        .or_else(|| cx.doc.project.tilesets.first().map(|t| t.slug.clone()));
    let Some(tileset) = tileset else {
        cx.status = Some("The project has no tilesets to draw from".into());
        return;
    };
    let Ok(cells) = CellMatch::try_from(".".to_string()) else {
        return;
    };
    let rule = Rule::region(
        slug.clone(),
        Region::new(cells),
        Place::new(PlaceAt::Along(Edge::Floor)),
        Output::Draw(RuleOutput::tiles(tileset, vec![TileAlt::single(0, 0)])),
    );
    push(
        cx.pending,
        format!("Add region rule {slug}"),
        vec![set_entry(&cx.layer, &g.slug, RuleEntry::Rule(rule))],
    );
    cx.r.selected = Some(slug);
}

fn new_rule(cx: &mut Cx<'_>, set: &RuleSet, g: &RuleGroup) {
    let taken = |s: &str| find_entry(set, s).is_some() || cx.r.stats.members.contains_key(s);
    let slug = fresh_slug("rule", &taken);
    // Draw from the tileset the group's (or the layer's) rules use most.
    let tileset = set
        .entries()
        .filter_map(|(_, e)| e.as_rule())
        .find_map(|r| r.out()?.as_tiles().map(|t| t.tileset.clone()))
        .or_else(|| cx.doc.project.tilesets.first().map(|t| t.slug.clone()));
    let Some(tileset) = tileset else {
        cx.status = Some("The project has no tilesets to draw from".into());
        return;
    };
    let rule = Rule::new(
        slug.clone(),
        Pattern::new(&["+"]),
        RuleOutput::tiles(tileset, vec![TileAlt::single(0, 0)]),
    );
    push(
        cx.pending,
        format!("Add rule {slug}"),
        vec![set_entry(&cx.layer, &g.slug, RuleEntry::Rule(rule))],
    );
    cx.r.selected = Some(slug);
}

/// Scroll the list to the selected row (`row`, drawn this frame) so that
/// its pass header and group header show above it. Nothing moves when they
/// are on screen already (scrolling a list that is only a little taller
/// than the pane would cut the rows at its top in half for nothing).
/// Otherwise the list's top edge goes to the `Pass n` header when the
/// header, the group header and the row fit in the view, else to the group
/// header, else the row is centred. Both headers were drawn earlier in this
/// frame, so their rects are this frame's.
fn reveal_row(ui: &egui::Ui, r: &RulesState, g: &RuleGroup, row: egui::Rect) {
    let clip = ui.clip_rect();
    let view = clip.height();
    let tops = [r.pass_rects.get(&g.pass), r.group_rects.get(&g.slug)];
    for top in tops.into_iter().flatten() {
        let span = top.union(row);
        if clip.contains_rect(span) {
            return;
        }
        if span.height() <= view {
            ui.scroll_to_rect_animation(
                span,
                Some(egui::Align::Min),
                egui::style::ScrollAnimation::none(),
            );
            return;
        }
    }
    ui.scroll_to_rect_animation(
        row,
        Some(egui::Align::Center),
        egui::style::ScrollAnimation::none(),
    );
}

fn entry_row(
    ui: &mut egui::Ui,
    cx: &mut Cx<'_>,
    set: &RuleSet,
    gi: usize,
    ei: usize,
    e: &RuleEntry,
) {
    let layer = cx.layer.clone();
    let g = &set.groups[gi];
    let slug = e.slug().clone();
    let row = ui.horizontal(|ui| {
        let h = ui
            .add(egui::Label::new(RichText::new("☰").weak()).sense(Sense::drag()))
            .on_hover_text("Drag to reorder, also into another group");
        if h.dragged() {
            h.dnd_set_drag_payload(Drag::Entry(slug.clone()));
        }
        let handle = h.rect;
        let mut active = e.active();
        if ui
            .checkbox(&mut active, "")
            .on_hover_text("Active")
            .changed()
        {
            let mut ne = e.clone();
            match &mut ne {
                RuleEntry::Rule(r) => r.active = active,
                RuleEntry::Use(u) => u.active = active,
            }
            push(
                cx.pending,
                format!("Rule {slug}: {}", if active { "on" } else { "off" }),
                vec![set_entry(&layer, &g.slug, ne)],
            );
        }
        let selected = cx.r.selected.as_ref() == Some(&slug) || cx.r.multi.contains(&slug);
        let text = match e {
            RuleEntry::Rule(_) => RichText::new(slug.as_str()),
            RuleEntry::Use(u) => RichText::new(format!("use {} as {}", u.template, u.slug())),
        };
        let text = if e.active() { text } else { text.weak() };
        let r = ui.selectable_label(selected, text);
        if cx.r.reveal && cx.r.selected.as_ref() == Some(&slug) {
            reveal_row(ui, cx.r, g, r.rect);
            cx.r.reveal = false;
        }
        if r.clicked() {
            if ui.input(|i| i.modifiers.command) && e.as_rule().is_some() {
                if !cx.r.multi.remove(&slug) {
                    cx.r.multi.insert(slug.clone());
                }
            } else {
                cx.r.multi.clear();
                cx.r.selected = Some(slug.clone());
            }
        }
        match e {
            RuleEntry::Rule(rule) => {
                if let Some(m) = rule.as_match() {
                    mini_pattern(ui, cx.doc, &layer, &m.pattern);
                }
                if let Some(r) = rule.as_region() {
                    ui.weak(format!("region {}", region_summary(r)));
                }
                if let Some((ts, tile)) = rule.out().and_then(|o| first_tile(&set.stamps, o)) {
                    tile_thumb(ui, cx, &ts, tile, 18.0);
                }
                if let Some(w) = rule.write() {
                    write_chip(ui, cx.doc, w);
                }
            }
            RuleEntry::Use(u) => {
                let n = cx.r.stats.members.get(&slug).map_or(0, Vec::len);
                ui.weak(format!(
                    "{} @ [{}, {}]",
                    u.tileset.as_ref().map_or("(own tilesets)", |t| t.as_str()),
                    u.origin[0],
                    u.origin[1]
                ));
                let open = cx.r.expanded.contains(&slug);
                if ui
                    .small_button(format!("{} {n} rules", if open { "⏷" } else { "⏵" }))
                    .on_hover_text("Show the rules it expands to")
                    .clicked()
                {
                    if open {
                        cx.r.expanded.remove(&slug);
                    } else {
                        cx.r.expanded.insert(slug.clone());
                    }
                }
                if ui
                    .small_button("Detach")
                    .on_hover_text(
                        "Replace the reference by its rules, keeping their slugs (no re-roll)",
                    )
                    .clicked()
                {
                    match detach_commands(&cx.doc.project, &layer, &slug) {
                        Ok(cmds) => push(cx.pending, format!("Detach {slug}"), cmds),
                        Err(e) => cx.status = Some(e),
                    }
                }
            }
        }
        ui.weak(counts_text(cx.r, &slug));
        handle
    });
    let resp = row.response;
    cx.r.row_rects.insert(slug.clone(), (resp.rect, row.inner));
    if let Some(p) = resp.dnd_hover_payload::<Drag>()
        && matches!(*p, Drag::Entry(_))
    {
        let after = ui
            .ctx()
            .pointer_hover_pos()
            .is_some_and(|p| p.y > resp.rect.center().y);
        let y = if after {
            resp.rect.bottom()
        } else {
            resp.rect.top()
        };
        ui.painter()
            .hline(resp.rect.x_range(), y, Stroke::new(2.0, cx.accent));
    }
    if let Some(p) = resp.dnd_release_payload::<Drag>()
        && let Drag::Entry(rule) = &*p
    {
        let after = ui
            .ctx()
            .pointer_interact_pos()
            .is_some_and(|p| p.y > resp.rect.center().y);
        if let Some((group, index)) = drop_target(set, rule, gi, Some(ei), after) {
            move_rule(cx, rule, group, index);
        }
    }
    // A reference's expansion: read-only rows with skip toggles.
    if let RuleEntry::Use(u) = e
        && cx.r.expanded.contains(&slug)
        && let Some(t) = cx.doc.project.template(u.template.as_str())
    {
        ui.indent(("expansion", &slug), |ui| {
            for tr in &t.rules {
                let expanded = Slug::new(format!("{}__{}", u.slug(), tr.slug)).ok();
                ui.horizontal(|ui| {
                    let mut on = !u.skip.contains(&tr.slug);
                    if ui
                        .checkbox(&mut on, "")
                        .on_hover_text("Unticked: in the reference's skip list")
                        .changed()
                    {
                        let mut nu = u.clone();
                        if on {
                            nu.skip.retain(|s| *s != tr.slug);
                        } else {
                            nu.skip.push(tr.slug.clone());
                        }
                        push(
                            cx.pending,
                            format!(
                                "Reference {slug}: {} {}",
                                if on { "use" } else { "skip" },
                                tr.slug
                            ),
                            vec![set_entry(&layer, &g.slug, RuleEntry::Use(nu))],
                        );
                    }
                    let Some(x) = expanded else {
                        ui.label(tr.slug.as_str());
                        return;
                    };
                    let sel = cx.r.selected.as_ref() == Some(&x);
                    // Weak when not selected; selected, the selection's own
                    // text colour (weak text on the selection fill is unreadable).
                    let text = RichText::new(x.as_str());
                    let row = ui.selectable_label(sel, if sel { text } else { text.weak() });
                    if cx.r.reveal && sel {
                        reveal_row(ui, cx.r, g, row.rect);
                        cx.r.reveal = false;
                    }
                    cx.r.row_rects.insert(x.clone(), (row.rect, row.rect));
                    if row.clicked() {
                        cx.r.selected = Some(x.clone());
                        cx.r.multi.clear();
                    }
                    if !u.skip.contains(&tr.slug) {
                        ui.weak(counts_text(cx.r, &x));
                    }
                });
            }
        });
    }
}

/// `→ pillar_marks = p` with the value's color.
fn write_chip(ui: &mut egui::Ui, doc: &Document, w: &Write) {
    let def = doc.project.layer(w.grid.as_str());
    let v = def.and_then(|d| d.value(w.value.as_str()));
    ui.weak(format!("writes {}", w.grid));
    if let Some(v) = v {
        let c = parse_color(v.color.as_str());
        let (r, _) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
        ui.painter()
            .rect_filled(r, 2.0, Color32::from_rgb(c[0], c[1], c[2]));
        ui.weak(v.slug.as_str());
    } else {
        ui.weak(w.value.as_str());
    }
}

/// The first tile a rule output draws (a stamp's top-left tile).
fn first_tile(stamps: &[worlds_core::Stamp], out: &RuleOutput) -> Option<(Slug, [u32; 2])> {
    match out {
        RuleOutput::Tiles(t) => Some((
            t.tileset.clone(),
            *t.tiles.iter().find_map(|a| a.tiles.first())?,
        )),
        RuleOutput::Stamp(s) => stamps
            .iter()
            .find(|st| st.slug == s.stamp)
            .map(|st| (st.tileset.clone(), [st.rect[0], st.rect[1]])),
    }
}

/// The int_grid layer a pattern reads.
fn pattern_grid<'a>(doc: &'a Document, layer: &Slug, p: &Pattern) -> Option<&'a LayerDef> {
    let source = doc.project.layer(layer.as_str())?.auto()?.source.clone();
    let g = p.grid.clone().unwrap_or(source);
    doc.project.layer(g.as_str())
}

/// Rows of a pattern, named ones resolved from macros.json.
fn resolved_rows(doc: &Document, p: &Pattern) -> Vec<String> {
    match &p.rows {
        Rows::Inline(r) => r.clone(),
        Rows::Named(n) => doc
            .project
            .macros
            .as_ref()
            .and_then(|m| m.patterns.get(n))
            .cloned()
            .unwrap_or_default(),
    }
}

/// A pattern with a named legend resolved (for colors and meanings).
fn resolved_pattern(doc: &Document, p: &Pattern) -> Pattern {
    let mut out = p.clone();
    out.rows = Rows::Inline(resolved_rows(doc, p));
    if let Legend::Named(n) = &p.legend {
        out.legend = Legend::Inline(
            doc.project
                .macros
                .as_ref()
                .and_then(|m| m.legends.get(n))
                .cloned()
                .unwrap_or_default(),
        );
    }
    out
}

/// Fill color of a cell test, and whether it is negated.
fn test_color(m: Option<&CellMatch>, grid: Option<&LayerDef>) -> (Color32, bool) {
    let slug_color = |s: &Slug| -> Color32 {
        let c = grid
            .and_then(|g| {
                g.values()
                    .iter()
                    .find(|v| v.slug == *s)
                    .map(|v| v.color.as_str().to_string())
                    .or_else(|| {
                        g.groups().iter().find(|x| x.slug == *s).map(|x| {
                            x.color
                                .as_ref()
                                .map_or("#707070", |c| c.as_str())
                                .to_string()
                        })
                    })
            })
            .map_or([200, 60, 200, 255], |c| parse_color(&c));
        Color32::from_rgb(c[0], c[1], c[2])
    };
    let first = |v: &[worlds_core::CellTest]| -> Color32 {
        v.iter()
            .find_map(|t| match t {
                worlds_core::CellTest::Is(s) => Some(slug_color(s)),
                worlds_core::CellTest::NonEmpty => Some(Color32::from_gray(150)),
                worlds_core::CellTest::Empty => None,
            })
            .unwrap_or(Color32::from_gray(25))
    };
    match m {
        None => (Color32::from_rgb(200, 40, 40), false),
        Some(CellMatch::Any) => (Color32::from_gray(48), false),
        Some(CellMatch::Empty) => (Color32::from_gray(16), false),
        Some(CellMatch::NonEmpty) => (Color32::from_gray(150), false),
        Some(CellMatch::Is(s)) => (slug_color(s), false),
        Some(CellMatch::Not(s)) => (slug_color(s), true),
        Some(CellMatch::OneOf(v)) => (first(v), false),
        Some(CellMatch::NotOneOf(v)) => (first(v), true),
    }
}

/// The small colored pattern in a list row.
fn mini_pattern(ui: &mut egui::Ui, doc: &Document, layer: &Slug, p: &Pattern) {
    let p = resolved_pattern(doc, p);
    let grid = pattern_grid(doc, layer, &p);
    let rows = p.inline_rows();
    let [w, h] = p.size();
    let cell = (18.0 / h.max(1) as f32).clamp(2.0, 6.0);
    let (rect, resp) =
        ui.allocate_exact_size(Vec2::new(w as f32 * cell, h as f32 * cell), Sense::hover());
    let painter = ui.painter_at(rect);
    let (ax, ay) = p.anchor();
    for (y, row) in rows.iter().enumerate() {
        for (x, g) in row.chars().enumerate() {
            let r = egui::Rect::from_min_size(
                rect.min + Vec2::new(x as f32 * cell, y as f32 * cell),
                Vec2::splat(cell),
            );
            let (c, neg) = test_color(p.resolve(g, grid).as_ref(), grid);
            painter.rect_filled(r, 0.0, if neg { c.gamma_multiply(0.35) } else { c });
            if (x as u32, y as u32) == (ax, ay) {
                painter.rect_stroke(r, 0.0, Stroke::new(1.0, Color32::WHITE), StrokeKind::Inside);
            }
        }
    }
    resp.on_hover_text(rows.join("\n"));
}

/// One atlas tile, drawn `size` points wide.
fn tile_thumb(ui: &mut egui::Ui, cx: &mut Cx<'_>, tileset: &Slug, tile: [u32; 2], size: f32) {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    paint_tile(ui, cx, tileset, tile, rect);
    resp.on_hover_text(format!("{tileset} [{}, {}]", tile[0], tile[1]));
}

fn paint_tile(ui: &egui::Ui, cx: &mut Cx<'_>, tileset: &Slug, tile: [u32; 2], rect: egui::Rect) {
    let Some(ts) = cx.doc.project.tileset(tileset.as_str()) else {
        ui.painter()
            .rect_filled(rect, 0.0, Color32::from_rgb(200, 0, 200));
        return;
    };
    let Some(tex) = super::panels::atlas_texture(cx.atlas, ui.ctx(), cx.doc, ts) else {
        return;
    };
    let size = tex.size_vec2();
    let [x, y] = ts.tile_px(tile);
    let [w, h] = ts.tile_size;
    let uv = egui::Rect::from_min_max(
        egui::pos2(x as f32 / size.x, y as f32 / size.y),
        egui::pos2((x + w) as f32 / size.x, (y + h) as f32 / size.y),
    );
    ui.painter().image(tex.id(), rect, uv, Color32::WHITE);
}

// ---- the template library ----

fn templates(ui: &mut egui::Ui, cx: &mut Cx<'_>) {
    let doc = cx.doc;
    let builtin = worlds_core::Macros::builtin_templates();
    let own = doc.project.macros.as_ref().map(|m| &m.templates);
    let n = builtin.len() + own.map_or(0, BTreeMap::len);
    egui::CollapsingHeader::new(format!("Templates ({n})"))
        .id_salt("template library")
        .default_open(false)
        .show(ui, |ui| {
            let all = own
                .into_iter()
                .flatten()
                .map(|(k, t)| (k, t, false))
                .chain(builtin.iter().map(|(k, t)| (k, t, true)));
            for (name, t, is_builtin) in all {
                ui.horizontal(|ui| {
                    ui.strong(name.as_str());
                    if is_builtin {
                        ui.weak("built-in");
                    }
                    ui.weak(format!("{} rules", t.rules.len()));
                    if ui.small_button("Apply…").clicked() {
                        cx.r.apply = Some(ApplyDialog {
                            template: name.clone(),
                            slug: name.to_string(),
                            tileset: cx.doc.project.tilesets.first().map(|t| t.slug.clone()),
                            origin: [0, 0],
                            group: None,
                            index: None,
                            error: None,
                        });
                    }
                    if is_builtin
                        && ui
                            .small_button("Copy into macros.json")
                            .on_hover_text("A copy you can change (built-ins are frozen)")
                            .clicked()
                    {
                        let taken = |s: &str| {
                            worlds_core::Macros::is_builtin(s)
                                || own.is_some_and(|o| o.contains_key(s))
                        };
                        let copy = fresh_slug(&format!("{name}_copy"), &taken);
                        push(
                            cx.pending,
                            format!("Copy template {name} into {copy}"),
                            vec![Command::SetMacro {
                                kind: worlds_core::command::MacroKind::Template,
                                name: copy,
                                def: worlds_core::command::MacroDef::Template(t.clone()),
                            }],
                        );
                    }
                });
                let doc_text = t.name.iter().chain(&t.doc).cloned().collect::<Vec<_>>();
                if !doc_text.is_empty() {
                    ui.indent(("tdoc", name), |ui| {
                        ui.weak(doc_text.join(": "));
                    });
                }
            }
        });
}

/// Where a template reference with this tileset and origin draws: atlas
/// tile -> the template rules (1-based) that draw it.
fn template_pieces(t: &Template, origin: [u32; 2]) -> BTreeMap<[u32; 2], Vec<usize>> {
    let mut out: BTreeMap<[u32; 2], Vec<usize>> = BTreeMap::new();
    for (i, r) in t.rules.iter().enumerate() {
        if let TemplateOutput::Tiles(o) = &r.out
            && o.tileset.is_none()
        {
            for alt in &o.tiles {
                for c in &alt.tiles {
                    let at = [c[0] + origin[0], c[1] + origin[1]];
                    let v = out.entry(at).or_default();
                    if !v.contains(&(i + 1)) {
                        v.push(i + 1);
                    }
                }
            }
        }
    }
    out
}

fn apply_dialog(ctx: &egui::Context, cx: &mut Cx<'_>) {
    let Some(mut d) = cx.r.apply.take() else {
        return;
    };
    let doc = cx.doc;
    let set = doc.project.rules_for(cx.layer.as_str());
    let mut open = true;
    let mut done = false;
    egui::Window::new(format!("Apply template {}", d.template))
        .id(egui::Id::new("apply template"))
        .open(&mut open)
        .collapsible(false)
        .default_width(520.0)
        .show(ctx, |ui| {
            let Some(t) = doc.project.template(d.template.as_str()) else {
                ui.label("The template is gone.");
                return;
            };
            if let Some(text) = &t.doc {
                ui.weak(text);
            }
            egui::Grid::new("apply grid").num_columns(2).show(ui, |ui| {
                ui.label("Reference slug");
                ui.text_edit_singleline(&mut d.slug);
                ui.end_row();
                ui.label("Tileset");
                tileset_combo(ui, doc, "apply tileset", &mut d.tileset);
                ui.end_row();
                ui.label("Origin");
                ui.horizontal(|ui| {
                    ui.add(egui::DragValue::new(&mut d.origin[0]).prefix("col "));
                    ui.add(egui::DragValue::new(&mut d.origin[1]).prefix("row "));
                    ui.weak("(or click the atlas)");
                });
                ui.end_row();
                ui.label("Group");
                let groups: Vec<Slug> = set
                    .map(|s| s.groups.iter().map(|g| g.slug.clone()).collect())
                    .unwrap_or_default();
                if d.group.is_none() {
                    d.group = groups.first().cloned();
                }
                egui::ComboBox::from_id_salt("apply group")
                    .selected_text(
                        d.group
                            .as_ref()
                            .map_or("(new group \"rules\")", |g| g.as_str()),
                    )
                    .show_ui(ui, |ui| {
                        for g in &groups {
                            ui.selectable_value(&mut d.group, Some(g.clone()), g.as_str());
                        }
                    });
                ui.end_row();
                ui.label("Index");
                let len = d
                    .group
                    .as_ref()
                    .and_then(|g| set?.groups.iter().find(|x| x.slug == *g))
                    .map_or(0, |g| g.rules.len());
                let mut i = d.index.unwrap_or(len).min(len);
                ui.add(egui::DragValue::new(&mut i).range(0..=len))
                    .on_hover_text("Position in the group (0 = first, evaluated first)");
                d.index = Some(i);
                ui.end_row();
            });
            if let Some(ts) = d
                .tileset
                .as_ref()
                .and_then(|s| doc.project.tileset(s.as_str()))
            {
                let pieces = template_pieces(t, d.origin);
                ui.weak("The template's tiles at this origin (number = its rule):");
                if let Some(clicked) = atlas_view(
                    ui,
                    cx,
                    ts,
                    &pieces
                        .iter()
                        .map(|(k, v)| (*k, v.first().copied().unwrap_or(0)))
                        .collect(),
                    "apply atlas",
                ) {
                    d.origin = clicked.0;
                }
            }
            if let Some(e) = &d.error {
                ui.colored_label(Color32::from_rgb(230, 120, 120), e);
            }
            ui.horizontal(|ui| {
                if ui.button("Apply").clicked() {
                    match Slug::new(d.slug.trim()) {
                        Ok(slug) => {
                            let mut u = TemplateRef::new(d.template.clone());
                            u.slug = (slug != d.template).then_some(slug.clone());
                            u.tileset = d.tileset.clone();
                            u.origin = d.origin;
                            let group = d
                                .group
                                .clone()
                                .unwrap_or_else(|| worlds_core::slug("rules"));
                            let mut cmds = Vec::new();
                            if set.is_none_or(|s| !s.groups.iter().any(|g| g.slug == group)) {
                                cmds.push(set_group(
                                    &cx.layer,
                                    RuleGroup::new(group.clone(), Vec::new()),
                                ));
                            }
                            cmds.push(Command::SetRule {
                                layer: cx.layer.clone(),
                                group,
                                rule: slug.clone(),
                                def: RuleEntry::Use(u),
                                index: d.index,
                            });
                            push(
                                cx.pending,
                                format!("Apply template {} as {slug}", d.template),
                                cmds,
                            );
                            cx.r.selected = Some(slug);
                            done = true;
                        }
                        Err(e) => d.error = Some(format!("Reference slug: {e}")),
                    }
                }
                if ui.button("Cancel").clicked() {
                    done = true;
                }
            });
        });
    if open && !done {
        cx.r.apply = Some(d);
    }
}

fn save_template_dialog(ctx: &egui::Context, cx: &mut Cx<'_>) {
    let Some(mut d) = cx.r.save.take() else {
        return;
    };
    let doc = cx.doc;
    let Some(set) = doc.project.rules_for(cx.layer.as_str()) else {
        return;
    };
    let rules: Vec<&Rule> = set
        .entries()
        .filter_map(|(_, e)| e.as_rule())
        .filter(|r| d.rules.contains(&r.slug))
        .collect();
    if rules.is_empty() {
        return;
    }
    let slugs: Vec<&Slug> = rules.iter().map(|r| &r.slug).collect();
    let prefix = common_prefix(&slugs);
    let mut open = true;
    let mut done = false;
    egui::Window::new("Save as template")
        .id(egui::Id::new("save template"))
        .open(&mut open)
        .collapsible(false)
        .show(ctx, |ui| {
            ui.label(format!(
                "{} rule{}: {}",
                rules.len(),
                if rules.len() == 1 { "" } else { "s" },
                d.rules
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            let tileset = common_tileset(&rules).cloned();
            egui::Grid::new("save grid").num_columns(2).show(ui, |ui| {
                ui.label("Template name");
                ui.text_edit_singleline(&mut d.name);
                ui.end_row();
                if tileset.is_some() {
                    ui.label("Origin");
                    ui.horizontal(|ui| {
                        ui.add(egui::DragValue::new(&mut d.origin[0]).prefix("col "));
                        ui.add(egui::DragValue::new(&mut d.origin[1]).prefix("row "));
                    });
                    ui.end_row();
                }
                ui.label("Replace by a reference");
                ui.checkbox(&mut d.replace, "");
                ui.end_row();
                if d.replace {
                    ui.label("Reference slug");
                    ui.text_edit_singleline(&mut d.reference);
                    ui.end_row();
                }
            });
            match &tileset {
                Some(t) => ui.weak(format!(
                    "Tiles become relative to the origin; the reference draws from {t}."
                )),
                None => ui.weak("The rules draw from several tilesets: each keeps its own."),
            };
            if d.replace && prefix.as_deref() != Some(d.reference.trim()) {
                ui.weak(
                    "The expanded rules get new slugs (<reference>__<rule>), so their random choices re-roll.",
                );
            }
            if let Some(e) = &d.error {
                ui.colored_label(Color32::from_rgb(230, 120, 120), e);
            }
            ui.horizontal(|ui| {
                if ui.button("Save").clicked() {
                    match save_template_commands(
                        &doc.project,
                        &cx.layer,
                        &d.rules,
                        &d.name,
                        &d.reference,
                        d.origin,
                        d.replace,
                    ) {
                        Ok((label, cmds)) => {
                            push(cx.pending, label, cmds);
                            if d.replace {
                                cx.r.selected = Slug::new(d.reference.trim()).ok();
                            }
                            cx.r.multi.clear();
                            done = true;
                        }
                        Err(e) => d.error = Some(e),
                    }
                }
                if ui.button("Cancel").clicked() {
                    done = true;
                }
            });
        });
    if open && !done {
        cx.r.save = Some(d);
    }
}

/// The commands "Save as template" sends: `set_macro` of a template made
/// from `rules` (inline rules of `layer`, in file order) with tiles
/// relative to `origin`; with `replace`, also their removal and a reference
/// `reference` where they were. Replacing needs the rules to be one run of
/// one group (so the reference keeps their priority and their group's
/// conditions), and a reference slug no other entry of the layer has. When every rule is
/// `<reference>__<x>` (what Detach leaves), the template rules are named
/// `x`, so the reference expands to the same slugs and nothing re-rolls.
/// Returns the undo label and the commands (one batch).
pub fn save_template_commands(
    project: &worlds_core::Project,
    layer: &Slug,
    rules: &[Slug],
    name: &str,
    reference: &str,
    origin: [u32; 2],
    replace: bool,
) -> Result<(String, Vec<Command>), String> {
    let set = project
        .rules_for(layer.as_str())
        .ok_or_else(|| format!("layer {layer} has no rules"))?;
    let chosen: Vec<&Rule> = set
        .entries()
        .filter_map(|(_, e)| e.as_rule())
        .filter(|r| rules.contains(&r.slug))
        .collect();
    if chosen.is_empty() {
        return Err("no inline rules selected".into());
    }
    let name = Slug::new(name.trim()).map_err(|e| format!("Template name: {e}"))?;
    let reference = Slug::new(reference.trim()).map_err(|e| format!("Reference slug: {e}"))?;
    let slugs: Vec<&Slug> = chosen.iter().map(|r| &r.slug).collect();
    let prefix = common_prefix(&slugs);
    let strip =
        (replace && prefix.as_deref() == Some(reference.as_str())).then_some(reference.as_str());
    let (template, tileset) = template_from_rules(&chosen, origin, strip)?;
    let mut cmds = vec![Command::SetMacro {
        kind: worlds_core::command::MacroKind::Template,
        name: name.clone(),
        def: worlds_core::command::MacroDef::Template(template),
    }];
    if !replace {
        return Ok((format!("Save template {name}"), cmds));
    }
    // The reference takes the rules' place: that is only the same order
    // and the same group gating if they are one run of one group.
    let at: Vec<(usize, usize)> = chosen
        .iter()
        .map(|r| find_entry(set, r.slug.as_str()).ok_or("rule not found"))
        .collect::<Result<_, _>>()?;
    let (gi, ei) = at[0];
    let last = at[at.len() - 1].1;
    if at.iter().any(|&(g, _)| g != gi) {
        return Err(
            "To replace rules by a reference, select rules of one group: the reference goes in \
             one group, so rules from another would change priority and group conditions"
                .into(),
        );
    }
    if let Some(e) = set.groups[gi].rules[ei..=last]
        .iter()
        .find(|e| !rules.contains(e.slug()))
    {
        return Err(format!(
            "To replace rules by a reference, select rules next to each other: {} sits between \
             them, and would end up after all of them",
            e.slug()
        ));
    }
    // A reference slug an entry outside the selection has would replace
    // that entry (or be refused in another group).
    if let Some((g, e)) = set
        .entries()
        .find(|(_, e)| *e.slug() == reference && !rules.contains(e.slug()))
    {
        return Err(format!(
            "Reference slug: {reference} is already {} {} in group {}; pick another",
            if e.as_use().is_some() {
                "the reference"
            } else {
                "the rule"
            },
            e.slug(),
            g.slug
        ));
    }
    let group = &set.groups[gi];
    let index = ei;
    for r in &chosen {
        cmds.push(Command::RemoveRule {
            layer: layer.clone(),
            rule: r.slug.clone(),
        });
    }
    let mut u = TemplateRef::new(name.clone());
    u.slug = (reference != name).then_some(reference.clone());
    if tileset.is_some() {
        u.tileset = tileset;
        u.origin = origin;
    }
    cmds.push(Command::SetRule {
        layer: layer.clone(),
        group: group.slug.clone(),
        rule: reference.clone(),
        def: RuleEntry::Use(u),
        index: Some(index),
    });
    Ok((format!("Save template {name} (as {reference})"), cmds))
}

// ---- the editor (right side) ----

fn editor(ui: &mut egui::Ui, cx: &mut Cx<'_>) {
    let doc = cx.doc;
    let layer = cx.layer.clone();
    let Some(sel) = cx.r.selected.clone() else {
        ui.weak("Select a rule to edit it. Click a pattern cell to cycle its glyph, right-click for *, Alt+click for the anchor @.");
        return;
    };
    let Some(set) = doc.project.rules_for(layer.as_str()) else {
        return;
    };
    if let Some((gi, ei)) = find_entry(set, sel.as_str()) {
        let g = &set.groups[gi];
        match &g.rules[ei] {
            RuleEntry::Rule(rule) => rule_editor(ui, cx, set, g, rule),
            RuleEntry::Use(u) => reference_editor(ui, cx, g, u),
        }
        return;
    }
    // An expanded template rule: find its reference.
    let Some((ref_slug, rule)) = sel.as_str().rsplit_once("__") else {
        ui.weak(format!("{sel} is gone."));
        return;
    };
    let Some((gi, ei)) = find_entry(set, ref_slug) else {
        ui.weak(format!("{sel} is gone."));
        return;
    };
    let g = &set.groups[gi];
    let Some(u) = g.rules[ei].as_use() else {
        return;
    };
    expanded_editor(ui, cx, g, u, &sel, rule);
}

fn rule_editor(ui: &mut egui::Ui, cx: &mut Cx<'_>, set: &RuleSet, g: &RuleGroup, rule: &Rule) {
    let layer = cx.layer.clone();
    let slug = rule.slug.clone();
    ui.horizontal_wrapped(|ui| {
        ui.heading(slug.as_str());
        ui.weak(format!("in {}", g.slug));
        ui.label(RichText::new(counts_text(cx.r, &slug)).color(cx.accent));
        {
            if ui
                .button("Delete")
                .on_hover_text("Remove the rule (undo brings it back)")
                .clicked()
            {
                push(
                    cx.pending,
                    format!("Remove rule {slug}"),
                    vec![Command::RemoveRule {
                        layer: layer.clone(),
                        rule: slug.clone(),
                    }],
                );
                cx.r.selected = None;
            }
        }
    });
    let commit = |cx: &mut Cx<'_>, label: &str, r: Rule| {
        push(
            cx.pending,
            format!("Rule {}: {label}", r.slug),
            vec![set_entry(&cx.layer, &g.slug, RuleEntry::Rule(r))],
        );
    };
    if rule.is_region() {
        if let Some((label, r)) = region_editor(ui, cx, rule) {
            commit(cx, &label, r);
        }
        ui.add_space(4.0);
        ui.strong("Options");
        if let Some((label, r)) = region_options(ui, rule) {
            commit(cx, &label, r);
        }
        ui.add_space(4.0);
        if let Some((label, r)) = output_section(ui, cx, set, rule) {
            commit(cx, &label, r);
        }
        return;
    }
    let Some(m) = rule.as_match() else {
        return;
    };
    ui.strong("Pattern");
    if let Some(p) = pattern_editor(ui, cx, &format!("{slug}:match"), &m.pattern, true) {
        let mut r = rule.clone();
        sel(&mut r).pattern = p;
        commit(cx, "pattern", r);
    }
    egui::CollapsingHeader::new(match m.require.first() {
        Some(_) => "require (also must hold)",
        None => "require (none)",
    })
    .id_salt(("require", &slug))
    .default_open(false)
    .show(ui, |ui| match m.require.first() {
        Some(req) => {
            if let Some(p) = pattern_editor(ui, cx, &format!("{slug}:require"), req, true) {
                let mut r = rule.clone();
                sel(&mut r).require[0] = p;
                commit(cx, "require", r);
            }
            if ui.button("Remove require").clicked() {
                let mut r = rule.clone();
                sel(&mut r).require.remove(0);
                commit(cx, "no require", r);
            }
        }
        None => {
            ui.weak("A second pattern, anchored on the same cell, usually on another grid.");
            if ui.button("Add require").clicked() {
                let mut r = rule.clone();
                sel(&mut r).require.push(Pattern::new(&["*"]));
                commit(cx, "require", r);
            }
        }
    });
    ui.add_space(4.0);
    ui.strong("Options");
    if let Some((label, r)) = options(ui, cx, rule) {
        commit(cx, &label, r);
    }
    ui.add_space(4.0);
    if let Some((label, r)) = output_section(ui, cx, set, rule) {
        commit(cx, &label, r);
    }
}

/// "Output": Draw (tiles or a stamp, [`output_editor`]) or Write (a
/// derived grid and value). Switching kinds is one edit.
fn output_section(
    ui: &mut egui::Ui,
    cx: &mut Cx<'_>,
    set: &RuleSet,
    rule: &Rule,
) -> Option<(String, Rule)> {
    let slug = rule.slug.clone();
    let mut res: Option<(String, Rule)> = None;
    let writes = rule.write().is_some();
    ui.horizontal(|ui| {
        ui.strong("Output");
        if ui
            .selectable_label(!writes, "Draw")
            .on_hover_text("Draw tiles or a stamp")
            .clicked()
            && writes
        {
            let tileset = set
                .entries()
                .filter_map(|(_, e)| e.as_rule())
                .find_map(|r| r.out()?.as_tiles().map(|t| t.tileset.clone()))
                .or_else(|| cx.doc.project.tilesets.first().map(|t| t.slug.clone()));
            match tileset {
                Some(ts) => {
                    let mut r = rule.clone();
                    r.output = Output::Draw(RuleOutput::tiles(ts, vec![TileAlt::single(0, 0)]));
                    res = Some(("draw".into(), r));
                }
                None => cx.status = Some("The project has no tilesets to draw from".into()),
            }
        }
        if ui
            .selectable_label(writes, "Write")
            .on_hover_text("Write a value into a derived int_grid layer (marks other rules read)")
            .clicked()
            && !writes
        {
            match derived_targets(cx.doc, &cx.layer).first() {
                Some((grid, value)) => {
                    let mut r = rule.clone();
                    r.output = Output::Write(Write {
                        grid: grid.clone(),
                        value: value.clone(),
                    });
                    res = Some(("write".into(), r));
                }
                None => {
                    cx.status = Some(
                        "No derived int_grid layer with a value on this layer's grid to write to: \
                         add one (\"derived\": true) first"
                            .into(),
                    )
                }
            }
        }
    });
    match &rule.output {
        Output::Draw(out) => {
            if let Some((label, out)) = output_editor(ui, cx, set, &slug, out) {
                let mut r = rule.clone();
                r.output = Output::Draw(out);
                res = Some((label, r));
            }
        }
        Output::Write(w) => {
            let doc = cx.doc;
            ui.horizontal(|ui| {
                ui.label("grid");
                let mut grid = w.grid.clone();
                egui::ComboBox::from_id_salt((&slug, "write grid"))
                    .selected_text(grid.as_str())
                    .show_ui(ui, |ui| {
                        for d in doc.project.layers.iter().filter(|d| d.is_derived()) {
                            ui.selectable_value(&mut grid, d.slug.clone(), d.slug.as_str());
                        }
                    });
                let def = doc.project.layer(grid.as_str());
                let mut value = if grid == w.grid {
                    w.value.clone()
                } else {
                    def.and_then(|d| d.values().first())
                        .map_or(w.value.clone(), |v| v.slug.clone())
                };
                ui.label("value");
                egui::ComboBox::from_id_salt((&slug, "write value"))
                    .selected_text(value.as_str())
                    .show_ui(ui, |ui| {
                        for v in def.map_or(&[][..], |d| d.values()) {
                            ui.selectable_value(&mut value, v.slug.clone(), v.slug.as_str());
                        }
                    });
                if grid != w.grid || value != w.value {
                    let mut r = rule.clone();
                    r.output = Output::Write(Write { grid, value });
                    res = Some(("write".into(), r));
                }
            });
            ui.weak(
                "The first write to a cell in a pass wins; later passes and other layers read the \
                 grid's effective values (overrides painted on it win).",
            );
        }
    }
    res
}

/// Derived int_grid layers on `layer`'s grid with their first value: what a
/// Write output can name.
fn derived_targets(doc: &Document, layer: &Slug) -> Vec<(Slug, Slug)> {
    let grid = doc
        .project
        .layer(layer.as_str())
        .map(|d| doc.project.layer_grid(d));
    doc.project
        .layers
        .iter()
        .filter(|d| d.is_derived() && Some(doc.project.layer_grid(d)) == grid)
        .filter_map(|d| Some((d.slug.clone(), d.values().first()?.slug.clone())))
        .collect()
}

/// A region rule's own options: break and salt.
fn region_options(ui: &mut egui::Ui, rule: &Rule) -> Option<(String, Rule)> {
    let mut out = None;
    ui.horizontal(|ui| {
        let mut b = rule.break_on_match;
        if ui
            .checkbox(
                &mut b,
                "break: later rules of the pass skip anchors this one fires at",
            )
            .changed()
        {
            let mut r = rule.clone();
            r.break_on_match = b;
            out = Some((if b { "break on" } else { "break off" }.to_string(), r));
        }
    });
    ui.horizontal(|ui| {
        ui.label("salt");
        ui.monospace(rule.salt.to_string());
        if ui
            .small_button("Re-roll")
            .on_hover_text("A new salt: `random` spacing changes, the slug stays")
            .clicked()
        {
            let mut r = rule.clone();
            r.salt = reroll(rule.salt);
            out = Some(("re-roll".to_string(), r));
        }
    });
    out
}

/// A region rule's Region (grid, cells, bounds) and Place (along/across,
/// spacing, every, margin, align, jitter, min_length, against). Each edit
/// is one `set_rule`; returns (undo label, new rule).
fn region_editor(ui: &mut egui::Ui, cx: &mut Cx<'_>, rule: &Rule) -> Option<(String, Rule)> {
    let rs = rule.as_region()?;
    let slug = rule.slug.as_str();
    let doc = cx.doc;
    let source = doc
        .project
        .layer(cx.layer.as_str())
        .and_then(|d| d.auto())
        .map(|a| a.source.clone());
    let mut out: Option<(String, Rule)> = None;
    let mut set = |label: &str, f: &dyn Fn(&mut worlds_core::RegionSelect)| {
        let mut r = rule.clone();
        if let Some(x) = r.as_region_mut() {
            f(x);
        }
        out = Some((label.to_string(), r));
    };
    let nz = |v: f64| std::num::NonZeroU32::new(v.max(0.0) as u32);
    ui.strong("Region");
    egui::Grid::new(("region", slug))
        .num_columns(2)
        .spacing([12.0, 4.0])
        .show(ui, |ui| {
            ui.label("grid");
            let cur = rs.region.grid.clone();
            let mut pick = cur.clone();
            let src = source.as_ref().map_or("?", |s| s.as_str());
            egui::ComboBox::from_id_salt(("region grid", slug))
                .selected_text(match &pick {
                    Some(g) => g.to_string(),
                    None => format!("{src} (source)"),
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut pick, None, format!("{src} (source)"));
                    for d in doc.project.layers.iter().filter(|d| d.is_int_grid()) {
                        let label = if d.is_derived() {
                            format!("{} (derived)", d.slug)
                        } else {
                            d.slug.to_string()
                        };
                        ui.selectable_value(&mut pick, Some(d.slug.clone()), label);
                    }
                });
            if pick != cur {
                set("region grid", &|r| r.region.grid = pick.clone());
            }
            ui.end_row();
            ui.label("cells");
            ui.horizontal(|ui| {
                let key = format!("{slug}:region cells");
                let shown = rs.region.cells.to_string();
                if let Some(t) = text_field(ui, &mut cx.r.strs, &key, &shown, 140.0, ". or !solid")
                {
                    match CellMatch::try_from(t.trim().to_string()) {
                        Ok(c) => set("region cells", &|r| r.region.cells = c.clone()),
                        Err(e) => cx.status = Some(format!("cells {t:?}: {e}")),
                    }
                }
            })
            .response
            .on_hover_text("A cell test: `.`, `solid`, `!solid`, `[.|oneway]`; Enter commits");
            ui.end_row();
            type Bound = fn(&mut Region) -> &mut Option<std::num::NonZeroU32>;
            let rows: [(&str, Bound, Bound); 3] = [
                ("width", |r| &mut r.min_width, |r| &mut r.max_width),
                ("height", |r| &mut r.min_height, |r| &mut r.max_height),
                ("area", |r| &mut r.min_area, |r| &mut r.max_area),
            ];
            for (name, lo, hi) in rows {
                ui.label(name);
                ui.horizontal(|ui| {
                    let mut g = rs.region.clone();
                    for (end, f) in [("min", lo), ("max", hi)] {
                        let cur = f(&mut g).map_or(0.0, |v| v.get() as f64);
                        let key = format!("{slug}:{end}_{name}");
                        if let Some(v) =
                            bound_num(ui, &mut cx.r.nums, &key, cur, &format!("{end} "))
                        {
                            set(&format!("{end}_{name}"), &|r| *f(&mut r.region) = nz(v));
                        }
                    }
                })
                .response
                .on_hover_text(
                    "Bounding box size or cell count, inclusive. An unset bound shows \
                     \"none\"; dragging to 0, typing 0 or clearing it unsets it (a bound is \
                     at least 1)",
                );
                ui.end_row();
            }
        });
    ui.add_space(4.0);
    ui.strong("Place");
    let p = &rs.place;
    let along = matches!(p.at, PlaceAt::Along(_));
    egui::Grid::new(("place", slug))
        .num_columns(2)
        .spacing([12.0, 4.0])
        .show(ui, |ui| {
            ui.label("where");
            let mut at = p.at;
            let name = |a: PlaceAt| match a {
                PlaceAt::Along(e) => format!("along {}", edge_name(e)),
                PlaceAt::Across => "across interior".into(),
            };
            egui::ComboBox::from_id_salt(("place at", slug))
                .selected_text(name(at))
                .show_ui(ui, |ui| {
                    for a in [
                        PlaceAt::Along(Edge::Floor),
                        PlaceAt::Along(Edge::Ceiling),
                        PlaceAt::Along(Edge::WallLeft),
                        PlaceAt::Along(Edge::WallRight),
                        PlaceAt::Across,
                    ] {
                        ui.selectable_value(&mut at, a, name(a));
                    }
                });
            if at != p.at {
                set("place", &|r| {
                    r.place.at = at;
                    if matches!(at, PlaceAt::Along(_)) {
                        // [x, y] forms are for `across` only.
                        r.place.every = PerAxis::Same(r.place.every.xy()[0]);
                        r.place.margin = PerAxis::Same(r.place.margin.xy()[0]);
                    } else {
                        // `min_length` and `against` are for `along` only.
                        r.place.min_length = std::num::NonZeroU32::MIN;
                        r.place.against = None;
                    }
                });
            }
            ui.end_row();
            ui.label("spacing");
            let mut sp = p.spacing;
            egui::ComboBox::from_id_salt(("spacing", slug))
                .selected_text(format!("{sp:?}").to_lowercase())
                .show_ui(ui, |ui| {
                    for x in [Spacing::Even, Spacing::Spread, Spacing::Random] {
                        ui.selectable_value(&mut sp, x, format!("{x:?}").to_lowercase());
                    }
                });
            if sp != p.spacing {
                set("spacing", &|r| {
                    r.place.spacing = sp;
                    // `align` is for even, `jitter` for random only.
                    if sp != Spacing::Even {
                        r.place.align = Align::Center;
                    }
                    if sp != Spacing::Random {
                        r.place.jitter = 0;
                    }
                });
            }
            ui.end_row();
            for (name, is_every) in [("every", true), ("margin", false)] {
                ui.label(name);
                ui.horizontal(|ui| {
                    let xy = if is_every {
                        p.every_xy()
                    } else {
                        p.margin_xy()
                    };
                    let axes: &[(usize, &str)] = if along {
                        &[(0, "")]
                    } else {
                        &[(0, "x "), (1, "y ")]
                    };
                    for &(a, prefix) in axes {
                        let key = format!("{slug}:{name}{a}");
                        let lo = if is_every { 1.0 } else { 0.0 };
                        if let Some(v) = num(
                            ui,
                            &mut cx.r.nums,
                            &key,
                            xy[a] as f64,
                            0.1,
                            lo..=4096.0,
                            0,
                            prefix,
                        ) {
                            let mut n = xy;
                            n[a] = v as u32;
                            set(name, &|r| {
                                if is_every {
                                    let e = n.map(|x| {
                                        std::num::NonZeroU32::new(x)
                                            .unwrap_or(std::num::NonZeroU32::MIN)
                                    });
                                    r.place.every = if along || e[0] == e[1] {
                                        PerAxis::Same(e[0])
                                    } else {
                                        PerAxis::Each(e)
                                    };
                                } else {
                                    r.place.margin = if along || n[0] == n[1] {
                                        PerAxis::Same(n[0])
                                    } else {
                                        PerAxis::Each(n)
                                    };
                                }
                            });
                        }
                    }
                })
                .response
                .on_hover_text(if is_every {
                    "Pitch (even) or least distance (spread, random), in cells"
                } else {
                    "Cells kept clear at each end of a run (of the bbox, across)"
                });
                ui.end_row();
            }
            ui.label("align");
            ui.add_enabled_ui(p.spacing == Spacing::Even, |ui| {
                let mut al = p.align;
                egui::ComboBox::from_id_salt(("align", slug))
                    .selected_text(format!("{al:?}").to_lowercase())
                    .show_ui(ui, |ui| {
                        for x in [Align::Start, Align::Center, Align::End] {
                            ui.selectable_value(&mut al, x, format!("{x:?}").to_lowercase());
                        }
                    });
                if al != p.align {
                    set("align", &|r| r.place.align = al);
                }
            });
            ui.end_row();
            ui.label("jitter");
            ui.add_enabled_ui(p.spacing == Spacing::Random, |ui| {
                let key = format!("{slug}:jitter");
                if let Some(v) = num(
                    ui,
                    &mut cx.r.nums,
                    &key,
                    p.jitter as f64,
                    0.1,
                    0.0..=4096.0,
                    0,
                    "",
                ) {
                    set("jitter", &|r| r.place.jitter = v as u32);
                }
            });
            ui.end_row();
            ui.label("min length");
            ui.add_enabled_ui(along, |ui| {
                let key = format!("{slug}:min_length");
                if let Some(v) = num(
                    ui,
                    &mut cx.r.nums,
                    &key,
                    p.min_length.get() as f64,
                    0.1,
                    1.0..=4096.0,
                    0,
                    "",
                ) {
                    set("min_length", &|r| {
                        r.place.min_length =
                            std::num::NonZeroU32::new(v as u32).unwrap_or(std::num::NonZeroU32::MIN)
                    });
                }
            });
            ui.end_row();
            ui.label("against");
            ui.add_enabled_ui(along, |ui| {
                let key = format!("{slug}:against");
                let shown = p.against.as_ref().map_or(String::new(), |c| c.to_string());
                if let Some(t) = text_field(
                    ui,
                    &mut cx.r.strs,
                    &key,
                    &shown,
                    140.0,
                    "any non-region cell",
                ) {
                    let t = t.trim().to_string();
                    if t.is_empty() {
                        set("against", &|r| r.place.against = None);
                    } else {
                        match CellMatch::try_from(t.clone()) {
                            Ok(c) => set("against", &|r| r.place.against = Some(c.clone())),
                            Err(e) => cx.status = Some(format!("against {t:?}: {e}")),
                        }
                    }
                }
            });
            ui.end_row();
        });
    out
}

/// The pattern part of a rule the pattern editor shows.
fn sel(r: &mut Rule) -> &mut MatchSelect {
    r.as_match_mut()
        .expect("the rule editor only edits pattern rules")
}

/// A committed text field: shows `cur`, returns the new text on Enter or
/// focus loss when it changed.
fn text_field(
    ui: &mut egui::Ui,
    bufs: &mut HashMap<String, String>,
    key: &str,
    cur: &str,
    width: f32,
    hint: &str,
) -> Option<String> {
    let mut s = bufs.get(key).cloned().unwrap_or_else(|| cur.to_string());
    let r = ui.add(
        egui::TextEdit::singleline(&mut s)
            .desired_width(width)
            .hint_text(hint),
    );
    if r.changed() {
        bufs.insert(key.to_string(), s.clone());
    }
    if r.lost_focus() {
        bufs.remove(key);
        if s != cur {
            return Some(s);
        }
    }
    None
}

/// A number committed on release (drag) or focus loss (typing): `None`
/// while it is being changed, the new value once.
#[allow(clippy::too_many_arguments)]
fn num(
    ui: &mut egui::Ui,
    bufs: &mut HashMap<String, f64>,
    key: &str,
    cur: f64,
    speed: f64,
    range: std::ops::RangeInclusive<f64>,
    decimals: usize,
    prefix: &str,
) -> Option<f64> {
    let mut v = bufs.get(key).copied().unwrap_or(cur);
    let r = ui.add(
        egui::DragValue::new(&mut v)
            .speed(speed)
            .range(range)
            .max_decimals(decimals)
            .prefix(prefix),
    );
    if r.changed() {
        bufs.insert(key.to_string(), v);
    }
    // Where it was drawn, for tests that drag it.
    ui.ctx()
        .data_mut(|d| d.insert_temp(egui::Id::new(("rules num", key)), r.rect));
    let done =
        r.drag_stopped() || r.lost_focus() || (r.changed() && !r.dragged() && !r.has_focus());
    if done && let Some(v) = bufs.remove(key) {
        return (v != cur).then_some(v);
    }
    None
}

/// What a region bound field shows: `none` for an unset bound (held as
/// 0), else the number.
pub fn bound_text(v: f64) -> String {
    if v < 1.0 {
        "none".to_string()
    } else {
        format!("{}", v as u32)
    }
}

/// What a region bound field reads back: empty, `none` or 0 unset it
/// (0); a positive integer sets it; anything else is no value.
pub fn parse_bound(s: &str) -> Option<f64> {
    let s = s.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("none") {
        return Some(0.0);
    }
    s.parse::<u32>().ok().map(f64::from)
}

/// A region bound (`min_width`, ...), committed like [`num`]: 0 stands for
/// "no bound" and shows as `none`, never as a bound of 0 (which the file
/// format refuses).
fn bound_num(
    ui: &mut egui::Ui,
    bufs: &mut HashMap<String, f64>,
    key: &str,
    cur: f64,
    prefix: &str,
) -> Option<f64> {
    let mut v = bufs.get(key).copied().unwrap_or(cur);
    let r = ui.add(
        egui::DragValue::new(&mut v)
            .speed(0.1)
            .range(0.0..=100_000.0)
            .max_decimals(0)
            .prefix(prefix)
            .custom_formatter(|v, _| bound_text(v))
            .custom_parser(parse_bound),
    );
    if r.changed() {
        bufs.insert(key.to_string(), v);
    }
    // Where it was drawn and what it shows, for tests.
    ui.ctx().data_mut(|d| {
        d.insert_temp(egui::Id::new(("rules num", key)), r.rect);
        d.insert_temp(egui::Id::new(("rules num text", key)), bound_text(v));
    });
    let done =
        r.drag_stopped() || r.lost_focus() || (r.changed() && !r.dragged() && !r.has_focus());
    if done && let Some(v) = bufs.remove(key) {
        return (v != cur).then_some(v);
    }
    None
}

/// A 0..1 slider committed on release.
fn slider01(
    ui: &mut egui::Ui,
    bufs: &mut HashMap<String, f64>,
    key: &str,
    cur: f64,
) -> Option<f64> {
    let mut v = bufs.get(key).copied().unwrap_or(cur);
    let r = ui.add(egui::Slider::new(&mut v, 0.0..=1.0).max_decimals(3));
    if r.changed() {
        bufs.insert(key.to_string(), v);
    }
    let done =
        r.drag_stopped() || r.lost_focus() || (r.changed() && !r.dragged() && !r.has_focus());
    if done && let Some(v) = bufs.remove(key) {
        return (v != cur).then_some(v);
    }
    None
}

/// The pattern grid, glyph chips, size buttons and the text box. Returns
/// the edited pattern once per committed edit (a click, a size change, a
/// text commit).
fn pattern_editor(
    ui: &mut egui::Ui,
    cx: &mut Cx<'_>,
    key: &str,
    p: &Pattern,
    editable: bool,
) -> Option<Pattern> {
    let doc = cx.doc;
    let layer = cx.layer.clone();
    let source = doc
        .project
        .layer(layer.as_str())
        .and_then(|d| d.auto())
        .map(|a| a.source.clone());
    let mut out: Option<Pattern> = None;
    // Grid the pattern reads.
    ui.horizontal(|ui| {
        ui.label("grid");
        let cur = p.grid.clone().or_else(|| source.clone());
        let mut pick = cur.clone();
        egui::ComboBox::from_id_salt((key, "grid"))
            .selected_text(cur.as_ref().map_or("?", |g| g.as_str()))
            .show_ui(ui, |ui| {
                for l in doc.project.layers.iter().filter(|l| l.is_int_grid()) {
                    ui.selectable_value(&mut pick, Some(l.slug.clone()), l.slug.as_str());
                }
            });
        if editable && pick != cur {
            let mut np = p.clone();
            np.grid = pick.filter(|g| Some(g) != source.as_ref());
            out = Some(np);
        }
        if let Rows::Named(n) = &p.rows {
            ui.weak(format!("rows = {n} (rules/macros.json)"));
            if editable
                && ui
                    .small_button("Make inline")
                    .on_hover_text("Copy the named rows into this rule")
                    .clicked()
            {
                let mut np = p.clone();
                np.rows = Rows::Inline(resolved_rows(doc, p));
                out = Some(np);
            }
        }
    });
    let shown = resolved_pattern(doc, p);
    let grid = pattern_grid(doc, &layer, p);
    let rows: Vec<String> = shown.inline_rows().to_vec();
    let cycle = glyph_cycle(&shown, grid);
    let named = matches!(p.rows, Rows::Named(_));
    // The clickable grid.
    let [w, h] = shown.size();
    let cell = 22.0;
    let (ax, ay) = shown.anchor();
    let (rect, _) = ui.allocate_exact_size(
        Vec2::new(w as f32 * cell + 2.0, h as f32 * cell + 2.0),
        Sense::hover(),
    );
    for (y, row) in rows.iter().enumerate() {
        for (x, g) in row.chars().enumerate() {
            let r = egui::Rect::from_min_size(
                rect.min + Vec2::new(1.0 + x as f32 * cell, 1.0 + y as f32 * cell),
                Vec2::splat(cell - 2.0),
            );
            let resp = ui.interact(r, ui.id().with((key, "cell", x, y)), Sense::click());
            cx.r.cell_rects
                .insert((key.to_string(), x as u32, y as u32), r);
            let m = shown.resolve(g, grid);
            let (fill, neg) = test_color(m.as_ref(), grid);
            let painter = ui.painter();
            painter.rect_filled(r, 2.0, if neg { fill.gamma_multiply(0.35) } else { fill });
            if neg {
                painter.line_segment([r.left_bottom(), r.right_top()], Stroke::new(1.5, fill));
            }
            if (x as u32, y as u32) == (ax, ay) {
                painter.rect_stroke(r, 2.0, Stroke::new(2.0, cx.accent), StrokeKind::Outside);
            }
            let text_col = if fill.intensity() > 0.5 && !neg {
                Color32::BLACK
            } else {
                Color32::WHITE
            };
            painter.text(
                r.center(),
                egui::Align2::CENTER_CENTER,
                g,
                egui::FontId::monospace(13.0),
                text_col,
            );
            let resp = resp.on_hover_text(format!(
                "{g}: {}{}",
                m.map_or("means nothing here".to_string(), |m| m.to_string()),
                if (x as u32, y as u32) == (ax, ay) {
                    " (anchor)"
                } else {
                    ""
                }
            ));
            if !editable || named {
                continue;
            }
            let mods = ui.input(|i| i.modifiers);
            let next = if resp.secondary_clicked() {
                Some('*')
            } else if resp.clicked() && mods.alt {
                Some('@')
            } else if resp.clicked() && mods.shift {
                cx.r.brush
            } else if resp.clicked() {
                let i = cycle.iter().position(|c| *c == g).map_or(0, |i| i + 1);
                Some(cycle[i % cycle.len()])
            } else {
                None
            };
            if let Some(c) = next.filter(|c| *c != g) {
                let mut np = p.clone();
                np.rows = Rows::Inline(set_cell(&rows, x, y, c));
                out = Some(np);
            }
        }
    }
    if editable && !named {
        // Glyph chips: what each glyph means; click one to make it the
        // Shift+click brush.
        ui.horizontal_wrapped(|ui| {
            ui.weak("glyphs:");
            for g in cycle.iter().chain(['@'].iter()) {
                let m = shown.resolve(*g, grid);
                let sel = cx.r.brush == Some(*g);
                if ui
                    .selectable_label(sel, RichText::new(g.to_string()).monospace())
                    .on_hover_text(format!(
                        "{}; click to paint it with Shift+click",
                        m.map_or("?".into(), |m| m.to_string())
                    ))
                    .clicked()
                {
                    cx.r.brush = (!sel).then_some(*g);
                }
            }
        });
        ui.horizontal(|ui| {
            let mut resize = |rows: Vec<String>| {
                let mut np = p.clone();
                np.rows = Rows::Inline(rows);
                out = Some(np);
            };
            let wide = |n: u32| "*".repeat(n as usize);
            if ui
                .small_button("+ ring")
                .on_hover_text("Grow by a ring of * (keeps the center)")
                .clicked()
            {
                let mut r = vec![wide(w + 2)];
                r.extend(rows.iter().map(|x| format!("*{x}*")));
                r.push(wide(w + 2));
                resize(r);
            }
            if ui
                .add_enabled(w > 2 && h > 2, egui::Button::new("− ring").small())
                .clicked()
            {
                resize(
                    rows[1..rows.len() - 1]
                        .iter()
                        .map(|x| x.chars().skip(1).take(w as usize - 2).collect())
                        .collect(),
                );
            }
            if ui
                .small_button("+ row")
                .on_hover_text("Add a row of * at the bottom")
                .clicked()
            {
                let mut r = rows.clone();
                r.push(wide(w.max(1)));
                resize(r);
            }
            if ui
                .add_enabled(h > 1, egui::Button::new("− row").small())
                .clicked()
            {
                resize(rows[..rows.len() - 1].to_vec());
            }
            if ui
                .small_button("+ col")
                .on_hover_text("Add a column of * on the right")
                .clicked()
            {
                resize(rows.iter().map(|x| format!("{x}*")).collect());
            }
            if ui
                .add_enabled(w > 1, egui::Button::new("− col").small())
                .clicked()
            {
                resize(
                    rows.iter()
                        .map(|x| x.chars().take(w as usize - 1).collect())
                        .collect(),
                );
            }
        });
    }
    // The text form: rows, blank line, legend. Enter commits (Shift+Enter
    // is a new line), so does leaving the box.
    let written = pattern_text(p);
    let editing = cx.r.text.as_ref().is_some_and(|t| t.key == key);
    let mut text = if editing {
        cx.r.text.as_ref().expect("editing").text.clone()
    } else {
        written.clone()
    };
    let resp = ui.add_enabled(
        editable,
        egui::TextEdit::multiline(&mut text)
            .code_editor()
            .desired_rows(3)
            .desired_width(240.0)
            .return_key(egui::KeyboardShortcut::new(
                egui::Modifiers::SHIFT,
                egui::Key::Enter,
            )),
    );
    let enter =
        resp.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift);
    if resp.changed() || (resp.has_focus() && !editing) {
        cx.r.text = Some(TextBuf {
            key: key.to_string(),
            text: text.clone(),
        });
    }
    let parsed = parse_pattern_text(&text, p.grid.clone());
    match &parsed {
        Err(e) if text != written => {
            ui.colored_label(Color32::from_rgb(230, 120, 120), e);
        }
        Ok(np) => {
            if let Some(w) = pattern_warnings(doc, &layer, np) {
                ui.colored_label(Color32::from_rgb(230, 190, 90), w);
            }
        }
        _ => {}
    }
    if (enter || resp.lost_focus()) && editing {
        if enter {
            resp.surrender_focus();
        }
        if let Ok(np) = parsed {
            cx.r.text = None;
            if np != *p {
                out = Some(np);
            }
        } else if resp.lost_focus() && !enter {
            // Leaving a box that doesn't parse throws the text away.
            cx.r.text = None;
        }
    }
    out
}

/// Problems a pattern has that don't stop it being saved (validation will
/// list them too): glyphs that mean nothing, ragged rows, `@` count, even
/// sizes without `@`.
fn pattern_warnings(doc: &Document, layer: &Slug, p: &Pattern) -> Option<String> {
    let shown = resolved_pattern(doc, p);
    let grid = pattern_grid(doc, layer, p);
    let mut bad: Vec<char> = Vec::new();
    for row in shown.inline_rows() {
        for g in row.chars() {
            if shown.resolve(g, grid).is_none() && !bad.contains(&g) {
                bad.push(g);
            }
        }
    }
    let mut out = Vec::new();
    if !bad.is_empty() {
        out.push(format!(
            "glyph{} {} mean nothing on {}",
            if bad.len() == 1 { "" } else { "s" },
            bad.iter()
                .map(|c| format!("{c:?}"))
                .collect::<Vec<_>>()
                .join(", "),
            grid.map_or("this grid", |g| g.slug.as_str())
        ));
    }
    let [w, h] = shown.size();
    match shown.anchor_count() {
        0 if w % 2 == 0 || h % 2 == 0 => {
            out.push("an even width or height needs an @ anchor".into())
        }
        0 | 1 => {}
        _ => out.push("more than one @".into()),
    }
    (!out.is_empty()).then(|| out.join("; "))
}

/// The rule's options. Returns (undo label, edited rule) once per commit.
fn options(ui: &mut egui::Ui, cx: &mut Cx<'_>, rule: &Rule) -> Option<(String, Rule)> {
    let slug = rule.slug.as_str();
    let m = rule
        .as_match()
        .expect("the rule editor only edits pattern rules");
    let mut out: Option<(String, Rule)> = None;
    let mut set = |label: &str, f: &dyn Fn(&mut Rule)| {
        let mut r = rule.clone();
        f(&mut r);
        out = Some((label.to_string(), r));
    };
    egui::Grid::new(("options", slug))
        .num_columns(2)
        .spacing([12.0, 4.0])
        .show(ui, |ui| {
            ui.label("symmetry");
            ui.horizontal(|ui| {
                for (s, name) in [
                    (Symmetry::MirrorX, "mirror x"),
                    (Symmetry::MirrorY, "mirror y"),
                    (Symmetry::Rotate, "rotate"),
                ] {
                    let mut on = m.symmetry.contains(&s);
                    let hover = if s == Symmetry::Rotate {
                        "Also try the 90-degree turns (all 8 orientations with a mirror). Needs square tiles."
                    } else {
                        "Also try the pattern mirrored; tiles are flipped to match"
                    };
                    if ui.checkbox(&mut on, name).on_hover_text(hover).changed() {
                        set(&format!("{name} {}", if on { "on" } else { "off" }), &|r| {
                            sel(r).symmetry.retain(|x| *x != s);
                            if on {
                                sel(r).symmetry.push(s);
                                sel(r).symmetry.sort_by_key(|x| *x as u8);
                            }
                        });
                    }
                }
            });
            ui.end_row();
            ui.label("break");
            let mut b = rule.break_on_match;
            if ui
                .checkbox(&mut b, "later rules skip cells this one draws")
                .changed()
            {
                set(if b { "break on" } else { "break off" }, &|r| {
                    r.break_on_match = b
                });
            }
            ui.end_row();
            ui.label("chance");
            if let Some(v) = slider01(ui, &mut cx.r.nums, &format!("{slug}:chance"), m.chance) {
                set("chance", &|r| sel(r).chance = v);
            }
            ui.end_row();
            ui.label("modulo");
            ui.horizontal(|ui| {
                let mut on = m.modulo.is_some();
                if ui.checkbox(&mut on, "").changed() {
                    set(if on { "modulo on" } else { "modulo off" }, &|r| {
                        sel(r).modulo = on.then_some(Modulo {
                            x: 2,
                            y: 1,
                            x_offset: 0,
                            y_offset: 0,
                        })
                    });
                }
                if let Some(m) = &m.modulo {
                    let k = |f: &str| format!("{slug}:modulo:{f}");
                    let bufs = &mut cx.r.nums;
                    if let Some(v) = num(ui, bufs, &k("x"), m.x as f64, 0.1, 1.0..=64.0, 0, "x ") {
                        set("modulo", &|r| sel(r).modulo.as_mut().expect("on").x = v as u32);
                    }
                    if let Some(v) = num(ui, bufs, &k("y"), m.y as f64, 0.1, 1.0..=64.0, 0, "y ") {
                        set("modulo", &|r| sel(r).modulo.as_mut().expect("on").y = v as u32);
                    }
                    if let Some(v) =
                        num(ui, bufs, &k("xo"), m.x_offset as f64, 0.1, -64.0..=64.0, 0, "+x ")
                    {
                        set("modulo", &|r| sel(r).modulo.as_mut().expect("on").x_offset = v as i32);
                    }
                    if let Some(v) =
                        num(ui, bufs, &k("yo"), m.y_offset as f64, 0.1, -64.0..=64.0, 0, "+y ")
                    {
                        set("modulo", &|r| sel(r).modulo.as_mut().expect("on").y_offset = v as i32);
                    }
                }
            });
            ui.end_row();
            ui.label("checker");
            let mut c = m.checker;
            egui::ComboBox::from_id_salt(("checker", slug))
                .selected_text(format!("{c:?}").to_lowercase())
                .show_ui(ui, |ui| {
                    for x in [Checker::None, Checker::Horizontal, Checker::Vertical] {
                        ui.selectable_value(&mut c, x, format!("{x:?}").to_lowercase());
                    }
                });
            if c != m.checker {
                set("checker", &|r| sel(r).checker = c);
            }
            ui.end_row();
            ui.label("perlin");
            ui.horizontal(|ui| {
                let mut on = m.perlin.is_some();
                if ui.checkbox(&mut on, "").changed() {
                    set(if on { "perlin on" } else { "perlin off" }, &|r| {
                        sel(r).perlin = on.then_some(Perlin {
                            active: true,
                            seed: 0,
                            scale: 0.2,
                            octaves: 2,
                        })
                    });
                }
                if let Some(p) = &m.perlin {
                    let mut a = p.active;
                    if ui.checkbox(&mut a, "active").changed() {
                        set("perlin", &|r| sel(r).perlin.as_mut().expect("on").active = a);
                    }
                    let k = |f: &str| format!("{slug}:perlin:{f}");
                    let bufs = &mut cx.r.nums;
                    if let Some(v) =
                        num(ui, bufs, &k("seed"), p.seed as f64, 1.0, -1e9..=1e9, 0, "seed ")
                    {
                        set("perlin", &|r| sel(r).perlin.as_mut().expect("on").seed = v as i64);
                    }
                    if let Some(v) = num(ui, bufs, &k("scale"), p.scale, 0.01, 0.0..=10.0, 3, "scale ")
                    {
                        set("perlin", &|r| sel(r).perlin.as_mut().expect("on").scale = v);
                    }
                    if let Some(v) =
                        num(ui, bufs, &k("oct"), p.octaves as f64, 0.1, 1.0..=8.0, 0, "octaves ")
                    {
                        set("perlin", &|r| sel(r).perlin.as_mut().expect("on").octaves = v as u32);
                    }
                }
            });
            ui.end_row();
            ui.label("out of bounds");
            let grid = pattern_grid(cx.doc, &cx.layer, &m.pattern);
            let cur = m.out_of_bounds.clone();
            let mut pick = cur.clone();
            let name = |o: &Option<OutOfBounds>| match o {
                None => "no match (default)".to_string(),
                Some(OutOfBounds::Empty) => ". (empty)".to_string(),
                Some(OutOfBounds::Value(v)) => v.to_string(),
            };
            egui::ComboBox::from_id_salt(("oob", slug))
                .selected_text(name(&cur))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut pick, None, name(&None));
                    ui.selectable_value(&mut pick, Some(OutOfBounds::Empty), ". (empty)");
                    for v in grid.map_or(&[][..], |g| g.values()) {
                        let o = Some(OutOfBounds::Value(v.slug.clone()));
                        ui.selectable_value(&mut pick, o, v.slug.as_str());
                    }
                });
            if pick != cur {
                set("out of bounds", &|r| sel(r).out_of_bounds = pick.clone());
            }
            ui.end_row();
            ui.label("salt");
            ui.horizontal(|ui| {
                ui.monospace(rule.salt.to_string());
                if ui
                    .small_button("Re-roll")
                    .on_hover_text("A new salt: the rule's random choices change, its slug stays")
                    .clicked()
                {
                    let next = reroll(rule.salt);
                    set("re-roll", &|r| r.salt = next);
                }
            });
            ui.end_row();
        });
    out
}

/// A new salt, different from `old`.
fn reroll(old: i64) -> i64 {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    let v = (worlds_rules::hash::mix(t ^ old as u64) % 100_000) as i64;
    if v == old { v + 1 } else { v }
}

fn tileset_combo(ui: &mut egui::Ui, doc: &Document, id: &str, pick: &mut Option<Slug>) {
    egui::ComboBox::from_id_salt(id)
        .selected_text(pick.as_ref().map_or("(none)", |t| t.as_str()))
        .show_ui(ui, |ui| {
            for t in &doc.project.tilesets {
                ui.selectable_value(pick, Some(t.slug.clone()), t.slug.as_str());
            }
        });
}

/// A rule's output: tileset, alternatives (weights), mode, pivot, alpha,
/// offsets, and the atlas picker. Returns (undo label, new output).
fn output_editor(
    ui: &mut egui::Ui,
    cx: &mut Cx<'_>,
    set: &RuleSet,
    slug: &Slug,
    out: &RuleOutput,
) -> Option<(String, RuleOutput)> {
    let mut res: Option<(String, RuleOutput)> = None;
    let doc = cx.doc;
    match out {
        RuleOutput::Tiles(t) => {
            ui.horizontal(|ui| {
                ui.label("tileset");
                let mut pick = Some(t.tileset.clone());
                tileset_combo(ui, doc, &format!("{slug}:tileset"), &mut pick);
                if let Some(p) = pick.filter(|p| *p != t.tileset) {
                    let mut o = t.clone();
                    o.tileset = p;
                    res = Some(("tileset".into(), RuleOutput::Tiles(o)));
                }
                ui.label("mode");
                let mut m = t.tile_mode;
                egui::ComboBox::from_id_salt((slug, "mode"))
                    .selected_text(if m == TileMode::Stack {
                        "stack"
                    } else {
                        "stamp"
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut m, TileMode::Stack, "stack");
                        ui.selectable_value(&mut m, TileMode::Stamp, "stamp");
                    });
                if m != t.tile_mode {
                    let mut o = t.clone();
                    o.tile_mode = m;
                    res = Some(("tile mode".into(), RuleOutput::Tiles(o)));
                }
                let mut av = t.avoid_overlap;
                if ui
                    .checkbox(&mut av, "avoid overlap")
                    .on_hover_text(
                        "Skip a placement whose footprint is occupied (by stamps with occupies)",
                    )
                    .changed()
                {
                    let mut o = t.clone();
                    o.avoid_overlap = av;
                    res = Some(("avoid overlap".into(), RuleOutput::Tiles(o)));
                }
                if t.tile_mode == TileMode::Stamp {
                    ui.label("pivot");
                    for a in 0..2 {
                        let k = format!("{slug}:pivot{a}");
                        if let Some(v) =
                            num(ui, &mut cx.r.nums, &k, t.pivot[a], 0.01, 0.0..=1.0, 2, "")
                        {
                            let mut o = t.clone();
                            o.pivot[a] = v;
                            res = Some(("pivot".into(), RuleOutput::Tiles(o)));
                        }
                    }
                }
            });
            // Alternatives: thumbnails, weight, remove; as many per line as
            // fit.
            let per_line = ((ui.available_width() / 170.0).floor() as usize).max(1);
            let alts: Vec<(usize, &TileAlt)> = t.tiles.iter().enumerate().collect();
            for line in alts.chunks(per_line) {
                ui.horizontal(|ui| {
                    for &(i, alt) in line {
                        egui::Frame::group(ui.style()).show(ui, |ui| {
                            ui.horizontal(|ui| {
                                for c in alt.tiles.iter().take(3) {
                                    tile_thumb(ui, cx, &t.tileset, *c, 24.0);
                                }
                                if alt.tiles.len() > 3 {
                                    ui.weak(format!("+{}", alt.tiles.len() - 3));
                                }
                                let k = format!("{slug}:w{i}");
                                if let Some(v) = num(
                                    ui,
                                    &mut cx.r.nums,
                                    &k,
                                    alt.weight as f64,
                                    0.1,
                                    0.0..=1000.0,
                                    0,
                                    "×",
                                ) {
                                    let mut o = t.clone();
                                    o.tiles[i].weight = v as u32;
                                    res = Some(("weight".into(), RuleOutput::Tiles(o)));
                                }
                                if ui
                                    .small_button("✕")
                                    .on_hover_text("Remove this alternative")
                                    .clicked()
                                {
                                    let mut o = t.clone();
                                    o.tiles.remove(i);
                                    res = Some(("remove alternative".into(), RuleOutput::Tiles(o)));
                                }
                            });
                        });
                    }
                });
            }
            if let Some(d) = draw_editor(ui, cx, slug, &t.draw) {
                let mut o = t.clone();
                o.draw = d.1;
                res = Some((d.0, RuleOutput::Tiles(o)));
            }
            if let Some(ts) = doc.project.tileset(t.tileset.as_str()) {
                ui.weak("Click a tile: draw only it. Shift+click, or drag a rectangle: add an alternative.");
                let mut marks: BTreeMap<[u32; 2], usize> = BTreeMap::new();
                for (i, alt) in t.tiles.iter().enumerate() {
                    for c in &alt.tiles {
                        marks.entry(*c).or_insert(i + 1);
                    }
                }
                if let Some((a, b, shift)) =
                    atlas_pick(ui, cx, ts, &marks, &format!("{slug}:atlas"))
                {
                    let alt = TileAlt::rect(
                        a[0].min(b[0]),
                        a[1].min(b[1]),
                        a[0].abs_diff(b[0]) + 1,
                        a[1].abs_diff(b[1]) + 1,
                    );
                    let mut o = t.clone();
                    // Shift+click or a drag over several tiles adds; a plain
                    // click replaces.
                    if shift || a != b {
                        o.tiles.push(alt);
                        res = Some(("add alternative".into(), RuleOutput::Tiles(o)));
                    } else {
                        o.tiles = vec![alt];
                        res = Some(("tiles".into(), RuleOutput::Tiles(o)));
                    }
                }
            }
        }
        RuleOutput::Stamp(s) => {
            ui.horizontal(|ui| {
                ui.label("stamp");
                let mut pick = s.stamp.clone();
                egui::ComboBox::from_id_salt((slug, "stamp"))
                    .selected_text(pick.as_str())
                    .show_ui(ui, |ui| {
                        for st in &set.stamps {
                            ui.selectable_value(&mut pick, st.slug.clone(), st.slug.as_str());
                        }
                    });
                if pick != s.stamp {
                    let mut o = s.clone();
                    o.stamp = pick;
                    res = Some(("stamp".into(), RuleOutput::Stamp(o)));
                }
                let mut av = s.avoid_overlap;
                if ui
                    .checkbox(&mut av, "avoid overlap")
                    .on_hover_text(
                        "Skip a placement whose footprint is occupied (by stamps with occupies)",
                    )
                    .changed()
                {
                    let mut o = s.clone();
                    o.avoid_overlap = av;
                    res = Some(("avoid overlap".into(), RuleOutput::Stamp(o)));
                }
            });
            if let Some((ts, tile)) = first_tile(&set.stamps, out) {
                tile_thumb(ui, cx, &ts, tile, 32.0);
            }
            if let Some(d) = draw_editor(ui, cx, slug, &s.draw) {
                let mut o = s.clone();
                o.draw = d.1;
                res = Some((d.0, RuleOutput::Stamp(o)));
            }
        }
    }
    res
}

/// Alpha and offsets of an output.
fn draw_editor(
    ui: &mut egui::Ui,
    cx: &mut Cx<'_>,
    slug: &Slug,
    d: &worlds_core::Draw,
) -> Option<(String, worlds_core::Draw)> {
    let mut res = None;
    ui.horizontal(|ui| {
        ui.label("alpha");
        if let Some(v) = slider01(ui, &mut cx.r.nums, &format!("{slug}:alpha"), d.alpha) {
            let mut n = d.clone();
            n.alpha = v;
            res = Some(("alpha".to_string(), n));
        }
        ui.label("offset px");
        for a in 0..2 {
            let k = format!("{slug}:off{a}");
            if let Some(v) = num(
                ui,
                &mut cx.r.nums,
                &k,
                d.offset_px[a] as f64,
                0.2,
                -512.0..=512.0,
                0,
                if a == 0 { "x " } else { "y " },
            ) {
                let mut n = d.clone();
                n.offset_px[a] = v as i32;
                res = Some(("offset".to_string(), n));
            }
        }
    });
    ui.horizontal(|ui| {
        let mut on = d.random_offset_px.is_some();
        if ui.checkbox(&mut on, "random offset").changed() {
            let mut n = d.clone();
            n.random_offset_px = on.then(RandomOffset::default);
            res = Some(("random offset".to_string(), n));
        }
        if let Some(r) = &d.random_offset_px {
            for (axis, range) in [("x", r.x), ("y", r.y)] {
                for (j, end) in ["min", "max"].iter().enumerate() {
                    let k = format!("{slug}:ro{axis}{j}");
                    if let Some(v) = num(
                        ui,
                        &mut cx.r.nums,
                        &k,
                        range[j] as f64,
                        0.2,
                        -512.0..=512.0,
                        0,
                        &format!("{axis} {end} "),
                    ) {
                        let mut n = d.clone();
                        let ro = n.random_offset_px.as_mut().expect("on");
                        if axis == "x" {
                            ro.x[j] = v as i32;
                        } else {
                            ro.y[j] = v as i32;
                        }
                        res = Some(("random offset".to_string(), n));
                    }
                }
            }
        }
    });
    res
}

/// The atlas with `marks` (tile -> number) drawn on it. Returns the tile
/// clicked, if any.
fn atlas_view(
    ui: &mut egui::Ui,
    cx: &mut Cx<'_>,
    ts: &TilesetDef,
    marks: &BTreeMap<[u32; 2], usize>,
    id: &str,
) -> Option<([u32; 2], bool)> {
    atlas_pick(ui, cx, ts, marks, id).map(|(a, _, s)| (a, s))
}

/// The atlas picker: draws the atlas with `marks` outlined and numbered;
/// returns `(first, last, shift)` tiles of a click or drag once released.
fn atlas_pick(
    ui: &mut egui::Ui,
    cx: &mut Cx<'_>,
    ts: &TilesetDef,
    marks: &BTreeMap<[u32; 2], usize>,
    id: &str,
) -> Option<([u32; 2], [u32; 2], bool)> {
    let tex = super::panels::atlas_texture(cx.atlas, ui.ctx(), cx.doc, ts)?;
    let img = tex.size_vec2();
    let mut res = None;
    egui::ScrollArea::both()
        .id_salt(id)
        .max_height(320.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            let scale = (ui.available_width() / img.x).floor().clamp(1.0, 3.0);
            let size = img * scale;
            let resp = ui.add(egui::Image::new((tex.id(), size)).sense(Sense::click_and_drag()));
            let to_tile = |p: egui::Pos2| {
                let local = (p - resp.rect.min) / scale;
                ts.tile_at_px([local.x.max(0.0) as u32, local.y.max(0.0) as u32])
            };
            let tile_rect = |t: [u32; 2]| {
                let a = ts.tile_px(t);
                egui::Rect::from_min_size(
                    resp.rect.min + Vec2::new(a[0] as f32, a[1] as f32) * scale,
                    Vec2::new(ts.tile_size[0] as f32, ts.tile_size[1] as f32) * scale,
                )
            };
            let p = ui.painter_at(resp.rect);
            for (t, n) in marks {
                let r = tile_rect(*t);
                p.rect_stroke(r, 0.0, Stroke::new(2.0, cx.accent), StrokeKind::Inside);
                p.text(
                    r.left_top() + Vec2::new(2.0, 1.0),
                    egui::Align2::LEFT_TOP,
                    n.to_string(),
                    egui::FontId::proportional(10.0),
                    Color32::WHITE,
                );
            }
            let pointer = resp.interact_pointer_pos();
            let shift = ui.input(|i| i.modifiers.shift);
            if (resp.drag_started() || resp.clicked())
                && cx.r.atlas_drag.is_none()
                && let Some(p) = pointer
            {
                cx.r.atlas_drag = Some((to_tile(p), shift));
            }
            if let (Some((a, _)), Some(pos)) = (cx.r.atlas_drag, pointer) {
                let b = to_tile(pos);
                let r = tile_rect([a[0].min(b[0]), a[1].min(b[1])])
                    .union(tile_rect([a[0].max(b[0]), a[1].max(b[1])]));
                p.rect_stroke(
                    r,
                    0.0,
                    Stroke::new(2.0, Color32::WHITE),
                    StrokeKind::Outside,
                );
            }
            if (resp.drag_stopped() || resp.clicked())
                && let Some((a, s)) = cx.r.atlas_drag.take()
            {
                let b = pointer.map_or(a, to_tile);
                res = Some((a, b, s));
            }
            if let Some(h) = resp.hover_pos() {
                let t = to_tile(h);
                resp.on_hover_text(format!("[{}, {}]", t[0], t[1]));
            }
        });
    res
}

fn reference_editor(ui: &mut egui::Ui, cx: &mut Cx<'_>, g: &RuleGroup, u: &TemplateRef) {
    let doc = cx.doc;
    let layer = cx.layer.clone();
    let slug = u.slug().clone();
    let commit = |cx: &mut Cx<'_>, label: &str, nu: TemplateRef| {
        push(
            cx.pending,
            format!("Reference {}: {label}", nu.slug()),
            vec![set_entry(&cx.layer, &g.slug, RuleEntry::Use(nu))],
        );
    };
    ui.heading(format!("use {} as {slug}", u.template));
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(counts_text(cx.r, &slug)).color(cx.accent));
        {
            if ui.button("Delete").clicked() {
                push(
                    cx.pending,
                    format!("Remove reference {slug}"),
                    vec![Command::RemoveRule {
                        layer: layer.clone(),
                        rule: slug.clone(),
                    }],
                );
                cx.r.selected = None;
            }
            if ui
                .button("Detach")
                .on_hover_text("Replace the reference by its rules, keeping their slugs")
                .clicked()
            {
                match detach_commands(&doc.project, &layer, &slug) {
                    Ok(cmds) => push(cx.pending, format!("Detach {slug}"), cmds),
                    Err(e) => cx.status = Some(e),
                }
            }
        }
    });
    let t = doc.project.template(u.template.as_str());
    if let Some(t) = t
        && let Some(d) = &t.doc
    {
        ui.weak(d);
    }
    if t.is_none() {
        ui.colored_label(
            Color32::from_rgb(230, 150, 90),
            format!(
                "No template {} in rules/macros.json or the built-ins",
                u.template
            ),
        );
    }
    egui::Grid::new(("ref", &slug))
        .num_columns(2)
        .show(ui, |ui| {
            ui.label("template");
            let mut pick = u.template.clone();
            egui::ComboBox::from_id_salt(("ref template", &slug))
                .selected_text(pick.as_str())
                .show_ui(ui, |ui| {
                    let own = doc.project.macros.as_ref().map(|m| &m.templates);
                    for name in own
                        .into_iter()
                        .flat_map(|o| o.keys())
                        .chain(worlds_core::Macros::builtin_templates().keys())
                    {
                        ui.selectable_value(&mut pick, name.clone(), name.as_str());
                    }
                });
            if pick != u.template {
                let mut nu = u.clone();
                // Keep the reference's slug (and so its rules' hash keys).
                nu.slug = Some(slug.clone());
                nu.template = pick;
                commit(cx, "template", nu);
            }
            ui.end_row();
            ui.label("tileset");
            let mut pick = u.tileset.clone();
            tileset_combo(ui, doc, &format!("{slug}:ref tileset"), &mut pick);
            if pick != u.tileset {
                let mut nu = u.clone();
                nu.tileset = pick;
                commit(cx, "tileset", nu);
            }
            ui.end_row();
            ui.label("origin");
            ui.horizontal(|ui| {
                for a in 0..2 {
                    let k = format!("{slug}:origin{a}");
                    if let Some(v) = num(
                        ui,
                        &mut cx.r.nums,
                        &k,
                        u.origin[a] as f64,
                        0.1,
                        0.0..=4096.0,
                        0,
                        if a == 0 { "col " } else { "row " },
                    ) {
                        let mut nu = u.clone();
                        nu.origin[a] = v as u32;
                        commit(cx, "origin", nu);
                    }
                }
            });
            ui.end_row();
            ui.label("legend");
            let written: String = u
                .legend
                .iter()
                .map(|(g, t)| format!("{g} = {t}"))
                .collect::<Vec<_>>()
                .join(", ");
            let key = format!("{slug}:ref legend");
            if let Some(s) = text_field(
                ui,
                &mut cx.r.strs,
                &key,
                &written,
                220.0,
                "# = fortified, S = !fortified",
            ) {
                match parse_legend_line(&s) {
                    Ok(m) => {
                        let mut nu = u.clone();
                        nu.legend = m;
                        commit(cx, "legend", nu);
                    }
                    Err(e) => cx.status = Some(e),
                }
            }
            ui.end_row();
            if !u.tiles.is_empty() {
                ui.label("tile overrides");
                ui.vertical(|ui| {
                    for (rule, alts) in &u.tiles {
                        ui.horizontal(|ui| {
                            ui.label(format!("{rule}: {} alternative(s)", alts.len()));
                            if ui.small_button("clear").clicked() {
                                let mut nu = u.clone();
                                nu.tiles.remove(rule);
                                commit(cx, &format!("clear {rule} tiles"), nu);
                            }
                        });
                    }
                });
                ui.end_row();
            }
        });
    ui.weak("Its rules (tick to use, untick to skip) are listed under it on the left; select one to see it or override its tiles.");
    if let (Some(t), Some(ts)) = (
        t,
        u.tileset
            .as_ref()
            .and_then(|s| doc.project.tileset(s.as_str())),
    ) {
        ui.weak("Where its pieces land at this origin (number = template rule). Click to move the origin there.");
        let pieces = template_pieces(t, u.origin);
        let marks = pieces
            .iter()
            .map(|(k, v)| (*k, v.first().copied().unwrap_or(0)))
            .collect();
        if let Some((tile, _)) = atlas_view(ui, cx, ts, &marks, &format!("{slug}:ref atlas"))
            && tile != u.origin
        {
            let mut nu = u.clone();
            nu.origin = tile;
            commit(cx, "origin", nu);
        }
    }
}

/// `"# = solid, S = !solid"` -> a legend map.
pub fn parse_legend_line(s: &str) -> Result<BTreeMap<char, CellMatch>, String> {
    let mut m = BTreeMap::new();
    for part in s.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let mut chars = part.chars();
        let glyph = chars.next().expect("not empty");
        let Some(test) = chars.as_str().trim_start().strip_prefix('=') else {
            return Err(format!("legend: expected `{glyph} = test`, got {part:?}"));
        };
        m.insert(glyph, CellMatch::try_from(test.trim().to_string())?);
    }
    Ok(m)
}

/// A template rule seen through a reference: read only, except its tiles,
/// which the atlas sets as the reference's override.
fn expanded_editor(
    ui: &mut egui::Ui,
    cx: &mut Cx<'_>,
    g: &RuleGroup,
    u: &TemplateRef,
    expanded: &Slug,
    rule: &str,
) {
    let doc = cx.doc;
    ui.horizontal(|ui| {
        ui.heading(expanded.as_str());
        ui.label(RichText::new(counts_text(cx.r, expanded)).color(cx.accent));
    });
    ui.weak(format!(
        "From reference {} (template {}). Its pattern and options live in the template; the atlas below sets this reference's tiles for {rule}.",
        u.slug(),
        u.template
    ));
    let Ok(groups) = doc.project.expanded_rules(cx.layer.as_str()) else {
        return;
    };
    let Some(er) = groups
        .iter()
        .flat_map(|g| &g.rules)
        .find(|r| r.rule.slug == *expanded)
    else {
        ui.weak("Skipped in this reference.");
        return;
    };
    let r = er.rule.clone().into_owned();
    let (Some(m), Some(out)) = (r.as_match(), r.out()) else {
        return; // templates hold only pattern rules that draw
    };
    pattern_editor(ui, cx, &format!("{expanded}:match"), &m.pattern, false);
    let Some(tiles) = out.as_tiles() else {
        return;
    };
    let fixed = doc
        .project
        .template(u.template.as_str())
        .and_then(|t| t.rules.iter().find(|x| x.slug == rule))
        .is_some_and(TemplateRule::is_fixed);
    let origin = if fixed { [0, 0] } else { u.origin };
    if u.tiles.contains_key(rule) {
        ui.horizontal(|ui| {
            ui.weak("This reference overrides the tiles.");
            if ui.small_button("Use the template's").clicked() {
                let mut nu = u.clone();
                nu.tiles.remove(rule);
                push(
                    cx.pending,
                    format!("Reference {}: template tiles for {rule}", u.slug()),
                    vec![set_entry(&cx.layer, &g.slug, RuleEntry::Use(nu))],
                );
            }
        });
    }
    let Some(ts) = doc.project.tileset(tiles.tileset.as_str()) else {
        return;
    };
    let mut marks: BTreeMap<[u32; 2], usize> = BTreeMap::new();
    for (i, alt) in tiles.tiles.iter().enumerate() {
        for c in &alt.tiles {
            marks.entry(*c).or_insert(i + 1);
        }
    }
    ui.weak("Click: draw only that tile. Shift+click, or drag a rectangle: add an alternative.");
    if let Some((a, b, shift)) = atlas_pick(ui, cx, ts, &marks, &format!("{expanded}:atlas")) {
        let lo = [a[0].min(b[0]), a[1].min(b[1])];
        if lo[0] < origin[0] || lo[1] < origin[1] {
            cx.status = Some(format!(
                "That tile is left of or above the reference's origin [{}, {}]",
                origin[0], origin[1]
            ));
            return;
        }
        let alt = TileAlt::rect(
            lo[0] - origin[0],
            lo[1] - origin[1],
            a[0].abs_diff(b[0]) + 1,
            a[1].abs_diff(b[1]) + 1,
        );
        let mut nu = u.clone();
        let rule_slug = match Slug::new(rule) {
            Ok(s) => s,
            Err(_) => return,
        };
        let current: Vec<TileAlt> = tiles
            .tiles
            .iter()
            .map(|t| TileAlt {
                tiles: t
                    .tiles
                    .iter()
                    .map(|c| {
                        [
                            c[0].saturating_sub(origin[0]),
                            c[1].saturating_sub(origin[1]),
                        ]
                    })
                    .collect(),
                weight: t.weight,
            })
            .collect();
        let entry = nu.tiles.entry(rule_slug).or_insert(current);
        if shift || a != b {
            entry.push(alt);
        } else {
            *entry = vec![alt];
        }
        push(
            cx.pending,
            format!("Reference {}: tiles for {rule}", u.slug()),
            vec![set_entry(&cx.layer, &g.slug, RuleEntry::Use(nu))],
        );
    }
}

// ---- the palette's compact list ----

/// The Palette pane for an auto layer: its groups and rules (click selects
/// in the Rules pane) and "Open in Rules".
pub fn palette_list(ui: &mut egui::Ui, st: &mut EditorState, layer: &Slug) {
    let EditorState { doc, rules, .. } = st;
    let Some(doc) = doc.as_ref() else {
        return;
    };
    if ui.button("Open in Rules").clicked() {
        rules.layer = Some(layer.clone());
        rules.focus = true;
    }
    let Some(set) = doc.project.rules_for(layer.as_str()) else {
        ui.weak("No rules yet.");
        return;
    };
    egui::ScrollArea::vertical()
        .auto_shrink([false, true])
        .show(ui, |ui| {
            for row in palette_rows(set) {
                match row {
                    PaletteRow::Pass(pass) => {
                        ui.add_space(2.0);
                        ui.label(RichText::new(format!("Pass {pass}")).weak());
                    }
                    PaletteRow::Group(gi) => {
                        let g = &set.groups[gi];
                        ui.strong(format!("{} ({})", g.slug, mode_name(g.mode)));
                    }
                    PaletteRow::Entry(gi, ei) => {
                        let e = &set.groups[gi].rules[ei];
                        let sel = rules.selected.as_ref() == Some(e.slug());
                        let text = match e {
                            RuleEntry::Rule(r) => r.slug.to_string(),
                            RuleEntry::Use(u) => format!("use {} as {}", u.template, u.slug()),
                        };
                        if ui.selectable_label(sel, text).clicked() {
                            rules.layer = Some(layer.clone());
                            rules.selected = Some(e.slug().clone());
                            rules.focus = true;
                        }
                    }
                }
            }
        });
}

/// One row of an auto layer's palette list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteRow {
    /// A `Pass n` header.
    Pass(u32),
    /// A group's header (index into `groups`).
    Group(usize),
    /// An entry of a group: (group index, entry index).
    Entry(usize, usize),
}

/// The palette list of a rule set: like the Rules pane, in run order
/// ([`run_order`]) under a `Pass n` header per pass in use.
pub fn palette_rows(set: &RuleSet) -> Vec<PaletteRow> {
    let mut out = Vec::new();
    let mut pass = None;
    for gi in run_order(set) {
        let g = &set.groups[gi];
        if pass != Some(g.pass) {
            pass = Some(g.pass);
            out.push(PaletteRow::Pass(g.pass));
        }
        out.push(PaletteRow::Group(gi));
        out.extend((0..g.rules.len()).map(|ei| PaletteRow::Entry(gi, ei)));
    }
    out
}

/// Draw the highlight over the canvas: fired anchors filled, matched but
/// blocked ones outlined. `cell_rect` maps a cell of the auto layer's grid
/// in the highlighted level to a screen rect.
pub fn draw_highlight(
    painter: &egui::Painter,
    h: &Highlight,
    accent: Color32,
    cell_rect: impl Fn([i32; 2]) -> egui::Rect,
) {
    // Occupied cells: hatched (diagonal lines, clipped to each cell).
    let hatch = Color32::from_rgba_unmultiplied(235, 90, 200, 210);
    for c in &h.occupied {
        let r = cell_rect(*c);
        let p = painter.with_clip_rect(r.intersect(painter.clip_rect()));
        let step = (r.width() / 4.0).max(3.0);
        let mut t = -r.height();
        while t < r.width() {
            p.line_segment(
                [
                    egui::pos2(r.min.x + t, r.max.y),
                    egui::pos2(r.min.x + t + r.height(), r.min.y),
                ],
                Stroke::new(1.5, hatch),
            );
            t += step;
        }
        p.rect_stroke(r, 0.0, Stroke::new(1.0, hatch), StrokeKind::Inside);
    }
    // A region rule's regions: bboxes (selected in the accent, others dim
    // grey) with `w × h`, then their runs as 2 px lines on the edge.
    let dim = Color32::from_rgba_unmultiplied(170, 170, 170, 170);
    for reg in &h.regions {
        let [x, y, w, hh] = reg.bbox;
        let r = cell_rect([x, y]).union(cell_rect([x + w - 1, y + hh - 1]));
        let col = if reg.selected { accent } else { dim };
        painter.rect_stroke(
            r,
            0.0,
            Stroke::new(if reg.selected { 2.0 } else { 1.0 }, col),
            StrokeKind::Inside,
        );
        let label = format!("{w} × {hh}");
        let font = egui::FontId::proportional(12.0);
        let galley = painter.layout_no_wrap(label, font, col);
        let at = r.min + Vec2::new(3.0, 2.0);
        painter.rect_filled(
            egui::Rect::from_min_size(at, galley.size()).expand(2.0),
            2.0,
            Color32::from_black_alpha(170),
        );
        painter.galley(at, galley, col);
        if let Some(e) = h.edge {
            for run in &reg.runs {
                let (a, b) = run_line(e, *run, &cell_rect);
                painter.line_segment([a, b], Stroke::new(2.0, col));
            }
        }
    }
    for c in &h.fired {
        let r = cell_rect(*c);
        painter.rect_filled(r, 0.0, accent.gamma_multiply(0.45));
        painter.rect_stroke(r, 0.0, Stroke::new(1.0, accent), StrokeKind::Inside);
    }
    for c in &h.blocked {
        for (r, s) in blocked_strokes(cell_rect(*c)) {
            painter.rect_stroke(r, 0.0, s, StrokeKind::Inside);
        }
    }
}

/// The outline of a cell where the rule matched but didn't fire: an orange
/// ring between a dark one outside and a light one inside, so it reads on
/// any cell colour (an orange derived value like `pillar_base` included).
pub fn blocked_strokes(r: egui::Rect) -> [(egui::Rect, Stroke); 3] {
    [
        (r, Stroke::new(1.5, Color32::from_rgb(20, 20, 20))),
        (
            r.shrink(1.5),
            Stroke::new(1.5, Color32::from_rgb(240, 170, 60)),
        ),
        (
            r.shrink(3.0),
            Stroke::new(1.0, Color32::from_rgb(250, 250, 250)),
        ),
    ]
}

/// The screen segment of an edge run `[x, y, len]`: along the bottom of a
/// floor run's cells, the top of a ceiling run's, the left of a
/// `wall_left` run's and the right of a `wall_right` run's (a pixel inside
/// the cells, so it isn't hidden under the neighbour's outline).
pub fn run_line(
    e: Edge,
    [x, y, len]: [i32; 3],
    cell_rect: &impl Fn([i32; 2]) -> egui::Rect,
) -> (egui::Pos2, egui::Pos2) {
    let (first, last) = match e {
        Edge::Floor | Edge::Ceiling => (cell_rect([x, y]), cell_rect([x + len - 1, y])),
        Edge::WallLeft | Edge::WallRight => (cell_rect([x, y]), cell_rect([x, y + len - 1])),
    };
    match e {
        Edge::Floor => (
            egui::pos2(first.min.x, first.max.y - 1.0),
            egui::pos2(last.max.x, last.max.y - 1.0),
        ),
        Edge::Ceiling => (
            egui::pos2(first.min.x, first.min.y + 1.0),
            egui::pos2(last.max.x, last.min.y + 1.0),
        ),
        Edge::WallLeft => (
            egui::pos2(first.min.x + 1.0, first.min.y),
            egui::pos2(last.min.x + 1.0, last.max.y),
        ),
        Edge::WallRight => (
            egui::pos2(first.max.x - 1.0, first.min.y),
            egui::pos2(last.max.x - 1.0, last.max.y),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use worlds_core::slug;

    #[test]
    fn pattern_text_round_trips() {
        let mut p = Pattern::new(&["***", ".#.", "S#S"]);
        p.legend = Legend::Inline(BTreeMap::from([
            ('S', CellMatch::try_from("!solid".to_string()).unwrap()),
            ('o', CellMatch::try_from("![solid|.]".to_string()).unwrap()),
        ]));
        let t = pattern_text(&p);
        assert_eq!(t, "***\n.#.\nS#S\n\nS = !solid\no = ![solid|.]");
        assert_eq!(parse_pattern_text(&t, None).unwrap(), p);
        let mut named = Pattern::new(&[]);
        named.rows = Rows::Named(slug("edge_top"));
        named.legend = Legend::Named(slug("walls"));
        named.grid = Some(slug("collision"));
        let t = pattern_text(&named);
        assert_eq!(t, "rows = edge_top\n\nlegend = walls");
        assert_eq!(
            parse_pattern_text(&t, Some(slug("collision"))).unwrap(),
            named
        );
        // Errors name the line.
        let e = parse_pattern_text("**\n***", None).unwrap_err();
        assert!(e.starts_with("line 2:"), "{e}");
        let e = parse_pattern_text("*\n\nS ! solid", None).unwrap_err();
        assert!(e.contains("line 3") && e.contains("S = test"), "{e}");
        let e = parse_pattern_text("*\n\nS = bad slug!", None).unwrap_err();
        assert!(e.contains("invalid cell test"), "{e}");
        assert!(parse_pattern_text("", None).is_err());
    }

    #[test]
    fn cells_cycle_and_the_anchor_moves() {
        let rows: Vec<String> = vec!["*@*".into(), "***".into()];
        assert_eq!(set_cell(&rows, 0, 1, '@'), vec!["***", "@**"]);
        assert_eq!(set_cell(&rows, 2, 0, '#'), vec!["*@#", "***"]);
        let mut p = Pattern::new(&["*"]);
        p.legend = Legend::Inline(BTreeMap::from([('S', CellMatch::Empty)]));
        assert_eq!(glyph_cycle(&p, None), vec!['*', '.', '+', 'S']);
    }

    /// WCAG contrast ratio of two opaque colours.
    fn contrast(a: Color32, b: Color32) -> f32 {
        let lum = |c: Color32| {
            let ch = |v: u8| {
                let v = v as f32 / 255.0;
                if v <= 0.03928 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * ch(c.r()) + 0.7152 * ch(c.g()) + 0.0722 * ch(c.b())
        };
        let (x, y) = (lum(a), lum(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    /// D12: the "matched but didn't fire" outline reads on any cell colour,
    /// `pillar_base`'s orange (#E0A040) included: some ring of it stands
    /// out at least 4:1 from the cell, and the rings nest inside the cell.
    #[test]
    fn blocked_outline_reads_on_any_cell_colour() {
        let r = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(16.0, 16.0));
        let strokes = blocked_strokes(r);
        for cell in [
            Color32::from_rgb(0xE0, 0xA0, 0x40),
            Color32::from_rgb(240, 170, 60),
            Color32::BLACK,
            Color32::WHITE,
            Color32::from_rgb(128, 128, 128),
            Color32::from_rgb(40, 60, 200),
        ] {
            let best = strokes
                .iter()
                .map(|(_, s)| contrast(s.color, cell))
                .fold(0.0, f32::max);
            assert!(best >= 4.0, "{cell:?}: best contrast {best}");
        }
        for (inner, _) in &strokes {
            assert!(r.contains_rect(*inner));
        }
    }

    /// D10: the auto layer's Palette list groups its groups by pass, like
    /// the Rules pane: run order, a `Pass n` header per pass.
    #[test]
    fn palette_rows_group_by_pass() {
        let mut set = set_of(&[("art", &["a1"]), ("marks", &["m1", "m2"]), ("more", &["x"])]);
        set.groups[0].pass = 1;
        set.groups[2].pass = 1;
        use PaletteRow::*;
        assert_eq!(
            palette_rows(&set),
            vec![
                Pass(0),
                Group(1),
                Entry(1, 0),
                Entry(1, 1),
                Pass(1),
                Group(0),
                Entry(0, 0),
                Group(2),
                Entry(2, 0),
            ]
        );
    }

    /// D13: an unset region bound shows `none`, never `0`; `0`, empty or
    /// `none` typed in unset it (a bound is at least 1).
    #[test]
    fn region_bounds_show_none_when_unset() {
        assert_eq!(bound_text(0.0), "none");
        assert_eq!(bound_text(15.0), "15");
        for s in ["", " ", "none", "None", "0"] {
            assert_eq!(parse_bound(s), Some(0.0), "{s:?}");
        }
        assert_eq!(parse_bound("15"), Some(15.0));
        assert_eq!(parse_bound("-3"), None);
        assert_eq!(parse_bound("x"), None);
    }

    fn set_of(groups: &[(&str, &[&str])]) -> RuleSet {
        RuleSet {
            layer: slug("l"),
            stamps: Vec::new(),
            groups: groups
                .iter()
                .map(|(g, rules)| {
                    RuleGroup::new(
                        slug(g),
                        rules
                            .iter()
                            .map(|r| {
                                Rule::new(
                                    slug(r),
                                    Pattern::new(&["+"]),
                                    RuleOutput::tiles(slug("t"), vec![TileAlt::single(0, 0)]),
                                )
                            })
                            .collect(),
                    )
                })
                .collect(),
        }
    }

    #[test]
    fn drop_targets_count_after_removal() {
        let set = set_of(&[("a", &["r1", "r2", "r3"]), ("b", &["s1"])]);
        let r1 = slug("r1");
        // r1 after r2: index 1 once r1 is out.
        assert_eq!(
            drop_target(&set, &r1, 0, Some(1), true),
            Some((slug("a"), 1))
        );
        // r1 before r2 is where it is: no move.
        assert_eq!(drop_target(&set, &r1, 0, Some(1), false), None);
        assert_eq!(drop_target(&set, &r1, 0, Some(0), false), None);
        // To the end of its group.
        assert_eq!(drop_target(&set, &r1, 0, None, false), Some((slug("a"), 2)));
        // Into another group, before s1.
        assert_eq!(
            drop_target(&set, &r1, 1, Some(0), false),
            Some((slug("b"), 0))
        );
        let r3 = slug("r3");
        assert_eq!(
            drop_target(&set, &r3, 0, Some(0), false),
            Some((slug("a"), 0))
        );
        // Groups.
        assert_eq!(group_drop_index(&set, &slug("a"), 1, true), Some(1));
        assert_eq!(group_drop_index(&set, &slug("a"), 1, false), None);
        assert_eq!(group_drop_index(&set, &slug("b"), 0, false), Some(0));
    }

    #[test]
    fn templates_from_rules_are_relative_and_keep_detached_slugs() {
        let mk = |s: &str, tiles: Vec<TileAlt>| {
            Rule::new(
                slug(s),
                Pattern::new(&["+"]),
                RuleOutput::tiles(slug("castle"), tiles),
            )
        };
        let a = mk("walls__lip", vec![TileAlt::rect(15, 0, 4, 1)]);
        let b = mk(
            "walls__fill",
            vec![TileAlt::single(16, 2), TileAlt::single(17, 3)],
        );
        let rules = [&a, &b];
        assert_eq!(min_tile(&rules), [15, 0]);
        let slugs: Vec<&Slug> = rules.iter().map(|r| &r.slug).collect();
        assert_eq!(common_prefix(&slugs).as_deref(), Some("walls"));
        let (t, ts) = template_from_rules(&rules, [14, 0], Some("walls")).unwrap();
        assert_eq!(ts, Some(slug("castle")));
        assert_eq!(t.rules[0].slug, "lip");
        let TemplateOutput::Tiles(o) = &t.rules[1].out else {
            panic!()
        };
        assert_eq!(o.tileset, None);
        assert_eq!(o.tiles, vec![TileAlt::single(2, 2), TileAlt::single(3, 3)]);
        // An origin right of a tile is refused.
        assert!(template_from_rules(&rules, [16, 0], None).is_err());
        // Mixed tilesets: rules stay fixed.
        let c = Rule::new(
            slug("other"),
            Pattern::new(&["+"]),
            RuleOutput::tiles(slug("set3"), vec![TileAlt::single(1, 1)]),
        );
        let (t, ts) = template_from_rules(&[&a, &c], [0, 0], None).unwrap();
        assert_eq!(ts, None);
        assert!(t.rules.iter().all(TemplateRule::is_fixed));
    }

    /// The pane over the `mini.worlds` fixture, driven with real pointer
    /// events: drawing queues nothing; a click, an Alt+click and a right
    /// click on a pattern cell each queue one labelled `set_rule`; a row
    /// dragged by its handle queues one `move_rule`.
    #[test]
    fn pattern_clicks_and_row_drags_queue_one_command_each() {
        fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
            std::fs::create_dir_all(to).unwrap();
            for e in std::fs::read_dir(from).unwrap() {
                let p = e.unwrap().path();
                let to = to.join(p.file_name().unwrap());
                if p.is_dir() {
                    copy_dir(&p, &to);
                } else {
                    std::fs::copy(&p, &to).unwrap();
                }
            }
        }
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("mini.worlds");
        copy_dir(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../worlds-core/tests/fixtures/mini.worlds"),
            &dir,
        );
        // The fixture has no art; the rules read tile opacity.
        std::fs::create_dir_all(dir.join("art")).unwrap();
        for (name, px) in [("castle_main", 256), ("ssw_set5", 2 + 8 * 33)] {
            image::RgbaImage::from_pixel(px, px, image::Rgba([90, 80, 70, 255]))
                .save(dir.join(format!("art/{name}.png")))
                .unwrap();
        }
        let mut doc = Document::open(&dir).unwrap();
        doc.bake_all_blocking();
        // A template reference too, so its row and editor draw.
        let mut u = TemplateRef::new(slug("blob47"));
        u.slug = Some(slug("moss"));
        u.tileset = Some(slug("castle_main"));
        doc.project.rules[0].groups[0].rules.push(RuleEntry::Use(u));
        let mut view = crate::view::ViewState::new();
        view.sync_layers(&doc, None);
        view.active_level = doc.level_ix("crevasse");
        let mut st = EditorState {
            doc: Some(doc),
            view,
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let mut t = 0.0;
        let mut frame = |st: &mut EditorState, events: Vec<egui::Event>, mods: egui::Modifiers| {
            t += 0.1;
            let mut events = events;
            events.insert(0, egui::Event::ModifiersChanged(mods));
            let raw = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1400.0, 900.0),
                )),
                time: Some(t),
                focused: true,
                events,
                ..Default::default()
            };
            let mut out = ctx.run_ui(raw, |ui| {
                refresh(st, 1, false);
                pane(ui, st);
            });
            out.textures_delta.clear();
        };
        let none = egui::Modifiers::NONE;
        for sel in [None, Some("moss"), Some("moss__m20"), Some("wall_top_edge")] {
            st.rules.selected = sel.map(slug);
            for _ in 0..3 {
                frame(&mut st, vec![], none);
            }
            assert!(st.pending.is_empty(), "{sel:?}: {:?}", st.pending);
        }
        // The highlight follows the selection.
        st.rules.selected = Some(slug("wall_fill"));
        frame(&mut st, vec![], none);
        let h = st.rules.highlight.clone().expect("a highlight");
        assert_eq!(h.layer, "main_tiling");
        let fired = st.doc.as_ref().unwrap().bakes[h.level].as_ref().unwrap()["main_tiling"]
            .fired
            .iter()
            .filter(|(_, r)| r.iter().any(|r| r == "wall_fill"))
            .count();
        assert!(fired > 0);
        assert_eq!(h.fired.len(), fired);
        st.rules.selected = Some(slug("wall_top_edge"));
        frame(&mut st, vec![], none);
        frame(&mut st, vec![], none);
        assert!(st.pending.is_empty());
        let mut click = |st: &mut EditorState, x: u32, y: u32, button, mods| {
            let r = st.rules.cell_rects[&("wall_top_edge:match".to_string(), x, y)];
            let p = r.center();
            for pressed in [true, false] {
                frame(
                    st,
                    vec![
                        egui::Event::PointerMoved(p),
                        egui::Event::PointerButton {
                            pos: p,
                            button,
                            pressed,
                            modifiers: mods,
                        },
                    ],
                    mods,
                );
            }
            frame(st, vec![], none);
            let cmd = st.pending.pop().expect("a command");
            assert!(st.pending.is_empty());
            let Command::Batch { label, commands } = cmd else {
                panic!("{cmd:?}")
            };
            assert_eq!(label, "Rule wall_top_edge: pattern");
            let [
                Command::SetRule {
                    def: RuleEntry::Rule(r),
                    ..
                },
            ] = &commands[..]
            else {
                panic!("{commands:?}")
            };
            r.as_match().unwrap().pattern.inline_rows().to_vec()
        };
        let primary = egui::PointerButton::Primary;
        // `*` cycles to the grid's first value glyph.
        assert_eq!(click(&mut st, 0, 0, primary, none), ["#**", ".#.", "###"]);
        assert_eq!(
            click(&mut st, 1, 2, primary, egui::Modifiers::ALT),
            ["***", ".#.", "#@#"]
        );
        assert_eq!(
            click(&mut st, 1, 1, egui::PointerButton::Secondary, none),
            ["***", ".*.", "###"]
        );
        // Drag wall_fill's handle onto the lower half of any_wall_group's
        // row: one move_rule to just after it.
        let (_, handle) = st.rules.row_rects[&slug("wall_fill")];
        let (target, _) = st.rules.row_rects[&slug("any_wall_group")];
        let from = handle.center();
        let to = egui::pos2(target.center().x, target.center().y + target.height() * 0.3);
        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: none,
        };
        frame(
            &mut st,
            vec![egui::Event::PointerMoved(from), button(from, true)],
            none,
        );
        for i in 1..=6 {
            let p = from + (to - from) * (i as f32 / 6.0);
            frame(&mut st, vec![egui::Event::PointerMoved(p)], none);
        }
        frame(&mut st, vec![button(to, false)], none);
        frame(&mut st, vec![], none);
        let moves: Vec<_> = st
            .pending
            .iter()
            .flat_map(|c| match c {
                Command::Batch { commands, .. } => commands.clone(),
                c => vec![c.clone()],
            })
            .collect();
        assert_eq!(
            moves,
            [Command::MoveRule {
                layer: slug("main_tiling"),
                rule: slug("wall_fill"),
                group: slug("walls"),
                index: 2,
            }]
        );
    }

    /// A `ui` step (or `--rule`) that selects a rule far down the list
    /// scrolls the list to it, also into an opened reference's expansion
    /// (e.g. `walls__fill`, the last row).
    #[test]
    fn ui_step_scrolls_the_list_to_the_selected_rule() {
        fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
            std::fs::create_dir_all(to).unwrap();
            for e in std::fs::read_dir(from).unwrap() {
                let p = e.unwrap().path();
                let to = to.join(p.file_name().unwrap());
                if p.is_dir() {
                    copy_dir(&p, &to);
                } else {
                    std::fs::copy(&p, &to).unwrap();
                }
            }
        }
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("mini.worlds");
        copy_dir(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../worlds-core/tests/fixtures/mini.worlds"),
            &dir,
        );
        std::fs::create_dir_all(dir.join("art")).unwrap();
        for (name, px) in [("castle_main", 256), ("ssw_set5", 2 + 8 * 33)] {
            image::RgbaImage::from_pixel(px, px, image::Rgba([90, 80, 70, 255]))
                .save(dir.join(format!("art/{name}.png")))
                .unwrap();
        }
        let mut doc = Document::open(&dir).unwrap();
        doc.bake_all_blocking();
        // 47 expanded rules below the inline ones: far more than fit.
        let mut u = TemplateRef::new(slug("blob47"));
        u.slug = Some(slug("moss"));
        u.tileset = Some(slug("castle_main"));
        doc.project.rules[0].groups[0].rules.push(RuleEntry::Use(u));
        let mut view = crate::view::ViewState::new();
        view.sync_layers(&doc, None);
        view.active_level = doc.level_ix("crevasse");
        let mut st = EditorState {
            doc: Some(doc),
            view,
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let mut t = 0.0;
        let mut frame = |st: &mut EditorState| {
            t += 0.1;
            let raw = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1400.0, 400.0),
                )),
                time: Some(t),
                focused: true,
                ..Default::default()
            };
            let mut out = ctx.run_ui(raw, |ui| {
                refresh(st, 1, false);
                pane(ui, st);
            });
            out.textures_delta.clear();
            out.shapes
        };
        // The colours a row's label was drawn with (`PLACEHOLDER`: the
        // widget's own, which for a selected row is the selection's).
        let label_colors = |st: &EditorState, shapes: &[egui::epaint::ClippedShape], text: &str| {
            let (row, _) = st.rules.row_rects[&slug(text)];
            let mut found = Vec::new();
            for s in shapes {
                if let egui::Shape::Text(t) = &s.shape
                    && t.galley.text() == text
                    && row.contains(t.visual_bounding_rect().center())
                {
                    found.extend(t.galley.job.sections.iter().map(|x| x.format.color));
                }
            }
            found
        };
        let visible = |st: &EditorState, rule: &str| {
            let pane = st.rules.pane_rect.expect("pane drawn");
            let (row, _) = st.rules.row_rects[&slug(rule)];
            pane.contains_rect(row)
        };
        // Selected without reveal (a click does that, on a visible row):
        // the list stays at the top, the last expanded row is off screen.
        st.rules.expanded.insert(slug("moss"));
        st.rules.selected = Some(slug("moss__m255"));
        for _ in 0..3 {
            frame(&mut st);
        }
        assert!(!visible(&st, "moss__m255"), "the test needs a long list");
        // What a `ui` step sets: scrolled into view, once.
        st.rules.reveal = true;
        for _ in 0..3 {
            frame(&mut st);
        }
        assert!(!st.rules.reveal, "reveal is used up");
        assert!(
            visible(&st, "moss__m255"),
            "selected row scrolled into view"
        );
        let shapes = frame(&mut st);
        assert_eq!(
            label_colors(&st, &shapes, "moss__m255"),
            [egui::Color32::PLACEHOLDER],
            "the selected expanded row isn't drawn weak"
        );
        // another expanded row on screen
        let other = st
            .rules
            .row_rects
            .keys()
            .map(|k| k.to_string())
            .find(|k| k.starts_with("moss__") && k != "moss__m255" && visible(&st, k))
            .expect("a neighbour row on screen");
        let weak = ctx.global_style().visuals.weak_text_color();
        assert_eq!(label_colors(&st, &shapes, &other), [weak], "others are");
        // An inline rule at the top, the same way.
        st.rules.selected = Some(slug("wall_top_edge"));
        st.rules.reveal = true;
        for _ in 0..3 {
            frame(&mut st);
        }
        assert!(visible(&st, "wall_top_edge"));
    }

    /// mini.worlds with a derived layer `marks` and an auto layer
    /// `pillars` (pass 0 group `find` with region rule `floor_marks`
    /// writing marks, pass 1 group `art` drawing on them), baked, in a
    /// pane state with `pillars` active.
    fn m6_state() -> (tempfile::TempDir, EditorState) {
        fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
            std::fs::create_dir_all(to).unwrap();
            for e in std::fs::read_dir(from).unwrap() {
                let p = e.unwrap().path();
                let to = to.join(p.file_name().unwrap());
                if p.is_dir() {
                    copy_dir(&p, &to);
                } else {
                    std::fs::copy(&p, &to).unwrap();
                }
            }
        }
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("mini.worlds");
        copy_dir(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../worlds-core/tests/fixtures/mini.worlds"),
            &dir,
        );
        std::fs::create_dir_all(dir.join("art")).unwrap();
        for (name, px) in [("castle_main", 256), ("ssw_set5", 2 + 8 * 33)] {
            image::RgbaImage::from_pixel(px, px, image::Rgba([90, 80, 70, 255]))
                .save(dir.join(format!("art/{name}.png")))
                .unwrap();
        }
        let mut doc = Document::open(&dir).unwrap();
        let setup: Command = serde_json::from_value(serde_json::json!({ "op": "batch", "label": "Passes", "commands": [
            { "op": "set_layer_def", "layer": "marks", "index": 1,
              "def": { "slug": "marks", "name": "Marks", "kind": "int_grid", "derived": true,
                       "values": [{ "slug": "pillar_base", "name": "Pillar base", "color": "#e0a040", "glyph": "p" }] } },
            { "op": "set_layer_def", "layer": "pillars", "index": 2,
              "def": { "slug": "pillars", "name": "Pillars", "kind": "auto", "source": "collision" } },
            { "op": "set_rule_group", "layer": "pillars", "group": "find", "index": 0,
              "def": { "slug": "find", "rules": [
                { "slug": "floor_marks",
                  "region": { "cells": ".", "min_width": 5 },
                  "place": { "along": "floor", "every": 4, "margin": 1 },
                  "write": { "grid": "marks", "value": "pillar_base" } } ] } },
            { "op": "set_rule_group", "layer": "pillars", "group": "art", "index": 1,
              "def": { "slug": "art", "pass": 1, "rules": [
                { "slug": "on_mark", "match": { "grid": "marks", "rows": ["p"] },
                  "out": { "tileset": "castle_main", "tiles": [[1, 1]] } } ] } }
        ] }))
        .unwrap();
        doc.project.apply(setup).unwrap();
        doc.reindex();
        doc.bake_all_blocking();
        let mut view = crate::view::ViewState::new();
        view.sync_layers(&doc, None);
        view.active_level = doc.level_ix("crevasse");
        view.active_layer = doc.project.layers.iter().position(|l| l.slug == "pillars");
        let st = EditorState {
            doc: Some(doc),
            view,
            ..Default::default()
        };
        (tmp, st)
    }

    /// A `ui` step selecting a rule of a later pass keeps the rule's pass
    /// header and group header on screen with it, and leaves a list alone
    /// when they already show (centring the row used to scroll a list only a
    /// little taller than the pane, cutting `Pass 0`'s rows in half at its
    /// top).
    #[test]
    fn revealing_a_rule_keeps_its_pass_and_group_headers_visible() {
        for height in [
            150.0, 170.0, 190.0, 210.0, 230.0, 260.0, 300.0, 400.0, 700.0,
        ] {
            let (_tmp, mut st) = m6_state();
            let ctx = egui::Context::default();
            let mut t = 0.0;
            let mut frame = |st: &mut EditorState| {
                t += 0.1;
                let raw = egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1400.0, height),
                    )),
                    time: Some(t),
                    focused: true,
                    ..Default::default()
                };
                let mut out = ctx.run_ui(raw, |ui| {
                    refresh(st, 1, false);
                    pane(ui, st);
                });
                out.textures_delta.clear();
            };
            // Selected by a click (no reveal): the list stays at the top.
            st.rules.selected = Some(slug("on_mark"));
            for _ in 0..3 {
                frame(&mut st);
            }
            let rects = |st: &EditorState| {
                (
                    st.rules.pass_rects[&0],
                    st.rules.pass_rects[&1],
                    st.rules.group_rects[&slug("art")],
                    st.rules.row_rects[&slug("on_mark")].0,
                )
            };
            let pane_rect = st.rules.pane_rect.expect("pane drawn");
            let before = rects(&st);
            // What a `ui` step sets.
            st.rules.reveal = true;
            for _ in 0..3 {
                frame(&mut st);
            }
            assert!(!st.rules.reveal);
            let (pass0, pass1, group, row) = rects(&st);
            let shows = |r: egui::Rect| pane_rect.contains_rect(r);
            if shows(before.1) && shows(before.2) && shows(before.3) {
                assert_eq!(
                    rects(&st),
                    before,
                    "height {height}: the headers and the row showed, nothing scrolls"
                );
                assert!(shows(pass0), "height {height}: Pass 0 still at the top");
            }
            assert!(shows(row), "height {height}: the row is on screen");
            if pass1.union(row).height() < pane_rect.height() / 2.0 {
                assert!(
                    shows(pass1) && shows(group),
                    "height {height}: Pass 1 {pass1:?} and its group {group:?} show above the \
                     row {row:?} in {pane_rect:?}"
                );
            }
        }
    }

    /// Passes in the list, a group dragged under `Pass 1`, the pass field,
    /// and a region rule's bound: each one command batch, one undo entry;
    /// a selected region rule highlights its regions and floor runs.
    #[test]
    fn passes_and_region_rules_edit_one_entry_each() {
        let (_tmp, mut st) = m6_state();
        let ctx = egui::Context::default();
        let mut t = 0.0;
        let mut frame = |st: &mut EditorState, events: Vec<egui::Event>| {
            t += 0.1;
            let raw = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1400.0, 1000.0),
                )),
                time: Some(t),
                focused: true,
                events,
                ..Default::default()
            };
            let mut out = ctx.run_ui(raw, |ui| {
                refresh(st, 1, false);
                pane(ui, st);
            });
            out.textures_delta.clear();
        };
        let press = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let drag = |frame: &mut dyn FnMut(&mut EditorState, Vec<egui::Event>),
                    st: &mut EditorState,
                    from: egui::Pos2,
                    to: egui::Pos2| {
            frame(st, vec![egui::Event::PointerMoved(from), press(from, true)]);
            for i in 1..=8 {
                let p = from + (to - from) * (i as f32 / 8.0);
                frame(st, vec![egui::Event::PointerMoved(p)]);
            }
            frame(st, vec![press(to, false)]);
            frame(st, vec![]);
        };
        // One batch of one command, applied: one more undo entry.
        let take_one = |st: &mut EditorState| -> Command {
            assert_eq!(st.pending.len(), 1, "{:?}", st.pending);
            let batch = st.pending.pop().unwrap();
            let Command::Batch { commands, .. } = &batch else {
                panic!("{batch:?}")
            };
            assert_eq!(commands.len(), 1, "{commands:?}");
            let one = commands[0].clone();
            let doc = st.doc.as_mut().unwrap();
            let mut h = worlds_core::History::default();
            h.push(&mut doc.project, batch).unwrap();
            assert_eq!(h.undo_entries().len(), 1);
            doc.bake_all_blocking();
            one
        };
        for _ in 0..3 {
            frame(&mut st, vec![]);
        }
        assert!(st.pending.is_empty(), "{:?}", st.pending);
        assert!(st.rules.pass_rects.contains_key(&0));
        assert!(st.rules.pass_rects.contains_key(&1));
        // The empty next pass is a drop target too.
        assert!(st.rules.pass_rects.contains_key(&2));

        // Drag group `find` (pass 0) under the `Pass 1` header.
        let from = st.rules.group_rects[&slug("find")].center();
        let to = st.rules.pass_rects[&1].center();
        drag(&mut frame, &mut st, from, to);
        let cmd = take_one(&mut st);
        let Command::SetRuleGroup {
            group, def, index, ..
        } = &cmd
        else {
            panic!("{cmd:?}")
        };
        assert_eq!((group.as_str(), def.pass, *index), ("find", 1, Some(0)));
        let set = st
            .doc
            .as_ref()
            .unwrap()
            .project
            .rules_for("pillars")
            .unwrap();
        assert!(set.groups.iter().all(|g| g.pass == 1));

        // The pass field of group `art`: dragged up by 2.
        frame(&mut st, vec![]);
        frame(&mut st, vec![]);
        let r = ctx
            .data(|d| d.get_temp::<egui::Rect>(egui::Id::new(("rules num", "pillars/art:pass"))))
            .expect("the pass field drawn");
        drag(
            &mut frame,
            &mut st,
            r.center(),
            r.center() + egui::vec2(40.0, 0.0),
        );
        let cmd = take_one(&mut st);
        let Command::SetRuleGroup { group, def, .. } = &cmd else {
            panic!("{cmd:?}")
        };
        assert_eq!(group.as_str(), "art");
        assert_eq!(def.pass, 3);

        // The region rule: its regions and runs on the canvas, and its
        // min_width dragged from 5 up.
        st.rules.selected = Some(slug("floor_marks"));
        for _ in 0..3 {
            frame(&mut st, vec![]);
        }
        assert!(st.pending.is_empty(), "{:?}", st.pending);
        let h = st.rules.highlight.clone().expect("a highlight");
        assert_eq!(h.edge, Some(Edge::Floor));
        assert!(!h.regions.is_empty());
        assert!(h.regions.iter().any(|r| r.selected && !r.runs.is_empty()));
        assert_eq!(h.fired.len(), 5, "{h:?}");
        // Unset bounds read `none`, a set one its number (D13).
        let shown = |k: &str| {
            ctx.data(|d| d.get_temp::<String>(egui::Id::new(("rules num text", k))))
                .unwrap_or_else(|| panic!("{k} drawn"))
        };
        assert_eq!(shown("floor_marks:min_width"), "5");
        for k in [
            "max_width",
            "min_height",
            "max_height",
            "min_area",
            "max_area",
        ] {
            assert_eq!(shown(&format!("floor_marks:{k}")), "none", "{k}");
        }
        let r = ctx
            .data(|d| {
                d.get_temp::<egui::Rect>(egui::Id::new(("rules num", "floor_marks:min_width")))
            })
            .expect("the min width field drawn");
        drag(
            &mut frame,
            &mut st,
            r.center(),
            r.center() + egui::vec2(30.0, 0.0),
        );
        let cmd = take_one(&mut st);
        let Command::SetRule {
            def: RuleEntry::Rule(rule),
            ..
        } = &cmd
        else {
            panic!("{cmd:?}")
        };
        let w = rule.as_region().unwrap().region.min_width.unwrap().get();
        assert!(w > 5, "{w}");
        // With "Show regions" off, only firings are highlighted.
        st.rules.hide_regions = true;
        frame(&mut st, vec![]);
        assert!(st.rules.highlight.as_ref().unwrap().regions.is_empty());
    }

    #[test]
    fn pass_moves_keep_the_file_in_pass_order() {
        let mut set = set_of(&[("a", &["r1"]), ("b", &["r2"]), ("c", &["r3"])]);
        set.groups[2].pass = 1;
        // `a` under Pass 1: first in pass 1, after b.
        let (def, index) = group_into_pass(&set, &slug("a"), 1).unwrap();
        assert_eq!((def.pass, index), (1, 1));
        // `c` under Pass 0: first of pass 0.
        assert_eq!(group_into_pass(&set, &slug("c"), 0).unwrap().1, 0);
        // Already first in its pass: no move.
        assert!(group_into_pass(&set, &slug("a"), 0).is_none());
        // The pass field: b to 1 keeps its place (still in order)...
        assert_eq!(set_pass(&set, &slug("b"), 1).unwrap().1, None);
        // ...a to 2 goes after everything; c to 0 goes last of pass 0.
        assert_eq!(set_pass(&set, &slug("a"), 2).unwrap().1, Some(2));
        let mut s2 = set.clone();
        s2.groups[1].pass = 1;
        assert_eq!(set_pass(&s2, &slug("c"), 0).unwrap().1, Some(1));
        assert!(set_pass(&set, &slug("a"), 0).is_none());
        // Run order sorts by pass, stably.
        let mut s3 = set.clone();
        s3.groups[0].pass = 2;
        assert_eq!(run_order(&s3), [1, 2, 0]);
    }

    #[test]
    fn legend_lines() {
        let m = parse_legend_line("# = fortified, S = !fortified").unwrap();
        assert_eq!(m.len(), 2);
        assert_eq!(m[&'S'].to_string(), "!fortified");
        assert!(parse_legend_line("# fortified").is_err());
        assert!(parse_legend_line("").unwrap().is_empty());
    }
}
