use std::borrow::Cow;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use instant::Instant;
use mio::unix::SourceFd;
use mio::{Interest, Poll, Token};
use parking_lot::FairMutex;
use warp_terminal::SizeInfo;
use warp_terminal::event::Event;
use warp_terminal::event_listener::ChannelEventListener;
use warp_terminal::local_tty::event_loop::{EventLoop, PTY_TOKEN, SIGNALS_TOKEN};
use warp_terminal::local_tty::{ChildEvent, EventedPty, EventedReadWrite, mio_channel};
use warp_terminal::writeable_pty::Message;
use warp_terminal::zmodem::runtime::{
    Control, OverwritePolicy, Role, TransferEvent, TransferOutcome,
};

use super::capture::Capture;
use super::peer::{EXIT_COMMAND, POLL_PAUSE, Peer, READY, RECOVER_COMMAND, RECOVERED, contains};
use super::worker::Report;

struct TestPty {
    file: File,
    fragment: usize,
}

impl Read for TestPty {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let count = bytes.len().min(self.fragment);
        self.file.read(&mut bytes[..count])
    }
}

impl Write for TestPty {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.file.write(&bytes[..bytes.len().min(self.fragment)])
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl EventedReadWrite for TestPty {
    type Reader = Self;
    type Writer = Self;

    fn register(&mut self, poll: &Poll, interest: Interest) -> io::Result<()> {
        poll.registry()
            .register(&mut SourceFd(&self.file.as_raw_fd()), PTY_TOKEN, interest)
    }

    fn reregister(&mut self, poll: &Poll, interest: Interest) -> io::Result<()> {
        poll.registry()
            .reregister(&mut SourceFd(&self.file.as_raw_fd()), PTY_TOKEN, interest)
    }

    fn deregister(&mut self, poll: &Poll) -> io::Result<()> {
        poll.registry()
            .deregister(&mut SourceFd(&self.file.as_raw_fd()))
    }

    fn reader(&mut self) -> &mut Self::Reader {
        self
    }
    fn writer(&mut self) -> &mut Self::Writer {
        self
    }
    fn read_token(&self) -> Token {
        PTY_TOKEN
    }
    fn write_token(&self) -> Token {
        PTY_TOKEN
    }
}

impl EventedPty for TestPty {
    fn child_event_token(&self) -> Token {
        SIGNALS_TOKEN
    }
    fn next_child_event(&mut self) -> Option<ChildEvent> {
        None
    }
    fn on_resize(&mut self, _: &SizeInfo) {}

    fn kill(self) -> anyhow::Result<()> {
        // The owning test reaps the peer. Shutdown must not conceal a stuck protocol by killing it.
        Ok(())
    }
}

pub struct Router {
    sender: mio_channel::Sender<Message>,
    thread: Option<JoinHandle<()>>,
    terminal: Arc<FairMutex<Capture>>,
    events: async_channel::Receiver<Event>,
    wakeups: async_channel::Receiver<()>,
}

impl Router {
    pub fn start(master: File, fragment: usize, enabled: bool) -> Self {
        assert!(fragment > 0);
        let terminal = Arc::new(FairMutex::new(Capture::default()));
        let (sender, receiver) = mio_channel::channel();
        let (events_tx, events) = async_channel::bounded(1024);
        let (wakeups_tx, wakeups) = async_channel::bounded(1);
        let (reads_tx, reads_rx) = async_broadcast::broadcast(1);
        drop(reads_rx);
        let listener = ChannelEventListener::new(wakeups_tx, events_tx, reads_tx);
        sender
            .send(Message::Zmodem(Control::Enable(enabled)))
            .expect("enable message");
        let thread = EventLoop::new(
            Arc::clone(&terminal),
            listener,
            TestPty {
                file: master,
                fragment,
            },
            receiver,
        )
        .spawn();
        Self {
            sender,
            thread: Some(thread),
            terminal,
            events,
            wakeups,
        }
    }

    pub fn send(&self, message: Message) {
        self.sender
            .send(message)
            .expect("live production event loop");
    }

    pub fn input(&self, bytes: &[u8]) {
        self.send(Message::Input(Cow::Owned(bytes.to_vec())));
    }

    pub fn rendered(&self) -> Option<Vec<u8>> {
        self.terminal
            .try_lock()
            .map(|terminal| terminal.bytes.clone())
    }

    pub fn next_event(&self) -> Option<Event> {
        while self.wakeups.try_recv().is_ok() {}
        self.events.try_recv().ok()
    }

    pub fn stop(&mut self, child_exited: bool, deadline: Instant) {
        let message = if child_exited {
            Message::ChildExited
        } else {
            Message::Shutdown
        };
        let _ = self.sender.send(message);
        let Some(handle) = self.thread.as_ref() else {
            return;
        };
        while !handle.is_finished() {
            assert!(
                Instant::now() < deadline,
                "production event loop did not stop"
            );
            thread::sleep(POLL_PAUSE);
        }
        self.thread
            .take()
            .expect("event loop thread")
            .join()
            .expect("event loop panicked");
    }
}

impl Drop for Router {
    fn drop(&mut self) {
        let _ = self.sender.send(Message::Shutdown);
        if let Some(handle) = self.thread.take() {
            let deadline = Instant::now() + Duration::from_secs(2);
            while !handle.is_finished() && Instant::now() < deadline {
                thread::sleep(POLL_PAUSE);
            }
            if handle.is_finished() {
                let _ = handle.join();
            } else {
                eprintln!("production event loop failed to stop during failure cleanup");
            }
        }
    }
}

pub fn transfer(paths: &[PathBuf], destination: &Path, role: Role, fragment: usize) -> Report {
    let mut peer = match role {
        Role::Download => {
            let mut args = vec![PathBuf::from("--zmodem"), PathBuf::from("--binary")];
            args.extend_from_slice(paths);
            Peer::spawn(destination, "sz", args)
        }
        Role::Upload => Peer::spawn(destination, "rz", ["--binary"]),
    };
    run(
        paths,
        destination,
        role,
        fragment,
        Duration::from_secs(45),
        &mut peer,
    )
}

pub fn upload_over_ssh(paths: &[PathBuf], host: &str, remote_command: &str) -> Report {
    let remote_command_option = format!("RemoteCommand={remote_command}");
    let mut peer = Peer::spawn(
        Path::new("."),
        "ssh",
        [
            "-o",
            "BatchMode=yes",
            "-o",
            remote_command_option.as_str(),
            host,
        ],
    );
    run(
        paths,
        Path::new("."),
        Role::Upload,
        16 * 1024,
        Duration::from_secs(240),
        &mut peer,
    )
}

fn run(
    paths: &[PathBuf],
    destination: &Path,
    role: Role,
    fragment: usize,
    timeout: Duration,
    peer: &mut Peer,
) -> Report {
    let mut router = Router::start(
        peer.master.try_clone().expect("clone master"),
        fragment,
        true,
    );
    // The peer remains gated until Enable has been queued ahead of this input message.
    router.input(b"zmodem-start\n");
    let started = Instant::now();
    let deadline = started + timeout;
    let mut report = Report::default();
    let mut transfer_id = None;
    let mut recovery_sent = false;
    let mut exit_sent = false;
    loop {
        assert!(
            Instant::now() < deadline,
            "production routing deadline: {report:?}: {}",
            peer.diagnostics()
        );
        while let Some(event) = router.next_event() {
            if let Event::Zmodem(event) = event {
                if let TransferEvent::Requested {
                    id,
                    role: requested_role,
                } = &event
                {
                    assert_eq!(*requested_role, role);
                    assert!(
                        transfer_id.replace(*id).is_none(),
                        "duplicate picker request"
                    );
                    let control = match role {
                        Role::Upload => Control::Upload {
                            id: *id,
                            paths: paths.to_vec(),
                        },
                        Role::Download => Control::Download {
                            id: *id,
                            directory: destination.to_owned(),
                            policy: OverwritePolicy::Skip,
                        },
                    };
                    router.send(Message::Zmodem(control));
                }
                report.event(event, transfer_id.expect("event before Requested"), role);
            }
        }
        if let Some(bytes) = router.rendered() {
            report.terminal = bytes;
        }
        if report.finished.is_some() && contains(&report.terminal, READY) && !recovery_sent {
            assert_eq!(report.finished, Some(TransferOutcome::Completed));
            router.input(RECOVER_COMMAND);
            recovery_sent = true;
        }
        if contains(&report.terminal, RECOVERED) && !exit_sent {
            router.input(EXIT_COMMAND);
            exit_sent = true;
        }
        if let Some(status) = peer.child.try_wait() {
            assert!(
                status.success(),
                "real peer failed: {status}: {report:?}: {}",
                peer.diagnostics()
            );
            assert!(
                recovery_sent && exit_sent,
                "shell recovery was not exercised"
            );
            assert_eq!(report.finished, Some(TransferOutcome::Completed));
            router.stop(true, deadline);
            assert!(
                !contains(&report.terminal, b"**\x18B"),
                "protocol leaked into ANSI parser"
            );
            report.elapsed = started.elapsed();
            return report;
        }
        thread::sleep(POLL_PAUSE);
    }
}
