//! Shared test helpers: a temp copy of the mini fixture, an in-process
//! engine on it, and a client for the built `worlds-mcp` binary on stdio.

#![allow(dead_code)]

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{Value, json};
use worlds_mcp::{Config, Engine};

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

/// A fresh copy of `mini.worlds` with opaque placeholder tileset images
/// (castle_main 512x512, ssw_set5 268x268). Keep the `TempDir`.
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

/// An engine on `dir` named `claude-code`.
/// The game fixture (`tests/fixtures/game/`) copied next to `dir`, so
/// `../game.worlds` is a second project to import from.
pub fn game_beside(dir: &Path) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/game");
    let parent = dir.parent().unwrap();
    copy_dir(&root.join("game.worlds"), &parent.join("game.worlds"));
    copy_dir(&root.join("assets"), &parent.join("assets"));
}

pub fn engine(dir: &Path, force_headless: bool) -> Engine {
    let mut e = Engine::new(Config {
        dir: dir.to_path_buf(),
        force_headless,
        name: "mcp".into(),
    });
    e.set_client_name("claude-code");
    e
}

/// A tool call's outcome: (is_error, the text blocks joined).
pub fn call(e: &mut Engine, tool: &str, args: Value) -> (bool, String) {
    let r = e.call_tool(tool, args);
    let text = r
        .content
        .iter()
        .filter_map(|c| c.as_text().map(|t| t.text.clone()))
        .collect::<Vec<_>>()
        .join("\n");
    (r.is_error == Some(true), text)
}

/// A successful call's JSON result.
pub fn ok(e: &mut Engine, tool: &str, args: Value) -> Value {
    let (err, text) = call(e, tool, args.clone());
    assert!(!err, "{tool} {args} failed: {text}");
    serde_json::from_str(&text).unwrap_or_else(|_| panic!("{tool}: not JSON: {text}"))
}

/// A failed call's message.
pub fn err(e: &mut Engine, tool: &str, args: Value) -> String {
    let (is_err, text) = call(e, tool, args.clone());
    assert!(is_err, "{tool} {args} should fail, got {text}");
    text
}

/// The rows of a saved int_grid file.
pub fn rows_on_disk(dir: &Path, level: &str, layer: &str) -> Vec<String> {
    let path = dir.join(format!("worlds/main/levels/{level}/{layer}.json"));
    let v: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    v["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r.as_str().unwrap().to_string())
        .collect()
}

/// The built binary on stdio.
pub struct McpProcess {
    pub child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl McpProcess {
    /// Start `worlds-mcp` with `args` (and extra environment).
    pub fn start(args: &[&str], cwd: &Path, env: &[(&str, &str)]) -> Self {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_worlds-mcp"));
        cmd.args(args)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        McpProcess {
            child,
            stdin,
            stdout,
        }
    }

    pub fn send(&mut self, v: &Value) {
        writeln!(self.stdin, "{v}").unwrap();
        self.stdin.flush().unwrap();
    }

    /// The next message with `id`.
    pub fn response(&mut self, id: u64) -> Value {
        loop {
            let mut line = String::new();
            let n = self.stdout.read_line(&mut line).unwrap();
            assert!(n > 0, "worlds-mcp closed stdout before answering {id}");
            let v: Value = serde_json::from_str(&line).unwrap();
            if v.get("id") == Some(&json!(id)) {
                return v;
            }
        }
    }

    /// `initialize` as `claude-code`, then `notifications/initialized`.
    pub fn initialize(&mut self) -> Value {
        self.send(
            &json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": { "name": "claude-code", "version": "1" } } }),
        );
        let r = self.response(1);
        self.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
        r
    }

    pub fn request(&mut self, id: u64, method: &str, params: Value) -> Value {
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        self.response(id)
    }
}

impl Drop for McpProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
