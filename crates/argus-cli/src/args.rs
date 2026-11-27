//! Command-line arguments as an op's input, read against the op's input schema
//! (`OpInfo::input_schema`), so every op takes arguments without a line of code of its
//! own and none can drift from its type:
//!
//! ```text
//! argus request-review <job> --reviewers preset:quick --lens security
//! argus decide <job> <note> --verdict request-changes --tree <oid>
//! argus set-task <job> --title Parser --weight 2 --status blocked:waiting for the API
//! argus start-jobs <repo> <title> <prompt> --agents claude:opus,codex:gpt-5
//! ```
//!
//! - Positional arguments fill the required text fields in declaration order
//!   (`request_id` aside); `--<field> <value>` sets any field (`-` or `_` in its name).
//! - A value is read by its field's type: text as is; numbers and `true`/`false`; a tag
//!   by name, ignoring case, `-` and `_` (`request-changes`); a struct as its fields
//!   joined by `:` (`claude:opus:high`); a tagged union as its variant, then its fields
//!   (`preset:quick`, `default`); a list as items joined by `,` (a repeated flag adds
//!   items). In `a:b:c`, the last field takes the rest, so it may hold `:`; an empty
//!   part leaves an optional field out. A value starting with `{` or `[` is JSON.
//! - A field the caller has a default for (`job` inside a run: `ARGUS_JOB`) takes no
//!   positional and is filled when not named.
//! - A missing `request_id` is minted (an error if none can be); other missing required
//!   fields are an error. A
//!   bare `--<field>` of a boolean is `true`.

use argus_json::{Json, Number};

#[derive(Debug, PartialEq)]
pub(crate) enum ArgsError {
    UnknownField(String),
    Missing(Vec<String>),
    Unexpected(String),
    BadValue {
        field: String,
        value: String,
        expected: String,
    },
}

impl std::fmt::Display for ArgsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ArgsError::UnknownField(name) => write!(f, "no field --{}", kebab(name)),
            ArgsError::Missing(names) => {
                let names: Vec<String> = names.iter().map(|n| format!("--{}", kebab(n))).collect();
                write!(f, "missing {}", names.join(", "))
            }
            ArgsError::Unexpected(arg) => write!(f, "unexpected argument {arg:?}"),
            ArgsError::BadValue {
                field,
                value,
                expected,
            } => write!(f, "--{}: {value:?} is not {expected}", kebab(field)),
        }
    }
}

/// The input object `args` describe, with fields in the schema's order.
pub(crate) fn input(
    schema: &Json,
    args: &[String],
    mint: impl FnOnce() -> Option<String>,
    defaults: &[(&str, String)],
) -> Result<Json, ArgsError> {
    let default = |name: &str| defaults.iter().find(|(d, _)| *d == name).map(|(_, v)| v);
    let fields = properties(schema);
    let required = required(schema);
    let mut given: Vec<(String, Json)> = Vec::new();
    let mut positionals = fields
        .iter()
        .filter(|(name, s)| {
            required.contains(name) && name != "request_id" && is_text(s) && default(name).is_none()
        })
        .map(|(name, _)| name.clone());
    let mut rest = args.iter().peekable();
    while let Some(arg) = rest.next() {
        let (name, text) = match arg.strip_prefix("--") {
            Some(flag) => {
                let name = flag.replace('-', "_");
                let Some((_, s)) = fields.iter().find(|(n, _)| *n == name) else {
                    return Err(ArgsError::UnknownField(name));
                };
                let bare = rest.peek().is_none_or(|next| next.starts_with("--"));
                let text = match (bare, kind(s)) {
                    (true, Some("boolean")) => "true".to_owned(),
                    (true, _) => return Err(ArgsError::Missing(vec![name])),
                    (false, _) => rest.next().cloned().unwrap_or_default(),
                };
                (name, text)
            }
            None => {
                let Some(name) = positionals.find(|n| !given.iter().any(|(g, _)| g == n)) else {
                    return Err(ArgsError::Unexpected(arg.clone()));
                };
                (name, arg.clone())
            }
        };
        let schema = &fields.iter().find(|(n, _)| *n == name).expect("found").1;
        let bad = |expected: String| ArgsError::BadValue {
            field: name.clone(),
            value: text.clone(),
            expected,
        };
        let parsed = value(schema, &text).map_err(bad)?;
        match given.iter_mut().find(|(g, _)| *g == name) {
            // A repeated list flag adds items; any other repeat replaces.
            Some((_, Json::Arr(items))) if matches!(parsed, Json::Arr(_)) => {
                let Json::Arr(more) = parsed else {
                    unreachable!()
                };
                items.extend(more);
            }
            Some((_, old)) => *old = parsed,
            None => given.push((name, parsed)),
        }
    }
    let mut mint = Some(mint);
    let mut missing = Vec::new();
    let mut out = Vec::new();
    for (name, _) in &fields {
        let v = match given.iter().position(|(g, _)| g == name) {
            Some(at) => given.swap_remove(at).1,
            None if name == "request_id" => match mint.take().and_then(|mint| mint()) {
                Some(id) => Json::str(id),
                None => {
                    missing.push(name.clone());
                    continue;
                }
            },
            None if default(name).is_some() => {
                let text = default(name).expect("some");
                let schema = &fields.iter().find(|(n, _)| n == name).expect("found").1;
                value(schema, text).map_err(|expected| ArgsError::BadValue {
                    field: name.clone(),
                    value: text.clone(),
                    expected,
                })?
            }
            None if required.contains(name) => {
                missing.push(name.clone());
                continue;
            }
            None => Json::Null,
        };
        out.push((name.clone(), v));
    }
    if !missing.is_empty() {
        return Err(ArgsError::Missing(missing));
    }
    Ok(Json::Obj(out))
}

/// `--field <what>` lines for `argus help <op>`; positional fields as `<field>`.
pub(crate) fn usage(op: &str, schema: &Json) -> String {
    let fields = properties(schema);
    let required = required(schema);
    let positional = |name: &str, s: &Json| {
        required.iter().any(|r| r == name) && name != "request_id" && is_text(s)
    };
    let mut line = format!("argus {}", kebab(op));
    for (name, s) in &fields {
        if positional(name, s) {
            line.push_str(&format!(" <{}>", kebab(name)));
        }
    }
    let mut out = line;
    for (name, s) in &fields {
        if positional(name, s) {
            continue;
        }
        let mut row = format!("\n  --{} {}", kebab(name), shape(s));
        if !required.contains(name) || name == "request_id" {
            row.push_str("  (optional)");
        }
        if let Some(doc) = s.get("description").and_then(Json::as_str) {
            row.push_str(&format!("  {doc}"));
        }
        out.push_str(&row);
    }
    out
}

/// `RequestChanges` -> `request-changes`, `group_by` -> `group-by`.
pub(crate) fn kebab(name: &str) -> String {
    let mut out = String::new();
    for (i, c) in name.chars().enumerate() {
        match c {
            '_' => out.push('-'),
            c if c.is_ascii_uppercase() => {
                if i > 0 {
                    out.push('-');
                }
                out.push(c.to_ascii_lowercase());
            }
            c => out.push(c),
        }
    }
    out
}

/// Case, `-` and `_` do not matter in names typed on a command line.
fn loose(name: &str) -> String {
    name.chars()
        .filter(|c| *c != '-' && *c != '_')
        .flat_map(char::to_lowercase)
        .collect()
}

fn properties(schema: &Json) -> Vec<(String, Json)> {
    match schema.get("properties") {
        Some(Json::Obj(fields)) => fields.clone(),
        _ => Vec::new(),
    }
}

fn required(schema: &Json) -> Vec<String> {
    match schema.get("required") {
        Some(Json::Arr(names)) => names
            .iter()
            .filter_map(Json::as_str)
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

/// Free text: a string that is not one of a set of tags.
fn is_text(schema: &Json) -> bool {
    kind(schema) == Some("string") && schema.get("enum").is_none()
}

fn kind(schema: &Json) -> Option<&str> {
    schema.get("type").and_then(Json::as_str)
}

fn list<'a>(schema: &'a Json, key: &str) -> &'a [Json] {
    match schema.get(key) {
        Some(Json::Arr(items)) => items,
        _ => &[],
    }
}

/// One value of `schema` from `text`; the error says what was expected.
fn value(schema: &Json, text: &str) -> Result<Json, String> {
    let json = || argus_json::parse(text.as_bytes()).map_err(|_| "valid JSON".to_owned());
    if kind(schema) != Some("string") && (text.starts_with('{') || text.starts_with('[')) {
        return json();
    }
    if let [some, none] = list(schema, "anyOf")
        && none.get("type").and_then(Json::as_str) == Some("null")
    {
        return if text == "null" {
            Ok(Json::Null)
        } else {
            value(some, text)
        };
    }
    let tags = list(schema, "enum");
    if !tags.is_empty() {
        let names: Vec<&str> = tags.iter().filter_map(Json::as_str).collect();
        return names
            .iter()
            .find(|t| loose(t) == loose(text))
            .map(|t| Json::str(*t))
            .ok_or_else(|| one_of(&names));
    }
    let arms = list(schema, "oneOf");
    if !arms.is_empty() {
        let (variant, rest) = text.split_once(':').unwrap_or((text, ""));
        let name = |arm: &Json| {
            arm.get("properties")
                .and_then(|p| p.get("kind"))
                .and_then(|k| k.get("const"))
                .and_then(Json::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        let Some(arm) = arms.iter().find(|arm| loose(&name(arm)) == loose(variant)) else {
            let names: Vec<String> = arms.iter().map(name).collect();
            return Err(one_of(
                &names.iter().map(String::as_str).collect::<Vec<_>>(),
            ));
        };
        let mut fields = vec![("kind".to_owned(), Json::str(name(arm)))];
        fields.extend(joined(arm, rest, true)?);
        return Ok(Json::Obj(fields));
    }
    match kind(schema) {
        Some("string") => Ok(Json::str(text)),
        Some("boolean") => match text {
            "true" => Ok(Json::Bool(true)),
            "false" => Ok(Json::Bool(false)),
            _ => Err("true or false".into()),
        },
        Some("integer") => {
            let n: i64 = text.parse().map_err(|_| "a whole number".to_owned())?;
            let bound = |key: &str| match schema.get(key) {
                Some(Json::Num(b)) => b.as_i64(),
                _ => None,
            };
            if bound("minimum").is_some_and(|min| n < min)
                || bound("maximum").is_some_and(|max| n > max)
            {
                return Err("in range".into());
            }
            Ok(Json::Num(Number::from_i64(n)))
        }
        Some("number") => text
            .parse::<f64>()
            .ok()
            .and_then(Number::from_f64)
            .map(Json::Num)
            .ok_or_else(|| "a number".into()),
        Some("array") => {
            let items = schema
                .get("items")
                .cloned()
                .unwrap_or(Json::Obj(Vec::new()));
            let parts = if text.is_empty() {
                Vec::new()
            } else {
                text.split(',')
                    .map(|part| value(&items, part))
                    .collect::<Result<_, _>>()?
            };
            Ok(Json::Arr(parts))
        }
        Some("object") => Ok(Json::Obj(joined(schema, text, false)?)),
        // Any JSON (no type): JSON if it reads as JSON, else text.
        _ => Ok(argus_json::parse(text.as_bytes()).unwrap_or_else(|_| Json::str(text))),
    }
}

/// The fields of a struct (or a union arm, past `kind`) from `a:b:c`.
fn joined(schema: &Json, text: &str, arm: bool) -> Result<Vec<(String, Json)>, String> {
    let fields: Vec<(String, Json)> = properties(schema)
        .into_iter()
        .filter(|(name, _)| !(arm && name == "kind"))
        .collect();
    let required = required(schema);
    let mut parts = Vec::new();
    let mut rest = Some(text).filter(|t| !t.is_empty());
    for i in 0..fields.len() {
        let last = i + 1 == fields.len();
        let part = match (rest, last) {
            (None, _) => None,
            (Some(r), true) => {
                rest = None;
                Some(r)
            }
            (Some(r), false) => {
                let (part, more) = r.split_once(':').unwrap_or((r, ""));
                rest = Some(more).filter(|m| !m.is_empty());
                Some(part)
            }
        };
        parts.push(part.filter(|p| !p.is_empty()));
    }
    if rest.is_some() || (fields.is_empty() && !text.is_empty()) {
        return Err(format!("{} parts", fields.len()));
    }
    fields
        .into_iter()
        .zip(parts)
        .map(|((name, s), part)| match part {
            Some(p) => value(&s, p).map(|v| (name, v)),
            None if required.contains(&name) => Err(format!("given a {}", kebab(&name))),
            None => Ok((name, Json::Null)),
        })
        .collect()
}

fn one_of(names: &[&str]) -> String {
    let names: Vec<String> = names.iter().map(|n| kebab(n)).collect();
    format!("one of {}", names.join(", "))
}

/// How a value of `schema` is typed, for `argus help`.
fn shape(schema: &Json) -> String {
    if let [some, _] = list(schema, "anyOf") {
        return shape(some);
    }
    let tags = list(schema, "enum");
    if !tags.is_empty() {
        let names: Vec<String> = tags.iter().filter_map(Json::as_str).map(kebab).collect();
        return names.join("|");
    }
    let arms = list(schema, "oneOf");
    if !arms.is_empty() {
        let forms: Vec<String> = arms
            .iter()
            .map(|arm| {
                let variant = arm
                    .get("properties")
                    .and_then(|p| p.get("kind"))
                    .and_then(|k| k.get("const"))
                    .and_then(Json::as_str)
                    .map(kebab)
                    .unwrap_or_default();
                format!("{variant}{}", parts(arm, true))
            })
            .collect();
        return forms.join(" | ");
    }
    match kind(schema) {
        Some("string") => "<text>".into(),
        Some("boolean") => "true|false".into(),
        Some("integer") | Some("number") => "<number>".into(),
        Some("array") => format!(
            "{},...",
            shape(&schema.get("items").cloned().unwrap_or(Json::Null))
        ),
        Some("object") => parts(schema, false)
            .strip_prefix(':')
            .unwrap_or_default()
            .to_owned(),
        _ => "<JSON>".into(),
    }
}

/// `:a:b[:c]`: a struct's fields (a union arm's, past `kind`), optional ones in brackets.
fn parts(schema: &Json, arm: bool) -> String {
    let required = required(schema);
    properties(schema)
        .into_iter()
        .filter(|(name, _)| !(arm && name == "kind"))
        .map(|(name, _)| match required.contains(&name) {
            true => format!(":{}", kebab(&name)),
            false => format!("[:{}]", kebab(&name)),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use argus_api::vm::*;
    use argus_api::wire::Wire;

    fn args(text: &[&str]) -> Vec<String> {
        text.iter().map(|s| s.to_string()).collect()
    }

    fn read<T: Wire>(text: &[&str]) -> Result<T, ArgsError> {
        let json = input(&T::schema(), &args(text), || Some("minted".into()), &[])?;
        Ok(T::from_json(&json).unwrap_or_else(|e| panic!("{}: {}", e.path, e.what)))
    }

    #[test]
    fn positionals_fill_the_required_text_fields_and_flags_the_rest() {
        let got: RequestReview =
            read(&["j1", "--reviewers", "preset:quick", "--lens", "security"]).unwrap();
        assert_eq!(
            got.to_json(),
            RequestReview {
                request_id: "minted".into(),
                job: "j1".into(),
                reviewers: Reviewers::Preset {
                    name: "quick".into()
                },
                lens: Lens::Security,
                focus: None,
            }
            .to_json()
        );
        let got: Decide = read(&[
            "j1",
            "ship it",
            "--verdict",
            "request-changes",
            "--request-id",
            "r7",
        ])
        .unwrap();
        assert_eq!(
            got.to_json(),
            Decide {
                request_id: "r7".into(),
                job: "j1".into(),
                tree: None,
                verdict: Verdict::RequestChanges,
                note: "ship it".into(),
            }
            .to_json()
        );
    }

    #[test]
    fn structs_unions_and_lists_are_written_with_colons_and_commas() {
        let got: StartJobs = read(&[
            "/src/app",
            "Parser",
            "write the parser",
            "--agents",
            "claude:opus,codex",
            "--agents",
            "codex:gpt-5",
        ])
        .unwrap();
        let picks: Vec<(Agent, Option<String>)> =
            got.agents.into_iter().map(|a| (a.agent, a.model)).collect();
        assert_eq!(
            format!("{picks:?}"),
            format!(
                "{:?}",
                vec![
                    (Agent::Claude, Some("opus".to_owned())),
                    (Agent::Codex, None),
                    (Agent::Codex, Some("gpt-5".to_owned())),
                ]
            )
        );
        // The last field takes the rest, colons and all.
        let got: SetTask = read(&[
            "j1",
            "--status",
            "blocked:waiting: on the API",
            "--weight",
            "2",
        ])
        .unwrap();
        assert_eq!(
            got.to_json(),
            SetTask {
                job: "j1".into(),
                task: None,
                parent: None,
                title: None,
                weight: Some(2),
                status: Some(TaskStatus::Blocked {
                    reason: "waiting: on the API".into()
                }),
            }
            .to_json()
        );
        // An empty part leaves an optional field out; a model pick with an effort.
        let got: RequestCheck = read(&["j1", "--check", "tla:specs/Gate.tla::java-tlc"]).unwrap();
        assert_eq!(
            got.check.to_json(),
            CheckChoice::Tla {
                spec: "specs/Gate.tla".into(),
                config: None,
                engine: Engine::JavaTlc,
                workers: None,
            }
            .to_json()
        );
        let got: RequestInsights = read(&["last-7d", "--model", "codex:gpt-5:max"]).unwrap();
        assert_eq!(
            got.model.to_json(),
            ModelPick {
                agent: Agent::Codex,
                model: "gpt-5".into(),
                effort: Some(Effort::Max),
            }
            .to_json()
        );
        // JSON stays available for anything.
        let got: RequestReview = read(&[
            "j1",
            "--lens",
            "general",
            "--reviewers",
            r#"{"kind":"Models","models":[{"agent":"Claude","model":"opus","effort":null}]}"#,
        ])
        .unwrap();
        assert!(matches!(got.reviewers, Reviewers::Models { models } if models.len() == 1));
    }

    #[test]
    fn booleans_and_numbers_are_checked_before_the_host_sees_them() {
        let got: DismissFinding = read(&["j1", "f1", "--dismissed"]).unwrap();
        assert!(got.dismissed);
        let err = read::<RawQuery>(&["j1", "--limit", "70000"]).unwrap_err();
        assert!(
            matches!(err, ArgsError::BadValue { ref field, .. } if field == "limit"),
            "{err:?}"
        );
    }

    #[test]
    fn without_a_fresh_request_id_the_user_must_name_one() {
        let err = input(
            &Decide::schema(),
            &args(&["j", "n", "--verdict", "approve"]),
            || None,
            &[],
        );
        assert_eq!(
            err.unwrap_err(),
            ArgsError::Missing(vec!["request_id".into()])
        );
    }

    #[test]
    fn a_field_with_a_default_is_filled_and_takes_no_positional() {
        let job = [("job", "j9".to_owned())];
        let got = input(&SetTask::schema(), &args(&["--title", "P"]), || None, &job).unwrap();
        assert_eq!(SetTask::from_json(&got).unwrap().job, "j9");
        let got = input(
            &Reply::schema(),
            &args(&["t1", "hello"]),
            || Some("r".into()),
            &job,
        );
        let got = Reply::from_json(&got.unwrap()).unwrap();
        assert_eq!(
            (got.job, got.thread, got.body),
            ("j9".into(), "t1".into(), "hello".into())
        );
        // Named, it wins.
        let got = input(&JobRef::schema(), &args(&["--job", "j1"]), || None, &job).unwrap();
        assert_eq!(JobRef::from_json(&got).unwrap().job, "j1");
    }

    #[test]
    fn mistakes_name_the_field() {
        assert_eq!(
            read::<Decide>(&["j1", "note"]).unwrap_err(),
            ArgsError::Missing(vec!["verdict".into()])
        );
        assert_eq!(
            read::<Decide>(&["j1", "note", "--frobnicate", "x"]).unwrap_err(),
            ArgsError::UnknownField("frobnicate".into())
        );
        assert_eq!(
            read::<JobRef>(&["j1", "j2"]).unwrap_err(),
            ArgsError::Unexpected("j2".into())
        );
        let err = read::<Decide>(&["j1", "n", "--verdict", "maybe"]).unwrap_err();
        assert_eq!(
            err.to_string(),
            r#"--verdict: "maybe" is not one of approve, request-changes, abandon"#
        );
    }

    #[test]
    fn help_shows_each_field_and_how_to_write_it() {
        let text = usage("request_review", &RequestReview::schema());
        assert!(text.starts_with("argus request-review <job>\n"), "{text}");
        assert!(
            text.contains("--reviewers default | preset:name | models:models"),
            "{text}"
        );
        assert!(
            text.contains("--lens general|correctness|security|tests"),
            "{text}"
        );
        assert!(text.contains("--focus <text>  (optional)"), "{text}");
        assert!(text.contains("--request-id <text>  (optional)"), "{text}");
        let text = usage("request_insights", &RequestInsights::schema());
        assert!(text.contains("--model agent:model[:effort]"), "{text}");
        let text = usage("set_task", &SetTask::schema());
        assert!(text.contains("blocked:reason | done[:result]"), "{text}");
    }
}
