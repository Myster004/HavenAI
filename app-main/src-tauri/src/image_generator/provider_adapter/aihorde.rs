use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;

use super::{parse_size_dimensions, ImageProviderAdapter, ImageRequestPayload, ImageResponseData};

use crate::image_generator::types::{
    ImageCharacterContext, ImageGenerationRequest, ImageSceneCharacter,
};

/* =========================================================
AI HORDE MODEL INFORMATION
========================================================= */

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AIHordeModel {
    pub name: String,

    #[serde(default)]
    pub count: Option<u64>,

    #[serde(default)]
    pub queued: Option<u64>,

    #[serde(default)]
    pub jobs: Option<u64>,

    #[serde(default)]
    pub performance: Option<f64>,

    #[serde(default)]
    pub baseline: Option<String>,

    #[serde(default)]
    pub type_: Option<String>,

    #[serde(default)]
    pub max_pixels: Option<u64>,

    #[serde(default)]
    pub min_bridge_version: Option<String>,

    #[serde(skip)]
    pub raw: Option<Value>,
}

impl AIHordeModel {
    pub fn display_name(&self) -> &str {
        &self.name
    }

    pub fn is_available(&self) -> bool {
        self.count.unwrap_or(0) > 0
    }
}

/* =========================================================
AI HORDE MODEL FETCH RESPONSE
========================================================= */

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum AIHordeModelsResponse {
    List(Vec<Value>),

    Wrapped {
        #[serde(default)]
        models: Vec<Value>,
    },
}

/* =========================================================
AI HORDE PROVIDER
========================================================= */

pub struct AIHordeAdapter;

impl AIHordeAdapter {
    pub const DEFAULT_BASE_URL: &'static str = "https://aihorde.net/api";

    /*
     * Public anonymous AI Horde key.
     *
     * A user-supplied key is preferred whenever available.
     */
    pub const ANONYMOUS_API_KEY: &'static str = "0000000000";

    /* =========================================================
    BASIC HELPERS
    ========================================================= */

    fn clean_api_key(api_key: &str) -> Option<String> {
        let key = api_key.trim();

        if key.is_empty() {
            None
        } else {
            Some(key.to_string())
        }
    }

    fn effective_api_key(api_key: &str) -> String {
        Self::clean_api_key(api_key).unwrap_or_else(|| Self::ANONYMOUS_API_KEY.to_string())
    }

    fn positive_u32(value: Option<u32>, default: u32) -> u32 {
        value.filter(|value| *value > 0).unwrap_or(default)
    }

    fn clamp_f64(value: f64, min: f64, max: f64) -> f64 {
        value.clamp(min, max)
    }

    fn normalize_base_url(base_url: &str) -> String {
        base_url.trim_end_matches('/').to_string()
    }

    /* =========================================================
    LIVE MODEL API
    ========================================================= */

    pub fn models_endpoint(base_url: &str) -> String {
        format!("{}/v2/status/models", Self::normalize_base_url(base_url))
    }

    pub async fn fetch_models(
        client: &Client,
        base_url: &str,
        api_key: &str,
    ) -> Result<Vec<AIHordeModel>, String> {
        let url = Self::models_endpoint(base_url);

        let effective_key = Self::effective_api_key(api_key);

        let response = client
            .get(&url)
            .header("Accept", "application/json")
            .header("apikey", effective_key)
            .send()
            .await
            .map_err(|error| format!("AI Horde model request failed: {}", error))?;

        let status = response.status();

        let body = response
            .text()
            .await
            .map_err(|error| format!("Failed to read AI Horde model response: {}", error))?;

        if !status.is_success() {
            return Err(format!(
                "AI Horde model request returned {}: {}",
                status, body
            ));
        }

        let parsed: AIHordeModelsResponse = serde_json::from_str(&body)
            .map_err(|error| format!("Failed to parse AI Horde models: {}", error))?;

        let values = match parsed {
            AIHordeModelsResponse::List(values) => values,

            AIHordeModelsResponse::Wrapped { models } => models,
        };

        let mut models = Vec::new();

        for value in values {
            let name = value
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty());

            let Some(name) = name else {
                continue;
            };

            let model = AIHordeModel {
                name: name.to_string(),

                count: value.get("count").and_then(Value::as_u64),

                queued: value.get("queued").and_then(Value::as_u64),

                jobs: value.get("jobs").and_then(Value::as_u64),

                performance: value.get("performance").and_then(Value::as_f64),

                baseline: value
                    .get("baseline")
                    .and_then(Value::as_str)
                    .map(str::to_string),

                type_: value
                    .get("type")
                    .and_then(Value::as_str)
                    .map(str::to_string),

                max_pixels: value.get("max_pixels").and_then(Value::as_u64),

                min_bridge_version: value
                    .get("min_bridge_version")
                    .and_then(Value::as_str)
                    .map(str::to_string),

                raw: Some(value),
            };

            models.push(model);
        }

        models.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

        Ok(models)
    }

    pub async fn fetch_models_default(
        base_url: Option<&str>,
        api_key: &str,
    ) -> Result<Vec<AIHordeModel>, String> {
        let client = Client::new();

        Self::fetch_models(&client, base_url.unwrap_or(Self::DEFAULT_BASE_URL), api_key).await
    }

    /* =========================================================
    IMAGE VALIDATION
    ========================================================= */

    fn is_valid_reference_image(image: &str) -> bool {
        let image = image.trim();

        if image.is_empty() {
            return false;
        }

        image.starts_with("http://")
            || image.starts_with("https://")
            || image.starts_with("data:image/")
            || image.starts_with("iVBOR")
            || image.starts_with("/9j/")
            || image.starts_with("UklGR")
            || image.starts_with("R0lGOD")
            || image.starts_with("Qk")
    }

    /* =========================================================
    CHARACTER REFERENCE IMAGE
    ========================================================= */

    fn reference_image(request: &ImageGenerationRequest) -> Option<&str> {
        /*
         * Priority:
         *
         * 1. Explicit character_reference_image
         * 2. character_context.reference_image
         * 3. character_context.reference_images
         * 4. character_reference_images
         * 5. generic input_images
         */

        if let Some(image) = request.character_reference_image.as_deref() {
            if Self::is_valid_reference_image(image) {
                return Some(image);
            }
        }

        if let Some(character) = request.character_context.as_ref() {
            if let Some(image) = character.reference_image.as_deref() {
                if Self::is_valid_reference_image(image) {
                    return Some(image);
                }
            }

            if let Some(image) = character
                .reference_images
                .iter()
                .map(String::as_str)
                .find(|image| Self::is_valid_reference_image(image))
            {
                return Some(image);
            }
        }

        if let Some(image) = request
            .character_reference_images
            .iter()
            .map(String::as_str)
            .find(|image| Self::is_valid_reference_image(image))
        {
            return Some(image);
        }

        request.input_images.as_ref().and_then(|images| {
            images
                .iter()
                .map(String::as_str)
                .find(|image| Self::is_valid_reference_image(image))
        })
    }

    /* =========================================================
    PROMPT HELPERS
    ========================================================= */

    fn push_context(parts: &mut Vec<String>, label: &str, value: Option<&str>) {
        let Some(value) = value else {
            return;
        };

        let value = value.trim();

        if value.is_empty() {
            return;
        }

        parts.push(format!("{}: {}", label, value));
    }

    /*
     * IMPORTANT:
     *
     * This function accepts ImageCharacterContext.
     *
     * ImageSceneCharacter contains an ImageCharacterContext
     * inside its `character` field.
     */
    fn push_character_context(parts: &mut Vec<String>, character: &ImageCharacterContext) {
        Self::push_context(parts, "Character name", character.name.as_deref());

        Self::push_context(
            parts,
            "Character description",
            character.description.as_deref(),
        );

        Self::push_context(
            parts,
            "Character appearance",
            character.appearance.as_deref(),
        );

        Self::push_context(
            parts,
            "Character personality",
            character.personality.as_deref(),
        );
    }

    /*
     * Scene-character helper.
     *
     * This fixes the previous:
     *
     * expected &ImageCharacterContext
     * found &ImageSceneCharacter
     *
     * error.
     */
    fn push_scene_character_context(
        parts: &mut Vec<String>,
        scene_character: &ImageSceneCharacter,
    ) {
        Self::push_character_context(parts, &scene_character.character);

        Self::push_context(parts, "Scene role", scene_character.scene_role.as_deref());

        Self::push_context(parts, "Scene action", scene_character.action.as_deref());
    }

    fn build_visual_prompt(request: &ImageGenerationRequest) -> String {
        let mut parts = Vec::<String>::new();

        /* -----------------------------------------------------
        SCENE CONTEXT
        ----------------------------------------------------- */

        if let Some(scene) = request.scene_context.as_ref() {
            if let Some(visual_prompt) = scene.visual_prompt.as_deref() {
                let visual_prompt = visual_prompt.trim();

                if !visual_prompt.is_empty() {
                    parts.push(visual_prompt.to_string());
                }
            } else {
                Self::push_context(&mut parts, "Scene", scene.description.as_deref());
            }

            Self::push_context(&mut parts, "Environment", scene.environment.as_deref());

            Self::push_context(&mut parts, "Lighting", scene.lighting.as_deref());

            Self::push_context(&mut parts, "Composition", scene.composition.as_deref());

            Self::push_context(&mut parts, "Pose", scene.pose.as_deref());

            Self::push_context(&mut parts, "Outfit", scene.outfit.as_deref());

            Self::push_context(&mut parts, "Visual style", scene.visual_style.as_deref());

            /*
             * IMPORTANT:
             *
             * scene.characters contains ImageSceneCharacter,
             * not ImageCharacterContext.
             *
             * Therefore we pass:
             *
             *     &character.character
             *
             * through push_scene_character_context().
             */
            for character in &scene.characters {
                Self::push_scene_character_context(&mut parts, character);
            }
        }

        /* -----------------------------------------------------
        PRIMARY CHARACTER
        ----------------------------------------------------- */

        if let Some(character) = request.character_context.as_ref() {
            Self::push_character_context(&mut parts, character);
        }

        /* -----------------------------------------------------
        ORIGINAL PROMPT
        ----------------------------------------------------- */

        let original_prompt = request.prompt.trim();

        if !original_prompt.is_empty() && !parts.iter().any(|part| part.contains(original_prompt)) {
            parts.push(original_prompt.to_string());
        }

        /* -----------------------------------------------------
        NEGATIVE PROMPT
        ----------------------------------------------------- */

        if let Some(scene) = request.scene_context.as_ref() {
            if let Some(negative_prompt) = scene.negative_prompt.as_deref() {
                let negative_prompt = negative_prompt.trim();

                if !negative_prompt.is_empty() {
                    parts.push(format!("### Negative prompt: {}", negative_prompt));
                }
            }
        }

        parts.join(", ")
    }

    /* =========================================================
    SD SETTINGS
    ========================================================= */

    fn steps(request: &ImageGenerationRequest) -> u32 {
        request
            .advanced_model_settings
            .as_ref()
            .and_then(|settings| settings.sd_steps)
            .unwrap_or(30)
            .clamp(1, 150)
    }

    fn cfg_scale(request: &ImageGenerationRequest) -> f64 {
        request
            .advanced_model_settings
            .as_ref()
            .and_then(|settings| settings.sd_cfg_scale)
            .map(|value| Self::clamp_f64(value, 1.0, 30.0))
            .unwrap_or(7.0)
    }

    fn seed(request: &ImageGenerationRequest) -> Option<String> {
        let seed = request
            .advanced_model_settings
            .as_ref()
            .and_then(|settings| settings.sd_seed)
            .unwrap_or_default();

        if seed == 0 {
            None
        } else {
            Some(seed.to_string())
        }
    }

    fn denoising_strength(request: &ImageGenerationRequest) -> f64 {
        request
            .advanced_model_settings
            .as_ref()
            .and_then(|settings| settings.sd_denoising_strength)
            .map(|value| Self::clamp_f64(value, 0.0, 1.0))
            .unwrap_or(0.55)
    }

    /* =========================================================
    SIZE
    ========================================================= */

    fn horde_dimension(value: u32) -> u32 {
        let value = Self::positive_u32(Some(value), 512);

        /*
         * Round to the nearest 64 pixels.
         */
        ((value + 32) / 64) * 64
    }

    /* =========================================================
    MODEL
    ========================================================= */

    fn selected_model(request: &ImageGenerationRequest) -> Option<&str> {
        let model = request.model.trim();

        if model.is_empty() {
            None
        } else {
            Some(model)
        }
    }
}

/* =============================================================
IMAGE PROVIDER ADAPTER
============================================================= */

impl ImageProviderAdapter for AIHordeAdapter {
    fn endpoint(&self, base_url: &str, _request: &ImageGenerationRequest) -> String {
        format!("{}/v2/generate/async", Self::normalize_base_url(base_url))
    }

    fn requires_api_key(&self) -> bool {
        false
    }

    fn required_auth_headers(&self) -> &'static [&'static str] {
        &[]
    }

    /* =========================================================
    HEADERS
    ========================================================= */

    fn headers(
        &self,
        api_key: &str,
        extra: Option<&HashMap<String, String>>,
    ) -> HashMap<String, String> {
        let mut headers = HashMap::new();

        headers.insert("Content-Type".to_string(), "application/json".to_string());

        headers.insert("Accept".to_string(), "application/json".to_string());

        headers.insert("apikey".to_string(), Self::effective_api_key(api_key));

        if let Some(extra) = extra {
            for (key, value) in extra {
                headers.insert(key.clone(), value.clone());
            }
        }

        headers
    }

    /* =========================================================
    PAYLOAD
    ========================================================= */

    fn payload(&self, request: &ImageGenerationRequest) -> Result<ImageRequestPayload, String> {
        /* -----------------------------------------------------
        PROMPT
        ----------------------------------------------------- */

        let prompt = Self::build_visual_prompt(request);

        if prompt.trim().is_empty() {
            return Err("AI Horde requires a non-empty visual prompt.".to_string());
        }

        /* -----------------------------------------------------
        SIZE
        ----------------------------------------------------- */

        let (raw_width, raw_height) = parse_size_dimensions(request.size.as_deref(), 512, 512);

        let width = Self::horde_dimension(raw_width);

        let height = Self::horde_dimension(raw_height);

        /* -----------------------------------------------------
        NUMBER OF IMAGES
        ----------------------------------------------------- */

        let number_of_images = request.n.unwrap_or(1).clamp(1, 20);

        /* -----------------------------------------------------
        SD PARAMETERS
        ----------------------------------------------------- */

        let mut params = json!({
            "width": width,
            "height": height,
            "steps": Self::steps(request),
            "cfg_scale": Self::cfg_scale(request),
            "n": number_of_images
        });

        if let Some(seed) = Self::seed(request) {
            params["seed"] = json!(seed);
        }

        /* -----------------------------------------------------
        REFERENCE IMAGE
        ----------------------------------------------------- */

        let reference_image = Self::reference_image(request);

        if reference_image.is_some() {
            params["denoising_strength"] = json!(Self::denoising_strength(request));
        }

        /* -----------------------------------------------------
        MAIN REQUEST
        ----------------------------------------------------- */

        let mut body = json!({
            "prompt": prompt,

            "params": params,

            "r2": true,

            "shared": false,

            "replacement_filter": false,

            "nsfw": true,

            "censor_nsfw": false
        });

        /* -----------------------------------------------------
        SELECTED HORDE MODEL
        ----------------------------------------------------- */

        if let Some(model) = Self::selected_model(request) {
            body["models"] = json!([model]);
        }

        /* -----------------------------------------------------
        CHARACTER REFERENCE / IMG2IMG
        ----------------------------------------------------- */

        if let Some(image) = reference_image {
            if !Self::is_valid_reference_image(image) {
                return Err(
                    "The supplied character reference image is not a supported URL or image-data value."
                        .to_string(),
                );
            }

            body["source_image"] = json!(image);

            body["source_processing"] = json!("img2img");
        }

        /* -----------------------------------------------------
        MASK / INPAINTING
        ----------------------------------------------------- */

        if let Some(mask) = request.mask_image.as_deref() {
            let mask = mask.trim();

            if !mask.is_empty() {
                if reference_image.is_none() {
                    return Err(
                        "AI Horde inpainting requires a source/reference image.".to_string()
                    );
                }

                if !Self::is_valid_reference_image(mask) {
                    return Err(
                        "The supplied AI Horde mask is not a supported URL or image-data value."
                            .to_string(),
                    );
                }

                body["source_mask"] = json!(mask);

                body["source_processing"] = json!("inpainting");
            }
        }

        /*
         * Local LoRA paths cannot directly be sent to AI Horde.
         *
         * Horde requires LoRA names/identifiers that exist on
         * Horde workers. We intentionally do not send local
         * filesystem paths.
         */

        Ok(ImageRequestPayload::Json(body))
    }

    /* =========================================================
    RESPONSE
    ========================================================= */

    fn parse_response(&self, response: Value) -> Result<Vec<ImageResponseData>, String> {
        /* -----------------------------------------------------
        ERROR HANDLING
        ----------------------------------------------------- */

        if let Some(message) = response.get("message").and_then(Value::as_str) {
            return Err(format!("AI Horde error: {}", message));
        }

        if let Some(error) = response.get("error").and_then(Value::as_str) {
            return Err(format!("AI Horde error: {}", error));
        }

        if let Some(rc) = response.get("rc").and_then(Value::as_str) {
            let id_exists = response
                .get("id")
                .and_then(Value::as_str)
                .map(|id| !id.trim().is_empty())
                .unwrap_or(false);

            if !id_exists {
                return Err(format!("AI Horde request rejected: {}", rc));
            }
        }

        /* -----------------------------------------------------
        ASYNC GENERATION ID
        ----------------------------------------------------- */

        let id = response
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .ok_or_else(|| {
                format!(
                    "AI Horde did not return a generation ID. Response: {}",
                    response
                )
            })?;

        /*
         * The higher-level image generation system can recognize:
         *
         *     aihorde://<id>
         *
         * and poll:
         *
         *     /v2/generate/check/{id}
         *
         * followed by:
         *
         *     /v2/generate/status/{id}
         */
        Ok(vec![ImageResponseData {
            url: Some(format!("aihorde://{}", id)),
            b64_json: None,
            text: None,
        }])
    }

    fn response_format(&self) -> super::ImageResponseFormat {
        super::ImageResponseFormat::Json
    }

    fn supports_stream(&self) -> bool {
        false
    }
}

/* =============================================================
TESTS
============================================================= */

#[cfg(test)]
mod tests {
    use super::*;

    fn base_request() -> ImageGenerationRequest {
        ImageGenerationRequest {
            prompt: "anime character".to_string(),

            model: "Test Model".to_string(),

            provider_id: "aihorde".to_string(),

            credential_id: "horde".to_string(),

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
    fn empty_api_key_is_replaced_by_anonymous_key() {
        assert_eq!(AIHordeAdapter::clean_api_key(""), None);

        assert_eq!(AIHordeAdapter::clean_api_key("   "), None);

        assert_eq!(
            AIHordeAdapter::clean_api_key(" abc "),
            Some("abc".to_string())
        );

        assert_eq!(
            AIHordeAdapter::effective_api_key(""),
            AIHordeAdapter::ANONYMOUS_API_KEY
        );
    }

    #[test]
    fn model_endpoint_is_correct() {
        assert_eq!(
            AIHordeAdapter::models_endpoint("https://aihorde.net/api"),
            "https://aihorde.net/api/v2/status/models"
        );

        assert_eq!(
            AIHordeAdapter::models_endpoint("https://aihorde.net/api/"),
            "https://aihorde.net/api/v2/status/models"
        );
    }

    #[test]
    fn reference_urls_are_valid() {
        assert!(AIHordeAdapter::is_valid_reference_image(
            "https://example.com/image.png"
        ));

        assert!(AIHordeAdapter::is_valid_reference_image(
            "http://example.com/image.jpg"
        ));
    }

    #[test]
    fn data_images_are_valid() {
        assert!(AIHordeAdapter::is_valid_reference_image(
            "data:image/png;base64,iVBOR..."
        ));
    }

    #[test]
    fn base64_images_are_valid() {
        assert!(AIHordeAdapter::is_valid_reference_image(
            "iVBORw0KGgoAAAANSUhEUg"
        ));

        assert!(AIHordeAdapter::is_valid_reference_image(
            "/9j/4AAQSkZJRgABAQ"
        ));

        assert!(AIHordeAdapter::is_valid_reference_image("UklGR"));
    }

    #[test]
    fn invalid_reference_is_rejected() {
        assert!(!AIHordeAdapter::is_valid_reference_image(""));

        assert!(!AIHordeAdapter::is_valid_reference_image("not-an-image"));
    }

    #[test]
    fn dimensions_are_rounded_to_64() {
        assert_eq!(AIHordeAdapter::horde_dimension(512), 512);

        assert_eq!(AIHordeAdapter::horde_dimension(513), 512);

        assert_eq!(AIHordeAdapter::horde_dimension(544), 576);

        assert_eq!(AIHordeAdapter::horde_dimension(1024), 1024);
    }

    #[test]
    fn async_job_is_parsed() {
        let response = json!({
            "id": "123456"
        });

        let result = AIHordeAdapter.parse_response(response).unwrap();

        assert_eq!(result.len(), 1);

        assert_eq!(result[0].url.as_deref(), Some("aihorde://123456"));
    }

    #[test]
    fn horde_error_is_returned() {
        let response = json!({
            "message":
                "Invalid prompt"
        });

        let result = AIHordeAdapter.parse_response(response);

        assert!(result.is_err());

        assert!(result.unwrap_err().contains("Invalid prompt"));
    }

    #[test]
    fn horde_rc_error_is_returned() {
        let response = json!({
            "rc":
                "Request cannot be satisfied"
        });

        let result = AIHordeAdapter.parse_response(response);

        assert!(result.is_err());

        assert!(result.unwrap_err().contains("Request cannot be satisfied"));
    }

    #[test]
    fn missing_generation_id_is_error() {
        let response = json!({
            "done": false
        });

        let result = AIHordeAdapter.parse_response(response);

        assert!(result.is_err());
    }

    #[test]
    fn selected_model_is_taken_from_request() {
        let request = base_request();

        let adapter = AIHordeAdapter;

        let payload = adapter.payload(&request).unwrap();

        let ImageRequestPayload::Json(body) = payload else {
            panic!("Expected JSON payload");
        };

        assert_eq!(body["models"][0], "Test Model");
    }

    #[test]
    fn empty_model_does_not_add_models_array() {
        let mut request = base_request();

        request.model = String::new();

        let payload = AIHordeAdapter.payload(&request).unwrap();

        let ImageRequestPayload::Json(body) = payload else {
            panic!("Expected JSON payload");
        };

        assert!(body.get("models").is_none());
    }

    #[test]
    fn anonymous_headers_are_created() {
        let adapter = AIHordeAdapter;

        let headers = adapter.headers("", None);

        assert_eq!(
            headers.get("apikey").map(String::as_str),
            Some(AIHordeAdapter::ANONYMOUS_API_KEY)
        );

        assert_eq!(
            headers.get("Content-Type").map(String::as_str),
            Some("application/json")
        );
    }

    #[test]
    fn custom_horde_key_is_preserved() {
        let adapter = AIHordeAdapter;

        let headers = adapter.headers(" my-horde-key ", None);

        assert_eq!(
            headers.get("apikey").map(String::as_str),
            Some("my-horde-key")
        );
    }

    #[test]
    fn img2img_adds_source_processing() {
        let mut request = base_request();

        request.input_images = Some(vec!["https://example.com/ref.png".to_string()]);

        let body = match AIHordeAdapter.payload(&request).unwrap() {
            ImageRequestPayload::Json(body) => body,

            _ => panic!("Expected JSON payload"),
        };

        assert_eq!(body["source_image"], "https://example.com/ref.png");

        assert_eq!(body["source_processing"], "img2img");
    }

    #[test]
    fn character_reference_image_is_used() {
        let mut request = base_request();

        request.character_reference_image = Some("https://example.com/character.png".to_string());

        let body = match AIHordeAdapter.payload(&request).unwrap() {
            ImageRequestPayload::Json(body) => body,

            _ => panic!("Expected JSON payload"),
        };

        assert_eq!(body["source_image"], "https://example.com/character.png");

        assert_eq!(body["source_processing"], "img2img");

        assert!(body["params"].get("denoising_strength").is_some());
    }

    #[test]
    fn character_context_reference_image_is_used() {
        let mut request = base_request();

        request.character_context = Some(ImageCharacterContext {
            name: Some("Alice".to_string()),

            description: Some("Anime girl".to_string()),

            appearance: Some("Long black hair".to_string()),

            personality: Some("Shy".to_string()),

            reference_image: Some("https://example.com/alice.png".to_string()),

            reference_images: Vec::new(),
        });

        let body = match AIHordeAdapter.payload(&request).unwrap() {
            ImageRequestPayload::Json(body) => body,

            _ => panic!("Expected JSON payload"),
        };

        assert_eq!(body["source_image"], "https://example.com/alice.png");
    }

    #[test]
    fn scene_character_context_is_added_to_prompt() {
        let mut request = base_request();

        request.scene_context = Some(crate::image_generator::types::ImageSceneContext {
            description: Some("A dark forest".to_string()),

            visual_prompt: None,

            negative_prompt: None,

            characters: vec![ImageSceneCharacter {
                character_id: Some("alice".to_string()),

                character: ImageCharacterContext {
                    name: Some("Alice".to_string()),

                    description: Some("A mysterious anime girl".to_string()),

                    appearance: Some("Long black hair".to_string()),

                    personality: Some("Quiet and shy".to_string()),

                    reference_image: None,

                    reference_images: Vec::new(),
                },

                scene_role: Some("Main heroine".to_string()),

                action: Some("Standing in the forest".to_string()),
            }],

            environment: None,

            lighting: None,

            composition: None,

            pose: None,

            outfit: None,

            visual_style: None,
        });

        let body = match AIHordeAdapter.payload(&request).unwrap() {
            ImageRequestPayload::Json(body) => body,

            _ => panic!("Expected JSON payload"),
        };

        let prompt = body["prompt"].as_str().unwrap();

        assert!(prompt.contains("Alice"));

        assert!(prompt.contains("Long black hair"));

        assert!(prompt.contains("Standing in the forest"));
    }

    #[test]
    fn inpainting_requires_reference_image() {
        let mut request = base_request();

        request.mask_image = Some("https://example.com/mask.png".to_string());

        assert!(AIHordeAdapter.payload(&request).is_err());
    }

    #[test]
    fn inpainting_uses_mask() {
        let mut request = base_request();

        request.character_reference_image = Some("https://example.com/source.png".to_string());

        request.mask_image = Some("https://example.com/mask.png".to_string());

        let body = match AIHordeAdapter.payload(&request).unwrap() {
            ImageRequestPayload::Json(body) => body,

            _ => panic!("Expected JSON payload"),
        };

        assert_eq!(body["source_image"], "https://example.com/source.png");

        assert_eq!(body["source_mask"], "https://example.com/mask.png");

        assert_eq!(body["source_processing"], "inpainting");
    }
}
