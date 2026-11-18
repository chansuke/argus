//! Identifiers and small value types. Every id that a client can submit is minted by the
//! submitter (CLI, app, policy), so a retried submission carries the same id and the
//! ledger can drop it as a duplicate (see `Event::dedup_key`).

use std::num::NonZeroU32;

pub use argus_log::{BlobId, EventId, Seq, Stamp};

/// 128-bit id. Random for human/agent submissions; derived (FNV-1a/128 over a canonical
/// key) for policy submissions, so the same policy decision always yields the same id.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Id128(pub u128);

impl Id128 {
    /// Wall time + pid + counter + `/dev/urandom`. Formats as a UUID so an agent run's
    /// id doubles as the Claude Code `--session-id`.
    pub fn mint() -> Self {
        use std::io::Read;
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);

        let mut random = [0u8; 16];
        // Without /dev/urandom the id is still unique: time, pid and counter go in too.
        let _ = std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut random));
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let mut key = Vec::with_capacity(16 + 16 + 4 + 8);
        key.extend_from_slice(&random);
        key.extend_from_slice(&now.to_le_bytes());
        key.extend_from_slice(&std::process::id().to_le_bytes());
        key.extend_from_slice(&COUNTER.fetch_add(1, Ordering::Relaxed).to_le_bytes());
        Id128(uuid_bits(
            fnv1a_128(&[&key]) ^ u128::from_le_bytes(random),
            4,
        ))
    }
    pub fn derive(namespace: &str, key: &[u8]) -> Self {
        let len = (namespace.len() as u64).to_le_bytes();
        Id128(uuid_bits(fnv1a_128(&[&len, namespace.as_bytes(), key]), 8))
    }
    /// The UUID text form, `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx` (either case).
    pub fn parse(s: &str) -> Option<Self> {
        let b = s.as_bytes();
        if b.len() != 36 || [8, 13, 18, 23].iter().any(|&i| b[i] != b'-') {
            return None;
        }
        let mut n = 0u128;
        for (i, &c) in b.iter().enumerate() {
            if matches!(i, 8 | 13 | 18 | 23) {
                continue;
            }
            n = n << 4 | u128::from((c as char).to_digit(16)?);
        }
        Some(Id128(n))
    }
}

impl std::fmt::Display for Id128 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let n = self.0;
        write!(
            f,
            "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
            n >> 96,
            n >> 80 & 0xFFFF,
            n >> 64 & 0xFFFF,
            n >> 48 & 0xFFFF,
            n & 0xFFFF_FFFF_FFFF
        )
    }
}

/// Sets the RFC 9562 version nibble and variant bits, so every id is a valid UUID.
fn uuid_bits(n: u128, version: u128) -> u128 {
    let n = n & !(0xF << 76) | version << 76;
    n & !(0x3 << 62) | 0x2 << 62
}

fn fnv1a_128(parts: &[&[u8]]) -> u128 {
    const OFFSET: u128 = 0x6c62272e07bb014262b821756295c58d;
    const PRIME: u128 = 0x0000000001000000000000000000013B;
    let mut h = OFFSET;
    for &b in parts.iter().flat_map(|p| p.iter()) {
        h ^= u128::from(b);
        h = h.wrapping_mul(PRIME);
    }
    h
}

macro_rules! id_newtype {
    ($($(#[$m:meta])* $name:ident;)*) => {$(
        $(#[$m])*
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
        pub struct $name(pub Id128);
    )*};
}

id_newtype! {
    JobId;
    /// One external effect: an agent session, a review, a check, a snapshot, ...
    RunId;
    /// One raw byte stream (a transcript file, a stdout pipe, a PTY).
    SourceId;
    ThreadId;
    CommentId;
    TaskKey;
    BookmarkId;
    InputId;
    /// A billed external resource (an EAS simulator session).
    LeaseId;
    /// One human verdict on a head snapshot (`Event::Decided`). Feedback delivery runs
    /// derive their `RunId` from it, so a delivery is requested at most once per decision.
    DecisionId;
}

impl DecisionId {
    /// The run that delivers this decision's feedback to the agent. Derived, so a
    /// delivery is requested at most once per decision (`policy::decide`), and the glance
    /// can find it.
    pub fn delivery_run(self) -> RunId {
        todo!("not implemented")
    }
}

/// Retry number of a run. Every attempt that starts also finishes exactly once
/// (possibly as `Abandoned` after a host crash).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Attempt(pub u16);

/// A decoded fact: frame `event`, part `part` (one raw record can decode into several
/// observations, e.g. an assistant message with thinking + text + two tool calls).
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct FactId {
    pub event: EventId,
    pub part: u16,
}

impl FactId {
    /// `<actor>:<seq>.<part>`, the form the API and stored references use.
    pub fn parse(s: &str) -> Option<Self> {
        let (event, part) = s.rsplit_once('.')?;
        Some(FactId {
            event: EventId::parse(event)?,
            part: part.parse().ok()?,
        })
    }
}

impl std::fmt::Display for FactId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.event, self.part)
    }
}

/// A finding inside a review run's result: the result fact + index within it.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct FindingId {
    pub fact: FactId,
    pub index: u16,
}

/// A skill candidate inside an insight run's result.
pub type CandidateId = FindingId;

/// Which ledger a cross-ledger reference points into.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum LedgerRef {
    Job(JobId),
    Meta,
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct FactRef {
    pub ledger: LedgerRef,
    pub fact: FactId,
}

/// Domain time in ms since the Unix epoch (UTC). Distinct from [`Stamp`], which is
/// append order: an imported 2025 transcript is stamped today but occurred in 2025.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct UnixMillis(pub i64);

impl UnixMillis {
    /// RFC 3339 date-time (`2026-10-24T03:16:30.255Z`, `...+09:00`). Fractions beyond
    /// milliseconds are truncated.
    pub fn parse_rfc3339(s: &str) -> Option<Self> {
        let b = s.as_bytes();
        let num = |r: std::ops::Range<usize>| -> Option<i64> {
            let d = b.get(r)?;
            d.iter()
                .all(u8::is_ascii_digit)
                .then(|| d.iter().fold(0, |n, &c| n * 10 + i64::from(c - b'0')))
        };
        let sep = |i: usize, c: u8| b.get(i).is_some_and(|x| x.eq_ignore_ascii_case(&c));
        if !(sep(4, b'-') && sep(7, b'-') && sep(10, b'T') && sep(13, b':') && sep(16, b':')) {
            return None;
        }
        let (year, month, day) = (num(0..4)?, num(5..7)?, num(8..10)?);
        let (hour, minute, second) = (num(11..13)?, num(14..16)?, num(17..19)?);
        let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
        let month_days = [
            31,
            if leap { 29 } else { 28 },
            31,
            30,
            31,
            30,
            31,
            31,
            30,
            31,
            30,
            31,
        ];
        if !(1..=12).contains(&month)
            || day < 1
            || day > month_days[month as usize - 1]
            || hour > 23
            || minute > 59
            || second > 60
        {
            return None;
        }
        let mut i = 19;
        let mut millis = 0;
        if sep(i, b'.') {
            i += 1;
            let start = i;
            while b.get(i).is_some_and(u8::is_ascii_digit) {
                i += 1;
            }
            if i == start {
                return None;
            }
            let digits = &s[start..i.min(start + 3)];
            millis = digits.parse::<i64>().ok()? * 10_i64.pow(3 - digits.len() as u32);
        }
        let offset_min = match b.get(i)? {
            b'Z' | b'z' if b.len() == i + 1 => 0,
            sign @ (b'+' | b'-') if b.len() == i + 6 && b[i + 3] == b':' => {
                let m = num(i + 1..i + 3)? * 60 + num(i + 4..i + 6)?;
                if *sign == b'+' { m } else { -m }
            }
            _ => return None,
        };
        // Days since 1970-01-01 in the proleptic Gregorian calendar (H. Hinnant).
        let y = if month <= 2 { year - 1 } else { year };
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let mp = (month + 9) % 12;
        let doy = (153 * mp + 2) / 5 + day - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        let days = era * 146_097 + doe - 719_468;
        let secs = days * 86_400 + hour * 3600 + minute * 60 + second - offset_min * 60;
        Some(UnixMillis(secs * 1000 + millis))
    }
}

/// Agent-native record id (Claude Code `uuid`). Records form a tree via `parentUuid`.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct RecordKey(pub String);

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct ToolCallId(pub String);

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct ToolName(pub String);

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct ModelId(pub String);

/// Hex object id of a git tree or commit (sha1 or sha256 repos). Validated on construction.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct GitOid(String);

impl GitOid {
    pub fn parse(hex: &str) -> Option<Self> {
        let ok = matches!(hex.len(), 40 | 64) && hex.bytes().all(|b| b.is_ascii_hexdigit());
        ok.then(|| GitOid(hex.to_ascii_lowercase()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A tree snapshot of a worktree. Review anchors and checks name trees, not times, so a
/// result is tied to exactly the content it judged.
pub type TreeOid = GitOid;
pub type CommitOid = GitOid;

/// Repo-relative, `/`-separated, normalized, never escaping the repo.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct RepoPath(String);

impl RepoPath {
    pub fn parse(p: &str) -> Option<Self> {
        let ok = !p.is_empty()
            && !p.starts_with('/')
            && p.split('/').all(|c| !c.is_empty() && c != "..");
        ok.then(|| RepoPath(p.to_owned()))
    }
    /// A path as an agent wrote it (absolute, or relative to its cwd = the worktree),
    /// made repo-relative. `None` when it points outside the worktree.
    pub fn within(worktree: &std::path::Path, agent_path: &str) -> Option<Self> {
        let p = std::path::Path::new(agent_path);
        let rel = if p.is_absolute() {
            p.strip_prefix(worktree).ok()?
        } else {
            p
        };
        RepoPath::parse(rel.to_str()?)
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Inclusive 1-based line range, `start <= end` by construction.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct LineRange {
    start: NonZeroU32,
    end: NonZeroU32,
}

impl LineRange {
    pub fn new(start: u32, end: u32) -> Option<Self> {
        let (s, e) = (NonZeroU32::new(start)?, NonZeroU32::new(end)?);
        (s <= e).then_some(LineRange { start: s, end: e })
    }
    pub fn start(self) -> u32 {
        self.start.get()
    }
    pub fn end(self) -> u32 {
        self.end.get()
    }
}

/// Kebab-case skill directory name, validated.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct SkillName(String);

impl SkillName {
    pub fn parse(s: &str) -> Option<Self> {
        let ok = !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        ok.then(|| SkillName(s.to_owned()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
