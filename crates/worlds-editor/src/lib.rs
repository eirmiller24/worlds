//! Worlds desktop editor: the `worlds` binary's library half.
//!
//! - [`app`]: the shell (dock layout, panels, toolbar, command palette,
//!   project lifecycle).
//! - [`viewport`]: the wgpu map renderer and camera.
//! - [`tools`]: the seam between viewport input and the command layer.
//! - [`doc`]: the open project, its level index and cached bakes.
//! - [`annot`]: annotations: hit tests, the Note and Measure tools, the
//!   selection edits across kinds (move, nudge, duplicate, delete).
//! - [`clipboard`]: copy, cut and paste of fragments, the paste ghost and
//!   its mirror and rotate.
//! - [`arrange`]: selection transforms, aligning levels, level-edge snaps.
//! - [`library`]: assemblies in the Palette's Library; [`thumbs`]: their
//!   thumbnails in `.worlds/thumbs/`.
//! - [`edit`]: the command layer (history, gestures, incremental rebake, save).
//! - [`headless`]: offscreen rendering for `--screenshot` and tests.
//! - [`script`]: `--script` files (commands and replayed tool gestures).
//! - [`signals`]: SIGINT/SIGTERM shut the editor down cleanly.
//! - [`autosave`]: autosave timing and the writer thread (crash recovery is
//!   in [`app::recovery`]).
//! - [`live`]: hosting the live API (`worlds-server`) in the editor;
//!   [`ghost`]: approval mode's ghost overlay; [`snapshot`]: map-only PNGs.
//!
//! See `README.md` in this crate for how to run it and how to wire tools.

pub mod annot;
pub mod app;
pub mod arrange;
pub mod autosave;
pub mod clipboard;
pub mod doc;
pub mod edit;
pub mod ghost;
pub mod headless;
pub mod library;
pub mod live;
pub mod log;
pub mod script;
pub mod signals;
pub mod snapshot;
pub mod thumbs;
pub mod tools;
pub mod view;
pub mod viewport;
