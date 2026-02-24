#!/usr/bin/env node
/**
 * @crdt-sync/server — Stateful WebSocket relay for the crdt-sync CRDT protocol.
 *
 * The relay maintains per-room server-side state (as an ordered log of received
 * envelopes) so that late-joining clients can catch up with a full snapshot.
 * Clients are isolated by room: mutations in Room A never reach Room B.
 *
 * Protocol (server → client):
 *   { type: 'SNAPSHOT', data: string }  — JSON array of all envelopes in the room,
 *                                          sent once to every new connection.
 *   { type: 'UPDATE',   data: string }  — JSON array of envelopes from one client,
 *                                          broadcast to every *other* client in the room.
 *
 * Protocol (client → server):
 *   JSON string whose value is an array of envelope JSON strings.
 *   Example: ["<envelope1>", "<envelope2>"]
 *
 * Routing:
 *   Clients connect to ws://host/rooms/<roomId>. Connections to any other path
 *   are placed in the implicit "default" room.
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

// ── Protocol types ────────────────────────────────────────────────────────────

/**
 * Messages sent from the server to clients.
 *
 * - `SNAPSHOT` carries a JSON-serialised array of **all** envelope strings
 *   accumulated in the room — delivered once to every newly connected client.
 * - `UPDATE` carries the raw batch payload forwarded from a peer client.
 */
export type ServerMessage =
    | { type: 'SNAPSHOT'; data: string }
    | { type: 'UPDATE'; data: string };

// ── Persistence adapter ───────────────────────────────────────────────────────

/**
 * Storage-agnostic interface for persisting room state.
 *
 * Implement this interface to save envelope logs to any durable store
 * (local file, Redis, S3, database, …) and pass it to `createRelay` via
 * `options.persistence`.
 *
 * @example
 * ```ts
 * // Simple file-based implementation
 * import { readFile, writeFile } from 'fs/promises';
 *
 * const filePersistence: PersistenceAdapter = {
 *   async load(roomId) {
 *     try {
 *       const raw = await readFile(`./state-${roomId}.json`, 'utf8');
 *       return JSON.parse(raw);
 *     } catch { return null; }
 *   },
 *   async save(roomId, envelopes) {
 *     await writeFile(`./state-${roomId}.json`, JSON.stringify(envelopes));
 *   },
 * };
 * ```
 */
export interface PersistenceAdapter {
    /**
     * Load the saved envelope log for a room.
     * Return `null` (or a rejected promise) if no state is saved yet.
     */
    load(roomId: string): Promise<string[] | null>;
    /**
     * Persist the current envelope log for a room.
     * Called after every batch of incoming envelopes.
     */
    save(roomId: string, envelopes: string[]): Promise<void>;
}

// ── Room ──────────────────────────────────────────────────────────────────────

/** Internal room state. */
interface Room {
    /** All envelopes received in this room, in arrival order. */
    envelopes: string[];
    /** Connected WebSocket clients currently in this room. */
    clients: Set<WebSocket>;
}

// ── RelayOptions / RelayServer ────────────────────────────────────────────────

export interface RelayOptions {
    /** Port to listen on. Defaults to 8080. */
    port?: number;
    /** Host to bind to. Defaults to '0.0.0.0'. */
    host?: string;
    /** Optional callback fired once the server is listening. */
    onListening?: (port: number) => void;
    /**
     * Optional persistence adapter.  When provided, room state is loaded on
     * first access and saved after every incoming batch.
     */
    persistence?: PersistenceAdapter;
}

export interface RelayServer {
    /** Closes the underlying WebSocketServer. */
    close(): Promise<void>;
    /** The underlying ws.Server instance. */
    wss: WebSocketServer;
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/**
 * Extract the room identifier from a WebSocket request URL.
 *
 * Paths of the form `/rooms/<id>` return `<id>`.
 * All other paths (including `/`) return `"default"`.
 */
function getRoomId(url: string | undefined): string {
    if (!url) return 'default';
    const match = /^\/rooms\/([^/?#]+)/.exec(url);
    return match ? match[1] : 'default';
}

// ── createRelay ───────────────────────────────────────────────────────────────

/**
 * Creates and starts a crdt-sync relay server with room support and snapshot delivery.
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
    const { port = 8080, host = '0.0.0.0', onListening, persistence } = options;

    /** Live room map — created on first join, deleted when last client leaves. */
    const rooms = new Map<string, Room>();

    /** Return an existing room or create a new one (loading persisted state). */
    function getOrCreateRoom(roomId: string): Room {
        let room = rooms.get(roomId);
        if (!room) {
            room = { envelopes: [], clients: new Set() };
            rooms.set(roomId, room);
        }
        return room;
    }

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

    wss.on('connection', async (socket: WebSocket, req: IncomingMessage) => {
        const roomId = getRoomId(req.url);
        const room = getOrCreateRoom(roomId);

        // Load persisted state into a fresh room (one-time, on first client).
        if (persistence && room.clients.size === 0 && room.envelopes.length === 0) {
            try {
                const saved = await persistence.load(roomId);
                if (saved) room.envelopes.push(...saved);
            } catch {
                // Persistence errors are non-fatal; proceed with empty state.
            }
        }

        room.clients.add(socket);

        // ── Snapshot delivery ─────────────────────────────────────────────────
        // Send the full accumulated state to the new joiner so it can catch up.
        const snapshot: ServerMessage = {
            type: 'SNAPSHOT',
            data: JSON.stringify(room.envelopes),
        };
        socket.send(JSON.stringify(snapshot));

        // ── Inbound messages ──────────────────────────────────────────────────
        socket.on('message', (raw: Buffer | string) => {
            const message = raw.toString();

            // Validate: must be a JSON array of envelope strings.
            let envelopes: string[];
            try {
                const parsed = JSON.parse(message);
                if (!Array.isArray(parsed)) {
                    socket.send(JSON.stringify({ error: 'Expected a JSON array of envelopes.' }));
                    return;
                }
                envelopes = parsed as string[];
            } catch {
                socket.send(JSON.stringify({ error: 'Invalid JSON.' }));
                return;
            }

            // Consolidate: append envelopes to the room's master log.
            room.envelopes.push(...envelopes);

            // Persist the updated log (fire-and-forget; errors are non-fatal).
            if (persistence) {
                persistence.save(roomId, room.envelopes).catch((err: unknown) => {
                    console.error('[crdt-sync] persistence save error:', err);
                });
            }

            // Broadcast as UPDATE to all other clients in the same room.
            const update: ServerMessage = { type: 'UPDATE', data: message };
            const updateStr = JSON.stringify(update);
            room.clients.forEach((client) => {
                if (client !== socket && client.readyState === WebSocket.OPEN) {
                    client.send(updateStr);
                }
            });
        });

        // ── Cleanup ───────────────────────────────────────────────────────────
        socket.on('close', () => {
            room.clients.delete(socket);
            // Hibernate the room when the last client leaves.
            if (room.clients.size === 0) {
                rooms.delete(roomId);
            }
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
