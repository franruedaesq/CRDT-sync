//! # WebAssembly bindings for `StateStore`
//!
//! This module provides a [`WasmStateStore`] wrapper around [`StateStore`]
//! annotated with `#[wasm_bindgen]` so it can be called from JavaScript or
//! TypeScript running in a browser or Node.js environment.
//!
//! All values and [`Envelope`] payloads cross the Wasm boundary as JSON
//! strings, leveraging the `serde_json` serialisation already used throughout
//! this crate.  This keeps the API dependency-light while remaining fully
//! type-safe on the Rust side.
//!
//! ## JavaScript / TypeScript usage
//!
//! ```js
//! import init, { WasmStateStore } from './crdt_sync.js';
//! await init();
//!
//! const store = new WasmStateStore("node-1");
//!
//! // Write a value and obtain the envelope to broadcast.
//! const envelope = store.set_register("robot.x", JSON.stringify(42.0));
//!
//! // On a peer node, apply the received envelope.
//! const peer = new WasmStateStore("node-2");
//! peer.apply_envelope(envelope);
//!
//! // Read back the value.
//! const value = JSON.parse(peer.get_register("robot.x")); // 42
//! ```

use wasm_bindgen::prelude::*;

use crate::state_store::{Envelope, StateStore};

/// A WebAssembly-compatible wrapper around [`StateStore`].
///
/// Methods accept and return JSON-encoded strings at the Wasm boundary so that
/// [`Envelope`] payloads and CRDT values can be exchanged between Rust and
/// JavaScript without sharing memory structures directly.
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

    /// Apply a remote [`Envelope`] (serialised as a JSON string) to this store.
    ///
    /// Throws a JavaScript error if the JSON cannot be deserialised.
    pub fn apply_envelope(&mut self, envelope_json: &str) -> Result<(), JsValue> {
        let env: Envelope = serde_json::from_str(envelope_json)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        self.inner.apply_envelope(env);
        Ok(())
    }

    // ── Register (LWW) ────────────────────────────────────────────────────

    /// Write a JSON-encoded value to the named LWW register.
    ///
    /// Returns the resulting [`Envelope`] serialised as a JSON string, ready
    /// to broadcast to peer nodes.
    pub fn set_register(&mut self, key: &str, value_json: &str) -> Result<String, JsValue> {
        let value: serde_json::Value = serde_json::from_str(value_json)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        let env = self.inner.set_register(key, value);
        serde_json::to_string(&env).map_err(|e| JsValue::from_str(&e.to_string()))
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
    /// Returns the resulting [`Envelope`] as a JSON string.
    pub fn set_add(&mut self, key: &str, value_json: &str) -> Result<String, JsValue> {
        let value: serde_json::Value = serde_json::from_str(value_json)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        let env = self.inner.set_add(key, value);
        serde_json::to_string(&env).map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Remove a JSON-encoded element from the named OR-Set.
    ///
    /// Returns the resulting [`Envelope`] as a JSON string, or `undefined` if
    /// the element was not present in the set.
    ///
    /// Throws a JavaScript error if `value_json` is not valid JSON.
    pub fn set_remove(&mut self, key: &str, value_json: &str) -> Result<Option<String>, JsValue> {
        let value: serde_json::Value = serde_json::from_str(value_json)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        match self.inner.set_remove(key, value) {
            Some(env) => {
                let s = serde_json::to_string(&env)
                    .map_err(|e| JsValue::from_str(&e.to_string()))?;
                Ok(Some(s))
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
    /// Returns the resulting [`Envelope`] as a JSON string.
    pub fn seq_insert(
        &mut self,
        key: &str,
        index: usize,
        value_json: &str,
    ) -> Result<String, JsValue> {
        let value: serde_json::Value = serde_json::from_str(value_json)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        let env = self.inner.seq_insert(key, index, value);
        serde_json::to_string(&env).map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Delete the element at visible `index` in the named RGA sequence.
    ///
    /// Returns the resulting [`Envelope`] as a JSON string, or `undefined` if
    /// `index` is out of bounds.
    pub fn seq_delete(&mut self, key: &str, index: usize) -> Option<String> {
        self.inner
            .seq_delete(key, index)
            .and_then(|env| serde_json::to_string(&env).ok())
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
