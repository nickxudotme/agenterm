//! Per-terminal metadata. Business file I/O belongs exclusively to the protocol worker.

use warp_terminal::zmodem::runtime::{
    FileOutcome, MAX_FILES, Role, TransferEvent, TransferId, TransferOutcome,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TransferPhase {
    SelectingExplicitUpload,
    SelectingRequested,
    Running,
    Cancelling,
}

#[derive(Clone, Debug)]
pub(crate) struct FileStatus {
    pub name: String,
    pub bytes: u64,
    pub total: Option<u64>,
    pub outcome: Option<FileOutcome>,
}

#[derive(Debug)]
pub(crate) struct ActiveTransfer {
    pub id: TransferId,
    pub role: Role,
    pub phase: TransferPhase,
    pub current_file: Option<FileStatus>,
    pub files: Vec<FileStatus>,
}

#[derive(Debug)]
pub(crate) struct TransferSummary {
    pub id: TransferId,
    pub role: Role,
    pub outcome: TransferOutcome,
    pub files: Vec<FileStatus>,
}

#[derive(Default)]
pub(crate) struct ZmodemTransfer {
    pub active: Option<ActiveTransfer>,
    pub last: Option<TransferSummary>,
    newest_id: TransferId,
}

impl ZmodemTransfer {
    pub fn begin_explicit_upload(&mut self, id: TransferId) -> bool {
        if self.active.is_some() || id <= self.newest_id {
            return false;
        }
        self.begin(id, Role::Upload, TransferPhase::SelectingExplicitUpload);
        true
    }

    fn begin(&mut self, id: TransferId, role: Role, phase: TransferPhase) {
        self.newest_id = id;
        self.last = None;
        self.active = Some(ActiveTransfer {
            id,
            role,
            phase,
            current_file: None,
            files: Vec::new(),
        });
    }

    /// Returns true only for a new request that needs a picker.
    pub fn request(&mut self, id: TransferId, role: Role) -> bool {
        if id <= self.newest_id {
            return false;
        }
        if self
            .active
            .as_ref()
            .is_some_and(|active| active.phase != TransferPhase::SelectingExplicitUpload)
        {
            return false;
        }
        self.begin(id, role, TransferPhase::SelectingRequested);
        true
    }

    pub fn is_selecting(&self, id: TransferId, role: Role, explicit: bool) -> bool {
        self.active.as_ref().is_some_and(|active| {
            active.id == id
                && active.role == role
                && active.phase
                    == if explicit {
                        TransferPhase::SelectingExplicitUpload
                    } else {
                        TransferPhase::SelectingRequested
                    }
        })
    }

    pub fn start(&mut self, id: TransferId) {
        if let Some(active) = self.active.as_mut().filter(|active| active.id == id) {
            active.phase = TransferPhase::Running;
        }
    }

    pub fn cancel(&mut self, id: TransferId) -> bool {
        let Some(active) = self.active.as_mut().filter(|active| active.id == id) else {
            return false;
        };
        if active.phase == TransferPhase::SelectingExplicitUpload {
            self.finish(id, TransferOutcome::Cancelled);
        } else {
            active.phase = TransferPhase::Cancelling;
        }
        true
    }

    /// Applies ordered runtime metadata, rejecting stale or foreign events.
    pub fn apply(&mut self, event: &TransferEvent) -> bool {
        let (id, role) = match event {
            TransferEvent::Requested { .. } => return false,
            TransferEvent::FileStarted { id, role, .. }
            | TransferEvent::Progress { id, role, .. }
            | TransferEvent::FileResult { id, role, .. }
            | TransferEvent::Finished { id, role, .. } => (*id, *role),
        };
        let Some(active) = self
            .active
            .as_mut()
            .filter(|active| active.id == id && active.role == role)
        else {
            return false;
        };
        match event {
            TransferEvent::Requested { .. } => return false,
            TransferEvent::FileStarted { name, size, .. } => {
                active.current_file = Some(FileStatus {
                    name: display_text(name),
                    bytes: 0,
                    total: *size,
                    outcome: None,
                });
            }
            TransferEvent::Progress {
                name, bytes, total, ..
            } => {
                active.current_file = Some(FileStatus {
                    name: display_text(name),
                    bytes: *bytes,
                    total: *total,
                    outcome: None,
                });
            }
            TransferEvent::FileResult {
                name,
                bytes,
                outcome,
                ..
            } => {
                let file = FileStatus {
                    name: display_text(name),
                    bytes: *bytes,
                    total: active.current_file.as_ref().and_then(|file| file.total),
                    outcome: Some(outcome.clone()),
                };
                active.current_file = Some(file.clone());
                if active.files.len() == MAX_FILES {
                    active.files.remove(0);
                }
                active.files.push(file);
            }
            TransferEvent::Finished { outcome, .. } => {
                self.finish(id, outcome.clone());
            }
        }
        true
    }

    pub fn finish(&mut self, id: TransferId, outcome: TransferOutcome) {
        if self.active.as_ref().is_none_or(|active| active.id != id) {
            return;
        }
        let active = self.active.take().expect("matching active transfer");
        self.last = Some(TransferSummary {
            id,
            role: active.role,
            outcome,
            files: active.files,
        });
    }

    pub fn status_text(&self) -> Option<String> {
        if let Some(active) = &self.active {
            let direction = direction(active.role);
            let phase = match active.phase {
                TransferPhase::SelectingExplicitUpload | TransferPhase::SelectingRequested => {
                    "Choose files or directory"
                }
                TransferPhase::Running => "Connecting",
                TransferPhase::Cancelling => return Some(format!("{direction}: cancelling")),
            };
            return Some(match &active.current_file {
                Some(file) => format!("{direction}: {}", file_status(file)),
                None => format!("{direction}: {phase}"),
            });
        }
        self.last.as_ref().map(|last| {
            let completed = last
                .files
                .iter()
                .filter(|file| matches!(file.outcome, Some(FileOutcome::Completed)))
                .count();
            let skipped = last
                .files
                .iter()
                .filter(|file| matches!(file.outcome, Some(FileOutcome::Skipped)))
                .count();
            let outcome = match &last.outcome {
                TransferOutcome::Completed => "complete".to_owned(),
                TransferOutcome::Cancelled => "cancelled".to_owned(),
                TransferOutcome::RemoteCancelled => "cancelled by remote".to_owned(),
                TransferOutcome::Failed(error) => {
                    format!("failed: {}", display_text(&error.message))
                }
            };
            format!(
                "{} {outcome}: {completed} completed, {skipped} skipped",
                direction(last.role)
            )
        })
    }
}

fn direction(role: Role) -> &'static str {
    match role {
        Role::Upload => "Upload",
        Role::Download => "Download",
    }
}

pub(crate) fn file_status(file: &FileStatus) -> String {
    let result = match &file.outcome {
        Some(FileOutcome::Completed) => "completed".to_owned(),
        Some(FileOutcome::Skipped) => "skipped".to_owned(),
        Some(FileOutcome::Failed(error)) => format!("failed: {}", display_text(&error.message)),
        None => match file.total {
            Some(total) => return format!("{}: {} / {total} bytes", file.name, file.bytes),
            None => "transferring".to_owned(),
        },
    };
    format!("{}: {} bytes, {result}", file.name, file.bytes)
}

pub(crate) fn display_text(text: &str) -> String {
    text.chars()
        .take(160)
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect()
}

#[cfg(test)]
#[path = "zmodem_transfer_tests.rs"]
mod tests;
