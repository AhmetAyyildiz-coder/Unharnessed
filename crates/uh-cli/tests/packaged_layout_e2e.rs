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

//! Kernel-packaging e2e: the packaged (exe-adjacent) layout boots a
//! session with NO `PI_PACKAGE_DIR`.
//!
//! The staged layout is the binary plus `package.json` (the version
//! manifest), the `unharnessed-runtime/` sidecar, and `skills/` beside it,
//! all resolved at runtime from the executable's directory.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The repo root (crates/uh-cli -> crates -> root): the vendored
/// unharnessed-runtime sidecar and skills live there.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .map(Path::to_path_buf)
        .expect("worktree root")
}

/// These tests stage full packaged layouts and boot the kernel; they are
/// heavy and contend on a small box, so each one holds this lock for its
/// whole body.
static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn serial_lock() -> std::sync::MutexGuard<'static, ()> {
    match TEST_LOCK.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Stage the packaged layout into `dir`: the binary, the version manifest,
/// and the shipped assets. `with_runtime` controls whether the
/// unharnessed-runtime sidecar is present (the failure-UX scenario removes
/// it).
fn stage_packaged_layout(dir: &Path, with_runtime: bool) {
    std::fs::create_dir_all(dir).expect("stage dir");
    let binary = dir.join("unharnessed");
    std::fs::copy(env!("CARGO_BIN_EXE_unharnessed"), &binary).expect("copy binary");
    set_executable(&binary);
    std::fs::write(
        dir.join("package.json"),
        format!(
            r#"{{"name":"unharnessed","version":"{}","piConfig":{{"name":"unharnessed","configDir":".prime/agent"}}}}"#,
            env!("CARGO_PKG_VERSION")
        ),
    )
    .expect("version manifest");
    for asset in ["skills", "README.md"] {
        let source = repo_root().join(asset);
        let target = dir.join(asset);
        if source.is_dir() {
            copy_dir(&source, &target);
        } else {
            std::fs::copy(&source, &target).expect("copy asset");
        }
    }
    if with_runtime {
        copy_dir(
            &repo_root().join("unharnessed-runtime"),
            &dir.join("unharnessed-runtime"),
        );
    }
}

fn copy_dir(source: &Path, target: &Path) {
    std::fs::create_dir_all(target).expect("asset target dir");
    for entry in std::fs::read_dir(source).expect("read asset dir") {
        let entry = entry.expect("asset entry");
        let path = entry.path();
        let destination = target.join(entry.file_name());
        if path.is_dir() {
            std::fs::create_dir_all(&destination).expect("asset dir");
            copy_dir(&path, &destination);
        } else {
            std::fs::copy(&path, &destination).unwrap_or_else(|error| {
                panic!(
                    "copy asset file {} -> {}: {error}",
                    path.display(),
                    destination.display()
                )
            });
        }
    }
}

#[cfg(unix)]
fn set_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = std::fs::metadata(path)
        .expect("staged binary metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).expect("staged binary permissions");
}

#[cfg(not(unix))]
fn set_executable(path: &Path) {
    let _ = path;
}

/// The kernel Python with unharnessed-runtime installed (the interpreter the
/// TS product's kernel venv bootstraps). Skipped (with a note) on machines
/// without a live install; `UH_E2E_KERNEL_PYTHON` points at an explicit one.
fn kernel_python() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("UH_E2E_KERNEL_PYTHON") {
        let explicit = PathBuf::from(explicit);
        assert!(
            explicit.exists(),
            "UH_E2E_KERNEL_PYTHON {} not found",
            explicit.display()
        );
        return Some(explicit);
    }
    let candidate = PathBuf::from(std::env::var("HOME").map_or_else(
        |_| "/home/ubuntu/.prime/agent/kernel-venv/bin/python".to_string(),
        |home| format!("{home}/.prime/agent/kernel-venv/bin/python"),
    ));
    if candidate.exists() {
        return Some(candidate);
    }
    eprintln!(
        "kernel python {} not found; skipping live kernel e2e",
        candidate.display()
    );
    None
}

struct Sandbox {
    home: tempfile::TempDir,
    agent_dir: PathBuf,
    cwd: PathBuf,
}

fn sandbox() -> Sandbox {
    let home = tempfile::TempDir::new().expect("sandbox home");
    let agent_dir = home.path().join("agent");
    let cwd = home.path().join("work");
    std::fs::create_dir_all(&agent_dir).expect("agent dir");
    std::fs::create_dir_all(&cwd).expect("cwd");
    Sandbox {
        home,
        agent_dir,
        cwd,
    }
}

impl Sandbox {
    /// The packaged binary with a hermetic environment: sandboxed HOME and
    /// agent dir, no ambient `PI_PACKAGE_DIR`, no ambient kernel-override
    /// env (`PRIME_AGENT_KERNEL_PYTHON` / `PRIME_AGENT_KERNEL_VENV` — the
    /// gate kernel-env recipe exports them for the live-kernel suites, and
    /// they must not steer the packaged layout's own resolution), no
    /// ambient API keys. A test that wants an override sets it after this
    /// call: a later `Command::env` wins over the scrub.
    fn command(&self, staged: &Path) -> Command {
        let mut command = Command::new(staged.join("unharnessed"));
        command
            .env("HOME", self.home.path())
            .env("PRIME_AGENT_CODING_AGENT_DIR", &self.agent_dir)
            .env_remove("PI_PACKAGE_DIR")
            .env_remove("PRIME_AGENT_KERNEL_PYTHON")
            .env_remove("PRIME_AGENT_KERNEL_VENV")
            .env_remove("PRIME_AGENT_SESSION_DIR")
            .env_remove("PRIME_AGENT_CODING_AGENT_SESSION_DIR")
            .env_remove("PRIME_API_KEY")
            .current_dir(&self.cwd);
        command
    }
}

/// The turn script: one ipython cell that proves the kernel runs, then the
/// closing text turn.
fn kernel_boot_script(receipt: &Path) -> serde_json::Value {
    let cell = format!(
        "import json\nfrom rlm import rlm as _r\npayload = {{\n  \"kernel_boot\": True,\n  \"rlm_available\": _r is not None,\n  \"spawn\": callable(_r.spawn),\n}}\nopen({receipt:?}, \"w\").write(json.dumps(payload))\nprint(\"KERNEL_BOOT_OK\")",
        receipt = receipt.display().to_string(),
    );
    serde_json::json!({
        "responses": [
            { "content": [ { "type": "toolCall", "name": "ipython", "arguments": {
                "code": cell,
            } } ] },
            // The next assistant message echoes the request's system prompt:
            // binary-level proof that the staged skills dir reached the
            // session's skill inventory.
            { "systemPrompt": true },
            { "text": "kernel boot verified" },
        ],
    })
}

fn run_json_turn(
    command: &mut Command,
    script: &serde_json::Value,
) -> (String, String, Option<i32>) {
    let output = command
        .arg("--mode")
        .arg("json")
        .arg("-p")
        .arg("use the kernel")
        .env("PRIME_AGENT_FAUX_SCRIPT", script.to_string())
        .output()
        .expect("run packaged binary");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.code(),
    )
}

/// A packaged session boots the kernel with the exe-adjacent layout and no
/// `PI_PACKAGE_DIR`: the ipython cell runs through the staged binary, the
/// bundled skills resolve from the staged `skills/` directory, and the
/// staged version manifest reports the pinned version.
#[test]
fn packaged_session_boots_kernel_without_pi_package_dir() {
    let _guard = serial_lock();
    let Some(kernel_python) = kernel_python() else {
        return;
    };
    let dir = tempfile::TempDir::new().expect("stage dir");
    let staged = dir.path();
    stage_packaged_layout(staged, true);
    // A marker skill that exists ONLY in the staged layout: proves the
    // bundled skills resolved exe-adjacent (not from the source checkout).
    let marker = staged.join("skills").join("staged-packaging-marker");
    std::fs::create_dir_all(&marker).expect("marker skill dir");
    std::fs::write(
        marker.join("SKILL.md"),
        "---\nname: staged-packaging-marker\ndescription: Only in the staged layout\n---\nMarker.",
    )
    .expect("marker skill");

    let box_ = sandbox();
    let receipt = box_.home.path().join("kernel-receipt.json");
    let script = kernel_boot_script(&receipt);

    let mut command = box_.command(staged);
    command.env("PRIME_AGENT_KERNEL_PYTHON", &kernel_python);
    let (stdout, stderr, code) = run_json_turn(&mut command, &script);
    assert_eq!(code, Some(0), "stdout: {stdout}\nstderr: {stderr}");
    // The ipython tool result carried the cell output.
    assert!(
        stdout.contains("KERNEL_BOOT_OK"),
        "kernel cell output missing; stdout: {stdout}\nstderr: {stderr}"
    );
    // The receipt proves the cell executed with a live rlm surface.
    let payload: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&receipt).unwrap_or_default())
            .unwrap_or(serde_json::Value::Null);
    assert_eq!(
        payload["kernel_boot"],
        serde_json::Value::Bool(true),
        "receipt: {payload}"
    );
    assert_eq!(
        payload["rlm_available"],
        serde_json::Value::Bool(true),
        "receipt: {payload}"
    );
    assert_eq!(
        payload["spawn"],
        serde_json::Value::Bool(true),
        "receipt: {payload}"
    );
    // The staged marker skill reached the session's skill inventory.
    assert!(
        stdout.contains("staged-packaging-marker"),
        "staged skill missing from the session output; stdout: {stdout}"
    );
}

/// The staged version manifest reports the pinned version (TS `--version`
/// reads the packaged package.json at runtime).
#[test]
fn packaged_binary_reports_manifest_version() {
    let _guard = serial_lock();
    let dir = tempfile::TempDir::new().expect("stage dir");
    let staged = dir.path();
    stage_packaged_layout(staged, false);
    // A re-pinned manifest: the binary must report it, not the compiled-in
    // fallback.
    std::fs::write(
        staged.join("package.json"),
        r#"{"name":"unharnessed","version":"9.8.7-test"}"#,
    )
    .expect("version manifest");
    let box_ = sandbox();
    let output = box_
        .command(staged)
        .arg("--version")
        .output()
        .expect("run packaged binary");
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "9.8.7-test",
        "the packaged manifest version must win"
    );
}

/// A missing sidecar (broken install) surfaces the actionable bootstrap
/// failure: the TS-matching base text plus the missing
/// unharnessed-runtime hint, not a raw uv/pip error.
#[test]
fn missing_sidecar_reports_actionable_bootstrap_error() {
    let _guard = serial_lock();
    let dir = tempfile::TempDir::new().expect("stage dir");
    let staged = dir.path();
    stage_packaged_layout(staged, false);
    let box_ = sandbox();
    let output = box_
        .command(staged)
        .arg("--unharnessed-bootstrap")
        // No uv anywhere the bootstrap looks (PATH and ~/.local/bin under
        // the sandboxed HOME), so the failure is the resolution error, not
        // an install attempt against the network.
        .env("PATH", "/usr/bin:/bin")
        .output()
        .expect("run packaged binary");
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Failed to set up the Python kernel runtime"),
        "TS bootstrap failure text missing: {stderr}"
    );
    assert!(
        stderr.contains("unharnessed-runtime directory was not found"),
        "missing-sidecar hint missing: {stderr}"
    );
    assert!(
        stderr.contains(staged.to_string_lossy().as_ref()),
        "the hint must name the executable directory: {stderr}"
    );
}

/// A `PRIME_AGENT_KERNEL_PYTHON` that lacks the runtime reports the
/// TS-matching override error (the existing UX path, asserted end to end
/// through the packaged binary).
#[test]
fn invalid_kernel_python_override_reports_ts_error() {
    let _guard = serial_lock();
    let dir = tempfile::TempDir::new().expect("stage dir");
    let staged = dir.path();
    stage_packaged_layout(staged, false);
    let box_ = sandbox();
    let bogus = box_.home.path().join("not-a-kernel-python");
    let output = box_
        .command(staged)
        .arg("--unharnessed-bootstrap")
        .env("PRIME_AGENT_KERNEL_PYTHON", &bogus)
        .output()
        .expect("run packaged binary");
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("PRIME_AGENT_KERNEL_PYTHON points to a Python missing"),
        "TS override error missing: {stderr}"
    );
}

/// The staged manifest version the hostile-env regression pins (the parent
/// writes it; the child asserts `--version` reports it, so the decoy
/// `PI_PACKAGE_DIR` manifest can never win).
const HOSTILE_STAGED_VERSION: &str = "9.8.7-hostile";

/// Re-exec marker: the child mode of the hostile-host-env regression test
/// below. The parent sets it (plus the hostile host env) on a re-exec of
/// this test binary; the child then exercises the staged layout through
/// the normal hermetic sandbox harness.
const HOSTILE_CHILD_STAGE: &str = "UH_PACKAGED_E2E_HOSTILE_STAGE";

/// The child half of the hostile-host-env regression: the whole test
/// process carries the hostile env — a `PRIME_AGENT_KERNEL_PYTHON` that
/// would satisfy the bootstrap if it leaked, a `PI_PACKAGE_DIR` package
/// dir with its own manifest, a decoy `PRIME_AGENT_KERNEL_VENV` — and the
/// packaged layout must still behave exactly as on a clean host.
fn hostile_child_assertions(staged: &Path) {
    let _guard = serial_lock();
    let box_ = sandbox();

    // The staged version manifest wins over the hostile PI_PACKAGE_DIR
    // decoy: --version reads the exe-adjacent package.json.
    let output = box_
        .command(staged)
        .arg("--version")
        .output()
        .expect("run packaged binary");
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        HOSTILE_STAGED_VERSION,
        "the hostile PI_PACKAGE_DIR must not win: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // The missing-sidecar bootstrap failure stays the actionable one:
    // exit 1 with the hint naming the STAGED dir — never the hostile
    // kernel-python override error and never the decoy package dir.
    let output = box_
        .command(staged)
        .arg("--unharnessed-bootstrap")
        .env("PATH", "/usr/bin:/bin")
        .output()
        .expect("run packaged binary");
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Failed to set up the Python kernel runtime"),
        "TS bootstrap failure text missing: {stderr}"
    );
    assert!(
        stderr.contains("unharnessed-runtime directory was not found"),
        "missing-sidecar hint missing: {stderr}"
    );
    assert!(
        stderr.contains(staged.to_string_lossy().as_ref()),
        "the hint must name the staged dir: {stderr}"
    );
    assert!(
        !stderr.contains("PRIME_AGENT_KERNEL_PYTHON points to"),
        "the hostile kernel-python override leaked into the packaged layout: {stderr}"
    );
    if let Some(package_decoy) = std::env::var_os("PI_PACKAGE_DIR") {
        assert!(
            !stderr.contains(Path::new(&package_decoy).to_string_lossy().as_ref()),
            "the hostile PI_PACKAGE_DIR leaked into the packaged layout: {stderr}"
        );
    }
}

/// Regression for the recurring battery finding: a gate/battery host that
/// exports the kernel-env recipe (`PRIME_AGENT_KERNEL_PYTHON` for the
/// live-kernel suites, `PI_PACKAGE_DIR` for the sidecar) leaks it into the
/// packaged layout's test env, and `missing_sidecar_reports_actionable_bootstrap_error`
/// flips (the bootstrap honors the override and exits 0 instead of 1).
/// The leak vector is env inheritance into the test process itself, so
/// this test re-execs this test binary with a hostile host env set and
/// asserts, in the child, that the staged layout behaves exactly as on a
/// clean host.
#[test]
fn packaged_layout_stays_hermetic_under_hostile_host_env() {
    if let Some(stage) = std::env::var_os(HOSTILE_CHILD_STAGE) {
        hostile_child_assertions(&PathBuf::from(stage));
        return;
    }
    let _guard = serial_lock();
    let dir = tempfile::TempDir::new().expect("stage dir");
    let staged = dir.path();
    stage_packaged_layout(staged, false);
    std::fs::write(
        staged.join("package.json"),
        format!(r#"{{"name":"unharnessed","version":"{HOSTILE_STAGED_VERSION}"}}"#),
    )
    .expect("version manifest");

    // Decoys that would win if the hostile env leaked into the packaged
    // binary: a package dir with its own manifest and a decoy kernel venv.
    let decoys = tempfile::TempDir::new().expect("decoys dir");
    let package_decoy = decoys.path().join("package");
    std::fs::create_dir_all(&package_decoy).expect("package decoy dir");
    std::fs::write(
        package_decoy.join("package.json"),
        r#"{"name":"unharnessed","version":"0.0.0-decoy"}"#,
    )
    .expect("decoy manifest");
    let venv_decoy = decoys.path().join("kernel-venv");
    std::fs::create_dir_all(venv_decoy.join("bin")).expect("venv decoy dir");
    std::fs::write(venv_decoy.join("bin").join("python"), "#!/bin/sh\nexit 0\n")
        .expect("decoy python");

    // The most hostile PRIME_AGENT_KERNEL_PYTHON is a python that would
    // satisfy the bootstrap (exit 0) if it leaked: the live kernel venv on
    // this machine when present, else a plausible bogus path (which would
    // flip the failure to the override error instead).
    let hostile_python =
        kernel_python().unwrap_or_else(|| decoys.path().join("hostile-kernel-python"));

    let child = Command::new(std::env::current_exe().expect("test binary"))
        .arg("--exact")
        .arg("packaged_layout_stays_hermetic_under_hostile_host_env")
        .env(HOSTILE_CHILD_STAGE, staged)
        .env("PRIME_AGENT_KERNEL_PYTHON", &hostile_python)
        .env("PI_PACKAGE_DIR", &package_decoy)
        .env("PRIME_AGENT_KERNEL_VENV", &venv_decoy)
        .output()
        .expect("re-exec the test binary with the hostile host env");
    assert!(
        child.status.success(),
        "the packaged layout must stay hermetic under a hostile host env; child: {}{}",
        String::from_utf8_lossy(&child.stdout),
        String::from_utf8_lossy(&child.stderr)
    );
}

/// Heavy (network + uv): the packaged sidecar bootstraps a fresh kernel venv
/// with no `PRIME_AGENT_KERNEL_PYTHON` and no `PI_PACKAGE_DIR` (both scrubbed
/// by the hermetic sandbox command, so a gate host exporting them cannot
/// steer this run), then boots a session on it. Run explicitly:
/// `cargo test -p uh-cli --test packaged_layout_e2e -- --ignored`
#[test]
#[ignore = "network + uv bootstrap (~minutes); the lane verifier runs it explicitly"]
fn bootstrap_kernel_venv_from_packaged_sidecar() {
    let dir = tempfile::TempDir::new().expect("stage dir");
    let staged = dir.path();
    stage_packaged_layout(staged, true);
    let box_ = sandbox();
    let venv = box_.home.path().join("kernel-venv");

    // --unharnessed-bootstrap (the installer handoff) builds the venv from
    // the exe-adjacent sidecar.
    let output = box_
        .command(staged)
        .arg("--unharnessed-bootstrap")
        .env("PRIME_AGENT_KERNEL_VENV", &venv)
        .output()
        .expect("run packaged binary");
    assert_eq!(
        output.status.code(),
        Some(0),
        "bootstrap failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("kernel python:"),
        "bootstrap stdout: {stdout}"
    );
    let venv_python = venv.join("bin").join("python");
    assert!(venv_python.exists(), "kernel venv python missing");

    // A session boots on the fresh venv: the same kernel-cell proof as the
    // ambient-venv test, this time without any kernel python override.
    let receipt = box_.home.path().join("kernel-receipt.json");
    let script = kernel_boot_script(&receipt);
    let mut command = box_.command(staged);
    command.env("PRIME_AGENT_KERNEL_VENV", &venv);
    let (stdout, stderr, code) = run_json_turn(&mut command, &script);
    assert_eq!(code, Some(0), "stdout: {stdout}\nstderr: {stderr}");
    assert!(
        stdout.contains("KERNEL_BOOT_OK"),
        "kernel cell output missing; stdout: {stdout}\nstderr: {stderr}"
    );
    let payload: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&receipt).unwrap_or_default())
            .unwrap_or(serde_json::Value::Null);
    assert_eq!(
        payload["kernel_boot"],
        serde_json::Value::Bool(true),
        "receipt: {payload}"
    );
}
