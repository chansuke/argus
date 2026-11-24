//! What happened when, across every lane of a job.
//!
//! Items are keyed by domain time, then fact id: insertion of an out-of-order fact (an
//! imported old transcript, a late subagent file) is O(log N), and identity is the fact
//! id, so nothing is ever searched for duplicates. Roll-ups are computed from items on read; only the
//! LLM-written text of a window is stored (as a `Summarize` run's output).

use std::collections::{BTreeMap, BTreeSet};

use crate::event::{Author, Event, RunOutcome, RunPurpose};
use crate::fact::{Fact, FactBody};
use crate::ids::*;
use crate::period::{BucketWidth, Window};
use crate::source::{AgentWhat, LeanSeverity, Observation, SourceFamily, SourceMeta};
use crate::views::Projection;

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Lane {
    /// One coding agent (its subagents render as nested sub-lanes via `sidechain`).
    Agent(RunId),
    /// Review / check / summary / device runs.
    Run(RunId),
    Human,
    Host,
}

#[derive(Clone, Debug)]
pub struct Item {
    pub lane: Lane,
    pub kind: ItemKind,
    /// One line; the raw view has the rest.
    pub title: String,
    /// Set when the matching end arrives (tool result, run finished).
    pub ended_at: Option<UnixMillis>,
}

#[derive(Clone, Debug)]
pub enum ItemKind {
    Prompt,
    Reply,
    Thinking,
    ToolCall {
        name: ToolName,
        ok: Option<bool>,
    },
    PlanChange,
    Snapshot,
    RunLifecycle,
    Finding,
    CheckVerdict,
    Artifact,
    Comment,
    TaskChange,
    HumanInput,
    Decision,
    /// The agent rewrote its transcript; the tailer continued after a marker.
    Discontinuity,
    Unrecognized,
}

#[derive(Default, Debug)]
pub struct Timeline {
    /// Two levels so a time range needs no "smallest FactId" sentinel.
    items: BTreeMap<UnixMillis, BTreeMap<FactId, Item>>,
    /// Open tool calls -> their item key, so a result closes its call in O(log N).
    open_calls: BTreeMap<ToolCallId, (UnixMillis, FactId)>,
    /// Summarize run -> window; window -> result fact once the run finished.
    summary_runs: BTreeMap<RunId, Window>,
    summaries: BTreeMap<Window, FactId>,
    /// Agent runs; every other run renders in a `Lane::Run`.
    agents: BTreeSet<RunId>,
    /// Started attempts -> their start item, closed when the attempt finishes.
    open_runs: BTreeMap<(RunId, Attempt), (UnixMillis, FactId)>,
}

#[derive(Clone, Debug)]
pub struct Bucket {
    pub window: Window,
    pub counts: BTreeMap<Lane, LaneCounts>,
    pub summary: Option<FactId>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct LaneCounts {
    pub tool_calls: u32,
    pub tool_errors: u32,
    pub replies: u32,
    pub human_inputs: u32,
}

impl Timeline {
    /// Items with `from <= at < to`, in (at, fact) order.
    pub fn range(
        &self,
        from: UnixMillis,
        to: UnixMillis,
    ) -> impl Iterator<Item = (UnixMillis, &FactId, &Item)> {
        self.items
            .range(from..to)
            .flat_map(|(at, items)| items.iter().map(move |(f, i)| (*at, f, i)))
    }
    /// M15 buckets come from items; H1 folds four M15, H4 folds four H1 (dsh-style
    /// roll-up, but derived, so it can never disagree with the items).
    ///
    /// Every window that overlaps `from..to` is returned whole, so a bucket's counts never
    /// depend on where the range was cut. Windows with no items and no summary are left out.
    pub fn buckets(&self, width: BucketWidth, from: UnixMillis, to: UnixMillis) -> Vec<Bucket> {
        if from >= to {
            return Vec::new();
        }
        let start = Window::containing(width, from).start;
        let to = Window::containing(width, UnixMillis(to.0 - 1)).end();
        let mut out: BTreeMap<Window, Bucket> = BTreeMap::new();
        for (at, _, item) in self.range(start, to) {
            let window = Window::containing(width, at);
            let bucket = out.entry(window).or_insert_with(|| Bucket {
                window,
                counts: BTreeMap::new(),
                summary: None,
            });
            let counts = bucket.counts.entry(item.lane.clone()).or_default();
            match &item.kind {
                ItemKind::ToolCall { ok, .. } => {
                    counts.tool_calls += 1;
                    counts.tool_errors += u32::from(*ok == Some(false));
                }
                ItemKind::Reply => counts.replies += 1,
                ItemKind::HumanInput => counts.human_inputs += 1,
                _ => {}
            }
        }
        let summarized = self
            .summaries
            .range(Window { width, start }..)
            .take_while(|(w, _)| w.start < to)
            .filter(|(w, _)| w.width == width);
        for (window, fact) in summarized {
            out.entry(*window)
                .or_insert_with(|| Bucket {
                    window: *window,
                    counts: BTreeMap::new(),
                    summary: None,
                })
                .summary = Some(fact.clone());
        }
        out.into_values().collect()
    }
    /// Windows that ended, had activity, and have no summary run yet (policy input).
    pub fn unsummarized(&self, width: BucketWidth, now: UnixMillis) -> Vec<Window> {
        let requested: BTreeSet<&Window> = self.summary_runs.values().collect();
        let mut out: Vec<Window> = Vec::new();
        for at in self.items.keys() {
            let window = Window::containing(width, *at);
            if window.end() > now {
                break;
            }
            if out.last() != Some(&window) && !requested.contains(&window) {
                out.push(window);
            }
        }
        out
    }

    fn insert(&mut self, at: UnixMillis, id: FactId, item: Item) {
        self.items.entry(at).or_default().insert(id, item);
    }

    fn item_mut(&mut self, (at, id): &(UnixMillis, FactId)) -> Option<&mut Item> {
        self.items.get_mut(at)?.get_mut(id)
    }

    fn run_lane(&self, run: RunId) -> Lane {
        if self.agents.contains(&run) {
            Lane::Agent(run)
        } else {
            Lane::Run(run)
        }
    }

    fn author_lane(author: &Author) -> Lane {
        match author {
            Author::Human => Lane::Human,
            Author::Agent(run) => Lane::Agent(*run),
            Author::Host | Author::Policy => Lane::Host,
        }
    }

    fn authored(&mut self, fact: &Fact<'_>, author: &Author, event: &Event) -> bool {
        let at = fact.at;
        let item = |lane, kind, title: String| Item {
            lane,
            kind,
            title,
            ended_at: None,
        };
        let new = match event {
            Event::JobOpened(_) => item(Lane::Host, ItemKind::RunLifecycle, "job opened".into()),
            Event::RunRequested { run, purpose } => {
                match purpose {
                    RunPurpose::Agent(_) => {
                        self.agents.insert(*run);
                    }
                    RunPurpose::Summarize { window, .. } => {
                        self.summary_runs.insert(*run, *window);
                    }
                    _ => {}
                }
                item(
                    self.run_lane(*run),
                    ItemKind::RunLifecycle,
                    format!("requested {}", purpose_name(purpose)),
                )
            }
            Event::RunStarted { run, attempt, .. } => {
                self.open_runs
                    .insert((*run, *attempt), (at, fact.id.clone()));
                item(
                    self.run_lane(*run),
                    ItemKind::RunLifecycle,
                    format!("attempt {} started", attempt.0),
                )
            }
            Event::RunInput { run, bytes, .. } => item(
                Lane::Human,
                ItemKind::HumanInput,
                first_line(&String::from_utf8_lossy(bytes)),
            ),
            Event::RunCancelRequested { run } => item(
                Self::author_lane(author),
                ItemKind::RunLifecycle,
                "cancel requested".into(),
            ),
            Event::RunFinished {
                run,
                attempt,
                outcome,
            } => {
                if let Some(key) = self.open_runs.remove(&(*run, *attempt))
                    && let Some(start) = self.item_mut(&key)
                {
                    start.ended_at = Some(at);
                }
                let kind = match outcome {
                    RunOutcome::Snapshot(_) => ItemKind::Snapshot,
                    _ => ItemKind::RunLifecycle,
                };
                item(self.run_lane(*run), kind, outcome_title(outcome))
            }
            Event::LeaseOpened { run, name, .. } => item(
                self.run_lane(*run),
                ItemKind::RunLifecycle,
                format!("lease {name} opened"),
            ),
            Event::LeaseBound { .. } => {
                item(Lane::Host, ItemKind::RunLifecycle, "lease bound".into())
            }
            Event::LeaseClosed { .. } => {
                item(Lane::Host, ItemKind::RunLifecycle, "lease closed".into())
            }
            Event::ArtifactCaptured { run, name, .. } => {
                item(self.run_lane(*run), ItemKind::Artifact, name.clone())
            }
            // Retention bookkeeping; the raw view says why the bytes are gone.
            Event::SourcePruned { .. } => return false,
            Event::ThreadOpened { body, .. } | Event::Replied { body, .. } => item(
                Self::author_lane(author),
                ItemKind::Comment,
                first_line(body),
            ),
            Event::ThreadResolved { resolved, .. } => item(
                Self::author_lane(author),
                ItemKind::Comment,
                if *resolved { "resolved" } else { "reopened" }.into(),
            ),
            Event::FindingPromoted { .. } => item(
                Self::author_lane(author),
                ItemKind::Finding,
                "finding promoted".into(),
            ),
            Event::FindingDismissed { dismissed, .. } => item(
                Self::author_lane(author),
                ItemKind::Finding,
                if *dismissed {
                    "finding dismissed"
                } else {
                    "finding restored"
                }
                .into(),
            ),
            Event::Decided { verdict, note, .. } => item(
                Self::author_lane(author),
                ItemKind::Decision,
                format!("{verdict:?}: {}", first_line(note)),
            ),
            Event::TaskSet { patch, .. } => item(
                Self::author_lane(author),
                ItemKind::TaskChange,
                patch.title.as_deref().map(first_line).unwrap_or_default(),
            ),
            Event::Bookmarked { kind, note, .. } => item(
                Self::author_lane(author),
                ItemKind::Comment,
                format!("{kind:?}: {}", first_line(note)),
            ),
            Event::CandidateDismissed { .. } => return false,
        };
        self.insert(at, fact.id.clone(), new);
        true
    }

    fn observed(&mut self, fact: &Fact<'_>, source: &SourceMeta, obs: &Observation) -> bool {
        let lane = match source.kind.family() {
            SourceFamily::AgentSession | SourceFamily::AgentHook => Lane::Agent(source.run),
            _ => Lane::Run(source.run),
        };
        let (kind, title) = match obs {
            Observation::Agent(event) => match &event.what {
                AgentWhat::Prompt { text } => (ItemKind::Prompt, first_line(text)),
                AgentWhat::Reply { text, .. } => (ItemKind::Reply, first_line(text)),
                AgentWhat::Thinking { text } => (ItemKind::Thinking, first_line(text)),
                AgentWhat::ToolCall { call, name, .. } => {
                    self.open_calls
                        .insert(call.clone(), (fact.at, fact.id.clone()));
                    let kind = ItemKind::ToolCall {
                        name: name.clone(),
                        ok: None,
                    };
                    (kind, name.0.clone())
                }
                AgentWhat::ToolResult { call, is_error, .. } => {
                    let Some(key) = self.open_calls.remove(call) else {
                        return false;
                    };
                    let Some(item) = self.item_mut(&key) else {
                        return false;
                    };
                    item.ended_at = Some(fact.at);
                    if let ItemKind::ToolCall { ok, .. } = &mut item.kind {
                        *ok = Some(!is_error);
                    }
                    return true;
                }
                AgentWhat::Plan(_) => (ItemKind::PlanChange, "plan changed".into()),
                AgentWhat::Result { text, .. } => {
                    if let Some(window) = self.summary_runs.get(&source.run) {
                        self.summaries.insert(*window, fact.id.clone());
                    }
                    (ItemKind::Reply, first_line(text))
                }
                // Shown by progress (`Activity`), not as moments of their own.
                AgentWhat::PermissionWait { .. } | AgentWhat::TurnEnded { .. } => return false,
                AgentWhat::Usage { .. } | AgentWhat::Other { .. } => return false,
            },
            Observation::Lean(m) if m.severity == LeanSeverity::Error => (
                ItemKind::Finding,
                format!("{}:{} {}", m.path, m.line, first_line(&m.text)),
            ),
            // TLC `-tool` classes: 1 = error, 2 = TLC bug. The rest is progress chatter;
            // the checks view reads the verdict.
            Observation::Tlc(m) if matches!(m.class, 1 | 2) => {
                (ItemKind::Finding, first_line(&m.text))
            }
            Observation::Lean(_) | Observation::Tlc(_) => return false,
            Observation::Device(a) => {
                let kind = ItemKind::ToolCall {
                    name: ToolName(a.action.clone()),
                    ok: None,
                };
                (kind, first_line(&a.detail))
            }
            Observation::Discontinuity { .. } => {
                (ItemKind::Discontinuity, "transcript rewritten".into())
            }
            Observation::Unrecognized { reason } => (ItemKind::Unrecognized, reason.clone()),
            Observation::Terminal | Observation::Line(_) => return false,
        };
        let item = Item {
            lane,
            kind,
            title,
            ended_at: None,
        };
        self.insert(fact.at, fact.id.clone(), item);
        true
    }
}

impl Projection for Timeline {
    const NAME: &'static str = "timeline";
    const VERSION: u32 = 1;
    const READS: &'static [SourceFamily] = &[
        SourceFamily::AgentSession,
        SourceFamily::AgentHeadless,
        SourceFamily::AgentHook,
        SourceFamily::Checker,
        SourceFamily::Device,
    ];
    fn apply(&mut self, fact: &Fact<'_>) -> bool {
        match &fact.body {
            FactBody::Authored { author, event } => self.authored(fact, author, event),
            FactBody::Observed { source, obs, .. } => self.observed(fact, source, obs),
        }
    }
}

fn first_line(text: &str) -> String {
    const MAX: usize = 120;
    let line = text
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    match line.char_indices().nth(MAX) {
        Some((cut, _)) => format!("{}…", &line[..cut]),
        None => line.to_owned(),
    }
}

fn purpose_name(purpose: &RunPurpose) -> &'static str {
    match purpose {
        RunPurpose::Agent(_) => "agent",
        RunPurpose::Snapshot { .. } => "snapshot",
        RunPurpose::Review(_) => "review",
        RunPurpose::Check(_) => "check",
        RunPurpose::Merge(_) => "merge",
        RunPurpose::Summarize { .. } => "summary",
        RunPurpose::Insight { .. } => "insight",
        RunPurpose::WriteSkill(_) => "skill write",
        RunPurpose::Import { .. } => "import",
    }
}

fn outcome_title(outcome: &RunOutcome) -> String {
    match outcome {
        RunOutcome::Exited { code } => format!("exited {code}"),
        RunOutcome::Killed { signal, cancelled } => match cancelled {
            true => "cancelled".into(),
            false => format!("killed by signal {signal}"),
        },
        RunOutcome::SpawnFailed { error } => format!("failed to start: {}", first_line(error)),
        RunOutcome::Abandoned => "abandoned".into(),
        RunOutcome::Snapshot(_) => "snapshot".into(),
        RunOutcome::Merged { .. } => "merged".into(),
        RunOutcome::SkillWritten { .. } => "skill written".into(),
        RunOutcome::Failed { reason } => format!("failed: {}", first_line(reason)),
    }
}
