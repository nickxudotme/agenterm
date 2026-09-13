use super::*;

const RZ_HEADER: &[u8] = b"**\x18B0100000023be50\r\x8a\x11";

fn listener() -> (ChannelEventListener, async_channel::Receiver<TerminalEvent>) {
    let (sender, receiver) = async_channel::unbounded();
    let listener = ChannelEventListener::builder_for_test()
        .with_terminal_events_tx(sender)
        .build();
    (listener, receiver)
}

#[test]
fn disabled_router_preserves_protocol_and_ordinary_bytes() {
    let (listener, events) = listener();
    let mut router = ZmodemRouter::default();
    let bytes = [b"prefix\x00".as_slice(), RZ_HEADER, b"prompt **"].concat();
    assert_eq!(router.route(&bytes, &listener), bytes);
    assert!(!router.busy());
    assert!(events.is_empty());
    assert_eq!(router.next_timeout(Instant::now()), None);
}

#[test]
fn candidate_expires_without_another_read_and_disable_flushes_once() {
    let (listener, _) = listener();
    let mut router = ZmodemRouter::default();
    router.control(Control::Enable(true), &listener);
    assert_eq!(router.route(b"ordinary **", &listener), b"ordinary ");
    let deadline = router.detector.next_deadline().unwrap();
    assert_eq!(router.poll(deadline, &listener), b"**");
    assert!(router.poll(deadline, &listener).is_empty());
    assert_eq!(router.next_timeout(deadline), None);
    assert!(router.route(b"*", &listener).is_empty());
    assert_eq!(router.control(Control::Enable(false), &listener), b"*");
    assert!(router.control(Control::Enable(false), &listener).is_empty());
}

#[test]
fn waiting_request_is_single_bounded_and_rejects_expired_selection() {
    let (listener, events) = listener();
    let mut router = ZmodemRouter::default();
    router.control(Control::Enable(true), &listener);
    router.route(RZ_HEADER, &listener);
    let id = router.active.as_ref().unwrap().id;
    assert!(matches!(events.try_recv().unwrap(), TerminalEvent::Zmodem(
        TransferEvent::Requested { id: requested, role: Role::Upload }
    ) if requested == id));
    router.route(RZ_HEADER, &listener);
    assert!(events.is_empty());
    assert!(!router.active.as_ref().unwrap().configured);
    let active = router.active.as_mut().unwrap();
    let remaining = MAX_RETAINED_INPUT - active.retained_len();
    active.retain(&vec![b'x'; remaining]);
    assert_eq!(router.read_capacity(), 0);
    assert_eq!(
        router.active.as_ref().unwrap().retained_len(),
        MAX_RETAINED_INPUT
    );
    assert_eq!(
        router.next_timeout(Instant::now()),
        Some(WORKER_POLL_INTERVAL)
    );

    router.active.as_mut().unwrap().choice_deadline = Instant::now();
    router.control(
        Control::Upload {
            id,
            paths: vec!["never-read".into()],
        },
        &listener,
    );
    assert!(!router.active.as_ref().unwrap().configured);
    router.poll(Instant::now(), &listener);
    assert_eq!(
        router.wire.front().unwrap().buffer.remaining_bytes(),
        abort_sequence()
    );
    assert!(matches!(events.try_recv().unwrap(), TerminalEvent::Zmodem(
        TransferEvent::Finished { id: finished, outcome: TransferOutcome::Failed(
            TransferError { kind: TransferErrorKind::Timeout, .. }
        ), .. }
    ) if finished == id));
}

#[test]
fn late_upload_callback_cannot_arm_an_idle_router() {
    let (listener, events) = listener();
    let mut router = ZmodemRouter::default();
    router.control(Control::Enable(true), &listener);
    assert!(
        router
            .control(
                Control::Upload {
                    id: next_transfer_id(),
                    paths: vec!["never-read".into()],
                },
                &listener
            )
            .is_empty()
    );
    assert!(!router.busy());
    assert!(events.is_empty());
}

#[test]
fn abort_recovery_suppresses_all_protocol_residue() {
    let (listener, events) = listener();
    let mut router = ZmodemRouter::default();
    router.control(Control::Enable(true), &listener);
    router.queue_abort(next_transfer_id());
    let initial_deadline = router.recovery_deadline.unwrap();
    let input = [RZ_HEADER, b"shell> \x00\xff"].concat();
    let mut rendered = Vec::new();
    for byte in input {
        rendered.extend(router.route(&[byte], &listener));
    }
    assert!(rendered.is_empty());
    assert!(events.is_empty());
    assert!(router.active.is_none());
    let deadline = router.recovery_deadline.unwrap();
    assert!(deadline >= initial_deadline);
    router.poll(deadline, &listener);
    assert!(router.recovery_deadline.is_none());
    assert_eq!(router.route(b"shell> ", &listener), b"shell> ");
}

struct PartialWriter {
    bytes: Vec<u8>,
    allowance: usize,
}

impl Write for PartialWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.allowance == 0 {
            return Err(io::Error::from(ErrorKind::WouldBlock));
        }
        let count = bytes.len().min(self.allowance);
        self.bytes.extend_from_slice(&bytes[..count]);
        self.allowance -= count;
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn partial_wire_keeps_sequence_and_abort_replaces_unwritten_suffix() {
    let id = next_transfer_id();
    let mut router = ZmodemRouter::default();
    router.wire.push_back(TransferWriting {
        id,
        sequence: Some(7),
        buffer: Writing::new(Cow::Owned(vec![0, 1, 2, 3, 255])),
    });
    let mut writer = PartialWriter {
        bytes: Vec::new(),
        allowance: 2,
    };
    let mut can_write = true;
    router.write(&mut writer, &mut can_write).unwrap();
    assert!(!can_write);
    assert_eq!(writer.bytes, [0, 1]);
    assert_eq!(router.wire.front().unwrap().sequence, Some(7));
    assert_eq!(
        router.wire.front().unwrap().buffer.remaining_bytes(),
        [2, 3, 255]
    );
    router.queue_abort(id);
    writer.allowance = 100;
    can_write = true;
    router.write(&mut writer, &mut can_write).unwrap();
    assert_eq!(writer.bytes, [vec![0, 1], abort_sequence()].concat());
    assert!(router.wire.is_empty());
}

#[test]
fn write_budget_and_deadlines_allow_control_channel_service() {
    let mut writer = Vec::new();
    let mut buffer = Writing::new(Cow::Owned(vec![0xff; IO_BUDGET + 1]));
    let mut can_write = true;
    assert!(!write_buffer(&mut writer, &mut buffer, &mut can_write).unwrap());
    assert_eq!(writer.len(), IO_BUDGET);
    assert!(can_write);
    assert!(write_buffer(&mut writer, &mut buffer, &mut can_write).unwrap());
    assert_eq!(writer.len(), IO_BUDGET + 1);
    assert_eq!(
        minimum_timeout(Some(Duration::from_secs(1)), Some(WORKER_POLL_INTERVAL)),
        Some(WORKER_POLL_INTERVAL)
    );
}

#[test]
fn status_names_cannot_inject_terminal_commands() {
    assert_eq!(
        sanitized_name("name\x1b]52;secret\x07\r\n\u{202e}txt"),
        "name?]52;secret????txt"
    );
    assert_eq!(sanitized_name(&"x".repeat(1000)).len(), 256);
    let line = status_line(
        &TransferEvent::FileStarted {
            id: 1,
            role: Role::Download,
            name: "file\n\x1b[2J".to_owned(),
            size: None,
        },
        None,
    );
    assert_eq!(line.iter().filter(|&&byte| byte == 0x1b).count(), 3);
    assert!(!String::from_utf8(line).unwrap().contains("\x1b[2J"));
}

#[test]
fn status_uses_remote_command_direction_and_weterm_style() {
    let download = status_line(
        &TransferEvent::Progress {
            id: 1,
            role: Role::Download,
            name: "archive.tar".to_owned(),
            bytes: 32 * 1024 * 1024,
            total: Some(64 * 1024 * 1024),
        },
        Some(4 * 1024 * 1024),
    );
    let download = String::from_utf8(download).unwrap();
    assert!(download.starts_with("\x1b[1A\r\x1b[2K"));
    assert!(download.ends_with("\r\n\x1b[0m"));
    assert!(
        download.contains("\x1b[32msz << archive.tar::50%, 32.0 MiB/64.0 MiB, 4.0 MiB/s, ETA 8s")
    );

    let upload = status_line(
        &TransferEvent::Requested {
            id: 2,
            role: Role::Upload,
        },
        None,
    );
    assert!(
        String::from_utf8(upload)
            .unwrap()
            .contains("** rz waiting to receive, please choose files **",)
    );
}

#[test]
fn progress_omits_eta_until_rate_is_available() {
    assert_eq!(progress_detail(512, Some(1024), None), "50%, 512 B/1 KiB");
    assert_eq!(format_duration(65), "1m05s");
    assert_eq!(format_duration(3 * 60 * 60 + 20 * 60), "3h20m");
}
