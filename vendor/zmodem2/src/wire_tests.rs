use super::*;

#[test]
fn decode_zbin32_header() {
    let data = [
        Frame::ZRINIT as u8,
        0x0a,
        0x0b,
        0x0c,
        0x0d,
        0x99,
        0xe2,
        0xae,
        0x4a,
    ];
    let header = decode_header(Encoding::ZBIN32, &data).unwrap();

    assert_eq!(header.encoding(), Encoding::ZBIN32);
    assert_eq!(header.frame(), Frame::ZRINIT);
    assert_eq!(header.count(), u32::from_le_bytes([0x0a, 0x0b, 0x0c, 0x0d]));
}

#[test]
fn decode_zbin32_bad_crc() {
    let data = [Frame::ZRINIT as u8, 0x0a, 0x0b, 0x0c, 0x0d, 0, 0, 0, 0];

    assert!(matches!(
        decode_header(Encoding::ZBIN32, &data),
        Err(Error::UnexpectedCrc32)
    ));
}

#[test]
fn escaped_subpacket_has_no_bare_line_feed() {
    let mut buffer = Buffer::<128>::new();
    write_subpacket(
        &mut BufferWriter::new(&mut buffer),
        Encoding::ZBIN32,
        SubpacketType::ZCRCE,
        b"\n~I",
    )
    .unwrap()
    .unwrap();

    assert!(buffer.windows(2).any(|bytes| bytes == [ZDLE, 0x4a]));
    assert!(!buffer.contains(&b'\n'));
}
