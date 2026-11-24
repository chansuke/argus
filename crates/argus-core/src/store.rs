//! The data root: `<root>/jobs/<job-id>/` (one ledger per job), `<root>/meta/` (the meta
//! ledger), `<root>/checkpoints/`. Default root: `$XDG_DATA_HOME/argus`.

use std::path::PathBuf;

use argus_log::LogError;

use crate::adapters::llm::{self, InsightSession};
use crate::ids::*;
use crate::ledger::{JobLedger, LedgerError, MetaLedger};
use crate::period::Period;
use crate::views::runs::Runs;
use crate::views::stats::{Analytics, JobStats};
use crate::views::timeline::Timeline;

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

    /// Store-wide analytics without opening any ledger for writing: each job's `JobStats`
    /// replayed from its logs (`ledger::replay`, safe while a host writes).
    ///
    /// TODO: start each replay from `checkpoints/<job>/stats.ckpt` (the projection plus its
    /// `ledger::Applied`, so record dedup survives) when usable, and save the new one
    /// (temp file + rename). Until then every call replays every job in full.
    pub fn analytics(&self) -> Result<Analytics, LedgerError> {
        let mut analytics = Analytics::default();
        for job in self.jobs()? {
            let dir = self.root.join("jobs").join(job.0.to_string());
            let (stats, _) = crate::ledger::replay::<JobStats>(&dir, None)?;
            analytics.jobs.insert(job, stats);
        }
        Ok(analytics)
    }

    /// The exact prompt for an insight run over `period` (`adapters::llm::insight_prompt`):
    /// per job, the timeline moments inside it worth learning from, each tagged so the
    /// model cites them as evidence. Existing skill names are listed so it proposes
    /// changes instead of duplicates. Stored verbatim in the `Insight` request. Reads like
    /// `analytics`: every job replayed, safe while a host writes.
    pub fn insight_prompt(
        &self,
        period: &Period,
        existing_skills: &[SkillName],
    ) -> Result<String, LedgerError> {
        let mut sessions = Vec::new();
        for job in self.jobs()? {
            let dir = self.root.join("jobs").join(job.0.to_string());
            let (timeline, _) = crate::ledger::replay::<Timeline>(&dir, None)?;
            let items: Vec<_> = timeline
                .range(period.from, period.to)
                .map(|(at, fact, item)| (at, fact.clone(), item.clone()))
                .collect();
            if items.is_empty() {
                continue;
            }
            let (runs, _) = crate::ledger::replay::<Runs>(&dir, None)?;
            let title = runs.job().map(|j| j.title.clone()).unwrap_or_default();
            sessions.push(InsightSession { job, title, items });
        }
        Ok(llm::insight_prompt(period, existing_skills, &sessions))
    }
}
