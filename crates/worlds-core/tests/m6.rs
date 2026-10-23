//! M6 format additions: passes, region rules, `write` outputs, stamps with
//! occupancy, derived grids with their overrides, `requires_level_tag` and
//! `require` as a list. Loading, saving, validation, commands, export and
//! the text view; the engine is `worlds-rules`'.

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use worlds_core::command::{Applied, CellValue, Command, CommandError, GridValue, History};
use worlds_core::export::BakedLevel;
use worlds_core::format::{self, IntGridFile};
use worlds_core::*;

fn s(x: &str) -> Slug {
    Slug::new(x).unwrap()
}

fn cmd(v: Value) -> Command {
    serde_json::from_value(v.clone()).unwrap_or_else(|e| panic!("{e}: {v}"))
}

fn apply(p: &mut Project, v: Value) -> Applied {
    p.apply(cmd(v.clone()))
        .unwrap_or_else(|e| panic!("{e}: {v}"))
}

fn refused(p: &mut Project, v: Value) -> CommandError {
    let before = p.clone();
    let e = p.apply(cmd(v)).unwrap_err();
    assert_eq!(*p, before, "a refused command changes nothing");
    e
}

/// The marks layer: derived, values `mark` (`p`, in group `spots`) and `big`.
fn marks_def() -> Value {
    json!({ "slug": "marks", "name": "Marks", "kind": "int_grid", "derived": true,
        "values": [
            { "slug": "mark", "name": "Mark", "color": "#e0a040", "glyph": "p", "group": "spots" },
            { "slug": "big", "name": "Big", "color": "#e04040", "glyph": "B" }
        ],
        "groups": [{ "slug": "spots", "name": "Spots", "glyph": "S" }] })
}

/// A region rule writing `marks`.
fn region_rule() -> Value {
    json!({ "slug": "big_room",
        "region": { "cells": ".", "min_width": 15 },
        "place": { "along": "floor", "every": 6, "margin": 3 },
        "write": { "grid": "marks", "value": "mark" } })
}

/// A pattern rule reading `marks`, with two `require` patterns.
fn stamp_rule() -> Value {
    json!({ "slug": "stamp_it",
        "match": { "grid": "marks", "rows": ["p"] },
        "require": [
            { "legend": { ".": "[.|oneway]" }, "rows": ["..", "@."] },
            { "grid": "marks", "rows": ["*"] }
        ],
        "out": { "stamp": "column", "avoid_overlap": true } })
}

/// The mini fixture with `marks` (derived) and `pillars` (auto, on
/// collision) whose two groups write and read it.
fn project() -> Project {
    let mut p = common::fixture();
    apply(
        &mut p,
        json!({ "op": "set_layer_def", "layer": "marks", "index": 1, "def": marks_def() }),
    );
    apply(
        &mut p,
        json!({ "op": "set_layer_def", "layer": "pillars", "index": 2,
            "def": { "slug": "pillars", "name": "Pillars", "kind": "auto", "source": "collision" } }),
    );
    apply(
        &mut p,
        json!({ "op": "set_stamp", "layer": "pillars", "stamp": "column",
            "def": { "slug": "column", "tileset": "castle_main", "rect": [0, 0, 1, 2],
                     "pivot": [0, 1.0], "occupies": true } }),
    );
    apply(
        &mut p,
        json!({ "op": "set_rule_group", "layer": "pillars", "group": "marks",
            "def": { "slug": "marks", "rules": [region_rule()] } }),
    );
    apply(
        &mut p,
        json!({ "op": "set_rule_group", "layer": "pillars", "group": "art",
            "def": { "slug": "art", "pass": 1,
                     "requires_level_tag": { "tags": ["grand"] }, "rules": [stamp_rule()] } }),
    );
    p
}

fn problems(p: &Project) -> Vec<String> {
    validate(p).iter().map(|x| x.to_string()).collect()
}

/// Problems `edit` adds to the M6 project, which itself validates clean.
fn new_problems(edit: impl FnOnce(&mut Project)) -> Vec<String> {
    let mut p = project();
    assert_eq!(problems(&p), Vec::<String>::new());
    edit(&mut p);
    problems(&p)
}

fn set_mut(p: &mut Project) -> &mut RuleSet {
    p.rules.iter_mut().find(|r| r.layer == "pillars").unwrap()
}

fn rule_mut(p: &mut Project, group: usize) -> &mut Rule {
    set_mut(p).groups[group].rules[0].as_rule_mut().unwrap()
}

fn parse_rules(text: &str) -> Result<RuleSet, String> {
    format::parse::<RuleSet>(Path::new("rules/x.json"), text).map_err(|e| e.to_string())
}

/// A rules file with one rule.
fn one_rule(rule: Value) -> Result<RuleSet, String> {
    parse_rules(
        &json!({ "$schema": "../schema/rules.json", "format": 1, "layer": "x",
            "groups": [{ "slug": "g", "rules": [rule] }] })
        .to_string(),
    )
}

fn rule_err(rule: Value) -> String {
    one_rule(rule.clone()).expect_err(&format!("should not load: {rule}"))
}

fn rendered(p: &Project, rel: &str) -> String {
    String::from_utf8(format::render_file(p, Path::new(rel)).unwrap().unwrap()).unwrap()
}

fn tree(dir: &Path) -> BTreeMap<String, String> {
    common::read_tree(dir)
}

// ---- rules files: load and save ----

#[test]
fn m6_rules_file_saves_in_table_order_and_round_trips() {
    let p = project();
    let text = rendered(&p, "rules/pillars.json");
    let expected = r#"{
  "$schema": "../schema/rules.json",
  "format": 1,
  "layer": "pillars",
  "stamps": [
    {
      "slug": "column",
      "tileset": "castle_main",
      "rect": [0, 0, 1, 2],
      "pivot": [0.0, 1.0],
      "occupies": true
    }
  ],
  "groups": [
    {
      "slug": "marks",
      "rules": [
        {
          "slug": "big_room",
          "region": { "cells": ".", "min_width": 15 },
          "place": { "along": "floor", "every": 6, "margin": 3 },
          "write": { "grid": "marks", "value": "mark" }
        }
      ]
    },
    {
      "slug": "art",
      "pass": 1,
      "requires_level_tag": { "tags": ["grand"] },
      "rules": [
        {
          "slug": "stamp_it",
          "match": {
            "grid": "marks",
            "rows": [
              "p"
            ]
          },
          "require": [
            {
              "legend": { ".": "[.|oneway]" },
              "rows": [
                "..",
                "@."
              ]
            },
            {
              "grid": "marks",
              "rows": [
                "*"
              ]
            }
          ],
          "out": { "stamp": "column", "avoid_overlap": true }
        }
      ]
    }
  ]
}
"#;
    assert_eq!(text, expected);
    // Reading it back gives the same rules, and the same bytes again.
    let set = parse_rules(&text).unwrap();
    assert_eq!(&set, p.rules_for("pillars").unwrap());
    let mut q = p.clone();
    *set_mut(&mut q) = set;
    assert_eq!(rendered(&q, "rules/pillars.json"), expected);
    // The shapes the model gives.
    let r = q.rules_for("pillars").unwrap().groups[0]
        .inline_rules()
        .next()
        .unwrap();
    let g = r.as_region().unwrap();
    assert!(r.is_region() && r.as_match().is_none() && r.out().is_none());
    assert_eq!(r.write().unwrap().grid, "marks");
    assert_eq!(g.place.at, PlaceAt::Along(Edge::Floor));
    assert_eq!((g.place.every_xy(), g.place.margin_xy()), ([6, 6], [3, 3]));
    assert_eq!(g.place.align, Align::Center);
    assert!(g.region.selects(15, 1, 1) && !g.region.selects(14, 9, 99));
    let m = q.rules_for("pillars").unwrap().groups[1]
        .inline_rules()
        .next()
        .unwrap();
    assert_eq!(m.as_match().unwrap().require.len(), 2);
    assert!(m.out().unwrap().avoid_overlap());
    assert_eq!(q.rules_for("pillars").unwrap().groups[1].pass, 1);
}

#[test]
fn every_place_option_round_trips_and_defaults_are_left_out() {
    let rule = json!({ "slug": "r",
        "region": { "grid": "collision", "cells": "![solid|fortified]",
                    "min_width": 2, "max_width": 30, "min_height": 1, "max_height": 9,
                    "min_area": 3, "max_area": 400 },
        "place": { "across": "interior", "spacing": "spread", "every": [6, 3], "margin": [3, 1] },
        "out": { "tileset": "castle_main", "tiles": [[1, 1]], "avoid_overlap": true } });
    let set = one_rule(rule.clone()).unwrap();
    let back = serde_json::to_value(&set.groups[0].rules[0]).unwrap();
    assert_eq!(back, rule);
    let r = set.groups[0].inline_rules().next().unwrap();
    let g = r.as_region().unwrap();
    assert_eq!(g.place.at, PlaceAt::Across);
    assert_eq!((g.place.every_xy(), g.place.margin_xy()), ([6, 3], [3, 1]));
    assert_eq!(g.region.bound_pairs().len(), 3);
    // Defaults are left out: `even`, every 1, margin 0, align center,
    // jitter 0, min_length 1.
    let rule = json!({ "slug": "r", "region": { "cells": "." },
        "place": { "along": "wall_left", "spacing": "even", "every": 1, "margin": 0,
                   "align": "center", "jitter": 0, "min_length": 1 },
        "write": { "grid": "m", "value": "v" } });
    let back = serde_json::to_value(&one_rule(rule).unwrap().groups[0].rules[0]).unwrap();
    assert_eq!(
        back,
        json!({ "slug": "r", "region": { "cells": "." }, "place": { "along": "wall_left" },
            "write": { "grid": "m", "value": "v" } })
    );
    let rule = json!({ "slug": "r", "region": { "cells": "." },
        "place": { "along": "ceiling", "spacing": "random", "every": 4, "jitter": 2,
                   "min_length": 5, "against": "solid", "align": "end" },
        "break": true, "salt": 9, "active": false,
        "write": { "grid": "m", "value": "v" } });
    let set = one_rule(rule.clone()).unwrap();
    assert_eq!(serde_json::to_value(&set.groups[0].rules[0]).unwrap(), rule);
    // A pattern rule that writes.
    let rule = json!({ "slug": "w", "match": { "rows": ["+"] }, "chance": 0.5,
        "write": { "grid": "m", "value": "v" } });
    let set = one_rule(rule.clone()).unwrap();
    assert_eq!(serde_json::to_value(&set.groups[0].rules[0]).unwrap(), rule);
}

#[test]
fn require_is_one_pattern_or_a_list() {
    let one = json!({ "slug": "r", "match": { "rows": ["#"] },
        "require": [{ "grid": "marks", "rows": ["p"] }],
        "out": { "tileset": "t", "tiles": [[0, 0]] } });
    let set = one_rule(one).unwrap();
    // A list of one saves as the single pattern.
    assert_eq!(
        serde_json::to_value(&set.groups[0].rules[0]).unwrap()["require"],
        json!({ "grid": "marks", "rows": ["p"] })
    );
    let single = json!({ "slug": "r", "match": { "rows": ["#"] },
        "require": { "rows": ["."] }, "out": { "tileset": "t", "tiles": [[0, 0]] } });
    let r = one_rule(single).unwrap();
    assert_eq!(
        r.groups[0]
            .inline_rules()
            .next()
            .unwrap()
            .as_match()
            .unwrap()
            .require
            .len(),
        1
    );
    let e = rule_err(
        json!({ "slug": "r", "match": { "rows": ["#"] }, "require": [],
        "out": { "tileset": "t", "tiles": [[0, 0]] } }),
    );
    assert!(
        e.contains("at groups[0].rules[0]") && e.contains("non-empty array"),
        "{e}"
    );
    // Templates take a list too.
    let m: Macros = format::parse(
        Path::new("rules/macros.json"),
        &json!({ "$schema": "../schema/macros.json", "format": 1, "templates": { "t": { "rules": [
            { "slug": "a", "match": { "rows": ["#"] },
              "require": [{ "rows": ["."] }, { "grid": "marks", "rows": ["*"] }],
              "out": { "tiles": [[0, 0]] } } ] } } })
        .to_string(),
    )
    .unwrap();
    assert_eq!(m.templates["t"].rules[0].require.len(), 2);
    assert_eq!(m.templates["t"].rules[0].patterns().count(), 3);
}

#[test]
fn misplaced_rule_keys_are_load_errors_naming_them() {
    let out = json!({ "tileset": "t", "tiles": [[0, 0]] });
    let write = json!({ "grid": "m", "value": "v" });
    let region = json!({ "cells": "." });
    let place = json!({ "along": "floor" });
    let cases = [
        (
            json!({ "slug": "r", "match": { "rows": ["#"] }, "region": region, "place": place, "out": out }),
            "a rule has either \"match\" (a pattern rule) or \"region\" (a region rule), not both",
        ),
        (
            json!({ "slug": "r", "out": out }),
            "missing field `match` (or `region` and `place`, for a region rule)",
        ),
        (
            json!({ "slug": "r", "match": { "rows": ["#"] }, "place": place, "out": out }),
            "\"place\" is only allowed on a region rule (one with \"region\")",
        ),
        (
            json!({ "slug": "r", "region": region, "out": out }),
            "a region rule needs \"place\" (where in each region it fires)",
        ),
        (
            json!({ "slug": "r", "match": { "rows": ["#"] }, "out": out, "write": write }),
            "a rule has either \"out\" (draw) or \"write\" (write a derived grid), not both",
        ),
        (
            json!({ "slug": "r", "match": { "rows": ["#"] } }),
            "missing field `out` (or `write`, to write a derived grid)",
        ),
        (
            json!({ "slug": "r", "region": region, "place": { "along": "floor", "across": "interior" }, "out": out }),
            "\"place\" needs exactly one of \"along\"",
        ),
        (
            json!({ "slug": "r", "region": region, "place": { "spacing": "even" }, "out": out }),
            "\"place\" needs exactly one of \"along\"",
        ),
        (
            json!({ "slug": "r", "region": region, "place": { "along": "floor", "every": [2, 3] }, "out": out }),
            "\"every\": [x, y] only works with \"across\"",
        ),
        (
            json!({ "slug": "r", "region": region, "place": { "along": "floor", "margin": [2, 3] }, "out": out }),
            "\"margin\": [x, y] only works with \"across\"",
        ),
        (
            json!({ "slug": "r", "region": region, "place": { "across": "interior", "min_length": 2 }, "out": out }),
            "\"min_length\" is only allowed with \"along\", not with \"across\"",
        ),
        (
            json!({ "slug": "r", "region": region, "place": { "across": "interior", "against": "solid" }, "out": out }),
            "\"against\" is only allowed with \"along\", not with \"across\"",
        ),
        (
            json!({ "slug": "r", "region": { "cells": ".", "min_width": 0 }, "place": place, "out": out }),
            "expected a nonzero u32",
        ),
        (
            json!({ "slug": "r", "region": { "cells": ".", "width": 3 }, "place": place, "out": out }),
            "unknown field `width`",
        ),
        (
            json!({ "slug": "r", "region": region, "place": { "along": "roof" }, "out": out }),
            "unknown variant `roof`",
        ),
        (
            json!({ "slug": "r", "region": region, "place": place, "write": { "grid": "m" } }),
            "missing field `value`",
        ),
    ];
    for (rule, needle) in cases {
        let e = rule_err(rule.clone());
        assert!(e.contains("at groups[0].rules[0]"), "{rule}: {e}");
        assert!(e.contains(needle), "{rule}: {e}");
    }
    // Every key only a pattern rule has.
    for (key, v) in [
        ("require", json!({ "rows": ["#"] })),
        ("symmetry", json!([])),
        ("chance", json!(1)),
        ("modulo", json!({ "x": 2, "y": 1 })),
        ("checker", json!("none")),
        (
            "perlin",
            json!({ "active": false, "seed": 1, "scale": 0.2, "octaves": 2 }),
        ),
        ("out_of_bounds", json!(".")),
    ] {
        let mut rule = json!({ "slug": "r", "region": region, "place": place, "out": out });
        rule[key] = v;
        let e = rule_err(rule);
        assert!(
            e.contains(&format!("\"{key}\" is not allowed on a region rule")),
            "{key}: {e}"
        );
    }
    // A region key on a template reference is a rule key there.
    let e = rule_err(json!({ "use": "blob47", "region": region }));
    assert!(
        e.contains("\"region\" is not allowed on a template reference"),
        "{e}"
    );
}

#[test]
fn templates_refuse_region_rules_and_writes() {
    for extra in [
        json!({ "region": { "cells": "." } }),
        json!({ "place": { "along": "floor" } }),
        json!({ "write": { "grid": "m", "value": "v" } }),
    ] {
        let mut r =
            json!({ "slug": "a", "match": { "rows": ["#"] }, "out": { "tiles": [[0, 0]] } });
        for (k, v) in extra.as_object().unwrap() {
            r[k] = v.clone();
        }
        let e = format::parse::<Macros>(
            Path::new("rules/macros.json"),
            &json!({ "$schema": "../schema/macros.json", "format": 1,
                "templates": { "t": { "rules": [r] } } })
            .to_string(),
        )
        .unwrap_err()
        .to_string();
        assert!(e.contains("at templates.t.rules[0]"), "{e}");
        assert!(
            e.contains("templates can't hold region rules (\"region\", \"place\") or rules that write a derived grid (\"write\")"),
            "{e}"
        );
    }
    // A region rule can't become a template rule either.
    let r: Rule = serde_json::from_value(region_rule()).unwrap();
    assert!(
        TemplateRule::try_from(r)
            .unwrap_err()
            .contains("templates can't hold region rules")
    );
}

#[test]
fn group_pass_and_level_tag_requirement() {
    let set = parse_rules(
        &json!({ "$schema": "../schema/rules.json", "format": 1, "layer": "x", "groups": [
            { "slug": "a", "mode": "optional", "pass": 2,
              "requires_level_field": { "field": "biome", "values": ["flooded"] },
              "requires_level_tag": { "tags": ["ruined", "forest"], "mode": "all" },
              "rules": [] },
            { "slug": "b", "pass": 0, "requires_level_tag": { "tags": ["x"], "mode": "any" }, "rules": [] }
        ] })
        .to_string(),
    )
    .unwrap();
    assert_eq!((set.groups[0].pass, set.groups[1].pass), (2, 0));
    let req = set.groups[0].requires_level_tag.as_ref().unwrap();
    assert_eq!(req.mode, RequirementMode::All);
    assert!(req.holds(&["forest".into(), "ruined".into()]));
    assert!(!req.holds(&["forest".into()]));
    let any = set.groups[1].requires_level_tag.as_ref().unwrap();
    assert!(any.holds(&["x".into()]) && !any.holds(&[]));
    // `pass: 0` and `mode: any` are defaults: left out when saved.
    let b = serde_json::to_value(&set.groups[1]).unwrap();
    assert_eq!(
        b,
        json!({ "slug": "b", "requires_level_tag": { "tags": ["x"] }, "rules": [] })
    );
    let a = serde_json::to_string(&set.groups[0]).unwrap();
    assert!(
        a.starts_with(r#"{"slug":"a","mode":"optional","pass":2,"requires_level_field""#),
        "pass after mode, the tag requirement after the field one: {a}"
    );
    assert!(
        a.contains(r#""requires_level_tag":{"tags":["ruined","forest"],"mode":"all"},"rules""#)
    );
    for (pass, needle) in [
        (json!(-1), "invalid value: integer `-1`"),
        (json!(1.5), "invalid type: floating point `1.5`"),
        (json!("1"), "invalid type: string"),
    ] {
        let e = parse_rules(
            &json!({ "$schema": "x", "format": 1, "layer": "x",
                "groups": [{ "slug": "a", "pass": pass, "rules": [] }] })
            .to_string(),
        )
        .unwrap_err();
        assert!(e.contains("at groups[0].pass") && e.contains(needle), "{e}");
    }
    let e = parse_rules(
        &json!({ "$schema": "x", "format": 1, "layer": "x",
            "groups": [{ "slug": "a", "requires_level_tag": { "tag": ["x"] }, "rules": [] }] })
        .to_string(),
    )
    .unwrap_err();
    assert!(e.contains("unknown field `tag`"), "{e}");
}

#[test]
fn avoid_overlap_works_on_any_output_and_occupies_on_stamps() {
    let set = one_rule(json!({ "slug": "r", "match": { "rows": ["#"] },
        "out": { "tileset": "castle_main", "tiles": [[0, 0]], "tile_mode": "stamp", "avoid_overlap": true } }))
    .unwrap();
    let r = set.groups[0].inline_rules().next().unwrap();
    assert!(r.out().unwrap().as_tiles().unwrap().avoid_overlap);
    assert_eq!(
        serde_json::to_value(r).unwrap()["out"],
        json!({ "tileset": "castle_main", "tiles": [[0, 0]], "tile_mode": "stamp", "avoid_overlap": true })
    );
    // A stamp can't take tile options, nor a tiles output a stamp.
    let e = rule_err(json!({ "slug": "r", "match": { "rows": ["#"] },
        "out": { "stamp": "s", "pivot": [0.5, 0.0] } }));
    assert!(
        e.contains("\"pivot\" is not allowed on stamp outputs"),
        "{e}"
    );
    // `occupies` only on stamps.
    let e = parse_rules(
        &json!({ "$schema": "x", "format": 1, "layer": "x", "groups": [{ "slug": "g", "rules": [
            { "slug": "r", "match": { "rows": ["#"] },
              "out": { "tileset": "t", "tiles": [[0, 0]], "occupies": true } } ] }] })
        .to_string(),
    )
    .unwrap_err();
    assert!(e.contains("unknown field `occupies`"), "{e}");
}

// ---- derived layers: files ----

/// The M6 project saved to a temp dir.
fn saved() -> (tempfile::TempDir, PathBuf, Project) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("p.worlds");
    let mut p = project();
    save(&mut p, &dir).unwrap();
    (tmp, dir, p)
}

const CREVASSE: &str = "worlds/main/levels/crevasse";

#[test]
fn derived_files_hold_overrides_and_say_so() {
    let (_tmp, dir, mut p) = saved();
    apply(
        &mut p,
        json!({ "op": "paint_cells", "level": "crevasse", "layer": "marks",
            "cells": [[4, 3, "mark"], [5, 3, "!"], [6, 3, "big"]] }),
    );
    save_dirty(&mut p, &dir).unwrap();
    let text = std::fs::read_to_string(dir.join(CREVASSE).join("marks.json")).unwrap();
    assert_eq!(
        text,
        r#"{
  "$schema": "../../../../schema/intgrid.json",
  "format": 1,
  "layer": "marks",
  "overrides": true,
  "legend": { "B": "big", "p": "mark" },
  "rows": [
    "....................",
    "....................",
    "....................",
    "....p!B.............",
    "...................."
  ]
}
"#
    );
    // It loads into the level's overrides, never as a grid.
    let q = load(&dir).unwrap();
    let l = q.level("crevasse").unwrap();
    assert!(l.int_grid("marks").is_none());
    let o = l.overrides("marks").unwrap();
    assert_eq!(o.get(4, 3), Some(Override::Value(1)));
    assert_eq!(o.get(5, 3), Some(Override::Suppress));
    assert!(o.is_suppressed(5, 3) && !o.is_suppressed(4, 3));
    assert_eq!(o.get(6, 3), Some(Override::Value(2)));
    assert_eq!(o.get(0, 0), Some(Override::None));
    assert_eq!(o.counts(), (2, 1));
    assert_eq!(q, p);
}

fn write_level_file(dir: &Path, name: &str, body: Value) {
    let mut v = json!({ "$schema": "../../../../schema/intgrid.json", "format": 1 });
    for (k, x) in body.as_object().unwrap() {
        v[k] = x.clone();
    }
    std::fs::write(dir.join(CREVASSE).join(name), v.to_string()).unwrap();
}

fn load_err(dir: &Path) -> String {
    load(dir).unwrap_err().to_string()
}

#[test]
fn overrides_key_is_required_on_derived_files_and_refused_elsewhere() {
    let (_tmp, dir, _) = saved();
    let rows = json!([
        "....................",
        "....................",
        "....................",
        "....p!..............",
        "...................."
    ]);
    write_level_file(
        &dir,
        "marks.json",
        json!({ "layer": "marks", "rows": rows }),
    );
    let e = load_err(&dir);
    assert!(
        e.contains("marks.json")
            && e.contains("layer \"marks\" is derived: its file holds overrides and must say \"overrides\": true"),
        "{e}"
    );
    write_level_file(
        &dir,
        "marks.json",
        json!({ "layer": "marks", "overrides": false, "rows": rows }),
    );
    let e = load_err(&dir);
    assert!(
        e.contains(
            "at overrides: \"overrides\" is either true (a derived layer's file) or left out"
        ),
        "{e}"
    );
    write_level_file(
        &dir,
        "marks.json",
        json!({ "layer": "marks", "overrides": true, "legend": { "!": null }, "rows": rows }),
    );
    let e = load_err(&dir);
    assert!(e.contains("at legend.!: '!' is built in"), "{e}");
    // `.` is built in too (no override): saving never lists it, and a
    // `".": null` entry would read as "empty", which a derived file can't say.
    write_level_file(
        &dir,
        "marks.json",
        json!({ "layer": "marks", "overrides": true, "legend": { ".": null }, "rows": rows }),
    );
    let e = load_err(&dir);
    assert!(
        e.contains("at legend..: '.' is built in (no override, in a derived layer's file)"),
        "{e}"
    );
    write_level_file(
        &dir,
        "marks.json",
        json!({ "layer": "marks", "overrides": true, "rows": rows }),
    );
    let p = load(&dir).unwrap();
    assert_eq!(
        p.level("crevasse")
            .unwrap()
            .overrides("marks")
            .unwrap()
            .counts(),
        (1, 1)
    );
    // On a layer that isn't derived.
    let collision = std::fs::read_to_string(dir.join(CREVASSE).join("collision.json")).unwrap();
    let mut v: Value = serde_json::from_str(&collision).unwrap();
    v["overrides"] = json!(true);
    std::fs::write(dir.join(CREVASSE).join("collision.json"), v.to_string()).unwrap();
    let e = load_err(&dir);
    assert!(
        e.contains("at overrides: \"overrides\" is only for a derived layer's file; layer \"collision\" isn't derived"),
        "{e}"
    );
    // `!` outside a derived file.
    v.as_object_mut().unwrap().remove("overrides");
    v["rows"][1] = json!("#........!.........#");
    std::fs::write(dir.join(CREVASSE).join("collision.json"), v.to_string()).unwrap();
    let e = load_err(&dir);
    assert!(
        e.contains("at rows[1]: '!' (suppress) only appears in a derived layer's file; layer \"collision\" isn't derived"),
        "{e}"
    );
}

#[test]
fn an_all_dot_override_file_is_not_saved() {
    let (_tmp, dir, _) = saved();
    write_level_file(
        &dir,
        "marks.json",
        json!({ "layer": "marks", "overrides": true, "rows": [
            "....................", "....................", "....................",
            "....................", "...................."] }),
    );
    let mut p = load(&dir).unwrap();
    assert!(
        p.level("crevasse")
            .unwrap()
            .overrides("marks")
            .unwrap()
            .is_empty()
    );
    assert!(
        format::render_file(&p, &Path::new(CREVASSE).join("marks.json"))
            .unwrap()
            .is_none()
    );
    save_all(&mut p, &dir).unwrap();
    assert!(
        !dir.join(CREVASSE).join("marks.json").exists(),
        "empty layers go away"
    );
}

// ---- project helpers ----

#[test]
fn writers_reads_and_bake_order() {
    let mut p = project();
    assert_eq!(p.derived_writers("marks"), [&s("pillars")]);
    assert_eq!(p.derived_writer("marks"), Some(&s("pillars")));
    assert_eq!(p.derived_writer("collision"), None);
    assert_eq!(p.layer_writes("pillars"), [s("marks")]);
    assert_eq!(p.layer_reads("pillars"), [s("collision"), s("marks")]);
    assert_eq!(p.layer_reads("main_tiling"), [s("collision")]);
    assert!(p.layer_reads("collision").is_empty());
    // `pillars` reads only what it writes itself: no dependency.
    assert!(p.layer_dependencies("pillars").is_empty());
    assert_eq!(
        p.auto_layer_order().unwrap(),
        [s("pillars"), s("main_tiling")],
        "layers.json order when nothing depends on anything"
    );
    // main_tiling (index 3) reads marks: still after pillars. Now make the
    // reader come first in layers.json: the writer bakes first anyway.
    rule_mut(&mut p, 0); // (exists)
    let reader = p
        .rules
        .iter_mut()
        .find(|r| r.layer == "main_tiling")
        .unwrap();
    reader.groups[0].rules[1]
        .as_rule_mut()
        .unwrap()
        .as_match_mut()
        .unwrap()
        .pattern
        .grid = Some(s("marks"));
    let mt = p
        .layers
        .iter()
        .position(|l| l.slug == "main_tiling")
        .unwrap();
    let def = p.layers.remove(mt);
    p.layers.insert(0, def);
    assert_eq!(
        p.layer_dependencies("main_tiling"),
        [(s("marks"), s("pillars"))]
    );
    assert_eq!(
        p.auto_layer_order().unwrap(),
        [s("pillars"), s("main_tiling")]
    );
    // A region rule's grid counts as read.
    let mut q = project();
    rule_mut(&mut q, 0).as_region_mut().unwrap().region.grid = Some(s("marks"));
    assert_eq!(q.layer_reads("pillars"), [s("collision"), s("marks")]);
}

/// Two auto layers, each reading the other's derived grid.
fn cyclic() -> Project {
    let mut p = project();
    let mut b = marks_def();
    b["slug"] = json!("marks_b");
    apply(
        &mut p,
        json!({ "op": "set_layer_def", "layer": "marks_b", "def": b }),
    );
    // main_tiling writes marks_b; pillars reads it; main_tiling reads marks.
    apply(
        &mut p,
        json!({ "op": "set_rule_group", "layer": "main_tiling", "group": "derive",
            "def": { "slug": "derive", "rules": [
                { "slug": "to_b", "match": { "grid": "marks", "rows": ["p"] },
                  "write": { "grid": "marks_b", "value": "mark" } } ] } }),
    );
    apply(
        &mut p,
        json!({ "op": "set_rule", "layer": "pillars", "group": "art", "rule": "from_b",
            "def": { "slug": "from_b", "match": { "grid": "marks_b", "rows": ["p"] },
                     "out": { "tileset": "castle_main", "tiles": [[0, 0]] } } }),
    );
    p
}

#[test]
fn a_cycle_of_derived_grids_has_no_order() {
    let p = cyclic();
    let e = p.auto_layer_order().unwrap_err();
    assert_eq!(
        e.path,
        [
            s("pillars"),
            s("marks_b"),
            s("main_tiling"),
            s("marks"),
            s("pillars")
        ]
    );
    assert_eq!(
        e.to_string(),
        "auto layers pillars and main_tiling read each other's derived grids: \
         pillars → marks_b → main_tiling → marks → pillars"
    );
    assert_eq!(e.layers(), [&s("pillars"), &s("main_tiling")]);
    let ps = problems(&p);
    let i = p.layers.iter().position(|l| l.slug == "pillars").unwrap();
    assert!(
        ps.contains(&format!(
            "layers.json: layers[{i}]: auto layers pillars and main_tiling read each other's \
             derived grids: pillars → marks_b → main_tiling → marks → pillars"
        )),
        "{ps:?}"
    );
}

// ---- validation ----

const PILLARS: &str = "rules/pillars.json";

fn has(ps: &[String], expected: &str) {
    assert!(
        ps.iter().any(|p| p == expected),
        "{expected}\nnot in {ps:#?}"
    );
}

#[test]
fn groups_must_be_in_pass_order() {
    let ps = new_problems(|p| {
        set_mut(p).groups.swap(0, 1);
    });
    assert_eq!(
        ps,
        [format!(
            "{PILLARS}: groups[1].pass: group marks (pass 0) comes after a pass 1 group; keep the \
             file in pass order"
        )]
    );
}

#[test]
fn level_tag_lists_are_checked() {
    let ps = new_problems(|p| {
        set_mut(p).groups[1].requires_level_tag = Some(LevelTagRequirement {
            tags: vec![],
            mode: RequirementMode::Any,
        });
    });
    assert_eq!(
        ps,
        [format!(
            "{PILLARS}: groups[1].requires_level_tag.tags: needs at least one tag"
        )]
    );
    let ps = new_problems(|p| {
        set_mut(p).groups[1]
            .requires_level_tag
            .as_mut()
            .unwrap()
            .tags = vec!["a".into(), "".into(), "a".into()];
    });
    assert_eq!(
        ps,
        [
            format!("{PILLARS}: groups[1].requires_level_tag.tags[1]: empty tag"),
            format!("{PILLARS}: groups[1].requires_level_tag.tags[2]: duplicate tag \"a\""),
        ]
    );
}

#[test]
fn region_rule_checks() {
    fn region(p: &mut Project) -> &mut RegionSelect {
        rule_mut(p, 0).as_region_mut().unwrap()
    }
    let at = format!("{PILLARS}: groups[0].rules[0]");
    let ps = new_problems(|p| region(p).region.cells = CellMatch::Any);
    assert_eq!(
        ps,
        [format!(
            "{at}.region.cells: \"*\" matches every cell: every cell would be one region"
        )]
    );
    let ps = new_problems(|p| {
        region(p).region.cells = serde_json::from_value(json!("![solid|lava]")).unwrap()
    });
    assert_eq!(
        ps,
        [format!(
            "{at}.region.cells: \"lava\" is not a value or group of \"collision\""
        )]
    );
    let ps = new_problems(|p| {
        let r = &mut region(p).region;
        r.max_width = std::num::NonZeroU32::new(10);
        r.min_area = std::num::NonZeroU32::new(5);
        r.max_area = std::num::NonZeroU32::new(5);
    });
    assert_eq!(
        ps,
        [format!(
            "{at}.region.min_width: min_width 15 is greater than max_width 10"
        )]
    );
    let ps = new_problems(|p| region(p).region.grid = Some(s("ghost")));
    assert_eq!(ps, [format!("{at}.region.grid: unknown layer \"ghost\"")]);
    let ps = new_problems(|p| region(p).region.grid = Some(s("decor_back")));
    assert_eq!(
        ps,
        [format!(
            "{at}.region.grid: \"decor_back\" is not an int_grid layer"
        )]
    );
    let ps = new_problems(|p| {
        p.layers
            .iter_mut()
            .find(|l| l.slug == "marks")
            .unwrap()
            .grid_size = Some(8);
        region(p).region.grid = Some(s("marks"));
    });
    has(
        &ps,
        &format!("{at}.region.grid: \"marks\" is on a 8 px grid, but this layer bakes on 16 px"),
    );
    let ps = new_problems(|p| {
        region(p).place.against = Some(serde_json::from_value(json!("[walls|lava]")).unwrap())
    });
    assert_eq!(
        ps,
        [format!(
            "{at}.place.against: \"lava\" is not a value or group of \"collision\""
        )]
    );
    let ps = new_problems(|p| {
        let pl = &mut region(p).place;
        pl.spacing = Spacing::Spread;
        pl.align = Align::End;
        pl.jitter = 2;
    });
    assert_eq!(
        ps,
        [
            format!("{at}.place.align: align only applies to spacing \"even\""),
            format!("{at}.place.jitter: jitter only applies to spacing \"random\""),
        ]
    );
    // Random spacing takes jitter, and align at its default is fine.
    let ps = new_problems(|p| {
        let pl = &mut region(p).place;
        pl.spacing = Spacing::Random;
        pl.jitter = 2;
    });
    assert!(ps.is_empty(), "{ps:?}");
}

#[test]
fn write_checks() {
    let at = format!("{PILLARS}: groups[0].rules[0]");
    fn write(p: &mut Project) -> &mut Write {
        rule_mut(p, 0).write_mut().unwrap()
    }
    let ps = new_problems(|p| write(p).grid = s("ghost"));
    assert_eq!(ps, [format!("{at}.write.grid: unknown layer \"ghost\"")]);
    let ps = new_problems(|p| write(p).grid = s("objects"));
    assert_eq!(
        ps,
        [format!(
            "{at}.write.grid: \"objects\" is not an int_grid layer"
        )]
    );
    let ps = new_problems(|p| {
        let w = write(p);
        w.grid = s("collision");
        w.value = s("solid");
    });
    assert_eq!(
        ps,
        [format!(
            "{at}.write.grid: \"collision\" is not a derived layer (only a layer with \
             \"derived\": true can be written)"
        )]
    );
    let ps = new_problems(|p| write(p).value = s("spots"));
    assert_eq!(
        ps,
        [format!(
            "{at}.write.value: \"spots\" is a group of \"marks\"; write needs a value"
        )]
    );
    let ps = new_problems(|p| write(p).value = s("lava"));
    assert_eq!(
        ps,
        [format!(
            "{at}.write.value: \"lava\" is not a value of \"marks\""
        )]
    );
    // A second layer writing the same grid.
    let ps = new_problems(|p| {
        apply(
            p,
            json!({ "op": "set_rule_group", "layer": "main_tiling", "group": "also",
                "def": { "slug": "also", "rules": [
                    { "slug": "also_marks", "match": { "rows": ["#"] },
                      "write": { "grid": "marks", "value": "big" } } ] } }),
        );
    });
    assert_eq!(
        ps,
        [
            "rules/main_tiling.json: groups[4].rules[0].write.grid: marks is already written by \
          layer pillars"
        ]
    );
}

#[test]
fn require_lists_are_checked_per_pattern() {
    let ps = new_problems(|p| {
        let m = rule_mut(p, 1).as_match_mut().unwrap();
        m.require[1].rows = Rows::Inline(vec!["q".into()]);
    });
    assert_eq!(
        ps,
        [format!(
            "{PILLARS}: groups[1].rules[0].require[1].rows[0]: glyph 'q' means nothing: not in \
             the legend, not a built-in (* + . @), not a value or group glyph of \"marks\""
        )]
    );
    // With one `require` the path has no index, as in the file.
    let ps = new_problems(|p| {
        let m = rule_mut(p, 1).as_match_mut().unwrap();
        m.require.truncate(1);
        m.require[0].rows = Rows::Inline(vec!["q".into()]);
    });
    assert!(
        ps[0].starts_with(&format!(
            "{PILLARS}: groups[1].rules[0].require.rows[0]: glyph 'q'"
        )),
        "{ps:?}"
    );
}

#[test]
fn the_collision_layer_cannot_be_derived() {
    let ps = new_problems(|p| p.collision_layer = Some(s("marks")));
    has(
        &ps,
        "project.json: collision_layer: \"marks\" is a derived layer; the collision layer is \
         painted, it can't be computed by rules",
    );
}

#[test]
fn stored_overrides_must_fit_the_level() {
    let ps = new_problems(|p| {
        let def = p.layer("marks").unwrap().clone();
        let l = p.worlds[0]
            .levels
            .iter_mut()
            .find(|l| l.slug == "crevasse")
            .unwrap();
        l.overrides.push(DerivedOverrides::new_empty(&def, [3, 2]));
    });
    assert_eq!(
        ps,
        [
            format!("{CREVASSE}/marks.json: rows: 2 rows, level is 5 cells tall"),
            format!("{CREVASSE}/marks.json: rows: rows are 3 glyphs wide, level is 20 cells wide"),
        ]
    );
}

// ---- commands on derived layers ----

#[test]
fn grid_values_on_the_wire_and_in_the_schema() {
    let c: CellValue = serde_json::from_value(json!([1, 2, "!"])).unwrap();
    assert_eq!(c.value, GridValue::Suppress);
    let c: CellValue = serde_json::from_value(json!([1, 2, null])).unwrap();
    assert_eq!(c.value, GridValue::Clear);
    let c: CellValue = serde_json::from_value(json!([1, 2, "mark"])).unwrap();
    assert_eq!(c.value, GridValue::Value(s("mark")));
    assert_eq!(serde_json::to_value(&c).unwrap(), json!([1, 2, "mark"]));
    assert_eq!(
        serde_json::to_value(CellValue::new(0, 0, GridValue::Suppress)).unwrap(),
        json!([0, 0, "!"])
    );
    let e = serde_json::from_value::<CellValue>(json!([1, 2, "Bad"])).unwrap_err();
    assert!(
        e.to_string()
            .contains("a grid value is a value slug, null, or \"!\""),
        "{e}"
    );
    // `value` may be left out (empty), as before.
    let c = cmd(json!({ "op": "fill_rect", "level": "a", "layer": "b", "rect": [0, 0, 1, 1] }));
    assert!(matches!(
        c,
        Command::FillRect {
            value: GridValue::Clear,
            ..
        }
    ));
    let schema = serde_json::to_value(schemars::schema_for!(Command)).unwrap();
    assert_eq!(
        schema["$defs"]["GridValue"]["anyOf"],
        json!([{ "$ref": "#/$defs/Slug" }, { "type": "null" }, { "const": "!" }])
    );
    assert_eq!(
        schema["$defs"]["CellValue"]["prefixItems"][2],
        json!({ "$ref": "#/$defs/GridValue" })
    );
    // Every grid command's value is a GridValue.
    let ops = schema["oneOf"].as_array().unwrap();
    for (op, keys) in [
        ("fill_rect", &["value"][..]),
        ("flood_fill", &["value"]),
        ("replace_value", &["from", "to"]),
    ] {
        let v = ops
            .iter()
            .find(|v| v["properties"]["op"]["const"] == op)
            .unwrap();
        for k in keys {
            assert_eq!(v["properties"][k]["$ref"], "#/$defs/GridValue", "{op}.{k}");
        }
    }
    // A command with "!" is labelled.
    let c =
        cmd(json!({ "op": "replace_value", "level": "a", "layer": "b", "from": "!", "to": null }));
    assert_eq!(c.label(), "Replace suppressed with empty on b");
}

#[test]
fn suppress_is_refused_on_layers_that_are_not_derived() {
    let mut p = project();
    for v in [
        json!({ "op": "paint_cells", "level": "crevasse", "layer": "collision", "cells": [[1, 1, "!"]] }),
        json!({ "op": "fill_rect", "level": "crevasse", "layer": "collision", "rect": [1, 1, 2, 1], "value": "!" }),
        json!({ "op": "flood_fill", "level": "crevasse", "layer": "collision", "at": [1, 1], "value": "!" }),
        json!({ "op": "replace_value", "level": "crevasse", "layer": "collision", "from": "!", "to": null }),
    ] {
        let e = refused(&mut p, v);
        assert_eq!(
            e,
            CommandError::Invalid(
                "\"!\" (suppress) only applies to derived layers; collision isn't derived".into()
            )
        );
    }
}

/// Apply `v`, save, check the tree is `check`ed; undo, save, the tree is
/// back byte for byte.
fn exact_on_disk(v: Value, check: impl Fn(&Project, &BTreeMap<String, String>)) {
    let (_tmp, dir, mut p) = saved();
    let before = tree(&dir);
    let mut h = History::default();
    h.push(&mut p, cmd(v)).unwrap();
    save_dirty(&mut p, &dir).unwrap();
    check(&p, &tree(&dir));
    h.undo(&mut p).unwrap();
    save_dirty(&mut p, &dir).unwrap();
    assert_eq!(tree(&dir), before, "undo is byte-exact on disk");
    h.redo(&mut p).unwrap();
    save_dirty(&mut p, &dir).unwrap();
    check(&p, &tree(&dir));
}

fn marks_rows(t: &BTreeMap<String, String>) -> Vec<String> {
    let v: Value = serde_json::from_str(&t[&format!("{CREVASSE}/marks.json")]).unwrap();
    assert_eq!(v["overrides"], true);
    serde_json::from_value(v["rows"].clone()).unwrap()
}

#[test]
fn paint_sets_clears_and_suppresses_overrides_exactly() {
    exact_on_disk(
        json!({ "op": "paint_cells", "level": "crevasse", "layer": "marks",
            "cells": [[1, 1, "mark"], [2, 1, "!"], [3, 1, null]] }),
        |_, t| assert_eq!(marks_rows(t)[1], ".p!................."),
    );
    // Back to the rules: `null` removes an override; the inverse puts the
    // old states back, `!` included.
    let mut p = project();
    apply(
        &mut p,
        json!({ "op": "paint_cells", "level": "crevasse", "layer": "marks",
            "cells": [[1, 1, "mark"], [2, 1, "!"]] }),
    );
    let a = apply(
        &mut p,
        json!({ "op": "paint_cells", "level": "crevasse", "layer": "marks",
            "cells": [[1, 1, null], [2, 1, "big"], [3, 1, "!"]] }),
    );
    assert_eq!(
        serde_json::to_value(&a.inverse).unwrap(),
        json!({ "op": "paint_cells", "level": "crevasse", "layer": "marks",
            "cells": [[1, 1, "mark"], [2, 1, "!"], [3, 1, null]] })
    );
    assert_eq!(
        a.changes,
        [command::Change::Grid {
            world: s("main"),
            level: s("crevasse"),
            layer: s("marks"),
            rect: command::Rect::new(1, 1, 3, 1),
        }]
    );
    let o = p.level("crevasse").unwrap().overrides("marks").unwrap();
    assert_eq!(
        [o.get(1, 1), o.get(2, 1), o.get(3, 1)],
        [
            Some(Override::None),
            Some(Override::Value(2)),
            Some(Override::Suppress)
        ]
    );
    // Painting what is there is a no-op.
    let a = apply(
        &mut p,
        json!({ "op": "paint_cells", "level": "crevasse", "layer": "marks", "cells": [[3, 1, "!"]] }),
    );
    assert!(a.changes.is_empty() && a.inverse.is_noop());
}

#[test]
fn the_overrides_file_goes_away_with_the_last_override() {
    let (_tmp, dir, mut p) = saved();
    apply(
        &mut p,
        json!({ "op": "paint_cells", "level": "crevasse", "layer": "marks", "cells": [[1, 1, "!"]] }),
    );
    save_dirty(&mut p, &dir).unwrap();
    let file = dir.join(CREVASSE).join("marks.json");
    assert!(file.exists());
    let before = tree(&dir);
    let mut h = History::default();
    h.push(
        &mut p,
        cmd(json!({ "op": "paint_cells", "level": "crevasse", "layer": "marks", "cells": [[1, 1, null]] })),
    )
    .unwrap();
    assert!(p.level("crevasse").unwrap().overrides("marks").is_none());
    save_dirty(&mut p, &dir).unwrap();
    assert!(!file.exists(), "no override left, no file");
    h.undo(&mut p).unwrap();
    save_dirty(&mut p, &dir).unwrap();
    assert_eq!(tree(&dir), before);
}

#[test]
fn fill_flood_and_replace_work_on_the_stored_states() {
    exact_on_disk(
        json!({ "op": "fill_rect", "level": "crevasse", "layer": "marks",
            "rect": [2, 2, 3, 2], "value": "!" }),
        |_, t| {
            let rows = marks_rows(t);
            assert_eq!(
                (rows[2].as_str(), rows[3].as_str()),
                ("..!!!...............", "..!!!...............")
            );
        },
    );
    let mut p = project();
    apply(
        &mut p,
        json!({ "op": "fill_rect", "level": "crevasse", "layer": "marks", "rect": [0, 0, 20, 1], "value": "mark" }),
    );
    apply(
        &mut p,
        json!({ "op": "paint_cells", "level": "crevasse", "layer": "marks", "cells": [[5, 0, "!"]] }),
    );
    // Flood fill from a `mark` cell floods the cells with the same stored
    // state: the run left of the `!`.
    let a = apply(
        &mut p,
        json!({ "op": "flood_fill", "level": "crevasse", "layer": "marks", "at": [0, 0], "value": "big" }),
    );
    let o = p.level("crevasse").unwrap().overrides("marks").unwrap();
    assert_eq!(
        o.to_rows(p.layer("marks").unwrap()).unwrap()[0],
        "BBBBB!pppppppppppppp"
    );
    assert_eq!(
        a.changes[0],
        command::Change::Grid {
            world: s("main"),
            level: s("crevasse"),
            layer: s("marks"),
            rect: command::Rect::new(0, 0, 5, 1),
        }
    );
    // Flood from a cell with no override floods "no override".
    apply(
        &mut p,
        json!({ "op": "flood_fill", "level": "crevasse", "layer": "marks", "at": [0, 4], "value": "!" }),
    );
    let rows = p
        .level("crevasse")
        .unwrap()
        .overrides("marks")
        .unwrap()
        .to_rows(p.layer("marks").unwrap())
        .unwrap();
    assert_eq!(rows[0], "BBBBB!pppppppppppppp");
    assert!(
        rows[1..].iter().all(|r| r == "!!!!!!!!!!!!!!!!!!!!"),
        "{rows:?}"
    );
    // replace_value takes "!" on both sides.
    let a = apply(
        &mut p,
        json!({ "op": "replace_value", "level": "crevasse", "layer": "marks", "from": "!", "to": null }),
    );
    let rows = p
        .level("crevasse")
        .unwrap()
        .overrides("marks")
        .unwrap()
        .to_rows(p.layer("marks").unwrap())
        .unwrap();
    assert_eq!(rows[0], "BBBBB.pppppppppppppp");
    assert!(rows[1..].iter().all(|r| r == "...................."));
    // Its inverse restores exactly.
    let before = p.clone();
    let inv = a.inverse.clone();
    p.apply(inv).unwrap();
    p.apply(cmd(json!({ "op": "replace_value", "level": "crevasse", "layer": "marks", "from": "!", "to": null }))).unwrap();
    assert_eq!(p, before);
    let a = apply(
        &mut p,
        json!({ "op": "replace_value", "level": "crevasse", "layer": "marks", "from": "big", "to": "!" }),
    );
    assert!(!a.changes.is_empty());
    assert_eq!(
        p.level("crevasse")
            .unwrap()
            .overrides("marks")
            .unwrap()
            .to_rows(p.layer("marks").unwrap())
            .unwrap()[0],
        "!!!!!.pppppppppppppp"
    );
}

#[test]
fn derived_layers_flip_only_without_content() {
    let mut p = project();
    apply(
        &mut p,
        json!({ "op": "paint_cells", "level": "crevasse", "layer": "marks", "cells": [[1, 1, "mark"]] }),
    );
    let mut plain = marks_def();
    plain.as_object_mut().unwrap().remove("derived");
    let e = refused(
        &mut p,
        json!({ "op": "set_layer_def", "layer": "marks", "def": plain }),
    );
    assert_eq!(
        e,
        CommandError::Invalid("marks has overrides in crevasse; clear them first".into())
    );
    let mut derived_collision = serde_json::to_value(p.layer("collision").unwrap()).unwrap();
    derived_collision["derived"] = json!(true);
    let e = refused(
        &mut p,
        json!({ "op": "set_layer_def", "layer": "collision", "def": derived_collision }),
    );
    assert_eq!(
        e,
        CommandError::Invalid(
            "collision has content in crevasse; clear it first (a derived layer's files hold \
             overrides, not cells)"
                .into()
        )
    );
    // Without content the flag changes, and back.
    apply(
        &mut p,
        json!({ "op": "paint_cells", "level": "crevasse", "layer": "marks", "cells": [[1, 1, null]] }),
    );
    let a = apply(
        &mut p,
        json!({ "op": "set_layer_def", "layer": "marks", "def": plain }),
    );
    assert!(!p.layer("marks").unwrap().is_derived());
    p.apply(a.inverse).unwrap();
    assert!(p.layer("marks").unwrap().is_derived());
}

#[test]
fn reordering_values_keeps_overrides_by_slug() {
    let mut p = project();
    apply(
        &mut p,
        json!({ "op": "paint_cells", "level": "crevasse", "layer": "marks",
            "cells": [[1, 1, "mark"], [2, 1, "big"], [3, 1, "!"]] }),
    );
    let before = p.clone();
    let mut def = marks_def();
    let values = def["values"].as_array_mut().unwrap();
    values.reverse();
    let a = apply(
        &mut p,
        json!({ "op": "set_layer_def", "layer": "marks", "def": def }),
    );
    let o = p.level("crevasse").unwrap().overrides("marks").unwrap();
    assert_eq!(
        o.to_rows(p.layer("marks").unwrap()).unwrap()[1],
        ".pB!................"
    );
    assert_eq!(o.get(1, 1), Some(Override::Value(2)), "mark is now value 2");
    assert!(
        a.changes
            .iter()
            .any(|c| matches!(c, command::Change::Grid { layer, .. } if layer == "marks"))
    );
    p.apply(a.inverse).unwrap();
    assert_eq!(p, before);
    // A value an override still uses can't go.
    let mut def = marks_def();
    def["values"].as_array_mut().unwrap().remove(1);
    let e = refused(
        &mut p,
        json!({ "op": "set_layer_def", "layer": "marks", "def": def }),
    );
    assert!(
        e.to_string().contains("value \"big\" of layer \"marks\" is still used by an override in level crevasse (cell [2, 1])"),
        "{e}"
    );
}

#[test]
fn rename_layer_rewrites_region_and_write_grids_and_the_overrides_file() {
    let (_tmp, dir, mut p) = saved();
    rule_mut(&mut p, 0).as_region_mut().unwrap().region.grid = Some(s("marks"));
    apply(
        &mut p,
        json!({ "op": "paint_cells", "level": "crevasse", "layer": "marks", "cells": [[1, 1, "!"]] }),
    );
    save_all(&mut p, &dir).unwrap();
    let before = tree(&dir);
    let mut h = History::default();
    let changes = h
        .push(
            &mut p,
            cmd(json!({ "op": "rename_layer", "from": "marks", "to": "spots_grid" })),
        )
        .unwrap();
    assert!(changes.contains(&command::Change::Def {
        kind: command::DefKind::Rules,
        slug: s("pillars"),
    }));
    assert!(changes.contains(&command::Change::LayerFileRenamed {
        world: s("main"),
        level: s("crevasse"),
        from: s("marks"),
        to: s("spots_grid"),
    }));
    let r = p.rules_for("pillars").unwrap();
    let region = r.groups[0].inline_rules().next().unwrap();
    assert_eq!(
        region.as_region().unwrap().region.grid,
        Some(s("spots_grid"))
    );
    assert_eq!(region.write().unwrap().grid, "spots_grid");
    let art = r.groups[1]
        .inline_rules()
        .next()
        .unwrap()
        .as_match()
        .unwrap();
    assert_eq!(art.pattern.grid, Some(s("spots_grid")));
    assert_eq!(art.require[1].grid, Some(s("spots_grid")));
    assert!(
        p.level("crevasse")
            .unwrap()
            .overrides("spots_grid")
            .is_some()
    );
    save_dirty(&mut p, &dir).unwrap();
    let t = tree(&dir);
    assert!(t.contains_key(&format!("{CREVASSE}/spots_grid.json")));
    assert!(!t.contains_key(&format!("{CREVASSE}/marks.json")));
    assert!(problems(&p).is_empty(), "{:?}", problems(&p));
    h.undo(&mut p).unwrap();
    save_dirty(&mut p, &dir).unwrap();
    assert_eq!(tree(&dir), before);
    // A dangling `write.grid` blocks a rename to that slug.
    let mut q = project();
    rule_mut(&mut q, 0).write_mut().unwrap().grid = s("ghost");
    let e = refused(
        &mut q,
        json!({ "op": "rename_layer", "from": "marks", "to": "ghost" }),
    );
    assert!(
        e.to_string()
            .contains("rule big_room in rules/pillars.json already refers to layer \"ghost\""),
        "{e}"
    );
}

#[test]
fn remove_and_restore_a_derived_layer_with_its_overrides() {
    let (_tmp, dir, mut p) = saved();
    apply(
        &mut p,
        json!({ "op": "paint_cells", "level": "crevasse", "layer": "marks", "cells": [[1, 1, "!"], [2, 2, "big"]] }),
    );
    save_all(&mut p, &dir).unwrap();
    let before = tree(&dir);
    let mut h = History::default();
    let changes = h
        .push(
            &mut p,
            cmd(json!({ "op": "remove_layer_def", "layer": "marks" })),
        )
        .unwrap();
    assert!(changes.contains(&command::Change::Grid {
        world: s("main"),
        level: s("crevasse"),
        layer: s("marks"),
        rect: command::Rect::new(0, 0, 20, 5),
    }));
    assert!(p.level("crevasse").unwrap().overrides.is_empty());
    save_dirty(&mut p, &dir).unwrap();
    assert!(!dir.join(CREVASSE).join("marks.json").exists());
    h.undo(&mut p).unwrap();
    save_dirty(&mut p, &dir).unwrap();
    assert_eq!(tree(&dir), before);
}

#[test]
fn resize_and_delete_level_keep_overrides_exactly() {
    for v in [
        json!({ "op": "resize_level", "world": "main", "level": "crevasse", "size": [18, 6], "anchor": [1, 1] }),
        json!({ "op": "delete_level", "world": "main", "level": "crevasse" }),
    ] {
        let (_tmp, dir, mut p) = saved();
        apply(
            &mut p,
            json!({ "op": "paint_cells", "level": "crevasse", "layer": "marks",
                "cells": [[0, 0, "!"], [5, 3, "mark"], [19, 4, "big"]] }),
        );
        save_all(&mut p, &dir).unwrap();
        let before = tree(&dir);
        let mut h = History::default();
        h.push(&mut p, cmd(v.clone())).unwrap();
        save_dirty(&mut p, &dir).unwrap();
        if v["op"] == "resize_level" {
            // Anchored right and bottom: cells move 2 left, 1 down; [0, 0]
            // is cropped away.
            let rows = marks_rows(&tree(&dir));
            assert_eq!(rows.len(), 6);
            assert_eq!(rows[4], "...p..............");
            assert_eq!(rows[5], ".................B");
            assert!(rows[0].chars().all(|c| c == '.'));
        }
        h.undo(&mut p).unwrap();
        save_dirty(&mut p, &dir).unwrap();
        assert_eq!(tree(&dir), before, "{v}");
    }
}

#[test]
fn level_content_carries_overrides() {
    let mut p = project();
    apply(
        &mut p,
        json!({ "op": "create_level", "world": "main",
            "level": { "id": "annexlvl22", "slug": "annex", "name": "Annex", "world_px": [640, 0], "size": [4, 2], "depth": 0 },
            "content": { "int_grids": [
                { "layer": "collision", "rows": ["####", "#..#"] },
                { "layer": "marks", "rows": ["p!..", "...."] } ] } }),
    );
    let l = p.level("annex").unwrap();
    assert_eq!(l.overrides("marks").unwrap().counts(), (1, 1));
    // The file form says `overrides: true` when the level comes back.
    let a = apply(
        &mut p,
        json!({ "op": "delete_level", "world": "main", "level": "annex" }),
    );
    let inv = serde_json::to_value(&a.inverse).unwrap();
    assert_eq!(
        inv["content"]["int_grids"][1],
        json!({ "layer": "marks", "overrides": true, "rows": ["p!..", "...."] })
    );
    p.apply(a.inverse).unwrap();
    assert_eq!(
        p.level("annex")
            .unwrap()
            .overrides("marks")
            .unwrap()
            .counts(),
        (1, 1)
    );
    // `!` in a plain layer's content is refused.
    let e = refused(
        &mut p,
        json!({ "op": "create_level", "world": "main",
            "level": { "id": "annexlvl33", "slug": "annex2", "name": "Annex", "world_px": [640, 64], "size": [1, 1], "depth": 0 },
            "content": { "int_grids": [{ "layer": "collision", "rows": ["!"] }] } }),
    );
    assert!(
        e.to_string()
            .contains("'!' (suppress) only appears in a derived layer's file"),
        "{e}"
    );
}

// ---- export ----

fn marks_grid(p: &Project, rows: &[&str]) -> IntGridLayer {
    IntGridLayer::from_rows(p.layer("marks").unwrap(), rows).unwrap()
}

/// The one effective-grid rule (engine, export and editor all use it):
/// override value, suppressed is empty, else the computed value, else
/// empty; sized for the level whether or not it stores overrides.
#[test]
fn derived_effective_is_overrides_over_computed() {
    let mut p = project();
    let def = p.layer("marks").unwrap().clone();
    let computed = marks_grid(
        &p,
        &[
            "....................",
            "....................",
            "....................",
            "....p.....p.........",
            "....................",
        ],
    );
    // No overrides stored: the computed grid, or all empty without one.
    let level = p.level("crevasse").unwrap();
    assert!(level.overrides("marks").is_none());
    assert_eq!(p.derived_effective(level, &def, Some(&computed)), computed);
    assert_eq!(
        p.derived_effective(level, &def, None),
        marks_grid(&p, &[&".".repeat(20)[..]; 5])
    );
    apply(
        &mut p,
        json!({ "op": "paint_cells", "level": "crevasse", "layer": "marks", "cells": [[1, 3, "big"], [4, 3, "!"]] }),
    );
    let level = p.level("crevasse").unwrap();
    let eff = p.derived_effective(level, &def, Some(&computed));
    assert_eq!(
        eff,
        marks_grid(
            &p,
            &[
                "....................",
                "....................",
                "....................",
                ".B........p.........",
                "....................",
            ],
        )
    );
    assert_eq!(
        eff,
        level.overrides("marks").unwrap().effective(Some(&computed))
    );
}

#[test]
fn derived_layers_export_their_effective_grid() {
    let mut p = project();
    apply(
        &mut p,
        json!({ "op": "paint_cells", "level": "crevasse", "layer": "marks", "cells": [[1, 3, "big"], [4, 3, "!"]] }),
    );
    let level = p.level("crevasse").unwrap();
    // Computed marks at 4 and 10; the overrides give 1 (big) and drop 4.
    let computed = marks_grid(
        &p,
        &[
            "....................",
            "....................",
            "....................",
            "....p.....p.........",
            "....................",
        ],
    );
    let effective = level.overrides("marks").unwrap().effective(Some(&computed));
    let baked = BakedLevel {
        tiles: BTreeMap::new(),
        derived: BTreeMap::from([(s("marks"), effective)]),
    };
    let v: Value =
        serde_json::from_str(&export::level_json(&p, level, &baked, "..").unwrap()).unwrap();
    assert_eq!(
        v["int_grids"]["marks"],
        json!({ "name": "Marks", "grid_size": 16, "derived": true,
            "legend": { ".": null, "B": "big", "p": "mark" },
            "rows": ["....................", "....................", "....................",
                     ".B........p.........", "...................."] })
    );
    // Non-derived layers carry no `derived` key.
    assert!(v["int_grids"]["collision"].get("derived").is_none());
    let keys: Vec<&String> = v["int_grids"]["marks"]
        .as_object()
        .unwrap()
        .keys()
        .collect();
    assert_eq!(keys.len(), 5);
    // The text keeps `derived` right after `grid_size`.
    let text = export::level_json(&p, level, &baked, "..").unwrap();
    assert!(
        text.contains("\"grid_size\": 16,\n      \"derived\": true,\n      \"legend\""),
        "{text}"
    );
}

#[test]
fn a_missing_derived_grid_is_an_export_error() {
    let p = project();
    let level = p.level("crevasse").unwrap();
    let e = export::level_json(&p, level, &BakedLevel::default(), "..").unwrap_err();
    assert_eq!(
        e.to_string(),
        "levels/crevasse.json: level crevasse: derived layer marks has no baked grid (bake the \
         auto layer that writes it, or pass its effective grid)"
    );
    // `write` refuses before writing any file.
    let tmp = tempfile::tempdir().unwrap();
    let baked = BTreeMap::from([(s("crevasse"), BakedLevel::default())]);
    let out = tmp.path().join("export");
    assert!(export::write(&p, &common::fixture_dir(), &out, &baked, true).is_err());
    assert!(!out.join("world.json").exists());
}

// ---- text view ----

#[test]
fn text_view_shows_derived_layers() {
    let mut p = project();
    apply(
        &mut p,
        json!({ "op": "paint_cells", "level": "crevasse", "layer": "marks", "cells": [[1, 3, "big"], [4, 3, "!"]] }),
    );
    // Without bake results: the stored overrides, said so.
    let t = text::render_level(&p, "crevasse", Some(&["marks"])).unwrap();
    let section = |t: &str| -> String {
        let start = t.find("\nint_grid marks").unwrap();
        let end = t[start + 1..]
            .find("\n\n")
            .map_or(t.len(), |e| start + 1 + e);
        t[start + 1..end].to_string()
    };
    assert_eq!(
        section(&t),
        "int_grid marks (20x5 cells of 16 px) derived by pillars (not baked: overrides only)
legend: . empty, p mark, B big
  0         1
  01234567890123456789
0 ....................
1 ....................
2 ....................
3 .B..!...............
4 ....................
overrides: 2 cells (1 set, 1 suppressed; . = from the rules)"
    );
    // With bake results: the effective grid.
    let computed = marks_grid(
        &p,
        &[
            "....................",
            "....................",
            "....................",
            "....p.....p.........",
            "....................",
        ],
    );
    let effective = p
        .level("crevasse")
        .unwrap()
        .overrides("marks")
        .unwrap()
        .effective(Some(&computed));
    let derived = BTreeMap::from([(s("marks"), effective)]);
    let t = text::render_level_baked(&p, "crevasse", Some(&["marks"]), &derived).unwrap();
    let sec = section(&t);
    assert!(
        sec.starts_with("int_grid marks (20x5 cells of 16 px) derived by pillars\n"),
        "{sec}"
    );
    assert!(sec.contains("\n3 .B........p.........\n"), "{sec}");
    assert!(sec.contains("overrides: 2 cells (1 set, 1 suppressed; . = from the rules)"));
    assert!(
        sec.ends_with("3 .B..!...............\n4 ...................."),
        "{sec}"
    );
    // No overrides: no overrides section; no writer: just `derived`.
    let mut q = project();
    set_mut(&mut q).groups.remove(0);
    let t = text::render_level_baked(&q, "crevasse", Some(&["marks"]), &BTreeMap::new()).unwrap();
    let sec = section(&t);
    assert!(
        sec.starts_with(
            "int_grid marks (20x5 cells of 16 px) derived (not baked: overrides only)\n"
        ),
        "{sec}"
    );
    assert!(!sec.contains("overrides:"), "{sec}");
    // The other layers are as before.
    assert_eq!(
        text::render_level(&q, "crevasse", Some(&["collision"])).unwrap(),
        text::render_level_baked(&q, "crevasse", Some(&["collision"]), &derived).unwrap()
    );
}

// ---- schemas ----

#[test]
fn schemas_take_the_m6_keys() {
    let rules = jsonschema::validator_for(&schema::for_kind(format::FileKind::Rules)).unwrap();
    let p = project();
    let file: Value = serde_json::from_str(&rendered(&p, "rules/pillars.json")).unwrap();
    let errors: Vec<String> = rules.iter_errors(&file).map(|e| e.to_string()).collect();
    assert!(errors.is_empty(), "{errors:?}");
    let doc = |rule: Value| json!({ "$schema": "x", "format": 1, "layer": "x", "groups": [{ "slug": "g", "rules": [rule] }] });
    for bad in [
        json!({ "slug": "r", "region": { "cells": "." }, "place": { "along": "floor" }, "chance": 0.5,
                "write": { "grid": "m", "value": "v" } }),
        json!({ "slug": "r", "match": { "rows": ["#"] }, "out": { "tileset": "t", "tiles": [[0, 0]] },
                "write": { "grid": "m", "value": "v" } }),
        json!({ "slug": "r", "match": { "rows": ["#"] }, "place": { "along": "floor" },
                "out": { "tileset": "t", "tiles": [[0, 0]] } }),
        json!({ "slug": "r", "region": { "cells": "." }, "place": { "along": "floor", "across": "interior" },
                "write": { "grid": "m", "value": "v" } }),
    ] {
        assert!(!rules.is_valid(&doc(bad.clone())), "{bad}");
    }
    let intgrid = jsonschema::validator_for(&schema::for_kind(format::FileKind::IntGrid)).unwrap();
    let f = json!({ "$schema": "x", "format": 1, "layer": "marks", "overrides": true, "rows": ["p!."] });
    assert!(intgrid.is_valid(&f));
    let mut no = f.clone();
    no["overrides"] = json!(false);
    assert!(!intgrid.is_valid(&no));
    // A derived layer's file and its layers.json entry validate.
    let (_tmp, dir, mut p) = saved();
    apply(
        &mut p,
        json!({ "op": "paint_cells", "level": "crevasse", "layer": "marks", "cells": [[1, 1, "!"]] }),
    );
    save_dirty(&mut p, &dir).unwrap();
    for (kind, rel) in [
        (format::FileKind::IntGrid, format!("{CREVASSE}/marks.json")),
        (format::FileKind::Layers, "layers.json".to_string()),
        (format::FileKind::Rules, "rules/pillars.json".to_string()),
    ] {
        let v: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join(&rel)).unwrap()).unwrap();
        let validator = jsonschema::validator_for(&schema::for_kind(kind)).unwrap();
        let errors: Vec<String> = validator.iter_errors(&v).map(|e| e.to_string()).collect();
        assert!(errors.is_empty(), "{rel}: {errors:?}");
    }
    let _ = IntGridFile::from_overrides; // the file form is public
}

/// What docs/commands.md says about its derived-layer example.
#[test]
fn commands_md_derived_example_does_what_it_says() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("p.worlds");
    let mut p = common::fixture();
    save(&mut p, &dir).unwrap();
    apply(
        &mut p,
        json!({ "op": "set_layer_def", "layer": "marks",
            "def": { "slug": "marks", "name": "Marks", "kind": "int_grid", "derived": true,
                     "values": [{ "slug": "pillar_base", "name": "Pillar base", "color": "#e0a040", "glyph": "p" }] } }),
    );
    let a = apply(
        &mut p,
        json!({ "op": "paint_cells", "level": "crevasse", "layer": "marks",
            "cells": [[4, 3, "pillar_base"], [10, 3, "!"]] }),
    );
    save_dirty(&mut p, &dir).unwrap();
    assert_eq!(marks_rows(&tree(&dir))[3], "....p.....!.........");
    assert_eq!(
        serde_json::to_value(&a.inverse).unwrap(),
        json!({ "op": "paint_cells", "level": "crevasse", "layer": "marks",
            "cells": [[4, 3, null], [10, 3, null]] })
    );
    let b = apply(
        &mut p,
        json!({ "op": "paint_cells", "level": "crevasse", "layer": "marks", "cells": [[10, 3, null]] }),
    );
    assert_eq!(
        serde_json::to_value(&b.inverse).unwrap(),
        json!({ "op": "paint_cells", "level": "crevasse", "layer": "marks", "cells": [[10, 3, "!"]] })
    );
    p.apply(b.inverse).unwrap();
    p.apply(a.inverse).unwrap();
    save_dirty(&mut p, &dir).unwrap();
    assert!(!dir.join(CREVASSE).join("marks.json").exists());
}
