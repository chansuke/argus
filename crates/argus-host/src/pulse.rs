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
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use argus_api::{ApiError, Caller};
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

/// Where a subscription's values go. Called outside the registry lock.
pub(crate) type Sink = Arc<Mutex<Box<dyn FnMut(Json) + Send>>>;

pub(crate) struct Subscription {
    pub caller: Caller,
    pub op: String,
    pub input: Json,
    pub deps: Vec<Dep>,
    pub last: Option<Json>,
    pub dirty: bool,
    pub sink: Sink,
}

/// Evaluates one op for a caller; returns its value and what it read.
pub(crate) type Eval<'a> =
    dyn FnMut(&Caller, &str, &Json) -> Result<(Json, Vec<Dep>), ApiError> + 'a;

#[derive(Default)]
pub(crate) struct Registry {
    subs: BTreeMap<SubscriptionId, Subscription>,
    next: u64,
    /// Wakes the coalescer when something became dirty.
    wake: Option<Sender<()>>,
}

impl Registry {
    pub(crate) fn new(wake: Sender<()>) -> Self {
        Registry {
            wake: Some(wake),
            ..Registry::default()
        }
    }

    /// Registered dirty: whatever changed between the first evaluation and now is caught
    /// by one more evaluation, pushed only if it differs.
    pub(crate) fn insert(&mut self, mut sub: Subscription) -> SubscriptionId {
        let id = SubscriptionId(self.next);
        self.next += 1;
        sub.dirty = true;
        self.subs.insert(id, sub);
        self.wake();
        id
    }

    /// Stops waking the coalescer, which then ends.
    pub(crate) fn close(&mut self) {
        self.wake = None;
    }

    pub(crate) fn remove(&mut self, id: SubscriptionId) {
        self.subs.remove(&id);
    }

    /// Called by a ledger thread after a batch. `stamp_of` evaluates `ViewSet::stamp` on
    /// that thread's views. O(subscriptions that read this ledger); never evaluates.
    pub(crate) fn changed(&mut self, ledger: LedgerRef, stamp_of: &dyn Fn(ViewMask) -> ViewStamp) {
        let mut any = false;
        for sub in self.subs.values_mut() {
            let moved = sub.deps.iter().any(|d| matches!(d, Dep::Views { ledger: l, mask, stamp } if *l == ledger && stamp_of(*mask) != *stamp));
            any |= moved && !sub.dirty;
            sub.dirty |= moved;
        }
        if any {
            self.wake();
        }
    }

    pub(crate) fn job_set_changed(&mut self) {
        let mut any = false;
        for sub in self.subs.values_mut() {
            let moved = sub.deps.contains(&Dep::JobSet);
            any |= moved && !sub.dirty;
            sub.dirty |= moved;
        }
        if any {
            self.wake();
        }
    }

    fn wake(&self) {
        if let Some(wake) = &self.wake {
            let _ = wake.send(());
        }
    }

    /// The dirty subscriptions, now clean: a change during their evaluation marks them
    /// dirty again.
    fn take_dirty(&mut self) -> Vec<(SubscriptionId, Caller, String, Json)> {
        self.subs
            .iter_mut()
            .filter(|(_, s)| s.dirty)
            .map(|(id, s)| {
                s.dirty = false;
                (*id, s.caller.clone(), s.op.clone(), s.input.clone())
            })
            .collect()
    }

    /// Stores what an evaluation read; returns the push to make if the value changed. An
    /// evaluation that failed keeps the old deps, so the next change retries it.
    fn settle(
        &mut self,
        id: SubscriptionId,
        result: Result<(Json, Vec<Dep>), ApiError>,
    ) -> Option<(Sink, Json)> {
        let sub = self.subs.get_mut(&id)?;
        let (value, deps) = result.ok()?;
        sub.deps = deps;
        if sub.last.as_ref() == Some(&value) {
            return None;
        }
        sub.last = Some(value.clone());
        Some((sub.sink.clone(), value))
    }
}

/// Coalescer tick: re-evaluates each dirty subscription and pushes the values that changed.
/// The lock is never held while evaluating (which waits on ledger threads, which take
/// the lock to mark changes) or while pushing.
pub(crate) fn flush(registry: &Mutex<Registry>, eval: &mut Eval<'_>) {
    let dirty = registry.lock().unwrap().take_dirty();
    for (id, caller, op, input) in dirty {
        let result = eval(&caller, &op, &input);
        let push = registry.lock().unwrap().settle(id, result);
        if let Some((sink, value)) = push {
            (sink.lock().unwrap())(value);
        }
    }
}
