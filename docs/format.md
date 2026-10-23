# Worlds project file format (format 1)

This is the reference for editing a Worlds project by hand. It's written for an AI agent working on the files directly, but it works for people too. The Rust types in `crates/worlds-core/src/model/` are the source of truth. The JSON Schemas in `schema/` are generated from them (`worlds-cli schema <dir>`), and this document describes the same thing in words.

## The short version

- A project is a directory, usually named `<name>.worlds/`. Each file holds one concern and is small enough to read whole. `worlds-cli new <dir> [--name <name>]` creates an empty one (one empty world `main`, plus `schema/`); `worlds-cli schema <dir>` rewrites the schemas.
- Every file is a JSON object that starts with `"$schema"` (the relative path to its schema) and `"format": 1`.
- **Unknown keys are errors.** A typo like `"glpyh"` fails to load with the file name and JSON path (`layers.json: at layers[0].values[2].glpyh: unknown field ...`). Don't add keys that aren't documented here. So is a key that belongs to another kind of the same thing, like `values` on a `tiles` layer or `min` on a `bool` field.
- Definitions are named by **slugs**: `^[a-z][a-z0-9_]*$`, for example `floor_button` or `castle_main`. A slug is also the file or directory name where there is one, and it must match.
- Instances (levels, entities, notes) also have an **id**: exactly 10 characters from `a-z` and `2-7`, for example `k3v7q2mzta`. Make one up at random when you create an instance, and never change it. The key `id` only ever means this; definitions, rules and stamps are named by `slug`.
- References are always by slug or id, never by array index.
- Optional keys with a default are left out when they're at the default. Leaving them out is always fine.
- After editing, run `worlds-cli validate <dir>`. It loads the project and lists every problem (dangling references, row lengths, unknown glyphs, entities outside their level, field type mismatches...).

## Directory layout

```
mygame.worlds/
  project.json                  name, grid size, seed, level fields
  layers.json                   layer definitions, bottom layer first
  tilesets.json                 tileset images and tile sizes
  enums.json                    shared enums
  entities/<slug>.json          one entity type per file
  rules/<auto layer>.json       autotile rules for one auto layer
  rules/macros.json             shared patterns, legends and templates (optional)
  worlds/<world>/world.json     world name, guide groups and world-scope notes and guides
  worlds/<world>/levels/<level>/
    level.json                  level placement, size, tags, fields, per-layer rule state
    <layer>.json                one per int_grid or tiles layer with content (a derived layer's: its overrides)
    entities.json               entity instances (all entities layers)
    notes.json                  level-scope notes, pins, paths and regions
  schema/                       generated JSON Schemas (worlds-cli schema)
  export/                       baked output for the game (generated)
  .worlds/                      editor cache (gitignored)
    editor.lock                 session lock: held (OS file lock) while an editor has the project open; never deleted
    editor.json                 who holds editor.lock: { pid, started, version }
    autosave/manifest.json      the editor's autosave of unsaved edits, for crash recovery (XXH3-64 hex hashes)
    autosave/g<N>/<path>        the autosaved bytes of each modified file, generation N
    autosave.discarded/         the last autosave discarded (one kept)
    server.json, server.lock    the live API's discovery file and its lock (api.md)
    ui.json                     the editor's view state (active level and layer, layer visibility, tool, annotation options)
```

Rules for the layout:

- Only `project.json` is required. A missing `layers.json`, `tilesets.json` or `enums.json` means none.
- A missing `<layer>.json`, `entities.json` or `notes.json` in a level means that layer is empty. An empty level directory with only `level.json` is a valid empty level.
- Inside a level directory, every `*.json` file other than `level.json`, `entities.json` and `notes.json` must be named after an `int_grid` or `tiles` layer. Anything else is a load error.
- So layers of kind `int_grid` or `tiles` can't be named `level`, `entities`, `notes` or `macros`, and an `auto` layer can't be named `macros` (its rules would be `rules/macros.json`). `entities` layers own no file and may use any slug.
- Files and directories starting with `.` are ignored. Non-JSON files (images, readmes) are ignored.

The `$schema` value is the path from the file to `schema/<kind>.json`:

| File | `$schema` |
| --- | --- |
| `project.json` | `schema/project.json` |
| `layers.json`, `tilesets.json`, `enums.json` | `schema/layers.json`, `schema/tilesets.json`, `schema/enums.json` |
| `entities/x.json` | `../schema/entity.json` |
| `rules/x.json` | `../schema/rules.json` |
| `rules/macros.json` | `../schema/macros.json` |
| `worlds/w/world.json` | `../../schema/world.json` |
| `worlds/w/levels/l/level.json` | `../../../../schema/level.json` |
| `worlds/w/levels/l/<int_grid layer>.json` | `../../../../schema/intgrid.json` |
| `worlds/w/levels/l/<tiles layer>.json` | `../../../../schema/tiles.json` |
| `worlds/w/levels/l/entities.json` | `../../../../schema/entities.json` |
| `worlds/w/levels/l/notes.json` | `../../../../schema/notes.json` |

The loader only checks that `$schema` is a string. Saving always writes the value above.

## Common value types

| Name | JSON | Notes |
| --- | --- | --- |
| slug | `"castle_main"` | `^[a-z][a-z0-9_]*$` |
| id | `"k3v7q2mzta"` | 10 characters, `a-z` and `2-7` |
| color | `"#c08040"` or `"#c0804080"` | `#rrggbb` or `#rrggbbaa`, either case |
| cell | `[x, y]` | Grid cell of a layer's grid, `[0, 0]` is the top-left cell of the level. x grows right, y grows down |
| atlas tile | `[col, row]` | Column and row in a tileset image, counted in tiles from the top-left. Not a linear index, so a tileset image growing doesn't renumber anything |
| tile ref | `{ "tileset": "castle_main", "tile": [3, 7] }` | One tile of one tileset |
| flip | `"x"`, `"y"`, `"xy"`, `"t"`, `"xt"`, `"yt"` or `"xyt"` | How a tile is turned. `t` (transpose) applies first: it swaps the tile's x and y (a mirror about its top-left to bottom-right diagonal); then `x` mirrors it left-right and `y` top-bottom, all inside the tile's own square. A quarter turn clockwise is `xt`, a half turn `xy`, three quarters `yt` |

Coordinates: level sizes are in **cells of the project grid** (`project.json` `grid_size`, in pixels). Cells inside a layer (grid rows, tile placements, entity positions) are in cells of **that layer's grid**, which is the project grid unless the layer sets its own `grid_size`. World positions and annotation geometry are in **pixels**.

A layer's grid must divide or be a multiple of the project grid. A level that is 20x5 cells on a 16 px project grid is 320x80 px, so a layer on an 8 px grid has 40x10 cells there, and one on a 32 px grid has 10x3 (the pixel size divided by the layer grid, rounded up).

## project.json

```json
{
  "$schema": "schema/project.json",
  "format": 1,
  "name": "My Game",
  "grid_size": 32,
  "seed": 42,
  "bg_color": "#1a1c2c",
  "collision_layer": "collision",
  "blocking_values": ["solid", "fortified", "ramp_l"],
  "level_fields": [
    { "slug": "biome", "type": "enum", "enum": "biome", "array": true, "default": [] },
    { "slug": "music", "type": "string", "nullable": true }
  ]
}
```

| Key | Type | Required | Meaning |
| --- | --- | --- | --- |
| `name` | string | yes | Display name |
| `grid_size` | integer | yes | Cell size in pixels. Level sizes are in these cells; layers use it unless they set their own |
| `seed` | integer | yes | Project-wide seed mixed into every random choice the rules make |
| `bg_color` | color | no | Default level background |
| `collision_layer` | slug | no | The `int_grid` layer whose values are the collision kinds. The game export reads each level's `open_edges` from it (see [export/](#export-generated)); left out, there are none. Must be an `int_grid` layer |
| `blocking_values` | array of slugs | no, see below | Values of `collision_layer` that block movement. A border cell holding anything else, empty included, is open in the export's `open_edges`; with `[]`, every border cell is open. Each must be a value (not a group) of the collision layer, listed once, and only given with a `collision_layer`. Left out is not the same as `[]`: see the migration rule below |
| `level_fields` | array of field definitions | no | Fields every level has. Same shape as entity fields (see [Field definitions](#field-definitions)). Values go in each `level.json` under `fields` |
| `default_level_size` | `[w, h]` | no | Size in cells of a level made with the editor's New Level command. Left out, the editor uses the most common level size in the project |
| `export_on_save` | bool | no | Default `false`. When `true`, the editor writes [export/](#export-generated) on every save, from its cached bakes |

**Migration rule for `blocking_values`.** When `collision_layer` is set and `blocking_values` is left out, loading fills it with the collision layer's values whose slug is one of `solid`, `fortified`, `breakable`, `ramp_r`, `ramp_l`, in the layer's order, if it has any. The next full save writes that list out. If the layer has none of them the key stays absent, every border cell is open, and validation reports it; write `"blocking_values": []` to say that on purpose. The export always uses this effective list.

## layers.json

Layer definitions, in draw order: **the first layer is drawn first, at the bottom**.

```json
{
  "$schema": "schema/layers.json",
  "format": 1,
  "layers": [
    {
      "slug": "collision",
      "name": "Collision",
      "kind": "int_grid",
      "values": [
        { "slug": "solid", "name": "Solid", "color": "#404040", "glyph": "#", "group": "walls" },
        { "slug": "fortified", "name": "Fortified", "color": "#606060", "glyph": "f", "group": "walls" },
        { "slug": "oneway", "name": "One-way", "color": "#a0a040", "glyph": "-" },
        { "slug": "ramp_l", "name": "Ramp left", "color": "#808080", "glyph": "L" }
      ],
      "groups": [ { "slug": "walls", "name": "Walls", "color": "#505050", "glyph": "w" } ]
    },
    { "slug": "main_tiling", "name": "Main tiling", "kind": "auto", "source": "collision" },
    { "slug": "decor_back", "name": "Decor (back)", "kind": "tiles", "tileset": "castle_main", "opacity": 0.8, "grid_size": 8 },
    { "slug": "objects", "name": "Objects", "kind": "entities" }
  ]
}
```

Layer kinds:

| `kind` | Holds | Stored per level in |
| --- | --- | --- |
| `int_grid` | A named value (or nothing) per cell, like collision | `<layer>.json` as text rows |
| `tiles` | Hand-placed tiles | `<layer>.json` as a sparse list |
| `auto` | Tiles generated by `rules/<layer>.json` from IntGrid layers | Nothing but optional rule state in `level.json`. Generated tiles are never saved; `worlds-cli bake` writes them to `export/` |
| `entities` | Entity instances | the level's `entities.json` |

Notes, pins, paths and regions are not a layer: they live in each level's `notes.json` and in `world.json`, and the editor shows or hides them per tag.

Layer keys:

| Key | Type | Required | Kinds | Meaning |
| --- | --- | --- | --- | --- |
| `slug` | slug | yes | all | Also the per-level file name for `int_grid` and `tiles` |
| `name` | string | yes | all | Display name |
| `kind` | string | yes | all | See above |
| `doc` | string | no | all | Free text for humans and agents |
| `opacity` | number 0..1 | no, default 1 | all | Display opacity |
| `grid_size` | integer | no, default the project's | all but `auto` | This layer's cell size in pixels. Must divide or be a multiple of the project grid. An `auto` layer always uses its source's grid; giving it a different one is a validation problem |
| `values` | array | no | `int_grid` | The values a cell can hold (below) |
| `groups` | array | no | `int_grid` | Named groups of values: `{ "slug", "name", "color"?, "glyph"? }`. Rule patterns can test "any value in this group". A group `glyph` means that group in patterns without a legend entry; it never appears in grid rows |
| `derived` | bool | no, default false | `int_grid` | A grid the rules compute (their [`write`](#output-write) outputs) instead of you painting it. Its level files hold only hand-painted overrides ([derived files](#derived-layers-overrides)). Rules, the editor, the text view and the export read its **effective** value per cell: the override where there is one, else what the rules computed. The `collision_layer` can't be derived (validation) |
| `source` | slug | yes | `auto` | The `int_grid` layer the rules read by default |
| `tile_pivot` | `[x, y]` 0..1 | no, default `[0, 0]` | `auto` | Shifts stamp-mode rule output, in cells |
| `tileset` | slug | no | `tiles` | Default tileset in the tile palette |

Using a key on the wrong kind is a load error (`"values" is not allowed on tiles layers`), even at its default value (`"values": []` or `"derived": false` on a `tiles` layer).

IntGrid values:

| Key | Type | Required | Meaning |
| --- | --- | --- | --- |
| `slug` | slug | yes | The value's name, used everywhere it's referenced |
| `name` | string | yes | Display name |
| `color` | color | yes | Editor color |
| `glyph` | one-character string | yes | Stands for this value in grid rows and rule patterns. Printable ASCII (no space), unique among the layer's value and group glyphs, and none of `.` `\` `"` `*` `+` `!` `@` `[` `]` `\|`: `.` is empty, the others are pattern syntax or need escaping in JSON. A reserved glyph is a load error |
| `group` | slug | no | One of the layer's `groups` |

Value slugs and group slugs share one namespace within a layer, and so do their glyphs. Good glyphs read like the thing: `#` solid, `-` one-way, `~` water, `x` hazard, `/` and `L` for ramps.

## tilesets.json

```json
{
  "$schema": "schema/tilesets.json",
  "format": 1,
  "tilesets": [
    { "slug": "castle_main", "image": "art/castle_main.png", "tile_size": [16, 16] },
    { "slug": "ssw_set5", "image": "art/ssw_set5.png", "tile_size": [32, 32], "spacing": 1, "padding": 2 }
  ]
}
```

| Key | Type | Required | Meaning |
| --- | --- | --- | --- |
| `slug` | slug | yes | |
| `image` | string | yes | PNG path relative to the project directory, `/`-separated. The export rewrites it relative to the export directory |
| `tile_size` | `[w, h]` | yes | One tile in pixels. Can differ from any layer's grid; a 32 px tile on a 16 px grid covers 2x2 cells. Stamp layouts step by this size |
| `spacing` | integer | no, default 0 | Pixels between tiles in the image (only affects where tiles are cut out, never where they are drawn) |
| `padding` | integer | no, default 0 | Pixels between the image edge and the first tile |

Image size isn't stored; it's read from the PNG.

## enums.json

```json
{
  "$schema": "schema/enums.json",
  "format": 1,
  "enums": [
    { "slug": "biome", "values": [ { "slug": "dry" }, { "slug": "flooded", "color": "#3060c0" } ] },
    {
      "slug": "facing",
      "name": "Facing",
      "values": [
        { "slug": "left", "name": "Left", "icon": { "tileset": "castle_main", "tile": [0, 9] } },
        { "slug": "right", "name": "Right" }
      ]
    }
  ]
}
```

| Key | Type | Required | Meaning |
| --- | --- | --- | --- |
| `slug` | slug | yes | Referenced by `enum` field definitions |
| `name` | string | no | Display name |
| `values` | array | yes | `{ "slug", "name"?, "color"?, "icon"? }`. `icon` is a tile ref. Field values store the value slug |

## entities/&lt;slug&gt;.json

One file per entity type. The file name must be `<slug>.json`.

```json
{
  "$schema": "../schema/entity.json",
  "format": 1,
  "slug": "locked_door",
  "name": "Locked Door",
  "category": "Puzzle/Doors",
  "tags": ["object", "puzzle"],
  "doc": "Opens when the player holds needs_item. Footprint is w x h tiles.",
  "size": [1, 3],
  "resizable": [false, true],
  "pivot": [0.5, 1.0],
  "icon": { "tileset": "castle_main", "tile": [3, 7] },
  "color": "#c08040",
  "limit": { "max": 4, "behavior": "discard_oldest" },
  "fields": [
    { "slug": "channel", "type": "int", "default": 0, "min": 0 },
    { "slug": "needs_item", "type": "string", "default": "" },
    { "slug": "consumes", "type": "bool", "default": false },
    { "slug": "facing", "type": "enum", "enum": "facing", "default": "left" }
  ]
}
```

| Key | Type | Required | Meaning |
| --- | --- | --- | --- |
| `slug` | slug | yes | Must match the file name. Instances refer to it as `"type"` |
| `name` | string | yes | Display name |
| `category` | string | no | `/`-separated palette path, e.g. `Puzzle/Doors` |
| `tags` | array of strings | no | Free tags, used by palette filters and `entity_ref` `allowed_tags` |
| `doc` | string | no | Shown in the inspector and on hover. Write what the entity does and what its fields mean |
| `size` | `[w, h]` | yes | Size in cells of the project grid |
| `resizable` | `[bool, bool]` | no, default `[false, false]` | Whether instances may change width / height |
| `pivot` | `[x, y]` 0..1 | no, default `[0, 0]` | Anchor point inside the box. `[0.5, 1.0]` is bottom center |
| `icon` | tile ref | no | Palette and canvas icon |
| `color` | color | yes | Editor color |
| `limit` | object | no (unlimited) | `{ "max": n, "scope"?: "level" (default) / "layer" / "world", "behavior"?: "prevent_adding" (default) / "discard_oldest" / "move_last_one" }` |
| `fields` | array | no | Field definitions, below |

### Field definitions

Entity `fields` and project `level_fields` use this shape:

```json
{ "slug": "exit_to", "type": "entity_ref", "cross_level": true, "allowed_tags": ["object"], "nullable": true }
```

| Key | Type | Applies to | Meaning |
| --- | --- | --- | --- |
| `slug` | slug | all | Key in instance `fields` |
| `name` | string | all | Display name (optional) |
| `type` | string | all | See the table below |
| `array` | bool | all | The value is an array of `type` values. Default false |
| `nullable` | bool | all | `null` is allowed. Default false |
| `default` | value | all | Same shape as an instance value. Used when an instance leaves the field out |
| `min`, `max` | number | `int`, `float` | Inclusive bounds |
| `regex` | string | `string`, `text` | Pattern the **whole** value must match (see below) |
| `enum` | slug | `enum` | A project enum. Give this or `values` |
| `values` | array of slugs | `enum` | Inline allowed values. Give this or `enum` |
| `allowed_types` | array of entity slugs | `entity_ref` | Allowed target types |
| `allowed_tags` | array of strings | `entity_ref` | Also allowed: any target whose type has one of these tags. With neither list set, any entity is allowed |
| `cross_level` | bool | `entity_ref` | The target may be in another level. Default false |
| `extensions` | array of strings | `path` | Allowed file extensions without the dot |
| `tileset` | slug | `tile` | Restrict to one tileset |
| `doc` | string | all | What the field means |
| `display` | object | all | `{ "canvas"?: bool, "arrow"?: bool, "world"?: bool }`: show the value on the canvas, draw an arrow to the target (`entity_ref` and `point` only), show in the world view |

`regex` uses the syntax of Rust's `regex` crate (no look-around or backreferences); `(?i)` at the start ignores case. Validation checks that it matches the whole value, as if written `^(?:…)$`: `brass` doesn't accept `brass_key`. The pattern is checked on its own first, so an unbalanced one such as `a)|(b` is reported as not compiling rather than closing that implied group, and an extended-mode pattern ending in a comment (`(?x)key # note`) works. Arrays are checked per element, and defaults too. A pattern that doesn't compile is a validation problem at the field's `regex`.

An option of another type (`min` on a `bool`, `tileset` on a `string`, even `"cross_level": false` on a `string`) is a load error, as is an `enum` field with both or neither of `enum` and `values`. The JSON Schema says the same.

Field types and the JSON value each one takes:

| `type` | Value | Example |
| --- | --- | --- |
| `int` | integer | `3` |
| `float` | number | `1.5` |
| `bool` | `true` / `false` | `true` |
| `string` | single-line string | `"brass_key"` |
| `text` | multi-line string | `"line one\nline two"` |
| `color` | color string | `"#ff8080"` |
| `enum` | value slug | `"left"` |
| `point` | cell in the same level, of the entity's layer grid (the project grid for level fields) | `[4, 2]` |
| `entity_ref` | entity instance id | `"doorshaft2"` |
| `path` | file path relative to the project | `"scripts/door.gd"` |
| `tile` | tile ref | `{ "tileset": "castle_main", "tile": [4, 4] }` |
| `json` | anything | `{ "a": 1 }` |

With `"array": true` the value is a JSON array of those, for example `[[1, 1], [2, 1]]` for an array of points. `null` is only valid when `nullable` is true (an array field holding `[]` is not null). On an array field, `nullable` also lets elements be `null` (`["doorshaft2", null]`).

A field an instance leaves out takes the definition's `default`. Validation checks a `point` or `entity_ref` default in each level or entity that inherits it, since whether the point is inside the level or the target exists (in the same level, unless `cross_level`) depends on where it is used.

## rules/&lt;layer&gt;.json

The autotile rules for one `auto` layer. The file name is the layer slug. The rules turn IntGrid cells into tiles: for every cell of the level, each group that runs there tries its rules in order; a rule whose pattern matches around the cell draws its output there. Groups run in [passes](#passes). Besides these **pattern rules**, a rule can be a **region rule**, which fires at places inside regions of cells (rooms, say: [Region rules](#region-rules)); and instead of drawing, a rule can **write** a value into a derived grid that later rules read ([`write`](#output-write)).

```json
{
  "$schema": "../schema/rules.json",
  "format": 1,
  "layer": "main_tiling",
  "groups": [
    {
      "slug": "walls",
      "name": "Walls",
      "rules": [
        {
          "slug": "wall_top_edge",
          "match": {
            "legend": { ".": "!solid" },
            "rows": [
              "***",
              ".#.",
              "###"
            ]
          },
          "symmetry": ["mirror_x"],
          "out": { "tileset": "castle_main", "tiles": [[15, 0], [16, 0], [17, 0], [18, 0]] },
          "break": true
        },
        {
          "slug": "ledge_lip",
          "match": { "rows": ["*", "-", "."] },
          "out": { "tileset": "castle_main", "tiles": [[2, 0, 2, 1]], "tile_mode": "stamp", "pivot": [0.5, 0.0] }
        },
        { "use": "blob47", "slug": "moss", "tileset": "castle_main", "origin": [0, 8] }
      ]
    },
    { "slug": "flooded_decor", "mode": "optional", "rules": [] }
  ]
}
```

Top level:

| Key | Type | Required | Meaning |
| --- | --- | --- | --- |
| `layer` | slug | yes | The `auto` layer. Must match the file name |
| `stamps` | array | no | Named tile rectangles that outputs can place by name ([Stamps](#stamps)) |
| `groups` | array | yes | Rule groups, run in this order |

### Groups

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `slug` | slug | required | Unique in the file. Levels switch optional groups on by this slug |
| `name` | string | none | Display name |
| `mode` | string | `"always"` | `"always"`: runs in every level. `"optional"`: runs only in levels that list it in `level.json` `layers.<layer>.optional_groups`. `"off"`: never runs (kept so it can be switched back on) |
| `pass` | integer ≥ 0 | `0` | The [pass](#passes) this group runs in. Keep the file in pass order: a group whose `pass` is lower than an earlier group's is a validation problem |
| `color` | color | none | Editor color |
| `icon` | tile ref | none | Editor icon |
| `requires_level_field` | object | none | Only run in levels whose enum field matches: `{ "field": "<level field slug>", "values": ["flooded"], "mode"?: "any" (default) / "all" }`. `any`: the field holds at least one of `values`. `all`: it holds every one. In a level that has the field, this decides on its own (an `optional` group whose requirement holds runs without being listed). It never switches on an `off` group. |
| `requires_level_tag` | object | none | Only run in levels with these tags (`level.json` `tags`): `{ "tags": ["ruined"], "mode"?: "any" (default) / "all" }`. `tags` is a non-empty list of non-empty strings without repeats (validation). `any`: the level has at least one; `all`: every one |
| `rules` | array | required | Rules and template references (`{ "use": ... }`, below), in order |

Whether a group runs in a level: an `off` group never does. Otherwise a `requires_level_tag` that fails stops it; else a `requires_level_field` on a field the level has decides (as above); else a `requires_level_tag` that holds runs it (an `optional` group too, without being listed); else an `always` group runs and an `optional` one runs only if the level lists it. World annotations (regions) are not rule inputs.

```json
{ "slug": "rubble", "pass": 1, "requires_level_tag": { "tags": ["ruined"], "mode": "any" }, "rules": [] }
```

### Passes

A pass is all the groups with the same `pass`. A layer bakes its passes in increasing order; inside a pass the groups run in file order (the engine sorts groups by `pass`, stably, so a file out of pass order still bakes, but validation reports it) and each group's rules in order.

- **Locks** (`break`, and the lock an opaque tile puts on its anchor) last for the rest of their pass only: a later pass starts with no locked anchors.
- **Derived grids**: a pass sees each derived grid as it was when the pass started: the overrides, plus what earlier passes of this layer and every upstream layer wrote. What a pass writes becomes visible to the next pass, so inside a pass the order of rules never changes what another rule reads.
- **Occupancy** ([Stamps](#stamps)) belongs to the layer and is kept across all its passes.
- **Draw order: passes stack bottom to top.** Pass 0's tiles are drawn first (lowest), the last pass on top. Inside a pass, earlier rules draw on top of later ones, as before: each rule's tiles by anchor row-major, then variant, then the alternative's tile order. A layer that only uses pass 0 draws exactly as before.

The typical use is a pass that writes marks into a derived grid and a later pass that draws stamps on them (see [Region rules](#region-rules)).

### Rules

A rule is one of two kinds, and either draws or writes:

- a **pattern rule** has `match`: it fires at each cell where its pattern holds;
- a **region rule** has `region` and `place`: it fires at places inside regions of cells ([Region rules](#region-rules));
- it **draws** with `out` ([Output](#output-out)), or **writes** a derived grid with `write` ([`write`](#output-write)).

Exactly one of `match` and `region`, exactly one of `out` and `write`, and `place` exactly when there is `region`: anything else is a load error naming the keys (`a rule has either "out" (draw) or "write" (write a derived grid), not both`). `null` is never a value in a rule or a template reference, nor in the `match`, `require`, `region`, `place`, `out` and `write` objects inside them: leave the key out instead (`"modulo": null` is a load error, and the schema refuses it too). A group's optional keys without a default (`name`, `color`, `icon`, `requires_level_field`, `requires_level_tag`) do take `null`, meaning left out. The keys either kind takes, in the order they are written:

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `slug` | slug | required | Unique within the file, across rules, template references and the rules references expand to (below). Also part of the hash behind the rule's random choices, so renaming a rule re-rolls it |
| `active` | bool | `true` | |
| `out` | output | | What to draw ([Output](#output-out)). Give this or `write` |
| `write` | object | | What to write into a derived grid ([`write`](#output-write)). Give this or `out` |
| `break` | bool | `false` | Once this rule fires at an anchor, later rules of the same [pass](#passes) skip that anchor |
| `salt` | integer | `0` | Mixed into this rule's random choices. Leave it out; set it only to re-roll a rule without renaming it. |

A pattern rule also takes (key order: `slug`, `active`, `match`, `require`, `symmetry`, `out` or `write`, `chance`, `break`, `modulo`, `checker`, `perlin`, `out_of_bounds`, `salt`):

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `match` | pattern | required | What must surround the cell (below) |
| `require` | pattern, or array of patterns | none | More patterns that must also hold, anchored on the same cell, usually on other grids (below). One pattern, or a non-empty list (all must hold); a list of one is saved as the pattern |
| `symmetry` | array | `[]` | Extra variants to try: `"mirror_x"` (flipped left-right), `"mirror_y"` (top-bottom), `"rotate"` (the 90-degree turns). The output is turned to match the variant that fired (below) |
| `chance` | number 0..1 | `1` | Probability a matching cell is drawn |
| `modulo` | object | none | `{ "x": n, "y": m, "x_offset"?: 0, "y_offset"?: 0 }`: only cells where `(x - x_offset) % n == 0` and `(y - y_offset) % m == 0`, counted from the level's top-left |
| `checker` | string | `"none"` | `"horizontal"` or `"vertical"`: checkerboard spacing on top of `modulo` |
| `perlin` | object | none | Noise gate: `{ "active": bool, "seed": int, "scale": number, "octaves": int }`. Kept when inactive |
| `out_of_bounds` | string | none | What pattern cells outside the level read as: `"."` (empty) or a value slug. Left out: a pattern cell off the level never matches |

A region rule takes `slug`, `active`, `region`, `place`, `out` or `write`, `break` and `salt`, in that order. A pattern-rule key on it is a load error (`"chance" is not allowed on a region rule`).

Randomness is deterministic: every random choice hashes the project `seed`, the level `id`, the layer slug, the level's optional per-layer `seed`, the rule `slug` and `salt`, the cell, and which decision it is. The same project bakes to the same bytes, and a one-cell edit only changes tiles near it.

**Symmetry variants.** Each variant reads the pattern (and `require`) turned, and draws the output turned the same way. Variants are tried in this order, and a variant that would read exactly the same cells as an earlier one is skipped:

| # | Variant | Tried with | Pattern cell `(x, y)` from the anchor is read at | Output flip |
| --- | --- | --- | --- | --- |
| 0 | as written | always | `(x, y)` | none |
| 1 | mirrored left-right | `mirror_x`, or `rotate` with a mirror | `(-x, y)` | `x` (1) |
| 2 | mirrored top-bottom | `mirror_y`, or `rotate` with a mirror | `(x, -y)` | `y` (2) |
| 3 | half turn | both mirrors, or `rotate` | `(-x, -y)` | `xy` (3) |
| 4 | quarter turn clockwise | `rotate` | `(-y, x)` | `xt` (5) |
| 5 | quarter turn counterclockwise | `rotate` | `(y, -x)` | `yt` (6) |
| 6 | transposed | `rotate` with a mirror | `(y, x)` | `t` (4) |
| 7 | anti-transposed | `rotate` with a mirror | `(-y, -x)` | `xyt` (7) |

So `rotate` alone gives the four turns and `rotate` with any mirror all eight orientations. y grows down; the numbers in brackets are the export's flip bits. Stamp layouts and pixel offsets turn with the variant, about the anchor cell's center. `rotate` needs square tiles: the output's tileset, and every tileset a level substitutes for it, must have `tile_size` with equal width and height.

### Patterns (`match`)

```json
"match": { "grid": "collision", "legend": { ".": "!solid" }, "rows": ["***", ".#.", "###"] }
```

| Key | Type | Required | Meaning |
| --- | --- | --- | --- |
| `grid` | slug | no, default the auto layer's `source` | The `int_grid` layer to read. It must be on the same grid size as the auto layer |
| `legend` | object or string | no | Glyph to cell test, for glyphs that don't already mean what you want (below). A string names a legend in `rules/macros.json` |
| `rows` | array of strings, or string | yes | The pattern, top row first. The cell being decided (the **anchor**) is the one marked `@`, else the **center**: without `@`, width and height must be odd. A string names a pattern in `rules/macros.json` |

```json
"match": { "legend": { "@": "!solid" }, "rows": ["@#", "##"] }
"match": { "legend": "walls", "rows": "edge_top" }
```

The first is a 2x2 block: solid to the right of, below and diagonally below an anchor that isn't solid. At most one cell is `@`.

A cell test is one of:

| Test | Matches |
| --- | --- |
| `"*"` | anything (don't care) |
| `"+"` | any value (not empty) |
| `"."` | empty |
| `"solid"` | that value, or any value in that group |
| `"!solid"` | anything except that value / group, including empty |
| `"[solid\|oneway]"` | any of the members: `.`, `+`, value or group slugs, separated by `\|` |
| `"![solid\|oneway\|.]"` | none of the members |

How a glyph in `rows` is read, first match wins:

1. the pattern's `legend`;
2. the built-ins: `*` don't care, `+` any value, `.` empty, `@` the anchor (don't care);
3. a value glyph of the `grid` layer: that value (`#` is `solid` if the layer says so);
4. a group glyph of the `grid` layer: any value in that group.

So most patterns need no legend at all: `[".#."]` already means "empty, solid, empty". The legend is for negations (`"S": "!solid"`), alternatives (`"w": "[solid|fortified]"`), groups without a glyph, overriding a built-in (the example above makes `.` mean "not solid" instead of "empty"), and giving the anchor a test (`"@": "!solid"`). Any other glyph is a validation problem. Legend keys can't be `!` `[` `]` `|` `\` `"`, and `@` only in a pattern whose rows contain `@`. A cell test that isn't one of the forms above (`"[solid|!oneway]"`, `"[solid"`) is a load error.

### Second pattern (`require`)

```json
{ "slug": "torch_on_wall",
  "match": { "legend": { "@": "." }, "rows": ["#", "@"] },
  "require": { "grid": "decor_marks", "legend": { "t": "torch" }, "rows": ["t"] },
  "out": { "tileset": "ssw_set5", "tiles": [[3, 9]] } }
```

`require` is a pattern like `match` (same keys and rules, `grid` defaulting to the auto layer's `source`, on the same grid size), anchored on the same cell, or a list of them. The rule matches where all of them hold. Symmetry variants turn them all together, and `out_of_bounds` applies to every one.

```json
{ "slug": "pillar_stamp",
  "match": { "grid": "pillar_marks", "rows": ["p"] },
  "require": [
    { "legend": { ".": "[.|oneway]" }, "rows": ["..", "..", "..", "@."] },
    { "grid": "decor_marks", "rows": ["."] }
  ],
  "out": { "stamp": "pillar", "avoid_overlap": true } }
```

### Region rules

A region rule selects regions of cells by their size and fires at places along their edges or across their interior, with a spacing strategy:

```json
{ "slug": "big_room_pillars",
  "region": { "cells": ".", "min_width": 15 },
  "place": { "along": "floor", "every": 6, "margin": 3, "align": "center" },
  "write": { "grid": "pillar_marks", "value": "pillar_base" } }
```

That writes `pillar_base` at evenly spaced floor cells of every room at least 15 cells wide, 6 apart, centered, keeping 3 cells clear at each end of the floor.

`region`, the selector:

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `grid` | slug | the auto layer's `source` | The `int_grid` layer read (a derived one by its effective values). On the auto layer's grid size (validation) |
| `cells` | cell test | required | Which cells belong to regions, as a [cell test](#patterns-match): `"."`, `"+"`, `"solid"`, `"!solid"`, `"[.\|oneway]"`, `"![solid\|fortified]"`. `"*"` is a validation problem (every cell would be one region) |
| `min_width`, `max_width` | integer ≥ 1 | none | Bounding-box width in cells, inclusive |
| `min_height`, `max_height` | integer ≥ 1 | none | Bounding-box height, inclusive |
| `min_area`, `max_area` | integer ≥ 1 | none | Number of cells, inclusive |

A `min_*` greater than its `max_*`, and a value or group in `cells` the grid doesn't have, are validation problems; a bound of 0 is a load error.

**Definitions** (on the cells of the region grid; a cell outside the level is never a region cell and never an edge neighbour):

- **Region**: a 4-connected component of cells passing `cells`. Its bounding box is `[min x, min y, max x − min x + 1, max y − min y + 1]`; width and height are the box's, area the cell count. A region is **selected** when every given bound holds.
- **Edge cells**: region cell `(x, y)` is on the `floor` when its neighbour `(x, y+1)` is inside the level, isn't a region cell and (with `place.against`) passes `against`; `ceiling` looks at `(x, y−1)`, `wall_left` at `(x−1, y)`, `wall_right` at `(x+1, y)`. A region's border at the level edge is no edge: a room whose floor is the level's bottom row has no floor there, and its open ends at the left and right edges of the level are not walls.
- **Runs**: floor and ceiling cells form runs, maximal sequences of consecutive cells in one row; wall cells form runs down one column. Runs are listed row-major by their first cell and never cross regions.
- **Interior**: every region cell (for `across`).

`place`:

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `along` | `"floor"`, `"ceiling"`, `"wall_left"`, `"wall_right"` | | Place on each run of that edge, independently |
| `across` | `"interior"` | | Place on a lattice over the region's bounding box, kept where the cell is a region cell. Exactly one of `along` and `across` (a load error otherwise) |
| `spacing` | `"even"`, `"spread"`, `"random"` | `"even"` | The 1D strategy (below) |
| `every` | integer ≥ 1, or `[x, y]` with `across` | `1` | The pitch (`even`) or the minimum distance (`spread`, `random`), in cells |
| `margin` | integer ≥ 0, or `[x, y]` with `across` | `0` | Cells kept clear at each end of a run (of the bounding box, per axis, with `across`) |
| `align` | `"start"`, `"center"`, `"end"` | `"center"` | `even` only: where the leftover goes. Start is left (floor, ceiling) or top (walls) |
| `jitter` | integer ≥ 0 | `0` | `random` only: extra random distance `0..=jitter` added to each step |
| `min_length` | integer ≥ 1 | `1` | `along` only: runs shorter than this (before `margin`) are skipped |
| `against` | cell test | none (any non-region cell) | `along` only: the neighbour across the edge must also pass this |

`[x, y]` forms with `along`, and `min_length` or `against` with `across`, are load errors; `align` (other than its default) with a spacing other than `even`, or `jitter` with one other than `random`, is a validation problem.

**The 1D strategies.** A run (or one axis of a bounding box) covers positions `a..=b`. With margin `m`: `lo = a + m`, `hi = b − m`, `U = hi − lo + 1`. If `U < 1` nothing is placed. Otherwise, with `k = (U − 1) div every + 1`:

- `even`, a fixed pitch: slack `s = (U − 1) − (k − 1) × every`, offset `o = 0` (`start`), `s div 2` (`center`; an odd slack leans to the start) or `s` (`end`). Positions `lo + o + i × every` for `i < k`.
- `spread`, as many as fit at least `every` apart, spread from end to end: with `k = 1` the position is `lo + (U − 1) div 2`; otherwise `p_i = lo + floor((2 i (U − 1) + (k − 1)) / (2 (k − 1)))`, which is `i (U − 1) / (k − 1)` rounded half up: `p_0 = lo`, `p_{k−1} = hi`, every gap at least `every`.
- `random`, seeded: `p_0 = lo + h(lo, PHASE) mod every`, then `p_{i+1} = p_i + every + h(p_i, STEP) mod (jitter + 1)`, stopping at the first `p > hi`. `h(p, s)` hashes the layer seed, the rule's slug and `salt`, the cell whose run coordinate is `p` (its other coordinate is the run's row or column; with `across`, x positions use row `bbox.y` and y positions column `bbox.x`) and the stream `s`. Gaps are `every..=every + jitter`; `salt` re-rolls.

With `across`, the x positions come from the strategy over `bbox.x ..= bbox.x + w − 1` with `every[0]` and `margin[0]`, the y positions over the box's rows with `every[1]` and `margin[1]` (a single number applies to both axes), and the candidates are every `(x, y)` pair that is a region cell.

Worked examples (`every 6, margin 3, align center` unless said):

| Case | Run | `lo..hi`, `U` | `k`, slack | Positions |
| --- | --- | --- | --- | --- |
| A 64-wide floor | 0..63 | 3..60, 58 | 10, 3 | 4, 10, …, 58 (offset 1: leans left) |
| A 25-wide floor | 1..25 | 4..22, 19 | 4, 0 | 4, 10, 16, 22 (symmetric about 13) |
| A 10-wide ledge | 0..9 | 3..6, 4 | 1, 3 | 4 |
| A 15-wide room | 0..14 | 3..11, 9 | 2, 2 | 4, 10 |
| A 14-wide room | 16..29 | — | — | not selected by `min_width: 15`; with `min_width: 14`: 19..26, `k` 2, slack 1 → 19, 25 |
| Margin larger than the run | 0..4 | 3..1, −1 | — | none |
| A run of exactly `2m + 1` | 0..6 | 3..3, 1 | 1, 0 | 3 |
| A floor broken by a pit (floor x1..6, pit x7..9, floor x10..15) | two runs | each on its own | | x1..6: `U` 0, none; x10..15: none; the pit's bottom is a third run |
| `spread`, run 0..29 | 0..29 | 3..26, 24 | 4 | 3, 11, 18, 26 |
| `across: interior`, bbox x0..29 y1..6, `every [6, 3]`, `margin [3, 1]` | | x: 5, 11, 17, 23; y: 2, 5 | | the 8 pairs that are region cells |

**Firing.** A region rule's **candidates** are the positions over every selected region (all of them, without repeats, row-major). When its turn comes, the rule fires at each candidate anchor in row-major order exactly like a pattern rule that matched there with its first variant (as written, not turned): it is skipped where an earlier rule of the pass locked the anchor; with `out`, the alternative is picked by weight, `random_offset_px` rolls, `alpha`, `offset_px`, stamps, `tile_mode` and `avoid_overlap` apply as for any rule, and so does the opaque-tile lock; with `write`, it writes; `break` locks the anchor for the rest of the pass. There is no chance or noise gate and no other variant.

### Template references (`use`)

A group's `rules` holds rules and references to templates, in evaluation order. A reference is an object with `use`; it stands for its template's rules (from `rules/macros.json`, or the built-in `blob47`), expanded in place:

```json
{ "use": "frame6", "slug": "walls", "tileset": "ssw_set3_main_lev_build", "origin": [0, 0],
  "tiles": { "lip": [[1, 0, 5, 1]], "fill": [[2, 2, 3, 3]] } }
{ "use": "blob47", "slug": "moss", "tileset": "ssw_set4_main_lev_build", "origin": [0, 8],
  "legend": { "#": "fortified", "S": "!fortified" } }
```

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `use` | slug | required | The template: a name in `rules/macros.json` `templates`, or a built-in |
| `slug` | slug | the `use` value | Names the reference. Unique within the file like a rule slug; a second reference to the same template needs its own |
| `tileset` | slug | none | Tileset for the template's rules that don't name their own. Required unless every rule used names its own. A level's tileset substitutions apply afterwards, as for any rule |
| `origin` | `[col, row]` | `[0, 0]` | Added to the tile coordinates of every template rule that doesn't name its own tileset |
| `tiles` | object | `{}` | Per template rule slug: replacement tile alternatives, relative to `origin` (absolute for a rule that names its own tileset) |
| `skip` | array of slugs | `[]` | Template rules to leave out |
| `legend` | object | `{}` | Merged over every expanded rule's `match` and `require` legends (so `{ "#": "fortified" }` makes the template draw on `fortified` instead) |
| `active` | bool | `true` | Off: every expanded rule is inactive |

A key that belongs to a rule (`match`, `out`, `chance`, ...) on a reference, or a reference key (`use`, `origin`, ...) on a rule, is a load error.

**Expansion.** Each template rule `r` not in `skip`, in template order, becomes a rule with:

- slug `<reference slug>__<r's slug>` (`walls__lip`). It must not collide with any other rule, reference or expanded slug in the file;
- rows and legends resolved (named patterns and legends looked up); legend = the template's `legend`, then `r`'s own, then the reference's `legend` (later entries win), for `match` and `require` alike;
- `out.tileset` = the reference's `tileset`, and tiles = the reference's `tiles` for `r` if given, else `r`'s own, moved by `origin`. A template rule that names its own `out.tileset` keeps it, and its tiles (and any override) are absolute;
- every `require` pattern resolved the same way as `match`;
- `active` = `r.active` and the reference's `active`; everything else (`break`, `chance`, `salt`, `symmetry`, ...) as in `r`.

The random choices of an expanded rule hash its expanded slug and `salt`, so moving `origin` or switching tileset doesn't re-roll them; renaming the reference or the template rule does. Renaming the template with `rename_macro` doesn't: a reference without a `slug` gets its old name as `slug` ([commands.md](commands.md#rename_macro)). In code, `worlds_core::Project::expanded_rules(layer)` returns exactly what runs.

### Output (`out`)

Either tiles from a tileset, or a named stamp. Mixing the two is a load error.

```json
"out": { "tileset": "castle_main", "tiles": [[15, 0], [16, 0], { "rect": [18, 0, 1, 1], "weight": 3 }] }
"out": { "tileset": "castle_main", "tiles": [[2, 0, 2, 1]], "tile_mode": "stamp", "pivot": [0.0, 1.0] }
"out": { "stamp": "pillar", "avoid_overlap": true }
```

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `tileset` | slug | | Tileset to draw from. Each rule names its own, so one layer can mix tilesets |
| `tiles` | array of alternatives | | One alternative is picked per cell, by weight |
| `tile_mode` | string | `"stack"` | `"stack"`: every tile of the chosen alternative is drawn at the anchor cell, in list order (normally an alternative is one tile). `"stamp"`: the alternative's tiles keep their atlas layout (below) |
| `pivot` | `[x, y]` | `[0, 0]` | Stamp mode only: which point of the alternative's pixel bounding box sits on the same point of the anchor cell, 0..1 per axis |
| `stamp` | slug | | A stamp from this file's [`stamps`](#stamps) |
| `avoid_overlap` | bool | `false` | Skip a placement whose footprint has an occupied cell (with tiles or a stamp; see [Stamps](#stamps)) |
| `alpha` | number 0..1 | `1` | Opacity |
| `offset_px` | `[x, y]` | `[0, 0]` | Fixed pixel offset |
| `random_offset_px` | object | none | `{ "x": [min, max], "y": [min, max] }`, inclusive pixel ranges; either may be left out |

A tile alternative is one of these (saving picks the shortest that fits):

| Form | Meaning |
| --- | --- |
| `[col, row]` | One tile, weight 1 |
| `[col, row, w, h]` | A `w` x `h` rectangle of tiles, listed row by row, weight 1 |
| `{ "rect": [col, row, w, h], "weight": n }` | A rectangle with a weight (`w` = `h` = 1 for one tile) |
| `{ "stack": [[col, row], ...], "weight": n }` | Any list of tiles in drawing order: non-rectangular, repeated, or empty (an empty alternative draws nothing but still counts as a match). `weight` may be left out |

Anything else is a load error that lists these forms.

**Stamp layout.** In stamp mode the alternative's tiles are laid out as in the atlas, one tileset tile apart (the tileset's `tile_size`, never its `spacing`), not one layer cell apart. The whole stamp's pixel box is then placed so that its `pivot` point lands on the same point of the anchor cell: `[0, 0]` puts the stamp's top-left on the cell's top-left, `[0.5, 1]` stands its bottom center on the cell's bottom center. The layer's `tile_pivot` shifts that by a fraction of a cell. With 32 px tiles on a 16 px layer a 2x2 stamp covers 64x64 px. A mirrored variant mirrors the layout about the anchor cell's center.

### Output (`write`)

Instead of `out`, a rule (pattern or region) can write a value into a derived grid where it fires:

```json
"write": { "grid": "pillar_marks", "value": "pillar_base" }
```

| Key | Type | Meaning |
| --- | --- | --- |
| `grid` | slug | A derived `int_grid` layer (`"derived": true`), on the auto layer's grid size (validation) |
| `value` | slug | A value (not a group) of that layer (validation) |

- The computed values start empty at every bake and are never saved. **The first write to a cell in a pass wins**: later writes to that cell in the same pass are dropped, but still count as the rule firing (it locks with `break`, it shows in the rule's firings). A later pass may overwrite the cell (its own first write wins again).
- **One writer per derived grid**: every `write` naming a grid must be in one auto layer's rules file. A second layer writing it is a validation problem at its rule's `write.grid` ("pillar_marks is already written by layer pillars"), and the bake refuses it.
- **Bake order across layers**: an auto layer that reads a derived grid another layer writes (by any pattern's `grid`, a region's `grid`, or its `source`) bakes after that writer; otherwise layers bake in `layers.json` order. Two layers that read each other's derived grids (directly or around a longer loop) are a validation problem on `layers.json` ("auto layers a and b read each other's derived grids: a → marks_b → b → marks_a → a") and can't bake. A reader in another layer sees the writer's final effective grid; a reader in the writer's own layer sees the grid as its pass started (see [Passes](#passes)).
- Where a derived grid has hand-painted overrides, they win over what the rules write ([derived files](#derived-layers-overrides)).
- A pattern rule that writes fires once per anchor however many symmetry variants match there. A write draws nothing, so the opaque-tile lock and occupancy don't apply to it; `break` does.

Templates can't hold region rules or rules that write (a load error in `rules/macros.json`).

### Stamps

Named rectangles of tiles that outputs place by name, with an optional occupancy footprint:

```json
"stamps": [ { "slug": "pillar", "tileset": "ssw_set5_main_lev_build", "rect": [34, 18, 2, 4], "pivot": [0, 1.0], "occupies": true } ]
"out": { "stamp": "pillar", "avoid_overlap": true }
```

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `slug` | slug | required | Referenced by `out.stamp` |
| `tileset` | slug | required | |
| `rect` | `[col, row, w, h]` | required | In tiles |
| `pivot` | `[x, y]` | `[0, 0]` | Which point of the stamp's pixel box sits on the same point of the anchor cell |
| `occupies` | bool | `false` | After each placement of this stamp, mark its footprint occupied, so `avoid_overlap` outputs skip it |

A stamp is placed by the bake like a stamp-mode output of one alternative ([Stamp layout](#output-out)).

**Occupancy.** Each auto layer has one occupancy grid per level, in cells of the layer's grid, all free when the layer's bake starts and kept across all its passes (layers don't share it).

- The **footprint** of a placement is every in-level cell whose **center** lies inside the pixel box of at least one tile it draws. A tile's box is half-open, `[x0, x0 + tw) × [y0, y0 + th)` in level pixels, after the pivot, the layer's `tile_pivot`, the variant's turn, `offset_px` and the rolled `random_offset_px` (`tw × th` is the tileset's tile size, swapped by a transpose). On a grid of `g` px, cell `(cx, cy)` is in it when `2·x0 ≤ 2·cx·g + g < 2·(x0 + tw)`, and the same for y. So a 64×128 stamp with pivot `[0, 1]` on a 32 px grid at anchor `(x, y)` covers cells `x, x+1 × y−3..y`; with pivot `[0.5, 1]` (tiles shifted half a cell) it covers `x−1, x`, and two such stamps 2 cells apart touch without colliding. A tile the size of the grid with pivot `[0, 0]` covers exactly its cell; a 32 px tile on a 16 px grid covers 2×2 cells; a 16 px tile on a 32 px grid at px `(0, 0)` covers no cell (its box doesn't reach the cell's center), at px `(8, 8)` it covers one.
- `occupies: true` marks a placement's footprint occupied after it is placed.
- `avoid_overlap: true` (on any output): before placing a variant's tiles at an anchor, if any cell of their footprint is occupied, that variant is skipped and draws nothing. A rule all of whose matching variants were skipped at an anchor did not fire there: no lock, no write.
- Everything happens in the sweep order: rules in run order, then anchors row-major, then variants. Of two overlapping placements of one rule, the first in reading order wins.
- Locks and occupancy are independent: a locked anchor is skipped before occupancy is looked at, a skipped placement doesn't lock, locks never mark occupancy and occupancy never locks. A placement without `avoid_overlap` is drawn whether or not its cells are occupied.

Stamps that avoid each other are best kept in a small pass of their own: a pass that uses occupancy is re-baked whole when anything in it changes.

### Not in format 1 yet

A template can't use another template, and templates can't hold region rules or rules that write a derived grid.

## rules/macros.json (optional)

Shared pattern pieces and templates, used by name from any rules file: `match.rows` / `require.rows` name a pattern, `match.legend` / `require.legend` a legend, and a group entry `{ "use": "<name>", ... }` a template (see [Template references](#template-references-use)).

```json
{
  "$schema": "../schema/macros.json",
  "format": 1,
  "patterns": { "edge_top": ["***", ".#.", "###"], "lip_above": ["***", "*S*", "*#*"] },
  "legends": { "walls": { "#": "solid", "S": "!solid" } },
  "templates": {
    "frame6": {
      "name": "Framed wall, 6 columns",
      "doc": "Lip row on top, then a fill block. origin = the top-left lip tile.",
      "legend": "walls",
      "rules": [
        {
          "slug": "lip",
          "match": { "rows": "lip_above" },
          "out": { "tiles": [[1, 0, 4, 1]] },
          "break": true,
          "out_of_bounds": "solid"
        },
        {
          "slug": "fill",
          "match": {
            "rows": [
              "*#*",
              "###",
              "*#*"
            ]
          },
          "out": { "tiles": [[1, 2, 4, 3]] },
          "break": true,
          "out_of_bounds": "solid"
        }
      ]
    }
  }
}
```

| Key | Type | Meaning |
| --- | --- | --- |
| `patterns` | object | Name to pattern rows |
| `legends` | object | Name to legend (glyph to cell test) |
| `templates` | object | Name to template (below) |

A template:

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `name` | string | none | Display name, for the editor's template library |
| `doc` | string | none | What it draws and how its tiles are laid out |
| `legend` | object or string | none | Default legend for every rule (an object, or a name in `legends`); a rule's own legend entries win |
| `rules` | array | required | Rules, in evaluation order, with slugs unique within the template |

Template rules are ordinary pattern [rules](#rules) that draw, with two differences: `out.tileset` may be left out (the reference supplies it), and then the tile coordinates are relative to the reference's `origin`. A rule that names its own `out.tileset` is *fixed*: it always draws from that tileset at those absolute tiles. A template can't contain a `use` entry, a region rule (`region`, `place`) or a rule that writes (`write`): those are load errors. Templates don't know which layer uses them, so their rules are checked against each referencing layer, where they are used. Named patterns and legends resolve the same way inside templates.

### Built-in templates

Names that every project can `use` without defining them. A project template can't take one of these names. They are frozen: a changed layout would ship under a new name.

**`blob47`**: the standard 47-piece blob terrain, laid out like Godot's "3x3 minimal" 12x4 terrain template, so tiles drawn for Godot work as they are. `origin` is the block's top-left tile. Its legend is `{ "#": "+", "S": "." }` (any value against empty); a reference remaps it, for example `"legend": { "#": "fortified", "S": "!fortified" }`. Cells off the level read as empty (`out_of_bounds: "."`), every piece has `break: true`, and the 47 patterns are exclusive, so exactly one piece fits any filled cell.

Each tile below shows the piece as the 3x3 neighbourhood it is drawn for: `#` filled, `.` not filled (a corner only matters when both edges beside it are filled; otherwise it is `.` here and "don't care" in the pattern). Column 10 of row 1 is unused.

```
     0   1   2   3   4   5   6   7   8   9   10  11
     ... ... ... ... ##. ... ... .## ... .#. ... ...
  0  .#. .## ### ##. ### ### ### ### .## ### ### ##.
     .#. .#. .#. .#. .#. .## ##. .#. .## ### ### ##.

     .#. .#. .#. .#. .#. .## ##. .#. .## .##     ##.
  1  .#. .## ### ##. .## ### ### ##. .## ###     ###
     .#. .#. .#. .#. .## ### ### ##. .## ##.     ##.

     .#. .#. .#. .#. .## ### ### ##. .## ### ##. ##.
  2  .#. .## ### ##. .## ### ### ##. ### ### ### ##.
     ... ... ... ... .#. .## ##. .#. .## ### .## ##.

     ... ... ... ... .#. .## ##. .#. .## ### ### ##.
  3  .#. .## ### ##. ### ### ### ### .## ### ### ##.
     ... ... ... ... ##. ... ... .## ... ... .#. ...
```

The rules are named `m<mask>`, where the mask adds up the filled neighbours: N 1, NE 2, E 4, SE 8, S 16, SW 32, W 64, NW 128. The full piece (row 2, column 9) is `m255`; the lone block (row 3, column 0) is `m0`. Expanded with `"slug": "moss"`, they are `moss__m0` ... `moss__m255`.

## worlds/&lt;world&gt;/world.json

```json
{
  "$schema": "../../schema/world.json",
  "format": 1,
  "slug": "main",
  "name": "Main",
  "guide_groups": [
    { "slug": "ruined", "name": "Ruined wing", "offset": [0, 769], "color": "#c08040" }
  ],
  "notes": [
    { "id": "pinstatue2", "kind": "pin", "title": "Old statue", "tags": ["landmark", "forest"], "links": ["level:crevasse"], "export": true, "px": [96, 64], "icon": "statue", "label": "Statue" },
    { "id": "guidewall2", "kind": "guide", "title": "Ruined wing, west wall", "tags": ["layout", "forest"], "locked": true, "points": [[2618, -1600], [2618, -1249]], "group": "ruined" }
  ]
}
```

| Key | Type | Required | Meaning |
| --- | --- | --- | --- |
| `slug` | slug | yes | Must match the directory name |
| `name` | string | no | Display name |
| `guide_groups` | array | no | Named sets of guides, each with an offset, in the order they were added (below) |
| `notes` | array | no | World-scope annotations, coordinates in world pixels, in the order they were added. See [Annotations](#notesjson-and-annotations) |

A guide group (`guide_groups[]`) lets a set of guides be shown where it is built rather than where it was designed, such as a ruined wing built one position low. A guide joins it with its `group` key.

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `slug` | slug | required | Unique in the world; guides name it in `group` |
| `name` | string | none | Display name |
| `offset` | `[dx, dy]` | `[0, 0]` | World pixels added to every point of the group's guides when shown as built |
| `color` | color | none | Drawing color of the group's guides (over a guide's own `color`) |

Guides store their points as designed. The editor draws a grouped guide at `points + offset` ("as built"), which is also where levels snap to and where the text view counts it; a view toggle shows the designed position as well. The offset is a translation only.

Levels are the subdirectories of `levels/`. Level placement lives in each `level.json`, not here.

## worlds/&lt;world&gt;/levels/&lt;level&gt;/level.json

```json
{
  "$schema": "../../../../schema/level.json",
  "format": 1,
  "id": "crevasse22",
  "slug": "crevasse",
  "name": "Crevasse",
  "world_px": [0, 0],
  "size": [20, 5],
  "depth": 0,
  "bg_color": "#202020",
  "tags": ["forest"],
  "fields": { "biome": ["flooded"], "music": null },
  "layers": {
    "main_tiling": { "optional_groups": ["flooded_decor"], "tilesets": { "castle_main": "castle_flooded" } }
  }
}
```

| Key | Type | Required | Meaning |
| --- | --- | --- | --- |
| `id` | id | yes | Random, stable for life. Feeds the rule randomness |
| `slug` | slug | yes | Must match the directory name. Unique across all worlds. Also the level's name in the export |
| `name` | string | yes | Display name |
| `world_px` | `[x, y]` | yes | Top-left corner in world pixels |
| `size` | `[w, h]` | yes | In cells of the project grid. Every grid file in the level must cover the same pixels (see [Coordinates](#common-value-types)) |
| `depth` | integer | yes | World-view draw order; higher is in front |
| `bg_color` | color | no | Overrides the project's |
| `tags` | array of strings | no | Free tags, the same vocabulary as annotation tags (`forest`, `ruined`). Non-empty, no repeats. A level tagged `T` must have its center inside a world region tagged `T` when the world has any (see [Validation](#validation-reference)). Exported on both export files |
| `fields` | object | no | Values for `project.json` `level_fields`, by field slug |
| `layers` | object | no | Per-layer rule state, by `auto` layer slug (below). An entry for any other layer is a validation problem |

Per-layer rule state (`layers.<auto layer slug>`), all optional:

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `seed` | integer | `0` | Extra seed for this layer's rules in this level. The randomness already differs per level and layer; set this only to re-roll this layer here. |
| `optional_groups` | array of slugs | `[]` | Rule groups with `"mode": "optional"` to switch on in this level. Naming an `always` or `off` group is a validation problem |
| `tilesets` | object | `{}` | Tileset substitutions in this level: `{ "<from tileset>": "<to tileset>" }`. Every rule output drawing from the first draws from the second instead, same `[col, row]`. Rules drawing from other tilesets are unaffected |

## &lt;int_grid layer&gt;.json

The IntGrid content of one layer in one level, as a picture.

```json
{
  "$schema": "../../../../schema/intgrid.json",
  "format": 1,
  "layer": "collision",
  "legend": {
    "#": "solid",
    "-": "oneway",
    ".": null,
    "L": "ramp_l"
  },
  "rows": [
    "####################",
    "#..................#",
    "#.......----.......#",
    "#L.............#####",
    "####################"
  ]
}
```

| Key | Type | Required | Meaning |
| --- | --- | --- | --- |
| `layer` | slug | yes | Must match the file name and be an `int_grid` layer |
| `legend` | object | no | Glyph to value slug, `null` for `.` (empty). Saving always writes the layer's full legend, in sorted glyph order, so the file stands alone. When reading, it may be left out or partial: glyphs not listed mean what `layers.json` says. Any entry given must agree with `layers.json`, or the file fails to load |
| `rows` | array of strings | yes | One row per cell row of the layer's grid, top row first, all the same length. Every glyph is `.` or a value glyph of the layer; anything else, or a ragged row, is a load error. The row count and length must match the level (a validation problem otherwise) |

To paint, change glyphs in `rows` and keep row lengths unchanged. Any value of the layer can be painted, whether or not the legend lists it. `!` is reserved (no value can use it) and is a load error in these files; it only appears in a derived layer's file.

### Derived layers: overrides

A derived layer's (`"derived": true`) level file holds only what you painted by hand: **overrides** of what the rules compute. It says so with `"overrides": true` (right after `layer`):

```json
{
  "$schema": "../../../../schema/intgrid.json",
  "format": 1,
  "layer": "pillar_marks",
  "overrides": true,
  "legend": { "p": "pillar_base" },
  "rows": [
    "....p!....",
    ".........."
  ]
}
```

| Glyph | In a derived layer's file |
| --- | --- |
| `.` | no override: the rules decide |
| a value glyph | override: this value, whatever the rules say |
| `!` | override: empty (suppress what the rules would put here) |

`"overrides": true` is required on a derived layer's file and a load error on any other `int_grid` file; `false` is a load error too (the key is `true` or absent). `.` and `!` are built in: neither is a legend entry (one in `legend` is a load error: here `.` means "no override", not "empty", so `".": null` would say the wrong thing), and saving writes the layer's value glyphs only. `!` is a load error in a file of a layer that isn't derived. A derived file whose rows are all `.` is not saved (an empty layer goes away). The values the rules compute are never saved.

The **effective** value of a cell is its override where there is one, else the computed value. That is what rules, the editor, the text view and the export read. A suppressed cell reads as empty, exactly like a cell nothing wrote: it passes `.` and `!pillar_base`, never `+` or a value.

## &lt;tiles layer&gt;.json

```json
{
  "$schema": "../../../../schema/tiles.json",
  "format": 1,
  "layer": "decor_back",
  "tiles": [
    { "cell": [4, 1], "tileset": "ssw_set5", "tile": [0, 3], "alpha": 0.5 },
    { "cell": [4, 2], "tileset": "castle_main", "tile": [5, 1] },
    { "cell": [5, 2], "tileset": "castle_main", "tile": [6, 1], "flip": "x" }
  ]
}
```

| Key | Type | Required | Meaning |
| --- | --- | --- | --- |
| `layer` | slug | yes | Must match the file name and be a `tiles` layer |
| `tiles` | array | yes | Placements, saved sorted by row (`cell[1]`) then column (`cell[0]`). Several placements on one cell stack in list order |

Placement keys: `cell` (cell of the layer's grid, required, inside the level; the tile's top-left corner sits on the cell's), `tileset` (required), `tile` (`[col, row]`, required), `flip` (optional, any [flip](#common-value-types) value), `alpha` (0..1, default 1).

## entities.json

All entity instances of a level, grouped by entities layer.

```json
{
  "$schema": "../../../../schema/entities.json",
  "format": 1,
  "layers": [
    {
      "layer": "objects",
      "entities": [
        { "id": "doorcrev22", "type": "locked_door", "cell": [18, 3], "size": [1, 2], "fields": { "needs_item": "brass_key" } },
        { "id": "btncrev222", "type": "floor_button", "cell": [12, 3], "offset_px": [0, -2], "fields": { "channel": 3, "latching": true, "target": "doorcrev22" } }
      ]
    }
  ]
}
```

| Key | Type | Required | Meaning |
| --- | --- | --- | --- |
| `layers[].layer` | slug | yes | An `entities` layer |
| `layers[].entities` | array | yes | Instances, in the order they were placed. Saving keeps the order; append new ones at the end |

Instance keys:

| Key | Type | Required | Meaning |
| --- | --- | --- | --- |
| `id` | id | yes | Random, unique across the whole project, stable for life. `entity_ref` fields point at it |
| `type` | slug | yes | An entity type from `entities/` |
| `cell` | `[x, y]` | yes | The cell (of the entities layer's grid) that holds the entity's pivot. Must be inside the level |
| `offset_px` | `[x, y]` | no, default `[0, 0]` | Extra pixel offset for off-grid placement |
| `size` | `[w, h]` | no | In cells of the project grid, only when resized; must respect the type's `resizable` |
| `fields` | object | no | Field values by field slug. Fields left out take the definition's `default` |

Position math: the pivot point sits at the same spot in its cell as in the entity box, so the pivot's pixel position in the level is `(cell + pivot) * layer grid + offset_px`. A 1x1 entity at cell `[12, 3]` covers exactly that cell whatever its pivot. A `[1, 3]` door with pivot `[0.5, 1.0]` at cell `[18, 3]` stands on the bottom of that cell and covers cells `[18, 1]` to `[18, 3]` (on a layer whose grid is the project's).

## notes.json and annotations

`notes.json` holds level-scope annotations; `world.json` `notes` holds world-scope ones. Same shape in both, kept in the order they were added. Annotations aren't a layer; the editor shows and hides them per tag.

```json
{
  "$schema": "../../../../schema/notes.json",
  "format": 1,
  "notes": [
    {
      "id": "notecrev22",
      "kind": "note",
      "title": "Widen the gap",
      "body": "Jump is **too easy**.\nTry 5 cells.",
      "tags": ["todo"],
      "links": ["entity:btncrev222", "level:shaft"],
      "px": [160, 48]
    },
    { "id": "pathcrev22", "kind": "path", "status": "done", "points": [[16, 64], [300, 64]] }
  ]
}
```

Coordinates are pixels: world pixels in `world.json`, pixels from the level's top-left corner in `notes.json`.

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `id` | id | required | Unique across the project |
| `kind` | string | required | `note`, `pin`, `path`, `region` or `guide` |
| `title` | string | none | |
| `body` | string | none | Markdown |
| `tags` | array of strings | `[]` | |
| `color` | color | none | |
| `links` | array of strings | `[]` | `level:<slug>`, `entity:<id>`, `note:<id>`, or a URL. The first three must resolve |
| `status` | string | `"open"` | `"open"` or `"done"` |
| `export` | bool | `false` | Include in `export/` for the game |
| `locked` | bool | `false` | Not movable in the editor |
| `px` | `[x, y]` | | `note`, `pin` (required): the point, in pixels |
| `points` | array of `[x, y]` | | `path` (2+ points), `region` polygon (3+), `guide` (2+) |
| `rect` | `[x, y, w, h]` | | `region`, instead of `points` |
| `closed` | bool | `false` | `guide`: join the last point to the first |
| `icon` | string | | `pin`: icon name (below) |
| `label` | string | | `pin`: short label |
| `group` | slug | none | `guide`: the `world.json` guide group it belongs to (last key) |

Geometry by kind: `note` and `pin` need `px`; `path` and `guide` need 2+ `points`; `region` needs either `rect` (positive width and height) or 3+ `points`. A level-scope `note` or `pin` must sit inside its level; paths and regions may extend past it. `guide` is world scope only, `locked` in practice, and never exported. A geometry key on the wrong kind (`px` on a `path`, `group` on a `pin`) is a load error; a wrong point count is a validation problem.

Pin icon names the editor draws: `pin` (the default), `statue`, `pool`, `door`, `star`, `chest`, `skull`, `eye`, `flag`, `key`. Any other name is kept and drawn as `pin`.

Guides are locked reference lines that levels are built along and snap to, placed by hand or written by a tool (`add_note`). They aren't todo items: tools that count or list notes for an agent leave them out unless asked.

## export/ (generated)

`worlds-cli bake <dir> [--level <slug>] [--out <export dir>]` bakes every `auto` layer and writes the files the game reads: `world.json` (the index), `enums.json` and `levels/<slug>.json`. Never edit them by hand; they are rewritten from the project. Same project, same bytes: a file is only rewritten when its content changes, and a full bake removes level files for levels that no longer exist. It prints `baked N level(s), T autotiles; F files (C changed[, R removed]) in <dir>`: `F` files make up the export, `C` of them had different bytes and were rewritten, `R` stale level files were removed. In code: `worlds_core::export::write(&project, project_dir, export_dir, &baked, prune)`, where `baked` maps each level's slug to a `BakedLevel { tiles, derived }`: the baked tiles of its auto layers and the effective grid of every derived layer (the CLI passes it `worlds_rules::bake_for_export` per level). `worlds_core::export::write_counted` (same arguments) does the same and also returns the counts the CLI prints (`Written { files, changed, removed }`). `worlds_core::export::render_files` (same arguments, no `prune`) returns the same files in memory, paths relative to the export directory, without writing. `worlds-cli bake` then prints each export warning (`worlds_rules::export_warnings`, also the `warnings` of the server's `project.bake`): something a game importer can't read faithfully. There are currently none. What a particular game importer refuses is checked by `worlds-cli check-export` ([below](#checking-an-export)).

Every path in the export is relative to the **export directory** (the one holding `world.json`), `/`-separated, so a reader resolves them from there without knowing where the project is. Tileset images are rewritten to match: a project `image` of `../assets/a.png` exports as `../../assets/a.png` from the default `<project>/export`, and as whatever reaches the same file from an `--out` directory. Paths are joined, not resolved, so they can take a detour: exporting into `assets/` itself gives `../assets/a.png`, not `a.png`.

The export has its own `format` (`1`), separate from the project's. Readers must ignore keys they don't know: new keys can be added to export files without bumping `format`, which changes only when an existing key's meaning or shape does.

`export/levels/<slug>.json`:

| Key | Meaning |
| --- | --- |
| `format` | Export format, `1` |
| `slug`, `id`, `name`, `world` | The level and its world's slug |
| `world_px`, `size`, `grid_size`, `depth` | Top-left in world pixels, size in cells of the project grid, the project grid in pixels, world draw order |
| `tags`? | The level's `tags`; left out when it has none |
| `bg_color` | The level's color, else the project's; left out if neither |
| `fields` | Every project level field with its effective value: the stored one, else the default (`null`, or `[]` for arrays, when there is no default) |
| `fields_set` | The slugs of the level fields `level.json` stores a value for, sorted. A field missing here carries its default in `fields`; a reconciling importer can leave its own value alone for those. A stored value equal to the default still counts as set |
| `int_grids` | Every `int_grid` layer by slug: `{ "name", "grid_size", "derived"?, "legend", "rows" }`. `name` is the layer's display name; `grid_size` is the layer's cell size in pixels; `legend` and `rows` are as in the level files, with the layer's full legend. A layer with no content is all `.`. A derived layer has `"derived": true` and its **effective** rows (the computed values with the overrides applied; never `!`): computed marks, which a game may use (spawn points) but must not take for collision. The bake handed to the export must carry every derived layer's grid; a missing one is an export error naming the level and layer |
| `tile_layers` | Every `tiles` and `auto` layer, bottom first: `{ "layer", "name", "kind", "grid_size", "opacity"?, "tilesets": [...] }`. `layer` is the slug, `name` the display name; `grid_size` is the layer's cell size in pixels (an `auto` layer's is its source's). Each `tilesets` entry is a run of consecutive tiles (in draw order) from one tileset at one alpha: `{ "tileset", "image", "tile_size", "spacing"?, "padding"?, "alpha"?, "tiles" }`. `image` is relative to the export directory (see above), and `tiles` holds `[x, y, col, row, flip]`: level pixels of the tile's top-left corner, atlas tile, flip bits: bit 4 = transpose, applied first (swap the tile's x and y), then bit 1 mirrors left-right and bit 2 top-bottom, all inside the tile's square; so 90° clockwise = 5, 180° = 3, 270° clockwise = 6 (Godot's `TRANSFORM_TRANSPOSE`, `TRANSFORM_FLIP_H` and `TRANSFORM_FLIP_V`). Draw the entries in order and each entry's tiles in order: that is the layer's draw order. A tileset can appear in several entries when another one is drawn in between (`a, a, b, a` is three entries). `auto` layers carry the baked autotiles |
| `entities` | `{ "id", "type", "layer", "cell", "px", "size", "pivot", "fields", "fields_set" }`, in file order. `type` is the entity type slug (display names are in `world.json` `entity_types`). `cell` is in the entities layer's grid; `px` is the pivot point in level pixels, `(cell + pivot) * layer grid + offset_px`; `size` is in cells of the project grid. `fields` has every field of the type with its effective value, as for levels; `fields_set` lists, sorted, the fields the instance stores in `entities.json` |
| `notes` | Level notes with `export: true` (never guides), in file order, coordinates moved to world pixels, `export`/`locked` dropped. See [Annotations in the export](#annotations-in-the-export) |

`entity_ref` field values (in `fields` of levels and entities) become `{ "level": <slug>, "id": <id> }`, `level` being the level that holds the target, whether that's the referencing entity's own level or another one. `"level": null` only appears for a dangling reference (no entity has that id, a validation problem); a stored `null` stays `null`. In an array field each element is converted.

`export/world.json`:

| Key | Meaning |
| --- | --- |
| `format`, `name`, `grid_size`, `bg_color`? | The project's display name, grid in pixels and default background |
| `collision_layer`? | The `project.json` one: the layer `open_edges` come from. Left out when unset |
| `blocking_values`? | The effective list of blocking collision values (possibly `[]`): `project.json`'s, or the defaults of the migration rule when it's left out. Present exactly when `collision_layer` is |
| `files` | The export's other files, relative to the export directory: `{ "enums": "enums.json" }` |
| `entity_types[]` | Every entity type, by slug: `{ "slug", "name", "category"?, "tags" }`. `name` is the display name, `category` the palette path, `tags` possibly `[]` |
| `worlds[]` | `{ "slug", "name"?, "notes", "levels" }`; `notes` are the world notes with `export: true` (never guides) |
| `levels[]` | `{ "slug", "id", "name", "file", "rect", "depth", "tags"?, "touching", "open_edges"? }`. `file` is relative to the export directory; `rect` is `[x, y, w, h]` in world pixels; `tags` as in the level file, left out when empty |
| `touching[]` | Levels of the same world sharing part of an edge: `{ "level", "edge": "left"/"right"/"top"/"bottom" (this level's), "span": [start, end) }` in world pixels. Corner contact doesn't count |
| `open_edges` | Per edge (`left`, `right`, `top`, `bottom`), runs of open border cells as `[first, end)` cell indices along the edge (top to bottom, left to right), in cells of the collision layer's grid. A cell is open when its collision value isn't in `blocking_values`: empty cells, and also kinds like `oneway` or `water` unless listed. Left out when there's no `collision_layer` (no layer is guessed). `null` when the level's collision grid is absent or all empty: the export doesn't describe that level's seams (an importer falls back on its own) |

`export/enums.json`: `{ "format": 1, "enums": [...] }`, every project enum in `enums.json` order as `{ "slug", "name", "values": [{ "slug", "name" }] }`, values in definition order. `name` is the display name, or the slug when none is set. Inline `values` of a field definition aren't project enums and aren't listed.

### Checking an export

`worlds-cli check-export [<export dir>] [--project <dir>] [--fresh] [--profile none|godot-rooms] [--json]` checks an export directory without the game: an independent reading of the files as plain JSON (not through the writer), against this section and an importer contract.

- **format**: every file parses and has `format` 1 and the keys and types above (unknown keys are allowed, as for any reader); `blocking_values` present exactly when `collision_layer` is; level slugs unique; every `world.json` level `file` exists and every `levels/*.json` is listed (a stale file is a problem, reported once even with `--fresh`); a level file's `slug`, `id`, `world` and `grid_size` agree with `world.json`, its `world_px` is on the grid and `world_px` + `size` x `grid_size` is its `rect`; int grid `rows` are the level's size in the layer's cells with glyphs from the legend; a tile layer's `kind` is `tiles` or `auto`; every tile is `[x, y, col, row, flip]` with a flip in 0..7 and its box (a transposed tile's swapped) overlaps the level; `touching` and `open_edges` equal what the rects and collision rows give (a missing `open_edges` is reported even when the rows themselves are wrong); the collision layer isn't derived (reported in each level file, at `int_grids.<collision>.derived`); exported annotations parse as annotations.
- **reference**: tileset `image`s exist (resolved from the export directory) and, for a PNG, every atlas tile lies inside it (with `spacing` and `padding`); entity `type`s are in `entity_types`; no `entity_ref` value has `"level": null`.
- **importer**: the profile's rules. `--profile none`, the default, checks the format only; `--profile godot-rooms` adds the rules of a Godot room importer ([below](#the-godot-rooms-profile)).
- **stale** (`--fresh`): the project (`--project`, else the export directory's parent when it holds `project.json`) is baked in memory exactly as if into the export directory being checked, image paths included (`export::render_files`), and every file whose bytes differ, is missing, or is a level the bake doesn't produce is a problem. Nothing is written. Without an export directory, `--project <dir>` checks `<dir>/export`; `--project` without `--fresh` is a usage error.
- **annotation** warnings, never problems: a `note:`, `level:` or `entity:` link that names nothing exported.
- **importer** warnings (`godot-rooms`): the layer order (a pair of layers the export stacks one way and the importer, by z, the other: `Worlds draws main_tiling above decor_front, but the importer draws DecorFront (z 2) above Skin (z -1); ...`, once per pair across levels), and `blocking_values` that aren't the importer's walls (`solid`, `fortified`, `breakable`, `ramp_r`, `ramp_l`; the importer then infers seams from the scenes).

The output is one line per problem, `file: json.path: message` as for `validate` (file relative to the export directory), then `warning: ...` lines, then `ok: N levels, no problems` or `N problem(s)` (each with `, W warning(s)` when there are some). `--json` prints instead (here for the `skin_offgrid` fixture below, with `--profile godot-rooms`):

```json
{ "levels": 1, "ok": false,
  "problems": [ { "file": "levels/room.json", "kind": "importer", "path": "tile_layers[main_tiling].tilesets[0].tiles[1]",
                  "message": "level room: main_tiling: 1 tile(s) off the 32 px grid; the skin must be one 32x32 tile per cell, on the 32 px grid" } ],
  "warnings": [] }
```

`kind` is one of `format`, `reference`, `importer`, `stale` (problems) and `annotation`, `importer` (warnings). Exit codes: **0** no problems (warnings allowed), **1** problems, **2** a usage or I/O error (no such directory, an unreadable file, a project that doesn't load or bake).

Fixtures: `crates/worlds-cli/tests/fixtures/importer_contract/<case>/` are small complete exports, each with an `expect.json` (`{ "accept", "error_contains", "warning_contains"? }`); the Rust tests run every case with `--profile godot-rooms`, and an importer can run the same cases to check it agrees on accept or refuse and on the warnings (`warning_contains` is a substring the warning carries; without it the case has none). Their README lists the cases.

### The godot-rooms profile

`check-export --profile godot-rooms` checks the contract of a Godot importer that turns each level into a room scene with a 32 px grid. These rules are that importer's conventions, not Worlds' (and not the [Godot addon](../godot/addons/worlds/README.md)'s, which reads any export the format check accepts): the profile checks the refusals (`crates/worlds-cli/src/importer_contract.rs`), and an importer following it refuses the same exports, the whole import at once. Use it as is if your importer follows these conventions, or as a starting point for your own; the default, `--profile none`, checks only the format.

| Export | In the room scene |
| --- | --- |
| `auto` layer `main_tiling` | The room's `Skin` TileMapLayer (z −1), copied cell for cell by `SkinLayer` at runtime. **Refused** unless every tile is 32x32, on the 32 px grid and alone on its cell. Empty: the scene's Skin is kept |
| Other `auto` layers | Their own TileMapLayer each, named after the layer's display name, z −1 when listed before `main_tiling` in `tile_layers`, else z 0; never promoted to Skin |
| Project | **Refused** unless `grid_size` is 32 and there is a `collision_layer`. `blocking_values` other than the importer's walls: a warning, seams are then inferred from the scenes |
| `tiles` layers `decor_back*` / `decor_front*` | `DecorBack*` (z −2) / `DecorFront*` (z 2) TileMapLayers. An empty or missing one leaves the scene's node alone. Any other `tiles` layer is **refused** when it has tiles and skipped when empty (nothing to place; its name isn't checked) |
| Tile placement | Exact, never snapped: tiles that don't sit on the layer node's 32 px cells (16 px tiles, a 16 px offset, overlapping tiles) go into child sub-layers (`_<w>x<h>+<ox>+<oy>#<k>`) positioned by the offset, in export draw order |
| `flip` | Godot alternative flags: bit 1 `TRANSFORM_FLIP_H`, 2 `TRANSFORM_FLIP_V`, 4 `TRANSFORM_TRANSPOSE` (transpose first, as in the export). A transposed non-square tile and a flip outside 0..7 are **refused** |
| Layer names | A layer's node name is its display name with `. : @ / " %` replaced by `_` (Godot's `validate_node_name`); the `auto` layer `main_tiling` is always `Skin`, whatever its name. **Refused** when, for any other layer (empty ones included), it is empty, is `Skin`, `Collision`, `Entries`, `Exits`, `Objects`, `Enemies`, `Props`, `SkinLayer`, `GreyboxLayer`, `Backdrop`, `Darkness`, `Breakables`, `SeamAprons` or `_SkinPreview`, starts with `_`, or is another layer's node name in the same level (`Back.Art` and `Back:Art` are both `Back_Art`) |
| Layer order | The importer draws by z (DecorBack −2, auto layers before the skin −1, Skin −1, auto layers after it 0, DecorFront 2), the export by list order: a pair the two stack differently is a warning (once per pair), never a refusal |
| `int_grids` | `collision_layer`'s grid becomes `Collision`; `derived: true` grids are ignored (never collision) |
| Entities, fields, seams | Nodes matched by `worlds_id`, fields owned per `fields_set`, seams from `touching`/`open_edges` |
| Level `tags` | Room metadata `worlds_tags` (left out when empty) |
| Annotations | Not checked: what an importer does with them is up to the game |

### Annotations in the export

The export carries geometry plus tags, and no game meaning: what an annotation is for is said by its tags, so the game can read the same export in different ways (map markers from pins, patrol routes from paths, area triggers from regions). Each exported annotation has the file form, minus `export` and `locked`, with world-pixel geometry:

```
{ "id": "pinstatuea", "kind": "pin", "title": "Statue", "tags": ["landmark", "caves"],
  "links": ["level:crevasse"], "px": [96, 64], "icon": "statue", "label": "Statue" }
{ "id": "pathwalkaa", "kind": "path", "title": "Walk", "tags": ["route", "caves"],
  "links": ["note:pinstatuea", "note:pinpoolaaa", "note:pingoldaaa"], "points": [[96, 64], [400, 64], [400, 300]] }
{ "id": "rgcavesaaa", "kind": "region", "title": "Caves", "tags": ["area", "caves"], "rect": [-224, -1600, 900, 769] }
```

- Where: world-scope ones in `world.json` `worlds[].notes`; level-scope ones in that level's `levels/<slug>.json` `notes`, moved to world pixels. Every coordinate is in world pixels, so a reader can merge both lists.
- Kinds: `note` (text at a point), `pin`, `path`, `region` (`rect` or `points`, never both). Never `guide`: guides don't export, and validation reports one flagged `export`.
- Keys: `{ id, kind, title?, body?, tags?, color?, links?, status?, px | points | rect, icon?, label? }`; `tags` and `links` are left out when empty, `status` when open.
- `links` keep the file strings. An exported annotation's `note:` links always point at exported annotations (validation reports one that doesn't), so they resolve inside the export; `entity:<id>` resolves through the level files' `entities[].id`, `level:<slug>` through `world.json` `levels[].slug`.
- Order: file order (world notes, then each level's), stable across bakes.

Tags are free-form: any tag is allowed, and only the region check of [validation](#validation-reference) reads them (a level tagged like a region annotation should lie inside it). The editor treats one specially: `todo` marks a note as a work item, not something for the game.

## How files are written

Knowing this lets you write edits that match what the editor would save, so diffs stay small:

- Two-space indentation. Keys in the order this document lists them.
- An array or object below the top level goes on one line if it fits in 100 columns and holds no list of objects. A `rows` array is always one row per line.
- Optional keys at their default are left out.
- Directory-backed things and definitions keyed by slug are sorted by slug. Entity instances and notes keep their order. Tile placements are sorted by row then column. Object keys inside free-form JSON (field values of type `json`, `tile`) are written in sorted order.
- Each file is written atomically (temporary file, fsync, rename, fsync the directory) and only when its bytes change. When saving deletes files, every directory that lost an entry is fsynced too. The same holds for `export/`: a full export that prunes stale level files fsyncs `export/levels`, and the export directory is fsynced after writing. (Directory fsyncs happen on Unix only; Windows can't open a directory as a file.)
- The editor saves incrementally: only files its commands changed are written (`worlds_core::save_dirty`, see [commands.md](commands.md#saving)), with the same bytes a full save would produce.
- Saving deletes a file only when the project loaded (or last saved) that very file and no longer produces it: a removed level, entity type, rule set, `entities.json`, `notes.json` or layer content. A file the editor never saw, such as a level an agent wrote while the editor was open, is never deleted, even in a managed directory.

## Text view

A level as plain text: the grids as pictures with coordinates, then everything else one item per line. It is what the MCP `read_level` tool returns, so an agent can see a room's geometry and the ids it needs in one read. In code: `worlds_core::text::render_level(&project, "crevasse", None)`; pass `Some(&["collision"])` to show only some layers (entities and notes are always listed). With bake results, `worlds_core::text::render_level_baked(&project, "crevasse", None, &derived)` (`derived`: the effective grid of each derived layer) shows derived layers as they are, not just their overrides. The format is stable: new information is only added as new keywords at the end of a line or as new sections.

```
level crevasse "Crevasse" world main id crevasse22 tags ["forest"]
size 20x5 cells of 16 px, world_px [0, 0], depth 0, bg #202020
fields: biome = ["flooded"], music = null
rule state main_tiling: seed 8123 optional_groups [flooded_decor]

int_grid collision (20x5 cells of 16 px)
legend: . empty, # solid, f fortified, o oneway, ~ water, L ramp_l
  0         1
  01234567890123456789
0 ####################
1 #..................#
2 #.......oooo.......#
3 #L..~~~.......ff####
4 ####################

auto main_tiling: generated from collision by rules/main_tiling.json (not shown)

tiles decor_back (40x10 cells of 8 px): 3 tiles
  [4, 1] ssw_set5 [0, 3] alpha 0.5
  [4, 2] castle_main [5, 1]
  [5, 2] castle_main [6, 1] flip x

entities: 2
  doorcrev22 locked_door in objects at [18, 3] size [1, 2] fields {"exit_to": "doorshaft2", "facing": "right", "needs_item": "brass_key", "script": "scripts/door.gd", "speed": 2, "sprite": {"tile": [4, 4], "tileset": "castle_main"}, "tint": "#ff8080", "waypoints": [[1, 1], [2, 1]]}
  btncrev222 floor_button in objects at [12, 3] offset_px [0, -2] fields {"channel": 3, "latching": true, "target": "doorcrev22"}

notes: 2
  notecrev22 note open at [160, 48] title "Widen the gap" tags ["todo"] links ["entity:btncrev222", "note:pinstatue2", "https://example.com"] body "Jump is **too easy**.\nTry 5 cells."
  pathcrev22 path done points [[16, 64], [300, 64]]

world notes over this level: 2 (world pixels)
  pinstatue2 pin open at [96, 64] icon "statue" label "Statue" title "Old statue" tags ["landmark", "forest"] links ["level:crevasse"] export
  regioneast region open rect [0, 0, 640, 480] title "Forest" tags ["zone", "forest"]
guides over this level: 1 (list_notes kind guide)
```

Line by line:

- **Header.** `level <slug> <name as a JSON string> world <world> id <id>`, then ` tags [...]` when the level has tags; then size (in cells of the project grid), position, depth and `bg` when set; `fields:` when the level stores any (values as compact JSON); one `rule state <auto layer>:` line per entry of `level.json` `layers`.
- **`int_grid <layer> (<w>x<h> cells of <n> px)`**, for each int_grid layer in `layers.json` order, then `legend:` with `.` first and the values in definition order, then the picture. Above the rows, a ruler of column numbers: the tens digit at every tenth column (only when the grid is wider than 10), then the ones digit of every column. Each row starts with its row number, right-aligned, and one space. Cell `[x, y]` is the character in column `x` of row `y`, exactly as in the layer file's `rows`. A layer with no content in the level shows as all `.`.
- **Derived layers** are `int_grid` sections whose header ends with ` derived by <writer layer>` (or ` derived` when no layer writes it). The picture is the effective grid when the view has bake results; without them it is the stored overrides, and the header adds ` (not baked: overrides only)`. When the level stores overrides, an extra section follows the picture: `overrides: <n> cells (<v> set, <s> suppressed; . = from the rules)` and the stored rows (`!` for a suppressed cell) with the same ruler; not baked, the picture already is the stored rows, so only the `overrides:` line follows it:

  ```
  int_grid pillar_marks (64x28 cells of 32 px) derived by pillars
  legend: . empty, p pillar_base
  ...the effective picture...
  overrides: 3 cells (2 set, 1 suppressed; . = from the rules)
  ...the stored rows...
  ```
- **`auto <layer>`**: auto layers are generated, so the view only says where from.
- **`tiles <layer> (...): <n> tiles`**, then one placement per line in file order (row, then column): `[x, y] <tileset> [col, row]`, then ` flip x|y|xy|t|xt|yt|xyt` and ` alpha <a>` when not the default.
- **`entities: <n>`**, then one per line in file order: `<id> <type> in <layer> at [x, y]`, then ` offset_px [x, y]`, ` size [w, h]`, ` fields <compact JSON>` when set.
- **`notes: <n>`**, then one per line in file order: `<id> <kind> open|done`, the geometry (` at [x, y]` for notes and pins, ` points [...]` for paths, guides and polygon regions, ` rect [x, y, w, h]` for rect regions, ` closed` for a closed guide, ` icon` and ` label` for pins), then ` title`, ` tags`, ` links`, ` export`, ` locked`, ` body` and (guides) ` group` when set. Strings are JSON strings, so a body's newlines read as `\n`; tags and links are JSON arrays.
- **`world notes over this level: <n> (world pixels)`**: the world-scope annotations other than guides whose bounding box meets the level's rect in the world, one per line in the same form as `notes:` (coordinates are world pixels), then **`guides over this level: <n> (list_notes kind guide)`**: only a count of the world's guides over the level, where they are built (group offset applied). Both lines are always there (with `0`), so the shape is predictable. Notes filtered out of `notes:` (done ones, for the MCP `read_level`) are filtered out here too; with no notes section at all, neither line is printed.

## Validation reference

`worlds-cli validate <dir> [--enum-source <enum>=<file.json>[#<json-pointer>]]... [--enum-extra <enum>=<value>[,<value>...]]...` reports problems as `file: json.path: message` and exits 0 when there are none, 1 when there are, 2 when the project doesn't load or an argument is wrong. Paths name list items the way you'd find them: entities and notes by id (`layers[objects].entities[btncrev222].cell`, `notes[notecrev22].links[0]`), tile placements by cell (`tiles[4,2].tileset`), and lists whose order is meaningful (layers, values, rules, fields) by index. A rule a template reference expands to is named by its expanded slug under the reference: `groups[0].rules[2].expanded[walls__lip].match.rows[1]`. It checks:

- references: `collision_layer` (an `int_grid` layer, not derived), `blocking_values` (values of the collision layer, only with one; reported when left out and the layer has none of the default blocking kinds, since every border cell would be open), auto layer `source`, every `tileset` (including level `tilesets` substitutions and template references), `enum`, entity `type`, `allowed_types`, `entity_ref` values (same level unless `cross_level`, allowed type or tag), pattern `grid` (`match` and every `require`, a list's patterns at `require[<k>]`) and legend slugs (including each `[a|b]` member), `region.grid`, `write.grid` and `write.value`, `out.stamp`, `requires_level_field`, note `links`;
- macros: named patterns and legends a rule or template uses exist in `rules/macros.json`, a reference's `use` names a template or a built-in, its `tiles` and `skip` name rules of that template (and `tiles` only tiles rules), it has a `tileset` when a template rule it uses needs one, and no project template takes a built-in's name (`blob47`). Every rule a reference expands to is checked like a hand-written rule, against the referencing layer;
- grids: each layer's `grid_size` divides or is a multiple of the project grid, an auto layer doesn't set a different one, a pattern's `grid` (`match` and `require`) is on its auto layer's grid; IntGrid content is the level's size in the layer's cells and holds only defined values;
- glyphs: value and group glyphs aren't reserved and don't repeat; every pattern glyph means something (legend, built-in, value or group glyph); legend keys aren't reserved, and `@` is a key only in a pattern (as written) whose rows contain `@`;
- per-level rule state: only on `auto` layers, `optional_groups` only names `optional` groups;
- passes and tags: groups in pass order (`groups[<i>].pass`: "group art (pass 0) comes after a pass 1 group; keep the file in pass order"); a `requires_level_tag` with no tags (`groups[<i>].requires_level_tag.tags`: "needs at least one tag"), an empty or repeated tag (`...tags[<k>]`);
- region rules: `region.grid` (as for a pattern's `grid`: an `int_grid` layer on the auto layer's grid), `region.cells` (not `"*"`: "every cell would be one region"; values and groups the grid has, as are `place.against`'s), a `min_*` greater than its `max_*` (`region.min_width`: "min_width 20 is greater than max_width 10"), `place.align` with a spacing other than `even` and `place.jitter` with one other than `random`;
- derived grids: a `write.grid` that isn't a derived `int_grid` layer on the auto layer's grid, a `write.value` that isn't a value of it (a group isn't one), a second layer writing a grid one layer already writes (`write.grid`: "pillar_marks is already written by layer pillars"), auto layers that read each other's derived grids (`layers.json` `layers[<first layer of the cycle>]`: "auto layers a and b read each other's derived grids: a → marks_b → b → marks_a → a"), a derived `collision_layer` (`project.json` `collision_layer`), and a derived layer's stored overrides the size of the level in its cells;
- tile placements and entity cells inside the level;
- duplicate slugs and ids (layers, values and groups, tilesets, enums and their values, entity types, fields, rule groups, stamps, worlds, levels, entity ids, note ids; in a rules file, rule slugs, reference slugs and expanded slugs together; template rule slugs within a template);
- field values and defaults against their field type, `min`/`max`, `regex` (the whole value must match), enum values, path extensions, nullability (per element for arrays); `point` and `entity_ref` defaults again in each level or entity that inherits them; a `regex` that doesn't compile;
- annotations, guide groups and level tags: a guide's `group` names a guide group of its world (`notes[<id>].group`); guide group slugs are unique in their world (`guide_groups[<i>].slug`); a guide flagged `export` (`notes[<id>].export`: guides are never exported, the flag is ignored); a level-scope `note` or `pin` whose `px` is outside its level's pixels (`notes[<id>].px`); a region `rect` with a width or height of 0 or less (`notes[<id>].rect`); an exported annotation whose `note:` link points at one that isn't exported, or at a guide (`notes[<id>].links[<i>]`: the game couldn't resolve it); an empty tag, or one given twice in a list, on annotations and levels (`notes[<id>].tags[<i>]`, `tags[<i>]`); and the **region check**: for each tag `T` of a level, when its world has at least one world-scope region tagged `T`, the level's center (`world_px + size * grid / 2`, integer division) must lie inside one of them (`level.json` `tags[<i>]`: "level tagged "forest" sits outside every region tagged "forest" (its center is [x, y])"). A rect is half-open (`[x, x + w)`); a polygon uses the even-odd rule, a point on an edge counting as inside. Regions are compared as stored (guide groups never move regions). A tag no region carries is never checked;
- entity types: a `category` is a path of non-empty segments separated by `/`, with no `/` at either end and no whitespace around a segment (`Puzzle/Doors`, not `/Puzzle`, `Puzzle//Doors` or `Puzzle / Doors`); no tag appears twice in one type's `tags`;
- layer slugs that collide with file names (`level`, `entities`, `notes`, `macros` for `int_grid`/`tiles` layers, `macros` for `auto` layers);
- shape rules: at most one `@` per pattern, odd pattern sizes unless the pattern has `@`, rows of one length, at least one tile alternative with weights of 1 or more, `pivot` only in stamp mode, `rotate` only with square tiles (the output's tileset and every level substitute for it), annotation point counts, guides only at world scope, entity `limit`s, numbers that must be in 0..1;
- enum sources (only when asked): `--enum-source skin=data/skins.json` compares a project enum with the game data file it mirrors. The values are the keys of the object at the JSON pointer (`#/tracks`; the whole file without one; keys starting with `_` are comments and skipped) or the strings of an array there. Each value the enum has and the file doesn't (`enums.json` `enums[<i>].values[<j>]`: "enum skin has "none", which data/skins.json doesn't (enum source drift)"), and each the file has and the enum doesn't (`enums[<i>]`: "enum skin lacks "jungle", which data/skins.json has (enum source drift)"), is a problem. `--enum-extra skin=none` names values only the enum may have (for example a "no skin" value). The file is relative to the current directory. Repeatable; an unknown enum, an unreadable file, a pointer that finds nothing, any other shape, or an `--enum-extra` for an enum no `--enum-source` compares is a usage error. A typical game check: `--enum-source skin=data/skins.json --enum-extra skin=none --enum-source background=data/backgrounds.json --enum-source music=data/music.json#/tracks`.

Load errors stop the load and name the file and JSON path: bad JSON, unknown keys, a key of another kind even at its default (`values` on a `tiles` layer, `min` on a `bool` field, `px` on a `path`), wrong types, `format` other than 1, a reserved glyph, a malformed tile alternative, an output mixing tiles and stamp, a rule key on a template reference or a reference key on a rule, a rule with both or neither of `match` and `region`, both or neither of `out` and `write`, `place` without `region` (or `region` without `place`), a pattern-rule key on a region rule, `place` with both or neither of `along` and `across`, `[x, y]` with `along` or `min_length`/`against` with `across`, a region bound of 0, a negative or fractional `pass`, an empty `require` list, a region rule or `write` in a template, `"overrides"` missing on a derived layer's file, present on another file or `false`, `!` in a file of a layer that isn't derived or in a legend, a cell test that isn't `*`, `+`, `.`, a slug, `!slug`, `[a|b]` or `![a|b]`, an IntGrid row with an unknown glyph or a different length, an IntGrid legend that disagrees with `layers.json`, a slug that doesn't match its file name, an unknown file in a level directory.
