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
  /** The register key that was updated (e.g. `"speed"` or `"robot.speed"`). */
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
 * developers a transparent, object-oriented experience.
 *
 * ## Reading state
 *
 * Access any registered key directly on `proxy.state`:
 *
 * ```ts
 * proxy.state.speed        // returns the registered value, or undefined
 * proxy.state["robot.speed"] // dot-path keys via bracket notation
 * ```
 *
 * ## Writing state
 *
 * Assign any value — primitives, objects, arrays:
 *
 * ```ts
 * proxy.state.speed = 100;
 * proxy.state.robot = { x: 10, y: 20 };    // store the whole object
 * proxy.state["robot.speed"] = 100;         // dot-path key
 * ```
 *
 * ## Listening for changes
 *
 * - `onUpdate(handler)` — fires on **local** writes with the full `UpdateEvent`
 *   (key, value, envelope). Used by `WebSocketManager` to collect outgoing envelopes.
 * - `onChange(handler)` — fires on **any** change: local writes _and_ remote
 *   `apply_envelope` notifications. Use this in UI frameworks to trigger re-renders.
 *
 * ```ts
 * const unsubscribe = proxy.onChange(() => setTick(t => t + 1));
 * ```
 */
export class CrdtStateProxy {
  private readonly _store: WasmStateStore;
  private readonly _handlers: Set<UpdateHandler> = new Set();
  private readonly _changeHandlers: Set<() => void> = new Set();
  private readonly _state: Record<string, unknown>;

  /**
   * Create a new `CrdtStateProxy` backed by the given `WasmStateStore`.
   *
   * @param store - The Wasm state store instance to proxy.
   */
  constructor(store: WasmStateStore) {
    this._store = store;
    this._state = this._makeProxy();
  }

  // ── Public API ────────────────────────────────────────────────────────

  /**
   * The proxied state object.
   *
   * Reading a key returns the registered value, or `undefined` if it has
   * not been set yet.  Assigning a value calls `WasmStateStore.set_register`
   * and fires all `onUpdate` and `onChange` listeners.
   */
  get state(): Record<string, unknown> {
    return this._state;
  }

  /**
   * Register a listener that fires whenever a value is written through
   * `proxy.state` (local writes only).
   *
   * The `UpdateEvent` carries the register key, new value, and the CRDT
   * envelope — forward the envelope to peers via `apply_envelope`.
   *
   * @returns An unsubscribe function.
   */
  onUpdate(handler: UpdateHandler): () => void {
    this._handlers.add(handler);
    return () => {
      this._handlers.delete(handler);
    };
  }

  /**
   * Register a listener that fires whenever state changes — whether from a
   * local write or an incoming remote update (after `notifyRemoteUpdate`).
   *
   * Use this in UI frameworks to trigger re-renders:
   *
   * ```ts
   * proxy.onChange(() => setTick(t => t + 1));
   * ```
   *
   * @returns An unsubscribe function.
   */
  onChange(handler: () => void): () => void {
    this._changeHandlers.add(handler);
    return () => {
      this._changeHandlers.delete(handler);
    };
  }

  /**
   * Notify all `onChange` listeners that remote state has been applied to the
   * store.  Called by `WebSocketManager` after every `apply_envelope` so that
   * UI frameworks re-render with the latest state.
   */
  notifyRemoteUpdate(): void {
    this._emitChange();
  }

  // ── Internal helpers ──────────────────────────────────────────────────

  /**
   * Build the state `Proxy`.
   *
   * - **`get` trap**: reads the value from the WASM store via `get_register`.
   *   Returns the parsed value, or `undefined` if the key has not been registered.
   * - **`set` trap**: serialises the value, calls `set_register`, and fires
   *   all `onUpdate` and `onChange` listeners.
   *
   * Keys may be dot-separated paths (e.g. `"robot.speed"`) when using bracket
   * notation: `proxy.state["robot.speed"] = 100`.
   */
  private _makeProxy(): Record<string, unknown> {
    return new Proxy({} as Record<string, unknown>, {
      get: (_target, prop) => {
        if (typeof prop === 'symbol') return undefined;
        const raw = this._store.get_register(String(prop));
        return raw !== undefined ? JSON.parse(raw) : undefined;
      },

      set: (_target, prop, value: unknown) => {
        if (typeof prop === 'symbol') return false;
        const key = String(prop);
        const envelope = this._store.set_register(key, JSON.stringify(value));
        this._emit({ key, value, envelope });
        this._emitChange();
        return true;
      },
    });
  }

  /** Dispatch an `UpdateEvent` to all `onUpdate` handlers. */
  private _emit(event: UpdateEvent): void {
    this._handlers.forEach((handler) => handler(event));
  }

  /** Notify all `onChange` handlers of a state change. */
  private _emitChange(): void {
    this._changeHandlers.forEach((handler) => handler());
  }
}
