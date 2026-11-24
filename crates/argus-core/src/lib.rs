//! argus domain core: event-sourced and UI-agnostic.
//!
//! One rule organizes the crate: **the per-actor logs are the only state; everything
//! shown is a fold over them.**
//!
//! ```text
//!  foreign bytes ──observe──▶ source logs (verbatim) ──decode──▶ Observation ┐
//!  human/agent/host/policy ──submit──▶ authored logs (Event) ─────────────────┼─▶ Fact ─▶ views
//!                                                                             ┘ (stamp order)
//!  views ──policy::decide / Runs::due──▶ run requests ──plan──▶ CommandSpec (host executes)
//!  views + head diff (host, off the sequencer) ──review_packet──▶ what the human reviews
//! ```
//!
//! ```ignore
//! let store = Store::open(data_root)?;
//! let mut job = store.open_job(job_id)?;                 // host only: takes the locks
//! for (run, purpose) in ReviewSpec::panel(request, &head, &base, lens, None, &models) {
//!     job.submit(Author::Human, Event::RunRequested { run, purpose })?;  // retry -> Duplicate
//! }
//! let g = job.views().glance(now);                       // state, share, attention queue
//! let packet = job.views().review_packet(&head_diff, &cfg)?;  // tiers, provenance, headline
//! job.submit(Author::Human, Event::Decided { decision, tree: Some(head), verdict, note })?;
//! let raw = job.raw(&fact.event)?;                       // verbatim bytes behind any item
//! ```

#![expect(
    dead_code,
    unused_variables,
    reason = "todo!() stubs (EAS device checks, insights, checkpoints) take arguments and hold items they do not use yet"
)]

mod adapters;
mod codec;
pub mod command;
pub mod diff;
pub mod event;
pub mod fact;
pub mod ids;
pub mod ledger;
pub mod models;
pub mod period;
pub mod plan;
pub mod policy;
pub mod source;
pub mod store;
pub mod views;

pub use ledger::{
    Applied, JobLedger, Ledger, LedgerError, MetaLedger, RawRecord, Reading, SourceCursor,
    Submitted, replay,
};
pub use store::Store;
