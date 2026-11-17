//! Automatic events derived from views. Pure; the host submits the results as
//! `Author::Policy` after every change.
//!
//! Idempotent by construction: each event's id is derived from what triggered it
//! (`Id128::derive`), so deciding twice (around a crash, or on every append) yields the
//! same ids and the ledger drops the repeats as duplicates.

use crate::event::{CheckSpec, Event, ReviewLens};
use crate::ids::*;
use crate::models::{ModelChoice, ModelSet};
use crate::views::JobViews;
use crate::views::timeline::BucketWidth;

#[derive(Clone, Debug, Default)]
pub struct PolicyConfig {
    /// Snapshot the worktree when an agent ends a turn.
    pub snapshot_on_turn_end: bool,
    /// AI review of every new head, one run per model. `None` (the default): review only
    /// on request, because each run costs money (open question in RATIONALE).
    pub review_on_snapshot: Option<(ModelSet, ReviewLens)>,
    /// Checks to run on each new head whose target depends on a changed file
    /// (`CheckTarget::depends_on`). Device checks belong here only if the user opted into
    /// their cost.
    pub checks_on_snapshot: Vec<CheckSpec>,
    pub summaries: Option<(BucketWidth, ModelChoice)>,
}

/// TODO, in this order:
/// - Snapshots: per agent run whose latest turn end is newer than the head's cut ->
///   RunRequested(derive("snapshot", turn-end fact), Snapshot { after }).
/// - Review / checks of a new head: `ReviewSpec::panel(derive("auto-review", tree), ..)`;
///   each configured check whose target depends on a path in `head.snapshot.files`
///   (derive("check", tree + target + engine)).
/// - Feedback delivery, for each `RequestChanges` decision D without a delivery run. The
///   derived id makes the request at-most-once, and `Agent` runs are never retried after a
///   crash (`OnInterrupt::Abandon`), so the agent never receives the same feedback twice.
///   prev = runs.latest_agent();
///   - prev live and AwaitingHuman -> RunCancelRequested { prev } (it exits at its prompt);
///   - prev live and mid-turn -> nothing yet (delivery waits for the turn end);
///   - prev not live -> RunRequested { run: RunId(derive("deliver", D)), Agent(AgentLaunch {
///     cli: prev.cli, model: prev.model, resume: Some(prev),
///     prompt: render_feedback(D.note, &review.feedback(Some(D))) }) }.
/// - `Abandon` decision -> RunCancelRequested for every live run of the job.
/// - Summaries: each `timeline.unsummarized(width, now)` window -> Summarize, prompt built
///   from the window's items.
///
/// Skips ids already present in `runs` (cheap pre-filter; the ledger dedups anyway).
pub fn decide(views: &JobViews, cfg: &PolicyConfig, now: UnixMillis) -> Vec<Event> {
    todo!("not implemented")
}
