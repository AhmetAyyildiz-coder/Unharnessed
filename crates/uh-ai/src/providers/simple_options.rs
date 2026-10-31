//! Shared simple-stream option assembly.
//! Ported from `packages/ai/src/providers/simple-options.ts`.

use crate::types::{
    Model, ModelThinkingLevel, SimpleStreamOptions, StreamOptions, ThinkingBudgets,
};

/// The output room the compaction trigger keeps free under the context
/// window (TS `buildBaseOptions` capped every request here). Requests ask for
/// the model's whole max output and are clamped to the room the context
/// leaves, so compaction only keeps this much.
pub const COMPACTION_OUTPUT_RESERVE: u64 = 32_000;

/// The smallest output budget a request may keep after clamping (TS
/// `adjustMaxTokensForThinking`: `minOutputTokens`).
pub const MIN_OUTPUT_TOKENS: u64 = 1_024;

/// The default per-request output budget for a model: its declared max
/// output, or `None` when the model declares none (providers that default
/// server-side). Reasoning counts against it, so a lower cap cuts long
/// thinking short.
#[must_use]
pub fn default_request_max_tokens(model: &Model) -> Option<u64> {
    (model.max_tokens > 0).then_some(model.max_tokens)
}

pub fn build_base_options(
    model: &Model,
    options: Option<&SimpleStreamOptions>,
    api_key: Option<&str>,
) -> StreamOptions {
    let base = options
        .map(|options| options.base.clone())
        .unwrap_or_default();
    StreamOptions {
        temperature: base.temperature,
        max_tokens: match base.max_tokens {
            Some(tokens) => Some(tokens),
            None => default_request_max_tokens(model),
        },
        signal: base.signal,
        api_key: Some(
            api_key
                .map(std::string::ToString::to_string)
                .unwrap_or_default(),
        )
        .filter(|key| !key.is_empty())
        .or(base.api_key),
        transport: base.transport,
        service_tier: base.service_tier,
        cache_retention: base.cache_retention,
        session_id: base.session_id,
        on_payload: base.on_payload,
        on_response: base.on_response,
        headers: base.headers,
        timeout_ms: base.timeout_ms,
        metadata: base.metadata,
    }
}

/// Clamp `xhigh`/`max` to `high` (mirrors `clampReasoning`).
pub fn clamp_reasoning(effort: ModelThinkingLevel) -> ModelThinkingLevel {
    match effort {
        ModelThinkingLevel::Xhigh | ModelThinkingLevel::Max => ModelThinkingLevel::High,
        other => other,
    }
}

/// Budget-based thinking token adjustment (mirrors `adjustMaxTokensForThinking`).
///
/// Returns an error mirroring the TS throw when there is not enough room for
/// thinking tokens plus the response.
pub fn adjust_max_tokens_for_thinking(
    base_max_tokens: u64,
    model_max_tokens: u64,
    reasoning_level: ModelThinkingLevel,
    custom_budgets: Option<&ThinkingBudgets>,
) -> Result<(u64, u64), String> {
    let default_budgets = ThinkingBudgets {
        minimal: Some(1024),
        low: Some(2048),
        medium: Some(8192),
        high: Some(16384),
    };
    let budgets = match custom_budgets {
        Some(custom) => ThinkingBudgets {
            minimal: custom.minimal.or(default_budgets.minimal),
            low: custom.low.or(default_budgets.low),
            medium: custom.medium.or(default_budgets.medium),
            high: custom.high.or(default_budgets.high),
        },
        None => default_budgets,
    };
    let min_output_tokens = MIN_OUTPUT_TOKENS;
    let min_thinking_tokens = 1024u64;
    let level = clamp_reasoning(reasoning_level);
    let level_budget = match level {
        ModelThinkingLevel::Minimal => budgets.minimal,
        ModelThinkingLevel::Low => budgets.low,
        ModelThinkingLevel::Medium => budgets.medium,
        ModelThinkingLevel::High | ModelThinkingLevel::Xhigh | ModelThinkingLevel::Max => {
            budgets.high
        }
        ModelThinkingLevel::Off => None,
    }
    .unwrap_or(min_thinking_tokens);
    let mut thinking_budget = level_budget.max(min_thinking_tokens);
    let max_tokens = (base_max_tokens + thinking_budget).min(model_max_tokens);
    if max_tokens <= min_thinking_tokens {
        return Err(
            "Budget-based thinking requires at least 1024 thinking tokens plus room for the response"
                .to_string(),
        );
    }
    if max_tokens <= thinking_budget {
        thinking_budget = (max_tokens.saturating_sub(min_output_tokens)).max(min_thinking_tokens);
    }
    Ok((max_tokens, thinking_budget))
}

/// The output room the compaction trigger reserves for a request against
/// `model` with this reasoning level: the model's max output capped at
/// [`COMPACTION_OUTPUT_RESERVE`], plus the thinking budget budget-folding
/// providers (Anthropic and Bedrock models without adaptive thinking) add on
/// top of it, capped at the model's declared max output
/// ([`adjust_max_tokens_for_thinking`]). A request may ask for more while the
/// context leaves room; the streaming facade clamps it to that room.
#[must_use]
pub fn compaction_output_reserve(model: &Model, reasoning: ModelThinkingLevel) -> u64 {
    let base = model.max_tokens.min(COMPACTION_OUTPUT_RESERVE);
    if matches!(reasoning, ModelThinkingLevel::Off) || base == 0 {
        return base;
    }
    let budget_folds = match model.api.as_str() {
        "anthropic" => !crate::providers::anthropic::supports_adaptive_thinking(&model.id),
        // The Bedrock fold runs only on Claude models without adaptive
        // thinking (`streamSimpleBedrock`'s own gate).
        "bedrock" => {
            crate::providers::bedrock::is_anthropic_claude_model(model)
                && !crate::providers::bedrock::supports_adaptive_thinking(
                    &model.id,
                    Some(&model.name),
                )
        }
        _ => false,
    };
    if !budget_folds {
        return base;
    }
    match adjust_max_tokens_for_thinking(base, model.max_tokens, reasoning, None) {
        Ok((max_tokens, _thinking_budget)) => max_tokens.max(base),
        Err(_) => base,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn model(api: &str, id: &str, max_tokens: u64) -> Model {
        serde_json::from_value(json!({
            "id": id, "name": id, "api": api, "provider": "p",
            "baseUrl": "http://localhost", "reasoning": true, "input": ["text"],
            "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0},
            "contextWindow": 200_000, "maxTokens": max_tokens,
        }))
        .expect("test model")
    }

    #[test]
    fn budget_folding_providers_add_the_thinking_budget_on_top() {
        // A non-adaptive Anthropic model: `high` folds 16_384 thinking
        // tokens onto the 32_000 base budget (adjustMaxTokensForThinking).
        let anthropic = model("anthropic", "claude-sonnet-4-5", 65_536);
        assert_eq!(
            compaction_output_reserve(&anthropic, ModelThinkingLevel::Off),
            32_000
        );
        assert_eq!(
            compaction_output_reserve(&anthropic, ModelThinkingLevel::Medium),
            32_000 + 8_192
        );
        assert_eq!(
            compaction_output_reserve(&anthropic, ModelThinkingLevel::High),
            32_000 + 16_384
        );
    }

    #[test]
    fn the_fold_caps_at_the_model_max_output() {
        // The model's own ceiling binds before the base + thinking sum.
        let small = model("anthropic", "claude-sonnet-4-5", 36_000);
        assert_eq!(
            compaction_output_reserve(&small, ModelThinkingLevel::High),
            36_000
        );
    }

    #[test]
    fn adaptive_and_non_budget_providers_keep_the_base_budget() {
        // Adaptive-thinking Anthropic models use effort, not budgets.
        let adaptive = model("anthropic", "claude-opus-4-6", 65_536);
        assert_eq!(
            compaction_output_reserve(&adaptive, ModelThinkingLevel::High),
            32_000
        );
        // OpenAI-style reasoning consumes the budget from within, never on
        // top of it.
        let openai = model("openai-completions", "gpt-x", 65_536);
        assert_eq!(
            compaction_output_reserve(&openai, ModelThinkingLevel::High),
            32_000
        );
    }

    #[test]
    fn a_model_without_a_declared_max_output_has_no_budget_to_fold() {
        let bare = model("anthropic", "claude-sonnet-4-5", 0);
        assert_eq!(
            compaction_output_reserve(&bare, ModelThinkingLevel::High),
            0
        );
    }
}
