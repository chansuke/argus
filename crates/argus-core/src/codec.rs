//! The stored JSON form of authored events. Logs keep these bytes forever, so the
//! format is pinned by a golden test, and every type reachable from `Event` is listed
//! here once.
//!
//! Structs are objects keyed by field name; a `None` field is left out. Enums are
//! externally tagged: `"Unit"` or `{"Variant": {...}}` / `{"Variant": value}`. The macros
//! destructure and construct every field, so a field or variant added to a type fails to
//! compile until it is listed here.

use std::num::NonZeroU32;
use std::path::PathBuf;

use argus_json::{Json, JsonError, Number};

use crate::diff::HunkAnchor;
use crate::event::*;
use crate::ids::*;
use crate::models::{AgentCli, Effort, ModelChoice};
use crate::period::Period;
use crate::views::timeline::{BucketWidth, Window};

pub(crate) fn encode(event: &Event) -> Vec<u8> {
    argus_json::write(&event.to_json()).into_bytes()
}

pub(crate) fn decode(bytes: &[u8]) -> Result<Event, JsonError> {
    let v = argus_json::parse(bytes)?;
    Event::from_json(&v).map_err(|what| JsonError { at: 0, what })
}

type R<T> = Result<T, String>;

trait Wire: Sized {
    fn to_json(&self) -> Json;
    fn from_json(v: &Json) -> R<Self>;
    /// As a struct field: `None` leaves the field out.
    fn to_field(&self) -> Option<Json> {
        Some(self.to_json())
    }
    fn from_field(v: Option<&Json>) -> R<Self> {
        Self::from_json(v.ok_or("missing field")?)
    }
}

impl<T: Wire> Wire for Option<T> {
    fn to_json(&self) -> Json {
        self.as_ref().map_or(Json::Null, T::to_json)
    }
    fn from_json(v: &Json) -> R<Self> {
        match v {
            Json::Null => Ok(None),
            v => T::from_json(v).map(Some),
        }
    }
    fn to_field(&self) -> Option<Json> {
        self.as_ref().map(T::to_json)
    }
    /// A present field is `Some`, even when it holds `null` (`Option<Option<T>>` stores
    /// "set to none" as `null` and "unchanged" as absent).
    fn from_field(v: Option<&Json>) -> R<Self> {
        match v {
            None => Ok(None),
            Some(v) => match T::from_json(v) {
                Ok(t) => Ok(Some(t)),
                Err(_) if *v == Json::Null => Ok(None),
                Err(e) => Err(e),
            },
        }
    }
}

impl<T: Wire> Wire for Vec<T> {
    fn to_json(&self) -> Json {
        Json::Arr(self.iter().map(T::to_json).collect())
    }
    fn from_json(v: &Json) -> R<Self> {
        match v {
            Json::Arr(items) => items.iter().map(T::from_json).collect(),
            _ => Err("expected an array".into()),
        }
    }
}

impl Wire for String {
    fn to_json(&self) -> Json {
        Json::Str(self.clone())
    }
    fn from_json(v: &Json) -> R<Self> {
        v.as_str()
            .map(str::to_owned)
            .ok_or_else(|| "expected a string".into())
    }
}

impl Wire for bool {
    fn to_json(&self) -> Json {
        Json::Bool(*self)
    }
    fn from_json(v: &Json) -> R<Self> {
        match v {
            Json::Bool(b) => Ok(*b),
            _ => Err("expected a boolean".into()),
        }
    }
}

macro_rules! wire_int {
    ($($t:ty),*) => {$(
        impl Wire for $t {
            fn to_json(&self) -> Json {
                Json::Num(Number::from_i64(i64::from(*self)))
            }
            fn from_json(v: &Json) -> R<Self> {
                let Json::Num(n) = v else { return Err("expected a number".into()) };
                n.as_i64()
                    .and_then(|n| <$t>::try_from(n).ok())
                    .ok_or_else(|| format!("not a {}", stringify!($t)))
            }
        }
    )*};
}
wire_int!(u8, u16, u32, i32, i64);

impl Wire for u64 {
    fn to_json(&self) -> Json {
        Json::Num(Number::from_u64(*self))
    }
    fn from_json(v: &Json) -> R<Self> {
        let Json::Num(n) = v else {
            return Err("expected a number".into());
        };
        n.as_u64().ok_or_else(|| "not a u64".into())
    }
}

/// Paths are stored as text; a non-UTF-8 path is stored lossily.
impl Wire for PathBuf {
    fn to_json(&self) -> Json {
        Json::Str(self.to_string_lossy().into_owned())
    }
    fn from_json(v: &Json) -> R<Self> {
        String::from_json(v).map(PathBuf::from)
    }
}

/// Types stored as one validated string.
macro_rules! wire_text {
    ($($t:ty => |$x:ident| $to:expr, |$s:ident| $from:expr;)*) => {$(
        impl Wire for $t {
            fn to_json(&self) -> Json {
                let $x = self;
                Json::Str($to)
            }
            fn from_json(v: &Json) -> R<Self> {
                let $s = v.as_str().ok_or("expected a string")?;
                $from.ok_or_else(|| format!("not a valid {}: {:?}", stringify!($t), $s))
            }
        }
    )*};
}

wire_text! {
    Id128 => |x| x.to_string(), |s| Id128::parse(s);
    GitOid => |x| x.as_str().to_owned(), |s| GitOid::parse(s);
    RepoPath => |x| x.as_str().to_owned(), |s| RepoPath::parse(s);
    SkillName => |x| x.as_str().to_owned(), |s| SkillName::parse(s);
    FactId => |x| x.to_string(), |s| FactId::parse(s);
    ModelId => |x| x.0.clone(), |s| Some(ModelId(s.to_owned()));
    BlobId => |x| x.0.iter().map(|b| format!("{b:02x}")).collect(), |s| parse_blob(s);
}

fn parse_blob(s: &str) -> Option<BlobId> {
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(s.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    Some(BlobId(out))
}

/// Newtypes stored as their inner value.
macro_rules! wire_newtype {
    ($($t:ident($inner:ty)),* $(,)?) => {$(
        impl Wire for $t {
            fn to_json(&self) -> Json {
                self.0.to_json()
            }
            fn from_json(v: &Json) -> R<Self> {
                <$inner>::from_json(v).map($t)
            }
        }
    )*};
}

wire_newtype!(
    JobId(Id128),
    RunId(Id128),
    SourceId(Id128),
    ThreadId(Id128),
    CommentId(Id128),
    TaskKey(Id128),
    BookmarkId(Id128),
    InputId(Id128),
    LeaseId(Id128),
    DecisionId(Id128),
    Attempt(u16),
    UnixMillis(i64),
);

impl Wire for Weight {
    fn to_json(&self) -> Json {
        self.0.get().to_json()
    }
    fn from_json(v: &Json) -> R<Self> {
        NonZeroU32::new(u32::from_json(v)?)
            .map(Weight)
            .ok_or_else(|| "a weight is at least 1".into())
    }
}

impl Wire for LineRange {
    fn to_json(&self) -> Json {
        obj(vec![
            ("start", Some(self.start().to_json())),
            ("end", Some(self.end().to_json())),
        ])
    }
    fn from_json(v: &Json) -> R<Self> {
        let f = fields_of(v)?;
        let (start, end) = (
            u32::from_field(field(f, "start"))?,
            u32::from_field(field(f, "end"))?,
        );
        LineRange::new(start, end).ok_or_else(|| format!("not a line range: {start}..{end}"))
    }
}

/// Minted only by the review view; decoding a stored one is the codec's other right.
impl Wire for ApprovedHead {
    fn to_json(&self) -> Json {
        obj(vec![
            ("tree", Some(self.tree().to_json())),
            ("base", Some(self.base().to_json())),
        ])
    }
    fn from_json(v: &Json) -> R<Self> {
        let f = fields_of(v)?;
        Ok(ApprovedHead::mint(
            GitOid::from_field(field(f, "tree"))?,
            GitOid::from_field(field(f, "base"))?,
        ))
    }
}

fn obj(fields: Vec<(&str, Option<Json>)>) -> Json {
    Json::Obj(
        fields
            .into_iter()
            .filter_map(|(k, v)| Some((k.to_owned(), v?)))
            .collect(),
    )
}

/// Unknown keys are ignored, so a field removed from a type still reads old logs.
fn fields_of(v: &Json) -> R<&[(String, Json)]> {
    match v {
        Json::Obj(fields) => Ok(fields),
        _ => Err("expected an object".into()),
    }
}

fn field<'a>(fields: &'a [(String, Json)], key: &str) -> Option<&'a Json> {
    fields.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

/// A variant with no stored fields is just its name.
fn tag(name: &str, inner: Json) -> Json {
    match inner {
        Json::Obj(fields) if fields.is_empty() => Json::str(name),
        inner => Json::Obj(vec![(name.to_owned(), inner)]),
    }
}

static NO_FIELDS: Json = Json::Obj(Vec::new());

fn tagged(v: &Json) -> R<(&str, &Json)> {
    match v {
        Json::Str(name) => Ok((name, &NO_FIELDS)),
        Json::Obj(fields) if fields.len() == 1 => Ok((&fields[0].0, &fields[0].1)),
        _ => Err("expected a variant".into()),
    }
}

macro_rules! wire_struct {
    ($($t:ident { $($f:ident),* $(,)? })*) => {$(
        impl Wire for $t {
            fn to_json(&self) -> Json {
                let $t { $($f),* } = self;
                obj(vec![$((stringify!($f), $f.to_field())),*])
            }
            fn from_json(v: &Json) -> R<Self> {
                let fields = fields_of(v)?;
                Ok($t {
                    $($f: Wire::from_field(field(fields, stringify!($f))).map_err(|e| {
                        format!("{}.{}: {e}", stringify!($t), stringify!($f))
                    })?,)*
                })
            }
        }
    )*};
}

macro_rules! wire_variant {
    (@pat $t:ident $v:ident { $($f:ident),* }) => { $t::$v { $($f),* } };
    (@pat $t:ident $v:ident ( $x:ident )) => { $t::$v($x) };
    (@enc $v:ident { $($f:ident),* }) => {
        tag(stringify!($v), obj(vec![$((stringify!($f), $f.to_field())),*]))
    };
    (@enc $v:ident ( $x:ident )) => { tag(stringify!($v), $x.to_json()) };
    (@dec $t:ident $v:ident { $($f:ident),* } $inner:ident) => {{
        let fields = fields_of($inner)?;
        Ok($t::$v {
            $($f: Wire::from_field(field(fields, stringify!($f))).map_err(|e| {
                format!("{}::{}.{}: {e}", stringify!($t), stringify!($v), stringify!($f))
            })?,)*
        })
    }};
    (@dec $t:ident $v:ident ( $x:ident ) $inner:ident) => {
        Wire::from_json($inner).map($t::$v)
    };
}

macro_rules! wire_enum {
    ($($t:ident { $($v:ident $body:tt),* $(,)? })*) => {$(
        impl Wire for $t {
            fn to_json(&self) -> Json {
                match self {
                    $(wire_variant!(@pat $t $v $body) => wire_variant!(@enc $v $body),)*
                }
            }
            fn from_json(v: &Json) -> R<Self> {
                let (name, inner) = tagged(v)?;
                $(if name == stringify!($v) {
                    return wire_variant!(@dec $t $v $body inner);
                })*
                Err(format!("unknown {} `{name}`", stringify!($t)))
            }
        }
    )*};
}

wire_struct! {
    JobInfo { job, title, repo, base, target, worktree, group, origin }
    AgentLaunch { cli, model, prompt, resume }
    LlmCall { model, prompt }
    ModelChoice { cli, model, effort }
    ReviewSpec { tree, base, lens, llm }
    CheckRequest { tree, base, spec }
    DeviceBudget { max_minutes, max_idle_minutes }
    Snapshot { tree, base, previous, delta, files, withheld }
    FileDelta { path, renamed_from, hunks }
    FileState { path, blob, hunks }
    Hunk { old_start, old_len, new_start, new_len }
    HunkAnchor { path, fingerprint }
    TaskPatch { parent, title, weight, state }
    SkillDraft { from, name, description, body, target }
    FindingId { fact, index }
    Period { from, to }
    Window { width, start }
}

wire_enum! {
    Event {
        JobOpened(info),
        RunRequested { run, purpose },
        RunStarted { run, attempt, pid },
        RunInput { input, run, bytes },
        RunCancelRequested { run },
        RunFinished { run, attempt, outcome },
        LeaseOpened { lease, run, attempt, provider, name },
        LeaseBound { lease, remote_id, preview_url },
        LeaseClosed { lease, how },
        ArtifactCaptured { run, attempt, blob, name, media_type, len },
        SourcePruned { source },
        ThreadOpened { thread, comment, anchor, body },
        Replied { comment, thread, body },
        ThreadResolved { thread, resolved },
        FindingPromoted { finding, thread, comment },
        FindingDismissed { finding, dismissed },
        Decided { decision, tree, verdict, note },
        TaskSet { task, patch },
        Bookmarked { bookmark, at, kind, note },
        CandidateDismissed { candidate },
    }
    JobOrigin {
        Launched {},
        Imported { transcript },
    }
    Decision {
        Approve {},
        RequestChanges {},
        Abandon {},
    }
    RunPurpose {
        Agent(launch),
        Snapshot { after },
        Review(spec),
        Check(request),
        Merge(head),
        Summarize { window, llm },
        Insight { period, llm },
        WriteSkill(draft),
        Import { transcript, cli },
    }
    AgentCli {
        Claude {},
        Codex {},
    }
    Effort {
        Low {},
        Medium {},
        High {},
        Max {},
    }
    ReviewLens {
        General {},
        Correctness {},
        Security {},
        Tests {},
    }
    CheckSpec {
        Tla { spec, config, engine, workers },
        Lean { root, mode },
        Device { project, platform, app, driver, budget },
    }
    TlaEngine {
        JavaTlc {},
        Tlars {},
    }
    LeanMode {
        LakeBuild {},
        Files(paths),
    }
    DevicePlatform {
        Ios {},
        Android {},
    }
    AppSource {
        BuildId(id),
        Fingerprint(hash),
        ArchiveUrl(url),
        ExpoGo { sdk },
    }
    DeviceDriver {
        Script(path),
        Agent(llm),
    }
    LeaseProvider {
        EasSimulator { platform },
    }
    LeaseEnd {
        Stopped {},
        Gone {},
    }
    RunOutcome {
        Exited { code },
        Killed { signal, cancelled },
        SpawnFailed { error },
        Abandoned {},
        Snapshot(snapshot),
        Merged { commit },
        SkillWritten { path },
        Failed { reason },
    }
    Anchor {
        Code { tree, path, lines },
        Task(task),
        Fact(fact),
        Window(window),
    }
    TaskState {
        Pending {},
        InProgress {},
        Blocked { reason },
        Done { result },
        Dropped {},
    }
    BookmarkKind {
        Idea {},
        Improvement {},
        Pitfall {},
        Exemplar {},
    }
    BucketWidth {
        M15 {},
        H1 {},
        H4 {},
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;
    use std::path::PathBuf;

    use super::{decode, encode};
    use crate::event::*;
    use crate::ids::*;
    use crate::models::{AgentCli, Effort, ModelChoice};
    use crate::period::Period;
    use crate::views::timeline::{BucketWidth, Window};

    fn id(n: u128) -> Id128 {
        Id128(n)
    }
    fn tree(c: char) -> TreeOid {
        GitOid::parse(&c.to_string().repeat(40)).unwrap()
    }
    fn path(p: &str) -> RepoPath {
        RepoPath::parse(p).unwrap()
    }
    fn fact() -> FactId {
        FactId::parse("agent-1:3.1").unwrap()
    }
    fn model(m: &str, effort: Option<Effort>) -> ModelChoice {
        ModelChoice {
            cli: AgentCli::Claude,
            model: ModelId(m.into()),
            effort,
        }
    }
    fn llm() -> LlmCall {
        LlmCall {
            model: model("claude-opus-5-5", Some(Effort::High)),
            prompt: "review \"this\"\n".into(),
        }
    }
    fn run(n: u128) -> RunId {
        RunId(id(n))
    }

    /// Exhaustive on purpose: a new variant does not compile until it is listed here and
    /// gets a sample below.
    fn variant(e: &Event) -> usize {
        match e {
            Event::JobOpened(_) => 0,
            Event::RunRequested { .. } => 1,
            Event::RunStarted { .. } => 2,
            Event::RunInput { .. } => 3,
            Event::RunCancelRequested { .. } => 4,
            Event::RunFinished { .. } => 5,
            Event::LeaseOpened { .. } => 6,
            Event::LeaseBound { .. } => 7,
            Event::LeaseClosed { .. } => 8,
            Event::ArtifactCaptured { .. } => 9,
            Event::SourcePruned { .. } => 10,
            Event::ThreadOpened { .. } => 11,
            Event::Replied { .. } => 12,
            Event::ThreadResolved { .. } => 13,
            Event::FindingPromoted { .. } => 14,
            Event::FindingDismissed { .. } => 15,
            Event::Decided { .. } => 16,
            Event::TaskSet { .. } => 17,
            Event::Bookmarked { .. } => 18,
            Event::CandidateDismissed { .. } => 19,
        }
    }
    const VARIANTS: usize = 20;

    fn samples() -> Vec<Event> {
        let finding = FindingId {
            fact: fact(),
            index: 2,
        };
        let purposes = vec![
            RunPurpose::Agent(AgentLaunch {
                cli: AgentCli::Claude,
                model: None,
                prompt: "build argus-log".into(),
                resume: Some(run(9)),
            }),
            RunPurpose::Snapshot {
                after: Some(fact()),
            },
            RunPurpose::Snapshot { after: None },
            RunPurpose::Review(ReviewSpec {
                tree: tree('a'),
                base: tree('b'),
                lens: ReviewLens::Security,
                llm: llm(),
            }),
            RunPurpose::Check(CheckRequest {
                tree: tree('a'),
                base: tree('b'),
                spec: CheckSpec::Tla {
                    spec: path("specs/tla/Lease.tla"),
                    config: Some(path("specs/tla/Lease.cfg")),
                    engine: TlaEngine::Tlars,
                    workers: Some(4),
                },
            }),
            RunPurpose::Check(CheckRequest {
                tree: tree('a'),
                base: tree('b'),
                spec: CheckSpec::Lean {
                    root: path("proofs"),
                    mode: LeanMode::Files(vec![path("proofs/A.lean")]),
                },
            }),
            RunPurpose::Check(CheckRequest {
                tree: tree('a'),
                base: tree('b'),
                spec: CheckSpec::Device {
                    project: path("ui/apps/mobile"),
                    platform: DevicePlatform::Ios,
                    app: AppSource::ExpoGo { sdk: None },
                    driver: DeviceDriver::Agent(llm()),
                    budget: DeviceBudget {
                        max_minutes: 15,
                        max_idle_minutes: 3,
                    },
                },
            }),
            RunPurpose::Merge(ApprovedHead::mint(tree('a'), tree('b'))),
            RunPurpose::Summarize {
                window: Window {
                    width: BucketWidth::H1,
                    start: UnixMillis(3_600_000),
                },
                llm: llm(),
            },
            RunPurpose::Insight {
                period: Period {
                    from: UnixMillis(0),
                    to: UnixMillis(-1),
                },
                llm: llm(),
            },
            RunPurpose::WriteSkill(SkillDraft {
                from: Some(finding.clone()),
                name: SkillName::parse("verify-first").unwrap(),
                description: "d".into(),
                body: "b".into(),
                target: PathBuf::from("/w/.claude/skills"),
            }),
            RunPurpose::Import {
                transcript: PathBuf::from("/h/.claude/projects/x/y.jsonl"),
                cli: AgentCli::Codex,
            },
        ];
        let outcomes = vec![
            RunOutcome::Exited { code: -1 },
            RunOutcome::Killed {
                signal: 9,
                cancelled: true,
            },
            RunOutcome::SpawnFailed {
                error: "ENOENT".into(),
            },
            RunOutcome::Abandoned,
            RunOutcome::Snapshot(Snapshot {
                tree: tree('c'),
                base: tree('b'),
                previous: None,
                delta: vec![FileDelta {
                    path: path("a.rs"),
                    renamed_from: Some(path("b.rs")),
                    hunks: vec![Hunk {
                        old_start: 1,
                        old_len: 0,
                        new_start: 1,
                        new_len: 3,
                    }],
                }],
                files: vec![FileState {
                    path: path("a.rs"),
                    blob: None,
                    hunks: vec![crate::diff::HunkAnchor {
                        path: path("a.rs"),
                        fingerprint: u64::MAX,
                    }],
                }],
                withheld: vec![path(".env")],
            }),
            RunOutcome::Merged { commit: tree('d') },
            RunOutcome::SkillWritten {
                path: PathBuf::from("/w/SKILL.md"),
            },
            RunOutcome::Failed {
                reason: "target moved".into(),
            },
        ];
        let anchors = vec![
            Anchor::Code {
                tree: tree('a'),
                path: path("crates/argus-log/src/frame.rs"),
                lines: LineRange::new(3, 9).unwrap(),
            },
            Anchor::Task(TaskKey(id(4))),
            Anchor::Fact(fact()),
            Anchor::Window(Window {
                width: BucketWidth::M15,
                start: UnixMillis(0),
            }),
        ];

        let mut events = vec![
            Event::JobOpened(JobInfo {
                job: JobId(id(1)),
                title: "argus-log".into(),
                repo: PathBuf::from("/w"),
                base: tree('b'),
                target: "main".into(),
                worktree: PathBuf::from("/w/.argus/wt/1"),
                group: Some(id(2)),
                origin: JobOrigin::Imported {
                    transcript: PathBuf::from("/h/t.jsonl"),
                },
            }),
            Event::RunStarted {
                run: run(1),
                attempt: Attempt(0),
                pid: None,
            },
            Event::RunInput {
                input: InputId(id(3)),
                run: run(1),
                bytes: vec![0, 0x1b, b'y', 0xff],
            },
            Event::RunCancelRequested { run: run(1) },
            Event::LeaseOpened {
                lease: LeaseId(id(5)),
                run: run(1),
                attempt: Attempt(2),
                provider: LeaseProvider::EasSimulator {
                    platform: DevicePlatform::Android,
                },
                name: "argus-5".into(),
            },
            Event::LeaseBound {
                lease: LeaseId(id(5)),
                remote_id: "sim-1".into(),
                preview_url: None,
            },
            Event::LeaseClosed {
                lease: LeaseId(id(5)),
                how: LeaseEnd::Gone,
            },
            Event::ArtifactCaptured {
                run: run(1),
                attempt: Attempt(0),
                blob: BlobId([0xab; 32]),
                name: "home.png".into(),
                media_type: "image/png".into(),
                len: 12,
            },
            Event::SourcePruned {
                source: SourceId(id(6)),
            },
            Event::Replied {
                comment: CommentId(id(8)),
                thread: ThreadId(id(7)),
                body: "ok".into(),
            },
            Event::ThreadResolved {
                thread: ThreadId(id(7)),
                resolved: false,
            },
            Event::FindingPromoted {
                finding: finding.clone(),
                thread: ThreadId(id(7)),
                comment: CommentId(id(9)),
            },
            Event::FindingDismissed {
                finding: finding.clone(),
                dismissed: true,
            },
            Event::Decided {
                decision: DecisionId(id(10)),
                tree: None,
                verdict: Decision::Abandon,
                note: String::new(),
            },
            Event::Decided {
                decision: DecisionId(id(11)),
                tree: Some(tree('a')),
                verdict: Decision::RequestChanges,
                note: "see threads".into(),
            },
            Event::TaskSet {
                task: TaskKey(id(4)),
                patch: TaskPatch::default(),
            },
            Event::TaskSet {
                task: TaskKey(id(4)),
                patch: TaskPatch {
                    parent: Some(None),
                    title: Some("t".into()),
                    weight: Some(Weight(NonZeroU32::new(3).unwrap())),
                    state: Some(TaskState::Blocked { reason: "r".into() }),
                },
            },
            Event::TaskSet {
                task: TaskKey(id(4)),
                patch: TaskPatch {
                    parent: Some(Some(TaskKey(id(12)))),
                    title: None,
                    weight: None,
                    state: Some(TaskState::Done { result: None }),
                },
            },
            Event::Bookmarked {
                bookmark: BookmarkId(id(13)),
                at: fact(),
                kind: BookmarkKind::Pitfall,
                note: "n".into(),
            },
            Event::CandidateDismissed { candidate: finding },
        ];
        events.extend(
            purposes
                .into_iter()
                .enumerate()
                .map(|(i, purpose)| Event::RunRequested {
                    run: run(100 + i as u128),
                    purpose,
                }),
        );
        events.extend(outcomes.into_iter().map(|outcome| Event::RunFinished {
            run: run(1),
            attempt: Attempt(0),
            outcome,
        }));
        events.extend(
            anchors
                .into_iter()
                .enumerate()
                .map(|(i, anchor)| Event::ThreadOpened {
                    thread: ThreadId(id(200 + i as u128)),
                    comment: CommentId(id(300 + i as u128)),
                    anchor,
                    body: "why?".into(),
                }),
        );
        events
    }

    #[test]
    fn every_variant_has_a_sample() {
        let mut seen = [false; VARIANTS];
        for e in samples() {
            seen[variant(&e)] = true;
        }
        assert!(seen.iter().all(|&s| s), "{seen:?}");
    }

    #[test]
    fn events_round_trip_through_their_stored_form() {
        for e in samples() {
            let bytes = encode(&e);
            let back = decode(&bytes)
                .unwrap_or_else(|err| panic!("{}: {err:?}", String::from_utf8_lossy(&bytes)));
            assert_eq!(back, e, "{}", String::from_utf8_lossy(&bytes));
        }
    }

    /// Stored logs are never rewritten, so a change here breaks every existing store.
    #[test]
    fn the_stored_form_is_stable() {
        let e = Event::Decided {
            decision: DecisionId(id(0x0f8f_ad5d_d9cb_469f_a165_7086_7728_950e)),
            tree: Some(tree('a')),
            verdict: Decision::Approve,
            note: "lgtm".into(),
        };
        assert_eq!(
            String::from_utf8(encode(&e)).unwrap(),
            r#"{"Decided":{"decision":"0f8fad5d-d9cb-469f-a165-70867728950e","tree":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","verdict":"Approve","note":"lgtm"}}"#
        );
        let patch = Event::TaskSet {
            task: TaskKey(id(1)),
            patch: TaskPatch {
                parent: Some(None),
                title: None,
                weight: None,
                state: Some(TaskState::Dropped),
            },
        };
        assert_eq!(
            String::from_utf8(encode(&patch)).unwrap(),
            r#"{"TaskSet":{"task":"00000000-0000-0000-0000-000000000001","patch":{"parent":null,"state":"Dropped"}}}"#
        );
    }

    #[test]
    fn invalid_stored_values_are_rejected() {
        for bad in [
            &b"{}"[..],
            br#"{"Nope":{}}"#,
            br#"{"SourcePruned":{}}"#,
            br#"{"SourcePruned":{"source":"not-a-uuid"}}"#,
            br#"{"Decided":{"decision":"0f8fad5d-d9cb-469f-a165-70867728950e","tree":"xyz","verdict":"Approve","note":""}}"#,
            br#"{"RunStarted":{"run":"0f8fad5d-d9cb-469f-a165-70867728950e","attempt":70000}}"#,
            br#"{"ThreadOpened":{"thread":"0f8fad5d-d9cb-469f-a165-70867728950e","comment":"0f8fad5d-d9cb-469f-a165-70867728950e","anchor":{"Code":{"tree":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","path":"a.rs","lines":{"start":5,"end":2}}},"body":""}}"#,
        ] {
            assert!(decode(bad).is_err(), "{}", String::from_utf8_lossy(bad));
        }
    }
}
