//! Worlds core: the project document model, the on-disk file format,
//! validation and JSON Schema generation.
//!
//! ```no_run
//! let mut project = worlds_core::load("mygame.worlds")?;
//! for problem in worlds_core::validate(&project) {
//!     eprintln!("{problem}");
//! }
//! worlds_core::save(&mut project, "mygame.worlds")?;
//! # Ok::<(), worlds_core::format::Error>(())
//! ```
//!
//! The model is plain data with no interior mutability. Every edit from the
//! editor, the API or an agent goes through [`command::Command`] and
//! [`Project::apply`], which returns the exact inverse and the changes;
//! [`command::History`] builds undo and redo on that.

pub mod assembly;
pub mod autosave;
pub mod command;
pub mod export;
pub mod find;
pub mod format;
pub mod fragment;
pub mod id;
pub mod ldtk;
pub mod model;
pub mod schema;
pub mod text;
pub mod transform;
pub mod validate;

pub use command::{Applied, Change, Command, CommandError, History};
pub use format::{Error, load, save, save_all, save_dirty};
pub use id::{Id, Slug, slug};
pub use model::*;
pub use validate::{Problem, validate};
