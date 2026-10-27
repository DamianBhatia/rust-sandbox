# rust-sandbox

A from-scratch Linux container sandbox, written in Rust as a learning project.
Each milestone adds one isolation mechanism the kernel already provides to
containers. The command you pass in is what runs inside.

Milestone 1 runs that command as a child process and returns its exit status.
Nothing is isolated yet.

## Roadmap

- [x] Process runner: fork, exec, wait, and shell-style exit codes
- [ ] UTS namespace (own hostname)
- [ ] PID namespace (command is PID 1; zombie reaping)
- [ ] Mount namespace and `pivot_root` into an Alpine minirootfs, with a fresh `/proc`
- [ ] cgroups v2 limits (memory, pids, cpu)
- [ ] Stretch: network namespace, dropped capabilities, seccomp, user namespace (rootless)

## Build and run

Linux only. WSL2 works. Other operating systems are not a target.

```bash
cargo build
./target/debug/sandbox run echo hello
./target/debug/sandbox run -- sh -c 'exit 3'
```

Put `--` before the program when its own arguments start with `-`:

```bash
./target/debug/sandbox run -- echo -n hi
```

Run the tests with `cargo test`. GitHub Actions runs `cargo fmt --check`,
`cargo clippy --all-targets --all-features -- -D warnings`, and `cargo test`
on Ubuntu.

## How it works

`sandbox run` does not use `std::process::Command`. That helper forks and
execs inside the standard library, with no place to run code in between.
Later milestones have to change hostname, namespaces, and the root filesystem
in the child before the target program starts, so the split is explicit:

1. The parent turns each argument into a `CString`, then calls `fork`. The
   kernel duplicates the process. Parent and child share the pages
   copy-on-write until one of them writes, which is why the child can see
   those strings without anyone sending them over a pipe.
2. The child calls `execvp`, which replaces this process image with the
   requested program. The pid does not change. If `execvp` returns, it failed.
   The child writes the raw error number to a pipe and `_exit`s. It does not
   format the message: after `fork`, allocating or taking a lock is unsafe
   unless the process is still single-threaded, and formatting does both.
3. The parent calls `waitpid`. That collects the child's status and reaps it.
   A child that has exited but has not been waited on is a zombie: the kernel
   keeps its process-table entry so the parent can still read the status.

Exit codes follow the shell convention:

- The child called `exit(N)` (or returned from `main`): the sandbox exits `N`.
- The command was not found (`ENOENT`): exit `127`.
- The command was found but `exec` failed (for example, permission denied): exit `126`.
- The child was killed by signal `N`: exit `128 + N`. A shell uses the same
  rule, which is why `SIGKILL` (signal 9) shows up as `137`.

Anything else (a failed `fork`, an argument containing an interior NUL) is the
sandbox's own error. It prints a message to stderr and exits `1`.

## License

MIT. Copyright 2026 Damian Bhatia.
