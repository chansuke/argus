//! What each run purpose becomes as a command. The flags are the ones the installed
//! tools accept (Claude Code 2.1.303 `--help`, TLC 2.18, tlars `-help`, lake 4.35); the
//! commands are not executed here.

use std::path::PathBuf;

use argus_core::command::{CommandSpec, Invocation, PlanEnv, SNAPSHOT_DENY, ToolPaths, Workdir};
use argus_core::event::{
    AgentLaunch, CheckRequest, CheckSpec, LeanMode, LlmCall, ReviewLens, ReviewSpec, RunPurpose,
    TlaEngine,
};
use argus_core::ids::{Attempt, GitOid, Id128, JobId, ModelId, RepoPath, RunId};
use argus_core::models::{AgentCli, Effort, ModelChoice};
use argus_core::period::{BucketWidth, Window};
use argus_core::plan::plan;
use argus_core::source::SourceKind;

const RUN: &str = "0f8fad5d-d9cb-469f-a165-70867728950e";
const JOB: &str = "00000000-0000-0000-0000-000000000001";

fn env() -> PlanEnv {
    PlanEnv {
        job: Some(JobId(Id128(1))),
        run: RunId(Id128::parse(RUN).unwrap()),
        attempt: Attempt(0),
        socket: PathBuf::from("/run/argus.sock"),
        tools: ToolPaths {
            argus: PathBuf::from("/bin/argus"),
            claude: PathBuf::from("/bin/claude"),
            codex: PathBuf::from("/bin/codex"),
            java: PathBuf::from("/bin/java"),
            tla2tools_jar: PathBuf::from("/share/tla2tools.jar"),
            tlars: PathBuf::from("/bin/tlars"),
            lake: PathBuf::from("/bin/lake"),
            eas: PathBuf::from("/bin/eas"),
            script: PathBuf::from("/bin/script"),
        },
        snapshot_deny: SNAPSHOT_DENY.iter().map(|s| s.to_string()).collect(),
    }
}

fn process(purpose: &RunPurpose) -> CommandSpec {
    match plan(purpose, &env()) {
        Invocation::Process(spec) => spec,
        other => panic!("{other:?}"),
    }
}

fn oid(c: char) -> GitOid {
    GitOid::parse(&c.to_string().repeat(40)).unwrap()
}

fn repo(p: &str) -> RepoPath {
    RepoPath::parse(p).unwrap()
}

fn args(spec: &CommandSpec) -> Vec<&str> {
    spec.args.iter().map(String::as_str).collect()
}

fn snapshot_workdir(spec: &CommandSpec) -> (GitOid, GitOid) {
    match &spec.workdir {
        Workdir::Snapshot { tree, base } => (tree.clone(), base.clone()),
        other => panic!("{other:?}"),
    }
}

/// The argus MCP server (`argus mcp`), as Claude Code takes it (`--mcp-config`).
const CLAUDE_MCP: &str = r#"{"mcpServers":{"argus":{"command":"/bin/argus","args":["mcp"]}}}"#;

/// The same server as codex `-c` overrides: it gets the run's identity from the agent's
/// environment, and its tools need no approval (exec cannot ask for one).
const CODEX_MCP: [&str; 4] = [
    r#"mcp_servers.argus.command="/bin/argus""#,
    r#"mcp_servers.argus.args=["mcp"]"#,
    r#"mcp_servers.argus.env_vars=["ARGUS_RUN","ARGUS_JOB","ARGUS_SOCKET","ARGUS_TOKEN"]"#,
    r#"mcp_servers.argus.default_tools_approval_mode="approve""#,
];

fn agent(prompt: &str, resume: Option<RunId>) -> RunPurpose {
    RunPurpose::Agent(AgentLaunch {
        cli: AgentCli::Claude,
        model: Some(ModelId("opus".into())),
        prompt: prompt.into(),
        resume,
    })
}

#[test]
fn a_claude_agent_runs_interactively_under_its_run_id() {
    let spec = process(&agent("fix the gate", None));
    assert_eq!(spec.program, PathBuf::from("/bin/claude"));
    assert_eq!(
        args(&spec),
        vec![
            "--mcp-config",
            CLAUDE_MCP,
            "--allowedTools",
            "mcp__argus",
            "--session-id",
            RUN,
            "--model",
            "opus",
            "fix the gate"
        ]
    );
    assert!(spec.terminal);
    assert!(matches!(spec.workdir, Workdir::JobWorktree));
    assert_eq!(spec.cwd, None);
    assert_eq!(spec.stdin, None);
    // The run id is the session id, so the transcript is found before it exists.
    let watched: Vec<_> = spec
        .watch
        .iter()
        .map(|w| (w.pattern.as_str(), w.kind))
        .collect();
    assert_eq!(
        watched,
        vec![
            (
                format!(".claude/projects/*/{RUN}.jsonl").as_str(),
                SourceKind::ClaudeTranscript
            ),
            (
                format!(".claude/projects/*/{RUN}/subagents/*.jsonl").as_str(),
                SourceKind::ClaudeTranscript
            ),
        ]
    );
    assert!(spec.env.contains(&("ARGUS_RUN".into(), RUN.into())));
    assert!(
        spec.env
            .contains(&("ARGUS_SOCKET".into(), "/run/argus.sock".into()))
    );
    // The agent names its own job in `argus` calls without being told it.
    assert!(spec.env.contains(&("ARGUS_JOB".into(), JOB.into())));
}

fn codex_agent(prompt: &str, resume: Option<RunId>) -> RunPurpose {
    RunPurpose::Agent(AgentLaunch {
        cli: AgentCli::Codex,
        model: Some(ModelId("gpt-6.2-sol".into())),
        prompt: prompt.into(),
        resume,
    })
}

#[test]
fn a_codex_agent_works_on_its_own_in_the_worktree_and_reports_on_stdout() {
    let spec = process(&codex_agent("fix the gate", None));
    assert_eq!(spec.program, PathBuf::from("/bin/codex"));
    assert_eq!(
        args(&spec),
        vec![
            "exec",
            "--sandbox",
            "workspace-write",
            "--json",
            "-c",
            CODEX_MCP[0],
            "-c",
            CODEX_MCP[1],
            "-c",
            CODEX_MCP[2],
            "-c",
            CODEX_MCP[3],
            "--model",
            "gpt-6.2-sol",
            "--skip-git-repo-check",
            "-",
        ]
    );
    assert_eq!(spec.stdin.as_deref(), Some(b"fix the gate".as_slice()));
    assert_eq!(
        (spec.stdout, spec.stderr),
        (SourceKind::CodexAgentExec, SourceKind::PlainText)
    );
    assert!(!spec.terminal);
    assert!(spec.watch.is_empty());
    assert!(matches!(spec.workdir, Workdir::JobWorktree));
    assert!(spec.env.contains(&("ARGUS_RUN".into(), RUN.into())));
    assert!(
        spec.env
            .contains(&("ARGUS_SOCKET".into(), "/run/argus.sock".into()))
    );
    // The agent names its own job in `argus` calls without being told it.
    assert!(spec.env.contains(&("ARGUS_JOB".into(), JOB.into())));
}

#[test]
fn codex_feedback_continues_the_latest_session_in_the_worktree() {
    let spec = process(&codex_agent(
        "- src/a.rs:2: keep the old name",
        Some(RunId(Id128(7))),
    ));
    assert_eq!(
        args(&spec),
        vec![
            "exec",
            "--sandbox",
            "workspace-write",
            "resume",
            "--last",
            "--json",
            "-c",
            CODEX_MCP[0],
            "-c",
            CODEX_MCP[1],
            "-c",
            CODEX_MCP[2],
            "-c",
            CODEX_MCP[3],
            "--model",
            "gpt-6.2-sol",
            "--skip-git-repo-check",
            "-",
        ]
    );
    assert_eq!(
        spec.stdin.as_deref(),
        Some(b"- src/a.rs:2: keep the old name".as_slice())
    );
}

#[test]
fn the_mcp_config_quotes_any_path_to_argus() {
    let mut env = env();
    env.tools.argus = PathBuf::from(r#"/opt/my "tools"\bin/argus"#);
    let arg_after = |purpose: &RunPurpose, flag: &str| -> String {
        let Invocation::Process(spec) = plan(purpose, &env) else {
            panic!()
        };
        let at = spec.args.iter().position(|a| a == flag).unwrap();
        spec.args[at + 1].clone()
    };
    let claude = arg_after(&agent("go", None), "--mcp-config");
    let parsed = argus_json::parse(claude.as_bytes()).unwrap();
    let command = parsed
        .get("mcpServers")
        .and_then(|s| s.get("argus"))
        .and_then(|s| s.get("command"))
        .and_then(argus_json::Json::as_str);
    assert_eq!(command, Some(r#"/opt/my "tools"\bin/argus"#));
    // A TOML basic string: quotes and backslashes escaped.
    assert_eq!(
        arg_after(&codex_agent("go", None), "-c"),
        r#"mcp_servers.argus.command="/opt/my \"tools\"\\bin/argus""#
    );
}

#[test]
fn feedback_resumes_a_copy_of_the_earlier_session_under_the_new_run_id() {
    let prev = RunId(Id128(7));
    let spec = process(&agent("- src/a.rs:2: keep the old name", Some(prev)));
    assert_eq!(
        args(&spec),
        vec![
            "--resume",
            prev.0.to_string().as_str(),
            "--fork-session",
            "--mcp-config",
            CLAUDE_MCP,
            "--allowedTools",
            "mcp__argus",
            "--session-id",
            RUN,
            "--model",
            "opus",
            "- src/a.rs:2: keep the old name",
        ]
    );
}

#[test]
fn a_reviewer_reads_a_checkout_of_the_snapshot_and_answers_on_stdout() {
    let review = RunPurpose::Review(ReviewSpec {
        tree: oid('1'),
        base: oid('b'),
        lens: ReviewLens::General,
        llm: LlmCall {
            model: ModelChoice {
                cli: AgentCli::Claude,
                model: ModelId("opus".into()),
                effort: Some(Effort::High),
            },
            prompt: "review this".into(),
        },
    });
    let spec = process(&review);
    assert_eq!(
        args(&spec),
        vec![
            "-p",
            "--model",
            "opus",
            "--effort",
            "high",
            "--output-format",
            "stream-json",
            "--verbose",
            "--no-session-persistence",
            "--tools",
            "Read,Grep,Glob,Bash",
            "--allowedTools",
            "Bash(git diff:*),Bash(git log:*),Bash(git show:*)",
        ]
    );
    assert_eq!(spec.stdin.as_deref(), Some(b"review this".as_slice()));
    assert_eq!(
        (spec.stdout, spec.stderr),
        (SourceKind::ClaudeStreamJson, SourceKind::PlainText)
    );
    assert!(!spec.terminal);
    assert_eq!(snapshot_workdir(&spec), (oid('1'), oid('b')));
}

#[test]
fn a_codex_reviewer_runs_exec_read_only_and_answers_on_stdout() {
    let review = |effort| {
        RunPurpose::Review(ReviewSpec {
            tree: oid('1'),
            base: oid('b'),
            lens: ReviewLens::General,
            llm: LlmCall {
                model: ModelChoice {
                    cli: AgentCli::Codex,
                    model: ModelId("gpt-6.2-sol".into()),
                    effort,
                },
                prompt: "review this".into(),
            },
        })
    };
    let spec = process(&review(Some(Effort::Max)));
    assert_eq!(spec.program, std::path::PathBuf::from("/bin/codex"));
    assert_eq!(
        args(&spec),
        vec![
            "exec",
            "--json",
            "--model",
            "gpt-6.2-sol",
            "-c",
            "model_reasoning_effort=\"xhigh\"",
            "--sandbox",
            "read-only",
            "--ephemeral",
            "--skip-git-repo-check",
            "--color",
            "never",
            "-",
        ]
    );
    assert_eq!(spec.stdin.as_deref(), Some(b"review this".as_slice()));
    assert_eq!(
        (spec.stdout, spec.stderr),
        (SourceKind::CodexExec, SourceKind::PlainText)
    );
    assert!(!spec.terminal);
    assert_eq!(snapshot_workdir(&spec), (oid('1'), oid('b')));
    // Without an effort, Codex's own default applies.
    assert!(!args(&process(&review(None))).contains(&"-c"));
}

#[test]
fn a_summary_gets_no_tools_at_all() {
    let summarize = RunPurpose::Summarize {
        window: Window {
            width: BucketWidth::M15,
            start: argus_core::ids::UnixMillis(0),
        },
        llm: LlmCall {
            model: ModelChoice {
                cli: AgentCli::Claude,
                model: ModelId("haiku".into()),
                effort: None,
            },
            prompt: "summarize".into(),
        },
    };
    let spec = process(&summarize);
    let a = args(&spec);
    let tools = a.iter().position(|x| *x == "--tools").unwrap();
    assert_eq!(a[tools + 1], "");
    assert!(!a.contains(&"--allowedTools"));
    assert!(matches!(spec.workdir, Workdir::Home));
}

fn tla(engine: TlaEngine, config: Option<&str>, workers: Option<u16>) -> RunPurpose {
    RunPurpose::Check(CheckRequest {
        tree: oid('1'),
        base: oid('b'),
        spec: CheckSpec::Tla {
            spec: repo("specs/tla/Lease.tla"),
            config: config.map(repo),
            engine,
            workers,
        },
    })
}

#[test]
fn tlc_runs_in_the_specs_directory_in_tool_mode() {
    let spec = process(&tla(
        TlaEngine::JavaTlc,
        Some("specs/tla/Lease.cfg"),
        Some(4),
    ));
    assert_eq!(spec.program, PathBuf::from("/bin/java"));
    assert_eq!(
        args(&spec),
        vec![
            "-XX:+UseParallelGC",
            "-cp",
            "/share/tla2tools.jar",
            "tlc2.TLC",
            "-tool",
            "-noGenerateSpecTE",
            "-workers",
            "4",
            "-config",
            "Lease.cfg",
            "Lease.tla",
        ]
    );
    assert_eq!(spec.cwd, Some(repo("specs/tla")));
    assert_eq!(
        (spec.stdout, spec.stderr),
        (SourceKind::TlcTool, SourceKind::PlainText)
    );
    assert_eq!(snapshot_workdir(&spec), (oid('1'), oid('b')));

    // tlars takes the same flags; a config elsewhere is named relative to the spec.
    let spec = process(&tla(TlaEngine::Tlars, Some("models/lease/Small.cfg"), None));
    assert_eq!(spec.program, PathBuf::from("/bin/tlars"));
    assert_eq!(
        args(&spec),
        vec![
            "-tool",
            "-noGenerateSpecTE",
            "-config",
            "../../models/lease/Small.cfg",
            "Lease.tla"
        ]
    );
}

fn lean(mode: LeanMode) -> RunPurpose {
    RunPurpose::Check(CheckRequest {
        tree: oid('1'),
        base: oid('b'),
        spec: CheckSpec::Lean {
            root: repo("proofs"),
            mode,
        },
    })
}

#[test]
fn lean_builds_in_the_package_directory() {
    let spec = process(&lean(LeanMode::LakeBuild));
    assert_eq!(spec.program, PathBuf::from("/bin/lake"));
    assert_eq!(args(&spec), vec!["build"]);
    assert_eq!(spec.cwd, Some(repo("proofs")));
    assert_eq!(spec.stdout, SourceKind::LeanText);
}

#[test]
fn lean_files_are_checked_one_by_one_and_the_worst_exit_wins() {
    let files = vec![
        repo("proofs/ArgusProofs/Holey.lean"),
        repo("proofs/ArgusProofs/Broken.lean"),
    ];
    let spec = process(&lean(LeanMode::Files(files)));
    assert_eq!(spec.program, PathBuf::from("sh"));
    let a = args(&spec);
    assert_eq!(a[0], "-c");
    // Positional arguments after `$0`: the files, relative to the package.
    assert_eq!(
        &a[2..],
        &["sh", "ArgusProofs/Holey.lean", "ArgusProofs/Broken.lean"]
    );
    assert!(a[1].contains(r#""$LAKE" env lean --json "$f""#), "{}", a[1]);
    assert!(spec.env.contains(&("LAKE".into(), "/bin/lake".into())));
    assert_eq!(spec.cwd, Some(repo("proofs")));
    assert_eq!(spec.stdout, SourceKind::LeanJson);
}
