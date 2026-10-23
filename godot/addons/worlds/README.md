# Worlds for Godot

A Godot 4.3+ addon that turns a Worlds export (`worlds-cli bake`) into
scenes. Copy `addons/worlds/` into your project and enable **Worlds** under
Project Settings > Plugins.

## Importing

Keep the export and your tileset images inside the Godot project (say
`res://game.worlds/export/` and `res://assets/`). Then pick
**Project > Tools > Import Worlds Export...** and choose the export's
`world.json`. **Reimport Worlds Export** runs it again on the same file;
do that after each bake.

It writes, under `res://worlds/` by default:

| File | What |
| --- | --- |
| `levels/<level>.tscn` | One scene per level |
| `<world>.tscn` | Each world, instancing its level scenes at their world positions |
| `tilesets/<tileset>.tres` | One TileSet per tileset, shared by every level |

Every import rewrites these, so don't edit them. Instance a level scene
inside a scene of your own and build on that. Reimporting an unchanged
export leaves the files untouched, and level scenes for levels that no
longer exist are removed.

Settings, under Project Settings > `worlds/import` (turn on Advanced
Settings to see them):

| Setting | Default | |
| --- | --- | --- |
| `world_json` | | The export to reimport (set by the file dialog) |
| `output_dir` | `res://worlds` | Where the scenes go |
| `entity_scene_dir` | | A folder of entity scenes: an entity of type `door` instances `door.tscn` from it |
| `collision` | on | Build collision shapes from the collision layer |

## What a level scene holds

```
crevasse (Node2D at the level's top-left, in world pixels)
├── Background      ColorRect in the level's color, when it has one
├── Tiles
│   ├── DecorBack   one Node2D per tile layer, bottom first, named after it
│   │   └── tiles#0 TileMapLayer(s)
│   └── MainTiling
├── Collision       StaticBody2D: a rectangle per run of blocking cells
└── Entities
    ├── doorcrev22  your door.tscn, or a Marker2D at the entity's pivot
    └── ...
```

- **Tiles** land on their exact pixel, with flips. Tiles that don't line up
  with their tileset's grid (a 16 px offset, say) or that stack on an
  occupied cell go into extra TileMapLayers named `<tileset>+<x>+<y>#<n>`,
  offset to match, in the export's draw order. A layer's opacity becomes
  its node's `modulate.a`.
- **Collision** covers every cell of the project's `collision_layer` whose
  value is in `blocking_values`, with rows merged into rectangles. Other
  int grids (and derived ones) are in the level's `worlds_int_grids`
  metadata.
- **Entities** are named by their id. Each field is copied onto a property
  of the same name when the scene has one (`@export var locked := false`).
  A scene can also define `func _worlds_apply(entity: Dictionary)` to read
  the whole entity. During an editor import that only runs if the script
  is a `@tool` script.
- **Metadata**: levels carry `worlds_slug`, `worlds_id`, `worlds_name`,
  `worlds_fields`, `worlds_fields_set`, `worlds_tags`, `worlds_size_px`,
  `worlds_touching`, `worlds_open_edges`, `worlds_int_grids` and
  `worlds_notes`. Entities carry `worlds_id`, `worlds_type`,
  `worlds_fields`, `worlds_fields_set` and `worlds_size`. Each world scene
  has its world notes in `worlds_notes`. Fields and notes are as in the
  export (`docs/format.md`); notes are in world pixels.

## At runtime

`WorldsBuilder` builds the same nodes without saving anything, for example
to load levels from an export shipped with the game:

```gdscript
var b := WorldsBuilder.open("res://game.worlds/export")
b.entity_scene_dir = "res://entities"
add_child(b.build_level("crevasse"))
for problem in b.errors:
    push_warning(problem)
```

`build_world()` builds every level of a world under one node.

## Checking an export

`worlds-cli check-export <export dir>` checks an export against the format
without Godot. The Worlds repo's `godot/tests/run.sh` runs this addon
headless against every fixture export there, comparing each tile, collision
cell and entity with the export.
