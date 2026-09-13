//! Bounded, lossless detection of ZMODEM transfer starts in an otherwise ordinary PTY stream.

use std::time::Duration;

use flate2::Crc;
use instant::Instant;

use crate::zmodem::{Encoding, Frame, Header, ZDLE, ZPAD, ZmodemRole};

/// Maximum time ordinary output may be retained as a possible header.
pub const PENDING_TIMEOUT: Duration = Duration::from_millis(100);

const MAX_PADS: usize = 32;
const PAYLOAD_LEN: usize = 5;
const MAX_BODY_LEN: usize = PAYLOAD_LEN + 4;
const MAX_PENDING: usize = MAX_PADS + 2 + MAX_BODY_LEN * 2;

/// Byte ownership after a detector read.
#[derive(Debug, PartialEq, Eq)]
pub enum DetectorOutcome {
    Render(Vec<u8>),
    Started {
        header: Header,
        role: ZmodemRole,
        render_before: Vec<u8>,
        /// The original header and every remaining byte from this read, without re-encoding.
        protocol: Vec<u8>,
    },
}

/// Idle-stream detector. After `Started`, route subsequent reads directly to the transfer worker.
///
/// Schedule `expire` using `next_deadline`, even when the PTY is silent. Render the bytes from
/// `flush` before disabling detection or closing the stream. Neither operation discards bytes.
#[derive(Debug, Default)]
pub struct ZmodemDetector {
    pending: Vec<u8>,
    deadline: Option<Instant>,
}

impl ZmodemDetector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.deadline
    }

    /// Returns expired candidate bytes exactly once. New fragments do not extend the deadline.
    pub fn expire(&mut self, now: Instant) -> Vec<u8> {
        if self.deadline.is_some_and(|deadline| now >= deadline) {
            self.flush()
        } else {
            Vec::new()
        }
    }

    /// Returns all retained bytes and restores the idle state.
    pub fn flush(&mut self) -> Vec<u8> {
        self.deadline = None;
        std::mem::take(&mut self.pending)
    }

    /// Detects the first complete CRC-valid ZRQINIT, ZRINIT or ZFILE header.
    ///
    /// Hex headers require at least two pads, fourteen hex digits and CR/LF (parity is allowed
    /// on the line ending). XON is optional and belongs to the worker once the header completes.
    /// Binary headers require at least one pad and valid ZDLE escapes. Excess preamble pads
    /// beyond the bounded candidate budget are returned as ordinary output.
    pub fn push(&mut self, bytes: &[u8], now: Instant) -> DetectorOutcome {
        let mut render_before = self.expire(now);
        let mut offset = 0;
        while offset < bytes.len() {
            if self.pending.is_empty() {
                let ordinary_len = bytes[offset..]
                    .iter()
                    .position(|&byte| byte == ZPAD)
                    .unwrap_or(bytes.len() - offset);
                render_before.extend_from_slice(&bytes[offset..offset + ordinary_len]);
                offset += ordinary_len;
                if offset == bytes.len() {
                    break;
                }
                self.deadline = Some(now + PENDING_TIMEOUT);
            }

            self.pending.push(bytes[offset]);
            offset += 1;

            // Only the fixed-size candidate is revisited, never the accumulated PTY read.
            let mut rejected = 0;
            while rejected < self.pending.len() {
                match decode_candidate(&self.pending[rejected..]) {
                    Candidate::Incomplete => break,
                    Candidate::Invalid => rejected += 1,
                    Candidate::Header(header) => {
                        let role = match header.frame {
                            Frame::Zrqinit | Frame::Zfile => Some(ZmodemRole::Download),
                            Frame::Zrinit => Some(ZmodemRole::Upload),
                            Frame::Zabort | Frame::Zcan | Frame::Other(_) => None,
                        };
                        if let Some(role) = role {
                            render_before.extend_from_slice(&self.pending[..rejected]);
                            let mut protocol = self.pending.split_off(rejected);
                            self.pending.clear();
                            self.deadline = None;
                            protocol.extend_from_slice(&bytes[offset..]);
                            return DetectorOutcome::Started {
                                header,
                                role,
                                render_before,
                                protocol,
                            };
                        }
                        // A valid non-start header is ordinary output, including embedded pads.
                        rejected = self.pending.len();
                    }
                }
            }
            render_before.extend_from_slice(&self.pending[..rejected]);
            if rejected != 0 {
                self.pending.copy_within(rejected.., 0);
                self.pending.truncate(self.pending.len() - rejected);
            }
            if self.pending.is_empty() {
                self.deadline = None;
            }
            debug_assert!(self.pending.len() <= MAX_PENDING);
        }
        DetectorOutcome::Render(render_before)
    }
}

enum Candidate {
    Incomplete,
    Invalid,
    Header(Header),
}

fn decode_candidate(bytes: &[u8]) -> Candidate {
    let pads = bytes.iter().take_while(|&&byte| byte == ZPAD).count();
    if pads == 0 || pads > MAX_PADS || bytes.len() > MAX_PENDING {
        return Candidate::Invalid;
    }
    let Some(&escape) = bytes.get(pads) else {
        return Candidate::Incomplete;
    };
    if escape != ZDLE {
        return Candidate::Invalid;
    }
    let Some(&encoding) = bytes.get(pads + 1) else {
        return Candidate::Incomplete;
    };
    let encoding = match encoding {
        b'A' => Encoding::Zbin,
        b'B' if pads >= 2 => Encoding::Zhex,
        b'C' => Encoding::Zbin32,
        _ => return Candidate::Invalid,
    };
    let body_len = match encoding {
        Encoding::Zbin | Encoding::Zhex => PAYLOAD_LEN + 2,
        Encoding::Zbin32 => MAX_BODY_LEN,
    };
    let mut body = [0; MAX_BODY_LEN];
    let mut offset = pads + 2;
    for value in &mut body[..body_len] {
        let Some(&byte) = bytes.get(offset) else {
            return Candidate::Incomplete;
        };
        offset += 1;
        *value = match encoding {
            Encoding::Zhex => {
                let Some(high) = hex_nibble(byte) else {
                    return Candidate::Invalid;
                };
                let Some(&low) = bytes.get(offset) else {
                    return Candidate::Incomplete;
                };
                let Some(low) = hex_nibble(low) else {
                    return Candidate::Invalid;
                };
                offset += 1;
                (high << 4) | low
            }
            Encoding::Zbin | Encoding::Zbin32 => {
                if byte == ZDLE {
                    let Some(&escaped) = bytes.get(offset) else {
                        return Candidate::Incomplete;
                    };
                    offset += 1;
                    match escaped {
                        b'l' => 0x7f,
                        b'm' => 0xff,
                        value if value & 0x60 == 0x40 => value ^ 0x40,
                        _ => return Candidate::Invalid,
                    }
                } else {
                    // Flow-control bytes are not payload when sent bare; accepting them here
                    // could claim a header that the transport/protocol interprets differently.
                    if matches!(byte, 0x11 | 0x13 | 0x91 | 0x93) {
                        return Candidate::Invalid;
                    }
                    byte
                }
            }
        };
    }

    let crc_valid = match encoding {
        Encoding::Zhex | Encoding::Zbin => {
            body[PAYLOAD_LEN..body_len] == crc16_xmodem(&body[..PAYLOAD_LEN]).to_be_bytes()
        }
        Encoding::Zbin32 => {
            let mut crc = Crc::new();
            crc.update(&body[..PAYLOAD_LEN]);
            body[PAYLOAD_LEN..body_len] == crc.sum().to_le_bytes()
        }
    };
    if !crc_valid {
        return Candidate::Invalid;
    }

    if encoding == Encoding::Zhex {
        for expected in [b'\r', b'\n'] {
            let Some(&byte) = bytes.get(offset) else {
                return Candidate::Incomplete;
            };
            if byte & 0x7f != expected {
                return Candidate::Invalid;
            }
            offset += 1;
        }
    }

    let frame = match body[0] {
        0 => Frame::Zrqinit,
        1 => Frame::Zrinit,
        4 => Frame::Zfile,
        7 => Frame::Zabort,
        0x17 => Frame::Zcan,
        other => Frame::Other(other),
    };
    Candidate::Header(Header {
        encoding,
        frame,
        flags: [body[1], body[2], body[3], body[4]],
    })
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

// Reuses the existing protocol implementation's CRC-16/XMODEM algorithm, whose helper is private.
fn crc16_xmodem(bytes: &[u8]) -> u16 {
    let mut crc = 0u16;
    for &byte in bytes {
        crc ^= u16::from(byte) << 8;
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

#[cfg(test)]
#[path = "zmodem_detector_tests.rs"]
mod tests;
