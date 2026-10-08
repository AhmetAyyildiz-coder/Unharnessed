//! Settings types (settings-manager.ts). All fields optional; JSON values of
//! the wrong type load as `None` (the TS access-time typechecks), never a
//! load error.

use serde::{Deserialize, Serialize};

/// Thinking levels in wire form ("off" | "minimal" | "low" | "medium" |
/// "high" | "xhigh" | "max").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ThinkingLevelSetting {
    Off,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl ThinkingLevelSetting {
    /// The reverse of [`ThinkingLevelSetting::model_level`]: a live switch
    /// (daemon `set_thinking_level`) persists a model-vocabulary level as
    /// the settings default without re-matching this enum at the call site.
    #[must_use]
    pub fn from_model_level(level: uh_types::ai::ModelThinkingLevel) -> ThinkingLevelSetting {
        match level {
            uh_types::ai::ModelThinkingLevel::Off => ThinkingLevelSetting::Off,
            uh_types::ai::ModelThinkingLevel::Minimal => ThinkingLevelSetting::Minimal,
            uh_types::ai::ModelThinkingLevel::Low => ThinkingLevelSetting::Low,
            uh_types::ai::ModelThinkingLevel::Medium => ThinkingLevelSetting::Medium,
            uh_types::ai::ModelThinkingLevel::High => ThinkingLevelSetting::High,
            uh_types::ai::ModelThinkingLevel::Xhigh => ThinkingLevelSetting::Xhigh,
            uh_types::ai::ModelThinkingLevel::Max => ThinkingLevelSetting::Max,
        }
    }

    /// The same level in the shared model vocabulary: the settings default
    /// feeds session thinking-level resolution, so callers need the
    /// uh-types value without re-matching this enum.
    #[must_use]
    pub fn model_level(self) -> uh_types::ai::ModelThinkingLevel {
        match self {
            ThinkingLevelSetting::Off => uh_types::ai::ModelThinkingLevel::Off,
            ThinkingLevelSetting::Minimal => uh_types::ai::ModelThinkingLevel::Minimal,
            ThinkingLevelSetting::Low => uh_types::ai::ModelThinkingLevel::Low,
            ThinkingLevelSetting::Medium => uh_types::ai::ModelThinkingLevel::Medium,
            ThinkingLevelSetting::High => uh_types::ai::ModelThinkingLevel::High,
            ThinkingLevelSetting::Xhigh => uh_types::ai::ModelThinkingLevel::Xhigh,
            ThinkingLevelSetting::Max => uh_types::ai::ModelThinkingLevel::Max,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum QueueModeSetting {
    All,
    #[serde(alias = "oneAtATime")]
    OneAtATime,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionSettings {
    pub enabled: Option<bool>,
    pub reserve_tokens: Option<u64>,
    pub keep_recent_tokens: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderWaitSettings {
    /// Park sessions for provider-reported resets beyond the bounded
    /// wait (TS #2375 `pauseUntilReset`; default true).
    pub pause_until_reset: Option<bool>,
    /// Maximum single park duration (TS `maxPauseMs`; default 24h,
    /// clamped to 7d).
    pub max_pause_ms: Option<u64>,
    /// Maximum parks per quota episode (TS `maxParks`; default 8).
    pub max_parks: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderRetrySettings {
    pub max_retry_delay_ms: Option<u64>,
    pub wait_for_usage: Option<ProviderWaitSettings>,
}

/// Provider-failover settings (`retry.failover`): when another configured
/// provider serves the same model, a provider that exhausts its quick
/// retries hands the turn to the next one instead of failing it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetryFailoverSettings {
    pub enabled: Option<bool>,
    /// Retries per provider before switching (default 3).
    pub max_retries: Option<u64>,
    /// First backoff delay, doubling each retry (default 1000ms).
    pub base_delay_ms: Option<u64>,
    /// Backoff ceiling per retry (default 30000ms).
    pub max_delay_ms: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetrySettings {
    pub enabled: Option<bool>,
    pub max_retries: Option<u64>,
    pub base_delay_ms: Option<u64>,
    pub failover: Option<RetryFailoverSettings>,
    pub provider: Option<ProviderRetrySettings>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageSettings {
    pub auto_resize: Option<bool>,
    pub block_images: Option<bool>,
}

/// One group of the `modelInstructions` setting: its text joins the system
/// prompt when the session runs one of its models.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ModelInstructions {
    /// `provider/model-id` references.
    pub models: Vec<String>,
    pub text: String,
}

/// User-declared MCP server (settings `mcpServers` entry).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum McpServerConfig {
    Http {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        headers: Option<serde_json::Map<String, serde_json::Value>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bearer_token_env_var: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        oauth: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        enabled: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        enabled_tools: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        disabled_tools: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        startup_timeout_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        call_timeout_ms: Option<u64>,
    },
    Stdio {
        command: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        args: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        env: Option<serde_json::Map<String, serde_json::Value>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        enabled: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        enabled_tools: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        disabled_tools: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        startup_timeout_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        call_timeout_ms: Option<u64>,
    },
}

/// The settings document: every known field optional; unknown keys preserved.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub default_provider: Option<String>,
    pub default_model: Option<String>,
    pub auxiliary_model: Option<String>,
    pub default_thinking_level: Option<ThinkingLevelSetting>,
    pub default_service_tier: Option<String>,
    pub steering_mode: Option<QueueModeSetting>,
    pub follow_up_mode: Option<QueueModeSetting>,
    pub compaction: Option<CompactionSettings>,
    pub retry: Option<RetrySettings>,
    /// Model ("provider/model-id" or a bare model id) that serves turns
    /// attaching images when the session model does not accept image
    /// input. Default: none - image turns on a text-only model fail with
    /// a configuration hint instead of silently dropping the images.
    pub image_model: Option<String>,
    pub mcp_servers: Option<serde_json::Map<String, serde_json::Value>>,
    /// Extra local MCP service-catalog files (TS `mcpCatalogSources`;
    /// ~-relative allowed), merged after the compiled built-ins, first
    /// source wins per id, and no bundled id can be shadowed.
    pub mcp_catalog_sources: Option<Vec<String>>,
    pub skills: Option<Vec<String>>,
    pub prompts: Option<Vec<String>>,
    pub enable_skill_commands: Option<bool>,
    pub enable_builtin_skills: Option<bool>,
    pub images: Option<ImageSettings>,
    /// Texts for the system prompt, each for some models: a session gets
    /// the texts of the groups that list its model, in order.
    pub model_instructions: Option<Vec<ModelInstructions>>,
    /// Rust-only daemon-level model allowlist: model patterns (the
    /// `--models` CLI scope grammar) the daemon may resolve to. Enforced at
    /// the daemon's model-resolution seams (`set_model`, RLM child-model
    /// resolution, the worker startup chain); a model outside the allowlist
    /// fails loudly instead of resolving, with no fallback. `None` is
    /// unrestricted (the TS behavior).
    pub allowed_models: Option<Vec<String>>,
    pub session_dir: Option<String>,
    /// Log per-request provider timing phases to the diagnostic log (TS
    /// `requestTiming`; unset means OFF, exactly the TS default).
    pub request_timing: Option<bool>,
    /// Unknown keys survive load/save round-trips (forward compatibility).
    #[serde(flatten, skip_serializing_if = "serde_json::Map::is_empty")]
    pub extra: serde_json::Map<String, serde_json::Value>,
}
