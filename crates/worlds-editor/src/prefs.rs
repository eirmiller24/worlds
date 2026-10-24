//! The user's preferences file: settings that belong to the person, not
//! the project (so not `.worlds/ui.json`, which travels with the project).
//!
//! It lives at `<config dir>/worlds/preferences.json`:
//!
//! - Linux and the BSDs: `$XDG_CONFIG_HOME/worlds/`, else `~/.config/worlds/`
//! - macOS: `~/Library/Application Support/worlds/`
//! - Windows: `%APPDATA%\worlds\`
//!
//! `WORLDS_CONFIG_DIR` replaces the whole `<config dir>/worlds` part (tests
//! and portable installs use it).
//!
//! Today it holds one thing, the shortcuts that differ from the defaults:
//!
//! ```json
//! { "shortcuts": { "undo": ["Ctrl+U"], "view.grid": [] } }
//! ```
//!
//! Only changed bindings are written, so the file stays small and a new
//! default in a later version reaches everyone who didn't change that key.
//! An empty list means "unbound". Keys this version doesn't know (a newer
//! Worlds wrote them) are kept as they are when the file is written back.
//!
//! A missing file is the defaults. A file that can't be read or parsed is
//! the defaults too, with a warning in the log, never a crash; before the
//! first save over it, it is copied to `preferences.json.bad` so nothing
//! the user wrote by hand is lost.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::keymap::Keymap;

/// The file's name in the config directory.
pub const FILE_NAME: &str = "preferences.json";

/// Replaces the config directory (the `worlds` folder itself).
pub const ENV_DIR: &str = "WORLDS_CONFIG_DIR";

/// This platform's per-user config directory for Worlds, honouring
/// [`ENV_DIR`]. `None` when there is no home to put it in.
pub fn config_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os(ENV_DIR).filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(d));
    }
    platform_config_dir().map(|d| d.join("worlds"))
}

/// Where the preferences file is (see the module docs).
pub fn default_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join(FILE_NAME))
}

#[cfg(not(target_os = "windows"))]
fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

#[cfg(target_os = "windows")]
fn platform_config_dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
}

#[cfg(target_os = "macos")]
fn platform_config_dir() -> Option<PathBuf> {
    home().map(|h| h.join("Library/Application Support"))
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn platform_config_dir() -> Option<PathBuf> {
    // XDG says a relative XDG_CONFIG_HOME is invalid and to be ignored.
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|d| d.is_absolute())
        .or_else(|| home().map(|h| h.join(".config")))
}

/// The file as JSON.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct File {
    /// Command id → chords, for the bindings that aren't the default.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    shortcuts: BTreeMap<String, Vec<String>>,
    /// Whatever else is in there (a newer version's settings).
    #[serde(flatten)]
    other: serde_json::Map<String, serde_json::Value>,
}

/// The preferences file and what was read from it.
#[derive(Debug, Default)]
pub struct Store {
    /// `None`: no file at all (tests, or no home directory): the defaults,
    /// nothing saved.
    pub path: Option<PathBuf>,
    /// Never write (`--screenshot` and other headless runs read only).
    pub read_only: bool,
    /// What the file had that this version doesn't use, written back as is.
    other: serde_json::Map<String, serde_json::Value>,
    /// Shortcut ids from the file this version doesn't know, kept.
    unknown: BTreeMap<String, Vec<String>>,
    /// The file was there but unreadable: back it up before overwriting.
    broken: bool,
    /// The last save failed with this (shown once, not every change).
    pub last_error: Option<String>,
}

impl Store {
    /// Read `path` (`None`: no file). Problems are logged and give the
    /// defaults.
    pub fn load(path: Option<PathBuf>, read_only: bool) -> (Store, Keymap) {
        let mut store = Store {
            path,
            read_only,
            ..Default::default()
        };
        let Some(path) = store.path.clone() else {
            return (store, Keymap::defaults());
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::info!(
                    "no preferences file at {}: default shortcuts",
                    path.display()
                );
                return (store, Keymap::defaults());
            }
            Err(e) => {
                tracing::warn!(
                    "can't read {}: {e}; using the default shortcuts",
                    path.display()
                );
                store.broken = true;
                return (store, Keymap::defaults());
            }
        };
        let file: File = match serde_json::from_str(&text) {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!(
                    "{} isn't valid preferences ({e}); using the default shortcuts",
                    path.display()
                );
                store.broken = true;
                return (store, Keymap::defaults());
            }
        };
        let (km, problems) = Keymap::with_overrides(&file.shortcuts);
        for p in &problems {
            tracing::warn!("{}: {p}", path.display());
        }
        store.unknown = file
            .shortcuts
            .into_iter()
            .filter(|(id, _)| crate::keymap::Cmd::from_id(id).is_none())
            .collect();
        store.other = file.other;
        tracing::info!("preferences read from {}", path.display());
        (store, km)
    }

    /// Write `km`'s non-default bindings (and whatever else the file had).
    /// Does nothing without a path or when read-only.
    pub fn save(&mut self, km: &Keymap) -> Result<(), String> {
        let Some(path) = self.path.clone() else {
            return Ok(());
        };
        if self.read_only {
            return Ok(());
        }
        let r = self.write(&path, km);
        match &r {
            Ok(()) => self.last_error = None,
            Err(e) => {
                tracing::warn!("{e}");
                self.last_error = Some(e.clone());
            }
        }
        r
    }

    fn write(&mut self, path: &Path, km: &Keymap) -> Result<(), String> {
        let mut shortcuts = km.overrides();
        for (id, v) in &self.unknown {
            shortcuts.entry(id.clone()).or_insert_with(|| v.clone());
        }
        let file = File {
            shortcuts,
            other: self.other.clone(),
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("can't create {}: {e}", dir.display()))?;
        }
        if self.broken {
            let bad = path.with_extension("json.bad");
            if std::fs::copy(path, &bad).is_ok() {
                tracing::warn!("kept the unreadable preferences as {}", bad.display());
            }
            self.broken = false;
        }
        let mut text = serde_json::to_string_pretty(&file).expect("preferences serialize");
        text.push('\n');
        // Write a sibling and rename it over, so a crash never leaves half
        // a file.
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text).map_err(|e| format!("can't write {}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, path).map_err(|e| format!("can't write {}: {e}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::{Chord, Cmd};

    #[test]
    fn missing_file_is_the_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let (store, km) = Store::load(Some(tmp.path().join(FILE_NAME)), false);
        assert_eq!(km, Keymap::defaults());
        assert!(!store.broken);
        let (_, km) = Store::load(None, false);
        assert_eq!(km, Keymap::defaults());
    }

    #[test]
    fn the_file_holds_only_changes_and_loads_over_the_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("sub").join(FILE_NAME);
        let (mut store, mut km) = Store::load(Some(path.clone()), false);
        km.set(Cmd::Undo, vec![Chord::parse("Ctrl+U").unwrap()]);
        km.set(Cmd::ToggleGrid, vec![]);
        store.save(&km).unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let shortcuts = v["shortcuts"].as_object().unwrap();
        assert_eq!(shortcuts.len(), 2, "{v}");
        assert_eq!(v["shortcuts"]["view.grid"], serde_json::json!([]));
        assert_eq!(
            v["shortcuts"]["undo"],
            serde_json::json!([Chord::parse("Ctrl+U").unwrap().to_string()])
        );
        let (_, back) = Store::load(Some(path.clone()), false);
        assert_eq!(back, km);
        // Everything back to the defaults: an empty file, not a stale one.
        km.reset_all();
        store.save(&km).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap().trim(), "{}");
    }

    #[test]
    fn unknown_entries_survive_a_save() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(FILE_NAME);
        std::fs::write(
            &path,
            r#"{ "shortcuts": { "future.thing": ["Ctrl+K"], "undo": ["F9"] }, "theme": "dark" }"#,
        )
        .unwrap();
        let (mut store, km) = Store::load(Some(path.clone()), false);
        assert_eq!(km.chords(Cmd::Undo), &[Chord::parse("F9").unwrap()]);
        store.save(&km).unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(v["theme"], "dark");
        assert_eq!(
            v["shortcuts"]["future.thing"],
            serde_json::json!(["Ctrl+K"])
        );
    }

    #[test]
    fn a_broken_file_is_the_defaults_and_kept_aside() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(FILE_NAME);
        std::fs::write(&path, "{ not json").unwrap();
        let (mut store, km) = Store::load(Some(path.clone()), false);
        assert_eq!(km, Keymap::defaults());
        store.save(&km).unwrap();
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("preferences.json.bad")).unwrap(),
            "{ not json"
        );
        assert!(std::fs::read_to_string(&path).unwrap().contains('{'));
    }

    #[test]
    fn read_only_never_writes() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(FILE_NAME);
        let (mut store, mut km) = Store::load(Some(path.clone()), true);
        km.set(Cmd::Undo, vec![]);
        store.save(&km).unwrap();
        assert!(!path.exists());
    }
}
