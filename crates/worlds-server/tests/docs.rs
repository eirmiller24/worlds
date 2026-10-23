//! `docs/api.md` is checked here, as its "Testing" section describes: every
//! JSON block parses; every request block (not `norun`) is replayed in
//! order over one WebSocket connection to a headless server on a fresh copy
//! of `mini.worlds`, and the next block is its expected response.

mod common;

use serde_json::{Value, json};
use worlds_server::ServerInfo;

/// The fields of a result that must match exactly; others need only exist.
const EXACT: [&str; 7] = [
    "rev", "status", "label", "id", "entries", "steps", "changes",
];

struct Block {
    value: Value,
    norun: bool,
    line: usize,
}

fn blocks(doc: &str) -> Vec<Block> {
    let mut out = Vec::new();
    let mut lines = doc.lines().enumerate();
    while let Some((n, line)) = lines.next() {
        let norun = match line.trim_end() {
            "```json" => false,
            "```json norun" => true,
            _ => continue,
        };
        let mut text = String::new();
        for (_, l) in lines.by_ref() {
            if l.trim_end() == "```" {
                break;
            }
            text.push_str(l);
            text.push('\n');
        }
        let value: Value = serde_json::from_str(&text).unwrap_or_else(|e| {
            panic!(
                "docs/api.md line {}: block doesn't parse: {e}\n{text}",
                n + 1
            )
        });
        out.push(Block {
            value,
            norun,
            line: n + 1,
        });
    }
    out
}

fn is_request(v: &Value) -> bool {
    v.get("jsonrpc").is_some() && v.get("method").is_some() && v.get("id").is_some()
}

fn check(method: &str, expected: &Value, got: &Value, line: usize) {
    let ctx = || format!("docs/api.md line {line}\nexpected {expected:#}\ngot {got:#}");
    if let Some(e) = expected.get("error") {
        let g = got
            .get("error")
            .unwrap_or_else(|| panic!("expected an error\n{}", ctx()));
        assert_eq!(e["code"], g["code"], "error code\n{}", ctx());
        let code = e["code"].as_i64().unwrap();
        if code == -32004 {
            assert_eq!(e["data"]["kind"], g["data"]["kind"], "data.kind\n{}", ctx());
        }
        if code == -32002 {
            assert_eq!(
                e["data"]["reason"],
                g["data"]["reason"],
                "data.reason\n{}",
                ctx()
            );
        }
        return;
    }
    let e = &expected["result"];
    let g = got
        .get("result")
        .unwrap_or_else(|| panic!("expected a result\n{}", ctx()));
    let e = e.as_object().unwrap();
    for (k, v) in e {
        let gv = g
            .get(k)
            .unwrap_or_else(|| panic!("result lacks {k:?}\n{}", ctx()));
        // The text view of `level.get` is exact too: it is the format an
        // agent reads (docs/format.md, "Text view").
        if EXACT.contains(&k.as_str()) || (method == "level.get" && k == "text") {
            assert_eq!(v, gv, "result.{k}\n{}", ctx());
        }
    }
}

#[test]
fn api_md_examples_replay() {
    let doc = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/api.md"),
    )
    .unwrap();
    let blocks = blocks(&doc);
    assert!(blocks.len() > 100, "found only {} blocks", blocks.len());

    let (_tmp, dir) = common::mini();
    let server = worlds_server::headless::run(&dir, Default::default()).unwrap();
    let info = ServerInfo::read(&dir).unwrap().unwrap();
    assert_eq!(info.port, server.port());

    let rt = tokio::runtime::Runtime::new().unwrap();
    let replayed = rt.block_on(async {
        let mut ws = common::connect(info.port).await;
        let mut replayed = 0;
        let mut i = 0;
        while i < blocks.len() {
            let b = &blocks[i];
            if !is_request(&b.value) {
                i += 1;
                continue;
            }
            let resp = blocks
                .get(i + 1)
                .unwrap_or_else(|| panic!("line {}: a request without a response block", b.line));
            assert_eq!(
                resp.value["id"], b.value["id"],
                "line {}: response id",
                resp.line
            );
            i += 2;
            if b.norun {
                continue;
            }
            let mut req = b.value.clone();
            if req["method"] == "session.hello" {
                req["params"]["token"] = json!(info.token);
            }
            common::send(&mut ws, &req).await;
            let got = loop {
                let v = common::recv(&mut ws)
                    .await
                    .unwrap_or_else(|| panic!("line {}: socket closed", b.line));
                if v.get("id") == Some(&req["id"]) {
                    break v;
                }
            };
            check(
                req["method"].as_str().unwrap_or(""),
                &resp.value,
                &got,
                b.line,
            );
            replayed += 1;
        }
        replayed
    });
    assert!(replayed >= 70, "replayed only {replayed} requests");
    server.stop();
    assert!(!ServerInfo::path(&dir).exists());
}
