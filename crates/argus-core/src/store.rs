//! The data root: `<root>/jobs/<job-id>/` (one ledger per job), `<root>/meta/` (the meta
//! ledger), `<root>/checkpoints/`. Default root: `$XDG_DATA_HOME/argus`.

use std::path::PathBuf;

use crate::ids::*;
use crate::ledger::{JobLedger, LedgerError, MetaLedger};
use crate::period::Period;
use crate::views::stats::Analytics;

#[derive(Clone, Debug)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn open(root: PathBuf) -> Result<Self, LedgerError> {
        todo!("not implemented")
    }
    pub fn jobs(&self) -> Result<Vec<JobId>, LedgerError> {
        todo!("not implemented")
    }
    /// Writable; takes the job's locks. Only the host calls this. Creates the directory
    /// for a new job (the caller then submits `JobOpened`).
    pub fn open_job(&self, job: JobId) -> Result<JobLedger, LedgerError> {
        todo!("not implemented")
    }
    pub fn open_meta(&self) -> Result<MetaLedger, LedgerError> {
        todo!("not implemented")
    }

    /// Store-wide analytics without opening any ledger for writing.
    ///
    /// TODO: for each job: load `checkpoints/<job>/stats.ckpt` (the projection plus its
    /// `ledger::Applied`, so record dedup survives the checkpoint) if usable, then
    /// `ledger::replay::<JobStats>(job_dir, Some(ckpt))` to catch up on the tail; save the
    /// new checkpoint (temp file + rename; concurrent writers are harmless since both
    /// write a valid checkpoint). Merge into `Analytics`.
    pub fn analytics(&self) -> Result<Analytics, LedgerError> {
        todo!("not implemented")
    }

    /// The exact prompt for an insight run over `period`: per conversation, the human
    /// prompts, corrections, tool errors, rework loops, and bookmarks, each tagged with a
    /// `FactRef` the model must cite as evidence. Existing skill names are listed so it
    /// proposes changes instead of duplicates. Stored verbatim in the `Insight` request.
    pub fn insight_prompt(
        &self,
        period: &Period,
        existing_skills: &[SkillName],
    ) -> Result<String, LedgerError> {
        todo!("not implemented")
    }
}
