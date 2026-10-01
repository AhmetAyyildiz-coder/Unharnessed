//! `ResourceResolver`: the session-resource resolution context (cwd, agent
//! dir, settings, bundled skills). The package-source install/remove/update
//! surface was removed with the `packages` setting.

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::settings::SettingsManager;

/// Built-in skills directory selection: the packaged layout, disabled
/// entirely, or an explicit directory.
#[derive(Debug, Clone, Default)]
pub enum BundledSkillsDir {
    /// Exe-adjacent `skills/` (the packaged layout).
    #[default]
    Packaged,
    /// Built-in skills disabled (tests, embedded hosts).
    Disabled,
    /// An explicit directory.
    Directory(PathBuf),
}

/// Construction options for [`ResourceResolver`].
pub struct ResourceResolverOptions {
    pub cwd: PathBuf,
    pub agent_dir: PathBuf,
    pub settings: SettingsManager,
    /// Built-in skills directory (default: the packaged layout).
    pub bundled_skills_dir: BundledSkillsDir,
    /// Extra force-exclude patterns for built-in skills (e.g. `-<server>/SKILL.md`
    /// overrides from integrations the user is not logged into).
    pub extra_builtin_skill_overrides: Vec<String>,
}

/// The resolution context: owns a settings manager and resolves session
/// resources.
pub struct ResourceResolver {
    cwd: PathBuf,
    agent_dir: PathBuf,
    settings: SettingsManager,
    /// `None` disables built-in skills entirely.
    bundled_skills_dir: Option<PathBuf>,
    extra_builtin_skill_overrides: Vec<String>,
}

impl ResourceResolver {
    #[must_use]
    pub fn with_options(options: ResourceResolverOptions) -> Self {
        let bundled_skills_dir = match options.bundled_skills_dir {
            BundledSkillsDir::Packaged => Some(super::get_bundled_skills_dir()),
            BundledSkillsDir::Disabled => None,
            BundledSkillsDir::Directory(dir) => Some(dir),
        };
        Self {
            cwd: options.cwd,
            agent_dir: options.agent_dir,
            settings: options.settings,
            bundled_skills_dir,
            extra_builtin_skill_overrides: options.extra_builtin_skill_overrides,
        }
    }

    #[must_use]
    pub fn settings(&self) -> &SettingsManager {
        &self.settings
    }

    /// Reload both settings scopes from storage (resolve reads live settings).
    ///
    /// # Errors
    ///
    /// Returns an error when the settings storage cannot be reloaded.
    pub fn reload_settings(&mut self) -> Result<()> {
        self.settings.reload()
    }

    pub(super) fn cwd(&self) -> &std::path::Path {
        &self.cwd
    }

    pub(super) fn agent_dir(&self) -> &std::path::Path {
        &self.agent_dir
    }

    /// Built-in skills directory; `None` when disabled.
    pub(super) fn bundled_skills_dir(&self) -> Option<&PathBuf> {
        self.bundled_skills_dir.as_ref()
    }

    /// Settings flag: load built-in skills (default true).
    pub(super) fn enable_builtin_skills(&self) -> bool {
        self.settings
            .settings()
            .enable_builtin_skills
            .unwrap_or(true)
    }

    /// Extra force-exclude patterns for built-in skills.
    pub(super) fn extra_builtin_skill_overrides(&self) -> &[String] {
        &self.extra_builtin_skill_overrides
    }

    pub(super) fn resolve_path_from_base(input: &str, base: &Path) -> PathBuf {
        let trimmed = input.trim();
        if let Some(path) = expand_tilde(trimmed) {
            return path;
        }
        super::source::lexical_resolve(base, trimmed)
    }
}

/// Expand `~`/`~/` against the home directory (a tilde path is absolute).
fn expand_tilde(input: &str) -> Option<PathBuf> {
    let home = pa_types::platform::home_dir()?;
    if input == "~" {
        return Some(home);
    }
    input
        .strip_prefix("~/")
        .map(|rest| home.join(rest))
        .or_else(|| input.strip_prefix('~').map(|rest| home.join(rest)))
}
