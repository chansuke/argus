//! Conversation logs as assets: skill candidates proposed by insight runs (meta ledger),
//! with evidence pointing back into job ledgers, and their fate.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::adapters::llm;
use crate::event::{Event, RunOutcome, RunPurpose, SkillDraft};
use crate::fact::{Fact, FactBody};
use crate::ids::*;
use crate::source::{AgentWhat, Observation, RawCandidate, SourceFamily};
use crate::views::Projection;

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
    /// Each insight run's prompt: its answer cites tags that only the prompt resolves.
    insight_runs: BTreeMap<RunId, String>,
    /// Skill writes started from a candidate.
    writes: BTreeMap<RunId, CandidateId>,
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
        match &fact.body {
            FactBody::Authored { event, .. } => match event {
                Event::RunRequested {
                    run,
                    purpose: RunPurpose::Insight { llm, .. },
                } => {
                    self.insight_runs.insert(*run, llm.prompt.clone());
                    true
                }
                Event::RunRequested {
                    run,
                    purpose: RunPurpose::WriteSkill(draft),
                } => {
                    let Some(candidate) = draft
                        .from
                        .as_ref()
                        .and_then(|id| self.candidates.get_mut(id))
                    else {
                        return false;
                    };
                    candidate.status = CandidateStatus::Accepted {
                        run: *run,
                        draft: draft.clone(),
                    };
                    self.writes.insert(*run, candidate.id.clone());
                    true
                }
                Event::RunFinished {
                    run,
                    outcome: RunOutcome::SkillWritten { path },
                    ..
                } => {
                    let Some(candidate) = self
                        .writes
                        .get(run)
                        .and_then(|id| self.candidates.get_mut(id))
                    else {
                        return false;
                    };
                    candidate.status = CandidateStatus::Written { path: path.clone() };
                    true
                }
                Event::CandidateDismissed { candidate } => {
                    let Some(candidate) = self.candidates.get_mut(candidate) else {
                        return false;
                    };
                    candidate.status = CandidateStatus::Dismissed;
                    true
                }
                _ => false,
            },
            FactBody::Observed {
                source,
                obs: Observation::Agent(event),
                ..
            } => {
                let AgentWhat::Result {
                    text,
                    is_error: false,
                } = &event.what
                else {
                    return false;
                };
                let Some(prompt) = self.insight_runs.get(&source.run) else {
                    return false;
                };
                // An answer the parser cannot read stays in the run's raw log.
                let Some(raw) = llm::parse_candidates(prompt, text) else {
                    return false;
                };
                for (index, raw) in raw.into_iter().enumerate() {
                    let id = CandidateId {
                        fact: fact.id.clone(),
                        index: u16::try_from(index).unwrap_or(u16::MAX),
                    };
                    let candidate = Candidate {
                        id: id.clone(),
                        raw,
                        status: CandidateStatus::Proposed,
                    };
                    self.candidates.insert(id, candidate);
                }
                true
            }
            FactBody::Observed { .. } => false,
        }
    }
}
