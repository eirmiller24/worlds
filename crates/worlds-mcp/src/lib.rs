//! worlds-mcp: an MCP server (stdio) that lets an agent read and edit a
//! Worlds project (`docs/mcp.md` is the contract).
//!
//! - [`catalog`]: the tools' names, descriptions and input schemas, and the
//!   server instructions, as constants equal to the doc.
//! - [`backend`]: the editor's socket or the in-process headless API behind
//!   one [`backend::Backend`] trait; [`backend::Connector`] picks one before
//!   every call.
//! - [`engine::Engine`]: runs tool calls (`tools.rs`), resources
//!   (`resources.rs`) and headless screenshots (`screenshot.rs`).
//! - [`server::WorldsMcp`]: the rmcp `ServerHandler` around the engine.

pub mod backend;
pub mod catalog;
pub mod engine;
pub mod project;
pub mod resources;
pub mod screenshot;
pub mod server;
mod tools;

pub use engine::{Config, Engine};
pub use server::WorldsMcp;
