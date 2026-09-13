use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::path::{Path, PathBuf};
use std::process::{Child, ExitStatus, Stdio};
use std::thread;
use std::time::Duration;

use command::blocking::Command;
use instant::Instant;
use nix::sys::termios::{SetArg, cfmakeraw, tcgetattr, tcsetattr};

pub const POLL_PAUSE: Duration = Duration::from_millis(1);
pub const READY: &[u8] = b"ZMODEM_PEER_READY";
pub const RECOVER_COMMAND: &[u8] = b"zmodem-recovery-7c91\n";
pub const RECOVERED: &[u8] = b"ZMODEM_SHELL_RECOVERED_7c91";
pub const EXIT_COMMAND: &[u8] = b"zmodem-exit\n";

// Arguments remain positional shell parameters, including Unicode and spaces in file names.
//
// `sz`/`rz` speak the protocol on their stdout, so the tool keeps the PTY as its stdout.
// The retry banner and progress text belong to stderr, which the harness captures separately.
//
// The peer reads commands from the PTY, as a real interactive shell session would.
const PEER_SCRIPT: &str = r#"
IFS= read -r start || exit 120
[ "$start" = zmodem-start ] || exit 121
"$@"
status=$?
printf '\nZMODEM_PEER_STATUS:%s\nZMODEM_PEER_READY\n' "$status"
while IFS= read -r token; do
    case "$token" in
        *zmodem-recovery-7c91) break ;;
    esac
done
[ -n "$token" ] || exit 122
printf '\nZMODEM_SHELL_RECOVERED_7c91\n'
IFS= read -r finish || exit 124
[ "$finish" = zmodem-exit ] || exit 125
exit "$status"
"#;

pub struct ChildGuard {
    child: Child,
    status: Option<ExitStatus>,
}

impl ChildGuard {
    pub fn spawn(command: &mut Command) -> Self {
        // SAFETY: setpgid is async-signal-safe and touches only the child. A private process
        // group lets failure cleanup terminate the shell and its lrzsz descendant together.
        unsafe {
            command.pre_exec(|| {
                if libc::setpgid(0, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        Self {
            child: command
                .spawn()
                .expect("spawn required integration-test process"),
            status: None,
        }
    }

    pub fn try_wait(&mut self) -> Option<ExitStatus> {
        if self.status.is_none() {
            self.status = self.child.try_wait().expect("poll child status");
        }
        self.status
    }

    pub fn wait_until(&mut self, deadline: Instant) -> ExitStatus {
        loop {
            if let Some(status) = self.try_wait() {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "child did not exit before deadline"
            );
            thread::sleep(POLL_PAUSE);
        }
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if self.status.is_some() {
            return;
        }
        // This path is failure cleanup only. Successful tests explicitly reap a natural exit.
        // SAFETY: the still-owned child is the leader of the group created in pre_exec.
        unsafe {
            libc::kill(-(self.child.id() as libc::pid_t), libc::SIGKILL);
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            match self.child.try_wait() {
                Ok(Some(status)) => {
                    self.status = Some(status);
                    return;
                }
                Ok(None) => thread::sleep(POLL_PAUSE),
                Err(_) => return,
            }
        }
        eprintln!("failed to reap integration-test child {}", self.child.id());
    }
}

pub fn require_tools() {
    for tool in ["sz", "rz", "shasum"] {
        let mut command = Command::new(tool);
        command
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = ChildGuard::spawn(&mut command);
        let status = child.wait_until(Instant::now() + Duration::from_secs(5));
        assert!(
            status.success(),
            "required integration tool {tool} is unavailable or broken: {status}"
        );
    }
}

pub fn raw_pty() -> (File, File) {
    let pty = nix::pty::openpty(None, None).expect("openpty");
    // SAFETY: openpty returned two distinct owned descriptors, transferred exactly once.
    let master = unsafe { File::from_raw_fd(pty.master) };
    let slave = unsafe { File::from_raw_fd(pty.slave) };
    let mut attrs = tcgetattr(slave.as_raw_fd()).expect("tcgetattr");
    cfmakeraw(&mut attrs);
    tcsetattr(slave.as_raw_fd(), SetArg::TCSANOW, &attrs).expect("tcsetattr raw");
    for fd in [master.as_raw_fd(), slave.as_raw_fd()] {
        // SAFETY: both descriptors remain owned and open throughout these fcntl calls.
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFD);
            assert!(flags >= 0, "F_GETFD: {}", io::Error::last_os_error());
            assert_eq!(libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC), 0);
        }
    }
    set_nonblocking(&master);
    (master, slave)
}

pub fn set_nonblocking(file: &File) {
    // SAFETY: the file description remains owned and open through both fcntl calls.
    unsafe {
        let flags = libc::fcntl(file.as_raw_fd(), libc::F_GETFL);
        assert!(flags >= 0, "F_GETFL: {}", io::Error::last_os_error());
        assert_eq!(
            libc::fcntl(file.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK),
            0
        );
    }
}

pub struct Peer {
    pub master: File,
    pub child: ChildGuard,
    stderr: File,
    /// Kept empty: the peer's stdout is the protocol stream, so no side copy is taken.
    pub diagnostics_path: PathBuf,
}

impl Peer {
    pub fn spawn<I, S>(directory: &Path, program: &str, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let (master, slave) = raw_pty();
        let stderr = tempfile::tempfile().expect("peer diagnostic file");
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", PEER_SCRIPT, "zmodem-peer", program])
            .args(args)
            .current_dir(directory)
            .env("LC_ALL", "C")
            .stdin(slave.try_clone().expect("clone slave stdin"))
            .stdout(slave)
            .stderr(stderr.try_clone().expect("clone diagnostic file"));
        let child = ChildGuard::spawn(&mut command);
        Self {
            master,
            child,
            stderr,
            diagnostics_path: PathBuf::new(),
        }
    }

    pub fn start(&mut self, deadline: Instant) {
        write_until(&mut self.master, b"zmodem-start\n", deadline);
    }

    pub fn diagnostics(&self) -> String {
        use std::os::unix::fs::FileExt;

        let mut bytes = [0; 4096];
        let count = self.stderr.read_at(&mut bytes, 0).unwrap_or(0);
        String::from_utf8_lossy(&bytes[..count]).into_owned()
    }

    /// Human-readable peer state for a failing test: status, stderr tail and any copy on disk.
    pub fn failure_context(&mut self) -> String {
        let status = self.child.try_wait();
        let stderr = self.diagnostics();
        let tail: String = if self.diagnostics_path.as_os_str().is_empty() {
            String::new()
        } else {
            std::fs::read_to_string(&self.diagnostics_path).unwrap_or_else(|error| {
                format!("<unreadable {}: {error}>", self.diagnostics_path.display())
            })
        };
        format!(
            "peer status={status:?} stderr={stderr:?} stdout_tail={tail:?} path={}",
            self.diagnostics_path.display()
        )
    }
}

pub fn write_some(file: &mut File, bytes: &[u8]) -> usize {
    match file.write(bytes) {
        Ok(0) => panic!("PTY returned a zero-length write"),
        Ok(count) => count,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) =>
        {
            0
        }
        Err(error) => panic!("PTY write failed: {error}"),
    }
}

/// Distinguishes a live back-pressured PTY from one whose peer is gone.
///
/// `EIO` here means the last slave side closed; the caller needs that state, not a panic,
/// so it can report the peer's exit status and diagnostics instead of only an opaque error.
pub fn write_some_or_eof(file: &mut File, bytes: &[u8]) -> WriteResult {
    match file.write(bytes) {
        Ok(0) => panic!("PTY returned a zero-length write"),
        Ok(count) => WriteResult::Bytes(count),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) =>
        {
            WriteResult::Pending
        }
        Err(error) if error.raw_os_error() == Some(libc::EIO) => WriteResult::Eof,
        Err(error) => WriteResult::Failed(error),
    }
}

pub enum WriteResult {
    Bytes(usize),
    Pending,
    Eof,
    Failed(io::Error),
}

pub fn write_until(file: &mut File, mut bytes: &[u8], deadline: Instant) {
    while !bytes.is_empty() {
        assert!(Instant::now() < deadline, "PTY write deadline exceeded");
        let count = write_some(file, bytes);
        bytes = &bytes[count..];
        if count == 0 {
            thread::sleep(POLL_PAUSE);
        }
    }
}

pub enum PtyRead {
    Bytes(usize),
    Pending,
    Eof,
}

pub fn read_some(file: &mut File, bytes: &mut [u8]) -> PtyRead {
    match file.read(bytes) {
        Ok(0) => PtyRead::Eof,
        Ok(count) => PtyRead::Bytes(count),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) =>
        {
            PtyRead::Pending
        }
        Err(error) if error.raw_os_error() == Some(libc::EIO) => PtyRead::Eof,
        Err(error) => panic!("PTY read failed: {error}"),
    }
}

pub fn contains(bytes: &[u8], needle: &[u8]) -> bool {
    bytes.windows(needle.len()).any(|window| window == needle)
}

pub fn append_terminal(bytes: &mut Vec<u8>, new: &[u8]) {
    const MAX_CAPTURE: usize = 64 * 1024;
    let excess = bytes
        .len()
        .saturating_add(new.len())
        .saturating_sub(MAX_CAPTURE);
    if excess > 0 {
        bytes.drain(..excess.min(bytes.len()));
    }
    bytes.extend_from_slice(new);
}
