use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::{
    EntitiesFile, EnumsFile, Error, FORMAT_VERSION, FileKind, IntGridFile, LayersFile, NotesFile,
    RESERVED_LAYER_SLUGS, RESERVED_RULES_SLUG, Result, TilesetsFile, io_err, json,
};
use crate::model::{Level, Project, ProjectFiles};

/// Save a whole project directory: the same as [`save_all`].
///
/// Kept as the default because it is always right, even after the model
/// was edited directly. The editor, which only edits through commands,
/// calls [`save_dirty`].
pub fn save(project: &mut Project, dir: impl AsRef<Path>) -> Result<()> {
    save_all(project, dir)
}

/// Full save. Renders every file and writes only those whose bytes changed,
/// each atomically (temp file, fsync, rename, fsync the directory).
///
/// A file is deleted only if this project loaded it from (or saved it to)
/// `dir` and no longer produces it: a removed entity type, rule set, world,
/// level, layer content, `entities.json` or `notes.json`. Files the project
/// never saw are left alone, even inside managed directories, as are images,
/// `schema/`, `export/` and `.worlds/`. Afterwards `project.files` lists
/// what was written and the dirty set is empty.
pub fn save_all(project: &mut Project, dir: impl AsRef<Path>) -> Result<()> {
    let dir = dir.as_ref();
    let files = render(project)?;
    fs::create_dir_all(dir).map_err(io_err(dir))?;
    for (rel, bytes) in &files {
        write_if_changed(&dir.join(rel), bytes)?;
    }
    if let Some(known) = project.files.known_in(dir) {
        let stale: Vec<PathBuf> = known
            .iter()
            .filter(|rel| !files.contains_key(*rel))
            .cloned()
            .collect();
        remove_stale(dir, &stale)?;
    }
    project.files = ProjectFiles::with_bases(
        dir,
        files
            .into_iter()
            .map(|(rel, bytes)| (rel, crate::autosave::hash(&bytes))),
    );
    Ok(())
}

/// Incremental save: write only the files commands marked dirty (see
/// [`Project::is_dirty`]), without reading them back first, and delete the
/// dirty files the project no longer produces (a deleted or renamed level,
/// a layer whose content was cleared). Same bytes as [`save_all`] for every
/// file it writes.
///
/// Falls back to [`save_all`] when the project wasn't loaded from or last
/// saved to `dir`. Edits made to the model directly, not through
/// [`Project::apply`], are not seen: use [`save_all`] after those. On error
/// the dirty set is kept, so a retry writes everything again, and the files
/// written before the error are still recorded as known, so a later save can
/// delete them (a level created, half saved, then deleted).
pub fn save_dirty(project: &mut Project, dir: impl AsRef<Path>) -> Result<()> {
    let dir = dir.as_ref();
    let Some(known) = project.files.known_in(dir).cloned() else {
        return save_all(project, dir);
    };
    let dirty = project.files.take_dirty();
    let mut done = Progress::default();
    let res = save_paths(project, dir, &known, &dirty, &mut done);
    project.files.saved(done.written, &done.removed);
    if res.is_err() {
        project.files.restore_dirty(dirty);
    }
    res
}

/// What a save has done so far.
#[derive(Default)]
struct Progress {
    /// Written files with the hash of their bytes.
    written: Vec<(PathBuf, u64)>,
    removed: BTreeSet<PathBuf>,
}

/// What an incremental save of the project would do right now: the bytes
/// of every file it would write and the files it would delete (relative to
/// the project directory). See [`render_dirty`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DirtyRender {
    pub files: BTreeMap<PathBuf, Vec<u8>>,
    pub deleted: BTreeSet<PathBuf>,
}

/// Render what [`save_dirty`] would write and delete right now, touching
/// nothing on disk: the same function `save_dirty` writes from, so the
/// bytes are exactly the ones a save would produce. A dirty level directory
/// expands to all its files plus a deletion for every known file of it the
/// level no longer produces; a removed level, entity type, rule set or
/// world file is a deletion when the project knows the file.
///
/// The editor's autosave writes this. A project that wasn't loaded from or
/// saved to a directory (no [`ProjectFiles::root`]) renders every file, as
/// `save_dirty` would fall back to a full save.
pub fn render_dirty(project: &Project) -> Result<DirtyRender> {
    if project.files.root().is_none() {
        return Ok(DirtyRender {
            files: render(project)?,
            deleted: BTreeSet::new(),
        });
    }
    render_paths(project, project.files.known(), project.files.dirty())
}

/// Write and delete what `dirty` needs, recording each step in `done` as it
/// happens (so it is right even when this fails part way).
fn save_paths(
    project: &Project,
    dir: &Path,
    known: &BTreeSet<PathBuf>,
    dirty: &BTreeSet<PathBuf>,
    done: &mut Progress,
) -> Result<()> {
    let DirtyRender { files, deleted } = render_paths(project, known, dirty)?;
    for (rel, bytes) in files {
        write_atomic(&dir.join(&rel), &bytes)?;
        done.written.push((rel, crate::autosave::hash(&bytes)));
    }
    let stale: Vec<PathBuf> = deleted.iter().cloned().collect();
    remove_stale(dir, &stale)?;
    done.removed = deleted;
    Ok(())
}

/// The files `dirty` needs written (rendered) and deleted: the one
/// rendering behind both [`save_dirty`] and [`render_dirty`].
fn render_paths(
    project: &Project,
    known: &BTreeSet<PathBuf>,
    dirty: &BTreeSet<PathBuf>,
) -> Result<DirtyRender> {
    let expanded;
    let dirty = if dirty.iter().any(|p| is_world_dir(p)) {
        expanded = expand_world_dirs(project, known, dirty);
        &expanded
    } else {
        dirty
    };
    let mut out: BTreeMap<PathBuf, Vec<u8>> = BTreeMap::new();
    let mut gone: BTreeSet<PathBuf> = BTreeSet::new();
    // Each level is rendered at most once, however many of its files are dirty.
    let mut levels: BTreeMap<PathBuf, BTreeMap<PathBuf, Vec<u8>>> = BTreeMap::new();
    let mut full: Option<BTreeMap<PathBuf, Vec<u8>>> = None;
    // What each dirty level needs rendered: all of it (a level directory
    // key) or only some files (a one-cell paint renders one file, not the
    // level's every layer).
    let mut wanted: BTreeMap<PathBuf, Option<BTreeSet<PathBuf>>> = BTreeMap::new();
    for rel in dirty {
        let parts: Vec<&str> = rel.iter().filter_map(|c| c.to_str()).collect();
        if let ["worlds", w, "levels", l, rest @ ..] = parts.as_slice()
            && rest.len() <= 1
        {
            let ldir = PathBuf::from("worlds").join(w).join("levels").join(l);
            let e = wanted.entry(ldir).or_insert_with(|| Some(BTreeSet::new()));
            match (rest.is_empty(), e) {
                (true, e) => *e = None,
                (false, Some(files)) => {
                    files.insert(rel.clone());
                }
                (false, None) => {}
            }
        }
    }
    for rel in dirty {
        let parts: Vec<&str> = rel.iter().filter_map(|c| c.to_str()).collect();
        match parts.as_slice() {
            ["project.json"] => {
                out.insert(rel.clone(), to_bytes(rel, FileKind::Project, project)?);
            }
            ["layers.json"] => {
                let body = LayersFile {
                    layers: project.layers.clone(),
                };
                out.insert(rel.clone(), to_bytes(rel, FileKind::Layers, &body)?);
            }
            ["tilesets.json"] => {
                let body = TilesetsFile {
                    tilesets: project.tilesets.clone(),
                };
                out.insert(rel.clone(), to_bytes(rel, FileKind::Tilesets, &body)?);
            }
            ["enums.json"] => {
                let body = EnumsFile {
                    enums: project.enums.clone(),
                };
                out.insert(rel.clone(), to_bytes(rel, FileKind::Enums, &body)?);
            }
            ["entities", file] if file.ends_with(".json") => {
                match project.entity_def(file.trim_end_matches(".json")) {
                    Some(def) => {
                        out.insert(rel.clone(), to_bytes(rel, FileKind::Entity, def)?);
                    }
                    None => {
                        gone.insert(rel.clone());
                    }
                }
            }
            ["assemblies", file] if file.ends_with(".json") => {
                match project.assembly(file.trim_end_matches(".json")) {
                    Some(a) => {
                        out.insert(rel.clone(), to_bytes(rel, FileKind::Assembly, a)?);
                    }
                    None => {
                        gone.insert(rel.clone());
                    }
                }
            }
            ["rules", file] if file.ends_with(".json") && *file != "macros.json" => {
                match project.rules_for(file.trim_end_matches(".json")) {
                    Some(set) => {
                        out.insert(rel.clone(), to_bytes(rel, FileKind::Rules, set)?);
                    }
                    None => {
                        gone.insert(rel.clone());
                    }
                }
            }
            ["worlds", w, "world.json"] => match project.world(w) {
                Some(world) => {
                    out.insert(rel.clone(), to_bytes(rel, FileKind::World, world)?);
                }
                None => {
                    gone.insert(rel.clone());
                }
            },
            ["worlds", w, "levels", l, rest @ ..] if rest.len() <= 1 => {
                let ldir = PathBuf::from("worlds").join(w).join("levels").join(l);
                if !levels.contains_key(&ldir) {
                    let files = match project.world(w).and_then(|x| x.level(l)) {
                        Some(level) => {
                            let only = wanted.get(&ldir).cloned().flatten();
                            let mut level = match &only {
                                Some(files) => {
                                    level.copy_with(|name| files.contains(&ldir.join(name)))
                                }
                                None => level.clone(),
                            };
                            level.normalize();
                            let mut f = Files(BTreeMap::new(), only);
                            render_level(
                                &mut f,
                                project,
                                &format!("worlds/{w}/levels/{l}"),
                                &level,
                            )?;
                            f.0
                        }
                        None => BTreeMap::new(),
                    };
                    levels.insert(ldir.clone(), files);
                }
                let files = &levels[&ldir];
                if rest.is_empty() {
                    // The whole level directory.
                    out.extend(files.iter().map(|(k, v)| (k.clone(), v.clone())));
                    gone.extend(
                        known
                            .iter()
                            .filter(|k| k.starts_with(&ldir) && !files.contains_key(*k))
                            .cloned(),
                    );
                } else {
                    match files.get(rel) {
                        Some(bytes) => {
                            out.insert(rel.clone(), bytes.clone());
                        }
                        None => {
                            gone.insert(rel.clone());
                        }
                    }
                }
            }
            _ => {
                // Not produced by commands; render everything once.
                let all = match &mut full {
                    Some(all) => all,
                    None => full.insert(render(project)?),
                };
                match all.get(rel) {
                    Some(bytes) => {
                        out.insert(rel.clone(), bytes.clone());
                    }
                    None => {
                        gone.insert(rel.clone());
                    }
                }
            }
        }
    }
    gone.retain(|p| known.contains(p) && !out.contains_key(p));
    Ok(DirtyRender {
        files: out,
        deleted: gone,
    })
}

/// `worlds/<world>`: a whole world directory (a created, removed or
/// renamed world).
fn is_world_dir(rel: &Path) -> bool {
    let parts: Vec<_> = rel.iter().collect();
    parts.len() == 2 && parts[0] == "worlds"
}

/// `dirty` with each whole world directory replaced by its `world.json`
/// and a whole-level entry for every level the world has, in the project
/// or among the known files (so saving deletes those it no longer has).
fn expand_world_dirs(
    project: &Project,
    known: &BTreeSet<PathBuf>,
    dirty: &BTreeSet<PathBuf>,
) -> BTreeSet<PathBuf> {
    let mut out = BTreeSet::new();
    for rel in dirty {
        if !is_world_dir(rel) {
            out.insert(rel.clone());
            continue;
        }
        out.insert(rel.join("world.json"));
        let w = rel.iter().nth(1).and_then(|c| c.to_str()).unwrap_or("");
        for l in project.world(w).into_iter().flat_map(|w| &w.levels) {
            out.insert(rel.join("levels").join(l.slug.as_str()));
        }
        let levels = rel.join("levels");
        for k in known {
            if let Ok(rest) = k.strip_prefix(&levels)
                && let Some(l) = rest.iter().next()
                && rest.iter().count() > 1
            {
                out.insert(levels.join(l));
            }
        }
    }
    out
}

/// Every file a full save of `project` would write right now, as (path
/// relative to the project directory, bytes), unsaved edits included.
/// Nothing is read from or written to disk. The live API's
/// `project.read_file` serves files from this.
pub fn render_files(project: &Project) -> Result<BTreeMap<PathBuf, Vec<u8>>> {
    render(project)
}

/// One file of [`render_files`], rendered on its own (nothing else of the
/// project is serialized): the same bytes, or `None` when `rel` (relative to
/// the project directory) isn't a file a save would write. Errors are those
/// of the file itself, or of its level's other files.
pub fn render_file(project: &Project, rel: &Path) -> Result<Option<Vec<u8>>> {
    let Some(parts) = rel
        .iter()
        .map(|c| c.to_str())
        .collect::<Option<Vec<&str>>>()
    else {
        return Ok(None);
    };
    let mut out = Files(BTreeMap::new(), Some(BTreeSet::from([rel.to_path_buf()])));
    match parts.as_slice() {
        ["project.json"] => out.add("project.json", FileKind::Project, project)?,
        ["layers.json"] => out.add(
            "layers.json",
            FileKind::Layers,
            &LayersFile {
                layers: project.layers.clone(),
            },
        )?,
        ["tilesets.json"] => out.add(
            "tilesets.json",
            FileKind::Tilesets,
            &TilesetsFile {
                tilesets: project.tilesets.clone(),
            },
        )?,
        ["enums.json"] => out.add(
            "enums.json",
            FileKind::Enums,
            &EnumsFile {
                enums: project.enums.clone(),
            },
        )?,
        ["entities", file] => {
            for e in project
                .entities
                .iter()
                .filter(|e| e.slug.as_str() == stem(file))
            {
                out.add(format!("entities/{}.json", e.slug), FileKind::Entity, e)?;
            }
        }
        ["assemblies", file] => {
            for a in project
                .assemblies
                .iter()
                .filter(|a| a.slug.as_str() == stem(file))
            {
                out.add(format!("assemblies/{}.json", a.slug), FileKind::Assembly, a)?;
            }
        }
        ["rules", "macros.json"] => {
            if let Some(m) = &project.macros {
                out.add("rules/macros.json", FileKind::Macros, m)?;
            }
        }
        ["rules", file] => {
            for r in project
                .rules
                .iter()
                .filter(|r| r.layer.as_str() == stem(file))
            {
                out.add(format!("rules/{}.json", r.layer), FileKind::Rules, r)?;
            }
        }
        ["worlds", world, "world.json"] => {
            for w in project.worlds.iter().filter(|w| w.slug.as_str() == *world) {
                out.add(format!("worlds/{}/world.json", w.slug), FileKind::World, w)?;
            }
        }
        ["worlds", world, "levels", level, _] => {
            let levels = project
                .worlds
                .iter()
                .filter(|w| w.slug.as_str() == *world)
                .flat_map(|w| &w.levels)
                .filter(|l| l.slug.as_str() == *level);
            for l in levels {
                let mut l = l.clone();
                l.normalize();
                render_level(
                    &mut out,
                    project,
                    &format!("worlds/{world}/levels/{level}"),
                    &l,
                )?;
            }
        }
        _ => {}
    }
    Ok(out.0.remove(rel))
}

/// `door` of `door.json` (anything else matches no slug).
fn stem(file: &str) -> &str {
    file.strip_suffix(".json").unwrap_or("")
}

/// Every project file as (path relative to the project dir, bytes).
pub(crate) fn render(project: &Project) -> Result<BTreeMap<PathBuf, Vec<u8>>> {
    let mut p = project.clone();
    p.normalize();
    let mut out = Files::default();

    out.add("project.json", FileKind::Project, &p)?;
    out.add(
        "layers.json",
        FileKind::Layers,
        &LayersFile {
            layers: p.layers.clone(),
        },
    )?;
    out.add(
        "tilesets.json",
        FileKind::Tilesets,
        &TilesetsFile {
            tilesets: p.tilesets.clone(),
        },
    )?;
    out.add(
        "enums.json",
        FileKind::Enums,
        &EnumsFile {
            enums: p.enums.clone(),
        },
    )?;
    for e in &p.entities {
        out.add(format!("entities/{}.json", e.slug), FileKind::Entity, e)?;
    }
    for r in &p.rules {
        if r.layer == RESERVED_RULES_SLUG {
            return Err(layout(
                "rules/macros.json",
                "a rule set can't be named \"macros\"",
            ));
        }
        out.add(format!("rules/{}.json", r.layer), FileKind::Rules, r)?;
    }
    if let Some(m) = &p.macros {
        out.add("rules/macros.json", FileKind::Macros, m)?;
    }
    for a in &p.assemblies {
        out.add(format!("assemblies/{}.json", a.slug), FileKind::Assembly, a)?;
    }
    for w in &p.worlds {
        let wdir = format!("worlds/{}", w.slug);
        out.add(format!("{wdir}/world.json"), FileKind::World, w)?;
        for l in &w.levels {
            render_level(&mut out, &p, &format!("{wdir}/levels/{}", l.slug), l)?;
        }
    }
    Ok(out.0)
}

fn render_level(out: &mut Files, p: &Project, ldir: &str, l: &Level) -> Result<()> {
    if out.wants(&format!("{ldir}/level.json")) {
        out.add(format!("{ldir}/level.json"), FileKind::Level, l)?;
    }
    let reserved = |layer: &str, rel: &str| -> Result<()> {
        if RESERVED_LAYER_SLUGS.contains(&layer) {
            Err(layout(rel, &format!("layer slug {layer:?} is reserved")))
        } else {
            Ok(())
        }
    };
    for g in &l.int_grids {
        let rel = format!("{ldir}/{}.json", g.layer);
        if !out.wants(&rel) {
            continue;
        }
        reserved(g.layer.as_str(), &rel)?;
        let Some(def) = p.layer(g.layer.as_str()).filter(|d| d.is_int_grid()) else {
            return Err(layout(
                &rel,
                &format!("no int_grid layer {:?} in layers.json", g.layer.as_str()),
            ));
        };
        if def.is_derived() {
            return Err(layout(
                &rel,
                &format!(
                    "layer {:?} is derived; its content is overrides, not a grid",
                    g.layer.as_str()
                ),
            ));
        }
        let file = IntGridFile::from_layer(g, def).map_err(|m| layout(&rel, &m))?;
        out.add(rel, FileKind::IntGrid, &file)?;
    }
    for o in &l.overrides {
        let rel = format!("{ldir}/{}.json", o.layer);
        if !out.wants(&rel) {
            continue;
        }
        reserved(o.layer.as_str(), &rel)?;
        let Some(def) = p.layer(o.layer.as_str()).filter(|d| d.is_derived()) else {
            return Err(layout(
                &rel,
                &format!(
                    "no derived int_grid layer {:?} in layers.json",
                    o.layer.as_str()
                ),
            ));
        };
        // Empty layers go away: a file of all `.` is not written.
        if o.is_empty() {
            continue;
        }
        let file = IntGridFile::from_overrides(o, def).map_err(|m| layout(&rel, &m))?;
        out.add(rel, FileKind::IntGrid, &file)?;
    }
    for t in &l.tiles {
        let rel = format!("{ldir}/{}.json", t.layer);
        if !out.wants(&rel) {
            continue;
        }
        reserved(t.layer.as_str(), &rel)?;
        out.add(rel, FileKind::Tiles, t)?;
    }
    if !l.entities.is_empty() && out.wants(&format!("{ldir}/entities.json")) {
        out.add(
            format!("{ldir}/entities.json"),
            FileKind::Entities,
            &EntitiesFile {
                layers: l.entities.clone(),
            },
        )?;
    }
    if !l.notes.is_empty() && out.wants(&format!("{ldir}/notes.json")) {
        out.add(
            format!("{ldir}/notes.json"),
            FileKind::Notes,
            &NotesFile {
                notes: l.notes.clone(),
            },
        )?;
    }
    Ok(())
}

#[derive(Default)]
struct Files(BTreeMap<PathBuf, Vec<u8>>, Option<BTreeSet<PathBuf>>);

impl Files {
    /// Whether `rel` is rendered (all are, unless a set of files is
    /// asked for).
    fn wants(&self, rel: &str) -> bool {
        self.1
            .as_ref()
            .is_none_or(|only| only.contains(Path::new(rel)))
    }

    fn add<T: Serialize>(
        &mut self,
        rel: impl Into<PathBuf>,
        kind: FileKind,
        body: &T,
    ) -> Result<()> {
        let rel = rel.into();
        if self.1.as_ref().is_some_and(|only| !only.contains(&rel)) {
            return Ok(());
        }
        let bytes = to_bytes(&rel, kind, body)?;
        if self.0.insert(rel.clone(), bytes).is_some() {
            return Err(Error::Layout {
                path: rel,
                message: "two items would be saved to this file (duplicate slug)".into(),
            });
        }
        Ok(())
    }
}

#[derive(Serialize)]
struct FileOut<'a, T> {
    #[serde(rename = "$schema")]
    schema: &'a str,
    format: u32,
    #[serde(flatten)]
    body: &'a T,
}

fn to_bytes<T: Serialize>(rel: &Path, kind: FileKind, body: &T) -> Result<Vec<u8>> {
    let schema = kind.schema_ref(rel);
    let out = FileOut {
        schema: &schema,
        format: FORMAT_VERSION,
        body,
    };
    let compact = serde_json::to_string(&out).map_err(|e| Error::Parse {
        path: rel.to_path_buf(),
        json_path: String::new(),
        message: format!("serialize: {e}"),
    })?;
    Ok(json::pretty(&compact).into_bytes())
}

fn layout(rel: &str, message: &str) -> Error {
    Error::Layout {
        path: rel.into(),
        message: message.into(),
    }
}

pub(crate) fn write_if_changed(path: &Path, bytes: &[u8]) -> Result<()> {
    if fs::read(path).is_ok_and(|old| old == bytes) {
        return Ok(());
    }
    write_atomic(path, bytes)
}

/// Write a file atomically: temp file, fsync, rename, fsync the directory.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().expect("project files live in a directory");
    fs::create_dir_all(parent).map_err(io_err(parent))?;
    let name = path.file_name().expect("file name").to_string_lossy();
    let tmp = parent.join(format!(".{name}.tmp"));
    let write = || -> std::io::Result<()> {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        fs::rename(&tmp, path)
    };
    write().map_err(|e| {
        let _ = fs::remove_file(&tmp);
        io_err(path)(e)
    })?;
    // Make the rename itself durable.
    sync_dir(parent).map_err(io_err(parent))
}

/// fsync a directory so entries created, renamed or removed in it survive a
/// crash. Unix only: elsewhere (Windows) a directory can't be opened as a
/// file, and the filesystem doesn't need it.
pub(crate) fn sync_dir(dir: &Path) -> std::io::Result<()> {
    if cfg!(unix) {
        fs::File::open(dir)?.sync_all()
    } else {
        Ok(())
    }
}

/// Delete `stale` files (relative to `dir`), then each parent directory
/// they leave empty, up to (not including) `dir`. Every surviving directory
/// that lost an entry is synced, deepest first. Returns those directories,
/// relative to `dir` (`""` for `dir` itself).
fn remove_stale(dir: &Path, stale: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut parents = BTreeSet::new();
    let mut touched = BTreeSet::new();
    for rel in stale {
        let path = dir.join(rel);
        match fs::remove_file(&path) {
            Ok(()) => {
                touched.insert(rel.parent().unwrap_or(Path::new("")).to_path_buf());
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(io_err(&path)(e)),
        }
        let mut p = rel.parent();
        while let Some(d) = p.filter(|d| !d.as_os_str().is_empty()) {
            parents.insert(d.to_path_buf());
            p = d.parent();
        }
    }
    // Deepest first; only succeeds on empty directories.
    let mut parents: Vec<PathBuf> = parents.into_iter().collect();
    parents.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
    for d in parents {
        if fs::remove_dir(dir.join(&d)).is_ok() {
            touched.remove(&d);
            touched.insert(d.parent().unwrap_or(Path::new("")).to_path_buf());
        }
    }
    let mut touched: Vec<PathBuf> = touched.into_iter().collect();
    touched.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
    for d in &touched {
        let full = dir.join(d);
        sync_dir(&full).map_err(io_err(&full))?;
    }
    Ok(touched)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_removal_syncs_every_directory_that_lost_an_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        for f in [
            "a/b/x.json",
            "a/y.json",
            "c/z.json",
            "c/keep.json",
            "top.json",
        ] {
            let p = dir.join(f);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, "{}").unwrap();
        }
        let stale: Vec<PathBuf> = ["a/b/x.json", "a/y.json", "c/z.json", "gone/never.json"]
            .iter()
            .map(PathBuf::from)
            .collect();
        let synced = remove_stale(dir, &stale).unwrap();
        // `a/b` and `a` emptied and went away, so their parents are synced
        // instead; `c` lost a file but stays; the root lost `a`.
        assert!(!dir.join("a").exists());
        assert!(dir.join("c/keep.json").is_file());
        assert_eq!(synced, [PathBuf::from("c"), PathBuf::from("")]);
        // Nothing removed, nothing synced.
        assert!(remove_stale(dir, &stale).unwrap().is_empty());
    }
}
