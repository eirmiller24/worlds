//! The tool catalog and server instructions, exactly as `docs/mcp.md`
//! publishes them (`tests/docs.rs` checks that). Kept as constants rather
//! than derived with schemars so the published schemas are the documented
//! ones by construction. Generated from the page:
//! `WORLDS_WRITE_CATALOG=1 cargo test -p worlds-mcp --test docs catalog` writes
//! it again after the page changes.

/// The `instructions` `initialize` returns.
pub const INSTRUCTIONS: &str = r#"Worlds is a 2D tile level editor. A project is a set of levels (rooms) laid out in a world. Each level has the same layers, defined once for the project:
- int_grid layers: a grid where each cell holds a named value or is empty. The collision layer (values like solid, oneway, water) is the playable truth. You paint these with `paint`. A derived int_grid layer (marks such as where pillars go) is computed by an auto layer's rules instead; painting it sets hand overrides on top of what the rules compute.
- auto layers: tiles generated from an int_grid layer by rules. Never painted directly: change the source grid, or the rules with `edit_rules`.
- tiles layers: hand-placed decoration tiles taken from a tileset image by atlas column and row (`place_tiles`).
- entities layers: objects such as doors and buttons, each at a cell with typed fields (`place_entity`). Entity types, their fields and enums are edited with `edit_definitions`.
Notes (todo notes, pins, paths, regions) annotate a level or the whole world; guides are locked reference lines that levels are built along and snap to. Levels and annotations carry tags (e.g. north, east, south, west for the world's regions); list_notes and read_level show them.

Coordinates: a cell is [x, y], counted from 0 at the level's top-left corner, x to the right, y down. Each layer has its own cell size (grid_size, in pixels): a level 20 cells wide on a 16 px layer is 40 cells wide on an 8 px layer. Rects are [x, y, width, height] in the layer's cells. Note geometry is in pixels.

Start with describe_project (layers, values and glyphs, entity types and fields, tilesets, levels and sizes), then read_level before editing a level. Things are named by slugs (lowercase snake_case, e.g. "crevasse", "solid"); entities and notes by 10-character ids (e.g. "doorcrev22").

Each write tool call is one undo entry that the user can undo with one click; undo(n) takes back your own last n. If the Worlds editor is open, your edits show live on the user's canvas and are saved when they save; otherwise each edit is written to disk immediately."#;

/// One tool: name, description, input schema (JSON text).
pub struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    pub schema: &'static str,
}

/// Every tool, in the order of `docs/mcp.md`.
pub const TOOLS: &[ToolDef] = &[
    ToolDef {
        name: "describe_project",
        description: r#"Get the map of the open Worlds project: the project grid size; every level with its world position, size in cells and pixels, tags, and how much content each layer holds; every layer with its kind and cell size, and for int_grid layers each value's slug and glyph (the character that stands for it in read_level and paint), for a derived int_grid layer (one the rules compute) "derived": true and its writer (the auto layer whose rules compute it), for an auto layer the passes its rule groups use; entity types with their fields, defaults and how many exist; enums; tilesets with tile size and atlas size in tiles; note counts (guides are counted apart) and each world's guide groups. Call this first: the slugs, glyphs and sizes you need for every other tool are here. Returns JSON."#,
        schema: r#"{ "type": "object", "properties": {}, "additionalProperties": false }"#,
    },
    ToolDef {
        name: "read_level",
        description: r#"Read one level. The default text format shows each int_grid layer as a picture, one character per cell, with column numbers above and row numbers on the left, then the level's tiles, entities and open notes, one per line, then the world's notes, pins, paths and regions that lie over the level (in world pixels) and how many guides cross it. Example:

int_grid collision (20x5 cells of 16 px)
legend: . empty, # solid, f fortified, o oneway, ~ water, L ramp_l
  0         1
  01234567890123456789
0 ####################
1 #..................#
2 #.......oooo.......#
3 #L..~~~.......ff####
4 ####################

Cell [x, y] is the character in column x of row y: here [1, 3] is L (ramp_l) and [4, 3] is ~ (water). The two ruler lines are the tens digit (every tenth column) and the ones digit of each column number. Entities read "<id> <type> in <layer> at [x, y]" plus offset, size and fields when set; notes read "<id> <kind> open at [x, y] title ..." (note positions are pixels). The first line ends with the level's tags when it has any. Auto layers are listed as generated and not drawn. A derived int_grid layer (computed by an auto layer's rules, named in its header: "derived by <layer>") is drawn as its effective grid, what the rules computed with the hand-painted overrides applied; when the level has overrides, a second picture follows the "overrides:" line, holding only them: a value glyph is an override, ! a suppressed cell, . left to the rules. Use `layers` to show only some layers (entities and notes are always included). format "json" returns the level and its content as stored in the project files instead, plus derived: { <layer>: { writer, rows, overrides? } } with each derived layer's effective rows and its stored override rows."#,
        schema: r#"{ "type": "object",
  "properties": {
    "level": { "type": "string", "description": "Level slug, e.g. \"crevasse\"." },
    "layers": { "type": "array", "items": { "type": "string" }, "description": "Only these int_grid, tiles and auto layers (slugs). Default: all." },
    "format": { "type": "string", "enum": ["text", "json"], "default": "text" },
    "include_done_notes": { "type": "boolean", "default": false, "description": "Also list resolved notes." } },
  "required": ["level"], "additionalProperties": false }"#,
    },
    ToolDef {
        name: "create_level",
        description: r#"Create a new, empty level (a room) in the world, as one undo entry. slug names it: lowercase snake_case, unused by any other level (it is also the level's name in the export); name is its display name. size is [width, height] in cells of the project grid, at least [1, 1] (describe_project gives the grid size in pixels and every level's size and world position). world_px is the level's top-left corner in world pixels: a level at [x, y] covers x to x + width * grid_size and y to y + height * grid_size, so to put it beside an existing level, start where that one ends; levels should not overlap. depth orders levels that do overlap (higher is in front, default 0). fields sets level field values by field slug (the project's level_fields in describe_project; encoded as for place_entity); fields left out take their default. tags are words saying what the level is part of, as for tag_levels. world is needed only when the project has more than one world. Afterwards paint, place_tiles and place_entity fill it like any level. Returns the new level's id."#,
        schema: r#"{ "type": "object",
  "properties": {
    "slug": { "type": "string", "description": "New level slug, e.g. \"east_annex\"." },
    "name": { "type": "string", "description": "Display name, e.g. \"East annex\"." },
    "size": { "type": "array", "items": { "type": "integer", "minimum": 1 }, "minItems": 2, "maxItems": 2, "description": "[width, height] in cells of the project grid." },
    "world_px": { "type": "array", "items": { "type": "integer" }, "minItems": 2, "maxItems": 2, "description": "Top-left corner in world pixels." },
    "depth": { "type": "integer", "default": 0 },
    "fields": { "type": "object", "description": "Level field slug -> value." },
    "tags": { "type": "array", "items": { "type": "string" }, "description": "e.g. [\"east\"]." },
    "world": { "type": "string", "description": "World slug. Needed only when the project has more than one." },
    "label": { "type": "string", "maxLength": 60, "description": "A few words on what this edit is, shown in the editor's undo history." } },
  "required": ["slug", "name", "size", "world_px"], "additionalProperties": false }"#,
    },
    ToolDef {
        name: "paint",
        description: r######"Write cells of an int_grid layer (collision and the like). Values are value slugs from describe_project ("solid", "water"); null means empty. Give one or more of these shapes; they apply in this order, all as one undo entry:
- text_block: a picture to paste, in the same glyphs read_level shows. { "text": "#####\n#...#\n#####", "origin": [4, 1] } puts the first character of the first row on cell [4, 1], continues right along each row and down row by row. "." paints empty. A space or "*" leaves that cell unchanged, so you can paint irregular shapes; a short row leaves the rest of its line alone. legend (optional) maps extra glyphs to values for this call, e.g. { "W": "water", "_": null }.
- rect + value: fill [x, y, width, height] with value; with outline true, only its border.
- line + value: cells on the straight line from one cell to another, both ends included.
- cells: explicit [x, y, value] triples, e.g. [[3, 1, "solid"], [8, 2, null]].
- flood_at + value: the 4-connected region of cells holding the same value as that cell (like a paint bucket).
rect, line and flood_at need value given explicitly: leaving it out is an error, so write "value": null to make cells empty. Coordinates are cells of this layer's grid, [0, 0] at the level's top-left. Any cell outside the layer fails the whole call and nothing changes. Auto layers regenerate from the result on their own. Paint the collision layer to shape a room; decoration goes on tiles layers (place_tiles).
A derived int_grid layer (describe_project says "derived": true) holds what an auto layer's rules compute; painting it sets overrides that win over the rules, cell by cell: a value slug (or its glyph) sets the cell to that value, "!" suppresses it (empty, whatever the rules say), and null (or "." in a text_block) removes the override, giving the cell back to the rules; " " and "*" leave a cell as it is. flood_at floods cells with the same override. "!" on a layer that isn't derived is an error."######,
        schema: r#"{ "type": "object",
  "properties": {
    "level": { "type": "string", "description": "Level slug." },
    "layer": { "type": "string", "description": "int_grid layer slug, e.g. \"collision\"." },
    "text_block": { "type": "object",
      "properties": {
        "text": { "type": "string", "description": "Rows of glyphs separated by \\n." },
        "origin": { "type": "array", "items": { "type": "integer", "minimum": 0 }, "minItems": 2, "maxItems": 2, "description": "Cell of the first character, [x, y]." },
        "legend": { "type": "object", "additionalProperties": { "type": ["string", "null"] }, "description": "Extra glyph -> value slug (or null for empty)." } },
      "required": ["text", "origin"], "additionalProperties": false },
    "rect": { "type": "array", "items": { "type": "integer", "minimum": 0 }, "minItems": 4, "maxItems": 4, "description": "[x, y, width, height] in cells; needs value." },
    "outline": { "type": "boolean", "default": false, "description": "With rect: paint only the border." },
    "line": { "type": "object",
      "properties": {
        "from": { "type": "array", "items": { "type": "integer", "minimum": 0 }, "minItems": 2, "maxItems": 2 },
        "to": { "type": "array", "items": { "type": "integer", "minimum": 0 }, "minItems": 2, "maxItems": 2 } },
      "required": ["from", "to"], "additionalProperties": false, "description": "Needs value." },
    "cells": { "type": "array", "description": "[x, y, value] triples; value null is empty (on a derived layer: back to the rules), \"!\" suppresses a derived layer's cell.",
      "items": { "type": "array", "prefixItems": [ { "type": "integer", "minimum": 0 }, { "type": "integer", "minimum": 0 }, { "type": ["string", "null"] } ], "minItems": 3, "maxItems": 3 } },
    "flood_at": { "type": "array", "items": { "type": "integer", "minimum": 0 }, "minItems": 2, "maxItems": 2, "description": "Cell to flood fill from; needs value." },
    "value": { "type": ["string", "null"], "description": "Value slug for rect, line and flood_at; null is empty (on a derived layer: back to the rules); \"!\" suppresses (derived layers only)." },
    "label": { "type": "string", "maxLength": 60, "description": "A few words on what this edit is, shown in the editor's undo history, e.g. \"east wall\"." } },
  "required": ["level", "layer"], "additionalProperties": false }"#,
    },
    ToolDef {
        name: "place_tiles",
        description: r#"Place decoration tiles on a tiles layer. A tile is picked from a tileset image by its atlas column and row (describe_project gives each tileset's tile size and atlas size in tiles). placements is a list of [x, y, col, row] (or [x, y, col, row, flip] with flip "x", "y" or "xy", or "t", "xt", "yt" or "xyt" to transpose first: "xt" is a quarter turn clockwise, "yt" counterclockwise): put atlas tile [col, row] on cell [x, y] of this layer. Placing on a cell replaces what was there; several placements on the same cell in one call stack, first one at the bottom. stamp lays a block of the atlas down whole: { "at": [x, y], "rect": [col, row, width, height] } puts atlas tile [col + i, row + j] at cell [x + i, y + j] (stepping by the tile size when tiles are bigger than the layer's cells). Cells are this layer's own grid: on an 8 px layer in a 16 px project there are twice as many cells per axis as read_level shows for collision. tileset defaults to the layer's own tileset."#,
        schema: r#"{ "type": "object",
  "properties": {
    "level": { "type": "string", "description": "Level slug." },
    "layer": { "type": "string", "description": "tiles layer slug." },
    "tileset": { "type": "string", "description": "Tileset slug. Default: the layer's tileset." },
    "placements": { "type": "array", "description": "[x, y, col, row] or [x, y, col, row, flip].",
      "items": { "type": "array", "prefixItems": [ { "type": "integer", "minimum": 0 }, { "type": "integer", "minimum": 0 }, { "type": "integer", "minimum": 0 }, { "type": "integer", "minimum": 0 }, { "type": "string", "enum": ["x", "y", "xy", "t", "xt", "yt", "xyt"] } ], "minItems": 4, "maxItems": 5 } },
    "stamp": { "type": "object",
      "properties": {
        "at": { "type": "array", "items": { "type": "integer", "minimum": 0 }, "minItems": 2, "maxItems": 2 },
        "rect": { "type": "array", "items": { "type": "integer", "minimum": 0 }, "minItems": 4, "maxItems": 4, "description": "[col, row, width, height] in the atlas." } },
      "required": ["at", "rect"], "additionalProperties": false },
    "label": { "type": "string", "maxLength": 60, "description": "A few words on what this edit is, shown in the editor's undo history." } },
  "required": ["level", "layer"], "additionalProperties": false }"#,
    },
    ToolDef {
        name: "clear_tiles",
        description: r#"Remove every tile whose cell is inside rect ([x, y, width, height] in this tiles layer's cells) on a tiles layer. Auto layers can't be cleared this way: change their source grid or rules instead."#,
        schema: r#"{ "type": "object",
  "properties": {
    "level": { "type": "string", "description": "Level slug." },
    "layer": { "type": "string", "description": "tiles layer slug." },
    "rect": { "type": "array", "items": { "type": "integer", "minimum": 0 }, "minItems": 4, "maxItems": 4 },
    "label": { "type": "string", "maxLength": 60, "description": "A few words on what this edit is, shown in the editor's undo history." } },
  "required": ["level", "layer", "rect"], "additionalProperties": false }"#,
    },
    ToolDef {
        name: "place_entity",
        description: r##"Place an entity (a door, a button, a spawn point...) of one of the project's entity types at a cell. cell is in the entities layer's grid; the entity's pivot (often its bottom center) sits in that cell, and offset_px nudges it by pixels. fields sets field values by field slug (types and defaults are in describe_project; fields left out take their default). Field values by type: int and float numbers; bool; string, text and path strings; color "#rrggbb"; point [x, y] in cells; tile { "tileset": slug, "tile": [col, row] }; entity_ref another entity's id; enum one of its value slugs; array fields a JSON array of those. Returns the new entity's id."##,
        schema: r#"{ "type": "object",
  "properties": {
    "level": { "type": "string", "description": "Level slug." },
    "type": { "type": "string", "description": "Entity type slug, e.g. \"locked_door\"." },
    "cell": { "type": "array", "items": { "type": "integer" }, "minItems": 2, "maxItems": 2 },
    "layer": { "type": "string", "description": "Entities layer slug. Needed only when the project has more than one." },
    "offset_px": { "type": "array", "items": { "type": "integer" }, "minItems": 2, "maxItems": 2 },
    "fields": { "type": "object", "description": "Field slug -> value." },
    "label": { "type": "string", "maxLength": 60, "description": "A few words on what this edit is, shown in the editor's undo history." } },
  "required": ["level", "type", "cell"], "additionalProperties": false }"#,
    },
    ToolDef {
        name: "update_entity",
        description: r##"Change an entity, found by id, in one undo entry. patch may hold: cell and offset_px to move it (within its level); size [width, height] in cells of the project grid for resizable types, or null for the type's size; fields to set values by field slug (encoded as for place_entity: color "#rrggbb", point [x, y], tile { "tileset", "tile": [col, row] }, entity_ref an id, enum a value slug, arrays as JSON arrays); a value given replaces the stored value whole, arrays included, nothing is merged; unset_fields to remove stored values so the defaults apply again."##,
        schema: r#"{ "type": "object",
  "properties": {
    "id": { "type": "string", "description": "Entity id, e.g. \"doorcrev22\"." },
    "patch": { "type": "object",
      "properties": {
        "cell": { "type": "array", "items": { "type": "integer" }, "minItems": 2, "maxItems": 2 },
        "offset_px": { "type": "array", "items": { "type": "integer" }, "minItems": 2, "maxItems": 2 },
        "size": { "type": ["array", "null"], "items": { "type": "integer", "minimum": 1 }, "minItems": 2, "maxItems": 2 },
        "fields": { "type": "object", "description": "Field slug -> new value." },
        "unset_fields": { "type": "array", "items": { "type": "string" } } },
      "additionalProperties": false },
    "label": { "type": "string", "maxLength": 60, "description": "A few words on what this edit is, shown in the editor's undo history." } },
  "required": ["id", "patch"], "additionalProperties": false }"#,
    },
    ToolDef {
        name: "remove_entity",
        description: r#"Remove entities by id, all in one undo entry."#,
        schema: r#"{ "type": "object",
  "properties": {
    "ids": { "type": "array", "items": { "type": "string" }, "minItems": 1 },
    "label": { "type": "string", "maxLength": 60, "description": "A few words on what this edit is, shown in the editor's undo history." } },
  "required": ["ids"], "additionalProperties": false }"#,
    },
    ToolDef {
        name: "find_entities",
        description: r#"Find entities across the project. Every filter given must match: type (entity type slug), tag (a tag of the entity's type, e.g. "puzzle"), fields (field slug -> value, compared with the effective value, defaults included), level, and rect ([x, y, width, height] in the entities layer's cells; needs level). With no filters, lists every entity. Returns each entity's id, type, level, layer, cell, offset, size and stored fields."#,
        schema: r#"{ "type": "object",
  "properties": {
    "type": { "type": "string" },
    "tag": { "type": "string" },
    "fields": { "type": "object" },
    "level": { "type": "string" },
    "rect": { "type": "array", "items": { "type": "integer", "minimum": 0 }, "minItems": 4, "maxItems": 4 } },
  "additionalProperties": false }"#,
    },
    ToolDef {
        name: "edit_definitions",
        description: r#"Change the project's definitions, all in one undo entry, all or nothing: layers (entries of layers.json), entity types (the kinds of object place_entity places, one file each), their fields, enums (named lists of values that enum fields pick from) and level fields (fields every level has). Read what is there first: the layers from worlds://defs/layers.json (format: worlds://schema/layers.json), an entity type from the resource worlds://defs/entities/<slug>.json (the format of a type and of a field: worlds://schema/entity.json), the enums from worlds://defs/enums.json, the level fields from worlds://project.json (level_fields); describe_project lists them all. A layer is { "slug", "name", "kind", ... }: kind int_grid ("values": [{ "slug", "name", "color", "glyph" }], "derived": true for a grid an auto layer's rules compute with write outputs), tiles ("tileset"), auto ("source": the int_grid layer its rules read; its rules are edited with edit_rules) or entities. A field is { "slug", "type", ... }: type is int, float, bool, string, text, color, enum ("enum": an enum's slug, or inline "values": [slugs]), point, entity_ref, path, tile or json; "array": true makes the value a list, "nullable": true allows null, "default" is the value an entity gets when it stores none, "doc" says what it is for.
patch holds any of these lists, applied in this order, so a later step can use a name an earlier one created or renamed (and must: a whole entity type in set_entity_types replaces the renamed one, so write it with the new names): rename_entity_types, rename_enums ([{ from, to }]), rename_enum_values ([{ enum, from, to }]), rename_entity_fields ([{ entity, from, to }]), rename_level_fields ([{ from, to }]) (a rename rewrites every stored value and reference); set_layers ([{ def, index? }]: a whole layers.json entry, replacing the layer with that slug or adding it at index in the layer list, default the end; index on an existing one moves it), set_enums (whole enums.json entries), set_entity_types (whole entity files; an existing slug is replaced, a new one created), set_entity_fields ([{ entity, def, index? }]: one field of a type, replacing the field with that slug or adding it at index, default the end; index on an existing field moves it), set_level_fields ([{ def, index? }], the same for level fields); then remove_entity_fields ([{ entity, field }]), remove_level_fields, remove_enums, remove_entity_types, remove_layers (slugs). Removing a field also removes the values entities (or levels) store for it; removing an entity type removes every entity of that type; removing a layer removes its content in every level and, for an auto layer, its rules. Every removal needs "confirm": true, and so do changing a field's type (its type, array flag, or which enum it uses) while entities or levels store a value for it, and taking a value out of an enum that something stores. Without it the call changes nothing and the error says what would be affected. Changing a definition never deletes stored values: run bake_and_validate afterwards to see values that no longer fit."#,
        schema: r#"{ "type": "object", "properties": {
  "patch": { "type": "object", "properties": {
    "rename_entity_types": { "type": "array", "items": { "type": "object", "properties": { "from": {"type":"string"}, "to": {"type":"string"} }, "required": ["from","to"], "additionalProperties": false } },
    "rename_enums":        { "type": "array", "items": { "type": "object", "properties": { "from": {"type":"string"}, "to": {"type":"string"} }, "required": ["from","to"], "additionalProperties": false } },
    "rename_enum_values":  { "type": "array", "items": { "type": "object", "properties": { "enum": {"type":"string"}, "from": {"type":"string"}, "to": {"type":"string"} }, "required": ["enum","from","to"], "additionalProperties": false } },
    "rename_entity_fields":{ "type": "array", "items": { "type": "object", "properties": { "entity": {"type":"string"}, "from": {"type":"string"}, "to": {"type":"string"} }, "required": ["entity","from","to"], "additionalProperties": false } },
    "rename_level_fields": { "type": "array", "items": { "type": "object", "properties": { "from": {"type":"string"}, "to": {"type":"string"} }, "required": ["from","to"], "additionalProperties": false } },
    "set_layers":          { "type": "array", "items": { "type": "object", "properties": { "def": {"type":"object"}, "index": {"type":"integer","minimum":0} }, "required": ["def"], "additionalProperties": false } },
    "set_enums":           { "type": "array", "items": { "type": "object" }, "description": "enums.json entries" },
    "set_entity_types":    { "type": "array", "items": { "type": "object" }, "description": "whole entities/<slug>.json without $schema/format" },
    "set_entity_fields":   { "type": "array", "items": { "type": "object", "properties": { "entity": {"type":"string"}, "def": {"type":"object"}, "index": {"type":"integer","minimum":0} }, "required": ["entity","def"], "additionalProperties": false } },
    "set_level_fields":    { "type": "array", "items": { "type": "object", "properties": { "def": {"type":"object"}, "index": {"type":"integer","minimum":0} }, "required": ["def"], "additionalProperties": false } },
    "remove_entity_fields":{ "type": "array", "items": { "type": "object", "properties": { "entity": {"type":"string"}, "field": {"type":"string"} }, "required": ["entity","field"], "additionalProperties": false } },
    "remove_level_fields": { "type": "array", "items": { "type": "string" } },
    "remove_enums":        { "type": "array", "items": { "type": "string" } },
    "remove_entity_types": { "type": "array", "items": { "type": "string" } },
    "remove_layers":       { "type": "array", "items": { "type": "string" } } },
    "additionalProperties": false },
  "confirm": { "type": "boolean", "default": false, "description": "Needed to remove a layer, field, enum or type, or to change the type of a field that has stored values." },
  "label": { "type": "string", "maxLength": 60 } },
  "required": ["patch"], "additionalProperties": false }"#,
    },
    ToolDef {
        name: "add_note",
        description: r##"Leave an annotation for the user: a todo or remark, a pin marking a place, a path, or a region. scope is { "level": "<slug>" } (geometry in pixels from the level's top-left corner) or { "world": "<slug>" } (world pixels). kind and geometry: note and pin take { "px": [x, y] }; path takes { "points": [[x, y], ...] } with 2 or more points; region takes { "rect": [x, y, width, height] } or 3 or more points; guide (world scope only) takes 2 or more points. A cell [x, y] of a 16 px layer starts at pixel [16x, 16y]. The first line of text is the note's title; any further lines are its body (Markdown). Optional: tags (e.g. ["todo"], or ["landmark", "east"] for a pin the game should know), color "#rrggbb", links ("level:<slug>", "entity:<id>", "note:<id>" or a URL), export true to put it in the game's export (notes, pins, paths and regions; guides never export), status "done"; for a pin, icon (pin, statue, pool, door, star, chest, skull, eye, flag or key) and pin_label (a short label drawn beside it); for a guide, closed (joins its last point to the first) and group (a guide group's slug, see set_guide_group). Returns the note's id."##,
        schema: r##"{ "type": "object",
  "properties": {
    "scope": { "type": "object",
      "properties": { "level": { "type": "string" }, "world": { "type": "string" } },
      "minProperties": 1, "maxProperties": 1, "additionalProperties": false },
    "kind": { "type": "string", "enum": ["note", "pin", "path", "region", "guide"] },
    "geometry": { "type": "object",
      "properties": {
        "px": { "type": "array", "items": { "type": "integer" }, "minItems": 2, "maxItems": 2 },
        "points": { "type": "array", "items": { "type": "array", "items": { "type": "integer" }, "minItems": 2, "maxItems": 2 } },
        "rect": { "type": "array", "items": { "type": "integer" }, "minItems": 4, "maxItems": 4 } },
      "additionalProperties": false },
    "text": { "type": "string", "description": "Title on the first line, optional Markdown body after." },
    "tags": { "type": "array", "items": { "type": "string" }, "description": "e.g. [\"todo\"]." },
    "color": { "type": "string", "description": "#rrggbb or #rrggbbaa." },
    "links": { "type": "array", "items": { "type": "string" }, "description": "level:<slug>, entity:<id>, note:<id> or a URL." },
    "export": { "type": "boolean", "default": false, "description": "Include it in the game's export." },
    "status": { "type": "string", "enum": ["open", "done"], "default": "open" },
    "icon": { "type": "string", "description": "Pin only: the icon's name." },
    "pin_label": { "type": "string", "description": "Pin only: a short label drawn beside it." },
    "closed": { "type": "boolean", "default": false, "description": "Guide only: join the last point to the first." },
    "group": { "type": "string", "description": "Guide only: a guide group slug." },
    "label": { "type": "string", "maxLength": 60, "description": "A few words on what this edit is, shown in the editor's undo history." } },
  "required": ["scope", "kind", "geometry", "text"], "additionalProperties": false }"##,
    },
    ToolDef {
        name: "list_notes",
        description: r#"List annotations (notes, pins, paths, regions) with their id, scope, kind, geometry (pixels), title, body, tags and status. Guides (locked reference lines) are left out unless kind is "guide" or group is given. Filters: scope ({ "level": slug } or { "world": slug }), tag, kind, status ("open" by default, "done", or "all"), group (the guides of that guide group), export (true: only what goes into the game's export; false: only what doesn't). Notes the user left often say what they want changed."#,
        schema: r#"{ "type": "object",
  "properties": {
    "scope": { "type": "object",
      "properties": { "level": { "type": "string" }, "world": { "type": "string" } },
      "minProperties": 1, "maxProperties": 1, "additionalProperties": false },
    "tag": { "type": "string" },
    "kind": { "type": "string", "enum": ["note", "pin", "path", "region", "guide"] },
    "status": { "type": "string", "enum": ["open", "done", "all"], "default": "open" },
    "group": { "type": "string", "description": "Guide group slug." },
    "export": { "type": "boolean" } },
  "additionalProperties": false }"#,
    },
    ToolDef {
        name: "resolve_note",
        description: r#"Mark a note done (for example after doing what it asked). It stays in the project with status done; the user can reopen it. status "open" reopens a note."#,
        schema: r#"{ "type": "object",
  "properties": {
    "id": { "type": "string", "description": "Note id." },
    "status": { "type": "string", "enum": ["done", "open"], "default": "done" },
    "label": { "type": "string", "maxLength": 60, "description": "A few words on what this edit is, shown in the editor's undo history." } },
  "required": ["id"], "additionalProperties": false }"#,
    },
    ToolDef {
        name: "update_note",
        description: r#"Change an annotation (note, pin, path, region or guide), found by id, in one undo entry. patch holds what changes; the rest stays as it is: text (title on the first line, Markdown body after, as for add_note; or title and body on their own, null removing one), tags, color, links (each list given replaces the old one whole), status ("open" or "done"), export, locked, a pin's icon and pin_label, a guide's closed and group (null takes it out of its group), and either geometry, the new shape ({ "px": [x, y] }, { "points": [[x, y], ...] } or { "rect": [x, y, width, height] }, in the annotation's own pixels: level pixels at level scope, world pixels at world scope) or move, [dx, dy] pixels added to every point. Kind and scope can't change: remove it and add a new one. A locked annotation (every guide is) can be retitled, retagged, resolved and unlocked freely, but moving it (geometry, move or group) needs "confirm": true; without it nothing changes and the error says so."#,
        schema: r##"{ "type": "object",
  "properties": {
    "id": { "type": "string", "description": "Annotation id." },
    "patch": { "type": "object",
      "properties": {
        "text": { "type": "string", "description": "Title on the first line, optional Markdown body after." },
        "title": { "type": ["string", "null"] },
        "body": { "type": ["string", "null"] },
        "tags": { "type": "array", "items": { "type": "string" } },
        "color": { "type": ["string", "null"], "description": "#rrggbb or #rrggbbaa; null for the default." },
        "links": { "type": "array", "items": { "type": "string" } },
        "status": { "type": "string", "enum": ["open", "done"] },
        "export": { "type": "boolean" },
        "locked": { "type": "boolean" },
        "icon": { "type": ["string", "null"] },
        "pin_label": { "type": ["string", "null"] },
        "closed": { "type": "boolean" },
        "group": { "type": ["string", "null"], "description": "Guide group slug; null takes the guide out of its group." },
        "geometry": { "type": "object",
          "properties": {
            "px": { "type": "array", "items": { "type": "integer" }, "minItems": 2, "maxItems": 2 },
            "points": { "type": "array", "items": { "type": "array", "items": { "type": "integer" }, "minItems": 2, "maxItems": 2 } },
            "rect": { "type": "array", "items": { "type": "integer" }, "minItems": 4, "maxItems": 4 } },
          "additionalProperties": false },
        "move": { "type": "array", "items": { "type": "integer" }, "minItems": 2, "maxItems": 2, "description": "[dx, dy] in pixels." } },
      "additionalProperties": false },
    "confirm": { "type": "boolean", "default": false, "description": "Needed to move a locked annotation." },
    "label": { "type": "string", "maxLength": 60, "description": "A few words on what this edit is, shown in the editor's undo history." } },
  "required": ["id", "patch"], "additionalProperties": false }"##,
    },
    ToolDef {
        name: "remove_note",
        description: r#"Remove annotations by id, all in one undo entry. Removing a locked one (every guide is locked) needs "confirm": true; without it nothing changes and the error names it."#,
        schema: r#"{ "type": "object",
  "properties": {
    "ids": { "type": "array", "items": { "type": "string" }, "minItems": 1 },
    "confirm": { "type": "boolean", "default": false, "description": "Needed to remove a locked annotation." },
    "label": { "type": "string", "maxLength": 60, "description": "A few words on what this edit is, shown in the editor's undo history." } },
  "required": ["ids"], "additionalProperties": false }"#,
    },
    ToolDef {
        name: "set_guide_group",
        description: r#"Create, change or remove a guide group: a named set of guides that the editor draws, and levels snap to, moved by the group's offset in world pixels from where they were designed; for example a section of the layout built one position lower than the rest. Give slug and any of name, offset and color; what you leave out stays as it is (a new group starts at offset [0, 0]). remove true removes the group instead, which is refused while a guide still names it (update_note each guide's group to null first). The guides keep their points; only where they are shown moves. Changes world.json."#,
        schema: r##"{ "type": "object",
  "properties": {
    "slug": { "type": "string", "description": "Guide group slug, e.g. \"shifted\"." },
    "name": { "type": "string" },
    "offset": { "type": "array", "items": { "type": "integer" }, "minItems": 2, "maxItems": 2, "description": "[dx, dy] in world pixels." },
    "color": { "type": "string", "description": "#rrggbb for the group's guides." },
    "remove": { "type": "boolean", "default": false },
    "world": { "type": "string", "description": "World slug. Needed only when the project has more than one." },
    "label": { "type": "string", "maxLength": 60, "description": "A few words on what this edit is, shown in the editor's undo history." } },
  "required": ["slug"], "additionalProperties": false }"##,
    },
    ToolDef {
        name: "tag_levels",
        description: r#"Set the tags of one or more levels, all in one undo entry: { "levels": { "crevasse": ["east"], "shaft": ["west", "flooded"] } }. Each list replaces that level's tags (an empty list clears them). Tags are words saying what a level is part of, such as region tags like east and west, or flooded; they go into the game's export, describe_project and read_level show them, and bake_and_validate reports a level tagged like a world region (a region annotation with that tag) whose center lies outside every such region."#,
        schema: r#"{ "type": "object",
  "properties": {
    "levels": { "type": "object", "additionalProperties": { "type": "array", "items": { "type": "string" } }, "minProperties": 1, "description": "Level slug -> its new tags." },
    "label": { "type": "string", "maxLength": 60, "description": "A few words on what this edit is, shown in the editor's undo history." } },
  "required": ["levels"], "additionalProperties": false }"#,
    },
    ToolDef {
        name: "edit_rules",
        description: r#"Change the autotiling rules of an auto layer, all in one undo entry. An auto layer's tiles are generated from an int_grid layer (its source, e.g. collision) by rules kept in rules/<layer>.json: read them from the resource worlds://defs/rules/<layer>.json, the file format from worlds://schema/rules.json. Rules sit in groups and run in order, first group first; where a rule with "break": true fires, later rules of the same pass don't draw on that cell. A rule is { "slug", "match", "out", ... }. match.rows is a small picture of the source grid around the cell being tiled, one glyph per cell, the middle one being that cell (odd width and height): a value's glyph (describe_project), "." empty, "+" not empty, "*" anything, and letters you define in match.legend, e.g. { "S": "!solid" } (not solid), { "w": "[solid|fortified]" } (either), { "o": "![solid|.]" } (neither). An "@" in the rows marks the cell being tiled instead of the middle (sizes may then be even); legend "@" gives it a test. out is { "tileset", "tiles": [[col, row], ...] }, one alternative picked per cell, or { "stamp": "<slug>" } to draw a stamp (a block of atlas tiles defined in the file's stamps). Optional: require (a second pattern, or a list of patterns, that must match too, e.g. on another int_grid layer named by its "grid"), symmetry (any of "mirror_x", "mirror_y", "rotate": also match mirrored or rotated copies, drawing the tile mirrored or rotated to suit), chance (0 to 1), break, out_of_bounds (what cells outside the level count as, e.g. "solid").
Passes: a group may say "pass": n (default 0). Groups run pass by pass, lowest first (keep the file in pass order); a later pass draws on top, starts with no break locks, and sees what earlier passes wrote. Writing: instead of out, a rule may have "write": { "grid": "<derived int_grid layer>", "value": "<its value>" }: where it fires it sets that cell of the derived layer (the first write to a cell in a pass wins), and a later pass, or another auto layer, can match on that layer with match.grid. Region rules: instead of match, a rule may have "region" and "place", firing at places inside rooms: region { "cells": a cell test such as "." (which cells form regions: 4-connected areas of them), "grid" (default the source), "min_width", "max_width", "min_height", "max_height", "min_area", "max_area" (bounding-box size and cell count, inclusive) } selects regions; place { "along": "floor", "ceiling", "wall_left" or "wall_right" (each run of cells on that edge of a region) or "across": "interior", "spacing": "even" (default, every cells apart, "align": "start", "center" or "end"), "spread" (as many as fit at least every apart, end to end) or "random" ("jitter" extra cells), "every", "margin" (cells kept clear at each end of a run), "min_length", "against" (a cell test the cell across the edge must pass) } says where in each region it fires. A region rule takes slug, active, region, place, out or write, break and salt, nothing else. find_regions shows the regions a cell test makes. Occupancy: a stamp with "occupies": true marks the cells it covers as taken when placed, and an out with "avoid_overlap": true skips a placement that would cover a taken cell (the first in reading order wins); the typical use is a pass that writes marks and a later pass that draws stamps on them.
Templates: rules/macros.json (worlds://defs/rules/macros.json) holds named patterns (rows usable as "rows": "<name>"), legends ("legend": "<name>") and templates. A template is a list of rules whose tiles are relative to an origin and whose tileset is left out. A group entry { "use": "<template>", "slug": "<name>", "tileset": "<tileset>", "origin": [col, row] } stands for all of a template's rules, named <slug>__<rule>, drawing from that tileset with each tile moved by origin; it may add "tiles" (per template rule slug, replacement alternatives relative to origin), "skip" (template rule slugs to leave out) and "legend" (glyphs to remap). Templates can't hold region rules or write. The built-in template blob47 is at worlds://templates/blob47.json.
patch holds any of these, applied in this order: set_macros ([{ kind: "pattern", "legend" or "template", name, def }], def as in macros.json), set_stamps ([{ stamp, index? }]), set_groups (whole groups as in the file; an existing slug is replaced in place, a new one appended), set_rules ([{ group, rule, index? }]: rule is a rule or a "use" entry; one with an existing slug in that group is replaced, and moved to index when given; a new one is inserted at index, default the end), move_rules ([{ rule, group, index }], also into another group; index counts after taking the rule out), then the removals: remove_rules (rule or reference slugs; an expanded <reference>__<rule> can't be removed, put its rule slug in the reference's skip), remove_groups, remove_stamps (slugs), remove_macros ([{ kind, name }]; refused while used). A rule's slug lives in one group: to move it, use move_rules, not remove and set. Auto layers rebake on their own. expand_rules shows exactly what runs; preview_rule shows where a rule fires and where it matches but is blocked."#,
        schema: r#"{ "type": "object",
  "properties": {
    "layer": { "type": "string", "description": "auto layer slug." },
    "patch": { "type": "object",
      "properties": {
        "set_macros": { "type": "array", "items": { "type": "object",
          "properties": { "kind": { "type": "string", "enum": ["pattern", "legend", "template"] }, "name": { "type": "string" }, "def": { "description": "Rows (array of strings), a legend (object) or a template (object), as in rules/macros.json." } },
          "required": ["kind", "name", "def"], "additionalProperties": false } },
        "set_stamps": { "type": "array", "items": { "type": "object",
          "properties": { "stamp": { "type": "object" }, "index": { "type": "integer", "minimum": 0 } },
          "required": ["stamp"], "additionalProperties": false } },
        "set_groups": { "type": "array", "items": { "type": "object" }, "description": "Rule groups as in rules/<layer>.json." },
        "set_rules": { "type": "array", "items": { "type": "object",
          "properties": { "group": { "type": "string" }, "rule": { "type": "object" }, "index": { "type": "integer", "minimum": 0 } },
          "required": ["group", "rule"], "additionalProperties": false } },
        "move_rules": { "type": "array", "items": { "type": "object",
          "properties": { "rule": { "type": "string" }, "group": { "type": "string" }, "index": { "type": "integer", "minimum": 0 } },
          "required": ["rule", "group", "index"], "additionalProperties": false } },
        "remove_rules": { "type": "array", "items": { "type": "string" } },
        "remove_groups": { "type": "array", "items": { "type": "string" } },
        "remove_stamps": { "type": "array", "items": { "type": "string" } },
        "remove_macros": { "type": "array", "items": { "type": "object",
          "properties": { "kind": { "type": "string", "enum": ["pattern", "legend", "template"] }, "name": { "type": "string" } },
          "required": ["kind", "name"], "additionalProperties": false } } },
      "additionalProperties": false },
    "label": { "type": "string", "maxLength": 60, "description": "A few words on what this edit is, shown in the editor's undo history." } },
  "required": ["layer", "patch"], "additionalProperties": false }"#,
    },
    ToolDef {
        name: "preview_rule",
        description: r#"Show where an autotile rule fires in a level, and where it matches but doesn't fire, without changing anything. rule is a rule's slug, a template reference's slug (all the rules it stands for), or one rule of a reference (<reference>__<rule>). Returns the cells, the rule's pass, and a picture of the int_grid layer the rule matches on (as in read_level; a derived layer by its effective values; a region rule's region grid) with "@" on every cell where it fires and "?" where its pattern matches but it doesn't fire there: its chance or perlin noise said no, an earlier rule of its pass with break already took the cell, or (listed in occupied) every placement there would cover a cell an occupying stamp already took. A rule that should fire somewhere but shows "?" is being blocked; one that shows neither doesn't match there at all. A region rule also returns regions: each region of its cell test with bbox [x, y, width, height], area, selected (its bounds hold), runs ([x, y, length] of its edge, before margin) and candidates (where it would fire). A write rule returns write { grid, value }. When the rules of a reference match on different int_grid layers, each layer gets its own picture (other_grids), marking only its own rules' cells."#,
        schema: r#"{ "type": "object",
  "properties": {
    "layer": { "type": "string", "description": "auto layer slug." },
    "rule": { "type": "string", "description": "Rule slug, template reference slug, or <reference>__<rule>." },
    "level": { "type": "string", "description": "Level slug." } },
  "required": ["layer", "rule", "level"], "additionalProperties": false }"#,
    },
    ToolDef {
        name: "find_regions",
        description: r#"Find the regions (rooms) of a level: the 4-connected areas of cells that pass a cell test on an int_grid layer, as region rules see them (edit_rules). cells is the test: "." empty cells, "+" any value, a value or group slug ("solid"), "!solid" (anything but), "[.|oneway]" (either), "![solid|water]" (neither). grid is the int_grid layer (default the project's collision layer; a derived layer is read by its effective values). Returns each region with its id and letter, bbox [x, y, width, height] in cells, width, height, area (cell count) and edges: the runs of its floor (region cells whose cell below is inside the level and not in the region), ceiling, wall_left and wall_right, each run [x, y, length] from its first cell (along a row for floor and ceiling, down a column for walls); along limits edges to one kind. A region's border at the level edge is no edge. picture shows the grid with each region's cells as its letter (a to z by id, then +) and every other cell #."#,
        schema: r#"{ "type": "object",
  "properties": {
    "level": { "type": "string", "description": "Level slug." },
    "cells": { "type": "string", "description": "Cell test, e.g. \".\" (empty cells) or \"[.|oneway]\"." },
    "grid": { "type": "string", "description": "int_grid layer slug. Default: the collision layer." },
    "along": { "type": "string", "enum": ["floor", "ceiling", "wall_left", "wall_right"], "description": "Only this kind of edge." } },
  "required": ["level", "cells"], "additionalProperties": false }"#,
    },
    ToolDef {
        name: "expand_rules",
        description: r#"List an auto layer's rules exactly as they run, in order: named patterns and legends written out, and every template reference ("use") replaced by its rules, each named <reference>__<rule>, with the reference's tileset, its tiles moved by the reference's origin, and "from" naming the reference. group limits it to one group. Use it to check what a template reference turns into before or after edit_rules. Returns JSON: { rev, layer, groups: [{ slug, rules }] }."#,
        schema: r#"{ "type": "object",
  "properties": {
    "layer": { "type": "string", "description": "auto layer slug." },
    "group": { "type": "string", "description": "Only this group (slug)." } },
  "required": ["layer"], "additionalProperties": false }"#,
    },
    ToolDef {
        name: "bake_and_validate",
        description: r#"Check the project: regenerate the auto layers (reporting levels whose bake fails, e.g. a missing tileset image) and run validation, which reports problems such as dangling references, field values of the wrong type, and entities outside their level, each as file, JSON path and message. With export true, also write the export/ directory the game reads. warnings lists what the game's importer can't read faithfully; it is currently always empty: the export holds nothing a game importer can't read (transposed tiles, tiles off their layer's grid and several auto layers per level are all supported). Run it after a set of edits; an empty problem list means the project is valid."#,
        schema: r#"{ "type": "object",
  "properties": {
    "levels": { "type": "array", "items": { "type": "string" }, "description": "Only bake these levels. Default: all." },
    "export": { "type": "boolean", "default": false } },
  "additionalProperties": false }"#,
    },
    ToolDef {
        name: "screenshot",
        description: r#"Render a level, or a rectangle of the world in world pixels ([x, y, width, height]), to a PNG image so you can see it as the game will roughly look: all visible layers including generated tiles, with entity boxes and open notes drawn on top. Give level or world_rect. size is the largest image wanted, default [1024, 1024]. Works with or without the Worlds editor open."#,
        schema: r#"{ "type": "object",
  "properties": {
    "level": { "type": "string" },
    "world_rect": { "type": "array", "items": { "type": "integer" }, "minItems": 4, "maxItems": 4 },
    "layers": { "type": "array", "items": { "type": "string" }, "description": "Only these layers. Default: all visible." },
    "size": { "type": "array", "items": { "type": "integer", "minimum": 16, "maximum": 4096 }, "minItems": 2, "maxItems": 2 } },
  "additionalProperties": false }"#,
    },
    ToolDef {
        name: "undo",
        description: r#"Undo your own last n edits (each tool call that wrote is one edit). Stops early, and says so, at an edit the user made, so it never takes back their work."#,
        schema: r#"{ "type": "object",
  "properties": { "n": { "type": "integer", "minimum": 1, "maximum": 50, "default": 1 } },
  "additionalProperties": false }"#,
    },
];
