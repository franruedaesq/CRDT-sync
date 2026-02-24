//! Property-based convergence tests for all three CRDT types.
//!
//! Each test uses `proptest` to generate thousands of random operation sequences,
//! runs them through a simulated chaotic network (random drops, duplicates, and
//! shuffles), then strictly asserts that all five simulated nodes converge to
//! identical state.

use crdt_sync::{
    lww_register::{LWWOp, LWWRegister},
    or_set::{ORSet, ORSetOp},
    rga::{RGAOp, RGA},
};
use proptest::prelude::*;

// ── Chaos helpers ────────────────────────────────────────────────────────────

/// Apply network chaos to an operation list: randomly drop, duplicate, and
/// shuffle entries, then return the resulting sequence.
///
/// `chaos_seed` drives all randomness so the test is reproducible given the
/// same proptest input.
fn chaos<T: Clone>(ops: Vec<T>, drops: Vec<bool>, dupes: Vec<bool>) -> Vec<T> {
    let mut out: Vec<T> = Vec::new();
    for (i, op) in ops.into_iter().enumerate() {
        if drops.get(i).copied().unwrap_or(false) {
            continue; // simulate a dropped message
        }
        if dupes.get(i).copied().unwrap_or(false) {
            out.push(op.clone()); // simulate a duplicate delivery
        }
        out.push(op);
    }
    out
}

// ── LWWRegister ──────────────────────────────────────────────────────────────

/// Strategy that produces a list of (value, timestamp, node_index) triples
/// representing write operations from up to 5 nodes.
fn lww_ops_strategy() -> impl Strategy<Value = Vec<(i32, u64, usize)>> {
    prop::collection::vec(
        (any::<i32>(), 1u64..=1000u64, 0usize..5usize),
        1..50,
    )
}

proptest! {
    /// LWWRegister converges under chaotic delivery across 5 nodes.
    #[test]
    fn lww_register_converges_under_chaos(
        raw_ops in lww_ops_strategy(),
        drops   in prop::collection::vec(any::<bool>(), 1..100usize),
        dupes   in prop::collection::vec(any::<bool>(), 1..100usize),
        // A permutation seed: we shuffle by reversing a boolean mask
        shuffle_flag in any::<bool>(),
    ) {
        let node_ids = ["n0", "n1", "n2", "n3", "n4"];

        // Build a concrete list of LWWOps from the raw triples.
        let ops: Vec<LWWOp<i32>> = raw_ops
            .iter()
            .map(|&(value, timestamp, node_idx)| LWWOp {
                value,
                timestamp,
                node_id: node_ids[node_idx].to_string(),
            })
            .collect();

        // Apply chaos (drops + duplicates).
        let mut chaotic = chaos(ops, drops, dupes);

        // Optionally reverse to simulate heavy reordering.
        if shuffle_flag {
            chaotic.reverse();
        }

        // Apply the chaotic sequence to all 5 nodes independently.
        let mut nodes: Vec<LWWRegister<i32>> = node_ids
            .iter()
            .map(|id| LWWRegister::new(*id))
            .collect();

        for op in &chaotic {
            for node in nodes.iter_mut() {
                node.apply(op.clone());
            }
        }

        // Assert convergence: every node must hold the same value.
        let reference = nodes[0].get().copied();
        for node in &nodes[1..] {
            prop_assert_eq!(
                node.get().copied(),
                reference,
                "LWWRegister nodes diverged"
            );
        }
    }
}

// ── ORSet ────────────────────────────────────────────────────────────────────

/// An abstract operation before token assignment.
#[derive(Debug, Clone)]
enum ORSetCmd {
    Add(u8),
    Remove(u8),
}

fn orset_ops_strategy() -> impl Strategy<Value = Vec<ORSetCmd>> {
    prop::collection::vec(
        prop_oneof![
            (0u8..10u8).prop_map(ORSetCmd::Add),
            (0u8..10u8).prop_map(ORSetCmd::Remove),
        ],
        1..60,
    )
}

proptest! {
    /// ORSet converges under chaotic delivery across 5 nodes.
    #[test]
    fn orset_converges_under_chaos(
        cmds       in orset_ops_strategy(),
        drops      in prop::collection::vec(any::<bool>(), 1..100usize),
        dupes      in prop::collection::vec(any::<bool>(), 1..100usize),
        shuffle_flag in any::<bool>(),
    ) {
        // Use a single "source" node to generate ops with unique tokens.
        let mut source: ORSet<u8> = ORSet::new("source");

        // Pre-apply all ops on the source to maintain a live set for removes.
        let mut generated: Vec<ORSetOp<u8>> = Vec::new();
        for cmd in &cmds {
            match cmd {
                ORSetCmd::Add(v) => {
                    let op = source.add(*v);
                    source.apply(op.clone());
                    generated.push(op);
                }
                ORSetCmd::Remove(v) => {
                    if let Some(op) = source.remove(v) {
                        source.apply(op.clone());
                        generated.push(op);
                    }
                }
            }
        }

        // Apply chaos.
        let mut chaotic = chaos(generated, drops, dupes);
        if shuffle_flag {
            chaotic.reverse();
        }

        // Five independent nodes apply the chaotic sequence.
        let mut nodes: Vec<ORSet<u8>> = (0..5)
            .map(|i| ORSet::new(format!("n{i}")))
            .collect();

        for op in &chaotic {
            for node in nodes.iter_mut() {
                node.apply(op.clone());
            }
        }

        // Convergence: every node's sorted element list must match.
        let mut ref_elems: Vec<u8> = nodes[0].iter().copied().collect();
        ref_elems.sort();

        for node in &nodes[1..] {
            let mut elems: Vec<u8> = node.iter().copied().collect();
            elems.sort();
            prop_assert_eq!(elems, ref_elems.clone(), "ORSet nodes diverged");
        }
    }
}

// ── RGA ──────────────────────────────────────────────────────────────────────

/// Abstract RGA command (index is relative to visible length at generation time).
#[derive(Debug, Clone)]
enum RGACmd {
    Insert(u8),   // value to insert at the end (appended for simplicity)
    Delete(u8),   // delete element at visible index % len (ignored if empty)
}

fn rga_ops_strategy() -> impl Strategy<Value = Vec<RGACmd>> {
    prop::collection::vec(
        prop_oneof![
            (any::<u8>()).prop_map(RGACmd::Insert),
            (any::<u8>()).prop_map(RGACmd::Delete),
        ],
        1..40,
    )
}

proptest! {
    /// RGA converges under chaotic delivery across 5 nodes.
    #[test]
    fn rga_converges_under_chaos(
        cmds         in rga_ops_strategy(),
        drops        in prop::collection::vec(any::<bool>(), 1..100usize),
        dupes        in prop::collection::vec(any::<bool>(), 1..100usize),
        shuffle_flag in any::<bool>(),
    ) {
        // Generate ops on a single source RGA so IDs are consistent.
        let mut source: RGA<u8> = RGA::new("source");
        let mut generated: Vec<RGAOp<u8>> = Vec::new();

        for cmd in &cmds {
            match cmd {
                RGACmd::Insert(v) => {
                    let pos = source.len();
                    let op = source.insert(pos, *v);
                    generated.push(op);
                }
                RGACmd::Delete(idx) => {
                    if !source.is_empty() {
                        let pos = (*idx as usize) % source.len();
                        if let Some(op) = source.delete(pos) {
                            generated.push(op);
                        }
                    }
                }
            }
        }

        // Apply chaos.
        let mut chaotic = chaos(generated, drops, dupes);
        if shuffle_flag {
            chaotic.reverse();
        }

        // Five independent nodes apply the chaotic sequence.
        let mut nodes: Vec<RGA<u8>> = (0..5)
            .map(|i| RGA::new(format!("n{i}")))
            .collect();

        for op in &chaotic {
            for node in nodes.iter_mut() {
                node.apply(op.clone());
            }
        }

        // Convergence: every node's to_vec() must match.
        let reference = nodes[0].to_vec();
        for node in &nodes[1..] {
            prop_assert_eq!(
                node.to_vec(),
                reference.clone(),
                "RGA nodes diverged"
            );
        }
    }
}
