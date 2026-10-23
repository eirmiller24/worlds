# worlds-server

The Worlds live API: JSON-RPC 2.0 over a localhost WebSocket, so agents and tools can read and edit the open project while you watch. `docs/api.md` is the wire contract (every example in it is replayed by `tests/docs.rs`); this README is about embedding the server.

## Layers

| Piece | What it is | Threads |
| --- | --- | --- |
| `Handler` (`src/handler/`) | Runs one request against a document a `Host` lends it. Every method, revisions, `expect_rev`, `confirm`, batches, undo/redo, approval batches, subscriptions and events. Produces `Out` items (notifications, approval decisions) for the caller to deliver | none |
| `Server::start` → `ServerHandle` (`server.rs`, `transport.rs`, `queue.rs`, `discovery.rs`) | Takes `.worlds/server.lock` (held while it runs, so two starts can't both pass the liveness check), binds `127.0.0.1:0`, writes `.worlds/server.json` (0600, temp file + rename), does the `session.hello` handshake (constant-time token check, `Origin` refused, 5 s timeout), parses requests into one queue per connection. The owner of the document calls `drain` | a 1-worker tokio runtime |
| `headless` (`headless.rs`) | `HeadlessHost` owns a project loaded from disk, its history and a `BakeCache` it keeps current after every mutation as the editor does (`BakeCache::apply_changes` per cached level). `Session` calls the handler in-process (worlds-mcp's fallback, the handler tests). `run` serves it over the socket from its own thread; `worlds-server <dir> [--save-on-write]` is that as a binary | `run`: one drain thread |

Module map: `rpc.rs` (framing, error codes), `describe.rs` (`project.describe`), `approval.rs` (`PendingBatch`, accept/reject), `handler/{project,defs,level,grid,tiles,entity,notes,rules,history,view,events}.rs` (one method group each), `handler/write.rs` (the write path), `handler/params.rs` (parsing and lookups).

## The seam: hosting the server in the editor

The editor's UI thread owns the document; requests wait in the queues until it drains them, once per frame, and never during a gesture. That keeps one writer and no locks on the document.

```rust
use worlds_server::{Doc, Host, Server, ServerConfig, ServerHandle, UserChange, ChangeKind};

struct EditorHost<'a> { doc: &'a mut Document, editing: &'a mut Editing, view: &'a mut ViewState }

impl Host for EditorHost<'_> {
    fn doc(&mut self) -> Doc<'_> {
        Doc { project: &mut self.doc.project, history: self.editing.history_mut(),
              bakes: self.editing.cache_mut(), images: &self.doc.images, dir: &self.doc.dir }
    }
    // Same path as the editor's own edits: invalidate chunks, rebake.
    fn applied(&mut self, changes: &[Change]) { self.editing.after_changes(self.doc, self.view, changes) }
    fn reload(&mut self) -> Result<(), RpcError> { /* reload the Document from disk */ }
    // Optional: save (default: save_dirty + export_on_save), problems, focus, select, snapshot.
}

// On project open (a ServerError::AlreadyRunning means: warn, run without a server).
let ctx = egui_ctx.clone();
let mut server: ServerHandle = Server::start(ServerConfig {
    project_dir: dir.clone(), project_name: project.name.clone(), headless: false,
    wake: Some(Arc::new(move || ctx.request_repaint())),
})?;

// Every frame, after tool events, when no gesture is in progress:
if !editing.in_gesture() {
    for h in server.drain(&mut EditorHost { doc, editing, view }) {
        if let Some(label) = h.label { toasts.agent(&h.client, &label) }   // one-click undo
    }
}

// Before the user's own entry is recorded, and before an undo/redo:
server.flush_batches(editing.history_mut());
let changes = editing.history_mut().push(&mut doc.project, cmd)?;     // or end_gesture / undo
server.notify_changed(UserChange { kind: ChangeKind::Edit, label: &label, entry_origin: None, changes: &changes });

// Other events the editor reports:
server.notify_selection(Some("crevasse"), &entity_ids, &note_ids, &level_slugs);
server.notify_saved(&files);          // after the user saves
server.notify_problems(&problems);    // after the problems panel is recomputed
server.notify_reloaded();             // after a reload from disk (clear the history yourself)

// The agent indicator and approval mode:
server.clients();                     // name, version, session, connected since
server.last_batch();                  // label of the last agent entry
server.set_approval(true);            // toolbar toggle; sends `approval`
for p in server.pending() { /* draw p.commands applied to a scratch copy as the ghost */ }
server.apply_pending(&mut host, &id); // oldest first; one undo entry, rev + 1, the agent's request gets `applied`
server.reject_pending(&id);           // also rejects that client's later pending batches
```

Rules the editor must keep:

- **Drain only between gestures.** A request during a stroke waits; reads too, so nobody sees half a stroke.
- **`flush_batches` before recording a user entry or stepping the history.** An agent's batch collects its inverses itself and records them with `History::record` at `end_batch`; flushing first records the part so far, so the undo stack stays in the order things happened (the batch then ends with `entries: 2`).
- **`notify_changed` once per user entry, undo or redo step** (a gesture once, when it ends). It bumps `rev` and sends `changed` with origin `user`. An empty change list is a no-op.
- **`Host::applied` is the redraw path.** It runs synchronously after every mutation the server makes (writes, agent undo/redo, accepted batches, a dropped connection's revert), before the response goes out. Route it through the same code as the editor's own edits.
- **Keep `Doc::bakes` current.** Reads of derived layers (`level.get`, `grid.get_cells`, `rules.regions`) and `project.bake` take effective grids and bakes from it, baking a level that isn't cached; a stale entry would be served as is.
- **A batch's mode is fixed when it begins**: a batch begun with approval off applies as it goes even if approval is turned on mid-batch.

`ServerHandle::drain` also reverts the unrecorded part of a closed connection's open batch and ends batches idle for 60 s. Dropping the handle (or `stop`) closes connections and removes `server.json` if it still holds this server's pid and token.

## Headless

```rust
// In-process, no runtime (worlds-mcp without an editor):
let mut s = worlds_server::headless::Session::open("mygame.worlds")?.client("claude-code");
s.call("grid.fill_rect", json!({ "level": "crevasse", "layer": "collision", "rect": [2, 2, 3, 1], "value": "water" }))?;
s.call("project.save", json!({}))?;

// Over the socket, same discovery file as the editor:
let server = worlds_server::headless::run("mygame.worlds", Options { save_on_write: true })?;
```

`worlds-server <project dir> [--save-on-write]` prints `{"port", "pid", "project"}` as one JSON line once listening and runs until SIGINT or SIGTERM.

## Tests

- `tests/docs.rs`: every JSON block of `docs/api.md` parses; every request is replayed in order over one WebSocket to a headless server on a fresh copy of `mini.worlds` (with placeholder tileset images), checking `rev`, `status`, `label`, `id`, `entries`, `steps`, `changes` (and a `level.get`'s text view) exactly and other keys for presence.
- `tests/fields.rs`: `defs.*` of the field kinds (`entity_field`, `level_field`): `defs.get` shapes, `entity` checks, `confirm` on a retype (type, array flag, enum source) while values are stored and on every remove, none for moves, renames and new fields, `duplicate_field` errors, `describe`'s `color`, `icon` and `limit`.
- `tests/notes.rs`: annotations: `notes.list` filters (`kind`, `kinds`, `group`, `export`), the locked rule (`needs_confirm` for moving or removing a locked note, never for text, tags, status or `locked`), `notes.move` per kind as one entry, guide groups, `level.update { tags }`, `describe` counting guides apart, `changed.files`, and `view.select` / `view.focus { note }` / the `selection` event against a stand-in editor host.
- Derived layers and region rules: `grid.get_cells` effective vs `stored: true`, `"!"` and `null` through every grid write on a derived layer (undo byte-exact) and `"!"` refused on others, `rules.regions` (bboxes, areas, edge runs, `along`, picture, errors), `rules.preview` of a region rule (regions, `rect`, truncation at 1000) and of an occupancy rule (`occupied`), `rules.list` `pass`/`kind`/`write`, `project.describe` `derived`/`writer`/`passes`, `level.get` derived rows (and the overrides-only fallback when the writer can't bake), `project.bake` warnings, and the headless bake cache staying equal to a fresh bake through strokes, undo, rule edits and a rename.
- `tests/rpc.rs`: the handler in-process: stale `expect_rev`, `confirm`, batches as one entry, a user edit splitting a batch, a dropped connection's revert, `cancel_batch`, undo stopping at other clients' entries, events, text blocks, `entity.find`, `describe`, every error code, idle batches, approval batches built on their scratch copy and accepted oldest first, `grid.line` bounds.
- `tests/ws.rs`: real sockets: discovery file, bad token, `Origin` and path refusal, framing errors, the queue across connections, approval mode (blocked then `applied`, for a write and for `end_batch`; `rejected`; `queued` then a `batch` event), the project lock, `--save-on-write`.
