//! M3 rule format additions: `@`, `[a|b]`, `require`, named patterns and
//! legends, templates and `use` references, the built-in `blob47`, and the
//! transpose flips.

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use worlds_core::*;

const MACROS: &str = r##"{
  "$schema": "../schema/macros.json",
  "format": 1,
  "patterns": { "lip_above": ["***", "*S*", "*#*"] },
  "legends": { "walls": { "#": "solid", "S": "!solid" } },
  "templates": {
    "frame": {
      "name": "Frame",
      "doc": "Lip, fill and a fixed trim.",
      "legend": "walls",
      "rules": [
        {
          "slug": "lip",
          "match": { "rows": "lip_above" },
          "out": { "tiles": [[1, 0], [2, 0, 2, 1]] },
          "break": true
        },
        {
          "slug": "fill",
          "match": {
            "legend": { "x": "[solid|fortified]" },
            "rows": [
              "*x*",
              "x#x",
              "*x*"
            ]
          },
          "out": { "tiles": [[1, 1]] },
          "break": true
        },
        {
          "slug": "trim",
          "match": {
            "legend": { "@": "!solid" },
            "rows": [
              "@",
              "#"
            ]
          },
          "require": {
            "grid": "collision",
            "legend": "walls",
            "rows": [
              "#"
            ]
          },
          "symmetry": ["mirror_x", "rotate"],
          "out": { "tileset": "ssw_set5", "tiles": [[9, 9]] }
        }
      ]
    }
  }
}
"##;

const RULES: &str = r##"{
  "$schema": "../schema/rules.json",
  "format": 1,
  "layer": "main_tiling",
  "groups": [
    {
      "slug": "walls",
      "rules": [
        {
          "slug": "named",
          "match": { "legend": "walls", "rows": "lip_above" },
          "out": { "tileset": "castle_main", "tiles": [[0, 0]] }
        },
        {
          "slug": "anchored",
          "match": {
            "legend": { "@": "![solid|oneway|.]" },
            "rows": [
              "**",
              "@#"
            ]
          },
          "require": {
            "rows": [
              "+"
            ]
          },
          "out": { "tileset": "castle_main", "tiles": [[0, 1]] }
        },
        {
          "use": "frame",
          "slug": "walls",
          "tileset": "castle_main",
          "origin": [4, 2],
          "tiles": { "fill": [[0, 0, 2, 2]], "trim": [[7, 7]] },
          "skip": ["lip"],
          "legend": { "#": "fortified" },
          "active": false
        },
        { "use": "frame", "tileset": "castle_main" },
        { "use": "blob47", "slug": "moss", "tileset": "ssw_set5", "origin": [0, 8] }
      ]
    },
    { "slug": "flooded_decor", "mode": "optional", "rules": [] },
    { "slug": "pillars", "mode": "optional", "rules": [] }
  ]
}
"##;

/// The mini fixture with `rules/main_tiling.json` and `rules/macros.json`
/// replaced, in a temp dir.
fn with_rules(rules: &str, macros: &str) -> (tempfile::TempDir, Result<Project, Error>) {
    let tmp = tempfile::tempdir().unwrap();
    save(&mut common::fixture(), tmp.path()).unwrap();
    fs::write(tmp.path().join("rules/main_tiling.json"), rules).unwrap();
    fs::write(tmp.path().join("rules/macros.json"), macros).unwrap();
    let p = load(tmp.path());
    (tmp, p)
}

fn project() -> Project {
    with_rules(RULES, MACROS)
        .1
        .unwrap_or_else(|e| panic!("{e}"))
}

fn rendered(p: &Project, rel: &str) -> String {
    String::from_utf8(format::render_file(p, Path::new(rel)).unwrap().unwrap()).unwrap()
}

fn s(x: &str) -> Slug {
    slug(x)
}

#[test]
fn every_new_key_round_trips_byte_identically() {
    let p = project();
    assert_eq!(rendered(&p, "rules/main_tiling.json"), RULES);
    assert_eq!(rendered(&p, "rules/macros.json"), MACROS);
    assert!(validate(&p).is_empty(), "{:#?}", validate(&p));
}

#[test]
fn programmatic_entries_save_in_the_shortest_form() {
    let mut p = common::fixture();
    let mut u = TemplateRef::new(s("blob47"));
    u.tileset = Some(s("ssw_set5"));
    let mut r = Rule::new(
        s("edge"),
        Pattern::new(&["@", "#"]),
        RuleOutput::tiles(s("castle_main"), vec![TileAlt::single(1, 2)]),
    );
    r.as_match_mut().unwrap().require = vec![Pattern {
        grid: None,
        legend: Legend::Named(s("walls")),
        rows: Rows::Named(s("edge_top")),
    }];
    p.rules[0].groups = vec![RuleGroup::from_entries(
        s("g"),
        vec![RuleEntry::Use(u), RuleEntry::Rule(r)],
    )];
    let text = rendered(&p, "rules/main_tiling.json");
    assert!(
        text.contains(r#"{ "use": "blob47", "tileset": "ssw_set5" }"#),
        "{text}"
    );
    assert!(
        text.contains(r#""require": { "legend": "walls", "rows": "edge_top" }"#),
        "{text}"
    );
    let mut m = Macros::default();
    m.templates.insert(
        s("t"),
        Template {
            name: None,
            doc: None,
            legend: Legend::default(),
            rules: vec![
                TemplateRule::try_from(Rule::new(
                    s("a"),
                    Pattern::new(&["#"]),
                    RuleOutput::tiles(s("castle_main"), vec![TileAlt::single(0, 0)]),
                ))
                .unwrap(),
            ],
        },
    );
    p.macros = Some(m);
    let text = rendered(&p, "rules/macros.json");
    assert!(
        text.contains(r#""out": { "tileset": "castle_main", "tiles": [[0, 0]] }"#),
        "{text}"
    );
    // A template rule without its own tileset leaves the key out.
    let t: TemplateRule = serde_json::from_str(
        r##"{ "slug": "a", "match": { "rows": ["#"] }, "out": { "tiles": [[0, 0]] } }"##,
    )
    .unwrap();
    assert!(t.needs_tileset() && !t.is_fixed());
    assert_eq!(
        serde_json::to_string(&t.out).unwrap(),
        r#"{"tiles":[[0,0]]}"#
    );
}

#[test]
fn flip_bits_round_trip_and_compose() {
    for b in 1..=7u8 {
        let f = Flip::from_bits(b).unwrap();
        assert_eq!(f.bits(), b);
        let json = serde_json::to_string(&f).unwrap();
        assert_eq!(json, format!("\"{}\"", f.as_str()));
        assert_eq!(serde_json::from_str::<Flip>(&json).unwrap(), f);
    }
    assert_eq!(Flip::from_bits(0), None);
    assert_eq!(Flip::from_bits(8), None);
    // Transpose first, then x: a quarter turn clockwise is 5.
    let cw = |f| Flip::mirror_x(Flip::transpose(f));
    assert_eq!(cw(None), Some(Flip::XT));
    assert_eq!(cw(cw(None)), Some(Flip::XY));
    assert_eq!(cw(cw(cw(None))), Some(Flip::YT));
    assert_eq!(cw(cw(cw(cw(None)))), None);
    assert_eq!(
        Flip::transpose(Flip::transpose(Some(Flip::X))),
        Some(Flip::X)
    );
    assert_eq!(Flip::mirror_y(Some(Flip::XY)), Some(Flip::X));
}

#[test]
fn transposed_tiles_load_save_and_export() {
    let mut p = common::fixture();
    let l = &mut p.worlds[0].levels[0];
    let t = &mut l.tiles[0].tiles[0];
    t.flip = Some(Flip::XT);
    let text = rendered(&p, "worlds/main/levels/crevasse/decor_back.json");
    assert!(text.contains(r#""flip": "xt""#), "{text}");
    let view = text::render_level(&p, "crevasse", None).unwrap();
    assert!(view.contains(" flip xt"), "{view}");
    let e: Result<TilePlacement, _> =
        serde_json::from_str(r#"{ "cell": [0, 0], "tileset": "a", "tile": [0, 0], "flip": "tx" }"#);
    assert!(e.is_err());
}

#[test]
fn cell_tests_parse_alternatives() {
    let m: CellMatch = serde_json::from_str(r#""![solid|oneway|.]""#).unwrap();
    assert_eq!(
        m,
        CellMatch::NotOneOf(vec![
            CellTest::Is(s("solid")),
            CellTest::Is(s("oneway")),
            CellTest::Empty
        ])
    );
    assert_eq!(m.to_string(), "![solid|oneway|.]");
    let m: CellMatch = serde_json::from_str(r#""[+|walls]""#).unwrap();
    assert_eq!(
        m,
        CellMatch::OneOf(vec![CellTest::NonEmpty, CellTest::Is(s("walls"))])
    );
    for bad in [
        "[solid|!x]",
        "[solid",
        "solid]",
        "[]",
        "[a||b]",
        "[*]",
        "[[a]]",
        "!!a",
    ] {
        let e = serde_json::from_str::<CellMatch>(&format!("\"{bad}\"")).unwrap_err();
        assert!(e.to_string().contains("invalid cell test"), "{bad}: {e}");
    }
}

fn err(rules: &str) -> String {
    with_rules(rules, MACROS)
        .1
        .expect_err("load should fail")
        .to_string()
}

#[test]
fn load_errors_name_the_exact_field() {
    // A typo inside a reference.
    let e = err(&RULES.replace("\"origin\": [4, 2]", "\"orgin\": [4, 2]"));
    assert!(
        e.contains("rules/main_tiling.json: at groups[0].rules[2].orgin: unknown field `orgin`"),
        "{e}"
    );
    // A typo inside a rule, and deep inside one.
    let e = err(&RULES.replace("\"require\": {", "\"requires\": {"));
    assert!(
        e.contains("at groups[0].rules[1].requires: unknown field `requires`"),
        "{e}"
    );
    let e = err(&RULES.replace(
        "\"rows\": [\n              \"+\"",
        "\"rowz\": [\n              \"+\"",
    ));
    assert!(
        e.contains("at groups[0].rules[1].require.rowz: unknown field `rowz`"),
        "{e}"
    );
    // A key of the other kind.
    let e = err(&RULES.replace(
        "\"slug\": \"named\",",
        "\"slug\": \"named\", \"origin\": [0, 0],",
    ));
    assert!(
        e.contains("at groups[0].rules[0]: \"origin\" is not allowed on a rule"),
        "{e}"
    );
    let e = err(&RULES.replace(
        "{ \"use\": \"frame\", \"tileset\": \"castle_main\" }",
        "{ \"use\": \"frame\", \"chance\": 0.5 }",
    ));
    assert!(
        e.contains("at groups[0].rules[3]: \"chance\" is not allowed on a template reference"),
        "{e}"
    );
    // A rule missing `out` still says so.
    let e = err(&RULES.replace(
        ",\n          \"out\": { \"tileset\": \"castle_main\", \"tiles\": [[0, 0]] }",
        "",
    ));
    assert!(
        e.contains("at groups[0].rules[0]: missing field `out`"),
        "{e}"
    );
    // A bad alternative list.
    let e = err(&RULES.replace("![solid|oneway|.]", "![solid|!oneway]"));
    assert!(
        e.contains("at groups[0].rules[1].match.legend.@: invalid cell test"),
        "{e}"
    );
    // A named row that isn't a slug.
    let e = err(&RULES.replace("\"rows\": \"lip_above\"", "\"rows\": \"Lip\""));
    assert!(e.contains("at groups[0].rules[0].match.rows"), "{e}");
    // A typo in a template rule.
    let e = with_rules(
        RULES,
        &MACROS.replace(
            "\"break\": true\n        },\n        {\n          \"slug\": \"fill\"",
            "\"brake\": true\n        },\n        {\n          \"slug\": \"fill\"",
        ),
    )
    .1
    .unwrap_err()
    .to_string();
    assert!(
        e.contains("rules/macros.json: at templates.frame.rules[0].brake: unknown field `brake`"),
        "{e}"
    );
}

fn expanded(p: &Project) -> Vec<(String, Option<String>, usize, Rule)> {
    p.expanded_rules("main_tiling")
        .unwrap()
        .into_iter()
        .flat_map(|g| g.rules)
        .map(|r| {
            (
                r.rule.slug.to_string(),
                r.from.as_ref().map(|s| s.to_string()),
                r.entry,
                r.rule.into_owned(),
            )
        })
        .collect()
}

fn tiles(r: &Rule) -> (&str, Vec<Vec<[u32; 2]>>) {
    let t = r.out().unwrap().as_tiles().unwrap();
    (
        t.tileset.as_str(),
        t.tiles.iter().map(|a| a.tiles.clone()).collect(),
    )
}

#[test]
fn expansion_resolves_names_and_applies_references() {
    let p = project();
    let x = expanded(&p);
    let slugs: Vec<&str> = x.iter().map(|r| r.0.as_str()).collect();
    assert_eq!(
        &slugs[..7],
        [
            "named",
            "anchored",
            "walls__fill",
            "walls__trim",
            "frame__lip",
            "frame__fill",
            "frame__trim"
        ]
    );
    assert_eq!(slugs.len(), 7 + 47);
    assert!(slugs[7..].iter().all(|s| s.starts_with("moss__m")));
    // `from` and the entry index.
    assert_eq!(x[0].1, None);
    assert_eq!((x[2].1.as_deref(), x[2].2), (Some("walls"), 2));
    assert_eq!((x[4].1.as_deref(), x[4].2), (Some("frame"), 3));
    assert_eq!(x[7].1.as_deref(), Some("moss"));

    // Inline rule: named rows and legend resolved.
    let named = x[0].3.as_match().unwrap();
    assert_eq!(named.pattern.inline_rows(), ["***", "*S*", "*#*"]);
    assert_eq!(
        named.pattern.inline_legend().unwrap()[&'S'],
        CellMatch::Not(s("solid"))
    );

    // Override relative to origin, legend: template < rule < reference.
    let fill = &x[2].3;
    assert_eq!(
        tiles(fill),
        ("castle_main", vec![vec![[4, 2], [5, 2], [4, 3], [5, 3]]])
    );
    let l = fill.as_match().unwrap().pattern.inline_legend().unwrap();
    assert_eq!(l[&'#'], CellMatch::Is(s("fortified")), "reference wins");
    assert_eq!(l[&'S'], CellMatch::Not(s("solid")), "template legend");
    assert!(matches!(l[&'x'], CellMatch::OneOf(_)), "rule legend");
    assert!(!fill.active, "inactive reference");

    // A fixed rule keeps its tileset; its override is absolute.
    let trim = &x[3].3;
    assert_eq!(tiles(trim), ("ssw_set5", vec![vec![[7, 7]]]));
    let tm = trim.as_match().unwrap();
    assert_eq!(tm.symmetry, [Symmetry::MirrorX, Symmetry::Rotate]);
    let req = &tm.require[0];
    assert_eq!(req.grid.as_ref().unwrap(), "collision");
    // The reference's legend applies to `require` too.
    assert_eq!(
        req.inline_legend().unwrap()[&'#'],
        CellMatch::Is(s("fortified"))
    );
    assert_eq!(tm.pattern.anchor(), (0, 0));
    let plain_trim = &x[6].3;
    assert_eq!(tiles(plain_trim), ("ssw_set5", vec![vec![[9, 9]]]));
    assert_eq!(
        plain_trim.as_match().unwrap().require[0]
            .inline_legend()
            .unwrap()[&'#'],
        CellMatch::Is(s("solid"))
    );

    // The plain reference: default slug, origin [0, 0], own tiles.
    assert_eq!(
        tiles(&x[4].3),
        ("castle_main", vec![vec![[1, 0]], vec![[2, 0], [3, 0]]])
    );
    assert!(x[4].3.active && x[4].3.break_on_match);

    // blob47 with origin: the full piece sits at (9, 2) + origin.
    let full = x.iter().find(|r| r.0 == "moss__m255").unwrap();
    assert_eq!(tiles(&full.3), ("ssw_set5", vec![vec![[9, 10]]]));
}

#[test]
fn inline_rules_without_names_are_borrowed() {
    let p = common::fixture();
    let groups = p.expanded_rules("main_tiling").unwrap();
    assert_eq!(groups.len(), p.rules[0].groups.len());
    for g in &groups {
        for r in &g.rules {
            assert!(matches!(r.rule, std::borrow::Cow::Borrowed(_)));
            assert_eq!(r.from, None);
        }
    }
    assert!(p.expanded_rules("nothing").unwrap().is_empty());
    assert!(!p.rules[0].uses_macros());
    assert!(project().rules[0].uses_macros());
}

#[test]
fn expansion_errors_name_what_is_missing() {
    let (_t, p) = with_rules(
        &RULES.replace(
            "\"use\": \"frame\", \"tileset\"",
            "\"use\": \"framez\", \"tileset\"",
        ),
        MACROS,
    );
    let e = p.unwrap().expanded_rules("main_tiling").unwrap_err();
    assert_eq!(
        e,
        ExpandError::UnknownTemplate {
            reference: s("framez"),
            template: s("framez")
        }
    );
    let (_t, p) = with_rules(
        &RULES.replace("\"rows\": \"lip_above\"", "\"rows\": \"lip\""),
        MACROS,
    );
    assert_eq!(
        p.unwrap().expanded_rules("main_tiling").unwrap_err(),
        ExpandError::UnknownPattern {
            rule: s("named"),
            name: s("lip")
        }
    );
    let (_t, p) = with_rules(
        &RULES.replace(
            "{ \"use\": \"frame\", \"tileset\": \"castle_main\" }",
            "{ \"use\": \"frame\" }",
        ),
        MACROS,
    );
    let p = p.unwrap();
    assert_eq!(
        p.expanded_rules("main_tiling").unwrap_err(),
        ExpandError::NoTileset {
            reference: s("frame"),
            rule: s("lip")
        }
    );
    // A validation problem, not a load error, at the reference's tileset.
    let problems = validate(&p);
    assert!(
        problems
            .iter()
            .any(|x| x.path == "groups[0].rules[3].tileset"
                && x.message.contains("names no \"tileset\"")),
        "{problems:#?}"
    );
}

#[test]
fn blob47_is_47_valid_rules() {
    let t = &Macros::builtin_templates()[&s("blob47")];
    assert_eq!(t.rules.len(), 47);
    let mut slugs: Vec<&str> = t.rules.iter().map(|r| r.slug.as_str()).collect();
    slugs.sort();
    slugs.dedup();
    assert_eq!(slugs.len(), 47);
    let mut cells = std::collections::BTreeSet::new();
    for r in &t.rules {
        let [w, h] = r.pattern.size();
        assert_eq!((w, h), (3, 3), "{}", r.slug);
        assert_eq!(r.pattern.anchor_count(), 0);
        assert!(r.needs_tileset());
        let TemplateOutput::Tiles(o) = &r.out else {
            panic!()
        };
        let [c, row] = o.tiles[0].tiles[0];
        assert!(c < 12 && row < 4);
        assert!(cells.insert([c, row]), "two pieces on one cell");
        // The slug is the neighbour mask the pattern requires.
        let rows = r.pattern.inline_rows();
        let at = |x: usize, y: usize| rows[y].as_bytes()[x];
        let bits = [
            (1, 0, 1),
            (2, 0, 2),
            (2, 1, 4),
            (2, 2, 8),
            (1, 2, 16),
            (0, 2, 32),
            (0, 1, 64),
            (0, 0, 128),
        ];
        let mask: u32 = bits
            .iter()
            .filter(|(x, y, _)| at(*x, *y) == b'#')
            .map(|(_, _, b)| b)
            .sum();
        assert_eq!(r.slug.as_str(), format!("m{mask}"));
    }
    assert!(!cells.contains(&[10, 1]), "the unused cell");
    // Every 3x3 neighbourhood matches exactly one piece.
    for n in 0..256u32 {
        let filled = |x: usize, y: usize| {
            let bit = match (x, y) {
                (1, 1) => return true,
                (1, 0) => 1,
                (2, 0) => 2,
                (2, 1) => 4,
                (2, 2) => 8,
                (1, 2) => 16,
                (0, 2) => 32,
                (0, 1) => 64,
                _ => 128,
            };
            n & bit != 0
        };
        let hits = t
            .rules
            .iter()
            .filter(|r| {
                let rows = r.pattern.inline_rows();
                (0..3).all(|y| {
                    (0..3).all(|x| match rows[y].as_bytes()[x] {
                        b'#' => filled(x, y),
                        b'S' => !filled(x, y),
                        _ => true,
                    })
                })
            })
            .count();
        assert_eq!(hits, 1, "neighbourhood {n}");
    }
    // A project using it validates cleanly.
    let p = project();
    assert!(validate(&p).is_empty(), "{:#?}", validate(&p));
}

fn problems(rules: &str, macros: &str) -> Vec<String> {
    let (_t, p) = with_rules(rules, macros);
    validate(&p.unwrap_or_else(|e| panic!("{e}")))
        .iter()
        .map(|p| p.to_string())
        .collect()
}

fn expect(rules: &str, macros: &str, want: &str) {
    let got = problems(rules, macros);
    assert!(
        got.iter().any(|p| p.contains(want)),
        "want {want:?} in {got:#?}"
    );
}

#[test]
fn validation_of_the_new_constructs() {
    let f = "rules/main_tiling.json: ";
    // Two @, even sizes without @.
    expect(
        &RULES.replace("\"@#\"", "\"@@\""),
        MACROS,
        &format!("{f}groups[0].rules[1].match.rows: pattern has 2 @ glyphs"),
    );
    expect(
        &RULES.replace("\"@#\"", "\"*#\""),
        MACROS,
        "groups[0].rules[1].match.rows: pattern is 2x2; without an @ anchor, width and height must be odd",
    );
    // @ in a written legend whose rows have none.
    expect(
        &RULES.replace(
            "\"**\",\n              \"@#\"",
            "\"*\",\n              \"#\",\n              \"*\"",
        ),
        MACROS,
        "groups[0].rules[1].match.legend.@: '@' is only a legend key in a pattern whose rows contain @",
    );
    expect(
        RULES,
        &MACROS.replace(
            "\"@\",\n              \"#\"",
            "\"*\",\n              \"#\",\n              \"*\"",
        ),
        "rules/macros.json: templates.frame.rules[2].match.legend.@: '@' is only a legend key",
    );
    // Unknown [a|b] member.
    expect(
        &RULES.replace("![solid|oneway|.]", "![solid|lava]"),
        MACROS,
        "groups[0].rules[1].match.legend.@: \"lava\" is not a value or group of \"collision\"",
    );
    // require on another grid size.
    expect(
        &RULES.replace(
            "\"require\": {\n            \"rows\"",
            "\"require\": {\n            \"grid\": \"decor_back\", \"rows\"",
        ),
        MACROS,
        "groups[0].rules[1].require.grid: \"decor_back\" is not an int_grid layer",
    );
    // Unknown names.
    expect(
        &RULES.replace(
            "\"legend\": \"walls\", \"rows\"",
            "\"legend\": \"wallz\", \"rows\"",
        ),
        MACROS,
        "groups[0].rules[0].match.legend: rule named: no legend \"wallz\"",
    );
    expect(
        &RULES.replace(
            "\"use\": \"frame\", \"tileset\"",
            "\"use\": \"framez\", \"tileset\"",
        ),
        MACROS,
        "groups[0].rules[3].use: reference framez: no template \"framez\"",
    );
    expect(
        RULES,
        &MACROS.replace(
            "\"legend\": \"walls\",\n      \"rules\"",
            "\"legend\": \"wallz\",\n      \"rules\"",
        ),
        "rules/macros.json: templates.frame.legend: no legend \"wallz\" in legends",
    );
    expect(
        RULES,
        &MACROS.replace("\"rows\": \"lip_above\"", "\"rows\": \"lip\""),
        "rules/macros.json: templates.frame.rules[0].match.rows: no pattern \"lip\" in patterns",
    );
    // Unknown override and skip slugs.
    expect(
        &RULES.replace("\"skip\": [\"lip\"]", "\"skip\": [\"lop\"]"),
        MACROS,
        "groups[0].rules[2].skip[0]: template \"frame\" has no rule \"lop\"",
    );
    expect(
        &RULES.replace("\"trim\": [[7, 7]]", "\"trum\": [[7, 7]]"),
        MACROS,
        "groups[0].rules[2].tiles.trum: template \"frame\" has no rule \"trum\"",
    );
    // Shadowing a built-in.
    expect(
        RULES,
        &MACROS.replace("\"frame\": {", "\"blob47\": {"),
        "rules/macros.json: templates.blob47: \"blob47\" is a built-in template",
    );
    // Effective slugs collide: an inline rule named like an expanded one.
    expect(
        &RULES.replace("\"slug\": \"named\"", "\"slug\": \"frame__fill\""),
        MACROS,
        "duplicate rule slug \"frame__fill\"",
    );
    // A second reference to the same template needs its own slug.
    expect(
        &RULES.replace(
            "\"use\": \"frame\",\n          \"slug\": \"walls\",\n",
            "\"use\": \"frame\",\n",
        ),
        MACROS,
        "duplicate rule slug \"frame\"",
    );
    // Template rules are checked against the referencing layer.
    expect(
        RULES,
        &MACROS.replace("\"x\": \"[solid|fortified]\"", "\"x\": \"[solid|lava]\""),
        "groups[0].rules[3].expanded[frame__fill].match.legend.x: \"lava\" is not a value",
    );
}

#[test]
fn rotate_needs_square_tiles() {
    let mut p = project();
    assert!(validate(&p).is_empty());
    let ts = p
        .tilesets
        .iter_mut()
        .find(|t| t.slug == "ssw_set5")
        .unwrap();
    ts.tile_size = [32, 16];
    let got: Vec<String> = validate(&p).iter().map(|x| x.to_string()).collect();
    assert!(
        got.iter().any(|x| x.contains(
            "groups[0].rules[2].expanded[walls__trim].symmetry: rotate needs square tiles, but tileset \"ssw_set5\" has 32x16 tiles"
        )),
        "{got:#?}"
    );
    // A level substituting a non-square tileset for a square one.
    let mut p = project();
    p.tilesets.push(TilesetDef {
        slug: s("tall"),
        image: "tall.png".into(),
        tile_size: [16, 32],
        spacing: 0,
        padding: 0,
    });
    p.worlds[0].levels[0]
        .layers
        .get_mut("main_tiling")
        .unwrap()
        .tilesets
        .insert(s("ssw_set5"), s("tall"));
    let got: Vec<String> = validate(&p).iter().map(|x| x.to_string()).collect();
    assert!(
        got.iter().any(|x| x
            .contains("level \"crevasse\" draws \"ssw_set5\" from tileset \"tall\" (16x32 tiles)")),
        "{got:#?}"
    );
}

#[test]
fn auto_layers_drawing_from_follows_references_and_substitutions() {
    let p = project();
    assert_eq!(
        p.auto_layers_drawing_from("castle_main"),
        [s("main_tiling")]
    );
    assert_eq!(p.auto_layers_drawing_from("ssw_set5"), [s("main_tiling")]);
    assert!(p.auto_layers_drawing_from("nothing").is_empty());
    let mut p = common::fixture();
    p.rules[0].groups.clear();
    p.rules[0].stamps.clear();
    for l in &mut p.worlds[0].levels {
        l.layers.values_mut().for_each(|st| st.tilesets.clear());
    }
    assert!(p.auto_layers_drawing_from("castle_main").is_empty());
    p.worlds[0].levels[0]
        .layers
        .get_mut("main_tiling")
        .unwrap()
        .tilesets = BTreeMap::from([(s("a"), s("castle_main"))]);
    assert_eq!(
        p.auto_layers_drawing_from("castle_main"),
        [s("main_tiling")]
    );
}

#[test]
fn renames_reach_into_references_and_templates() {
    use worlds_core::command::{Change, DefKind};
    let mut p = project();
    // A tileset rename rewrites a reference's tileset and a fixed template rule's.
    let applied = p
        .apply(Command::RenameTileset {
            from: s("ssw_set5"),
            to: s("set5"),
        })
        .unwrap();
    assert!(applied.changes.contains(&Change::Def {
        kind: DefKind::Macros,
        slug: s("macros")
    }));
    let text = rendered(&p, "rules/main_tiling.json");
    assert!(text.contains("\"tileset\": \"set5\""), "{text}");
    assert!(rendered(&p, "rules/macros.json").contains("\"tileset\": \"set5\""));
    assert!(validate(&p).is_empty(), "{:#?}", validate(&p));
    p.apply(applied.inverse).unwrap();
    assert_eq!(rendered(&p, "rules/macros.json"), MACROS);
    assert_eq!(rendered(&p, "rules/main_tiling.json"), RULES);

    // Removing a tileset a reference uses is refused, naming the use.
    let e = p
        .apply(Command::RemoveTilesetDef {
            tileset: s("ssw_set5"),
            keep_uses: false,
        })
        .unwrap_err()
        .to_string();
    assert!(e.contains("template frame"), "{e}");

    // A layer rename rewrites require.grid in templates (a macros change).
    let applied = p
        .apply(Command::RenameLayer {
            from: s("collision"),
            to: s("solidity"),
        })
        .unwrap();
    assert!(applied.changes.contains(&Change::Def {
        kind: DefKind::Macros,
        slug: s("macros")
    }));
    assert!(rendered(&p, "rules/macros.json").contains("\"grid\": \"solidity\""));
    p.apply(applied.inverse).unwrap();
    assert_eq!(rendered(&p, "rules/macros.json"), MACROS);
}

#[test]
fn ldtk_slugs_never_contain_a_double_underscore() {
    for name in [
        "a__b", "a--b", "a  b", "__x__", "A_-_B", "Foo__Bar", "x _ y", "-_-",
    ] {
        let s = worlds_core::ldtk::snake_case(name, "n");
        assert!(!s.contains("__"), "{name:?} -> {s:?}");
        assert!(Slug::new(&s).is_ok(), "{name:?} -> {s:?}");
    }
}
