//! Conversation structure per job: the record tree (Claude records point at parents;
//! rewinds and edits create branches, a resumed run continues its predecessor's tree),
//! subagent sidechains, the index from records to the facts (and so the raw bytes) they
//! came from, and the file edits the agents made.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use crate::event::Event;
use crate::fact::{Fact, FactBody};
use crate::ids::*;
use crate::source::{AgentWhat, Observation, SourceFamily, TouchHow};
use crate::views::Projection;

#[derive(Default, Debug)]
pub struct Transcript {
    /// From `JobOpened`: edit paths are made repo-relative against it.
    worktree: Option<PathBuf>,
    nodes: BTreeMap<RecordKey, Node>,
    children: BTreeMap<RecordKey, Vec<RecordKey>>,
    /// Newest record per run (by fact order): default leaf of that run's main thread.
    latest: BTreeMap<RunId, RecordKey>,
    latest_turn_end: BTreeMap<RunId, FactId>,
    /// Observations without a record id (hook payloads, stream-json system lines).
    loose: Vec<FactId>,
    /// In stamp order, so a revision window is one contiguous slice.
    edits: Vec<EditTrace>,
}

#[derive(Debug)]
pub struct Node {
    /// The run whose source first carried this record (`ledger::Applied` owns the rule).
    pub run: RunId,
    pub parent: Option<RecordKey>,
    pub at: UnixMillis,
    pub sidechain: bool,
    /// A prompt, reply, thinking, or tool call: conversation that can branch. Tool results
    /// and bookkeeping records cannot. Parallel tool calls give a call two children (the
    /// next call and the first result), which is not a branch.
    pub can_branch: bool,
    /// Every observation decoded from this record, in order.
    pub facts: Vec<FactId>,
}

/// One file edit an agent made, read from its tool call (`AgentWhat::ToolCall.touches`).
/// The link from a diff hunk to the agent's own raw words (`packet::provenance`).
#[derive(Clone, Debug)]
pub struct EditTrace {
    pub fact: FactId,
    pub stamp: Stamp,
    pub run: RunId,
    pub path: RepoPath,
    pub how: EditHow,
}

#[derive(Clone, Debug)]
pub enum EditHow {
    /// `diff::line_hash` of each inserted line.
    Structured { inserted: Vec<u64> },
    /// A shell command named the path; content unknown.
    Shell,
}

impl Transcript {
    /// Root-to-leaf path ending at `leaf` (default: `run`'s latest record).
    /// A parent that was never seen, or a parent loop, ends the walk.
    pub fn thread(&self, run: &RunId, leaf: Option<&RecordKey>) -> Vec<&RecordKey> {
        let Some(mut at) = leaf.or_else(|| self.latest.get(run)) else {
            return Vec::new();
        };
        let mut seen = BTreeSet::new();
        let mut path = Vec::new();
        while let Some((key, node)) = self.nodes.get_key_value(at) {
            if !seen.insert(key) {
                break;
            }
            path.push(key);
            match &node.parent {
                Some(parent) => at = parent,
                None => break,
            }
        }
        path.reverse();
        path
    }
    /// Records with more than one child that can branch: where the human rewound or
    /// edited, or a reply was regenerated.
    pub fn branch_points(&self) -> BTreeSet<&RecordKey> {
        self.children
            .iter()
            .filter(|(_, kids)| {
                let branching = kids
                    .iter()
                    .filter(|k| self.nodes.get(*k).is_some_and(|n| n.can_branch));
                branching.count() > 1
            })
            .map(|(key, _)| key)
            .collect()
    }
    pub fn latest_turn_end(&self, run: &RunId) -> Option<&FactId> {
        self.latest_turn_end.get(run)
    }
    /// Edits with `after < stamp <= until`. A revision window is the edits between two
    /// consecutive snapshot cuts; `after = None` means from the start of the job.
    pub fn edits_between(&self, after: Option<Stamp>, until: Stamp) -> &[EditTrace] {
        let lo = after.map_or(0, |a| self.edits.partition_point(|e| e.stamp <= a));
        let hi = self.edits.partition_point(|e| e.stamp <= until);
        &self.edits[lo..hi.max(lo)]
    }
}

impl Projection for Transcript {
    const NAME: &'static str = "transcript";
    const VERSION: u32 = 1;
    const READS: &'static [SourceFamily] = &[
        SourceFamily::AgentSession,
        SourceFamily::AgentHeadless,
        SourceFamily::AgentHook,
    ];
    fn apply(&mut self, fact: &Fact<'_>) -> bool {
        let (source, event) = match &fact.body {
            FactBody::Authored {
                event: Event::JobOpened(info),
                ..
            } => {
                self.worktree = Some(info.worktree.clone());
                return true;
            }
            FactBody::Authored { .. } => return false,
            FactBody::Observed {
                source,
                obs: Observation::Agent(event),
                ..
            } => (source, event),
            FactBody::Observed { .. } => return false,
        };
        let run = source.run;
        match &event.record {
            Some(key) => match self.nodes.get_mut(key) {
                Some(node) => node.facts.push(fact.id.clone()),
                None => {
                    let can_branch = matches!(
                        event.what,
                        AgentWhat::Prompt { .. }
                            | AgentWhat::Reply { .. }
                            | AgentWhat::Thinking { .. }
                            | AgentWhat::ToolCall { .. }
                    );
                    let node = Node {
                        run,
                        parent: event.parent.clone(),
                        at: event.at.unwrap_or(fact.at),
                        sidechain: event.sidechain,
                        can_branch,
                        facts: vec![fact.id.clone()],
                    };
                    self.nodes.insert(key.clone(), node);
                    if let Some(parent) = &event.parent {
                        self.children
                            .entry(parent.clone())
                            .or_default()
                            .push(key.clone());
                    }
                    if !event.sidechain {
                        self.latest.insert(run, key.clone());
                    }
                }
            },
            None => self.loose.push(fact.id.clone()),
        }
        match &event.what {
            AgentWhat::TurnEnded { .. } => {
                self.latest_turn_end.insert(run, fact.id.clone());
            }
            AgentWhat::ToolCall { touches, .. } => {
                let Some(worktree) = &self.worktree else {
                    return true;
                };
                for touch in touches {
                    // Paths outside the worktree are not part of the job's diff.
                    let Some(path) = RepoPath::within(worktree, &touch.path) else {
                        continue;
                    };
                    let how = match &touch.how {
                        TouchHow::Inserted(lines) => EditHow::Structured {
                            inserted: lines.clone(),
                        },
                        TouchHow::Shell => EditHow::Shell,
                    };
                    self.edits.push(EditTrace {
                        fact: fact.id.clone(),
                        stamp: fact.stamp,
                        run,
                        path,
                        how,
                    });
                }
            }
            _ => {}
        }
        true
    }
}
