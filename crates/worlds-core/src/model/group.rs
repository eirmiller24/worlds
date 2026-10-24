//! Object groups (`level.json` `groups`): a named set of a level's entities
//! and annotations that the editor selects and moves together (a puzzle, an
//! encounter). Members are ids; tile stacks have no id and are never
//! members. Editor organisation only: nothing of it is exported.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::id::Id;

/// One entry of `level.json` `groups`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ObjectGroup {
    /// Unique among the level's groups.
    pub id: Id,
    pub name: String,
    /// Entity and level annotation ids, in the order they were added.
    pub members: Vec<Id>,
}

impl ObjectGroup {
    pub fn new(id: Id, name: impl Into<String>, members: Vec<Id>) -> Self {
        ObjectGroup {
            id,
            name: name.into(),
            members,
        }
    }

    pub fn contains(&self, id: &Id) -> bool {
        self.members.contains(id)
    }
}
