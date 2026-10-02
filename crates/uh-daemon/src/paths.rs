//! Filesystem helpers for the session lease guard.

use std::path::Path;
use std::time::{Duration, SystemTime};

/// Age of a file's mtime, for stale-lease detection.
#[must_use]
pub fn mtime_age(path: &Path) -> Option<Duration> {
    let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok()?;
    SystemTime::now().duration_since(modified).ok()
}
