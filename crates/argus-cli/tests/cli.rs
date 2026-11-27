//! The `argus` binary against a stand-in host that speaks the socket frames
//! (`argus_api::ClientFrame` / `ServerFrame`, one JSON object per line). The real host
//! serves the same frames (`argus-host` transport tests), so this checks the client half.
#![expect(
    clippy::disallowed_methods,
    reason = "an end-to-end test starts the argus binary"
)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread::JoinHandle;

use argus_json::Json;

fn scratch() -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "argus-cli-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A stand-in host for one client.
struct Host {
    socket: PathBuf,
    thread: JoinHandle<Vec<String>>,
}

impl Host {
    /// Accepts one client, reads `Hello` and one request, answers with `answer(id)` (one
    /// or more lines), and closes.
    fn serve(socket: &Path, answer: fn(u64) -> Vec<String>) -> Host {
        let listener = UnixListener::bind(socket).unwrap();
        let thread = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut seen = Vec::new();
            for line in BufReader::new(&stream).lines().take(2) {
                match line {
                    Ok(line) => seen.push(line),
                    Err(_) => return seen,
                }
            }
            let Some(id) = seen.get(1).and_then(|request| {
                match argus_json::parse(request.as_bytes()).ok()?.get("id")? {
                    Json::Num(n) => n.as_u64(),
                    _ => None,
                }
            }) else {
                return seen;
            };
            let mut w = &stream;
            for line in answer(id) {
                w.write_all(format!("{line}\n").as_bytes()).unwrap();
            }
            seen
        });
        Host {
            socket: socket.to_path_buf(),
            thread,
        }
    }

    /// The `Hello` and the request the client sent. A client that never connected gets
    /// the stand-in's accept released by a connection of the test's own, which sends
    /// nothing.
    fn seen(self) -> [String; 2] {
        drop(std::os::unix::net::UnixStream::connect(&self.socket));
        let seen = self.thread.join().unwrap();
        <[String; 2]>::try_from(seen).unwrap_or_else(|seen| panic!("the client sent {seen:?}"))
    }
}

/// `argus` with nothing from this environment but what the test names.
fn argus(socket: &Path, args: &[&str], env: &[(&str, &str)], stdin: &[u8]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_argus"))
        .args(args)
        .env_clear()
        .env("ARGUS_SOCKET", socket)
        .envs(env.iter().copied())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    child.wait_with_output().unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn a_call_prints_the_hosts_reply() {
    let dir = scratch();
    let socket = dir.join("s.sock");
    let host = Host::serve(&socket, |id| {
        vec![format!(
            r#"{{"kind":"Reply","id":{id},"ok":{{"default":"x"}},"err":null}}"#
        )]
    });
    let out = argus(&socket, &["call", "presets"], &[], b"");
    let [hello, request] = host.seen();
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), "{\"default\":\"x\"}\n");
    // Run by the user: no run, no token.
    assert_eq!(hello, r#"{"kind":"Hello","token":null,"run":null}"#);
    assert!(request.contains(r#""kind":"Call""#), "{request}");
    assert!(request.contains(r#""op":"presets""#), "{request}");
    assert!(request.contains(r#""input":{}"#), "{request}");
}

#[test]
fn an_agent_says_which_run_it_is_with_its_token() {
    let dir = scratch();
    let socket = dir.join("s.sock");
    let host = Host::serve(&socket, |id| {
        vec![format!(
            r#"{{"kind":"Reply","id":{id},"ok":{{"task":"t1"}},"err":null}}"#
        )]
    });
    let input = r#"{"title":"Parser"}"#;
    let env = [("ARGUS_RUN", "r1"), ("ARGUS_TOKEN", "secret")];
    let out = argus(&socket, &["call", "set_task", input], &env, b"");
    let [hello, request] = host.seen();
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(hello, r#"{"kind":"Hello","token":"secret","run":"r1"}"#);
    assert!(
        request.contains(r#""input":{"title":"Parser"}"#),
        "{request}"
    );
}

#[test]
fn a_refused_call_fails_and_says_why() {
    let dir = scratch();
    let socket = dir.join("s.sock");
    let host = Host::serve(&socket, |id| {
        vec![format!(
            r#"{{"kind":"Reply","id":{id},"ok":null,"err":{{"kind":"Forbidden","op":"merge"}}}}"#
        )]
    });
    let out = argus(&socket, &["call", "merge", r#"{"job":"j"}"#], &[], b"");
    host.seen();
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    let err = text(&out.stderr);
    assert!(err.contains("merge") && err.contains("Forbidden"), "{err}");
}

#[test]
fn a_watch_prints_each_value_the_host_pushes() {
    let dir = scratch();
    let socket = dir.join("s.sock");
    let host = Host::serve(&socket, |id| {
        vec![
            format!(r#"{{"kind":"Push","id":{id},"value":{{"n":1}}}}"#),
            format!(r#"{{"kind":"Push","id":{id},"value":{{"n":2}}}}"#),
        ]
    });
    let out = argus(&socket, &["watch", "overview"], &[], b"");
    let [_, request] = host.seen();
    assert!(request.contains(r#""kind":"Subscribe""#), "{request}");
    assert_eq!(text(&out.stdout), "{\"n\":1}\n{\"n\":2}\n");
}

#[test]
fn a_hook_sends_what_it_reads_verbatim() {
    let dir = scratch();
    let socket = dir.join("s.sock");
    let host = Host::serve(&socket, |id| {
        vec![format!(
            r#"{{"kind":"Reply","id":{id},"ok":{{"duplicate":false}},"err":null}}"#
        )]
    });
    let body = "{\"hook_event_name\":\"Stop\"}\n";
    let out = argus(&socket, &["hook"], &[], body.as_bytes());
    let [_, request] = host.seen();
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(request.contains(r#""op":"hook""#), "{request}");
    assert!(
        request.contains(r#""input":{"body":"{\"hook_event_name\":\"Stop\"}\n"}"#),
        "{request}"
    );
}

#[test]
fn without_a_host_the_cli_says_where_it_looked() {
    let dir = scratch();
    let socket = dir.join("none.sock");
    let out = argus(&socket, &["call", "presets"], &[], b"");
    assert_eq!(out.status.code(), Some(1));
    let err = text(&out.stderr);
    assert!(err.contains(socket.to_str().unwrap()), "{err}");
}

#[test]
fn bad_arguments_print_the_usage() {
    let dir = scratch();
    let socket = dir.join("none.sock");
    for args in [
        &[][..],
        &["frobnicate"],
        &["call"],
        &["call", "presets", "{"],
    ] {
        let out = argus(&socket, args, &[], b"");
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(text(&out.stderr).contains("usage"), "{args:?}");
    }
}

#[test]
fn ops_lists_every_op_with_its_kind_and_who_may_call_it() {
    let dir = scratch();
    let out = argus(&dir.join("none.sock"), &["ops"], &[], b"");
    assert!(out.status.success());
    let listed = text(&out.stdout);
    assert_eq!(listed.lines().count(), argus_api::OPS.len());
    assert!(listed.contains("set_task command agent"), "{listed}");
    assert!(listed.contains("overview subscribe human"), "{listed}");
}

#[test]
fn a_hello_the_host_refuses_is_reported_as_such() {
    let dir = scratch();
    let socket = dir.join("s.sock");
    // The host closes a connection whose run and token do not match, without a reply.
    let host = Host::serve(&socket, |_| Vec::new());
    let env = [("ARGUS_RUN", "r1"), ("ARGUS_TOKEN", "stale")];
    let out = argus(&socket, &["call", "presets"], &env, b"");
    host.seen();
    assert_eq!(out.status.code(), Some(1));
    let err = text(&out.stderr);
    assert!(err.contains("ARGUS_TOKEN"), "{err}");
}

#[test]
fn mcp_tools_reach_the_host_as_the_runs_agent() {
    let dir = scratch();
    let socket = dir.join("s.sock");
    let host = Host::serve(&socket, |id| {
        vec![format!(
            r#"{{"kind":"Reply","id":{id},"ok":{{"task":"t1"}},"err":null}}"#
        )]
    });
    let requests = [
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"c","version":"1"}}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"set_task","arguments":{"job":"j","title":"Parser"}}}"#,
    ]
    .join("\n");
    let env = [("ARGUS_RUN", "r1"), ("ARGUS_TOKEN", "secret")];
    let out = argus(&socket, &["mcp"], &env, requests.as_bytes());
    let [hello, request] = host.seen();
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(hello, r#"{"kind":"Hello","token":"secret","run":"r1"}"#);
    assert!(request.contains(r#""op":"set_task""#), "{request}");
    let replies = text(&out.stdout);
    let lines: Vec<&str> = replies.lines().collect();
    assert_eq!(lines.len(), 2, "{replies}");
    assert!(
        lines[0].contains(r#""protocolVersion":"2025-11-25""#),
        "{replies}"
    );
    assert!(
        lines[1].contains(r#""structuredContent":{"task":"t1"}"#),
        "{replies}"
    );
}

#[test]
fn an_op_takes_its_fields_as_arguments() {
    let dir = scratch();
    let socket = dir.join("s.sock");
    let host = Host::serve(&socket, |id| {
        vec![format!(
            r#"{{"kind":"Reply","id":{id},"ok":{{"runs":[]}},"err":null}}"#
        )]
    });
    let args = [
        "request-review",
        "j1",
        "--reviewers",
        "preset:quick",
        "--lens",
        "security",
        "--request-id",
        "r1",
    ];
    let out = argus(&socket, &args, &[], b"");
    let [_, request] = host.seen();
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(request.contains(r#""op":"request_review""#), "{request}");
    assert!(
        request.contains(
            r#""input":{"request_id":"r1","job":"j1","reviewers":{"kind":"Preset","name":"quick"},"lens":"Security","focus":null}"#
        ),
        "{request}"
    );
    assert_eq!(text(&out.stdout), "{\"runs\":[]}\n");
}

#[test]
fn a_watch_takes_arguments_too() {
    let dir = scratch();
    let socket = dir.join("s.sock");
    let host = Host::serve(&socket, |id| {
        vec![format!(r#"{{"kind":"Push","id":{id},"value":{{"n":1}}}}"#)]
    });
    let out = argus(&socket, &["watch", "job", "j1"], &[], b"");
    let [_, request] = host.seen();
    assert!(request.contains(r#""kind":"Subscribe""#), "{request}");
    assert!(request.contains(r#""input":{"job":"j1"}"#), "{request}");
    assert_eq!(text(&out.stdout), "{\"n\":1}\n");
}

#[test]
fn a_mistake_in_the_arguments_points_at_the_ops_help() {
    let dir = scratch();
    let socket = dir.join("none.sock");
    let out = argus(&socket, &["decide", "j1", "looks good"], &[], b"");
    assert_eq!(out.status.code(), Some(2));
    let err = text(&out.stderr);
    assert!(err.contains("missing --verdict"), "{err}");
    assert!(err.contains("argus help decide"), "{err}");

    let out = argus(&socket, &["help", "decide"], &[], b"");
    assert!(out.status.success(), "{}", text(&out.stderr));
    let help = text(&out.stdout);
    assert!(help.contains("argus decide <job> <note>"), "{help}");
    assert!(
        help.contains("--verdict approve|request-changes|abandon"),
        "{help}"
    );
}
