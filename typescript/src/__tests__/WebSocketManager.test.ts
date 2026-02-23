import { CrdtStateProxy, WasmStateStore } from '../CrdtStateProxy';
import { WebSocketManager, WebSocketLike } from '../WebSocketManager';

// ── Helpers ───────────────────────────────────────────────────────────────────

function makeStore(): jest.Mocked<WasmStateStore> {
  const store: Record<string, string> = {};
  return {
    set_register: jest.fn((key: string, value_json: string) => {
      store[key] = value_json;
      return JSON.stringify({ timestamp: 1, node_id: 'node-1', op: { kind: 'Register', key, op: { value: JSON.parse(value_json), timestamp: 1, node_id: 'node-1' } } });
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
  /** Helper: simulate a message arriving from a peer. */
  simulateMessage(data: string): void;
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
    simulateMessage(data: string) {
      ws.onmessage?.({ data });
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

// ── Outbound: local writes are broadcast over WebSocket ───────────────────────

describe('WebSocketManager – outbound broadcast', () => {
  test('sends the envelope returned by set_register over WebSocket after open', () => {
    const store = makeStore();
    const proxy = new CrdtStateProxy(store);
    const ws = makeWebSocket();
    new WebSocketManager(store, proxy, ws);

    ws.simulateOpen();
    (proxy.state as Record<string, unknown>).x = 10;

    expect(ws.send).toHaveBeenCalledTimes(1);
    const sentPayload = ws.send.mock.calls[0][0] as string;
    const parsed = JSON.parse(sentPayload) as Record<string, unknown>;
    expect(parsed).toMatchObject({ op: { key: 'x' } });
  });

  test('does not send before the connection is open', () => {
    const store = makeStore();
    const proxy = new CrdtStateProxy(store);
    const ws = makeWebSocket(); // stays in CONNECTING state
    new WebSocketManager(store, proxy, ws);

    (proxy.state as Record<string, unknown>).x = 10; // no onopen yet

    expect(ws.send).not.toHaveBeenCalled();
  });

  test('sends one message per proxy write', () => {
    const store = makeStore();
    const proxy = new CrdtStateProxy(store);
    const ws = makeWebSocket();
    new WebSocketManager(store, proxy, ws);

    ws.simulateOpen();
    (proxy.state as Record<string, unknown>).a = 1;
    (proxy.state as Record<string, unknown>).b = 2;
    (proxy.state as Record<string, unknown>).c = 3;

    expect(ws.send).toHaveBeenCalledTimes(3);
  });

  test('sends the envelope for nested property writes', () => {
    const store = makeStore();
    const proxy = new CrdtStateProxy(store);
    const ws = makeWebSocket();
    new WebSocketManager(store, proxy, ws);

    ws.simulateOpen();
    (proxy.state as Record<string, Record<string, unknown>>).robot.speed = 99;

    const sentPayload = ws.send.mock.calls[0][0] as string;
    const parsed = JSON.parse(sentPayload) as Record<string, unknown>;
    expect(parsed).toMatchObject({ op: { key: 'robot.speed' } });
  });

  test('stops broadcasting after disconnect', () => {
    const store = makeStore();
    const proxy = new CrdtStateProxy(store);
    const ws = makeWebSocket();
    const manager = new WebSocketManager(store, proxy, ws);

    ws.simulateOpen();
    manager.disconnect();

    (proxy.state as Record<string, unknown>).x = 10;
    expect(ws.send).not.toHaveBeenCalled();
  });

  test('stops broadcasting after WebSocket closes', () => {
    const store = makeStore();
    const proxy = new CrdtStateProxy(store);
    const ws = makeWebSocket();
    new WebSocketManager(store, proxy, ws);

    ws.simulateOpen();
    ws.simulateClose();

    (proxy.state as Record<string, unknown>).x = 10;
    expect(ws.send).not.toHaveBeenCalled();
  });

  test('stops broadcasting after a WebSocket error', () => {
    const store = makeStore();
    const proxy = new CrdtStateProxy(store);
    const ws = makeWebSocket();
    new WebSocketManager(store, proxy, ws);

    ws.simulateOpen();
    ws.simulateError();

    (proxy.state as Record<string, unknown>).x = 10;
    expect(ws.send).not.toHaveBeenCalled();
  });
});

// ── Inbound: peer envelopes are applied to the local store ───────────────────

describe('WebSocketManager – inbound apply', () => {
  test('calls apply_envelope with the raw message data', () => {
    const store = makeStore();
    const proxy = new CrdtStateProxy(store);
    const ws = makeWebSocket();
    new WebSocketManager(store, proxy, ws);

    const envelope = JSON.stringify({ timestamp: 2, node_id: 'node-2', op: { kind: 'Register', key: 'y', op: { value: 7, timestamp: 2, node_id: 'node-2' } } });
    ws.simulateMessage(envelope);

    expect(store.apply_envelope).toHaveBeenCalledWith(envelope);
  });

  test('applies multiple incoming envelopes in order', () => {
    const store = makeStore();
    const proxy = new CrdtStateProxy(store);
    const ws = makeWebSocket();
    new WebSocketManager(store, proxy, ws);

    ws.simulateMessage('env-1');
    ws.simulateMessage('env-2');

    expect(store.apply_envelope).toHaveBeenNthCalledWith(1, 'env-1');
    expect(store.apply_envelope).toHaveBeenNthCalledWith(2, 'env-2');
  });

  test('applies incoming envelopes regardless of connection open state', () => {
    const store = makeStore();
    const proxy = new CrdtStateProxy(store);
    const ws = makeWebSocket(); // no simulateOpen
    new WebSocketManager(store, proxy, ws);

    ws.simulateMessage('any-envelope');

    expect(store.apply_envelope).toHaveBeenCalledWith('any-envelope');
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
  test('buffers envelopes written while the socket is closed and flushes on reconnect', () => {
    const store = makeStore();
    const proxy = new CrdtStateProxy(store);
    const ws = makeWebSocket();
    new WebSocketManager(store, proxy, ws);

    ws.simulateOpen();
    ws.simulateClose();

    // Writes while offline should be buffered, not sent immediately.
    (proxy.state as Record<string, unknown>).x = 1;
    (proxy.state as Record<string, unknown>).y = 2;
    expect(ws.send).not.toHaveBeenCalled();

    // Reconnect: all buffered envelopes must be flushed in order.
    ws.readyState = 1;
    ws.simulateOpen();
    expect(ws.send).toHaveBeenCalledTimes(2);
    const first = JSON.parse(ws.send.mock.calls[0][0] as string) as Record<string, unknown>;
    const second = JSON.parse(ws.send.mock.calls[1][0] as string) as Record<string, unknown>;
    expect(first).toMatchObject({ op: { key: 'x' } });
    expect(second).toMatchObject({ op: { key: 'y' } });
  });

  test('buffers envelopes written before the first connection opens', () => {
    const store = makeStore();
    const proxy = new CrdtStateProxy(store);
    const ws = makeWebSocket(); // readyState = 0 (CONNECTING)
    new WebSocketManager(store, proxy, ws);

    (proxy.state as Record<string, unknown>).a = 42;
    expect(ws.send).not.toHaveBeenCalled();

    ws.simulateOpen();
    expect(ws.send).toHaveBeenCalledTimes(1);
    const sent = JSON.parse(ws.send.mock.calls[0][0] as string) as Record<string, unknown>;
    expect(sent).toMatchObject({ op: { key: 'a' } });
  });

  test('buffers envelopes written after a WebSocket error and flushes on reconnect', () => {
    const store = makeStore();
    const proxy = new CrdtStateProxy(store);
    const ws = makeWebSocket();
    new WebSocketManager(store, proxy, ws);

    ws.simulateOpen();
    ws.simulateError(); // sets readyState = 3

    (proxy.state as Record<string, unknown>).z = 99;
    expect(ws.send).not.toHaveBeenCalled();

    ws.readyState = 1;
    ws.simulateOpen();
    expect(ws.send).toHaveBeenCalledTimes(1);
    const sent = JSON.parse(ws.send.mock.calls[0][0] as string) as Record<string, unknown>;
    expect(sent).toMatchObject({ op: { key: 'z' } });
  });

  test('discards the offline buffer when disconnect is called', () => {
    const store = makeStore();
    const proxy = new CrdtStateProxy(store);
    const ws = makeWebSocket();
    const manager = new WebSocketManager(store, proxy, ws);

    ws.simulateOpen();
    ws.simulateClose();

    (proxy.state as Record<string, unknown>).x = 1;

    manager.disconnect();

    // Reconnecting after an explicit disconnect should not flush the old buffer.
    ws.readyState = 1;
    ws.simulateOpen();
    expect(ws.send).not.toHaveBeenCalled();
  });

  test('sends normally after reconnect when no envelopes were buffered', () => {
    const store = makeStore();
    const proxy = new CrdtStateProxy(store);
    const ws = makeWebSocket();
    new WebSocketManager(store, proxy, ws);

    ws.simulateOpen();
    ws.simulateClose();

    // No writes while offline.
    ws.readyState = 1;
    ws.simulateOpen();

    // Write after reconnect is sent directly.
    (proxy.state as Record<string, unknown>).n = 5;
    expect(ws.send).toHaveBeenCalledTimes(1);
    const sent = JSON.parse(ws.send.mock.calls[0][0] as string) as Record<string, unknown>;
    expect(sent).toMatchObject({ op: { key: 'n' } });
  });
});
