//! worlds-server: the Worlds live API, JSON-RPC 2.0 over a localhost
//! WebSocket (`docs/api.md` is the contract).
//!
//! Three layers, each usable on its own:
//!
//! - [`Handler`] runs one request against a document lent by a [`Host`]:
//!   no threads, no sockets. Every method of `docs/api.md` lives here.
//! - [`Server::start`] binds `127.0.0.1:0`, writes `.worlds/server.json`,
//!   does the `session.hello` handshake on a tokio runtime and queues
//!   requests; the returned [`ServerHandle`] is what the editor's UI thread
//!   drains once per frame ([`ServerHandle::drain`]), so the document has
//!   one writer.
//! - [`headless`] hosts a project loaded from disk: [`headless::run`]
//!   serves it over the same socket from a thread of its own, and
//!   [`headless::Session`] answers calls in-process with no runtime at all.
//!
//! See the crate README for the editor loop.

mod approval;
mod describe;
mod discovery;
pub mod handler;
pub mod headless;
mod host;
mod queue;
pub mod rpc;
mod server;
mod transport;

pub use approval::PendingBatch;
pub use describe::field_json;
pub use discovery::ServerInfo;
pub use handler::{
    ChangeKind, Client, ConnId, Handler, HandlerConfig, Out, Outcome, UserChange, is_write,
};
pub use host::{
    Doc, Host, Selection, Snapshot, SnapshotRequest, ViewTarget, ViewportInfo, needs_editor,
    unknown_viewport,
};
pub use rpc::RpcError;
pub use server::{ClientInfo, Handled, Server, ServerConfig, ServerError, ServerHandle};

/// The protocol version `session.hello` reports.
pub const PROTOCOL: u32 = 1;

/// `{ name, version }` of this server.
pub fn server_info() -> serde_json::Value {
    serde_json::json!({ "name": "worlds", "version": env!("CARGO_PKG_VERSION") })
}

/// A fresh session id: 8 characters of `a-z 0-9`.
pub fn random_session() -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    (0..8)
        .map(|_| ALPHABET[rand::random_range(0..ALPHABET.len())] as char)
        .collect()
}

/// The project-relative paths (`/`-separated) a save of the `dirty` paths
/// writes, as `project.save` and the `saved` event report them: a dirty
/// level directory stands for every file in it, so paths inside one are
/// left out.
pub fn saved_paths<'a>(dirty: impl IntoIterator<Item = &'a std::path::PathBuf>) -> Vec<String> {
    let dirty: Vec<&std::path::PathBuf> = dirty.into_iter().collect();
    dirty
        .iter()
        .filter(|p| !p.ancestors().skip(1).any(|a| dirty.iter().any(|d| *d == a)))
        .map(|p| handler::params::rel_path(p))
        .collect()
}

/// Save the dirty files of a document (`save_dirty`), then `export/` when
/// the project has `export_on_save`. Returns the project-relative paths
/// saved (a level directory, without extension, stands for every file in
/// it). The default of [`Host::save`].
pub fn save_project(doc: &mut Doc<'_>) -> Result<Vec<String>, RpcError> {
    let files = saved_paths(doc.project.files.dirty());
    worlds_core::save_dirty(doc.project, doc.dir).map_err(|e| RpcError::from_format(&e))?;
    if doc.project.export_on_save {
        let levels: Vec<worlds_core::Slug> = doc.project.levels().map(|l| l.slug.clone()).collect();
        // The warnings are `project.bake`'s and `project.export`'s to report.
        handler::export_levels(doc.project, doc.dir, doc.images, &levels, true)?;
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    #[test]
    fn saved_paths_fold_level_directories() {
        let dirty: Vec<PathBuf> = [
            "layers.json",
            "worlds/main/levels/wing",
            "worlds/main/levels/wing/collision.json",
            "worlds/main/levels/crevasse/collision.json",
        ]
        .iter()
        .map(PathBuf::from)
        .collect();
        assert_eq!(
            super::saved_paths(&dirty),
            [
                "layers.json",
                "worlds/main/levels/wing",
                "worlds/main/levels/crevasse/collision.json"
            ]
        );
    }
}
