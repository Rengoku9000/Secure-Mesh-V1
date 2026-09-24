//! Phase 1 LoRa transport: a diagnostic-only sibling to [`super::libp2p_transport`].
//!
//! # Scope
//!
//! This is deliberately **not** a peer-authenticated [`MeshTransport`] in the
//! sense the trait's own documentation requires (proof of key possession,
//! encrypted transit). A raw E22 UART link has neither, and building a
//! handshake over it is out of scope for Phase 1. So this implementation is
//! conservative on purpose:
//!
//! - [`LoraTransport::send`] (the trait method) always fails — the existing
//!   signed JSON [`Envelope`] is never put on the air here. Real sync traffic
//!   keeps flowing over QUIC exclusively.
//! - [`LoraTransport::connected_peers`] and [`LoraTransport::poll_events`]
//!   always return empty. Nothing surfaces a `PeerConnected` over LoRa yet,
//!   so the sync engine never mistakes an E22 link for an authenticated peer.
//!
//! The actual Phase 1 capability — send/receive a small binary diagnostic
//! frame — lives in the inherent methods [`LoraTransport::send_diagnostic`]
//! and [`LoraTransport::poll_diagnostic_frames`], entirely outside the
//! `MeshTransport` trait. This is what the integration plan called "an
//! explicit/test-only LoRa send path".
//!
//! # Frame format
//!
//! A fixed binary layout, independent of the QUIC wire format:
//!
//! ```text
//! magic (4)  version (1)  message_type (1)  source_node_id (32)
//! sequence (4)  payload_length (1)  payload (0..=200)  crc32 (4)
//! ```
//!
//! `source_node_id` is the 32 raw bytes behind a SecureMesh node ID (it is
//! `SHA-256(public key)` as hex; this stores the decoded bytes rather than the
//! 64-character hex string, which alone saves 32 bytes per frame — real
//! estate that matters against an E22 240-byte sub-packet). `payload_length`
//! is a single byte, so [`MAX_LORA_PAYLOAD_BYTES`] is capped at 200: with the
//! 43-byte header and 4-byte CRC, that keeps the largest possible frame
//! (247 bytes) close to, and typical diagnostic frames well under, one
//! sub-packet.
//!
//! # Failure isolation
//!
//! [`LoraTransport::from_env`] never returns an error. Absent configuration,
//! a serial port that will not open, and a device that vanishes mid-session
//! are all folded into the same outcome: no `LoraTransport` exists, or (once
//! constructed) its send/receive calls quietly do nothing further. Nothing
//! here can prevent QUIC from starting or keep running.

use super::protocol::Envelope;
use super::{MeshEvent, MeshTransport, PeerDescriptor};
use crate::error::{CoreError, CoreResult};
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Environment variable naming the serial device. Absent means "no LoRa this
/// run", not an error.
pub const SECUREMESH_LORA_SERIAL_ENV: &str = "SECUREMESH_LORA_SERIAL";

/// Matches the verified bench configuration (9600 8N1, no flow control).
pub const DEFAULT_BAUD_RATE: u32 = 9600;

/// 4-byte frame marker. Chosen to be vanishingly unlikely to occur by chance
/// in arbitrary serial noise, so resynchronisation after a corrupted frame is
/// cheap.
pub const LORA_FRAME_MAGIC: [u8; 4] = *b"SMLR";

/// Bumped whenever the frame layout changes incompatibly.
pub const LORA_FRAME_VERSION: u8 = 1;

/// Fixed header size in bytes: magic(4) + version(1) + message_type(1) +
/// source_node_id(32) + sequence(4) + payload_length(1).
const HEADER_LEN: usize = 4 + 1 + 1 + 32 + 4 + 1;

/// CRC width in bytes.
const CRC_LEN: usize = 4;

/// Largest payload one frame may carry. See the module docs for the sizing
/// rationale against the E22's 240-byte sub-packet.
pub const MAX_LORA_PAYLOAD_BYTES: usize = 200;

/// Largest total frame size Phase 3A's compact SecureMesh event encoding
/// (`lora_event_codec`) may produce, in bytes.
///
/// Deliberately tighter than the 247-byte ceiling [`MAX_LORA_PAYLOAD_BYTES`]
/// allows for a generic frame: an event frame must stay strictly under the
/// E22's 240-byte sub-packet, with a byte to spare, rather than merely "close
/// to" it. 239 = `HEADER_LEN` (43) + [`MAX_SECUREMESH_EVENT_PAYLOAD_BYTES`]
/// (192) + `CRC_LEN` (4).
pub const MAX_SECUREMESH_EVENT_FRAME_BYTES: usize = 239;

/// Largest payload a `SecureMeshEvent` frame may carry — derived from
/// [`MAX_SECUREMESH_EVENT_FRAME_BYTES`] by subtracting this frame format's
/// fixed overhead, not chosen independently of it.
pub const MAX_SECUREMESH_EVENT_PAYLOAD_BYTES: usize =
    MAX_SECUREMESH_EVENT_FRAME_BYTES - HEADER_LEN - CRC_LEN;

/// What kind of message a frame carries.
///
/// `Diagnostic` is the Phase 1 capability: a small operator/test payload,
/// never a SecureMesh envelope. `SecureMeshEvent` is Phase 3A's compact,
/// already-signed event encoding (see `lora_event_codec`) — added here only
/// as a recognised frame type; nothing in this module or
/// [`MeshTransport`](super::MeshTransport) treats it specially yet. The field
/// is a full byte rather than a bool so later kinds need no format version
/// bump.
///
/// `SyncRequest` (Phase 6) carries a signed `lora_sync` payload asking a peer
/// for events this node is missing. It is recognised and queued here only;
/// nothing in this module interprets it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoraMessageType {
    Diagnostic = 1,
    SecureMeshEvent = 2,
    SyncRequest = 3,
}

impl LoraMessageType {
    fn from_u8(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::Diagnostic),
            2 => Some(Self::SecureMeshEvent),
            3 => Some(Self::SyncRequest),
            _ => None,
        }
    }
}

/// Most `SyncRequest` frames held awaiting collection. Arrivals beyond this
/// are dropped rather than displacing queued ones, so a burst of requests —
/// including forged ones, which are only rejected later at verification —
/// cannot grow memory without bound.
pub const MAX_QUEUED_SYNC_REQUESTS: usize = 16;

/// A decoded, CRC-verified Phase 1 LoRa frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoraFrame {
    pub message_type: LoraMessageType,
    /// Raw 32 bytes behind the sender's SecureMesh node ID.
    pub source_node_id: [u8; 32],
    /// Per-sender monotonic counter. A frame-transport concept only: it is
    /// unrelated to the domain event log's `origin_seq`.
    pub sequence: u32,
    pub payload: Vec<u8>,
}

impl LoraFrame {
    /// Encodes this frame to its wire bytes, appending the trailing CRC.
    pub fn encode(&self) -> CoreResult<Vec<u8>> {
        if self.payload.len() > MAX_LORA_PAYLOAD_BYTES {
            return Err(CoreError::validation(
                "LoRa payload exceeds the frame's maximum size",
            ));
        }

        let mut buffer = Vec::with_capacity(HEADER_LEN + self.payload.len() + CRC_LEN);
        buffer.extend_from_slice(&LORA_FRAME_MAGIC);
        buffer.push(LORA_FRAME_VERSION);
        buffer.push(self.message_type as u8);
        buffer.extend_from_slice(&self.source_node_id);
        buffer.extend_from_slice(&self.sequence.to_be_bytes());
        buffer.push(self.payload.len() as u8);
        buffer.extend_from_slice(&self.payload);

        let crc = crc32(&buffer);
        buffer.extend_from_slice(&crc.to_be_bytes());
        Ok(buffer)
    }

    /// Decodes and fully validates a frame read from the serial link.
    ///
    /// Checks run cheapest-first — length, magic, version, then CRC — so
    /// garbage on the wire is rejected before any of it is trusted.
    pub fn decode(bytes: &[u8]) -> CoreResult<Self> {
        if bytes.len() < HEADER_LEN + CRC_LEN {
            return Err(CoreError::validation("LoRa frame is too short"));
        }

        if bytes[0..4] != LORA_FRAME_MAGIC {
            return Err(CoreError::validation("LoRa frame has an invalid magic"));
        }

        if bytes[4] != LORA_FRAME_VERSION {
            return Err(CoreError::validation(
                "LoRa frame has an unsupported version",
            ));
        }

        let message_type = LoraMessageType::from_u8(bytes[5])
            .ok_or_else(|| CoreError::validation("LoRa frame has an unknown message type"))?;

        let mut source_node_id = [0u8; 32];
        source_node_id.copy_from_slice(&bytes[6..38]);

        let sequence = u32::from_be_bytes(bytes[38..42].try_into().unwrap());
        let payload_len = bytes[42] as usize;

        let expected_total = HEADER_LEN + payload_len + CRC_LEN;
        if bytes.len() != expected_total {
            return Err(CoreError::validation(
                "LoRa frame length does not match its declared payload length",
            ));
        }

        let payload = bytes[HEADER_LEN..HEADER_LEN + payload_len].to_vec();

        let crc_offset = HEADER_LEN + payload_len;
        let expected_crc =
            u32::from_be_bytes(bytes[crc_offset..crc_offset + CRC_LEN].try_into().unwrap());
        let actual_crc = crc32(&bytes[..crc_offset]);
        if actual_crc != expected_crc {
            return Err(CoreError::validation("LoRa frame failed its CRC check"));
        }

        Ok(Self {
            message_type,
            source_node_id,
            sequence,
            payload,
        })
    }
}

/// CRC-32 (IEEE 802.3 polynomial, reflected), computed bit by bit.
///
/// No lookup table: frames here are at most a couple hundred bytes and this
/// runs at most a few times a second, so the table's memory and setup cost
/// buys nothing. Verified against the standard check value for `"123456789"`
/// in the unit tests below.
fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// Commands sent from the calling thread into the serial I/O thread.
enum LoraCommand {
    Send(Vec<u8>),
    Shutdown,
}

/// Frames waiting to be collected, kept in two separate queues by
/// [`LoraMessageType`] so a caller asking for one kind can never be handed
/// the other.
///
/// This is what stops a `SecureMeshEvent` frame from silently reaching
/// [`LoraTransport::poll_diagnostic_frames`] (or a `Diagnostic` frame from
/// reaching [`LoraTransport::poll_event_frames`]) if both ever arrive on the
/// same link — the split happens once, at the point a frame is decoded off
/// the wire, not at each call site.
#[derive(Default)]
struct LoraInboxes {
    diagnostic: Vec<LoraFrame>,
    event: Vec<LoraFrame>,
    /// FIFO, bounded by [`MAX_QUEUED_SYNC_REQUESTS`].
    sync_request: VecDeque<LoraFrame>,
}

/// A Phase 1 LoRa transport, backed by a real serial device.
///
/// See the module docs for why this is intentionally inert as a
/// [`MeshTransport`] — the useful surface is
/// [`send_diagnostic`](Self::send_diagnostic),
/// [`poll_diagnostic_frames`](Self::poll_diagnostic_frames), and (Phase 3A)
/// [`poll_event_frames`](Self::poll_event_frames).
pub struct LoraTransport {
    local_node_id: String,
    commands: Sender<LoraCommand>,
    inboxes: Arc<Mutex<LoraInboxes>>,
    sequence: AtomicU32,
}

impl LoraTransport {
    /// Opens a serial device and starts its background I/O thread.
    ///
    /// The path is a caller-supplied parameter, never hardcoded — on Linux
    /// this is typically a `/dev/serial/by-id/...` path, on Windows a `COMn`
    /// name. See [`Self::from_env`] for the configuration-driven entry point.
    pub fn open(path: &str, local_node_id: String, baud_rate: u32) -> CoreResult<Self> {
        let port = serialport::new(path, baud_rate)
            .data_bits(serialport::DataBits::Eight)
            .parity(serialport::Parity::None)
            .stop_bits(serialport::StopBits::One)
            .flow_control(serialport::FlowControl::None)
            .timeout(Duration::from_millis(200))
            .open()
            .map_err(|error| {
                CoreError::internal(format!("could not open LoRa serial device {path}: {error}"))
            })?;

        eprintln!("[securemesh] lora initialized");
        eprintln!("[securemesh] lora serial device={path}");

        Ok(Self::spawn(port, local_node_id))
    }

    /// Spawns the I/O thread over any duplex byte stream and returns the
    /// transport. Shared by [`Self::open`] (a real serial port) and the
    /// `#[cfg(test)]` mock constructor below, so both paths run the exact
    /// same framing and threading logic.
    fn spawn<IO>(io: IO, local_node_id: String) -> Self
    where
        IO: Read + Write + Send + 'static,
    {
        let (command_tx, command_rx) = mpsc::channel();
        let inboxes = Arc::new(Mutex::new(LoraInboxes::default()));
        let thread_inboxes = Arc::clone(&inboxes);

        // Best effort, matching `Libp2pTransport`'s own thread: if the OS
        // cannot even start a thread, the transport still exists, and
        // send/poll simply have nothing to talk to rather than making this a
        // second, harder-to-explain way for LoRa to be "unavailable".
        let _ = std::thread::Builder::new()
            .name("securemesh-lora".to_string())
            .spawn(move || run_lora_io(io, command_rx, thread_inboxes));

        Self {
            local_node_id,
            commands: command_tx,
            inboxes,
            sequence: AtomicU32::new(0),
        }
    }

    /// Test-only entry point: builds a transport over any mock duplex stream,
    /// skipping `serialport` entirely so frame encoding, threading, and the
    /// diagnostic send path are exercised without physical hardware.
    #[cfg(test)]
    pub(crate) fn open_with_io<IO>(io: IO, local_node_id: String) -> Self
    where
        IO: Read + Write + Send + 'static,
    {
        Self::spawn(io, local_node_id)
    }

    /// Builds a transport from [`SECUREMESH_LORA_SERIAL`], or returns `None`.
    ///
    /// `None` covers both "the variable is not set" and "it is set but the
    /// device could not be opened" — both mean the same thing to a caller:
    /// proceed without LoRa. The latter case is logged so the condition is
    /// visible without being fatal.
    ///
    /// [`SECUREMESH_LORA_SERIAL`]: SECUREMESH_LORA_SERIAL_ENV
    pub fn from_env(local_node_id: String) -> Option<Self> {
        let Some(path) = std::env::var(SECUREMESH_LORA_SERIAL_ENV).ok() else {
            eprintln!("[securemesh] lora unavailable reason={SECUREMESH_LORA_SERIAL_ENV} not set");
            return None;
        };
        match Self::open(&path, local_node_id, DEFAULT_BAUD_RATE) {
            Ok(transport) => Some(transport),
            Err(error) => {
                eprintln!("[securemesh] lora unavailable reason={error}");
                None
            }
        }
    }

    /// This node's own node ID, decoded to the raw 32 bytes a `source_node_id`
    /// field holds. Shared by every LoRa send path so the decode/length check
    /// exists in exactly one place.
    fn local_source_node_id(&self) -> CoreResult<[u8; 32]> {
        let decoded = hex::decode(&self.local_node_id)
            .map_err(|_| CoreError::internal("local node id is not valid hex"))?;
        if decoded.len() != 32 {
            return Err(CoreError::internal(
                "local node id does not decode to 32 bytes",
            ));
        }
        let mut source_node_id = [0u8; 32];
        source_node_id.copy_from_slice(&decoded);
        Ok(source_node_id)
    }

    /// Sends a small diagnostic payload as a Phase 1 LoRa frame.
    ///
    /// This is the "explicit/test-only" path: it never carries a
    /// SecureMesh [`Envelope`] and is never called by the sync engine.
    pub fn send_diagnostic(&self, payload: &[u8]) -> CoreResult<()> {
        let frame = LoraFrame {
            message_type: LoraMessageType::Diagnostic,
            source_node_id: self.local_source_node_id()?,
            sequence: self.sequence.fetch_add(1, Ordering::Relaxed),
            payload: payload.to_vec(),
        };

        let bytes = frame.encode()?;
        eprintln!("[securemesh] lora diagnostic send bytes={}", bytes.len());
        self.commands
            .send(LoraCommand::Send(bytes))
            .map_err(|_| CoreError::internal("the LoRa I/O thread is no longer running"))
    }

    /// Sends an already-encoded `SecureMeshEvent` payload as a Phase 3A LoRa
    /// frame.
    ///
    /// `payload` must already be the output of
    /// [`super::lora_event_codec::encode`] — this performs no encoding, no
    /// signing, and no size decision of its own. [`LoraFrame::encode`]'s own
    /// size check (against [`MAX_LORA_PAYLOAD_BYTES`]) is only the last line
    /// of defense: the codec itself already refuses anything over
    /// [`MAX_SECUREMESH_EVENT_PAYLOAD_BYTES`], which is the tighter of the
    /// two limits, before this is ever reached. Nothing here truncates or
    /// fragments an oversized payload — it is refused, exactly like
    /// `LoraFrame::encode` refuses one that is merely over the generic limit.
    ///
    /// Shares this transport's frame sequence counter with
    /// [`Self::send_diagnostic`]: the counter is a frame-transport concept
    /// only (see [`LoraFrame::sequence`]'s own doc comment), unrelated to the
    /// domain event log's `origin_seq`, so there is no reason to keep two.
    pub fn send_event_payload(&self, payload: &[u8]) -> CoreResult<()> {
        let frame = LoraFrame {
            message_type: LoraMessageType::SecureMeshEvent,
            source_node_id: self.local_source_node_id()?,
            sequence: self.sequence.fetch_add(1, Ordering::Relaxed),
            payload: payload.to_vec(),
        };

        let bytes = frame.encode()?;
        eprintln!("[securemesh] lora event send bytes={}", bytes.len());
        self.commands
            .send(LoraCommand::Send(bytes))
            .map_err(|_| CoreError::internal("the LoRa I/O thread is no longer running"))
    }

    /// Takes any diagnostic frames received since the last call.
    ///
    /// Never includes a `SecureMeshEvent` frame — see [`LoraInboxes`].
    pub fn poll_diagnostic_frames(&self) -> Vec<LoraFrame> {
        std::mem::take(&mut self.lock_inboxes().diagnostic)
    }

    /// Takes any `SecureMeshEvent` frames received since the last call.
    ///
    /// Never includes a `Diagnostic` frame, and — unlike
    /// [`MeshTransport::poll_events`] — never surfaces as an authenticated
    /// peer event. This is Phase 3A's own receive path: a caller (the
    /// runtime) is expected to hand each frame to
    /// [`super::lora_event_ingest::ingest_event_frame`], which is where trust
    /// and signature checks actually happen. This method performs no
    /// authentication itself; it only separates event-shaped frames from
    /// diagnostic ones.
    pub fn poll_event_frames(&self) -> Vec<LoraFrame> {
        std::mem::take(&mut self.lock_inboxes().event)
    }

    /// Sends an already-signed `SyncRequest` payload as a Phase 6 LoRa frame.
    ///
    /// `payload` must be the output of [`super::lora_sync::encode`]. Framing
    /// only — no signing, no policy. A payload of the wrong size is refused
    /// here, since every receiver would reject it anyway.
    pub fn send_sync_request(&self, payload: &[u8]) -> CoreResult<()> {
        if payload.len() != super::lora_sync::SYNC_REQUEST_PAYLOAD_BYTES {
            return Err(CoreError::validation(
                "LoRa sync request payload has the wrong size",
            ));
        }

        let frame = LoraFrame {
            message_type: LoraMessageType::SyncRequest,
            source_node_id: self.local_source_node_id()?,
            sequence: self.sequence.fetch_add(1, Ordering::Relaxed),
            payload: payload.to_vec(),
        };

        let bytes = frame.encode()?;
        eprintln!("[securemesh] lora sync request send bytes={}", bytes.len());
        self.commands
            .send(LoraCommand::Send(bytes))
            .map_err(|_| CoreError::internal("the LoRa I/O thread is no longer running"))
    }

    /// Takes up to `max` `SyncRequest` frames, oldest first. Anything beyond
    /// `max` stays queued for the next call.
    ///
    /// Never includes a `Diagnostic` or `SecureMeshEvent` frame. Performs no
    /// authentication — see [`super::lora_sync::verify`].
    pub fn poll_sync_requests(&self, max: usize) -> Vec<LoraFrame> {
        let mut inboxes = self.lock_inboxes();
        let take = max.min(inboxes.sync_request.len());
        inboxes.sync_request.drain(..take).collect()
    }

    fn lock_inboxes(&self) -> std::sync::MutexGuard<'_, LoraInboxes> {
        self.inboxes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl Drop for LoraTransport {
    fn drop(&mut self) {
        // Best effort, mirroring `Libp2pTransport`: if the I/O thread already
        // exited, there is nothing left to stop.
        let _ = self.commands.send(LoraCommand::Shutdown);
    }
}

/// Deliberately inert as a [`MeshTransport`]. See the module docs.
impl MeshTransport for LoraTransport {
    fn local_node_id(&self) -> String {
        self.local_node_id.clone()
    }

    fn send(&self, _to: &str, _envelope: &Envelope) -> CoreResult<()> {
        Err(CoreError::internal(
            "LoRa does not carry SecureMesh sync envelopes in Phase 1",
        ))
    }

    fn connected_peers(&self) -> Vec<PeerDescriptor> {
        Vec::new()
    }

    fn poll_events(&self) -> Vec<MeshEvent> {
        Vec::new()
    }
}

/// The serial I/O loop, run on its own thread.
///
/// Isolated here so a slow, stalled, or erroring serial device can never
/// block the caller: reads use a short timeout and any I/O error is logged
/// and retried, never propagated or panicked on.
fn run_lora_io<IO>(mut port: IO, commands: Receiver<LoraCommand>, inboxes: Arc<Mutex<LoraInboxes>>)
where
    IO: Read + Write,
{
    let mut read_buf = [0u8; 256];
    let mut pending: Vec<u8> = Vec::new();

    loop {
        match commands.try_recv() {
            Ok(LoraCommand::Send(bytes)) => {
                if let Err(error) = port.write_all(&bytes) {
                    eprintln!("[securemesh] LoRa write failed: {error}");
                }
            }
            Ok(LoraCommand::Shutdown) => return,
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => return,
        }

        match port.read(&mut read_buf) {
            Ok(0) => {}
            Ok(n) => {
                pending.extend_from_slice(&read_buf[..n]);
                drain_frames(&mut pending, &inboxes);
            }
            Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {}
            Err(error) => {
                eprintln!("[securemesh] LoRa read failed: {error}");
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }
}

/// Extracts every complete, decodable frame from the front of `pending`,
/// leaving any trailing partial frame for the next read.
///
/// Resynchronises on corruption by scanning forward for the next magic word,
/// so one bad frame does not wedge the link.
///
/// Routes each decoded frame into its own queue by [`LoraMessageType`] — see
/// [`LoraInboxes`] — so `Diagnostic` and `SecureMeshEvent` traffic never
/// mixes, regardless of the order frames actually arrive in.
fn drain_frames(pending: &mut Vec<u8>, inboxes: &Arc<Mutex<LoraInboxes>>) {
    loop {
        if pending.len() < 4 {
            return;
        }

        if pending[0..4] != LORA_FRAME_MAGIC {
            match pending.windows(4).position(|w| w == LORA_FRAME_MAGIC) {
                Some(offset) => {
                    pending.drain(0..offset);
                    continue;
                }
                None => {
                    // No magic anywhere in the buffer. Keep the last 3 bytes
                    // in case a magic word is split across two reads.
                    let keep_from = pending.len().saturating_sub(3);
                    pending.drain(0..keep_from);
                    return;
                }
            }
        }

        if pending.len() < HEADER_LEN {
            return; // Need more bytes before the payload length is even known.
        }

        let payload_len = pending[42] as usize;
        let total_len = HEADER_LEN + payload_len + CRC_LEN;
        if pending.len() < total_len {
            return; // Frame not fully arrived yet.
        }

        let frame_bytes: Vec<u8> = pending.drain(0..total_len).collect();
        match LoraFrame::decode(&frame_bytes) {
            Ok(frame) => {
                let mut guard = inboxes
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                match frame.message_type {
                    LoraMessageType::Diagnostic => guard.diagnostic.push(frame),
                    LoraMessageType::SecureMeshEvent => guard.event.push(frame),
                    LoraMessageType::SyncRequest => {
                        if guard.sync_request.len() < MAX_QUEUED_SYNC_REQUESTS {
                            guard.sync_request.push_back(frame);
                        } else {
                            eprintln!("[securemesh] dropped a LoRa sync request: queue full");
                        }
                    }
                }
            }
            Err(error) => {
                eprintln!("[securemesh] discarded an invalid LoRa frame: {error}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_frame() -> LoraFrame {
        LoraFrame {
            message_type: LoraMessageType::Diagnostic,
            source_node_id: [0xAB; 32],
            sequence: 7,
            payload: b"hello-lora".to_vec(),
        }
    }

    #[test]
    fn crc32_matches_the_standard_check_value() {
        // The well-known CRC-32/ISO-HDLC check value for the ASCII string
        // "123456789", used across implementations to catch a wrong
        // polynomial or bit order rather than just self-consistency.
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn a_frame_round_trips_through_encode_and_decode() {
        let frame = sample_frame();
        let bytes = frame.encode().unwrap();
        let decoded = LoraFrame::decode(&bytes).unwrap();
        assert_eq!(decoded, frame);
    }

    #[test]
    fn an_empty_payload_round_trips() {
        let frame = LoraFrame {
            payload: Vec::new(),
            ..sample_frame()
        };
        let bytes = frame.encode().unwrap();
        assert_eq!(LoraFrame::decode(&bytes).unwrap(), frame);
    }

    #[test]
    fn a_maximum_size_payload_round_trips() {
        let frame = LoraFrame {
            payload: vec![0x42; MAX_LORA_PAYLOAD_BYTES],
            ..sample_frame()
        };
        let bytes = frame.encode().unwrap();
        assert_eq!(LoraFrame::decode(&bytes).unwrap(), frame);
    }

    #[test]
    fn a_payload_over_the_limit_is_refused_at_encode_time() {
        let frame = LoraFrame {
            payload: vec![0x00; MAX_LORA_PAYLOAD_BYTES + 1],
            ..sample_frame()
        };
        let err = frame.encode().unwrap_err();
        assert!(err.message().contains("maximum size"));
    }

    #[test]
    fn an_invalid_magic_is_rejected() {
        let mut bytes = sample_frame().encode().unwrap();
        bytes[0] = b'X';
        let err = LoraFrame::decode(&bytes).unwrap_err();
        assert!(err.message().contains("invalid magic"));
    }

    #[test]
    fn an_invalid_version_is_rejected() {
        let mut bytes = sample_frame().encode().unwrap();
        bytes[4] = LORA_FRAME_VERSION + 1;
        let err = LoraFrame::decode(&bytes).unwrap_err();
        assert!(err.message().contains("unsupported version"));
    }

    #[test]
    fn a_length_mismatch_is_rejected() {
        // Declares a longer payload than actually follows.
        let mut bytes = sample_frame().encode().unwrap();
        bytes[42] = bytes[42].wrapping_add(1);
        let err = LoraFrame::decode(&bytes).unwrap_err();
        assert!(err.message().contains("length"));
    }

    #[test]
    fn a_truncated_frame_is_rejected() {
        let bytes = sample_frame().encode().unwrap();
        let err = LoraFrame::decode(&bytes[..bytes.len() - 1]).unwrap_err();
        assert!(err.message().contains("length") || err.message().contains("too short"));
    }

    #[test]
    fn a_corrupted_crc_is_rejected() {
        let mut bytes = sample_frame().encode().unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xFF;
        let err = LoraFrame::decode(&bytes).unwrap_err();
        assert!(err.message().contains("CRC"));
    }

    #[test]
    fn a_corrupted_payload_byte_is_caught_by_the_crc() {
        let mut bytes = sample_frame().encode().unwrap();
        bytes[HEADER_LEN] ^= 0x01; // first payload byte
        let err = LoraFrame::decode(&bytes).unwrap_err();
        assert!(err.message().contains("CRC"));
    }

    #[test]
    fn an_unknown_message_type_is_rejected() {
        let mut bytes = sample_frame().encode().unwrap();
        bytes[5] = 0xFF; // corrupt the type byte
                         // Recompute the CRC so this test isolates the type check, not CRC.
        let crc = crc32(&bytes[..bytes.len() - CRC_LEN]);
        let crc_start = bytes.len() - CRC_LEN;
        bytes[crc_start..].copy_from_slice(&crc.to_be_bytes());

        let err = LoraFrame::decode(&bytes).unwrap_err();
        assert!(err.message().contains("message type"));
    }

    /// A real `SecureMeshEvent` frame captured off the air during a bench
    /// test. This is a pure transport-level regression test: it proves only
    /// that this frame decodes, not that its payload is a valid compact
    /// event (that is `lora_event_codec`'s job) or that it would pass
    /// signature/trust checks (that is `lora_event_ingest`'s job, not yet
    /// wired to this frame type). No serial device, trust store, or database
    /// is touched.
    #[test]
    fn a_captured_securemesh_event_frame_decodes_correctly() {
        const CAPTURED_HEX: &str = "53 4D 4C 52 01 02 3B 0A F6 F3 E0 7B B3 AA 0E 9D 3D D1 1F D0 18 8B 64 C5 10 2E A0 CE 5F 72 A1 19 9C BA 9C 13 92 A6 00 00 00 00 8E 01 01 81 5F F0 67 4A 6A 4C 8A 97 88 47 3E 00 83 5F 9E 00 00 00 00 00 00 00 02 00 00 01 A0 D1 13 A5 F7 0B 35 0C 39 F0 6E 9D B2 ED 8B 51 23 46 08 1E F2 A4 56 C7 25 66 CA D9 41 1A 5F 3E A7 3D 6D 1A 78 F7 22 01 E5 37 17 50 29 88 A3 F1 AD 96 51 DE 19 A4 28 17 29 F0 48 C5 EC AB CF 1D BA A6 E2 7C 0D 47 0F 17 10 98 73 47 1B A9 6E 18 CD C7 82 EA 66 01 03 00 18 4C 6F 52 61 20 52 46 20 69 6E 74 65 67 72 61 74 69 6F 6E 20 74 65 73 74 49 B5 56 77";

        let bytes = hex::decode(CAPTURED_HEX.replace(' ', "")).unwrap();
        assert_eq!(bytes.len(), 189, "captured fixture must be 189 bytes");

        // Magic and version, checked directly against the raw bytes as well
        // as implicitly by `decode` succeeding at all.
        assert_eq!(&bytes[0..4], &LORA_FRAME_MAGIC);
        assert_eq!(bytes[4], LORA_FRAME_VERSION);
        assert_eq!(bytes[42], 142, "declared payload_length byte");

        let frame = LoraFrame::decode(&bytes).expect("a genuine captured frame must decode");

        assert_eq!(frame.message_type, LoraMessageType::SecureMeshEvent);
        // The 32-byte node ID as it actually appears in the captured frame
        // (bytes 6..38). Note this is one hex nibble longer than the
        // "3b0a...392a" value quoted in the accompanying request text, which
        // is short a trailing "6" — the frame bytes are authoritative.
        assert_eq!(
            hex::encode(frame.source_node_id),
            "3b0af6f3e07bb3aa0e9d3dd11fd0188b64c5102ea0ce5f72a1199cba9c1392a6"
        );
        assert_eq!(frame.sequence, 0);
        assert_eq!(frame.payload.len(), 142);

        // Round trip: re-encoding the decoded frame reproduces the captured
        // bytes exactly, which is an independent check that decode did not
        // silently drop or reorder anything (the CRC alone would not catch
        // that, since it is verified during decode, not re-derived here).
        assert_eq!(frame.encode().unwrap(), bytes);
    }

    #[test]
    fn lora_transport_is_unavailable_without_the_environment_variable() {
        // SAFETY: test-local; no other test in this process reads or writes
        // this specific variable.
        unsafe {
            std::env::remove_var(SECUREMESH_LORA_SERIAL_ENV);
        }
        assert!(LoraTransport::from_env("a".repeat(64)).is_none());
    }

    #[test]
    fn lora_transport_is_unavailable_for_a_nonexistent_device() {
        let err = LoraTransport::open("this-device-does-not-exist-12345", "a".repeat(64), 9600);
        assert!(err.is_err());
    }

    #[test]
    fn from_env_degrades_cleanly_when_the_configured_device_is_invalid() {
        // Exercises the exact path `start_node` uses: a set-but-bad path must
        // still come back as `None`, never a panic or an `Err`.
        // SAFETY: test-local; no other test in this process reads or writes
        // this specific variable.
        unsafe {
            std::env::set_var(
                SECUREMESH_LORA_SERIAL_ENV,
                "this-device-does-not-exist-12345",
            );
        }
        let result = LoraTransport::from_env("a".repeat(64));
        unsafe {
            std::env::remove_var(SECUREMESH_LORA_SERIAL_ENV);
        }
        assert!(result.is_none());
    }

    #[test]
    fn a_diagnostic_payload_reaches_the_serial_backend() {
        let mock = test_support::MockSerial::default();
        let written = Arc::clone(&mock.written);

        let transport = LoraTransport::open_with_io(mock, "ab".repeat(32));
        transport
            .send_diagnostic(b"SECUREMESH-LORA-RUST-TEST-01")
            .unwrap();

        let mut waited = Duration::ZERO;
        while written.lock().unwrap().is_empty() && waited < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(10));
            waited += Duration::from_millis(10);
        }

        let bytes = written.lock().unwrap().clone();
        let frame = LoraFrame::decode(&bytes).expect("a valid frame should have been written");
        assert_eq!(frame.payload, b"SECUREMESH-LORA-RUST-TEST-01");
        assert_eq!(frame.message_type, LoraMessageType::Diagnostic);
    }

    // --- Stage 4A: the SecureMeshEvent receive path (transport level) ------
    //
    // These drive genuine bytes through the real `run_lora_io`/`drain_frames`
    // I/O thread via `MockSerial::queue_inbound`, rather than calling any
    // internal function directly — the same code a real E22 link runs.

    fn wait_until(mut predicate: impl FnMut() -> bool) {
        let mut waited = Duration::ZERO;
        while !predicate() && waited < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(10));
            waited += Duration::from_millis(10);
        }
    }

    #[test]
    fn a_securemesh_event_frame_reaches_poll_event_frames_not_diagnostic() {
        let frame = LoraFrame {
            message_type: LoraMessageType::SecureMeshEvent,
            source_node_id: [0x11; 32],
            sequence: 3,
            payload: b"not a real codec payload, transport level only".to_vec(),
        };
        let bytes = frame.encode().unwrap();

        let mock = test_support::MockSerial::default();
        mock.queue_inbound(&bytes);
        let transport = LoraTransport::open_with_io(mock, "cd".repeat(32));

        wait_until(|| !transport.lock_inboxes().event.is_empty());

        let events = transport.poll_event_frames();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].message_type, LoraMessageType::SecureMeshEvent);
        assert_eq!(events[0].sequence, 3);

        // Draining the event queue must not have touched the diagnostic one.
        assert!(transport.poll_diagnostic_frames().is_empty());
    }

    #[test]
    fn diagnostic_frames_never_enter_the_event_queue() {
        let diagnostic = LoraFrame {
            message_type: LoraMessageType::Diagnostic,
            source_node_id: [0x22; 32],
            sequence: 1,
            payload: b"ping".to_vec(),
        };
        let bytes = diagnostic.encode().unwrap();

        let mock = test_support::MockSerial::default();
        mock.queue_inbound(&bytes);
        let transport = LoraTransport::open_with_io(mock, "ef".repeat(32));

        wait_until(|| !transport.lock_inboxes().diagnostic.is_empty());

        // The diagnostic frame is where it belongs...
        let diagnostics = transport.poll_diagnostic_frames();
        assert_eq!(diagnostics.len(), 1);
        // ...and never leaked into the event queue, even transiently.
        assert!(transport.poll_event_frames().is_empty());
    }

    #[test]
    fn mixed_diagnostic_and_event_traffic_is_strictly_separated() {
        let diagnostic = LoraFrame {
            message_type: LoraMessageType::Diagnostic,
            source_node_id: [0x33; 32],
            sequence: 1,
            payload: b"diag".to_vec(),
        };
        let event = LoraFrame {
            message_type: LoraMessageType::SecureMeshEvent,
            source_node_id: [0x44; 32],
            sequence: 2,
            payload: b"event-shaped-bytes".to_vec(),
        };

        let mut wire = diagnostic.encode().unwrap();
        wire.extend(event.encode().unwrap());

        let mock = test_support::MockSerial::default();
        mock.queue_inbound(&wire);
        let transport = LoraTransport::open_with_io(mock, "12".repeat(32));

        wait_until(|| {
            let guard = transport.lock_inboxes();
            !guard.diagnostic.is_empty() && !guard.event.is_empty()
        });

        let diagnostics = transport.poll_diagnostic_frames();
        let events = transport.poll_event_frames();
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(events.len(), 1);
        assert_eq!(diagnostics[0].message_type, LoraMessageType::Diagnostic);
        assert_eq!(events[0].message_type, LoraMessageType::SecureMeshEvent);
    }

    // --- Phase 6 steps 1–2: the SyncRequest frame type (transport level) ---

    fn sync_request_frame(sequence: u32) -> LoraFrame {
        LoraFrame {
            message_type: LoraMessageType::SyncRequest,
            source_node_id: [0x55; 32],
            sequence,
            // Transport level only: the queue neither decodes nor verifies.
            payload: vec![0xAA; super::super::lora_sync::SYNC_REQUEST_PAYLOAD_BYTES],
        }
    }

    fn diagnostic_marker() -> LoraFrame {
        LoraFrame {
            message_type: LoraMessageType::Diagnostic,
            source_node_id: [0x66; 32],
            sequence: 999,
            payload: b"end".to_vec(),
        }
    }

    /// Queues `frames` followed by a diagnostic marker and waits for the
    /// marker, which proves every earlier frame has been through
    /// `drain_frames` (frames are decoded strictly in wire order).
    fn transport_after(frames: &[LoraFrame]) -> LoraTransport {
        let mut wire = Vec::new();
        for frame in frames {
            wire.extend(frame.encode().unwrap());
        }
        wire.extend(diagnostic_marker().encode().unwrap());

        let mock = test_support::MockSerial::default();
        mock.queue_inbound(&wire);
        let transport = LoraTransport::open_with_io(mock, "77".repeat(32));
        wait_until(|| !transport.lock_inboxes().diagnostic.is_empty());
        transport
    }

    #[test]
    fn message_type_3_is_recognised_as_a_sync_request() {
        assert_eq!(
            LoraMessageType::from_u8(3),
            Some(LoraMessageType::SyncRequest)
        );
        let frame = sync_request_frame(4);
        assert_eq!(LoraFrame::decode(&frame.encode().unwrap()).unwrap(), frame);
    }

    #[test]
    fn a_sync_request_enters_only_its_own_queue() {
        let transport = transport_after(&[sync_request_frame(1)]);

        let requests = transport.poll_sync_requests(usize::MAX);
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].message_type, LoraMessageType::SyncRequest);

        assert!(transport.poll_event_frames().is_empty());
        // Only the marker is in the diagnostic queue.
        let diagnostics = transport.poll_diagnostic_frames();
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].sequence, 999);
    }

    #[test]
    fn poll_sync_requests_preserves_fifo_order() {
        let transport = transport_after(&[
            sync_request_frame(1),
            sync_request_frame(2),
            sync_request_frame(3),
        ]);

        let sequences: Vec<u32> = transport
            .poll_sync_requests(10)
            .iter()
            .map(|f| f.sequence)
            .collect();
        assert_eq!(sequences, vec![1, 2, 3]);
        assert!(transport.poll_sync_requests(10).is_empty());
    }

    #[test]
    fn poll_sync_requests_max_leaves_the_rest_queued() {
        let transport = transport_after(&[
            sync_request_frame(1),
            sync_request_frame(2),
            sync_request_frame(3),
        ]);

        assert!(transport.poll_sync_requests(0).is_empty());

        let first: Vec<u32> = transport
            .poll_sync_requests(2)
            .iter()
            .map(|f| f.sequence)
            .collect();
        assert_eq!(first, vec![1, 2]);

        let rest: Vec<u32> = transport
            .poll_sync_requests(2)
            .iter()
            .map(|f| f.sequence)
            .collect();
        assert_eq!(rest, vec![3]);
    }

    #[test]
    fn the_sync_request_queue_is_bounded_and_keeps_the_oldest() {
        let frames: Vec<LoraFrame> = (0..(MAX_QUEUED_SYNC_REQUESTS as u32 + 5))
            .map(sync_request_frame)
            .collect();
        let transport = transport_after(&frames);

        let held = transport.poll_sync_requests(usize::MAX);
        assert_eq!(held.len(), MAX_QUEUED_SYNC_REQUESTS);
        let sequences: Vec<u32> = held.iter().map(|f| f.sequence).collect();
        let expected: Vec<u32> = (0..MAX_QUEUED_SYNC_REQUESTS as u32).collect();
        assert_eq!(sequences, expected);
    }

    #[test]
    fn event_and_diagnostic_queues_are_unchanged_by_sync_request_traffic() {
        let diagnostic = LoraFrame {
            message_type: LoraMessageType::Diagnostic,
            source_node_id: [0x33; 32],
            sequence: 10,
            payload: b"diag".to_vec(),
        };
        let event = LoraFrame {
            message_type: LoraMessageType::SecureMeshEvent,
            source_node_id: [0x44; 32],
            sequence: 11,
            payload: b"event-shaped-bytes".to_vec(),
        };
        let transport = transport_after(&[
            sync_request_frame(1),
            diagnostic.clone(),
            event.clone(),
            sync_request_frame(2),
        ]);

        // The event queue: exactly the one event, taken whole, as before.
        assert_eq!(transport.poll_event_frames(), vec![event]);
        assert!(transport.poll_event_frames().is_empty());

        // The diagnostic queue: the diagnostic frame then the marker.
        let diagnostics = transport.poll_diagnostic_frames();
        assert_eq!(diagnostics.len(), 2);
        assert_eq!(diagnostics[0], diagnostic);

        // Draining the other queues left both sync requests in place.
        assert_eq!(transport.poll_sync_requests(usize::MAX).len(), 2);
    }

    #[test]
    fn a_sync_request_payload_reaches_the_serial_backend_as_type_3() {
        let mock = test_support::MockSerial::default();
        let written = Arc::clone(&mock.written);
        let transport = LoraTransport::open_with_io(mock, "ab".repeat(32));

        let payload = vec![0x42; super::super::lora_sync::SYNC_REQUEST_PAYLOAD_BYTES];
        transport.send_sync_request(&payload).unwrap();

        wait_until(|| !written.lock().unwrap().is_empty());
        let bytes = written.lock().unwrap().clone();
        let frame = LoraFrame::decode(&bytes).unwrap();
        assert_eq!(frame.message_type, LoraMessageType::SyncRequest);
        assert_eq!(frame.source_node_id, [0xAB; 32]);
        assert_eq!(frame.payload, payload);
    }

    #[test]
    fn a_wrongly_sized_sync_request_payload_is_refused_before_framing() {
        let mock = test_support::MockSerial::default();
        let written = Arc::clone(&mock.written);
        let transport = LoraTransport::open_with_io(mock, "ab".repeat(32));

        let size = super::super::lora_sync::SYNC_REQUEST_PAYLOAD_BYTES;
        assert!(transport.send_sync_request(&vec![0; size - 1]).is_err());
        assert!(transport.send_sync_request(&vec![0; size + 1]).is_err());

        std::thread::sleep(Duration::from_millis(50));
        assert!(written.lock().unwrap().is_empty());
    }
}

/// A mock duplex byte stream standing in for a real serial port, so the
/// framing and threading logic in this module is exercised without physical
/// hardware. `pub(crate)` rather than test-private because
/// `composite_transport`'s and `runtime`'s own tests need it too, to prove
/// QUIC is unaffected by a LoRa side that is actually attached and working,
/// and (Phase 3A) to drive genuine frames through the real I/O thread without
/// a device.
#[cfg(test)]
pub(crate) mod test_support {
    use std::io::{Read, Write};
    use std::sync::{Arc, Mutex};

    /// Captures every byte written to it. Reads return whatever has been
    /// queued via [`MockSerial::queue_inbound`], in order; with nothing
    /// queued, a read behaves like a timeout, matching how the real port
    /// behaves with nothing on the wire. This is the seam that lets a test
    /// feed bytes through the *actual* `run_lora_io`/`drain_frames` code path
    /// — the same one a real E22 link runs — without opening a device.
    #[derive(Clone, Default)]
    pub(crate) struct MockSerial {
        pub written: Arc<Mutex<Vec<u8>>>,
        to_read: Arc<Mutex<Vec<u8>>>,
    }

    impl MockSerial {
        /// Queues bytes to be handed back by subsequent `read` calls, as if
        /// they had just arrived on the wire.
        pub(crate) fn queue_inbound(&self, bytes: &[u8]) {
            self.to_read
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .extend_from_slice(bytes);
        }
    }

    impl Read for MockSerial {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let mut queued = self
                .to_read
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if queued.is_empty() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "mock serial has no inbound data",
                ));
            }
            let n = buf.len().min(queued.len());
            buf[..n].copy_from_slice(&queued[..n]);
            queued.drain(0..n);
            Ok(n)
        }
    }

    impl Write for MockSerial {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.written
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
}
