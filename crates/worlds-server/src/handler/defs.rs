//! `defs.*`: definitions by `kind` and `slug`, dispatched to the per-kind
//! definition commands (`docs/commands.md`, "Definition commands").

use serde::Deserialize;
use serde_json::{Map, Value, json};
use worlds_core::{
    Command, CommandError, EntityDef, EnumDef, EnumSource, FieldDef, FieldType, LayerDef, Project,
    Slug, TilesetDef,
};

use super::params;
use super::{Confirm, Write};
use crate::rpc::RpcError;

/// The `rules.*` method that edits a definition of `kind` (stamps and
/// macros are edited there, not with `defs.*`), for `defs.*` with `method`.
fn rules_method(method: &str, kind: &str) -> Option<&'static str> {
    Some(match (kind, method) {
        ("stamp", "defs.set") => "rules.set_stamp { layer, stamp, def, index? }",
        ("stamp", "defs.remove") => "rules.remove_stamp { layer, stamp }",
        ("stamp", "defs.rename") => {
            "rules.set_stamp with the new slug, then rules.remove_stamp of the old one (stamps have no rename)"
        }
        ("macros", "defs.set") => "rules.set_macro { kind, name, def }",
        ("macros", "defs.remove") => "rules.remove_macro { kind, name }",
        ("macros", "defs.rename") => "rules.rename_macro { kind, from, to }",
        _ => return None,
    })
}

fn bad_kind(method: &str, kind: &str, allowed: &str) -> RpcError {
    if let Some(m) = rules_method(method, kind) {
        return RpcError::invalid_params(format!(
            "{method}: kind {kind} isn't edited with defs.*; use {m}"
        ));
    }
    RpcError::invalid_params(format!("{method}: kind {kind:?} isn't one of {allowed}"))
}

/// The field kinds: `entity_field` (the `fields` of the entity type named
/// by `entity`) and `level_field` (`project.json` `level_fields`).
#[derive(Clone, Copy)]
enum FieldOwner<'a> {
    Entity(&'a Slug),
    Level,
}

impl<'a> FieldOwner<'a> {
    /// The owner a field-kind request names: `entity` is required with
    /// `entity_field` and refused with any other kind. `None` for a kind that
    /// isn't a field kind.
    fn of(
        method: &str,
        kind: &str,
        entity: Option<&'a Slug>,
    ) -> Result<Option<FieldOwner<'a>>, RpcError> {
        match (kind, entity) {
            ("entity_field", Some(e)) => Ok(Some(FieldOwner::Entity(e))),
            ("entity_field", None) => Err(RpcError::invalid_params(format!(
                "{method}: kind entity_field needs entity (the entity type's slug)"
            ))),
            ("level_field", None) => Ok(Some(FieldOwner::Level)),
            (_, Some(_)) => Err(RpcError::invalid_params(format!(
                "{method}: entity goes with kind entity_field"
            ))),
            (_, None) => Ok(None),
        }
    }

    /// As the field commands' errors name it (`entity type "door"`,
    /// `project level_fields`).
    fn describe(self) -> String {
        match self {
            FieldOwner::Entity(e) => format!("entity type {e:?}"),
            FieldOwner::Level => "project level_fields".into(),
        }
    }

    /// The field list (an unknown entity type is `unknown_entity_type`).
    fn fields(self, project: &Project) -> Result<&[FieldDef], RpcError> {
        match self {
            FieldOwner::Entity(e) => project
                .entity_def(e.as_str())
                .map(|d| d.fields.as_slice())
                .ok_or_else(|| RpcError::command(&CommandError::UnknownEntityType(e.clone()))),
            FieldOwner::Level => Ok(&project.level_fields),
        }
    }

    /// How many instances of the type (or levels) store a value under
    /// `field`.
    fn holders(self, project: &Project, field: &Slug) -> usize {
        match self {
            FieldOwner::Entity(ty) => project.entity_field_holders(ty, field),
            FieldOwner::Level => project.level_field_holders(field),
        }
    }

    /// "1 entity stores" / "2 levels store".
    fn store(self, n: usize) -> String {
        match self {
            FieldOwner::Entity(_) => entities(n),
            FieldOwner::Level if n == 1 => "1 level stores".into(),
            FieldOwner::Level => format!("{n} levels store"),
        }
    }

    /// "field channel of floor_button" / "level field music".
    fn field_name(self, field: &Slug) -> String {
        match self {
            FieldOwner::Entity(e) => format!("field {field} of {e}"),
            FieldOwner::Level => format!("level field {field}"),
        }
    }
}

/// `defs.get` of a field kind: the list, or one field with `slug`.
fn get_fields(
    project: &Project,
    kind: &str,
    owner: FieldOwner,
    slug: Option<Slug>,
) -> Result<Value, RpcError> {
    let fields = owner.fields(project)?;
    let Some(slug) = slug else {
        let list: Vec<Value> = fields.iter().map(file_form).collect();
        return Ok(json!({ "defs": { "fields": list } }));
    };
    let def = fields.iter().find(|f| f.slug == slug).ok_or_else(|| {
        RpcError::command(&CommandError::UnknownField {
            owner: owner.describe(),
            field: slug.clone(),
        })
    })?;
    let mut out = json!({ "kind": kind });
    if let FieldOwner::Entity(e) = owner {
        out["entity"] = json!(e);
    }
    out["def"] = file_form(def);
    Ok(out)
}

/// `defs.set` of a field needs confirm when it replaces a field with one of
/// another type (see [`same_type`]) while instances (or levels) store a
/// value for it.
fn field_confirm(project: &Project, owner: FieldOwner, new: &FieldDef) -> Option<Confirm> {
    let old = owner
        .fields(project)
        .ok()?
        .iter()
        .find(|f| f.slug == new.slug)?;
    if same_type(old, new) {
        return None;
    }
    let n = owner.holders(project, &new.slug);
    if n == 0 {
        return None;
    }
    let what = match owner {
        FieldOwner::Entity(_) => "field",
        FieldOwner::Level => "level field",
    };
    let reason = format!(
        "{} a value for {what} {}, whose type changes",
        owner.store(n),
        new.slug
    );
    Some(Confirm::new(
        reason.clone(),
        format!(
            "{reason}; the values stay stored and validation will report the ones that don't fit. Send confirm: true to do it"
        ),
    ))
}

/// A definition in file form (no `$schema`, no `format`).
fn file_form<T: serde::Serialize>(v: &T) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

/// One kind's definitions: kind, plural key, (slug, file form) list.
type DefList<'p> = (&'static str, &'static str, Vec<(&'p Slug, Value)>);

/// `defs.get { kind?, slug? }`.
pub(crate) fn get(project: &Project, method: &str, p: Value) -> Result<Value, RpcError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        kind: Option<String>,
        entity: Option<Slug>,
        slug: Option<Slug>,
    }
    let p: P = params::parse(method, p)?;
    if let Some(kind) = p.kind.as_deref() {
        if let Some(owner) = FieldOwner::of(method, kind, p.entity.as_ref())? {
            return get_fields(project, kind, owner, p.slug);
        }
    } else if p.entity.is_some() {
        return Err(RpcError::invalid_params(format!(
            "{method}: entity goes with kind entity_field"
        )));
    }
    // `rules/macros.json` is one file, not a list: `defs.macros` is its
    // file form (`{}` when there is none), slug `macros`.
    if p.kind.as_deref() == Some("macros") {
        let file = super::rules::macros_file(project);
        return match p.slug {
            None => Ok(json!({ "defs": { "macros": file } })),
            Some(s) if s == "macros" => Ok(json!({ "kind": "macros", "def": file })),
            Some(s) => Err(RpcError::invalid_params(format!(
                "{method}: kind macros has the one slug macros (rules/macros.json), not {s}"
            ))),
        };
    }
    let lists: [DefList; 5] = [
        (
            "layer",
            "layers",
            project
                .layers
                .iter()
                .map(|d| (&d.slug, file_form(d)))
                .collect(),
        ),
        (
            "entity",
            "entities",
            project
                .entities
                .iter()
                .map(|d| (&d.slug, file_form(d)))
                .collect(),
        ),
        (
            "enum",
            "enums",
            project
                .enums
                .iter()
                .map(|d| (&d.slug, file_form(d)))
                .collect(),
        ),
        (
            "tileset",
            "tilesets",
            project
                .tilesets
                .iter()
                .map(|d| (&d.slug, file_form(d)))
                .collect(),
        ),
        (
            "rules",
            "rules",
            project
                .rules
                .iter()
                .map(|d| (&d.layer, file_form(d)))
                .collect(),
        ),
    ];
    match (p.kind, p.slug) {
        (None, Some(_)) => Err(RpcError::invalid_params(format!(
            "{method}: slug needs kind"
        ))),
        (None, None) => {
            let mut defs = Map::new();
            for (_, plural, list) in lists {
                defs.insert(plural.into(), list.into_iter().map(|(_, v)| v).collect());
            }
            defs.insert("macros".into(), super::rules::macros_file(project));
            Ok(json!({ "defs": defs }))
        }
        (Some(kind), slug) => {
            let Some((_, plural, list)) = lists.into_iter().find(|(k, _, _)| *k == kind) else {
                return Err(bad_kind(
                    method,
                    &kind,
                    "layer, entity, entity_field, level_field, enum, tileset, rules, macros",
                ));
            };
            match slug {
                None => {
                    let mut defs = Map::new();
                    defs.insert(plural.into(), list.into_iter().map(|(_, v)| v).collect());
                    Ok(json!({ "defs": defs }))
                }
                Some(slug) => {
                    let def = list
                        .into_iter()
                        .find(|(s, _)| **s == slug)
                        .map(|(_, v)| v)
                        .ok_or_else(|| unknown(&kind, &slug))?;
                    Ok(json!({ "kind": kind, "def": def }))
                }
            }
        }
    }
}

/// The command error for a missing definition.
fn unknown(kind: &str, slug: &Slug) -> RpcError {
    RpcError::command(&match kind {
        "layer" => CommandError::UnknownLayer(slug.clone()),
        "entity" => CommandError::UnknownEntityType(slug.clone()),
        "enum" => CommandError::UnknownEnum(slug.clone()),
        "tileset" => CommandError::UnknownTileset(slug.clone()),
        _ => CommandError::Invalid(format!("no rule set {slug}")),
    })
}

/// `defs.set { kind, def, index?, confirm? }`.
pub(crate) fn set(project: &Project, method: &str, p: Value) -> Result<Write, RpcError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        kind: String,
        entity: Option<Slug>,
        def: Value,
        index: Option<usize>,
    }
    let p: P = params::parse(method, p)?;
    if let Some(owner) = FieldOwner::of(method, &p.kind, p.entity.as_ref())? {
        let def: FieldDef = params::parse(method, p.def)?;
        let confirm = field_confirm(project, owner, &def);
        let field = def.slug.clone();
        let cmd = match owner {
            FieldOwner::Entity(e) => Command::SetEntityFieldDef {
                entity: e.clone(),
                field,
                def,
                index: p.index,
            },
            FieldOwner::Level => Command::SetLevelFieldDef {
                field,
                def,
                index: p.index,
            },
        };
        return Ok(Write::Commands {
            cmds: vec![cmd],
            id: None,
            confirm,
        });
    }
    let (cmd, confirm) = match p.kind.as_str() {
        "layer" => {
            let def: LayerDef = params::parse(method, p.def)?;
            (
                Command::SetLayerDef {
                    layer: def.slug.clone(),
                    def,
                    index: p.index,
                    content: Vec::new(),
                    rules: None,
                },
                None,
            )
        }
        "entity" => {
            if p.index.is_some() {
                return Err(RpcError::invalid_params(format!(
                    "{method}: entity types have no order, so kind entity takes no index"
                )));
            }
            let def: EntityDef = params::parse(method, p.def)?;
            let confirm = entity_confirm(project, &def);
            (
                Command::SetEntityDef {
                    entity: def.slug.clone(),
                    def,
                },
                confirm,
            )
        }
        "enum" => {
            let def: EnumDef = params::parse(method, p.def)?;
            let confirm = enum_confirm(project, &def);
            (
                Command::SetEnumDef {
                    enum_: def.slug.clone(),
                    def,
                    index: p.index,
                },
                confirm,
            )
        }
        "tileset" => {
            let def: TilesetDef = params::parse(method, p.def)?;
            (
                Command::SetTilesetDef {
                    tileset: def.slug.clone(),
                    def,
                    index: p.index,
                },
                None,
            )
        }
        k => {
            return Err(bad_kind(
                method,
                k,
                "layer, entity, entity_field, level_field, enum, tileset",
            ));
        }
    };
    Ok(Write::Commands {
        cmds: vec![cmd],
        id: None,
        confirm,
    })
}

/// `defs.rename { kind, from, to }` or `{ kind: "enum_value", enum, from, to }`.
pub(crate) fn rename(_: &Project, method: &str, p: Value) -> Result<Write, RpcError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        kind: String,
        #[serde(rename = "enum")]
        enum_: Option<Slug>,
        entity: Option<Slug>,
        from: Slug,
        to: Slug,
    }
    let p: P = params::parse(method, p)?;
    if p.enum_.is_some() && p.kind != "enum_value" {
        return Err(RpcError::invalid_params(format!(
            "{method}: enum goes with kind enum_value"
        )));
    }
    let (from, to) = (p.from, p.to);
    match FieldOwner::of(method, &p.kind, p.entity.as_ref())? {
        Some(FieldOwner::Entity(e)) => {
            return Ok(Write::one(Command::RenameEntityField {
                entity: e.clone(),
                from,
                to,
            }));
        }
        Some(FieldOwner::Level) => return Ok(Write::one(Command::RenameLevelField { from, to })),
        None => {}
    }
    let cmd = match p.kind.as_str() {
        "layer" => Command::RenameLayer { from, to },
        "entity" => Command::RenameEntityType { from, to },
        "enum" => Command::RenameEnum { from, to },
        "tileset" => Command::RenameTileset { from, to },
        "enum_value" => Command::RenameEnumValue {
            enum_: p.enum_.ok_or_else(|| {
                RpcError::invalid_params(format!("{method}: kind enum_value needs enum"))
            })?,
            from,
            to,
        },
        k => {
            return Err(bad_kind(
                method,
                k,
                "layer, entity, entity_field, level_field, enum, enum_value, tileset",
            ));
        }
    };
    Ok(Write::one(cmd))
}

/// `defs.remove { kind, entity?, slug, confirm: true }`.
pub(crate) fn remove(project: &Project, method: &str, p: Value) -> Result<Write, RpcError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        kind: String,
        entity: Option<Slug>,
        slug: Slug,
    }
    let p: P = params::parse(method, p)?;
    let s = p.slug;
    if let Some(owner) = FieldOwner::of(method, &p.kind, p.entity.as_ref())? {
        let name = owner.field_name(&s);
        // The command takes the stored values with it (undo puts them back).
        let reason = match owner.holders(project, &s) {
            0 => format!("removes {name}"),
            n => {
                let (the, them) = if n == 1 {
                    ("the value", "it")
                } else {
                    ("the values", "them")
                };
                format!(
                    "removes {name} and {the} {}; undo brings {them} back",
                    owner.store(n)
                )
            }
        };
        let cmd = match owner {
            FieldOwner::Entity(e) => Command::RemoveEntityFieldDef {
                entity: e.clone(),
                field: s,
                keep_values: false,
            },
            FieldOwner::Level => Command::RemoveLevelFieldDef {
                field: s,
                keep_values: false,
            },
        };
        return Ok(Write::Commands {
            cmds: vec![cmd],
            id: None,
            confirm: Some(Confirm::new(
                reason,
                format!("removing {name} needs confirm: true"),
            )),
        });
    }
    let (cmd, what) = match p.kind.as_str() {
        "layer" => (Command::RemoveLayerDef { layer: s.clone() }, "layer"),
        "entity" => (
            Command::RemoveEntityDef {
                entity: s.clone(),
                keep_instances: false,
            },
            "entity type",
        ),
        "enum" => (Command::RemoveEnumDef { enum_: s.clone() }, "enum"),
        "tileset" => (
            Command::RemoveTilesetDef {
                tileset: s.clone(),
                keep_uses: false,
            },
            "tileset",
        ),
        k => {
            return Err(bad_kind(
                method,
                k,
                "layer, entity, entity_field, level_field, enum, tileset",
            ));
        }
    };
    Ok(Write::Commands {
        cmds: vec![cmd],
        id: None,
        confirm: Some(Confirm::new(
            format!("removes {what} {s}"),
            format!("removing {what} {s} needs confirm: true"),
        )),
    })
}

/// "N entities" / "1 entity".
fn entities(n: usize) -> String {
    if n == 1 {
        "1 entity stores".into()
    } else {
        format!("{n} entities store")
    }
}

/// Whether two field definitions store the same kind of value: the same
/// type name and array flag, and for enums the same source (the same project
/// enum, or inline values on both sides: a project enum and an inline list,
/// or two project enums, hold different value sets).
fn same_type(a: &FieldDef, b: &FieldDef) -> bool {
    let same_source = match (&a.ty, &b.ty) {
        (FieldType::Enum(x), FieldType::Enum(y)) => match (x, y) {
            (EnumSource::Project(p), EnumSource::Project(q)) => p == q,
            (EnumSource::Inline(_), EnumSource::Inline(_)) => true,
            _ => false,
        },
        _ => true,
    };
    a.array == b.array && a.ty.name() == b.ty.name() && same_source
}

/// `defs.set` of an entity type needs confirm when instances store a value
/// for a field the new definition takes out or changes the type of
/// ([`same_type`]).
fn entity_confirm(project: &Project, new: &EntityDef) -> Option<Confirm> {
    let old = project.entity_def(new.slug.as_str())?;
    let gone: Vec<&Slug> = old
        .fields
        .iter()
        .filter(|f| new.field(f.slug.as_str()).is_none_or(|n| !same_type(f, n)))
        .map(|f| &f.slug)
        .collect();
    if gone.is_empty() {
        return None;
    }
    let mut hit: Vec<&Slug> = Vec::new();
    let mut n = 0;
    for e in project.levels().flat_map(|l| l.all_entities()) {
        if e.ty != new.slug {
            continue;
        }
        let stored: Vec<&Slug> = gone
            .iter()
            .copied()
            .filter(|f| e.fields.contains_key(*f))
            .collect();
        if !stored.is_empty() {
            n += 1;
            for f in stored {
                if !hit.contains(&f) {
                    hit.push(f);
                }
            }
        }
    }
    if n == 0 {
        return None;
    }
    let names: Vec<&str> = hit.iter().map(|s| s.as_str()).collect();
    let reason = format!(
        "{} a value for a field that no longer exists or becomes invalid: {}",
        entities(n),
        names.join(", ")
    );
    Some(Confirm::new(
        reason.clone(),
        format!(
            "{reason}; the values stay stored and validation will report them. Send confirm: true to do it"
        ),
    ))
}

/// `defs.set` of an enum needs confirm when a value it takes out is stored
/// somewhere: an entity field, a level field or a field default.
fn enum_confirm(project: &Project, new: &EnumDef) -> Option<Confirm> {
    let old = project.enum_def(new.slug.as_str())?;
    let gone: Vec<&Slug> = old
        .values
        .iter()
        .map(|v| &v.slug)
        .filter(|v| !new.has_value(v.as_str()))
        .collect();
    if gone.is_empty() {
        return None;
    }
    let is_ours =
        |f: &FieldDef| matches!(&f.ty, FieldType::Enum(EnumSource::Project(e)) if *e == new.slug);
    let holds = |v: &Value| -> Vec<String> {
        let one = |x: &Value| {
            x.as_str()
                .filter(|s| gone.iter().any(|g| g.as_str() == *s))
                .map(str::to_string)
        };
        match v {
            Value::Array(a) => a.iter().filter_map(one).collect(),
            x => one(x).into_iter().collect(),
        }
    };
    let mut uses: Vec<String> = Vec::new();
    let mut n_entities = 0;
    let mut n_other = 0;
    for def in &project.entities {
        for f in def.fields.iter().filter(|f| is_ours(f)) {
            for v in f.default.iter().flat_map(&holds) {
                n_other += 1;
                uses.push(format!("the default of {}.{}: {v}", def.slug, f.slug));
            }
        }
    }
    for f in project.level_fields.iter().filter(|f| is_ours(f)) {
        for v in f.default.iter().flat_map(&holds) {
            n_other += 1;
            uses.push(format!("the default of level field {}: {v}", f.slug));
        }
    }
    for l in project.levels() {
        for f in project.level_fields.iter().filter(|f| is_ours(f)) {
            for v in l.fields.get(&f.slug).iter().flat_map(|x| holds(x)) {
                n_other += 1;
                uses.push(format!("level {} {}: {v}", l.slug, f.slug));
            }
        }
        for e in l.all_entities() {
            let Some(def) = project.entity_def(e.ty.as_str()) else {
                continue;
            };
            let mut counted = false;
            for f in def.fields.iter().filter(|f| is_ours(f)) {
                for v in e.fields.get(&f.slug).iter().flat_map(|x| holds(x)) {
                    if !counted {
                        n_entities += 1;
                        counted = true;
                    }
                    uses.push(format!("{} {}: {v}", e.id, f.slug));
                }
            }
        }
    }
    if uses.is_empty() {
        return None;
    }
    let who = match (n_entities, n_other) {
        (n, 0) => entities(n),
        (0, 1) => "1 place stores".to_string(),
        (e, o) => format!("{} places store", e + o),
    };
    let shown: Vec<&str> = uses.iter().take(5).map(String::as_str).collect();
    let more = if uses.len() > 5 {
        format!(", and {} more", uses.len() - 5)
    } else {
        String::new()
    };
    let reason = format!(
        "{who} a value this takes out of enum {} ({}{more})",
        new.slug,
        shown.join(", ")
    );
    Some(Confirm::new(
        reason.clone(),
        format!(
            "{reason}; the values stay stored and validation will report them. Send confirm: true to do it"
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(v: Value) -> FieldDef {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn retype_covers_type_array_and_enum_source() {
        let f = |v| field(v);
        let int = f(json!({ "slug": "a", "type": "int" }));
        // Bounds, defaults and display aren't the type.
        assert!(same_type(
            &int,
            &f(json!({ "slug": "a", "type": "int", "min": 0, "default": 3 }))
        ));
        assert!(!same_type(
            &int,
            &f(json!({ "slug": "a", "type": "float" }))
        ));
        assert!(!same_type(
            &int,
            &f(json!({ "slug": "a", "type": "int", "array": true }))
        ));
        let facing = f(json!({ "slug": "a", "type": "enum", "enum": "facing" }));
        let inline = f(json!({ "slug": "a", "type": "enum", "values": ["l", "r"] }));
        assert!(same_type(&facing, &facing.clone()));
        assert!(!same_type(
            &facing,
            &f(json!({ "slug": "a", "type": "enum", "enum": "biome" }))
        ));
        assert!(!same_type(&facing, &inline));
        assert!(!same_type(&inline, &facing));
        assert!(same_type(
            &inline,
            &f(json!({ "slug": "a", "type": "enum", "values": ["l", "r", "u"] }))
        ));
    }
}
