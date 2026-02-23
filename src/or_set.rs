//! # OR-Set (Observed-Remove Set)
//!
//! An operation-based CRDT for sets of unique items. Each element is tagged with
//! a unique token when added; removal only removes the specific tokens that were
//! *observed* at the time of the remove operation.  This means concurrent
//! add + remove always favours the add, and concurrent adds of the same value by
//! different nodes are both preserved.
//!
//! ## Guarantees
//! - Commutative: applying operations in any order yields the same set.
//! - Idempotent: applying the same operation twice has no extra effect.
//!
//! ## Example
//! ```rust
//! use crdt_sync::or_set::ORSet;
//!
//! let mut node_a: ORSet<&str> = ORSet::new("A");
//! let mut node_b: ORSet<&str> = ORSet::new("B");
//!
//! // A adds "robot"
//! let add_op = node_a.add("robot");
//! node_a.apply(add_op.clone());
//! node_b.apply(add_op);
//!
//! // B removes "robot" while A also adds it concurrently
//! let remove_op = node_b.remove(&"robot").unwrap();
//! let add_op2 = node_a.add("robot");
//!
//! node_a.apply(remove_op.clone());
//! node_b.apply(remove_op);
//! node_a.apply(add_op2.clone());
//! node_b.apply(add_op2);
//!
//! // "robot" is still present due to the concurrent add
//! assert!(node_a.contains(&"robot"));
//! assert!(node_b.contains(&"robot"));
//! ```

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

/// A unique token that identifies a specific *instance* of an element being added.
pub type Token = String;

/// Operations that can be applied to an [`ORSet`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ORSetOp<T> {
    /// Add `value` with the given unique token.
    Add { value: T, token: Token },
    /// Remove all listed tokens (which were observed at remove time).
    Remove { tokens: Vec<Token> },
}

/// Observed-Remove Set.
///
/// Internally maintains a map `value → set<token>`.  An element is considered
/// *present* if at least one of its tokens is still in the map.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ORSet<T>
where
    T: Eq + std::hash::Hash + Clone,
{
    node_id: String,
    /// Map from element value to the set of add-tokens currently active for it.
    entries: HashMap<T, HashSet<Token>>,
}

impl<T> ORSet<T>
where
    T: Eq + std::hash::Hash + Clone,
{
    /// Create a new empty OR-Set owned by `node_id`.
    pub fn new(node_id: impl Into<String>) -> Self {
        Self {
            node_id: node_id.into(),
            entries: HashMap::new(),
        }
    }

    /// Returns `true` if `value` is currently a member of the set.
    pub fn contains(&self, value: &T) -> bool {
        self.entries.get(value).map_or(false, |tokens| !tokens.is_empty())
    }

    /// Return an iterator over all currently present elements.
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.entries
            .iter()
            .filter(|(_, tokens)| !tokens.is_empty())
            .map(|(v, _)| v)
    }

    /// Return the number of distinct elements currently present.
    pub fn len(&self) -> usize {
        self.entries.values().filter(|tokens| !tokens.is_empty()).count()
    }

    /// Returns `true` if the set is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Generate an **add** operation for `value`.
    ///
    /// A globally unique token is created automatically.  The operation is not
    /// applied locally; call [`apply`](Self::apply) to do that.
    pub fn add(&self, value: T) -> ORSetOp<T> {
        let token = format!("{}-{}", self.node_id, Uuid::new_v4());
        ORSetOp::Add { value, token }
    }

    /// Generate a **remove** operation for `value`.
    ///
    /// Returns `None` if `value` is not currently in the set (there is nothing
    /// to observe-remove).  The set of tokens captured here represents the
    /// *observed* state at this point in time.
    pub fn remove(&self, value: &T) -> Option<ORSetOp<T>> {
        let tokens = self.entries.get(value)?;
        if tokens.is_empty() {
            return None;
        }
        Some(ORSetOp::Remove {
            tokens: tokens.iter().cloned().collect(),
        })
    }

    /// Apply an operation (local or remote) to this replica.
    pub fn apply(&mut self, op: ORSetOp<T>) {
        match op {
            ORSetOp::Add { value, token } => {
                self.entries.entry(value).or_default().insert(token);
            }
            ORSetOp::Remove { tokens } => {
                let token_set: HashSet<&Token> = tokens.iter().collect();
                for token_map in self.entries.values_mut() {
                    token_map.retain(|t| !token_set.contains(t));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_set() {
        let s: ORSet<i32> = ORSet::new("n1");
        assert!(s.is_empty());
        assert!(!s.contains(&1));
    }

    #[test]
    fn add_and_contains() {
        let mut s: ORSet<i32> = ORSet::new("n1");
        let op = s.add(42);
        s.apply(op);
        assert!(s.contains(&42));
        assert_eq!(s.len(), 1);
    }

    #[test]
    fn remove_element() {
        let mut s: ORSet<i32> = ORSet::new("n1");
        let add_op = s.add(42);
        s.apply(add_op);

        let rem_op = s.remove(&42).unwrap();
        s.apply(rem_op);

        assert!(!s.contains(&42));
        assert!(s.is_empty());
    }

    #[test]
    fn remove_nonexistent_returns_none() {
        let s: ORSet<i32> = ORSet::new("n1");
        assert!(s.remove(&99).is_none());
    }

    #[test]
    fn concurrent_add_beats_remove() {
        let mut node_a: ORSet<&str> = ORSet::new("A");
        let mut node_b: ORSet<&str> = ORSet::new("B");

        // Both nodes observe the first add
        let first_add = node_a.add("x");
        node_a.apply(first_add.clone());
        node_b.apply(first_add);

        // node_b removes "x" (observing the first token)
        let remove_op = node_b.remove(&"x").unwrap();

        // node_a concurrently adds "x" again (new token)
        let second_add = node_a.add("x");

        // apply operations in different orders
        node_a.apply(remove_op.clone());
        node_a.apply(second_add.clone());

        node_b.apply(second_add);
        node_b.apply(remove_op);

        // The concurrent add wins; "x" is still present
        assert!(node_a.contains(&"x"));
        assert!(node_b.contains(&"x"));
    }

    #[test]
    fn apply_is_idempotent() {
        let mut s: ORSet<i32> = ORSet::new("n1");
        let op = s.add(1);
        s.apply(op.clone());
        s.apply(op.clone());
        s.apply(op);
        assert_eq!(s.len(), 1);
    }

    #[test]
    fn multiple_elements() {
        let mut s: ORSet<i32> = ORSet::new("n1");
        for i in 0..5 {
            let op = s.add(i);
            s.apply(op);
        }
        assert_eq!(s.len(), 5);
        let rem = s.remove(&2).unwrap();
        s.apply(rem);
        assert_eq!(s.len(), 4);
        assert!(!s.contains(&2));
    }

    #[test]
    fn apply_is_commutative() {
        let mut s1: ORSet<i32> = ORSet::new("n1");
        let mut s2: ORSet<i32> = ORSet::new("n1");

        let op_a = s1.add(1);
        let op_b = s1.add(2);

        s1.apply(op_a.clone());
        s1.apply(op_b.clone());

        s2.apply(op_b);
        s2.apply(op_a);

        let mut v1: Vec<i32> = s1.iter().cloned().collect();
        let mut v2: Vec<i32> = s2.iter().cloned().collect();
        v1.sort();
        v2.sort();
        assert_eq!(v1, v2);
    }
}
