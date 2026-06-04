//! Shared helpers for the a2a-shim binary's integration tests.
//!
//! Cargo treats every file under tests/ as a separate test binary, so a
//! plain `mod` doesn't work for cross-test sharing. The convention is to
//! put shared code under `tests/common/mod.rs` (not `tests/common.rs`)
//! so cargo does not treat it as a test binary in its own right, then
//! `mod common;` from each test file.

#![allow(dead_code)]

use std::process::Child;

/// Kill a process and all its descendants. On Windows the default
/// `Child::kill` only kills the immediate process, leaving any
/// subprocesses it spawned as orphans; for our e2e tests the serve
/// shim spawns `mock_acp_agent` which would otherwise survive past
/// the test run and hold file locks on the binary, deadlocking later
/// `cargo build` / `cargo test` invocations.
pub fn kill_tree(child: &mut Child) {
    let pid = child.id();
    #[cfg(windows)]
    {
        // `/T` = tree-kill, `/F` = forceful. Ignore errors — the child
        // may already be dead or never started any descendants.
        let _ = std::process::Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    #[cfg(not(windows))]
    {
        // Unix: SIGTERM the immediate process; descendants typically
        // inherit and die. If they don't, the test runner's job
        // control reaps them.
        let _ = child.kill();
        let _ = pid; // suppress unused warning when cfg!(not(windows))
    }
    let _ = child.wait();
}
