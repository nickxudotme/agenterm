use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::mpsc::TryRecvError;
use std::thread;
use std::time::Duration;

use instant::Instant;
use warp_terminal::zmodem::runtime::{
    FileOutcome, OverwritePolicy, Role, RuntimeOutput, SubmitError, TransferEvent, TransferHandle,
    TransferId, TransferOutcome, abort_sequence, next_transfer_id,
};

use super::peer::{
    EXIT_COMMAND, POLL_PAUSE, Peer, PtyRead, READY, RECOVER_COMMAND, RECOVERED, WriteResult,
    append_terminal, contains, read_some, write_some_or_eof,
};

/// Time to let a finished `sz`/`rz` drain its retry banner before the harness drives the shell.
#[derive(Clone, Copy)]
pub struct Options {
    pub read_chunk: usize,
    pub submit_chunk: usize,
    pub write_chunk: usize,
    pub timeout: Duration,
    pub cancel_after_bytes: Option<u64>,
    /// Observes a finished transfer without driving the peer script's recovery handshake.
    ///
    /// The handshake needs the harness to write shell commands back; a probe that only wants the
    /// completed files and the byte tail must not be forced through it.
    pub stop_when_finished: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            read_chunk: 16 * 1024,
            submit_chunk: 16 * 1024,
            write_chunk: 16 * 1024,
            timeout: Duration::from_secs(45),
            cancel_after_bytes: None,
            stop_when_finished: false,
        }
    }
}

#[derive(Default, Debug)]
pub struct Report {
    pub files: Vec<TransferEvent>,
    pub finished: Option<TransferOutcome>,
    pub committed: Vec<PathBuf>,
    pub terminal: Vec<u8>,
    pub elapsed: Duration,
    pub wire_read: u64,
    pub wire_written: u64,
    pub max_pending_input: usize,
    pub max_pending_output: usize,
    pub intermediate_progress: usize,
    progress: HashMap<String, u64>,
}

impl Report {
    pub fn event(&mut self, event: TransferEvent, id: TransferId, role: Role) {
        match &event {
            TransferEvent::Requested {
                id: actual,
                role: actual_role,
            } => {
                assert_eq!((*actual, *actual_role), (id, role));
            }
            TransferEvent::FileStarted {
                id: actual,
                role: actual_role,
                name,
                ..
            } => {
                assert!(self.finished.is_none(), "file start after terminal event");
                assert_eq!((*actual, *actual_role), (id, role));
                assert!(
                    self.progress.insert(name.clone(), 0).is_none(),
                    "duplicate start"
                );
                self.files.push(event);
            }
            TransferEvent::Progress {
                id: actual,
                role: actual_role,
                name,
                bytes,
                total,
            } => {
                assert!(self.finished.is_none(), "progress after terminal event");
                assert_eq!((*actual, *actual_role), (id, role));
                let previous = self
                    .progress
                    .get_mut(name)
                    .expect("progress precedes file start");
                assert!(*bytes >= *previous, "progress regressed");
                assert!(
                    total.is_none_or(|total| *bytes <= total),
                    "progress exceeds size"
                );
                if *bytes > 0 && total.is_some_and(|total| *bytes < total) {
                    self.intermediate_progress += 1;
                }
                *previous = *bytes;
            }
            TransferEvent::FileResult {
                id: actual,
                role: actual_role,
                name,
                ..
            } => {
                assert!(self.finished.is_none(), "file result after terminal event");
                assert_eq!((*actual, *actual_role), (id, role));
                assert!(
                    self.progress.contains_key(name),
                    "file result precedes start"
                );
                assert!(
                    !self.files.iter().any(|previous| matches!(
                        previous, TransferEvent::FileResult { name: previous_name, .. }
                            if previous_name == name
                    )),
                    "duplicate file result"
                );
                self.files.push(event);
            }
            TransferEvent::Finished {
                id: actual,
                role: actual_role,
                outcome,
                committed_paths,
            } => {
                assert_eq!((*actual, *actual_role), (id, role));
                assert!(
                    self.finished.replace(outcome.clone()).is_none(),
                    "duplicate finish"
                );
                self.committed.clone_from(committed_paths);
            }
        }
        assert!(self.files.len() <= 256, "unbounded lifecycle metadata");
    }

    pub fn assert_completed(&self, files: usize) {
        assert_eq!(self.finished, Some(TransferOutcome::Completed));
        let results: Vec<_> = self
            .files
            .iter()
            .filter_map(|event| match event {
                TransferEvent::FileResult { outcome, .. } => Some(outcome),
                TransferEvent::Requested { .. }
                | TransferEvent::FileStarted { .. }
                | TransferEvent::Progress { .. }
                | TransferEvent::Finished { .. } => None,
            })
            .collect();
        assert_eq!(results.len(), files);
        assert!(
            results
                .iter()
                .all(|outcome| **outcome == FileOutcome::Completed)
        );
    }
}

struct PendingWrite {
    bytes: Vec<u8>,
    offset: usize,
    sequence: Option<u64>,
}

pub fn download(
    paths: &[PathBuf],
    destination: &Path,
    policy: OverwritePolicy,
    options: Options,
) -> Report {
    let mut args = vec![PathBuf::from("--zmodem"), PathBuf::from("--binary")];
    args.extend_from_slice(paths);
    let handle = TransferHandle::spawn(next_transfer_id(), Role::Download).expect("spawn download");
    let mut peer = Peer::spawn(destination, "sz", args);
    handle
        .configure_download(destination.to_owned(), policy)
        .expect("configure download");
    pump(&mut peer, handle, options)
}

pub fn upload(paths: &[PathBuf], destination: &Path, options: Options) -> Report {
    upload_with_args(paths, destination, options, &["--binary"])
}

pub fn upload_protected(paths: &[PathBuf], destination: &Path, options: Options) -> Report {
    upload_with_args(paths, destination, options, &["--binary", "--protect"])
}

fn upload_with_args(
    paths: &[PathBuf],
    destination: &Path,
    options: Options,
    args: &[&str],
) -> Report {
    let handle = TransferHandle::spawn(next_transfer_id(), Role::Upload).expect("spawn upload");
    let mut peer = Peer::spawn(destination, "rz", args);
    handle
        .configure_upload(paths.to_vec())
        .expect("configure upload");
    pump(&mut peer, handle, options)
}

fn assert_peer_exit(status: ExitStatus, report: &Report, diagnostics: &str) {
    // 123 is the peer script's "recovery token mismatch" path: include the bytes the harness
    // actually handed back after the protocol finished, since that is what the script read.
    let terminal = String::from_utf8_lossy(&report.terminal);
    let detail = format!("{diagnostics}; harness tail={terminal:?}");
    match &report.finished {
        Some(TransferOutcome::Completed) => assert!(status.success(), "peer: {status}: {detail}"),
        Some(TransferOutcome::Cancelled) => {
            assert!(
                status.code().is_some(),
                "peer killed instead of cancelling: {status}"
            );
            assert!(
                !status.success(),
                "cancelled peer unexpectedly reported success"
            );
        }
        outcome => panic!("unexpected terminal outcome {outcome:?}: {detail}"),
    }
}

fn pump(peer: &mut Peer, mut handle: TransferHandle, options: Options) -> Report {
    assert!(options.read_chunk > 0 && options.submit_chunk > 0 && options.write_chunk > 0);
    let started = Instant::now();
    let deadline = started + options.timeout;
    let mut report = Report::default();
    let mut input = [0; 16 * 1024];
    let mut input_start = 0;
    let mut input_end = 0;
    let mut output: Option<PendingWrite> = None;
    // Shell commands the harness sends after the protocol ends. They must not depend on the
    // worker still producing output: a finished transfer closes that channel by design.
    let mut shell_output: Option<PendingWrite> = None;
    let mut cancelled_at = None;
    let mut recovery_sent = false;
    let mut exit_sent = false;
    let mut output_closed = false;
    let mut peer_closed_early = false;
    let mut last_sequence = 0;
    let mut recover_ready_at: Option<Instant> = None;
    peer.start(deadline);

    loop {
        assert!(
            Instant::now() < deadline,
            "transfer deadline: finished={:?}, in={}, out={}, terminal={:?}, peer={}",
            report.finished,
            report.wire_read,
            report.wire_written,
            String::from_utf8_lossy(&report.terminal),
            peer.diagnostics()
        );
        let mut progressed = false;

        let transferred_wire = match handle.role() {
            Role::Upload => report.wire_written,
            Role::Download => report.wire_read,
        };
        if let Some(threshold) = options.cancel_after_bytes
            && cancelled_at.is_none()
            && !report.progress.is_empty()
            && transferred_wire >= threshold
        {
            handle.cancel();
            cancelled_at = Some(Instant::now());
            output = None;
        }

        if output.is_none() && !output_closed {
            for _ in 0..64 {
                match handle.try_recv() {
                    Ok(RuntimeOutput::Event(event)) => {
                        report.event(event, handle.id(), handle.role());
                        progressed = true;
                    }
                    Ok(RuntimeOutput::Wire {
                        id,
                        sequence,
                        bytes,
                    }) => {
                        assert_eq!(id, handle.id());
                        assert!(sequence > last_sequence, "wire sequence did not advance");
                        last_sequence = sequence;
                        assert!(!bytes.is_empty());
                        report.max_pending_output = report.max_pending_output.max(bytes.len());
                        assert!(bytes.len() <= 1024 * 1024, "unbounded worker output buffer");
                        output = Some(PendingWrite {
                            bytes,
                            offset: 0,
                            sequence: Some(sequence),
                        });
                        progressed = true;
                        break;
                    }
                    Ok(RuntimeOutput::Tail { id, bytes }) => {
                        assert_eq!(id, handle.id());
                        append_terminal(&mut report.terminal, &bytes);
                        progressed = true;
                    }
                    Ok(RuntimeOutput::Abort { id }) => {
                        assert_eq!(id, handle.id());
                        output = Some(PendingWrite {
                            bytes: abort_sequence(),
                            offset: 0,
                            sequence: None,
                        });
                        progressed = true;
                        break;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        output_closed = true;
                        assert!(report.finished.is_some(), "worker closed without Finished");
                        break;
                    }
                }
            }
        }

        // A closed peer makes further protocol writes meaningless; stop driving the transfer
        // instead of reporting a false completion while the peer is gone.
        if output.is_some() && !output_closed {
            let end = {
                let pending = output.as_ref().expect("checked pending write");
                (pending.offset + options.write_chunk).min(pending.bytes.len())
            };
            let slice = {
                let pending = output.as_ref().expect("checked pending write");
                pending.bytes[pending.offset..end].to_vec()
            };
            match write_some_or_eof(&mut peer.master, &slice) {
                WriteResult::Bytes(count) => {
                    let pending = output.as_mut().expect("checked pending write");
                    pending.offset += count;
                    report.wire_written += count as u64;
                    progressed |= count != 0;
                }
                WriteResult::Pending => {}
                // The peer closed the slave side mid-transfer. Report its real state instead of
                // masking it as a harness I/O fault, then stop writing.
                WriteResult::Eof => {
                    output = None;
                    peer_closed_early = true;
                    eprintln!(
                        "worker harness: peer closed the PTY during a write: {}",
                        peer.failure_context()
                    );
                }
                WriteResult::Failed(error) => {
                    panic!("PTY write failed: {error}: {}", peer.failure_context())
                }
            }
            let completed = output
                .as_ref()
                .is_some_and(|pending| pending.offset == pending.bytes.len());
            if completed {
                let sequence = output.as_ref().and_then(|pending| pending.sequence);
                output = None;
                if let Some(sequence) = sequence {
                    assert!(
                        handle.acknowledge_write(sequence),
                        "wire credit acknowledgement failed"
                    );
                }
            }
        }

        if input_start < input_end {
            if output_closed {
                append_terminal(&mut report.terminal, &input[input_start..input_end]);
                input_start = input_end;
                progressed = true;
            } else {
                let end = (input_start + options.submit_chunk).min(input_end);
                match handle.try_submit(&input[input_start..end]) {
                    Ok(count) => {
                        assert!(count > 0 && count <= end - input_start);
                        input_start += count;
                        progressed = true;
                    }
                    Err(SubmitError::Full | SubmitError::Closed) => {}
                }
            }
        }

        // Read the PTY before deciding anything else: after the worker closes its channel the
        // remaining bytes are ordinary shell output (the peer's status and recovery markers),
        // and treating them as protocol input would swallow the very markers we wait for.
        if input_start == input_end {
            let limit = options.read_chunk.min(input.len());
            match read_some(&mut peer.master, &mut input[..limit]) {
                PtyRead::Bytes(count) => {
                    input_start = 0;
                    input_end = count;
                    if output_closed {
                        append_terminal(&mut report.terminal, &input[..count]);
                        input_start = input_end;
                    } else {
                        report.wire_read += count as u64;
                        report.max_pending_input = report.max_pending_input.max(count);
                    }
                    progressed = true;
                }
                PtyRead::Pending | PtyRead::Eof => {}
            }
        }

        // The worker closes its output channel as soon as the transfer finishes, so waiting for
        // `output.is_none()` here would race the Finished event and never send the token. Drive
        // shell recovery purely from the peer's own status line.
        // A peer that left protocol residue on the terminal can consume the first token as
        // ordinary input, so resend until the peer confirms recovery instead of failing once.
        // The peer script reads its next token from the terminal. `sz`/`rz` leave protocol
        // residue there after finishing, and that residue can be consumed instead of the
        // harness's token, so drain whatever is pending before writing each command.
        let ready = contains(&report.terminal, READY);
        let recovered = contains(&report.terminal, RECOVERED);
        if !peer_closed_early && ready && !recovered && !recovery_sent {
            recover_ready_at = recover_ready_at.or(Some(Instant::now()));
        }
        if !peer_closed_early
            && recover_ready_at.is_some_and(|at| at.elapsed() >= RECOVERY_RESEND)
            && !recovery_sent
        {
            shell_output = Some(PendingWrite {
                bytes: RECOVER_COMMAND.to_vec(),
                offset: 0,
                sequence: None,
            });
            recovery_sent = true;
            recover_ready_at = None;
        }
        if !peer_closed_early && recovered && recovery_sent && !exit_sent {
            shell_output = Some(PendingWrite {
                bytes: EXIT_COMMAND.to_vec(),
                offset: 0,
                sequence: None,
            });
            exit_sent = true;
        }

        if let Some(pending) = shell_output.as_mut() {
            let end = (pending.offset + options.write_chunk).min(pending.bytes.len());
            match write_some_or_eof(&mut peer.master, &pending.bytes[pending.offset..end]) {
                WriteResult::Bytes(count) => {
                    pending.offset += count;
                    progressed = true;
                }
                WriteResult::Pending => {}
                WriteResult::Eof => {
                    peer_closed_early = true;
                    shell_output = None;
                    eprintln!(
                        "worker harness: peer closed the PTY during shell recovery: {}",
                        peer.failure_context()
                    );
                }
                WriteResult::Failed(error) => {
                    panic!("PTY write failed: {error}: {}", peer.failure_context())
                }
            }
            if shell_output
                .as_ref()
                .is_some_and(|pending| pending.offset == pending.bytes.len())
            {
                shell_output = None;
            }
        }

        if let Some(cancelled_at) = cancelled_at {
            assert!(
                report.finished.is_some() || cancelled_at.elapsed() < Duration::from_secs(1),
                "cancellation exceeded one second"
            );
        }

        // The peer script waits for the recovery token before printing its recovery marker, and
        // only exits after the final token. Both writes are harness-driven shell input, so they
        // must complete before the child can be expected to finish normally.
        // A finished transfer that is only being observed still has to leave the peer running:
        // the probe then reports the committed files and the byte tail without a handshake.
        if options.stop_when_finished
            && report.finished.is_some()
            && output_closed
            && output.is_none()
            && input_start == input_end
        {
            report.elapsed = started.elapsed();
            return report;
        }

        if let Some(status) = peer.child.try_wait() {
            // Drive the handshake rather than demanding it already happened: the peer's own
            // status line is what tells us the transfer ended, and the shell prompt is only
            // usable once the harness has written the recovery and exit tokens.
            if !recovery_sent {
                shell_output = Some(PendingWrite {
                    bytes: RECOVER_COMMAND.to_vec(),
                    offset: 0,
                    sequence: None,
                });
                recovery_sent = true;
            } else if !exit_sent {
                shell_output = Some(PendingWrite {
                    bytes: EXIT_COMMAND.to_vec(),
                    offset: 0,
                    sequence: None,
                });
                exit_sent = true;
            }
            if !recovery_sent || !exit_sent {
                continue;
            }
            assert!(
                recovery_sent && exit_sent,
                "peer exited before shell recovery: {status}: {}",
                peer.failure_context()
            );
            assert!(
                !peer_closed_early,
                "peer exited before the transfer finished: {}",
                peer.failure_context()
            );
            assert_peer_exit(status, &report, &peer.diagnostics());
            if output_closed && input_start == input_end && contains(&report.terminal, RECOVERED) {
                assert!(output.is_none());
                if options.cancel_after_bytes.is_some() {
                    assert!(cancelled_at.is_some(), "cancel threshold was never reached");
                    assert_eq!(report.finished, Some(TransferOutcome::Cancelled));
                }
                report.elapsed = started.elapsed();
                return report;
            }
            panic!(
                "peer exited without recovery marker; tail={:?}; {}",
                String::from_utf8_lossy(&report.terminal),
                peer.failure_context()
            );
        }
        if !progressed {
            thread::sleep(POLL_PAUSE);
        }
    }
}
/// Interval for re-driving the peer's shell recovery when protocol residue consumes a token.
const RECOVERY_RESEND: Duration = Duration::from_millis(200);
