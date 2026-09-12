//! Writes ZMODEM file transfers to disk.
//!
//! The PTY event loop drives the protocol but cannot block on file I/O or open
//! a save dialog, so it emits progress events instead. This model consumes
//! those events, buffers incoming file data, and asks the user where to save
//! each download.

use std::fs::File;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use warpui::{AppContext, Entity, ModelContext, SingletonEntity, WindowId};

use crate::view_components::DismissibleToast;
use crate::workspace::ToastStack;

/// Default location offered for downloads when nothing was chosen before.
fn default_download_directory() -> PathBuf {
    directories::UserDirs::new()
        .and_then(|dirs| dirs.download_dir().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// A file being received from the remote side.
struct PendingDownload {
    name: String,
    bytes: Vec<u8>,
    bytes_total: u64,
}

/// Tracks one ZMODEM transfer and persists the files that arrive.
#[derive(Default)]
pub struct ZmodemTransfer {
    /// Data buffered for the file currently in flight.
    pending: Option<PendingDownload>,
    /// Directory last used for a download, so the next dialog starts there.
    last_directory: Option<PathBuf>,
}

impl Entity for ZmodemTransfer {
    type Event = ();
}

impl SingletonEntity for ZmodemTransfer {}

impl ZmodemTransfer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records progress for the file in flight, creating it if needed.
    pub fn note_progress(
        &mut self,
        file_name: String,
        _bytes_transferred: u64,
        bytes_total: u64,
    ) {
        match self.pending.as_mut() {
            Some(pending) => {
                if pending.name != file_name {
                    // A new file started without a completion for the previous
                    // one; keep the newer file and drop the stale buffer.
                    self.pending = Some(PendingDownload {
                        name: file_name,
                        bytes: Vec::new(),
                        bytes_total,
                    });
                } else {
                    pending.bytes_total = bytes_total;
                }
            }
            None => {
                self.pending = Some(PendingDownload {
                    name: file_name,
                    bytes: Vec::new(),
                    bytes_total,
                });
            }
        }
    }

    /// Appends received data to the file in flight.
    pub fn append_data(&mut self, data: &[u8]) {
        if let Some(pending) = self.pending.as_mut() {
            pending.bytes.extend_from_slice(data);
        }
    }

    /// Finishes the file in flight, prompting for a destination and writing it.
    ///
    /// The dialog is asynchronous, so this returns as soon as it is shown.
    pub fn finish_file(&mut self, window_id: WindowId, ctx: &mut ModelContext<Self>) {
        let Some(pending) = self.pending.take() else {
            return;
        };

        let directory = self
            .last_directory
            .clone()
            .unwrap_or_else(default_download_directory);
        let config = warpui::platform::file_picker::SaveFilePickerConfiguration::new()
            .with_default_filename(pending.name.clone())
            .with_default_directory(directory.clone());
        self.last_directory = Some(directory);

        let name = pending.name.clone();
        let bytes = pending.bytes.clone();

        ctx.open_save_file_picker(
            move |path_opt: Option<String>, ctx: &mut AppContext| {
                let Some(path) = path_opt else {
                    // The user cancelled; the transfer already happened, so
                    // there is nothing to undo beyond discarding the buffer.
                    return;
                };
                match write_file(Path::new(&path), &bytes) {
                    Ok(written) => {
                        ToastStack::handle(ctx).update(ctx, |stack, ctx| {
                            stack.add_ephemeral_toast(
                                DismissibleToast::success(format!(
                                    "Saved {name} ({written} bytes)"
                                )),
                                window_id,
                                ctx,
                            );
                        });
                    }
                    Err(error) => {
                        log::warn!("Failed to write ZMODEM download: {error}");
                        ToastStack::handle(ctx).update(ctx, |stack, ctx| {
                            stack.add_ephemeral_toast(
                                DismissibleToast::error(format!(
                                    "Could not save {name}: {error}"
                                )),
                                window_id,
                                ctx,
                            );
                        });
                    }
                }
            },
            config,
        );
    }

    /// Reports the end of a transfer through a toast.
    pub fn finish_transfer(
        &mut self,
        error: Option<String>,
        window_id: WindowId,
        ctx: &mut ModelContext<Self>,
    ) {
        // A file that never emitted a completion still holds buffered data;
        // it is dropped here so the next transfer starts clean.
        self.pending = None;
        ToastStack::handle(ctx).update(ctx, |stack, ctx| {
            let toast = match error {
                Some(message) => {
                    DismissibleToast::error(format!("ZMODEM transfer failed: {message}"))
                }
                None => DismissibleToast::success("ZMODEM transfer complete".to_owned()),
            };
            stack.add_ephemeral_toast(toast, window_id, ctx);
        });
    }
}

/// Writes `bytes` to `path`, creating or truncating the file.
fn write_file(path: &Path, bytes: &[u8]) -> std::io::Result<usize> {
    let mut file = File::create(path)?;
    file.write_all(bytes)?;
    Ok(bytes.len())
}

#[cfg(test)]
#[path = "zmodem_transfer_tests.rs"]
mod tests;
