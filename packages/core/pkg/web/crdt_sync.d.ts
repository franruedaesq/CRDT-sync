/* tslint:disable */
/* eslint-disable */

/**
 * A WebAssembly-compatible wrapper around [`StateStore`].
 *
 * Envelope methods accept and return **MessagePack bytes** (`Uint8Array` in
 * JavaScript) so that binary frames can be sent over WebSocket without the
 * overhead of JSON serialisation/deserialisation.  CRDT values (register
 * contents, set elements, sequence elements) are still supplied as JSON
 * strings for ergonomic interoperability with the JavaScript ecosystem.
 */
export class WasmStateStore {
    free(): void;
    [Symbol.dispose](): void;
    /**
     * Apply a remote [`Envelope`] (serialised as MessagePack bytes) to this store.
     *
     * Throws a JavaScript error if the bytes cannot be deserialised.
     */
    apply_envelope(envelope_bytes: Uint8Array): void;
    /**
     * Return the current Lamport clock value.
     *
     * Returned as `f64` because JavaScript's `Number` type cannot safely
     * represent all `u64` values.  Values up to `2^53 − 1`
     * (`Number.MAX_SAFE_INTEGER`) are represented exactly.  For distributed
     * systems that could conceivably tick the clock beyond that threshold,
     * treat the returned value as approximate or use `BigInt` on the JS side.
     */
    clock(): number;
    /**
     * Read the current value of a named LWW register as a JSON string.
     *
     * Returns `undefined` in JavaScript if the key has never been written.
     */
    get_register(key: string): string | undefined;
    /**
     * Merge a full state snapshot (serialised `StateStore` as MessagePack bytes)
     * into this store.
     *
     * The Rust relay server serialises the entire `StateStore` as MessagePack
     * and sends it in the initial `SNAPSHOT` message.  Call this method to
     * hydrate the local store with the server's consolidated state before
     * processing any new deltas.
     *
     * Throws a JavaScript error if the bytes cannot be deserialised.
     */
    merge_snapshot(state_bytes: Uint8Array): void;
    /**
     * Create a new `WasmStateStore` for the given node identifier.
     */
    constructor(node_id: string);
    /**
     * Physically remove tombstoned entries from all CRDTs in this store that
     * were deleted at or before `before_ts` (inclusive upper bound).
     *
     * Called in response to a server `PRUNE` broadcast after the server has
     * determined that all connected clients have advanced their clocks past
     * `before_ts`, meaning no client can still be "in the middle of" an
     * operation that references those deleted entries.  Specifically, any RGA
     * node with `id.clock ≤ before_ts` that has been tombstoned is physically
     * removed, and any OR-Set entry with an empty token set is dropped.
     *
     * `before_ts` is passed as `f64` because JavaScript's `Number` type
     * cannot safely represent all `u64` values.
     */
    prune_tombstones(before_ts: number): void;
    /**
     * Delete the element at visible `index` in the named RGA sequence.
     *
     * Returns the resulting [`Envelope`] as **MessagePack bytes**, or
     * `undefined` if `index` is out of bounds.
     */
    seq_delete(key: string, index: number): Uint8Array | undefined;
    /**
     * Insert a JSON-encoded element at `index` in the named RGA sequence.
     *
     * Returns the resulting [`Envelope`] as **MessagePack bytes**.
     */
    seq_insert(key: string, index: number, value_json: string): Uint8Array;
    /**
     * Return all visible elements of the named sequence as a JSON array string.
     *
     * Throws a JavaScript error if serialisation fails.
     */
    seq_items(key: string): string;
    /**
     * Return the number of visible elements in the named sequence.
     */
    seq_len(key: string): number;
    /**
     * Add a JSON-encoded element to the named OR-Set.
     *
     * Returns the resulting [`Envelope`] as **MessagePack bytes**.
     */
    set_add(key: string, value_json: string): Uint8Array;
    /**
     * Returns `true` if the named OR-Set contains the JSON-encoded `value`.
     */
    set_contains(key: string, value_json: string): boolean;
    /**
     * Return all elements of the named OR-Set as a JSON array string.
     *
     * Throws a JavaScript error if serialisation fails.
     */
    set_items(key: string): string;
    /**
     * Write a JSON-encoded value to the named LWW register.
     *
     * Returns the resulting [`Envelope`] serialised as **MessagePack bytes**
     * (`Uint8Array`), ready to broadcast to peer nodes.
     */
    set_register(key: string, value_json: string): Uint8Array;
    /**
     * Remove a JSON-encoded element from the named OR-Set.
     *
     * Returns the resulting [`Envelope`] as **MessagePack bytes**, or
     * `undefined` if the element was not present in the set.
     *
     * Throws a JavaScript error if `value_json` is not valid JSON.
     */
    set_remove(key: string, value_json: string): Uint8Array | undefined;
}

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly __wbg_wasmstatestore_free: (a: number, b: number) => void;
    readonly wasmstatestore_apply_envelope: (a: number, b: number, c: number) => [number, number];
    readonly wasmstatestore_clock: (a: number) => number;
    readonly wasmstatestore_get_register: (a: number, b: number, c: number) => [number, number];
    readonly wasmstatestore_merge_snapshot: (a: number, b: number, c: number) => [number, number];
    readonly wasmstatestore_new: (a: number, b: number) => number;
    readonly wasmstatestore_prune_tombstones: (a: number, b: number) => void;
    readonly wasmstatestore_seq_delete: (a: number, b: number, c: number, d: number) => [number, number];
    readonly wasmstatestore_seq_insert: (a: number, b: number, c: number, d: number, e: number, f: number) => [number, number, number, number];
    readonly wasmstatestore_seq_items: (a: number, b: number, c: number) => [number, number, number, number];
    readonly wasmstatestore_seq_len: (a: number, b: number, c: number) => number;
    readonly wasmstatestore_set_add: (a: number, b: number, c: number, d: number, e: number) => [number, number, number, number];
    readonly wasmstatestore_set_contains: (a: number, b: number, c: number, d: number, e: number) => number;
    readonly wasmstatestore_set_items: (a: number, b: number, c: number) => [number, number, number, number];
    readonly wasmstatestore_set_register: (a: number, b: number, c: number, d: number, e: number) => [number, number, number, number];
    readonly wasmstatestore_set_remove: (a: number, b: number, c: number, d: number, e: number) => [number, number, number, number];
    readonly __wbindgen_exn_store: (a: number) => void;
    readonly __externref_table_alloc: () => number;
    readonly __wbindgen_externrefs: WebAssembly.Table;
    readonly __wbindgen_malloc: (a: number, b: number) => number;
    readonly __externref_table_dealloc: (a: number) => void;
    readonly __wbindgen_realloc: (a: number, b: number, c: number, d: number) => number;
    readonly __wbindgen_free: (a: number, b: number, c: number) => void;
    readonly __wbindgen_start: () => void;
}

export type SyncInitInput = BufferSource | WebAssembly.Module;

/**
 * Instantiates the given `module`, which can either be bytes or
 * a precompiled `WebAssembly.Module`.
 *
 * @param {{ module: SyncInitInput }} module - Passing `SyncInitInput` directly is deprecated.
 *
 * @returns {InitOutput}
 */
export function initSync(module: { module: SyncInitInput } | SyncInitInput): InitOutput;

/**
 * If `module_or_path` is {RequestInfo} or {URL}, makes a request and
 * for everything else, calls `WebAssembly.instantiate` directly.
 *
 * @param {{ module_or_path: InitInput | Promise<InitInput> }} module_or_path - Passing `InitInput` directly is deprecated.
 *
 * @returns {Promise<InitOutput>}
 */
export default function __wbg_init (module_or_path?: { module_or_path: InitInput | Promise<InitInput> } | InitInput | Promise<InitInput>): Promise<InitOutput>;
