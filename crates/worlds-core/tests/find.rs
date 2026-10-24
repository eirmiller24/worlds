//! `worlds_core::find` on mini.worlds: each kind, the field tests against
//! effective values, and Replace as one batch.

mod common;

use serde_json::json;
use worlds_core::find::{self, FindKind, Query};
use worlds_core::{Command, Id, Slug};

fn q(s: &str) -> Query {
    Query::parse(s).unwrap_or_else(|e| panic!("{s}: {e}"))
}

fn entity_ids(p: &worlds_core::Project, s: &str) -> Vec<String> {
    find::entities(p, &q(s))
        .unwrap()
        .iter()
        .map(|h| h.entity.id.to_string())
        .collect()
}

#[test]
fn entities_by_type_tag_field_and_text() {
    let p = common::fixture();
    assert_eq!(
        entity_ids(&p, ""),
        ["doorcrev22", "btncrev222", "doorshaft2"]
    );
    assert_eq!(
        entity_ids(&p, "type:locked_door"),
        ["doorcrev22", "doorshaft2"]
    );
    assert_eq!(entity_ids(&p, "type:*door level:shaft"), ["doorshaft2"]);
    assert_eq!(entity_ids(&p, "tag:puzzle channel:3"), ["btncrev222"]);
    // Defaults count: doorshaft2 stores no needs_item, so it is "".
    assert_eq!(entity_ids(&p, r#"needs_item:"""#), ["doorshaft2"]);
    assert_eq!(
        entity_ids(&p, "type:locked_door field:needs_item != \"\""),
        ["doorcrev22"]
    );
    // The button has no needs_item at all: null, which isn't "".
    assert_eq!(
        entity_ids(&p, "field:needs_item != \"\""),
        ["doorcrev22", "btncrev222"]
    );
    // A field the type has, with no default and nothing stored, is null.
    assert_eq!(
        entity_ids(&p, "type:locked_door field:label = null").len(),
        2
    );
    // A field no type has is null everywhere.
    assert_eq!(entity_ids(&p, "field:nope = null").len(), 3);
    assert_eq!(entity_ids(&p, "speed:2.0"), ["doorcrev22"]);
    assert_eq!(entity_ids(&p, "button"), ["btncrev222"]);
    assert_eq!(entity_ids(&p, "LOCKED shaft2"), ["doorshaft2"]);
    assert_eq!(entity_ids(&p, "layer:objects world:main").len(), 3);
    assert!(entity_ids(&p, "world:other").is_empty());
    assert!(find::entities(&p, &q("kind:pin")).is_err());
}

#[test]
fn levels_notes_and_tiles() {
    let p = common::fixture();
    let levels = |s: &str| -> Vec<String> {
        find::levels(&p, &q(s))
            .unwrap()
            .iter()
            .map(|h| h.level.slug.to_string())
            .collect()
    };
    assert_eq!(levels(""), ["crevasse", "shaft"]);
    assert_eq!(levels("field:music = null"), ["crevasse", "shaft"]);
    assert_eq!(levels(r#"biome:["flooded"]"#), ["crevasse"]);
    assert_eq!(levels("biome:[]"), ["shaft"]);
    assert_eq!(levels("Shaft"), ["shaft"]);
    assert!(find::levels(&p, &q("type:x")).is_err());

    let notes = |s: &str| -> Vec<String> {
        find::notes(&p, &q(s))
            .unwrap()
            .iter()
            .map(|h| h.note.id.to_string())
            .collect()
    };
    assert_eq!(
        notes(""),
        [
            "guidestem2",
            "pinstatue2",
            "regioneast",
            "notecrev22",
            "pathcrev22"
        ]
    );
    assert_eq!(notes("too easy"), ["notecrev22"]);
    assert_eq!(notes("\"too easy\""), ["notecrev22"]);
    // A pin's label is text too.
    assert_eq!(notes("statue kind:pin"), ["pinstatue2"]);
    assert_eq!(notes("status:done"), ["pathcrev22"]);
    assert_eq!(notes("tag:todo level:crevasse"), ["notecrev22"]);
    // level: leaves world notes out.
    assert_eq!(notes("level:crevasse"), ["notecrev22", "pathcrev22"]);

    let tiles = |s: &str| -> Vec<([u32; 2], String)> {
        find::tiles(&p, &q(s))
            .unwrap()
            .iter()
            .map(|h| (h.tile.cell, h.layer.to_string()))
            .collect()
    };
    assert_eq!(tiles("").len(), 3);
    assert_eq!(tiles("tileset:castle_main").len(), 2);
    assert_eq!(
        tiles("tileset:castle_main tile:6,1"),
        [([5, 2], "decor_back".to_string())]
    );
    assert!(tiles("level:shaft").is_empty());
    assert!(find::tiles(&p, &q("castle")).is_err());
    assert!(q("tileset:x").applies(FindKind::Tiles));
}

#[test]
fn replace_is_one_batch_over_the_entities_that_have_the_field() {
    let mut p = common::fixture();
    let ids: Vec<Id> = find::entities(&p, &q(""))
        .unwrap()
        .iter()
        .map(|h| h.entity.id.clone())
        .collect();
    let field = Slug::new("channel").unwrap();
    let (cmd, n) = find::replace_field(&p, &ids, &field, &json!(7)).unwrap();
    assert_eq!(n, 3);
    let Command::Batch { label, commands } = &cmd else {
        panic!("{cmd:?}")
    };
    assert_eq!(label, "Replace channel in 3 entities");
    assert_eq!(commands.len(), 3);
    p.apply(cmd).unwrap();
    assert_eq!(entity_ids(&p, "channel:7").len(), 3);
    // Only the button has a target.
    let target = Slug::new("target").unwrap();
    let (cmd, n) = find::replace_field(&p, &ids, &target, &json!(null)).unwrap();
    assert_eq!(n, 1);
    assert!(matches!(&cmd, Command::Batch { label, .. } if label == "Replace target in 1 entity"));
    let nope = Slug::new("nope").unwrap();
    assert!(find::replace_field(&p, &ids, &nope, &json!(1)).is_none());
}

#[test]
fn replace_values_follow_the_field_type() {
    let p = common::fixture();
    let def = |ty: &str, f: &str| p.entity_def(ty).unwrap().field(f).unwrap().clone();
    assert_eq!(
        find::replace_value(&def("floor_button", "channel"), "3"),
        json!(3)
    );
    assert_eq!(
        find::replace_value(&def("locked_door", "needs_item"), "3"),
        json!("3")
    );
    assert_eq!(
        find::replace_value(&def("locked_door", "needs_item"), " brass key "),
        json!("brass key")
    );
    assert_eq!(
        find::replace_value(&def("locked_door", "label"), "null"),
        json!(null)
    );
    assert_eq!(
        find::replace_value(&def("locked_door", "waypoints"), "[[1, 2]]"),
        json!([[1, 2]])
    );
    assert_eq!(
        find::replace_value(&def("floor_button", "channel"), "\"3\""),
        json!("3")
    );
}
