//! LDtk import: every fixture imports, validates clean, survives a save/load
//! round trip, and keeps every baked autotile in the reference.

use std::path::{Path, PathBuf};

use worlds_core::ldtk;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

/// Every fixture. `Test_file_for_API_showing_all_features.ldtk` mixes 8 px
/// and 16 px layers (see `mixed_grids`).
const FIXTURES: &[&str] = &[
    "ldtk-samples/Test_file_for_API_showing_all_features.ldtk",
    "ldtk-samples/AutoLayers_1_basic.ldtk",
    "ldtk-samples/AutoLayers_2_stamps.ldtk",
    "ldtk-samples/AutoLayers_3_Mosaic.ldtk",
    "ldtk-samples/AutoLayers_4_Assistant.ldtk",
    "ldtk-samples/AutoLayers_5_Advanced.ldtk",
    "ldtk-samples/AutoLayers_6_OptionalRules.ldtk",
    "ldtk-samples/AutoLayers_7_Biomes.ldtk",
    "ldtk-samples/Entities.ldtk",
];

/// `autoLayerTiles` entries in the raw file, counted independently of the importer.
fn source_autotiles(path: &Path) -> usize {
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let mut levels: Vec<&serde_json::Value> = Vec::new();
    levels.extend(v["levels"].as_array().into_iter().flatten());
    for w in v["worlds"].as_array().into_iter().flatten() {
        levels.extend(w["levels"].as_array().into_iter().flatten());
    }
    levels
        .iter()
        .flat_map(|l| l["layerInstances"].as_array().into_iter().flatten())
        .map(|li| li["autoLayerTiles"].as_array().map_or(0, Vec::len))
        .sum()
}

fn import_one(rel: &str) -> (ldtk::Imported, tempfile::TempDir, PathBuf) {
    let src = fixtures().join(rel);
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("imported.worlds");
    let imported = ldtk::import(&src, &out).unwrap_or_else(|e| panic!("{rel}: {e}"));
    (imported, tmp, out)
}

#[test]
fn fixtures_import_validate_and_round_trip() {
    for rel in FIXTURES {
        let (imported, _tmp, out) = import_one(rel);
        let problems = worlds_core::validate(&imported.project);
        assert!(
            problems.is_empty(),
            "{rel}: validation problems:\n{}",
            problems
                .iter()
                .map(|p| p.to_string())
                .collect::<Vec<_>>()
                .join("\n")
        );

        ldtk::write(&imported, &out).unwrap();
        assert!(
            out.join("schema/project.json").is_file(),
            "{rel}: schema/ written"
        );
        let loaded = worlds_core::load(&out).unwrap_or_else(|e| panic!("{rel}: {e}"));
        let mut expected = imported.project.clone();
        expected.normalize();
        assert_eq!(loaded, expected, "{rel}: save/load round trip");

        let src = fixtures().join(rel);
        assert_eq!(
            imported.reference.autotile_count(),
            source_autotiles(&src),
            "{rel}: reference autotile count"
        );
    }
}

#[test]
fn every_tileset_image_resolves() {
    // Sample atlases ship with the fixtures, so their relative paths must resolve.
    for rel in FIXTURES.iter().filter(|r| r.starts_with("ldtk-samples")) {
        let (imported, _tmp, out) = import_one(rel);
        ldtk::write(&imported, &out).unwrap();
        for t in &imported.project.tilesets {
            let img = out.join(&t.image);
            assert!(img.is_file(), "{rel}: {} -> {}", t.slug, img.display());
        }
    }
}

#[test]
fn images_point_next_to_the_project() {
    // Tileset paths are relative to the .ldtk file; the project's are
    // relative to the project directory, here one level down.
    let src = fixtures().join("ldtk-samples/AutoLayers_1_basic.ldtk");
    let imported = ldtk::import(&src, &fixtures().join("ldtk-samples/game.worlds")).unwrap();
    let ts = imported
        .project
        .tileset("cavernas_by_adam_saltsman")
        .unwrap();
    assert_eq!(ts.image, "../atlas/Cavernas_by_Adam_Saltsman.png");
}

#[test]
fn mixed_grids() {
    let (imported, _tmp, _out) =
        import_one("ldtk-samples/Test_file_for_API_showing_all_features.ldtk");
    let p = &imported.project;
    // Levels like 680x568 px aren't whole 16 px cells, so the project grid
    // is 8 px and the 16 px layers keep their own grid.
    assert_eq!(p.grid_size, 8);
    let grid = |s: &str| p.layer(s).unwrap().grid_size;
    assert_eq!(grid("int_grid_8px_grid"), None);
    assert_eq!(grid("int_grid_with_rules"), Some(16));
    assert_eq!(grid("entities"), Some(16));
    // An auto layer never sets one: it uses its source's.
    assert_eq!(grid("pure_auto_layer"), None);
    assert_eq!(p.layer_grid(p.layer("pure_auto_layer").unwrap()), 16);
    let everything = p.level("everything").unwrap();
    assert_eq!(everything.size, [85, 71]);
    let def = p.layer("int_grid_with_rules").unwrap();
    assert_eq!(p.layer_cells(def, everything), [43, 36]);
    if let Some(g) = everything.int_grid("int_grid_with_rules") {
        assert_eq!(g.size(), [43, 36]);
    }

    // Grids where neither divides the other are refused.
    let src = fixtures().join("ldtk-samples/Test_file_for_API_showing_all_features.ldtk");
    let mut v: serde_json::Value = serde_json::from_slice(&std::fs::read(&src).unwrap()).unwrap();
    for l in v["defs"]["layers"].as_array_mut().unwrap() {
        if l["identifier"] == "IntGrid_8px_grid" {
            l["gridSize"] = 12.into();
        }
    }
    let tmp = tempfile::tempdir().unwrap();
    let bad = tmp.path().join("bad.ldtk");
    std::fs::write(&bad, serde_json::to_vec(&v).unwrap()).unwrap();
    let err = ldtk::import(&bad, Path::new("out.worlds"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("neither divides the other"), "{err}");
}

#[test]
fn group_modes_and_glyphs() {
    let (imported, _tmp, _out) = import_one("ldtk-samples/AutoLayers_6_OptionalRules.ldtk");
    let modes: Vec<worlds_core::GroupMode> = imported
        .project
        .rules
        .iter()
        .flat_map(|r| r.groups.iter().map(|g| g.mode))
        .collect();
    assert!(
        modes.contains(&worlds_core::GroupMode::Optional),
        "{modes:?}"
    );

    // A platformer's collision values get the readable glyphs.
    let imported = import_collision_layer();
    let def = imported.project.layer("collision").unwrap();
    let glyphs: Vec<(String, char)> = def
        .values()
        .iter()
        .map(|v| (v.slug.to_string(), v.glyph))
        .collect();
    for (slug, glyph) in [
        ("solid", '#'),
        ("oneway", '-'),
        ("water", '~'),
        ("hazard", 'x'),
        ("breakable", 'b'),
        ("fortified", 'f'),
        ("ramp_r", '/'),
        ("ramp_l", 'L'),
    ] {
        assert!(glyphs.contains(&(slug.to_string(), glyph)), "{glyphs:?}");
    }
}

/// `AutoLayers_5_Advanced` with its IntGrid layer renamed `Collision` and
/// given a platformer's values (in this order).
fn import_collision_layer() -> ldtk::Imported {
    import_edited("ldtk-samples/AutoLayers_5_Advanced.ldtk", |v| {
        for l in v["defs"]["layers"].as_array_mut().unwrap() {
            if l["identifier"] == "IntGrid_layer" {
                l["identifier"] = "Collision".into();
                let values = l["intGridValues"].as_array_mut().unwrap();
                let template = values[0].clone();
                values.clear();
                for (i, id) in [
                    "solid",
                    "oneway",
                    "water",
                    "hazard",
                    "breakable",
                    "fortified",
                    "ramp_r",
                    "ramp_l",
                ]
                .into_iter()
                .enumerate()
                {
                    let mut val = template.clone();
                    val["value"] = (i + 1).into();
                    val["identifier"] = id.into();
                    values.push(val);
                }
            }
        }
    })
}

#[test]
fn sample_mapping() {
    let (imported, _tmp, out) = import_one("ldtk-samples/AutoLayers_5_Advanced.ldtk");
    let p = &imported.project;
    assert_eq!(p.grid_size, 8);
    assert_eq!(p.levels().count(), 1);

    // Bottom first: LDtk lists the IntGrid layer first (top). Its rules
    // become an auto layer right above it.
    let order: Vec<&str> = p.layers.iter().map(|l| l.slug.as_str()).collect();
    assert_eq!(order, ["sky", "int_grid_layer", "int_grid_layer_tiles"]);
    assert_eq!(
        p.layer("int_grid_layer_tiles")
            .unwrap()
            .auto()
            .unwrap()
            .source,
        "int_grid_layer"
    );
    let rules = p.rules_for("int_grid_layer_tiles").unwrap();
    let n: usize = rules.groups.iter().map(|g| g.rules.len()).sum();
    assert_eq!(n, 43);
    // Imported rules keep their LDtk uid as the salt.
    let r = rules.groups[0].rules[0].as_rule().unwrap();
    assert_eq!(r.slug, "r_103");
    assert_eq!(r.salt, 103);

    let level = p.level("auto_layers_advanced_demo").unwrap();
    assert_eq!(level.size, [37, 38]);
    assert!(level.layer_seed("int_grid_layer_tiles") != 0);
    assert!(
        imported
            .report
            .renamed
            .contains(&("IntGrid_layer".into(), "int_grid_layer".into()))
    );

    ldtk::write(&imported, &out).unwrap();
    let level_dir = out.join("worlds/main/levels/auto_layers_advanced_demo");
    insta::assert_snapshot!(
        "sample_int_grid",
        std::fs::read_to_string(level_dir.join("int_grid_layer.json")).unwrap()
    );
    insta::assert_snapshot!(
        "sample_rules",
        std::fs::read_to_string(out.join("rules/int_grid_layer_tiles.json")).unwrap()
    );
}

/// Import `rel` after `edit` changes its JSON.
fn import_edited(rel: &str, edit: impl FnOnce(&mut serde_json::Value)) -> ldtk::Imported {
    let src = fixtures().join(rel);
    let mut v: serde_json::Value = serde_json::from_slice(&std::fs::read(&src).unwrap()).unwrap();
    edit(&mut v);
    // Next to the original, so relative paths still resolve.
    let tmp = tempfile::Builder::new()
        .prefix(".edited")
        .suffix(".ldtk")
        .tempfile_in(src.parent().unwrap())
        .unwrap();
    std::fs::write(tmp.path(), serde_json::to_vec(&v).unwrap()).unwrap();
    ldtk::import(tmp.path(), &src.parent().unwrap().join("out.worlds")).unwrap()
}

#[test]
fn array_fields_at_element_defaults_keep_their_values() {
    // SpotLight `colors` is an Array<Color>; one light has `["#FFEF85"]`
    // with realEditorValues `[null]` (the element is at its default).
    let (imported, _tmp, _out) = import_one("ldtk-samples/Entities.ldtk");
    let lights: Vec<&serde_json::Value> = imported
        .project
        .levels()
        .flat_map(|l| l.all_entities())
        .filter(|e| e.ty == "spot_light")
        .map(|e| &e.fields["colors"])
        .collect();
    assert!(
        lights.contains(&&serde_json::json!(["#FFEF85"])),
        "{lights:?}"
    );
    assert!(
        lights.contains(&&serde_json::json!(["#FF7D1A", "#B20101"])),
        "{lights:?}"
    );
}

#[test]
fn tile_rects_account_for_padding_and_spacing() {
    // Give the Door entity an icon from a padded, spaced tileset.
    let imported = import_edited("ldtk-samples/Entities.ldtk", |v| {
        for t in v["defs"]["tilesets"].as_array_mut().unwrap() {
            if t["uid"] == 126 {
                t["padding"] = 8.into();
                t["spacing"] = 4.into();
            }
        }
        for e in v["defs"]["entities"].as_array_mut().unwrap() {
            if e["identifier"] == "Door" {
                // Tile [5, 3] is at padding + n * (16 + spacing) = (108, 68),
                // which a plain `px / 16` would read as [6, 4].
                e["tileRect"] = serde_json::json!({
                    "tilesetUid": 126, "x": 8 + 5 * 20, "y": 8 + 3 * 20, "w": 16, "h": 16
                });
            }
        }
    });
    let door = imported.project.entity_def("door").unwrap();
    let icon = door.icon.as_ref().unwrap();
    assert_eq!(icon.tileset, "inca_front_by_kronbits_extended");
    assert_eq!(icon.tile, [5, 3]);
}

#[test]
fn path_fields_are_rebased_to_the_project_dir() {
    // `FilePath` holds "README.md", relative to the .ldtk file.
    let src = fixtures().join("ldtk-samples/Test_file_for_API_showing_all_features.ldtk");
    let out = fixtures().join("ldtk-samples/sub/game.worlds");
    let imported = ldtk::import(&src, &out).unwrap();
    let values: Vec<&serde_json::Value> = imported
        .project
        .levels()
        .flat_map(|l| l.all_entities())
        .filter_map(|e| e.fields.get("file_path"))
        .collect();
    assert!(!values.is_empty());
    for v in values {
        assert_eq!(v, "../../README.md");
    }
}

#[test]
fn collision_layer_is_the_layer_named_collision() {
    let imported = import_collision_layer();
    assert_eq!(
        imported
            .project
            .collision_layer
            .as_ref()
            .map(|s| s.as_str()),
        Some("collision")
    );
    // The blocking kinds the layer has, in its value order.
    let blocking: Vec<&str> = imported
        .project
        .blocking_values
        .iter()
        .flatten()
        .map(|s| s.as_str())
        .collect();
    assert_eq!(
        blocking,
        ["solid", "breakable", "fortified", "ramp_r", "ramp_l"]
    );
    assert!(
        worlds_core::validate(&imported.project)
            .iter()
            .all(|p| !p.to_string().contains("blocking")),
    );
    // Nothing named collision: no guess.
    let (imported, _tmp, _out) = import_one("ldtk-samples/AutoLayers_1_basic.ldtk");
    assert_eq!(imported.project.collision_layer, None);
    assert_eq!(imported.project.blocking_values, None);
}
