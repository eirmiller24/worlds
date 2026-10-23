# worlds-mcp

An MCP server (stdio, `rmcp`) that lets an agent such as Claude Code read and edit a Worlds project: 37 coarse tools (`describe_project`, `read_level`, `create_level`, `paint`, ...) and `worlds://` resources. [`docs/mcp.md`](../../docs/mcp.md) is the contract: tool names, descriptions and input schemas there are exactly what the binary publishes (`tests/docs.rs` checks it).

```
worlds-mcp [--project <dir>] [--headless] [--name <client>]
```

## Finding the project

`--project <dir>` (relative to the working directory) must hold `project.json`. Without it, worlds-mcp walks up from the working directory: the first directory holding `project.json` wins, else the first one with exactly one `*.worlds` child holding it (so starting in a game repo finds `mygame.worlds/`). Otherwise it exits with an error saying what it tried.

## Two modes

Before every tool call it reads `<project>/.worlds/server.json`:

- **Editor**: the file is live (pid running, port accepting, `project` is this directory). Calls go over the editor's WebSocket (`session.hello` with the token, client name from MCP `clientInfo.name`, sent with `initialize` or in each request's `_meta`, else `--name`, else `mcp`). Edits show on the canvas and are saved when the user saves. If the connection to a live editor fails, the call fails (nothing is written to disk behind the editor's back) and the next call tries again.
- **Headless**: otherwise. The live API's handler runs in-process on the project loaded from disk (`worlds_server::headless::Session`), and every writing call ends with `project.save`. Files changed on disk by someone else are reloaded at the start of the next call. `screenshot` runs `worlds --screenshot ... --snapshot` (the binary next to `worlds-mcp`, or `WORLDS_EDITOR_BIN`).

`--headless` forces the second and refuses to work while the editor has the project open. The mode and project are logged to stderr at startup; stdout is only the MCP channel.

Code map: `catalog.rs` (descriptions, schemas, instructions as constants, generated from `docs/mcp.md`: `WORLDS_WRITE_CATALOG=1 cargo test -p worlds-mcp --test docs catalog`), `backend.rs` (`Backend` trait, `EditorBackend`, `HeadlessBackend`, `Connector`), `engine.rs` (revs, `expect_rev` and the stale retry, batches, notices, results), `tools.rs` (one method per tool), `resources.rs`, `screenshot.rs`, `server.rs` (the rmcp `ServerHandler`; the engine runs on its own thread).

## Installing into Claude Code

Build `cargo build --release -p worlds-mcp -p worlds-editor`, then either

```
claude mcp add worlds -- /path/to/worlds/target/release/worlds-mcp --project mygame.worlds
```

or put [`mcp.json.example`](mcp.json.example) into your game repo as `.mcp.json`. Claude Code starts the server in the repo root, so `mygame.worlds` resolves there.

## Trying it by hand

One JSON-RPC message per line on stdin:

```
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"claude-code","version":"1"}}}' \
  '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"describe_project","arguments":{}}}' \
  | target/release/worlds-mcp --project crates/worlds-core/tests/fixtures/mini.worlds
```

Careful: in headless mode write tools save to that directory; try writes on a copy.

## Tests

- `tests/docs.rs`: the published tools and instructions equal `docs/mcp.md`; every JSON block parses; `mcp.json.example` is the doc's snippet.
- `tests/tools.rs`: every tool in-process, headless, on a temp copy of `mini.worlds` (files on disk after each call, undo, stale-rev retry, errors as tool errors, resources, reload after outside edits).
- `tests/notes.rs`: the annotation tools: `add_note`'s new properties, `list_notes` leaving guides out, `resolve_note` reopening, `update_note` (text, geometry, move, `confirm` for locked ones, one entry), `remove_note`, `set_guide_group` keeping what isn't given, `tag_levels` and `create_level` tags, the `world.json` resource.
- `tests/definitions.rs`: `edit_definitions` headless and over a socket: renames before sets, `confirm` passed through, a `needs_confirm` or other refusal mid-patch cancels the call (files on disk unchanged, no undo entry), level fields, enums and whole types, the same files in editor and headless mode.
- `tests/settings.rs`: project settings, rule state and replacing a level: `edit_definitions` `set_project` (one undo entry, after `set_layers` so a new layer can be the collision layer, a refusal cancelling the call), `edit_rules` `set_level_states` (set, `null` removes, refusals), `create_level` with `content`, and with `replace` (`confirm` naming what would go, id, background, fields and rule state kept, undo, a `read_level` json round trip changing nothing).
- `tests/editor.rs`: against `worlds_server::headless::run` through `server.json` (one history entry labelled `Agent: ...`, notices about other clients' edits, falling back when the server goes away or `server.json` is stale, a tool error and no disk write when a live editor refuses the connection), and against a stand-in editor in approval mode (a multi-write call waits for Apply; one that fails part-way submits nothing).
- Derived layers and region rules through the tools: `edit_definitions` `set_layers`/`remove_layers` (the `confirm` path, one undo), `edit_rules` with passes, a region rule writing a derived layer and an occupying stamp, `find_regions`, `preview_rule` (`pass`, `regions`, `write`, `occupied`, the effective picture), `paint` with `"!"` (and its refusal on collision), `read_level` derived grids, `bake_and_validate` warnings.
- `tests/stdio.rs`: the built binary over stdio (`initialize`, `tools/list`, `tools/call`, resources, the headless screenshot failure path, the client name reaching the editor's history through `initialize` and through request `_meta` after `server/discover`).
