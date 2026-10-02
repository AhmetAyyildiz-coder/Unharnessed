//! The in-process ACP picker machinery (the TS #2455 port's engine side):
//! the session's picker state and serialized config queue, the
//! `session/set_config_option` handler over the session engine, the
//! model/thinking-level/fast-mode application semantics, the agent-run
//! refresh that republishes `config_option_update` on change, and the
//! re-discovery after a provider login or logout (or a catalog refresh).
//!
//! The wire-shape builders and the compare-gated publisher live in
//! [`super::config_options`]; this module owns the in-process state and
//! the apply/refresh lifecycle over [`AcpModeState`] and [`AcpSession`].

use std::sync::Arc;

use serde_json::{json, Value};
use tokio::sync::Mutex;

use super::config_options::{
    acp_model_registry, config_options_value, discover_available_models, fast_mode_option,
    model_value, publish_config_options, session_config_options, PickerModel, SessionConfigOption,
    FAST_MODE,
};
use super::jsonrpc;
use super::producer;
use super::session::AcpSession;
use super::types;
use super::{internal_error, AcpModeState, ConnectionState};

/// The in-process picker state: the published options, the discovered
/// models, and the serialized queue every config operation runs through
/// (TS `enqueueConfig`/`configTask` — close drains it before the
/// producer fences).
pub(crate) struct InProcessConfig {
    pub(crate) published: tokio::sync::Mutex<Vec<SessionConfigOption>>,
    pub(crate) models: tokio::sync::Mutex<Vec<uh_types::ai::Model>>,
}

/// Build the admitted session's picker state (TS `session/new`'s
/// `entry.configOptions`/`entry.models`): discovery must not block
/// admission (a failure degrades to an empty list), and the options come
/// from the live model, its supported levels, and the agent's resolved
/// thinking level.
pub(super) async fn admit_session_config(mode: &AcpModeState) -> Arc<InProcessConfig> {
    let models = tokio::task::spawn_blocking({
        let agent_dir = Arc::clone(&mode.agent_dir);
        move || discover_available_models(&agent_dir)
    })
    .await
    .ok()
    .and_then(std::result::Result::ok)
    .unwrap_or_default();
    let published = {
        let current = mode.current_model().await;
        let state = mode.engine.session.agent().state().await;
        let thinking_level =
            uh_core::session_engine::provider_adapter::model_thinking_level(state.thinking_level)
                .wire_name()
                .to_string();
        let levels = supported_levels(current.as_ref());
        let mut options = session_config_options(
            current.as_ref().map(PickerModel::from_model),
            &thinking_level,
            &levels,
            &models,
        );
        if let Some(fast) = fast_mode(current.as_ref(), mode).await {
            options.push(fast);
        }
        options
    };
    Arc::new(InProcessConfig {
        published: tokio::sync::Mutex::new(published),
        models: tokio::sync::Mutex::new(models),
    })
}

/// `session/set_config_option`: apply one picker selection and answer the
/// refreshed options (TS #2455). Config work is serialized through the
/// session's queue, so selections and event-driven refreshes observe one
/// another in arrival order.
pub(super) async fn handle_set_config_option(
    id: Value,
    params: Value,
    state: Arc<Mutex<ConnectionState>>,
    mode: AcpModeState,
    tx: producer::FrameSink,
) {
    let params = types::SetConfigOptionParams::parse(&params);
    // The session resolves before the queue (TS reads `session?.id`).
    let resolved = {
        let state = state.lock().await;
        state
            .session
            .as_ref()
            .filter(|entry| entry.session.id == params.session_id)
            .map(|entry| (Arc::clone(&entry.session), Arc::clone(&entry.config)))
    };
    let Some((session, config)) = resolved else {
        let _ = tx.send(jsonrpc::error_response(
            &id,
            jsonrpc::INVALID_PARAMS,
            "Invalid params",
            Some(&json!({ "reason": format!("Unknown ACP session: {}", params.session_id) })),
        ));
        return;
    };
    // One config operation at a time (TS `enqueueConfig`): the queue also
    // serializes the agent-run refreshes and the trigger-consuming arms.
    let _guard = mode.config_queue.lock().await;
    // The queue can outlive the session: a close admitted between the
    // resolution and the run refuses further config work (TS
    // `sessionCloseInFlight`).
    let live = {
        let state = state.lock().await;
        !state.session_close_in_flight
            && state
                .session
                .as_ref()
                .is_some_and(|current| current.session.id == session.id)
    };
    let outcome = if live {
        apply_in_process_config(&session, &config, &mode, &params.config_id, &params.value).await
    } else {
        Err(ConfigOptionError::invalid_params(
            "ACP session is closed or closing",
        ))
    };
    let options = match outcome {
        Ok(options) => options,
        Err(error) => {
            let _ = tx.send(error.response(&id));
            return;
        }
    };
    let _ = tx.send(jsonrpc::response(&id, &config_options_value(&options)));
}

/// One failed config operation: the TS handler's `RequestError` shape.
#[derive(Debug)]
enum ConfigOptionError {
    InvalidParams(String),
    Internal(String),
}

impl ConfigOptionError {
    fn invalid_params(reason: &str) -> ConfigOptionError {
        ConfigOptionError::InvalidParams(reason.to_string())
    }

    /// The JSON-RPC error frame (the TS `invalidParams` data shape).
    fn response(self, id: &Value) -> Value {
        match self {
            ConfigOptionError::InvalidParams(reason) => jsonrpc::error_response(
                id,
                jsonrpc::INVALID_PARAMS,
                "Invalid params",
                Some(&json!({ "reason": reason })),
            ),
            ConfigOptionError::Internal(details) => internal_error(id, &details),
        }
    }
}

/// Apply one selection (TS `session/set_config_option`'s handler body):
/// validate against the live session state, apply through the engine, then
/// return the refreshed options with the change published.
async fn apply_in_process_config(
    session: &Arc<AcpSession>,
    config: &Arc<InProcessConfig>,
    mode: &AcpModeState,
    config_id: &str,
    value: &Value,
) -> Result<Vec<SessionConfigOption>, ConfigOptionError> {
    if config_id == FAST_MODE {
        let enabled = value
            .as_bool()
            .ok_or_else(|| ConfigOptionError::invalid_params("Fast mode takes a boolean value"))?;
        apply_fast_mode(mode, enabled).await?;
        return Ok(refresh_in_process_config(session, config, mode).await);
    }
    match (config_id, value.as_str()) {
        ("model", Some(value)) => {
            // The no-op check reads the agent's live model (TS
            // `getState().model`): the current model re-selected refreshes
            // only, no discovery (a resync during a discovery outage
            // still answers).
            let live = session.agent().state().await;
            if model_value(&live.model.provider, &live.model.id) == value {
                return Ok(refresh_in_process_config(session, config, mode).await);
            }
            // Discover the available models (TS `getAvailableModels`): the
            // registry the composition resolved against. Discovery failures
            // are the handler's "try again later" invalid-params.
            let models = tokio::task::spawn_blocking({
                let agent_dir = Arc::clone(&mode.agent_dir);
                move || discover_available_models(&agent_dir)
            })
            .await
            .map_err(|_| ConfigOptionError::Internal("model discovery task failed".to_string()))?
            .map_err(|_| {
                ConfigOptionError::invalid_params("Model discovery is unavailable; try again later")
            })?;
            let model = models
                .iter()
                .find(|model| model_value(&model.provider, &model.id) == value)
                .cloned()
                .ok_or_else(|| {
                    ConfigOptionError::invalid_params(&format!("Unavailable model: {value}"))
                })?;
            apply_in_process_model_switch(session, mode, model)
                .await
                .map_err(|error| {
                    ConfigOptionError::Internal(format!("model switch failed: {error:#}"))
                })?;
            *config.models.lock().await = models;
            Ok(refresh_in_process_config(session, config, mode).await)
        }
        ("thought_level", Some(value)) => {
            let current = mode.current_model().await;
            let levels = supported_levels(current.as_ref());
            // #2858's map-driven capability: an addressable
            // thinking-level map marks the model thinking-capable even
            // when the coarse `reasoning` flag is false, so the flag
            // must not veto a level the map advertises.
            let supported = current
                .as_ref()
                .is_some_and(uh_ai::models::supports_thinking)
                && levels.iter().any(|level| level == value);
            if !supported {
                return Err(ConfigOptionError::invalid_params(&format!(
                    "Unsupported reasoning effort: {value}"
                )));
            }
            apply_in_process_thinking_level(session, mode, value).await?;
            Ok(refresh_in_process_config(session, config, mode).await)
        }
        _ => Err(ConfigOptionError::invalid_params(&format!(
            "Invalid configuration option: {config_id}"
        ))),
    }
}

/// The model's supported thinking levels as wire names (TS
/// `availableThinkingLevels`).
fn supported_levels(model: Option<&uh_types::ai::Model>) -> Vec<String> {
    model
        .map(|model| {
            uh_ai::models::get_supported_thinking_levels(model)
                .into_iter()
                .map(|level| level.wire_name().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Switch the session's model (TS `setModel`): swap the provider stream
/// target, set the agent's model, record the durable row and the settings
/// default, and restore the thinking level against the new model.
async fn apply_in_process_model_switch(
    session: &Arc<AcpSession>,
    mode: &AcpModeState,
    model: uh_types::ai::Model,
) -> anyhow::Result<()> {
    // The request key for the switched model: the same registry resolution
    // the create-time composition ran (TS re-registers the stream with the
    // model's own auth).
    let mut registry = tokio::task::spawn_blocking({
        let agent_dir = Arc::clone(&mode.agent_dir);
        move || acp_model_registry(&agent_dir)
    })
    .await
    .map_err(|_| anyhow::anyhow!("model registry task failed"))?;
    let resolved = registry.get_api_key_and_headers(&model, model.headers.as_ref());
    // An unresolvable selection refuses the switch BEFORE anything
    // moves (the RPC path's own rule, `apply_model_selection`):
    // `get_api_key_and_headers` can answer `ok: false` even for a model
    // discovery admits, and installing it would leave the provider
    // target without usable authentication — the NEXT turn would fail
    // instead of this request. TS's `setModel` refuses the same way
    // ("No API key for ...").
    if !resolved.ok {
        anyhow::bail!(resolved.error.unwrap_or_else(|| {
            format!(
                "Model is not available: no credentials resolved for {}/{}",
                model.provider, model.id
            )
        }));
    }
    // The previous live state, for the rollback a failed durable write
    // takes (a failure would otherwise leave the target switched and the
    // session's model identity split).
    let previous_target = mode
        .provider_target
        .read()
        .expect("provider target lock")
        .clone();
    let previous_model = mode.current_model().await;
    let previous_api_key = mode.current_api_key().await;
    // The service-tier preference survives the switch, clamped to what the
    // selected model serves.
    let service_tier = request_service_tier(&model, *mode.service_tier.lock().await);
    // The stream target swaps in place: the next turn streams on the
    // selected model.
    {
        let mut target = mode.provider_target.write().expect("provider target lock");
        match target.as_mut() {
            Some(target) => {
                target.api_key.clone_from(&resolved.api_key);
                target.model = model.clone();
                target.headers.clone_from(&resolved.headers);
                target.service_tier = service_tier;
            }
            None => {
                *target = Some(uh_core::session_engine::provider_adapter::ProviderTarget {
                    api_key: resolved.api_key.clone(),
                    model: model.clone(),
                    service_tier,
                    headers: resolved.headers.clone(),
                });
            }
        }
    }
    // The agent's model plus the durable `model_change` row (TS records
    // every switch, even to the current model). A failed persist rolls
    // the live state back: the client sees the refusal and the session
    // keeps running on the model it still reports.
    if let Err(error) = mode
        .engine
        .session
        .set_model(&model, &model.provider, &model.id)
        .await
    {
        roll_back_model(
            session,
            mode,
            previous_model.as_ref(),
            previous_target,
            previous_api_key,
        )
        .await;
        anyhow::bail!(error);
    }
    // The session's live request key follows the switch (the
    // session-command executors authenticate against the switched
    // model's provider).
    *mode.api_key.lock().await = resolved.api_key.clone();
    // The live model slot follows with the same step, so the switch's
    // live trio (provider target, request key, model) never splits:
    // `current_model()` — picker validation, `/compact` —
    // always answers the model the turns stream on, before OR after a
    // later step fails (TS's `setModel` assigns the session model before
    // it persists, so a failed write rejects with the switched session
    // applied, never a split one).
    *mode.model.lock().await = Some(model.clone());
    // TS `session.setModel` persists the default provider/model so the
    // next session starts on the switched model. The settings store is
    // file-backed (reads, an atomic write, and lock-contention retries
    // that sleep), so the write runs off the Tokio worker — the config
    // queue this handler holds must not park a runtime thread other ACP
    // work shares (`spawn_blocking`'s contract).
    let persist = {
        let cwd = Arc::clone(&mode.actual_cwd);
        let agent_dir = Arc::clone(&mode.agent_dir);
        let provider = model.provider.clone();
        let id = model.id.clone();
        tokio::task::spawn_blocking(move || {
            let mut settings =
                uh_core::settings::SettingsManager::create(cwd.as_path(), agent_dir.as_path());
            settings.set_default_model_and_provider(&provider, &id)
        })
        .await
        .map_err(|error| anyhow::anyhow!("the settings persist task failed: {error}"))?
    };
    if let Err(error) = persist {
        anyhow::bail!(error);
    }
    // The thinking level follows the switch (TS
    // `_getThinkingLevelForModelSwitch` + `setThinkingLevel`): the current
    // level when the pre-switch model is thinking-capable, else the
    // settings default — always clamped to the switched model. TS reads
    // the decision off `this.model` BEFORE the assignment, so the
    // pre-switch capture (`previous_model`) is the source, not the live
    // slot (which already names the switched model). The capability is
    // #2858's map-driven one: an addressable thinking-level map marks
    // the model thinking-capable even when the coarse `reasoning` flag
    // is false.
    let current_level = session.agent().state().await.thinking_level;
    let requested = if previous_model
        .as_ref()
        .is_some_and(uh_ai::models::supports_thinking)
    {
        uh_core::session_engine::provider_adapter::model_thinking_level(current_level)
    } else {
        // The settings read runs off the worker like the write above.
        let default = {
            let cwd = Arc::clone(&mode.actual_cwd);
            let agent_dir = Arc::clone(&mode.agent_dir);
            tokio::task::spawn_blocking(move || {
                let settings =
                    uh_core::settings::SettingsManager::create(cwd.as_path(), agent_dir.as_path());
                settings.get_default_thinking_level()
            })
            .await
            .map_err(|error| anyhow::anyhow!("the settings read task failed: {error}"))?
        };
        default.map_or(
            uh_types::ai::ModelThinkingLevel::Medium,
            uh_core::settings::ThinkingLevelSetting::model_level,
        )
    };
    let effective = uh_ai::models::clamp_thinking_level(&model, requested);
    // The failed-durable-write fallback for the switched model: the
    // pre-switch level clamped to the model the switch installed (the
    // raw pre-switch level can be one it does not support).
    let restore = uh_core::session_engine::provider_adapter::map_thinking_level(
        uh_ai::models::clamp_thinking_level(
            &model,
            uh_core::session_engine::provider_adapter::model_thinking_level(current_level),
        ),
    );
    apply_level_change(
        session,
        mode,
        effective,
        uh_ai::models::supports_thinking(&model),
        restore,
    )
    .await
    .map_err(|error| anyhow::anyhow!("{error:?}"))?;
    Ok(())
}

/// Roll the live session back to its previous model state after a failed
/// durable `model_change` write: the provider target, the agent's model,
/// and the mode's live slots all return to the pre-switch pair the client
/// still sees.
async fn roll_back_model(
    session: &Arc<AcpSession>,
    mode: &AcpModeState,
    previous_model: Option<&uh_types::ai::Model>,
    previous_target: Option<uh_core::session_engine::provider_adapter::ProviderTarget>,
    previous_api_key: Option<String>,
) {
    {
        let mut target = mode.provider_target.write().expect("provider target lock");
        *target = previous_target;
    }
    *mode.api_key.lock().await = previous_api_key;
    if let Some(model) = previous_model {
        let wire: Option<uh_agent::types::Model> = serde_json::to_value(model)
            .ok()
            .and_then(|value| serde_json::from_value(value).ok());
        if let Some(wire) = wire {
            session.agent().set_model(wire).await;
        }
        *mode.model.lock().await = Some(model.clone());
    }
}

/// Apply a thinking-level selection (TS `setThinkingLevel`): the level was
/// validated against the model's supported levels, so the agent follows it
/// and the durable row plus the settings default record the change.
async fn apply_in_process_thinking_level(
    session: &Arc<AcpSession>,
    mode: &AcpModeState,
    level: &str,
) -> Result<(), ConfigOptionError> {
    let parsed = uh_types::ai::thinking_level_from_str(level)
        .ok_or_else(|| ConfigOptionError::invalid_params("Invalid thinking level"))?;
    let current = mode.current_model().await;
    // #2858's map-driven capability (see the validation gate).
    let thinking = current
        .as_ref()
        .is_some_and(uh_ai::models::supports_thinking);
    // The standalone change's fallback is the level it replaces — the
    // model did not change, so it is supported.
    let restore = session.agent().state().await.thinking_level;
    apply_level_change(session, mode, parsed, thinking, restore).await
}

/// The shared level application: agent + durable row when the effective
/// level changed, settings default when the model has a thinking surface
/// or the level is not `off` (TS `setThinkingLevel`'s persist gate, gated
/// on #2858's map-driven capability rather than the coarse flag). A
/// failed durable `thinking_level_change` write rolls the live agent back
/// to `restore`: the caller names a level the model the agent now runs
/// supports — a standalone change returns to the level it replaced; a
/// model switch returns to the pre-switch level CLAMPED to the switched
/// model (the raw pre-switch level can be one it does not support).
async fn apply_level_change(
    session: &Arc<AcpSession>,
    mode: &AcpModeState,
    level: uh_types::ai::ModelThinkingLevel,
    thinking: bool,
    restore: uh_agent::types::ThinkingLevel,
) -> Result<(), ConfigOptionError> {
    let mapped = uh_core::session_engine::provider_adapter::map_thinking_level(level);
    let current = session.agent().state().await.thinking_level;
    if current == mapped {
        return Ok(());
    }
    if let Err(error) = mode.engine.session.set_thinking_level(mapped).await {
        session.agent().set_thinking_level(restore).await;
        return Err(ConfigOptionError::Internal(format!(
            "thinking level switch failed: {error:#}"
        )));
    }
    if thinking || level != uh_types::ai::ModelThinkingLevel::Off {
        // The file-backed settings write runs off the Tokio worker, like
        // the model switch's persist (the queue this handler holds must
        // not park a runtime thread).
        let cwd = Arc::clone(&mode.actual_cwd);
        let agent_dir = Arc::clone(&mode.agent_dir);
        let setting = uh_core::settings::ThinkingLevelSetting::from_model_level(level);
        let persist = tokio::task::spawn_blocking(move || {
            let mut settings =
                uh_core::settings::SettingsManager::create(cwd.as_path(), agent_dir.as_path());
            settings.set_default_thinking_level(setting)
        })
        .await
        .map_err(|error| {
            ConfigOptionError::Internal(format!("the settings persist task failed: {error}"))
        })?;
        // TS `setThinkingLevel`'s chain rejects on a failed settings
        // write (the level assignment and the durable row already ran,
        // exactly like the model switch).
        persist.map_err(|error| {
            ConfigOptionError::Internal(format!("thinking level default persist failed: {error:#}"))
        })?;
    }
    Ok(())
}

/// Recompute the options from the live session state and publish the
/// change (TS `refreshConfig`): the picker identity and the thinking
/// level are the agent's own view, so an out-of-band model or level
/// switch still republishes; the supported levels come from the full
/// registry model (discovered or tracked) that matches the live agent
/// model.
async fn refresh_in_process_config(
    session: &Arc<AcpSession>,
    config: &Arc<InProcessConfig>,
    mode: &AcpModeState,
) -> Vec<SessionConfigOption> {
    let tracked = mode.current_model().await;
    let state = session.agent().state().await;
    let thinking_level =
        uh_core::session_engine::provider_adapter::model_thinking_level(state.thinking_level)
            .wire_name()
            .to_string();
    // The picker's identity is the agent's live model (an out-of-band
    // failover switch moves it without the picker slot); the supported
    // levels need the full registry model, so the discovered list (or the
    // tracked slot) supplies the map when the agent's view matches.
    let models = config.models.lock().await;
    let live_model = PickerModel::from_agent_model(&state.model);
    let full_model = models
        .iter()
        .find(|model| model.provider == live_model.provider && model.id == live_model.id)
        .cloned()
        .or_else(|| {
            tracked
                .as_ref()
                .filter(|model| model.provider == live_model.provider && model.id == live_model.id)
                .cloned()
        });
    let levels = match full_model.as_ref() {
        Some(model) => supported_levels(Some(model)),
        // No registry entry: the agent model's reasoning flag alone decides
        // the no-map ladder (the map-less levels the registry would give).
        None if live_model.reasoning => [
            uh_types::ai::ModelThinkingLevel::Off,
            uh_types::ai::ModelThinkingLevel::Minimal,
            uh_types::ai::ModelThinkingLevel::Low,
            uh_types::ai::ModelThinkingLevel::Medium,
            uh_types::ai::ModelThinkingLevel::High,
        ]
        .iter()
        .map(|level| level.wire_name().to_string())
        .collect(),
        None => Vec::new(),
    };
    let mut options = session_config_options(Some(live_model), &thinking_level, &levels, &models);
    drop(models);
    if let Some(fast) = fast_mode(full_model.as_ref(), mode).await {
        options.push(fast);
    }
    publish_config_options(session.producer(), &config.published, options.clone()).await;
    options
}

/// The `fast_mode` toggle for `model`, when it has a priority tier: on
/// while the preference is the priority tier.
async fn fast_mode(
    model: Option<&uh_types::ai::Model>,
    mode: &AcpModeState,
) -> Option<SessionConfigOption> {
    model.filter(|model| uh_types::ai::supports_fast_mode(model))?;
    let preference = *mode.service_tier.lock().await;
    Some(fast_mode_option(
        preference == uh_types::ai::ServiceTier::Priority,
    ))
}

/// The tier a request on `model` carries for the `preference` (TS
/// `clampServiceTier`): nothing for the default tier, so a request
/// without a preference stays as it was.
pub(crate) fn request_service_tier(
    model: &uh_types::ai::Model,
    preference: uh_types::ai::ServiceTier,
) -> Option<uh_types::ai::ServiceTier> {
    uh_types::ai::clamp_service_tier(Some(model), Some(preference))
        .filter(|tier| *tier != uh_types::ai::ServiceTier::Default)
}

/// Apply a fast-mode toggle (TS `/fast`, `setServiceTier`): the priority
/// tier when on, the default one when off. The preference reaches the
/// live target, the durable `service_tier_change` row and the settings
/// default (a fresh session starts on it); a model switch keeps it,
/// clamped to what the selected model serves.
async fn apply_fast_mode(mode: &AcpModeState, enabled: bool) -> Result<(), ConfigOptionError> {
    use uh_types::ai::ServiceTier;
    let model = mode
        .current_model()
        .await
        .filter(uh_types::ai::supports_fast_mode)
        .ok_or_else(|| {
            ConfigOptionError::invalid_params("Fast mode is not available for this model")
        })?;
    let tier = if enabled {
        ServiceTier::Priority
    } else {
        ServiceTier::Default
    };
    let previous = std::mem::replace(&mut *mode.service_tier.lock().await, tier);
    let set_target = |tier| {
        if let Some(target) = mode
            .provider_target
            .write()
            .expect("provider target lock")
            .as_mut()
        {
            target.service_tier = request_service_tier(&model, tier);
        }
    };
    set_target(tier);
    // A failed durable row rolls the live preference back: the client
    // sees the refusal and the next turn keeps the tier it still reports.
    if let Err(error) = mode.engine.session.set_service_tier(Some(tier)).await {
        *mode.service_tier.lock().await = previous;
        set_target(previous);
        return Err(ConfigOptionError::Internal(format!(
            "fast mode switch failed: {error:#}"
        )));
    }
    // The file-backed settings write runs off the Tokio worker, like the
    // other persists under the config queue.
    let cwd = Arc::clone(&mode.actual_cwd);
    let agent_dir = Arc::clone(&mode.agent_dir);
    tokio::task::spawn_blocking(move || {
        let mut settings =
            uh_core::settings::SettingsManager::create(cwd.as_path(), agent_dir.as_path());
        settings.set_default_service_tier(tier)
    })
    .await
    .map_err(|error| {
        ConfigOptionError::Internal(format!("the settings persist task failed: {error}"))
    })?
    .map_err(|error| {
        ConfigOptionError::Internal(format!("fast mode default persist failed: {error:#}"))
    })
}

/// Re-discover the models after a provider login or logout, or a catalog
/// refresh, and republish the pickers when they changed. A current model
/// still available gets its credentials resolved again (a new key or token
/// reaches the live target); one no longer available (an agent started
/// before any login runs on the catalog's first model, a logout) switches
/// to the preferred available model.
pub(super) async fn refresh_after_auth_change(
    state: &Arc<Mutex<ConnectionState>>,
    mode: &AcpModeState,
) {
    let entry = {
        let state = state.lock().await;
        state
            .session
            .as_ref()
            .map(|entry| (Arc::clone(&entry.session), Arc::clone(&entry.config)))
    };
    let Some((session, config)) = entry else {
        return;
    };
    let _guard = mode.config_queue.lock().await;
    let live = {
        let state = state.lock().await;
        !state.session_close_in_flight
            && state
                .session
                .as_ref()
                .is_some_and(|current| current.session.id == session.id)
    };
    if !live {
        return;
    }
    // A failed discovery keeps the last list.
    let discovered = tokio::task::spawn_blocking({
        let agent_dir = Arc::clone(&mode.agent_dir);
        move || discover_available_models(&agent_dir)
    })
    .await
    .ok()
    .and_then(std::result::Result::ok);
    let Some(models) = discovered else {
        return;
    };
    (*config.models.lock().await).clone_from(&models);
    if let Some(current) = mode.current_model().await {
        let available = models
            .iter()
            .any(|model| model.provider == current.provider && model.id == current.id);
        if available {
            refresh_credentials(mode, &current).await;
        } else if let Some(next) = uh_core::models::find_preferred_default_model(&models)
            .or(models.first())
            .cloned()
        {
            if let Err(error) = apply_in_process_model_switch(&session, mode, next).await {
                eprintln!("uh-daemon: switching off an unavailable model failed: {error:#}");
            }
        }
    }
    let _ = refresh_in_process_config(&session, &config, mode).await;
}

/// Resolve the current model's credentials again (a replaced key, a new
/// sign-in), so the next turn sends them.
async fn refresh_credentials(mode: &AcpModeState, current: &uh_types::ai::Model) {
    let Ok(mut registry) = tokio::task::spawn_blocking({
        let agent_dir = Arc::clone(&mode.agent_dir);
        move || acp_model_registry(&agent_dir)
    })
    .await
    else {
        return;
    };
    let resolved = registry.get_api_key_and_headers(current, current.headers.as_ref());
    if !resolved.ok {
        return;
    }
    {
        let mut target = mode.provider_target.write().expect("provider target lock");
        if let Some(target) = target.as_mut().filter(|target| {
            target.model.provider == current.provider && target.model.id == current.id
        }) {
            target.api_key.clone_from(&resolved.api_key);
            target.headers.clone_from(&resolved.headers);
        }
    }
    *mode.api_key.lock().await = resolved.api_key;
}

/// Wire the picker refresh to the end of every agent run (the TS
/// `agent_end` trigger): the refresh is fire-and-forget through the
/// serialized queue, so the agent loop never waits on config work.
pub(super) async fn wire_config_refresh(
    session: &Arc<AcpSession>,
    config: Arc<InProcessConfig>,
    mode: AcpModeState,
) -> uh_agent::agent::Subscription {
    let agent = Arc::clone(session.agent());
    // The owned clones the 'static listener captures (the borrow ends here).
    let session = Arc::clone(session);
    agent
        .subscribe(move |event, _signal| {
            let session = Arc::clone(&session);
            let config = Arc::clone(&config);
            let mode = mode.clone();
            Box::pin(async move {
                if matches!(event, uh_agent::types::AgentEvent::AgentEnd { .. }) {
                    tokio::spawn(async move {
                        let _guard = mode.config_queue.lock().await;
                        let _ = refresh_in_process_config(&session, &config, &mode).await;
                    });
                }
                Ok(())
            })
        })
        .await
}
