use serde_json::{json, Value};
use tauri::{AppHandle, Manager};

use crate::api::{api_request, ApiRequest};
use crate::chat_manager::execution::{build_provider_extra_fields, RequestSettings};
use crate::chat_manager::request::{
    extract_error_message, extract_gemini_content, extract_reasoning, extract_text, extract_usage,
};
use crate::chat_manager::request_builder::build_chat_request;
use crate::chat_manager::service::require_api_key;
use crate::chat_manager::take_aborted_request;
use crate::chat_manager::tooling::ToolConfig;
use crate::chat_manager::types::{Model, ProviderCredential, Session, Settings, UsageSummary};
use crate::utils::{
    emit_debug, emit_error_event, emit_info, log_error, log_info, log_warn, now_millis,
};

pub(crate) struct ConversationExecutionInput<'a> {
    pub app: &'a AppHandle,
    pub session_id: &'a str,
    pub request_session: &'a Session,
    pub settings: &'a Settings,
    pub model: &'a Model,
    pub credential: &'a ProviderCredential,
    pub messages: &'a Vec<Value>,
    pub stream: bool,
    pub request_id: Option<String>,
    pub operation: &'a str,
    pub log_scope: &'a str,
    pub tool_config: Option<&'a ToolConfig>,
    pub effective_max_tokens: Option<u32>,
}

#[derive(Debug)]
pub(crate) struct ConversationExecutionOutput {
    pub text: String,
    pub reasoning: Option<String>,
    pub gemini_content: Option<Value>,
    pub usage: Option<UsageSummary>,
    pub generated_image_data_urls: Vec<String>,
    pub api_key: String,
}

#[derive(Debug)]
pub(crate) struct ConversationExecutionFailure {
    pub message: String,
    pub usage: Option<UsageSummary>,
}

impl std::fmt::Display for ConversationExecutionFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ConversationExecutionFailure {}

impl ConversationExecutionFailure {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            usage: None,
        }
    }
}

pub(crate) async fn execute_generation(
    input: ConversationExecutionInput<'_>,
) -> Result<ConversationExecutionOutput, ConversationExecutionFailure> {
    let api_key = require_api_key(input.app, input.credential, input.log_scope)
        .map_err(ConversationExecutionFailure::new)?;
    let mut request_settings =
        RequestSettings::resolve(input.request_session, input.model, input.settings);
    // Preserve the first pass's clamped budget — do not restore original max_tokens.
    if let Some(v) = input.effective_max_tokens {
        request_settings.max_tokens = v;
    }

    // -----------------------------------------------------------------------
    // Context-window enforcement (provider-agnostic, token-aware)
    // Ensures input_tokens + reserved_output <= limit by trimming oldest
    // user/assistant turns and clamping output budget. Uses o200k tokenizer.
    // -----------------------------------------------------------------------
    let enforcement = crate::chat_manager::prompting::context_window::enforce_context_window(
        input.messages,
        &request_settings,
        input.model,
        input.credential,
        None,
    )
    .map_err(ConversationExecutionFailure::new)?;

    {
        let dbg = crate::chat_manager::prompting::context_window::debug_info_for_enforcement(
            input.model,
            input.credential,
            &enforcement,
        );
        // Do not log API keys — only model/limit/token counts.
        if enforcement.context_limit != 0 {
            log_info(
                input.app,
                "context_window",
                format!(
                    "enforce model={} provider={} limit={} input={} (orig {}) requested={} final_output={} removed={} final_total={} reasoning_budget={:?}",
                    dbg.model_name,
                    dbg.provider_id,
                    dbg.context_limit,
                    dbg.final_input_tokens,
                    dbg.original_input_tokens,
                    dbg.requested_max_tokens,
                    dbg.final_output_tokens,
                    dbg.removed_count,
                    dbg.final_total_estimate,
                    dbg.reasoning_budget,
                ),
            );
            emit_debug(
                input.app,
                "context_window",
                json!({
                    "model": dbg.model_name,
                    "modelId": dbg.model_id,
                    "providerId": dbg.provider_id,
                    "contextLimit": dbg.context_limit,
                    "requestedMaxTokens": dbg.requested_max_tokens,
                    "reasoningBudget": dbg.reasoning_budget,
                    "originalInputTokens": dbg.original_input_tokens,
                    "finalInputTokens": dbg.final_input_tokens,
                    "availableOutput": dbg.available_output,
                    "finalOutputTokens": dbg.final_output_tokens,
                    "removedCount": dbg.removed_count,
                    "finalTotalEstimate": dbg.final_total_estimate,
                }),
            );
        } else {
            log_info(
                input.app,
                "context_window",
                format!(
                    "no context limit configured for model={} provider={}; skipping enforcement (input ~{} tokens, requested {})",
                    input.model.name,
                    input.credential.provider_id,
                    enforcement.input_tokens,
                    enforcement.requested_output_tokens
                ),
            );
        }
    }

    let effective_messages = &enforcement.messages;
    let mut effective_max_tokens = enforcement.final_output_tokens;

    let extra_body_fields = build_provider_extra_fields(
        &input.credential.provider_id,
        input.request_session,
        input.model,
        input.settings,
        &request_settings,
    );
    let mut built = build_chat_request(
        input.credential,
        &api_key,
        &input.model.name,
        effective_messages,
        None,
        request_settings.temperature,
        request_settings.top_p,
        effective_max_tokens,
        request_settings.context_length,
        input.stream,
        input.request_id.clone(),
        request_settings.frequency_penalty,
        request_settings.presence_penalty,
        request_settings.top_k,
        input.tool_config,
        request_settings.reasoning_enabled,
        request_settings.reasoning_effort.clone(),
        request_settings.reasoning_budget,
        request_settings.prompt_caching_enabled.unwrap_or(false),
        extra_body_fields,
    );

    // Provider-finalization safety: re-estimate after adapter transforms and clamp if still over limit.
    {
        let limit = request_settings
            .context_length
            .unwrap_or(crate::chat_manager::execution::FALLBACK_CONTEXT_LENGTH);
        let margin = crate::chat_manager::prompting::context_window::context_safety_margin(limit);
        let reestimated_input =
            crate::chat_manager::prompting::context_window::estimate_tokens_for_messages(
                effective_messages,
            );
        let reasoning = request_settings.reasoning_budget.unwrap_or(0);
        let total_needed = reestimated_input
            .saturating_add(effective_max_tokens)
            .saturating_add(margin)
            .saturating_add(reasoning);
        if total_needed > limit {
            let available = limit
                .saturating_sub(reestimated_input)
                .saturating_sub(margin)
                .saturating_sub(reasoning);
            let clamped = available.max(16).min(effective_max_tokens);
            if clamped < effective_max_tokens {
                log_info(
                    input.app,
                    "context_window",
                    format!(
                        "final safety clamp model={} reestimated_input={} limit={} effective_max {} -> {} (total {} > limit)",
                        input.model.name, reestimated_input, limit, effective_max_tokens, clamped, total_needed
                    ),
                );
                effective_max_tokens = clamped;
                if let Some(obj) = built.body.as_object_mut() {
                    for key in [
                        "max_tokens",
                        "max_completion_tokens",
                        "max_output_tokens",
                        "num_predict",
                        "maxTokens",
                    ] {
                        if obj.contains_key(key) {
                            obj.insert(key.to_string(), json!(clamped));
                        }
                    }
                    // Some providers nest under `generationConfig` or `options`
                    if let Some(opts) = obj.get_mut("options").and_then(|v| v.as_object_mut()) {
                        if opts.contains_key("num_predict") {
                            opts.insert("num_predict".to_string(), json!(clamped));
                        }
                    }
                }
            }
        }
    }

    let request_started_at = now_millis().unwrap_or_default();
    emit_info(
        input.app,
        "sending_request",
        json!({
            "operation": input.operation,
            "sessionId": input.session_id,
            "providerId": input.credential.provider_id,
            "model": input.model.name,
            "stream": input.stream,
            "requestId": input.request_id,
            "endpoint": built.url,
            "requestStartedAt": request_started_at,
            "requestBody": &built.body,
            "requestSettings": {
                "temperature": request_settings.temperature,
                "topP": request_settings.top_p,
                "maxTokens": request_settings.max_tokens,
                "contextLength": request_settings.context_length,
                "frequencyPenalty": request_settings.frequency_penalty,
                "presencePenalty": request_settings.presence_penalty,
                "topK": request_settings.top_k,
                "reasoningEnabled": request_settings.reasoning_enabled,
                "reasoningEffort": request_settings.reasoning_effort,
                "reasoningBudget": request_settings.reasoning_budget,
            },
        }),
    );

    let response = api_request(
        input.app.clone(),
        ApiRequest {
            url: built.url,
            method: Some("POST".into()),
            headers: Some(built.headers),
            query: None,
            body: Some(built.body),
            timeout_ms: Some(crate::transport::DEFAULT_REQUEST_TIMEOUT_MS),
            stream: Some(built.stream),
            request_id: built.request_id,
            provider_id: Some(input.credential.provider_id.clone()),
            cache_key: Some(input.session_id.to_string()),
        },
    )
    .await
    .map_err(|message| {
        log_error(input.app, input.log_scope, &message);
        ConversationExecutionFailure::new(message)
    })?;

    emit_info(
        input.app,
        "response",
        json!({
            "operation": input.operation,
            "sessionId": input.session_id,
            "requestId": input.request_id,
            "status": response.status,
            "ok": response.ok,
            "model": input.model.name,
            "elapsedMs": now_millis().unwrap_or_default().saturating_sub(request_started_at),
        }),
    );

    if !response.ok {
        let fallback = format!("Provider returned status {}", response.status);
        let message = extract_error_message(response.data()).unwrap_or(fallback.clone());
        let usage = extract_usage(response.data());
        emit_error_event(
            input.app,
            "provider_error",
            json!({
                "operation": input.operation,
                "sessionId": input.session_id,
                "requestId": input.request_id,
                "status": response.status,
                "message": message,
                "usage": usage,
                "model": input.model.name,
            }),
        );
        return Err(ConversationExecutionFailure {
            message: if message == fallback {
                message
            } else {
                format!("{} (status {})", message, response.status)
            },
            usage,
        });
    }

    if take_aborted_request(input.app, input.request_id.as_deref()) {
        return Err(ConversationExecutionFailure::new("Request aborted by user"));
    }

    let generated_image_data_urls = match response.data() {
        Value::String(value) if value.contains("data:") => {
            crate::chat_manager::sse::accumulate_image_data_urls_from_sse(value)
        }
        value => crate::chat_manager::sse::image_data_urls_from_response(value),
    };
    let text =
        extract_text(response.data(), Some(&input.credential.provider_id)).unwrap_or_default();
    let reasoning = extract_reasoning(response.data(), Some(&input.credential.provider_id));
    let usage = extract_usage(response.data());
    let gemini_content = crate::chat_manager::provider_adapter::is_gemini_format_provider(
        &input.credential.provider_id,
    )
    .then(|| extract_gemini_content(response.data()))
    .flatten();

    if text.trim().is_empty() && generated_image_data_urls.is_empty() {
        let message = if reasoning
            .as_ref()
            .is_some_and(|value| !value.trim().is_empty())
        {
            "Model completed reasoning but generated no response text. This may indicate the model ran out of tokens or encountered an issue during generation."
        } else {
            "Empty response from provider"
        };
        return Err(ConversationExecutionFailure::new(message));
    }

    if let Some(filter) = input
        .app
        .try_state::<crate::content_filter::ContentFilter>()
    {
        if filter.is_enabled() {
            let result = filter.check_text(&text);
            if result.blocked {
                log_warn(
                    input.app,
                    input.log_scope,
                    format!(
                        "Content blocked by Pure Mode (score={:.1}, terms={:?})",
                        result.score, result.matched_terms
                    ),
                );
                return Err(ConversationExecutionFailure::new(
                    "Response blocked by Pure Mode. Try rephrasing your message.",
                ));
            }
        }
    }

    Ok(ConversationExecutionOutput {
        text,
        reasoning,
        gemini_content,
        usage,
        generated_image_data_urls,
        api_key,
    })
}
