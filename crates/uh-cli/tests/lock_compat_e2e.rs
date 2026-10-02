// The Tier-C/D ruling (fleet-uniform, 2026-09-28): stack-resident futures
// by design on hot paths (boxing 130 fns is allocation-churn with zero
// correctness gain); the fn-length threshold is a style gate, not
// correctness (the harness fns are intentionally linear); 64-bit targets -
// the narrowing sits at OS/protocol boundaries where the values are
// bounded (pid syscalls, epoch/elapsed milliseconds, calendar math,
// guarded parses), and checked conversions would add panic paths where
// silent wrap was deliberate (the one genuinely-suspect family, args.rs's
// parse_positive_u32 lacking its u32::MAX bound, is flagged in the lane
// dossier for the conductor).
#![allow(
    clippy::large_futures,
    clippy::too_many_lines,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]

//! Lock-convention compatibility, binary level: the lock artifact at
//! `<file>.lock` is an empty DIRECTORY (the `proper-lockfile` convention the
//! TS product and older session stores use), and a stale regular FILE there
//! (the flock artifact of pre-compat Rust builds) is healed by the Rust
//! binary, which then proceeds. Fixture agent dirs only.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

fn sandbox(prefix: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!(
        "uh-cli-lock-compat-{prefix}-{}",
        std::process::id()
    ));
    for dir in ["home", "cwd", "agent", "tmp"] {
        std::fs::create_dir_all(base.join(dir)).expect("create sandbox directory");
    }
    base
}

fn run(binary: &Path, args: &[&str], sandbox: &Path) -> (Option<i32>, String, String) {
    let output = Command::new("timeout")
        .arg("30")
        .arg(binary)
        .args(args)
        // Fixture agent dir only: never the ambient installation.
        .env("PRIME_AGENT_CODING_AGENT_DIR", sandbox.join("agent"))
        .env("HOME", sandbox.join("home"))
        .env("TMPDIR", sandbox.join("tmp"))
        .env("PI_OFFLINE", "1")
        .current_dir(sandbox.join("cwd"))
        .output()
        .expect("failed to spawn binary under test");
    (
        output.status.code(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn settings_path(sandbox: &Path) -> PathBuf {
    sandbox.join("agent").join("settings.json")
}

fn lock_path(sandbox: &Path) -> PathBuf {
    settings_path(sandbox).with_extension("json.lock")
}

/// Write a stale FILE artifact at the lock path (a pre-compat Rust flock
/// lock) with an mtime past every staleness threshold in play.
fn seed_stale_lock_file(sandbox: &Path) {
    let path = lock_path(sandbox);
    std::fs::write(&path, "pre-compat flock artifact").expect("seed stale lock file");
    age(&path);
}

/// Write a stale lock DIRECTORY artifact at the lock path: exactly what a
/// crashed TS (or Rust) holder leaves behind.
fn seed_stale_lock_dir(sandbox: &Path) {
    let path = lock_path(sandbox);
    std::fs::create_dir(&path).expect("seed stale lock directory");
    age(&path);
}

/// Set a path's mtime far past every staleness threshold in play.
fn age(path: &Path) {
    use std::os::unix::ffi::OsStrExt;
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).expect("path to cstring");
    let ancient = libc::timespec {
        tv_sec: 1_000_000_000,
        tv_nsec: 0,
    };
    let times = [ancient, ancient];
    let result = unsafe { libc::utimensat(-1, c_path.as_ptr(), times.as_ptr(), 0) };
    assert_eq!(result, 0, "aging {}", path.display());
}

fn assert_server_added(output: &str, name: &str) {
    assert!(
        output.contains(&format!("Added MCP server \"{name}\".")),
        "expected {name} to be added, stdout: {output:?}"
    );
}

fn assert_settings_has_server(sandbox: &Path, name: &str) {
    let content = std::fs::read_to_string(settings_path(sandbox)).expect("settings.json written");
    let value: serde_json::Value = serde_json::from_str(&content).expect("settings.json parses");
    assert!(
        value
            .get("mcpServers")
            .and_then(|servers| servers.get(name))
            .is_some(),
        "settings.json must contain server {name}: {content}"
    );
}

#[test]
fn rust_binary_reclaims_stale_lock_directory_artifact() {
    let rust = PathBuf::from(env!("CARGO_BIN_EXE_prime-agent"));
    let sandbox = sandbox("rs-stale-dir");
    seed_stale_lock_dir(&sandbox);
    let (exit, stdout, stderr) = run(
        &rust,
        &[
            "mcp",
            "add",
            "http-four",
            "--url",
            "https://example.invalid/mcp",
        ],
        &sandbox,
    );
    assert_eq!(exit, Some(0), "stdout: {stdout:?}, stderr: {stderr:?}");
    assert_server_added(&stdout, "http-four");
    assert_settings_has_server(&sandbox, "http-four");
    assert!(
        !lock_path(&sandbox).exists(),
        "the released lock directory must be gone"
    );
}

#[test]
fn rust_binary_heals_stale_lock_file_artifact() {
    // Pre-compat Rust builds left flock FILEs at the lock path. The Rust
    // binary removes the foreign artifact and proceeds; the TS binary cannot.
    let rust = PathBuf::from(env!("CARGO_BIN_EXE_prime-agent"));
    let sandbox = sandbox("rs-stale-file");
    seed_stale_lock_file(&sandbox);
    let (exit, stdout, stderr) = run(
        &rust,
        &[
            "mcp",
            "add",
            "http-five",
            "--url",
            "https://example.invalid/mcp",
        ],
        &sandbox,
    );
    assert_eq!(exit, Some(0), "stdout: {stdout:?}, stderr: {stderr:?}");
    assert_server_added(&stdout, "http-five");
    assert_settings_has_server(&sandbox, "http-five");
    assert!(
        !lock_path(&sandbox).exists(),
        "the healed artifact must be gone; release removes the lock directory"
    );
}
