# Importer-contract fixtures

Small, complete game exports that the `godot-rooms` importer contract
(`check-export --profile godot-rooms`) accepts or refuses.
`crates/worlds-cli/tests/check_export.rs` runs
`worlds-cli check-export <case> --json` on each one. A game-side importer
can run the same cases (point it at this directory) and must reach the same
verdict and warnings.

The contract itself is written down in [`docs/format.md`](../../../../../docs/format.md#the-godot-rooms-profile).

## A case

Each directory is one export, self-contained (nothing outside it is read):

```
<case>/
  world.json        the index (format 1), one world `main`
  enums.json        a small game's enums
  levels/room.json  one 10x6 room on the 32 px grid; tile layers bottom to
                    top decor_back, main_tiling, decor_front (the order the
                    importer draws them, so no layer-order warning)
  tiles.png         128x64 RGBA atlas: 16x16 blocks of distinct colors, each
                    block's top-left pixel white (so flips show)
  expect.json       the verdict
```

Tileset images are named relative to the case directory, as every export
path is relative to its export directory. The atlas is sliced three ways:
`tiles` (32x32, 4x2 tiles), `tiles16` (16x16, 8x4) and `tiles64x32` (2x2).

`expect.json`:

```json
{ "accept": true, "error_contains": "", "warning_contains": "nothere22" }
```

- `accept`: whether the import goes through (`check-export` exits 0; an
  importer writes the room). Warnings never refuse.
- `error_contains`: for a refused case, a substring of one of
  `check-export`'s problem messages (empty when accepted). An importer only
  has to agree on the verdict; its messages are its own.
- `warning_contains` (optional): a substring that one warning carries (an
  id, or the layer-order sentence), the part any wording shares. When it is
  absent, the case has no warnings.

## Cases

| Case | Verdict | What |
| --- | --- | --- |
| `clean_minimal` | accept | Collision, skin (`main_tiling`) on the walls, one DecorBack tile, a room entry |
| `two_auto_layers` | accept | A second auto layer `pillars` (below the skin) with on-grid tiles |
| `offgrid_pillars` | accept | `pillars` tiles at x = 112 (x mod 32 = 16): exact placement through a sub-layer |
| `decor_16px` | accept | `decor_back` on a 16 px grid with 16x16 tiles, some at 16 mod 32 |
| `all_flips` | accept | `decor_back` tiles with flips 0..7 (4..7 transposed, square tiles) |
| `transposed_nonsquare` | refuse | A 64x32 tile with flip 4 |
| `flip_out_of_range` | refuse | Flip 8 |
| `skin_offgrid` | refuse | A `main_tiling` tile at x = 48 (16 off the grid) |
| `skin_16px` | refuse | A 16x16 tile on `main_tiling` |
| `skin_stacked` | refuse | Two `main_tiling` tiles on one cell |
| `unknown_tiles_layer` | refuse | A tiles layer `trim` (not `decor_back*`/`decor_front*`) with a tile |
| `empty_unknown_tiles_layer` | accept | The same layer `trim`, empty: nothing to place, skipped |
| `reserved_layer_name` | refuse | An auto layer whose name is `Objects` |
| `reserved_breakables` | refuse | An auto layer whose name is `Breakables` (a node `Room` builds) |
| `empty_layer_name` | refuse | `decor_back`'s name is empty |
| `layer_name_collision` | refuse | Tiles layers `Back.Art` and `Back:Art`: both the node `Back_Art` |
| `layer_order` | accept, warning | `decor_front` listed below `main_tiling`: Worlds draws the skin over it, the importer the reverse |
| `project_grid_16` | refuse | Project grid 16 px (rooms need 32) |
| `no_collision_layer` | refuse | No `collision_layer` (nor `blocking_values`, `open_edges`) |
| `derived_int_grid` | accept | A `derived: true` int grid next to collision (never collision) |
| `annotations` | accept | Pins (world and level scope), a path linking them, a region, a level tag: the profile has no annotation rules |
| `dangling_note_link` | accept, warning | A drawing whose `note:` link names no exported annotation |

## Regenerating

`python3 gen.py` rewrites every case from one base export plus a per-case
change (standard library only). Add a case there, run it, and add its row
above; the Rust test picks up every directory with an `expect.json`.
