//! Google Antigravity streaming provider: Gemini through the Cloud Code
//! Assist backend a Google AI subscription serves Antigravity from.
//!
//! The backend takes the Gemini request of [`crate::providers::google`]
//! inside an envelope naming the model and the account's project, and
//! answers each stream chunk inside a `response` field. It serves
//! requests that identify as Antigravity: the client's `User-Agent`,
//! its system preamble, and its `agent` request type.

use serde_json::{json, Map, Value};

use crate::client_versions::Client;
use crate::event_stream::{
    create_assistant_message_event_stream, AssistantMessageEvent, AssistantMessageEventStream,
    AssistantMessageEventWriter,
};
use crate::oauth::{
    antigravity_user_agent, parse_google_antigravity_api_key, DAILY_ENDPOINT, PROD_ENDPOINT,
};
use crate::providers::google::{build_params, simple_google_options, GoogleOptions};
use crate::providers::google_shared::{is_gemini3_flash_model, is_gemini3_pro_model};
use crate::providers::google_stream::GoogleStreamState;
use crate::registry::Provider;
use crate::types::{
    done_reason, error_reason, AssistantMessage, Context, Model, SimpleStreamOptions, StopReason,
    StreamOptions, Usage,
};
use crate::utils_inner::diagnostics::now_ms;
use crate::utils_inner::http::{send, HttpResponse, RequestOptions};
use crate::utils_inner::json_parse::parse_json_with_repair;
use crate::utils_inner::sanitize_unicode::sanitize_surrogates;
use crate::utils_inner::sse::SseDecoder;
use crate::utils_inner::stream_failure::{
    format_stream_failure_message, record_stream_failure, stream_failure_from_stop_reason,
    ProviderError,
};

pub const API_GOOGLE_ANTIGRAVITY: &str = "google-antigravity";

/// Antigravity's system preamble, which the backend expects first.
const ANTIGRAVITY_SYSTEM_INSTRUCTION: &str = "You are Antigravity, a powerful agentic AI coding assistant designed by the Google Deepmind team working on Advanced Agentic Coding.You are pair programming with a USER to solve their coding task. The task may require creating a new codebase, modifying or debugging an existing codebase, or simply answering a question.**Absolute paths only****Proactiveness**";
/// The signature Google documents for a function call that has none
/// (one made by another model earlier in the session): Gemini 3 refuses
/// an unsigned call.
const UNSIGNED_CALL_SIGNATURE: &str = "skip_thought_signature_validator";

pub fn stream_google_antigravity(
    model: &Model,
    context: &Context,
    options: Option<&GoogleOptions>,
) -> AssistantMessageEventStream {
    let options = options.cloned().unwrap_or_default();
    let model = model.clone();
    let context = context.clone();
    let (writer, reader) = create_assistant_message_event_stream();

    tokio::spawn(async move {
        let mut output = AssistantMessage {
            content: Vec::new(),
            api: API_GOOGLE_ANTIGRAVITY.to_string(),
            provider: model.provider.clone(),
            model: model.id.clone(),
            response_model: None,
            response_id: None,
            diagnostics: None,
            usage: Usage::default(),
            stop_reason: StopReason::Stop,
            stop_reason_raw: None,
            error_message: None,
            timestamp: now_ms(),
            rest: Map::default(),
        };

        match run_stream(&model, &context, &options, &mut output, &writer).await {
            Ok(()) => {
                writer.push(AssistantMessageEvent::Done {
                    reason: done_reason(output.stop_reason),
                    message: output,
                });
                writer.end(None);
            }
            Err(error) => {
                output.stop_reason = if error == ProviderError::Aborted {
                    StopReason::Aborted
                } else {
                    StopReason::Error
                };
                output.error_message = Some(format_stream_failure_message(&error));
                record_stream_failure(
                    (&model.provider, &model.id, &model.api),
                    &mut output,
                    &error,
                );
                writer.push(AssistantMessageEvent::Error {
                    reason: error_reason(output.stop_reason),
                    error: output.clone(),
                });
                writer.end(Some(output));
            }
        }
    });

    reader
}

/// The Gemini request in the backend's envelope. Three fields differ
/// from the Gemini API's: the system instruction opens with
/// Antigravity's preamble, the tools carry `parameters` schemas, and
/// the session id rides the request.
fn build_request(
    model: &Model,
    context: &Context,
    options: &GoogleOptions,
    project_id: &str,
) -> Value {
    let mut request = build_params(model, context, options);
    let body = request
        .as_object_mut()
        .expect("the Gemini request is an object");

    let mut instruction = vec![
        json!({ "text": ANTIGRAVITY_SYSTEM_INSTRUCTION }),
        json!({ "text": format!("Please ignore following [ignore]{ANTIGRAVITY_SYSTEM_INSTRUCTION}[/ignore]") }),
    ];
    if let Some(system_prompt) = context
        .system_prompt
        .as_deref()
        .filter(|prompt| !prompt.trim().is_empty())
    {
        instruction.push(json!({ "text": sanitize_surrogates(system_prompt) }));
    }
    body.insert(
        "systemInstruction".into(),
        json!({ "role": "user", "parts": instruction }),
    );

    if let Some(tools) = context.tools.as_ref().filter(|tools| !tools.is_empty()) {
        let declarations: Vec<Value> = tools
            .iter()
            .map(|tool| {
                json!({
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": parameters_schema(&tool.parameters),
                })
            })
            .collect();
        body.insert(
            "tools".into(),
            json!([{ "functionDeclarations": declarations }]),
        );
    }

    if let Some(session_id) = &options.base.session_id {
        body.insert("sessionId".into(), json!(session_id));
    }

    if is_gemini3_pro_model(&model.id) || is_gemini3_flash_model(&model.id) {
        let parts = body
            .get_mut("contents")
            .and_then(Value::as_array_mut)
            .into_iter()
            .flatten()
            .filter_map(|content| content.get_mut("parts").and_then(Value::as_array_mut))
            .flatten()
            .filter_map(Value::as_object_mut);
        for part in parts {
            if part.contains_key("functionCall") && !part.contains_key("thoughtSignature") {
                part.insert("thoughtSignature".into(), json!(UNSIGNED_CALL_SIGNATURE));
            }
        }
    }

    json!({
        "model": model.id,
        "project": project_id,
        "request": request,
        "requestType": "agent",
        "userAgent": "antigravity",
        "requestId": format!("agent-{}-{:x}", now_ms(), rand::random::<u64>()),
    })
}

/// A tool's JSON Schema as the backend's `parameters` schema: upper-case
/// types, none of the keywords it refuses, `required` limited to the
/// declared properties, and every array given an item type.
fn parameters_schema(schema: &Value) -> Value {
    let Value::Object(map) = schema else {
        return match schema {
            Value::Array(items) => Value::Array(items.iter().map(parameters_schema).collect()),
            other => other.clone(),
        };
    };
    let properties = map.get("properties").and_then(Value::as_object);
    let mut out = Map::new();
    for (key, value) in map {
        let converted = match key.as_str() {
            "additionalProperties"
            | "$schema"
            | "$id"
            | "$comment"
            | "$ref"
            | "$defs"
            | "definitions"
            | "const"
            | "contentMediaType"
            | "contentEncoding"
            | "if"
            | "then"
            | "else"
            | "not"
            | "patternProperties"
            | "unevaluatedProperties"
            | "unevaluatedItems"
            | "dependentRequired"
            | "dependentSchemas"
            | "propertyNames"
            | "minContains"
            | "maxContains" => continue,
            "type" => value
                .as_str()
                .map_or_else(|| value.clone(), |kind| json!(kind.to_ascii_uppercase())),
            "properties" => Value::Object(
                properties
                    .into_iter()
                    .flatten()
                    .map(|(name, property)| (name.clone(), parameters_schema(property)))
                    .collect(),
            ),
            "items" | "anyOf" | "oneOf" | "allOf" => parameters_schema(value),
            "required" => {
                let required: Vec<Value> = value
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|name| {
                        properties.is_none_or(|properties| {
                            name.as_str()
                                .is_some_and(|name| properties.contains_key(name))
                        })
                    })
                    .cloned()
                    .collect();
                if required.is_empty() {
                    continue;
                }
                Value::Array(required)
            }
            _ => value.clone(),
        };
        out.insert(key.clone(), converted);
    }
    if out.get("type") == Some(&json!("ARRAY")) && !out.contains_key("items") {
        out.insert("items".into(), json!({ "type": "STRING" }));
    }
    Value::Object(out)
}

/// Post the request to the model's endpoint, then to production when
/// the first refuses the account or does not know the model.
async fn send_request(
    model: &Model,
    options: &GoogleOptions,
    token: &str,
    body: &str,
) -> Result<HttpResponse, ProviderError> {
    Client::Antigravity.refresh_if_stale();
    let mut endpoint = if model.base_url.is_empty() {
        DAILY_ENDPOINT
    } else {
        model.base_url.trim_end_matches('/')
    };
    loop {
        let mut headers = vec![
            ("Authorization".to_string(), format!("Bearer {token}")),
            ("User-Agent".to_string(), antigravity_user_agent()),
            ("Content-Type".to_string(), "application/json".to_string()),
            ("Accept".to_string(), "text/event-stream".to_string()),
        ];
        let extra = model.headers.iter().flatten();
        for (name, value) in extra.chain(options.base.headers.iter().flatten()) {
            headers.retain(|(existing, _)| !existing.eq_ignore_ascii_case(name));
            headers.push((name.clone(), value.clone()));
        }
        let response = send(RequestOptions {
            method: reqwest::Method::POST,
            url: format!("{endpoint}/v1internal:streamGenerateContent?alt=sse"),
            headers,
            body: Some(body.to_string()),
            signal: options.base.signal.clone(),
            timeout_ms: options.base.timeout_ms,
            connection: crate::utils_inner::stream_failure::ConnectionErrorProfile::RawFetch,
            transport: crate::utils_inner::http::Transport::Http1,
        })
        .await?;
        if let Some(on_response) = &options.base.on_response {
            on_response(
                crate::types::ProviderResponse {
                    status: response.status,
                    headers: response.headers.clone().into_iter().collect(),
                },
                model,
            );
        }
        if endpoint == PROD_ENDPOINT || !matches!(response.status, 403 | 404) {
            return Ok(response);
        }
        endpoint = PROD_ENDPOINT;
    }
}

async fn run_stream(
    model: &Model,
    context: &Context,
    options: &GoogleOptions,
    output: &mut AssistantMessage,
    writer: &AssistantMessageEventWriter,
) -> Result<(), ProviderError> {
    let (token, project_id) = options
        .base
        .api_key
        .as_deref()
        .and_then(parse_google_antigravity_api_key)
        .ok_or_else(|| {
            ProviderError::Message(
                "Google Antigravity needs its subscription sign-in. Sign in again.".to_string(),
            )
        })?;
    let aborted = || {
        options
            .base
            .signal
            .as_ref()
            .is_some_and(tokio_util::sync::CancellationToken::is_cancelled)
    };
    if aborted() {
        return Err(ProviderError::Aborted);
    }

    let mut params = build_request(model, context, options, &project_id);
    if let Some(on_payload) = &options.base.on_payload {
        if let Some(next) = on_payload(params.clone(), model) {
            params = next;
        }
    }
    let mut response = send_request(model, options, &token, &params.to_string()).await?;

    if response.status >= 400 {
        let body = response.read_all_text().await.unwrap_or_default();
        return Err(ProviderError::from_http_status_body(
            response.status,
            &body,
            response.headers.clone(),
        ));
    }

    writer.push(AssistantMessageEvent::Start {
        partial: output.clone(),
    });

    let mut state = GoogleStreamState::new();
    let mut decoder = SseDecoder::new();
    let mut handle = |data: &str, output: &mut AssistantMessage| -> Result<(), ProviderError> {
        if data.trim().is_empty() {
            return Ok(());
        }
        let chunk = parse_json_with_repair(data).map_err(|error| {
            ProviderError::Message(format!("Could not parse Antigravity SSE chunk: {error}"))
        })?;
        if let Some(error) = chunk.get("error") {
            return Err(ProviderError::Message(
                error["message"]
                    .as_str()
                    .map_or_else(|| error.to_string(), str::to_string),
            ));
        }
        match chunk.get("response") {
            Some(response) => state
                .handle_chunk(response, model, output, writer)
                .map_err(ProviderError::Message),
            None => Ok(()),
        }
    };
    while let Some(text) = response.next_text().await? {
        for sse in decoder.push_text(&text) {
            handle(&sse.data, output)?;
        }
    }
    for sse in decoder.finish() {
        handle(&sse.data, output)?;
    }
    state.finish(output, writer);

    if aborted() {
        return Err(ProviderError::Aborted);
    }
    if matches!(output.stop_reason, StopReason::Aborted | StopReason::Error) {
        return Err(ProviderError::StreamFailure(
            stream_failure_from_stop_reason(output.stop_reason_raw.as_deref(), None),
        ));
    }
    Ok(())
}

/// Registry provider for the `google-antigravity` API.
pub struct GoogleAntigravityProvider;

impl Provider for GoogleAntigravityProvider {
    fn api(&self) -> &str {
        API_GOOGLE_ANTIGRAVITY
    }

    fn stream(
        &self,
        model: &Model,
        context: &Context,
        options: Option<&StreamOptions>,
    ) -> AssistantMessageEventStream {
        let options = options.map(|base| GoogleOptions::from_base(base.clone()));
        stream_google_antigravity(model, context, options.as_ref())
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: Option<&SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        stream_google_antigravity(model, context, Some(&simple_google_options(model, options)))
    }
}
