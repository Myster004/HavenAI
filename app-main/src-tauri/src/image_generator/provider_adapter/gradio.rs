use std::collections::HashMap;
use std::time::Duration;

use serde_json::{json, Value};

use super::{
    parse_size_dimensions, ImageProviderAdapter, ImageRequestPayload, ImageResponseData,
    ImageResponseFormat,
};
use crate::image_generator::types::ImageGenerationRequest;

// =========================================================
// GRADIO / HUGGING FACE SPACE ADAPTER
// =========================================================

/// Gradio / Hugging Face Space adapter.
///
/// Supports:
/// - Externally hosted Hugging Face Spaces (e.g. https://user-space.hf.space)
/// - Gradio 3 (`/api/predict`) and Gradio 4+ (`/gradio_api/call/predict`)
/// - Docker Spaces that expose a Gradio UI
/// - Any Gradio-compatible endpoint with configurable `data` mapping
///
/// Configuration via `provider_credential.config`:
///
/// - `gradioEndpoint`: API endpoint name (default "predict")
/// - `gradioFnIndex`: fn_index for Gradio 3 (default 0)
/// - `gradioUseQueue`: whether to use queue API (/gradio_api/call/...) default true
/// - `authType`: none/bearer/apiKey
/// - `timeoutSeconds`: default 120
///
/// Hosting agnostic: `base_url` can be localhost, LAN IP, HF Space, VPS, etc.
/// Model is always `request.model` (e.g. DreamShaper_8, Flux, SDXL) and never hardcoded.
///
pub struct GradioAdapter;

fn normalize_base_url(base_url: &str) -> String {
    base_url.trim_end_matches('/').to_string()
}

fn get_size(request: &ImageGenerationRequest) -> (u32, u32) {
    let advanced = request.advanced_model_settings.as_ref();
    let size_override = request
        .size
        .as_deref()
        .or_else(|| advanced.and_then(|s| s.sd_size.as_deref()));
    parse_size_dimensions(size_override, 512, 512)
}

fn get_steps(request: &ImageGenerationRequest) -> u32 {
    request
        .advanced_model_settings
        .as_ref()
        .and_then(|s| s.sd_steps)
        .unwrap_or(25)
}

fn get_cfg(request: &ImageGenerationRequest) -> f64 {
    request
        .advanced_model_settings
        .as_ref()
        .and_then(|s| s.sd_cfg_scale)
        .unwrap_or(7.0)
}

fn get_seed(request: &ImageGenerationRequest) -> i64 {
    request
        .advanced_model_settings
        .as_ref()
        .and_then(|s| s.sd_seed)
        .map(|v| v as i64)
        .unwrap_or(-1)
}

fn effective_api_key(api_key: &str) -> String {
    let trimmed = api_key.trim();
    if !trimmed.is_empty() {
        return trimmed.to_string();
    }
    for key in [
        "IMAGE_API_KEY",
        "HF_API_KEY",
        "HF_TOKEN",
        "HUGGINGFACE_API_KEY",
    ] {
        if let Ok(val) = std::env::var(key) {
            let t = val.trim();
            if !t.is_empty() {
                return t.to_string();
            }
        }
    }
    String::new()
}

fn config_str<'a>(config: Option<&'a Value>, key: &str) -> Option<&'a str> {
    config?
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

impl GradioAdapter {
    fn gradio_endpoint_name(config: Option<&Value>) -> String {
        config_str(config, "gradioEndpoint")
            .or_else(|| config_str(config, "endpoint"))
            .or_else(|| config_str(config, "apiEndpoint"))
            .unwrap_or("predict")
            .trim_matches('/')
            .to_string()
    }

    fn gradio_fn_index(config: Option<&Value>) -> u32 {
        config
            .and_then(|c| c.get("gradioFnIndex"))
            .and_then(Value::as_u64)
            .map(|v| v as u32)
            .or_else(|| {
                config
                    .and_then(|c| c.get("fnIndex"))
                    .and_then(Value::as_u64)
                    .map(|v| v as u32)
            })
            .unwrap_or(0)
    }

    fn use_queue(config: Option<&Value>) -> bool {
        config
            .and_then(|c| c.get("gradioUseQueue"))
            .and_then(Value::as_bool)
            .or_else(|| {
                config
                    .and_then(|c| c.get("useQueue"))
                    .and_then(Value::as_bool)
            })
            .unwrap_or(true)
    }

    fn build_gradio_data(request: &ImageGenerationRequest, config: Option<&Value>) -> Vec<Value> {
        // If config has gradioDataTemplate as JSON array string, use it
        if let Some(template) = config_str(config, "gradioDataTemplate") {
            // Try to parse as JSON array and replace placeholders
            // For now, support simple replacement for {{prompt}} etc.
            let (width, height) = get_size(request);
            let steps = get_steps(request);
            let cfg = get_cfg(request);
            let seed = get_seed(request);
            let mut rendered = template.to_string();
            rendered = rendered.replace("{{prompt}}", &request.prompt.replace('"', "\\\""));
            rendered = rendered.replace("{{model}}", &request.model.replace('"', "\\\""));
            rendered = rendered.replace("{{width}}", &width.to_string());
            rendered = rendered.replace("{{height}}", &height.to_string());
            rendered = rendered.replace("{{steps}}", &steps.to_string());
            rendered = rendered.replace("{{cfg_scale}}", &cfg.to_string());
            rendered = rendered.replace("{{seed}}", &seed.to_string());
            if let Ok(val) = serde_json::from_str::<Value>(&rendered) {
                if let Some(arr) = val.as_array() {
                    return arr.clone();
                }
                return vec![val];
            }
        }

        // Default: data array with prompt as first element, plus optional params
        // Many DreamShaper-like Gradio spaces expect: [prompt, negative_prompt, steps, cfg, width, height, seed]
        // We provide a minimal compatible payload that works for simple spaces:
        // For maximum compatibility, we check config for explicit mapping
        if let Some(mapping) = config
            .and_then(|c| c.get("gradioInputsMapping"))
            .and_then(Value::as_array)
        {
            let mut data = Vec::new();
            for item in mapping {
                if let Some(field) = item.as_str() {
                    let val = match field {
                        "prompt" => Value::String(request.prompt.clone()),
                        "negative_prompt" => Value::String(
                            request
                                .advanced_model_settings
                                .as_ref()
                                .and_then(|s| s.sd_negative_prompt.clone())
                                .unwrap_or_default(),
                        ),
                        "model" => Value::String(request.model.clone()),
                        "width" => Value::Number(serde_json::Number::from(get_size(request).0)),
                        "height" => Value::Number(serde_json::Number::from(get_size(request).1)),
                        "steps" => Value::Number(serde_json::Number::from(get_steps(request))),
                        "cfg_scale" => Value::Number(
                            serde_json::Number::from_f64(get_cfg(request))
                                .unwrap_or(serde_json::Number::from(7)),
                        ),
                        "seed" => Value::Number(serde_json::Number::from(get_seed(request))),
                        "n" => Value::Number(serde_json::Number::from(request.n.unwrap_or(1))),
                        _ => Value::Null,
                    };
                    data.push(val);
                }
            }
            return data;
        }

        // Simple default: just prompt. This works for many HF Spaces where the Gradio interface
        // has a single textbox for prompt. For spaces with more inputs, the user can configure
        // gradioDataTemplate or gradioInputsMapping.
        vec![Value::String(request.prompt.clone())]
    }

    pub fn is_gradio_response_queued(response: &Value) -> bool {
        response
            .get("event_id")
            .and_then(Value::as_str)
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false)
    }

    pub fn extract_event_id(response: &Value) -> Option<String> {
        response
            .get("event_id")
            .and_then(Value::as_str)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }
}

impl ImageProviderAdapter for GradioAdapter {
    fn endpoint(&self, base_url: &str, _request: &ImageGenerationRequest) -> String {
        self.endpoint_with_config(base_url, _request, None)
    }

    fn endpoint_with_config(
        &self,
        base_url: &str,
        _request: &ImageGenerationRequest,
        config: Option<&Value>,
    ) -> String {
        let base = normalize_base_url(base_url);
        let endpoint_name = Self::gradio_endpoint_name(config);
        // Gradio 4+ queue API: /gradio_api/call/{endpoint}
        // For backward compat, also support /api/predict
        if Self::use_queue(config) {
            format!("{}/gradio_api/call/{}", base, endpoint_name)
        } else {
            format!("{}/api/predict", base)
        }
    }

    fn required_auth_headers(&self) -> &'static [&'static str] {
        &[]
    }

    fn headers(
        &self,
        _api_key: &str,
        extra: Option<&HashMap<String, String>>,
    ) -> HashMap<String, String> {
        let mut headers = HashMap::new();
        headers.insert("Content-Type".to_string(), "application/json".to_string());
        headers.insert("Accept".to_string(), "application/json".to_string());
        if let Some(extra) = extra {
            for (k, v) in extra.iter() {
                headers.insert(k.clone(), v.clone());
            }
        }
        headers
    }

    fn headers_with_config(
        &self,
        api_key: &str,
        extra: Option<&HashMap<String, String>>,
        config: Option<&Value>,
    ) -> HashMap<String, String> {
        let mut headers = self.headers(api_key, extra);
        let auth_type = config_str(config, "authType")
            .unwrap_or("none")
            .to_ascii_lowercase();
        let effective_key = effective_api_key(api_key);
        match auth_type.as_str() {
            "bearer" => {
                if !effective_key.is_empty() {
                    headers.insert(
                        "Authorization".to_string(),
                        format!("Bearer {}", effective_key),
                    );
                }
            }
            "apikey" | "api_key" | "hf" | "huggingface" => {
                if !effective_key.is_empty() {
                    // HF Spaces often use Authorization: Bearer <token>
                    headers.insert(
                        "Authorization".to_string(),
                        format!("Bearer {}", effective_key),
                    );
                }
            }
            "custom" => {
                if let Some(custom_headers) = config
                    .and_then(|c| c.get("customHeaders"))
                    .and_then(Value::as_object)
                {
                    for (k, v) in custom_headers {
                        if let Some(val) = v.as_str() {
                            headers.insert(k.clone(), val.to_string());
                        }
                    }
                }
            }
            _ => {
                if !effective_key.is_empty() {
                    // Default to Bearer for HF
                    headers.insert(
                        "Authorization".to_string(),
                        format!("Bearer {}", effective_key),
                    );
                }
            }
        }
        headers
    }

    fn payload(&self, request: &ImageGenerationRequest) -> Result<ImageRequestPayload, String> {
        self.payload_with_config(request, None)
    }

    fn payload_with_config(
        &self,
        request: &ImageGenerationRequest,
        config: Option<&Value>,
    ) -> Result<ImageRequestPayload, String> {
        let data = Self::build_gradio_data(request, config);
        let fn_index = Self::gradio_fn_index(config);

        // Gradio 4 queue API expects {"data": [...], "fn_index": 0, "session_hash": "..."}
        // Gradio 3 /api/predict expects {"data": [...], "fn_index": 0}
        // We will send the queue-style payload; for non-queue we also use same but endpoint is /api/predict
        let body = if Self::use_queue(config) {
            json!({
                "data": data,
                "fn_index": fn_index,
                "session_hash": "haven_session"
            })
        } else {
            json!({
                "data": data,
                "fn_index": fn_index
            })
        };
        Ok(ImageRequestPayload::Json(body))
    }

    fn parse_response(&self, response: Value) -> Result<Vec<ImageResponseData>, String> {
        self.parse_response_with_config(response, None)
    }

    fn parse_response_with_config(
        &self,
        response: Value,
        _config: Option<&Value>,
    ) -> Result<Vec<ImageResponseData>, String> {
        // Check for queued response with event_id
        if let Some(event_id) = Self::extract_event_id(&response) {
            // Return a placeholder that commands.rs can detect and poll
            return Ok(vec![ImageResponseData {
                url: Some(format!("gradio://{}", event_id)),
                b64_json: None,
                text: None,
            }]);
        }

        // Check for error
        if let Some(err) = response
            .get("error")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return Err(format!("Gradio error: {}", err));
        }

        // Gradio typically returns {"data": ["base64...", ...]} or {"data": [{"url": "..."}]}
        if let Some(data) = response.get("data") {
            let mut out = Vec::new();
            Self::collect_gradio_data(data, &mut out);
            if !out.is_empty() {
                return Ok(out);
            }
        }

        // Also try direct image fields (for Docker-style spaces that return {"image": "base64..."})
        let mut out = Vec::new();
        Self::collect_generic(&response, &mut out);
        if !out.is_empty() {
            return Ok(out);
        }

        Err(format!(
            "Gradio response did not contain image data. Response: {}",
            truncate(&response)
        ))
    }

    fn response_format(&self) -> ImageResponseFormat {
        ImageResponseFormat::Json
    }

    fn response_format_with_config(&self, config: Option<&Value>) -> ImageResponseFormat {
        let fmt = config_str(config, "responseFormat")
            .unwrap_or("json")
            .to_ascii_lowercase();
        match fmt.as_str() {
            "binary" => ImageResponseFormat::Binary,
            _ => ImageResponseFormat::Json,
        }
    }

    fn timeout(&self, config: Option<&Value>) -> Duration {
        let secs = config
            .and_then(|c| c.get("timeoutSeconds"))
            .and_then(Value::as_u64)
            .or_else(|| {
                config
                    .and_then(|c| c.get("timeout_seconds"))
                    .and_then(Value::as_u64)
            })
            .unwrap_or(120);
        Duration::from_secs(secs.clamp(5, 600))
    }
}

impl GradioAdapter {
    fn collect_gradio_data(value: &Value, out: &mut Vec<ImageResponseData>) {
        match value {
            Value::String(s) => {
                let trimmed = s.trim();
                if trimmed.is_empty() {
                    return;
                }
                if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
                    out.push(ImageResponseData {
                        url: Some(trimmed.to_string()),
                        b64_json: None,
                        text: None,
                    });
                } else if trimmed.starts_with("data:") {
                    out.push(ImageResponseData {
                        url: None,
                        b64_json: Some(trimmed.to_string()),
                        text: None,
                    });
                } else if trimmed.len() > 20 && !trimmed.contains(' ') {
                    // Likely base64
                    out.push(ImageResponseData {
                        url: None,
                        b64_json: Some(trimmed.to_string()),
                        text: None,
                    });
                } else {
                    // Could be a URL or base64 without prefix, try as b64
                    out.push(ImageResponseData {
                        url: None,
                        b64_json: Some(trimmed.to_string()),
                        text: None,
                    });
                }
            }
            Value::Array(arr) => {
                for item in arr {
                    Self::collect_gradio_data(item, out);
                }
            }
            Value::Object(obj) => {
                // Gradio may return {"url": "https://...", "is_file": true} or {"data": "...", "is_file": false}
                if let Some(url) = obj.get("url").and_then(Value::as_str) {
                    out.push(ImageResponseData {
                        url: Some(url.to_string()),
                        b64_json: None,
                        text: None,
                    });
                    return;
                }
                if let Some(path) = obj.get("path").and_then(Value::as_str) {
                    // File path from Gradio, may need to be turned into URL via /file={path}
                    // For now, treat as url if it looks like http, otherwise as b64 placeholder
                    if path.starts_with("http") {
                        out.push(ImageResponseData {
                            url: Some(path.to_string()),
                            b64_json: None,
                            text: None,
                        });
                    }
                    return;
                }
                // Try data field
                if let Some(data) = obj.get("data") {
                    Self::collect_gradio_data(data, out);
                    if !out.is_empty() {
                        return;
                    }
                }
                // Fallback: try all values
                for val in obj.values() {
                    Self::collect_gradio_data(val, out);
                }
            }
            _ => {}
        }
    }

    fn collect_generic(value: &Value, out: &mut Vec<ImageResponseData>) {
        // Similar to generic_http's collect
        match value {
            Value::Object(obj) => {
                for key in [
                    "image",
                    "images",
                    "b64_json",
                    "base64",
                    "url",
                    "urls",
                    "output",
                    "result",
                    "data",
                    "generated_image",
                ] {
                    if let Some(val) = obj.get(key) {
                        let mut tmp = Vec::new();
                        Self::collect_gradio_data(val, &mut tmp);
                        if !tmp.is_empty() {
                            out.extend(tmp);
                            return;
                        }
                    }
                }
                // Try all values
                for val in obj.values() {
                    Self::collect_gradio_data(val, out);
                }
            }
            _ => Self::collect_gradio_data(&value, out),
        }
    }
}

fn truncate(value: &Value) -> String {
    let s = value.to_string();
    if s.len() > 800 {
        format!("{}... (truncated)", &s[..800])
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image_generator::types::ImageGenerationRequest;
    use serde_json::json;

    fn base_request(prompt: &str, model: &str) -> ImageGenerationRequest {
        ImageGenerationRequest {
            prompt: prompt.to_string(),
            model: model.to_string(),
            provider_id: "gradio".to_string(),
            credential_id: "test".to_string(),
            advanced_model_settings: None,
            input_images: None,
            mask_image: None,
            loras: None,
            character_context: None,
            character_reference_image: None,
            character_reference_images: Vec::new(),
            scene_context: None,
            output_modalities: None,
            size: Some("512x512".to_string()),
            quality: None,
            style: None,
            n: Some(1),
            session_id: None,
            character_id: None,
            character_name: None,
            usage_source: None,
        }
    }

    #[test]
    fn gradio_endpoint_defaults_to_queue() {
        let adapter = GradioAdapter;
        let req = base_request("test", "DreamShaper_8");
        let url = adapter.endpoint("https://user-space.hf.space", &req);
        assert_eq!(url, "https://user-space.hf.space/gradio_api/call/predict");
    }

    #[test]
    fn gradio_endpoint_custom() {
        let adapter = GradioAdapter;
        let req = base_request("test", "DreamShaper_8");
        let config = json!({"gradioEndpoint": "generate"});
        let url = adapter.endpoint_with_config("https://example.com", &req, Some(&config));
        assert_eq!(url, "https://example.com/gradio_api/call/generate");
    }

    #[test]
    fn gradio_endpoint_no_queue() {
        let adapter = GradioAdapter;
        let req = base_request("test", "m");
        let config = json!({"gradioUseQueue": false});
        let url = adapter.endpoint_with_config("https://example.com", &req, Some(&config));
        assert_eq!(url, "https://example.com/api/predict");
    }

    #[test]
    fn gradio_payload_contains_prompt() {
        let adapter = GradioAdapter;
        let req = base_request("a cat", "DreamShaper_8");
        let payload = adapter.payload(&req).unwrap();
        match payload {
            super::super::ImageRequestPayload::Json(val) => {
                let data = val.get("data").and_then(Value::as_array).unwrap();
                assert_eq!(data[0].as_str().unwrap(), "a cat");
            }
            _ => panic!(),
        }
    }

    #[test]
    fn gradio_parses_base64_response() {
        let adapter = GradioAdapter;
        let resp = json!({"data": ["iVBORw0KGgoAAAANSUhEUgAAAAEAAAAB"]});
        let images = adapter.parse_response(resp).unwrap();
        assert_eq!(images.len(), 1);
        assert!(images[0].b64_json.is_some());
    }

    #[test]
    fn gradio_parses_url_response() {
        let adapter = GradioAdapter;
        let resp = json!({"data": ["https://example.com/image.png"]});
        let images = adapter.parse_response(resp).unwrap();
        assert_eq!(images.len(), 1);
        assert_eq!(
            images[0].url.as_deref(),
            Some("https://example.com/image.png")
        );
    }

    #[test]
    fn gradio_handles_queued_event_id() {
        let adapter = GradioAdapter;
        let resp = json!({"event_id": "abc123"});
        let images = adapter.parse_response(resp).unwrap();
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].url.as_deref(), Some("gradio://abc123"));
    }

    #[test]
    fn gradio_handles_docker_style_response() {
        let adapter = GradioAdapter;
        let resp = json!({"image": "base64_docker"});
        let images = adapter.parse_response(resp).unwrap();
        assert_eq!(images.len(), 1);
    }

    #[test]
    fn hosting_agnostic_localhost_and_hf() {
        let adapter = GradioAdapter;
        let req = base_request("test", "Flux");
        let localhost = adapter.endpoint("http://localhost:7860", &req);
        let lan = adapter.endpoint("http://192.168.1.10:7860", &req);
        let hf = adapter.endpoint("https://user-dreamshaper.hf.space", &req);
        assert!(localhost.contains("localhost"));
        assert!(lan.contains("192.168.1.10"));
        assert!(hf.contains("hf.space"));
        // All should use same provider logic, no hardcoded host
        let payload_str = match adapter.payload(&req).unwrap() {
            crate::image_generator::provider_adapter::ImageRequestPayload::Json(v) => v.to_string(),
            crate::image_generator::provider_adapter::ImageRequestPayload::Multipart(_) => {
                String::new()
            }
        };
        assert_eq!(payload_str.contains("test"), true);
    }

    #[test]
    fn model_switching_reflected() {
        let adapter = GradioAdapter;
        // Gradio's default payload doesn't include model, but we ensure prompt still works
        // For providers that use model field, generic does; Gradio is prompt-only by default
        // This test ensures model switching doesn't break payload
        let req1 = base_request("cat", "DreamShaper_8");
        let req2 = base_request("cat", "Flux");
        assert!(adapter.payload(&req1).is_ok());
        assert!(adapter.payload(&req2).is_ok());
        // If user configures gradioDataTemplate to include model, it should be rendered
        let config = json!({"gradioDataTemplate": "[\"{{prompt}}\", \"{{model}}\"]"});
        let p1 = adapter.payload_with_config(&req1, Some(&config)).unwrap();
        let p2 = adapter.payload_with_config(&req2, Some(&config)).unwrap();
        match (p1, p2) {
            (
                super::super::ImageRequestPayload::Json(v1),
                super::super::ImageRequestPayload::Json(v2),
            ) => {
                assert!(v1.get("data").unwrap().as_array().unwrap()[1]
                    .as_str()
                    .unwrap()
                    .contains("DreamShaper"));
                assert!(v2.get("data").unwrap().as_array().unwrap()[1]
                    .as_str()
                    .unwrap()
                    .contains("Flux"));
            }
            _ => panic!(),
        }
    }

    #[test]
    fn authentication_via_env() {
        let adapter = GradioAdapter;
        // Set env var temporarily
        std::env::set_var("HF_API_KEY", "test_hf_token");
        let headers = adapter.headers_with_config("", None, None);
        // Gradio default adds Authorization if env key exists
        assert!(headers.get("Authorization").is_some());
        std::env::remove_var("HF_API_KEY");
    }

    #[test]
    fn invalid_response_errors() {
        let adapter = GradioAdapter;
        let resp = json!({"status": "ok"});
        let err = adapter.parse_response(resp).unwrap_err();
        assert!(err.contains("did not contain image data"));
    }
}
