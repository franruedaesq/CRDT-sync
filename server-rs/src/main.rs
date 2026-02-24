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
//! ## Protocol (server → client)
//!
//! - `{ "type": "SNAPSHOT", "data": "<StateStore JSON>" }` — sent once to every
//!   newly connected client so it can hydrate from the authoritative server state
//!   using `WasmStateStore.merge_snapshot()`.
//! - `{ "type": "UPDATE", "data": "<envelope batch JSON>" }` — forwarded to every
//!   *other* client in the room whenever a client submits new envelopes.
//!
//! ## Protocol (client → server)
//!
//! A JSON array of envelope strings, e.g. `["<env1>", "<env2>"]`.
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
use serde_json::json;
use tokio::sync::{mpsc, Mutex};

// ── Room ──────────────────────────────────────────────────────────────────────

/// Per-room server state.
struct Room {
    /// The authoritative CRDT state for this room.
    store: StateStore,
    /// Active client connections: client_id → sender for UPDATE messages.
    clients: HashMap<u64, mpsc::Sender<String>>,
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
    let (client_tx, mut client_rx) = mpsc::channel::<String>(64);

    // ── Register client & build snapshot ─────────────────────────────────────
    let snapshot_json = {
        let mut map = rooms.lock().await;
        let room = map.entry(room_id.clone()).or_insert_with(|| Room {
            store: StateStore::new("server"),
            clients: HashMap::new(),
        });
        room.clients.insert(client_id, client_tx);
        serde_json::to_string(&room.store).expect("StateStore must be serialisable")
    };

    let snapshot_msg = json!({ "type": "SNAPSHOT", "data": snapshot_json }).to_string();

    let (mut ws_sender, mut ws_receiver) = socket.split();

    // ── Send SNAPSHOT to the new joiner ───────────────────────────────────────
    if ws_sender.send(Message::Text(snapshot_msg.into())).await.is_err() {
        cleanup(&rooms, &room_id, client_id).await;
        return;
    }

    // ── Task: forward UPDATE messages from the mpsc channel → WS ─────────────
    let mut send_task = tokio::spawn(async move {
        while let Some(msg) = client_rx.recv().await {
            if ws_sender.send(Message::Text(msg.into())).await.is_err() {
                break;
            }
        }
    });

    // ── Task: receive envelopes from this client, apply & broadcast ───────────
    let rooms2 = rooms.clone();
    let room_id2 = room_id.clone();
    let mut recv_task = tokio::spawn(async move {
        while let Some(result) = ws_receiver.next().await {
            let text = match result {
                Ok(Message::Text(t)) => t.to_string(),
                Ok(Message::Close(_)) | Err(_) => break,
                _ => continue,
            };

            // Validate: must be a JSON array.
            let parsed = match serde_json::from_str::<serde_json::Value>(&text) {
                Ok(v) if v.is_array() => v,
                _ => continue,
            };

            // Apply envelopes & collect peer senders (brief lock).
            let (update_msg, peer_senders) = {
                let mut map = rooms2.lock().await;
                let Some(room) = map.get_mut(&room_id2) else {
                    break;
                };

                // Apply each envelope to the server's authoritative store.
                for env_val in parsed.as_array().unwrap() {
                    if let Some(env_str) = env_val.as_str() {
                        if let Ok(env) = serde_json::from_str::<Envelope>(env_str) {
                            room.store.apply_envelope(env);
                        }
                    }
                }

                let update = json!({ "type": "UPDATE", "data": text }).to_string();
                let peers: Vec<mpsc::Sender<String>> = room
                    .clients
                    .iter()
                    .filter(|&(&id, _)| id != client_id)
                    .map(|(_, tx)| tx.clone())
                    .collect();
                (update, peers)
            };

            // Broadcast outside the lock.
            for tx in &peer_senders {
                let _ = tx.try_send(update_msg.clone());
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
        if room.clients.is_empty() {
            map.remove(room_id);
        }
    }
}
