//! Runs, timeline and progress, fed facts directly: what the Glance and timeline screens show
//! for a given sequence of authored events and observed agent records.

use std::path::PathBuf;

use argus_core::event::{Author, Event, RunOutcome, RunPurpose, TaskPatch, TaskState};
use argus_core::ids::{Attempt, Id128, RunId, TaskKey, ToolName, UnixMillis};
use argus_core::models::AgentCli;
use argus_core::source::{AgentWhat, Observation, PlanOp, PlanStatus, TokenUsage};
use argus_core::views::progress::{Activity, PlanNode, Progress, WaitReason};
use argus_core::views::runs::{Due, MAX_ATTEMPTS, Runs};
use argus_core::views::timeline::{BucketWidth, ItemKind, Lane, Timeline, Window};

mod common;
use common::*;

// ---------------------------------------------------------------- timeline

#[test]
fn a_tool_result_closes_its_call_instead_of_adding_an_item() {
    let mut tl = Timeline::default();
    let mut feed = Feed::default();
    let src = transcript(run(1));
    feed.authored(&mut tl, T0, Author::Human, agent_requested(1));
    let (call_fact, changed) = feed.agent(&mut tl, &src, T0 + 10, call("t1", "Bash"));
    assert!(changed);
    let (_, changed) = feed.agent(&mut tl, &src, T0 + 50, result("t1", true));
    assert!(changed);

    let calls: Vec<_> = tl
        .range(UnixMillis(T0), UnixMillis(T0 + M15))
        .filter(|(_, _, item)| matches!(item.kind, ItemKind::ToolCall { .. }))
        .collect();
    assert_eq!(calls.len(), 1, "{calls:#?}");
    let (at, id, item) = &calls[0];
    assert_eq!((*at, *id), (UnixMillis(T0 + 10), &call_fact));
    assert_eq!(item.lane, Lane::Agent(run(1)));
    assert_eq!(item.ended_at, Some(UnixMillis(T0 + 50)));
    assert!(matches!(
        &item.kind,
        ItemKind::ToolCall { name, ok: Some(false) } if name.0 == "Bash"
    ));
}

#[test]
fn items_are_ordered_by_domain_time_not_arrival() {
    let mut tl = Timeline::default();
    let mut feed = Feed::default();
    let src = transcript(run(1));
    feed.authored(&mut tl, T0, Author::Human, agent_requested(1));
    let (late, _) = feed.agent(&mut tl, &src, T0 + 300, reply("second"));
    // An imported older record arrives after a newer one.
    let (early, _) = feed.agent(&mut tl, &src, T0 + 100, reply("first"));
    let replies: Vec<_> = tl
        .range(UnixMillis(T0), UnixMillis(T0 + M15))
        .filter(|(_, _, i)| matches!(i.kind, ItemKind::Reply))
        .map(|(at, id, i)| (at, id.clone(), i.title.clone()))
        .collect();
    assert_eq!(
        replies,
        vec![
            (UnixMillis(T0 + 100), early, "first".to_owned()),
            (UnixMillis(T0 + 300), late, "second".to_owned()),
        ]
    );
    // `range` is half-open.
    assert_eq!(
        tl.range(UnixMillis(T0 + 100), UnixMillis(T0 + 300))
            .filter(|(_, _, i)| matches!(i.kind, ItemKind::Reply))
            .count(),
        1
    );
}

#[test]
fn each_fact_lands_in_the_lane_of_who_acted() {
    let mut tl = Timeline::default();
    let mut feed = Feed::default();
    let window = Window {
        width: BucketWidth::M15,
        start: UnixMillis(T0),
    };
    feed.authored(&mut tl, T0, Author::Human, agent_requested(1));
    feed.authored(
        &mut tl,
        T0 + 1,
        Author::Policy,
        Event::RunRequested {
            run: run(2),
            purpose: RunPurpose::Summarize { window, llm: llm() },
        },
    );
    feed.authored(&mut tl, T0 + 2, Author::Host, started(1));
    feed.authored(&mut tl, T0 + 3, Author::Host, started(2));
    feed.agent(&mut tl, &transcript(run(1)), T0 + 4, reply("on it"));
    feed.agent(&mut tl, &headless(run(2)), T0 + 5, reply("summary"));
    feed.authored(&mut tl, T0 + 6, Author::Human, set(1, None, 1, DONE));
    feed.authored(&mut tl, T0 + 7, Author::Host, finished(1, 0));

    let lanes: Vec<_> = tl
        .range(UnixMillis(T0), UnixMillis(T0 + M15))
        .map(|(_, _, i)| i.lane.clone())
        .collect();
    assert_eq!(
        lanes,
        vec![
            Lane::Agent(run(1)),
            Lane::Run(run(2)),
            Lane::Agent(run(1)),
            Lane::Run(run(2)),
            Lane::Agent(run(1)),
            Lane::Run(run(2)),
            Lane::Human,
            Lane::Agent(run(1)),
        ]
    );
    // The run's start item is closed when the attempt finishes.
    let start = tl
        .range(UnixMillis(T0 + 2), UnixMillis(T0 + 3))
        .next()
        .unwrap();
    assert!(matches!(start.2.kind, ItemKind::RunLifecycle));
    assert_eq!(start.2.ended_at, Some(UnixMillis(T0 + 7)));
}

#[test]
fn bookkeeping_changes_nothing_visible() {
    let mut tl = Timeline::default();
    let mut feed = Feed::default();
    let src = transcript(run(1));
    feed.authored(&mut tl, T0, Author::Human, agent_requested(1));
    let before = tl.range(UnixMillis(0), UnixMillis(i64::MAX)).count();
    let usage = AgentWhat::Usage {
        message: Some("msg_1".into()),
        usage: TokenUsage::default(),
    };
    assert!(!feed.agent(&mut tl, &src, T0 + 1, usage).1);
    let other = AgentWhat::Other {
        tag: "ai-title".into(),
    };
    assert!(!feed.agent(&mut tl, &src, T0 + 2, other).1);
    assert_eq!(
        tl.range(UnixMillis(0), UnixMillis(i64::MAX)).count(),
        before
    );

    // An unreadable record is never hidden.
    let unknown = Observation::Unrecognized {
        reason: "new record type".into(),
    };
    assert!(feed.observed(&mut tl, &src, T0 + 3, unknown).1);
    assert!(
        tl.range(UnixMillis(0), UnixMillis(i64::MAX))
            .any(|(_, _, i)| matches!(i.kind, ItemKind::Unrecognized))
    );
}

#[test]
fn coarse_buckets_are_folds_of_fine_ones() {
    let mut tl = Timeline::default();
    let mut feed = Feed::default();
    let src = transcript(run(1));
    feed.authored(&mut tl, T0, Author::Human, agent_requested(1));
    // Calls spread over the first hour: 1 in the first quarter, 2 in the second (one
    // failing), none in the third, 1 in the fourth; then 1 in the next hour.
    let calls = [
        (T0 + 1_000, true),
        (T0 + M15 + 1_000, true),
        (T0 + M15 + 2_000, false),
        (T0 + 3 * M15 + 5, true),
        (T0 + 4 * M15 + 7, true),
    ];
    for (i, (at, ok)) in calls.iter().enumerate() {
        let id = format!("t{i}");
        feed.agent(&mut tl, &src, *at, call(&id, "Read"));
        feed.agent(&mut tl, &src, *at + 1, result(&id, !ok));
    }
    feed.authored(
        &mut tl,
        T0 + M15 + 3_000,
        Author::Human,
        set(1, None, 1, DONE),
    );

    let lane = Lane::Agent(run(1));
    let fine = tl.buckets(BucketWidth::M15, UnixMillis(T0), UnixMillis(T0 + 8 * M15));
    let starts: Vec<_> = fine.iter().map(|b| b.window.start.0 - T0).collect();
    // Only windows with activity; the empty third quarter is absent.
    assert_eq!(starts, vec![0, M15, 3 * M15, 4 * M15]);
    let counts: Vec<_> = fine
        .iter()
        .map(|b| {
            let c = b.counts[&lane];
            (c.tool_calls, c.tool_errors)
        })
        .collect();
    assert_eq!(counts, vec![(1, 0), (2, 1), (1, 0), (1, 0)]);

    let hours = tl.buckets(BucketWidth::H1, UnixMillis(T0), UnixMillis(T0 + 8 * M15));
    assert_eq!(hours.len(), 2);
    assert_eq!(hours[0].window.start, UnixMillis(T0));
    assert_eq!(hours[0].window.width, BucketWidth::H1);
    assert_eq!(hours[0].counts[&lane].tool_calls, 4);
    assert_eq!(hours[0].counts[&lane].tool_errors, 1);
    assert_eq!(hours[1].counts[&lane].tool_calls, 1);
    assert!(hours[0].counts.contains_key(&Lane::Human));

    // `from` inside a window still yields the whole window it falls in.
    let mid = tl.buckets(BucketWidth::H1, UnixMillis(T0 + 5), UnixMillis(T0 + M15));
    assert_eq!(mid.len(), 1);
    assert_eq!(mid[0].counts[&lane].tool_calls, 4);
}

#[test]
fn ended_active_windows_wait_for_a_summary_until_one_is_requested() {
    let mut tl = Timeline::default();
    let mut feed = Feed::default();
    let src = transcript(run(1));
    feed.authored(&mut tl, T0, Author::Human, agent_requested(1));
    feed.agent(&mut tl, &src, T0 + 10, reply("a"));
    feed.agent(&mut tl, &src, T0 + 2 * M15 + 10, reply("b"));
    feed.agent(&mut tl, &src, T0 + 3 * M15 + 10, reply("c"));

    let w = |q: i64| Window {
        width: BucketWidth::M15,
        start: UnixMillis(T0 + q * M15),
    };
    // The window holding `now` has not ended; the empty second window had no activity.
    let now = UnixMillis(T0 + 3 * M15 + 20);
    assert_eq!(tl.unsummarized(BucketWidth::M15, now), vec![w(0), w(2)]);

    feed.authored(
        &mut tl,
        now.0,
        Author::Policy,
        Event::RunRequested {
            run: run(9),
            purpose: RunPurpose::Summarize {
                window: w(0),
                llm: llm(),
            },
        },
    );
    assert_eq!(tl.unsummarized(BucketWidth::M15, now), vec![w(2)]);

    let summary = AgentWhat::Result {
        text: "fixed the gate".into(),
        is_error: false,
    };
    let (summary_fact, _) = feed.agent(&mut tl, &headless(run(9)), now.0 + 1, summary);
    feed.authored(&mut tl, now.0 + 2, Author::Host, finished(9, 0));
    let first = &tl.buckets(BucketWidth::M15, UnixMillis(T0), UnixMillis(T0 + M15))[0];
    assert_eq!(first.summary, Some(summary_fact));

    let hour = Window {
        width: BucketWidth::H1,
        start: UnixMillis(T0),
    };
    assert_eq!(
        tl.unsummarized(BucketWidth::H1, UnixMillis(T0 + 5 * M15)),
        vec![hour]
    );
}

// ---------------------------------------------------------------- progress

fn observed_share(p: &Progress) -> Vec<(RunId, f64, bool)> {
    p.tree()
        .roots
        .iter()
        .filter_map(|n| match n {
            PlanNode::Observed {
                run,
                share,
                counted,
                ..
            } => Some((**run, share.get(), *counted)),
            PlanNode::Task { .. } => None,
        })
        .collect()
}

#[test]
fn the_agents_own_plan_tool_gives_progress_without_cooperation() {
    let mut p = Progress::default();
    let mut feed = Feed::default();
    let src = transcript(run(1));
    feed.authored(&mut p, T0, Author::Human, agent_requested(1));
    let plan = todo(&[
        ("Ground", PlanStatus::Completed),
        ("Sketch", PlanStatus::InProgress),
        ("Gone", PlanStatus::Deleted),
    ]);
    assert!(feed.agent(&mut p, &src, T0 + 1, plan).1);
    // Deleted items leave the denominator.
    assert_eq!(p.share().unwrap().get(), 0.5);
    assert_eq!(observed_share(&p), vec![(run(1), 0.5, true)]);

    let upsert = AgentWhat::Plan(PlanOp::Upsert {
        task: "7".into(),
        subject: Some("Ship".into()),
        status: Some(PlanStatus::Completed),
    });
    feed.agent(&mut p, &src, T0 + 2, upsert);
    let tree = p.tree();
    let PlanNode::Observed { plan, .. } = &tree.roots[0] else {
        panic!("{tree:#?}")
    };
    let subjects: Vec<_> = plan.items.values().map(|(s, _)| s.as_str()).collect();
    assert_eq!(subjects, vec!["Ground", "Sketch", "Gone", "Ship"]);
    assert!((p.share().unwrap().get() - 2.0 / 3.0).abs() < 1e-9);
}

#[test]
fn no_plan_is_not_the_same_as_nothing_done() {
    let mut p = Progress::default();
    let mut feed = Feed::default();
    feed.authored(&mut p, T0, Author::Human, agent_requested(1));
    feed.agent(&mut p, &transcript(run(1)), T0 + 1, call("t1", "Bash"));
    assert_eq!(p.share(), None);
    // A plan whose every item was deleted, or every task dropped, measures nothing either.
    feed.agent(
        &mut p,
        &transcript(run(1)),
        T0 + 2,
        todo(&[("gone", PlanStatus::Deleted)]),
    );
    feed.authored(
        &mut p,
        T0 + 3,
        Author::Human,
        set(1, None, 1, TaskState::Dropped),
    );
    assert_eq!(p.share(), None);
    feed.authored(
        &mut p,
        T0 + 4,
        Author::Human,
        set(2, None, 1, TaskState::Pending),
    );
    assert_eq!(p.share().map(|s| s.get()), Some(0.0));
}

#[test]
fn self_reported_tasks_replace_the_observed_plan_of_that_run() {
    let mut p = Progress::default();
    let mut feed = Feed::default();
    feed.authored(&mut p, T0, Author::Human, agent_requested(1));
    feed.agent(
        &mut p,
        &transcript(run(1)),
        T0 + 1,
        todo(&[("a", PlanStatus::Completed), ("b", PlanStatus::Completed)]),
    );
    feed.authored(&mut p, T0 + 2, Author::Agent(run(1)), set(1, None, 1, DONE));
    feed.authored(
        &mut p,
        T0 + 3,
        Author::Agent(run(1)),
        set(2, None, 1, TaskState::InProgress),
    );
    // Counted once, from the self-report; the observed plan is still listed.
    assert_eq!(p.share().unwrap().get(), 0.5);
    assert_eq!(observed_share(&p), vec![(run(1), 1.0, false)]);
}

#[test]
fn a_human_plan_is_the_skeleton_and_subtasks_split_their_parents_share() {
    let mut p = Progress::default();
    let mut feed = Feed::default();
    feed.authored(&mut p, T0, Author::Human, agent_requested(1));
    feed.authored(&mut p, T0 + 1, Author::Human, set(1, None, 3, DONE));
    feed.authored(
        &mut p,
        T0 + 2,
        Author::Human,
        set(2, None, 1, TaskState::Pending),
    );
    feed.authored(
        &mut p,
        T0 + 3,
        Author::Human,
        set(3, None, 5, TaskState::Dropped),
    );
    assert_eq!(p.share().unwrap().get(), 0.75);

    // The agent attaches work under the human's second task: done + pending halves it.
    feed.authored(
        &mut p,
        T0 + 4,
        Author::Agent(run(1)),
        set(21, Some(2), 1, DONE),
    );
    feed.authored(
        &mut p,
        T0 + 5,
        Author::Agent(run(1)),
        set(
            22,
            Some(2),
            1,
            TaskState::Blocked {
                reason: "needs a key".into(),
            },
        ),
    );
    // The agent's observed plan is listed but the human plan decides.
    feed.agent(
        &mut p,
        &transcript(run(1)),
        T0 + 6,
        todo(&[("x", PlanStatus::Completed)]),
    );
    assert_eq!(p.share().unwrap().get(), 0.875);
    assert_eq!(observed_share(&p), vec![(run(1), 1.0, false)]);

    let tree = p.tree();
    let shares: Vec<_> = tree
        .roots
        .iter()
        .filter_map(|n| match n {
            PlanNode::Task {
                key,
                share,
                children,
                ..
            } => Some((**key, share.get(), children.len())),
            PlanNode::Observed { .. } => None,
        })
        .collect();
    assert_eq!(
        shares,
        vec![(task(1), 1.0, 0), (task(2), 0.5, 2), (task(3), 0.0, 0)]
    );
}

#[test]
fn task_patches_assign_only_the_fields_they_carry() {
    let mut p = Progress::default();
    let mut feed = Feed::default();
    feed.authored(
        &mut p,
        T0,
        Author::Human,
        set(1, None, 1, TaskState::Pending),
    );
    feed.authored(
        &mut p,
        T0 + 1,
        Author::Human,
        set(2, Some(1), 1, TaskState::Pending),
    );
    let done = Event::TaskSet {
        task: task(2),
        patch: TaskPatch {
            state: Some(DONE),
            ..TaskPatch::default()
        },
    };
    feed.authored(&mut p, T0 + 2, Author::Human, done);
    assert_eq!(p.share().unwrap().get(), 1.0);

    // `Some(None)` moves the task to the top level; its title is kept.
    let detach = Event::TaskSet {
        task: task(2),
        patch: TaskPatch {
            parent: Some(None),
            ..TaskPatch::default()
        },
    };
    feed.authored(&mut p, T0 + 3, Author::Human, detach);
    assert_eq!(p.share().unwrap().get(), 0.5);
    let tree = p.tree();
    let PlanNode::Task { node, .. } = &tree.roots[1] else {
        panic!("{tree:#?}")
    };
    assert_eq!(node.title, "task 2");
    assert_eq!(node.updated_at, UnixMillis(T0 + 3));
}

#[test]
fn a_parent_cycle_neither_hangs_nor_counts_a_task_twice() {
    let mut p = Progress::default();
    let mut feed = Feed::default();
    feed.authored(&mut p, T0, Author::Human, set(1, Some(2), 1, DONE));
    feed.authored(
        &mut p,
        T0 + 1,
        Author::Human,
        set(2, Some(1), 1, TaskState::Pending),
    );
    feed.authored(&mut p, T0 + 2, Author::Human, set(3, Some(2), 1, DONE));
    let tree = p.tree();
    let mut seen = Vec::new();
    fn walk(n: &PlanNode<'_>, seen: &mut Vec<TaskKey>) {
        if let PlanNode::Task { key, children, .. } = n {
            seen.push(**key);
            children.iter().for_each(|c| walk(c, seen));
        }
    }
    tree.roots.iter().for_each(|n| walk(n, &mut seen));
    seen.sort();
    assert_eq!(seen, vec![task(1), task(2), task(3)]);
    let share = p.share().unwrap().get();
    assert!((0.0..=1.0).contains(&share), "{share}");
}

#[test]
fn activity_follows_the_agents_own_stream() {
    let mut p = Progress::default();
    let mut feed = Feed::default();
    let src = transcript(run(1));
    let now = |p: &Progress| {
        p.activities()
            .map(|(r, a)| (*r, a.clone()))
            .collect::<Vec<_>>()
    };

    feed.authored(&mut p, T0, Author::Human, agent_requested(1));
    feed.authored(&mut p, T0 + 1, Author::Host, started(1));
    assert_eq!(
        now(&p),
        vec![(
            run(1),
            Activity::Starting {
                since: UnixMillis(T0 + 1)
            }
        )]
    );

    let (call_fact, _) = feed.agent(&mut p, &src, T0 + 2, call("t1", "Bash"));
    assert_eq!(
        now(&p),
        vec![(
            run(1),
            Activity::Tool {
                name: ToolName("Bash".into()),
                since: UnixMillis(T0 + 2),
                call: call_fact
            }
        )]
    );
    let wait = AgentWhat::PermissionWait {
        message: "Claude needs your permission to use Bash".into(),
    };
    feed.agent(&mut p, &hook(run(1)), T0 + 3, wait);
    assert_eq!(
        now(&p)[0].1,
        Activity::AwaitingHuman {
            since: UnixMillis(T0 + 3),
            why: WaitReason::Permission {
                message: "Claude needs your permission to use Bash".into()
            }
        }
    );
    feed.agent(&mut p, &src, T0 + 4, result("t1", false));
    assert_eq!(
        now(&p)[0].1,
        Activity::Thinking {
            since: UnixMillis(T0 + 4)
        }
    );
    feed.agent(&mut p, &src, T0 + 5, reply("done"));
    assert_eq!(
        now(&p)[0].1,
        Activity::Writing {
            since: UnixMillis(T0 + 5)
        }
    );
    let usage = AgentWhat::Usage {
        message: None,
        usage: TokenUsage::default(),
    };
    assert!(!feed.agent(&mut p, &src, T0 + 6, usage).1);
    let end = AgentWhat::TurnEnded {
        stop_reason: Some("end_turn".into()),
    };
    feed.agent(&mut p, &src, T0 + 7, end);
    assert_eq!(
        now(&p)[0].1,
        Activity::AwaitingHuman {
            since: UnixMillis(T0 + 7),
            why: WaitReason::TurnEnded
        }
    );
    feed.authored(&mut p, T0 + 8, Author::Host, finished(1, 3));
    let exited = Activity::Exited {
        at: UnixMillis(T0 + 8),
        ok: false,
    };
    assert_eq!(now(&p)[0].1, exited);
    // The tailer may flush the transcript's last lines after the process exits.
    feed.agent(&mut p, &src, T0 + 9, reply("late"));
    assert_eq!(now(&p)[0].1, exited);
}

#[test]
fn only_agent_runs_have_activity() {
    let mut p = Progress::default();
    let mut feed = Feed::default();
    feed.authored(
        &mut p,
        T0,
        Author::Human,
        Event::RunRequested {
            run: run(2),
            purpose: RunPurpose::Import {
                transcript: PathBuf::from("/h/old.jsonl"),
                cli: AgentCli::Claude,
            },
        },
    );
    feed.authored(&mut p, T0 + 1, Author::Host, started(2));
    feed.agent(&mut p, &transcript(run(2)), T0 + 2, reply("from 2025"));
    feed.agent(
        &mut p,
        &transcript(run(2)),
        T0 + 3,
        todo(&[("old", PlanStatus::Completed)]),
    );
    assert_eq!(p.activities().count(), 0);
    // The imported plan still counts toward progress.
    assert_eq!(p.share().unwrap().get(), 1.0);
}

#[test]
fn plan_staleness_counts_tool_calls_since_the_plan_last_changed() {
    let mut p = Progress::default();
    let mut feed = Feed::default();
    let src = transcript(run(1));
    feed.authored(&mut p, T0, Author::Human, agent_requested(1));
    feed.agent(&mut p, &src, T0 + 1, call("a", "Read"));
    feed.agent(&mut p, &src, T0 + 2, call("b", "Read"));
    assert_eq!(p.staleness().collect::<Vec<_>>(), vec![(&run(1), 2)]);
    feed.agent(&mut p, &src, T0 + 3, todo(&[("x", PlanStatus::Pending)]));
    feed.agent(&mut p, &src, T0 + 4, call("c", "Read"));
    assert_eq!(p.staleness().collect::<Vec<_>>(), vec![(&run(1), 1)]);
}

// ---------------------------------------------------------------- runs

fn snapshot_requested(n: u128) -> Event {
    Event::RunRequested {
        run: run(n),
        purpose: RunPurpose::Snapshot { after: None },
    }
}

fn attempt_started(n: u128, attempt: u16) -> Event {
    Event::RunStarted {
        run: run(n),
        attempt: Attempt(attempt),
        pid: Some(4242),
    }
}

fn attempt_finished(n: u128, attempt: u16, outcome: RunOutcome) -> Event {
    Event::RunFinished {
        run: run(n),
        attempt: Attempt(attempt),
        outcome,
    }
}

fn live(runs: &[u128]) -> std::collections::BTreeSet<RunId> {
    runs.iter().map(|n| run(*n)).collect()
}

#[test]
fn a_requested_run_is_due_to_start_until_it_starts() {
    let mut r = Runs::default();
    let mut feed = Feed::default();
    feed.authored(&mut r, T0, Author::Human, agent_requested(1));
    let state = r.get(&run(1)).unwrap();
    assert_eq!(state.requested_by, Author::Human);
    assert_eq!(state.requested_at, UnixMillis(T0));
    assert_eq!(
        r.due(&live(&[])),
        vec![Due::Start {
            run: run(1),
            attempt: Attempt(0)
        }]
    );
    // The host is already starting it.
    assert_eq!(r.due(&live(&[1])), vec![]);

    feed.authored(&mut r, T0 + 1, Author::Host, attempt_started(1, 0));
    assert_eq!(r.due(&live(&[1])), vec![]);
    feed.authored(
        &mut r,
        T0 + 2,
        Author::Host,
        attempt_finished(1, 0, RunOutcome::Exited { code: 0 }),
    );
    assert_eq!(r.due(&live(&[])), vec![]);
    let attempt = &r.get(&run(1)).unwrap().attempts[0];
    assert_eq!(attempt.pid, Some(4242));
    assert_eq!(
        attempt.finished,
        Some((UnixMillis(T0 + 2), RunOutcome::Exited { code: 0 }))
    );
}

#[test]
fn a_host_crash_abandons_the_attempt_and_retries_only_what_is_safe() {
    let mut r = Runs::default();
    let mut feed = Feed::default();
    feed.authored(&mut r, T0, Author::Human, agent_requested(1));
    feed.authored(&mut r, T0, Author::Policy, snapshot_requested(2));
    feed.authored(&mut r, T0 + 1, Author::Host, attempt_started(1, 0));
    feed.authored(&mut r, T0 + 1, Author::Host, attempt_started(2, 0));
    // After a restart nothing is live.
    assert_eq!(
        r.due(&live(&[])),
        vec![
            Due::Abandon {
                run: run(1),
                attempt: Attempt(0)
            },
            Due::Abandon {
                run: run(2),
                attempt: Attempt(0)
            },
        ]
    );
    feed.authored(
        &mut r,
        T0 + 2,
        Author::Host,
        attempt_finished(1, 0, RunOutcome::Abandoned),
    );
    feed.authored(
        &mut r,
        T0 + 2,
        Author::Host,
        attempt_finished(2, 0, RunOutcome::Abandoned),
    );
    // The agent has conversational state and waits for a human; the snapshot retries.
    assert_eq!(
        r.due(&live(&[])),
        vec![Due::Start {
            run: run(2),
            attempt: Attempt(1)
        }]
    );
}

#[test]
fn retries_stop_after_the_last_attempt() {
    let mut r = Runs::default();
    let mut feed = Feed::default();
    feed.authored(&mut r, T0, Author::Policy, snapshot_requested(2));
    for a in 0..MAX_ATTEMPTS {
        feed.authored(&mut r, T0 + 1, Author::Host, attempt_started(2, a));
        feed.authored(
            &mut r,
            T0 + 2,
            Author::Host,
            attempt_finished(2, a, RunOutcome::Abandoned),
        );
    }
    assert_eq!(
        r.get(&run(2)).unwrap().attempts.len(),
        usize::from(MAX_ATTEMPTS)
    );
    assert_eq!(r.due(&live(&[])), vec![]);
}

#[test]
fn a_cancel_kills_a_live_run_and_keeps_an_unstarted_one_from_starting() {
    let mut r = Runs::default();
    let mut feed = Feed::default();
    feed.authored(&mut r, T0, Author::Human, agent_requested(1));
    feed.authored(&mut r, T0, Author::Human, agent_requested(3));
    feed.authored(&mut r, T0 + 1, Author::Host, attempt_started(1, 0));
    feed.authored(
        &mut r,
        T0 + 2,
        Author::Human,
        Event::RunCancelRequested { run: run(1) },
    );
    feed.authored(
        &mut r,
        T0 + 2,
        Author::Human,
        Event::RunCancelRequested { run: run(3) },
    );
    assert!(r.get(&run(1)).unwrap().cancel_requested);
    assert_eq!(r.due(&live(&[1])), vec![Due::Kill { run: run(1) }]);
    let killed = RunOutcome::Killed {
        signal: 15,
        cancelled: true,
    };
    feed.authored(&mut r, T0 + 3, Author::Host, attempt_finished(1, 0, killed));
    assert_eq!(r.due(&live(&[])), vec![]);
}

#[test]
fn an_open_lease_without_a_live_run_is_released() {
    use argus_core::event::{DevicePlatform, LeaseEnd, LeaseProvider};
    use argus_core::ids::LeaseId;
    let mut r = Runs::default();
    let mut feed = Feed::default();
    let lease = LeaseId(Id128(7));
    feed.authored(&mut r, T0, Author::Policy, snapshot_requested(2));
    feed.authored(&mut r, T0 + 1, Author::Host, attempt_started(2, 0));
    feed.authored(
        &mut r,
        T0 + 2,
        Author::Host,
        Event::LeaseOpened {
            lease,
            run: run(2),
            attempt: Attempt(0),
            provider: LeaseProvider::EasSimulator {
                platform: DevicePlatform::Ios,
            },
            name: "argus-2-0".into(),
        },
    );
    feed.authored(
        &mut r,
        T0 + 3,
        Author::Host,
        Event::LeaseBound {
            lease,
            remote_id: "sim-1".into(),
            preview_url: Some("https://example.invalid/p".into()),
        },
    );
    assert_eq!(r.due(&live(&[2])), vec![]);
    assert_eq!(r.unclosed_leases().count(), 1);
    assert_eq!(r.lease(&lease).unwrap().remote_id.as_deref(), Some("sim-1"));
    assert!(r.due(&live(&[])).contains(&Due::Release { lease }));

    feed.authored(
        &mut r,
        T0 + 4,
        Author::Host,
        Event::LeaseClosed {
            lease,
            how: LeaseEnd::Stopped,
        },
    );
    assert_eq!(r.unclosed_leases().count(), 0);
    assert!(!r.due(&live(&[])).contains(&Due::Release { lease }));
}

#[test]
fn artifacts_and_the_latest_agent_are_kept_per_run() {
    use argus_core::ids::BlobId;
    let mut r = Runs::default();
    let mut feed = Feed::default();
    assert!(r.latest_agent().is_none());
    feed.authored(&mut r, T0, Author::Human, agent_requested(1));
    feed.authored(&mut r, T0 + 1, Author::Policy, snapshot_requested(2));
    feed.authored(&mut r, T0 + 2, Author::Human, agent_requested(3));
    feed.authored(&mut r, T0 + 3, Author::Policy, snapshot_requested(4));
    // Request order, not id order, decides.
    assert_eq!(r.latest_agent().map(|(id, _)| *id), Some(run(3)));

    let (fact, changed) = feed.authored(
        &mut r,
        T0 + 4,
        Author::Host,
        Event::ArtifactCaptured {
            run: run(2),
            attempt: Attempt(0),
            blob: BlobId([1; 32]),
            name: "screen.png".into(),
            media_type: "image/png".into(),
            len: 10,
        },
    );
    assert!(changed);
    let artifacts = &r.get(&run(2)).unwrap().artifacts;
    assert_eq!(artifacts.len(), 1);
    assert_eq!(
        (artifacts[0].name.as_str(), &artifacts[0].fact),
        ("screen.png", &fact)
    );
    // Events that are not about runs change nothing here.
    assert!(
        !feed
            .authored(&mut r, T0 + 5, Author::Human, set(1, None, 1, DONE))
            .1
    );
}
