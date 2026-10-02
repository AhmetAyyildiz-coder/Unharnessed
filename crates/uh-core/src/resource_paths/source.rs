//! Git URL parsing (the session git context identifies its remote with it)
//! and the path helpers resource resolution uses.
//!
//! Git sources are explicit protocol URLs (`https://`, `http://`, `ssh://`,
//! `git://`) or - with the `git:` prefix - host/path shorthand and
//! scp-like forms.

use std::path::{Path, PathBuf};

/// Scope a resolved resource belongs to: user settings, project settings, or
/// an ephemeral scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SourceScope {
    User,
    Project,
    Temporary,
}

/// A parsed git source (subset of the hosted-git-info forms; the generic
/// fallback covers every host containing a dot or `localhost`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitSource {
    /// Clone URL, always valid for `git clone` (no ref suffix).
    pub repo: String,
    /// Git host domain (`github.com`).
    pub host: String,
    /// Repository path (`user/repo`).
    pub path: String,
    /// Optional ref (branch, tag, or commit).
    pub r#ref: Option<String>,
    /// True when a ref was specified.
    pub pinned: bool,
}

/// Split a possibly ref-suffixed git URL into clone URL and ref, matching the
/// three URL shapes the product accepts (scp-like, protocol, shorthand).
fn split_ref(url: &str) -> (String, Option<String>) {
    // scp-like: git@host:path[@ref]
    if let Some(rest) = url.strip_prefix("git@") {
        if let Some((host, path_with_ref)) = rest.split_once(':') {
            if let Some((repo_path, r)) = split_ref_after(path_with_ref) {
                return (format!("git@{host}:{repo_path}"), Some(r.to_string()));
            }
            return (url.to_string(), None);
        }
    }

    // protocol URLs: ref is the segment after '@' in the path
    if let Some((prefix, path)) = split_protocol_url(url) {
        let path_with_ref = path.trim_start_matches('/');
        if let Some((repo_path, r)) = split_ref_after(path_with_ref) {
            return (format!("{prefix}/{repo_path}"), Some(r.to_string()));
        }
        return (url.to_string(), None);
    }

    // shorthand: host/path[@ref]
    if let Some((host, path_with_ref)) = url.split_once('/') {
        if let Some((repo_path, r)) = split_ref_after(path_with_ref) {
            return (format!("{host}/{repo_path}"), Some(r.to_string()));
        }
    }

    (url.to_string(), None)
}

/// Split a protocol URL into `scheme://authority` and the path (the path
/// separator is kept in the prefix so URLs can be rebuilt by appending).
fn split_protocol_url(url: &str) -> Option<(&str, &str)> {
    let scheme_end = url.find("://")?;
    let path_start = url[scheme_end + 3..]
        .find('/')
        .map(|offset| scheme_end + 3 + offset)?;
    Some((&url[..path_start], &url[path_start..]))
}

/// Split `path[@ref]` at the first `@`; an empty side keeps the URL intact.
fn split_ref_after(path: &str) -> Option<(&str, &str)> {
    let index = path.find('@')?;
    let (repo, r) = (&path[..index], &path[index + 1..]);
    if repo.is_empty() || r.is_empty() {
        return None;
    }
    Some((repo, r))
}

/// A known git host recognized from short forms, mirroring the hosted-git-info
/// domain table the TS product uses (`github.com`, `gitlab.com`,
/// `bitbucket.org`, `gist.github.com`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct HostedInfo {
    domain: String,
    user: String,
    project: String,
    committish: Option<String>,
}

const HOSTED_SHORTCUTS: &[(&str, &str)] = &[
    ("github:", "github.com"),
    ("gitlab:", "gitlab.com"),
    ("bitbucket:", "bitbucket.org"),
    ("gist:", "gist.github.com"),
];

/// Recognized hosted domains and the `www.`/`git.` prefixes they tolerate.
const HOSTED_DOMAINS: &[(&str, &str)] = &[
    ("github.com", "github.com"),
    ("www.github.com", "github.com"),
    ("git.github.com", "github.com"),
    ("gist.github.com", "gist.github.com"),
    ("gitlab.com", "gitlab.com"),
    ("www.gitlab.com", "gitlab.com"),
    ("bitbucket.org", "bitbucket.org"),
    ("www.bitbucket.org", "bitbucket.org"),
];

/// Resolve a URL to a known hosted domain, or `None` for everything else.
/// Supports `#committish` suffixes and `git+` URL prefixes, which is what the
/// TS candidate loop relies on.
fn hosted_from_url(candidate: &str) -> Option<HostedInfo> {
    let (url_part, committish) = match candidate.split_once('#') {
        Some((url, c)) => (url, Some(c.to_string())),
        None => (candidate, None),
    };

    // Shortcut forms: github:user/repo
    for (prefix, domain) in HOSTED_SHORTCUTS {
        if let Some(rest) = url_part.strip_prefix(prefix) {
            let (user, project) = rest.split_once('/')?;
            if user.is_empty() || project.is_empty() {
                return None;
            }
            return Some(HostedInfo {
                domain: (*domain).to_string(),
                user: user.to_string(),
                project: project.trim_end_matches(".git").to_string(),
                committish,
            });
        }
    }

    let url = url_part.strip_prefix("git+").unwrap_or(url_part);
    let (prefix, path) = split_protocol_url(url)?;
    let authority = &prefix[prefix.find("://")? + 3..];
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let host = host.split(':').next().unwrap_or(host);
    for (domain, canonical) in HOSTED_DOMAINS {
        if host == *domain {
            let (user, project) = path.split_once('/')?;
            if user.is_empty() || project.is_empty() {
                return None;
            }
            return Some(HostedInfo {
                domain: (*canonical).to_string(),
                user: user.to_string(),
                project: project.trim_end_matches(".git").to_string(),
                committish,
            });
        }
    }
    None
}

/// Generic git URL parser: scp-like, protocol, and `host/path` shorthand forms
/// where the host contains a dot or is `localhost`.
fn parse_generic_git_url(url: &str) -> Option<GitSource> {
    let (repo, r) = split_ref(url);
    let repo = repo.as_str();

    let (host, path, clone_url) = if let Some(rest) = repo.strip_prefix("git@") {
        let (host, path) = rest.split_once(':')?;
        (host.to_string(), path.to_string(), repo.to_string())
    } else if is_protocol_url(repo) {
        let (prefix, path) = split_protocol_url(repo)?;
        let authority = &prefix[prefix.find("://").expect("checked above") + 3..];
        let host = authority
            .rsplit_once('@')
            .map_or(authority, |(_, host)| host);
        let host = host.split(':').next().unwrap_or(host);
        (
            host.to_string(),
            path.trim_start_matches('/').to_string(),
            repo.to_string(),
        )
    } else {
        let (host, path) = repo.split_once('/')?;
        if !host.contains('.') && host != "localhost" {
            return None;
        }
        (
            host.to_string(),
            path.to_string(),
            format!("https://{repo}"),
        )
    };

    let path = path
        .trim_end_matches(".git")
        .trim_start_matches('/')
        .to_string();
    if host.is_empty() || path.is_empty() || path.split('/').count() < 2 {
        return None;
    }

    Some(GitSource {
        repo: clone_url,
        host,
        path,
        r#ref: r.clone(),
        pinned: r.is_some(),
    })
}

/// Parse a source string into a git source.
///
/// Rules: with the `git:` prefix every historical shorthand form is accepted;
/// without it only explicit protocol URLs parse as git.
#[must_use]
pub fn parse_git_url(source: &str) -> Option<GitSource> {
    let trimmed = source.trim();
    let has_git_prefix = trimmed.starts_with("git:");
    let url = if has_git_prefix {
        trimmed.strip_prefix("git:")?.trim()
    } else {
        trimmed
    };

    if !has_git_prefix && !is_protocol_url(url) {
        return None;
    }

    let (split_repo, split_ref) = split_ref(url);

    // Hosted candidates first (carries the ref as #committish), then the raw
    // URL, matching the TS candidate order.
    let candidates = [
        split_ref.as_ref().map(|r| format!("{split_repo}#{r}")),
        Some(url.to_string()),
    ];
    for candidate in candidates.into_iter().flatten() {
        if let Some(info) = hosted_from_url(&candidate) {
            if split_ref.is_some() && info.project.contains('@') {
                continue;
            }
            let https_prefixed = !split_repo.starts_with("http://")
                && !split_repo.starts_with("https://")
                && !split_repo.starts_with("ssh://")
                && !split_repo.starts_with("git://")
                && !split_repo.starts_with("git@");
            return Some(GitSource {
                repo: if https_prefixed {
                    format!("https://{split_repo}")
                } else {
                    split_repo
                },
                host: info.domain,
                path: format!("{}/{}", info.user, info.project),
                r#ref: info.committish.clone().or_else(|| split_ref.clone()),
                pinned: info.committish.is_some() || split_ref.is_some(),
            });
        }
    }

    parse_generic_git_url(url)
}

/// `https?://`, `ssh://`, or `git://` prefix check (the TS gate for
/// non-`git:`-prefixed URLs).
fn is_protocol_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.starts_with("https://")
        || lower.starts_with("http://")
        || lower.starts_with("ssh://")
        || lower.starts_with("git://")
}

/// Lexical absolutization (node `path.resolve`): joins onto the base and
/// collapses `.`/`..` without touching the filesystem.
pub fn lexical_resolve(base: &Path, input: &str) -> PathBuf {
    let path = Path::new(input);
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    };
    let mut out = PathBuf::new();
    for component in joined.components() {
        match component {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// node `path.relative` over pre-resolved absolute paths.
pub fn path_relative(from: &Path, to: &Path) -> String {
    let from: Vec<_> = from.components().collect();
    let to: Vec<_> = to.components().collect();
    let mut shared = 0;
    while shared < from.len() && shared < to.len() && from[shared] == to[shared] {
        shared += 1;
    }
    let mut segments: Vec<String> = Vec::new();
    for _ in shared..from.len() {
        segments.push("..".to_string());
    }
    for component in &to[shared..] {
        segments.push(component.as_os_str().to_string_lossy().into_owned());
    }
    segments.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_https_github_urls() {
        let git = parse_git_url("https://github.com/user/repo").expect("expected git");
        assert_eq!(git.host, "github.com");
        assert_eq!(git.path, "user/repo");
        assert_eq!(git.repo, "https://github.com/user/repo");
        assert!(!git.pinned);
    }

    #[test]
    fn parses_https_urls_with_git_prefix() {
        let git = parse_git_url("git:https://github.com/user/repo").expect("expected git");
        assert_eq!(git.host, "github.com");
        assert_eq!(git.path, "user/repo");
    }

    #[test]
    fn parses_https_urls_with_ref() {
        let git = parse_git_url("https://github.com/user/repo@v1.2.3").expect("expected git");
        assert_eq!(git.repo, "https://github.com/user/repo");
        assert_eq!(git.r#ref.as_deref(), Some("v1.2.3"));
        assert!(git.pinned);

        let git =
            parse_git_url("https://github.com/user/repo@feature/branch").expect("expected git");
        assert_eq!(git.r#ref.as_deref(), Some("feature/branch"));
    }

    #[test]
    fn parses_host_path_shorthand_only_with_git_prefix() {
        let git = parse_git_url("git:github.com/user/repo").expect("expected git");
        assert_eq!(git.repo, "https://github.com/user/repo");
        assert_eq!(git.host, "github.com");
        assert_eq!(git.path, "user/repo");

        assert!(parse_git_url("github.com/user/repo").is_none());
    }

    #[test]
    fn parses_https_urls_with_git_suffix() {
        let git = parse_git_url("https://github.com/user/repo.git").expect("expected git");
        assert_eq!(git.path, "user/repo");
    }

    #[test]
    fn parses_other_hosted_domains() {
        for url in [
            "https://gitlab.com/user/repo",
            "https://bitbucket.org/user/repo",
            "https://codeberg.org/user/repo",
        ] {
            let git = parse_git_url(url).expect("expected git for {url}");
            assert_eq!(git.path, "user/repo");
        }
    }

    #[test]
    fn keeps_scp_clone_urls_for_scp_like_sources() {
        let git = parse_git_url("git:git@github.com:user/repo").expect("expected git");
        assert_eq!(git.repo, "git@github.com:user/repo");
        assert_eq!(git.host, "github.com");
        assert_eq!(git.path, "user/repo");
    }

    #[test]
    fn parses_ssh_protocol_urls() {
        let git = parse_git_url("git:ssh://git@github.com/user/repo").expect("expected git");
        assert_eq!(git.repo, "ssh://git@github.com/user/repo");
        assert_eq!(git.host, "github.com");
        assert_eq!(git.path, "user/repo");
    }

    #[test]
    fn resolves_paths_lexically() {
        let base = Path::new("/work/project");
        assert_eq!(
            lexical_resolve(base, "sub/pkg"),
            PathBuf::from("/work/project/sub/pkg")
        );
        assert_eq!(lexical_resolve(base, "../x"), PathBuf::from("/work/x"));
        assert_eq!(lexical_resolve(base, "/abs"), PathBuf::from("/abs"));
        assert_eq!(path_relative(base, Path::new("/work")), "..");
        assert_eq!(path_relative(base, base), "");
    }
}
