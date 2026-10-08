//! Local model servers: Ollama, LM Studio, llama.cpp, vLLM, or any server
//! with the `OpenAI` chat completions API. [`scan`] finds the ones running at
//! their usual addresses, [`probe`] reads what one serves, and
//! [`save_server`] and [`remove_server`] keep its entry in models.json: one
//! custom provider per server, with the models picked from it.
//!
//! models.json is written whole again: `//` comments in it do not survive a
//! save.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use futures::future::join_all;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use super::custom::{load_custom_models, parse_models_config, strip_json_comments};

/// What the servers speak.
const API: &str = "openai-completions";

/// The key of a server that asks for none: sent, and ignored.
const NO_KEY: &str = "local";

/// The servers [`scan`] looks for, at their default addresses: Ollama, LM
/// Studio, llama.cpp and vLLM.
const USUAL: [&str; 4] = [
    "http://localhost:11434/v1",
    "http://localhost:1234/v1",
    "http://localhost:8080/v1",
    "http://localhost:8000/v1",
];

const LIST_TIMEOUT: Duration = Duration::from_secs(3);
const DETAIL_TIMEOUT: Duration = Duration::from_secs(4);

/// The longest answer asked of a model: the agent's default, or a quarter
/// of a small context.
const MAX_TOKENS: u64 = 16_384;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Ollama,
    LmStudio,
    LlamaCpp,
    Vllm,
    Other,
}

impl Kind {
    /// The provider id a new server of this kind gets in models.json.
    fn id(self) -> &'static str {
        match self {
            Kind::Ollama => "ollama",
            Kind::LmStudio => "lmstudio",
            Kind::LlamaCpp => "llama-cpp",
            Kind::Vllm => "vllm",
            Kind::Other => "local",
        }
    }

    fn name(self) -> Option<&'static str> {
        match self {
            Kind::Ollama => Some("Ollama"),
            Kind::LmStudio => Some("LM Studio"),
            Kind::LlamaCpp => Some("llama.cpp"),
            Kind::Vllm => Some("vLLM"),
            Kind::Other => None,
        }
    }
}

/// A server and the models it serves.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalServer {
    /// The provider id it gets in models.json, when it is not there yet.
    pub id: String,
    /// What the server is (`Ollama`), else its address (`localhost:9000`).
    pub name: String,
    pub base_url: String,
    pub models: Vec<LocalModel>,
}

/// One model of a server, with what the server tells of it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LocalModel {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    pub image: bool,
    pub reasoning: bool,
}

/// Why a server could not be read: `{ kind, message }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProbeError {
    pub kind: ProbeErrorKind,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ProbeErrorKind {
    /// Not an address, or something answers there that lists no models.
    Invalid,
    /// Nothing answers there.
    Unreachable,
    /// The server asks for an API key, or refused the one given.
    NeedsKey,
}

impl ProbeError {
    fn new(kind: ProbeErrorKind, message: String) -> Self {
        ProbeError { kind, message }
    }
}

impl std::fmt::Display for ProbeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// The servers running at their usual addresses on this machine, each with
/// its models (none yet, for a server that has none).
pub async fn scan() -> Vec<LocalServer> {
    join_all(USUAL.iter().map(|address| probe(address, None)))
        .await
        .into_iter()
        .filter_map(Result::ok)
        .collect()
}

/// What the server at `address` serves. `localhost:11434` stands for
/// `http://localhost:11434`; a server's root stands for its `/v1`.
///
/// # Errors
///
/// [`ProbeErrorKind::Invalid`] when `address` is not an http address or
/// what answers lists no models, [`ProbeErrorKind::Unreachable`] when
/// nothing answers, [`ProbeErrorKind::NeedsKey`] when the server refuses
/// the request without a key or with `api_key`.
pub async fn probe(address: &str, api_key: Option<&str>) -> Result<LocalServer, ProbeError> {
    let base = normalize(address)?;
    let api_key = api_key.map(str::trim).filter(|key| !key.is_empty());
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(1))
        // A redirect must not carry the key elsewhere.
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap_or_default();
    let (base, rows) = match list(&client, &base, api_key).await {
        Ok(rows) => (base, rows),
        Err(error) if error.kind == ProbeErrorKind::Invalid && !base.ends_with("/v1") => {
            let v1 = format!("{base}/v1");
            match list(&client, &v1, api_key).await {
                Ok(rows) => (v1, rows),
                Err(_) => return Err(error),
            }
        }
        Err(error) => return Err(error),
    };
    let mut models: Vec<LocalModel> = rows.iter().filter_map(model_of).collect();
    let root = base.strip_suffix("/v1").unwrap_or(&base);
    let owner = rows
        .iter()
        .find_map(|row| row.get("owned_by").and_then(Value::as_str));
    let kind = match owner {
        Some("llamacpp") => {
            llama_cpp(&client, root, api_key, &mut models).await;
            Kind::LlamaCpp
        }
        Some("vllm") => Kind::Vllm,
        _ => {
            let version = get(&client, &format!("{root}/api/version"), api_key).await;
            if version.is_some_and(|body| body.get("version").is_some()) {
                ollama(&client, root, api_key, &mut models).await;
                Kind::Ollama
            } else if let Some(listed) = get(&client, &format!("{root}/api/v0/models"), api_key)
                .await
                .and_then(|body| body.get("data").and_then(Value::as_array).cloned())
            {
                lm_studio(&listed, &mut models);
                Kind::LmStudio
            } else {
                Kind::Other
            }
        }
    };
    Ok(LocalServer {
        id: kind.id().to_string(),
        name: kind
            .name()
            .map_or_else(|| host_of(&base), ToString::to_string),
        base_url: base,
        models,
    })
}

/// `localhost:11434/` → `http://localhost:11434`.
fn normalize(address: &str) -> Result<String, ProbeError> {
    let trimmed = address.trim().trim_end_matches('/');
    let full = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("http://{trimmed}")
    };
    match url::Url::parse(&full) {
        Ok(url) if matches!(url.scheme(), "http" | "https") && url.host().is_some() => Ok(full),
        _ => Err(ProbeError::new(
            ProbeErrorKind::Invalid,
            format!(
                "\"{}\" is not an address, like http://localhost:11434/v1.",
                address.trim()
            ),
        )),
    }
}

fn host_of(base: &str) -> String {
    url::Url::parse(base)
        .ok()
        .and_then(|url| {
            let host = url.host_str()?.to_string();
            Some(
                url.port()
                    .map_or_else(|| host.clone(), |port| format!("{host}:{port}")),
            )
        })
        .unwrap_or_else(|| base.to_string())
}

fn authorized(request: reqwest::RequestBuilder, api_key: Option<&str>) -> reqwest::RequestBuilder {
    match api_key {
        Some(key) => request.bearer_auth(key),
        None => request,
    }
}

/// The rows of `GET <base>/models`.
async fn list(
    client: &reqwest::Client,
    base: &str,
    api_key: Option<&str>,
) -> Result<Vec<Value>, ProbeError> {
    let request = authorized(client.get(format!("{base}/models")), api_key);
    let response = request.timeout(LIST_TIMEOUT).send().await.map_err(|_| {
        ProbeError::new(
            ProbeErrorKind::Unreachable,
            format!("Nothing answers at {base}."),
        )
    })?;
    let status = response.status();
    if matches!(status.as_u16(), 401 | 403) {
        let message = if api_key.is_some() {
            format!("{base} refused this API key.")
        } else {
            format!("{base} asks for an API key.")
        };
        return Err(ProbeError::new(ProbeErrorKind::NeedsKey, message));
    }
    let not_a_server = |what: &str| {
        ProbeError::new(
            ProbeErrorKind::Invalid,
            format!("{base} answers, but lists no models ({what})."),
        )
    };
    if !status.is_success() {
        return Err(not_a_server(&format!("HTTP {}", status.as_u16())));
    }
    let body: Value = response
        .json()
        .await
        .map_err(|_| not_a_server("not JSON"))?;
    body.get("data")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| not_a_server("no data"))
}

/// A JSON answer, or `None`: the details are a bonus.
async fn get(client: &reqwest::Client, url: &str, api_key: Option<&str>) -> Option<Value> {
    send(authorized(client.get(url), api_key)).await
}

async fn send(request: reqwest::RequestBuilder) -> Option<Value> {
    let response = request.timeout(DETAIL_TIMEOUT).send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    response.json().await.ok()
}

fn positive(value: Option<&Value>) -> Option<u64> {
    value.and_then(Value::as_u64).filter(|tokens| *tokens > 0)
}

/// A row of `/models`, with the context vLLM (`max_model_len`) and
/// llama.cpp (`meta.n_ctx_train`) give there.
fn model_of(row: &Value) -> Option<LocalModel> {
    let id = row.get("id")?.as_str()?.trim();
    if id.is_empty() {
        return None;
    }
    let context_window = ["max_model_len", "context_length", "max_context_length"]
        .iter()
        .find_map(|key| positive(row.get(*key)))
        .or_else(|| positive(row.pointer("/meta/n_ctx_train")));
    Some(LocalModel {
        id: id.to_string(),
        context_window,
        ..LocalModel::default()
    })
}

/// Ollama's `/api/show`: what each model can do and its context. A model
/// that cannot chat (an embedding model) is left out.
async fn ollama(
    client: &reqwest::Client,
    root: &str,
    api_key: Option<&str>,
    models: &mut Vec<LocalModel>,
) {
    let url = format!("{root}/api/show");
    let shows = join_all(models.iter().map(|model| {
        let request = client.post(&url).json(&json!({ "model": model.id }));
        send(authorized(request, api_key))
    }))
    .await;
    let mut kept = Vec::with_capacity(models.len());
    for (mut model, show) in models.drain(..).zip(shows) {
        if let Some(show) = show {
            let capabilities: Vec<&str> = show
                .get("capabilities")
                .and_then(Value::as_array)
                .map(|list| list.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            if !capabilities.is_empty() && !capabilities.contains(&"completion") {
                continue;
            }
            model.image = capabilities.contains(&"vision");
            model.reasoning = capabilities.contains(&"thinking");
            model.context_window = model.context_window.or_else(|| ollama_context(&show));
        }
        kept.push(model);
    }
    *models = kept;
}

/// `model_info["<architecture>.context_length"]`.
fn ollama_context(show: &Value) -> Option<u64> {
    let info = show.get("model_info")?.as_object()?;
    let architecture = info.get("general.architecture").and_then(Value::as_str);
    let named = architecture.and_then(|name| info.get(&format!("{name}.context_length")));
    positive(named.or_else(|| {
        info.iter()
            .find(|(key, _)| key.ends_with(".context_length"))
            .map(|(_, value)| value)
    }))
}

/// LM Studio's `/api/v0/models`: the loaded context, else the longest;
/// `vlm` models read images; embedding models are left out.
fn lm_studio(listed: &[Value], models: &mut Vec<LocalModel>) {
    models.retain_mut(|model| {
        let Some(row) = listed
            .iter()
            .find(|row| row.get("id").and_then(Value::as_str) == Some(model.id.as_str()))
        else {
            return true;
        };
        let kind = row.get("type").and_then(Value::as_str);
        if kind == Some("embeddings") {
            return false;
        }
        model.image = kind == Some("vlm");
        model.context_window = model.context_window.or_else(|| {
            positive(row.get("loaded_context_length"))
                .or_else(|| positive(row.get("max_context_length")))
        });
        true
    });
}

/// llama.cpp's `/props`: the context it runs with, and whether it reads
/// images.
async fn llama_cpp(
    client: &reqwest::Client,
    root: &str,
    api_key: Option<&str>,
    models: &mut [LocalModel],
) {
    let Some(props) = get(client, &format!("{root}/props"), api_key).await else {
        return;
    };
    let context = positive(props.pointer("/default_generation_settings/n_ctx"));
    let image = props
        .pointer("/modalities/vision")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    for model in models {
        model.context_window = context.or(model.context_window);
        model.image |= image;
    }
}

/// A server added here or by hand: a custom provider of models.json with
/// models of its own.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalProvider {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub models: Vec<String>,
}

/// The local servers in `models_json`; none when it cannot be read.
#[must_use]
pub fn local_providers(models_json: &Path) -> Vec<LocalProvider> {
    let Ok(content) = std::fs::read_to_string(models_json) else {
        return Vec::new();
    };
    let Ok(config) = parse_models_config(&content) else {
        return Vec::new();
    };
    config
        .providers
        .into_iter()
        .filter(|(id, _)| !built_in(id))
        .filter_map(|(id, provider)| {
            let models: Vec<String> = provider.models?.into_iter().map(|model| model.id).collect();
            if models.is_empty() {
                return None;
            }
            Some(LocalProvider {
                name: provider.name.unwrap_or_else(|| id.clone()),
                base_url: provider.base_url.unwrap_or_default(),
                id,
                models,
            })
        })
        .collect()
}

/// A server to write into models.json, with the models picked from it.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SaveServer {
    /// The server's provider id, when it is in models.json already.
    pub provider_id: Option<String>,
    /// The id a new server gets ([`LocalServer::id`]), made unique.
    pub id: String,
    pub name: String,
    pub base_url: String,
    /// Kept as it is when absent.
    pub api_key: Option<String>,
    pub models: Vec<LocalModel>,
}

/// Write the server into `models_json`: a new provider, or the one already
/// there for its address, whose models become those picked. A model that
/// was there keeps its definition; the others get what the server told of
/// them. Answers the provider id.
///
/// # Errors
///
/// When the address is not one, no model is picked, `provider_id` names no
/// local server, models.json cannot be read or is not valid, or it cannot
/// be written.
pub fn save_server(models_json: &Path, request: &SaveServer) -> Result<String, String> {
    let base_url = normalize(&request.base_url).map_err(|error| error.message)?;
    let mut picked: Vec<&LocalModel> = Vec::new();
    for model in &request.models {
        if !model.id.trim().is_empty() && !picked.iter().any(|seen| seen.id == model.id) {
            picked.push(model);
        }
    }
    if picked.is_empty() {
        return Err("Pick at least one model.".to_string());
    }
    let api_key = request
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|key| !key.is_empty());
    let mut document = read_document(models_json)?;
    let providers = providers_of(&mut document)?;
    let existing = match &request.provider_id {
        Some(id) if is_local(id, providers.get(id)) => Some(id.clone()),
        Some(id) => return Err(format!("No local server {id} in models.json.")),
        None => providers
            .iter()
            .find(|(id, provider)| {
                !built_in(id)
                    && provider
                        .get("baseUrl")
                        .and_then(Value::as_str)
                        .is_some_and(|url| url.trim_end_matches('/') == base_url)
            })
            .map(|(id, _)| id.clone()),
    };
    let id = if let Some(id) = existing {
        let provider = providers
            .get_mut(&id)
            .and_then(Value::as_object_mut)
            .ok_or_else(|| format!("models.json: provider {id} is not an object."))?;
        let mut kept: HashMap<String, Value> = provider
            .get("models")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|model| Some((model.get("id")?.as_str()?.to_string(), model.clone())))
            .collect();
        let models = picked
            .iter()
            .map(|model| kept.remove(&model.id).unwrap_or_else(|| definition(model)))
            .collect();
        provider.insert("baseUrl".to_string(), json!(base_url));
        if let Some(key) = api_key {
            provider.insert("apiKey".to_string(), json!(key));
        }
        provider.insert("models".to_string(), Value::Array(models));
        id
    } else {
        let id = unique_id(providers, &request.id);
        let name = match request.name.trim() {
            "" => host_of(&base_url),
            name => name.to_string(),
        };
        let models: Vec<Value> = picked.iter().map(|model| definition(model)).collect();
        providers.insert(
            id.clone(),
            json!({
                "name": name,
                "baseUrl": base_url,
                "api": API,
                "apiKey": api_key.unwrap_or(NO_KEY),
                // What every local server takes: the system role, no
                // `store`, and `max_tokens`.
                "compat": {
                    "supportsDeveloperRole": false,
                    "supportsStore": false,
                    "maxTokensField": "max_tokens",
                },
                "models": models,
            }),
        );
        id
    };
    write_document(models_json, &document)?;
    Ok(id)
}

/// Take the server out of `models_json`, with its models.
///
/// # Errors
///
/// When `provider_id` names no local server, or models.json cannot be read,
/// is not valid, or cannot be written.
pub fn remove_server(models_json: &Path, provider_id: &str) -> Result<(), String> {
    let mut document = read_document(models_json)?;
    let providers = providers_of(&mut document)?;
    if !is_local(provider_id, providers.get(provider_id)) {
        return Err(format!("No local server {provider_id} in models.json."));
    }
    providers.shift_remove(provider_id);
    write_document(models_json, &document)
}

fn built_in(provider: &str) -> bool {
    !uh_ai::models_generated::get_models(provider).is_empty()
}

fn is_local(id: &str, provider: Option<&Value>) -> bool {
    !built_in(id)
        && provider
            .and_then(|provider| provider.get("models"))
            .and_then(Value::as_array)
            .is_some_and(|models| !models.is_empty())
}

fn definition(model: &LocalModel) -> Value {
    let mut definition = json!({ "id": model.id.trim() });
    if let Some(window) = model.context_window {
        definition["contextWindow"] = json!(window);
        definition["maxTokens"] = json!(MAX_TOKENS.min(window / 4).max(1));
    }
    if model.image {
        definition["input"] = json!(["text", "image"]);
    }
    if model.reasoning {
        definition["reasoning"] = json!(true);
    }
    definition
}

/// `wanted` as an id no built-in provider and no other entry has.
fn unique_id(providers: &Map<String, Value>, wanted: &str) -> String {
    let slug: String = wanted
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let slug = match slug.trim_matches('-') {
        "" => "local".to_string(),
        slug => slug.to_string(),
    };
    let free = |id: &str| !built_in(id) && !providers.contains_key(id);
    if free(&slug) {
        return slug;
    }
    let mut n = 2;
    loop {
        let id = format!("{slug}-{n}");
        if free(&id) {
            return id;
        }
        n += 1;
    }
}

fn read_document(path: &Path) -> Result<Value, String> {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(format!("models.json could not be read: {error}")),
    };
    if content.trim().is_empty() {
        return Ok(json!({ "providers": {} }));
    }
    let document: Value =
        serde_json::from_str(&strip_json_comments(&content)).map_err(|error| {
            format!("models.json is not valid JSON ({error}). Fix it, then try again.")
        })?;
    if document.is_object() {
        Ok(document)
    } else {
        Err("models.json is not a JSON object. Fix it, then try again.".to_string())
    }
}

fn providers_of(document: &mut Value) -> Result<&mut Map<String, Value>, String> {
    document
        .as_object_mut()
        .and_then(|object| {
            object
                .entry("providers")
                .or_insert_with(|| json!({}))
                .as_object_mut()
        })
        .ok_or_else(|| {
            "models.json: \"providers\" is not an object. Fix it, then try again.".to_string()
        })
}

/// Written only when the agent will load it: the whole file, checked as
/// the model registry checks it.
fn write_document(path: &Path, document: &Value) -> Result<(), String> {
    let content = serde_json::to_string_pretty(document)
        .map_err(|error| format!("models.json could not be written: {error}"))?;
    let content = format!("{content}\n");
    let loaded = load_custom_models(&content, &built_in, &|provider| {
        uh_ai::models_generated::get_models(provider)
            .first()
            .map(|model| (model.api.clone(), model.base_url.clone()))
    });
    if let Some(error) = loaded.error {
        return Err(error);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|error| format!("models.json could not be written: {error}"))?;
    }
    crate::settings::storage::atomic_write(path, &content)
        .map_err(|error| format!("models.json could not be written: {error}"))
}
