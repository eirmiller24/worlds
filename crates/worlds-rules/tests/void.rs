//! Non-rectangular levels: void cells read as out of bounds.

mod common;

use common::*;
use worlds_core::command::{Command, Rect};
use worlds_core::{CellMatch, IntGridLayer, OutOfBounds, Project, TileAlt, slug};

fn one(id: &str, rows: &[&str]) -> worlds_core::Rule {
    rule(id, rows, vec![TileAlt::single(1, 1)])
}

fn void(l: &mut worlds_core::Level, r: &[[u32; 4]]) {
    l.void = r.iter().map(|&r| Rect::from(r)).collect();
}

#[test]
fn a_pattern_cell_over_the_void_is_out_of_bounds() {
    let mut p = project();
    // Empty anchor with an empty cell on its right: never fires against
    // the edge, so never against the void either.
    let plain = one("plain", &["*.."]);
    // Empty anchor with solid on its right, out of bounds reading solid:
    // fires against the edge and against the void.
    let mut walled = one("walled", &["*.#"]);
    walled.as_match_mut().unwrap().out_of_bounds = Some(OutOfBounds::Value(slug("solid")));
    set_groups(&mut p, vec![group("g", vec![plain, walled])]);
    let mut l = level(&p, &["....", "...."]);
    let before = bake(&p, &l);
    assert_eq!(
        fired_at(&before, "plain"),
        [[0, 0], [0, 1], [1, 0], [1, 1], [2, 0], [2, 1]]
    );
    assert_eq!(fired_at(&before, "walled"), [[3, 0], [3, 1]]);
    // Cut out the top-right cell: an L.
    void(&mut l, &[[3, 0, 1, 1]]);
    let b = bake(&p, &l);
    assert_eq!(
        fired_at(&b, "plain"),
        [[0, 0], [0, 1], [1, 0], [1, 1], [2, 1]]
    );
    // [2, 0] now has the void on its right; [3, 0] is void, never an anchor.
    assert_eq!(fired_at(&b, "walled"), [[2, 0], [3, 1]]);
}

#[test]
fn walls_close_around_the_inside_corner_of_an_l() {
    // A wall-top rule: a solid cell with nothing solid above it, out of
    // bounds reading empty. The wall under the notch has the void above
    // it, so the wall-top tile runs along it as it runs along the box's
    // top, and around the L's inside corner.
    let mut p = project();
    let mut top = one("top", &[".", "#", "*"]);
    top.as_match_mut().unwrap().out_of_bounds = Some(OutOfBounds::Empty);
    set_groups(&mut p, vec![group("g", vec![top])]);
    // The cells the void cuts out still hold walls here (validation would
    // report them): the void reads as out of bounds whatever it holds.
    let mut l = level(&p, &["######", "#.####", "#.####", "#....#", "######"]);
    let sorted = |b: &worlds_rules::LayerBake| {
        let mut got = fired_at(b, "top");
        got.sort_by_key(|c| (c[1], c[0]));
        got
    };
    let floor = [[1, 4], [2, 4], [3, 4], [4, 4]];
    let box_top = [[0, 0], [1, 0], [2, 0], [3, 0], [4, 0], [5, 0]];
    assert_eq!(sorted(&bake(&p, &l)), [&box_top[..], &floor[..]].concat());
    void(&mut l, &[[3, 0, 3, 2]]);
    // The top runs along the box's top, then along the wall under the
    // notch (row 2, x 3..5).
    let notch = [[0, 0], [1, 0], [2, 0], [3, 2], [4, 2], [5, 2]];
    assert_eq!(sorted(&bake(&p, &l)), [&notch[..], &floor[..]].concat());
}

#[test]
fn regions_end_at_the_void() {
    let p = project();
    let def = p.layer("collision").unwrap();
    // One open room 5x2 inside walls; void cuts its two right columns out
    // of the bottom row.
    let g = IntGridLayer::from_rows(def, &["#######", "#.....#", "#.....#", "#######"]).unwrap();
    let mut l = level(&p, &["#######", "#.....#", "#.....#", "#######"]);
    void(&mut l, &[[4, 2, 3, 2]]);
    let mask = p.layer_void(def, &l).unwrap();
    let r = worlds_rules::regions_in(&p, "collision", &g, &CellMatch::Empty, Some(&mask)).unwrap();
    assert_eq!(r.regions.len(), 1);
    assert_eq!(r.regions[0].area, 8);
    // The floor (cells with a wall below) stops where the void starts; the
    // cells above the void aren't floor (the void is the border, not a wall).
    assert_eq!(r.regions[0].edges.floor, vec![[1, 2, 3]]);
    // Without the void it is one run under the whole room.
    let r = worlds_rules::regions(&p, "collision", &g, &CellMatch::Empty).unwrap();
    assert_eq!(r.regions[0].edges.floor, vec![[1, 2, 5]]);
}

#[test]
fn the_cache_rebakes_a_level_whose_void_changes() {
    let mut p: Project = project();
    let mut walled = one("walled", &["*.#"]);
    walled.as_match_mut().unwrap().out_of_bounds = Some(OutOfBounds::Value(slug("solid")));
    set_groups(&mut p, vec![group("g", vec![walled])]);
    let l = level(&p, &["....", "...."]);
    p.worlds[0].levels.push(l);
    let images = images();
    let mut cache = worlds_rules::BakeCache::new();
    cache.bake_level(&p, "room", &images).unwrap();
    let a = p
        .apply(Command::SetLevelVoid {
            level: slug("room"),
            void: vec![Rect::new(3, 0, 1, 1)],
            restore: false,
        })
        .unwrap();
    cache
        .apply_changes(&p, "room", &images, &a.changes)
        .unwrap();
    let fresh = bake(&p, p.level("room").unwrap());
    assert_eq!(cache.get("room", "auto").unwrap(), &fresh);
    assert_eq!(fired_at(&fresh, "walled"), [[2, 0], [3, 1]]);
}
