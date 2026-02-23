/**
 * TypeScript interface matching the Wasm-bindgen-generated `WasmStateStore`.
 *
 * In production, import the real `WasmStateStore` from the compiled Wasm
 * package (e.g. `import { WasmStateStore } from './crdt_sync.js'`).
 * In tests the interface can be satisfied by any mock object.
 */
export interface WasmStateStore {
  /** Write a JSON-encoded value to the named LWW register. Returns the Envelope JSON. */
  set_register(key: string, value_json: string): string;
  /** Read the current value of a named LWW register as a JSON string, or `undefined`. */
  get_register(key: string): string | undefined;
  /** Apply a remote Envelope (serialised as JSON) to this store. */
  apply_envelope(envelope_json: string): void;
}

/**
 * Payload delivered to every `onUpdate` listener when a property is written
 * through the proxy.
 */
export interface UpdateEvent {
  /** Dot-separated key path that was updated (e.g. `"robot.speed"`). */
  key: string;
  /** The new JavaScript value. */
  value: unknown;
  /**
   * The CRDT Envelope returned by `WasmStateStore.set_register`, serialised
   * as a JSON string.  Broadcast this to peer nodes via `apply_envelope`.
   */
  envelope: string;
}

/** Callback type for `onUpdate` listeners. */
export type UpdateHandler = (event: UpdateEvent) => void;

/**
 * A TypeScript proxy wrapper around `WasmStateStore` that gives frontend
 * developers a **magical, object-oriented** experience.
 *
 * ## How it works
 *
 * 1. **JS `Proxy` interception** – accessing a nested path on `state` returns
 *    another `Proxy`.  Assigning a value anywhere in the tree intercepts the
 *    write and forwards it to the underlying Wasm store via
 *    `set_register(dotPath, JSON.stringify(value))`.
 *
 * 2. **Wasm call** – the interceptor immediately calls
 *    `WasmStateStore.set_register()` so the CRDT operation is recorded and
 *    returns an `Envelope` JSON string ready for broadcasting.
 *
 * 3. **Event emitter** – every write fires all `onUpdate` listeners with the
 *    full `UpdateEvent` (key, value, envelope), enabling React / Vue and other
 *    UI frameworks to react to state changes.
 *
 * ## Usage
 *
 * ```ts
 * import init, { WasmStateStore } from './crdt_sync.js';
 * import { CrdtStateProxy } from './CrdtStateProxy.js';
 *
 * await init();
 * const store = new WasmStateStore('node-1');
 * const proxy = new CrdtStateProxy(store);
 *
 * // Register a listener (e.g. trigger a React re-render).
 * const unsubscribe = proxy.onUpdate(({ key, value, envelope }) => {
 *   console.log(`${key} =`, value);
 *   broadcast(envelope); // send to peers
 * });
 *
 * // Write through the proxy — the interceptor handles everything.
 * proxy.state.speed = 100;
 * proxy.state.robot.x = 42;
 *
 * // Clean up when done.
 * unsubscribe();
 * ```
 */
export class CrdtStateProxy {
  private readonly _store: WasmStateStore;
  private readonly _handlers: Set<UpdateHandler> = new Set();
  private readonly _state: Record<string, unknown>;

  /**
   * Create a new `CrdtStateProxy` backed by the given `WasmStateStore`.
   *
   * @param store - The Wasm state store instance to proxy.
   */
  constructor(store: WasmStateStore) {
    this._store = store;
    this._state = this._makeProxy('');
  }

  // ── Public API ────────────────────────────────────────────────────────

  /**
   * The proxied state object.
   *
   * Assigning any property (or nested property) on this object will
   * automatically call `WasmStateStore.set_register` and fire `onUpdate`
   * listeners.
   *
   * ```ts
   * proxy.state.speed = 100;       // key: "speed"
   * proxy.state.robot.speed = 100; // key: "robot.speed"
   * ```
   */
  get state(): Record<string, unknown> {
    return this._state;
  }

  /**
   * Register a listener that is called whenever a property is written through
   * `proxy.state`.
   *
   * @param handler - Callback receiving an `UpdateEvent`.
   * @returns An unsubscribe function — call it to remove the listener.
   */
  onUpdate(handler: UpdateHandler): () => void {
    this._handlers.add(handler);
    return () => {
      this._handlers.delete(handler);
    };
  }

  // ── Internal helpers ──────────────────────────────────────────────────

  /**
   * Recursively build a `Proxy` for the given dot-path `prefix`.
   *
   * - **`get` trap**: returns a child proxy for the nested path so that deep
   *   assignments like `proxy.state.robot.speed = 100` work correctly.
   * - **`set` trap**: serialises the value, calls `set_register`, and fires
   *   all `onUpdate` listeners.
   */
  private _makeProxy(prefix: string): Record<string, unknown> {
    const children: Record<string, Record<string, unknown>> = {};

    return new Proxy({} as Record<string, unknown>, {
      get: (_target, prop: string) => {
        const key = prefix ? `${prefix}.${prop}` : prop;
        if (!(prop in children)) {
          children[prop] = this._makeProxy(key);
        }
        return children[prop];
      },

      set: (_target, prop: string, value: unknown) => {
        const key = prefix ? `${prefix}.${prop}` : prop;
        const envelope = this._store.set_register(key, JSON.stringify(value));
        this._emit({ key, value, envelope });
        return true;
      },
    });
  }

  /** Dispatch an `UpdateEvent` to all registered handlers. */
  private _emit(event: UpdateEvent): void {
    this._handlers.forEach((handler) => handler(event));
  }
}
