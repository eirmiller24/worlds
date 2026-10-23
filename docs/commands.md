# Worlds commands

Every edit to a Worlds project, from the editor, the JSON-RPC server or an MCP agent, is a **command**: one JSON object tagged by `op`. This page is the reference for all of them: what each does, its JSON shape, its inverse, and the changes it reports. The [live API](api.md) wraps them in methods whose params are a command's fields without `op` (`grid.fill_rect` is `fill_rect`; `set_project_setting` is `project.update`, `set_layer_state` is `level.set_layer_state` and `replace_level` is `level.replace`).

In Rust they are `worlds_core::command::Command`, applied with `Project::apply(cmd) -> Result<Applied, CommandError>`. `Applied` holds the `inverse` (a command) and the `changes` (a list). A JSON Schema comes from `schemars::schema_for!(Command)` (MCP resource `worlds://schema/command.json`). Commands deserialize from these JSON objects with serde, so a Rust program can apply the examples below as they are ([crates/worlds-core/README.md](../crates/worlds-core/README.md)).

The examples below are a script: run in order against `crates/worlds-core/tests/fixtures/mini.worlds`, each one applies, and `tests/docs.rs` checks that they do.

## How every command behaves

- **Atomic.** A command that fails changes nothing. A `batch` that fails part way undoes what it already did, and the error says which command failed. An inverse always applies to the state it was made from, even where a fresh edit would be refused (an entity a resize left outside the level goes back there).
- **Exact inverse.** Applying a command and then its inverse gives back the same project, down to whether an empty layer file exists. The inverse of the inverse redoes it.
- **No-ops are free.** A command that changes nothing (painting a cell the value it already has, moving a level where it is) reports no changes, dirties no file, and its inverse is an empty `batch`. History doesn't record it.
- **Only what changed is reported.** A paint of 10 cells where 3 actually change reports a rect around those 3, and its inverse only restores those 3.
- **Structural checks only.** A command is refused when it can't be applied or would break the model: unknown world, level, layer, value, tileset, entity type, field or id; a layer of the wrong kind; a cell outside the layer; a duplicate slug or id; a definition change that would orphan stored content (see [Definition commands](#definition-commands)). Everything else (field value types, entity limits, link targets, overlapping notes) is left to `worlds-cli validate` and the problems panel, so a half-finished edit is never blocked.
- **Empty layers go away.** When a command empties a layer's content in a level (every cell empty, no tiles left, no entities left in an entities layer), that content is dropped, so no empty file is saved. Set `keep_empty: true` on `paint_cells`, `set_tiles` or `remove_entity` to keep it. Inverses set it themselves when the empty content existed before, which is what keeps them exact.
- **Coordinates.** Cells are `[x, y]` in the layer's own grid (a layer with `grid_size: 8` in a 16 px project has twice the cells per axis). Rects are `[x, y, width, height]` in cells. Note coordinates are pixels.

Values: `null` is empty for grid cells (on a derived layer: no override), and "no value" for optional settings. For `set_level_field` and `set_entity_field`, leaving out `value` removes the stored value (the default applies), while `"value": null` stores null.

## Grid commands

These work on `int_grid` layers. A cell value (`GridValue` in Rust and in the JSON Schema) is a value slug from `layers.json`, `null`, or `"!"`:

| Value | On an `int_grid` layer | On a derived layer (`"derived": true`) |
| --- | --- | --- |
| a value slug | that value | an override: this value, whatever the rules say |
| `null` | empty | no override: back to what the rules compute |
| `"!"` | refused (`invalid`: `"!" (suppress) only applies to derived layers; collision isn't derived`) | an override: empty, suppressing what the rules would put there |

On a derived layer the grid commands work on the stored overrides (the level's `<layer>.json`, see [format.md](format.md#derived-layers-overrides)), never on what the rules computed: `flood_fill` floods the cells with the same stored state as `at` (from a cell with no override, that is every connected cell with none), and `replace_value` takes `"!"` as `from` or `to`. Inverses restore the stored states exactly, `"!"` included, and a level's overrides file goes away when no override is left in it. The changes are the usual `grid` change on the derived layer.

### paint_cells

Set cells to values. If a cell appears twice, the last one wins.

```json
{ "op": "paint_cells", "level": "crevasse", "layer": "collision",
  "cells": [[3, 1, "solid"], [4, 1, "solid"], [8, 2, null]] }
```

- **Inverse:** `paint_cells` with the old values of the cells that changed.
- **Changes:** one `grid` with the bounding rect of the cells that changed.

### fill_rect

Set every cell of a rectangle to one value. The rect must be inside the layer.

```json
{ "op": "fill_rect", "level": "crevasse", "layer": "collision", "rect": [2, 2, 3, 1], "value": "water" }
```

- **Inverse:** `paint_cells` with the old values of the cells that changed.
- **Changes:** one `grid`.

### flood_fill

Set the 4-connected region of cells holding the same value as `at` (diagonal neighbours don't connect). Filling with the value already there does nothing.

```json
{ "op": "flood_fill", "level": "crevasse", "layer": "collision", "at": [10, 1], "value": "oneway" }
```

- **Inverse:** `paint_cells` restoring the region.
- **Changes:** one `grid` with the region's bounding rect.

### replace_value

Every cell of the layer holding `from` gets `to`. Either may be `null` (empty).

```json
{ "op": "replace_value", "level": "crevasse", "layer": "collision", "from": "oneway", "to": null }
```

- **Inverse:** `paint_cells` with the cells that changed (not a reverse `replace_value`, which would also catch cells that already held `to`).
- **Changes:** one `grid`.

### Derived layers

A derived layer's cells are overrides. Here one is made, then a mark is set by hand, one cell suppressed, and the suppression taken back:

```json
{ "op": "set_layer_def", "layer": "marks",
  "def": { "slug": "marks", "name": "Marks", "kind": "int_grid", "derived": true,
           "values": [{ "slug": "pillar_base", "name": "Pillar base", "color": "#e0a040", "glyph": "p" }] } }
```

```json
{ "op": "paint_cells", "level": "crevasse", "layer": "marks",
  "cells": [[4, 3, "pillar_base"], [10, 3, "!"]] }
```

```json
{ "op": "paint_cells", "level": "crevasse", "layer": "marks", "cells": [[10, 3, null]] }
```

The second command writes `worlds/main/levels/crevasse/marks.json` with `"overrides": true` and the row `"....p.....!........."`; its inverse is `paint_cells` with `[[4, 3, null], [10, 3, null]]`, which deletes that file again. The third one's inverse is `[[10, 3, "!"]]`.

## Tile commands

These work on `tiles` layers. A cell can hold a stack of placements, drawn in list order.

### set_tiles

Replace the whole stack at each cell named in `tiles` with the placements given for it (in order), and empty each cell in `clear`. Cells not named are untouched.

```json
{ "op": "set_tiles", "level": "crevasse", "layer": "decor_back",
  "tiles": [
    { "cell": [4, 2], "tileset": "castle_main", "tile": [1, 1] },
    { "cell": [9, 0], "tileset": "castle_main", "tile": [2, 2] },
    { "cell": [9, 0], "tileset": "ssw_set5", "tile": [0, 0], "alpha": 0.5 }
  ],
  "clear": [[5, 2]] }
```

- **Inverse:** `set_tiles` with the old stacks of the cells that changed, and `clear` for those that were empty.
- **Changes:** one `tiles` with the bounding rect of the cells whose stack changed.

### clear_tiles

Remove every placement whose cell is in `rect`.

```json
{ "op": "clear_tiles", "level": "crevasse", "layer": "decor_back", "rect": [0, 0, 8, 4] }
```

- **Inverse:** `set_tiles` putting the removed stacks back.
- **Changes:** one `tiles`.

## Level commands

Level commands name the world as well as the level, so the agent states where it thinks the level is. Level slugs are unique across worlds.

### create_level

`level` is what goes in `level.json` (an `id` is required: 10 characters from `a-z` and `2-7`). `content` is optional and holds the level's layer files, entities and notes in their file form: `int_grids` (each like an `<int_grid layer>.json`, legend optional; a derived layer's entry holds its overrides, `"overrides": true` optional), `tiles` (each like a `<tiles layer>.json`), `entities` (the `layers` of `entities.json`) and `notes`. `size` must be at least 1x1, grid rows must match the level size in the layer's cells, and the level, entity and note ids must be unused.

`restore: true` (default `false`) puts the level back exactly as given: a grid whose size doesn't match the level, and ids another level also uses, are accepted. A project can hold those (the loader reads them and `validate` reports them, see [format.md](format.md)), so this is what keeps undoing the deletion of such a level exact. Edits leave it off; inverses set it.

```json
{ "op": "create_level", "world": "main",
  "level": { "id": "annexlvl22", "slug": "annex", "name": "Annex", "world_px": [320, 0], "size": [6, 3], "depth": 0 },
  "content": {
    "int_grids": [
      { "layer": "collision", "rows": ["######", "#....#", "######"] }
    ]
  } }
```

- **Inverse:** `delete_level`.
- **Changes:** `level_created` (the whole level directory is dirty).

### replace_level

Replace a level, found by `level.slug`, wholesale: `level.json` and all content. Content left out of `content` is removed. This is mostly the inverse of `resize_level`, but it is also the way to rewrite a level in one step.

```json
{ "op": "replace_level", "world": "main",
  "level": { "id": "annexlvl22", "slug": "annex", "name": "Annex", "world_px": [320, 0], "size": [6, 3], "depth": 0 },
  "content": {
    "int_grids": [
      { "layer": "collision", "rows": ["######", "#..~.#", "######"] }
    ]
  } }
```

`restore` works as for `create_level`.

- **Inverse:** `replace_level` with the old level, `restore: true`.
- **Changes:** `level_meta` (part `all`), then a `grid` or `tiles` for every layer with content before or after, an `entity_layer` for every entities layer whose entry appeared, went away or went from empty to not (or back), an `entity` for every entity and a `note` for every note before or after.

### move_level

Set the top-left corner in world pixels.

```json
{ "op": "move_level", "world": "main", "level": "annex", "world_px": [480, 0] }
```

- **Inverse:** `move_level` to the old position.
- **Changes:** `level_meta`, part `position`.

### resize_level

Change the size, in cells of the project grid. `anchor` says what stays put in the world, per axis: `-1` the left (or top) edge, `0` the center, `1` the right (or bottom) edge. Default `[0, 0]`. With a center anchor and an odd difference, the extra cell goes on the right or bottom.

The content keeps its place in the world: when the left edge moves, `world_px` changes and everything inside shifts the other way. Grid and tile content is cropped or extended (new cells are empty). Entities and notes shift with the content and are kept even when they end up outside; each entity left outside is reported. A layer on another grid shifts by the same number of pixels, which must be whole cells of that layer, or the resize is refused (pick another anchor).

```json
{ "op": "resize_level", "world": "main", "level": "annex", "size": [8, 4], "anchor": [1, -1] }
```

- **Inverse:** `replace_level` with the level as it was, `restore: true` (a resize can crop, so the inverse carries the old content).
- **Changes:** `level_meta` part `size`, `level_meta` part `position` when the content shifted, a `grid` or `tiles` per layer with content (whole layer), an `entity` and `note` per item that moved, and `entity_outside` for each entity now outside the level.

### rename_level

Change the slug (the level's directory, and its name in the export) and the display name. Every note link `level:<old slug>` becomes `level:<new slug>`. Refused if the new slug is taken, or if a note already links to `level:<new slug>` (undo couldn't tell it from a rewritten link).

```json
{ "op": "rename_level", "world": "main", "level": "annex", "slug": "wing", "name": "West wing" }
```

- **Inverse:** `rename_level` back.
- **Changes:** `level_renamed` (dirties both directories), a `note` per rewritten link, and `level_meta` part `name` if the name changed.

### set_level_field

Set a level field from `project.json` `level_fields`. Leave out `value` to remove the stored value; `"value": null` stores null. Removing works for any key; setting needs a defined field unless `allow_unknown: true` (default `false`). Edits leave that off; inverses set it to put back a value stored under a key no field defines, so undoing the removal of an unknown key always works.

```json
{ "op": "set_level_field", "world": "main", "level": "crevasse", "field": "music", "value": "cave_theme" }
```

- **Inverse:** `set_level_field` with the old value (and `allow_unknown: true` when no field defines the key), or without `value` if there was none.
- **Changes:** `level_meta`, part `fields` (rule groups can require level fields, so the `auto` layers with a group requiring that field rebake).

### set_level_depth

```json
{ "op": "set_level_depth", "world": "main", "level": "wing", "depth": 2 }
```

- **Inverse:** the old depth. **Changes:** `level_meta`, part `depth`.

### set_level_bg

`null` falls back to the project's background.

```json
{ "op": "set_level_bg", "world": "main", "level": "wing", "bg_color": "#101018" }
```

- **Inverse:** the old color. **Changes:** `level_meta`, part `bg_color`.

### set_level_tags

Replace a level's `tags` (stored as given, order kept; an empty list removes them). Tags are free strings in the same vocabulary as annotation tags; validation reports empty or repeated ones, and a level tagged `T` whose center is outside every world region tagged `T`.

```json
{ "op": "set_level_tags", "world": "main", "level": "wing", "tags": ["forest"] }
```

- **Inverse:** `set_level_tags` with the old list. **Changes:** `level_meta`, part `tags`.

### delete_level

```json
{ "op": "delete_level", "world": "main", "level": "shaft" }
```

- **Inverse:** `create_level` with the full level, content included, and `restore: true`, so the level comes back byte for byte even if it held a validation problem a fresh `create_level` would refuse.
- **Changes:** `level_deleted` (the whole level directory is dirty, and saving removes its files).

## Entity commands

Entities are found by id anywhere in the project.

### add_entity

Place an entity on an `entities` layer of a level. `index` is its position in the layer's list (default: the end). The id must be unused, the type must exist, and the cell must be inside the level unless `allow_outside: true` (default `false`). Edits leave that off; inverses set it to put back an entity that a `resize_level` left outside the level, so undoing the removal of such an entity always works.

```json
{ "op": "add_entity", "level": "wing", "layer": "objects",
  "entity": { "id": "wingdoor22", "type": "locked_door", "cell": [3, 2], "fields": { "channel": 2 } } }
```

- **Inverse:** `remove_entity`.
- **Changes:** `entity`.

### move_entity

Set the cell (inside the level, unless `allow_outside: true`, as for `add_entity`) and pixel offset (default `[0, 0]`).

```json
{ "op": "move_entity", "id": "wingdoor22", "cell": [4, 2], "offset_px": [0, -2] }
```

- **Inverse:** `move_entity` to the old cell and offset, with `allow_outside: true` when the old cell was outside the level. **Changes:** `entity`.

### resize_entity

Set the per-instance size in cells of the project grid; `null` goes back to the type's size. Whether the type is resizable is checked by validation, not here.

```json
{ "op": "resize_entity", "id": "wingdoor22", "size": [1, 3] }
```

- **Inverse:** the old size. **Changes:** `entity`.

### set_entity_field

Set a field the entity's type defines. Leave out `value` to remove it (the type's default applies); `null` stores null. Removing works for any key; setting a key the type doesn't define needs `allow_unknown: true` (default `false`), which only inverses set, as for `set_level_field`.

```json
{ "op": "set_entity_field", "id": "wingdoor22", "field": "needs_item", "value": "brass_key" }
```

- **Inverse:** the old value (with `allow_unknown: true` when the type doesn't define the key), or no value. **Changes:** `entity`.

### remove_entity

```json
{ "op": "remove_entity", "id": "btncrev222" }
```

- **Inverse:** `add_entity` with the entity at its old `index`, and `allow_outside: true` when the entity was outside the level.
- **Changes:** `entity`.

## Note commands

Annotations live at world scope (`world.json`, world pixels) or level scope (`notes.json`, level pixels). `scope` is `{ "world": "<slug>" }` or `{ "level": "<slug>" }`. Note ids are unique across the project. Moving, resolving, locking or retagging an annotation is an `update_note`. Commands ignore `locked` (the editor's tools and the API's `confirm` rule enforce it). Guide groups (`world.json` `guide_groups`) have the last two commands of this section.

### add_note

`note` is an annotation exactly as in the files. `index` is its position in the list (default: the end).

```json
{ "op": "add_note", "scope": { "level": "wing" },
  "note": { "id": "wingnote22", "kind": "note", "title": "Needs a key room", "px": [40, 24], "tags": ["todo"] } }
```

- **Inverse:** `remove_note`. **Changes:** `note`.

### update_note

Replace an annotation wholesale (to resolve it, send it back with `"status": "done"`). `note.id` must equal `id`.

```json
{ "op": "update_note", "id": "wingnote22",
  "note": { "id": "wingnote22", "kind": "note", "title": "Needs a key room", "px": [40, 24], "tags": ["todo"], "status": "done" } }
```

- **Inverse:** `update_note` with the old annotation. **Changes:** `note`.

### remove_note

```json
{ "op": "remove_note", "id": "pathcrev22" }
```

- **Inverse:** `add_note` at the old scope and index. **Changes:** `note`.

### set_guide_group

Add a guide group to `world.json` `guide_groups`, or replace the one with the same `group.slug` in place. `group` is the entry exactly as in the file. `index` is the position of a new group (default: the end), or where to move an existing one (default: where it is); past the end is refused. A guide joins a group through its own `group` key (`update_note`).

```json
{ "op": "set_guide_group", "world": "main",
  "group": { "slug": "ruined", "name": "Ruined wing", "offset": [0, 769], "color": "#c08040" } }
```

- **Inverse:** `set_guide_group` with the old entry at its old index, or `remove_guide_group` for a new one. **Changes:** `guide_group`.

### remove_guide_group

Refused (`invalid`) while a guide of the world names the group: "guide group ruined is used by 3 guides (guide00001, ...); move them out first". An unknown group is `unknown_guide_group`.

```json
{ "op": "remove_guide_group", "world": "main", "slug": "ruined" }
```

- **Inverse:** `set_guide_group` with the entry at its old index. **Changes:** `guide_group`.

## Rule state and project settings

### set_layer_state

Set an `auto` layer's per-level rule state (`seed`, `optional_groups`, `tilesets`, as in `level.json` `layers`). `null` removes the entry.

```json
{ "op": "set_layer_state", "level": "crevasse", "layer": "main_tiling",
  "state": { "seed": 7, "optional_groups": ["flooded_decor"] } }
```

- **Inverse:** the old state, or `null`. **Changes:** `layer_state` (that layer rebakes in full).

### set_project_setting

One of the simple settings in `project.json`: `name`, `bg_color` (or `null`), `collision_layer` (an `int_grid` layer, or `null`), `seed`, `grid_size` (at least 1; refused once the project has a level, since level sizes and positions are in cells of it), `blocking_values` (a list of the collision layer's value slugs, or `null` to leave the key out; values the collision layer doesn't have are left to validation).

```json
{ "op": "set_project_setting", "setting": { "key": "seed", "value": 1234 } }
```

```json
{ "op": "set_project_setting", "setting": { "key": "blocking_values", "value": ["solid", "fortified"] } }
```

- **Inverse:** the old value. **Changes:** `project` with the key (a new `seed` rebakes everything, and so does a new `grid_size`; `collision_layer` and `blocking_values` bake nothing, they only matter to the export).

## Definition commands

Layers, entity types, enums, tilesets, rule groups, rules, stamps, macros (`rules/macros.json`), and the fields of entity types and levels ([Field definition commands](#field-definition-commands)).

- **`set_*`** creates a definition or replaces it wholesale. `def` is exactly the entry as it is in the file (`layers.json`, `entities/<slug>.json`, `enums.json`, `tilesets.json`, a group, rule or stamp of `rules/<layer>.json`, an entry of `rules/macros.json`), and its `slug` must equal the slug the command names. Replacing with the same definition (at the same place) changes nothing. Lists whose order matters (layers, enums, tilesets, rule groups, rules, stamps) take an optional `index`: the position for a new entry (default: the end), or where to move an existing one (default: where it is).
- **`remove_*`** takes the definition away, with the content that can't exist without it (a layer's content in every level, an entity type's instances). References from elsewhere (an `auto` layer's `source`, the `collision_layer`, a field's `enum`) are left for `worlds-cli validate` to report. Tilesets, macros and stamps are the exceptions: removing one that is used is refused (unless `keep_uses`, the inverse of defining one that was already named).
- **`rename_*`** changes a slug and rewrites every reference to it, so the inverse is the reverse rename. It is refused when the new slug is taken, or when something already refers to the new slug without it existing (a dangling reference): undo couldn't tell that reference from a rewritten one. The error says what refers to it.
- **Changes:** `def` for each definition added, replaced, removed or rewritten, `def_renamed` for the renamed one, and a change for every other file a rename rewrote (an `entity` per rewritten instance, `level_meta` part `fields`, `layer_state`, `tiles` with the rewritten cells, `project` key `collision_layer` or `level_fields`, `def` kind `rules` or `macros`).
- **Which changes affect bakes** (of `auto` layers, in every level unless the change names one):
  - `def` and `def_renamed` of kind `rules`, `macros` and `tileset`: the auto layers that use that rule set, macro file or tileset, and every auto layer that depends on one of them (reads a derived grid it writes, directly or through others), fully, with no paint stroke needed;
  - `def` of kind `layer`, and `def_renamed` of kind `layer` or `tileset`: everything (for a derived layer, its writer first and then its readers);
  - `grid` on a derived layer (an override edit): the auto layers that read it, and its writer when the writer reads it too;
  - `level_meta` part `tags`: the auto layers with a group whose `requires_level_tag` it can change, and their dependents;
  - `project` key `seed`: every bake;
  - `project` key `level_fields`, and `level_meta` part `fields`: only the auto layers with a group whose `requires_level_field` names a field that changed;
  - `def` or `def_renamed` of kind `entity` or `enum`, and `project` key `collision_layer`: no bake (`collision_layer` matters only to the export). A rename that rewrites a rule group's requirement reports that rule set's `def` too.

### set_layer_def

Replacing a layer keeps its content: an `int_grid` layer's cells (and a derived layer's overrides) keep their values by slug, so values can be reordered, renamed glyphs show up in the files, and a value can be added. Refused: taking out a value some cell or override still holds (`replace_value` it first), changing the kind or the grid of a layer that has content, switching `derived` on or off while the layer has content in any level (its files would change meaning: "pillar_marks has overrides in crevasse; clear them first"), and a slug a layer of that kind can't have (`level`, `entities`, `notes` or `macros` for `int_grid` and `tiles`, `macros` for `auto`).

A new layer can come with `content`, one entry per level, `{ "level", "rows" | "tiles" | "entities" | "state" }` (the rows of `<layer>.json`, a derived layer's override rows with `.` and `!`, its tile placements, the layer's `entities.json` entry, its rule state in `level.json`), and with `rules`, its `rules/<layer>.json`. That is how the inverse of `remove_layer_def` puts everything back. Content is taken as it is in the files: rows and tiles must fit the layer's kind, entity ids must be unused, and the rest is for validation.

```json
{ "op": "set_layer_def", "layer": "hazards", "index": 1,
  "def": { "slug": "hazards", "name": "Hazards", "kind": "int_grid",
           "values": [{ "slug": "spikes", "name": "Spikes", "color": "#c03030", "glyph": "^" }] },
  "content": [
    { "level": "crevasse",
      "rows": ["....................", "....................", "....................",
               "..^^................", "...................."] }
  ] }
```

- **Inverse:** a new layer: `remove_layer_def`. A replaced one: `set_layer_def` with the old definition (and its old `index` if it moved).
- **Changes:** `def` kind `layer`. A new layer with content also reports what `remove_layer_def` would. Replacing an `int_grid` layer's values reports a `grid` (whole layer) for every level with content.

### rename_layer

Rewrites the level files (`<from>.json` becomes `<to>.json`, a derived layer's overrides file included), `entities.json` entries, rule state keys in `level.json`, `auto` layers' `source`, rule patterns' `grid` (`match` and every `require`, in rule files and in templates), region rules' `region.grid` and `write.grid`, the layer's own `rules/<layer>.json` (which becomes `rules/<to>.json`) and the project's `collision_layer`.

```json
{ "op": "rename_layer", "from": "hazards", "to": "traps" }
```

- **Inverse:** `rename_layer` back.
- **Changes:** `def_renamed` kind `layer`, `def_renamed` kind `rules` if it has a rule set, a `def` per rewritten layer or rule set (`match.grid`, `require.grid`, `region.grid`, `write.grid`), `def` kind `macros` if a template rule's grid was rewritten, `project` key `collision_layer`, then per level a `layer_file_renamed` for its `int_grid` or `tiles` file, an `entity_layer` for its entities entry and a `layer_state` for its rule state.

### remove_layer_def

Removes the layer, its content in every level (grid, overrides or tiles file, `entities.json` entry with every entity on it, rule state) and its `rules/<layer>.json`.

```json
{ "op": "remove_layer_def", "layer": "traps" }
```

- **Inverse:** `set_layer_def` with the definition at its old `index`, the `content` of every level and the `rules`.
- **Changes:** `def` kind `layer`, `def` kind `rules` if it had a rule set, then per level a `grid` or `tiles` (whole layer), an `entity_layer` and an `entity` per instance, and a `layer_state`.

### set_entity_def

```json
{ "op": "set_entity_def", "entity": "lever",
  "def": { "slug": "lever", "name": "Lever", "size": [1, 1], "color": "#8080ff",
           "fields": [{ "slug": "target", "type": "entity_ref", "allowed_types": ["locked_door"] }] } }
```

- **Inverse:** a new type: `remove_entity_def`, with `keep_instances: true` when instances already named the type (they were dangling, and undo leaves them so). A replaced one: `set_entity_def` with the old definition.
- **Changes:** `def` kind `entity` (its file is `entities/<slug>.json`).

### rename_entity_type

Rewrites every instance's `type` and every `entity_ref` field's `allowed_types` (entity types and level fields). Tags are free text and stay as they are.

```json
{ "op": "rename_entity_type", "from": "locked_door", "to": "door" }
```

- **Inverse:** `rename_entity_type` back.
- **Changes:** `def_renamed` kind `entity` (saving deletes the old file), a `def` per entity type whose fields were rewritten, `project` key `level_fields` if a level field was, and an `entity` per instance.

### remove_entity_def

Removes the type and every instance of it. With `keep_instances: true` (optional, default `false`) it removes only the type and leaves its instances, dangling, for validation to report; that is the inverse of defining a type that instances already named.

```json
{ "op": "remove_entity_def", "entity": "lever" }
```

- **Inverse:** with no instances, or with `keep_instances`, `set_entity_def`. With instances, the command runs as a `batch` of `remove_entity` for each instance and then `remove_entity_def`, so the inverse is a `batch` that puts the type back and then each instance at its old index.
- **Changes:** an `entity` per instance, then `def` kind `entity` (saving deletes the file).

### set_enum_def

```json
{ "op": "set_enum_def", "enum": "weather", "index": 0,
  "def": { "slug": "weather", "values": [{ "slug": "rain" }, { "slug": "snow" }] } }
```

- **Inverse:** a new enum: `remove_enum_def`. A replaced one: `set_enum_def` with the old definition (and its old `index` if it moved). Values taken out of an enum stay where they are stored; validation reports them.
- **Changes:** `def` kind `enum`.

### rename_enum

Rewrites every `enum` field's `enum` (entity types and level fields).

```json
{ "op": "rename_enum", "from": "facing", "to": "direction" }
```

- **Inverse:** `rename_enum` back.
- **Changes:** `def_renamed` kind `enum`, a `def` per rewritten entity type, `project` key `level_fields` if a level field was rewritten.

### rename_enum_value

Rewrites the value everywhere a field of this enum holds it: entity field values, level field values, field defaults (single values and arrays), and rule groups' `requires_level_field` on a level field of this enum. Refused if the enum already has a value `to`.

```json
{ "op": "rename_enum_value", "enum": "biome", "from": "flooded", "to": "submerged" }
```

- **Inverse:** `rename_enum_value` back.
- **Changes:** `def` kind `enum`, a `def` per entity type whose defaults changed, `project` key `level_fields`, a `level_meta` part `fields` per level and an `entity` per instance that held it, and a `def` kind `rules` per rewritten rule set.

### remove_enum_def

```json
{ "op": "remove_enum_def", "enum": "weather" }
```

- **Inverse:** `set_enum_def` at the old `index`. **Changes:** `def` kind `enum`.

### set_tileset_def

```json
{ "op": "set_tileset_def", "tileset": "cave_fg",
  "def": { "slug": "cave_fg", "image": "art/cave_fg.png", "tile_size": [16, 16] } }
```

- **Inverse:** a new tileset: `remove_tileset_def`, with `keep_uses: true` when something already named it (a dangling reference, which undo leaves as it was). A replaced one: `set_tileset_def` with the old definition (and its old `index` if it moved).
- **Changes:** `def` kind `tileset`.

### rename_tileset

Rewrites every reference: tile placements, `tiles` layers' palette `tileset`, rule outputs and stamps, template references' `tileset` and the tilesets fixed template rules name, rule state tileset substitutions (both sides), icons (enum values, entity types, rule groups), `tile` fields' `tileset` and every `tile` field value or default.

```json
{ "op": "rename_tileset", "from": "castle_main", "to": "castle" }
```

- **Inverse:** `rename_tileset` back.
- **Changes:** `def_renamed` kind `tileset`, then a `def` per rewritten layer, enum, entity type or rule set (a template reference's `tileset` is in its rule set), `def` kind `macros` if a template rule's own tileset was rewritten, `project` key `level_fields`, a `tiles` per level and layer (the rect of the rewritten cells), a `layer_state`, a `level_meta` part `fields` and an `entity` per rewritten value.

### remove_tileset_def

Refused while anything uses the tileset; the error (`in_use`) lists the first few uses (`layer decor_back`, `rule set main_tiling`, `rule set main_tiling reference walls`, `template frame6`, `tiles of layer decor_back in level crevasse`, ...). Rename or remove those first. `keep_uses: true` (optional, default `false`) removes it anyway and leaves the uses dangling for validation to report; it is the inverse of defining a tileset that something already named.

```json
{ "op": "remove_tileset_def", "tileset": "cave_fg" }
```

- **Inverse:** `set_tileset_def` at the old `index`. **Changes:** `def` kind `tileset`.

### set_rule_group

Create or replace one group of an `auto` layer's `rules/<layer>.json`. `index` is its place in evaluation order. A layer without a rule set gets one.

Groups are found by slug, so while two groups of a layer share one (a validation problem the loader accepts) every group and rule command on that layer (`set_rule_group`, `remove_rule_group`, `set_rule`, `remove_rule`, `move_rule`, and `rename_macro` of a template the layer references without a slug) is refused as `invalid`, naming the slug: which group is meant, and where undo would put it back, would be a guess. Fix the file by hand first. Stamp commands are refused the same way while two stamps share a slug.

```json
{ "op": "set_rule_group", "layer": "main_tiling", "group": "vines", "index": 1,
  "def": { "slug": "vines", "mode": "optional", "rules": [
    { "slug": "vine", "match": { "rows": ["#", ".", "*"] },
      "out": { "tileset": "castle", "tiles": [[7, 7]] } } ] } }
```

- **Inverse:** a new group: `remove_rule_group` (with `keep_empty` when the rule set was there and empty). A replaced one: `set_rule_group` with the old group (and its old `index` if it moved).
- **Changes:** `def` kind `rules`, slug the layer.

### remove_rule_group

A rule set left without groups and stamps goes away (its file is deleted) unless `keep_empty: true`.

```json
{ "op": "remove_rule_group", "layer": "main_tiling", "group": "retired" }
```

- **Inverse:** `set_rule_group` at the old `index`. **Changes:** `def` kind `rules`.

### set_rule

Create or replace one entry of a rule group: a rule, or a template reference (`{ "use": ... }`, see [format.md](format.md)). `rule` is the entry's slug (a reference's `slug`, which defaults to its template's name). A new entry goes at `index` in the group (default: the end). An entry of that group with that slug is replaced in place, or moved to `index` when it is given. Refused when another group of the layer has an entry with that slug (use `move_rule`), or when two entries of the group already have it (a validation problem; replace the group with `set_rule_group` to tell them apart). A reference to a template that doesn't exist, or a slug that collides with an expanded rule, is left for validation.

```json
{ "op": "set_rule", "layer": "main_tiling", "group": "walls", "rule": "ledges", "index": 1,
  "def": { "use": "ledge", "slug": "ledges", "tileset": "castle", "origin": [14, 0] } }
```

- **Inverse:** a new entry: `remove_rule`. A replaced one: `set_rule` with the old entry (and its old `index` if it moved).
- **Changes:** `def` kind `rules`, slug the layer.

### move_rule

Move an entry (found by slug across the layer's groups) to position `index` of `group`, which may be another group. `index` counts the group's entries after the moved one is taken out of its old place, so within one group `index` is where it ends up. Moving it to where it is changes nothing.

```json
{ "op": "move_rule", "layer": "main_tiling", "rule": "vine", "group": "walls", "index": 0 }
```

- **Inverse:** `move_rule` back to the old group and index. **Changes:** `def` kind `rules`.

### remove_rule

Remove an entry, a rule or a template reference, found by slug across the layer's groups. A rule a reference expands to (`<reference>__<rule>`) isn't an entry: the error says to add it to the reference's `skip` instead. A slug held by several entries (a validation problem) is refused until they are told apart.

```json
{ "op": "remove_rule", "layer": "main_tiling", "rule": "any_wall_group" }
```

- **Inverse:** `set_rule` in the old group at the old `index`. **Changes:** `def` kind `rules`.

### set_macro

Create or replace a named pattern, legend or template in `rules/macros.json` (the file is made when missing). `kind` is `pattern`, `legend` or `template`; `def` is the entry as in the file: rows (an array of strings), a legend (glyph to cell test) or a template. Patterns, legends and templates have separate names. A built-in template's name (`blob47`) is refused.

```json
{ "op": "set_macro", "kind": "template", "name": "ledge",
  "def": { "name": "Ledge", "legend": "walls", "rules": [
    { "slug": "top", "match": { "rows": "edge_top" }, "out": { "tiles": [[1, 0]] }, "break": true },
    { "slug": "fill", "match": { "rows": ["#"] }, "out": { "tiles": [[1, 1]] } } ] } }
```

- **Inverse:** a new macro: `remove_macro`, with `keep_uses: true` when something already referred to the name, and `keep_empty: true` when `rules/macros.json` was there and empty. A replaced one: `set_macro` with the old entry.
- **Changes:** `def` kind `macros`, slug `macros`.

### rename_macro

Rename a pattern, legend or template and rewrite every reference to it: `match.rows` and `require.rows` (patterns), `match.legend`, `require.legend` and a template's `legend` (legends), and `use` (templates), in every rule file and in templates. A reference with no `slug` of its own is named by its template, so renaming the template pins its `slug` to the old name (`{ "use": "edge", "slug": "frame" }` after renaming `frame` to `edge`): its expanded rules keep their slugs, and so their random choices. Refused when such a reference's slug is also on another entry of its layer (undo finds it by slug).

```json
{ "op": "rename_macro", "kind": "pattern", "from": "edge_top", "to": "top_edge" }
```

- **Inverse:** `rename_macro` back; when references were pinned, a `batch` of that and a `set_rule` per pinned reference with its old entry (the pin taken out).
- **Changes:** a `def` kind `rules` per rule set with a pinned reference, `def` kind `macros`, then a `def` kind `rules` per other rewritten rule set. No `def_renamed`: a macro has no file of its own.

### remove_macro

Refused while a rule, a reference or a template refers to it; the error (`in_use`, `def_kind` `macros`) lists the first few. `keep_uses: true` removes it anyway and leaves the references dangling for validation to report (the inverse of defining a name that was already referred to). A `rules/macros.json` left empty goes away (its file is deleted) unless `keep_empty: true`. Here the reference that uses `ledge` goes first:

```json
{ "op": "remove_rule", "layer": "main_tiling", "rule": "ledges" }
```

```json
{ "op": "remove_macro", "kind": "template", "name": "ledge" }
```

- **Inverse:** `set_macro` with the old entry. **Changes:** `def` kind `macros`.

### set_stamp

Create or replace a stamp of `rules/<layer>.json`. `index` as for `set_rule_group`. A layer without a rule set gets one. A stamp with `occupies: true` marks its footprint in the layer's occupancy grid, which outputs with `avoid_overlap` respect (see [format.md](format.md#stamps)); changing it rebakes the layer like any rule set edit.

```json
{ "op": "set_stamp", "layer": "main_tiling", "stamp": "arch",
  "def": { "slug": "arch", "tileset": "castle", "rect": [4, 4, 3, 2], "pivot": [0.5, 1.0] } }
```

- **Inverse:** a new stamp: `remove_stamp` (with `keep_uses` when a rule already placed it, `keep_empty` when the rule set was there and empty). A replaced one: `set_stamp` with the old stamp (and its old `index` if it moved).
- **Changes:** `def` kind `rules`.

### remove_stamp

Refused while an `out.stamp` names it, in the layer's rules or in a template the layer references; the error is `in_use` with `def_kind` `stamp` and the `layer`. `keep_uses: true` removes it anyway. A rule set left without groups and stamps goes away unless `keep_empty: true`.

```json
{ "op": "remove_stamp", "layer": "main_tiling", "stamp": "arch" }
```

- **Inverse:** `set_stamp` at the old `index`. **Changes:** `def` kind `rules`.

## Field definition commands

The fields of an entity type (its `fields`) and the level fields (`project.json` `level_fields`) have commands of their own, so an edit to one field doesn't resend the whole type. `def` is a field exactly as in the file (see [format.md](format.md#field-definitions)) and its `slug` must equal `field`. The level field commands are the entity field ones without `entity`.

Fields are found by slug, so while two fields of a type (or two level fields) share the slug a command names (a validation problem the loader accepts), the command is refused as `invalid`: which field is meant, and where undo would put it back, would be a guess. Fix the file by hand first.

### set_entity_field_def

Create a field or replace it wholesale. `index` is its position in the type's `fields`: for a new field, default the end; for an existing one, default where it is, so a move is a `set_entity_field_def` with an `index`. Stored values are never touched, even when the type changes: values that no longer fit are left for validation to report.

```json
{ "op": "set_entity_field_def", "entity": "door", "field": "key_color", "index": 2,
  "def": { "slug": "key_color", "type": "color", "default": "#ffd040", "display": { "canvas": true } } }
```

- **Inverse:** a new field: `remove_entity_field_def`, with `keep_values: true` when instances already stored a value under that key (undo leaves those values). A replaced one: `set_entity_field_def` with the old definition (and its old `index` if it moved).
- **Changes:** `def` kind `entity`.

### rename_entity_field

Renames the field and the key every instance of the type stores it under. An instance that stored a value before stores it under the new slug, so it is still in the export's `fields_set`. Refused when the type already has a field `to` (`duplicate_field`), and when an instance of the type already stores a value under `to` (undo couldn't tell it from a renamed one; the error names the first such entity).

```json
{ "op": "rename_entity_field", "entity": "door", "from": "needs_item", "to": "key_item" }
```

- **Inverse:** `rename_entity_field` back.
- **Changes:** `def` kind `entity`, then an `entity` per instance whose `fields` held `from`.

### remove_entity_field_def

Removes the field and the values instances store for it. Each value is taken out by entity id, so while an instance that stores one shares its id with another entity (a validation problem), the command is refused (`invalid`) unless `keep_values` is set: which instance an unset means would be a guess. With `keep_values: true` (optional, default `false`) only the definition goes and stored values stay, as unknown keys for validation to report; that is the inverse of creating a field instances already stored a value for.

```json
{ "op": "remove_entity_field_def", "entity": "door", "field": "tint" }
```

- **Inverse:** with no stored values, or with `keep_values`: `set_entity_field_def` at the old `index`. With stored values, the command runs as a `batch` of `set_entity_field` without `value` for each instance that stores one, then `remove_entity_field_def`, so the inverse is a `batch` that puts the field back and then each value.
- **Changes:** an `entity` per instance that stored a value, then `def` kind `entity`.

### set_level_field_def

```json
{ "op": "set_level_field_def", "field": "ambience",
  "def": { "slug": "ambience", "type": "float", "default": 0.5, "min": 0, "max": 1 } }
```

- **Inverse:** as for `set_entity_field_def`, with `remove_level_field_def` (`keep_values: true` when a level already stored a value under that key) or the old definition.
- **Changes:** `project` key `level_fields`.

### rename_level_field

Renames the field, the key every level stores it under, and every rule group's `requires_level_field` on it. Refused when a level field `to` exists (`duplicate_field`), when a level already stores a value under `to`, or when a rule group already requires a level field `to`.

```json
{ "op": "rename_level_field", "from": "biome", "to": "zone" }
```

- **Inverse:** `rename_level_field` back.
- **Changes:** `project` key `level_fields`, a `level_meta` part `fields` per level that stored it, and a `def` kind `rules` per rule set with a rewritten requirement.

### remove_level_field_def

Removes the field and every level's value for it; `keep_values` as for `remove_entity_field_def`. A rule group that requires it is left for validation to report.

```json
{ "op": "remove_level_field_def", "field": "music" }
```

- **Inverse:** as for `remove_entity_field_def`: `set_level_field_def` at the old `index`, or, with stored values, a `batch` that puts the field back and then each level's value (the command ran as a `batch` of `set_level_field` without `value` per level, then the remove).
- **Changes:** a `level_meta` part `fields` per level that stored a value, then `project` key `level_fields`.

## batch

Apply several commands as one: all of them or none. If command `i` fails, the ones before it are undone in reverse and the error is `batch "<label>", command <i>: <why>`. Batches nest. Inverses are built to always apply to the state they came from, so the rollback can't fail in practice; if one is refused anyway, the rest are still applied, every file the batch touched is marked dirty, and the error is `rollback` (it names the batch error and the refused inverses). Treat the project as changed after a `rollback` error. An agent's request and an editor gesture are each one batch, and one undo entry.

```json
{ "op": "batch", "label": "Agent: wall off the gap",
  "commands": [
    { "op": "fill_rect", "level": "crevasse", "layer": "collision", "rect": [6, 1, 1, 3], "value": "solid" },
    { "op": "paint_cells", "level": "crevasse", "layer": "collision", "cells": [[7, 3, "solid"]] }
  ] }
```

- **Inverse:** a `batch` with the same label holding the inverses in reverse order (no-op steps left out).
- **Changes:** the steps' changes in order, merged: duplicates dropped, and the `grid` (or `tiles`) rects of one layer joined into their bounding rect. Level creation, deletion and renaming are kept in place and nothing merges across them.

## Changes

Each change says what was touched, precisely enough for the editor to redraw and rebake only that and for the server to send it as an event. In Rust: `worlds_core::command::Change`; `change.file()` is the project-relative file it dirties.

| Change | Fields | File it dirties |
| --- | --- | --- |
| `grid` | `world`, `level`, `layer`, `rect` | `worlds/<world>/levels/<level>/<layer>.json` |
| `tiles` | `world`, `level`, `layer`, `rect` | same |
| `level_meta` | `world`, `level`, `part`: `position`, `size`, `name`, `depth`, `bg_color`, `fields`, `tags` or `all` | `.../<level>/level.json` |
| `level_created` | `world`, `level` | the directory `.../levels/<level>` (every file in it) |
| `level_deleted` | `world`, `level` | the directory (saving deletes its files) |
| `level_renamed` | `world`, `from`, `to` | both directories (`change.files()` lists them) |
| `entity` | `world`, `level`, `id` | `.../<level>/entities.json` |
| `entity_outside` | `world`, `level`, `id` | same; after a resize, this entity sits outside the level |
| `entity_layer` | `world`, `level`, `layer` | same; an entities layer's entry was added, removed or emptied (`replace_level` with empty entries) |
| `note` | `world`, `level` (`null` at world scope), `id` | `worlds/<world>/world.json` or `.../<level>/notes.json` |
| `layer_state` | `world`, `level`, `layer` | `.../<level>/level.json` |
| `guide_group` | `world`, `slug` | `worlds/<world>/world.json`; a guide group was added, replaced, moved or removed |
| `project` | `key`: `name`, `bg_color`, `collision_layer`, `seed` or `level_fields` | `project.json` |
| `def` | `kind`: `layer`, `entity`, `enum`, `tileset`, `rules` or `macros`; `slug` | `layers.json`, `entities/<slug>.json`, `enums.json`, `tilesets.json`, `rules/<slug>.json` (the slug of a rule set is its layer's) or `rules/macros.json` (slug `macros`) |
| `def_renamed` | `kind`, `from`, `to` | the file of `to`; for `entity` and `rules` also the file of `from`, which saving deletes |
| `layer_file_renamed` | `world`, `level`, `from`, `to` | `.../<level>/<to>.json`, and `.../<level>/<from>.json`, which saving deletes |

Rects are in cells of the layer's grid and cover the cells whose value or tile stack changed. A tile drawn from a cell can overhang it; the renderer knows tile sizes.

```json
{ "change": "grid", "world": "main", "level": "crevasse", "layer": "collision", "rect": [3, 1, 6, 2] }
```

```json
{ "change": "level_renamed", "world": "main", "from": "annex", "to": "wing" }
```

```json
{ "change": "note", "world": "main", "level": null, "id": "pinstatue2" }
```

```json
{ "change": "def_renamed", "kind": "entity", "from": "locked_door", "to": "door" }
```

```json
{ "change": "guide_group", "world": "main", "slug": "ruined" }
```

## Undo and redo

`worlds_core::command::History` keeps two stacks of entries. An entry is a label for the UI, an origin (`user`, or `{ "agent": "<name>" }`) and one command: on the undo stack, the inverse of what was done; on the redo stack, the command that redoes it. Undoing applies the entry's command and moves its inverse to the other stack, so undo and redo are the same operation. Depth is unlimited.

- `push(project, cmd)` applies and records one entry labelled by the command. `push_as(project, cmd, origin)` sets the origin (an agent's batch).
- `begin_gesture(label, origin)`, then any number of `push`, then `end_gesture()`: one entry whose command is a `batch` of the collected inverses in reverse. A brush stroke is one gesture. `cancel_gesture(project)` reverts it instead (Escape during a drag).
- `undo(project)` and `redo(project)` return the changes (or `None` when the stack is empty). Undo first closes an open gesture. If an entry fails to apply, nothing changes and the entry stays.
- A command that changes nothing records nothing. A new entry clears the redo stack.
- `record(label, origin, inverses) -> bool` records one entry for commands the caller applied itself with `Project::apply`, without touching the open gesture: `inverses` in the order the commands were applied (the entry undoes them last first), no-ops dropped, `false` and nothing recorded when none are left.

An agent's batch on the [live API](api.md#history) uses `record`, not a gesture, because the one gesture slot belongs to the user: if the agent shared it, the user's pushes would join the agent's entry, the user's next `begin_gesture` would end the agent's batch, and cancelling the batch would revert the user's edits. So the server collects the inverses of the batch's writes itself and records them as one entry when the batch ends. When the user records an entry while an agent batch is open, the server first records the agent's part so far, so the batch becomes two entries (the undo stack stays in the order things happened, and each entry undoes only its own edits). When the agent's connection drops, the server reverts only the part it hasn't recorded yet.

## Saving

`Project::apply` marks every file its changes name as dirty; `project.is_dirty()` says whether there are any. `worlds_core::save_dirty(project, dir)` writes just those files, without reading them back first, and deletes the ones the project no longer produces (a deleted or renamed level, a layer whose content was emptied, a removed or renamed entity type or rule set, the old file of a renamed layer). A one-cell paint rewrites exactly one file. When the project wasn't loaded from or last saved to `dir`, `save_dirty` does a full save instead. If it fails part way, the dirty set is kept for a retry and the files it did write are remembered, so deleting what they belong to later still removes them.

`project.mark_clean(path)` drops one project-relative file (`"entities/door.json"`) from the dirty set, for a caller that applied a change it read from that very file (the editor's file watcher), so it doesn't show as unsaved; other dirty files stay.

`worlds_core::save_all` (and `save`, which is the same) renders every file and writes those whose bytes changed. It is always right, including after edits made to the model directly rather than through commands; `save_dirty` only knows about commands.

## Errors

`CommandError` says why a command was refused; its message is meant for the agent to read. The kinds: unknown world, level, layer, value, tileset, entity type, field, entity, note, guide group (`unknown_guide_group`), enum, enum value, rule group, rule (`unknown_rule`), macro (`unknown_macro`) or stamp (`unknown_stamp`); a definition slug that is taken; a field slug that is taken (`duplicate_field`: renaming a field to the slug of another); a definition still in use (`in_use`: removing a tileset, macro or stamp something uses, with the first few uses); a level that is in another world; a layer of the wrong kind; a cell outside the layer (with the layer size); a stored grid whose size doesn't match the level; a cell holding a value index the layer doesn't define; a duplicate id or level slug; an invalid argument (bad anchor, index past the end, `note.id` mismatch, `"!"` on a layer that isn't derived, `derived` switched on a layer with content); a failed batch step (`batch`, with the index and the cause); a batch whose rollback failed too (`rollback`: the project may differ from before).

On the wire (the [live API](api.md#errors)) a `CommandError` serializes as `{ "kind": <variant in snake_case>, ...fields, "message": <the Display text> }`: `{ "kind": "unknown_value", "layer": "collision", "value": "lava", "message": "\"lava\" is not a value of layer \"collision\"" }`. Variants with one unnamed field name it (`unknown_level { level }`, `unknown_entity_type { type }`, `unknown_entity { id }`, ...), `enum_` is `enum`, the definition kind of `duplicate_def` and `in_use` is `def_kind` (a stamp in use is `in_use` with `def_kind: "stamp"` and its `layer`), the macro kind of `unknown_macro` is `macro_kind`, `unknown_guide_group` is `{ world, group }`, `unknown_field` and `duplicate_field` are `{ owner, field }` with `owner` naming the list (`entity type "door"` or `project level_fields`), and `invalid` has only its `message`. `batch { label, index, source }` and `rollback { batch, failed }` nest errors in the same form.
