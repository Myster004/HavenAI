use serde::{Deserialize, Serialize};

use crate::chat_manager::types::AdvancedModelSettings;

/* =========================================================
LORA
========================================================= */

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageLora {
    pub path: String,

    pub multiplier: f64,

    #[serde(default)]
    pub is_high_noise: bool,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
}

/* =========================================================
CHARACTER IMAGE CONTEXT
========================================================= */

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageCharacterContext {
    /// Character display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// Character description/persona.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Physical appearance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub appearance: Option<String>,

    /// Personality information.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub personality: Option<String>,

    /// Primary profile/reference image.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference_image: Option<String>,

    /// Additional character reference images.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reference_images: Vec<String>,
}

impl ImageCharacterContext {
    /// Returns the primary reference image.
    pub fn primary_reference_image(&self) -> Option<&str> {
        self.reference_image
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
    }

    /// Returns all character reference images.
    ///
    /// Owned Strings are returned intentionally so callers do not
    /// run into lifetime problems when combining images from
    /// multiple sources.
    pub fn all_reference_images(&self) -> Vec<String> {
        let mut images = Vec::<String>::new();

        if let Some(image) = self.primary_reference_image() {
            images.push(image.to_string());
        }

        for image in &self.reference_images {
            let image = image.trim();

            if image.is_empty() {
                continue;
            }

            if !images.iter().any(|existing| existing == image) {
                images.push(image.to_string());
            }
        }

        images
    }

    /// Returns true when this context contains no useful data.
    pub fn is_empty(&self) -> bool {
        self.name
            .as_deref()
            .map(str::trim)
            .map_or(true, str::is_empty)
            && self
                .description
                .as_deref()
                .map(str::trim)
                .map_or(true, str::is_empty)
            && self
                .appearance
                .as_deref()
                .map(str::trim)
                .map_or(true, str::is_empty)
            && self
                .personality
                .as_deref()
                .map(str::trim)
                .map_or(true, str::is_empty)
            && self.all_reference_images().is_empty()
    }
}

/* =========================================================
SCENE CHARACTER
========================================================= */

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageSceneCharacter {
    /// Optional character identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub character_id: Option<String>,

    /// Character information.
    pub character: ImageCharacterContext,

    /// Optional scene-specific role.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene_role: Option<String>,

    /// Optional scene-specific action.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
}

/* =========================================================
SCENE IMAGE CONTEXT
========================================================= */

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageSceneContext {
    /// General scene description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Final visual prompt produced by Scene Writer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visual_prompt: Option<String>,

    /// Negative prompt produced by Scene Writer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub negative_prompt: Option<String>,

    /// Characters participating in this scene.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub characters: Vec<ImageSceneCharacter>,

    /// Location/environment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,

    /// Lighting description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lighting: Option<String>,

    /// Camera/composition description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub composition: Option<String>,

    /// Pose/action description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pose: Option<String>,

    /// Clothing/outfit description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outfit: Option<String>,

    /// Visual style.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visual_style: Option<String>,
}

impl ImageSceneContext {
    /// Returns the best available visual prompt.
    pub fn effective_visual_prompt(&self) -> Option<&str> {
        self.visual_prompt
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .or_else(|| {
                self.description
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
            })
    }
}

/* =========================================================
IMAGE GENERATION REQUEST
========================================================= */

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageGenerationRequest {
    /* -----------------------------------------------------
    PROVIDER
    ----------------------------------------------------- */
    /// Final image prompt.
    pub prompt: String,

    /// Image model.
    ///
    /// For AI Horde this should contain the exact Horde model
    /// name selected by the user.
    pub model: String,

    /// Provider identifier.
    pub provider_id: String,

    /// Stored provider credential ID.
    pub credential_id: String,

    /* -----------------------------------------------------
    MODEL SETTINGS
    ----------------------------------------------------- */
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub advanced_model_settings: Option<AdvancedModelSettings>,

    /* -----------------------------------------------------
    GENERIC INPUT IMAGES
    ----------------------------------------------------- */
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_images: Option<Vec<String>>,

    /// Optional mask image.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask_image: Option<String>,

    /* -----------------------------------------------------
    LORA
    ----------------------------------------------------- */
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loras: Option<Vec<ImageLora>>,

    /* -----------------------------------------------------
    CHARACTER CONTEXT
    ----------------------------------------------------- */
    /// Primary character information.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub character_context: Option<ImageCharacterContext>,

    /// Explicit primary character reference image.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub character_reference_image: Option<String>,

    /// Additional character reference images.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub character_reference_images: Vec<String>,

    /* -----------------------------------------------------
    SCENE CONTEXT
    ----------------------------------------------------- */
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene_context: Option<ImageSceneContext>,

    /* -----------------------------------------------------
    OUTPUT
    ----------------------------------------------------- */
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_modalities: Option<Vec<String>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<String>,

    /// Number of images requested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub n: Option<u32>,

    /* -----------------------------------------------------
    USAGE ATTRIBUTION
    ----------------------------------------------------- */
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub character_id: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub character_name: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_source: Option<String>,
}

/* =========================================================
IMAGE REQUEST HELPERS
========================================================= */

impl ImageGenerationRequest {
    /// Returns the primary character reference image.
    ///
    /// Priority:
    ///
    /// 1. Explicit character_reference_image
    /// 2. character_context.reference_image
    /// 3. First character_context.reference_images
    /// 4. First character_reference_images entry
    /// 5. First input_images entry
    pub fn primary_reference_image(&self) -> Option<&str> {
        self.character_reference_image
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .or_else(|| {
                self.character_context
                    .as_ref()
                    .and_then(ImageCharacterContext::primary_reference_image)
            })
            .or_else(|| {
                self.character_context.as_ref().and_then(|context| {
                    context
                        .reference_images
                        .iter()
                        .map(String::as_str)
                        .map(str::trim)
                        .find(|value| !value.is_empty())
                })
            })
            .or_else(|| {
                self.character_reference_images
                    .iter()
                    .map(String::as_str)
                    .map(str::trim)
                    .find(|value| !value.is_empty())
            })
            .or_else(|| {
                self.input_images.as_ref().and_then(|images| {
                    images
                        .iter()
                        .map(String::as_str)
                        .map(str::trim)
                        .find(|value| !value.is_empty())
                })
            })
    }

    /// Collect every usable reference image.
    ///
    /// Returns owned Strings to avoid lifetime issues.
    ///
    /// Duplicates are removed while insertion order is preserved.
    pub fn all_reference_images(&self) -> Vec<String> {
        let mut result = Vec::<String>::new();

        let mut add_image = |image: &str| {
            let image = image.trim();

            if image.is_empty() {
                return;
            }

            if !result.iter().any(|existing| existing == image) {
                result.push(image.to_string());
            }
        };

        if let Some(image) = self.character_reference_image.as_deref() {
            add_image(image);
        }

        if let Some(context) = self.character_context.as_ref() {
            if let Some(image) = context.reference_image.as_deref() {
                add_image(image);
            }

            for image in &context.reference_images {
                add_image(image);
            }
        }

        for image in &self.character_reference_images {
            add_image(image);
        }

        if let Some(images) = self.input_images.as_ref() {
            for image in images {
                add_image(image);
            }
        }

        result
    }

    /// Returns the Scene Writer visual prompt when available.
    pub fn effective_prompt(&self) -> &str {
        self.scene_context
            .as_ref()
            .and_then(ImageSceneContext::effective_visual_prompt)
            .unwrap_or_else(|| self.prompt.trim())
    }

    /// Returns the Scene Writer negative prompt.
    pub fn negative_prompt(&self) -> Option<&str> {
        self.scene_context.as_ref().and_then(|scene| {
            scene
                .negative_prompt
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
        })
    }

    /// Returns true when this is a Scene Writer generation.
    pub fn is_scene_generation(&self) -> bool {
        self.usage_source
            .as_deref()
            .map(|source| source.eq_ignore_ascii_case("scene"))
            .unwrap_or(false)
            || self
                .scene_context
                .as_ref()
                .is_some_and(|scene| scene.effective_visual_prompt().is_some())
    }
}

/* =========================================================
GENERATED IMAGE
========================================================= */

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeneratedImage {
    pub asset_id: String,

    pub file_path: String,

    pub mime_type: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/* =========================================================
IMAGE GENERATION RESPONSE
========================================================= */

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageGenerationResponse {
    pub images: Vec<GeneratedImage>,

    pub model: String,

    pub provider_id: String,
}
