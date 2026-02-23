import { CrdtStateProxy } from './CrdtStateProxy.js';
import type { WasmStateStore } from './CrdtStateProxy.js';

/**
 * Minimal subset of the browser `WebSocket` API used by `WebSocketManager`.
 *
 * The real browser `WebSocket` satisfies this interface out-of-the-box.
 * In tests, a plain mock object can be used instead.
 */
export interface WebSocketLike {
  /** Current connection state (0 = CONNECTING, 1 = OPEN, 2 = CLOSING, 3 = CLOSED). */
  readonly readyState: number;
  /** Send a UTF-8 string frame to the server. */
  send(data: string): void;
  /** Initiate the closing handshake. */
  close(): void;
  /** Fired when a message frame is received. */
  onmessage: ((event: { data: string }) => void) | null;
  /** Fired when the connection is established. */
  onopen: ((event: unknown) => void) | null;
  /** Fired when the connection is closed. */
  onclose: ((event: unknown) => void) | null;
  /** Fired when an error occurs. */
  onerror: ((event: unknown) => void) | null;
}

/**
 * Bridges a `CrdtStateProxy` to a WebSocket connection so that every CRDT
 * operation produced locally is broadcast to peers, and every envelope
 * received from a peer is applied to the local store.
 *
 * ## Data flow
 *
 * ```
 * Local write
 *   → CrdtStateProxy.onUpdate  (envelope)
 *   → WebSocket.send(envelope)            // broadcast to peers
 *
 * Incoming message
 *   → WebSocket.onmessage  (envelope)
 *   → WasmStateStore.apply_envelope()    // merge into local store
 * ```
 *
 * ## Usage
 *
 * ```ts
 * import init, { WasmStateStore } from './crdt_sync.js';
 * import { CrdtStateProxy, WebSocketManager } from './index.js';
 *
 * await init();
 * const store = new WasmStateStore('node-1');
 * const proxy = new CrdtStateProxy(store);
 * const manager = new WebSocketManager(store, proxy, new WebSocket('wss://example.com/sync'));
 *
 * // Writes are automatically broadcast to peers.
 * proxy.state.robot = { x: 10, y: 20 };
 *
 * // Clean up.
 * manager.disconnect();
 * ```
 */
export class WebSocketManager {
  private readonly _store: WasmStateStore;
  private readonly _proxy: CrdtStateProxy;
  private readonly _ws: WebSocketLike;
  private _unsubscribe: (() => void) | null = null;

  /**
   * Create a `WebSocketManager` and attach it to the given WebSocket.
   *
   * @param store - The Wasm state store. Incoming peer envelopes will be
   *   applied to this store via `apply_envelope`.
   * @param proxy - The CRDT state proxy. Outgoing envelopes produced by
   *   `set_register` calls will be read from the proxy's `onUpdate` events.
   * @param ws - An open or connecting WebSocket (or any `WebSocketLike` object).
   */
  constructor(store: WasmStateStore, proxy: CrdtStateProxy, ws: WebSocketLike) {
    this._store = store;
    this._proxy = proxy;
    this._ws = ws;
    this._attach();
  }

  // ── Internal setup ────────────────────────────────────────────────────

  private _attach(): void {
    const ws = this._ws;

    // Subscribe to proxy updates once the connection is open so that every
    // CRDT envelope is immediately broadcast.
    ws.onopen = () => {
      this._unsubscribe = this._proxy.onUpdate(({ envelope }) => {
        if (ws.readyState === 1 /* OPEN */) {
          ws.send(envelope);
        }
      });
    };

    // Apply envelopes received from peers to the local store.
    ws.onmessage = (event) => {
      this._store.apply_envelope(event.data);
    };

    // Unsubscribe from proxy updates when the connection closes.
    ws.onclose = () => {
      this._unsubscribe?.();
      this._unsubscribe = null;
    };

    // Also clean up on errors so stale listeners are not kept alive.
    ws.onerror = () => {
      this._unsubscribe?.();
      this._unsubscribe = null;
    };
  }

  // ── Public API ────────────────────────────────────────────────────────

  /**
   * Unsubscribe from proxy updates and close the WebSocket connection.
   *
   * Safe to call more than once.
   */
  disconnect(): void {
    this._unsubscribe?.();
    this._unsubscribe = null;
    this._ws.close();
  }
}
