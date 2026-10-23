#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mini.worlds")
}

pub fn fixture() -> worlds_core::Project {
    worlds_core::load(fixture_dir()).unwrap_or_else(|e| panic!("{e}"))
}

/// Every file under `dir` as (relative path with `/`, contents), sorted.
pub fn read_tree(dir: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            walk(root, &path, out);
        } else {
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            out.insert(rel, std::fs::read_to_string(&path).unwrap());
        }
    }
}
