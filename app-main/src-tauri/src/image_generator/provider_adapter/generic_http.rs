use std::collections::HashMap;
use std::time::Duration;

use serde_json::{json, Value};

use super::{
    parse_size_dimensions, ImageProviderAdapter, ImageRequestPayload, ImageResponseData,
    ImageResponseFormat,
};
use crate::image_generator::types::ImageGenerationRequest;

// =========================================================
// GENERIC HTTP ADAPTER
// =========================================================

/// Generic HTTP image generation adapter.
///
/// This adapter is hosting-agnostic and provider-agnostic.
/// It allows Haven to connect to ANY HTTP/HTTPS image generation
/// endpoint without source-code changes.
///
/// Configuration is supplied via `provider_credential.base_url` (endpoint)
/// and `provider_credential.config` (JSON object) with fields like:
///
/// - `endpointPath`: optional path to append to base_url
/// - `requestMethod`: POST (default)
/// - `authType`: none/bearer/apiKey/custom
/// - `authHeaderName`: header name for apiKey/custom
/// - `requestTemplate`: JSON string with {{placeholders}}
/// - `responseImageField`: JSON path to image data (e.g. "image", "data[0]")
/// - `responseFormat`: json/binary (default json)
/// - `timeoutSeconds`: 5-600 (default 120)
///
/// Model and prompt are always taken from `ImageGenerationRequest` and
/// never hardcoded. Example model: DreamShaper_8 is just a value of
/// `request.model` supplied at runtime.
///
pub struct GenericHttpAdapter;

// ---------------------------------------------------------
// Helpers
// ---------------------------------------------------------

fn env_api_key() -> Option<String> {
    for key in [
        "IMAGE_API_KEY",
        "GENERIC_IMAGE_API_KEY",
        "HF_API_KEY",
        "HF_TOKEN",
        "HUGGINGFACE_API_KEY",
    ] {
        if let Ok(val) = std::env::var(key) {
            let trimmed = val.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

fn effective_api_key(api_key: &str, config: Option<&Value>) -> String {
    let trimmed = api_key.trim();
    if !trimmed.is_empty() {
        return trimmed.to_string();
    }
    // Try env var
    if let Some(env_key) = env_api_key() {
        return env_key;
    }
    // Try config apiKey field (for hosted env)
    if let Some(cfg) = config {
        if let Some(key) = cfg
            .get("apiKey")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return key.to_string();
        }
        if let Some(key) = cfg
            .get("api_key")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return key.to_string();
        }
    }
    String::new()
}

fn config_string(config: Option<&Value>, keys: &[&str]) -> Option<String> {
    let cfg = config?;
    for key in keys {
        if let Some(val) = cfg
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return Some(val.to_string());
        }
    }
    None
}

fn config_str<'a>(config: Option<&'a Value>, key: &str) -> Option<&'a str> {
    config?
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn normalize_base_url(base_url: &str) -> String {
    base_url.trim_end_matches('/').to_string()
}

fn build_endpoint(base_url: &str, config: Option<&Value>) -> String {
    let base = normalize_base_url(base_url);
    if base.is_empty() {
        return base;
    }
    // If config has endpointPath, append if not already present
    if let Some(path) = config_string(
        config,
        &["endpointPath", "endpoint_path", "apiEndpoint", "path"],
    ) {
        let path = path.trim();
        let path = if path.starts_with('/') {
            path.to_string()
        } else {
            format!("/{}", path)
        };
        if !base.ends_with(&path) && !base.contains(&path) {
            return format!("{}{}", base, path);
        }
    }
    base
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

fn get_negative_prompt(request: &ImageGenerationRequest) -> String {
    request
        .advanced_model_settings
        .as_ref()
        .and_then(|s| s.sd_negative_prompt.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            request
                .scene_context
                .as_ref()
                .and_then(|c| c.negative_prompt.as_deref())
                .map(str::trim)
                .filter(|s| !s.is_empty())
        })
        .unwrap_or("")
        .to_string()
}

fn render_template(template: &str, request: &ImageGenerationRequest) -> String {
    let (width, height) = get_size(request);
    let steps = get_steps(request);
    let cfg = get_cfg(request);
    let seed = get_seed(request);
    let negative = get_negative_prompt(request);
    let n = request.n.unwrap_or(1);
    let images_json = request
        .input_images
        .as_ref()
        .map(|imgs| serde_json::to_string(imgs).unwrap_or("[]".to_string()))
        .unwrap_or("[]".to_string());
    let first_image = request
        .input_images
        .as_ref()
        .and_then(|imgs| imgs.first())
        .map(|s| s.as_str())
        .unwrap_or("");

    let mut out = template.to_string();
    out = out.replace("{{prompt}}", &escape_json_string(&request.prompt));
    out = out.replace("{{negative_prompt}}", &escape_json_string(&negative));
    out = out.replace("{{model}}", &escape_json_string(&request.model));
    out = out.replace("{{width}}", &width.to_string());
    out = out.replace("{{height}}", &height.to_string());
    out = out.replace("{{steps}}", &steps.to_string());
    out = out.replace("{{cfg_scale}}", &cfg.to_string());
    out = out.replace("{{cfg}}", &cfg.to_string());
    out = out.replace("{{seed}}", &seed.to_string());
    out = out.replace("{{n}}", &n.to_string());
    out = out.replace("{{images}}", &images_json);
    out = out.replace("{{image}}", &escape_json_string(first_image));
    // Also support without escaping for raw JSON number insertion (already numbers)
    out
}

fn escape_json_string(s: &str) -> String {
    // For template insertion into JSON string values, we need to escape.
    // But if template already has quotes around {{prompt}}, we should not double-escape.
    // We will just return the raw string and let JSON parsing handle it; for template case,
    // the user is responsible for quoting. So we return raw for now and the template
    // should be valid JSON after replacement. To be safe, we escape quotes and backslashes.
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

fn build_default_payload(request: &ImageGenerationRequest) -> Value {
    let (width, height) = get_size(request);
    let steps = get_steps(request);
    let cfg = get_cfg(request);
    let seed = get_seed(request);
    let negative = get_negative_prompt(request);
    let mut map = serde_json::Map::new();
    map.insert("prompt".to_string(), Value::String(request.prompt.clone()));
    if !negative.is_empty() {
        map.insert("negative_prompt".to_string(), Value::String(negative));
    }
    map.insert("model".to_string(), Value::String(request.model.clone()));
    map.insert("width".to_string(), json!(width));
    map.insert("height".to_string(), json!(height));
    map.insert("steps".to_string(), json!(steps));
    map.insert("cfg_scale".to_string(), json!(cfg));
    // Also support alternative names for compatibility with different providers
    map.insert("cfgScale".to_string(), json!(cfg));
    map.insert("guidance_scale".to_string(), json!(cfg));
    if seed != -1 {
        map.insert("seed".to_string(), json!(seed));
    }
    map.insert("n".to_string(), json!(request.n.unwrap_or(1)));
    // For providers that expect "num_images", "batch_size", etc., also add
    map.insert("num_images".to_string(), json!(request.n.unwrap_or(1)));
    map.insert("batch_size".to_string(), json!(request.n.unwrap_or(1)));

    if let Some(images) = request.input_images.as_ref().filter(|v| !v.is_empty()) {
        // Provide both "images" and "init_images" for compatibility
        let arr: Vec<Value> = images.iter().map(|s| Value::String(s.clone())).collect();
        map.insert("images".to_string(), Value::Array(arr.clone()));
        map.insert("init_images".to_string(), Value::Array(arr));
        if let Some(first) = images.first() {
            map.insert("image".to_string(), Value::String(first.clone()));
            map.insert("init_image".to_string(), Value::String(first.clone()));
        }
    }
    if let Some(mask) = request
        .mask_image
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        map.insert("mask".to_string(), Value::String(mask.to_string()));
        map.insert("mask_image".to_string(), Value::String(mask.to_string()));
    }
    // Also provide alternative field names for HF/gradio compatibility
    map.insert("inputs".to_string(), Value::String(request.prompt.clone()));
    let mut params = serde_json::Map::new();
    params.insert("width".to_string(), json!(width));
    params.insert("height".to_string(), json!(height));
    params.insert("steps".to_string(), json!(steps));
    params.insert("cfg_scale".to_string(), json!(cfg));
    map.insert("parameters".to_string(), Value::Object(params));

    Value::Object(map)
}

fn extract_by_path(value: &Value, path: &str) -> Option<Value> {
    let path = path.trim();
    if path.is_empty() {
        return None;
    }
    // Support simple dot notation and array index like "data[0].image" or "images[0]"
    let mut current = value;
    for segment in path.split('.') {
        let segment = segment.trim();
        if segment.is_empty() {
            continue;
        }
        // Check for array index: "images[0]" or "data[0]"
        if let Some(bracket) = segment.find('[') {
            let key = &segment[..bracket];
            let rest = &segment[bracket..];
            // Get object key first if present
            if !key.is_empty() {
                current = current.get(key)?;
            }
            // Parse indices like [0][1]
            let mut indices_str = rest;
            while let Some(start) = indices_str.find('[') {
                let end = indices_str.find(']')?;
                let idx_str = &indices_str[start + 1..end];
                let idx: usize = idx_str.parse().ok()?;
                current = current.get(idx)?;
                indices_str = &indices_str[end + 1..];
                if !indices_str.is_empty()
                    && !indices_str.starts_with('.')
                    && !indices_str.starts_with('[')
                {
                    return None;
                }
            }
        } else {
            // Simple key or numeric index
            if let Ok(idx) = segment.parse::<usize>() {
                current = current.get(idx)?;
            } else {
                current = current.get(segment)?;
            }
        }
    }
    Some(current.clone())
}

fn collect_images_from_value(value: &Value, out: &mut Vec<ImageResponseData>) {
    match value {
        Value::String(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                return;
            }
            // Heuristic: URL vs base64
            if trimmed.starts_with("http://")
                || trimmed.starts_with("https://")
                || trimmed.starts_with("data:")
            {
                // Check if it's a URL or data URL
                if trimmed.starts_with("http") {
                    out.push(ImageResponseData {
                        url: Some(trimmed.to_string()),
                        b64_json: None,
                        text: None,
                    });
                } else {
                    // data URL – treat as b64_json (strip prefix and keep data)
                    // The storage layer handles data URLs, but we normalize to b64_json for consistency
                    // Keep full data URL as b64_json for now; save_image handles data URLs
                    out.push(ImageResponseData {
                        url: None,
                        b64_json: Some(trimmed.to_string()),
                        text: None,
                    });
                }
            } else if trimmed.len() > 20 {
                // Likely base64 (check if it looks like base64)
                // We'll treat any long string without spaces as base64
                if !trimmed.contains(' ')
                    && trimmed
                        .chars()
                        .all(|c| c.is_alphanumeric() || c == '+' || c == '/' || c == '=')
                {
                    out.push(ImageResponseData {
                        url: None,
                        b64_json: Some(trimmed.to_string()),
                        text: None,
                    });
                } else {
                    // Fallback: treat as b64
                    out.push(ImageResponseData {
                        url: None,
                        b64_json: Some(trimmed.to_string()),
                        text: None,
                    });
                }
            }
        }
        Value::Array(arr) => {
            for item in arr {
                collect_images_from_value(item, out);
            }
        }
        Value::Object(obj) => {
            // If object has image-like fields, prefer those
            // Try common keys in priority order
            let mut found = false;
            for key in [
                "image",
                "images",
                "b64_json",
                "base64",
                "data",
                "output",
                "result",
                "url",
                "urls",
                "generated_image",
                "generated_images",
            ] {
                if let Some(val) = obj.get(key) {
                    let before = out.len();
                    collect_images_from_value(val, out);
                    if out.len() > before {
                        found = true;
                        break;
                    }
                }
            }
            if found {
                return;
            }
            // If object is like {"url": "https://...", "b64_json": "..."} treat as single image
            if obj.contains_key("url") || obj.contains_key("b64_json") || obj.contains_key("base64")
            {
                let url = obj
                    .get("url")
                    .and_then(Value::as_str)
                    .map(|s| s.to_string());
                let b64 = obj
                    .get("b64_json")
                    .or_else(|| obj.get("base64"))
                    .or_else(|| obj.get("image"))
                    .and_then(Value::as_str)
                    .map(|s| s.to_string());
                if url.is_some() || b64.is_some() {
                    out.push(ImageResponseData {
                        url,
                        b64_json: b64,
                        text: None,
                    });
                    return;
                }
            }
            // Fallback: try all values
            for val in obj.values() {
                collect_images_from_value(val, out);
            }
        }
        _ => {}
    }
}

// =========================================================
// Impl
// =========================================================

impl ImageProviderAdapter for GenericHttpAdapter {
    fn endpoint(&self, base_url: &str, _request: &ImageGenerationRequest) -> String {
        build_endpoint(base_url, None)
    }

    fn endpoint_with_config(
        &self,
        base_url: &str,
        _request: &ImageGenerationRequest,
        config: Option<&Value>,
    ) -> String {
        build_endpoint(base_url, config)
    }

    fn required_auth_headers(&self) -> &'static [&'static str] {
        &[]
    }

    fn requires_api_key(&self) -> bool {
        false
    }

    fn headers(
        &self,
        _api_key: &str,
        extra: Option<&HashMap<String, String>>,
    ) -> HashMap<String, String> {
        let mut headers = HashMap::new();
        headers.insert("Content-Type".to_string(), "application/json".to_string());
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
        let mut headers = HashMap::new();
        headers.insert("Content-Type".to_string(), "application/json".to_string());
        headers.insert("Accept".to_string(), "application/json".to_string());

        // Determine auth type
        let auth_type = config_str(config, "authType")
            .or_else(|| config_str(config, "auth_type"))
            .unwrap_or("none")
            .to_ascii_lowercase();

        let effective_key = effective_api_key(api_key, config);

        match auth_type.as_str() {
            "bearer" | "bearer_token" | "token" => {
                if !effective_key.is_empty() {
                    headers.insert(
                        "Authorization".to_string(),
                        format!("Bearer {}", effective_key),
                    );
                }
            }
            "apikey" | "api_key" | "api-key" | "x-api-key" => {
                let header_name = config_string(
                    config,
                    &["authHeaderName", "auth_header_name", "apiKeyHeader"],
                )
                .unwrap_or_else(|| "x-api-key".to_string());
                if !effective_key.is_empty() {
                    headers.insert(header_name, effective_key);
                }
            }
            "custom" | "custom_header" => {
                if let Some(custom) = config
                    .and_then(|c| c.get("customHeaders"))
                    .and_then(Value::as_object)
                {
                    for (k, v) in custom {
                        if let Some(val_str) = v.as_str() {
                            headers.insert(k.clone(), val_str.to_string());
                        }
                    }
                }
                if let Some(header_name) =
                    config_string(config, &["authHeaderName", "customHeaderName"])
                {
                    if !effective_key.is_empty() && !headers.contains_key(&header_name) {
                        headers.insert(header_name, effective_key);
                    }
                }
            }
            "header" => {
                // Generic header auth (like custom)
                let header_name = config_string(config, &["authHeaderName", "headerName"])
                    .unwrap_or_else(|| "Authorization".to_string());
                if !effective_key.is_empty() {
                    // If header is Authorization and value doesn't already have Bearer, add Bearer
                    let value = if header_name.eq_ignore_ascii_case("authorization")
                        && !effective_key.to_ascii_lowercase().starts_with("bearer ")
                    {
                        format!("Bearer {}", effective_key)
                    } else {
                        effective_key.clone()
                    };
                    headers.insert(header_name, value);
                }
            }
            _ => {
                // "none" or unknown: no auth, but if api_key provided and no explicit authType, try bearer
                if !effective_key.is_empty() {
                    // Check if config explicitly says none
                    let explicit_none = config_str(config, "authType")
                        .map(|v| v.eq_ignore_ascii_case("none"))
                        .unwrap_or(false);
                    if !explicit_none {
                        headers.insert(
                            "Authorization".to_string(),
                            format!("Bearer {}", effective_key),
                        );
                    }
                }
            }
        }

        // Also handle config.headers object for additional static headers
        if let Some(cfg_headers) = config
            .and_then(|c| c.get("headers"))
            .and_then(Value::as_object)
        {
            for (k, v) in cfg_headers {
                if let Some(val_str) = v.as_str() {
                    headers.insert(k.clone(), val_str.to_string());
                }
            }
        }
        if let Some(extra) = extra {
            for (k, v) in extra.iter() {
                headers.insert(k.clone(), v.clone());
            }
        }
        headers
    }

    fn payload(&self, request: &ImageGenerationRequest) -> Result<ImageRequestPayload, String> {
        Ok(ImageRequestPayload::Json(build_default_payload(request)))
    }

    fn payload_with_config(
        &self,
        request: &ImageGenerationRequest,
        config: Option<&Value>,
    ) -> Result<ImageRequestPayload, String> {
        // If config has requestTemplate, use it
        if let Some(template) =
            config_str(config, "requestTemplate").or_else(|| config_str(config, "request_template"))
        {
            let rendered = render_template(template, request);
            // Try to parse as JSON
            match serde_json::from_str::<Value>(&rendered) {
                Ok(val) => return Ok(ImageRequestPayload::Json(val)),
                Err(e) => {
                    // If template is not JSON, treat as raw prompt? But we expect JSON.
                    // Fallback: try to see if it's a JSON object string without outer braces?
                    // For now, error with helpful message
                    return Err(format!("Failed to parse requestTemplate as JSON after rendering: {} (rendered: {})", e, rendered));
                }
            }
        }
        // Check for requestFields mapping (advanced)
        // For now, use default
        Ok(ImageRequestPayload::Json(build_default_payload(request)))
    }

    fn parse_response(&self, response: Value) -> Result<Vec<ImageResponseData>, String> {
        self.parse_response_with_config(response, None)
    }

    fn parse_response_with_config(
        &self,
        response: Value,
        config: Option<&Value>,
    ) -> Result<Vec<ImageResponseData>, String> {
        // If config specifies responseImageField, try that path first
        if let Some(field) = config_string(
            config,
            &[
                "responseImageField",
                "responseField",
                "imageField",
                "outputField",
            ],
        ) {
            if let Some(extracted) = extract_by_path(&response, &field) {
                let mut out = Vec::new();
                collect_images_from_value(&extracted, &mut out);
                if !out.is_empty() {
                    return Ok(out);
                }
            }
            // Also try as direct field
            if let Some(val) = response.get(&field) {
                let mut out = Vec::new();
                collect_images_from_value(val, &mut out);
                if !out.is_empty() {
                    return Ok(out);
                }
            }
        }

        // Try common top-level fields in priority order
        let mut out = Vec::new();

        // First, check if response itself is a string (direct base64 or url)
        if let Value::String(s) = &response {
            collect_images_from_value(&Value::String(s.clone()), &mut out);
            if !out.is_empty() {
                return Ok(out);
            }
        }

        // Check for error field first
        if let Some(err) = response
            .get("error")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return Err(format!("Provider error: {}", err));
        }
        if let Some(msg) = response
            .get("message")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            // Some providers return {"message": "error"} on failure, but also on success they may have message.
            // We will treat it as error only if no image found later.
        }

        // Try to collect from known fields
        for key in [
            "images",
            "image",
            "b64_json",
            "base64",
            "data",
            "output",
            "result",
            "url",
            "urls",
            "generated_images",
            "artifacts",
        ] {
            if let Some(val) = response.get(key) {
                collect_images_from_value(val, &mut out);
                if !out.is_empty() {
                    return Ok(out);
                }
            }
        }

        // Special handling for Gradio-like {"data": [["base64"]]} or {"data": ["url"]}
        if let Some(data) = response.get("data") {
            collect_images_from_value(data, &mut out);
            if !out.is_empty() {
                return Ok(out);
            }
        }

        // If response is array, treat each element as image
        if let Value::Array(arr) = &response {
            for item in arr {
                collect_images_from_value(item, &mut out);
            }
            if !out.is_empty() {
                return Ok(out);
            }
        }

        // Fallback: try to find any image-like string in the entire response
        collect_images_from_value(&response, &mut out);
        if !out.is_empty() {
            return Ok(out);
        }

        Err(format!("Generic response did not contain any image data (response keys: {}). Provide responseImageField in provider config if the image is under a custom field. Full response: {}", 
            response.as_object().map(|o| o.keys().cloned().collect::<Vec<_>>().join(", ")).unwrap_or_else(|| "non-object".to_string()),
            truncate_response(&response)
        ))
    }

    fn response_format(&self) -> ImageResponseFormat {
        ImageResponseFormat::Json
    }

    fn response_format_with_config(&self, config: Option<&Value>) -> ImageResponseFormat {
        let fmt = config_str(config, "responseFormat")
            .or_else(|| config_str(config, "response_format"))
            .unwrap_or("json")
            .to_ascii_lowercase();
        match fmt.as_str() {
            "binary" | "image" | "bytes" => ImageResponseFormat::Binary,
            _ => ImageResponseFormat::Json,
        }
    }

    fn timeout(&self, config: Option<&Value>) -> Duration {
        let secs = config
            .and_then(|c| c.get("timeoutSeconds"))
            .and_then(|v| v.as_u64())
            .or_else(|| {
                config
                    .and_then(|c| c.get("timeout_seconds"))
                    .and_then(|v| v.as_u64())
            })
            .or_else(|| {
                config
                    .and_then(|c| c.get("timeout"))
                    .and_then(|v| v.as_u64())
            })
            .unwrap_or(120);
        Duration::from_secs(secs.clamp(5, 600))
    }
}

fn truncate_response(value: &Value) -> String {
    let s = value.to_string();
    if s.len() > 800 {
        format!("{}... (truncated, {} chars)", &s[..800], s.len())
    } else {
        s
    }
}

// =========================================================
// Tests
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image_generator::types::{ImageCharacterContext, ImageGenerationRequest};
    use serde_json::json;

    fn base_request(prompt: &str, model: &str) -> ImageGenerationRequest {
        ImageGenerationRequest {
            prompt: prompt.to_string(),
            model: model.to_string(),
            provider_id: "generic_http".to_string(),
            credential_id: "test-cred".to_string(),
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
    fn generic_json_request_contains_prompt_and_size() {
        let req = base_request("a cat", "DreamShaper_8");
        let adapter = GenericHttpAdapter;
        let payload = adapter.payload(&req).unwrap();
        match payload {
            ImageRequestPayload::Json(val) => {
                assert_eq!(val.get("prompt").and_then(Value::as_str), Some("a cat"));
                assert_eq!(
                    val.get("model").and_then(Value::as_str),
                    Some("DreamShaper_8")
                );
                assert_eq!(val.get("width").and_then(Value::as_u64), Some(512));
                assert_eq!(val.get("height").and_then(Value::as_u64), Some(512));
            }
            _ => panic!("expected json"),
        }
    }

    #[test]
    fn generic_handles_template() {
        let req = base_request("hello", "Flux");
        let adapter = GenericHttpAdapter;
        let config =
            json!({"requestTemplate": "{\"inputs\": \"{{prompt}}\", \"model\": \"{{model}}\"}"});
        let payload = adapter.payload_with_config(&req, Some(&config)).unwrap();
        match payload {
            ImageRequestPayload::Json(val) => {
                assert_eq!(val.get("inputs").and_then(Value::as_str), Some("hello"));
                assert_eq!(val.get("model").and_then(Value::as_str), Some("Flux"));
            }
            _ => panic!("expected json"),
        }
    }

    #[test]
    fn generic_parses_base64_response() {
        let adapter = GenericHttpAdapter;
        let resp = json!({"image": "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+ip1sAAAAASUVORK5CYII="});
        let images = adapter.parse_response(resp).unwrap();
        assert_eq!(images.len(), 1);
        assert!(images[0].b64_json.is_some());
        assert!(images[0].url.is_none());
    }

    #[test]
    fn generic_parses_url_response() {
        let adapter = GenericHttpAdapter;
        let resp = json!({"url": "https://example.com/image.png"});
        let images = adapter.parse_response(resp).unwrap();
        assert_eq!(images.len(), 1);
        assert_eq!(
            images[0].url.as_deref(),
            Some("https://example.com/image.png")
        );
    }

    #[test]
    fn generic_parses_array_of_images() {
        let adapter = GenericHttpAdapter;
        let resp = json!({"images": ["base64_1", "base64_2"]});
        let images = adapter.parse_response(resp).unwrap();
        assert_eq!(images.len(), 2);
    }

    #[test]
    fn generic_uses_custom_response_field() {
        let adapter = GenericHttpAdapter;
        let resp = json!({"output": {"my_image": "base64_custom"}});
        let config = json!({"responseImageField": "output.my_image"});
        let images = adapter
            .parse_response_with_config(resp, Some(&config))
            .unwrap();
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].b64_json.as_deref(), Some("base64_custom"));
    }

    #[test]
    fn binary_response_format_respected() {
        let adapter = GenericHttpAdapter;
        let config = json!({"responseFormat": "binary"});
        assert_eq!(
            adapter.response_format_with_config(Some(&config)),
            ImageResponseFormat::Binary
        );
        assert_eq!(adapter.response_format(), ImageResponseFormat::Json);
    }

    #[test]
    fn authentication_bearer() {
        let adapter = GenericHttpAdapter;
        let config = json!({"authType": "bearer"});
        let headers = adapter.headers_with_config("my-token", None, Some(&config));
        assert_eq!(
            headers.get("Authorization").map(|s| s.as_str()),
            Some("Bearer my-token")
        );
    }

    #[test]
    fn authentication_api_key() {
        let adapter = GenericHttpAdapter;
        let config = json!({"authType": "apiKey", "authHeaderName": "x-api-key"});
        let headers = adapter.headers_with_config("secret123", None, Some(&config));
        assert_eq!(
            headers.get("x-api-key").map(|s| s.as_str()),
            Some("secret123")
        );
    }

    #[test]
    fn authentication_none_no_header() {
        let adapter = GenericHttpAdapter;
        let config = json!({"authType": "none"});
        let headers = adapter.headers_with_config("should-not-appear", None, Some(&config));
        assert!(!headers.contains_key("Authorization"));
        assert!(!headers.contains_key("x-api-key"));
    }

    #[test]
    fn timeout_config_respected() {
        let adapter = GenericHttpAdapter;
        let config = json!({"timeoutSeconds": 30});
        assert_eq!(adapter.timeout(Some(&config)), Duration::from_secs(30));
        assert_eq!(adapter.timeout(None), Duration::from_secs(120));
    }

    #[test]
    fn http_error_handled_as_invalid_response() {
        let adapter = GenericHttpAdapter;
        let resp = json!({"error": "invalid prompt"});
        let err = adapter.parse_response(resp).unwrap_err();
        assert!(err.contains("Provider error") || err.contains("invalid"));
    }

    #[test]
    fn invalid_response_no_image_field_errors() {
        let adapter = GenericHttpAdapter;
        let resp = json!({"status": "ok", "message": "no image here"});
        let err = adapter.parse_response(resp).unwrap_err();
        assert!(err.contains("did not contain any image data"));
    }

    #[test]
    fn provider_switching_different_payloads() {
        let generic = GenericHttpAdapter;
        let openai = crate::image_generator::provider_adapter::openai::OpenAIAdapter;
        let req = base_request("test", "DreamShaper_8");
        let generic_payload = generic.payload(&req).unwrap();
        let openai_payload = openai.payload(&req).unwrap();
        // They should be different structures but both contain prompt
        match generic_payload {
            ImageRequestPayload::Json(v) => assert!(v.get("prompt").is_some()),
            _ => panic!(),
        }
        match openai_payload {
            ImageRequestPayload::Json(v) => assert!(v.get("prompt").is_some()),
            _ => panic!(),
        }
    }

    #[test]
    fn model_switching_reflected_in_payload() {
        let adapter = GenericHttpAdapter;
        let req1 = base_request("cat", "DreamShaper_8");
        let req2 = base_request("cat", "Flux");
        let p1 = match adapter.payload(&req1).unwrap() {
            ImageRequestPayload::Json(v) => v,
            _ => panic!(),
        };
        let p2 = match adapter.payload(&req2).unwrap() {
            ImageRequestPayload::Json(v) => v,
            _ => panic!(),
        };
        assert_eq!(
            p1.get("model").and_then(Value::as_str),
            Some("DreamShaper_8")
        );
        assert_eq!(p2.get("model").and_then(Value::as_str), Some("Flux"));
    }

    #[test]
    fn endpoint_building_respects_base_url() {
        let adapter = GenericHttpAdapter;
        let req = base_request("test", "model");
        assert_eq!(
            adapter.endpoint("https://example.com/api/generate", &req),
            "https://example.com/api/generate"
        );
        assert_eq!(
            adapter.endpoint_with_config(
                "https://example.com",
                &req,
                Some(&json!({"endpointPath": "/generate"}))
            ),
            "https://example.com/generate"
        );
    }
}
