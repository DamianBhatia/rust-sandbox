//! End-to-end checks for `sandbox run`. These spawn the built binary the way
//! a user would, so they cover CLI parsing and the real fork/exec/wait path.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use assert_cmd::Command;
use predicates::str::contains;

fn sandbox() -> Command {
    Command::cargo_bin("sandbox").expect("build the sandbox binary")
}

#[test]
fn echo_stdout_is_inherited() {
    sandbox()
        .args(["run", "echo", "hello"])
        .assert()
        .success()
        .stdout("hello\n");
}

#[test]
fn child_stderr_is_inherited() {
    sandbox()
        .args(["run", "sh", "-c", "echo oops >&2"])
        .assert()
        .success()
        .stdout("")
        .stderr("oops\n");
}

#[test]
fn exit_code_is_propagated() {
    sandbox()
        .args(["run", "sh", "-c", "exit 3"])
        .assert()
        .code(3)
        .stdout("")
        .stderr("");
}

#[test]
fn missing_command_exits_127() {
    sandbox()
        .args(["run", "no-such-sandbox-command"])
        .assert()
        .code(127)
        .stderr(contains("no-such-sandbox-command"))
        .stderr(contains("No such file or directory"));
}

#[test]
fn non_executable_command_exits_126() {
    let path = temp_script();
    sandbox()
        .args(["run", path.to_str().expect("temp path is utf-8")])
        .assert()
        .code(126)
        .stderr(contains("Permission denied"));
    let _ = std::fs::remove_file(path);
}

#[test]
fn signal_killed_child_exits_128_plus_signal() {
    // SIGKILL is 9 and cannot be caught, so the shell dies with 128 + 9.
    sandbox()
        .args(["run", "sh", "-c", "kill -KILL $$"])
        .assert()
        .code(128 + 9);
}

#[test]
fn double_dash_passes_flag_like_arguments() {
    sandbox()
        .args(["run", "--", "echo", "-n", "hi"])
        .assert()
        .success()
        .stdout("hi");
}

/// A file the kernel will refuse to exec. Mode 0644 has no execute bit.
fn temp_script() -> PathBuf {
    let path = std::env::temp_dir().join(format!("sandbox-not-executable-{}", std::process::id()));
    std::fs::write(&path, b"#!/bin/sh\necho should-not-run\n").expect("write temp script");
    let mut permissions = std::fs::metadata(&path)
        .expect("stat temp script")
        .permissions();
    permissions.set_mode(0o644);
    std::fs::set_permissions(&path, permissions).expect("chmod temp script");
    path
}
