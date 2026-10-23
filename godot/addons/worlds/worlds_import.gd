@tool
class_name WorldsImport
extends RefCounted
## Turns a Worlds export into scenes:
##
##     <out_dir>/tilesets/<tileset>.tres   one TileSet per tileset, shared
##     <out_dir>/levels/<level>.tscn       one scene per level
##     <out_dir>/<world>.tscn              each world, instancing its levels
##
## Every run rewrites these files, so don't edit them: instance a level scene
## in a scene of your own and add to that. Level scenes left over from levels
## that no longer exist are removed.


## Imports the export whose `world.json` is at `world_json`. Returns the
## problems met (empty when everything imported).
static func run(world_json: String, out_dir: String, entity_scene_dir := "", build_collision := true) -> PackedStringArray:
	var b := WorldsBuilder.open(world_json.get_base_dir())
	if b == null:
		return PackedStringArray(["can't read %s" % world_json])
	b.entity_scene_dir = entity_scene_dir
	b.build_collision = build_collision

	for sub in ["", "levels", "tilesets"]:
		var dir := out_dir.path_join(sub)
		if not DirAccess.dir_exists_absolute(dir):
			DirAccess.make_dir_recursive_absolute(dir)

	var levels := {}
	for slug in b.level_slugs():
		var level := b.build_level(slug)
		if level != null:
			levels[slug] = level

	# Tilesets first, so the level scenes reference the saved files instead
	# of embedding copies.
	var tilesets := b.tilesets()
	for name in tilesets:
		var ts: TileSet = tilesets[name]
		if ts == null:
			continue
		var path := out_dir.path_join("tilesets/%s.tres" % name)
		if _write(ts, path, b.errors):
			ts.take_over_path(path)

	var saved := {}
	for slug in levels:
		var path := out_dir.path_join("levels/%s.tscn" % slug)
		if _save(levels[slug], path, b.errors):
			saved[slug] = path
		levels[slug].free()

	for w in b.world.get("worlds", []):
		var root := Node2D.new()
		root.name = w["slug"]
		root.set_meta("worlds_notes", w.get("notes", []))
		for slug in b.world_level_slugs(w["slug"]):
			if not saved.has(slug):
				continue
			var inst: Node2D = load(saved[slug]).instantiate(PackedScene.GEN_EDIT_STATE_INSTANCE)
			root.add_child(inst, true)
			inst.owner = root
		_save(root, out_dir.path_join("%s.tscn" % w["slug"]), b.errors)
		root.free()

	_remove_stale(out_dir.path_join("levels"), saved)
	return b.errors


static func _save(node: Node, path: String, errors: PackedStringArray) -> bool:
	var scene := PackedScene.new()
	var err := scene.pack(node)
	if err != OK:
		errors.append("can't pack %s (error %d)" % [path, err])
		return false
	if not _write(scene, path, errors):
		return false
	# An open copy of an older version must not shadow the new file.
	scene.take_over_path(path)
	return true


## Saves `res` to `path` unless the file there already says the same. Godot
## gives nodes and sub-resources fresh random ids on every save, so an
## unchanged export would otherwise rewrite every file; comparing with the
## ids renumbered keeps a reimport of the same export from touching any.
static func _write(res: Resource, path: String, errors: PackedStringArray) -> bool:
	var tmp := "%s.tmp.%s" % [path.get_basename(), path.get_extension()]
	var err := ResourceSaver.save(res, tmp)
	if err != OK:
		errors.append("can't save %s (error %d)" % [path, err])
		return false
	var fresh := FileAccess.get_file_as_string(tmp)
	if FileAccess.file_exists(path) and _canonical(FileAccess.get_file_as_string(path)) == _canonical(fresh):
		DirAccess.remove_absolute(tmp)
		return true
	if FileAccess.file_exists(path):
		DirAccess.remove_absolute(path)  # rename won't replace a file on Windows
	err = DirAccess.rename_absolute(tmp, path)
	if err != OK:
		errors.append("can't replace %s (error %d)" % [path, err])
		return false
	return true


## The file with its random ids replaced by their order of appearance.
static func _canonical(text: String) -> String:
	var re := RegEx.create_from_string("(unique_id=|uid=\"|id=\"|Resource\\(\")([^\")\\s]+)")
	var ids := {}
	var parts := PackedStringArray()
	var last := 0
	for m in re.search_all(text):
		var id := m.get_string(2)
		if not ids.has(id):
			ids[id] = ids.size()
		parts.append(text.substr(last, m.get_start(2) - last))
		parts.append("#%d" % ids[id])
		last = m.get_end(2)
	parts.append(text.substr(last))
	return "".join(parts)


static func _remove_stale(dir: String, saved: Dictionary) -> void:
	for file in DirAccess.get_files_at(dir):
		if file.get_extension() == "tscn" and not saved.has(file.get_basename()):
			DirAccess.remove_absolute(dir.path_join(file))
