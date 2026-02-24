//! # WebAssembly bindings for `StateStore`
//!
//! This module provides a [`WasmStateStore`] wrapper around [`StateStore`]
//! annotated with `#[wasm_bindgen]` so it can be called from JavaScript or
//! TypeScript running in a browser or Node.js environment.
//!
//! All [`Envelope`] payloads cross the Wasm boundary as **MessagePack bytes**
//! (`Uint8Array` on the JavaScript side), using `rmp-serde` for serialisation.
//! CRDT values written from JavaScript are still supplied as JSON strings for
//! ergonomic compatibility with the JavaScript ecosystem; only the network
//! envelope format is binary.
//!
//! ## JavaScript / TypeScript usage
//!
//! ```js
//! import init, { WasmStateStore } from './crdt_sync.js';
//! await init();
//!
//! const store = new WasmStateStore("node-1");
//!
//! // Write a value — returns a Uint8Array (MessagePack-encoded Envelope).
//! const envelopeBytes = store.set_register("robot.x", JSON.stringify(42.0));
//!
//! // On a peer node, apply the received bytes.
//! const peer = new WasmStateStore("node-2");
//! peer.apply_envelope(envelopeBytes);
//!
//! // Read back the value.
//! const value = JSON.parse(peer.get_register("robot.x")); // 42
//! ```

use wasm_bindgen::prelude::*;

use crate::state_store::{Envelope, StateStore};

/// A WebAssembly-compatible wrapper around [`StateStore`].
///
/// Envelope methods accept and return **MessagePack bytes** (`Uint8Array` in
/// JavaScript) so that binary frames can be sent over WebSocket without the
/// overhead of JSON serialisation/deserialisation.  CRDT values (register
/// contents, set elements, sequence elements) are still supplied as JSON
/// strings for ergonomic interoperability with the JavaScript ecosystem.
#[wasm_bindgen]
pub struct WasmStateStore {
    inner: StateStore,
}

#[wasm_bindgen]
impl WasmStateStore {
    /// Create a new `WasmStateStore` for the given node identifier.
    #[wasm_bindgen(constructor)]
    pub fn new(node_id: &str) -> WasmStateStore {
        WasmStateStore {
            inner: StateStore::new(node_id),
        }
    }

    /// Apply a remote [`Envelope`] (serialised as MessagePack bytes) to this store.
    ///
    /// Throws a JavaScript error if the bytes cannot be deserialised.
    pub fn apply_envelope(&mut self, envelope_bytes: &[u8]) -> Result<(), JsValue> {
        let env: Envelope = rmp_serde::from_slice(envelope_bytes)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        self.inner.apply_envelope(env);
        Ok(())
    }

    // ── Register (LWW) ────────────────────────────────────────────────────

    /// Write a JSON-encoded value to the named LWW register.
    ///
    /// Returns the resulting [`Envelope`] serialised as **MessagePack bytes**
    /// (`Uint8Array`), ready to broadcast to peer nodes.
    pub fn set_register(&mut self, key: &str, value_json: &str) -> Result<Vec<u8>, JsValue> {
        let value: serde_json::Value = serde_json::from_str(value_json)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        let env = self.inner.set_register(key, value);
        rmp_serde::to_vec_named(&env).map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Read the current value of a named LWW register as a JSON string.
    ///
    /// Returns `undefined` in JavaScript if the key has never been written.
    pub fn get_register(&self, key: &str) -> Option<String> {
        let value: Option<serde_json::Value> = self.inner.get_register(key);
        value.and_then(|v| serde_json::to_string(&v).ok())
    }

    // ── OR-Set ────────────────────────────────────────────────────────────

    /// Add a JSON-encoded element to the named OR-Set.
    ///
    /// Returns the resulting [`Envelope`] as **MessagePack bytes**.
    pub fn set_add(&mut self, key: &str, value_json: &str) -> Result<Vec<u8>, JsValue> {
        let value: serde_json::Value = serde_json::from_str(value_json)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        let env = self.inner.set_add(key, value);
        rmp_serde::to_vec_named(&env).map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Remove a JSON-encoded element from the named OR-Set.
    ///
    /// Returns the resulting [`Envelope`] as **MessagePack bytes**, or
    /// `undefined` if the element was not present in the set.
    ///
    /// Throws a JavaScript error if `value_json` is not valid JSON.
    pub fn set_remove(&mut self, key: &str, value_json: &str) -> Result<Option<Vec<u8>>, JsValue> {
        let value: serde_json::Value = serde_json::from_str(value_json)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        match self.inner.set_remove(key, value) {
            Some(env) => {
                let bytes = rmp_serde::to_vec_named(&env)
                    .map_err(|e| JsValue::from_str(&e.to_string()))?;
                Ok(Some(bytes))
            }
            None => Ok(None),
        }
    }

    /// Returns `true` if the named OR-Set contains the JSON-encoded `value`.
    pub fn set_contains(&self, key: &str, value_json: &str) -> bool {
        match serde_json::from_str::<serde_json::Value>(value_json) {
            Ok(value) => self.inner.set_contains(key, &value),
            Err(_) => false,
        }
    }

    /// Return all elements of the named OR-Set as a JSON array string.
    ///
    /// Throws a JavaScript error if serialisation fails.
    pub fn set_items(&self, key: &str) -> Result<String, JsValue> {
        let items: Vec<serde_json::Value> = self.inner.set_items(key);
        serde_json::to_string(&items).map_err(|e| JsValue::from_str(&e.to_string()))
    }

    // ── RGA (Sequence) ────────────────────────────────────────────────────

    /// Insert a JSON-encoded element at `index` in the named RGA sequence.
    ///
    /// Returns the resulting [`Envelope`] as **MessagePack bytes**.
    pub fn seq_insert(
        &mut self,
        key: &str,
        index: usize,
        value_json: &str,
    ) -> Result<Vec<u8>, JsValue> {
        let value: serde_json::Value = serde_json::from_str(value_json)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        let env = self.inner.seq_insert(key, index, value);
        rmp_serde::to_vec_named(&env).map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Delete the element at visible `index` in the named RGA sequence.
    ///
    /// Returns the resulting [`Envelope`] as **MessagePack bytes**, or
    /// `undefined` if `index` is out of bounds.
    pub fn seq_delete(&mut self, key: &str, index: usize) -> Option<Vec<u8>> {
        self.inner
            .seq_delete(key, index)
            .and_then(|env| rmp_serde::to_vec_named(&env).ok())
    }

    /// Return all visible elements of the named sequence as a JSON array string.
    ///
    /// Throws a JavaScript error if serialisation fails.
    pub fn seq_items(&self, key: &str) -> Result<String, JsValue> {
        let items: Vec<serde_json::Value> = self.inner.seq_items(key);
        serde_json::to_string(&items).map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Return the number of visible elements in the named sequence.
    pub fn seq_len(&self, key: &str) -> usize {
        self.inner.seq_len(key)
    }

    // ── Clock ─────────────────────────────────────────────────────────────

    /// Merge a full state snapshot (serialised `StateStore` as MessagePack bytes)
    /// into this store.
    ///
    /// The Rust relay server serialises the entire `StateStore` as MessagePack
    /// and sends it in the initial `SNAPSHOT` message.  Call this method to
    /// hydrate the local store with the server's consolidated state before
    /// processing any new deltas.
    ///
    /// Throws a JavaScript error if the bytes cannot be deserialised.
    pub fn merge_snapshot(&mut self, state_bytes: &[u8]) -> Result<(), JsValue> {
        let other: StateStore = rmp_serde::from_slice(state_bytes)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        self.inner.merge(&other);
        Ok(())
    }

    /// Physically remove tombstoned entries from all CRDTs in this store that
    /// were deleted at or before `before_ts`.
    ///
    /// Called in response to a server `PRUNE` broadcast after the server has
    /// determined that all connected clients have advanced their clocks past
    /// `before_ts`, meaning no client can still be "in the middle of" an
    /// operation that references those deleted entries.
    ///
    /// `before_ts` is passed as `f64` because JavaScript's `Number` type
    /// cannot safely represent all `u64` values.
    pub fn prune_tombstones(&mut self, before_ts: f64) {
        self.inner.prune_tombstones(before_ts as u64);
    }

    /// Return the current Lamport clock value.
    ///
    /// Returned as `f64` because JavaScript's `Number` type cannot safely
    /// represent all `u64` values.  Values up to `2^53 − 1`
    /// (`Number.MAX_SAFE_INTEGER`) are represented exactly.  For distributed
    /// systems that could conceivably tick the clock beyond that threshold,
    /// treat the returned value as approximate or use `BigInt` on the JS side.
    pub fn clock(&self) -> f64 {
        self.inner.clock() as f64
    }
}
