# @crdt-sync/server

A zero-dependency\* WebSocket relay server for the `crdt-sync` CRDT library.

> \*The only runtime dependency is the [`ws`](https://github.com/websockets/ws) library.

## How it works

The relay is intentionally simple — it maintains **no server-side state**. All CRDT merge logic runs inside the Wasm `StateStore` in each browser tab.

```
Client A ──► Relay ──broadcast──► Client B
                   └──broadcast──► Client C
```

When Client A writes to its CRDT store, the resulting **envelope** (a compact operation log entry) is sent to the relay as a JSON array. The relay broadcasts that array to every other connected client, which then applies the envelopes to their local Wasm stores.

## Installation

```bash
npm install @crdt-sync/server
```

## Usage

### CLI

```bash
# Start on the default port (8080)
npx crdt-sync-server

# Use a custom port
npx crdt-sync-server --port 3001
```

### Programmatic

```ts
import { createRelay } from '@crdt-sync/server';

const relay = createRelay({
  port: 8080,
  host: '0.0.0.0',         // optional, default '0.0.0.0'
  onListening: (port) => {  // optional callback
    console.log(`Relay ready on port ${port}`);
  },
});

// Shut down gracefully
await relay.close();
```

## Protocol

| Direction       | Format                                   | Description                                       |
|-----------------|------------------------------------------|---------------------------------------------------|
| Client → Server | `JSON string of string[]`                | Array of CRDT envelope JSON strings to broadcast  |
| Server → Client | Same JSON string (forwarded as-is)       | Sent to all clients **except** the sender         |

### Example message

```json
["{\"type\":\"lww\",\"key\":\"speed\",\"value\":100,\"ts\":1708801200000,\"nodeId\":\"client-abc123\"}"]
```

## Self-hosting / Custom relay

You don't need this package to run the relay. Any WebSocket server that implements the protocol table above will work. Here's a minimal Node.js example using the `ws` library directly:

```ts
import { WebSocketServer, WebSocket } from 'ws';

const wss = new WebSocketServer({ port: 8080 });

wss.on('connection', (socket) => {
  socket.on('message', (raw) => {
    // Broadcast to all other clients
    wss.clients.forEach((client) => {
      if (client !== socket && client.readyState === WebSocket.OPEN) {
        client.send(raw.toString());
      }
    });
  });
});
```
