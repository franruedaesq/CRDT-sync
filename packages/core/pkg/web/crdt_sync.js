/**
 * Stub WASM module for @crdt-sync/core.
 *
 * This file is a placeholder that allows the package to be imported in
 * environments where the real compiled WebAssembly is not yet available
 * (e.g. test environments that mock the module, or bundler/SSR setups).
 *
 * In production, replace this file with the real wasm-pack–generated output
 * (run `npm run build:wasm` from the workspace root).
 */

export class WasmStateStore {
    constructor(_nodeId) {
        throw new Error(
            '@crdt-sync/core: WasmStateStore is not available. ' +
            'Please build the WebAssembly module with `npm run build:wasm`.'
        );
    }

    set_register(_key, _value_json) { return ''; }
    get_register(_key) { return undefined; }
    apply_envelope(_envelope_json) {}
}

export default async function init(_input) {
    throw new Error(
        '@crdt-sync/core: WebAssembly module not built. ' +
        'Please run `npm run build:wasm` to compile the Rust WASM module.'
    );
}
