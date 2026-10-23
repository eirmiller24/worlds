//! JSON-RPC 2.0 framing and the error codes of `docs/api.md` ("Errors").
//!
//! Hand-rolled: a message is parsed into [`Incoming`], a reply is built with
//! [`response`], an event with [`notification`]. Every error is an
//! [`RpcError`] whose `message` is a sentence meant for an agent to read.

use std::fmt;

use serde_json::{Map, Value, json};
use worlds_core::CommandError;

/// The error codes. Names follow `docs/api.md`.
pub mod code {
    /// Not JSON.
    pub const PARSE_ERROR: i64 = -32700;
    /// Not a JSON-RPC request object (or a batch array).
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    /// Wrong shape, unknown key, missing key; `data.message` says which.
    pub const INVALID_PARAMS: i64 = -32602;
    /// A bug; `data.message`.
    pub const INTERNAL: i64 = -32603;
    /// No or bad `session.hello`.
    pub const UNAUTHORIZED: i64 = -32000;
    /// `expect_rev` isn't the current rev; `data: { expected, current }`.
    pub const STALE_REV: i64 = -32001;
    /// A destructive method without `confirm: true`; `data: { reason }`.
    pub const NEEDS_CONFIRM: i64 = -32002;
    /// The user rejected the batch in approval mode.
    pub const REJECTED: i64 = -32003;
    /// The command was refused; `data` is the `CommandError`.
    pub const COMMAND_ERROR: i64 = -32004;
    /// Not available here (`view.*` headless, reload in approval mode, ...).
    pub const UNSUPPORTED: i64 = -32005;
    /// The project is closing or loading; retry.
    pub const BUSY: i64 = -32006;
    /// `end_batch` with no open batch, `begin_batch` with one open.
    pub const BATCH_STATE: i64 = -32007;
    /// Saving or exporting failed; `data: { path, message }`.
    pub const IO: i64 = -32008;
}

/// A JSON-RPC error object.
#[derive(Debug, Clone, PartialEq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

impl RpcError {
    pub fn new(code: i64, message: impl Into<String>, data: Option<Value>) -> Self {
        RpcError {
            code,
            message: message.into(),
            data,
        }
    }

    /// `-32602`, with the same text in `data.message`.
    pub fn invalid_params(message: impl Into<String>) -> Self {
        let message = message.into();
        let data = json!({ "message": message });
        RpcError::new(code::INVALID_PARAMS, message, Some(data))
    }

    /// `-32603`: a bug.
    pub fn internal(message: impl Into<String>) -> Self {
        let message = message.into();
        let data = json!({ "message": message });
        RpcError::new(code::INTERNAL, message, Some(data))
    }

    pub fn method_not_found(method: &str) -> Self {
        RpcError::new(code::METHOD_NOT_FOUND, format!("no method {method}"), None)
    }

    pub fn invalid_request(message: impl Into<String>) -> Self {
        RpcError::new(code::INVALID_REQUEST, message, None)
    }

    pub fn unauthorized(message: impl Into<String>) -> Self {
        RpcError::new(code::UNAUTHORIZED, message, None)
    }

    pub fn stale_rev(expected: u64, current: u64) -> Self {
        RpcError::new(
            code::STALE_REV,
            format!("the project is at rev {current}, not {expected}: re-read before writing"),
            Some(json!({ "expected": expected, "current": current })),
        )
    }

    /// `-32002`. `reason` goes in `data.reason`; the message is `message`.
    pub fn needs_confirm(reason: impl Into<String>, message: impl Into<String>) -> Self {
        RpcError::new(
            code::NEEDS_CONFIRM,
            message,
            Some(json!({ "reason": reason.into() })),
        )
    }

    pub fn rejected(message: impl Into<String>) -> Self {
        RpcError::new(code::REJECTED, message, None)
    }

    /// `-32004`: `data` is the error as it serializes, the message its text.
    pub fn command(e: &CommandError) -> Self {
        let data = serde_json::to_value(e).unwrap_or_else(|_| json!({ "kind": "invalid" }));
        RpcError::new(code::COMMAND_ERROR, e.to_string(), Some(data))
    }

    /// `-32004` with kind `invalid`.
    pub fn invalid(message: impl Into<String>) -> Self {
        RpcError::command(&CommandError::Invalid(message.into()))
    }

    pub fn unsupported(message: impl Into<String>) -> Self {
        RpcError::new(code::UNSUPPORTED, message, None)
    }

    pub fn busy(message: impl Into<String>) -> Self {
        RpcError::new(code::BUSY, message, None)
    }

    pub fn batch_state(message: impl Into<String>) -> Self {
        RpcError::new(code::BATCH_STATE, message, None)
    }

    /// `-32008` with `data: { path, message }`.
    pub fn io(path: impl Into<String>, message: impl Into<String>) -> Self {
        let (path, message) = (path.into(), message.into());
        RpcError::new(
            code::IO,
            format!("{path}: {message}"),
            Some(json!({ "path": path, "message": message })),
        )
    }

    /// From a load/save error of worlds-core: `io` naming the file.
    pub fn from_format(e: &worlds_core::Error) -> Self {
        let path = match e {
            worlds_core::Error::Io { path, .. }
            | worlds_core::Error::Parse { path, .. }
            | worlds_core::Error::Layout { path, .. } => path.to_string_lossy().into_owned(),
        };
        // The message without the path (`io` puts it in front once).
        let message = match e {
            worlds_core::Error::Io { source, .. } => source.to_string(),
            worlds_core::Error::Parse {
                json_path, message, ..
            } if !json_path.is_empty() => format!("at {json_path}: {message}"),
            worlds_core::Error::Parse { message, .. }
            | worlds_core::Error::Layout { message, .. } => message.clone(),
        };
        RpcError::io(path, message)
    }

    /// The JSON-RPC error object.
    pub fn to_value(&self) -> Value {
        let mut m = Map::new();
        m.insert("code".into(), self.code.into());
        m.insert("message".into(), self.message.clone().into());
        if let Some(d) = &self.data {
            m.insert("data".into(), d.clone());
        }
        Value::Object(m)
    }

    /// Parse an error object back (clients and tests).
    pub fn from_value(v: &Value) -> Option<Self> {
        Some(RpcError {
            code: v.get("code")?.as_i64()?,
            message: v.get("message")?.as_str()?.to_string(),
            data: v.get("data").cloned(),
        })
    }
}

impl fmt::Display for RpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.message, self.code)
    }
}

impl std::error::Error for RpcError {}

impl From<CommandError> for RpcError {
    fn from(e: CommandError) -> Self {
        RpcError::command(&e)
    }
}

/// A response to request `id`.
pub fn response(id: &Value, result: Result<Value, RpcError>) -> Value {
    match result {
        Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
        Err(e) => json!({ "jsonrpc": "2.0", "id": id, "error": e.to_value() }),
    }
}

/// A server-to-client notification.
pub fn notification(method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "method": method, "params": params })
}

/// One inbound message, parsed.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    /// A request to answer.
    Request {
        id: Value,
        method: String,
        /// Always an object (`{}` when the request had none).
        params: Value,
    },
    /// A client notification (no `id`): ignored.
    Notification,
    /// Not a valid request: send this error response.
    Invalid(Value),
}

/// Parse one text frame.
pub fn parse_message(text: &str) -> Incoming {
    let v: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => {
            return Incoming::Invalid(response(
                &Value::Null,
                Err(RpcError::new(
                    code::PARSE_ERROR,
                    format!("the message is not JSON: {e}"),
                    None,
                )),
            ));
        }
    };
    let invalid = |id: &Value, msg: &str| {
        Incoming::Invalid(response(id, Err(RpcError::invalid_request(msg))))
    };
    let obj = match &v {
        Value::Array(_) => {
            return invalid(
                &Value::Null,
                "batch arrays are not supported: send one request per frame and use history.begin_batch to group edits",
            );
        }
        Value::Object(o) => o,
        _ => return invalid(&Value::Null, "a request must be a JSON object"),
    };
    let id = match obj.get("id") {
        None => return Incoming::Notification,
        Some(id @ (Value::Number(_) | Value::String(_))) => id.clone(),
        Some(_) => return invalid(&Value::Null, "the request id must be a number or a string"),
    };
    if obj.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return invalid(&id, "the request needs \"jsonrpc\": \"2.0\"");
    }
    let Some(method) = obj.get("method").and_then(Value::as_str) else {
        return invalid(&id, "the request needs a \"method\" string");
    };
    let params = match obj.get("params") {
        None | Some(Value::Null) => Value::Object(Map::new()),
        Some(p @ Value::Object(_)) => p.clone(),
        Some(_) => {
            return Incoming::Invalid(response(
                &id,
                Err(RpcError::invalid_params(
                    "params must be an object of named parameters",
                )),
            ));
        }
    };
    Incoming::Request {
        id,
        method: method.to_string(),
        params,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err_code(i: Incoming) -> (i64, Value) {
        match i {
            Incoming::Invalid(v) => (v["error"]["code"].as_i64().unwrap(), v["id"].clone()),
            other => panic!("{other:?}"),
        }
    }

    /// The file is named once, in front, for every kind of format error.
    #[test]
    fn format_errors_name_the_path_once() {
        use std::path::PathBuf;
        let path = PathBuf::from("levels/x/level.json");
        let cases = [
            (
                worlds_core::Error::Layout {
                    path: path.clone(),
                    message: "duplicate slug".into(),
                },
                "levels/x/level.json: duplicate slug",
            ),
            (
                worlds_core::Error::Parse {
                    path: path.clone(),
                    json_path: "rows[2]".into(),
                    message: "bad glyph".into(),
                },
                "levels/x/level.json: at rows[2]: bad glyph",
            ),
            (
                worlds_core::Error::Parse {
                    path: path.clone(),
                    json_path: String::new(),
                    message: "not JSON".into(),
                },
                "levels/x/level.json: not JSON",
            ),
            (
                worlds_core::Error::Io {
                    path: path.clone(),
                    source: std::io::Error::other("disk full"),
                },
                "levels/x/level.json: disk full",
            ),
        ];
        for (e, want) in cases {
            let r = RpcError::from_format(&e);
            assert_eq!(r.message, want);
            assert_eq!(r.data.as_ref().unwrap()["path"], "levels/x/level.json");
        }
    }

    #[test]
    fn framing() {
        assert_eq!(err_code(parse_message("{")).0, code::PARSE_ERROR);
        assert_eq!(
            err_code(parse_message("[]")),
            (code::INVALID_REQUEST, Value::Null)
        );
        assert_eq!(
            err_code(parse_message(r#"{"jsonrpc":"2.0","id":3}"#)),
            (code::INVALID_REQUEST, json!(3))
        );
        assert_eq!(
            err_code(parse_message(
                r#"{"jsonrpc":"2.0","id":3,"method":"x","params":[1]}"#
            ))
            .0,
            code::INVALID_PARAMS
        );
        assert_eq!(
            parse_message(r#"{"jsonrpc":"2.0","method":"x"}"#),
            Incoming::Notification
        );
        assert_eq!(
            parse_message(r#"{"jsonrpc":"2.0","id":"a","method":"level.list"}"#),
            Incoming::Request {
                id: json!("a"),
                method: "level.list".into(),
                params: json!({})
            }
        );
    }

    #[test]
    fn command_errors_carry_the_wire_form() {
        let e = RpcError::command(&CommandError::UnknownLevel(worlds_core::slug("annex")));
        assert_eq!(e.code, code::COMMAND_ERROR);
        assert_eq!(e.message, "no level \"annex\"");
        assert_eq!(e.data.unwrap()["kind"], "unknown_level");
    }
}
