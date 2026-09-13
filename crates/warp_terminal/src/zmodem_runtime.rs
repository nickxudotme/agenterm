//! Bounded native ZMODEM transfers. Only the worker touches business files.

use std::collections::VecDeque;
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver as ChannelReceiver, SyncSender, TryRecvError, TrySendError};
use std::thread;
use std::time::{Duration, SystemTime};

use instant::Instant;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use zmodem2::{Action, Event, FileInfo, Position, Receiver, Sender};

pub type TransferId = u64;

static NEXT_TRANSFER_ID: AtomicU64 = AtomicU64::new(1);

pub fn next_transfer_id() -> TransferId {
    NEXT_TRANSFER_ID.fetch_add(1, Ordering::Relaxed)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Role {
    Upload,
    Download,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum OverwritePolicy {
    #[default]
    Skip,
    Rename,
    Overwrite,
}

#[derive(Clone, Debug)]
pub enum Control {
    Enable(bool),
    StartUpload {
        id: TransferId,
        paths: Vec<PathBuf>,
    },
    Upload {
        id: TransferId,
        paths: Vec<PathBuf>,
    },
    Download {
        id: TransferId,
        directory: PathBuf,
        policy: OverwritePolicy,
    },
    Cancel {
        id: TransferId,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ErrorKind {
    Configuration,
    UnsafeName,
    SourceChanged,
    FileTooLarge,
    Storage,
    Protocol,
    Timeout,
    WorkerLimit,
    WorkerStopped,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransferError {
    pub kind: ErrorKind,
    pub message: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FileOutcome {
    Completed,
    Skipped,
    Failed(TransferError),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransferOutcome {
    Completed,
    Cancelled,
    RemoteCancelled,
    Failed(TransferError),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransferEvent {
    Requested {
        id: TransferId,
        role: Role,
    },
    FileStarted {
        id: TransferId,
        role: Role,
        name: String,
        size: Option<u64>,
    },
    Progress {
        id: TransferId,
        role: Role,
        name: String,
        bytes: u64,
        total: Option<u64>,
    },
    FileResult {
        id: TransferId,
        role: Role,
        name: String,
        bytes: u64,
        outcome: FileOutcome,
        path: Option<PathBuf>,
    },
    Finished {
        id: TransferId,
        role: Role,
        outcome: TransferOutcome,
        committed_paths: Vec<PathBuf>,
    },
}

pub type RuntimeEvent = TransferEvent;

/// Wire buffers retain credit until explicitly acknowledged after the final PTY write.
#[derive(Debug)]
pub enum RuntimeOutput {
    Event(TransferEvent),
    Wire {
        id: TransferId,
        sequence: u64,
        bytes: Vec<u8>,
    },
    /// Bytes after a successful ZFIN/OO exchange, in original stream order.
    Tail {
        id: TransferId,
        bytes: Vec<u8>,
    },
    /// Discard pending transfer writes and send [`abort_sequence`].
    Abort {
        id: TransferId,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubmitError {
    Full,
    Closed,
}

pub const MAX_INPUT_CHUNK: usize = 16 * 1024;
pub const MAX_FILES: usize = 128;
pub const MAX_PATH_BYTES: usize = 64 * 1024;
pub const MAX_WORKERS: usize = 8;

/// Ten CAN bytes followed by ten backspaces, as used by lrzsz.
pub fn abort_sequence() -> Vec<u8> {
    [vec![0x18; 10], vec![0x08; 10]].concat()
}

const INPUT_SLOTS: usize = 8;
const OUTPUT_SLOTS: usize = 32;
/// How many unacknowledged wire buffers may be queued before the worker waits for the PTY.
///
/// Kept below [`OUTPUT_SLOTS`] so the in-flight window cannot exceed the output queue depth.
const WIRE_WINDOW: u64 = 8;
// Payload accounting includes queued input, a worker input block, the output queue, retained
// write credit, configuration paths, committed paths and the terminal paths event. Native codec
// buffers and bounded names leave this below 1 MiB even while the PTY retains a wire block.
pub const MAX_QUEUE_BYTES: usize = 1024 * 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(20);
const CHOICE_TIMEOUT: Duration = Duration::from_secs(120);
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(30);
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);
const CANCELLED: u8 = 1;
const COMMITTING: u8 = 2;
static WORKERS: AtomicUsize = AtomicUsize::new(0);

enum Configuration {
    Upload(Vec<PathBuf>),
    Download(PathBuf, OverwritePolicy),
}

#[derive(Default)]
struct Ingress {
    chunks: VecDeque<Vec<u8>>,
    closed: bool,
    choice_cans: u8,
    remote_cancelled: bool,
}

struct Shared {
    ingress: Mutex<Ingress>,
    phase: AtomicU8,
    acknowledged: AtomicU64,
    configured: AtomicBool,
    wake: SyncSender<()>,
}

/// Dropping a handle cancels without joining the worker or waiting for disk I/O.
pub struct TransferHandle {
    id: TransferId,
    role: Role,
    shared: Arc<Shared>,
    outputs: ChannelReceiver<RuntimeOutput>,
    configuration: SyncSender<Configuration>,
    outstanding_write: Option<u64>,
}

impl TransferHandle {
    /// Starts an unconfigured worker. The PTY owns emitting `Requested` when a picker is needed.
    pub fn spawn(id: TransferId, role: Role) -> Result<Self, TransferError> {
        let slot = WorkerSlot::acquire()?;
        let (wake, wake_receiver) = mpsc::sync_channel(1);
        let (output, outputs) = mpsc::sync_channel(OUTPUT_SLOTS);
        let (configuration, configuration_receiver) = mpsc::sync_channel(1);
        let shared = Arc::new(Shared {
            ingress: Mutex::new(Ingress::default()),
            phase: AtomicU8::new(0),
            acknowledged: AtomicU64::new(0),
            configured: AtomicBool::new(false),
            wake,
        });
        let worker_shared = Arc::clone(&shared);
        thread::Builder::new()
            .name(format!("zmodem-{id}"))
            .spawn(move || {
                let _slot = slot;
                Worker::new(
                    id,
                    role,
                    worker_shared,
                    output,
                    wake_receiver,
                    configuration_receiver,
                )
                .run();
            })
            .map_err(|_| error(ErrorKind::WorkerStopped, "Cannot start transfer worker"))?;
        Ok(Self {
            id,
            role,
            shared,
            outputs,
            configuration,
            outstanding_write: None,
        })
    }

    pub fn id(&self) -> TransferId {
        self.id
    }

    pub fn role(&self) -> Role {
        self.role
    }

    pub fn configure_upload(&self, paths: Vec<PathBuf>) -> Result<(), TransferError> {
        if self.role != Role::Upload || paths.is_empty() || paths.len() > MAX_FILES {
            return Err(error(ErrorKind::Configuration, "Invalid upload selection"));
        }
        validate_path_budget(paths.iter().map(PathBuf::as_path))?;
        self.configure(Configuration::Upload(paths))
    }

    pub fn configure_download(
        &self,
        directory: PathBuf,
        policy: OverwritePolicy,
    ) -> Result<(), TransferError> {
        if self.role != Role::Download {
            return Err(error(ErrorKind::Configuration, "Not a download transfer"));
        }
        validate_path_budget(std::iter::once(directory.as_path()))?;
        self.configure(Configuration::Download(directory, policy))
    }

    fn configure(&self, configuration: Configuration) -> Result<(), TransferError> {
        if self.is_cancelled() || self.shared.configured.swap(true, Ordering::AcqRel) {
            return Err(error(
                ErrorKind::Configuration,
                "Transfer no longer accepts configuration",
            ));
        }
        self.configuration.try_send(configuration).map_err(|_| {
            error(
                ErrorKind::WorkerStopped,
                "Transfer configuration receiver closed",
            )
        })?;
        let _ = self.shared.wake.try_send(());
        Ok(())
    }

    /// Accepts at most `MAX_INPUT_CHUNK` bytes. Retain and retry the unaccepted suffix.
    /// `Full` also indicates transient ingress contention; no bytes were accepted in either case.
    pub fn try_submit(&self, bytes: &[u8]) -> Result<usize, SubmitError> {
        let Some(mut ingress) = self.shared.ingress.try_lock() else {
            return Err(SubmitError::Full);
        };
        if ingress.closed || self.is_cancelled() {
            return Err(SubmitError::Closed);
        }
        if bytes.is_empty() {
            return Ok(0);
        }
        if ingress.chunks.len() == INPUT_SLOTS {
            return Err(SubmitError::Full);
        }
        let count = bytes.len().min(MAX_INPUT_CHUNK);
        if !self.shared.configured.load(Ordering::Acquire) {
            for byte in &bytes[..count] {
                ingress.choice_cans = if *byte == 0x18 {
                    ingress.choice_cans.saturating_add(1)
                } else {
                    0
                };
                ingress.remote_cancelled |= ingress.choice_cans >= 5;
            }
        }
        ingress.chunks.push_back(bytes[..count].to_vec());
        drop(ingress);
        let _ = self.shared.wake.try_send(());
        Ok(count)
    }

    pub fn try_recv(&mut self) -> Result<RuntimeOutput, TryRecvError> {
        loop {
            let output = self.outputs.try_recv()?;
            if let RuntimeOutput::Wire { sequence, .. } = &output {
                if self.is_cancelled() {
                    continue;
                }
                self.outstanding_write = Some(*sequence);
            }
            return Ok(output);
        }
    }

    /// Call once the complete corresponding wire buffer has reached the PTY, not on dequeue.
    pub fn acknowledge_write(&mut self, sequence: u64) -> bool {
        if self.outstanding_write != Some(sequence) {
            return false;
        }
        self.outstanding_write = None;
        self.shared.acknowledged.store(sequence, Ordering::Release);
        let _ = self.shared.wake.try_send(());
        true
    }

    /// Atomic and independent of queue capacity and publication I/O.
    pub fn cancel(&self) {
        self.shared.phase.fetch_or(CANCELLED, Ordering::AcqRel);
        let _ = self.shared.wake.try_send(());
    }

    pub fn is_cancelled(&self) -> bool {
        self.shared.phase.load(Ordering::Acquire) & CANCELLED != 0
    }
}

impl Drop for TransferHandle {
    fn drop(&mut self) {
        self.cancel();
    }
}

struct WorkerSlot;

impl WorkerSlot {
    fn acquire() -> Result<Self, TransferError> {
        WORKERS
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count < MAX_WORKERS).then_some(count + 1)
            })
            .map(|_| Self)
            .map_err(|_| {
                error(
                    ErrorKind::WorkerLimit,
                    "All eight transfer workers are occupied",
                )
            })
    }
}

impl Drop for WorkerSlot {
    fn drop(&mut self) {
        WORKERS.fetch_sub(1, Ordering::AcqRel);
    }
}

impl std::fmt::Display for TransferError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for TransferError {}

fn error(kind: ErrorKind, message: &str) -> TransferError {
    TransferError {
        kind,
        message: message.to_owned(),
    }
}

fn validate_path_budget<'a>(paths: impl Iterator<Item = &'a Path>) -> Result<(), TransferError> {
    let mut bytes = 0usize;
    for path in paths {
        bytes = bytes.saturating_add(path.as_os_str().len());
        if bytes > MAX_PATH_BYTES || path.as_os_str().len() > 4096 {
            return Err(error(
                ErrorKind::Configuration,
                "Selected paths exceed metadata budget",
            ));
        }
    }
    Ok(())
}

enum Protocol {
    Upload(Box<Sender>),
    Download(Box<Receiver>),
}

impl Protocol {
    fn new(role: Role) -> Result<Self, TransferError> {
        match role {
            Role::Upload => {
                let mut sender = Sender::new().map_err(protocol_error)?;
                sender.set_streaming_window(usize::MAX);
                Ok(Self::Upload(Box::new(sender)))
            }
            Role::Download => {
                let mut receiver = Receiver::with_flow_control(0, true).map_err(protocol_error)?;
                receiver.set_manual_file_accept(true);
                Ok(Self::Download(Box::new(receiver)))
            }
        }
    }

    fn poll(&mut self) -> Action<'_> {
        match self {
            Self::Upload(sender) => sender.poll(),
            Self::Download(receiver) => receiver.poll(),
        }
    }

    fn submit(&mut self, bytes: &[u8]) -> Result<usize, TransferError> {
        match self {
            Self::Upload(sender) => sender.submit_wire(bytes),
            Self::Download(receiver) => receiver.submit_wire(bytes),
        }
        .map_err(protocol_error)
    }

    fn wire_written(&mut self, count: usize) {
        match self {
            Self::Upload(sender) => sender.wire_written(count),
            Self::Download(receiver) => receiver.wire_written(count),
        }
    }

    fn timeout(&mut self) -> Result<(), TransferError> {
        match self {
            Self::Upload(sender) => sender.timeout(),
            Self::Download(receiver) => receiver.timeout(),
        }
        .map_err(protocol_error)
    }
}

fn protocol_error(failure: zmodem2::Error) -> TransferError {
    let kind = match failure {
        zmodem2::Error::MalformedFileSize => ErrorKind::FileTooLarge,
        _ => ErrorKind::Protocol,
    };
    // Native errors may contain an untrusted byte value, never echo wire fragments.
    error(kind, "Native ZMODEM protocol rejected the transfer")
}

fn storage_error(failure: std::io::Error) -> TransferError {
    error(
        ErrorKind::Storage,
        &format!("Local file operation failed ({:?})", failure.kind()),
    )
}

#[derive(Debug)]
enum Stop {
    Cancelled,
    RemoteCancelled,
    Failed(TransferError),
}

impl From<TransferError> for Stop {
    fn from(failure: TransferError) -> Self {
        Self::Failed(failure)
    }
}

struct Worker {
    id: TransferId,
    role: Role,
    shared: Arc<Shared>,
    output: SyncSender<RuntimeOutput>,
    wake: ChannelReceiver<()>,
    configuration: ChannelReceiver<Configuration>,
    sequence: u64,
    deadline: Instant,
    last_progress: Instant,
    current: Option<CurrentFile>,
    committed_paths: Vec<PathBuf>,
    committed_path_bytes: usize,
}

impl Worker {
    fn new(
        id: TransferId,
        role: Role,
        shared: Arc<Shared>,
        output: SyncSender<RuntimeOutput>,
        wake: ChannelReceiver<()>,
        configuration: ChannelReceiver<Configuration>,
    ) -> Self {
        let now = Instant::now();
        Self {
            id,
            role,
            shared,
            output,
            wake,
            configuration,
            sequence: 0,
            deadline: now + CHOICE_TIMEOUT,
            last_progress: now,
            current: None,
            committed_paths: Vec::new(),
            committed_path_bytes: 0,
        }
    }

    fn run(mut self) {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.transfer()))
            .unwrap_or_else(|_| {
                Err(error(ErrorKind::WorkerStopped, "Transfer worker panicked").into())
            });
        let outcome = match result {
            Ok(()) => TransferOutcome::Completed,
            Err(Stop::Cancelled) => TransferOutcome::Cancelled,
            Err(Stop::RemoteCancelled) => TransferOutcome::RemoteCancelled,
            Err(Stop::Failed(failure)) => TransferOutcome::Failed(failure),
        };
        self.shared.ingress.lock().closed = true;
        if outcome != TransferOutcome::Completed {
            self.shared.phase.fetch_or(CANCELLED, Ordering::AcqRel);
            let _ = self.emit(RuntimeOutput::Abort { id: self.id }, true);
            if let Some(file) = self.current.take() {
                let failure = match &outcome {
                    TransferOutcome::Failed(failure) => failure.clone(),
                    TransferOutcome::Cancelled | TransferOutcome::RemoteCancelled => {
                        error(ErrorKind::Protocol, "File transfer cancelled")
                    }
                    TransferOutcome::Completed => unreachable!(),
                };
                let _ = self.file_result(&file, FileOutcome::Failed(failure), None, true);
            }
        }
        let event = TransferEvent::Finished {
            id: self.id,
            role: self.role,
            outcome,
            committed_paths: std::mem::take(&mut self.committed_paths),
        };
        let _ = self.emit(RuntimeOutput::Event(event), true);
    }

    fn check(&self) -> Result<(), Stop> {
        if self.shared.phase.load(Ordering::Acquire) & CANCELLED != 0 {
            return Err(Stop::Cancelled);
        }
        if Instant::now() >= self.deadline {
            return Err(error(
                ErrorKind::Timeout,
                "No effective transfer progress before deadline",
            )
            .into());
        }
        Ok(())
    }

    fn wait(&self) {
        let _ = self.wake.recv_timeout(POLL_INTERVAL);
    }

    fn emit(&self, mut output: RuntimeOutput, terminal: bool) -> Result<(), Stop> {
        let drain_deadline = Instant::now() + Duration::from_secs(1);
        loop {
            if terminal {
                if Instant::now() >= drain_deadline {
                    return Err(
                        error(ErrorKind::WorkerStopped, "Transfer event consumer stalled").into(),
                    );
                }
            } else {
                self.check()?;
            }
            match self.output.try_send(output) {
                Ok(()) => return Ok(()),
                Err(TrySendError::Full(returned)) => output = returned,
                Err(TrySendError::Disconnected(_)) => {
                    return Err(
                        error(ErrorKind::WorkerStopped, "Transfer event consumer closed").into(),
                    );
                }
            }
            self.wait();
        }
    }

    fn event(&self, event: TransferEvent) -> Result<(), Stop> {
        self.emit(RuntimeOutput::Event(event), false)
    }

    /// Queues one wire buffer, allowing a bounded number to be in flight before waiting.
    ///
    /// Waiting for every buffer serialises the transfer on a round trip through the PTY, which
    /// caps throughput near one buffer per poll. The window stays below the output queue depth so
    /// memory remains bounded.
    fn write_wire(&mut self, bytes: Vec<u8>) -> Result<(), Stop> {
        self.sequence += 1;
        self.emit(
            RuntimeOutput::Wire {
                id: self.id,
                sequence: self.sequence,
                bytes,
            },
            false,
        )?;
        let acknowledged = self.shared.acknowledged.load(Ordering::Acquire);
        if self.sequence - acknowledged >= WIRE_WINDOW {
            while self.shared.acknowledged.load(Ordering::Acquire) + WIRE_WINDOW <= self.sequence {
                self.check()?;
                self.wait();
            }
        }
        Ok(())
    }

    fn configure(&self) -> Result<Configuration, Stop> {
        loop {
            self.check()?;
            let ingress = self.shared.ingress.lock();
            if ingress.remote_cancelled {
                return Err(Stop::RemoteCancelled);
            }
            drop(ingress);
            match self.configuration.try_recv() {
                Ok(configuration) => return Ok(configuration),
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => return Err(Stop::Cancelled),
            }
            self.wait();
        }
    }

    fn transfer(&mut self) -> Result<(), Stop> {
        let configuration = self.configure()?;
        let mut uploads = VecDeque::new();
        let mut downloads = None;
        match configuration {
            Configuration::Upload(paths) => {
                // Snapshot every selected source before starting the batch, without retaining handles.
                for path in paths {
                    self.check()?;
                    uploads.push_back(UploadSource::inspect(path)?);
                }
            }
            Configuration::Download(path, policy) => {
                downloads = Some(DownloadDirectory::open(path, policy)?);
            }
        }
        self.deadline = Instant::now() + TRANSFER_TIMEOUT;
        let mut protocol = Protocol::new(self.role)?;
        let mut input = Vec::new();
        let mut offset = 0;
        let mut cans = 0u8;
        let mut complete = false;
        let mut finishing = false;
        let mut files_seen = 0usize;
        let mut retry_at = Instant::now() + Duration::from_secs(5);
        loop {
            self.check()?;
            match protocol.poll() {
                Action::WriteWire(bytes) => {
                    if bytes.len() > MAX_INPUT_CHUNK {
                        return Err(error(
                            ErrorKind::Protocol,
                            "Native wire buffer exceeded budget",
                        )
                        .into());
                    }
                    let bytes = bytes.to_vec();
                    let count = bytes.len();
                    self.write_wire(bytes)?;
                    protocol.wire_written(count);
                }
                Action::ReadFile { offset, max_len } => {
                    let bytes = {
                        let Some(file) = self.current.as_mut() else {
                            return Err(error(
                                ErrorKind::Protocol,
                                "File read without an active upload",
                            )
                            .into());
                        };
                        let FileStorage::Upload(source) = &mut file.storage else {
                            return Err(error(
                                ErrorKind::Protocol,
                                "Read requested during download",
                            )
                            .into());
                        };
                        source.read(offset.get(), max_len)?
                    };
                    self.check()?;
                    if let Protocol::Upload(sender) = &mut protocol {
                        sender.submit_file(&bytes).map_err(protocol_error)?;
                    }
                    if let Some(file) = self.current.as_mut() {
                        file.bytes = u64::from(offset.get()) + bytes.len() as u64;
                    }
                    self.deadline = Instant::now() + TRANSFER_TIMEOUT;
                    self.progress(false)?;
                }
                Action::WriteFile(bytes) => {
                    let count = bytes.len();
                    let Some(file) = self.current.as_mut() else {
                        return Err(
                            error(ErrorKind::Protocol, "File data without acceptance").into()
                        );
                    };
                    let FileStorage::Download(target) = &mut file.storage else {
                        return Err(
                            error(ErrorKind::Protocol, "Write requested during upload").into()
                        );
                    };
                    if file
                        .size
                        .is_some_and(|size| file.bytes + count as u64 > size)
                    {
                        return Err(error(
                            ErrorKind::Protocol,
                            "Received data exceeds advertised size",
                        )
                        .into());
                    }
                    target
                        .file
                        .as_file_mut()
                        .write_all(bytes)
                        .map_err(storage_error)?;
                    file.bytes += count as u64;
                    self.check()?;
                    if let Protocol::Download(receiver) = &mut protocol {
                        receiver.file_written(count).map_err(protocol_error)?;
                    }
                    if count != 0 {
                        self.deadline = Instant::now() + TRANSFER_TIMEOUT;
                        self.progress(false)?;
                    }
                }
                Action::Event(Event::FileStarted(info)) => {
                    let name = safe_name(info.name)?;
                    let size = info.size.map(|size| u64::from(size.get()));
                    files_seen += 1;
                    if files_seen > MAX_FILES {
                        return Err(
                            error(ErrorKind::Configuration, "Batch exceeds 128 files").into()
                        );
                    }
                    self.deadline = Instant::now() + TRANSFER_TIMEOUT;
                    self.event(TransferEvent::FileStarted {
                        id: self.id,
                        role: self.role,
                        name: name.clone(),
                        size,
                    })?;
                    if let Some(directory) = downloads.as_ref() {
                        let path = directory.path.join(&name);
                        let budget = path.as_os_str().len() + 32;
                        if self.committed_path_bytes + budget > MAX_PATH_BYTES {
                            return Err(error(
                                ErrorKind::Configuration,
                                "Download path metadata budget exceeded",
                            )
                            .into());
                        }
                        if directory.should_skip(&name)? {
                            self.event(TransferEvent::FileResult {
                                id: self.id,
                                role: self.role,
                                name,
                                bytes: 0,
                                outcome: FileOutcome::Skipped,
                                path: None,
                            })?;
                            if let Protocol::Download(receiver) = &mut protocol {
                                receiver.skip_file().map_err(protocol_error)?;
                            }
                        } else {
                            self.current = Some(CurrentFile {
                                name: name.clone(),
                                size,
                                bytes: 0,
                                storage: FileStorage::Download(directory.create(&name)?),
                            });
                            self.check()?;
                            if let Protocol::Download(receiver) = &mut protocol {
                                receiver.accept_file_at(0).map_err(protocol_error)?;
                            }
                        }
                    }
                }
                Action::Event(Event::FileCompleted) => {
                    self.finish_file(downloads.as_ref())?;
                    self.deadline = Instant::now() + TRANSFER_TIMEOUT;
                }
                Action::Event(Event::FileSkipped) => {
                    let file = self.current.take().ok_or_else(|| {
                        error(ErrorKind::Protocol, "Skip without an active upload")
                    })?;
                    self.file_result(&file, FileOutcome::Skipped, None, false)?;
                    self.deadline = Instant::now() + TRANSFER_TIMEOUT;
                }
                Action::Event(Event::SessionCompleted) => {
                    if self.current.is_some() {
                        return Err(error(
                            ErrorKind::Protocol,
                            "Session finished with an incomplete file",
                        )
                        .into());
                    }
                    complete = true;
                }
                Action::Event(Event::Aborted) => return Err(Stop::RemoteCancelled),
                Action::Idle => {
                    if complete {
                        return self.finish_input(input, offset);
                    }
                    if self.current.is_none()
                        && !finishing
                        && let Protocol::Upload(sender) = &mut protocol
                    {
                        if let Some(source) = uploads.pop_front() {
                            let source = source.open()?;
                            sender
                                .start_file(FileInfo::new(
                                    source.name.as_bytes(),
                                    Some(Position::new(source.snapshot.len as u32)),
                                ))
                                .map_err(protocol_error)?;
                            self.current = Some(CurrentFile {
                                name: source.name.clone(),
                                size: Some(source.snapshot.len),
                                bytes: 0,
                                storage: FileStorage::Upload(source),
                            });
                        } else {
                            sender.finish().map_err(protocol_error)?;
                            finishing = true;
                        }
                        continue;
                    }
                    if offset == input.len() {
                        input.clear();
                        offset = 0;
                        if let Some(chunk) = self.shared.ingress.lock().chunks.pop_front() {
                            input = chunk;
                        }
                    }
                    if offset < input.len() {
                        // Hold CAN runs before decoding: a split cancellation must not be mistaken
                        // for a malformed escape/header before the fifth CAN arrives.
                        if input[offset] == 0x18 {
                            cans += 1;
                            offset += 1;
                            if cans >= 5 {
                                return Err(Stop::RemoteCancelled);
                            }
                            continue;
                        }
                        if cans != 0 {
                            let count = protocol.submit(&[0x18; 4][..usize::from(cans)])?;
                            if count == 0 {
                                return Err(error(
                                    ErrorKind::Protocol,
                                    "Native escape made no progress",
                                )
                                .into());
                            }
                            cans -= count as u8;
                            continue;
                        }
                        // The codec's consumed count is authoritative, including successful tails.
                        let boundary = input[offset..]
                            .iter()
                            .position(|byte| *byte == 0x18)
                            .map_or(input.len(), |index| offset + index);
                        let submitted = &input[offset..boundary];
                        let count = protocol.submit(submitted)?;
                        if count == 0 {
                            return Err(error(
                                ErrorKind::Protocol,
                                "Native protocol made no input progress",
                            )
                            .into());
                        }
                        offset += count;
                        retry_at = Instant::now() + Duration::from_secs(5);
                        if let Protocol::Upload(sender) = &protocol
                            && let Some(file) = self.current.as_mut()
                        {
                            let confirmed = u64::from(sender.acknowledged_position().get());
                            if confirmed > file.bytes {
                                file.bytes = confirmed;
                                self.deadline = Instant::now() + TRANSFER_TIMEOUT;
                                self.progress(false)?;
                            }
                        }
                    } else {
                        if Instant::now() >= retry_at {
                            protocol.timeout()?;
                            retry_at = Instant::now() + Duration::from_secs(5);
                        }
                        self.wait();
                    }
                }
                // zmodem2 keeps these enums non-exhaustive for upstream extensions.
                _ => {
                    return Err(
                        error(ErrorKind::Protocol, "Unsupported native protocol action").into(),
                    );
                }
            }
        }
    }

    fn finish_input(&self, input: Vec<u8>, offset: usize) -> Result<(), Stop> {
        let queued = {
            let mut ingress = self.shared.ingress.lock();
            ingress.closed = true;
            std::mem::take(&mut ingress.chunks)
        };
        if offset < input.len() {
            self.emit(
                RuntimeOutput::Tail {
                    id: self.id,
                    bytes: input[offset..].to_vec(),
                },
                false,
            )?;
        }
        for bytes in queued {
            self.emit(RuntimeOutput::Tail { id: self.id, bytes }, false)?;
        }
        Ok(())
    }

    fn progress(&mut self, force: bool) -> Result<(), Stop> {
        if !force && self.last_progress.elapsed() < PROGRESS_INTERVAL {
            return Ok(());
        }
        if let Some(file) = self.current.as_ref() {
            self.event(TransferEvent::Progress {
                id: self.id,
                role: self.role,
                name: file.name.clone(),
                bytes: file.bytes,
                total: file.size,
            })?;
            self.last_progress = Instant::now();
        }
        Ok(())
    }

    fn file_result(
        &self,
        file: &CurrentFile,
        outcome: FileOutcome,
        path: Option<PathBuf>,
        terminal: bool,
    ) -> Result<(), Stop> {
        self.emit(
            RuntimeOutput::Event(TransferEvent::FileResult {
                id: self.id,
                role: self.role,
                name: file.name.clone(),
                bytes: file.bytes,
                outcome,
                path,
            }),
            terminal,
        )
    }

    fn finish_file(&mut self, directory: Option<&DownloadDirectory>) -> Result<(), Stop> {
        let file = self
            .current
            .as_mut()
            .ok_or_else(|| error(ErrorKind::Protocol, "Completion without an active file"))?;
        match &mut file.storage {
            FileStorage::Upload(source) => {
                source.validate()?;
                file.bytes = source.snapshot.len;
            }
            FileStorage::Download(target) => {
                if file.size.is_some_and(|size| file.bytes != size) {
                    return Err(error(
                        ErrorKind::Protocol,
                        "Received file size does not match metadata",
                    )
                    .into());
                }
                target.file.as_file().sync_all().map_err(storage_error)?;
            }
        }
        self.check()?;
        self.progress(true)?;
        let file = self.current.take().expect("checked active file");
        let CurrentFile {
            name,
            bytes,
            storage,
            ..
        } = file;
        let mut outcome = FileOutcome::Completed;
        let mut path = None;
        match storage {
            FileStorage::Upload(_) => {}
            FileStorage::Download(target) => {
                let directory = directory
                    .ok_or_else(|| error(ErrorKind::Configuration, "Missing download directory"))?;
                if self
                    .shared
                    .phase
                    .compare_exchange(0, COMMITTING, Ordering::AcqRel, Ordering::Acquire)
                    .is_err()
                {
                    self.emit(
                        RuntimeOutput::Event(TransferEvent::FileResult {
                            id: self.id,
                            role: self.role,
                            name,
                            bytes,
                            outcome: FileOutcome::Failed(error(
                                ErrorKind::Protocol,
                                "File transfer cancelled before publication",
                            )),
                            path: None,
                        }),
                        true,
                    )?;
                    return Err(Stop::Cancelled);
                }
                // Cancellation never waits for this OS operation. Once CAS wins, publication is final.
                let published = directory.publish(target);
                self.shared.phase.fetch_and(!COMMITTING, Ordering::AcqRel);
                match published {
                    Ok(Some(published)) => {
                        self.committed_path_bytes += published.as_os_str().len();
                        self.committed_paths.push(published.clone());
                        path = Some(published);
                    }
                    Ok(None) => outcome = FileOutcome::Skipped,
                    Err(failure) => {
                        self.emit(
                            RuntimeOutput::Event(TransferEvent::FileResult {
                                id: self.id,
                                role: self.role,
                                name,
                                bytes,
                                outcome: FileOutcome::Failed(failure.clone()),
                                path: None,
                            }),
                            true,
                        )?;
                        return Err(failure.into());
                    }
                }
            }
        }
        // A cancellation racing a successful commit cannot erase its truthful file result.
        self.emit(
            RuntimeOutput::Event(TransferEvent::FileResult {
                id: self.id,
                role: self.role,
                name,
                bytes,
                outcome,
                path,
            }),
            true,
        )?;
        self.check()
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.shared.ingress.lock().closed = true;
    }
}

struct CurrentFile {
    name: String,
    size: Option<u64>,
    bytes: u64,
    storage: FileStorage,
}

enum FileStorage {
    Upload(OpenUpload),
    Download(DownloadTarget),
}

fn safe_name(bytes: &[u8]) -> Result<String, TransferError> {
    let name = std::str::from_utf8(bytes)
        .map_err(|_| error(ErrorKind::UnsafeName, "File name is not valid UTF-8"))?;
    if name.is_empty()
        || name.len() > 255
        || name.contains(['/', '\\', ':'])
        || name.chars().any(char::is_control)
        || name.ends_with([' ', '.'])
    {
        return Err(error(ErrorKind::UnsafeName, "Unsafe transfer file name"));
    }
    let mut components = Path::new(name).components();
    if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
        return Err(error(
            ErrorKind::UnsafeName,
            "Transfer name must be a single file name",
        ));
    }
    Ok(name.to_owned())
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Snapshot {
    len: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    identity: (u64, u64, i64, i64),
}

impl Snapshot {
    fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            identity: {
                use std::os::unix::fs::MetadataExt;
                (
                    metadata.dev(),
                    metadata.ino(),
                    metadata.ctime(),
                    metadata.ctime_nsec(),
                )
            },
        }
    }
}

struct UploadSource {
    path: PathBuf,
    name: String,
    snapshot: Snapshot,
}

impl UploadSource {
    fn inspect(path: PathBuf) -> Result<Self, TransferError> {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| error(ErrorKind::UnsafeName, "Invalid upload file name"))?;
        let name = safe_name(name.as_bytes())?;
        let metadata = fs::symlink_metadata(&path).map_err(storage_error)?;
        if !metadata.is_file() {
            return Err(error(
                ErrorKind::Configuration,
                "Uploads must be regular files, not links or devices",
            ));
        }
        if metadata.len() > u64::from(u32::MAX) {
            return Err(error(
                ErrorKind::FileTooLarge,
                "ZMODEM files cannot exceed u32::MAX bytes",
            ));
        }
        Ok(Self {
            path,
            name,
            snapshot: Snapshot::from_metadata(&metadata),
        })
    }

    fn open(self) -> Result<OpenUpload, TransferError> {
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let file = options.open(&self.path).map_err(storage_error)?;
        let source = OpenUpload {
            path: self.path,
            name: self.name,
            snapshot: self.snapshot,
            file,
        };
        source.validate()?;
        Ok(source)
    }
}

struct OpenUpload {
    path: PathBuf,
    name: String,
    snapshot: Snapshot,
    file: File,
}

impl OpenUpload {
    fn validate(&self) -> Result<(), TransferError> {
        let descriptor = self.file.metadata().map_err(storage_error)?;
        let path = fs::symlink_metadata(&self.path).map_err(storage_error)?;
        if !descriptor.is_file()
            || !path.is_file()
            || Snapshot::from_metadata(&descriptor) != self.snapshot
            || Snapshot::from_metadata(&path) != self.snapshot
        {
            return Err(error(
                ErrorKind::SourceChanged,
                "Upload source changed during transfer",
            ));
        }
        Ok(())
    }

    fn read(&mut self, offset: u32, requested: usize) -> Result<Vec<u8>, TransferError> {
        self.validate()?;
        let remaining = self
            .snapshot
            .len
            .checked_sub(u64::from(offset))
            .ok_or_else(|| error(ErrorKind::Protocol, "Peer requested an invalid file offset"))?;
        let length = requested.min(MAX_INPUT_CHUNK).min(remaining as usize);
        if length == 0 {
            return Err(error(ErrorKind::SourceChanged, "Unexpected upload EOF"));
        }
        self.file
            .seek(SeekFrom::Start(u64::from(offset)))
            .map_err(storage_error)?;
        let mut data = vec![0; length];
        self.file.read_exact(&mut data).map_err(|failure| {
            if failure.kind() == std::io::ErrorKind::UnexpectedEof {
                error(ErrorKind::SourceChanged, "Upload source ended early")
            } else {
                storage_error(failure)
            }
        })?;
        self.validate()?;
        Ok(data)
    }
}

struct DownloadDirectory {
    path: PathBuf,
    policy: OverwritePolicy,
}

struct DownloadTarget {
    name: String,
    file: NamedTempFile,
}

impl DownloadDirectory {
    fn open(path: PathBuf, policy: OverwritePolicy) -> Result<Self, TransferError> {
        let path = path.canonicalize().map_err(storage_error)?;
        validate_path_budget(std::iter::once(path.as_path()))?;
        if !path.is_dir() {
            return Err(error(
                ErrorKind::Configuration,
                "Download destination is not a directory",
            ));
        }
        Ok(Self { path, policy })
    }

    fn should_skip(&self, name: &str) -> Result<bool, TransferError> {
        if self.policy != OverwritePolicy::Skip {
            return Ok(false);
        }
        match fs::symlink_metadata(self.path.join(name)) {
            Ok(_) => Ok(true),
            Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(failure) => Err(storage_error(failure)),
        }
    }

    fn create(&self, name: &str) -> Result<DownloadTarget, TransferError> {
        let file = tempfile::Builder::new()
            .prefix(".agenterm-zmodem-")
            .tempfile_in(&self.path)
            .map_err(storage_error)?;
        Ok(DownloadTarget {
            name: name.to_owned(),
            file,
        })
    }

    fn publish(&self, target: DownloadTarget) -> Result<Option<PathBuf>, TransferError> {
        let path = self.path.join(&target.name);
        match self.policy {
            OverwritePolicy::Overwrite => {
                target
                    .file
                    .persist(&path)
                    .map_err(|failure| storage_error(failure.error))?;
                Ok(Some(path))
            }
            OverwritePolicy::Skip => match target.file.persist_noclobber(&path) {
                Ok(_) => Ok(Some(path)),
                Err(failure) if failure.error.kind() == std::io::ErrorKind::AlreadyExists => {
                    Ok(None)
                }
                Err(failure) => Err(storage_error(failure.error)),
            },
            OverwritePolicy::Rename => {
                let mut file = target.file;
                for index in 0..10_000 {
                    let path = if index == 0 {
                        path.clone()
                    } else {
                        let name = renamed_name(&target.name, index);
                        self.path.join(name)
                    };
                    match file.persist_noclobber(&path) {
                        Ok(_) => return Ok(Some(path)),
                        Err(failure)
                            if failure.error.kind() == std::io::ErrorKind::AlreadyExists =>
                        {
                            file = failure.file;
                        }
                        Err(failure) => return Err(storage_error(failure.error)),
                    }
                }
                Err(error(
                    ErrorKind::Storage,
                    "No available download name after 10000 attempts",
                ))
            }
        }
    }
}

fn renamed_name(name: &str, index: usize) -> String {
    let suffix = format!(" ({index})");
    let (stem, extension) = name
        .rsplit_once('.')
        .filter(|(stem, _)| !stem.is_empty())
        .map_or((name, ""), |(stem, _)| (stem, &name[stem.len()..]));
    let extension = if extension.len() > 64 { "" } else { extension };
    let mut end = stem.len().min(255 - suffix.len() - extension.len());
    while !stem.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{suffix}{extension}", &stem[..end])
}

#[cfg(test)]
#[path = "zmodem_runtime_tests.rs"]
mod tests;
