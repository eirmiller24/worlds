//! Every complete example file in docs/format.md must parse as its kind.

use std::path::Path;

use worlds_core::format::{self, EntitiesFile, EnumsFile, LayersFile, NotesFile, TilesetsFile};
use worlds_core::*;

#[test]
fn format_md_examples_parse() {
    let doc_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/format.md");
    let doc = std::fs::read_to_string(&doc_path).unwrap();
    let mut checked = Vec::new();
    for block in doc.split("```json\n").skip(1) {
        let text = block.split("\n```").next().unwrap();
        let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
            continue;
        };
        let Some(schema) = value.get("$schema").and_then(|s| s.as_str()) else {
            continue;
        };
        let kind = schema
            .rsplit('/')
            .next()
            .unwrap()
            .trim_end_matches(".json")
            .to_string();
        let path = Path::new("docs/format.md");
        let r = match kind.as_str() {
            "project" => format::parse::<Project>(path, text).map(drop),
            "layers" => format::parse::<LayersFile>(path, text).map(drop),
            "tilesets" => format::parse::<TilesetsFile>(path, text).map(drop),
            "enums" => format::parse::<EnumsFile>(path, text).map(drop),
            "entity" => format::parse::<EntityDef>(path, text).map(drop),
            "rules" => format::parse::<RuleSet>(path, text).map(drop),
            "macros" => format::parse::<Macros>(path, text).map(drop),
            "world" => format::parse::<World>(path, text).map(drop),
            "level" => format::parse::<Level>(path, text).map(drop),
            "intgrid" => format::parse::<format::IntGridFile>(path, text).map(drop),
            "tiles" => format::parse::<TilesLayer>(path, text).map(drop),
            "entities" => format::parse::<EntitiesFile>(path, text).map(drop),
            "notes" => format::parse::<NotesFile>(path, text).map(drop),
            other => panic!("unknown schema kind {other}"),
        };
        r.unwrap_or_else(|e| panic!("example for {kind} doesn't parse: {e}\n{text}"));
        checked.push(kind);
    }
    checked.sort();
    checked.dedup();
    assert_eq!(
        checked.len(),
        format::FileKind::ALL.len(),
        "examples found for {checked:?}"
    );
}

/// Every command example in docs/commands.md parses, and in order they all
/// apply to the mini fixture; every command kind has an example; every
/// change example parses.
#[test]
fn commands_md_examples_apply_in_order() {
    use worlds_core::command::{Change, Command};
    let doc_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/commands.md");
    let doc = std::fs::read_to_string(&doc_path).unwrap();
    let mut project =
        load(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mini.worlds")).unwrap();
    let mut ops = std::collections::BTreeSet::new();
    let mut changes = 0;
    for block in doc.split("```json\n").skip(1) {
        let text = block.split("\n```").next().unwrap();
        let value: serde_json::Value =
            serde_json::from_str(text).unwrap_or_else(|e| panic!("{e}\n{text}"));
        if let Some(op) = value.get("op").and_then(|o| o.as_str()) {
            let cmd: Command = serde_json::from_value(value.clone())
                .unwrap_or_else(|e| panic!("{op} example doesn't parse: {e}\n{text}"));
            let applied = project
                .apply(cmd)
                .unwrap_or_else(|e| panic!("{op} example doesn't apply: {e}\n{text}"));
            assert!(!applied.changes.is_empty(), "{op} example changes nothing");
            ops.insert(op.to_string());
        } else {
            let _: Change = serde_json::from_value(value)
                .unwrap_or_else(|e| panic!("change example doesn't parse: {e}\n{text}"));
            changes += 1;
        }
    }
    let all = [
        "paint_cells",
        "fill_rect",
        "flood_fill",
        "replace_value",
        "set_tiles",
        "clear_tiles",
        "create_level",
        "delete_level",
        "replace_level",
        "move_level",
        "resize_level",
        "rename_level",
        "set_level_field",
        "set_level_depth",
        "set_level_bg",
        "set_level_tags",
        "add_entity",
        "remove_entity",
        "move_entity",
        "resize_entity",
        "set_entity_field",
        "add_note",
        "remove_note",
        "update_note",
        "set_guide_group",
        "remove_guide_group",
        "set_layer_state",
        "set_project_setting",
        "set_layer_def",
        "rename_layer",
        "remove_layer_def",
        "set_entity_def",
        "rename_entity_type",
        "remove_entity_def",
        "set_entity_field_def",
        "rename_entity_field",
        "remove_entity_field_def",
        "set_level_field_def",
        "rename_level_field",
        "remove_level_field_def",
        "set_enum_def",
        "rename_enum",
        "rename_enum_value",
        "remove_enum_def",
        "set_tileset_def",
        "rename_tileset",
        "remove_tileset_def",
        "set_rule_group",
        "remove_rule_group",
        "set_rule",
        "remove_rule",
        "move_rule",
        "set_macro",
        "remove_macro",
        "rename_macro",
        "set_stamp",
        "remove_stamp",
        "batch",
        "duplicate_level",
        "create_world",
        "rename_world",
        "remove_world",
        "reorder_worlds",
        "move_level_to_world",
        "duplicate_layer",
        "add_reference",
        "update_reference",
        "remove_reference",
    ];
    let missing: Vec<&str> = all.iter().copied().filter(|o| !ops.contains(*o)).collect();
    assert!(missing.is_empty(), "no example for {missing:?}");
    // The list above is complete: the schema has exactly these ops.
    let schema = serde_json::to_value(schemars::schema_for!(Command)).unwrap();
    let mut variants: Vec<&str> = schema["oneOf"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["properties"]["op"]["const"].as_str().unwrap())
        .collect();
    variants.sort();
    let mut expected = all.to_vec();
    expected.sort();
    assert_eq!(variants, expected);
    assert!(changes >= 3);
    assert!(
        validate(&project)
            .iter()
            .all(|p| !p.to_string().contains("wing")),
        "{:?}",
        validate(&project)
    );
}

/// Rule and group fragments in docs/format.md (objects with a `slug` and a
/// `match`, `region` or `rules`) parse as what they show.
#[test]
fn format_md_rule_fragments_parse() {
    let doc_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/format.md");
    let doc = std::fs::read_to_string(&doc_path).unwrap();
    let mut rules = 0;
    let mut groups = 0;
    for block in doc.split("```json\n").skip(1) {
        let text = block.split("\n```").next().unwrap();
        let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
            continue;
        };
        if value.get("$schema").is_some() || value.get("slug").is_none() {
            continue;
        }
        if value.get("match").is_some() || value.get("region").is_some() {
            serde_json::from_value::<RuleEntry>(value.clone())
                .unwrap_or_else(|e| panic!("rule example doesn't parse: {e}\n{text}"));
            rules += 1;
        } else if value.get("rules").is_some() {
            serde_json::from_value::<RuleGroup>(value.clone())
                .unwrap_or_else(|e| panic!("group example doesn't parse: {e}\n{text}"));
            groups += 1;
        }
    }
    assert!(rules >= 3 && groups >= 1, "{rules} rules, {groups} groups");
}
