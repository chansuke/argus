use std::time::{SystemTime, UNIX_EPOCH};

/// Append-order timestamp. Totally ordered within one ledger because one [`Clock`]
/// (the ledger's sequencer) issues every stamp in it. Wall time is kept for display and
/// coarse range queries; `logical` breaks ties and absorbs a wall clock that went backwards.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Stamp {
    pub wall_ns: u64,
    pub logical: u32,
}

/// Hybrid logical clock for one ledger. Never issues a stamp <= one it issued before,
/// including across restarts, because it is seeded from the max stamp found in the logs.
#[derive(Debug)]
pub struct Clock {
    last: Option<Stamp>,
}

impl Clock {
    pub fn starting_after(last: Option<Stamp>) -> Self {
        Self { last }
    }

    pub fn tick(&mut self) -> Stamp {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as u64);
        let next = match self.last {
            Some(last) if now <= last.wall_ns => Stamp {
                wall_ns: last.wall_ns,
                logical: last.logical + 1,
            },
            _ => Stamp {
                wall_ns: now,
                logical: 0,
            },
        };
        self.last = Some(next);
        next
    }
}
