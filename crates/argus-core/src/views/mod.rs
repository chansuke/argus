//! Projections: pure folds over facts. No IO, no clock, no randomness: given the same
//! facts in the same order they produce the same state, which is what makes replay equal
//! live and makes checkpoints safe to discard.
//!
//! Projections do not dedup anything: the ledger's `Applied` state guarantees each frame
//! and each agent record reaches them once.
//!
//! Read-time compositions (`glance`, `packet`) are pure functions over several views.
//! They are computed when asked, never stored or synced.

pub mod checks;
pub mod glance;
pub mod insights;
pub mod packet;
pub mod progress;
pub mod review;
pub mod runs;
pub mod stats;
pub mod timeline;
pub mod transcript;

use std::cell::Cell;

use crate::event::{Author, Event, LedgerKind, Rejected};
use crate::fact::{Fact, FactBody};
use crate::source::{SourceFamily, SourceMeta};

use checks::Checks;
use insights::Insights;
use progress::Progress;
use review::ReviewBoard;
use runs::Runs;
use stats::JobStats;
use timeline::Timeline;
use transcript::Transcript;

pub trait Projection: Default {
    const NAME: &'static str;
    /// Bump when `apply` changes meaning; part of the checkpoint key.
    const VERSION: u32;
    /// Source families this projection reads. Logs of other families are not even opened
    /// on replay (terminal bytes dominate disk). Authored events reach every projection.
    const READS: &'static [SourceFamily];
    /// Folds one fact. Returns whether anything readable through the view changed. This
    /// drives subscription recompute, so return `true` when unsure.
    fn apply(&mut self, fact: &Fact<'_>) -> bool;
}

fn reads<P: Projection>(source: &SourceMeta) -> bool {
    P::READS.contains(&source.kind.family())
}

/// Projections cheap enough to persist and expensive enough to need it (store-wide
/// analytics over hundreds of jobs). A checkpoint is usable only if its view version,
/// the decoder versions of every source kind it reads, and its `Applied` state (cursor
/// <= each read log's durable length) all still hold; otherwise it is rebuilt.
pub trait Checkpoint: Projection {
    fn save(&self) -> Vec<u8>;
    fn load(bytes: &[u8]) -> Option<Self>;
}

/// Which views of a set a read touched, one bit per view in declaration order.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct ViewMask(u32);

/// Sum of the versions of the views in a mask. Versions only grow, so the sum changes
/// exactly when one of those views changed.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct ViewStamp(u64);

/// A fixed set of views a ledger keeps live.
pub trait ViewSet: Default {
    const KIND: LedgerKind;
    fn wants(source: &SourceMeta) -> bool;
    fn apply(&mut self, fact: &Fact<'_>);
    /// View-dependent admission of an authored event, run by `Ledger::submit` on the
    /// sequencer right before the append, so the check and the append are atomic.
    fn admit(&self, author: &Author, event: &Event) -> Result<(), Rejected>;
    /// Views read through accessors since the last call (which resets it). The host takes
    /// it before and after evaluating a subscription: a subscription depends on exactly
    /// the views its code read, so there is no dependency list to keep in sync.
    fn take_reads(&self) -> ViewMask;
    fn stamp(&self, mask: ViewMask) -> ViewStamp;
}

/// One list per ledger kind generates the struct, one accessor per view (which records
/// the read), a version per view (bumped when its `apply` reports a change), the replay
/// filter, and the fan-out. A view cannot be declared but forgotten in `apply`, and a
/// subscription cannot read a view without depending on it.
macro_rules! view_set {
    ($(#[$m:meta])* $name:ident ($slot:ident): $kind:expr, admit = $admit:path => {
        $($(#[$fm:meta])* $field:ident : $ty:ty),* $(,)?
    }) => {
        #[allow(non_camel_case_types)]
        #[derive(Clone, Copy, Debug)]
        enum $slot { $($field),* }

        $(#[$m])*
        #[derive(Default)]
        pub struct $name {
            $($field: $ty,)*
            versions: [u64; 16],
            reads: Cell<ViewMask>,
        }

        impl $name {
            $( $(#[$fm])* pub fn $field(&self) -> &$ty {
                let ViewMask(m) = self.reads.get();
                self.reads.set(ViewMask(m | 1 << $slot::$field as u32));
                &self.$field
            } )*
        }

        impl ViewSet for $name {
            const KIND: LedgerKind = $kind;
            fn wants(source: &SourceMeta) -> bool {
                false $(|| reads::<$ty>(source))*
            }
            fn apply(&mut self, fact: &Fact<'_>) {
                let source = match &fact.body {
                    FactBody::Observed { source, .. } => Some(*source),
                    FactBody::Authored { .. } => None,
                };
                $(if source.is_none_or(reads::<$ty>) && self.$field.apply(fact) {
                    self.versions[$slot::$field as usize] += 1;
                })*
            }
            fn admit(&self, author: &Author, event: &Event) -> Result<(), Rejected> {
                $admit(self, author, event)
            }
            fn take_reads(&self) -> ViewMask {
                self.reads.replace(ViewMask::default())
            }
            fn stamp(&self, ViewMask(mask): ViewMask) -> ViewStamp {
                ViewStamp(self.versions.iter().enumerate().filter(|(i, _)| mask & 1 << i != 0).map(|(_, v)| v).sum())
            }
        }
    };
}

view_set! {
    /// Everything shown about one job.
    JobViews(JobView): LedgerKind::Job, admit = review::admit => {
        runs: Runs,
        transcript: Transcript,
        timeline: Timeline,
        progress: Progress,
        review: ReviewBoard,
        checks: Checks,
        stats: JobStats,
    }
}

view_set! {
    MetaViews(MetaView): LedgerKind::Meta, admit = admit_any => {
        runs: Runs,
        insights: Insights,
    }
}

fn admit_any(_: &MetaViews, _: &Author, _: &Event) -> Result<(), Rejected> {
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GateState {
    NotConfigured,
    Pending,
    Green,
    Red {
        open: u32,
    },
    /// The latest result judged an older tree than the head snapshot.
    Stale,
}
