#![allow(dead_code)]

//! Tiny hand-built projects for the rule engine tests.
//!
//! - `collision` (int_grid): `#` solid and `f` fortified (both in group
//!   `walls`), `~` water (ungrouped).
//! - `auto` (auto, source collision).
//! - tilesets `main` and `alt`, 16 px tiles on a 16 px grid. In-memory
//!   opacity: tile `[0, 0]`, `[0, 1]` and `[4, 0]` are opaque, the rest not.

use worlds_core::{
    AutoDef, Color, IntGridDef, IntGridGroup, IntGridLayer, IntGridValue, LayerDef, LayerKind,
    Level, Pattern, Project, Rule, RuleGroup, RuleOutput, RuleSet, Stamp, TileAlt, TilesetDef,
    slug,
};
use worlds_rules::{LayerBake, Placement, TileOpacity, TilesetImages};

pub fn project() -> Project {
    let mut p = Project::new("test");
    p.seed = 1234;
    p.grid_size = 16;
    let value = |s: &str, g: char, group: Option<&str>| IntGridValue {
        slug: slug(s),
        name: s.into(),
        color: Color::new("#808080").unwrap(),
        glyph: g,
        group: group.map(slug),
    };
    let col = LayerDef::new(
        slug("collision"),
        "Collision",
        LayerKind::IntGrid(IntGridDef {
            values: vec![
                value("solid", '#', Some("walls")),
                value("fortified", 'f', Some("walls")),
                value("water", '~', None),
            ],
            groups: vec![IntGridGroup {
                slug: slug("walls"),
                name: "Walls".into(),
                color: None,
                glyph: None,
            }],
            derived: false,
        }),
    );
    let auto = LayerDef::new(
        slug("auto"),
        "Auto",
        LayerKind::Auto(AutoDef::new(slug("collision"))),
    );
    p.layers = vec![col, auto];
    let ts = |s: &str| TilesetDef {
        slug: slug(s),
        image: format!("{s}.png"),
        tile_size: [16, 16],
        spacing: 0,
        padding: 0,
    };
    p.tilesets = vec![ts("main"), ts("alt")];
    p.rules = vec![RuleSet {
        layer: slug("auto"),
        stamps: vec![Stamp {
            slug: slug("pillar"),
            tileset: slug("main"),
            rect: [4, 0, 2, 2],
            pivot: [0.5, 1.0],
            occupies: false,
        }],
        groups: vec![],
    }];
    p
}

/// A level whose collision grid is `rows` (glyphs as in the layer def).
pub fn level(p: &Project, rows: &[&str]) -> Level {
    let def = p.layer("collision").unwrap();
    let g = IntGridLayer::from_rows(def, rows).unwrap();
    let mut l = Level::new(slug("room"), "Room", g.size());
    l.int_grids = vec![g];
    l
}

pub fn images() -> TilesetImages {
    let imgs = TilesetImages::in_memory();
    for ts in ["main", "alt"] {
        let mut flags = vec![false; 64];
        flags[0] = true; // [0, 0]
        flags[8] = true; // [0, 1]
        flags[4] = true; // [4, 0]
        imgs.insert(slug(ts), TileOpacity::from_flags(8, 8, flags));
    }
    imgs
}

/// A rule reading the auto layer's source (`collision`), drawing from
/// `main`. `#`, `f` and `~` mean their values without a legend.
pub fn rule(slug_: &str, rows: &[&str], tiles: Vec<TileAlt>) -> Rule {
    Rule::new(
        slug(slug_),
        Pattern::new(rows),
        RuleOutput::tiles(slug("main"), tiles),
    )
}

pub fn legend(mut r: Rule, glyph: char, test: &str) -> Rule {
    let worlds_core::Legend::Inline(l) = &mut r.as_match_mut().unwrap().pattern.legend else {
        panic!("named legend");
    };
    l.insert(glyph, serde_json::from_value(test.into()).unwrap());
    r
}

pub fn group(slug_: &str, rules: Vec<Rule>) -> RuleGroup {
    RuleGroup::new(slug(slug_), rules)
}

pub fn tiles(r: &mut Rule) -> &mut worlds_core::TilesOutput {
    r.out_mut().unwrap().as_tiles_mut().unwrap()
}

pub fn set_groups(p: &mut Project, groups: Vec<RuleGroup>) {
    p.rules[0].groups = groups;
}

pub fn bake(p: &Project, l: &Level) -> LayerBake {
    worlds_rules::bake_layer(p, l, &slug("auto"), &images()).unwrap_or_else(|e| panic!("{e}"))
}

/// Cells where `rule` fired, sorted by (x, y).
pub fn fired_at(b: &LayerBake, rule: &str) -> Vec<[i32; 2]> {
    b.fired
        .iter()
        .filter(|(_, rs)| rs.iter().any(|r| r == rule))
        .map(|(c, _)| *c)
        .collect()
}

pub fn by_rule<'a>(b: &'a LayerBake, rule: &str) -> Vec<&'a Placement> {
    b.placements.iter().filter(|p| p.rule == rule).collect()
}
