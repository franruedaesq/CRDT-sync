#!/usr/bin/env node
/**
 * @crdt-sync/server — Stateful WebSocket relay for the crdt-sync CRDT protocol.
 *
 * The relay maintains per-room server-side state (as an ordered log of received
 * envelope blobs) so that late-joining clients can catch up with a full snapshot.
 * Clients are isolated by room: mutations in Room A never reach Room B.
 *
 * Protocol (server → client) — all frames are **binary MessagePack**:
 *   { type: 'SNAPSHOT', data: Uint8Array[] }  — array of envelope blobs sent once
 *                                               to every new connection.
 *   { type: 'UPDATE',   data: Uint8Array[] }  — batch of envelope blobs from one
 *                                               client, broadcast to every *other*
 *                                               client in the room.
 *   { type: 'PRUNE',    data: number }        — Lamport timestamp; clients should
 *                                               call prune_tombstones(data) to erase
 *                                               any tombstones created at or before
 *                                               this timestamp.
 *
 * Protocol (client → server) — binary MessagePack:
 *   A MessagePack-encoded array of envelope blobs (Uint8Array[]).
 *
 * Vector clock tracking:
 *   The server decodes each incoming envelope to extract its Lamport timestamp and
 *   tracks the maximum timestamp received per connected client.  When the minimum
 *   across all clients advances past a tombstone's creation timestamp, the server
 *   broadcasts a PRUNE message so every replica can reclaim the dead memory.
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

import { encode as msgpackEncode, decode as msgpackDecode } from '@msgpack/msgpack';
import { WebSocketServer, WebSocket } from 'ws';
import { IncomingMessage } from 'http';

// ── Protocol types ────────────────────────────────────────────────────────────

/**
 * Messages sent from the server to clients (before MessagePack encoding).
 *
 * - `SNAPSHOT` carries an array of **all** envelope blobs accumulated in the
 *   room — delivered once to every newly connected client.
 * - `UPDATE` carries the batch of envelope blobs forwarded from a peer client.
 * - `PRUNE` tells clients to physically erase tombstones created at or before
 *   the given Lamport timestamp.
 */
export type ServerMessage =
    | { type: 'SNAPSHOT'; data: Uint8Array[] }
    | { type: 'UPDATE'; data: Uint8Array[] }
    | { type: 'PRUNE'; data: number };

// ── Persistence adapter ───────────────────────────────────────────────────────

/**
 * Storage-agnostic interface for persisting room state.
 *
 * Envelopes are stored as binary blobs (`Uint8Array`).
 *
 * @example
 * ```ts
 * // Simple file-based implementation
 * import { readFile, writeFile } from 'fs/promises';
 *
 * const filePersistence: PersistenceAdapter = {
 *   async load(roomId) {
 *     try {
 *       const raw = await readFile(`./state-${roomId}.bin`);
 *       return msgpackDecode(raw) as Uint8Array[];
 *     } catch { return null; }
 *   },
 *   async save(roomId, envelopes) {
 *     await writeFile(`./state-${roomId}.bin`, msgpackEncode(envelopes));
 *   },
 * };
 * ```
 */
export interface PersistenceAdapter {
    /**
     * Load the saved envelope log for a room.
     * Return `null` (or a rejected promise) if no state is saved yet.
     */
    load(roomId: string): Promise<Uint8Array[] | null>;
    /**
     * Persist the current envelope log for a room.
     * Called after every batch of incoming envelopes.
     */
    save(roomId: string, envelopes: Uint8Array[]): Promise<void>;
}

// ── Room ──────────────────────────────────────────────────────────────────────

/** Metadata about a pending tombstone that may be pruned. */
interface TombstoneEntry {
    /** Lamport timestamp of the delete/remove envelope. */
    ts: number;
    /** Which CRDT the tombstone belongs to ('Sequence' or 'Set'). */
    kind: 'Sequence' | 'Set';
    /** The CRDT key (register/set/sequence name). */
    key: string;
}

/** Internal room state. */
interface Room {
    /** All envelope blobs received in this room, in arrival order. */
    envelopes: Uint8Array[];
    /** Connected WebSocket clients currently in this room. */
    clients: Set<WebSocket>;
    /**
     * In-flight persistence load promise, set while state is being loaded
     * from the persistence adapter.  Subsequent joiners await the same
     * promise rather than issuing a second concurrent load.
     */
    loading: Promise<void> | null;
    /**
     * Per-client maximum Lamport timestamp seen (from incoming envelopes).
     * Tracks the "VectorClock" of what each client has written / acknowledged.
     * New clients are seeded with the room's current high-watermark so that
     * they are considered "up to date" immediately upon receiving the snapshot.
     */
    clientClocks: Map<WebSocket, number>;
    /**
     * Maximum Lamport timestamp across all envelopes ever received in this room.
     * Used to seed the initial clock of newly joining clients.
     */
    highWatermark: number;
    /**
     * Delete / remove operations received in this room, pending physical pruning.
     * Entries are removed once a PRUNE has been broadcast for their timestamp.
     */
    tombstones: TombstoneEntry[];
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

/**
 * Broadcast a PRUNE message to all connected clients in a room when the
 * server determines that the minimum clock across all clients has advanced
 * past one or more tombstone timestamps.
 *
 * Tombstones up to (and including) the pruned timestamp are removed from
 * the room's tracking list after broadcasting.
 */
function tryPrune(room: Room): void {
    if (room.tombstones.length === 0 || room.clients.size === 0) return;

    // Only prune when every connected client has a known clock entry.
    // New clients are seeded at room.highWatermark so this is satisfied
    // immediately after the snapshot is sent.
    if (room.clientClocks.size < room.clients.size) return;

    const minClock = Math.min(...Array.from(room.clientClocks.values()));
    if (!Number.isFinite(minClock) || minClock <= 0) return;

    // Tombstones strictly older than minClock can be pruned: every client
    // has already sent an envelope with a timestamp >= minClock, meaning
    // their local store has processed everything up to minClock - 1.
    const pruneable = room.tombstones.filter((t) => t.ts < minClock);
    if (pruneable.length === 0) return;

    const maxPruneTs = Math.max(...pruneable.map((t) => t.ts));

    const pruneMsg: ServerMessage = { type: 'PRUNE', data: maxPruneTs };
    const pruneBytes = msgpackEncode(pruneMsg);
    room.clients.forEach((client) => {
        if (client.readyState === WebSocket.OPEN) {
            client.send(pruneBytes);
        }
    });

    room.tombstones = room.tombstones.filter((t) => t.ts > maxPruneTs);
}

// ── createRelay ───────────────────────────────────────────────────────────────

/**
 * Creates and starts a crdt-sync relay server with room support, snapshot
 * delivery, per-client vector clock tracking, and tombstone pruning.
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
            room = {
                envelopes: [],
                clients: new Set(),
                loading: null,
                clientClocks: new Map(),
                highWatermark: 0,
                tombstones: [],
            };
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

        // Load persisted state into a fresh room exactly once, even when
        // multiple clients connect concurrently before the load completes.
        if (persistence && room.envelopes.length === 0 && room.loading === null) {
            room.loading = (async () => {
                try {
                    const saved = await persistence.load(roomId);
                    if (saved) {
                        room.envelopes.push(...saved);
                        // Rebuild highWatermark from persisted envelopes.
                        for (const blob of saved) {
                            updateHighWatermark(room, blob);
                        }
                    }
                } catch {
                    // Persistence errors are non-fatal; proceed with empty state.
                } finally {
                    room.loading = null;
                }
            })();
        }
        // If a load is already in progress, wait for it to complete before
        // sending the snapshot so the new client receives the full state.
        if (room.loading !== null) {
            await room.loading;
        }

        room.clients.add(socket);
        // Seed the new client's clock at the room high-watermark so it is
        // considered "up to date" from the moment it receives the snapshot.
        room.clientClocks.set(socket, room.highWatermark);

        // ── Snapshot delivery ─────────────────────────────────────────────────
        // Send the full accumulated state to the new joiner so it can catch up.
        const snapshot: ServerMessage = {
            type: 'SNAPSHOT',
            data: room.envelopes,
        };
        socket.send(msgpackEncode(snapshot));

        // ── Inbound messages ──────────────────────────────────────────────────
        socket.on('message', (raw: Buffer | string) => {
            // Only accept binary frames.
            if (typeof raw === 'string') {
                socket.send(msgpackEncode({ error: 'Binary frames required.' }));
                return;
            }

            // Decode the outer MessagePack frame: should be a Uint8Array[].
            let envelopeBlobs: Uint8Array[];
            try {
                // In Node.js, the ws library delivers messages as Buffer (extends Uint8Array).
                const decoded = msgpackDecode(raw as Uint8Array);
                if (!Array.isArray(decoded)) {
                    socket.send(msgpackEncode({ error: 'Expected a MessagePack array of envelope blobs.' }));
                    return;
                }
                envelopeBlobs = decoded as Uint8Array[];
            } catch {
                socket.send(msgpackEncode({ error: 'Invalid MessagePack.' }));
                return;
            }

            // Process each envelope blob.
            let maxTsInBatch = 0;
            for (const blob of envelopeBlobs) {
                if (!(blob instanceof Uint8Array)) continue;

                // Extract Lamport timestamp and op metadata for GC tracking.
                try {
                    const env = msgpackDecode(blob) as {
                        timestamp?: number;
                        op?: { kind?: string; key?: string; op?: unknown };
                    };
                    const ts = typeof env.timestamp === 'number' ? env.timestamp : 0;
                    if (ts > maxTsInBatch) maxTsInBatch = ts;

                    // Track tombstones for deferred pruning.
                    const kind = env.op?.kind;
                    const key = env.op?.key ?? '';
                    if (kind === 'Sequence') {
                        const inner = env.op?.op as Record<string, unknown> | null;
                        if (inner && 'Delete' in inner) {
                            room.tombstones.push({ ts, kind: 'Sequence', key });
                        }
                    } else if (kind === 'Set') {
                        const inner = env.op?.op as Record<string, unknown> | null;
                        if (inner && 'Remove' in inner) {
                            room.tombstones.push({ ts, kind: 'Set', key });
                        }
                    }
                } catch {
                    // Non-fatal: continue without GC metadata for this envelope.
                }
            }

            // Update this client's known clock.
            if (maxTsInBatch > 0) {
                const prev = room.clientClocks.get(socket) ?? 0;
                if (maxTsInBatch > prev) {
                    room.clientClocks.set(socket, maxTsInBatch);
                }
                if (maxTsInBatch > room.highWatermark) {
                    room.highWatermark = maxTsInBatch;
                }
            }

            // Consolidate: append envelopes to the room's master log.
            room.envelopes.push(...envelopeBlobs);

            // Persist the updated log (fire-and-forget; errors are non-fatal).
            if (persistence) {
                persistence.save(roomId, room.envelopes).catch((err: unknown) => {
                    console.error('[crdt-sync] persistence save error:', err);
                });
            }

            // Broadcast as UPDATE to all other clients in the same room.
            const update: ServerMessage = { type: 'UPDATE', data: envelopeBlobs };
            const updateBytes = msgpackEncode(update);
            room.clients.forEach((client) => {
                if (client !== socket && client.readyState === WebSocket.OPEN) {
                    client.send(updateBytes);
                }
            });

            // Check whether any tombstones can now be pruned.
            tryPrune(room);
        });

        // ── Cleanup ───────────────────────────────────────────────────────────
        socket.on('close', () => {
            room.clients.delete(socket);
            room.clientClocks.delete(socket);
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

// ── Internal helpers ──────────────────────────────────────────────────────────

/** Decode a blob and advance the room's highWatermark if the blob has a higher ts. */
function updateHighWatermark(room: Room, blob: Uint8Array): void {
    try {
        const env = msgpackDecode(blob) as { timestamp?: number };
        const ts = typeof env.timestamp === 'number' ? env.timestamp : 0;
        if (ts > room.highWatermark) {
            room.highWatermark = ts;
        }
    } catch {
        // Non-fatal.
    }
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
