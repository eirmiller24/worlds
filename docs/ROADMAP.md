# Worlds roadmap

What Worlds still needs to be a complete level editor for a 2D game, and what each piece is. This is the planning document: once a feature ships, its specification moves into [format.md](format.md), [commands.md](commands.md), [api.md](api.md), [mcp.md](mcp.md) or the editor README, and the entry here shrinks to a line.

How to read an entry: **What** is the feature from the user's side, **Format** what changes on disk, **Commands** the new undo units, **API / MCP** what agents get, **Editor** the UI, **Decisions** what has been settled and what is still open. Entries follow the rules of [DESIGN.md](DESIGN.md): worlds-core owns the behaviour, the editor, the server, the CLI and the MCP server are thin clients; every edit is a command; the files stay small, schema-checked and readable by an agent.

Phases are an order, not a schedule. Within a phase, items are in the order to build them.

## Already covered

These keep coming up in comparisons with other editors and are done: grid painting (brush, line, rectangle, fill, eraser), stamps, autotiling with passes, derived grids, region rules, templates and macros, typed entities with references and limits, the searchable palette, level fields and tags, notes, pins, paths, regions and guides with guide groups, mixed selection with move, nudge, duplicate and delete, unlimited undo, autosave and crash recovery, hot reload of images and files, live validation, the JSON Schemas, Godot export and the addon, the LDtk importer, the live API and the MCP server.

## Phase 1: foundations

The three features the rest of the roadmap leans on. Multi-world comes first: viewport tabs and project import both need a world to point at.

### 1.1 Multiple worlds per project

**What.** A project holds several world maps (overworld and dungeon set, chapters, a sequel's new continent), each its own coordinate space with its own levels, world-scope annotations and guide groups. The user creates, renames, reorders and deletes worlds, moves levels between them, and the editor shows one world at a time.

**Today.** The format already has `worlds/<world>/` and `Project.worlds` is a list; the API takes `world` where it is ambiguous and the export indexes per world. But nothing can create a second world (no command; `worlds-cli new` makes `main`), and the editor flattens every world into one space: `Project::levels()` iterates all of them and the viewport draws that, so two worlds would overlap on screen. A few editor helpers hard-code `worlds[0]`.

**Format.** No new files. `world.json` already carries the world's `name`. Directory listing order means nothing, so `project.json` gains `worlds: ["main", "underdark"]` as the display order; a world directory not listed is appended and validation says so. Level slugs stay unique across worlds (already the case), so every existing reference by level slug keeps working.

**Commands.** `create_world { slug, name? }`, `rename_world { world, slug?, name? }` (a slug rename moves the directory and rewrites every `world:` reference), `remove_world { world, confirm }` (refused unless the world is empty or `confirm` is set; inverse restores levels and notes), `reorder_worlds { order }`, `move_level_to_world { level, world, world_px? }` (the level keeps its slug, id and content; world-scope links to it stay valid; its inverse moves it back).

**API / MCP.** `project.describe` lists worlds with level counts and bounding boxes. `world.create`, `world.rename`, `world.remove`, `world.reorder`, `level.move_to_world`. MCP: `create_world`, `edit_worlds` (rename, reorder, remove) and a `world` argument on `move_level`. `read_level`'s header already names the world. `view.focus` takes `world`.

**Editor.** An active world in the view state (`.worlds/ui.json` `active_world`), a world switcher in the toolbar (a combo, or tabs once 1.2 lands), New world / Rename / Delete in the command palette and the Project pane, "Move to world" in a level's context menu and the Levels pane (3.1). Fit world (Shift+F) fits the active world. The flattened iteration stays for things that are genuinely project-wide (validation, the entity palette's counts, Find).

**Decisions.** One project grid for all worlds (a per-world grid would multiply every layer rule). Layers, tilesets, entity types, enums and rules stay project-wide; a world is a map, not a sub-project. The LDtk importer should map each LDtk world to a Worlds world (verify what it does now with a multi-world LDtk file).

### 1.2 Multiple viewport tabs

**What.** Open any number of map views as dock tabs (or side by side). Each tab looks at one world at its own camera position and active level: the overworld in one tab, the dungeon in another, or two ends of the same world. Dragging a tab out of the center splits the view.

**Format.** Nothing on disk in the project. `.worlds/ui.json` stores the tabs: `viewports: [{ id, world, camera, active_level }]` and which one is focused.

**Editor.** `Pane::Viewport` becomes `Pane::Viewport(ViewportId)`. Per tab: camera, world, active level, selection framing. Global (shared by every tab): tool, Paint mode, brush, active layer, layer visibility, lock and opacity, annotation visibility, selection. The focused tab is where keyboard shortcuts and single-key tools apply; the status bar reads from the tab under the pointer. "New viewport" and "Duplicate viewport" in the command palette and the tab's context menu; closing the last tab is refused. The GPU chunk cache is camera-independent and shared, so the cost of a second tab is one more set of draw calls. Optional: "Link cameras" between two tabs (same zoom and relative pan) for comparing areas.

**API / MCP.** `view.focus`, `view.select` and `view.snapshot` take an optional `viewport` (id); default is the focused tab. `view.list_viewports`. `view.open_viewport { world, focus? }` returns the id. MCP `screenshot` takes the same optional argument; `focus` opens nothing on its own.

**Decisions.** Tool, brush and active layer are global so switching tabs never changes what the next click paints (the Blender and Photoshop convention). Selection is global so a copy in one tab and a paste in another work (2.1). Camera bookmarks (3.6) are the natural companion.

### 1.3 Import from another Worlds project

**What.** Bring part of another Worlds project into the open one and choose what: definitions (layers, tilesets, entity types, enums, rules, macros, level fields, project settings), whole worlds, or individual levels with everything they need. The headline use: start a sequel from a finished game with one "definitions only" import, then design levels.

**Core.** `worlds_core::merge`: `plan(src: &Project, dst: &Project, selection, policy) -> Plan` and `Plan::commands() -> Vec<Command>`. The plan is computed first and shown before anything changes: every item to add, every collision and what the policy does with it, every dependency pulled in, every id re-rolled. Applying it is one batch of existing commands (`set_layer_def`, `set_tileset`, `set_entity_def`, `set_enum`, `set_rule_group`, `set_macro`, `create_level`, `replace_level`, `add_note`, `set_guide_group`, `create_world`), so undo is one entry and the live API path needs nothing new.

- **Selection**: a checkbox tree. Definitions by kind and slug; worlds; levels. Checking a level checks its dependencies (its layers, their tilesets, the entity types it places, their enums, the rules of its auto layers and the macros they use) and greys them out as required. Presets: *Definitions only*, *Everything*, *Pick*.
- **Collisions** (a slug the destination already has): per item `skip` (keep ours), `overwrite` (take theirs; refused when it would orphan stored content, as `set_layer_def` already refuses), or `rename` (`<slug>_2`, with every reference inside the imported set rewritten). Identical definitions are reported as "same" and skipped silently.
- **Instance ids** (levels, entities, notes): kept unless the destination has them; a colliding id is re-rolled and every `entity_ref` and `note:` link inside the imported set follows. A reference that points outside the imported set is kept if the destination resolves it, else dropped and reported.
- **Placement**: imported levels go into a new world (default, named after the source project), into an existing world at an offset, or keep their coordinates.
- **Assets**: tileset images and `path` field targets are copied under the project (`art/`), with the same collision policy by file name.

**CLI.** `worlds-cli import-project <src.worlds> --into <dst.worlds> [--defs-only | --all | --levels a,b | --worlds w] [--on-conflict skip|overwrite|rename] [--world <slug> | --new-world <slug>] [--offset x,y] [--dry-run]`. `--dry-run` prints the plan as the dialog shows it, and as JSON with `--json`.

**API / MCP.** `project.import_plan { source, selection, policy }` returns the plan; `project.import { plan_id }` applies it as one batch. MCP `import_project` with the same arguments and a `dry_run` flag, so an agent can show the user the plan and then run it.

**Editor.** File > Import from Worlds project… opens the tree with a search box, the presets, the policy per collision, the placement, and a summary ("12 definitions, 3 levels, 2 renamed, 1 skipped"). Apply is one undo entry labelled `Import from <name>`.

## Phase 2: everyday editing

The tools a designer reaches for every hour. GPT's review ranked these highest and the source agrees: there is no clipboard at all, Ctrl+D does not copy levels, geometry is edited as JSON.

### 2.1 Clipboard

**What.** Copy, cut and paste tiles, grid cells, entities and annotations, within a level, across levels and across worlds. Paste places a ghost that follows the pointer until a click, like entity placement, so the paste lands where you want it.

**Format.** A clipboard **fragment** is JSON, also written to the system clipboard as text so it works between two editor instances, into a file, or into a chat with an agent:

```json
{ "worlds_fragment": 1, "grid_size": 16, "size": [8, 5],
  "grids": { "collision": { "legend": { "#": "solid" }, "rows": ["########", "#......#"] } },
  "tiles": { "decor_back": [ { "cell": [1, 1], "tileset": "castle_main", "tile": [3, 2] } ] },
  "entities": [ { "id": "k3v7q2mzta", "type": "switch", "cell": [2, 1], "fields": { "target": "door22aaaa" } } ],
  "notes": [ ... ] }
```

Cells are relative to the fragment's origin. The same shape is what assemblies (2.3) store on disk.

**Commands.** None new: a paste is one batch of `paint_cells`, `set_tiles`, `add_entity` and `add_note`, one undo entry "Paste 14 items". On paste every instance gets a new id and references inside the fragment are remapped together (the copied switch targets the copied door); a reference to something outside the fragment is kept when the target exists in the destination (cross-level allowed) else nulled, with a problem. Content outside the level is clipped (grid cells and tiles) or refused (entities, with a message). Layers the destination lacks are skipped and reported.

**API / MCP.** `edit.copy { selection }` returns a fragment; `edit.paste { level, layer_map?, origin, fragment }`. MCP `copy_fragment` and `paste_fragment`; `read_level` already gives an agent the text form, so the agent's own "copy" is a read.

**Editor.** Ctrl+C, Ctrl+X, Ctrl+V on the selection (what the marquee picks today: entities, the active tiles layer's stacks, annotations), plus a Copy of a marquee rectangle on the active int_grid layer. Paste ghost with X, Y and R to mirror and rotate before placing (2.4). Paste in place (Ctrl+Shift+V) keeps the original cells.

### 2.2 Duplicate level

**What.** Copy a whole level: every layer's content, entities and notes with new ids and references remapped, fields, tags, per-layer rule state and tileset substitutions. Placed to the right of the original by default (the Ctrl+D convention), or where you drop it.

**Commands.** `duplicate_level { level, slug, name?, world?, world_px? }`. Inverse: `delete_level`.

**API / MCP.** `level.duplicate`; MCP `duplicate_level`.

**Editor.** Ctrl+D with a level selected (today it leaves levels alone), the level's context menu, the Levels pane. The copy becomes the active level and is selected.

### 2.3 Assemblies (prefabs)

**What.** Save a selection as a named, reusable piece of content: a doorway with its door, switch and wiring; a furnished room corner; an encounter. Place it from a palette like a stamp. Independent copies in v1 (edits to a placed copy do not touch the assembly); linked instances with overrides are a stretch goal.

**Format.** `assemblies/<slug>.json`: the fragment shape of 2.1 plus `slug`, `name`, `category` (a `/` path like entity categories), `tags`, `doc`, and a thumbnail rendered into `.worlds/thumbs/`. Committed with the project so a team shares them; importable with 1.3.

**Commands.** `set_assembly { assembly }`, `remove_assembly { slug }`, `rename_assembly`. Placing one is a paste (2.1), so it is a batch of existing commands with the undo label "Place assembly doorway".

**API / MCP.** `defs.get/set/rename` grow a kind `assembly`. MCP: `edit_definitions` takes assemblies; `place_assembly { level, slug, cell }`; `save_assembly { selection, slug, ... }`. `describe_project` lists them.

**Editor.** "Save selection as assembly…" (Ctrl+Shift+S in the map, or the context menu): slug, name, category, tags. A Library part of the Palette with the same search, favorites and category tree as entities, thumbnails from the cache. Click to arm the brush, place with a click; mirror and rotate before placing as for a paste.

### 2.4 Selection and brush transforms

**What.** Mirror and rotate what you are about to place or what you have selected, and arrange levels.

- **Brush transforms**: X mirrors the tile stamp or paste ghost left-right, Y top-bottom, R rotates a quarter turn (square tiles only, as rules already require for `rotate`). Shown in the Paint strip.
- **Selection transforms**: Mirror X, Mirror Y, Rotate 90 on the selection, about its bounding box. Tiles turn with flip bits; int_grid cells move; entities move to the mirrored cell and keep their size and fields (the editor cannot know which field means "facing"); annotations' geometry turns. One batch, one entry.
- **Align and distribute levels**: align selected levels' left, right, top, bottom, horizontal or vertical centers; distribute them with equal gaps horizontally or vertically; "butt together" closes gaps in a direction. Each is one batch of `move_level`. While dragging a level, snap to other levels' edges as it already snaps to guides.

**Decisions.** No free rotation; everything is on the grid. Entity fields are never changed by a transform; a status message says so when a transformed entity has an enum field whose values look directional.

### 2.5 Graphical geometry editing

**What.** Edit paths, regions and guides on the canvas instead of as JSON in the Inspector.

**Editor.** With one annotation selected its vertices are handles: drag moves one (snapping to half the project grid, Ctrl for pixels, guides as today), Alt+click on a segment inserts a vertex, Delete with a vertex highlighted removes it (a path keeps at least 2, a polygon 3), rect regions get edge and corner handles, a note or pin's point is draggable as before. Each gesture is one `update_note`. The JSON editor stays for exact values. Locked annotations show handles but refuse edits, as the tools already do.

### 2.6 Random and scatter brushes

**What.** Paint from a weighted set of tiles or stamps instead of one, and scatter at a density.

**Editor.** On a tiles layer the brush can be a list of alternatives with weights (the same shape as a rule's `out.tiles`), built from a multi-selection in the atlas with a "Random" toggle in the Paint strip. A density slider (1 to 100 %) skips cells on the stroke. The randomness is the brush's, not the project's (no determinism needed; what lands is what is saved). Brushes with alternatives can be saved as named presets in `.worlds/ui.json`.

### 2.7 Layer operations

**What.** Duplicate a layer (definition and every level's content, `<slug>_copy`), merge a tiles layer into the one below it (per level, the upper's stacks over the lower's), and move the selected tiles or entities to another layer of the same kind.

**Commands.** `duplicate_layer { layer, slug }`; merge and move are batches of `set_tiles`, `remove_entity`, `add_entity` and `remove_layer_def`.

### 2.8 Level resize with an anchor, crop to content

**What.** `resize_level` already changes the size from the bottom-right. Add an anchor so a level can grow or shrink at any edge with its content shifting accordingly, and "Crop to content", which shrinks to the bounding box of the non-empty cells, tiles and entities.

**Commands.** `resize_level` grows `anchor: [ax, ay]` (0, 0.5 or 1 per axis; default `[0, 0]`, today's behaviour) and shifts content; entities that fall outside are refused as the tools refuse them today.

## Phase 3: organisation and navigation

Finding your way around a big project, and planning on the map.

### 3.1 Levels pane

**What.** A dock pane listing the active world's levels (or every world, grouped) with thumbnails from the existing cache, name, size, tags and problem count; search by name and tag; sort by name, position or order; drag to reorder (PgUp/PgDn and the export follow that order). Right-click: Frame, Rename, Duplicate (2.2), Move to world (1.1), Delete. Double-click frames the level in the focused viewport.

**Commands.** `reorder_levels { world, order }`.

### 3.2 Find and replace

**What.** A Find pane for project-wide queries: entities by type, tag or field value (`type:door channel:3`, `field:target = null`), levels by tag or field, notes by text or tag, tiles by tileset and atlas tile. Results list with world, level and a preview; click jumps; "Select all" puts them in the selection so a field edit in the Inspector or a Delete applies to all of them. Replace: set a field value across the results in one batch.

**API / MCP.** `entity.find` exists; add `level.find` and `tiles.find`, and MCP `find` wrapping all three. The pane uses the same queries.

### 3.3 Object groups and layer folders

**What.** Two different kinds of grouping that both come up:

- **Object groups**: a named set of entities and annotations in a level (a puzzle, an encounter) that selects and moves together. Clicking a member selects the group; double-click enters the group to pick one member.
- **Layer folders**: `layers.json` layers gain an optional `folder` (`"Decor/Back"`, a `/` path like entity categories). The Layers pane shows folders collapsible with visibility, lock and opacity applied to every layer inside them.

**Format.** `level.json` `groups: [{ "id", "name", "members": [ids] }]` (entities and annotations by id; tile stacks have no id and are not members in v1). `folder` on layer definitions. Nothing in the export: both are editor organisation.

**Commands.** `set_group`, `remove_group`; `folder` is edited with `set_layer_def`.

### 3.4 Reference images

**What.** Put a sketch, a concept painting or a screenshot of the old game behind the grid, position and scale it, set its opacity, lock it. Level scope or world scope. Never exported (a game background is a different feature; see stretch goals).

**Format.** `level.json` and `world.json` grow `references: [{ "id", "image", "px", "scale", "opacity", "locked", "visible" }]`, the image a path under the project like tileset images. Validation reports a missing file.

**Commands.** `add_reference`, `update_reference`, `remove_reference`.

**Editor.** A "References" pseudo-layer at the bottom of the Layers pane with the usual eye and lock; drag to move, corner handles to scale, Inspector for exact values. Images hot-reload like tilesets.

### 3.5 Zoom range

**What.** Zoom is 1.5625 % to 1600 % in fixed steps (`viewport/camera.rs` `ZOOM_STEPS`, 1/64 to 16), so a world larger than 64 times the viewport cannot be fitted. Let Fit world (and `view.focus` with a rect) pick any zoom below the smallest step, add steps 1/128 and 1/256, and have the wheel step from an off-grid zoom to the nearest step. At very small zooms levels already draw as thumbnails, so nothing new needs drawing.

### 3.6 Camera bookmarks

**What.** Save the current view (world, camera) under a name and jump back to it from the command palette, a list in the Levels pane, or Ctrl+Shift+1..9. Per project, in `.worlds/ui.json` (view state, not design intent; a shared landmark is a pin).

### 3.7 Connectors between rooms

**What.** A planning annotation that joins two things and follows them when they move: a line from one room (or a door entity) to another with optional waypoints and a label such as "needs double jump" or "one-way drop". Paths today are fixed polylines; a connector's ends are anchored.

**Format.** A new annotation kind `connector` in `notes.json` or `world.json`: `{ "kind": "connector", "from": { "level": slug } | { "entity": id }, "to": ..., "waypoints": [[x, y], ...], "label"?: string, "style"?: "arrow" | "line" | "double" }` plus the common annotation keys (title, body, tags, color, status, export). Endpoint positions are computed (the nearest edge point of a level, the pivot of an entity), never stored, so moving a room moves the line. Exported like paths when flagged, with `from` and `to` resolved to level slugs and entity ids.

**Validation.** A dangling endpoint is a problem. **Editor.** The Note tool gains Connector: click a room or entity, click another, Enter; waypoints are added and dragged as in 2.5. The Notes pane lists them under their kind.

### 3.8 History pane

**What.** The undo log as a list: label, origin (user, agent, disk), time, files touched; click to undo or redo to that point. The log already exists; this is a view of it. Agent batches expand to show their commands.

### 3.9 World layout modes

**What.** Besides Free (the only layout today), `horizontal`, `vertical` and `grid_vania` as LDtk has them: in a strip layout levels are laid out in order with a gap and the order is what you edit (drag in the Levels pane); in grid vania levels snap to a cell lattice of a chosen size and must be whole cells.

**Format.** `world.json` `layout: { "mode": "free" | "horizontal" | "vertical" | "grid_vania", "gap"?: px, "cell"?: [w, h] }`. `world_px` stays the stored truth in every mode (the export and the game never need to know the mode); the mode drives the tools: strip layouts recompute `world_px` from the order on every reorder, grid vania snaps drags and validation reports an off-lattice level.

## Phase 4: content and the game loop

### 4.1 Per-tile metadata

**What.** Tags and typed data on tiles of a tileset: material, footstep sound, "is grass", damage. Searchable in the palette and Find, carried into the export so the game can read it, and dropped by the LDtk importer today (it warns about `tags`, per-tile enum tags and custom data).

**Format.** `tilesets.json` tileset grows `tile_fields` (field definitions, the shape entity fields use) and a sibling file `tilesets/<slug>.json` holding `{ "tiles": [ { "tile": [col, row], "tags": [...], "fields": {...} } ] }`, sparse, sorted by row then column. The export's `world.json` carries each tileset's tile table.

**Commands.** `set_tile_meta { tileset, tile, tags?, fields? }` (null clears), `set_tileset` grows `tile_fields`.

**Editor.** A tile inspector when a palette tile is selected (tags, fields); the palette filters by tag; Alt+click on the map shows the tile's metadata in the status bar. **Rules**: a later step may let a `require` pattern test tags of a tiles layer; not in the first cut.

### 4.2 Tiled importer

**What.** `worlds-cli import-tiled <map.tmx | map.json | world.world> <out.worlds>`: orthogonal maps only. Tile layers become `tiles` layers (one per Tiled layer, flips carried), tilesets become tilesets (image collections are refused with a message), object layers become `entities` layers with one entity type per Tiled class or type and fields from custom properties, each map a level, a `.world` file a world with positions. Tiled's terrains and Wang sets are not converted (a warning lists them; Worlds rules are the replacement). Same report style as the LDtk importer (Carried / Warned / Errors).

### 4.3 Play from here

**What.** One key launches the game at the current level, or at the cell under the pointer. The design doc rules out an embedded play mode; this is a configurable external command, which is what every engine integration actually needs.

**Format.** `project.json` `run: { "command": "godot --path ../game -- --level {level} --at {px_x},{px_y}", "cwd"?: path, "bake_first"?: bool }`. Placeholders: `{project}`, `{export}`, `{world}`, `{level}`, `{cell_x}`, `{cell_y}`, `{px_x}`, `{px_y}`, `{selection_id}`.

**Editor.** A Play button in the toolbar and Ctrl+F5 (F5 is reload); with `bake_first` the export is written first from the cached bakes. Output streams into the Log pane; a non-zero exit is a toast. **API / MCP.** `project.run { level?, at? }`; MCP `run_game`. The Godot addon gets a small runtime entry point that reads those arguments and loads the level scene.

### 4.4 A second engine

**What.** The export is engine-neutral JSON; the Godot addon is the only reader. Add one more first-party reader so the format is proven generic: a Rust crate `worlds-export` that loads `export/` into typed structs (useful for Bevy and for tests), and later a Unity package if there is demand. Both are checked by the same `check-export` fixtures as the Godot addon.

### 4.5 User-defined lint rules

**What.** Declarative checks the project's designer writes once and the problems panel, `worlds-cli validate` and `bake_and_validate` run forever after: "every level has exactly one `player_spawn`", "no `exit` whose `target` is null", "every level tagged `boss` has the field `music` set". Fits the agent story: an agent can read the lints to learn the project's rules and write new ones.

**Format.** `lints/<slug>.json`, one per lint:

```json
{ "slug": "one_spawn", "severity": "error", "doc": "Every room needs exactly one spawn.",
  "scope": "level", "where"?: { "tags": ["playable"] },
  "check": { "count": { "entity": "player_spawn" }, "min": 1, "max": 1 } }
```

The check vocabulary starts small: `count` of entities by type or tag (min, max) per level or per world; `field` constraints (`not_null`, `in`, `matches`) on entities of a type or on levels; `pair` (for every entity of type A with field f, an entity of type B whose field g equals it exists). Anything else waits for a real need. **Validation** reports lint failures with the lint's slug and `doc`, at the offending level or entity, so the agent's `bake_and_validate` shows them too.

### 4.6 LDtk parity: constraints that keep content valid

**What.** From the importer's "Warned (meaning Worlds can't hold yet)" list, the items that stop invalid content rather than change how it looks:

- entity `min_size` / `max_size` and `keep_aspect` (enforced by the entity tool's Shift-drag and by validation), `allow_out_of_bounds` (today an entity outside its level is always a problem);
- array fields `min_len` / `max_len`; `entity_ref` `symmetric` (setting A's ref to B sets B's ref to A, as one batch);
- layer `offset_px` (a layer drawn shifted; exported; useful for half-cell decor);
- `entities` layers `required_tags` / `excluded_tags` (which types a layer accepts; the palette hides the rest and validation reports strays);
- an `icon` tile on int_grid values for the palette.

Display-only LDtk settings (nine-slice entity rendering, per-field presentation modes) stay dropped.

## Phase 5: platform

### 5.1 Windows and macOS

**What.** The design doc says Linux-only for v1 and "fully featured" ends that. eframe, wgpu, rfd and notify are cross-platform already; the work is the session lock (`editor.lock` uses an OS file lock: verify on NTFS and APFS), path handling (`/`-separated paths in files are already the rule), the `--serve` discovery file, a CI matrix with the GPU tests on a software adapter, packaging and signing. The MCP server and CLI should ship for all three at the same time, since an agent's game repo may live anywhere.

## Stretch goals

Worth doing for some games, deliberately not planned. Each one is a format change, so if one is picked up it gets a full entry above first.

- **Tile animation.** Frames and durations per tile in the tile metadata (4.1), a preview in the palette, exported for the game. Worlds has no animation model today; the game owns time.
- **Shaped collision.** Slopes, polygons and explicit one-way shapes per tile or per collision value. Today the Godot builder makes rectangles from blocking cells, which is right for most tile games; ramps are a collision value the game interprets.
- **Hexagonal and isometric grids.** Everything in the model and the rule engine assumes a square orthogonal grid. Tiled covers these well.
- **Level background images and parallax.** A game-facing background (as opposed to a reference image, 3.4) with scroll factors, exported. Simple to add once the game needs it.
- **Linked assembly instances.** An assembly placed as a live instance with per-instance overrides, so editing the assembly updates every copy (Tiled's templates). Needs an instance file shape and a conflict story; v1 copies are enough to start.
- **Direct Aseprite loading.** Read `.aseprite` files as tileset images. Image hot-reload already covers the export-a-PNG workflow.
- **Nine-slice and other entity presentation modes** from LDtk.
- **Multi-user editing.** The design doc's one-writer model is deliberate; the file format is already git-friendly, and a rendered diff of two revisions on the canvas (reusing the crash-recovery diff) would be the next step before any live collaboration.
- **In-editor scripting or a plugin system, and a web build.** Listed as out of scope in the design doc; the MCP server is the extension point.
