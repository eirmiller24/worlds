# worlds-core

The project document model, the file format (load and save), the edit commands with their undo history, validation, the game export and the JSON Schemas. Everything else (the rules engine, the CLI, the server, the editor) is built on it, and a program of your own can be too.

The crates aren't on crates.io; depend on them by path (or as a `git` dependency on this repository):

```toml
[dependencies]
worlds-core = { path = "../worlds/crates/worlds-core" }
worlds-rules = { path = "../worlds/crates/worlds-rules" }   # baking auto layers, for the export
worlds-server = { path = "../worlds/crates/worlds-server" } # the live API's methods, in-process
serde_json = "1"
```

## Load, edit, validate, save, export

```rust
use std::{collections::BTreeMap, path::Path};
use worlds_core::Command;

let dir = Path::new("mygame.worlds");
let mut project = worlds_core::load(dir)?;

// Every edit is a command: the JSON objects of docs/commands.md.
let cmd: Command = serde_json::from_value(serde_json::json!({
    "op": "fill_rect", "level": "crevasse", "layer": "collision",
    "rect": [2, 2, 3, 1], "value": "water" }))?;
let applied = project.apply(cmd)?;           // applied.inverse undoes it, applied.changes says what moved

for problem in worlds_core::validate(&project) {
    eprintln!("{problem}");                  // as `worlds-cli validate` prints them
}
worlds_core::save_dirty(&mut project, dir)?; // writes only the files the commands touched

// What `worlds-cli bake` does: bake every auto layer, write export/.
let images = worlds_rules::TilesetImages::new(dir, &project.tilesets);
let mut baked = BTreeMap::new();
for level in project.levels() {
    baked.insert(level.slug.clone(), worlds_rules::bake_for_export(&project, level, &images)?);
}
worlds_core::export::write(&project, dir, &dir.join("export"), &baked, true)?;
```

Where each piece is documented:

| Piece | Where |
| --- | --- |
| `load`, `save`, `save_all`, `save_dirty`, the files they read and write | [docs/format.md](../../docs/format.md); saving: [docs/commands.md](../../docs/commands.md#saving) |
| `Command`, `Project::apply`, `Applied`, `Change`, `CommandError` | [docs/commands.md](../../docs/commands.md) |
| `History` (undo, redo, gestures, `record`) | [docs/commands.md](../../docs/commands.md#undo-and-redo) |
| `validate`, `Problem` | [docs/format.md](../../docs/format.md#validation-reference) |
| `export::write`, `write_counted`, `render_files` | [docs/format.md](../../docs/format.md#export-generated) |
| `text::render_level` (the text view agents read) | [docs/format.md](../../docs/format.md#text-view) |
| `schema::write_all`, `schema::for_kind` | the JSON Schemas `worlds-cli schema` writes |
| Baking, `BakeCache`, rule previews and regions | [crates/worlds-rules/README.md](../worlds-rules/README.md) |

The model is plain data: you can also change `project` directly and save with `save_all` (which renders every file; `save_dirty` only knows about commands). Commands are the better route for anything a user might undo, and they refuse edits that would break the model.

## The live API without a socket

`worlds_server::headless::Session` runs the [live API](../../docs/api.md)'s methods in-process on a project loaded from disk, with its own history and bake cache. Results are the same JSON the socket returns:

```rust
let mut s = worlds_server::headless::Session::open("mygame.worlds")?.client("my-tool");
let rev = s.call("level.list", serde_json::json!({}))?["rev"].clone();
s.call("grid.fill_rect", serde_json::json!({ "level": "crevasse", "layer": "collision",
    "rect": [2, 2, 3, 1], "value": "water", "expect_rev": rev }))?;
s.call("project.save", serde_json::json!({}))?;
```

Don't write to a project's files while the editor has it open (`.worlds/editor.lock` is held, [docs/format.md](../../docs/format.md#directory-layout)): talk to the editor's server instead ([docs/api.md](../../docs/api.md#getting-started)).
