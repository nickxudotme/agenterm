use super::*;

fn receive(handle: &mut TransferHandle) -> RuntimeOutput {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match handle.try_recv() {
            Ok(output) => return output,
            Err(TryRecvError::Empty) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(2));
            }
            result => panic!("Worker output unavailable: {result:?}"),
        }
    }
}

fn submit(handle: &TransferHandle, mut bytes: &[u8]) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !bytes.is_empty() {
        match handle.try_submit(bytes) {
            Ok(count) => bytes = &bytes[count..],
            Err(SubmitError::Full) if Instant::now() < deadline => thread::yield_now(),
            result => panic!("Worker input unavailable: {result:?}"),
        }
    }
}

#[test]
fn unsafe_names_are_rejected_without_interpreting_remote_paths() {
    for name in [
        "",
        ".",
        "..",
        "/tmp/file",
        "../file",
        "a/b",
        "a\\b",
        "C:file",
        "a\x1bb",
        "a\nb",
    ] {
        assert_eq!(
            safe_name(name.as_bytes()).unwrap_err().kind,
            ErrorKind::UnsafeName
        );
    }
    assert!(safe_name("文件 with spaces.bin".as_bytes()).is_ok());
    assert!(safe_name(&[0xff]).is_err());
}

#[test]
#[serial_test::serial(zmodem_runtime)]
fn choice_holds_handshake_and_input_is_bounded() {
    let mut handle = TransferHandle::spawn(next_transfer_id(), Role::Download).unwrap();
    let bytes = vec![0; MAX_INPUT_CHUNK * 2];
    let deadline = Instant::now() + Duration::from_secs(5);
    for _ in 0..INPUT_SLOTS {
        loop {
            match handle.try_submit(&bytes) {
                Ok(count) => {
                    assert_eq!(count, MAX_INPUT_CHUNK);
                    break;
                }
                Err(SubmitError::Full) if Instant::now() < deadline => thread::yield_now(),
                result => panic!("Failed to fill bounded input queue: {result:?}"),
            }
        }
    }
    assert_eq!(handle.try_submit(b"extra"), Err(SubmitError::Full));
    thread::sleep(Duration::from_millis(40));
    assert!(matches!(handle.try_recv(), Err(TryRecvError::Empty)));
    handle.cancel();
    assert!(matches!(receive(&mut handle), RuntimeOutput::Abort { .. }));
    assert!(matches!(
        receive(&mut handle),
        RuntimeOutput::Event(TransferEvent::Finished {
            outcome: TransferOutcome::Cancelled,
            ..
        })
    ));
}

#[test]
#[serial_test::serial(zmodem_runtime)]
fn dequeue_does_not_release_write_credit_and_cancel_bypasses_it() {
    let directory = tempfile::tempdir().unwrap();
    let mut handle = TransferHandle::spawn(next_transfer_id(), Role::Download).unwrap();
    handle
        .configure_download(directory.path().to_owned(), OverwritePolicy::Skip)
        .unwrap();
    let RuntimeOutput::Wire { sequence, .. } = receive(&mut handle) else {
        panic!("Expected initial handshake");
    };
    // Credit is per sequence: acknowledging a sequence that was never handed out must fail.
    assert!(!handle.acknowledge_write(sequence + 1));
    submit(&handle, &super::super::zrqinit_sequence());
    // Nothing more is queued while the first buffer is still unacknowledged, so wire output can
    // never grow without bound ahead of the PTY.
    assert!(matches!(handle.try_recv(), Err(TryRecvError::Empty)));
    // Cancellation bypasses the wait for credit entirely.
    handle.cancel();
    assert!(matches!(receive(&mut handle), RuntimeOutput::Abort { .. }));
}

#[test]
fn upload_reads_bounded_offsets_and_detects_source_replacement() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("source");
    let data: Vec<_> = (0..40_000).map(|index| (index % 251) as u8).collect();
    fs::write(&path, &data).unwrap();
    let mut source = UploadSource::inspect(path.clone()).unwrap().open().unwrap();
    assert_eq!(
        source.read(1234, usize::MAX).unwrap(),
        data[1234..1234 + MAX_INPUT_CHUNK]
    );
    assert_eq!(source.read(0, 17).unwrap(), data[..17]);
    fs::write(&path, b"changed").unwrap();
    assert_eq!(
        source.read(0, 17).unwrap_err().kind,
        ErrorKind::SourceChanged
    );
}

#[test]
fn rejects_oversized_sparse_uploads() {
    let file = NamedTempFile::new().unwrap();
    file.as_file().set_len(u64::from(u32::MAX) + 1).unwrap();
    assert!(matches!(
        UploadSource::inspect(file.path().to_owned()),
        Err(TransferError {
            kind: ErrorKind::FileTooLarge,
            ..
        })
    ));
}

fn target(directory: &DownloadDirectory, name: &str, data: &[u8]) -> DownloadTarget {
    let mut target = directory.create(name).unwrap();
    target.file.write_all(data).unwrap();
    target.file.as_file().sync_all().unwrap();
    target
}

#[test]
fn temporary_files_are_private_cleaned_and_not_published_early() {
    let root = tempfile::tempdir().unwrap();
    let directory = DownloadDirectory::open(root.path().to_owned(), OverwritePolicy::Skip).unwrap();
    let target = target(&directory, "file", b"partial");
    let temporary = target.file.path().to_owned();
    assert!(!root.path().join("file").exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            target
                .file
                .as_file()
                .metadata()
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    drop(target);
    assert!(!temporary.exists());
}

#[test]
fn skip_handles_publication_race_and_rename_is_no_clobber() {
    let root = tempfile::tempdir().unwrap();
    let mut directory =
        DownloadDirectory::open(root.path().to_owned(), OverwritePolicy::Skip).unwrap();
    let pending = target(&directory, "file.txt", b"new");
    fs::write(root.path().join("file.txt"), b"existing").unwrap();
    assert_eq!(directory.publish(pending).unwrap(), None);
    directory.policy = OverwritePolicy::Rename;
    let one = directory
        .publish(target(&directory, "file.txt", b"one"))
        .unwrap()
        .unwrap();
    let two = directory
        .publish(target(&directory, "file.txt", b"two"))
        .unwrap()
        .unwrap();
    assert_ne!(one, two);
    assert_eq!(fs::read(one).unwrap(), b"one");
    assert_eq!(fs::read(two).unwrap(), b"two");
    assert_eq!(fs::read(root.path().join("file.txt")).unwrap(), b"existing");
}

#[cfg(unix)]
#[test]
fn overwrite_replaces_symlink_without_writing_its_target() {
    let root = tempfile::tempdir().unwrap();
    let original = root.path().join("original");
    fs::write(&original, b"original").unwrap();
    std::os::unix::fs::symlink(&original, root.path().join("file")).unwrap();
    let directory =
        DownloadDirectory::open(root.path().to_owned(), OverwritePolicy::Overwrite).unwrap();
    directory
        .publish(target(&directory, "file", b"replacement"))
        .unwrap();
    assert_eq!(fs::read(original).unwrap(), b"original");
    assert_eq!(fs::read(root.path().join("file")).unwrap(), b"replacement");
    assert!(
        !fs::symlink_metadata(root.path().join("file"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn publication_cancellation_gate_has_a_nonblocking_winner() {
    let phase = AtomicU8::new(0);
    phase.fetch_or(CANCELLED, Ordering::AcqRel);
    assert!(
        phase
            .compare_exchange(0, COMMITTING, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
    );
    phase.store(0, Ordering::Release);
    assert!(
        phase
            .compare_exchange(0, COMMITTING, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    );
    phase.fetch_or(CANCELLED, Ordering::AcqRel);
    phase.fetch_and(!COMMITTING, Ordering::AcqRel);
    assert_eq!(phase.load(Ordering::Acquire), CANCELLED);
}

#[test]
#[serial_test::serial(zmodem_runtime)]
fn native_worker_roundtrip_empty_binary_batch_and_exact_tail() {
    let source = tempfile::tempdir().unwrap();
    let destination = tempfile::tempdir().unwrap();
    let payload: Vec<_> = (0..20_000).map(|index| (index % 256) as u8).collect();
    let paths: Vec<_> = ["empty", "binary with spaces", "中文.txt"]
        .iter()
        .map(|name| source.path().join(name))
        .collect();
    fs::write(&paths[0], b"").unwrap();
    fs::write(&paths[1], &payload).unwrap();
    fs::write(&paths[2], b"last").unwrap();
    let mut upload = TransferHandle::spawn(next_transfer_id(), Role::Upload).unwrap();
    let mut download = TransferHandle::spawn(next_transfer_id(), Role::Download).unwrap();
    upload.configure_upload(paths).unwrap();
    download
        .configure_download(destination.path().to_owned(), OverwritePolicy::Skip)
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut done = [false; 2];
    let mut results = [0; 2];
    let mut tails = Vec::new();
    while !done.iter().all(|done| *done) {
        assert!(Instant::now() < deadline, "Native roundtrip deadline");
        for side in 0..2 {
            let (handle, peer) = if side == 0 {
                (&mut upload, &download)
            } else {
                (&mut download, &upload)
            };
            match handle.try_recv() {
                Ok(RuntimeOutput::Wire {
                    sequence,
                    mut bytes,
                    ..
                }) => {
                    if bytes == b"OO" {
                        bytes.extend_from_slice(b"\r\nprompt> ");
                    }
                    submit(peer, &bytes);
                    assert!(handle.acknowledge_write(sequence));
                }
                Ok(RuntimeOutput::Event(TransferEvent::FileResult { outcome, path, .. })) => {
                    assert_eq!(outcome, FileOutcome::Completed);
                    if side == 1 {
                        assert!(path.unwrap().exists());
                    }
                    results[side] += 1;
                }
                Ok(RuntimeOutput::Event(TransferEvent::Finished {
                    outcome,
                    committed_paths,
                    ..
                })) => {
                    assert_eq!(outcome, TransferOutcome::Completed);
                    assert_eq!(results[side], 3);
                    assert_eq!(committed_paths.len(), if side == 1 { 3 } else { 0 });
                    done[side] = true;
                }
                Ok(RuntimeOutput::Event(_)) => {}
                Ok(RuntimeOutput::Tail { bytes, .. }) => tails.extend(bytes),
                Ok(RuntimeOutput::Abort { .. }) => panic!("Unexpected abort"),
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => assert!(done[side]),
            }
        }
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(tails, b"\r\nprompt> ");
    assert_eq!(
        fs::read(destination.path().join("binary with spaces")).unwrap(),
        payload
    );
    assert_eq!(fs::read(destination.path().join("empty")).unwrap(), b"");
    assert_eq!(
        fs::read(destination.path().join("中文.txt")).unwrap(),
        b"last"
    );
}

#[test]
#[serial_test::serial(zmodem_runtime)]
fn cancelled_but_unexited_workers_keep_their_slots() {
    let deadline = Instant::now() + Duration::from_secs(5);
    while WORKERS.load(Ordering::Acquire) != 0 {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    let mut slots: Vec<_> = (0..MAX_WORKERS)
        .map(|_| WorkerSlot::acquire().unwrap())
        .collect();
    assert!(matches!(
        WorkerSlot::acquire(),
        Err(TransferError {
            kind: ErrorKind::WorkerLimit,
            ..
        })
    ));
    // A slot belongs to the worker stack, not its cancel flag or TransferHandle lifetime.
    let cancel = AtomicU8::new(0);
    cancel.fetch_or(CANCELLED, Ordering::AcqRel);
    assert!(WorkerSlot::acquire().is_err());
    slots.pop();
    let replacement = WorkerSlot::acquire().unwrap();
    assert!(WorkerSlot::acquire().is_err());
    drop(replacement);
    drop(slots);
}

#[test]
#[serial_test::serial(zmodem_runtime)]
fn canonical_remote_cancel_is_detected_across_choice_chunks() {
    let mut handle = TransferHandle::spawn(next_transfer_id(), Role::Download).unwrap();
    submit(&handle, &[0x18; 2]);
    submit(&handle, &[0x18; 3]);
    assert!(matches!(receive(&mut handle), RuntimeOutput::Abort { .. }));
    assert!(matches!(
        receive(&mut handle),
        RuntimeOutput::Event(TransferEvent::Finished {
            outcome: TransferOutcome::RemoteCancelled,
            ..
        })
    ));
}

#[test]
fn timer_deadline_fires_without_input_and_noise_cannot_reset_it() {
    let (wake, receiver) = mpsc::sync_channel(1);
    let shared = Arc::new(Shared {
        ingress: Mutex::new(Ingress::default()),
        phase: AtomicU8::new(0),
        acknowledged: AtomicU64::new(0),
        configured: AtomicBool::new(false),
        wake,
    });
    let (output, _) = mpsc::sync_channel(OUTPUT_SLOTS);
    let (_configuration, configuration_receiver) = mpsc::sync_channel(1);
    let mut worker = Worker::new(
        1,
        Role::Download,
        shared,
        output,
        receiver,
        configuration_receiver,
    );
    worker.deadline = Instant::now() - Duration::from_millis(1);
    assert!(matches!(
        worker.check(),
        Err(Stop::Failed(TransferError {
            kind: ErrorKind::Timeout,
            ..
        }))
    ));
    worker
        .shared
        .ingress
        .lock()
        .chunks
        .push_back(b"noise".to_vec());
    assert!(matches!(
        worker.check(),
        Err(Stop::Failed(TransferError {
            kind: ErrorKind::Timeout,
            ..
        }))
    ));
}

#[test]
fn queue_payload_envelope_including_retained_credit_stays_below_one_mib() {
    let payloads = (INPUT_SLOTS + 1 + OUTPUT_SLOTS + 1) * MAX_INPUT_CHUNK;
    let metadata = 3 * MAX_PATH_BYTES + MAX_FILES * 255;
    let codec = 16 * 1024;
    assert!(payloads + metadata + codec < MAX_QUEUE_BYTES);
}

fn storage_worker() -> (Worker, ChannelReceiver<RuntimeOutput>) {
    let (wake, receiver) = mpsc::sync_channel(1);
    let shared = Arc::new(Shared {
        ingress: Mutex::new(Ingress::default()),
        phase: AtomicU8::new(0),
        acknowledged: AtomicU64::new(0),
        configured: AtomicBool::new(true),
        wake,
    });
    let (output, outputs) = mpsc::sync_channel(OUTPUT_SLOTS);
    let (_configuration, configuration_receiver) = mpsc::sync_channel(1);
    (
        Worker::new(
            1,
            Role::Download,
            shared,
            output,
            receiver,
            configuration_receiver,
        ),
        outputs,
    )
}

#[test]
fn cancellation_before_commit_keeps_existing_destination_and_cleans_partial() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("file"), b"existing").unwrap();
    let directory =
        DownloadDirectory::open(root.path().to_owned(), OverwritePolicy::Overwrite).unwrap();
    let pending = target(&directory, "file", b"replacement");
    let temporary = pending.file.path().to_owned();
    let (mut worker, _outputs) = storage_worker();
    worker.current = Some(CurrentFile {
        name: "file".to_owned(),
        size: Some(11),
        bytes: 11,
        storage: FileStorage::Download(pending),
    });
    worker.shared.phase.fetch_or(CANCELLED, Ordering::AcqRel);
    assert!(matches!(
        worker.finish_file(Some(&directory)),
        Err(Stop::Cancelled)
    ));
    drop(worker);
    assert!(!temporary.exists());
    assert_eq!(fs::read(root.path().join("file")).unwrap(), b"existing");
}

#[test]
fn failed_atomic_publication_emits_failure_never_completion() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("file")).unwrap();
    let directory =
        DownloadDirectory::open(root.path().to_owned(), OverwritePolicy::Overwrite).unwrap();
    let pending = target(&directory, "file", b"data");
    let temporary = pending.file.path().to_owned();
    let (mut worker, outputs) = storage_worker();
    worker.current = Some(CurrentFile {
        name: "file".to_owned(),
        size: Some(4),
        bytes: 4,
        storage: FileStorage::Download(pending),
    });
    assert!(matches!(
        worker.finish_file(Some(&directory)),
        Err(Stop::Failed(_))
    ));
    assert!(worker.committed_paths.is_empty());
    assert!(root.path().join("file").is_dir());
    assert!(!temporary.exists());
    let events: Vec<_> = outputs.try_iter().collect();
    assert!(events.iter().any(|event| matches!(
        event,
        RuntimeOutput::Event(TransferEvent::FileResult {
            outcome: FileOutcome::Failed(_),
            ..
        })
    )));
    assert!(!events.iter().any(|event| matches!(
        event,
        RuntimeOutput::Event(TransferEvent::FileResult {
            outcome: FileOutcome::Completed,
            ..
        })
    )));
}

#[test]
#[serial_test::serial(zmodem_runtime)]
fn canonical_abort_during_active_receive_is_not_a_frame_error() {
    let root = tempfile::tempdir().unwrap();
    let mut handle = TransferHandle::spawn(next_transfer_id(), Role::Download).unwrap();
    handle
        .configure_download(root.path().to_owned(), OverwritePolicy::Skip)
        .unwrap();
    let RuntimeOutput::Wire { sequence, .. } = receive(&mut handle) else {
        panic!("Expected initial handshake");
    };
    handle.acknowledge_write(sequence);
    submit(&handle, b"**");
    for _ in 0..5 {
        submit(&handle, &[0x18]);
        thread::sleep(Duration::from_millis(2));
    }
    assert!(matches!(receive(&mut handle), RuntimeOutput::Abort { .. }));
    assert!(matches!(
        receive(&mut handle),
        RuntimeOutput::Event(TransferEvent::Finished {
            outcome: TransferOutcome::RemoteCancelled,
            ..
        })
    ));
}
