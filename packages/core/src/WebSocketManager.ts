import { encode as msgpackEncode, decode as msgpackDecode } from '@msgpack/msgpack';
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
  /** Send a binary frame to the server. */
  send(data: Uint8Array | ArrayBuffer): void;
  /** Initiate the closing handshake. */
  close(): void;
  /** Fired when a message frame is received. */
  onmessage: ((event: { data: ArrayBuffer | Uint8Array }) => void) | null;
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
 *   → CrdtStateProxy.onUpdate  (envelope Uint8Array collected in _pendingEnvelopes)
 *   → requestAnimationFrame / setTimeout schedules a batch flush
 *   → WebSocket.send(msgpackEncode(envelopes))   // one binary payload per frame
 *
 * Incoming SNAPSHOT (first message from server after connection opens)
 *   → WebSocket.onmessage  { data: ArrayBuffer }  decoded as { type: 'SNAPSHOT', data: Uint8Array[] | Uint8Array }
 *   → envelope array → WasmStateStore.apply_envelope() for each envelope
 *   → OR StateStore msgpack blob → WasmStateStore.merge_snapshot()
 *   → offline queue flushed so buffered local writes reach the server
 *
 * Incoming UPDATE (peer delta from server)
 *   → WebSocket.onmessage  { data: ArrayBuffer }  decoded as { type: 'UPDATE', data: Uint8Array[] }
 *   → WasmStateStore.apply_envelope() for each envelope in the array
 *
 * Incoming PRUNE (server GC broadcast)
 *   → WebSocket.onmessage  { data: ArrayBuffer }  decoded as { type: 'PRUNE', data: number }
 *   → WasmStateStore.prune_tombstones(data) to physically erase dead entries
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
  /** Envelope bytes collected in the current frame, waiting for the batch flush. */
  private _pendingEnvelopes: Uint8Array[] = [];
  /** Cancels the currently scheduled batch flush (rAF or setTimeout handle). */
  private _cancelFlush: (() => void) | null = null;
  /** Envelope bytes queued while the socket is not open or snapshot has not yet arrived. */
  private _offlineQueue: Uint8Array[] = [];
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
    // All server messages are binary-encoded MessagePack frames:
    //   { type: 'SNAPSHOT', data: Uint8Array[] | Uint8Array }
    //   { type: 'UPDATE',   data: Uint8Array[] }
    //   { type: 'PRUNE',    data: number }
    ws.onmessage = (event) => {
      // Normalise to Uint8Array regardless of whether the WebSocket delivers
      // an ArrayBuffer (browsers) or a Buffer/Uint8Array (Node.js / ws lib).
      const raw = event.data;
      let bytes: Uint8Array;
      if (raw instanceof ArrayBuffer) {
        bytes = new Uint8Array(raw);
      } else if (raw instanceof Uint8Array) {
        bytes = raw;
      } else {
        // Non-binary frame — ignore (not part of the binary protocol).
        return;
      }

      let msg: unknown;
      try {
        msg = msgpackDecode(bytes);
      } catch {
        return;
      }

      if (msg === null || typeof msg !== 'object' || !('type' in (msg as object))) return;
      const { type, data } = msg as { type: string; data: unknown };

      // ── SNAPSHOT ───────────────────────────────────────────────────────
      if (type === 'SNAPSHOT') {
        if (Array.isArray(data)) {
          // TypeScript relay: array of MessagePack-encoded envelope blobs.
          for (const envBytes of data) {
            if (envBytes instanceof Uint8Array) {
              this._store.apply_envelope(envBytes);
            }
          }
        } else if (data instanceof Uint8Array && this._store.merge_snapshot) {
          // Rust relay: full StateStore serialised as MessagePack.
          this._store.merge_snapshot(data);
        }
        this._snapshotReceived = true;
        this._proxy.notifyRemoteUpdate();
        this._flushOfflineQueue();
        return;
      }

      // ── UPDATE ─────────────────────────────────────────────────────────
      if (type === 'UPDATE') {
        if (Array.isArray(data)) {
          for (const envBytes of data) {
            if (envBytes instanceof Uint8Array) {
              this._store.apply_envelope(envBytes);
            }
          }
        } else if (data instanceof Uint8Array) {
          this._store.apply_envelope(data);
        }
        this._proxy.notifyRemoteUpdate();
        return;
      }

      // ── PRUNE ──────────────────────────────────────────────────────────
      // The server has determined that all clients have advanced their clocks
      // past `data` (a Lamport timestamp), so tombstones created at or before
      // that timestamp can be physically erased from memory.
      if (type === 'PRUNE' && typeof data === 'number') {
        if (this._store.prune_tombstones) {
          this._store.prune_tombstones(data);
        }
        this._proxy.notifyRemoteUpdate();
        return;
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
   * Send all pending envelopes as a single MessagePack-encoded binary frame,
   * or move them to the offline queue if the socket is not currently open or
   * the snapshot has not yet been received.
   */
  private _flushBatch(): void {
    const envelopes = this._pendingEnvelopes.splice(0);
    if (envelopes.length === 0) return;

    const ws = this._ws;
    if (ws.readyState === 1 /* OPEN */ && this._snapshotReceived) {
      ws.send(msgpackEncode(envelopes));
    } else {
      this._offlineQueue.push(...envelopes);
    }
  }

  /**
   * Send all offline-queued envelopes in a single binary batch.
   * Called after the server's SNAPSHOT has been applied so that buffered
   * local writes finally reach peers.
   */
  private _flushOfflineQueue(): void {
    const offline = this._offlineQueue.splice(0);
    if (offline.length === 0) return;
    const ws = this._ws;
    if (ws.readyState === 1 /* OPEN */) {
      ws.send(msgpackEncode(offline));
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
