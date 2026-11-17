//! Subscriptions without recompute-everything.
//!
//! A subscription remembers, per ledger its last evaluation read, which views that read
//! touched (`ViewSet::take_reads`) and their combined version then (`ViewSet::stamp`).
//! After a ledger applies a batch, only subscriptions whose stamp for that ledger moved
//! are marked dirty. Dirty ones are re-evaluated at most every ~100 ms (coalesced) and
//! pushed only when their JSON differs from the last push.
//!
//! Dependencies are never declared by hand: they are whatever the op's code read through
//! the view accessors, so they cannot drift from it. The review packet's git diff needs
//! no dependency of its own: it is keyed by the head tree, which is review-view state.

use std::collections::BTreeMap;

use argus_api::Caller;
use argus_core::ids::LedgerRef;
use argus_core::views::{ViewMask, ViewStamp};
use argus_json::Json;

use crate::SubscriptionId;

/// One input of an evaluation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Dep {
    Views {
        ledger: LedgerRef,
        mask: ViewMask,
        stamp: ViewStamp,
    },
    /// The set of jobs (the overview). Opening a job marks these dirty.
    JobSet,
}

pub(crate) struct Subscription {
    pub caller: Caller,
    pub op: String,
    pub input: Json,
    pub deps: Vec<Dep>,
    pub last: Option<Json>,
    pub dirty: bool,
    pub sink: Box<dyn FnMut(Json) + Send>,
}

/// Evaluates one op for a caller; returns its value and what it read.
pub(crate) type Eval<'a> = dyn FnMut(&Caller, &str, &Json) -> (Json, Vec<Dep>) + 'a;

#[derive(Default)]
pub(crate) struct Registry {
    subs: BTreeMap<SubscriptionId, Subscription>,
    next: u64,
}

impl Registry {
    /// Called by a ledger thread after a batch. `stamp_of` evaluates `ViewSet::stamp` on
    /// that thread's views. O(subscriptions that read this ledger); never evaluates.
    pub(crate) fn changed(&mut self, ledger: LedgerRef, stamp_of: &dyn Fn(ViewMask) -> ViewStamp) {
        for sub in self.subs.values_mut() {
            let moved = sub.deps.iter().any(|d| matches!(d, Dep::Views { ledger: l, mask, stamp } if *l == ledger && stamp_of(*mask) != *stamp));
            sub.dirty |= moved;
        }
    }

    pub(crate) fn job_set_changed(&mut self) {
        for sub in self.subs.values_mut() {
            sub.dirty |= sub.deps.contains(&Dep::JobSet);
        }
    }

    /// Coalescer tick: re-evaluate each dirty subscription through `Host::call` with a
    /// handler that records the deps of every read it makes; store the new deps; push
    /// when the JSON differs from `last`.
    pub(crate) fn flush(&mut self, eval: &mut Eval<'_>) {
        todo!("not implemented")
    }
}
