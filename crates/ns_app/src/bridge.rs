//! Loopback bridge that serves the editor's maps to the Blender add-on in
//! `blender/n_shade_bridge`.
//!
//! Design constraints, in order:
//!
//! * **Never block the UI.** The listener and the per-client reader are plain
//!   `std::thread`s; evaluation runs on `AsyncComputeTaskPool` against a
//!   document snapshot, exactly like the editor's own background jobs.
//! * **Read-only.** The only commands a client can send are "tell me what you
//!   have" and "send me these maps". Nothing here can edit, save or export.
//! * **Bounded memory.** One evaluation in flight, a four-slot outbound queue,
//!   a 64 MiB cache budget and previews capped at 4096 px.
//! * **Idle costs nothing.** A request can carry `wait_ms`, and the host then
//!   holds it until the document's edit serial, the publish epoch or the
//!   deadline changes. An idle session moves a few dozen bytes per second and
//!   runs no evaluation, while a change is delivered on the next frame.
//! * **No new dependencies.** `std::net` plus `serde_json`, which the crate
//!   already depends on.
//!
//! Wire format (mirrored in `blender/n_shade_bridge/protocol.py`):
//! `magic "QXL1" | op u8 | flags u8 | hlen u16 | plen u32 | seq u32`, then
//! `hlen` bytes of JSON header and `plen` bytes of payload, all little endian.
//! Map payloads are raw RGBA8/RGBA16, the same layout the live preview texture
//! uses, so nothing is encoded or decoded on either side of the loopback hop.

use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, sync_channel, Receiver, Sender, SyncSender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use bevy::log::{debug, info};
use bevy::prelude::*;
use bevy::tasks::{block_on, poll_once, AsyncComputeTaskPool, Task};
use glam::UVec2;
use ns_core::EvalCache;
use ns_pbr::{MapStore, Stage, StageKey, StageOutput};
use serde_json::{json, Value};

use crate::app::{DocumentRes, EditorState, SharedDocument};
use crate::bridge_maps::{self as maps, Bits, DocumentParams, JobSource};
use crate::pbr_ui::PbrRuntime;

pub use crate::bridge_maps::MapId;

pub const DEFAULT_PORT: u16 = 47_100;
const PORT_SCAN: u16 = 20;
const PROTOCOL_VERSION: u64 = 2;

const MAGIC: [u8; 4] = *b"QXL1";
const HEADER_SIZE: usize = 16;
const MAX_HEADER_BYTES: usize = 8 * 1024;
/// Requests are tiny; only the host sends bulk data.
const MAX_INBOUND_PAYLOAD: usize = 64 * 1024;
const MAX_FRAME_BYTES: usize = 256 * 1024 * 1024;

const OP_HELLO: u8 = 0x01;
const OP_HELLO_OK: u8 = 0x02;
const OP_HELLO_ERR: u8 = 0x03;
const OP_GET_MAP: u8 = 0x10;
const OP_MAP: u8 = 0x11;
const OP_UNCHANGED: u8 = 0x12;
const OP_PING: u8 = 0x13;
const OP_PONG: u8 = 0x14;
const OP_BYE: u8 = 0x15;
const OP_WATCH: u8 = 0x16;
const OP_PUBLISHED: u8 = 0x17;
const OP_CATALOG: u8 = 0x18;
const OP_NOTICE: u8 = 0x19;
/// Blender -> host: a mesh file to import (UV layout and preview model).
const OP_PUSH_MESH: u8 = 0x1A;
/// Ceiling on an imported mesh file; an OBJ of a game mesh is a few MB.
const MAX_MESH_BYTES: u64 = 512 * 1024 * 1024;

const READ_TIMEOUT: Duration = Duration::from_millis(25);
const WRITE_TIMEOUT: Duration = Duration::from_secs(4);
const HANDSHAKE_DEADLINE: Duration = Duration::from_secs(5);
const IDLE_DISCONNECT: Duration = Duration::from_secs(180);
/// Frames queued for a client that stopped reading. One request plus one late
/// reply is plenty; dropping is cheaper than buffering.
const OUT_QUEUE: usize = 4;
/// Floor between two evaluations. Continuous painting would otherwise run one
/// evaluation per received frame.
const MIN_EVAL_INTERVAL: Duration = Duration::from_millis(60);
/// A client may hold a request open this long at most.
const MAX_WAIT: Duration = Duration::from_millis(2000);
/// How often the accept thread looks at its stop flag while idle.
const ACCEPT_POLL: Duration = Duration::from_millis(40);
const RECONNECT_DELAY: Duration = Duration::from_secs(2);

// ---------------------------------------------------------------------------
// Plugin and resource
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct BridgePlugin {
    port: u16,
    enabled: bool,
}

impl BridgePlugin {
    pub fn new(port: u16, enabled: bool) -> Self {
        Self { port, enabled }
    }
}

impl Default for BridgePlugin {
    fn default() -> Self {
        Self {
            port: DEFAULT_PORT,
            enabled: false,
        }
    }
}

impl Plugin for BridgePlugin {
    fn build(&self, app: &mut App) {
        // Always installed: the Settings window can open the listener later,
        // and a disabled bridge costs one boolean check per frame.
        app.insert_resource(BridgeRuntime::new(self.port, self.enabled))
            .add_systems(Update, (bridge_control, bridge_tick).chain());
    }
}

/// A started listener plus the channels that belong to its session.
struct Session {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    port: u16,
    token: String,
}
struct OutFrame {
    op: u8,
    seq: u32,
    header: Value,
    payload: Vec<u8>,
}

/// Everything the UI can display about the bridge. Owned by Bevy and touched
/// only from the main thread.
#[derive(Resource)]
pub struct BridgeRuntime {
    /// Mirror of the Settings checkbox. `bridge_control` reconciles it with
    /// `session`.
    pub enabled: bool,
    pub port: u16,
    pub status: String,
    pub client: Option<String>,
    /// Frames served since the listener started, and their payload bytes.
    pub frames: u64,
    pub bytes: u64,
    /// Bumped by [`BridgeRuntime::publish`]; part of every cache key so a push
    /// invalidates what the client already holds.
    pub publish_epoch: u64,
    pub published: Vec<MapId>,
    pub last_map: Option<MapId>,
    pub last_eval_ms: f32,

    session: Option<Session>,
    /// `Receiver` is not `Sync`, and a Bevy resource must be; the mutex is
    /// never contended because only the tick system touches this side.
    commands: Option<Mutex<Receiver<Command>>>,
    out: Option<SyncSender<OutFrame>>,
    eval: Option<Task<BridgeJob>>,
    cache: Option<EvalCache>,
    sent: Option<SentKey>,
    last_eval: Option<Instant>,
    pending: Option<Pending>,
    catalog: Vec<CatalogEntry>,
    catalog_sent: bool,
    retry_at: Option<Instant>,
    /// A mesh push we accepted and have not reported the outcome of yet.
    mesh_ack: Option<MeshAck>,
}

impl BridgeRuntime {
    fn new(port: u16, enabled: bool) -> Self {
        Self {
            enabled,
            port,
            status: if enabled {
                "Starting...".into()
            } else {
                "Off".into()
            },
            client: None,
            frames: 0,
            bytes: 0,
            publish_epoch: 0,
            published: Vec::new(),
            last_map: None,
            last_eval_ms: 0.0,
            session: None,
            commands: None,
            out: None,
            eval: None,
            cache: None,
            sent: None,
            last_eval: None,
            pending: None,
            catalog: Vec::new(),
            catalog_sent: false,
            retry_at: None,
            mesh_ack: None,
        }
    }

    pub fn listening(&self) -> bool {
        self.session.is_some()
    }

    pub fn connected(&self) -> bool {
        self.client.is_some()
    }

    /// Hand the given maps to the connected add-on on its next request.
    ///
    /// Returns the number of maps queued, or 0 when the bridge cannot serve
    /// them (listener off, no client, nothing available).
    pub fn publish(&mut self, requested: impl IntoIterator<Item = MapId>) -> usize {
        let maps: Vec<MapId> = {
            let mut seen = Vec::new();
            for map in requested {
                if !seen.contains(&map) {
                    seen.push(map);
                }
            }
            seen
        };
        if maps.is_empty() {
            return 0;
        }
        if !self.enabled || self.session.is_none() {
            self.status = "Enable the Blender bridge first".into();
            return 0;
        }
        self.publish_epoch = self.publish_epoch.wrapping_add(1);
        self.published = maps.clone();
        // A stale `sent` key would otherwise answer the next request with
        // "unchanged" and the push would vanish.
        self.sent = None;
        self.status = if self.client.is_some() {
            format!("Sending {} map(s) to Blender", maps.len())
        } else {
            format!("Waiting for Blender: {} map(s) queued", maps.len())
        };
        maps.len()
    }

    fn start(&mut self) {
        if self.session.is_some() {
            return;
        }
        let Some((listener, port)) = bind(self.port) else {
            self.status = format!(
                "No free loopback port in {}..{}",
                self.port,
                self.port.saturating_add(PORT_SCAN)
            );
            self.retry_at = Some(Instant::now() + RECONNECT_DELAY);
            return;
        };
        let Some(token) = new_token() else {
            self.status = "Bridge disabled: the OS random source is unavailable".into();
            self.retry_at = Some(Instant::now() + RECONNECT_DELAY);
            return;
        };
        let config = match write_config(port, &token) {
            Ok(path) => {
                debug!("bridge: pairing file {}", path.display());
                String::new()
            }
            Err(error) => format!(" (no pairing file: {error})"),
        };
        let stop = Arc::new(AtomicBool::new(false));
        let (commands_tx, commands_rx) = channel::<Command>();
        let (out_tx, out_rx) = sync_channel::<OutFrame>(OUT_QUEUE);
        let thread = spawn_accept(listener, token.clone(), commands_tx, out_rx, stop.clone());
        self.session = Some(Session {
            stop,
            thread,
            port,
            token,
        });
        self.commands = Some(Mutex::new(commands_rx));
        self.out = Some(out_tx);
        self.client = None;
        self.sent = None;
        self.pending = None;
        self.catalog_sent = false;
        self.retry_at = None;
        self.port = port;
        self.status = format!("Listening on 127.0.0.1:{port}{config}");
        info!("n-shade bridge: 127.0.0.1:{port}");
    }

    fn stop(&mut self) {
        if let Some(mut session) = self.session.take() {
            session.stop.store(true, Ordering::SeqCst);
            if let Some(thread) = session.thread.take() {
                // Bounded: the accept loop polls every 40 ms and the client
                // loop every 25 ms.
                let _ = thread.join();
            }
            remove_config(&session.token);
        }
        self.commands = None;
        self.out = None;
        self.client = None;
        self.eval = None;
        self.pending = None;
        self.sent = None;
        self.catalog_sent = false;
        self.status = "Off".into();
    }

    fn send(&mut self, op: u8, seq: u32, header: Value, payload: Vec<u8>) {
        if let Some(out) = self.out.as_ref() {
            let _ = out.try_send(OutFrame {
                op,
                seq,
                header,
                payload,
            });
        }
    }
}

/// Reconciles the Settings checkbox with the listener.
fn bridge_control(mut runtime: ResMut<BridgeRuntime>) {
    if runtime.enabled {
        if let Some(session) = runtime.session.as_ref() {
            if session.port != runtime.port {
                // The port field changed; come back next frame with the new one.
                runtime.stop();
                return;
            }
        }
        if runtime.session.is_none() {
            match runtime.retry_at {
                Some(at) if Instant::now() < at => {}
                _ => runtime.start(),
            }
        }
    } else if runtime.session.is_some() {
        runtime.stop();
    }
}

// ---------------------------------------------------------------------------
// Wire format
// ---------------------------------------------------------------------------

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}

fn framing(op: u8, seq: u32, header_len: usize, payload_len: usize) -> [u8; HEADER_SIZE] {
    let mut out = [0u8; HEADER_SIZE];
    out[0..4].copy_from_slice(&MAGIC);
    out[4] = op;
    out[5] = 0;
    out[6..8].copy_from_slice(&(header_len as u16).to_le_bytes());
    out[8..12].copy_from_slice(&(payload_len as u32).to_le_bytes());
    out[12..16].copy_from_slice(&seq.to_le_bytes());
    out
}

fn write_frame(
    stream: &mut TcpStream,
    op: u8,
    seq: u32,
    header: &Value,
    payload: &[u8],
) -> io::Result<()> {
    let head = serde_json::to_vec(header).map_err(|error| invalid(&error.to_string()))?;
    if head.len() > MAX_HEADER_BYTES || payload.len() > MAX_FRAME_BYTES {
        return Err(invalid("outgoing frame is too large"));
    }
    // Three writes, no concatenation: the payload is never copied to make a
    // single buffer.
    stream.write_all(&framing(op, seq, head.len(), payload.len()))?;
    stream.write_all(&head)?;
    stream.write_all(payload)?;
    Ok(())
}

/// Accumulates inbound bytes until a complete frame is available.
///
/// A read timeout must not lose a partially received frame, so the buffer
/// outlives individual `read` calls and `take` only consumes whole frames.
struct FrameReader {
    buffer: Vec<u8>,
}

impl FrameReader {
    fn new() -> Self {
        Self {
            buffer: Vec::with_capacity(256),
        }
    }

    fn next(&mut self, stream: &mut TcpStream) -> io::Result<Option<(u8, u32, Value)>> {
        loop {
            if let Some(frame) = self.take()? {
                return Ok(Some(frame));
            }
            let mut chunk = [0u8; 2048];
            match stream.read(&mut chunk) {
                Ok(0) => return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "peer closed")),
                Ok(read) => self.buffer.extend_from_slice(&chunk[..read]),
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock
                            | io::ErrorKind::TimedOut
                            | io::ErrorKind::Interrupted
                    ) =>
                {
                    return Ok(None)
                }
                Err(error) => return Err(error),
            }
        }
    }

    fn take(&mut self) -> io::Result<Option<(u8, u32, Value)>> {
        if self.buffer.len() < HEADER_SIZE {
            return Ok(None);
        }
        let parsed = {
            let bytes = &self.buffer[..];
            if bytes[0..4] != MAGIC {
                return Err(invalid("bad magic"));
            }
            let op = bytes[4];
            let header_len = u16::from_le_bytes([bytes[6], bytes[7]]) as usize;
            let payload_len =
                u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]) as usize;
            let seq = u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]);
            if header_len > MAX_HEADER_BYTES {
                return Err(invalid("header too large"));
            }
            if payload_len > MAX_INBOUND_PAYLOAD {
                return Err(invalid("inbound payload too large"));
            }
            let total = HEADER_SIZE + header_len + payload_len;
            if bytes.len() < total {
                return Ok(None);
            }
            let header = if header_len == 0 {
                Value::Object(serde_json::Map::new())
            } else {
                serde_json::from_slice(&bytes[HEADER_SIZE..HEADER_SIZE + header_len])
                    .map_err(|error| invalid(&format!("header is not JSON: {error}")))?
            };
            (op, seq, header, total)
        };
        let (op, seq, header, total) = parsed;
        self.buffer.drain(..total);
        Ok(Some((op, seq, header)))
    }
}

// ---------------------------------------------------------------------------
// Pairing file
// ---------------------------------------------------------------------------

fn config_path() -> PathBuf {
    if cfg!(windows) {
        if let Ok(base) = std::env::var("LOCALAPPDATA") {
            return PathBuf::from(base).join("n-shade").join("bridge.json");
        }
        return std::env::temp_dir().join("n-shade-bridge.json");
    }
    if let Ok(runtime) = std::env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(runtime).join("n-shade-bridge.json");
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home)
            .join(".config")
            .join("n-shade")
            .join("bridge.json");
    }
    std::env::temp_dir().join("n-shade-bridge.json")
}

/// Pairing file the add-on reads. Shown in the Settings window so a user can
/// point a second machine's Blender at it by hand.
pub fn pairing_path_display() -> String {
    config_path().display().to_string()
}

/// 128 bits from the operating system's CSPRNG, hex-encoded (32 chars).
///
/// This is not a long-term secret: it exists so an unrelated local process, or
/// a web page probing loopback ports, cannot pull a document stream it was not
/// invited to see. `None` if the OS refuses; the caller then refuses to start
/// the bridge rather than fall back to something guessable.
fn new_token() -> Option<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).ok()?;
    let mut out = String::with_capacity(32);
    for byte in bytes {
        use std::fmt::Write;
        let _ = write!(out, "{byte:02x}");
    }
    Some(out)
}

fn write_config(port: u16, token: &str) -> io::Result<PathBuf> {
    let path = config_path();
    write_config_at(&path, port, token)?;
    Ok(path)
}

fn write_config_at(path: &Path, port: u16, token: &str) -> io::Result<()> {
    let body = serde_json::to_vec_pretty(&json!({
        "protocol": PROTOCOL_VERSION,
        "app": "n-shade",
        "port": port,
        "token": token,
        "pid": std::process::id(),
    }))
    .map_err(|error| io::Error::other(error.to_string()))?;
    // The atomic writer creates the parent directory and fsyncs, so a reader
    // never sees a half-written pairing file.
    ns_core::io::write_atomic(path, &body).map_err(|error| io::Error::other(error.to_string()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(error) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
            // Do not leave a pairing file with default permissions behind.
            let _ = std::fs::remove_file(path);
            return Err(error);
        }
    }
    Ok(())
}

/// Removes the pairing file only if it still carries this session's token, so
/// stopping one instance never un-pairs another.
fn remove_config(token: &str) {
    let path = config_path();
    let Ok(body) = std::fs::read_to_string(&path) else {
        return;
    };
    let Ok(value) = serde_json::from_str::<Value>(&body) else {
        return;
    };
    if value.get("token").and_then(Value::as_str) == Some(token) {
        let _ = std::fs::remove_file(&path);
    }
}

// ---------------------------------------------------------------------------
// Socket threads
// ---------------------------------------------------------------------------

enum Command {
    Connected {
        client: String,
    },
    Closed {
        reason: String,
    },
    GetMap(GetRequest),
    Watch {
        seq: u32,
        epoch: u64,
        wait: Duration,
    },
    PushMesh {
        path: String,
        name: String,
        seq: u32,
    },
}

#[derive(Clone, Debug)]
struct GetRequest {
    map: MapId,
    max_size: u32,
    bits: Bits,
    wait: Duration,
    force: bool,
    seq: u32,
    /// Publish generation the client has seen. Older than the host's means a
    /// push happened while this request was in flight, so the reply carries
    /// the published map list with it.
    epoch: u64,
}

fn bind(preferred: u16) -> Option<(TcpListener, u16)> {
    for offset in 0..PORT_SCAN {
        let port = preferred.saturating_add(offset);
        // Loopback only. Binding 0.0.0.0 would expose the document to the LAN.
        if let Ok(listener) = TcpListener::bind((Ipv4Addr::LOCALHOST, port)) {
            return Some((listener, port));
        }
    }
    None
}

fn spawn_accept(
    listener: TcpListener,
    token: String,
    commands: Sender<Command>,
    out_rx: Receiver<OutFrame>,
    stop: Arc<AtomicBool>,
) -> Option<JoinHandle<()>> {
    let builder = std::thread::Builder::new().name("n-shade-bridge".into());
    builder
        .spawn(move || {
            // Non-blocking accept so the thread can honour the stop flag; the
            // 40 ms poll costs nothing measurable.
            let _ = listener.set_nonblocking(true);
            let mut out_rx = out_rx;
            while !stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        let _ = stream.set_nodelay(true);
                        // Replies queued for a session that already ended
                        // belong to nobody.
                        while out_rx.try_recv().is_ok() {}
                        out_rx = match serve(stream, &token, &commands, out_rx, &stop) {
                            Some(receiver) => receiver,
                            None => break,
                        };
                    }
                    Err(ref error) if error.kind() == io::ErrorKind::WouldBlock => {
                        std::thread::sleep(ACCEPT_POLL);
                    }
                    Err(error) => {
                        debug!("bridge: accept failed: {error}");
                        std::thread::sleep(ACCEPT_POLL);
                    }
                }
            }
            debug!("bridge: listener stopped");
        })
        .ok()
}

/// Serves one client until it leaves or the bridge is stopped.
///
/// Returns the receiver when the loop should accept another client, or `None`
/// when the listener itself is going away.
fn serve(
    mut stream: TcpStream,
    token: &str,
    commands: &Sender<Command>,
    out_rx: Receiver<OutFrame>,
    stop: &AtomicBool,
) -> Option<Receiver<OutFrame>> {
    let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
    let _ = stream.set_write_timeout(Some(WRITE_TIMEOUT));
    let mut reader = FrameReader::new();

    if !handshake(&mut stream, &mut reader, token, commands, stop) {
        // The client reports the reason itself; do not overwrite the editor's
        // status bar for a connection that never became a session.
        return Some(out_rx);
    }

    let mut last_rx = Instant::now();
    let mut closing: Option<String> = None;
    loop {
        if stop.load(Ordering::SeqCst) {
            closing = Some("bridge stopped".into());
            break;
        }
        // Flush replies first: this loop wakes at least every READ_TIMEOUT, so
        // a reply reaches the add-on within ~25 ms of being produced.
        loop {
            match out_rx.try_recv() {
                Ok(frame) => {
                    if write_frame(
                        &mut stream,
                        frame.op,
                        frame.seq,
                        &frame.header,
                        &frame.payload,
                    )
                    .is_err()
                    {
                        closing = Some("write failed".into());
                        break;
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    return Some(out_rx);
                }
            }
        }
        if closing.is_some() {
            break;
        }

        match reader.next(&mut stream) {
            Ok(Some((op, seq, header))) => {
                last_rx = Instant::now();
                match op {
                    OP_GET_MAP => {
                        let Some(request) = parse_get(&header, seq) else {
                            let _ = write_frame(
                                &mut stream,
                                OP_NOTICE,
                                seq,
                                &json!({
                                    "level": "warn",
                                    "message": "unknown map id",
                                    "map": header.get("map").cloned().unwrap_or(Value::Null),
                                }),
                                &[],
                            );
                            continue;
                        };
                        let _ = commands.send(Command::GetMap(request));
                    }
                    OP_WATCH => {
                        let epoch = header.get("epoch").and_then(Value::as_u64).unwrap_or(0);
                        let wait = wait_of(&header);
                        let _ = commands.send(Command::Watch { seq, epoch, wait });
                    }
                    OP_PUSH_MESH => {
                        let path = header
                            .get("path")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                        let name = header
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("blender mesh")
                            .to_string();
                        let _ = commands.send(Command::PushMesh { path, name, seq });
                    }
                    OP_PING => {
                        let _ = write_frame(&mut stream, OP_PONG, seq, &json!({"ok": true}), &[]);
                    }
                    OP_BYE => {
                        closing = Some("client left".into());
                        break;
                    }
                    other => debug!("bridge: ignoring op 0x{other:02x}"),
                }
            }
            Ok(None) => {
                if last_rx.elapsed() > IDLE_DISCONNECT {
                    closing = Some("idle".into());
                    break;
                }
            }
            Err(error) => {
                closing = Some(error.to_string());
                break;
            }
        }
    }

    let reason = closing.unwrap_or_else(|| "closed".into());
    let _ = write_frame(&mut stream, OP_BYE, 0, &json!({"reason": reason}), &[]);
    let _ = commands.send(Command::Closed { reason });
    Some(out_rx)
}

fn wait_of(header: &Value) -> Duration {
    let millis = header.get("wait_ms").and_then(Value::as_u64).unwrap_or(0);
    Duration::from_millis(millis).min(MAX_WAIT)
}

fn parse_get(header: &Value, seq: u32) -> Option<GetRequest> {
    let map = header
        .get("map")
        .and_then(Value::as_str)
        .and_then(MapId::from_id)?;
    let max_size = header
        .get("max_size")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(maps::MAX_SIDE as u64) as u32;
    let bits = Bits::from_wire(header.get("bits").and_then(Value::as_u64).unwrap_or(8));
    Some(GetRequest {
        map,
        max_size,
        bits,
        wait: wait_of(header),
        force: header
            .get("force")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        seq,
        epoch: header.get("epoch").and_then(Value::as_u64).unwrap_or(0),
    })
}

fn handshake(
    stream: &mut TcpStream,
    reader: &mut FrameReader,
    token: &str,
    commands: &Sender<Command>,
    stop: &AtomicBool,
) -> bool {
    let deadline = Instant::now() + HANDSHAKE_DEADLINE;
    while Instant::now() < deadline {
        if stop.load(Ordering::SeqCst) {
            return false;
        }
        let frame = match reader.next(stream) {
            Ok(Some(frame)) => frame,
            Ok(None) => continue,
            Err(_) => return false,
        };
        let (op, seq, header) = frame;
        if op != OP_HELLO {
            let _ = write_frame(
                stream,
                OP_HELLO_ERR,
                seq,
                &json!({"code": "expected_hello", "message": "the first frame must be HELLO"}),
                &[],
            );
            return false;
        }
        if header.get("protocol").and_then(Value::as_u64) != Some(PROTOCOL_VERSION) {
            let _ = write_frame(
                stream,
                OP_HELLO_ERR,
                seq,
                &json!({
                    "code": "protocol",
                    "message": format!("this host speaks protocol {PROTOCOL_VERSION}"),
                }),
                &[],
            );
            return false;
        }
        if header.get("token").and_then(Value::as_str) != Some(token) {
            let _ = write_frame(
                stream,
                OP_HELLO_ERR,
                seq,
                &json!({"code": "token", "message": "pairing token does not match"}),
                &[],
            );
            return false;
        }

        let client = header
            .get("client")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let version = header
            .get("client_version")
            .and_then(Value::as_str)
            .unwrap_or("?");
        let _ = commands.send(Command::Connected {
            client: format!("{client} {version}"),
        });
        let session: String = new_token().unwrap_or_default().chars().take(8).collect();
        let _ = write_frame(
            stream,
            OP_HELLO_OK,
            seq,
            &json!({
                "protocol": PROTOCOL_VERSION,
                "app": "n-shade",
                "app_version": env!("CARGO_PKG_VERSION"),
                "session": session,
                "max_size": maps::MAX_SIDE,
                "epoch": 0,
                // The catalogue travels in its own frame right after HELLO_OK
                // so it can be refreshed whenever the PBR pipeline finishes a
                // stage.
                "catalog": true,
            }),
            &[],
        );
        return true;
    }

    let _ = write_frame(
        stream,
        OP_HELLO_ERR,
        0,
        &json!({"code": "timeout", "message": "no HELLO within 5 s"}),
        &[],
    );
    false
}

// ---------------------------------------------------------------------------
// Cache keys
// ---------------------------------------------------------------------------

/// What the client already has. A matching key means "nothing changed", which
/// costs the client a small reply instead of a full map.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SentKey {
    /// Document pointer or PBR stage hash.
    source: u64,
    /// Edit serial or the second half of the stage hash.
    serial: u64,
    /// Parameters the map depends on beyond the serial (derived document
    /// maps), folded so a slider drag invalidates a delivered frame.
    extra: u64,
    /// Publish generation; a push invalidates everything sent before it.
    epoch: u64,
    map: MapId,
    width: u32,
    height: u32,
    bits16: bool,
}

#[derive(PartialEq, Eq, Clone)]
struct CatalogEntry {
    id: &'static str,
    label: &'static str,
    group: &'static str,
    space: &'static str,
    bits: u8,
    ready: bool,
}

impl CatalogEntry {
    fn json(&self) -> Value {
        json!({
            "id": self.id,
            "label": self.label,
            "group": self.group,
            "space": self.space,
            // Suggested sample width; the addon may ask for either.
            "bits": self.bits,
            "ready": self.ready,
        })
    }
}

enum Pending {
    Get {
        request: GetRequest,
        deadline: Instant,
    },
    Watch {
        seq: u32,
        epoch: u64,
        deadline: Instant,
    },
}

/// A mesh push in flight: the host accepted the file and is waiting for the
/// importer to report a terminal status before it answers Blender.
struct MeshAck {
    seq: u32,
    deadline: Instant,
    /// Status text before the import started, so a previous import's
    /// "Imported …" line is never mistaken for this one's result.
    previous: String,
}

struct BridgeJob {
    key: SentKey,
    seq: u32,
    map: MapId,
    frame: Option<maps::Frame>,
    /// Source extent, for the "1024 → 2048" line in the Blender panel.
    source: UVec2,
    cache: EvalCache,
    eval_ms: f32,
    /// Publish generation the client had when it asked.
    client_epoch: u64,
}

// ---------------------------------------------------------------------------
// Bevy side
// ---------------------------------------------------------------------------

fn bridge_tick(
    mut runtime: ResMut<BridgeRuntime>,
    document: Res<DocumentRes>,
    mut state: ResMut<EditorState>,
    pbr: Res<PbrRuntime>,
) {
    if !runtime.enabled || runtime.session.is_none() {
        return;
    }

    // 1. Collect a finished evaluation.
    if let Some(mut task) = runtime.eval.take() {
        match block_on(poll_once(&mut task)) {
            Some(job) => deliver(runtime.as_mut(), job),
            None => runtime.eval = Some(task),
        }
    }

    // 2. Drain what the socket threads reported.
    let commands: Vec<Command> = match runtime.commands.as_ref() {
        Some(receiver) => match receiver.lock() {
            Ok(receiver) => receiver.try_iter().take(32).collect(),
            Err(_) => return,
        },
        None => return,
    };
    for command in commands {
        match command {
            Command::Connected { client } => {
                runtime.client = Some(client.clone());
                runtime.status = format!("Blender connected: {client}");
                runtime.sent = None;
                runtime.catalog_sent = false;
            }
            Command::Closed { reason } => {
                runtime.client = None;
                runtime.sent = None;
                runtime.pending = None;
                runtime.status = format!("Listening on 127.0.0.1:{} ({reason})", runtime.port);
            }
            Command::Watch { seq, epoch, wait } => {
                // A newer watch supersedes an older one: the addon only keeps
                // one request in flight.
                if matches!(runtime.pending, Some(Pending::Watch { .. })) {
                    runtime.pending = None;
                }
                runtime.pending = Some(Pending::Watch {
                    seq,
                    epoch,
                    deadline: Instant::now() + wait,
                });
            }
            Command::PushMesh { path, name, seq } => {
                match accept_mesh(&path) {
                    Ok(accepted) => {
                        // The existing importer picks this up next frame: it
                        // builds the preview model and the UV overlay the 2D
                        // canvas draws.
                        state.import_request = Some(accepted);
                        // The overlay is off by default, and a mesh pushed from
                        // Blender is useless without it, so switch it on for
                        // both views rather than making the user hunt for it.
                        state.show_uv_overlay = true;
                        state.show_uv_overlay_2d = true;
                        runtime.mesh_ack = Some(MeshAck {
                            seq,
                            deadline: Instant::now() + Duration::from_secs(15),
                            previous: state.import_status.clone(),
                        });
                        runtime.status = format!("Importing {name} from Blender...");
                    }
                    Err(message) => {
                        runtime.status = message.clone();
                        runtime.send(
                            OP_NOTICE,
                            seq,
                            json!({"level": "error", "message": message}),
                            Vec::new(),
                        );
                    }
                }
            }
            Command::GetMap(request) => {
                // Same rule for maps: the newest request wins, and a queued
                // watch for the same client is dropped. `wait_ms` turns the
                // request into a long poll that resolves the moment the map
                // changes.
                let deadline = Instant::now() + request.wait;
                runtime.pending = Some(Pending::Get { request, deadline });
            }
        }
    }

    // 3. Report how a pushed mesh turned out. The importer works a frame or two
    //    later and can still fail (no UVs, unsupported geometry), so the answer
    //    Blender gets is the importer's own status, not an optimistic guess.
    if let Some(ack) = runtime.mesh_ack.take() {
        let status = state.import_status.clone();
        let finished = status != ack.previous;
        let failed = status.starts_with("Import failed")
            || status.starts_with("Unsupported mesh")
            || status.starts_with("Import timed out")
            || status.contains("no meshes");
        let ready = status.starts_with("Imported ");
        if finished && (failed || ready) {
            let level = if failed {
                "error"
            } else if status.contains("no UV") {
                "warn"
            } else {
                "info"
            };
            runtime.send(
                OP_NOTICE,
                ack.seq,
                json!({"level": level, "message": status.clone()}),
                Vec::new(),
            );
            runtime.status = status;
        } else if Instant::now() > ack.deadline {
            runtime.send(
                OP_NOTICE,
                ack.seq,
                json!({
                    "level": "error",
                    "message": "N-shade could not read the mesh file",
                }),
                Vec::new(),
            );
            runtime.status = "Mesh import timed out".into();
        } else {
            runtime.mesh_ack = Some(ack);
        }
    }

    // 4. Announce availability changes (the PBR pipeline filling in stages).
    let entries: Vec<CatalogEntry> = MapId::ALL
        .into_iter()
        .map(|map| CatalogEntry {
            id: map.id(),
            label: map.label(),
            group: map.group(),
            space: map.space(),
            bits: if map.prefers_16() { 16 } else { 8 },
            ready: is_available(map, &pbr),
        })
        .collect();
    if entries != runtime.catalog {
        runtime.catalog = entries;
        runtime.catalog_sent = false;
    }
    if !runtime.catalog_sent && runtime.client.is_some() {
        let payload = json!({
            "maps": runtime.catalog.iter().map(CatalogEntry::json).collect::<Vec<_>>(),
        });
        runtime.send(OP_CATALOG, 0, payload, Vec::new());
        runtime.catalog_sent = true;
    }

    // 5. Service (or keep holding) the pending request.
    service_pending(runtime.as_mut(), &document, &state, &pbr);
}

fn deliver(runtime: &mut BridgeRuntime, job: BridgeJob) {
    runtime.cache = Some(job.cache);
    runtime.last_eval = Some(Instant::now());
    runtime.last_map = Some(job.map);
    runtime.last_eval_ms = job.eval_ms;

    let Some(frame) = job.frame else {
        // Nothing was delivered, so the client does not have this key: leaving
        // `sent` alone is what makes the next request retry the evaluation.
        runtime.send(
            OP_NOTICE,
            job.seq,
            json!({
                "level": "warn",
                "message": format!("{} is not generated yet", job.map.label()),
                "map": job.map.id(),
            }),
            Vec::new(),
        );
        return;
    };

    runtime.sent = Some(job.key.clone());

    runtime.frames = runtime.frames.wrapping_add(1);
    runtime.bytes = runtime.bytes.wrapping_add(frame.payload.len() as u64);
    let mut header = json!({
        "map": job.map.id(),
        "format": frame.bits.format(),
        "w": frame.w,
        "h": frame.h,
        "space": job.map.space(),
        "src_w": job.source.x,
        "src_h": job.source.y,
        "serial": job.key.serial,
        "epoch": job.key.epoch,
        "eval_ms": (job.eval_ms * 10.0).round() / 10.0,
    });
    // A push that happened while this request was in flight is delivered with
    // the map it rode on, so manual mode and live mode both learn about it.
    if job.key.epoch > job.client_epoch {
        if let Some(object) = header.as_object_mut() {
            object.insert(
                "published".into(),
                json!(runtime
                    .published
                    .iter()
                    .map(|map| map.id())
                    .collect::<Vec<_>>()),
            );
        }
    }
    runtime.send(OP_MAP, job.seq, header, frame.payload);
}

fn service_pending(
    runtime: &mut BridgeRuntime,
    document: &DocumentRes,
    state: &EditorState,
    pbr: &PbrRuntime,
) {
    let Some(pending) = runtime.pending.take() else {
        return;
    };
    match pending {
        Pending::Watch {
            seq,
            epoch,
            deadline,
        } => {
            if runtime.publish_epoch > epoch {
                let maps: Vec<&str> = runtime.published.iter().map(|map| map.id()).collect();
                runtime.send(
                    OP_PUBLISHED,
                    seq,
                    json!({
                        "maps": maps,
                        "epoch": runtime.publish_epoch,
                        "serial": state.edit_serial,
                    }),
                    Vec::new(),
                );
            } else if Instant::now() >= deadline {
                runtime.send(
                    OP_UNCHANGED,
                    seq,
                    json!({
                        "serial": state.edit_serial,
                        "epoch": runtime.publish_epoch,
                    }),
                    Vec::new(),
                );
            } else {
                runtime.pending = Some(Pending::Watch {
                    seq,
                    epoch,
                    deadline,
                });
            }
        }
        Pending::Get { request, deadline } => {
            if runtime.eval.is_some() {
                // One evaluation at a time; the request keeps its place.
                runtime.pending = Some(Pending::Get { request, deadline });
                return;
            }
            let Some((key, source)) =
                compose(&request, document, state, pbr, runtime.publish_epoch)
            else {
                runtime.send(
                    OP_NOTICE,
                    request.seq,
                    json!({
                        "level": "warn",
                        "message": format!("{} is not generated yet", request.map.label()),
                        "map": request.map.id(),
                    }),
                    Vec::new(),
                );
                return;
            };

            let up_to_date = !request.force && runtime.sent.as_ref() == Some(&key);
            if up_to_date {
                // Hold the request open: when the key changes the client gets
                // the map, the moment a change lands. Otherwise it gets a
                // small "unchanged" and asks again.
                if Instant::now() < deadline {
                    runtime.pending = Some(Pending::Get { request, deadline });
                } else {
                    let mut header = json!({
                        "serial": key.serial,
                        "epoch": runtime.publish_epoch,
                    });
                    if runtime.publish_epoch > request.epoch {
                        if let Some(object) = header.as_object_mut() {
                            object.insert(
                                "published".into(),
                                json!(runtime
                                    .published
                                    .iter()
                                    .map(|map| map.id())
                                    .collect::<Vec<_>>()),
                            );
                        }
                    }
                    runtime.send(OP_UNCHANGED, request.seq, header, Vec::new());
                }
                return;
            }

            if let Some(last) = runtime.last_eval {
                if last.elapsed() < MIN_EVAL_INTERVAL {
                    // Rate limit: keep the request waiting rather than evaluate
                    // every frame while a brush is down.
                    let deadline = deadline.max(Instant::now() + Duration::from_millis(8));
                    runtime.pending = Some(Pending::Get { request, deadline });
                    return;
                }
            }
            begin_eval(runtime, &request, key, source);
        }
    }
}

fn begin_eval(runtime: &mut BridgeRuntime, request: &GetRequest, key: SentKey, source: JobSource) {
    let (width, height) = maps::preview_size(source.size(), request.max_size);
    let bits = request.bits;
    let map = request.map;
    let seq = request.seq;
    let client_epoch = request.epoch;
    let source_size = source.size();
    let job_key = key.clone();
    let mut cache = runtime.cache.take().unwrap_or_default();
    let task = AsyncComputeTaskPool::get().spawn(async move {
        let started = Instant::now();
        let frame = maps::render(map, &source, width, height, bits, &mut cache);
        let eval_ms = started.elapsed().as_secs_f32() * 1000.0;
        debug!(
            "bridge: {} {}x{} in {eval_ms:.1} ms",
            map.id(),
            width,
            height
        );
        BridgeJob {
            key: job_key,
            seq,
            map,
            frame,
            source: source_size,
            cache,
            eval_ms,
            client_epoch,
        }
    });
    runtime.eval = Some(task);
}

/// Builds the cache key and the job input for one request.
fn compose(
    request: &GetRequest,
    document: &DocumentRes,
    state: &EditorState,
    pbr: &PbrRuntime,
    epoch: u64,
) -> Option<(SentKey, JobSource)> {
    let (source, serial, extra) = if request.map.is_pbr() {
        let (serial, extra) = pbr_tokens(request.map, &pbr.store)?;
        (0, serial, extra)
    } else {
        let params = DocumentParams {
            wrap: document.0.wrap,
            edge_wear: state.edge_wear_params,
            edge_detect: state.edge_detect_params,
        };
        (
            document.0.ptr() as usize as u64,
            state.edit_serial,
            params.hash(),
        )
    };

    let source_size = if request.map.is_pbr() {
        pbr_size(request.map, &pbr.store)?
    } else {
        document.0.size
    };
    let (width, height) = maps::preview_size(source_size, request.max_size);

    let job = if request.map.is_pbr() {
        if request.map == MapId::PbrOrm {
            let roughness = stage_output(&pbr.store, Stage::Roughness)?;
            let metallic = stage_output(&pbr.store, Stage::Metallic)?;
            let ao = stage_output(&pbr.store, Stage::Ao);
            JobSource::Orm {
                ao,
                roughness,
                metallic,
                size: source_size,
            }
        } else {
            JobSource::Pbr {
                output: stage_output(&pbr.store, request.map.stage()?)?,
                size: source_size,
            }
        }
    } else {
        JobSource::Document {
            shared: SharedDocument::clone(&document.0),
            params: DocumentParams {
                wrap: document.0.wrap,
                edge_wear: state.edge_wear_params,
                edge_detect: state.edge_detect_params,
            },
            size: source_size,
        }
    };

    Some((
        SentKey {
            source,
            serial,
            extra,
            epoch,
            map: request.map,
            width,
            height,
            bits16: request.bits == Bits::Sixteen,
        },
        job,
    ))
}

fn stage_output(store: &MapStore, stage: Stage) -> Option<Arc<StageOutput>> {
    let key = store.current_key(stage)?;
    store.get(&key)
}

/// `(serial, extra)` for a PBR map: the stage key is a content hash, so two
/// different settings produce two different tokens without any extra state.
fn pbr_tokens(map: MapId, store: &MapStore) -> Option<(u64, u64)> {
    if map == MapId::PbrOrm {
        // The packed map depends on three stages; fold all of them so a change
        // to metallic alone still invalidates a delivered ORM frame.
        let (rough_a, rough_b) = split_key(&store.current_key(Stage::Roughness)?);
        let (metal_a, metal_b) = split_key(&store.current_key(Stage::Metallic)?);
        let (ao_a, ao_b) = store
            .current_key(Stage::Ao)
            .map(|key| split_key(&key))
            .unwrap_or((0, 0));
        return Some((rough_a ^ metal_a ^ ao_a, rough_b ^ metal_b ^ ao_b));
    }
    let key: StageKey = store.current_key(map.stage()?)?;
    Some(split_key(&key))
}

fn split_key(key: &StageKey) -> (u64, u64) {
    let mut first = [0u8; 8];
    let mut second = [0u8; 8];
    first.copy_from_slice(&key.0[0..8]);
    second.copy_from_slice(&key.0[8..16]);
    (u64::from_le_bytes(first), u64::from_le_bytes(second))
}

fn pbr_size(map: MapId, store: &MapStore) -> Option<UVec2> {
    let output = stage_output(store, map.stage()?)?;
    output_size(&output)
}

fn output_size(output: &StageOutput) -> Option<UVec2> {
    match output {
        StageOutput::Prep(prep) => Some(UVec2::new(prep.image.w, prep.image.h)),
        StageOutput::Delit(delit) => Some(UVec2::new(delit.image.w, delit.image.h)),
        StageOutput::Geometry(geometry) => Some(UVec2::new(geometry.height.w, geometry.height.h)),
        StageOutput::Displacement(displacement) => {
            Some(UVec2::new(displacement.map.w, displacement.map.h))
        }
        StageOutput::Curvature(curvature) => Some(UVec2::new(curvature.map.w, curvature.map.h)),
        StageOutput::Ao(grid)
        | StageOutput::Cavity(grid)
        | StageOutput::EdgeWear(grid)
        | StageOutput::Roughness(grid)
        | StageOutput::Metallic(grid) => Some(UVec2::new(grid.w, grid.h)),
    }
}

fn is_available(map: MapId, pbr: &PbrRuntime) -> bool {
    if !map.is_pbr() {
        return true;
    }
    if map == MapId::PbrOrm {
        return pbr_size(MapId::PbrRoughness, &pbr.store).is_some()
            && pbr_size(MapId::PbrMetallic, &pbr.store).is_some();
    }
    map.stage()
        .and_then(|stage| pbr.store.current_key(stage))
        .is_some()
}

/// Validates a mesh path a client asked us to import.
///
/// A local client is trusted to speak the protocol, not to hand the editor an
/// arbitrary path: only existing files with a mesh extension below the size cap
/// get through, so a stray or hostile frame cannot make the app read something
/// unrelated.
fn accept_mesh(path: &str) -> Result<PathBuf, String> {
    if path.trim().is_empty() {
        return Err("Blender sent no mesh path".into());
    }
    let path = PathBuf::from(path);
    if !path.is_file() {
        return Err(format!("Mesh file not found: {}", path.display()));
    }
    if !crate::import::is_mesh_path(&path) {
        return Err(format!(
            "Unsupported mesh format: {}",
            path.extension()
                .map(|ext| ext.to_string_lossy().to_string())
                .unwrap_or_else(|| "unknown".into())
        ));
    }
    match std::fs::metadata(&path) {
        Ok(meta) if meta.len() > MAX_MESH_BYTES => Err(format!(
            "Mesh is too large: {} MB",
            meta.len() / (1024 * 1024)
        )),
        Ok(_) => Ok(path.canonicalize().unwrap_or(path)),
        Err(error) => Err(format!("Could not read {}: {error}", path.display())),
    }
}

/// Every map the document (layer stack) side can serve, in menu order.
pub const DOCUMENT_MAPS: [MapId; 6] = [
    MapId::Normal,
    MapId::Height,
    MapId::Curvature,
    MapId::EdgeWear,
    MapId::EdgeDetect,
    MapId::IdMap,
];

/// Maps in the export card the app considers selected.
pub fn document_selection(state: &EditorState) -> Vec<MapId> {
    let mut maps = Vec::new();
    if state.export_normal {
        maps.push(MapId::Normal);
    }
    if state.export_height {
        maps.push(MapId::Height);
    }
    if state.export_curvature {
        maps.push(MapId::Curvature);
    }
    if state.export_edge_wear {
        maps.push(MapId::EdgeWear);
    }
    if state.export_edge_detect {
        maps.push(MapId::EdgeDetect);
    }
    if state.export_id_map {
        maps.push(MapId::IdMap);
    }
    maps
}

/// Maps the PBR export card considers selected.
pub fn pbr_selection(runtime: &PbrRuntime) -> Vec<MapId> {
    let selection = runtime.export_options.maps.unwrap_or(runtime.settings.maps);
    let mut maps = Vec::new();
    for kind in ns_pbr::MapKind::ALL {
        if !selection.enabled(kind) {
            continue;
        }
        let map = match kind {
            ns_pbr::MapKind::Normal => MapId::PbrNormal,
            ns_pbr::MapKind::Height => MapId::PbrHeight,
            ns_pbr::MapKind::Displacement => MapId::PbrDisplacement,
            ns_pbr::MapKind::Ao => MapId::PbrAo,
            ns_pbr::MapKind::Cavity => MapId::PbrCavity,
            ns_pbr::MapKind::Curvature => MapId::PbrCurvature,
            ns_pbr::MapKind::EdgeWear => MapId::PbrEdgeWear,
            ns_pbr::MapKind::Roughness => MapId::PbrRoughness,
            ns_pbr::MapKind::Metallic => MapId::PbrMetallic,
            ns_pbr::MapKind::Delit => MapId::PbrBaseColor,
        };
        maps.push(map);
    }
    maps
}

/// PBR maps that have finished generating, regardless of the export selection.
pub fn pbr_available(runtime: &PbrRuntime) -> Vec<MapId> {
    MapId::ALL
        .into_iter()
        .filter(|map| map.is_pbr() && is_available(*map, runtime))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn frame_bytes(op: u8, seq: u32, header: &Value, payload: &[u8]) -> Vec<u8> {
        let head = serde_json::to_vec(header).unwrap();
        let mut out = framing(op, seq, head.len(), payload.len()).to_vec();
        out.extend_from_slice(&head);
        out.extend_from_slice(payload);
        out
    }

    #[test]
    fn request_frames_round_trip() {
        let bytes = frame_bytes(
            OP_GET_MAP,
            7,
            &json!({ "map": "height", "max_size": 1024 }),
            &[],
        );
        let mut reader = FrameReader::new();
        reader.buffer.extend_from_slice(&bytes);
        let (op, seq, header) = reader.take().unwrap().unwrap();
        assert_eq!(op, OP_GET_MAP);
        assert_eq!(seq, 7);
        assert_eq!(header["map"], "height");
        assert_eq!(header["max_size"], 1024);
        assert!(reader.buffer.is_empty(), "the whole frame is consumed");
    }

    #[test]
    fn partial_frames_wait_for_the_rest() {
        let bytes = frame_bytes(OP_PING, 3, &json!({ "t": 1.0 }), &[]);
        let mut reader = FrameReader::new();
        reader.buffer.extend_from_slice(&bytes[..6]);
        assert!(reader.take().unwrap().is_none());
        reader.buffer.extend_from_slice(&bytes[6..]);
        let (op, seq, _header) = reader.take().unwrap().unwrap();
        assert_eq!(op, OP_PING);
        assert_eq!(seq, 3);
    }

    #[test]
    fn oversized_and_foreign_frames_are_rejected() {
        let mut reader = FrameReader::new();
        let mut bad = framing(OP_HELLO, 0, 0, 0).to_vec();
        bad[0] = b'X';
        reader.buffer.extend_from_slice(&bad);
        assert!(reader.take().is_err());

        let mut reader = FrameReader::new();
        reader
            .buffer
            .extend_from_slice(&framing(OP_HELLO, 0, 0, MAX_INBOUND_PAYLOAD + 1));
        assert!(reader.take().is_err());
    }

    #[test]
    fn get_requests_are_clamped_and_parsed() {
        let request = parse_get(
            &json!({
                "map": "pbr_height",
                "max_size": 100_000,
                "bits": 16,
                "wait_ms": 10_000,
                "force": true,
            }),
            4,
        )
        .expect("known map");
        assert_eq!(request.map, MapId::PbrHeight);
        assert_eq!(request.max_size, maps::MAX_SIDE);
        assert_eq!(request.bits, Bits::Sixteen);
        assert_eq!(request.wait, MAX_WAIT);
        assert!(request.force);
        assert_eq!(request.seq, 4);

        assert!(parse_get(&json!({ "map": "nope" }), 0).is_none());
        // A missing map id would silently preview the normal map in v1; v2
        // rejects the frame instead.
        assert!(parse_get(&json!({}), 0).is_none());
    }

    #[test]
    fn keys_ignore_nothing_that_changes_pixels() {
        let base = SentKey {
            source: 1,
            serial: 2,
            extra: 3,
            epoch: 4,
            map: MapId::Height,
            width: 8,
            height: 8,
            bits16: false,
        };
        assert_eq!(base, base.clone());
        for mutated in [
            SentKey {
                source: 9,
                ..base.clone()
            },
            SentKey {
                serial: 9,
                ..base.clone()
            },
            SentKey {
                extra: 9,
                ..base.clone()
            },
            SentKey {
                epoch: 9,
                ..base.clone()
            },
            SentKey {
                map: MapId::Normal,
                ..base.clone()
            },
            SentKey {
                width: 9,
                ..base.clone()
            },
            SentKey {
                height: 9,
                ..base.clone()
            },
            SentKey {
                bits16: true,
                ..base.clone()
            },
        ] {
            assert_ne!(base, mutated);
        }
    }

    #[test]
    fn framing_is_exactly_sixteen_little_endian_bytes() {
        // Pinned byte-for-byte because the Python side builds the same frame
        // with struct.pack("<4sBBHII"); see protocol.py's matching test.
        let bytes = framing(OP_GET_MAP, 7, 0, 3);
        assert_eq!(
            bytes.to_vec(),
            vec![
                0x51, 0x58, 0x4C, 0x31, // "QXL1"
                0x10, // GET_MAP
                0x00, // flags
                0x00, 0x00, // header length
                0x03, 0x00, 0x00, 0x00, // payload length
                0x07, 0x00, 0x00, 0x00, // seq
            ]
        );
        assert_eq!(bytes.len(), HEADER_SIZE);
    }

    #[test]
    fn tokens_are_32_hex_chars_and_differ() {
        let first = new_token().unwrap();
        let second = new_token().unwrap();
        assert_eq!(first.len(), 32);
        assert!(first.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(first, second);
    }

    #[test]
    fn the_pairing_file_is_atomic_and_private_on_unix() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bridge.json");
        write_config_at(&path, 1234, "token").unwrap();

        let value: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(value["token"], "token");
        assert_eq!(value["port"], 1234);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn catalog_entries_describe_every_map() {
        let entries: Vec<Value> = MapId::ALL
            .into_iter()
            .map(|map| {
                CatalogEntry {
                    id: map.id(),
                    label: map.label(),
                    group: map.group(),
                    space: map.space(),
                    bits: if map.prefers_16() { 16 } else { 8 },
                    ready: false,
                }
                .json()
            })
            .collect();
        assert_eq!(entries.len(), MapId::ALL.len());
        assert!(entries.iter().all(|entry| entry["id"].is_string()));
        assert_eq!(entries[0]["group"], "document");
    }
}
