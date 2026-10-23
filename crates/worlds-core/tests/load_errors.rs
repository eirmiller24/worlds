mod common;

use std::fs;
use std::path::Path;

use worlds_core::*;

/// Copy the fixture to a temp dir via save, apply `edit` to the files, load.
fn load_edited(edit: impl FnOnce(&Path)) -> Result<Project, Error> {
    let tmp = tempfile::tempdir().unwrap();
    save(&mut common::fixture(), tmp.path()).unwrap();
    edit(tmp.path());
    load(tmp.path())
}

fn replace(path: &Path, from: &str, to: &str) {
    let s = fs::read_to_string(path).unwrap();
    assert!(s.contains(from), "{from:?} not in {}", path.display());
    fs::write(path, s.replacen(from, to, 1)).unwrap();
}

fn err_text(r: Result<Project, Error>) -> String {
    r.expect_err("load should fail").to_string()
}

#[test]
fn unknown_key_names_file_and_json_path() {
    let e = err_text(load_edited(|d| {
        replace(
            &d.join("layers.json"),
            "\"glyph\": \"o\"",
            "\"glyph\": \"o\", \"glpyh\": 1",
        );
    }));
    assert!(
        e.contains("layers.json: at layers[0].values[2].glpyh: unknown field `glpyh`"),
        "{e}"
    );
}

#[test]
fn bad_value_names_json_path() {
    let e = err_text(load_edited(|d| {
        replace(
            &d.join("worlds/main/levels/crevasse/entities.json"),
            "\"cell\": [12, 3]",
            "\"cell\": [12]",
        );
    }));
    assert!(
        e.contains("crevasse/entities.json: at layers[0].entities[1].cell:"),
        "{e}"
    );

    let e = err_text(load_edited(|d| {
        replace(
            &d.join("rules/main_tiling.json"),
            "\"!solid\"",
            "\"!Solid\"",
        );
    }));
    assert!(
        e.contains("rules/main_tiling.json: at groups[0].rules[0].match.legend.."),
        "{e}"
    );
    assert!(e.contains("invalid cell test"), "{e}");
}

#[test]
fn header_is_required() {
    let e = err_text(load_edited(|d| {
        replace(&d.join("enums.json"), "\"format\": 1", "\"format\": 2")
    }));
    assert!(
        e.contains("enums.json: at format: unsupported format 2"),
        "{e}"
    );
    let e = err_text(load_edited(|d| {
        replace(
            &d.join("enums.json"),
            "\"$schema\": \"schema/enums.json\",",
            "",
        )
    }));
    assert!(e.contains("missing \"$schema\""), "{e}");
}

#[test]
fn syntax_error_names_file() {
    let e = err_text(load_edited(|d| {
        replace(&d.join("tilesets.json"), "{", "{{")
    }));
    assert!(
        e.contains("tilesets.json: key must be a string at line 1"),
        "{e}"
    );
}

#[test]
fn names_must_match_files() {
    let e = err_text(load_edited(|d| {
        fs::rename(
            d.join("entities/floor_button.json"),
            d.join("entities/button.json"),
        )
        .unwrap();
    }));
    assert!(e.contains("entities/button.json: slug is \"floor_button\" but the file or directory name says \"button\""), "{e}");

    let e = err_text(load_edited(|d| {
        fs::rename(
            d.join("worlds/main/levels/shaft"),
            d.join("worlds/main/levels/pit"),
        )
        .unwrap();
    }));
    assert!(e.contains("pit/level.json: slug is \"shaft\""), "{e}");
}

#[test]
fn unknown_level_file_is_an_error() {
    let e = err_text(load_edited(|d| {
        fs::copy(
            d.join("worlds/main/levels/crevasse/collision.json"),
            d.join("worlds/main/levels/crevasse/colision.json"),
        )
        .unwrap();
    }));
    assert!(
        e.contains("colision.json: no layer named \"colision\""),
        "{e}"
    );
}

#[test]
fn missing_layer_files_mean_empty() {
    let p = load_edited(|d| {
        let l = d.join("worlds/main/levels/crevasse");
        for f in [
            "collision.json",
            "decor_back.json",
            "entities.json",
            "notes.json",
        ] {
            fs::remove_file(l.join(f)).unwrap();
        }
        fs::remove_file(d.join("enums.json")).unwrap();
    })
    .unwrap();
    let l = p.level("crevasse").unwrap();
    assert!(
        l.int_grid("collision").is_none()
            && l.tiles.is_empty()
            && l.entities.is_empty()
            && l.notes.is_empty()
    );
    assert!(p.enums.is_empty());
}

#[test]
fn kind_specific_keys_are_load_errors() {
    // A key of another kind, with the path through the kind-tagged layer.
    let e = err_text(load_edited(|d| {
        replace(
            &d.join("layers.json"),
            "\"kind\": \"entities\"",
            "\"kind\": \"entities\", \"source\": \"collision\"",
        );
    }));
    assert!(
        e.contains("layers.json: at layers[3]: \"source\" is not allowed on entities layers"),
        "{e}"
    );
    // Inside a variant, the path still reaches the field.
    let e = err_text(load_edited(|d| {
        replace(&d.join("layers.json"), "\"glyph\": \"o\"", "\"glyph\": 7");
    }));
    assert!(
        e.contains("at layers[0].values[2].glyph: invalid type"),
        "{e}"
    );
    let e = err_text(load_edited(|d| {
        replace(&d.join("layers.json"), "\"source\": \"collision\",", "");
    }));
    assert!(e.contains("auto layers need a \"source\""), "{e}");
    // Fields and annotations work the same way.
    let e = err_text(load_edited(|d| {
        replace(
            &d.join("entities/locked_door.json"),
            "\"type\": \"bool\", \"default\": false",
            "\"type\": \"bool\", \"default\": false, \"min\": 0",
        );
    }));
    assert!(
        e.contains("locked_door.json: at fields[2]: \"min\" is not allowed on bool fields"),
        "{e}"
    );
    let e = err_text(load_edited(|d| {
        replace(
            &d.join("worlds/main/levels/crevasse/notes.json"),
            "\"status\": \"done\",",
            "\"status\": \"done\", \"px\": [1, 2],",
        );
    }));
    assert!(
        e.contains("notes.json: at notes[1]: \"px\" is not allowed on path annotations"),
        "{e}"
    );
    let e = err_text(load_edited(|d| {
        replace(
            &d.join("worlds/main/levels/crevasse/notes.json"),
            "\"px\": [160, 48]",
            "\"at\": [160, 48]",
        );
    }));
    assert!(e.contains("unknown field `at`"), "{e}");
}

#[test]
fn reserved_glyphs_are_load_errors() {
    for bad in ["\\\\", "*", "@", "|", "[", "."] {
        let e = err_text(load_edited(|d| {
            replace(
                &d.join("layers.json"),
                "\"glyph\": \"o\"",
                &format!("\"glyph\": \"{bad}\""),
            );
        }));
        assert!(e.contains("at layers[0].values[2].glyph:"), "{bad}: {e}");
        assert!(e.contains("reserved"), "{bad}: {e}");
    }
}

#[test]
fn rule_outputs_say_what_they_accept() {
    let e = err_text(load_edited(|d| {
        replace(
            &d.join("rules/main_tiling.json"),
            "[17, 0]",
            "{ \"tile\": [17, 0] }",
        );
    }));
    assert!(
        e.contains("rules/main_tiling.json: at groups[0].rules[0].out.tiles[2]: a tile alternative is [col, row]"),
        "{e}"
    );
    assert!(e.contains("got {\"tile\":[17,0]}"), "{e}");
    let e = err_text(load_edited(|d| {
        replace(
            &d.join("rules/main_tiling.json"),
            "\"stamp\": \"pillar\"",
            "\"stamp\": \"pillar\", \"tileset\": \"ssw_set5\"",
        );
    }));
    assert!(
        e.contains("an output is either \"tileset\" + \"tiles\""),
        "{e}"
    );
    let e = err_text(load_edited(|d| {
        replace(
            &d.join("rules/main_tiling.json"),
            "\"slug\": \"drips\"",
            "\"id\": \"drips\"",
        );
    }));
    assert!(e.contains("unknown field `id`"), "{e}");
}

#[test]
fn intgrid_rows_are_checked_on_load() {
    let f = "worlds/main/levels/crevasse/collision.json";
    let e = err_text(load_edited(|d| {
        replace(&d.join(f), "#.......oooo", "#.......oZoo")
    }));
    assert!(
        e.contains("collision.json: at rows[2]: glyph 'Z' is not a value of layer \"collision\""),
        "{e}"
    );
    let e = err_text(load_edited(|d| {
        replace(&d.join(f), "#.......oooo", "#.......ooo")
    }));
    assert!(
        e.contains("at rows[2]: row has 19 glyphs, the first row has 20"),
        "{e}"
    );
    // Legend entries must agree with layers.json.
    let e = err_text(load_edited(|d| {
        replace(&d.join(f), "\"#\": \"solid\"", "\"S\": \"solid\"")
    }));
    assert!(
        e.contains("at legend.S: layer \"collision\" uses glyph '#' for \"solid\""),
        "{e}"
    );
    let e = err_text(load_edited(|d| {
        replace(&d.join(f), "\"o\": \"oneway\"", "\"o\": null")
    }));
    assert!(
        e.contains("means \"oneway\" in the layer definition, not empty"),
        "{e}"
    );
    let e = err_text(load_edited(|d| {
        replace(
            &d.join(f),
            "\"o\": \"oneway\"",
            "\"o\": \"oneway\", \"x\": \"lava\"",
        )
    }));
    assert!(
        e.contains("\"lava\" is not a value of layer \"collision\""),
        "{e}"
    );
}

#[test]
fn reserved_layer_slug_can_not_be_saved() {
    let mut p = common::fixture();
    let crev = p.worlds[0]
        .levels
        .iter_mut()
        .find(|l| l.slug == "crevasse")
        .unwrap();
    crev.int_grids[0].layer = slug("level");
    let tmp = tempfile::tempdir().unwrap();
    let e = save(&mut p, tmp.path()).unwrap_err().to_string();
    assert!(e.contains("layer slug \"level\" is reserved"), "{e}");
}

#[test]
fn kind_specific_keys_at_their_default_are_still_load_errors() {
    // Saving would silently drop these, so loading must refuse them.
    for (kind, extra, key) in [
        ("tiles", "\"values\": []", "values"),
        ("tiles", "\"derived\": false", "derived"),
        ("int_grid", "\"tile_pivot\": [0, 0]", "tile_pivot"),
        ("entities", "\"groups\": []", "groups"),
    ] {
        let e = err_text(load_edited(|d| {
            replace(
                &d.join("layers.json"),
                &format!("\"kind\": \"{kind}\""),
                &format!("\"kind\": \"{kind}\", {extra}"),
            );
        }));
        assert!(
            e.contains(&format!("\"{key}\" is not allowed on {kind} layers")),
            "{extra}: {e}"
        );
    }
    for (extra, key) in [
        ("\"cross_level\": false", "cross_level"),
        ("\"allowed_types\": []", "allowed_types"),
        ("\"extensions\": []", "extensions"),
    ] {
        let e = err_text(load_edited(|d| {
            replace(
                &d.join("entities/locked_door.json"),
                "\"type\": \"bool\", \"default\": false",
                &format!("\"type\": \"bool\", \"default\": false, {extra}"),
            );
        }));
        assert!(
            e.contains(&format!("\"{key}\" is not allowed on bool fields")),
            "{extra}: {e}"
        );
    }
    // On the right kind, a key at its default is fine (and saved without it).
    let p = load_edited(|d| {
        replace(
            &d.join("layers.json"),
            "\"kind\": \"int_grid\"",
            "\"kind\": \"int_grid\", \"derived\": false",
        );
    })
    .unwrap();
    assert!(!p.layers[0].int_grid().unwrap().derived);
}
