//! Autotile rules: `rules/<layer>.json` and `rules/macros.json`.
//!
//! Covers every LDtk 1.5 rule feature so an import is lossless, plus Worlds'
//! own additions: the `@` anchor glyph, `[a|b]` alternatives in legends, a
//! `require` second pattern, named patterns and legends, and templates
//! (`{ "use": "blob47", ... }`, expanded by [`crate::Project::expanded_rules`]);
//! passes (a `pass` key on groups), region rules
//! (`region` / `place`), outputs that write a derived grid (`write`), stamp
//! occupancy (`occupies` / `avoid_overlap`), `requires_level_tag` and
//! `require` as a list.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt;
use std::num::NonZeroU32;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::common::{
    Color, TileRef, VariantKeys, is_default, is_one, is_true, one, present, variant_schema, yes,
};
use super::layer::{LayerDef, misplaced};
use crate::id::Slug;

/// All rule groups of one `auto` layer: `rules/<layer>.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RuleSet {
    /// The `auto` layer these rules draw into. Same as the file name.
    pub layer: Slug,
    /// Named multi-tile stamps that rule outputs can reference.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stamps: Vec<Stamp>,
    /// Groups in evaluation order.
    pub groups: Vec<RuleGroup>,
}

/// An ordered list of rules that are switched on and off together.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RuleGroup {
    pub slug: Slug,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// When the group runs. Default `always`.
    #[serde(default, skip_serializing_if = "is_default")]
    pub mode: GroupMode,
    /// The pass this group runs in. Default 0. Groups run sorted by pass
    /// (stably), and the file should list them in pass order.
    #[serde(default, skip_serializing_if = "is_default")]
    pub pass: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<Color>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<TileRef>,
    /// Only runs in levels whose enum field matches (LDtk "biomes").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_level_field: Option<LevelFieldRequirement>,
    /// Only runs in levels with these tags (`level.json` `tags`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_level_tag: Option<LevelTagRequirement>,
    /// Rules and template references, in evaluation order.
    pub rules: Vec<RuleEntry>,
}

impl RuleGroup {
    /// An `always` group of inline rules with no options.
    pub fn new(slug: Slug, rules: Vec<Rule>) -> Self {
        Self::from_entries(slug, rules.into_iter().map(RuleEntry::Rule).collect())
    }

    /// An `always` group of rules and template references with no options.
    pub fn from_entries(slug: Slug, rules: Vec<RuleEntry>) -> Self {
        RuleGroup {
            slug,
            name: None,
            mode: GroupMode::Always,
            pass: 0,
            color: None,
            icon: None,
            requires_level_field: None,
            requires_level_tag: None,
            rules,
        }
    }

    /// The inline rules (not template references), in order.
    pub fn inline_rules(&self) -> impl Iterator<Item = &Rule> {
        self.rules.iter().filter_map(RuleEntry::as_rule)
    }

    /// The inline rules, mutably.
    pub fn inline_rules_mut(&mut self) -> impl Iterator<Item = &mut Rule> {
        self.rules.iter_mut().filter_map(RuleEntry::as_rule_mut)
    }
}

impl RuleSet {
    /// Every entry (inline rule or template reference) as written on disk,
    /// with its group, in evaluation order. What runs is
    /// [`crate::Project::expanded_rules`].
    pub fn entries(&self) -> impl Iterator<Item = (&RuleGroup, &RuleEntry)> {
        self.groups
            .iter()
            .flat_map(|g| g.rules.iter().map(move |e| (g, e)))
    }

    /// Whether anything in this set refers to `rules/macros.json` (a
    /// template reference, a named pattern or legend), so a macros edit can
    /// change what it bakes. Built-in templates count too.
    pub fn uses_macros(&self) -> bool {
        self.entries().any(|(_, e)| match e {
            RuleEntry::Use(_) => true,
            RuleEntry::Rule(r) => r.patterns().any(Pattern::is_named),
        })
    }
}

/// Whether a rule group runs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GroupMode {
    /// Runs in every level (subject to `requires_level_field`).
    #[default]
    Always,
    /// Runs only in levels that list it in `layers.<layer>.optional_groups`
    /// (a matching `requires_level_field` also switches it on).
    Optional,
    /// Never runs. Kept so it can be switched back on.
    Off,
}

/// Gate a group on a level field value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LevelFieldRequirement {
    /// Level field slug (an enum field, possibly an array).
    pub field: Slug,
    /// Enum value slugs.
    pub values: Vec<Slug>,
    /// `any`: the field holds at least one of `values`. `all`: it holds every one.
    #[serde(default, skip_serializing_if = "is_default")]
    pub mode: RequirementMode,
}

/// Gate a group on level tags.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LevelTagRequirement {
    /// Level tags: a non-empty list of non-empty strings, no repeats
    /// (validation).
    pub tags: Vec<String>,
    /// `any`: the level has at least one of `tags`. `all`: it has every one.
    #[serde(default, skip_serializing_if = "is_default")]
    pub mode: RequirementMode,
}

impl LevelTagRequirement {
    /// Whether a level with `tags` meets this requirement.
    pub fn holds(&self, tags: &[String]) -> bool {
        match self.mode {
            RequirementMode::Any => self.tags.iter().any(|t| tags.contains(t)),
            RequirementMode::All => self.tags.iter().all(|t| tags.contains(t)),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RequirementMode {
    #[default]
    Any,
    All,
}

/// One rule. It is a **pattern rule** (`match`: fires where a pattern holds
/// around a cell) or a **region rule** (`region` + `place`: fires at places
/// in regions of cells), and it either draws (`out`) or writes a derived
/// grid (`write`).
///
/// On disk a rule is one flat object; which keys it may have depends on its
/// kind, and a key of the other kind is a load error (`"chance" is not
/// allowed on a region rule`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "RuleRepr", into = "RuleRepr")]
pub struct Rule {
    /// Names the rule: unique within the rules file, and part of the hash
    /// behind its random choices.
    pub slug: Slug,
    /// Default true.
    pub active: bool,
    /// Where the rule fires: a pattern, or places in regions.
    pub select: Select,
    /// What it does there: draw, or write a derived grid.
    pub output: Output,
    /// After this rule fires at an anchor, later rules of the same pass skip
    /// that anchor.
    pub break_on_match: bool,
    /// Extra integer mixed into this rule's random choices. Default 0; the
    /// hash already includes the rule slug. Change it only to re-roll the
    /// rule without renaming it. The LDtk importer sets the LDtk rule uid.
    pub salt: i64,
}

/// Where a rule fires.
#[derive(Debug, Clone, PartialEq)]
pub enum Select {
    /// A pattern rule: `match` (and its options).
    Match(MatchSelect),
    /// A region rule: `region` + `place`.
    Region(RegionSelect),
}

/// A pattern rule's `match` and the keys that only pattern rules have.
#[derive(Debug, Clone, PartialEq)]
pub struct MatchSelect {
    /// `match`.
    pub pattern: Pattern,
    /// Patterns that must also hold, anchored on the same cell (usually on
    /// another grid). On disk one pattern, or a list when there are several.
    pub require: Vec<Pattern>,
    /// Extra mirrored or rotated variants of the pattern to try.
    pub symmetry: Vec<Symmetry>,
    /// Probability 0 to 1 that a matching cell is drawn. Default 1.
    pub chance: f64,
    pub modulo: Option<Modulo>,
    pub checker: Checker,
    pub perlin: Option<Perlin>,
    /// What pattern cells outside the level read as. Omitted: LDtk default
    /// (the pattern cannot match where it reaches outside the level).
    pub out_of_bounds: Option<OutOfBounds>,
}

impl MatchSelect {
    /// `match` with every option at its default.
    pub fn new(pattern: Pattern) -> Self {
        MatchSelect {
            pattern,
            require: Vec::new(),
            symmetry: Vec::new(),
            chance: 1.0,
            modulo: None,
            checker: Checker::None,
            perlin: None,
            out_of_bounds: None,
        }
    }

    /// `match`, then every `require` pattern.
    pub fn patterns(&self) -> impl Iterator<Item = &Pattern> {
        std::iter::once(&self.pattern).chain(self.require.iter())
    }

    /// `match`, then every `require` pattern, mutably.
    pub fn patterns_mut(&mut self) -> impl Iterator<Item = &mut Pattern> {
        std::iter::once(&mut self.pattern).chain(self.require.iter_mut())
    }
}

/// A region rule's `region` and `place`.
#[derive(Debug, Clone, PartialEq)]
pub struct RegionSelect {
    pub region: Region,
    pub place: Place,
}

/// What a rule does where it fires.
#[derive(Debug, Clone, PartialEq)]
pub enum Output {
    /// `out`: draw tiles or a stamp.
    Draw(RuleOutput),
    /// `write`: set a cell of a derived grid.
    Write(Write),
}

impl Rule {
    /// A pattern rule that draws, every option at its default.
    pub fn new(slug: Slug, pattern: Pattern, out: RuleOutput) -> Self {
        Rule {
            slug,
            active: true,
            select: Select::Match(MatchSelect::new(pattern)),
            output: Output::Draw(out),
            break_on_match: false,
            salt: 0,
        }
    }

    /// A region rule, every option at its default.
    pub fn region(slug: Slug, region: Region, place: Place, output: Output) -> Self {
        Rule {
            slug,
            active: true,
            select: Select::Region(RegionSelect { region, place }),
            output,
            break_on_match: false,
            salt: 0,
        }
    }

    /// The pattern rule part (`match` and its options); `None` for a region rule.
    pub fn as_match(&self) -> Option<&MatchSelect> {
        match &self.select {
            Select::Match(m) => Some(m),
            Select::Region(_) => None,
        }
    }

    pub fn as_match_mut(&mut self) -> Option<&mut MatchSelect> {
        match &mut self.select {
            Select::Match(m) => Some(m),
            Select::Region(_) => None,
        }
    }

    /// The region rule part (`region` and `place`); `None` for a pattern rule.
    pub fn as_region(&self) -> Option<&RegionSelect> {
        match &self.select {
            Select::Region(r) => Some(r),
            Select::Match(_) => None,
        }
    }

    pub fn as_region_mut(&mut self) -> Option<&mut RegionSelect> {
        match &mut self.select {
            Select::Region(r) => Some(r),
            Select::Match(_) => None,
        }
    }

    pub fn is_region(&self) -> bool {
        matches!(self.select, Select::Region(_))
    }

    /// What it draws (`out`); `None` for a rule that writes.
    pub fn out(&self) -> Option<&RuleOutput> {
        match &self.output {
            Output::Draw(o) => Some(o),
            Output::Write(_) => None,
        }
    }

    pub fn out_mut(&mut self) -> Option<&mut RuleOutput> {
        match &mut self.output {
            Output::Draw(o) => Some(o),
            Output::Write(_) => None,
        }
    }

    /// What it writes (`write`); `None` for a rule that draws.
    pub fn write(&self) -> Option<&Write> {
        match &self.output {
            Output::Write(w) => Some(w),
            Output::Draw(_) => None,
        }
    }

    pub fn write_mut(&mut self) -> Option<&mut Write> {
        match &mut self.output {
            Output::Write(w) => Some(w),
            Output::Draw(_) => None,
        }
    }

    /// `match`, then every `require` pattern; none for a region rule.
    pub fn patterns(&self) -> impl Iterator<Item = &Pattern> {
        self.as_match().into_iter().flat_map(MatchSelect::patterns)
    }

    /// [`Rule::patterns`], mutably.
    pub fn patterns_mut(&mut self) -> impl Iterator<Item = &mut Pattern> {
        self.as_match_mut()
            .into_iter()
            .flat_map(MatchSelect::patterns_mut)
    }

    /// Every int_grid layer this rule reads, given the auto layer's
    /// `source`: each pattern's `grid`, or the region's `grid` (all default
    /// to `source`). In order, with repeats.
    pub fn grids_read<'a>(&'a self, source: &'a Slug) -> Vec<&'a Slug> {
        match &self.select {
            Select::Match(m) => m.patterns().map(|p| p.grid_or(source)).collect(),
            Select::Region(r) => vec![r.region.grid_or(source)],
        }
    }
}

/// `region`: which cells form regions (4-connected components of cells
/// passing `cells`), and which regions are selected (every given bound holds).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Region {
    /// The int_grid layer read (a derived one by its effective values).
    /// Default: the auto layer's `source`.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Slug")]
    pub grid: Option<Slug>,
    /// Which cells belong to regions: a cell test (`"."`, `"!solid"`,
    /// `"[.|oneway]"`, ...).
    pub cells: CellMatch,
    /// Bounding-box width in cells, inclusive.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "NonZeroU32")]
    pub min_width: Option<NonZeroU32>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "NonZeroU32")]
    pub max_width: Option<NonZeroU32>,
    /// Bounding-box height in cells, inclusive.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "NonZeroU32")]
    pub min_height: Option<NonZeroU32>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "NonZeroU32")]
    pub max_height: Option<NonZeroU32>,
    /// Number of cells, inclusive.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "NonZeroU32")]
    pub min_area: Option<NonZeroU32>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "NonZeroU32")]
    pub max_area: Option<NonZeroU32>,
}

impl Region {
    /// A region of cells passing `cells` on the auto layer's source, no bounds.
    pub fn new(cells: CellMatch) -> Self {
        Region {
            grid: None,
            cells,
            min_width: None,
            max_width: None,
            min_height: None,
            max_height: None,
            min_area: None,
            max_area: None,
        }
    }

    /// The grid this region reads, given the auto layer's `source`.
    pub fn grid_or<'a>(&'a self, source: &'a Slug) -> &'a Slug {
        self.grid.as_ref().unwrap_or(source)
    }

    /// Whether a region with this bounding-box size and cell count is
    /// selected: every given bound holds (bounds are inclusive).
    pub fn selects(&self, width: u32, height: u32, area: u32) -> bool {
        let within = |v: u32, lo: Option<NonZeroU32>, hi: Option<NonZeroU32>| {
            lo.is_none_or(|lo| v >= lo.get()) && hi.is_none_or(|hi| v <= hi.get())
        };
        within(width, self.min_width, self.max_width)
            && within(height, self.min_height, self.max_height)
            && within(area, self.min_area, self.max_area)
    }

    /// `(min key, min, max key, max)` for each bound pair that has both.
    pub fn bound_pairs(&self) -> Vec<(&'static str, u32, &'static str, u32)> {
        [
            ("min_width", self.min_width, "max_width", self.max_width),
            ("min_height", self.min_height, "max_height", self.max_height),
            ("min_area", self.min_area, "max_area", self.max_area),
        ]
        .into_iter()
        .filter_map(|(a, lo, b, hi)| Some((a, lo?.get(), b, hi?.get())))
        .collect()
    }
}

/// `place`: where in each selected region a region rule fires.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "PlaceRepr", into = "PlaceRepr")]
pub struct Place {
    /// `along` an edge (each run of that edge on its own), or `across` the
    /// interior (a lattice over the region's bounding box).
    pub at: PlaceAt,
    /// The 1D strategy. Default `even`.
    pub spacing: Spacing,
    /// Pitch (`even`) or minimum distance (`spread`, `random`), in cells.
    /// Default 1. `[x, y]` only with `across`.
    pub every: PerAxis<NonZeroU32>,
    /// Cells kept clear at each end of a run (of the bounding box per axis
    /// with `across`). Default 0. `[x, y]` only with `across`.
    pub margin: PerAxis<u32>,
    /// `even` only: where the leftover goes. Default `center`.
    pub align: Align,
    /// `random` only: extra random distance `0..=jitter` added to each step.
    pub jitter: u32,
    /// `along` only: runs shorter than this (before `margin`) are skipped.
    /// Default 1.
    pub min_length: NonZeroU32,
    /// `along` only: the neighbour across the edge must also pass this.
    pub against: Option<CellMatch>,
}

impl Place {
    /// Every option at its default, at `at`.
    pub fn new(at: PlaceAt) -> Self {
        Place {
            at,
            spacing: Spacing::Even,
            every: PerAxis::Same(NonZeroU32::MIN),
            margin: PerAxis::Same(0),
            align: Align::Center,
            jitter: 0,
            min_length: NonZeroU32::MIN,
            against: None,
        }
    }

    /// `every` as `[x, y]` (a single value applies to both axes).
    pub fn every_xy(&self) -> [u32; 2] {
        self.every.xy().map(NonZeroU32::get)
    }

    /// `margin` as `[x, y]`.
    pub fn margin_xy(&self) -> [u32; 2] {
        self.margin.xy()
    }
}

/// Where a region rule places: along one edge, or across the interior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PlaceAt {
    Along(Edge),
    /// `"across": "interior"`.
    Across,
}

/// A region edge. A region cell is on an edge when its neighbour on that
/// side is inside the level and not a region cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Edge {
    /// The neighbour below.
    Floor,
    /// The neighbour above.
    Ceiling,
    /// The neighbour to the left.
    WallLeft,
    /// The neighbour to the right.
    WallRight,
}

/// The value `across` takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum Interior {
    Interior,
}

/// A region rule's 1D spacing strategy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Spacing {
    /// Fixed pitch `every`, the leftover placed by `align`.
    #[default]
    Even,
    /// As many as fit at least `every` apart, spread end to end.
    Spread,
    /// Seeded: steps of `every` plus `0..=jitter`.
    Random,
}

/// Where `even` spacing puts the leftover.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Align {
    /// Left (floor, ceiling) or top (walls).
    Start,
    /// Half the leftover each side; an odd leftover leans to the start.
    #[default]
    Center,
    End,
}

/// One number for both axes, or `[x, y]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PerAxis<T> {
    Same(T),
    Each([T; 2]),
}

impl<T: Copy> PerAxis<T> {
    /// `[x, y]`.
    pub fn xy(&self) -> [T; 2] {
        match *self {
            PerAxis::Same(v) => [v, v],
            PerAxis::Each(v) => v,
        }
    }
}

impl<T: Serialize> Serialize for PerAxis<T> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            PerAxis::Same(v) => v.serialize(s),
            PerAxis::Each(v) => v.serialize(s),
        }
    }
}

impl<'de, T: serde::de::DeserializeOwned> Deserialize<'de> for PerAxis<T> {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let v = serde_json::Value::deserialize(d)?;
        if v.is_array() {
            serde_json::from_value(v)
                .map(PerAxis::Each)
                .map_err(|e| D::Error::custom(format!("expected [x, y]: {e}")))
        } else {
            serde_json::from_value(v)
                .map(PerAxis::Same)
                .map_err(|e| D::Error::custom(format!("expected a number or [x, y]: {e}")))
        }
    }
}

impl<T: JsonSchema> JsonSchema for PerAxis<T> {
    fn schema_name() -> Cow<'static, str> {
        format!("PerAxis_{}", T::schema_name()).into()
    }
    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        let one = generator.subschema_for::<T>();
        json_schema!({
            "description": "One number for both axes, or [x, y] (only with \"across\").",
            "anyOf": [
                one,
                { "type": "array", "items": one, "minItems": 2, "maxItems": 2 }
            ]
        })
    }
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PlaceRepr {
    /// Place on each run of this edge: `floor`, `ceiling`, `wall_left` or
    /// `wall_right`. Give this or `across`.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Edge")]
    along: Option<Edge>,
    /// `"interior"`: a lattice over the region's bounding box, kept where
    /// the cell is a region cell. Give this or `along`.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Interior")]
    across: Option<Interior>,
    /// `even` (default), `spread` or `random`.
    #[serde(default, skip_serializing_if = "is_default")]
    spacing: Spacing,
    /// Pitch or minimum distance in cells, at least 1. Default 1.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "PerAxis<NonZeroU32>")]
    every: Option<PerAxis<NonZeroU32>>,
    /// Cells kept clear at each end. Default 0.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "PerAxis<u32>")]
    margin: Option<PerAxis<u32>>,
    /// `even` only: `start`, `center` (default) or `end`.
    #[serde(default, skip_serializing_if = "is_default")]
    align: Align,
    /// `random` only: extra random distance per step. Default 0.
    #[serde(default, skip_serializing_if = "is_default")]
    jitter: u32,
    /// `along` only: skip runs shorter than this. Default 1.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "NonZeroU32")]
    min_length: Option<NonZeroU32>,
    /// `along` only: the neighbour across the edge must pass this cell test.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "CellMatch")]
    against: Option<CellMatch>,
}

impl TryFrom<PlaceRepr> for Place {
    type Error = String;
    fn try_from(r: PlaceRepr) -> Result<Self, String> {
        let at = match (r.along, r.across) {
            (Some(e), None) => PlaceAt::Along(e),
            (None, Some(Interior::Interior)) => PlaceAt::Across,
            _ => {
                return Err("\"place\" needs exactly one of \"along\" (an edge: floor, \
                            ceiling, wall_left, wall_right) and \"across\" (\"interior\")"
                    .into());
            }
        };
        if let PlaceAt::Along(_) = at {
            for (key, pair) in [
                ("every", matches!(r.every, Some(PerAxis::Each(_)))),
                ("margin", matches!(r.margin, Some(PerAxis::Each(_)))),
            ] {
                if pair {
                    return Err(format!(
                        "\"{key}\": [x, y] only works with \"across\"; along an edge it is one \
                         number"
                    ));
                }
            }
        } else {
            for (key, on) in [
                ("min_length", r.min_length.is_some()),
                ("against", r.against.is_some()),
            ] {
                if on {
                    return Err(format!(
                        "\"{key}\" is only allowed with \"along\", not with \"across\""
                    ));
                }
            }
        }
        Ok(Place {
            at,
            spacing: r.spacing,
            every: r.every.unwrap_or(PerAxis::Same(NonZeroU32::MIN)),
            margin: r.margin.unwrap_or(PerAxis::Same(0)),
            align: r.align,
            jitter: r.jitter,
            min_length: r.min_length.unwrap_or(NonZeroU32::MIN),
            against: r.against,
        })
    }
}

impl From<Place> for PlaceRepr {
    fn from(p: Place) -> Self {
        let (along, across) = match p.at {
            PlaceAt::Along(e) => (Some(e), None),
            PlaceAt::Across => (None, Some(Interior::Interior)),
        };
        PlaceRepr {
            along,
            across,
            spacing: p.spacing,
            every: (p.every != PerAxis::Same(NonZeroU32::MIN)).then_some(p.every),
            margin: (p.margin != PerAxis::Same(0)).then_some(p.margin),
            align: p.align,
            jitter: p.jitter,
            min_length: (p.min_length != NonZeroU32::MIN).then_some(p.min_length),
            against: p.against,
        }
    }
}

const PLACE_VARIANTS: &[VariantKeys] = &[
    VariantKeys {
        name: "along",
        keys: &["along", "min_length", "against"],
        required: &["along"],
    },
    VariantKeys {
        name: "across",
        keys: &["across"],
        required: &["across"],
    },
];

impl JsonSchema for Place {
    fn schema_name() -> Cow<'static, str> {
        "Place".into()
    }
    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        variant_schema(
            PlaceRepr::json_schema(generator),
            None,
            PLACE_VARIANTS,
            "Where in each selected region a region rule fires: along an edge or across the \
             interior.",
        )
    }
}

/// `write`: what a rule writes into a derived grid where it fires.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Write {
    /// A derived `int_grid` layer, on the auto layer's grid.
    pub grid: Slug,
    /// A value (not a group) of that layer.
    pub value: Slug,
}

/// `require` on disk: one pattern, or a non-empty list of them (all must
/// hold). Saving writes a single pattern when there is one.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct RequireRepr(Vec<Pattern>);

impl Serialize for RequireRepr {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self.0.as_slice() {
            [one] => one.serialize(s),
            many => many.serialize(s),
        }
    }
}

impl<'de> Deserialize<'de> for RequireRepr {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = RequireRepr;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a pattern, or a non-empty array of patterns")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                a: A,
            ) -> Result<RequireRepr, A::Error> {
                Pattern::deserialize(serde::de::value::MapAccessDeserializer::new(a))
                    .map(|p| RequireRepr(vec![p]))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                a: A,
            ) -> Result<RequireRepr, A::Error> {
                let v =
                    Vec::<Pattern>::deserialize(serde::de::value::SeqAccessDeserializer::new(a))?;
                if v.is_empty() {
                    return Err(serde::de::Error::custom(
                        "\"require\" is a pattern or a non-empty array of patterns; leave it out \
                         for none",
                    ));
                }
                Ok(RequireRepr(v))
            }
        }
        d.deserialize_any(V)
    }
}

impl JsonSchema for RequireRepr {
    fn schema_name() -> Cow<'static, str> {
        "Require".into()
    }
    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        let one = generator.subschema_for::<Pattern>();
        json_schema!({
            "description": "Patterns that must also hold, anchored on the same cell: one pattern, or a non-empty array (all must hold).",
            "anyOf": [one, { "type": "array", "items": one, "minItems": 1 }]
        })
    }
}

/// serde helpers for a `require` stored as a `Vec<Pattern>`.
mod require_list {
    use super::{Pattern, RequireRepr};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(v: &[Pattern], s: S) -> Result<S::Ok, S::Error> {
        RequireRepr(v.to_vec()).serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<Pattern>, D::Error> {
        RequireRepr::deserialize(d).map(|r| r.0)
    }
}

/// A rule as it is on disk: every key either kind may have.
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RuleRepr {
    /// Unique within the rules file; part of the hash behind the rule's
    /// random choices.
    slug: Slug,
    /// Default true.
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    active: bool,
    /// Pattern rule: what must surround the anchor cell.
    #[serde(rename = "match", default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Pattern")]
    pattern: Option<Pattern>,
    /// Region rule: which cells form regions and which regions are selected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Region")]
    region: Option<Region>,
    /// Region rule: where in each selected region it fires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Place")]
    place: Option<Place>,
    /// Pattern rule: patterns that must also hold at the anchor.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "RequireRepr")]
    require: Option<RequireRepr>,
    /// Pattern rule: extra mirrored or rotated variants to try.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Vec<Symmetry>")]
    symmetry: Option<Vec<Symmetry>>,
    /// What to draw. Give this or `write`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "RuleOutput")]
    out: Option<RuleOutput>,
    /// What to write into a derived grid. Give this or `out`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Write")]
    write: Option<Write>,
    /// Pattern rule: probability 0 to 1 that a matching cell fires. Default 1.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "f64")]
    chance: Option<f64>,
    /// After this rule fires at an anchor, later rules of the pass skip it.
    #[serde(rename = "break", default, skip_serializing_if = "is_default")]
    break_on_match: bool,
    /// Pattern rule.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Modulo")]
    modulo: Option<Modulo>,
    /// Pattern rule.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Checker")]
    checker: Option<Checker>,
    /// Pattern rule.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Perlin")]
    perlin: Option<Perlin>,
    /// Pattern rule: what pattern cells outside the level read as.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "OutOfBounds")]
    out_of_bounds: Option<OutOfBounds>,
    /// Extra integer mixed into this rule's random choices. Default 0.
    #[serde(default, skip_serializing_if = "is_default")]
    salt: i64,
}

/// The keys only a pattern rule may have, with whether `r` has each.
fn match_only_keys(r: &RuleRepr) -> [(&'static str, bool); 7] {
    [
        ("require", r.require.is_some()),
        ("symmetry", r.symmetry.is_some()),
        ("chance", r.chance.is_some()),
        ("modulo", r.modulo.is_some()),
        ("checker", r.checker.is_some()),
        ("perlin", r.perlin.is_some()),
        ("out_of_bounds", r.out_of_bounds.is_some()),
    ]
}

impl TryFrom<RuleRepr> for Rule {
    type Error = String;
    fn try_from(r: RuleRepr) -> Result<Self, String> {
        let only_match = match_only_keys(&r);
        let select = match (r.pattern, r.region) {
            (Some(_), Some(_)) => {
                return Err(
                    "a rule has either \"match\" (a pattern rule) or \"region\" (a \
                            region rule), not both"
                        .into(),
                );
            }
            (None, None) => {
                return Err(
                    "missing field `match` (or `region` and `place`, for a region rule)".into(),
                );
            }
            (Some(pattern), None) => {
                if r.place.is_some() {
                    return Err("\"place\" is only allowed on a region rule (one with \
                                \"region\")"
                        .into());
                }
                Select::Match(MatchSelect {
                    pattern,
                    require: r.require.map(|q| q.0).unwrap_or_default(),
                    symmetry: r.symmetry.unwrap_or_default(),
                    chance: r.chance.unwrap_or(1.0),
                    modulo: r.modulo,
                    checker: r.checker.unwrap_or_default(),
                    perlin: r.perlin,
                    out_of_bounds: r.out_of_bounds,
                })
            }
            (None, Some(region)) => {
                if let Some((k, _)) = only_match.iter().find(|(_, on)| *on) {
                    return Err(format!("{k:?} is not allowed on a region rule"));
                }
                let place = r
                    .place
                    .ok_or("a region rule needs \"place\" (where in each region it fires)")?;
                Select::Region(RegionSelect { region, place })
            }
        };
        let output = match (r.out, r.write) {
            (Some(o), None) => Output::Draw(o),
            (None, Some(w)) => Output::Write(w),
            (Some(_), Some(_)) => {
                return Err(
                    "a rule has either \"out\" (draw) or \"write\" (write a derived \
                            grid), not both"
                        .into(),
                );
            }
            (None, None) => {
                return Err("missing field `out` (or `write`, to write a derived grid)".into());
            }
        };
        Ok(Rule {
            slug: r.slug,
            active: r.active,
            select,
            output,
            break_on_match: r.break_on_match,
            salt: r.salt,
        })
    }
}

impl From<Rule> for RuleRepr {
    fn from(r: Rule) -> Self {
        let mut out = RuleRepr {
            slug: r.slug,
            active: r.active,
            pattern: None,
            region: None,
            place: None,
            require: None,
            symmetry: None,
            out: None,
            write: None,
            chance: None,
            break_on_match: r.break_on_match,
            modulo: None,
            checker: None,
            perlin: None,
            out_of_bounds: None,
            salt: r.salt,
        };
        match r.select {
            Select::Match(m) => {
                out.pattern = Some(m.pattern);
                out.require = (!m.require.is_empty()).then_some(RequireRepr(m.require));
                out.symmetry = (!m.symmetry.is_empty()).then_some(m.symmetry);
                out.chance = (m.chance != 1.0).then_some(m.chance);
                out.modulo = m.modulo;
                out.checker = (m.checker != Checker::None).then_some(m.checker);
                out.perlin = m.perlin;
                out.out_of_bounds = m.out_of_bounds;
            }
            Select::Region(g) => {
                out.region = Some(g.region);
                out.place = Some(g.place);
            }
        }
        match r.output {
            Output::Draw(o) => out.out = Some(o),
            Output::Write(w) => out.write = Some(w),
        }
        out
    }
}

const RULE_VARIANTS: &[VariantKeys] = &[
    VariantKeys {
        name: "pattern rule drawing",
        keys: &[
            "match",
            "require",
            "symmetry",
            "chance",
            "modulo",
            "checker",
            "perlin",
            "out_of_bounds",
            "out",
        ],
        required: &["match", "out"],
    },
    VariantKeys {
        name: "pattern rule writing",
        keys: &[
            "match",
            "require",
            "symmetry",
            "chance",
            "modulo",
            "checker",
            "perlin",
            "out_of_bounds",
            "write",
        ],
        required: &["match", "write"],
    },
    VariantKeys {
        name: "region rule drawing",
        keys: &["region", "place", "out"],
        required: &["region", "place", "out"],
    },
    VariantKeys {
        name: "region rule writing",
        keys: &["region", "place", "write"],
        required: &["region", "place", "write"],
    },
];

impl JsonSchema for Rule {
    fn schema_name() -> Cow<'static, str> {
        "Rule".into()
    }
    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        variant_schema(
            RuleRepr::json_schema(generator),
            None,
            RULE_VARIANTS,
            "A rule: a pattern rule (match) or a region rule (region + place), that draws (out) \
             or writes a derived grid (write).",
        )
    }
}

/// A rectangular grid pattern, anchored at its `@` cell, else its center.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Pattern {
    /// The int_grid layer to read. Default: the auto layer's `source`.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Slug")]
    pub grid: Option<Slug>,
    /// Glyph to cell test, for glyphs that don't mean what they mean by
    /// default (see [`Pattern::resolve`]); or the name of a legend in
    /// `rules/macros.json`.
    #[serde(default, skip_serializing_if = "Legend::is_empty")]
    pub legend: Legend,
    /// Rows of glyphs, top first; or the name of a pattern in
    /// `rules/macros.json`. The anchor is the `@` cell, else the center
    /// (then width and height must be odd).
    pub rows: Rows,
}

/// The glyph used to mark a pattern's anchor cell when it isn't the center.
pub const ANCHOR_GLYPH: char = '@';

impl Pattern {
    /// A pattern on the auto layer's source grid, with no legend.
    pub fn new(rows: &[&str]) -> Self {
        Pattern {
            grid: None,
            legend: Legend::default(),
            rows: Rows::Inline(rows.iter().map(|r| r.to_string()).collect()),
        }
    }

    /// The rows written inline; empty for named rows (resolve those first:
    /// [`crate::Project::expanded_rules`] always gives inline rows).
    pub fn inline_rows(&self) -> &[String] {
        match &self.rows {
            Rows::Inline(r) => r,
            Rows::Named(_) => &[],
        }
    }

    /// The legend written inline; `None` for a named legend.
    pub fn inline_legend(&self) -> Option<&BTreeMap<char, CellMatch>> {
        match &self.legend {
            Legend::Inline(m) => Some(m),
            Legend::Named(_) => None,
        }
    }

    /// Whether the rows or the legend name something in `rules/macros.json`.
    pub fn is_named(&self) -> bool {
        matches!(self.rows, Rows::Named(_)) || matches!(self.legend, Legend::Named(_))
    }

    /// `[width, height]` of the inline rows, taking width from the first row.
    pub fn size(&self) -> [u32; 2] {
        let rows = self.inline_rows();
        let w = rows.first().map_or(0, |r| r.chars().count());
        [w as u32, rows.len() as u32]
    }

    /// `(x, y)` of the anchor cell in the inline rows: the first `@`, else
    /// the center (rounded down for an even size, which only `@` allows).
    pub fn anchor(&self) -> (u32, u32) {
        for (y, row) in self.inline_rows().iter().enumerate() {
            if let Some(x) = row.chars().position(|c| c == ANCHOR_GLYPH) {
                return (x as u32, y as u32);
            }
        }
        let [w, h] = self.size();
        (w / 2, h / 2)
    }

    /// How many `@` glyphs the inline rows hold (at most one is allowed).
    pub fn anchor_count(&self) -> usize {
        self.inline_rows()
            .iter()
            .map(|r| r.chars().filter(|&c| c == ANCHOR_GLYPH).count())
            .sum()
    }

    /// The grid this pattern reads, given the auto layer's `source`.
    pub fn grid_or<'a>(&'a self, source: &'a Slug) -> &'a Slug {
        self.grid.as_ref().unwrap_or(source)
    }

    /// How a glyph is tested, given the int_grid layer the pattern reads:
    ///
    /// 1. the pattern's (inline) `legend`;
    /// 2. the built-ins `*` (anything), `+` (any value), `.` (empty) and
    ///    `@` (the anchor; anything);
    /// 3. a value glyph of `grid`: that value;
    /// 4. a group glyph of `grid`: any value in that group.
    ///
    /// `None` if the glyph means nothing.
    pub fn resolve(&self, glyph: char, grid: Option<&LayerDef>) -> Option<CellMatch> {
        if let Some(m) = self.inline_legend().and_then(|l| l.get(&glyph)) {
            return Some(m.clone());
        }
        match glyph {
            '*' | ANCHOR_GLYPH => return Some(CellMatch::Any),
            '+' => return Some(CellMatch::NonEmpty),
            '.' => return Some(CellMatch::Empty),
            _ => {}
        }
        let def = grid?;
        if let Some(v) = def.value_by_glyph(glyph) {
            return Some(CellMatch::Is(v.slug.clone()));
        }
        def.groups()
            .iter()
            .find(|g| g.glyph == Some(glyph))
            .map(|g| CellMatch::Is(g.slug.clone()))
    }

    /// What `glyph` means without a legend entry (steps 2 to 4 of
    /// [`Pattern::resolve`]).
    pub fn default_meaning(glyph: char, grid: Option<&LayerDef>) -> Option<CellMatch> {
        Pattern {
            grid: None,
            legend: Legend::default(),
            rows: Rows::Inline(Vec::new()),
        }
        .resolve(glyph, grid)
    }
}

/// A pattern's `rows`: written inline, or the name of a pattern in
/// `rules/macros.json` `patterns`. On disk an array of strings or a string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rows {
    Inline(Vec<String>),
    Named(Slug),
}

impl Default for Rows {
    fn default() -> Self {
        Rows::Inline(Vec::new())
    }
}

impl From<Vec<String>> for Rows {
    fn from(rows: Vec<String>) -> Self {
        Rows::Inline(rows)
    }
}

impl Serialize for Rows {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Rows::Inline(r) => r.serialize(s),
            Rows::Named(n) => n.serialize(s),
        }
    }
}

impl<'de> Deserialize<'de> for Rows {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = Rows;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("an array of row strings, or the name of a pattern in macros.json")
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Rows, E> {
                Slug::new(v).map(Rows::Named).map_err(E::custom)
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(self, a: A) -> Result<Rows, A::Error> {
                Vec::<String>::deserialize(serde::de::value::SeqAccessDeserializer::new(a))
                    .map(Rows::Inline)
            }
        }
        d.deserialize_any(V)
    }
}

impl JsonSchema for Rows {
    fn schema_name() -> Cow<'static, str> {
        "Rows".into()
    }
    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        let named = generator.subschema_for::<Slug>();
        json_schema!({
            "description": "Rows of glyphs, top first, or the name of a pattern in rules/macros.json.",
            "anyOf": [
                { "type": "array", "items": { "type": "string" } },
                named
            ]
        })
    }
}

/// A pattern's `legend`: written inline, or the name of a legend in
/// `rules/macros.json` `legends`. On disk an object or a string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Legend {
    Inline(BTreeMap<char, CellMatch>),
    Named(Slug),
}

impl Default for Legend {
    fn default() -> Self {
        Legend::Inline(BTreeMap::new())
    }
}

impl Legend {
    /// An empty inline legend (left out when saving).
    pub fn is_empty(&self) -> bool {
        matches!(self, Legend::Inline(m) if m.is_empty())
    }
}

impl From<BTreeMap<char, CellMatch>> for Legend {
    fn from(m: BTreeMap<char, CellMatch>) -> Self {
        Legend::Inline(m)
    }
}

impl Serialize for Legend {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Legend::Inline(m) => m.serialize(s),
            Legend::Named(n) => n.serialize(s),
        }
    }
}

impl<'de> Deserialize<'de> for Legend {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = Legend;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str(
                    "an object of glyph to cell test, or the name of a legend in macros.json",
                )
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Legend, E> {
                Slug::new(v).map(Legend::Named).map_err(E::custom)
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(self, a: A) -> Result<Legend, A::Error> {
                BTreeMap::<char, CellMatch>::deserialize(
                    serde::de::value::MapAccessDeserializer::new(a),
                )
                .map(Legend::Inline)
            }
        }
        d.deserialize_any(V)
    }
}

impl JsonSchema for Legend {
    fn schema_name() -> Cow<'static, str> {
        "Legend".into()
    }
    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        let inline = generator.subschema_for::<BTreeMap<char, CellMatch>>();
        let named = generator.subschema_for::<Slug>();
        json_schema!({
            "description": "Glyph to cell test, or the name of a legend in rules/macros.json.",
            "anyOf": [inline, named]
        })
    }
}

/// A single-cell test. Written as a string: `*`, `+`, `.`, `slug`, `!slug`,
/// `[a|b]` or `![a|b]`, where a slug names an IntGrid value or value group.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum CellMatch {
    /// `*`: anything, don't care.
    Any,
    /// `+`: any value, not empty.
    NonEmpty,
    /// `.`: empty.
    Empty,
    /// `slug`: this value, or any value in this group.
    Is(Slug),
    /// `!slug`: anything else, including empty.
    Not(Slug),
    /// `[a|b]`: any of the members.
    OneOf(Vec<CellTest>),
    /// `![a|b]`: none of the members.
    NotOneOf(Vec<CellTest>),
}

/// One member of `[a|b]`: `.` (empty), `+` (any value), or a value or group
/// slug.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CellTest {
    Empty,
    NonEmpty,
    Is(Slug),
}

impl fmt::Display for CellTest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CellTest::Empty => f.write_str("."),
            CellTest::NonEmpty => f.write_str("+"),
            CellTest::Is(s) => write!(f, "{s}"),
        }
    }
}

impl CellMatch {
    /// The value or group slugs this test names.
    pub fn slugs(&self) -> Vec<&Slug> {
        match self {
            CellMatch::Is(s) | CellMatch::Not(s) => vec![s],
            CellMatch::OneOf(m) | CellMatch::NotOneOf(m) => m
                .iter()
                .filter_map(|t| match t {
                    CellTest::Is(s) => Some(s),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        }
    }
}

const CELL_TEST_FORMS: &str =
    "expected *, +, ., a value slug, !slug, [a|b] or ![a|b] (members ., + or slugs)";

impl TryFrom<String> for CellMatch {
    type Error = String;
    fn try_from(s: String) -> Result<Self, String> {
        let bad = || format!("invalid cell test {s:?}: {CELL_TEST_FORMS}");
        match s.as_str() {
            "*" => return Ok(CellMatch::Any),
            "+" => return Ok(CellMatch::NonEmpty),
            "." => return Ok(CellMatch::Empty),
            _ => {}
        }
        let (neg, rest) = match s.strip_prefix('!') {
            Some(rest) => (true, rest),
            None => (false, s.as_str()),
        };
        if let Some(inner) = rest.strip_prefix('[') {
            let inner = inner.strip_suffix(']').ok_or_else(bad)?;
            let members: Option<Vec<CellTest>> = inner
                .split('|')
                .map(|m| match m {
                    "." => Some(CellTest::Empty),
                    "+" => Some(CellTest::NonEmpty),
                    _ => Slug::new(m).ok().map(CellTest::Is),
                })
                .collect();
            let members = members.ok_or_else(bad)?;
            return Ok(if neg {
                CellMatch::NotOneOf(members)
            } else {
                CellMatch::OneOf(members)
            });
        }
        let slug = Slug::new(rest).map_err(|_| bad())?;
        Ok(if neg {
            CellMatch::Not(slug)
        } else {
            CellMatch::Is(slug)
        })
    }
}

impl From<CellMatch> for String {
    fn from(m: CellMatch) -> String {
        m.to_string()
    }
}

impl fmt::Display for CellMatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let list = |f: &mut fmt::Formatter<'_>, m: &[CellTest]| {
            let parts: Vec<String> = m.iter().map(|t| t.to_string()).collect();
            write!(f, "[{}]", parts.join("|"))
        };
        match self {
            CellMatch::Any => f.write_str("*"),
            CellMatch::NonEmpty => f.write_str("+"),
            CellMatch::Empty => f.write_str("."),
            CellMatch::Is(s) => write!(f, "{s}"),
            CellMatch::Not(s) => write!(f, "!{s}"),
            CellMatch::OneOf(m) => list(f, m),
            CellMatch::NotOneOf(m) => {
                f.write_str("!")?;
                list(f, m)
            }
        }
    }
}

impl JsonSchema for CellMatch {
    fn schema_name() -> Cow<'static, str> {
        "CellMatch".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "pattern": "^([*+.]|!?[a-z][a-z0-9_]*|!?\\[([+.]|[a-z][a-z0-9_]*)(\\|([+.]|[a-z][a-z0-9_]*))*\\])$",
            "description": "Cell test: * anything, + any value, . empty, slug = that value or group, !slug = not that value or group, [a|b] = any of the members, ![a|b] = none of them (members: ., + or slugs)."
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Symmetry {
    /// Also try the pattern flipped left-right; output tiles are flipped to match.
    MirrorX,
    /// Also try the pattern flipped top-bottom.
    MirrorY,
    /// Also try the three 90-degree rotations; with a mirror, all eight
    /// orientations. Needs square tiles.
    Rotate,
}

/// What a rule draws: tiles from a tileset, or a named stamp.
///
/// On disk this is one object without a tag: `tileset` + `tiles` (+ tile
/// options), or `stamp`; the drawing options (`avoid_overlap`, `alpha`,
/// `offset_px`, `random_offset_px`) go with either. (A rule that writes a
/// derived grid has `write` instead of `out`: [`Output::Write`].)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "RuleOutputRepr", into = "RuleOutputRepr")]
pub enum RuleOutput {
    Tiles(TilesOutput),
    Stamp(StampOutput),
}

/// Draw one of several tile alternatives.
#[derive(Debug, Clone, PartialEq)]
pub struct TilesOutput {
    /// Tileset to draw from. Each rule names its own.
    pub tileset: Slug,
    /// Alternatives; one is picked per cell by weight.
    pub tiles: Vec<TileAlt>,
    pub tile_mode: TileMode,
    /// stamp mode: which point of the alternative's pixel bounding box sits
    /// on the same point of the matching cell, 0 to 1 per axis (LDtk
    /// pivotX/Y). Default `[0, 0]`.
    pub pivot: [f64; 2],
    /// Skip a placement whose footprint has an occupied cell.
    pub avoid_overlap: bool,
    pub draw: Draw,
}

/// Place a named stamp from the rules file's `stamps`.
#[derive(Debug, Clone, PartialEq)]
pub struct StampOutput {
    /// A stamp slug.
    pub stamp: Slug,
    /// Skip a placement whose footprint has an occupied cell.
    pub avoid_overlap: bool,
    pub draw: Draw,
}

/// How output tiles are drawn.
#[derive(Debug, Clone, PartialEq)]
pub struct Draw {
    /// Opacity 0 to 1. Default 1.
    pub alpha: f64,
    /// Fixed pixel offset. Default `[0, 0]`.
    pub offset_px: [i32; 2],
    pub random_offset_px: Option<RandomOffset>,
}

impl Default for Draw {
    fn default() -> Self {
        Draw {
            alpha: 1.0,
            offset_px: [0, 0],
            random_offset_px: None,
        }
    }
}

impl RuleOutput {
    /// Draw one of `tiles` from `tileset` (stack mode), every option at its default.
    pub fn tiles(tileset: Slug, tiles: Vec<TileAlt>) -> Self {
        RuleOutput::Tiles(TilesOutput {
            tileset,
            tiles,
            tile_mode: TileMode::Stack,
            pivot: [0.0, 0.0],
            avoid_overlap: false,
            draw: Draw::default(),
        })
    }

    /// Place the named stamp, every option at its default.
    pub fn stamp(stamp: Slug) -> Self {
        RuleOutput::Stamp(StampOutput {
            stamp,
            avoid_overlap: false,
            draw: Draw::default(),
        })
    }

    pub fn draw(&self) -> &Draw {
        match self {
            RuleOutput::Tiles(t) => &t.draw,
            RuleOutput::Stamp(s) => &s.draw,
        }
    }

    pub fn draw_mut(&mut self) -> &mut Draw {
        match self {
            RuleOutput::Tiles(t) => &mut t.draw,
            RuleOutput::Stamp(s) => &mut s.draw,
        }
    }

    /// `avoid_overlap`: skip a placement whose footprint is occupied.
    pub fn avoid_overlap(&self) -> bool {
        match self {
            RuleOutput::Tiles(t) => t.avoid_overlap,
            RuleOutput::Stamp(s) => s.avoid_overlap,
        }
    }

    pub fn as_tiles(&self) -> Option<&TilesOutput> {
        match self {
            RuleOutput::Tiles(t) => Some(t),
            RuleOutput::Stamp(_) => None,
        }
    }

    pub fn as_tiles_mut(&mut self) -> Option<&mut TilesOutput> {
        match self {
            RuleOutput::Tiles(t) => Some(t),
            RuleOutput::Stamp(_) => None,
        }
    }
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RuleOutputRepr {
    /// Tileset to draw from (with `tiles`).
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Slug")]
    tileset: Option<Slug>,
    /// Alternatives; one is picked per cell by weight (with `tileset`).
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Vec<TileAlt>")]
    tiles: Option<Vec<TileAlt>>,
    /// With `tiles`: `stack` (default) or `stamp`.
    #[serde(default, skip_serializing_if = "is_default")]
    tile_mode: TileMode,
    /// With `tile_mode: stamp`: anchor inside the alternative's box. Default `[0, 0]`.
    #[serde(default, skip_serializing_if = "is_default")]
    pivot: [f64; 2],
    /// A stamp slug from this file's `stamps` (instead of `tileset` + `tiles`).
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Slug")]
    stamp: Option<Slug>,
    /// Skip a placement whose footprint has an occupied cell. Default false.
    #[serde(default, skip_serializing_if = "is_default")]
    avoid_overlap: bool,
    /// Opacity 0 to 1. Default 1.
    #[serde(default = "one", skip_serializing_if = "is_one")]
    alpha: f64,
    /// Fixed pixel offset. Default `[0, 0]`.
    #[serde(default, skip_serializing_if = "is_default")]
    offset_px: [i32; 2],
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "RandomOffset")]
    random_offset_px: Option<RandomOffset>,
}

const OUTPUT_VARIANTS: &[VariantKeys] = &[
    VariantKeys {
        name: "tiles",
        keys: &["tileset", "tiles", "tile_mode", "pivot"],
        required: &["tileset", "tiles"],
    },
    VariantKeys {
        name: "stamp",
        keys: &["stamp"],
        required: &["stamp"],
    },
];

impl TryFrom<RuleOutputRepr> for RuleOutput {
    type Error = String;
    fn try_from(r: RuleOutputRepr) -> Result<Self, String> {
        let draw = Draw {
            alpha: r.alpha,
            offset_px: r.offset_px,
            random_offset_px: r.random_offset_px,
        };
        match (r.tileset, r.tiles, r.stamp) {
            (Some(tileset), Some(tiles), None) => Ok(RuleOutput::Tiles(TilesOutput {
                tileset,
                tiles,
                tile_mode: r.tile_mode,
                pivot: r.pivot,
                avoid_overlap: r.avoid_overlap,
                draw,
            })),
            (None, None, Some(stamp)) => {
                misplaced(
                    "stamp",
                    "output",
                    &[
                        ("tile_mode", r.tile_mode != TileMode::Stack),
                        ("pivot", r.pivot != [0.0, 0.0]),
                    ],
                )?;
                Ok(RuleOutput::Stamp(StampOutput {
                    stamp,
                    avoid_overlap: r.avoid_overlap,
                    draw,
                }))
            }
            _ => Err(
                "an output is either \"tileset\" + \"tiles\" (tiles from a tileset) or \
                      \"stamp\" (a named stamp), never a mix"
                    .into(),
            ),
        }
    }
}

impl From<RuleOutput> for RuleOutputRepr {
    fn from(o: RuleOutput) -> Self {
        let mut r = RuleOutputRepr {
            tileset: None,
            tiles: None,
            tile_mode: TileMode::Stack,
            pivot: [0.0, 0.0],
            stamp: None,
            avoid_overlap: false,
            alpha: 1.0,
            offset_px: [0, 0],
            random_offset_px: None,
        };
        let draw = match o {
            RuleOutput::Tiles(t) => {
                r.tileset = Some(t.tileset);
                r.tiles = Some(t.tiles);
                r.tile_mode = t.tile_mode;
                r.pivot = t.pivot;
                r.avoid_overlap = t.avoid_overlap;
                t.draw
            }
            RuleOutput::Stamp(s) => {
                r.stamp = Some(s.stamp);
                r.avoid_overlap = s.avoid_overlap;
                s.draw
            }
        };
        r.alpha = draw.alpha;
        r.offset_px = draw.offset_px;
        r.random_offset_px = draw.random_offset_px;
        r
    }
}

impl JsonSchema for RuleOutput {
    fn schema_name() -> Cow<'static, str> {
        "RuleOutput".into()
    }
    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        variant_schema(
            RuleOutputRepr::json_schema(generator),
            None,
            OUTPUT_VARIANTS,
            "What a rule draws: tileset + tiles, or stamp.",
        )
    }
}

/// How the chosen alternative's tiles are laid out.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TileMode {
    /// Every tile of the alternative is drawn at the matching cell, stacked
    /// in list order (normally an alternative is one tile).
    #[default]
    Stack,
    /// The alternative's tiles keep their atlas layout, scaled by the
    /// tileset's tile size and positioned by `pivot` (LDtk "stamp").
    Stamp,
}

/// One weighted tile alternative: an ordered list of atlas tiles.
///
/// JSON forms:
/// - `[col, row]`: one tile, weight 1;
/// - `[col, row, w, h]`: a rectangle listed row by row, weight 1;
/// - `{ "rect": [col, row, w, h], "weight": n }`: a weighted rectangle
///   (`w = h = 1` for one tile);
/// - `{ "stack": [[col, row], ...], "weight": n }`: any list of tiles in
///   drawing order, including empty, repeated or non-rectangular (LDtk allows
///   all three).
///
/// `weight` defaults to 1. Saving picks the most compact form that fits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TileAlt {
    /// `[column, row]` of each tile, in drawing order.
    pub tiles: Vec<[u32; 2]>,
    pub weight: u32,
}

const ALT_FORMS: &str = "a tile alternative is [col, row], [col, row, w, h], \
     { \"rect\": [col, row, w, h], \"weight\": n } or \
     { \"stack\": [[col, row], ...], \"weight\": n }";

impl TileAlt {
    pub fn single(col: u32, row: u32) -> Self {
        TileAlt {
            tiles: vec![[col, row]],
            weight: 1,
        }
    }

    /// A `w` x `h` rectangle, listed row by row.
    pub fn rect(col: u32, row: u32, w: u32, h: u32) -> Self {
        let tiles = (row..row + h)
            .flat_map(|r| (col..col + w).map(move |c| [c, r]))
            .collect();
        TileAlt { tiles, weight: 1 }
    }

    /// `[left, top, right, bottom]` (inclusive) of the tiles, or None if empty.
    pub fn bounds(&self) -> Option<[u32; 4]> {
        let first = self.tiles.first()?;
        Some(
            self.tiles
                .iter()
                .fold([first[0], first[1], first[0], first[1]], |b, t| {
                    [
                        b[0].min(t[0]),
                        b[1].min(t[1]),
                        b[2].max(t[0]),
                        b[3].max(t[1]),
                    ]
                }),
        )
    }

    /// `[col, row, w, h]` if the tiles are exactly a rectangle listed row by row.
    pub fn as_rect(&self) -> Option<[u32; 4]> {
        let [l, t, r, b] = self.bounds()?;
        let rect = TileAlt::rect(l, t, r - l + 1, b - t + 1);
        (rect.tiles == self.tiles).then_some([l, t, r - l + 1, b - t + 1])
    }

    fn from_json(v: &serde_json::Value) -> Result<Self, String> {
        use serde_json::Value;
        let uint = |v: &Value| v.as_u64().and_then(|n| u32::try_from(n).ok());
        let nums = |v: &Value| -> Option<Vec<u32>> { v.as_array()?.iter().map(uint).collect() };
        let rect = |n: &[u32]| -> Result<Self, String> {
            if n[2] == 0 || n[3] == 0 {
                Err("tile rectangle width and height must be at least 1".into())
            } else {
                Ok(TileAlt::rect(n[0], n[1], n[2], n[3]))
            }
        };
        let bad = || {
            let mut got = v.to_string();
            if got.len() > 60 {
                got.truncate(57);
                got.push_str("...");
            }
            format!("{ALT_FORMS}; got {got}")
        };
        match v {
            Value::Array(_) => match nums(v).as_deref() {
                Some(&[c, r]) => Ok(TileAlt::single(c, r)),
                Some(n @ &[_, _, _, _]) => rect(n),
                _ => Err(bad()),
            },
            Value::Object(m) => {
                if m.keys()
                    .any(|k| !matches!(k.as_str(), "rect" | "stack" | "weight"))
                {
                    return Err(bad());
                }
                let weight = match m.get("weight") {
                    None => 1,
                    Some(w) => uint(w).ok_or_else(bad)?,
                };
                let mut alt = match (m.get("rect"), m.get("stack")) {
                    (Some(r), None) => match nums(r).as_deref() {
                        Some(n @ &[_, _, _, _]) => rect(n)?,
                        _ => return Err(bad()),
                    },
                    (None, Some(s)) => {
                        let tiles: Option<Vec<[u32; 2]>> = s
                            .as_array()
                            .ok_or_else(bad)?
                            .iter()
                            .map(|t| match nums(t).as_deref() {
                                Some(&[c, r]) => Some([c, r]),
                                _ => None,
                            })
                            .collect();
                        TileAlt {
                            tiles: tiles.ok_or_else(bad)?,
                            weight: 1,
                        }
                    }
                    _ => return Err(bad()),
                };
                alt.weight = weight;
                Ok(alt)
            }
            _ => Err(bad()),
        }
    }

    fn to_json(&self) -> serde_json::Value {
        use serde_json::json;
        match (self.as_rect(), self.weight) {
            (Some([c, r, 1, 1]), 1) => json!([c, r]),
            (Some(rect), 1) => json!(rect),
            (Some(rect), w) => json!({ "rect": rect, "weight": w }),
            (None, 1) => json!({ "stack": self.tiles }),
            (None, w) => json!({ "stack": self.tiles, "weight": w }),
        }
    }
}

impl Serialize for TileAlt {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.to_json().serialize(s)
    }
}

impl<'de> Deserialize<'de> for TileAlt {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = serde_json::Value::deserialize(d)?;
        TileAlt::from_json(&v).map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for TileAlt {
    fn schema_name() -> Cow<'static, str> {
        "TileAlt".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        let uint = json!({ "type": "integer", "minimum": 0 });
        let pair = json!({ "type": "array", "items": uint, "minItems": 2, "maxItems": 2 });
        let quad = json!({ "type": "array", "items": uint, "minItems": 4, "maxItems": 4 });
        let weight = json!({ "type": "integer", "minimum": 0, "description": "Default 1." });
        json_schema!({
            "description": "Tile alternative: [col, row], [col, row, w, h], \
                { \"rect\": [col, row, w, h], \"weight\": n } or \
                { \"stack\": [[col, row], ...], \"weight\": n }.",
            "oneOf": [
                pair,
                quad,
                {
                    "type": "object",
                    "properties": { "rect": quad, "weight": weight },
                    "required": ["rect"],
                    "additionalProperties": false
                },
                {
                    "type": "object",
                    "properties": { "stack": { "type": "array", "items": pair }, "weight": weight },
                    "required": ["stack"],
                    "additionalProperties": false
                }
            ]
        })
    }
}

/// Only cells where `(x - x_offset) % x == 0` and `(y - y_offset) % y == 0`,
/// counted from the level origin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Modulo {
    pub x: u32,
    pub y: u32,
    #[serde(default, skip_serializing_if = "is_default")]
    pub x_offset: i32,
    #[serde(default, skip_serializing_if = "is_default")]
    pub y_offset: i32,
}

/// Checkerboard spacing: skip every other cell along an axis, alternating per line.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Checker {
    #[default]
    None,
    Horizontal,
    Vertical,
}

/// Perlin noise gate: the rule only fires where noise is positive.
/// Kept when inactive so an LDtk import is lossless.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Perlin {
    pub active: bool,
    pub seed: i64,
    pub scale: f64,
    pub octaves: u32,
}

/// Pattern cells outside the level read as empty (`"."`) or as a value slug.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum OutOfBounds {
    Empty,
    Value(Slug),
}

impl TryFrom<String> for OutOfBounds {
    type Error = String;
    fn try_from(s: String) -> Result<Self, String> {
        if s == "." {
            return Ok(OutOfBounds::Empty);
        }
        Slug::new(s)
            .map(OutOfBounds::Value)
            .map_err(|e| format!("out_of_bounds must be \".\" or a value slug: {e}"))
    }
}

impl From<OutOfBounds> for String {
    fn from(o: OutOfBounds) -> String {
        match o {
            OutOfBounds::Empty => ".".into(),
            OutOfBounds::Value(s) => s.into(),
        }
    }
}

impl JsonSchema for OutOfBounds {
    fn schema_name() -> Cow<'static, str> {
        "OutOfBounds".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "pattern": "^(\\.|[a-z][a-z0-9_]*)$",
            "description": "\".\" for empty, or an IntGrid value slug."
        })
    }
}

/// Random pixel offset ranges, inclusive.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RandomOffset {
    /// `[min, max]`. Default `[0, 0]`.
    #[serde(default, skip_serializing_if = "is_default")]
    pub x: [i32; 2],
    #[serde(default, skip_serializing_if = "is_default")]
    pub y: [i32; 2],
}

/// A named rectangle of tiles that rule outputs can place.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Stamp {
    /// Referenced by `out.stamp`.
    pub slug: Slug,
    pub tileset: Slug,
    /// `[col, row, w, h]` in tiles.
    pub rect: [u32; 4],
    /// Which point of the stamp's pixel bounding box sits on the same point
    /// of the matching cell, 0 to 1. Default `[0, 0]`.
    #[serde(default, skip_serializing_if = "is_default")]
    pub pivot: [f64; 2],
    /// After each placement of this stamp, mark its footprint occupied in
    /// the layer's occupancy grid, so `avoid_overlap` outputs skip it.
    #[serde(default, skip_serializing_if = "is_default")]
    pub occupies: bool,
}

/// Shared pattern pieces and templates: `rules/macros.json` (optional).
/// Rules refer to them by name: `match.rows` / `require.rows` to
/// `patterns`, `match.legend` / `require.legend` to `legends`, and
/// `{ "use": ... }` entries to `templates` (or a built-in, see
/// [`Macros::builtin_templates`]).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Macros {
    /// Named pattern rows.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub patterns: BTreeMap<Slug, Vec<String>>,
    /// Named legends.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub legends: BTreeMap<Slug, BTreeMap<char, CellMatch>>,
    /// Named templates: parameterized lists of rules that a group uses with
    /// `{ "use": "<name>", ... }`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub templates: BTreeMap<Slug, Template>,
}

/// The built-in template sources, by name (the read-only `std` namespace).
/// Frozen: a changed layout ships under a new name.
const BUILTIN_TEMPLATES: &[(&str, &str)] =
    &[("blob47", include_str!("../../templates/blob47.json"))];

impl Macros {
    /// The built-in templates (`blob47`), which every project can `use` and
    /// no project template may shadow.
    pub fn builtin_templates() -> &'static BTreeMap<Slug, Template> {
        static BUILTIN: std::sync::OnceLock<BTreeMap<Slug, Template>> = std::sync::OnceLock::new();
        BUILTIN.get_or_init(|| {
            BUILTIN_TEMPLATES
                .iter()
                .map(|(name, text)| {
                    let t: Template = serde_json::from_str(text)
                        .unwrap_or_else(|e| panic!("built-in template {name}: {e}"));
                    (Slug::new(*name).expect("built-in name is a slug"), t)
                })
                .collect()
        })
    }

    /// Whether `name` is a built-in template.
    pub fn is_builtin(name: &str) -> bool {
        Self::builtin_templates().contains_key(name)
    }

    /// The template `use: name` means: a built-in first (they can't be
    /// shadowed), else one of `macros`.
    pub fn template<'a>(macros: Option<&'a Macros>, name: &str) -> Option<&'a Template> {
        Self::builtin_templates()
            .get(name)
            .or_else(|| macros?.templates.get(name))
    }
}

/// A template in `rules/macros.json`: rules whose tiles are relative to a
/// reference's `origin` and whose tileset comes from the reference.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Template {
    /// Display name, for the editor's template library.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// What it draws and how its tiles are laid out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    /// Default legend for every rule (an object, or a name in `legends`);
    /// a rule's own `match.legend` entries win.
    #[serde(default, skip_serializing_if = "Legend::is_empty")]
    pub legend: Legend,
    /// The rules, in evaluation order. Slugs are unique within the template.
    pub rules: Vec<TemplateRule>,
}

/// A rule inside a template: a pattern [`Rule`] that draws, whose
/// `out.tileset` may be left out (it comes from the reference). Tile
/// coordinates of a rule without its own `out.tileset` are relative to the
/// reference's `origin`; a rule that names its own tileset is *fixed*:
/// absolute coordinates, its own tileset. Region rules and `write` outputs
/// can't be in a template (a load error).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TemplateRule {
    /// Unique within the template. The expanded rule is `<reference slug>__<slug>`.
    pub slug: Slug,
    /// Default true.
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub active: bool,
    #[serde(rename = "match")]
    pub pattern: Pattern,
    /// Patterns that must also hold, anchored on the same cell (one, or a
    /// list).
    #[serde(default, skip_serializing_if = "Vec::is_empty", with = "require_list")]
    #[schemars(with = "RequireRepr")]
    pub require: Vec<Pattern>,
    /// Extra mirrored or rotated variants of the pattern to try.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub symmetry: Vec<Symmetry>,
    pub out: TemplateOutput,
    /// Probability 0 to 1 that a matching cell is drawn. Default 1.
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub chance: f64,
    /// After this rule draws a cell, later rules skip that cell.
    #[serde(rename = "break", default, skip_serializing_if = "is_default")]
    pub break_on_match: bool,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Modulo")]
    pub modulo: Option<Modulo>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub checker: Checker,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Perlin")]
    pub perlin: Option<Perlin>,
    /// What pattern cells outside the level read as.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "OutOfBounds")]
    pub out_of_bounds: Option<OutOfBounds>,
    /// Extra integer mixed into this rule's random choices. Default 0.
    #[serde(default, skip_serializing_if = "is_default")]
    pub salt: i64,
    // Keys of a region rule or a writing rule: present, they are a load
    // error with a message that says why (not serde's "unknown field").
    #[serde(default, skip_serializing, deserialize_with = "refused")]
    #[schemars(skip)]
    region: (),
    #[serde(default, skip_serializing, deserialize_with = "refused")]
    #[schemars(skip)]
    place: (),
    #[serde(default, skip_serializing, deserialize_with = "refused")]
    #[schemars(skip)]
    write: (),
}

/// Always an error: a template rule can't be a region rule or write.
fn refused<'de, D: serde::Deserializer<'de>>(_: D) -> Result<(), D::Error> {
    Err(serde::de::Error::custom(
        "templates can't hold region rules (\"region\", \"place\") or rules that write a \
         derived grid (\"write\"); put that rule in the layer's own rules file",
    ))
}

impl TemplateRule {
    /// Whether the rule names its own tileset (absolute tiles, no origin).
    pub fn is_fixed(&self) -> bool {
        matches!(&self.out, TemplateOutput::Tiles(t) if t.tileset.is_some())
    }

    /// Whether the rule draws tiles from the reference's tileset.
    pub fn needs_tileset(&self) -> bool {
        matches!(&self.out, TemplateOutput::Tiles(t) if t.tileset.is_none())
    }

    /// `match`, then every `require` pattern.
    pub fn patterns(&self) -> impl Iterator<Item = &Pattern> {
        std::iter::once(&self.pattern).chain(self.require.iter())
    }

    /// `match`, then every `require` pattern, mutably.
    pub fn patterns_mut(&mut self) -> impl Iterator<Item = &mut Pattern> {
        std::iter::once(&mut self.pattern).chain(self.require.iter_mut())
    }
}

impl TryFrom<Rule> for TemplateRule {
    type Error = String;
    /// A pattern rule that draws, as a fixed template rule (its tileset
    /// stays its own). Region rules and writing rules can't be in a template.
    fn try_from(r: Rule) -> Result<Self, String> {
        let (Select::Match(m), Output::Draw(out)) = (r.select, r.output) else {
            return Err(format!(
                "rule {}: templates can't hold region rules or rules that write a derived grid",
                r.slug
            ));
        };
        Ok(TemplateRule {
            slug: r.slug,
            active: r.active,
            pattern: m.pattern,
            require: m.require,
            symmetry: m.symmetry,
            out: match out {
                RuleOutput::Tiles(t) => TemplateOutput::Tiles(TemplateTilesOutput {
                    tileset: Some(t.tileset),
                    tiles: t.tiles,
                    tile_mode: t.tile_mode,
                    pivot: t.pivot,
                    avoid_overlap: t.avoid_overlap,
                    draw: t.draw,
                }),
                RuleOutput::Stamp(s) => TemplateOutput::Stamp(s),
            },
            chance: m.chance,
            break_on_match: r.break_on_match,
            modulo: m.modulo,
            checker: m.checker,
            perlin: m.perlin,
            out_of_bounds: m.out_of_bounds,
            salt: r.salt,
            region: (),
            place: (),
            write: (),
        })
    }
}

/// A template rule's output: like [`RuleOutput`], but `tileset` may be left
/// out of a tiles output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "RuleOutputRepr", into = "RuleOutputRepr")]
pub enum TemplateOutput {
    Tiles(TemplateTilesOutput),
    Stamp(StampOutput),
}

/// [`TilesOutput`] with an optional tileset.
#[derive(Debug, Clone, PartialEq)]
pub struct TemplateTilesOutput {
    /// `None`: the reference's tileset, tiles relative to its origin.
    pub tileset: Option<Slug>,
    pub tiles: Vec<TileAlt>,
    pub tile_mode: TileMode,
    pub pivot: [f64; 2],
    pub avoid_overlap: bool,
    pub draw: Draw,
}

impl TryFrom<RuleOutputRepr> for TemplateOutput {
    type Error = String;
    fn try_from(mut r: RuleOutputRepr) -> Result<Self, String> {
        if r.tileset.is_none() && r.tiles.is_some() && r.stamp.is_none() {
            // Borrow the plain output's checks with a placeholder tileset.
            r.tileset = Some(crate::id::slug("x"));
            return match RuleOutput::try_from(r)? {
                RuleOutput::Tiles(t) => Ok(TemplateOutput::Tiles(TemplateTilesOutput {
                    tileset: None,
                    tiles: t.tiles,
                    tile_mode: t.tile_mode,
                    pivot: t.pivot,
                    avoid_overlap: t.avoid_overlap,
                    draw: t.draw,
                })),
                RuleOutput::Stamp(_) => unreachable!("tiles output"),
            };
        }
        Ok(match RuleOutput::try_from(r)? {
            RuleOutput::Tiles(t) => TemplateOutput::Tiles(TemplateTilesOutput {
                tileset: Some(t.tileset),
                tiles: t.tiles,
                tile_mode: t.tile_mode,
                pivot: t.pivot,
                avoid_overlap: t.avoid_overlap,
                draw: t.draw,
            }),
            RuleOutput::Stamp(s) => TemplateOutput::Stamp(s),
        })
    }
}

impl From<TemplateOutput> for RuleOutputRepr {
    fn from(o: TemplateOutput) -> Self {
        match o {
            TemplateOutput::Tiles(t) => {
                let tileset = t.tileset.clone();
                let mut r = RuleOutputRepr::from(RuleOutput::Tiles(TilesOutput {
                    tileset: crate::id::slug("x"),
                    tiles: t.tiles,
                    tile_mode: t.tile_mode,
                    pivot: t.pivot,
                    avoid_overlap: t.avoid_overlap,
                    draw: t.draw,
                }));
                r.tileset = tileset;
                r
            }
            TemplateOutput::Stamp(s) => RuleOutputRepr::from(RuleOutput::Stamp(s)),
        }
    }
}

impl TemplateOutput {
    pub fn draw(&self) -> &Draw {
        match self {
            TemplateOutput::Tiles(t) => &t.draw,
            TemplateOutput::Stamp(s) => &s.draw,
        }
    }
}

const TEMPLATE_OUTPUT_VARIANTS: &[VariantKeys] = &[
    VariantKeys {
        name: "tiles",
        keys: &["tileset", "tiles", "tile_mode", "pivot"],
        required: &["tiles"],
    },
    VariantKeys {
        name: "stamp",
        keys: &["stamp"],
        required: &["stamp"],
    },
];

impl JsonSchema for TemplateOutput {
    fn schema_name() -> Cow<'static, str> {
        "TemplateOutput".into()
    }
    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        variant_schema(
            RuleOutputRepr::json_schema(generator),
            None,
            TEMPLATE_OUTPUT_VARIANTS,
            "What a template rule draws: tiles (tileset optional: the reference's), or stamp.",
        )
    }
}

/// A `{ "use": ... }` entry in a group: the rules of a template, expanded
/// in place (see [`crate::Project::expanded_rules`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TemplateRef {
    /// Template name: one of `rules/macros.json` `templates`, or a built-in.
    #[serde(rename = "use")]
    pub template: Slug,
    /// Names the reference; default the template name. Expanded rules are
    /// `<slug>__<template rule slug>`.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Slug")]
    pub slug: Option<Slug>,
    /// Tileset for the template's rules that don't name their own.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Slug")]
    pub tileset: Option<Slug>,
    /// `[col, row]` added to every relative tile coordinate. Default `[0, 0]`.
    #[serde(default, skip_serializing_if = "is_default")]
    pub origin: [u32; 2],
    /// Per template rule slug: replacement alternatives (relative to
    /// `origin`, absolute for a rule with its own tileset).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tiles: BTreeMap<Slug, Vec<TileAlt>>,
    /// Template rule slugs to leave out.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skip: Vec<Slug>,
    /// Merged over every expanded rule's legend (wins over the template's
    /// and the rule's).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub legend: BTreeMap<char, CellMatch>,
    /// Default true. Off: every expanded rule is inactive.
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub active: bool,
}

impl TemplateRef {
    /// A reference to `template` with every option at its default.
    pub fn new(template: Slug) -> Self {
        TemplateRef {
            template,
            slug: None,
            tileset: None,
            origin: [0, 0],
            tiles: BTreeMap::new(),
            skip: Vec::new(),
            legend: BTreeMap::new(),
            active: true,
        }
    }

    /// The reference's slug: `slug`, else the template name.
    pub fn slug(&self) -> &Slug {
        self.slug.as_ref().unwrap_or(&self.template)
    }
}

/// One entry of a group's `rules`: an inline rule, or a template reference.
/// On disk both are plain objects; the `use` key makes a reference.
// Most entries are rules; boxing them would cost every caller a deref.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum RuleEntry {
    Rule(Rule),
    Use(TemplateRef),
}

impl RuleEntry {
    /// The rule's or the reference's slug.
    pub fn slug(&self) -> &Slug {
        match self {
            RuleEntry::Rule(r) => &r.slug,
            RuleEntry::Use(u) => u.slug(),
        }
    }

    pub fn active(&self) -> bool {
        match self {
            RuleEntry::Rule(r) => r.active,
            RuleEntry::Use(u) => u.active,
        }
    }

    pub fn as_rule(&self) -> Option<&Rule> {
        match self {
            RuleEntry::Rule(r) => Some(r),
            RuleEntry::Use(_) => None,
        }
    }

    pub fn as_rule_mut(&mut self) -> Option<&mut Rule> {
        match self {
            RuleEntry::Rule(r) => Some(r),
            RuleEntry::Use(_) => None,
        }
    }

    pub fn as_use(&self) -> Option<&TemplateRef> {
        match self {
            RuleEntry::Use(u) => Some(u),
            RuleEntry::Rule(_) => None,
        }
    }

    pub fn as_use_mut(&mut self) -> Option<&mut TemplateRef> {
        match self {
            RuleEntry::Use(u) => Some(u),
            RuleEntry::Rule(_) => None,
        }
    }
}

impl From<Rule> for RuleEntry {
    fn from(r: Rule) -> Self {
        RuleEntry::Rule(r)
    }
}

impl From<TemplateRef> for RuleEntry {
    fn from(u: TemplateRef) -> Self {
        RuleEntry::Use(u)
    }
}

impl Serialize for RuleEntry {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            RuleEntry::Rule(r) => r.serialize(s),
            RuleEntry::Use(u) => u.serialize(s),
        }
    }
}

/// Every key either kind of entry may have. Deserializing through this
/// keeps serde's exact unknown-field errors (with their JSON path) for a
/// typo in either kind; [`RuleEntry`] then checks which kind it is by the
/// `use` key and rejects keys of the other kind.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuleEntryRepr {
    #[serde(default, deserialize_with = "present")]
    slug: Option<Slug>,
    #[serde(default, deserialize_with = "present")]
    active: Option<bool>,
    #[serde(rename = "match", default, deserialize_with = "present")]
    pattern: Option<Pattern>,
    #[serde(default, deserialize_with = "present")]
    region: Option<Region>,
    #[serde(default, deserialize_with = "present")]
    place: Option<Place>,
    #[serde(default, deserialize_with = "present")]
    require: Option<RequireRepr>,
    #[serde(default, deserialize_with = "present")]
    symmetry: Option<Vec<Symmetry>>,
    #[serde(default, deserialize_with = "present")]
    out: Option<RuleOutput>,
    #[serde(default, deserialize_with = "present")]
    write: Option<Write>,
    #[serde(default, deserialize_with = "present")]
    chance: Option<f64>,
    #[serde(rename = "break", default, deserialize_with = "present")]
    break_on_match: Option<bool>,
    #[serde(default, deserialize_with = "present")]
    modulo: Option<Modulo>,
    #[serde(default, deserialize_with = "present")]
    checker: Option<Checker>,
    #[serde(default, deserialize_with = "present")]
    perlin: Option<Perlin>,
    #[serde(default, deserialize_with = "present")]
    out_of_bounds: Option<OutOfBounds>,
    #[serde(default, deserialize_with = "present")]
    salt: Option<i64>,
    #[serde(rename = "use", default, deserialize_with = "present")]
    template: Option<Slug>,
    #[serde(default, deserialize_with = "present")]
    tileset: Option<Slug>,
    #[serde(default, deserialize_with = "present")]
    origin: Option<[u32; 2]>,
    #[serde(default, deserialize_with = "present")]
    tiles: Option<BTreeMap<Slug, Vec<TileAlt>>>,
    #[serde(default, deserialize_with = "present")]
    skip: Option<Vec<Slug>>,
    #[serde(default, deserialize_with = "present")]
    legend: Option<BTreeMap<char, CellMatch>>,
}

impl TryFrom<RuleEntryRepr> for RuleEntry {
    type Error = String;
    fn try_from(r: RuleEntryRepr) -> Result<Self, String> {
        let rule_keys = [
            ("match", r.pattern.is_some()),
            ("region", r.region.is_some()),
            ("place", r.place.is_some()),
            ("require", r.require.is_some()),
            ("symmetry", r.symmetry.is_some()),
            ("out", r.out.is_some()),
            ("write", r.write.is_some()),
            ("chance", r.chance.is_some()),
            ("break", r.break_on_match.is_some()),
            ("modulo", r.modulo.is_some()),
            ("checker", r.checker.is_some()),
            ("perlin", r.perlin.is_some()),
            ("out_of_bounds", r.out_of_bounds.is_some()),
            ("salt", r.salt.is_some()),
        ];
        let use_keys = [
            ("tileset", r.tileset.is_some()),
            ("origin", r.origin.is_some()),
            ("tiles", r.tiles.is_some()),
            ("skip", r.skip.is_some()),
            ("legend", r.legend.is_some()),
        ];
        let first =
            |keys: &[(&'static str, bool)]| keys.iter().find(|(_, on)| *on).map(|(k, _)| *k);
        match r.template {
            Some(template) => {
                if let Some(k) = first(&rule_keys) {
                    return Err(format!(
                        "{k:?} is not allowed on a template reference (an entry with \"use\"); \
                         it belongs to a rule"
                    ));
                }
                Ok(RuleEntry::Use(TemplateRef {
                    template,
                    slug: r.slug,
                    tileset: r.tileset,
                    origin: r.origin.unwrap_or_default(),
                    tiles: r.tiles.unwrap_or_default(),
                    skip: r.skip.unwrap_or_default(),
                    legend: r.legend.unwrap_or_default(),
                    active: r.active.unwrap_or(true),
                }))
            }
            None => {
                if let Some(k) = first(&use_keys) {
                    return Err(format!(
                        "{k:?} is not allowed on a rule; it belongs to a template reference \
                         (an entry with \"use\")"
                    ));
                }
                let rule = RuleRepr {
                    slug: r.slug.ok_or("missing field `slug`")?,
                    active: r.active.unwrap_or(true),
                    pattern: r.pattern,
                    region: r.region,
                    place: r.place,
                    require: r.require,
                    symmetry: r.symmetry,
                    out: r.out,
                    write: r.write,
                    chance: r.chance,
                    break_on_match: r.break_on_match.unwrap_or(false),
                    modulo: r.modulo,
                    checker: r.checker,
                    perlin: r.perlin,
                    out_of_bounds: r.out_of_bounds,
                    salt: r.salt.unwrap_or(0),
                };
                Rule::try_from(rule).map(RuleEntry::Rule)
            }
        }
    }
}

impl<'de> Deserialize<'de> for RuleEntry {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        RuleEntry::try_from(RuleEntryRepr::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for RuleEntry {
    fn schema_name() -> Cow<'static, str> {
        "RuleEntry".into()
    }
    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        let rule = generator.subschema_for::<Rule>();
        let reference = generator.subschema_for::<TemplateRef>();
        json_schema!({
            "description": "A rule, or a template reference (an object with \"use\").",
            "oneOf": [rule, reference]
        })
    }
}

#[cfg(test)]
mod tests {
    use super::TileAlt;

    fn round_trip(json: &str) -> TileAlt {
        let t: TileAlt = serde_json::from_str(json).unwrap();
        assert_eq!(serde_json::to_string(&t).unwrap(), json);
        t
    }

    #[test]
    fn tile_alt_forms() {
        assert_eq!(round_trip("[3,4]"), TileAlt::single(3, 4));
        let r = round_trip("[1,2,2,2]");
        assert_eq!(r.tiles, vec![[1, 2], [2, 2], [1, 3], [2, 3]]);
        assert_eq!(round_trip(r#"{"rect":[1,2,1,1],"weight":3}"#).weight, 3);
        let odd = round_trip(r#"{"stack":[[5,0],[4,0]]}"#);
        assert_eq!(odd.bounds(), Some([4, 0, 5, 0]));
        assert_eq!(odd.as_rect(), None);
        assert!(round_trip(r#"{"stack":[]}"#).tiles.is_empty());
        assert_eq!(
            round_trip(r#"{"stack":[[5,0],[4,0]],"weight":2}"#).weight,
            2
        );
        // A rectangle written as a stack saves as the plain form.
        let t: TileAlt = serde_json::from_str(r#"{"stack":[[1,1]]}"#).unwrap();
        assert_eq!(serde_json::to_string(&t).unwrap(), "[1,1]");
        let t: TileAlt = serde_json::from_str(r#"{"rect":[1,1,2,1]}"#).unwrap();
        assert_eq!(serde_json::to_string(&t).unwrap(), "[1,1,2,1]");
    }

    #[test]
    fn tile_alt_errors_say_what_is_accepted() {
        for bad in [
            r#"{"rect":[1,1,1,1],"stack":[]}"#,
            r#"{"tiles":[[1,1]]}"#,
            r#"{"tile":[1,1]}"#,
            "[1,1,1]",
            "[-1,1]",
            "\"a\"",
        ] {
            let e = serde_json::from_str::<TileAlt>(bad)
                .unwrap_err()
                .to_string();
            assert!(e.contains("a tile alternative is [col, row]"), "{bad}: {e}");
        }
        let e = serde_json::from_str::<TileAlt>("[1,1,0,2]")
            .unwrap_err()
            .to_string();
        assert!(e.contains("at least 1"), "{e}");
    }
}
