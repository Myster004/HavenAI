use reqwest::multipart::Form;
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;

use super::types::ImageGenerationRequest;

// =========================================================
// IMAGE PROVIDER MODULES
// =========================================================

pub mod aihorde;
pub mod automatic1111;
pub mod diffusers;
pub mod generic_http;
pub mod google_gemini;
pub mod gradio;
pub mod literouter;
pub mod nanogpt;
pub mod openai;
pub mod openrouter;
pub mod pollinations;
pub mod stability;
pub mod xai;

// =========================================================
// REQUEST PAYLOAD
// =========================================================

/// Provider request body.
///
/// Most providers use JSON, while providers such as
/// OpenAI/Gemini/Stability may require multipart/form-data.
pub enum ImageRequestPayload {
    Json(Value),
    Multipart(Form),
}

// =========================================================
// RESPONSE FORMAT
// =========================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageResponseFormat {
    Json,
    Binary,
}

// =========================================================
// IMAGE PROVIDER ADAPTER
// =========================================================

/// Common interface implemented by every image provider.
///
/// The image-generation manager is responsible for:
///
/// 1. Selecting the provider.
/// 2. Obtaining credentials.
/// 3. Building headers.
/// 4. Building the request payload.
/// 5. Sending the request.
/// 6. Parsing the provider response.
///
/// Providers may implement synchronous or asynchronous
/// generation internally.
///
/// AI Horde is asynchronous and returns:
///
///     aihorde://<job-id>
///
/// which the image-generation manager can resolve through
/// the Horde polling implementation.
pub trait ImageProviderAdapter: Send + Sync {
    // =====================================================
    // ENDPOINT
    // =====================================================

    /// Build the provider endpoint.
    fn endpoint(&self, base_url: &str, request: &ImageGenerationRequest) -> String;

    /// Build endpoint with provider config (for generic providers).
    fn endpoint_with_config(
        &self,
        base_url: &str,
        request: &ImageGenerationRequest,
        config: Option<&Value>,
    ) -> String {
        let _ = config;
        self.endpoint(base_url, request)
    }

    // =====================================================
    // AUTHENTICATION
    // =====================================================

    /// Whether this provider requires an API key.
    ///
    /// The default implementation derives this from the
    /// authentication headers declared by the provider.
    fn requires_api_key(&self) -> bool {
        !self.required_auth_headers().is_empty()
    }

    /// Headers required for authentication.
    ///
    /// Examples:
    ///
    /// - OpenAI: Authorization
    /// - Stability: Authorization
    /// - AI Horde: no required authentication
    #[allow(dead_code)]
    fn required_auth_headers(&self) -> &'static [&'static str];

    /// Build provider request headers.
    ///
    /// `api_key` may be empty for providers that support
    /// anonymous requests.
    fn headers(
        &self,
        api_key: &str,
        extra: Option<&HashMap<String, String>>,
    ) -> HashMap<String, String>;

    /// Build headers with provider config.
    fn headers_with_config(
        &self,
        api_key: &str,
        extra: Option<&HashMap<String, String>>,
        config: Option<&Value>,
    ) -> HashMap<String, String> {
        let _ = config;
        self.headers(api_key, extra)
    }

    // =====================================================
    // PAYLOAD
    // =====================================================

    /// Construct the provider request body.
    fn payload(&self, request: &ImageGenerationRequest) -> Result<ImageRequestPayload, String>;

    /// Construct payload with provider config.
    fn payload_with_config(
        &self,
        request: &ImageGenerationRequest,
        config: Option<&Value>,
    ) -> Result<ImageRequestPayload, String> {
        let _ = config;
        self.payload(request)
    }

    // =====================================================
    // RESPONSE
    // =====================================================

    /// Parse the provider response into the common image
    /// response representation.
    fn parse_response(&self, response: Value) -> Result<Vec<ImageResponseData>, String>;

    /// Parse response with provider config.
    fn parse_response_with_config(
        &self,
        response: Value,
        config: Option<&Value>,
    ) -> Result<Vec<ImageResponseData>, String> {
        let _ = config;
        self.parse_response(response)
    }

    /// Determine whether the provider response is JSON or
    /// binary.
    fn response_format(&self) -> ImageResponseFormat {
        ImageResponseFormat::Json
    }

    /// Response format with config.
    fn response_format_with_config(&self, config: Option<&Value>) -> ImageResponseFormat {
        let _ = config;
        self.response_format()
    }

    // =====================================================
    // OPTIONAL CAPABILITIES
    // =====================================================

    /// Timeout for image generation request.
    fn timeout(&self, config: Option<&Value>) -> Duration {
        let secs = config
            .and_then(|c| c.get("timeoutSeconds"))
            .and_then(|v| v.as_u64())
            .or_else(|| {
                config
                    .and_then(|c| c.get("timeout_seconds"))
                    .and_then(|v| v.as_u64())
            })
            .unwrap_or(120);
        Duration::from_secs(secs.clamp(5, 600))
    }

    /// Whether this provider supports streaming.
    #[allow(dead_code)]
    fn supports_stream(&self) -> bool {
        false
    }

    /// Whether this provider supports model discovery.
    fn supports_model_discovery(&self) -> bool {
        false
    }
}

// =========================================================
// IMAGE RESPONSE
// =========================================================

/// Normalized image response returned by a provider adapter.
///
/// Providers may return:
///
/// - a remote URL
/// - base64 image data
/// - text
///
/// AI Horde initially returns an internal:
///
///     aihorde://<job-id>
///
/// URL. The image-generation layer can then poll Horde and
/// replace it with the final image URL/base64 data.
#[derive(Debug, Clone)]
pub struct ImageResponseData {
    pub url: Option<String>,
    pub b64_json: Option<String>,
    pub text: Option<String>,
}

// =========================================================
// IMAGE SIZE PARSER
// =========================================================

/// Parse a generic `WIDTHxHEIGHT` image size.
///
/// Examples:
///
///     512x512
///     768x512
///     1024x1024
///
/// Invalid sizes fall back to the supplied defaults.
pub fn parse_size_dimensions(
    size: Option<&str>,
    default_width: u32,
    default_height: u32,
) -> (u32, u32) {
    let Some(size) = size else {
        return (default_width, default_height);
    };

    let size = size.trim();

    let Some((width, height)) = size.split_once('x') else {
        return (default_width, default_height);
    };

    let width = width.trim().parse::<u32>().ok().filter(|value| *value > 0);

    let height = height.trim().parse::<u32>().ok().filter(|value| *value > 0);

    match (width, height) {
        (Some(width), Some(height)) => (width, height),

        _ => (default_width, default_height),
    }
}

// =========================================================
// PROVIDER ID NORMALIZATION
// =========================================================

/// Normalize provider aliases.
///
/// This keeps the rest of the application from needing to
/// know about historical aliases such as `ai-horde`.
pub fn normalize_provider_id(provider_id: &str) -> String {
    match provider_id.trim().to_ascii_lowercase().as_str() {
        "ai-horde" => "aihorde".to_string(),

        "ai_horde" => "aihorde".to_string(),

        "google-gemini" => "gemini".to_string(),

        "google_gemini" => "gemini".to_string(),

        "gemini-agent-platform" => "gemini-agent-platform-express".to_string(),

        "automatic-1111" => "automatic1111".to_string(),

        "automatic_1111" => "automatic1111".to_string(),

        // Generic HTTP aliases
        "generic" | "generic-http" | "generic_http" | "http" | "https" | "custom-http"
        | "custom_http" => "generic_http".to_string(),

        // Gradio / Hugging Face aliases
        "gradio" | "huggingface" | "hugging_face" | "hf" | "hf-space" | "huggingface-space"
        | "hugging_face_space" | "gradio-space" => "gradio".to_string(),

        // Hugging Face Docker (generic http) aliases
        "docker" | "docker-space" | "hf-docker" => "generic_http".to_string(),

        other => other.to_string(),
    }
}

// =========================================================
// PROVIDER ADAPTER FACTORY
// =========================================================

/// Create the image provider adapter for a provider ID.
///
/// This is the central registration point for image
/// providers.
///
/// AI Horde is registered here as:
///
///     aihorde
///
/// and accepts the aliases:
///
///     ai-horde
///     ai_horde
pub fn get_adapter(provider_id: &str) -> Result<Box<dyn ImageProviderAdapter>, String> {
    let provider_id = normalize_provider_id(provider_id);

    match provider_id.as_str() {
        // =================================================
        // AI HORDE
        // =================================================
        "aihorde" => Ok(Box::new(aihorde::AIHordeAdapter)),

        // =================================================
        // AUTOMATIC1111
        // =================================================
        "automatic1111" => Ok(Box::new(automatic1111::Automatic1111Adapter)),

        // =================================================
        // DIFFUSERS
        // =================================================
        "diffusers" => Ok(Box::new(diffusers::DiffusersAdapter)),

        // =================================================
        // OPENAI
        // =================================================
        "openai" => Ok(Box::new(openai::OpenAIAdapter)),

        // =================================================
        // OPENROUTER
        // =================================================
        "openrouter" => Ok(Box::new(openrouter::OpenRouterAdapter)),

        // =================================================
        // POLLINATIONS
        // =================================================
        "pollinations" => Ok(Box::new(pollinations::PollinationsAdapter)),

        // =================================================
        // GOOGLE GEMINI
        // =================================================
        "gemini" => Ok(Box::new(google_gemini::GoogleGeminiAdapter)),

        // =================================================
        // GOOGLE GEMINI AGENT PLATFORM
        // =================================================
        "gemini-agent-platform-express" => {
            Ok(Box::new(google_gemini::GeminiAgentPlatformExpressAdapter))
        }

        // =================================================
        // STABILITY
        // =================================================
        "stability" => Ok(Box::new(stability::StabilityAdapter)),

        // =================================================
        // XAI
        // =================================================
        "xai" => Ok(Box::new(xai::XAIAdapter)),

        // =================================================
        // NANOGPT
        // =================================================
        "nanogpt" => Ok(Box::new(nanogpt::NanoGPTAdapter)),

        // =================================================
        // LITEROUTER
        // =================================================
        "literouter" => Ok(Box::new(literouter::LiteRouterAdapter)),

        // =================================================
        // CUSTOM / LETTUCE HOST
        // =================================================
        "custom" | "lettuce-host" => Ok(Box::new(openai::OpenAIAdapter)),

        // =================================================
        // GENERIC HTTP (hosting-agnostic)
        // =================================================
        "generic_http" => Ok(Box::new(generic_http::GenericHttpAdapter)),

        // =================================================
        // GRADIO / HUGGING FACE
        // =================================================
        "gradio" => Ok(Box::new(gradio::GradioAdapter)),

        // =================================================
        // UNKNOWN PROVIDER
        // =================================================
        _ => Err(format!("Image provider '{}' is not supported", provider_id)),
    }
}

// =========================================================
// PROVIDER CAPABILITIES
// =========================================================

/// Basic provider metadata used by the UI/model selector.
///
/// This is intentionally separate from the actual adapter so
/// the frontend can display provider choices without creating
/// an HTTP client.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageProviderInfo {
    pub id: &'static str,
    pub name: &'static str,
    pub supports_reference_images: bool,
    pub supports_async: bool,
}

/// Return providers supported by the image-generation system.
///
/// AI Horde is explicitly marked as supporting reference
/// images and asynchronous generation.
pub fn available_providers() -> &'static [ImageProviderInfo] {
    &[
        ImageProviderInfo {
            id: "aihorde",
            name: "AI Horde",
            supports_reference_images: true,
            supports_async: true,
        },
        ImageProviderInfo {
            id: "automatic1111",
            name: "Automatic1111",
            supports_reference_images: true,
            supports_async: false,
        },
        ImageProviderInfo {
            id: "diffusers",
            name: "Diffusers",
            supports_reference_images: true,
            supports_async: false,
        },
        ImageProviderInfo {
            id: "openai",
            name: "OpenAI",
            supports_reference_images: true,
            supports_async: false,
        },
        ImageProviderInfo {
            id: "openrouter",
            name: "OpenRouter",
            supports_reference_images: false,
            supports_async: false,
        },
        ImageProviderInfo {
            id: "pollinations",
            name: "Pollinations",
            supports_reference_images: false,
            supports_async: false,
        },
        ImageProviderInfo {
            id: "gemini",
            name: "Google Gemini",
            supports_reference_images: true,
            supports_async: false,
        },
        ImageProviderInfo {
            id: "stability",
            name: "Stability AI",
            supports_reference_images: true,
            supports_async: false,
        },
        ImageProviderInfo {
            id: "xai",
            name: "xAI",
            supports_reference_images: false,
            supports_async: false,
        },
        ImageProviderInfo {
            id: "nanogpt",
            name: "NanoGPT",
            supports_reference_images: false,
            supports_async: false,
        },
        ImageProviderInfo {
            id: "literouter",
            name: "LiteRouter",
            supports_reference_images: false,
            supports_async: false,
        },
        ImageProviderInfo {
            id: "custom",
            name: "Custom / Lettuce Host",
            supports_reference_images: true,
            supports_async: false,
        },
        ImageProviderInfo {
            id: "generic_http",
            name: "Generic HTTP",
            supports_reference_images: true,
            supports_async: false,
        },
        ImageProviderInfo {
            id: "gradio",
            name: "Gradio / Hugging Face",
            supports_reference_images: true,
            supports_async: true,
        },
    ]
}

// =========================================================
// TESTS
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_size() {
        assert_eq!(parse_size_dimensions(Some("768x512"), 512, 512), (768, 512));
    }

    #[test]
    fn invalid_size_uses_default() {
        assert_eq!(parse_size_dimensions(Some("invalid"), 512, 512), (512, 512));
    }

    #[test]
    fn zero_size_uses_default() {
        assert_eq!(parse_size_dimensions(Some("0x512"), 512, 512), (512, 512));
    }

    #[test]
    fn normalizes_ai_horde_alias() {
        assert_eq!(normalize_provider_id("ai-horde"), "aihorde");

        assert_eq!(normalize_provider_id("AI_HORDE"), "aihorde");

        assert_eq!(normalize_provider_id("aihorde"), "aihorde");
    }

    #[test]
    fn normalizes_gemini_alias() {
        assert_eq!(normalize_provider_id("google-gemini"), "gemini");
    }

    #[test]
    fn aihorde_adapter_is_registered() {
        let adapter = get_adapter("aihorde");

        assert!(adapter.is_ok());
    }

    #[test]
    fn aihorde_alias_is_registered() {
        let adapter = get_adapter("ai-horde");

        assert!(adapter.is_ok());
    }

    #[test]
    fn unknown_provider_returns_error() {
        let result = get_adapter("does-not-exist");

        assert!(result.is_err());
    }

    #[test]
    fn provider_list_contains_ai_horde() {
        let providers = available_providers();

        assert!(providers
            .iter()
            .any(|provider| { provider.id == "aihorde" }));
    }
}
