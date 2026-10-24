# Export fixtures

Small, complete exports that `worlds-cli check-export` accepts or refuses.
`crates/worlds-cli/tests/check_export.rs` runs
`worlds-cli check-export <case> --json` on each one, and
`godot/tests/run.sh` imports each one with the Godot addon. Any other
reader of the export format can run the same cases (point it at this
directory) and check it reaches the same verdict.

The format itself is written down in [`docs/format.md`](../../../../../docs/format.md#checking-an-export).

## A case

Each directory is one export, self-contained (nothing outside it is read):

```
<case>/
  world.json        the index (format 1), one world `main`
  enums.json        a few enums
  levels/room.json  one 10x6 level on a 32 px grid; tile layers bottom to
                    top back (tiles), walls (auto), front (tiles, empty)
  levels/annex.json (the `shaped_*` cases) a second, 4x3 level
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

- `accept`: whether the export is valid (`check-export` exits 0).
  Warnings never refuse.
- `error_contains`: for a refused case, a substring of one of
  `check-export`'s problem messages (empty when accepted). Another reader
  only has to agree on the verdict; its messages are its own.
- `warning_contains` (optional): a substring that one warning carries (an
  id), the part any wording shares. When it is absent, the case has no
  warnings.

## Cases

| Case | Verdict | What |
| --- | --- | --- |
| `clean_minimal` | accept | Collision, autotiled `walls`, one `back` tile, one entity |
| `two_auto_layers` | accept | A second auto layer `pillars` (below `walls`) with on-grid tiles |
| `offgrid_auto_tiles` | accept | `pillars` tiles at x = 112 (x mod 32 = 16): off the level grid |
| `tiles_16px` | accept | `back` on a 16 px grid with 16x16 tiles, some at 16 mod 32 |
| `all_flips` | accept | `back` tiles with flips 0..7 (4..7 transposed, square tiles) |
| `transposed_nonsquare` | accept | A 64x32 tile with flip 4 (its box is 32x64) |
| `flip_out_of_range` | refuse | Flip 8 |
| `no_collision_layer` | accept | No `collision_layer` (nor `blocking_values`, `open_edges`) |
| `derived_int_grid` | accept | A `derived: true` int grid next to collision |
| `annotations` | accept | Pins (world and level scope), a path linking them, a region, a level tag |
| `dangling_note_link` | accept, warning | A drawing whose `note:` link names no exported annotation |
| `shaped_level` | accept | `room` is an L (`void` `[[6, 0, 4, 3]]`, its `outline`, walls closing around the inside corner), a second level `annex` in the notch: `touching` with `at` both ways, `open_edges` runs `[first, end, at]` |
| `shaped_wrong_outline` | refuse | `shaped_level` with an `outline` that doesn't follow the void |
| `shaped_wall_in_void` | refuse | `shaped_level` with a wall glyph in a void cell |

## Regenerating

`python3 gen.py` rewrites every case from one base export plus a per-case
change (standard library only). Add a case there, run it, and add its row
above; the Rust test picks up every directory with an `expect.json`.
