//! `argus mcp`: an MCP server on stdio for agents that prefer tools to shelling out.
//! Its tools are the `argus_api::OPS` entries open to agents, with each op's doc as the
//! description and its input type's schema (`Wire::schema`) as `inputSchema`, so the MCP
//! surface cannot drift from the CLI or the API. A call goes to the host like any other.
//!
//! JSON-RPC 2.0, one message per line (MCP stdio transport). It answers the
//! `initialize` handshake of protocol revisions up to 2025-11-25:
//! <https://modelcontextprotocol.io/specification/2025-11-25/basic/lifecycle>.

use std::io::{BufRead, Write};

use argus_api::wire::Wire;
use argus_api::{OPS, OpAccess, OpInfo, Transport};
use argus_json::{Json, Number};

/// The revisions whose `initialize` this server answers, newest first.
const VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

/// Serves requests from `input` until it ends, writing one response per request.
pub(crate) fn serve<T: Transport>(
    transport: &mut T,
    input: impl BufRead,
    mut output: impl Write,
) -> std::io::Result<()> {
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let response = match argus_json::parse(line.as_bytes()) {
            Ok(message) => handle(transport, &message),
            Err(_) => Some(error(Json::Null, -32700, "Parse error")),
        };
        if let Some(response) = response {
            let mut text = argus_json::write(&response);
            text.push('\n');
            output.write_all(text.as_bytes())?;
            output.flush()?;
        }
    }
    Ok(())
}

fn obj(pairs: Vec<(&str, Json)>) -> Json {
    Json::Obj(pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect())
}

fn result(id: Json, result: Json) -> Json {
    obj(vec![
        ("jsonrpc", Json::str("2.0")),
        ("id", id),
        ("result", result),
    ])
}

fn error(id: Json, code: i64, message: &str) -> Json {
    obj(vec![
        ("jsonrpc", Json::str("2.0")),
        ("id", id),
        (
            "error",
            obj(vec![
                ("code", Json::Num(Number::from_i64(code))),
                ("message", Json::str(message)),
            ]),
        ),
    ])
}

fn tools() -> impl Iterator<Item = &'static OpInfo> {
    OPS.iter().filter(|op| op.access == OpAccess::Agent)
}

/// The response to one message; `None` for a notification.
fn handle<T: Transport>(transport: &mut T, message: &Json) -> Option<Json> {
    let id = message.get("id").cloned();
    let method = message.get("method").and_then(Json::as_str);
    let (Some(id), Some(method)) = (id, method) else {
        return match (message.get("id"), method) {
            // A notification (`notifications/initialized`, ...) is not answered.
            (None, Some(_)) => None,
            (id, _) => Some(error(
                id.cloned().unwrap_or(Json::Null),
                -32600,
                "Invalid Request",
            )),
        };
    };
    if message.get("jsonrpc").and_then(Json::as_str) != Some("2.0") {
        return Some(error(id, -32600, "Invalid Request"));
    }
    let params = message.get("params");
    let param = |name: &str| params.and_then(|p| p.get(name));
    Some(match method {
        "initialize" => {
            let asked = param("protocolVersion").and_then(Json::as_str);
            // The asked revision if it is one of ours, else our newest.
            let version = VERSIONS
                .iter()
                .find(|v| Some(**v) == asked)
                .unwrap_or(&VERSIONS[0]);
            result(
                id,
                obj(vec![
                    ("protocolVersion", Json::str(*version)),
                    ("capabilities", obj(vec![("tools", obj(vec![]))])),
                    (
                        "serverInfo",
                        obj(vec![
                            ("name", Json::str("argus")),
                            ("version", Json::str(env!("CARGO_PKG_VERSION"))),
                        ]),
                    ),
                ]),
            )
        }
        "ping" => result(id, obj(vec![])),
        "tools/list" => {
            let listed = tools()
                .map(|op| {
                    obj(vec![
                        ("name", Json::str(op.name)),
                        ("description", Json::str(op.doc.trim_end())),
                        ("inputSchema", (op.input_schema)()),
                    ])
                })
                .collect();
            result(id, obj(vec![("tools", Json::Arr(listed))]))
        }
        "tools/call" => {
            let name = param("name").and_then(Json::as_str).unwrap_or("");
            let Some(op) = tools().find(|op| op.name == name) else {
                return Some(error(id, -32602, &format!("Unknown tool: {name}")));
            };
            let input = param("arguments").cloned().unwrap_or(Json::Obj(Vec::new()));
            // A refusal is the tool's result, so the model reads why (MCP: tool
            // execution errors are reported with `isError`).
            let (value, failed) = match transport.call(op.name, input) {
                Ok(value) => (value, false),
                Err(e) => (e.to_json(), true),
            };
            let mut fields = vec![(
                "content",
                Json::Arr(vec![obj(vec![
                    ("type", Json::str("text")),
                    ("text", Json::str(argus_json::write(&value))),
                ])]),
            )];
            if !failed && matches!(value, Json::Obj(_)) {
                fields.push(("structuredContent", value));
            }
            fields.push(("isError", Json::Bool(failed)));
            result(id, obj(fields))
        }
        _ => error(id, -32601, "Method not found"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use argus_api::ApiError;

    /// Records each call and answers with what `reply` says.
    struct Fake {
        calls: Vec<(String, String)>,
        reply: fn(&str) -> Result<Json, ApiError>,
    }

    impl Transport for Fake {
        fn call(&mut self, op: &str, input: Json) -> Result<Json, ApiError> {
            self.calls.push((op.into(), argus_json::write(&input)));
            (self.reply)(op)
        }
    }

    fn fake(reply: fn(&str) -> Result<Json, ApiError>) -> Fake {
        Fake {
            calls: Vec::new(),
            reply,
        }
    }

    /// The response lines to `requests` (one per line).
    fn exchange(transport: &mut Fake, requests: &[&str]) -> Vec<Json> {
        let mut out = Vec::new();
        serve(transport, requests.join("\n").as_bytes(), &mut out).unwrap();
        let out = String::from_utf8(out).unwrap();
        out.lines()
            .map(|l| argus_json::parse(l.as_bytes()).unwrap())
            .collect()
    }

    fn at<'a>(v: &'a Json, path: &[&str]) -> &'a Json {
        path.iter().fold(v, |v, k| {
            v.get(k)
                .unwrap_or_else(|| panic!("no {k} in {}", argus_json::write(v)))
        })
    }

    #[test]
    fn the_handshake_agrees_on_a_revision_and_offers_tools() {
        let mut t = fake(|_| unreachable!("no call"));
        let replies = exchange(
            &mut t,
            &[
                r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"c","version":"1"}}}"#,
                r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
                r#"{"jsonrpc":"2.0","id":"p","method":"ping"}"#,
                r#"{"jsonrpc":"2.0","id":2,"method":"initialize","params":{"protocolVersion":"1999-01-01"}}"#,
            ],
        );
        // The notification gets no answer.
        assert_eq!(replies.len(), 3);
        assert_eq!(at(&replies[0], &["id"]), &Json::Num(Number::from_i64(1)));
        assert_eq!(
            at(&replies[0], &["result", "protocolVersion"]),
            &Json::str("2025-06-18")
        );
        assert!(
            at(&replies[0], &["result", "capabilities"])
                .get("tools")
                .is_some()
        );
        assert_eq!(at(&replies[1], &["id"]), &Json::str("p"));
        assert_eq!(at(&replies[1], &["result"]), &Json::Obj(Vec::new()));
        // A revision it does not know is answered with its newest.
        assert_eq!(
            at(&replies[2], &["result", "protocolVersion"]),
            &Json::str(VERSIONS[0])
        );
    }

    #[test]
    fn the_tools_are_the_ops_open_to_agents_with_their_schemas() {
        let mut t = fake(|_| unreachable!("no call"));
        let replies = exchange(
            &mut t,
            &[r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#],
        );
        let Json::Arr(listed) = at(&replies[0], &["result", "tools"]) else {
            panic!("tools is a list")
        };
        let names: Vec<&str> = listed
            .iter()
            .map(|t| at(t, &["name"]).as_str().unwrap())
            .collect();
        let open: Vec<&str> = tools().map(|op| op.name).collect();
        assert_eq!(names, open);
        assert!(names.contains(&"set_task") && !names.contains(&"merge"));
        let set_task = &listed[names.iter().position(|n| *n == "set_task").unwrap()];
        assert_eq!(
            at(set_task, &["inputSchema"]),
            &argus_api::vm::SetTask::schema()
        );
        assert!(!at(set_task, &["description"]).as_str().unwrap().is_empty());
    }

    #[test]
    fn a_tool_call_goes_to_the_host_and_a_refusal_is_the_tools_error() {
        let mut t = fake(|op| match op {
            "set_task" => Ok(argus_json::parse(br#"{"task":"t1"}"#).unwrap()),
            _ => Err(ApiError::NotFound {
                what: "job j".into(),
            }),
        });
        let replies = exchange(
            &mut t,
            &[
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"set_task","arguments":{"job":"j","title":"Parser"}}}"#,
                r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"request_check","arguments":{}}}"#,
                r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"merge","arguments":{}}}"#,
            ],
        );
        assert_eq!(
            t.calls,
            vec![
                ("set_task".into(), r#"{"job":"j","title":"Parser"}"#.into()),
                ("request_check".into(), "{}".into()),
            ]
        );
        assert_eq!(at(&replies[0], &["result", "isError"]), &Json::Bool(false));
        assert_eq!(
            at(&replies[0], &["result", "structuredContent", "task"]),
            &Json::str("t1")
        );
        let Json::Arr(content) = at(&replies[0], &["result", "content"]) else {
            panic!("content is a list")
        };
        assert_eq!(at(&content[0], &["text"]), &Json::str(r#"{"task":"t1"}"#));
        assert_eq!(at(&replies[1], &["result", "isError"]), &Json::Bool(true));
        assert!(replies[1].get("error").is_none());
        // Not a tool at all (agents may not merge): a protocol error, not a call.
        assert_eq!(
            at(&replies[2], &["error", "code"]),
            &Json::Num(Number::from_i64(-32602))
        );
    }

    #[test]
    fn malformed_messages_get_json_rpc_errors() {
        let mut t = fake(|_| unreachable!("no call"));
        let replies = exchange(
            &mut t,
            &[
                "{not json",
                r#"{"jsonrpc":"2.0","id":1,"method":"resources/list"}"#,
                r#"{"jsonrpc":"1.0","id":2,"method":"ping"}"#,
                r#"{"jsonrpc":"2.0","id":3}"#,
            ],
        );
        let codes: Vec<i64> = replies
            .iter()
            .map(|r| match at(r, &["error", "code"]) {
                Json::Num(n) => n.as_i64().unwrap(),
                _ => panic!("a code is a number"),
            })
            .collect();
        assert_eq!(codes, vec![-32700, -32601, -32600, -32600]);
        assert_eq!(at(&replies[0], &["id"]), &Json::Null);
    }
}
