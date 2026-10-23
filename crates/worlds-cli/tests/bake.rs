//! `worlds-cli bake` on an imported LDtk sample with several levels:
//! snapshot of the export, byte-identical rebakes, and `--level`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use worlds_core::{Id, Project};

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_worlds-cli"))
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

/// A fixed id: `prefix` padded with base32 digits of `n` to 10 characters.
fn fixed_id(prefix: &str, mut n: usize) -> Id {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut tail = Vec::new();
    while tail.len() + prefix.len() < Id::LEN {
        tail.push(ALPHABET[n % 32]);
        n /= 32;
    }
    tail.reverse();
    Id::new(format!("{prefix}{}", String::from_utf8(tail).unwrap())).unwrap()
}

/// The import gives the project a random seed and levels and entities
/// random ids (the seed and level ids feed the rule hash). Fix them so the
/// export is reproducible.
fn fix_ids(p: &mut Project) {
    p.seed = 42;
    let mut map: HashMap<String, Id> = HashMap::new();
    let mut n = 0;
    for w in &mut p.worlds {
        for (i, l) in w.levels.iter_mut().enumerate() {
            l.id = fixed_id("level", i);
            for layer in &mut l.entities {
                for e in &mut layer.entities {
                    let id = fixed_id("ent", n);
                    n += 1;
                    map.insert(e.id.to_string(), id.clone());
                    e.id = id;
                }
            }
        }
    }
    let ref_fields: HashMap<String, Vec<String>> = p
        .entities
        .iter()
        .map(|d| {
            let refs = d
                .fields
                .iter()
                .filter(|f| f.ty.is_entity_ref())
                .map(|f| f.slug.to_string())
                .collect();
            (d.slug.to_string(), refs)
        })
        .collect();
    let remap = |v: &mut serde_json::Value| {
        let one = |v: &mut serde_json::Value| {
            if let Some(new) = v.as_str().and_then(|s| map.get(s)) {
                *v = new.to_string().into();
            }
        };
        match v {
            serde_json::Value::Array(a) => a.iter_mut().for_each(one),
            v => one(v),
        }
    };
    for w in &mut p.worlds {
        for l in &mut w.levels {
            for layer in &mut l.entities {
                for e in &mut layer.entities {
                    for f in &ref_fields[e.ty.as_str()] {
                        if let Some(v) = e.fields.get_mut(f.as_str()) {
                            remap(v);
                        }
                    }
                }
            }
        }
    }
    p.normalize();
}

/// `AutoLayers_6_OptionalRules` (4 levels) imported into a temp dir next
/// to a copy of the sample atlases, saved with fixed ids.
fn sample() -> (tempfile::TempDir, PathBuf) {
    let samples = fixtures().join("ldtk-samples");
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("sample.ldtk");
    std::fs::copy(samples.join("AutoLayers_6_OptionalRules.ldtk"), &src).unwrap();
    std::fs::create_dir(tmp.path().join("atlas")).unwrap();
    for e in std::fs::read_dir(samples.join("atlas")).unwrap() {
        let e = e.unwrap();
        std::fs::copy(e.path(), tmp.path().join("atlas").join(e.file_name())).unwrap();
    }
    let dir = tmp.path().join("sample.worlds");
    let mut imported = worlds_core::ldtk::import(&src, &dir).unwrap();
    fix_ids(&mut imported.project);
    worlds_core::ldtk::write(&imported, &dir).unwrap();
    (tmp, dir)
}

fn run(args: &[&std::ffi::OsStr]) -> String {
    let out = cli().args(args).output().unwrap();
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn bake_sample() {
    let (_tmp, dir) = sample();
    let stdout = run(&["bake".as_ref(), dir.as_os_str()]);
    assert!(
        stdout.contains("baked 4 level(s), 5351 autotiles"),
        "{stdout}"
    );

    let export = dir.join("export");
    let world = std::fs::read_to_string(export.join("world.json")).unwrap();
    let level = std::fs::read_to_string(export.join("levels/frozen_caves.json")).unwrap();
    insta::assert_snapshot!("sample_world", world);
    insta::assert_snapshot!("sample_frozen_caves", level);

    // Same input, byte-identical output.
    let read_all = || -> Vec<(PathBuf, Vec<u8>)> {
        let mut files: Vec<PathBuf> = std::fs::read_dir(export.join("levels"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        files.push(export.join("world.json"));
        files.sort();
        files
            .into_iter()
            .map(|f| {
                let b = std::fs::read(&f).unwrap();
                (f, b)
            })
            .collect()
    };
    let first = read_all();
    assert_eq!(first.len(), 5);
    std::fs::remove_dir_all(&export).unwrap();
    run(&["bake".as_ref(), dir.as_os_str()]);
    assert_eq!(read_all(), first);

    // One level into another directory.
    let out = dir.join("one");
    run(&[
        "bake".as_ref(),
        dir.as_os_str(),
        "--level".as_ref(),
        "antique_manor".as_ref(),
        "--out".as_ref(),
        out.as_os_str(),
    ]);
    let levels: Vec<_> = std::fs::read_dir(out.join("levels"))
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(levels, ["antique_manor.json"]);
    assert_eq!(
        std::fs::read(out.join("levels/antique_manor.json")).unwrap(),
        std::fs::read(export.join("levels/antique_manor.json")).unwrap()
    );

    // An unknown level is an error.
    let out = cli()
        .args([
            "bake".as_ref(),
            dir.as_os_str(),
            "--level".as_ref(),
            "nope".as_ref(),
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}
