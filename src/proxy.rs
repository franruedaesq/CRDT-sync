//! # StateProxy – Automatic CRDT Operation Interceptor
//!
//! Wraps a [`StateStore`] and provides a proxy API that automatically
//! converts field mutations into CRDT operations without requiring
//! developers to manually handle [`Envelope`] values.
//!
//! This is the Rust equivalent of JavaScript Proxy-based state observation:
//! instead of calling `store.set_register("key", value)` and manually
//! collecting the returned [`Envelope`], you simply call
//! `proxy.set("key", value)` and the proxy intercepts the change,
//! records the CRDT operation, and queues it for broadcast.
//!
//! ## Example
//!
//! ```rust
//! use crdt_sync::state_store::StateStore;
//! use crdt_sync::proxy::StateProxy;
//!
//! let mut store_a = StateStore::new("node-A");
//! let mut store_b = StateStore::new("node-B");
//!
//! // Use the proxy to set fields without manually handling Envelopes.
//! {
//!     let mut proxy = StateProxy::new(&mut store_a);
//!     proxy.set("robot.x", 10.0_f64)
//!          .set("robot.y", 20.0_f64);
//!
//!     // Retrieve queued CRDT operations and broadcast them.
//!     for env in proxy.drain_pending() {
//!         store_b.apply_envelope(env);
//!     }
//! }
//!
//! assert_eq!(store_b.get_register::<f64>("robot.x"), Some(10.0));
//! assert_eq!(store_b.get_register::<f64>("robot.y"), Some(20.0));
//! ```

use serde::{Deserialize, Serialize};

use crate::state_store::{Envelope, StateStore};

/// A proxy over a [`StateStore`] that intercepts field changes and
/// automatically converts them into CRDT operations.
///
/// Pending operations are queued internally and can be retrieved via
/// [`drain_pending`](StateProxy::drain_pending) for broadcasting to other
/// nodes.  This means developers never need to manually capture or forward
/// the [`Envelope`] returned by individual [`StateStore`] methods.
///
/// Create a proxy from an existing store with [`StateProxy::new`] or the
/// convenience method [`StateStore::proxy`].
pub struct StateProxy<'a> {
    store: &'a mut StateStore,
    pending: Vec<Envelope>,
}

impl<'a> StateProxy<'a> {
    /// Create a new proxy backed by `store`.
    pub fn new(store: &'a mut StateStore) -> Self {
        Self {
            store,
            pending: Vec::new(),
        }
    }

    // ── Scalar register (LWW) ─────────────────────────────────────────────

    /// Intercept a scalar-field write.
    ///
    /// The change is automatically recorded as a Last-Writer-Wins register
    /// operation and queued for broadcast.  Returns `&mut Self` so calls can
    /// be chained:
    ///
    /// ```rust
    /// # use crdt_sync::state_store::StateStore;
    /// # use crdt_sync::proxy::StateProxy;
    /// # let mut store = StateStore::new("n");
    /// # let mut proxy = StateProxy::new(&mut store);
    /// proxy.set("x", 1_i32).set("y", 2_i32).set("z", 3_i32);
    /// ```
    pub fn set<T: Serialize>(&mut self, field: &str, value: T) -> &mut Self {
        let env = self.store.set_register(field, value);
        self.pending.push(env);
        self
    }

    /// Read the current value of a scalar field.
    ///
    /// Returns `None` if the field has never been written.
    pub fn get<T: for<'de> Deserialize<'de>>(&self, field: &str) -> Option<T> {
        self.store.get_register(field)
    }

    // ── OR-Set ────────────────────────────────────────────────────────────

    /// Intercept an add to a set field.
    ///
    /// Queued as an OR-Set add operation.  Returns `&mut Self` for chaining.
    pub fn set_add<T: Serialize>(&mut self, field: &str, value: T) -> &mut Self {
        let env = self.store.set_add(field, value);
        self.pending.push(env);
        self
    }

    /// Intercept a removal from a set field.
    ///
    /// Returns `true` if the item was present and the operation was queued,
    /// or `false` if the item wasn't present (no operation generated).
    pub fn set_remove<T: Serialize>(&mut self, field: &str, value: T) -> bool {
        if let Some(env) = self.store.set_remove(field, value) {
            self.pending.push(env);
            true
        } else {
            false
        }
    }

    /// Returns `true` if the named set contains `value`.
    pub fn set_contains<T: Serialize>(&self, field: &str, value: &T) -> bool {
        self.store.set_contains(field, value)
    }

    // ── RGA (Sequence) ────────────────────────────────────────────────────

    /// Intercept an append to a sequence field.
    ///
    /// Inserts `value` at the end of the named sequence and queues the
    /// resulting RGA operation for broadcast.  Returns `&mut Self` for
    /// chaining.
    pub fn seq_push<T: Serialize>(&mut self, field: &str, value: T) -> &mut Self {
        let idx = self.store.seq_len(field);
        let env = self.store.seq_insert(field, idx, value);
        self.pending.push(env);
        self
    }

    /// Intercept an insert at an arbitrary position in a sequence field.
    ///
    /// Returns `&mut Self` for chaining.
    pub fn seq_insert<T: Serialize>(&mut self, field: &str, index: usize, value: T) -> &mut Self {
        let env = self.store.seq_insert(field, index, value);
        self.pending.push(env);
        self
    }

    /// Intercept a deletion from a sequence field.
    ///
    /// Returns `true` if the index was valid and the operation was queued.
    pub fn seq_delete(&mut self, field: &str, index: usize) -> bool {
        if let Some(env) = self.store.seq_delete(field, index) {
            self.pending.push(env);
            true
        } else {
            false
        }
    }

    /// Return all visible elements of the named sequence.
    pub fn seq_items<T: for<'de> Deserialize<'de>>(&self, field: &str) -> Vec<T> {
        self.store.seq_items(field)
    }

    // ── Pending operations ────────────────────────────────────────────────

    /// Drain and return all CRDT operations queued since the last call.
    ///
    /// The returned [`Envelope`]s should be broadcast to all peer nodes via
    /// [`StateStore::apply_envelope`].  After this call the internal queue
    /// is empty.
    pub fn drain_pending(&mut self) -> Vec<Envelope> {
        std::mem::take(&mut self.pending)
    }

    /// Returns the number of CRDT operations currently queued for broadcast.
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    // ── Store access ──────────────────────────────────────────────────────

    /// Borrow the underlying [`StateStore`] immutably.
    pub fn store(&self) -> &StateStore {
        self.store
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Scalar register ───────────────────────────────────────────────────

    #[test]
    fn set_intercepts_and_queues_operation() {
        let mut store = StateStore::new("n1");
        let mut proxy = StateProxy::new(&mut store);
        proxy.set("x", 42_i32);
        assert_eq!(proxy.pending_count(), 1);
    }

    #[test]
    fn set_updates_local_state() {
        let mut store = StateStore::new("n1");
        let mut proxy = StateProxy::new(&mut store);
        proxy.set("x", 42_i32);
        assert_eq!(proxy.get::<i32>("x"), Some(42));
    }

    #[test]
    fn chained_sets_queue_all_operations() {
        let mut store = StateStore::new("n1");
        let mut proxy = StateProxy::new(&mut store);
        proxy.set("a", 1_i32).set("b", 2_i32).set("c", 3_i32);
        assert_eq!(proxy.pending_count(), 3);
    }

    #[test]
    fn drain_pending_returns_and_clears_queue() {
        let mut store = StateStore::new("n1");
        let mut proxy = StateProxy::new(&mut store);
        proxy.set("x", 1_i32).set("y", 2_i32);

        let ops = proxy.drain_pending();
        assert_eq!(ops.len(), 2);
        assert_eq!(proxy.pending_count(), 0);
    }

    #[test]
    fn broadcast_via_drain_pending_syncs_peer() {
        let mut store_a = StateStore::new("A");
        let mut store_b = StateStore::new("B");

        {
            let mut proxy = StateProxy::new(&mut store_a);
            proxy.set("robot.x", 10.0_f64).set("robot.y", 20.0_f64);
            for env in proxy.drain_pending() {
                store_b.apply_envelope(env);
            }
        }

        assert_eq!(store_b.get_register::<f64>("robot.x"), Some(10.0));
        assert_eq!(store_b.get_register::<f64>("robot.y"), Some(20.0));
    }

    #[test]
    fn proxy_convenience_method_on_store() {
        let mut store_a = StateStore::new("A");
        let mut store_b = StateStore::new("B");

        let ops = {
            let mut proxy = store_a.proxy();
            proxy.set("speed", 99_i32);
            proxy.drain_pending()
        };

        for env in ops {
            store_b.apply_envelope(env);
        }

        assert_eq!(store_b.get_register::<i32>("speed"), Some(99));
    }

    // ── OR-Set ────────────────────────────────────────────────────────────

    #[test]
    fn set_add_intercepts_and_queues() {
        let mut store = StateStore::new("n1");
        let mut proxy = StateProxy::new(&mut store);
        proxy.set_add("fleet", "unit-1");
        assert_eq!(proxy.pending_count(), 1);
        assert!(proxy.set_contains("fleet", &"unit-1"));
    }

    #[test]
    fn set_remove_intercepts_present_element() {
        let mut store = StateStore::new("n1");
        let mut proxy = StateProxy::new(&mut store);
        proxy.set_add("fleet", "unit-1");
        proxy.drain_pending();

        let removed = proxy.set_remove("fleet", "unit-1");
        assert!(removed);
        assert_eq!(proxy.pending_count(), 1);
        assert!(!proxy.set_contains("fleet", &"unit-1"));
    }

    #[test]
    fn set_remove_absent_element_returns_false() {
        let mut store = StateStore::new("n1");
        let mut proxy = StateProxy::new(&mut store);
        assert!(!proxy.set_remove("fleet", "ghost"));
        assert_eq!(proxy.pending_count(), 0);
    }

    #[test]
    fn set_operations_sync_peer() {
        let mut store_a = StateStore::new("A");
        let mut store_b = StateStore::new("B");

        {
            let mut proxy = StateProxy::new(&mut store_a);
            proxy.set_add("fleet", "unit-1").set_add("fleet", "unit-2");
            for env in proxy.drain_pending() {
                store_b.apply_envelope(env);
            }
        }

        assert!(store_b.set_contains("fleet", &"unit-1"));
        assert!(store_b.set_contains("fleet", &"unit-2"));
    }

    // ── RGA sequence ──────────────────────────────────────────────────────

    #[test]
    fn seq_push_intercepts_and_queues() {
        let mut store = StateStore::new("n1");
        let mut proxy = StateProxy::new(&mut store);
        proxy.seq_push("log", "entry-1");
        assert_eq!(proxy.pending_count(), 1);
        assert_eq!(proxy.seq_items::<String>("log"), vec!["entry-1"]);
    }

    #[test]
    fn seq_push_appends_in_order() {
        let mut store = StateStore::new("n1");
        let mut proxy = StateProxy::new(&mut store);
        proxy
            .seq_push("log", "a")
            .seq_push("log", "b")
            .seq_push("log", "c");
        assert_eq!(proxy.seq_items::<String>("log"), vec!["a", "b", "c"]);
    }

    #[test]
    fn seq_delete_intercepts_present_index() {
        let mut store = StateStore::new("n1");
        let mut proxy = StateProxy::new(&mut store);
        proxy.seq_push("log", "x");
        proxy.drain_pending();

        let deleted = proxy.seq_delete("log", 0);
        assert!(deleted);
        assert_eq!(proxy.pending_count(), 1);
        assert!(proxy.seq_items::<String>("log").is_empty());
    }

    #[test]
    fn seq_delete_out_of_bounds_returns_false() {
        let mut store = StateStore::new("n1");
        let mut proxy = StateProxy::new(&mut store);
        assert!(!proxy.seq_delete("log", 5));
        assert_eq!(proxy.pending_count(), 0);
    }

    #[test]
    fn seq_operations_sync_peer() {
        let mut store_a = StateStore::new("A");
        let mut store_b = StateStore::new("B");

        {
            let mut proxy = StateProxy::new(&mut store_a);
            proxy.seq_push("events", "boot").seq_push("events", "ready");
            for env in proxy.drain_pending() {
                store_b.apply_envelope(env);
            }
        }

        assert_eq!(
            store_b.seq_items::<String>("events"),
            vec!["boot", "ready"]
        );
    }

    // ── Mixed usage ───────────────────────────────────────────────────────

    #[test]
    fn mixed_operations_all_queued() {
        let mut store = StateStore::new("n1");
        let mut proxy = StateProxy::new(&mut store);
        proxy.set("name", "robot-1");
        proxy.set_add("tags", "active");
        proxy.seq_push("log", "started");
        assert_eq!(proxy.pending_count(), 3);
    }

    #[test]
    fn multiple_drain_calls_accumulate_separately() {
        let mut store = StateStore::new("n1");
        let mut proxy = StateProxy::new(&mut store);

        proxy.set("a", 1_i32);
        let batch1 = proxy.drain_pending();
        assert_eq!(batch1.len(), 1);

        proxy.set("b", 2_i32).set("c", 3_i32);
        let batch2 = proxy.drain_pending();
        assert_eq!(batch2.len(), 2);

        assert_eq!(proxy.pending_count(), 0);
    }
}
