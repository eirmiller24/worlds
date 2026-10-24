//! Commands: the one way to change a [`Project`].
//!
//! The editor, the JSON-RPC server and the MCP server all build a
//! [`Command`] and call [`Project::apply`]. Applying returns an [`Applied`]:
//! the exact inverse (apply it to get back an `==` project) and the list of
//! [`Change`]s, which say what to re-render, what to rebake, which event to
//! send and which file got dirty. [`History`] keeps undo and redo stacks of
//! inverses on top of that.
//!
//! Every command is plain serde data with a JSON Schema, tagged by `op`:
//!
//! ```json
//! { "op": "paint_cells", "level": "crevasse", "layer": "collision",
//!   "cells": [[3, 1, "solid"], [4, 1, null]] }
//! ```
//!
//! `docs/commands.md` lists every command with an example, its inverse and
//! its changes.
//!
//! Rules every command follows:
//!
//! - **Atomic.** A command that fails changes nothing. A [`Command::Batch`]
//!   that fails part way rolls back what it already applied. Inverses always
//!   apply to the state they were made from; should one still be refused,
//!   the rollback applies the rest and returns [`CommandError::Rollback`].
//! - **Exact inverse.** `apply(c)` then `apply(inverse)` gives back an equal
//!   project, including whether an empty layer file exists.
//! - **No-ops are free.** A command that changes nothing (painting a cell
//!   its current value) emits no changes, dirties nothing, and its inverse
//!   is an empty batch. [`History`] doesn't record it.
//! - **Structural checks only.** Commands refuse what they can't apply or
//!   that would break the model's own invariants: unknown level, layer,
//!   value, entity type, field or id; a layer of the wrong kind; cells
//!   outside the layer; duplicate slugs and ids. Everything else (field value
//!   types, entity limits, link targets) is left to [`crate::validate`], so
//!   a half-finished edit is never blocked.
//! - **Empty containers go away.** A layer whose content a command empties
//!   (an all-empty grid, no tiles, no entities) is dropped, so no empty file
//!   is written. The `keep_empty` flag on [`Command::PaintCells`],
//!   [`Command::SetTiles`] and [`Command::RemoveEntity`] turns that off;
//!   inverses set it when the empty container existed before, which is what
//!   keeps them exact.

mod apply;
mod error_serde;
mod history;
pub mod layer_ops;

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize};

use crate::assembly::Assembly;
use crate::format::IntGridFile;
use crate::id::{Id, Slug};
use crate::model::{
    Annotation, Color, EntitiesLayer, EntityInstance, GuideGroup, Level, Reference, TilePlacement,
};
use crate::model::{CellMatch, RuleEntry, Stamp, Template};
use crate::model::{EntityDef, EnumDef, FieldDef, LayerDef, RuleGroup, RuleSet, TilesetDef};
use crate::model::{LayerState, TilesLayer};

pub use history::{Entry, History, Origin};

/// One edit. See the module docs and `docs/commands.md`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    // ---- grid (int_grid layers) ----
    /// Set cells to values. Duplicate cells: the last one wins. On a
    /// derived layer the cells are its overrides (see [`GridValue`]).
    PaintCells {
        level: Slug,
        layer: Slug,
        /// `[x, y, value]`; value `null` is empty (on a derived layer: no
        /// override), `"!"` suppresses (derived layers only).
        cells: Vec<CellValue>,
        /// Keep the layer's (empty) content if this leaves every cell empty.
        #[serde(default, skip_serializing_if = "is_false")]
        keep_empty: bool,
    },
    /// Set every cell of a rectangle to one value.
    FillRect {
        level: Slug,
        layer: Slug,
        rect: Rect,
        #[serde(default)]
        value: GridValue,
    },
    /// Set the 4-connected region of cells holding the same value as `at`
    /// (on a derived layer: the same stored override state).
    FloodFill {
        level: Slug,
        layer: Slug,
        at: [u32; 2],
        #[serde(default)]
        value: GridValue,
    },
    /// Every cell holding `from` gets `to`, across the whole layer.
    ReplaceValue {
        level: Slug,
        layer: Slug,
        #[serde(default)]
        from: GridValue,
        #[serde(default)]
        to: GridValue,
    },

    // ---- tiles layers ----
    /// Replace the stack of tiles at each cell named in `tiles` (several
    /// placements on one cell stack in list order), and empty the cells in
    /// `clear`.
    SetTiles {
        level: Slug,
        layer: Slug,
        tiles: Vec<TilePlacement>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        clear: Vec<[u32; 2]>,
        /// Keep the layer's (empty) content if this removes every tile.
        #[serde(default, skip_serializing_if = "is_false")]
        keep_empty: bool,
    },
    /// Remove every tile whose cell is in `rect`.
    ClearTiles {
        level: Slug,
        layer: Slug,
        rect: Rect,
    },

    // ---- levels ----
    /// Add a level. `level` is the `level.json` part; layer content, entities
    /// and notes go in `content`.
    CreateLevel {
        world: Slug,
        level: Level,
        #[serde(default, skip_serializing_if = "LevelContent::is_empty")]
        content: LevelContent,
        /// Put the level back exactly as given: a grid whose size doesn't
        /// match the level, and ids another level also uses, are accepted
        /// (the project held them before; `validate` reports them). Inverses
        /// set it; edits leave it off.
        #[serde(default, skip_serializing_if = "is_false")]
        restore: bool,
    },
    DeleteLevel {
        world: Slug,
        level: Slug,
    },
    /// Replace a level (found by `level.slug`) wholesale: metadata and all
    /// content. The inverse of `resize_level`.
    ReplaceLevel {
        world: Slug,
        level: Level,
        #[serde(default, skip_serializing_if = "LevelContent::is_empty")]
        content: LevelContent,
        /// As for `CreateLevel`. Inverses set it.
        #[serde(default, skip_serializing_if = "is_false")]
        restore: bool,
    },
    /// Set the top-left corner in world pixels.
    MoveLevel {
        world: Slug,
        level: Slug,
        world_px: [i64; 2],
    },
    /// Change the size in cells of the project grid. `anchor` says what stays
    /// put in the world, per axis: `-1` the left/top edge, `0` the center,
    /// `1` the right/bottom edge. Content keeps its world position: grids and
    /// tiles are cropped or extended, entities and notes move with the
    /// content and are kept even when they end up outside.
    ResizeLevel {
        world: Slug,
        level: Slug,
        size: [u32; 2],
        #[serde(default)]
        anchor: [i8; 2],
    },
    /// Change the slug (directory and Godot room name) and display name.
    /// `level:<slug>` note links are rewritten.
    RenameLevel {
        world: Slug,
        level: Slug,
        slug: Slug,
        name: String,
    },
    /// Set a level field (one of `project.json` `level_fields`). Leave out
    /// `value` to remove the stored value; `null` stores null.
    SetLevelField {
        world: Slug,
        level: Slug,
        field: Slug,
        #[serde(
            default,
            deserialize_with = "present_value",
            skip_serializing_if = "Option::is_none"
        )]
        #[schemars(with = "serde_json::Value")]
        value: Option<serde_json::Value>,
        /// Store `value` even though no field definition has that key.
        /// Only the inverse of removing such an unknown key sets it, so undo
        /// can put the value back; edits leave it off.
        #[serde(default, skip_serializing_if = "is_false")]
        allow_unknown: bool,
    },
    SetLevelDepth {
        world: Slug,
        level: Slug,
        depth: i32,
    },
    SetLevelBg {
        world: Slug,
        level: Slug,
        bg_color: Option<Color>,
    },
    /// Replace a level's tags. `tags` is stored as given (order kept).
    SetLevelTags {
        world: Slug,
        level: Slug,
        tags: Vec<String>,
    },

    // ---- entities ----
    /// Place an entity. `index` is its position in the layer's list
    /// (default: the end).
    AddEntity {
        level: Slug,
        layer: Slug,
        entity: EntityInstance,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        index: Option<usize>,
        /// Accept a cell outside the layer. Inverses set it to put back an
        /// entity a resize left outside; edits leave it off.
        #[serde(default, skip_serializing_if = "is_false")]
        allow_outside: bool,
    },
    RemoveEntity {
        id: Id,
        /// Keep the entities layer's (empty) entry if this was its last entity.
        #[serde(default, skip_serializing_if = "is_false")]
        keep_empty: bool,
    },
    MoveEntity {
        id: Id,
        cell: [i32; 2],
        #[serde(default)]
        offset_px: [i32; 2],
        /// Accept a cell outside the layer (see `AddEntity`).
        #[serde(default, skip_serializing_if = "is_false")]
        allow_outside: bool,
    },
    /// Set the per-instance size in cells; `null` goes back to the type's size.
    ResizeEntity {
        id: Id,
        size: Option<[u32; 2]>,
    },
    /// Set a field value. Leave out `value` to remove it (the default
    /// applies); `null` stores null.
    SetEntityField {
        id: Id,
        field: Slug,
        #[serde(
            default,
            deserialize_with = "present_value",
            skip_serializing_if = "Option::is_none"
        )]
        #[schemars(with = "serde_json::Value")]
        value: Option<serde_json::Value>,
        /// Store `value` even though no field definition has that key.
        /// Only the inverse of removing such an unknown key sets it, so undo
        /// can put the value back; edits leave it off.
        #[serde(default, skip_serializing_if = "is_false")]
        allow_unknown: bool,
    },

    // ---- notes ----
    /// Add an annotation at world or level scope. `index`: position in the
    /// list (default: the end).
    AddNote {
        scope: NoteScope,
        note: Annotation,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        index: Option<usize>,
    },
    RemoveNote {
        id: Id,
    },
    /// Replace an annotation wholesale; `note.id` must be `id`.
    UpdateNote {
        id: Id,
        note: Annotation,
    },
    /// Add a guide group to `world.json`, or replace the one with this slug
    /// in place. `index`: position for a new group, or where to move an
    /// existing one (default: the end / where it is).
    SetGuideGroup {
        world: Slug,
        group: GuideGroup,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        index: Option<usize>,
    },
    /// Remove a guide group. Refused while a guide names it.
    RemoveGuideGroup {
        world: Slug,
        slug: Slug,
    },

    // ---- per-level rule state ----
    /// Set an `auto` layer's rule state in a level; `null` removes it.
    SetLayerState {
        level: Slug,
        layer: Slug,
        state: Option<LayerState>,
    },

    // ---- project ----
    SetProjectSetting {
        setting: ProjectSetting,
    },

    // ---- definitions ----
    /// Create or replace a layer definition (a `layers.json` entry;
    /// `def.slug` must be `layer`). `index` is its draw-order position
    /// (default: the end for a new layer, where it is for an existing one).
    /// `content` and `rules` only come with a new layer: they put back what
    /// `remove_layer_def` took away. Replacing an `int_grid` layer's values
    /// keeps every stored cell's value by slug.
    SetLayerDef {
        layer: Slug,
        def: LayerDef,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        index: Option<usize>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        content: Vec<LayerContent>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rules: Option<RuleSet>,
    },
    /// Remove a layer definition with every level's content for it, its
    /// per-level rule state and its rule set.
    RemoveLayerDef {
        layer: Slug,
    },
    /// Rename a layer and rewrite every reference to it.
    RenameLayer {
        from: Slug,
        to: Slug,
    },
    /// Create or replace an entity type (`entities/<slug>.json`; `def.slug`
    /// must be `entity`).
    SetEntityDef {
        entity: Slug,
        def: EntityDef,
    },
    /// Remove an entity type and every instance of it. With
    /// `keep_instances`, only the type: its instances stay (dangling, for
    /// validation to report). That is the inverse of creating a type that
    /// instances already named.
    RemoveEntityDef {
        entity: Slug,
        #[serde(default, skip_serializing_if = "is_false")]
        keep_instances: bool,
    },
    /// Rename an entity type: instances and `entity_ref` `allowed_types` follow.
    RenameEntityType {
        from: Slug,
        to: Slug,
    },
    /// Create or replace one field of an entity type (`def.slug` must be
    /// `field`). `index`: position in `fields` (default: the end for a new
    /// field, where it is for an existing one). Stored values are never
    /// touched, even when the type changes.
    SetEntityFieldDef {
        entity: Slug,
        field: Slug,
        def: FieldDef,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        index: Option<usize>,
    },
    /// Remove a field of an entity type and every value instances store for
    /// it. With `keep_values`, only the definition: stored values stay (as
    /// unknown keys, for validation to report). That is the inverse of
    /// creating a field that instances already stored a value for.
    RemoveEntityFieldDef {
        entity: Slug,
        field: Slug,
        #[serde(default, skip_serializing_if = "is_false")]
        keep_values: bool,
    },
    /// Rename a field of an entity type: the instances' stored keys follow.
    RenameEntityField {
        entity: Slug,
        from: Slug,
        to: Slug,
    },
    /// Create or replace a level field (a `project.json` `level_fields`
    /// entry), as `set_entity_field_def`.
    SetLevelFieldDef {
        field: Slug,
        def: FieldDef,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        index: Option<usize>,
    },
    /// Remove a level field and every level's stored value for it;
    /// `keep_values` as for `remove_entity_field_def`.
    RemoveLevelFieldDef {
        field: Slug,
        #[serde(default, skip_serializing_if = "is_false")]
        keep_values: bool,
    },
    /// Rename a level field: the levels' stored keys and rule groups'
    /// `requires_level_field` follow.
    RenameLevelField {
        from: Slug,
        to: Slug,
    },
    /// Create or replace an enum (an `enums.json` entry; `def.slug` must be
    /// `enum`). `index`: position in the list (default: the end for a new
    /// enum, where it is for an existing one).
    SetEnumDef {
        #[serde(rename = "enum")]
        enum_: Slug,
        def: EnumDef,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        index: Option<usize>,
    },
    /// Remove an enum. Fields that use it are left for validation to report.
    RemoveEnumDef {
        #[serde(rename = "enum")]
        enum_: Slug,
    },
    /// Rename an enum: `enum` field definitions follow.
    RenameEnum {
        from: Slug,
        to: Slug,
    },
    /// Rename one value of an enum: stored values, defaults and rule group
    /// requirements follow.
    RenameEnumValue {
        #[serde(rename = "enum")]
        enum_: Slug,
        from: Slug,
        to: Slug,
    },
    /// Create or replace a tileset (a `tilesets.json` entry; `def.slug` must
    /// be `tileset`). `index` as for `set_enum_def`.
    SetTilesetDef {
        tileset: Slug,
        def: TilesetDef,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        index: Option<usize>,
    },
    /// Remove a tileset nothing uses (refused while anything does). With
    /// `keep_uses`, it is removed even while used and the uses stay
    /// (dangling, for validation to report). That is the inverse of
    /// creating a tileset that something already named.
    RemoveTilesetDef {
        tileset: Slug,
        #[serde(default, skip_serializing_if = "is_false")]
        keep_uses: bool,
    },
    /// Rename a tileset and rewrite every reference to it.
    RenameTileset {
        from: Slug,
        to: Slug,
    },
    /// Create or replace a rule group in `rules/<layer>.json` (`def.slug`
    /// must be `group`). `index`: position in evaluation order (default:
    /// the end for a new group, where it is for an existing one).
    SetRuleGroup {
        layer: Slug,
        group: Slug,
        def: RuleGroup,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        index: Option<usize>,
    },
    /// Remove a rule group. A rule set left with no groups and no stamps
    /// goes away unless `keep_empty`.
    RemoveRuleGroup {
        layer: Slug,
        group: Slug,
        #[serde(default, skip_serializing_if = "is_false")]
        keep_empty: bool,
    },
    /// Create or replace one entry (a rule, or a `{ "use": ... }` template
    /// reference) of a rule group; `def.slug` (a reference's slug, default
    /// its template name) must be `rule`. A new entry goes at `index`
    /// (default: the end); an existing one in that group is replaced in
    /// place, or moved to `index` if given. A slug that another group holds
    /// is refused (use `move_rule`).
    SetRule {
        layer: Slug,
        group: Slug,
        rule: Slug,
        def: RuleEntry,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        index: Option<usize>,
    },
    /// Remove a rule or template reference, found by slug across the
    /// layer's groups. A rule a reference expands to (`<ref>__<rule>`) can't
    /// be removed this way: put it in the reference's `skip`.
    RemoveRule {
        layer: Slug,
        rule: Slug,
    },
    /// Move a rule or reference to position `index` of `group` (counted
    /// after taking it out of its old place).
    MoveRule {
        layer: Slug,
        rule: Slug,
        group: Slug,
        index: usize,
    },
    /// Create or replace a named pattern, legend or template in
    /// `rules/macros.json` (made if missing). `def` is the entry as in the
    /// file: rows for a pattern, a glyph map for a legend, a template object.
    SetMacro {
        kind: MacroKind,
        name: Slug,
        def: MacroDef,
    },
    /// Remove a named pattern, legend or template. Refused while a rule, a
    /// reference or a template refers to it, unless `keep_uses` (the
    /// inverse of defining a name already referred to). A macros file left
    /// empty goes away unless `keep_empty`.
    RemoveMacro {
        kind: MacroKind,
        name: Slug,
        #[serde(default, skip_serializing_if = "is_false")]
        keep_uses: bool,
        #[serde(default, skip_serializing_if = "is_false")]
        keep_empty: bool,
    },
    /// Rename a named pattern, legend or template and rewrite every
    /// reference to it (rule files and templates).
    RenameMacro {
        kind: MacroKind,
        from: Slug,
        to: Slug,
    },
    /// Create or replace a stamp of `rules/<layer>.json` (`def.slug` must be
    /// `stamp`). `index` as for `set_rule_group`. A layer without a rule set
    /// gets one.
    SetStamp {
        layer: Slug,
        stamp: Slug,
        def: Stamp,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        index: Option<usize>,
    },
    /// Remove a stamp. Refused while an `out.stamp` of the layer's rules (or
    /// of a template it references) names it, unless `keep_uses`. A rule
    /// set left with no groups and no stamps goes away unless `keep_empty`.
    RemoveStamp {
        layer: Slug,
        stamp: Slug,
        #[serde(default, skip_serializing_if = "is_false")]
        keep_uses: bool,
        #[serde(default, skip_serializing_if = "is_false")]
        keep_empty: bool,
    },

    /// Apply several commands as one. All or nothing: if one fails, the ones
    /// before it are undone and the error names the failing index.
    Batch {
        #[serde(default, skip_serializing_if = "String::is_empty")]
        label: String,
        commands: Vec<Command>,
    },

    // ---- levels (continued) ----
    /// Copy a level under a new slug: `level.json` (fields, tags, rule state
    /// and tileset substitutions, depth, background), every layer's content,
    /// entities and notes. Entities and notes get new ids and the copy's
    /// references between them (`entity_ref` fields, level fields, note
    /// links) follow; references to anything outside the level are kept.
    /// `world` defaults to the level's; `world_px` to just right of the
    /// original (the first spot there no other level of that world covers);
    /// `name` to the original's with " copy".
    DuplicateLevel {
        level: Slug,
        slug: Slug,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        world: Option<Slug>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        world_px: Option<[i64; 2]>,
    },

    // ---- worlds ----
    /// Add an empty world (`worlds/<slug>/world.json`), last in the display
    /// order.
    CreateWorld {
        slug: Slug,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    /// Change a world's slug (its directory) and/or display name. Leave a
    /// key out to keep it; `name: ""` removes the name.
    RenameWorld {
        world: Slug,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        slug: Option<Slug>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    /// Remove a world with its levels, notes and guide groups. Refused
    /// unless the world is empty or `confirm` is set, and for the last world.
    RemoveWorld {
        world: Slug,
        #[serde(default, skip_serializing_if = "is_false")]
        confirm: bool,
    },
    /// Set the display order (`project.json` `worlds`): every world once.
    /// `[]` drops the list (slug order). `restore` takes the list as given
    /// (unknown or missing slugs); inverses set it.
    ReorderWorlds {
        order: Vec<Slug>,
        #[serde(default, skip_serializing_if = "is_false")]
        restore: bool,
    },
    /// Move a level to another world, keeping its slug, id and content;
    /// `world_px` places it there (default: where it was).
    MoveLevelToWorld {
        level: Slug,
        world: Slug,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        world_px: Option<[i64; 2]>,
    },

    // ---- layer operations ----
    /// Copy a layer: its definition (placed right above it in draw order),
    /// its content in every level, its per-level rule state and, for an
    /// `auto` layer, its rule set, all under the new slug. Copied entities
    /// get ids derived from the original id and `slug`. The inverse is
    /// `remove_layer_def` of the copy.
    DuplicateLayer {
        layer: Slug,
        slug: Slug,
    },

    // ---- reference images ----
    /// Add a reference image at world or level scope. `index`: position in
    /// the list, bottom first (default: the end, drawn on top).
    AddReference {
        scope: NoteScope,
        reference: Reference,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        index: Option<usize>,
    },
    /// Replace a reference image wholesale; `reference.id` must be `id`.
    UpdateReference {
        id: Id,
        reference: Reference,
    },
    RemoveReference {
        id: Id,
    },

    // ---- assemblies ----
    /// Create or replace an assembly (`assemblies/<slug>.json`, named by
    /// `assembly.slug`).
    SetAssembly {
        assembly: Assembly,
    },
    /// Remove an assembly. Copies placed from it stay: they never were
    /// linked to it.
    RemoveAssembly {
        slug: Slug,
    },
    /// Rename an assembly (its file follows).
    RenameAssembly {
        slug: Slug,
        new_slug: Slug,
    },
    // ---- level shape ----
    /// Replace a level's `void` (rects of project-grid cells cut out of its
    /// box; `[]` makes it rectangular again). Grid cells, derived overrides
    /// and tiles that fall in the new void are cleared; the inverse puts the
    /// old void and that content back. Refused when an entity's cell would
    /// be void, a rect is empty or reaches outside the box, or no cell would
    /// be left (unless `restore`).
    SetLevelVoid {
        level: Slug,
        void: Vec<Rect>,
        /// Accept the rects as given (what the project held before;
        /// `validate` reports them). Inverses set it; edits leave it off.
        #[serde(default, skip_serializing_if = "is_false")]
        restore: bool,
    },
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// A key that is present is `Some`, even when it is `null`.
fn present_value<'de, D: Deserializer<'de>>(d: D) -> Result<Option<serde_json::Value>, D::Error> {
    serde_json::Value::deserialize(d).map(Some)
}

impl Command {
    /// The command that does nothing: an empty batch.
    pub fn noop() -> Command {
        Command::Batch {
            label: String::new(),
            commands: Vec::new(),
        }
    }

    /// True for an empty batch (or a batch of empty batches).
    pub fn is_noop(&self) -> bool {
        match self {
            Command::Batch { commands, .. } => commands.iter().all(Command::is_noop),
            _ => false,
        }
    }

    /// A short human label for undo menus ("Paint 3 cells").
    pub fn label(&self) -> String {
        use Command::*;
        let plural = |n: usize, what: &str| {
            if n == 1 {
                format!("1 {what}")
            } else {
                format!("{n} {what}s")
            }
        };
        match self {
            PaintCells { cells, layer, .. } => {
                format!("Paint {} on {layer}", plural(cells.len(), "cell"))
            }
            FillRect { layer, .. } => format!("Fill rectangle on {layer}"),
            FloodFill { layer, .. } => format!("Flood fill on {layer}"),
            ReplaceValue {
                layer, from, to, ..
            } => format!(
                "Replace {} with {} on {layer}",
                value_name(from),
                value_name(to)
            ),
            SetTiles { tiles, layer, .. } => {
                format!("Place {} on {layer}", plural(tiles.len(), "tile"))
            }
            ClearTiles { layer, .. } => format!("Clear tiles on {layer}"),
            CreateLevel { level, .. } => format!("Create level {}", level.slug),
            DeleteLevel { level, .. } => format!("Delete level {level}"),
            ReplaceLevel { level, .. } => format!("Replace level {}", level.slug),
            MoveLevel { level, .. } => format!("Move level {level}"),
            ResizeLevel { level, .. } => format!("Resize level {level}"),
            RenameLevel { level, slug, .. } => format!("Rename level {level} to {slug}"),
            SetLevelField { level, field, .. } => format!("Set {field} on {level}"),
            SetLevelDepth { level, .. } => format!("Set depth of {level}"),
            SetLevelBg { level, .. } => format!("Set background of {level}"),
            SetLevelTags { level, .. } => format!("Tag level {level}"),
            AddEntity { entity, .. } => format!("Place {}", entity.ty),
            RemoveEntity { id, .. } => format!("Remove entity {id}"),
            MoveEntity { id, .. } => format!("Move entity {id}"),
            ResizeEntity { id, .. } => format!("Resize entity {id}"),
            SetEntityField { id, field, .. } => format!("Set {field} on {id}"),
            AddNote { note, .. } => format!("Add {}", note.shape.name()),
            RemoveNote { id } => format!("Remove note {id}"),
            UpdateNote { id, .. } => format!("Edit note {id}"),
            SetGuideGroup { group, .. } => format!("Set guide group {}", group.slug),
            RemoveGuideGroup { slug, .. } => format!("Remove guide group {slug}"),
            SetLayerState { level, layer, .. } => format!("Set {layer} rule state on {level}"),
            SetProjectSetting { setting } => format!("Set project {}", setting.key().name()),
            SetLayerDef { layer, .. } => format!("Set layer {layer}"),
            RemoveLayerDef { layer } => format!("Remove layer {layer}"),
            RenameLayer { from, to } => format!("Rename layer {from} to {to}"),
            SetEntityDef { entity, .. } => format!("Set entity type {entity}"),
            RemoveEntityDef { entity, .. } => format!("Remove entity type {entity}"),
            RenameEntityType { from, to } => format!("Rename entity type {from} to {to}"),
            SetEntityFieldDef { entity, field, .. } => format!("Set field {field} of {entity}"),
            RemoveEntityFieldDef { entity, field, .. } => {
                format!("Remove field {field} of {entity}")
            }
            RenameEntityField { entity, from, to } => {
                format!("Rename field {from} of {entity} to {to}")
            }
            SetLevelFieldDef { field, .. } => format!("Set level field {field}"),
            RemoveLevelFieldDef { field, .. } => format!("Remove level field {field}"),
            RenameLevelField { from, to } => format!("Rename level field {from} to {to}"),
            SetEnumDef { enum_, .. } => format!("Set enum {enum_}"),
            RemoveEnumDef { enum_ } => format!("Remove enum {enum_}"),
            RenameEnum { from, to } => format!("Rename enum {from} to {to}"),
            RenameEnumValue { enum_, from, to } => {
                format!("Rename {enum_} value {from} to {to}")
            }
            SetTilesetDef { tileset, .. } => format!("Set tileset {tileset}"),
            RemoveTilesetDef { tileset, .. } => format!("Remove tileset {tileset}"),
            RenameTileset { from, to } => format!("Rename tileset {from} to {to}"),
            SetRuleGroup { layer, group, .. } => format!("Set rule group {group} on {layer}"),
            RemoveRuleGroup { layer, group, .. } => {
                format!("Remove rule group {group} from {layer}")
            }
            SetRule { layer, rule, .. } => format!("Set rule {rule} on {layer}"),
            RemoveRule { layer, rule } => format!("Remove rule {rule} from {layer}"),
            MoveRule {
                layer, rule, group, ..
            } => format!("Move rule {rule} to {group} on {layer}"),
            SetMacro { kind, name, .. } => format!("Set {kind} {name}"),
            RemoveMacro { kind, name, .. } => format!("Remove {kind} {name}"),
            RenameMacro { kind, from, to } => format!("Rename {kind} {from} to {to}"),
            SetStamp { layer, stamp, .. } => format!("Set stamp {stamp} on {layer}"),
            RemoveStamp { layer, stamp, .. } => format!("Remove stamp {stamp} from {layer}"),
            SetLevelVoid { level, void, .. } if void.is_empty() => {
                format!("Clear the void of level {level}")
            }
            SetLevelVoid { level, .. } => format!("Shape level {level}"),
            Batch { label, commands } if label.is_empty() => {
                format!("{} edits", commands.len())
            }
            Batch { label, .. } => label.clone(),
            DuplicateLevel { level, slug, .. } => format!("Duplicate level {level} as {slug}"),
            CreateWorld { slug, .. } => format!("Create world {slug}"),
            RenameWorld {
                world,
                slug: Some(slug),
                ..
            } if slug != world => format!("Rename world {world} to {slug}"),
            RenameWorld { world, .. } => format!("Rename world {world}"),
            RemoveWorld { world, .. } => format!("Delete world {world}"),
            ReorderWorlds { .. } => "Reorder worlds".into(),
            MoveLevelToWorld { level, world, .. } => {
                format!("Move level {level} to world {world}")
            }
            DuplicateLayer { layer, slug } => format!("Duplicate layer {layer} as {slug}"),
            AddReference { .. } => "Add reference image".into(),
            UpdateReference { id, .. } => format!("Edit reference image {id}"),
            RemoveReference { id } => format!("Remove reference image {id}"),
            SetAssembly { assembly } => format!("Set assembly {}", assembly.slug),
            RemoveAssembly { slug } => format!("Remove assembly {slug}"),
            RenameAssembly { slug, new_slug } => format!("Rename assembly {slug} to {new_slug}"),
        }
    }
}

fn value_name(v: &GridValue) -> String {
    match v {
        GridValue::Clear => "empty".into(),
        GridValue::Value(s) => s.to_string(),
        GridValue::Suppress => "suppressed".into(),
    }
}

/// A grid cell value in a command: a value slug, `null` or `"!"`.
///
/// On an ordinary `int_grid` layer a slug is that value and `null` is empty;
/// `"!"` is refused. On a derived layer the grid commands work on the
/// stored overrides: a slug sets an override, `null` removes it (back to
/// the rules), `"!"` suppresses (overrides to empty).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub enum GridValue {
    /// `null`: empty (on a derived layer, no override).
    #[default]
    Clear,
    /// A value slug.
    Value(Slug),
    /// `"!"`: suppress (derived layers only).
    Suppress,
}

impl GridValue {
    /// The value slug, if this is one.
    pub fn slug(&self) -> Option<&Slug> {
        match self {
            GridValue::Value(s) => Some(s),
            _ => None,
        }
    }
}

impl From<Option<Slug>> for GridValue {
    fn from(v: Option<Slug>) -> Self {
        match v {
            Some(s) => GridValue::Value(s),
            None => GridValue::Clear,
        }
    }
}

impl From<Slug> for GridValue {
    fn from(s: Slug) -> Self {
        GridValue::Value(s)
    }
}

impl fmt::Display for GridValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GridValue::Clear => f.write_str("null"),
            GridValue::Value(s) => write!(f, "{s}"),
            GridValue::Suppress => f.write_str("!"),
        }
    }
}

impl Serialize for GridValue {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            GridValue::Clear => s.serialize_none(),
            GridValue::Value(v) => v.serialize(s),
            GridValue::Suppress => s.serialize_str("!"),
        }
    }
}

impl<'de> Deserialize<'de> for GridValue {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        match Option::<String>::deserialize(d)? {
            None => Ok(GridValue::Clear),
            Some(s) if s == "!" => Ok(GridValue::Suppress),
            Some(s) => Slug::new(s).map(GridValue::Value).map_err(|e| {
                D::Error::custom(format!(
                    "{e}; a grid value is a value slug, null, or \"!\" (suppress, derived \
                     layers only)"
                ))
            }),
        }
    }
}

impl JsonSchema for GridValue {
    fn schema_name() -> Cow<'static, str> {
        "GridValue".into()
    }
    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        let slug = generator.subschema_for::<Slug>();
        json_schema!({
            "description": "A grid cell value: a value slug, null (empty; on a derived layer: no override, back to the rules) or \"!\" (suppress: derived layers only).",
            "anyOf": [slug, { "type": "null" }, { "const": "!" }]
        })
    }
}

/// One cell write: `[x, y, value]` on the wire (see [`GridValue`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "(u32, u32, GridValue)", into = "(u32, u32, GridValue)")]
pub struct CellValue {
    /// `[x, y]` cell of the layer's grid.
    pub cell: [u32; 2],
    pub value: GridValue,
}

impl CellValue {
    pub fn new(x: u32, y: u32, value: impl Into<GridValue>) -> Self {
        CellValue {
            cell: [x, y],
            value: value.into(),
        }
    }
}

impl From<(u32, u32, GridValue)> for CellValue {
    fn from((x, y, value): (u32, u32, GridValue)) -> Self {
        CellValue::new(x, y, value)
    }
}

impl From<CellValue> for (u32, u32, GridValue) {
    fn from(c: CellValue) -> Self {
        (c.cell[0], c.cell[1], c.value)
    }
}

impl JsonSchema for CellValue {
    fn schema_name() -> Cow<'static, str> {
        "CellValue".into()
    }
    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        let value = generator.subschema_for::<GridValue>();
        json_schema!({
            "description": "[x, y, value]: a cell of the layer's grid and a grid value (a value slug, null, or \"!\" on a derived layer).",
            "type": "array",
            "prefixItems": [
                { "type": "integer", "minimum": 0 },
                { "type": "integer", "minimum": 0 },
                value
            ],
            "minItems": 3,
            "maxItems": 3
        })
    }
    fn inline_schema() -> bool {
        false
    }
}

/// A rectangle of cells: `[x, y, width, height]` on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(from = "[u32; 4]", into = "[u32; 4]")]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    pub fn new(x: u32, y: u32, w: u32, h: u32) -> Self {
        Rect { x, y, w, h }
    }

    /// The 1x1 rect at a cell.
    pub fn cell(c: [u32; 2]) -> Self {
        Rect::new(c[0], c[1], 1, 1)
    }

    /// The whole of a `[w, h]` layer.
    pub fn of_size(size: [u32; 2]) -> Self {
        Rect::new(0, 0, size[0], size[1])
    }

    pub fn is_empty(&self) -> bool {
        self.w == 0 || self.h == 0
    }

    /// Exclusive right and bottom edges.
    pub fn end(&self) -> [u32; 2] {
        [self.x + self.w, self.y + self.h]
    }

    pub fn contains(&self, c: [u32; 2]) -> bool {
        c[0] >= self.x && c[1] >= self.y && c[0] < self.x + self.w && c[1] < self.y + self.h
    }

    /// The smallest rect holding both (an empty rect holds nothing).
    pub fn union(self, other: Rect) -> Rect {
        if self.is_empty() {
            return other;
        }
        if other.is_empty() {
            return self;
        }
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let [ex, ey] = self.end();
        let [ox, oy] = other.end();
        Rect::new(x, y, ex.max(ox) - x, ey.max(oy) - y)
    }

    /// Grow to hold a cell.
    pub fn include(self, c: [u32; 2]) -> Rect {
        self.union(Rect::cell(c))
    }

    /// The bounding rect of some cells (empty if none).
    pub fn bounding(cells: impl IntoIterator<Item = [u32; 2]>) -> Rect {
        cells
            .into_iter()
            .fold(Rect::new(0, 0, 0, 0), |r, c| r.include(c))
    }
}

impl From<[u32; 4]> for Rect {
    fn from([x, y, w, h]: [u32; 4]) -> Self {
        Rect { x, y, w, h }
    }
}

impl From<Rect> for [u32; 4] {
    fn from(r: Rect) -> Self {
        [r.x, r.y, r.w, r.h]
    }
}

impl JsonSchema for Rect {
    fn schema_name() -> Cow<'static, str> {
        "Rect".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "description": "[x, y, width, height] in cells.",
            "type": "array",
            "items": { "type": "integer", "minimum": 0 },
            "minItems": 4,
            "maxItems": 4
        })
    }
}

/// Where an annotation lives: `{ "world": "main" }` or `{ "level": "crevasse" }`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NoteScope {
    /// In `worlds/<world>/world.json`, world pixel coordinates.
    World(Slug),
    /// In the level's `notes.json`, level pixel coordinates.
    Level(Slug),
}

/// The simple project settings, `{ "key": "seed", "value": 42 }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "key", content = "value", rename_all = "snake_case")]
pub enum ProjectSetting {
    Name(String),
    BgColor(Option<Color>),
    /// Must name an `int_grid` layer.
    CollisionLayer(Option<Slug>),
    Seed(i64),
    /// The project grid in pixels (at least 1). Only while the project has
    /// no levels: their sizes and positions are in cells of it.
    GridSize(u32),
    /// Values of the collision layer that block movement (`None`: left out,
    /// see the migration rule in docs/format.md). Values that aren't the
    /// collision layer's are left to validation.
    BlockingValues(Option<Vec<Slug>>),
    /// Size in cells of a level made with the editor's New Level command
    /// (both at least 1; `None`: left out, the most common level size).
    DefaultLevelSize(Option<[u32; 2]>),
    /// Whether the editor writes `export/` on every save.
    ExportOnSave(bool),
}

impl ProjectSetting {
    pub fn key(&self) -> ProjectKey {
        match self {
            ProjectSetting::Name(_) => ProjectKey::Name,
            ProjectSetting::BgColor(_) => ProjectKey::BgColor,
            ProjectSetting::CollisionLayer(_) => ProjectKey::CollisionLayer,
            ProjectSetting::Seed(_) => ProjectKey::Seed,
            ProjectSetting::GridSize(_) => ProjectKey::GridSize,
            ProjectSetting::BlockingValues(_) => ProjectKey::BlockingValues,
            ProjectSetting::DefaultLevelSize(_) => ProjectKey::DefaultLevelSize,
            ProjectSetting::ExportOnSave(_) => ProjectKey::ExportOnSave,
        }
    }
}

/// Which project setting a [`Change::Project`] touched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProjectKey {
    Name,
    BgColor,
    CollisionLayer,
    /// Changes every bake.
    Seed,
    /// `level_fields` (only definition commands touch it: the level field
    /// commands, and renames that rewrite a field's type or default).
    LevelFields,
    /// The project grid (changes every layer's cells; only allowed with no
    /// levels).
    GridSize,
    /// Matters only to the export.
    BlockingValues,
    /// Matters only to the editor's New Level command.
    DefaultLevelSize,
    /// Matters only to saving.
    ExportOnSave,
    /// `worlds`: the display order of the worlds.
    Worlds,
}

impl ProjectKey {
    pub fn name(self) -> &'static str {
        match self {
            ProjectKey::Name => "name",
            ProjectKey::BgColor => "bg_color",
            ProjectKey::CollisionLayer => "collision_layer",
            ProjectKey::Seed => "seed",
            ProjectKey::LevelFields => "level_fields",
            ProjectKey::GridSize => "grid_size",
            ProjectKey::BlockingValues => "blocking_values",
            ProjectKey::DefaultLevelSize => "default_level_size",
            ProjectKey::ExportOnSave => "export_on_save",
            ProjectKey::Worlds => "worlds",
        }
    }
}

/// One level's content for one layer, in file form: what
/// [`Command::RemoveLayerDef`] takes away and its inverse (a
/// [`Command::SetLayerDef`] with `content`) puts back. Give the key that
/// fits the layer's kind; `entities` and `state` are taken for any kind.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LayerContent {
    pub level: Slug,
    /// int_grid: the rows of `<layer>.json` (the layer's glyphs, `.` empty;
    /// for a derived layer its overrides, `.` none, `!` suppressed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows: Option<Vec<String>>,
    /// tiles: the placements of `<layer>.json`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tiles: Option<Vec<TilePlacement>>,
    /// The layer's instances in `entities.json` (possibly an empty entry).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entities: Option<Vec<EntityInstance>>,
    /// The layer's rule state in `level.json` `layers`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<LayerState>,
}

impl LayerContent {
    fn is_empty(&self) -> bool {
        self.rows.is_none()
            && self.tiles.is_none()
            && self.entities.is_none()
            && self.state.is_none()
    }
}

/// A kind of definition, for [`Change::Def`] and [`Change::DefRenamed`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DefKind {
    /// An entry of `layers.json`.
    Layer,
    /// `entities/<slug>.json`.
    Entity,
    /// An entry of `enums.json`.
    Enum,
    /// An entry of `tilesets.json`.
    Tileset,
    /// `rules/<layer>.json` (the slug is the layer's).
    Rules,
    /// `rules/macros.json` (the slug is always `macros`).
    Macros,
    /// `assemblies/<slug>.json`.
    Assembly,
}

impl DefKind {
    pub fn name(self) -> &'static str {
        match self {
            DefKind::Layer => "layer",
            DefKind::Entity => "entity type",
            DefKind::Enum => "enum",
            DefKind::Tileset => "tileset",
            DefKind::Rules => "rule set",
            DefKind::Macros => "macro",
            DefKind::Assembly => "assembly",
        }
    }

    /// The project-relative file holding the definition `slug` of this kind.
    pub fn file(self, slug: &Slug) -> PathBuf {
        match self {
            DefKind::Layer => PathBuf::from("layers.json"),
            DefKind::Entity => PathBuf::from("entities").join(format!("{slug}.json")),
            DefKind::Enum => PathBuf::from("enums.json"),
            DefKind::Tileset => PathBuf::from("tilesets.json"),
            DefKind::Rules => PathBuf::from("rules").join(format!("{slug}.json")),
            DefKind::Macros => PathBuf::from("rules").join("macros.json"),
            DefKind::Assembly => PathBuf::from("assemblies").join(format!("{slug}.json")),
        }
    }
}

impl fmt::Display for DefKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Which section of `rules/macros.json` a macro command names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MacroKind {
    /// `patterns`: named rows, used as `match.rows` / `require.rows`.
    Pattern,
    /// `legends`: named legends, used as a pattern's or template's `legend`.
    Legend,
    /// `templates`: used by `{ "use": ... }` entries.
    Template,
}

impl MacroKind {
    pub fn name(self) -> &'static str {
        match self {
            MacroKind::Pattern => "pattern",
            MacroKind::Legend => "legend",
            MacroKind::Template => "template",
        }
    }
}

impl fmt::Display for MacroKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The body of a `set_macro`, as in `rules/macros.json`: rows (an array of
/// strings) for a pattern, a glyph map for a legend, a template object.
/// On the wire it is just that value; an object whose keys are all single
/// characters (or `{}`) is a legend, any other object a template.
#[derive(Debug, Clone, PartialEq)]
pub enum MacroDef {
    Pattern(Vec<String>),
    Legend(BTreeMap<char, CellMatch>),
    Template(Template),
}

impl MacroDef {
    /// The kind of macro this body is.
    pub fn kind(&self) -> MacroKind {
        match self {
            MacroDef::Pattern(_) => MacroKind::Pattern,
            MacroDef::Legend(_) => MacroKind::Legend,
            MacroDef::Template(_) => MacroKind::Template,
        }
    }
}

impl Serialize for MacroDef {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            MacroDef::Pattern(r) => r.serialize(s),
            MacroDef::Legend(l) => l.serialize(s),
            MacroDef::Template(t) => t.serialize(s),
        }
    }
}

impl<'de> Deserialize<'de> for MacroDef {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let v = serde_json::Value::deserialize(d)?;
        let legend = v
            .as_object()
            .is_some_and(|m| m.keys().all(|k| k.chars().count() == 1));
        if v.is_array() {
            serde_json::from_value(v)
                .map(MacroDef::Pattern)
                .map_err(|e| D::Error::custom(format!("pattern rows: {e}")))
        } else if legend {
            serde_json::from_value(v)
                .map(MacroDef::Legend)
                .map_err(|e| D::Error::custom(format!("legend: {e}")))
        } else if v.is_object() {
            serde_json::from_value(v)
                .map(MacroDef::Template)
                .map_err(|e| D::Error::custom(format!("template: {e}")))
        } else {
            Err(D::Error::custom(
                "a macro is rows (an array of strings), a legend (an object of glyph to cell \
                 test) or a template (an object with \"rules\")",
            ))
        }
    }
}

impl JsonSchema for MacroDef {
    fn schema_name() -> Cow<'static, str> {
        "MacroDef".into()
    }
    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        let legend = generator.subschema_for::<BTreeMap<char, CellMatch>>();
        let template = generator.subschema_for::<Template>();
        json_schema!({
            "description": "A pattern's rows, a legend, or a template, as in rules/macros.json.",
            "anyOf": [
                { "type": "array", "items": { "type": "string" } },
                legend,
                template
            ]
        })
    }
}

/// A level's layer content, entities and notes: everything of a level that
/// isn't in `level.json`, in file form (grids as rows).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LevelContent {
    /// One per int_grid layer with content, as in `<layer>.json` (the
    /// legend may be left out; a derived layer's entry holds its overrides,
    /// and may leave out `"overrides": true`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub int_grids: Vec<IntGridFile>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tiles: Vec<TilesLayer>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entities: Vec<EntitiesLayer>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<Annotation>,
}

impl LevelContent {
    pub fn is_empty(&self) -> bool {
        self.int_grids.is_empty()
            && self.tiles.is_empty()
            && self.entities.is_empty()
            && self.notes.is_empty()
    }
}

/// What applying a command did.
#[derive(Debug, Clone, PartialEq)]
pub struct Applied {
    /// Apply this to undo exactly.
    pub inverse: Command,
    /// What changed, in order. Empty for a no-op.
    pub changes: Vec<Change>,
}

/// Which part of `level.json` a [`Change::LevelMeta`] touched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LevelPart {
    /// `world_px`.
    Position,
    /// `size` (layer content was cropped or extended too).
    Size,
    Name,
    Depth,
    BgColor,
    /// `fields` (can switch rule groups on or off).
    Fields,
    /// `tags` (can switch rule groups with `requires_level_tag`).
    Tags,
    /// Everything (`replace_level`).
    All,
    /// `void` (the level's shape; layer content in the new void was
    /// cleared, reported as its own changes).
    Void,
}

impl LevelPart {
    /// Whether a change to this part can change an `auto` layer's bake.
    pub fn affects_bake(self) -> bool {
        matches!(
            self,
            LevelPart::Size
                | LevelPart::Fields
                | LevelPart::Tags
                | LevelPart::All
                | LevelPart::Void
        )
    }
}

/// One thing a command touched. Precise enough to re-render and rebake only
/// what changed, to send as a server event, and to know the dirty file.
/// Rects are in cells of the layer's grid and cover the cells whose value
/// or tile stack changed (a tile drawn from a cell may overhang it).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "change", rename_all = "snake_case")]
pub enum Change {
    Grid {
        world: Slug,
        level: Slug,
        layer: Slug,
        rect: Rect,
    },
    Tiles {
        world: Slug,
        level: Slug,
        layer: Slug,
        rect: Rect,
    },
    LevelMeta {
        world: Slug,
        level: Slug,
        part: LevelPart,
    },
    LevelCreated {
        world: Slug,
        level: Slug,
    },
    LevelDeleted {
        world: Slug,
        level: Slug,
    },
    LevelRenamed {
        world: Slug,
        from: Slug,
        to: Slug,
    },
    /// Added, removed, moved, resized or a field set.
    Entity {
        world: Slug,
        level: Slug,
        id: Id,
    },
    /// After a resize, this entity's cell is outside the level. It is kept;
    /// the editor should point it out (validation reports it too).
    EntityOutside {
        world: Slug,
        level: Slug,
        id: Id,
    },
    /// An entities layer's entry in `entities.json` was added, removed or
    /// emptied (a `replace_level` whose entity layers differ only in empty
    /// entries reports no `Entity`, so it reports this).
    EntityLayer {
        world: Slug,
        level: Slug,
        layer: Slug,
    },
    /// `level: None` is world scope.
    Note {
        world: Slug,
        level: Option<Slug>,
        id: Id,
    },
    LayerState {
        world: Slug,
        level: Slug,
        layer: Slug,
    },
    /// A `world.json` guide group was added, replaced, moved or removed.
    GuideGroup {
        world: Slug,
        slug: Slug,
    },
    Project {
        key: ProjectKey,
    },
    /// A definition was added, replaced or removed, or a reference inside
    /// it was rewritten. Bakes anywhere may depend on it.
    Def {
        kind: DefKind,
        slug: Slug,
    },
    /// A definition's slug changed (for entity types and rule sets, its
    /// file too). The rewritten references are reported as their own changes.
    DefRenamed {
        kind: DefKind,
        from: Slug,
        to: Slug,
    },
    /// A level's `<from>.json` layer file became `<to>.json` (its layer was
    /// renamed).
    LayerFileRenamed {
        world: Slug,
        level: Slug,
        from: Slug,
        to: Slug,
    },
    /// A world was added (its directory is dirty).
    WorldCreated {
        world: Slug,
    },
    /// A world was removed with everything in it (saving deletes its
    /// directory). Its levels are reported as `level_deleted` first.
    WorldRemoved {
        world: Slug,
    },
    /// A world's slug changed: its directory moved, levels and all.
    WorldRenamed {
        from: Slug,
        to: Slug,
    },
    /// A world's `world.json` name changed.
    WorldMeta {
        world: Slug,
    },
    /// A reference image was added, replaced or removed. `level: None` is
    /// world scope.
    Reference {
        world: Slug,
        level: Option<Slug>,
        id: Id,
    },
}

fn world_dir(world: &Slug) -> PathBuf {
    PathBuf::from("worlds").join(world.as_str())
}

fn level_dir(world: &Slug, level: &Slug) -> PathBuf {
    PathBuf::from("worlds")
        .join(world.as_str())
        .join("levels")
        .join(level.as_str())
}

impl Change {
    /// The project-relative file this change dirties. For
    /// `LevelCreated`/`Deleted`/`Renamed` it is the level directory (no
    /// extension): every file in it. A rename also dirties the old
    /// directory; see [`Change::files`].
    pub fn file(&self) -> PathBuf {
        use Change::*;
        match self {
            Grid {
                world,
                level,
                layer,
                ..
            }
            | Tiles {
                world,
                level,
                layer,
                ..
            } => level_dir(world, level).join(format!("{layer}.json")),
            LevelMeta { world, level, .. } | LayerState { world, level, .. } => {
                level_dir(world, level).join("level.json")
            }
            LevelCreated { world, level } | LevelDeleted { world, level } => {
                level_dir(world, level)
            }
            LevelRenamed { world, to, .. } => level_dir(world, to),
            Entity { world, level, .. }
            | EntityOutside { world, level, .. }
            | EntityLayer { world, level, .. } => level_dir(world, level).join("entities.json"),
            Note {
                world, level: None, ..
            } => PathBuf::from("worlds")
                .join(world.as_str())
                .join("world.json"),
            Note {
                world,
                level: Some(level),
                ..
            } => level_dir(world, level).join("notes.json"),
            GuideGroup { world, .. } => PathBuf::from("worlds")
                .join(world.as_str())
                .join("world.json"),
            Project { .. } => PathBuf::from("project.json"),
            Def { kind, slug } => kind.file(slug),
            DefRenamed { kind, to, .. } => kind.file(to),
            LayerFileRenamed {
                world, level, to, ..
            } => level_dir(world, level).join(format!("{to}.json")),
            WorldCreated { world } | WorldRemoved { world } => world_dir(world),
            WorldRenamed { to, .. } => world_dir(to),
            WorldMeta { world } => world_dir(world).join("world.json"),
            Reference {
                world, level: None, ..
            } => PathBuf::from("worlds")
                .join(world.as_str())
                .join("world.json"),
            Reference {
                world,
                level: Some(level),
                ..
            } => level_dir(world, level).join("level.json"),
        }
    }

    /// Every file or directory this change dirties: [`Change::file`], plus
    /// the old directory of a renamed level, the old file of a renamed
    /// entity type or rule set, and the old file of a renamed layer.
    pub fn files(&self) -> Vec<PathBuf> {
        match self {
            Change::LevelRenamed { world, from, to } => {
                vec![level_dir(world, from), level_dir(world, to)]
            }
            Change::WorldRenamed { from, to } => vec![world_dir(from), world_dir(to)],
            Change::DefRenamed { kind, from, to } => {
                let (a, b) = (kind.file(from), kind.file(to));
                if a == b { vec![b] } else { vec![a, b] }
            }
            Change::LayerFileRenamed {
                world,
                level,
                from,
                to,
            } => {
                let dir = level_dir(world, level);
                vec![
                    dir.join(format!("{from}.json")),
                    dir.join(format!("{to}.json")),
                ]
            }
            c => vec![c.file()],
        }
    }

    /// The level this change is about, if any (the new slug for a rename).
    pub fn level(&self) -> Option<&Slug> {
        use Change::*;
        match self {
            Grid { level, .. }
            | Tiles { level, .. }
            | LevelMeta { level, .. }
            | LevelCreated { level, .. }
            | LevelDeleted { level, .. }
            | Entity { level, .. }
            | EntityOutside { level, .. }
            | EntityLayer { level, .. }
            | LayerState { level, .. }
            | LayerFileRenamed { level, .. } => Some(level),
            LevelRenamed { to, .. } => Some(to),
            Note { level, .. } | Reference { level, .. } => level.as_ref(),
            GuideGroup { .. }
            | Project { .. }
            | Def { .. }
            | DefRenamed { .. }
            | WorldCreated { .. }
            | WorldRemoved { .. }
            | WorldRenamed { .. }
            | WorldMeta { .. } => None,
        }
    }
}

/// Cells of a line from `a` to `b` into `out` (cleared first): 8-connected
/// Bresenham, both ends included. The editor's line tool and brush strokes
/// and the live API's `grid.line` all use it, so they paint the same cells.
pub fn line_cells(a: [i32; 2], b: [i32; 2], out: &mut Vec<[i32; 2]>) {
    out.clear();
    let (mut x, mut y) = (a[0], a[1]);
    let dx = (b[0] - x).abs();
    let dy = -(b[1] - y).abs();
    let sx = if x < b[0] { 1 } else { -1 };
    let sy = if y < b[1] { 1 } else { -1 };
    let mut err = dx + dy;
    loop {
        out.push([x, y]);
        if x == b[0] && y == b[1] {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x += sx;
        }
        if e2 <= dx {
            err += dx;
            y += sy;
        }
    }
}

/// Merge a list of changes: drop exact duplicates and merge the `Grid` and
/// `Tiles` rects of one layer into their bounding rect. Level creation,
/// deletion and renaming, and definition and layer file renames, act as
/// barriers (nothing merges across them), so the order of structural
/// changes is kept.
pub fn coalesce(changes: Vec<Change>) -> Vec<Change> {
    use std::collections::HashMap;
    #[derive(PartialEq, Eq, Hash)]
    enum Key {
        Grid(Slug, Slug, bool),
        Other(Change),
    }
    let mut out: Vec<Change> = Vec::with_capacity(changes.len());
    let mut seen: HashMap<Key, usize> = HashMap::new();
    for c in changes {
        let key = match &c {
            Change::Grid { level, layer, .. } => Key::Grid(level.clone(), layer.clone(), true),
            Change::Tiles { level, layer, .. } => Key::Grid(level.clone(), layer.clone(), false),
            Change::LevelCreated { .. }
            | Change::LevelDeleted { .. }
            | Change::LevelRenamed { .. }
            | Change::DefRenamed { .. }
            | Change::LayerFileRenamed { .. }
            | Change::WorldCreated { .. }
            | Change::WorldRemoved { .. }
            | Change::WorldRenamed { .. } => {
                seen.clear();
                out.push(c);
                continue;
            }
            other => Key::Other(other.clone()),
        };
        match seen.get(&key) {
            Some(&i) => {
                if let (
                    Change::Grid { rect: a, .. } | Change::Tiles { rect: a, .. },
                    Change::Grid { rect: b, .. } | Change::Tiles { rect: b, .. },
                ) = (&mut out[i], &c)
                {
                    *a = a.union(*b);
                }
            }
            None => {
                seen.insert(key, out.len());
                out.push(c);
            }
        }
    }
    out
}

/// Why a command was refused. The project is unchanged.
///
/// Serializes as `{ "kind": <variant in snake_case>, ...fields, "message": <Display> }`
/// (see `error_serde.rs` for the field names of tuple variants).
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum CommandError {
    #[error("no world {0:?}")]
    UnknownWorld(Slug),
    #[error("no level {0:?}")]
    UnknownLevel(Slug),
    #[error("level {level:?} is not in world {world:?}")]
    LevelNotInWorld { world: Slug, level: Slug },
    #[error("no layer {0:?}")]
    UnknownLayer(Slug),
    #[error("layer {layer:?} is a {actual} layer, this needs a {expected} layer")]
    WrongLayerKind {
        layer: Slug,
        expected: &'static str,
        actual: &'static str,
    },
    #[error("{value:?} is not a value of layer {layer:?}")]
    UnknownValue { layer: Slug, value: Slug },
    #[error("cell [{x}, {y}] is outside layer {layer:?} ({w}x{h} cells)")]
    OutOfBounds {
        layer: Slug,
        x: i64,
        y: i64,
        w: u32,
        h: u32,
    },
    #[error(
        "layer {layer:?} content is {actual:?} cells but the level needs {expected:?}; fix the file or resize the level"
    )]
    GridSize {
        layer: Slug,
        actual: [u32; 2],
        expected: [u32; 2],
    },
    #[error(
        "cell [{x}, {y}] of layer {layer:?} holds value #{index}, which the layer doesn't define"
    )]
    UndefinedIndex {
        layer: Slug,
        x: u32,
        y: u32,
        index: u16,
    },
    #[error("no tileset {0:?}")]
    UnknownTileset(Slug),
    #[error("no entity type {0:?}")]
    UnknownEntityType(Slug),
    #[error("{owner} has no field {field:?}")]
    UnknownField { owner: String, field: Slug },
    /// A field slug that is taken (renaming a field to the slug of another).
    #[error("{owner} already has a field {field:?}")]
    DuplicateField { owner: String, field: Slug },
    #[error("no entity {0:?}")]
    UnknownEntity(Id),
    #[error("no note {0:?}")]
    UnknownNote(Id),
    #[error("world {world:?} has no guide group {group:?}")]
    UnknownGuideGroup { world: Slug, group: Slug },
    #[error("id {0:?} is already used")]
    DuplicateId(Id),
    #[error("a level {0:?} already exists")]
    DuplicateLevel(Slug),
    #[error("no enum {0:?}")]
    UnknownEnum(Slug),
    #[error("{value:?} is not a value of enum {enum_:?}")]
    UnknownEnumValue { enum_: Slug, value: Slug },
    #[error("rules of layer {layer:?} have no group {group:?}")]
    UnknownRuleGroup { layer: Slug, group: Slug },
    #[error("rules of layer {layer:?} have no rule or reference {rule:?}")]
    UnknownRule { layer: Slug, rule: Slug },
    #[error("rules/macros.json has no {kind} {name:?}")]
    UnknownMacro { kind: MacroKind, name: Slug },
    #[error("rules of layer {layer:?} have no stamp {stamp:?}")]
    UnknownStamp { layer: Slug, stamp: Slug },
    #[error("a {kind} {slug:?} already exists")]
    DuplicateDef { kind: DefKind, slug: Slug },
    /// A definition can't go while something uses it (a tileset, or a
    /// macro: kind `macros`, `slug` the macro's name).
    #[error(
        "{kind} {slug:?} is still used by {}; remove or rename those uses first",
        uses.join(", ")
    )]
    InUse {
        kind: DefKind,
        slug: Slug,
        /// Where it is used ("layer decor_back", "level crevasse tiles decor_back"),
        /// the first few.
        uses: Vec<String>,
    },
    /// A stamp can't go while a rule output names it. On the wire this is
    /// an `in_use` with `def_kind: "stamp"` and the `layer`.
    #[error(
        "stamp {stamp:?} of layer {layer:?} is still used by {}; remove or change those uses first",
        uses.join(", ")
    )]
    StampInUse {
        layer: Slug,
        stamp: Slug,
        uses: Vec<String>,
    },
    #[error("{0}")]
    Invalid(String),
    #[error("batch {label:?}, command {index}: {source}")]
    Batch {
        label: String,
        index: usize,
        source: Box<CommandError>,
    },
    /// A batch failed and undoing what it had applied failed too (or a
    /// nested batch's did). Every inverse that could be applied was; the
    /// project may still differ from before the batch, and every file the
    /// batch touched is marked dirty. Callers must treat the project as
    /// changed (re-read it, drop caches).
    #[error(
        "{batch}; rolling it back failed ({} inverse(s) refused{}), so the project may differ from before the batch",
        failed.len(),
        failed.first().map(|e| format!(", first: {e}")).unwrap_or_default()
    )]
    Rollback {
        batch: Box<CommandError>,
        failed: Vec<CommandError>,
    },
    #[error("a world {0:?} already exists")]
    DuplicateWorld(Slug),
    #[error("no reference image {0:?}")]
    UnknownReference(Id),
    #[error("no assembly {0:?}")]
    UnknownAssembly(Slug),
}

impl fmt::Display for NoteScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NoteScope::World(w) => write!(f, "world {w}"),
            NoteScope::Level(l) => write!(f, "level {l}"),
        }
    }
}

#[cfg(test)]
mod line_tests {
    use super::line_cells;

    #[test]
    fn line_is_connected_and_inclusive() {
        let mut v = Vec::new();
        line_cells([5, 1], [7, 1], &mut v);
        assert_eq!(v, [[5, 1], [6, 1], [7, 1]]);
        line_cells([0, 0], [3, 2], &mut v);
        assert_eq!(v, [[0, 0], [1, 1], [2, 1], [3, 2]]);
        line_cells([2, 2], [2, 2], &mut v);
        assert_eq!(v, [[2, 2]]);
    }
}
