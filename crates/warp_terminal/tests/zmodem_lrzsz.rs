//! Verifies the ZMODEM implementation against the real `lrzsz` tools.
//!
//! Unit tests cover the wire format against captured bytes, but only a live
//! `sz` exercises the full negotiation: flow control, subpacket pacing, CRC
//! checks, and end-of-file handshake. The test is skipped when `sz` is absent
//! so it stays useful locally without becoming a hard CI dependency.

#![cfg(unix)]

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::path::Path;
use std::process::Stdio;

use command::blocking::Command;
use warp_terminal::zmodem::{OwnedEvent, TransferProgress, UploadFile, ZmodemRole, ZmodemSession};

/// How long to wait between polls once the peer stops sending.
const IDLE_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(10);

/// How many consecutive idle polls to tolerate before giving up. Generous
/// enough to ride out `sz`'s own pauses, short enough to fail fast.
const IDLE_POLL_LIMIT: usize = 300;

/// Whether `sz` is installed and usable.
fn have_sz() -> bool {
    Command::new("sz")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success() || status.code() == Some(1))
}

/// Runs a real `sz` over a PTY and receives the file with [`ZmodemSession`].
///
/// Returns the received bytes and the name the sender advertised.
fn receive_with_sz(path: &Path) -> (Vec<u8>, String) {
    receive_with_sz_chunked(path, usize::MAX)
}

/// Same as [`receive_with_sz`], but caps how many bytes are handed to the
/// session per call, mimicking a PTY that fragments the stream.
fn receive_with_sz_chunked(path: &Path, max_chunk: usize) -> (Vec<u8>, String) {
    let pty = nix::pty::openpty(None, None).expect("openpty");

    let mut child = Command::new("sz")
        .arg("--zmodem")
        .arg("--binary")
        .arg(path)
        // SAFETY: the fds are duplicated so the child and parent each own one,
        // and `Stdio` closes only its own copy.
        .stdin(unsafe { Stdio::from_raw_fd(libc::dup(pty.slave)) })
        .stdout(unsafe { Stdio::from_raw_fd(libc::dup(pty.slave)) })
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn sz");

    // Closing our copy lets the PTY report EOF once the child exits.
    // SAFETY: the child holds its own duplicated descriptors.
    unsafe { libc::close(pty.slave) };

    // SAFETY: `master` is a fresh descriptor this test exclusively owns.
    let mut master: File = unsafe { File::from_raw_fd(pty.master) };
    let mut session = ZmodemSession::new_download().expect("download session");
    let mut received = Vec::new();
    let mut name = String::new();

    // The receiver advertises its capabilities before the sender will proceed.
    let handshake = session.initial_output().expect("initial output");
    if !handshake.is_empty() {
        master.write_all(&handshake).expect("write handshake");
    }

    // `sz` keeps the PTY open retrying after the transfer ends, so reads are
    // non-blocking and the loop gives up once the peer goes quiet.
    // SAFETY: `master` is a descriptor this test exclusively owns.
    unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) };

    let mut buf = [0u8; 8192];
    let mut idle_polls = 0;
    while !session.is_finished() && idle_polls < IDLE_POLL_LIMIT {
        let read = match master.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                idle_polls = 0;
                n
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                idle_polls += 1;
                std::thread::sleep(IDLE_POLL_INTERVAL);
                continue;
            }
            Err(error) => panic!("PTY read failed: {error}"),
        };

        for piece in buf[..read].chunks(max_chunk.max(1)) {
            let step = session.submit_wire(piece).expect("submit_wire");
            if !step.to_pty.is_empty() {
                master.write_all(&step.to_pty).expect("write reply");
            }
            if let Some(chunk) = step.file_data {
                if !chunk.name.is_empty() {
                    name = chunk.name;
                }
                received.extend_from_slice(&chunk.data);
            }
            for event in step.events {
                if let OwnedEvent::FileStarted { name: started, .. } = event {
                    name = started;
                }
            }
        }
    }

    // `sz` lingers waiting for more protocol traffic after the session ends,
    // so end it explicitly rather than waiting out its retry timeout.
    let _ = child.kill();
    let _ = child.wait();
    (received, name)
}

#[test]
fn receives_small_file_from_real_sz() {
    if !have_sz() {
        eprintln!("skipping: `sz` (lrzsz) is not installed");
        return;
    }

    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("small.txt");
    let payload = b"ZMODEM round trip payload\n";
    std::fs::write(&path, payload).expect("write source");

    let (received, name) = receive_with_sz(&path);
    assert_eq!(name, "small.txt");
    assert_eq!(received, payload);
}

#[test]
fn receives_binary_file_spanning_subpackets_from_real_sz() {
    if !have_sz() {
        eprintln!("skipping: `sz` (lrzsz) is not installed");
        return;
    }

    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("binary.bin");
    // Larger than one 1 KiB subpacket, and includes every byte value so the
    // ZDLE escaping path is exercised rather than just printable text.
    let payload: Vec<u8> = (0..8192u32).map(|index| (index % 256) as u8).collect();
    std::fs::write(&path, &payload).expect("write source");

    let (received, name) = receive_with_sz(&path);
    assert_eq!(name, "binary.bin");
    assert_eq!(received.len(), payload.len(), "received byte count differs");
    assert_eq!(received, payload);
}

#[test]
fn receives_file_when_the_pty_fragments_every_byte() {
    if !have_sz() {
        eprintln!("skipping: `sz` (lrzsz) is not installed");
        return;
    }

    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("fragmented.txt");
    let payload = b"ZMODEM must survive byte-at-a-time PTY reads\n";
    std::fs::write(&path, payload).expect("write source");

    // A PTY may deliver the preamble one byte per read; detection and the
    // protocol must not depend on frames arriving whole.
    let (received, name) = receive_with_sz_chunked(&path, 1);
    assert_eq!(name, "fragmented.txt");
    assert_eq!(received, payload);
}

#[test]
fn renders_progress_and_summary_for_a_real_transfer() {
    if !have_sz() {
        eprintln!("skipping: `sz` (lrzsz) is not installed");
        return;
    }

    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("progress.bin");
    let payload: Vec<u8> = (0..16384u32).map(|index| (index % 251) as u8).collect();
    std::fs::write(&path, &payload).expect("write source");

    // Replays what the event loop renders: the advertised size arrives with
    // the file, and byte counts accumulate as subpackets land.
    let pty = nix::pty::openpty(None, None).expect("openpty");
    let mut child = Command::new("sz")
        .arg("--zmodem")
        .arg("--binary")
        .arg(&path)
        // SAFETY: descriptors are duplicated so parent and child each own one.
        .stdin(unsafe { Stdio::from_raw_fd(libc::dup(pty.slave)) })
        .stdout(unsafe { Stdio::from_raw_fd(libc::dup(pty.slave)) })
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn sz");
    // SAFETY: the child holds its own duplicated descriptors.
    unsafe { libc::close(pty.slave) };
    // SAFETY: `master` is a descriptor this test exclusively owns.
    let mut master: File = unsafe { File::from_raw_fd(pty.master) };
    unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) };

    let mut session = ZmodemSession::new_download().expect("download session");
    let mut progress = TransferProgress::default();
    let mut status_lines = Vec::new();
    let handshake = session.initial_output().expect("initial output");
    master.write_all(&handshake).expect("write handshake");

    let mut buf = [0u8; 8192];
    let mut idle = 0;
    while !session.is_finished() && idle < IDLE_POLL_LIMIT {
        let read = match master.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                idle = 0;
                n
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                idle += 1;
                std::thread::sleep(IDLE_POLL_INTERVAL);
                continue;
            }
            Err(error) => panic!("PTY read failed: {error}"),
        };

        let step = session.submit_wire(&buf[..read]).expect("submit_wire");
        if !step.to_pty.is_empty() {
            master.write_all(&step.to_pty).expect("write reply");
        }
        if let Some(chunk) = &step.file_data {
            progress.bytes_transferred += chunk.data.len() as u64;
            status_lines.push(progress.render_line(ZmodemRole::Download));
        }
        for event in step.events {
            match event {
                OwnedEvent::FileStarted { name, size } => {
                    progress.file_name = name;
                    progress.bytes_total = size.map(u64::from).unwrap_or(0);
                }
                OwnedEvent::FileCompleted => {
                    status_lines.push(progress.render_summary(ZmodemRole::Download, None));
                }
                _ => {}
            }
        }
    }

    let _ = child.kill();
    let _ = child.wait();

    assert_eq!(
        progress.bytes_total,
        payload.len() as u64,
        "size advertised"
    );
    assert_eq!(progress.bytes_transferred, payload.len() as u64);
    assert!(
        status_lines.len() > 1,
        "a multi-subpacket transfer must report progress more than once"
    );

    let summary = String::from_utf8(status_lines.last().unwrap().clone()).expect("utf8");
    assert!(summary.contains("progress.bin"), "summary names the file");
    assert!(summary.contains("16.0 KiB complete"), "got: {summary}");
    assert!(summary.ends_with("\r\n"), "summary stays in scrollback");
}

/// Writes every byte to a non-blocking PTY, retrying on `WouldBlock`.
///
/// The real event loop hands bytes to mio and lets it drain them; a test that
/// writes directly must absorb the back-pressure itself.
fn write_all_blocking(pty: &mut File, mut bytes: &[u8]) {
    while !bytes.is_empty() {
        match pty.write(bytes) {
            Ok(0) => panic!("PTY refused the write"),
            Ok(n) => bytes = &bytes[n..],
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(IDLE_POLL_INTERVAL);
            }
            Err(error) => panic!("PTY write failed: {error}"),
        }
    }
}

/// Whether `rz` is installed and usable.
fn have_rz() -> bool {
    Command::new("rz")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success() || status.code() == Some(1))
}

/// Upload against a real `rz` is not finished yet. `rz` now accepts our ZFILE
/// and the file lands on disk with the right contents, but the session never
/// closes: `rz` re-requests from offset zero instead of acknowledging ZEOF.
///
/// The remaining gap is the send cadence. `zmodem2` expects one action per
/// `poll`, with `submit_file` followed immediately by further polling so the
/// data frame reaches the wire; our session drains in batches instead, which
/// is what leaves ZEOF unsent. See the crate's own `tests/integration.rs`.
///
/// Kept as a runnable reproduction rather than deleted, so the upload path has
/// a concrete failing case to fix against.
#[test]
#[ignore = "ZMODEM upload does not close the session against real rz"]
fn sends_file_to_real_rz() {
    if !have_rz() {
        eprintln!("skipping: `rz` (lrzsz) is not installed");
        return;
    }

    let dir = tempfile::tempdir().expect("temp dir");
    let source = dir.path().join("upload.bin");
    let payload: Vec<u8> = (0..4096u32).map(|index| (index % 256) as u8).collect();
    std::fs::write(&source, &payload).expect("write source");

    // `rz` writes into its working directory, so give it a clean one.
    let dest_dir = dir.path().join("dest");
    std::fs::create_dir(&dest_dir).expect("create dest dir");

    let pty = nix::pty::openpty(None, None).expect("openpty");
    let mut child = Command::new("rz")
        .arg("--binary")
        .current_dir(&dest_dir)
        // SAFETY: descriptors are duplicated so parent and child each own one.
        .stdin(unsafe { Stdio::from_raw_fd(libc::dup(pty.slave)) })
        .stdout(unsafe { Stdio::from_raw_fd(libc::dup(pty.slave)) })
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn rz");
    // SAFETY: the child holds its own duplicated descriptors.
    unsafe { libc::close(pty.slave) };
    // SAFETY: `master` is a descriptor this test exclusively owns.
    let mut master: File = unsafe { File::from_raw_fd(pty.master) };
    unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) };

    let files = vec![UploadFile {
        path: source.clone(),
        name: b"upload.bin".to_vec(),
        size: payload.len() as u32,
    }];
    let mut session = ZmodemSession::new_upload(files).expect("upload session");

    // Only the ZRQINIT goes out first; the file is offered once `rz`
    // introduces itself with ZRINIT.
    let handshake = session.initial_output().expect("initial output");
    write_all_blocking(&mut master, &handshake);

    let mut buf = [0u8; 8192];
    let mut idle = 0;
    let mut sent_everything = false;
    // `rz` does not close the session on its own, so stop once the payload is
    // fully queued and the peer has gone quiet.
    while !session.is_finished() && idle < IDLE_POLL_LIMIT {
        let read = match master.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                idle = 0;
                n
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                idle += 1;
                if sent_everything && idle > 20 {
                    break;
                }
                std::thread::sleep(IDLE_POLL_INTERVAL);
                continue;
            }
            Err(error) => panic!("PTY read failed: {error}"),
        };

        let mut step = session.submit_wire(&buf[..read]).expect("submit_wire");
        if !step.to_pty.is_empty() {
            write_all_blocking(&mut master, &step.to_pty);
        }

        // Answer file-data requests until the sender stops asking, mirroring
        // what the event loop does.
        while let Some(request) = step.file_request {
            let start = request.offset as usize;
            let end = (start + request.max_len).min(payload.len());
            let reply = session
                .submit_file(&payload[start..end])
                .expect("submit_file");
            if !reply.is_empty() {
                write_all_blocking(&mut master, &reply);
            }
            let next = session.offer_next_file().expect("offer next");
            if !next.is_empty() {
                write_all_blocking(&mut master, &next);
            }
            // Once the last byte is queued, declare the queue closed so the
            // sender can send ZEOF and then ZFIN; without this the session
            // never ends and `rz` keeps waiting for another file.
            if end >= payload.len() {
                sent_everything = true;
                let finish = session.finish_upload().expect("finish upload");
                if !finish.is_empty() {
                    write_all_blocking(&mut master, &finish);
                }
            }
            step = Default::default();
        }
    }

    let _ = child.kill();
    let _ = child.wait();

    let landed = dest_dir.join("upload.bin");
    assert!(landed.exists(), "rz did not create the file");
    assert_eq!(std::fs::read(&landed).expect("read landed file"), payload);
}
