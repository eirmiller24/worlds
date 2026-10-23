//! Layer definitions and auto-layer rules.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use super::names::{GlyphPicker, Namer, snake_case};
use super::{AutoInfo, Ctx, GroupInfo, LayerInfo, Result, ValueInfo, json as j};
use crate::format::{RESERVED_LAYER_SLUGS, RESERVED_RULES_SLUG};
use crate::id::Slug;
use crate::model::{
    AutoDef, CellMatch, Checker, GroupMode, IntGridDef, IntGridGroup, IntGridValue, LayerDef,
    LayerKind, LevelFieldRequirement, MatchSelect, Modulo, OutOfBounds, Output, Pattern, Perlin,
    RandomOffset, RequirementMode, Rule, RuleEntry, RuleGroup, RuleOutput, RuleSet, Select,
    Symmetry, TileAlt, TileMode, TilesDef,
};

/// LDtk's "anything" pattern code: `+1000001` any value, `-1000001` empty.
const ANYTHING: i64 = 1_000_001;

impl Ctx {
    /// Layer definitions (Worlds order: bottom first) and one rule set per
    /// auto layer.
    pub(super) fn layers(&mut self, defs: &j::Defs) -> Result<(Vec<LayerDef>, Vec<RuleSet>)> {
        self.name_layers(&defs.layers);
        // LDtk lists layers top first; Worlds lists them bottom first.
        let mut top_first = Vec::new();
        let mut rules = Vec::new();
        for l in &defs.layers {
            let info = self.layers[&l.uid].clone();
            let what = format!("layer {:?}", l.identifier);
            self.layer_warnings(l, &what);
            let opacity = l.display_opacity.unwrap_or(1.0).clamp(0.0, 1.0);
            let own_grid = (info.grid != self.grid).then_some(info.grid);
            let kind = match l.ty.as_str() {
                "IntGrid" => {
                    let groups = info
                        .groups
                        .iter()
                        .map(|(uid, g)| {
                            let src = l.int_grid_values_groups.iter().find(|x| x.uid == *uid);
                            IntGridGroup {
                                slug: g.slug.clone(),
                                name: src
                                    .and_then(|x| x.identifier.clone())
                                    .unwrap_or_else(|| g.slug.to_string()),
                                color: src.and_then(|x| x.color.clone()).and_then(|c| {
                                    self.color(&c, &format!("{what} group {:?}", g.slug.as_str()))
                                }),
                                glyph: Some(g.glyph),
                            }
                        })
                        .collect();
                    let with_tiles: Vec<String> = l
                        .int_grid_values
                        .iter()
                        .filter(|v| v.tile.is_some())
                        .map(|v| v.value.to_string())
                        .collect();
                    if !with_tiles.is_empty() {
                        self.warn(format!(
                            "{what}: editor tiles of values {} dropped",
                            with_tiles.join(", ")
                        ));
                    }
                    let mut values = Vec::new();
                    for v in &l.int_grid_values {
                        let vi = &info.values[&v.value];
                        let vwhat = format!("{what} value {}", v.value);
                        let color = self
                            .color(&v.color, &vwhat)
                            .unwrap_or_else(|| crate::model::Color::new("#808080").expect("ok"));
                        values.push(IntGridValue {
                            slug: vi.slug.clone(),
                            name: v.identifier.clone().unwrap_or_else(|| v.value.to_string()),
                            color,
                            glyph: vi.glyph,
                            group: info.groups.get(&vi.group_uid).map(|g| g.slug.clone()),
                        });
                    }
                    if let Some(auto) = &info.auto {
                        let mut a = LayerDef::new(
                            auto.slug.clone(),
                            format!("{} tiles", l.identifier),
                            LayerKind::Auto(AutoDef {
                                source: info.slug.clone(),
                                tile_pivot: [l.tile_pivot_x, l.tile_pivot_y],
                            }),
                        );
                        a.opacity = opacity;
                        rules.push(self.rule_set(l, l.uid, &auto.slug)?);
                        top_first.push(a);
                    } else if !l.auto_rule_groups.is_empty() {
                        self.warn(format!("{what}: has rules but no tileset; rules dropped"));
                    }
                    LayerKind::IntGrid(IntGridDef {
                        values,
                        groups,
                        derived: false,
                    })
                }
                "AutoLayer" => {
                    let source = l
                        .auto_source_layer_def_uid
                        .and_then(|u| self.layers.get(&u))
                        .cloned();
                    match source {
                        Some(s) if s.ldtk_type == "IntGrid" => {
                            if s.grid != info.grid {
                                self.warn(format!(
                                    "{what}: {} px grid differs from its source's {} px; the \
                                     auto layer uses its source's grid",
                                    info.grid, s.grid
                                ));
                            }
                            let src_uid = l.auto_source_layer_def_uid.expect("checked");
                            if l.tileset_def_uid.is_some() {
                                rules.push(self.rule_set(l, src_uid, &info.slug)?);
                            } else {
                                self.warn(format!("{what}: no tileset; rules dropped"));
                                rules.push(RuleSet {
                                    layer: info.slug.clone(),
                                    stamps: Vec::new(),
                                    groups: Vec::new(),
                                });
                            }
                            LayerKind::Auto(AutoDef {
                                source: s.slug.clone(),
                                tile_pivot: [l.tile_pivot_x, l.tile_pivot_y],
                            })
                        }
                        _ => {
                            self.warn(format!(
                                "{what}: auto layer without an IntGrid source; layer dropped"
                            ));
                            continue;
                        }
                    }
                }
                "Tiles" => LayerKind::Tiles(TilesDef {
                    tileset: l
                        .tileset_def_uid
                        .and_then(|u| self.tilesets.get(&u))
                        .map(|t| t.slug.clone()),
                }),
                "Entities" => LayerKind::Entities,
                other => {
                    self.warn(format!(
                        "{what}: unknown layer type {other:?}; layer dropped"
                    ));
                    continue;
                }
            };
            let mut def = LayerDef::new(info.slug.clone(), l.identifier.clone(), kind);
            def.doc = l.doc.clone().filter(|d| !d.is_empty());
            def.opacity = opacity;
            // An auto layer always uses its source's grid.
            if !def.is_auto() {
                def.grid_size = own_grid;
            }
            top_first.push(def);
        }
        top_first.reverse();
        Ok((top_first, rules))
    }

    fn layer_warnings(&mut self, l: &j::LayerDef, what: &str) {
        if l.px_offset_x != 0 || l.px_offset_y != 0 {
            self.warn(format!(
                "{what}: pixel offset ({}, {}) dropped (no per-layer offsets)",
                l.px_offset_x, l.px_offset_y
            ));
        }
        if l.parallax_factor_x != 0.0 || l.parallax_factor_y != 0.0 {
            self.warn(format!("{what}: parallax dropped"));
        }
        if !l.required_tags.is_empty() || !l.excluded_tags.is_empty() {
            self.warn(format!("{what}: required/excluded entity tags dropped"));
        }
        if l.auto_tiles_killed_by_other_layer_uid.is_some() {
            self.warn(format!(
                "{what}: autoTilesKilledByOtherLayerUid (tiles removed under another layer) not \
                 carried"
            ));
        }
    }

    /// Claim layer slugs, value slugs and glyphs for every layer up front so
    /// auto layers can refer to their source in any order.
    fn name_layers(&mut self, src: &[j::LayerDef]) {
        let mut names = Namer::default();
        for l in src {
            let file_backed = matches!(l.ty.as_str(), "IntGrid" | "Tiles");
            let mut base = snake_case(&l.identifier, "layer");
            if (file_backed && RESERVED_LAYER_SLUGS.contains(&base.as_str()))
                || base == RESERVED_RULES_SLUG
            {
                base.push_str("_layer");
            }
            let slug = names.claim(&base);
            self.renamed(&l.identifier, &slug);
            let has_rules =
                l.ty == "IntGrid" && l.tileset_def_uid.is_some() && !l.auto_rule_groups.is_empty();
            let auto_slug = match l.ty.as_str() {
                "AutoLayer" => Some(slug.clone()),
                _ if has_rules => Some(names.claim(&format!("{slug}_tiles"))),
                _ => None,
            };
            let (values, groups) = self.name_values(l);
            let auto = auto_slug.map(|s| AutoInfo {
                slug: s,
                groups: HashMap::new(),
                rules: HashMap::new(),
                tilesets: BTreeSet::new(),
            });
            self.layers.insert(
                l.uid,
                LayerInfo {
                    ldtk_name: l.identifier.clone(),
                    ldtk_type: l.ty.clone(),
                    slug,
                    grid: l.grid_size.max(1),
                    tileset_uid: l.tileset_def_uid,
                    values,
                    groups,
                    auto,
                },
            );
        }
    }

    fn name_values(
        &mut self,
        l: &j::LayerDef,
    ) -> (BTreeMap<i64, ValueInfo>, BTreeMap<i64, GroupInfo>) {
        let mut names = Namer::default();
        let mut glyphs = GlyphPicker::default();
        let mut values = BTreeMap::new();
        for (i, v) in l.int_grid_values.iter().enumerate() {
            let base = match &v.identifier {
                Some(id) => snake_case(id, "v"),
                None => format!("v{}", v.value),
            };
            let slug = names.claim(&base);
            if let Some(id) = &v.identifier {
                self.renamed(id, &slug);
            }
            let glyph = glyphs.for_name(slug.as_str());
            values.insert(
                v.value,
                ValueInfo {
                    slug,
                    index: i as u16 + 1,
                    glyph,
                    not_glyph: ' ',
                    group_uid: v.group_uid,
                },
            );
        }
        let mut groups = BTreeMap::new();
        for g in &l.int_grid_values_groups {
            let base = match &g.identifier {
                Some(id) => snake_case(id, "group"),
                None => format!("group_{}", g.uid),
            };
            let slug = names.claim(&base);
            if let Some(id) = &g.identifier {
                self.renamed(id, &slug);
            }
            groups.insert(
                g.uid,
                GroupInfo {
                    slug,
                    glyph: ' ',
                    not_glyph: ' ',
                },
            );
        }
        // Pattern-only glyphs, after every value glyph is fixed so they never clash.
        for v in values.values_mut() {
            v.not_glyph = glyphs.related(v.glyph, v.slug.as_str());
        }
        for g in groups.values_mut() {
            g.glyph = glyphs.for_name(g.slug.as_str());
            g.not_glyph = glyphs.related(g.glyph, g.slug.as_str());
        }
        (values, groups)
    }

    /// The rules of LDtk layer `l`, reading the IntGrid layer `source_uid`,
    /// as the rule set of the auto layer `layer`.
    fn rule_set(&mut self, l: &j::LayerDef, source_uid: i64, layer: &Slug) -> Result<RuleSet> {
        let what = format!("layer {:?}", l.identifier);
        let source = self.layers[&source_uid].clone();
        let tileset = l
            .tileset_def_uid
            .and_then(|u| self.tilesets.get(&u))
            .cloned();
        let biome_field = l
            .biome_field_uid
            .and_then(|u| self.level_fields.get(&u))
            .cloned();
        if tileset.is_none() {
            // Callers only get here with a tileset uid, so it was skipped
            // (LDtk's embedded icon atlas) or never defined.
            let n: usize = l.auto_rule_groups.iter().map(|g| g.rules.len()).sum();
            if n > 0 {
                self.warn(format!(
                    "{what}: its tileset wasn't imported; {n} rule{} dropped",
                    if n == 1 { "" } else { "s" }
                ));
            }
        }
        let mut group_names = Namer::default();
        let mut groups = Vec::new();
        let mut group_ids = HashMap::new();
        let mut rule_ids = HashMap::new();
        for g in &l.auto_rule_groups {
            let gwhat = format!("{what} rule group {:?}", g.name);
            let slug = group_names.claim(&snake_case(&g.name, "group"));
            self.renamed(&g.name, &slug);
            group_ids.insert(g.uid, slug.clone());
            let requires_level_field = if g.required_biome_values.is_empty() {
                None
            } else {
                match &biome_field {
                    Some(f) => {
                        let e = f.enum_uid.and_then(|u| self.enums.get(&u)).cloned();
                        let mut values = Vec::new();
                        for b in &g.required_biome_values {
                            match e.as_ref().and_then(|e| e.values.get(b)) {
                                Some(s) => values.push(s.clone()),
                                None => self.warn(format!(
                                    "{gwhat}: biome {b:?} is not a value of {:?}; dropped",
                                    f.ldtk_name
                                )),
                            }
                        }
                        Some(LevelFieldRequirement {
                            field: f.def.slug.clone(),
                            values,
                            mode: if g.biome_requirement_mode == 1 {
                                RequirementMode::All
                            } else {
                                RequirementMode::Any
                            },
                        })
                    }
                    None => {
                        self.warn(format!(
                            "{gwhat}: requires biomes {:?} but the layer has no biome field; \
                             requirement dropped",
                            g.required_biome_values
                        ));
                        None
                    }
                }
            };
            let icon = match &g.icon {
                Some(r) => self.tile_rect(r, &gwhat),
                None => None,
            };
            let color = g.color.as_deref().and_then(|c| self.color(c, &gwhat));
            let mut rules = Vec::new();
            if let Some(ts) = &tileset {
                for r in &g.rules {
                    match self.rule(r, &source, ts) {
                        Ok(rule) => {
                            rule_ids.insert(r.uid, rule.slug.clone());
                            rules.push(RuleEntry::Rule(rule));
                        }
                        Err(msg) => {
                            self.warn(format!("{what} rule {}: {msg}; rule dropped", r.uid))
                        }
                    }
                }
            }
            // LDtk: an optional group runs where a level enables it; an
            // inactive one never runs, except that a level's optionalRules
            // can force it on. Worlds only lets levels enable `optional`.
            let mode = match (g.active, g.is_optional) {
                (_, true) => GroupMode::Optional,
                (true, false) => GroupMode::Always,
                (false, false) if self.enabled_groups.contains(&g.uid) => {
                    self.warn(format!(
                        "{gwhat}: inactive, but some levels force it on; imported as optional \
                         (on only in those levels)"
                    ));
                    GroupMode::Optional
                }
                (false, false) => GroupMode::Off,
            };
            groups.push(RuleGroup {
                slug,
                name: Some(g.name.clone()),
                mode,
                pass: 0,
                color,
                icon,
                requires_level_field,
                requires_level_tag: None,
                rules,
            });
        }
        let used: BTreeSet<Slug> = groups
            .iter()
            .flat_map(|g: &RuleGroup| g.inline_rules())
            .filter_map(|r| r.out()?.as_tiles().map(|t| t.tileset.clone()))
            .collect();
        if let Some(a) = self.layers.get_mut(&l.uid).and_then(|i| i.auto.as_mut()) {
            a.groups = group_ids;
            a.rules = rule_ids;
            a.tilesets = used;
        }
        Ok(RuleSet {
            layer: layer.clone(),
            stamps: Vec::new(),
            groups,
        })
    }

    fn rule(
        &mut self,
        r: &j::Rule,
        source: &LayerInfo,
        ts: &super::TilesetInfo,
    ) -> std::result::Result<Rule, String> {
        let slug = Slug::new(format!("r_{}", r.uid)).map_err(|e| e.to_string())?;
        let (pattern, notes) = pattern(r, source)?;
        for n in notes {
            self.warn(format!("rule {}: {n}", r.uid));
        }
        if r.tile_rects_ids.is_empty() {
            return Err("no tiles (LDtk never draws it)".into());
        }
        let tiles = r
            .tile_rects_ids
            .iter()
            .map(|alt| TileAlt {
                tiles: alt.iter().map(|&t| ts.tile(t)).collect(),
                weight: 1,
            })
            .collect();
        let mut out = RuleOutput::tiles(ts.slug.clone(), tiles);
        if let RuleOutput::Tiles(t) = &mut out
            && r.tile_mode == "Stamp"
        {
            t.tile_mode = TileMode::Stamp;
            t.pivot = [r.pivot_x, r.pivot_y];
        }
        let draw = out.draw_mut();
        draw.alpha = r.alpha;
        draw.offset_px = [r.tile_x_offset, r.tile_y_offset];
        if [
            r.tile_random_x_min,
            r.tile_random_x_max,
            r.tile_random_y_min,
            r.tile_random_y_max,
        ] != [0; 4]
        {
            let sort = |a: i32, b: i32| [a.min(b), a.max(b)];
            draw.random_offset_px = Some(RandomOffset {
                x: sort(r.tile_random_x_min, r.tile_random_x_max),
                y: sort(r.tile_random_y_min, r.tile_random_y_max),
            });
        }
        let mut sel = MatchSelect::new(pattern);
        sel.chance = r.chance;
        if r.flip_x {
            sel.symmetry.push(Symmetry::MirrorX);
        }
        if r.flip_y {
            sel.symmetry.push(Symmetry::MirrorY);
        }
        if r.x_modulo < 1 || r.y_modulo < 1 {
            return Err(format!(
                "modulo {}x{} (LDtk never matches with a zero modulo)",
                r.x_modulo, r.y_modulo
            ));
        }
        if r.x_modulo != 1 || r.y_modulo != 1 || r.x_offset != 0 || r.y_offset != 0 {
            sel.modulo = Some(Modulo {
                x: r.x_modulo as u32,
                y: r.y_modulo as u32,
                x_offset: r.x_offset,
                y_offset: r.y_offset,
            });
        }
        sel.checker = match r.checker.as_str() {
            "Horizontal" => Checker::Horizontal,
            "Vertical" => Checker::Vertical,
            _ => Checker::None,
        };
        // LDtk gives every rule a random perlin seed; an inactive gate with
        // default settings carries no meaning.
        if r.perlin_active || r.perlin_scale != 0.2 || r.perlin_octaves != 2.0 {
            sel.perlin = Some(Perlin {
                active: r.perlin_active,
                seed: r.perlin_seed as i64,
                scale: r.perlin_scale,
                octaves: r.perlin_octaves.max(0.0) as u32,
            });
        }
        sel.out_of_bounds = match r.out_of_bounds_value {
            None => None,
            Some(0) => Some(OutOfBounds::Empty),
            Some(v) => match source.values.get(&v) {
                Some(vi) => Some(OutOfBounds::Value(vi.slug.clone())),
                // LDtk's own tidy() nulls an undefined out-of-bounds value.
                None => {
                    self.warn(format!(
                        "rule {}: out-of-bounds value {v} is not defined; treated as unset, as \
                         LDtk does",
                        r.uid
                    ));
                    None
                }
            },
        };
        Ok(Rule {
            slug,
            active: r.active,
            select: Select::Match(sel),
            output: Output::Draw(out),
            break_on_match: r.break_on_match,
            salt: r.uid,
        })
    }
}

/// LDtk's flat `size * size` pattern codes as rows and a per-rule legend,
/// plus notes worth a warning. The legend only lists glyphs that don't
/// already mean the right thing: value and group glyphs are implied by the
/// source layer, so only negations (`!slug`) need an entry. `grid` is left
/// out: it is the auto layer's source.
///
/// A code naming an undefined value or group can't be written as a slug.
/// Imported grids never hold undefined values (they import as empty), so a
/// positive code never matches (the rule is dropped, as it could never fire)
/// and a negative one always matches (it becomes `*`).
fn pattern(r: &j::Rule, source: &LayerInfo) -> std::result::Result<(Pattern, Vec<String>), String> {
    let mut notes = Vec::new();
    let n = r.size;
    if n == 0 || n.is_multiple_of(2) || r.pattern.len() != n * n {
        return Err(format!(
            "pattern has {} cells for size {n}",
            r.pattern.len()
        ));
    }
    let any_grouped = source.values.values().any(|v| v.group_uid != 0);
    let mut legend = BTreeMap::new();
    let mut rows = Vec::with_capacity(n);
    for y in 0..n {
        let mut row = String::with_capacity(n);
        for x in 0..n {
            let code = r.pattern[x + y * n];
            let glyph = match code {
                0 => '*',
                ANYTHING => '+',
                c if c == -ANYTHING => '.',
                c if c.abs() > 999 => {
                    let g = c.abs() / 1000 - 1;
                    if g == 0 {
                        // "Any value outside every group".
                        if any_grouped {
                            return Err("pattern tests \"ungrouped value\", which Worlds can't \
                                        express when the layer has groups"
                                .into());
                        }
                        if c > 0 { '+' } else { '.' }
                    } else {
                        let Some(gi) = source.groups.get(&g) else {
                            if c > 0 {
                                return Err(format!(
                                    "pattern requires unknown value group {g} (never matches)"
                                ));
                            }
                            notes.push(format!(
                                "pattern cell \"not in unknown group {g}\" always holds; became `*`"
                            ));
                            row.push('*');
                            continue;
                        };
                        if c > 0 {
                            gi.glyph
                        } else {
                            legend.insert(gi.not_glyph, CellMatch::Not(gi.slug.clone()));
                            gi.not_glyph
                        }
                    }
                }
                c => {
                    let Some(vi) = source.values.get(&c.abs()) else {
                        if c > 0 {
                            return Err(format!(
                                "pattern requires undefined value {c} (never matches)"
                            ));
                        }
                        notes.push(format!(
                            "pattern cell \"not undefined value {}\" always holds; became `*`",
                            -c
                        ));
                        row.push('*');
                        continue;
                    };
                    if c > 0 {
                        vi.glyph
                    } else {
                        legend.insert(vi.not_glyph, CellMatch::Not(vi.slug.clone()));
                        vi.not_glyph
                    }
                }
            };
            row.push(glyph);
        }
        rows.push(row);
    }
    Ok((
        Pattern {
            grid: None,
            legend: legend.into(),
            rows: rows.into(),
        },
        notes,
    ))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::id::slug;
    use crate::ldtk::TilesetInfo;

    /// IntGrid source with value 1 `solid` (glyph `s`, `!solid` = `S`) in
    /// group uid 1 `walls` (glyph `w`, `!walls` = `W`).
    fn source() -> LayerInfo {
        LayerInfo {
            ldtk_name: "Collision".into(),
            ldtk_type: "IntGrid".into(),
            slug: slug("collision"),
            grid: 16,
            tileset_uid: None,
            values: BTreeMap::from([(
                1,
                ValueInfo {
                    slug: slug("solid"),
                    index: 1,
                    glyph: 's',
                    not_glyph: 'S',
                    group_uid: 1,
                },
            )]),
            groups: BTreeMap::from([(
                1,
                GroupInfo {
                    slug: slug("walls"),
                    glyph: 'w',
                    not_glyph: 'W',
                },
            )]),
            auto: None,
        }
    }

    fn rule3(codes: [i64; 9]) -> j::Rule {
        j::Rule {
            uid: 7,
            size: 3,
            pattern: codes.to_vec(),
            tile_rects_ids: vec![vec![0]],
            ..Default::default()
        }
    }

    #[test]
    fn pattern_codes() {
        let (p, notes) = pattern(
            &rule3([0, 1, -1, ANYTHING, -ANYTHING, 0, 2000, -2000, 0]),
            &source(),
        )
        .unwrap();
        assert_eq!(p.inline_rows(), ["*sS", "+.*", "wW*"]);
        // Only the negations need a legend: `s` and `w` are the layer's own glyphs.
        let legend = p.inline_legend().unwrap();
        assert_eq!(
            legend.keys().copied().collect::<Vec<_>>(),
            ['S', 'W'],
            "{legend:?}"
        );
        assert_eq!(legend[&'S'], CellMatch::Not(slug("solid")));
        assert_eq!(legend[&'W'], CellMatch::Not(slug("walls")));
        assert_eq!(p.grid, None);
        assert!(notes.is_empty());
    }

    #[test]
    fn undefined_codes() {
        // "Not value 5" and "not in group 9" always hold on imported grids.
        let (p, notes) = pattern(&rule3([0, 0, 0, 0, 1, -5, 0, -10_000, 0]), &source()).unwrap();
        assert_eq!(p.inline_rows(), ["***", "*s*", "***"]);
        assert_eq!(notes.len(), 2, "{notes:?}");
        // "Value 5" and "in group 9" never do: the rule can't fire.
        assert!(pattern(&rule3([0, 0, 0, 0, 1, 5, 0, 0, 0]), &source()).is_err());
        assert!(pattern(&rule3([0, 0, 0, 0, 1, 0, 0, 10_000, 0]), &source()).is_err());
    }

    #[test]
    fn rules_on_a_skipped_tileset_are_reported() {
        let mut ctx = Ctx::new(&j::Project::default(), PathBuf::new(), Path::new("out")).unwrap();
        ctx.layers.insert(1, source());
        // Tileset 99 was skipped (LDtk's embedded icon atlas).
        ctx.skipped_tilesets.insert(99);
        let l = j::LayerDef {
            uid: 2,
            identifier: "Deco".into(),
            ty: "AutoLayer".into(),
            tileset_def_uid: Some(99),
            auto_rule_groups: vec![j::RuleGroup {
                uid: 5,
                name: "Walls".into(),
                active: true,
                rules: (0..3).map(|_| rule3([0, 0, 0, 0, 1, 0, 0, 0, 0])).collect(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let set = ctx.rule_set(&l, 1, &slug("deco")).unwrap();
        assert!(set.groups[0].rules.is_empty());
        assert!(
            ctx.report
                .warnings
                .iter()
                .any(|w| w == "layer \"Deco\": its tileset wasn't imported; 3 rules dropped"),
            "{:?}",
            ctx.report.warnings
        );
    }

    #[test]
    fn undefined_out_of_bounds_is_unset() {
        let mut ctx = Ctx::new(&j::Project::default(), PathBuf::new(), Path::new("out")).unwrap();
        let ts = TilesetInfo {
            slug: slug("ts"),
            c_wid: 4,
            def: crate::model::TilesetDef {
                slug: slug("ts"),
                image: "ts.png".into(),
                tile_size: [16, 16],
                spacing: 0,
                padding: 0,
            },
        };
        let mut r = rule3([0, 0, 0, 0, 1, 0, 0, 0, 0]);
        r.out_of_bounds_value = Some(42);
        let rule = ctx.rule(&r, &source(), &ts).unwrap();
        assert_eq!(rule.as_match().unwrap().out_of_bounds, None);
        assert!(ctx.report.warnings[0].contains("out-of-bounds value 42"));
        r.out_of_bounds_value = Some(0);
        assert_eq!(
            ctx.rule(&r, &source(), &ts)
                .unwrap()
                .as_match()
                .unwrap()
                .out_of_bounds,
            Some(OutOfBounds::Empty)
        );
    }
}
