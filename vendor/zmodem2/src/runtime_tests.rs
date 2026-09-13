// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::header::{Encoding, Frame, Header};
use crate::wire::BufferWriter;
use crate::{Action, Event, FileInfo, Position, Receiver, Sender};

const ESCCTL_ZRINIT: u32 = 0x4000_0000;

fn header(frame: Frame, count: u32) -> Vec<u8> {
    let mut buffer = crate::buffer::Buffer::<128>::new();
    Header::new(Encoding::ZHEX, frame, count.to_le_bytes())
        .write(&mut BufferWriter::new(&mut buffer)).unwrap().unwrap();
    buffer.to_vec()
}

fn drain_sender(sender: &mut Sender) -> Vec<u8> {
    let mut result = Vec::new();
    loop {
        match sender.poll() {
            Action::WriteWire(bytes) => {
                let count = bytes.len();
                result.extend_from_slice(bytes);
                sender.wire_written(count);
            }
            Action::Idle => return result,
            action => panic!("Unexpected action: {action:?}"),
        }
    }
}

fn drain_receiver(receiver: &mut Receiver) -> Vec<u8> {
    let mut result = Vec::new();
    loop {
        match receiver.poll() {
            Action::WriteWire(bytes) => {
                let count = bytes.len();
                result.extend_from_slice(bytes);
                receiver.wire_written(count);
            }
            Action::Idle => return result,
            action => panic!("Unexpected action: {action:?}"),
        }
    }
}

#[test]
fn sender_start_skip_and_strict_finish_tail() {
    let mut sender = Sender::new().unwrap();
    drain_sender(&mut sender);
    sender.start_file(FileInfo::new(b"empty", Some(Position::ZERO))).unwrap();
    assert!(matches!(sender.poll(), Action::Event(Event::FileStarted(_))));
    sender
        .submit_wire(&header(Frame::ZRINIT, ESCCTL_ZRINIT))
        .unwrap();
    let wire = drain_sender(&mut sender);
    assert!(wire.windows(b"0 0 100644 0 1 0".len()).any(|part| part == b"0 0 100644 0 1 0"));
    sender.submit_wire(&header(Frame::ZSKIP, 0)).unwrap();
    assert_eq!(sender.poll(), Action::Event(Event::FileSkipped));
    sender.finish().unwrap();
    drain_sender(&mut sender);
    sender
        .submit_wire(&header(Frame::ZRINIT, ESCCTL_ZRINIT))
        .unwrap();
    assert!(!drain_sender(&mut sender).is_empty());
    let mut finish = header(Frame::ZFIN, 0);
    let boundary = finish.len();
    finish.extend_from_slice(b"prompt> ");
    assert_eq!(sender.submit_wire(&finish).unwrap(), boundary);
    assert_eq!(sender.poll(), Action::Event(Event::SessionCompleted));
    assert_eq!(drain_sender(&mut sender), b"OO");
}

#[test]
fn receiver_requires_fragmented_oo_and_preserves_tail() {
    let mut receiver = Receiver::new().unwrap();
    drain_receiver(&mut receiver);
    let finish = header(Frame::ZFIN, 0);
    for byte in finish {
        assert_eq!(receiver.submit_wire(&[byte]).unwrap(), 1);
    }
    assert!(!drain_receiver(&mut receiver).is_empty());
    assert_eq!(receiver.submit_wire(b"O").unwrap(), 1);
    assert_eq!(receiver.poll(), Action::Idle);
    assert_eq!(receiver.submit_wire(b"O\r\nprompt> ").unwrap(), 1);
    assert_eq!(receiver.poll(), Action::Event(Event::SessionCompleted));
    assert_eq!(receiver.submit_wire(b"untouched").unwrap(), 0);
}

#[test]
fn receiver_does_not_consume_input_while_acceptance_is_pending() {
    let mut sender = Sender::new().unwrap();
    drain_sender(&mut sender);
    sender.start_file(FileInfo::new(b"empty", Some(Position::ZERO))).unwrap();
    assert!(matches!(sender.poll(), Action::Event(Event::FileStarted(_))));
    sender
        .submit_wire(&header(Frame::ZRINIT, ESCCTL_ZRINIT))
        .unwrap();
    let file = drain_sender(&mut sender);
    let mut receiver = Receiver::new().unwrap();
    receiver.set_manual_file_accept(true);
    drain_receiver(&mut receiver);
    let mut offset = 0;
    while offset < file.len() {
        offset += receiver.submit_wire(&file[offset..]).unwrap();
    }
    assert!(matches!(receiver.poll(), Action::Event(Event::FileStarted(_))));
    assert_eq!(receiver.submit_wire(&header(Frame::ZEOF, 0)).unwrap(), 0);
    receiver.accept_file_at(0).unwrap();
    drain_receiver(&mut receiver);
    receiver.submit_wire(&header(Frame::ZEOF, 0)).unwrap();
    assert_eq!(receiver.poll(), Action::Event(Event::FileCompleted));
    assert!(matches!(receiver.poll(), Action::WriteWire(_)));
}

#[test]
fn sender_timeout_restarts_an_unacknowledged_data_window() {
    let mut sender = Sender::new().unwrap();
    drain_sender(&mut sender);
    sender
        .start_file(FileInfo::new(b"payload", Some(Position::new(2048))))
        .unwrap();
    assert!(matches!(
        sender.poll(),
        Action::Event(Event::FileStarted(_))
    ));
    sender
        .submit_wire(&header(Frame::ZRINIT, ESCCTL_ZRINIT))
        .unwrap();
    drain_sender(&mut sender);
    sender.submit_wire(&header(Frame::ZRPOS, 0)).unwrap();
    let Action::ReadFile { offset, max_len } = sender.poll() else {
        panic!("sender did not request the first file chunk");
    };
    assert_eq!(offset, Position::ZERO);
    sender.submit_file(&vec![0x5a; max_len]).unwrap();
    drain_sender(&mut sender);
    assert_eq!(sender.poll(), Action::Idle);

    sender.timeout().unwrap();
    assert_eq!(
        sender.poll(),
        Action::ReadFile {
            offset: Position::ZERO,
            max_len,
        }
    );
}

#[test]
fn sender_negotiates_control_escaping_before_file_offer() {
    let mut sender = Sender::new().unwrap();
    drain_sender(&mut sender);
    sender
        .start_file(FileInfo::new(b"payload", Some(Position::new(1))))
        .unwrap();
    assert!(matches!(
        sender.poll(),
        Action::Event(Event::FileStarted(_))
    ));

    sender.submit_wire(&header(Frame::ZRINIT, 0)).unwrap();
    let sinit = drain_sender(&mut sender);
    assert!(sinit.windows(b"0200000040".len()).any(|bytes| bytes == b"0200000040"));

    sender.submit_wire(&header(Frame::ZACK, 0)).unwrap();
    let offer = drain_sender(&mut sender);
    assert!(offer.windows(b"payload".len()).any(|bytes| bytes == b"payload"));
}
