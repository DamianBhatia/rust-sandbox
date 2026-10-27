//! Failures the sandbox reports, and the exit code each one maps to.
//!
//! `thiserror` is used instead of `anyhow` because the exit code depends on
//! which failure happened (127, 126, or 1). A typed variant keeps that
//! decision in one place. `#[from]` on `NulError` generates
//! `From<NulError> for Error`, which is what `?` calls to convert the error
//! at the `CString` call site.

use std::ffi::NulError;

use nix::errno::Errno;
use nix::sys::wait::WaitStatus;
use thiserror::Error;

#[derive(Debug, Error)]
pub(crate) enum Error {
    /// A syscall in the parent failed. `context` is a string literal, so it
    /// can be `&'static str` rather than an owned `String`.
    #[error("{context}: {source}")]
    Syscall {
        context: &'static str,
        #[source]
        source: Errno,
    },

    /// `execvp` failed. `exit_code` is the shell convention: 127 when the
    /// command does not exist, 126 for any other exec failure.
    #[error("failed to execute '{command}': {source}")]
    Exec {
        command: String,
        exit_code: i32,
        #[source]
        source: Errno,
    },

    #[error("an argument contains an interior null byte, which C strings cannot represent: {0}")]
    NulInArgument(#[from] NulError),

    #[error("no command was given")]
    EmptyCommand,

    /// `Stopped`, `Continued`, or another state we do not ask `waitpid` to report.
    #[error("waitpid returned a status this sandbox does not handle: {status:?}")]
    UnexpectedStatus { status: WaitStatus },

    #[error("the exec-error pipe closed after a partial write")]
    PartialExecReport,
}

impl Error {
    /// Exit code for this failure.
    ///
    /// `Exec` carries a shell-style code. Every other variant is the
    /// sandbox's own problem and exits 1, so a sandbox bug is not confused
    /// with a child that chose to exit 1.
    pub(crate) fn exit_code(&self) -> i32 {
        match self {
            Error::Exec { exit_code, .. } => *exit_code,
            _ => 1,
        }
    }
}

/// Attach a static description to a `nix` error, then let the caller `?` it.
pub(crate) fn syscall<T>(context: &'static str, result: nix::Result<T>) -> Result<T, Error> {
    result.map_err(|source| Error::Syscall { context, source })
}
