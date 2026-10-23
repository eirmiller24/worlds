//! `rules.*`: rule sets, edited one group or one entry at a time, macros
//! (`rules/macros.json`), stamps, `rules.expand` and `rules.preview`.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use serde_json::{Value, json};
use worlds_core::command::Rect;
use worlds_core::{
    CellMatch, Command, CommandError, Edge, Macros, Project, RuleEntry, RuleGroup, Slug,
};

use super::params::{self, find_level, layer_of_kind};
use super::{Handler, Write, with_rev};
use crate::host::Host;
use crate::rpc::RpcError;

/// Most cells `rules.preview` lists per rule (fired and matched each).
const MAX_CELLS: usize = 1000;

/// The names in `rules/macros.json`, plus the built-in templates.
fn macro_names(project: &Project) -> Value {
    let m = project.macros.as_ref();
    let names = |f: &dyn Fn(&Macros) -> Vec<&Slug>| m.map(f).unwrap_or_default();
    json!({
        "patterns": names(&|m| m.patterns.keys().collect()),
        "legends": names(&|m| m.legends.keys().collect()),
        "templates": names(&|m| m.templates.keys().collect()),
        "builtin": Macros::builtin_templates().keys().collect::<Vec<_>>(),
    })
}

/// `rules.list { layer? }`.
pub(crate) fn list(project: &Project, method: &str, p: Value) -> Result<Value, RpcError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        layer: Option<Slug>,
    }
    let p: P = params::parse(method, p)?;
    if let Some(l) = &p.layer {
        layer_of_kind(project, l.as_str(), "auto")?;
    }
    let layers: Vec<Value> = project
        .layers
        .iter()
        .filter(|d| d.is_auto())
        .filter(|d| p.layer.as_ref().is_none_or(|l| *l == d.slug))
        .map(|d| {
            let set = project.rules_for(d.slug.as_str());
            let groups: Vec<Value> = set
                .map(|s| {
                    s.groups
                        .iter()
                        .map(|g| {
                            let rules: Vec<Value> = g
                                .rules
                                .iter()
                                .map(|r| {
                                    let mut v = json!({ "slug": r.slug(), "active": r.active() });
                                    match r {
                                        RuleEntry::Use(u) => v["use"] = json!(u.template),
                                        RuleEntry::Rule(rule) => {
                                            if rule.is_region() {
                                                v["kind"] = json!("region");
                                            }
                                            if let Some(w) = rule.write() {
                                                v["write"] = json!(w.grid);
                                            }
                                        }
                                    }
                                    v
                                })
                                .collect();
                            let mut v = json!({ "slug": g.slug });
                            if let Some(n) = &g.name {
                                v["name"] = json!(n);
                            }
                            v["mode"] = json!(g.mode);
                            v["pass"] = json!(g.pass);
                            if let Some(t) = &g.requires_level_tag {
                                v["requires_level_tag"] = json!(t);
                            }
                            v["rules"] = json!(rules);
                            v
                        })
                        .collect()
                })
                .unwrap_or_default();
            json!({ "layer": d.slug, "source": d.auto().map(|a| &a.source),
                "stamps": set.map(|s| s.stamps.iter().map(|st| &st.slug).collect::<Vec<_>>()).unwrap_or_default(),
                "groups": groups })
        })
        .collect();
    Ok(json!({ "layers": layers, "macros": macro_names(project) }))
}

fn group<'p>(
    project: &'p Project,
    layer: &Slug,
    group: &Slug,
) -> Result<(usize, &'p RuleGroup), RpcError> {
    layer_of_kind(project, layer.as_str(), "auto")?;
    project
        .rules_for(layer.as_str())
        .and_then(|s| s.groups.iter().enumerate().find(|(_, g)| g.slug == *group))
        .ok_or_else(|| {
            RpcError::command(&CommandError::UnknownRuleGroup {
                layer: layer.clone(),
                group: group.clone(),
            })
        })
}

/// `rules.get_group { layer, group }` → `{ layer, index, group }`.
pub(crate) fn get_group(project: &Project, method: &str, p: Value) -> Result<Value, RpcError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        layer: Slug,
        group: Slug,
    }
    let p: P = params::parse(method, p)?;
    let (index, g) = group(project, &p.layer, &p.group)?;
    Ok(json!({ "layer": p.layer, "index": index, "group": g }))
}

/// `rules.set_group { layer, group, index? }` → `set_rule_group`.
pub(crate) fn set_group(_: &Project, method: &str, p: Value) -> Result<Write, RpcError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        layer: Slug,
        group: RuleGroup,
        index: Option<usize>,
    }
    let p: P = params::parse(method, p)?;
    Ok(Write::one(Command::SetRuleGroup {
        layer: p.layer,
        group: p.group.slug.clone(),
        def: p.group,
        index: p.index,
    }))
}

/// `rules.remove_group { layer, group }` → `remove_rule_group`.
pub(crate) fn remove_group(_: &Project, method: &str, p: Value) -> Result<Write, RpcError> {
    Ok(Write::one(params::command(method, "remove_rule_group", p)?))
}

/// `rules.set_rule { layer, group, rule, index? }`, `rule` an entry in file
/// form (a rule or a `{ "use": ... }` reference) → `set_rule`.
pub(crate) fn set_rule(_: &Project, method: &str, p: Value) -> Result<Write, RpcError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        layer: Slug,
        group: Slug,
        rule: RuleEntry,
        index: Option<usize>,
    }
    let p: P = params::parse(method, p)?;
    Ok(Write::one(Command::SetRule {
        layer: p.layer,
        group: p.group,
        rule: p.rule.slug().clone(),
        def: p.rule,
        index: p.index,
    }))
}

/// `rules.remove_rule { layer, rule }` → `remove_rule`.
pub(crate) fn remove_rule(_: &Project, method: &str, p: Value) -> Result<Write, RpcError> {
    Ok(Write::one(params::command(method, "remove_rule", p)?))
}

/// `rules.move_rule { layer, rule, group, index }` → `move_rule`.
pub(crate) fn move_rule(_: &Project, method: &str, p: Value) -> Result<Write, RpcError> {
    Ok(Write::one(params::command(method, "move_rule", p)?))
}

/// `rules.set_macro { kind, name, def }` → `set_macro`.
pub(crate) fn set_macro(_: &Project, method: &str, p: Value) -> Result<Write, RpcError> {
    Ok(Write::one(params::command(method, "set_macro", p)?))
}

/// `rules.remove_macro { kind, name, keep_uses? }` → `remove_macro`.
pub(crate) fn remove_macro(_: &Project, method: &str, p: Value) -> Result<Write, RpcError> {
    Ok(Write::one(params::command(method, "remove_macro", p)?))
}

/// `rules.rename_macro { kind, from, to }` → `rename_macro`.
pub(crate) fn rename_macro(_: &Project, method: &str, p: Value) -> Result<Write, RpcError> {
    Ok(Write::one(params::command(method, "rename_macro", p)?))
}

/// `rules.set_stamp { layer, stamp, def, index? }` → `set_stamp`.
pub(crate) fn set_stamp(_: &Project, method: &str, p: Value) -> Result<Write, RpcError> {
    Ok(Write::one(params::command(method, "set_stamp", p)?))
}

/// `rules.remove_stamp { layer, stamp, keep_uses? }` → `remove_stamp`.
pub(crate) fn remove_stamp(_: &Project, method: &str, p: Value) -> Result<Write, RpcError> {
    Ok(Write::one(params::command(method, "remove_stamp", p)?))
}

/// `rules.get_macros {}` → `{ macros, builtin }`: `rules/macros.json` in
/// file form (`{}` when there is none) and the built-in template names.
pub(crate) fn get_macros(project: &Project, method: &str, p: Value) -> Result<Value, RpcError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {}
    let P {} = params::parse(method, p)?;
    Ok(json!({ "macros": macros_file(project),
        "builtin": Macros::builtin_templates().keys().collect::<Vec<_>>() }))
}

/// `rules/macros.json` in file form (no `$schema`, no `format`), `{}` when
/// the project has none.
pub(crate) fn macros_file(project: &Project) -> Value {
    project
        .macros
        .as_ref()
        .and_then(|m| serde_json::to_value(m).ok())
        .unwrap_or_else(|| json!({}))
}

/// The rules of `layer` as they run, or the expansion error as
/// `command_error` `invalid`.
fn expanded<'p>(
    project: &'p Project,
    layer: &Slug,
) -> Result<Vec<worlds_core::ExpandedGroup<'p>>, RpcError> {
    project.expanded_rules(layer.as_str()).map_err(|e| {
        RpcError::invalid(format!("the rules of layer {layer} can't be expanded: {e}"))
    })
}

/// `rules.expand { layer, group? }` → `{ layer, groups: [{ slug, rules }] }`:
/// every rule as the engine runs it, in file form, `from` naming the
/// reference an expanded rule came from.
pub(crate) fn expand(project: &Project, method: &str, p: Value) -> Result<Value, RpcError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        layer: Slug,
        group: Option<Slug>,
    }
    let p: P = params::parse(method, p)?;
    layer_of_kind(project, p.layer.as_str(), "auto")?;
    if let Some(g) = &p.group {
        group(project, &p.layer, g)?;
    }
    let groups: Vec<Value> = expanded(project, &p.layer)?
        .iter()
        .filter(|g| p.group.as_ref().is_none_or(|s| *s == g.group.slug))
        .map(|g| {
            let rules: Vec<Value> = g
                .rules
                .iter()
                .map(|r| {
                    let mut v = serde_json::to_value(&*r.rule).unwrap_or(Value::Null);
                    if let (Some(from), Value::Object(m)) = (&r.from, &mut v) {
                        m.insert("from".into(), json!(from));
                    }
                    v
                })
                .collect();
            json!({ "slug": g.group.slug, "rules": rules })
        })
        .collect();
    Ok(json!({ "layer": p.layer, "groups": groups }))
}

/// Anchors inside `rect` (all of them without one), row-major.
fn in_rect(cells: impl IntoIterator<Item = [i32; 2]>, rect: Option<Rect>) -> Vec<[i32; 2]> {
    let mut out: Vec<[i32; 2]> = cells
        .into_iter()
        .filter(|a| {
            rect.is_none_or(|r| a[0] >= 0 && a[1] >= 0 && r.contains([a[0] as u32, a[1] as u32]))
        })
        .collect();
    out.sort_by_key(|c| (c[1], c[0]));
    out.dedup();
    out
}

/// Whether `bbox` (`[x, y, w, h]` in cells) meets `rect` (always, without
/// one).
fn meets(bbox: [i32; 4], rect: Option<Rect>) -> bool {
    rect.is_none_or(|r| {
        let [x, y, w, h] = [r.x as i64, r.y as i64, r.w as i64, r.h as i64];
        let [bx, by, bw, bh] = bbox.map(i64::from);
        bx < x + w && x < bx + bw && by < y + h && y < by + bh
    })
}

/// A list cut to [`MAX_CELLS`] entries, noting the cut in `truncated`.
fn cut<T: serde::Serialize>(mut list: Vec<T>, truncated: &mut bool) -> Value {
    if list.len() > MAX_CELLS {
        list.truncate(MAX_CELLS);
        *truncated = true;
    }
    json!(list)
}

/// `rules.preview { layer, level, rule?, group?, rect?, matches? }`: where
/// rules fire (and, with `matches`, where they match but are stopped), from
/// a fresh bake that isn't kept. Region rules add their regions.
pub(crate) fn preview(
    h: &mut Handler,
    host: &mut dyn Host,
    method: &str,
    p: Value,
) -> Result<Value, RpcError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        layer: Slug,
        level: Slug,
        rule: Option<Slug>,
        group: Option<Slug>,
        rect: Option<[u32; 4]>,
        #[serde(default)]
        matches: bool,
    }
    let p: P = params::parse(method, p)?;
    let doc = host.doc();
    let project = &*doc.project;
    layer_of_kind(project, p.layer.as_str(), "auto")?;
    let (_, level) = find_level(project, p.level.as_str())?;
    if let Some(g) = &p.group {
        group(project, &p.layer, g)?;
    }
    let groups = expanded(project, &p.layer)?;
    // Each expanded rule asked for, with the reference it came from.
    let mut wanted: BTreeMap<&Slug, Option<&Slug>> = BTreeMap::new();
    for g in &groups {
        if p.group.as_ref().is_some_and(|s| *s != g.group.slug) {
            continue;
        }
        for r in &g.rules {
            let slug = &r.rule.slug;
            if let Some(want) = &p.rule
                && want != slug
                && r.from.as_ref() != Some(want)
            {
                continue;
            }
            wanted.insert(slug, r.from.as_ref());
        }
    }
    if let Some(r) = &p.rule
        && wanted.is_empty()
    {
        let known = groups
            .iter()
            .flat_map(|g| &g.group.rules)
            .any(|e| e.slug() == r)
            || groups
                .iter()
                .flat_map(|g| &g.rules)
                .any(|x| x.rule.slug == *r);
        if !known {
            return Err(RpcError::command(&CommandError::UnknownRule {
                layer: p.layer.clone(),
                rule: r.clone(),
            }));
        }
    }
    // Only the rules asked for, and their matches only with `matches`.
    let previews = worlds_rules::preview_rules_for(
        project,
        level,
        &p.layer,
        doc.images,
        &|s| wanted.contains_key(s),
        p.matches,
    )
    .map_err(|e| RpcError::invalid(format!("baking {} in {}: {e}", p.layer, p.level)))?;
    let rect = p.rect.map(Rect::from);
    let mut fires = Vec::new();
    for pv in previews {
        let Some(from) = wanted.get(&pv.rule) else {
            continue;
        };
        let cells = in_rect(pv.fired.iter().copied(), rect);
        let (blocked, occupied) = if p.matches {
            let fired: BTreeSet<&[i32; 2]> = pv.fired.iter().collect();
            (
                in_rect(
                    pv.matches.keys().filter(|c| !fired.contains(c)).copied(),
                    rect,
                ),
                in_rect(pv.occupied.iter().copied(), rect),
            )
        } else {
            (Vec::new(), Vec::new())
        };
        let regions: Option<Vec<&worlds_rules::RegionPreview>> = pv
            .regions
            .as_ref()
            .map(|rs| rs.iter().filter(|r| meets(r.bbox, rect)).collect());
        let has_regions = regions.as_ref().is_some_and(|r| !r.is_empty());
        if cells.is_empty() && !(p.matches && (!blocked.is_empty() || has_regions)) {
            continue;
        }
        let mut v = json!({ "rule": pv.rule, "group": pv.group });
        if let Some(f) = from {
            v["from"] = json!(f);
        }
        v["pass"] = json!(pv.pass);
        v["grid"] = json!(pv.grid);
        v["count"] = json!(cells.len());
        let mut truncated = false;
        v["cells"] = cut(cells, &mut truncated);
        if p.matches {
            v["matched_count"] = json!(blocked.len());
            v["matched"] = cut(blocked, &mut truncated);
            v["occupied"] = cut(occupied, &mut truncated);
        }
        if let Some(rs) = regions {
            let list: Vec<Value> = rs
                .into_iter()
                .map(|r| {
                    json!({ "id": r.id, "bbox": r.bbox, "area": r.area, "selected": r.selected,
                        "runs": cut(r.runs.clone(), &mut truncated),
                        "candidates": cut(r.candidates.clone(), &mut truncated) })
                })
                .collect();
            v["regions"] = cut(list, &mut truncated);
        }
        if let Some(w) = &pv.write {
            v["write"] = json!({ "grid": w.grid, "value": w.value });
        }
        if truncated {
            v["truncated"] = true.into();
        }
        fires.push(v);
    }
    with_rev(h.rev, json!({ "fires": fires }))
}

/// `rules.regions { level, cells, grid?, along? }`: the regions of a cell
/// test on an int_grid layer (by effective value), with their edge runs and
/// a picture.
pub(crate) fn regions(
    h: &mut Handler,
    host: &mut dyn Host,
    method: &str,
    p: Value,
) -> Result<Value, RpcError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        level: Slug,
        cells: CellMatch,
        grid: Option<Slug>,
        along: Option<Edge>,
    }
    let p: P = params::parse(method, p)?;
    let mut doc = host.doc();
    let project: &Project = doc.project;
    find_level(project, p.level.as_str())?;
    let grid = match p.grid {
        Some(g) => g,
        None => project.collision_layer.clone().ok_or_else(|| {
            RpcError::invalid_params(format!(
                "{method}: the project has no collision_layer; give grid (an int_grid layer)"
            ))
        })?,
    };
    let def = layer_of_kind(project, grid.as_str(), "int_grid")?.clone();
    let values = super::baked::effective_values(&mut doc, p.level.as_str(), &def)?;
    let found = worlds_rules::regions(doc.project, grid.as_str(), &values, &p.cells)
        .map_err(|e| RpcError::invalid(format!("{method}: {e}")))?;
    let regions: Vec<Value> = found
        .regions
        .iter()
        .map(|r| {
            let mut edges = serde_json::Map::new();
            for (name, e) in [
                ("floor", Edge::Floor),
                ("ceiling", Edge::Ceiling),
                ("wall_left", Edge::WallLeft),
                ("wall_right", Edge::WallRight),
            ] {
                if p.along.is_none_or(|a| a == e) {
                    edges.insert(name.into(), json!(r.edges.get(e)));
                }
            }
            json!({ "id": r.id, "bbox": r.bbox, "width": r.width(), "height": r.height(),
                "area": r.area, "edges": edges })
        })
        .collect();
    with_rev(
        h.rev,
        json!({ "grid": grid, "regions": regions, "picture": found.picture() }),
    )
}
