// The code in this file is adapted from the alacritty_terminal crate under the
// Apache license; see: crates/warp_terminal/src/model/LICENSE-ALACRITTY.

//! The main event loop which performs I/O on the pseudoterminal.

use std::borrow::Cow;
use std::collections::VecDeque;
use std::io::{self, ErrorKind, Read, Write};
use std::sync::Arc;
use std::sync::mpsc::TryRecvError;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use instant::Instant;
use log::{error, warn};
use mio::{self, Events, Interest};
use parking_lot::FairMutex;

use super::mio_channel::Receiver;
use crate::event::{Event as TerminalEvent, ExitReason};
use crate::event_listener::ChannelEventListener;
use crate::local_tty;
use crate::model::ansi;
use crate::writeable_pty::Message;
use crate::zmodem::ZmodemRole;
use crate::zmodem::runtime::{
    Control, ErrorKind as TransferErrorKind, FileOutcome, MAX_INPUT_CHUNK, Role, RuntimeOutput,
    SubmitError, TransferError, TransferEvent, TransferHandle, TransferId, TransferOutcome,
    abort_sequence, next_transfer_id,
};
use crate::zmodem_detector::{DetectorOutcome, ZmodemDetector};

const READ_BUFFER_SIZE: usize = MAX_INPUT_CHUNK;
const MAX_RETAINED_INPUT: usize = 4 * MAX_INPUT_CHUNK;
const IO_BUDGET: usize = 64 * 1024;
// Transfers are acknowledgement-driven: the worker blocks until a wire buffer has actually been
// written to the PTY. Polling faster keeps large transfers from stalling on this round trip.
const WORKER_POLL_INTERVAL: Duration = Duration::from_millis(1);
const CHOICE_TIMEOUT: Duration = Duration::from_secs(120);
const CANCEL_TIMEOUT: Duration = Duration::from_millis(500);
const PROGRESS_RENDER_INTERVAL: Duration = Duration::from_millis(100);

pub const CHANNEL_TOKEN: mio::Token = mio::Token(0);
pub const PTY_TOKEN: mio::Token = mio::Token(1);
pub const SIGNALS_TOKEN: mio::Token = mio::Token(2);

pub trait ActiveTerminal: ansi::Handler + Send {
    fn exit(&mut self, reason: ExitReason);
}

pub struct EventLoop<P: local_tty::EventedPty, M: ActiveTerminal> {
    poll: mio::Poll,
    pty: P,
    rx: Receiver<Message>,
    terminal: Arc<FairMutex<M>>,
    zmodem: ZmodemRouter,
    event_listener: ChannelEventListener,
}

struct Writing {
    source: Cow<'static, [u8]>,
    written: usize,
}

/// ANSI parser and ordinary terminal responses. Transfer data never enters this queue.
pub struct State {
    write_list: VecDeque<Cow<'static, [u8]>>,
    writing: Option<Writing>,
    parser: ansi::Processor,
}

impl Default for State {
    fn default() -> Self {
        Self {
            write_list: VecDeque::new(),
            writing: None,
            parser: ansi::Processor::new(),
        }
    }
}

impl State {
    fn needs_write(&self) -> bool {
        self.writing.is_some() || !self.write_list.is_empty()
    }

    fn clear_writes(&mut self) {
        self.writing = None;
        self.write_list.clear();
    }
}

impl Writing {
    fn new(source: Cow<'static, [u8]>) -> Self {
        Self { source, written: 0 }
    }

    fn remaining_bytes(&self) -> &[u8] {
        &self.source[self.written..]
    }

    fn finished(&self) -> bool {
        self.written == self.source.len()
    }
}

struct TransferWriting {
    id: TransferId,
    sequence: Option<u64>,
    buffer: Writing,
}

struct ActiveTransfer {
    id: TransferId,
    role: Role,
    worker: TransferHandle,
    configured: bool,
    choice_deadline: Instant,
    cancel_deadline: Option<Instant>,
    retained: Vec<u8>,
    retained_offset: usize,
    tail_received: bool,
    tail: Vec<u8>,
    completed: bool,
    finished: bool,
    last_progress_render: Option<Instant>,
    rate_baseline: Option<(u64, Instant)>,
    last_rate: Option<u64>,
}

impl ActiveTransfer {
    fn retained_len(&self) -> usize {
        self.retained.len() - self.retained_offset
    }

    fn retain(&mut self, bytes: &[u8]) {
        if self.retained_offset != 0 {
            self.retained.drain(..self.retained_offset);
            self.retained_offset = 0;
        }
        debug_assert!(self.retained.len() + bytes.len() <= MAX_RETAINED_INPUT);
        self.retained.extend_from_slice(bytes);
    }

    fn submit_retained(&mut self) {
        if self.cancel_deadline.is_some() || self.tail_received {
            return;
        }
        while self.retained_offset < self.retained.len() {
            match self
                .worker
                .try_submit(&self.retained[self.retained_offset..])
            {
                Ok(0) | Err(SubmitError::Full | SubmitError::Closed) => break,
                Ok(accepted) => self.retained_offset += accepted,
            }
        }
        if self.retained_offset == self.retained.len() {
            self.retained.clear();
            self.retained_offset = 0;
        }
    }
}

#[derive(Default)]
struct ZmodemRouter {
    enabled: bool,
    detector: ZmodemDetector,
    active: Option<ActiveTransfer>,
    retiring: Vec<ActiveTransfer>,
    /// Outgoing protocol buffers, in order.
    ///
    /// This is a queue, not a single slot: assigning one buffer per poll silently drops a
    /// buffer that has not been written yet, which loses protocol bytes and makes the peer
    /// retransmit forever.
    wire: VecDeque<TransferWriting>,
    recovery_deadline: Option<Instant>,
}

fn sanitized_name(name: &str) -> String {
    name.chars()
        .take(256)
        .map(|character| {
            if character.is_control()
                || matches!(character, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
            {
                '?'
            } else {
                character
            }
        })
        .collect()
}

fn status_line(event: &TransferEvent, rate: Option<u64>) -> Vec<u8> {
    let direction = |role| match role {
        Role::Upload => "rz >>",
        Role::Download => "sz <<",
    };
    let (color, line) = match event {
        TransferEvent::Requested {
            role: Role::Download,
            ..
        } => (
            "\x1b[32m",
            "** sz waiting to send, please choose receive folder **\r\n".to_owned(),
        ),
        TransferEvent::Requested {
            role: Role::Upload, ..
        } => (
            "\x1b[32m",
            "** rz waiting to receive, please choose files **\r\n".to_owned(),
        ),
        TransferEvent::FileStarted { role, name, .. } => (
            "\x1b[32m",
            format!("{} {}\r\n", direction(*role), sanitized_name(name)),
        ),
        TransferEvent::Progress {
            role,
            name,
            bytes,
            total,
            ..
        } => (
            "\x1b[32m",
            format!(
                "{} {}::{}",
                direction(*role),
                sanitized_name(name),
                progress_detail(*bytes, *total, rate)
            ) + "\r\n",
        ),
        TransferEvent::FileResult {
            role,
            name,
            bytes,
            outcome,
            ..
        } => {
            let (color, result) = match outcome {
                FileOutcome::Completed => ("\x1b[90m", "completed"),
                FileOutcome::Skipped => ("\x1b[33m", "skipped"),
                FileOutcome::Failed(_) => ("\x1b[31m", "failed"),
            };
            (
                color,
                format!(
                    "{} {}::{}, {result}\r\n",
                    direction(*role),
                    sanitized_name(name),
                    progress_detail(*bytes, None, rate),
                ),
            )
        }
        TransferEvent::Finished { role, outcome, .. } => {
            let (color, result) = match outcome {
                TransferOutcome::Completed => ("\x1b[90m", "completed"),
                TransferOutcome::Cancelled => ("\x1b[33m", "cancelled"),
                TransferOutcome::RemoteCancelled => ("\x1b[33m", "cancelled by remote"),
                TransferOutcome::Failed(_) => ("\x1b[31m", "failed"),
            };
            (color, format!("{} {result}\r\n", direction(*role)))
        }
    };
    let replace_previous = matches!(
        event,
        TransferEvent::Progress { .. } | TransferEvent::FileResult { .. }
    );
    let move_up = if replace_previous { "\x1b[1A" } else { "" };
    format!("{move_up}\r\x1b[2K{color}{line}\x1b[0m").into_bytes()
}

/// A completed file's line reports the average rate, not a live sample.
/// Formats `percent, transferred/total, rate, ETA` in the style WeTERM uses.
fn progress_detail(bytes: u64, total: Option<u64>, rate: Option<u64>) -> String {
    let transferred = format_bytes(bytes);
    match total {
        Some(total) if total > 0 => {
            let percent = (bytes.saturating_mul(100) / total).min(100);
            let remaining = total.saturating_sub(bytes);
            match rate {
                Some(rate) if rate > 0 => format!(
                    "{percent}%, {transferred}/{}, {}/s, ETA {}",
                    format_bytes(total),
                    format_bytes(rate),
                    format_duration(remaining.saturating_add(rate - 1) / rate)
                ),
                _ => format!("{percent}%, {transferred}/{}", format_bytes(total)),
            }
        }
        _ => match rate {
            Some(rate) if rate > 0 => format!("{transferred}, {}/s", format_bytes(rate)),
            _ => transferred,
        },
    }
}

/// Byte counts in the units a person reads, sized for a progress line.
fn format_bytes(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;
    match bytes {
        b if b >= GIB => format!("{:.1} GiB", b as f64 / GIB as f64),
        b if b >= MIB => format!("{:.1} MiB", b as f64 / MIB as f64),
        b if b >= KIB => format!("{:.0} KiB", b as f64 / KIB as f64),
        b => format!("{b} B"),
    }
}

/// Renders a remaining-time estimate compactly, e.g. `12s`, `3m05s`.
fn format_duration(seconds: u64) -> String {
    if seconds < 60 {
        return format!("{seconds}s");
    }
    let minutes = seconds / 60;
    let seconds = seconds % 60;
    if minutes < 60 {
        return format!("{minutes}m{seconds:02}s");
    }
    format!("{}h{:02}m", minutes / 60, minutes % 60)
}

fn update_rate(active: &mut ActiveTransfer, event: &TransferEvent, now: Instant) {
    match event {
        TransferEvent::FileStarted { .. } => {
            active.rate_baseline = Some((0, now));
            active.last_rate = None;
        }
        TransferEvent::Progress { bytes, .. } => {
            let Some((baseline_bytes, baseline_time)) = active.rate_baseline else {
                active.rate_baseline = Some((*bytes, now));
                return;
            };
            let elapsed = now.saturating_duration_since(baseline_time);
            if elapsed < Duration::from_millis(250) || *bytes < baseline_bytes {
                return;
            }
            let elapsed_millis = elapsed.as_millis();
            if elapsed_millis == 0 {
                return;
            }
            let sample = bytes.saturating_sub(baseline_bytes).saturating_mul(1000)
                / u64::try_from(elapsed_millis).unwrap_or(u64::MAX);
            if sample > 0 {
                active.last_rate = Some(match active.last_rate {
                    Some(previous) => previous.saturating_mul(3).saturating_add(sample) / 4,
                    None => sample,
                });
            }
            active.rate_baseline = Some((*bytes, now));
        }
        TransferEvent::Requested { .. }
        | TransferEvent::FileResult { .. }
        | TransferEvent::Finished { .. } => {}
    }
}

fn log_transfer_failure(event: &TransferEvent) {
    match event {
        TransferEvent::FileResult {
            id,
            role,
            name,
            bytes,
            outcome: FileOutcome::Failed(error),
            ..
        } => warn!(
            "ZMODEM file failed: id={id} role={role:?} name={} bytes={bytes} kind={:?} error={}",
            sanitized_name(name),
            error.kind,
            error.message,
        ),
        TransferEvent::Finished {
            id,
            role,
            outcome: TransferOutcome::Failed(error),
            ..
        } => warn!(
            "ZMODEM transfer failed: id={id} role={role:?} kind={:?} error={}",
            error.kind, error.message,
        ),
        TransferEvent::Requested { .. }
        | TransferEvent::FileStarted { .. }
        | TransferEvent::Progress { .. }
        | TransferEvent::FileResult { .. }
        | TransferEvent::Finished { .. } => {}
    }
}

impl ZmodemRouter {
    fn busy(&self) -> bool {
        self.active.is_some() || !self.wire.is_empty()
    }

    fn read_capacity(&self) -> usize {
        match &self.active {
            Some(active) if active.cancel_deadline.is_some() || active.tail_received => 0,
            Some(active) if active.configured && active.retained_len() != 0 => 0,
            Some(active) => MAX_RETAINED_INPUT - active.retained_len(),
            None => READ_BUFFER_SIZE,
        }
    }

    fn next_timeout(&self, now: Instant) -> Option<Duration> {
        let candidate = self
            .detector
            .next_deadline()
            .map(|at| at.saturating_duration_since(now));
        let worker = self.active.as_ref().map(|active| {
            let deadline = active.cancel_deadline.unwrap_or(active.choice_deadline);
            if active.configured && active.cancel_deadline.is_none() {
                WORKER_POLL_INTERVAL
            } else {
                WORKER_POLL_INTERVAL.min(deadline.saturating_duration_since(now))
            }
        });
        let recovery = self
            .recovery_deadline
            .map(|at| at.saturating_duration_since(now));
        let retiring = (!self.retiring.is_empty()).then_some(WORKER_POLL_INTERVAL);
        minimum_timeout(
            minimum_timeout(candidate, worker),
            minimum_timeout(recovery, retiring),
        )
    }

    fn emit(
        event: TransferEvent,
        rate: Option<u64>,
        listener: &ChannelEventListener,
        render: &mut Vec<u8>,
    ) {
        render.extend(status_line(&event, rate));
        listener.send_terminal_event(TerminalEvent::Zmodem(event));
    }

    fn begin(
        &mut self,
        id: TransferId,
        role: Role,
        requested: bool,
        listener: &ChannelEventListener,
        render: &mut Vec<u8>,
    ) {
        match TransferHandle::spawn(id, role) {
            Ok(worker) => {
                self.active = Some(ActiveTransfer {
                    id,
                    role,
                    worker,
                    configured: false,
                    choice_deadline: Instant::now() + CHOICE_TIMEOUT,
                    cancel_deadline: None,
                    retained: Vec::new(),
                    retained_offset: 0,
                    tail_received: false,
                    tail: Vec::new(),
                    completed: false,
                    finished: false,
                    last_progress_render: None,
                    rate_baseline: None,
                    last_rate: None,
                });
                if requested {
                    Self::emit(
                        TransferEvent::Requested { id, role },
                        None,
                        listener,
                        render,
                    );
                }
            }
            Err(error) => {
                self.queue_abort(id);
                Self::emit(
                    TransferEvent::Finished {
                        id,
                        role,
                        outcome: TransferOutcome::Failed(error),
                        committed_paths: Vec::new(),
                    },
                    None,
                    listener,
                    render,
                );
            }
        }
    }

    fn route(&mut self, bytes: &[u8], listener: &ChannelEventListener) -> Vec<u8> {
        if let Some(active) = &mut self.active {
            active.retain(bytes);
            active.submit_retained();
            return Vec::new();
        }
        if !self.enabled {
            return bytes.to_vec();
        }
        if self.recovery_deadline.is_some() {
            let mut render = Vec::new();
            for byte in bytes {
                match self
                    .detector
                    .push(std::slice::from_ref(byte), Instant::now())
                {
                    DetectorOutcome::Render(bytes) => render.extend(bytes),
                    DetectorOutcome::Started { render_before, .. } => render.extend(render_before),
                }
            }
            return render;
        }
        if !self.wire.is_empty() {
            let mut render = Vec::new();
            // Only a validated header is suppressed during bounded recovery, never its following
            // bytes: a repeated handshake and the shell prompt may share the same PTY read.
            for byte in bytes {
                match self
                    .detector
                    .push(std::slice::from_ref(byte), Instant::now())
                {
                    DetectorOutcome::Render(bytes) => render.extend(bytes),
                    DetectorOutcome::Started { render_before, .. } => render.extend(render_before),
                }
            }
            return render;
        }
        match self.detector.push(bytes, Instant::now()) {
            DetectorOutcome::Render(bytes) => bytes,
            DetectorOutcome::Started {
                role,
                mut render_before,
                protocol,
                ..
            } => {
                let role = match role {
                    ZmodemRole::Upload => Role::Upload,
                    ZmodemRole::Download => Role::Download,
                };
                self.begin(next_transfer_id(), role, true, listener, &mut render_before);
                if let Some(active) = &mut self.active {
                    active.retain(&protocol);
                    active.submit_retained();
                }
                render_before
            }
        }
    }

    /// Discards queued protocol data and sends the abort sequence first.
    ///
    /// Dropping the pending buffers matters for a user-initiated cancel: their Ctrl-C must not
    /// be followed by the rest of a file being written into the terminal.
    fn queue_abort(&mut self, id: TransferId) {
        self.recovery_deadline = Some(Instant::now() + CANCEL_TIMEOUT);
        self.wire.clear();
        self.wire.push_back(TransferWriting {
            id,
            sequence: None,
            buffer: Writing::new(Cow::Owned(abort_sequence())),
        });
    }

    fn cancel(&mut self, now: Instant) {
        if let Some(active) = &mut self.active {
            if active.cancel_deadline.is_some() {
                return;
            }
            active.worker.cancel();
            active.cancel_deadline = Some(now + CANCEL_TIMEOUT);
            let id = active.id;
            self.queue_abort(id);
        }
    }

    fn control(&mut self, control: Control, listener: &ChannelEventListener) -> Vec<u8> {
        let mut render = Vec::new();
        match control {
            Control::Enable(enabled) => {
                self.enabled = enabled;
                if !enabled {
                    render.extend(self.detector.flush());
                    self.cancel(Instant::now());
                }
            }
            Control::StartUpload { id, paths } => {
                if self.enabled && !self.busy() {
                    render.extend(self.detector.flush());
                    self.begin(id, Role::Upload, false, listener, &mut render);
                    render.extend(self.control(Control::Upload { id, paths }, listener));
                }
            }
            Control::Upload { id, paths } => {
                if let Some(active) = &mut self.active
                    && active.id == id
                    && active.role == Role::Upload
                    && !active.configured
                    && active.cancel_deadline.is_none()
                    && Instant::now() < active.choice_deadline
                {
                    match active.worker.configure_upload(paths) {
                        Ok(()) => active.configured = true,
                        Err(error) => self.fail(error, listener, &mut render),
                    }
                }
            }
            Control::Download {
                id,
                directory,
                policy,
            } => {
                if let Some(active) = &mut self.active
                    && active.id == id
                    && active.role == Role::Download
                    && !active.configured
                    && active.cancel_deadline.is_none()
                    && Instant::now() < active.choice_deadline
                {
                    match active.worker.configure_download(directory, policy) {
                        Ok(()) => active.configured = true,
                        Err(error) => self.fail(error, listener, &mut render),
                    }
                }
            }
            Control::Cancel { id } => {
                if self.active.as_ref().is_some_and(|active| active.id == id) {
                    self.cancel(Instant::now());
                }
            }
        }
        render
    }

    fn fail(
        &mut self,
        error: TransferError,
        listener: &ChannelEventListener,
        render: &mut Vec<u8>,
    ) {
        self.cancel(Instant::now());
        if let Some(active) = &mut self.active
            && !active.finished
        {
            active.finished = true;
            Self::emit(
                TransferEvent::Finished {
                    id: active.id,
                    role: active.role,
                    outcome: TransferOutcome::Failed(error),
                    committed_paths: Vec::new(),
                },
                active.last_rate,
                listener,
                render,
            );
        }
    }

    fn poll(&mut self, now: Instant, listener: &ChannelEventListener) -> Vec<u8> {
        let mut render = self.detector.expire(now);
        self.poll_retiring(listener, &mut render);
        if self
            .recovery_deadline
            .is_some_and(|deadline| now >= deadline)
        {
            self.recovery_deadline = None;
            render.extend(self.detector.flush());
            if self.active.is_none() {
                self.wire.retain(|wire| wire.sequence.is_some());
            }
        }
        if self.active.as_ref().is_some_and(|active| {
            !active.configured && !active.finished && now >= active.choice_deadline
        }) {
            self.fail(
                TransferError {
                    kind: TransferErrorKind::Timeout,
                    message: "File selection timed out".to_owned(),
                },
                listener,
                &mut render,
            );
        }
        let mut disconnected = false;
        // One wire buffer per write keeps the worker's credit exact, but draining several per
        // event-loop turn is what lets a large transfer pipeline instead of stalling.
        for _ in 0..256 {
            let Some(active) = &mut self.active else {
                break;
            };
            // The worker waits for each sequence acknowledgement before producing another wire
            // buffer, but can still send Abort while the current PTY write is stalled.
            match active.worker.try_recv() {
                Ok(RuntimeOutput::Wire {
                    id,
                    sequence,
                    bytes,
                }) => {
                    if id == active.id && active.cancel_deadline.is_none() {
                        self.wire.push_back(TransferWriting {
                            id,
                            sequence: Some(sequence),
                            buffer: Writing::new(Cow::Owned(bytes)),
                        });
                    }
                }
                Ok(RuntimeOutput::Abort { id }) => {
                    if id == active.id && active.cancel_deadline.is_none() {
                        active.cancel_deadline = Some(now + CANCEL_TIMEOUT);
                        self.queue_abort(id);
                    }
                }
                Ok(RuntimeOutput::Tail { id, bytes }) => {
                    if id == active.id {
                        active.tail_received = true;
                        active.tail.extend(bytes);
                    }
                }
                Ok(RuntimeOutput::Event(event)) => {
                    if active.finished {
                        continue;
                    }
                    let finished = matches!(event, TransferEvent::Finished { .. });
                    log_transfer_failure(&event);
                    update_rate(active, &event, now);
                    if finished {
                        active.finished = true;
                        active.completed = matches!(
                            event,
                            TransferEvent::Finished {
                                outcome: TransferOutcome::Completed,
                                ..
                            }
                        );
                    }
                    let progress = matches!(event, TransferEvent::Progress { .. });
                    let refresh = !progress
                        || active.last_progress_render.is_none_or(|at| {
                            now.saturating_duration_since(at) >= PROGRESS_RENDER_INTERVAL
                        });
                    if refresh {
                        if progress {
                            active.last_progress_render = Some(now);
                        }
                        Self::emit(event, active.last_rate, listener, &mut render);
                    }
                    if finished {
                        disconnected = true;
                        break;
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }
        let detach = self.active.as_ref().is_some_and(|active| {
            active
                .cancel_deadline
                .is_some_and(|deadline| now >= deadline)
        });
        if disconnected || detach {
            if let Some(active) = self.active.take() {
                if active.cancel_deadline.is_some() {
                    self.recovery_deadline = Some(now + CANCEL_TIMEOUT);
                }
                if detach && !disconnected && !active.finished {
                    // Cancellation cannot interrupt a blocked OS operation. Keep receiving its
                    // ordered results after releasing the PTY, rather than guessing whether an
                    // already-started atomic file publication committed. Live workers are capped
                    // globally by the runtime, including these retiring handles.
                    self.retiring.push(active);
                    return render;
                }
                if !active.finished {
                    let outcome = if active.cancel_deadline.is_some() {
                        TransferOutcome::Cancelled
                    } else {
                        TransferOutcome::Failed(TransferError {
                            kind: TransferErrorKind::WorkerStopped,
                            message: "Transfer worker stopped".to_owned(),
                        })
                    };
                    Self::emit(
                        TransferEvent::Finished {
                            id: active.id,
                            role: active.role,
                            outcome,
                            committed_paths: Vec::new(),
                        },
                        active.last_rate,
                        listener,
                        &mut render,
                    );
                    self.queue_abort(active.id);
                }
                // Accepted bytes belong to the worker; only its Tail and our unaccepted suffix
                // may return to ANSI. Never replay an ingress buffer after successful submission.
                if active.cancel_deadline.is_none() {
                    render.extend(active.tail);
                    if active.completed || active.tail_received {
                        render.extend_from_slice(&active.retained[active.retained_offset..]);
                    }
                }
            }
        } else if let Some(active) = &mut self.active {
            active.submit_retained();
        }
        render
    }

    fn poll_retiring(&mut self, listener: &ChannelEventListener, render: &mut Vec<u8>) {
        let mut index = 0;
        while index < self.retiring.len() {
            let active = &mut self.retiring[index];
            let mut disconnected = false;
            for _ in 0..64 {
                match active.worker.try_recv() {
                    Ok(RuntimeOutput::Event(event)) => {
                        if !active.finished {
                            log_transfer_failure(&event);
                            active.finished = matches!(event, TransferEvent::Finished { .. });
                            if self.active.is_none() {
                                render.extend(status_line(&event, active.last_rate));
                            }
                            listener.send_terminal_event(TerminalEvent::Zmodem(event));
                        }
                    }
                    Ok(RuntimeOutput::Tail { bytes, .. }) => active.tail.extend(bytes),
                    Ok(RuntimeOutput::Wire { .. } | RuntimeOutput::Abort { .. }) => {}
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
            if disconnected {
                let active = self.retiring.remove(index);
                if !active.finished {
                    Self::emit(
                        TransferEvent::Finished {
                            id: active.id,
                            role: active.role,
                            outcome: TransferOutcome::Failed(TransferError {
                                kind: TransferErrorKind::WorkerStopped,
                                message: "Cancelled worker stopped before reporting its result"
                                    .to_owned(),
                            }),
                            committed_paths: Vec::new(),
                        },
                        active.last_rate,
                        listener,
                        render,
                    );
                }
            } else {
                index += 1;
            }
        }
    }

    /// Writes queued protocol buffers in order, acknowledging each once fully written.
    fn write(&mut self, writer: &mut impl Write, can_write: &mut bool) -> io::Result<()> {
        let Some(wire) = self.wire.front_mut() else {
            return Ok(());
        };
        if !write_buffer(writer, &mut wire.buffer, can_write)? {
            return Ok(());
        }
        let wire = self.wire.pop_front().expect("completed transfer write");
        if wire.sequence.is_none() {
            self.recovery_deadline = Some(Instant::now() + CANCEL_TIMEOUT);
        }
        if let Some(sequence) = wire.sequence
            && let Some(active) = &mut self.active
            && active.id == wire.id
        {
            active.worker.acknowledge_write(sequence);
        }
        Ok(())
    }
}

fn minimum_timeout(a: Option<Duration>, b: Option<Duration>) -> Option<Duration> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(timeout), None) | (None, Some(timeout)) => Some(timeout),
        (None, None) => None,
    }
}

fn write_buffer(
    writer: &mut impl Write,
    buffer: &mut Writing,
    can_write: &mut bool,
) -> io::Result<bool> {
    let mut budget = IO_BUDGET;
    while !buffer.finished() && budget != 0 {
        let remaining = buffer.remaining_bytes();
        match writer.write(&remaining[..remaining.len().min(budget)]) {
            Ok(0) => {
                *can_write = false;
                break;
            }
            Ok(written) => {
                buffer.written += written;
                budget -= written;
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                *can_write = false;
                break;
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => break,
            Err(error) => return Err(error),
        }
    }
    Ok(buffer.finished())
}

enum ChannelResult {
    Continue { more: bool },
    TerminateLoop { child_exited: bool },
}

impl<P, M> EventLoop<P, M>
where
    P: local_tty::EventedPty + Send + 'static,
    M: ActiveTerminal + 'static,
{
    pub fn new(
        terminal: Arc<FairMutex<M>>,
        event_listener: ChannelEventListener,
        pty: P,
        rx: Receiver<Message>,
    ) -> Self {
        Self {
            poll: mio::Poll::new().expect("create mio Poll"),
            pty,
            rx,
            terminal,
            zmodem: ZmodemRouter::default(),
            event_listener,
        }
    }

    fn render(&self, bytes: &[u8], state: &mut State) {
        if bytes.is_empty() {
            return;
        }
        let force_visible = self.zmodem.busy();
        if force_visible && state.parser.sync_output_buffer_len().is_some() {
            let mut responses = Vec::new();
            state
                .parser
                .finish_sync_output(&mut *self.terminal.lock(), &mut responses);
        }
        let mut responses = Vec::new();
        for chunk in bytes.chunks(IO_BUDGET) {
            state
                .parser
                .parse_bytes(&mut *self.terminal.lock(), chunk, &mut responses);
        }
        if !responses.is_empty() && !self.zmodem.busy() {
            state.write_list.push_back(Cow::Owned(responses));
        }
        if force_visible || bytes.len() > state.parser.sync_output_buffer_len().unwrap_or(0) {
            self.event_listener.send_wakeup_event();
        }
    }

    fn drain_recv_channel(&mut self, state: &mut State) -> ChannelResult {
        for _ in 0..256 {
            let msg = match self.rx.try_recv() {
                Ok(msg) => msg,
                Err(TryRecvError::Empty) => return ChannelResult::Continue { more: false },
                Err(TryRecvError::Disconnected) => {
                    return ChannelResult::TerminateLoop {
                        child_exited: false,
                    };
                }
            };
            match msg {
                Message::Input(input) => {
                    if self.zmodem.busy() {
                        if input.contains(&0x03) {
                            self.zmodem.cancel(Instant::now());
                        }
                    } else if !input.is_empty() {
                        state.write_list.push_back(input);
                    }
                }
                Message::Shutdown => {
                    return ChannelResult::TerminateLoop {
                        child_exited: false,
                    };
                }
                Message::Resize(size) => self.pty.on_resize(&size),
                Message::ChildExited => return ChannelResult::TerminateLoop { child_exited: true },
                Message::Zmodem(control) => {
                    let render = self.zmodem.control(control, &self.event_listener);
                    if self.zmodem.busy() {
                        state.clear_writes();
                    }
                    self.render(&render, state);
                }
                Message::StartZmodemUpload { .. } => {
                    self.event_listener
                        .send_terminal_event(TerminalEvent::ZmodemFinished {
                            error: Some("Legacy upload request is no longer supported".to_owned()),
                        });
                }
            }
        }
        ChannelResult::Continue { more: true }
    }

    fn pty_read(
        &mut self,
        state: &mut State,
        buf: &mut [u8],
        can_read: &mut bool,
    ) -> io::Result<()> {
        let capacity = buf.len().min(self.zmodem.read_capacity());
        if capacity == 0 {
            return Ok(());
        }
        match self.pty.reader().read(&mut buf[..capacity]) {
            Ok(0) => *can_read = false,
            Ok(count) => {
                let render = self.zmodem.route(&buf[..count], &self.event_listener);
                if self.zmodem.busy() {
                    state.clear_writes();
                }
                self.render(&render, state);
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => *can_read = false,
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
        Ok(())
    }

    fn pty_write(&mut self, state: &mut State, can_write: &mut bool) -> io::Result<()> {
        if self.zmodem.busy() {
            return self.zmodem.write(self.pty.writer(), can_write);
        }
        if state.writing.is_none() {
            state.writing = state.write_list.pop_front().map(Writing::new);
        }
        if let Some(buffer) = &mut state.writing
            && write_buffer(self.pty.writer(), buffer, can_write)?
        {
            state.writing = None;
        }
        Ok(())
    }

    fn needs_write(&self, state: &State) -> bool {
        if self.zmodem.busy() {
            !self.zmodem.wire.is_empty()
        } else {
            state.needs_write()
        }
    }

    pub fn spawn(mut self) -> JoinHandle<()> {
        #[cfg(test)]
        let feature_flag_overrides = warp_core::features::get_overrides();

        thread::Builder::new()
            .name("PTY reader".into())
            .spawn(move || {
                #[cfg(test)]
                warp_core::features::set_overrides(feature_flag_overrides);

                let mut state = State::default();
                let mut buf = [0u8; READ_BUFFER_SIZE];
                let mut can_read = false;
                let mut can_write = false;
                let mut more_control = false;
                self.poll
                    .registry()
                    .register(&mut self.rx, CHANNEL_TOKEN, Interest::READABLE)
                    .unwrap();
                self.pty
                    .register(&self.poll, Interest::READABLE | Interest::WRITABLE)
                    .unwrap();
                let mut events = Events::with_capacity(1024);
                let mut child_exited = false;
                let mut closing_deadline: Option<Instant> = None;

                loop {
                    events.clear();
                    let ready = more_control
                        || (can_read && self.zmodem.read_capacity() != 0)
                        || (can_write && self.needs_write(&state));
                    let timeout = if ready {
                        Some(Duration::ZERO)
                    } else {
                        let now = Instant::now();
                        minimum_timeout(
                            minimum_timeout(
                                state.parser.sync_output_remaining_timeout(),
                                self.zmodem.next_timeout(now),
                            ),
                            closing_deadline
                                .map(|deadline| deadline.saturating_duration_since(now)),
                        )
                    };
                    if let Err(error) = self.poll.poll(&mut events, timeout) {
                        if error.kind() == ErrorKind::Interrupted {
                            continue;
                        }
                        error!("EventLoop polling error: {error}");
                        break;
                    }

                    // Timed worker polls must not prematurely finish ANSI synchronized output.
                    if state
                        .parser
                        .sync_output_remaining_timeout()
                        .is_some_and(|timeout| timeout.is_zero())
                    {
                        let mut responses = Vec::new();
                        state
                            .parser
                            .finish_sync_output(&mut *self.terminal.lock(), &mut responses);
                        if !responses.is_empty() && !self.zmodem.busy() {
                            state.write_list.push_back(Cow::Owned(responses));
                        }
                        self.event_listener.send_wakeup_event();
                    }

                    match self.drain_recv_channel(&mut state) {
                        ChannelResult::Continue { more } => more_control = more,
                        ChannelResult::TerminateLoop {
                            child_exited: exited,
                        } => {
                            if exited {
                                child_exited = true;
                                closing_deadline.get_or_insert(Instant::now() + CANCEL_TIMEOUT);
                                can_read = true;
                            } else {
                                break;
                            }
                        }
                    }

                    for event in events.iter() {
                        let token = event.token();
                        if token == CHANNEL_TOKEN {
                            continue;
                        }
                        if token == self.pty.child_event_token() {
                            if let Some(local_tty::ChildEvent::Exited) = self.pty.next_child_event()
                            {
                                child_exited = true;
                                closing_deadline.get_or_insert(Instant::now() + CANCEL_TIMEOUT);
                                can_read = true;
                            }
                        } else if token == self.pty.read_token() || token == self.pty.write_token()
                        {
                            #[cfg(unix)]
                            if event.is_read_closed() || event.is_write_closed() {
                                closing_deadline.get_or_insert(Instant::now() + CANCEL_TIMEOUT);
                                can_read = true;
                            }
                            can_read |= event.is_readable();
                            can_write |= event.is_writable();
                        }
                    }

                    let render = self.zmodem.poll(Instant::now(), &self.event_listener);
                    self.render(&render, &mut state);
                    if can_read
                        && self.zmodem.read_capacity() != 0
                        && let Err(error) = self.pty_read(&mut state, &mut buf, &mut can_read)
                    {
                        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
                        if error.raw_os_error() == Some(libc::EIO) {
                            can_read = false;
                            closing_deadline.get_or_insert(Instant::now() + CANCEL_TIMEOUT);
                            continue;
                        }
                        error!("Error reading from PTY in event loop: {error}");
                        break;
                    }
                    if can_write
                        && self.needs_write(&state)
                        && let Err(error) = self.pty_write(&mut state, &mut can_write)
                    {
                        error!("Error writing to PTY in event loop: {error}");
                        if closing_deadline.is_some() {
                            can_write = false;
                        } else {
                            break;
                        }
                    }
                    if closing_deadline.is_some_and(|deadline| Instant::now() >= deadline)
                        || (closing_deadline.is_some() && !can_read && !self.zmodem.busy())
                    {
                        break;
                    }
                }

                let pending = self.zmodem.detector.flush();
                self.render(&pending, &mut state);
                self.zmodem.cancel(Instant::now());
                // Dropping a handle requests cancellation; it never joins a disk worker.
                self.zmodem.active = None;
                self.zmodem.retiring.clear();
                let _ = self.poll.registry().deregister(&mut self.rx);
                let _ = self.pty.deregister(&self.poll);
                if !child_exited && let Err(error) = self.pty.kill() {
                    log::warn!("Failed to kill PTY process: {error:#}");
                }
                if child_exited {
                    self.terminal.lock().exit(ExitReason::ShellProcessExited);
                    self.event_listener.send_wakeup_event();
                }
                self.terminal.lock().exit(ExitReason::PtyDisconnected);
            })
            .expect("thread spawn works")
    }
}

#[cfg(test)]
#[path = "event_loop_tests.rs"]
mod tests;
