//! Project-wide find: one small query language for entities, levels,
//! annotations and tile placements, shared by the editor's Find pane, the
//! live API (`entity.find`, `level.find`, `tiles.find`, `notes.list`) and
//! the MCP `find` tool, so all three agree on what a query means.
//!
//! A query is words separated by spaces, every one of which must hold:
//!
//! ```text
//! type:locked_door channel:3        a type and a field value
//! tag:puzzle                        a tag (the entity's type's, a level's, a note's)
//! field:target = null               a field test, = or != (spaces optional)
//! world:main level:crevasse layer:objects
//! kind:pin status:open              annotations only
//! tileset:castle_main tile:4,4      tile placements only
//! brass "key room"                  free text: name, slug or id (notes: title, body, label)
//! ```
//!
//! A bare `key:value` whose key is not one of the reserved words above is a
//! field test (`channel:3` is `field:channel = 3`). Values are read as JSON
//! when they parse (`3`, `true`, `null`, `[1,2]`: no spaces inside, since
//! a space ends the word), else as a string; a quoted value (`"3"`,
//! `"brass key"`) is always a string. Slug values (`type`, `tag`,
//! `world`, `level`, `layer`, `tileset`) may use `*` as a wildcard:
//! `type:*door` matches `locked_door`. Free text matches case-insensitively
//! as a substring.
//!
//! Each kind takes only some words ([`Query::check`]): `type` is for
//! entities, `kind` and `status` for annotations, `tileset` and `tile` for
//! tiles, fields for entities and levels, free text for everything but
//! tiles. [`Query::applies`] says whether a query can match a kind at all.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::{
    Annotation, Command, EntityInstance, FieldDef, FieldType, Id, Level, NoteStatus, Project,
    Shape, Slug, TilePlacement, World,
};

/// What a query finds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FindKind {
    Entities,
    Levels,
    Notes,
    Tiles,
}

impl FindKind {
    pub const ALL: [FindKind; 4] = [
        FindKind::Entities,
        FindKind::Levels,
        FindKind::Notes,
        FindKind::Tiles,
    ];

    /// `entities`, `levels`, `notes`, `tiles`.
    pub fn name(self) -> &'static str {
        match self {
            FindKind::Entities => "entities",
            FindKind::Levels => "levels",
            FindKind::Notes => "notes",
            FindKind::Tiles => "tiles",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        FindKind::ALL.into_iter().find(|k| k.name() == s)
    }
}

/// `=` or `!=`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Eq,
    Ne,
}

/// One field test: the effective value (stored, else the default, else
/// `null`) compared with `value`.
#[derive(Debug, Clone, PartialEq)]
pub struct FieldTest {
    pub field: String,
    pub op: Op,
    pub value: Value,
}

/// A parsed query. Every list is a set of tests that must all hold, so
/// merging two queries ([`Query::and`]) is appending.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Query {
    pub types: Vec<String>,
    pub tags: Vec<String>,
    pub fields: Vec<FieldTest>,
    pub worlds: Vec<String>,
    pub levels: Vec<String>,
    pub layers: Vec<String>,
    /// Annotation kinds (`note`, `pin`, ...).
    pub kinds: Vec<String>,
    pub status: Vec<NoteStatus>,
    pub tilesets: Vec<String>,
    pub tiles: Vec<[u32; 2]>,
    /// Lowercased free-text words.
    pub text: Vec<String>,
}

/// The reserved words (anything else before a `:` is a field).
const KEYS: [&str; 11] = [
    "type", "tag", "field", "world", "level", "layer", "kind", "status", "tileset", "tile", "text",
];

/// The annotation kinds `kind:` takes.
pub const NOTE_KINDS: [&str; 6] = ["note", "pin", "path", "region", "guide", "connector"];

/// One word of the query: its text and whether it was quoted.
#[derive(Debug, Clone, PartialEq)]
struct Token {
    text: String,
    quoted: bool,
}

/// Split on whitespace; `"..."` is one token (`\"` inside it is a quote).
/// A quote may start mid-word: `level:"a b"` is one token `level:a b`,
/// quoted.
fn tokens(s: &str) -> Result<Vec<Token>, String> {
    let mut out = Vec::new();
    let mut chars = s.chars().peekable();
    loop {
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        if chars.peek().is_none() {
            return Ok(out);
        }
        let mut text = String::new();
        let mut quoted = false;
        while let Some(&c) = chars.peek() {
            if c.is_whitespace() {
                break;
            }
            chars.next();
            // A quote opens at the start of a word or a value (after `:`
            // or `=`); elsewhere (inside JSON such as `["a"]`) it is text.
            let opens = text.is_empty() || text.ends_with(':') || text.ends_with('=');
            if c == '"' && opens {
                quoted = true;
                loop {
                    match chars.next() {
                        None => return Err(format!("unclosed quote in {s:?}")),
                        Some('"') => break,
                        Some('\\') if chars.peek() == Some(&'"') => {
                            chars.next();
                            text.push('"');
                        }
                        Some(c) => text.push(c),
                    }
                }
            } else {
                text.push(c);
            }
        }
        out.push(Token { text, quoted });
    }
}

/// A value as a query reads it: JSON when it parses, else the text.
/// Quoted text is always a string.
pub fn parse_value(text: &str, quoted: bool) -> Value {
    if quoted {
        return Value::String(text.to_string());
    }
    serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.to_string()))
}

/// `"4,4"` or `"[4, 4]"` as an atlas `[column, row]`.
fn parse_tile(s: &str) -> Result<[u32; 2], String> {
    let t = s.trim().trim_start_matches('[').trim_end_matches(']');
    let parts: Vec<&str> = t.split(',').map(str::trim).collect();
    match parts.as_slice() {
        [c, r] => match (c.parse(), r.parse()) {
            (Ok(c), Ok(r)) => Ok([c, r]),
            _ => Err(format!("tile:{s}: expected column,row, e.g. tile:4,2")),
        },
        _ => Err(format!("tile:{s}: expected column,row, e.g. tile:4,2")),
    }
}

/// `name = value`, `name=value`, `name != value`: the field test, given the
/// part after `field:` and the tokens that follow (consumed as needed).
fn field_test(
    first: &Token,
    rest: &mut std::iter::Peekable<std::vec::IntoIter<Token>>,
) -> Result<FieldTest, String> {
    // Split `first` (name, maybe with the operator and value glued on).
    let (name, mut op, mut value) = match first.text.find("!=").or(first.text.find('=')) {
        Some(i) => {
            let ne = first.text[i..].starts_with("!=");
            let name = first.text[..i].to_string();
            let v = &first.text[i + if ne { 2 } else { 1 }..];
            let op = if ne { Op::Ne } else { Op::Eq };
            let v = (!v.is_empty()).then(|| parse_value(v, first.quoted));
            (name, Some(op), v)
        }
        None => (first.text.clone(), None, None),
    };
    if name.is_empty() {
        return Err("field: needs a field name, e.g. field:channel = 3".into());
    }
    if op.is_none() {
        let next = rest
            .next()
            .ok_or_else(|| format!("field:{name}: expected = or != and a value"))?;
        let (o, v) = if let Some(v) = next.text.strip_prefix("!=") {
            (Op::Ne, v)
        } else if let Some(v) = next.text.strip_prefix('=') {
            (Op::Eq, v)
        } else {
            return Err(format!(
                "field:{name}: expected = or != and a value, found {:?}",
                next.text
            ));
        };
        op = Some(o);
        if !v.is_empty() {
            value = Some(parse_value(v, next.quoted));
        }
    }
    let value = match value {
        Some(v) => v,
        None => {
            let t = rest
                .next()
                .ok_or_else(|| format!("field:{name}: expected a value after the operator"))?;
            parse_value(&t.text, t.quoted)
        }
    };
    Ok(FieldTest {
        field: name,
        op: op.expect("set above"),
        value,
    })
}

impl Query {
    /// Parse the query language (see the module docs).
    pub fn parse(s: &str) -> Result<Query, String> {
        let mut q = Query::default();
        let mut it = tokens(s)?.into_iter().peekable();
        while let Some(t) = it.next() {
            // `key:value`, a lowercase key (a URL's `https://` is text).
            let Some((key, val)) = t.text.split_once(':').filter(|(k, v)| {
                !k.is_empty()
                    && !v.starts_with('/')
                    && k.chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
            }) else {
                if !t.text.is_empty() {
                    q.text.push(t.text.to_lowercase());
                }
                continue;
            };
            let val = val.to_string();
            let need = |v: &str| -> Result<String, String> {
                if v.is_empty() {
                    Err(format!("{key}: needs a value, e.g. {key}:something"))
                } else {
                    Ok(v.to_string())
                }
            };
            match key {
                "type" => q.types.push(need(&val)?),
                "tag" => q.tags.push(need(&val)?),
                "world" => q.worlds.push(need(&val)?),
                "level" => q.levels.push(need(&val)?),
                "layer" => q.layers.push(need(&val)?),
                "tileset" => q.tilesets.push(need(&val)?),
                "text" => q.text.push(need(&val)?.to_lowercase()),
                "kind" => {
                    if !NOTE_KINDS.contains(&val.as_str()) {
                        return Err(format!(
                            "kind:{val}: the annotation kinds are {}",
                            NOTE_KINDS.join(", ")
                        ));
                    }
                    q.kinds.push(val);
                }
                "status" => q.status.push(match val.as_str() {
                    "open" => NoteStatus::Open,
                    "done" => NoteStatus::Done,
                    _ => return Err(format!("status:{val}: expected open or done")),
                }),
                "tile" => q.tiles.push(parse_tile(&val)?),
                "field" => {
                    let first = Token {
                        text: val,
                        quoted: t.quoted,
                    };
                    q.fields.push(field_test(&first, &mut it)?);
                }
                _ => {
                    debug_assert!(!KEYS.contains(&key));
                    if val.is_empty() && !t.quoted {
                        return Err(format!(
                            "{key}: needs a value (a field test is {key}:value)"
                        ));
                    }
                    q.fields.push(FieldTest {
                        field: key.to_string(),
                        op: Op::Eq,
                        value: parse_value(&val, t.quoted),
                    });
                }
            }
        }
        Ok(q)
    }

    /// Both queries' tests.
    pub fn and(mut self, other: Query) -> Query {
        self.types.extend(other.types);
        self.tags.extend(other.tags);
        self.fields.extend(other.fields);
        self.worlds.extend(other.worlds);
        self.levels.extend(other.levels);
        self.layers.extend(other.layers);
        self.kinds.extend(other.kinds);
        self.status.extend(other.status);
        self.tilesets.extend(other.tilesets);
        self.tiles.extend(other.tiles);
        self.text.extend(other.text);
        self
    }

    /// No tests at all.
    pub fn is_empty(&self) -> bool {
        *self == Query::default()
    }

    /// The words this query uses that `kind` doesn't take, as an error
    /// ("type: is for entities").
    pub fn check(&self, kind: FindKind) -> Result<(), String> {
        use FindKind::*;
        let uses: [(&str, bool, &[FindKind]); 9] = [
            ("tag", !self.tags.is_empty(), &[Entities, Levels, Notes]),
            ("type", !self.types.is_empty(), &[Entities]),
            ("field", !self.fields.is_empty(), &[Entities, Levels]),
            ("layer", !self.layers.is_empty(), &[Entities, Tiles]),
            ("kind", !self.kinds.is_empty(), &[Notes]),
            ("status", !self.status.is_empty(), &[Notes]),
            ("tileset", !self.tilesets.is_empty(), &[Tiles]),
            ("tile", !self.tiles.is_empty(), &[Tiles]),
            ("text", !self.text.is_empty(), &[Entities, Levels, Notes]),
        ];
        for (word, used, kinds) in uses {
            if used && !kinds.contains(&kind) {
                let names: Vec<&str> = kinds.iter().map(|k| k.name()).collect();
                let what = if word == "field" {
                    "field tests are".to_string()
                } else if word == "text" {
                    "free text is".to_string()
                } else {
                    format!("{word}: is")
                };
                return Err(format!(
                    "{what} for {}, not {}",
                    names.join(" and "),
                    kind.name()
                ));
            }
        }
        Ok(())
    }

    /// Whether this query can match `kind` at all ([`Query::check`] holds).
    pub fn applies(&self, kind: FindKind) -> bool {
        self.check(kind).is_ok()
    }

    fn place_ok(&self, world: &World, level: Option<&Level>) -> bool {
        self.worlds.iter().all(|w| glob(w, world.slug.as_str()))
            && self
                .levels
                .iter()
                .all(|l| level.is_some_and(|lv| glob(l, lv.slug.as_str())))
    }

    /// Whether the entity passes every test.
    pub fn entity_matches(
        &self,
        project: &Project,
        world: &World,
        level: &Level,
        layer: &Slug,
        e: &EntityInstance,
    ) -> bool {
        if !self.place_ok(world, Some(level))
            || !self.layers.iter().all(|l| glob(l, layer.as_str()))
            || !self.types.iter().all(|t| glob(t, e.ty.as_str()))
        {
            return false;
        }
        let def = project.entity_def(e.ty.as_str());
        if !self
            .tags
            .iter()
            .all(|t| def.is_some_and(|d| d.tags.iter().any(|x| glob(t, x))))
        {
            return false;
        }
        if !self.text.iter().all(|w| {
            contains(e.id.as_str(), w)
                || contains(e.ty.as_str(), w)
                || def.is_some_and(|d| contains(&d.name, w))
        }) {
            return false;
        }
        self.fields.is_empty() || {
            let eff = entity_fields(project, e);
            self.fields_hold(&eff)
        }
    }

    /// Whether the level passes every test.
    pub fn level_matches(&self, project: &Project, world: &World, level: &Level) -> bool {
        self.place_ok(world, Some(level))
            && self
                .tags
                .iter()
                .all(|t| level.tags.iter().any(|x| glob(t, x)))
            && self.text.iter().all(|w| {
                contains(level.slug.as_str(), w)
                    || contains(&level.name, w)
                    || contains(level.id.as_str(), w)
            })
            && (self.fields.is_empty() || self.fields_hold(&level_fields(project, level)))
    }

    /// Whether the annotation passes every test (`level` is `None` for a
    /// world-scope one, which `level:` never matches).
    pub fn note_matches(&self, world: &World, level: Option<&Level>, n: &Annotation) -> bool {
        self.place_ok(world, level)
            && self.tags.iter().all(|t| n.tags.iter().any(|x| glob(t, x)))
            && self.kinds.iter().all(|k| n.shape.name() == k)
            && self.status.iter().all(|s| n.status == *s)
            && self.text.iter().all(|w| {
                contains(n.id.as_str(), w)
                    || n.title.as_deref().is_some_and(|t| contains(t, w))
                    || n.body.as_deref().is_some_and(|t| contains(t, w))
                    || note_label(n).is_some_and(|t| contains(t, w))
            })
    }

    /// Whether the tile placement passes every test.
    pub fn tile_matches(
        &self,
        world: &World,
        level: &Level,
        layer: &Slug,
        t: &TilePlacement,
    ) -> bool {
        self.place_ok(world, Some(level))
            && self.layers.iter().all(|l| glob(l, layer.as_str()))
            && self.tilesets.iter().all(|s| glob(s, t.tileset.as_str()))
            && self.tiles.iter().all(|x| *x == t.tile)
    }

    fn fields_hold(&self, eff: &BTreeMap<String, Value>) -> bool {
        self.fields.iter().all(|f| {
            let v = eff.get(&f.field).unwrap_or(&Value::Null);
            let eq = values_equal(v, &f.value);
            match f.op {
                Op::Eq => eq,
                Op::Ne => !eq,
            }
        })
    }
}

/// A pin's or connector's label.
fn note_label(n: &Annotation) -> Option<&str> {
    match &n.shape {
        Shape::Pin { label, .. } | Shape::Connector { label, .. } => label.as_deref(),
        _ => None,
    }
}

/// Case-insensitive substring; `needle` is already lowercase.
fn contains(hay: &str, needle: &str) -> bool {
    hay.to_lowercase().contains(needle)
}

/// `pat` against `s`, `*` matching any run of characters (none included).
/// Without a `*`, plain equality.
pub fn glob(pat: &str, s: &str) -> bool {
    if !pat.contains('*') {
        return pat == s;
    }
    let parts: Vec<&str> = pat.split('*').collect();
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if !s.starts_with(first) || s.len() < first.len() + last.len() || !s.ends_with(last) {
        return false;
    }
    let mut rest = &s[first.len()..s.len() - last.len()];
    for mid in &parts[1..parts.len() - 1] {
        match rest.find(mid) {
            Some(i) => rest = &rest[i + mid.len()..],
            None => return false,
        }
    }
    true
}

/// JSON equality, with numbers compared by value (`3` equals `3.0`).
pub fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => match (x.as_f64(), y.as_f64()) {
            (Some(x), Some(y)) => x == y,
            _ => x == y,
        },
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(a, b)| values_equal(a, b))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| y.get(k).is_some_and(|w| values_equal(v, w)))
        }
        _ => a == b,
    }
}

fn effective(defs: &[FieldDef], stored: &BTreeMap<Slug, Value>) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    for f in defs {
        let v = stored
            .get(&f.slug)
            .cloned()
            .or_else(|| f.default.clone())
            .unwrap_or(Value::Null);
        out.insert(f.slug.to_string(), v);
    }
    for (k, v) in stored {
        out.entry(k.to_string()).or_insert_with(|| v.clone());
    }
    out
}

/// Every field of the entity's type, its stored value or default (`null`
/// with neither), plus stored values of fields the type doesn't define.
pub fn entity_fields(project: &Project, e: &EntityInstance) -> BTreeMap<String, Value> {
    let defs = project
        .entity_def(e.ty.as_str())
        .map_or(&[][..], |d| &d.fields[..]);
    effective(defs, &e.fields)
}

/// The same for a level and the project's level fields.
pub fn level_fields(project: &Project, level: &Level) -> BTreeMap<String, Value> {
    effective(&project.level_fields, &level.fields)
}

/// An entity a query found, and where it lives.
#[derive(Debug, Clone, Copy)]
pub struct EntityHit<'p> {
    pub world: &'p World,
    pub level: &'p Level,
    pub layer: &'p Slug,
    /// Its index in the layer's list.
    pub index: usize,
    pub entity: &'p EntityInstance,
}

/// A level a query found.
#[derive(Debug, Clone, Copy)]
pub struct LevelHit<'p> {
    pub world: &'p World,
    pub level: &'p Level,
}

/// An annotation a query found.
#[derive(Debug, Clone, Copy)]
pub struct NoteHit<'p> {
    pub world: &'p World,
    /// `None`: world scope.
    pub level: Option<&'p Level>,
    pub note: &'p Annotation,
}

/// A tile placement a query found.
#[derive(Debug, Clone, Copy)]
pub struct TileHit<'p> {
    pub world: &'p World,
    pub level: &'p Level,
    pub layer: &'p Slug,
    pub tile: &'p TilePlacement,
}

/// Every entity the query matches, world by world, level by level, in file
/// order. Refused when the query uses words entities don't take.
pub fn entities<'p>(project: &'p Project, q: &Query) -> Result<Vec<EntityHit<'p>>, String> {
    q.check(FindKind::Entities)?;
    let mut out = Vec::new();
    for w in &project.worlds {
        for l in &w.levels {
            for el in &l.entities {
                for (index, e) in el.entities.iter().enumerate() {
                    if q.entity_matches(project, w, l, &el.layer, e) {
                        out.push(EntityHit {
                            world: w,
                            level: l,
                            layer: &el.layer,
                            index,
                            entity: e,
                        });
                    }
                }
            }
        }
    }
    Ok(out)
}

/// Every level the query matches, in `level.list` order.
pub fn levels<'p>(project: &'p Project, q: &Query) -> Result<Vec<LevelHit<'p>>, String> {
    q.check(FindKind::Levels)?;
    Ok(project
        .worlds
        .iter()
        .flat_map(|w| {
            w.levels
                .iter()
                .map(move |l| LevelHit { world: w, level: l })
        })
        .filter(|h| q.level_matches(project, h.world, h.level))
        .collect())
}

/// Every annotation the query matches: each world's world-scope ones, then
/// its levels' (`notes.list` order: all worlds' world notes first, then
/// every level).
pub fn notes<'p>(project: &'p Project, q: &Query) -> Result<Vec<NoteHit<'p>>, String> {
    q.check(FindKind::Notes)?;
    let mut out = Vec::new();
    for w in &project.worlds {
        for n in &w.notes {
            if q.note_matches(w, None, n) {
                out.push(NoteHit {
                    world: w,
                    level: None,
                    note: n,
                });
            }
        }
    }
    for w in &project.worlds {
        for l in &w.levels {
            for n in &l.notes {
                if q.note_matches(w, Some(l), n) {
                    out.push(NoteHit {
                        world: w,
                        level: Some(l),
                        note: n,
                    });
                }
            }
        }
    }
    Ok(out)
}

/// Every hand-placed tile the query matches (baked autotiles are not
/// placements), level by level, layer by layer, in file order (row, then
/// column).
pub fn tiles<'p>(project: &'p Project, q: &Query) -> Result<Vec<TileHit<'p>>, String> {
    q.check(FindKind::Tiles)?;
    let mut out = Vec::new();
    for w in &project.worlds {
        for l in &w.levels {
            for tl in &l.tiles {
                for t in &tl.tiles {
                    if q.tile_matches(w, l, &tl.layer, t) {
                        out.push(TileHit {
                            world: w,
                            level: l,
                            layer: &tl.layer,
                            tile: t,
                        });
                    }
                }
            }
        }
    }
    Ok(out)
}

/// A value typed into a Replace box, for field `def`: as a query reads it
/// ([`parse_value`]), except that a text-like field (string, text, color,
/// enum, entity_ref, path) takes unquoted text as a string even when it
/// would parse as JSON (`3` is `"3"`); `null` stays null.
pub fn replace_value(def: &FieldDef, text: &str) -> Value {
    let t = text.trim();
    let textlike = !def.array
        && matches!(
            def.ty,
            FieldType::String { .. }
                | FieldType::Text { .. }
                | FieldType::Color
                | FieldType::Enum(_)
                | FieldType::EntityRef { .. }
                | FieldType::Path { .. }
        );
    if let Some(s) = t
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .filter(|_| t.len() >= 2)
    {
        return Value::String(s.to_string());
    }
    if textlike && t != "null" {
        return Value::String(t.to_string());
    }
    parse_value(t, false)
}

/// "Replace channel in 12 entities": one batch setting `field` to `value`
/// on each of `ids` whose type has that field, and how many it touches.
/// `None` when none of them has it.
pub fn replace_field(
    project: &Project,
    ids: &[Id],
    field: &Slug,
    value: &Value,
) -> Option<(Command, usize)> {
    let mut commands = Vec::new();
    for id in ids {
        let has = project
            .levels()
            .flat_map(|l| l.all_entities())
            .find(|e| e.id == *id)
            .and_then(|e| project.entity_def(e.ty.as_str()))
            .is_some_and(|d| d.field(field.as_str()).is_some());
        if has {
            commands.push(Command::SetEntityField {
                id: id.clone(),
                field: field.clone(),
                value: Some(value.clone()),
                allow_unknown: false,
            });
        }
    }
    let n = commands.len();
    (n > 0).then(|| {
        (
            Command::Batch {
                label: format!(
                    "Replace {field} in {n} {}",
                    if n == 1 { "entity" } else { "entities" }
                ),
                commands,
            },
            n,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ft(field: &str, op: Op, value: Value) -> FieldTest {
        FieldTest {
            field: field.into(),
            op,
            value,
        }
    }

    #[test]
    fn parses_the_language() {
        let q = Query::parse("type:door channel:3 tag:puzzle").unwrap();
        assert_eq!(q.types, ["door"]);
        assert_eq!(q.tags, ["puzzle"]);
        assert_eq!(q.fields, [ft("channel", Op::Eq, json!(3))]);
        for s in [
            "field:target = null",
            "field:target=null",
            "field:target= null",
            "field:target =null",
        ] {
            let q = Query::parse(s).unwrap();
            assert_eq!(q.fields, [ft("target", Op::Eq, Value::Null)], "{s}");
        }
        let q = Query::parse("field:needs_item != brass_key").unwrap();
        assert_eq!(q.fields, [ft("needs_item", Op::Ne, json!("brass_key"))]);
        let q = Query::parse(r#"label:"3" Brass "Key Room""#).unwrap();
        assert_eq!(q.fields, [ft("label", Op::Eq, json!("3"))]);
        assert_eq!(q.text, ["brass", "key room"]);
        let q = Query::parse("kind:pin status:open tileset:castle_* tile:4,2").unwrap();
        assert_eq!(q.kinds, ["pin"]);
        assert_eq!(q.status, [NoteStatus::Open]);
        assert_eq!(q.tiles, [[4, 2]]);
        // A colon in free text that isn't a key: text.
        let q = Query::parse("https://example.com Foo:Bar").unwrap();
        assert_eq!(q.text, ["https://example.com", "foo:bar"]);
        assert!(Query::parse("").unwrap().is_empty());
        for bad in [
            "kind:door",
            "status:maybe",
            "tile:4",
            "field:target",
            "field:target =",
            "field:target maybe",
            "type:",
            "\"open",
            "channel:",
        ] {
            assert!(Query::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn checks_words_per_kind() {
        let q = Query::parse("type:door").unwrap();
        assert!(q.applies(FindKind::Entities));
        assert_eq!(
            q.check(FindKind::Levels).unwrap_err(),
            "type: is for entities, not levels"
        );
        let q = Query::parse("tag:x").unwrap();
        assert!(!q.applies(FindKind::Tiles) && q.applies(FindKind::Notes));
        let q = Query::parse("channel:3").unwrap();
        assert_eq!(
            q.check(FindKind::Notes).unwrap_err(),
            "field tests are for entities and levels, not notes"
        );
        assert!(
            Query::parse("door")
                .unwrap()
                .check(FindKind::Tiles)
                .is_err()
        );
        assert!(
            Query::parse("level:a world:b")
                .unwrap()
                .applies(FindKind::Tiles)
        );
    }

    #[test]
    fn globs_and_values() {
        assert!(glob("*door", "locked_door"));
        assert!(glob("lock*", "locked_door"));
        assert!(glob("l*d*r", "locked_door"));
        assert!(glob("*", ""));
        assert!(!glob("door", "locked_door"));
        assert!(!glob("a*a", "a"));
        assert!(values_equal(&json!(3), &json!(3.0)));
        assert!(values_equal(
            &json!([1, {"a": 2}]),
            &json!([1.0, {"a": 2.0}])
        ));
        assert!(!values_equal(&json!("3"), &json!(3)));
    }
}
