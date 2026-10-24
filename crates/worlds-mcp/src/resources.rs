//! `worlds://` resources (`docs/mcp.md`, "Resources"): project files read
//! through `project.read_file` (so unsaved editor edits are included and
//! nothing is read from the filesystem by path), and the JSON Schemas.

use serde_json::{Value, json};
use worlds_core::format::FileKind;

use crate::engine::Engine;

/// The resource templates: (URI template, name, description).
pub const TEMPLATES: &[(&str, &str, &str)] = &[
    (
        "worlds://defs/{file}",
        "definition file",
        "layers.json, tilesets.json, enums.json, entities/<slug>.json, rules/<layer>.json or rules/macros.json",
    ),
    (
        "worlds://worlds/{world}/world.json",
        "world file",
        "a world's world.json: its guide groups and world-scope annotations (pins, paths, regions, guides)",
    ),
    (
        "worlds://levels/{slug}/{file}",
        "level file",
        "level.json, <layer>.json, entities.json or notes.json of a level",
    ),
    (
        "worlds://templates/{name}.json",
        "built-in template",
        "a built-in rule template (blob47), as a template in rules/macros.json is written",
    ),
    (
        "worlds://schema/{name}.json",
        "JSON Schema",
        "the schema of a file kind (project, layers, tilesets, enums, entity, rules, macros, world, level, intgrid, tiles, entities, notes) or command",
    ),
];

/// One listed resource: URI and a short name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    pub uri: String,
    pub name: String,
}

/// The schema names: every file kind, plus `command`.
pub fn schema_names() -> Vec<&'static str> {
    let mut v: Vec<&'static str> = FileKind::ALL.iter().map(|k| k.name()).collect();
    v.push("command");
    v.push("fragment");
    v
}

/// The built-in template `name` as pretty JSON text.
pub fn builtin_template_text(name: &str) -> Option<String> {
    let t = worlds_core::Macros::builtin_templates().get(name)?;
    serde_json::to_string_pretty(t).ok().map(|s| s + "\n")
}

/// The schema `name` as pretty JSON text.
pub fn schema_text(name: &str) -> Option<String> {
    let v = if name == "command" {
        serde_json::to_value(schemars::schema_for!(worlds_core::Command)).ok()?
    } else if name == "fragment" {
        worlds_core::fragment::schema()
    } else {
        let kind = FileKind::ALL.into_iter().find(|k| k.name() == name)?;
        worlds_core::schema::for_kind(kind)
    };
    serde_json::to_string_pretty(&v).ok().map(|s| s + "\n")
}

fn defs_file_ok(f: &str) -> bool {
    match f {
        "layers.json" | "tilesets.json" | "enums.json" => true,
        _ => {
            let parts: Vec<&str> = f.split('/').collect();
            parts.len() == 2
                && (parts[0] == "entities" || parts[0] == "rules")
                && parts[1].ends_with(".json")
                && parts[1].len() > 5
        }
    }
}

impl Engine {
    /// Every concrete resource of the open project.
    pub fn list_resources(&mut self) -> Result<Vec<Listed>, String> {
        self.prepare()?;
        let d = self.read("project.describe", json!({}))?;
        let mut out = Vec::new();
        let mut add = |uri: String| {
            let name = uri.trim_start_matches("worlds://").to_string();
            out.push(Listed { uri, name });
        };
        add("worlds://project.json".into());
        for f in ["layers.json", "tilesets.json", "enums.json"] {
            add(format!("worlds://defs/{f}"));
        }
        for e in d["entity_types"].as_array().into_iter().flatten() {
            add(format!(
                "worlds://defs/entities/{}.json",
                e["slug"].as_str().unwrap_or("")
            ));
        }
        let layers = d["layers"].as_array().cloned().unwrap_or_default();
        for l in &layers {
            if l["kind"] == "auto" {
                add(format!(
                    "worlds://defs/rules/{}.json",
                    l["slug"].as_str().unwrap_or("")
                ));
            }
        }
        if self
            .rpc("project.read_file", json!({ "path": "rules/macros.json" }))
            .is_ok()
        {
            add("worlds://defs/rules/macros.json".into());
        }
        for w in d["worlds"].as_array().into_iter().flatten() {
            add(format!(
                "worlds://worlds/{}/world.json",
                w["slug"].as_str().unwrap_or("")
            ));
        }
        for l in d["levels"].as_array().into_iter().flatten() {
            let slug = l["slug"].as_str().unwrap_or("");
            add(format!("worlds://levels/{slug}/level.json"));
            let content = l["content"].as_object().cloned().unwrap_or_default();
            let mut has_entities = false;
            for (layer, c) in &content {
                if c.get("entities").is_some() {
                    has_entities = true;
                } else {
                    add(format!("worlds://levels/{slug}/{layer}.json"));
                }
            }
            if has_entities {
                add(format!("worlds://levels/{slug}/entities.json"));
            }
            let notes = &l["notes"];
            if notes["open"].as_u64().unwrap_or(0) + notes["done"].as_u64().unwrap_or(0) > 0 {
                add(format!("worlds://levels/{slug}/notes.json"));
            }
        }
        for n in worlds_core::Macros::builtin_templates().keys() {
            add(format!("worlds://templates/{n}.json"));
        }
        for n in schema_names() {
            add(format!("worlds://schema/{n}.json"));
        }
        Ok(out)
    }

    /// The text of a resource.
    pub fn read_resource(&mut self, uri: &str) -> Result<String, String> {
        let unknown = || {
            format!(
                "unknown resource {uri:?}: resources are worlds://project.json, worlds://defs/<file>, worlds://worlds/<world>/world.json, worlds://levels/<slug>/<file>, worlds://templates/<name>.json and worlds://schema/<name>.json"
            )
        };
        let rest = uri.strip_prefix("worlds://").ok_or_else(unknown)?;
        if let Some(name) = rest
            .strip_prefix("schema/")
            .and_then(|s| s.strip_suffix(".json"))
        {
            return schema_text(name).ok_or_else(|| {
                format!(
                    "no schema {name:?}; the schemas are {}",
                    schema_names().join(", ")
                )
            });
        }
        if let Some(name) = rest
            .strip_prefix("templates/")
            .and_then(|s| s.strip_suffix(".json"))
        {
            return builtin_template_text(name).ok_or_else(|| {
                format!(
                    "no built-in template {name:?}; the built-in templates are {}",
                    worlds_core::Macros::builtin_templates()
                        .keys()
                        .map(|k| k.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            });
        }
        self.prepare()?;
        let path = if rest == "project.json" {
            rest.to_string()
        } else if let Some(f) = rest.strip_prefix("defs/") {
            if !defs_file_ok(f) {
                return Err(unknown());
            }
            f.to_string()
        } else if let Some(wf) = rest.strip_prefix("worlds/") {
            let world = wf.strip_suffix("/world.json").ok_or_else(unknown)?;
            if world.is_empty() || world.contains('/') {
                return Err(unknown());
            }
            let d = self.read("project.describe", json!({}))?;
            if !d["worlds"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|w| w["slug"] == world)
            {
                return Err(format!("there is no world {world:?}"));
            }
            format!("worlds/{world}/world.json")
        } else if let Some(lf) = rest.strip_prefix("levels/") {
            let (slug, file) = lf.split_once('/').ok_or_else(unknown)?;
            if file.contains('/') || !file.ends_with(".json") {
                return Err(unknown());
            }
            let list = self.read("level.list", json!({}))?;
            let world = list["levels"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|l| l["slug"] == slug)
                .and_then(|l| l["world"].as_str().map(str::to_string))
                .ok_or_else(|| format!("there is no level {slug:?}"))?;
            format!("worlds/{world}/levels/{slug}/{file}")
        } else {
            return Err(unknown());
        };
        let r = self.read("project.read_file", json!({ "path": path }))?;
        Ok(r["text"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| Value::to_string(&r["text"])))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schemas_and_defs_paths() {
        assert_eq!(schema_names().len(), 15);
        for n in schema_names() {
            let t = schema_text(n).unwrap();
            assert!(serde_json::from_str::<Value>(&t).is_ok(), "{n}");
        }
        assert!(schema_text("nope").is_none());
        let blob: Value = serde_json::from_str(&builtin_template_text("blob47").unwrap()).unwrap();
        assert!(blob["rules"].as_array().unwrap().len() >= 47);
        assert!(builtin_template_text("nope").is_none());
        assert!(defs_file_ok("entities/door.json"));
        assert!(defs_file_ok("rules/macros.json"));
        assert!(!defs_file_ok("project.json"));
        assert!(!defs_file_ok("rules/../x.json"));
        assert!(!defs_file_ok("worlds/main/world.json"));
    }
}
