//! Definition commands: exact inverses (down to the files on disk), renames
//! that rewrite every reference, no-ops, refusals, and the incremental save
//! after a rename.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::json;
use worlds_core::command::{Change, Command, CommandError, DefKind, LayerContent, ProjectKey};
use worlds_core::*;

fn s(x: &str) -> Slug {
    slug(x)
}

/// The fixture saved to a temp dir and loaded back from it, so it knows its files.
fn on_disk() -> (tempfile::TempDir, Project) {
    let tmp = tempfile::tempdir().unwrap();
    save_all(&mut common::fixture(), tmp.path()).unwrap();
    let p = load(tmp.path()).unwrap();
    (tmp, p)
}

/// What a full save of `p` writes.
fn full_tree(p: &Project) -> BTreeMap<String, String> {
    let tmp = tempfile::tempdir().unwrap();
    save_all(&mut p.clone(), tmp.path()).unwrap();
    common::read_tree(tmp.path())
}

fn round_trip(cmd: &Command) -> Command {
    let json = serde_json::to_string(cmd).unwrap();
    let back: Command = serde_json::from_str(&json).unwrap_or_else(|e| panic!("{e}: {json}"));
    assert_eq!(&back, cmd, "JSON round trip: {json}");
    back
}

/// Apply `cmd` to `p`, saved in `dir`, and check: the incremental save
/// afterwards writes and deletes exactly what a full save would; the
/// inverse restores an equal project and, saved incrementally, the same
/// files as before (which files exist included); redo gets the edited state
/// back with the same changes; every command survives a JSON round trip.
/// Leaves the edit applied (and saved) and returns its changes.
fn check(dir: &Path, p: &mut Project, cmd: Command) -> Vec<Change> {
    save_dirty(p, dir).unwrap();
    let before_tree = common::read_tree(dir);
    let before = p.clone();
    let a = p
        .apply(round_trip(&cmd))
        .unwrap_or_else(|e| panic!("{}: {e}", cmd.label()));
    assert!(!a.changes.is_empty(), "{} changed nothing", cmd.label());
    let after = p.clone();
    save_dirty(p, dir).unwrap();
    let after_tree = full_tree(p);
    assert_eq!(
        common::read_tree(dir),
        after_tree,
        "incremental save after {}",
        cmd.label()
    );
    let b = p.apply(round_trip(&a.inverse)).unwrap();
    assert!(*p == before, "undo of {} isn't exact", cmd.label());
    save_dirty(p, dir).unwrap();
    assert_eq!(
        common::read_tree(dir),
        before_tree,
        "files after undo of {}",
        cmd.label()
    );
    let c = p.apply(round_trip(&b.inverse)).unwrap();
    assert!(*p == after, "redo of {} isn't exact", cmd.label());
    assert_eq!(
        c.changes,
        a.changes,
        "redo of {} emits the same changes",
        cmd.label()
    );
    save_dirty(p, dir).unwrap();
    assert_eq!(common::read_tree(dir), after_tree);
    a.changes
}

fn refused(p: &mut Project, cmd: Command) -> CommandError {
    let before = p.clone();
    let e = p.apply(cmd).unwrap_err();
    assert!(*p == before, "a refused command changed the project: {e}");
    assert!(!p.is_dirty() || before.is_dirty());
    e
}

fn noop(p: &mut Project, cmd: Command) {
    let before = p.clone();
    let dirty = p.files.dirty().clone();
    let a = p.apply(cmd).unwrap();
    assert!(a.changes.is_empty() && a.inverse.is_noop());
    assert!(*p == before);
    assert_eq!(*p.files.dirty(), dirty);
}

fn layer_json(v: serde_json::Value) -> LayerDef {
    serde_json::from_value(v).unwrap()
}

fn rows(p: &Project, level: &str, layer: &str) -> Vec<String> {
    let def = p.layer(layer).unwrap();
    p.level(level)
        .unwrap()
        .int_grid(layer)
        .unwrap()
        .to_rows(def)
        .unwrap()
}

/// Every saved file that still mentions `"slug"` (quoted, so an image path
/// like `art/castle_main.png` doesn't count).
fn mentions(p: &Project, slug: &str) -> Vec<String> {
    let needle = format!("\"{slug}\"");
    full_tree(p)
        .into_iter()
        .filter(|(_, text)| text.contains(&needle))
        .map(|(f, _)| f)
        .collect()
}

fn files(changes: &[Change]) -> BTreeSet<PathBuf> {
    changes.iter().flat_map(|c| c.files()).collect()
}

// ---- layers ----

#[test]
fn create_a_layer_with_content_and_remove_it() {
    let (tmp, mut p) = on_disk();
    let def = layer_json(json!({
        "slug": "hazards", "name": "Hazards", "kind": "int_grid",
        "values": [{ "slug": "spikes", "name": "Spikes", "color": "#c03030", "glyph": "^" }]
    }));
    let content = LayerContent {
        level: s("crevasse"),
        rows: Some(vec![
            "....................".into(),
            "....................".into(),
            "....................".into(),
            "..^^................".into(),
            "....................".into(),
        ]),
        tiles: None,
        entities: Some(Vec::new()),
        state: None,
    };
    let changes = check(
        tmp.path(),
        &mut p,
        Command::SetLayerDef {
            layer: s("hazards"),
            def,
            index: Some(1),
            content: vec![content],
            rules: None,
        },
    );
    assert_eq!(p.layers[1].slug, "hazards");
    assert_eq!(rows(&p, "crevasse", "hazards")[3], "..^^................");
    assert!(
        tmp.path()
            .join("worlds/main/levels/crevasse/hazards.json")
            .is_file()
    );
    assert_eq!(
        changes,
        [
            Change::Def {
                kind: DefKind::Layer,
                slug: s("hazards")
            },
            Change::Grid {
                world: s("main"),
                level: s("crevasse"),
                layer: s("hazards"),
                rect: [0, 0, 20, 5].into()
            },
            Change::EntityLayer {
                world: s("main"),
                level: s("crevasse"),
                layer: s("hazards")
            },
        ]
    );

    // Removing the collision layer takes its content with it; the inverse
    // puts it back (checked by `check`).
    let changes = check(
        tmp.path(),
        &mut p,
        Command::RemoveLayerDef {
            layer: s("collision"),
        },
    );
    assert!(p.layer("collision").is_none());
    assert!(p.level("crevasse").unwrap().int_grid("collision").is_none());
    assert!(
        !tmp.path()
            .join("worlds/main/levels/crevasse/collision.json")
            .exists()
    );
    // The references are left for validation to report.
    assert_eq!(p.collision_layer, Some(s("collision")));
    assert!(changes.contains(&Change::Grid {
        world: s("main"),
        level: s("crevasse"),
        layer: s("collision"),
        rect: [0, 0, 20, 5].into()
    }));

    // An auto layer: its rule set and per-level rule state go too.
    let changes = check(
        tmp.path(),
        &mut p,
        Command::RemoveLayerDef {
            layer: s("main_tiling"),
        },
    );
    assert!(p.rules.is_empty());
    assert!(p.levels().all(|l| l.layers.is_empty()));
    assert!(!tmp.path().join("rules/main_tiling.json").exists());
    assert_eq!(changes.len(), 4, "{changes:?}"); // def, rules, two states
    // An entities layer: every instance on it goes.
    check(
        tmp.path(),
        &mut p,
        Command::RemoveLayerDef {
            layer: s("objects"),
        },
    );
    assert!(p.levels().all(|l| l.entities_layer("objects").is_none()));
    assert!(
        !tmp.path()
            .join("worlds/main/levels/shaft/entities.json")
            .exists()
    );
}

#[test]
fn replacing_an_int_grid_layer_keeps_each_cell_value() {
    let (tmp, mut p) = on_disk();
    let before = rows(&p, "crevasse", "collision");
    let mut def = p.layer("collision").unwrap().clone();
    let ig = def.int_grid_mut().unwrap();
    // Reverse the values, change a glyph, add one.
    ig.values.reverse();
    ig.values
        .iter_mut()
        .find(|v| v.slug == "oneway")
        .unwrap()
        .glyph = '-';
    let mut new = ig.values[0].clone();
    new.slug = s("lava");
    new.glyph = 'v';
    ig.values.push(new);
    let changes = check(
        tmp.path(),
        &mut p,
        Command::SetLayerDef {
            layer: s("collision"),
            def,
            index: None,
            content: Vec::new(),
            rules: None,
        },
    );
    let after = rows(&p, "crevasse", "collision");
    assert_eq!(after[2], before[2].replace('o', "-"));
    assert_eq!(after[3], before[3]);
    assert_eq!(changes.len(), 2, "{changes:?}"); // def + crevasse grid
    // Moving it in draw order (same def) is a change too; saying where it is isn't.
    let def = p.layer("collision").unwrap().clone();
    let set = |index| Command::SetLayerDef {
        layer: s("collision"),
        def: def.clone(),
        index,
        content: Vec::new(),
        rules: None,
    };
    noop(&mut p, set(Some(0)));
    noop(&mut p, set(None));
    check(tmp.path(), &mut p, set(Some(3)));
    assert_eq!(p.layers[3].slug, "collision");
    let e = refused(&mut p, set(Some(4)));
    assert!(e.to_string().contains("past the end"), "{e}");

    // Taking out a value that is still painted is refused.
    let mut def = p.layer("collision").unwrap().clone();
    def.int_grid_mut()
        .unwrap()
        .values
        .retain(|v| v.slug != "water");
    let e = refused(
        &mut p,
        Command::SetLayerDef {
            layer: s("collision"),
            def,
            index: None,
            content: Vec::new(),
            rules: None,
        },
    );
    assert!(e.to_string().contains("\"water\""), "{e}");
}

#[test]
fn layer_def_refusals() {
    let mut p = common::fixture();
    let set = |def: LayerDef| Command::SetLayerDef {
        layer: def.slug.clone(),
        def,
        index: None,
        content: Vec::new(),
        rules: None,
    };
    // A kind change while there is content.
    let mut def = p.layer("decor_back").unwrap().clone();
    def.kind = LayerKind::Entities;
    let e = refused(&mut p, set(def));
    assert!(
        e.to_string()
            .contains("can't change from tiles to entities"),
        "{e}"
    );
    // A grid change while there is content.
    let mut def = p.layer("decor_back").unwrap().clone();
    def.grid_size = Some(16);
    let e = refused(&mut p, set(def));
    assert!(e.to_string().contains("misplace"), "{e}");
    // Reserved names.
    let e = refused(
        &mut p,
        set(layer_json(
            json!({ "slug": "notes", "name": "N", "kind": "tiles" }),
        )),
    );
    assert!(e.to_string().contains("notes.json"), "{e}");
    // A slug mismatch, and content on a layer that exists.
    let def = p.layer("collision").unwrap().clone();
    assert!(matches!(
        refused(
            &mut p,
            Command::SetLayerDef {
                layer: s("other"),
                def: def.clone(),
                index: None,
                content: Vec::new(),
                rules: None,
            }
        ),
        CommandError::Invalid(_)
    ));
    let content = LayerContent {
        level: s("shaft"),
        rows: None,
        tiles: None,
        entities: None,
        state: Some(LayerState::default()),
    };
    assert!(
        refused(
            &mut p,
            Command::SetLayerDef {
                layer: s("collision"),
                def,
                index: None,
                content: vec![content.clone()],
                rules: None,
            }
        )
        .to_string()
        .contains("only come with a new layer")
    );
    // Content of the wrong kind, or for an unknown level.
    let new = layer_json(json!({ "slug": "decor2", "name": "D", "kind": "tiles" }));
    let with = |c: LayerContent| Command::SetLayerDef {
        layer: s("decor2"),
        def: new.clone(),
        index: None,
        content: vec![c],
        rules: None,
    };
    assert!(matches!(
        refused(
            &mut p,
            with(LayerContent {
                rows: Some(vec!["..".into()]),
                state: None,
                ..content.clone()
            })
        ),
        CommandError::WrongLayerKind { .. }
    ));
    assert!(matches!(
        refused(
            &mut p,
            with(LayerContent {
                level: s("nowhere"),
                ..content.clone()
            })
        ),
        CommandError::UnknownLevel(_)
    ));
    // Entity ids must stay unique.
    let dup = p.level("shaft").unwrap().entities[0].entities.clone();
    assert!(matches!(
        refused(
            &mut p,
            with(LayerContent {
                entities: Some(dup),
                state: None,
                ..content
            })
        ),
        CommandError::DuplicateId(_)
    ));
    assert!(matches!(
        refused(&mut p, Command::RemoveLayerDef { layer: s("nope") }),
        CommandError::UnknownLayer(_)
    ));
}

#[test]
fn rename_layer_rewrites_every_reference() {
    let (tmp, mut p) = on_disk();
    // An int_grid layer: the collision layer, an auto layer's source, rule
    // patterns' `grid`, and a level file.
    let changes = check(
        tmp.path(),
        &mut p,
        Command::RenameLayer {
            from: s("collision"),
            to: s("walls"),
        },
    );
    assert_eq!(p.collision_layer, Some(s("walls")));
    assert_eq!(
        p.layer("main_tiling").unwrap().auto().unwrap().source,
        "walls"
    );
    assert!(
        mentions(&p, "collision").is_empty(),
        "{:?}",
        mentions(&p, "collision")
    );
    assert!(
        tmp.path()
            .join("worlds/main/levels/crevasse/walls.json")
            .is_file()
    );
    assert!(
        !tmp.path()
            .join("worlds/main/levels/crevasse/collision.json")
            .exists()
    );
    assert_eq!(
        changes,
        [
            Change::DefRenamed {
                kind: DefKind::Layer,
                from: s("collision"),
                to: s("walls")
            },
            Change::Def {
                kind: DefKind::Layer,
                slug: s("main_tiling")
            },
            Change::Def {
                kind: DefKind::Rules,
                slug: s("main_tiling")
            },
            Change::Project {
                key: ProjectKey::CollisionLayer
            },
            Change::LayerFileRenamed {
                world: s("main"),
                level: s("crevasse"),
                from: s("collision"),
                to: s("walls")
            },
        ]
    );
    // An auto layer: its rule set file and per-level rule state.
    check(
        tmp.path(),
        &mut p,
        Command::RenameLayer {
            from: s("main_tiling"),
            to: s("tiling"),
        },
    );
    assert!(tmp.path().join("rules/tiling.json").is_file());
    assert!(!tmp.path().join("rules/main_tiling.json").exists());
    assert_eq!(p.level("shaft").unwrap().layer_seed("tiling"), 4);
    assert!(mentions(&p, "main_tiling").is_empty());
    // A tiles layer and an entities layer.
    check(
        tmp.path(),
        &mut p,
        Command::RenameLayer {
            from: s("decor_back"),
            to: s("decor"),
        },
    );
    let changes = check(
        tmp.path(),
        &mut p,
        Command::RenameLayer {
            from: s("objects"),
            to: s("things"),
        },
    );
    assert_eq!(changes.len(), 3); // def + two entities.json
    assert!(mentions(&p, "decor_back").is_empty() && mentions(&p, "objects").is_empty());
    assert!(validate(&p).is_empty(), "{:?}", validate(&p));

    noop(
        &mut p,
        Command::RenameLayer {
            from: s("walls"),
            to: s("walls"),
        },
    );
    assert!(matches!(
        refused(
            &mut p,
            Command::RenameLayer {
                from: s("walls"),
                to: s("tiling")
            }
        ),
        CommandError::DuplicateDef {
            kind: DefKind::Layer,
            ..
        }
    ));
    assert!(matches!(
        refused(
            &mut p,
            Command::RenameLayer {
                from: s("ghost"),
                to: s("x")
            }
        ),
        CommandError::UnknownLayer(_)
    ));
    let e = refused(
        &mut p,
        Command::RenameLayer {
            from: s("decor"),
            to: s("level"),
        },
    );
    assert!(e.to_string().contains("level.json"), "{e}");
    // A dangling reference to the new slug: undo couldn't tell it apart.
    p.rules[0].groups[0].rules[0]
        .as_rule_mut()
        .unwrap()
        .as_match_mut()
        .unwrap()
        .pattern
        .grid = Some(s("ghost"));
    let e = refused(
        &mut p,
        Command::RenameLayer {
            from: s("decor"),
            to: s("ghost"),
        },
    );
    assert!(
        e.to_string().contains("already refers to layer \"ghost\""),
        "{e}"
    );
}

#[test]
fn save_dirty_after_renames_writes_and_deletes_exactly_those_files() {
    let (tmp, mut p) = on_disk();
    let mut changes = Vec::new();
    for c in [
        Command::RenameLayer {
            from: s("collision"),
            to: s("walls"),
        },
        Command::RenameEntityType {
            from: s("locked_door"),
            to: s("door"),
        },
    ] {
        changes.extend(p.apply(c).unwrap().changes);
    }
    let dirty: Vec<String> = p
        .files
        .dirty()
        .iter()
        .map(|f| f.to_string_lossy().replace('\\', "/"))
        .collect();
    assert_eq!(
        dirty,
        [
            "entities/door.json",
            "entities/floor_button.json",
            "entities/locked_door.json",
            "layers.json",
            "project.json",
            "rules/main_tiling.json",
            "worlds/main/levels/crevasse/collision.json",
            "worlds/main/levels/crevasse/entities.json",
            "worlds/main/levels/crevasse/walls.json",
            "worlds/main/levels/shaft/entities.json",
        ]
    );
    assert_eq!(files(&changes), p.files.dirty().clone());
    let before = common::read_tree(tmp.path());
    save_dirty(&mut p, tmp.path()).unwrap();
    let after = common::read_tree(tmp.path());
    assert_eq!(after, full_tree(&p));
    let mut differ: Vec<&String> = before
        .keys()
        .chain(after.keys())
        .filter(|f| before.get(*f) != after.get(*f))
        .collect();
    differ.sort();
    differ.dedup();
    assert_eq!(differ, dirty.iter().collect::<Vec<_>>());
    assert!(!tmp.path().join("entities/locked_door.json").exists());
    assert_eq!(load(tmp.path()).unwrap(), p);
}

// ---- entity types ----

#[test]
fn entity_def_set_remove_rename() {
    let (tmp, mut p) = on_disk();
    let mut lever = p.entity_def("floor_button").unwrap().clone();
    lever.slug = s("lever");
    lever.name = "Lever".into();
    check(
        tmp.path(),
        &mut p,
        Command::SetEntityDef {
            entity: s("lever"),
            def: lever.clone(),
        },
    );
    assert!(tmp.path().join("entities/lever.json").is_file());
    noop(
        &mut p,
        Command::SetEntityDef {
            entity: s("lever"),
            def: lever.clone(),
        },
    );
    lever.size = [2, 1];
    check(
        tmp.path(),
        &mut p,
        Command::SetEntityDef {
            entity: s("lever"),
            def: lever,
        },
    );
    // Removing an unused type; removing a used one takes its instances.
    check(
        tmp.path(),
        &mut p,
        Command::RemoveEntityDef {
            entity: s("lever"),
            keep_instances: false,
        },
    );
    let changes = check(
        tmp.path(),
        &mut p,
        Command::RemoveEntityDef {
            entity: s("locked_door"),
            keep_instances: false,
        },
    );
    assert_eq!(changes.len(), 3, "{changes:?}"); // two doors and the def
    assert!(
        p.levels()
            .flat_map(|l| l.all_entities())
            .all(|e| e.ty != "locked_door")
    );
    // shaft's only entity was a door: its entities layer entry went too.
    assert!(p.level("shaft").unwrap().entities.is_empty());
}

#[test]
fn rename_entity_type_rewrites_instances_and_allowed_types() {
    let (tmp, mut p) = on_disk();
    // floor_button's `target` field allows locked_door; add a level field
    // that does too.
    let mut f = FieldDef::new(s("boss_door"), FieldType::entity_ref());
    if let FieldType::EntityRef { allowed_types, .. } = &mut f.ty {
        allowed_types.push(s("locked_door"));
    }
    p.level_fields.push(f);
    save_all(&mut p, tmp.path()).unwrap();
    let changes = check(
        tmp.path(),
        &mut p,
        Command::RenameEntityType {
            from: s("locked_door"),
            to: s("door"),
        },
    );
    assert!(mentions(&p, "locked_door").is_empty());
    assert_eq!(
        changes,
        [
            Change::DefRenamed {
                kind: DefKind::Entity,
                from: s("locked_door"),
                to: s("door")
            },
            Change::Def {
                kind: DefKind::Entity,
                slug: s("floor_button")
            },
            Change::Project {
                key: ProjectKey::LevelFields
            },
            Change::Entity {
                world: s("main"),
                level: s("crevasse"),
                id: Id::new("doorcrev22").unwrap()
            },
            Change::Entity {
                world: s("main"),
                level: s("shaft"),
                id: Id::new("doorshaft2").unwrap()
            },
        ]
    );
    noop(
        &mut p,
        Command::RenameEntityType {
            from: s("door"),
            to: s("door"),
        },
    );
    assert!(matches!(
        refused(
            &mut p,
            Command::RenameEntityType {
                from: s("door"),
                to: s("floor_button")
            }
        ),
        CommandError::DuplicateDef { .. }
    ));
    assert!(matches!(
        refused(
            &mut p,
            Command::RemoveEntityDef {
                entity: s("ghost"),
                keep_instances: false,
            }
        ),
        CommandError::UnknownEntityType(_)
    ));
    // An instance whose type is already the new slug.
    p.worlds[0].levels[0].entities[0].entities[0].ty = s("ghost");
    let e = refused(
        &mut p,
        Command::RenameEntityType {
            from: s("door"),
            to: s("ghost"),
        },
    );
    assert!(e.to_string().contains("entity doorcrev22"), "{e}");
}

// ---- enums ----

#[test]
fn enum_set_remove_rename() {
    let (tmp, mut p) = on_disk();
    let weather: EnumDef =
        serde_json::from_value(json!({ "slug": "weather", "values": [{ "slug": "rain" }] }))
            .unwrap();
    check(
        tmp.path(),
        &mut p,
        Command::SetEnumDef {
            enum_: s("weather"),
            def: weather.clone(),
            index: Some(0),
        },
    );
    assert_eq!(p.enums[0].slug, "weather");
    // Move it to the end.
    check(
        tmp.path(),
        &mut p,
        Command::SetEnumDef {
            enum_: s("weather"),
            def: weather,
            index: Some(2),
        },
    );
    check(
        tmp.path(),
        &mut p,
        Command::RemoveEnumDef {
            enum_: s("weather"),
        },
    );
    // A rename rewrites `enum` field types.
    let changes = check(
        tmp.path(),
        &mut p,
        Command::RenameEnum {
            from: s("biome"),
            to: s("region"),
        },
    );
    assert_eq!(changes.len(), 2); // enums.json, project.json level_fields
    assert!(mentions(&p, "biome").iter().all(|f| f != "enums.json"));
    check(
        tmp.path(),
        &mut p,
        Command::RenameEnum {
            from: s("facing"),
            to: s("direction"),
        },
    );
    assert!(validate(&p).is_empty(), "{:?}", validate(&p));
    assert!(matches!(
        refused(
            &mut p,
            Command::RenameEnum {
                from: s("facing"),
                to: s("x")
            }
        ),
        CommandError::UnknownEnum(_)
    ));
    assert!(matches!(
        refused(
            &mut p,
            Command::RenameEnum {
                from: s("region"),
                to: s("direction")
            }
        ),
        CommandError::DuplicateDef { .. }
    ));
    assert!(matches!(
        refused(&mut p, Command::RemoveEnumDef { enum_: s("nope") }),
        CommandError::UnknownEnum(_)
    ));
}

#[test]
fn rename_enum_value_rewrites_values_defaults_and_requirements() {
    let (tmp, mut p) = on_disk();
    // biome is a level field (crevasse holds ["flooded"]) and a rule
    // group requirement.
    let changes = check(
        tmp.path(),
        &mut p,
        Command::RenameEnumValue {
            enum_: s("biome"),
            from: s("flooded"),
            to: s("wet"),
        },
    );
    assert_eq!(p.level("crevasse").unwrap().fields["biome"], json!(["wet"]));
    let req = p.rules[0].groups[1].requires_level_field.as_ref().unwrap();
    assert_eq!(req.values, [s("wet")]);
    assert_eq!(changes.len(), 3, "{changes:?}"); // enums, crevasse level.json, rules
    // facing: a default (left) and a stored value (doorcrev22: right).
    check(
        tmp.path(),
        &mut p,
        Command::RenameEnumValue {
            enum_: s("facing"),
            from: s("left"),
            to: s("west"),
        },
    );
    let door = p.entity_def("locked_door").unwrap();
    assert_eq!(door.field("facing").unwrap().default, Some(json!("west")));
    let changes = check(
        tmp.path(),
        &mut p,
        Command::RenameEnumValue {
            enum_: s("facing"),
            from: s("right"),
            to: s("east"),
        },
    );
    assert!(changes.contains(&Change::Entity {
        world: s("main"),
        level: s("crevasse"),
        id: Id::new("doorcrev22").unwrap()
    }));
    assert!(validate(&p).is_empty(), "{:?}", validate(&p));

    let rename = |from: &str, to: &str| Command::RenameEnumValue {
        enum_: s("facing"),
        from: s(from),
        to: s(to),
    };
    noop(&mut p, rename("east", "east"));
    assert!(matches!(
        refused(&mut p, rename("north", "up")),
        CommandError::UnknownEnumValue { .. }
    ));
    assert!(
        refused(&mut p, rename("east", "west"))
            .to_string()
            .contains("already has")
    );
    // A stored value that already says the new slug.
    p.worlds[0].levels[0].entities[0].entities[0]
        .fields
        .insert(s("facing"), json!("up"));
    let e = refused(&mut p, rename("east", "up"));
    assert!(
        e.to_string()
            .contains("entity doorcrev22 already holds \"up\""),
        "{e}"
    );
}

// ---- tilesets ----

#[test]
fn rename_tileset_rewrites_every_reference() {
    let (tmp, mut p) = on_disk();
    let changes = check(
        tmp.path(),
        &mut p,
        Command::RenameTileset {
            from: s("castle_main"),
            to: s("castle"),
        },
    );
    assert!(
        mentions(&p, "castle_main").is_empty(),
        "{:?}",
        mentions(&p, "castle_main")
    );
    // Layer def, enum icon, entity icon + tile field + its value, rules,
    // placements, a level's tileset substitution.
    let want = [
        "tilesets.json",
        "layers.json",
        "enums.json",
        "entities/locked_door.json",
        "rules/main_tiling.json",
        "worlds/main/levels/crevasse/decor_back.json",
        "worlds/main/levels/crevasse/entities.json",
        "worlds/main/levels/shaft/level.json",
    ];
    let got: BTreeSet<PathBuf> = files(&changes);
    assert_eq!(got, want.iter().map(PathBuf::from).collect());
    assert!(changes.contains(&Change::Tiles {
        world: s("main"),
        level: s("crevasse"),
        layer: s("decor_back"),
        rect: [4, 2, 2, 1].into()
    }));
    // Stamps, group icons, outputs, and a substitution's target.
    check(
        tmp.path(),
        &mut p,
        Command::RenameTileset {
            from: s("ssw_set5"),
            to: s("ssw"),
        },
    );
    assert!(mentions(&p, "ssw_set5").is_empty());
    assert_eq!(
        p.level("shaft").unwrap().layers[&s("main_tiling")].tilesets[&s("castle")],
        "ssw"
    );
    assert!(
        validate(&p).iter().all(|e| e.to_string().contains("image")),
        "{:?}",
        validate(&p)
    );

    noop(
        &mut p,
        Command::RenameTileset {
            from: s("ssw"),
            to: s("ssw"),
        },
    );
    assert!(matches!(
        refused(
            &mut p,
            Command::RenameTileset {
                from: s("ssw"),
                to: s("castle")
            }
        ),
        CommandError::DuplicateDef { .. }
    ));
    p.worlds[0].levels[0].tiles[0].tiles[0].tileset = s("ghost");
    let e = refused(
        &mut p,
        Command::RenameTileset {
            from: s("ssw"),
            to: s("ghost"),
        },
    );
    assert!(
        e.to_string()
            .contains("tiles of layer decor_back in level crevasse"),
        "{e}"
    );
}

/// Defining a type or tileset that something already names (a dangling
/// reference, e.g. after a hand edit) is undone exactly: the definition goes,
/// the references stay.
#[test]
fn creating_a_definition_that_dangling_references_name_undoes_exactly() {
    let tmp = tempfile::tempdir().unwrap();
    let mut p = common::fixture();
    let door = p.entity_def("locked_door").unwrap().clone();
    let ti = p
        .tilesets
        .iter()
        .position(|t| t.slug == "castle_main")
        .unwrap();
    let castle = p.tilesets[ti].clone();
    p.entities.retain(|e| e.slug != "locked_door");
    p.tilesets.remove(ti);
    save_all(&mut p, tmp.path()).unwrap();
    let mut p = load(tmp.path()).unwrap();
    let doors = |p: &Project| {
        p.levels()
            .flat_map(|l| l.all_entities())
            .filter(|e| e.ty == "locked_door")
            .count()
    };
    assert_eq!(doors(&p), 2);

    let mut q = p.clone();
    let a = q
        .apply(Command::SetEntityDef {
            entity: s("locked_door"),
            def: door.clone(),
        })
        .unwrap();
    assert_eq!(
        a.inverse,
        Command::RemoveEntityDef {
            entity: s("locked_door"),
            keep_instances: true,
        }
    );
    check(
        tmp.path(),
        &mut p,
        Command::SetEntityDef {
            entity: s("locked_door"),
            def: door,
        },
    );
    assert_eq!(doors(&p), 2);

    let mut q = p.clone();
    let a = q
        .apply(Command::SetTilesetDef {
            tileset: s("castle_main"),
            def: castle.clone(),
            index: Some(ti),
        })
        .unwrap();
    assert_eq!(
        a.inverse,
        Command::RemoveTilesetDef {
            tileset: s("castle_main"),
            keep_uses: true,
        }
    );
    check(
        tmp.path(),
        &mut p,
        Command::SetTilesetDef {
            tileset: s("castle_main"),
            def: castle,
            index: Some(ti),
        },
    );
    // Removing a used tileset still needs keep_uses.
    assert!(matches!(
        refused(
            &mut p,
            Command::RemoveTilesetDef {
                tileset: s("castle_main"),
                keep_uses: false,
            }
        ),
        CommandError::InUse { .. }
    ));
}

#[test]
fn tileset_set_and_remove_refuses_while_in_use() {
    let (tmp, mut p) = on_disk();
    let cave: TilesetDef = serde_json::from_value(
        json!({ "slug": "cave", "image": "art/cave.png", "tile_size": [8, 8] }),
    )
    .unwrap();
    check(
        tmp.path(),
        &mut p,
        Command::SetTilesetDef {
            tileset: s("cave"),
            def: cave.clone(),
            index: None,
        },
    );
    let mut bigger = cave.clone();
    bigger.tile_size = [16, 16];
    check(
        tmp.path(),
        &mut p,
        Command::SetTilesetDef {
            tileset: s("cave"),
            def: bigger,
            index: Some(0),
        },
    );
    check(
        tmp.path(),
        &mut p,
        Command::RemoveTilesetDef {
            tileset: s("cave"),
            keep_uses: false,
        },
    );
    match refused(
        &mut p,
        Command::RemoveTilesetDef {
            tileset: s("castle_main"),
            keep_uses: false,
        },
    ) {
        CommandError::InUse { kind, uses, .. } => {
            assert_eq!(kind, DefKind::Tileset);
            assert_eq!(uses.len(), 6, "{uses:?}"); // five shown, then "N more"
            assert_eq!(uses[0], "layer decor_back");
            assert!(uses[5].ends_with(" more"));
        }
        e => panic!("{e}"),
    }
    assert!(matches!(
        refused(
            &mut p,
            Command::RemoveTilesetDef {
                tileset: s("nope"),
                keep_uses: false,
            }
        ),
        CommandError::UnknownTileset(_)
    ));
    let e = refused(
        &mut p,
        Command::SetTilesetDef {
            tileset: s("cave"),
            def: cave,
            index: Some(3),
        },
    );
    assert!(e.to_string().contains("past the end"), "{e}");
}

// ---- rule groups ----

#[test]
fn rule_groups_set_move_remove() {
    let (tmp, mut p) = on_disk();
    let group: RuleGroup = serde_json::from_value(json!({
        "slug": "vines", "mode": "optional", "rules": [{
            "slug": "vine", "match": { "rows": ["#", "."] },
            "out": { "tileset": "castle_main", "tiles": [[7, 7]] }
        }]
    }))
    .unwrap();
    let set = |group: &RuleGroup, index| Command::SetRuleGroup {
        layer: s("main_tiling"),
        group: group.slug.clone(),
        def: group.clone(),
        index,
    };
    check(tmp.path(), &mut p, set(&group, Some(0)));
    assert_eq!(p.rules[0].groups[0].slug, "vines");
    noop(&mut p, set(&group, None));
    check(tmp.path(), &mut p, set(&group, Some(4)));
    let mut changed = group.clone();
    changed.mode = GroupMode::Always;
    check(tmp.path(), &mut p, set(&changed, None));
    check(
        tmp.path(),
        &mut p,
        Command::RemoveRuleGroup {
            layer: s("main_tiling"),
            group: s("retired"),
            keep_empty: false,
        },
    );
    assert!(matches!(
        refused(
            &mut p,
            Command::RemoveRuleGroup {
                layer: s("main_tiling"),
                group: s("retired"),
                keep_empty: false
            }
        ),
        CommandError::UnknownRuleGroup { .. }
    ));
    // Not an auto layer.
    assert!(matches!(
        refused(
            &mut p,
            Command::SetRuleGroup {
                layer: s("collision"),
                group: s("vines"),
                def: group.clone(),
                index: None
            }
        ),
        CommandError::WrongLayerKind { .. }
    ));

    // A new auto layer without rules: the first group makes the rule set,
    // removing the last one (no stamps) removes it again unless kept.
    let def = layer_json(
        json!({ "slug": "moss", "name": "Moss", "kind": "auto", "source": "collision" }),
    );
    check(
        tmp.path(),
        &mut p,
        Command::SetLayerDef {
            layer: s("moss"),
            def,
            index: None,
            content: Vec::new(),
            rules: None,
        },
    );
    let on_moss = Command::SetRuleGroup {
        layer: s("moss"),
        group: s("vines"),
        def: group.clone(),
        index: None,
    };
    check(tmp.path(), &mut p, on_moss);
    assert!(tmp.path().join("rules/moss.json").is_file());
    let remove = |keep_empty| Command::RemoveRuleGroup {
        layer: s("moss"),
        group: s("vines"),
        keep_empty,
    };
    check(tmp.path(), &mut p, remove(true));
    assert!(p.rules_for("moss").is_some_and(|r| r.groups.is_empty()));
    // Back, then remove without keep_empty: the empty set goes.
    p.apply(Command::SetRuleGroup {
        layer: s("moss"),
        group: s("vines"),
        def: group,
        index: None,
    })
    .unwrap();
    check(tmp.path(), &mut p, remove(false));
    assert!(p.rules_for("moss").is_none());
    assert!(!tmp.path().join("rules/moss.json").exists());
    // Removing the layer with its rule set, and back.
    p.apply(Command::RemoveLayerDef { layer: s("moss") })
        .unwrap();
}

#[test]
fn definition_commands_in_a_batch_and_history() {
    let mut p = common::fixture();
    let start = p.clone();
    let mut h = History::new();
    h.push(
        &mut p,
        Command::Batch {
            label: "Agent: tidy names".into(),
            commands: vec![
                Command::RenameLayer {
                    from: s("collision"),
                    to: s("walls"),
                },
                Command::RenameTileset {
                    from: s("castle_main"),
                    to: s("castle"),
                },
                Command::RemoveEntityDef {
                    entity: s("floor_button"),
                    keep_instances: false,
                },
            ],
        },
    )
    .unwrap();
    let edited = p.clone();
    h.undo(&mut p).unwrap();
    assert!(p == start);
    h.redo(&mut p).unwrap();
    assert!(p == edited);
    // A batch whose last step fails rolls the renames back.
    let before = p.clone();
    let e = p
        .apply(Command::Batch {
            label: "half".into(),
            commands: vec![
                Command::RenameLayer {
                    from: s("walls"),
                    to: s("solid_ground"),
                },
                Command::RemoveTilesetDef {
                    tileset: s("castle"),
                    keep_uses: false,
                },
            ],
        })
        .unwrap_err();
    assert!(matches!(e, CommandError::Batch { index: 1, .. }), "{e}");
    assert!(p == before);
}

// ---- field definitions ----

fn field(v: serde_json::Value) -> FieldDef {
    serde_json::from_value(v).unwrap()
}

fn set_door_field(def: serde_json::Value, index: Option<usize>) -> Command {
    let def = field(def);
    Command::SetEntityFieldDef {
        entity: s("locked_door"),
        field: def.slug.clone(),
        def,
        index,
    }
}

fn door_fields(p: &Project) -> Vec<String> {
    p.entity_def("locked_door")
        .unwrap()
        .fields
        .iter()
        .map(|f| f.slug.to_string())
        .collect()
}

fn instance<'a>(p: &'a Project, id: &str) -> &'a EntityInstance {
    p.levels()
        .flat_map(|l| l.all_entities())
        .find(|e| e.id.as_str() == id)
        .unwrap()
}

fn door_change() -> Change {
    Change::Def {
        kind: DefKind::Entity,
        slug: s("locked_door"),
    }
}

fn entity_change(level: &str, id: &str) -> Change {
    Change::Entity {
        world: s("main"),
        level: s(level),
        id: Id::new(id).unwrap(),
    }
}

fn level_fields_change() -> Change {
    Change::Project {
        key: ProjectKey::LevelFields,
    }
}

fn fields_change(level: &str) -> Change {
    Change::LevelMeta {
        world: s("main"),
        level: s(level),
        part: worlds_core::command::LevelPart::Fields,
    }
}

#[test]
fn entity_field_def_create_replace_move_and_noop() {
    let (tmp, mut p) = on_disk();
    let key_color = json!({ "slug": "key_color", "type": "color", "default": "#ffd040" });
    // A new field, at an index.
    let changes = check(
        tmp.path(),
        &mut p,
        set_door_field(key_color.clone(), Some(2)),
    );
    assert_eq!(changes, [door_change()]);
    assert_eq!(door_fields(&p)[2], "key_color");
    let a = p
        .clone()
        .apply(set_door_field(key_color.clone(), Some(2)))
        .unwrap();
    assert!(a.changes.is_empty() && a.inverse.is_noop());
    noop(&mut p, set_door_field(key_color.clone(), None));
    noop(&mut p, set_door_field(key_color, Some(2)));
    // Its inverse is a plain remove: nothing stored a value for it.
    let mut q = p.clone();
    let inv = q
        .apply(set_door_field(
            json!({ "slug": "new_one", "type": "bool" }),
            None,
        ))
        .unwrap()
        .inverse;
    assert_eq!(
        inv,
        Command::RemoveEntityFieldDef {
            entity: s("locked_door"),
            field: s("new_one"),
            keep_values: false
        }
    );
    // Replaced in place, then moved to the front.
    let replaced = json!({ "slug": "key_color", "type": "color", "default": "#000000" });
    check(tmp.path(), &mut p, set_door_field(replaced.clone(), None));
    assert_eq!(door_fields(&p)[2], "key_color");
    let mut q = p.clone();
    let inv = q
        .apply(set_door_field(replaced.clone(), Some(0)))
        .unwrap()
        .inverse;
    assert!(
        matches!(&inv, Command::SetEntityFieldDef { index: Some(2), .. }),
        "{inv:?}"
    );
    check(tmp.path(), &mut p, set_door_field(replaced, Some(0)));
    assert_eq!(door_fields(&p)[0], "key_color");
    // Retyping keeps the stored values; validation reports them.
    check(
        tmp.path(),
        &mut p,
        set_door_field(json!({ "slug": "needs_item", "type": "int" }), None),
    );
    assert_eq!(instance(&p, "doorcrev22").fields["needs_item"], "brass_key");
    assert!(
        validate(&p)
            .iter()
            .any(|x| x.path.ends_with("fields.needs_item") && x.message.contains("integer")),
        "{:?}",
        validate(&p)
    );
}

#[test]
fn entity_field_def_refusals() {
    let (_tmp, mut p) = on_disk();
    let e = refused(
        &mut p,
        Command::SetEntityFieldDef {
            entity: s("locked_door"),
            field: s("other"),
            def: field(json!({ "slug": "key_color", "type": "color" })),
            index: None,
        },
    );
    assert!(e.to_string().contains("is not \"other\""), "{e}");
    let e = refused(
        &mut p,
        Command::SetEntityFieldDef {
            entity: s("ghost"),
            field: s("x"),
            def: field(json!({ "slug": "x", "type": "bool" })),
            index: None,
        },
    );
    assert_eq!(e, CommandError::UnknownEntityType(s("ghost")));
    let n = door_fields(&p).len();
    let e = refused(
        &mut p,
        set_door_field(json!({ "slug": "x", "type": "bool" }), Some(n + 1)),
    );
    assert!(e.to_string().contains("past the end"), "{e}");
    // Moving an existing field: the last index is `len - 1`.
    let e = refused(
        &mut p,
        set_door_field(json!({ "slug": "channel", "type": "int" }), Some(n)),
    );
    assert!(e.to_string().contains("past the end"), "{e}");
    let e = refused(
        &mut p,
        Command::RemoveEntityFieldDef {
            entity: s("locked_door"),
            field: s("nope"),
            keep_values: false,
        },
    );
    assert_eq!(
        e,
        CommandError::UnknownField {
            owner: "entity type \"locked_door\"".into(),
            field: s("nope")
        }
    );
    let e = refused(
        &mut p,
        Command::RenameEntityField {
            entity: s("locked_door"),
            from: s("nope"),
            to: s("x"),
        },
    );
    assert!(matches!(e, CommandError::UnknownField { .. }), "{e}");
    let e = refused(
        &mut p,
        Command::RenameEntityField {
            entity: s("ghost"),
            from: s("nope"),
            to: s("x"),
        },
    );
    assert_eq!(e, CommandError::UnknownEntityType(s("ghost")));
    let e = refused(
        &mut p,
        Command::RenameEntityField {
            entity: s("locked_door"),
            from: s("needs_item"),
            to: s("channel"),
        },
    );
    assert_eq!(
        e,
        CommandError::DuplicateField {
            owner: "entity type \"locked_door\"".into(),
            field: s("channel")
        }
    );
    assert_eq!(serde_json::to_value(&e).unwrap()["kind"], "duplicate_field");
    noop(
        &mut p,
        Command::RenameEntityField {
            entity: s("locked_door"),
            from: s("channel"),
            to: s("channel"),
        },
    );
    // Two fields slugged `channel` (a validation problem): every field
    // command naming it is refused; others still work.
    let k = p
        .entities
        .iter()
        .position(|e| e.slug == "locked_door")
        .unwrap();
    let dup = p.entities[k].fields[0].clone();
    p.entities[k].fields.push(dup);
    for cmd in [
        set_door_field(json!({ "slug": "channel", "type": "int" }), None),
        Command::RemoveEntityFieldDef {
            entity: s("locked_door"),
            field: s("channel"),
            keep_values: false,
        },
        Command::RenameEntityField {
            entity: s("locked_door"),
            from: s("channel"),
            to: s("chan"),
        },
    ] {
        let e = refused(&mut p, cmd);
        assert!(
            e.to_string()
                .contains("2 fields of entity type \"locked_door\""),
            "{e}"
        );
    }
    p.apply(Command::RenameEntityField {
        entity: s("locked_door"),
        from: s("speed"),
        to: s("pace"),
    })
    .unwrap();
}

#[test]
fn remove_entity_field_def_takes_the_values_and_undo_brings_them_back() {
    let (tmp, mut p) = on_disk();
    // Nothing stores `consumes`: a plain remove, undone at its index.
    let i = door_fields(&p)
        .iter()
        .position(|f| f == "consumes")
        .unwrap();
    let mut q = p.clone();
    let inv = q
        .apply(Command::RemoveEntityFieldDef {
            entity: s("locked_door"),
            field: s("consumes"),
            keep_values: false,
        })
        .unwrap()
        .inverse;
    assert!(
        matches!(inv, Command::SetEntityFieldDef { index: Some(j), .. } if j == i),
        "{inv:?}"
    );
    let changes = check(
        tmp.path(),
        &mut p,
        Command::RemoveEntityFieldDef {
            entity: s("locked_door"),
            field: s("consumes"),
            keep_values: false,
        },
    );
    assert_eq!(changes, [door_change()]);
    // doorcrev22 stores `tint`: a batch that unsets it, then the remove.
    let mut q = p.clone();
    let a = q
        .apply(Command::RemoveEntityFieldDef {
            entity: s("locked_door"),
            field: s("tint"),
            keep_values: false,
        })
        .unwrap();
    let Command::Batch { label, commands } = &a.inverse else {
        panic!("{:?}", a.inverse)
    };
    assert_eq!(label, "Remove field tint of locked_door");
    assert!(matches!(commands[0], Command::SetEntityFieldDef { .. }));
    assert!(
        matches!(&commands[1], Command::SetEntityField { value: Some(v), .. } if v == "#ff8080")
    );
    let changes = check(
        tmp.path(),
        &mut p,
        Command::RemoveEntityFieldDef {
            entity: s("locked_door"),
            field: s("tint"),
            keep_values: false,
        },
    );
    assert_eq!(
        changes,
        [entity_change("crevasse", "doorcrev22"), door_change()]
    );
    assert!(!instance(&p, "doorcrev22").fields.contains_key("tint"));
    // With `keep_values` the values stay, as unknown keys.
    let changes = check(
        tmp.path(),
        &mut p,
        Command::RemoveEntityFieldDef {
            entity: s("locked_door"),
            field: s("speed"),
            keep_values: true,
        },
    );
    assert_eq!(changes, [door_change()]);
    assert_eq!(instance(&p, "doorcrev22").fields["speed"], 2);
    // Defining it again (values already stored) is undone by a remove that
    // leaves them.
    let mut q = p.clone();
    let inv = q
        .apply(set_door_field(
            json!({ "slug": "speed", "type": "float" }),
            None,
        ))
        .unwrap()
        .inverse;
    assert_eq!(
        inv,
        Command::RemoveEntityFieldDef {
            entity: s("locked_door"),
            field: s("speed"),
            keep_values: true
        }
    );
    check(
        tmp.path(),
        &mut p,
        set_door_field(json!({ "slug": "speed", "type": "float" }), None),
    );
}

fn instance_mut<'a>(p: &'a mut Project, level: &str, id: &str) -> &'a mut EntityInstance {
    p.worlds
        .iter_mut()
        .flat_map(|w| &mut w.levels)
        .filter(|l| l.slug == level)
        .flat_map(|l| &mut l.entities)
        .flat_map(|x| &mut x.entities)
        .find(|e| e.id.as_str() == id)
        .unwrap()
}

#[test]
fn remove_entity_field_def_is_refused_while_a_holder_shares_its_id() {
    // doorcrev22 (crevasse) stores tint; give doorshaft2 a tint too, then
    // the id doorcrev22: unsetting by id would hit the first one twice.
    let (_tmp, mut p) = on_disk();
    let dup = instance_mut(&mut p, "shaft", "doorshaft2");
    dup.fields.insert(s("tint"), json!("#00ff00"));
    dup.id = Id::new("doorcrev22").unwrap();
    let remove = |keep_values| Command::RemoveEntityFieldDef {
        entity: s("locked_door"),
        field: s("tint"),
        keep_values,
    };
    let e = refused(&mut p, remove(false));
    assert!(
        matches!(&e, CommandError::Invalid(m) if m.contains("several entities have id \"doorcrev22\"")),
        "{e}"
    );
    // Keeping the values touches no instance, so it is fine.
    let mut q = p.clone();
    q.apply(remove(true)).unwrap();
    // A shared id among entities that store nothing for the field doesn't
    // matter: the unset still finds the one holder, and undo puts it back.
    let other = instance_mut(&mut p, "shaft", "doorcrev22");
    other.fields.remove("tint");
    other.id = Id::new("btncrev222").unwrap();
    let before = p.clone();
    let a = p.apply(remove(false)).unwrap();
    p.apply(a.inverse).unwrap();
    assert!(p == before);
}

#[test]
fn unsetting_an_unknown_key_undoes() {
    // A value under a key the type doesn't define (validation reports it);
    // the inspector offers to remove it. Undo used to be refused with
    // `unknown_field`, wedging the history.
    let (tmp, mut p) = on_disk();
    instance_mut(&mut p, "crevasse", "doorcrev22")
        .fields
        .insert(s("stale"), json!(7));
    save_all(&mut p, tmp.path()).unwrap();
    let changes = check(
        tmp.path(),
        &mut p,
        Command::SetEntityField {
            id: Id::new("doorcrev22").unwrap(),
            field: s("stale"),
            value: None,
            allow_unknown: false,
        },
    );
    assert_eq!(changes, [entity_change("crevasse", "doorcrev22")]);
    // Undo carries allow_unknown; an edit without it is still refused.
    let mut q = p.clone();
    let e = refused(
        &mut q,
        Command::SetEntityField {
            id: Id::new("doorcrev22").unwrap(),
            field: s("stale"),
            value: Some(json!(7)),
            allow_unknown: false,
        },
    );
    assert!(matches!(e, CommandError::UnknownField { .. }), "{e}");
    // The same for a level field.
    let level = p
        .worlds
        .iter_mut()
        .flat_map(|w| &mut w.levels)
        .find(|l| l.slug == "crevasse")
        .unwrap();
    level.fields.insert(s("stale"), json!("x"));
    save_all(&mut p, tmp.path()).unwrap();
    let changes = check(
        tmp.path(),
        &mut p,
        Command::SetLevelField {
            world: s("main"),
            level: s("crevasse"),
            field: s("stale"),
            value: None,
            allow_unknown: false,
        },
    );
    assert_eq!(changes, [fields_change("crevasse")]);
}

#[test]
fn rename_entity_field_rewrites_stored_keys() {
    let (tmp, mut p) = on_disk();
    // Give doorshaft2 a value too.
    p.apply(Command::SetEntityField {
        id: Id::new("doorshaft2").unwrap(),
        field: s("needs_item"),
        value: Some(json!("iron_key")),
        allow_unknown: false,
    })
    .unwrap();
    save_dirty(&mut p, tmp.path()).unwrap();
    let changes = check(
        tmp.path(),
        &mut p,
        Command::RenameEntityField {
            entity: s("locked_door"),
            from: s("needs_item"),
            to: s("key_item"),
        },
    );
    assert_eq!(
        changes,
        [
            door_change(),
            entity_change("crevasse", "doorcrev22"),
            entity_change("shaft", "doorshaft2"),
        ]
    );
    assert!(mentions(&p, "needs_item").is_empty());
    assert_eq!(instance(&p, "doorcrev22").fields["key_item"], "brass_key");
    assert_eq!(door_fields(&p)[1], "key_item");
    // `fields_set` in the export follows: set before, set after.
    let level = p.level("crevasse").unwrap();
    let out: serde_json::Value = serde_json::from_str(
        &worlds_core::export::level_json(
            &p,
            level,
            &worlds_core::export::BakedLevel::default(),
            "..",
        )
        .unwrap(),
    )
    .unwrap();
    let door = &out["entities"][0];
    assert_eq!(door["id"], "doorcrev22");
    let set: Vec<&str> = door["fields_set"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(
        set.contains(&"key_item") && !set.contains(&"needs_item"),
        "{set:?}"
    );
    assert_eq!(door["fields"]["key_item"], "brass_key");
    // Pinned: an instance already stores a value under the new slug.
    p.worlds[0].levels[1].entities[0].entities[0]
        .fields
        .insert(s("ghost"), json!(1));
    let e = refused(
        &mut p,
        Command::RenameEntityField {
            entity: s("locked_door"),
            from: s("key_item"),
            to: s("ghost"),
        },
    );
    assert!(
        e.to_string()
            .contains("entity doorshaft2 already stores a value under \"ghost\""),
        "{e}"
    );
    // Instances of other types don't pin it.
    p.clone()
        .apply(Command::RenameEntityField {
            entity: s("floor_button"),
            from: s("channel"),
            to: s("ghost"),
        })
        .unwrap();
}

#[test]
fn level_field_defs_set_remove_rename() {
    let (tmp, mut p) = on_disk();
    let ambience = field(json!({ "slug": "ambience", "type": "float", "default": 0.5, "min": 0 }));
    let set = |def: &FieldDef, index| Command::SetLevelFieldDef {
        field: def.slug.clone(),
        def: def.clone(),
        index,
    };
    let changes = check(tmp.path(), &mut p, set(&ambience, Some(0)));
    assert_eq!(changes, [level_fields_change()]);
    assert_eq!(p.level_fields[0].slug, "ambience");
    noop(&mut p, set(&ambience, None));
    check(tmp.path(), &mut p, set(&ambience, Some(2)));
    let mut louder = ambience.clone();
    louder.default = Some(json!(0.9));
    check(tmp.path(), &mut p, set(&louder, None));
    assert_eq!(p.level_fields[2].default, Some(json!(0.9)));
    // Nothing stores it: a plain remove.
    let changes = check(
        tmp.path(),
        &mut p,
        Command::RemoveLevelFieldDef {
            field: s("ambience"),
            keep_values: false,
        },
    );
    assert_eq!(changes, [level_fields_change()]);
    // crevasse stores `music` (null): a batch.
    let changes = check(
        tmp.path(),
        &mut p,
        Command::RemoveLevelFieldDef {
            field: s("music"),
            keep_values: false,
        },
    );
    assert_eq!(changes, [fields_change("crevasse"), level_fields_change()]);
    assert!(!p.level("crevasse").unwrap().fields.contains_key("music"));
    // `biome` is stored by crevasse and required by a rule group.
    let changes = check(
        tmp.path(),
        &mut p,
        Command::RenameLevelField {
            from: s("biome"),
            to: s("zone"),
        },
    );
    assert_eq!(
        changes,
        [
            level_fields_change(),
            fields_change("crevasse"),
            Change::Def {
                kind: DefKind::Rules,
                slug: s("main_tiling")
            },
        ]
    );
    // Only the enum (and the field's `"enum": "biome"`) are still called biome.
    assert_eq!(mentions(&p, "biome"), ["enums.json", "project.json"]);
    assert_eq!(
        p.level("crevasse").unwrap().fields["zone"],
        json!(["flooded"])
    );
    let req = p.rules[0]
        .groups
        .iter()
        .find_map(|g| g.requires_level_field.as_ref())
        .unwrap();
    assert_eq!(req.field, "zone");
    assert!(validate(&p).is_empty(), "{:?}", validate(&p));
    // Refusals.
    let e = refused(
        &mut p,
        Command::RenameLevelField {
            from: s("nope"),
            to: s("x"),
        },
    );
    assert_eq!(
        e,
        CommandError::UnknownField {
            owner: "project level_fields".into(),
            field: s("nope")
        }
    );
    p.apply(set(&ambience, None)).unwrap();
    let e = refused(
        &mut p,
        Command::RenameLevelField {
            from: s("zone"),
            to: s("ambience"),
        },
    );
    assert!(matches!(e, CommandError::DuplicateField { .. }), "{e}");
    // Pinned by a level storing the new slug...
    p.worlds[0].levels[1]
        .fields
        .insert(s("weather"), json!("rain"));
    let e = refused(
        &mut p,
        Command::RenameLevelField {
            from: s("zone"),
            to: s("weather"),
        },
    );
    assert!(
        e.to_string()
            .contains("level shaft already stores a value under \"weather\""),
        "{e}"
    );
    // ...or by a rule group requiring it.
    p.worlds[0].levels[1].fields.clear();
    p.rules[0].groups[0].requires_level_field =
        Some(serde_json::from_value(json!({ "field": "weather", "values": ["rain"] })).unwrap());
    let e = refused(
        &mut p,
        Command::RenameLevelField {
            from: s("ambience"),
            to: s("weather"),
        },
    );
    assert!(
        e.to_string()
            .contains("already requires level field \"weather\""),
        "{e}"
    );
    // A duplicate slug is refused as ambiguous.
    let dup = p.level_fields[0].clone();
    p.level_fields.push(dup.clone());
    let e = refused(
        &mut p,
        Command::RemoveLevelFieldDef {
            field: dup.slug.clone(),
            keep_values: false,
        },
    );
    assert!(
        e.to_string().contains("2 fields of project level_fields"),
        "{e}"
    );
}

#[test]
fn field_commands_in_history() {
    let (_tmp, mut p) = on_disk();
    let start = p.clone();
    let mut h = History::new();
    for cmd in [
        set_door_field(json!({ "slug": "key_color", "type": "color" }), Some(0)),
        Command::RenameEntityField {
            entity: s("locked_door"),
            from: s("needs_item"),
            to: s("key_item"),
        },
        Command::RemoveEntityFieldDef {
            entity: s("locked_door"),
            field: s("tint"),
            keep_values: false,
        },
        Command::RenameLevelField {
            from: s("biome"),
            to: s("zone"),
        },
        Command::RemoveLevelFieldDef {
            field: s("music"),
            keep_values: false,
        },
    ] {
        let label = cmd.label();
        h.push(&mut p, cmd).unwrap();
        assert_eq!(h.undo_label(), Some(label.as_str()));
    }
    let end = p.clone();
    while h.undo(&mut p).unwrap().is_some() {}
    assert!(p == start);
    while h.redo(&mut p).unwrap().is_some() {}
    assert!(p == end);
}
