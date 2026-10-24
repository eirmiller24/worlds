//! Several worlds per project: the world commands (exact inverses, the
//! `project.json` `worlds` display order), moving levels between worlds,
//! saving (directories created, moved, removed) and old projects.

mod common;

use std::fs;

use worlds_core::command::{Change, Command, CommandError, NoteScope, ProjectKey};
use worlds_core::*;

fn s(x: &str) -> Slug {
    slug(x)
}

/// Apply `cmd`, check undo and redo are exact and every command survives a
/// JSON round trip; leave it applied and return its changes.
fn check(p: &mut Project, cmd: Command) -> Vec<Change> {
    let json = serde_json::to_string(&cmd).unwrap();
    assert_eq!(serde_json::from_str::<Command>(&json).unwrap(), cmd);
    let before = p.clone();
    let a = p
        .apply(cmd.clone())
        .unwrap_or_else(|e| panic!("{}: {e}", cmd.label()));
    let after = p.clone();
    let inv: Command = serde_json::from_str(&serde_json::to_string(&a.inverse).unwrap()).unwrap();
    assert_eq!(inv, a.inverse);
    let b = p.apply(inv).unwrap();
    assert!(*p == before, "undo of {} isn't exact", cmd.label());
    let c = p.apply(b.inverse).unwrap();
    assert!(*p == after, "redo of {} isn't exact", cmd.label());
    // Redoing a world's creation or removal undoes a batch (the world list
    // is put back on its own), so the changes come in another order; they
    // must still cover every file the original touched.
    let files = |cs: &[Change]| -> std::collections::BTreeSet<std::path::PathBuf> {
        cs.iter().flat_map(|c| c.files()).collect()
    };
    assert!(
        files(&c.changes).is_superset(&files(&a.changes)),
        "redo of {} covers less: {:?} vs {:?}",
        cmd.label(),
        c.changes,
        a.changes
    );
    a.changes
}

fn order(p: &Project) -> Vec<&str> {
    p.worlds.iter().map(|w| w.slug.as_str()).collect()
}

fn create(slug: &str, name: Option<&str>) -> Command {
    Command::CreateWorld {
        slug: s(slug),
        name: name.map(String::from),
    }
}

#[test]
fn create_world_lists_the_order_once_there_are_two() {
    let mut p = common::fixture();
    assert!(p.world_order.is_empty());
    let changes = check(&mut p, create("dungeon", Some("Dungeon")));
    assert_eq!(
        changes,
        [
            Change::WorldCreated {
                world: s("dungeon")
            },
            Change::Project {
                key: ProjectKey::Worlds
            }
        ]
    );
    assert_eq!(order(&p), ["main", "dungeon"]);
    assert_eq!(p.world_order, [s("main"), s("dungeon")]);
    assert_eq!(p.world("dungeon").unwrap().name.as_deref(), Some("Dungeon"));
    // A third goes last, whatever its slug.
    check(&mut p, create("attic", None));
    assert_eq!(order(&p), ["main", "dungeon", "attic"]);
    assert_eq!(
        p.apply(create("attic", None)).unwrap_err(),
        CommandError::DuplicateWorld(s("attic"))
    );
    assert!(validate(&p).is_empty(), "{:?}", validate(&p));
}

#[test]
fn reorder_and_rename_worlds() {
    let mut p = common::fixture();
    p.apply(create("dungeon", None)).unwrap();
    p.apply(create("attic", None)).unwrap();
    check(
        &mut p,
        Command::ReorderWorlds {
            order: vec![s("attic"), s("main"), s("dungeon")],
            restore: false,
        },
    );
    assert_eq!(order(&p), ["attic", "main", "dungeon"]);
    // Not a permutation: refused.
    for bad in [
        vec![s("attic"), s("main")],
        vec![s("attic"), s("main"), s("main")],
        vec![s("attic"), s("main"), s("cellar")],
    ] {
        assert!(
            p.apply(Command::ReorderWorlds {
                order: bad,
                restore: false
            })
            .is_err()
        );
    }
    // `[]` drops the list: slug order.
    check(
        &mut p,
        Command::ReorderWorlds {
            order: vec![],
            restore: false,
        },
    );
    assert_eq!(order(&p), ["attic", "dungeon", "main"]);
    p.apply(Command::ReorderWorlds {
        order: vec![s("main"), s("dungeon"), s("attic")],
        restore: false,
    })
    .unwrap();

    // Rename the slug: the list follows, the place in the order stays.
    let changes = check(
        &mut p,
        Command::RenameWorld {
            world: s("main"),
            slug: Some(s("overworld")),
            name: Some("Overworld".into()),
        },
    );
    assert_eq!(
        changes,
        [
            Change::WorldRenamed {
                from: s("main"),
                to: s("overworld")
            },
            Change::Project {
                key: ProjectKey::Worlds
            },
            Change::WorldMeta {
                world: s("overworld")
            }
        ]
    );
    assert_eq!(order(&p), ["overworld", "dungeon", "attic"]);
    let w = p.world("overworld").unwrap();
    assert_eq!(w.levels.len(), 2);
    assert_eq!(w.name.as_deref(), Some("Overworld"));
    // Name only; "" removes it.
    check(
        &mut p,
        Command::RenameWorld {
            world: s("overworld"),
            slug: None,
            name: Some(String::new()),
        },
    );
    assert_eq!(p.world("overworld").unwrap().name, None);
    assert_eq!(
        p.apply(Command::RenameWorld {
            world: s("overworld"),
            slug: Some(s("dungeon")),
            name: None,
        })
        .unwrap_err(),
        CommandError::DuplicateWorld(s("dungeon"))
    );
    // Notes still resolve by scope after the rename.
    let note = p.find_note(&Id::new("pinstatue2").unwrap()).unwrap();
    assert_eq!(note.scope(), NoteScope::World(s("overworld")));
}

#[test]
fn remove_world_needs_confirm_unless_empty_and_undo_restores_everything() {
    let mut p = common::fixture();
    p.apply(create("dungeon", None)).unwrap();
    // The last world can't go; an empty one can without confirm.
    let mut one = common::fixture();
    assert!(
        one.apply(Command::RemoveWorld {
            world: s("main"),
            confirm: true
        })
        .is_err()
    );
    let e = p
        .apply(Command::RemoveWorld {
            world: s("main"),
            confirm: false,
        })
        .unwrap_err();
    assert!(e.to_string().contains("set confirm"), "{e}");
    let changes = check(
        &mut p,
        Command::RemoveWorld {
            world: s("main"),
            confirm: true,
        },
    );
    assert!(changes.contains(&Change::LevelDeleted {
        world: s("main"),
        level: s("crevasse")
    }));
    assert!(changes.contains(&Change::WorldRemoved { world: s("main") }));
    assert_eq!(order(&p), ["dungeon"]);
    assert!(p.level("crevasse").is_none());
    // An empty world goes without confirm.
    p.apply(create("attic", None)).unwrap();
    check(
        &mut p,
        Command::RemoveWorld {
            world: s("attic"),
            confirm: false,
        },
    );
    assert_eq!(order(&p), ["dungeon"]);
}

#[test]
fn move_level_to_world_keeps_the_level_and_its_links() {
    let mut p = common::fixture();
    p.apply(create("dungeon", None)).unwrap();
    let before = p.level("crevasse").unwrap().clone();
    let changes = check(
        &mut p,
        Command::MoveLevelToWorld {
            level: s("crevasse"),
            world: s("dungeon"),
            world_px: Some([64, 32]),
            keep_layout: false,
        },
    );
    assert_eq!(
        changes,
        [
            Change::LevelDeleted {
                world: s("main"),
                level: s("crevasse")
            },
            Change::LevelCreated {
                world: s("dungeon"),
                level: s("crevasse")
            }
        ]
    );
    let moved = p.world("dungeon").unwrap().level("crevasse").unwrap();
    assert_eq!(moved.id, before.id);
    assert_eq!(moved.world_px, [64, 32]);
    assert_eq!(moved.int_grids, before.int_grids);
    assert!(p.world("main").unwrap().level("crevasse").is_none());
    // `level:crevasse` links (world notes of main) still resolve: slugs
    // are unique across worlds.
    assert!(
        !validate(&p).iter().any(|x| x.message.contains("crevasse")),
        "{:?}",
        validate(&p)
    );
    // Same world, new position: a plain move.
    let changes = check(
        &mut p,
        Command::MoveLevelToWorld {
            level: s("crevasse"),
            world: s("dungeon"),
            world_px: Some([0, 0]),
            keep_layout: false,
        },
    );
    assert!(matches!(changes[..], [Change::LevelMeta { .. }]));
    assert!(
        p.apply(Command::MoveLevelToWorld {
            level: s("crevasse"),
            world: s("dungeon"),
            world_px: None,
            keep_layout: false,
        })
        .unwrap()
        .changes
        .is_empty()
    );
}

#[test]
fn saving_moves_world_directories_and_old_projects_stay_byte_for_byte() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let mut p = common::fixture();
    save_all(&mut p, dir).unwrap();
    let start = common::read_tree(dir);
    assert!(!start["project.json"].contains("\"worlds\""));
    // Reloaded and saved again: nothing changes.
    let mut again = load(dir).unwrap();
    save_all(&mut again, dir).unwrap();
    assert_eq!(common::read_tree(dir), start);

    // A second world: project.json lists the order, the directory appears.
    p.apply(create("dungeon", Some("Dungeon"))).unwrap();
    p.apply(Command::MoveLevelToWorld {
        level: s("shaft"),
        world: s("dungeon"),
        world_px: None,
        keep_layout: false,
    })
    .unwrap();
    save_dirty(&mut p, dir).unwrap();
    let tree = common::read_tree(dir);
    assert!(
        tree["project.json"].contains("\"worlds\": [\"main\", \"dungeon\"]"),
        "{}",
        tree["project.json"]
    );
    assert!(tree.contains_key("worlds/dungeon/world.json"));
    assert!(tree.contains_key("worlds/dungeon/levels/shaft/level.json"));
    assert!(
        !tree
            .keys()
            .any(|k| k.starts_with("worlds/main/levels/shaft"))
    );
    assert_eq!(load(dir).unwrap(), p);

    // Renaming moves the directory; the old one goes away.
    p.apply(Command::RenameWorld {
        world: s("dungeon"),
        slug: Some(s("underdark")),
        name: None,
    })
    .unwrap();
    save_dirty(&mut p, dir).unwrap();
    assert!(!dir.join("worlds/dungeon").exists());
    assert!(
        dir.join("worlds/underdark/levels/shaft/level.json")
            .is_file()
    );
    let loaded = load(dir).unwrap();
    assert_eq!(loaded, p);
    assert_eq!(order(&loaded), ["main", "underdark"]);

    // Removing deletes it, files and directory.
    p.apply(Command::RemoveWorld {
        world: s("underdark"),
        confirm: true,
    })
    .unwrap();
    save_dirty(&mut p, dir).unwrap();
    assert!(!dir.join("worlds/underdark").exists());
    assert_eq!(load(dir).unwrap(), p);
}

#[test]
fn load_orders_worlds_by_the_list_and_validation_reports_strays() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let mut p = common::fixture();
    for w in ["zeta", "alpha", "beta"] {
        p.apply(create(w, None)).unwrap();
    }
    p.apply(Command::ReorderWorlds {
        order: vec![s("zeta"), s("main"), s("alpha"), s("beta")],
        restore: false,
    })
    .unwrap();
    save_all(&mut p, dir).unwrap();
    assert_eq!(
        order(&load(dir).unwrap()),
        ["zeta", "main", "alpha", "beta"]
    );

    // Hand-edit the list: drop two worlds, add one that doesn't exist.
    let path = dir.join("project.json");
    let text = fs::read_to_string(&path).unwrap().replace(
        "[\"zeta\", \"main\", \"alpha\", \"beta\"]",
        "[\"main\", \"ghost\", \"zeta\"]",
    );
    fs::write(&path, text).unwrap();
    let q = load(dir).unwrap();
    // Unlisted worlds come after the listed ones, in slug order.
    assert_eq!(order(&q), ["main", "zeta", "alpha", "beta"]);
    let problems: Vec<String> = validate(&q).iter().map(|x| x.to_string()).collect();
    assert!(
        problems.contains(&"project.json: worlds[1]: no world \"ghost\"".to_string()),
        "{problems:?}"
    );
    assert!(
        problems
            .iter()
            .any(|x| x.contains("world \"alpha\" isn't listed")),
        "{problems:?}"
    );
    // Saving keeps what the file said (it's the user's to fix).
    let mut q = q;
    save_all(&mut q, dir).unwrap();
    assert!(
        fs::read_to_string(&path)
            .unwrap()
            .contains("[\"main\", \"ghost\", \"zeta\"]")
    );
}

/// `duplicate_level` into another world lands in that world's space: just
/// right of the original's place, past the levels of that world only.
#[test]
fn duplicate_level_into_another_world() {
    let mut p = common::fixture();
    p.apply(create("dungeon", None)).unwrap();
    p.apply(Command::MoveLevelToWorld {
        level: s("shaft"),
        world: s("dungeon"),
        world_px: None,
        keep_layout: false,
    })
    .unwrap();
    let src = p.level("crevasse").unwrap().clone();
    let g = p.grid_size as i64;
    check(
        &mut p,
        Command::DuplicateLevel {
            level: s("crevasse"),
            slug: s("crevasse_b"),
            name: None,
            world: Some(s("dungeon")),
            world_px: None,
        },
    );
    let copy = p.world("dungeon").unwrap().level("crevasse_b").unwrap();
    assert!(p.world("main").unwrap().level("crevasse_b").is_none());
    assert_eq!(copy.world_px[1], src.world_px[1]);
    assert!(copy.world_px[0] >= src.world_px[0] + src.size[0] as i64 * g);
    // It overlaps no level of the dungeon.
    let r = |l: &Level| {
        [
            l.world_px[0],
            l.world_px[1],
            l.world_px[0] + l.size[0] as i64 * g,
            l.world_px[1] + l.size[1] as i64 * g,
        ]
    };
    let c = r(copy);
    for l in &p.world("dungeon").unwrap().levels {
        if l.slug == "crevasse_b" {
            continue;
        }
        let o = r(l);
        assert!(c[2] <= o[0] || o[2] <= c[0] || c[3] <= o[1] || o[3] <= c[1]);
    }
}
