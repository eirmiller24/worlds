//! `import_project`: a dry run shows the plan, a real one applies it as one
//! undo entry and saves (headless).

mod common;

use common::{engine, err, game_beside, mini, ok};
use serde_json::json;

#[test]
fn dry_run_then_import() {
    let (_t, dir) = mini();
    game_beside(&dir);
    let mut e = engine(&dir, true);
    let args = json!({ "source": "../game.worlds",
        "selection": { "entities": ["lever", "pickup"] },
        "policy": { "on_conflict": "rename" } });
    let mut dry = args.clone();
    dry["dry_run"] = json!(true);
    let plan = ok(&mut e, "import_project", dry);
    assert_eq!(plan["label"], "Import from game");
    assert!(plan["plan_id"].is_string());
    assert!(
        !dir.join("entities/lever.json").exists(),
        "a dry run writes nothing"
    );

    let r = ok(&mut e, "import_project", args.clone());
    assert_eq!(r["applied"], true);
    assert_eq!(r["saved"], true);
    assert_eq!(r["undo_label"], "Agent: import_project from ../game.worlds");
    assert_eq!(r["plan"]["summary"], plan["summary"]);
    assert!(r["plan"].get("plan_id").is_none());
    assert!(dir.join("entities/lever.json").is_file());
    assert!(dir.join("entities/pickup.json").is_file());

    // Again: everything is the same now.
    let msg = err(&mut e, "import_project", args);
    assert!(msg.starts_with("nothing to import: "), "{msg}");

    // One undo takes it back.
    let u = ok(&mut e, "undo", json!({}));
    assert_eq!(
        u["undone"],
        json!(["Agent: import_project from ../game.worlds"])
    );
    assert!(!dir.join("entities/lever.json").exists());
}
