//! The data root: `<root>/jobs/<job-id>/` (one ledger per job), `<root>/meta/` (the meta
//! ledger), `<root>/checkpoints/`. Default root: `$XDG_DATA_HOME/argus`.

use std::path::PathBuf;

use argus_log::LogError;

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
        for dir in ["jobs", "meta"] {
            std::fs::create_dir_all(root.join(dir)).map_err(LogError::from)?;
        }
        Ok(Store { root })
    }
    /// Every job directory, in id order. Anything not named by a job id is not a job.
    pub fn jobs(&self) -> Result<Vec<JobId>, LedgerError> {
        let entries = std::fs::read_dir(self.root.join("jobs")).map_err(LogError::from)?;
        let mut jobs = Vec::new();
        for entry in entries {
            let entry = entry.map_err(LogError::from)?;
            let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
            let id = entry.file_name().to_str().and_then(Id128::parse);
            if let (true, Some(id)) = (is_dir, id) {
                jobs.push(JobId(id));
            }
        }
        jobs.sort();
        Ok(jobs)
    }
    /// Writable; takes the job's locks. Only the host calls this. Creates the directory
    /// for a new job (the caller then submits `JobOpened`).
    pub fn open_job(&self, job: JobId) -> Result<JobLedger, LedgerError> {
        JobLedger::open(&self.root.join("jobs").join(job.0.to_string()))
    }
    pub fn open_meta(&self) -> Result<MetaLedger, LedgerError> {
        MetaLedger::open(&self.root.join("meta"))
    }
    pub fn root(&self) -> &std::path::Path {
        &self.root
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
