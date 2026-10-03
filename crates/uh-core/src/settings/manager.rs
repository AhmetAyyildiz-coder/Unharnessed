//! `SettingsManager`: loads global + project settings, merges them, tracks
//! modified fields, and writes back only what this session changed.

use std::sync::Arc;

use anyhow::Result;

use super::load::from_value_lenient;
use super::merge::{deep_merge, migrate};
use super::storage::{SettingsScope, SettingsStorage};
use super::types::{QueueModeSetting, Settings, ThinkingLevelSetting};

#[derive(Debug, Clone)]
pub struct SettingsError {
    pub scope: SettingsScope,
    pub message: String,
}

pub struct SettingsManager {
    storage: Arc<dyn SettingsStorage>,
    global: Settings,
    project: Settings,
    merged: Settings,
    runtime_overrides: Settings,
    errors: Vec<SettingsError>,
    /// The raw global document (the parsed JSON before the lenient field
    /// load). The daemon model allowlist distinguishes an ABSENT
    /// `allowedModels` key (unrestricted) from a PRESENT-but-malformed one
    /// (the security gate fails closed) — the typed `Settings` drops both
    /// to `None`, so the raw value is the only witness.
    global_raw: Option<serde_json::Value>,
    /// Load failures per scope; a scope whose file failed to parse is never
    /// written back (the TS `save` guard against clobbering bad settings).
    global_load_error: Option<String>,
    project_load_error: Option<String>,
}

impl SettingsManager {
    /// Load global + project settings from a storage backend.
    pub fn from_storage(storage: Arc<dyn SettingsStorage>) -> Self {
        let mut errors = Vec::new();
        let (global, global_raw, global_load_error) =
            load_scope(storage.as_ref(), SettingsScope::Global, &mut errors);
        let (project, _, project_load_error) =
            load_scope(storage.as_ref(), SettingsScope::Project, &mut errors);
        let merged = deep_merge(&global, &project);
        Self {
            storage,
            global,
            project,
            merged,
            runtime_overrides: Settings::default(),
            global_raw,
            errors,
            global_load_error,
            project_load_error,
        }
    }

    /// File-backed manager (agentDir + cwd/.unharnessed).
    pub fn create(
        cwd: impl AsRef<std::path::Path>,
        agent_dir: impl AsRef<std::path::Path>,
    ) -> Self {
        let cwd = std::path::PathBuf::from(cwd.as_ref());
        let agent_dir = std::path::PathBuf::from(agent_dir.as_ref());
        let storage: Arc<dyn SettingsStorage> =
            Arc::new(super::storage::FileSettingsStorage::new(cwd, agent_dir));
        Self::from_storage(storage)
    }

    /// In-memory manager (tests, embedded hosts).
    #[must_use]
    pub fn in_memory(initial: &Settings) -> Self {
        let storage: Arc<dyn SettingsStorage> =
            Arc::new(super::storage::InMemorySettingsStorage::default());
        let content = serde_json::to_string_pretty(&initial).unwrap_or_default();
        storage
            .with_lock(SettingsScope::Global, &mut |current| {
                let _ = current;
                Some(content.clone())
            })
            .ok();
        Self::from_storage(storage)
    }

    /// Effective (global + project) settings.
    #[must_use]
    pub fn settings(&self) -> &Settings {
        &self.merged
    }

    #[must_use]
    pub fn global_settings(&self) -> &Settings {
        &self.global
    }

    /// The raw global document (post-migration, pre-lenient-load value);
    /// `None` when the scope has no document or it failed to parse (the
    /// load error covers the latter).
    #[must_use]
    pub fn global_raw(&self) -> Option<&serde_json::Value> {
        self.global_raw.as_ref()
    }

    #[must_use]
    pub fn project_settings(&self) -> &Settings {
        &self.project
    }

    #[must_use]
    pub fn errors(&self) -> &[SettingsError] {
        &self.errors
    }

    /// Take and clear the recorded settings errors (warnings are printed once
    /// by the CLI commands that surface them).
    pub fn drain_errors(&mut self) -> Vec<SettingsError> {
        std::mem::take(&mut self.errors)
    }

    /// Reload both scopes from storage.
    ///
    /// # Errors
    ///
    /// The current implementation never returns `Err`; scope load problems
    /// are recorded as load errors on the manager instead.
    pub fn reload(&mut self) -> Result<()> {
        let mut errors = std::mem::take(&mut self.errors);
        let (global, global_raw, global_load_error) =
            load_scope(self.storage.as_ref(), SettingsScope::Global, &mut errors);
        let (project, _, project_load_error) =
            load_scope(self.storage.as_ref(), SettingsScope::Project, &mut errors);
        self.global = global;
        self.project = project;
        self.global_raw = global_raw;
        self.global_load_error = global_load_error;
        self.project_load_error = project_load_error;
        self.errors = errors;
        self.merged = deep_merge(&self.global, &self.project);
        Ok(())
    }

    /// Runtime overrides layered on top (CLI flags); not persisted.
    pub fn apply_overrides(&mut self, overrides: &Settings) {
        self.merged = deep_merge(&self.merged, overrides);
        self.runtime_overrides = deep_merge(&self.runtime_overrides, overrides);
    }

    /// Mutable global settings for the setters in sibling modules (the
    /// TS setters mutate `globalSettings` then save).
    pub(crate) fn global_mut(&mut self) -> &mut Settings {
        &mut self.global
    }

    /// Persist the global scope (sibling-module setters share this).
    ///
    /// # Errors
    ///
    /// Returns an error when the global settings file cannot be written.
    pub(crate) fn save_global_scope(&mut self) -> Result<()> {
        self.save_global()
    }

    // -- persisted setters --------------------------------------------------

    /// `defaultProvider` setter.
    ///
    /// # Errors
    ///
    /// Returns an error when the global settings file cannot be written.
    pub fn set_default_provider(&mut self, provider: String) -> Result<()> {
        self.global.default_provider = Some(provider);
        self.save_global()
    }

    /// `defaultModel` setter.
    ///
    /// # Errors
    ///
    /// Returns an error when the global settings file cannot be written.
    pub fn set_default_model(&mut self, model: String) -> Result<()> {
        self.global.default_model = Some(model);
        self.save_global()
    }

    /// `defaultModel` + `defaultProvider` setter (the model switch's
    /// combined write).
    ///
    /// # Errors
    ///
    /// Returns an error when the global settings file cannot be written.
    pub fn set_default_model_and_provider(&mut self, provider: &str, model: &str) -> Result<()> {
        self.global.default_provider = Some(provider.to_string());
        self.global.default_model = Some(model.to_string());
        self.save_global()
    }

    /// `steeringMode` setter.
    ///
    /// # Errors
    ///
    /// Returns an error when the global settings file cannot be written.
    pub fn set_steering_mode(&mut self, mode: QueueModeSetting) -> Result<()> {
        self.global.steering_mode = Some(mode);
        self.save_global()
    }

    /// `followUpMode` setter.
    ///
    /// # Errors
    ///
    /// Returns an error when the global settings file cannot be written.
    pub fn set_follow_up_mode(&mut self, mode: QueueModeSetting) -> Result<()> {
        self.global.follow_up_mode = Some(mode);
        self.save_global()
    }

    /// `defaultThinkingLevel` setter.
    ///
    /// # Errors
    ///
    /// Returns an error when the global settings file cannot be written.
    pub fn set_default_thinking_level(&mut self, level: ThinkingLevelSetting) -> Result<()> {
        self.global.default_thinking_level = Some(level);
        self.save_global()
    }

    /// TS `setRetryEnabled`: the auto-retry toggle the provider retry
    /// policy reads (`retry.enabled`).
    ///
    /// # Errors
    ///
    /// Returns an error when the global settings file cannot be written.
    pub fn set_retry_enabled(&mut self, enabled: bool) -> Result<()> {
        self.global
            .retry
            .get_or_insert_with(Default::default)
            .enabled = Some(enabled);
        self.save_global()
    }

    /// TS `setCompactionEnabled`: the auto-compaction toggle
    /// (`compaction.enabled` in the global settings file). The connection
    /// state's `autoCompactionEnabled` is this value in TS, so a daemon
    /// restart re-seeds the flag from the persisted setting.
    ///
    /// # Errors
    ///
    /// Returns an error when the global settings file cannot be written.
    pub fn set_compaction_enabled(&mut self, enabled: bool) -> Result<()> {
        self.global
            .compaction
            .get_or_insert_with(Default::default)
            .enabled = Some(enabled);
        self.save_global()
    }

    /// TS `getCompactionEnabled`: the auto-compaction toggle, on until the
    /// user opts out (the merged view, like every TS settings getter).
    #[must_use]
    pub fn get_compaction_enabled(&self) -> bool {
        self.merged
            .compaction
            .as_ref()
            .and_then(|compaction| compaction.enabled)
            .unwrap_or(true)
    }

    /// Replace one resource-path array (`skills`/`prompts`) in the
    /// global settings file (TS `setSkillPaths` & friends).
    pub fn set_global_resource_array(&mut self, field: &str, values: Vec<String>) {
        let array: Vec<serde_json::Value> =
            values.into_iter().map(serde_json::Value::String).collect();
        match field {
            "skills" => self.global.skills = Some(strings(&array)),
            "prompts" => self.global.prompts = Some(strings(&array)),
            _ => return,
        }
        self.persist_scope_field(
            SettingsScope::Global,
            field,
            &serde_json::Value::Array(array),
        );
        self.merged = deep_merge(&self.global, &self.project);
    }

    /// Replace one resource-path array in the project settings file (TS
    /// `setProjectSkillPaths` & friends).
    pub fn set_project_resource_array(&mut self, field: &str, values: Vec<String>) {
        let array: Vec<serde_json::Value> =
            values.into_iter().map(serde_json::Value::String).collect();
        match field {
            "skills" => self.project.skills = Some(strings(&array)),
            "prompts" => self.project.prompts = Some(strings(&array)),
            _ => return,
        }
        self.persist_scope_field(
            SettingsScope::Project,
            field,
            &serde_json::Value::Array(array),
        );
        self.merged = deep_merge(&self.global, &self.project);
    }

    /// Write one field into a scope's file, merging with the current on-disk
    /// document so concurrently-added fields survive. Settings failures are
    /// recorded as warnings, never thrown (the TS save contract).
    fn persist_scope_field(
        &mut self,
        scope: SettingsScope,
        field: &str,
        value: &serde_json::Value,
    ) {
        let load_error = match scope {
            SettingsScope::Global => self.global_load_error.clone(),
            SettingsScope::Project => self.project_load_error.clone(),
        };
        if let Some(message) = load_error {
            let label = match scope {
                SettingsScope::Global => "Global",
                SettingsScope::Project => "Project",
            };
            self.errors.push(SettingsError {
                scope,
                message: format!(
                    "{label} settings not saved: settings file failed to parse: {message}"
                ),
            });
            return;
        }
        let result = self.storage.with_lock(scope, &mut |current| {
            let mut map: serde_json::Map<String, serde_json::Value> = current
                .and_then(|content| serde_json::from_str::<serde_json::Value>(&content).ok())
                .and_then(|value| match value {
                    serde_json::Value::Object(mut map) => {
                        super::merge::migrate(&mut map);
                        Some(map)
                    }
                    _ => None,
                })
                .unwrap_or_default();
            map.insert(field.to_string(), value.clone());
            serde_json::to_string_pretty(&serde_json::Value::Object(map)).ok()
        });
        if let Err(error) = result {
            self.errors.push(SettingsError {
                scope,
                message: error.to_string(),
            });
        }
    }

    /// Set one value at `path` (`["compaction", "enabled"]`) in the global
    /// settings file, or remove it with `None` (the key then reads its
    /// default; parents left empty go too). The rest of the file, unknown
    /// keys included, is kept as it is on disk, less its `null`s (unset
    /// keys). The settings screen's write.
    ///
    /// # Errors
    ///
    /// Returns an error when the global file failed to parse (it is never
    /// overwritten then), is no longer a JSON object, or cannot be written.
    pub fn set_global_value(
        &mut self,
        path: &[&str],
        value: Option<&serde_json::Value>,
    ) -> Result<()> {
        let (last, parents) = path
            .split_last()
            .ok_or_else(|| anyhow::anyhow!("empty settings path"))?;
        if let Some(message) = &self.global_load_error {
            anyhow::bail!("the settings file failed to parse: {message}");
        }
        let mut refused: Option<String> = None;
        self.storage
            .with_lock(SettingsScope::Global, &mut |current| {
                let mut document = match current.as_deref().map(serde_json::from_str) {
                    None => serde_json::Map::new(),
                    Some(Ok(serde_json::Value::Object(map))) => map,
                    Some(_) => {
                        refused = Some("the settings file is not a JSON object".to_string());
                        return None;
                    }
                };
                migrate(&mut document);
                match value {
                    Some(value) => insert_path(&mut document, parents, last, value),
                    None => remove_path(&mut document, parents, last),
                }
                // A `null` is an unset key: the file only holds what is set.
                serde_json::to_string_pretty(&without_nulls(serde_json::Value::Object(document)))
                    .ok()
            })?;
        if let Some(message) = refused {
            anyhow::bail!(message);
        }
        self.reload()
    }

    // -- getters with TS semantics ------------------------------------------

    #[must_use]
    pub fn get_default_provider(&self) -> Option<&str> {
        self.merged.default_provider.as_deref()
    }

    #[must_use]
    pub fn get_default_model(&self) -> Option<&str> {
        self.merged.default_model.as_deref()
    }

    #[must_use]
    pub fn get_auxiliary_model(&self) -> Option<&str> {
        self.merged.auxiliary_model.as_deref()
    }

    /// TS `getImageModel`: the "provider/model-id" (or bare id) reference
    /// that serves turns attaching images on session models without image
    /// input. Malformed values behave as unset and the image-turn refusal
    /// names the setting instead.
    #[must_use]
    pub fn get_image_model(&self) -> Option<String> {
        self.merged
            .image_model
            .as_ref()
            .map(|m| m.trim().to_string())
            .filter(|m| !m.is_empty())
    }

    /// The daemon-level model allowlist (settings `allowedModels`): model
    /// patterns the daemon may resolve to, enforced at every daemon
    /// model resolution (`set_model`, RLM child-model resolution, the
    /// worker startup chain) — a model outside the allowlist fails loudly,
    /// never a fallback. Rust-only guardrail (no TS equivalent); `None` is
    /// unrestricted. Read from the global scope only, so a project cannot
    /// weaken a box-level pin.
    /// A list that trims to empty behaves as unset.
    #[must_use]
    pub fn get_allowed_models(&self) -> Option<Vec<String>> {
        let patterns = self.global.allowed_models.as_ref()?;
        let patterns: Vec<String> = patterns
            .iter()
            .map(|pattern| pattern.trim().to_string())
            .filter(|pattern| !pattern.is_empty())
            .collect();
        (!patterns.is_empty()).then_some(patterns)
    }

    /// TS `setDefaultServiceTier`: the persisted default a fresh session
    /// starts from; the stored string is the same vocabulary
    /// `get_default_service_tier` parses.
    ///
    /// # Errors
    ///
    /// Returns an error when the global settings file cannot be written.
    pub fn set_default_service_tier(&mut self, tier: uh_types::ai::ServiceTier) -> Result<()> {
        use uh_types::ai::ServiceTier;
        let name = match tier {
            ServiceTier::Auto => "auto",
            ServiceTier::Default => "default",
            ServiceTier::Flex => "flex",
            ServiceTier::Scale => "scale",
            ServiceTier::Priority => "priority",
        };
        self.global.default_service_tier = Some(name.to_string());
        self.save_global()
    }

    /// Service tier a fresh session records as its preference (TS
    /// `getDefaultServiceTier`: the setting when present, else `default`).
    /// An unrecognized setting value falls back to the same `default`.
    #[must_use]
    pub fn get_default_service_tier(&self) -> uh_types::ai::ServiceTier {
        use uh_types::ai::ServiceTier;
        self.merged
            .default_service_tier
            .as_deref()
            .and_then(|value| match value.trim().to_lowercase().as_str() {
                "auto" => Some(ServiceTier::Auto),
                "flex" => Some(ServiceTier::Flex),
                "scale" => Some(ServiceTier::Scale),
                "priority" => Some(ServiceTier::Priority),
                "default" => Some(ServiceTier::Default),
                _ => None,
            })
            .unwrap_or(ServiceTier::Default)
    }

    /// The steering queue's delivery mode (TS `steeringMode`): `all`
    /// batches every queued steering message into ONE co-delivered turn
    /// at the next turn boundary; `one-at-a-time` delivers one per turn.
    /// The product default is `all`; both modes stay selectable through
    /// the setting surface.
    #[must_use]
    pub fn get_steering_mode(&self) -> QueueModeSetting {
        self.merged.steering_mode.unwrap_or(QueueModeSetting::All)
    }

    #[must_use]
    pub fn get_follow_up_mode(&self) -> QueueModeSetting {
        self.merged
            .follow_up_mode
            .unwrap_or(QueueModeSetting::OneAtATime)
    }

    #[must_use]
    pub fn get_default_thinking_level(&self) -> Option<ThinkingLevelSetting> {
        self.merged.default_thinking_level
    }

    /// The shared provider retry policy from settings, combining the TS
    /// `getRetrySettings` and `getProviderRetrySettings` reads: the
    /// `retry.enabled`, `retry.maxRetries`, and `retry.baseDelayMs` knobs
    /// plus the `retry.provider.maxRetryDelayMs` cap.
    #[must_use]
    pub fn get_provider_retry_policy(
        &self,
    ) -> crate::session_engine::provider_retry::ProviderRetryPolicy {
        crate::session_engine::provider_retry::ProviderRetryPolicy {
            enabled: self
                .merged
                .retry
                .as_ref()
                .and_then(|retry| retry.enabled)
                .unwrap_or(true),
            max_retries: self
                .merged
                .retry
                .as_ref()
                .and_then(|retry| retry.max_retries)
                .map_or(
                    crate::session_engine::provider_retry::DEFAULT_PROVIDER_RETRY_POLICY
                        .max_retries,
                    |retries| retries.min(u64::from(u32::MAX)) as u32,
                ),
            base_delay_ms: self
                .merged
                .retry
                .as_ref()
                .and_then(|retry| retry.base_delay_ms)
                .unwrap_or(
                    crate::session_engine::provider_retry::DEFAULT_PROVIDER_RETRY_POLICY
                        .base_delay_ms,
                ),
            max_retry_delay_ms: self
                .merged
                .retry
                .as_ref()
                .and_then(|retry| retry.provider.as_ref())
                .and_then(|provider| provider.max_retry_delay_ms)
                .unwrap_or(
                    crate::session_engine::provider_retry::DEFAULT_PROVIDER_RETRY_POLICY
                        .max_retry_delay_ms,
                ),
            max_delay_ms: crate::session_engine::provider_retry::UNBOUNDED_BACKOFF_MS,
        }
    }

    /// The provider-failover policy from settings (`retry.failover`).
    #[must_use]
    pub fn get_provider_failover_policy(
        &self,
    ) -> crate::session_engine::provider_failover::ProviderFailoverPolicy {
        let defaults = crate::session_engine::provider_failover::DEFAULT_PROVIDER_FAILOVER_POLICY;
        let failover = self
            .merged
            .retry
            .as_ref()
            .and_then(|retry| retry.failover.as_ref());
        crate::session_engine::provider_failover::ProviderFailoverPolicy {
            enabled: failover
                .and_then(|failover| failover.enabled)
                .unwrap_or(defaults.enabled),
            max_retries: failover
                .and_then(|failover| failover.max_retries)
                .map_or(defaults.max_retries, |retries| {
                    retries.min(u64::from(u32::MAX)) as u32
                }),
            base_delay_ms: failover
                .and_then(|failover| failover.base_delay_ms)
                .unwrap_or(defaults.base_delay_ms),
            max_delay_ms: failover
                .and_then(|failover| failover.max_delay_ms)
                .unwrap_or(defaults.max_delay_ms),
        }
    }

    /// The quota-park policy from settings
    /// (`retry.provider.waitForUsage`; TS #2375): whether resets beyond
    /// the bounded wait park the session, the per-park ceiling (clamped
    /// to one week), and the per-episode park budget. Only the park keys
    /// have a consumer until a wait-for-usage port lands.
    #[must_use]
    pub fn get_provider_park_policy(
        &self,
    ) -> crate::session_engine::provider_park::ProviderParkPolicy {
        let defaults = crate::session_engine::provider_park::DEFAULT_PROVIDER_PARK_POLICY;
        let wait = self
            .merged
            .retry
            .as_ref()
            .and_then(|retry| retry.provider.as_ref())
            .and_then(|provider| provider.wait_for_usage.as_ref());
        crate::session_engine::provider_park::ProviderParkPolicy {
            pause_until_reset: wait
                .and_then(|wait| wait.pause_until_reset)
                .unwrap_or(defaults.pause_until_reset),
            max_pause_ms: wait
                .and_then(|wait| wait.max_pause_ms)
                .unwrap_or(defaults.max_pause_ms),
            max_parks: wait
                .and_then(|wait| wait.max_parks)
                .map_or(defaults.max_parks, |parks| {
                    parks.min(u64::from(u32::MAX)) as u32
                }),
        }
    }

    /// `requestTiming`: unset means OFF — the per-request timing timeline
    /// is opt-in, exactly the TS default (`getRequestTiming`).
    #[must_use]
    pub fn get_request_timing(&self) -> bool {
        self.merged.request_timing.unwrap_or(false)
    }

    #[must_use]
    pub fn get_session_dir(&self) -> Option<std::path::PathBuf> {
        let session_dir = self.merged.session_dir.as_ref()?;
        let home = uh_types::platform::home_dir()?;
        Some(if session_dir == "~" {
            home
        } else if let Some(rest) = session_dir.strip_prefix("~/") {
            home.join(rest)
        } else {
            session_dir.into()
        })
    }

    // -- persistence ---------------------------------------------------------

    /// Write the global scope back (project scope is host-written, not
    /// user-set in this port), then re-derive the effective settings.
    fn save_global(&mut self) -> Result<()> {
        // Unset fields serialize as `null`; the file only holds what is set
        // (the TS `JSON.stringify` drops `undefined`).
        let content =
            serde_json::to_string_pretty(&without_nulls(serde_json::to_value(&self.global)?))?;
        self.storage
            .with_lock(SettingsScope::Global, &mut |current| {
                let _ = current;
                Some(content.clone())
            })?;
        self.merged = deep_merge(&self.global, &self.project);
        Ok(())
    }
}

/// The value with every `null` object member removed, at any depth.
fn without_nulls(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.into_iter()
                .filter(|(_, value)| !value.is_null())
                .map(|(key, value)| (key, without_nulls(value)))
                .collect(),
        ),
        other => other,
    }
}

/// Set `parents.last` in the document; a parent that is missing or not an
/// object becomes an empty object first.
fn insert_path(
    document: &mut serde_json::Map<String, serde_json::Value>,
    parents: &[&str],
    last: &str,
    value: &serde_json::Value,
) {
    match parents.split_first() {
        None => {
            document.insert(last.to_string(), value.clone());
        }
        Some((first, rest)) => {
            let entry = document
                .entry((*first).to_string())
                .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
            if !entry.is_object() {
                *entry = serde_json::Value::Object(serde_json::Map::new());
            }
            if let serde_json::Value::Object(child) = entry {
                insert_path(child, rest, last, value);
            }
        }
    }
}

/// Remove `parents.last` from the document, then each parent the removal
/// left empty.
fn remove_path(
    document: &mut serde_json::Map<String, serde_json::Value>,
    parents: &[&str],
    last: &str,
) {
    match parents.split_first() {
        None => {
            document.remove(last);
        }
        Some((first, rest)) => {
            let Some(serde_json::Value::Object(child)) = document.get_mut(*first) else {
                return;
            };
            remove_path(child, rest, last);
            if child.is_empty() {
                document.remove(*first);
            }
        }
    }
}

fn strings(array: &[serde_json::Value]) -> Vec<String> {
    array
        .iter()
        .filter_map(|value| value.as_str().map(str::to_string))
        .collect()
}

/// One loaded scope: the leniently-parsed settings, the migrated raw
/// document (the value before the lenient field load — the only witness
/// for a PRESENT-but-malformed known field, which the lenient load drops),
/// and the load error (`Some` when the scope's document exists but could
/// not be read or parsed).
#[allow(clippy::type_complexity)]
fn load_scope(
    storage: &dyn SettingsStorage,
    scope: SettingsScope,
    errors: &mut Vec<SettingsError>,
) -> (Settings, Option<serde_json::Value>, Option<String>) {
    let mut load_error: Option<String> = None;
    // The pure-read arm: a locked protocol read on any cache miss, the
    // process-cached copy on a hit (see `SettingsStorage::read`).
    let content = match storage.read(scope) {
        Ok(content) => content,
        Err(error) => {
            let message = error.to_string();
            errors.push(SettingsError {
                scope,
                message: message.clone(),
            });
            return (Settings::default(), None, Some(message));
        }
    };
    let Some(content) = content else {
        return (Settings::default(), None, None);
    };
    let value: serde_json::Value = match serde_json::from_str(&content) {
        Ok(value) => value,
        Err(error) => {
            load_error = Some(error.to_string());
            serde_json::Value::Null
        }
    };
    if let Some(message) = load_error {
        errors.push(SettingsError {
            scope,
            message: message.clone(),
        });
        return (Settings::default(), None, Some(message));
    }
    // Migrate the raw document, then load leniently.
    let migrated = match value {
        serde_json::Value::Object(mut map) => {
            migrate(&mut map);
            serde_json::Value::Object(map)
        }
        other => other,
    };
    (from_value_lenient(&migrated), Some(migrated), None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_memory_loads_merges_and_saves() {
        let mut manager = SettingsManager::in_memory(&Settings::default());
        manager
            .set_default_model_and_provider("prime-inference", "z-ai/glm-5.3")
            .unwrap();
        assert_eq!(manager.get_default_provider(), Some("prime-inference"));
        assert_eq!(manager.get_default_model(), Some("z-ai/glm-5.3"));
        manager.reload().unwrap();
        assert_eq!(manager.get_default_model(), Some("z-ai/glm-5.3"));
    }

    /// The steering default is "all" (every queued steer co-delivers as
    /// ONE turn at the next tool-call boundary) with "one-at-a-time"
    /// selectable; the follow-up default stays "one-at-a-time".
    #[test]
    fn steering_mode_defaults_to_all_follow_ups_stay_one_at_a_time() {
        let mut manager = SettingsManager::in_memory(&Settings::default());
        assert_eq!(manager.get_steering_mode(), QueueModeSetting::All);
        assert_eq!(manager.get_follow_up_mode(), QueueModeSetting::OneAtATime);
        manager
            .set_steering_mode(QueueModeSetting::OneAtATime)
            .unwrap();
        assert_eq!(
            manager.get_steering_mode(),
            QueueModeSetting::OneAtATime,
            "the explicit one-at-a-time setting still selects one-per-turn"
        );
    }

    #[test]
    fn migrations_apply_on_load() {
        let storage: Arc<dyn SettingsStorage> =
            Arc::new(super::super::storage::InMemorySettingsStorage::default());
        storage
            .with_lock(SettingsScope::Global, &mut |_| {
                Some(r#"{ "queueMode": "all" }"#.into())
            })
            .unwrap();
        let manager = SettingsManager::from_storage(storage);
        assert_eq!(manager.get_steering_mode(), QueueModeSetting::All);
    }

    #[test]
    fn wrong_typed_fields_never_fail_load() {
        let storage: Arc<dyn SettingsStorage> =
            Arc::new(super::super::storage::InMemorySettingsStorage::default());
        storage
            .with_lock(SettingsScope::Global, &mut |_| {
                Some(r#"{ "defaultProvider": 42, "auxiliaryModel": "openai/gpt-5.4" }"#.into())
            })
            .unwrap();
        let manager = SettingsManager::from_storage(storage);
        assert_eq!(manager.get_default_provider(), None);
        assert_eq!(manager.get_auxiliary_model(), Some("openai/gpt-5.4"));
    }

    #[test]
    fn compaction_toggle_defaults_on_and_persists() {
        // TS getCompactionEnabled: absent -> true (auto-compaction is on
        // until the user opts out); setCompactionEnabled writes the global
        // scope, so the value survives a reload (a restarted session
        // re-seeds its flag from it).
        let mut manager = SettingsManager::in_memory(&Settings::default());
        assert!(manager.get_compaction_enabled());
        manager.set_compaction_enabled(false).unwrap();
        assert!(!manager.get_compaction_enabled());
        manager.reload().unwrap();
        assert!(!manager.get_compaction_enabled());
    }

    /// TS `getImageModel`: the `imageModel` reference reads trimmed, and
    /// malformed values (empty/whitespace) behave as unset.
    #[test]
    fn image_model_reads_trimmed_or_unset() {
        let manager = SettingsManager::in_memory(&Settings {
            image_model: Some("  battery/mock-vision  ".to_string()),
            ..Settings::default()
        });
        assert_eq!(
            manager.get_image_model().as_deref(),
            Some("battery/mock-vision")
        );
        let manager = SettingsManager::in_memory(&Settings {
            image_model: Some("   ".to_string()),
            ..Settings::default()
        });
        assert_eq!(manager.get_image_model(), None);
        let manager = SettingsManager::in_memory(&Settings::default());
        assert_eq!(manager.get_image_model(), None);
    }
}
