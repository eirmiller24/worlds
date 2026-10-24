//! JSON Schemas for every file kind, generated from the Rust types.

use std::fs;
use std::path::Path;

use schemars::{JsonSchema, schema_for};
use serde_json::{Value, json};

use crate::format::{
    EntitiesFile, EnumsFile, Error, FORMAT_VERSION, FileKind, IntGridFile, LayersFile, NotesFile,
    Result, TilesetsFile,
};
use crate::model::{EntityDef, Level, Macros, Project, RuleSet, TilesLayer, World};

/// The schema for one file kind, including the `$schema` and `format` header.
pub fn for_kind(kind: FileKind) -> Value {
    match kind {
        FileKind::Project => file_schema::<Project>(kind),
        FileKind::Layers => file_schema::<LayersFile>(kind),
        FileKind::Tilesets => file_schema::<TilesetsFile>(kind),
        FileKind::Enums => file_schema::<EnumsFile>(kind),
        FileKind::Entity => file_schema::<EntityDef>(kind),
        FileKind::Rules => file_schema::<RuleSet>(kind),
        FileKind::Macros => file_schema::<Macros>(kind),
        FileKind::World => file_schema::<World>(kind),
        FileKind::Level => file_schema::<Level>(kind),
        FileKind::IntGrid => file_schema::<IntGridFile>(kind),
        FileKind::Tiles => file_schema::<TilesLayer>(kind),
        FileKind::Entities => file_schema::<EntitiesFile>(kind),
        FileKind::Notes => file_schema::<NotesFile>(kind),
        FileKind::Assembly => file_schema::<crate::assembly::Assembly>(kind),
    }
}

/// Write `schema/<kind>.json` for every file kind under the project directory
/// `project_dir`. Files whose content is unchanged are not rewritten.
pub fn write_all(project_dir: impl AsRef<Path>) -> Result<()> {
    let dir = project_dir.as_ref().join("schema");
    fs::create_dir_all(&dir).map_err(|source| Error::Io {
        path: dir.clone(),
        source,
    })?;
    for kind in FileKind::ALL {
        let path = dir.join(format!("{}.json", kind.name()));
        let mut text = serde_json::to_string_pretty(&for_kind(kind)).expect("schema serializes");
        text.push('\n');
        if fs::read_to_string(&path).is_ok_and(|old| old == text) {
            continue;
        }
        fs::write(&path, text).map_err(|source| Error::Io { path, source })?;
    }
    Ok(())
}

fn file_schema<T: JsonSchema>(kind: FileKind) -> Value {
    let mut v = schema_for!(T).to_value();
    let obj = v.as_object_mut().expect("object schema");
    obj.insert(
        "title".into(),
        json!(format!("Worlds {} file", kind.name())),
    );
    let props = obj
        .entry("properties")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .expect("properties object");
    props.insert(
        "$schema".into(),
        json!({ "type": "string", "description": format!("Relative path to schema/{}.json.", kind.name()) }),
    );
    props.insert(
        "format".into(),
        json!({ "const": FORMAT_VERSION, "description": "File format version." }),
    );
    let required = obj
        .entry("required")
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .expect("required array");
    required.insert(0, json!("format"));
    required.insert(0, json!("$schema"));
    v
}
