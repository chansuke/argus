use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap};

use crate::{ActorKey, EventId, Frame, FrameIter, LogError, Seq, Stamp};

/// Merges per-actor iterators into one stream ordered by `(stamp, actor)`.
///
/// Because one sequencer stamps every frame of a ledger, this order equals the order in
/// which the live host applied the frames. That makes replay deterministic and equal to
/// live: a projection sees the same sequence either way. O(N log A) for A actors.
pub fn merge_by_stamp(iters: Vec<FrameIter>) -> MergedFrames {
    MergedFrames {
        iters,
        heap: BinaryHeap::new(),
        primed: false,
    }
}

#[derive(Debug)]
pub struct MergedFrames {
    iters: Vec<FrameIter>,
    heap: BinaryHeap<Reverse<(Stamp, ActorKey, usize)>>,
    primed: bool,
}

impl Iterator for MergedFrames {
    type Item = Result<Frame, LogError>;
    fn next(&mut self) -> Option<Self::Item> {
        // TODO: on first call push (peek_stamp, actor, idx) for each iter; then pop min,
        //       take that iter's next frame, re-push its new peek if any.
        todo!("not implemented")
    }
}

/// "Applied up to" per actor: the next seq each actor is expected to deliver.
///
/// This is the replay-idempotency mechanism. Admitting a frame is an O(log A) map lookup;
/// there is no scan over previously seen ids (the O(N^2) flaw of the dsh timeline plugin,
/// which re-scanned for duplicate ids on every report event).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Cursor {
    next: BTreeMap<ActorKey, Seq>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Admit {
    /// Exactly the next frame for its actor: apply it.
    Fresh,
    /// Already applied (re-delivery after a refresh, or a checkpoint that covers it): skip.
    Seen,
    /// A frame was skipped. Never valid; the caller treats it as corruption.
    Gap { expected: Seq },
}

impl Cursor {
    pub fn admit(&mut self, id: &EventId) -> Admit {
        let expected = self.next.get(&id.actor).copied().unwrap_or(Seq::FIRST);
        if id.seq < expected {
            Admit::Seen
        } else if id.seq == expected {
            self.next.insert(id.actor.clone(), expected.next());
            Admit::Fresh
        } else {
            Admit::Gap { expected }
        }
    }
    pub fn next_for(&self, actor: &ActorKey) -> Seq {
        self.next.get(actor).copied().unwrap_or(Seq::FIRST)
    }
    pub fn iter(&self) -> impl Iterator<Item = (&ActorKey, Seq)> {
        self.next.iter().map(|(k, v)| (k, *v))
    }
}
