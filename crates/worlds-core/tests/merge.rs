//! `worlds_core::merge`: importing from another Worlds project, between the
//! mini fixture and the game fixture (two different projects), and between
//! copies of one project (where everything collides).

mod common;

use std::path::{Path, PathBuf};

use worlds_core::merge::{
    self, Action, Conflict, Kind, Placement, PlacementMode, Plan, Policy, Preset, Selection,
};
use worlds_core::{Command, Project, slug};

fn game_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/game/game.worlds")
}

fn game() -> Project {
    worlds_core::load(game_dir()).unwrap_or_else(|e| panic!("{e}"))
}

fn mini() -> Project {
    common::fixture()
}

/// An empty project saved in a temp dir (so assets have somewhere to go).
fn empty(grid: u32) -> (tempfile::TempDir, Project) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("new.worlds");
    let mut p = Project::new("Sequel");
    p.grid_size = grid;
    p.seed = 1;
    worlds_core::save(&mut p, &dir).unwrap();
    let p = worlds_core::load(&dir).unwrap();
    (tmp, p)
}

fn action(plan: &Plan, kind: Kind, s: &str) -> Action {
    plan.item(kind, s)
        .unwrap_or_else(|| panic!("no {kind} {s} in the plan:\n{}", plan.text()))
        .action
}

/// Apply the plan's batch; check undo gives back the project exactly.
fn apply(dst: &mut Project, plan: &Plan) {
    let before = dst.clone();
    let applied = dst
        .apply(plan.command())
        .unwrap_or_else(|e| panic!("{e}\n{}", plan.text()));
    let mut undone = dst.clone();
    undone.apply(applied.inverse).unwrap();
    assert_eq!(undone, before, "undo of the import isn't exact");
}

fn policy(on_conflict: Conflict) -> Policy {
    Policy {
        on_conflict,
        ..Policy::default()
    }
}

#[test]
fn definitions_only_starts_a_sequel() {
    let src = game();
    let (_tmp, mut dst) = empty(16);
    let plan = merge::plan(
        &src,
        &dst,
        &Selection::preset(Preset::DefsOnly),
        &Policy::default(),
    )
    .unwrap();
    assert_eq!(plan.label, "Import from game");
    for (kind, s) in merge::catalog(&src) {
        if kind == Kind::Level {
            assert!(
                plan.item(kind, s.as_str()).is_none(),
                "level {s} came along"
            );
            continue;
        }
        // The empty project takes the game's grid: it has no levels yet.
        let a = action(&plan, kind, s.as_str());
        let want = if s == "grid_size" {
            Action::Overwrite
        } else {
            Action::Add
        };
        assert_eq!(a, want, "{kind} {s}");
    }
    assert!(plan.summary.starts_with(&format!(
        "{} definitions, 0 levels, 0 renamed, 0 skipped",
        plan.items.len()
    )));
    // The tileset images live outside the game project; they are copied
    // under art/ and the tilesets point there.
    let to: Vec<&str> = plan.assets.iter().map(|a| a.to.as_str()).collect();
    assert_eq!(to, ["art/tiles.png", "art/props.png"]);
    apply(&mut dst, &plan);
    assert_eq!(dst.grid_size, 32);
    assert_eq!(dst.tileset("tiles").unwrap().image, "art/tiles.png");
    assert_eq!(dst.layers.len(), src.layers.len());
    for (a, b) in dst.layers.iter().zip(&src.layers) {
        assert_eq!(a, b, "layers keep their draw order");
    }
    assert_eq!(dst.rules, src.rules);
    assert_eq!(dst.entities, src.entities);
    assert_eq!(dst.level_fields, src.level_fields);
    assert_eq!(dst.collision_layer, src.collision_layer);
    assert_eq!(dst.blocking_values, src.blocking_values);
    assert_eq!(dst.levels().count(), 0);

    // The files go across, and a second plan finds everything the same.
    let copied = merge::copy_assets(&plan, &game_dir(), dst.files.root().unwrap()).unwrap();
    assert_eq!(copied.len(), 2);
    let root = dst.files.root().unwrap().to_path_buf();
    worlds_core::save(&mut dst, &root).unwrap();
    let again = merge::plan(
        &src,
        &dst,
        &Selection::preset(Preset::DefsOnly),
        &Policy::default(),
    )
    .unwrap();
    assert!(
        again.items.iter().all(|i| i.action == Action::Same),
        "{}",
        again.text()
    );
    assert!(again.is_empty(), "{}", again.text());
    assert!(again.assets.iter().all(|a| a.action == Action::Same));
}

#[test]
fn everything_goes_into_a_new_world_named_after_the_source() {
    let src = mini();
    let (_tmp, mut dst) = empty(16);
    let plan = merge::plan(
        &src,
        &dst,
        &Selection::preset(Preset::Everything),
        &Policy::default(),
    )
    .unwrap();
    assert_eq!(action(&plan, Kind::World, "mini"), Action::Add);
    assert_eq!(action(&plan, Kind::Level, "crevasse"), Action::Add);
    assert_eq!(
        plan.item(Kind::Level, "shaft")
            .unwrap()
            .world
            .as_ref()
            .unwrap(),
        "mini"
    );
    assert!(plan.ids.is_empty(), "nothing collides: {:?}", plan.ids);
    assert!(plan.dropped.is_empty(), "{:?}", plan.dropped);
    apply(&mut dst, &plan);
    let w = dst.world("mini").unwrap();
    assert_eq!(w.name.as_deref(), Some("Mini"));
    assert_eq!(w.levels.len(), 2);
    // World notes come with the world; levels keep ids, content and position.
    assert_eq!(w.notes, src.world("main").unwrap().notes);
    for l in src.levels() {
        assert_eq!(dst.level(l.slug.as_str()).unwrap(), l);
    }
    // The import validates as cleanly as the source did.
    let msgs = |p: &Project| {
        let mut v: Vec<String> = worlds_core::validate(p)
            .iter()
            .map(|x| x.message.clone())
            .collect();
        v.sort();
        v
    };
    assert_eq!(msgs(&dst), msgs(&src));
}

#[test]
fn importing_a_project_into_itself_changes_nothing() {
    let src = mini();
    let dst = mini();
    let plan = merge::plan(
        &src,
        &dst,
        &Selection::preset(Preset::DefsOnly),
        &policy(Conflict::Rename),
    )
    .unwrap();
    assert!(plan.items.iter().all(|i| i.action == Action::Same));
    assert!(plan.commands().is_empty());
    assert_eq!(
        plan.summary,
        format!(
            "0 definitions, 0 levels, 0 renamed, 0 skipped, {} same",
            plan.items.len()
        )
    );
    // Levels are compared without their position and id.
    let sel = Selection {
        levels: vec![slug("shaft")],
        ..Selection::default()
    };
    let plan = merge::plan(&src, &dst, &sel, &policy(Conflict::Rename)).unwrap();
    assert_eq!(action(&plan, Kind::Level, "shaft"), Action::Same);
}

#[test]
fn rename_rewrites_references_inside_the_imported_set_and_rerolls_ids() {
    let src = mini();
    let mut dst = mini();
    // Ours differs: a door of another colour.
    let mut door = dst.entity_def("locked_door").unwrap().clone();
    door.color = worlds_core::Color::new("#000000").unwrap();
    dst.apply(Command::SetEntityDef {
        entity: slug("locked_door"),
        def: door,
    })
    .unwrap();
    let sel = Selection {
        levels: vec![slug("crevasse")],
        ..Selection::default()
    };
    let plan = merge::plan(&src, &dst, &sel, &policy(Conflict::Rename)).unwrap();
    let door = plan.item(Kind::Entity, "locked_door").unwrap();
    assert_eq!(door.action, Action::Rename, "{}", plan.text());
    assert_eq!(door.to.as_ref().unwrap(), "locked_door_2");
    assert_eq!(door.required_by, ["level:crevasse"]);
    // floor_button names locked_door in allowed_types: rewritten, so it
    // differs from ours now and is renamed too.
    assert_eq!(action(&plan, Kind::Entity, "floor_button"), Action::Rename);
    assert_eq!(action(&plan, Kind::Layer, "collision"), Action::Same);
    let lv = plan.item(Kind::Level, "crevasse").unwrap();
    assert_eq!(lv.action, Action::Rename);
    assert_eq!(lv.to.as_ref().unwrap(), "crevasse_2");
    // Every id collides: re-rolled, and references follow.
    let kinds: Vec<&str> = plan.ids.iter().map(|c| c.kind.as_str()).collect();
    assert_eq!(kinds, ["level", "entity", "entity", "note", "note"]);
    let new_id = |old: &str| {
        plan.ids
            .iter()
            .find(|c| c.from.as_str() == old)
            .map(|c| c.to.clone())
            .unwrap()
    };
    let before = dst.clone();
    apply(&mut dst, &plan);
    let l = dst.level("crevasse_2").unwrap();
    assert_eq!(l.id, new_id("crevasse22"));
    let btn = l.all_entities().find(|e| e.ty == "floor_button_2").unwrap();
    assert_eq!(btn.id, new_id("btncrev222"));
    assert_eq!(btn.fields["target"], new_id("doorcrev22").to_string());
    let door = l.all_entities().find(|e| e.ty == "locked_door_2").unwrap();
    // exit_to points at doorshaft2 in shaft: not imported, but ours has it.
    assert_eq!(door.fields["exit_to"], "doorshaft2");
    let note = l
        .notes
        .iter()
        .find(|n| n.id == new_id("notecrev22"))
        .unwrap();
    assert_eq!(
        note.links,
        [
            format!("entity:{}", new_id("btncrev222")),
            "note:pinstatue2".into(),
            "https://example.com".into()
        ]
    );
    let def = dst.entity_def("floor_button_2").unwrap();
    assert_eq!(
        def.fields[2].ty,
        worlds_core::FieldType::EntityRef {
            allowed_types: vec![slug("locked_door_2")],
            allowed_tags: vec![],
            cross_level: false
        }
    );
    // The same plan twice gives the same ids (they are derived).
    let again = merge::plan(&src, &before, &sel, &policy(Conflict::Rename)).unwrap();
    assert_eq!(
        again.ids.iter().find(|c| c.kind == "level").unwrap().to,
        new_id("crevasse22")
    );
}

#[test]
fn references_outside_the_imported_set_are_dropped_when_nothing_resolves_them() {
    let src = mini();
    let (_tmp, mut dst) = empty(16);
    let sel = Selection {
        levels: vec![slug("crevasse")],
        ..Selection::default()
    };
    let plan = merge::plan(&src, &dst, &sel, &Policy::default()).unwrap();
    assert_eq!(
        plan.dropped,
        [
            "entity doorcrev22 (locked_door): field exit_to pointed at entity doorshaft2, which \
             isn't imported and isn't in this project; set to null",
            "note notecrev22: link note:pinstatue2 doesn't resolve here; dropped"
        ]
    );
    // The level brought what it needs: its layers (with the auto layer
    // drawing from collision), entity types, their enums and tilesets.
    for (kind, s) in [
        (Kind::Layer, "collision"),
        (Kind::Layer, "main_tiling"),
        (Kind::Layer, "decor_back"),
        (Kind::Layer, "objects"),
        (Kind::Entity, "locked_door"),
        (Kind::Entity, "floor_button"),
        (Kind::Enum, "facing"),
        (Kind::Enum, "biome"),
        (Kind::Tileset, "castle_main"),
        (Kind::LevelField, "biome"),
        (Kind::LevelField, "music"),
    ] {
        let it = plan
            .item(kind, s)
            .unwrap_or_else(|| panic!("{kind} {s} not pulled in:\n{}", plan.text()));
        assert_eq!(it.action, Action::Add);
        assert!(!it.required_by.is_empty(), "{kind} {s}");
    }
    apply(&mut dst, &plan);
    let l = dst.level("crevasse").unwrap();
    let door = l
        .all_entities()
        .find(|e| e.id.as_str() == "doorcrev22")
        .unwrap();
    assert_eq!(door.fields["exit_to"], serde_json::Value::Null);
}

#[test]
fn placement_into_an_existing_world_at_an_offset() {
    let src = mini();
    let (_tmp, mut dst) = empty(16);
    let mut pol = Policy::default();
    pol.placement = Placement {
        mode: PlacementMode::World,
        world: Some(slug("main")),
        offset: [1000, -16],
    };
    let sel = Selection {
        worlds: vec![slug("main")],
        ..Selection::default()
    };
    let plan = merge::plan(&src, &dst, &sel, &pol).unwrap();
    assert_eq!(action(&plan, Kind::World, "main"), Action::Same);
    apply(&mut dst, &plan);
    assert_eq!(dst.worlds.len(), 1);
    assert_eq!(dst.level("shaft").unwrap().world_px, [1320, 64]);
    let pin = dst
        .find_note(&worlds_core::Id::new("pinstatue2").unwrap())
        .unwrap();
    assert_eq!(pin.note.shape, {
        let mut s = src
            .find_note(&worlds_core::Id::new("pinstatue2").unwrap())
            .unwrap()
            .note
            .shape
            .clone();
        s.translate([1000, -16]);
        s
    });

    // An unknown world, or a new world that exists, is refused.
    pol.placement.world = Some(slug("nowhere"));
    assert!(merge::plan(&src, &dst, &sel, &pol).is_err());
    pol.placement.mode = PlacementMode::NewWorld;
    pol.placement.world = Some(slug("main"));
    assert!(merge::plan(&src, &dst, &sel, &pol).is_err());
    // Keep: into the world of the same slug, made when missing.
    pol.placement = Placement {
        mode: PlacementMode::Keep,
        ..Placement::default()
    };
    let (_tmp2, fresh) = empty(16);
    let plan = merge::plan(&src, &fresh, &sel, &pol).unwrap();
    assert_eq!(action(&plan, Kind::World, "main"), Action::Same);
    assert_eq!(
        plan.item(Kind::Level, "crevasse")
            .unwrap()
            .world
            .as_ref()
            .unwrap(),
        "main"
    );
}

#[test]
fn overwrite_is_refused_where_it_would_orphan_content() {
    // Game's decor_back is on the project grid; mini's (with tiles in
    // crevasse) is on an 8 px grid, which a layer with content can't change.
    let src = game();
    let mut dst = mini();
    let sel = Selection {
        layers: vec![slug("decor_back")],
        entities: vec![slug("floor_button")],
        ..Selection::default()
    };
    let plan = merge::plan(&src, &dst, &sel, &policy(Conflict::Overwrite)).unwrap();
    let c = plan.item(Kind::Layer, "decor_back").unwrap();
    assert_eq!(c.action, Action::Refused, "{}", plan.text());
    assert!(
        c.reason
            .as_ref()
            .unwrap()
            .starts_with("overwrite refused: "),
        "{c:?}"
    );
    assert_eq!(
        action(&plan, Kind::Entity, "floor_button"),
        Action::Overwrite
    );
    assert!(plan.summary.contains("1 refused"), "{}", plan.summary);
    apply(&mut dst, &plan);
    assert_eq!(
        dst.entity_def("floor_button"),
        src.entity_def("floor_button")
    );
    assert_eq!(dst.layer("decor_back"), mini().layer("decor_back"));

    // Skip keeps ours; a per-item policy beats the default.
    let mut pol = policy(Conflict::Skip);
    pol.conflicts
        .insert("entity:floor_button".into(), Conflict::Rename);
    let plan = merge::plan(&src, &mini(), &sel, &pol).unwrap();
    assert_eq!(action(&plan, Kind::Layer, "decor_back"), Action::Skip);
    assert_eq!(action(&plan, Kind::Entity, "floor_button"), Action::Rename);
}

#[test]
fn levels_need_the_same_grid() {
    // game (32 px) into mini (16 px, with levels): the grid stays ours, so
    // the level is refused, and everything else still goes in.
    let src = game();
    let mut dst = mini();
    let sel = Selection {
        levels: vec![slug("level_2")],
        ..Selection::default()
    };
    let plan = merge::plan(&src, &dst, &sel, &policy(Conflict::Rename)).unwrap();
    let l = plan.item(Kind::Level, "level_2").unwrap();
    assert_eq!(l.action, Action::Refused, "{}", plan.text());
    assert!(l.reason.as_ref().unwrap().contains("32 px"));
    apply(&mut dst, &plan);
}

#[test]
fn game_levels_into_a_fresh_project() {
    let src = game();
    let (_tmp, mut dst) = empty(32);
    let plan = merge::plan(
        &src,
        &dst,
        &Selection::preset(Preset::Everything),
        &Policy::default(),
    )
    .unwrap();
    assert_eq!(plan.summary.split(", ").nth(1), Some("5 levels"));
    apply(&mut dst, &plan);
    assert!(
        worlds_core::validate(&dst).is_empty(),
        "{:?}",
        worlds_core::validate(&dst)
    );
    assert_eq!(dst.world("game").unwrap().levels.len(), 5);
    // The text form names every item.
    let text = plan.text();
    assert!(
        text.starts_with("Import from game (25 definitions, 5 levels"),
        "{text}"
    );
    assert!(
        text.contains("Levels:\n  add       crevasse (into world game)\n"),
        "{text}"
    );
}

#[test]
fn unknown_selection_is_refused() {
    let sel = Selection {
        levels: vec![slug("nowhere")],
        ..Selection::default()
    };
    let e = merge::plan(&mini(), &game(), &sel, &Policy::default()).unwrap_err();
    assert_eq!(e.to_string(), "no level \"nowhere\"");
}

#[test]
fn overwriting_an_auto_layer_takes_their_rule_set() {
    let mut src = mini();
    // A rule using the named pattern and legend of rules/macros.json.
    src.apply(
        serde_json::from_value(serde_json::json!({
            "op": "set_rule", "layer": "main_tiling", "group": "walls", "rule": "top_edge",
            "def": { "slug": "top_edge", "match": { "rows": "edge_top", "legend": "walls" },
                     "out": { "tileset": "castle_main", "tiles": [[1, 0]] } } }))
        .unwrap(),
    )
    .unwrap();
    let mut dst = game();
    let sel = Selection {
        layers: vec![slug("main_tiling")],
        ..Selection::default()
    };
    let mut pol = policy(Conflict::Skip);
    pol.conflicts
        .insert("layer:main_tiling".into(), Conflict::Overwrite);
    let plan = merge::plan(&src, &dst, &sel, &pol).unwrap();
    assert_eq!(action(&plan, Kind::Layer, "main_tiling"), Action::Overwrite);
    assert_eq!(action(&plan, Kind::Layer, "collision"), Action::Skip);
    // The macros its rules use come along, with their tilesets.
    for (kind, s) in [(Kind::Pattern, "edge_top"), (Kind::Legend, "walls")] {
        let it = plan.item(kind, s);
        assert!(
            it.is_some_and(|i| i.action == Action::Add),
            "{kind} {s}:\n{}",
            plan.text()
        );
    }
    apply(&mut dst, &plan);
    assert_eq!(dst.rules_for("main_tiling"), src.rules_for("main_tiling"));
    assert_eq!(dst.layer("main_tiling"), src.layer("main_tiling"));
}
