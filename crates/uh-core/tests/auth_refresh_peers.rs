//! OAuth refresh across processes: several agents share one `auth.json`,
//! and a provider takes each refresh token once. Real processes (this test
//! binary run again as a child) on a real auth document; the provider is a
//! fake that refuses a refresh token it already took.
//!
//! How it can fail:
//! 1. Two agents find the credential expired and both send its refresh
//!    token: the second is refused and that agent loses its login.
//! 2. The lock is released before the new credential is written: the
//!    waiter reads the old one and sends the used token.
//! 3. The waiter does not read the document again once it has the lock:
//!    same.
//! 4. An agent that died while refreshing keeps the others waiting.
//! 5. The lock cannot be had: the agents refresh unfenced, as in 1.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use uh_core::auth::{
    AuthCredential, AuthStorage, AuthStorageBackend, AuthStorageData, FileAuthStorageBackend,
    OAuthIntegration,
};

const PROVIDER: &str = "x-peers";
const PEER_DIR: &str = "UH_AUTH_PEER_DIR";
const PEER_ROLE: &str = "UH_AUTH_PEER_ROLE";

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

fn credential(access: &str, refresh: &str, expires: i64) -> AuthCredential {
    AuthCredential::Oauth {
        access: access.into(),
        refresh: Some(refresh.into()),
        expires,
        account_id: None,
        enterprise_url: None,
        endpoint: None,
        token_endpoint: None,
        client_id: None,
        resource: None,
        issuer: None,
    }
}

/// The provider: a refresh token is good once (a `used-<token>` file marks
/// it taken, created exclusively); the request takes a while, so the agents
/// overlap.
struct SingleUseProvider(PathBuf);

impl OAuthIntegration for SingleUseProvider {
    fn api_key_for(&self, _provider: &str, credential: &AuthCredential) -> Option<String> {
        match credential {
            AuthCredential::Oauth { access, .. } => Some(access.clone()),
            _ => None,
        }
    }

    fn refresh(&self, provider: &str, credentials: &AuthStorageData) -> Option<AuthCredential> {
        let Some(AuthCredential::Oauth {
            refresh: Some(token),
            ..
        }) = credentials.credential(provider)
        else {
            return None;
        };
        std::thread::sleep(Duration::from_millis(300));
        let first_use = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(self.0.join(format!("used-{token}")));
        if first_use.is_err() {
            return None;
        }
        let pid = std::process::id();
        Some(credential(
            &format!("access-{pid}"),
            &format!("refresh-{pid}"),
            now_ms() + 3_600_000,
        ))
    }
}

/// The child side: does nothing unless run as a peer.
#[test]
fn peer() {
    let (Ok(dir), Ok(role)) = (std::env::var(PEER_DIR), std::env::var(PEER_ROLE)) else {
        return;
    };
    let dir = PathBuf::from(dir);
    let backend = Arc::new(FileAuthStorageBackend::new(dir.join("auth.json")));
    if role == "die" {
        // Take the refresh lock, say so, and wait to be killed.
        let _held = backend.hold_refresh(PROVIDER).expect("refresh lock");
        std::fs::write(dir.join("held"), "").unwrap();
        std::thread::sleep(Duration::from_secs(120));
        return;
    }
    while !dir.join("go").exists() {
        std::thread::sleep(Duration::from_millis(5));
    }
    let mut auth = AuthStorage::from_storage(backend, Arc::new(SingleUseProvider(dir.clone())));
    let key = auth.get_api_key(PROVIDER).unwrap_or_default();
    std::fs::write(dir.join(format!("key-{}", std::process::id())), key).unwrap();
}

fn spawn_peer(dir: &Path, role: &str) -> Child {
    Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "peer", "--nocapture", "--test-threads", "1"])
        .env(PEER_DIR, dir)
        .env(PEER_ROLE, role)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

fn expired_document(dir: &Path) {
    let mut data = AuthStorageData::default();
    data.insert(PROVIDER, &credential("old-access", "old-refresh", 1000));
    std::fs::write(
        dir.join("auth.json"),
        serde_json::to_string_pretty(&data.0).unwrap(),
    )
    .unwrap();
}

fn keys(dir: &Path) -> Vec<String> {
    let mut keys: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("key-"))
        .map(|entry| std::fs::read_to_string(entry.path()).unwrap())
        .collect();
    keys.sort();
    keys
}

fn used_tokens(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("used-"))
        .count()
}

#[test]
fn agents_on_one_document_refresh_once_and_share_the_new_credential() {
    let dir = tempfile::tempdir().unwrap();
    expired_document(dir.path());
    let peers: Vec<Child> = (0..4).map(|_| spawn_peer(dir.path(), "refresh")).collect();
    std::fs::write(dir.path().join("go"), "").unwrap();
    for mut peer in peers {
        assert!(peer.wait().unwrap().success());
    }
    let keys = keys(dir.path());
    assert_eq!(keys.len(), 4, "every agent answered: {keys:?}");
    assert!(
        keys.iter()
            .all(|key| key.starts_with("access-") && *key == keys[0]),
        "every agent has the one new credential: {keys:?}"
    );
    assert_eq!(used_tokens(dir.path()), 1, "one refresh token spent");
}

#[test]
fn an_agent_that_died_refreshing_does_not_hold_the_others() {
    let dir = tempfile::tempdir().unwrap();
    expired_document(dir.path());
    let mut holder = spawn_peer(dir.path(), "die");
    let deadline = Instant::now() + Duration::from_secs(30);
    while !dir.path().join("held").exists() {
        assert!(Instant::now() < deadline, "the holder took the lock");
        std::thread::sleep(Duration::from_millis(10));
    }
    holder.kill().unwrap();
    holder.wait().unwrap();
    std::fs::write(dir.path().join("go"), "").unwrap();
    let began = Instant::now();
    assert!(spawn_peer(dir.path(), "refresh").wait().unwrap().success());
    assert!(
        began.elapsed() < Duration::from_secs(10),
        "the dead holder's lock went with it ({:?})",
        began.elapsed()
    );
    assert_eq!(keys(dir.path()).len(), 1);
    assert!(keys(dir.path())[0].starts_with("access-"));
}

#[test]
fn agents_that_cannot_lock_the_refresh_do_not_spend_the_token() {
    let dir = tempfile::tempdir().unwrap();
    expired_document(dir.path());
    // The lock file cannot be opened where a directory stands.
    std::fs::create_dir(
        dir.path()
            .join(format!("auth.json.refresh-{PROVIDER}.lock")),
    )
    .unwrap();
    let peers: Vec<Child> = (0..4).map(|_| spawn_peer(dir.path(), "refresh")).collect();
    std::fs::write(dir.path().join("go"), "").unwrap();
    for mut peer in peers {
        assert!(peer.wait().unwrap().success());
    }
    let keys = keys(dir.path());
    assert_eq!(keys, vec![String::new(); 4], "no agent got a key: {keys:?}");
    assert_eq!(
        used_tokens(dir.path()),
        0,
        "the refresh token is kept for a retry"
    );
}
