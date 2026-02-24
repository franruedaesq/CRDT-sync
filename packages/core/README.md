# @crdt-sync/core

The core, framework-agnostic TypeScript SDK for `crdt-sync`. 

It acts as a TypeScript proxy wrapper around the powerful, Rust-compiled WebAssembly (Wasm) `StateStore` engine. It handles initializing the Wasm binaries, orchestrating the state sync via CRDTs, and managing the WebSocket network connections under the hood.

## Installation

```bash
npm install @crdt-sync/core
```

**Note:** If you are using React, we highly recommend using the magic hook adapter instead:
```bash
npm install @crdt-sync/core @crdt-sync/react
```

## Features

- **Wasm-powered CRDT Engine**: Lightning fast, robust conflict resolution written in Rust.
- **JavaScript State Proxying**: Wraps your regular JS objects into observed CRDT proxies. Mutations (e.g., `state.x = 10`) are instantly and invisibly converted into CRDT operations and queued.
- **WebSocket Synchronization**: Comes with a built-in `WebSocketManager` to easily broadcast the encoded Wasm envelopes between your client and your backend server.

## Basic Usage (Vanilla JS/TS)

While using framework adapters (like React's) is recommended for DOM-based apps, you can use `@crdt-sync/core` directly in any plain JavaScript or Node.js environment:

```typescript
import { CrdtStateProxy, WebSocketManager } from '@crdt-sync/core';
// Import the Wasm initializer
import init, { WasmStateStore } from '@crdt-sync/core/pkg/web/crdt_sync.js';

async function main() {
    // 1. Initialize the Wasm Module
    await init();
    
    // 2. Create a unique ID for this client
    const clientId = 'client-' + Math.random().toString(36).substring(2, 11);
    const store = new WasmStateStore(clientId);
    
    // 3. Create the State Proxy
    const proxy = new CrdtStateProxy(store);
    
    // Set initial state
    proxy.state.robot = {
        speed: 0,
        active: false
    };

    // 4. Hook up to the network
    const ws = new WebSocket('wss://api.example.com/sync');
    const manager = new WebSocketManager(store, proxy, ws);
    
    // 5. To mutate the state and broadcast to peers, simply mutate the proxy!
    proxy.state.robot.speed = 42; 
    
    // Listen for incoming changes from the server
    proxy.onUpdate(() => {
        console.log("State updated remotely!", proxy.state);
    });
}
```

## License

MIT
