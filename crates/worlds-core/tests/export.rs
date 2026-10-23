//! The game export (`export/`) written from the mini fixture: image paths,
//! the index tables, `open_edges` and field bookkeeping.

mod common;

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{Value, json};
use worlds_core::export::{self, BakedLevel};
use worlds_core::id::slug;
use worlds_core::*;

fn read(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// Every level, no autotiles (the tiles layer is enough to see images).
fn unbaked(p: &Project) -> BTreeMap<Slug, BakedLevel> {
    p.levels()
        .map(|l| (l.slug.clone(), BakedLevel::default()))
        .collect()
}

fn write(p: &Project, project_dir: &Path, export_dir: &Path) {
    export::write(p, project_dir, export_dir, &unbaked(p), true).unwrap();
}

/// Every `image` in a level file.
fn images(level: &Value) -> Vec<String> {
    level["tile_layers"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|l| l["tilesets"].as_array().unwrap())
        .map(|g| g["image"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn images_resolve_from_the_export_root() {
    // game/assets/<slug>.png, and the project two directories below game/.
    let tmp = tempfile::tempdir().unwrap();
    let game = tmp.path().join("game");
    let project_dir = game.join("data/maps/mygame.worlds");
    std::fs::create_dir_all(game.join("assets")).unwrap();
    std::fs::create_dir_all(&project_dir).unwrap();
    let mut p = common::fixture();
    for t in &mut p.tilesets {
        t.image = format!("../../../assets/{}.png", t.slug);
        std::fs::write(game.join("assets").join(format!("{}.png", t.slug)), b"").unwrap();
    }

    for (export_dir, want) in [
        // The usual place: one level below the project.
        (
            project_dir.join("export"),
            "../../../../assets/castle_main.png",
        ),
        // Somewhere else entirely.
        (
            tmp.path().join("out/deep/export"),
            "../../../game/assets/castle_main.png",
        ),
        // Next to the images (not folded further: paths are joined, not
        // resolved).
        (game.join("assets"), "../assets/castle_main.png"),
    ] {
        write(&p, &project_dir, &export_dir);
        let level = read(&export_dir.join("levels/crevasse.json"));
        let found = images(&level);
        assert!(found.iter().any(|i| i == want), "{found:?}");
        for image in found {
            assert!(
                export_dir.join(&image).is_file(),
                "{image} doesn't resolve from {}",
                export_dir.display()
            );
        }
    }
}

#[test]
fn world_index_tables() {
    let tmp = tempfile::tempdir().unwrap();
    let mut p = common::fixture();
    p.entities[0].category = Some("Puzzle/Buttons".into());
    write(&p, tmp.path(), &tmp.path().join("export"));
    let export = tmp.path().join("export");
    let world = read(&export.join("world.json"));

    assert_eq!(world["name"], "Mini");
    assert_eq!(world["grid_size"], 16);
    assert_eq!(world["files"], json!({ "enums": "enums.json" }));
    assert_eq!(
        world["entity_types"],
        json!([
            { "slug": "floor_button", "name": "Floor Button", "category": "Puzzle/Buttons", "tags": ["object", "puzzle"] },
            { "slug": "locked_door", "name": "Locked Door", "category": "Puzzle/Doors", "tags": ["object", "puzzle"] }
        ])
    );

    // Display names fall back to the slug.
    let enums = read(&export.join(world["files"]["enums"].as_str().unwrap()));
    assert_eq!(
        enums,
        json!({
            "format": 1,
            "enums": [
                { "slug": "biome", "name": "biome", "values": [
                    { "slug": "dry", "name": "dry" }, { "slug": "flooded", "name": "flooded" } ] },
                { "slug": "facing", "name": "Facing", "values": [
                    { "slug": "left", "name": "Left" }, { "slug": "right", "name": "Right" } ] }
            ]
        })
    );

    // Layer display names.
    let level = read(&export.join("levels/crevasse.json"));
    assert_eq!(level["int_grids"]["collision"]["name"], "Collision");
    let names: Vec<(&str, &str)> = level["tile_layers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| (l["layer"].as_str().unwrap(), l["name"].as_str().unwrap()))
        .collect();
    assert_eq!(
        names,
        [
            ("main_tiling", "Main tiling"),
            ("decor_back", "Decor (back)")
        ]
    );
}

#[test]
fn open_edges_skip_only_blocking_values() {
    let mut p = common::fixture();
    let def = p.layer("collision").unwrap().clone();
    let level = &mut p.worlds[0].levels[0];
    assert_eq!(level.slug, "crevasse");
    let grid = level
        .int_grids
        .iter_mut()
        .find(|g| g.layer == "collision")
        .unwrap();
    // Right edge, top to bottom: solid, oneway, fortified, water, solid.
    grid.set_value(&def, 19, 1, Some("oneway"));
    grid.set_value(&def, 19, 2, Some("fortified"));
    grid.set_value(&def, 19, 3, Some("water"));
    // Left edge: oneway at row 2, ramp_l at row 3 (not listed as blocking here).
    grid.set_value(&def, 0, 2, Some("oneway"));
    grid.set_value(&def, 0, 3, Some("ramp_l"));
    p.blocking_values = Some(vec![slug("solid"), slug("fortified")]);
    assert!(validate(&p).is_empty(), "{:?}", validate(&p));

    let world: Value = serde_json::from_str(&export::world_json(&p)).unwrap();
    assert_eq!(world["blocking_values"], json!(["solid", "fortified"]));
    let edges = &world["worlds"][0]["levels"][0]["open_edges"];
    assert_eq!(edges["right"], json!([[1, 2], [3, 4]]));
    assert_eq!(edges["left"], json!([[2, 4]]));
    assert_eq!(edges["top"], json!([]));

    // Absent: the default kinds the layer has (solid, fortified, ramp_l).
    p.blocking_values = None;
    let world: Value = serde_json::from_str(&export::world_json(&p)).unwrap();
    assert_eq!(
        world["blocking_values"],
        json!(p.default_blocking_values().unwrap())
    );
    assert!(world["blocking_values"].as_array().unwrap().len() >= 2);
    assert_eq!(
        world["worlds"][0]["levels"][0]["open_edges"]["left"],
        json!([[2, 3]])
    );

    // Explicitly nothing blocking: every border cell is open.
    p.blocking_values = Some(Vec::new());
    let world: Value = serde_json::from_str(&export::world_json(&p)).unwrap();
    assert_eq!(world["blocking_values"], json!([]));
    assert_eq!(
        world["worlds"][0]["levels"][0]["open_edges"]["top"],
        json!([[0, 20]])
    );

    // No collision layer: neither key.
    p.collision_layer = None;
    let world: Value = serde_json::from_str(&export::world_json(&p)).unwrap();
    assert!(world.get("blocking_values").is_none());
    assert!(world["worlds"][0]["levels"][0].get("open_edges").is_none());
}

#[test]
fn fields_set_and_entity_refs() {
    let mut p = common::fixture();
    // A same-level ref with no stored level, and a dangling one.
    let door = p.worlds[0].levels[0].entities[0]
        .entities
        .iter_mut()
        .find(|e| e.id.as_str() == "doorcrev22")
        .unwrap();
    door.fields.insert(slug("exit_to"), json!("btncrev222"));
    let level_json = |p: &Project, l: &str| -> Value {
        let level = p.level(l).unwrap();
        serde_json::from_str(&export::level_json(p, level, &BakedLevel::default(), "..").unwrap())
            .unwrap()
    };

    let crevasse = level_json(&p, "crevasse");
    assert_eq!(crevasse["fields_set"], json!(["biome", "music"]));
    let entities = crevasse["entities"].as_array().unwrap();
    let door = &entities[0];
    assert_eq!(
        door["fields"]["exit_to"],
        json!({ "level": "crevasse", "id": "btncrev222" }),
        "same-level refs name their level too"
    );
    let button = &entities[1];
    assert_eq!(
        button["fields_set"],
        json!(["channel", "latching", "target"])
    );
    assert_eq!(
        button["fields"]["target"],
        json!({ "level": "crevasse", "id": "doorcrev22" })
    );

    // The shaft level stores no fields; its door none either.
    let shaft = level_json(&p, "shaft");
    assert_eq!(shaft["fields_set"], json!([]));
    assert_eq!(shaft["fields"]["biome"], json!([]));
    let shaft_door = &shaft["entities"][0];
    assert_eq!(shaft_door["fields_set"], json!([]));
    // Every field is still exported, at its default.
    assert_eq!(shaft_door["fields"]["facing"], "left");
    assert_eq!(shaft_door["fields"]["exit_to"], Value::Null);

    // A dangling ref is the only way to get `level: null`.
    let door = p.worlds[0].levels[0].entities[0]
        .entities
        .iter_mut()
        .find(|e| e.id.as_str() == "doorcrev22")
        .unwrap();
    door.fields.insert(slug("exit_to"), json!("nowherexx2"));
    let crevasse = level_json(&p, "crevasse");
    assert_eq!(
        crevasse["entities"][0]["fields"]["exit_to"],
        json!({ "level": null, "id": "nowherexx2" })
    );
}

fn crevasse(p: &mut Project) -> &mut Level {
    p.worlds[0]
        .levels
        .iter_mut()
        .find(|l| l.slug == "crevasse")
        .unwrap()
}

/// Every file of a fresh export of `p`.
fn export_tree(p: &Project) -> BTreeMap<String, String> {
    let tmp = tempfile::tempdir().unwrap();
    write(p, tmp.path(), &tmp.path().join("export"));
    common::read_tree(&tmp.path().join("export"))
}

#[test]
fn guides_never_export() {
    let mut p = common::fixture();
    for n in &mut p.worlds[0].notes {
        n.export = true;
    }
    let tree = export_tree(&p);
    let world: Value = serde_json::from_str(&tree["world.json"]).unwrap();
    let ids: Vec<&str> = world["worlds"][0]["notes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["pinstatue2", "regioneast"]);
    // Exported annotations drop the editor-only flags.
    assert_eq!(
        world["worlds"][0]["notes"][1],
        json!({ "id": "regioneast", "kind": "region", "title": "Forest", "color": "#40c04080",
                "rect": [0, 0, 640, 480] })
    );
}

#[test]
fn level_tags_export_on_both_files() {
    let mut p = common::fixture();
    crevasse(&mut p).tags = vec!["forest".into(), "ruined".into()];
    let tree = export_tree(&p);
    let level = &tree["levels/crevasse.json"];
    assert!(
        level.contains("\"depth\": 0,\n  \"tags\": [\"forest\", \"ruined\"],\n  \"bg_color\""),
        "{level}"
    );
    let world = &tree["world.json"];
    assert!(
        world.contains(
            "\"depth\": 0,\n          \"tags\": [\"forest\", \"ruined\"],\n          \"touching\""
        ),
        "{world}"
    );
    // Levels without tags leave the key out.
    assert!(!tree["levels/shaft.json"].contains("\"tags\""));
    let w: Value = serde_json::from_str(world).unwrap();
    assert!(w["worlds"][0]["levels"][1].get("tags").is_none());
}

/// A project without level tags exports exactly what it did before level
/// tags and guide groups existed: guide groups, grouped guides (even
/// flagged `export`) and level tags set back to none change no byte, and
/// the index keys are the original ones.
#[test]
fn a_project_without_tags_exports_as_before() {
    let p = common::fixture();
    let plain = export_tree(&p);
    let mut q = p.clone();
    q.worlds[0].guide_groups = vec![GuideGroup {
        offset: [0, 769],
        ..GuideGroup::new(slug("ruined"))
    }];
    if let Shape::Guide { group, .. } = &mut q.worlds[0].notes[0].shape {
        *group = Some(slug("ruined"));
    }
    q.worlds[0].notes[0].export = true;
    crevasse(&mut q).tags = vec!["forest".into()];
    crevasse(&mut q).tags.clear();
    assert_eq!(export_tree(&q), plain);
    let world: Value = serde_json::from_str(&plain["world.json"]).unwrap();
    let keys: Vec<&String> = world["worlds"][0]["levels"][0]
        .as_object()
        .unwrap()
        .keys()
        .collect();
    let mut want = [
        "depth",
        "file",
        "id",
        "name",
        "open_edges",
        "rect",
        "slug",
        "touching",
    ];
    want.sort();
    assert_eq!(keys, want);
    let level: Value = serde_json::from_str(&plain["levels/crevasse.json"]).unwrap();
    assert!(level.get("tags").is_none());
}
