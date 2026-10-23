//! Connectors (format.md "Connectors"): the file form, computed ends that
//! follow their rooms and entities, validation of dangling ends, the text
//! view and the export.

mod common;

use std::collections::BTreeMap;

use serde_json::{Value, json};
use worlds_core::command::NoteScope;
use worlds_core::export::{self, BakedLevel};
use worlds_core::*;

/// format.md's example, world scope in the mini fixture.
fn example() -> Annotation {
    serde_json::from_value(json!({
        "id": "conncrev22", "kind": "connector", "title": "Back up", "label": "needs double jump",
        "from": { "level": "crevasse" }, "to": { "entity": "doorshaft2" },
        "waypoints": [[360, 40]], "style": "double"
    }))
    .unwrap()
}

fn with_connector(n: Annotation) -> Project {
    let mut p = common::fixture();
    p.apply(Command::AddNote {
        scope: NoteScope::World(Slug::new("main").unwrap()),
        note: n,
        index: None,
    })
    .unwrap();
    p
}

fn line(p: &Project, id: &str) -> Vec<[i64; 2]> {
    let at = p.find_note(&Id::new(id).unwrap()).unwrap();
    p.connector_points(at).unwrap()
}

#[test]
fn the_file_form_round_trips_and_keeps_its_key_order() {
    let n = example();
    let Shape::Connector {
        from, to, style, ..
    } = &n.shape
    else {
        panic!("{n:?}")
    };
    assert_eq!(*from, Endpoint::Level(Slug::new("crevasse").unwrap()));
    assert_eq!(*to, Endpoint::Entity(Id::new("doorshaft2").unwrap()));
    assert_eq!(*style, ConnectorStyle::Double);
    assert_eq!(
        serde_json::to_string(&n).unwrap(),
        r#"{"id":"conncrev22","kind":"connector","title":"Back up","label":"needs double jump","from":{"level":"crevasse"},"to":{"entity":"doorshaft2"},"waypoints":[[360,40]],"style":"double"}"#
    );
    // The default style and no waypoints are left out.
    let plain: Annotation = serde_json::from_value(json!({
        "id": "conncrev23", "kind": "connector", "from": { "level": "crevasse" },
        "to": { "level": "shaft" }, "style": "arrow"
    }))
    .unwrap();
    assert_eq!(
        serde_json::to_value(&plain).unwrap(),
        json!({ "id": "conncrev23", "kind": "connector", "from": { "level": "crevasse" },
                "to": { "level": "shaft" } })
    );
}

#[test]
fn wrong_keys_are_load_errors() {
    let bad = |v: Value| {
        serde_json::from_value::<Annotation>(v)
            .unwrap_err()
            .to_string()
    };
    let e = bad(
        json!({ "id": "aaaabbbbcc", "kind": "path", "points": [[0, 0], [1, 1]],
                        "from": { "level": "crevasse" } }),
    );
    assert!(e.contains("\"from\" is not allowed on path"), "{e}");
    let e = bad(
        json!({ "id": "aaaabbbbcc", "kind": "connector", "from": { "level": "crevasse" },
                        "to": { "level": "shaft" }, "points": [[0, 0]] }),
    );
    assert!(e.contains("\"points\" is not allowed on connector"), "{e}");
    let e =
        bad(json!({ "id": "aaaabbbbcc", "kind": "connector", "from": { "level": "crevasse" } }));
    assert!(e.contains("needs \"to\""), "{e}");
    let e = bad(
        json!({ "id": "aaaabbbbcc", "kind": "connector", "to": { "level": "crevasse" },
                        "from": { "level": "crevasse", "entity": "doorshaft2" } }),
    );
    assert!(!e.is_empty());
    let e = bad(
        json!({ "id": "aaaabbbbcc", "kind": "connector", "from": { "level": "crevasse" },
                        "to": { "level": "shaft" }, "style": "dotted" }),
    );
    assert!(e.contains("dotted"), "{e}");
}

/// Its ends are computed, so a moved room or entity moves the line, and
/// undoing the move moves it back.
#[test]
fn ends_follow_rooms_and_entities() {
    let mut p = with_connector(example());
    // crevasse is [0, 0, 320, 80]; the waypoint is right of it; the door's
    // pivot is shaft's [320, 80] + ((2 + 0.5) * 16, (7 + 1) * 16).
    assert_eq!(line(&p, "conncrev22"), [[320, 40], [360, 40], [360, 208]]);
    let applied = p
        .apply(Command::MoveLevel {
            world: Slug::new("main").unwrap(),
            level: Slug::new("shaft").unwrap(),
            world_px: [400, 80],
        })
        .unwrap();
    assert_eq!(line(&p, "conncrev22"), [[320, 40], [360, 40], [440, 208]]);
    p.apply(applied.inverse).unwrap();
    let applied = p
        .apply(Command::MoveLevel {
            world: Slug::new("main").unwrap(),
            level: Slug::new("crevasse").unwrap(),
            world_px: [0, 100],
        })
        .unwrap();
    // The waypoint is now above crevasse: the end leaves through its top.
    assert_eq!(line(&p, "conncrev22"), [[320, 100], [360, 40], [360, 208]]);
    p.apply(applied.inverse).unwrap();
    // A level-scope connector's waypoints are in its level's pixels.
    let mut n = example();
    n.id = Id::new("conncrev24").unwrap();
    n.shape.translate([-320, -80]);
    let mut p = common::fixture();
    p.apply(Command::AddNote {
        scope: NoteScope::Level(Slug::new("shaft").unwrap()),
        note: n,
        index: None,
    })
    .unwrap();
    assert_eq!(line(&p, "conncrev24"), [[320, 40], [360, 40], [360, 208]]);
}

#[test]
fn a_dangling_end_is_a_problem() {
    let mut n = example();
    n.shape = Shape::Connector {
        from: Endpoint::Level(Slug::new("nowhere").unwrap()),
        to: Endpoint::Entity(Id::new("gonegone22").unwrap()),
        waypoints: Vec::new(),
        label: None,
        style: ConnectorStyle::Arrow,
    };
    let p = with_connector(n);
    let problems: Vec<String> = validate(&p)
        .iter()
        .map(|x| x.to_string())
        .filter(|x| x.contains("conncrev22"))
        .collect();
    assert_eq!(
        problems,
        [
            "worlds/main/world.json: notes[conncrev22].from: level \"nowhere\" does not exist: the connector's end dangles",
            "worlds/main/world.json: notes[conncrev22].to: entity gonegone22 does not exist: the connector's end dangles",
        ]
    );
    assert!(
        p.connector_points(p.find_note(&Id::new("conncrev22").unwrap()).unwrap())
            .is_none()
    );
    // A valid one has no problem.
    let p = with_connector(example());
    assert!(
        !validate(&p)
            .iter()
            .any(|x| x.to_string().contains("conncrev22")),
        "{:?}",
        validate(&p)
    );
}

#[test]
fn an_end_in_another_world_is_a_problem() {
    let mut p = with_connector(example());
    let mut w = World::new(Slug::new("other").unwrap());
    let shaft = p.worlds[0]
        .levels
        .iter()
        .position(|l| l.slug == "shaft")
        .unwrap();
    w.levels.push(p.worlds[0].levels.remove(shaft));
    p.worlds.push(w);
    let problems: Vec<String> = validate(&p)
        .iter()
        .map(|x| x.to_string())
        .filter(|x| x.contains("conncrev22"))
        .collect();
    assert_eq!(
        problems,
        [
            "worlds/main/world.json: notes[conncrev22].to: entity doorshaft2 is in world \"other\", not the connector's world \"main\""
        ]
    );
}

#[test]
fn the_text_view_names_the_ends() {
    let p = with_connector(example());
    let text = text::render_level(&p, "shaft", None).unwrap();
    assert!(
        text.contains(
            "  conncrev22 connector open from level:crevasse to entity:doorshaft2 waypoints [[360, 40]] label \"needs double jump\" style double title \"Back up\""
        ),
        "{text}"
    );
}

#[test]
fn the_export_resolves_the_ends() {
    let mut n = example();
    n.export = true;
    n.locked = true;
    let p = with_connector(n);
    let tmp = tempfile::tempdir().unwrap();
    let baked: BTreeMap<Slug, BakedLevel> = p
        .levels()
        .map(|l| (l.slug.clone(), BakedLevel::default()))
        .collect();
    export::write(&p, tmp.path(), &tmp.path().join("export"), &baked, true).unwrap();
    let world: Value = serde_json::from_str(
        &std::fs::read_to_string(tmp.path().join("export/world.json")).unwrap(),
    )
    .unwrap();
    let c = world["worlds"][0]["notes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["id"] == "conncrev22")
        .unwrap();
    assert_eq!(
        *c,
        json!({ "id": "conncrev22", "kind": "connector", "title": "Back up", "label": "needs double jump",
                "from": { "level": "crevasse" }, "to": { "entity": "doorshaft2", "level": "shaft" },
                "waypoints": [[360, 40]], "style": "double", "points": [[320, 40], [360, 40], [360, 208]] })
    );
    // A dangling one isn't exported.
    let mut n = example();
    n.export = true;
    n.shape = Shape::Connector {
        from: Endpoint::Level(Slug::new("nowhere").unwrap()),
        to: Endpoint::Level(Slug::new("shaft").unwrap()),
        waypoints: Vec::new(),
        label: None,
        style: ConnectorStyle::Arrow,
    };
    let p = with_connector(n);
    assert!(!export::world_json(&p).contains("conncrev22"));
}
