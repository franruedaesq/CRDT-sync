import { encode as msgpackEncode, decode as msgpackDecode } from '@msgpack/msgpack';
import { CrdtStateProxy, WasmStateStore } from '../CrdtStateProxy';
import { WebSocketManager, WebSocketLike } from '../WebSocketManager';

// ── Helpers ───────────────────────────────────────────────────────────────────

function makeStore(): jest.Mocked<WasmStateStore> {
  const store: Record<string, string> = {};
  return {
    set_register: jest.fn((key: string, value_json: string) => {
      store[key] = value_json;
      return msgpackEncode({ timestamp: 1, node_id: 'node-1', op: { kind: 'Register', key, op: { value: JSON.parse(value_json), timestamp: 1, node_id: 'node-1' } } });
    }),
    get_register: jest.fn((key: string) => store[key]),
    apply_envelope: jest.fn(),
  };
}

interface MockWebSocket extends WebSocketLike {
  readyState: number;
  send: jest.Mock;
  close: jest.Mock;
  /** Helper: simulate the connection opening. */
  simulateOpen(): void;
  /** Helper: simulate a SNAPSHOT message from the server (binary). */
  simulateSnapshot(envelopes: Uint8Array[]): void;
  /** Helper: simulate an UPDATE message from the server (binary). */
  simulateUpdate(envelopes: Uint8Array[]): void;
  /** Helper: simulate a PRUNE message from the server (binary). */
  simulatePrune(beforeTs: number): void;
  /** Helper: simulate the connection closing. */
  simulateClose(): void;
  /** Helper: simulate an error on the connection. */
  simulateError(): void;
}

function makeWebSocket(): MockWebSocket {
  const ws: MockWebSocket = {
    readyState: 0, // CONNECTING
    send: jest.fn(),
    close: jest.fn(),
    onmessage: null,
    onopen: null,
    onclose: null,
    onerror: null,
    simulateOpen() {
      ws.readyState = 1; // OPEN
      ws.onopen?.({});
    },
    simulateSnapshot(envelopes: Uint8Array[]) {
      const bytes = msgpackEncode({ type: 'SNAPSHOT', data: envelopes });
      ws.onmessage?.({ data: bytes });
    },
    simulateUpdate(envelopes: Uint8Array[]) {
      const bytes = msgpackEncode({ type: 'UPDATE', data: envelopes });
      ws.onmessage?.({ data: bytes });
    },
    simulatePrune(beforeTs: number) {
      const bytes = msgpackEncode({ type: 'PRUNE', data: beforeTs });
      ws.onmessage?.({ data: bytes });
    },
    simulateClose() {
      ws.readyState = 3; // CLOSED
      ws.onclose?.({});
    },
    simulateError() {
      ws.readyState = 3; // CLOSED – real browsers close the socket on error
      ws.onerror?.({});
    },
  };
  return ws;
}

// Helper: decode a binary batch sent via ws.send and return the array of decoded envelopes.
function decodeSentBatch(callArg: unknown): Record<string, unknown>[] {
  const bytes = callArg instanceof ArrayBuffer
    ? new Uint8Array(callArg)
    : callArg as Uint8Array;
  const envelopeBlobs = msgpackDecode(bytes) as Uint8Array[];
  return envelopeBlobs.map((b) => msgpackDecode(b) as Record<string, unknown>);
}

// ── Outbound: local writes are broadcast over WebSocket ───────────────────────

describe('WebSocketManager – outbound broadcast', () => {
  beforeEach(() => jest.useFakeTimers());
  afterEach(() => jest.useRealTimers());

  test('sends the envelope returned by set_register over WebSocket after open and snapshot', () => {
    const store = makeStore();
    const proxy = new CrdtStateProxy(store);
    const ws = makeWebSocket();
    new WebSocketManager(store, proxy, ws);

    ws.simulateOpen();
    ws.simulateSnapshot([]); // hydrate: enables outgoing sends
    (proxy.state as Record<string, unknown>).x = 10;
    jest.runAllTimers();

    expect(ws.send).toHaveBeenCalledTimes(1);
    // The payload is a MessagePack-encoded array of envelope blobs.
    const envelopes = decodeSentBatch(ws.send.mock.calls[0][0]);
    expect(envelopes).toHaveLength(1);
    expect(envelopes[0]).toMatchObject({ op: { key: 'x' } });
  });

  test('does not send before the connection is open', () => {
    const store = makeStore();
    const proxy = new CrdtStateProxy(store);
    const ws = makeWebSocket(); // stays in CONNECTING state
    new WebSocketManager(store, proxy, ws);

    (proxy.state as Record<string, unknown>).x = 10; // no onopen yet
    jest.runAllTimers();

    expect(ws.send).not.toHaveBeenCalled();
  });

  test('does not send before the snapshot is received', () => {
    const store = makeStore();
    const proxy = new CrdtStateProxy(store);
    const ws = makeWebSocket();
    new WebSocketManager(store, proxy, ws);

    ws.simulateOpen(); // open but no snapshot yet
    (proxy.state as Record<string, unknown>).x = 10;
    jest.runAllTimers();

    expect(ws.send).not.toHaveBeenCalled();
  });

  test('batches multiple synchronous writes into a single send', () => {
    const store = makeStore();
    const proxy = new CrdtStateProxy(store);
    const ws = makeWebSocket();
    new WebSocketManager(store, proxy, ws);

    ws.simulateOpen();
    ws.simulateSnapshot([]);
    (proxy.state as Record<string, unknown>).a = 1;
    (proxy.state as Record<string, unknown>).b = 2;
    (proxy.state as Record<string, unknown>).c = 3;
    jest.runAllTimers();

    // All three writes are coalesced into one send call.
    expect(ws.send).toHaveBeenCalledTimes(1);
    const envelopes = decodeSentBatch(ws.send.mock.calls[0][0]);
    expect(envelopes).toHaveLength(3);
  });

  test('sends separate batches for writes in separate frames', () => {
    const store = makeStore();
    const proxy = new CrdtStateProxy(store);
    const ws = makeWebSocket();
    new WebSocketManager(store, proxy, ws);

    ws.simulateOpen();
    ws.simulateSnapshot([]);
    (proxy.state as Record<string, unknown>).a = 1;
    jest.runAllTimers(); // flush first frame

    (proxy.state as Record<string, unknown>).b = 2;
    jest.runAllTimers(); // flush second frame

    expect(ws.send).toHaveBeenCalledTimes(2);
    const envelopes1 = decodeSentBatch(ws.send.mock.calls[0][0]);
    const envelopes2 = decodeSentBatch(ws.send.mock.calls[1][0]);
    expect(envelopes1[0]).toMatchObject({ op: { key: 'a' } });
    expect(envelopes2[0]).toMatchObject({ op: { key: 'b' } });
  });

  test('sends the envelope for nested property writes', () => {
    const store = makeStore();
    const proxy = new CrdtStateProxy(store);
    const ws = makeWebSocket();
    new WebSocketManager(store, proxy, ws);

    ws.simulateOpen();
    ws.simulateSnapshot([]);
    (proxy.state as Record<string, unknown>)['robot.speed'] = 99;
    jest.runAllTimers();

    const envelopes = decodeSentBatch(ws.send.mock.calls[0][0]);
    expect(envelopes[0]).toMatchObject({ op: { key: 'robot.speed' } });
  });

  test('stops broadcasting after disconnect', () => {
    const store = makeStore();
    const proxy = new CrdtStateProxy(store);
    const ws = makeWebSocket();
    const manager = new WebSocketManager(store, proxy, ws);

    ws.simulateOpen();
    ws.simulateSnapshot([]);
    manager.disconnect();

    (proxy.state as Record<string, unknown>).x = 10;
    jest.runAllTimers();
    expect(ws.send).not.toHaveBeenCalled();
  });

  test('stops broadcasting after WebSocket closes', () => {
    const store = makeStore();
    const proxy = new CrdtStateProxy(store);
    const ws = makeWebSocket();
    new WebSocketManager(store, proxy, ws);

    ws.simulateOpen();
    ws.simulateSnapshot([]);
    ws.simulateClose();

    (proxy.state as Record<string, unknown>).x = 10;
    jest.runAllTimers();
    expect(ws.send).not.toHaveBeenCalled();
  });

  test('stops broadcasting after a WebSocket error', () => {
    const store = makeStore();
    const proxy = new CrdtStateProxy(store);
    const ws = makeWebSocket();
    new WebSocketManager(store, proxy, ws);

    ws.simulateOpen();
    ws.simulateSnapshot([]);
    ws.simulateError();

    (proxy.state as Record<string, unknown>).x = 10;
    jest.runAllTimers();
    expect(ws.send).not.toHaveBeenCalled();
  });
});

// ── Inbound: SNAPSHOT and UPDATE messages ────────────────────────────────────

describe('WebSocketManager – inbound SNAPSHOT', () => {
  test('applies each envelope in the snapshot and notifies', () => {
    const store = makeStore();
    const proxy = new CrdtStateProxy(store);
    const ws = makeWebSocket();
    new WebSocketManager(store, proxy, ws);

    const env1 = msgpackEncode({ timestamp: 1, node_id: 'n1', op: { kind: 'Register', key: 'a', op: {} } });
    const env2 = msgpackEncode({ timestamp: 2, node_id: 'n2', op: { kind: 'Register', key: 'b', op: {} } });
    ws.simulateOpen();
    ws.simulateSnapshot([env1, env2]);

    expect(store.apply_envelope).toHaveBeenCalledTimes(2);
    expect(store.apply_envelope).toHaveBeenNthCalledWith(1, env1);
    expect(store.apply_envelope).toHaveBeenNthCalledWith(2, env2);
  });

  test('empty snapshot applies no envelopes but still enables outgoing sends', () => {
    const store = makeStore();
    const proxy = new CrdtStateProxy(store);
    const ws = makeWebSocket();

    jest.useFakeTimers();
    new WebSocketManager(store, proxy, ws);

    ws.simulateOpen();
    ws.simulateSnapshot([]);
    expect(store.apply_envelope).not.toHaveBeenCalled();

    // After snapshot, local writes should be sent
    (proxy.state as Record<string, unknown>).n = 1;
    jest.runAllTimers();
    expect(ws.send).toHaveBeenCalledTimes(1);
    jest.useRealTimers();
  });

  test('calls merge_snapshot for a StateStore-format snapshot (Rust relay)', () => {
    const store = makeStore();
    store.merge_snapshot = jest.fn();
    const proxy = new CrdtStateProxy(store);
    const ws = makeWebSocket();
    new WebSocketManager(store, proxy, ws);

    const stateStoreBytes = msgpackEncode({
      node_id: 'server',
      clock: { time: 3 },
      registers: { 'robot.x': { node_id: 'server', state: [42, 3, 'server'] } },
      sets: {},
      sequences: {},
    });
    ws.simulateOpen();
    // Simulate a SNAPSHOT whose data is a single Uint8Array (Rust relay StateStore)
    const snapshotBytes = msgpackEncode({ type: 'SNAPSHOT', data: stateStoreBytes });
    ws.onmessage?.({ data: snapshotBytes });

    expect(store.merge_snapshot).toHaveBeenCalledTimes(1);
    expect(store.merge_snapshot).toHaveBeenCalledWith(stateStoreBytes);
    expect(store.apply_envelope).not.toHaveBeenCalled();
  });

  test('does not call merge_snapshot when store does not support it', () => {
    // Store has no merge_snapshot — should not throw, and apply_envelope not called either
    const store = makeStore(); // no merge_snapshot property
    const proxy = new CrdtStateProxy(store);
    const ws = makeWebSocket();
    new WebSocketManager(store, proxy, ws);

    const stateStoreBytes = msgpackEncode({ node_id: 'server', clock: { time: 0 }, registers: {}, sets: {}, sequences: {} });
    ws.simulateOpen();
    expect(() => {
      const snapshotBytes = msgpackEncode({ type: 'SNAPSHOT', data: stateStoreBytes });
      ws.onmessage?.({ data: snapshotBytes });
    }).not.toThrow();
    expect(store.apply_envelope).not.toHaveBeenCalled();
  });

  test('StateStore-format snapshot still enables outgoing sends', () => {
    jest.useFakeTimers();
    const store = makeStore();
    store.merge_snapshot = jest.fn();
    const proxy = new CrdtStateProxy(store);
    const ws = makeWebSocket();
    new WebSocketManager(store, proxy, ws);

    const stateStoreBytes = msgpackEncode({ node_id: 'server', clock: { time: 0 }, registers: {}, sets: {}, sequences: {} });
    ws.simulateOpen();
    const snapshotBytes = msgpackEncode({ type: 'SNAPSHOT', data: stateStoreBytes });
    ws.onmessage?.({ data: snapshotBytes });

    (proxy.state as Record<string, unknown>).x = 7;
    jest.runAllTimers();
    expect(ws.send).toHaveBeenCalledTimes(1);
    jest.useRealTimers();
  });
});

describe('WebSocketManager – inbound UPDATE', () => {
  test('calls apply_envelope for each envelope in an UPDATE batch', () => {
    const store = makeStore();
    const proxy = new CrdtStateProxy(store);
    const ws = makeWebSocket();
    new WebSocketManager(store, proxy, ws);

    const env1 = msgpackEncode({ timestamp: 1, node_id: 'n2', op: { kind: 'Register', key: 'a', op: {} } });
    const env2 = msgpackEncode({ timestamp: 2, node_id: 'n2', op: { kind: 'Register', key: 'b', op: {} } });
    ws.simulateUpdate([env1, env2]);

    expect(store.apply_envelope).toHaveBeenCalledTimes(2);
    expect(store.apply_envelope).toHaveBeenNthCalledWith(1, env1);
    expect(store.apply_envelope).toHaveBeenNthCalledWith(2, env2);
  });
});

// ── Inbound: PRUNE message calls prune_tombstones ────────────────────────────

describe('WebSocketManager – inbound PRUNE', () => {
  test('calls prune_tombstones with the before_ts from the PRUNE message', () => {
    const store = makeStore();
    store.prune_tombstones = jest.fn();
    const proxy = new CrdtStateProxy(store);
    const ws = makeWebSocket();
    new WebSocketManager(store, proxy, ws);

    ws.simulatePrune(42);

    expect(store.prune_tombstones).toHaveBeenCalledTimes(1);
    expect(store.prune_tombstones).toHaveBeenCalledWith(42);
  });

  test('does not throw when store does not support prune_tombstones', () => {
    const store = makeStore(); // no prune_tombstones
    const proxy = new CrdtStateProxy(store);
    const ws = makeWebSocket();
    new WebSocketManager(store, proxy, ws);

    expect(() => ws.simulatePrune(10)).not.toThrow();
  });
});

// ── disconnect ────────────────────────────────────────────────────────────────

describe('WebSocketManager – disconnect', () => {
  test('calls ws.close on disconnect', () => {
    const store = makeStore();
    const proxy = new CrdtStateProxy(store);
    const ws = makeWebSocket();
    const manager = new WebSocketManager(store, proxy, ws);

    manager.disconnect();

    expect(ws.close).toHaveBeenCalledTimes(1);
  });

  test('disconnect is safe to call multiple times', () => {
    const store = makeStore();
    const proxy = new CrdtStateProxy(store);
    const ws = makeWebSocket();
    const manager = new WebSocketManager(store, proxy, ws);

    manager.disconnect();
    manager.disconnect();

    expect(ws.close).toHaveBeenCalledTimes(2); // close() itself may be called twice
  });
});

// ── Offline buffering ─────────────────────────────────────────────────────────

describe('WebSocketManager – offline buffering', () => {
  beforeEach(() => jest.useFakeTimers());
  afterEach(() => jest.useRealTimers());

  test('buffers envelopes written while the socket is closed and flushes on reconnect+snapshot', () => {
    const store = makeStore();
    const proxy = new CrdtStateProxy(store);
    const ws = makeWebSocket();
    new WebSocketManager(store, proxy, ws);

    ws.simulateOpen();
    ws.simulateSnapshot([]); // first connection snapshot
    ws.simulateClose();

    // Writes while offline should be buffered, not sent immediately.
    (proxy.state as Record<string, unknown>).x = 1;
    (proxy.state as Record<string, unknown>).y = 2;
    jest.runAllTimers(); // batch flush moves envelopes to offline queue
    expect(ws.send).not.toHaveBeenCalled();

    // Reconnect: offline queue flushed only after snapshot arrives.
    ws.readyState = 1;
    ws.simulateOpen();
    expect(ws.send).not.toHaveBeenCalled(); // still waiting for snapshot

    ws.simulateSnapshot([]); // snapshot received → flush offline queue
    expect(ws.send).toHaveBeenCalledTimes(1);
    const envelopes = decodeSentBatch(ws.send.mock.calls[0][0]);
    expect(envelopes).toHaveLength(2);
    expect(envelopes[0]).toMatchObject({ op: { key: 'x' } });
    expect(envelopes[1]).toMatchObject({ op: { key: 'y' } });
  });

  test('buffers envelopes written before the first connection opens', () => {
    const store = makeStore();
    const proxy = new CrdtStateProxy(store);
    const ws = makeWebSocket(); // readyState = 0 (CONNECTING)
    new WebSocketManager(store, proxy, ws);

    (proxy.state as Record<string, unknown>).a = 42;
    expect(ws.send).not.toHaveBeenCalled();

    ws.simulateOpen();
    expect(ws.send).not.toHaveBeenCalled(); // waiting for snapshot

    ws.simulateSnapshot([]); // snapshot received → flush offline queue
    expect(ws.send).toHaveBeenCalledTimes(1);
    const envelopes = decodeSentBatch(ws.send.mock.calls[0][0]);
    expect(envelopes).toHaveLength(1);
    expect(envelopes[0]).toMatchObject({ op: { key: 'a' } });
  });

  test('buffers envelopes written after a WebSocket error and flushes on reconnect+snapshot', () => {
    const store = makeStore();
    const proxy = new CrdtStateProxy(store);
    const ws = makeWebSocket();
    new WebSocketManager(store, proxy, ws);

    ws.simulateOpen();
    ws.simulateSnapshot([]); // first connection snapshot (no pending writes yet)
    ws.simulateError(); // sets readyState = 3

    (proxy.state as Record<string, unknown>).z = 99;
    jest.runAllTimers(); // batch flush moves envelope to offline queue
    expect(ws.send).not.toHaveBeenCalled();

    ws.readyState = 1;
    ws.simulateOpen(); // reconnect, waiting for snapshot
    ws.simulateSnapshot([]); // snapshot received → flush offline queue
    expect(ws.send).toHaveBeenCalledTimes(1);
    const envelopes = decodeSentBatch(ws.send.mock.calls[0][0]);
    expect(envelopes).toHaveLength(1);
    expect(envelopes[0]).toMatchObject({ op: { key: 'z' } });
  });

  test('discards the offline buffer when disconnect is called', () => {
    const store = makeStore();
    const proxy = new CrdtStateProxy(store);
    const ws = makeWebSocket();
    const manager = new WebSocketManager(store, proxy, ws);

    ws.simulateOpen();
    ws.simulateSnapshot([]);
    ws.simulateClose();

    (proxy.state as Record<string, unknown>).x = 1;
    jest.runAllTimers(); // batch flush moves envelope to offline queue

    manager.disconnect();

    // Reconnecting after an explicit disconnect should not flush the old buffer.
    ws.readyState = 1;
    ws.simulateOpen();
    ws.simulateSnapshot([]);
    expect(ws.send).not.toHaveBeenCalled();
  });

  test('sends normally after reconnect when no envelopes were buffered', () => {
    const store = makeStore();
    const proxy = new CrdtStateProxy(store);
    const ws = makeWebSocket();
    new WebSocketManager(store, proxy, ws);

    ws.simulateOpen();
    ws.simulateSnapshot([]);
    ws.simulateClose();

    // No writes while offline.
    ws.readyState = 1;
    ws.simulateOpen();
    ws.simulateSnapshot([]);

    // Write after reconnect is batched and sent on next tick.
    (proxy.state as Record<string, unknown>).n = 5;
    jest.runAllTimers();
    expect(ws.send).toHaveBeenCalledTimes(1);
    const envelopes = decodeSentBatch(ws.send.mock.calls[0][0]);
    expect(envelopes).toHaveLength(1);
    expect(envelopes[0]).toMatchObject({ op: { key: 'n' } });
  });
});
