//! The built binary over stdio, as Claude Code drives it: JSON-RPC lines
//! in, JSON-RPC lines out.

mod common;

use serde_json::{Value, json};

#[test]
fn initialize_list_and_describe() {
    let (_t, dir) = common::mini();
    // Started inside the project, no --project: found by walking up.
    let mut p = common::McpProcess::start(&[], &dir.join("worlds"), &[]);
    let init = p.initialize();
    assert_eq!(init["result"]["serverInfo"]["name"], "worlds-mcp");
    assert!(init["result"]["capabilities"]["tools"].is_object());
    assert!(init["result"]["capabilities"]["resources"].is_object());
    let list = p.request(2, "tools/list", json!({}));
    let tools = list["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 34);
    let ed = tools
        .iter()
        .find(|t| t["name"] == "edit_definitions")
        .expect("edit_definitions is listed");
    assert_eq!(ed["inputSchema"]["required"], json!(["patch"]));
    let r = p.request(
        3,
        "tools/call",
        json!({ "name": "describe_project", "arguments": {} }),
    );
    let res = &r["result"];
    assert_eq!(res["isError"], false);
    let text = res["content"][0]["text"].as_str().unwrap();
    let d: Value = serde_json::from_str(text).unwrap();
    assert_eq!(d["name"], "Mini");
    assert_eq!(d["dir"], dir.to_str().unwrap());

    // A write lands on disk, labelled with the client's name.
    let r = p.request(
        4,
        "tools/call",
        json!({ "name": "paint", "arguments": {
        "level": "crevasse", "layer": "collision", "cells": [[1, 1, "water"]] } }),
    );
    let w: Value =
        serde_json::from_str(r["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(
        (w["applied"].clone(), w["saved"].clone()),
        (json!(true), json!(true))
    );
    assert!(common::rows_on_disk(&dir, "crevasse", "collision")[1].starts_with("#~"));

    // A failing tool is a result with isError, not a protocol error.
    let r = p.request(
        5,
        "tools/call",
        json!({ "name": "read_level", "arguments": { "level": "nowhere" } }),
    );
    assert_eq!(r["result"]["isError"], true);
    assert!(
        r["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("nowhere")
    );

    // Resources.
    let r = p.request(6, "resources/list", json!({}));
    assert!(r["result"]["resources"].as_array().unwrap().len() > 10);
    let r = p.request(7, "resources/templates/list", json!({}));
    assert_eq!(
        r["result"]["resourceTemplates"].as_array().unwrap().len(),
        5
    );
    let r = p.request(
        8,
        "resources/read",
        json!({ "uri": "worlds://levels/crevasse/collision.json" }),
    );
    let c = &r["result"]["contents"][0];
    assert_eq!(c["mimeType"], "application/json");
    assert!(c["text"].as_str().unwrap().contains("#~"));
}

#[cfg(unix)]
#[test]
fn headless_screenshot_failure_names_the_binary() {
    use std::os::unix::fs::PermissionsExt;
    let (t, dir) = common::mini();
    let script = t.path().join("fake-worlds");
    std::fs::write(&script, "#!/bin/sh\necho \"no GPU here: $*\" >&2\nexit 3\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut p = common::McpProcess::start(
        &["--project", dir.to_str().unwrap()],
        t.path(),
        &[("WORLDS_EDITOR_BIN", script.to_str().unwrap())],
    );
    p.initialize();
    let r = p.request(
        2,
        "tools/call",
        json!({ "name": "screenshot", "arguments": { "level": "crevasse", "size": [256, 256] } }),
    );
    assert_eq!(r["result"]["isError"], true);
    let m = r["result"]["content"][0]["text"].as_str().unwrap();
    assert!(m.contains(script.to_str().unwrap()), "{m}");
    assert!(
        m.contains("exit status: 3") || m.contains("exit code: 3"),
        "{m}"
    );
    // The contract's flags reached the binary.
    assert!(
        m.contains("--snapshot --level crevasse --max-size 256x256"),
        "{m}"
    );
    // A missing binary is an error naming the path too.
    let mut p = common::McpProcess::start(
        &["--project", dir.to_str().unwrap()],
        t.path(),
        &[("WORLDS_EDITOR_BIN", "/nonexistent/worlds")],
    );
    p.initialize();
    let r = p.request(
        2,
        "tools/call",
        json!({ "name": "screenshot", "arguments": { "level": "crevasse" } }),
    );
    assert_eq!(r["result"]["isError"], true);
    let m = r["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        m.contains("/nonexistent/worlds") && m.contains("WORLDS_EDITOR_BIN"),
        "{m}"
    );
}

#[test]
fn startup_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_worlds-mcp"))
        .args(["--project", "nope"])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("no project.json"), "{err}");
    assert!(out.stdout.is_empty(), "nothing on the MCP channel");
}

/// The client's name reaches the editor's history, whichever lifecycle the
/// client uses: `initialize` with `clientInfo`, or (as Claude Code does) the
/// stateless one, `server/discover` and then `clientInfo` in every
/// request's `_meta` with no `initialize` at all.
#[test]
fn the_client_name_reaches_the_editor() {
    use worlds_mcp::backend::{Backend, EditorBackend};
    use worlds_server::{ServerInfo, headless};

    let (_t, dir) = common::mini();
    let server = headless::run(&dir, headless::Options::default()).unwrap();
    let info = ServerInfo::read(&dir).unwrap().unwrap();
    let mut observer = EditorBackend::connect(&info, &dir, "observer").unwrap();
    let origin_of_last = |o: &mut EditorBackend| {
        let u = o.call("history.undo", json!({ "any": true })).unwrap();
        o.call("history.redo", json!({ "any": true })).unwrap();
        u["steps"][0]["origin"]["agent"].clone()
    };
    let paint = |x: u32| {
        json!({ "name": "paint", "arguments": {
            "level": "crevasse", "layer": "collision", "cells": [[x, 1, "water"]] } })
    };

    // Stateless: discover, then _meta on the call.
    let mut p = common::McpProcess::start(&["--project", dir.to_str().unwrap()], &dir, &[]);
    let meta = json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientInfo": { "name": "claude-code", "version": "2.1.307" },
        "io.modelcontextprotocol/clientCapabilities": {}
    });
    let d = p.request(1, "server/discover", json!({ "_meta": meta }));
    assert!(d.get("result").is_some(), "{d}");
    let mut call = paint(1);
    call["_meta"] = meta.clone();
    let r = p.request(2, "tools/call", call);
    assert_eq!(r["result"]["isError"], false, "{r}");
    assert_eq!(origin_of_last(&mut observer), "claude-code");
    drop(p);

    // initialize with clientInfo.
    let mut p = common::McpProcess::start(&["--project", dir.to_str().unwrap()], &dir, &[]);
    p.send(
        &json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-11-25", "capabilities": {},
        "clientInfo": { "name": "Other Agent", "version": "1" } } }),
    );
    p.response(1);
    p.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
    let r = p.request(2, "tools/call", paint(2));
    assert_eq!(r["result"]["isError"], false, "{r}");
    assert_eq!(origin_of_last(&mut observer), "otheragent");
    drop(p);
    server.stop();
}

/// `edit_rules` with a new template, a reference to it and a move is one
/// undo entry; `undo` puts `rules/main_tiling.json` and `rules/macros.json`
/// back byte for byte (headless: on disk), as saving the project before the
/// edit would have written them (the fixture itself is hand-formatted).
#[test]
fn edit_rules_with_templates_undoes_byte_identically() {
    let (_t, dir) = common::mini();
    let read = |f: &str| std::fs::read(dir.join(f)).unwrap();
    let mut p = common::McpProcess::start(
        &["--project", dir.to_str().unwrap(), "--headless"],
        &dir,
        &[],
    );
    p.initialize();
    // The files as a save would write them now.
    let resource = |p: &mut common::McpProcess, id: u64, uri: &str| -> Vec<u8> {
        let r = p.request(id, "resources/read", json!({ "uri": uri }));
        r["result"]["contents"][0]["text"]
            .as_str()
            .unwrap()
            .as_bytes()
            .to_vec()
    };
    let rules0 = resource(&mut p, 10, "worlds://defs/rules/main_tiling.json");
    let macros0 = resource(&mut p, 11, "worlds://defs/rules/macros.json");
    let tool = |p: &mut common::McpProcess, id: u64, name: &str, args: Value| -> Value {
        let r = p.request(id, "tools/call", json!({ "name": name, "arguments": args }));
        assert_eq!(r["result"]["isError"], false, "{name}: {r}");
        serde_json::from_str(r["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
    };
    let r = tool(
        &mut p,
        2,
        "edit_rules",
        json!({ "layer": "main_tiling", "label": "ledges", "patch": {
            "set_macros": [ { "kind": "template", "name": "ledge", "def": { "legend": "walls", "rules": [
                { "slug": "top", "match": { "rows": [".", "#", "*"] }, "out": { "tiles": [[0, 0]] }, "break": true },
                { "slug": "face", "match": { "rows": "edge_top" }, "out": { "tiles": [[0, 1]] } } ] } } ],
            "set_rules": [ { "group": "walls", "rule": { "use": "ledge", "slug": "ledges", "tileset": "castle_main", "origin": [14, 0] } } ],
            "move_rules": [ { "rule": "ledges", "group": "pillars", "index": 0 } ] } }),
    );
    assert_eq!(r["undo_label"], "Agent: edit_rules ledges");
    assert_eq!(r["saved"], true);
    let rules1 = String::from_utf8(read("rules/main_tiling.json")).unwrap();
    let pillars = rules1.find("\"pillars\"").unwrap();
    assert!(rules1.find("\"ledges\"").unwrap() > pillars, "{rules1}");
    assert!(
        String::from_utf8(read("rules/macros.json"))
            .unwrap()
            .contains("\"ledge\"")
    );
    let u = tool(&mut p, 3, "undo", json!({}));
    assert_eq!(u["undone"], json!(["Agent: edit_rules ledges"]));
    assert_eq!(read("rules/main_tiling.json"), rules0);
    assert_eq!(read("rules/macros.json"), macros0);
    // Nothing else of ours to undo: it was one entry.
    let u = tool(&mut p, 4, "undo", json!({}));
    assert_eq!(u["undone"], json!([]));
    assert_eq!(
        resource(&mut p, 12, "worlds://defs/rules/main_tiling.json"),
        rules0
    );
}
