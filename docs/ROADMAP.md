# Worlds roadmap

What Worlds still needs to be a complete level editor for a 2D game, and what each piece is. This is the planning document: once a feature ships, its specification moves into [format.md](format.md), [commands.md](commands.md), [api.md](api.md), [mcp.md](mcp.md) or the editor README, and the entry here shrinks to a line.

How to read an entry: **What** is the feature from the user's side, **Format** what changes on disk, **Commands** the new undo units, **API / MCP** what agents get, **Editor** the UI, **Decisions** what has been settled and what is still open. Entries follow the rules of [DESIGN.md](DESIGN.md): worlds-core owns the behaviour, the editor, the server, the CLI and the MCP server are thin clients; every edit is a command; the files stay small, schema-checked and readable by an agent.

Phases are an order, not a schedule. Within a phase, items are in the order to build them.

## Already covered

These keep coming up in comparisons with other editors and are done: grid painting (brush, line, rectangle, fill, eraser), stamps, autotiling with passes, derived grids, region rules, templates and macros, typed entities with references and limits, the searchable palette, level fields and tags, notes, pins, paths, regions and guides with guide groups, mixed selection with move, nudge, duplicate and delete, unlimited undo, autosave and crash recovery, hot reload of images and files, live validation, the JSON Schemas, Godot export and the addon, the LDtk importer, the live API and the MCP server.

## Phase 1: foundations

The three features the rest of the roadmap leans on. Multi-world comes first: viewport tabs and project import both need a world to point at.

### 1.1 Multiple worlds per project

Shipped. The spec now lives in [format.md](format.md) (`project.json` `worlds`, `worlds/<world>/world.json`), [commands.md](commands.md#world-commands), [api.md](api.md) (`world.*`, `level.move_to_world`, `view.focus { world }`), [mcp.md](mcp.md) (`create_world`, `edit_worlds`, `move_level`) and the [editor README](../crates/worlds-editor/README.md#worlds).

### 1.2 Multiple viewport tabs

Shipped: the editor README's "Map tabs and bookmarks" (per-tab camera, world and level; focus, New / Duplicate / Close, Link cameras, `ui.json` `viewports`), `view.list_viewports`, `view.open_viewport` and the `viewport` argument of `view.focus`, `view.select` and `view.snapshot` in [api.md](api.md#view), MCP `screenshot`'s `viewport` in [mcp.md](mcp.md#screenshot).

### 1.3 Import from another Worlds project

Shipped. The spec now lives in [importing_worlds.md](importing_worlds.md) (`worlds_core::merge`, `worlds-cli import-project`), [api.md](api.md#import-from-another-project) (`project.import_plan`, `project.import`), [mcp.md](mcp.md#import_project) (`import_project`) and the [editor README](../crates/worlds-editor/README.md#import-from-worlds-project) (Import…, `"ui": "import_project"`).

## Phase 2: everyday editing

The tools a designer reaches for every hour. GPT's review ranked these highest and the source agrees: there is no clipboard at all, Ctrl+D does not copy levels, geometry is edited as JSON.

### 2.1 Clipboard

Shipped: the fragment format is in [format.md](format.md#clipboard-fragments), `edit.copy`/`edit.paste` in [api.md](api.md#clipboard-copy-and-paste), `copy_fragment`/`paste_fragment` in [mcp.md](mcp.md#copy_fragment), Ctrl+C/X/V, the paste ghost and Ctrl+Shift+V in the [editor README](../crates/worlds-editor/README.md) (mirror and rotate of the ghost are 2.4).

### 2.2 Duplicate level

Shipped: `duplicate_level` is in [commands.md](commands.md#duplicate_level), `level.duplicate` in [api.md](api.md#level), MCP `duplicate_level` in [mcp.md](mcp.md#duplicate_level), Ctrl+D on a level, its context menu and the level Inspector in the [editor README](../crates/worlds-editor/README.md) and the Levels pane's row menu.

### 2.3 Assemblies (prefabs)

Shipped: `assemblies/<slug>.json` is in [format.md](format.md#assembliesslugjson), `set_assembly`, `remove_assembly`, `rename_assembly` and placing (a paste) in [commands.md](commands.md#assembly-commands), `defs.*` kind `assembly`, `edit.save_assembly` and `edit.place_assembly` in [api.md](api.md#assemblies-and-transforms), MCP `save_assembly`, `place_assembly` and assemblies in `edit_definitions` and `describe_project` in [mcp.md](mcp.md#save_assembly), Save selection as assembly (Ctrl+Shift+S) and the Palette's Library with cached thumbnails in the [editor README](../crates/worlds-editor/README.md) (rename and removal of what an assembly names do not follow into it yet: validation reports them).

### 2.4 Selection and brush transforms

Shipped: `worlds_core::transform` (fragments, selections, arranging levels) in [commands.md](commands.md#transforms), `edit.paste`/`edit.place_assembly` `flip`, `edit.transform` and `level.arrange` in [api.md](api.md#assemblies-and-transforms), MCP `arrange_levels` and `flip` in [mcp.md](mcp.md#arrange_levels), X/Y/R on the paste ghost and the tile stamp, Mirror X/Y and Rotate 90 on the selection, Arrange levels and level-edge snapping in the [editor README](../crates/worlds-editor/README.md).

### 2.5 Graphical geometry editing

Shipped: the Select tool's geometry handles and the `vertex` script gesture are specified in the editor README ("Notes and guides", `--script`).

### 2.6 Random and scatter brushes

Shipped: the editor README's "Random and scatter brushes" (Paint strip, palette, presets in `ui.json`, the script `brush` forms).

### 2.7 Layer operations

Shipped: `duplicate_layer` in [commands.md](commands.md#layer-operations) (merge down and move to layer are `layer_ops` batches there), `defs.duplicate`, `defs.merge_down`, `tiles.move_to_layer` and `entity.move_to_layer` in [api.md](api.md#layer-operations), `layer_operation` in [mcp.md](mcp.md#layer_operation), the Layers pane's menu in the [editor README](../crates/worlds-editor/README.md#layer-operations).

### 2.8 Level resize with an anchor, crop to content

Shipped: [commands.md](commands.md#resize_level) (`anchor`, crop to content), `level.crop` in [api.md](api.md), `resize_level` and `crop_level` in [mcp.md](mcp.md), the Inspector's Resize in the editor README.

## Phase 3: organisation and navigation

Finding your way around a big project, and planning on the map.

### 3.1 Levels pane

Shipped: the level order (`world.json` `levels`) in [format.md](format.md#worldsltworldgtworldjson), `reorder_levels` in [commands.md](commands.md#reorder_levels), `level.reorder` in [api.md](api.md#level-order-world-layouts-and-object-groups), MCP `reorder_levels` in [mcp.md](mcp.md#reorder_levels), the Levels pane (Ctrl+L, thumbnails, search, sort, drag to reorder) and the `"ui": "levels"` step in the [editor README](../crates/worlds-editor/README.md#the-levels-pane) (the camera bookmarks list of 3.6 has a slot there).

### 3.2 Find and replace

Shipped: the query language is `worlds_core::find`, `entity.find`/`level.find`/`tiles.find` and `notes.list`'s `text`/`query` in [api.md](api.md#find-entities-levels-notes-and-tiles), MCP `find` in [mcp.md](mcp.md#find), the Find pane (Ctrl+F, Select all, Replace as one entry) and the `"ui": "find"` step in the [editor README](../crates/worlds-editor/README.md#the-find-pane).

### 3.3 Object groups and layer folders

Shipped: `groups` and `folder` in [format.md](format.md#object-groups), `set_group`/`remove_group` in [commands.md](commands.md#set_group), `group.*` in [api.md](api.md#level-order-world-layouts-and-object-groups), MCP `edit_groups` in [mcp.md](mcp.md#edit_groups), groups (click, double click, Ctrl+G, Ctrl+Shift+G, the Inspector) and layer folders in the Layers pane in the [editor README](../crates/worlds-editor/README.md#object-groups).

### 3.4 Reference images

Shipped: the format in [format.md](format.md#reference-images), `add_reference`, `update_reference`, `remove_reference` in [commands.md](commands.md#reference-image-commands), `reference.*` in [api.md](api.md#reference-images), the MCP tools in [mcp.md](mcp.md#add_reference), the References pseudo-layer, map and Inspector in the [editor README](../crates/worlds-editor/README.md#reference-images).

### 3.5 Zoom range

Shipped: the editor README's Wheel row (steps 1/256 to 16, fits below them) and `view.focus` in [api.md](api.md).

### 3.6 Camera bookmarks

Shipped: the editor README's "Map tabs and bookmarks" (the toolbar's ★ list, the command palette, Ctrl+Shift+1..9, `ui.json` `bookmarks`, the `"ui": "bookmark"` script step).

### 3.7 Connectors between rooms

Shipped: the `connector` kind is specified in [format.md](format.md#connectors) (export, validation), [api.md](api.md#notes), [mcp.md](mcp.md#add_note) and the editor README ("Notes and guides").

### 3.8 History pane

Shipped: the History pane in the [editor README](../crates/worlds-editor/README.md#the-history-pane); entries' time, files and parts in [commands.md](commands.md#undo-and-redo).

### 3.9 World layout modes

Shipped: `layout` in [format.md](format.md#world-layouts) (the lattice check is `worlds_core::lattice_problem`, which takes any rect), `set_world_layout` and the strip relayout in [commands.md](commands.md#set_world_layout), `world.set_layout` in [api.md](api.md#level-order-world-layouts-and-object-groups), `edit_worlds` `layout` in [mcp.md](mcp.md#edit_worlds), the Project pane's Layout, the lattice on the map and snapped drags in the [editor README](../crates/worlds-editor/README.md#world-layouts).

### 4.1 Non-rectangular levels

Shipped: a level's `void` rects; the spec is in [format.md](format.md#non-rectangular-levels-void) (and its export section), [commands.md](commands.md#set_level_void), [api.md](api.md#non-rectangular-levels), [mcp.md](mcp.md) (`shape_level`) and the editor README (Non-rectangular levels). Export stayed `format` 1; grid vania (3.9) should require whole lattice cells of void.

### 4.2 Per-tile metadata

**What.** Tags and typed data on tiles of a tileset: material, footstep sound, "is grass", damage. Searchable in the palette and Find, carried into the export so the game can read it, and dropped by the LDtk importer today (it warns about `tags`, per-tile enum tags and custom data).

**Format.** `tilesets.json` tileset grows `tile_fields` (field definitions, the shape entity fields use) and a sibling file `tilesets/<slug>.json` holding `{ "tiles": [ { "tile": [col, row], "tags": [...], "fields": {...} } ] }`, sparse, sorted by row then column. The export's `world.json` carries each tileset's tile table.

**Commands.** `set_tile_meta { tileset, tile, tags?, fields? }` (null clears), `set_tileset` grows `tile_fields`.

**Editor.** A tile inspector when a palette tile is selected (tags, fields); the palette filters by tag; Alt+click on the map shows the tile's metadata in the status bar. **Rules**: a later step may let a `require` pattern test tags of a tiles layer; not in the first cut.

### 4.3 Tiled importer

**What.** `worlds-cli import-tiled <map.tmx | map.json | world.world> <out.worlds>`: orthogonal maps only. Tile layers become `tiles` layers (one per Tiled layer, flips carried), tilesets become tilesets (image collections are refused with a message), object layers become `entities` layers with one entity type per Tiled class or type and fields from custom properties, each map a level, a `.world` file a world with positions. Tiled's terrains and Wang sets are not converted (a warning lists them; Worlds rules are the replacement). Same report style as the LDtk importer (Carried / Warned / Errors).

### 4.4 Play from here

**What.** One key launches the game at the current level, or at the cell under the pointer. The design doc rules out an embedded play mode; this is a configurable external command, which is what every engine integration actually needs.

**Format.** `project.json` `run: { "command": "godot --path ../game -- --level {level} --at {px_x},{px_y}", "cwd"?: path, "bake_first"?: bool }`. Placeholders: `{project}`, `{export}`, `{world}`, `{level}`, `{cell_x}`, `{cell_y}`, `{px_x}`, `{px_y}`, `{selection_id}`.

**Editor.** A Play button in the toolbar and Ctrl+F5 (F5 is reload); with `bake_first` the export is written first from the cached bakes. Output streams into the Log pane; a non-zero exit is a toast. **API / MCP.** `project.run { level?, at? }`; MCP `run_game`. The Godot addon gets a small runtime entry point that reads those arguments and loads the level scene.

### 4.5 A second engine

**What.** The export is engine-neutral JSON; the Godot addon is the only reader. Add one more first-party reader so the format is proven generic: a Rust crate `worlds-export` that loads `export/` into typed structs (useful for Bevy and for tests), and later a Unity package if there is demand. Both are checked by the same `check-export` fixtures as the Godot addon.

### 4.6 Scripting packages for Python and JavaScript

**What.** First-party client libraries for the live API, one for Python (3.10+) and one for JavaScript and TypeScript (Node 22+, Deno, Bun), so a project can be scripted from outside: procedural generation, bulk fixes, pipeline steps, project-specific checks. With the editor open a script's edits show live and land in the undo history as the client's; without it the package starts a headless server. Scripting is the API with nicer hands; nothing runs inside the editor.

```python
import worlds

with worlds.open("mygame.worlds", client="cavegen") as w, w.batch("Carve caves"):
    for level in w.project.describe()["levels"]:
        w.grid.paint_cells(level=level["slug"], layer="collision", cells=carve(level))
```

```ts
import { open } from "@worlds/client";

const w = await open("mygame.worlds", { client: "cavegen" });
await w.batch("Carve caves", async () => {
  for (const level of (await w.project.describe()).levels)
    await w.grid.paintCells({ level: level.slug, layer: "collision", cells: carve(level) });
});
await w.close();
```

**Packages.** Both thin and with the same behaviour: discovery from `.worlds/server.json` with the liveness check of [api.md](api.md#discovery-worldsserverjson); `session.hello` with the token and a client name; request and response matching, with notifications as callbacks (Python) or an async iterator (JS); the API's errors as typed exceptions (`StaleRev` carrying `expected` and `current`, the command errors by code); `batch` as a context manager or a callback around `history.begin_batch`/`end_batch` that ends the batch when the body throws; approval mode's `queued` status and `approval_wait_ms` passed through. With no live editor, `open` starts `worlds-server <dir>` as a child process (`$WORLDS_SERVER` or `PATH`, with an opt-in `save_on_write`) and stops it on close. Python depends on `websockets` only (its sync client); JS on nothing (the built-in `WebSocket`, which sends no `Origin`). Browsers can't connect, by design: the server refuses any handshake with an `Origin`.

**Generated surface.** Method wrappers and their parameter and result types (Python `TypedDict`s with a `.pyi`, TypeScript `.d.ts`) are generated from a method catalog, so the clients can't drift from the server. `worlds-cli schema` also writes `schema/api.json`: every method with its params and result as JSON Schema, from the same types the handler parses (the commands have schemas already; results need `JsonSchema` derives). A test fails when the catalog and the handler's method table disagree, and CI checks that the generated code is up to date. `call(method, params)` stays as the escape hatch.

**Where.** `clients/python/` and `clients/js/` in this repo, versioned with the API protocol: a client refuses a `session.hello` protocol it doesn't know. Published to PyPI and npm. Each package's tests run in CI against `worlds-server` on the mini fixture with the same scenarios: describe, paint, a batch is one undo entry, a stale `expect_rev` raises, events arrive, the headless fallback starts and stops. `docs/scripting.md` has getting started in both languages and recipes (generate a cave, rename a field value across every entity, a pre-bake check in CI), and the hand-rolled Python client in [api.md](api.md#getting-started) becomes a pointer to it.

**Decisions.** External processes, not an embedded interpreter: the live API already gives scripts undo, approval, ordering and the gesture wait, and a script that crashes or loops forever can't take the editor with it (embedding is a stretch goal). No `worlds-cli run` wrapper: the package starts the headless server itself. Python is sync-only at first, since scripts are mostly sequential; asyncio can follow. Open: the package names (`worlds` is likely taken on PyPI).

### 4.7 User-defined lint rules

**What.** Declarative checks the project's designer writes once and the problems panel, `worlds-cli validate` and `bake_and_validate` run forever after: "every level has exactly one `player_spawn`", "no `exit` whose `target` is null", "every level tagged `boss` has the field `music` set". Fits the agent story: an agent can read the lints to learn the project's rules and write new ones.

**Format.** `lints/<slug>.json`, one per lint:

```json
{ "slug": "one_spawn", "severity": "error", "doc": "Every room needs exactly one spawn.",
  "scope": "level", "where"?: { "tags": ["playable"] },
  "check": { "count": { "entity": "player_spawn" }, "min": 1, "max": 1 } }
```

The check vocabulary starts small: `count` of entities by type or tag (min, max) per level or per world; `field` constraints (`not_null`, `in`, `matches`) on entities of a type or on levels; `pair` (for every entity of type A with field f, an entity of type B whose field g equals it exists). Anything else waits for a real need. **Validation** reports lint failures with the lint's slug and `doc`, at the offending level or entity, so the agent's `bake_and_validate` shows them too.

### 4.8 LDtk parity: constraints that keep content valid

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

- **Tile animation.** Frames and durations per tile in the tile metadata (4.2), a preview in the palette, exported for the game. Worlds has no animation model today; the game owns time.
- **Shaped collision.** Slopes, polygons and explicit one-way shapes per tile or per collision value. Today the Godot builder makes rectangles from blocking cells, which is right for most tile games; ramps are a collision value the game interprets.
- **Hexagonal and isometric grids.** Everything in the model and the rule engine assumes a square orthogonal grid. Tiled covers these well.
- **Level background images and parallax.** A game-facing background (as opposed to a reference image, 3.4) with scroll factors, exported. Simple to add once the game needs it.
- **Linked assembly instances.** An assembly placed as a live instance with per-instance overrides, so editing the assembly updates every copy (Tiled's templates). Needs an instance file shape and a conflict story; v1 copies are enough to start.
- **Direct Aseprite loading.** Read `.aseprite` files as tileset images. Image hot-reload already covers the export-a-PNG workflow.
- **Nine-slice and other entity presentation modes** from LDtk.
- **Multi-user editing.** The design doc's one-writer model is deliberate; the file format is already git-friendly, and a rendered diff of two revisions on the canvas (reusing the crash-recovery diff) would be the next step before any live collaboration.
- **An embedded scripting language.** A Script pane that runs Lua (`mlua`, vendored) on a worker thread as one more live API connection, so ordering, the gesture wait, approval and undo come from the API unchanged; output in the Log pane, a Stop button, scripts bindable to toolbar buttons. Lua over embedded Python (libpython to ship on three platforms, no real sandbox, the GIL against the UI thread) and over JavaScript (more runtime for the same API). Only worth it if the scripting packages (4.6) show a need for tools inside the editor. Scripts inside the data (custom rules, brushes or export hooks the bake runs) are a much bigger step: a deterministic runtime, declared inputs for the incremental bake, and opening a project would run code.
- **A plugin system and a web build.** Listed as out of scope in the design doc; the scripting packages (4.6) and the MCP server are the extension points.
