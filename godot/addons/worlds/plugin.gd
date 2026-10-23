@tool
extends EditorPlugin
## Project > Tools > Import Worlds Export... picks an export's `world.json`
## and writes scenes (see worlds_import.gd). Reimport Worlds Export runs it
## again on the same export. Settings live under Project Settings >
## worlds/import (enable Advanced Settings to see them).

const IMPORT := "Import Worlds Export..."
const REIMPORT := "Reimport Worlds Export"

const SETTINGS := {
	"worlds/import/world_json": ["", PROPERTY_HINT_FILE, "*.json"],
	"worlds/import/output_dir": ["res://worlds", PROPERTY_HINT_DIR, ""],
	"worlds/import/entity_scene_dir": ["", PROPERTY_HINT_DIR, ""],
	"worlds/import/collision": [true, PROPERTY_HINT_NONE, ""],
}

var _dialog: EditorFileDialog


func _enter_tree() -> void:
	for key in SETTINGS:
		var spec: Array = SETTINGS[key]
		if not ProjectSettings.has_setting(key):
			ProjectSettings.set_setting(key, spec[0])
		ProjectSettings.set_initial_value(key, spec[0])
		ProjectSettings.add_property_info({
			"name": key, "type": typeof(spec[0]), "hint": spec[1], "hint_string": spec[2],
		})
	add_tool_menu_item(IMPORT, _pick)
	add_tool_menu_item(REIMPORT, _reimport)


func _exit_tree() -> void:
	remove_tool_menu_item(IMPORT)
	remove_tool_menu_item(REIMPORT)
	if _dialog != null:
		_dialog.queue_free()


func _pick() -> void:
	if _dialog == null:
		_dialog = EditorFileDialog.new()
		_dialog.file_mode = EditorFileDialog.FILE_MODE_OPEN_FILE
		_dialog.access = EditorFileDialog.ACCESS_FILESYSTEM
		_dialog.title = "Pick the export's world.json"
		_dialog.add_filter("world.json", "Worlds export index")
		_dialog.file_selected.connect(_on_picked)
		EditorInterface.get_base_control().add_child(_dialog)
	_dialog.popup_file_dialog()


func _on_picked(path: String) -> void:
	ProjectSettings.set_setting("worlds/import/world_json", ProjectSettings.localize_path(path))
	ProjectSettings.save()
	_import(path)


func _reimport() -> void:
	var path: String = ProjectSettings.get_setting("worlds/import/world_json", "")
	if path == "":
		_pick()
	else:
		_import(path)


func _import(path: String) -> void:
	var out: String = ProjectSettings.get_setting("worlds/import/output_dir", "res://worlds")
	var errors := WorldsImport.run(
		path,
		out,
		ProjectSettings.get_setting("worlds/import/entity_scene_dir", ""),
		ProjectSettings.get_setting("worlds/import/collision", true),
	)
	EditorInterface.get_resource_filesystem().scan()
	for e in errors:
		push_warning("Worlds import: " + e)
	if errors.is_empty():
		print("Worlds: imported %s into %s" % [path, out])
	else:
		push_error("Worlds: imported %s into %s with %d problem(s); see the warnings above" % [path, out, errors.size()])
