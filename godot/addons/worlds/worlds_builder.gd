@tool
class_name WorldsBuilder
extends RefCounted
## Builds Godot nodes from a Worlds export directory (the one holding
## `world.json`, written by `worlds-cli bake`). Works in the editor (the
## Worlds plugin saves the result as scenes) and at runtime:
##
##     var b := WorldsBuilder.open("res://worlds/export")
##     add_child(b.build_level("crevasse"))
##
## The export format is documented in Worlds' docs/format.md, "export/".

const FORMAT := 1

# Godot's alternative-tile transform flags, by export flip bit.
const FLIP_H := TileSetAtlasSource.TRANSFORM_FLIP_H
const FLIP_V := TileSetAtlasSource.TRANSFORM_FLIP_V
const TRANSPOSE := TileSetAtlasSource.TRANSFORM_TRANSPOSE

## Folder of entity scenes: an entity of type `door` instances
## `<entity_scene_dir>/door.tscn` when it exists, else it is a Marker2D.
var entity_scene_dir := ""
## Build a StaticBody2D from the collision layer's blocking cells.
var build_collision := true
## Problems met while building (missing images, unknown levels...). Building
## carries on past them; check this afterwards.
var errors := PackedStringArray()

var export_dir := ""
var world := {}
var enums := {}

var _levels := {}  # slug -> world.json level entry (plus "world")
var _tilesets := {}  # key -> TileSet
var _tileset_slugs := {}  # key -> tileset slug


## Reads `world.json` (and `enums.json`) from `dir`. Returns null, after
## pushing an error, when the export can't be read.
static func open(dir: String) -> WorldsBuilder:
	var b := WorldsBuilder.new()
	b.export_dir = dir.trim_suffix("/")
	b.world = b._read_json("world.json")
	if b.world.is_empty():
		return null
	if int(b.world.get("format", 0)) != FORMAT:
		push_error("Worlds: %s/world.json has format %s; this importer reads format %d" % [dir, b.world.get("format"), FORMAT])
		return null
	var enums_file: String = b.world.get("files", {}).get("enums", "")
	if enums_file != "":
		b.enums = b._read_json(enums_file)
	for w in b.world.get("worlds", []):
		for l in w.get("levels", []):
			var entry: Dictionary = l.duplicate()
			entry["world"] = w["slug"]
			b._levels[l["slug"]] = entry
	return b


## Every level's slug, in `world.json` order.
func level_slugs() -> PackedStringArray:
	return PackedStringArray(_levels.keys())


## The slugs of the levels of one world.
func world_level_slugs(world_slug: String) -> PackedStringArray:
	var out := PackedStringArray()
	for slug in _levels:
		if _levels[slug]["world"] == world_slug:
			out.append(slug)
	return out


## The tilesets built so far (shared by every level that uses them), by slug.
## Save them as resources before packing level scenes so the scenes share
## them instead of each embedding a copy.
func tilesets() -> Dictionary:
	var out := {}
	for key in _tilesets:
		var slug: String = _tileset_slugs[key]
		# Same tileset slug with different settings can't happen within one
		# export, but keep both rather than lose one.
		if out.has(slug):
			slug = "%s_%d" % [slug, out.size()]
		out[slug] = _tilesets[key]
	return out


## Builds one level: a Node2D at its world position (top-left), holding
## `Background`, `Tiles`, `Collision` and `Entities`. Returns null for an
## unknown level or an unreadable level file.
func build_level(slug: String) -> Node2D:
	if not _levels.has(slug):
		errors.append("unknown level %s" % slug)
		return null
	var entry: Dictionary = _levels[slug]
	var data := _read_json(entry["file"])
	if data.is_empty():
		errors.append("level %s: can't read %s" % [slug, entry["file"]])
		return null

	var root := Node2D.new()
	root.name = slug
	root.position = _vec2(data["world_px"])
	var grid := int(data["grid_size"])
	var size_px := Vector2(data["size"][0], data["size"][1]) * grid
	root.set_meta("worlds_slug", slug)
	root.set_meta("worlds_id", data["id"])
	root.set_meta("worlds_name", data.get("name", slug))
	root.set_meta("worlds_world", data["world"])
	root.set_meta("worlds_size_px", size_px)
	root.set_meta("worlds_depth", data.get("depth", 0))
	root.set_meta("worlds_tags", PackedStringArray(data.get("tags", [])))
	root.set_meta("worlds_fields", data.get("fields", {}))
	root.set_meta("worlds_fields_set", PackedStringArray(data.get("fields_set", [])))
	root.set_meta("worlds_touching", entry.get("touching", []))
	root.set_meta("worlds_open_edges", entry.get("open_edges"))
	root.set_meta("worlds_int_grids", data.get("int_grids", {}))
	# Level notes are in world pixels, like the world's.
	root.set_meta("worlds_notes", data.get("notes", []))

	var bg = data.get("bg_color", world.get("bg_color"))
	if bg != null:
		var rect := ColorRect.new()
		rect.name = "Background"
		rect.color = Color.html(bg)
		rect.size = size_px
		rect.mouse_filter = Control.MOUSE_FILTER_IGNORE
		rect.z_index = -100
		_add(root, rect)

	var tiles := Node2D.new()
	tiles.name = "Tiles"
	_add(root, tiles)
	for layer in data.get("tile_layers", []):
		_build_tile_layer(tiles, layer, slug)

	if build_collision:
		var body := _build_collision(data)
		if body != null:
			_add(root, body)

	var entities := Node2D.new()
	entities.name = "Entities"
	_add(root, entities)
	for e in data.get("entities", []):
		_add(entities, _build_entity(e))

	_own(root, root)
	return root


## Builds every level of a world (the first one when `world_slug` is empty)
## under one Node2D, each at its world position. World notes go in the
## root's `worlds_notes` meta.
func build_world(world_slug := "") -> Node2D:
	var worlds: Array = world.get("worlds", [])
	if worlds.is_empty():
		return null
	var w: Dictionary = worlds[0]
	for candidate in worlds:
		if candidate["slug"] == world_slug:
			w = candidate
	var root := Node2D.new()
	root.name = w["slug"]
	root.set_meta("worlds_notes", w.get("notes", []))
	for slug in world_level_slugs(w["slug"]):
		var level := build_level(slug)
		if level != null:
			_add(root, level)
	_own(root, root)
	return root


# Tile layers ------------------------------------------------------------

func _build_tile_layer(parent: Node2D, layer: Dictionary, level: String) -> void:
	var holder := Node2D.new()
	holder.name = _node_name(layer.get("name", layer["layer"]))
	holder.set_meta("worlds_layer", layer["layer"])
	holder.set_meta("worlds_kind", layer["kind"])
	if layer.has("opacity"):
		holder.modulate.a = float(layer["opacity"])
	_add(parent, holder)

	# Each run of tiles from one tileset goes into TileMapLayers whose cells
	# are that tileset's tiles. Tiles off that grid (a 16 px offset) or
	# stacked on an occupied cell go into further layers, offset to match,
	# so every tile lands on its exact pixel. Layers keep export draw order.
	for run in layer.get("tilesets", []):
		var ts := _tileset(run, level)
		if ts == null:
			continue
		var tile_size := ts.tile_size
		var subs := {}  # "ox,oy" -> Array of TileMapLayers, in creation order
		for t in run["tiles"]:
			if int(t[4]) < 0 or int(t[4]) > 7:
				errors.append("level %s: %s: tile at %d,%d has flip %d (0..7)" % [level, layer["layer"], t[0], t[1], t[4]])
				continue
			var x := int(t[0])
			var y := int(t[1])
			var off := Vector2i(posmod(x, tile_size.x), posmod(y, tile_size.y))
			var cell := Vector2i((x - off.x) / tile_size.x, (y - off.y) / tile_size.y)
			var atlas := Vector2i(int(t[2]), int(t[3]))
			var source: TileSetAtlasSource = ts.get_source(0)
			if not source.has_tile(atlas):
				if not _atlas_fits(source, atlas):
					errors.append("level %s: %s: atlas tile %s is outside %s" % [level, layer["layer"], atlas, run["image"]])
					continue
				source.create_tile(atlas)
			var key := "%d,%d" % [off.x, off.y]
			if not subs.has(key):
				subs[key] = []
			var target: TileMapLayer = null
			for candidate in subs[key]:
				if candidate.get_cell_source_id(cell) == -1:
					target = candidate
					break
			if target == null:
				target = TileMapLayer.new()
				target.tile_set = ts
				target.position = Vector2(off)
				var n: int = holder.get_child_count()
				target.name = _tile_map_name(run["tileset"], off, n)
				_add(holder, target)
				subs[key].append(target)
			target.set_cell(cell, 0, atlas, _alternative(int(t[4])))


func _tile_map_name(tileset: String, off: Vector2i, index: int) -> String:
	if off == Vector2i.ZERO:
		return "%s#%d" % [tileset, index]
	return "%s+%d+%d#%d" % [tileset, off.x, off.y, index]


## Export flip bits (1 = mirror x, 2 = mirror y, 4 = transpose, transpose
## first) as Godot's alternative-tile flags, which apply the same way.
static func _alternative(flip: int) -> int:
	var alt := 0
	if flip & 1:
		alt |= FLIP_H
	if flip & 2:
		alt |= FLIP_V
	if flip & 4:
		alt |= TRANSPOSE
	return alt


func _tileset(run: Dictionary, level: String) -> TileSet:
	var size := Vector2i(int(run["tile_size"][0]), int(run["tile_size"][1]))
	var spacing := int(run.get("spacing", 0))
	var padding := int(run.get("padding", 0))
	var path := _resolve(run["image"])
	var key := "%s|%s|%d|%d" % [path, size, spacing, padding]
	if _tilesets.has(key):
		return _tilesets[key]
	var texture := _texture(path)
	if texture == null:
		errors.append("level %s: can't load tileset image %s" % [level, path])
		_tilesets[key] = null
		_tileset_slugs[key] = run["tileset"]
		return null
	var ts := TileSet.new()
	ts.tile_size = size
	var source := TileSetAtlasSource.new()
	source.texture = texture
	source.texture_region_size = size
	source.separation = Vector2i(spacing, spacing)
	source.margins = Vector2i(padding, padding)
	ts.add_source(source, 0)
	_tilesets[key] = ts
	_tileset_slugs[key] = run["tileset"]
	return ts


func _atlas_fits(source: TileSetAtlasSource, atlas: Vector2i) -> bool:
	return source.get_atlas_grid_size().x > atlas.x and source.get_atlas_grid_size().y > atlas.y \
		and atlas.x >= 0 and atlas.y >= 0


## An imported texture when the image is a project resource, else one built
## from the file (runtime use, or an export outside the project).
func _texture(path: String) -> Texture2D:
	if path.begins_with("res://") and ResourceLoader.exists(path):
		return load(path)
	var image := Image.load_from_file(path)
	if image == null or image.is_empty():
		return null
	return ImageTexture.create_from_image(image)


# Collision ----------------------------------------------------------------

## One StaticBody2D with a rectangle per run of blocking cells, rows merged
## downwards where they line up. Null when there's no collision layer or no
## blocking cell.
func _build_collision(data: Dictionary) -> StaticBody2D:
	var layer = world.get("collision_layer")
	if layer == null:
		return null
	var grid_data: Dictionary = data.get("int_grids", {}).get(layer, {})
	if grid_data.is_empty():
		return null
	var blocking := {}
	for v in world.get("blocking_values", []):
		blocking[v] = true
	var solid_glyphs := {}
	var legend: Dictionary = grid_data.get("legend", {})
	for glyph in legend:
		if legend[glyph] != null and blocking.has(legend[glyph]):
			solid_glyphs[glyph] = true
	var cell := int(grid_data.get("grid_size", data["grid_size"]))

	# Runs per row, then grow each run down while the next row has the same.
	var open := {}  # "x0,x1" -> Rect2i in cells, still growing
	var rects: Array[Rect2i] = []
	var rows: Array = grid_data.get("rows", [])
	for y in rows.size() + 1:
		var runs := {}
		if y < rows.size():
			var row: String = rows[y]
			var x := 0
			while x < row.length():
				if solid_glyphs.has(row[x]):
					var x0 := x
					while x < row.length() and solid_glyphs.has(row[x]):
						x += 1
					runs["%d,%d" % [x0, x]] = Vector2i(x0, x)
				else:
					x += 1
		for k in open.keys():
			if runs.has(k):
				var grown: Rect2i = open[k]
				grown.size.y += 1
				open[k] = grown
				runs.erase(k)
			else:
				rects.append(open[k])
				open.erase(k)
		for k in runs:
			var r: Vector2i = runs[k]
			open[k] = Rect2i(r.x, y, r.y - r.x, 1)
	if rects.is_empty():
		return null

	var body := StaticBody2D.new()
	body.name = "Collision"
	for r in rects:
		var shape := RectangleShape2D.new()
		shape.size = Vector2(r.size * cell)
		var cs := CollisionShape2D.new()
		cs.shape = shape
		cs.position = Vector2(r.position * cell) + shape.size / 2
		_add(body, cs)
	return body


# Entities -------------------------------------------------------------------

func _build_entity(e: Dictionary) -> Node2D:
	var node: Node2D = null
	if entity_scene_dir != "":
		var path := entity_scene_dir.path_join("%s.tscn" % e["type"])
		if ResourceLoader.exists(path):
			var scene: PackedScene = load(path)
			var inst := scene.instantiate()
			if inst is Node2D:
				node = inst
			else:
				errors.append("entity %s: %s's root isn't a Node2D" % [e["id"], path])
				inst.free()
	if node == null:
		node = Marker2D.new()
	node.name = e["id"]
	node.position = _vec2(e["px"])
	var fields: Dictionary = e.get("fields", {})
	node.set_meta("worlds_id", e["id"])
	node.set_meta("worlds_type", e["type"])
	node.set_meta("worlds_layer", e.get("layer", ""))
	node.set_meta("worlds_size", _vec2(e.get("size", [1, 1])))
	node.set_meta("worlds_fields", fields)
	node.set_meta("worlds_fields_set", PackedStringArray(e.get("fields_set", [])))
	# Fields land on properties of the same name when the scene has them
	# (an @export var, say), so a door scene can just declare `var locked`.
	for f in fields:
		if f in node and fields[f] != null:
			node.set(f, fields[f])
	# A scene can also take the whole entity: `func _worlds_apply(entity)`.
	# In the editor that only runs when the script is a @tool script.
	if node.has_method("_worlds_apply"):
		node.call("_worlds_apply", e)
	return node


# Helpers --------------------------------------------------------------------

func _read_json(rel: String) -> Dictionary:
	var path := _resolve(rel)
	var text := FileAccess.get_file_as_string(path)
	if text == "":
		push_error("Worlds: can't read %s" % path)
		return {}
	var parsed = JSON.parse_string(text)
	if not parsed is Dictionary:
		push_error("Worlds: %s isn't a JSON object" % path)
		return {}
	return parsed


## Export paths are relative to the export directory, `/`-separated.
func _resolve(rel: String) -> String:
	return export_dir.path_join(rel).simplify_path()


static func _vec2(a: Array) -> Vector2:
	return Vector2(a[0], a[1])


## A layer's display name as a node name (Godot replaces . : @ / " % too,
## but would also rename silently; doing it here keeps names predictable).
static func _node_name(name: String) -> String:
	var out := name
	for c in [".", ":", "@", "/", "\"", "%"]:
		out = out.replace(c, "_")
	return out if out != "" else "_"


static func _add(parent: Node, child: Node) -> void:
	parent.add_child(child, true)


## Sets `owner` on every descendant so PackedScene.pack keeps them. Nodes
## inside an instanced entity scene keep their own owner.
static func _own(node: Node, owner: Node) -> void:
	for child in node.get_children():
		child.owner = owner
		if child.scene_file_path == "":
			_own(child, owner)
