//! Conversation structure per job: the record tree (Claude records point at parents;
//! rewinds and edits create branches, a resumed run continues its predecessor's tree),
//! subagent sidechains, the index from records to the facts (and so the raw bytes) they
//! came from, and the file edits the agents made.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use crate::fact::Fact;
use crate::ids::*;
use crate::source::SourceFamily;
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
    pub fn thread(&self, run: &RunId, leaf: Option<&RecordKey>) -> Vec<&RecordKey> {
        todo!("not implemented")
    }
    /// Records with more than one child: where the human rewound or edited.
    pub fn branch_points(&self) -> BTreeSet<&RecordKey> {
        todo!("not implemented")
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
        // TODO: JobOpened -> worktree. Observed agent events of `source.run` -> insert a
        //       node on first sight of a record key, else push the fact onto it (parts of
        //       one frame); TurnEnded -> latest_turn_end[run]; ToolCall touches ->
        //       `RepoPath::within(worktree, path)` -> push EditTrace (paths outside the
        //       worktree are ignored).
        todo!("not implemented")
    }
}
