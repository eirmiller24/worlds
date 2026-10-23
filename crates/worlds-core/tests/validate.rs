mod common;

use worlds_core::*;

/// Apply `edit` to the fixture and assert some problem in `file` contains `needle`.
fn expect(file: &str, needle: &str, edit: impl FnOnce(&mut Project)) {
    let mut p = common::fixture();
    edit(&mut p);
    let problems = validate(&p);
    assert!(
        problems
            .iter()
            .any(|pr| pr.file == file && pr.to_string().contains(needle)),
        "expected a problem in {file} containing {needle:?}, got:\n{}",
        problems
            .iter()
            .map(|p| p.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    );
}

fn crevasse(p: &mut Project) -> &mut Level {
    p.worlds[0]
        .levels
        .iter_mut()
        .find(|l| l.slug == "crevasse")
        .unwrap()
}

fn door(p: &mut Project) -> &mut EntityInstance {
    crevasse(p).entities[0]
        .entities
        .iter_mut()
        .find(|e| e.ty == "locked_door")
        .unwrap()
}

fn first_rule(p: &mut Project) -> &mut Rule {
    p.rules[0].groups[0].rules[0].as_rule_mut().unwrap()
}

fn legend(p: &mut Pattern) -> &mut std::collections::BTreeMap<char, CellMatch> {
    match &mut p.legend {
        Legend::Inline(m) => m,
        Legend::Named(_) => panic!("named legend"),
    }
}

fn rows(p: &mut Pattern) -> &mut Vec<String> {
    match &mut p.rows {
        Rows::Inline(r) => r,
        Rows::Named(_) => panic!("named rows"),
    }
}

fn collision(p: &mut Project) -> &mut IntGridDef {
    p.layers[0].int_grid_mut().unwrap()
}

const CREV: &str = "worlds/main/levels/crevasse";

#[test]
fn dangling_layer_source() {
    expect(
        "layers.json",
        "layers[1].source: unknown layer \"nope\"",
        |p| {
            p.layers[1].auto_mut().unwrap().source = slug("nope");
        },
    );
    expect("layers.json", "is not an int_grid layer", |p| {
        p.layers[1].auto_mut().unwrap().source = slug("decor_back");
    });
}

#[test]
fn dangling_tileset_refs() {
    expect(
        "rules/main_tiling.json",
        "out.tileset: unknown tileset \"gone\"",
        |p| {
            first_rule(p)
                .out_mut()
                .unwrap()
                .as_tiles_mut()
                .unwrap()
                .tileset = slug("gone");
        },
    );
    // Tiles are named by cell, not by their index in the sorted list.
    expect(
        &format!("{CREV}/decor_back.json"),
        "tiles[4,1].tileset: unknown tileset",
        |p| {
            crevasse(p).tiles[0].tiles[0].tileset = slug("gone");
        },
    );
    expect("entities/locked_door.json", "icon.tileset", |p| {
        p.entities[1].icon.as_mut().unwrap().tileset = slug("gone");
    });
}

#[test]
fn dangling_enum_and_entity_type() {
    expect(
        "entities/locked_door.json",
        "unknown enum \"compass\"",
        |p| {
            p.entities[1].fields[3].ty = FieldType::Enum(EnumSource::Project(slug("compass")));
        },
    );
    expect(
        &format!("{CREV}/entities.json"),
        "unknown entity type \"ghost\"",
        |p| {
            door(p).ty = slug("ghost");
        },
    );
    expect(
        "entities/floor_button.json",
        "unknown entity type \"ghost\"",
        |p| {
            let FieldType::EntityRef { allowed_types, .. } = &mut p.entities[0].fields[2].ty else {
                panic!("target is an entity_ref");
            };
            *allowed_types = vec![slug("ghost")];
        },
    );
}

#[test]
fn dangling_entity_refs() {
    expect(
        &format!("{CREV}/entities.json"),
        "no entity with id \"zzzzzzzzzz\"",
        |p| {
            door(p).fields.insert(slug("exit_to"), "zzzzzzzzzz".into());
        },
    );
    // floor_button.target is not cross_level; point it at the other level.
    expect(
        &format!("{CREV}/entities.json"),
        "cross_level is off",
        |p| {
            let btn = crevasse(p).entities[0]
                .entities
                .iter_mut()
                .find(|e| e.ty == "floor_button")
                .unwrap();
            btn.fields.insert(slug("target"), "doorshaft2".into());
        },
    );
    // target only allows locked_door.
    expect(&format!("{CREV}/entities.json"), "doesn't allow", |p| {
        let btn = crevasse(p).entities[0]
            .entities
            .iter_mut()
            .find(|e| e.ty == "floor_button")
            .unwrap();
        btn.fields.insert(slug("target"), "btncrev222".into());
    });
    expect(
        &format!("{CREV}/notes.json"),
        "\"entity:aaaaaaaaaa\" does not resolve",
        |p| {
            crevasse(p).notes[0].links.push("entity:aaaaaaaaaa".into());
        },
    );
}

#[test]
fn grid_size_must_fit_the_level() {
    let f = format!("{CREV}/collision.json");
    expect(
        &f,
        "rows: rows are 19 glyphs wide, level is 20 cells wide",
        |p| {
            crevasse(p).int_grids[0] =
                IntGridLayer::from_cells(slug("collision"), [19, 5], vec![0; 95]);
        },
    );
    expect(&f, "rows: 4 rows, level is 5 cells tall", |p| {
        crevasse(p).int_grids[0] =
            IntGridLayer::from_cells(slug("collision"), [20, 4], vec![0; 80]);
    });
    expect(
        &f,
        "rows[2]: cell [1, 2] holds value #9, but the layer has 5 values",
        |p| {
            crevasse(p).int_grids[0].set_index(1, 2, 9);
        },
    );
}

#[test]
fn per_layer_grids() {
    // decor_back is on an 8 px grid over the 16 px project: 40x10 cells.
    let f = format!("{CREV}/decor_back.json");
    expect(
        &f,
        "tiles[40,0]: [40, 0] is outside the level (40x10)",
        |p| {
            crevasse(p).tiles[0].tiles[0].cell = [40, 0];
        },
    );
    assert!(
        validate(&{
            let mut p = common::fixture();
            crevasse(&mut p).tiles[0].tiles[0].cell = [39, 9];
            p
        })
        .is_empty()
    );
    expect(
        "layers.json",
        "neither divides nor is a multiple of the project grid",
        |p| {
            p.layers[2].grid_size = Some(12);
        },
    );
    expect("layers.json", "an auto layer uses its source's grid", |p| {
        p.layers[1].grid_size = Some(8);
    });
    // A pattern can't read a grid on another cell size than the layer bakes on.
    expect(
        "rules/main_tiling.json",
        "is on a 8 px grid, but this layer bakes on 16 px",
        |p| {
            let fine = LayerDef::new(
                slug("fine"),
                "Fine",
                LayerKind::IntGrid(IntGridDef::default()),
            );
            let mut fine = fine;
            fine.grid_size = Some(8);
            p.layers.push(fine);
            first_rule(p).as_match_mut().unwrap().pattern.grid = Some(slug("fine"));
        },
    );
}

#[test]
fn entity_outside_level() {
    // Entities are named by id, whatever their position in the file.
    expect(
        &format!("{CREV}/entities.json"),
        "layers[objects].entities[doorcrev22].cell: [20, 3] is outside the level (20x5)",
        |p| {
            door(p).cell = [20, 3];
        },
    );
    expect(&format!("{CREV}/entities.json"), "outside the level", |p| {
        door(p).cell = [-1, 0];
    });
}

/// A repeated entity or note id is reported in the file holding the repeat
/// (world name included), at the instance, like every other problem.
#[test]
fn duplicate_ids_name_the_file_and_the_instance() {
    let mut p = common::fixture();
    let shaft = p.worlds[0]
        .levels
        .iter()
        .position(|l| l.slug == "shaft")
        .unwrap();
    let door = door(&mut p).clone();
    let note = crevasse(&mut p).notes[0].clone();
    let layer = p.worlds[0].levels[shaft].entities[0].layer.clone();
    p.worlds[0].levels[shaft].entities[0]
        .entities
        .push(door.clone());
    p.worlds[0].levels[shaft].notes.push(note.clone());
    let problems = validate(&p);
    let want = |file: String, path: String, msg: String| {
        assert!(
            problems
                .iter()
                .any(|pr| pr.file == file && pr.path == path && pr.message == msg),
            "no {file}: {path}: {msg} in\n{}",
            problems
                .iter()
                .map(|p| p.to_string())
                .collect::<Vec<_>>()
                .join("\n")
        );
    };
    want(
        "worlds/main/levels/shaft/entities.json".into(),
        format!("layers[{layer}].entities[{}].id", door.id),
        format!("duplicate entity id {:?}", door.id.as_str()),
    );
    want(
        "worlds/main/levels/shaft/notes.json".into(),
        format!("notes[{}].id", note.id),
        format!("duplicate note id {:?}", note.id.as_str()),
    );
    assert!(problems.iter().all(|pr| pr.file != "worlds"
        || !pr.message.contains("entity id") && !pr.message.contains("note id")));
}

#[test]
fn duplicate_slugs() {
    expect("layers.json", "duplicate layer \"collision\"", |p| {
        let l = p.layers[0].clone();
        p.layers.push(l);
    });
    expect("entities", "duplicate entity type \"floor_button\"", |p| {
        let e = p.entities[0].clone();
        p.entities.push(e);
    });
    expect("worlds", "duplicate level slug \"crevasse\"", |p| {
        let mut l = crevasse(p).clone();
        l.id = Id::generate();
        p.worlds[0].levels.push(l);
    });
    expect(
        "rules/main_tiling.json",
        "duplicate rule slug \"wall_top_edge\"",
        |p| {
            let r = first_rule(p).clone();
            p.rules[0].groups[1].rules.push(RuleEntry::Rule(r));
        },
    );
    expect("layers.json", "duplicate value or group \"walls\"", |p| {
        collision(p).values[2].slug = slug("walls");
    });
    expect("layers.json", "duplicate glyph '#'", |p| {
        collision(p).values[2].glyph = '#';
    });
    // Value and group glyphs share a namespace too.
    expect("layers.json", "groups[0].glyph: duplicate glyph 'o'", |p| {
        collision(p).groups[0].glyph = Some('o');
    });
}

#[test]
fn reserved_glyphs_in_memory() {
    for c in ['\\', '"', '*', '+', '!', '@', '[', ']', '|', '.', ' '] {
        expect("layers.json", "layers[0].values[2].glyph", |p| {
            collision(p).values[2].glyph = c;
        });
    }
}

#[test]
fn field_values_must_match_type() {
    let f = format!("{CREV}/entities.json");
    expect(&f, "fields.speed: expected a number", |p| {
        door(p).fields.insert(slug("speed"), "fast".into());
    });
    expect(&f, "fields.speed: 0.1 is below min 0.5", |p| {
        door(p).fields.insert(slug("speed"), 0.1.into());
    });
    expect(
        &f,
        "fields.facing: \"up\" is not a value of this enum",
        |p| {
            door(p).fields.insert(slug("facing"), "up".into());
        },
    );
    expect(&f, "fields.tint: expected a #rrggbb color", |p| {
        door(p).fields.insert(slug("tint"), "red".into());
    });
    expect(
        &f,
        "fields.waypoints[1]: point [99, 1] is outside the level",
        |p| {
            door(p)
                .fields
                .insert(slug("waypoints"), serde_json::json!([[1, 1], [99, 1]]));
        },
    );
    expect(&f, "fields.needs_item: null is not allowed", |p| {
        door(p)
            .fields
            .insert(slug("needs_item"), serde_json::Value::Null);
    });
    expect(&f, "has no field \"colour\"", |p| {
        door(p).fields.insert(slug("colour"), 1.into());
    });
    expect(&f, "does not have one of the extensions", |p| {
        door(p).fields.insert(slug("script"), "door.py".into());
    });
    expect(
        &format!("{CREV}/level.json"),
        "fields.biome[0]: \"lava\" is not a value",
        |p| {
            crevasse(p)
                .fields
                .insert(slug("biome"), serde_json::json!(["lava"]));
        },
    );
    expect(
        "entities/locked_door.json",
        "fields[0].default: expected an integer",
        |p| {
            p.entities[1].fields[0].default = Some("zero".into());
        },
    );
}

#[test]
fn rule_checks() {
    let f = "rules/main_tiling.json";
    expect(f, "width and height must be odd", |p| {
        first_rule(p).as_match_mut().unwrap().pattern.rows =
            Rows::Inline(vec!["##".into(), "##".into()]);
    });
    expect(f, "legend.x: \"lava\" is not a value or group", |p| {
        legend(&mut first_rule(p).as_match_mut().unwrap().pattern)
            .insert('x', CellMatch::Not(slug("lava")));
    });
    expect(f, "legend.|: '|' can't be a pattern glyph", |p| {
        legend(&mut first_rule(p).as_match_mut().unwrap().pattern).insert('|', CellMatch::Any);
    });
    expect(
        f,
        "legend.@: '@' is only a legend key in a pattern whose rows contain @",
        |p| {
            legend(&mut first_rule(p).as_match_mut().unwrap().pattern).insert('@', CellMatch::Any);
        },
    );
    expect(f, "out.stamp: unknown stamp \"nope\"", |p| {
        first_rule(p).output = Output::Draw(RuleOutput::stamp(slug("nope")));
    });
    expect(f, "out.tiles: needs at least one alternative", |p| {
        first_rule(p)
            .out_mut()
            .unwrap()
            .as_tiles_mut()
            .unwrap()
            .tiles
            .clear();
    });
    expect(f, "pivot only applies with tile_mode \"stamp\"", |p| {
        first_rule(p)
            .out_mut()
            .unwrap()
            .as_tiles_mut()
            .unwrap()
            .pivot = [0.5, 0.5];
    });
    // Value and group glyphs mean themselves; anything else must be in the legend.
    expect(f, "glyph 'q' means nothing", |p| {
        rows(&mut first_rule(p).as_match_mut().unwrap().pattern)[0] = "*q*".into();
    });
    assert!(
        validate(&{
            let mut p = common::fixture();
            rows(&mut first_rule(&mut p).as_match_mut().unwrap().pattern)[0] = "fw~".into();
            p
        })
        .is_empty()
    );
    expect(f, "requires_level_field.field: unknown level field", |p| {
        p.rules[0].groups[1]
            .requires_level_field
            .as_mut()
            .unwrap()
            .field = slug("weather");
    });
    fn state(p: &mut Project) -> &mut LayerState {
        crevasse(p).layers.get_mut("main_tiling").unwrap()
    }
    expect(
        &format!("{CREV}/level.json"),
        "rule group \"walls\" is always on, not optional",
        |p| state(p).optional_groups.push(slug("walls")),
    );
    expect(
        &format!("{CREV}/level.json"),
        "rule group \"retired\" is off, not optional",
        |p| state(p).optional_groups.push(slug("retired")),
    );
    expect(
        &format!("{CREV}/level.json"),
        "layers.main_tiling.tilesets.castle_main: unknown tileset \"gone\"",
        |p| {
            state(p).tilesets.insert(slug("castle_main"), slug("gone"));
        },
    );
}

#[test]
fn layer_state_only_on_auto_layers() {
    expect(
        &format!("{CREV}/level.json"),
        "layers.decor_back: \"decor_back\" is a tiles layer; only auto layers have per-level rule state",
        |p| {
            crevasse(p)
                .layers
                .insert(slug("decor_back"), LayerState::default());
        },
    );
    expect(
        &format!("{CREV}/level.json"),
        "layers.ghost: unknown layer \"ghost\"",
        |p| {
            crevasse(p)
                .layers
                .insert(slug("ghost"), LayerState::default());
        },
    );
}

#[test]
fn layer_and_annotation_shape() {
    expect(
        &format!("{CREV}/notes.json"),
        "guides are world scope only",
        |p| {
            crevasse(p).notes.push(Annotation::new(Shape::Guide {
                points: vec![[0, 0], [1, 1]],
                closed: false,
                group: None,
            }));
        },
    );
    // Notes are named by id.
    expect(
        &format!("{CREV}/notes.json"),
        "notes[pathcrev22]: a path needs at least 2 points",
        |p| {
            crevasse(p).notes[1].shape = Shape::Path {
                points: vec![[0, 0]],
            };
        },
    );
    expect(
        &format!("{CREV}/notes.json"),
        "notes[notecrev22].links[3]: \"level:nowhere\" does not resolve",
        |p| {
            crevasse(p).notes[0].links.push("level:nowhere".into());
        },
    );
    expect(
        "worlds/main/world.json",
        "a region takes rect or points, not both",
        |p| {
            p.worlds[0].notes[2].shape = Shape::Region {
                points: vec![[0, 0], [1, 0], [1, 1]],
                rect: Some([0, 0, 1, 1]),
            };
        },
    );
    expect(
        &format!("{CREV}/entities.json"),
        "limit is 4 per Level",
        |p| {
            let d = door(p).clone();
            for _ in 0..4 {
                let mut d = d.clone();
                d.id = Id::generate();
                crevasse(p).entities[0].entities.push(d);
            }
        },
    );
}

fn button(p: &mut Project) -> &mut EntityInstance {
    crevasse(p).entities[0]
        .entities
        .iter_mut()
        .find(|e| e.ty == "floor_button")
        .unwrap()
}

/// Problems whose text contains `needle`.
fn problems_with(p: &Project, needle: &str) -> Vec<String> {
    validate(p)
        .iter()
        .map(|pr| pr.to_string())
        .filter(|s| s.contains(needle))
        .collect()
}

#[test]
fn inherited_defaults_are_checked_where_they_apply() {
    let f = format!("{CREV}/entities.json");
    // A default can't be checked against a level on the definition, but the
    // button that inherits it can.
    expect(
        &f,
        "fields.target: inherited default: no entity with id \"zzzzzzzzzz\"",
        |p| {
            p.entities[0].fields[2].default = Some("zzzzzzzzzz".into());
            button(p).fields.remove("target");
        },
    );
    expect(
        &f,
        "fields.target: inherited default: entity \"doorshaft2\" is in level \"shaft\" and \
         cross_level is off",
        |p| {
            p.entities[0].fields[2].default = Some("doorshaft2".into());
            button(p).fields.remove("target");
        },
    );
    expect(
        &f,
        "fields.anchor: inherited default: point [99, 1] is outside the level",
        |p| {
            p.entities[1].fields[8].default = Some(serde_json::json!([99, 1]));
        },
    );
    // The crevasse door sets its waypoints; the shaft door inherits them.
    expect(
        "worlds/main/levels/shaft/entities.json",
        "fields.waypoints[1]: inherited default: point [99, 0] is outside",
        |p| {
            p.entities[1].fields[9].default = Some(serde_json::json!([[0, 0], [99, 0]]));
        },
    );
    // Level fields too.
    expect(
        &format!("{CREV}/level.json"),
        "fields.spawn: inherited default: point [500, 0] is outside the level",
        |p| {
            let mut d = FieldDef::new(slug("spawn"), FieldType::Point);
            d.default = Some(serde_json::json!([500, 0]));
            p.level_fields.push(d);
        },
    );
    // An instance that sets the field doesn't inherit; a shape error is
    // reported once, on the definition.
    let mut p = common::fixture();
    p.entities[1].fields[8].default = Some("nowhere".into());
    let found = problems_with(&p, "nowhere");
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(
        found[0].starts_with("entities/locked_door.json"),
        "{found:?}"
    );
}

#[test]
fn nullable_arrays_may_hold_null_elements() {
    let mut p = common::fixture();
    // exit_to is nullable; as an array, its elements may be null.
    p.entities[1].fields[10].array = true;
    door(&mut p)
        .fields
        .insert(slug("exit_to"), serde_json::json!(["doorshaft2", null]));
    assert_eq!(problems_with(&p, "exit_to"), Vec::<String>::new());
    // waypoints is not nullable.
    expect(
        &format!("{CREV}/entities.json"),
        "fields.waypoints[0]: null is not allowed",
        |p| {
            door(p)
                .fields
                .insert(slug("waypoints"), serde_json::json!([null]));
        },
    );
}

#[test]
fn layer_slugs_that_collide_with_files() {
    // An auto layer's rules are rules/<slug>.json, next to rules/macros.json.
    expect(
        "layers.json",
        "layers[1].slug: \"macros\" is reserved",
        |p| {
            p.layers[1].slug = slug("macros");
            p.rules[0].layer = slug("macros");
        },
    );
    expect(
        "layers.json",
        "layers[0].slug: \"notes\" is reserved",
        |p| {
            p.layers[0].slug = slug("notes");
        },
    );
    // Auto and entities layers own no level file.
    let mut p = common::fixture();
    p.layers[1].slug = slug("notes");
    p.layers[3].slug = slug("level");
    assert_eq!(problems_with(&p, "is reserved"), Vec::<String>::new());
}

#[test]
fn collision_layer_must_be_an_int_grid_layer() {
    expect(
        "project.json",
        "collision_layer: \"main_tiling\" is not an int_grid layer",
        |p| p.collision_layer = Some(slug("main_tiling")),
    );
    expect(
        "project.json",
        "collision_layer: unknown layer \"walls\"",
        |p| p.collision_layer = Some(slug("walls")),
    );
}

#[test]
fn blocking_values_are_collision_values() {
    expect(
        "project.json",
        "blocking_values[1]: \"lava\" is not a value of collision layer \"collision\"",
        |p| p.blocking_values = Some(vec![slug("solid"), slug("lava")]),
    );
    expect(
        "project.json",
        "blocking_values[1]: duplicate blocking value \"solid\"",
        |p| p.blocking_values = Some(vec![slug("solid"), slug("solid")]),
    );
    expect(
        "project.json",
        "blocking_values[0]: set without a collision_layer",
        |p| {
            p.collision_layer = None;
            p.blocking_values = Some(vec![slug("solid")]);
        },
    );
    let mut p = common::fixture();
    p.blocking_values = Some(vec![slug("solid"), slug("fortified")]);
    assert_eq!(problems_with(&p, "blocking"), Vec::<String>::new());
    // Absent, with default kinds to fall back on: fine. Explicitly empty: fine.
    p.blocking_values = None;
    assert_eq!(problems_with(&p, "blocking"), Vec::<String>::new());
    p.blocking_values = Some(Vec::new());
    assert_eq!(problems_with(&p, "blocking"), Vec::<String>::new());
}

#[test]
fn a_collision_layer_without_blocking_values_is_reported() {
    // A collision layer with none of the default blocking kinds and no
    // list: every border cell would be open.
    let mut p = common::fixture();
    p.blocking_values = None;
    for l in &mut p.layers {
        if l.slug == "collision"
            && let LayerKind::IntGrid(g) = &mut l.kind
        {
            for v in g.values.iter_mut() {
                if worlds_core::DEFAULT_BLOCKING_VALUES.contains(&v.slug.as_str()) {
                    v.slug = slug(&format!("{}_x", v.slug));
                }
            }
        }
    }
    assert_eq!(
        problems_with(&p, "blocking"),
        [
            "project.json: blocking_values: missing, and collision layer \"collision\" has none of the default blocking kinds, so every border cell is open in the export; list the values that block, or [] if none do"
        ]
    );
}

fn door_def(p: &mut Project) -> &mut EntityDef {
    p.entities
        .iter_mut()
        .find(|e| e.slug == "locked_door")
        .unwrap()
}

fn set_regex(p: &mut Project, field: &str, r: &str) {
    let f = door_def(p)
        .fields
        .iter_mut()
        .find(|f| f.slug == field)
        .unwrap();
    match &mut f.ty {
        FieldType::String { regex } | FieldType::Text { regex } => *regex = Some(r.into()),
        other => panic!("{other:?}"),
    }
}

fn problems(p: &Project) -> Vec<String> {
    validate(p).iter().map(|x| x.to_string()).collect()
}

#[test]
fn regex_must_match_the_whole_value() {
    let lvl = "worlds/main/levels/crevasse/entities.json";
    // doorcrev22 stores needs_item "brass_key"; the default is "".
    let base = |p: &mut Project| {
        set_regex(p, "needs_item", "([a-z_]*key)?");
    };
    let mut p = common::fixture();
    base(&mut p);
    assert!(problems(&p).is_empty(), "{:?}", problems(&p));
    // A pattern that only matches part of the value is a problem.
    expect(lvl, "doesn't match regex \"brass\"", |p| {
        set_regex(p, "needs_item", "brass")
    });
    expect(
        lvl,
        "layers[objects].entities[doorcrev22].fields.needs_item",
        |p| set_regex(p, "needs_item", "key"),
    );
    // Alternation is grouped: `a|b` can't match just a prefix.
    expect(lvl, "doesn't match", |p| {
        set_regex(p, "needs_item", "brass|iron")
    });
    let mut p = common::fixture();
    set_regex(&mut p, "needs_item", "brass_key|iron_key|");
    assert!(problems(&p).is_empty(), "{:?}", problems(&p));
    // `(?i)` ignores case.
    let mut p = common::fixture();
    set_regex(&mut p, "needs_item", "(?i)BRASS_KEY|");
    assert!(problems(&p).is_empty(), "{:?}", problems(&p));
    expect(lvl, "doesn't match", |p| {
        set_regex(p, "needs_item", "BRASS_KEY|")
    });
    // Defaults are checked too ("" doesn't match `.+`).
    expect(
        "entities/locked_door.json",
        "fields[1].default: \"\" doesn't match regex \".+\"",
        |p| set_regex(p, "needs_item", ".+"),
    );
    // A pattern that doesn't compile is reported once, on the definition,
    // and values aren't checked against it.
    let mut p = common::fixture();
    set_regex(&mut p, "needs_item", "(brass");
    let found = problems(&p);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(
        found[0].starts_with(
            "entities/locked_door.json: fields[1].regex: regex \"(brass\" doesn't compile"
        ),
        "{found:?}"
    );
}

#[test]
fn regex_syntax_is_checked_on_its_own_not_inside_the_anchoring_wrapper() {
    // Unbalanced patterns used to splice into `^(?:…)$`: `a)|(b` became
    // `^(?:a)|(b)$`, compiled, and accepted any value starting with "a".
    // Now they are reported as not compiling and values aren't checked.
    for bad in ["a)|(b", "a)|(?:b", "brass)|(?:x"] {
        let mut p = common::fixture();
        set_regex(&mut p, "needs_item", bad);
        let found = problems(&p);
        assert_eq!(found.len(), 1, "{bad}: {found:?}");
        assert!(
            found[0].starts_with(&format!(
                "entities/locked_door.json: fields[1].regex: regex {bad:?} doesn't compile"
            )),
            "{bad}: {found:?}"
        );
    }
    // A valid extended-mode pattern ending in a comment used to swallow the
    // wrapper's closing parenthesis and be rejected.
    let mut p = common::fixture();
    set_regex(
        &mut p,
        "needs_item",
        "(?x) ( brass_key | ) # empty or the key",
    );
    assert!(problems(&p).is_empty(), "{:?}", problems(&p));
    // ...and it still anchors: "brass_key" alone doesn't match `brass`.
    expect(
        "worlds/main/levels/crevasse/entities.json",
        "doesn't match regex",
        |p| set_regex(p, "needs_item", "(?x) brass # partial"),
    );
}

#[test]
fn regex_checks_array_elements_and_level_fields() {
    // An array of strings: each element.
    let mut p = common::fixture();
    let f = door_def(&mut p)
        .fields
        .iter_mut()
        .find(|f| f.slug == "label")
        .unwrap();
    f.ty = FieldType::String {
        regex: Some("[a-z]+".into()),
    };
    f.array = true;
    f.nullable = false;
    let d = door(&mut p);
    d.fields
        .insert(slug("label"), serde_json::json!(["ok", "Not ok", "fine"]));
    let found = problems(&p);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(
        found[0].contains("fields.label[1]: \"Not ok\" doesn't match"),
        "{found:?}"
    );
    // A level field.
    expect(
        "worlds/main/levels/crevasse/level.json",
        "doesn't match regex",
        |p| {
            p.level_fields[1].ty = FieldType::String {
                regex: Some("[a-z_]+".into()),
            };
            crevasse(p)
                .fields
                .insert(slug("music"), serde_json::json!("Cave Theme"));
        },
    );
    expect("project.json", "level_fields[1].regex", |p| {
        p.level_fields[1].ty = FieldType::String {
            regex: Some("[".into()),
        };
    });
}

#[test]
fn category_paths_and_tags() {
    let mut p = common::fixture();
    door_def(&mut p).category = Some("Puzzle/Doors/Big ones".into());
    assert!(problems(&p).is_empty(), "{:?}", problems(&p));
    for (bad, why) in [
        ("/Puzzle", "starts or ends with"),
        ("Puzzle/", "starts or ends with"),
        ("Puzzle//Doors", "empty segment"),
        ("", "empty segment"),
        ("Puzzle/ Doors", "whitespace"),
        ("Puzzle /Doors", "whitespace"),
    ] {
        expect("entities/locked_door.json", why, |p| {
            door_def(p).category = Some(bad.into())
        });
        expect("entities/locked_door.json", "category:", |p| {
            door_def(p).category = Some(bad.into())
        });
    }
    expect(
        "entities/locked_door.json",
        "tags[2]: duplicate tag \"object\"",
        |p| door_def(p).tags.push("object".into()),
    );
}

// ---- annotations, guide groups and level tags ----

const WORLD: &str = "worlds/main/world.json";

fn world_note<'a>(p: &'a mut Project, id: &str) -> &'a mut Annotation {
    p.worlds[0]
        .notes
        .iter_mut()
        .find(|n| n.id.as_str() == id)
        .unwrap()
}

fn set_group(p: &mut Project, g: Option<&str>) {
    if let Shape::Guide { group, .. } = &mut world_note(p, "guidestem2").shape {
        *group = g.map(slug);
    }
}

#[test]
fn v1_guide_group_must_be_defined() {
    expect(
        WORLD,
        "notes[guidestem2].group: guide group \"ruined\" is not defined in world.json guide_groups",
        |p| set_group(p, Some("ruined")),
    );
    let mut p = common::fixture();
    set_group(&mut p, Some("ruined"));
    p.worlds[0]
        .guide_groups
        .push(GuideGroup::new(slug("ruined")));
    assert!(problems(&p).is_empty(), "{:?}", problems(&p));
}

#[test]
fn v2_guide_group_slugs_are_unique() {
    expect(
        WORLD,
        "guide_groups[1].slug: duplicate guide group \"ruined\"",
        |p| {
            p.worlds[0].guide_groups = vec![
                GuideGroup::new(slug("ruined")),
                GuideGroup::new(slug("ruined")),
            ]
        },
    );
}

#[test]
fn v3_guides_never_export() {
    expect(
        WORLD,
        "notes[guidestem2].export: guides are never exported (export is ignored)",
        |p| world_note(p, "guidestem2").export = true,
    );
}

#[test]
fn v4_level_notes_and_pins_sit_inside_their_level() {
    let msg = "notes[notecrev22].px: outside its level (level is 320x80 px)";
    for px in [[320, 48], [-1, 0], [0, 80]] {
        expect(&format!("{CREV}/notes.json"), msg, |p| {
            crevasse(p).notes[0].shape = Shape::Note { px }
        });
    }
    expect(
        &format!("{CREV}/notes.json"),
        "notes[notecrev22].px: outside its level",
        |p| {
            crevasse(p).notes[0].shape = Shape::Pin {
                px: [400, 0],
                icon: None,
                label: None,
            }
        },
    );
    // The last pixel is inside; paths may leave the level.
    let mut p = common::fixture();
    crevasse(&mut p).notes[0].shape = Shape::Note { px: [319, 79] };
    crevasse(&mut p).notes[1].shape = Shape::Path {
        points: vec![[-50, 0], [900, 900]],
    };
    assert!(problems(&p).is_empty(), "{:?}", problems(&p));
}

#[test]
fn v5_region_rects_need_a_size() {
    for rect in [[0, 0, 0, 480], [0, 0, 640, -1]] {
        expect(
            WORLD,
            "notes[regioneast].rect: needs a positive width and height",
            |p| {
                world_note(p, "regioneast").shape = Shape::Region {
                    points: vec![],
                    rect: Some(rect),
                }
            },
        );
    }
}

fn tag_region(p: &mut Project, shape: Shape) {
    let r = world_note(p, "regioneast");
    r.tags = vec!["zone".into(), "forest".into()];
    r.shape = shape;
}

#[test]
fn v6_a_tagged_level_sits_in_a_region_with_its_tag() {
    let rect = |r: [i64; 4]| Shape::Region {
        points: vec![],
        rect: Some(r),
    };
    // Crevasse is [0, 0, 320, 80]: its center is [160, 40].
    let mut p = common::fixture();
    crevasse(&mut p).tags = vec!["forest".into(), "ruined".into()];
    tag_region(&mut p, rect([0, 0, 640, 480]));
    assert!(problems(&p).is_empty(), "{:?}", problems(&p));
    // Half-open: a region ending at the center misses it.
    tag_region(&mut p, rect([0, 0, 160, 480]));
    assert_eq!(
        problems(&p),
        [format!(
            "{CREV}/level.json: tags[0]: level tagged \"forest\" sits outside every region \
             tagged \"forest\" (its center is [160, 40])"
        )]
    );
    // One region with the tag holding the center is enough.
    let mut other = Annotation::new(rect([100, 0, 100, 100]));
    other.tags = vec!["forest".into()];
    p.worlds[0].notes.push(other);
    assert!(problems(&p).is_empty(), "{:?}", problems(&p));
    p.worlds[0].notes.pop();
    // Polygons: even-odd, a point on an edge is inside.
    let poly = |pts: &[[i64; 2]]| Shape::Region {
        points: pts.to_vec(),
        rect: None,
    };
    tag_region(&mut p, poly(&[[0, 0], [320, 0], [160, 80]]));
    assert!(problems(&p).is_empty(), "{:?}", problems(&p));
    tag_region(&mut p, poly(&[[160, 0], [400, 0], [400, 80], [160, 80]]));
    assert!(problems(&p).is_empty(), "{:?}", problems(&p));
    tag_region(&mut p, poly(&[[161, 0], [400, 0], [400, 80], [161, 80]]));
    assert_eq!(problems(&p).len(), 1, "{:?}", problems(&p));
    // A concave polygon whose notch holds the center.
    tag_region(
        &mut p,
        poly(&[
            [0, 0],
            [320, 0],
            [320, 80],
            [200, 80],
            [200, 20],
            [100, 20],
            [100, 80],
            [0, 80],
        ]),
    );
    assert_eq!(problems(&p).len(), 1, "{:?}", problems(&p));
    // No region tagged ruined: no check for that tag. Guide groups never
    // apply to regions, and other kinds tagged forest don't count.
    world_note(&mut p, "guidestem2").tags = vec!["forest".into()];
    assert!(problems(&p).iter().all(|s| !s.contains("ruined")));
}

#[test]
fn v7_exported_notes_link_exported_notes() {
    let msg =
        "notes[pinstatue2].links[1]: links a note that is not exported: the game can't resolve it";
    expect(WORLD, msg, |p| {
        world_note(p, "pinstatue2")
            .links
            .push("note:regioneast".into())
    });
    // Guides never export, even flagged.
    expect(WORLD, msg, |p| {
        world_note(p, "guidestem2").export = true;
        world_note(p, "pinstatue2")
            .links
            .push("note:guidestem2".into());
    });
    // An exported target, or a link from a note that isn't exported, is fine.
    let mut p = common::fixture();
    world_note(&mut p, "regioneast").export = true;
    world_note(&mut p, "pinstatue2")
        .links
        .push("note:regioneast".into());
    assert!(problems(&p).is_empty(), "{:?}", problems(&p));
    // A guide flagged export is reported once (V3), not for its links.
    let mut p = common::fixture();
    world_note(&mut p, "guidestem2").export = true;
    world_note(&mut p, "guidestem2").links = vec!["note:regioneast".into()];
    assert_eq!(problems(&p).len(), 1, "{:?}", problems(&p));
    let p = common::fixture();
    assert!(
        p.level("crevasse").unwrap().notes[0]
            .links
            .contains(&"note:pinstatue2".to_string())
    );
}

#[test]
fn v8_tags_are_non_empty_and_unique() {
    let level = format!("{CREV}/level.json");
    expect(&level, "tags[0]: empty tag", |p| {
        crevasse(p).tags = vec!["".into()]
    });
    expect(&level, "tags[2]: duplicate tag \"a\"", |p| {
        crevasse(p).tags = vec!["a".into(), "b".into(), "a".into()]
    });
    expect(
        &format!("{CREV}/notes.json"),
        "notes[notecrev22].tags[1]: duplicate tag \"todo\"",
        |p| crevasse(p).notes[0].tags.push("todo".into()),
    );
    expect(WORLD, "notes[pinstatue2].tags[1]: empty tag", |p| {
        world_note(p, "pinstatue2").tags.push("".into())
    });
}
