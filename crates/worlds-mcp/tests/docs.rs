//! `docs/mcp.md` against the binary: the tool set, every description and
//! input schema, and the server instructions are exactly the page's.

mod common;

use std::collections::BTreeMap;

use serde_json::{Value, json};

fn doc() -> String {
    std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/mcp.md")).unwrap()
}

/// The fenced blocks of `text` as (info string, body).
fn blocks(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut lines = text.lines();
    while let Some(l) = lines.next() {
        if let Some(info) = l.strip_prefix("```") {
            let mut body = Vec::new();
            for b in lines.by_ref() {
                if b == "```" {
                    break;
                }
                body.push(b);
            }
            out.push((info.trim().to_string(), body.join("\n")));
        }
    }
    out
}

/// Per tool section under `## Tools`: (description, schema).
fn documented_tools(doc: &str) -> BTreeMap<String, (String, Value)> {
    let tools = doc.split("\n## Tools\n").nth(1).unwrap();
    let tools = tools.split("\n## ").next().unwrap();
    let mut out = BTreeMap::new();
    for section in tools.split("\n### ").skip(1) {
        let name = section.lines().next().unwrap().trim().to_string();
        let bs = blocks(section);
        let text: Vec<&(String, String)> = bs.iter().filter(|(i, _)| i == "text").collect();
        let json: Vec<&(String, String)> = bs.iter().filter(|(i, _)| i == "json").collect();
        assert_eq!(
            (text.len(), json.len()),
            (1, 1),
            "### {name}: one text and one json block"
        );
        let schema: Value = serde_json::from_str(&json[0].1)
            .unwrap_or_else(|e| panic!("### {name}: schema doesn't parse: {e}"));
        out.insert(name, (text[0].1.clone(), schema));
    }
    out
}

#[test]
fn every_json_block_parses() {
    let doc = doc();
    let mut n = 0;
    for (info, body) in blocks(&doc) {
        if info == "json" {
            serde_json::from_str::<Value>(&body).unwrap_or_else(|e| panic!("{e}:\n{body}"));
            n += 1;
        }
    }
    assert!(n >= 20, "{n} json blocks");
}

#[test]
fn published_tools_and_instructions_equal_the_doc() {
    let doc = doc();
    let documented = documented_tools(&doc);
    assert_eq!(documented.len(), 40);
    let instructions = doc
        .split("\n## Server instructions\n")
        .nth(1)
        .and_then(|s| blocks(s).into_iter().find(|(i, _)| i == "text"))
        .unwrap()
        .1;

    let (_t, dir) = common::mini();
    let mut p = common::McpProcess::start(
        &["--project", dir.to_str().unwrap(), "--headless"],
        &dir,
        &[],
    );
    let init = p.initialize();
    assert_eq!(init["result"]["instructions"], json!(instructions));
    let list = p.request(2, "tools/list", json!({}));
    let tools = list["result"]["tools"].as_array().unwrap();
    let published: BTreeMap<String, (String, Value)> = tools
        .iter()
        .map(|t| {
            (
                t["name"].as_str().unwrap().to_string(),
                (
                    t["description"].as_str().unwrap().to_string(),
                    t["inputSchema"].clone(),
                ),
            )
        })
        .collect();
    assert_eq!(
        published.keys().collect::<Vec<_>>(),
        documented.keys().collect::<Vec<_>>()
    );
    for (name, (desc, schema)) in &documented {
        let (pd, ps) = &published[name];
        assert_eq!(pd, desc, "description of {name}");
        assert_eq!(ps, schema, "input schema of {name}");
    }
}

#[test]
fn mcp_json_example_is_the_doc_snippet() {
    let doc = doc();
    let snippet = blocks(&doc)
        .into_iter()
        .find(|(i, b)| i == "json" && b.contains("mcpServers"))
        .unwrap()
        .1;
    let example =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/mcp.json.example")).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&example).unwrap(),
        serde_json::from_str::<Value>(&snippet).unwrap()
    );
}

/// `s` as a Rust raw string literal: `r#"..."#` with enough `#`s.
fn raw(s: &str) -> String {
    let mut n = 1;
    while s.contains(&format!("\"{}", "#".repeat(n))) {
        n += 1;
    }
    let h = "#".repeat(n);
    format!("r{h}\"{s}\"{h}")
}

/// `src/catalog.rs` as generated from `docs/mcp.md`: the instructions and
/// every tool's description and schema, as raw strings, in the page's
/// order.
fn generated_catalog(doc: &str) -> String {
    let instructions = doc
        .split("\n## Server instructions\n")
        .nth(1)
        .and_then(|s| blocks(s).into_iter().find(|(i, _)| i == "text"))
        .unwrap()
        .1;
    let mut out = String::from(
        "//! The tool catalog and server instructions, exactly as `docs/mcp.md`
//! publishes them (`tests/docs.rs` checks that). Kept as constants rather
//! than derived with schemars so the published schemas are the documented
//! ones by construction. Generated from the page:
//! `WORLDS_WRITE_CATALOG=1 cargo test -p worlds-mcp --test docs catalog` writes
//! it again after the page changes.

/// The `instructions` `initialize` returns.
",
    );
    out += &format!("pub const INSTRUCTIONS: &str = {};\n\n", raw(&instructions));
    out += "/// One tool: name, description, input schema (JSON text).
pub struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    pub schema: &'static str,
}

/// Every tool, in the order of `docs/mcp.md`.
pub const TOOLS: &[ToolDef] = &[
";
    let tools = doc.split("\n## Tools\n").nth(1).unwrap();
    let tools = tools.split("\n## ").next().unwrap();
    for section in tools.split("\n### ").skip(1) {
        let name = section.lines().next().unwrap().trim();
        let bs = blocks(section);
        let text = &bs.iter().find(|(i, _)| i == "text").unwrap().1;
        let json = &bs.iter().find(|(i, _)| i == "json").unwrap().1;
        out += &format!(
            "    ToolDef {{\n        name: \"{name}\",\n        description: {},\n        schema: {},\n    }},\n",
            raw(text),
            raw(json)
        );
    }
    out += "];\n";
    out
}

/// `src/catalog.rs` is exactly what the page generates, so neither can
/// drift from the other. With `WORLDS_WRITE_CATALOG` set, write it instead.
#[test]
fn catalog_is_generated_from_the_doc() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/src/catalog.rs");
    let want = generated_catalog(&doc());
    if std::env::var_os("WORLDS_WRITE_CATALOG").is_some() {
        std::fs::write(path, &want).unwrap();
        return;
    }
    let have = std::fs::read_to_string(path).unwrap();
    assert!(
        have == want,
        "src/catalog.rs isn't what docs/mcp.md generates; run \
         WORLDS_WRITE_CATALOG=1 cargo test -p worlds-mcp --test docs catalog"
    );
}
