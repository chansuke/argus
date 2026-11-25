//! `Wire`: one declaration of a view-model type yields its Rust type, its JSON codec, and
//! its TypeScript declaration. `vm_struct!` / `vm_enum!` generate all three, so the three
//! cannot drift (std-only stand-in for serde + ts-rs/specta).
//!
//! Wire conventions: field and op names stay snake_case in JSON and TS (no renaming
//! layer to get wrong); enums are tagged unions `{ kind: "Variant", ...fields }`;
//! ids and 64-bit counters that may exceed 2^53 travel as strings.
//!
//! `schema()` is the same declaration as a JSON Schema, for clients that pick inputs from
//! one (MCP tools). A field whose type is an `Option` may be left out.

use std::collections::BTreeMap;

use argus_json::{Json, Number};

#[derive(Clone, Debug, PartialEq)]
pub struct WireError {
    pub path: String,
    pub what: String,
}

pub trait Wire: Sized {
    /// TS type expression naming this type.
    fn ts_ref() -> String;
    /// Inserts this type's TS declaration (if it is a named type) and its dependencies'.
    fn ts_collect(out: &mut BTreeMap<String, String>) {
        let _ = out;
    }
    /// JSON Schema of the values `from_json` accepts.
    fn schema() -> Json;
    /// Whether `null` is a value of this type, so a field of it may be left out.
    const NULLABLE: bool = false;
    fn to_json(&self) -> Json;
    fn from_json(v: &Json) -> Result<Self, WireError>;
}

/// `{ "key": value, ... }` in the order given.
pub(crate) fn object(pairs: Vec<(&str, Json)>) -> Json {
    Json::Obj(pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect())
}

/// `{ "type": "<name>" }`.
fn typed(name: &str) -> Json {
    object(vec![("type", Json::str(name))])
}

/// A declaration's doc lines as one description.
pub(crate) fn describe(lines: &[&str]) -> String {
    lines.iter().map(|l| l.trim()).collect::<Vec<_>>().join(" ")
}

/// Reads one field; a missing field reads as `null` (so `Option` fields may be omitted).
pub fn field<T: Wire>(v: &Json, name: &str) -> Result<T, WireError> {
    T::from_json(v.get(name).unwrap_or(&Json::Null)).map_err(|e| WireError {
        path: format!("{name}.{}", e.path),
        what: e.what,
    })
}

fn mismatch(expected: &str) -> WireError {
    WireError {
        path: String::new(),
        what: format!("expected {expected}"),
    }
}

impl Wire for String {
    fn ts_ref() -> String {
        "string".into()
    }
    fn schema() -> Json {
        typed("string")
    }
    fn to_json(&self) -> Json {
        Json::Str(self.clone())
    }
    fn from_json(v: &Json) -> Result<Self, WireError> {
        v.as_str()
            .map(str::to_owned)
            .ok_or_else(|| mismatch("string"))
    }
}

impl Wire for bool {
    fn ts_ref() -> String {
        "boolean".into()
    }
    fn schema() -> Json {
        typed("boolean")
    }
    fn to_json(&self) -> Json {
        Json::Bool(*self)
    }
    fn from_json(v: &Json) -> Result<Self, WireError> {
        match v {
            Json::Bool(b) => Ok(*b),
            _ => Err(mismatch("boolean")),
        }
    }
}

macro_rules! wire_int {
    ($($t:ty),*) => {$(
        impl Wire for $t {
            fn ts_ref() -> String { "number".into() }
            fn schema() -> Json {
                object(vec![
                    ("type", Json::str("integer")),
                    ("minimum", Json::Num(Number::from_i64(<$t>::MIN as i64))),
                    ("maximum", Json::Num(Number::from_i64(<$t>::MAX as i64))),
                ])
            }
            fn to_json(&self) -> Json { Json::Num(Number::from_i64(*self as i64)) }
            fn from_json(v: &Json) -> Result<Self, WireError> {
                match v {
                    Json::Num(n) => n.as_i64().and_then(|x| <$t>::try_from(x).ok()).ok_or_else(|| mismatch(stringify!($t))),
                    _ => Err(mismatch("number")),
                }
            }
        }
    )*};
}
wire_int!(u16, u32, i64);

impl Wire for f64 {
    fn ts_ref() -> String {
        "number".into()
    }
    fn schema() -> Json {
        typed("number")
    }
    fn to_json(&self) -> Json {
        Number::from_f64(*self).map_or(Json::Null, Json::Num)
    }
    fn from_json(v: &Json) -> Result<Self, WireError> {
        match v {
            Json::Num(n) => n.as_f64().ok_or_else(|| mismatch("number")),
            _ => Err(mismatch("number")),
        }
    }
}

/// Opaque JSON (frame payloads before dispatch).
impl Wire for Json {
    fn ts_ref() -> String {
        "unknown".into()
    }
    fn schema() -> Json {
        object(vec![])
    }
    const NULLABLE: bool = true;
    fn to_json(&self) -> Json {
        self.clone()
    }
    fn from_json(v: &Json) -> Result<Self, WireError> {
        Ok(v.clone())
    }
}

impl<T: Wire> Wire for Vec<T> {
    fn ts_ref() -> String {
        format!("Array<{}>", T::ts_ref())
    }
    fn schema() -> Json {
        object(vec![("type", Json::str("array")), ("items", T::schema())])
    }
    fn ts_collect(out: &mut BTreeMap<String, String>) {
        T::ts_collect(out)
    }
    fn to_json(&self) -> Json {
        Json::Arr(self.iter().map(Wire::to_json).collect())
    }
    fn from_json(v: &Json) -> Result<Self, WireError> {
        match v {
            Json::Arr(xs) => xs.iter().map(T::from_json).collect(),
            _ => Err(mismatch("array")),
        }
    }
}

impl<T: Wire> Wire for Option<T> {
    fn ts_ref() -> String {
        format!("{} | null", T::ts_ref())
    }
    fn schema() -> Json {
        object(vec![("anyOf", Json::Arr(vec![T::schema(), typed("null")]))])
    }
    const NULLABLE: bool = true;
    fn ts_collect(out: &mut BTreeMap<String, String>) {
        T::ts_collect(out)
    }
    fn to_json(&self) -> Json {
        self.as_ref().map_or(Json::Null, Wire::to_json)
    }
    fn from_json(v: &Json) -> Result<Self, WireError> {
        match v {
            Json::Null => Ok(None),
            v => T::from_json(v).map(Some),
        }
    }
}

/// `vm_struct! { /// doc  Name { field: Type, ... } }` -> struct + Wire impl + `export interface`.
macro_rules! vm_struct {
    ($(#[doc = $doc:literal])* $name:ident { $($(#[doc = $fdoc:literal])* $field:ident : $ty:ty),* $(,)? }) => {
        $(#[doc = $doc])*
        #[derive(Clone, Debug, PartialEq)]
        pub struct $name { $($(#[doc = $fdoc])* pub $field: $ty),* }

        impl $crate::wire::Wire for $name {
            fn ts_ref() -> String { stringify!($name).to_string() }
            fn ts_collect(out: &mut ::std::collections::BTreeMap<String, String>) {
                if out.contains_key(stringify!($name)) { return; }
                let fields: Vec<String> = vec![$(format!("  {}: {};", stringify!($field), <$ty as $crate::wire::Wire>::ts_ref())),*];
                out.insert(stringify!($name).to_string(), format!("export interface {} {{\n{}\n}}", stringify!($name), fields.join("\n")));
                $(<$ty as $crate::wire::Wire>::ts_collect(out);)*
            }
            fn schema() -> argus_json::Json {
                use argus_json::Json;
                let properties: Vec<(&str, Json)> = vec![$((stringify!($field), {
                    let mut schema = <$ty as $crate::wire::Wire>::schema();
                    let doc = $crate::wire::describe(&[$($fdoc),*]);
                    if let (Json::Obj(rules), false) = (&mut schema, doc.is_empty()) {
                        rules.push(("description".into(), Json::str(doc)));
                    }
                    schema
                })),*];
                let fields: Vec<(&str, bool)> = vec![$(( stringify!($field), <$ty as $crate::wire::Wire>::NULLABLE )),*];
                let required: Vec<Json> = fields
                    .into_iter()
                    .filter(|(_, nullable)| !nullable)
                    .map(|(name, _)| Json::str(name))
                    .collect();
                let mut schema = vec![
                    ("type", Json::str("object")),
                    ("properties", $crate::wire::object(properties)),
                    ("required", Json::Arr(required)),
                ];
                let doc = $crate::wire::describe(&[$($doc),*]);
                if !doc.is_empty() {
                    schema.push(("description", Json::str(doc)));
                }
                $crate::wire::object(schema)
            }
            fn to_json(&self) -> argus_json::Json {
                argus_json::Json::Obj(vec![$((stringify!($field).to_string(), $crate::wire::Wire::to_json(&self.$field))),*])
            }
            #[allow(unused_variables, reason = "a struct with no fields reads nothing from `v`")]
            fn from_json(v: &argus_json::Json) -> Result<Self, $crate::wire::WireError> {
                Ok(Self { $($field: $crate::wire::field(v, stringify!($field))?),* })
            }
        }
    };
}

/// `vm_enum! { Name { A {}, B { x: T } } }` -> enum + Wire impl + TS tagged union.
macro_rules! vm_enum {
    ($(#[doc = $doc:literal])* $name:ident { $($(#[doc = $vdoc:literal])* $variant:ident { $($field:ident : $ty:ty),* $(,)? }),* $(,)? }) => {
        $(#[doc = $doc])*
        #[derive(Clone, Debug, PartialEq)]
        pub enum $name { $($(#[doc = $vdoc])* $variant { $($field: $ty),* }),* }

        impl $crate::wire::Wire for $name {
            fn ts_ref() -> String { stringify!($name).to_string() }
            fn ts_collect(out: &mut ::std::collections::BTreeMap<String, String>) {
                if out.contains_key(stringify!($name)) { return; }
                let arms: Vec<String> = vec![$({
                    let fields: Vec<String> = vec![$(format!(" {}: {};", stringify!($field), <$ty as $crate::wire::Wire>::ts_ref())),*];
                    format!("{{ kind: \"{}\";{} }}", stringify!($variant), fields.concat())
                }),*];
                out.insert(stringify!($name).to_string(), format!("export type {} =\n  | {};", stringify!($name), arms.join("\n  | ")));
                $($(<$ty as $crate::wire::Wire>::ts_collect(out);)*)*
            }
            fn schema() -> argus_json::Json {
                use argus_json::Json;
                let arms: Vec<Json> = vec![$({
                    let properties: Vec<(&str, Json)> = vec![
                        ("kind", $crate::wire::object(vec![("const", Json::str(stringify!($variant)))]))
                        $(, (stringify!($field), <$ty as $crate::wire::Wire>::schema()))*
                    ];
                    let required: Vec<Json> = vec![("kind", false) $(, (stringify!($field), <$ty as $crate::wire::Wire>::NULLABLE))*]
                        .into_iter()
                        .filter(|(_, nullable)| !nullable)
                        .map(|(name, _)| Json::str(name))
                        .collect();
                    $crate::wire::object(vec![
                        ("type", Json::str("object")),
                        ("properties", $crate::wire::object(properties)),
                        ("required", Json::Arr(required)),
                    ])
                }),*];
                $crate::wire::object(vec![("oneOf", Json::Arr(arms))])
            }
            fn to_json(&self) -> argus_json::Json {
                match self {
                    $(Self::$variant { $($field),* } => argus_json::Json::Obj(vec![
                        ("kind".to_string(), argus_json::Json::str(stringify!($variant)))
                        $(, (stringify!($field).to_string(), $crate::wire::Wire::to_json($field)))*
                    ]),)*
                }
            }
            fn from_json(v: &argus_json::Json) -> Result<Self, $crate::wire::WireError> {
                let kind = v.get("kind").and_then(argus_json::Json::as_str).unwrap_or("");
                $(if kind == stringify!($variant) {
                    return Ok(Self::$variant { $($field: $crate::wire::field(v, stringify!($field))?),* });
                })*
                Err($crate::wire::WireError { path: "kind".into(), what: format!("unknown variant {kind:?}") })
            }
        }
    };
}

/// `vm_tags! { Name { A, B } }` -> fieldless enum + Wire impl as a bare JSON string + TS
/// string-literal union `"A" | "B"`. For every closed set on the wire (agent, lens,
/// engine, platform, verdict, group-by, ...): no plain `string` where the set is known.
macro_rules! vm_tags {
    ($(#[doc = $doc:literal])* $name:ident { $($variant:ident),* $(,)? }) => {
        $(#[doc = $doc])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
        pub enum $name { $($variant),* }

        impl $crate::wire::Wire for $name {
            fn ts_ref() -> String { stringify!($name).to_string() }
            fn ts_collect(out: &mut ::std::collections::BTreeMap<String, String>) {
                let tags: Vec<String> = vec![$(format!("\"{}\"", stringify!($variant))),*];
                out.insert(stringify!($name).to_string(), format!("export type {} = {};", stringify!($name), tags.join(" | ")));
            }
            fn schema() -> argus_json::Json {
                $crate::wire::object(vec![
                    ("type", argus_json::Json::str("string")),
                    ("enum", argus_json::Json::Arr(vec![$(argus_json::Json::str(stringify!($variant))),*])),
                ])
            }
            fn to_json(&self) -> argus_json::Json {
                argus_json::Json::str(match self { $(Self::$variant => stringify!($variant)),* })
            }
            fn from_json(v: &argus_json::Json) -> Result<Self, $crate::wire::WireError> {
                match v.as_str() {
                    $(Some(stringify!($variant)) => Ok(Self::$variant),)*
                    other => Err($crate::wire::WireError { path: String::new(), what: format!("unknown {} {other:?}", stringify!($name)) }),
                }
            }
        }
    };
}

pub(crate) use {vm_enum, vm_struct, vm_tags};
