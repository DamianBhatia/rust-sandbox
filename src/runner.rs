//! Fork a child, optionally configure it, exec the requested program, and wait.
//!
//! `std::process::Command` forks and execs inside the standard library, so
//! there is no place to run code in the child first. Later milestones set the
//! hostname, namespaces, and root filesystem in that window. `fork` is enough
//! for this milestone; `clone` with namespace flags can replace the `fork`
//! call when a new PID namespace has to make the child PID 1.

use std::ffi::{CString, OsString};
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;

use nix::errno::Errno;
use nix::fcntl::OFlag;
use nix::sys::wait::{waitpid, WaitStatus};
use nix::unistd::{self, ForkResult, Pid};

use crate::error::{syscall, Error};

/// Everything one `sandbox run` needs.
///
/// `argv` is all the parent has to build today. Later milestones add fields
/// the child reads in [`Spec::configure_child`] (hostname, mounts) and fields
/// the parent applies after `fork` returns (cgroup membership of the child pid).
#[derive(Debug)]
pub(crate) struct Spec {
    pub(crate) argv: Vec<CString>,
}

impl Spec {
    pub(crate) fn from_args(args: &[OsString]) -> Result<Self, Error> {
        Ok(Spec {
            argv: cstring_argv(args)?,
        })
    }

    /// Runs in the child, after `fork` and before `execvp`.
    ///
    /// Empty on purpose. The calls that belong here (`unshare`, `sethostname`,
    /// `pivot_root`) must see the sandbox binary, not the program it execs.
    /// Do not panic: unwinding runs destructors, and those call the allocator,
    /// which is not async-signal-safe in a forked child.
    fn configure_child(&self) {}
}

/// Run `spec.argv` and return the shell-style exit code.
pub(crate) fn run(spec: &Spec) -> Result<i32, Error> {
    if spec.argv.is_empty() {
        return Err(Error::EmptyCommand);
    }

    // `O_CLOEXEC` is set in the same syscall as the pipe so the write end
    // cannot survive `execvp`. If the executed program inherited it, the
    // parent's read would never see EOF and would block forever.
    let (read_fd, write_fd) = syscall("pipe2", unistd::pipe2(OFlag::O_CLOEXEC))?;

    // SAFETY: this process is single-threaded here, so `fork` cannot
    // duplicate it while another thread holds a lock. The child path only
    // closes a descriptor, writes the raw errno, and execs or `_exit`s.
    // `nix`'s `execvp` does allocate a short pointer vector; that is safe
    // only because no other thread exists to hold the allocator lock.
    // Do not spawn threads before this call.
    let forked = unsafe { unistd::fork() };
    match syscall("fork", forked)? {
        ForkResult::Parent { child } => parent(child, read_fd, write_fd, spec),
        // This arm must not return into `main`: both processes would keep
        // running the sandbox, and the child's return path would run
        // destructors. `Drop` calls the allocator, which is not
        // async-signal-safe after `fork`. `run_child -> !` makes a missing
        // `_exit` a compile error.
        ForkResult::Child => {
            drop(read_fd);
            run_child(&write_fd, spec)
        }
    }
}

/// Arguments for `execvp`, built in the parent before `fork`.
///
/// `execvp` is a C API, so each string needs a trailing NUL and no interior
/// NUL. `CString::new` checks that and allocates. Doing it here keeps that
/// allocation out of the child. On failure, `?` converts `NulError` into
/// `Error` via the `From` impl that `#[from]` generates in `error.rs`.
fn cstring_argv(args: &[OsString]) -> Result<Vec<CString>, Error> {
    let mut argv = Vec::with_capacity(args.len());
    for arg in args {
        argv.push(CString::new(arg.as_bytes())?);
    }
    Ok(argv)
}

fn parent(child: Pid, read_fd: OwnedFd, write_fd: OwnedFd, spec: &Spec) -> Result<i32, Error> {
    // `fork` copied the `OwnedFd` values, but each process has its own
    // descriptor table. Dropping the write end here closes only the parent's
    // copy. The child still holds one. We must drop ours before reading:
    // after a successful exec, `CLOEXEC` closes the child's copy, and a
    // leftover parent copy would keep the read blocked forever.
    drop(write_fd);

    let reported = read_exec_errno(&read_fd);
    drop(read_fd);

    // Always wait, including when the pipe read failed. `waitpid` both
    // returns the status and reaps the child. Without it the exited child
    // stays a zombie until this process exits.
    let status = wait_child(child)?;
    match reported? {
        Some(errno) => Err(Error::Exec {
            command: command_name(&spec.argv),
            exit_code: shell_exec_status(errno),
            source: errno,
        }),
        // No errno means exec replaced the process, or the child died before
        // it could write. Either way, `waitpid`'s status is the result.
        None => status_to_exit_code(status),
    }
}

fn run_child(write_fd: &OwnedFd, spec: &Spec) -> ! {
    spec.configure_child();

    // Borrow the vector. The child must not drop it: `Drop` calls the
    // allocator, which is not async-signal-safe here. Copy-on-write means a
    // write in the child would not change the parent's pages anyway.
    let argv = &spec.argv;
    match unistd::execvp(&argv[0], argv) {
        // Success does not return. `Result<Infallible, _>` has an `Ok` arm
        // only so the type checks; `never` has no value to match on.
        Ok(never) => match never {},
        Err(err) => {
            report_errno(write_fd, err);
            // `_exit` skips destructors and C `atexit` handlers, so stdio
            // buffers copied from the parent are not flushed a second time.
            exit_child(shell_exec_status(err));
        }
    }
}

fn command_name(argv: &[CString]) -> String {
    match argv.first() {
        Some(name) => name.to_string_lossy().into_owned(),
        None => String::new(),
    }
}

/// Shell convention, so a caller can tell "not found" from "could not run".
fn shell_exec_status(err: Errno) -> i32 {
    if err == Errno::ENOENT {
        127
    } else {
        126
    }
}

/// The child can only write raw bytes. Formatting an error message allocates.
fn report_errno(fd: &OwnedFd, err: Errno) {
    let bytes = (err as i32).to_ne_bytes();
    let mut rest = bytes.as_slice();
    while !rest.is_empty() {
        match unistd::write(fd, rest) {
            Ok(0) => break,
            Ok(n) => rest = &rest[n..],
            // The write was interrupted. It did not happen; try again.
            Err(Errno::EINTR) => continue,
            // Nothing else we can do. The parent still sees `_exit`'s code
            // if these bytes never arrive.
            Err(_) => break,
        }
    }
}

fn exit_child(code: i32) -> ! {
    // SAFETY: `_exit` ends this process without running Rust destructors.
    // The binding is `unsafe` because it is a raw libc call, not because
    // the termination itself is optional.
    unsafe { nix::libc::_exit(code) }
}

fn read_exec_errno(fd: &OwnedFd) -> Result<Option<Errno>, Error> {
    // `i32` native endian: both ends of the pipe are this same machine.
    let mut buf = [0u8; 4];
    let mut filled = 0;
    while filled < buf.len() {
        match unistd::read(fd, &mut buf[filled..]) {
            Ok(0) if filled == 0 => return Ok(None),
            Ok(0) => return Err(Error::PartialExecReport),
            Ok(n) => filled += n,
            Err(Errno::EINTR) => continue,
            Err(source) => {
                return Err(Error::Syscall {
                    context: "reading the exec-error pipe",
                    source,
                });
            }
        }
    }
    Ok(Some(Errno::from_raw(i32::from_ne_bytes(buf))))
}

fn wait_child(child: Pid) -> Result<WaitStatus, Error> {
    loop {
        match waitpid(child, None) {
            Ok(status) => return Ok(status),
            Err(Errno::EINTR) => continue,
            Err(source) => {
                return Err(Error::Syscall {
                    context: "waitpid",
                    source,
                });
            }
        }
    }
}

/// Map a reaped child to the code a shell would exit with.
fn status_to_exit_code(status: WaitStatus) -> Result<i32, Error> {
    match status {
        WaitStatus::Exited(_, code) => Ok(code),
        WaitStatus::Signaled(_, signal, _) => {
            // `Signal` is `#[repr(i32)]` with the libc numbers as discriminants.
            // Shells report a fatal signal as `128 + n`, so `SIGKILL` (9) is 137.
            Ok(128 + signal as i32)
        }
        other => Err(Error::UnexpectedStatus { status: other }),
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    use nix::sys::signal::Signal;
    use nix::sys::wait::WaitStatus;
    use nix::unistd::Pid;

    use super::*;

    fn pid() -> Pid {
        Pid::from_raw(1)
    }

    #[test]
    fn exited_status_is_the_child_code() {
        let status = WaitStatus::Exited(pid(), 3);
        assert_eq!(status_to_exit_code(status).unwrap(), 3);
    }

    #[test]
    fn signaled_status_is_128_plus_signal() {
        let status = WaitStatus::Signaled(pid(), Signal::SIGKILL, true);
        assert_eq!(status_to_exit_code(status).unwrap(), 137);
        let status = WaitStatus::Signaled(pid(), Signal::SIGTERM, false);
        assert_eq!(status_to_exit_code(status).unwrap(), 143);
    }

    #[test]
    fn continued_status_is_an_error() {
        let status = WaitStatus::Continued(pid());
        assert!(status_to_exit_code(status).is_err());
    }

    #[test]
    fn exec_status_follows_the_shell() {
        assert_eq!(shell_exec_status(Errno::ENOENT), 127);
        assert_eq!(shell_exec_status(Errno::EACCES), 126);
    }

    #[test]
    fn interior_nul_is_rejected_before_fork() {
        let arg = OsString::from_vec(b"ab\0cd".to_vec());
        let err = Spec::from_args(&[arg]).unwrap_err();
        assert_eq!(err.exit_code(), 1);
    }
}
