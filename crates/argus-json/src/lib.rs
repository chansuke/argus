//! Minimal JSON value, parser, and writer (std only).
//!
//! Three users, one representation: agent-transcript adapters (foreign JSONL), the
//! authored-event payload codec (argus's own on-disk format), and the API wire.
//! Numbers keep their source lexeme so foreign values (token counts, ids) never lose
//! precision on a round trip. Objects keep key order, so encoding is deterministic.

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
    /// `None` for NaN and the infinities, which JSON cannot write. `Display` gives the
    /// shortest digits that read back as the same float, without an exponent.
    pub fn from_f64(x: f64) -> Option<Self> {
        x.is_finite().then(|| Number(x.to_string()))
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

/// Strict RFC 8259 parse of one value. Lone surrogate escapes, which JavaScript strings
/// may hold and Rust strings cannot, become U+FFFD.
pub fn parse(bytes: &[u8]) -> Result<Json, JsonError> {
    let mut p = Parser { bytes, at: 0 };
    p.skip_ws();
    let value = p.value(0)?;
    p.skip_ws();
    if p.at != bytes.len() {
        return Err(p.error("trailing bytes after the value"));
    }
    Ok(value)
}

/// Compact form with `JSON.stringify`'s escaping (only `"`, `\` and control characters).
/// Object keys keep their order, so output is deterministic.
pub fn write(value: &Json) -> String {
    let mut out = String::new();
    write_into(value, &mut out);
    out
}

const MAX_DEPTH: usize = 256;

struct Parser<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Parser<'_> {
    fn error(&self, what: &str) -> JsonError {
        JsonError {
            at: self.at,
            what: what.to_owned(),
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.at += 1;
        }
    }

    fn expect(&mut self, b: u8) -> Result<(), JsonError> {
        if self.peek() == Some(b) {
            self.at += 1;
            Ok(())
        } else {
            Err(self.error(&format!("expected `{}`", b as char)))
        }
    }

    fn literal(&mut self, word: &str, value: Json) -> Result<Json, JsonError> {
        if self.bytes[self.at..].starts_with(word.as_bytes()) {
            self.at += word.len();
            Ok(value)
        } else {
            Err(self.error("invalid literal"))
        }
    }

    /// `depth` counts the arrays and objects around this value.
    fn value(&mut self, depth: usize) -> Result<Json, JsonError> {
        match self.peek() {
            Some(b'n') => self.literal("null", Json::Null),
            Some(b't') => self.literal("true", Json::Bool(true)),
            Some(b'f') => self.literal("false", Json::Bool(false)),
            Some(b'"') => self.string().map(Json::Str),
            Some(b'-' | b'0'..=b'9') => self.number().map(Json::Num),
            Some(b'[') => {
                let depth = self.enter(depth)?;
                let mut items = Vec::new();
                if !self.close(b']') {
                    loop {
                        self.skip_ws();
                        items.push(self.value(depth)?);
                        self.skip_ws();
                        if self.close(b']') {
                            break;
                        }
                        self.expect(b',')?;
                    }
                }
                Ok(Json::Arr(items))
            }
            Some(b'{') => {
                let depth = self.enter(depth)?;
                let mut fields = Vec::new();
                if !self.close(b'}') {
                    loop {
                        self.skip_ws();
                        if self.peek() != Some(b'"') {
                            return Err(self.error("expected a string key"));
                        }
                        let key = self.string()?;
                        self.skip_ws();
                        self.expect(b':')?;
                        self.skip_ws();
                        fields.push((key, self.value(depth)?));
                        self.skip_ws();
                        if self.close(b'}') {
                            break;
                        }
                        self.expect(b',')?;
                    }
                }
                Ok(Json::Obj(fields))
            }
            _ => Err(self.error("expected a value")),
        }
    }

    fn enter(&mut self, depth: usize) -> Result<usize, JsonError> {
        if depth >= MAX_DEPTH {
            return Err(self.error("nested too deeply"));
        }
        self.at += 1;
        Ok(depth + 1)
    }

    /// Consumes `b` (after whitespace) if it comes next.
    fn close(&mut self, b: u8) -> bool {
        self.skip_ws();
        if self.peek() == Some(b) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn number(&mut self) -> Result<Number, JsonError> {
        let start = self.at;
        let digits = |p: &mut Self| {
            let from = p.at;
            while matches!(p.peek(), Some(b'0'..=b'9')) {
                p.at += 1;
            }
            p.at > from
        };
        if self.peek() == Some(b'-') {
            self.at += 1;
        }
        match self.peek() {
            Some(b'0') => self.at += 1,
            Some(b'1'..=b'9') => {
                digits(self);
            }
            _ => return Err(self.error("expected a digit")),
        }
        if self.peek() == Some(b'.') {
            self.at += 1;
            if !digits(self) {
                return Err(self.error("expected a digit after `.`"));
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.at += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.at += 1;
            }
            if !digits(self) {
                return Err(self.error("expected an exponent"));
            }
        }
        // Only ASCII digits and signs were consumed.
        let lexeme = std::str::from_utf8(&self.bytes[start..self.at]).unwrap();
        Ok(Number(lexeme.to_owned()))
    }

    fn string(&mut self) -> Result<String, JsonError> {
        self.at += 1;
        let mut out = String::new();
        loop {
            let run = self.at;
            while matches!(self.peek(), Some(b) if b != b'"' && b != b'\\' && b >= 0x20) {
                self.at += 1;
            }
            match std::str::from_utf8(&self.bytes[run..self.at]) {
                Ok(text) => out.push_str(text),
                Err(e) => {
                    self.at = run + e.valid_up_to();
                    return Err(self.error("invalid UTF-8"));
                }
            }
            match self.peek() {
                Some(b'"') => {
                    self.at += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.at += 1;
                    self.escape(&mut out)?;
                }
                Some(_) => return Err(self.error("control character in a string")),
                None => return Err(self.error("unterminated string")),
            }
        }
    }

    fn escape(&mut self, out: &mut String) -> Result<(), JsonError> {
        let c = match self.peek() {
            Some(b'"') => '"',
            Some(b'\\') => '\\',
            Some(b'/') => '/',
            Some(b'b') => '\u{8}',
            Some(b'f') => '\u{c}',
            Some(b'n') => '\n',
            Some(b'r') => '\r',
            Some(b't') => '\t',
            Some(b'u') => {
                self.at += 1;
                let unit = self.hex4()?;
                let c = match unit {
                    0xD800..=0xDBFF if self.bytes[self.at..].starts_with(b"\\u") => {
                        let save = self.at;
                        self.at += 2;
                        let low = self.hex4()?;
                        if (0xDC00..=0xDFFF).contains(&low) {
                            let code = 0x10000 + ((unit - 0xD800) << 10) + (low - 0xDC00);
                            char::from_u32(code).unwrap()
                        } else {
                            // Not a pair: the second escape is read on its own.
                            self.at = save;
                            '\u{fffd}'
                        }
                    }
                    _ => char::from_u32(unit).unwrap_or('\u{fffd}'),
                };
                out.push(c);
                return Ok(());
            }
            _ => return Err(self.error("invalid escape")),
        };
        self.at += 1;
        out.push(c);
        Ok(())
    }

    fn hex4(&mut self) -> Result<u32, JsonError> {
        let digits = self
            .bytes
            .get(self.at..self.at + 4)
            .ok_or_else(|| self.error("short \\u escape"))?;
        let mut n = 0;
        for &d in digits {
            let v = (d as char)
                .to_digit(16)
                .ok_or_else(|| self.error("invalid \\u escape"))?;
            n = n * 16 + v;
        }
        self.at += 4;
        Ok(n)
    }
}

fn write_into(value: &Json, out: &mut String) {
    match value {
        Json::Null => out.push_str("null"),
        Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Json::Num(n) => out.push_str(&n.0),
        Json::Str(s) => write_str(s, out),
        Json::Arr(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_into(item, out);
            }
            out.push(']');
        }
        Json::Obj(fields) => {
            out.push('{');
            for (i, (key, item)) in fields.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_str(key, out);
                out.push(':');
                write_into(item, out);
            }
            out.push('}');
        }
    }
}

fn write_str(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
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
