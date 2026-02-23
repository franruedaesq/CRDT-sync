//! # `crdt_state!` – Typed State Proxy Macro
//!
//! Generates a **typed state proxy** struct from a plain struct declaration.
//! Each named field becomes a pair of `set_<field>` / `get_<field>` methods
//! backed by a [`StateProxy`](crate::proxy::StateProxy) (and therefore a
//! [`StateStore`](crate::state_store::StateStore)).
//!
//! This is the Rust-macro equivalent of JavaScript Proxy-based state
//! observation: developers work with ordinary field names and concrete types
//! rather than raw string keys and `serde_json::Value`.  Every mutation is
//! automatically converted into a CRDT operation and queued for broadcast,
//! with no manual [`Envelope`](crate::state_store::Envelope) handling required.
//!
//! ## Example
//!
//! ```rust
//! use crdt_sync::state_store::StateStore;
//! use crdt_sync::crdt_state;
//!
//! crdt_state! {
//!     pub struct RobotState {
//!         x: f64,
//!         y: f64,
//!         name: String,
//!     }
//! }
//!
//! let mut store_a = StateStore::new("node-A");
//! let mut store_b = StateStore::new("node-B");
//!
//! let ops = {
//!     let mut state = RobotState::new(&mut store_a);
//!     state.set_x(10.0).set_y(20.0).set_name("robot-1".to_string());
//!     state.drain_pending()
//! };
//!
//! for env in ops {
//!     store_b.apply_envelope(env);
//! }
//!
//! assert_eq!(store_b.get_register::<f64>("x"), Some(10.0));
//! assert_eq!(store_b.get_register::<f64>("y"), Some(20.0));
//! assert_eq!(store_b.get_register::<String>("name"), Some("robot-1".to_string()));
//! ```

/// Generate a typed state proxy struct from a struct-like field declaration.
///
/// Each field `foo: T` in the declaration produces:
/// - `set_foo(&mut self, value: T) -> &mut Self` – intercepts the write,
///   records it as a LWW-register CRDT operation, and queues it for broadcast.
/// - `get_foo(&self) -> Option<T>` – reads the current value from the
///   underlying [`StateStore`](crate::state_store::StateStore).
///
/// The generated struct also exposes:
/// - `new(store: &mut StateStore) -> Self`
/// - `drain_pending() -> Vec<Envelope>` – drain queued ops for broadcasting.
/// - `pending_count() -> usize` – number of ops currently queued.
/// - `store() -> &StateStore` – read-only access to the underlying store.
///
/// # Example
///
/// ```rust
/// use crdt_sync::state_store::StateStore;
/// use crdt_sync::crdt_state;
///
/// crdt_state! {
///     /// A state object for a single robot.
///     pub struct RobotState {
///         speed: f64,
///         active: bool,
///     }
/// }
///
/// let mut store = StateStore::new("n1");
/// let mut state = RobotState::new(&mut store);
///
/// state.set_speed(42.0).set_active(true);
/// assert_eq!(state.get_speed(), Some(42.0));
/// assert_eq!(state.pending_count(), 2);
/// ```
#[macro_export]
macro_rules! crdt_state {
    (
        $(#[$meta:meta])*
        $vis:vis struct $name:ident {
            $($field:ident : $ftype:ty),* $(,)?
        }
    ) => {
        $(#[$meta])*
        $vis struct $name<'__store> {
            proxy: $crate::proxy::StateProxy<'__store>,
        }

        impl<'__store> $name<'__store> {
            /// Create a new typed state proxy backed by `store`.
            pub fn new(store: &'__store mut $crate::state_store::StateStore) -> Self {
                Self { proxy: $crate::proxy::StateProxy::new(store) }
            }

            $(
                ::paste::paste! {
                    /// Intercept a write to this field and queue a CRDT operation.
                    /// Returns `&mut Self` so calls can be chained.
                    pub fn [<set_ $field>](&mut self, value: $ftype) -> &mut Self {
                        self.proxy.set(stringify!($field), value);
                        self
                    }

                    /// Read the current value of this field from the store.
                    pub fn [<get_ $field>](&self) -> Option<$ftype> {
                        self.proxy.get(stringify!($field))
                    }
                }
            )*

            /// Drain and return all CRDT operations queued since the last call.
            ///
            /// The returned envelopes should be broadcast to all peer nodes via
            /// [`StateStore::apply_envelope`](
            ///     $crate::state_store::StateStore::apply_envelope).
            pub fn drain_pending(&mut self) -> Vec<$crate::state_store::Envelope> {
                self.proxy.drain_pending()
            }

            /// Number of CRDT operations currently queued for broadcast.
            pub fn pending_count(&self) -> usize {
                self.proxy.pending_count()
            }

            /// Read-only access to the underlying [`StateStore`](
            ///     $crate::state_store::StateStore).
            pub fn store(&self) -> &$crate::state_store::StateStore {
                self.proxy.store()
            }
        }
    };
}

#[cfg(test)]
mod tests {
    use crate::state_store::StateStore;

    // Declare a typed proxy struct using the macro.
    crdt_state! {
        pub struct RobotState {
            x: f64,
            y: f64,
            speed: f64,
            name: String,
            active: bool,
        }
    }

    // ── Basic set / get ────────────────────────────────────────────────────

    #[test]
    fn set_stores_value_locally() {
        let mut store = StateStore::new("n1");
        let mut state = RobotState::new(&mut store);
        state.set_x(10.0);
        assert_eq!(state.get_x(), Some(10.0));
    }

    #[test]
    fn get_returns_none_before_set() {
        let mut store = StateStore::new("n1");
        let state = RobotState::new(&mut store);
        assert_eq!(state.get_x(), None);
    }

    #[test]
    fn set_returns_self_for_chaining() {
        let mut store = StateStore::new("n1");
        let mut state = RobotState::new(&mut store);
        state.set_x(1.0).set_y(2.0).set_speed(3.0);
        assert_eq!(state.get_x(), Some(1.0));
        assert_eq!(state.get_y(), Some(2.0));
        assert_eq!(state.get_speed(), Some(3.0));
    }

    #[test]
    fn set_queues_crdt_operation() {
        let mut store = StateStore::new("n1");
        let mut state = RobotState::new(&mut store);
        state.set_x(5.0);
        assert_eq!(state.pending_count(), 1);
    }

    #[test]
    fn multiple_sets_queue_all_operations() {
        let mut store = StateStore::new("n1");
        let mut state = RobotState::new(&mut store);
        state.set_x(1.0).set_y(2.0).set_speed(3.0);
        assert_eq!(state.pending_count(), 3);
    }

    // ── drain_pending ─────────────────────────────────────────────────────

    #[test]
    fn drain_pending_returns_correct_count() {
        let mut store = StateStore::new("n1");
        let mut state = RobotState::new(&mut store);
        state.set_x(1.0).set_y(2.0);
        let ops = state.drain_pending();
        assert_eq!(ops.len(), 2);
    }

    #[test]
    fn drain_pending_clears_the_queue() {
        let mut store = StateStore::new("n1");
        let mut state = RobotState::new(&mut store);
        state.set_x(1.0);
        state.drain_pending();
        assert_eq!(state.pending_count(), 0);
    }

    // ── Peer synchronisation via drain_pending ────────────────────────────

    #[test]
    fn ops_sync_to_peer_store() {
        let mut store_a = StateStore::new("A");
        let mut store_b = StateStore::new("B");

        let ops = {
            let mut state = RobotState::new(&mut store_a);
            state.set_x(10.0).set_y(20.0).set_name("robot-1".to_string());
            state.drain_pending()
        };

        for env in ops {
            store_b.apply_envelope(env);
        }

        assert_eq!(store_b.get_register::<f64>("x"), Some(10.0));
        assert_eq!(store_b.get_register::<f64>("y"), Some(20.0));
        assert_eq!(
            store_b.get_register::<String>("name"),
            Some("robot-1".to_string())
        );
    }

    #[test]
    fn string_field_round_trips() {
        let mut store = StateStore::new("n1");
        let mut state = RobotState::new(&mut store);
        state.set_name("hello".to_string());
        assert_eq!(state.get_name(), Some("hello".to_string()));
    }

    #[test]
    fn bool_field_round_trips() {
        let mut store = StateStore::new("n1");
        let mut state = RobotState::new(&mut store);
        state.set_active(true);
        assert_eq!(state.get_active(), Some(true));
    }

    // ── store() accessor ──────────────────────────────────────────────────

    #[test]
    fn store_accessor_reflects_mutations() {
        let mut store = StateStore::new("n1");
        let mut state = RobotState::new(&mut store);
        state.set_speed(99.0);
        assert_eq!(state.store().get_register::<f64>("speed"), Some(99.0));
    }

    // ── Multiple drain batches ─────────────────────────────────────────────

    #[test]
    fn successive_drain_calls_accumulate_separately() {
        let mut store = StateStore::new("n1");
        let mut state = RobotState::new(&mut store);

        state.set_x(1.0);
        let batch1 = state.drain_pending();
        assert_eq!(batch1.len(), 1);

        state.set_y(2.0).set_speed(3.0);
        let batch2 = state.drain_pending();
        assert_eq!(batch2.len(), 2);

        assert_eq!(state.pending_count(), 0);
    }

    // ── Doc-test style: two-node scenario ─────────────────────────────────

    #[test]
    fn two_node_scenario() {
        let mut store_a = StateStore::new("node-A");
        let mut store_b = StateStore::new("node-B");

        let ops = {
            let mut state = RobotState::new(&mut store_a);
            state
                .set_x(3.0)
                .set_y(4.0)
                .set_speed(5.0)
                .set_name("unit-7".to_string())
                .set_active(true);
            state.drain_pending()
        };

        for env in ops {
            store_b.apply_envelope(env);
        }

        assert_eq!(store_b.get_register::<f64>("x"), Some(3.0));
        assert_eq!(store_b.get_register::<f64>("y"), Some(4.0));
        assert_eq!(store_b.get_register::<f64>("speed"), Some(5.0));
        assert_eq!(
            store_b.get_register::<String>("name"),
            Some("unit-7".to_string())
        );
        assert_eq!(store_b.get_register::<bool>("active"), Some(true));
    }
}
