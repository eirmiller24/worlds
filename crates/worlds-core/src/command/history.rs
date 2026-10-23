//! Undo and redo on top of [`Project::apply`].
//!
//! Each entry holds the command that reverses it (an inverse on the undo
//! stack, the original again on the redo stack), a label for the UI and who
//! made it. A gesture (a brush stroke, a drag) collects the inverses of every
//! command pushed during it and becomes one entry, a `Batch` of those
//! inverses in reverse order. Depth is unlimited.

use serde::{Deserialize, Serialize};

use super::{Change, Command, CommandError};
use crate::model::Project;

/// Who made an edit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    User,
    /// An agent over the API; the string names it ("claude-code").
    Agent(String),
}

/// One undoable (or redoable) step.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub label: String,
    pub origin: Origin,
    /// Applying this moves the entry to the other stack: on the undo stack
    /// it is the inverse of what was done, on the redo stack the redo.
    pub command: Command,
}

#[derive(Debug, Clone, Default)]
struct Gesture {
    label: String,
    origin: Option<Origin>,
    /// Inverses in the order their commands were applied.
    inverses: Vec<Command>,
}

/// Undo and redo stacks of labelled entries.
#[derive(Debug, Clone, Default)]
pub struct History {
    undo: Vec<Entry>,
    redo: Vec<Entry>,
    gesture: Option<Gesture>,
}

impl History {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start a gesture: every command pushed until [`Self::end_gesture`]
    /// becomes one entry with this label and origin. Starting a gesture
    /// while one is open ends the open one first.
    pub fn begin_gesture(&mut self, label: impl Into<String>, origin: Origin) {
        self.end_gesture();
        self.gesture = Some(Gesture {
            label: label.into(),
            origin: Some(origin),
            inverses: Vec::new(),
        });
    }

    /// True while a gesture is open.
    pub fn in_gesture(&self) -> bool {
        self.gesture.is_some()
    }

    /// Apply a command and record it. Inside a gesture it joins the gesture;
    /// otherwise it is its own entry, labelled [`Command::label`], made by
    /// the user. A no-op records nothing. Returns the changes.
    pub fn push(
        &mut self,
        project: &mut Project,
        cmd: Command,
    ) -> Result<Vec<Change>, CommandError> {
        self.push_as(project, cmd, Origin::User)
    }

    /// [`Self::push`] with an origin for a stand-alone entry (inside a
    /// gesture, the gesture's origin wins).
    pub fn push_as(
        &mut self,
        project: &mut Project,
        cmd: Command,
        origin: Origin,
    ) -> Result<Vec<Change>, CommandError> {
        let label = cmd.label();
        let applied = project.apply(cmd)?;
        if applied.changes.is_empty() {
            return Ok(applied.changes);
        }
        match &mut self.gesture {
            Some(g) => g.inverses.push(applied.inverse),
            None => self.push_entry(Entry {
                label,
                origin,
                command: applied.inverse,
            }),
        }
        Ok(applied.changes)
    }

    /// Close the open gesture. Returns true if it recorded an entry (a
    /// gesture where nothing changed records none).
    pub fn end_gesture(&mut self) -> bool {
        let Some(g) = self.gesture.take() else {
            return false;
        };
        if g.inverses.is_empty() {
            return false;
        }
        let command = undo_command(&g.label, g.inverses);
        self.push_entry(Entry {
            label: g.label,
            origin: g.origin.unwrap_or(Origin::User),
            command,
        });
        true
    }

    /// Record one entry for commands the caller applied itself, without
    /// touching the open gesture. `inverses` are the inverses of those
    /// commands in the order the commands were applied; the entry undoes
    /// them last first, like a gesture. No-op inverses are dropped; when
    /// nothing is left, nothing is recorded and this returns false.
    /// Otherwise the redo stack is cleared and it returns true.
    ///
    /// This is how the API server records an agent's batch: it collects
    /// the inverses of the batch's writes itself (so the user's gestures
    /// and the agent's batch never share the one gesture slot) and records
    /// them as one entry when the batch ends. If the user records an entry
    /// while the batch is open, the server records the agent's part so far
    /// first, so the undo stack stays in the order things happened.
    pub fn record(
        &mut self,
        label: impl Into<String>,
        origin: Origin,
        inverses: Vec<Command>,
    ) -> bool {
        let inverses: Vec<Command> = inverses.into_iter().filter(|c| !c.is_noop()).collect();
        if inverses.is_empty() {
            return false;
        }
        let label = label.into();
        let command = undo_command(&label, inverses);
        self.push_entry(Entry {
            label,
            origin,
            command,
        });
        true
    }

    /// Abandon the open gesture, reverting what it applied (Escape during a
    /// drag). Returns the changes of the revert.
    pub fn cancel_gesture(&mut self, project: &mut Project) -> Result<Vec<Change>, CommandError> {
        let Some(mut g) = self.gesture.take() else {
            return Ok(Vec::new());
        };
        g.inverses.reverse();
        let a = project.apply(Command::Batch {
            label: g.label,
            commands: g.inverses,
        })?;
        Ok(a.changes)
    }

    fn push_entry(&mut self, entry: Entry) {
        self.undo.push(entry);
        self.redo.clear();
    }

    /// Undo the last entry (ending an open gesture first). `Ok(None)` when
    /// there is nothing to undo. If the inverse fails (the project was
    /// changed behind the history's back), nothing changes and the entry
    /// stays.
    pub fn undo(&mut self, project: &mut Project) -> Result<Option<Vec<Change>>, CommandError> {
        self.end_gesture();
        Self::step(project, &mut self.undo, &mut self.redo)
    }

    /// Redo the last undone entry.
    pub fn redo(&mut self, project: &mut Project) -> Result<Option<Vec<Change>>, CommandError> {
        self.end_gesture();
        Self::step(project, &mut self.redo, &mut self.undo)
    }

    fn step(
        project: &mut Project,
        from: &mut Vec<Entry>,
        to: &mut Vec<Entry>,
    ) -> Result<Option<Vec<Change>>, CommandError> {
        let Some(entry) = from.last() else {
            return Ok(None);
        };
        let applied = project.apply(entry.command.clone())?;
        let entry = from.pop().unwrap();
        to.push(Entry {
            command: applied.inverse,
            ..entry
        });
        Ok(Some(applied.changes))
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
            || self
                .gesture
                .as_ref()
                .is_some_and(|g| !g.inverses.is_empty())
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Label of what undo would undo.
    pub fn undo_label(&self) -> Option<&str> {
        self.undo.last().map(|e| e.label.as_str())
    }

    pub fn redo_label(&self) -> Option<&str> {
        self.redo.last().map(|e| e.label.as_str())
    }

    /// The undo stack, oldest first.
    pub fn undo_entries(&self) -> &[Entry] {
        &self.undo
    }

    /// The redo stack, the next redo last.
    pub fn redo_entries(&self) -> &[Entry] {
        &self.redo
    }

    /// Forget everything (after reloading the project from disk).
    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.gesture = None;
    }
}

/// One command undoing `inverses` (given in the order their commands were
/// applied): the inverse itself when there is one, else a batch of them in
/// reverse order.
fn undo_command(label: &str, mut inverses: Vec<Command>) -> Command {
    if inverses.len() == 1 {
        return inverses.pop().unwrap();
    }
    inverses.reverse();
    Command::Batch {
        label: label.to_owned(),
        commands: inverses,
    }
}
