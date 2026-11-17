//! What a projection sees: one stored frame, or one observation decoded from it.

use crate::event::{Author, Event};
use crate::ids::{FactId, Stamp, UnixMillis};
use crate::source::{Observation, SourceMeta};

#[derive(Debug)]
pub struct Fact<'a> {
    pub id: FactId,
    /// Append order. Facts reach projections in stamp order, live and on replay alike.
    pub stamp: Stamp,
    /// Domain time: the record's own timestamp when it has one, else the stamp's wall time.
    /// Timelines and analytics key on this.
    pub at: UnixMillis,
    pub body: FactBody<'a>,
}

#[derive(Debug)]
pub enum FactBody<'a> {
    Authored {
        author: &'a Author,
        event: &'a Event,
    },
    Observed {
        source: &'a SourceMeta,
        obs: &'a Observation,
        raw: &'a [u8],
    },
}
