//! # Lamport Clock
//!
//! A scalar logical clock that establishes a total order of events across
//! distributed nodes without relying on physical (wall-clock) time.
//!
//! ## Rules
//! - **Local event / send**: increment the counter by 1 and stamp the event.
//! - **Receive**: advance to `max(local, received) + 1`.
//!
//! These two rules guarantee that if event A causally precedes event B then
//! `timestamp(A) < timestamp(B)`. The converse is not guaranteed — Lamport
//! clocks cannot detect concurrency — but they suffice for LWW and total-order
//! disambiguation in CRDTs.
//!
//! ## Example
//! ```rust
//! use crdt_sync::lamport_clock::LamportClock;
//!
//! let mut node_a = LamportClock::new();
//! let mut node_b = LamportClock::new();
//!
//! // node_a produces an event and sends its timestamp
//! let ts_a = node_a.tick(); // ts_a == 1
//!
//! // node_b receives the event, then produces its own
//! node_b.update(ts_a);      // b's clock advances past ts_a
//! let ts_b = node_b.tick(); // ts_b > ts_a
//!
//! assert!(ts_b > ts_a);
//! ```

use serde::{Deserialize, Serialize};

/// A scalar Lamport logical clock.
///
/// Maintains a single monotonically increasing counter. Use [`tick`](Self::tick)
/// when generating a new event (or sending a message) and [`update`](Self::update)
/// when receiving a message from a remote node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LamportClock {
    time: u64,
}

impl LamportClock {
    /// Create a new clock initialised to 0.
    pub fn new() -> Self {
        Self { time: 0 }
    }

    /// Return the current time value.
    pub fn time(&self) -> u64 {
        self.time
    }

    /// Advance the clock for a **local event or send**.
    ///
    /// Increments the counter by 1 and returns the new timestamp, which
    /// should be attached to the outgoing event or message.
    pub fn tick(&mut self) -> u64 {
        self.time += 1;
        self.time
    }

    /// Advance the clock upon **receiving** a message with timestamp `received`.
    ///
    /// Applies the Lamport receive rule: `time = max(local, received) + 1`,
    /// ensuring the local clock is strictly greater than both the previous
    /// local value and the received timestamp.
    pub fn update(&mut self, received: u64) {
        self.time = self.time.max(received) + 1;
    }
}

impl Default for LamportClock {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_clock_starts_at_zero() {
        let c = LamportClock::new();
        assert_eq!(c.time(), 0);
    }

    #[test]
    fn tick_increments_and_returns_new_time() {
        let mut c = LamportClock::new();
        assert_eq!(c.tick(), 1);
        assert_eq!(c.tick(), 2);
        assert_eq!(c.time(), 2);
    }

    #[test]
    fn update_advances_past_received_timestamp() {
        let mut c = LamportClock::new();
        c.update(5);
        assert_eq!(c.time(), 6); // max(0, 5) + 1
    }

    #[test]
    fn update_does_not_go_backwards() {
        let mut c = LamportClock::new();
        c.tick();
        c.tick();
        c.tick(); // time = 3
        c.update(1); // received a stale timestamp
        assert_eq!(c.time(), 4); // max(3, 1) + 1
    }

    #[test]
    fn tick_after_update_is_ordered_after_received() {
        let mut sender = LamportClock::new();
        let mut receiver = LamportClock::new();

        let ts = sender.tick(); // ts = 1
        receiver.update(ts); // receiver time = 2
        let ts2 = receiver.tick(); // ts2 = 3

        assert!(ts2 > ts);
    }

    #[test]
    fn causal_ordering_across_three_nodes() {
        let mut a = LamportClock::new();
        let mut b = LamportClock::new();
        let mut c = LamportClock::new();

        let ts_a1 = a.tick(); // a=1
        b.update(ts_a1); // b=2
        let ts_b1 = b.tick(); // b=3, ts_b1=3
        c.update(ts_b1); // c=4
        let ts_c1 = c.tick(); // c=5, ts_c1=5

        // Causal chain: a1 → b1 → c1
        assert!(ts_c1 > ts_b1);
        assert!(ts_b1 > ts_a1);
    }

    #[test]
    fn concurrent_events_may_have_equal_timestamps() {
        let mut a = LamportClock::new();
        let mut b = LamportClock::new();

        // Both tick independently with no message exchange
        let ts_a = a.tick(); // a=1
        let ts_b = b.tick(); // b=1

        // Concurrent events can have the same Lamport timestamp
        assert_eq!(ts_a, ts_b);
    }

    #[test]
    fn serialise_and_deserialise() {
        let mut c = LamportClock::new();
        c.tick();
        c.tick();
        let json = serde_json::to_string(&c).unwrap();
        let c2: LamportClock = serde_json::from_str(&json).unwrap();
        assert_eq!(c, c2);
    }
}
