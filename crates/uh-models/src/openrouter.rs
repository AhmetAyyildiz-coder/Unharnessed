//! `OpenRouter`: the live public model list.
//!
//! The catalog repo builds its `OpenRouter` section from this same public
//! `/models` endpoint, but only when a maintainer re-runs its exporter, so
//! new `OpenRouter` models reach the picker late. Reading the endpoint here
//! closes that gap. The mapping is the exporter's
//! (`prime-agent-catalog/exporter/generate-models.ts`:
//! `fetchOpenRouterModels`, `getOpenRouterReasoningCapabilities`, the
//! `OpenRouter` overrides and the id-based `applyThinkingLevelMetadata`
//! rules); its output then goes through the strict catalog schema and
//! transport pinning like any catalog entry.
//!
//! Merge (the exporter's `mergeExistingCatalogModel`): a live entry
//! replaces the catalog entry with the same id, catalog-only entries (the
//! `auto` alias, ids `OpenRouter` stopped listing) stay.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{json, Map, Value};

use crate::cache::{CatalogCache, RefreshOptions, PUBLIC_SCOPE};
use crate::fetch::CatalogFetcher;
use crate::pinning::{parse_provider_model_catalog, PinnedTemplates};
use crate::Model;

/// The `OpenRouter` provider id.
pub const OPENROUTER_PROVIDER: &str = "openrouter";

/// The `OpenRouter` API base URL (models at `/models`).
pub const OPENROUTER_BASE_URL: &str = "https://openrouter.ai/api/v1";

const CACHE_FILE: &str = "openrouter-models-cache.json";

const THINKING_LEVELS: [&str; 6] = ["minimal", "low", "medium", "high", "xhigh", "max"];

/// Map `OpenRouter`'s `/models` payload to catalog entries and validate them
/// like the remote catalog (strict schema, skip-invalid, transport pinning).
///
/// # Errors
///
/// Fails when the payload has no `data` array or no entry survives.
pub fn parse_openrouter_models(
    payload: &Value,
    templates: &PinnedTemplates,
) -> Result<Vec<Model>, String> {
    let Some(items) = payload.get("data").and_then(Value::as_array) else {
        return Err("Invalid OpenRouter model list".into());
    };
    let mut seen = HashSet::new();
    let models: Vec<Value> = items
        .iter()
        .filter_map(catalog_entry)
        // The exporter keeps the first entry of a duplicated id.
        .filter(|entry| seen.insert(entry["id"].as_str().unwrap_or_default().to_string()))
        .collect();
    if models.is_empty() {
        return Err("OpenRouter model list has no tool-capable models".into());
    }
    parse_provider_model_catalog(&json!({"schemaVersion": 1, "models": models}), templates)
}

/// One `/models` item as a `catalog.v1` entry; `None` for models a session
/// cannot use (no tool support, `:batch` routes) or unusable pricing.
fn catalog_entry(item: &Value) -> Option<Value> {
    let item = item.as_object()?;
    let id = item.get("id")?.as_str()?;
    let supported: Vec<&str> = item
        .get("supported_parameters")
        .and_then(Value::as_array)
        .map(|params| params.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    // A session always attaches its tools; `:batch` routes do not stream.
    if !supported.contains(&"tools") || id.ends_with(":batch") {
        return None;
    }
    let image = match item.get("architecture").and_then(|a| a.get("modality")) {
        Some(Value::String(modality)) => modality.contains("image"),
        Some(Value::Array(modality)) => modality.iter().any(|m| m.as_str() == Some("image")),
        _ => false,
    };
    let pricing = item.get("pricing");
    let mut entry = Map::new();
    entry.insert("id".into(), json!(id));
    entry.insert(
        "name".into(),
        item.get("name").cloned().unwrap_or(Value::Null),
    );
    entry.insert("api".into(), json!("openai-completions"));
    entry.insert("provider".into(), json!(OPENROUTER_PROVIDER));
    entry.insert("baseUrl".into(), json!(OPENROUTER_BASE_URL));
    entry.insert("reasoning".into(), json!(supported.contains(&"reasoning")));
    if let Some((levels, supports_effort)) = reasoning_capabilities(item, &supported) {
        entry.insert("thinkingLevelMap".into(), Value::Object(levels));
        if !supports_effort {
            entry.insert("compat".into(), json!({"supportsReasoningEffort": false}));
        }
    }
    entry.insert(
        "input".into(),
        if image {
            json!(["text", "image"])
        } else {
            json!(["text"])
        },
    );
    entry.insert(
        "cost".into(),
        json!({
            "input": peak_price(pricing, "prompt")?,
            "output": peak_price(pricing, "completion")?,
            "cacheRead": peak_price(pricing, "input_cache_read")?,
            "cacheWrite": peak_price(pricing, "input_cache_write")?,
        }),
    );
    entry.insert(
        "contextWindow".into(),
        or_default_tokens(item.get("context_length")),
    );
    entry.insert(
        "maxTokens".into(),
        or_default_tokens(
            item.get("top_provider")
                .and_then(|p| p.get("max_completion_tokens")),
        ),
    );
    apply_overrides(id, &mut entry);
    Some(Value::Object(entry))
}

/// The exporter's `getOpenRouterReasoningCapabilities`: the effort map the
/// route declares and whether it takes an effort (`false`: on/off only).
fn reasoning_capabilities(
    item: &Map<String, Value>,
    supported: &[&str],
) -> Option<(Map<String, Value>, bool)> {
    let reasoning = item.get("reasoning")?.as_object()?;
    if !supported.contains(&"reasoning") {
        return None;
    }
    let mut levels = Map::new();
    if reasoning.get("mandatory") == Some(&Value::Bool(true)) {
        levels.insert("off".into(), Value::Null);
    }
    let efforts: Option<Vec<&str>> = match reasoning.get("supported_efforts") {
        Some(Value::Null) => Some(THINKING_LEVELS.to_vec()),
        Some(Value::Array(raw)) if !raw.is_empty() => {
            let known: Vec<&str> = THINKING_LEVELS
                .into_iter()
                .filter(|level| raw.iter().any(|effort| effort.as_str() == Some(level)))
                .collect();
            (!known.is_empty()).then_some(known)
        }
        _ => None,
    };
    let supports_effort = efforts.is_some();
    // Without efforts the route only toggles reasoning: one generic level.
    let efforts = efforts.unwrap_or_else(|| vec!["high"]);
    for level in THINKING_LEVELS {
        levels.insert(
            level.into(),
            if efforts.contains(&level) {
                json!(level)
            } else {
                Value::Null
            },
        );
    }
    Some((levels, supports_effort))
}

/// USD per million tokens. Time-windowed tariffs commit the peak rate;
/// negative placeholders count as 0; an unparsable price drops the model.
fn peak_price(pricing: Option<&Value>, field: &str) -> Option<f64> {
    let parse = |value: Option<&Value>| -> f64 {
        match value {
            None | Some(Value::Null) => 0.0,
            Some(Value::String(text)) if text.is_empty() => 0.0,
            Some(Value::String(text)) => text.trim().parse().unwrap_or(f64::NAN),
            Some(Value::Number(number)) => number.as_f64().unwrap_or(f64::NAN),
            Some(_) => f64::NAN,
        }
    };
    let windowed = pricing
        .and_then(|p| p.get("overrides"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|tariff| tariff.get("utc_start").is_some_and(Value::is_number));
    let mut peak = 0.0_f64;
    for price in std::iter::once(parse(pricing.and_then(|p| p.get(field))))
        .chain(windowed.map(|tariff| parse(tariff.get(field))))
    {
        if price.is_nan() {
            return None;
        }
        peak = peak.max(price);
    }
    Some(peak * 1_000_000.0)
}

/// `value || 4096`: a missing or zero limit falls back to 4096.
fn or_default_tokens(value: Option<&Value>) -> Value {
    match value {
        None | Some(Value::Null) => json!(4096),
        Some(Value::Number(number)) if number.as_f64() == Some(0.0) => json!(4096),
        Some(value) => value.clone(),
    }
}

/// `mergeThinkingLevelMap`: set `pairs` over the entry's map, creating it.
fn merge_levels(entry: &mut Map<String, Value>, pairs: &[(&str, Option<&str>)]) {
    let levels = entry
        .entry("thinkingLevelMap")
        .or_insert_with(|| Value::Object(Map::new()));
    if let Value::Object(levels) = levels {
        for (level, value) in pairs {
            levels.insert((*level).into(), value.map_or(Value::Null, |v| json!(v)));
        }
    }
}

const DEEPSEEK_V4_LEVELS: [(&str, Option<&str>); 6] = [
    ("minimal", None),
    ("low", None),
    ("medium", None),
    ("high", Some("high")),
    ("xhigh", Some("max")),
    ("max", None),
];

/// The exporter's per-model knowledge the API does not carry, in its order:
/// the pinned `OpenRouter` metadata, the `DeepSeek` V4 wire format, then the
/// id-based thinking-level rules.
fn apply_overrides(id: &str, entry: &mut Map<String, Value>) {
    let mut set = |key: &str, value: Value| {
        if let Some(field) = key.strip_prefix("cost.") {
            if let Some(cost) = entry.get_mut("cost").and_then(Value::as_object_mut) {
                cost.insert(field.into(), value);
            }
        } else {
            entry.insert(key.into(), value);
        }
    };
    // "Keep selected OpenRouter model metadata stable until upstream settles."
    match id {
        "moonshotai/kimi-k2.5" => {
            set("cost.input", json!(0.41));
            set("cost.output", json!(2.06));
            set("cost.cacheRead", json!(0.07));
            set("maxTokens", json!(4096));
        }
        "moonshotai/kimi-k3" => set("maxTokens", json!(1_048_576)),
        "z-ai/glm-5" => {
            set("cost.input", json!(0.6));
            set("cost.output", json!(1.9));
            set("cost.cacheRead", json!(0.119));
        }
        _ => {}
    }

    if id.contains("deepseek-v4") {
        let compat = entry.entry("compat").or_insert_with(|| json!({}));
        if let Some(compat) = compat.as_object_mut() {
            compat.insert(
                "requiresReasoningContentOnAssistantMessages".into(),
                json!(true),
            );
            compat.insert("thinkingFormat".into(), json!("deepseek"));
        }
        merge_levels(entry, &DEEPSEEK_V4_LEVELS);
    }
    apply_thinking_level_rules(id, entry);
}

/// The exporter's `applyThinkingLevelMetadata` rules that can match an
/// `OpenRouter` id (the others are gated on another api or provider).
fn apply_thinking_level_rules(id: &str, entry: &mut Map<String, Value>) {
    let has = |needles: &[&str]| needles.iter().any(|needle| id.contains(needle));
    if has(&["gpt-5.2", "gpt-5.3", "gpt-5.4", "gpt-5.5", "gpt-5.6"]) {
        merge_levels(entry, &[("xhigh", Some("xhigh"))]);
    }
    if has(&["gpt-5.6"]) {
        merge_levels(entry, &[("minimal", None), ("max", Some("max"))]);
    }
    if has(&["gpt-6"]) {
        merge_levels(
            entry,
            &[
                ("minimal", None),
                ("xhigh", Some("xhigh")),
                ("max", Some("max")),
            ],
        );
    }
    if has(&["opus-4-6", "opus-4.6", "sonnet-4-6", "sonnet-4.6"]) {
        merge_levels(entry, &[("max", Some("max"))]);
    }
    if has(&[
        "opus-4-7", "opus-4.7", "opus-4-8", "opus-4.8", "opus-5", "sonnet-5",
    ]) {
        merge_levels(entry, &[("xhigh", Some("xhigh")), ("max", Some("max"))]);
    }
    if has(&["fable-5", "mythos-5"]) {
        merge_levels(
            entry,
            &[
                ("off", None),
                ("xhigh", Some("xhigh")),
                ("max", Some("max")),
            ],
        );
    }
    if has(&["opus-5-5", "opus-5.5"]) {
        merge_levels(entry, &[("off", None)]);
    }
    if has(&["mythos-preview"]) {
        merge_levels(entry, &[("off", None), ("max", Some("max"))]);
    }
    if id.contains("deepseek-v4") {
        merge_levels(entry, &DEEPSEEK_V4_LEVELS);
    }
    let lower = id.to_ascii_lowercase();
    let kimi_k3 = |rest: &str| rest.is_empty() || rest.starts_with('-');
    let is_kimi_k3 = lower.strip_prefix("k3").is_some_and(kimi_k3)
        || lower.match_indices("kimi-k3").any(|(at, _)| {
            (at == 0 || lower[..at].ends_with('/')) && kimi_k3(&lower[at + "kimi-k3".len()..])
        });
    if !entry.contains_key("thinkingLevelMap") && is_kimi_k3 {
        merge_levels(
            entry,
            &[
                ("off", None),
                ("minimal", None),
                ("low", None),
                ("medium", None),
                ("high", None),
                ("xhigh", None),
                ("max", Some("max")),
            ],
        );
    }
}

/// Overlay the live list on `base`'s `OpenRouter` entries: a live entry
/// replaces the same id (keeping the catalog's `featured`), new ids append,
/// catalog-only entries stay.
#[must_use]
pub fn merge_openrouter_models(base: Vec<Model>, live: &[Model]) -> Vec<Model> {
    let mut live_by_id: HashMap<&str, &Model> = live
        .iter()
        .map(|model| (model.id.as_str(), model))
        .collect();
    let mut merged: Vec<Model> = base
        .into_iter()
        .map(|model| {
            if model.provider != OPENROUTER_PROVIDER {
                return model;
            }
            match live_by_id.remove(model.id.as_str()) {
                Some(fresh) => Model {
                    featured: model.featured,
                    ..fresh.clone()
                },
                None => model,
            }
        })
        .collect();
    merged.extend(
        live.iter()
            .filter(|model| live_by_id.contains_key(model.id.as_str()))
            .cloned(),
    );
    merged
}

/// The last-good `OpenRouter` list (unauthenticated, public scope).
pub struct OpenRouterCatalog {
    cache: CatalogCache<Vec<Model>>,
}

impl OpenRouterCatalog {
    /// A list persisted beside `models_dir` (`openrouter-models-cache.json`),
    /// fetched from `{base_url}/models`.
    #[must_use]
    pub fn new(models_dir: Option<PathBuf>, base_url: &str, templates: PinnedTemplates) -> Self {
        let parse: crate::cache::CatalogParse<Vec<Model>> =
            Arc::new(move |payload, _scope| parse_openrouter_models(payload, &templates));
        Self {
            cache: CatalogCache::new(
                &format!("{base_url}/models"),
                models_dir.map(|dir| dir.join(CACHE_FILE)),
                Arc::new(CatalogFetcher::new()),
                parse,
            ),
        }
    }

    /// The last-good list, if one was ever fetched.
    pub fn get(&self) -> Option<Vec<Model>> {
        self.cache.get(PUBLIC_SCOPE)
    }

    /// Fetch the list (hourly-gated unless `force`); failures keep last-good.
    pub async fn refresh(&self, force: bool) -> Option<Vec<Model>> {
        self.cache
            .refresh(
                PUBLIC_SCOPE,
                RefreshOptions {
                    force,
                    ..RefreshOptions::default()
                },
            )
            .await
    }
}
