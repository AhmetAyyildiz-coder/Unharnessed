//! The header + rlm-depth concern (moved with its concern): the
//! first-line header read, the depth validation, and the root depth
//! from the environment.

use super::{Path, SessionHeader};

/// Read just the header of a session file (first line).
#[must_use]
pub fn read_session_header(file_path: &Path) -> Option<SessionHeader> {
    use std::io::BufRead;
    let file = std::fs::File::open(file_path).ok()?;
    let mut first_line = String::new();
    std::io::BufReader::new(file)
        .read_line(&mut first_line)
        .ok()?;
    let wrapper: SessionHeaderWrapper = serde_json::from_str(&first_line).ok()?;
    Some(wrapper.header)
}

#[derive(serde::Deserialize)]
struct SessionHeaderWrapper {
    #[serde(flatten)]
    header: SessionHeader,
}
