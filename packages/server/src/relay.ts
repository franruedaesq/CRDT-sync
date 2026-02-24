#!/usr/bin/env node
/**
 * @crdt-sync/server — WebSocket relay for the crdt-sync CRDT protocol.
 *
 * The relay is intentionally minimal: it receives JSON arrays of CRDT envelopes
 * from one client and broadcasts them verbatim to every other connected client.
 * No server-side CRDT state is maintained — all merge logic lives inside the
 * Wasm StateStore running in each browser tab.
 *
 * Protocol:
 *   Client → Server: JSON string, value is an array of envelope JSON strings.
 *                    Example: ["<envelope1>", "<envelope2>"]
 *   Server → Client: Same JSON string, forwarded to all *other* connected clients.
 *
 * Usage (programmatic):
 *   import { createRelay } from '@crdt-sync/server';
 *   const relay = createRelay({ port: 8080 });
 *   relay.close(); // shut down the server
 *
 * Usage (CLI):
 *   npx crdt-sync-server --port 8080
 *   node dist/relay.js --port 8080
 */

import { WebSocketServer, WebSocket } from 'ws';
import { IncomingMessage } from 'http';

export interface RelayOptions {
    /** Port to listen on. Defaults to 8080. */
    port?: number;
    /** Host to bind to. Defaults to '0.0.0.0'. */
    host?: string;
    /** Optional callback fired once the server is listening. */
    onListening?: (port: number) => void;
}

export interface RelayServer {
    /** Closes the underlying WebSocketServer. */
    close(): Promise<void>;
    /** The underlying ws.Server instance. */
    wss: WebSocketServer;
}

/**
 * Creates and starts a crdt-sync relay server.
 *
 * @param options - Server configuration.
 * @returns A `RelayServer` handle with a `close()` method.
 *
 * @example
 * ```ts
 * import { createRelay } from '@crdt-sync/server';
 *
 * const relay = createRelay({ port: 8080 });
 * // relay.close() to shut down
 * ```
 */
export function createRelay(options: RelayOptions = {}): RelayServer {
    const { port = 8080, host = '0.0.0.0', onListening } = options;

    const wss = new WebSocketServer({ port, host });

    wss.on('listening', () => {
        const addr = wss.address();
        const resolvedPort = typeof addr === 'object' && addr ? addr.port : port;
        if (onListening) {
            onListening(resolvedPort);
        } else {
            console.log(`[crdt-sync] relay listening on ws://${host}:${resolvedPort}`);
        }
    });

    wss.on('connection', (socket: WebSocket, _req: IncomingMessage) => {
        socket.on('message', (raw: Buffer | string) => {
            const message = raw.toString();

            // Validate: must be a JSON array of envelope strings.
            try {
                const envelopes = JSON.parse(message);
                if (!Array.isArray(envelopes)) {
                    socket.send(JSON.stringify({ error: 'Expected a JSON array of envelopes.' }));
                    return;
                }
            } catch {
                socket.send(JSON.stringify({ error: 'Invalid JSON.' }));
                return;
            }

            // Broadcast to all other connected clients.
            wss.clients.forEach((client) => {
                if (client !== socket && client.readyState === WebSocket.OPEN) {
                    client.send(message);
                }
            });
        });

        socket.on('error', (err) => {
            console.error('[crdt-sync] client error:', err.message);
        });
    });

    return {
        wss,
        close(): Promise<void> {
            return new Promise((resolve, reject) => {
                wss.close((err) => (err ? reject(err) : resolve()));
            });
        },
    };
}

// ── CLI entry point ───────────────────────────────────────────────────────────

if (require.main === module) {
    const args = process.argv.slice(2);
    const portArg = args.indexOf('--port');
    const port = portArg !== -1 ? parseInt(args[portArg + 1], 10) : 8080;

    if (isNaN(port)) {
        console.error('[crdt-sync] Invalid port. Usage: crdt-sync-server --port <number>');
        process.exit(1);
    }

    createRelay({ port });
}
