# Importing from another Worlds project

> The import is one way and one batch: it reads the other project and changes only this one. Undo takes it back in one step (files it copied stay; see [Files](#files)).

```
worlds-cli import-project first_game.worlds --into sequel.worlds --defs-only
```

Brings part of another Worlds project into this one: definitions (layers with their rule sets, tilesets, entity types, enums, macros, level fields, project settings), whole worlds, or single levels with everything they need. The headline use is starting a sequel from a finished game: one *definitions only* import into a fresh project (`worlds-cli new`), then design levels.

The same import is the API's `project.import_plan` and `project.import` ([api.md](api.md#import-from-another-project)), MCP's `import_project` ([mcp.md](mcp.md#import_project)) and the editor's **Import…** (toolbar, or "Import from Worlds project…" in the command palette; [editor README](../crates/worlds-editor/README.md#import-from-worlds-project)). All of them go through `worlds_core::merge`.

In code: `worlds_core::merge::plan(&src, &dst, &selection, &policy)` returns a `Plan` and changes nothing; `plan.command()` is the import as one `batch` labelled `Import from <name>`, and `merge::copy_assets(&plan, src_dir, dst_dir)` copies its files (do that first).

## The plan comes first

Nothing changes until the plan is applied, and the plan says everything the import will do:

- every **item** with its action: `add` (new here), `same` (here already and identical: nothing to do), `skip` (a collision, ours kept), `overwrite` (a collision, theirs taken), `rename` (a collision, theirs taken under a new slug, `to`), or `refused` (its commands would be refused, `reason` says why; nothing of it goes in);
- every **dependency** pulled in: an item nobody selected carries `required_by` (`level:crevasse`);
- every **instance id** re-rolled (`ids`);
- every **reference dropped** (`dropped`) and anything else worth knowing (`problems`);
- every **file** to copy (`assets`);
- a summary: `12 definitions, 3 levels, 2 renamed, 1 skipped` (then `N same`, `N refused` and `N files` when there are any).

`worlds-cli import-project ... --dry-run` prints it (as JSON with `--json`, the shape the API returns):

```
Import from Mini (12 definitions, 1 level, 0 renamed, 0 skipped)
Tilesets:
  add       castle_main [needed by level:crevasse]
...
Worlds:
  add       mini
Levels:
  add       crevasse (into world mini)
dropped: note notecrev22: link note:pinstatue2 doesn't resolve here; dropped
```

Applying is one batch of existing commands: `set_tileset_def`, `set_enum_def`, `set_macro`, `set_layer_def` (with its `rules` for a new auto layer; `set_rule_group`, `set_stamp`, `remove_rule_group` and `remove_stamp` when an auto layer is overwritten), `set_level_field_def`, `set_entity_def`, `set_project_setting`, `create_world`, `set_guide_group`, `create_level`, `replace_level` and `add_note`, in that order. So undo is one entry, and the live API needs nothing new to apply it. Each item's commands are tried on a copy of this project while the plan is made, so the batch applies to the project it was planned against; the API refuses a plan once the project changed since (`stale_rev`: plan again).

## What to import

| Selection | What it takes |
| --- | --- |
| `--defs-only` (`preset: "defs_only"`) | every definition and project setting; no levels |
| `--all` (`preset: "everything"`) | that, plus every world with its levels, guide groups and world notes |
| `--levels a,b` (`levels`) | those levels and what they need |
| `--worlds w` (`worlds`) | those worlds: their levels, guide groups and world notes |
| `tilesets`, `enums`, `patterns`, `legends`, `templates`, `layers`, `level_fields`, `entities`, `settings` (API, MCP, editor) | those definitions and what they need |

What an item needs comes along:

- a **level**: the layers it has content or rule state on, the auto layers drawing from those, the entity types it places, the level fields it sets, and the tilesets its tiles, rule state and tile fields name;
- a **layer**: a tiles layer's tileset; an auto layer's source and the grids its rules read or write, its rule set's tilesets and macros, and the level fields its groups require; a derived layer's writer;
- an **entity type** or **level field**: the enums its fields use, the tilesets of its icon and tile fields, the entity types its `entity_ref` fields allow;
- an **enum**: the tilesets of its value icons; a **template**: the patterns, legends and tilesets it uses;
- the `collision_layer` setting: that layer.

The project settings an import carries are `grid_size`, `bg_color`, `collision_layer`, `blocking_values` and `default_level_size` (only those the source sets). The project's name, seed and `export_on_save` stay this project's.

## Collisions

A collision is a slug this project already has (definitions by kind; levels across all worlds; guide groups in the world they go into) with a different definition. Identical ones are `same` and skipped silently. For the rest, `--on-conflict` (`policy.on_conflict`, default `skip`) decides, and `policy.conflicts` overrides it per item by `"<kind>:<slug>"` (`"layer:collision"`, `"level:crevasse"`, `"asset:tiles.png"`; the editor has a menu on each collision):

| Policy | Does |
| --- | --- |
| `skip` | keeps ours. What the import brings that refers to it now refers to ours |
| `overwrite` | takes theirs, through the same command an edit would use, so it is refused where that command is: `set_layer_def` won't drop a value cells still hold, change the kind or grid of a layer with content, or switch `derived` on a layer with content. A refused overwrite is reported (`refused`, "overwrite refused: ...; kept ours") and the rest goes in. An overwritten auto layer takes their whole rule set. An overwritten level is replaced in place: its world and position stay ours, the rest is theirs |
| `rename` | takes theirs as `<slug>_2` (`_3`, ... until free in both projects), and every reference inside the imported set is rewritten |

`rename` works by renaming the item in a copy of the source with the source's own `rename_*` command (`rename_layer`, `rename_entity_type`, `rename_tileset`, `rename_enum`, `rename_macro`, `rename_level_field`, `rename_level`), which rewrites every reference to it there: instances' `type`, `allowed_types`, tile placements, rule outputs, `level:` links and the rest, exactly as an edit would. The import then reads from that copy. A rename can make another item differ from ours (a `floor_button` whose `allowed_types` named the renamed `locked_door`); that one is then a collision too, and the policy decides again. A setting can't be renamed: `rename` keeps ours.

One special case: a project with no levels yet takes the source's `grid_size` (`overwrite`, "this project has no levels yet") unless `policy.conflicts` names `setting:grid_size`, so a fresh project imported into gets the grid its levels will need.

## Instance ids and references

Level, entity and note ids are kept unless this project already uses them; a colliding id is re-rolled to an id derived from the old one (`Id::derived`), so the same import of the same projects gives the same ids. Every reference inside the imported set follows the new ids: `entity_ref` fields (entities and level fields), `entity:` and `note:` links, connector ends.

A reference to something outside the imported set is kept when this project resolves it (a door's `exit_to` naming an entity id this project has, a `level:` link to a level of ours) and otherwise dropped and listed in `dropped`: an `entity_ref` becomes `null` (or leaves its array), a link is removed, and a connector whose end resolves to nothing is left out.

## Where levels go

| Placement | CLI | Where |
| --- | --- | --- |
| `new_world` (default) | `--new-world <slug>` (or nothing) | one new world, by default named after the source project (`game`, then `game_2` if that is taken; an explicit slug that is taken is refused) |
| `world` | `--world <slug>` | an existing world |
| `keep` | `--keep-worlds` | the world with the slug of each level's own source world, made when missing |

`--offset x,y` (`placement.offset`) adds world pixels to every imported level's position and every imported world note's geometry. Levels keep their own coordinates otherwise; guide groups keep their offset.

A level can only move between projects with the same grid size (level sizes and positions are in cells of it): a level from a project with another grid is `refused`, unless the import also takes `grid_size` into a project with no levels.

## Files

Tileset images and `path` field values of imported entities and levels are copied into this project under `art/<file name>`, and the imported tilesets and values point there. A file `art/` already has is `same` when its bytes are equal; otherwise the policy decides by file name (`asset:<name>`): `skip` uses ours, `overwrite` copies theirs over it, `rename` copies theirs as `<stem>_2.<ext>`. A file missing in the source is listed as missing and the reference is kept as it is.

Copying files isn't part of the batch: they are copied just before it is applied, and undoing the import leaves them (they are new files nothing refers to any more, or, for an `overwrite`, theirs). Old files are otherwise untouched: only what the batch writes changes.
