//! Minimal JSON value, parser, and writer (std only).
//!
//! Three users, one representation: agent-transcript adapters (foreign JSONL), the
//! authored-event payload codec (argus's own on-disk format), and the API wire.
//! Numbers keep their source lexeme so foreign values (token counts, ids) never lose
//! precision on a round trip. Objects keep key order, so encoding is deterministic.

#![allow(dead_code, unused_variables)]

#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(Number),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

/// A JSON number kept as its lexeme, e.g. `"12"`, `"-1.5e3"`.
#[derive(Clone, Debug, PartialEq)]
pub struct Number(String);

impl Number {
    pub fn from_i64(n: i64) -> Self {
        Number(n.to_string())
    }
    pub fn from_u64(n: u64) -> Self {
        Number(n.to_string())
    }
    pub fn as_i64(&self) -> Option<i64> {
        self.0.parse().ok()
    }
    pub fn as_u64(&self) -> Option<u64> {
        self.0.parse().ok()
    }
    pub fn as_f64(&self) -> Option<f64> {
        self.0.parse().ok()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct JsonError {
    pub at: usize,
    pub what: String,
}

pub fn parse(bytes: &[u8]) -> Result<Json, JsonError> {
    // TODO: recursive descent; depth limit 256; reject trailing bytes except whitespace.
    todo!("not implemented")
}

/// Compact form, RFC 8259 escaping. Object keys keep their order, so output is deterministic.
pub fn write(value: &Json) -> String {
    todo!("not implemented")
}

impl Json {
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn str(s: impl Into<String>) -> Json {
        Json::Str(s.into())
    }
}
