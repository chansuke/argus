//! Automatic events derived from views. Pure; the host submits the results as
//! `Author::Policy` after every change.
//!
//! Idempotent by construction: each event's id is derived from what triggered it
//! (`Id128::derive`), so deciding twice (around a crash, or on every append) yields the
//! same ids and the ledger drops the repeats as duplicates.

use crate::event::{
    AgentLaunch, CheckRequest, CheckSpec, Decision, Event, LlmCall, ReviewLens, ReviewSpec,
    RunPurpose,
};
use crate::ids::*;
use crate::models::{ModelChoice, ModelSet};
use crate::period::{BucketWidth, Window};
use crate::views::JobViews;
use crate::views::packet::render_feedback;
use crate::views::progress::{Activity, WaitReason};
use crate::views::timeline::{Item, ItemKind, Lane};

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

/// In this order:
/// - Snapshots (`snapshot_on_turn_end`): each agent run's latest turn end gets one
///   snapshot run, `derive("snapshot", turn end fact)`.
/// - A head gets the configured review panel (`derive("auto-review", tree)`) and every
///   configured check whose target depends on a file the head changed
///   (`derive("check", tree + spec)`). Nothing new is started for an abandoned job.
/// - Feedback delivery, when the latest decision is `RequestChanges` D without a delivery
///   run. The derived id (`DecisionId::delivery_run`) makes the request at-most-once, and
///   `Agent` runs are never retried after a crash (`OnInterrupt::Abandon`), so the agent
///   never receives the same feedback twice. prev = `runs.latest_agent()`:
///   - prev live and waiting at its prompt -> `RunCancelRequested { prev }` (it exits);
///   - prev live and mid-turn -> nothing yet (delivery waits for the turn end);
///   - prev not live -> an `Agent` run resuming prev, with prev's cli and model, prompted
///     with `render_feedback(D.note, review.feedback(Some(D)))`, the same text the human
///     previewed.
/// - The latest decision is `Abandon` -> `RunCancelRequested` for every live run.
/// - Summaries: each `timeline.unsummarized(width, now)` window -> `Summarize`
///   (`derive("summary", width + start)`), prompted with the window's items.
///
/// Skips runs already requested and cancels already asked for (the ledger would drop
/// them as duplicates anyway).
pub fn decide(views: &JobViews, cfg: &PolicyConfig, now: UnixMillis) -> Vec<Event> {
    let (runs, review) = (views.runs(), views.review());
    let mut out = Vec::new();
    let request = |out: &mut Vec<Event>, run: RunId, purpose: RunPurpose| {
        if runs.get(&run).is_none() {
            out.push(Event::RunRequested { run, purpose });
        }
    };
    let live = |run: &RunId| {
        runs.get(run)
            .and_then(|r| r.attempts.last())
            .is_some_and(|a| a.finished.is_none())
    };
    let latest_decision = review.decisions().last();
    let abandoned = latest_decision.is_some_and(|d| d.verdict == Decision::Abandon);

    if cfg.snapshot_on_turn_end && !abandoned {
        for (run, state) in runs.iter() {
            if !matches!(state.purpose, RunPurpose::Agent(_)) {
                continue;
            }
            if let Some(turn_end) = views.transcript().latest_turn_end(run) {
                let id = RunId(Id128::derive("snapshot", turn_end.to_string().as_bytes()));
                let after = Some(turn_end.clone());
                request(&mut out, id, RunPurpose::Snapshot { after });
            }
        }
    }

    if let Some(head) = review.head().filter(|_| !abandoned) {
        let (tree, base) = (&head.snapshot.tree, &head.snapshot.base);
        if let Some((models, lens)) = &cfg.review_on_snapshot {
            let request_id = Id128::derive("auto-review", tree.as_str().as_bytes());
            for (run, purpose) in ReviewSpec::panel(request_id, tree, base, *lens, None, models) {
                request(&mut out, run, purpose);
            }
        }
        for spec in &cfg.checks_on_snapshot {
            let target = spec.target();
            if !head
                .snapshot
                .files
                .iter()
                .any(|f| target.depends_on(&f.path))
            {
                continue;
            }
            let key = format!("{}\0{spec:?}", tree.as_str());
            let check = CheckRequest {
                tree: tree.clone(),
                base: base.clone(),
                spec: spec.clone(),
            };
            request(
                &mut out,
                RunId(Id128::derive("check", key.as_bytes())),
                RunPurpose::Check(check),
            );
        }
    }

    if let Some(d) = latest_decision.filter(|d| d.verdict == Decision::RequestChanges)
        && runs.get(&d.id.delivery_run()).is_none()
        && let Some((prev, state)) = runs.latest_agent()
        && let RunPurpose::Agent(launch) = &state.purpose
    {
        if live(prev) {
            let waiting = views.progress().activities().any(|(r, a)| {
                r == prev
                    && matches!(
                        a,
                        Activity::AwaitingHuman {
                            why: WaitReason::TurnEnded,
                            ..
                        }
                    )
            });
            if waiting && !state.cancel_requested {
                out.push(Event::RunCancelRequested { run: *prev });
            }
        } else {
            let prompt = render_feedback(&d.note, &review.feedback(Some(d.id)));
            let resume = AgentLaunch {
                cli: launch.cli,
                model: launch.model.clone(),
                prompt,
                resume: Some(*prev),
            };
            request(&mut out, d.id.delivery_run(), RunPurpose::Agent(resume));
        }
    }

    if abandoned {
        for (run, state) in runs.iter() {
            if live(run) && !state.cancel_requested {
                out.push(Event::RunCancelRequested { run: *run });
            }
        }
    }

    if let Some((width, model)) = &cfg.summaries {
        let timeline = views.timeline();
        for window in timeline.unsummarized(*width, now) {
            let mut key = format!("{width:?}").into_bytes();
            key.extend_from_slice(&window.start.0.to_le_bytes());
            let items = timeline
                .range(window.start, window.end())
                .map(|(at, _, item)| (at, item));
            let llm = LlmCall {
                model: model.clone(),
                prompt: summary_prompt(&window, items),
            };
            let id = RunId(Id128::derive("summary", &key));
            request(&mut out, id, RunPurpose::Summarize { window, llm });
        }
    }
    out
}

/// The prompt of a `Summarize` run: the window's items, one line each, oldest first.
fn summary_prompt<'a>(
    window: &Window,
    items: impl Iterator<Item = (UnixMillis, &'a Item)>,
) -> String {
    let clock = |t: UnixMillis| {
        let s = t.0.div_euclid(1000).rem_euclid(86_400);
        format!("{:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    };
    let mut out = format!(
        "Summarize this part of a coding session, {} to {} UTC, in three to five short \
         sentences for someone who was away: what was done, what failed, and what is \
         waiting. Use only the events below.\n\nEvents:\n",
        clock(window.start),
        clock(window.end()),
    );
    for (at, item) in items {
        let lane = match &item.lane {
            Lane::Agent(run) => format!("agent {}", short(&run.0)),
            Lane::Run(run) => format!("run {}", short(&run.0)),
            Lane::Human => "human".into(),
            Lane::Host => "host".into(),
        };
        let kind = match &item.kind {
            ItemKind::ToolCall { name, ok } => match ok {
                Some(false) => format!("tool {} failed", name.0),
                _ => format!("tool {}", name.0),
            },
            other => format!("{other:?}").to_lowercase(),
        };
        out.push_str(&format!("{} [{lane}] {kind}: {}\n", clock(at), item.title));
    }
    out
}

/// The first 8 hex digits of an id: enough to tell lanes apart in a prompt.
fn short(id: &crate::ids::Id128) -> String {
    id.to_string()[..8].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_summary_prompt_says_which_tool_calls_failed() {
        use crate::ids::{RunId, ToolName};
        use crate::period::{BucketWidth, Window};
        use crate::views::timeline::{Item, ItemKind, Lane};
        let call = |ok| Item {
            lane: Lane::Agent(RunId(crate::ids::Id128(1))),
            kind: ItemKind::ToolCall {
                name: ToolName("Bash".into()),
                ok,
            },
            title: "Bash".into(),
            ended_at: None,
        };
        let window = Window {
            width: BucketWidth::M15,
            start: UnixMillis(0),
        };
        let items = [call(Some(false)), call(Some(true)), call(None)];
        let prompt = summary_prompt(&window, items.iter().map(|i| (UnixMillis(0), i)));
        assert_eq!(prompt.matches("tool Bash failed").count(), 1);
        assert_eq!(prompt.matches("] tool Bash: ").count(), 2);
        assert!(prompt.contains("00:00:00 to 00:15:00 UTC"), "{prompt}");
    }
}
