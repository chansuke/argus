//! Skill candidates on the meta ledger: proposed by an insight run's answer, then
//! dismissed, or accepted into a skill write that finishes with a file.

use std::path::PathBuf;

use argus_core::event::{Author, Event, LlmCall, RunOutcome, RunPurpose, SkillDraft};
use argus_core::ids::{
    Attempt, FactId, FactRef, FindingId, Id128, JobId, LedgerRef, SkillName, UnixMillis,
};
use argus_core::period::Period;
use argus_core::source::AgentWhat;
use argus_core::views::insights::{CandidateStatus, Insights};

mod common;
use common::*;

/// The tag table an insight prompt ends with (`adapters::llm::insight_prompt`).
fn prompt() -> String {
    format!(
        "...\n\nargus evidence index (read by argus, not by you):\nE1 {} human:0.0\n",
        Id128(1)
    )
}

const ANSWER: &str = "Two lessons.\n\n```json\n[\
    {\"name\": \"run-fmt-first\", \"description\": \"Use before tests.\", \"why\": \"fmt failed twice\", \"body\": \"Run cargo fmt.\", \"evidence\": [\"E1\"]},\
    {\"name\": \"read-errors\", \"description\": \"Use when a command fails.\", \"why\": \"retries\", \"body\": \"Read the error.\", \"evidence\": []}\
    ]\n```\n";

fn insight_requested(n: u128) -> Event {
    Event::RunRequested {
        run: run(n),
        purpose: RunPurpose::Insight {
            period: Period {
                from: UnixMillis(0),
                to: UnixMillis(T0),
            },
            llm: LlmCall {
                prompt: prompt(),
                ..llm()
            },
        },
    }
}

fn answered(view: &mut Insights, feed: &mut Feed, n: u128, text: &str) -> FactId {
    let result = AgentWhat::Result {
        text: text.into(),
        is_error: false,
    };
    feed.agent(view, &headless(run(n)), T0 + 1, result).0
}

#[test]
fn an_insight_answer_proposes_one_candidate_per_entry_with_its_evidence() {
    let mut view = Insights::default();
    let mut feed = Feed::default();
    feed.authored(&mut view, T0, Author::Human, insight_requested(1));
    let fact = answered(&mut view, &mut feed, 1, ANSWER);

    let open: Vec<_> = view.open().collect();
    assert_eq!(open.len(), 2);
    assert_eq!(
        open[0].id,
        FindingId {
            fact: fact.clone(),
            index: 0
        }
    );
    assert_eq!(open[0].raw.name, "run-fmt-first");
    assert_eq!(
        open[0].raw.evidence,
        [FactRef {
            ledger: LedgerRef::Job(JobId(Id128(1))),
            fact: FactId::parse("human:0.0").unwrap(),
        }]
    );
    assert!(matches!(open[1].status, CandidateStatus::Proposed));
}

#[test]
fn only_a_readable_answer_of_an_insight_run_proposes_anything() {
    let mut view = Insights::default();
    let mut feed = Feed::default();
    // Not an insight run.
    answered(&mut view, &mut feed, 9, ANSWER);
    feed.authored(&mut view, T0, Author::Human, insight_requested(1));
    answered(&mut view, &mut feed, 1, "no block");
    let failed = AgentWhat::Result {
        text: ANSWER.into(),
        is_error: true,
    };
    feed.agent(&mut view, &headless(run(1)), T0 + 2, failed);
    assert_eq!(view.open().count(), 0);
}

#[test]
fn a_candidate_is_dismissed_or_accepted_and_then_written() {
    let mut view = Insights::default();
    let mut feed = Feed::default();
    feed.authored(&mut view, T0, Author::Human, insight_requested(1));
    let fact = answered(&mut view, &mut feed, 1, ANSWER);
    let [first, second] = [0, 1].map(|index| FindingId {
        fact: fact.clone(),
        index,
    });

    let (_, changed) = feed.authored(
        &mut view,
        T0 + 2,
        Author::Human,
        Event::CandidateDismissed {
            candidate: second.clone(),
        },
    );
    assert!(changed);
    assert!(matches!(
        view.get(&second).unwrap().status,
        CandidateStatus::Dismissed
    ));

    let draft = SkillDraft {
        from: Some(first.clone()),
        name: SkillName::parse("run-fmt-first").unwrap(),
        description: "Use before tests.".into(),
        body: "Run cargo fmt.".into(),
        target: PathBuf::from("/repo/.claude/skills"),
    };
    let write = Event::RunRequested {
        run: run(2),
        purpose: RunPurpose::WriteSkill(draft.clone()),
    };
    feed.authored(&mut view, T0 + 3, Author::Human, write);
    let CandidateStatus::Accepted { run: by, draft: d } = &view.get(&first).unwrap().status else {
        panic!("{:?}", view.get(&first))
    };
    assert_eq!((*by, d), (run(2), &draft));
    assert_eq!(view.open().count(), 0);

    let path = PathBuf::from("/repo/.claude/skills/run-fmt-first/SKILL.md");
    let written = Event::RunFinished {
        run: run(2),
        attempt: Attempt(0),
        outcome: RunOutcome::SkillWritten { path: path.clone() },
    };
    feed.authored(&mut view, T0 + 4, Author::Host, written);
    let CandidateStatus::Written { path: p } = &view.get(&first).unwrap().status else {
        panic!("{:?}", view.get(&first))
    };
    assert_eq!(p, &path);
}

#[test]
fn a_dismissal_of_an_unknown_candidate_changes_nothing() {
    let mut view = Insights::default();
    let mut feed = Feed::default();
    let unknown = FindingId {
        fact: FactId::parse("host:99.0").unwrap(),
        index: 0,
    };
    let (_, changed) = feed.authored(
        &mut view,
        T0,
        Author::Human,
        Event::CandidateDismissed { candidate: unknown },
    );
    assert!(!changed);
}
