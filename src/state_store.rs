//! # StateStore – Composite Synchronization Engine
//!
//! A high-level synchronization engine that manages a named collection of CRDT
//! registers (LWW), sets (OR-Set) and sequences (RGA) under a single roof.
//!
//! The store:
//! - Assigns Lamport timestamps to every operation it generates.
//! - Produces `Envelope` messages suitable for broadcasting over a network.
//! - Accepts remote `Envelope` messages from other nodes and applies them to
//!   the correct CRDT, advancing the local Lamport clock as needed.
//!
//! ## Example
//! ```rust
//! use crdt_sync::state_store::{StateStore, StoreOp};
//!
//! let mut store_a = StateStore::new("node-A");
//! let mut store_b = StateStore::new("node-B");
//!
//! // node-A writes a register
//! let env = store_a.set_register("robot.x", 10.0_f64);
//!
//! // node-B receives and applies the envelope
//! store_b.apply_envelope(env);
//!
//! assert_eq!(store_b.get_register::<f64>("robot.x"), Some(10.0_f64));
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::lamport_clock::LamportClock;
use crate::lww_register::{LWWOp, LWWRegister};
use crate::or_set::{ORSet, ORSetOp};
use crate::rga::{RGAOp, RGA};

// ── Type-erased storage ───────────────────────────────────────────────────────
//
// We want to store heterogeneous CRDT instances keyed by name.  We achieve this
// by serialising state to/from `serde_json::Value` so the store itself stays
// `'static` and doesn't need GATs or dynamic dispatch.

/// The operation payload carried inside an [`Envelope`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum StoreOp {
    /// A write to a named LWW register (value encoded as JSON).
    Register {
        key: String,
        /// JSON-encoded `LWWOp<serde_json::Value>` so we stay type-erased.
        op: LWWOp<serde_json::Value>,
    },
    /// An operation on a named OR-Set (elements encoded as JSON).
    Set {
        key: String,
        op: ORSetOp<serde_json::Value>,
    },
    /// An operation on a named RGA (elements encoded as JSON).
    Sequence {
        key: String,
        op: RGAOp<serde_json::Value>,
    },
}

/// A network message produced by [`StateStore`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    /// Lamport timestamp of this message.
    pub timestamp: u64,
    /// The originating node.
    pub node_id: String,
    /// The CRDT operation.
    pub op: StoreOp,
}

// ── Internal type-erased CRDT wrappers ───────────────────────────────────────

type JsonRegister = LWWRegister<serde_json::Value>;
type JsonORSet = ORSet<serde_json::Value>;
type JsonRGA = RGA<serde_json::Value>;

/// A multi-CRDT state synchronization engine.
#[derive(Clone)]
pub struct StateStore {
    node_id: String,
    clock: LamportClock,
    registers: HashMap<String, JsonRegister>,
    sets: HashMap<String, JsonORSet>,
    sequences: HashMap<String, JsonRGA>,
}

impl StateStore {
    /// Create a new store for the given node.
    pub fn new(node_id: impl Into<String>) -> Self {
        Self {
            node_id: node_id.into(),
            clock: LamportClock::new(),
            registers: HashMap::new(),
            sets: HashMap::new(),
            sequences: HashMap::new(),
        }
    }

    // ── Clock ─────────────────────────────────────────────────────────────

    fn tick(&mut self) -> u64 {
        self.clock.tick()
    }

    fn update_clock(&mut self, remote_ts: u64) {
        self.clock.update(remote_ts);
    }

    /// Return the current Lamport clock value for this node.
    pub fn clock(&self) -> u64 {
        self.clock.time()
    }

    // ── Register (LWW) ────────────────────────────────────────────────────

    /// Write a value to the named LWW register and return an [`Envelope`] for
    /// broadcasting.
    ///
    /// `T` must be serialisable to JSON.
    pub fn set_register<T: Serialize>(&mut self, key: &str, value: T) -> Envelope {
        let ts = self.tick();
        let json_value = serde_json::to_value(value).expect("value must be serialisable");

        let reg = self
            .registers
            .entry(key.to_owned())
            .or_insert_with(|| LWWRegister::new(self.node_id.clone()));

        let op = reg.set(json_value, ts);
        reg.apply(op.clone());

        Envelope {
            timestamp: ts,
            node_id: self.node_id.clone(),
            op: StoreOp::Register { key: key.to_owned(), op },
        }
    }

    /// Read the current value of a named LWW register.
    ///
    /// Returns `None` if the key doesn't exist.
    /// Panics (via `serde_json::from_value`) if the stored value cannot be
    /// deserialised into `T`.
    pub fn get_register<T: for<'de> Deserialize<'de>>(&self, key: &str) -> Option<T> {
        let reg = self.registers.get(key)?;
        let json_val = reg.get()?.clone();
        serde_json::from_value(json_val).ok()
    }

    // ── OR-Set ────────────────────────────────────────────────────────────

    /// Add an element to the named OR-Set and return an [`Envelope`] for
    /// broadcasting.
    pub fn set_add<T: Serialize>(&mut self, key: &str, value: T) -> Envelope {
        let ts = self.tick();
        let json_value = serde_json::to_value(value).expect("value must be serialisable");

        let set = self
            .sets
            .entry(key.to_owned())
            .or_insert_with(|| ORSet::new(self.node_id.clone()));

        let op = set.add(json_value);
        set.apply(op.clone());

        Envelope {
            timestamp: ts,
            node_id: self.node_id.clone(),
            op: StoreOp::Set { key: key.to_owned(), op },
        }
    }

    /// Remove an element from the named OR-Set.
    ///
    /// Returns `None` if the key doesn't exist or the element isn't present.
    pub fn set_remove<T: Serialize>(&mut self, key: &str, value: T) -> Option<Envelope> {
        let ts = self.tick();
        let json_value = serde_json::to_value(value).expect("value must be serialisable");

        let set = self.sets.get_mut(key)?;
        let op = set.remove(&json_value)?;
        set.apply(op.clone());

        Some(Envelope {
            timestamp: ts,
            node_id: self.node_id.clone(),
            op: StoreOp::Set { key: key.to_owned(), op },
        })
    }

    /// Returns `true` if the named OR-Set contains `value`.
    pub fn set_contains<T: Serialize>(&self, key: &str, value: &T) -> bool {
        let json_value = serde_json::to_value(value).expect("value must be serialisable");
        self.sets
            .get(key)
            .map_or(false, |s: &JsonORSet| s.contains(&json_value))
    }

    /// Return all elements of the named OR-Set as deserialised `T` values.
    pub fn set_items<T: for<'de> Deserialize<'de>>(&self, key: &str) -> Vec<T> {
        match self.sets.get(key) {
            None => Vec::new(),
            Some(s) => s
                .iter()
                .filter_map(|v: &serde_json::Value| serde_json::from_value(v.clone()).ok())
                .collect(),
        }
    }

    // ── RGA (Sequence) ────────────────────────────────────────────────────

    /// Insert an element at `index` in the named RGA and return an [`Envelope`].
    pub fn seq_insert<T: Serialize>(&mut self, key: &str, index: usize, value: T) -> Envelope {
        let ts = self.tick();
        let json_value = serde_json::to_value(value).expect("value must be serialisable");

        let seq = self
            .sequences
            .entry(key.to_owned())
            .or_insert_with(|| RGA::new(self.node_id.clone()));

        let op = seq.insert_op(index, json_value);
        seq.apply(op.clone());

        Envelope {
            timestamp: ts,
            node_id: self.node_id.clone(),
            op: StoreOp::Sequence { key: key.to_owned(), op },
        }
    }

    /// Delete the element at visible `index` in the named RGA.
    ///
    /// Returns `None` if the key doesn't exist or `index` is out of bounds.
    pub fn seq_delete(&mut self, key: &str, index: usize) -> Option<Envelope> {
        let ts = self.tick();

        let seq = self.sequences.get_mut(key)?;
        let op = seq.delete_op(index)?;
        seq.apply(op.clone());

        Some(Envelope {
            timestamp: ts,
            node_id: self.node_id.clone(),
            op: StoreOp::Sequence { key: key.to_owned(), op },
        })
    }

    /// Return all visible elements of the named sequence as deserialised `T`.
    pub fn seq_items<T: for<'de> Deserialize<'de>>(&self, key: &str) -> Vec<T> {
        match self.sequences.get(key) {
            None => Vec::new(),
            Some(s) => s
                .iter()
                .filter_map(|v: &serde_json::Value| serde_json::from_value(v.clone()).ok())
                .collect(),
        }
    }

    /// Return the number of visible elements in the named sequence.
    pub fn seq_len(&self, key: &str) -> usize {
        self.sequences.get(key).map_or(0, |s| s.len())
    }

    // ── Proxy ─────────────────────────────────────────────────────────────

    /// Create a [`StateProxy`] backed by this store.
    ///
    /// The proxy intercepts field mutations and automatically queues CRDT
    /// operations for broadcast, so callers never need to handle [`Envelope`]
    /// values directly.
    ///
    /// [`StateProxy`]: crate::proxy::StateProxy
    pub fn proxy(&mut self) -> crate::proxy::StateProxy<'_> {
        crate::proxy::StateProxy::new(self)
    }

    // ── Apply remote envelopes ────────────────────────────────────────────

    /// Apply an [`Envelope`] received from a remote node.
    ///
    /// Idempotent: replaying the same envelope has no additional effect.
    pub fn apply_envelope(&mut self, env: Envelope) {
        self.update_clock(env.timestamp);

        match env.op {
            StoreOp::Register { key, op } => {
                let reg = self
                    .registers
                    .entry(key)
                    .or_insert_with(|| LWWRegister::new(self.node_id.clone()));
                reg.apply(op);
            }
            StoreOp::Set { key, op } => {
                let set = self
                    .sets
                    .entry(key)
                    .or_insert_with(|| ORSet::new(self.node_id.clone()));
                set.apply(op);
            }
            StoreOp::Sequence { key, op } => {
                let seq = self
                    .sequences
                    .entry(key)
                    .or_insert_with(|| RGA::new(self.node_id.clone()));
                seq.apply(op);
            }
        }
    }

    /// Merge another `StateStore`'s full state into this one (state-based / CvRDT).
    ///
    /// All registers, sets and sequences from `other` are merged into `self`
    /// using the state-based merge of the underlying CRDT.  Keys present only
    /// in `other` are adopted wholesale.  The Lamport clock is advanced to
    /// reflect the merged state.
    ///
    /// - **Commutative**: `a.merge(&b)` and `b.merge(&a)` converge to the same
    ///   composite state.
    /// - **Associative** and **idempotent** by the properties of the underlying
    ///   per-CRDT merges.
    pub fn merge(&mut self, other: &StateStore) {
        self.update_clock(other.clock.time());

        let node_id = self.node_id.clone();

        for (key, other_reg) in &other.registers {
            let reg = self
                .registers
                .entry(key.clone())
                .or_insert_with(|| LWWRegister::new(node_id.clone()));
            reg.merge(other_reg);
        }

        for (key, other_set) in &other.sets {
            let set = self
                .sets
                .entry(key.clone())
                .or_insert_with(|| ORSet::new(node_id.clone()));
            set.merge(other_set);
        }

        for (key, other_seq) in &other.sequences {
            let seq = self
                .sequences
                .entry(key.clone())
                .or_insert_with(|| RGA::new(node_id.clone()));
            seq.merge(other_seq);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_write_and_read() {
        let mut store = StateStore::new("n1");
        store.set_register("robot.x", 10.0_f64);
        assert_eq!(store.get_register::<f64>("robot.x"), Some(10.0));
    }

    #[test]
    fn register_sync_between_nodes() {
        let mut a = StateStore::new("A");
        let mut b = StateStore::new("B");

        let env = a.set_register("robot.x", 42.0_f64);
        b.apply_envelope(env);

        assert_eq!(b.get_register::<f64>("robot.x"), Some(42.0));
    }

    #[test]
    fn register_lww_semantics() {
        let mut a = StateStore::new("A");
        let mut b = StateStore::new("B");

        let env1 = a.set_register("val", 1_i64);
        let env2 = b.set_register("val", 2_i64);

        // Apply in different orders
        a.apply_envelope(env2.clone());
        b.apply_envelope(env1.clone());

        // Both must converge (latest timestamp wins; node-B > node-A for ties)
        assert_eq!(a.get_register::<i64>("val"), b.get_register::<i64>("val"));
    }

    #[test]
    fn or_set_add_remove() {
        let mut store = StateStore::new("n1");
        store.set_add("robots", "robot-A");
        assert!(store.set_contains("robots", &"robot-A"));
        store.set_remove("robots", "robot-A").unwrap();
        assert!(!store.set_contains("robots", &"robot-A"));
    }

    #[test]
    fn or_set_sync_between_nodes() {
        let mut a = StateStore::new("A");
        let mut b = StateStore::new("B");

        let env = a.set_add("fleet", "unit-1");
        b.apply_envelope(env);

        assert!(b.set_contains("fleet", &"unit-1"));
    }

    #[test]
    fn sequence_insert_sync() {
        let mut a = StateStore::new("A");
        let mut b = StateStore::new("B");

        let op1 = a.seq_insert("log", 0, "entry-1");
        let op2 = a.seq_insert("log", 1, "entry-2");

        b.apply_envelope(op2);
        b.apply_envelope(op1);

        assert_eq!(
            a.seq_items::<String>("log"),
            b.seq_items::<String>("log")
        );
    }

    #[test]
    fn sequence_delete_sync() {
        let mut a = StateStore::new("A");
        let mut b = StateStore::new("B");

        let ins = a.seq_insert("items", 0, "x");
        b.apply_envelope(ins);

        let del = a.seq_delete("items", 0).unwrap();
        b.apply_envelope(del);

        assert_eq!(a.seq_items::<String>("items"), Vec::<String>::new());
        assert_eq!(b.seq_items::<String>("items"), Vec::<String>::new());
    }

    #[test]
    fn lamport_clock_advances_on_receive() {
        let mut a = StateStore::new("A");
        let mut b = StateStore::new("B");

        // advance b's clock significantly
        for _ in 0..10 {
            b.set_register("dummy", 0_i32);
        }
        assert_eq!(b.clock(), 10);

        // A receives from B → A's clock must catch up
        let env = b.set_register("x", 99_i32);
        a.apply_envelope(env);
        assert!(a.clock() >= 11);
    }

    #[test]
    fn apply_envelope_is_idempotent() {
        let mut a = StateStore::new("A");
        let mut b = StateStore::new("B");

        let env = a.set_register("k", 7_i32);
        b.apply_envelope(env.clone());
        b.apply_envelope(env.clone());
        b.apply_envelope(env);

        assert_eq!(b.get_register::<i32>("k"), Some(7));
    }

    // ── merge() tests ────────────────────────────────────────────────────────

    #[test]
    fn merge_register_state() {
        let mut a = StateStore::new("A");
        let mut b = StateStore::new("B");

        a.set_register("x", 10_i64);
        b.merge(&a);

        assert_eq!(b.get_register::<i64>("x"), Some(10));
    }

    #[test]
    fn merge_register_is_commutative() {
        let mut a = StateStore::new("A");
        let mut b = StateStore::new("B");

        a.set_register("val", 1_i32);
        b.set_register("val", 2_i32);

        let mut a2 = StateStore::new("A");
        a2.set_register("val", 1_i32);
        let mut b2 = StateStore::new("B");
        b2.set_register("val", 2_i32);

        a.merge(&b2);  // a absorbs b
        b.merge(&a2);  // b absorbs a

        // Both must converge to the same value
        assert_eq!(
            a.get_register::<i32>("val"),
            b.get_register::<i32>("val")
        );
    }

    #[test]
    fn merge_set_state() {
        let mut a = StateStore::new("A");
        let mut b = StateStore::new("B");

        a.set_add("fleet", "unit-1");
        b.set_add("fleet", "unit-2");

        a.merge(&b);

        assert!(a.set_contains("fleet", &"unit-1"));
        assert!(a.set_contains("fleet", &"unit-2"));
    }

    #[test]
    fn merge_sequence_state() {
        let mut a = StateStore::new("A");
        let mut b = StateStore::new("B");

        a.seq_insert("log", 0, "entry-A");
        b.seq_insert("log", 0, "entry-B");

        let b_items: Vec<String> = b.seq_items("log");
        assert_eq!(b_items.len(), 1);

        a.merge(&b);

        // Both entries must appear after merge
        let items: Vec<String> = a.seq_items("log");
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn merge_is_idempotent() {
        let mut a = StateStore::new("A");
        a.set_register("k", 42_i32);
        a.set_add("s", "item");
        a.seq_insert("seq", 0, "v");

        let snapshot = a.clone();
        a.merge(&snapshot);
        a.merge(&snapshot);

        assert_eq!(a.get_register::<i32>("k"), Some(42));
        assert!(a.set_contains("s", &"item"));
        assert_eq!(a.seq_items::<String>("seq"), vec!["v".to_string()]);
    }

    #[test]
    fn merge_advances_clock() {
        let mut a = StateStore::new("A");
        let mut b = StateStore::new("B");

        // Advance b's clock significantly
        for _ in 0..10 {
            b.set_register("dummy", 0_i32);
        }

        let clock_before = a.clock();
        a.merge(&b);
        assert!(a.clock() > clock_before);
    }
}
