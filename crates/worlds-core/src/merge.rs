//! Import from another Worlds project: definitions, worlds and levels of
//! one project brought into another as one batch of existing commands.
//!
//! [`plan`] works out everything first and changes nothing: every item to
//! add, every collision on a slug and what the [`Policy`] does with it,
//! every dependency a selected item pulls in, every instance id re-rolled,
//! every reference dropped and every file to copy. [`Plan::command`] is the
//! import as one `batch` labelled `Import from <name>` (`set_tileset_def`,
//! `set_enum_def`, `set_macro`, `set_layer_def`, `set_rule_group`,
//! `set_stamp`, `set_level_field_def`, `set_entity_def`,
//! `set_project_setting`, `create_world`, `set_world_layout`,
//! `set_guide_group`, `create_level`, `replace_level`, `reorder_levels`,
//! `add_note`), so it is one undo entry
//! and the editor, the API and the MCP server apply it like any other
//! command. The files it copies ([`Plan::assets`], [`copy_assets`]) are
//! the one thing outside the batch: copy them first, then apply.
//!
//! How it works: the source is cloned, and a definition renamed to dodge a
//! collision is renamed *in the clone* with the source's own `rename_*`
//! command, which rewrites every reference to it there. What is imported
//! is then read out of the clone, so references inside the imported set
//! follow the rename. Each item's commands are tried on a copy of the
//! destination as they are made; an item whose commands are refused (an
//! `overwrite` that would orphan stored content, a level of another grid)
//! is left out and reported, so the batch always applies to the project
//! it was planned against. See `docs/importing_worlds.md`.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt;
use std::path::Path;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::command::{
    Command, CommandError, DefKind, LevelContent, MacroDef, MacroKind, NoteScope, ProjectSetting,
};
use crate::fragment::derived_free;
use crate::id::{Id, Slug};
use crate::model::{
    Annotation, Endpoint, FieldDef, FieldType, Level, Link, Project, Shape, parse_link,
};

/// What holds a reference ([`Project::tileset_refs`],
/// [`Project::macro_refs`]): the dependency graph's edges come from these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Owner {
    Def(DefKind, Slug),
    /// An auto layer's rule set (or the layer itself).
    Layer(Slug),
    Template(Slug),
    Level(Slug),
}

/// A kind of importable item. The order is the order the batch applies
/// them in.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Tileset,
    Enum,
    Pattern,
    Legend,
    Template,
    Layer,
    LevelField,
    Entity,
    /// A project setting (`grid_size`, `bg_color`, `collision_layer`,
    /// `blocking_values`, `default_level_size`).
    Setting,
    /// A world of the destination that imported levels go into.
    World,
    GuideGroup,
    Level,
}

impl Kind {
    pub const ALL: [Kind; 12] = [
        Kind::Tileset,
        Kind::Enum,
        Kind::Pattern,
        Kind::Legend,
        Kind::Template,
        Kind::Layer,
        Kind::LevelField,
        Kind::Entity,
        Kind::Setting,
        Kind::World,
        Kind::GuideGroup,
        Kind::Level,
    ];

    /// The snake_case name (`level_field`).
    pub fn name(self) -> &'static str {
        match self {
            Kind::Tileset => "tileset",
            Kind::Enum => "enum",
            Kind::Pattern => "pattern",
            Kind::Legend => "legend",
            Kind::Template => "template",
            Kind::Layer => "layer",
            Kind::LevelField => "level_field",
            Kind::Entity => "entity",
            Kind::Setting => "setting",
            Kind::World => "world",
            Kind::GuideGroup => "guide_group",
            Kind::Level => "level",
        }
    }

    /// The heading the editor and `--dry-run` group items under.
    pub fn plural(self) -> &'static str {
        match self {
            Kind::Tileset => "Tilesets",
            Kind::Enum => "Enums",
            Kind::Pattern => "Patterns",
            Kind::Legend => "Legends",
            Kind::Template => "Templates",
            Kind::Layer => "Layers",
            Kind::LevelField => "Level fields",
            Kind::Entity => "Entity types",
            Kind::Setting => "Project settings",
            Kind::World => "Worlds",
            Kind::GuideGroup => "Guide groups",
            Kind::Level => "Levels",
        }
    }

    /// A definition (counted as one in the summary).
    pub fn is_definition(self) -> bool {
        self <= Kind::Setting
    }

    fn macro_kind(self) -> Option<MacroKind> {
        match self {
            Kind::Pattern => Some(MacroKind::Pattern),
            Kind::Legend => Some(MacroKind::Legend),
            Kind::Template => Some(MacroKind::Template),
            _ => None,
        }
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The project settings an import can carry, in the order they apply.
pub const SETTINGS: [&str; 5] = [
    "grid_size",
    "bg_color",
    "collision_layer",
    "blocking_values",
    "default_level_size",
];

/// A shortcut for a whole selection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Preset {
    /// Only what the lists name (and what that needs).
    #[default]
    Pick,
    /// Every definition and project setting, no levels.
    DefsOnly,
    /// Every definition, setting and world with all its levels.
    Everything,
}

/// What to import from the source, by kind and slug. A level brings its
/// dependencies along (its layers, their tilesets, the entity types it
/// places, their enums, the rules of its auto layers and the macros they
/// use); so does every definition. A world brings its levels, guide groups
/// and world notes.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    #[serde(default)]
    pub preset: Preset,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tilesets: Vec<Slug>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub enums: Vec<Slug>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub patterns: Vec<Slug>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub legends: Vec<Slug>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub templates: Vec<Slug>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub layers: Vec<Slug>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub level_fields: Vec<Slug>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entities: Vec<Slug>,
    /// Keys of [`SETTINGS`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub settings: Vec<Slug>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub worlds: Vec<Slug>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub levels: Vec<Slug>,
}

impl Selection {
    /// Just a preset.
    pub fn preset(preset: Preset) -> Self {
        Selection {
            preset,
            ..Selection::default()
        }
    }

    /// The list for `kind` (none for [`Kind::World`] targets and guide
    /// groups, which come with their world).
    pub fn list_mut(&mut self, kind: Kind) -> Option<&mut Vec<Slug>> {
        Some(match kind {
            Kind::Tileset => &mut self.tilesets,
            Kind::Enum => &mut self.enums,
            Kind::Pattern => &mut self.patterns,
            Kind::Legend => &mut self.legends,
            Kind::Template => &mut self.templates,
            Kind::Layer => &mut self.layers,
            Kind::LevelField => &mut self.level_fields,
            Kind::Entity => &mut self.entities,
            Kind::Setting => &mut self.settings,
            Kind::Level => &mut self.levels,
            Kind::World | Kind::GuideGroup => return None,
        })
    }

    fn lists(&self) -> [(Kind, &Vec<Slug>); 10] {
        [
            (Kind::Tileset, &self.tilesets),
            (Kind::Enum, &self.enums),
            (Kind::Pattern, &self.patterns),
            (Kind::Legend, &self.legends),
            (Kind::Template, &self.templates),
            (Kind::Layer, &self.layers),
            (Kind::LevelField, &self.level_fields),
            (Kind::Entity, &self.entities),
            (Kind::Setting, &self.settings),
            (Kind::Level, &self.levels),
        ]
    }
}

/// What to do with an item whose slug the destination already has (with
/// a different definition; identical ones are "same" and skipped).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Conflict {
    /// Keep ours.
    #[default]
    Skip,
    /// Take theirs. Refused (and reported) where the command refuses it,
    /// as `set_layer_def` does a change that would orphan stored content.
    Overwrite,
    /// Take theirs as `<slug>_2` (or `_3`, ...), every reference inside the
    /// imported set rewritten.
    Rename,
}

/// Where imported levels go.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlacementMode {
    /// A new world (`world`, default the source project's name as a slug).
    #[default]
    NewWorld,
    /// The existing world `world`.
    World,
    /// The destination world with the slug of the level's own source world
    /// (made if missing), at the level's own coordinates.
    Keep,
}

/// Where imported levels, guide groups and world notes go.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Placement {
    #[serde(default)]
    pub mode: PlacementMode,
    /// The world for `new_world` (optional) and `world` (required).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub world: Option<Slug>,
    /// World pixels added to every imported level's position and every
    /// imported world note's geometry.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub offset: [i64; 2],
}

fn is_zero(v: &[i64; 2]) -> bool {
    *v == [0, 0]
}

/// How collisions are handled and where things go.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    /// For every collision not named in `conflicts`.
    #[serde(default)]
    pub on_conflict: Conflict,
    /// Per item, by `"<kind>:<slug>"` (`"layer:collision"`,
    /// `"level:crevasse"`, `"asset:castle.png"` for a file under `art/`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub conflicts: BTreeMap<String, Conflict>,
    #[serde(default)]
    pub placement: Placement,
}

impl Policy {
    fn conflict(&self, key: &str) -> Conflict {
        self.conflicts.get(key).copied().unwrap_or(self.on_conflict)
    }
}

/// What the import does with one item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// New here.
    Add,
    /// Already here, identical: nothing to do.
    Same,
    /// A collision kept as ours.
    Skip,
    /// A collision replaced by theirs.
    Overwrite,
    /// A collision taken under a new slug (`to`).
    Rename,
    /// Its commands were refused (`reason`); nothing of it is imported.
    Refused,
}

impl Action {
    /// Something of it goes in.
    pub fn imports(self) -> bool {
        matches!(self, Action::Add | Action::Overwrite | Action::Rename)
    }

    pub fn name(self) -> &'static str {
        match self {
            Action::Add => "add",
            Action::Same => "same",
            Action::Skip => "skip",
            Action::Overwrite => "overwrite",
            Action::Rename => "rename",
            Action::Refused => "refused",
        }
    }
}

/// One item of a plan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PlanItem {
    pub kind: Kind,
    /// Its slug in the source (for a world: in the destination).
    pub slug: Slug,
    pub action: Action,
    /// The new slug of a renamed item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<Slug>,
    /// Levels and guide groups: the destination world they go into.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub world: Option<Slug>,
    /// A dependency nobody selected: the selected items that need it
    /// (`"level:crevasse"`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_by: Vec<String>,
    /// Why it was skipped or refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl PlanItem {
    /// `"<kind>:<slug>"`, the key of [`Policy::conflicts`].
    pub fn key(&self) -> String {
        format!("{}:{}", self.kind, self.slug)
    }

    /// Whether it collided with something of the destination.
    pub fn collided(&self) -> bool {
        matches!(
            self.action,
            Action::Skip | Action::Overwrite | Action::Rename
        ) || (self.action == Action::Refused && self.reason_is_collision())
    }

    fn reason_is_collision(&self) -> bool {
        self.reason
            .as_deref()
            .is_some_and(|r| r.starts_with("overwrite refused"))
    }
}

/// An instance id the import changed because the destination has it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct IdChange {
    /// `level`, `entity` or `note`.
    pub kind: String,
    pub from: Id,
    pub to: Id,
}

/// A file the import copies into the destination.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AssetCopy {
    /// As the source names it (relative to the source project).
    pub from: String,
    /// Where it goes, relative to the destination (`art/<file>`).
    pub to: String,
    /// `add`, `same` (that file is already there), `skip` (ours is kept and
    /// used), `overwrite`, `rename` or `refused` (missing in the source).
    pub action: Action,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// An import, worked out ([`plan`]). Nothing has changed yet.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct Plan {
    /// The source project's name.
    pub source: String,
    /// `Import from <name>`: the batch's label and undo entry.
    pub label: String,
    /// `12 definitions, 3 levels, 2 renamed, 1 skipped`.
    pub summary: String,
    /// Every item, in the order the batch applies them.
    pub items: Vec<PlanItem>,
    /// Instance ids re-rolled because the destination has them.
    pub ids: Vec<IdChange>,
    /// References dropped because nothing here resolves them.
    pub dropped: Vec<String>,
    /// Anything else worth knowing (a note left out, a missing file).
    pub problems: Vec<String>,
    /// Files to copy before applying ([`copy_assets`]).
    pub assets: Vec<AssetCopy>,
    #[serde(skip)]
    commands: Vec<Command>,
}

impl Plan {
    /// The steps of the batch, in order.
    pub fn commands(&self) -> Vec<Command> {
        self.commands.clone()
    }

    /// The import as one command: a batch labelled [`Plan::label`].
    pub fn command(&self) -> Command {
        Command::Batch {
            label: self.label.clone(),
            commands: self.commands.clone(),
        }
    }

    /// Nothing to apply and nothing to copy.
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty() && !self.assets.iter().any(|a| a.action.imports())
    }

    /// The item of `kind` with source slug `slug`.
    pub fn item(&self, kind: Kind, slug: &str) -> Option<&PlanItem> {
        self.items.iter().find(|i| i.kind == kind && i.slug == slug)
    }

    /// The plan as the dialog and `--dry-run` show it: the summary, then
    /// the items by kind, then ids, dropped references, files and problems.
    pub fn text(&self) -> String {
        let mut out = format!("{} ({})\n", self.label, self.summary);
        for kind in Kind::ALL {
            let items: Vec<&PlanItem> = self.items.iter().filter(|i| i.kind == kind).collect();
            if items.is_empty() {
                continue;
            }
            out.push_str(&format!("{}:\n", kind.plural()));
            for i in items {
                let mut line = format!("  {:<9} {}", i.action.name(), i.slug);
                if let Some(to) = &i.to {
                    line.push_str(&format!(" -> {to}"));
                }
                if let Some(w) = &i.world
                    && i.kind != Kind::World
                {
                    line.push_str(&format!(" (into world {w})"));
                }
                if !i.required_by.is_empty() {
                    line.push_str(&format!(" [needed by {}]", i.required_by.join(", ")));
                }
                if let Some(r) = &i.reason {
                    line.push_str(&format!(": {r}"));
                }
                out.push_str(&line);
                out.push('\n');
            }
        }
        if !self.ids.is_empty() {
            out.push_str("Ids re-rolled:\n");
            for c in &self.ids {
                out.push_str(&format!("  {} {} -> {}\n", c.kind, c.from, c.to));
            }
        }
        if !self.assets.is_empty() {
            out.push_str("Files:\n");
            for a in &self.assets {
                let mut line = if a.action == Action::Refused {
                    format!("  {:<9} {}", "missing", a.from)
                } else {
                    format!("  {:<9} {} -> {}", a.action.name(), a.from, a.to)
                };
                if let Some(r) = &a.reason {
                    line.push_str(&format!(": {r}"));
                }
                out.push_str(&line);
                out.push('\n');
            }
        }
        for d in &self.dropped {
            out.push_str(&format!("dropped: {d}\n"));
        }
        for p in &self.problems {
            out.push_str(&format!("problem: {p}\n"));
        }
        out
    }
}

/// Copy the plan's files from `src_dir` into `dst_dir` (those it adds,
/// overwrites or renames), making directories as needed. Returns the
/// destination paths copied. Not undoable: undoing the import leaves them.
pub fn copy_assets(plan: &Plan, src_dir: &Path, dst_dir: &Path) -> std::io::Result<Vec<String>> {
    let mut done = Vec::new();
    for a in plan.assets.iter().filter(|a| a.action.imports()) {
        let to = dst_dir.join(&a.to);
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(src_dir.join(&a.from), &to)?;
        done.push(a.to.clone());
    }
    Ok(done)
}

/// Every item of a project that a [`Selection`] can name, by kind, in the
/// order the editor's tree lists them: tilesets, enums and layers in file
/// order, entity types, macros and levels by slug (levels world by world),
/// settings in [`SETTINGS`] order (those the project sets).
pub fn catalog(p: &Project) -> Vec<(Kind, Slug)> {
    let mut out = Vec::new();
    out.extend(p.tilesets.iter().map(|t| (Kind::Tileset, t.slug.clone())));
    out.extend(p.enums.iter().map(|e| (Kind::Enum, e.slug.clone())));
    if let Some(m) = &p.macros {
        out.extend(m.patterns.keys().map(|k| (Kind::Pattern, k.clone())));
        out.extend(m.legends.keys().map(|k| (Kind::Legend, k.clone())));
        out.extend(m.templates.keys().map(|k| (Kind::Template, k.clone())));
    }
    out.extend(p.layers.iter().map(|l| (Kind::Layer, l.slug.clone())));
    out.extend(
        p.level_fields
            .iter()
            .map(|f| (Kind::LevelField, f.slug.clone())),
    );
    out.extend(p.entities.iter().map(|e| (Kind::Entity, e.slug.clone())));
    for key in SETTINGS {
        if setting_of(p, key).is_some() {
            out.push((Kind::Setting, crate::id::slug(key)));
        }
    }
    for w in &p.worlds {
        out.extend(w.levels.iter().map(|l| (Kind::Level, l.slug.clone())));
    }
    out
}

/// A project setting's value as an import carries it; `None` when the
/// project doesn't set it.
fn setting_of(p: &Project, key: &str) -> Option<ProjectSetting> {
    match key {
        "grid_size" => Some(ProjectSetting::GridSize(p.grid_size)),
        "bg_color" => p.bg_color.clone().map(|c| ProjectSetting::BgColor(Some(c))),
        "collision_layer" => p
            .collision_layer
            .clone()
            .map(|c| ProjectSetting::CollisionLayer(Some(c))),
        "blocking_values" => p
            .blocking_values
            .clone()
            .map(|v| ProjectSetting::BlockingValues(Some(v))),
        "default_level_size" => p
            .default_level_size
            .map(|s| ProjectSetting::DefaultLevelSize(Some(s))),
        _ => None,
    }
}

/// An item of the source: kind, slug, and for a guide group its source world.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct Item {
    kind: Kind,
    slug: Slug,
    world: Option<Slug>,
}

impl Item {
    fn new(kind: Kind, slug: &Slug) -> Item {
        Item {
            kind,
            slug: slug.clone(),
            world: None,
        }
    }

    fn key(&self) -> String {
        format!("{}:{}", self.kind, self.slug)
    }
}

/// What the decision loop settled for an item.
#[derive(Debug, Clone)]
struct Decision {
    action: Action,
    to: Option<Slug>,
    reason: Option<String>,
}

fn invalid(msg: impl Into<String>) -> CommandError {
    CommandError::Invalid(msg.into())
}

/// A slug from free text (a project name): lowercase, every run of other
/// characters one `_`, a letter first.
fn slugify(s: &str) -> Option<Slug> {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('_') {
            out.push('_');
        }
    }
    let out = out.trim_end_matches('_');
    let out = if out.starts_with(|c: char| c.is_ascii_digit()) {
        format!("w_{out}")
    } else {
        out.to_string()
    };
    Slug::new(out).ok()
}

/// `<base>_2`, `<base>_3`, ...: the first one `taken` refuses.
fn next_free(base: &Slug, n0: u32, taken: impl Fn(&str) -> bool) -> Slug {
    let mut n = n0;
    loop {
        let cand = format!("{base}_{n}");
        if !taken(&cand) {
            return Slug::new(cand).expect("a slug with a suffix is a slug");
        }
        n += 1;
    }
}

/// Work out an import of `sel` from `src` into `dst` under `policy`.
/// Nothing changes. Refused: a selection naming something the source
/// doesn't have, a `world` placement into an unknown world, a `new_world`
/// placement naming a world the destination already has.
pub fn plan(
    src: &Project,
    dst: &Project,
    sel: &Selection,
    policy: &Policy,
) -> Result<Plan, CommandError> {
    Planner::new(src, dst, sel, policy)?.run()
}

struct Planner<'a> {
    src: &'a Project,
    dst: &'a Project,
    policy: &'a Policy,
    /// The source, with renames applied.
    s: Project,
    /// Every item to consider, in batch order.
    order: Vec<Item>,
    /// Pulled in by: dependency to the selected items needing it.
    required_by: BTreeMap<Item, BTreeSet<String>>,
    /// Current slug in `s` (after a rename).
    cur: BTreeMap<Item, Slug>,
    decided: BTreeMap<Item, Decision>,
    /// Source world slug to destination world slug.
    targets: BTreeMap<Slug, Slug>,
    /// Source worlds whose guide groups and world notes come along.
    whole_worlds: BTreeSet<Slug>,
    assets: Vec<AssetCopy>,
    problems: Vec<String>,
    dropped: Vec<String>,
    ids: Vec<IdChange>,
    /// Levels and guide groups: the destination world each went into.
    targets_of_items: BTreeMap<Item, Slug>,
}

impl<'a> Planner<'a> {
    fn new(
        src: &'a Project,
        dst: &'a Project,
        sel: &Selection,
        policy: &'a Policy,
    ) -> Result<Self, CommandError> {
        let mut s = src.clone();
        let tileset_refs = s.tileset_refs();
        let mut macro_refs = Vec::new();
        for (kind, mk) in [
            (Kind::Pattern, MacroKind::Pattern),
            (Kind::Legend, MacroKind::Legend),
            (Kind::Template, MacroKind::Template),
        ] {
            for (o, n) in s.macro_refs(mk) {
                macro_refs.push((o, kind, n));
            }
        }
        let mut p = Planner {
            src,
            dst,
            policy,
            s,
            order: Vec::new(),
            required_by: BTreeMap::new(),
            cur: BTreeMap::new(),
            decided: BTreeMap::new(),
            targets: BTreeMap::new(),
            whole_worlds: BTreeSet::new(),
            assets: Vec::new(),
            problems: Vec::new(),
            dropped: Vec::new(),
            ids: Vec::new(),
            targets_of_items: BTreeMap::new(),
        };
        // ---- the selection ----
        let mut wanted: Vec<Item> = Vec::new();
        let all = catalog(src);
        let defs = |all: &[(Kind, Slug)]| -> Vec<Item> {
            all.iter()
                .filter(|(k, _)| k.is_definition())
                .map(|(k, s)| Item::new(*k, s))
                .collect()
        };
        match sel.preset {
            Preset::Pick => {}
            Preset::DefsOnly => wanted.extend(defs(&all)),
            Preset::Everything => {
                wanted.extend(defs(&all));
                p.whole_worlds
                    .extend(src.worlds.iter().map(|w| w.slug.clone()));
            }
        }
        for (kind, list) in sel.lists() {
            for slug in list {
                if !all.iter().any(|(k, s)| *k == kind && s == slug) {
                    return Err(match kind {
                        Kind::Tileset => CommandError::UnknownTileset(slug.clone()),
                        Kind::Enum => CommandError::UnknownEnum(slug.clone()),
                        Kind::Layer => CommandError::UnknownLayer(slug.clone()),
                        Kind::Entity => CommandError::UnknownEntityType(slug.clone()),
                        Kind::Level => CommandError::UnknownLevel(slug.clone()),
                        Kind::Pattern | Kind::Legend | Kind::Template => {
                            CommandError::UnknownMacro {
                                kind: kind.macro_kind().expect("a macro kind"),
                                name: slug.clone(),
                            }
                        }
                        Kind::Setting if !SETTINGS.contains(&slug.as_str()) => invalid(format!(
                            "{slug} is not a setting an import carries ({})",
                            SETTINGS.join(", ")
                        )),
                        Kind::Setting => invalid(format!("the source doesn't set {slug}")),
                        _ => CommandError::UnknownField {
                            owner: "project level_fields".into(),
                            field: slug.clone(),
                        },
                    });
                }
                wanted.push(Item::new(kind, slug));
            }
        }
        for w in &sel.worlds {
            if src.world(w.as_str()).is_none() {
                return Err(CommandError::UnknownWorld(w.clone()));
            }
            p.whole_worlds.insert(w.clone());
        }
        for w in &src.worlds {
            if p.whole_worlds.contains(&w.slug) {
                wanted.extend(w.levels.iter().map(|l| Item::new(Kind::Level, &l.slug)));
            }
        }
        // ---- dependencies ----
        let wanted_set: BTreeSet<Item> = wanted.iter().cloned().collect();
        let mut closure: BTreeSet<Item> = wanted_set.clone();
        let mut queue: Vec<Item> = wanted.clone();
        while let Some(it) = queue.pop() {
            for dep in p.deps(&it, &tileset_refs, &macro_refs) {
                if !wanted_set.contains(&dep) {
                    let by = if wanted_set.contains(&it) {
                        vec![it.key()]
                    } else {
                        p.required_by
                            .get(&it)
                            .into_iter()
                            .flatten()
                            .cloned()
                            .collect()
                    };
                    p.required_by.entry(dep.clone()).or_default().extend(by);
                }
                if closure.insert(dep.clone()) {
                    queue.push(dep);
                }
            }
        }
        // Guide groups of whole worlds.
        for w in &src.worlds {
            if p.whole_worlds.contains(&w.slug) {
                for g in &w.guide_groups {
                    closure.insert(Item {
                        kind: Kind::GuideGroup,
                        slug: g.slug.clone(),
                        world: Some(w.slug.clone()),
                    });
                }
            }
        }
        // ---- the order: the catalog's, then guide groups ----
        for (k, slug) in &all {
            let it = Item::new(*k, slug);
            if closure.contains(&it) {
                p.order.push(it);
            }
        }
        p.order.extend(
            closure
                .iter()
                .filter(|i| i.kind == Kind::GuideGroup)
                .cloned(),
        );
        p.order.sort_by_key(|i| i.kind);
        for it in &p.order {
            p.cur.insert(it.clone(), it.slug.clone());
        }
        p.place()?;
        Ok(p)
    }

    /// What an item needs, as source items.
    fn deps(
        &self,
        it: &Item,
        tileset_refs: &[(Owner, Slug)],
        macro_refs: &[(Owner, Kind, Slug)],
    ) -> Vec<Item> {
        let s = &self.src;
        let mut out: Vec<Item> = Vec::new();
        let owner_is = |o: &Owner| match (o, it.kind) {
            (Owner::Def(DefKind::Entity, x), Kind::Entity)
            | (Owner::Def(DefKind::Enum, x), Kind::Enum)
            | (Owner::Def(DefKind::Layer, x), Kind::Layer)
            | (Owner::Layer(x), Kind::Layer)
            | (Owner::Template(x), Kind::Template)
            | (Owner::Level(x), Kind::Level) => *x == it.slug,
            _ => false,
        };
        for (o, ts) in tileset_refs {
            if owner_is(o) && s.tileset(ts.as_str()).is_some() {
                out.push(Item::new(Kind::Tileset, ts));
            }
        }
        for (o, k, n) in macro_refs {
            let has = s.macros.as_ref().is_some_and(|m| match k {
                Kind::Pattern => m.patterns.contains_key(n),
                Kind::Legend => m.legends.contains_key(n),
                _ => m.templates.contains_key(n),
            });
            if owner_is(o) && has {
                out.push(Item::new(*k, n));
            }
        }
        let field_deps = |fields: &[FieldDef], out: &mut Vec<Item>| {
            for f in fields {
                match &f.ty {
                    FieldType::Enum(crate::model::EnumSource::Project(e))
                        if s.enum_def(e.as_str()).is_some() =>
                    {
                        out.push(Item::new(Kind::Enum, e));
                    }
                    FieldType::Tile { tileset: Some(t) } if s.tileset(t.as_str()).is_some() => {
                        out.push(Item::new(Kind::Tileset, t));
                    }
                    FieldType::EntityRef { allowed_types, .. } => {
                        for t in allowed_types {
                            if s.entity_def(t.as_str()).is_some() {
                                out.push(Item::new(Kind::Entity, t));
                            }
                        }
                    }
                    _ => {}
                }
                // A `tile` default names a tileset too.
                if let Some(Value::Object(m)) = &f.default
                    && let Some(Value::String(t)) = m.get("tileset")
                    && let Some(def) = s.tileset(t)
                {
                    out.push(Item::new(Kind::Tileset, &def.slug));
                }
            }
        };
        match it.kind {
            Kind::Layer => {
                if let Some(def) = s.layer(it.slug.as_str()) {
                    if def.is_auto() {
                        for l in s
                            .layer_reads(it.slug.as_str())
                            .into_iter()
                            .chain(s.layer_writes(it.slug.as_str()))
                        {
                            if l != it.slug && s.layer(l.as_str()).is_some() {
                                out.push(Item::new(Kind::Layer, &l));
                            }
                        }
                        for g in s.rules_for(it.slug.as_str()).iter().flat_map(|r| &r.groups) {
                            if let Some(req) = &g.requires_level_field
                                && s.level_fields.iter().any(|f| f.slug == req.field)
                            {
                                out.push(Item::new(Kind::LevelField, &req.field));
                            }
                        }
                    }
                    if def.is_derived() {
                        for w in s.derived_writers(it.slug.as_str()) {
                            out.push(Item::new(Kind::Layer, w));
                        }
                    }
                }
            }
            Kind::Entity => {
                if let Some(d) = s.entity_def(it.slug.as_str()) {
                    field_deps(&d.fields, &mut out);
                }
            }
            Kind::LevelField => {
                if let Some(f) = s.level_fields.iter().find(|f| f.slug == it.slug) {
                    field_deps(std::slice::from_ref(f), &mut out);
                }
            }
            Kind::Setting if it.slug == "collision_layer" => {
                if let Some(c) = &s.collision_layer
                    && s.layer(c.as_str()).is_some()
                {
                    out.push(Item::new(Kind::Layer, c));
                }
            }
            Kind::Level => {
                if let Some(l) = s.level(it.slug.as_str()) {
                    let mut layers: BTreeSet<&Slug> = BTreeSet::new();
                    layers.extend(l.int_grids.iter().map(|g| &g.layer));
                    layers.extend(l.overrides.iter().map(|g| &g.layer));
                    layers.extend(l.tiles.iter().map(|g| &g.layer));
                    layers.extend(l.entities.iter().map(|g| &g.layer));
                    layers.extend(l.layers.keys());
                    // The auto layers drawing from what it holds.
                    for d in s.layers.iter().filter(|d| d.is_auto()) {
                        if s.layer_reads(d.slug.as_str())
                            .iter()
                            .any(|r| layers.contains(r))
                        {
                            out.push(Item::new(Kind::Layer, &d.slug));
                        }
                    }
                    for x in layers {
                        if s.layer(x.as_str()).is_some() {
                            out.push(Item::new(Kind::Layer, x));
                        }
                    }
                    for e in l.all_entities() {
                        if s.entity_def(e.ty.as_str()).is_some() {
                            out.push(Item::new(Kind::Entity, &e.ty));
                        }
                    }
                    for k in l.fields.keys() {
                        if s.level_fields.iter().any(|f| f.slug == *k) {
                            out.push(Item::new(Kind::LevelField, k));
                        }
                    }
                }
            }
            _ => {}
        }
        out.retain(|d| d != it);
        out.sort();
        out.dedup();
        out
    }

    /// Settle where levels go: [`Self::targets`].
    fn place(&mut self) -> Result<(), CommandError> {
        let pl = &self.policy.placement;
        let mut sources: BTreeSet<Slug> = self.whole_worlds.clone();
        for it in self.order.iter().filter(|i| i.kind == Kind::Level) {
            if let Some(w) = self.src.world_of_level(it.slug.as_str()) {
                sources.insert(w.slug.clone());
            }
        }
        if sources.is_empty() {
            return Ok(());
        }
        match pl.mode {
            PlacementMode::NewWorld => {
                let slug = match &pl.world {
                    Some(w) => {
                        if self.dst.world(w.as_str()).is_some() {
                            return Err(invalid(format!(
                                "the project already has a world {w}; import into it with the \
                                 world placement, or name a new one"
                            )));
                        }
                        w.clone()
                    }
                    None => {
                        let base =
                            slugify(&self.src.name).unwrap_or_else(|| crate::id::slug("imported"));
                        if self.dst.world(base.as_str()).is_some() {
                            next_free(&base, 2, |c| self.dst.world(c).is_some())
                        } else {
                            base
                        }
                    }
                };
                for w in sources {
                    self.targets.insert(w, slug.clone());
                }
            }
            PlacementMode::World => {
                let Some(w) = &pl.world else {
                    return Err(invalid("the world placement needs a world"));
                };
                if self.dst.world(w.as_str()).is_none() {
                    return Err(CommandError::UnknownWorld(w.clone()));
                }
                for s in sources {
                    self.targets.insert(s, w.clone());
                }
            }
            PlacementMode::Keep => {
                for s in sources {
                    self.targets.insert(s.clone(), s);
                }
            }
        }
        Ok(())
    }

    /// The destination world a source level goes into.
    fn target_of_level(&self, src_slug: &Slug) -> Option<Slug> {
        let w = self.src.world_of_level(src_slug.as_str())?;
        self.targets.get(&w.slug).cloned()
    }

    fn run(mut self) -> Result<Plan, CommandError> {
        self.plan_tileset_images();
        self.decide();
        self.prune_assets();
        let (commands, world_items) = self.build()?;
        Ok(self.finish(commands, world_items))
    }

    // ---- assets ----

    /// Rewrite the image of every tileset considered to `art/<file>` and
    /// plan its copy. Done before deciding, so a tileset imported before
    /// compares "same" against its copy.
    fn plan_tileset_images(&mut self) {
        let Some(_) = self.src.files.root() else {
            return;
        };
        let tilesets: Vec<Slug> = self
            .order
            .iter()
            .filter(|i| i.kind == Kind::Tileset)
            .map(|i| i.slug.clone())
            .collect();
        for t in tilesets {
            let Some(i) = self.s.tilesets.iter().position(|d| d.slug == t) else {
                continue;
            };
            let image = self.s.tilesets[i].image.clone();
            if let Some(to) = self.asset(&image) {
                self.s.tilesets[i].image = to;
            }
        }
    }

    /// Plan copying the source file `rel` under `art/`: the path it gets in
    /// the destination, or `None` to leave the reference as it is (the
    /// file is missing, or there is no source directory).
    fn asset(&mut self, rel: &str) -> Option<String> {
        let root = self.src.files.root()?;
        if let Some(a) = self.assets.iter().find(|a| a.from == rel) {
            return (a.action != Action::Refused).then(|| a.to.clone());
        }
        let from_path = root.join(rel);
        let name = Path::new(rel).file_name()?.to_string_lossy().into_owned();
        if !from_path.is_file() {
            self.assets.push(AssetCopy {
                from: rel.to_string(),
                to: rel.to_string(),
                action: Action::Refused,
                reason: Some("not in the source project; the reference is kept as it is".into()),
            });
            return None;
        }
        let dst_root = self.dst.files.root().map(Path::to_path_buf);
        let planned = |to: &str, assets: &[AssetCopy]| assets.iter().any(|a| a.to == to);
        let exists = |to: &str| dst_root.as_ref().is_some_and(|d| d.join(to).exists());
        let mut to = format!("art/{name}");
        let mut action = Action::Add;
        let mut reason = None;
        let same_bytes = |to: &str| {
            dst_root.as_ref().is_some_and(|d| {
                match (std::fs::read(d.join(to)), std::fs::read(&from_path)) {
                    (Ok(a), Ok(b)) => a == b,
                    _ => false,
                }
            })
        };
        if planned(&to, &self.assets) || exists(&to) {
            if !planned(&to, &self.assets) && same_bytes(&to) {
                action = Action::Same;
            } else {
                match self.policy.conflict(&format!("asset:{name}")) {
                    Conflict::Skip => {
                        action = Action::Skip;
                        reason = Some(format!("{to} is already there; ours is used"));
                    }
                    Conflict::Overwrite if !planned(&to, &self.assets) => {
                        action = Action::Overwrite;
                    }
                    _ => {
                        let p = Path::new(&name);
                        let stem = p
                            .file_stem()
                            .map(|s| s.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        let ext = p
                            .extension()
                            .map(|e| format!(".{}", e.to_string_lossy()))
                            .unwrap_or_default();
                        let mut n = 2;
                        loop {
                            let cand = format!("art/{stem}_{n}{ext}");
                            if !planned(&cand, &self.assets) && !exists(&cand) {
                                to = cand;
                                break;
                            }
                            n += 1;
                        }
                        action = Action::Rename;
                    }
                }
            }
        }
        self.assets.push(AssetCopy {
            from: rel.to_string(),
            to: to.clone(),
            action,
            reason,
        });
        Some(to)
    }

    /// Keep only the files of tilesets that are imported (and path values,
    /// planned later).
    fn prune_assets(&mut self) {
        let images: BTreeSet<String> = self
            .order
            .iter()
            .filter(|i| i.kind == Kind::Tileset && self.decided[*i].action.imports())
            .filter_map(|i| self.src.tileset(i.slug.as_str()).map(|t| t.image.clone()))
            .collect();
        self.assets.retain(|a| images.contains(&a.from));
    }

    // ---- collisions ----

    /// The definition (or level) `it` names in `p` under `slug`, as JSON
    /// for comparing. A level leaves out its id and position.
    fn value_of(&self, p: &Project, it: &Item, slug: &Slug, world: Option<&Slug>) -> Option<Value> {
        let v = |x: Result<Value, serde_json::Error>| x.ok();
        match it.kind {
            Kind::Tileset => v(serde_json::to_value(p.tileset(slug.as_str())?)),
            Kind::Enum => v(serde_json::to_value(p.enum_def(slug.as_str())?)),
            Kind::Entity => v(serde_json::to_value(p.entity_def(slug.as_str())?)),
            Kind::LevelField => v(serde_json::to_value(
                p.level_fields.iter().find(|f| f.slug == *slug)?,
            )),
            Kind::Layer => {
                let def = p.layer(slug.as_str())?;
                v(serde_json::to_value((def, p.rules_for(slug.as_str()))))
            }
            Kind::Pattern | Kind::Legend | Kind::Template => v(serde_json::to_value(
                p.macro_def(it.kind.macro_kind()?, slug)?,
            )),
            Kind::Setting => v(serde_json::to_value(setting_of(p, slug.as_str())?)),
            Kind::GuideGroup => v(serde_json::to_value(
                p.world(world?.as_str())?.guide_group(slug.as_str())?,
            )),
            Kind::Level => {
                let l = p.level(slug.as_str())?;
                let (mut meta, content) = p.split_level(l.clone()).ok()?;
                meta.id = Id::derived("");
                meta.world_px = [0, 0];
                v(serde_json::to_value((meta, content)))
            }
            Kind::World => None,
        }
    }

    /// Where a guide group of source world `w` goes.
    fn target_world(&self, w: Option<&Slug>) -> Option<Slug> {
        self.targets.get(w?).cloned()
    }

    /// Decide every item, renaming in the source clone until nothing more
    /// changes (a rename can make another item differ from ours).
    fn decide(&mut self) {
        let items = self.order.clone();
        for _round in 0..=items.len() {
            let mut renamed = false;
            for it in &items {
                if self
                    .decided
                    .get(it)
                    .is_some_and(|d| d.action == Action::Rename)
                {
                    continue;
                }
                let cur = self.cur[it].clone();
                let target = self.target_world(it.world.as_ref());
                let theirs = self.value_of(&self.s, it, &cur, it.world.as_ref());
                let ours = self.value_of(self.dst, it, &cur, target.as_ref());
                let d = match (&theirs, &ours) {
                    (None, _) if it.kind == Kind::Setting => Decision {
                        action: Action::Same,
                        to: None,
                        reason: Some("not set in the source".into()),
                    },
                    (_, None) => Decision {
                        action: Action::Add,
                        to: None,
                        reason: None,
                    },
                    (Some(a), Some(b)) if a == b => Decision {
                        action: Action::Same,
                        to: None,
                        reason: None,
                    },
                    // A project with no levels yet takes the source's grid
                    // (unless told otherwise): nothing of ours depends on it.
                    _ if it.kind == Kind::Setting
                        && it.slug == "grid_size"
                        && self.dst.levels().next().is_none()
                        && !self.policy.conflicts.contains_key(&it.key()) =>
                    {
                        Decision {
                            action: Action::Overwrite,
                            to: None,
                            reason: Some("this project has no levels yet".into()),
                        }
                    }
                    _ => match self.policy.conflict(&it.key()) {
                        Conflict::Skip => Decision {
                            action: Action::Skip,
                            to: None,
                            reason: Some("this project has its own; kept".into()),
                        },
                        Conflict::Overwrite => Decision {
                            action: Action::Overwrite,
                            to: None,
                            reason: None,
                        },
                        Conflict::Rename if it.kind == Kind::Setting => Decision {
                            action: Action::Skip,
                            to: None,
                            reason: Some("a setting can't be renamed; kept ours".into()),
                        },
                        Conflict::Rename => match self.rename(it, &cur, target.as_ref()) {
                            Ok(to) => {
                                renamed = true;
                                self.cur.insert(it.clone(), to.clone());
                                Decision {
                                    action: Action::Rename,
                                    to: Some(to),
                                    reason: None,
                                }
                            }
                            Err(e) => Decision {
                                action: Action::Skip,
                                to: None,
                                reason: Some(format!("couldn't rename it ({e}); kept ours")),
                            },
                        },
                    },
                };
                self.decided.insert(it.clone(), d);
            }
            if !renamed {
                break;
            }
        }
    }

    /// Rename `it` (now `cur`) in the source clone to the first free
    /// `<slug>_N`, with the source's own rename command.
    fn rename(&mut self, it: &Item, cur: &Slug, target: Option<&Slug>) -> Result<Slug, String> {
        let mut n = 2;
        let mut last = String::new();
        for _ in 0..32 {
            let to = next_free(&it.slug, n, |c| {
                slug_taken(&self.s, self.dst, it, target, c)
            });
            n = to
                .as_str()
                .rsplit('_')
                .next()
                .and_then(|x| x.parse::<u32>().ok())
                .unwrap_or(n)
                + 1;
            let cmd = match it.kind {
                Kind::Tileset => Command::RenameTileset {
                    from: cur.clone(),
                    to: to.clone(),
                },
                Kind::Enum => Command::RenameEnum {
                    from: cur.clone(),
                    to: to.clone(),
                },
                Kind::Entity => Command::RenameEntityType {
                    from: cur.clone(),
                    to: to.clone(),
                },
                Kind::Layer => Command::RenameLayer {
                    from: cur.clone(),
                    to: to.clone(),
                },
                Kind::LevelField => Command::RenameLevelField {
                    from: cur.clone(),
                    to: to.clone(),
                },
                Kind::Pattern | Kind::Legend | Kind::Template => Command::RenameMacro {
                    kind: it.kind.macro_kind().expect("a macro kind"),
                    from: cur.clone(),
                    to: to.clone(),
                },
                Kind::Level => {
                    let Some(w) = self.s.world_of_level(cur.as_str()) else {
                        return Err(format!("no level {cur}"));
                    };
                    let name = w
                        .level(cur.as_str())
                        .map(|l| l.name.clone())
                        .unwrap_or_default();
                    Command::RenameLevel {
                        world: w.slug.clone(),
                        level: cur.clone(),
                        slug: to.clone(),
                        name,
                    }
                }
                Kind::GuideGroup => {
                    // No command renames a guide group: rewrite the clone.
                    let Some(w) = it
                        .world
                        .as_ref()
                        .and_then(|w| self.s.worlds.iter_mut().find(|x| x.slug == *w))
                    else {
                        return Err("no such world".into());
                    };
                    for g in w.guide_groups.iter_mut().filter(|g| g.slug == *cur) {
                        g.slug = to.clone();
                    }
                    for n in &mut w.notes {
                        if let Shape::Guide { group: Some(g), .. } = &mut n.shape
                            && g == cur
                        {
                            *g = to.clone();
                        }
                    }
                    return Ok(to);
                }
                Kind::Setting | Kind::World => return Err("can't be renamed".into()),
            };
            match self.s.apply(cmd) {
                Ok(_) => return Ok(to),
                Err(e) => last = e.to_string(),
            }
        }
        Err(last)
    }

    // ---- the batch ----

    /// Make every item's commands, trying each on a copy of the
    /// destination; refused ones are left out and marked. Returns the
    /// commands and the world items.
    fn build(&mut self) -> Result<(Vec<Command>, Vec<PlanItem>), CommandError> {
        let mut d = self.dst.clone();
        let mut out: Vec<Command> = Vec::new();
        let try_apply = |d: &mut Project, out: &mut Vec<Command>, cmds: Vec<Command>| {
            if cmds.is_empty() {
                return Ok(());
            }
            let batch = Command::Batch {
                label: String::new(),
                commands: cmds.clone(),
            };
            match d.apply(batch) {
                Ok(_) => {
                    out.extend(cmds);
                    Ok(())
                }
                Err(e) => Err(match e {
                    CommandError::Batch { source, .. } => source.to_string(),
                    e => e.to_string(),
                }),
            }
        };

        // ---- definitions and settings ----
        let defs: Vec<Item> = self
            .order
            .iter()
            .filter(|i| i.kind.is_definition())
            .cloned()
            .collect();
        for it in &defs {
            let d0 = &self.decided[it];
            if !d0.action.imports() {
                continue;
            }
            let cmds = self.def_commands(it, &d);
            if let Err(e) = try_apply(&mut d, &mut out, cmds) {
                self.refuse(it, e);
            }
        }

        // ---- ids ----
        let levels: Vec<Item> = self
            .order
            .iter()
            .filter(|i| i.kind == Kind::Level && self.decided[*i].action.imports())
            .cloned()
            .collect();
        // Levels of ours being replaced: their ids are free for the import.
        let overwritten: BTreeSet<Slug> = levels
            .iter()
            .filter(|i| self.decided[*i].action == Action::Overwrite)
            .map(|i| self.cur[i].clone())
            .collect();
        let kept = |l: &&Level| !overwritten.contains(&l.slug);
        let mut level_ids: HashSet<Id> = self
            .dst
            .levels()
            .filter(kept)
            .map(|l| l.id.clone())
            .collect();
        let mut entity_ids: HashSet<Id> = self
            .dst
            .levels()
            .filter(kept)
            .flat_map(|l| l.all_entities().map(|e| e.id.clone()))
            .collect();
        let dst_entities = entity_ids.clone();
        let mut note_ids: HashSet<Id> = self
            .dst
            .worlds
            .iter()
            .flat_map(|w| w.notes.iter().map(|n| n.id.clone()))
            .chain(
                self.dst
                    .levels()
                    .filter(kept)
                    .flat_map(|l| l.notes.iter().map(|n| n.id.clone())),
            )
            .collect();
        let dst_notes = note_ids.clone();
        let mut lmap: BTreeMap<Id, Id> = BTreeMap::new();
        let mut emap: BTreeMap<Id, Id> = BTreeMap::new();
        let mut nmap: BTreeMap<Id, Id> = BTreeMap::new();
        let reroll = |kind: &str,
                      id: &Id,
                      seed: String,
                      taken: &mut HashSet<Id>,
                      map: &mut BTreeMap<Id, Id>,
                      ids: &mut Vec<IdChange>| {
            if map.contains_key(id) {
                return;
            }
            let new = if taken.insert(id.clone()) {
                id.clone()
            } else {
                let n = derived_free(&seed, taken);
                ids.push(IdChange {
                    kind: kind.into(),
                    from: id.clone(),
                    to: n.clone(),
                });
                n
            };
            map.insert(id.clone(), new);
        };
        for it in &levels {
            let Some(l) = self.s.level(self.cur[it].as_str()) else {
                continue;
            };
            reroll(
                "level",
                &l.id,
                format!("import:level:{}", l.id),
                &mut level_ids,
                &mut lmap,
                &mut self.ids,
            );
            let lid = lmap[&l.id].clone();
            for e in l.all_entities() {
                reroll(
                    "entity",
                    &e.id,
                    format!("import:{lid}:{}", e.id),
                    &mut entity_ids,
                    &mut emap,
                    &mut self.ids,
                );
            }
            for n in &l.notes {
                reroll(
                    "note",
                    &n.id,
                    format!("import:{lid}:{}", n.id),
                    &mut note_ids,
                    &mut nmap,
                    &mut self.ids,
                );
            }
        }
        let world_notes: Vec<(Slug, Annotation)> = self
            .s
            .worlds
            .iter()
            .filter(|w| self.whole_worlds.contains(&w.slug))
            .flat_map(|w| w.notes.iter().map(move |n| (w.slug.clone(), n.clone())))
            .collect();
        for (w, n) in &world_notes {
            reroll(
                "note",
                &n.id,
                format!("import:world:{w}:{}", n.id),
                &mut note_ids,
                &mut nmap,
                &mut self.ids,
            );
        }
        let imported_levels: BTreeSet<Slug> = levels.iter().map(|i| self.cur[i].clone()).collect();
        let refs = Refs {
            emap: &emap,
            nmap: &nmap,
            levels: &imported_levels,
            dst: self.dst,
            dst_entities: &dst_entities,
            dst_notes: &dst_notes,
        };

        // ---- worlds ----
        let mut world_items = Vec::new();
        // New worlds whose level order follows their source world's.
        let mut keep_order: Vec<(Slug, Slug)> = Vec::new();
        let mut needed: BTreeSet<Slug> = BTreeSet::new();
        for it in &levels {
            if self.decided[it].action != Action::Overwrite
                && let Some(t) = self.target_of_level(&it.slug)
            {
                needed.insert(t);
            }
        }
        for it in self.order.iter().filter(|i| i.kind == Kind::GuideGroup) {
            if self.decided[it].action.imports()
                && let Some(t) = self.target_world(it.world.as_ref())
            {
                needed.insert(t);
            }
        }
        for (w, _) in &world_notes {
            if let Some(t) = self.targets.get(w) {
                needed.insert(t.clone());
            }
        }
        // In display order of the source's worlds (new world: one).
        let mut seen = BTreeSet::new();
        let order: Vec<Slug> = self
            .src
            .worlds
            .iter()
            .filter_map(|w| self.targets.get(&w.slug).cloned())
            .filter(|t| needed.contains(t) && seen.insert(t.clone()))
            .collect();
        for t in order {
            if self.dst.world(t.as_str()).is_some() {
                world_items.push(PlanItem {
                    kind: Kind::World,
                    slug: t,
                    action: Action::Same,
                    to: None,
                    world: None,
                    required_by: Vec::new(),
                    reason: Some("exists; imported levels go into it".into()),
                });
                continue;
            }
            let name = match self.policy.placement.mode {
                PlacementMode::Keep => self.src.world(t.as_str()).and_then(|w| w.name.clone()),
                _ => Some(self.src.name.clone()).filter(|n| !n.is_empty()),
            };
            let mut cmds = vec![Command::CreateWorld {
                slug: t.clone(),
                name,
            }];
            // A new world made of one source world keeps its layout (and,
            // once its levels are in, its level order).
            let sources: Vec<&Slug> = self
                .targets
                .iter()
                .filter(|(_, to)| **to == t)
                .map(|(from, _)| from)
                .collect();
            let only = match sources[..] {
                [one] => self.s.world(one.as_str()),
                _ => None,
            };
            if let Some(sw) = only {
                if !sw.layout.is_default() {
                    cmds.push(Command::SetWorldLayout {
                        world: t.clone(),
                        layout: sw.layout.clone(),
                        restore: false,
                    });
                }
                if !sw.level_order.is_empty() || sw.layout.is_strip() {
                    keep_order.push((t.clone(), sw.slug.clone()));
                }
            }
            let (action, reason) = match try_apply(&mut d, &mut out, cmds) {
                Ok(()) => (Action::Add, None),
                Err(e) => {
                    keep_order.retain(|(w, _)| *w != t);
                    (Action::Refused, Some(e))
                }
            };
            world_items.push(PlanItem {
                kind: Kind::World,
                slug: t,
                action,
                to: None,
                world: None,
                required_by: Vec::new(),
                reason,
            });
        }

        // ---- guide groups ----
        let offset = self.policy.placement.offset;
        let groups: Vec<Item> = self
            .order
            .iter()
            .filter(|i| i.kind == Kind::GuideGroup)
            .cloned()
            .collect();
        for it in &groups {
            if !self.decided[it].action.imports() {
                continue;
            }
            let Some(target) = self.target_world(it.world.as_ref()) else {
                continue;
            };
            let cur = self.cur[it].clone();
            let Some(group) = it
                .world
                .as_ref()
                .and_then(|w| self.s.world(w.as_str()))
                .and_then(|w| w.guide_group(cur.as_str()))
                .cloned()
            else {
                continue;
            };
            let cmd = Command::SetGuideGroup {
                world: target,
                group,
                index: None,
            };
            if let Err(e) = try_apply(&mut d, &mut out, vec![cmd]) {
                self.refuse(it, e);
            }
        }

        // ---- levels ----
        for it in &levels {
            let cur = self.cur[it].clone();
            let Some(level) = self.s.level(cur.as_str()).cloned() else {
                continue;
            };
            if self.s.grid_size != d.grid_size {
                self.refuse(
                    it,
                    format!(
                        "the source grid is {} px, this project's is {} px; a level can't move \
                         between grids",
                        self.s.grid_size, d.grid_size
                    ),
                );
                continue;
            }
            let (mut meta, mut content) = match self.s.split_level(level) {
                Ok(x) => x,
                Err(e) => {
                    self.refuse(it, e.to_string());
                    continue;
                }
            };
            meta.id = lmap.get(&meta.id).cloned().unwrap_or(meta.id);
            let who = format!("level {cur}");
            let level_fields = self.s.level_fields.clone();
            let mut dropped = Vec::new();
            let mut assets = Vec::new();
            remap_fields(
                &mut meta.fields,
                &level_fields,
                &refs,
                &who,
                &mut dropped,
                &mut assets,
            );
            let old_entities: BTreeSet<Id> = content
                .entities
                .iter()
                .flat_map(|el| el.entities.iter().map(|e| e.id.clone()))
                .collect();
            self.remap_content(&mut content, &refs, &mut dropped, &mut assets);
            remap_groups(
                &mut meta,
                &content,
                &old_entities,
                &refs,
                &who,
                &mut dropped,
            );
            let mut fix_paths = BTreeMap::new();
            for a in assets {
                if let Some(to) = self.asset(&a) {
                    fix_paths.insert(a, to);
                }
            }
            if !fix_paths.is_empty() {
                self.rewrite_paths(&mut meta.fields, &level_fields, &fix_paths);
                for el in &mut content.entities {
                    for e in &mut el.entities {
                        let defs = self
                            .s
                            .entity_def(e.ty.as_str())
                            .map(|d| d.fields.clone())
                            .unwrap_or_default();
                        self.rewrite_paths(&mut e.fields, &defs, &fix_paths);
                    }
                }
            }
            let cmd = if self.decided[it].action == Action::Overwrite {
                let Some(w) = self.dst.world_of_level(cur.as_str()) else {
                    continue;
                };
                meta.world_px = w.level(cur.as_str()).map_or(meta.world_px, |l| l.world_px);
                if let Some(d) = self.decided.get_mut(it) {
                    d.reason = None;
                }
                self.set_world(it, w.slug.clone());
                Command::ReplaceLevel {
                    world: w.slug.clone(),
                    level: meta,
                    content,
                    restore: false,
                }
            } else {
                let Some(target) = self.target_of_level(&it.slug) else {
                    continue;
                };
                meta.world_px = [meta.world_px[0] + offset[0], meta.world_px[1] + offset[1]];
                // Grid vania: onto the lattice (a strip places it itself).
                if let Some(w) = d.world(target.as_str()) {
                    meta.world_px = w.layout.snap(d.grid_size, meta.world_px);
                }
                self.set_world(it, target.clone());
                Command::CreateLevel {
                    world: target,
                    level: meta,
                    content,
                    restore: false,
                }
            };
            match try_apply(&mut d, &mut out, vec![cmd]) {
                Ok(()) => self.dropped.extend(dropped),
                Err(e) => self.refuse(it, e),
            }
        }

        // ---- level order of new worlds ----
        for (t, src) in keep_order {
            let (Some(sw), Some(tw)) = (self.s.world(src.as_str()), d.world(t.as_str())) else {
                continue;
            };
            let order: Vec<Slug> = sw
                .levels
                .iter()
                .filter(|l| tw.level(l.slug.as_str()).is_some())
                .map(|l| l.slug.clone())
                .collect();
            if order != tw.level_order {
                let cmd = Command::ReorderLevels {
                    world: t.clone(),
                    order,
                    restore: false,
                };
                if let Err(e) = try_apply(&mut d, &mut out, vec![cmd]) {
                    self.problems
                        .push(format!("world {t}: level order not carried over: {e}"));
                }
            }
        }

        // ---- world notes ----
        for (w, mut n) in world_notes {
            let Some(target) = self.targets.get(&w).cloned() else {
                continue;
            };
            let old = n.id.clone();
            n.id = nmap.get(&old).cloned().unwrap_or(old.clone());
            let who = format!("note {}", n.id);
            let mut dropped = Vec::new();
            if !remap_note(&mut n, &refs, &who, &mut dropped) {
                self.dropped.extend(dropped);
                continue;
            }
            // A guide whose group stayed behind (refused) is put in none.
            if let Shape::Guide { group, .. } = &mut n.shape
                && let Some(g) = group
                && d.world(target.as_str())
                    .is_none_or(|tw| tw.guide_group(g.as_str()).is_none())
            {
                self.problems.push(format!(
                    "{who}: world {target} has no guide group {g}; the guide was put in no group"
                ));
                *group = None;
            }
            n.shape.translate(offset);
            let cmd = Command::AddNote {
                scope: NoteScope::World(target),
                note: n,
                index: None,
            };
            match try_apply(&mut d, &mut out, vec![cmd]) {
                Ok(()) => self.dropped.extend(dropped),
                Err(e) => self
                    .problems
                    .push(format!("{who} (from world {w}): not imported: {e}")),
            }
        }
        Ok((out, world_items))
    }

    fn set_world(&mut self, it: &Item, w: Slug) {
        self.targets_of_items.insert(it.clone(), w);
    }

    fn refuse(&mut self, it: &Item, why: String) {
        let d = self.decided.get_mut(it).expect("decided");
        let why = if d.action == Action::Overwrite {
            format!("overwrite refused: {why}; kept ours")
        } else {
            why
        };
        d.action = Action::Refused;
        d.to = None;
        d.reason = Some(why);
    }

    /// The commands that import definition `it` (its decision imports),
    /// against `d`, the destination as the batch has it so far.
    fn def_commands(&self, it: &Item, d: &Project) -> Vec<Command> {
        let s = &self.s;
        let cur = &self.cur[it];
        let c = cur.as_str();
        let overwrite = self.decided[it].action == Action::Overwrite;
        match it.kind {
            Kind::Tileset => s
                .tileset(c)
                .map(|def| Command::SetTilesetDef {
                    tileset: cur.clone(),
                    def: def.clone(),
                    index: None,
                })
                .into_iter()
                .collect(),
            Kind::Enum => s
                .enum_def(c)
                .map(|def| Command::SetEnumDef {
                    enum_: cur.clone(),
                    def: def.clone(),
                    index: None,
                })
                .into_iter()
                .collect(),
            Kind::Entity => s
                .entity_def(c)
                .map(|def| Command::SetEntityDef {
                    entity: cur.clone(),
                    def: def.clone(),
                })
                .into_iter()
                .collect(),
            Kind::LevelField => s
                .level_fields
                .iter()
                .find(|f| f.slug == *cur)
                .map(|def| Command::SetLevelFieldDef {
                    field: cur.clone(),
                    def: def.clone(),
                    index: None,
                })
                .into_iter()
                .collect(),
            Kind::Pattern | Kind::Legend | Kind::Template => {
                let mk = it.kind.macro_kind().expect("a macro kind");
                s.macro_def(mk, cur)
                    .map(|def: MacroDef| Command::SetMacro {
                        kind: mk,
                        name: cur.clone(),
                        def,
                    })
                    .into_iter()
                    .collect()
            }
            Kind::Setting => setting_of(s, c)
                .map(|setting| Command::SetProjectSetting { setting })
                .into_iter()
                .collect(),
            Kind::Layer => {
                let Some(def) = s.layer(c) else {
                    return Vec::new();
                };
                let rules = s.rules_for(c).cloned();
                if !overwrite || d.layer(c).is_none() {
                    return vec![Command::SetLayerDef {
                        layer: cur.clone(),
                        def: def.clone(),
                        index: None,
                        content: Vec::new(),
                        rules,
                    }];
                }
                let mut out = vec![Command::SetLayerDef {
                    layer: cur.clone(),
                    def: def.clone(),
                    index: None,
                    content: Vec::new(),
                    rules: None,
                }];
                // Theirs replaces our rule set: their groups and stamps in
                // their order, ours that they don't have taken out.
                let ours = d.rules_for(c);
                let theirs_groups: Vec<_> = rules.iter().flat_map(|r| &r.groups).collect();
                let theirs_stamps: Vec<_> = rules.iter().flat_map(|r| &r.stamps).collect();
                if let Some(o) = ours {
                    for g in &o.groups {
                        if !theirs_groups.iter().any(|t| t.slug == g.slug) {
                            out.push(Command::RemoveRuleGroup {
                                layer: cur.clone(),
                                group: g.slug.clone(),
                                keep_empty: true,
                            });
                        }
                    }
                    for st in &o.stamps {
                        if !theirs_stamps.iter().any(|t| t.slug == st.slug) {
                            out.push(Command::RemoveStamp {
                                layer: cur.clone(),
                                stamp: st.slug.clone(),
                                keep_uses: true,
                                keep_empty: true,
                            });
                        }
                    }
                }
                for (i, st) in theirs_stamps.iter().enumerate() {
                    out.push(Command::SetStamp {
                        layer: cur.clone(),
                        stamp: st.slug.clone(),
                        def: (*st).clone(),
                        index: Some(i),
                    });
                }
                for (i, g) in theirs_groups.iter().enumerate() {
                    out.push(Command::SetRuleGroup {
                        layer: cur.clone(),
                        group: g.slug.clone(),
                        def: (*g).clone(),
                        index: Some(i),
                    });
                }
                out
            }
            Kind::World | Kind::GuideGroup | Kind::Level => Vec::new(),
        }
    }

    /// Rewrite a level's content for the destination: ids, references,
    /// links. Path values are collected into `assets`.
    fn remap_content(
        &self,
        content: &mut LevelContent,
        refs: &Refs<'_>,
        dropped: &mut Vec<String>,
        assets: &mut Vec<String>,
    ) {
        for el in &mut content.entities {
            for e in &mut el.entities {
                let old = e.id.clone();
                e.id = refs.emap.get(&old).cloned().unwrap_or(old);
                let defs = self
                    .s
                    .entity_def(e.ty.as_str())
                    .map(|d| d.fields.clone())
                    .unwrap_or_default();
                let who = format!("entity {} ({})", e.id, e.ty);
                remap_fields(&mut e.fields, &defs, refs, &who, dropped, assets);
            }
        }
        let notes = std::mem::take(&mut content.notes);
        for mut n in notes {
            let old = n.id.clone();
            n.id = refs.nmap.get(&old).cloned().unwrap_or(old);
            let who = format!("note {}", n.id);
            if remap_note(&mut n, refs, &who, dropped) {
                content.notes.push(n);
            }
        }
    }

    /// Point `path` field values at their copies.
    fn rewrite_paths(
        &self,
        fields: &mut BTreeMap<Slug, Value>,
        defs: &[FieldDef],
        to: &BTreeMap<String, String>,
    ) {
        for (slug, v) in fields.iter_mut() {
            let Some(fd) = defs.iter().find(|f| f.slug == *slug) else {
                continue;
            };
            if !matches!(fd.ty, FieldType::Path { .. }) {
                continue;
            }
            let one = |x: &mut Value| {
                if let Value::String(s) = x
                    && let Some(n) = to.get(s.as_str())
                {
                    *s = n.clone();
                }
            };
            match v {
                Value::Array(items) => items.iter_mut().for_each(one),
                x => one(x),
            }
        }
    }

    fn finish(self, commands: Vec<Command>, world_items: Vec<PlanItem>) -> Plan {
        let mut items: Vec<PlanItem> = Vec::new();
        for it in &self.order {
            let d = &self.decided[it];
            let world = match it.kind {
                Kind::Level | Kind::GuideGroup => {
                    self.targets_of_items
                        .get(it)
                        .cloned()
                        .or_else(|| match it.kind {
                            Kind::Level => self.target_of_level(&it.slug),
                            _ => self.target_world(it.world.as_ref()),
                        })
                }
                _ => None,
            };
            items.push(PlanItem {
                kind: it.kind,
                slug: it.slug.clone(),
                action: d.action,
                to: d.to.clone(),
                world,
                required_by: self
                    .required_by
                    .get(it)
                    .map(|s| s.iter().cloned().collect())
                    .unwrap_or_default(),
                reason: d.reason.clone(),
            });
        }
        // Worlds go before guide groups and levels.
        let at = items
            .iter()
            .position(|i| i.kind > Kind::World)
            .unwrap_or(items.len());
        items.splice(at..at, world_items);
        let count = |f: &dyn Fn(&PlanItem) -> bool| items.iter().filter(|i| f(i)).count();
        let defs = count(&|i| i.kind.is_definition() && i.action.imports());
        let levels = count(&|i| i.kind == Kind::Level && i.action.imports());
        let renamed = count(&|i| i.action == Action::Rename);
        let skipped = count(&|i| i.action == Action::Skip);
        let same = count(&|i| i.action == Action::Same && i.kind != Kind::World);
        let refused = count(&|i| i.action == Action::Refused);
        let plural = |n: usize, what: &str| {
            if n == 1 {
                format!("1 {what}")
            } else {
                format!("{n} {what}s")
            }
        };
        let mut summary = format!(
            "{}, {}, {renamed} renamed, {skipped} skipped",
            plural(defs, "definition"),
            plural(levels, "level")
        );
        if same > 0 {
            summary.push_str(&format!(", {same} same"));
        }
        if refused > 0 {
            summary.push_str(&format!(", {refused} refused"));
        }
        let files = self.assets.iter().filter(|a| a.action.imports()).count();
        if files > 0 {
            summary.push_str(&format!(", {}", plural(files, "file")));
        }
        Plan {
            source: self.src.name.clone(),
            label: format!("Import from {}", self.src.name),
            summary,
            items,
            ids: self.ids,
            dropped: self.dropped,
            problems: self.problems,
            assets: self.assets,
            commands,
        }
    }
}

fn slug_taken(s: &Project, d: &Project, it: &Item, target: Option<&Slug>, c: &str) -> bool {
    match it.kind {
        Kind::Tileset => s.tileset(c).is_some() || d.tileset(c).is_some(),
        Kind::Enum => s.enum_def(c).is_some() || d.enum_def(c).is_some(),
        Kind::Entity => s.entity_def(c).is_some() || d.entity_def(c).is_some(),
        Kind::Layer => s.layer(c).is_some() || d.layer(c).is_some(),
        Kind::LevelField => {
            s.level_fields.iter().any(|f| f.slug == c) || d.level_fields.iter().any(|f| f.slug == c)
        }
        Kind::Pattern | Kind::Legend | Kind::Template => {
            let mk = it.kind.macro_kind().expect("a macro kind");
            let c = crate::id::slug(c);
            s.macro_def(mk, &c).is_some()
                || d.macro_def(mk, &c).is_some()
                || (mk == MacroKind::Template && crate::model::Macros::is_builtin(c.as_str()))
        }
        Kind::Level => s.level(c).is_some() || d.level(c).is_some(),
        Kind::GuideGroup => {
            it.world
                .as_ref()
                .and_then(|w| s.world(w.as_str()))
                .is_some_and(|w| w.guide_group(c).is_some())
                || target
                    .and_then(|w| d.world(w.as_str()))
                    .is_some_and(|w| w.guide_group(c).is_some())
        }
        Kind::Setting | Kind::World => true,
    }
}

/// What references in imported content resolve to.
struct Refs<'a> {
    emap: &'a BTreeMap<Id, Id>,
    nmap: &'a BTreeMap<Id, Id>,
    /// Imported levels, by slug (as renamed).
    levels: &'a BTreeSet<Slug>,
    dst: &'a Project,
    /// The destination's entity and note ids (those the import keeps).
    dst_entities: &'a HashSet<Id>,
    dst_notes: &'a HashSet<Id>,
}

impl Refs<'_> {
    fn entity(&self, id: &Id) -> Option<Id> {
        self.emap
            .get(id)
            .cloned()
            .or_else(|| self.dst_entities.contains(id).then(|| id.clone()))
    }

    fn note(&self, id: &Id) -> Option<Id> {
        self.nmap
            .get(id)
            .cloned()
            .or_else(|| self.dst_notes.contains(id).then(|| id.clone()))
    }

    fn level(&self, slug: &str) -> bool {
        self.levels.contains(slug) || self.dst.level(slug).is_some()
    }

    /// A link as it reads here, or `None` when nothing resolves it.
    fn link(&self, link: &str) -> Option<String> {
        match parse_link(link) {
            Link::Level(s) => self.level(s).then(|| link.to_string()),
            Link::Entity(s) => Id::new(s)
                .ok()
                .and_then(|id| self.entity(&id))
                .map(|n| format!("entity:{n}")),
            Link::Note(s) => Id::new(s)
                .ok()
                .and_then(|id| self.note(&id))
                .map(|n| format!("note:{n}")),
            Link::Url(_) => Some(link.to_string()),
        }
    }
}

/// Rewrite the `entity_ref` values of `fields` through `refs` (dropping
/// those nothing resolves) and collect `path` values.
fn remap_fields(
    fields: &mut BTreeMap<Slug, Value>,
    defs: &[FieldDef],
    refs: &Refs<'_>,
    who: &str,
    dropped: &mut Vec<String>,
    assets: &mut Vec<String>,
) {
    for (slug, v) in fields.iter_mut() {
        let Some(fd) = defs.iter().find(|f| f.slug == *slug) else {
            continue;
        };
        match &fd.ty {
            FieldType::EntityRef { .. } => {
                let mut one = |s: &str| -> Option<Value> {
                    let Ok(id) = Id::new(s) else {
                        return Some(Value::String(s.to_string()));
                    };
                    match refs.entity(&id) {
                        Some(n) => Some(Value::String(n.to_string())),
                        None => {
                            dropped.push(format!(
                                "{who}: field {slug} pointed at entity {s}, which isn't imported \
                                 and isn't in this project; set to null"
                            ));
                            None
                        }
                    }
                };
                match v {
                    Value::String(s) => {
                        let s = s.clone();
                        *v = one(&s).unwrap_or(Value::Null);
                    }
                    Value::Array(items) => {
                        let mut out = Vec::new();
                        for it in items.iter() {
                            match it {
                                Value::String(s) => out.extend(one(s)),
                                other => out.push(other.clone()),
                            }
                        }
                        *items = out;
                    }
                    _ => {}
                }
            }
            FieldType::Path { .. } => match v {
                Value::String(s) => assets.push(s.clone()),
                Value::Array(items) => {
                    assets.extend(items.iter().filter_map(|x| x.as_str().map(String::from)))
                }
                _ => {}
            },
            _ => {}
        }
    }
}

/// Point a level's object groups at the remapped ids of their members
/// (`old_entities`: the source ids of the level's entities; every other
/// member is an annotation). A member left out of the import (a dropped
/// connector) leaves its group, and a group left empty goes (reported).
fn remap_groups(
    meta: &mut Level,
    content: &LevelContent,
    old_entities: &BTreeSet<Id>,
    refs: &Refs<'_>,
    who: &str,
    dropped: &mut Vec<String>,
) {
    if meta.groups.is_empty() {
        return;
    }
    let here: BTreeSet<&Id> = content
        .entities
        .iter()
        .flat_map(|el| el.entities.iter().map(|e| &e.id))
        .chain(content.notes.iter().map(|n| &n.id))
        .collect();
    for g in &mut meta.groups {
        g.members = std::mem::take(&mut g.members)
            .into_iter()
            .filter_map(|m| {
                let map = if old_entities.contains(&m) {
                    refs.emap
                } else {
                    refs.nmap
                };
                let to = map.get(&m).cloned().unwrap_or(m);
                here.contains(&to).then_some(to)
            })
            .collect();
    }
    meta.groups.retain(|g| {
        let keep = !g.members.is_empty();
        if !keep {
            dropped.push(format!("{who}: group {} has no member left; dropped", g.id));
        }
        keep
    });
}

/// Rewrite an annotation's links and connector ends. False: a connector
/// end resolves to nothing here, so the note is left out (reported).
fn remap_note(n: &mut Annotation, refs: &Refs<'_>, who: &str, dropped: &mut Vec<String>) -> bool {
    let links = std::mem::take(&mut n.links);
    for link in links {
        match refs.link(&link) {
            Some(l) => n.links.push(l),
            None => dropped.push(format!("{who}: link {link} doesn't resolve here; dropped")),
        }
    }
    if let Shape::Connector { from, to, .. } = &mut n.shape {
        for end in [from, to] {
            let ok = match end {
                Endpoint::Level(s) => refs.level(s.as_str()),
                Endpoint::Entity(id) => match refs.entity(id) {
                    Some(new) => {
                        *id = new;
                        true
                    }
                    None => false,
                },
            };
            if !ok {
                dropped.push(format!(
                    "{who}: a connector to {}, which isn't imported and isn't in this project; \
                     the connector was left out",
                    end.link()
                ));
                return false;
            }
        }
    }
    true
}

/// The default world slug a `new_world` import of `src` makes.
pub fn default_world(src: &Project) -> Slug {
    slugify(&src.name).unwrap_or_else(|| crate::id::slug("imported"))
}
