mod common;

use worlds_core::format::FileKind;

#[test]
fn writes_one_schema_per_file_kind() {
    let tmp = tempfile::tempdir().unwrap();
    worlds_core::schema::write_all(tmp.path()).unwrap();
    let files = common::read_tree(&tmp.path().join("schema"));
    assert_eq!(files.len(), FileKind::ALL.len());
    for kind in FileKind::ALL {
        let text = &files[&format!("{}.json", kind.name())];
        let v: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(v["type"], "object", "{}", kind.name());
        assert_eq!(v["additionalProperties"], false, "{}", kind.name());
        assert_eq!(v["properties"]["format"]["const"], 1);
        let req = v["required"].as_array().unwrap();
        assert!(req.contains(&"$schema".into()) && req.contains(&"format".into()));
    }
}

/// Every top-level key of every saved fixture file is declared by its schema,
/// and every required key is present.
#[test]
fn schemas_match_saved_files() {
    let tmp = tempfile::tempdir().unwrap();
    worlds_core::save(&mut common::fixture(), tmp.path()).unwrap();
    worlds_core::schema::write_all(tmp.path()).unwrap();
    for (path, text) in common::read_tree(tmp.path()) {
        if path.starts_with("schema/") {
            continue;
        }
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        let schema_ref = v["$schema"].as_str().unwrap();
        let schema_path = tmp.path().join(&path).parent().unwrap().join(schema_ref);
        let schema: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&schema_path).unwrap()).unwrap();
        let props = schema["properties"].as_object().unwrap();
        for key in v.as_object().unwrap().keys() {
            assert!(
                props.contains_key(key),
                "{path}: key {key} not in {schema_ref}"
            );
        }
        for req in schema["required"].as_array().unwrap() {
            assert!(
                v.get(req.as_str().unwrap()).is_some(),
                "{path}: missing required {req}"
            );
        }
    }
}

#[test]
fn slug_and_id_patterns_are_in_schema() {
    let v = worlds_core::schema::for_kind(FileKind::Level);
    assert_eq!(v["$defs"]["Slug"]["pattern"], "^[a-z][a-z0-9_]*$");
    assert_eq!(v["$defs"]["Id"]["pattern"], "^[a-z2-7]{10}$");
}

/// The schemas are real JSON Schema: a saved file validates, and a nested
/// mistake the loader rejects fails validation too.
#[test]
fn enum_fields_take_exactly_one_source_in_the_schema() {
    let schema = worlds_core::schema::for_kind(FileKind::Entity);
    let validator = jsonschema::validator_for(&schema).unwrap();
    let doc = |field: serde_json::Value| {
        serde_json::json!({
            "$schema": "../schema/entity.json", "format": 1,
            "slug": "door", "name": "Door", "size": [1, 1], "color": "#808080",
            "fields": [field]
        })
    };
    let ok = [
        serde_json::json!({ "slug": "facing", "type": "enum", "enum": "facing" }),
        serde_json::json!({ "slug": "style", "type": "enum", "values": ["wood", "iron"] }),
    ];
    for f in ok {
        let errors: Vec<String> = validator
            .iter_errors(&doc(f.clone()))
            .map(|e| e.to_string())
            .collect();
        assert!(errors.is_empty(), "{f}: {errors:?}");
    }
    let bad = [
        serde_json::json!({ "slug": "x", "type": "enum", "enum": "facing", "values": ["a"] }),
        serde_json::json!({ "slug": "x", "type": "enum" }),
        serde_json::json!({ "slug": "x", "type": "bool", "values": ["a"] }),
    ];
    for f in bad {
        assert!(!validator.is_valid(&doc(f.clone())), "{f} should fail");
    }

    // And every file the fixture saves validates against its schema.
    let tmp = tempfile::tempdir().unwrap();
    worlds_core::save(&mut common::fixture(), tmp.path()).unwrap();
    for (path, text) in common::read_tree(tmp.path()) {
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        let kind = FileKind::ALL
            .into_iter()
            .find(|k| {
                v["$schema"]
                    .as_str()
                    .unwrap()
                    .ends_with(&format!("/{}.json", k.name()))
            })
            .unwrap();
        let validator = jsonschema::validator_for(&worlds_core::schema::for_kind(kind)).unwrap();
        let errors: Vec<String> = validator.iter_errors(&v).map(|e| e.to_string()).collect();
        assert!(errors.is_empty(), "{path}: {errors:?}");
    }
}

/// M5 keys: `guide_groups` and a guide's `group` in world.json, `tags` in
/// level.json; `group` on another kind is refused by the schema too.
#[test]
fn guide_groups_and_level_tags_are_in_the_schemas() {
    let world = jsonschema::validator_for(&worlds_core::schema::for_kind(FileKind::World)).unwrap();
    let doc = |note: serde_json::Value| {
        serde_json::json!({
            "$schema": "../../schema/world.json", "format": 1, "slug": "main",
            "guide_groups": [{ "slug": "ruined", "name": "Ruined wing", "offset": [0, 769], "color": "#c08040" }],
            "notes": [note]
        })
    };
    let guide = serde_json::json!({ "id": "guideaaaab", "kind": "guide", "locked": true,
        "points": [[0, 0], [0, 10]], "group": "ruined" });
    let errors: Vec<String> = world
        .iter_errors(&doc(guide))
        .map(|e| e.to_string())
        .collect();
    assert!(errors.is_empty(), "{errors:?}");
    let pin =
        serde_json::json!({ "id": "pinstatue2", "kind": "pin", "px": [0, 0], "group": "ruined" });
    assert!(!world.is_valid(&doc(pin)));
    let level = jsonschema::validator_for(&worlds_core::schema::for_kind(FileKind::Level)).unwrap();
    let l = serde_json::json!({ "$schema": "../../../../schema/level.json", "format": 1,
        "id": "crevasse22", "slug": "crevasse", "name": "Crevasse", "world_px": [0, 0],
        "size": [20, 5], "depth": 0, "tags": ["forest"] });
    assert!(level.is_valid(&l));
    // And the loader agrees on `group` outside a guide.
    let e = worlds_core::format::parse::<worlds_core::format::NotesFile>(
        std::path::Path::new("notes.json"),
        r#"{ "$schema": "x", "format": 1, "notes": [{ "id": "pinstatue2", "kind": "pin", "px": [0, 0], "group": "ruined" }] }"#,
    )
    .unwrap_err();
    assert!(
        e.to_string()
            .contains("\"group\" is not allowed on pin annotations"),
        "{e}"
    );
}

/// Inside a rules file entry (a rule or a template reference, and the
/// pattern, region, place and output objects in it) `null` is never a
/// value: the schema and the loader both refuse it on every key, the older
/// optional keys (`modulo`, `perlin`, `out_of_bounds`, `tileset`, ...)
/// included. Group keys with no default (`name`, `color`, `icon`,
/// `requires_*`) take `null` for "left out" in both.
#[test]
fn rules_schema_and_loader_agree_on_null() {
    use serde_json::{Value, json};
    let validator =
        jsonschema::validator_for(&worlds_core::schema::for_kind(FileKind::Rules)).unwrap();
    let file = |entry: Value, group: Value| {
        let mut g = json!({ "slug": "g", "rules": [entry] });
        for (k, v) in group.as_object().unwrap() {
            g[k] = v.clone();
        }
        json!({ "$schema": "../schema/rules.json", "format": 1, "layer": "auto", "groups": [g] })
    };
    let check = |name: &str, doc: Value, ok: bool| {
        let schema = validator.is_valid(&doc);
        let loader = worlds_core::format::parse::<worlds_core::RuleSet>(
            std::path::Path::new("auto.json"),
            &doc.to_string(),
        );
        assert_eq!(
            schema,
            loader.is_ok(),
            "{name}: schema {schema}, loader {loader:?}"
        );
        assert_eq!(schema, ok, "{name}: schema {schema}");
    };
    let pat = json!({ "slug": "r", "match": { "grid": "g", "rows": ["#"] },
        "out": { "tileset": "t", "tiles": [[0, 0]], "random_offset_px": { "x": [0, 1], "y": [0, 0] } } });
    let reg = json!({ "slug": "r", "region": { "grid": "g", "cells": ".", "min_width": 2 },
        "place": { "along": "floor", "every": 2, "margin": 1, "min_length": 2, "against": "solid" },
        "out": { "stamp": "s" } });
    let tref = json!({ "use": "blob47", "slug": "moss", "tileset": "t" });
    check("pattern rule", file(pat.clone(), json!({})), true);
    check("region rule", file(reg.clone(), json!({})), true);
    check("template reference", file(tref.clone(), json!({})), true);
    let null_at = |base: &Value, path: &[&str]| {
        let mut v = base.clone();
        let mut at = &mut v;
        for k in &path[..path.len() - 1] {
            at = &mut at[*k];
        }
        at[path[path.len() - 1]] = Value::Null;
        v
    };
    for k in [
        "active",
        "match",
        "require",
        "symmetry",
        "out",
        "write",
        "chance",
        "break",
        "modulo",
        "checker",
        "perlin",
        "out_of_bounds",
        "salt",
    ] {
        check(
            &format!("rule {k}"),
            file(null_at(&pat, &[k]), json!({})),
            false,
        );
    }
    for path in [
        &["match", "grid"][..],
        &["match", "legend"],
        &["out", "tileset"],
        &["out", "tiles"],
        &["out", "tile_mode"],
        &["out", "pivot"],
        &["out", "avoid_overlap"],
        &["out", "random_offset_px"],
    ] {
        check(&path.join("."), file(null_at(&pat, path), json!({})), false);
    }
    for k in ["region", "place"] {
        check(
            &format!("rule {k}"),
            file(null_at(&reg, &[k]), json!({})),
            false,
        );
    }
    for k in [
        "grid",
        "min_width",
        "max_width",
        "min_height",
        "max_height",
        "min_area",
        "max_area",
    ] {
        check(
            &format!("region.{k}"),
            file(null_at(&reg, &["region", k]), json!({})),
            false,
        );
    }
    for k in [
        "along",
        "against",
        "every",
        "margin",
        "min_length",
        "align",
        "spacing",
        "jitter",
    ] {
        check(
            &format!("place.{k}"),
            file(null_at(&reg, &["place", k]), json!({})),
            false,
        );
    }
    check(
        "out.stamp",
        file(null_at(&reg, &["out", "stamp"]), json!({})),
        false,
    );
    for k in [
        "slug", "tileset", "origin", "tiles", "skip", "legend", "active",
    ] {
        check(
            &format!("use.{k}"),
            file(null_at(&tref, &[k]), json!({})),
            false,
        );
    }
    for k in [
        "name",
        "color",
        "icon",
        "requires_level_field",
        "requires_level_tag",
    ] {
        check(
            &format!("group.{k}"),
            file(pat.clone(), json!({ k: null })),
            true,
        );
    }
    for k in ["mode", "pass"] {
        check(
            &format!("group.{k}"),
            file(pat.clone(), json!({ k: null })),
            false,
        );
    }
}
