use std::process::Command;

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_worlds-cli"))
}

#[test]
fn new_schema_validate() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("mygame.worlds");

    let out = cli().arg("new").arg(&dir).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(dir.join("project.json").exists());
    assert!(dir.join("schema/level.json").exists());
    let project = std::fs::read_to_string(dir.join("project.json")).unwrap();
    assert!(project.contains("\"name\": \"mygame\""), "{project}");
    assert!(project.contains("\"grid_size\": 16"), "{project}");

    // `--grid` sets the cell size; 0 is refused.
    let big = tmp.path().join("big.worlds");
    assert!(
        cli()
            .args(["new", "--grid", "32"])
            .arg(&big)
            .status()
            .unwrap()
            .success()
    );
    let p = std::fs::read_to_string(big.join("project.json")).unwrap();
    assert!(p.contains("\"grid_size\": 32"), "{p}");
    let zero = tmp.path().join("zero.worlds");
    assert!(
        !cli()
            .args(["new", "--grid", "0"])
            .arg(&zero)
            .status()
            .unwrap()
            .success()
    );

    // `new` refuses a non-empty directory.
    assert!(!cli().arg("new").arg(&dir).status().unwrap().success());

    std::fs::remove_dir_all(dir.join("schema")).unwrap();
    assert!(cli().arg("schema").arg(&dir).status().unwrap().success());
    assert!(dir.join("schema/rules.json").exists());

    let out = cli().arg("validate").arg(&dir).output().unwrap();
    assert_eq!(out.status.code(), Some(0));

    // A dangling auto-layer source is a problem: exit 1.
    std::fs::write(
        dir.join("layers.json"),
        r#"{ "$schema": "schema/layers.json", "format": 1,
             "layers": [ { "slug": "walls", "name": "Walls", "kind": "auto", "source": "nope" } ] }"#,
    )
    .unwrap();
    let out = cli().arg("validate").arg(&dir).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("layers.json: layers[0].source: unknown layer \"nope\""),
        "{stdout}"
    );

    // A parse error: exit 2 with the file in the message.
    std::fs::write(dir.join("enums.json"), "{}").unwrap();
    let out = cli().arg("validate").arg(&dir).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("enums.json"));
}

#[test]
fn import_ldtk() {
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("sample.worlds");

    let out = cli()
        .arg("import-ldtk")
        .arg(fixtures.join("ldtk-samples/AutoLayers_5_Advanced.ldtk"))
        .arg("--out")
        .arg(&dir)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{stdout}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("renamed: IntGrid_layer -> int_grid_layer"),
        "{stdout}"
    );
    assert!(dir.join("rules/int_grid_layer_tiles.json").exists());
    assert!(dir.join("schema/rules.json").exists());
    assert_eq!(
        cli().arg("validate").arg(&dir).status().unwrap().code(),
        Some(0)
    );

    // Mixed grid sizes import, with per-layer grids, and validate clean.
    let api = tmp.path().join("api.worlds");
    let out = cli()
        .arg("import-ldtk")
        .arg(fixtures.join("ldtk-samples/Test_file_for_API_showing_all_features.ldtk"))
        .arg("--out")
        .arg(&api)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}");
    assert!(stdout.contains("project grid is 8 px"), "{stdout}");
    assert_eq!(
        cli().arg("validate").arg(&api).status().unwrap().code(),
        Some(0)
    );
    assert!(cli().arg("bake").arg(&api).status().unwrap().success());
}

/// Run `validate <dir> <args>` in `cwd`: exit code and stdout + stderr.
fn validate_in(
    cwd: &std::path::Path,
    dir: &std::path::Path,
    args: &[&str],
) -> (Option<i32>, String) {
    let out = cli()
        .current_dir(cwd)
        .arg("validate")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    (
        out.status.code(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

/// `validate --enum-source`: an enum against the game file it mirrors,
/// object keys (`_` keys skipped) or an array of strings at a JSON pointer;
/// `--enum-extra` for values only Worlds has.
#[test]
fn validate_enum_source() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("game.worlds");
    assert!(cli().arg("new").arg(&dir).status().unwrap().success());
    std::fs::write(
        dir.join("enums.json"),
        r#"{ "$schema": "schema/enums.json", "format": 1, "enums": [
             { "slug": "theme", "values": [ { "slug": "none" }, { "slug": "cave" }, { "slug": "mine" } ] },
             { "slug": "music", "values": [ { "slug": "quiet" }, { "slug": "boss" } ] } ] }"#,
    )
    .unwrap();
    std::fs::create_dir(tmp.path().join("data")).unwrap();
    std::fs::write(
        tmp.path().join("data/themes.json"),
        r#"{ "_comment": "not a theme", "cave": {}, "mine": {}, "jungle": {} }"#,
    )
    .unwrap();
    std::fs::write(
        tmp.path().join("data/music.json"),
        r#"{ "tracks": ["quiet", "boss"] }"#,
    )
    .unwrap();
    std::fs::write(tmp.path().join("data/bad.json"), "[1, 2]").unwrap();
    let cwd = tmp.path();
    // Drift both ways, relative to the current directory.
    let (code, out) = validate_in(cwd, &dir, &["--enum-source", "theme=data/themes.json"]);
    assert_eq!(code, Some(1), "{out}");
    assert_eq!(
        out,
        "enums.json: enums[0].values[0]: enum theme has \"none\", which data/themes.json doesn't \
         (enum source drift)\n\
         enums.json: enums[0]: enum theme lacks \"jungle\", which data/themes.json has (enum source \
         drift)\n2 problem(s)\n"
    );
    // --enum-extra allows "none" on the Worlds side only.
    let (code, out) = validate_in(
        cwd,
        &dir,
        &[
            "--enum-source",
            "theme=data/themes.json",
            "--enum-extra",
            "theme=none,other",
        ],
    );
    assert_eq!(code, Some(1), "{out}");
    assert!(!out.contains("\"none\""), "{out}");
    assert!(out.contains("lacks \"jungle\""), "{out}");
    // An array at a pointer, with an absolute path: no drift.
    let music = format!("music={}#/tracks", cwd.join("data/music.json").display());
    let (code, out) = validate_in(cwd, &dir, &["--enum-source", &music]);
    assert_eq!(code, Some(0), "{out}");
    // Usage errors: exit 2.
    for args in [
        &["--enum-source", "nope=data/themes.json"][..],
        &["--enum-source", "theme=data/missing.json"],
        &["--enum-source", "theme=data/themes.json#/cave/x"],
        &["--enum-source", "music=data/bad.json"],
        &["--enum-source", "theme"],
        &["--enum-source", "theme=data/themes.json#cave"],
        &["--enum-source", &music, "--enum-extra", "theme=none"],
        &["--enum-source", &music, "--enum-extra", "music="],
    ] {
        let (code, out) = validate_in(cwd, &dir, args);
        assert_eq!(code, Some(2), "{args:?}: {out}");
    }
}
