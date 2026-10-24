//! Shared test helpers: a temp copy of the mini fixture with placeholder
//! tileset images, and a small WebSocket client.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

pub fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../worlds-core/tests/fixtures/mini.worlds")
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// A fresh copy of `mini.worlds` with opaque placeholder images for its
/// tilesets (castle_main 512x512, ssw_set5 268x268). Keep the `TempDir`.
pub fn mini() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("mini.worlds");
    copy_dir(&fixture_dir(), &dir);
    std::fs::create_dir_all(dir.join("art")).unwrap();
    for (name, size) in [("castle_main.png", 512), ("ssw_set5.png", 268)] {
        image::RgbaImage::from_pixel(size, size, image::Rgba([90, 90, 90, 255]))
            .save(dir.join("art").join(name))
            .unwrap();
    }
    let dir = std::fs::canonicalize(dir).unwrap();
    (tmp, dir)
}

/// The game fixture (`tests/fixtures/game/`: `game.worlds` and the
/// `assets/` its tilesets name) copied next to `dir`, so `../game.worlds`
/// is a second project to import from.
pub fn game_beside(dir: &Path) -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/game");
    let parent = dir.parent().unwrap();
    copy_dir(&root.join("game.worlds"), &parent.join("game.worlds"));
    copy_dir(&root.join("assets"), &parent.join("assets"));
    parent.join("game.worlds")
}

pub type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Connect to a server on `port`.
pub async fn connect(port: u16) -> Ws {
    let (ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/"))
        .await
        .unwrap();
    ws
}

pub async fn send(ws: &mut Ws, v: &Value) {
    ws.send(Message::Text(v.to_string().into())).await.unwrap();
}

/// The next text message as JSON (None when the socket closed), within 5 s.
pub async fn recv(ws: &mut Ws) -> Option<Value> {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("no message within 5 s")?;
        match msg {
            Ok(Message::Text(t)) => return Some(serde_json::from_str(&t).unwrap()),
            Ok(Message::Close(_)) | Err(_) => return None,
            Ok(_) => continue,
        }
    }
}

/// Send a request and wait for its response, collecting notifications
/// that arrive meanwhile.
pub async fn call(
    ws: &mut Ws,
    id: u64,
    method: &str,
    params: Value,
    events: &mut Vec<Value>,
) -> Value {
    send(
        ws,
        &json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }),
    )
    .await;
    loop {
        let v = recv(ws).await.expect("socket closed before the response");
        if v.get("id") == Some(&json!(id)) {
            return v;
        }
        events.push(v);
    }
}

/// Say hello as `name`.
pub async fn hello(ws: &mut Ws, token: &str, name: &str) -> Value {
    let mut ev = Vec::new();
    call(
        ws,
        0,
        "session.hello",
        json!({ "token": token, "client": { "name": name, "version": "1" } }),
        &mut ev,
    )
    .await
}
