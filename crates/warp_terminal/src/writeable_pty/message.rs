use std::borrow::Cow;
use std::path::PathBuf;

use crate::SizeInfo;

/// Messages that may be sent to the `EventLoop`.
#[derive(Debug)]
pub enum Message {
    /// Data that should be written to the PTY.
    Input(Cow<'static, [u8]>),

    /// Indicates that the `EventLoop` should be shut down.
    Shutdown,

    /// Indicates that the child process has exited.
    ///
    /// Only used on Windows, as we need to pass this information to the
    /// event loop via the channel (and cannot use the child event token).
    #[cfg_attr(not(windows), allow(dead_code))]
    ChildExited,

    /// Instruction to resize the PTY.
    Resize(SizeInfo),

    /// Start a ZMODEM upload of the given local files to a remote `rz`.
    ///
    /// The transfer is driven on the PTY thread, which owns the byte stream.
    StartZmodemUpload { paths: Vec<PathBuf> },
}
