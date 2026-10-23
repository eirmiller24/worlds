//! The tool engine: one synchronous object that owns the backend and the
//! agent's view of the project (last seen rev, what it last read, notices),
//! and runs tool calls. The MCP layer (`server.rs`) only forwards to it, and
//! the tests drive it directly.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use rmcp::model::{CallToolResult, ContentBlock};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use worlds_server::RpcError;
use worlds_server::rpc::code;

use crate::backend::{Connector, Mode};

/// What a tool returns: its content blocks, or an agent-readable error.
pub type ToolResult = Result<Vec<ContentBlock>, String>;

/// At most this many changes are listed in a write result.
const MAX_CHANGES: usize = 20;

/// Who configured the engine.
#[derive(Debug, Clone)]
pub struct Config {
    /// The project directory (canonical).
    pub dir: PathBuf,
    /// `--headless`: never talk to the editor.
    pub force_headless: bool,
    /// The client name until `initialize` names one (`--name`, else `mcp`).
    pub name: String,
}

/// An edit someone else made to a level, remembered until the agent reads
/// or writes that level.
#[derive(Debug, Clone)]
struct Foreign {
    rev: u64,
    level: String,
    text: String,
}

/// The tool engine.
pub struct Engine {
    pub(crate) conn: Connector,
    /// The newest rev seen (results and `changed` events).
    pub(crate) rev: u64,
    /// The rev at which the agent last read (or wrote) each level.
    last_read: HashMap<String, u64>,
    foreign: Vec<Foreign>,
    /// Notices for the next tool result.
    notices: Vec<String>,
    /// Approval batches that came back `queued`: batch id -> label.
    queued: HashMap<String, String>,
    /// Name given on the command line (`--name`), used when `initialize`
    /// carries none.
    fallback_name: String,
}

/// A write attempt's failure.
pub(crate) enum Fail {
    /// `stale_rev`, with the current rev.
    Stale(u64),
    Rpc(RpcError),
    Other(String),
}

impl From<RpcError> for Fail {
    fn from(e: RpcError) -> Self {
        if e.code == code::STALE_REV {
            let current = e
                .data
                .as_ref()
                .and_then(|d| d["current"].as_u64())
                .unwrap_or(0);
            Fail::Stale(current)
        } else {
            Fail::Rpc(e)
        }
    }
}

impl From<String> for Fail {
    fn from(e: String) -> Self {
        Fail::Other(e)
    }
}

/// The agent-readable text of an API error.
pub(crate) fn rpc_message(e: &RpcError) -> String {
    match e.code {
        code::REJECTED => format!("the user rejected this edit in the editor: {}", e.message),
        // edit_definitions, and the note tools touching a locked note.
        code::NEEDS_CONFIRM => {
            let reason = e
                .data
                .as_ref()
                .and_then(|d| d["reason"].as_str())
                .unwrap_or(&e.message);
            format!(
                "nothing was changed: this needs \"confirm\": true ({reason}). Call again with \"confirm\": true if that is what you want; the user can undo it"
            )
        }
        _ => e.message.clone(),
    }
}

/// Lowercase, keep `a-z 0-9 - _ .`, cut to 32 characters; `None` when
/// nothing is left.
pub fn sanitize_name(raw: &str) -> Option<String> {
    let s: String = raw
        .to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.'))
        .take(32)
        .collect();
    (!s.is_empty()).then_some(s)
}

/// Parse a tool's arguments.
pub(crate) fn args<T: DeserializeOwned>(tool: &str, v: Value) -> Result<T, String> {
    serde_json::from_value(v).map_err(|e| format!("invalid arguments for {tool}: {e}"))
}

/// One text block holding `v` as two-space indented JSON.
pub(crate) fn json_block(v: &Value) -> ContentBlock {
    ContentBlock::text(pretty(v))
}

/// Keys that lead an object, in this order; the rest follow sorted.
const KEY_ORDER: &[&str] = &[
    "applied",
    "status",
    "rev",
    "id",
    "undo_label",
    "mode",
    "saved",
    "batch_id",
    "message",
    "slug",
    "name",
    "kind",
    "type",
    "level",
    "layer",
    "world",
];

/// Two-space indented JSON for an agent to read: a value whose one-line
/// form is short (a rect, a cell list, a small change) stays on one line,
/// and result keys come in a reading order (`applied`, `rev`, ...) rather
/// than sorted.
pub fn pretty(v: &Value) -> String {
    let mut out = String::new();
    write_pretty(&mut out, v, 0);
    out
}

/// The one-line form: `{ "a": 1, "b": [1, 2] }`.
fn one_line(v: &Value) -> String {
    match v {
        Value::Array(a) if a.is_empty() => "[]".into(),
        Value::Array(a) => format!(
            "[{}]",
            a.iter().map(one_line).collect::<Vec<_>>().join(", ")
        ),
        Value::Object(o) if o.is_empty() => "{}".into(),
        Value::Object(o) => format!(
            "{{ {} }}",
            ordered(o)
                .map(|(k, v)| format!("{}: {}", Value::String(k.clone()), one_line(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        other => other.to_string(),
    }
}

fn ordered(o: &serde_json::Map<String, Value>) -> impl Iterator<Item = (&String, &Value)> {
    let mut keys: Vec<(&String, &Value)> = o.iter().collect();
    keys.sort_by_key(|(k, _)| {
        (
            KEY_ORDER
                .iter()
                .position(|x| x == k)
                .unwrap_or(KEY_ORDER.len()),
            (*k).clone(),
        )
    });
    keys.into_iter()
}

fn write_pretty(out: &mut String, v: &Value, indent: usize) {
    let line = one_line(v);
    if line.len() + indent <= 100 || !matches!(v, Value::Array(_) | Value::Object(_)) {
        out.push_str(&line);
        return;
    }
    let pad = "  ".repeat(indent + 1);
    match v {
        Value::Array(a) => {
            out.push_str("[\n");
            for (i, x) in a.iter().enumerate() {
                out.push_str(&pad);
                write_pretty(out, x, indent + 1);
                out.push_str(if i + 1 < a.len() { ",\n" } else { "\n" });
            }
            out.push_str(&"  ".repeat(indent));
            out.push(']');
        }
        Value::Object(o) => {
            out.push_str("{\n");
            let n = o.len();
            for (i, (k, x)) in ordered(o).enumerate() {
                out.push_str(&pad);
                out.push_str(&Value::String(k.clone()).to_string());
                out.push_str(": ");
                write_pretty(out, x, indent + 1);
                out.push_str(if i + 1 < n { ",\n" } else { "\n" });
            }
            out.push_str(&"  ".repeat(indent));
            out.push('}');
        }
        _ => unreachable!(),
    }
}

/// `[a, b, c]` with spaces, as the docs write arrays.
pub(crate) fn compact(v: &Value) -> String {
    match v {
        Value::Array(a) => format!("[{}]", a.iter().map(compact).collect::<Vec<_>>().join(", ")),
        other => other.to_string(),
    }
}

impl Engine {
    pub fn new(config: Config) -> Self {
        Engine {
            conn: Connector::new(config.dir, config.force_headless, config.name.clone()),
            rev: 0,
            last_read: HashMap::new(),
            foreign: Vec::new(),
            notices: Vec::new(),
            queued: HashMap::new(),
            fallback_name: config.name,
        }
    }

    /// Name the client after the MCP client's `clientInfo.name`
    /// (sanitized); falls back to the configured name when nothing is left.
    pub fn set_client_name(&mut self, raw: &str) {
        let name = sanitize_name(raw).unwrap_or_else(|| self.fallback_name.clone());
        self.conn.set_name(name);
    }

    /// Pretend the newest rev seen is `rev` (tests: force a stale write).
    #[doc(hidden)]
    pub fn set_seen_rev(&mut self, rev: u64) {
        self.rev = rev;
    }

    /// The client name in use.
    pub fn client_name(&self) -> &str {
        self.conn.name()
    }

    /// The mode of the open backend (opening one if needed).
    pub fn mode(&mut self) -> Result<Mode, String> {
        self.prepare()?;
        Ok(self.backend_mode())
    }

    fn backend_mode(&mut self) -> Mode {
        self.conn
            .current()
            .map(|b| b.mode())
            .unwrap_or(Mode::Headless)
    }

    /// Run one tool. Errors become `isError` results with the message.
    pub fn call_tool(&mut self, name: &str, arguments: Value) -> CallToolResult {
        let arguments = if arguments.is_null() {
            json!({})
        } else {
            arguments
        };
        let r = self.prepare().and_then(|()| self.dispatch(name, arguments));
        match r {
            Ok(content) => CallToolResult::success(content),
            Err(msg) => CallToolResult::error(vec![ContentBlock::text(msg)]),
        }
    }

    fn dispatch(&mut self, name: &str, a: Value) -> ToolResult {
        match name {
            "describe_project" => self.describe_project(a),
            "read_level" => self.read_level(a),
            "create_level" => self.create_level(a),
            "paint" => self.paint(a),
            "place_tiles" => self.place_tiles(a),
            "clear_tiles" => self.clear_tiles(a),
            "place_entity" => self.place_entity(a),
            "update_entity" => self.update_entity(a),
            "remove_entity" => self.remove_entity(a),
            "find_entities" => self.find_entities(a),
            "add_note" => self.add_note(a),
            "list_notes" => self.list_notes(a),
            "resolve_note" => self.resolve_note(a),
            "update_note" => self.update_note(a),
            "remove_note" => self.remove_note(a),
            "set_guide_group" => self.set_guide_group(a),
            "tag_levels" => self.tag_levels(a),
            "edit_rules" => self.edit_rules(a),
            "edit_definitions" => self.edit_definitions(a),
            "preview_rule" => self.preview_rule(a),
            "find_regions" => self.find_regions(a),
            "expand_rules" => self.expand_rules(a),
            "bake_and_validate" => self.bake_and_validate(a),
            "screenshot" => self.screenshot(a),
            "undo" => self.undo(a),
            "copy_fragment" => self.copy_fragment(a),
            "paste_fragment" => self.paste_fragment(a),
            "duplicate_level" => self.duplicate_level(a),
            other => Err(format!(
                "there is no tool {other:?}; the tools are {}",
                crate::catalog::TOOLS
                    .iter()
                    .map(|t| t.name)
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }

    /// Open or switch the backend before a call (`docs/mcp.md`: "a switch
    /// between modes happens between calls").
    pub(crate) fn prepare(&mut self) -> Result<(), String> {
        let had = self
            .conn
            .current()
            .map(|b| (b.mode(), b.session().to_string()));
        let sw = self.conn.ensure()?;
        self.notices.extend(sw.notices);
        let now = self
            .conn
            .current()
            .map(|b| (b.mode(), b.session().to_string()));
        if had != now {
            // A new connection: revs start over, earlier reads say nothing.
            self.rev = 0;
            self.last_read.clear();
            self.foreign.clear();
            self.rpc("level.list", json!({}))
                .map_err(|e| rpc_message(&e))?;
        }
        Ok(())
    }

    /// Call the API, keeping the rev and events up to date. A broken
    /// connection drops the backend so the next call reconnects.
    pub(crate) fn rpc(&mut self, method: &str, params: Value) -> Result<Value, RpcError> {
        let Some(b) = self.conn.current() else {
            return Err(crate::backend::transport_error("no project is open"));
        };
        let r = b.call(method, params);
        let events = b.take_events();
        if let Err(e) = &r
            && e.code == code::BUSY
            && e.data.is_none()
            && b.mode() == Mode::Editor
        {
            self.conn.drop_backend();
        }
        self.absorb(events);
        match &r {
            Ok(v) => {
                if let Some(rev) = v.get("rev").and_then(Value::as_u64) {
                    self.rev = self.rev.max(rev);
                }
            }
            Err(e) if e.code == code::STALE_REV => {
                if let Some(cur) = e.data.as_ref().and_then(|d| d["current"].as_u64()) {
                    self.rev = cur;
                }
            }
            Err(_) => {}
        }
        r
    }

    /// `rpc` with the error as agent text.
    pub(crate) fn read(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.rpc(method, params).map_err(|e| rpc_message(&e))
    }

    /// Fold notifications into the rev, the foreign-edit log and notices.
    fn absorb(&mut self, events: Vec<Value>) {
        let me = self
            .conn
            .current()
            .map(|b| b.session().to_string())
            .unwrap_or_default();
        let my_name = self.conn.name().to_string();
        for ev in events {
            let p = &ev["params"];
            match ev["method"].as_str() {
                Some("changed") => {
                    let rev = p["rev"].as_u64().unwrap_or(0);
                    self.rev = self.rev.max(rev);
                    if p["origin"].get("agent").is_some() && p["session"].as_str() == Some(&me) {
                        continue;
                    }
                    let who = match p["origin"].get("agent").and_then(Value::as_str) {
                        Some(n) if n == my_name => format!("another {n} session"),
                        Some(n) => n.to_string(),
                        None => "the user".to_string(),
                    };
                    let kind = p["kind"].as_str().unwrap_or("edit");
                    if kind == "reload" {
                        self.notices.push(format!(
                            "{who} reloaded the project from disk; re-read what you rely on"
                        ));
                        self.last_read.clear();
                        continue;
                    }
                    let verb = match kind {
                        "undo" => format!("undid \"{}\" on", p["label"].as_str().unwrap_or("")),
                        "redo" => format!("redid \"{}\" on", p["label"].as_str().unwrap_or("")),
                        _ => "changed".to_string(),
                    };
                    let mut by_level: BTreeMap<String, Vec<String>> = BTreeMap::new();
                    for c in p["changes"].as_array().into_iter().flatten() {
                        for level in change_levels(c) {
                            let what = describe_change(c);
                            let list = by_level.entry(level).or_default();
                            if !list.contains(&what) {
                                list.push(what);
                            }
                        }
                    }
                    for (level, whats) in by_level {
                        self.foreign.push(Foreign {
                            rev,
                            text: format!("{who} {verb} {level} ({})", whats.join(", ")),
                            level,
                        });
                    }
                }
                Some("batch") => {
                    let id = p["batch_id"].as_str().unwrap_or("").to_string();
                    let label = self.queued.remove(&id).unwrap_or_else(|| id.clone());
                    let status = p["status"].as_str().unwrap_or("decided");
                    let mut n =
                        format!("your queued edit \"{label}\" ({id}) was {status} by the user");
                    if status == "failed" {
                        n = format!(
                            "your queued edit \"{label}\" ({id}) was accepted but no longer applied"
                        );
                    }
                    if let Some(e) = p["error"].get("message").and_then(Value::as_str) {
                        n.push_str(&format!(": {e}"));
                    }
                    if let Some(rev) = p["rev"].as_u64() {
                        n.push_str(&format!(" (rev {rev})"));
                    }
                    self.notices.push(n);
                }
                _ => {}
            }
        }
    }

    /// Notices for the agent about `levels` (edits by others since it last
    /// read them), plus the pending ones; marks the levels as read at `rev`.
    pub(crate) fn take_notices(&mut self, levels: &[String], rev: u64, verb: &str) -> Vec<String> {
        let mut out = std::mem::take(&mut self.notices);
        for level in levels {
            if let Some(&since) = self.last_read.get(level) {
                for f in &self.foreign {
                    if &f.level == level && f.rev > since {
                        out.push(format!("{} since your last {verb}", f.text));
                    }
                }
            }
            self.foreign.retain(|f| &f.level != level);
            self.last_read.insert(level.clone(), rev);
        }
        out
    }

    /// Mark a level as read now (read_level): forget what others did to it.
    pub(crate) fn mark_read(&mut self, level: &str, rev: u64) {
        self.foreign.retain(|f| f.level != level);
        self.last_read.insert(level.to_string(), rev);
    }

    /// Pending notices only (for read tools).
    pub(crate) fn pending_notices(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notices)
    }

    /// Run a write tool: `build` turns the (re-)read project into API
    /// writes; they are sent with `expect_rev` as one undo entry labelled
    /// `label`, retried once after a refresh on `stale_rev`. Returns the
    /// write result of `docs/mcp.md` ("Results").
    pub(crate) fn write(
        &mut self,
        label: &str,
        build: impl FnMut(&mut Self) -> Result<Vec<(&'static str, Value)>, String>,
    ) -> Result<Value, String> {
        self.write_full(label, build).map(|(out, _)| out)
    }

    /// [`Engine::write`], also returning the API result of a call made of
    /// one write (`None` for a batch), for tools that report more of it.
    pub(crate) fn write_full(
        &mut self,
        label: &str,
        mut build: impl FnMut(&mut Self) -> Result<Vec<(&'static str, Value)>, String>,
    ) -> Result<(Value, Option<Value>), String> {
        for attempt in 0..2 {
            let writes = build(self)?;
            if writes.is_empty() {
                return Err("nothing to do: the call asks for no change".into());
            }
            let single = writes.len() == 1;
            match self.apply(label, writes) {
                Ok((r, id)) => {
                    let raw = single.then(|| r.clone());
                    return Ok((self.finish(label, r, id), raw));
                }
                Err(Fail::Stale(_)) if attempt == 0 => continue,
                Err(Fail::Stale(cur)) => {
                    return Err(format!(
                        "the project changed twice while this edit was being sent (now at rev {cur}); nothing was applied. Read again and retry"
                    ));
                }
                Err(Fail::Rpc(e)) => return Err(rpc_message(&e)),
                Err(Fail::Other(e)) => return Err(e),
            }
        }
        unreachable!("the loop returns")
    }

    /// Send the writes: one with `label`, or several in a batch. A failure
    /// part-way cancels the batch (`history.cancel_batch`): what applied is
    /// reverted, or in approval mode never submitted, so a call is all or
    /// nothing.
    fn apply(
        &mut self,
        label: &str,
        mut writes: Vec<(&'static str, Value)>,
    ) -> Result<(Value, Option<Value>), Fail> {
        let expect = self.rev;
        if writes.len() == 1 {
            let (m, mut p) = writes.pop().expect("one write");
            p["label"] = json!(label);
            p["expect_rev"] = json!(expect);
            let r = self.rpc(m, p)?;
            let id = r.get("id").cloned();
            return Ok((r, id));
        }
        self.rpc("history.begin_batch", json!({ "label": label }))?;
        let mut id = None;
        for (i, (m, mut p)) in writes.into_iter().enumerate() {
            if i == 0 {
                p["expect_rev"] = json!(expect);
            }
            match self.rpc(m, p) {
                Ok(r) => {
                    if id.is_none() {
                        id = r.get("id").cloned();
                    }
                }
                Err(e) => {
                    // Revert what applied so far (in approval mode: drop
                    // what was collected), so nothing of a failed call is
                    // left behind or submitted for approval.
                    let _ = self.rpc("history.cancel_batch", json!({}));
                    return Err(e.into());
                }
            }
        }
        let end = self.rpc("history.end_batch", json!({}))?;
        Ok((end, id))
    }

    /// The write result: saved (headless), notices, at most 20 changes.
    fn finish(&mut self, label: &str, r: Value, id: Option<Value>) -> Value {
        let mode = self.backend_mode();
        if r["status"] == "queued" {
            let batch = r["batch_id"].as_str().unwrap_or("").to_string();
            self.queued.insert(batch.clone(), label.to_string());
            return json!({ "applied": false, "status": "queued", "batch_id": batch,
                "message": "waiting for the user to accept it in the editor; it is not applied yet" });
        }
        let rev = r["rev"].as_u64().unwrap_or(self.rev);
        let changes = r["changes"].as_array().cloned().unwrap_or_default();
        let mut extra = Vec::new();
        let saved = if mode == Mode::Headless {
            match self.rpc("project.save", json!({})) {
                Ok(_) => true,
                Err(e) => {
                    extra.push(format!(
                        "the edit is applied but saving it to disk failed: {}",
                        e.message
                    ));
                    false
                }
            }
        } else {
            false
        };
        let mut levels: Vec<String> = Vec::new();
        for c in &changes {
            for l in change_levels(c) {
                if !levels.contains(&l) {
                    levels.push(l);
                }
            }
        }
        let mut notices = self.take_notices(&levels, rev, "read_level");
        notices.extend(extra);
        let mut listed: Vec<Value> = changes.iter().take(MAX_CHANGES).cloned().collect();
        if changes.len() > MAX_CHANGES {
            listed.push(json!(format!(
                "... and {} more",
                changes.len() - MAX_CHANGES
            )));
        }
        let mut out = json!({ "applied": true, "rev": rev, "undo_label": r["label"].clone(),
            "mode": mode.as_str(), "saved": saved, "changes": listed });
        if let Some(id) = id {
            out["id"] = id;
        }
        if !notices.is_empty() {
            out["notices"] = json!(notices);
        }
        out
    }

    /// `Agent: <tool> <label or summary>`, the label cut to 60 characters.
    pub(crate) fn label(tool: &str, label: Option<&str>, summary: &str) -> String {
        let s = match label.map(str::trim).filter(|l| !l.is_empty()) {
            Some(l) => l.chars().take(60).collect::<String>(),
            None => summary.to_string(),
        };
        format!("Agent: {tool} {s}")
    }

    /// A JSON read result, with pending notices added when there are any.
    pub(crate) fn json_result(&mut self, mut v: Value) -> ToolResult {
        let notices = self.pending_notices();
        if !notices.is_empty()
            && let Some(o) = v.as_object_mut()
        {
            o.insert("notices".into(), json!(notices));
        }
        Ok(vec![json_block(&v)])
    }
}

/// The levels a change touches.
fn change_levels(c: &Value) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(l) = c["level"].as_str() {
        out.push(l.to_string());
    }
    if c["change"] == "level_renamed"
        && let Some(l) = c["to"].as_str()
    {
        out.push(l.to_string());
    }
    out
}

/// A change in a few words: `collision [10, 1, 4, 1]`, `entity doorcrev22`.
fn describe_change(c: &Value) -> String {
    let kind = c["change"].as_str().unwrap_or("change");
    if let (Some(layer), Some(rect)) = (c["layer"].as_str(), c.get("rect")) {
        return format!("{layer} {}", compact(rect));
    }
    if let Some(id) = c["id"].as_str() {
        return format!("{kind} {id}");
    }
    if let Some(part) = c["part"].as_str() {
        return format!("level {part}");
    }
    kind.replace('_', " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_sanitized() {
        assert_eq!(sanitize_name("Claude Code").as_deref(), Some("claudecode"));
        assert_eq!(sanitize_name("claude-code").as_deref(), Some("claude-code"));
        assert_eq!(sanitize_name("!!!"), None);
        assert_eq!(sanitize_name(&"a".repeat(40)).unwrap().len(), 32);
    }

    #[test]
    fn pretty_keeps_short_values_on_one_line() {
        let v = json!({ "rev": 3, "applied": true, "changes": [ { "change": "grid", "rect": [1, 2, 3, 4] } ],
            "long": ["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"] });
        let p = pretty(&v);
        assert_eq!(
            p,
            "{\n  \"applied\": true,\n  \"rev\": 3,\n  \"changes\": [{ \"change\": \"grid\", \"rect\": [1, 2, 3, 4] }],\n  \"long\": [\n    \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\",\n    \"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\"\n  ]\n}"
        );
        assert_eq!(serde_json::from_str::<Value>(&p).unwrap(), v);
    }

    #[test]
    fn labels() {
        assert_eq!(
            Engine::label("paint", Some("east wall"), "collision in crevasse"),
            "Agent: paint east wall"
        );
        assert_eq!(
            Engine::label("paint", None, "collision in crevasse"),
            "Agent: paint collision in crevasse"
        );
        assert_eq!(
            Engine::label("paint", Some(&"x".repeat(80)), "s").len(),
            "Agent: paint ".len() + 60
        );
    }

    #[test]
    fn changes_in_words() {
        let c = json!({ "change": "grid", "world": "main", "level": "crevasse", "layer": "collision", "rect": [10, 1, 4, 1] });
        assert_eq!(describe_change(&c), "collision [10, 1, 4, 1]");
        assert_eq!(change_levels(&c), vec!["crevasse".to_string()]);
        let c = json!({ "change": "entity", "world": "main", "level": "wing", "id": "wingdoor22" });
        assert_eq!(describe_change(&c), "entity wingdoor22");
    }
}
