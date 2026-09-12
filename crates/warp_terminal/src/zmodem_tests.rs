use std::path::PathBuf;

use super::*;

/// The first bytes real `lrzsz` `sz` writes: `rz\r` then a `ZRQINIT` header
/// with all-zero flags. Captured from `sz --zmodem <file>`.
///
/// The trailer is `\r`, `0x8a` (LF with the high bit set), then XON.
const REAL_SZ_PREAMBLE: &[u8] = b"rz\r**\x18B00000000000000\r\x8a\x11";

/// Extracts the header from a [`DetectorOutcome::Started`].
fn started(outcome: DetectorOutcome) -> (Header, Vec<u8>, Vec<u8>) {
    match outcome {
        DetectorOutcome::Started {
            header,
            render_before,
            protocol,
        } => (header, render_before, protocol),
        DetectorOutcome::Render(_) => panic!("expected a transfer to start"),
    }
}

#[test]
fn detects_zrqinit_from_real_sz() {
    let mut detector = ZmodemDetector::new();
    let (header, render_before, protocol) = started(detector.push(REAL_SZ_PREAMBLE));

    assert_eq!(header.frame, Frame::Zrqinit);
    assert_eq!(header.encoding, Encoding::Zhex);
    assert_eq!(header.count(), 0);
    // `rz\r` precedes the header and still renders normally.
    assert_eq!(render_before, b"rz\r");
    assert_eq!(protocol, REAL_SZ_PREAMBLE.strip_prefix(b"rz\r").unwrap());
}

#[test]
fn renders_plain_text_unchanged() {
    let mut detector = ZmodemDetector::new();
    match detector.push(b"hello, world\n") {
        DetectorOutcome::Render(bytes) => assert_eq!(bytes, b"hello, world\n"),
        other => panic!("expected render, got {other:?}"),
    }
}

#[test]
fn does_not_detect_lone_zpad_run() {
    let mut detector = ZmodemDetector::new();
    match detector.push(b"***") {
        DetectorOutcome::Render(_) => {}
        other => panic!("expected render, got {other:?}"),
    }
}

#[test]
fn does_not_detect_zpad_zdle_with_bad_crc() {
    // Well-formed framing, but the CRC does not match the payload.
    let mut detector = ZmodemDetector::new();
    match detector.push(b"**\x18B0100000000ffff\r\x8a\x11") {
        DetectorOutcome::Render(_) => {}
        other => panic!("expected render, got {other:?}"),
    }
}

#[test]
fn does_not_detect_unknown_encoding_byte() {
    let mut detector = ZmodemDetector::new();
    match detector.push(b"**\x18Z0000000000\r\x8a\x11") {
        DetectorOutcome::Render(_) => {}
        other => panic!("expected render, got {other:?}"),
    }
}

#[test]
fn detects_header_split_across_reads() {
    let mut detector = ZmodemDetector::new();
    // The split header must be held back rather than rendered as garbage.
    assert_eq!(
        detector.push(b"rz\r**\x18B0000"),
        DetectorOutcome::Render(b"rz\r".to_vec())
    );

    let (header, render_before, _) = started(detector.push(b"0000000000\r\x8a\x11"));
    assert_eq!(header.frame, Frame::Zrqinit);
    assert!(render_before.is_empty());
}

#[test]
fn detects_header_after_garbage_prefix() {
    let mut detector = ZmodemDetector::new();
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"noise that is not zmodem at all\n");
    bytes.extend_from_slice(REAL_SZ_PREAMBLE);

    let (header, render_before, _) = started(detector.push(&bytes));
    assert_eq!(header.frame, Frame::Zrqinit);
    assert_eq!(
        render_before,
        b"noise that is not zmodem at all\nrz\r".to_vec()
    );
}

#[test]
fn releases_held_bytes_once_header_cannot_complete() {
    let mut detector = ZmodemDetector::new();
    // A pad run that never becomes a header must not swallow the following text.
    match detector.push(b"**\x18B0000xxxx\n") {
        DetectorOutcome::Render(bytes) => assert_eq!(bytes, b"**\x18B0000xxxx\n"),
        other => panic!("expected render, got {other:?}"),
    }
}

#[test]
fn does_not_detect_binary_that_only_looks_similar() {
    // Every byte value, which includes `*` and control characters.
    let bytes: Vec<u8> = (0..=255u8).collect();
    let mut detector = ZmodemDetector::new();
    assert!(matches!(detector.push(&bytes), DetectorOutcome::Render(_)));
}

#[test]
fn round_trips_generated_hex_header() {
    let mut detector = ZmodemDetector::new();
    let generated = zrqinit_sequence();
    let (header, render_before, protocol) = started(detector.push(&generated));
    assert_eq!(header.frame, Frame::Zrqinit);
    assert!(render_before.is_empty());
    assert_eq!(protocol, generated);
}

#[test]
fn zrqinit_sequence_matches_expected_wire_bytes() {
    // `ZRQINIT` is frame 0 with all-zero flags; `lrzsz` emits exactly this.
    assert_eq!(
        zrqinit_sequence(),
        REAL_SZ_PREAMBLE.strip_prefix(b"rz\r").unwrap()
    );
}

#[test]
fn abort_sequence_starts_with_three_pads_and_can() {
    let seq = abort_sequence();
    assert_eq!(&seq[..3], &[ZPAD, ZPAD, ZPAD]);
    assert_eq!(seq[3], CAN);
    assert_eq!(seq.len(), 13);
}

#[test]
fn crc_helpers_match_known_vectors() {
    // CRC-16/XMODEM check value.
    assert_eq!(crc16_xmodem(b"123456789"), 0x31C3);
    // CRC-32/ISO-HDLC check value.
    assert_eq!(crc32_iso_hdlc(b"123456789"), 0xCBF4_3926);
}

#[test]
fn encodes_header_with_zdle_in_body() {
    // Flags that make a `ZDLE` appear in the hex body, forcing an escape.
    let header = encode_hex_header(Frame::Zfile, &[ZDLE, ZDLE, ZDLE, ZDLE]);
    let mut detector = ZmodemDetector::new();
    let (found, _, _) = started(detector.push(&header));
    assert_eq!(found.frame, Frame::Zfile);
    assert_eq!(found.flags, [ZDLE, ZDLE, ZDLE, ZDLE]);
}

#[test]
fn sink_collects_and_takes_bytes() {
    let mut sink = PtySink::new();
    std::io::Write::write_all(&mut sink, b"abc").unwrap();
    std::io::Write::write_all(&mut sink, b"def").unwrap();
    assert_eq!(sink.take(), b"abcdef");
    assert!(sink.take().is_empty());
}

#[test]
fn upload_session_queues_files() {
    let files = vec![
        UploadFile {
            path: PathBuf::from("/tmp/a.txt"),
            name: b"a.txt".to_vec(),
            size: 4,
        },
        UploadFile {
            path: PathBuf::from("/tmp/b.txt"),
            name: b"b.txt".to_vec(),
            size: 8,
        },
    ];
    let mut session = ZmodemSession::new_upload(files).expect("upload session");
    assert_eq!(session.role(), ZmodemRole::Upload);
    assert!(!session.is_finished());

    // The sender opens with a ZRQINIT so a waiting `rz` starts handshaking.
    let initial = session.initial_output().expect("initial output");
    let mut detector = ZmodemDetector::new();
    let (header, _, _) = started(detector.push(&initial));
    assert_eq!(header.frame, Frame::Zrqinit);
}

#[test]
fn download_session_opens_with_zrinit() {
    let mut session = ZmodemSession::new_download().expect("download session");
    assert_eq!(session.role(), ZmodemRole::Download);

    // The receiver advertises its capabilities before any data arrives.
    let initial = session.initial_output().expect("initial output");
    let mut detector = ZmodemDetector::new();
    let (header, _, _) = started(detector.push(&initial));
    assert_eq!(header.frame, Frame::Zrinit);
}

#[test]
fn download_consumes_real_sz_handshake_without_rendering() {
    let mut session = ZmodemSession::new_download().expect("download session");
    let _ = session.initial_output().expect("initial output");

    // Feed the exact ZRQINIT that `sz` emits; it must be accepted, and the
    // receiver must answer rather than leave the transfer silent.
    let protocol = REAL_SZ_PREAMBLE.strip_prefix(b"rz\r").unwrap();
    let step = session.submit_wire(protocol).expect("submit_wire");
    assert!(!step.finished, "handshake must not end the session");
    assert!(
        !step.to_pty.is_empty(),
        "receiver must reply to the sender's ZRQINIT"
    );
}

#[test]
fn upload_only_sessions_ignore_download_operations() {
    // Guards the role split: a download must never be asked for file bytes.
    let mut download = ZmodemSession::new_download().expect("download session");
    assert!(download.offer_next_file().expect("no-op").is_empty());
    assert!(download.submit_file(b"data").expect("no-op").is_empty());
}

#[test]
fn abort_returns_cancel_sequence_for_both_roles() {
    let mut download = ZmodemSession::new_download().expect("download session");
    assert_eq!(download.abort(), abort_sequence());

    let mut upload = ZmodemSession::new_upload(Vec::new()).expect("upload session");
    assert_eq!(upload.abort(), abort_sequence());
}

#[test]
fn download_consumes_the_detected_header_bytes() {
    // The event loop hands the detected header straight to a fresh session.
    // If the session cannot consume those bytes, the handshake never advances
    // and `sz` waits forever.
    let mut session = ZmodemSession::new_download().expect("download session");
    let protocol = REAL_SZ_PREAMBLE.strip_prefix(b"rz\r").unwrap();

    let step = session.submit_wire(protocol).expect("submit_wire");
    assert!(
        !step.to_pty.is_empty(),
        "receiver must answer the sender's ZRQINIT"
    );

    // Feeding the same header again must not be required: the first call has
    // to make progress, or the transfer deadlocks.
    let second = session.submit_wire(protocol).expect("submit_wire");
    assert!(
        !second.to_pty.is_empty() || second.finished,
        "session made no progress across two identical reads"
    );
}

#[test]
fn detects_sz_when_shell_echoes_the_command_first() {
    // In a real terminal the shell echoes `sz file` and a newline before the
    // protocol starts, and the PTY usually delivers that in the same read.
    let mut detector = ZmodemDetector::new();
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"sz payload.txt\r\n");
    bytes.extend_from_slice(REAL_SZ_PREAMBLE);

    let (header, _, _) = started(detector.push(&bytes));
    assert_eq!(header.frame, Frame::Zrqinit);
}

#[test]
fn detects_sz_arriving_one_byte_at_a_time() {
    // A PTY can deliver the preamble in arbitrarily small reads; detection
    // must not depend on the header landing in a single chunk.
    let mut detector = ZmodemDetector::new();
    let mut found = None;
    for byte in REAL_SZ_PREAMBLE {
        if let DetectorOutcome::Started { header, .. } = detector.push(&[*byte]) {
            found = Some(header);
            break;
        }
    }
    assert_eq!(
        found.map(|header| header.frame),
        Some(Frame::Zrqinit),
        "byte-at-a-time delivery must still detect the header"
    );
}

#[test]
fn holds_back_bytes_exactly_once_across_reads() {
    // Held-back bytes must be returned for rendering exactly once. Returning
    // them again on a later read duplicates output on screen.
    let mut detector = ZmodemDetector::new();
    let mut rendered = Vec::new();
    for byte in b"hello*" {
        if let DetectorOutcome::Render(bytes) = detector.push(&[*byte]) {
            rendered.extend_from_slice(&bytes);
        }
    }
    // The trailing `*` is a header candidate, so only `hello` may render.
    assert_eq!(rendered, b"hello");

    // A non-pad byte settles it: the `*` is ordinary text after all.
    if let DetectorOutcome::Render(bytes) = detector.push(b"x") {
        rendered.extend_from_slice(&bytes);
    }
    assert_eq!(
        rendered, b"hello*x",
        "held bytes must render once, in order"
    );
}
