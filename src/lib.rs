//! # crdt-sync
//!
//! A generic state synchronization engine based on CRDTs (Conflict-free Replicated Data Types).
//!
//! This library provides operation-based CRDTs (CmRDTs) suitable for real-time delta-sync
//! scenarios such as keeping a UI (digital twin) and backend (AI/robotics logic) in perfect
//! harmony without data loss or locking.
//!
//! ## CRDT types provided
//!
//! - [`lww_register::LWWRegister`] – Last-Writer-Wins Register for scalar key-value properties.
//! - [`or_set::ORSet`] – Observed-Remove Set for collections of unique items.
//! - [`rga::RGA`] – Replicated Growable Array for ordered sequences / lists.
//! - [`state_store::StateStore`] – A composite synchronization engine that hosts multiple CRDTs.
//!
//! ## Logical clocks
//!
//! Physical wall-clock time (NTP) is unreliable in distributed systems. This library provides
//! two implementations for tracking causal ordering of events:
//!
//! - [`lamport_clock::LamportClock`] – A scalar Lamport clock. Sufficient for total-order
//!   disambiguation used by [`LWWRegister`] and [`StateStore`].
//! - [`vector_clock::VectorClock`] – A per-node vector clock that can detect **concurrent**
//!   events in addition to causal ordering.
//!
//! ## Quick example
//!
//! ```rust
//! use crdt_sync::lww_register::LWWRegister;
//! use crdt_sync::or_set::ORSet;
//! use crdt_sync::rga::RGA;
//!
//! // LWW-Register
//! let mut reg: LWWRegister<f64> = LWWRegister::new("node-1");
//! reg.set_and_apply(42.0, 1);
//! assert_eq!(reg.get(), Some(&42.0));
//!
//! // OR-Set
//! let mut set: ORSet<String> = ORSet::new("node-1");
//! let op = set.add("robot-A".to_string());
//! set.apply(op);
//! assert!(set.contains(&"robot-A".to_string()));
//!
//! // RGA
//! let mut rga: RGA<char> = RGA::new("node-1");
//! rga.insert(0, 'H');
//! rga.insert(1, 'i');
//! assert_eq!(rga.to_vec(), vec!['H', 'i']);
//! ```

pub mod lamport_clock;
pub mod lww_register;
pub mod or_set;
pub mod rga;
pub mod state_store;
pub mod vector_clock;

// Re-export the most commonly used types for ergonomic access.
pub use lamport_clock::LamportClock;
pub use lww_register::LWWRegister;
pub use or_set::ORSet;
pub use rga::RGA;
pub use state_store::StateStore;
pub use vector_clock::{VectorClock, VectorTimestamp};

