//! Autosave and crash recovery, end to end with the `worlds` binary on
//! scratch copies of the `game` fixture (`tests/fixtures/game/`; its
//! `../assets` is linked read-only). Needs a GPU adapter for the
//! headless editor: these run with `WORLDS_GPU_TESTS=1` (the perf
//! measurements of the autosave render run without).
//!
//! Each kill test compares the whole recovered tree (deletions included)
//! with a control copy that received the same live API edits and was saved.

use std::collections::BTreeMap;
use std::io::BufRead as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use worlds_core::autosave::{self, Inspect};

fn gpu_tests() -> bool {
    let on = std::env::var("WORLDS_GPU_TESTS").as_deref() == Ok("1");
    if !on {
        eprintln!("skipped: set WORLDS_GPU_TESTS=1 to run");
    }
    on
}

/// The `game` fixture project (read only: tests work on copies).
fn game() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/game/game.worlds")
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let p = e.path();
        if e.file_name() == ".worlds" || e.file_name() == ".git" {
            continue;
        }
        if e.file_type().unwrap().is_dir() {
            copy_dir(&p, &to.join(e.file_name()));
        } else {
            std::fs::copy(&p, to.join(e.file_name())).unwrap();
        }
    }
}

/// A copy of the game fixture in a tempdir, `assets` linked beside it
/// (read only: nothing here writes there).
fn copy_of_game(tmp: &Path, name: &str) -> PathBuf {
    let root = tmp.join(name);
    let dir = root.join("game.worlds");
    copy_dir(&game(), &dir);
    if let Some(parent) = game().parent() {
        let assets = parent.join("assets");
        if assets.is_dir() {
            #[cfg(unix)]
            std::os::unix::fs::symlink(&assets, root.join("assets")).unwrap();
            #[cfg(not(unix))]
            copy_dir(&assets, &root.join("assets"));
        }
    }
    dir
}

/// Every file (bytes) and directory under `dir` but `.worlds/`.
fn tree(dir: &Path) -> BTreeMap<String, Option<Vec<u8>>> {
    fn walk(root: &Path, d: &Path, out: &mut BTreeMap<String, Option<Vec<u8>>>) {
        for e in std::fs::read_dir(d).unwrap() {
            let p = e.unwrap().path();
            let rel = p.strip_prefix(root).unwrap().to_string_lossy().into_owned();
            if rel == ".worlds" {
                continue;
            }
            if p.is_dir() {
                out.insert(format!("{rel}/"), None);
                walk(root, &p, out);
            } else {
                out.insert(rel, Some(std::fs::read(&p).unwrap()));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

fn differences(
    a: &BTreeMap<String, Option<Vec<u8>>>,
    b: &BTreeMap<String, Option<Vec<u8>>>,
) -> Vec<String> {
    let mut out: Vec<String> = a
        .iter()
        .filter(|(k, v)| b.get(*k) != Some(v))
        .map(|(k, _)| k.clone())
        .collect();
    out.extend(b.keys().filter(|k| !a.contains_key(*k)).cloned());
    out.sort();
    out.dedup();
    out
}

/// A PNG path for a screenshot nobody looks at.
fn shot() -> String {
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    std::env::temp_dir()
        .join(format!(
            "worlds-autosave-test-{}-{n}.png",
            std::process::id()
        ))
        .to_string_lossy()
        .into_owned()
}

fn worlds() -> Command {
    Command::new(env!("CARGO_BIN_EXE_worlds"))
}

/// `worlds <args>`: (exit code, stderr).
fn run(args: &[&str], dir: &Path, env: &[(&str, &str)]) -> (i32, String) {
    let mut c = worlds();
    c.args(args).arg(dir);
    for (k, v) in env {
        c.env(k, v);
    }
    let out = c.output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// A headless `--serve` editor on a project.
struct Served {
    child: Child,
    dir: PathBuf,
    rest: Option<std::thread::JoinHandle<String>>,
}

impl Served {
    fn start(dir: &Path, extra: &[&str], env: &[(&str, &str)]) -> Served {
        let shot = dir.parent().unwrap().join("served.png");
        let mut c = worlds();
        c.arg("--screenshot")
            .arg(&shot)
            .args(["--size", "400x300", "--serve"])
            .args(extra)
            .arg(dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        for (k, v) in env {
            c.env(k, v);
        }
        let mut child = c.spawn().unwrap();
        let mut out = std::io::BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        out.read_line(&mut line).unwrap();
        assert!(line.contains("\"port\""), "no port line: {line:?}");
        let rest = std::thread::spawn(move || {
            let mut s = String::new();
            let _ = std::io::Read::read_to_string(&mut out, &mut s);
            s
        });
        Served {
            child,
            dir: dir.to_path_buf(),
            rest: Some(rest),
        }
    }

    fn rpc(&self, calls: &[(&str, Value)]) -> Vec<Value> {
        rpc(&self.dir, calls)
    }

    fn signal(&mut self, sig: &str) -> std::process::ExitStatus {
        let ok = Command::new("kill")
            .args([sig, &self.child.id().to_string()])
            .status()
            .unwrap();
        assert!(ok.success());
        let t = Instant::now();
        loop {
            if let Some(s) = self.child.try_wait().unwrap() {
                if let Some(r) = self.rest.take() {
                    let _ = r.join();
                }
                return s;
            }
            assert!(t.elapsed() < Duration::from_secs(30), "still running");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Served {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Call the live API of the editor serving `dir` (hello first).
fn rpc(dir: &Path, calls: &[(&str, Value)]) -> Vec<Value> {
    try_rpc(dir, calls).unwrap_or_else(|e| panic!("live API: {e}"))
}

/// [`rpc`], with a broken connection (a killed editor) as an error.
fn try_rpc(dir: &Path, calls: &[(&str, Value)]) -> Result<Vec<Value>, String> {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    let text =
        std::fs::read_to_string(dir.join(".worlds/server.json")).map_err(|e| e.to_string())?;
    let disc: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let port = disc["port"].as_u64().ok_or("no port")?;
    let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    rt.block_on(async {
        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/"))
            .await
            .map_err(|e| e.to_string())?;
        let mut id = 0;
        let mut call = async |ws: &mut tokio_tungstenite::WebSocketStream<_>,
                              method: &str,
                              params: Value|
               -> Result<Value, String> {
            id += 1;
            ws.send(Message::Text(
                json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
                    .to_string()
                    .into(),
            ))
            .await
            .map_err(|e| e.to_string())?;
            loop {
                let m = tokio::time::timeout(Duration::from_secs(20), ws.next())
                    .await
                    .map_err(|_| "no answer within 20 s".to_string())?
                    .ok_or("closed")?
                    .map_err(|e| e.to_string())?;
                if let Message::Text(t) = m {
                    let v: Value = serde_json::from_str(&t).map_err(|e| e.to_string())?;
                    if v["id"] == id {
                        return Ok(v);
                    }
                }
            }
        };
        let hello = call(
            &mut ws,
            "session.hello",
            json!({ "token": disc["token"], "client": { "name": "autosave-test", "version": "1" } }),
        )
        .await?;
        if hello.get("result").is_none() {
            return Err(format!("hello: {hello}"));
        }
        let mut out = Vec::new();
        for (m, p) in calls {
            out.push(call(&mut ws, m, p.clone()).await?);
        }
        Ok(out)
    })
}

/// The edits of the kill tests: a wall painted, an entity placed, a rule
/// removed, a level deleted, an entity type deleted.
fn edits() -> Vec<(&'static str, Value)> {
    vec![
        (
            "grid.fill_rect",
            json!({ "level": "crevasse", "layer": "collision", "rect": [28, 12, 2, 5], "value": "solid" }),
        ),
        (
            "entity.add",
            json!({ "level": "crevasse", "entity": { "id": "torchtst22", "type": "torch", "cell": [5, 5] } }),
        ),
        (
            "rules.remove_rule",
            json!({ "layer": "main_tiling", "rule": "hazard" }),
        ),
        (
            "level.delete",
            json!({ "level": "level_4", "confirm": true }),
        ),
        (
            "defs.remove",
            json!({ "kind": "entity", "slug": "gate", "confirm": true }),
        ),
    ]
}

fn apply_edits(s: &Served) {
    for r in s.rpc(&edits()) {
        assert_eq!(r["result"]["status"], "applied", "{r}");
    }
}

/// What the files of the session's next save would hold, as the live API
/// renders them (`project.read_file`).
fn rendered(s: &Served, paths: &[&str]) -> BTreeMap<String, Vec<u8>> {
    let calls: Vec<(&str, Value)> = paths
        .iter()
        .map(|p| ("project.read_file", json!({ "path": p })))
        .collect();
    paths
        .iter()
        .zip(s.rpc(&calls))
        .map(|(p, r)| {
            let text = r["result"]["text"]
                .as_str()
                .unwrap_or_else(|| panic!("{p}: {r}"));
            (p.to_string(), text.as_bytes().to_vec())
        })
        .collect()
}

/// Wait until the autosave on disk holds exactly `want` for those files.
fn wait_for_autosave(dir: &Path, want: &BTreeMap<String, Vec<u8>>) -> autosave::Manifest {
    let t = Instant::now();
    loop {
        if let Ok(Some(m)) = autosave::read_manifest(dir) {
            let g = autosave::autosave_dir(dir).join(format!("g{}", m.generation));
            let all = want.iter().all(|(p, bytes)| {
                m.files.get(p).and_then(|e| e.hash.clone())
                    == Some(autosave::hex(autosave::hash(bytes)))
                    && std::fs::read(g.join(p)).ok().as_ref() == Some(bytes)
            });
            if all {
                return m;
            }
        }
        assert!(
            t.elapsed() < Duration::from_secs(30),
            "no autosave of the edits"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

const MODIFIED: [&str; 3] = [
    "worlds/main/levels/crevasse/collision.json",
    "worlds/main/levels/crevasse/entities.json",
    "rules/main_tiling.json",
];

/// The control: the same edits on a fresh copy, saved through the live API.
fn control(tmp: &Path) -> BTreeMap<String, Option<Vec<u8>>> {
    let dir = copy_of_game(tmp, "control");
    let s = Served::start(&dir, &[], &[]);
    apply_edits(&s);
    let r = s.rpc(&[("project.save", json!({}))]);
    assert!(r[0].get("result").is_some(), "{r:?}");
    drop(s);
    tree(&dir)
}

/// Serve `dir` with autosave every second, make the edits, wait for their
/// autosave, `kill -9`. Returns the manifest.
fn crash_with_edits(dir: &Path) -> autosave::Manifest {
    let mut s = Served::start(dir, &["--autosave", "1"], &[]);
    apply_edits(&s);
    let want = rendered(&s, &MODIFIED);
    let m = wait_for_autosave(dir, &want);
    let st = s.signal("-KILL");
    assert!(!st.success());
    m
}

fn lock_is_free(dir: &Path) -> bool {
    let f = std::fs::OpenOptions::new()
        .write(true)
        .open(dir.join(".worlds/editor.lock"))
        .unwrap();
    f.try_lock().is_ok()
}

/// kill -9 after an autosave, then `--recover apply --save`
/// equals the control tree, whole tree, deletions included. A second
/// editor on the project while the first runs exits 2.
#[test]
fn kill_after_autosave_recovers_to_the_saved_tree() {
    if !gpu_tests() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let want = control(tmp.path());
    let dir = copy_of_game(tmp.path(), "crashed");
    let pristine = tree(&dir);

    // a second editing session is refused while one runs.
    {
        let s = Served::start(&dir, &[], &[]);
        let (code, err) = run(&["--screenshot", &shot(), "--save"], &dir, &[]);
        assert_eq!(code, 2, "{err}");
        assert!(err.contains("open in another Worlds"), "{err}");
        drop(s);
    }

    let m = crash_with_edits(&dir);
    assert_eq!(tree(&dir), pristine, "nothing was saved before the kill");
    assert!(lock_is_free(&dir), "the dead editor's lock was released");
    let mut dirty = m.dirty.clone();
    dirty.sort();
    assert_eq!(
        dirty,
        [
            "entities/gate.json",
            "rules/main_tiling.json",
            "worlds/main/levels/crevasse/collision.json",
            "worlds/main/levels/crevasse/entities.json",
            "worlds/main/levels/level_4",
        ]
    );
    let deleted: Vec<&String> = m
        .files
        .iter()
        .filter(|(_, e)| e.state == autosave::FileState::Deleted)
        .map(|(p, _)| p)
        .collect();
    assert!(
        deleted.contains(&&"entities/gate.json".to_string()),
        "{deleted:?}"
    );
    assert!(
        deleted
            .iter()
            .any(|p| p.starts_with("worlds/main/levels/level_4/")),
        "{deleted:?}"
    );

    // A session that starts from a recovery says so in project.describe
    // (killed again: the adopted autosave stays for the run below).
    {
        let s = Served::start(&dir, &["--recover", "apply"], &[]);
        let d = s.rpc(&[("project.describe", json!({}))]);
        let rec = &d[0]["result"]["recovered"];
        assert_eq!(
            rec["files"].as_array().map(Vec::len),
            Some(m.files.len()),
            "{rec}"
        );
        assert_eq!(rec["skipped"], json!([]));
        drop(s);
    }
    assert!(autosave::autosave_dir(&dir).join("manifest.json").exists());

    let report = tmp.path().join("report.json");
    let empty = tmp.path().join("empty.json");
    std::fs::write(&empty, "[]").unwrap();
    let (code, err) = run(
        &[
            "--screenshot",
            tmp.path().join("y.png").to_str().unwrap(),
            "--recover",
            "apply",
            "--recover-report",
            report.to_str().unwrap(),
            "--script",
            empty.to_str().unwrap(),
            "--save",
        ],
        &dir,
        &[],
    );
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("Recovered"), "{err}");
    let got = tree(&dir);
    assert!(
        got == want,
        "recovered tree differs from the control in {:?}",
        differences(&got, &want)
    );
    assert!(
        !autosave::autosave_dir(&dir).exists(),
        "autosave left after the save"
    );
    let r: Value = serde_json::from_str(&std::fs::read_to_string(&report).unwrap()).unwrap();
    assert_eq!(r["found"], true);
    assert_eq!(
        r["applied"].as_array().unwrap().len(),
        r["files"].as_array().unwrap().len()
    );
    assert!(
        r["files"][0]["diff"]
            .as_str()
            .unwrap()
            .starts_with("--- disk/"),
        "{r}"
    );
    // The next open finds nothing.
    let (code, _) = run(
        &[
            "--screenshot",
            &shot(),
            "--recover-report",
            report.to_str().unwrap(),
            "--recover",
            "keep",
        ],
        &dir,
        &[],
    );
    assert_eq!(code, 0);
    let r: Value = serde_json::from_str(&std::fs::read_to_string(&report).unwrap()).unwrap();
    assert_eq!(r, json!({ "found": false }));
}

/// a file changed on disk after the crash is a conflict: `keep`
/// lists it, `apply` skips it (disk wins, the old autosave kept in
/// `autosave.discarded/`), `--recover-conflicts` applies it.
#[test]
fn conflicts_are_listed_skipped_or_applied() {
    if !gpu_tests() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let want = control(tmp.path());
    let dir = copy_of_game(tmp.path(), "conflict");
    crash_with_edits(&dir);
    let coll = dir.join("worlds/main/levels/crevasse/collision.json");
    let theirs = std::fs::read_to_string(&coll)
        .unwrap()
        .replacen("\"#..", "\"##.", 1);
    std::fs::write(&coll, &theirs).unwrap();
    let saved = tmp.path().join("autosave.copy");
    copy_dir(&autosave::autosave_dir(&dir), &saved);

    let report = tmp.path().join("r.json");
    let read =
        |p: &Path| -> Value { serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap() };
    let (code, err) = run(
        &[
            "--screenshot",
            &shot(),
            "--recover",
            "keep",
            "--recover-report",
            report.to_str().unwrap(),
        ],
        &dir,
        &[],
    );
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("not applied"), "{err}");
    let r = read(&report);
    let files = r["files"].as_array().unwrap();
    let conflict: Vec<&str> = files
        .iter()
        .filter(|f| f["conflict"] == true)
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    assert_eq!(conflict, ["worlds/main/levels/crevasse/collision.json"]);
    assert!(
        autosave::autosave_dir(&dir).join("manifest.json").exists(),
        "keep keeps it"
    );

    // apply: the conflict is skipped, the rest recovered.
    let (code, err) = run(
        &["--screenshot", &shot(), "--recover", "apply", "--save"],
        &dir,
        &[],
    );
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("1 conflicting file skipped"), "{err}");
    assert_eq!(std::fs::read_to_string(&coll).unwrap(), theirs, "disk wins");
    let got = tree(&dir);
    let diff = differences(&got, &want);
    assert_eq!(
        diff,
        ["worlds/main/levels/crevasse/collision.json"],
        "only the conflict differs"
    );
    assert!(
        autosave::discarded_dir(&dir).join("manifest.json").exists(),
        "the skipped edits are kept"
    );

    // --recover-conflicts: the autosave wins.
    std::fs::write(&coll, &theirs).unwrap();
    let dir2 = copy_of_game(tmp.path(), "conflict2");
    // The same crash state: pristine files, the conflicting collision.json,
    // the saved autosave.
    std::fs::write(
        dir2.join("worlds/main/levels/crevasse/collision.json"),
        &theirs,
    )
    .unwrap();
    copy_dir(&saved, &autosave::autosave_dir(&dir2));
    let (code, err) = run(
        &[
            "--screenshot",
            &shot(),
            "--recover",
            "apply",
            "--recover-conflicts",
            "--save",
        ],
        &dir2,
        &[],
    );
    assert_eq!(code, 0, "{err}");
    let got = tree(&dir2);
    assert!(got == want, "differs in {:?}", differences(&got, &want));
}

/// save then kill: nothing to recover. SIGTERM with unsaved edits
/// (autosave interval far away): autosaved on the way out, exit 0, and
/// recoverable. Close-prompt Discard leaves no autosave (in-process test
/// below).
#[test]
fn clean_paths_and_sigterm() {
    if !gpu_tests() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    // Save, then kill -9.
    let dir = copy_of_game(tmp.path(), "saved");
    let mut s = Served::start(&dir, &["--autosave", "1"], &[]);
    apply_edits(&s);
    let want = rendered(&s, &MODIFIED);
    wait_for_autosave(&dir, &want);
    s.rpc(&[("project.save", json!({}))]);
    let t = Instant::now();
    while autosave::autosave_dir(&dir).exists() {
        assert!(
            t.elapsed() < Duration::from_secs(10),
            "autosave not removed after save"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    s.signal("-KILL");
    assert!(matches!(autosave::inspect(&dir), Inspect::None));
    assert_eq!(autosave::pending(&dir), autosave::Pending::None);

    // SIGTERM with unsaved edits.
    let dir = copy_of_game(tmp.path(), "term");
    let mut s = Served::start(&dir, &["--autosave", "3600"], &[]);
    apply_edits(&s);
    let want = rendered(&s, &MODIFIED);
    assert!(
        !autosave::autosave_dir(&dir).exists(),
        "too early for the interval"
    );
    let st = s.signal("-TERM");
    assert!(st.success(), "{st:?}");
    let m = wait_for_autosave(&dir, &want);
    assert!(m.dirty.len() >= 5, "{:?}", m.dirty);
    assert!(lock_is_free(&dir));
    assert!(matches!(
        autosave::pending(&dir),
        autosave::Pending::Crashed(_)
    ));
    let (code, err) = run(
        &["--screenshot", &shot(), "--recover", "apply", "--save"],
        &dir,
        &[],
    );
    assert_eq!(code, 0, "{err}");
    let ctl = control(tmp.path());
    let got = tree(&dir);
    assert!(got == ctl, "differs in {:?}", differences(&got, &ctl));
}

/// kill -9 at random moments while autosave writes as often as it
/// can (`WORLDS_AUTOSAVE_SECS=0.05`) and an agent paints: afterwards the
/// manifest is absent or valid (every listed file present with its hash),
/// never torn. 40 runs, fixed seed.
#[test]
fn kill_during_autosave_writes_never_tears_the_manifest() {
    if !gpu_tests() {
        return;
    }
    let runs: usize = std::env::var("WORLDS_KILL_RUNS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(40);
    let tmp = tempfile::tempdir().unwrap();
    let dir = copy_of_game(tmp.path(), "loop");
    let mut seed: u64 = 0x5eed_1234;
    let mut next = move || {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (seed >> 33) as u32
    };
    let (mut valid, mut absent) = (0, 0);
    for i in 0..runs {
        // Each run first discards what the last one left.
        let s = Served::start(
            &dir,
            &["--recover", "discard"],
            &[("WORLDS_AUTOSAVE_SECS", "0.05")],
        );
        let d = dir.clone();
        let painter = std::thread::spawn(move || {
            for k in 0..200u32 {
                let x = 2 + (k * 7 + i as u32) % 50;
                let v = if k % 2 == 0 { "solid" } else { "water" };
                let calls = [(
                    "grid.fill_rect",
                    json!({ "level": "crevasse", "layer": "collision", "rect": [x, 14, 1, 2], "value": v }),
                )];
                if try_rpc(&d, &calls).is_err() {
                    return;
                }
            }
        });
        std::thread::sleep(Duration::from_millis(300 + u64::from(next() % 1200)));
        let mut s = s;
        s.signal("-KILL");
        let _ = painter.join();
        match autosave::inspect(&dir) {
            Inspect::Broken(m) | Inspect::DiskUnreadable(m) => {
                panic!("run {i}: torn autosave: {m}")
            }
            Inspect::None => absent += 1,
            Inspect::Found(_) => valid += 1,
        }
    }
    eprintln!("{runs} kills: {valid} valid autosaves, {absent} without one");
    assert!(
        valid > runs / 2,
        "the loop hardly ever autosaved: {valid}/{runs}"
    );
}

/// The autosave render (the part on the UI thread) on the game fixture after
/// one stroke, and on a 500-level synthetic world with 50 dirty levels.
/// Prints p50/p95/max; fails over the 8 ms UI budget in a release build
/// (a debug build gets 5x that). No GPU needed.
#[test]
fn autosave_render_fits_in_a_frame() {
    let dir = game();
    let stats = |mut v: Vec<f64>| {
        v.sort_by(f64::total_cmp);
        (v[v.len() / 2], v[v.len() * 95 / 100], v[v.len() - 1])
    };
    let time = |p: &worlds_core::Project, n: usize| -> Vec<f64> {
        (0..n)
            .map(|_| {
                let t = Instant::now();
                let r = worlds_core::format::render_dirty(p).unwrap();
                let g = autosave::generation(p, r, 1, 1, None);
                std::hint::black_box(&g);
                t.elapsed().as_secs_f64() * 1e3
            })
            .collect()
    };
    let mut p = worlds_core::load(&dir).unwrap();
    let paint: worlds_core::Command = serde_json::from_value(json!({
        "op": "paint_cells", "level": "crevasse", "layer": "collision",
        "cells": [[30, 14, "solid"], [31, 14, "solid"], [32, 14, "solid"]] }))
    .unwrap();
    p.apply(paint).unwrap();
    let budget = if cfg!(debug_assertions) { 40.0 } else { 8.0 };
    let (p50, p95, max) = stats(time(&p, 200));
    eprintln!("autosave render, one stroke: p50 {p50:.3} ms  p95 {p95:.3}  max {max:.3}");
    assert!(p95 < budget, "p95 {p95:.2} ms");

    let mut big = worlds_core::load(&dir).unwrap();
    worlds_editor::doc::synthesize(&mut big, 500);
    let levels: Vec<String> = big.levels().map(|l| l.slug.to_string()).take(50).collect();
    assert_eq!(levels.len(), 50);
    for l in &levels {
        let c: worlds_core::Command = serde_json::from_value(json!({
            "op": "paint_cells", "level": l, "layer": "collision", "cells": [[1, 1, "solid"]] }))
        .unwrap();
        big.apply(c).unwrap();
    }
    assert_eq!(big.files.dirty().len(), 50);
    let (p50, p95, max) = stats(time(&big, 60));
    eprintln!(
        "autosave render, synthetic 500 levels, 50 dirty: p50 {p50:.3} ms  p95 {p95:.3}  max {max:.3}"
    );
    assert!(p95 < budget, "synthetic p95 {p95:.2} ms");
}

/// Edits for the in-process tests, as commands: a wall and a deleted level.
fn crash_commands() -> Vec<worlds_core::Command> {
    [
        json!({ "op": "fill_rect", "level": "crevasse", "layer": "collision", "rect": [28, 12, 2, 5], "value": "solid" }),
        json!({ "op": "delete_level", "world": "main", "level": "level_4" }),
    ]
    .into_iter()
    .map(|v| serde_json::from_value(v).unwrap())
    .collect()
}

/// Leave the autosave a session with [`crash_commands`] would have, with
/// no editor holding the lock (a crash). Returns the saved control tree.
fn leave_crash(tmp: &Path, dir: &Path) -> BTreeMap<String, Option<Vec<u8>>> {
    let mut p = worlds_core::load(dir).unwrap();
    for c in crash_commands() {
        p.apply(c).unwrap();
    }
    let r = worlds_core::format::render_dirty(&p).unwrap();
    autosave::write(
        dir,
        &autosave::generation(&p, r, 1, 2, Some("Delete level".into())),
    )
    .unwrap();
    let ctl = copy_of_game(tmp, &format!("ctl{}", std::process::id()));
    let mut q = worlds_core::load(&ctl).unwrap();
    for c in crash_commands() {
        q.apply(c).unwrap();
    }
    worlds_core::save_dirty(&mut q, &ctl).unwrap();
    let t = tree(&ctl);
    std::fs::remove_dir_all(ctl.parent().unwrap()).unwrap();
    t
}

fn editor_on(
    dir: &Path,
    recover: worlds_editor::app::recovery::RecoverMode,
) -> (
    worlds_editor::headless::Headless,
    egui::Context,
    worlds_editor::app::EditorApp,
) {
    use worlds_editor::app::{AppOptions, EditorApp};
    let hl = worlds_editor::headless::Headless::new([1600, 900]).unwrap();
    let ctx = egui::Context::default();
    let mut app = EditorApp::new(
        &ctx,
        hl.gpu.clone(),
        worlds_editor::log::init(),
        AppOptions {
            headless: true,
            edits: true,
            autosave: Some(Duration::from_secs(3600)),
            recover: Some(recover),
            ..Default::default()
        },
    );
    app.set_commands(Box::new(worlds_editor::edit::Editing::new()));
    app.open(dir);
    (hl, ctx, app)
}

/// The recovery prompt in the app: Later blocks autosave and asking again
/// brings the prompt back; Recover loads the edits unsaved with an empty
/// history, and saving gives the control tree and removes the autosave;
/// the close prompt's Discard removes the session's own autosave.
#[test]
fn the_prompt_waits_recovers_and_discards() {
    use worlds_editor::app::recovery::{Choice, RecoverMode};
    use worlds_editor::autosave::Blocked;
    if !gpu_tests() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let dir = copy_of_game(tmp.path(), "prompt");
    let want = leave_crash(tmp.path(), &dir);
    let pristine = tree(&dir);
    let (hl, ctx, mut app) = editor_on(&dir, RecoverMode::Ask);
    assert!(app.recovery_pending());
    assert_eq!(app.autosaver.blocked, Some(Blocked::Undecided));
    let mut t = 0.0;
    for _ in 0..4 {
        hl.frame(&ctx, &mut app, t);
        t += 1.0 / 60.0;
    }
    let img = hl.read().unwrap();
    let png = tmp.path().join("m7_recovery_prompt_headless.png");
    img.save(&png).unwrap();
    if let Some(out) = std::env::var_os("WORLDS_TEST_OUT") {
        std::fs::create_dir_all(&out).unwrap();
        std::fs::copy(&png, Path::new(&out).join(png.file_name().unwrap())).unwrap();
    }

    // Later: nothing changes, autosave stays off; asking again re-opens.
    app.recovery_choose(Choice::Later).unwrap();
    assert!(!app.recovery_pending());
    assert_eq!(app.autosaver.blocked, Some(Blocked::Later));
    assert!(!app.autosaver.active());
    assert!(!app.has_unsaved());
    app.reask_recovery();
    assert!(app.recovery_pending());

    // Recover.
    app.recovery_choose(Choice::Recover).unwrap();
    assert!(app.has_unsaved());
    assert_eq!(app.undo_entries(), 0);
    assert!(app.autosaver.active());
    let files = app.recovered().unwrap()["files"].as_array().unwrap().len();
    assert!(files >= 4, "{:?}", app.recovered());
    assert_eq!(tree(&dir), pristine, "nothing written before a save");
    hl.frame(&ctx, &mut app, t);
    app.save().unwrap();
    hl.frame(&ctx, &mut app, t + 0.1);
    app.shutdown();
    let got = tree(&dir);
    assert!(got == want, "differs in {:?}", differences(&got, &want));
    assert!(!autosave::autosave_dir(&dir).exists());
    drop(app);

    // Discard in the close prompt: this session's autosave goes.
    let (_hl, _ctx, mut app) = editor_on(&dir, RecoverMode::Keep);
    app.apply_command(crash_commands().remove(0)).ok();
    let paint: worlds_core::Command = serde_json::from_value(json!({
        "op": "paint_cells", "level": "crevasse", "layer": "collision", "cells": [[3, 14, "water"]] }))
    .unwrap();
    app.apply_command(paint).unwrap();
    app.autosave_now();
    assert!(app.autosaver.flush(Duration::from_secs(5)));
    assert!(autosave::autosave_dir(&dir).join("manifest.json").exists());
    app.discard_and_close();
    app.shutdown();
    assert!(
        !autosave::autosave_dir(&dir).exists(),
        "Discard left the autosave"
    );
}

/// An Xvfb server of our own (so `import` can screenshot its root window).
struct Xvfb {
    child: Child,
    display: String,
    /// `LD_LIBRARY_PATH` for winit's `libxkbcommon-x11` (see
    /// `window_exits_cleanly_on_signals_under_xvfb`).
    ld: Option<PathBuf>,
}

impl Xvfb {
    fn start() -> Xvfb {
        let n = (40..90)
            .map(|k| k + std::process::id() % 50)
            .find(|n| !Path::new(&format!("/tmp/.X{n}-lock")).exists())
            .expect("a free X display");
        let display = format!(":{n}");
        let child = Command::new("Xvfb")
            .args([&display, "-screen", "0", "1600x960x24", "-nolisten", "tcp"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("Xvfb");
        let t = Instant::now();
        while !Path::new(&format!("/tmp/.X11-unix/X{n}")).exists() {
            assert!(t.elapsed() < Duration::from_secs(10), "Xvfb didn't start");
            std::thread::sleep(Duration::from_millis(50));
        }
        let ld = std::env::var_os("WORLDS_XKB_DIR").map(PathBuf::from);
        Xvfb { child, display, ld }
    }

    /// The window on `dir` with `args`; returns once its live API is up
    /// (a `server.json` with its own pid).
    fn window(&self, dir: &Path, args: &[&str]) -> Child {
        let _ = std::fs::remove_file(dir.join(".worlds/server.json"));
        let mut c = worlds();
        c.args(args)
            .arg(dir)
            .env("DISPLAY", &self.display)
            .env_remove("WAYLAND_DISPLAY")
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(ld) = &self.ld {
            c.env("LD_LIBRARY_PATH", ld);
        }
        let mut child = c.spawn().unwrap();
        let t = Instant::now();
        loop {
            if let Ok(text) = std::fs::read_to_string(dir.join(".worlds/server.json"))
                && let Ok(v) = serde_json::from_str::<Value>(&text)
                && v["pid"].as_u64() == Some(u64::from(child.id()))
            {
                break;
            }
            if let Some(s) = child.try_wait().unwrap() {
                panic!("the window exited early: {s:?}");
            }
            assert!(t.elapsed() < Duration::from_secs(90), "no window");
            std::thread::sleep(Duration::from_millis(100));
        }
        // A few frames in.
        std::thread::sleep(Duration::from_secs(2));
        child
    }

    fn screenshot(&self, png: &Path) {
        let st = Command::new("import")
            .args(["-display", &self.display, "-window", "root"])
            .arg(png)
            .status()
            .expect("ImageMagick import");
        assert!(st.success());
    }
}

impl Drop for Xvfb {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn stop(child: &mut Child, sig: &str) -> std::process::ExitStatus {
    Command::new("kill")
        .args([sig, &child.id().to_string()])
        .status()
        .unwrap();
    let t = Instant::now();
    loop {
        if let Some(s) = child.try_wait().unwrap() {
            return s;
        }
        assert!(
            t.elapsed() < Duration::from_secs(30),
            "still running after {sig}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// the window under Xvfb: edits over the live API, autosave, kill -9;
/// relaunched, the window shows the recovery prompt (screenshot
/// `m7_recovery_prompt.png` to `WORLDS_TEST_OUT`); SIGTERM while undecided
/// keeps the autosave; relaunched with `--recover apply`, a save over the
/// API gives the control tree.
#[test]
fn window_offers_recovery_after_a_kill() {
    if !gpu_tests() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let want = control(tmp.path());
    let dir = copy_of_game(tmp.path(), "window");
    let x = Xvfb::start();

    let mut w = x.window(&dir, &["--autosave", "1"]);
    for r in rpc(&dir, &edits()) {
        assert_eq!(r["result"]["status"], "applied", "{r}");
    }
    let calls: Vec<(&str, Value)> = MODIFIED
        .iter()
        .map(|p| ("project.read_file", json!({ "path": p })))
        .collect();
    let rendered: BTreeMap<String, Vec<u8>> = MODIFIED
        .iter()
        .zip(rpc(&dir, &calls))
        .map(|(p, r)| {
            (
                p.to_string(),
                r["result"]["text"].as_str().unwrap().as_bytes().to_vec(),
            )
        })
        .collect();
    wait_for_autosave(&dir, &rendered);
    assert!(!stop(&mut w, "-KILL").success());

    let mut w = x.window(&dir, &[]);
    let png = tmp.path().join("m7_recovery_prompt.png");
    x.screenshot(&png);
    let img = image::open(&png).unwrap().to_rgba8();
    assert!(
        worlds_editor::headless::non_blank_fraction(&img) > 0.2,
        "blank window screenshot"
    );
    if let Some(out) = std::env::var_os("WORLDS_TEST_OUT") {
        std::fs::create_dir_all(&out).unwrap();
        std::fs::copy(&png, Path::new(&out).join("m7_recovery_prompt.png")).unwrap();
    }
    assert!(stop(&mut w, "-TERM").success());
    assert!(
        autosave::autosave_dir(&dir).join("manifest.json").exists(),
        "an undecided autosave must survive the session"
    );

    let mut w = x.window(&dir, &["--recover", "apply"]);
    // A second window on the same project: the "open in another Worlds"
    // message, no project, no server; it stays up until Quit.
    let mut c = worlds();
    c.arg(&dir)
        .env("DISPLAY", &x.display)
        .env_remove("WAYLAND_DISPLAY")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(ld) = &x.ld {
        c.env("LD_LIBRARY_PATH", ld);
    }
    let mut second = c.spawn().unwrap();
    std::thread::sleep(Duration::from_secs(4));
    assert!(
        second.try_wait().unwrap().is_none(),
        "the second window died"
    );
    let png = tmp.path().join("m7_already_open.png");
    x.screenshot(&png);
    if let Some(out) = std::env::var_os("WORLDS_TEST_OUT") {
        std::fs::copy(&png, Path::new(&out).join("m7_already_open.png")).unwrap();
    }
    stop(&mut second, "-TERM");
    let r = rpc(&dir, &[("project.save", json!({}))]);
    assert!(r[0].get("result").is_some(), "{r:?}");
    assert!(stop(&mut w, "-TERM").success());
    let got = tree(&dir);
    assert!(got == want, "differs in {:?}", differences(&got, &want));
    assert!(!autosave::autosave_dir(&dir).exists());
}

/// Recover replaces the open document, so it refuses while there are edits
/// made after the prompt came up (they would be lost).
#[test]
fn recover_refuses_over_newer_edits() {
    use worlds_editor::app::recovery::{Choice, RecoverMode};
    if !gpu_tests() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let dir = copy_of_game(tmp.path(), "newer");
    leave_crash(tmp.path(), &dir);
    let (_hl, _ctx, mut app) = editor_on(&dir, RecoverMode::Ask);
    assert!(app.recovery_pending());
    let paint: worlds_core::Command = serde_json::from_value(json!({
        "op": "paint_cells", "level": "crevasse", "layer": "collision", "cells": [[3, 14, "water"]] }))
    .unwrap();
    app.apply_command(paint).unwrap();
    let e = app.recovery_choose(Choice::Recover).unwrap_err();
    assert!(e.contains("save or undo"), "{e}");
    assert!(app.recovery_pending(), "the prompt stays");
    assert!(app.has_unsaved());
}

fn paint(app: &mut worlds_editor::app::EditorApp, x: i64) {
    let c: worlds_core::Command = serde_json::from_value(json!({
        "op": "paint_cells", "level": "crevasse", "layer": "collision", "cells": [[x, 14, "water"]] }))
    .unwrap();
    app.apply_command(c).unwrap();
}

fn open_dir(app: &worlds_editor::app::EditorApp) -> PathBuf {
    std::fs::canonicalize(&app.document().unwrap().dir).unwrap()
}

/// Opening another project: the open one's unsaved edits are autosaved
/// first and its lock is kept until the writer is done (a stuck writer
/// refuses the switch); File > Open asks before replacing unsaved edits;
/// re-opening the same project after a save doesn't take this session's
/// own autosave for a crashed one.
#[test]
fn switching_projects_keeps_edits_and_waits_for_the_writer() {
    use worlds_editor::app::OpenChoice;
    use worlds_editor::app::recovery::RecoverMode;
    use worlds_editor::autosave::Blocked;
    if !gpu_tests() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let a = std::fs::canonicalize(copy_of_game(tmp.path(), "a")).unwrap();
    let b = std::fs::canonicalize(copy_of_game(tmp.path(), "b")).unwrap();
    let (_hl, _ctx, mut app) = editor_on(&a, RecoverMode::Keep);
    paint(&mut app, 3);

    // A stuck writer: the switch is refused and A stays locked.
    app.autosaver.pause_writer(true);
    let t = Instant::now();
    app.open(&b);
    assert!(
        t.elapsed() >= Duration::from_secs(4),
        "waited for the writer"
    );
    assert_eq!(open_dir(&app), a, "nothing switched");
    assert!(!lock_is_free(&a), "A's lock kept while its writer runs");
    assert!(app.has_unsaved());
    app.autosaver.pause_writer(false);

    // Now it switches: A's edits are in its autosave, its lock is free.
    app.open(&b);
    assert_eq!(open_dir(&app), b);
    assert!(lock_is_free(&a));
    assert!(!lock_is_free(&b));
    let Inspect::Found(f) = autosave::inspect(&a) else {
        panic!("A's unsaved edits weren't autosaved");
    };
    assert!(
        f.entries
            .iter()
            .any(|e| autosave::slash(&e.path).contains("crevasse")),
        "{:?}",
        f.entries.iter().map(|e| &e.path).collect::<Vec<_>>()
    );

    // Saved, then re-opened at once: no recovery of our own autosave.
    paint(&mut app, 4);
    app.autosave_now();
    app.save().unwrap();
    app.open(&b);
    assert!(!app.recovery_pending());
    assert_eq!(app.autosaver.blocked, None);
    assert!(!autosave::autosave_dir(&b).exists());

    // File > Open over unsaved edits asks; Cancel keeps everything.
    paint(&mut app, 5);
    app.autosave_now();
    assert!(app.autosaver.flush(Duration::from_secs(5)));
    app.request_open(&a);
    assert!(app.open_prompt_pending());
    assert_eq!(open_dir(&app), b);
    app.open_prompt_choose(OpenChoice::Cancel);
    assert!(!app.open_prompt_pending());
    assert_eq!(open_dir(&app), b);
    assert!(app.has_unsaved());
    // Discard opens A and drops B's edits with their autosave; A offers
    // its own back (Keep: autosave off until decided).
    app.request_open(&a);
    app.open_prompt_choose(OpenChoice::Discard);
    assert_eq!(open_dir(&app), a);
    assert!(!autosave::autosave_dir(&b).exists());
    assert_eq!(app.autosaver.blocked, Some(Blocked::Later));
    app.shutdown();
    assert!(lock_is_free(&a));
}

/// Recover looks at the disk again: a file changed while the prompt was
/// open turns the prompt into a conflict instead of being overwritten, and
/// no `--recover-report` body is made without the flag.
#[test]
fn recover_looks_again_when_the_disk_changes_under_the_prompt() {
    use worlds_editor::app::recovery::{Choice, RecoverMode};
    if !gpu_tests() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let dir = copy_of_game(tmp.path(), "stale");
    leave_crash(tmp.path(), &dir);
    let (_hl, _ctx, mut app) = editor_on(&dir, RecoverMode::Ask);
    let p = app.recovery_prompt_state().unwrap();
    let e = p
        .found
        .entries
        .iter()
        .find(|e| e.state == autosave::FileState::Modified && !e.conflict)
        .expect("a modified entry")
        .path
        .clone();
    // Someone edits that file on disk while the prompt is up.
    let mut bytes = std::fs::read(dir.join(&e)).unwrap();
    bytes.extend_from_slice(b"\n");
    std::fs::write(dir.join(&e), &bytes).unwrap();
    let err = app.recovery_choose(Choice::Recover).unwrap_err();
    assert!(err.contains("changed on disk"), "{err}");
    let p = app
        .recovery_prompt_state()
        .expect("the prompt stays, refreshed");
    assert!(
        p.found.entries.iter().any(|x| x.path == e && x.conflict),
        "now a conflict"
    );
    // Recover (conflicts skipped by default): the disk's change is kept.
    app.recovery_choose(Choice::Recover).unwrap();
    let skipped = app.recovered().unwrap()["skipped"].clone();
    assert!(
        skipped.to_string().contains(&autosave::slash(&e)),
        "{skipped}"
    );
    app.save().unwrap();
    assert_eq!(std::fs::read(dir.join(&e)).unwrap(), bytes);
    assert_eq!(app.recover_reports_made(), 0);
    app.shutdown();
}

/// Opening a folder that isn't a project creates nothing in it.
#[test]
fn opening_a_folder_that_is_not_a_project_writes_nothing() {
    use worlds_editor::app::recovery::RecoverMode;
    if !gpu_tests() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let (_hl, _ctx, app) = editor_on(tmp.path(), RecoverMode::Keep);
    assert!(app.document().is_none());
    assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
}
