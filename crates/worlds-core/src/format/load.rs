use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde_json::Value;

use super::{
    EntitiesFile, EnumsFile, Error, FORMAT_VERSION, IntGridFile, LayersFile, NotesFile, Result,
    TilesetsFile, io_err,
};
use crate::model::{EntityDef, Level, Macros, Project, ProjectFiles, RuleSet, TilesLayer, World};

/// Files to read in place of the disk's, by project-relative path: `Some`
/// is the file's content, `None` means the file is absent (deleted). See
/// [`load_with_overlay`].
pub type Overlay = BTreeMap<PathBuf, Option<Vec<u8>>>;

/// Load a project directory. Missing optional files (`layers.json`,
/// `tilesets.json`, `enums.json`, per-level layer files, `entities.json`,
/// `notes.json`) mean empty. The result is normalized (see `Project::normalize`)
/// and remembers which files it was read from, with a hash of each (see
/// [`ProjectFiles`]).
pub fn load(dir: impl AsRef<Path>) -> Result<Project> {
    let dir = dir.as_ref();
    let src = Source {
        dir,
        overlay: None,
        read: RefCell::default(),
    };
    let mut project = load_from(&src)?;
    project.files = ProjectFiles::with_bases(dir, src.read.into_inner());
    Ok(project)
}

/// Load a project directory as if the files in `overlay` held its content
/// (or, for `None`, didn't exist): the editor's crash recovery, which loads
/// the disk with an autosave's files on top.
///
/// The result is rooted at `dir` like a plain [`load`], and what it knows
/// about the disk is the disk's: its known files are the managed files
/// really there (so an overlay deletion is deleted by the next save), each
/// with the hash of its bytes on disk. `dirty` becomes the dirty set (the
/// keys the autosaving session had, a level directory included), so the
/// next [`super::save_dirty`] writes and deletes exactly what that session's
/// save would have.
pub fn load_with_overlay(
    dir: impl AsRef<Path>,
    overlay: &Overlay,
    dirty: impl IntoIterator<Item = PathBuf>,
) -> Result<Project> {
    let dir = dir.as_ref();
    let src = Source {
        dir,
        overlay: Some(overlay),
        read: RefCell::default(),
    };
    let mut project = load_from(&src)?;
    let seen: BTreeSet<PathBuf> = src.read.into_inner().into_iter().map(|(p, _)| p).collect();
    let mut bases = BTreeMap::new();
    for rel in seen.iter().chain(overlay.keys()) {
        if bases.contains_key(rel) {
            continue;
        }
        match fs::read(dir.join(rel)) {
            Ok(bytes) => {
                bases.insert(rel.clone(), crate::autosave::hash(&bytes));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(io_err(&dir.join(rel))(e)),
        }
    }
    project.files = ProjectFiles::with_bases(dir, bases);
    project.files.restore_dirty(dirty.into_iter().collect());
    Ok(project)
}

/// Where [`load_from`] reads: the disk under `dir`, with `overlay` on top.
/// Records every file read (project-relative) with the hash of its bytes.
struct Source<'a> {
    dir: &'a Path,
    overlay: Option<&'a Overlay>,
    read: RefCell<Vec<(PathBuf, u64)>>,
}

impl Source<'_> {
    fn rel<'p>(&self, path: &'p Path) -> Option<&'p Path> {
        path.strip_prefix(self.dir).ok()
    }

    fn overlaid(&self, path: &Path) -> Option<&Option<Vec<u8>>> {
        self.overlay?.get(self.rel(path)?)
    }

    fn exists(&self, path: &Path) -> bool {
        match self.overlaid(path) {
            Some(o) => o.is_some(),
            None => path.exists(),
        }
    }

    /// A file's text; recorded as read.
    fn read_text(&self, path: &Path) -> Result<String> {
        let bytes = match self.overlaid(path) {
            Some(Some(b)) => b.clone(),
            Some(None) => {
                return Err(io_err(path)(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "deleted",
                )));
            }
            None => fs::read(path).map_err(io_err(path))?,
        };
        if let Some(rel) = self.rel(path) {
            self.read
                .borrow_mut()
                .push((rel.to_path_buf(), crate::autosave::hash(&bytes)));
        }
        String::from_utf8(bytes)
            .map_err(|e| io_err(path)(std::io::Error::new(std::io::ErrorKind::InvalidData, e)))
    }

    /// Entries of `dir` (sorted, dotfiles skipped) as (path, name, is_dir).
    /// Missing dir: none. With an overlay: its files in `dir` are added,
    /// its deletions removed, and a directory is listed when anything is
    /// left in it (a deleted level's directory is not).
    fn entries(&self, dir: &Path) -> Result<Vec<(PathBuf, String, bool)>> {
        let mut out: BTreeMap<String, (PathBuf, bool)> = BTreeMap::new();
        match fs::read_dir(dir) {
            Ok(rd) => {
                for entry in rd {
                    let entry = entry.map_err(io_err(dir))?;
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if !name.starts_with('.') {
                        let path = entry.path();
                        let is_dir = path.is_dir();
                        out.insert(name, (path, is_dir));
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(io_err(dir)(e)),
        }
        if let (Some(overlay), Some(rel_dir)) = (self.overlay, self.rel(dir)) {
            for (rel, content) in overlay {
                let Ok(rest) = rel.strip_prefix(rel_dir) else {
                    continue;
                };
                let mut parts = rest.iter();
                let Some(first) = parts.next() else {
                    continue;
                };
                let name = first.to_string_lossy().into_owned();
                if name.starts_with('.') {
                    continue;
                }
                let deeper = parts.next().is_some();
                match (content, deeper) {
                    (Some(_), false) => {
                        out.insert(name.clone(), (dir.join(&name), false));
                    }
                    (Some(_), true) => {
                        out.insert(name.clone(), (dir.join(&name), true));
                    }
                    (None, false) => {
                        out.remove(&name);
                    }
                    (None, true) => {}
                }
            }
            out.retain(|_, (path, is_dir)| !*is_dir || self.has_files(path));
        }
        Ok(out
            .into_iter()
            .map(|(name, (path, is_dir))| (path, name, is_dir))
            .collect())
    }

    /// Whether `dir` holds a file in the overlaid view (anywhere below it).
    fn has_files(&self, dir: &Path) -> bool {
        let Some(overlay) = self.overlay else {
            return true;
        };
        let Some(rel_dir) = self.rel(dir) else {
            return true;
        };
        if overlay
            .iter()
            .any(|(rel, c)| c.is_some() && rel.starts_with(rel_dir))
        {
            return true;
        }
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            let Ok(rd) = fs::read_dir(&d) else {
                continue;
            };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if !matches!(self.overlaid(&p), Some(None)) {
                    return true;
                }
            }
        }
        false
    }
}

fn load_from(src: &Source<'_>) -> Result<Project> {
    let dir = src.dir;
    let path = dir.join("project.json");
    let mut project: Project = read(src, &path)?;
    let path = dir.join("layers.json");
    if let Some(f) = read_opt::<LayersFile>(src, &path)? {
        project.layers = f.layers;
    }
    let path = dir.join("tilesets.json");
    if let Some(f) = read_opt::<TilesetsFile>(src, &path)? {
        project.tilesets = f.tilesets;
    }
    let path = dir.join("enums.json");
    if let Some(f) = read_opt::<EnumsFile>(src, &path)? {
        project.enums = f.enums;
    }

    for (path, stem) in json_files(src, &dir.join("entities"))? {
        let def: EntityDef = read(src, &path)?;
        check_name(&path, "slug", def.slug.as_str(), &stem)?;
        project.entities.push(def);
    }

    for (path, stem) in json_files(src, &dir.join("rules"))? {
        if stem == "macros" {
            project.macros = Some(read::<Macros>(src, &path)?);
        } else {
            let set: RuleSet = read(src, &path)?;
            check_name(&path, "layer", set.layer.as_str(), &stem)?;
            project.rules.push(set);
        }
    }

    for (world_dir, world_name) in subdirs(src, &dir.join("worlds"))? {
        let path = world_dir.join("world.json");
        let mut world: World = read(src, &path)?;
        check_name(&path, "slug", world.slug.as_str(), &world_name)?;
        for (level_dir, level_name) in subdirs(src, &world_dir.join("levels"))? {
            let level = load_level(src, &project, &level_dir, &level_name)?;
            world.levels.push(level);
        }
        project.worlds.push(world);
    }

    project.normalize();
    // Migration: a collision layer without a `blocking_values` list gets the
    // default blocking kinds it has (see `DEFAULT_BLOCKING_VALUES`).
    if project.collision_layer.is_some() && project.blocking_values.is_none() {
        project.blocking_values = project.default_blocking_values();
    }
    Ok(project)
}

/// A level from its directory.
fn load_level(src: &Source<'_>, project: &Project, dir: &Path, dir_name: &str) -> Result<Level> {
    let path = dir.join("level.json");
    let mut level: Level = read(src, &path)?;
    check_name(&path, "slug", level.slug.as_str(), dir_name)?;
    for (path, stem) in json_files(src, dir)? {
        match stem.as_str() {
            "level" => continue,
            "entities" => level.entities = read::<EntitiesFile>(src, &path)?.layers,
            "notes" => level.notes = read::<NotesFile>(src, &path)?.notes,
            layer => match project.layer(layer) {
                Some(def) if def.is_int_grid() => {
                    let f: IntGridFile = read(src, &path)?;
                    check_name(&path, "layer", f.layer.as_str(), layer)?;
                    let err = |(json_path, message)| Error::Parse {
                        path: path.clone(),
                        json_path,
                        message,
                    };
                    if def.is_derived() {
                        level
                            .overrides
                            .push(f.to_overrides(def, true).map_err(err)?);
                    } else {
                        level.int_grids.push(f.to_layer(def).map_err(err)?);
                    }
                }
                Some(def) if def.is_tiles() => {
                    let t: TilesLayer = read(src, &path)?;
                    check_name(&path, "layer", t.layer.as_str(), layer)?;
                    level.tiles.push(t);
                }
                Some(def) => {
                    let kind = def.kind.name();
                    return Err(Error::Layout {
                        path,
                        message: format!(
                            "layer {layer:?} is a {kind} layer, which has no per-level file"
                        ),
                    });
                }
                None => {
                    return Err(Error::Layout {
                        path,
                        message: format!(
                            "no layer named {layer:?} in layers.json (level files are \
                             level.json, entities.json, notes.json or <layer>.json)"
                        ),
                    });
                }
            },
        }
    }
    Ok(level)
}

fn check_name(path: &Path, field: &str, value: &str, expected: &str) -> Result<()> {
    if value == expected {
        Ok(())
    } else {
        Err(Error::Layout {
            path: path.to_path_buf(),
            message: format!(
                "{field} is {value:?} but the file or directory name says {expected:?}"
            ),
        })
    }
}

/// `*.json` files in `dir` (sorted, dotfiles skipped) with their stems. Missing dir: none.
fn json_files(src: &Source<'_>, dir: &Path) -> Result<Vec<(PathBuf, String)>> {
    Ok(src
        .entries(dir)?
        .into_iter()
        .filter(|(_, _, is_dir)| !is_dir)
        .filter_map(|(p, name, _)| {
            let stem = name.strip_suffix(".json")?.to_string();
            Some((p, stem))
        })
        .collect())
}

/// Subdirectories of `dir` (sorted, dotdirs skipped). Missing dir: none.
fn subdirs(src: &Source<'_>, dir: &Path) -> Result<Vec<(PathBuf, String)>> {
    Ok(src
        .entries(dir)?
        .into_iter()
        .filter(|(_, _, is_dir)| *is_dir)
        .map(|(p, name, _)| (p, name))
        .collect())
}

fn read_opt<T: DeserializeOwned>(src: &Source<'_>, path: &Path) -> Result<Option<T>> {
    if src.exists(path) {
        read(src, path).map(Some)
    } else {
        Ok(None)
    }
}

/// Read one file: check the `$schema` / `format` header, then parse the body
/// strictly, reporting the JSON path of the first error.
fn read<T: DeserializeOwned>(src: &Source<'_>, path: &Path) -> Result<T> {
    let text = src.read_text(path)?;
    parse(path, &text)
}

/// Parse one file's text as `T` with the same checks as `load`. `path` is
/// only used in error messages.
pub fn parse<T: DeserializeOwned>(path: &Path, text: &str) -> Result<T> {
    let err = |json_path: &str, message: String| Error::Parse {
        path: path.to_path_buf(),
        json_path: json_path.to_string(),
        message,
    };
    let value: Value = serde_json::from_str(text).map_err(|e| err("", e.to_string()))?;
    let Value::Object(mut map) = value else {
        return Err(err("", "expected a JSON object".into()));
    };
    match map.remove("$schema") {
        Some(Value::String(_)) => {}
        Some(_) => return Err(err("$schema", "expected a string".into())),
        None => return Err(err("", "missing \"$schema\"".into())),
    }
    match map.remove("format") {
        Some(Value::Number(n)) if n.as_u64() == Some(FORMAT_VERSION as u64) => {}
        Some(v) => {
            return Err(err(
                "format",
                format!("unsupported format {v}, expected {FORMAT_VERSION}"),
            ));
        }
        None => return Err(err("", "missing \"format\"".into())),
    }
    serde_path_to_error::deserialize(Value::Object(map)).map_err(|e| {
        let p = e.path().to_string();
        let p = if p == "." { String::new() } else { p };
        err(&p, e.into_inner().to_string())
    })
}
