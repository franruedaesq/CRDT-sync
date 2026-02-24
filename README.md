# CRDT-sync

[![Crates.io](https://img.shields.io/crates/v/crdt-sync.svg)](https://crates.io/crates/crdt-sync)
[![Docs.rs](https://docs.rs/crdt-sync/badge.svg)](https://docs.rs/crdt-sync)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

A **generic state synchronization engine** built in Rust, based on
**CRDTs (Conflict-free Replicated Data Types)**.

Designed to keep a UI (digital twin) and a backend (AI/robotics logic) in
**perfect harmony** — even when multiple agents write concurrently or the
network drops for a moment. No locks, no merge conflicts, no data loss.

---

## Features

| CRDT | Type | Use case |
|------|------|----------|
| [`LWWRegister`] | Last-Writer-Wins Register | Scalar key-value properties (`robot.x = 10`) |
| [`ORSet`] | Observed-Remove Set | Collections of unique items |
| [`RGA`] | Replicated Growable Array | Ordered sequences / lists |
| [`StateStore`] | Composite sync engine | Hosts all CRDTs under one roof with Lamport clocks and network `Envelope`s |
| [`StateProxy`] | State observation proxy | Intercepts field mutations and auto-queues CRDT operations (no manual `Envelope` handling) |
| [`crdt_state!`] | Typed state proxy macro | Generates a typed proxy struct from a field declaration; each field gets `set_<field>` / `get_<field>` methods with compile-time type safety |

### Logical Clocks

Physical wall-clock time (NTP) is unreliable for distributed synchronization.
This library provides two implementations that track causal ordering without
relying on system time:

| Clock | Module | Use case |
|-------|--------|----------|
| [`LamportClock`] | `lamport_clock` | Scalar logical clock; used internally by `StateStore` for total-order timestamps |
| [`VectorClock`] | `vector_clock` | Per-node vector clock; detects **concurrent** events in addition to causal ordering |

All CRDTs are **operation-based (CmRDT)** and satisfy:
- **Commutativity** – apply operations in any order, always converge.
- **Idempotency** – replaying an operation is safe.
- **Causal buffering** (RGA) – out-of-order delivery is handled automatically.

---

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
crdt-sync = "0.1"
```

---

## Quick Start

### LWW-Register (Last-Writer-Wins)

```rust
use crdt_sync::LWWRegister;

let mut node_a: LWWRegister<f64> = LWWRegister::new("node-A");
let mut node_b: LWWRegister<f64> = LWWRegister::new("node-B");

// node-A writes robot.x
let op = node_a.set_and_apply(10.0, 1);

// Broadcast op to node-B
node_b.apply(op);

assert_eq!(node_b.get(), Some(&10.0));
```

### OR-Set (Observed-Remove Set)

```rust
use crdt_sync::ORSet;

let mut node_a: ORSet<String> = ORSet::new("node-A");
let mut node_b: ORSet<String> = ORSet::new("node-B");

// Add a robot to the fleet
let op = node_a.add("robot-1".to_string());
node_a.apply(op.clone());
node_b.apply(op);

assert!(node_b.contains(&"robot-1".to_string()));
```

### RGA (Replicated Growable Array)

```rust
use crdt_sync::RGA;

let mut node_a: RGA<char> = RGA::new("node-A");
let mut node_b: RGA<char> = RGA::new("node-B");

// node-A builds a sequence
let op1 = node_a.insert(0, 'H');
let op2 = node_a.insert(1, 'i');

// node-B receives operations in reverse order — handled automatically
node_b.apply(op2);
node_b.apply(op1);

assert_eq!(node_a.to_vec(), node_b.to_vec()); // ['H', 'i']
```

### StateStore (Composite Sync Engine)

The `StateStore` is the recommended high-level API. It:
- Manages named LWW registers, OR-Sets and RGAs in one place.
- Assigns **Lamport timestamps** automatically (via the built-in `LamportClock`).
- Produces **`Envelope`** messages ready to send over a network channel.

```rust
use crdt_sync::StateStore;

let mut node_a = StateStore::new("node-A");
let mut node_b = StateStore::new("node-B");

// Write a scalar property
let env = node_a.set_register("robot.x", 42.0_f64);
node_b.apply_envelope(env);
assert_eq!(node_b.get_register::<f64>("robot.x"), Some(42.0));

// Add to a set
let env = node_a.set_add("fleet", "unit-1");
node_b.apply_envelope(env);
assert!(node_b.set_contains("fleet", &"unit-1"));

// Append to a sequence
let env1 = node_a.seq_insert("log", 0, "boot");
let env2 = node_a.seq_insert("log", 1, "ready");
node_b.apply_envelope(env2);
node_b.apply_envelope(env1); // out-of-order — still converges
assert_eq!(
    node_a.seq_items::<String>("log"),
    node_b.seq_items::<String>("log"),
);
```

### LamportClock

```rust
use crdt_sync::LamportClock;

let mut node_a = LamportClock::new();
let mut node_b = LamportClock::new();

// node_a produces and sends an event
let ts = node_a.tick(); // ts = 1

// node_b receives it, then produces its own event
node_b.update(ts);      // b advances to max(0, 1) + 1 = 2
let ts_b = node_b.tick(); // ts_b = 3

assert!(ts_b > ts);
```

### VectorClock

```rust
use crdt_sync::VectorClock;

let mut a = VectorClock::new("A");
let mut b = VectorClock::new("B");

let v_a = a.increment(); // A sends {A:1}
let v_b = b.increment(); // B sends {B:1} – concurrent with v_a

// Neither causally precedes the other
assert!(v_a.concurrent_with(&v_b));

// B receives A's event
b.update(&v_a);
let v_b2 = b.increment(); // {A:1, B:2} – causally after v_a

assert!(v_a.happened_before(&v_b2));
```

---

### StateProxy (State Observation)

`StateProxy` is the Rust equivalent of JavaScript Proxy-based state observation.
It wraps a `StateStore` and **intercepts every field mutation**, automatically
converting it into a CRDT operation and queuing it for broadcast.  Developers
work with plain `set` / `get` calls and never touch `Envelope` values directly.

```rust
use crdt_sync::state_store::StateStore;
use crdt_sync::proxy::StateProxy;

let mut store_a = StateStore::new("node-A");
let mut store_b = StateStore::new("node-B");

// Use the proxy – no manual Envelope handling required.
let ops = {
    let mut proxy = store_a.proxy();     // or StateProxy::new(&mut store_a)
    proxy
        .set("robot.x", 10.0_f64)       // scalar field
        .set("robot.y", 20.0_f64)
        .set_add("fleet", "unit-1")      // set field
        .seq_push("log", "boot");        // sequence field

    proxy.drain_pending()                // collect queued ops for broadcast
};

// Broadcast to all peers.
for env in ops {
    store_b.apply_envelope(env);
}

assert_eq!(store_b.get_register::<f64>("robot.x"), Some(10.0));
assert!(store_b.set_contains("fleet", &"unit-1"));
assert_eq!(store_b.seq_items::<String>("log"), vec!["boot"]);
```

---

### `crdt_state!` Macro (Typed State Proxy)

The `crdt_state!` macro generates a **typed proxy struct** from a plain struct-like
field declaration.  Instead of using string keys (`proxy.set("x", 10.0)`), you get
compile-time-checked `set_x` / `get_x` methods for every declared field.

```rust
use crdt_sync::state_store::StateStore;
use crdt_sync::crdt_state;

// Declare a typed state proxy struct.
crdt_state! {
    pub struct RobotState {
        x: f64,
        y: f64,
        speed: f64,
        name: String,
        active: bool,
    }
}

let mut store_a = StateStore::new("node-A");
let mut store_b = StateStore::new("node-B");

// Mutations are intercepted; no Envelope handling required.
let ops = {
    let mut state = RobotState::new(&mut store_a);
    state
        .set_x(3.0)
        .set_y(4.0)
        .set_speed(5.0)
        .set_name("unit-7".to_string())
        .set_active(true);

    state.drain_pending()   // collect queued ops for broadcast
};

// Broadcast to all peers.
for env in ops {
    store_b.apply_envelope(env);
}

assert_eq!(store_b.get_register::<f64>("x"),      Some(3.0));
assert_eq!(store_b.get_register::<f64>("speed"),  Some(5.0));
assert_eq!(store_b.get_register::<bool>("active"), Some(true));
```

The generated struct also exposes:
- `pending_count() -> usize` – number of ops queued for broadcast.
- `store() -> &StateStore` – read-only access to the underlying store.

---

```
crdt-sync
├── lamport_clock – Scalar Lamport logical clock (tick / update rules)
├── vector_clock  – Per-node vector clock (happened-before / concurrent detection)
├── lww_register  – LWW-Register CmRDT
├── or_set        – OR-Set CmRDT
├── rga           – RGA CmRDT (with causal buffering)
├── state_store   – Composite sync engine (LamportClock + Envelope)
├── proxy         – StateProxy: intercepts mutations, auto-queues CRDT ops
└── macros        – crdt_state! macro: typed proxy structs from field declarations
```

Each module is self-contained and can be used independently. The `StateStore`
type-erases values via `serde_json::Value` so heterogeneous data can be stored
without dynamic dispatch.

---

## TypeScript SDK & React Integration

`crdt-sync` provides a seamless TypeScript monorepo offering a framework-agnostic core (`@crdt-sync/core`) and framework-specific adapters (`@crdt-sync/react`), built on top of WebAssembly.

### Installation

```bash
npm install @crdt-sync/core @crdt-sync/react
```

### React usage (`useCrdtState`)

The React hook magically handles Wasm initialization, WebSocket network sync, CRDT proxying, and React component re-renders:

```tsx
import { useCrdtState } from '@crdt-sync/react';

export function RobotDashboard() {
  // Binds the CRDT Wasm engine and networking directly to React state
  const { state, proxy, status } = useCrdtState('wss://api.example.com/sync', {
    robot: { speed: 0, active: true }
  });

  if (status !== 'open') return <p>Connecting to sync engine...</p>;

  return (
    <div>
      <h1>Speed: {state.robot.speed}</h1>
      {/* 
        Direct mutation is intercepted, applied as a CRDT operation, 
        broadcast over WebSocket, and triggers a local React re-render.
      */}
      <button onClick={() => proxy!.state.robot.speed += 10}>
        Increase Speed
      </button>
    </div>
  );
}
```

---

## Running Tests

```bash
cargo test
```

---

## License

MIT
