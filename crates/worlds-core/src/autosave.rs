//! The editor's session lock, autosave and crash recovery, on disk.
//!
//! ```text
//! .worlds/editor.lock              held (flock) by the editor that has the project open
//! .worlds/editor.json              { pid, started, version } of that editor, for messages
//! .worlds/autosave/manifest.json   the commit point, written last and atomically
//! .worlds/autosave/g<N>/<path>     the autosaved bytes of each modified file, generation N
//! .worlds/autosave.discarded/      the last autosave the user discarded (one kept)
//! ```
//!
//! Liveness is the lock alone: an editor takes `editor.lock` with
//! [`EditorLock::acquire`] when it opens a project for editing and holds it
//! for the document's life; the OS releases it when the process dies, however
//! it dies. The lock file is never renamed or deleted (a new inode at that
//! path would be a second, free lock). An autosave with the lock free is an
//! editor session that ended without saving: [`pending`] tells headless
//! tools which case they are in, and [`inspect`] is what recovery reads.
//!
//! An autosave holds exactly what the editor's next save would write
//! ([`crate::format::render_dirty`]): the dirty files' rendered bytes and the
//! files the save would delete, with the dirty set itself, so loading the
//! disk with the autosave on top ([`crate::format::load_with_overlay`]) and
//! saving gives the tree the crashed session would have saved. Each entry
//! records `base`, the hash of the disk file when the session loaded or last
//! saved it, so a file changed on disk since (git, an agent's headless edit)
//! is a conflict, not silently overwritten.
//!
//! Hashes are XXH3-64 of the bytes, as 16 lowercase hex digits: stable
//! across builds and platforms (unlike `DefaultHasher`).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::format::{DirtyRender, Overlay};
use crate::model::Project;

/// The manifest's `format`.
pub const FORMAT: u32 = 1;

const MANIFEST: &str = "manifest.json";
const LOCK: &str = "editor.lock";
const INFO: &str = "editor.json";

/// The stable content hash used for `base` and `hash` (XXH3-64).
pub fn hash(bytes: &[u8]) -> u64 {
    xxhash_rust::xxh3::xxh3_64(bytes)
}

/// A hash as the manifest writes it: 16 lowercase hex digits.
pub fn hex(h: u64) -> String {
    format!("{h:016x}")
}

fn parse_hex(s: &str) -> Option<u64> {
    (s.len() == 16)
        .then(|| u64::from_str_radix(s, 16).ok())
        .flatten()
}

/// `<project>/.worlds`.
pub fn worlds_dir(project_dir: &Path) -> PathBuf {
    project_dir.join(".worlds")
}

/// `<project>/.worlds/autosave`.
pub fn autosave_dir(project_dir: &Path) -> PathBuf {
    worlds_dir(project_dir).join("autosave")
}

/// `<project>/.worlds/autosave.discarded`.
pub fn discarded_dir(project_dir: &Path) -> PathBuf {
    worlds_dir(project_dir).join("autosave.discarded")
}

// ---- time ----------------------------------------------------------------

/// Now as RFC 3339 in UTC, to the second (`2026-10-22T14:02:31Z`).
pub fn now_rfc3339() -> String {
    rfc3339(SystemTime::now())
}

/// `t` as RFC 3339 in UTC, to the second.
pub fn rfc3339(t: SystemTime) -> String {
    let secs = t
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil from days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// Parse what [`rfc3339`] writes (UTC, `Z`, whole seconds).
pub fn parse_rfc3339(s: &str) -> Option<SystemTime> {
    let b = s.as_bytes();
    if b.len() != 20 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[19] != b'Z' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, m, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (hh, mm, ss) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || hh > 23 || mm > 59 || ss > 60 {
        return None;
    }
    // Days from civil.
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + hh * 3600 + mm * 60 + ss;
    UNIX_EPOCH.checked_add(Duration::from_secs(u64::try_from(secs).ok()?))
}

/// "14:02:31 UTC, 3 min ago" for an RFC 3339 time (as written), else the
/// text itself.
pub fn describe_time(written: &str) -> String {
    let Some(t) = parse_rfc3339(written) else {
        return written.to_string();
    };
    let clock = written.get(11..19).unwrap_or(written);
    match SystemTime::now().duration_since(t) {
        Ok(d) => format!("{clock} UTC, {} ago", human(d)),
        Err(_) => format!("{clock} UTC"),
    }
}

fn human(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..60 => format!("{s} s"),
        60..3600 => format!("{} min", s / 60),
        3600..172_800 => format!("{} h", s / 3600),
        _ => format!("{} days", s / 86_400),
    }
}

// ---- the session lock --------------------------------------------------------

/// `.worlds/editor.json`: who holds the lock, for messages only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EditorInfo {
    pub pid: u32,
    /// RFC 3339.
    pub started: String,
    /// The editor's version.
    pub version: String,
}

/// The project's session lock, held while this value lives (see the module
/// docs). Dropping it releases the lock; the lock file stays.
#[derive(Debug)]
pub struct EditorLock {
    /// `None` where the filesystem can't lock: the editor goes on without.
    file: Option<fs::File>,
    dir: PathBuf,
}

/// Why [`EditorLock::acquire`] failed.
#[derive(Debug)]
pub enum LockError {
    /// Another editor holds the lock (its `editor.json`, if readable).
    Held(Option<EditorInfo>),
    /// `.worlds/` or the lock file couldn't be created.
    Io(io::Error),
}

impl fmt::Display for LockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LockError::Held(Some(i)) => write!(
                f,
                "this project is open in another Worlds (pid {}, since {})",
                i.pid,
                describe_time(&i.started)
            ),
            LockError::Held(None) => write!(f, "this project is open in another Worlds"),
            LockError::Io(e) => write!(f, "can't take the project's editor lock: {e}"),
        }
    }
}

impl std::error::Error for LockError {}

impl EditorLock {
    /// Take `<project>/.worlds/editor.lock` (retrying for a moment, in case
    /// a headless tool is checking it with [`pending`]) and write
    /// `editor.json`. Where the filesystem can't lock, succeeds without a
    /// lock ([`Self::is_locked`] is false).
    pub fn acquire(project_dir: &Path, version: &str) -> Result<EditorLock, LockError> {
        let wdir = worlds_dir(project_dir);
        fs::create_dir_all(&wdir).map_err(LockError::Io)?;
        let file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(wdir.join(LOCK))
            .map_err(LockError::Io)?;
        let mut tries = 0;
        let file = loop {
            match file.try_lock() {
                Ok(()) => break Some(file),
                Err(fs::TryLockError::WouldBlock) if tries < 10 => {
                    tries += 1;
                    std::thread::sleep(Duration::from_millis(30));
                }
                Err(fs::TryLockError::WouldBlock) => {
                    return Err(LockError::Held(read_info(project_dir)));
                }
                Err(fs::TryLockError::Error(e)) => {
                    tracing_warn(&format!(
                        "can't lock {}: {e}; going on without a session lock",
                        wdir.join(LOCK).display()
                    ));
                    break None;
                }
            }
        };
        let info = EditorInfo {
            pid: std::process::id(),
            started: now_rfc3339(),
            version: version.to_string(),
        };
        let text = serde_json::to_string_pretty(&info).expect("serializes") + "\n";
        if let Err(e) = crate::format::write_atomic(&wdir.join(INFO), text.as_bytes()) {
            tracing_warn(&format!("can't write {}: {e}", wdir.join(INFO).display()));
        }
        Ok(EditorLock {
            file,
            dir: project_dir.to_path_buf(),
        })
    }

    /// Whether the lock is really held (false where the filesystem can't
    /// lock).
    pub fn is_locked(&self) -> bool {
        self.file.is_some()
    }

    /// A lock that holds nothing, as [`Self::acquire`] returns where the
    /// filesystem can't lock (tests of what callers do then).
    #[doc(hidden)]
    pub fn unlocked(project_dir: &Path) -> EditorLock {
        EditorLock {
            file: None,
            dir: project_dir.to_path_buf(),
        }
    }

    /// The project directory this lock is for.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Release the lock and remove `editor.json` (a clean close).
    pub fn release(self) {
        let _ = fs::remove_file(worlds_dir(&self.dir).join(INFO));
        drop(self);
    }
}

/// `.worlds/editor.json`, if it reads.
pub fn read_info(project_dir: &Path) -> Option<EditorInfo> {
    let text = fs::read_to_string(worlds_dir(project_dir).join(INFO)).ok()?;
    serde_json::from_str(&text).ok()
}

/// What `try_lock` on the lock file says, without keeping it.
enum Probe {
    /// No lock file, or it is free.
    Free,
    /// Another open file holds it: a live editor.
    Held,
    /// The filesystem can't lock.
    Unknown,
}

fn probe(project_dir: &Path) -> Probe {
    let Ok(file) = fs::OpenOptions::new()
        .write(true)
        .open(worlds_dir(project_dir).join(LOCK))
    else {
        return Probe::Free;
    };
    match file.try_lock() {
        Ok(()) => Probe::Free,
        Err(fs::TryLockError::WouldBlock) => Probe::Held,
        Err(fs::TryLockError::Error(_)) => Probe::Unknown,
    }
}

fn tracing_warn(msg: &str) {
    eprintln!("warning: {msg}");
}

// ---- the manifest ------------------------------------------------------------

/// `.worlds/autosave/manifest.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// [`FORMAT`].
    pub format: u32,
    /// `N` of the `g<N>/` directory holding the modified files.
    pub generation: u64,
    /// When this generation was written (RFC 3339, UTC).
    pub written: String,
    /// The editor process that wrote it.
    pub pid: u32,
    /// The editor's document revision at the time.
    pub rev: u64,
    /// The label of the newest undo entry then, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_label: Option<String>,
    /// The session's dirty set, verbatim (a path without an extension is a
    /// whole level directory).
    pub dirty: Vec<String>,
    /// Every file the session's save would write (`modified`) or delete
    /// (`deleted`), by project-relative path with `/`.
    pub files: BTreeMap<String, FileEntry>,
}

/// One file of a [`Manifest`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileEntry {
    pub state: FileState,
    /// Hash of the file on disk when the session loaded or last saved it;
    /// `null` if it didn't exist.
    pub base: Option<String>,
    /// Hash of the autosaved bytes (`modified` only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileState {
    Modified,
    Deleted,
}

impl FileState {
    pub fn name(self) -> &'static str {
        match self {
            FileState::Modified => "modified",
            FileState::Deleted => "deleted",
        }
    }
}

/// A generation ready to write: the manifest and the modified files'
/// bytes (relative paths).
#[derive(Debug, Clone)]
pub struct Generation {
    pub manifest: Manifest,
    pub files: BTreeMap<PathBuf, Vec<u8>>,
}

/// A project-relative path as the manifest writes it: components joined
/// by `/` on every platform.
pub fn slash(p: &Path) -> String {
    p.iter()
        .map(|c| c.to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// Turn a [`render_dirty`](crate::format::render_dirty) of `project` into
/// generation `generation`: each written file `modified` with its hash, each
/// deleted file `deleted`, each with its `base` from
/// [`crate::model::ProjectFiles::base_hash`], and the dirty set verbatim.
pub fn generation(
    project: &Project,
    render: DirtyRender,
    generation: u64,
    rev: u64,
    last_label: Option<String>,
) -> Generation {
    let base = |rel: &Path| project.files.base_hash(rel).map(hex);
    let mut files = BTreeMap::new();
    for (rel, bytes) in &render.files {
        files.insert(
            slash(rel),
            FileEntry {
                state: FileState::Modified,
                base: base(rel),
                hash: Some(hex(hash(bytes))),
            },
        );
    }
    for rel in &render.deleted {
        files.insert(
            slash(rel),
            FileEntry {
                state: FileState::Deleted,
                base: base(rel),
                hash: None,
            },
        );
    }
    Generation {
        manifest: Manifest {
            format: FORMAT,
            generation,
            written: now_rfc3339(),
            pid: std::process::id(),
            rev,
            last_label,
            dirty: project.files.dirty().iter().map(|p| slash(p)).collect(),
            files,
        },
        files: render.files,
    }
}

fn gen_name(n: u64) -> String {
    format!("g{n}")
}

fn gen_number(name: &str) -> Option<u64> {
    name.strip_prefix('g')?.parse().ok()
}

/// The generation number after every one in `.worlds/autosave/` (the
/// manifest's and any `g<N>/` left over), so a new session never writes
/// into a generation a manifest may name.
pub fn next_generation(project_dir: &Path) -> u64 {
    let adir = autosave_dir(project_dir);
    let mut max = read_manifest(project_dir)
        .ok()
        .flatten()
        .map_or(0, |m| m.generation);
    if let Ok(rd) = fs::read_dir(&adir) {
        for e in rd.flatten() {
            if let Some(n) = gen_number(&e.file_name().to_string_lossy()) {
                max = max.max(n);
            }
        }
    }
    max + 1
}

/// Write a generation: its files into `g<N>/` (each written and fsynced,
/// then the directories), then `manifest.json` atomically (the commit
/// point), then remove every other `g<M>/`. A crash at any point leaves
/// either the old manifest with its generation intact or the new ones
/// complete.
pub fn write(project_dir: &Path, g: &Generation) -> io::Result<()> {
    let adir = autosave_dir(project_dir);
    let gdir = adir.join(gen_name(g.manifest.generation));
    if gdir.exists() {
        fs::remove_dir_all(&gdir)?;
    }
    fs::create_dir_all(&gdir)?;
    let mut dirs: BTreeSet<PathBuf> = BTreeSet::new();
    for (rel, bytes) in &g.files {
        let path = gdir.join(rel);
        let parent = path.parent().expect("has a parent").to_path_buf();
        fs::create_dir_all(&parent)?;
        let mut f = fs::File::create(&path)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        let mut d = Some(parent.as_path());
        while let Some(x) = d.filter(|x| x.starts_with(&gdir)) {
            dirs.insert(x.to_path_buf());
            d = x.parent();
        }
    }
    let mut dirs: Vec<PathBuf> = dirs.into_iter().collect();
    dirs.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
    for d in dirs.iter().chain([&gdir, &adir]) {
        crate::format::sync_dir(d)?;
    }
    let text = serde_json::to_string_pretty(&g.manifest).expect("serializes") + "\n";
    crate::format::write_atomic(&adir.join(MANIFEST), text.as_bytes())
        .map_err(|e| io::Error::other(e.to_string()))?;
    remove_other_generations(&adir, Some(g.manifest.generation));
    Ok(())
}

fn remove_other_generations(adir: &Path, keep: Option<u64>) {
    let Ok(rd) = fs::read_dir(adir) else {
        return;
    };
    let mut removed = false;
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if let Some(n) = gen_number(&name)
            && Some(n) != keep
        {
            let _ = fs::remove_dir_all(e.path());
            removed = true;
        }
    }
    if removed {
        let _ = crate::format::sync_dir(adir);
    }
}

/// Delete `.worlds/autosave/`: the manifest first (so a crash part way
/// leaves no manifest), then everything else. Nothing there is fine.
pub fn clear(project_dir: &Path) -> io::Result<()> {
    let adir = autosave_dir(project_dir);
    match fs::remove_file(adir.join(MANIFEST)) {
        Ok(()) => crate::format::sync_dir(&adir)?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    match fs::remove_dir_all(&adir) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    }
    crate::format::sync_dir(&worlds_dir(project_dir))
}

/// Move `.worlds/autosave/` to `.worlds/autosave.discarded/` (replacing the
/// one discarded before). Returns where it went; nothing there is fine.
pub fn discard(project_dir: &Path) -> io::Result<PathBuf> {
    let adir = autosave_dir(project_dir);
    let ddir = discarded_dir(project_dir);
    if !adir.exists() {
        return Ok(ddir);
    }
    match fs::remove_dir_all(&ddir) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    fs::rename(&adir, &ddir)?;
    crate::format::sync_dir(&worlds_dir(project_dir))?;
    Ok(ddir)
}

/// The manifest, if there is one; `Err` if it doesn't parse.
pub fn read_manifest(project_dir: &Path) -> Result<Option<Manifest>, String> {
    let path = autosave_dir(project_dir).join(MANIFEST);
    let text = match fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let m: Manifest =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    if m.format != FORMAT {
        return Err(format!(
            "{}: unsupported format {}, expected {FORMAT}",
            path.display(),
            m.format
        ));
    }
    Ok(Some(m))
}

// ---- recovery ----------------------------------------------------------------

/// One autosaved file compared with the disk.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// Project-relative.
    pub path: PathBuf,
    pub state: FileState,
    /// The disk changed since the session's base: recovering it overwrites
    /// that change on the next save.
    pub conflict: bool,
    /// The autosaved bytes (`modified`).
    pub autosaved: Option<Vec<u8>>,
    /// The disk's bytes now, if the file exists.
    pub disk: Option<Vec<u8>>,
    /// Lines added and removed from disk to autosave.
    pub plus: usize,
    pub minus: usize,
}

impl Entry {
    /// The unified line diff from the disk to the autosave.
    pub fn diff(&self) -> String {
        let a = String::from_utf8_lossy(self.disk.as_deref().unwrap_or_default());
        let b = String::from_utf8_lossy(self.autosaved.as_deref().unwrap_or_default());
        let p = slash(&self.path);
        unified_diff(&a, &b, &format!("disk/{p}"), &format!("autosave/{p}"))
    }
}

/// A previous session's autosave, validated and compared with the disk.
#[derive(Debug, Clone)]
pub struct Found {
    pub manifest: Manifest,
    /// The entries that differ from the disk (files already saved are left
    /// out), sorted by path.
    pub entries: Vec<Entry>,
}

/// What [`inspect`] found in `.worlds/autosave/`.
#[derive(Debug, Clone)]
pub enum Inspect {
    /// No manifest.
    None,
    /// A manifest that doesn't parse, or a generation with a file missing
    /// or not matching its hash.
    Broken(String),
    /// A valid autosave, but a project file it covers couldn't be read
    /// (permissions, I/O): it can't be compared with the disk now. The
    /// autosave is fine and must be kept, not set aside as broken.
    DiskUnreadable(String),
    /// A valid autosave. Its `entries` may be empty: everything in it is
    /// already on disk.
    Found(Found),
}

/// Read and validate the autosave and compare each entry with the disk:
/// an entry whose bytes equal the disk (or a deletion of a file that is
/// gone) is dropped; one whose disk file no longer matches its `base` is a
/// conflict. Reads only; the caller holds the session lock.
pub fn inspect(project_dir: &Path) -> Inspect {
    let m = match read_manifest(project_dir) {
        Ok(None) => return Inspect::None,
        Ok(Some(m)) => m,
        Err(e) => return Inspect::Broken(e),
    };
    let gdir = autosave_dir(project_dir).join(gen_name(m.generation));
    let mut entries = Vec::new();
    // A project file that doesn't read is the disk's problem, not the
    // autosave's: the rest of the generation is still validated, then this
    // is reported instead of the entries.
    let mut disk_error = None;
    for (rel, e) in &m.files {
        let path = PathBuf::from(rel);
        if path.is_absolute()
            || path
                .components()
                .any(|c| !matches!(c, std::path::Component::Normal(_)))
        {
            return Inspect::Broken(format!("autosave: bad path {rel:?}"));
        }
        let autosaved = match e.state {
            FileState::Modified => {
                let Some(want) = e.hash.as_deref().and_then(parse_hex) else {
                    return Inspect::Broken(format!("autosave: {rel} has no valid hash"));
                };
                let bytes = match fs::read(gdir.join(&path)) {
                    Ok(b) => b,
                    Err(err) => {
                        return Inspect::Broken(format!(
                            "autosave generation {} is incomplete: {rel}: {err}",
                            m.generation
                        ));
                    }
                };
                if hash(&bytes) != want {
                    return Inspect::Broken(format!(
                        "autosave generation {}: {rel} doesn't match its hash",
                        m.generation
                    ));
                }
                Some(bytes)
            }
            FileState::Deleted => None,
        };
        let disk = match fs::read(project_dir.join(&path)) {
            Ok(b) => Some(b),
            Err(err) if err.kind() == io::ErrorKind::NotFound => None,
            Err(err) => {
                disk_error.get_or_insert_with(|| format!("{rel}: {err}"));
                continue;
            }
        };
        if disk == autosaved {
            continue;
        }
        let disk_hash = disk.as_deref().map(hash);
        let base = e.base.as_deref().and_then(parse_hex);
        let (plus, minus) = {
            let a = String::from_utf8_lossy(disk.as_deref().unwrap_or_default());
            let b = String::from_utf8_lossy(autosaved.as_deref().unwrap_or_default());
            counts(&diff_lines(&lines(&a), &lines(&b)))
        };
        entries.push(Entry {
            path,
            state: e.state,
            conflict: disk_hash != base,
            autosaved,
            disk,
            plus,
            minus,
        });
    }
    if let Some(e) = disk_error {
        return Inspect::DiskUnreadable(e);
    }
    Inspect::Found(Found {
        manifest: m,
        entries,
    })
}

/// What recovering a [`Found`] loads: see [`Found::overlay`].
#[derive(Debug, Clone, Default)]
pub struct Plan {
    /// For [`crate::format::load_with_overlay`].
    pub overlay: Overlay,
    /// The dirty set to restore.
    pub dirty: Vec<PathBuf>,
    /// Entries recovered.
    pub applied: Vec<PathBuf>,
    /// Conflicting entries left out (the disk wins).
    pub skipped: Vec<PathBuf>,
}

impl Found {
    /// Whether any entry is a conflict.
    pub fn has_conflicts(&self) -> bool {
        self.entries.iter().any(|e| e.conflict)
    }

    /// The overlay and dirty set that recover these entries: every entry,
    /// or with `conflicts` false all but the conflicting ones. The dirty
    /// set is the session's, minus keys no recovered entry is in (files
    /// already saved, skipped conflicts), so the next save writes and
    /// deletes exactly the recovered files.
    pub fn plan(&self, conflicts: bool) -> Plan {
        let mut plan = Plan::default();
        for e in &self.entries {
            if e.conflict && !conflicts {
                plan.skipped.push(e.path.clone());
                continue;
            }
            plan.overlay.insert(e.path.clone(), e.autosaved.clone());
            plan.applied.push(e.path.clone());
        }
        plan.dirty = self
            .manifest
            .dirty
            .iter()
            .map(PathBuf::from)
            .filter(|key| plan.applied.iter().any(|p| p.starts_with(key)))
            .collect();
        plan
    }

    /// The `--recover-report` body: `{ found, session, files, applied }`
    /// (`applied` as given, or empty).
    pub fn report(&self, applied: &[PathBuf], with_diff: bool) -> Value {
        let files: Vec<Value> = self
            .entries
            .iter()
            .map(|e| {
                let mut v = json!({
                    "path": slash(&e.path),
                    "state": e.state.name(),
                    "conflict": e.conflict,
                    "plus": e.plus,
                    "minus": e.minus,
                });
                if with_diff {
                    v["diff"] = e.diff().into();
                }
                v
            })
            .collect();
        json!({
            "found": true,
            "session": {
                "pid": self.manifest.pid,
                "written": self.manifest.written,
                "last_label": self.manifest.last_label,
                "generation": self.manifest.generation,
            },
            "files": files,
            "applied": applied.iter().map(|p| slash(p)).collect::<Vec<_>>(),
        })
    }

    /// One line for logs: "N files from session pid P (written …, after
    /// 'label')".
    pub fn summary(&self) -> String {
        let n = self.entries.len();
        let conflicts = self.entries.iter().filter(|e| e.conflict).count();
        format!(
            "{n} file{} from session pid {} (last autosave {}{}){}",
            if n == 1 { "" } else { "s" },
            self.manifest.pid,
            describe_time(&self.manifest.written),
            self.manifest
                .last_label
                .as_deref()
                .map(|l| format!(", after {l:?}"))
                .unwrap_or_default(),
            if conflicts > 0 {
                format!(", {conflicts} changed on disk since")
            } else {
                String::new()
            }
        )
    }
}

// ---- headless tools ----------------------------------------------------------

/// What [`pending`] found.
#[derive(Debug, Clone, PartialEq)]
pub enum Pending {
    /// No autosave.
    None,
    /// A Worlds editor has the project open (its autosave is its own).
    Live,
    /// An autosave from an editor session that ended without saving.
    Crashed(Summary),
}

/// A crashed session's autosave, for a warning.
#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    pub pid: Option<u32>,
    pub written: Option<String>,
    pub files: usize,
    pub last_label: Option<String>,
    /// The manifest didn't read (`files` is 0).
    pub unreadable: Option<String>,
}

impl Summary {
    /// The warning headless writers print.
    pub fn warning(&self) -> String {
        if let Some(e) = &self.unreadable {
            return format!(
                "an unreadable autosave from a Worlds session that didn't close cleanly is in .worlds/autosave/ ({e}); open the project in the Worlds editor to deal with it"
            );
        }
        format!(
            "an unrecovered autosave from a Worlds session{} that ended without saving holds unsaved edits to {} file{}{}; open the project in the Worlds editor to recover or discard them (edits made now are kept: the editor shows them as conflicts)",
            self.pid.map(|p| format!(" (pid {p})")).unwrap_or_default(),
            self.files,
            if self.files == 1 { "" } else { "s" },
            self.written
                .as_deref()
                .map(|w| format!(", last written {}", describe_time(w)))
                .unwrap_or_default(),
        )
    }

    /// The `autosave` field of worlds-mcp's `describe_project`.
    pub fn to_json(&self) -> Value {
        json!({
            "unrecovered": true,
            "pid": self.pid,
            "written": self.written,
            "files": self.files,
            "last_label": self.last_label,
            "message": self.warning(),
        })
    }
}

/// Whether `.worlds/autosave/` holds edits nobody is looking after. A
/// locked `editor.lock` means a live editor owns the autosave (say nothing);
/// a free one with a manifest is a session that ended without saving. Where
/// the filesystem can't lock, a manifest counts as crashed. Takes the lock
/// for a moment at most.
pub fn pending(project_dir: &Path) -> Pending {
    let manifest = autosave_dir(project_dir).join(MANIFEST);
    if !manifest.exists() {
        return Pending::None;
    }
    if let Probe::Held = probe(project_dir) {
        return Pending::Live;
    }
    match read_manifest(project_dir) {
        Ok(None) => Pending::None,
        Ok(Some(m)) => Pending::Crashed(Summary {
            pid: Some(m.pid),
            written: Some(m.written),
            files: m.files.len(),
            last_label: m.last_label,
            unreadable: None,
        }),
        Err(e) => Pending::Crashed(Summary {
            pid: None,
            written: None,
            files: 0,
            last_label: None,
            unreadable: Some(e),
        }),
    }
}

/// For headless writers (worlds-cli): print [`Summary::warning`] to stderr
/// when [`pending`] finds a crashed session's autosave.
pub fn warn_pending(project_dir: &Path) {
    if let Pending::Crashed(s) = pending(project_dir) {
        eprintln!("warning: {}", s.warning());
    }
}

// ---- line diff ---------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Same(usize, usize),
    Del(usize),
    Add(usize),
}

fn lines(s: &str) -> Vec<&str> {
    s.lines().collect()
}

fn counts(ops: &[Op]) -> (usize, usize) {
    ops.iter().fold((0, 0), |(p, m), op| match op {
        Op::Add(_) => (p + 1, m),
        Op::Del(_) => (p, m + 1),
        Op::Same(..) => (p, m),
    })
}

/// Line edit script from `a` to `b`: the common prefix and suffix, and an
/// LCS of the middle (when it is small enough; else all of the middle is
/// replaced).
fn diff_lines(a: &[&str], b: &[&str]) -> Vec<Op> {
    let pre = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    let suf = a[pre..]
        .iter()
        .rev()
        .zip(b[pre..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let (am, bm) = (&a[pre..a.len() - suf], &b[pre..b.len() - suf]);
    let mut ops: Vec<Op> = (0..pre).map(|i| Op::Same(i, i)).collect();
    let (n, m) = (am.len(), bm.len());
    if n * m <= 4_000_000 {
        // lcs[i][j]: LCS length of am[i..] and bm[j..].
        let w = m + 1;
        let mut lcs = vec![0u32; (n + 1) * w];
        for i in (0..n).rev() {
            for j in (0..m).rev() {
                lcs[i * w + j] = if am[i] == bm[j] {
                    lcs[(i + 1) * w + j + 1] + 1
                } else {
                    lcs[(i + 1) * w + j].max(lcs[i * w + j + 1])
                };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < n || j < m {
            if i < n && j < m && am[i] == bm[j] {
                ops.push(Op::Same(pre + i, pre + j));
                i += 1;
                j += 1;
            } else if i < n && (j == m || lcs[(i + 1) * w + j] >= lcs[i * w + j + 1]) {
                ops.push(Op::Del(pre + i));
                i += 1;
            } else {
                ops.push(Op::Add(pre + j));
                j += 1;
            }
        }
    } else {
        ops.extend((0..n).map(|i| Op::Del(pre + i)));
        ops.extend((0..m).map(|j| Op::Add(pre + j)));
    }
    ops.extend((0..suf).map(|k| Op::Same(a.len() - suf + k, b.len() - suf + k)));
    ops
}

/// A unified diff (3 lines of context) from `a` to `b`, with `---`/`+++`
/// headers; empty when they are equal.
pub fn unified_diff(a: &str, b: &str, a_name: &str, b_name: &str) -> String {
    const CONTEXT: usize = 3;
    let (al, bl) = (lines(a), lines(b));
    let ops = diff_lines(&al, &bl);
    let changed: Vec<usize> = ops
        .iter()
        .enumerate()
        .filter(|(_, o)| !matches!(o, Op::Same(..)))
        .map(|(i, _)| i)
        .collect();
    if changed.is_empty() {
        return String::new();
    }
    let mut out = format!("--- {a_name}\n+++ {b_name}\n");
    let mut k = 0;
    while k < changed.len() {
        let start = changed[k].saturating_sub(CONTEXT);
        let mut end = changed[k];
        while k < changed.len() && changed[k] <= end + 2 * CONTEXT + 1 {
            end = changed[k];
            k += 1;
        }
        let end = (end + CONTEXT + 1).min(ops.len());
        let hunk = &ops[start..end];
        // Hunk header positions (1-based; a side with no lines gives the
        // line before).
        let a_start = hunk
            .iter()
            .find_map(|o| match o {
                Op::Same(i, _) | Op::Del(i) => Some(*i),
                Op::Add(_) => None,
            })
            .map_or_else(|| a_line_before(&ops[..start]), |i| i + 1);
        let b_start = hunk
            .iter()
            .find_map(|o| match o {
                Op::Same(_, j) | Op::Add(j) => Some(*j),
                Op::Del(_) => None,
            })
            .map_or_else(|| b_line_before(&ops[..start]), |j| j + 1);
        let a_len = hunk.iter().filter(|o| !matches!(o, Op::Add(_))).count();
        let b_len = hunk.iter().filter(|o| !matches!(o, Op::Del(_))).count();
        out.push_str(&format!("@@ -{a_start},{a_len} +{b_start},{b_len} @@\n"));
        for o in hunk {
            match *o {
                Op::Same(i, _) => out.push_str(&format!(" {}\n", al[i])),
                Op::Del(i) => out.push_str(&format!("-{}\n", al[i])),
                Op::Add(j) => out.push_str(&format!("+{}\n", bl[j])),
            }
        }
    }
    out
}

fn a_line_before(ops: &[Op]) -> usize {
    ops.iter().filter(|o| !matches!(o, Op::Add(_))).count()
}

fn b_line_before(ops: &[Op]) -> usize {
    ops.iter().filter(|o| !matches!(o, Op::Del(_))).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_round_trips() {
        for secs in [0u64, 951_782_400, 1_792_670_551, 4_102_444_799] {
            let t = UNIX_EPOCH + Duration::from_secs(secs);
            let s = rfc3339(t);
            assert_eq!(parse_rfc3339(&s), Some(t), "{s}");
        }
        assert_eq!(
            rfc3339(UNIX_EPOCH + Duration::from_secs(951_782_400)),
            "2000-02-29T00:00:00Z"
        );
        assert_eq!(parse_rfc3339("2026-13-01T00:00:00Z"), None);
    }

    #[test]
    fn hash_is_stable() {
        // XXH3-64 of these bytes is fixed by the algorithm, whatever the
        // Rust version: a manifest written by one build reads in another.
        assert_eq!(hex(hash(b"")), "2d06800538d394c2");
        assert_eq!(parse_hex(&hex(hash(b"worlds"))), Some(hash(b"worlds")));
        assert_eq!(parse_hex("xyz"), None);
    }

    #[test]
    fn unified_diff_shows_changes_with_context() {
        let a = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\n";
        let b = "a\nb\nC\nd\ne\nf\ng\nh\ni\nj\nk\n";
        let d = unified_diff(a, b, "disk/x", "autosave/x");
        assert_eq!(
            d,
            "--- disk/x\n+++ autosave/x\n@@ -1,6 +1,6 @@\n a\n b\n-c\n+C\n d\n e\n f\n@@ -8,3 +8,4 @@\n h\n i\n j\n+k\n"
        );
        assert_eq!(unified_diff(a, a, "x", "y"), "");
        // Everything new, everything gone.
        assert_eq!(
            unified_diff("", "x\n", "a", "b"),
            "--- a\n+++ b\n@@ -0,0 +1,1 @@\n+x\n"
        );
        assert_eq!(
            unified_diff("x\n", "", "a", "b"),
            "--- a\n+++ b\n@@ -1,1 +0,0 @@\n-x\n"
        );
        assert_eq!(counts(&diff_lines(&lines(a), &lines(b))), (2, 1));
    }

    #[test]
    fn a_second_lock_fails_and_rewriting_the_info_keeps_it_locked() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let lock = EditorLock::acquire(dir, "test").unwrap();
        assert!(lock.is_locked());
        let info = read_info(dir).unwrap();
        assert_eq!(info.pid, std::process::id());
        // Someone rewrites editor.json (atomically: a new inode); the lock
        // file is a different file and stays locked.
        std::fs::write(dir.join(".worlds/editor.json.new"), "{}").unwrap();
        std::fs::rename(
            dir.join(".worlds/editor.json.new"),
            dir.join(".worlds/editor.json"),
        )
        .unwrap();
        match EditorLock::acquire(dir, "test") {
            Err(LockError::Held(_)) => {}
            other => panic!("a second lock: {other:?}"),
        }
        assert!(matches!(probe(dir), Probe::Held));
        lock.release();
        assert!(matches!(probe(dir), Probe::Free));
        assert!(!dir.join(".worlds/editor.json").exists());
        assert!(dir.join(".worlds/editor.lock").exists());
        let again = EditorLock::acquire(dir, "test").unwrap();
        drop(again);
    }

    #[test]
    fn pending_tells_a_live_editor_from_a_crashed_one() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        assert_eq!(pending(dir), Pending::None);
        let g = Generation {
            manifest: Manifest {
                format: FORMAT,
                generation: 1,
                written: now_rfc3339(),
                pid: 4242,
                rev: 3,
                last_label: Some("Paint".into()),
                dirty: vec!["a.json".into()],
                files: BTreeMap::from([(
                    "a.json".to_string(),
                    FileEntry {
                        state: FileState::Modified,
                        base: None,
                        hash: Some(hex(hash(b"x"))),
                    },
                )]),
            },
            files: BTreeMap::from([(PathBuf::from("a.json"), b"x".to_vec())]),
        };
        write(dir, &g).unwrap();
        let lock = EditorLock::acquire(dir, "test").unwrap();
        assert_eq!(pending(dir), Pending::Live);
        drop(lock);
        let Pending::Crashed(s) = pending(dir) else {
            panic!("crashed expected");
        };
        assert_eq!((s.pid, s.files), (Some(4242), 1));
        assert!(s.warning().contains("edits to 1 file,"), "{}", s.warning());
        // pending let go of the lock again.
        drop(EditorLock::acquire(dir, "test").unwrap());
        clear(dir).unwrap();
        assert_eq!(pending(dir), Pending::None);
    }

    /// A project file that can't be read (here: a directory where the file
    /// should be) is the disk's problem: the autosave is reported as
    /// unreadable to compare, not broken, so recovery keeps it.
    #[test]
    fn an_unreadable_project_file_does_not_break_the_autosave() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::create_dir_all(dir.join("a.json")).unwrap();
        let g = Generation {
            manifest: Manifest {
                format: FORMAT,
                generation: 1,
                written: now_rfc3339(),
                pid: 1,
                rev: 1,
                last_label: None,
                dirty: vec!["a.json".into(), "b.json".into()],
                files: BTreeMap::from([
                    (
                        "a.json".to_string(),
                        FileEntry {
                            state: FileState::Modified,
                            base: None,
                            hash: Some(hex(hash(b"x"))),
                        },
                    ),
                    (
                        "b.json".to_string(),
                        FileEntry {
                            state: FileState::Modified,
                            base: None,
                            hash: Some(hex(hash(b"y"))),
                        },
                    ),
                ]),
            },
            files: BTreeMap::from([
                (PathBuf::from("a.json"), b"x".to_vec()),
                (PathBuf::from("b.json"), b"y".to_vec()),
            ]),
        };
        write(dir, &g).unwrap();
        match inspect(dir) {
            Inspect::DiskUnreadable(m) => assert!(m.starts_with("a.json:"), "{m}"),
            other => panic!("expected DiskUnreadable, got {other:?}"),
        }
        // The rest of the generation is still validated: a broken one is
        // broken whatever the disk does.
        std::fs::write(autosave_dir(dir).join("g1/b.json"), "tampered").unwrap();
        assert!(matches!(inspect(dir), Inspect::Broken(m) if m.contains("hash")));
    }

    #[test]
    fn write_replaces_generations_and_inspect_validates() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::write(dir.join("same.json"), "same\n").unwrap();
        std::fs::write(dir.join("gone.json"), "old\n").unwrap();
        let mk = |n: u64, text: &str| {
            let files = BTreeMap::from([
                (PathBuf::from("w/x.json"), text.as_bytes().to_vec()),
                (PathBuf::from("same.json"), b"same\n".to_vec()),
            ]);
            let mut entries: BTreeMap<String, FileEntry> = files
                .iter()
                .map(|(p, b)| {
                    (
                        slash(p),
                        FileEntry {
                            state: FileState::Modified,
                            base: None,
                            hash: Some(hex(hash(b))),
                        },
                    )
                })
                .collect();
            entries.insert(
                "gone.json".into(),
                FileEntry {
                    state: FileState::Deleted,
                    base: Some(hex(hash(b"old\n"))),
                    hash: None,
                },
            );
            Generation {
                manifest: Manifest {
                    format: FORMAT,
                    generation: n,
                    written: now_rfc3339(),
                    pid: 1,
                    rev: n,
                    last_label: None,
                    dirty: vec!["w/x.json".into(), "gone.json".into(), "same.json".into()],
                    files: entries,
                },
                files,
            }
        };
        write(dir, &mk(1, "one\n")).unwrap();
        assert_eq!(next_generation(dir), 2);
        write(dir, &mk(2, "two\n")).unwrap();
        let adir = autosave_dir(dir);
        assert!(!adir.join("g1").exists() && adir.join("g2/w/x.json").exists());
        let Inspect::Found(f) = inspect(dir) else {
            panic!("found expected")
        };
        // same.json equals the disk: dropped. x.json is new (base null,
        // disk absent: no conflict); gone.json is deleted, base matches.
        let paths: Vec<String> = f.entries.iter().map(|e| slash(&e.path)).collect();
        assert_eq!(paths, ["gone.json", "w/x.json"]);
        assert!(!f.has_conflicts());
        let plan = f.plan(false);
        assert_eq!(
            plan.overlay[Path::new("w/x.json")].as_deref(),
            Some(&b"two\n"[..])
        );
        assert_eq!(plan.overlay[Path::new("gone.json")], None);
        assert_eq!(
            plan.dirty,
            [PathBuf::from("w/x.json"), PathBuf::from("gone.json")]
        );
        // Someone edits gone.json on disk: a conflict, skipped by default.
        std::fs::write(dir.join("gone.json"), "theirs\n").unwrap();
        let Inspect::Found(f) = inspect(dir) else {
            panic!()
        };
        let plan = f.plan(false);
        assert_eq!(plan.skipped, [PathBuf::from("gone.json")]);
        assert_eq!(plan.dirty, [PathBuf::from("w/x.json")]);
        assert_eq!(f.plan(true).applied.len(), 2);
        // A tampered generation is broken.
        std::fs::write(adir.join("g2/w/x.json"), "tampered").unwrap();
        assert!(matches!(inspect(dir), Inspect::Broken(m) if m.contains("hash")));
        std::fs::remove_file(adir.join("g2/w/x.json")).unwrap();
        assert!(matches!(inspect(dir), Inspect::Broken(m) if m.contains("incomplete")));
        // Discard moves it aside, replacing an older discard.
        let d = discard(dir).unwrap();
        assert!(d.join("manifest.json").exists() && !adir.exists());
        assert!(matches!(inspect(dir), Inspect::None));
    }
}
