//! [`CommandError`] on the wire.
//!
//! An error serializes as `{ "kind": <variant in snake_case>, ...fields,
//! "message": <Display> }`, so the API can hand it to an agent as is.
//! Tuple variants get a named field (`unknown_level { level }`), the
//! definition kind of `duplicate_def` and `in_use` is `def_kind` (the tag
//! already takes `kind`), the macro kind of `unknown_macro` is `macro_kind`,
//! a stamp in use is an `in_use` with `def_kind: "stamp"` and its `layer`,
//! and `enum_` is `enum`. Nested errors (`batch`,
//! `rollback`) serialize the same way, each with its own `message`.
//! `docs/api.md` ("Errors") lists every kind.

use serde::{Serialize, Serializer};

use super::{CommandError, DefKind, MacroKind};
use crate::id::{Id, Slug};

impl Serialize for CommandError {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        Wire {
            fields: Fields::from(self),
            message: self.to_string(),
        }
        .serialize(s)
    }
}

#[derive(Serialize)]
struct Wire<'a> {
    #[serde(flatten)]
    fields: Fields<'a>,
    message: String,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Fields<'a> {
    UnknownWorld {
        world: &'a Slug,
    },
    UnknownLevel {
        level: &'a Slug,
    },
    LevelNotInWorld {
        world: &'a Slug,
        level: &'a Slug,
    },
    UnknownLayer {
        layer: &'a Slug,
    },
    WrongLayerKind {
        layer: &'a Slug,
        expected: &'static str,
        actual: &'static str,
    },
    UnknownValue {
        layer: &'a Slug,
        value: &'a Slug,
    },
    OutOfBounds {
        layer: &'a Slug,
        x: i64,
        y: i64,
        w: u32,
        h: u32,
    },
    GridSize {
        layer: &'a Slug,
        actual: [u32; 2],
        expected: [u32; 2],
    },
    UndefinedIndex {
        layer: &'a Slug,
        x: u32,
        y: u32,
        index: u16,
    },
    UnknownTileset {
        tileset: &'a Slug,
    },
    UnknownEntityType {
        #[serde(rename = "type")]
        ty: &'a Slug,
    },
    UnknownField {
        owner: &'a str,
        field: &'a Slug,
    },
    DuplicateField {
        owner: &'a str,
        field: &'a Slug,
    },
    UnknownEntity {
        id: &'a Id,
    },
    UnknownNote {
        id: &'a Id,
    },
    UnknownGuideGroup {
        world: &'a Slug,
        group: &'a Slug,
    },
    DuplicateId {
        id: &'a Id,
    },
    DuplicateLevel {
        level: &'a Slug,
    },
    UnknownEnum {
        #[serde(rename = "enum")]
        enum_: &'a Slug,
    },
    UnknownEnumValue {
        #[serde(rename = "enum")]
        enum_: &'a Slug,
        value: &'a Slug,
    },
    UnknownRuleGroup {
        layer: &'a Slug,
        group: &'a Slug,
    },
    UnknownRule {
        layer: &'a Slug,
        rule: &'a Slug,
    },
    UnknownMacro {
        macro_kind: MacroKind,
        name: &'a Slug,
    },
    UnknownStamp {
        layer: &'a Slug,
        stamp: &'a Slug,
    },
    DuplicateDef {
        def_kind: DefKind,
        slug: &'a Slug,
    },
    InUse {
        def_kind: DefKind,
        slug: &'a Slug,
        uses: &'a [String],
    },
    /// A stamp in use: an `in_use` whose `def_kind` is `stamp`, with the layer.
    #[serde(rename = "in_use")]
    StampInUse {
        def_kind: &'static str,
        layer: &'a Slug,
        slug: &'a Slug,
        uses: &'a [String],
    },
    Invalid {},
    Batch {
        label: &'a str,
        index: usize,
        source: &'a CommandError,
    },
    Rollback {
        batch: &'a CommandError,
        failed: &'a [CommandError],
    },
    DuplicateWorld {
        world: &'a Slug,
    },
    UnknownReference {
        id: &'a Id,
    },
}

impl<'a> From<&'a CommandError> for Fields<'a> {
    fn from(e: &'a CommandError) -> Self {
        use CommandError as E;
        match e {
            E::UnknownWorld(world) => Fields::UnknownWorld { world },
            E::UnknownLevel(level) => Fields::UnknownLevel { level },
            E::LevelNotInWorld { world, level } => Fields::LevelNotInWorld { world, level },
            E::UnknownLayer(layer) => Fields::UnknownLayer { layer },
            E::WrongLayerKind {
                layer,
                expected,
                actual,
            } => Fields::WrongLayerKind {
                layer,
                expected,
                actual,
            },
            E::UnknownValue { layer, value } => Fields::UnknownValue { layer, value },
            E::OutOfBounds { layer, x, y, w, h } => Fields::OutOfBounds {
                layer,
                x: *x,
                y: *y,
                w: *w,
                h: *h,
            },
            E::GridSize {
                layer,
                actual,
                expected,
            } => Fields::GridSize {
                layer,
                actual: *actual,
                expected: *expected,
            },
            E::UndefinedIndex { layer, x, y, index } => Fields::UndefinedIndex {
                layer,
                x: *x,
                y: *y,
                index: *index,
            },
            E::UnknownTileset(tileset) => Fields::UnknownTileset { tileset },
            E::UnknownEntityType(ty) => Fields::UnknownEntityType { ty },
            E::UnknownField { owner, field } => Fields::UnknownField { owner, field },
            E::DuplicateField { owner, field } => Fields::DuplicateField { owner, field },
            E::UnknownEntity(id) => Fields::UnknownEntity { id },
            E::UnknownNote(id) => Fields::UnknownNote { id },
            E::UnknownGuideGroup { world, group } => Fields::UnknownGuideGroup { world, group },
            E::DuplicateId(id) => Fields::DuplicateId { id },
            E::DuplicateLevel(level) => Fields::DuplicateLevel { level },
            E::UnknownEnum(enum_) => Fields::UnknownEnum { enum_ },
            E::UnknownEnumValue { enum_, value } => Fields::UnknownEnumValue { enum_, value },
            E::UnknownRuleGroup { layer, group } => Fields::UnknownRuleGroup { layer, group },
            E::UnknownRule { layer, rule } => Fields::UnknownRule { layer, rule },
            E::UnknownMacro { kind, name } => Fields::UnknownMacro {
                macro_kind: *kind,
                name,
            },
            E::UnknownStamp { layer, stamp } => Fields::UnknownStamp { layer, stamp },
            E::StampInUse { layer, stamp, uses } => Fields::StampInUse {
                def_kind: "stamp",
                layer,
                slug: stamp,
                uses,
            },
            E::DuplicateDef { kind, slug } => Fields::DuplicateDef {
                def_kind: *kind,
                slug,
            },
            E::InUse { kind, slug, uses } => Fields::InUse {
                def_kind: *kind,
                slug,
                uses,
            },
            // The text is the whole error; it goes out as `message`.
            E::Invalid(_) => Fields::Invalid {},
            E::Batch {
                label,
                index,
                source,
            } => Fields::Batch {
                label,
                index: *index,
                source,
            },
            E::Rollback { batch, failed } => Fields::Rollback { batch, failed },
            E::DuplicateWorld(world) => Fields::DuplicateWorld { world },
            E::UnknownReference(id) => Fields::UnknownReference { id },
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::id::slug;

    fn wire(e: CommandError) -> serde_json::Value {
        serde_json::to_value(e).unwrap()
    }

    #[test]
    fn tuple_variants_name_their_field() {
        assert_eq!(
            wire(CommandError::UnknownLevel(slug("annex"))),
            json!({ "kind": "unknown_level", "level": "annex", "message": "no level \"annex\"" })
        );
        assert_eq!(
            wire(CommandError::UnknownEntityType(slug("lever"))),
            json!({ "kind": "unknown_entity_type", "type": "lever", "message": "no entity type \"lever\"" })
        );
        assert_eq!(
            wire(CommandError::Invalid("index 9 is past the end".into())),
            json!({ "kind": "invalid", "message": "index 9 is past the end" })
        );
    }

    #[test]
    fn struct_variants_keep_their_fields_with_renames() {
        assert_eq!(
            wire(CommandError::UnknownValue {
                layer: slug("collision"),
                value: slug("lava")
            }),
            json!({ "kind": "unknown_value", "layer": "collision", "value": "lava",
                    "message": "\"lava\" is not a value of layer \"collision\"" })
        );
        assert_eq!(
            wire(CommandError::UnknownEnumValue {
                enum_: slug("facing"),
                value: slug("up")
            }),
            json!({ "kind": "unknown_enum_value", "enum": "facing", "value": "up",
                    "message": "\"up\" is not a value of enum \"facing\"" })
        );
        let e = CommandError::InUse {
            kind: DefKind::Tileset,
            slug: slug("ssw_set5"),
            uses: vec!["rule set main_tiling".into()],
        };
        let msg = e.to_string();
        assert_eq!(
            wire(e),
            json!({ "kind": "in_use", "def_kind": "tileset", "slug": "ssw_set5",
                    "uses": ["rule set main_tiling"], "message": msg })
        );
    }

    #[test]
    fn rule_macro_and_stamp_errors() {
        assert_eq!(
            wire(CommandError::UnknownMacro {
                kind: MacroKind::Legend,
                name: slug("walls")
            }),
            json!({ "kind": "unknown_macro", "macro_kind": "legend", "name": "walls",
                    "message": "rules/macros.json has no legend \"walls\"" })
        );
        assert_eq!(
            wire(CommandError::UnknownRule {
                layer: slug("main_tiling"),
                rule: slug("lip")
            })["rule"],
            "lip"
        );
        let e = CommandError::StampInUse {
            layer: slug("main_tiling"),
            stamp: slug("pillar"),
            uses: vec!["rule pillar_on_floor".into()],
        };
        let msg = e.to_string();
        assert_eq!(
            wire(e),
            json!({ "kind": "in_use", "def_kind": "stamp", "layer": "main_tiling",
                    "slug": "pillar", "uses": ["rule pillar_on_floor"], "message": msg })
        );
    }

    #[test]
    fn field_errors_name_the_owner() {
        assert_eq!(
            wire(CommandError::DuplicateField {
                owner: "entity type \"door\"".into(),
                field: slug("channel")
            }),
            json!({ "kind": "duplicate_field", "owner": "entity type \"door\"", "field": "channel",
                    "message": "entity type \"door\" already has a field \"channel\"" })
        );
        assert_eq!(
            wire(CommandError::UnknownField {
                owner: "project level_fields".into(),
                field: slug("zone")
            })["kind"],
            "unknown_field"
        );
    }

    #[test]
    fn guide_group_errors() {
        assert_eq!(
            wire(CommandError::UnknownGuideGroup {
                world: slug("main"),
                group: slug("ruined")
            }),
            json!({ "kind": "unknown_guide_group", "world": "main", "group": "ruined",
                    "message": "world \"main\" has no guide group \"ruined\"" })
        );
    }

    #[test]
    fn nested_errors_serialize_the_same_way() {
        let e = CommandError::Batch {
            label: "Agent: pool".into(),
            index: 1,
            source: Box::new(CommandError::UnknownLayer(slug("lava"))),
        };
        let msg = e.to_string();
        assert_eq!(
            wire(e),
            json!({ "kind": "batch", "label": "Agent: pool", "index": 1,
                    "source": { "kind": "unknown_layer", "layer": "lava", "message": "no layer \"lava\"" },
                    "message": msg })
        );
    }
}
