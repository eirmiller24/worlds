//! Rule-level, macro and stamp commands: exact inverses down to the files
//! on disk, no-ops, every refusal, the rewrites of `rename_macro`, the
//! `in_use` phrases, and random sequences on the mini fixture and on an
//! imported LDtk sample.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use worlds_core::command::{Change, Command, CommandError, DefKind, MacroDef, MacroKind};
use worlds_core::*;

const MACROS: &str = r##"{
  "$schema": "../schema/macros.json",
  "format": 1,
  "patterns": { "lip_above": ["***", "*S*", "*#*"], "spare": ["#"] },
  "legends": { "walls": { "#": "solid", "S": "!solid" } },
  "templates": {
    "frame": {
      "legend": "walls",
      "rules": [
        { "slug": "lip", "match": { "rows": "lip_above" }, "out": { "tiles": [[1, 0]] }, "break": true },
        { "slug": "fill", "match": { "rows": ["#"] }, "require": { "legend": "walls", "rows": "lip_above" },
          "out": { "tiles": [[1, 1]] } },
        { "slug": "post", "match": { "rows": ["#"] }, "out": { "stamp": "pillar" } },
        { "slug": "trim", "match": { "rows": ["#"] }, "out": { "tileset": "ssw_set5", "tiles": [[9, 9]] } }
      ]
    }
  }
}
"##;

const RULES: &str = r##"{
  "$schema": "../schema/rules.json",
  "format": 1,
  "layer": "main_tiling",
  "stamps": [
    { "slug": "pillar", "tileset": "ssw_set5", "rect": [12, 4, 2, 3] },
    { "slug": "arch", "tileset": "castle_main", "rect": [0, 4, 3, 2] }
  ],
  "groups": [
    { "slug": "walls", "rules": [
      { "slug": "named", "match": { "legend": "walls", "rows": "lip_above" },
        "out": { "tileset": "castle_main", "tiles": [[0, 0]] } },
      { "use": "frame", "slug": "walls", "tileset": "castle_main", "origin": [4, 2] },
      { "slug": "plain", "match": { "rows": ["#"] }, "out": { "tileset": "castle_main", "tiles": [[0, 1]] } },
      { "use": "blob47", "slug": "moss", "tileset": "ssw_set5" }
    ] },
    { "slug": "decor", "rules": [
      { "slug": "post_rule", "match": { "rows": ["+"] }, "out": { "stamp": "pillar" } }
    ] },
    { "slug": "spare_group", "rules": [] }
  ]
}
"##;

fn s(x: &str) -> Slug {
    slug(x)
}

/// The mini fixture with these rules and macros, saved to a temp dir and
/// loaded from it (so it knows its files).
fn on_disk() -> (tempfile::TempDir, Project) {
    let tmp = tempfile::tempdir().unwrap();
    save_all(&mut common::fixture(), tmp.path()).unwrap();
    fs::write(tmp.path().join("rules/main_tiling.json"), RULES).unwrap();
    fs::write(tmp.path().join("rules/macros.json"), MACROS).unwrap();
    let mut p = load(tmp.path()).unwrap_or_else(|e| panic!("{e}"));
    // Normalise the hand-written files to what the saver writes.
    save_all(&mut p, tmp.path()).unwrap();
    let p = load(tmp.path()).unwrap();
    (tmp, p)
}

fn full_tree(p: &Project) -> BTreeMap<String, String> {
    let tmp = tempfile::tempdir().unwrap();
    save_all(&mut p.clone(), tmp.path()).unwrap();
    common::read_tree(tmp.path())
}

fn round_trip(cmd: &Command) -> Command {
    let json = serde_json::to_string(cmd).unwrap();
    let back: Command = serde_json::from_str(&json).unwrap_or_else(|e| panic!("{e}: {json}"));
    assert_eq!(&back, cmd, "JSON round trip: {json}");
    back
}

fn files(changes: &[Change]) -> BTreeSet<PathBuf> {
    changes.iter().flat_map(|c| c.files()).collect()
}

/// Apply `cmd` and check the inverse and redo are exact, on the project and
/// on disk (see `tests/defs.rs`). Leaves the edit applied; returns its changes.
fn check(dir: &Path, p: &mut Project, cmd: Command) -> Vec<Change> {
    save_dirty(p, dir).unwrap();
    let before_tree = common::read_tree(dir);
    let before = p.clone();
    let a = p
        .apply(round_trip(&cmd))
        .unwrap_or_else(|e| panic!("{}: {e}", cmd.label()));
    assert!(!a.changes.is_empty(), "{} changed nothing", cmd.label());
    let after = p.clone();
    save_dirty(p, dir).unwrap();
    let after_tree = full_tree(p);
    assert_eq!(
        common::read_tree(dir),
        after_tree,
        "save after {}",
        cmd.label()
    );
    let b = p.apply(round_trip(&a.inverse)).unwrap();
    assert!(*p == before, "undo of {} isn't exact", cmd.label());
    assert_eq!(files(&b.changes), files(&a.changes), "{}", cmd.label());
    save_dirty(p, dir).unwrap();
    assert_eq!(
        common::read_tree(dir),
        before_tree,
        "undo of {}",
        cmd.label()
    );
    let c = p.apply(round_trip(&b.inverse)).unwrap();
    assert!(*p == after, "redo of {} isn't exact", cmd.label());
    assert_eq!(c.changes, a.changes);
    save_dirty(p, dir).unwrap();
    assert_eq!(common::read_tree(dir), after_tree);
    a.changes
}

fn refused(p: &mut Project, cmd: Command) -> CommandError {
    let before = p.clone();
    let dirty = p.files.dirty().clone();
    let e = p.apply(cmd).unwrap_err();
    assert!(*p == before, "a refused command changed the project: {e}");
    assert_eq!(*p.files.dirty(), dirty);
    e
}

fn noop(p: &mut Project, cmd: Command) {
    let before = p.clone();
    let dirty = p.files.dirty().clone();
    let a = p.apply(cmd).unwrap();
    assert!(a.changes.is_empty() && a.inverse.is_noop());
    assert!(*p == before);
    assert_eq!(*p.files.dirty(), dirty);
}

fn entry(v: Value) -> RuleEntry {
    serde_json::from_value(v).unwrap()
}

fn rule(slug: &str, tile: u32) -> RuleEntry {
    entry(json!({ "slug": slug, "match": { "rows": ["#"] },
                  "out": { "tileset": "castle_main", "tiles": [[tile, 0]] } }))
}

fn set_rule(group: &str, def: RuleEntry, index: Option<usize>) -> Command {
    Command::SetRule {
        layer: s("main_tiling"),
        group: s(group),
        rule: def.slug().clone(),
        def,
        index,
    }
}

fn remove_rule(rule: &str) -> Command {
    Command::RemoveRule {
        layer: s("main_tiling"),
        rule: s(rule),
    }
}

fn move_rule(rule: &str, group: &str, index: usize) -> Command {
    Command::MoveRule {
        layer: s("main_tiling"),
        rule: s(rule),
        group: s(group),
        index,
    }
}

/// `group: [entry slugs]` of main_tiling.
fn layout(p: &Project) -> Vec<(String, Vec<String>)> {
    p.rules_for("main_tiling")
        .unwrap()
        .groups
        .iter()
        .map(|g| {
            (
                g.slug.to_string(),
                g.rules.iter().map(|e| e.slug().to_string()).collect(),
            )
        })
        .collect()
}

fn group<'a>(p: &'a Project, g: &str) -> Vec<&'a str> {
    p.rules_for("main_tiling")
        .unwrap()
        .groups
        .iter()
        .find(|x| x.slug == g)
        .unwrap()
        .rules
        .iter()
        .map(|e| e.slug().as_str())
        .collect()
}

fn rules_def(layer: &str) -> Change {
    Change::Def {
        kind: DefKind::Rules,
        slug: s(layer),
    }
}

fn macros_def() -> Change {
    Change::Def {
        kind: DefKind::Macros,
        slug: s("macros"),
    }
}

fn macro_def(v: Value) -> MacroDef {
    serde_json::from_value(v).unwrap()
}

// ---- rules ----

#[test]
fn set_rule_creates_replaces_and_moves_within_its_group() {
    let (tmp, mut p) = on_disk();
    let changes = check(
        tmp.path(),
        &mut p,
        set_rule("walls", rule("extra", 3), Some(1)),
    );
    assert_eq!(changes, [rules_def("main_tiling")]);
    assert_eq!(
        group(&p, "walls"),
        ["named", "extra", "walls", "plain", "moss"]
    );
    // The inverse of a new entry is remove_rule.
    let a = p.apply(set_rule("walls", rule("more", 4), None)).unwrap();
    assert_eq!(a.inverse, remove_rule("more"));
    p.apply(a.inverse).unwrap();
    // Replaced in place.
    check(
        tmp.path(),
        &mut p,
        set_rule("walls", rule("extra", 5), None),
    );
    assert_eq!(
        group(&p, "walls"),
        ["named", "extra", "walls", "plain", "moss"]
    );
    // Replaced and moved: the inverse puts the old entry back at the old index.
    let a = p
        .apply(set_rule("walls", rule("extra", 6), Some(3)))
        .unwrap();
    assert_eq!(
        group(&p, "walls"),
        ["named", "walls", "plain", "extra", "moss"]
    );
    assert_eq!(a.inverse, set_rule("walls", rule("extra", 5), Some(1)));
    p.apply(a.inverse).unwrap();
    check(
        tmp.path(),
        &mut p,
        set_rule("walls", rule("extra", 5), Some(4)),
    );
    assert_eq!(
        group(&p, "walls"),
        ["named", "walls", "plain", "moss", "extra"]
    );
    // Same entry, same place: nothing. Same entry, moved: a move.
    noop(&mut p, set_rule("walls", rule("extra", 5), None));
    noop(&mut p, set_rule("walls", rule("extra", 5), Some(4)));
    check(
        tmp.path(),
        &mut p,
        set_rule("walls", rule("extra", 5), Some(0)),
    );
    // A reference is an entry like any other: replace one.
    let mut walls = p.rules_for("main_tiling").unwrap().groups[0].rules[2].clone();
    assert_eq!(walls.slug(), "walls");
    walls.as_use_mut().unwrap().origin = [0, 8];
    walls.as_use_mut().unwrap().skip = vec![s("lip")];
    check(tmp.path(), &mut p, set_rule("walls", walls, None));
    // A new reference with no slug of its own is named by its template.
    check(
        tmp.path(),
        &mut p,
        set_rule(
            "spare_group",
            entry(json!({ "use": "frame", "tileset": "ssw_set5" })),
            None,
        ),
    );
    assert_eq!(group(&p, "spare_group"), ["frame"]);
    // An expanded slug isn't an entry, so set_rule may create it (a
    // detach keeps the slug); validation reports the collision.
    check(
        tmp.path(),
        &mut p,
        set_rule("decor", rule("walls__fill", 2), None),
    );
    assert!(
        validate(&p)
            .iter()
            .any(|x| x.to_string().contains("walls__fill"))
    );
}

#[test]
fn set_rule_refusals() {
    let (_tmp, mut p) = on_disk();
    let e = refused(
        &mut p,
        Command::SetRule {
            layer: s("main_tiling"),
            group: s("walls"),
            rule: s("other"),
            def: rule("extra", 1),
            index: None,
        },
    );
    assert!(e.to_string().contains("def.slug"), "{e}");
    // In another group: use move_rule.
    let e = refused(&mut p, set_rule("decor", rule("plain", 1), None));
    assert!(matches!(e, CommandError::Invalid(_)));
    assert!(e.to_string().contains("move_rule"), "{e}");
    let e = refused(&mut p, set_rule("walls", rule("post_rule", 1), None));
    assert!(e.to_string().contains("group decor"), "{e}");
    // A reference whose slug another group's rule has.
    let e = refused(
        &mut p,
        set_rule(
            "walls",
            entry(json!({ "use": "frame", "slug": "post_rule" })),
            None,
        ),
    );
    assert!(e.to_string().contains("move_rule"), "{e}");
    assert!(matches!(
        refused(&mut p, set_rule("nope", rule("x", 1), None)),
        CommandError::UnknownRuleGroup { .. }
    ));
    let on = |layer: &str| Command::SetRule {
        layer: s(layer),
        group: s("walls"),
        rule: s("x"),
        def: rule("x", 1),
        index: None,
    };
    assert!(matches!(
        refused(&mut p, on("nope")),
        CommandError::UnknownLayer(_)
    ));
    assert!(matches!(
        refused(&mut p, on("collision")),
        CommandError::UnknownRuleGroup { .. }
    ));
    let e = refused(&mut p, set_rule("walls", rule("x", 1), Some(5)));
    assert!(e.to_string().contains("past the end"), "{e}");
    let e = refused(&mut p, set_rule("walls", rule("plain", 1), Some(4)));
    assert!(e.to_string().contains("past the end"), "{e}");
}

#[test]
fn remove_rule_and_move_rule() {
    let (tmp, mut p) = on_disk();
    // Inline rule and reference.
    let changes = check(tmp.path(), &mut p, remove_rule("plain"));
    assert_eq!(changes, [rules_def("main_tiling")]);
    let a = p.apply(remove_rule("walls")).unwrap();
    assert!(matches!(
        &a.inverse,
        Command::SetRule { group, index: Some(1), def: RuleEntry::Use(_), .. } if group == "walls"
    ));
    p.apply(a.inverse).unwrap();
    check(tmp.path(), &mut p, remove_rule("walls"));
    check(tmp.path(), &mut p, remove_rule("post_rule"));
    assert!(group(&p, "decor").is_empty());

    let (tmp, mut p) = on_disk();
    // Within a group, both ways; index counts after taking it out.
    check(tmp.path(), &mut p, move_rule("named", "walls", 2));
    assert_eq!(group(&p, "walls"), ["walls", "plain", "named", "moss"]);
    check(tmp.path(), &mut p, move_rule("moss", "walls", 0));
    assert_eq!(group(&p, "walls"), ["moss", "walls", "plain", "named"]);
    check(tmp.path(), &mut p, move_rule("moss", "walls", 3));
    assert_eq!(group(&p, "walls"), ["walls", "plain", "named", "moss"]);
    noop(&mut p, move_rule("plain", "walls", 1));
    // Across groups, to the end and into an empty group.
    check(tmp.path(), &mut p, move_rule("walls", "decor", 1));
    assert_eq!(group(&p, "decor"), ["post_rule", "walls"]);
    let a = p.apply(move_rule("plain", "spare_group", 0)).unwrap();
    assert_eq!(a.inverse, move_rule("plain", "walls", 0));
    p.apply(a.inverse).unwrap();
    check(tmp.path(), &mut p, move_rule("plain", "spare_group", 0));
    assert_eq!(
        layout(&p),
        [
            ("walls".into(), vec!["named".into(), "moss".into()]),
            ("decor".into(), vec!["post_rule".into(), "walls".into()]),
            ("spare_group".into(), vec!["plain".into()]),
        ]
    );
}

#[test]
fn remove_and_move_rule_refusals() {
    let (_tmp, mut p) = on_disk();
    // An expanded rule: use the reference's skip.
    let e = refused(&mut p, remove_rule("walls__fill"));
    assert!(matches!(e, CommandError::Invalid(_)), "{e}");
    let msg = e.to_string();
    assert!(msg.contains("skip") && msg.contains("\"walls\""), "{msg}");
    let e = refused(&mut p, remove_rule("moss__f0"));
    assert!(matches!(e, CommandError::UnknownRule { .. }), "{e}");
    let some_blob = Macros::builtin_templates()["blob47"].rules[0].slug.clone();
    let e = refused(&mut p, remove_rule(&format!("moss__{some_blob}")));
    assert!(e.to_string().contains("skip"), "{e}");
    assert!(matches!(
        refused(&mut p, move_rule("walls__lip", "walls", 0)),
        CommandError::Invalid(_)
    ));
    assert!(matches!(
        refused(&mut p, remove_rule("nope")),
        CommandError::UnknownRule { .. }
    ));
    assert!(matches!(
        refused(
            &mut p,
            Command::RemoveRule {
                layer: s("nope"),
                rule: s("x")
            }
        ),
        CommandError::UnknownLayer(_)
    ));
    assert!(matches!(
        refused(&mut p, move_rule("plain", "nope", 0)),
        CommandError::UnknownRuleGroup { .. }
    ));
    let e = refused(&mut p, move_rule("plain", "walls", 4));
    assert!(e.to_string().contains("past the end"), "{e}");
    let e = refused(&mut p, move_rule("plain", "decor", 2));
    assert!(e.to_string().contains("past the end"), "{e}");
    // A slug on two entries (a validation problem) can't be removed or moved.
    p.rules[0].groups[2].rules.push(rule("plain", 9));
    for cmd in [remove_rule("plain"), move_rule("plain", "decor", 0)] {
        let e = refused(&mut p, cmd);
        assert!(e.to_string().contains("2 entries"), "{e}");
    }
}

// ---- macros ----

#[test]
fn set_and_remove_macros_of_every_kind() {
    let (tmp, mut p) = on_disk();
    let set = |kind, name: &str, def: Value| Command::SetMacro {
        kind,
        name: s(name),
        def: macro_def(def),
    };
    let remove = |kind, name: &str| Command::RemoveMacro {
        kind,
        name: s(name),
        keep_uses: false,
        keep_empty: false,
    };
    let changes = check(
        tmp.path(),
        &mut p,
        set(MacroKind::Pattern, "spare", json!(["+"])),
    );
    assert_eq!(changes, [macros_def()]);
    check(
        tmp.path(),
        &mut p,
        set(MacroKind::Pattern, "edge", json!(["*.*", "###"])),
    );
    check(
        tmp.path(),
        &mut p,
        set(MacroKind::Legend, "solid_only", json!({ "#": "solid" })),
    );
    check(
        tmp.path(),
        &mut p,
        set(MacroKind::Legend, "walls", json!({})),
    );
    let frame = json!({ "name": "Edge", "rules": [
        { "slug": "e", "match": { "rows": "edge", "legend": "solid_only" }, "out": { "tiles": [[0, 0]] } } ] });
    check(
        tmp.path(),
        &mut p,
        set(MacroKind::Template, "edge", frame.clone()),
    );
    noop(&mut p, set(MacroKind::Template, "edge", frame));
    check(tmp.path(), &mut p, remove(MacroKind::Template, "edge"));
    check(tmp.path(), &mut p, remove(MacroKind::Pattern, "edge"));
    // A pattern and a legend may share a name.
    check(
        tmp.path(),
        &mut p,
        set(MacroKind::Legend, "spare", json!({ "x": "+" })),
    );
    check(tmp.path(), &mut p, remove(MacroKind::Pattern, "spare"));

    // Refusals.
    assert!(matches!(
        refused(&mut p, remove(MacroKind::Pattern, "spare")),
        CommandError::UnknownMacro {
            kind: MacroKind::Pattern,
            ..
        }
    ));
    let e = refused(&mut p, set(MacroKind::Legend, "x", json!(["#"])));
    assert!(
        e.to_string()
            .contains("kind is legend but def is a pattern"),
        "{e}"
    );
    for cmd in [
        set(MacroKind::Template, "blob47", json!({ "rules": [] })),
        remove(MacroKind::Template, "blob47"),
        Command::RenameMacro {
            kind: MacroKind::Template,
            from: s("frame"),
            to: s("blob47"),
        },
    ] {
        let e = refused(&mut p, cmd);
        assert!(e.to_string().contains("built-in"), "{e}");
    }
    // In use: by a rule, a reference and a template, each named.
    match refused(&mut p, remove(MacroKind::Legend, "walls")) {
        CommandError::InUse { kind, slug, uses } => {
            assert_eq!((kind, slug.as_str()), (DefKind::Macros, "walls"));
            assert_eq!(
                uses,
                ["rule named in rules/main_tiling.json", "template frame"]
            );
        }
        e => panic!("{e}"),
    }
    match refused(&mut p, remove(MacroKind::Template, "frame")) {
        CommandError::InUse { uses, .. } => {
            assert_eq!(uses, ["reference walls in rules/main_tiling.json"])
        }
        e => panic!("{e}"),
    }
    let e = refused(&mut p, remove(MacroKind::Pattern, "lip_above"));
    assert!(
        e.to_string()
            .starts_with("macro \"lip_above\" is still used by"),
        "{e}"
    );
    // keep_uses removes it anyway; the inverse puts it back.
    check(
        tmp.path(),
        &mut p,
        Command::RemoveMacro {
            kind: MacroKind::Template,
            name: s("frame"),
            keep_uses: true,
            keep_empty: false,
        },
    );
    // Defining a name already referred to: the inverse keeps the uses.
    let a = p
        .apply(set(MacroKind::Template, "frame", json!({ "rules": [] })))
        .unwrap();
    assert!(matches!(
        a.inverse,
        Command::RemoveMacro {
            keep_uses: true,
            keep_empty: false,
            ..
        }
    ));
    p.apply(a.inverse).unwrap();
}

#[test]
fn an_emptied_macros_file_goes_away_unless_kept() {
    let tmp = tempfile::tempdir().unwrap();
    save_all(&mut common::fixture(), tmp.path()).unwrap();
    let mut p = load(tmp.path()).unwrap();
    // The mini fixture's macros are unused: removing both deletes the file.
    for (kind, name) in [
        (MacroKind::Pattern, "edge_top"),
        (MacroKind::Legend, "walls"),
    ] {
        check(
            tmp.path(),
            &mut p,
            Command::RemoveMacro {
                kind,
                name: s(name),
                keep_uses: false,
                keep_empty: false,
            },
        );
    }
    assert!(p.macros.is_none());
    assert!(!tmp.path().join("rules/macros.json").exists());
    let set = Command::SetMacro {
        kind: MacroKind::Pattern,
        name: s("one"),
        def: MacroDef::Pattern(vec!["#".into()]),
    };
    // From no file: the inverse deletes the file again.
    let a = p.apply(set.clone()).unwrap();
    assert!(matches!(
        a.inverse,
        Command::RemoveMacro {
            keep_empty: false,
            ..
        }
    ));
    p.apply(a.inverse).unwrap();
    check(tmp.path(), &mut p, set.clone());
    assert!(tmp.path().join("rules/macros.json").is_file());
    let remove = |keep_empty| Command::RemoveMacro {
        kind: MacroKind::Pattern,
        name: s("one"),
        keep_uses: false,
        keep_empty,
    };
    check(tmp.path(), &mut p, remove(true));
    assert!(p.macros.as_ref().is_some_and(|m| m.patterns.is_empty()));
    assert!(tmp.path().join("rules/macros.json").is_file());
    // From an empty file: the inverse keeps it.
    let a = p.apply(set.clone()).unwrap();
    assert!(matches!(
        a.inverse,
        Command::RemoveMacro {
            keep_empty: true,
            ..
        }
    ));
    check(tmp.path(), &mut p, remove(false));
    assert!(p.macros.is_none());
    assert!(!tmp.path().join("rules/macros.json").exists());
}

#[test]
fn rename_macro_rewrites_every_reference() {
    let (tmp, mut p) = on_disk();
    let rename = |kind, from: &str, to: &str| Command::RenameMacro {
        kind,
        from: s(from),
        to: s(to),
    };
    // A legend: a rule's match.legend, a template's legend and a template
    // rule's require.legend.
    let changes = check(
        tmp.path(),
        &mut p,
        rename(MacroKind::Legend, "walls", "solidity"),
    );
    assert_eq!(changes, [macros_def(), rules_def("main_tiling")]);
    let set = &p.rules_for("main_tiling").unwrap().groups[0];
    assert_eq!(
        set.rules[0]
            .as_rule()
            .unwrap()
            .as_match()
            .unwrap()
            .pattern
            .legend,
        Legend::Named(s("solidity"))
    );
    let frame = &p.macros.as_ref().unwrap().templates["frame"];
    assert_eq!(frame.legend, Legend::Named(s("solidity")));
    assert_eq!(
        frame.rules[1].require[0].legend,
        Legend::Named(s("solidity"))
    );
    assert!(p.named_legend("walls").is_none() && p.named_legend("solidity").is_some());
    // A pattern: match.rows in a rule and in a template, require.rows.
    check(
        tmp.path(),
        &mut p,
        rename(MacroKind::Pattern, "lip_above", "lip"),
    );
    let frame = &p.macros.as_ref().unwrap().templates["frame"];
    assert_eq!(frame.rules[0].pattern.rows, Rows::Named(s("lip")));
    assert_eq!(frame.rules[1].require[0].rows, Rows::Named(s("lip")));
    // Unused: only macros.json changes.
    let changes = check(
        tmp.path(),
        &mut p,
        rename(MacroKind::Pattern, "spare", "single"),
    );
    assert_eq!(changes, [macros_def()]);
    // A template: every `use`; the expanded slugs follow the reference slug.
    let changes = check(
        tmp.path(),
        &mut p,
        rename(MacroKind::Template, "frame", "framed"),
    );
    assert_eq!(changes, [macros_def(), rules_def("main_tiling")]);
    let walls = p.rules_for("main_tiling").unwrap().groups[0].rules[1]
        .as_use()
        .unwrap();
    assert_eq!(
        (walls.template.as_str(), walls.slug().as_str()),
        ("framed", "walls")
    );
    // Every renamed name still resolves.
    let expanded = p.expanded_rules("main_tiling").unwrap();
    assert!(
        expanded[0]
            .rules
            .iter()
            .any(|r| r.rule.slug == "walls__fill")
    );
    noop(&mut p, rename(MacroKind::Template, "framed", "framed"));

    // Refusals: unknown, taken, already referred to, built-in.
    assert!(matches!(
        refused(&mut p, rename(MacroKind::Pattern, "nope", "x")),
        CommandError::UnknownMacro { .. }
    ));
    check(
        tmp.path(),
        &mut p,
        Command::SetMacro {
            kind: MacroKind::Pattern,
            name: s("other"),
            def: MacroDef::Pattern(vec!["+".into()]),
        },
    );
    assert!(matches!(
        refused(&mut p, rename(MacroKind::Pattern, "lip", "other")),
        CommandError::DuplicateDef {
            kind: DefKind::Macros,
            ..
        }
    ));
    // A legend named "other" doesn't exist, so that's free.
    check(
        tmp.path(),
        &mut p,
        rename(MacroKind::Legend, "solidity", "other"),
    );
    p.apply(rename(MacroKind::Legend, "other", "solidity"))
        .unwrap();
    // A rule already names a legend "ghost" that doesn't exist.
    p.apply(set_rule(
        "spare_group",
        entry(
            json!({ "slug": "haunted", "match": { "legend": "ghost", "rows": ["#"] },
                      "out": { "tileset": "castle_main", "tiles": [[0, 0]] } }),
        ),
        None,
    ))
    .unwrap();
    let e = refused(&mut p, rename(MacroKind::Legend, "solidity", "ghost"));
    assert!(
        e.to_string()
            .contains("rule haunted in rules/main_tiling.json"),
        "{e}"
    );
    assert!(matches!(
        refused(&mut p, rename(MacroKind::Template, "blob47", "blob")),
        CommandError::Invalid(_)
    ));
}

/// Renaming a template keeps the name of a reference that had none of its
/// own (its slug is pinned to the old template name), so its expanded
/// slugs, and the random rolls keyed on them, stay; undo takes the pin out.
#[test]
fn renaming_a_template_pins_slugless_references() {
    let (tmp, mut p) = on_disk();
    check(
        tmp.path(),
        &mut p,
        set_rule(
            "spare_group",
            entry(json!({ "use": "frame", "tileset": "ssw_set5" })),
            None,
        ),
    );
    let expanded = |p: &Project| -> Vec<String> {
        p.expanded_rules("main_tiling")
            .unwrap()
            .iter()
            .flat_map(|g| g.rules.iter().map(|r| r.rule.slug.to_string()))
            .collect()
    };
    let before = expanded(&p);
    assert!(before.contains(&"frame__fill".to_string()), "{before:?}");
    let rename = Command::RenameMacro {
        kind: MacroKind::Template,
        from: s("frame"),
        to: s("framed"),
    };
    let changes = check(tmp.path(), &mut p, rename.clone());
    // The pinned layer first (what a redo reports too), then the file.
    assert_eq!(changes, [rules_def("main_tiling"), macros_def()]);
    let u = p.rules_for("main_tiling").unwrap().groups[2].rules[0]
        .as_use()
        .unwrap();
    assert_eq!(u.template, "framed");
    assert_eq!(u.slug.as_ref().map(Slug::as_str), Some("frame"));
    // `walls` had its own slug: only its `use` changed.
    let walls = p.rules_for("main_tiling").unwrap().groups[0].rules[1]
        .as_use()
        .unwrap();
    assert_eq!(
        (
            walls.template.as_str(),
            walls.slug.as_ref().map(Slug::as_str)
        ),
        ("framed", Some("walls"))
    );
    assert_eq!(expanded(&p), before);
    let saved = fs::read_to_string(tmp.path().join("rules/main_tiling.json")).unwrap();
    assert!(
        saved.contains(r#""use": "framed""#) && saved.contains(r#""slug": "frame""#),
        "{saved}"
    );

    // A slug-less reference undo couldn't find again by its slug (another
    // entry has it too): the rename is refused.
    let (_tmp, mut p) = on_disk();
    p.apply(set_rule(
        "spare_group",
        entry(json!({ "use": "frame" })),
        None,
    ))
    .unwrap();
    p.rules[0].groups[1]
        .rules
        .push(entry(json!({ "use": "frame" })));
    let e = refused(&mut p, rename);
    assert!(e.to_string().contains("2 entries"), "{e}");
}

/// Two groups with one slug (a validation problem the loader accepts): every
/// command that finds a group by slug is refused, naming the duplicate,
/// instead of editing the first one and undoing into the other.
#[test]
fn duplicate_group_slugs_refuse_group_and_rule_commands() {
    let (_tmp, mut p) = on_disk();
    let mut twin = p.rules[0].groups[1].clone();
    twin.rules.clear();
    p.rules[0].groups.push(twin.clone());
    let set_group = |def: RuleGroup, index| Command::SetRuleGroup {
        layer: s("main_tiling"),
        group: s("decor"),
        def,
        index,
    };
    let mut changed = twin.clone();
    changed.rules.push(rule("again", 1));
    for cmd in [
        set_group(changed, Some(0)),
        Command::RemoveRuleGroup {
            layer: s("main_tiling"),
            group: s("decor"),
            keep_empty: false,
        },
        set_rule("decor", rule("post_rule", 2), Some(0)),
        set_rule("walls", rule("fresh", 2), None),
        remove_rule("plain"),
        move_rule("post_rule", "walls", 0),
    ] {
        let what = cmd.label();
        let e = refused(&mut p, cmd);
        assert!(matches!(e, CommandError::Invalid(_)), "{what}: {e}");
        assert!(
            e.to_string().contains("groups of rules/main_tiling.json")
                && e.to_string().contains("\"decor\""),
            "{what}: {e}"
        );
    }
    // Stamps are found by slug the same way.
    let (_tmp, mut p) = on_disk();
    let arch = p.rules[0].stamps[1].clone();
    p.rules[0].stamps.push(arch.clone());
    let mut moved = arch.clone();
    moved.rect[0] = 9;
    for cmd in [
        Command::SetStamp {
            layer: s("main_tiling"),
            stamp: s("arch"),
            def: moved,
            index: Some(0),
        },
        Command::RemoveStamp {
            layer: s("main_tiling"),
            stamp: s("arch"),
            keep_uses: false,
            keep_empty: false,
        },
    ] {
        let e = refused(&mut p, cmd);
        assert!(
            e.to_string().contains("stamps of rules/main_tiling.json"),
            "{e}"
        );
    }
}

/// `set_rule` on a slug two entries of its group share: which one it
/// replaces, and where undo puts it, would be a guess.
#[test]
fn set_rule_refuses_a_slug_twice_in_its_group() {
    let (_tmp, mut p) = on_disk();
    p.rules[0].groups[0].rules.push(rule("plain", 9));
    let e = refused(&mut p, set_rule("walls", rule("plain", 3), Some(1)));
    assert!(e.to_string().contains("2 entries of group walls"), "{e}");
}

#[test]
fn macro_defs_parse_by_shape() {
    assert_eq!(macro_def(json!(["#"])).kind(), MacroKind::Pattern);
    assert_eq!(macro_def(json!({})).kind(), MacroKind::Legend);
    assert_eq!(macro_def(json!({ "#": "solid" })).kind(), MacroKind::Legend);
    assert_eq!(
        macro_def(json!({ "rules": [] })).kind(),
        MacroKind::Template
    );
    for (bad, want) in [
        (json!(3), "a macro is rows"),
        (json!({ "#": 3 }), "legend"),
        (json!({ "name": "x" }), "rules"),
        (json!({ "rules": [], "bogus": 1 }), "bogus"),
    ] {
        let e = serde_json::from_value::<MacroDef>(bad.clone())
            .unwrap_err()
            .to_string();
        assert!(e.contains(want), "{bad}: {e}");
    }
    let cmd: Command = serde_json::from_value(json!({
        "op": "set_macro", "kind": "legend", "name": "w", "def": { "w": "[solid|fortified]" }
    }))
    .unwrap();
    assert_eq!(round_trip(&cmd), cmd);
}

// ---- stamps ----

#[test]
fn stamps_set_replace_and_remove() {
    let (tmp, mut p) = on_disk();
    let stamp = |slug: &str, col: u32| -> Stamp {
        serde_json::from_value(
            json!({ "slug": slug, "tileset": "castle_main", "rect": [col, 0, 2, 2] }),
        )
        .unwrap()
    };
    let set = |st: Stamp, index| Command::SetStamp {
        layer: s("main_tiling"),
        stamp: st.slug.clone(),
        def: st,
        index,
    };
    let remove = |slug: &str, keep_uses, keep_empty| Command::RemoveStamp {
        layer: s("main_tiling"),
        stamp: s(slug),
        keep_uses,
        keep_empty,
    };
    let changes = check(tmp.path(), &mut p, set(stamp("door", 0), Some(0)));
    assert_eq!(changes, [rules_def("main_tiling")]);
    check(tmp.path(), &mut p, set(stamp("door", 4), None));
    check(tmp.path(), &mut p, set(stamp("door", 4), Some(2)));
    noop(&mut p, set(stamp("door", 4), None));
    check(tmp.path(), &mut p, remove("door", false, false));
    check(tmp.path(), &mut p, remove("arch", false, false));
    // In use by an inline rule and through a referenced template.
    match refused(&mut p, remove("pillar", false, false)) {
        CommandError::StampInUse { layer, stamp, uses } => {
            assert_eq!((layer.as_str(), stamp.as_str()), ("main_tiling", "pillar"));
            assert_eq!(uses, ["template frame (reference walls)", "rule post_rule"]);
        }
        e => panic!("{e}"),
    }
    let e = serde_json::to_value(refused(&mut p, remove("pillar", false, false))).unwrap();
    assert_eq!(
        (e["kind"].as_str(), e["def_kind"].as_str()),
        (Some("in_use"), Some("stamp"))
    );
    check(tmp.path(), &mut p, remove("pillar", true, false));
    // Defining a stamp a rule already places: the inverse keeps the use.
    let a = p.apply(set(stamp("pillar", 9), None)).unwrap();
    assert!(matches!(
        a.inverse,
        Command::RemoveStamp {
            keep_uses: true,
            ..
        }
    ));
    p.apply(a.inverse).unwrap();

    // Refusals.
    assert!(matches!(
        refused(&mut p, remove("nope", false, false)),
        CommandError::UnknownStamp { .. }
    ));
    let e = refused(
        &mut p,
        Command::SetStamp {
            layer: s("main_tiling"),
            stamp: s("x"),
            def: stamp("y", 0),
            index: None,
        },
    );
    assert!(e.to_string().contains("def.slug"), "{e}");
    let e = refused(&mut p, set(stamp("x", 0), Some(3)));
    assert!(e.to_string().contains("past the end"), "{e}");
    let on = |layer: &str| Command::SetStamp {
        layer: s(layer),
        stamp: s("x"),
        def: stamp("x", 0),
        index: None,
    };
    assert!(matches!(
        refused(&mut p, on("collision")),
        CommandError::WrongLayerKind { .. }
    ));
    assert!(matches!(
        refused(&mut p, on("nope")),
        CommandError::UnknownLayer(_)
    ));

    // A new auto layer gets a rule set from its first stamp; removing the
    // last one removes the set again unless kept.
    let def: LayerDef = serde_json::from_value(
        json!({ "slug": "moss", "name": "Moss", "kind": "auto", "source": "collision" }),
    )
    .unwrap();
    p.apply(Command::SetLayerDef {
        layer: s("moss"),
        def,
        index: None,
        content: Vec::new(),
        rules: None,
    })
    .unwrap();
    let on_moss = Command::SetStamp {
        layer: s("moss"),
        stamp: s("rock"),
        def: stamp("rock", 0),
        index: None,
    };
    let e = refused(
        &mut p,
        Command::SetStamp {
            layer: s("moss"),
            stamp: s("rock"),
            def: stamp("rock", 0),
            index: Some(1),
        },
    );
    assert!(e.to_string().contains("past the end"), "{e}");
    check(tmp.path(), &mut p, on_moss.clone());
    assert!(tmp.path().join("rules/moss.json").is_file());
    let remove_rock = |keep_empty| Command::RemoveStamp {
        layer: s("moss"),
        stamp: s("rock"),
        keep_uses: false,
        keep_empty,
    };
    check(tmp.path(), &mut p, remove_rock(true));
    assert!(p.rules_for("moss").is_some_and(|r| r.stamps.is_empty()));
    let a = p.apply(on_moss).unwrap();
    assert!(matches!(
        a.inverse,
        Command::RemoveStamp {
            keep_empty: true,
            ..
        }
    ));
    check(tmp.path(), &mut p, remove_rock(false));
    assert!(p.rules_for("moss").is_none());
    assert!(!tmp.path().join("rules/moss.json").exists());
}

// ---- walkers ----

#[test]
fn tileset_uses_name_templates_and_references() {
    let (_tmp, mut p) = on_disk();
    match refused(
        &mut p,
        Command::RemoveTilesetDef {
            tileset: s("ssw_set5"),
            keep_uses: false,
        },
    ) {
        CommandError::InUse { uses, .. } => {
            assert!(uses.contains(&"template frame".to_string()), "{uses:?}");
            assert!(
                uses.contains(&"rule set main_tiling reference moss".to_string()),
                "{uses:?}"
            );
            assert!(
                uses.contains(&"rule set main_tiling".to_string()),
                "{uses:?}"
            );
        }
        e => panic!("{e}"),
    }
    // Rename reaches the reference, the fixed template rule and the stamp.
    let a = p
        .apply(Command::RenameTileset {
            from: s("ssw_set5"),
            to: s("set5"),
        })
        .unwrap();
    assert!(a.changes.contains(&macros_def()) && a.changes.contains(&rules_def("main_tiling")));
    let set = p.rules_for("main_tiling").unwrap();
    assert_eq!(set.stamps[0].tileset, "set5");
    assert_eq!(
        set.groups[0].rules[3].as_use().unwrap().tileset,
        Some(s("set5"))
    );
    let trim = &p.macros.as_ref().unwrap().templates["frame"].rules[3];
    assert!(matches!(&trim.out, TemplateOutput::Tiles(t) if t.tileset == Some(s("set5"))));
}

#[test]
fn layer_round_trips_carry_references_and_stamps() {
    let (tmp, mut p) = on_disk();
    let set = p.rules_for("main_tiling").unwrap().clone();
    let a = p
        .apply(Command::RemoveLayerDef {
            layer: s("main_tiling"),
        })
        .unwrap();
    match &a.inverse {
        Command::SetLayerDef { rules: Some(r), .. } => assert_eq!(*r, set),
        c => panic!("{c:?}"),
    }
    p.apply(a.inverse).unwrap();
    // A rename of the auto layer keeps its rule set whole.
    check(
        tmp.path(),
        &mut p,
        Command::RenameLayer {
            from: s("main_tiling"),
            to: s("tiling"),
        },
    );
    let mut renamed = set;
    renamed.layer = s("tiling");
    assert_eq!(p.rules_for("tiling"), Some(&renamed));
}

#[test]
fn rule_edits_in_a_batch_report_one_change() {
    let (_tmp, mut p) = on_disk();
    let a = p
        .apply(Command::Batch {
            label: "Panel: three edits".into(),
            commands: vec![
                set_rule("walls", rule("a", 1), None),
                move_rule("a", "decor", 0),
                remove_rule("plain"),
                Command::SetMacro {
                    kind: MacroKind::Pattern,
                    name: s("p"),
                    def: MacroDef::Pattern(vec!["#".into()]),
                },
                set_rule("walls", rule("b", 1), None),
            ],
        })
        .unwrap();
    assert_eq!(a.changes, [rules_def("main_tiling"), macros_def()]);
}

// ---- random sequences ----

/// A small xorshift, so the sequences are the same every run.
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545_f491_4f6c_dd1d) % n.max(1) as u64) as usize
    }

    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len())]
    }

    fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }
}

const NAMES: &[&str] = &["walls", "lip_above", "frame", "m0", "m1", "blob47", "spare"];
const SLUGS: &[&str] = &["r0", "r1", "r2", "named", "walls", "plain", "moss", "frame"];
const STAMPS: &[&str] = &["pillar", "arch", "s0", "s1"];
const KINDS: &[MacroKind] = &[MacroKind::Pattern, MacroKind::Legend, MacroKind::Template];

fn some_slugs(p: &Project, layer: &str) -> Vec<String> {
    let mut out: Vec<String> = SLUGS.iter().map(|x| x.to_string()).collect();
    if let Some(set) = p.rules_for(layer) {
        out.extend(set.entries().map(|(_, e)| e.slug().to_string()));
        // An expanded slug now and then.
        out.extend(
            set.entries()
                .filter_map(|(_, e)| e.as_use())
                .map(|u| format!("{}__lip", u.slug())),
        );
    }
    out
}

fn random_pattern(rng: &mut Rng) -> Value {
    let rows: Value = if rng.chance(30) {
        json!(rng.pick(NAMES))
    } else {
        json!([rng.pick(&["#", "+", "S#S"])])
    };
    let mut p = json!({ "rows": rows });
    if rng.chance(30) {
        p["legend"] = json!(rng.pick(NAMES));
    }
    p
}

fn random_out(rng: &mut Rng, template: bool) -> Value {
    if rng.chance(20) {
        json!({ "stamp": rng.pick(STAMPS) })
    } else if template && rng.chance(50) {
        json!({ "tiles": [[rng.below(4), 0]] })
    } else {
        json!({ "tileset": rng.pick(&["castle_main", "ssw_set5"]), "tiles": [[rng.below(4), 0]] })
    }
}

fn random_entry(rng: &mut Rng, slug: &str) -> RuleEntry {
    if rng.chance(30) {
        let mut u = json!({ "use": rng.pick(NAMES), "slug": slug });
        if rng.chance(70) {
            u["tileset"] = json!("castle_main");
        }
        if rng.chance(30) {
            u["skip"] = json!(["lip"]);
        }
        return entry(u);
    }
    let mut r =
        json!({ "slug": slug, "match": random_pattern(rng), "out": random_out(rng, false) });
    if rng.chance(20) {
        r["require"] = random_pattern(rng);
    }
    entry(r)
}

fn random_macro(rng: &mut Rng, kind: MacroKind) -> MacroDef {
    // Now and then the wrong kind, which is refused.
    let kind = if rng.chance(5) {
        *rng.pick(KINDS)
    } else {
        kind
    };
    match kind {
        MacroKind::Pattern => macro_def(json!([rng.pick(&["#", "+", "S#S"])])),
        MacroKind::Legend => macro_def(json!({ "#": rng.pick(&["solid", "!solid", "+"]) })),
        MacroKind::Template => {
            let mut t = json!({ "rules": [
                { "slug": "lip", "match": random_pattern(rng), "out": random_out(rng, true) }
            ] });
            if rng.chance(40) {
                t["legend"] = json!(rng.pick(NAMES));
            }
            macro_def(t)
        }
    }
}

fn random_command(rng: &mut Rng, p: &Project, layers: &[&str]) -> Command {
    let layer = *rng.pick(layers);
    let groups: Vec<String> = p.rules_for(layer).map_or_else(Vec::new, |set| {
        set.groups.iter().map(|g| g.slug.to_string()).collect()
    });
    let group = if groups.is_empty() || rng.chance(5) {
        "nope".to_string()
    } else {
        rng.pick(&groups).clone()
    };
    let slugs = some_slugs(p, layer);
    let slug = rng.pick(&slugs).clone();
    let index = |rng: &mut Rng| rng.below(6);
    match rng.below(10) {
        0 | 1 => {
            let i = index(rng);
            Command::SetRule {
                layer: s(layer),
                group: s(&group),
                rule: s(&slug),
                def: random_entry(rng, &slug),
                index: rng.chance(50).then_some(i),
            }
        }
        2 => Command::RemoveRule {
            layer: s(layer),
            rule: s(&slug),
        },
        3 => Command::MoveRule {
            layer: s(layer),
            rule: s(&slug),
            group: s(&group),
            index: index(rng),
        },
        4 => {
            let kind = *rng.pick(KINDS);
            Command::SetMacro {
                kind,
                name: s(rng.pick::<&str>(NAMES)),
                def: random_macro(rng, kind),
            }
        }
        5 => Command::RemoveMacro {
            kind: *rng.pick(KINDS),
            name: s(rng.pick::<&str>(NAMES)),
            keep_uses: rng.chance(30),
            keep_empty: rng.chance(30),
        },
        6 => Command::RenameMacro {
            kind: *rng.pick(KINDS),
            from: s(rng.pick::<&str>(NAMES)),
            to: s(rng.pick::<&str>(NAMES)),
        },
        7 => {
            let stamp = *rng.pick(STAMPS);
            let def = serde_json::from_value(json!({
                "slug": stamp, "tileset": "castle_main", "rect": [rng.below(3), 0, 1, 1]
            }))
            .unwrap();
            let i = index(rng);
            Command::SetStamp {
                layer: s(layer),
                stamp: s(stamp),
                def,
                index: rng.chance(30).then_some(i % 3),
            }
        }
        8 => Command::RemoveStamp {
            layer: s(layer),
            stamp: s(rng.pick::<&str>(STAMPS)),
            keep_uses: rng.chance(30),
            keep_empty: rng.chance(30),
        },
        _ => Command::Batch {
            label: "random".into(),
            commands: (0..2 + rng.below(3))
                .map(|_| random_command(rng, p, layers))
                .filter(|c| !matches!(c, Command::Batch { .. }))
                .collect(),
        },
    }
}

/// Apply random rule, macro and stamp commands. Each one either is refused
/// and changes nothing, or is a no-op, or its inverse gives back the whole
/// project exactly (dirtying the same files), and the inverse of the inverse
/// redoes it with the same changes. With `dir`, the incremental save after
/// each edit writes exactly what a full save would.
fn random_sequences(mut p: Project, layers: &[&str], steps: usize, seed: u64, dir: Option<&Path>) {
    let mut rng = Rng(seed);
    let (mut applied, mut noops, mut refusals) = (0, 0, 0);
    for step in 0..steps {
        let cmd = random_command(&mut rng, &p, layers);
        let what = format!("step {step}: {}", serde_json::to_string(&cmd).unwrap());
        let before = p.clone();
        let dirty = p.files.dirty().clone();
        let a = match p.apply(round_trip(&cmd)) {
            Err(e) => {
                assert!(p == before, "{what}: refused ({e}) but changed the project");
                assert_eq!(*p.files.dirty(), dirty, "{what}");
                refusals += 1;
                continue;
            }
            Ok(a) => a,
        };
        if a.changes.is_empty() {
            assert!(
                p == before && a.inverse.is_noop(),
                "{what}: a no-op that changed"
            );
            assert_eq!(*p.files.dirty(), dirty, "{what}");
            noops += 1;
            continue;
        }
        applied += 1;
        let mut want_dirty = dirty.clone();
        want_dirty.extend(files(&a.changes));
        assert_eq!(*p.files.dirty(), want_dirty, "{what}");
        let after = p.clone();
        let b = p
            .apply(round_trip(&a.inverse))
            .unwrap_or_else(|e| panic!("{what}: inverse refused: {e}"));
        assert!(p == before, "{what}: undo isn't exact");
        assert_eq!(files(&b.changes), files(&a.changes), "{what}");
        assert_eq!(*p.files.dirty(), want_dirty, "{what}");
        let c = p
            .apply(round_trip(&b.inverse))
            .unwrap_or_else(|e| panic!("{what}: redo refused: {e}"));
        assert!(p == after, "{what}: redo isn't exact");
        assert_eq!(c.changes, a.changes, "{what}");
        if let Some(dir) = dir {
            save_dirty(&mut p, dir).unwrap();
            assert_eq!(common::read_tree(dir), full_tree(&p), "{what}: saved files");
        }
    }
    eprintln!("{applied} applied, {noops} no-ops, {refusals} refused");
    assert!(applied > steps / 4 && refusals > 0 && noops > 0);
}

#[test]
fn random_rule_macro_and_stamp_edits_invert_exactly_on_mini() {
    let (tmp, p) = on_disk();
    random_sequences(p, &["main_tiling"], 400, 0x5eed_0001, Some(tmp.path()));
    let p = common::fixture();
    random_sequences(p, &["main_tiling", "collision"], 300, 0x5eed_0002, None);
}

#[test]
fn random_rule_macro_and_stamp_edits_invert_exactly_on_an_imported_project() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/ldtk-samples/AutoLayers_5_Advanced.ldtk");
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("imported.worlds");
    let imported = worlds_core::ldtk::import(&src, &dir).unwrap();
    worlds_core::ldtk::write(&imported, &dir).unwrap();
    let p = load(&dir).unwrap_or_else(|e| panic!("{e}"));
    let layers: Vec<String> = p.rules.iter().map(|r| r.layer.to_string()).collect();
    let layers: Vec<&str> = layers.iter().map(String::as_str).collect();
    assert!(!layers.is_empty());
    random_sequences(p, &layers, 400, 0x5eed_0003, None);
}
