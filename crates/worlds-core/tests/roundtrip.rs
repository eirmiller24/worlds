mod common;

use std::fs;
use std::time::{Duration, SystemTime};

use worlds_core::*;

#[test]
fn fixture_is_valid() {
    let problems = validate(&common::fixture());
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
fn save_then_load_round_trips() {
    let project = common::fixture();
    let tmp = tempfile::tempdir().unwrap();
    save(&mut project.clone(), tmp.path()).unwrap();
    let mut loaded = load(tmp.path()).unwrap();
    assert_eq!(loaded, project);

    // And a second generation is byte-identical.
    let tmp2 = tempfile::tempdir().unwrap();
    save(&mut loaded, tmp2.path()).unwrap();
    assert_eq!(
        common::read_tree(tmp.path()),
        common::read_tree(tmp2.path())
    );
}

#[test]
fn saved_files_snapshot() {
    let tmp = tempfile::tempdir().unwrap();
    save(&mut common::fixture(), tmp.path()).unwrap();
    let mut all = String::new();
    for (path, text) in common::read_tree(tmp.path()) {
        all.push_str(&format!("=== {path}\n{text}"));
    }
    insta::assert_snapshot!("mini_worlds_saved", all);
}

#[test]
fn save_only_rewrites_changed_files_and_removes_deleted_ones() {
    let mut project = common::fixture();
    let tmp = tempfile::tempdir().unwrap();
    save(&mut project, tmp.path()).unwrap();

    // Backdate every file, then save after a one-cell edit.
    let old = SystemTime::now() - Duration::from_secs(3600);
    let files: Vec<_> = common::read_tree(tmp.path()).into_keys().collect();
    for f in &files {
        fs::File::options()
            .write(true)
            .open(tmp.path().join(f))
            .unwrap()
            .set_modified(old)
            .unwrap();
    }
    let def = project.layer("collision").unwrap().clone();
    let level = &mut project.worlds[0].levels[0];
    assert_eq!(level.slug, "crevasse");
    let grid = level
        .int_grids
        .iter_mut()
        .find(|g| g.layer == "collision")
        .unwrap();
    assert!(grid.set_value(&def, 3, 1, Some("solid")));
    save(&mut project, tmp.path()).unwrap();

    let touched: Vec<_> = files
        .iter()
        .filter(|f| {
            fs::metadata(tmp.path().join(f))
                .unwrap()
                .modified()
                .unwrap()
                > old + Duration::from_secs(1)
        })
        .cloned()
        .collect();
    assert_eq!(touched, ["worlds/main/levels/crevasse/collision.json"]);
    assert!(
        !tmp.path()
            .join("worlds/main/levels/crevasse/.collision.json.tmp")
            .exists()
    );

    // Delete a level, an entity type, notes and a tiles layer.
    project.worlds[0].levels.retain(|l| l.slug != "shaft");
    project.entities.retain(|e| e.slug != "floor_button");
    let crevasse = &mut project.worlds[0].levels[0];
    crevasse.notes.clear();
    crevasse.tiles.clear();
    save(&mut project, tmp.path()).unwrap();
    assert!(!tmp.path().join("worlds/main/levels/shaft").exists());
    assert!(!tmp.path().join("entities/floor_button.json").exists());
    assert!(
        !tmp.path()
            .join("worlds/main/levels/crevasse/notes.json")
            .exists()
    );
    assert!(
        !tmp.path()
            .join("worlds/main/levels/crevasse/decor_back.json")
            .exists()
    );
    assert_eq!(load(tmp.path()).unwrap(), project);
}

#[test]
fn unmanaged_files_survive_save() {
    let mut project = common::fixture();
    let tmp = tempfile::tempdir().unwrap();
    save(&mut project, tmp.path()).unwrap();
    fs::create_dir_all(tmp.path().join("art")).unwrap();
    fs::write(tmp.path().join("art/castle_main.png"), b"png").unwrap();
    fs::write(
        tmp.path().join("worlds/main/levels/shaft/readme.txt"),
        b"hi",
    )
    .unwrap();
    // JSON files the project never loaded, in managed directories: an
    // agent's new entity type and a level written while the editor was open.
    fs::write(tmp.path().join("entities/agent_new.json"), b"{}").unwrap();
    fs::create_dir_all(tmp.path().join("worlds/main/levels/pit")).unwrap();
    fs::write(tmp.path().join("worlds/main/levels/pit/level.json"), b"{}").unwrap();
    fs::write(
        tmp.path().join("worlds/main/levels/shaft/notes.json"),
        b"{}",
    )
    .unwrap();

    project.worlds[0].levels.retain(|l| l.slug != "shaft");
    save(&mut project, tmp.path()).unwrap();
    assert!(tmp.path().join("art/castle_main.png").exists());
    assert!(tmp.path().join("entities/agent_new.json").exists());
    assert!(
        tmp.path()
            .join("worlds/main/levels/pit/level.json")
            .exists()
    );
    // The loaded shaft files are gone; what the project never saw stays.
    let shaft = tmp.path().join("worlds/main/levels/shaft");
    assert!(!shaft.join("level.json").exists());
    assert!(!shaft.join("entities.json").exists());
    assert!(shaft.join("readme.txt").exists());
    assert!(shaft.join("notes.json").exists());
}

#[test]
fn saving_elsewhere_deletes_nothing_there() {
    // Files known from one directory don't license deletes in another.
    let tmp = tempfile::tempdir().unwrap();
    let mut a = common::fixture();
    save(&mut a, tmp.path()).unwrap();
    let mut b = common::fixture();
    b.entities.retain(|e| e.slug != "floor_button");
    save(&mut b, tmp.path()).unwrap();
    assert!(tmp.path().join("entities/floor_button.json").exists());
    // Once b has saved there itself, its own later removals do delete.
    b.worlds[0].levels.retain(|l| l.slug != "shaft");
    save(&mut b, tmp.path()).unwrap();
    assert!(!tmp.path().join("worlds/main/levels/shaft").exists());
}

#[test]
fn entities_and_notes_keep_their_order() {
    let mut p = common::fixture();
    let ids = |p: &Project| -> Vec<String> {
        let l = p.level("crevasse").unwrap();
        l.all_entities()
            .map(|e| e.id.to_string())
            .chain(l.notes.iter().map(|n| n.id.to_string()))
            .collect()
    };
    // The fixture lists the door before the button, and a note before a path.
    assert_eq!(
        ids(&p),
        ["doorcrev22", "btncrev222", "notecrev22", "pathcrev22"]
    );
    let crev = &mut p.worlds[0].levels[0];
    crev.entities[0].entities.reverse();
    crev.notes.reverse();
    let tmp = tempfile::tempdir().unwrap();
    save(&mut p.clone(), tmp.path()).unwrap();
    let loaded = load(tmp.path()).unwrap();
    assert_eq!(
        ids(&loaded),
        ["btncrev222", "doorcrev22", "pathcrev22", "notecrev22"]
    );
}

#[test]
fn intgrid_legend_is_optional_on_read_and_always_written() {
    // The fixture's crevasse collision.json has no legend.
    let src = std::fs::read_to_string(
        common::fixture_dir().join("worlds/main/levels/crevasse/collision.json"),
    )
    .unwrap();
    assert!(!src.contains("legend"));
    let mut p = common::fixture();
    let def = p.layer("collision").unwrap().clone();
    let grid = p.level("crevasse").unwrap().int_grid("collision").unwrap();
    assert_eq!(grid.size(), [20, 5]);
    assert_eq!(grid.value_at(&def, 1, 3).map(Slug::as_str), Some("ramp_l"));
    assert_eq!(grid.value_at(&def, 1, 1), None);
    let tmp = tempfile::tempdir().unwrap();
    save(&mut p, tmp.path()).unwrap();
    let saved = std::fs::read_to_string(
        tmp.path()
            .join("worlds/main/levels/crevasse/collision.json"),
    )
    .unwrap();
    assert!(saved.contains(r#""L": "ramp_l""#), "{saved}");
}

#[test]
fn project_built_in_code_round_trips() {
    let mut p = Project::new("Built");
    let collision = LayerDef::new(
        slug("collision"),
        "Collision",
        LayerKind::IntGrid(IntGridDef {
            values: vec![IntGridValue {
                slug: slug("solid"),
                name: "Solid".into(),
                color: Color::new("#ffffff").unwrap(),
                glyph: '#',
                group: None,
            }],
            ..Default::default()
        }),
    );
    let auto = LayerDef::new(
        slug("walls"),
        "Walls",
        LayerKind::Auto(AutoDef::new(slug("collision"))),
    );
    let mut fine = LayerDef::new(slug("fine"), "Fine", LayerKind::Tiles(TilesDef::default()));
    fine.grid_size = Some(8);
    p.layers = vec![collision.clone(), auto, fine];
    p.tilesets.push(TilesetDef {
        slug: slug("castle"),
        image: "castle.png".into(),
        tile_size: [16, 16],
        spacing: 0,
        padding: 0,
    });
    let mut rule = Rule::new(
        slug("edge"),
        Pattern::new(&[".", "#", "*"]),
        RuleOutput::tiles(
            slug("castle"),
            vec![TileAlt::single(1, 0), TileAlt::rect(2, 0, 2, 2)],
        ),
    );
    rule.as_match_mut()
        .unwrap()
        .symmetry
        .push(Symmetry::MirrorX);
    p.rules.push(RuleSet {
        layer: slug("walls"),
        stamps: vec![],
        groups: vec![RuleGroup::new(slug("main"), vec![rule])],
    });
    let mut level = Level::new(slug("room"), "Room", [4, 3]);
    let mut grid = IntGridLayer::new_empty(&collision, level.size);
    assert!(grid.set_value(&collision, 0, 2, Some("solid")));
    assert!(!grid.set_value(&collision, 0, 2, Some("lava")));
    assert!(!grid.set_value(&collision, 9, 2, None));
    assert_eq!(
        grid.value_at(&collision, 0, 2).map(Slug::as_str),
        Some("solid")
    );
    assert_eq!(grid.value_at(&collision, 1, 2), None);
    level.int_grids.push(grid);
    level.tiles.push(TilesLayer {
        layer: slug("fine"),
        tiles: vec![TilePlacement {
            cell: [7, 5],
            tileset: slug("castle"),
            tile: [0, 0],
            flip: None,
            alpha: 1.0,
        }],
    });
    level.layers.insert(
        slug("walls"),
        LayerState {
            seed: 5,
            ..Default::default()
        },
    );
    level
        .notes
        .push(Annotation::new(Shape::Note { px: [1, 2] }));
    p.worlds[0].levels.push(level);
    assert_eq!(
        p.layer_cells(p.layer("fine").unwrap(), p.level("room").unwrap()),
        [8, 6]
    );

    assert_eq!(validate(&p), vec![]);
    let tmp = tempfile::tempdir().unwrap();
    save(&mut p.clone(), tmp.path()).unwrap();
    assert_eq!(load(tmp.path()).unwrap(), p);
    assert_eq!(p.levels().next().unwrap().layer_seed("walls"), 5);
}

#[test]
fn absent_blocking_values_load_as_the_default_kinds() {
    // The fixture's project.json names a collision layer but no
    // blocking_values: load fills in the default kinds the layer has.
    let p = common::fixture();
    let raw = fs::read_to_string(common::fixture_dir().join("project.json")).unwrap();
    assert!(!raw.contains("blocking_values"));
    let expected: Vec<&str> = p
        .layer("collision")
        .unwrap()
        .values()
        .iter()
        .map(|v| v.slug.as_str())
        .filter(|s| DEFAULT_BLOCKING_VALUES.contains(s))
        .collect();
    assert!(!expected.is_empty());
    assert_eq!(
        p.blocking_values
            .as_ref()
            .unwrap()
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>(),
        expected
    );
    // An explicit [] survives a save and load: it is not "absent".
    let mut p = p;
    p.blocking_values = Some(Vec::new());
    let tmp = tempfile::tempdir().unwrap();
    save(&mut p.clone(), tmp.path()).unwrap();
    assert_eq!(load(tmp.path()).unwrap().blocking_values, Some(Vec::new()));
}

/// `render_files` is exactly what a full save writes, without touching disk.
#[test]
fn render_files_matches_a_full_save() {
    let mut p = common::fixture();
    let rendered = worlds_core::format::render_files(&p).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    worlds_core::save_all(&mut p, tmp.path()).unwrap();
    let on_disk = common::read_tree(tmp.path());
    assert_eq!(rendered.len(), on_disk.len());
    for (rel, bytes) in rendered {
        let key = rel.to_string_lossy().replace('\\', "/");
        assert_eq!(on_disk[&key].as_bytes(), &bytes[..], "{key}");
    }
}

/// `render_file` gives exactly the bytes `render_files` has for every file,
/// and nothing for a path a save doesn't write.
#[test]
fn render_file_matches_the_full_render() {
    let mut p = common::fixture();
    // Unsorted content, as edits leave it: normalized the same way.
    p.worlds[0].levels[0].int_grids.reverse();
    let all = format::render_files(&p).unwrap();
    assert!(all.len() > 10);
    for (rel, bytes) in &all {
        assert_eq!(
            format::render_file(&p, rel).unwrap().as_deref(),
            Some(bytes.as_slice()),
            "{}",
            rel.display()
        );
    }
    for missing in [
        "nope.json",
        "entities/ghost.json",
        "entities/../project.json",
        "rules/collision.json",
        "worlds/main/levels/nowhere/level.json",
        "worlds/main/levels/crevasse/nothing.json",
        "worlds/main/levels/crevasse",
    ] {
        assert_eq!(
            format::render_file(&p, std::path::Path::new(missing)).unwrap(),
            None,
            "{missing}"
        );
    }
}

/// An imported project (an LDtk sample) loads and renders every file it
/// has byte-identically: a saved project survives load and save unchanged.
#[test]
fn imported_project_saves_byte_identically() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/ldtk-samples/AutoLayers_5_Advanced.ldtk");
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("imported.worlds");
    let imported = worlds_core::ldtk::import(&src, &dir).unwrap();
    worlds_core::ldtk::write(&imported, &dir).unwrap();
    let p = load(&dir).unwrap_or_else(|e| panic!("{e}"));
    let rendered = format::render_files(&p).unwrap();
    assert!(rendered.len() > 5);
    for (rel, bytes) in rendered {
        let on_disk = fs::read(dir.join(&rel)).unwrap_or_default();
        assert!(on_disk == bytes, "{} differs after a save", rel.display());
    }
}
