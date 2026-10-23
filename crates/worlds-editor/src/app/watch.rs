//! Watch the project directory (and tileset image directories outside it)
//! and report changes once things have been quiet for [`DEBOUNCE`].
//! [`OwnWrites`] tells the editor's own saves from external changes.

use std::collections::{BTreeSet, HashMap};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use notify::{EventKind, RecursiveMode, Watcher};

use crate::doc::Document;

pub const DEBOUNCE: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchChange {
    /// Project files changed (absolute paths): reload the document, unless
    /// [`OwnWrites`] explains every one of them.
    Project(Vec<PathBuf>),
    /// Tileset `i`'s image changed: re-upload it.
    Tileset(usize),
}

pub struct ProjectWatcher {
    _watcher: notify::RecommendedWatcher,
    rx: Receiver<notify::Result<notify::Event>>,
    pending: BTreeSet<PathBuf>,
    last_event: Option<Instant>,
    dir: PathBuf,
    tilesets: Vec<(usize, PathBuf)>,
}

/// The one form every path here is compared in: symlinks resolved as far
/// as the path exists (macOS reports `/private/var/...` for `/var/...`;
/// Windows canonical paths are `\\?\`-prefixed), so a deleted file still
/// matches what we recorded while it was there.
fn canon(p: &Path) -> PathBuf {
    worlds_core::export::resolve(p)
}

impl ProjectWatcher {
    /// `wake` is called from the watcher thread on every raw event (use it
    /// to request a repaint).
    pub fn new(doc: &Document, wake: impl Fn() + Send + 'static) -> notify::Result<Self> {
        let (tx, rx) = channel();
        let mut watcher = notify::recommended_watcher(move |res| {
            let _ = tx.send(res);
            wake();
        })?;
        let dir = canon(&doc.dir);
        watcher.watch(&dir, RecursiveMode::Recursive)?;
        let tilesets: Vec<(usize, PathBuf)> = doc
            .project
            .tilesets
            .iter()
            .enumerate()
            .map(|(i, t)| (i, canon(&doc.dir.join(&t.image))))
            .collect();
        let extra: BTreeSet<PathBuf> = tilesets
            .iter()
            .filter(|(_, p)| !p.starts_with(&dir))
            .filter_map(|(_, p)| p.parent().map(Path::to_path_buf))
            .collect();
        for d in extra {
            if let Err(e) = watcher.watch(&d, RecursiveMode::NonRecursive) {
                tracing::warn!("cannot watch {}: {e}", d.display());
            }
        }
        Ok(ProjectWatcher {
            _watcher: watcher,
            rx,
            pending: BTreeSet::new(),
            last_event: None,
            dir,
            tilesets,
        })
    }

    /// Drain events. Returns the changes once no event has arrived for
    /// [`DEBOUNCE`], plus how long to wait before asking again if some are
    /// still settling.
    pub fn poll(&mut self) -> (Vec<WatchChange>, Option<Duration>) {
        while let Ok(res) = self.rx.try_recv() {
            match res {
                Ok(ev) => {
                    if matches!(ev.kind, EventKind::Access(_)) {
                        continue;
                    }
                    for p in ev.paths {
                        if self.relevant(&p) {
                            self.pending.insert(p);
                            self.last_event = Some(Instant::now());
                        }
                    }
                }
                Err(e) => tracing::warn!("file watcher: {e}"),
            }
        }
        let Some(last) = self.last_event else {
            return (Vec::new(), None);
        };
        let quiet = last.elapsed();
        if quiet < DEBOUNCE {
            return (Vec::new(), Some(DEBOUNCE - quiet));
        }
        self.last_event = None;
        let mut out = Vec::new();
        let mut project = Vec::new();
        for p in std::mem::take(&mut self.pending) {
            let c = canon(&p);
            if let Some((i, _)) = self.tilesets.iter().find(|(_, t)| *t == c || *t == p) {
                let ch = WatchChange::Tileset(*i);
                if !out.contains(&ch) {
                    out.push(ch);
                }
            } else if p.extension().is_some_and(|e| e == "json") {
                project.push(p);
            }
        }
        if !project.is_empty() {
            out.push(WatchChange::Project(project));
        }
        (out, None)
    }

    /// Project JSON outside the generated/cache dirs, or a tileset image.
    fn relevant(&self, p: &Path) -> bool {
        if let Ok(rel) = p.strip_prefix(&self.dir) {
            let first = rel.components().next().and_then(|c| c.as_os_str().to_str());
            if matches!(first, Some(".worlds" | "export" | "schema" | ".git")) {
                return false;
            }
            if p.extension().is_some_and(|e| e == "json") {
                return true;
            }
        }
        p.extension().is_some_and(|e| e.eq_ignore_ascii_case("png"))
    }
}

/// What the editor's own saves left on disk: the content hash of every file
/// a save touched (`None`: deleted), and the level directories it rewrote
/// or removed wholesale. A watcher event is ours when the file still holds
/// exactly what we wrote, so the check doesn't depend on what is in memory
/// now (an undo right after a save must not look like an external edit),
/// and it reads only the changed files, never the whole project.
#[derive(Debug, Default)]
pub struct OwnWrites {
    files: HashMap<PathBuf, Option<u64>>,
    dirs: BTreeSet<PathBuf>,
}

fn content_hash(p: &Path) -> Option<u64> {
    let bytes = std::fs::read(p).ok()?;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    Some(h.finish())
}

impl OwnWrites {
    /// Record the state of `rels` (project-relative, as in
    /// `ProjectFiles::dirty`; a path without an extension is a level
    /// directory) right after saving them into `dir`.
    pub fn record<'a>(&mut self, dir: &Path, rels: impl IntoIterator<Item = &'a PathBuf>) {
        let dir = canon(dir);
        for rel in rels {
            // Component by component: `rel` may be `/`-separated, which a
            // `\\?\` path on Windows doesn't split on.
            let mut abs = dir.clone();
            abs.extend(rel.components());
            if rel.extension().is_some() {
                self.files.insert(abs.clone(), content_hash(&abs));
                continue;
            }
            let mut stack = vec![abs.clone()];
            while let Some(d) = stack.pop() {
                for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
                    let p = e.path();
                    if p.is_dir() {
                        stack.push(p);
                    } else {
                        self.files.insert(p.clone(), content_hash(&p));
                    }
                }
            }
            self.dirs.insert(abs);
        }
    }

    /// Whether `p` (absolute, as the watcher reports it) holds what our last
    /// save left there.
    pub fn explains(&self, p: &Path) -> bool {
        let c = canon(p);
        match self.files.get(&c) {
            Some(h) => *h == content_hash(p),
            // A file of a level directory we removed (or rewrote) that is
            // gone now.
            None => !p.exists() && self.dirs.iter().any(|d| c.starts_with(d)),
        }
    }

    pub fn clear(&mut self) {
        self.files.clear();
        self.dirs.clear();
    }
}

/// The changed paths as project-relative `entities/<name>.json` paths, if
/// every one of them is an entity definition file (directly in
/// `entities/`); `None` otherwise (the whole project reloads then).
pub fn entity_files(dir: &Path, paths: &[PathBuf]) -> Option<Vec<PathBuf>> {
    let dir = canon(dir);
    let mut out = Vec::new();
    for p in paths {
        let abs = canon(p);
        let rel = abs.strip_prefix(&dir).ok()?;
        let mut parts = rel.components();
        let (Some(first), Some(file), None) = (parts.next(), parts.next(), parts.next()) else {
            return None;
        };
        if first.as_os_str() != "entities"
            || Path::new(file.as_os_str())
                .extension()
                .is_none_or(|e| e != "json")
        {
            return None;
        }
        let rel = rel.to_path_buf();
        if !out.contains(&rel) {
            out.push(rel);
        }
    }
    (!out.is_empty()).then_some(out)
}

/// What disk changes to entity definition files mean for the project.
#[derive(Debug, Default)]
pub struct DiskEntities {
    /// One `set_entity_def` (file there) or `remove_entity_def {
    /// keep_instances }` (file gone) per changed file whose content differs
    /// from the project's, as one batch labelled `Disk: entities/<slug>.json`
    /// (several: `Disk: N entity files`). `None`: nothing to change.
    pub command: Option<worlds_core::Command>,
    /// The files the command covers (to mark clean once applied).
    pub files: Vec<PathBuf>,
    /// Files that didn't load: the old definition stays.
    pub problems: Vec<worlds_core::Problem>,
}

/// Read changed `entities/*.json` files (project-relative `rels`) with the
/// core loader's checks and turn them into commands (see
/// [`DiskEntities`]).
pub fn disk_entity_command(doc: &Document, rels: &[PathBuf]) -> DiskEntities {
    use worlds_core::{Command, EntityDef, Problem, Slug};
    let mut out = DiskEntities::default();
    let mut cmds = Vec::new();
    for rel in rels {
        let file = rel.to_string_lossy().replace('\\', "/");
        let stem = rel
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let abs = doc.dir.join(rel);
        let problem = |path: &str, message: String| Problem {
            file: file.clone(),
            path: path.to_string(),
            message,
        };
        match std::fs::read_to_string(&abs) {
            Ok(text) => match worlds_core::format::parse::<EntityDef>(&abs, &text) {
                Ok(def) if def.slug.as_str() != stem => out.problems.push(problem(
                    "slug",
                    format!(
                        "slug {:?} doesn't match the file name {stem:?}; not loaded",
                        def.slug.as_str()
                    ),
                )),
                Ok(def) => {
                    if doc.project.entity_def(def.slug.as_str()) != Some(&def) {
                        cmds.push(Command::SetEntityDef {
                            entity: def.slug.clone(),
                            def,
                        });
                        out.files.push(rel.clone());
                    }
                }
                Err(worlds_core::Error::Parse {
                    json_path, message, ..
                }) => out.problems.push(problem(
                    &json_path,
                    format!("{message}; the old definition stays"),
                )),
                Err(e) => out
                    .problems
                    .push(problem("", format!("{e}; the old definition stays"))),
            },
            Err(_) if !abs.exists() => {
                if let Ok(slug) = Slug::new(&stem)
                    && doc.project.entity_def(slug.as_str()).is_some()
                {
                    cmds.push(Command::RemoveEntityDef {
                        entity: slug,
                        keep_instances: true,
                    });
                    out.files.push(rel.clone());
                }
            }
            Err(e) => out.problems.push(problem(
                "",
                format!("cannot read: {e}; the old definition stays"),
            )),
        }
    }
    if !cmds.is_empty() {
        let label = match &out.files[..] {
            [one] => format!("Disk: {}", one.to_string_lossy().replace('\\', "/")),
            many => format!("Disk: {} entity files", many.len()),
        };
        out.command = Some(Command::Batch {
            label,
            commands: cmds,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_entity_definition_files_take_the_entity_path() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::create_dir_all(dir.join("entities/sub")).unwrap();
        std::fs::write(dir.join("entities/door.json"), "{}").unwrap();
        let rel = |s: &str| PathBuf::from(s);
        // A changed file and a deleted one (no longer there to canonicalize).
        assert_eq!(
            entity_files(
                dir,
                &[
                    dir.join("entities/door.json"),
                    dir.join("entities/gone.json")
                ]
            ),
            Some(vec![rel("entities/door.json"), rel("entities/gone.json")])
        );
        assert_eq!(
            entity_files(
                dir,
                &[
                    dir.join("entities/door.json"),
                    dir.join("entities/door.json")
                ]
            ),
            Some(vec![rel("entities/door.json")])
        );
        for other in [
            "entities/sub/x.json",
            "entities/notes.txt",
            "enums.json",
            "worlds/main/levels/a/entities.json",
        ] {
            assert_eq!(
                entity_files(dir, &[dir.join("entities/door.json"), dir.join(other)]),
                None,
                "{other}"
            );
        }
        assert_eq!(entity_files(dir, &[]), None);
    }

    #[test]
    fn own_writes_explain_only_what_we_left() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let level = dir.join("worlds/main/levels/a");
        std::fs::create_dir_all(&level).unwrap();
        std::fs::write(dir.join("project.json"), "{}").unwrap();
        std::fs::write(level.join("level.json"), "{ \"a\": 1 }").unwrap();
        std::fs::write(level.join("collision.json"), "[]").unwrap();
        let mut own = OwnWrites::default();
        let rels = [
            PathBuf::from("project.json"),
            PathBuf::from("worlds/main/levels/a"),
            PathBuf::from("worlds/main/levels/gone"),
        ];
        own.record(dir, &rels);
        assert!(own.explains(&dir.join("project.json")));
        assert!(own.explains(&level.join("collision.json")));
        // A deleted level's files are ours while they stay gone.
        assert!(own.explains(&dir.join("worlds/main/levels/gone/level.json")));
        // Someone else edits a file we saved, or one we never touched.
        std::fs::write(dir.join("project.json"), "{ \"name\": \"x\" }").unwrap();
        assert!(!own.explains(&dir.join("project.json")));
        std::fs::write(dir.join("layers.json"), "{}").unwrap();
        assert!(!own.explains(&dir.join("layers.json")));
        // Or brings a deleted level back.
        std::fs::create_dir_all(dir.join("worlds/main/levels/gone")).unwrap();
        std::fs::write(dir.join("worlds/main/levels/gone/level.json"), "{}").unwrap();
        assert!(!own.explains(&dir.join("worlds/main/levels/gone/level.json")));
        own.clear();
        assert!(!own.explains(&level.join("collision.json")));
    }

    /// The project opened through a symlink (as macOS's `/var` is
    /// `/private/var`) while the watcher reports real paths: our save is
    /// still ours, including the files of a level we deleted.
    #[cfg(unix)]
    #[test]
    fn own_writes_see_through_a_symlinked_project_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        let level = real.join("worlds/main/levels/a");
        std::fs::create_dir_all(&level).unwrap();
        std::fs::write(level.join("level.json"), "{}").unwrap();
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let mut own = OwnWrites::default();
        let rels = [
            PathBuf::from("worlds/main/levels/a"),
            PathBuf::from("worlds/main/levels/gone"),
        ];
        own.record(&link, &rels);
        for dir in [&real, &link] {
            assert!(own.explains(&dir.join("worlds/main/levels/a/level.json")));
            assert!(own.explains(&dir.join("worlds/main/levels/gone/level.json")));
        }
    }
}
