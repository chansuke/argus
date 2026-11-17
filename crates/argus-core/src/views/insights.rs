//! Conversation logs as assets: skill candidates proposed by insight runs (meta ledger),
//! with evidence pointing back into job ledgers, and their fate.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::event::SkillDraft;
use crate::fact::Fact;
use crate::ids::*;
use crate::source::SourceFamily;
use crate::views::Projection;

/// What `adapters::llm::parse_candidates` yields.
#[derive(Clone, Debug)]
pub struct RawCandidate {
    pub name: String,
    pub description: String,
    pub why: String,
    pub body: String,
    pub evidence: Vec<FactRef>,
}

#[derive(Debug)]
pub struct Candidate {
    pub id: CandidateId,
    pub raw: RawCandidate,
    pub status: CandidateStatus,
}

#[derive(Debug)]
pub enum CandidateStatus {
    Proposed,
    Dismissed,
    /// A `WriteSkill` run was requested from it.
    Accepted {
        run: RunId,
        draft: SkillDraft,
    },
    Written {
        path: PathBuf,
    },
}

#[derive(Default, Debug)]
pub struct Insights {
    candidates: BTreeMap<CandidateId, Candidate>,
    insight_runs: BTreeMap<RunId, crate::period::Period>,
}

impl Insights {
    pub fn open(&self) -> impl Iterator<Item = &Candidate> {
        self.candidates
            .values()
            .filter(|c| matches!(c.status, CandidateStatus::Proposed))
    }
    pub fn get(&self, id: &CandidateId) -> Option<&Candidate> {
        self.candidates.get(id)
    }
}

impl Projection for Insights {
    const NAME: &'static str = "insights";
    const VERSION: u32 = 1;
    const READS: &'static [SourceFamily] = &[SourceFamily::AgentHeadless];
    fn apply(&mut self, fact: &Fact<'_>) -> bool {
        // TODO: Result of an insight run -> parse_candidates -> Proposed;
        //       CandidateDismissed -> Dismissed; RunRequested(WriteSkill{from}) -> Accepted;
        //       RunFinished(SkillWritten) -> Written.
        todo!("not implemented")
    }
}
