//! `worlds-cli bake` warns about a crashed
//! editor session's unrecovered autosave, and says nothing about a live
//! editor's (its session lock is held).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use worlds_core::autosave::{self, EditorLock, FileEntry, FileState, Generation, Manifest};

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_worlds-cli"))
}

fn leave_autosave(dir: &Path) {
    let bytes = b"autosaved\n".to_vec();
    autosave::write(
        dir,
        &Generation {
            manifest: Manifest {
                format: autosave::FORMAT,
                generation: 3,
                written: autosave::now_rfc3339(),
                pid: 777_777,
                rev: 1,
                last_label: None,
                dirty: vec!["project.json".into()],
                files: BTreeMap::from([(
                    "project.json".to_string(),
                    FileEntry {
                        state: FileState::Modified,
                        base: None,
                        hash: Some(autosave::hex(autosave::hash(&bytes))),
                    },
                )]),
            },
            files: BTreeMap::from([(PathBuf::from("project.json"), bytes)]),
        },
    )
    .unwrap();
}

fn bake_stderr(dir: &Path) -> String {
    let out = cli().arg("bake").arg(dir).output().unwrap();
    assert!(out.status.success(), "{out:?}");
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn bake_warns_about_a_crashed_sessions_autosave_only() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("p.worlds");
    let st = cli().arg("new").arg(&dir).status().unwrap();
    assert!(st.success());
    assert!(!bake_stderr(&dir).contains("autosave"));

    leave_autosave(&dir);
    let err = bake_stderr(&dir);
    assert!(
        err.contains("warning: an unrecovered autosave from a Worlds session (pid 777777)"),
        "{err}"
    );
    assert!(err.contains("1 file"), "{err}");

    // A live editor (holding the lock) owns its autosave: no warning.
    let lock = EditorLock::acquire(&dir, "test").unwrap();
    assert!(!bake_stderr(&dir).contains("autosave"));
    drop(lock);
    // The bake left the autosave alone.
    assert!(autosave::autosave_dir(&dir).join("manifest.json").exists());
}
