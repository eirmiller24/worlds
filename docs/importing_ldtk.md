# Importing an LDtk project

> Worlds does not round-trip: the import is one way, and nothing is ever written back to LDtk.

```
worlds-cli import-ldtk mygame.ldtk --out mygame.worlds
```

This converts an LDtk 1.5 project (`.ldtk`, plus `.ldtkl` files when levels are external) into a Worlds project directory, writes `schema/`, and prints every warning and every identifier that changed. It exits non-zero on error. The goal is to carry the project's *meaning*, not LDtk's file. Uids, `__*` mirrors, caches and editor state don't come along.

In code: `worlds_core::ldtk::import(path, out_dir)` returns the `Project`, an `ImportReport { warnings, renamed }` and an in-memory `LdtkReference` (below); `worlds_core::ldtk::write(&imported, out_dir)` saves it. Nothing is written until `write`.

## Names

Every LDtk identifier becomes a snake_case slug: `Main_Tiling` -> `main_tiling`, `FloorButton` -> `floor_button`, `Heavy sword` -> `heavy_sword`, `SSW_CastlePack` -> `ssw_castle_pack`. A slug that would start with a digit gets a prefix (`8px` -> `n_8px`). Collisions within one namespace get `_2`, `_3`... Every change is listed in `renamed`.

Layers of kind `int_grid` or `tiles` named `level`, `entities`, `notes` or `macros` (reserved file names) get a `_layer` suffix.

Display names keep the LDtk spelling: layer and entity `name` is the LDtk identifier; enum values and fields get a `name` only when their slug differs.

## What maps to what

| LDtk | Worlds |
| --- | --- |
| file stem | `project.json` `name` |
| layer `gridSize` | project `grid_size`, plus a layer `grid_size` where a layer differs (see [Grids](#grids)) |
| `defaultLevelBgColor` | project `bg_color` |
| IntGrid layer named `collision` (any case) | project `collision_layer`; left unset when there is none (nothing else is guessed) |
| (no LDtk equivalent) | project `blocking_values`: `["solid"]` when the collision layer has a `solid` value (see [Blocking values](#blocking-values)) |
| `defs.levelFields` | project `level_fields` |
| `defs.layers` (first = top) | `layers.json` reversed (first = bottom) |
| IntGrid layer | `int_grid` layer; values keep `value` order with `identifier` (or `v<n>`), color, group, and a glyph (see [Glyphs](#glyphs)) |
| IntGrid layer with a tileset and rules | `int_grid` layer plus an `auto` layer `<slug>_tiles` right above it, `source` = the IntGrid layer |
| AutoLayer | `auto` layer, `source` = its `autoSourceLayerDefUid` |
| Tiles layer | `tiles` layer, `tileset` = its default tileset |
| Entities layer | `entities` layer |
| `displayOpacity` | `opacity` |
| `tilePivotX/Y` | `tile_pivot` on the auto layer |
| IntGrid value groups | `groups` with a glyph each (value and group slugs share one namespace) |
| tileset | `tilesets.json`: slug, `image` relative to the output directory pointing at the original PNG (never copied), `tile_size` = `[tileGridSize, tileGridSize]`, `spacing`, `padding` |
| enums and external enums | `enums.json`; value slugs snake_case; value color (0 = none), value tile icon |
| entity def | `entities/<slug>.json`: tags, doc, size in cells, resizable, pivot, icon (`uiTileRect` else `tileRect`, top-left tile; see below for the pixel arithmetic), color, `limit` from `maxCount`/`limitScope`/`limitBehavior` (0 = unlimited) |
| field def | type, `isArray`, `canBeNull`, `defaultOverride`, min/max, regex (without `/.../flags`; the `i` flag becomes a leading `(?i)`, and the pattern is otherwise kept as is: Worlds requires a whole-value match where LDtk accepts a match anywhere, so anchor or widen it yourself if a substring match was meant), `acceptFileTypes` -> `extensions`, tileset, doc, display |
| `allowedRefs` | `OnlySame` -> `allowed_types` = own type; `OnlySpecificEntity` -> that type; `OnlyTags` -> `allowed_tags`; `allowOutOfLevelRef` -> `cross_level` |
| `editorDisplayMode` / `editorShowInWorld` | `display.canvas` unless `Hidden`; `display.arrow` for refs and points shown as links/paths; `display.world` |
| world (`MultiWorlds`) or the implicit one | `worlds/<slug>/`; a single-world file becomes world `main` |
| level | slug, name, `world_px` (`worldX/Y`; Linear layouts are laid out left to right / top to bottom), `size` in cells, `depth` = `worldDepth`, `bg_color` (null = project default), fields |
| layer instance `seed`, `optionalRules`, `overrideTilesetUid` | `level.json` `layers.<auto layer>`: `seed` (kept for continuity; Worlds doesn't need one), `optional_groups` (group slugs), `tilesets`: each tileset the layer's rules draw from mapped to the override (left out when they're the same) |
| `intGridCsv` | `<layer>.json` rows of glyphs in the layer's own cells, `0` -> `.`; an all-empty grid writes no file |
| `gridTiles` | `<layer>.json` placements: `cell` from `px` and the layer's grid, `tile` = `[t % cWid, t / cWid]`, `flip` from `f`, `alpha` from `a`, tileset = instance override or layer default |
| entity instances | `entities.json` in LDtk's order: fresh random `id`, `type`, `cell` (in the entities layer's grid) + `offset_px` (below), `size` in project cells only when resized, fields |
| field instances | only fields whose `realEditorValues` are set; unset fields take the definition default, as in LDtk. An array field is set whenever it has elements: its `realEditorValues` has one entry per element and `null` there means "element at its default", so the resolved `__value` is imported, length kept. Enums become value slugs, colors hex, points `[cx, cy]`, entity refs the new ids, tiles tile refs, `path` values (and defaults and array elements) are rebased from the `.ldtk` file's directory to the output directory like tileset images (an empty path stays empty) |

`cWid` (tiles per atlas row) is computed as LDtk does: `ceil((pxWid - 2 * padding) / (tileGridSize + spacing))` (`TilesetDef::columns`). A mismatch with the saved `__cWid` is a warning.

Tile rectangles (entity and enum icons, rule group icons, `tile` field values) are in atlas pixels; the tile is `(x - padding) / (tileGridSize + spacing)` per axis (`TilesetDef::tile_at_px`), the inverse of LDtk's `padding + col * (tileGridSize + spacing)`.

### Rules

Each auto layer gets `rules/<layer>.json`. Groups keep their order, name (slug from it), color and icon. `active` and `isOptional` become one `mode`:

| LDtk | Worlds `mode` |
| --- | --- |
| `isOptional` | `optional` |
| `active`, not optional | `always` |
| not `active`, not optional | `off` |
| not `active`, but some level lists it in `optionalRules` (LDtk's "force on") | `optional`, with a warning; those levels list it in `optional_groups` |

A group's `requiredBiomeValues` becomes `requires_level_field`: `field` = the layer's `biomeFieldUid` level field, `values` = the enum value slugs, `mode` = `any` (0) or `all` (1).

Each rule becomes `slug: "r_<uid>"`, `salt: <uid>` (kept so nothing about the import is arbitrary; Worlds doesn't need it), and:

| LDtk | Worlds |
| --- | --- |
| `pattern` (flat `size*size`) | `match.rows`, row-major. `match.grid` is left out (it is the auto layer's `source`), and the `legend` only lists negations |
| `0` | `*` |
| `v` / `-v` | the value's own glyph (no legend entry) / its negation glyph -> `!slug` |
| `1000001` / `-1000001` | `+` / `.` |
| `±(groupUid+1)*1000` | the group's glyph (no legend entry) / its negation glyph -> `!group` |
| `flipX`, `flipY` | `symmetry: ["mirror_x", "mirror_y"]` |
| `tileRectsIds` | `out.tiles`, one alternative per inner list, weight 1, tileset = the layer's |
| `tileMode`, `pivotX/Y` | `tile_mode` (`Single` -> `stack`, `Stamp` -> `stamp`), `pivot` (stamp mode only) |
| `chance`, `breakOnMatch`, `active`, `alpha` | `chance`, `break`, `active`, `out.alpha` |
| `xModulo/yModulo/xOffset/yOffset` | `modulo` (left out when 1/1/0/0) |
| `checker` | `checker` |
| `perlin*` | `perlin` when active or when scale/octaves aren't LDtk's defaults (0.2, 2); an inactive default gate is just LDtk's random seed |
| `outOfBoundsValue` | `out_of_bounds`: null -> left out, 0 -> `.`, value -> its slug |
| `tileXOffset/YOffset` | `out.offset_px` |
| `tileRandom*` | `out.random_offset_px` |

Patterns are not trimmed; LDtk trims on save.

### Glyphs

Each IntGrid value gets one glyph, stored in `layers.json`. Common names get a glyph that reads well: `solid` `#`, `oneway` `-`, `water` `~`, `hazard` `x`, `breakable` `b`, `fortified` `f`, `ramp_r` `/`, `ramp_l` `L`. Other values get the first letter of the slug, else the first letter of a later word, else the uppercase first letter, else the next free character from a fixed pool (`# = ~ ^ % & $ ? / < > : ; - _` ...). A glyph is never one of the reserved `. \ " * + ! @ [ ] |`.

Each value group gets a glyph the same way (stored on the group in `layers.json`), so patterns can name a group without a legend. Negations (`!solid`) need a pattern-only glyph: the other case of a letter glyph, else the uppercase first letter of a word of the slug (`!solid` is `S` when solid is `#`), else the next free one. All glyphs of a layer come from one pool, so they never clash.

### Grids

LDtk gives every layer its own grid size; Worlds has a project grid that sizes levels, and a layer may use another grid that divides it or is a multiple of it. The importer tries the layer grids as the project grid, most common first (ties: `defaultGridSize`, then the larger), and takes the first that every other layer grid fits with and that every level is a whole number of cells of. Layers on another grid keep it as `grid_size`. `Test_file_for_API_showing_all_features.ldtk` (8 px and 16 px layers, a 680x568 px level) imports with an 8 px project grid and `grid_size: 16` on its 16 px layers. An auto layer takes its source's grid.

### Entity placement

LDtk's `px` is the pivot point. Worlds puts the pivot at `(cell + pivot) * grid + offset_px`, with `grid` the entities layer's grid, so `cell = floor((px - pivot * grid) / grid)` and the remainder becomes `offset_px` (with a warning, since it's off the grid). Entity sizes in pixels become cells of the project grid, rounded up; a size that isn't whole cells is a warning. Instance sizes that differ from the definition become `size`.

## The parity reference

`Imported.reference` is for the bake parity test and is never saved:

- `autotiles[level][auto layer]`: LDtk's saved `autoLayerTiles`, in file order, as `{ px, tileset, tile: [col, row], flip, alpha, rule: "r_<uid>", anchor_cell }`, decoded from `d = [ruleUid, coordId]` with `coordId = cx + cy * cWid` (`cWid` in the layer's own cells).
- `opaque_tiles[tileset]`: the `[col, row]` tiles LDtk's `cachedPixelData.opaqueTiles` marked opaque, to cross-check opacity computed from the PNGs.

## Dropped silently

Derived or cached data and editor-only state: every `__*` field, `toc`, `cachedPixelData`, `savedSelections`, `__neighbours`, `nextUid`, `appBuildId`, iids and uids, `autoLayerTiles` (reference only), `visible`, `usesWizard`, `invalidated`, `guideGrid*`, `uiColor`, `hideInList`, `inactiveOpacity`, `renderMode`, `tileRenderMode`, `nineSliceBorders`, fill/line/tile opacities, `showName`, `exportToToc`, `useForSmartColor`, `editorDisplayScale/Pos/Color`, `editorLinkStyle`, text prefix/suffix, `textLanguageMode`, project export settings, `identifierStyle`, the project `bgColor` (editor background), and display modes Worlds has no equivalent for (radius, tile display). LDtk's level `useAutoIdentifier` and `levelNamePattern` don't apply.

## Warned (meaning Worlds can't hold yet)

- layer `pxOffsetX/Y` (definition or instance), parallax, entity layer `requiredTags`/`excludedTags`, `autoTilesKilledByOtherLayerUid`
- IntGrid value editor tiles
- tileset `tags`, per-tile enum tags, per-tile custom data; LDtk's built-in `LdtkIcons` atlas (no image file: the tileset and every icon using it are dropped, reported once; a layer whose rules draw from it loses them, with a warning giving the count per layer)
- enum `tags`; external enums (imported as plain enums, no longer synced)
- entity min/max size, `keepAspectRatio`, `allowOutOfBounds`; entities whose pivot lies outside the level are dropped
- entity size not a whole number of cells (rounded up), off-grid entity placement (kept as `offset_px`)
- field `arrayMinLength`/`arrayMaxLength`, `symmetricalRef`, a `defaultOverride` on an array field (LDtk's is per new element), enum values or entity refs that don't resolve (LDtk files can hold stale ones)
- level background images
- rules Worlds can't express or that can never fire (a pattern cell requiring an undefined value or group, a zero modulo, no tiles): the rule is dropped. LDtk never fires those either, except a pattern on "any ungrouped value" in a layer that has groups
- a pattern cell "not <undefined value or group>": always true on imported grids (undefined cells import as empty), so it becomes `*`
- an `outOfBoundsValue` that isn't a defined value: treated as unset, as LDtk's own `tidy()` does
- a level's tileset override whose atlas rows are a different width from the layer's tileset: LDtk reuses tile *ids* in the override, Worlds reuses `[col, row]`
- an inactive rule group that some level forces on: imported as `optional` (see Rules)
- an AutoLayer whose grid differs from its source's: it uses its source's grid
- the project grid, when it isn't `defaultGridSize` or layers have their own

## Errors

- Layer grids where neither divides the other (16 px and 24 px), or no layer grid that every level is a whole number of cells of. See [Grids](#grids).
- Unreadable JSON or a missing external level file.

## Blocking values

The importer sets `project.json` `blocking_values` to `["solid"]` when the collision layer has a `solid` value. LDtk has no notion of which values are walls, so list any other wall kinds yourself (`breakable`, ramps, ...), in the editor's Project pane or `project.json`. With no collision layer, or no `solid` value, it is left out (and validation reports a collision layer with none). Loading a project whose `blocking_values` is left out applies the same rule (see the migration rule in [format.md](format.md)). The export's `open_edges` are the border cells whose value isn't listed, so `oneway`, `water` and `hazard` cells on an edge count as open.
