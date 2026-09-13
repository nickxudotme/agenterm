//! Opt-in native-worker and production-EventLoop interoperability tests.
//!
//! Enable `integration_tests` and select this test binary. Missing sz, rz or shasum is a failure.
//! The two ignored large tests default to 256 MiB; ZMODEM_TEST_MIB selects a different size for
//! memory-scaling comparisons. Run each large test in a fresh process for useful peak-RSS data.
//! Worker tests do not exercise GUI pickers or production routing; `production_*` tests exercise
//! the real EventLoop with a capture Handler and synthetic picker replies, not the GUI itself.

#![cfg(all(unix, feature = "integration_tests"))]

mod zmodem_support;

use std::path::{Path, PathBuf};
use std::time::Duration;
use std::{fs, thread};

use instant::Instant;
use serial_test::serial;
use warp_terminal::event::Event;
use warp_terminal::writeable_pty::Message;
use warp_terminal::zmodem::runtime::{
    Control, ErrorKind, FileOutcome, OverwritePolicy, Role, TransferEvent, TransferOutcome,
};
use zmodem_support::data::{assert_equal, entries, generate, peak_rss_bytes, small_batch};
use zmodem_support::peer::{POLL_PAUSE, raw_pty, require_tools, set_nonblocking, write_until};
use zmodem_support::routing::{self, Router};
use zmodem_support::worker::{self, Options};

fn directories(root: &Path) -> (PathBuf, PathBuf) {
    let source = root.join("source");
    let destination = root.join("destination");
    fs::create_dir(&source).expect("create source directory");
    fs::create_dir(&destination).expect("create destination directory");
    (source, destination)
}

fn assert_batch(paths: &[PathBuf], destination: &Path) {
    assert_eq!(
        entries(destination).len(),
        paths.len(),
        "unexpected files or temporary residue"
    );
    for source in paths {
        assert_equal(
            source,
            &destination.join(source.file_name().expect("fixture file name")),
        );
    }
}

#[test]
#[serial]
fn worker_download_binary_empty_multifile_unicode_and_spaces() {
    require_tools();
    let root = tempfile::tempdir().expect("test directory");
    let (source, destination) = directories(root.path());
    let paths = small_batch(&source);
    let report = worker::download(
        &paths,
        &destination,
        OverwritePolicy::Skip,
        Options::default(),
    );
    report.assert_completed(paths.len());
    assert_eq!(report.committed.len(), paths.len());
    assert_batch(&paths, &destination);
}

#[test]
#[serial]
fn worker_upload_binary_empty_multifile_unicode_and_spaces() {
    require_tools();
    let root = tempfile::tempdir().expect("test directory");
    let (source, destination) = directories(root.path());
    let paths = small_batch(&source);
    let report = worker::upload(&paths, &destination, Options::default());
    report.assert_completed(paths.len());
    assert_batch(&paths, &destination);
}

#[test]
#[serial]
fn worker_both_directions_survive_one_byte_reads_submits_and_writes() {
    require_tools();
    for role in [Role::Download, Role::Upload] {
        let root = tempfile::tempdir().expect("test directory");
        let (source, destination) = directories(root.path());
        let path = source.join("fragmented all bytes.bin");
        generate(&path, 513);
        let paths = vec![path];
        let options = Options {
            read_chunk: 1,
            submit_chunk: 1,
            write_chunk: 1,
            ..Options::default()
        };
        let report = match role {
            Role::Download => {
                worker::download(&paths, &destination, OverwritePolicy::Skip, options)
            }
            Role::Upload => worker::upload(&paths, &destination, options),
        };
        report.assert_completed(1);
        assert_eq!(report.max_pending_input, 1);
        assert_batch(&paths, &destination);
    }
}

#[test]
#[serial]
fn worker_download_skip_rename_overwrite_preserves_policy_and_results() {
    require_tools();
    for policy in [
        OverwritePolicy::Skip,
        OverwritePolicy::Rename,
        OverwritePolicy::Overwrite,
    ] {
        let root = tempfile::tempdir().expect("test directory");
        let (source, destination) = directories(root.path());
        let conflict = source.join("conflict.bin");
        let fresh = source.join("fresh.bin");
        generate(&conflict, 8193);
        generate(&fresh, 1025);
        let existing = destination.join("conflict.bin");
        fs::write(&existing, b"keep original bytes").expect("create conflicting destination");
        let paths = vec![conflict.clone(), fresh.clone()];
        let report = worker::download(&paths, &destination, policy, Options::default());
        assert_eq!(report.finished, Some(TransferOutcome::Completed));
        let results: Vec<_> = report
            .files
            .iter()
            .filter_map(|event| match event {
                TransferEvent::FileResult {
                    name,
                    outcome,
                    path,
                    ..
                } => Some((name, outcome, path)),
                TransferEvent::Requested { .. }
                | TransferEvent::FileStarted { .. }
                | TransferEvent::Progress { .. }
                | TransferEvent::Finished { .. } => None,
            })
            .collect();
        assert_eq!(results.len(), 2);
        let (_, result, received_path) = results
            .iter()
            .find(|(name, ..)| name.as_str() == "conflict.bin")
            .expect("conflicting file result");
        match policy {
            OverwritePolicy::Skip => {
                assert_eq!(**result, FileOutcome::Skipped);
                assert_eq!(
                    fs::read(&existing).expect("existing bytes"),
                    b"keep original bytes"
                );
                assert_eq!(entries(&destination).len(), 2);
                assert_eq!(report.committed.len(), 1);
            }
            OverwritePolicy::Rename => {
                assert_eq!(**result, FileOutcome::Completed);
                let renamed = received_path.as_ref().expect("renamed path");
                let canonical_destination =
                    destination.canonicalize().expect("canonical destination");
                assert_ne!(renamed, &existing);
                assert_eq!(renamed.parent(), Some(canonical_destination.as_path()));
                assert_equal(&conflict, renamed);
                assert_eq!(
                    fs::read(&existing).expect("existing bytes"),
                    b"keep original bytes"
                );
                assert_eq!(entries(&destination).len(), 3);
                assert_eq!(report.committed.len(), 2);
            }
            OverwritePolicy::Overwrite => {
                assert_eq!(**result, FileOutcome::Completed);
                assert_equal(&conflict, &existing);
                assert_eq!(entries(&destination).len(), 2);
                assert_eq!(report.committed.len(), 2);
            }
        }
        assert_equal(&fresh, &destination.join("fresh.bin"));
    }
}

#[test]
#[serial]
fn worker_upload_peer_skip_is_not_reported_as_file_success() {
    require_tools();
    let root = tempfile::tempdir().expect("test directory");
    let (source, destination) = directories(root.path());
    let conflict = source.join("conflict.bin");
    let fresh = source.join("fresh.bin");
    generate(&conflict, 8193);
    generate(&fresh, 1025);
    let existing = destination.join("conflict.bin");
    fs::write(&existing, b"protected original").expect("create protected peer file");
    let report =
        worker::upload_protected(&[conflict, fresh.clone()], &destination, Options::default());
    assert_eq!(report.finished, Some(TransferOutcome::Completed));
    let mut outcomes = Vec::new();
    for event in report.files {
        if let TransferEvent::FileResult { name, outcome, .. } = event {
            outcomes.push((name, outcome));
        }
    }
    assert_eq!(
        outcomes,
        vec![
            ("conflict.bin".to_owned(), FileOutcome::Skipped),
            ("fresh.bin".to_owned(), FileOutcome::Completed),
        ]
    );
    assert_eq!(
        fs::read(&existing).expect("read protected peer file"),
        b"protected original"
    );
    assert_equal(&fresh, &destination.join("fresh.bin"));
    assert_eq!(entries(&destination).len(), 2);
}

#[test]
#[serial]
fn worker_cancel_download_mid_file_has_no_published_or_temporary_file() {
    require_tools();
    let root = tempfile::tempdir().expect("test directory");
    let (source, destination) = directories(root.path());
    let path = source.join("cancel.bin");
    generate(&path, 8 * 1024 * 1024);
    let report = worker::download(
        &[path],
        &destination,
        OverwritePolicy::Skip,
        Options {
            cancel_after_bytes: Some(4096),
            ..Options::default()
        },
    );
    assert_eq!(report.finished, Some(TransferOutcome::Cancelled));
    assert!(report.committed.is_empty());
    assert!(
        entries(&destination).is_empty(),
        "failed download left files behind"
    );
}

#[test]
#[serial]
fn worker_cancel_upload_mid_file_notifies_real_receiver_and_recovers_shell() {
    require_tools();
    let root = tempfile::tempdir().expect("test directory");
    let (source, destination) = directories(root.path());
    let path = source.join("cancel-upload.bin");
    generate(&path, 8 * 1024 * 1024);
    let report = worker::upload(
        &[path],
        &destination,
        Options {
            cancel_after_bytes: Some(4096),
            ..Options::default()
        },
    );
    assert_eq!(report.finished, Some(TransferOutcome::Cancelled));
    assert!(!report.files.iter().any(|event| matches!(
        event,
        TransferEvent::FileResult {
            outcome: FileOutcome::Completed,
            ..
        }
    )));
}

#[test]
#[serial]
fn production_event_loop_both_directions_binary_empty_multifile() {
    require_tools();
    for role in [Role::Download, Role::Upload] {
        let root = tempfile::tempdir().expect("test directory");
        let (source, destination) = directories(root.path());
        let paths = small_batch(&source);
        let report = routing::transfer(&paths, &destination, role, 16 * 1024);
        report.assert_completed(paths.len());
        assert_batch(&paths, &destination);
    }
}

#[test]
#[serial]
fn production_event_loop_fragmented_preamble_and_binary_payload() {
    require_tools();
    for role in [Role::Download, Role::Upload] {
        let root = tempfile::tempdir().expect("test directory");
        let (source, destination) = directories(root.path());
        let path = source.join("one-byte.bin");
        generate(&path, 513);
        let paths = vec![path];
        let report = routing::transfer(&paths, &destination, role, 1);
        report.assert_completed(1);
        assert_batch(&paths, &destination);
    }
}

#[test]
#[serial]
fn production_event_loop_trailing_stars_expire_without_another_read() {
    for tail in [b"*".as_slice(), b"**"] {
        let (master, mut slave) = raw_pty();
        set_nonblocking(&slave);
        let mut router = Router::start(master, 1, true);
        let expected = [b"ordinary output".as_slice(), tail].concat();
        let deadline = Instant::now() + Duration::from_secs(2);
        write_until(&mut slave, &expected, deadline);
        loop {
            assert!(
                Instant::now() < deadline,
                "silent PTY star tail never rendered"
            );
            assert!(
                router.next_event().is_none(),
                "ordinary text requested a transfer"
            );
            if router.rendered().as_deref() == Some(expected.as_slice()) {
                break;
            }
            thread::sleep(POLL_PAUSE);
        }
        router.stop(false, deadline);
    }
}

#[test]
#[serial]
fn production_event_loop_disabled_does_not_claim_valid_header() {
    let (master, mut slave) = raw_pty();
    set_nonblocking(&slave);
    let mut router = Router::start(master, 3, false);
    let expected = b"prefix**\x18B00000000000000\r\x8a\x11suffix**";
    let deadline = Instant::now() + Duration::from_secs(2);
    write_until(&mut slave, expected, deadline);
    loop {
        assert!(
            Instant::now() < deadline,
            "disabled detector swallowed output"
        );
        assert!(
            router.next_event().is_none(),
            "disabled detector requested a transfer"
        );
        if router.rendered().as_deref() == Some(expected.as_slice()) {
            break;
        }
        thread::sleep(POLL_PAUSE);
    }
    router.stop(false, deadline);
}

#[test]
#[serial]
fn production_event_loop_silent_peer_times_out_without_new_input() {
    let root = tempfile::tempdir().expect("test directory");
    let (master, mut slave) = raw_pty();
    set_nonblocking(&slave);
    let mut router = Router::start(master, 4096, true);
    let deadline = Instant::now() + Duration::from_secs(35);
    write_until(&mut slave, b"**\x18B00000000000000\r\x8a\x11", deadline);
    let mut id = None;
    loop {
        assert!(
            Instant::now() < deadline,
            "silent peer did not time out independently"
        );
        if let Some(Event::Zmodem(event)) = router.next_event() {
            match event {
                TransferEvent::Requested {
                    id: requested_id,
                    role,
                } => {
                    assert_eq!(role, Role::Download);
                    assert!(id.replace(requested_id).is_none());
                    router.send(Message::Zmodem(Control::Download {
                        id: requested_id,
                        directory: root.path().to_owned(),
                        policy: OverwritePolicy::Skip,
                    }));
                }
                TransferEvent::Finished {
                    id: finished_id,
                    outcome,
                    ..
                } => {
                    assert_eq!(Some(finished_id), id);
                    let TransferOutcome::Failed(error) = outcome else {
                        panic!("silent peer must fail, got {outcome:?}");
                    };
                    assert_eq!(error.kind, ErrorKind::Timeout);
                    break;
                }
                TransferEvent::FileStarted { .. }
                | TransferEvent::Progress { .. }
                | TransferEvent::FileResult { .. } => panic!("silent peer produced file events"),
            }
        }
        thread::sleep(POLL_PAUSE);
    }
    assert!(entries(root.path()).is_empty());
    write_until(&mut slave, b"ordinary after timeout**", deadline);
    loop {
        assert!(
            Instant::now() < deadline,
            "ordinary output did not recover after timeout"
        );
        if router
            .rendered()
            .is_some_and(|bytes| bytes.ends_with(b"ordinary after timeout**"))
        {
            break;
        }
        if let Some(Event::Zmodem(event)) = router.next_event() {
            panic!("unexpected transfer event after timeout: {event:?}");
        }
        thread::sleep(POLL_PAUSE);
    }
    router.stop(false, deadline);
}

fn large_transfer(role: Role) {
    require_tools();
    let mib = std::env::var("ZMODEM_TEST_MIB")
        .map(|value| {
            value
                .parse::<u64>()
                .expect("ZMODEM_TEST_MIB must be an integer")
        })
        .unwrap_or(256);
    assert!(
        (1..=4095).contains(&mib),
        "fixture must fit the ZMODEM u32 size limit"
    );
    let root = tempfile::tempdir().expect("test directory");
    let (source, destination) = directories(root.path());
    let path = source.join("large.bin");
    let size = mib * 1024 * 1024;
    generate(&path, size);
    let baseline_rss = peak_rss_bytes();
    let options = Options {
        timeout: Duration::from_secs(600),
        ..Options::default()
    };
    let paths = vec![path];
    let report = match role {
        Role::Download => worker::download(&paths, &destination, OverwritePolicy::Skip, options),
        Role::Upload => worker::upload(&paths, &destination, options),
    };
    report.assert_completed(1);
    let transfer_peak_rss = peak_rss_bytes();
    assert_batch(&paths, &destination);
    let seconds = report.elapsed.as_secs_f64();
    eprintln!(
        "{role:?}: {mib} MiB in {seconds:.3}s, {:.2} MiB/s, baseline_peak_rss={baseline_rss}, \
         transfer_peak_rss={transfer_peak_rss}, harness_pending_input={}, harness_pending_output={}",
        mib as f64 / seconds,
        report.max_pending_input,
        report.max_pending_output
    );
}

#[test]
#[ignore = "large real-peer benchmark; default 256 MiB, ZMODEM_TEST_MIB overrides sizing"]
#[serial]
fn worker_large_streaming_download() {
    large_transfer(Role::Download);
}

#[test]
#[ignore = "large real-peer benchmark; default 256 MiB, ZMODEM_TEST_MIB overrides sizing"]
#[serial]
fn worker_large_streaming_upload() {
    large_transfer(Role::Upload);
}

/// Measures throughput through the production event loop, the path a real session uses.
///
/// Ignored by default because it writes a large fixture; `ZMODEM_TEST_MIB` sizes it.
#[test]
#[ignore = "large production benchmark; ZMODEM_TEST_MIB overrides sizing"]
#[serial]
fn production_event_loop_large_streaming_both_directions() {
    require_tools();
    let mib = std::env::var("ZMODEM_TEST_MIB")
        .map(|value| {
            value
                .parse::<u64>()
                .expect("ZMODEM_TEST_MIB must be an integer")
        })
        .unwrap_or(64);
    assert!(
        (1..=4095).contains(&mib),
        "fixture must fit the ZMODEM u32 size limit"
    );
    for role in [Role::Download, Role::Upload] {
        let root = tempfile::tempdir().expect("test directory");
        let (source, destination) = directories(root.path());
        let path = source.join("large.bin");
        generate(&path, mib * 1024 * 1024);
        let paths = vec![path];
        let started = Instant::now();
        let report = routing::transfer(&paths, &destination, role, 16 * 1024);
        let seconds = started.elapsed().as_secs_f64();
        report.assert_completed(paths.len());
        assert_batch(&paths, &destination);
        eprintln!(
            "production {role:?}: {mib} MiB in {seconds:.3}s, {:.2} MiB/s",
            mib as f64 / seconds
        );
    }
}

#[test]
#[ignore = "requires the developer's ssh mini host"]
#[serial]
fn production_event_loop_uploads_real_dmg_over_ssh() {
    require_tools();
    let path = std::env::var_os("ZMODEM_TEST_FILE")
        .map(PathBuf::from)
        .expect("ZMODEM_TEST_FILE must name the upload fixture");
    let host = std::env::var("ZMODEM_TEST_HOST").unwrap_or_else(|_| "mini".to_owned());
    let remote_directory = format!("/tmp/agenterm-zmodem-test-{}", std::process::id());
    let rz = std::env::var("ZMODEM_TEST_RZ").unwrap_or_else(|_| "/opt/homebrew/bin/rz".to_owned());
    let direct_command = format!(
        "mkdir -p {remote_directory} && cd {remote_directory} && exec {rz} --binary --overwrite"
    );
    let remote_command = match std::env::var("ZMODEM_TEST_NESTED_HOST") {
        Ok(nested_host) => {
            format!("TERM=xterm-256color ssh -tt {nested_host} '{direct_command}'")
        }
        Err(_) => direct_command,
    };
    let report = routing::upload_over_ssh(std::slice::from_ref(&path), &host, &remote_command);
    report.assert_completed(1);
    assert!(
        report.intermediate_progress > 0,
        "upload completed without observable intermediate progress"
    );
    eprintln!("remote fixture retained at {remote_directory}");
}
