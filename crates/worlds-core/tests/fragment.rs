//! Clipboard fragments (copy, paste) and `duplicate_level`.

mod common;

use std::collections::BTreeMap;

use serde_json::json;
use worlds_core::command::{Command, CommandError, Rect};
use worlds_core::fragment::{self, CopySelection, Fragment, PasteOptions};
use worlds_core::*;

fn s(x: &str) -> Slug {
    slug(x)
}

fn id(x: &str) -> Id {
    Id::new(x).unwrap()
}

fn rows(p: &Project, level: &str, layer: &str) -> Vec<String> {
    let def = p.layer(layer).unwrap();
    p.level(level)
        .unwrap()
        .int_grid(layer)
        .map(|g| g.to_rows(def).unwrap())
        .unwrap_or_default()
}

fn entity<'p>(p: &'p Project, id: &Id) -> &'p EntityInstance {
    p.levels()
        .flat_map(|l| l.all_entities())
        .find(|e| e.id == *id)
        .unwrap_or_else(|| panic!("no entity {id}"))
}

/// Apply, check the inverse restores exactly and the inverse of the
/// inverse redoes exactly; leave it applied.
fn apply_exact(p: &mut Project, cmd: Command) {
    let json = serde_json::to_string(&cmd).unwrap();
    assert_eq!(serde_json::from_str::<Command>(&json).unwrap(), cmd);
    let before = p.clone();
    let a = p.apply(cmd).unwrap();
    assert!(!a.changes.is_empty());
    let after = p.clone();
    let b = p.apply(a.inverse).unwrap();
    assert!(*p == before, "undo isn't exact");
    p.apply(b.inverse).unwrap();
    assert!(*p == after, "redo isn't exact");
}

fn paste_at(level: &str, origin: [i64; 2]) -> PasteOptions {
    PasteOptions {
        level: s(level),
        origin,
        layer_map: BTreeMap::new(),
    }
}

#[test]
fn copy_entities_and_paste_remaps_references_together() {
    let mut p = common::fixture();
    let frag = fragment::copy(
        &p,
        &CopySelection {
            entities: vec![id("btncrev222"), id("doorcrev22")],
            ..Default::default()
        },
    )
    .unwrap();
    // Origin at the button's cell (12, 3); the door is 6 cells right.
    let src = frag.source.clone().unwrap();
    assert_eq!(src.level, Some(s("crevasse")));
    assert_eq!(src.origin, [12, 3]);
    assert_eq!(frag.size, [7, 1]);
    assert_eq!(frag.entities[0].cell, [0, 0]);
    assert_eq!(frag.entities[1].cell, [6, 0]);
    // Point fields are from the origin too.
    assert_eq!(
        frag.entities[1].fields["waypoints"],
        json!([[-11, -2], [-10, -2]])
    );
    // Round trip through text, as the clipboard does.
    let text = serde_json::to_string(&frag).unwrap();
    let back = Fragment::from_text(&text).unwrap().unwrap();
    assert_eq!(back, frag);

    let plan = fragment::paste(&p, &frag, &paste_at("crevasse", [2, 1])).unwrap();
    assert_eq!(plan.label, "Paste 2 items");
    assert!(plan.problems.is_empty(), "{:?}", plan.problems);
    apply_exact(&mut p, plan.command());
    let btn = entity(&p, &plan.ids[&id("btncrev222")]);
    let door = &plan.ids[&id("doorcrev22")];
    assert_eq!(btn.cell, [2, 1]);
    assert_eq!(btn.offset_px, [0, -2]);
    // The copied switch targets the copied door.
    assert_eq!(btn.fields["target"], json!(door.as_str()));
    let d = entity(&p, door);
    assert_eq!(d.cell, [8, 1]);
    // A cross-level reference outside the fragment that resolves is kept.
    assert_eq!(d.fields["exit_to"], json!("doorshaft2"));
    assert_eq!(d.fields["waypoints"], json!([[-9, -1], [-8, -1]]));
    assert_eq!(plan.entities.len(), 2);
}

#[test]
fn outside_references_are_kept_when_they_resolve_else_nulled() {
    let p = common::fixture();
    let frag = fragment::copy(
        &p,
        &CopySelection {
            entities: vec![id("btncrev222")],
            ..Default::default()
        },
    )
    .unwrap();
    // Same level: the original door is there.
    let plan = fragment::paste(&p, &frag, &paste_at("crevasse", [3, 3])).unwrap();
    let Command::AddEntity { entity, .. } = &plan.commands[0] else {
        panic!("{:?}", plan.commands)
    };
    assert_eq!(entity.fields["target"], json!("doorcrev22"));
    assert!(plan.problems.is_empty());
    // Another level: `target` isn't cross_level, so it is nulled.
    let plan = fragment::paste(&p, &frag, &paste_at("shaft", [1, 1])).unwrap();
    let Command::AddEntity { entity, .. } = &plan.commands[0] else {
        panic!()
    };
    assert_eq!(entity.fields["target"], json!(null));
    assert_eq!(plan.problems.len(), 1, "{:?}", plan.problems);
    assert!(plan.problems[0].contains("target"), "{:?}", plan.problems);
}

#[test]
fn grid_rect_paste_clips_and_undoes_exactly() {
    let mut p = common::fixture();
    let frag = fragment::copy(
        &p,
        &CopySelection {
            level: Some(s("crevasse")),
            rect: Some(Rect::new(3, 2, 4, 2)),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(frag.size, [4, 2]);
    let g = &frag.grids[&s("collision")];
    assert_eq!(g.rows, ["....", ".~~~"]);
    assert_eq!(g.legend[&'~'], Some(s("water")));
    // 2 cells off the right edge of the 20-wide level.
    let plan = fragment::paste(&p, &frag, &paste_at("crevasse", [18, 1])).unwrap();
    assert_eq!(plan.items, 4);
    assert!(
        plan.problems
            .iter()
            .any(|m| m.contains("4 cells fell outside")),
        "{:?}",
        plan.problems
    );
    apply_exact(&mut p, plan.command());
    let r = rows(&p, "crevasse", "collision");
    assert_eq!(&r[1][18..], "..");
    assert_eq!(&r[2][18..], ".~");
}

#[test]
fn spaces_leave_cells_and_unknown_layers_are_reported() {
    let p = common::fixture();
    let frag: Fragment = serde_json::from_value(json!({
        "worlds_fragment": 1, "grid_size": 16, "size": [3, 1],
        "grids": { "collision": { "rows": ["# ~"] }, "lava": { "rows": ["#"] } },
        "entities": [ { "id": "aaaaaaaaaa", "type": "floor_button", "cell": [0, 0] },
                      { "id": "bbbbbbbbbb", "type": "floor_button", "cell": [5, 0] },
                      { "id": "cccccccccc", "type": "ghost", "cell": [1, 0] } ]
    }))
    .unwrap();
    let plan = fragment::paste(&p, &frag, &paste_at("crevasse", [16, 2])).unwrap();
    let Command::PaintCells { cells, .. } = &plan.commands[0] else {
        panic!()
    };
    assert_eq!(cells.len(), 2);
    let msgs = plan.problems.join("\n");
    assert!(msgs.contains("layer lava: not in this project"), "{msgs}");
    assert!(msgs.contains("would land outside level crevasse"), "{msgs}");
    assert!(msgs.contains("no entity type ghost"), "{msgs}");
    assert_eq!(plan.entities.len(), 1);
    // A layer map can send it elsewhere; a fragment of another grid is refused.
    let mut opts = paste_at("crevasse", [0, 0]);
    opts.layer_map.insert(s("collision"), s("decor_back"));
    let plan = fragment::paste(&p, &frag, &opts).unwrap();
    assert!(plan.problems.iter().any(|m| m.contains("not int_grid")));
    let mut other = frag.clone();
    other.grid_size = 32;
    assert!(matches!(
        fragment::paste(&p, &other, &paste_at("crevasse", [0, 0])),
        Err(CommandError::Invalid(_))
    ));
}

#[test]
fn tiles_notes_and_links() {
    let mut p = common::fixture();
    let frag = fragment::copy(
        &p,
        &CopySelection {
            level: Some(s("crevasse")),
            tiles: [(s("decor_back"), vec![[4, 2], [5, 2]])].into(),
            notes: vec![id("notecrev22"), id("pinstatue2")],
            entities: vec![id("btncrev222")],
            ..Default::default()
        },
    )
    .unwrap();
    // decor_back is 8 px: its cells (4, 2) and (5, 2) are pixels 32..48, 16..24.
    assert_eq!(frag.source.as_ref().unwrap().origin, [2, 1]);
    assert_eq!(frag.tiles[&s("decor_back")].len(), 2);
    assert_eq!(frag.tiles[&s("decor_back")][0].cell, [0, 0]);
    let plan = fragment::paste(&p, &frag, &paste_at("crevasse", [0, 0])).unwrap();
    assert!(plan.problems.is_empty(), "{:?}", plan.problems);
    apply_exact(&mut p, plan.command());
    let lv = p.level("crevasse").unwrap();
    // The stacks at decor cells (4, 2) and (5, 2) went to (0, 0) and (1, 0).
    let cells: Vec<[u32; 2]> = lv
        .tiles_layer("decor_back")
        .unwrap()
        .tiles
        .iter()
        .map(|t| t.cell)
        .collect();
    assert!(
        cells.contains(&[0, 0]) && cells.contains(&[1, 0]),
        "{cells:?}"
    );
    // The note's entity link follows the pasted button; the pin (a world
    // note) comes in as a level note, and the note's link to it follows.
    let new_note = &plan.ids[&id("notecrev22")];
    let note = lv.notes.iter().find(|n| n.id == *new_note).unwrap();
    let btn = &plan.ids[&id("btncrev222")];
    assert!(
        note.links.contains(&format!("entity:{btn}")),
        "{:?}",
        note.links
    );
    let pin_new = &plan.ids[&id("pinstatue2")];
    assert!(
        note.links.contains(&format!("note:{pin_new}")),
        "{:?}",
        note.links
    );
    assert!(lv.notes.iter().any(|n| n.id == *pin_new));
    assert_eq!(lv.notes.len(), 4);
    assert_eq!(plan.items, 2 + 2 + 1);
}

#[test]
fn copy_refuses_mixed_levels_and_empty_selections() {
    let p = common::fixture();
    let e = fragment::copy(
        &p,
        &CopySelection {
            entities: vec![id("btncrev222"), id("doorshaft2")],
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(e.to_string().contains("one level"), "{e}");
    assert!(fragment::copy(&p, &CopySelection::default()).is_err());
    assert!(matches!(
        fragment::copy(
            &p,
            &CopySelection {
                entities: vec![id("zzzzzzzzzz")],
                ..Default::default()
            }
        ),
        Err(CommandError::UnknownEntity(_))
    ));
}

#[test]
fn fragment_schema_accepts_a_copy() {
    let p = common::fixture();
    let frag = fragment::copy(
        &p,
        &CopySelection {
            level: Some(s("crevasse")),
            rect: Some(Rect::new(0, 0, 20, 5)),
            layers: vec![s("collision"), s("decor_back")],
            entities: vec![id("doorcrev22")],
            notes: vec![id("notecrev22")],
            ..Default::default()
        },
    )
    .unwrap();
    let schema = fragment::schema();
    let v = jsonschema::validator_for(&schema).unwrap();
    let inst = serde_json::to_value(&frag).unwrap();
    assert!(v.is_valid(&inst), "{inst}");
    assert!(
        !v.is_valid(&json!({ "worlds_fragment": 1, "grid_size": 16, "size": [1, 1], "bogus": 1 }))
    );
    assert_eq!(frag.item_count(), 100 + 3 + 1 + 1);
}

#[test]
fn duplicate_level_copies_everything_with_new_ids() {
    let mut p = common::fixture();
    let cmd: Command = serde_json::from_value(json!({
        "op": "duplicate_level", "level": "crevasse", "slug": "crevasse_copy"
    }))
    .unwrap();
    assert_eq!(cmd.label(), "Duplicate level crevasse as crevasse_copy");
    apply_exact(&mut p, cmd.clone());
    let src = p.level("crevasse").unwrap().clone();
    let copy = p.level("crevasse_copy").unwrap().clone();
    assert_ne!(copy.id, src.id);
    assert_eq!(copy.name, "Crevasse copy");
    // Right of the original: crevasse is 320x80 px at the origin; shaft
    // (at y 80) doesn't reach up into 0..80, so the spot is free.
    assert_eq!(copy.world_px, [320, 0]);
    assert_eq!(copy.layers, src.layers);
    assert_eq!(copy.fields, src.fields);
    assert_eq!(
        rows(&p, "crevasse_copy", "collision"),
        rows(&p, "crevasse", "collision")
    );
    assert_eq!(copy.tiles, src.tiles);
    let ids: Vec<&Id> = copy.all_entities().map(|e| &e.id).collect();
    assert_eq!(ids.len(), 2);
    assert!(!ids.contains(&&id("doorcrev22")));
    let btn = copy
        .all_entities()
        .find(|e| e.ty == "floor_button")
        .unwrap();
    let door = copy.all_entities().find(|e| e.ty == "locked_door").unwrap();
    assert_eq!(btn.fields["target"], json!(door.id.as_str()));
    assert_eq!(door.fields["exit_to"], json!("doorshaft2"));
    let note = &copy.notes[0];
    assert_ne!(note.id, id("notecrev22"));
    assert!(note.links.contains(&format!("entity:{}", btn.id)));
    assert!(note.links.contains(&"note:pinstatue2".to_string()));
    assert!(
        validate(&p)
            .iter()
            .all(|pr| !pr.file.contains("crevasse_copy") || pr.message.contains("limit")),
        "{:?}",
        validate(&p)
    );

    // Same command again: the slug is taken.
    assert!(matches!(p.apply(cmd), Err(CommandError::DuplicateLevel(_))));
    // Shaft, where the default spot (416, 80) is free; into a given spot.
    apply_exact(
        &mut p,
        Command::DuplicateLevel {
            level: s("shaft"),
            slug: s("shaft_b"),
            name: Some("Shaft B".into()),
            world: None,
            world_px: Some([0, 400]),
        },
    );
    let b = p.level("shaft_b").unwrap();
    assert_eq!((b.name.as_str(), b.world_px), ("Shaft B", [0, 400]));
    assert_eq!(b.layers, p.level("shaft").unwrap().layers);
    // A copy of crevasse again goes past both crevasse_copy and shaft.
    p.apply(Command::DuplicateLevel {
        level: s("crevasse"),
        slug: s("third"),
        name: None,
        world: None,
        world_px: None,
    })
    .unwrap();
    assert_eq!(p.level("third").unwrap().world_px, [640, 0]);
    assert!(matches!(
        p.apply(Command::DuplicateLevel {
            level: s("crevasse"),
            slug: s("fourth"),
            name: None,
            world: Some(s("nowhere")),
            world_px: None,
        }),
        Err(CommandError::UnknownWorld(_))
    ));
}

#[test]
fn format_md_fragment_example_parses_and_pastes() {
    let doc = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/format.md"),
    )
    .unwrap();
    let text = doc
        .split("```json\n")
        .map(|b| b.split("\n```").next().unwrap())
        .find(|b| b.contains("\"worlds_fragment\""))
        .expect("a fragment example");
    let frag = Fragment::from_text(text).unwrap().unwrap();
    let p = common::fixture();
    let plan = fragment::paste(&p, &frag, &paste_at("crevasse", [2, 1])).unwrap();
    assert!(plan.problems.is_empty(), "{:?}", plan.problems);
    let mut q = p.clone();
    q.apply(plan.command()).unwrap();
}
