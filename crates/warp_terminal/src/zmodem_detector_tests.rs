use rand::rngs::StdRng;
use rand::{RngCore, SeedableRng};

use super::*;

const REAL_SZ_HEADER: &[u8] = b"**\x18B00000000000000\r\x8a\x11";
const REAL_RZ_HEADER: &[u8] = b"**\x18B0100000023be50\r\x8a\x11";
const REAL_ZFILE_HEADER: &[u8] = b"*\x18C\x04\x00\x00\x00\x01\x4b\x61\xa5\x44";

fn body(encoding: Encoding, frame: u8, flags: [u8; 4]) -> Vec<u8> {
    let mut body = vec![frame];
    body.extend_from_slice(&flags);
    match encoding {
        Encoding::Zhex | Encoding::Zbin => {
            let crc = crc16_xmodem(&body);
            body.extend_from_slice(&crc.to_be_bytes());
        }
        Encoding::Zbin32 => {
            let mut crc = Crc::new();
            crc.update(&body);
            body.extend_from_slice(&crc.sum().to_le_bytes());
        }
    }
    body
}

fn encode_body(encoding: Encoding, body: &[u8]) -> Vec<u8> {
    let mut wire = vec![ZPAD];
    if encoding == Encoding::Zhex {
        wire.push(ZPAD);
    }
    wire.extend_from_slice(&[ZDLE, encoding as u8]);
    match encoding {
        Encoding::Zhex => {
            wire.extend_from_slice(hex::encode(body).as_bytes());
            wire.extend_from_slice(b"\r\n\x11");
        }
        Encoding::Zbin | Encoding::Zbin32 => {
            for &byte in body {
                match byte {
                    0x7f => wire.extend_from_slice(&[ZDLE, b'l']),
                    0xff => wire.extend_from_slice(&[ZDLE, b'm']),
                    byte if byte & 0x60 == 0 => {
                        wire.extend_from_slice(&[ZDLE, byte ^ 0x40]);
                    }
                    byte => wire.push(byte),
                }
            }
        }
    }
    wire
}

fn encode(encoding: Encoding, frame: u8, flags: [u8; 4]) -> Vec<u8> {
    encode_body(encoding, &body(encoding, frame, flags))
}

#[derive(Default)]
struct Capture {
    rendered: Vec<u8>,
    protocol: Vec<u8>,
    start: Option<(Header, ZmodemRole)>,
}

impl Capture {
    fn push(&mut self, detector: &mut ZmodemDetector, bytes: &[u8], now: Instant) {
        if self.start.is_some() {
            self.protocol.extend_from_slice(bytes);
            return;
        }
        match detector.push(bytes, now) {
            DetectorOutcome::Render(bytes) => self.rendered.extend(bytes),
            DetectorOutcome::Started {
                header,
                role,
                render_before,
                protocol,
            } => {
                self.start = Some((header, role));
                self.rendered.extend(render_before);
                self.protocol.extend(protocol);
                assert_eq!(detector.next_deadline(), None);
                assert!(detector.flush().is_empty());
            }
        }
        assert!(detector.pending.len() <= MAX_PENDING);
    }
}

fn assert_start_at_every_split(wire: &[u8], expected: Header, role: ZmodemRole) {
    let prefix = b"ordinary output\r\nrz\r";
    let suffix = b"next protocol bytes\x00\xff*";
    let input = [prefix.as_slice(), wire, suffix.as_slice()].concat();
    let now = Instant::now();
    for split in 0..=input.len() {
        let mut detector = ZmodemDetector::new();
        let mut capture = Capture::default();
        capture.push(&mut detector, &input[..split], now);
        capture.push(&mut detector, &input[split..], now);
        assert_eq!(capture.start, Some((expected, role)), "split {split}");
        assert_eq!(capture.rendered, prefix, "split {split}");
        assert_eq!(capture.protocol, [wire, suffix.as_slice()].concat());
        assert_eq!([capture.rendered, capture.protocol].concat(), input);
    }
}

fn assert_roundtrip_at_every_split(input: &[u8]) {
    let now = Instant::now();
    for split in 0..=input.len() {
        let mut detector = ZmodemDetector::new();
        let mut capture = Capture::default();
        capture.push(&mut detector, &input[..split], now);
        capture.push(&mut detector, &input[split..], now);
        capture.rendered.extend(detector.flush());
        assert_eq!(capture.start, None, "split {split}: {input:?}");
        assert_eq!(capture.rendered, input, "split {split}");
        assert!(detector.flush().is_empty());
        assert_eq!(detector.next_deadline(), None);
    }
}

#[test]
fn captured_peer_headers_detect_at_every_split() {
    for (wire, encoding, frame, flags, role) in [
        (
            REAL_SZ_HEADER,
            Encoding::Zhex,
            Frame::Zrqinit,
            [0; 4],
            ZmodemRole::Download,
        ),
        (
            REAL_RZ_HEADER,
            Encoding::Zhex,
            Frame::Zrinit,
            [0, 0, 0, 0x23],
            ZmodemRole::Upload,
        ),
        (
            REAL_ZFILE_HEADER,
            Encoding::Zbin32,
            Frame::Zfile,
            [0, 0, 0, 1],
            ZmodemRole::Download,
        ),
    ] {
        assert_start_at_every_split(
            wire,
            Header {
                encoding,
                frame,
                flags,
            },
            role,
        );
    }
}

#[test]
fn all_start_frames_and_encodings_detect_at_every_split() {
    for encoding in [Encoding::Zhex, Encoding::Zbin, Encoding::Zbin32] {
        for (frame, role) in [
            (Frame::Zrqinit, ZmodemRole::Download),
            (Frame::Zrinit, ZmodemRole::Upload),
            (Frame::Zfile, ZmodemRole::Download),
        ] {
            for flags in [[0; 4], [0x18, 0x7f, 0xff, 0x91], [0x11, 0x13, 0x90, 0x93]] {
                let wire = encode(encoding, frame.frame_byte(), flags);
                assert_start_at_every_split(
                    &wire,
                    Header {
                        encoding,
                        frame,
                        flags,
                    },
                    role,
                );
            }
        }
    }
}

#[test]
fn repeated_pad_preamble_survives_bytewise_and_three_way_splits() {
    let now = Instant::now();
    for encoding in [Encoding::Zhex, Encoding::Zbin, Encoding::Zbin32] {
        let flags = [0x18, 0x7f, 0xff, 0x91];
        let header = encode(encoding, 4, flags);
        let mut wire = vec![ZPAD; 7];
        wire.extend(header);
        for first in 0..=wire.len() {
            for second in first..=wire.len() {
                let mut detector = ZmodemDetector::new();
                let mut capture = Capture::default();
                capture.push(&mut detector, &wire[..first], now);
                capture.push(&mut detector, &wire[first..second], now);
                capture.push(&mut detector, &wire[second..], now);
                assert!(capture.start.is_some(), "splits {first}, {second}");
                assert!(capture.rendered.is_empty());
                assert_eq!(capture.protocol, wire);
            }
        }
        let mut detector = ZmodemDetector::new();
        let mut capture = Capture::default();
        for byte in &wire {
            capture.push(&mut detector, &[*byte], now);
        }
        assert!(capture.start.is_some());
        assert_eq!(capture.protocol, wire);
        assert!(capture.rendered.is_empty());
    }
}

#[test]
fn every_non_start_frame_is_preserved_with_following_ordinary_output() {
    for encoding in [Encoding::Zhex, Encoding::Zbin, Encoding::Zbin32] {
        for frame in 0..=u8::MAX {
            if matches!(frame, 0 | 1 | 4) {
                continue;
            }
            let wire = encode(encoding, frame, [ZPAD, ZPAD, ZDLE, 0x7f]);
            let input = [b"before".as_slice(), &wire, b"\r\nafter**"].concat();
            assert_roundtrip_at_every_split(&input);
        }
    }
}

#[test]
fn unsupported_header_does_not_hide_the_next_start() {
    let now = Instant::now();
    let unsupported = encode(Encoding::Zbin32, 8, [ZPAD; 4]);
    let prefix = [unsupported.as_slice(), b"ordinary output\r\n"].concat();
    let input = [prefix.as_slice(), REAL_SZ_HEADER].concat();
    for split in 0..=input.len() {
        let mut detector = ZmodemDetector::new();
        let mut capture = Capture::default();
        capture.push(&mut detector, &input[..split], now);
        capture.push(&mut detector, &input[split..], now);
        assert!(capture.start.is_some());
        assert_eq!(capture.rendered, prefix);
        assert_eq!(capture.protocol, REAL_SZ_HEADER);
    }
}

#[test]
fn random_and_all_byte_output_roundtrips_with_random_chunks() {
    let mut rng = StdRng::seed_from_u64(0x5a4d_4f44_454d);
    let mut input: Vec<u8> = (0..=u8::MAX).collect();
    let mut random = vec![0; 128 * 1024];
    rng.fill_bytes(&mut random);
    input.extend(random);
    input.extend_from_slice(b"**\x18Bnot hex\r\n***");

    let now = Instant::now();
    for max_chunk in [1, 7, 64, 4096, input.len()] {
        let mut detector = ZmodemDetector::new();
        let mut capture = Capture::default();
        let mut offset = 0;
        while offset < input.len() {
            let len = (rng.next_u32() as usize % max_chunk + 1).min(input.len() - offset);
            capture.push(&mut detector, &input[offset..offset + len], now);
            offset += len;
        }
        capture.rendered.extend(detector.flush());
        assert_eq!(capture.start, None);
        assert_eq!(capture.rendered, input);
    }
}

#[test]
fn trailing_stars_expire_without_new_pty_input() {
    let now = Instant::now();
    for tail in [b"*".as_slice(), b"**"] {
        let mut detector = ZmodemDetector::new();
        let input = [b"prompt ".as_slice(), tail].concat();
        assert_eq!(
            detector.push(&input, now),
            DetectorOutcome::Render(b"prompt ".to_vec())
        );
        assert_eq!(detector.next_deadline(), Some(now + PENDING_TIMEOUT));
        assert!(detector.expire(now + Duration::from_millis(99)).is_empty());
        assert_eq!(detector.expire(now + PENDING_TIMEOUT), tail);
        assert_eq!(detector.next_deadline(), None);
        assert!(detector.expire(now + PENDING_TIMEOUT).is_empty());
        assert!(detector.flush().is_empty());
    }
}

#[test]
fn fragments_do_not_postpone_the_first_byte_deadline() {
    let now = Instant::now();
    let mut detector = ZmodemDetector::new();
    assert_eq!(detector.push(b"*", now), DetectorOutcome::Render(vec![]));
    assert_eq!(
        detector.push(b"*\x18B0000", now + Duration::from_millis(90)),
        DetectorOutcome::Render(vec![])
    );
    assert_eq!(detector.next_deadline(), Some(now + PENDING_TIMEOUT));
    assert_eq!(detector.expire(now + PENDING_TIMEOUT), b"**\x18B0000");
}

#[test]
fn push_expires_old_candidate_before_accepting_more_bytes() {
    let now = Instant::now();
    let mut detector = ZmodemDetector::new();
    detector.push(&REAL_SZ_HEADER[..8], now);
    assert_eq!(
        detector.push(&REAL_SZ_HEADER[8..], now + PENDING_TIMEOUT),
        DetectorOutcome::Render(REAL_SZ_HEADER.to_vec())
    );
    assert!(detector.flush().is_empty());

    detector.push(b"**", now);
    let outcome = detector.push(REAL_RZ_HEADER, now + PENDING_TIMEOUT);
    let DetectorOutcome::Started {
        render_before,
        protocol,
        role,
        ..
    } = outcome
    else {
        panic!("expected a new start after the expired candidate");
    };
    assert_eq!(render_before, b"**");
    assert_eq!(protocol, REAL_RZ_HEADER);
    assert_eq!(role, ZmodemRole::Upload);
}

#[test]
fn flush_releases_every_incomplete_prefix_once() {
    let now = Instant::now();
    for encoding in [Encoding::Zhex, Encoding::Zbin, Encoding::Zbin32] {
        let mut wire = encode(encoding, 4, [0x18, 0x7f, 0xff, 0x91]);
        if encoding == Encoding::Zhex {
            wire.pop();
        }
        for end in 0..wire.len() {
            let mut detector = ZmodemDetector::new();
            assert_eq!(
                detector.push(&wire[..end], now),
                DetectorOutcome::Render(vec![])
            );
            assert_eq!(detector.flush(), wire[..end]);
            assert!(detector.flush().is_empty());
            assert_eq!(detector.next_deadline(), None);
            assert_eq!(
                detector.push(b"ordinary", now),
                DetectorOutcome::Render(b"ordinary".to_vec())
            );
        }
    }
}

#[test]
fn excessive_pads_stay_bounded_and_preserve_the_whole_stream() {
    let now = Instant::now();
    let mut detector = ZmodemDetector::new();
    let pads = vec![ZPAD; 64 * 1024];
    let mut capture = Capture::default();
    for chunk in pads.chunks(7) {
        capture.push(&mut detector, chunk, now);
    }
    assert_eq!(detector.pending.len(), MAX_PADS);
    assert_eq!(detector.next_deadline(), Some(now + PENDING_TIMEOUT));
    let header_tail = &REAL_SZ_HEADER[2..];
    capture.push(&mut detector, header_tail, now);
    assert!(capture.start.is_some());
    assert_eq!(capture.rendered.len(), pads.len() - MAX_PADS);
    assert_eq!(
        [capture.rendered, capture.protocol].concat(),
        [pads.as_slice(), header_tail].concat()
    );
}

#[test]
fn corrupt_headers_roundtrip_at_every_split() {
    for encoding in [Encoding::Zhex, Encoding::Zbin, Encoding::Zbin32] {
        let original = body(encoding, 4, [0x18, 0x7f, 0xff, 0x91]);
        for index in 0..original.len() {
            let mut corrupted = original.clone();
            corrupted[index] ^= 1;
            let wire = encode_body(encoding, &corrupted);
            assert_roundtrip_at_every_split(&[wire.as_slice(), b"ordinary output"].concat());
        }
    }
}

#[test]
fn hex_framing_must_be_complete_and_strict() {
    for invalid in [
        b"*\x18B00000000000000\r\n".as_slice(),
        b"**B00000000000000\r\n",
        b"**\x18Z00000000000000\r\n",
        b"**\x18B00000000000000x\n",
        b"**\x18B00000000000000\rx",
        b"**\x18B00000000000000\n",
        b"**\x18B00000000000000\r",
        b"**\x18B00000000000000",
        b"**\x18B0000000000000g\r\n",
        b"**\x18B\x18000000000000000\r\n",
    ] {
        assert_roundtrip_at_every_split(invalid);
    }
}

#[test]
fn hex_case_parity_and_optional_xon_are_supported() {
    for trailer in [b"\r\n".as_slice(), b"\r\x8a", b"\x8d\x8a\x11"] {
        let mut wire = b"**\x18B0100000023BE50".to_vec();
        wire.extend_from_slice(trailer);
        assert_start_at_every_split(
            &wire,
            Header {
                encoding: Encoding::Zhex,
                frame: Frame::Zrinit,
                flags: [0, 0, 0, 0x23],
            },
            ZmodemRole::Upload,
        );
    }
}

#[test]
fn invalid_binary_escapes_cannot_start_a_transfer() {
    for encoding in [Encoding::Zbin, Encoding::Zbin32] {
        for invalid_escape in [0x00, 0x18, b'0', b'a', b'h', b'i', b'j', b'k', b'n', 0xff] {
            let mut wire = vec![ZPAD, ZDLE, encoding as u8, ZDLE, invalid_escape];
            wire.extend_from_slice(b"ordinary output**");
            assert_roundtrip_at_every_split(&wire);
        }

        // A permissive identity unescape table would accept this otherwise CRC-valid header.
        let mut wire = encode(encoding, 0, [0; 4]);
        assert_eq!(&wire[3..5], &[ZDLE, 0x40]);
        wire[4] = 0;
        assert_roundtrip_at_every_split(&wire);

        for flow_control in [0x11, 0x13, 0x91, 0x93] {
            let mut wire = encode(encoding, 4, [flow_control, 0, 0, 0]);
            assert_eq!(&wire[5..7], &[ZDLE, flow_control ^ 0x40]);
            wire[5] = flow_control;
            wire.remove(6);
            assert_roundtrip_at_every_split(&wire);
        }
    }
}

#[test]
fn binary_flags_cover_all_byte_values_and_escape_splits() {
    for encoding in [Encoding::Zbin, Encoding::Zbin32] {
        for flag in 0..=u8::MAX {
            let flags = [flag; 4];
            let wire = encode(encoding, 4, flags);
            assert_start_at_every_split(
                &wire,
                Header {
                    encoding,
                    frame: Frame::Zfile,
                    flags,
                },
                ZmodemRole::Download,
            );
        }
    }
}

#[test]
fn invalid_candidate_resynchronizes_on_a_fragmented_start() {
    let now = Instant::now();
    let prefix = b"**\x18B0000garbage\r\n*\x18C\x04";
    let input = [prefix.as_slice(), REAL_SZ_HEADER].concat();
    for split in 0..=input.len() {
        let mut detector = ZmodemDetector::new();
        let mut capture = Capture::default();
        capture.push(&mut detector, &input[..split], now);
        capture.push(&mut detector, &input[split..], now);
        assert!(capture.start.is_some());
        assert_eq!(capture.rendered, prefix);
        assert_eq!(capture.protocol, REAL_SZ_HEADER);
    }
}

#[test]
fn crc_algorithms_match_independent_check_vectors() {
    assert_eq!(crc16_xmodem(b"123456789"), 0x31c3);
    let mut crc = Crc::new();
    crc.update(b"123456789");
    assert_eq!(crc.sum(), 0xcbf4_3926);
}

#[test]
fn detector_reuse_does_not_replay_previous_protocol_bytes() {
    let now = Instant::now();
    let mut detector = ZmodemDetector::new();
    for wire in [REAL_SZ_HEADER, REAL_RZ_HEADER, REAL_ZFILE_HEADER] {
        let DetectorOutcome::Started {
            render_before,
            protocol,
            ..
        } = detector.push(wire, now)
        else {
            panic!("expected a transfer start");
        };
        assert!(render_before.is_empty());
        assert_eq!(protocol, wire);
        assert_eq!(detector.next_deadline(), None);
        assert!(detector.expire(now + PENDING_TIMEOUT).is_empty());
        assert!(detector.flush().is_empty());
        assert_eq!(
            detector.push(b"prompt\r\n", now),
            DetectorOutcome::Render(b"prompt\r\n".to_vec())
        );
    }
}
