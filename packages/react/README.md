# @crdt-sync/react

A high-level React adapter for `crdt-sync`. This package exposes the `useCrdtState` hook, which allows you to bind CRDT-powered, collaborative state directly to your React components.

## Installation

You need both the framework-agnostic core and the React adapter. The Wasm binaries are bundled within the `@crdt-sync/core` package.

```bash
npm install @crdt-sync/core @crdt-sync/react
```

## Quick Start: `useCrdtState`

The `useCrdtState` hook magically handles:
- **Wasm Initialization**: Bootstraps the rust-generated `crdt-sync` bindings automatically.
- **Networking**: Connects via WebSocket out-of-the-box.
- **State Proxying**: Wraps your state with JavaScript proxies that automatically broadcast operations seamlessly upon assignment.
- **Reactivity**: Automatically hooks up to React's lifecycle to re-render when remote changes are received.

### Example

Here's how to create a simple connected component:

```tsx
import { useCrdtState } from '@crdt-sync/react';

// Define the shape of your state
type MyState = {
  robot: {
    speed: number;
    active: boolean;
  };
};

export function RobotDashboard() {
  // Bind the Wasm engine and network directly to React state
  const { state, proxy, status } = useCrdtState<MyState>(
    'wss://api.example.com/sync',
    {
       robot: { speed: 0, active: true } // Initial State
    }
  );

  if (status === 'connecting') {
    return <p>Connecting to sync engine...</p>;
  }

  if (status === 'error') {
    return <p>Failed to connect to the sync server!</p>;
  }

  return (
    <div>
      <h1>Robot Speed: {state.robot.speed}</h1>
      {/* 
        Direct mutation is intercepted, applied as a CRDT operation, 
        broadcast over WebSocket, and triggers a local React re-render.
      */}
      <button 
        onClick={() => {
          // Mutate the state using the returned proxy wrapper
          if (proxy) {
            proxy.state.robot.speed += 10;
          }
        }}
      >
        Increase Speed
      </button>

      <button
        onClick={() => {
          if (proxy) {
            proxy.state.robot.active = !proxy.state.robot.active;
          }
        }}
      >
        Toggle Active Status ({state.robot.active ? 'Active' : 'Idle'})
      </button>
    </div>
  );
}
```

## API Reference

### `useCrdtState<T>(url: string, initialState: T)`

Accepts a TypeScript generic `T` that extends `Record<string, unknown>`.

#### Parameters:
- `url` (`string`): The WebSocket URL to connect to the backend sync service.
- `initialState` (`T`): The default state to build the proxy structure over. 

#### Returns (`UseCrdtStateResult<T>`):
- `state`: The plain Javascript object representation of your state. You should use `state` for reading values when rendering.
- `proxy`: The `CrdtStateProxy` object. You **must** perform all mutations on `proxy.state`. Mutating `state` directly will not trigger network events.
- `status`: Connective status indicator (`'connecting'` | `'open'` | `'error'`).
