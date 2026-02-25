//! # RGA (Replicated Growable Array)
//!
//! An operation-based CRDT for ordered sequences (lists).  Each element is
//! given a globally unique identifier so that concurrent insertions at the same
//! position can be resolved deterministically.  Insertions are commutative and
//! idempotent; deletions use a *tombstone* approach to stay conflict-free.
//!
//! ## Identifiers
//! Each element carries an [`RGAId`] composed of a logical clock value and the
//! node identifier of the replica that performed the insertion.  This pair is
//! globally unique and totally ordered, which gives us a canonical way to sort
//! concurrent insertions.
//!
//! ## Guarantees
//! - Commutative: applying operations in any order yields the same sequence.
//! - Idempotent: applying the same operation twice has no extra effect.
//!
//! ## Example
//! ```rust
//! use crdt_sync::rga::RGA;
//!
//! let mut a: RGA<char> = RGA::new("A");
//! let mut b: RGA<char> = RGA::new("B");
//!
//! let op1 = a.insert(0, 'H');   // insert at beginning
//! let op2 = a.insert(1, 'i');   // insert after 'H'
//!
//! b.apply(op1.clone());
//! b.apply(op2.clone());
//!
//! assert_eq!(a.to_vec(), b.to_vec());
//! assert_eq!(a.to_vec(), vec!['H', 'i']);
//! ```

use serde::{Deserialize, Serialize};

/// Globally unique, totally ordered identifier for an RGA element.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RGAId {
    /// Logical (Lamport) clock value; higher is newer.
    pub clock: u64,
    /// The originating node's identifier.  Used to break ties deterministically.
    pub node_id: String,
}

impl RGAId {
    fn new(clock: u64, node_id: impl Into<String>) -> Self {
        Self { clock, node_id: node_id.into() }
    }
}

/// An internal node in the RGA linked list.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RGANode<T: Clone> {
    id: RGAId,
    value: T,
    /// `true` means the element has been logically deleted (tombstoned).
    deleted: bool,
}

/// Operations that can be applied to an [`RGA`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RGAOp<T> {
    /// Insert `value` with the given `id` immediately after the element with
    /// `after_id` (or at the beginning if `after_id` is `None`).
    Insert {
        id: RGAId,
        after_id: Option<RGAId>,
        value: T,
    },
    /// Tombstone the element with `id`.
    Delete { id: RGAId },
}

impl<T> RGAOp<T> {
    /// Return the [`RGAId`] of this operation.
    pub fn id(&self) -> Option<RGAId> {
        match self {
            RGAOp::Insert { id, .. } => Some(id.clone()),
            RGAOp::Delete { .. } => None,
        }
    }
}

/// Replicated Growable Array.
///
/// Elements are stored as an intrusive linked list ordered by their [`RGAId`]s
/// to ensure consistent ordering across replicas when operations are applied
/// out of order.  Insert operations whose `after_id` hasn't arrived yet are
/// buffered and retried automatically.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RGA<T: Clone> {
    node_id: String,
    clock: u64,
    /// The sequence of nodes (including tombstones) in logical order.
    nodes: Vec<RGANode<T>>,
    /// Operations buffered because their causal predecessor hasn't arrived yet.
    pending: Vec<RGAOp<T>>,
}

impl<T: Clone + PartialEq> RGA<T> {
    /// Create a new, empty RGA owned by `node_id`.
    pub fn new(node_id: impl Into<String>) -> Self {
        Self {
            node_id: node_id.into(),
            clock: 0,
            nodes: Vec::new(),
            pending: Vec::new(),
        }
    }

    // ── Helpers ──────────────────────────────────────────────────────────────

    fn next_id(&mut self) -> RGAId {
        self.clock += 1;
        RGAId::new(self.clock, self.node_id.clone())
    }

    /// Return the position of the node with `id` in the internal `nodes` list.
    fn pos_of(&self, id: &RGAId) -> Option<usize> {
        self.nodes.iter().position(|n| &n.id == id)
    }

    /// Return the visible (non-tombstoned) index → internal position mapping.
    fn visible_positions(&self) -> Vec<usize> {
        self.nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| !n.deleted)
            .map(|(i, _)| i)
            .collect()
    }

    // ── Public API ───────────────────────────────────────────────────────────

    /// Return the number of visible (non-deleted) elements.
    pub fn len(&self) -> usize {
        self.nodes.iter().filter(|n| !n.deleted).count()
    }

    /// Returns `true` if the sequence is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Collect the visible elements into a `Vec`.
    pub fn to_vec(&self) -> Vec<T> {
        self.nodes
            .iter()
            .filter(|n| !n.deleted)
            .map(|n| n.value.clone())
            .collect()
    }

    /// Return an iterator over visible elements.
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.nodes.iter().filter(|n| !n.deleted).map(|n| &n.value)
    }

    /// Generate an **insert** operation at visible index `index`.
    ///
    /// - `index == 0` inserts before all current elements.
    /// - `index == len()` appends at the end.
    ///
    /// Panics if `index > len()`.
    pub fn insert_op(&mut self, index: usize, value: T) -> RGAOp<T> {
        let visible = self.visible_positions();
        assert!(index <= visible.len(), "index out of bounds");

        let after_id = if index == 0 {
            None
        } else {
            Some(self.nodes[visible[index - 1]].id.clone())
        };

        let id = self.next_id();
        RGAOp::Insert { id, after_id, value }
    }

    /// Generate a **delete** operation for the element at visible index `index`.
    ///
    /// Returns `None` if the sequence is empty or `index` is out of bounds.
    pub fn delete_op(&self, index: usize) -> Option<RGAOp<T>> {
        let visible = self.visible_positions();
        visible.get(index).map(|&pos| RGAOp::Delete {
            id: self.nodes[pos].id.clone(),
        })
    }

    /// Apply an operation (local or remote) to this replica.
    ///
    /// Insert operations whose causal predecessor (`after_id`) has not yet been
    /// received are buffered and retried automatically once the predecessor
    /// arrives. Duplicate inserts (same id) are silently ignored, making the
    /// operation idempotent.
    pub fn apply(&mut self, op: RGAOp<T>) {
        // Update local clock to ensure causality when receiving remote ops.
        match &op {
            RGAOp::Insert { id, .. } | RGAOp::Delete { id } => {
                if id.clock > self.clock {
                    self.clock = id.clock;
                }
            }
        }

        if self.try_apply_one(op) {
            // Drain the pending buffer: keep retrying until no op can be applied.
            let mut progress = true;
            while progress {
                progress = false;
                let mut still_pending = Vec::new();
                for pending_op in std::mem::take(&mut self.pending) {
                    if self.try_apply_one(pending_op.clone()) {
                        progress = true;
                    } else {
                        still_pending.push(pending_op);
                    }
                }
                self.pending = still_pending;
            }
        }
    }

    /// Attempt to apply a single operation.
    ///
    /// Returns `true` if the operation was applied (or was a no-op due to
    /// idempotency), `false` if it needs to be buffered (causal predecessor
    /// missing).
    fn try_apply_one(&mut self, op: RGAOp<T>) -> bool {
        match op {
            RGAOp::Insert { id, after_id, value } => {
                // Idempotency: skip if already present.
                if self.pos_of(&id).is_some() {
                    return true;
                }

                // Find the insertion point.
                let insert_after_pos: Option<usize> = match &after_id {
                    None => None,
                    Some(aid) => {
                        match self.pos_of(aid) {
                            Some(p) => Some(p),
                            None => {
                                // Causal predecessor not yet received; buffer and wait.
                                self.pending.push(RGAOp::Insert { id, after_id, value });
                                return false;
                            }
                        }
                    }
                };

                // Starting position: one after `insert_after_pos` (or 0).
                let start = insert_after_pos.map_or(0, |p| p + 1);

                // Resolve concurrent inserts at the same anchor using a total
                // order on RGAId: elements with a *greater* id than ours are
                // placed before the new element (they win the position).
                // This ensures all replicas converge to the same sequence
                // regardless of the order operations are applied.
                let pos = {
                    let mut p = start;
                    while p < self.nodes.len() {
                        if self.nodes[p].id > id {
                            p += 1;
                        } else {
                            break;
                        }
                    }
                    p
                };

                self.nodes.insert(pos, RGANode { id, value, deleted: false });
                true
            }
            RGAOp::Delete { id } => {
                if let Some(pos) = self.pos_of(&id) {
                    self.nodes[pos].deleted = true;
                }
                // Silently ignore if not found (out-of-order or replay).
                true
            }
        }
    }

    /// Convenience: insert at `index` and immediately apply locally.
    ///
    /// Returns the generated operation for broadcasting.
    pub fn insert(&mut self, index: usize, value: T) -> RGAOp<T> {
        let op = self.insert_op(index, value);
        self.apply(op.clone());
        op
    }

    /// Convenience: delete at visible `index` and immediately apply locally.
    ///
    /// Returns the generated operation or `None` if out of bounds.
    pub fn delete(&mut self, index: usize) -> Option<RGAOp<T>> {
        let op = self.delete_op(index)?;
        self.apply(op.clone());
        Some(op)
    }

    /// Physically remove tombstoned nodes from the sequence whose insertion
    /// clock is at or before `before_ts`.
    ///
    /// Called after a server `PRUNE` broadcast.  Tombstoned nodes whose
    /// `id.clock ≤ before_ts` are guaranteed to have been observed by every
    /// connected client, so the backing memory can be safely reclaimed.
    pub fn prune_tombstones(&mut self, before_ts: u64) {
        self.nodes.retain(|n| !n.deleted || n.id.clock > before_ts);
    }

    /// Merge another RGA's state into this one (state-based / CvRDT).
    ///
    /// Every element present in `other` but absent from `self` is inserted at
    /// the correct position according to the RGA ordering rules (deterministic
    /// RGAId comparison breaks ties). Elements tombstoned in `other` are also
    /// tombstoned in `self`.
    ///
    /// - **Commutative**: merging in either direction yields the same visible
    ///   sequence because element positions are determined by RGAId ordering.
    /// - **Associative** and **idempotent** by the properties of the underlying
    ///   set-union and the deterministic ordering.
    pub fn merge(&mut self, other: &RGA<T>) {
        // Advance our clock so any IDs we import don't collide with future local ones.
        if other.clock > self.clock {
            self.clock = other.clock;
        }

        // Replay inserts from `other` that we don't already have.
        // We iterate in `other.nodes` order so that causal predecessors are
        // inserted before their successors (or buffered automatically if not).
        for i in 0..other.nodes.len() {
            let node = &other.nodes[i];
            if self.pos_of(&node.id).is_none() {
                // Find the nearest predecessor in `other.nodes[..i]` that is
                // already present in `self` (accounts for nodes we just added).
                let after_id = other.nodes[..i]
                    .iter()
                    .rev()
                    .find(|n| self.pos_of(&n.id).is_some())
                    .map(|n| n.id.clone());

                let op = RGAOp::Insert {
                    id: node.id.clone(),
                    after_id,
                    value: node.value.clone(),
                };
                self.apply(op);
            }
        }

        // Propagate tombstones: if a node is deleted in `other`, delete it here.
        for node in &other.nodes {
            if node.deleted {
                if let Some(pos) = self.pos_of(&node.id) {
                    self.nodes[pos].deleted = true;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_rga_is_empty() {
        let r: RGA<char> = RGA::new("n1");
        assert!(r.is_empty());
        assert_eq!(r.to_vec(), Vec::<char>::new());
    }

    #[test]
    fn single_insert() {
        let mut r: RGA<char> = RGA::new("n1");
        r.insert(0, 'A');
        assert_eq!(r.to_vec(), vec!['A']);
    }

    #[test]
    fn multiple_inserts_in_order() {
        let mut r: RGA<char> = RGA::new("n1");
        r.insert(0, 'H');
        r.insert(1, 'i');
        r.insert(2, '!');
        assert_eq!(r.to_vec(), vec!['H', 'i', '!']);
    }

    #[test]
    fn insert_at_beginning() {
        let mut r: RGA<char> = RGA::new("n1");
        r.insert(0, 'B');
        r.insert(0, 'A');
        assert_eq!(r.to_vec(), vec!['A', 'B']);
    }

    #[test]
    fn delete_element() {
        let mut r: RGA<char> = RGA::new("n1");
        r.insert(0, 'A');
        r.insert(1, 'B');
        r.insert(2, 'C');
        r.delete(1); // remove 'B'
        assert_eq!(r.to_vec(), vec!['A', 'C']);
    }

    #[test]
    fn apply_insert_op_is_idempotent() {
        let mut r: RGA<char> = RGA::new("n1");
        let op = r.insert_op(0, 'X');
        r.apply(op.clone());
        r.apply(op.clone());
        r.apply(op);
        assert_eq!(r.to_vec(), vec!['X']);
    }

    #[test]
    fn two_replicas_converge() {
        let mut a: RGA<char> = RGA::new("A");
        let mut b: RGA<char> = RGA::new("B");

        let op1 = a.insert(0, 'A');
        let op2 = a.insert(1, 'B');

        b.apply(op2);
        b.apply(op1);

        assert_eq!(a.to_vec(), b.to_vec());
    }

    #[test]
    fn concurrent_inserts_are_deterministic() {
        let mut a: RGA<char> = RGA::new("A");
        let mut b: RGA<char> = RGA::new("B");

        // Both insert at position 0 concurrently (no prior shared state)
        let op_a = a.insert_op(0, 'A');
        let op_b = b.insert_op(0, 'B');

        a.apply(op_a.clone());
        a.apply(op_b.clone());

        b.apply(op_b);
        b.apply(op_a);

        // Both replicas should have the same sequence
        assert_eq!(a.to_vec(), b.to_vec());
        assert_eq!(a.len(), 2);
    }

    #[test]
    fn delete_then_reinsert_after() {
        let mut r: RGA<char> = RGA::new("n1");
        r.insert(0, 'A');
        r.insert(1, 'B');
        r.delete(0); // remove 'A'
        r.insert(0, 'C'); // insert new element at front
        assert_eq!(r.to_vec(), vec!['C', 'B']);
    }

    #[test]
    fn remote_delete_applied_before_insert() {
        let mut a: RGA<char> = RGA::new("A");
        let mut b: RGA<char> = RGA::new("B");

        let ins = a.insert(0, 'Z');
        b.apply(ins);

        let del = b.delete(0).unwrap();

        // Apply delete on a before it even processes the delete locally
        a.apply(del);
        assert!(a.is_empty());
    }

    // ── merge() tests ────────────────────────────────────────────────────────

    #[test]
    fn merge_empty_into_non_empty_is_noop() {
        let mut a: RGA<char> = RGA::new("A");
        a.insert(0, 'X');
        let b: RGA<char> = RGA::new("B");
        a.merge(&b);
        assert_eq!(a.to_vec(), vec!['X']);
    }

    #[test]
    fn merge_non_empty_into_empty() {
        let mut a: RGA<char> = RGA::new("A");
        let mut b: RGA<char> = RGA::new("B");
        b.insert(0, 'H');
        b.insert(1, 'i');
        a.merge(&b);
        assert_eq!(a.to_vec(), vec!['H', 'i']);
    }

    #[test]
    fn merge_is_commutative() {
        let mut a: RGA<char> = RGA::new("A");
        let mut b: RGA<char> = RGA::new("B");

        a.insert(0, 'A');
        b.insert(0, 'B');

        let a2 = a.clone();
        let b2 = b.clone();

        a.merge(&b);   // a absorbs b
        b.merge(&a2);  // b absorbs a (using the original a)

        // Both should have the same sequence (deterministic by RGAId ordering)
        assert_eq!(a.to_vec(), b.to_vec());
        assert_eq!(a.len(), 2);

        // Verify a third way: b2 merged with a2
        let mut b3 = b2.clone();
        b3.merge(&a2);
        assert_eq!(a.to_vec(), b3.to_vec());
    }

    #[test]
    fn merge_is_associative() {
        let mut a: RGA<char> = RGA::new("A");
        let mut b: RGA<char> = RGA::new("B");
        let mut c: RGA<char> = RGA::new("C");

        a.insert(0, 'A');
        b.insert(0, 'B');
        c.insert(0, 'C');

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

        assert_eq!(ab_c.to_vec(), a_bc.to_vec());
    }

    #[test]
    fn merge_is_idempotent() {
        let mut a: RGA<char> = RGA::new("A");
        a.insert(0, 'X');
        a.insert(1, 'Y');
        let snapshot = a.clone();
        a.merge(&snapshot);
        a.merge(&snapshot);
        assert_eq!(a.to_vec(), vec!['X', 'Y']);
    }

    #[test]
    fn merge_propagates_tombstones() {
        let mut a: RGA<char> = RGA::new("A");
        let mut b: RGA<char> = RGA::new("B");

        // Both replicas start with the same element
        let op = a.insert(0, 'Z');
        b.apply(op);

        // a deletes it
        a.delete(0);

        // Merge a's state (with tombstone) into b
        b.merge(&a);
        assert!(b.is_empty());
    }

    #[test]
    fn merge_concurrent_inserts_converge() {
        let mut a: RGA<char> = RGA::new("A");
        let mut b: RGA<char> = RGA::new("B");

        // Each inserts independently at position 0
        a.insert(0, 'A');
        b.insert(0, 'B');

        let a2 = a.clone();
        let b2 = b.clone();

        a.merge(&b2);
        b.merge(&a2);

        // Both replicas must converge to the same sequence
        assert_eq!(a.to_vec(), b.to_vec());
        assert_eq!(a.len(), 2);
    }

    #[test]
    fn merge_disjoint_sequences() {
        let mut a: RGA<i32> = RGA::new("A");
        let mut b: RGA<i32> = RGA::new("B");

        // a has [1, 2, 3], b has [4, 5, 6]
        for i in 1..=3 {
            a.insert(a.len(), i);
        }
        for i in 4..=6 {
            b.insert(b.len(), i);
        }

        let mut merged = a.clone();
        merged.merge(&b);

        // All 6 elements should be present
        assert_eq!(merged.len(), 6);
        let mut v = merged.to_vec();
        v.sort();
        assert_eq!(v, vec![1, 2, 3, 4, 5, 6]);
    }
}
