//! The `resolve()` method on the resolution context: the settings top-level
//! arrays and auto-discovery.

use std::path::{Path, PathBuf};

use super::super::source::SourceScope;
use super::super::ResourceResolver;

use super::discovery::collect_resource_files;
use super::patterns::{apply_patterns, split_patterns};
use super::{
    settings_array, to_resolved_paths, top_level_metadata, MetadataSource, PathMetadata,
    ResolvedPaths, ResourceAccumulator, ResourceType, RESOURCE_TYPES,
};

/// The same directory, links resolved (as given when one does not exist).
pub(crate) fn same_dir(a: &Path, b: &Path) -> bool {
    let real = |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    real(a) == real(b)
}

/// Files or directory trees behind a list of paths: files pass through,
/// directories are collected per resource kind; missing paths are skipped.
pub(crate) fn collect_files_from_paths(
    paths: &[PathBuf],
    resource_type: ResourceType,
) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for path in paths {
        let Ok(stats) = std::fs::metadata(path) else {
            continue;
        };
        if stats.is_file() {
            files.push(path.clone());
        } else if stats.is_dir() {
            files.extend(collect_resource_files(path, resource_type));
        }
    }
    files
}

impl ResourceResolver {
    /// Resolve every session resource path: the settings top-level arrays,
    /// then auto-discovery.
    #[must_use]
    pub fn resolve(&self) -> ResolvedPaths {
        let global_base_dir = self.agent_dir().to_path_buf();
        let project_base_dir = self.cwd().join(super::super::CONFIG_DIR_NAME);
        let global = self.settings().global_settings().clone();
        // With the home folder as cwd, the project's config dir is the agent
        // dir itself: no project of its own, its skills and settings are the
        // user's.
        let project = if same_dir(&project_base_dir, &global_base_dir) {
            crate::settings::Settings::default()
        } else {
            self.settings().project_settings().clone()
        };
        let mut accumulator = ResourceAccumulator::default();

        for resource_type in RESOURCE_TYPES {
            let project_entries = settings_array(&project, resource_type);
            let global_entries = settings_array(&global, resource_type);
            Self::resolve_local_entries(
                &project_entries,
                resource_type,
                &mut accumulator,
                &top_level_metadata(MetadataSource::Local, SourceScope::Project),
                &project_base_dir,
            );
            Self::resolve_local_entries(
                &global_entries,
                resource_type,
                &mut accumulator,
                &top_level_metadata(MetadataSource::Local, SourceScope::User),
                &global_base_dir,
            );
        }

        self.add_auto_discovered_resources(
            &mut accumulator,
            &global,
            &project,
            &global_base_dir,
            &project_base_dir,
        );

        to_resolved_paths(accumulator)
    }

    /// Settings top-level array entries: plain paths relative to the
    /// settings base, pattern entries applied as filters.
    fn resolve_local_entries(
        entries: &[String],
        resource_type: ResourceType,
        accumulator: &mut ResourceAccumulator,
        metadata: &PathMetadata,
        base_dir: &Path,
    ) {
        if entries.is_empty() {
            return;
        }
        let (plain, pattern_entries) = split_patterns(entries);
        let resolved_plain: Vec<PathBuf> = plain
            .iter()
            .map(|path| ResourceResolver::resolve_path_from_base(path, base_dir))
            .collect();
        let all_files = collect_files_from_paths(&resolved_plain, resource_type);
        let enabled_paths = apply_patterns(&all_files, &pattern_entries, base_dir);
        for file in all_files {
            let enabled = enabled_paths.contains(&file);
            accumulator
                .map(resource_type)
                .add(&file, metadata.clone(), enabled);
        }
    }
}
