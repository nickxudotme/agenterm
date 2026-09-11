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
    assert_eq!(detector.push(b"rz\r**\x18B0000"), DetectorOutcome::Render(b"rz\r".to_vec()));

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
    assert_eq!(zrqinit_sequence(), REAL_SZ_PREAMBLE.strip_prefix(b"rz\r").unwrap());
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
