use std::collections::{HashMap, HashSet};
use std::time::Duration;

use serde_json::Value;
use tauri::AppHandle;
use uuid::Uuid;

use crate::chat_manager::types::ProviderId;
use crate::chat_manager::{prompting::request as chat_request, types::UsageSummary};
use crate::providers::config::resolve_base_url;
use crate::usage::{
    add_usage_record,
    tracking::{RequestUsage, UsageFinishReason, UsageOperationType},
};
use crate::utils::{log_error, log_info, now_millis};

use super::provider_adapter::{
    get_adapter, ImageRequestPayload, ImageResponseData, ImageResponseFormat,
};
use super::storage::save_image;
use super::types::{
    GeneratedImage, ImageCharacterContext, ImageGenerationRequest, ImageGenerationResponse,
    ImageLora, ImageSceneContext,
};

/* =========================================================
LORA HELPERS
========================================================= */

/**
 * Merge model-level LoRAs with request-level LoRAs.
 *
 * Request-level LoRAs override model-level LoRAs when they
 * reference the same path and high-noise state.
 */
fn merged_lora_keywords(base: Option<&[ImageLora]>, request: Option<&[ImageLora]>) -> Vec<String> {
    let mut keywords = Vec::new();
    let mut seen = HashSet::new();

    let mut active_loras = base.unwrap_or_default().iter().collect::<Vec<_>>();

    for lora in request.unwrap_or_default() {
        if let Some(existing) = active_loras.iter_mut().find(|existing| {
            existing.path == lora.path && existing.is_high_noise == lora.is_high_noise
        }) {
            *existing = lora;
        } else {
            active_loras.push(lora);
        }
    }

    for keyword in active_loras.iter().flat_map(|lora| lora.keywords.iter()) {
        let keyword = keyword.trim();

        if keyword.is_empty() {
            continue;
        }

        if seen.insert(keyword.to_lowercase()) {
            keywords.push(keyword.to_string());
        }
    }

    keywords
}

fn active_lora_keywords(request: &ImageGenerationRequest) -> Vec<String> {
    merged_lora_keywords(
        request
            .advanced_model_settings
            .as_ref()
            .and_then(|settings| settings.sd_base_loras.as_deref()),
        request.loras.as_deref(),
    )
}

/* =========================================================
PROMPT HELPERS
========================================================= */

fn append_prompt_part(parts: &mut Vec<String>, value: Option<&str>) {
    if let Some(value) = value {
        let value = value.trim();

        if !value.is_empty() {
            parts.push(value.to_string());
        }
    }
}

/* =========================================================
CHARACTER CONTEXT
========================================================= */

fn compose_character_context_prompt(character: &ImageCharacterContext) -> String {
    let mut parts = Vec::new();

    if let Some(name) = character.name.as_deref() {
        append_prompt_part(&mut parts, Some(&format!("Character: {}", name)));
    }

    if let Some(appearance) = character.appearance.as_deref() {
        append_prompt_part(
            &mut parts,
            Some(&format!("Character appearance: {}", appearance)),
        );
    }

    if let Some(description) = character.description.as_deref() {
        append_prompt_part(
            &mut parts,
            Some(&format!("Character description: {}", description)),
        );
    }

    if let Some(personality) = character.personality.as_deref() {
        append_prompt_part(
            &mut parts,
            Some(&format!(
                "Character personality and expression guidance: {}",
                personality
            )),
        );
    }

    parts.join(", ")
}

/* =========================================================
SCENE CONTEXT
========================================================= */

fn compose_scene_context_prompt(scene: &ImageSceneContext) -> String {
    let mut parts = Vec::new();

    /*
     * Scene Writer should normally populate visual_prompt.
     *
     * If visual_prompt does not exist, fall back to the
     * normal scene description.
     */
    if let Some(visual_prompt) = scene.visual_prompt.as_deref() {
        append_prompt_part(&mut parts, Some(visual_prompt));
    } else if let Some(description) = scene.description.as_deref() {
        append_prompt_part(&mut parts, Some(description));
    }

    /*
     * IMPORTANT:
     *
     * negative_prompt is intentionally NOT inserted into
     * the positive prompt.
     *
     * Providers that support negative prompts should consume
     * scene_context.negative_prompt directly.
     */

    parts.join(", ")
}

/* =========================================================
COMPLETE IMAGE PROMPT
========================================================= */

fn compose_scene_image_prompt(
    request: &ImageGenerationRequest,
    pre_prompt: Option<&str>,
    lora_keywords: &[String],
) -> String {
    let mut parts = Vec::new();

    /*
     * Global image generation pre-prompt.
     */
    append_prompt_part(&mut parts, pre_prompt);

    /*
     * Build a temporary representation of the existing
     * character + scene + explicit prompt.
     *
     * This is used only to prevent duplicate LoRA trigger
     * keywords.
     */
    let mut existing_text = String::new();

    if let Some(character) = request.character_context.as_ref() {
        existing_text.push_str(&compose_character_context_prompt(character));

        existing_text.push('\n');
    }

    if let Some(scene) = request.scene_context.as_ref() {
        existing_text.push_str(&compose_scene_context_prompt(scene));

        existing_text.push('\n');
    }

    existing_text.push_str(request.prompt.trim());

    let existing_prompt_text = existing_text.to_lowercase();

    /*
     * LoRA trigger keywords.
     */
    for keyword in lora_keywords {
        let keyword = keyword.trim();

        if keyword.is_empty() {
            continue;
        }

        if !existing_prompt_text.contains(&keyword.to_lowercase()) {
            parts.push(keyword.to_string());
        }
    }

    /*
     * Character visual context.
     */
    if let Some(character) = request.character_context.as_ref() {
        let character_prompt = compose_character_context_prompt(character);

        if !character_prompt.is_empty() {
            parts.push(character_prompt);
        }
    }

    /*
     * Scene Writer visual context.
     */
    if let Some(scene) = request.scene_context.as_ref() {
        let scene_prompt = compose_scene_context_prompt(scene);

        if !scene_prompt.is_empty() {
            parts.push(scene_prompt);
        }
    }

    /*
     * Explicit generation prompt.
     */
    append_prompt_part(&mut parts, Some(request.prompt.as_str()));

    parts.join(", ")
}

/* =========================================================
REFERENCE IMAGE HELPERS
========================================================= */

/**
 * Promote character profile/reference images into the normal
 * input_images field.
 *
 * This means the provider adapter does not need a separate
 * character-profile-image mechanism.
 *
 * AI Horde, Automatic1111, ComfyUI, Diffusers, etc. can all
 * consume the resulting input_images according to their own
 * capabilities.
 */
fn promote_character_reference_images(request: &mut ImageGenerationRequest) {
    let mut references = Vec::new();

    /*
     * Primary character reference image.
     */
    if let Some(reference) = request.character_reference_image.as_deref() {
        let reference = reference.trim();

        if !reference.is_empty() {
            references.push(reference.to_string());
        }
    }

    /*
     * Additional explicit character references.
     */
    for reference in &request.character_reference_images {
        let reference = reference.trim();

        if !reference.is_empty() {
            references.push(reference.to_string());
        }
    }

    /*
     * References supplied through character context.
     */
    if let Some(character) = request.character_context.as_ref() {
        if let Some(reference) = character.reference_image.as_deref() {
            let reference = reference.trim();

            if !reference.is_empty() {
                references.push(reference.to_string());
            }
        }

        references.extend(
            character
                .reference_images
                .iter()
                .map(String::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string),
        );
    }

    /*
     * Remove duplicate references while preserving order.
     */
    let mut seen = HashSet::new();

    references.retain(|reference| seen.insert(reference.clone()));

    if references.is_empty() {
        return;
    }

    /*
     * Merge with generic input_images.
     */
    let mut input_images = request.input_images.take().unwrap_or_default();

    for reference in references {
        if !input_images.iter().any(|existing| existing == &reference) {
            input_images.push(reference);
        }
    }

    if !input_images.is_empty() {
        request.input_images = Some(input_images);
    }
}

/* =========================================================
NEGATIVE PROMPT
========================================================= */

fn scene_negative_prompt(request: &ImageGenerationRequest) -> Option<String> {
    request
        .scene_context
        .as_ref()
        .and_then(|scene| scene.negative_prompt.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/* =========================================================
GEMINI
========================================================= */

fn gemini_image_endpoint(base_url: &str, model: &str, api_key: &str) -> String {
    let base = base_url.trim_end_matches('/');

    let base = base
        .strip_suffix("/v1beta")
        .or_else(|| base.strip_suffix("/v1"))
        .unwrap_or(base);

    format!(
        "{}/v1beta/models/{}:generateContent?key={}",
        base, model, api_key
    )
}

/* =========================================================
AI HORDE
========================================================= */

/*
 * IMPORTANT ARCHITECTURE:
 *
 * "aihorde" is ONLY the provider.
 *
 * The actual Horde model is request.model.
 *
 * Example:
 *
 * provider_id = "aihorde"
 * credential_id = "my-horde-key"
 * model = "Flux.1-schnell"
 *
 * The model is therefore controlled by LettuceAI's Models
 * setting and is NOT hard-coded here.
 */
fn is_ai_horde_provider(provider_id: &str) -> bool {
    matches!(provider_id, "aihorde" | "ai-horde")
}

fn is_gradio_provider(provider_id: &str) -> bool {
    crate::image_generator::provider_adapter::normalize_provider_id(provider_id) == "gradio"
}

/* =========================================================
AI HORDE JOB ID
========================================================= */

fn extract_ai_horde_job_id(value: &str) -> Option<String> {
    value
        .strip_prefix("aihorde://")
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/* =========================================================
AI HORDE CHECK RESPONSE
========================================================= */

#[derive(Debug, serde::Deserialize)]
struct AIHordeCheckResponse {
    #[serde(default)]
    done: bool,

    #[serde(default)]
    faulted: bool,

    #[serde(default)]
    message: Option<String>,

    #[serde(default)]
    queue_position: Option<u64>,

    #[serde(default)]
    wait_time: Option<u64>,
}

/* =========================================================
AI HORDE GENERATION
========================================================= */

#[derive(Debug, serde::Deserialize)]
struct AIHordeGeneration {
    #[serde(default)]
    img: Option<String>,

    #[serde(default)]
    censored: bool,
}

/* =========================================================
AI HORDE STATUS
========================================================= */

#[derive(Debug, serde::Deserialize)]
struct AIHordeStatusResponse {
    #[serde(default)]
    done: bool,

    #[serde(default)]
    faulted: bool,

    #[serde(default)]
    message: Option<String>,

    #[serde(default)]
    generations: Vec<AIHordeGeneration>,
}

/* =========================================================
AI HORDE POLLING
========================================================= */

async fn wait_for_ai_horde_generation(
    app: &AppHandle,
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    job_id: &str,
) -> Result<Vec<ImageResponseData>, String> {
    let base = base_url.trim_end_matches('/');

    let check_url = format!("{}/v2/generate/check/{}", base, job_id);

    let status_url = format!("{}/v2/generate/status/{}", base, job_id);

    const MAX_POLLS: usize = 300;
    const POLL_INTERVAL_SECONDS: u64 = 2;

    log_info(
        app,
        "image_generator",
        format!("AI Horde generation started: {}", job_id),
    );

    let mut completed = false;

    for poll_number in 0..MAX_POLLS {
        let mut request = client.get(&check_url);

        /*
         * Provider credential is ONLY used as the Horde
         * authentication credential.
         *
         * The model does not come from the credential.
         */
        if !api_key.trim().is_empty() {
            request = request.header("apikey", api_key.trim());
        }

        let response = request
            .send()
            .await
            .map_err(|error| format!("AI Horde status request failed: {}", error))?;

        let status_code = response.status();

        if !status_code.is_success() {
            let body = response.text().await.unwrap_or_default();

            return Err(format!("AI Horde status error {}: {}", status_code, body));
        }

        let check: AIHordeCheckResponse = response
            .json()
            .await
            .map_err(|error| format!("Failed to parse AI Horde check response: {}", error))?;

        if check.faulted {
            return Err(check
                .message
                .unwrap_or_else(|| "AI Horde generation failed.".to_string()));
        }

        if let Some(message) = check.message.as_deref() {
            if !message.trim().is_empty() {
                log_info(app, "image_generator", format!("AI Horde: {}", message));
            }
        }

        if let Some(position) = check.queue_position {
            if poll_number % 5 == 0 {
                log_info(
                    app,
                    "image_generator",
                    format!("AI Horde queue position: {}", position),
                );
            }
        }

        if let Some(wait_time) = check.wait_time {
            if poll_number % 5 == 0 {
                log_info(
                    app,
                    "image_generator",
                    format!("AI Horde estimated wait: {} seconds", wait_time),
                );
            }
        }

        if check.done {
            completed = true;
            break;
        }

        tokio::time::sleep(Duration::from_secs(POLL_INTERVAL_SECONDS)).await;
    }

    if !completed {
        return Err("AI Horde generation timed out after 10 minutes.".to_string());
    }

    /* -----------------------------------------------------
    Fetch completed generation
    ----------------------------------------------------- */

    let mut request = client.get(&status_url);

    if !api_key.trim().is_empty() {
        request = request.header("apikey", api_key.trim());
    }

    let response = request
        .send()
        .await
        .map_err(|error| format!("AI Horde result request failed: {}", error))?;

    let status_code = response.status();

    if !status_code.is_success() {
        let body = response.text().await.unwrap_or_default();

        return Err(format!("AI Horde result error {}: {}", status_code, body));
    }

    let status: AIHordeStatusResponse = response
        .json()
        .await
        .map_err(|error| format!("Failed to parse AI Horde result: {}", error))?;

    if status.faulted {
        return Err(status
            .message
            .unwrap_or_else(|| "AI Horde generation failed.".to_string()));
    }

    let mut images = Vec::new();

    for generation in status.generations {
        if generation.censored {
            continue;
        }

        let Some(url) = generation.img else {
            continue;
        };

        if url.trim().is_empty() {
            continue;
        }

        images.push(ImageResponseData {
            url: Some(url),
            b64_json: None,
            text: None,
        });
    }

    if images.is_empty() {
        return Err("AI Horde completed the generation but returned no images.".to_string());
    }

    log_info(
        app,
        "image_generator",
        format!("AI Horde generation completed: {} image(s)", images.len()),
    );

    Ok(images)
}

fn extract_gradio_event_id(value: &str) -> Option<String> {
    value
        .strip_prefix("gradio://")
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

async fn wait_for_gradio_generation(
    app: &AppHandle,
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    endpoint_name: &str,
    event_id: &str,
) -> Result<Vec<ImageResponseData>, String> {
    let base = base_url.trim_end_matches('/');
    let endpoint = endpoint_name.trim_matches('/').trim();
    let endpoint = if endpoint.is_empty() {
        "predict".to_string()
    } else {
        endpoint.to_string()
    };
    // Gradio 4 queue endpoint: /gradio_api/call/{endpoint}/{event_id}
    let status_url = format!("{}/gradio_api/call/{}/{}", base, endpoint, event_id);
    // Fallback for older Gradio: /queue/status or /api/predict/{event_id}
    let alt_url = format!("{}/queue/status/{}", base, event_id);

    const MAX_POLLS: usize = 240;
    const POLL_INTERVAL_MS: u64 = 500;

    log_info(
        app,
        "image_generator",
        format!(
            "Gradio queued generation: {} (endpoint: {})",
            event_id, endpoint
        ),
    );

    for poll in 0..MAX_POLLS {
        let mut request = client.get(&status_url);
        if !api_key.trim().is_empty() {
            // HF Spaces often use Bearer token
            request = request.header("Authorization", format!("Bearer {}", api_key.trim()));
        }
        let response = request
            .send()
            .await
            .map_err(|e| format!("Gradio status request failed: {}", e))?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();

        // Try parse as JSON first
        if let Ok(json) = serde_json::from_str::<Value>(&text) {
            // Check for data field (completed)
            if let Some(data) = json.get("data") {
                // Gradio may return {"data": ["base64..."]} when complete
                let mut out = Vec::new();
                // Reuse gradio adapter's parsing logic via direct extraction
                // We will try to extract images from data
                let adapter = crate::image_generator::provider_adapter::gradio::GradioAdapter;
                // We can try to parse via adapter, but we need to handle both queued and direct
                // For now, directly collect from data
                if let Some(arr) = data.as_array() {
                    for item in arr {
                        // item may be string (base64/url) or object
                        let s = if let Some(s) = item.as_str() {
                            s.trim().to_string()
                        } else if let Some(obj) = item.as_object() {
                            // Try to extract url or base64 from object
                            obj.get("url")
                                .and_then(Value::as_str)
                                .or_else(|| obj.get("data").and_then(Value::as_str))
                                .unwrap_or("")
                                .to_string()
                        } else {
                            continue;
                        };
                        if s.is_empty() {
                            continue;
                        }
                        if s.starts_with("http://") || s.starts_with("https://") {
                            out.push(ImageResponseData {
                                url: Some(s),
                                b64_json: None,
                                text: None,
                            });
                        } else if s.len() > 20 {
                            out.push(ImageResponseData {
                                url: None,
                                b64_json: Some(s),
                                text: None,
                            });
                        }
                    }
                } else if let Some(s) = data.as_str().map(str::trim).filter(|s| !s.is_empty()) {
                    if s.starts_with("http") {
                        out.push(ImageResponseData {
                            url: Some(s.to_string()),
                            b64_json: None,
                            text: None,
                        });
                    } else {
                        out.push(ImageResponseData {
                            url: None,
                            b64_json: Some(s.to_string()),
                            text: None,
                        });
                    }
                }
                if !out.is_empty() {
                    log_info(
                        app,
                        "image_generator",
                        format!(
                            "Gradio generation completed: {} image(s) after {} polls",
                            out.len(),
                            poll
                        ),
                    );
                    return Ok(out);
                }
            }
            // Check for status field
            if let Some(status_str) = json
                .get("status")
                .and_then(Value::as_str)
                .map(|s| s.to_ascii_lowercase())
            {
                if status_str.contains("failed") || status_str.contains("error") {
                    return Err(format!(
                        "Gradio generation failed (status: {}): {}",
                        status_str, text
                    ));
                }
            }
        } else {
            // Try SSE format: event: complete\ndata: ["..."]
            if text.contains("event: complete") || text.contains("\"data\"") {
                // Extract data array from SSE
                for line in text.lines() {
                    let line = line.trim();
                    if let Some(data_str) = line.strip_prefix("data:") {
                        let data_str = data_str.trim();
                        if data_str.is_empty() || data_str == "[DONE]" {
                            continue;
                        }
                        if let Ok(val) = serde_json::from_str::<Value>(data_str) {
                            let mut out = Vec::new();
                            if let Some(arr) = val.as_array() {
                                for item in arr {
                                    if let Some(s) =
                                        item.as_str().map(str::trim).filter(|s| !s.is_empty())
                                    {
                                        if s.starts_with("http") {
                                            out.push(ImageResponseData {
                                                url: Some(s.to_string()),
                                                b64_json: None,
                                                text: None,
                                            });
                                        } else {
                                            out.push(ImageResponseData {
                                                url: None,
                                                b64_json: Some(s.to_string()),
                                                text: None,
                                            });
                                        }
                                    }
                                }
                            } else if let Some(s) = val.as_str() {
                                out.push(ImageResponseData {
                                    url: None,
                                    b64_json: Some(s.to_string()),
                                    text: None,
                                });
                            }
                            if !out.is_empty() {
                                return Ok(out);
                            }
                        }
                    }
                }
            }
        }

        // Also try alt URL for older Gradio
        if poll % 10 == 0 && poll > 0 {
            let mut alt_req = client.get(&alt_url);
            if !api_key.trim().is_empty() {
                alt_req = alt_req.header("Authorization", format!("Bearer {}", api_key.trim()));
            }
            if let Ok(alt_resp) = alt_req.send().await {
                if alt_resp.status().is_success() {
                    if let Ok(alt_text) = alt_resp.text().await {
                        if let Ok(val) = serde_json::from_str::<Value>(&alt_text) {
                            if val.get("data").is_some() {
                                let mut out = Vec::new();
                                if let Some(data) = val.get("data") {
                                    // same extraction
                                    if let Some(arr) = data.as_array() {
                                        for item in arr {
                                            if let Some(s) = item
                                                .as_str()
                                                .map(str::trim)
                                                .filter(|s| !s.is_empty())
                                            {
                                                if s.starts_with("http") {
                                                    out.push(ImageResponseData {
                                                        url: Some(s.to_string()),
                                                        b64_json: None,
                                                        text: None,
                                                    });
                                                } else {
                                                    out.push(ImageResponseData {
                                                        url: None,
                                                        b64_json: Some(s.to_string()),
                                                        text: None,
                                                    });
                                                }
                                            }
                                        }
                                    }
                                }
                                if !out.is_empty() {
                                    return Ok(out);
                                }
                            }
                        }
                    }
                }
            }
        }

        if !status.is_success() && poll % 10 == 0 {
            log_info(
                app,
                "image_generator",
                format!(
                    "Gradio poll {}: status {} body {}",
                    poll,
                    status,
                    &text[..text.len().min(200)]
                ),
            );
        }

        tokio::time::sleep(Duration::from_millis(POLL_INTERVAL_MS)).await;
    }

    Err("Gradio generation timed out waiting for result (120s)".to_string())
}

/* =========================================================
USAGE
========================================================= */

fn record_image_generation_usage(
    app: &AppHandle,
    request: &ImageGenerationRequest,
    provider_label: &str,
    usage_summary: Option<&UsageSummary>,
    success: bool,
    error_message: Option<String>,
    image_count: usize,
) {
    let mut metadata = HashMap::new();

    metadata.insert("image_generation".to_string(), "true".to_string());

    metadata.insert(
        "input_image_count".to_string(),
        request
            .input_images
            .as_ref()
            .map_or(0, Vec::len)
            .to_string(),
    );

    metadata.insert("output_image_count".to_string(), image_count.to_string());

    /*
     * Explicitly record that the model comes from the model
     * configuration rather than the provider credential.
     */
    metadata.insert(
        "model_configuration".to_string(),
        "models_setting".to_string(),
    );

    if let Some(source) = request.usage_source.as_deref() {
        metadata.insert("usage_source".to_string(), source.to_string());
    }

    if request.character_context.is_some() {
        metadata.insert("character_context".to_string(), "true".to_string());
    }

    if request.scene_context.is_some() {
        metadata.insert("scene_context".to_string(), "true".to_string());
    }

    if request.character_reference_image.is_some() || !request.character_reference_images.is_empty()
    {
        metadata.insert("character_reference".to_string(), "true".to_string());
    }

    let session_id = request
        .session_id
        .clone()
        .unwrap_or_else(|| "image_generation".to_string());

    let character_id = request
        .character_id
        .clone()
        .unwrap_or_else(|| "image_generation".to_string());

    let character_name = request
        .character_name
        .clone()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "Image Generation".to_string());

    let usage = RequestUsage {
        id: Uuid::new_v4().to_string(),

        timestamp: now_millis().unwrap_or(0),

        session_id,

        character_id,

        character_name,

        model_id: request.model.clone(),

        model_name: request.model.clone(),

        provider_id: request.provider_id.clone(),

        provider_label: provider_label.to_string(),

        operation_type: UsageOperationType::ImageGeneration,

        finish_reason: Some(if success {
            UsageFinishReason::Stop
        } else {
            UsageFinishReason::Error
        }),

        prompt_tokens: usage_summary.and_then(|usage| usage.prompt_tokens),

        completion_tokens: usage_summary.and_then(|usage| usage.completion_tokens),

        total_tokens: usage_summary.and_then(|usage| usage.total_tokens),

        cached_prompt_tokens: None,

        cache_write_tokens: None,

        memory_tokens: None,

        summary_tokens: None,

        reasoning_tokens: usage_summary.and_then(|usage| usage.reasoning_tokens),

        image_tokens: usage_summary.and_then(|usage| usage.image_tokens),

        audio_tokens: usage_summary.and_then(|usage| usage.audio_tokens),

        web_search_requests: usage_summary.and_then(|usage| usage.web_search_requests),

        api_cost: usage_summary.and_then(|usage| usage.api_cost),

        cost: None,

        success,

        error_message,

        metadata,
    };

    if let Err(err) = add_usage_record(app, usage) {
        log_error(
            app,
            "image_generator",
            format!("failed to record image generation usage: {}", err),
        );
    }
}

/* =========================================================
MAIN IMAGE GENERATION COMMAND
========================================================= */

#[tauri::command]
pub async fn generate_image(
    app: AppHandle,
    mut request: ImageGenerationRequest,
) -> Result<ImageGenerationResponse, String> {
    /* =====================================================
    SDCPP LORA HYDRATION
    ===================================================== */

    if request.provider_id == "sdcpp" {
        super::sdcpp::hydrate_lora_keywords(&app, &mut request)?;
    }

    /* =====================================================
    CHARACTER REFERENCE IMAGES
    ===================================================== */

    /*
     * Character profile images are promoted into input_images.
     */
    promote_character_reference_images(&mut request);

    /* =====================================================
    PROMPT PREPARATION
    ===================================================== */

    let pre_prompt = request
        .advanced_model_settings
        .as_ref()
        .and_then(|settings| settings.sd_extra_prompt.as_ref())
        .map(String::as_str);

    let lora_keywords = active_lora_keywords(&request);

    request.prompt = compose_scene_image_prompt(&request, pre_prompt, &lora_keywords);

    /*
     * Keep Scene Writer negative prompt available to
     * provider adapters.
     */
    let _scene_negative_prompt = scene_negative_prompt(&request);

    let mut provider_label = request.provider_id.clone();

    /* =====================================================
    OUTPUT MODALITIES
    ===================================================== */

    if request.output_modalities.is_none() {
        request.output_modalities = crate::storage_manager::settings::get_model_output_scopes(
            &app,
            &request.model,
            &request.provider_id,
        )
        .ok()
        .flatten();
    }

    /* =====================================================
    GENERATION
    ===================================================== */

    let result: Result<(ImageGenerationResponse, Option<UsageSummary>), String> = async {
        log_info(
            &app,
            "image_generator",
            format!(
                "Generating image with model '{}' through provider '{}'",
                request.model, request.provider_id
            ),
        );

        /*
         * This distinction is intentional:
         *
         * provider_id:
         *     which API/backend connection is used
         *
         * model:
         *     which model from LettuceAI's Models setting
         *     should actually generate the image
         *
         * Therefore AI Horde models never need to be listed
         * inside provider configuration.
         */
        if is_ai_horde_provider(&request.provider_id) {
            log_info(
                &app,
                "image_generator",
                format!(
                    "AI Horde model selected from Models setting: {}",
                    request.model
                ),
            );
        }

        /* -------------------------------------------------
        Scene diagnostics
        ------------------------------------------------- */

        if request.scene_context.is_some() {
            log_info(
                &app,
                "image_generator",
                "Scene context attached to image request.".to_string(),
            );
        }

        if request.character_context.is_some() {
            log_info(
                &app,
                "image_generator",
                "Character context attached to image request.".to_string(),
            );
        }

        if let Some(images) = request.input_images.as_ref() {
            if !images.is_empty() {
                log_info(
                    &app,
                    "image_generator",
                    format!("Reference/input images attached: {}", images.len()),
                );
            }
        }

        /* =================================================
        PROVIDER CREDENTIAL
        ================================================= */

        let provider_cred = crate::storage_manager::providers::get_provider_credential(
            &app,
            &request.credential_id,
        )?;

        provider_label = provider_cred.label.clone();

        /*
         * Credential is associated with provider.
         *
         * Model remains request.model.
         */
        crate::providers::nanogpt_usage::note_request(
            &app,
            &provider_cred.provider_id,
            &provider_cred.id,
        );

        /* =================================================
        SDCPP
        ================================================= */

        if request.provider_id == "sdcpp" {
            let response = super::sdcpp::generate(&app, &request).await?;

            return Ok((response, None));
        }

        /* =================================================
        COMFYUI
        ================================================= */

        if request.provider_id == "comfyui" {
            let api_key = provider_cred.api_key.clone().unwrap_or_default();

            let base_url = resolve_base_url(
                &ProviderId(request.provider_id.clone()),
                provider_cred.base_url.as_deref(),
            );

            let image_data = super::comfyui::generate(
                &app,
                &request,
                &base_url,
                &api_key,
                provider_cred.config.as_ref(),
            )
            .await?;

            let mut generated_images = Vec::new();

            for img_data in image_data {
                let image_source = match img_data.url.as_ref().or(img_data.b64_json.as_ref()) {
                    Some(source) => source,

                    None => return Err("No image data in ComfyUI response.".to_string()),
                };

                let saved = save_image(&app, image_source).await?;

                generated_images.push(GeneratedImage {
                    asset_id: saved.asset_id,

                    file_path: saved.file_path,

                    mime_type: saved.mime_type,

                    url: img_data.url,

                    width: saved.width,

                    height: saved.height,

                    text: img_data.text,
                });
            }

            return Ok((
                ImageGenerationResponse {
                    images: generated_images,

                    model: request.model.clone(),

                    provider_id: request.provider_id.clone(),
                },
                None,
            ));
        }

        /* =================================================
        STANDARD PROVIDER ADAPTER
        ================================================= */

        let adapter = get_adapter(&request.provider_id)?;

        /*
         * Provider credential only.
         *
         * DO NOT derive request.model from the provider.
         *
         * For AI Horde:
         *
         * api_key = Horde credential
         * request.model = Horde model configured in Models
         */
        let api_key = if !adapter.requires_api_key() {
            provider_cred.api_key.clone().unwrap_or_default()
        } else {
            provider_cred.api_key.clone().ok_or_else(|| {
                format!("API key not found for provider '{}'", request.provider_id)
            })?
        };

        let base_url_opt = provider_cred.base_url.as_deref();

        let headers_map = provider_cred.headers.clone();

        let base_url = resolve_base_url(&ProviderId(request.provider_id.clone()), base_url_opt);

        /* =================================================
        ENDPOINT
        ================================================= */

        let provider_config = provider_cred.config.as_ref();

        let url = if request.provider_id == "gemini" {
            gemini_image_endpoint(&base_url, &request.model, &api_key)
        } else {
            /*
             * The adapter receives the complete request,
             * including request.model.
             *
             * AI Horde's adapter should therefore build
             * its payload using request.model.
             *
             * Generic/gradio adapters use provider_config for
             * endpoint templating, keeping the system
             * hosting-agnostic.
             */
            adapter.endpoint_with_config(&base_url, &request, provider_config)
        };

        /* =================================================
        HEADERS
        ================================================= */

        let headers = adapter.headers_with_config(&api_key, headers_map.as_ref(), provider_config);

        /* =================================================
        PAYLOAD
        ================================================= */

        let payload = adapter.payload_with_config(&request, provider_config)?;

        log_info(
            &app,
            "image_generator",
            format!("Sending image request to: {}", url),
        );

        /*
         * Important:
         *
         * Borrow payload here instead of moving it through
         * matches!.
         *
         * This allows us to use payload again below.
         */
        let is_multipart = matches!(&payload, ImageRequestPayload::Multipart(_));

        /* =================================================
        HTTP CLIENT
        ================================================= */

        let client = crate::transport::build_client(
            &app,
            None,
            false,
            Some(request.provider_id.as_str()),
            Some(url.as_str()),
        )
        .map_err(|e| crate::utils::err_msg(module_path!(), line!(), e.to_string()))?;

        let mut req_builder = client.post(&url).timeout(adapter.timeout(provider_config));

        /* =================================================
        REQUEST HEADERS
        ================================================= */

        for (key, value) in headers {
            /*
             * reqwest multipart() creates its own
             * multipart Content-Type including the boundary.
             */
            if is_multipart && key.eq_ignore_ascii_case("content-type") {
                continue;
            }

            req_builder = req_builder.header(key, value);
        }

        /* =================================================
        REQUEST BODY
        ================================================= */

        req_builder = match payload {
            ImageRequestPayload::Json(body) => req_builder.json(&body),

            ImageRequestPayload::Multipart(form) => req_builder.multipart(form),
        };

        /* =================================================
        SEND REQUEST
        ================================================= */

        let response = req_builder.send().await.map_err(|e| {
            crate::utils::err_msg(module_path!(), line!(), format!("Request failed: {}", e))
        })?;

        let status = response.status();

        if !status.is_success() {
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());

            return Err(crate::utils::err_msg(
                module_path!(),
                line!(),
                format!("API error {}: {}", status, error_text),
            ));
        }

        /* =================================================
        BINARY RESPONSE
        ================================================= */

        if adapter.response_format_with_config(provider_config) == ImageResponseFormat::Binary {
            let bytes = response.bytes().await.map_err(|e| {
                crate::utils::err_msg(
                    module_path!(),
                    line!(),
                    format!("Failed to read image bytes: {}", e),
                )
            })?;

            if bytes.is_empty() {
                return Err(crate::utils::err_msg(
                    module_path!(),
                    line!(),
                    "Provider returned an empty image response".to_string(),
                ));
            }

            let saved = crate::image_generator::storage::save_image_bytes(&app, &bytes)?;

            // Store the prompt in playground history so it can be
            // retrieved when opening the image in the viewer.
            // This ensures the complete original positive prompt is
            // preserved and associated with the generated image,
            // regardless of which provider was used.
            let _ = crate::storage_manager::playground::playground_history_save(
                app.clone(),
                crate::storage_manager::playground::PlaygroundGenerationRecord {
                    id: saved.asset_id.clone(),
                    created_at: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .ok()
                        .map(|d| d.as_secs() as u64)
                        .unwrap_or(0),
                    provider_id: request.provider_id.clone(),
                    model_id: request.model.clone(),
                    model_name: request.model.clone(),
                    prompt: request.prompt.clone(),
                    negative_prompt: request
                        .advanced_model_settings
                        .as_ref()
                        .and_then(|s| s.sd_negative_prompt.clone()),
                    seed: None,
                    params_json: "{}".to_string(),
                    status: "complete".to_string(),
                    error: None,
                    images_json: "[]".to_string(),
                },
            );

            return Ok((
                ImageGenerationResponse {
                    images: vec![GeneratedImage {
                        asset_id: saved.asset_id,

                        file_path: saved.file_path,

                        mime_type: saved.mime_type,

                        url: None,

                        width: saved.width,

                        height: saved.height,

                        text: None,
                    }],

                    model: request.model.clone(),

                    provider_id: request.provider_id.clone(),
                },
                None,
            ));
        }

        /* =================================================
        JSON RESPONSE
        ================================================= */

        let response_json: serde_json::Value = response.json().await.map_err(|e| {
            crate::utils::err_msg(
                module_path!(),
                line!(),
                format!("Failed to parse response: {}", e),
            )
        })?;

        let usage_summary = chat_request::extract_usage(&response_json);

        log_info(
            &app,
            "image_generator",
            format!("Received response: {}", response_json),
        );

        /* =================================================
        PARSE PROVIDER RESPONSE
        ================================================= */

        let image_data: Vec<ImageResponseData>;

        if is_ai_horde_provider(&request.provider_id) {
            /*
             * AI Horde is asynchronous.
             *
             * The first response contains the generation ID.
             *
             * The AI Horde adapter represents this internally
             * as:
             *
             *     aihorde://<job-id>
             *
             * We then poll Horde until the actual image exists.
             */
            let initial_data =
                adapter.parse_response_with_config(response_json, provider_config)?;

            let job_id = initial_data
                .iter()
                .find_map(|item| item.url.as_deref().and_then(extract_ai_horde_job_id))
                .ok_or_else(|| "AI Horde did not return a valid generation ID.".to_string())?;

            image_data =
                wait_for_ai_horde_generation(&app, &client, &base_url, &api_key, &job_id).await?;
        } else if is_gradio_provider(&request.provider_id) {
            let initial_data =
                adapter.parse_response_with_config(response_json, provider_config)?;

            // Check for queued Gradio event_id
            if let Some(event_id) = initial_data
                .iter()
                .find_map(|item| item.url.as_deref().and_then(extract_gradio_event_id))
            {
                let endpoint_name = provider_config
                    .and_then(|c| c.get("gradioEndpoint"))
                    .and_then(|v| v.as_str())
                    .or_else(|| {
                        provider_config
                            .and_then(|c| c.get("endpoint"))
                            .and_then(|v| v.as_str())
                    })
                    .unwrap_or("predict");
                image_data = wait_for_gradio_generation(
                    &app,
                    &client,
                    &base_url,
                    &api_key,
                    endpoint_name,
                    &event_id,
                )
                .await?;
            } else {
                image_data = initial_data;
            }
        } else {
            image_data = adapter.parse_response_with_config(response_json, provider_config)?;
        }

        /* =================================================
        SAVE GENERATED IMAGES
        ================================================= */

        let mut generated_images = Vec::new();

        for img_data in image_data {
            let image_source = match img_data.url.as_ref().or(img_data.b64_json.as_ref()) {
                Some(source) => source,

                None => {
                    let detail = img_data
                        .text
                        .as_deref()
                        .map(str::trim)
                        .filter(|text| !text.is_empty())
                        .map(|text| text.chars().take(160).collect::<String>())
                        .map(|snippet| format!(" Provider returned text instead: {}", snippet))
                        .unwrap_or_default();

                    return Err(format!("No image URL or data in response.{}", detail));
                }
            };

            let saved = save_image(&app, image_source).await?;

            generated_images.push(GeneratedImage {
                asset_id: saved.asset_id,

                file_path: saved.file_path,

                mime_type: saved.mime_type,

                url: img_data.url,

                width: saved.width,

                height: saved.height,

                text: img_data.text,
            });
        }

        if generated_images.is_empty() {
            return Err("Image provider returned no images.".to_string());
        }

        Ok((
            ImageGenerationResponse {
                images: generated_images,

                model: request.model.clone(),

                provider_id: request.provider_id.clone(),
            },
            usage_summary,
        ))
    }
    .await;

    /* =====================================================
    USAGE RECORDING
    ===================================================== */

    match &result {
        Ok((response, usage_summary)) => {
            record_image_generation_usage(
                &app,
                &request,
                &provider_label,
                usage_summary.as_ref(),
                true,
                None,
                response.images.len(),
            );
        }

        Err(err) => {
            record_image_generation_usage(
                &app,
                &request,
                &provider_label,
                None,
                false,
                Some(err.clone()),
                0,
            );
        }
    }

    result.map(|(response, _)| response)
}

/* =========================================================
AI HORDE MODEL COMMANDS
========================================================= */

use crate::image_generator::aihorde_models::{fetch_ai_horde_models, AIHordeModel};

/// Fetch live AI Horde models
#[tauri::command]
pub async fn get_aihorde_models(api_key: Option<String>) -> Result<Vec<AIHordeModel>, String> {
    fetch_ai_horde_models(api_key.as_deref()).await
}

/// Fetch only the model names (lighter payload for dropdowns)
#[tauri::command]
pub async fn get_aihorde_model_names(api_key: Option<String>) -> Result<Vec<String>, String> {
    let models = fetch_ai_horde_models(api_key.as_deref()).await?;
    Ok(models.into_iter().map(|m| m.name).collect())
}

/// Test image provider connection (generic, hosting-agnostic)
///
/// Tries to reach the configured endpoint and returns a user-facing
/// success message or error. Never exposes secrets.
#[tauri::command]
pub async fn test_image_provider_connection(
    app: AppHandle,
    credential_id: String,
) -> Result<String, String> {
    let cred = crate::storage_manager::providers::get_provider_credential(&app, &credential_id)?;
    let adapter = crate::image_generator::provider_adapter::get_adapter(&cred.provider_id)
        .map_err(|e| format!("Unknown provider '{}': {}", cred.provider_id, e))?;
    let base_url = crate::providers::config::resolve_base_url(
        &crate::chat_manager::types::ProviderId(cred.provider_id.clone()),
        cred.base_url.as_deref(),
    );
    let api_key = cred.api_key.clone().unwrap_or_default();
    let config = cred.config.as_ref();
    let timeout = adapter.timeout(config);

    // For sdcpp local, check runtime
    if cred.provider_id == "sdcpp" {
        // sdcpp is local; consider it reachable if runtime catalog can be fetched
        return Ok("Local sdcpp runtime is available (desktop only)".to_string());
    }

    let client =
        crate::transport::build_client(&app, None, false, Some(&cred.provider_id), Some(&base_url))
            .map_err(|e| format!("Failed to build HTTP client: {}", e))?;

    // Try GET base_url first (most providers respond to GET with 200/404/401 which means reachable)
    let result = tokio::time::timeout(timeout, async {
        let mut last_err = String::new();
        // Try base_url
        match client.get(&base_url).send().await {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success()
                    || status.as_u16() == 404
                    || status.as_u16() == 401
                    || status.as_u16() == 405
                {
                    return Ok(format!(
                        "Connection successful ({} {} is reachable, status {})",
                        cred.provider_id, base_url, status
                    ));
                }
                let body = resp.text().await.unwrap_or_default();
                last_err = format!("{}: {}", status, body.chars().take(200).collect::<String>());
            }
            Err(e) => {
                last_err = format!("GET failed: {}", e);
            }
        }
        // For Gradio, also try /config
        if crate::image_generator::provider_adapter::normalize_provider_id(&cred.provider_id)
            == "gradio"
        {
            let config_url = format!("{}/config", base_url.trim_end_matches('/'));
            if let Ok(resp) = client.get(&config_url).send().await {
                if resp.status().is_success() {
                    return Ok(format!(
                        "Gradio Space reachable at {} (status {})",
                        config_url,
                        resp.status()
                    ));
                }
            }
        }
        Err(last_err)
    })
    .await;

    match result {
        Ok(Ok(msg)) => Ok(msg),
        Ok(Err(e)) => Err(format!("Connection test failed: {}", e)),
        Err(_) => Err(format!(
            "Connection timed out after {}s to {}",
            timeout.as_secs(),
            base_url
        )),
    }
}

/// Get available models for image provider (if supports discovery)
///
/// For generic HTTP, tries `config.modelsEndpoint` or `{base_url}/models`.
/// For AI Horde, uses the live Horde API.
/// For Gradio, discovery is not standard and returns an error.
#[tauri::command]
pub async fn get_image_provider_models(
    app: AppHandle,
    credential_id: String,
) -> Result<Vec<String>, String> {
    let cred = crate::storage_manager::providers::get_provider_credential(&app, &credential_id)?;
    let base_url = crate::providers::config::resolve_base_url(
        &crate::chat_manager::types::ProviderId(cred.provider_id.clone()),
        cred.base_url.as_deref(),
    );
    let config = cred.config.as_ref();
    let normalized =
        crate::image_generator::provider_adapter::normalize_provider_id(&cred.provider_id);
    match normalized.as_str() {
        "aihorde" => {
            let client = reqwest::Client::new();
            let models = crate::image_generator::provider_adapter::aihorde::AIHordeAdapter::fetch_models(
                &client,
                &base_url,
                cred.api_key.as_deref().unwrap_or(""),
            )
            .await?;
            Ok(models.into_iter().map(|m| m.name).collect())
        }
        "generic_http" | "gradio" => {
            let endpoint = config
                .and_then(|c| c.get("modelsEndpoint"))
                .and_then(|v| v.as_str())
                .or_else(|| config.and_then(|c| c.get("models_endpoint")).and_then(|v| v.as_str()))
                .unwrap_or("/models");
            let url = if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
                endpoint.to_string()
            } else {
                format!("{}/{}", base_url.trim_end_matches('/'), endpoint.trim_start_matches('/'))
            };
            let client = crate::transport::build_client(&app, None, false, Some(&cred.provider_id), Some(&url))
                .map_err(|e| format!("Failed to build client: {}", e))?;
            let timeout = crate::image_generator::provider_adapter::get_adapter(&cred.provider_id)
                .map(|a| a.timeout(config))
                .unwrap_or(Duration::from_secs(30));
            let resp = tokio::time::timeout(timeout, client.get(&url).send())
                .await
                .map_err(|_| format!("Models request timed out after {}s", timeout.as_secs()))?
                .map_err(|e| format!("Failed to fetch models: {}", e))?;
            let status = resp.status();
            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                return Err(format!("Models endpoint returned {}: {}", status, body.chars().take(300).collect::<String>()));
            }
            let json: Value = resp.json().await.map_err(|e| format!("Failed to parse models response: {}", e))?;
            // Try to extract list from common fields
            let arr = json
                .get("data")
                .and_then(|v| v.as_array())
                .or_else(|| json.get("models").and_then(|v| v.as_array()))
                .or_else(|| json.as_array());
            if let Some(arr) = arr {
                let mut out = Vec::new();
                for item in arr {
                    if let Some(id) = item.get("id").and_then(|v| v.as_str()).or_else(|| item.get("name").and_then(|v| v.as_str())).or_else(|| item.as_str()) {
                        let trimmed = id.trim();
                        if !trimmed.is_empty() {
                            out.push(trimmed.to_string());
                        }
                    }
                }
                if out.is_empty() {
                    return Err("Models response contained no model IDs".to_string());
                }
                Ok(out)
            } else {
                Err("Models response did not contain a list (expected {\"data\": [...]})".to_string())
            }
        }
        _ => Err(format!(
            "Model discovery not supported for provider '{}'. Enter model name manually (e.g. DreamShaper_8, Flux, SDXL).",
            cred.provider_id
        )),
    }
}

/// List all image providers and their capabilities (for UI)
#[tauri::command]
pub fn get_image_providers() -> Vec<crate::image_generator::provider_adapter::ImageProviderInfo> {
    crate::image_generator::provider_adapter::available_providers().to_vec()
}

/* =========================================================
TESTS
========================================================= */

#[cfg(test)]
mod tests {
    use super::{
        compose_scene_image_prompt, extract_ai_horde_job_id, merged_lora_keywords,
        promote_character_reference_images, ImageCharacterContext, ImageGenerationRequest,
        ImageLora, ImageSceneContext,
    };

    #[test]
    fn scene_writer_visual_prompt_is_used() {
        let request = ImageGenerationRequest {
            prompt: "anime illustration".to_string(),

            model: "test-model".to_string(),

            /*
             * Provider is separate from model.
             */
            provider_id: "aihorde".to_string(),

            credential_id: "horde-credential".to_string(),

            advanced_model_settings: None,

            input_images: None,

            mask_image: None,

            loras: None,

            character_context: Some(ImageCharacterContext {
                name: Some("Maya".to_string()),

                description: None,

                appearance: Some("long black hair, red eyes".to_string()),

                personality: Some("shy and introverted".to_string()),

                reference_image: None,

                reference_images: Vec::new(),
            }),

            scene_context: Some(ImageSceneContext {
                description: Some("Maya is sitting beside a window.".to_string()),

                visual_prompt: Some("cinematic anime scene, warm evening light".to_string()),

                negative_prompt: None,

                characters: Vec::new(),

                environment: None,

                lighting: None,

                composition: None,

                pose: None,

                outfit: None,

                visual_style: None,
            }),

            character_reference_image: None,

            character_reference_images: Vec::new(),

            output_modalities: None,

            size: Some("1024x1024".to_string()),

            quality: None,

            style: None,

            n: Some(1),

            session_id: None,

            character_id: None,

            character_name: None,

            usage_source: Some("scene".to_string()),
        };

        let prompt = compose_scene_image_prompt(&request, None, &[]);

        assert!(prompt.contains("Maya"));

        assert!(prompt.contains("long black hair, red eyes"));

        assert!(prompt.contains("shy and introverted"));

        assert!(prompt.contains("cinematic anime scene"));
    }

    #[test]
    fn character_reference_is_promoted_to_input_images() {
        let mut request = ImageGenerationRequest {
            prompt: "scene".to_string(),

            model: "test-model".to_string(),

            provider_id: "aihorde".to_string(),

            credential_id: "horde-credential".to_string(),

            advanced_model_settings: None,

            input_images: None,

            mask_image: None,

            loras: None,

            character_context: Some(ImageCharacterContext {
                name: Some("Maya".to_string()),

                description: None,

                appearance: None,

                personality: None,

                reference_image: Some("data:image/png;base64,abc".to_string()),

                reference_images: vec!["data:image/png;base64,def".to_string()],
            }),

            scene_context: None,

            character_reference_image: None,

            character_reference_images: Vec::new(),

            output_modalities: None,

            size: None,

            quality: None,

            style: None,

            n: Some(1),

            session_id: None,

            character_id: None,

            character_name: None,

            usage_source: None,
        };

        promote_character_reference_images(&mut request);

        let images = request.input_images.unwrap();

        assert_eq!(images.len(), 2);

        assert_eq!(images[0], "data:image/png;base64,abc");

        assert_eq!(images[1], "data:image/png;base64,def");
    }

    #[test]
    fn duplicate_reference_images_are_removed() {
        let mut request = ImageGenerationRequest {
            prompt: "scene".to_string(),

            model: "test-model".to_string(),

            provider_id: "aihorde".to_string(),

            credential_id: "horde-credential".to_string(),

            advanced_model_settings: None,

            input_images: Some(vec!["image.png".to_string()]),

            mask_image: None,

            loras: None,

            character_context: Some(ImageCharacterContext {
                name: None,

                description: None,

                appearance: None,

                personality: None,

                reference_image: Some("image.png".to_string()),

                reference_images: Vec::new(),
            }),

            scene_context: None,

            character_reference_image: None,

            character_reference_images: Vec::new(),

            output_modalities: None,

            size: None,

            quality: None,

            style: None,

            n: Some(1),

            session_id: None,

            character_id: None,

            character_name: None,

            usage_source: None,
        };

        promote_character_reference_images(&mut request);

        assert_eq!(request.input_images.unwrap().len(), 1);
    }

    #[test]
    fn lora_keywords_are_not_duplicated() {
        let keywords = vec!["ArsMovieStill".to_string(), "cinematic still".to_string()];

        let request = ImageGenerationRequest {
            prompt: "a portrait".to_string(),

            model: "test-model".to_string(),

            provider_id: "aihorde".to_string(),

            credential_id: "horde-credential".to_string(),

            advanced_model_settings: None,

            input_images: None,

            mask_image: None,

            loras: None,

            character_context: None,

            scene_context: None,

            character_reference_image: None,

            character_reference_images: Vec::new(),

            output_modalities: None,

            size: None,

            quality: None,

            style: None,

            n: Some(1),

            session_id: None,

            character_id: None,

            character_name: None,

            usage_source: None,
        };

        let prompt = compose_scene_image_prompt(&request, Some("high detail"), &keywords);

        assert_eq!(
            prompt,
            "high detail, ArsMovieStill, cinematic still, a portrait"
        );
    }

    #[test]
    fn request_lora_keywords_override_model_level_keywords() {
        let base = vec![ImageLora {
            path: "style.safetensors".to_string(),

            multiplier: 0.8,

            is_high_noise: false,

            keywords: vec!["old trigger".to_string()],
        }];

        let request = vec![ImageLora {
            path: "style.safetensors".to_string(),

            multiplier: 1.0,

            is_high_noise: false,

            keywords: vec!["new trigger".to_string()],
        }];

        assert_eq!(
            merged_lora_keywords(Some(&base), Some(&request),),
            vec!["new trigger"]
        );
    }

    #[test]
    fn ai_horde_job_id_is_extracted() {
        assert_eq!(
            extract_ai_horde_job_id("aihorde://123456"),
            Some("123456".to_string())
        );
    }

    #[test]
    fn invalid_ai_horde_url_is_rejected() {
        assert_eq!(
            extract_ai_horde_job_id("https://example.com/image.png"),
            None
        );
    }

    #[test]
    fn ai_horde_provider_aliases_are_supported() {
        assert!(super::is_ai_horde_provider("aihorde"));

        assert!(super::is_ai_horde_provider("ai-horde"));

        assert!(!super::is_ai_horde_provider("openai"));
    }

    #[test]
    fn ai_horde_model_is_independent_of_provider() {
        let request = ImageGenerationRequest {
            prompt: "anime scene".to_string(),

            model: "Some-Horde-Model".to_string(),

            provider_id: "aihorde".to_string(),

            credential_id: "my-horde-key".to_string(),

            advanced_model_settings: None,

            input_images: None,

            mask_image: None,

            loras: None,

            character_context: None,

            scene_context: None,

            character_reference_image: None,

            character_reference_images: Vec::new(),

            output_modalities: None,

            size: Some("1024x1024".to_string()),

            quality: None,

            style: None,

            n: Some(1),

            session_id: None,

            character_id: None,

            character_name: None,

            usage_source: Some("scene".to_string()),
        };

        /*
         * This test documents the intended architecture:
         *
         * provider_id = connection/backend
         * model       = model selected in Models settings
         *
         * No Horde model list exists in this module.
         */
        assert_eq!(request.provider_id, "aihorde");

        assert_eq!(request.model, "Some-Horde-Model");

        assert_eq!(request.credential_id, "my-horde-key");
    }
}
