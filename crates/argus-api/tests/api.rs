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
