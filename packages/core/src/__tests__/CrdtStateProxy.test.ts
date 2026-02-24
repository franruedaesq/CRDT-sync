import { encode as msgpackEncode } from '@msgpack/msgpack';
import { CrdtStateProxy, WasmStateStore, UpdateEvent } from '../CrdtStateProxy';

// ── Mock WasmStateStore ───────────────────────────────────────────────────────

function makeStore(): jest.Mocked<WasmStateStore> {
  const store: Record<string, string> = {};
  return {
    set_register: jest.fn((key: string, value_json: string) => {
      store[key] = value_json;
      // Return a minimal fake envelope as MessagePack bytes.
      return msgpackEncode({ timestamp: 1, node_id: 'node-1', op: { kind: 'Register', key, op: { value: JSON.parse(value_json), timestamp: 1, node_id: 'node-1' } } });
    }),
    get_register: jest.fn((key: string) => store[key]),
    apply_envelope: jest.fn(),
  };
}

// ── set_register is called on property write ──────────────────────────────────

describe('CrdtStateProxy – Wasm call on property write', () => {
  test('writes a top-level property via set_register', () => {
    const store = makeStore();
    const proxy = new CrdtStateProxy(store);

    (proxy.state as Record<string, unknown>).speed = 100;

    expect(store.set_register).toHaveBeenCalledWith('speed', JSON.stringify(100));
  });

  test('writes a dot-path key via bracket notation', () => {
    const store = makeStore();
    const proxy = new CrdtStateProxy(store);

    (proxy.state as Record<string, unknown>)['robot.speed'] = 100;

    expect(store.set_register).toHaveBeenCalledWith('robot.speed', JSON.stringify(100));
  });

  test('writes a deeply nested dot-path key via bracket notation', () => {
    const store = makeStore();
    const proxy = new CrdtStateProxy(store);

    (proxy.state as Record<string, unknown>)['fleet.robot.x'] = 42;

    expect(store.set_register).toHaveBeenCalledWith('fleet.robot.x', JSON.stringify(42));
  });

  test('serialises string values as JSON', () => {
    const store = makeStore();
    const proxy = new CrdtStateProxy(store);

    (proxy.state as Record<string, unknown>).name = 'robot-1';

    expect(store.set_register).toHaveBeenCalledWith('name', JSON.stringify('robot-1'));
  });

  test('serialises boolean values as JSON', () => {
    const store = makeStore();
    const proxy = new CrdtStateProxy(store);

    (proxy.state as Record<string, unknown>).active = true;

    expect(store.set_register).toHaveBeenCalledWith('active', JSON.stringify(true));
  });

  test('serialises object values as JSON', () => {
    const store = makeStore();
    const proxy = new CrdtStateProxy(store);

    (proxy.state as Record<string, unknown>).config = { retry: 3 };

    expect(store.set_register).toHaveBeenCalledWith('config', JSON.stringify({ retry: 3 }));
  });
});

// ── get trap ──────────────────────────────────────────────────────────────────

describe('CrdtStateProxy – get trap', () => {
  test('returns undefined for Symbol keys without crashing', () => {
    const store = makeStore();
    const proxy = new CrdtStateProxy(store);

    expect(() => {
      const _ = (proxy.state as any)[Symbol.toPrimitive];
    }).not.toThrow();
    expect((proxy.state as any)[Symbol.toPrimitive]).toBeUndefined();
  });

  test('returns undefined for an unregistered key', () => {
    const store = makeStore();
    const proxy = new CrdtStateProxy(store);

    expect((proxy.state as Record<string, unknown>).notYetSet).toBeUndefined();
  });

  test('reads a previously written top-level value from the store', () => {
    const store = makeStore();
    const proxy = new CrdtStateProxy(store);

    (proxy.state as Record<string, unknown>).speed = 42;

    expect((proxy.state as Record<string, unknown>).speed).toBe(42);
  });

  test('reads a dot-path value written via bracket notation', () => {
    const store = makeStore();
    const proxy = new CrdtStateProxy(store);

    (proxy.state as Record<string, unknown>)['robot.speed'] = 99;

    expect((proxy.state as Record<string, unknown>)['robot.speed']).toBe(99);
  });

  test('reads an object value written as a whole', () => {
    const store = makeStore();
    const proxy = new CrdtStateProxy(store);

    (proxy.state as Record<string, unknown>).robot = { x: 10, y: 20 };

    expect((proxy.state as Record<string, unknown>).robot).toEqual({ x: 10, y: 20 });
  });
});

// ── onUpdate event emitter ────────────────────────────────────────────────────

describe('CrdtStateProxy – onUpdate event emitter', () => {
  test('fires onUpdate listener on property write', () => {
    const store = makeStore();
    const proxy = new CrdtStateProxy(store);
    const handler = jest.fn();

    proxy.onUpdate(handler);
    (proxy.state as Record<string, unknown>).speed = 100;

    expect(handler).toHaveBeenCalledTimes(1);
  });

  test('passes correct key and value to onUpdate listener', () => {
    const store = makeStore();
    const proxy = new CrdtStateProxy(store);
    const events: UpdateEvent[] = [];

    proxy.onUpdate((e) => events.push(e));
    (proxy.state as Record<string, unknown>).speed = 100;

    expect(events[0].key).toBe('speed');
    expect(events[0].value).toBe(100);
  });

  test('passes the envelope bytes from set_register to onUpdate', () => {
    const store = makeStore();
    const proxy = new CrdtStateProxy(store);
    const events: UpdateEvent[] = [];

    proxy.onUpdate((e) => events.push(e));
    (proxy.state as Record<string, unknown>).x = 42;

    const expectedEnvelope = store.set_register.mock.results[0].value as Uint8Array;
    expect(events[0].envelope).toEqual(expectedEnvelope);
  });

  test('fires onUpdate with dot-path key for bracket-notation writes', () => {
    const store = makeStore();
    const proxy = new CrdtStateProxy(store);
    const events: UpdateEvent[] = [];

    proxy.onUpdate((e) => events.push(e));
    (proxy.state as Record<string, unknown>)['robot.speed'] = 100;

    expect(events[0].key).toBe('robot.speed');
    expect(events[0].value).toBe(100);
  });

  test('fires multiple registered listeners', () => {
    const store = makeStore();
    const proxy = new CrdtStateProxy(store);
    const handler1 = jest.fn();
    const handler2 = jest.fn();

    proxy.onUpdate(handler1);
    proxy.onUpdate(handler2);
    (proxy.state as Record<string, unknown>).x = 1;

    expect(handler1).toHaveBeenCalledTimes(1);
    expect(handler2).toHaveBeenCalledTimes(1);
  });

  test('unsubscribe removes the listener', () => {
    const store = makeStore();
    const proxy = new CrdtStateProxy(store);
    const handler = jest.fn();

    const unsubscribe = proxy.onUpdate(handler);
    unsubscribe();
    (proxy.state as Record<string, unknown>).x = 1;

    expect(handler).not.toHaveBeenCalled();
  });

  test('unsubscribe only removes the specific listener', () => {
    const store = makeStore();
    const proxy = new CrdtStateProxy(store);
    const handler1 = jest.fn();
    const handler2 = jest.fn();

    const unsubscribe1 = proxy.onUpdate(handler1);
    proxy.onUpdate(handler2);
    unsubscribe1();
    (proxy.state as Record<string, unknown>).x = 1;

    expect(handler1).not.toHaveBeenCalled();
    expect(handler2).toHaveBeenCalledTimes(1);
  });

  test('does not fire listener after multiple unsubscribes', () => {
    const store = makeStore();
    const proxy = new CrdtStateProxy(store);
    const handler = jest.fn();

    const unsubscribe = proxy.onUpdate(handler);
    unsubscribe();
    unsubscribe(); // calling twice is safe
    (proxy.state as Record<string, unknown>).x = 1;

    expect(handler).not.toHaveBeenCalled();
  });

  test('fires listener for each individual write', () => {
    const store = makeStore();
    const proxy = new CrdtStateProxy(store);
    const handler = jest.fn();

    proxy.onUpdate(handler);
    (proxy.state as Record<string, unknown>).a = 1;
    (proxy.state as Record<string, unknown>).b = 2;
    (proxy.state as Record<string, unknown>).c = 3;

    expect(handler).toHaveBeenCalledTimes(3);
  });
});

// ── onChange event emitter ────────────────────────────────────────────────────

describe('CrdtStateProxy – onChange event emitter', () => {
  test('fires onChange on local write', () => {
    const store = makeStore();
    const proxy = new CrdtStateProxy(store);
    const handler = jest.fn();

    proxy.onChange(handler);
    (proxy.state as Record<string, unknown>).x = 1;

    expect(handler).toHaveBeenCalledTimes(1);
  });

  test('fires onChange after notifyRemoteUpdate', () => {
    const store = makeStore();
    const proxy = new CrdtStateProxy(store);
    const handler = jest.fn();

    proxy.onChange(handler);
    proxy.notifyRemoteUpdate();

    expect(handler).toHaveBeenCalledTimes(1);
  });

  test('does not fire onUpdate on notifyRemoteUpdate', () => {
    const store = makeStore();
    const proxy = new CrdtStateProxy(store);
    const updateHandler = jest.fn();
    const changeHandler = jest.fn();

    proxy.onUpdate(updateHandler);
    proxy.onChange(changeHandler);
    proxy.notifyRemoteUpdate();

    expect(updateHandler).not.toHaveBeenCalled();
    expect(changeHandler).toHaveBeenCalledTimes(1);
  });

  test('onChange unsubscribe works correctly', () => {
    const store = makeStore();
    const proxy = new CrdtStateProxy(store);
    const handler = jest.fn();

    const unsubscribe = proxy.onChange(handler);
    unsubscribe();
    (proxy.state as Record<string, unknown>).x = 1;
    proxy.notifyRemoteUpdate();

    expect(handler).not.toHaveBeenCalled();
  });
});

// ── state accessor ────────────────────────────────────────────────────────────

describe('CrdtStateProxy – state accessor', () => {
  test('state getter returns the same proxy object on repeated access', () => {
    const store = makeStore();
    const proxy = new CrdtStateProxy(store);

    expect(proxy.state).toBe(proxy.state);
  });
});
