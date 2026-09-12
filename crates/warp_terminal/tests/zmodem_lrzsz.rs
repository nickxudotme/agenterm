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
use warp_terminal::zmodem::{OwnedEvent, ZmodemSession};

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

        let step = session.submit_wire(&buf[..read]).expect("submit_wire");
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
