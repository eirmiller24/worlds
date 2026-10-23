//! Deterministic pretty printer for project files.
//!
//! serde_json's pretty printer puts every array element on its own line, so
//! `"at": [4, 2]` takes four lines. This re-indents serde_json's compact
//! output (keeping key order) with two rules:
//!
//! - an array or object below the top level goes on one line if it fits in
//!   [`WIDTH`] columns (counting the trailing comma) and holds no array of objects;
//! - a `"rows"` array is always one entry per line, so grids read as pictures.

const WIDTH: usize = 100;
const INDENT: usize = 2;

enum Node<'a> {
    Scalar(&'a str),
    Array(Vec<Node<'a>>),
    Object(Vec<(&'a str, Node<'a>)>),
}

/// Re-indent compact JSON text produced by serde_json. Ends with a newline.
pub(crate) fn pretty(compact: &str) -> String {
    let mut p = Parser {
        s: compact.as_bytes(),
        src: compact,
        i: 0,
    };
    let node = p.value();
    let mut out = String::with_capacity(compact.len() * 2);
    write_node(&node, None, 0, 0, &mut out);
    out.push('\n');
    out
}

struct Parser<'a> {
    s: &'a [u8],
    src: &'a str,
    i: usize,
}

impl<'a> Parser<'a> {
    fn skip_ws(&mut self) {
        while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }

    fn value(&mut self) -> Node<'a> {
        self.skip_ws();
        match self.s[self.i] {
            b'{' => {
                self.i += 1;
                let mut entries = Vec::new();
                loop {
                    self.skip_ws();
                    if self.s[self.i] == b'}' {
                        self.i += 1;
                        break;
                    }
                    let key = self.string();
                    self.skip_ws();
                    debug_assert_eq!(self.s[self.i], b':');
                    self.i += 1;
                    let v = self.value();
                    entries.push((key, v));
                    self.skip_ws();
                    if self.s[self.i] == b',' {
                        self.i += 1;
                    }
                }
                Node::Object(entries)
            }
            b'[' => {
                self.i += 1;
                let mut items = Vec::new();
                loop {
                    self.skip_ws();
                    if self.s[self.i] == b']' {
                        self.i += 1;
                        break;
                    }
                    items.push(self.value());
                    self.skip_ws();
                    if self.s[self.i] == b',' {
                        self.i += 1;
                    }
                }
                Node::Array(items)
            }
            b'"' => Node::Scalar(self.string()),
            _ => {
                let start = self.i;
                while self.i < self.s.len()
                    && !matches!(self.s[self.i], b',' | b']' | b'}')
                    && !self.s[self.i].is_ascii_whitespace()
                {
                    self.i += 1;
                }
                Node::Scalar(&self.src[start..self.i])
            }
        }
    }

    /// A string literal including its quotes.
    fn string(&mut self) -> &'a str {
        let start = self.i;
        self.i += 1;
        while self.s[self.i] != b'"' {
            if self.s[self.i] == b'\\' {
                self.i += 1;
            }
            self.i += 1;
        }
        self.i += 1;
        &self.src[start..self.i]
    }
}

/// The one-line rendering of a node, if it is allowed on one line at all.
fn inline(node: &Node, key: Option<&str>) -> Option<String> {
    match node {
        Node::Scalar(s) => Some(s.to_string()),
        Node::Array(items) if items.is_empty() => Some("[]".into()),
        Node::Array(items) => {
            if key == Some("\"rows\"") || items.iter().any(|n| matches!(n, Node::Object(_))) {
                return None;
            }
            let parts: Option<Vec<String>> = items.iter().map(|n| inline(n, None)).collect();
            Some(format!("[{}]", parts?.join(", ")))
        }
        Node::Object(entries) if entries.is_empty() => Some("{}".into()),
        Node::Object(entries) => {
            let parts: Option<Vec<String>> = entries
                .iter()
                .map(|(k, v)| inline(v, Some(k)).map(|v| format!("{k}: {v}")))
                .collect();
            Some(format!("{{ {} }}", parts?.join(", ")))
        }
    }
}

fn write_node(node: &Node, key: Option<&str>, indent: usize, depth: usize, out: &mut String) {
    let prefix = key.map_or(0, |k| k.len() + 2);
    if depth > 0
        && let Some(line) = inline(node, key)
        && (matches!(node, Node::Scalar(_)) || indent + prefix + line.len() < WIDTH)
    {
        out.push_str(&line);
        return;
    }
    let pad = |out: &mut String, n: usize| out.extend(std::iter::repeat_n(' ', n));
    match node {
        Node::Scalar(s) => out.push_str(s),
        Node::Array(items) if items.is_empty() => out.push_str("[]"),
        Node::Object(entries) if entries.is_empty() => out.push_str("{}"),
        Node::Array(items) => {
            out.push_str("[\n");
            for (i, item) in items.iter().enumerate() {
                pad(out, indent + INDENT);
                write_node(item, None, indent + INDENT, depth + 1, out);
                out.push_str(if i + 1 < items.len() { ",\n" } else { "\n" });
            }
            pad(out, indent);
            out.push(']');
        }
        Node::Object(entries) => {
            out.push_str("{\n");
            for (i, (k, v)) in entries.iter().enumerate() {
                pad(out, indent + INDENT);
                out.push_str(k);
                out.push_str(": ");
                write_node(v, Some(k), indent + INDENT, depth + 1, out);
                out.push_str(if i + 1 < entries.len() { ",\n" } else { "\n" });
            }
            pad(out, indent);
            out.push('}');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::pretty;

    #[test]
    fn layout() {
        let src = r###"{"$schema":"x","tiles":[{"at":[4,2],"tileset":"a","tile":[5,1]}],"rows":["##","#."],"legend":{".":null,"#":"solid"},"s":"a,\"]}"}"###;
        let expected = r###"{
  "$schema": "x",
  "tiles": [
    { "at": [4, 2], "tileset": "a", "tile": [5, 1] }
  ],
  "rows": [
    "##",
    "#."
  ],
  "legend": { ".": null, "#": "solid" },
  "s": "a,\"]}"
}
"###;
        assert_eq!(pretty(src), expected);
        // Output parses back to the same value.
        let a: serde_json::Value = serde_json::from_str(src).unwrap();
        let b: serde_json::Value = serde_json::from_str(&pretty(src)).unwrap();
        assert_eq!(a, b);
    }
}
