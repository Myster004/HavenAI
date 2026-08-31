use serde_json::Value;

use crate::chat_manager::execution::RequestSettings;
use crate::chat_manager::types::{Model, ProviderCredential};

/// Minimum useful generation budget. Spec says maintain configurable minimum where possible.
/// We choose 256 as a sensible default; callers may override.
pub const DEFAULT_MIN_GENERATION_TOKENS: u32 = 256;
/// Absolute floor — a provider still needs at least this many tokens to emit something.
pub const ABSOLUTE_MIN_GENERATION_TOKENS: u32 = 16;

#[derive(Debug, Clone)]
pub struct ContextEnforcementResult {
    /// Possibly trimmed messages
    pub messages: Vec<Value>,
    /// Tokens for final input
    pub input_tokens: u32,
    /// Original input tokens before trimming
    pub original_input_tokens: u32,
    /// Context limit used
    pub context_limit: u32,
    /// Requested output before clamping
    pub requested_output_tokens: u32,
    /// Final output budget after clamping
    pub final_output_tokens: u32,
    /// Number of conversation history messages removed
    pub removed_count: usize,
    /// Reasoning budget if any
    pub reasoning_budget: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct DebugContextInfo {
    pub model_name: String,
    pub model_id: String,
    pub provider_id: String,
    pub context_limit: u32,
    pub requested_max_tokens: u32,
    pub reasoning_budget: Option<u32>,
    pub original_input_tokens: u32,
    pub final_input_tokens: u32,
    pub available_output: i64,
    pub final_output_tokens: u32,
    pub removed_count: usize,
    pub final_total_estimate: u32,
}

// ---------------------------------------------------------------------------
// Token counting
// ---------------------------------------------------------------------------

/// Extract plain text from a message content field.
/// Handles both string content and array of {type: text, text: "..."} parts.
pub fn extract_text_from_content(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => {
            let mut out = String::new();
            for part in parts {
                if let Some(obj) = part.as_object() {
                    if obj.get("type").and_then(|t| t.as_str()) == Some("text") {
                        if let Some(t) = obj.get("text").and_then(|v| v.as_str()) {
                            if !out.is_empty() {
                                out.push('\n');
                            }
                            out.push_str(t);
                        }
                    }
                }
            }
            out
        }
        _ => String::new(),
    }
}

pub fn message_text_for_counting(msg: &Value) -> String {
    let role = msg
        .get("role")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let content = msg.get("content");
    let text = content.map(extract_text_from_content).unwrap_or_default();
    // Small overhead for role marker similar to OpenAI counting
    // We prepend role to ensure role tokens are counted.
    format!("{}: {}", role, text)
}

pub fn estimate_tokens_for_messages(messages: &[Value]) -> u32 {
    if messages.is_empty() {
        return 0;
    }
    let texts: Vec<String> = messages.iter().map(message_text_for_counting).collect();

    // Try existing tokenizer; fallback to len/3.5 if unavailable (slightly conservative).
    let counts = match crate::tokens::tokens_count_batch(texts.clone()) {
        Ok(v) => v,
        Err(_) => {
            // Fallback estimation — ~3.5 chars per token is conservative vs 4.
            return texts
                .iter()
                .map(|t| ((t.len() as f32 / 3.5).ceil() as u32).max(1))
                .sum::<u32>()
                + (messages.len() as u32 * 4);
        }
    };
    let mut total: u32 = counts.iter().sum();
    // Add overhead per message (role tags, separators) as OpenAI does (~4 tokens per message)
    total = total.saturating_add((messages.len() as u32).saturating_mul(4));
    // Extra overhead for priming
    total.saturating_add(2)
}

pub fn estimate_tokens_for_text(text: &str) -> u32 {
    let counts = match crate::tokens::tokens_count_batch(vec![text.to_string()]) {
        Ok(v) => v.first().copied().unwrap_or(0),
        Err(_) => (text.len() as f32 / 3.5).ceil() as u32,
    };
    counts.max(1)
}

// ---------------------------------------------------------------------------
// Context limit resolution
// ---------------------------------------------------------------------------

pub fn effective_context_limit(
    request_settings: &RequestSettings,
    _model: &Model,
    _credential: &ProviderCredential,
) -> Option<u32> {
    // Primary source: RequestSettings.context_length which already aggregates
    // session > model > settings.
    request_settings.context_length
}

pub(crate) fn context_safety_margin(context_limit: u32) -> u32 {
    // 256-token minimum plus ~5% of limit, matching SillyTavern-like headroom
    // for tokenizer drift and provider overhead (role markers etc.).
    256.max(context_limit / 20)
}

/// Clamp output budget given available space.
fn clamp_output_budget(requested: u32, available: u32, min_useful: u32) -> u32 {
    if requested <= available {
        requested
    } else {
        // Shrink to available but try to keep at least min_useful if possible.
        // If available < min_useful we still return available (which may be small)
        // The caller may have already trimmed history to try to reach min_useful.
        // Never return 0 if available >0; floor at 1 to avoid provider rejecting 0.
        let clamped = available;
        if clamped == 0 {
            // No space left — provider would reject regardless. Return 1 as floor
            // but caller should have errored earlier when input >= limit.
            1
        } else if clamped < min_useful {
            // Keep clamped as is but log warning at caller. Still return clamped.
            clamped
        } else {
            clamped
        }
    }
}

// ---------------------------------------------------------------------------
// Generic flat-message trimming (provider-agnostic fallback)
// ---------------------------------------------------------------------------

/// Enforce context window on a flat assembled messages array.
///
/// - Preserves all messages with role == "system" or "developer" (prompt, lore, memory)
/// - Preserves the last message (current user message) — never truncated
/// - Trims oldest removable user/assistant messages first
/// - Recalculates token count after each removal
/// - Clamps output budget to available space
pub fn enforce_context_window(
    messages: &[Value],
    request_settings: &RequestSettings,
    model: &Model,
    credential: &ProviderCredential,
    min_useful_tokens: Option<u32>,
) -> Result<ContextEnforcementResult, String> {
    let min_useful = min_useful_tokens.unwrap_or(DEFAULT_MIN_GENERATION_TOKENS);

    let Some(context_limit) = effective_context_limit(request_settings, model, credential) else {
        // No limit known — skip enforcement (behave as before)
        let input_tokens = estimate_tokens_for_messages(messages);
        let _requested = request_settings
            .max_tokens
            .saturating_add(request_settings.reasoning_budget.unwrap_or(0));
        return Ok(ContextEnforcementResult {
            messages: messages.to_vec(),
            input_tokens,
            original_input_tokens: input_tokens,
            context_limit: 0,
            requested_output_tokens: request_settings.max_tokens,
            final_output_tokens: request_settings.max_tokens,
            removed_count: 0,
            reasoning_budget: request_settings.reasoning_budget,
        });
    };

    if context_limit == 0 {
        let input_tokens = estimate_tokens_for_messages(messages);
        return Ok(ContextEnforcementResult {
            messages: messages.to_vec(),
            input_tokens,
            original_input_tokens: input_tokens,
            context_limit,
            requested_output_tokens: request_settings.max_tokens,
            final_output_tokens: request_settings.max_tokens,
            removed_count: 0,
            reasoning_budget: request_settings.reasoning_budget,
        });
    }

    let margin = context_safety_margin(context_limit);
    let original_input_tokens = estimate_tokens_for_messages(messages);
    let requested_total = request_settings
        .max_tokens
        .saturating_add(request_settings.reasoning_budget.unwrap_or(0));
    // Quick check: if already fits with requested + margin, no trimming needed.
    if original_input_tokens
        .saturating_add(requested_total)
        .saturating_add(margin)
        <= context_limit
    {
        return Ok(ContextEnforcementResult {
            messages: messages.to_vec(),
            input_tokens: original_input_tokens,
            original_input_tokens,
            context_limit,
            requested_output_tokens: request_settings.max_tokens,
            final_output_tokens: request_settings.max_tokens,
            removed_count: 0,
            reasoning_budget: request_settings.reasoning_budget,
        });
    }

    // Need to trim. We will iteratively remove removable messages.
    let mut trimmed = messages.to_vec();
    let mut removed: usize = 0;

    // Helper to find oldest removable index (user/assistant, not last, not system/developer)
    fn find_removable_index(msgs: &[Value]) -> Option<usize> {
        if msgs.len() <= 1 {
            return None;
        }
        let last_idx = msgs.len() - 1;
        for (idx, msg) in msgs.iter().enumerate() {
            if idx == last_idx {
                continue; // preserve current user message
            }
            let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");
            if role == "system" || role == "developer" {
                continue;
            }
            if role == "user" || role == "assistant" {
                // Consider removable
                return Some(idx);
            }
            // For other roles (tool etc.) consider removable as well? But conservative to skip.
            // We'll treat unknown non-system as removable if not last.
            // To be safe, only user/assistant are removable per spec.
        }
        None
    }

    loop {
        let input_tokens = estimate_tokens_for_messages(&trimmed);
        let total_requested = request_settings
            .max_tokens
            .saturating_add(request_settings.reasoning_budget.unwrap_or(0));

        // If fits with full requested + margin, we can stop trimming.
        if input_tokens
            .saturating_add(total_requested)
            .saturating_add(margin)
            <= context_limit
        {
            // Reserve margin + reasoning from available budget
            let available = context_limit
                .saturating_sub(input_tokens)
                .saturating_sub(margin)
                .saturating_sub(request_settings.reasoning_budget.unwrap_or(0));
            let final_output =
                clamp_output_budget(request_settings.max_tokens, available, min_useful);
            // reasoning already subtracted from available, so final_output is max budget
            return Ok(ContextEnforcementResult {
                messages: trimmed.clone(),
                input_tokens,
                original_input_tokens,
                context_limit,
                requested_output_tokens: request_settings.max_tokens,
                final_output_tokens: final_output,
                removed_count: removed,
                reasoning_budget: request_settings.reasoning_budget,
            });
        }

        // Not fitting. Try to trim more if possible.
        if let Some(idx) = find_removable_index(&trimmed) {
            trimmed.remove(idx);
            removed += 1;
            // Recalculate on next loop
            continue;
        } else {
            // No more removable history to trim.
            let input_tokens = estimate_tokens_for_messages(&trimmed);
            if input_tokens.saturating_add(margin) >= context_limit {
                return Err(format!(
                    "Prompt configuration alone requires ~{} tokens, which exceeds the model's context limit of {} tokens. The character definition, system prompt, lorebook entries, and current message together are too large even after removing all conversation history. Reduce the character definition, remove some lorebook entries, shorten the current message, or increase the model's context length setting. (model: {}, limit: {})",
                    input_tokens, context_limit, model.name, context_limit
                ));
            }

            let available = context_limit
                .saturating_sub(input_tokens)
                .saturating_sub(margin);
            // If available is too small to be useful, still report but clamp.
            if available < ABSOLUTE_MIN_GENERATION_TOKENS {
                return Err(format!(
                    "Prompt requires ~{} tokens, leaving only {} tokens for generation (limit {}). Even after removing all removable history, there is insufficient space for a meaningful response. Shorten the prompt or increase context length. (model: {})",
                    input_tokens, available, context_limit, model.name
                ));
            }

            // Clamp output to available (reasoning already subtracted above via margin calc, but need to handle reasoning separately if present)
            let final_output = if let Some(reasoning) = request_settings.reasoning_budget {
                if available <= reasoning {
                    return Err(format!(
                        "Prompt requires ~{} tokens, leaving {} tokens, but reasoning budget alone requires {} tokens (limit {}). Reduce prompt size or lower reasoning budget. (model: {})",
                        input_tokens, available, reasoning, context_limit, model.name
                    ));
                }
                let avail_for_max = available.saturating_sub(reasoning);
                clamp_output_budget(request_settings.max_tokens, avail_for_max, min_useful)
            } else {
                clamp_output_budget(request_settings.max_tokens, available, min_useful)
            };

            return Ok(ContextEnforcementResult {
                messages: trimmed.clone(),
                input_tokens,
                original_input_tokens,
                context_limit,
                requested_output_tokens: request_settings.max_tokens,
                final_output_tokens: final_output,
                removed_count: removed,
                reasoning_budget: request_settings.reasoning_budget,
            });
        }
    }
}

// ---------------------------------------------------------------------------
// Stored-message-aware trimming for flows (preserves pinned)
// ---------------------------------------------------------------------------

pub fn enforce_context_window_for_chat(
    prompt_entries: &[crate::chat_manager::types::SystemPromptEntry],
    chat_messages: &[Value],
    system_role: &str,
    request_settings: &RequestSettings,
    model: &Model,
    credential: &ProviderCredential,
    pinned_count: usize,
    min_useful_tokens: Option<u32>,
) -> Result<(Vec<Value>, ContextEnforcementResult), String> {
    let min_useful = min_useful_tokens.unwrap_or(DEFAULT_MIN_GENERATION_TOKENS);
    let Some(context_limit) = effective_context_limit(request_settings, model, credential) else {
        // No limit, return as-is
        let assembled = crate::chat_manager::prompting::turn_builder::assemble_prompt_messages(
            prompt_entries.to_vec(),
            chat_messages.to_vec(),
            system_role,
        );
        let input_tokens = estimate_tokens_for_messages(&assembled);
        let result = ContextEnforcementResult {
            messages: assembled.clone(),
            input_tokens,
            original_input_tokens: input_tokens,
            context_limit: 0,
            requested_output_tokens: request_settings.max_tokens,
            final_output_tokens: request_settings.max_tokens,
            removed_count: 0,
            reasoning_budget: request_settings.reasoning_budget,
        };
        return Ok((chat_messages.to_vec(), result));
    };

    // We need to iteratively trim chat_messages (specifically recent part)
    let margin = context_safety_margin(context_limit);
    let mut trimmed_chat = chat_messages.to_vec();
    let _original_chat_len = trimmed_chat.len();
    let original_assembled = crate::chat_manager::prompting::turn_builder::assemble_prompt_messages(
        prompt_entries.to_vec(),
        trimmed_chat.clone(),
        system_role,
    );
    let original_input_tokens = estimate_tokens_for_messages(&original_assembled);
    let total_requested = request_settings
        .max_tokens
        .saturating_add(request_settings.reasoning_budget.unwrap_or(0));

    // Quick fit check — include safety margin
    if original_input_tokens
        .saturating_add(total_requested)
        .saturating_add(margin)
        <= context_limit
    {
        let result = ContextEnforcementResult {
            messages: original_assembled,
            input_tokens: original_input_tokens,
            original_input_tokens,
            context_limit,
            requested_output_tokens: request_settings.max_tokens,
            final_output_tokens: request_settings.max_tokens,
            removed_count: 0,
            reasoning_budget: request_settings.reasoning_budget,
        };
        return Ok((trimmed_chat, result));
    }

    // Need to trim. Removable indices are within chat_messages, excluding pinned prefix and last element.
    let mut removed: usize = 0;
    loop {
        let assembled = crate::chat_manager::prompting::turn_builder::assemble_prompt_messages(
            prompt_entries.to_vec(),
            trimmed_chat.clone(),
            system_role,
        );
        let input_tokens = estimate_tokens_for_messages(&assembled);
        if input_tokens
            .saturating_add(total_requested)
            .saturating_add(margin)
            <= context_limit
        {
            let available = context_limit
                .saturating_sub(input_tokens)
                .saturating_sub(margin)
                .saturating_sub(request_settings.reasoning_budget.unwrap_or(0));
            let final_output =
                clamp_output_budget(request_settings.max_tokens, available, min_useful);
            let result = ContextEnforcementResult {
                messages: assembled.clone(),
                input_tokens,
                original_input_tokens,
                context_limit,
                requested_output_tokens: request_settings.max_tokens,
                final_output_tokens: final_output,
                removed_count: removed,
                reasoning_budget: request_settings.reasoning_budget,
            };
            return Ok((trimmed_chat, result));
        }

        // Find oldest removable index in trimmed_chat
        // Removable range: [pinned_count, trimmed_chat.len() -1)  (exclude last)
        if trimmed_chat.len() <= pinned_count + 1 {
            // No removable left (only pinned + current)
            if input_tokens.saturating_add(margin) >= context_limit {
                return Err(format!(
                    "Prompt configuration alone requires ~{} tokens, which exceeds the model's context limit of {} tokens. The character definition, system prompt, lorebook entries, and current message together are too large even after removing all conversation history. Reduce the character definition, remove some lorebook entries, shorten the current message, or increase the model's context length setting. (model: {}, limit: {})",
                    input_tokens, context_limit, model.name, context_limit
                ));
            }
            let available = context_limit
                .saturating_sub(input_tokens)
                .saturating_sub(margin);
            if available < ABSOLUTE_MIN_GENERATION_TOKENS {
                return Err(format!(
                    "Prompt requires ~{} tokens, leaving only {} tokens for generation (limit {}). Even after removing all removable history, there is insufficient space for a meaningful response. Shorten the prompt or increase context length. (model: {})",
                    input_tokens, available, context_limit, model.name
                ));
            }
            let final_output = if let Some(reasoning) = request_settings.reasoning_budget {
                if available <= reasoning {
                    return Err(format!(
                        "Prompt requires ~{} tokens, leaving {} tokens, but reasoning budget alone requires {} tokens (limit {}). Reduce prompt size or lower reasoning budget. (model: {})",
                        input_tokens, available, reasoning, context_limit, model.name
                    ));
                }
                let avail_for_max = available.saturating_sub(reasoning);
                clamp_output_budget(request_settings.max_tokens, avail_for_max, min_useful)
            } else {
                clamp_output_budget(request_settings.max_tokens, available, min_useful)
            };
            let result = ContextEnforcementResult {
                messages: assembled.clone(),
                input_tokens,
                original_input_tokens,
                context_limit,
                requested_output_tokens: request_settings.max_tokens,
                final_output_tokens: final_output,
                removed_count: removed,
                reasoning_budget: request_settings.reasoning_budget,
            };
            return Ok((trimmed_chat, result));
        }

        // Remove oldest removable: index = pinned_count
        // This corresponds to oldest recent message.
        trimmed_chat.remove(pinned_count);
        removed += 1;
        // Continue loop to recalc
    }
}

pub fn debug_info_for_enforcement(
    model: &Model,
    credential: &ProviderCredential,
    result: &ContextEnforcementResult,
) -> DebugContextInfo {
    let available = if result.context_limit == 0 {
        0
    } else {
        result.context_limit.saturating_sub(result.input_tokens) as i64
    };
    DebugContextInfo {
        model_name: model.name.clone(),
        model_id: model.id.clone(),
        provider_id: credential.provider_id.clone(),
        context_limit: result.context_limit,
        requested_max_tokens: result.requested_output_tokens,
        reasoning_budget: result.reasoning_budget,
        original_input_tokens: result.original_input_tokens,
        final_input_tokens: result.input_tokens,
        available_output: available,
        final_output_tokens: result.final_output_tokens,
        removed_count: result.removed_count,
        final_total_estimate: result
            .input_tokens
            .saturating_add(result.final_output_tokens)
            .saturating_add(result.reasoning_budget.unwrap_or(0)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat_manager::execution::RequestSettings;
    use crate::chat_manager::types::{
        AdvancedModelSettings, Model, ProviderCredential, Session, Settings,
    };
    use serde_json::json;

    fn test_model(id: &str, context_length: Option<u32>, max_output: Option<u32>) -> Model {
        Model {
            id: id.to_string(),
            name: id.to_string(),
            provider_id: "openai".to_string(),
            provider_credential_id: None,
            provider_label: "openai".to_string(),
            display_name: id.to_string(),
            created_at: 0,
            input_scopes: vec!["text".into()],
            output_scopes: vec!["text".into()],
            advanced_model_settings: Some(AdvancedModelSettings {
                context_length,
                max_output_tokens: max_output,
                ..Default::default()
            }),
            prompt_template_id: None,
            voice_config: None,
            system_prompt: None,
        }
    }

    fn cred() -> ProviderCredential {
        ProviderCredential {
            id: "c1".into(),
            provider_id: "openai".into(),
            label: "openai".into(),
            api_key: Some("k".into()),
            base_url: None,
            default_model: None,
            headers: None,
            config: None,
        }
    }

    fn settings_with_model(model: Model) -> Settings {
        let mut s = crate::chat_manager::persistence::storage::default_settings();
        s.models = vec![model];
        s
    }

    fn session_with_id() -> Session {
        serde_json::from_value(json!({
            "id": "s1",
            "characterId": "c1",
            "title": "t",
            "createdAt": 0,
            "updatedAt": 0
        }))
        .unwrap()
    }

    fn msg(role: &str, content: &str) -> Value {
        json!({"role": role, "content": content})
    }

    fn estimate(text: &str) -> u32 {
        estimate_tokens_for_text(text)
    }

    #[test]
    fn estimate_tokens_nonzero_for_content() {
        let msgs = vec![
            msg("system", "You are a helpful assistant."),
            msg("user", "Hello world"),
        ];
        let tokens = estimate_tokens_for_messages(&msgs);
        assert!(tokens > 0);
        assert!(tokens < 1000);
    }

    #[test]
    fn comfortably_below_limit_nothing_removed() {
        let model = test_model("gpt-test", Some(4096), Some(512));
        let credential = cred();
        let session = session_with_id();
        let settings = settings_with_model(model.clone());
        let rs = RequestSettings::resolve(&session, &model, &settings);
        // 2 small messages
        let messages = vec![
            msg("system", "system prompt"),
            msg("user", "hi"),
            msg("assistant", "hello"),
            msg("user", "how are you?"),
        ];
        let result = enforce_context_window(&messages, &rs, &model, &credential, None).unwrap();
        assert_eq!(result.removed_count, 0);
        assert_eq!(result.final_output_tokens, rs.max_tokens);
        assert_eq!(result.messages.len(), messages.len());
    }

    #[test]
    fn slightly_above_limit_removes_oldest() {
        let model = test_model("gpt-test", Some(50), Some(20));
        let credential = cred();
        let session = session_with_id();
        let settings = settings_with_model(model.clone());
        let rs = RequestSettings::resolve(&session, &model, &settings);
        // Build messages that will exceed 50 tokens when including requested 20 = 70 limit
        // Each message about 10 tokens, 6 messages ~60 plus overhead -> will exceed
        let messages = vec![
            msg("system", "system prompt with some context about character"),
            msg("user", "oldest message that should be removed"),
            msg("assistant", "old response"),
            msg("user", "middle message"),
            msg("assistant", "middle response"),
            msg("user", "current user message that must be preserved"),
        ];
        let result = enforce_context_window(&messages, &rs, &model, &credential, None).unwrap();
        assert!(result.removed_count >= 1);
        // Current user message should still be present as last
        let last = result.messages.last().unwrap();
        assert_eq!(
            last.get("content").unwrap().as_str().unwrap(),
            "current user message that must be preserved"
        );
        // System prompt preserved
        let first = result.messages.first().unwrap();
        assert_eq!(first.get("role").unwrap().as_str().unwrap(), "system");
    }

    #[test]
    fn very_long_conversation_removes_enough() {
        let model = test_model("gpt-test", Some(80), Some(20));
        let credential = cred();
        let session = session_with_id();
        let settings = settings_with_model(model.clone());
        let rs = RequestSettings::resolve(&session, &model, &settings);
        let mut messages = vec![msg("system", "system prompt")];
        for i in 0..20 {
            messages.push(msg("user", &format!("user message number {}", i)));
            messages.push(msg("assistant", &format!("assistant reply number {}", i)));
        }
        messages.push(msg("user", "final current message"));
        let result = enforce_context_window(&messages, &rs, &model, &credential, None).unwrap();
        let input = result.input_tokens;
        assert!(input + result.final_output_tokens <= 80);
        // Should have removed many
        assert!(result.removed_count > 10);
        // Current preserved
        assert_eq!(
            result
                .messages
                .last()
                .unwrap()
                .get("content")
                .unwrap()
                .as_str()
                .unwrap(),
            "final current message"
        );
    }

    #[test]
    fn current_user_message_never_truncated() {
        let model = test_model("gpt-test", Some(40), Some(10));
        let credential = cred();
        let session = session_with_id();
        let settings = settings_with_model(model.clone());
        let rs = RequestSettings::resolve(&session, &model, &settings);
        let long_current = "a".repeat(200);
        let messages = vec![
            msg("system", "system prompt"),
            msg("user", "old history 1"),
            msg("assistant", "old response 1"),
            msg("user", &long_current),
        ];
        let result = enforce_context_window(&messages, &rs, &model, &credential, None).unwrap();
        // last message content should be exactly long_current, not truncated
        let last_content = result
            .messages
            .last()
            .unwrap()
            .get("content")
            .unwrap()
            .as_str()
            .unwrap();
        assert_eq!(last_content, long_current);
    }

    #[test]
    fn system_prompt_preserved() {
        let model = test_model("gpt-test", Some(60), Some(10));
        let credential = cred();
        let session = session_with_id();
        let settings = settings_with_model(model.clone());
        let rs = RequestSettings::resolve(&session, &model, &settings);
        let system_content = "You are Sarah, a character with detailed definition that must stay";
        let messages = vec![
            msg("system", system_content),
            msg("user", "history 1"),
            msg("assistant", "reply 1"),
            msg("user", "history 2"),
            msg("assistant", "reply 2"),
            msg("user", "current"),
        ];
        let result = enforce_context_window(&messages, &rs, &model, &credential, None).unwrap();
        let preserved = result
            .messages
            .iter()
            .find(|m| m.get("role").unwrap() == "system")
            .unwrap();
        assert_eq!(
            preserved.get("content").unwrap().as_str().unwrap(),
            system_content
        );
    }

    #[test]
    fn requested_output_reduced_when_necessary() {
        let model = test_model("gpt-test", Some(100), Some(80));
        let credential = cred();
        let session = session_with_id();
        let settings = settings_with_model(model.clone());
        let rs = RequestSettings::resolve(&session, &model, &settings);
        // Prompt that takes ~80 tokens, requested 80, limit 100 => we need to clamp to 20
        let large_prompt = "word ".repeat(50); // ~50 tokens
        let messages = vec![
            msg("system", &large_prompt),
            msg("user", "current small message"),
        ];
        let result = enforce_context_window(&messages, &rs, &model, &credential, None).unwrap();
        // input ~50+ overhead ~ 60, available = 40, requested 80 => clamped to 40
        assert!(result.final_output_tokens < rs.max_tokens);
        assert!(result.input_tokens + result.final_output_tokens <= 100);
    }

    #[test]
    fn prompt_too_large_error() {
        let model = test_model("gpt-test", Some(30), Some(10));
        let credential = cred();
        let session = session_with_id();
        let settings = settings_with_model(model.clone());
        let rs = RequestSettings::resolve(&session, &model, &settings);
        let huge_system = "word ".repeat(100); // ~100 tokens > limit 30
        let messages = vec![msg("system", &huge_system), msg("user", "current")];
        let err = enforce_context_window(&messages, &rs, &model, &credential, None).unwrap_err();
        assert!(err.to_lowercase().contains("exceeds") || err.to_lowercase().contains("too large"));
    }

    #[test]
    fn different_model_context_lengths_respected() {
        let model_small = test_model("small", Some(50), Some(10));
        let model_large = test_model("large", Some(500), Some(10));
        let credential = cred();
        let session = session_with_id();

        let messages = vec![
            msg("system", "system"),
            msg("user", "history 1"),
            msg("assistant", "reply 1"),
            msg("user", "history 2"),
            msg("assistant", "reply 2"),
            msg("user", "current"),
        ];

        let settings_small = settings_with_model(model_small.clone());
        let rs_small = RequestSettings::resolve(&session, &model_small, &settings_small);
        let res_small =
            enforce_context_window(&messages, &rs_small, &model_small, &credential, None).unwrap();

        let settings_large = settings_with_model(model_large.clone());
        let rs_large = RequestSettings::resolve(&session, &model_large, &settings_large);
        let res_large =
            enforce_context_window(&messages, &rs_large, &model_large, &credential, None).unwrap();

        // Large model should have removed <= small model
        assert!(res_large.removed_count <= res_small.removed_count);
        // With large limit, likely nothing removed
        // But at least check both fit
        assert!(res_small.input_tokens + res_small.final_output_tokens <= 50);
        assert!(res_large.input_tokens + res_large.final_output_tokens <= 500);
    }

    #[test]
    fn respects_reasoning_budget() {
        let mut model = test_model("reasoning-model", Some(100), Some(40));
        // Enable reasoning
        if let Some(adv) = model.advanced_model_settings.as_mut() {
            adv.reasoning_enabled = Some(true);
            adv.reasoning_budget_tokens = Some(30);
        }
        let credential = cred();
        let session = session_with_id();
        let settings = settings_with_model(model.clone());
        let rs = RequestSettings::resolve(&session, &model, &settings);
        let messages = vec![
            msg("system", &("prompt ".repeat(20))),
            msg("user", "current"),
        ];
        let result = enforce_context_window(&messages, &rs, &model, &credential, None).unwrap();
        // total should be input + max + reasoning <= limit
        let total =
            result.input_tokens + result.final_output_tokens + result.reasoning_budget.unwrap_or(0);
        assert!(total <= 100);
    }

    #[test]
    fn stored_conversation_unchanged_after_trimming() {
        let model = test_model("gpt-test", Some(60), Some(20));
        let credential = cred();
        let session = session_with_id();
        let settings = settings_with_model(model.clone());
        let rs = RequestSettings::resolve(&session, &model, &settings);
        let messages = vec![
            msg("system", "system prompt"),
            msg("user", "history 1"),
            msg("assistant", "reply 1"),
            msg("user", "history 2"),
            msg("assistant", "reply 2"),
            msg("user", "history 3"),
            msg("assistant", "reply 3"),
            msg("user", "current"),
        ];
        let original_len = messages.len();
        let original_clone = messages.clone();
        let result = enforce_context_window(&messages, &rs, &model, &credential, None).unwrap();
        // Original must be unchanged
        assert_eq!(messages.len(), original_len);
        assert_eq!(messages, original_clone);
        // Result may be smaller
        assert!(result.messages.len() <= original_len);
        // At least current and system preserved
        assert_eq!(
            result.messages.last().unwrap().get("content").unwrap(),
            original_clone.last().unwrap().get("content").unwrap()
        );
    }

    #[test]
    fn existing_providers_continue_working() {
        // Test that different provider ids still produce a valid enforcement result
        // without panicking and with provider-agnostic trimming.
        let providers = vec![
            "openai",
            "anthropic",
            "openrouter",
            "google",
            "mistral",
            "groq",
            "ollama",
            "llamacpp",
        ];
        let mut messages = vec![msg("system", "system prompt")];
        for i in 0..5 {
            messages.push(msg("user", &format!("user msg {}", i)));
            messages.push(msg("assistant", &format!("assistant reply {}", i)));
        }
        messages.push(msg("user", "current"));

        for provider in providers {
            let mut model = test_model("provider-test", Some(80), Some(20));
            model.provider_id = provider.to_string();
            let mut credential = cred();
            credential.provider_id = provider.to_string();
            let session = session_with_id();
            let settings = settings_with_model(model.clone());
            let rs = RequestSettings::resolve(&session, &model, &settings);
            let result = enforce_context_window(&messages, &rs, &model, &credential, None);
            assert!(
                result.is_ok(),
                "provider {} should not fail: {:?}",
                provider,
                result.err()
            );
            let res = result.unwrap();
            assert!(
                res.input_tokens + res.final_output_tokens <= 80,
                "provider {} exceeded limit",
                provider
            );
        }
    }

    #[test]
    fn enforce_for_chat_preserves_pinned_and_current() {
        use crate::chat_manager::types::PromptEntryPosition;
        use crate::chat_manager::types::PromptEntryRole;
        use crate::chat_manager::types::SystemPromptEntry;

        let model = test_model("gpt-test", Some(80), Some(20));
        let credential = cred();
        let session = session_with_id();
        let settings = settings_with_model(model.clone());
        let rs = RequestSettings::resolve(&session, &model, &settings);

        let system_entry = SystemPromptEntry {
            id: "sys".into(),
            name: "system".into(),
            role: PromptEntryRole::System,
            content: "You are a helpful character".into(),
            enabled: true,
            injection_position: PromptEntryPosition::Relative,
            injection_depth: 0,
            conditional_min_messages: None,
            interval_turns: None,
            system_prompt: true,
            conditions: None,
            prompt_entry_payload: None,
        };
        let prompt_entries = vec![system_entry];

        // Build chat_messages with pinned + recent
        let pinned_chat = vec![
            msg("user", "pinned important history"),
            msg("assistant", "pinned reply"),
        ];
        let recent_chat = vec![
            msg("user", "old history 1"),
            msg("assistant", "old reply 1"),
            msg("user", "old history 2"),
            msg("assistant", "old reply 2"),
            msg("user", "current final"),
        ];
        let mut chat_messages = Vec::new();
        chat_messages.extend(pinned_chat.clone());
        chat_messages.extend(recent_chat.clone());
        let pinned_count = pinned_chat.len();

        let (trimmed_chat, ctx_res) = enforce_context_window_for_chat(
            &prompt_entries,
            &chat_messages,
            "system",
            &rs,
            &model,
            &credential,
            pinned_count,
            None,
        )
        .unwrap();

        // Pinned should be preserved at start
        assert!(trimmed_chat.len() >= pinned_count + 1); // at least pinned + current
        for i in 0..pinned_count {
            assert_eq!(
                trimmed_chat[i], pinned_chat[i],
                "pinned message {} should be preserved",
                i
            );
        }
        // Current preserved as last
        assert_eq!(trimmed_chat.last().unwrap(), recent_chat.last().unwrap());
        // Some old history may have been removed
        assert!(ctx_res.removed_count <= recent_chat.len() - 1);
    }

    #[test]
    fn enforce_for_chat_output_clamped_and_tiny_prompt_error() {
        use crate::chat_manager::types::{PromptEntryPosition, PromptEntryRole, SystemPromptEntry};
        let model = test_model("tiny", Some(25), Some(20));
        let credential = cred();
        let session = session_with_id();
        let settings = settings_with_model(model.clone());
        let rs = RequestSettings::resolve(&session, &model, &settings);
        let huge_system = SystemPromptEntry {
            id: "huge".into(),
            name: "huge".into(),
            role: PromptEntryRole::System,
            content: "word ".repeat(30),
            enabled: true,
            injection_position: PromptEntryPosition::Relative,
            injection_depth: 0,
            conditional_min_messages: None,
            interval_turns: None,
            system_prompt: true,
            conditions: None,
            prompt_entry_payload: None,
        };
        let chat = vec![msg("user", "current")];
        let err = enforce_context_window_for_chat(
            &[huge_system],
            &chat,
            "system",
            &rs,
            &model,
            &credential,
            0,
            None,
        )
        .unwrap_err();
        assert!(err.contains("exceeds") || err.contains("too large"));
    }

    // --- New fallback / propagation tests (required) ---

    #[test]
    fn missing_context_length_uses_fallback_8192() {
        let model = test_model("no-limit", None, Some(4096));
        let credential = cred();
        let session = session_with_id();
        let settings = settings_with_model(model.clone());
        let rs = RequestSettings::resolve(&session, &model, &settings);
        // resolve should now fallback to 8192
        assert_eq!(rs.context_length, Some(8192));
        // Build input that exceeds 8192 with requested 4096
        let large = "word ".repeat(2500); // ~7000 tokens with conservative estimator
        let messages = vec![msg("system", &large), msg("user", "current")];
        let res = enforce_context_window(&messages, &rs, &model, &credential, None).unwrap();
        assert!(res.input_tokens + res.final_output_tokens <= 8192);
        assert!(res.final_output_tokens < 4096 || res.removed_count > 0);
    }

    #[test]
    fn zero_context_length_uses_fallback() {
        let model = test_model("zero-limit", Some(0), Some(512));
        let credential = cred();
        let session = session_with_id();
        let settings = settings_with_model(model.clone());
        let rs = RequestSettings::resolve(&session, &model, &settings);
        assert_eq!(rs.context_length, Some(8192));
        let large = "word ".repeat(2000);
        let messages = vec![msg("system", &large), msg("user", "hi")];
        let res = enforce_context_window(&messages, &rs, &model, &credential, None).unwrap();
        assert_eq!(res.context_limit, 8192);
    }

    #[test]
    fn explicit_large_context_respected() {
        let model = test_model("large", Some(32768), Some(4096));
        let credential = cred();
        let session = session_with_id();
        let settings = settings_with_model(model.clone());
        let rs = RequestSettings::resolve(&session, &model, &settings);
        assert_eq!(rs.context_length, Some(32768));
        let messages = vec![msg("system", "small prompt"), msg("user", "hi")];
        let res = enforce_context_window(&messages, &rs, &model, &credential, None).unwrap();
        assert_eq!(res.context_limit, 32768);
        assert_eq!(res.removed_count, 0);
    }

    #[test]
    fn first_enforcement_reduced_preserved_in_second() {
        let model = test_model("gpt-test", Some(8192), Some(4096));
        let credential = cred();
        let session = session_with_id();
        let settings = settings_with_model(model.clone());
        let rs = RequestSettings::resolve(&session, &model, &settings);
        let large_prompt = "word ".repeat(2000); // ~5700 tokens + overhead
        let messages = vec![msg("system", &large_prompt), msg("user", "current")];
        let first = enforce_context_window(&messages, &rs, &model, &credential, None).unwrap();
        assert!(first.final_output_tokens < 4096);
        // Simulate executor receiving reduced budget
        let mut rs2 = rs.clone();
        rs2.max_tokens = first.final_output_tokens;
        let second =
            enforce_context_window(&first.messages, &rs2, &model, &credential, None).unwrap();
        assert!(second.input_tokens + second.final_output_tokens <= 8192);
        // If we had restored original, it would exceed
        let would_exceed = first.input_tokens + 4096 > 8192;
        assert!(would_exceed);
    }

    #[test]
    fn regenerate_path_preserves_clamped_budget() {
        use crate::chat_manager::types::{PromptEntryPosition, PromptEntryRole, SystemPromptEntry};
        let model = test_model("regen", Some(8192), Some(4096));
        let credential = cred();
        let session = session_with_id();
        let settings = settings_with_model(model.clone());
        let rs = RequestSettings::resolve(&session, &model, &settings);
        let sys = SystemPromptEntry {
            id: "s".into(),
            name: "s".into(),
            role: PromptEntryRole::System,
            content: "word ".repeat(2000),
            enabled: true,
            injection_position: PromptEntryPosition::Relative,
            injection_depth: 0,
            conditional_min_messages: None,
            interval_turns: None,
            system_prompt: true,
            conditions: None,
            prompt_entry_payload: None,
        };
        let chat = vec![
            msg("user", "old history 1"),
            msg("assistant", "old reply 1"),
            msg("user", "current"),
        ];
        let (trimmed, res) = enforce_context_window_for_chat(
            &[sys],
            &chat,
            "system",
            &rs,
            &model,
            &credential,
            0,
            None,
        )
        .unwrap();
        let clamped = res.final_output_tokens;
        assert!(clamped < 4096);
        // Second stage with clamped should still fit
        let assembled = crate::chat_manager::prompting::turn_builder::assemble_prompt_messages(
            vec![],
            trimmed,
            "system",
        );
        let second = enforce_context_window(&assembled, &rs, &model, &credential, None).unwrap();
        // Simulate using clamped budget — second pass with clamped would fit, with original would not
        let mut rs_clamped = rs.clone();
        rs_clamped.max_tokens = clamped;
        let with_clamped =
            enforce_context_window(&assembled, &rs_clamped, &model, &credential, None).unwrap();
        assert!(with_clamped.input_tokens + with_clamped.final_output_tokens <= 8192);
    }

    #[test]
    fn continuation_path_same_as_regenerate() {
        // Continuation uses same enforce_for_chat path; verify fallback and clamping apply
        let model = test_model("cont", None, Some(4096));
        let credential = cred();
        let session = session_with_id();
        let settings = settings_with_model(model.clone());
        let rs = RequestSettings::resolve(&session, &model, &settings);
        assert_eq!(rs.context_length, Some(8192));
        let large = "word ".repeat(2500);
        let msgs = vec![msg("system", &large), msg("user", "current")];
        let res = enforce_context_window(&msgs, &rs, &model, &credential, None).unwrap();
        assert!(res.final_output_tokens < 4096);
    }

    #[test]
    fn prompt_alone_exceeds_returns_friendly_error() {
        let model = test_model("tiny", Some(1000), Some(512));
        let credential = cred();
        let session = session_with_id();
        let settings = settings_with_model(model.clone());
        let rs = RequestSettings::resolve(&session, &model, &settings);
        let huge = "word ".repeat(5000);
        let msgs = vec![msg("system", &huge), msg("user", "hi")];
        let err = enforce_context_window(&msgs, &rs, &model, &credential, None).unwrap_err();
        assert!(err.contains("exceeds") || err.contains("Prompt configuration"));
    }
}
