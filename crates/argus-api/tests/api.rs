//! The API's access rule and the wire codec's basic types, on their own.

use std::collections::BTreeMap;

use argus_api::wire::{Wire, field};
use argus_api::{Caller, OPS, OpAccess};
use argus_json::{Json, parse};

#[test]
fn people_may_call_every_op_and_agents_only_the_ones_open_to_them() {
    let local = Caller::Local;
    let phone = Caller::Phone {
        device: "pixel".into(),
    };
    let agent = Caller::Agent { run: "r".into() };
    assert!(OPS.iter().any(|op| op.access == OpAccess::Agent));
    assert!(OPS.iter().any(|op| op.access == OpAccess::Human));
    for op in OPS {
        assert!(local.may(op), "{}", op.name);
        assert!(phone.may(op), "{}", op.name);
        assert_eq!(agent.may(op), op.access == OpAccess::Agent, "{}", op.name);
    }
}

fn json(s: &str) -> Json {
    parse(s.as_bytes()).unwrap()
}

#[test]
fn basic_types_decode_their_own_json_and_refuse_others() {
    assert_eq!(String::from_json(&json(r#""x""#)), Ok("x".to_owned()));
    assert!(String::from_json(&json("1")).is_err());
    assert_eq!(bool::from_json(&json("true")), Ok(true));
    assert!(bool::from_json(&json(r#""true""#)).is_err());
    assert_eq!(u32::from_json(&json("7")), Ok(7));
    assert!(u32::from_json(&json("-1")).is_err());
    assert_eq!(f64::from_json(&json("1.5")), Ok(1.5));
    assert!(f64::from_json(&json("null")).is_err());
    assert_eq!(Vec::<u32>::from_json(&json("[1,2]")), Ok(vec![1, 2]));
    assert!(Vec::<u32>::from_json(&json("{}")).is_err());
    assert!(Vec::<u32>::from_json(&json(r#"[1,"2"]"#)).is_err());
    assert_eq!(Option::<u32>::from_json(&json("null")), Ok(None));
    assert_eq!(Option::<u32>::from_json(&json("3")), Ok(Some(3)));
    assert!(Option::<u32>::from_json(&json(r#""3""#)).is_err());
}

#[test]
fn a_missing_field_reads_as_null_and_a_wrong_one_names_its_path() {
    let v = json(r#"{"n":"x"}"#);
    assert_eq!(field::<Option<u32>>(&v, "absent"), Ok(None));
    let err = field::<u32>(&v, "n").unwrap_err();
    assert!(err.path.starts_with("n."), "{err:?}");
}

#[test]
fn basic_types_name_their_typescript_type() {
    assert_eq!(String::ts_ref(), "string");
    assert_eq!(bool::ts_ref(), "boolean");
    assert_eq!(Json::ts_ref(), "unknown");
    assert_eq!(f64::ts_ref(), "number");
    assert_eq!(u32::ts_ref(), "number");
    assert_eq!(Vec::<String>::ts_ref(), "Array<string>");
    assert_eq!(Option::<bool>::ts_ref(), "boolean | null");
    // Containers pass their element's declarations through.
    let mut decls = BTreeMap::new();
    Vec::<argus_api::vm::PlanRow>::ts_collect(&mut decls);
    assert!(decls.contains_key("PlanRow"), "{decls:?}");
    let mut decls = BTreeMap::new();
    Option::<argus_api::vm::PlanRow>::ts_collect(&mut decls);
    assert!(decls.contains_key("PlanRow"), "{decls:?}");
}

#[test]
fn a_float_writes_json_that_reads_back_as_the_same_number() {
    for x in [0.0, 1.0, 0.5, 0.1, 2.0 / 3.0, 1e-7, 1e21, -3.25] {
        let text = argus_json::write(&x.to_json());
        assert_eq!(f64::from_json(&json(&text)), Ok(x), "{text}");
    }
    assert_eq!(argus_json::write(&0.25f64.to_json()), "0.25");
    // JSON has no NaN or infinity.
    for x in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(x.to_json(), Json::Null);
    }
}

/// The part of JSON Schema that `Wire::schema` emits: `type`, `properties`, `required`,
/// `items`, `anyOf`, `oneOf`, `enum`, `const`, `minimum`, `maximum`.
fn fits(schema: &Json, value: &Json) -> bool {
    let Json::Obj(rules) = schema else {
        panic!("a schema is an object: {}", argus_json::write(schema))
    };
    rules.iter().all(|(rule, arg)| match (rule.as_str(), arg) {
        ("type", Json::Str(t)) => match (t.as_str(), value) {
            ("object", Json::Obj(_)) | ("array", Json::Arr(_)) => true,
            ("string", Json::Str(_)) | ("boolean", Json::Bool(_)) | ("null", Json::Null) => true,
            ("number", Json::Num(_)) => true,
            ("integer", Json::Num(n)) => n.as_i64().is_some(),
            _ => false,
        },
        ("properties", Json::Obj(props)) => props
            .iter()
            .all(|(name, s)| value.get(name).is_none_or(|v| fits(s, v))),
        ("required", Json::Arr(names)) => names
            .iter()
            .all(|n| value.get(n.as_str().unwrap()).is_some()),
        ("items", s) => match value {
            Json::Arr(xs) => xs.iter().all(|x| fits(s, x)),
            _ => true,
        },
        ("anyOf", Json::Arr(ss)) => ss.iter().any(|s| fits(s, value)),
        ("oneOf", Json::Arr(ss)) => ss.iter().filter(|s| fits(s, value)).count() == 1,
        ("enum", Json::Arr(xs)) => xs.contains(value),
        ("const", x) => x == value,
        ("minimum", Json::Num(m)) => match value {
            Json::Num(n) => n.as_f64() >= m.as_f64(),
            _ => true,
        },
        ("maximum", Json::Num(m)) => match value {
            Json::Num(n) => n.as_f64() <= m.as_f64(),
            _ => true,
        },
        ("description", _) => true,
        (rule, _) => panic!("a rule the schema should not use: {rule}"),
    })
}

/// The schema accepts exactly what the decoder accepts, on each sample.
fn agrees<T: Wire>(samples: &[&str]) {
    let schema = T::schema();
    for sample in samples {
        let value = json(sample);
        assert_eq!(
            fits(&schema, &value),
            T::from_json(&value).is_ok(),
            "{sample} against {}",
            argus_json::write(&schema)
        );
    }
}

#[test]
fn a_types_json_schema_accepts_what_its_decoder_accepts() {
    use argus_api::vm::{RequestCheck, SetTask};
    agrees::<SetTask>(&[
        r#"{"job":"j"}"#,
        r#"{"job":"j","title":"Parser","weight":2,"status":{"kind":"Done"}}"#,
        r#"{"job":"j","status":{"kind":"Blocked","reason":"needs a key"}}"#,
        r#"{"job":"j","task":null}"#,
        r#"{}"#,
        r#"{"job":1}"#,
        r#"{"job":"j","weight":-1}"#,
        r#"{"job":"j","weight":1.5}"#,
        r#"{"job":"j","status":{"kind":"Blocked"}}"#,
        r#"{"job":"j","status":{"kind":"Nope"}}"#,
        r#"[]"#,
    ]);
    agrees::<RequestCheck>(&[
        r#"{"request_id":"r","job":"j","check":{"kind":"Lean","root":"p","files":["A.lean"]}}"#,
        r#"{"request_id":"r","job":"j","check":{"kind":"Tla","spec":"s","engine":"Tlars"}}"#,
        r#"{"request_id":"r","job":"j","check":{"kind":"Tla","spec":"s","engine":"Tlars","workers":70000}}"#,
        r#"{"request_id":"r","job":"j","check":{"kind":"Tla","spec":"s","engine":"Tlc"}}"#,
        r#"{"request_id":"r","job":"j","check":{"kind":"Lean","root":"p","files":[1]}}"#,
        r#"{"request_id":"r","job":"j","check":{"kind":"Coq"}}"#,
    ]);
}

#[test]
fn every_op_says_what_input_it_takes_and_what_it_does() {
    for op in OPS {
        let schema = (op.input_schema)();
        assert_eq!(
            schema.get("type"),
            Some(&Json::str("object")),
            "{}",
            op.name
        );
        // Agents pick tools by their description.
        if op.access == OpAccess::Agent {
            assert!(!op.doc.trim().is_empty(), "{} has no doc", op.name);
        }
    }
    let set_task = OPS.iter().find(|op| op.name == "set_task").unwrap();
    assert_eq!((set_task.input_schema)(), argus_api::vm::SetTask::schema());
}
