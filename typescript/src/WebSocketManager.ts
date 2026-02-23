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
 *   → CrdtStateProxy.onUpdate  (envelope collected in _pendingEnvelopes)
 *   → requestAnimationFrame / setTimeout schedules a batch flush
 *   → WebSocket.send(JSON.stringify(envelopes))   // one payload per frame
 *
 * Incoming message (single envelope or JSON array of envelopes)
 *   → WebSocket.onmessage
 *   → WasmStateStore.apply_envelope()    // merge into local store
 * ```
 *
 * ## Throttling / batching
 *
 * Multiple proxy writes that occur within the same JavaScript task (e.g. a
 * 60 FPS game loop) are collected in `_pendingEnvelopes` and sent as a single
 * JSON array payload on the next animation frame (browser) or the next
 * `setTimeout(fn, 0)` tick (Node.js / non-browser environments).  This keeps
 * network traffic proportional to frame rate rather than to the raw mutation
 * rate.
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
 * // Writes are automatically batched and broadcast to peers.
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
  /** Envelopes collected in the current frame, waiting for the batch flush. */
  private _pendingEnvelopes: string[] = [];
  /** Cancels the currently scheduled batch flush (rAF or setTimeout handle). */
  private _cancelFlush: (() => void) | null = null;
  /** Envelopes queued while the socket is not open, flushed on reconnection. */
  private _offlineQueue: string[] = [];

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

    // Collect envelopes and schedule a batch flush once per frame so that
    // multiple synchronous writes (e.g. a 60 FPS game loop) are coalesced
    // into a single network payload instead of one message per mutation.
    this._unsubscribe = this._proxy.onUpdate(({ envelope }) => {
      this._pendingEnvelopes.push(envelope);
      this._scheduleBatchFlush();
    });

    // On (re)connection, immediately flush pending envelopes together with any
    // envelopes that were queued while the socket was offline.
    ws.onopen = () => {
      // Cancel any scheduled flush — we will drain everything right now.
      this._cancelFlush?.();
      this._cancelFlush = null;
      const offline = this._offlineQueue;
      const pending = this._pendingEnvelopes;
      this._offlineQueue = [];
      this._pendingEnvelopes = [];
      const batch = [...offline, ...pending];
      if (batch.length > 0) {
        ws.send(JSON.stringify(batch));
      }
    };

    // Apply envelopes received from peers.  Peers may send either a single
    // envelope JSON string or a JSON array of envelope strings (batch format).
    ws.onmessage = (event) => {
      let parsed: unknown;
      try {
        parsed = JSON.parse(event.data);
      } catch {
        parsed = null;
      }
      if (Array.isArray(parsed)) {
        for (const env of parsed) {
          this._store.apply_envelope(env as string);
        }
      } else {
        // Fallback: treat the raw frame data as a single envelope string.
        this._store.apply_envelope(event.data);
      }
    };

    // On close or error the subscription stays active so that writes made
    // while offline are buffered and flushed when the socket reconnects.
    ws.onclose = () => { /* keep buffering */ };
    ws.onerror = () => { /* keep buffering */ };
  }

  /**
   * Schedule a single batch flush for the current frame.  Subsequent calls
   * before the flush fires are no-ops (only one flush is ever outstanding).
   *
   * Uses `requestAnimationFrame` when available (browser, ~60 FPS cadence),
   * falling back to `setTimeout(fn, 0)` in non-browser environments.
   */
  private _scheduleBatchFlush(): void {
    if (this._cancelFlush !== null) return; // already scheduled

    const doFlush = () => {
      this._cancelFlush = null;
      this._flushBatch();
    };

    if (typeof requestAnimationFrame === 'function') {
      const id = requestAnimationFrame(doFlush);
      this._cancelFlush = () => cancelAnimationFrame(id);
    } else {
      const id = setTimeout(doFlush, 0);
      this._cancelFlush = () => clearTimeout(id);
    }
  }

  /**
   * Send all pending envelopes as a single JSON-array payload, or move them
   * to the offline queue if the socket is not currently open.
   */
  private _flushBatch(): void {
    const envelopes = this._pendingEnvelopes.splice(0);
    if (envelopes.length === 0) return;

    const ws = this._ws;
    if (ws.readyState === 1 /* OPEN */) {
      ws.send(JSON.stringify(envelopes));
    } else {
      this._offlineQueue.push(...envelopes);
    }
  }

  // ── Public API ────────────────────────────────────────────────────────

  /**
   * Unsubscribe from proxy updates, discard any buffered envelopes, and close
   * the WebSocket connection.
   *
   * Safe to call more than once.
   */
  disconnect(): void {
    this._unsubscribe?.();
    this._unsubscribe = null;
    this._cancelFlush?.();
    this._cancelFlush = null;
    this._pendingEnvelopes = [];
    this._offlineQueue = [];
    this._ws.close();
  }
}
