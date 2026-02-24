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
 * Incoming SNAPSHOT (first message from server after connection opens)
 *   → WebSocket.onmessage  { type: 'SNAPSHOT', data: envelopes_json }
 *   → WasmStateStore.apply_envelope() for each envelope in the snapshot
 *   → offline queue flushed so buffered local writes reach the server
 *
 * Incoming UPDATE (peer delta from server)
 *   → WebSocket.onmessage  { type: 'UPDATE', data: batch_json }
 *   → WasmStateStore.apply_envelope() for each envelope in the batch
 * ```
 *
 * ## Snapshot wait
 *
 * On every (re)connection, the manager buffers outgoing envelopes until the
 * server's `SNAPSHOT` message is received.  This ensures the local store is
 * hydrated with the server's consolidated state before the client's own writes
 * are broadcast, preventing stale overwrites.
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
 * const manager = new WebSocketManager(store, proxy, new WebSocket('wss://example.com/rooms/robot-42'));
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
  /** Envelopes queued while the socket is not open or snapshot has not yet arrived. */
  private _offlineQueue: string[] = [];
  /**
   * Set to `true` once the server's initial `SNAPSHOT` message has been applied.
   * Outgoing envelopes are held in `_offlineQueue` until this flag is set so that
   * the local store is fully hydrated before the client's writes reach peers.
   */
  private _snapshotReceived: boolean = false;

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

    // On (re)connection, cancel any pending batch flush and move everything
    // to the offline queue.  Outgoing writes are held there until the server's
    // SNAPSHOT message arrives and the local store has been hydrated.
    ws.onopen = () => {
      this._snapshotReceived = false;
      this._cancelFlush?.();
      this._cancelFlush = null;
      const pending = this._pendingEnvelopes;
      this._pendingEnvelopes = [];
      this._offlineQueue.push(...pending);
      // Do not flush yet — wait for the SNAPSHOT before sending local writes.
    };

    // Apply messages received from the server.
    // The server sends two kinds of typed messages:
    //   { type: 'SNAPSHOT', data: string }  — full state on first connect
    //   { type: 'UPDATE',   data: string }  — a peer's envelope batch
    // For backward-compatibility with older relay versions that send raw
    // envelope arrays directly, the legacy format is still handled as a
    // fallback.
    ws.onmessage = (event) => {
      let parsed: unknown;
      try {
        parsed = JSON.parse(event.data);
      } catch {
        parsed = null;
      }

      // ── Typed protocol (new server) ──────────────────────────────────────
      if (parsed !== null && typeof parsed === 'object' && !Array.isArray(parsed) && 'type' in (parsed as object)) {
        const msg = parsed as { type: string; data: string };

        if (msg.type === 'SNAPSHOT') {
          // Hydrate local store with the server's consolidated state.
          let snapshotEnvelopes: unknown;
          try {
            snapshotEnvelopes = JSON.parse(msg.data);
          } catch {
            snapshotEnvelopes = [];
          }
          if (Array.isArray(snapshotEnvelopes)) {
            for (const env of snapshotEnvelopes) {
              if (typeof env === 'string') {
                this._store.apply_envelope(env);
              }
            }
          }
          // Allow outgoing writes now that we hold the server's state.
          this._snapshotReceived = true;
          this._proxy.notifyRemoteUpdate();
          this._flushOfflineQueue();
          return;
        }

        if (msg.type === 'UPDATE') {
          // Apply a peer's batch of envelopes.
          let updateEnvelopes: unknown;
          try {
            updateEnvelopes = JSON.parse(msg.data);
          } catch {
            updateEnvelopes = null;
          }
          if (Array.isArray(updateEnvelopes)) {
            for (const env of updateEnvelopes) {
              if (typeof env === 'string') {
                this._store.apply_envelope(env);
              }
            }
          } else {
            this._store.apply_envelope(msg.data);
          }
          this._proxy.notifyRemoteUpdate();
          return;
        }
      }

      // ── Legacy fallback (old relay format) ───────────────────────────────
      // Peers may send either a single envelope JSON string or a JSON array
      // of envelope strings (batch format).
      if (Array.isArray(parsed)) {
        for (const env of parsed) {
          if (typeof env === 'string') {
            this._store.apply_envelope(env);
          }
        }
      } else {
        this._store.apply_envelope(event.data);
      }
      // Notify UI listeners (e.g. React) that remote state has been applied.
      this._proxy.notifyRemoteUpdate();
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
   * to the offline queue if the socket is not currently open or the snapshot
   * has not yet been received.
   */
  private _flushBatch(): void {
    const envelopes = this._pendingEnvelopes.splice(0);
    if (envelopes.length === 0) return;

    const ws = this._ws;
    if (ws.readyState === 1 /* OPEN */ && this._snapshotReceived) {
      ws.send(JSON.stringify(envelopes));
    } else {
      this._offlineQueue.push(...envelopes);
    }
  }

  /**
   * Send all offline-queued envelopes in a single batch.
   * Called after the server's SNAPSHOT has been applied so that buffered
   * local writes finally reach peers.
   */
  private _flushOfflineQueue(): void {
    const offline = this._offlineQueue.splice(0);
    if (offline.length === 0) return;
    const ws = this._ws;
    if (ws.readyState === 1 /* OPEN */) {
      ws.send(JSON.stringify(offline));
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
