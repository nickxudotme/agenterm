//! ZMODEM (`sz` / `rz`) file transfer support.
//!
//! The terminal owns the PTY byte stream, so a ZMODEM transfer only works if
//! the protocol bytes are intercepted before the ANSI parser renders them as
//! garbage. This module owns that interception.
//!
//! Wire decoding here mirrors `zmodem2`'s own (CRC algorithms, ZDLE unescaping,
//! header lengths) so the detector can never claim a transfer that the state
//! machine would immediately reject.

use std::collections::VecDeque;
use std::io::Write;
use std::path::PathBuf;

pub use zmodem2::{Action, Error as ZmodemError, Event, FileInfo, Position, Receiver, Sender};

/// ZMODEM pad character.
pub const ZPAD: u8 = b'*';
/// ZMODEM data link escape.
pub const ZDLE: u8 = 0x18;
/// XON; terminates a hex header.
const XON: u8 = 0x11;
/// CAN; used to build the abort sequence.
const CAN: u8 = 0x18;
/// Backspace padding used by the canonical abort sequence.
const BS: u8 = 0x08;

/// Header payload size: one frame-type byte plus four flag bytes.
const HEADER_PAYLOAD_SIZE: usize = 5;
/// Longest possible header body (`ZHEX`, two hex characters per byte).
const MAX_HEADER_BODY_LEN: usize = (HEADER_PAYLOAD_SIZE + 2) * 2;

/// ZMODEM frame encodings, matching the on-the-wire values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    Zbin = 0x41,
    Zhex = 0x42,
    Zbin32 = 0x43,
}

impl Encoding {
    /// Number of bytes the encoded header body occupies on the wire.
    fn body_len(self) -> usize {
        let payload_and_crc = HEADER_PAYLOAD_SIZE
            + match self {
                Encoding::Zbin32 => 4,
                Encoding::Zbin | Encoding::Zhex => 2,
            };
        match self {
            // `ZHEX` sends every byte as two hex characters.
            Encoding::Zhex => payload_and_crc * 2,
            Encoding::Zbin | Encoding::Zbin32 => payload_and_crc,
        }
    }
}

/// Frame types the terminal needs to distinguish.
///
/// Discriminants are the on-the-wire ZMODEM frame type values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Frame {
    /// `sz` announces it is ready to send.
    Zrqinit = 0,
    /// `rz` announces it is ready to receive.
    Zrinit = 1,
    /// A file is about to be transferred.
    Zfile = 4,
    /// The peer aborted the session.
    Zabort = 7,
    /// The session was cancelled.
    Zcan = 0x17,
    /// Any other frame; we only need to know that it decoded cleanly.
    Other(u8),
}

impl Frame {
    /// The on-the-wire frame type value.
    pub fn frame_byte(self) -> u8 {
        match self {
            Frame::Zrqinit => 0,
            Frame::Zrinit => 1,
            Frame::Zfile => 4,
            Frame::Zabort => 7,
            Frame::Zcan => 0x17,
            Frame::Other(other) => other,
        }
    }

    fn from_byte(byte: u8) -> Self {
        match byte {
            0 => Frame::Zrqinit,
            1 => Frame::Zrinit,
            4 => Frame::Zfile,
            7 => Frame::Zabort,
            0x17 => Frame::Zcan,
            other => Frame::Other(other),
        }
    }
}

/// A decoded ZMODEM header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub encoding: Encoding,
    pub frame: Frame,
    /// The four flag bytes, usually read as a 32-bit count.
    pub flags: [u8; 4],
}

impl Header {
    /// The flag bytes read as a little-endian count.
    pub fn count(self) -> u32 {
        u32::from_le_bytes(self.flags)
    }
}

fn crc16_xmodem(data: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for &byte in data {
        crc ^= (byte as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

fn crc32_iso_hdlc(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// Maps the byte following a `ZDLE` back to its real value.
const UNZDLE_TABLE: [u8; 0x100] = [
    0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f,
    0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f,
    0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x2b, 0x2c, 0x2d, 0x2e, 0x2f,
    0x30, 0x31, 0x32, 0x33, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x3b, 0x3c, 0x3d, 0x3e, 0x3f,
    0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f,
    0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f,
    0x60, 0x61, 0x62, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x6b, 0x7f, 0xff, 0x6e, 0x6f,
    0x70, 0x71, 0x72, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x7b, 0x7c, 0x7d, 0x7e, 0x7f,
    0x80, 0x81, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x8b, 0x8c, 0x8d, 0x8e, 0x8f,
    0x90, 0x91, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a, 0x9b, 0x9c, 0x9d, 0x9e, 0x9f,
    0xa0, 0xa1, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xab, 0xac, 0xad, 0xae, 0xaf,
    0xb0, 0xb1, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xbb, 0xbc, 0xbd, 0xbe, 0xbf,
    0x80, 0x81, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x8b, 0x8c, 0x8d, 0x8e, 0x8f,
    0x90, 0x91, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a, 0x9b, 0x9c, 0x9d, 0x9e, 0x9f,
    0xe0, 0xe1, 0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xeb, 0xec, 0xed, 0xee, 0xef,
    0xf0, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa, 0xfb, 0xfc, 0xfd, 0xfe, 0xff,
];

/// Outcome of decoding a header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DecodeResult {
    /// A well-formed header was decoded. `len` counts every wire byte it used,
    /// including the `ZPAD [ZPAD] ZDLE` prefix and any escaped bytes.
    Header { header: Header, len: usize },
    /// These bytes cannot begin a header.
    NotAHeader,
    /// The header is incomplete.
    NeedMore,
}

/// Decodes a header from `bytes`, which must start with `ZPAD [ZPAD] ZDLE`.
///
/// `prefix_len` is the length of that prefix. Escaped bytes consume two wire
/// bytes each, so `len` can exceed the logical header size.
fn decode_header(bytes: &[u8], prefix_len: usize) -> DecodeResult {
    let Some(&encoding_byte) = bytes.get(prefix_len) else {
        return DecodeResult::NeedMore;
    };
    let Some(encoding) = encoding_from_byte(encoding_byte) else {
        return DecodeResult::NotAHeader;
    };

    let mut body: [u8; MAX_HEADER_BODY_LEN] = [0; MAX_HEADER_BODY_LEN];
    let mut body_len = 0;
    let mut consumed = prefix_len + 1;
    let mut escape_pending = false;

    while body_len < encoding.body_len() {
        let Some(&byte) = bytes.get(consumed) else {
            return DecodeResult::NeedMore;
        };
        consumed += 1;
        let value = if escape_pending {
            escape_pending = false;
            UNZDLE_TABLE[byte as usize]
        } else if byte == ZDLE {
            escape_pending = true;
            continue;
        } else {
            byte
        };
        // A `ZHEX` body is pure hex once unescaped, so a non-hex byte rules
        // the candidate out immediately instead of stalling the read.
        if encoding == Encoding::Zhex && !is_hex_digit(value) {
            return DecodeResult::NotAHeader;
        }
        body[body_len] = value;
        body_len += 1;
    }

    match decode_header_body(encoding, &body[..body_len]) {
        Some(header) => DecodeResult::Header {
            header,
            len: consumed,
        },
        None => DecodeResult::NotAHeader,
    }
}

fn encoding_from_byte(byte: u8) -> Option<Encoding> {
    match byte {
        0x41 => Some(Encoding::Zbin),
        0x42 => Some(Encoding::Zhex),
        0x43 => Some(Encoding::Zbin32),
        _ => None,
    }
}

/// Whether `byte` is a hex digit in either case.
fn is_hex_digit(byte: u8) -> bool {
    byte.is_ascii_hexdigit()
}

/// Validates a fully received header body and decodes it.
fn decode_header_body(encoding: Encoding, body: &[u8]) -> Option<Header> {
    if encoding == Encoding::Zhex {
        if body.len() % 2 != 0 {
            return None;
        }
        // Both hex cases are legal on the wire.
        let nibble = |byte: u8| -> Option<u8> {
            match byte {
                b'0'..=b'9' => Some(byte - b'0'),
                b'a'..=b'f' => Some(byte - b'a' + 10),
                b'A'..=b'F' => Some(byte - b'A' + 10),
                _ => None,
            }
        };
        let mut decoded = [0u8; MAX_HEADER_BODY_LEN];
        for (index, pair) in body.chunks(2).enumerate() {
            decoded[index] = (nibble(pair[0])? << 4) | nibble(pair[1])?;
        }
        let len = body.len() / 2;
        return decode_header_body_binary(encoding, &decoded[..len]);
    }
    decode_header_body_binary(encoding, body)
}

fn decode_header_body_binary(encoding: Encoding, body: &[u8]) -> Option<Header> {
    let crc_len = match encoding {
        Encoding::Zbin32 => 4,
        Encoding::Zbin | Encoding::Zhex => 2,
    };
    if body.len() < HEADER_PAYLOAD_SIZE + crc_len {
        return None;
    }
    let (payload, crc_bytes) = body.split_at(HEADER_PAYLOAD_SIZE);

    match encoding {
        Encoding::Zbin32 => {
            let expected = crc32_iso_hdlc(payload).to_le_bytes();
            if crc_bytes != &expected[..crc_len] {
                return None;
            }
        }
        Encoding::Zbin | Encoding::Zhex => {
            let expected = crc16_xmodem(payload).to_be_bytes();
            if crc_bytes != &expected[..crc_len] {
                return None;
            }
        }
    }

    let mut flags = [0u8; 4];
    flags.copy_from_slice(&payload[1..=4]);
    Some(Header {
        encoding,
        frame: Frame::from_byte(payload[0]),
        flags,
    })
}

/// A header located in the PTY stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FoundHeader {
    pub header: Header,
    /// How many bytes the header occupies from where it starts.
    pub len: usize,
}

/// What the caller should do with the bytes just fed to [`ZmodemDetector`].
#[derive(Debug, PartialEq, Eq)]
pub enum DetectorOutcome {
    /// No transfer: render these bytes through the normal terminal path.
    ///
    /// Empty when every byte is still held back as a possible header prefix.
    Render(Vec<u8>),
    /// A transfer starts. Render `render_before` through the normal path, then
    /// feed `protocol` to [`ZmodemSession`]. `protocol` starts at the detected
    /// header and includes any bytes that followed it in the same read.
    Started {
        header: Header,
        render_before: Vec<u8>,
        protocol: Vec<u8>,
    },
}

/// Longest byte run that can still turn into a header.
const MAX_PENDING: usize = MAX_HEADER_BODY_LEN + 8;

/// Scans PTY output for a well-formed ZMODEM header.
///
/// Well-formedness is the detection bar: a transfer is claimed only when
/// `zmodem2` would itself accept the frame, so ordinary binary output cannot be
/// mistaken for ZMODEM. Bytes that might still become a header are held back
/// for one read and released as [`DetectorOutcome::Render`] if they do not.
#[derive(Default)]
pub struct ZmodemDetector {
    /// Bytes held because they could still complete a header.
    pending: Vec<u8>,
}

impl ZmodemDetector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Discards any held bytes, e.g. when a transfer ends.
    pub fn reset(&mut self) {
        self.pending.clear();
    }

    /// Feeds `bytes` and reports how they should be handled.
    pub fn push(&mut self, bytes: &[u8]) -> DetectorOutcome {
        let mut working = std::mem::take(&mut self.pending);
        working.extend_from_slice(bytes);

        let outcome = self.scan(&working);
        match &outcome {
            DetectorOutcome::Render(rendered) => {
                // Keep whatever was not accounted for as renderable.
                self.pending = working[rendered.len()..].to_vec();
            }
            DetectorOutcome::Started { .. } => self.reset(),
        }
        outcome
    }

    /// Finds the first valid header in `working`, or reports what to render.
    fn scan(&self, working: &[u8]) -> DetectorOutcome {
        let mut index = 0;
        while index < working.len() {
            if working[index] != ZPAD {
                index += 1;
                continue;
            }
            // ZMODEM accepts one or two pads before the `ZDLE`.
            let pads = if working.get(index + 1) == Some(&ZPAD) {
                2
            } else {
                1
            };
            if working.get(index + pads) != Some(&ZDLE) {
                index += 1;
                continue;
            }

            let prefix_len = pads + 1;
            match decode_header(&working[index..], prefix_len) {
                DecodeResult::Header { header, .. } => {
                    return DetectorOutcome::Started {
                        header,
                        render_before: working[..index].to_vec(),
                        protocol: working[index..].to_vec(),
                    };
                }
                DecodeResult::NotAHeader => {
                    // This pad run is not a header; keep scanning after it.
                    index += 1;
                }
                DecodeResult::NeedMore => {
                    // Candidate header at the tail: hold it for the next read.
                    if working.len() - index > MAX_PENDING {
                        index += 1;
                        continue;
                    }
                    return DetectorOutcome::Render(working[..index].to_vec());
                }
            }
        }
        DetectorOutcome::Render(working.to_vec())
    }
}

/// Which side of the transfer the terminal plays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ZmodemRole {
    /// The remote runs `sz`; we write files to disk.
    Download,
    /// The remote runs `rz`; we read files from disk.
    Upload,
}

/// A file queued for upload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UploadFile {
    pub path: PathBuf,
    /// Name advertised on the wire.
    pub name: Vec<u8>,
    pub size: u32,
}

/// Builds the canonical ZMODEM abort sequence.
///
/// Two `CAN`s are the protocol payload; the trailing backspaces are padding
/// that survives line noise.
pub fn abort_sequence() -> Vec<u8> {
    let mut out = Vec::with_capacity(16);
    out.extend_from_slice(&[ZPAD, ZPAD, ZPAD]);
    out.extend_from_slice(&[CAN, CAN]);
    out.extend_from_slice(&[BS; 8]);
    out
}

/// Builds a `ZRQINIT` header, which nudges a waiting `rz` into handshaking.
pub fn zrqinit_sequence() -> Vec<u8> {
    encode_hex_header(Frame::Zrqinit, &[0; 4])
}

/// Encodes a `ZHEX` header with the given frame type and flags.
fn encode_hex_header(frame: Frame, flags: &[u8; 4]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(HEADER_PAYLOAD_SIZE + 2);
    payload.push(frame.frame_byte());
    payload.extend_from_slice(flags);
    payload.extend_from_slice(&crc16_xmodem(&payload).to_be_bytes());

    let mut out = Vec::new();
    out.extend_from_slice(&[ZPAD, ZPAD, ZDLE, Encoding::Zhex as u8]);
    for &byte in &payload {
        let mut hex = [0u8; 2];
        hex::encode_to_slice([byte], &mut hex).expect("one byte encodes to two hex chars");
        for &nibble in &hex {
            out.push(nibble);
            // A `ZDLE` inside the body must itself be escaped.
            if nibble == ZDLE {
                out.push(ZDLE);
            }
        }
    }
    // `lrzsz` terminates a hex header with CR, LF-with-high-bit, XON; match
    // it so our upload headers are byte-identical to what `sz` would send.
    out.extend_from_slice(&[b'\r', b'\n' | 0x80, XON]);
    out
}

/// A live ZMODEM transfer.
///
/// Wraps the `zmodem2` state machine for the terminal's role and turns PTY
/// output into file bytes (or the reverse) plus protocol bytes to write back.
/// The caller owns all I/O: nothing here blocks or touches the filesystem
/// except through the buffers it hands back.
pub enum ZmodemSession {
    /// The remote is running `sz`; we are writing files to disk.
    Download(Box<DownloadSession>),
    /// The remote is running `rz`; we are reading files from disk.
    Upload(Box<UploadSession>),
}

impl ZmodemSession {
    /// Starts receiving files from a remote `sz`.
    pub fn new_download() -> Result<Self, ZmodemError> {
        Ok(ZmodemSession::Download(Box::new(DownloadSession {
            receiver: Receiver::with_flow_control(0, true)?,
            current_name: String::new(),
        })))
    }

    /// Starts sending `files` to a remote `rz`.
    pub fn new_upload(files: Vec<UploadFile>) -> Result<Self, ZmodemError> {
        Ok(ZmodemSession::Upload(Box::new(UploadSession {
            sender: Sender::new()?,
            files: files.into_iter().collect::<VecDeque<_>>(),
            current: None,
        })))
    }

    pub fn role(&self) -> ZmodemRole {
        match self {
            ZmodemSession::Download(_) => ZmodemRole::Download,
            ZmodemSession::Upload(_) => ZmodemRole::Upload,
        }
    }

    /// Feeds PTY output into the protocol and returns what the caller must do.
    ///
    /// `file_bytes` carries received data for downloads; `to_pty` carries
    /// protocol bytes that must be written back to advance the transfer.
    pub fn submit_wire(&mut self, bytes: &[u8]) -> Result<ZmodemStep, ZmodemError> {
        match self {
            ZmodemSession::Download(session) => session.submit_wire(bytes),
            ZmodemSession::Upload(session) => session.submit_wire(bytes),
        }
    }

    /// Bytes to write to the PTY right after the session is created.
    pub fn initial_output(&mut self) -> Result<Vec<u8>, ZmodemError> {
        match self {
            ZmodemSession::Download(session) => session.step(),
            ZmodemSession::Upload(session) => session.step(),
        }
    }

    /// Advances an upload by offering the next queued file.
    ///
    /// Returns protocol bytes to write; a no-op once the queue is empty.
    pub fn offer_next_file(&mut self) -> Result<Vec<u8>, ZmodemError> {
        match self {
            ZmodemSession::Download(_) => Ok(Vec::new()),
            ZmodemSession::Upload(session) => {
                session.offer_next_file()?;
                session.step()
            }
        }
    }

    /// Supplies file data the sender asked for via [`ZmodemStep::file_request`].
    pub fn submit_file(&mut self, data: &[u8]) -> Result<Vec<u8>, ZmodemError> {
        match self {
            ZmodemSession::Download(_) => Ok(Vec::new()),
            ZmodemSession::Upload(session) => {
                session.sender.submit_file(data)?;
                session.step()
            }
        }
    }

    /// Aborts the transfer, returning bytes to write to the PTY.
    pub fn abort(&mut self) -> Vec<u8> {
        match self {
            ZmodemSession::Download(session) => session.abort(),
            ZmodemSession::Upload(session) => session.abort(),
        }
    }
}

/// One round of protocol progress.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ZmodemStep {
    /// Bytes that must be written to the PTY.
    pub to_pty: Vec<u8>,
    /// File data received, with the name advertised by the sender.
    pub file_data: Option<FileChunk>,
    /// Events the protocol reported.
    pub events: Vec<OwnedEvent>,
    /// File bytes the sender must supply next.
    pub file_request: Option<FileRequest>,
    /// Whether the session has finished, successfully or not.
    pub finished: bool,
}

/// A chunk of a file being received.
#[derive(Debug, PartialEq, Eq)]
pub struct FileChunk {
    pub name: String,
    /// Byte offset this data belongs to.
    pub offset: u64,
    pub data: Vec<u8>,
}

/// A request for the sender side to produce file bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileRequest {
    pub offset: u32,
    pub max_len: usize,
}

pub struct DownloadSession {
    receiver: Receiver,
    /// Name advertised by the sender for the file in flight.
    current_name: String,
}

impl DownloadSession {
    fn step(&mut self) -> Result<Vec<u8>, ZmodemError> {
        let mut sink = PtySink::new();
        let mut events = Vec::new();
        // Drain the handshake: `poll` yields wire bytes until it goes idle.
        loop {
            match self.receiver.poll() {
                Action::WriteWire(bytes) => {
                    let len = bytes.len();
                    sink.write_all(bytes).expect("sink never errors");
                    self.receiver.wire_written(len);
                }
                Action::Idle => break,
                Action::Event(event) => events.push(own_event(event)),
                _ => break,
            }
        }
        Ok(sink.take())
    }

    fn submit_wire(&mut self, bytes: &[u8]) -> Result<ZmodemStep, ZmodemError> {
        let mut step = ZmodemStep::default();
        let mut consumed = 0usize;
        let mut sink = PtySink::new();

        while consumed < bytes.len() {
            let n = self.receiver.submit_wire(&bytes[consumed..])?;
            if n == 0 {
                break;
            }
            consumed += n;
        }

        loop {
            match self.receiver.poll() {
                Action::WriteWire(to_write) => {
                    let len = to_write.len();
                    sink.write_all(to_write).expect("sink never errors");
                    self.receiver.wire_written(len);
                }
                Action::WriteFile(data) => {
                    let len = data.len();
                    let chunk = step
                        .file_data
                        .get_or_insert_with(|| FileChunk {
                            name: self.current_name.clone(),
                            offset: 0,
                            data: Vec::new(),
                        });
                    chunk.name = self.current_name.clone();
                    chunk.data.extend_from_slice(data);
                    self.receiver.file_written(len)?;
                }
                Action::Event(event) => {
                    let owned = own_event(event);
                    if let OwnedEvent::FileStarted { ref name, .. } = owned {
                        self.current_name.clone_from(name);
                    }
                    if matches!(owned, OwnedEvent::SessionCompleted | OwnedEvent::Aborted) {
                        step.finished = true;
                    }
                    step.events.push(owned);
                }
                // `Action` is `#[non_exhaustive]`; unknown variants cannot be
                // handled meaningfully, so stop rather than spin.
                _ => break,
            }
        }

        step.to_pty = sink.take();
        Ok(step)
    }

    fn abort(&mut self) -> Vec<u8> {
        let _ = self.receiver.abort();
        abort_sequence()
    }
}

pub struct UploadSession {
    sender: Sender,
    files: VecDeque<UploadFile>,
    current: Option<UploadFile>,
}

impl UploadSession {
    fn step(&mut self) -> Result<Vec<u8>, ZmodemError> {
        let mut sink = PtySink::new();
        loop {
            match self.sender.poll() {
                Action::WriteWire(bytes) => {
                    let len = bytes.len();
                    sink.write_all(bytes).expect("sink never errors");
                    self.sender.wire_written(len);
                }
                _ => break,
            }
        }
        Ok(sink.take())
    }

    fn submit_wire(&mut self, bytes: &[u8]) -> Result<ZmodemStep, ZmodemError> {
        let mut step = ZmodemStep::default();
        let mut consumed = 0usize;
        let mut sink = PtySink::new();

        while consumed < bytes.len() {
            let n = self.sender.submit_wire(&bytes[consumed..])?;
            if n == 0 {
                break;
            }
            consumed += n;
        }

        loop {
            match self.sender.poll() {
                Action::WriteWire(to_write) => {
                    let len = to_write.len();
                    sink.write_all(to_write).expect("sink never errors");
                    self.sender.wire_written(len);
                }
                Action::ReadFile { offset, max_len } => {
                    step.file_request = Some(FileRequest {
                        offset: offset.get(),
                        max_len,
                    });
                    break;
                }
                Action::Event(event) => {
                    let owned = own_event(event);
                    if matches!(owned, OwnedEvent::FileCompleted) {
                        self.current = None;
                    }
                    if matches!(owned, OwnedEvent::SessionCompleted | OwnedEvent::Aborted) {
                        step.finished = true;
                    }
                    step.events.push(owned);
                }
                _ => break,
            }
        }

        step.to_pty = sink.take();
        Ok(step)
    }

    /// Offers the next queued file once the sender is ready for one.
    fn offer_next_file(&mut self) -> Result<(), ZmodemError> {
        if self.current.is_some() {
            return Ok(());
        }
        let Some(file) = self.files.pop_front() else {
            return Ok(());
        };
        self.sender.start_file(FileInfo::new(
            &file.name,
            Some(Position::new(file.size)),
        ))?;
        self.current = Some(file);
        Ok(())
    }

    fn abort(&mut self) -> Vec<u8> {
        self.sender.abort();
        abort_sequence()
    }
}

/// An owned protocol event, so sessions can outlive the borrow `poll` gives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OwnedEvent {
    FileStarted {
        name: String,
        size: Option<u32>,
    },
    FileCompleted,
    SessionCompleted,
    Aborted,
}

fn own_event(event: Event<'_>) -> OwnedEvent {
    match event {
        Event::FileStarted(info) => OwnedEvent::FileStarted {
            name: String::from_utf8_lossy(info.name).into_owned(),
            size: info.size.map(Position::get),
        },
        Event::FileCompleted => OwnedEvent::FileCompleted,
        Event::SessionCompleted => OwnedEvent::SessionCompleted,
        Event::Aborted => OwnedEvent::Aborted,
        _ => OwnedEvent::Aborted,
    }
}

/// Sink that `zmodem2` writes outgoing protocol bytes into.
#[derive(Default)]
pub struct PtySink {
    bytes: Vec<u8>,
}

impl PtySink {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn take(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.bytes)
    }
}

impl Write for PtySink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.bytes.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
#[path = "zmodem_tests.rs"]
mod tests;
