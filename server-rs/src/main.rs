//! # crdt-sync-server-rs
//!
//! Rust WebSocket relay server for the crdt-sync CRDT protocol.
//!
//! ## Rooms
//!
//! Clients connect to `ws://host:8000/rooms/<roomId>`.  Each room owns its own
//! [`StateStore`] instance on the server.  State from different rooms never
//! bleeds across room boundaries.
//!
//! ## Protocol (server → client) — binary MessagePack frames
//!
//! - `{ "type": "SNAPSHOT", "data": <MessagePack-encoded StateStore bytes> }` —
//!   sent once to every newly connected client so it can hydrate from the
//!   authoritative server state using `WasmStateStore.merge_snapshot()`.
//! - `{ "type": "UPDATE", "data": <Uint8Array[]> }` — forwarded to every
//!   *other* client in the room whenever a client submits new envelopes.
//! - `{ "type": "PRUNE", "data": <Lamport timestamp number> }` — broadcast
//!   when all clients have advanced past a tombstone's timestamp, so each
//!   replica can erase dead memory.
//!
//! ## Protocol (client → server) — binary MessagePack frames
//!
//! A MessagePack-encoded array of envelope blobs (`Uint8Array[]`).
//!
//! ## Usage
//!
//! ```bash
//! cargo run -p crdt-sync-server-rs -- --port 8000
//! ```

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use axum::{
    Router,
    extract::{
        Path, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    response::IntoResponse,
    routing::get,
};
use crdt_sync::state_store::{Envelope, StateStore};
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use tokio::sync::{mpsc, Mutex};

// ── Server message types ──────────────────────────────────────────────────────

/// Messages sent from the server to clients (before MessagePack encoding).
#[derive(Serialize)]
#[serde(tag = "type", content = "data")]
enum ServerMessage<'a> {
    /// Full state snapshot as MessagePack-encoded [`StateStore`] bytes.
    #[serde(rename = "SNAPSHOT")]
    Snapshot(&'a [u8]),
    /// Batch of envelope blobs forwarded from a peer.
    #[serde(rename = "UPDATE")]
    Update(Vec<Vec<u8>>),
    /// Prune tombstones up to and including this Lamport timestamp.
    #[serde(rename = "PRUNE")]
    Prune(u64),
}

// ── Room ──────────────────────────────────────────────────────────────────────

/// Per-room server state.
struct Room {
    /// The authoritative CRDT state for this room.
    store: StateStore,
    /// Active client connections: client_id → sender for UPDATE messages.
    clients: HashMap<u64, mpsc::Sender<Vec<u8>>>,
    /// Maximum Lamport timestamp received per client (for GC / pruning).
    client_clocks: HashMap<u64, u64>,
    /// Tombstones waiting to be pruned: (Lamport ts, kind, key).
    tombstones: Vec<(u64, &'static str, String)>,
    /// Room-wide high-watermark timestamp.
    high_watermark: u64,
}

/// Shared map of all live rooms.
type Rooms = Arc<Mutex<HashMap<String, Room>>>;

// ── Global client-id counter ──────────────────────────────────────────────────

static NEXT_CLIENT_ID: AtomicU64 = AtomicU64::new(0);

fn next_client_id() -> u64 {
    NEXT_CLIENT_ID.fetch_add(1, Ordering::Relaxed)
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    let port = parse_port_arg().unwrap_or(8000);
    let rooms: Rooms = Arc::new(Mutex::new(HashMap::new()));

    let app = Router::new()
        .route("/rooms/{room_id}", get(ws_handler))
        .with_state(rooms);

    let addr = format!("0.0.0.0:{port}");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("failed to bind");

    println!("[crdt-sync-rs] relay listening on ws://{addr}");
    axum::serve(listener, app).await.expect("server error");
}

/// Parse `--port <number>` from process arguments.
fn parse_port_arg() -> Option<u16> {
    let args: Vec<String> = std::env::args().collect();
    let idx = args.iter().position(|a| a == "--port")?;
    args.get(idx + 1)?.parse().ok()
}

// ── WebSocket handler ─────────────────────────────────────────────────────────

async fn ws_handler(
    Path(room_id): Path<String>,
    ws: WebSocketUpgrade,
    State(rooms): State<Rooms>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, room_id, rooms))
}

async fn handle_socket(socket: WebSocket, room_id: String, rooms: Rooms) {
    let client_id = next_client_id();
    let (client_tx, mut client_rx) = mpsc::channel::<Vec<u8>>(64);

    // ── Register client & build snapshot ─────────────────────────────────────
    let snapshot_bytes = {
        let mut map = rooms.lock().await;
        let room = map.entry(room_id.clone()).or_insert_with(|| Room {
            store: StateStore::new("server"),
            clients: HashMap::new(),
            client_clocks: HashMap::new(),
            tombstones: Vec::new(),
            high_watermark: 0,
        });
        room.clients.insert(client_id, client_tx);
        // Seed new client's clock at the room high-watermark.
        room.client_clocks.insert(client_id, room.high_watermark);

        // Serialise the full StateStore as MessagePack bytes (the `data` field).
        let store_bytes = rmp_serde::to_vec_named(&room.store)
            .expect("StateStore must be serialisable");
        // Wrap in { type: "SNAPSHOT", data: <bytes> }.
        rmp_serde::to_vec_named(&ServerMessage::Snapshot(&store_bytes))
            .expect("ServerMessage must be serialisable")
    };

    let (mut ws_sender, mut ws_receiver) = socket.split();

    // ── Send SNAPSHOT to the new joiner ───────────────────────────────────────
    if ws_sender.send(Message::Binary(snapshot_bytes.into())).await.is_err() {
        cleanup(&rooms, &room_id, client_id).await;
        return;
    }

    // ── Task: forward UPDATE/PRUNE messages from the mpsc channel → WS ───────
    let mut send_task = tokio::spawn(async move {
        while let Some(bytes) = client_rx.recv().await {
            if ws_sender.send(Message::Binary(bytes.into())).await.is_err() {
                break;
            }
        }
    });

    // ── Task: receive envelopes from this client, apply & broadcast ───────────
    let rooms2 = rooms.clone();
    let room_id2 = room_id.clone();
    let mut recv_task = tokio::spawn(async move {
        while let Some(result) = ws_receiver.next().await {
            let bytes = match result {
                Ok(Message::Binary(b)) => b,
                Ok(Message::Close(_)) | Err(_) => break,
                _ => continue,
            };

            // Decode outer frame: expected to be a Vec<Vec<u8>> (array of blobs).
            let blobs: Vec<Vec<u8>> = match rmp_serde::from_slice::<Vec<Vec<u8>>>(&bytes) {
                Ok(v) => v,
                Err(_) => continue,
            };

            // Apply envelopes & collect peer senders (brief lock).
            let (update_bytes_opt, peer_senders, prune_bytes_opt) = {
                let mut map = rooms2.lock().await;
                let Some(room) = map.get_mut(&room_id2) else {
                    break;
                };

                let mut max_ts_in_batch: u64 = 0;

                for blob in &blobs {
                    if let Ok(env) = rmp_serde::from_slice::<Envelope>(blob) {
                        let ts = env.timestamp;
                        if ts > max_ts_in_batch {
                            max_ts_in_batch = ts;
                        }
                        // Track tombstones for deferred pruning.
                        match &env.op {
                            crdt_sync::state_store::StoreOp::Sequence { key, op } => {
                                if matches!(op, crdt_sync::rga::RGAOp::Delete { .. }) {
                                    room.tombstones.push((ts, "Sequence", key.clone()));
                                }
                            }
                            crdt_sync::state_store::StoreOp::Set { key, op } => {
                                if matches!(op, crdt_sync::or_set::ORSetOp::Remove { .. }) {
                                    room.tombstones.push((ts, "Set", key.clone()));
                                }
                            }
                            _ => {}
                        }
                        room.store.apply_envelope(env);
                    }
                }

                // Update vector clock for this client.
                if max_ts_in_batch > 0 {
                    let entry = room.client_clocks.entry(client_id).or_insert(0);
                    if max_ts_in_batch > *entry {
                        *entry = max_ts_in_batch;
                    }
                    if max_ts_in_batch > room.high_watermark {
                        room.high_watermark = max_ts_in_batch;
                    }
                }

                // Build UPDATE message for peers.
                let update_msg = rmp_serde::to_vec_named(&ServerMessage::Update(blobs.clone()))
                    .ok();

                let peers: Vec<mpsc::Sender<Vec<u8>>> = room
                    .clients
                    .iter()
                    .filter(|&(&id, _)| id != client_id)
                    .map(|(_, tx)| tx.clone())
                    .collect();

                // Check whether any tombstones can be pruned.
                let prune_bytes = check_and_prune(room);

                (update_msg, peers, prune_bytes)
            };

            // Broadcast outside the lock.
            if let Some(update_bytes) = update_bytes_opt {
                for tx in &peer_senders {
                    let _ = tx.try_send(update_bytes.clone());
                }
            }
            // Broadcast PRUNE to ALL clients (including sender) if needed.
            if let Some(prune_bytes) = prune_bytes_opt {
                let mut map = rooms2.lock().await;
                if let Some(room) = map.get_mut(&room_id2) {
                    for (_, tx) in &room.clients {
                        let _ = tx.try_send(prune_bytes.clone());
                    }
                }
            }
        }
    });

    // ── Wait for either task to complete ──────────────────────────────────────
    tokio::select! {
        _ = &mut send_task => recv_task.abort(),
        _ = &mut recv_task => send_task.abort(),
    }

    cleanup(&rooms, &room_id, client_id).await;
}

/// Remove the client from the room; drop the room when it is empty.
async fn cleanup(rooms: &Rooms, room_id: &str, client_id: u64) {
    let mut map = rooms.lock().await;
    if let Some(room) = map.get_mut(room_id) {
        room.clients.remove(&client_id);
        room.client_clocks.remove(&client_id);
        if room.clients.is_empty() {
            map.remove(room_id);
        }
    }
}

/// Check if any tombstones can be pruned and return the serialised PRUNE message
/// if so, removing the pruneable entries from the room's tracking list.
fn check_and_prune(room: &mut Room) -> Option<Vec<u8>> {
    if room.tombstones.is_empty() || room.clients.is_empty() {
        return None;
    }
    if room.client_clocks.len() < room.clients.len() {
        return None;
    }
    let min_clock = room.client_clocks.values().copied().min()?;
    if min_clock == 0 {
        return None;
    }

    // Tombstones strictly older than min_clock can be pruned.
    let pruneable: Vec<u64> = room
        .tombstones
        .iter()
        .filter(|(ts, _, _)| *ts < min_clock)
        .map(|(ts, _, _)| *ts)
        .collect();
    if pruneable.is_empty() {
        return None;
    }

    let max_prune_ts = *pruneable.iter().max()?;
    room.tombstones.retain(|(ts, _, _)| *ts > max_prune_ts);

    rmp_serde::to_vec_named(&ServerMessage::Prune(max_prune_ts)).ok()
}
