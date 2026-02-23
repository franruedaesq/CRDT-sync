//! # LWW-Register (Last-Writer-Wins Register)
//!
//! An operation-based CRDT that stores a single value. When two concurrent writes
//! happen, the write with the **higher timestamp** wins. Ties are broken
//! deterministically by comparing node identifiers lexicographically, so the
//! outcome is always consistent across replicas regardless of message ordering.
//!
//! ## Guarantees
//! - Commutative: applying operations in any order yields the same result.
//! - Idempotent: applying the same operation twice has no extra effect.
//!
//! ## Example
//! ```rust
//! use crdt_sync::lww_register::LWWRegister;
//!
//! let mut a: LWWRegister<i32> = LWWRegister::new("node-A");
//! let mut b: LWWRegister<i32> = LWWRegister::new("node-B");
//!
//! let op_a = a.set_and_apply(10, 1);
//! let op_b = b.set_and_apply(20, 1); // same timestamp → higher node-id wins
//!
//! // Merge both operations into both replicas
//! a.apply(op_b.clone());
//! b.apply(op_a.clone());
//!
//! // Both converge to the same value
//! assert_eq!(a.get(), b.get());
//! ```

use serde::{Deserialize, Serialize};

/// A single write operation produced by [`LWWRegister::set`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LWWOp<T> {
    /// The new value being written.
    pub value: T,
    /// Logical (Lamport) timestamp of this write.
    pub timestamp: u64,
    /// Identifier of the node that produced this operation.
    pub node_id: String,
}

/// Last-Writer-Wins Register.
///
/// `T` must be `Clone` so that the internal value can be read without moving it.
/// Implement `PartialOrd` / `Ord` is **not** required on `T`; ordering is solely
/// based on `(timestamp, node_id)`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LWWRegister<T: Clone> {
    node_id: String,
    /// The currently winning value, together with its winning timestamp and origin.
    state: Option<(T, u64, String)>,
}

impl<T: Clone> LWWRegister<T> {
    /// Create a new, empty register owned by `node_id`.
    pub fn new(node_id: impl Into<String>) -> Self {
        Self {
            node_id: node_id.into(),
            state: None,
        }
    }

    /// Return the current value, or `None` if the register has never been written.
    pub fn get(&self) -> Option<&T> {
        self.state.as_ref().map(|(v, _, _)| v)
    }

    /// Return the timestamp of the currently winning write, or `None`.
    pub fn timestamp(&self) -> Option<u64> {
        self.state.as_ref().map(|(_, ts, _)| *ts)
    }

    /// Generate a **set** operation with the given logical `timestamp`.
    ///
    /// The caller is responsible for supplying a monotonically increasing
    /// timestamp (e.g. a Lamport clock).  The operation is **not** automatically
    /// applied to this replica; call [`apply`](Self::apply) afterwards if you
    /// want local state to reflect the write immediately.
    pub fn set(&self, value: T, timestamp: u64) -> LWWOp<T> {
        LWWOp {
            value,
            timestamp,
            node_id: self.node_id.clone(),
        }
    }

    /// Apply an operation (local or remote) to this replica.
    ///
    /// Returns `true` if the operation was accepted (i.e. it had a newer
    /// timestamp, or the same timestamp with a lexicographically greater
    /// node-id), `false` if it was discarded as stale.
    pub fn apply(&mut self, op: LWWOp<T>) -> bool {
        let wins = match &self.state {
            None => true,
            Some((_, cur_ts, cur_node)) => {
                op.timestamp > *cur_ts
                    || (op.timestamp == *cur_ts && op.node_id > *cur_node)
            }
        };
        if wins {
            self.state = Some((op.value, op.timestamp, op.node_id));
        }
        wins
    }

    /// Convenience: set a value **and** immediately apply it locally.
    ///
    /// Returns the generated operation so it can be broadcast to other replicas.
    pub fn set_and_apply(&mut self, value: T, timestamp: u64) -> LWWOp<T> {
        let op = self.set(value, timestamp);
        self.apply(op.clone());
        op
    }

    /// Merge another register's state into this one (state-based / CvRDT).
    ///
    /// Whichever state carries the higher `(timestamp, node_id)` pair wins,
    /// producing the same result regardless of the order in which replicas are
    /// merged.
    ///
    /// - **Commutative**: `a.merge(&b)` and `b.merge(&a)` converge to the same
    ///   winning value.
    /// - **Associative**: `(a.merge(&b)).merge(&c)` == `a.merge(&b.merge_cloned(&c))`.
    /// - **Idempotent**: `a.merge(&a)` leaves the state unchanged.
    pub fn merge(&mut self, other: &LWWRegister<T>) {
        if let Some((value, timestamp, node_id)) = &other.state {
            let op = LWWOp {
                value: value.clone(),
                timestamp: *timestamp,
                node_id: node_id.clone(),
            };
            self.apply(op);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_register_is_empty() {
        let reg: LWWRegister<i32> = LWWRegister::new("n1");
        assert_eq!(reg.get(), None);
    }

    #[test]
    fn set_and_apply_stores_value() {
        let mut reg: LWWRegister<i32> = LWWRegister::new("n1");
        reg.set_and_apply(42, 1);
        assert_eq!(reg.get(), Some(&42));
    }

    #[test]
    fn higher_timestamp_wins() {
        let mut reg: LWWRegister<i32> = LWWRegister::new("n1");
        reg.set_and_apply(10, 1);
        reg.apply(LWWOp { value: 20, timestamp: 2, node_id: "n2".into() });
        assert_eq!(reg.get(), Some(&20));
    }

    #[test]
    fn lower_timestamp_is_rejected() {
        let mut reg: LWWRegister<i32> = LWWRegister::new("n1");
        reg.set_and_apply(10, 5);
        let accepted = reg.apply(LWWOp { value: 99, timestamp: 3, node_id: "n2".into() });
        assert!(!accepted);
        assert_eq!(reg.get(), Some(&10));
    }

    #[test]
    fn tie_broken_by_node_id() {
        let mut a: LWWRegister<i32> = LWWRegister::new("node-A");
        let mut b: LWWRegister<i32> = LWWRegister::new("node-B");

        // Both write at the same timestamp
        let op_a = a.set_and_apply(10, 1);
        let op_b = b.set_and_apply(20, 1);

        a.apply(op_b.clone());
        b.apply(op_a.clone());

        // node-B > node-A lexicographically, so node-B's value (20) wins everywhere
        assert_eq!(a.get(), Some(&20));
        assert_eq!(b.get(), Some(&20));
    }

    #[test]
    fn apply_is_idempotent() {
        let mut reg: LWWRegister<i32> = LWWRegister::new("n1");
        let op = reg.set_and_apply(7, 1);
        reg.apply(op.clone());
        reg.apply(op);
        assert_eq!(reg.get(), Some(&7));
    }

    #[test]
    fn apply_is_commutative() {
        let ops = vec![
            LWWOp { value: 1, timestamp: 3, node_id: "n1".into() },
            LWWOp { value: 2, timestamp: 1, node_id: "n2".into() },
            LWWOp { value: 3, timestamp: 2, node_id: "n3".into() },
        ];

        let mut reg1: LWWRegister<i32> = LWWRegister::new("n0");
        for op in ops.iter() {
            reg1.apply(op.clone());
        }

        let mut reg2: LWWRegister<i32> = LWWRegister::new("n0");
        for op in ops.iter().rev() {
            reg2.apply(op.clone());
        }

        assert_eq!(reg1.get(), reg2.get()); // both should settle on ts=3 → value 1
    }

    // ── merge() tests ────────────────────────────────────────────────────────

    #[test]
    fn merge_empty_into_non_empty_is_noop() {
        let mut a: LWWRegister<i32> = LWWRegister::new("n1");
        a.set_and_apply(42, 1);
        let b: LWWRegister<i32> = LWWRegister::new("n2");
        a.merge(&b);
        assert_eq!(a.get(), Some(&42));
    }

    #[test]
    fn merge_non_empty_into_empty() {
        let mut a: LWWRegister<i32> = LWWRegister::new("n1");
        let mut b: LWWRegister<i32> = LWWRegister::new("n2");
        b.set_and_apply(99, 5);
        a.merge(&b);
        assert_eq!(a.get(), Some(&99));
        assert_eq!(a.timestamp(), Some(5));
    }

    #[test]
    fn merge_is_commutative() {
        let mut a: LWWRegister<i32> = LWWRegister::new("node-A");
        let mut b: LWWRegister<i32> = LWWRegister::new("node-B");
        a.set_and_apply(10, 3);
        b.set_and_apply(20, 3); // same timestamp → node-B wins lexicographically

        let a2 = a.clone();
        let mut b2 = b.clone();

        a.merge(&b);   // a absorbs b
        b2.merge(&a2); // b absorbs a

        assert_eq!(a.get(), b2.get()); // commutative: same winner
    }

    #[test]
    fn merge_is_associative() {
        let mut a: LWWRegister<i32> = LWWRegister::new("n1");
        let mut b: LWWRegister<i32> = LWWRegister::new("n2");
        let mut c: LWWRegister<i32> = LWWRegister::new("n3");
        a.set_and_apply(1, 1);
        b.set_and_apply(2, 2);
        c.set_and_apply(3, 3);

        // (a merge b) merge c
        let mut ab = a.clone();
        ab.merge(&b);
        let mut ab_c = ab.clone();
        ab_c.merge(&c);

        // a merge (b merge c)
        let mut bc = b.clone();
        bc.merge(&c);
        let mut a_bc = a.clone();
        a_bc.merge(&bc);

        assert_eq!(ab_c.get(), a_bc.get());
    }

    #[test]
    fn merge_is_idempotent() {
        let mut a: LWWRegister<i32> = LWWRegister::new("n1");
        a.set_and_apply(7, 1);
        let snapshot = a.clone();
        a.merge(&snapshot);
        a.merge(&snapshot);
        assert_eq!(a.get(), Some(&7));
        assert_eq!(a.timestamp(), Some(1));
    }

    #[test]
    fn merge_higher_timestamp_wins() {
        let mut a: LWWRegister<i32> = LWWRegister::new("n1");
        let mut b: LWWRegister<i32> = LWWRegister::new("n2");
        a.set_and_apply(10, 1);
        b.set_and_apply(20, 5);

        a.merge(&b);
        assert_eq!(a.get(), Some(&20));
        assert_eq!(a.timestamp(), Some(5));
    }
}
