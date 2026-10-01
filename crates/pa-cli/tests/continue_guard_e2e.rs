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

//! End-to-end verifier for the print-mode active-session guard (B-11): a
//! headless `-c`/`-r` must refuse to open a session file whose runtime
//! lease another live process holds. The real binary and the scripted faux
//! provider drive the full path.
#![cfg(unix)]

use std::process::Command;

use serde_json::json;

fn run_print(args: &[&str], env: &[(String, String)]) -> (String, String, i32) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_prime-agent"));
    command.args(args);
    for (key, value) in env {
        command.env(key, value);
    }
    let output = command.output().expect("binary present");
    (
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
        output.status.code().unwrap_or(-1),
    )
}

/// A session file held by a live FOREIGN lease holder — this test process
/// stands in for the other product's holder on the shared session store —
/// refuses `--resume` with the session-hold refusal. No daemon runs: the
/// roster probe has nothing to answer, and the guard's lease probe is what
/// must catch a holder no roster of this product's daemon can see. The
/// guard runs before the file is opened, so the file itself only has to
/// exist (the resume selector names it directly).
#[test]
fn print_resume_refuses_a_foreign_lease_holder() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let agent_dir = dir.path().join("agent");
    let sessions = agent_dir.join("sessions");
    std::fs::create_dir_all(&sessions).expect("sessions dir");
    let env = vec![
        ("HOME".to_string(), dir.path().to_string_lossy().to_string()),
        (
            "PRIME_AGENT_CODING_AGENT_DIR".to_string(),
            agent_dir.to_string_lossy().to_string(),
        ),
        (
            "PRIME_AGENT_FAUX_SCRIPT".to_string(),
            json!({"responses": [{"text": "first turn"}]}).to_string(),
        ),
    ];
    let session_path = sessions.join("foreign-held.jsonl");
    std::fs::write(&session_path, "{}\n").expect("session file");

    // The foreign holder: this test process takes the runtime lease, in
    // exactly the role the other product's daemon worker plays on the
    // shared session store. The lease-enable env is consumed at the
    // acquire itself, so it leaves this test's window immediately.
    std::env::set_var(pa_daemon::lease::SESSION_LEASES_ENABLED_ENV, "1");
    std::env::set_var(
        pa_daemon::lease::SESSION_LEASE_OWNER_ID_ENV,
        "foreign01ab3c",
    );
    let holder = pa_daemon::lease::acquire_session_lease(Some(&session_path), &agent_dir)
        .expect("lease acquire probe")
        .expect("the lease must be held");
    std::env::remove_var(pa_daemon::lease::SESSION_LEASE_OWNER_ID_ENV);
    std::env::remove_var(pa_daemon::lease::SESSION_LEASES_ENABLED_ENV);

    // The refusal: this test process runs a build of this product (the
    // cargo test binary under /target/), so the holder classification
    // reads as another Rust build, never the TypeScript product and never
    // an unnamed process.
    // The exact actionable refusal: the classified headline, the
    // continue path with the `--daemon-socket` attach shape, the
    // take-over `kill` of this test process's pid with the holder-image
    // annotation (the pid-reuse guard: the refusal names what the kill
    // would hit, here this test binary), and the session footer (the
    // `{}\n` fixture file carries no name, so no paren).
    let holder_image = std::env::current_exe()
        .expect("own exe")
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let expected = format!(
        concat!(
            "Error: This session is currently open in another Rust build of Prime Agent ",
            "(active in foreign01ab3c) — another daemon or window of this product holds the file's ",
            "runtime lease.\n",
            "\n",
            "• Continue where you left off:\n",
            "  prime-agent --daemon-socket <socket> --resume 'foreign01ab3c'\n",
            "  (<socket> is that instance's daemon socket, from the shell where you started ",
            "it — that daemon owns this session)\n",
            "\n",
            "• Take over on this daemon:\n",
            "  kill {} # the holder is {}\n",
            "  Then retry — the file unlocks when the holder exits.\n",
            "\n",
            "Session: foreign01ab3c\n"
        ),
        std::process::id(),
        holder_image
    );
    let (stdout, stderr, code) = run_print(
        &[
            "-p",
            "--resume",
            &session_path.to_string_lossy(),
            "resume me",
        ],
        &env,
    );
    assert_eq!(code, 1);
    assert!(stdout.is_empty(), "stdout: {stdout}");
    assert_eq!(stderr, expected);

    holder.release();

    // With the holder gone the same open proceeds: the guard's lease
    // probe answers None for a released lease (the stale record is not
    // live ownership).
    let (_stdout, stderr, code) = run_print(
        &[
            "-p",
            "--resume",
            &session_path.to_string_lossy(),
            "resume me now",
        ],
        &env,
    );
    assert_eq!(
        code, 0,
        "the released holder must let the open proceed: stderr {stderr}"
    );
    assert!(!stderr.contains("This session is currently open"));
}
