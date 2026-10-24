# Worlds live API (worlds-server)

The editor hosts a JSON-RPC 2.0 server so agents and tools can read and edit the open project while you watch. Every write is one of the [commands](commands.md) with a wire wrapper, lands in the editor's undo stack as one entry, and shows on the canvas the next frame. `worlds-mcp` ([mcp.md](mcp.md)) is the main client; anything that speaks WebSocket and JSON can use it too.

The same handler also runs **headless**, over a `Project` loaded from disk without the editor. `worlds-mcp` uses that when no editor is open, and the tests use it. Differences are called out where they exist; the short list is in [Headless mode](#headless-mode).

## Getting started

Three ways in, all running the same handler:

| You have | Run | Then |
| --- | --- | --- |
| The editor open on the project (`worlds <dir>`) | nothing | connect to the editor's socket; edits show live and land in its undo history |
| No editor | `worlds-server <dir> [--save-on-write]` | connect to its socket; it prints `{"port", "pid", "project"}` once listening and serves until SIGINT or SIGTERM. Without `--save-on-write`, nothing reaches disk until you call `project.save` |
| Rust code | `worlds_server::headless::Session` | call methods in-process, no socket (see [Headless mode](#headless-mode)) |

Over the socket: read `<project>/.worlds/server.json` for `port` and `token` ([below](#discovery-worldsserverjson)), open `ws://127.0.0.1:<port>/` **without an `Origin` header**, send [`session.hello`](#sessionhello) first, then one request per text frame. A minimal Python client (`pip install websocket-client`, whose `Origin` header has to be turned off with `suppress_origin`; Node's built-in `WebSocket` sends none):

```python
import json, sys
from websocket import create_connection

info = json.load(open(sys.argv[1] + "/.worlds/server.json"))
ws = create_connection(f"ws://127.0.0.1:{info['port']}/", suppress_origin=True)
next_id = 0

def call(method, **params):
    global next_id
    next_id += 1
    ws.send(json.dumps({"jsonrpc": "2.0", "id": next_id, "method": method, "params": params}))
    while True:
        msg = json.loads(ws.recv())
        if msg.get("id") == next_id:          # skip notifications
            if "error" in msg:
                raise RuntimeError(msg["error"]["message"])
            return msg["result"]

call("session.hello", token=info["token"], client={"name": "my-script", "version": "1"})
rev = call("project.describe")["rev"]
print(call("grid.fill_rect", level="crevasse", layer="collision",
           rect=[2, 2, 3, 1], value="water", expect_rev=rev))
```

From there: [`project.describe`](#project) is the map (layers, values, entity types, levels), `level.get` with `format: "text"` shows a level, and every write is one undo entry. A script that makes several writes should wrap them in [`history.begin_batch`/`end_batch`](#history) so the user can undo them in one step.

The project settings that [`project.update`](#project) writes (`name`, `bg_color`, `collision_layer`, `seed`, `grid_size`, `blocking_values`, `default_level_size`, `export_on_save`) are [`set_project_setting`](commands.md#set_project_setting) commands, the same ones the editor's Project tab makes.

## Discovery: `.worlds/server.json`

When the editor opens a project it binds `127.0.0.1` on a free port and writes `<project>/.worlds/server.json` (created with file mode `0600` as a temp file in `.worlds/`, then renamed over the old one, so a reader never sees a half-written file or a moment where the token is world-readable):

```json
{ "port": 41783, "token": "3f9c0e5a1b7d4c2e8f6a0b1c2d3e4f5a6b7c8d9e0f1a2b3c4d5e6f708192a3b4", "pid": 48211,
  "project": "/home/me/mygame/mygame.worlds" }
```

- `port`: the WebSocket port. `token`: 64 hex chars (32 random bytes), new for every open. `pid`: the editor process. `project`: the absolute, canonical project directory.
- Written on project open, removed on project close, project switch and normal exit (only if the file still holds this editor's `pid` and `token`). Reloading the same project keeps the file and the connections.
- **Liveness.** A client treats the file as live when the `pid` is alive (`kill(pid, 0)` succeeds) *and* a connection to `port` is accepted. Otherwise the file is stale: the editor crashed or the pid was reused. Clients never delete it; the next editor to open the project overwrites it.
- **Second editor.** If an editor opens a project whose `server.json` is live and belongs to another process, it shows a warning and runs without a server (one writer per project). It doesn't touch the file. Starting a server first takes an exclusive lock on `.worlds/server.lock` (held while it runs, released by the OS if the process dies), so checking `server.json` and publishing a new one is one step: of two editors opening a project at the same moment, one serves and the other gets the warning.

## Transport

- WebSocket on `ws://127.0.0.1:<port>/`. Any other path gets HTTP 404. A handshake carrying an `Origin` header (a browser) is refused with HTTP 403, so web pages can't reach the server even if they guess the port.
- Text frames, one JSON-RPC message per frame, UTF-8. Inbound messages (client to server) are capped at 16 MiB, which is far more than any request needs; a bigger one closes the socket (code 1009). Outbound messages may be up to 64 MiB, since snapshots are large. Binary frames close the socket (code 1003).
- **Batch arrays are not supported.** An array gets one error response, `-32600` with `id: null`. Send one request per frame; use [`history.begin_batch`](#history) to group edits.
- Requests need an `id` (number or string), echoed in the response. A message without `id` (a client notification) is ignored.
- Server-to-client notifications are JSON-RPC notifications: `{ "jsonrpc": "2.0", "method": "<event>", "params": {...} }`.

### session.hello

The first message on a connection must be `session.hello`. Anything else before a successful hello gets error `-32000` and the socket is closed (code 1008). A wrong token does the same; the token is compared in constant time. No hello within 5 s closes the socket.

| Param | Type | |
| --- | --- | --- |
| `token` | string | from `server.json` |
| `client` | `{ name, version }` | `name` is 1 to 32 chars of `a-z 0-9 - _ .`; it names this client in the undo history, toasts and events: every edit it makes is recorded as `Origin::Agent(name)` (`{ "agent": "<name>" }` on the wire). `worlds-mcp` passes the MCP client's name (`claude-code`) |
| `approval_wait_ms` | integer, optional | how long a write may block waiting for approval (see [Approval mode](#approval-mode)); default 20000, 0 to 120000 |

Result: the protocol version (`1`), the server, the project, the current `rev`, whether this is a headless server, whether approval mode is on, and `session`: a short random id for this connection (8 characters of `a-z 0-9`). Two clients can share a name (two `claude-code` windows); `changed` and `agent` events carry the `session` of the connection that made the edit so they can be told apart. The undo history only records the name (`Origin::Agent(name)`).

```json
{ "jsonrpc": "2.0", "id": 1, "method": "session.hello",
  "params": { "token": "3f9c0e5a1b7d4c2e8f6a0b1c2d3e4f5a6b7c8d9e0f1a2b3c4d5e6f708192a3b4", "client": { "name": "claude-code", "version": "2.1.0" } } }
```

```json
{ "jsonrpc": "2.0", "id": 1, "result": { "protocol": 1, "server": { "name": "worlds", "version": "0.1.0" },
  "project": { "dir": "/tmp/mini.worlds", "name": "Mini" }, "rev": 0, "headless": true, "approval": false, "session": "k3v9q2xa" } }
```

## Revisions

`rev` is a u64 per open project. It starts at 0 when the project is loaded and goes up by exactly one for every applied mutation: each write request, each undo and redo step, each reload (by request or because files changed on disk), and each user edit in the editor (a gesture counts once, when it ends). A write that changes nothing (painting a cell its current value) is not a mutation: `rev` stays, `changes` is empty, nothing is recorded. Revs are monotonic, not contiguous from one client's view: other clients and the user also bump them.

- Every result carries `rev`: for a read, the rev it was read at; for a write, the rev after it.
- Every write method, and `history.undo`/`history.redo`, accepts `expect_rev` (optional). If it isn't the current rev, nothing is applied and the error is `stale_rev` (`-32001`) with `data: { "expected", "current" }`.
- **Batches.** Inside a [batch](#history) each write applies at once and bumps `rev` by one, as outside; `end_batch` doesn't bump it. In [approval mode](#approval-mode) a batch's writes are only collected, and accepting the batch applies all of it and bumps `rev` once.
- Saving, baking, exporting and validating don't change `rev`.

## Queue and ordering

The editor's UI thread owns the project. Requests arrive on the server's tokio runtime and go into **one queue per connection**; the UI thread drains the queues once per frame, taking requests across connections in arrival order (an arriving request wakes the UI, so an idle editor still answers within a frame). A request that arrives while the user is mid-gesture (a brush stroke, a drag) waits until the gesture ends or is cancelled; that holds for reads too, so nobody sees half a stroke.

- **Per connection, strictly in order.** A connection's requests are handled one at a time in the order sent, and responses come back in that order. A request waiting for approval holds up that connection's later requests until it is decided or returns `queued`.
- **Across connections**, requests interleave in arrival order, except that while one connection has an open batch, the drain skips another connection's *write* (it stays at the head of its queue, and that connection's later requests wait behind it) until the batch ends. Reads from other connections go ahead and see the batch so far. One connection's parked write never holds up a third connection.
- **Approval never blocks the UI thread.** A write in approval mode is checked and submitted as a pending batch on the UI thread, which then moves on; the wait for the user's decision happens on the connection's side, in the server runtime.
- **Headless** has no frames and no gestures: each request applies as soon as it is read.

## Writes

Each write method builds one [command](commands.md) (several become one `batch`), applies it, and records it as one undo entry with origin `{ "agent": "<client name>" }` (inside a [batch](#history), it becomes part of the batch's entry). The parameters are the command's fields without `op`, plus:

| Param | |
| --- | --- |
| `expect_rev` | optional; see [Revisions](#revisions) |
| `label` | optional; the undo entry's label and the toast text. Default `<client name>: <command label>` ("claude-code: Paint 3 cells on collision"). A method that builds several commands sends one when it only needs one, else a `batch` labelled like its first command |
| `confirm` | required (`true`) by the destructive methods listed in [Confirmation](#confirmation), ignored elsewhere |

**Levels are addressed by slug.** Level slugs are unique across worlds, so `level` alone finds a level. Commands that name the world (`move_level`, ...) get it filled in by the server; pass `world` only to assert it (a mismatch is the `level_not_in_world` command error). `world` is required only where nothing else says it: `level.create` and world-scope notes, and only when the project has more than one world (with one world it defaults to that one). **Entities and notes are addressed by id**: the 10-character `Id` strings of the files (`doorcrev22`). Methods that create one accept an `id` and generate one when it's left out; the result returns it.

A write's result:

```json
{ "rev": 7, "status": "applied", "label": "claude-code: Place 1 tile on decor_back", "changes": [] }
```

`changes` are the [`Change`](commands.md#changes) objects the command reported, coalesced. `status` is `applied`, or in approval mode `queued` or `collected` (see below). Methods that create something add `id`.

## Confirmation

These, and only these, need `confirm: true`; without it the error is `needs_confirm` (`-32002`) and nothing is applied:

| Method | When |
| --- | --- |
| `level.delete` | always |
| `level.replace` | when the level holds something the new `content` leaves out: an `int_grid` layer's cells or a derived layer's overrides, a `tiles` layer's tiles, an entity or a note (by id). The reason names them ("drops the marks overrides, which the new content leaves out") |
| `defs.remove` | always. For a field kind the reason says how many stored values go with it ("removes field needs_item of locked_door and the values 2 entities store; undo brings them back") |
| `defs.set` of an `entity` | when entities store a value for a field the new definition takes out, or for a field it retypes (the reason says "2 entities store a value for a field that no longer exists or becomes invalid: delay, channel"). A field is retyped when its `type`, its `array` flag or its enum source changes: a project `enum` for inline `values` or the other way round, or another project enum. Reordering fields, or changing anything else about them (default, bounds, inline values, display), is not |
| `defs.set` of an `entity_field` | when it replaces a field and retypes it (as above) while instances of the type store a value for it ("2 entities store a value for field needs_item, whose type changes"). Creating or moving a field never needs it |
| `defs.set` of a `level_field` | the same, counting levels that store a value ("1 level stores a value for level field music, whose type changes") |
| `defs.set` of an `enum` | when a value the new definition takes out is stored somewhere: an entity field, a level field or a field default |
| `project.reload` | when there are unsaved edits (they are discarded) |
| `notes.update`, `notes.move` | only when a locked annotation would move: its geometry or a guide's `group` changes ([notes](#notes)) |
| `notes.remove` | only when a listed annotation is locked |
| `reference.update` | only when a locked reference image would move or scale: its `px` or `scale` changes ([reference images](#reference-images)) |
| `reference.remove` | only when a listed reference image is locked |

`defs.set` never drops data: stored values of a removed field or enum value stay in the files, where `project.validate` reports them, and undo brings the definition back. `confirm` is only there so an agent doesn't do it by accident. An `int_grid` layer's value can't be taken out while a cell holds it at all: that is refused (`command_error`, `invalid`) with or without `confirm`; `grid.replace_value` it first.

Removing entities, unlocked notes, tiles, rule groups, rules, stamps or macros never needs it: the undo entry and toast are the safety net.

## Errors

| Code | Name | Meaning |
| --- | --- | --- |
| -32700 | parse error | not JSON |
| -32600 | invalid request | not a JSON-RPC request object (or a batch array) |
| -32601 | method not found | |
| -32602 | invalid params | wrong shape, unknown key, missing key; `data.message` says which |
| -32603 | internal error | a bug; `data.message` |
| -32000 | `unauthorized` | no or bad hello |
| -32001 | `stale_rev` | `data: { "expected", "current" }` |
| -32002 | `needs_confirm` | `data: { "reason" }`, e.g. "1 entity stores a value for a field that no longer exists or becomes invalid: delay" |
| -32003 | `rejected` | the user rejected the batch in approval mode |
| -32004 | `command_error` | the request names something that doesn't exist or can't be applied; `data` is the `CommandError` (below) |
| -32005 | `unsupported` | not available here: `view.*` in headless mode, a reload in approval mode |
| -32006 | `busy` | the project is closing or loading; retry |
| -32007 | `batch_state` | `end_batch` or `cancel_batch` with no open batch, `begin_batch` with one open |
| -32008 | `io` | saving or exporting failed; `data: { "path", "message" }` |

The JSON-RPC `message` is always a sentence meant for an agent to read. For `command_error`, `data` is the `worlds_core::command::CommandError` as it serializes: `{ "kind": <variant in snake_case>, ...fields, "message": <its Display> }`, and the JSON-RPC `message` is that same text. Every kind:

| `kind` | Fields |
| --- | --- |
| `unknown_world` | `world` |
| `unknown_level` | `level` |
| `level_not_in_world` | `world`, `level` |
| `unknown_layer` | `layer` |
| `wrong_layer_kind` | `layer`, `expected`, `actual` (layer kinds) |
| `unknown_value` | `layer`, `value` |
| `out_of_bounds` | `layer`, `x`, `y`, `w`, `h` (the cell and the layer's size in cells) |
| `grid_size` | `layer`, `actual`, `expected` (`[w, h]`) |
| `undefined_index` | `layer`, `x`, `y`, `index` |
| `unknown_tileset` | `tileset` |
| `unknown_entity_type` | `type` |
| `unknown_field` | `owner` (what should have it, e.g. `entity type "lever"`, or `project level_fields`), `field` |
| `duplicate_field` | `owner`, `field`: renaming a field to a slug its owner already has |
| `unknown_entity` | `id` |
| `unknown_note` | `id` |
| `unknown_guide_group` | `world`, `group` |
| `duplicate_id` | `id` |
| `duplicate_level` | `level` |
| `duplicate_world` | `world` |
| `unknown_enum` | `enum` |
| `unknown_enum_value` | `enum`, `value` |
| `unknown_rule_group` | `layer`, `group` |
| `unknown_rule` | `layer`, `rule` (a rule or template reference slug) |
| `unknown_macro` | `macro_kind` (`pattern`, `legend`, `template`), `name` |
| `unknown_stamp` | `layer`, `stamp` |
| `duplicate_def` | `def_kind` (`layer`, `entity`, `enum`, `tileset`, `rules`, `macros`), `slug` |
| `in_use` | `def_kind` (`tileset`, `macros`, or `stamp` with its `layer`), `slug`, `uses` (the first few, as phrases) |
| `invalid` | none: `message` says it all |
| `batch` | `label`, `index` (of the failed command), `source` (its error, in this same form) |
| `rollback` | `batch` (the batch's error), `failed` (the inverses that were refused, as errors); treat the project as changed |
| `unknown_reference` | `id` (a reference image's) |

New variants follow the same rule: a variant with one unnamed field names it after what it holds.

```json
{ "jsonrpc": "2.0", "id": 9, "error": { "code": -32004, "message": "\"lava\" is not a value of layer \"collision\"",
  "data": { "kind": "unknown_value", "layer": "collision", "value": "lava", "message": "\"lava\" is not a value of layer \"collision\"" } } }
```

## Methods

Examples run in order against a fresh copy of `crates/worlds-core/tests/fixtures/mini.worlds` (see [Testing](#testing)); each `rev`, `status`, `label`, `id`, `entries`, `steps` and `changes` in a response, and the text view `level.get` returns, is what that step really produces. Other values that depend on bake output or file rendering (tile counts, rule firings, saved file lists) are illustrative.

### project

**`project.describe {}`**: the agent's map of the project.

| Key | |
| --- | --- |
| `name`, `dir`, `grid_size`, `bg_color`, `collision_layer`, `seed`, `blocking_values`, `default_level_size`, `export_on_save`, `rev` | project settings (`dir` absolute; `blocking_values` is the effective list, see [format.md](format.md#projectjson); `default_level_size` is `null` when left out) |
| `mode` | `{ headless, approval, pending }`: `pending` counts approval batches waiting for the user |
| `level_fields` | field definitions as in `project.json`, without `name` and `display` (`doc` is kept: it tells an agent what the field is for) |
| `worlds[]` | In display order (`project.json` `worlds`): `{ slug, name, levels: [slug], level_count, bbox, notes, guides, guide_groups }`: `bbox` is `[x, y, w, h]` in world pixels around the world's levels (`null` when it has none), `notes` counts the world-scope annotations that aren't guides, `guides` the world's guides, `guide_groups` is `[{ slug, name?, offset, color?, guides }]` (`guides`: how many guides name the group), `[]` when there are none |
| `levels[]` | `{ slug, name, id, world, world_px, size, size_px, depth, tags, content, notes }`. `tags` is the level's tags (`[]` when it has none). `size` in cells of the project grid, `size_px` in pixels. `content` per layer with content: `{ "cells": n }` (non-empty int_grid cells), `{ "overrides": n }` (a derived layer's hand-painted overrides, suppressed cells included), `{ "tiles": n }`, `{ "entities": n }`. `notes`: `{ open, done }` |
| `layers[]` | `{ slug, name, kind, grid_size }`, plus `values: [{ slug, glyph, color, group? }]` and `groups: [{ slug, glyph? }]` for `int_grid` (and, for a [derived](format.md#derived-layers-overrides) one, `derived: true` and `writer`: the auto layer whose rules write it, `null` when none does), `tileset` for `tiles`, `source`, `rule_groups` (count) and `passes` (the distinct `pass` numbers its groups use, ascending) for `auto`. `grid_size` is the effective one (an `auto` layer's is its source's) |
| `entity_types[]` | `{ slug, name, doc?, category?, tags, size, pivot, resizable, color, icon?, limit?, fields, count }`. `pivot` is the point of the entity that sits on its cell, as fractions of its size (`[0.5, 1.0]` is bottom center). `color` is the type's `#rrggbb`, `icon` its `{ tileset, tile: [col, row] }` atlas tile, `limit` `{ max, scope, behavior }` with the defaults spelled out (`scope` `level`, `behavior` `prevent_adding`). `fields` as in the entity file without `name` and `display` (with `doc` when set); `count` across the project |
| `enums[]` | `{ slug, values: [slug] }` |
| `tilesets[]` | `{ slug, image, tile_size, spacing?, padding?, atlas }`. `atlas` is `[columns, rows]` in tiles, `null` if the image can't be read |
| `notes` | `{ open, done }` across the project. Guides are not counted in any `notes` count: they are reference lines, not work items (`worlds[].guides` counts them) |
| `recovered`? | Only when this editor session started from a crash recovery (the editor's *Recover*): `{ files: [paths recovered], skipped: [conflicting paths left out], session: { pid, written } }`, `session` being the crashed one |

```json
{ "jsonrpc": "2.0", "id": 2, "method": "project.describe", "params": {} }
```

```json
{ "jsonrpc": "2.0", "id": 2, "result": {
  "rev": 0, "name": "Mini", "dir": "/tmp/mini.worlds", "grid_size": 16, "bg_color": "#1a1c2c",
  "collision_layer": "collision", "seed": 42, "blocking_values": ["solid", "fortified", "ramp_l"],
  "default_level_size": null, "export_on_save": false,
  "mode": { "headless": true, "approval": false, "pending": 0 },
  "level_fields": [
    { "slug": "biome", "type": "enum", "enum": "biome", "array": true, "default": [] },
    { "slug": "music", "type": "string", "nullable": true } ],
  "worlds": [ { "slug": "main", "name": "Main", "levels": ["crevasse", "shaft"], "level_count": 2, "bbox": [0, 0, 416, 208],
    "notes": 2, "guides": 1, "guide_groups": [] } ],
  "levels": [
    { "slug": "crevasse", "name": "Crevasse", "id": "crevasse22", "world": "main", "world_px": [0, 0],
      "size": [20, 5], "size_px": [320, 80], "depth": 0, "tags": [],
      "content": { "collision": { "cells": 59 }, "decor_back": { "tiles": 3 }, "objects": { "entities": 2 } },
      "notes": { "open": 1, "done": 1 } },
    { "slug": "shaft", "name": "Shaft", "id": "shaft22222", "world": "main", "world_px": [320, 80],
      "size": [6, 8], "size_px": [96, 128], "depth": 1, "tags": [],
      "content": { "objects": { "entities": 1 } }, "notes": { "open": 0, "done": 0 } } ],
  "layers": [
    { "slug": "collision", "name": "Collision", "kind": "int_grid", "grid_size": 16,
      "values": [
        { "slug": "solid", "glyph": "#", "color": "#404040", "group": "walls" },
        { "slug": "fortified", "glyph": "f", "color": "#606060", "group": "walls" },
        { "slug": "oneway", "glyph": "o", "color": "#a0a040" },
        { "slug": "water", "glyph": "~", "color": "#3060c0" },
        { "slug": "ramp_l", "glyph": "L", "color": "#808080" } ],
      "groups": [ { "slug": "walls", "glyph": "w" } ] },
    { "slug": "main_tiling", "name": "Main tiling", "kind": "auto", "grid_size": 16, "source": "collision", "rule_groups": 4, "passes": [0] },
    { "slug": "decor_back", "name": "Decor (back)", "kind": "tiles", "grid_size": 8, "tileset": "castle_main" },
    { "slug": "objects", "name": "Objects", "kind": "entities", "grid_size": 16 } ],
  "entity_types": [
    { "slug": "floor_button", "name": "Floor Button", "category": "Puzzle/Switches", "tags": ["object", "puzzle"],
      "size": [1, 1], "pivot": [0.5, 1.0], "resizable": [false, false], "color": "#c04040", "count": 1,
      "fields": [
        { "slug": "channel", "type": "int", "default": 0, "min": 0, "max": 15 },
        { "slug": "latching", "type": "bool", "default": false },
        { "slug": "target", "type": "entity_ref", "allowed_types": ["locked_door"], "nullable": true } ] },
    { "slug": "locked_door", "name": "Locked Door", "doc": "Opens when the player holds needs_item. Footprint is w x h tiles.",
      "category": "Puzzle/Doors", "tags": ["object", "puzzle"], "size": [1, 3], "pivot": [0.5, 1.0], "resizable": [false, true],
      "color": "#c08040", "icon": { "tileset": "castle_main", "tile": [3, 7] }, "limit": { "max": 4, "scope": "level", "behavior": "discard_oldest" }, "count": 2,
      "fields": [
        { "slug": "channel", "type": "int", "default": 0, "min": 0 },
        { "slug": "needs_item", "type": "string", "default": "" },
        { "slug": "consumes", "type": "bool", "default": false },
        { "slug": "facing", "type": "enum", "enum": "facing", "default": "left" },
        { "slug": "style", "type": "enum", "values": ["wood", "iron"], "default": "wood" },
        { "slug": "tint", "type": "color", "default": "#ffffff" },
        { "slug": "speed", "type": "float", "default": 1.5, "min": 0.5 },
        { "slug": "label", "type": "text", "nullable": true },
        { "slug": "anchor", "type": "point", "nullable": true },
        { "slug": "waypoints", "type": "point", "array": true, "default": [] },
        { "slug": "exit_to", "type": "entity_ref", "cross_level": true, "allowed_tags": ["object"], "nullable": true },
        { "slug": "sprite", "type": "tile", "tileset": "castle_main", "nullable": true },
        { "slug": "script", "type": "path", "extensions": ["gd"], "nullable": true },
        { "slug": "extra", "type": "json", "default": { "a": 1 } } ] } ],
  "enums": [ { "slug": "biome", "values": ["dry", "flooded"] }, { "slug": "facing", "values": ["left", "right"] } ],
  "tilesets": [
    { "slug": "castle_main", "image": "art/castle_main.png", "tile_size": [16, 16], "atlas": [32, 32] },
    { "slug": "ssw_set5", "image": "art/ssw_set5.png", "tile_size": [32, 32], "spacing": 1, "padding": 2, "atlas": [8, 8] } ],
  "notes": { "open": 3, "done": 1 } } }
```

**`project.validate {}`** → `{ rev, problems }`, each problem `{ file, path, message }` as `worlds-cli validate` reports them. Also refreshes the editor's problems panel and sends `problems`.

```json
{ "jsonrpc": "2.0", "id": 3, "method": "project.validate", "params": {} }
```

```json
{ "jsonrpc": "2.0", "id": 3, "result": { "rev": 0, "problems": [] } }
```

**`project.bake { levels? }`** bakes the `auto` layers of the given levels (default all) and reports what came out, without writing anything. In the editor it uses (and refreshes) the cached bakes. → `{ rev, levels: [{ level, layers: [{ layer, tiles }], error? }], warnings }`; a level whose bake fails (a missing tileset image) carries `error` instead of failing the call. `warnings` are the export warnings of the levels that baked, as `worlds-cli bake` prints them (`worlds_rules::export_warnings`), each a sentence; `[]` when there are none, which is always the case at present (importer-specific rules are checked by `worlds-cli check-export`, [format.md](format.md#checking-an-export)).

**`project.export { levels? }`** bakes and writes `export/` (as `worlds-cli bake`; a full export when `levels` is left out). → `{ rev, dir: "export", files, warnings }`, files relative to the export directory, `warnings` the export warnings of the levels written, as `project.bake` gives them (`[]` when there are none). Fails with `io`, or with `command_error` when a bake fails.

**`project.save {}`** writes the dirty files (`save_dirty`), then, with `export_on_save`, `export/`. → `{ rev, files }` (project-relative). Sends `saved`.

**`project.reload { confirm? }`** reloads from disk, discarding unsaved edits (`confirm: true` needed if there are any) and the undo history. Bumps `rev`; sends `changed` with `"kind": "reload"`. Refused (`unsupported`) in approval mode. A reload, by request or because files changed on disk, closes every open batch without recording it (the history is gone anyway; the owner's `end_batch` then gets `batch_state`) and rejects every pending approval batch (a `batch` notification with status `rejected`).

**`project.update { name?, bg_color?, collision_layer?, seed?, grid_size?, blocking_values?, default_level_size?, export_on_save? }`** is a write: one `set_project_setting` per key given, in that order (a `batch` labelled like its first when there are several, all or nothing). `bg_color`, `collision_layer`, `blocking_values` and `default_level_size` take `null` to unset them (leave the key out of `project.json`). `collision_layer` must name an `int_grid` layer. `grid_size` must be at least 1 and is refused (`invalid`) once the project has a level, since level sizes and positions are in cells of it. `blocking_values` is a list of the collision layer's value slugs (values it doesn't have are left to validation). `default_level_size` is `[w, h]` in cells, both at least 1: the size of a level made with the editor's New Level command. `export_on_save` makes every save also write `export/`. Each setting reports `{ "change": "project", "key" }`; a new `seed` or `grid_size` changes every bake, and any setting change drops the headless bake cache. A setting given its current value changes nothing (no `changes`, no `rev` bump when none of them change). Giving no key is `invalid params`. Examples are in [project settings, rule state and replacing a level](#project-settings-rule-state-and-replacing-a-level).

**`project.read_file { path }`** → `{ rev, path, text }`: a project file (`project.json`, `layers.json`, `entities/<slug>.json`, `worlds/<w>/levels/<l>/<layer>.json`, ...) rendered from memory exactly as saving would write it now, unsaved edits included. `path` is matched as a string against the set of project-relative paths a full save would write right now; nothing is read from the filesystem, so `..`, absolute paths and symlinks can't reach anything. A path not in that set is `command_error` (`invalid`).

```json
{ "jsonrpc": "2.0", "id": 4, "method": "project.bake", "params": { "levels": ["crevasse"] } }
```

```json
{ "jsonrpc": "2.0", "id": 4, "result": { "rev": 0, "levels": [ { "level": "crevasse", "layers": [ { "layer": "main_tiling", "tiles": 113 } ] } ], "warnings": [] } }
```

### level

**`level.list { world? }`** → `{ rev, levels: [{ slug, name, id, world, world_px, size, depth }] }`. To search levels by tag, field or name, see `level.find` ([find](#find-entities-levels-notes-and-tiles)).

```json
{ "jsonrpc": "2.0", "id": 5, "method": "level.list", "params": {} }
```

```json
{ "jsonrpc": "2.0", "id": 5, "result": { "rev": 0, "levels": [
  { "slug": "crevasse", "name": "Crevasse", "id": "crevasse22", "world": "main", "world_px": [0, 0], "size": [20, 5], "depth": 0 },
  { "slug": "shaft", "name": "Shaft", "id": "shaft22222", "world": "main", "world_px": [320, 80], "size": [6, 8], "depth": 1 } ] } }
```

**`level.get { level, layers?, format?, notes? }`**. `layers` limits which `int_grid`, `tiles` and `auto` layers are included (default all); entities are always included. `notes`: `"all"` (default), `"open"` or `"none"`; it filters the level's notes and the world notes over the level alike, and `"none"` leaves both sections out.

- `format: "text"` → `{ rev, text }`: the [text view](format.md#text-view), with bake results: a derived layer is drawn as its effective grid (header `derived by <writer>`), then its stored overrides when it has any. A derived layer whose writer can't bake is drawn from its overrides alone (header `(not baked: overrides only)`).
- `format: "json"` (default) → `{ rev, world, level, content, derived? }`: `level` as in `level.json`, `content` as in `create_level` (`int_grids` with `rows` and `legend`, `tiles`, `entities`, `notes`), all in file form. `content.int_grids` has no derived layers; `derived` holds each derived layer shown, `{ <layer>: { writer, rows, overrides? } }`: `writer` the auto layer whose rules write it (or `null`), `rows` its effective rows (glyphs of the layer's legend; `null`, with `error` saying why, when the writer can't bake), `overrides` the stored override rows (`.` none, a value glyph, `!` suppressed) when the level has any.

Reads of derived layers take their effective grids from the host's bake cache (the editor's, or headless mode's own), baking the level first when it isn't cached.

```json
{ "jsonrpc": "2.0", "id": 6, "method": "level.get", "params": { "level": "shaft", "layers": ["collision"], "format": "text" } }
```

```json
{ "jsonrpc": "2.0", "id": 6, "result": { "rev": 0, "text": "level shaft \"Shaft\" world main id shaft22222\nsize 6x8 cells of 16 px, world_px [320, 80], depth 1\nrule state main_tiling: seed 4 optional_groups [pillars] tileset castle_main -> ssw_set5\n\nint_grid collision (6x8 cells of 16 px)\nlegend: . empty, # solid, f fortified, o oneway, ~ water, L ramp_l\n  012345\n0 ......\n1 ......\n2 ......\n3 ......\n4 ......\n5 ......\n6 ......\n7 ......\n\nentities: 1\n  doorshaft2 locked_door in objects at [2, 7]\n\nnotes: 0\n\nworld notes over this level: 1 (world pixels)\n  regioneast region open rect [0, 0, 640, 480] title \"Forest\"\nguides over this level: 1 (list_notes kind guide)\n" } }
```

```json
{ "jsonrpc": "2.0", "id": 7, "method": "level.get", "params": { "level": "shaft", "layers": [] } }
```

```json
{ "jsonrpc": "2.0", "id": 7, "result": { "rev": 0, "world": "main",
  "level": { "id": "shaft22222", "slug": "shaft", "name": "Shaft", "world_px": [320, 80], "size": [6, 8], "depth": 1,
    "layers": { "main_tiling": { "seed": 4, "optional_groups": ["pillars"], "tilesets": { "castle_main": "ssw_set5" } } } },
  "content": { "entities": [ { "layer": "objects", "entities": [ { "id": "doorshaft2", "type": "locked_door", "cell": [2, 7] } ] } ] } } }
```

| Write | Params | Command |
| --- | --- | --- |
| `level.create` | `{ world?, level, content? }`; `level.id` optional; `level.tags` as in `level.json` | `create_level` |
| `level.delete` | `{ level, confirm: true }` | `delete_level` |
| `level.move` | `{ level, world_px }` | `move_level` |
| `level.resize` | `{ level, size, anchor? }` | `resize_level` |
| `level.crop` | `{ level }` | crop to content: the `batch` of `resize_level`s that shrinks the level to the bounding box of its cells, tiles and entities ([commands.md](commands.md#resize_level)), labelled `Crop level <slug>`, one undo entry. A level that already fits applies nothing (same `rev`); one with no content is `command_error` `invalid` |
| `level.update` | `{ level, slug?, name?, depth?, bg_color?, tags? }` | `batch` of `rename_level`, `set_level_depth`, `set_level_bg`, `set_level_tags` (only what's given; `tags` replaces the level's tags, `[]` clears them) |
| `level.set_fields` | `{ level, fields?: { slug: value }, unset?: [slug] }` | `batch` of `set_level_field` (`fields` stores, `unset` removes the stored value) |
| `level.set_layer_state` | `{ level, layer, state }`; `state` is an `auto` layer's per-level rule state as in `level.json` `layers` (`{ seed?, optional_groups?, tilesets? }`), or `null` to remove it | `set_layer_state`, reporting `layer_state` (that layer rebakes in the level). `state` replaces the entry whole. Checked first: `layer` must be an `auto` layer (`wrong_layer_kind`), each of `optional_groups` an `optional` group of its rules (`unknown_rule_group`, or `invalid` for an `always` or `off` group), each tileset of `tilesets` (keys and values) defined (`unknown_tileset`) |
| `level.replace` | `{ world?, level, content?, confirm? }`, `level` and `content` as for `level.create`; `level.slug` names the level to replace, and a left-out `level.id` keeps its id | `replace_level`: `level.json` and all content, wholesale. Content left out of `content` is removed, so it needs `confirm` when the level holds anything `content` leaves out ([Confirmation](#confirmation)); rewriting the cells of a layer `content` includes never does. Reports `level_meta` part `all`, then a `grid` or `tiles` per layer with content before or after, an `entity_layer`, `entity` or `note` per entities layer entry, entity and note before or after ([commands.md](commands.md#replace_level)). It can't rename: use `level.update` |
| `level.duplicate` | `{ level, slug, name?, world?, world_px? }` | `duplicate_level`: a copy of the level under `slug` with new ids and its internal references remapped ([commands.md](commands.md#duplicate_level)). `world` is where the copy goes (default: the level's world), `world_px` where (default: just right of the original, past any level there). The result adds the copy's level `id` and `world_px` |

### grid

`int_grid` layers. Cells are `[x, y]` in the layer's own grid, origin top-left; values are value slugs, `null` is empty.

**Derived layers** ([format.md](format.md#derived-layers-overrides)) hold what rules compute; what you paint there are **overrides**. Every grid write works on a derived layer's override plane: a value slug sets an override, `null` removes it (the cell goes back to the rules) and `"!"` suppresses the cell (empty, whatever the rules say). `flood_fill` floods cells with the same stored state, and `replace_value`'s `from`/`to` may be `"!"`. `"!"` on any other layer is refused (`command_error` `invalid`: "\"!\" (suppress) only applies to derived layers; collision isn't derived").

**`grid.get_cells { level, layer, rect?, stored? }`** → `{ rev, rect, legend, rows }`: the cells of `rect` (default the whole layer) as rows of glyphs, with the layer's legend (glyph → value slug, `"."` → `null`). On a derived layer the rows are its **effective** values (overrides over what the rules computed); with `stored: true` they are the override plane instead (`.` no override, a value glyph, `!` suppressed), and the legend is the layer's value glyphs plus `"!": "!"`, without `"."` (built in, "no override", as in the layer's file). `stored` is ignored on other layers.

```json
{ "jsonrpc": "2.0", "id": 8, "method": "grid.get_cells", "params": { "level": "crevasse", "layer": "collision", "rect": [0, 2, 12, 2] } }
```

```json
{ "jsonrpc": "2.0", "id": 8, "result": { "rev": 0, "rect": [0, 2, 12, 2],
  "legend": { ".": null, "#": "solid", "f": "fortified", "o": "oneway", "~": "water", "L": "ramp_l" },
  "rows": ["#.......oooo", "#L..~~~....."] } }
```

**`grid.set_cells`** → `paint_cells`. Either `{ level, layer, cells }` with `[x, y, value]` triples, or `{ level, layer, text, origin, legend? }`, a block of glyph rows; both take `keep_empty?` (as `paint_cells`):

- `text`: rows separated by `\n` (a trailing newline adds no row). The first character of the first row is cell `origin`; each character is the next cell to the right, each row the next one down.
- Glyphs are the layer's value glyphs and `.` for empty (on a derived layer: no override). `!` suppresses a derived layer's cell (refused on other layers, as above). A space or `*` leaves its cell untouched, and a short row leaves the rest of its line untouched, so a block can paint an irregular shape.
- `legend` (optional): `{ "<glyph>": "<value slug>" | null | "!" }`, added to (and overriding) the layer's glyphs for this request. A glyph neither defines is `invalid params`.
- A cell outside the layer fails the whole request (`out_of_bounds`).

Giving both forms, or neither, is `invalid params`.

```json
{ "jsonrpc": "2.0", "id": 10, "method": "grid.set_cells", "params": { "level": "crevasse", "layer": "collision",
  "cells": [[3, 1, "solid"], [4, 1, "solid"], [8, 2, null]], "expect_rev": 0 } }
```

```json
{ "jsonrpc": "2.0", "id": 10, "result": { "rev": 1, "status": "applied", "label": "claude-code: Paint 3 cells on collision",
  "changes": [ { "change": "grid", "world": "main", "level": "crevasse", "layer": "collision", "rect": [3, 1, 6, 2] } ] } }
```

```json
{ "jsonrpc": "2.0", "id": 11, "method": "grid.set_cells", "params": { "level": "crevasse", "layer": "collision",
  "text": "~~~\n#*#", "origin": [13, 1], "label": "Agent: pool by the east wall" } }
```

```json
{ "jsonrpc": "2.0", "id": 11, "result": { "rev": 2, "status": "applied", "label": "Agent: pool by the east wall",
  "changes": [ { "change": "grid", "world": "main", "level": "crevasse", "layer": "collision", "rect": [13, 1, 3, 2] } ] } }
```

The other grid writes:

| Write | Params | Command |
| --- | --- | --- |
| `grid.fill_rect` | `{ level, layer, rect, value }` (`value` a slug, `null` or `"!"`, as for every grid write) | `fill_rect` |
| `grid.flood_fill` | `{ level, layer, at, value }` | `flood_fill` |
| `grid.line` | `{ level, layer, from: [x, y], to: [x, y], value }` | `paint_cells` with the cells of the editor's line tool (8-connected Bresenham, both ends included) |
| `grid.replace_value` | `{ level, layer, from, to }` | `replace_value` |

```json
{ "jsonrpc": "2.0", "id": 12, "method": "grid.fill_rect", "params": { "level": "crevasse", "layer": "collision", "rect": [2, 2, 3, 1], "value": "water" } }
```

```json
{ "jsonrpc": "2.0", "id": 12, "result": { "rev": 3, "status": "applied", "label": "claude-code: Fill rectangle on collision",
  "changes": [ { "change": "grid", "world": "main", "level": "crevasse", "layer": "collision", "rect": [2, 2, 3, 1] } ] } }
```

```json
{ "jsonrpc": "2.0", "id": 13, "method": "grid.flood_fill", "params": { "level": "crevasse", "layer": "collision", "at": [9, 2], "value": "fortified" } }
```

```json
{ "jsonrpc": "2.0", "id": 13, "result": { "rev": 4, "status": "applied", "label": "claude-code: Flood fill on collision",
  "changes": [ { "change": "grid", "world": "main", "level": "crevasse", "layer": "collision", "rect": [9, 2, 3, 1] } ] } }
```

```json
{ "jsonrpc": "2.0", "id": 14, "method": "grid.line", "params": { "level": "crevasse", "layer": "collision", "from": [5, 1], "to": [7, 1], "value": "solid" } }
```

```json
{ "jsonrpc": "2.0", "id": 14, "result": { "rev": 5, "status": "applied", "label": "claude-code: Paint 3 cells on collision",
  "changes": [ { "change": "grid", "world": "main", "level": "crevasse", "layer": "collision", "rect": [5, 1, 3, 1] } ] } }
```

```json
{ "jsonrpc": "2.0", "id": 15, "method": "grid.replace_value", "params": { "level": "crevasse", "layer": "collision", "from": "fortified", "to": "solid" } }
```

```json
{ "jsonrpc": "2.0", "id": 15, "result": { "rev": 6, "status": "applied", "label": "claude-code: Replace fortified with solid on collision",
  "changes": [ { "change": "grid", "world": "main", "level": "crevasse", "layer": "collision", "rect": [9, 2, 7, 2] } ] } }
```

A stale `expect_rev` and a refused command change nothing:

```json
{ "jsonrpc": "2.0", "id": 16, "method": "grid.fill_rect", "params": { "level": "crevasse", "layer": "collision", "rect": [1, 1, 2, 1], "value": "solid", "expect_rev": 2 } }
```

```json
{ "jsonrpc": "2.0", "id": 16, "error": { "code": -32001, "message": "the project is at rev 6, not 2: re-read before writing", "data": { "expected": 2, "current": 6 } } }
```

```json
{ "jsonrpc": "2.0", "id": 9, "method": "grid.set_cells", "params": { "level": "crevasse", "layer": "collision", "cells": [[1, 1, "lava"]] } }
```

```json
{ "jsonrpc": "2.0", "id": 9, "error": { "code": -32004, "message": "\"lava\" is not a value of layer \"collision\"",
  "data": { "kind": "unknown_value", "layer": "collision", "value": "lava", "message": "\"lava\" is not a value of layer \"collision\"" } } }
```

### tiles

`tiles` layers; placements as in the layer files: `{ cell, tileset, tile: [col, row], flip?, alpha? }`, `tile` being the atlas column and row.

**`tiles.get { level, layer, rect? }`** → `{ rev, tiles }`, the placements whose cell is in `rect` (default all), in file order. `tiles.find` searches every level by tileset and atlas tile ([find](#find-entities-levels-notes-and-tiles)).

```json
{ "jsonrpc": "2.0", "id": 17, "method": "tiles.get", "params": { "level": "crevasse", "layer": "decor_back", "rect": [0, 0, 10, 4] } }
```

```json
{ "jsonrpc": "2.0", "id": 17, "result": { "rev": 6, "tiles": [
  { "cell": [4, 1], "tileset": "ssw_set5", "tile": [0, 3], "alpha": 0.5 },
  { "cell": [4, 2], "tileset": "castle_main", "tile": [5, 1] },
  { "cell": [5, 2], "tileset": "castle_main", "tile": [6, 1], "flip": "x" } ] } }
```

| Write | Params | Command |
| --- | --- | --- |
| `tiles.set` | `{ level, layer, tiles, clear? }` | `set_tiles` (each named cell's stack is replaced; several placements on one cell stack in order) |
| `tiles.clear_rect` | `{ level, layer, rect }` | `clear_tiles` |
| `tiles.stamp` | `{ level, layer, at: [x, y], tileset, rect: [col, row, w, h] }` | `set_tiles` laying the atlas rect down whole: atlas tile `[col + i, row + j]` goes on cell `at + [i * sx, j * sy]`, where `s = max(1, tile size / layer grid size)` per axis, as the editor's stamp brush does |

```json
{ "jsonrpc": "2.0", "id": 18, "method": "tiles.set", "params": { "level": "crevasse", "layer": "decor_back",
  "tiles": [ { "cell": [10, 6], "tileset": "castle_main", "tile": [1, 1] } ], "clear": [[5, 2]] } }
```

```json
{ "jsonrpc": "2.0", "id": 18, "result": { "rev": 7, "status": "applied", "label": "claude-code: Place 1 tile on decor_back",
  "changes": [ { "change": "tiles", "world": "main", "level": "crevasse", "layer": "decor_back", "rect": [5, 2, 6, 5] } ] } }
```

```json
{ "jsonrpc": "2.0", "id": 19, "method": "tiles.clear_rect", "params": { "level": "crevasse", "layer": "decor_back", "rect": [4, 1, 1, 1] } }
```

```json
{ "jsonrpc": "2.0", "id": 19, "result": { "rev": 8, "status": "applied", "label": "claude-code: Clear tiles on decor_back",
  "changes": [ { "change": "tiles", "world": "main", "level": "crevasse", "layer": "decor_back", "rect": [4, 1, 1, 1] } ] } }
```

```json
{ "jsonrpc": "2.0", "id": 20, "method": "tiles.stamp", "params": { "level": "crevasse", "layer": "decor_back", "at": [20, 2], "tileset": "castle_main", "rect": [0, 0, 2, 2] } }
```

```json
{ "jsonrpc": "2.0", "id": 20, "result": { "rev": 9, "status": "applied", "label": "claude-code: Place 4 tiles on decor_back",
  "changes": [ { "change": "tiles", "world": "main", "level": "crevasse", "layer": "decor_back", "rect": [20, 2, 3, 3] } ] } }
```

### level writes

```json
{ "jsonrpc": "2.0", "id": 21, "method": "level.create", "params": {
  "level": { "id": "annexlvl22", "slug": "annex", "name": "Annex", "world_px": [320, 0], "size": [6, 3], "depth": 0 },
  "content": { "int_grids": [ { "layer": "collision", "rows": ["######", "#....#", "######"] } ] } } }
```

```json
{ "jsonrpc": "2.0", "id": 21, "result": { "rev": 10, "status": "applied", "label": "claude-code: Create level annex", "id": "annexlvl22",
  "changes": [ { "change": "level_created", "world": "main", "level": "annex" } ] } }
```

```json
{ "jsonrpc": "2.0", "id": 22, "method": "level.move", "params": { "level": "annex", "world_px": [480, 0] } }
```

```json
{ "jsonrpc": "2.0", "id": 22, "result": { "rev": 11, "status": "applied", "label": "claude-code: Move level annex",
  "changes": [ { "change": "level_meta", "world": "main", "level": "annex", "part": "position" } ] } }
```

```json
{ "jsonrpc": "2.0", "id": 23, "method": "level.resize", "params": { "level": "annex", "size": [8, 4], "anchor": [1, -1] } }
```

```json
{ "jsonrpc": "2.0", "id": 23, "result": { "rev": 12, "status": "applied", "label": "claude-code: Resize level annex",
  "changes": [
    { "change": "level_meta", "world": "main", "level": "annex", "part": "size" },
    { "change": "level_meta", "world": "main", "level": "annex", "part": "position" },
    { "change": "grid", "world": "main", "level": "annex", "layer": "collision", "rect": [0, 0, 8, 4] } ] } }
```

```json
{ "jsonrpc": "2.0", "id": 24, "method": "level.update", "params": { "level": "annex", "slug": "wing", "name": "West wing", "depth": 2 } }
```

```json
{ "jsonrpc": "2.0", "id": 24, "result": { "rev": 13, "status": "applied", "label": "claude-code: Rename level annex to wing",
  "changes": [
    { "change": "level_renamed", "world": "main", "from": "annex", "to": "wing" },
    { "change": "level_meta", "world": "main", "level": "wing", "part": "name" },
    { "change": "level_meta", "world": "main", "level": "wing", "part": "depth" } ] } }
```

```json
{ "jsonrpc": "2.0", "id": 25, "method": "level.set_fields", "params": { "level": "crevasse", "fields": { "music": "cave_theme" } } }
```

```json
{ "jsonrpc": "2.0", "id": 25, "result": { "rev": 14, "status": "applied", "label": "claude-code: Set music on crevasse",
  "changes": [ { "change": "level_meta", "world": "main", "level": "crevasse", "part": "fields" } ] } }
```

Tags say what a level is part of (a region of the world, say); they go into the export and `project.validate` checks them against the world's tagged regions ([format.md](format.md)):

```json
{ "jsonrpc": "2.0", "id": 90, "method": "level.update", "params": { "level": "crevasse", "tags": ["forest"] } }
```

```json
{ "jsonrpc": "2.0", "id": 90, "result": { "rev": 15, "status": "applied", "label": "claude-code: Tag level crevasse",
  "changes": [ { "change": "level_meta", "world": "main", "level": "crevasse", "part": "tags" } ] } }
```

### entity

An entity on the wire: `{ id, type, level, world, layer, cell, offset_px, size, fields }`, with `cell` in the entities layer's grid, `size` `null` when it's the type's, and `fields` the stored values.

| Method | Params | Result / command |
| --- | --- | --- |
| `entity.list` | `{ level?, layer?, type? }` | `{ rev, entities }` in file order |
| `entity.get` | `{ id }` | `{ rev, entity, index, effective_fields }`: `effective_fields` holds every field of the type, defaults applied |
| `entity.find` | `{ query?, type?, tag?, fields?, world?, level?, layer?, text?, rect? }` | `{ rev, entities }`. All filters must hold: `query` (the [find query language](#find-entities-levels-notes-and-tiles)), `type` (slug, `*` a wildcard), `tag` (one of the type's tags), `fields` (`{ slug: value }`, compared as JSON to the effective value, numbers by value), `world`, `level`, `layer`, `text` (in the id, type slug or type name), `rect` (`[x, y, w, h]` in the entities layer's cells; needs `level`) |
| `entity.add` | `{ level, layer?, entity, index? }`; `entity.id` optional; `layer` optional when the project has one entities layer | `add_entity`; result has `id` |
| `entity.update` | `{ id, fields?, unset?, size? }` (`size: null` resets it) | `batch` of `set_entity_field` and `resize_entity` |
| `entity.move` | `{ id, cell, offset_px? }` | `move_entity` |
| `entity.remove` | `{ ids: [id] }` | `batch` of `remove_entity` |

```json
{ "jsonrpc": "2.0", "id": 26, "method": "entity.add", "params": { "level": "wing", "layer": "objects",
  "entity": { "id": "wingdoor22", "type": "locked_door", "cell": [3, 2], "fields": { "channel": 2 } } } }
```

```json
{ "jsonrpc": "2.0", "id": 26, "result": { "rev": 16, "status": "applied", "label": "claude-code: Place locked_door", "id": "wingdoor22",
  "changes": [ { "change": "entity", "world": "main", "level": "wing", "id": "wingdoor22" } ] } }
```

```json
{ "jsonrpc": "2.0", "id": 27, "method": "entity.get", "params": { "id": "wingdoor22" } }
```

```json
{ "jsonrpc": "2.0", "id": 27, "result": { "rev": 16, "index": 0,
  "entity": { "id": "wingdoor22", "type": "locked_door", "level": "wing", "world": "main", "layer": "objects",
    "cell": [3, 2], "offset_px": [0, 0], "size": null, "fields": { "channel": 2 } },
  "effective_fields": { "channel": 2, "needs_item": "", "consumes": false, "facing": "left", "style": "wood", "tint": "#ffffff",
    "speed": 1.5, "label": null, "anchor": null, "waypoints": [], "exit_to": null, "sprite": null, "script": null, "extra": { "a": 1 } } } }
```

```json
{ "jsonrpc": "2.0", "id": 28, "method": "entity.update", "params": { "id": "wingdoor22", "fields": { "needs_item": "brass_key" }, "size": [1, 3] } }
```

```json
{ "jsonrpc": "2.0", "id": 28, "result": { "rev": 17, "status": "applied", "label": "claude-code: Set needs_item on wingdoor22",
  "changes": [ { "change": "entity", "world": "main", "level": "wing", "id": "wingdoor22" } ] } }
```

```json
{ "jsonrpc": "2.0", "id": 29, "method": "entity.move", "params": { "id": "wingdoor22", "cell": [4, 2], "offset_px": [0, -2] } }
```

```json
{ "jsonrpc": "2.0", "id": 29, "result": { "rev": 18, "status": "applied", "label": "claude-code: Move entity wingdoor22",
  "changes": [ { "change": "entity", "world": "main", "level": "wing", "id": "wingdoor22" } ] } }
```

```json
{ "jsonrpc": "2.0", "id": 30, "method": "entity.find", "params": { "tag": "puzzle", "level": "crevasse", "rect": [10, 0, 10, 5] } }
```

```json
{ "jsonrpc": "2.0", "id": 30, "result": { "rev": 18, "entities": [
  { "id": "doorcrev22", "type": "locked_door", "level": "crevasse", "world": "main", "layer": "objects", "cell": [18, 3], "offset_px": [0, 0], "size": [1, 2],
    "fields": { "exit_to": "doorshaft2", "facing": "right", "needs_item": "brass_key", "script": "scripts/door.gd", "speed": 2,
      "sprite": { "tile": [4, 4], "tileset": "castle_main" }, "tint": "#ff8080", "waypoints": [[1, 1], [2, 1]] } },
  { "id": "btncrev222", "type": "floor_button", "level": "crevasse", "world": "main", "layer": "objects", "cell": [12, 3], "offset_px": [0, -2], "size": null,
    "fields": { "channel": 3, "latching": true, "target": "doorcrev22" } } ] } }
```

```json
{ "jsonrpc": "2.0", "id": 31, "method": "entity.remove", "params": { "ids": ["btncrev222"] } }
```

```json
{ "jsonrpc": "2.0", "id": 31, "result": { "rev": 19, "status": "applied", "label": "claude-code: Remove entity btncrev222",
  "changes": [ { "change": "entity", "world": "main", "level": "crevasse", "id": "btncrev222" } ] } }
```

### notes

A note on the wire: `{ scope, note }`, `scope` being `{ "world": slug }` or `{ "level": slug }` and `note` the annotation in file form (pixels: world pixels at world scope, level pixels at level scope). Annotations are notes, pins, paths, regions, guides and connectors; guides are world-scope reference lines, grouped by an optional `group` naming one of the world's `guide_groups`, and a connector joins two levels or entities (`from`, `to`), its ends computed from where they are ([format.md](format.md#connectors)).

**Locked annotations.** The commands ignore `locked`; this API enforces it. Moving a locked annotation (changing its geometry: `px`, `points`, `rect`, `closed`, a guide's `group`, or a connector's `from`, `to` or `waypoints`) with `notes.update` or `notes.move`, or removing one with `notes.remove`, needs `confirm: true` ([Confirmation](#confirmation)); without it the error is `needs_confirm` with the reason "note guidestem2 is locked; moving it needs confirm: true". Changing its text, tags, color, links, status, export flag, a pin's icon or label, a connector's label or style, or `locked` itself never does.

| Method | Params | Result / command |
| --- | --- | --- |
| `notes.list` | `{ scope?, tag?, status?, kind?, kinds?, group?, export?, text?, query? }`; `status`: `"open"`, `"done"` or `"all"` (default); `kind` one kind, `kinds` any of several (`["note", "pin", "path", "region", "connector"]` leaves guides out); `group`: only the guides of that guide group; `export`: only annotations with that `export` flag; `text`: in the id, title, body or label (case-insensitive); `query`: the [find query language](#find-entities-levels-notes-and-tiles). Every kind is listed by default, guides included | `{ rev, notes }`, world notes first, then levels in `level.list` order |
| `notes.add` | `{ scope, note, index? }`; `note.id` optional; a guide may name a `group` | `add_note`; result has `id` |
| `notes.update` | `{ id, note }` (wholesale) or `{ id, patch }` (top-level keys merged in, geometry keys included; `null` removes an optional key; `kind` can't change), plus `confirm?` for a locked one | `update_note` |
| `notes.move` | `{ ids: [id], delta: [dx, dy], confirm? }`: every point of each annotation moves by `delta` pixels | `batch` of `update_note`, labelled "Move note \<id\>" or "Move \<n\> notes" |
| `notes.resolve` | `{ id, status? }`, `status` default `"done"` | `update_note` with the status |
| `notes.remove` | `{ ids: [id], confirm? }` | `batch` of `remove_note` |
| `notes.set_guide_group` | `{ world?, group, index? }`, `group` as in `world.json` `guide_groups` | `set_guide_group`: replaces the group with `group.slug` (moved to `index` when given) or adds it (at `index`, default the end) |
| `notes.remove_guide_group` | `{ world?, slug }` | `remove_guide_group`; refused (`invalid`) while a guide names the group |

`world` defaults to the only world (with several, it is required). Guide groups and world-scope annotations live in `worlds/<world>/world.json`, so their changes (`guide_group`, and `note` with `level: null`) name that file in `changed.files`.

```json
{ "jsonrpc": "2.0", "id": 32, "method": "notes.add", "params": { "scope": { "level": "wing" },
  "note": { "id": "wingnote22", "kind": "note", "title": "Needs a key room", "px": [40, 24], "tags": ["todo"] } } }
```

```json
{ "jsonrpc": "2.0", "id": 32, "result": { "rev": 20, "status": "applied", "label": "claude-code: Add note", "id": "wingnote22",
  "changes": [ { "change": "note", "world": "main", "level": "wing", "id": "wingnote22" } ] } }
```

```json
{ "jsonrpc": "2.0", "id": 33, "method": "notes.list", "params": { "tag": "todo", "status": "open" } }
```

```json
{ "jsonrpc": "2.0", "id": 33, "result": { "rev": 20, "notes": [
  { "scope": { "level": "crevasse" }, "note": { "id": "notecrev22", "kind": "note", "title": "Widen the gap", "body": "Jump is **too easy**.\nTry 5 cells.",
    "px": [160, 48], "tags": ["todo"], "links": ["entity:btncrev222", "note:pinstatue2", "https://example.com"] } },
  { "scope": { "level": "wing" }, "note": { "id": "wingnote22", "kind": "note", "title": "Needs a key room", "px": [40, 24], "tags": ["todo"] } } ] } }
```

```json
{ "jsonrpc": "2.0", "id": 34, "method": "notes.update", "params": { "id": "wingnote22", "patch": { "body": "Put the brass key two rooms back." } } }
```

```json
{ "jsonrpc": "2.0", "id": 34, "result": { "rev": 21, "status": "applied", "label": "claude-code: Edit note wingnote22",
  "changes": [ { "change": "note", "world": "main", "level": "wing", "id": "wingnote22" } ] } }
```

```json
{ "jsonrpc": "2.0", "id": 35, "method": "notes.resolve", "params": { "id": "wingnote22" } }
```

```json
{ "jsonrpc": "2.0", "id": 35, "result": { "rev": 22, "status": "applied", "label": "claude-code: Edit note wingnote22",
  "changes": [ { "change": "note", "world": "main", "level": "wing", "id": "wingnote22" } ] } }
```

```json
{ "jsonrpc": "2.0", "id": 36, "method": "notes.remove", "params": { "ids": ["pathcrev22"] } }
```

```json
{ "jsonrpc": "2.0", "id": 36, "result": { "rev": 23, "status": "applied", "label": "claude-code: Remove note pathcrev22",
  "changes": [ { "change": "note", "world": "main", "level": "crevasse", "id": "pathcrev22" } ] } }
```

Guides are listed with every other kind unless a filter leaves them out:

```json
{ "jsonrpc": "2.0", "id": 91, "method": "notes.list", "params": { "kind": "guide" } }
```

```json
{ "jsonrpc": "2.0", "id": 91, "result": { "rev": 23, "notes": [
  { "scope": { "world": "main" }, "note": { "id": "guidestem2", "kind": "guide", "title": "Wall line", "tags": ["layout"], "locked": true,
    "points": [[0, 0], [0, 480], [320, 480]] } } ] } }
```

It is locked: a new title is fine, new points need `confirm`:

```json
{ "jsonrpc": "2.0", "id": 92, "method": "notes.update", "params": { "id": "guidestem2", "patch": { "points": [[0, 0], [0, 480]] } } }
```

```json
{ "jsonrpc": "2.0", "id": 92, "error": { "code": -32002, "message": "note guidestem2 is locked; moving it needs confirm: true",
  "data": { "reason": "note guidestem2 is locked; moving it needs confirm: true" } } }
```

```json
{ "jsonrpc": "2.0", "id": 93, "method": "notes.update", "params": { "id": "guidestem2", "patch": { "title": "West wall line" } } }
```

```json
{ "jsonrpc": "2.0", "id": 93, "result": { "rev": 24, "status": "applied", "label": "claude-code: Edit note guidestem2",
  "changes": [ { "change": "note", "world": "main", "level": null, "id": "guidestem2" } ] } }
```

```json
{ "jsonrpc": "2.0", "id": 94, "method": "notes.move", "params": { "ids": ["guidestem2"], "delta": [16, 0], "confirm": true } }
```

```json
{ "jsonrpc": "2.0", "id": 94, "result": { "rev": 25, "status": "applied", "label": "claude-code: Move note guidestem2",
  "changes": [ { "change": "note", "world": "main", "level": null, "id": "guidestem2" } ] } }
```

```json
{ "jsonrpc": "2.0", "id": 95, "method": "notes.move", "params": { "ids": ["pinstatue2", "regioneast"], "delta": [0, 32] } }
```

```json
{ "jsonrpc": "2.0", "id": 95, "result": { "rev": 26, "status": "applied", "label": "claude-code: Move 2 notes",
  "changes": [ { "change": "note", "world": "main", "level": null, "id": "pinstatue2" },
    { "change": "note", "world": "main", "level": null, "id": "regioneast" } ] } }
```

A guide group, then a guide in it, and the guides that name the group:

```json
{ "jsonrpc": "2.0", "id": 96, "method": "notes.set_guide_group", "params": { "group": { "slug": "shifted", "name": "Shifted", "offset": [0, 25] } } }
```

```json
{ "jsonrpc": "2.0", "id": 96, "result": { "rev": 27, "status": "applied", "label": "claude-code: Set guide group shifted",
  "changes": [ { "change": "guide_group", "world": "main", "slug": "shifted" } ] } }
```

```json
{ "jsonrpc": "2.0", "id": 97, "method": "notes.add", "params": { "scope": { "world": "main" },
  "note": { "id": "guidelay22", "kind": "guide", "title": "Wall line", "tags": ["layout"], "locked": true,
    "points": [[26, -40], [26, -16]], "group": "shifted" } } }
```

```json
{ "jsonrpc": "2.0", "id": 97, "result": { "rev": 28, "status": "applied", "label": "claude-code: Add guide", "id": "guidelay22",
  "changes": [ { "change": "note", "world": "main", "level": null, "id": "guidelay22" } ] } }
```

```json
{ "jsonrpc": "2.0", "id": 98, "method": "notes.list", "params": { "group": "shifted" } }
```

```json
{ "jsonrpc": "2.0", "id": 98, "result": { "rev": 28, "notes": [
  { "scope": { "world": "main" }, "note": { "id": "guidelay22", "kind": "guide", "title": "Wall line", "tags": ["layout"],
    "locked": true, "points": [[26, -40], [26, -16]], "group": "shifted" } } ] } }
```

A group its guides still name can't go, and a locked note isn't removed without `confirm`:

```json
{ "jsonrpc": "2.0", "id": 99, "method": "notes.remove_guide_group", "params": { "slug": "shifted" } }
```

```json
{ "jsonrpc": "2.0", "id": 99, "error": { "code": -32004,
  "message": "guide group shifted is used by 1 guide (guidelay22); move them out first",
  "data": { "kind": "invalid", "message": "guide group shifted is used by 1 guide (guidelay22); move them out first" } } }
```

```json
{ "jsonrpc": "2.0", "id": 100, "method": "notes.remove", "params": { "ids": ["guidestem2"] } }
```

```json
{ "jsonrpc": "2.0", "id": 100, "error": { "code": -32002, "message": "note guidestem2 is locked; removing it needs confirm: true",
  "data": { "reason": "note guidestem2 is locked; removing it needs confirm: true" } } }
```

A connector from a room to a door entity, through one waypoint (shown, not replayed):

```json norun
{ "jsonrpc": "2.0", "id": 101, "method": "notes.add", "params": { "scope": { "world": "main" },
  "note": { "id": "connshaft2", "kind": "connector", "title": "Drop", "label": "one-way drop",
    "from": { "level": "crevasse" }, "to": { "entity": "doorshaft2" }, "waypoints": [[360, 40]] } } }
```

```json norun
{ "jsonrpc": "2.0", "id": 101, "result": { "rev": 29, "status": "applied", "label": "claude-code: Add connector", "id": "connshaft2",
  "changes": [ { "change": "note", "world": "main", "level": null, "id": "connshaft2" } ] } }
```

### defs

Definitions by `kind` and `slug`, in file form without `$schema` and `format`. The editable kinds are `layer` (an entry of `layers.json`), `entity` (an `entities/<slug>.json`), `entity_field` (one field of an entity type, the type named by `entity`), `level_field` (an entry of `project.json` `level_fields`), `enum` (an entry of `enums.json`) and `tileset` (an entry of `tilesets.json`). `rules` (a `rules/<layer>.json`, slug the layer) and `macros` (`rules/macros.json`, slug `macros`) can be read here; they are edited with the [`rules.*`](#rules) methods, one group, rule, stamp or macro at a time. There is no generic definition command: the server dispatches on `kind` to the per-kind [definition commands](commands.md#definition-commands):

| `kind` | `defs.set` | `defs.remove` | `defs.rename` |
| --- | --- | --- | --- |
| `layer` | `set_layer_def { layer, def, index? }` | `remove_layer_def { layer }` | `rename_layer { from, to }` |
| `entity` | `set_entity_def { entity, def }` | `remove_entity_def { entity }` | `rename_entity_type { from, to }` |
| `enum` | `set_enum_def { enum, def, index? }` | `remove_enum_def { enum }` | `rename_enum { from, to }` |
| `entity_field` | `set_entity_field_def { entity, field, def, index? }` | `remove_entity_field_def { entity, field }` | `rename_entity_field { entity, from, to }` |
| `level_field` | `set_level_field_def { field, def, index? }` | `remove_level_field_def { field }` | `rename_level_field { from, to }` |
| `enum_value` | | | `rename_enum_value { enum, from, to }` |
| `tileset` | `set_tileset_def { tileset, def, index? }` | `remove_tileset_def { tileset }` | `rename_tileset { from, to }` |

The command's slug field (`layer`, `entity`, `enum`, `tileset`, and `field` for the field kinds) is `def.slug` for `defs.set` and `slug` for `defs.remove`. `entity` is required with `entity_field` and refused with every other kind (`invalid params`). A kind left out of a column is `invalid params`. The kinds `stamp` and `macros` are `invalid params` too, with a message naming the `rules.*` method that edits them (`rules.set_stamp`, `rules.remove_stamp`, `rules.set_macro`, `rules.remove_macro`, `rules.rename_macro`; stamps have no rename). Any other kind is `invalid params`.

Each command reports a `def` change `{ "change": "def", "kind", "slug" }` for every definition it adds, replaces, removes or rewrites; a rename also reports `def_renamed` `{ "change": "def_renamed", "kind", "from", "to" }`, `layer_file_renamed` for every level file a layer rename moves, and a change for every other file it rewrote ([commands.md](commands.md#definition-commands) lists them per command).

| Method | Params | Result / command |
| --- | --- | --- |
| `defs.get` | `{ kind?, entity?, slug? }` | `{ rev, defs: { layers, entities, enums, tilesets, rules, macros } }` (only `kind`'s entry when given; `kind` may be `rules` or `macros`); with `kind` and `slug`: `{ rev, kind, def }`. `macros` is not a list but the one file `rules/macros.json` (`{}` when the project has none), slug `macros`. The field kinds aren't in the full listing (entity fields are in their type, level fields in `project.json`): `{ kind: "entity_field", entity }` and `{ kind: "level_field" }` give `{ rev, defs: { fields } }` in file order, and with `slug` `{ rev, kind, entity?, def }` |
| `defs.set` | `{ kind, entity?, def, index?, confirm? }` | the `set_*` command of `kind`: replaces the definition with `def.slug`, or creates it (at `index`, default the end; `entity` has no order and takes no `index`). For a field, `index` on an existing one moves it. Never drops stored data; `confirm` per [Confirmation](#confirmation). `set_layer_def` refuses taking out an `int_grid` value a cell holds, changing the kind or grid of a layer that has content, and changing `derived` on a layer with content in any level ("marks has overrides in hall; clear them first") |
| `defs.rename` | `{ kind, from, to }`, `{ kind: "entity_field", entity, from, to }`, or `{ kind: "enum_value", enum, from, to }` | the `rename_*` command of `kind`, rewriting every reference (below). Never needs `confirm` |
| `defs.remove` | `{ kind, entity?, slug, confirm: true }` | the `remove_*` command of `kind` (see below) |

What `defs.remove` takes with it:

- `layer`: the layer's content in every level (grid or tiles file, its entities and their `entities.json` entry, rule state) and its rules file. Undo puts all of it back.
- `entity`: every instance of the type, then the type (one `batch`; undo puts the type and each instance back).
- `enum`: only the enum. Fields and level fields that use it, and their stored values, stay as they are for `project.validate` to report.
- `tileset`: refused while anything uses it, with `command_error` `in_use` naming the first few uses. Rename or remove those first.
- `entity_field`, `level_field`: the field and every value instances of the type (levels, for a level field) store for it (one `batch`; undo puts the field and each value back). A rule group that requires a removed level field stays, for `project.validate` to report.

What `defs.rename` rewrites (a rules file can't be renamed on its own: renaming the `auto` layer renames it):

| Kind | References rewritten |
| --- | --- |
| `layer` | level layer files (renamed), `entities.json` layer entries, `level.json` rule state keys, `collision_layer`, `auto` layers' `source`, the layer's own rules file (renamed), rule patterns' `grid` (`match` and `require`, in rules files and in `rules/macros.json` templates), region rules' `region.grid` and `write.grid` |
| `entity` | the entity file (renamed), every instance's `type`, `allowed_types` of entity types and level fields |
| `enum` | the `enum` of entity and level fields |
| `entity_field` | the key every instance of the type stores the field's value under. Refused (`invalid`) while an instance already stores a value under `to` |
| `level_field` | the key every level stores the value under, rule groups' `requires_level_field` field. Refused (`invalid`) while a level stores a value under `to` or a rule group already requires `to` |
| `enum_value` | stored entity and level field values (arrays per element), field defaults, rule groups' `requires_level_field` values |
| `tileset` | tile placements, `tiles` layers' `tileset`, rule outputs and stamps, template references' `tileset` and fixed template rules' `out.tileset`, rule state tileset substitutions, icons, `tile` fields' `tileset` and every `tile` value and default |

```json
{ "jsonrpc": "2.0", "id": 37, "method": "defs.get", "params": { "kind": "entity", "slug": "floor_button" } }
```

```json
{ "jsonrpc": "2.0", "id": 37, "result": { "rev": 28, "kind": "entity", "def": {
  "slug": "floor_button", "name": "Floor Button", "category": "Puzzle/Switches", "tags": ["object", "puzzle"], "size": [1, 1], "pivot": [0.5, 1.0], "color": "#c04040",
  "fields": [
    { "slug": "channel", "type": "int", "default": 0, "min": 0, "max": 15, "display": { "canvas": true } },
    { "slug": "latching", "type": "bool", "default": false },
    { "slug": "target", "type": "entity_ref", "allowed_types": ["locked_door"], "nullable": true, "display": { "arrow": true } } ] } } }
```

```json
{ "jsonrpc": "2.0", "id": 38, "method": "defs.set", "params": { "kind": "entity", "def": {
  "slug": "floor_button", "name": "Floor Button", "category": "Puzzle/Switches", "tags": ["object", "puzzle"], "size": [1, 1], "pivot": [0.5, 1.0], "color": "#c04040",
  "fields": [
    { "slug": "channel", "type": "int", "default": 0, "min": 0, "max": 15, "display": { "canvas": true } },
    { "slug": "latching", "type": "bool", "default": false },
    { "slug": "target", "type": "entity_ref", "allowed_types": ["locked_door"], "nullable": true, "display": { "arrow": true } },
    { "slug": "delay", "type": "float", "default": 0, "min": 0 } ] } } }
```

```json
{ "jsonrpc": "2.0", "id": 38, "result": { "rev": 29, "status": "applied", "label": "claude-code: Set entity type floor_button",
  "changes": [ { "change": "def", "kind": "entity", "slug": "floor_button" } ] } }
```

Taking `right` out of `facing` while `doorcrev22` stores it needs `confirm`:

```json
{ "jsonrpc": "2.0", "id": 39, "method": "defs.set", "params": { "kind": "enum", "def": {
  "slug": "facing", "name": "Facing", "values": [ { "slug": "left", "name": "Left", "icon": { "tileset": "castle_main", "tile": [0, 9] } } ] } } }
```

```json
{ "jsonrpc": "2.0", "id": 39, "error": { "code": -32002,
  "message": "1 entity stores a value this takes out of enum facing (doorcrev22 facing: right); the values stay stored and validation will report them. Send confirm: true to do it",
  "data": { "reason": "1 entity stores a value this takes out of enum facing (doorcrev22 facing: right)" } } }
```

```json
{ "jsonrpc": "2.0", "id": 40, "method": "defs.rename", "params": { "kind": "enum_value", "enum": "facing", "from": "right", "to": "east" } }
```

```json
{ "jsonrpc": "2.0", "id": 40, "result": { "rev": 30, "status": "applied", "label": "claude-code: Rename facing value right to east",
  "changes": [ { "change": "def", "kind": "enum", "slug": "facing" }, { "change": "entity", "world": "main", "level": "crevasse", "id": "doorcrev22" } ] } }
```

No level field or default holds `facing.right`, so that is the whole list; renaming `biome.flooded` instead would also report `level_meta` part `fields` for `crevasse` and a `def` of kind `rules` for `main_tiling` (a `requires_level_field`).

```json
{ "jsonrpc": "2.0", "id": 41, "method": "defs.remove", "params": { "kind": "tileset", "slug": "ssw_set5" } }
```

```json
{ "jsonrpc": "2.0", "id": 41, "error": { "code": -32002, "message": "removing tileset ssw_set5 needs confirm: true", "data": { "reason": "removes tileset ssw_set5" } } }
```

```json
{ "jsonrpc": "2.0", "id": 65, "method": "defs.remove", "params": { "kind": "tileset", "slug": "ssw_set5", "confirm": true } }
```

```json
{ "jsonrpc": "2.0", "id": 65, "error": { "code": -32004,
  "message": "tileset \"ssw_set5\" is still used by rule set main_tiling, the main_tiling rule state of level shaft; remove or rename those uses first",
  "data": { "kind": "in_use", "def_kind": "tileset", "slug": "ssw_set5", "uses": ["rule set main_tiling", "the main_tiling rule state of level shaft"],
    "message": "tileset \"ssw_set5\" is still used by rule set main_tiling, the main_tiling rule state of level shaft; remove or rename those uses first" } } }
```

Fields one at a time. A field of an entity type is named by `entity` and its slug:

```json
{ "jsonrpc": "2.0", "id": 82, "method": "defs.get", "params": { "kind": "entity_field", "entity": "locked_door", "slug": "needs_item" } }
```

```json
{ "jsonrpc": "2.0", "id": 82, "result": { "rev": 30, "kind": "entity_field", "entity": "locked_door",
  "def": { "slug": "needs_item", "type": "string", "default": "" } } }
```

Turning it into an enum while `doorcrev22` and `wingdoor22` store a key name needs `confirm` (the values stay stored, for validation to report the ones that don't fit):

```json
{ "jsonrpc": "2.0", "id": 83, "method": "defs.set", "params": { "kind": "entity_field", "entity": "locked_door",
  "def": { "slug": "needs_item", "type": "enum", "values": ["brass_key", "iron_key"], "nullable": true } } }
```

```json
{ "jsonrpc": "2.0", "id": 83, "error": { "code": -32002,
  "message": "2 entities store a value for field needs_item, whose type changes; the values stay stored and validation will report the ones that don't fit. Send confirm: true to do it",
  "data": { "reason": "2 entities store a value for field needs_item, whose type changes" } } }
```

A new field goes at `index` (default the end); `index` on an existing field moves it, and a move alone never needs `confirm`:

```json
{ "jsonrpc": "2.0", "id": 84, "method": "defs.set", "params": { "kind": "entity_field", "entity": "locked_door", "index": 2,
  "def": { "slug": "key_color", "type": "color", "default": "#ffd040", "display": { "canvas": true } } } }
```

```json
{ "jsonrpc": "2.0", "id": 84, "result": { "rev": 31, "status": "applied", "label": "claude-code: Set field key_color of locked_door",
  "changes": [ { "change": "def", "kind": "entity", "slug": "locked_door" } ] } }
```

A rename moves every stored value to the new key; a slug the type already has is `duplicate_field`:

```json
{ "jsonrpc": "2.0", "id": 85, "method": "defs.rename", "params": { "kind": "entity_field", "entity": "locked_door", "from": "needs_item", "to": "consumes" } }
```

```json
{ "jsonrpc": "2.0", "id": 85, "error": { "code": -32004, "message": "entity type \"locked_door\" already has a field \"consumes\"",
  "data": { "kind": "duplicate_field", "owner": "entity type \"locked_door\"", "field": "consumes", "message": "entity type \"locked_door\" already has a field \"consumes\"" } } }
```

```json
{ "jsonrpc": "2.0", "id": 86, "method": "defs.rename", "params": { "kind": "entity_field", "entity": "locked_door", "from": "needs_item", "to": "key_item" } }
```

```json
{ "jsonrpc": "2.0", "id": 86, "result": { "rev": 32, "status": "applied", "label": "claude-code: Rename field needs_item of locked_door to key_item",
  "changes": [ { "change": "def", "kind": "entity", "slug": "locked_door" },
    { "change": "entity", "world": "main", "level": "crevasse", "id": "doorcrev22" },
    { "change": "entity", "world": "main", "level": "wing", "id": "wingdoor22" } ] } }
```

Level fields are the same without `entity`:

```json
{ "jsonrpc": "2.0", "id": 87, "method": "defs.get", "params": { "kind": "level_field" } }
```

```json
{ "jsonrpc": "2.0", "id": 87, "result": { "rev": 32, "defs": { "fields": [
  { "slug": "biome", "type": "enum", "enum": "biome", "array": true, "default": [] },
  { "slug": "music", "type": "string", "nullable": true } ] } } }
```

```json
{ "jsonrpc": "2.0", "id": 88, "method": "defs.remove", "params": { "kind": "level_field", "slug": "music" } }
```

```json
{ "jsonrpc": "2.0", "id": 88, "error": { "code": -32002, "message": "removing level field music needs confirm: true",
  "data": { "reason": "removes level field music and the value 1 level stores; undo brings it back" } } }
```

```json
{ "jsonrpc": "2.0", "id": 89, "method": "defs.remove", "params": { "kind": "level_field", "slug": "music", "confirm": true } }
```

```json
{ "jsonrpc": "2.0", "id": 89, "result": { "rev": 33, "status": "applied", "label": "claude-code: Remove level field music",
  "changes": [ { "change": "level_meta", "world": "main", "level": "crevasse", "part": "fields" }, { "change": "project", "key": "level_fields" } ] } }
```

### rules

Rule sets (`rules/<layer>.json`) are edited a group, a rule (or template reference), or a stamp at a time, and `rules/macros.json` one named pattern, legend or template at a time; there is no command that replaces a whole file. The file formats are in [format.md](format.md#rulesltlayergtjson). Each write is one [command](commands.md#definition-commands) and reports a `def` change: kind `rules` (slug the layer) for groups, rules and stamps, kind `macros` (slug `macros`) for macros, plus a `rules` change per rules file a macro rename rewrote.

| Method | Params | Result / command |
| --- | --- | --- |
| `rules.list` | `{ layer? }` | `{ rev, layers: [{ layer, source, stamps: [slug], groups: [{ slug, name?, mode, pass, requires_level_tag?, rules: [{ slug, active, use?, kind?, write? }] }] }], macros: { patterns, legends, templates, builtin } }`. A group's `pass` is its [pass](format.md#passes) (0 by default). A group's `rules` are its entries as written: a template reference has `use` (its template), a [region rule](format.md#region-rules) `kind: "region"`, a rule that writes a derived grid `write` (that grid's slug). `macros` names what `rules/macros.json` defines, and `builtin` the built-in templates (`blob47`) |
| `rules.get_group` | `{ layer, group }` | `{ rev, layer, index, group }`, the group in file form |
| `rules.set_group` | `{ layer, group, index? }`, `group` in file form | `set_rule_group { layer, group: group.slug, def: group, index? }`: replaces the group with that slug (in place unless `index`) or inserts it (at `index`, default the end) |
| `rules.remove_group` | `{ layer, group }` (a slug) | `remove_rule_group` |
| `rules.set_rule` | `{ layer, group, rule, index? }`, `rule` an entry in file form: a rule, or a template reference `{ "use": ... }` | `set_rule { layer, group, rule: <its slug>, def: rule, index? }`: replaces the entry with that slug in `group` (in place, or moved to `index` when given) or inserts it (at `index`, default the end). Refused when another group holds that slug: use `rules.move_rule` |
| `rules.move_rule` | `{ layer, rule, group, index }` | `move_rule`: `index` counts `group`'s entries after the rule is taken out of its old place |
| `rules.remove_rule` | `{ layer, rule }` | `remove_rule`. `rule` is a rule's or a reference's slug; a rule a reference expands to (`<reference>__<rule>`) is refused (`invalid`): put it in the reference's `skip` |
| `rules.set_stamp` | `{ layer, stamp, def, index? }` | `set_stamp` |
| `rules.remove_stamp` | `{ layer, stamp, keep_uses? }` | `remove_stamp`: refused (`in_use`) while a rule places it, unless `keep_uses` |
| `rules.get_macros` | `{}` | `{ rev, macros, builtin }`: `rules/macros.json` in file form (`{}` when there is none), and the names of the built-in templates, which can be used but not edited (their rules: `rules.expand` of a layer that uses one, or the files under `crates/worlds-core/templates/`) |
| `rules.set_macro` | `{ kind, name, def }`, `kind` `pattern`, `legend` or `template` | `set_macro`: creates or replaces the entry |
| `rules.remove_macro` | `{ kind, name, keep_uses? }` | `remove_macro`: refused (`in_use`, listing the uses) while a rule, reference or template refers to it, unless `keep_uses` |
| `rules.rename_macro` | `{ kind, from, to }` | `rename_macro`, rewriting every reference to it |
| `rules.expand` | `{ layer, group? }` | `{ rev, layer, groups: [{ slug, rules }] }`: the rules exactly as the engine runs them, in evaluation order, each in file form with named patterns and legends resolved and every template reference replaced by its rules (slug `<reference>__<template rule>`, tiles moved by `origin`, the reference's tileset), each of those with `from`: the reference's slug. A rules file that can't be expanded (an unknown template or pattern) is `command_error` `invalid` naming it |
| `rules.preview` | `{ layer, level, rule?, group?, rect?, matches? }` | where rules fire in `level`, from a fresh bake that isn't kept: `{ rev, fires: [{ rule, group, from?, pass, grid, cells, count, matched?, matched_count?, occupied?, regions?, write? }] }`, one entry per rule as it runs (passes in order, expanded: a template reference's rules are listed one by one, with `from`). `rule` limits it to one rule, or to every rule of a reference when it names one; `group` to one group; `rect` (`[x, y, w, h]`) to anchors inside it (and regions whose bbox meets it). `pass` is the rule's group's pass. `grid` is the `int_grid` layer the rule reads (its `match.grid`, a region rule's `region.grid`, else the layer's `source`), read by effective value when it is derived, and `cells` are `[x, y]` in that grid's cells where it fired (drew or wrote). Without `matches`, only rules that fire are listed. With `matches: true`, each entry also has `matched`: the cells where the rule could fire but didn't (its `chance` or `perlin` said no, an earlier rule of its pass with `break` took the cell, or every placement there was skipped by `avoid_overlap`), `matched_count`, and `occupied`: the part of `matched` skipped because the footprint was occupied; a rule that matches somewhere but never fires is then listed too, with `count: 0`, and so is a region rule whose grid has a region. A [region rule](format.md#region-rules)'s entry has `regions`: `[{ id, bbox, area, selected, runs, candidates }]`, every region of its `region.cells` on its grid (`id` as in `rules.regions`, `selected` whether every bound holds, `runs` the `[x, y, len]` runs of its `place.along` edge before `margin`, `[]` for `across`, `candidates` the cells it would fire on). A rule with `write` has `write: { grid, value }`. Every list holds at most 1000 entries, then the entry has `"truncated": true`. A `rule` that is neither a rule, a reference nor an expanded rule of the layer is `unknown_rule` |
| `rules.regions` | `{ level, cells, grid?, along? }` | the regions of a cell test, as region rules see them: `{ rev, grid, regions: [{ id, bbox, width, height, area, edges: { floor, ceiling, wall_left, wall_right } }], picture }`. `cells` is a cell test (`"."`, `"+"`, `"solid"`, `"!solid"`, `"[.\|oneway]"`, `"![solid\|water]"`); `grid` the `int_grid` layer (default `project.collision_layer`; a derived one by its effective values). A region is a 4-connected component of passing cells; `id` is the row-major order of its first cell, `bbox` `[x, y, w, h]`, `area` its cell count. Each edge is a list of runs `[x, y, len]` from the first cell (along a row for `floor` and `ceiling`, down a column for walls); `along` (`floor`, `ceiling`, `wall_left`, `wall_right`) keeps only that kind. `picture` is the grid's rows with each region cell as its region's letter (`a` to `z` by id, then `+`) and every other cell `#`. A test naming a value or group the grid doesn't have is `command_error` `invalid` |

```json
{ "jsonrpc": "2.0", "id": 42, "method": "rules.get_group", "params": { "layer": "main_tiling", "group": "pillars" } }
```

```json
{ "jsonrpc": "2.0", "id": 42, "result": { "rev": 33, "layer": "main_tiling", "index": 2, "group": {
  "slug": "pillars", "mode": "optional", "icon": { "tileset": "ssw_set5", "tile": [12, 4] },
  "rules": [ { "slug": "pillar_on_floor", "match": { "rows": ["...", "...", "+++"] }, "out": { "stamp": "pillar", "avoid_overlap": true } } ] } } }
```

```json
{ "jsonrpc": "2.0", "id": 43, "method": "rules.set_group", "params": { "layer": "main_tiling", "index": 0, "group": {
  "slug": "accents", "rules": [ { "slug": "water_surface", "match": { "rows": [".", "~", "*"] }, "out": { "tileset": "castle_main", "tiles": [[7, 2]] } } ] } } }
```

```json
{ "jsonrpc": "2.0", "id": 43, "result": { "rev": 34, "status": "applied", "label": "claude-code: Set rule group accents on main_tiling",
  "changes": [ { "change": "def", "kind": "rules", "slug": "main_tiling" } ] } }
```

```json
{ "jsonrpc": "2.0", "id": 44, "method": "rules.preview", "params": { "layer": "main_tiling", "level": "crevasse", "rule": "water_surface" } }
```

```json
{ "jsonrpc": "2.0", "id": 44, "result": { "rev": 34, "fires": [ { "rule": "water_surface", "group": "accents", "pass": 0, "grid": "collision", "cells": [[2, 2], [5, 3], [6, 3]], "count": 3 } ] } }
```

A template in `rules/macros.json`: a wall's top edge and its face, tiles relative to where a reference puts it (`legend` names the fixture's legend `walls`, `#` any wall and `.` anything else):

```json
{ "jsonrpc": "2.0", "id": 66, "method": "rules.set_macro", "params": { "kind": "template", "name": "ledge", "def": {
  "name": "Ledge", "doc": "A wall's top edge and its face. origin = the edge tile.", "legend": "walls",
  "rules": [
    { "slug": "top", "match": { "rows": [".", "#", "*"] }, "out": { "tiles": [[0, 0]] }, "break": true },
    { "slug": "face", "match": { "rows": ["#"] }, "out": { "tiles": [[0, 1]] }, "chance": 0.5 } ] } } }
```

```json
{ "jsonrpc": "2.0", "id": 66, "result": { "rev": 35, "status": "applied", "label": "claude-code: Set template ledge",
  "changes": [ { "change": "def", "kind": "macros", "slug": "macros" } ] } }
```

A reference to it, second in the `walls` group, drawing from `castle_main` with the edge tile at atlas `[14, 0]`:

```json
{ "jsonrpc": "2.0", "id": 67, "method": "rules.set_rule", "params": { "layer": "main_tiling", "group": "walls", "index": 1,
  "rule": { "use": "ledge", "slug": "ledges", "tileset": "castle_main", "origin": [14, 0] } } }
```

```json
{ "jsonrpc": "2.0", "id": 67, "result": { "rev": 36, "status": "applied", "label": "claude-code: Set rule ledges on main_tiling",
  "changes": [ { "change": "def", "kind": "rules", "slug": "main_tiling" } ] } }
```

```json
{ "jsonrpc": "2.0", "id": 68, "method": "rules.list", "params": { "layer": "main_tiling" } }
```

```json
{ "jsonrpc": "2.0", "id": 68, "result": { "rev": 36,
  "layers": [ { "layer": "main_tiling", "source": "collision", "stamps": ["pillar"], "groups": [
    { "slug": "accents", "mode": "always", "pass": 0, "rules": [ { "slug": "water_surface", "active": true } ] },
    { "slug": "walls", "name": "Walls", "mode": "always", "pass": 0, "rules": [
      { "slug": "wall_top_edge", "active": true }, { "slug": "ledges", "active": true, "use": "ledge" },
      { "slug": "wall_fill", "active": true }, { "slug": "any_wall_group", "active": true } ] },
    { "slug": "flooded_decor", "mode": "optional", "pass": 0, "rules": [ { "slug": "drips", "active": false } ] },
    { "slug": "pillars", "mode": "optional", "pass": 0, "rules": [ { "slug": "pillar_on_floor", "active": true } ] },
    { "slug": "retired", "mode": "off", "pass": 0, "rules": [] } ] } ],
  "macros": { "patterns": ["edge_top"], "legends": ["walls"], "templates": ["ledge"], "builtin": ["blob47"] } } }
```

What runs:

```json
{ "jsonrpc": "2.0", "id": 69, "method": "rules.expand", "params": { "layer": "main_tiling", "group": "walls" } }
```

```json
{ "jsonrpc": "2.0", "id": 69, "result": { "rev": 36, "layer": "main_tiling", "groups": [ { "slug": "walls", "rules": [
  { "slug": "wall_top_edge", "match": { "grid": "collision", "legend": { "#": "solid", ".": "!solid" }, "rows": ["***", ".#.", "###"] },
    "symmetry": ["mirror_x"], "out": { "tileset": "castle_main", "tiles": [[15, 0], [16, 0], [17, 0], { "rect": [18, 0, 1, 1], "weight": 3 }] },
    "break": true, "salt": 1101 },
  { "slug": "ledges__top", "from": "ledges", "match": { "legend": { "#": "walls", ".": "!walls" }, "rows": [".", "#", "*"] },
    "out": { "tileset": "castle_main", "tiles": [[14, 0]] }, "break": true },
  { "slug": "ledges__face", "from": "ledges", "match": { "legend": { "#": "walls", ".": "!walls" }, "rows": ["#"] },
    "out": { "tileset": "castle_main", "tiles": [[14, 1]] }, "chance": 0.5 },
  { "slug": "wall_fill", "match": { "rows": ["+"] }, "out": { "tileset": "castle_main", "tiles": [[1, 1], { "stack": [[5, 0], [4, 0], [4, 0]] }] },
    "out_of_bounds": "solid" },
  { "slug": "any_wall_group", "match": { "legend": { "n": "!walls" }, "rows": ["n", "w", "*"] },
    "out": { "tileset": "castle_main", "tiles": [[2, 0, 2, 1]], "tile_mode": "stamp", "pivot": [0.0, 1.0] }, "out_of_bounds": ".", "salt": -7 } ] } ] } }
```

Where the reference's rules fire, and where they match but don't (`face` loses its coin toss on some cells, and `top` takes the top edge with `break`):

```json
{ "jsonrpc": "2.0", "id": 70, "method": "rules.preview", "params": { "layer": "main_tiling", "level": "crevasse", "rule": "ledges", "rect": [0, 2, 8, 3], "matches": true } }
```

```json
{ "jsonrpc": "2.0", "id": 70, "result": { "rev": 36, "fires": [
  { "rule": "ledges__top", "group": "walls", "from": "ledges", "pass": 0, "grid": "collision",
    "cells": [[1, 4], [2, 4], [3, 4], [4, 4], [5, 4], [6, 4], [7, 4]], "count": 7, "matched": [], "matched_count": 0, "occupied": [] },
  { "rule": "ledges__face", "group": "walls", "from": "ledges", "pass": 0, "grid": "collision", "cells": [[0, 2], [0, 4]], "count": 2,
    "matched": [[0, 3], [1, 4], [2, 4], [3, 4], [4, 4], [5, 4], [6, 4], [7, 4]], "matched_count": 8, "occupied": [] } ] } }
```

```json
{ "jsonrpc": "2.0", "id": 71, "method": "rules.move_rule", "params": { "layer": "main_tiling", "rule": "ledges", "group": "walls", "index": 0 } }
```

```json
{ "jsonrpc": "2.0", "id": 71, "result": { "rev": 37, "status": "applied", "label": "claude-code: Move rule ledges to walls on main_tiling",
  "changes": [ { "change": "def", "kind": "rules", "slug": "main_tiling" } ] } }
```

A template in use can't be removed, and an expanded rule isn't an entry of its own:

```json
{ "jsonrpc": "2.0", "id": 72, "method": "rules.remove_macro", "params": { "kind": "template", "name": "ledge" } }
```

```json
{ "jsonrpc": "2.0", "id": 72, "error": { "code": -32004,
  "message": "macro \"ledge\" is still used by reference ledges in rules/main_tiling.json; remove or rename those uses first",
  "data": { "kind": "in_use", "def_kind": "macros", "slug": "ledge", "uses": ["reference ledges in rules/main_tiling.json"],
    "message": "macro \"ledge\" is still used by reference ledges in rules/main_tiling.json; remove or rename those uses first" } } }
```

```json
{ "jsonrpc": "2.0", "id": 73, "method": "rules.remove_rule", "params": { "layer": "main_tiling", "rule": "ledges__top" } }
```

```json
{ "jsonrpc": "2.0", "id": 73, "error": { "code": -32004,
  "message": "rule \"ledges__top\" comes from template reference \"ledges\" (template \"ledge\") in rules/main_tiling.json; add \"top\" to that reference's \"skip\" (set_rule on \"ledges\") instead",
  "data": { "kind": "invalid", "message": "rule \"ledges__top\" comes from template reference \"ledges\" (template \"ledge\") in rules/main_tiling.json; add \"top\" to that reference's \"skip\" (set_rule on \"ledges\") instead" } } }
```

```json
{ "jsonrpc": "2.0", "id": 74, "method": "rules.remove_rule", "params": { "layer": "main_tiling", "rule": "ledges" } }
```

```json
{ "jsonrpc": "2.0", "id": 74, "result": { "rev": 38, "status": "applied", "label": "claude-code: Remove rule ledges from main_tiling",
  "changes": [ { "change": "def", "kind": "rules", "slug": "main_tiling" } ] } }
```

```json
{ "jsonrpc": "2.0", "id": 75, "method": "rules.remove_macro", "params": { "kind": "template", "name": "ledge" } }
```

```json
{ "jsonrpc": "2.0", "id": 75, "result": { "rev": 39, "status": "applied", "label": "claude-code: Remove template ledge",
  "changes": [ { "change": "def", "kind": "macros", "slug": "macros" } ] } }
```

Renaming a macro rewrites what uses it (here nothing does, so only `rules/macros.json` changes):

```json
{ "jsonrpc": "2.0", "id": 76, "method": "rules.rename_macro", "params": { "kind": "pattern", "from": "edge_top", "to": "top_edge" } }
```

```json
{ "jsonrpc": "2.0", "id": 76, "result": { "rev": 40, "status": "applied", "label": "claude-code: Rename pattern edge_top to top_edge",
  "changes": [ { "change": "def", "kind": "macros", "slug": "macros" } ] } }
```

```json
{ "jsonrpc": "2.0", "id": 77, "method": "rules.get_macros", "params": {} }
```

```json
{ "jsonrpc": "2.0", "id": 77, "result": { "rev": 40,
  "macros": { "patterns": { "top_edge": ["***", ".#.", "###"] }, "legends": { "walls": { "#": "walls", ".": "!walls" } } },
  "builtin": ["blob47"] } }
```

Stamps:

```json
{ "jsonrpc": "2.0", "id": 78, "method": "rules.set_stamp", "params": { "layer": "main_tiling", "stamp": "arch",
  "def": { "slug": "arch", "tileset": "castle_main", "rect": [4, 4, 3, 2], "pivot": [0.5, 1.0] } } }
```

```json
{ "jsonrpc": "2.0", "id": 78, "result": { "rev": 41, "status": "applied", "label": "claude-code: Set stamp arch on main_tiling",
  "changes": [ { "change": "def", "kind": "rules", "slug": "main_tiling" } ] } }
```

```json
{ "jsonrpc": "2.0", "id": 79, "method": "rules.remove_stamp", "params": { "layer": "main_tiling", "stamp": "pillar" } }
```

```json
{ "jsonrpc": "2.0", "id": 79, "error": { "code": -32004,
  "message": "stamp \"pillar\" of layer \"main_tiling\" is still used by rule pillar_on_floor; remove or change those uses first",
  "data": { "kind": "in_use", "def_kind": "stamp", "layer": "main_tiling", "slug": "pillar", "uses": ["rule pillar_on_floor"],
    "message": "stamp \"pillar\" of layer \"main_tiling\" is still used by rule pillar_on_floor; remove or change those uses first" } } }
```

```json
{ "jsonrpc": "2.0", "id": 80, "method": "rules.remove_stamp", "params": { "layer": "main_tiling", "stamp": "arch" } }
```

```json
{ "jsonrpc": "2.0", "id": 80, "result": { "rev": 42, "status": "applied", "label": "claude-code: Remove stamp arch from main_tiling",
  "changes": [ { "change": "def", "kind": "rules", "slug": "main_tiling" } ] } }
```

Stamps and macros aren't `defs.*` kinds:

```json
{ "jsonrpc": "2.0", "id": 81, "method": "defs.set", "params": { "kind": "macros", "def": {} } }
```

```json
{ "jsonrpc": "2.0", "id": 81, "error": { "code": -32602, "message": "defs.set: kind macros isn't edited with defs.*; use rules.set_macro { kind, name, def }",
  "data": { "message": "defs.set: kind macros isn't edited with defs.*; use rules.set_macro { kind, name, def }" } } }
```

### history

**`history.begin_batch { label }`** opens a batch on this connection: every write until `end_batch` applies at once as usual (the agent and the canvas see it, `rev` bumps per write), but the writes become **one undo entry** with `label`, origin the client, one `changed` event (at `end_batch`, with the final `rev` and the coalesced changes) and one toast. A batch belongs to its connection; other connections' writes wait until it ends ([Queue and ordering](#queue-and-ordering)).

How it is recorded: the editor's undo history has one gesture slot, and it belongs to the user (a brush stroke). A batch doesn't use it. The server keeps the batch's state itself: it applies each write with `Project::apply`, collects the inverses, and at `end_batch` records them as one entry with `History::record_with` ([commands.md](commands.md#undo-and-redo)), along with each write's command label and the files they changed, which the editor's History pane lists under the entry. So a user stroke during the batch never joins the agent's entry, and the agent's batch never swallows the user's stroke.

- **`history.end_batch {}`** → `{ rev, status, label, changes, entries }`. `entries` is how many undo entries the batch became, normally 1. Before the user's edit is recorded as an entry, and before any undo or redo step, the server records the batch's part so far as an entry of its own; the rest of the batch becomes another entry with the same label. So a batch the user edited or undid during ends with `entries: 2` (or more), and each entry undoes only what was done in it. A batch where nothing changed has `entries: 0`.
- A **dropped connection reverts the part of its open batch that isn't recorded yet**: the server applies those inverses (last first) without recording them and sends `changed` with `"kind": "cancel"`. Parts already recorded (because the user edited in between) stay in the history as normal entries, and nothing the user did is touched.
- A batch idle for 60 s is ended as if `end_batch` were called: its collected writes are recorded as one entry, normally, and the owner's later `end_batch` gets `batch_state`. A request counts as activity from the moment it is queued, so a write parked behind a gesture or another connection's batch doesn't let the batch time out.
- **`history.cancel_batch {}`** → `{ rev }` ends the open batch without recording it: its unrecorded writes are reverted as for a dropped connection (`changed` kind `cancel`), and in approval mode its collected writes are dropped, never submitted. Parts already recorded stay. `worlds-mcp` uses it when a write of a multi-write tool call fails.
- `begin_batch` with a batch open, or `end_batch` or `cancel_batch` with none, is `batch_state` (`-32007`). Batches don't nest.

**`history.undo { n?, any?, expect_rev? }`** and **`history.redo { n?, any?, expect_rev? }`** step the editor's shared undo stack up to `n` entries (default 1). They stop early, without error, at the first entry this client didn't make, unless `any: true` (refused in approval mode). → `{ rev, steps: [{ label, origin }], changes, stopped? }`, `origin` being who made each entry and `stopped` naming the entry they stopped at (`{ label, origin }`).

**`history.batch_status { batch_id }`** → `{ rev, batch_id, status, rev_applied?, error? }` for a batch submitted in approval mode.

```json
{ "jsonrpc": "2.0", "id": 45, "method": "history.begin_batch", "params": { "label": "Agent: wall off the gap" } }
```

```json
{ "jsonrpc": "2.0", "id": 45, "result": { "rev": 42 } }
```

```json
{ "jsonrpc": "2.0", "id": 46, "method": "grid.fill_rect", "params": { "level": "crevasse", "layer": "collision", "rect": [6, 1, 1, 3], "value": "solid" } }
```

```json
{ "jsonrpc": "2.0", "id": 46, "result": { "rev": 43, "status": "applied", "label": "Agent: wall off the gap",
  "changes": [ { "change": "grid", "world": "main", "level": "crevasse", "layer": "collision", "rect": [6, 2, 1, 2] } ] } }
```

```json
{ "jsonrpc": "2.0", "id": 47, "method": "grid.set_cells", "params": { "level": "crevasse", "layer": "collision", "cells": [[7, 3, "solid"]] } }
```

```json
{ "jsonrpc": "2.0", "id": 47, "result": { "rev": 44, "status": "applied", "label": "Agent: wall off the gap",
  "changes": [ { "change": "grid", "world": "main", "level": "crevasse", "layer": "collision", "rect": [7, 3, 1, 1] } ] } }
```

```json
{ "jsonrpc": "2.0", "id": 48, "method": "history.end_batch", "params": {} }
```

```json
{ "jsonrpc": "2.0", "id": 48, "result": { "rev": 44, "status": "applied", "label": "Agent: wall off the gap", "entries": 1,
  "changes": [ { "change": "grid", "world": "main", "level": "crevasse", "layer": "collision", "rect": [6, 2, 2, 2] } ] } }
```

```json
{ "jsonrpc": "2.0", "id": 49, "method": "history.undo", "params": {} }
```

```json
{ "jsonrpc": "2.0", "id": 49, "result": { "rev": 45, "steps": [ { "label": "Agent: wall off the gap", "origin": { "agent": "claude-code" } } ],
  "changes": [ { "change": "grid", "world": "main", "level": "crevasse", "layer": "collision", "rect": [6, 2, 2, 2] } ] } }
```

```json
{ "jsonrpc": "2.0", "id": 50, "method": "history.redo", "params": { "n": 1 } }
```

```json
{ "jsonrpc": "2.0", "id": 50, "result": { "rev": 46, "steps": [ { "label": "Agent: wall off the gap", "origin": { "agent": "claude-code" } } ],
  "changes": [ { "change": "grid", "world": "main", "level": "crevasse", "layer": "collision", "rect": [6, 2, 2, 2] } ] } }
```

### view

Editor only: in headless mode every `view.*` method is `unsupported` (`-32005`). They change what the editor shows, never the project, so they need no `expect_rev` and don't bump `rev`.

- **`view.focus { level }`**, **`{ world_rect: [x, y, w, h], world? }`** (world pixels), **`{ note: id }`** or **`{ world }`**: makes the level active and frames it, frames the rect, frames an annotation (at the largest zoom step that fits, or, for a rect too big for the smallest step, 1/256, the exact zoom that fits it): its bounding box as built (a grouped guide moved by its group's offset, a level note in world pixels), grown by 15 % of its size on each side and to at least 8 project cells each way (a pin is a point), or fits a whole world. The editor shows one world at a time: focusing a world makes it the active world, a level or note switches to the world it is in, and a `world_rect` with `world` switches to that world (without `world`, the active one). A level-scope note's level becomes active. → `{ rev }`. An unknown note is `command_error` `unknown_note`, an unknown world `unknown_world`.
- **`view.select { entities?, notes?, levels? }`** or **`{ level }`**: replaces the selection. The lists add up: give any of them, in any combination (empty lists are fine, and all of them empty clears the selection), and the selection holds every listed entity (by id), annotation (by id) and level (by slug); the editor's inspector edits a set of entities together. The active level becomes the first entity's level, else the first level-scope note's level, else the first of `levels`, else it stays. `{ level }` alone makes that level active and clears the selection; `level` with any list is `invalid params`. Every id and slug must exist, or nothing changes and the error is `command_error` (`unknown_entity`, `unknown_note`, `unknown_level`). → `{ rev }`. Sends `selection`.
- **`view.snapshot { level }`** or **`{ world_rect, world? }`**, plus `size?: [w, h]` (the largest image allowed, default `[1024, 1024]`), `layers?` (default all visible), `overlays?` (entity boxes and annotations: open notes, pins, paths and regions, and every guide, default `true`; a snapshot shows the data, so the editor's hidden tags don't apply) → `{ rev, png, width, height, scale }`. `png` is base64 (no `data:` prefix). The map is drawn alone, without the UI, at the largest scale that fits `size` (an integer scale when it is 1 or more). Rendered by the editor's offscreen renderer on the UI thread.

```json norun
{ "jsonrpc": "2.0", "id": 51, "method": "view.focus", "params": { "level": "crevasse" } }
```

```json norun
{ "jsonrpc": "2.0", "id": 51, "result": { "rev": 46 } }
```

```json norun
{ "jsonrpc": "2.0", "id": 52, "method": "view.select", "params": { "entities": ["doorcrev22"] } }
```

```json norun
{ "jsonrpc": "2.0", "id": 52, "result": { "rev": 46 } }
```

```json norun
{ "jsonrpc": "2.0", "id": 102, "method": "view.select", "params": { "notes": ["pinstatue2"], "levels": ["crevasse", "shaft"] } }
```

```json norun
{ "jsonrpc": "2.0", "id": 102, "result": { "rev": 46 } }
```

```json norun
{ "jsonrpc": "2.0", "id": 103, "method": "view.focus", "params": { "note": "pinstatue2" } }
```

```json norun
{ "jsonrpc": "2.0", "id": 103, "result": { "rev": 46 } }
```

```json norun
{ "jsonrpc": "2.0", "id": 53, "method": "view.snapshot", "params": { "level": "crevasse", "size": [640, 640] } }
```

```json norun
{ "jsonrpc": "2.0", "id": 53, "result": { "rev": 46, "png": "iVBORw0KGgoAAAANSUhEUgAAAoAAAACgCAYAAAA=", "width": 640, "height": 160, "scale": 2 } }
```

In headless mode:

```json
{ "jsonrpc": "2.0", "id": 54, "method": "view.snapshot", "params": { "level": "crevasse" } }
```

```json
{ "jsonrpc": "2.0", "id": 54, "error": { "code": -32005, "message": "view.snapshot needs the editor; open the project with `worlds <dir>`" } }
```

### events

**`events.subscribe { events: [name] }`** and **`events.unsubscribe { events: [name] }`** → `{ rev, events }` (the connection's subscriptions after the call). A new connection is subscribed to nothing, except that `batch` notifications for its own batches always come.

| Event | Params | Sent when |
| --- | --- | --- |
| `changed` | `{ rev, origin, session?, entry_origin?, label, kind, changes, files }`, described below | after every mutation, or once per batch |
| `selection` | `{ level, entities, notes, levels }`: `level` is the active level (or `null`), `entities` and `notes` every selected entity and annotation id, `levels` every selected level's slug | the editor's selection or active level changed |
| `saved` | `{ rev, files }` (project-relative) | after any save, by request or by the user |
| `problems` | `{ rev, problems }` | the problems list was recomputed and is sent whenever it changes: the editor revalidates shortly after edits (never mid-gesture), and after a save, a reload and `project.validate` |
| `agent` | `{ client, session, batch_id?, label, status }`; `status`: `queued`, `applied`, `rejected`, `failed`, `cancelled` | another client's batch changed state (for observers) |
| `batch` | `{ batch_id, status, rev?, error? }` | one of this connection's approval batches was decided |
| `approval` | `{ enabled }` | the user toggled approval mode |

```json
{ "jsonrpc": "2.0", "id": 55, "method": "events.subscribe", "params": { "events": ["changed", "saved", "problems"] } }
```

```json
{ "jsonrpc": "2.0", "id": 55, "result": { "rev": 46, "events": ["changed", "problems", "saved"] } }
```

```json
{ "jsonrpc": "2.0", "method": "changed", "params": { "rev": 47, "origin": "user", "label": "Paint collision", "kind": "edit",
  "changes": [ { "change": "grid", "world": "main", "level": "crevasse", "layer": "collision", "rect": [10, 1, 4, 1] } ],
  "files": ["worlds/main/levels/crevasse/collision.json"] } }
```

The user undoing an agent's batch:

```json
{ "jsonrpc": "2.0", "method": "changed", "params": { "rev": 48, "origin": "user", "entry_origin": { "agent": "claude-code" },
  "label": "Agent: wall off the gap", "kind": "undo",
  "changes": [ { "change": "grid", "world": "main", "level": "crevasse", "layer": "collision", "rect": [6, 2, 2, 2] } ],
  "files": ["worlds/main/levels/crevasse/collision.json"] } }
```

The user selecting a pin and two levels on the world view:

```json
{ "jsonrpc": "2.0", "method": "selection", "params": { "level": "crevasse", "entities": [], "notes": ["pinstatue2"], "levels": ["crevasse", "shaft"] } }
```

`changed` params:

- `origin`: who did this mutation, `"user"` or `{ "agent": "<name>" }`. For an undo or redo that is whoever stepped the history, not who made the entry.
- `session`: when `origin` is an agent, the [`session`](#sessionhello) of its connection, so two clients with the same name can be told apart (and a client can recognise its own edits).
- `entry_origin`: for `undo` and `redo` only, who made the entry being undone or redone: `"user"`, `{ "agent": "<name>" }`, or `"disk"` for entity definition files changed on disk that the editor applied as an entry.
- `label`: the entry's label (the batch label for a batch).
- `kind`: `edit`, `undo`, `redo`, `reload` or `cancel` (an open batch reverted: its connection dropped, or `history.cancel_batch`). `reload` has empty `changes` and `files`: re-read everything.
- `changes`: the [changes](commands.md#changes), coalesced; `files`: the project-relative files they touch (`Change::files()` of each, deduplicated, in order), the same files saving would write or delete: `worlds/<world>/world.json` for world-scope annotations (`note` with `level: null`) and guide groups, `worlds/<world>/levels/<level>/notes.json` for a level's notes, `level.json` for a level's tags (`level_meta` part `tags`).

### Approval mode

Off by default; the user turns it on per session from the toolbar. While it's on, agent writes don't apply right away: each becomes a **pending batch**, drawn on the canvas as a ghost overlay until the user accepts or rejects it.

- A write outside a batch is one pending batch. Inside `begin_batch`/`end_batch`, writes are collected, not applied (each returns `{ "rev", "status": "collected" }`, `rev` unchanged), and `end_batch` submits them as one pending batch, then waits for the decision exactly as a write does (below; accepted in time, its result is the batch's `applied` result with `entries`). Each collected request is read against the batch so far: a write may name a layer or level an earlier write of the same batch created.
- Accepting a batch applies it as one `batch` command, records one undo entry with the client's origin and the batch's label, and bumps `rev` once. Its `changed` event has `origin` the agent (with its `session`) and that label, so observers see the agent's edit, not the user's click.
- At submission the server checks `expect_rev` and applies the batch to a scratch copy to compute the ghost: a stale rev or a refused command is an immediate error, nothing is queued.
- The request then **blocks up to `approval_wait_ms`** (from `session.hello`, default 20 s). Accepted in time: the normal `applied` result. Rejected in time: error `rejected` (`-32003`). Neither: the result is `{ "rev", "status": "queued", "batch_id" }`, the connection moves on, and a `batch` notification follows when the user decides (`history.batch_status` asks).
- Pending batches stay in submission order; the editor accepts them oldest first (accepting a later one while an older one waits is refused, and the Apply button is only on the oldest), and rejecting one also rejects the later ones from the same client (they may build on it). An accepted batch applies to the project as it is then; if it no longer applies, its status is `failed` with the command error.
- Reads see the project without pending batches. A dropped connection leaves its pending batches for the user to decide. `history.undo`/`redo` of the client's own entries are never queued; `project.reload` is refused.

Why block-then-queue: an MCP tool call is a request with a timeout, and the model can't act on anything until it returns. Returning `queued` at once would make every approved edit look unapplied and push the agent into polling; blocking until the user decides would hit the tool timeout whenever they look away. Waiting a bounded 20 s covers the common case (the user is watching and clicks Accept) with a plain `applied` result, and degrades to an honest `queued` the agent can report, well inside typical tool timeouts.

```json norun
{ "jsonrpc": "2.0", "id": 60, "method": "grid.fill_rect", "params": { "level": "crevasse", "layer": "collision", "rect": [1, 1, 2, 1], "value": "solid" } }
```

```json norun
{ "jsonrpc": "2.0", "id": 60, "result": { "rev": 46, "status": "queued", "batch_id": "b7" } }
```

```json
{ "jsonrpc": "2.0", "method": "batch", "params": { "batch_id": "b7", "status": "applied", "rev": 47 } }
```

### project writes

```json
{ "jsonrpc": "2.0", "id": 56, "method": "project.export", "params": {} }
```

```json
{ "jsonrpc": "2.0", "id": 56, "result": { "rev": 46, "dir": "export", "files": ["world.json", "enums.json", "levels/crevasse.json", "levels/shaft.json", "levels/wing.json"], "warnings": [] } }
```

```json
{ "jsonrpc": "2.0", "id": 57, "method": "project.read_file", "params": { "path": "worlds/main/levels/wing/collision.json" } }
```

```json
{ "jsonrpc": "2.0", "id": 57, "result": { "rev": 46, "path": "worlds/main/levels/wing/collision.json",
  "text": "{\n  \"$schema\": \"../../../../schema/intgrid.json\",\n  \"format\": 1,\n  \"layer\": \"collision\",\n  \"legend\": {\n    \"#\": \"solid\",\n    \".\": null,\n    \"L\": \"ramp_l\",\n    \"f\": \"fortified\",\n    \"o\": \"oneway\",\n    \"~\": \"water\"\n  },\n  \"rows\": [\n    \"..######\",\n    \"..#....#\",\n    \"..######\",\n    \"........\"\n  ]\n}\n" } }
```

```json
{ "jsonrpc": "2.0", "id": 58, "method": "project.save", "params": {} }
```

```json
{ "jsonrpc": "2.0", "id": 58, "result": { "rev": 46, "files": ["entities/floor_button.json", "entities/locked_door.json", "enums.json", "project.json", "rules/macros.json", "rules/main_tiling.json",
  "worlds/main/levels/crevasse/collision.json", "worlds/main/levels/crevasse/decor_back.json", "worlds/main/levels/crevasse/entities.json",
  "worlds/main/levels/crevasse/level.json", "worlds/main/levels/crevasse/notes.json", "worlds/main/levels/wing", "worlds/main/world.json"] } }
```

```json
{ "jsonrpc": "2.0", "id": 59, "method": "project.reload", "params": {} }
```

```json
{ "jsonrpc": "2.0", "id": 59, "result": { "rev": 47 } }
```

```json
{ "jsonrpc": "2.0", "id": 61, "method": "level.delete", "params": { "level": "wing" } }
```

```json
{ "jsonrpc": "2.0", "id": 61, "error": { "code": -32002, "message": "deleting level wing needs confirm: true", "data": { "reason": "deletes level wing" } } }
```

```json
{ "jsonrpc": "2.0", "id": 62, "method": "level.delete", "params": { "level": "wing", "confirm": true } }
```

```json
{ "jsonrpc": "2.0", "id": 62, "result": { "rev": 48, "status": "applied", "label": "claude-code: Delete level wing",
  "changes": [ { "change": "level_deleted", "world": "main", "level": "wing" } ] } }
```

```json
{ "jsonrpc": "2.0", "id": 63, "method": "level.frobnicate", "params": {} }
```

```json
{ "jsonrpc": "2.0", "id": 63, "error": { "code": -32601, "message": "no method level.frobnicate" } }
```

```json
{ "jsonrpc": "2.0", "id": 64, "method": "grid.set_cells", "params": { "level": "crevasse", "layer": "collision", "cells": [], "text": "#", "origin": [1, 1] } }
```

```json
{ "jsonrpc": "2.0", "id": 64, "error": { "code": -32602, "message": "give either cells or text with origin, not both", "data": { "message": "give either cells or text with origin, not both" } } }
```

### derived grids and region rules

A walkthrough of the region and pass rule features on a new level: a derived layer of marks, a region rule writing marks along room floors (pass 0), a pass 1 rule drawing an occupying stamp on each mark, then hand overrides. The formats are in [format.md](format.md#region-rules).

A level with a 9-wide room and a 4-wide one:

```json
{ "jsonrpc": "2.0", "id": 104, "method": "level.create", "params": {
  "level": { "id": "hall222222", "slug": "hall", "name": "Hall", "world_px": [0, 160], "size": [16, 6], "depth": 0 },
  "content": { "int_grids": [ { "layer": "collision", "rows": ["################", "#.........#....#", "#.........#....#", "#.........#....#", "#.........#....#", "################"] } ] } } }
```

```json
{ "jsonrpc": "2.0", "id": 104, "result": { "rev": 49, "status": "applied", "label": "claude-code: Create level hall", "id": "hall222222",
  "changes": [ { "change": "level_created", "world": "main", "level": "hall" } ] } }
```

The derived layer, then the rules: a stamp that occupies its cells, a region rule writing a mark every 3 cells along the floor of rooms at least 6 wide, and a pass 1 rule drawing the stamp on each mark unless the cells are taken:

```json
{ "jsonrpc": "2.0", "id": 105, "method": "defs.set", "params": { "kind": "layer", "index": 1, "def": {
  "slug": "marks", "name": "Marks", "kind": "int_grid", "derived": true, "values": [ { "slug": "mark", "name": "Mark", "color": "#e0a040", "glyph": "m" } ] } } }
```

```json
{ "jsonrpc": "2.0", "id": 105, "result": { "rev": 50, "status": "applied", "label": "claude-code: Set layer marks",
  "changes": [ { "change": "def", "kind": "layer", "slug": "marks" } ] } }
```

```json
{ "jsonrpc": "2.0", "id": 106, "method": "rules.set_stamp", "params": { "layer": "main_tiling", "stamp": "post",
  "def": { "slug": "post", "tileset": "castle_main", "rect": [4, 4, 2, 2], "pivot": [0, 1.0], "occupies": true } } }
```

```json
{ "jsonrpc": "2.0", "id": 106, "result": { "rev": 51, "status": "applied", "label": "claude-code: Set stamp post on main_tiling",
  "changes": [ { "change": "def", "kind": "rules", "slug": "main_tiling" } ] } }
```

```json
{ "jsonrpc": "2.0", "id": 107, "method": "rules.set_group", "params": { "layer": "main_tiling", "group": { "slug": "room_marks", "rules": [
  { "slug": "floor_marks", "region": { "cells": ".", "min_width": 6 }, "place": { "along": "floor", "every": 3, "margin": 1 },
    "write": { "grid": "marks", "value": "mark" } } ] } } }
```

```json
{ "jsonrpc": "2.0", "id": 107, "result": { "rev": 52, "status": "applied", "label": "claude-code: Set rule group room_marks on main_tiling",
  "changes": [ { "change": "def", "kind": "rules", "slug": "main_tiling" } ] } }
```

```json
{ "jsonrpc": "2.0", "id": 108, "method": "rules.set_group", "params": { "layer": "main_tiling", "group": { "slug": "posts", "pass": 1, "rules": [
  { "slug": "post_on_mark", "match": { "grid": "marks", "rows": ["m"] }, "out": { "stamp": "post", "avoid_overlap": true } } ] } } }
```

```json
{ "jsonrpc": "2.0", "id": 108, "result": { "rev": 53, "status": "applied", "label": "claude-code: Set rule group posts on main_tiling",
  "changes": [ { "change": "def", "kind": "rules", "slug": "main_tiling" } ] } }
```

The rooms of the level, by empty cells, with their floors:

```json
{ "jsonrpc": "2.0", "id": 109, "method": "rules.regions", "params": { "level": "hall", "cells": ".", "along": "floor" } }
```

```json
{ "jsonrpc": "2.0", "id": 109, "result": { "rev": 53, "grid": "collision",
  "regions": [
    { "id": 0, "bbox": [1, 1, 9, 4], "width": 9, "height": 4, "area": 36, "edges": { "floor": [[1, 4, 9]] } },
    { "id": 1, "bbox": [11, 1, 4, 4], "width": 4, "height": 4, "area": 16, "edges": { "floor": [[11, 4, 4]] } } ],
  "picture": ["################", "#aaaaaaaaa#bbbb#", "#aaaaaaaaa#bbbb#", "#aaaaaaaaa#bbbb#", "#aaaaaaaaa#bbbb#", "################"] } }
```

What the region rule does with them: the 9-wide room is selected, its floor run `[1, 4, 9]` less a margin of 1 leaves x 2 to 8, so marks go at 2, 5 and 8; the 4-wide room isn't selected:

```json
{ "jsonrpc": "2.0", "id": 110, "method": "rules.preview", "params": { "layer": "main_tiling", "level": "hall", "rule": "floor_marks", "matches": true } }
```

```json
{ "jsonrpc": "2.0", "id": 110, "result": { "rev": 53, "fires": [
  { "rule": "floor_marks", "group": "room_marks", "pass": 0, "grid": "collision", "cells": [[2, 4], [5, 4], [8, 4]], "count": 3,
    "matched": [], "matched_count": 0, "occupied": [],
    "regions": [
      { "id": 0, "bbox": [1, 1, 9, 4], "area": 36, "selected": true, "runs": [[1, 4, 9]], "candidates": [[2, 4], [5, 4], [8, 4]] },
      { "id": 1, "bbox": [11, 1, 4, 4], "area": 16, "selected": false, "runs": [[11, 4, 4]], "candidates": [] } ],
    "write": { "grid": "marks", "value": "mark" } } ] } }
```

The marks are the derived layer's effective values:

```json
{ "jsonrpc": "2.0", "id": 111, "method": "grid.get_cells", "params": { "level": "hall", "layer": "marks", "rect": [0, 4, 16, 1] } }
```

```json
{ "jsonrpc": "2.0", "id": 111, "result": { "rev": 53, "rect": [0, 4, 16, 1], "legend": { ".": null, "m": "mark" }, "rows": ["..m..m..m......."] } }
```

Painting a derived layer sets overrides: a mark of our own at x 3, and the one at x 8 suppressed:

```json
{ "jsonrpc": "2.0", "id": 112, "method": "grid.set_cells", "params": { "level": "hall", "layer": "marks", "cells": [[3, 4, "mark"], [8, 4, "!"]] } }
```

```json
{ "jsonrpc": "2.0", "id": 112, "result": { "rev": 54, "status": "applied", "label": "claude-code: Paint 2 cells on marks",
  "changes": [ { "change": "grid", "world": "main", "level": "hall", "layer": "marks", "rect": [3, 4, 6, 1] } ] } }
```

```json
{ "jsonrpc": "2.0", "id": 113, "method": "grid.get_cells", "params": { "level": "hall", "layer": "marks", "rect": [0, 4, 16, 1], "stored": true } }
```

```json
{ "jsonrpc": "2.0", "id": 113, "result": { "rev": 54, "rect": [0, 4, 16, 1], "legend": { "m": "mark", "!": "!" }, "rows": ["...m....!......."] } }
```

The post at x 2 covers x 2 and 3, so the one the override asks for at x 3 is skipped (`occupied`):

```json
{ "jsonrpc": "2.0", "id": 114, "method": "rules.preview", "params": { "layer": "main_tiling", "level": "hall", "rule": "post_on_mark", "matches": true } }
```

```json
{ "jsonrpc": "2.0", "id": 114, "result": { "rev": 54, "fires": [
  { "rule": "post_on_mark", "group": "posts", "pass": 1, "grid": "marks", "cells": [[2, 4], [5, 4]], "count": 2,
    "matched": [[3, 4]], "matched_count": 1, "occupied": [[3, 4]] } ] } }
```

The text view draws the effective grid, then the overrides:

```json
{ "jsonrpc": "2.0", "id": 115, "method": "level.get", "params": { "level": "hall", "layers": ["marks"], "format": "text", "notes": "none" } }
```

```json
{ "jsonrpc": "2.0", "id": 115, "result": { "rev": 54, "text": "level hall \"Hall\" world main id hall222222\nsize 16x6 cells of 16 px, world_px [0, 160], depth 0\n\nint_grid marks (16x6 cells of 16 px) derived by main_tiling\nlegend: . empty, m mark\n  0         1\n  0123456789012345\n0 ................\n1 ................\n2 ................\n3 ................\n4 ..mm.m..........\n5 ................\noverrides: 2 cells (1 set, 1 suppressed; . = from the rules)\n  0         1\n  0123456789012345\n0 ................\n1 ................\n2 ................\n3 ................\n4 ...m....!.......\n5 ................\n\nentities: 0\n" } }
```

```json
{ "jsonrpc": "2.0", "id": 116, "method": "level.get", "params": { "level": "hall", "layers": ["marks"] } }
```

```json
{ "jsonrpc": "2.0", "id": 116, "result": { "rev": 54, "world": "main",
  "level": { "id": "hall222222", "slug": "hall", "name": "Hall", "world_px": [0, 160], "size": [16, 6], "depth": 0 },
  "content": {},
  "derived": { "marks": { "writer": "main_tiling",
    "rows": ["................", "................", "................", "................", "..mm.m..........", "................"],
    "overrides": ["................", "................", "................", "................", "...m....!.......", "................"] } } } }
```

`!` only applies to derived layers:

```json
{ "jsonrpc": "2.0", "id": 117, "method": "grid.set_cells", "params": { "level": "hall", "layer": "collision", "text": "!", "origin": [1, 1] } }
```

```json
{ "jsonrpc": "2.0", "id": 117, "error": { "code": -32004, "message": "\"!\" (suppress) only applies to derived layers; collision isn't derived",
  "data": { "kind": "invalid", "message": "\"!\" (suppress) only applies to derived layers; collision isn't derived" } } }
```

`project.bake` reports the tiles each auto layer baked to, and `warnings` (currently always empty):

```json
{ "jsonrpc": "2.0", "id": 118, "method": "project.bake", "params": { "levels": ["hall"] } }
```

```json
{ "jsonrpc": "2.0", "id": 118, "result": { "rev": 54, "levels": [ { "level": "hall", "layers": [ { "layer": "main_tiling", "tiles": 98 } ] } ],
  "warnings": [] } }
```

### project settings, rule state and replacing a level

A new project seed (every bake changes) and no default background, in one write:

```json
{ "jsonrpc": "2.0", "id": 119, "method": "project.update", "params": { "seed": 7, "bg_color": null } }
```

```json
{ "jsonrpc": "2.0", "id": 119, "result": { "rev": 55, "status": "applied", "label": "claude-code: Set project bg_color",
  "changes": [ { "change": "project", "key": "bg_color" }, { "change": "project", "key": "seed" } ] } }
```

The collision layer must be an `int_grid` layer:

```json
{ "jsonrpc": "2.0", "id": 120, "method": "project.update", "params": { "collision_layer": "main_tiling" } }
```

```json
{ "jsonrpc": "2.0", "id": 120, "error": { "code": -32004, "message": "layer \"main_tiling\" is a auto layer, this needs a int_grid layer",
  "data": { "kind": "wrong_layer_kind", "layer": "main_tiling", "expected": "int_grid", "actual": "auto", "message": "layer \"main_tiling\" is a auto layer, this needs a int_grid layer" } } }
```

Re-rolling `main_tiling` in `crevasse` and switching its `pillars` group on there too; the state replaces the old one whole, so `flooded_decor` is listed again:

The grid can't change once there are levels, since their sizes and positions are in cells of it:

```json
{ "jsonrpc": "2.0", "id": 121, "method": "project.update", "params": { "grid_size": 8 } }
```

```json
{ "jsonrpc": "2.0", "id": 121, "error": { "code": -32004, "message": "grid_size can only change while the project has no levels (it has 3): their sizes and positions are in cells of it",
  "data": { "kind": "invalid", "message": "grid_size can only change while the project has no levels (it has 3): their sizes and positions are in cells of it" } } }
```

`solid` and `fortified` block movement in the export, new levels are 24x12 cells, and every save also writes `export/` (`null` would leave `blocking_values` or `default_level_size` out of `project.json` again):

```json
{ "jsonrpc": "2.0", "id": 122, "method": "project.update", "params": { "blocking_values": ["solid", "fortified"], "default_level_size": [24, 12], "export_on_save": true } }
```

```json
{ "jsonrpc": "2.0", "id": 122, "result": { "rev": 56, "status": "applied", "label": "claude-code: Set project blocking_values",
  "changes": [ { "change": "project", "key": "blocking_values" }, { "change": "project", "key": "default_level_size" },
               { "change": "project", "key": "export_on_save" } ] } }
```

```json
{ "jsonrpc": "2.0", "id": 123, "method": "level.set_layer_state", "params": { "level": "crevasse", "layer": "main_tiling",
  "state": { "seed": 9, "optional_groups": ["flooded_decor", "pillars"] } } }
```

```json
{ "jsonrpc": "2.0", "id": 123, "result": { "rev": 57, "status": "applied", "label": "claude-code: Set main_tiling rule state on crevasse",
  "changes": [ { "change": "layer_state", "world": "main", "level": "crevasse", "layer": "main_tiling" } ] } }
```

Only `optional` groups are switched on per level:

```json
{ "jsonrpc": "2.0", "id": 124, "method": "level.set_layer_state", "params": { "level": "crevasse", "layer": "main_tiling", "state": { "optional_groups": ["walls"] } } }
```

```json
{ "jsonrpc": "2.0", "id": 124, "error": { "code": -32004, "message": "rule group walls of main_tiling is not optional; only optional groups are switched on per level",
  "data": { "kind": "invalid", "message": "rule group walls of main_tiling is not optional; only optional groups are switched on per level" } } }
```

Rewriting `hall` as a smaller room in one step. Its `marks` overrides aren't in the new content, so that needs `confirm`:

```json
{ "jsonrpc": "2.0", "id": 125, "method": "level.replace", "params": {
  "level": { "slug": "hall", "name": "Hall", "world_px": [0, 160], "size": [8, 4], "depth": 0 },
  "content": { "int_grids": [ { "layer": "collision", "rows": ["########", "#......#", "#......#", "########"] } ] } } }
```

```json
{ "jsonrpc": "2.0", "id": 125, "error": { "code": -32002,
  "message": "replacing level hall drops the marks overrides, which the new content leaves out. Send confirm: true to do it",
  "data": { "reason": "drops the marks overrides, which the new content leaves out" } } }
```

```json
{ "jsonrpc": "2.0", "id": 126, "method": "level.replace", "params": {
  "level": { "slug": "hall", "name": "Hall", "world_px": [0, 160], "size": [8, 4], "depth": 0 },
  "content": { "int_grids": [ { "layer": "collision", "rows": ["########", "#......#", "#......#", "########"] } ] }, "confirm": true } }
```

```json
{ "jsonrpc": "2.0", "id": 126, "result": { "rev": 58, "status": "applied", "label": "claude-code: Replace level hall",
  "changes": [
    { "change": "level_meta", "world": "main", "level": "hall", "part": "all" },
    { "change": "grid", "world": "main", "level": "hall", "layer": "collision", "rect": [0, 0, 16, 6] },
    { "change": "grid", "world": "main", "level": "hall", "layer": "marks", "rect": [0, 0, 16, 6] } ] } }
```

Undo puts the old level back, overrides and all:

```json
{ "jsonrpc": "2.0", "id": 127, "method": "history.undo", "params": {} }
```

```json
{ "jsonrpc": "2.0", "id": 127, "result": { "rev": 59, "steps": [ { "label": "claude-code: Replace level hall", "origin": { "agent": "claude-code" } } ],
  "changes": [
    { "change": "level_meta", "world": "main", "level": "hall", "part": "all" },
    { "change": "grid", "world": "main", "level": "hall", "layer": "collision", "rect": [0, 0, 16, 6] },
    { "change": "grid", "world": "main", "level": "hall", "layer": "marks", "rect": [0, 0, 16, 6] } ] } }
```

### clipboard: copy and paste

`edit.copy` reads a **fragment** out of the project ([format.md](format.md#clipboard-fragments)): what the editor's Ctrl+C puts on the clipboard. `edit.paste` writes one into a level as one batch and one undo entry, `Paste <n> items`. A fragment is plain JSON, so an agent can also write one itself.

| Method | Params | Result |
| --- | --- | --- |
| `edit.copy` | `{ selection: { level?, entities?: [id], notes?: [id], tiles?: { layer: [[x, y]] }, rect?: [x, y, w, h], layers?: [slug] } }`. Everything comes from one level (`level`, else the level of the first entity or level note); world notes of its world may come along. `tiles` names tile stacks by cell; `rect` (cells of the project grid) copies the cells of `layers` inside it, empty ones included (default: every int_grid layer that isn't derived; tiles layers may be listed) | `{ rev, fragment }` (a read). Refused (`command_error`): unknown ids, levels or layers, things from two levels, a rect outside the level, a derived or auto layer |
| `edit.paste` (write) | `{ level, origin: [x, y], fragment, layer_map?: { from: to } }`: `origin` is where the fragment's top-left corner goes, in cells of the project grid (may be negative) | `batch` of `paint_cells`, `set_tiles`, `add_entity`, `add_note`. The write result adds `items` (how many grid cells, tile stacks, entities and notes), `ids` (fragment id to new id), `entities` and `notes` (the new ids, in fragment order) and `problems` (what was clipped, refused, skipped or nulled, in words). Every entity and note gets a new id, references inside the fragment follow, references outside are kept when they resolve here (see [format.md](format.md#what-a-paste-does)). A fragment of another version or project grid size is `invalid_params`/`invalid`; a paste that would write nothing is `invalid`, naming the problems |

Copy the door at the end of `crevasse` with the cells around its foot:

```json
{ "jsonrpc": "2.0", "id": 128, "method": "edit.copy", "params": { "selection": { "entities": ["doorcrev22"], "rect": [15, 3, 4, 1] } } }
```

```json
{ "jsonrpc": "2.0", "id": 128, "result": { "rev": 59, "fragment": { "worlds_fragment": 1, "grid_size": 16, "size": [4, 1],
  "source": { "level": "crevasse", "origin": [15, 3] },
  "grids": { "collision": { "legend": { "#": "solid", "f": "fortified" }, "rows": ["f###"] } },
  "entities": [
    { "id": "doorcrev22", "type": "locked_door", "layer": "objects", "cell": [3, 0], "size": [1, 2],
      "fields": { "exit_to": "doorshaft2", "facing": "right", "needs_item": "brass_key", "script": "scripts/door.gd",
                  "speed": 2, "sprite": { "tileset": "castle_main", "tile": [4, 4] }, "tint": "#ff8080",
                  "waypoints": [[-14, -2], [-13, -2]] } } ] } } }
```

A fragment is plain JSON, so an agent can write one too. This one is a button, the door it opens and a strip of one-way floor, pasted at cell [2, 1] of `crevasse`. The pasted button targets the pasted door; the door's `exit_to` still finds `doorshaft2` (a `cross_level` field), so it is kept:

```json
{ "jsonrpc": "2.0", "id": 129, "method": "edit.paste", "params": { "level": "crevasse", "origin": [2, 1], "fragment": {
  "worlds_fragment": 1, "grid_size": 16, "size": [7, 1],
  "grids": { "collision": { "rows": ["  ooo"] } },
  "entities": [
    { "id": "btncrev222", "type": "floor_button", "layer": "objects", "cell": [0, 0], "fields": { "target": "doorcrev22" } },
    { "id": "doorcrev22", "type": "locked_door", "layer": "objects", "cell": [6, 0], "fields": { "exit_to": "doorshaft2" } } ] } } }
```

```json
{ "jsonrpc": "2.0", "id": 129, "result": { "rev": 60, "status": "applied", "label": "claude-code: Paste 5 items",
  "items": 5, "ids": { "btncrev222": "75rk7gzrhn", "doorcrev22": "5w6isso7ut" }, "entities": ["75rk7gzrhn", "5w6isso7ut"], "notes": [], "problems": [],
  "changes": [
    { "change": "grid", "world": "main", "level": "crevasse", "layer": "collision", "rect": [4, 1, 3, 1] },
    { "change": "entity", "world": "main", "level": "crevasse", "id": "75rk7gzrhn" },
    { "change": "entity", "world": "main", "level": "crevasse", "id": "5w6isso7ut" } ] } }
```

The new ids are derived from the destination level, the origin and the fragment's ids, so the same paste into the same project gives the same ids; pasting it again at the same place moves on to other free ones.

**`level.duplicate`** copies a whole level:

```json
{ "jsonrpc": "2.0", "id": 130, "method": "level.duplicate", "params": { "level": "shaft", "slug": "shaft_b" } }
```

```json
{ "jsonrpc": "2.0", "id": 130, "result": { "rev": 61, "status": "applied", "label": "claude-code: Duplicate level shaft as shaft_b",
  "id": "s64pc4niie", "world_px": [416, 80],
  "changes": [ { "change": "level_created", "world": "main", "level": "shaft_b" } ] } }
```

### find: entities, levels, notes and tiles

Four reads search the whole project, every world, with one **query language** (the editor's Find pane and the MCP `find` tool use the same one, from `worlds_core::find`). A `query` is words separated by spaces, and every word must hold:

| Word | Matches |
| --- | --- |
| `type:locked_door` | entities of that type |
| `tag:puzzle` | an entity whose type has the tag, a level or an annotation with the tag |
| `field:target = null`, `field:needs_item != ""` | a field's effective value (stored, else the default, else `null`) equal or not equal to the value; spaces around the operator are optional |
| `channel:3` | any other `key:value` is a field test, `field:channel = 3` |
| `world:main`, `level:crevasse`, `layer:objects` | where it is (`level:` never matches a world-scope annotation) |
| `kind:pin`, `status:open` | annotations of that kind (`note`, `pin`, `path`, `region`, `guide`, `connector`) or status (`open`, `done`) |
| `tileset:castle_main`, `tile:4,4` | tile placements from that tileset, of that atlas `[column, row]` |
| `brass`, `"key room"` | free text, case-insensitive, in an entity's id, type slug or type name; a level's slug, name or id; an annotation's id, title, body or pin or connector label |

Values are read as JSON when they parse (`3`, `true`, `null`, `[1,2]`, written without spaces), else as text; a quoted value (`"3"`) is always text. Numbers compare by value (`2` equals `2.0`). Slug values (`type`, `tag`, `world`, `level`, `layer`, `tileset`) may use `*` as a wildcard: `type:*door`. Each method takes only the words that make sense for it, and anything else is `invalid_params` naming it ("type: is for entities, not levels"): field tests are for entities and levels, `type` for entities, `kind` and `status` for annotations, `tileset` and `tile` for tiles, and free text for everything but tiles. The other parameters are the same tests spelled as keys, and add up with `query`.

| Method | Params | Result |
| --- | --- | --- |
| `entity.find` | `{ query?, type?, tag?, fields?, world?, level?, layer?, text?, rect? }` | `{ rev, entities }`, as in [entity](#entity) |
| `level.find` | `{ query?, tag?, fields?: { slug: value }, world?, text? }` | `{ rev, levels }`, in `level.list` order, each as `level.list` gives it plus `tags` and the stored `fields` |
| `tiles.find` | `{ query?, tileset?, tile?: [col, row], world?, level?, layer?, limit? }` | `{ rev, tiles, total }`: hand-placed tiles (not baked autotiles), level by level and in file order, each placement as in the layer file plus `world`, `level` and `layer`; at most `limit` (default 1000) of them, `total` counting every match |
| `notes.list` | the filters in [notes](#notes), plus `text?` and `query?` | `{ rev, notes }` |

Floor buttons that open something, then levels by name:

```json
{ "jsonrpc": "2.0", "id": 131, "method": "entity.find", "params": { "query": "type:floor_button field:target != null" } }
```

```json
{ "jsonrpc": "2.0", "id": 131, "result": { "rev": 61, "entities": [
  { "id": "75rk7gzrhn", "type": "floor_button", "level": "crevasse", "world": "main", "layer": "objects", "cell": [2, 1], "offset_px": [0, 0], "size": null,
    "fields": { "target": "5w6isso7ut" } } ] } }
```

```json
{ "jsonrpc": "2.0", "id": 132, "method": "level.find", "params": { "query": "shaft" } }
```

```json
{ "jsonrpc": "2.0", "id": 132, "result": { "rev": 61, "levels": [
  { "slug": "shaft", "name": "Shaft", "id": "shaft22222", "world": "main", "world_px": [320, 80], "size": [6, 8], "depth": 1, "tags": [], "fields": {} },
  { "slug": "shaft_b", "name": "Shaft copy", "id": "s64pc4niie", "world": "main", "world_px": [416, 80], "size": [6, 8], "depth": 1, "tags": [], "fields": {} } ] } }
```

Where one tileset's tiles are, and the open todo notes that mention the gap:

```json
{ "jsonrpc": "2.0", "id": 133, "method": "tiles.find", "params": { "query": "tileset:castle_main tile:5,1", "level": "crevasse" } }
```

```json
{ "jsonrpc": "2.0", "id": 133, "result": { "rev": 61, "total": 1, "tiles": [
  { "world": "main", "level": "crevasse", "layer": "decor_back", "cell": [4, 2], "tileset": "castle_main", "tile": [5, 1] } ] } }
```

```json
{ "jsonrpc": "2.0", "id": 134, "method": "notes.list", "params": { "query": "tag:todo status:open gap" } }
```

```json
{ "jsonrpc": "2.0", "id": 134, "result": { "rev": 61, "notes": [
  { "scope": { "level": "crevasse" }, "note": { "id": "notecrev22", "kind": "note", "title": "Widen the gap", "body": "Jump is **too easy**.\nTry 5 cells.",
    "px": [160, 48], "tags": ["todo"], "links": ["entity:btncrev222", "note:pinstatue2", "https://example.com"] } } ] } }
```

### worlds

A project holds one or more worlds: separate maps, each with its own levels, world-scope annotations and guide groups ([commands.md](commands.md#world-commands)). Layers, tilesets, entity types, enums, rules and the grid are shared, and level slugs are unique across worlds, so every method that takes a `level` finds it in whichever world it is. `project.describe` lists the worlds in display order (`project.json` `worlds`) with their levels, `level_count` and `bbox`.

| Write | Params | Command |
| --- | --- | --- |
| `world.create` | `{ slug, name? }` | `create_world`: an empty world, last in the order |
| `world.rename` | `{ world, slug?, name? }` (at least one; `name: ""` removes the name) | `rename_world`: a new slug moves the world's directory on save and keeps its place in the order |
| `world.reorder` | `{ order }`: every world's slug once (`[]` drops the list: slug order) | `reorder_worlds` |
| `world.remove` | `{ world, confirm? }` | `remove_world`, with its levels, world notes and guide groups; needs `confirm` unless the world is empty ([Confirmation](#confirmation), reason `deletes world <w> with <what it holds>`). The last world can't be removed (`invalid`) |
| `level.move_to_world` | `{ level, world, world_px? }` | `move_level_to_world`: the level keeps its slug, id and content; `world_px` is its corner in the new world (default: unchanged). Into its own world it is a `level.move` |

The changes are `world_created`, `world_renamed` (`from`, `to`), `world_meta` (the name), `world_removed` (after a `level_deleted` per level and a `note` per world note), and `project` key `worlds` when the order in `project.json` changes; moving a level reports `level_deleted` in the old world and `level_created` in the new one. A slug that is taken is `command_error` `duplicate_world`.

```json
{ "jsonrpc": "2.0", "id": 135, "method": "world.create", "params": { "slug": "underdark", "name": "Underdark" } }
```

```json
{ "jsonrpc": "2.0", "id": 135, "result": { "rev": 62, "status": "applied", "label": "claude-code: Create world underdark",
  "changes": [
    { "change": "world_created", "world": "underdark" },
    { "change": "project", "key": "worlds" } ] } }
```

```json
{ "jsonrpc": "2.0", "id": 136, "method": "level.move_to_world", "params": { "level": "hall", "world": "underdark", "world_px": [0, 0] } }
```

```json
{ "jsonrpc": "2.0", "id": 136, "result": { "rev": 63, "status": "applied", "label": "claude-code: Move level hall to world underdark",
  "changes": [
    { "change": "level_deleted", "world": "main", "level": "hall" },
    { "change": "level_created", "world": "underdark", "level": "hall" } ] } }
```

```json
{ "jsonrpc": "2.0", "id": 137, "method": "world.rename", "params": { "world": "underdark", "slug": "caves", "name": "The Caves" } }
```

```json
{ "jsonrpc": "2.0", "id": 137, "result": { "rev": 64, "status": "applied", "label": "claude-code: Rename world underdark to caves",
  "changes": [
    { "change": "world_renamed", "from": "underdark", "to": "caves" },
    { "change": "project", "key": "worlds" },
    { "change": "world_meta", "world": "caves" } ] } }
```

```json
{ "jsonrpc": "2.0", "id": 138, "method": "world.reorder", "params": { "order": ["caves", "main"] } }
```

```json
{ "jsonrpc": "2.0", "id": 138, "result": { "rev": 65, "status": "applied", "label": "claude-code: Reorder worlds",
  "changes": [ { "change": "project", "key": "worlds" } ] } }
```

```json
{ "jsonrpc": "2.0", "id": 139, "method": "world.create", "params": { "slug": "main" } }
```

```json
{ "jsonrpc": "2.0", "id": 139, "error": { "code": -32004, "message": "a world \"main\" already exists",
  "data": { "kind": "duplicate_world", "world": "main", "message": "a world \"main\" already exists" } } }
```

```json
{ "jsonrpc": "2.0", "id": 140, "method": "world.remove", "params": { "world": "caves" } }
```

```json
{ "jsonrpc": "2.0", "id": 140, "error": { "code": -32002, "message": "deleting world caves needs confirm: true: it holds 1 level",
  "data": { "reason": "deletes world caves with 1 level" } } }
```

```json
{ "jsonrpc": "2.0", "id": 141, "method": "world.remove", "params": { "world": "caves", "confirm": true } }
```

```json
{ "jsonrpc": "2.0", "id": 141, "result": { "rev": 66, "status": "applied", "label": "claude-code: Delete world caves",
  "changes": [
    { "change": "level_deleted", "world": "caves", "level": "hall" },
    { "change": "world_removed", "world": "caves" },
    { "change": "project", "key": "worlds" } ] } }
```

### layer operations

Copy a layer, merge a tiles layer down into the one below it, or move tile stacks or entities to another layer of the same kind ([commands.md](commands.md#layer-operations)). Each is one write and one undo entry; merge and the moves are a `batch` built by `worlds_core::command::layer_ops`, so the editor's Layers pane and these methods make the same commands.

| Method | Params | Result / command |
| --- | --- | --- |
| `defs.duplicate` | `{ kind: "layer", slug, to? }`, `to` default `<slug>_copy` | `duplicate_layer { layer: slug, slug: to }`: the definition right above the original, its content in every level, its rule state and (an `auto` layer) its rules. Copied entities get new ids |
| `defs.merge_down` | `{ kind: "layer", slug }` | a `batch` labelled "Merge \<slug\> down into \<lower\>": a `set_tiles` per level on the layer right below (each of the upper layer's stacks on top of the lower's), then `remove_layer_def`. Refused (`command_error`) unless both are tiles layers on the same grid |
| `tiles.move_to_layer` | `{ level, layer, to, cells: [[x, y]] }` | a `batch` labelled "Move \<n\> tile stacks to \<to\>": the stacks at `cells` go on top of `to`'s there, and leave `layer`. Both tiles layers on the same grid; cells without tiles are skipped |
| `entity.move_to_layer` | `{ ids, layer }` | a `batch` labelled "Move \<id\> to \<layer\>" or "Move \<n\> entities to \<layer\>": per entity `remove_entity` and `add_entity` on `layer` in its level, same id, cell and fields. `layer` is an entities layer on the same grid; entities already on it are skipped |

Nothing here needs `confirm`: no content is dropped, and undo puts everything back.

```json
{ "jsonrpc": "2.0", "id": 138, "method": "defs.duplicate", "params": { "kind": "layer", "slug": "decor_back" } }
```

```json
{ "jsonrpc": "2.0", "id": 138, "result": { "rev": 67, "status": "applied", "label": "claude-code: Duplicate layer decor_back as decor_back_copy",
  "changes": [
    { "change": "def", "kind": "layer", "slug": "decor_back_copy" },
    { "change": "tiles", "world": "main", "level": "crevasse", "layer": "decor_back_copy", "rect": [0, 0, 40, 10] } ] } }
```

```json
{ "jsonrpc": "2.0", "id": 139, "method": "tiles.move_to_layer", "params": { "level": "crevasse", "layer": "decor_back", "to": "decor_back_copy", "cells": [[4, 2], [10, 6]] } }
```

```json
{ "jsonrpc": "2.0", "id": 139, "result": { "rev": 68, "status": "applied", "label": "claude-code: Move 2 tile stacks to decor_back_copy",
  "changes": [
    { "change": "tiles", "world": "main", "level": "crevasse", "layer": "decor_back_copy", "rect": [4, 2, 7, 5] },
    { "change": "tiles", "world": "main", "level": "crevasse", "layer": "decor_back", "rect": [4, 2, 7, 5] } ] } }
```

```json
{ "jsonrpc": "2.0", "id": 140, "method": "defs.merge_down", "params": { "kind": "layer", "slug": "decor_back_copy" } }
```

```json
{ "jsonrpc": "2.0", "id": 140, "result": { "rev": 69, "status": "applied", "label": "claude-code: Merge decor_back_copy down into decor_back",
  "changes": [
    { "change": "tiles", "world": "main", "level": "crevasse", "layer": "decor_back", "rect": [4, 2, 19, 5] },
    { "change": "def", "kind": "layer", "slug": "decor_back_copy" },
    { "change": "tiles", "world": "main", "level": "crevasse", "layer": "decor_back_copy", "rect": [0, 0, 40, 10] } ] } }
```

A tiles layer doesn't merge into the auto layer below it, and an entity only moves to an entities layer:

```json
{ "jsonrpc": "2.0", "id": 141, "method": "defs.merge_down", "params": { "kind": "layer", "slug": "decor_back" } }
```

```json
{ "jsonrpc": "2.0", "id": 141, "error": { "code": -32004,
  "message": "the layer below decor_back is main_tiling, an auto layer; only a tiles layer merges into a tiles layer",
  "data": { "kind": "invalid", "message": "the layer below decor_back is main_tiling, an auto layer; only a tiles layer merges into a tiles layer" } } }
```

```json
{ "jsonrpc": "2.0", "id": 142, "method": "entity.move_to_layer", "params": { "ids": ["doorcrev22"], "layer": "collision" } }
```

```json
{ "jsonrpc": "2.0", "id": 142, "error": { "code": -32004,
  "message": "layer \"collision\" is a int_grid layer, this needs a entities layer",
  "data": { "kind": "wrong_layer_kind", "layer": "collision", "expected": "entities", "actual": "int_grid",
    "message": "layer \"collision\" is a int_grid layer, this needs a entities layer" } } }
```

### reference images

Reference images are drawn under the layer stack in the editor and never exported ([format.md](format.md#reference-images)). On the wire a reference is `{ scope, reference }`, `scope` being `{ "world": slug }` or `{ "level": slug }` and `reference` the entry in file form (`id`, `image`, `px`, and `scale`, `opacity`, `locked`, `visible` when not at their default). `image` is a path inside the project; a file that doesn't exist is accepted and reported by `project.validate`. As for notes, `locked` is this API's rule, not the commands': moving or scaling a locked reference (`px`, `scale`) with `reference.update`, or removing one, needs `confirm: true`; its opacity, visibility, image and `locked` itself never do.

| Method | Params | Result / command |
| --- | --- | --- |
| `reference.list` | `{ scope? }` | `{ rev, references }`, world references first, then levels in `level.list` order, each list bottom first |
| `reference.add` | `{ scope, reference, index? }`; `reference.id` optional | `add_reference`; result has `id` |
| `reference.update` | `{ id, reference }` (wholesale) or `{ id, patch }` (top-level keys merged in; `null` puts a key back to its default), plus `confirm?` for a locked one | `update_reference` |
| `reference.remove` | `{ ids, confirm? }` | `batch` of `remove_reference` |

A reference's change is `{ "change": "reference", "world", "level", "id" }` (`level: null` at world scope); its file is the `level.json` or `world.json` that holds it.

```json
{ "jsonrpc": "2.0", "id": 143, "method": "reference.add", "params": { "scope": { "level": "crevasse" },
  "reference": { "id": "refsketch2", "image": "art/sketch.png", "px": [0, -16], "opacity": 0.5 } } }
```

```json
{ "jsonrpc": "2.0", "id": 143, "result": { "rev": 70, "status": "applied", "label": "claude-code: Add reference image", "id": "refsketch2",
  "changes": [ { "change": "reference", "world": "main", "level": "crevasse", "id": "refsketch2" } ] } }
```

```json
{ "jsonrpc": "2.0", "id": 144, "method": "reference.update", "params": { "id": "refsketch2", "patch": { "scale": 2, "locked": true } } }
```

```json
{ "jsonrpc": "2.0", "id": 144, "result": { "rev": 71, "status": "applied", "label": "claude-code: Edit reference image refsketch2",
  "changes": [ { "change": "reference", "world": "main", "level": "crevasse", "id": "refsketch2" } ] } }
```

```json
{ "jsonrpc": "2.0", "id": 145, "method": "reference.list", "params": {} }
```

```json
{ "jsonrpc": "2.0", "id": 145, "result": { "rev": 71, "references": [
  { "scope": { "level": "crevasse" }, "reference": { "id": "refsketch2", "image": "art/sketch.png", "px": [0, -16], "scale": 2.0, "opacity": 0.5, "locked": true } } ] } }
```

It is locked now, so moving it needs `confirm`:

```json
{ "jsonrpc": "2.0", "id": 146, "method": "reference.update", "params": { "id": "refsketch2", "patch": { "px": [16, -16] } } }
```

```json
{ "jsonrpc": "2.0", "id": 146, "error": { "code": -32002, "message": "reference image refsketch2 is locked; moving or scaling it needs confirm: true",
  "data": { "reason": "reference image refsketch2 is locked; moving or scaling it needs confirm: true" } } }
```

```json
{ "jsonrpc": "2.0", "id": 147, "method": "reference.remove", "params": { "ids": ["refsketch2"], "confirm": true } }
```

```json
{ "jsonrpc": "2.0", "id": 147, "result": { "rev": 72, "status": "applied", "label": "claude-code: Remove reference image refsketch2",
  "changes": [ { "change": "reference", "world": "main", "level": "crevasse", "id": "refsketch2" } ] } }
```

### import from another project

Bring definitions, worlds or levels of another Worlds project into this one ([importing_worlds.md](importing_worlds.md)). Planning and applying are two calls, so the plan can be shown before anything changes.

| Method | Params | Result |
| --- | --- | --- |
| `project.import_plan` | `{ source, selection?, policy? }`: `source` is a project directory, absolute or relative to this project's. `selection`: `{ preset?: "pick" \| "defs_only" \| "everything", tilesets?, enums?, patterns?, legends?, templates?, layers?, level_fields?, entities?, settings?, worlds?, levels? }` (lists of slugs; `settings` are `grid_size`, `bg_color`, `collision_layer`, `blocking_values`, `default_level_size`). `policy`: `{ on_conflict?: "skip" \| "overwrite" \| "rename", conflicts?: { "<kind>:<slug>": ... }, placement?: { mode?: "new_world" \| "world" \| "keep", world?, offset?: [x, y] } }` | `{ rev, plan_id, empty, source, label, summary, items, ids, dropped, problems, assets }` (a read: nothing changes). `items` lists every item with its `kind`, `slug`, `action` (`add`, `same`, `skip`, `overwrite`, `rename`, `refused`), `to` (a rename's new slug), `world` (where a level or guide group goes), `required_by` (for a dependency nobody selected) and `reason`. `ids`: instance ids re-rolled (`{ kind, from, to }`); `dropped`: references nothing here resolves; `assets`: files to copy under `art/` (`{ from, to, action, reason? }`). An unknown slug in the selection is `command_error`; a source that doesn't load is `invalid_params` |
| `project.import` (write) | `{ plan_id }` | Copies the plan's files, then applies it as one `batch` labelled `Import from <name>`: one undo entry. The write result adds `plan_id`, `summary` and `copied` (the files written). Refused with `stale_rev` when the project changed since the plan was made (plan again), `invalid_params` for an unknown `plan_id` (plans are kept until used, the last 16), `invalid` when the plan imports nothing |

Plan an import of two entity types from the `game` project next to this one, renaming what collides. `locked_door` differs from ours, so theirs comes in as `locked_door_2`; `lever` needs nothing we lack:

```json
{ "jsonrpc": "2.0", "id": 148, "method": "project.import_plan", "params": { "source": "../game.worlds",
  "selection": { "entities": ["lever", "locked_door"] }, "policy": { "on_conflict": "rename" } } }
```

```json
{ "jsonrpc": "2.0", "id": 148, "result": { "rev": 72, "plan_id": "plan_1", "empty": false, "source": "game",
  "label": "Import from game", "summary": "2 definitions, 0 levels, 1 renamed, 0 skipped",
  "items": [
    { "kind": "entity", "slug": "lever", "action": "add" },
    { "kind": "entity", "slug": "locked_door", "action": "rename", "to": "locked_door_2" } ],
  "ids": [], "dropped": [], "problems": [], "assets": [] } }
```

```json
{ "jsonrpc": "2.0", "id": 149, "method": "project.import", "params": { "plan_id": "plan_1" } }
```

```json
{ "jsonrpc": "2.0", "id": 149, "result": { "rev": 73, "status": "applied", "label": "claude-code: Import from game",
  "plan_id": "plan_1", "summary": "2 definitions, 0 levels, 1 renamed, 0 skipped", "copied": [],
  "changes": [
    { "change": "def", "kind": "entity", "slug": "lever" },
    { "change": "def", "kind": "entity", "slug": "locked_door_2" } ] } }
```

A plan made before the project changed is refused; plan again:

```json
{ "jsonrpc": "2.0", "id": 150, "method": "project.import_plan", "params": { "source": "../game.worlds",
  "selection": { "entities": ["torch"] } } }
```

```json
{ "jsonrpc": "2.0", "id": 150, "result": { "rev": 73, "plan_id": "plan_2", "empty": false, "source": "game",
  "label": "Import from game", "summary": "1 definition, 0 levels, 0 renamed, 0 skipped",
  "items": [ { "kind": "entity", "slug": "torch", "action": "add" } ],
  "ids": [], "dropped": [], "problems": [], "assets": [] } }
```

```json
{ "jsonrpc": "2.0", "id": 151, "method": "world.create", "params": { "slug": "annex" } }
```

```json
{ "jsonrpc": "2.0", "id": 151, "result": { "rev": 74, "status": "applied", "label": "claude-code: Create world annex",
  "changes": [
    { "change": "world_created", "world": "annex" },
    { "change": "project", "key": "worlds" } ] } }
```

```json
{ "jsonrpc": "2.0", "id": 152, "method": "project.import", "params": { "plan_id": "plan_2" } }
```

```json
{ "jsonrpc": "2.0", "id": 152, "error": { "code": -32001,
  "message": "the project changed since plan plan_2 was made (rev 67 then, 68 now): call project.import_plan again",
  "data": { "expected": 67, "current": 68 } } }
```

## Headless mode

`worlds_server::headless` runs the same method handler over a `Project` loaded from a directory, with its own `History` and `BakeCache`: `headless::Session` answers calls in-process with no socket, and `headless::run` (the `worlds-server <dir>` binary) serves it over the same WebSocket and `server.json` as the editor (the docs test uses that). Differences from the editor:

- Requests apply immediately; there are no gestures, no frames and no approval mode (`approval: false` always).
- `view.*` is `unsupported`. (Snapshots need the editor's renderer, which the server crate doesn't link.)
- Everything else is the same handler, results included.
- Nothing is saved unless `project.save` is called, or the server runs with `--save-on-write` (it then saves whenever `rev` moved and no batch is open). `worlds-mcp` calls `project.save` after every tool call that writes.
- `selection` and `approval` events never fire; `changed` fires for the client's own writes.
- The problems list is never recomputed on its own, after an edit, a save or a reload: `problems` is sent only when `project.validate` runs.
- Bakes are made when a read needs them (`project.bake`; for the effective grid of a derived layer in `level.get`, `grid.get_cells` and `rules.regions`, only its writer and the writers that one depends on) and kept up to date as the editor keeps its own: after every mutation each baked level is updated incrementally (`BakeCache::apply_changes`: only the layers and cells the changes reach); a definition rename or a project setting change drops them all, to be baked again when next read. `rules.preview` always bakes afresh.

## Testing

`crates/worlds-server/tests/docs.rs` checks this page. Every ` ```json ` and ` ```json norun ` block must parse. A ` ```json ` block holding a request (an object with `jsonrpc`, `method` and `id`) is replayed, in document order, over one WebSocket connection to a headless server on a fresh copy of `mini.worlds`; the next block is its expected response. The test first writes opaque placeholder images for the fixture's tilesets (`art/castle_main.png` 512x512, `art/ssw_set5.png` 268x268, so atlases are 32x32 and 8x8 tiles) and copies the game fixture (`tests/fixtures/game/`: `game.worlds` and its `assets/`) next to the project, so `../game.worlds` is a second project to import from. The `session.hello` example is sent with the server's real token. For each replayed request (notifications that arrive meanwhile are skipped): an error response must come back as an error with the same `code`, and for `command_error` and `needs_confirm` the same `data.kind` or `data.reason`; a result must come back as a result, and each of `rev`, `status`, `label`, `id`, `entries`, `steps` and `changes` that the example has must equal the real one exactly (as JSON values), as must the `text` of a `level.get` (the text view). Other result keys are checked for presence only, so values that depend on bakes, rendering or the clock can stay illustrative. Blocks that aren't requests or responses (notifications, standalone objects) are only parsed. `norun` blocks (editor-only and approval-mode exchanges) are parsed but not replayed.
