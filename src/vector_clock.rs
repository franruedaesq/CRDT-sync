//! # Vector Clock
//!
//! A vector logical clock that tracks causal ordering of events across a set
//! of named nodes. Unlike scalar Lamport clocks, vector clocks can precisely
//! detect **concurrent** events (events that are causally independent).
//!
//! ## Rules
//! - **Local event / send**: increment the local node's counter and snapshot.
//! - **Receive**: component-wise max with the received vector, then increment
//!   the local component.
//!
//! ## Happened-before relation
//! Timestamp `v1` happened-before `v2` iff for every node `k`:
//! `v1[k] ≤ v2[k]`, and there exists at least one `k` where `v1[k] < v2[k]`.
//!
//! ## Example
//! ```rust
//! use crdt_sync::vector_clock::VectorClock;
//!
//! let mut a = VectorClock::new("A");
//! let mut b = VectorClock::new("B");
//!
//! let v1 = a.increment();   // A sends an event; v1 = {A:1}
//! b.update(&v1);            // B receives it
//! let v2 = b.increment();   // B produces a reply; v2 = {A:1, B:1}
//!
//! // v1 causally precedes v2
//! assert!(v1.happened_before(&v2));
//! assert!(!v2.happened_before(&v1));
//! assert!(!v1.concurrent_with(&v2));
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// An immutable snapshot of a vector clock that can be attached to messages.
///
/// Missing node entries are implicitly 0.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VectorTimestamp(pub HashMap<String, u64>);

impl VectorTimestamp {
    /// Returns `true` if `self` happened-before `other`.
    ///
    /// Formally: for all nodes `k`, `self[k] ≤ other[k]`, and there exists at
    /// least one node `k` where `self[k] < other[k]`.  Missing entries are
    /// treated as 0.
    pub fn happened_before(&self, other: &VectorTimestamp) -> bool {
        // Every component of self must be ≤ the corresponding component of other.
        for (node, &ts) in &self.0 {
            if ts > other.0.get(node).copied().unwrap_or(0) {
                return false;
            }
        }
        // At least one component of self must be strictly less than other.
        // This covers both nodes present in self and nodes present only in other
        // (whose implicit value in self is 0).
        let any_strictly_less = self.0.iter().any(|(node, &ts)| {
            ts < other.0.get(node).copied().unwrap_or(0)
        }) || other.0.iter().any(|(node, &other_ts)| {
            other_ts > self.0.get(node).copied().unwrap_or(0)
        });

        any_strictly_less
    }

    /// Returns `true` if `self` and `other` are concurrent.
    ///
    /// Two timestamps are concurrent when neither happened-before the other
    /// and they are not equal.
    pub fn concurrent_with(&self, other: &VectorTimestamp) -> bool {
        !self.happened_before(other) && !other.happened_before(self) && self != other
    }
}

/// A vector logical clock owned by a single node.
///
/// Each node maintains a counter per peer it has communicated with. Call
/// [`increment`](Self::increment) when generating a local event or sending a
/// message, and [`update`](Self::update) when receiving a message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorClock {
    node_id: String,
    vector: HashMap<String, u64>,
}

impl VectorClock {
    /// Create a new vector clock for `node_id` with all counters at 0.
    pub fn new(node_id: impl Into<String>) -> Self {
        Self {
            node_id: node_id.into(),
            vector: HashMap::new(),
        }
    }

    /// Return the current counter value for this node.
    pub fn local_time(&self) -> u64 {
        self.vector.get(&self.node_id).copied().unwrap_or(0)
    }

    /// Advance the clock for a **local event or send**.
    ///
    /// Increments this node's own counter and returns an immutable
    /// [`VectorTimestamp`] snapshot to attach to the outgoing event.
    pub fn increment(&mut self) -> VectorTimestamp {
        let entry = self.vector.entry(self.node_id.clone()).or_insert(0);
        *entry += 1;
        VectorTimestamp(self.vector.clone())
    }

    /// Advance the clock upon **receiving** a message with timestamp `received`.
    ///
    /// Performs a component-wise max merge with `received`, then increments
    /// the local node's counter so the receive event is causally after the
    /// sender's event.
    pub fn update(&mut self, received: &VectorTimestamp) {
        for (node, &ts) in &received.0 {
            let entry = self.vector.entry(node.clone()).or_insert(0);
            if ts > *entry {
                *entry = ts;
            }
        }
        // Increment the local component to record the receive event.
        let local = self.vector.entry(self.node_id.clone()).or_insert(0);
        *local += 1;
    }

    /// Return the current vector as an immutable [`VectorTimestamp`] snapshot
    /// without advancing the clock.
    pub fn snapshot(&self) -> VectorTimestamp {
        VectorTimestamp(self.vector.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_clock_has_no_entries() {
        let c = VectorClock::new("A");
        assert_eq!(c.local_time(), 0);
        assert_eq!(c.snapshot(), VectorTimestamp(HashMap::new()));
    }

    #[test]
    fn increment_increases_local_counter() {
        let mut c = VectorClock::new("A");
        let v1 = c.increment();
        assert_eq!(v1.0.get("A").copied().unwrap_or(0), 1);
        let v2 = c.increment();
        assert_eq!(v2.0.get("A").copied().unwrap_or(0), 2);
    }

    #[test]
    fn update_merges_and_increments_local() {
        let mut a = VectorClock::new("A");
        let mut b = VectorClock::new("B");

        let v_a = a.increment(); // A: {A:1}
        b.update(&v_a); // B: {A:1, B:1} (merge then increment B)

        let snap = b.snapshot();
        assert_eq!(snap.0.get("A").copied().unwrap_or(0), 1);
        assert_eq!(snap.0.get("B").copied().unwrap_or(0), 1);
    }

    #[test]
    fn happened_before_simple_causal_chain() {
        let mut a = VectorClock::new("A");
        let mut b = VectorClock::new("B");

        let v1 = a.increment(); // {A:1}
        b.update(&v1); // {A:1, B:1}
        let v2 = b.increment(); // {A:1, B:2}

        assert!(v1.happened_before(&v2));
        assert!(!v2.happened_before(&v1));
    }

    #[test]
    fn concurrent_events_detected() {
        let mut a = VectorClock::new("A");
        let mut b = VectorClock::new("B");

        // Both produce events without exchanging messages
        let v_a = a.increment(); // {A:1}
        let v_b = b.increment(); // {B:1}

        assert!(!v_a.happened_before(&v_b));
        assert!(!v_b.happened_before(&v_a));
        assert!(v_a.concurrent_with(&v_b));
    }

    #[test]
    fn equal_timestamps_are_not_concurrent() {
        let c = VectorClock::new("A");
        let v = c.snapshot();
        assert!(!v.concurrent_with(&v));
        assert!(!v.happened_before(&v));
    }

    #[test]
    fn transitive_causal_ordering() {
        let mut a = VectorClock::new("A");
        let mut b = VectorClock::new("B");
        let mut c = VectorClock::new("C");

        let v_a = a.increment(); // {A:1}
        b.update(&v_a); // {A:1, B:1}
        let v_b = b.increment(); // {A:1, B:2}
        c.update(&v_b); // {A:1, B:2, C:1}
        let v_c = c.increment(); // {A:1, B:2, C:2}

        // Transitive: v_a → v_b → v_c
        assert!(v_a.happened_before(&v_b));
        assert!(v_b.happened_before(&v_c));
        assert!(v_a.happened_before(&v_c));
    }

    #[test]
    fn concurrent_then_merge_is_ordered() {
        let mut a = VectorClock::new("A");
        let mut b = VectorClock::new("B");

        let v_a = a.increment(); // {A:1}
        let v_b = b.increment(); // {B:1} – concurrent with v_a

        // Now A receives B's event
        a.update(&v_b); // {A:2, B:1}
        let v_a2 = a.increment(); // {A:3, B:1}

        // v_b happened-before v_a2 (A saw B's event before producing v_a2)
        assert!(v_b.happened_before(&v_a2));
        // v_a is still concurrent with v_b
        assert!(v_a.concurrent_with(&v_b));
    }

    #[test]
    fn serialise_and_deserialise() {
        let mut c = VectorClock::new("node-1");
        c.increment();
        c.increment();
        let snap = c.snapshot();
        let json = serde_json::to_string(&snap).unwrap();
        let snap2: VectorTimestamp = serde_json::from_str(&json).unwrap();
        assert_eq!(snap, snap2);
    }
}
