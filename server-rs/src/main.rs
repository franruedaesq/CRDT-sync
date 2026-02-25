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
//! ## Security
//!
//! Envelopes may carry an Ed25519 `signature` field.  If the sending node's
//! public key is registered with the server (via `AppState::key_registry`),
//! the signature is verified before the envelope is applied.  Additionally, a
//! per-node field-level Access Control List (`AppState::acl`) restricts which
//! keys each node is allowed to write.
//!
//! ## Persistence
//!
//! Room state is periodically (every 10 s) serialised as MessagePack and saved
//! to an SQLite database (`crdt_sync.db`).  When a room is first requested but
//! not yet in memory, the server tries to load the last saved snapshot from the
//! database before creating a fresh empty store.
//!
//! ## Usage
//!
//! ```bash
//! cargo run -p crdt-sync-server-rs -- --port 8000
//! ```

use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex as StdMutex},
};
use std::sync::atomic::{AtomicU64, Ordering};

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
use ed25519_dalek::{Signature, VerifyingKey, Verifier};
use futures_util::{SinkExt, StreamExt};
use rusqlite::Connection;
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

// ── Security types ────────────────────────────────────────────────────────────

/// Registry mapping node_id → 32-byte Ed25519 verifying key.
///
/// If a node's public key is registered here, every envelope it sends *must*
/// carry a valid signature.  Nodes not present in the registry are trusted
/// implicitly (backward-compatible default).
type KeyRegistry = HashMap<String, [u8; 32]>;

/// Field-level Access Control List: node_id → set of allowed CRDT key prefixes.
///
/// A node with no entry in this map is allowed to write any key.  A node with
/// an entry may only write keys whose name equals or starts with one of the
/// strings in its allowed set (e.g., `"sensors."` would permit
/// `"sensors.battery"` and `"sensors.temp"`).
type Acl = HashMap<String, HashSet<String>>;

// ── SQLite helpers ────────────────────────────────────────────────────────────

/// Thread-safe SQLite connection handle (sync mutex, used with spawn_blocking).
type DbConn = Arc<StdMutex<Connection>>;

/// Open (or create) the SQLite database and ensure the `rooms` table exists.
fn open_db(path: &str) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS rooms (
            room_id    TEXT    PRIMARY KEY,
            state_blob BLOB    NOT NULL,
            updated_at INTEGER NOT NULL
         );",
    )?;
    Ok(conn)
}

/// Persist a room's [`StateStore`] snapshot to SQLite.
fn db_save_room(conn: &Connection, room_id: &str, store: &StateStore) -> rusqlite::Result<()> {
    let blob = rmp_serde::to_vec_named(store).map_err(|e| {
        rusqlite::Error::ToSqlConversionFailure(Box::new(e))
    })?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    conn.execute(
        "INSERT INTO rooms (room_id, state_blob, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(room_id) DO UPDATE SET state_blob = excluded.state_blob,
                                             updated_at  = excluded.updated_at",
        rusqlite::params![room_id, blob, now],
    )?;
    Ok(())
}

/// Load a [`StateStore`] snapshot for `room_id` from SQLite, if one exists.
fn db_load_room(conn: &Connection, room_id: &str) -> rusqlite::Result<Option<StateStore>> {
    let mut stmt = conn.prepare(
        "SELECT state_blob FROM rooms WHERE room_id = ?1",
    )?;
    let mut rows = stmt.query(rusqlite::params![room_id])?;
    if let Some(row) = rows.next()? {
        let blob: Vec<u8> = row.get(0)?;
        let store: StateStore = rmp_serde::from_slice(&blob).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Blob, Box::new(e))
        })?;
        return Ok(Some(store));
    }
    Ok(None)
}

// ── Security helpers ──────────────────────────────────────────────────────────

/// Return `true` if `node_id` is permitted to write `key` according to `acl`.
///
/// If the node has no ACL entry, all keys are permitted.
fn acl_allows(acl: &Acl, node_id: &str, key: &str) -> bool {
    match acl.get(node_id) {
        None => true,
        Some(allowed) => allowed.iter().any(|prefix| {
            key == prefix.as_str() || key.starts_with(prefix.as_str())
        }),
    }
}

/// Verify the Ed25519 signature attached to `env`, if the sender is registered.
///
/// Returns `Ok(())` if:
/// - the node is not in the key registry (no signature required), or
/// - the node is registered and the signature is present and valid.
///
/// Returns `Err(reason)` if the signature is missing or invalid for a
/// registered node.
fn verify_envelope_signature(
    key_registry: &KeyRegistry,
    env: &Envelope,
) -> Result<(), &'static str> {
    let Some(key_bytes) = key_registry.get(&env.node_id) else {
        return Ok(()); // unregistered node — trust implicitly
    };

    let Some(sig_bytes) = &env.signature else {
        return Err("missing signature from registered node");
    };

    let sig_arr: [u8; 64] = sig_bytes
        .as_slice()
        .try_into()
        .map_err(|_| "signature must be 64 bytes")?;
    let verifying_key = VerifyingKey::from_bytes(key_bytes)
        .map_err(|_| "invalid verifying key")?;
    let signature = Signature::from_bytes(&sig_arr);

    // The signed payload is the canonical MessagePack encoding of
    // (timestamp, node_id, op) — i.e., the envelope without the signature.
    let payload = rmp_serde::to_vec_named(&(&env.timestamp, &env.node_id, &env.op))
        .map_err(|_| "failed to serialise payload")?;

    verifying_key
        .verify(&payload, &signature)
        .map_err(|_| "invalid signature")
}

/// Extract the CRDT key from an envelope operation, if applicable.
fn envelope_key(env: &Envelope) -> Option<&str> {
    use crdt_sync::state_store::StoreOp;
    match &env.op {
        StoreOp::Register { key, .. } => Some(key),
        StoreOp::Set { key, .. } => Some(key),
        StoreOp::Sequence { key, .. } => Some(key),
    }
}

// ── Application state ─────────────────────────────────────────────────────────

/// Shared application state threaded through all handlers and background tasks.
struct AppState {
    /// Live room map.
    rooms: Mutex<HashMap<String, Room>>,
    /// Persistent SQLite store.
    db: DbConn,
    /// Ed25519 public keys for signature-enforced nodes.
    key_registry: KeyRegistry,
    /// Per-node field-level access control list.
    acl: Acl,
}

type SharedAppState = Arc<AppState>;

// ── Global client-id counter ──────────────────────────────────────────────────

static NEXT_CLIENT_ID: AtomicU64 = AtomicU64::new(0);

fn next_client_id() -> u64 {
    NEXT_CLIENT_ID.fetch_add(1, Ordering::Relaxed)
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    let port = parse_port_arg().unwrap_or(8000);

    // Open (or create) the SQLite database.
    let conn = open_db("crdt_sync.db").expect("failed to open SQLite database");
    let db: DbConn = Arc::new(StdMutex::new(conn));

    // Populate key registry and ACL from environment / config.
    // These are intentionally empty by default so existing clients continue to
    // work unchanged.  Operators add entries to enforce signature checking and
    // field-level access control.
    let key_registry = build_key_registry_from_env();
    let acl = build_acl_from_env();

    let state = Arc::new(AppState {
        rooms: Mutex::new(HashMap::new()),
        db: db.clone(),
        key_registry,
        acl,
    });

    // ── Background persistence task ───────────────────────────────────────────
    {
        let state_clone = state.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(10));
            loop {
                interval.tick().await;
                persist_all_rooms(&state_clone).await;
            }
        });
    }

    let app = Router::new()
        .route("/rooms/{room_id}", get(ws_handler))
        .with_state(state);

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

/// Build a key registry from `CRDT_KEY_<NODE_ID>=<hex-encoded-32-bytes>` env vars.
///
/// Example:
/// ```bash
/// export CRDT_KEY_sensor_node_1=aabbcc...  # 64 hex chars = 32 bytes
/// ```
fn build_key_registry_from_env() -> KeyRegistry {
    let mut registry = KeyRegistry::new();
    for (k, v) in std::env::vars() {
        let Some(node_id) = k.strip_prefix("CRDT_KEY_") else {
            continue;
        };
        if let Ok(bytes) = hex_decode_32(&v) {
            registry.insert(node_id.to_string(), bytes);
        }
    }
    registry
}

/// Build an ACL from `CRDT_ACL_<NODE_ID>=<comma-separated-key-prefixes>` env vars.
///
/// Example:
/// ```bash
/// export CRDT_ACL_sensor_node_1=sensors.,status
/// ```
fn build_acl_from_env() -> Acl {
    let mut acl = Acl::new();
    for (k, v) in std::env::vars() {
        let Some(node_id) = k.strip_prefix("CRDT_ACL_") else {
            continue;
        };
        let allowed: HashSet<String> = v.split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        if !allowed.is_empty() {
            acl.insert(node_id.to_string(), allowed);
        }
    }
    acl
}

/// Decode a 64-character hex string into a 32-byte array.
fn hex_decode_32(s: &str) -> Result<[u8; 32], ()> {
    let s = s.trim();
    if s.len() != 64 {
        return Err(());
    }
    let mut out = [0u8; 32];
    for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
        let hi = hex_nibble(chunk[0])?;
        let lo = hex_nibble(chunk[1])?;
        out[i] = (hi << 4) | lo;
    }
    Ok(out)
}

fn hex_nibble(b: u8) -> Result<u8, ()> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        _ => Err(()),
    }
}

// ── Persistence helpers ───────────────────────────────────────────────────────

/// Serialise every live room and upsert it into SQLite.
async fn persist_all_rooms(state: &AppState) {
    // Collect (room_id, store snapshot) pairs under a brief lock.
    let snapshots: Vec<(String, StateStore)> = {
        let map = state.rooms.lock().await;
        map.iter()
            .map(|(id, room)| (id.clone(), room.store.clone()))
            .collect()
    };

    if snapshots.is_empty() {
        return;
    }

    let db = state.db.clone();
    tokio::task::spawn_blocking(move || {
        if let Ok(conn) = db.lock() {
            for (room_id, store) in &snapshots {
                if let Err(e) = db_save_room(&conn, room_id, store) {
                    eprintln!("[crdt-sync-rs] failed to persist room {room_id}: {e}");
                }
            }
        }
    })
    .await
    .ok();
}

/// Try to load a saved room from the database; returns `None` if not found.
async fn try_load_room_from_db(db: &DbConn, room_id: &str) -> Option<StateStore> {
    let db = db.clone();
    let room_id = room_id.to_string();
    tokio::task::spawn_blocking(move || {
        let conn = db.lock().ok()?;
        db_load_room(&conn, &room_id).ok()?
    })
    .await
    .ok()
    .flatten()
}

// ── WebSocket handler ─────────────────────────────────────────────────────────

async fn ws_handler(
    Path(room_id): Path<String>,
    ws: WebSocketUpgrade,
    State(state): State<SharedAppState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, room_id, state))
}

async fn handle_socket(socket: WebSocket, room_id: String, state: SharedAppState) {
    let client_id = next_client_id();
    let (client_tx, mut client_rx) = mpsc::channel::<Vec<u8>>(64);

    // ── Register client & build snapshot ─────────────────────────────────────
    let snapshot_bytes = {
        let mut map = state.rooms.lock().await;
        let room = if let Some(r) = map.get_mut(&room_id) {
            r
        } else {
            // Attempt to restore state from the database before starting fresh.
            let store = match try_load_room_from_db(&state.db, &room_id).await {
                Some(saved) => {
                    println!("[crdt-sync-rs] room {room_id}: restored from database");
                    saved
                }
                None => StateStore::new("server"),
            };
            map.entry(room_id.clone()).or_insert_with(|| Room {
                store,
                clients: HashMap::new(),
                client_clocks: HashMap::new(),
                tombstones: Vec::new(),
                high_watermark: 0,
            })
        };

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
        cleanup(&state, &room_id, client_id).await;
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
    let state2 = state.clone();
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
                let mut map = state2.rooms.lock().await;
                let Some(room) = map.get_mut(&room_id2) else {
                    break;
                };

                let mut max_ts_in_batch: u64 = 0;

                for blob in &blobs {
                    let env = match rmp_serde::from_slice::<Envelope>(blob) {
                        Ok(e) => e,
                        Err(_) => continue,
                    };

                    // ── Signature verification ────────────────────────────────
                    if let Err(reason) = verify_envelope_signature(&state2.key_registry, &env) {
                        eprintln!(
                            "[crdt-sync-rs] rejected envelope from {}: {}",
                            env.node_id, reason
                        );
                        continue;
                    }

                    // ── Field-level ACL check ─────────────────────────────────
                    if let Some(key) = envelope_key(&env) {
                        if !acl_allows(&state2.acl, &env.node_id, key) {
                            eprintln!(
                                "[crdt-sync-rs] ACL denied: {} attempted to write '{}'",
                                env.node_id, key
                            );
                            continue;
                        }
                    }

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
                let mut map = state2.rooms.lock().await;
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

    cleanup(&state, &room_id, client_id).await;
}

/// Remove the client from the room; drop the room when it is empty.
async fn cleanup(state: &AppState, room_id: &str, client_id: u64) {
    let mut map = state.rooms.lock().await;
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
    let max_prune_ts = room
        .tombstones
        .iter()
        .filter(|(ts, _, _)| *ts < min_clock)
        .map(|(ts, _, _)| *ts)
        .max()?;
    room.tombstones.retain(|(ts, _, _)| *ts > max_prune_ts);

    rmp_serde::to_vec_named(&ServerMessage::Prune(max_prune_ts)).ok()
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crdt_sync::state_store::{StateStore, StoreOp};
    use crdt_sync::lww_register::LWWOp;
    use ed25519_dalek::{SigningKey, Signer};
    use rand_core::OsRng;

    // ── hex_decode_32 ────────────────────────────────────────────────────────

    #[test]
    fn hex_decode_32_valid() {
        let hex = "00".repeat(32);
        assert_eq!(hex_decode_32(&hex), Ok([0u8; 32]));

        let hex = format!("{}{}", "0f".repeat(16), "f0".repeat(16));
        let result = hex_decode_32(&hex).unwrap();
        assert_eq!(result[0], 0x0f);
        assert_eq!(result[16], 0xf0);
    }

    #[test]
    fn hex_decode_32_invalid_length() {
        assert!(hex_decode_32("aabb").is_err());
        assert!(hex_decode_32("").is_err());
    }

    #[test]
    fn hex_decode_32_invalid_chars() {
        let bad = "zz".repeat(32);
        assert!(hex_decode_32(&bad).is_err());
    }

    // ── acl_allows ───────────────────────────────────────────────────────────

    #[test]
    fn acl_allows_unrestricted_node() {
        let acl: Acl = HashMap::new();
        assert!(acl_allows(&acl, "any-node", "sensors.battery"));
    }

    #[test]
    fn acl_allows_exact_key_match() {
        let mut acl: Acl = HashMap::new();
        let mut allowed = HashSet::new();
        allowed.insert("sensors.battery".to_string());
        acl.insert("node-1".to_string(), allowed);

        assert!(acl_allows(&acl, "node-1", "sensors.battery"));
        assert!(!acl_allows(&acl, "node-1", "sensors.temp"));
        assert!(!acl_allows(&acl, "node-1", "robot.x"));
    }

    #[test]
    fn acl_allows_prefix_match() {
        let mut acl: Acl = HashMap::new();
        let mut allowed = HashSet::new();
        allowed.insert("sensors.".to_string());
        acl.insert("node-1".to_string(), allowed);

        assert!(acl_allows(&acl, "node-1", "sensors.battery"));
        assert!(acl_allows(&acl, "node-1", "sensors.temp"));
        assert!(!acl_allows(&acl, "node-1", "robot.x"));
    }

    // ── verify_envelope_signature ────────────────────────────────────────────

    fn make_envelope(node_id: &str, key: &str, value: i32) -> Envelope {
        Envelope {
            timestamp: 1,
            node_id: node_id.to_string(),
            op: StoreOp::Register {
                key: key.to_string(),
                op: LWWOp {
                    value: serde_json::json!(value),
                    timestamp: 1,
                    node_id: node_id.to_string(),
                },
            },
            signature: None,
        }
    }

    fn sign_envelope(env: &mut Envelope, signing_key: &SigningKey) {
        let payload = rmp_serde::to_vec_named(&(&env.timestamp, &env.node_id, &env.op))
            .expect("serialisation must succeed");
        let sig = signing_key.sign(&payload);
        env.signature = Some(sig.to_bytes().to_vec());
    }

    #[test]
    fn verify_unregistered_node_always_passes() {
        let registry: KeyRegistry = HashMap::new();
        let env = make_envelope("unknown", "x", 1);
        assert!(verify_envelope_signature(&registry, &env).is_ok());
    }

    #[test]
    fn verify_valid_signature_passes() {
        let signing_key = SigningKey::generate(&mut OsRng);
        let verifying_key: [u8; 32] = signing_key.verifying_key().to_bytes();

        let mut registry: KeyRegistry = HashMap::new();
        registry.insert("node-1".to_string(), verifying_key);

        let mut env = make_envelope("node-1", "x", 42);
        sign_envelope(&mut env, &signing_key);

        assert!(verify_envelope_signature(&registry, &env).is_ok());
    }

    #[test]
    fn verify_missing_signature_from_registered_node_fails() {
        let signing_key = SigningKey::generate(&mut OsRng);
        let verifying_key: [u8; 32] = signing_key.verifying_key().to_bytes();

        let mut registry: KeyRegistry = HashMap::new();
        registry.insert("node-1".to_string(), verifying_key);

        let env = make_envelope("node-1", "x", 42); // no signature
        assert!(verify_envelope_signature(&registry, &env).is_err());
    }

    #[test]
    fn verify_invalid_signature_from_registered_node_fails() {
        let signing_key = SigningKey::generate(&mut OsRng);
        let verifying_key: [u8; 32] = signing_key.verifying_key().to_bytes();

        let mut registry: KeyRegistry = HashMap::new();
        registry.insert("node-1".to_string(), verifying_key);

        let mut env = make_envelope("node-1", "x", 42);
        // Attach a garbage 64-byte signature.
        env.signature = Some(vec![0u8; 64]);

        assert!(verify_envelope_signature(&registry, &env).is_err());
    }

    // ── SQLite round-trip ────────────────────────────────────────────────────

    #[test]
    fn db_save_and_load_round_trip() {
        let conn = Connection::open_in_memory().expect("in-memory DB");
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS rooms (
                room_id    TEXT    PRIMARY KEY,
                state_blob BLOB    NOT NULL,
                updated_at INTEGER NOT NULL
             );",
        )
        .unwrap();

        let mut store = StateStore::new("server");
        store.set_register("robot.x", 7_i32);
        store.set_add("fleet", "unit-1");

        db_save_room(&conn, "room-A", &store).expect("save should succeed");

        let loaded = db_load_room(&conn, "room-A")
            .expect("query should succeed")
            .expect("room-A should be found");

        assert_eq!(loaded.get_register::<i32>("robot.x"), Some(7));
        assert!(loaded.set_contains("fleet", &"unit-1"));
    }

    #[test]
    fn db_load_missing_room_returns_none() {
        let conn = Connection::open_in_memory().expect("in-memory DB");
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS rooms (
                room_id    TEXT    PRIMARY KEY,
                state_blob BLOB    NOT NULL,
                updated_at INTEGER NOT NULL
             );",
        )
        .unwrap();

        let result = db_load_room(&conn, "nonexistent").expect("query should succeed");
        assert!(result.is_none());
    }

    #[test]
    fn db_save_overwrites_existing() {
        let conn = Connection::open_in_memory().expect("in-memory DB");
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS rooms (
                room_id    TEXT    PRIMARY KEY,
                state_blob BLOB    NOT NULL,
                updated_at INTEGER NOT NULL
             );",
        )
        .unwrap();

        let mut store = StateStore::new("server");
        store.set_register("x", 1_i32);
        db_save_room(&conn, "room", &store).unwrap();

        // Overwrite with newer state.
        store.set_register("x", 99_i32);
        db_save_room(&conn, "room", &store).unwrap();

        let loaded = db_load_room(&conn, "room").unwrap().unwrap();
        assert_eq!(loaded.get_register::<i32>("x"), Some(99));
    }
}
