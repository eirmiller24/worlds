extends SceneTree
## Headless tests for the Worlds addon. Run through tests/run.sh, which bakes
## the fixtures first:
##
##     godot --headless --path godot -s tests/test_import.gd -- <dir>...
##
## Each <dir> is an export directory. For every level, every tile of the
## export must come out of the TileMapLayers at the same pixel with the same
## atlas tile and flip, every blocking collision cell must be covered by
## exactly the collision shapes, and every entity must be at its pivot. The
## last <dir> is also imported to scenes under user://, which are loaded
## back.

var failures := 0
var checks := 0


func _init() -> void:
	var dirs := OS.get_cmdline_user_args()
	if dirs.is_empty():
		_fail("no export directories given")
	for dir in dirs:
		_check_export(dir)
	if not dirs.is_empty():
		_check_scenes(dirs[-1])
	print("%d checks, %d failure(s)" % [checks, failures])
	quit(1 if failures > 0 else 0)


func _check_export(dir: String) -> void:
	var b := WorldsBuilder.open(dir)
	if b == null:
		_fail("%s: can't open" % dir)
		return
	for slug in b.level_slugs():
		var data: Dictionary = JSON.parse_string(FileAccess.get_file_as_string(
			dir.path_join(_level_file(b, slug))))
		var level := b.build_level(slug)
		if level == null:
			_fail("%s %s: not built" % [dir, slug])
			continue
		_check_tiles(dir, slug, data, level)
		_check_collision(dir, slug, b.world, data, level)
		_check_entities(dir, slug, data, level)
		_check_outline(dir, slug, data, level)
		level.free()
	for e in b.errors:
		# Out-of-range flips are a refusal case; anything else is a bug.
		if not dir.ends_with("flip_out_of_range"):
			_fail("%s: %s" % [dir, e])


func _level_file(b: WorldsBuilder, slug: String) -> String:
	for w in b.world["worlds"]:
		for l in w["levels"]:
			if l["slug"] == slug:
				return l["file"]
	return ""


func _check_tiles(dir: String, slug: String, data: Dictionary, level: Node2D) -> void:
	for layer in data["tile_layers"]:
		var want := {}
		for run in layer["tilesets"]:
			for t in run["tiles"]:
				if int(t[4]) > 7:
					continue
				_count(want, "%d,%d %d,%d %d" % [t[0], t[1], t[2], t[3], WorldsBuilder._alternative(int(t[4]))])
		var got := {}
		var holder := level.get_node("Tiles").get_node_or_null(WorldsBuilder._node_name(layer["name"]))
		if holder == null:
			_fail("%s %s: no node for layer %s" % [dir, slug, layer["layer"]])
			continue
		for map in holder.get_children():
			var tm := map as TileMapLayer
			var size := tm.tile_set.tile_size
			for cell in tm.get_used_cells():
				var px := Vector2i(tm.position) + cell * size
				var atlas := tm.get_cell_atlas_coords(cell)
				_count(got, "%d,%d %d,%d %d" % [px.x, px.y, atlas.x, atlas.y, tm.get_cell_alternative_tile(cell)])
		_expect(want == got, "%s %s %s: tiles differ\n  want %s\n  got  %s" % [dir, slug, layer["layer"], want, got])


func _check_collision(dir: String, slug: String, world: Dictionary, data: Dictionary, level: Node2D) -> void:
	var layer = world.get("collision_layer")
	var want := {}
	var cell := 0
	if layer != null and data["int_grids"].has(layer):
		var g: Dictionary = data["int_grids"][layer]
		cell = int(g["grid_size"])
		var rows: Array = g["rows"]
		for y in rows.size():
			for x in rows[y].length():
				var v = g["legend"].get(rows[y][x])
				if v != null and v in world["blocking_values"]:
					want["%d,%d" % [x, y]] = 1
	var got := {}
	var body := level.get_node_or_null("Collision")
	if body != null:
		for cs in body.get_children():
			var size: Vector2 = cs.shape.size
			var top_left: Vector2 = cs.position - size / 2
			for y in int(size.y) / cell:
				for x in int(size.x) / cell:
					var k := "%d,%d" % [int(top_left.x) / cell + x, int(top_left.y) / cell + y]
					_expect(not got.has(k), "%s %s: collision cell %s covered twice" % [dir, slug, k])
					got[k] = 1
	_expect(want == got, "%s %s: collision covers %d cell(s), want %d" % [dir, slug, got.size(), want.size()])


func _check_entities(dir: String, slug: String, data: Dictionary, level: Node2D) -> void:
	var entities := level.get_node("Entities")
	_expect(entities.get_child_count() == data["entities"].size(),
		"%s %s: %d entities, want %d" % [dir, slug, entities.get_child_count(), data["entities"].size()])
	for e in data["entities"]:
		var node := entities.get_node_or_null(NodePath(e["id"])) as Node2D
		if node == null:
			_fail("%s %s: no entity %s" % [dir, slug, e["id"]])
			continue
		_expect(node.position == Vector2(e["px"][0], e["px"][1]), "%s %s: %s at %s" % [dir, slug, e["id"], node.position])
		_expect(node.get_meta("worlds_type") == e["type"], "%s %s: %s type" % [dir, slug, e["id"]])
		_expect(node.get_meta("worlds_fields") == e["fields"], "%s %s: %s fields" % [dir, slug, e["id"]])


## Imports to scenes, twice (the second run must give the same files), and
## loads them back.
## `worlds_outline`: the export's outline moved to the level's pixels, or
## the box's corners for a rectangular level; a shaped level's background
## is a polygon of it.
func _check_outline(dir: String, slug: String, data: Dictionary, level: Node2D) -> void:
	var got: PackedVector2Array = level.get_meta("worlds_outline")
	var want := PackedVector2Array()
	var g := int(data["grid_size"])
	if data.has("outline"):
		for p in data["outline"]:
			want.append(Vector2(p[0] - data["world_px"][0], p[1] - data["world_px"][1]))
		var bg := level.get_node_or_null("Background")
		if bg != null:
			_expect(bg is Polygon2D and bg.polygon == want, "%s %s: background isn't the outline" % [dir, slug])
	else:
		var s := Vector2(data["size"][0] * g, data["size"][1] * g)
		want = PackedVector2Array([Vector2.ZERO, Vector2(s.x, 0), s, Vector2(0, s.y)])
	_expect(got == want, "%s %s: worlds_outline %s, want %s" % [dir, slug, got, want])
	_expect(level.get_meta("worlds_void") == data.get("void", []), "%s %s: worlds_void" % [dir, slug])


func _check_scenes(dir: String) -> void:
	var out := "user://worlds_test"
	_wipe(out)
	var errors := WorldsImport.run(dir.path_join("world.json"), out)
	_expect(errors.is_empty(), "import: %s" % [errors])
	var first := _snapshot(out)
	errors = WorldsImport.run(dir.path_join("world.json"), out)
	_expect(_snapshot(out) == first, "a second import changed the files")

	var b := WorldsBuilder.open(dir)
	for slug in b.level_slugs():
		var path := out.path_join("levels/%s.tscn" % slug)
		_expect(FileAccess.file_exists(path), "no %s" % path)
		var scene: PackedScene = load(path)
		var level := scene.instantiate()
		_expect(level.get_node("Tiles").get_child_count() > 0, "%s has no tile layers" % path)
		# Tilesets are shared files, not embedded copies.
		for holder in level.get_node("Tiles").get_children():
			for map in holder.get_children():
				_expect(map.tile_set.resource_path.begins_with(out.path_join("tilesets/")),
					"%s: tileset embedded (%s)" % [path, map.tile_set.resource_path])
		level.free()
	for w in b.world["worlds"]:
		var world: Node = load(out.path_join("%s.tscn" % w["slug"])).instantiate()
		_expect(world.get_child_count() == b.world_level_slugs(w["slug"]).size(), "world %s levels" % w["slug"])
		world.free()


func _snapshot(dir: String) -> Dictionary:
	var out := {}
	for sub in ["", "levels", "tilesets"]:
		var d := dir.path_join(sub)
		for f in DirAccess.get_files_at(d):
			out[sub.path_join(f)] = FileAccess.get_file_as_string(d.path_join(f))
	return out


func _wipe(dir: String) -> void:
	for sub in ["levels", "tilesets", ""]:
		var d := dir.path_join(sub)
		if DirAccess.dir_exists_absolute(d):
			for f in DirAccess.get_files_at(d):
				DirAccess.remove_absolute(d.path_join(f))


func _count(d: Dictionary, k: String) -> void:
	d[k] = d.get(k, 0) + 1


func _expect(ok: bool, msg: String) -> void:
	checks += 1
	if not ok:
		_fail(msg)


func _fail(msg: String) -> void:
	failures += 1
	printerr("FAIL: " + msg)
