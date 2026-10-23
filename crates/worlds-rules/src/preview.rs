//! What each rule of a layer does in a level, for the editor's highlight,
//! the server's `rules.preview` and MCP `preview_rule`.

use worlds_core::{Level, Project, Slug, Write};

use crate::bake::{Baker, Matches, auto_def};
use crate::region::EdgeRun;
use crate::{Result, TilesetImages, derived};

/// One region a region rule looked at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegionPreview {
    /// Row-major order of the region's first cell, from 0.
    pub id: u32,
    /// `[x, y, w, h]` in cells.
    pub bbox: [i32; 4],
    pub area: u32,
    /// Every bound of the rule's `region` holds.
    pub selected: bool,
    /// The runs of the rule's edge (`place.along`, with `against`), before
    /// `margin` and `min_length`; empty for `across`.
    pub runs: Vec<EdgeRun>,
    /// The rule's candidates in this region (row-major; empty when not
    /// selected).
    pub candidates: Vec<[i32; 2]>,
}

/// One rule of a layer in one level.
#[derive(Debug, Clone, PartialEq)]
pub struct RulePreview {
    /// Expanded slug.
    pub rule: Slug,
    pub group: Slug,
    /// The group's `pass`.
    pub pass: u32,
    /// The grid the rule reads first: `match`'s, or `region.grid` (both
    /// defaulting to the layer's `source`).
    pub grid: Slug,
    /// Anchors where it fired (drew at least one tile or wrote), row-major.
    pub fired: Vec<[i32; 2]>,
    /// Where it could fire (see [`crate::rule_matches`]): a pattern rule's
    /// matching variants, a region rule's candidates (`[None]` each).
    /// Anchors that didn't fire are `matches` minus `fired`. Empty when
    /// [`preview_rules_for`] wasn't asked for matches.
    pub matches: Matches,
    /// Anchors where it matched and its gates passed but every matching
    /// variant was skipped because its footprint was occupied
    /// (`avoid_overlap`), row-major.
    pub occupied: Vec<[i32; 2]>,
    /// A region rule's regions (every region of `region.cells` on its grid,
    /// selected or not); `None` for a pattern rule.
    pub regions: Option<Vec<RegionPreview>>,
    /// A `write` rule's output.
    pub write: Option<Write>,
}

/// Every rule of `layer` that runs in `level`, in run order (groups by
/// `pass`, then file order), from one full bake of the layer (and of the
/// writers it depends on). Grids are read by effective value, as each
/// rule's pass sees them. A rule that doesn't run in the level (group
/// doesn't apply, inactive, no alternatives) isn't listed.
pub fn preview_rules(
    project: &Project,
    level: &Level,
    layer: &Slug,
    images: &TilesetImages,
) -> Result<Vec<RulePreview>> {
    preview_rules_for(project, level, layer, images, &|_| true, true)
}

/// [`preview_rules`] for the rules `wanted` keeps (by expanded slug) only,
/// and with [`RulePreview::matches`] worked out only when `matches` is
/// true (empty otherwise): the bake is the same, but the per-anchor
/// pattern tests of the rules left out, or of every rule without
/// `matches`, aren't done.
pub fn preview_rules_for(
    project: &Project,
    level: &Level,
    layer: &Slug,
    images: &TilesetImages,
    wanted: &dyn Fn(&Slug) -> bool,
    matches: bool,
) -> Result<Vec<RulePreview>> {
    let def = auto_def(project, layer.as_str())?;
    if project.rules_for(layer.as_str()).is_none() {
        return Ok(Vec::new());
    }
    let upstream = derived::upstream(project, level, layer.as_str(), images)?;
    let baker = Baker::new(project, level, def, &upstream)?;
    let run = baker.run(images, true)?;
    let mut out = Vec::with_capacity(baker.rule_count());
    for ri in 0..baker.rule_count() {
        let (rule, group) = baker.rule_info(ri);
        if !wanted(&rule.slug) {
            continue;
        }
        let mut fired: Vec<[i32; 2]> = run.out.buckets[ri].iter().map(|r| r.anchor).collect();
        fired.dedup();
        fired.sort_by_key(|c| (c[1], c[0]));
        fired.dedup();
        let p = baker.pass_of(ri);
        let (matches, regions) = if baker.is_region(ri) {
            let m: Matches = if matches {
                run.candidates[ri]
                    .iter()
                    .map(|c| (*c, vec![None]))
                    .collect()
            } else {
                Matches::new()
            };
            let regions = run.regions[ri].as_ref().map(|(found, per)| {
                found
                    .iter()
                    .zip(per)
                    .map(|(f, c)| RegionPreview {
                        id: f.id,
                        bbox: f.bbox,
                        area: f.area,
                        selected: c.selected,
                        runs: c.runs.clone(),
                        candidates: c.candidates.clone(),
                    })
                    .collect()
            });
            (m, regions)
        } else if matches {
            (
                baker.pattern_matches(ri, &baker.grid_view(&run.snapshots[p])),
                None,
            )
        } else {
            (Matches::new(), None)
        };
        out.push(RulePreview {
            rule: rule.slug.clone(),
            group: group.slug.clone(),
            pass: group.pass,
            grid: baker.rule_grid(ri).clone(),
            fired,
            matches,
            occupied: run.out.blocked[ri].clone(),
            regions,
            write: rule.write().cloned(),
        });
    }
    Ok(out)
}

/// The regions region rule `rule` (an expanded slug) of `layer` looks at in
/// `level`: [`RulePreview::regions`] of that rule. `None` when the rule
/// isn't a region rule or doesn't run in the level.
pub fn region_preview(
    project: &Project,
    level: &Level,
    layer: &Slug,
    rule: &str,
    images: &TilesetImages,
) -> Result<Option<Vec<RegionPreview>>> {
    Ok(
        preview_rules_for(project, level, layer, images, &|s| s == rule, false)?
            .into_iter()
            .next()
            .and_then(|p| p.regions),
    )
}
