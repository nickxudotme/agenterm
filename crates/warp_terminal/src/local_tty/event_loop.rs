// The code in this file is adapted from the alacritty_terminal crate under the
// Apache license; see: crates/warp_terminal/src/model/LICENSE-ALACRITTY.

//! The main event loop which performs I/O on the pseudoterminal.

use std::borrow::Cow;
use std::collections::VecDeque;
use std::io::{self, ErrorKind, Read, Write};
use std::marker::Send;
use std::ops::DerefMut;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use instant::Instant;
use log::error;
use mio::{self, Events, Interest};
use parking_lot::{FairMutex, FairMutexGuard};

use super::mio_channel::Receiver;
use crate::event::{Event as TerminalEvent, ExitReason};
use crate::event_listener::ChannelEventListener;
use crate::local_tty;
use crate::model::ansi;
use crate::writeable_pty::Message;
use crate::zmodem::{
    DetectorOutcome, Frame, OwnedEvent, TransferProgress, UploadFile, ZmodemDetector, ZmodemError,
    ZmodemRole, ZmodemSession, ZmodemStep,
};

/// The size of the buffer to read data into from the PTY.
const READ_BUFFER_SIZE: usize = 0x4_0000;

/// Max bytes to process from the PTY while holding the lock before giving
/// someone else an opportunity to lock it.
const MAX_LOCKED_READ: usize = 0x1_0000;

/// How often the ZMODEM status line is refreshed. Fast enough to look live,
/// slow enough that a high-throughput transfer does not flood the parser.
const PROGRESS_RENDER_INTERVAL: Duration = Duration::from_millis(100);

/// Status-line state for the transfer being rendered into the terminal.
#[derive(Default)]
struct ZmodemStatus {
    progress: TransferProgress,
    /// When the status line was last drawn, used to throttle refreshes.
    last_render: Option<Instant>,
}

impl ZmodemStatus {
    /// Whether enough time has passed to redraw the status line.
    fn should_refresh(&self) -> bool {
        self.last_render
            .is_none_or(|at| at.elapsed() >= PROGRESS_RENDER_INTERVAL)
    }

    /// Draws the status line and records when it happened.
    fn refresh(&mut self, role: ZmodemRole) -> Vec<u8> {
        self.last_render = Some(Instant::now());
        self.progress.render_line(role)
    }
}

pub const CHANNEL_TOKEN: mio::Token = mio::Token(0);
pub const PTY_TOKEN: mio::Token = mio::Token(1);
pub const SIGNALS_TOKEN: mio::Token = mio::Token(2);

/// The main event!.. loop.
///
/// Handles all the PTY I/O and runs the PTY parser which updates terminal
/// state.
pub trait ActiveTerminal: ansi::Handler + Send {
    fn exit(&mut self, reason: ExitReason);
}

pub struct EventLoop<P: local_tty::EventedPty, M: ActiveTerminal> {
    poll: mio::Poll,
    pty: P,
    rx: Receiver<Message>,
    terminal: Arc<FairMutex<M>>,

    /// Watches PTY output for the start of a ZMODEM transfer. Only consulted
    /// while no transfer is active, so streaming output pays a scan, not a
    /// protocol round trip.
    zmodem_detector: ZmodemDetector,

    /// The active ZMODEM transfer, once one has been detected. Bytes are routed
    /// here instead of the ANSI parser until the transfer ends.
    zmodem: Option<ZmodemSession>,

    /// Rendering state for the in-flight transfer.
    zmodem_status: ZmodemStatus,

    /// The event listener is available to the PTY event loop
    /// to emit relevant events to subscribers. The ansi handler
    /// also has a handle to the event listener, so events may also
    /// be emitted at a later stage (i.e. when we have a better idea
    /// of what the bytes from the PTY actually meant).
    event_listener: ChannelEventListener,
}

/// Helper type which tracks how much of a buffer has been written.
struct Writing {
    source: Cow<'static, [u8]>,
    written: usize,
}

/// All of the mutable state needed to run the event loop.
///
/// Contains list of items to write, current write state, etc. Anything that
/// would otherwise be mutated on the `EventLoop` goes here.
pub struct State {
    write_list: VecDeque<Cow<'static, [u8]>>,
    writing: Option<Writing>,
    parser: ansi::Processor,
}

impl Default for State {
    fn default() -> State {
        State {
            write_list: VecDeque::new(),
            parser: ansi::Processor::new(),
            writing: None,
        }
    }
}

impl State {
    #[inline]
    fn ensure_next(&mut self) {
        if self.writing.is_none() {
            self.goto_next();
        }
    }

    #[inline]
    fn goto_next(&mut self) {
        self.writing = self.write_list.pop_front().map(Writing::new);
    }

    #[inline]
    fn take_current(&mut self) -> Option<Writing> {
        self.writing.take()
    }

    #[inline]
    fn needs_write(&self) -> bool {
        self.writing.is_some() || !self.write_list.is_empty()
    }

    #[inline]
    fn set_current(&mut self, new: Option<Writing>) {
        self.writing = new;
    }
}

impl Writing {
    #[inline]
    fn new(c: Cow<'static, [u8]>) -> Writing {
        Writing {
            source: c,
            written: 0,
        }
    }

    #[inline]
    fn advance(&mut self, n: usize) {
        self.written += n;
    }

    #[inline]
    fn remaining_bytes(&self) -> &[u8] {
        &self.source[self.written..]
    }

    #[inline]
    fn finished(&self) -> bool {
        self.written >= self.source.len()
    }
}

enum ChannelResult {
    Continue,
    TerminateLoop { child_exited: bool },
}

/// Routes PTY output through the active ZMODEM transfer, if any.
///
/// Returns the bytes that should still be rendered by the terminal. During a
/// transfer that is only whatever preceded the protocol frames; the protocol
/// bytes themselves are consumed and answered on the PTY.
///
/// This is a free function so it never holds a borrow on the event loop across
/// the terminal lock taken by `pty_read`.
fn route_zmodem(
    detector: &mut ZmodemDetector,
    active: &mut Option<ZmodemSession>,
    status: &mut ZmodemStatus,
    listener: &ChannelEventListener,
    bytes: &[u8],
    state: &mut State,
) -> Vec<u8> {
    if let Some(session) = active.as_mut() {
        // Uploads read and answer their own file data, so forwarding PTY
        // output is all that is needed to advance either direction.
        let step = session.submit_wire(bytes);
        let role = session.role();
        return finish_zmodem_step(step, active, detector, status, role, listener, state)
            .unwrap_or_default();
    }

    match detector.push(bytes) {
        DetectorOutcome::Render(renderable) => renderable,
        DetectorOutcome::Started {
            header,
            render_before,
            protocol,
        } => {
            // Only `sz` (which opens with ZRQINIT or ZFILE) starts a download
            // automatically. A ZRINIT means the remote is running `rz` and is
            // waiting for us to send, which only an explicit upload starts.
            if !matches!(header.frame, Frame::Zrqinit | Frame::Zfile) {
                return render_before;
            }
            match ZmodemSession::new_download() {
                Ok(mut session) => {
                    log::info!(
                        "ZMODEM download detected: frame={:?} protocol_bytes={}",
                        header.frame,
                        protocol.len()
                    );
                    listener.send_terminal_event(TerminalEvent::ZmodemDownloadStarted {
                        file_name: None,
                    });
                    let reply = session.submit_wire(&protocol);
                    let role = session.role();
                    detector.reset();
                    *status = ZmodemStatus::default();
                    *active = Some(session);

                    let mut rendered = render_before.clone();
                    if let Some(line) =
                        finish_zmodem_step(reply, active, detector, status, role, listener, state)
                    {
                        rendered.extend_from_slice(&line);
                    }
                    rendered
                }
                Err(error) => {
                    log::warn!("Failed to start ZMODEM download: {error}");
                    detector.reset();
                    // Fall back to rendering. The output is garbled, but the
                    // terminal stays usable instead of swallowing bytes.
                    let mut all = render_before;
                    all.extend_from_slice(&protocol);
                    all
                }
            }
        }
    }
}

/// Applies one protocol step: queues PTY replies, reports progress, and ends
/// the transfer once it is done.
#[allow(clippy::too_many_arguments)]
fn finish_zmodem_step(
    result: Result<ZmodemStep, ZmodemError>,
    active: &mut Option<ZmodemSession>,
    detector: &mut ZmodemDetector,
    status: &mut ZmodemStatus,
    role: ZmodemRole,
    listener: &ChannelEventListener,
    state: &mut State,
) -> Option<Vec<u8>> {
    let step = match result {
        Ok(step) => step,
        Err(error) => {
            log::warn!("ZMODEM transfer failed: {error}");
            let summary = status
                .progress
                .render_summary(role, Some(&error.to_string()));
            end_zmodem(active, detector, listener, Some(error.to_string()));
            return Some(summary);
        }
    };

    if !step.to_pty.is_empty() {
        log::info!("ZMODEM queued {} reply bytes to the PTY", step.to_pty.len());
        state.write_list.push_back(Cow::Owned(step.to_pty.clone()));
    } else if !step.events.is_empty() || step.file_data.is_some() {
        log::info!("ZMODEM step produced no reply bytes");
    }

    let mut rendered = Vec::new();

    if let Some(chunk) = &step.file_data
        && !chunk.data.is_empty()
    {
        status.progress.bytes_transferred += chunk.data.len() as u64;
        listener.send_terminal_event(TerminalEvent::ZmodemFileData {
            name: chunk.name.clone(),
            data: chunk.data.clone(),
        });
    }

    for event in &step.events {
        match event {
            OwnedEvent::FileStarted { name, size } => {
                status.progress.file_name = name.clone();
                status.progress.bytes_transferred = 0;
                status.progress.bytes_total = size.map(u64::from).unwrap_or(0);
                // Show the file immediately; waiting for the first throttled
                // tick would leave the block blank as a transfer starts.
                rendered.extend_from_slice(&status.refresh(role));
                listener.send_terminal_event(TerminalEvent::ZmodemProgress {
                    file_name: name.clone(),
                    bytes_transferred: 0,
                    bytes_total: status.progress.bytes_total,
                });
            }
            OwnedEvent::FileCompleted => {
                rendered.extend_from_slice(&status.progress.render_summary(role, None));
                status.last_render = None;
                listener.send_terminal_event(TerminalEvent::ZmodemFileCompleted {
                    file_name: status.progress.file_name.clone(),
                });
            }
            OwnedEvent::SessionCompleted => {
                end_zmodem(active, detector, listener, None);
                return Some(rendered);
            }
            OwnedEvent::Aborted => {
                rendered
                    .extend_from_slice(&status.progress.render_summary(role, Some("cancelled")));
                end_zmodem(
                    active,
                    detector,
                    listener,
                    Some("Transfer cancelled".to_owned()),
                );
                return Some(rendered);
            }
        }
    }

    // Refresh the status line at a readable rate rather than once per
    // subpacket, which would flood the parser on a fast transfer.
    if rendered.is_empty() && step.file_data.is_some() && status.should_refresh() {
        rendered.extend_from_slice(&status.refresh(role));
    }

    if step.finished {
        end_zmodem(active, detector, listener, None);
    }
    Some(rendered)
}

/// Tears down the active transfer and hands the stream back to the parser.
fn end_zmodem(
    active: &mut Option<ZmodemSession>,
    detector: &mut ZmodemDetector,
    listener: &ChannelEventListener,
    error: Option<String>,
) {
    log::info!("ZMODEM transfer ended: error={error:?}");
    *active = None;
    detector.reset();
    listener.send_terminal_event(TerminalEvent::ZmodemFinished { error });
    listener.send_wakeup_event();
}

impl<P, M> EventLoop<P, M>
where
    P: local_tty::EventedPty + Send + 'static,
    M: ActiveTerminal + 'static,
{
    /// Create a new event loop.
    pub fn new(
        terminal: Arc<FairMutex<M>>,
        event_listener: ChannelEventListener,
        pty: P,
        rx: Receiver<Message>,
    ) -> EventLoop<P, M> {
        EventLoop {
            poll: mio::Poll::new().expect("create mio Poll"),
            pty,
            rx,
            terminal,
            zmodem_detector: ZmodemDetector::new(),
            zmodem: None,
            zmodem_status: ZmodemStatus::default(),
            event_listener,
        }
    }

    /// Drain the channel.
    ///
    /// Returns `false` when a shutdown message was received.
    fn drain_recv_channel(&mut self, state: &mut State) -> ChannelResult {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Message::Input(input) => state.write_list.push_back(input),
                Message::Shutdown => {
                    return ChannelResult::TerminateLoop {
                        child_exited: false,
                    };
                }
                Message::Resize(size) => self.pty.on_resize(&size),
                Message::StartZmodemUpload { paths } => self.start_zmodem_upload(paths, state),
                Message::ChildExited => return ChannelResult::TerminateLoop { child_exited: true },
            }
        }

        ChannelResult::Continue
    }

    /// Returns a `bool` indicating whether or not the event loop should continue running.
    #[inline]
    fn channel_event(&mut self, state: &mut State) -> ChannelResult {
        self.drain_recv_channel(state)
    }

    /// Reads from the pty into the provided buffer, using the provided state
    /// information in order to properly advance the ANSI parser.
    ///
    /// If `writer` is `Some`, a copy of all bytes read will be written to that
    /// writer.
    ///
    /// Returns the number of bytes read from the PTY.
    #[inline]
    #[allow(clippy::unwrap_in_result)]
    fn pty_read(
        &mut self,
        state: &mut State,
        buf: &mut [u8],
        can_read: &mut bool,
    ) -> io::Result<()> {
        let mut bytes_in_buffer = 0;
        let mut bytes_processed = 0;

        let mut terminal = None;

        // We read up to sizeof(buf) to limit the amount of time spent
        // reading from the PTY for a given event. Currently, the buf
        // has size [`MAX_READ`].
        loop {
            match self.pty.reader().read(&mut buf[bytes_in_buffer..]) {
                Ok(0) if bytes_in_buffer == 0 => {
                    // If we get 0 here with an empty buffer (guaranteed if
                    // bytes_in_buffer == 0), it means the object is unable to
                    // receive reads.
                    *can_read = false;
                    // There is nothing to be processed in the buffer, so return
                    // to the event loop.
                    break;
                }
                // Otherwise, track how many additional bytes we read and move
                // on to byte processing.
                Ok(got) => bytes_in_buffer += got,
                Err(err) => match err.kind() {
                    ErrorKind::Interrupted | ErrorKind::WouldBlock => {
                        if err.kind() == ErrorKind::WouldBlock {
                            *can_read = false;
                        }
                        if bytes_in_buffer == 0 {
                            break;
                        }
                    }
                    _ => return Err(err),
                },
            }

            let terminal = match &mut terminal {
                Some(terminal) => terminal,
                None => terminal.insert(match self.terminal.try_lock() {
                    // If we've filled up the buffer, block on locking the terminal.
                    None if bytes_in_buffer >= READ_BUFFER_SIZE => self.terminal.lock(),
                    // Otherwise, if we failed to acquire the lock, try to read more
                    // data into the buffer.
                    None => continue,
                    // Finally, if we acquired the lock, make use of it.
                    Some(terminal) => terminal,
                }),
            };

            // A ZMODEM transfer owns the stream: protocol bytes must never
            // reach the ANSI parser, or they render as garbage.
            //
            // This runs only once the terminal lock is held, because the
            // `continue` above retries with the same `bytes_in_buffer`; routing
            // before it would feed the protocol the same bytes twice.
            let renderable = route_zmodem(
                &mut self.zmodem_detector,
                &mut self.zmodem,
                &mut self.zmodem_status,
                &self.event_listener,
                &buf[..bytes_in_buffer],
                state,
            );

            // Process the bytes read into the buffer.
            let mut terminal_response_sequences = Vec::new();
            state.parser.parse_bytes(
                terminal.deref_mut(),
                &renderable,
                &mut terminal_response_sequences,
            );
            if !terminal_response_sequences.is_empty() {
                state
                    .write_list
                    .push_back(Cow::Owned(terminal_response_sequences));
            }

            bytes_processed += bytes_in_buffer;
            bytes_in_buffer = 0;

            if bytes_processed >= MAX_LOCKED_READ {
                break;
            }

            // Give up the lock to a waiting thread, if any, before reading
            // more bytes from the PTY.
            FairMutexGuard::bump(terminal);
        }

        // Queue a terminal redraw if we processed some number
        // of non-(synchronized output) bytes.
        if bytes_processed > state.parser.sync_output_buffer_len().unwrap_or(0) {
            self.event_listener.send_wakeup_event();
        }

        Ok(())
    }

    /// Starts a ZMODEM upload of `paths` to a remote `rz`.
    ///
    /// Files are read eagerly: the PTY thread must not block on disk I/O while
    /// the protocol is mid-frame, and ZMODEM needs each size up front anyway.
    fn start_zmodem_upload(&mut self, paths: Vec<std::path::PathBuf>, state: &mut State) {
        if self.zmodem.is_some() {
            log::warn!("A ZMODEM transfer is already in progress; ignoring upload request");
            return;
        }

        // Sizes are needed up front because ZMODEM advertises them in ZFILE,
        // but contents are read by the session as each file is offered.
        let mut files = Vec::new();
        for path in paths {
            let size = match std::fs::metadata(&path).map(|meta| meta.len()) {
                Ok(len) => match u32::try_from(len) {
                    Ok(size) => size,
                    Err(_) => {
                        log::warn!("Skipping ZMODEM upload file larger than 4 GiB");
                        continue;
                    }
                },
                Err(error) => {
                    log::warn!("Skipping unreadable ZMODEM upload file: {error}");
                    continue;
                }
            };
            let name = path
                .file_name()
                .map(|name| name.as_encoded_bytes().to_vec())
                .unwrap_or_default();
            files.push(UploadFile { path, name, size });
        }

        if files.is_empty() {
            self.event_listener
                .send_terminal_event(TerminalEvent::ZmodemFinished {
                    error: Some("No readable files to send".to_owned()),
                });
            return;
        }

        let mut session = match ZmodemSession::new_upload(files) {
            Ok(session) => session,
            Err(error) => {
                log::warn!("Failed to start ZMODEM upload: {error}");
                self.event_listener
                    .send_terminal_event(TerminalEvent::ZmodemFinished {
                        error: Some(error.to_string()),
                    });
                return;
            }
        };

        // `rz` waits for the sender, so the handshake has to go out first.
        match session.begin_upload() {
            Ok(bytes) if !bytes.is_empty() => state.write_list.push_back(Cow::Owned(bytes)),
            Ok(_) => {}
            Err(error) => {
                log::warn!("Failed to begin ZMODEM upload handshake: {error}");
                return;
            }
        }

        self.zmodem = Some(session);
        self.zmodem_detector.reset();
        self.zmodem_status = ZmodemStatus::default();
    }

    #[inline]
    fn pty_write(&mut self, state: &mut State, can_write: &mut bool) -> io::Result<()> {
        state.ensure_next();

        'write_many: while let Some(mut current) = state.take_current() {
            'write_one: loop {
                match self.pty.writer().write(current.remaining_bytes()) {
                    Ok(0) => {
                        state.set_current(Some(current));
                        // We never attempt to write an empty buffer, so if we
                        // get 0 here, it means the object is unable to receive
                        // writes.
                        *can_write = false;
                        break 'write_many;
                    }
                    Ok(n) => {
                        current.advance(n);
                        if current.finished() {
                            state.goto_next();
                            break 'write_one;
                        }
                    }
                    Err(err) => {
                        state.set_current(Some(current));
                        match err.kind() {
                            ErrorKind::Interrupted | ErrorKind::WouldBlock => {
                                if err.kind() == ErrorKind::WouldBlock {
                                    *can_write = false;
                                }
                                break 'write_many;
                            }
                            _ => return Err(err),
                        }
                    }
                }
            }
        }

        Ok(())
    }

    pub fn spawn(mut self) -> JoinHandle<()> {
        #[cfg(test)]
        let feature_flag_overrides = warp_core::features::get_overrides();

        thread::Builder::new()
            .name("PTY reader".into())
            .spawn(move || {
                // Make sure any overridden feature flags are also overridden
                // in the PTY reader thread.
                #[cfg(test)]
                warp_core::features::set_overrides(feature_flag_overrides);

                let mut state = State::default();
                let mut buf = [0u8; READ_BUFFER_SIZE];

                // Keep track of whether we've "drained" read and write
                // readiness.  Once we receive a read or write readiness event,
                // we won't receive another until the operation would block.
                // These let us know whether we should keep processing reads
                // and writes, even without receiving a new readiness event.
                let mut can_read = false;
                let mut can_write = false;

                self.poll
                    .registry()
                    .register(&mut self.rx, CHANNEL_TOKEN, Interest::READABLE)
                    .unwrap();

                // Register TTY through EventedRW interface.
                self.pty
                    .register(&self.poll, Interest::READABLE | Interest::WRITABLE)
                    .unwrap();

                let mut events = Events::with_capacity(1024);

                // True if the child exiting caused the event loop to wind down
                // (e.g. CTRL D or `exit`) rather than the inverse.
                let mut child_exited = false;

                'event_loop: loop {
                    // Clear the events so that we can reliably equate the absence of events
                    // to the timeout being fired.
                    events.clear();

                    // Wait for events, but only up to the remaining timeout for the synchronous output
                    // update (if any).
                    let sync_state_timeout = state.parser.sync_output_remaining_timeout();
                    if let Err(err) = self.poll.poll(&mut events, sync_state_timeout) {
                        match err.kind() {
                            ErrorKind::Interrupted => continue,
                            _ => panic!("EventLoop polling error: {err:?}"),
                        }
                    }

                    // If there were no events but `poll` returned, that means we hit the timeout.
                    if events.is_empty() {
                        let mut terminal_response_sequences = Vec::new();
                        state.parser.finish_sync_output(
                            &mut *self.terminal.lock(),
                            &mut terminal_response_sequences,
                        );
                        if !terminal_response_sequences.is_empty() {
                            state
                                .write_list
                                .push_back(Cow::Owned(terminal_response_sequences));
                        }
                    }

                    for event in events.iter() {
                        match event.token() {
                            token if token == CHANNEL_TOKEN => {
                                match self.channel_event(&mut state) {
                                    ChannelResult::Continue => {}
                                    ChannelResult::TerminateLoop {
                                        child_exited: exited,
                                    } => {
                                        if exited {
                                            self.terminal
                                                .lock()
                                                .exit(ExitReason::ShellProcessExited);
                                            child_exited = true;
                                            self.event_listener.send_wakeup_event();
                                        }
                                        break 'event_loop;
                                    }
                                }
                            }

                            token if token == self.pty.child_event_token() => {
                                if let Some(local_tty::ChildEvent::Exited) =
                                    self.pty.next_child_event()
                                {
                                    self.terminal.lock().exit(ExitReason::ShellProcessExited);
                                    child_exited = true;
                                    self.event_listener.send_wakeup_event();
                                    break 'event_loop;
                                }
                            }

                            token
                                if token == self.pty.read_token()
                                    || token == self.pty.write_token() =>
                            {
                                #[cfg(unix)]
                                if event.is_read_closed() || event.is_write_closed() {
                                    // Don't try to do I/O on a dead PTY.
                                    continue;
                                }

                                if event.is_readable() {
                                    can_read = true;
                                }
                                if event.is_writable() {
                                    can_write = true;
                                }
                            }
                            _ => (),
                        }
                    }

                    // As long as we have work to do, do it.  Once we need to
                    // wait on some readiness (pty readability, pty writability,
                    // or new data to write), go back to the start of the event
                    // loop.
                    while can_read || (state.needs_write() && can_write) {
                        if can_read {
                            match self.pty_read(&mut state, &mut buf, &mut can_read) {
                                Ok(_) => {}
                                Err(err) => {
                                    // On Linux, a `read` on the master side of a PTY can fail
                                    // with `EIO` if the client side hangs up.  In that case,
                                    // just loop back round for the inevitable `Exited` event.
                                    // This sucks, but checking the process is either racy or
                                    // blocking.
                                    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
                                    if err.kind() == ErrorKind::Other {
                                        continue;
                                    }

                                    error!("Error reading from PTY in event loop: {err}");
                                    break 'event_loop;
                                }
                            }
                        }

                        if state.needs_write()
                            && can_write
                            && let Err(err) = self.pty_write(&mut state, &mut can_write)
                        {
                            error!("Error writing to PTY in event loop: {err}");
                            break 'event_loop;
                        }
                    }
                }

                // The evented instances are not dropped here so deregister them explicitly.
                let _ = self.poll.registry().deregister(&mut self.rx);
                let _ = self.pty.deregister(&self.poll);

                // Terminate the PTY process, if it's not the initiator of the shutdown.
                if !child_exited {
                    let res = self.pty.kill();
                    if let Err(err) = res {
                        log::warn!("Failed to kill PTY process: {err:#}");
                    }
                }
                // Notify the terminal model that the PTY process has exited.
                self.terminal.lock().exit(ExitReason::PtyDisconnected);
            })
            .expect("thread spawn works")
    }
}
