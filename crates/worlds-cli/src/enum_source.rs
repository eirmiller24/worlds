//! `worlds-cli validate --enum-source <enum>=<file.json>[#<json-pointer>]`:
//! compare a Worlds enum with the game data file it mirrors (a
//! `theme` enum and `data/themes.json`, say) and report drift either way.
//! Game-agnostic: the caller names the enum, the file and where in it the
//! values are.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde_json::Value;

/// One `--enum-source` argument, parsed.
#[derive(Debug, Clone, PartialEq)]
pub struct EnumSource {
    pub spec: String,
    pub enum_slug: String,
    pub file: PathBuf,
    /// RFC 6901 JSON pointer; empty for the whole document.
    pub pointer: String,
}

impl EnumSource {
    /// `<enum>=<file.json>[#<json-pointer>]`.
    pub fn parse(spec: &str) -> Result<Self, String> {
        let bad =
            || format!("--enum-source {spec:?}: expected <enum>=<file.json>[#<json-pointer>]");
        let (slug, rest) = spec.split_once('=').ok_or_else(bad)?;
        let (file, pointer) = match rest.split_once('#') {
            Some((f, p)) => (f, p),
            None => (rest, ""),
        };
        if slug.is_empty() || file.is_empty() {
            return Err(bad());
        }
        if !pointer.is_empty() && !pointer.starts_with('/') {
            return Err(format!(
                "--enum-source {spec:?}: the JSON pointer must start with / (e.g. #/tracks)"
            ));
        }
        Ok(EnumSource {
            spec: spec.to_string(),
            enum_slug: slug.to_string(),
            file: PathBuf::from(file),
            pointer: pointer.to_string(),
        })
    }

    /// The values the game file defines: the keys of the object at the
    /// pointer (keys starting with `_` are comments and skipped), or the
    /// strings of the array there. Errs on I/O, bad JSON, a pointer that
    /// finds nothing, or any other shape.
    pub fn values(&self) -> Result<Vec<String>> {
        let text = std::fs::read_to_string(&self.file)
            .with_context(|| format!("reading {}", self.file.display()))?;
        let doc: Value = serde_json::from_str(&text)
            .with_context(|| format!("{}: not JSON", self.file.display()))?;
        let at = doc
            .pointer(&self.pointer)
            .with_context(|| format!("{}: nothing at #{}", self.file.display(), self.pointer))?;
        match at {
            Value::Object(o) => Ok(o.keys().filter(|k| !k.starts_with('_')).cloned().collect()),
            Value::Array(a) => a
                .iter()
                .map(|v| {
                    v.as_str().map(str::to_string).with_context(|| {
                        format!(
                            "{}#{}: an array of values must hold only strings",
                            self.file.display(),
                            self.pointer
                        )
                    })
                })
                .collect(),
            _ => bail!(
                "{}#{}: expected an object (its keys are the values) or an array of strings",
                self.file.display(),
                self.pointer
            ),
        }
    }
}

/// One `--enum-extra <enum>=<v1,v2,...>` argument: values the Worlds enum
/// may have that its source doesn't (a `theme` enum may have `none`, which
/// `data/themes.json` doesn't list because it means "no theme").
#[derive(Debug, Clone, PartialEq)]
pub struct EnumExtra {
    pub spec: String,
    pub enum_slug: String,
    pub values: Vec<String>,
}

impl EnumExtra {
    pub fn parse(spec: &str) -> Result<Self, String> {
        let bad = || format!("--enum-extra {spec:?}: expected <enum>=<value>[,<value>...]");
        let (slug, rest) = spec.split_once('=').ok_or_else(bad)?;
        let values: Vec<String> = rest.split(',').map(|v| v.trim().to_string()).collect();
        if slug.is_empty() || values.iter().any(String::is_empty) {
            return Err(bad());
        }
        Ok(EnumExtra {
            spec: spec.to_string(),
            enum_slug: slug.to_string(),
            values,
        })
    }
}

/// Drift between the project's enums and each source, as validate problems
/// on `enums.json`. A value listed by an `extras` entry for its enum isn't
/// drift when only the Worlds enum has it. Errs (a usage error) when an
/// enum doesn't exist, an extra names an enum no source compares, or a
/// source can't be read.
pub fn drift(
    project: &worlds_core::Project,
    sources: &[EnumSource],
    extras: &[EnumExtra],
) -> Result<Vec<worlds_core::Problem>> {
    for x in extras {
        if !sources.iter().any(|s| s.enum_slug == x.enum_slug) {
            bail!(
                "--enum-extra {:?}: no --enum-source compares enum {:?}",
                x.spec,
                x.enum_slug
            );
        }
    }
    let mut out = Vec::new();
    for src in sources {
        let Some((i, def)) = project
            .enums
            .iter()
            .enumerate()
            .find(|(_, e)| e.slug == src.enum_slug.as_str())
        else {
            bail!(
                "--enum-source {:?}: the project has no enum {:?}",
                src.spec,
                src.enum_slug
            );
        };
        let game = src.values()?;
        let extra = |v: &str| {
            extras
                .iter()
                .filter(|x| x.enum_slug == src.enum_slug)
                .any(|x| x.values.iter().any(|e| e == v))
        };
        let shown = if src.pointer.is_empty() {
            src.file.display().to_string()
        } else {
            format!("{}#{}", src.file.display(), src.pointer)
        };
        let problem = |path: String, message: String| worlds_core::Problem {
            file: "enums.json".into(),
            path,
            message,
        };
        for (j, v) in def.values.iter().enumerate() {
            if !game.iter().any(|g| g == v.slug.as_str()) && !extra(v.slug.as_str()) {
                out.push(problem(
                    format!("enums[{i}].values[{j}]"),
                    format!(
                        "enum {} has {:?}, which {shown} doesn't (enum source drift)",
                        def.slug,
                        v.slug.as_str()
                    ),
                ));
            }
        }
        for g in &game {
            if !def.has_value(g) {
                out.push(problem(
                    format!("enums[{i}]"),
                    format!(
                        "enum {} lacks {g:?}, which {shown} has (enum source drift)",
                        def.slug
                    ),
                ));
            }
        }
    }
    Ok(out)
}
