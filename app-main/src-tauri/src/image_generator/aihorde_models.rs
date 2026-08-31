/* =============================================================
NEW FILE: src-tauri/src/image_generator/aihorde_models.rs

commands.rs already imports from this module:

    use crate::image_generator::aihorde_models::{
        fetch_ai_horde_models,
        AIHordeModel,
    };

...and calls it from two commands that are already registered
in app/commands.rs's invoke_handler!:

    get_aihorde_models       -> Vec<AIHordeModel>
    get_aihorde_model_names  -> Vec<String> (mapped from .name)

This file provides both. After adding it, also add this line
to image_generator/mod.rs:

    pub mod aihorde_models;

(alongside the existing comfyui / commands / provider_adapter /
sdcpp / storage / types module declarations)
============================================================= */

/// A single model entry as returned to the frontend Models picker.
///
/// Kept intentionally small — just what the picker UI needs.
/// `name` is the exact string to store as `request.model` for
/// later image-generation calls.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AIHordeModel {
    pub name: String,

    /// Workers currently online for this model. 0 means the
    /// model is known but nobody is currently serving it — a
    /// request would still queue rather than fail outright.
    #[serde(default)]
    pub count: u32,

    /// Jobs currently queued against this model.
    #[serde(default)]
    pub queued: f64,

    /// Rough seconds-per-job estimate, when Horde provides one.
    #[serde(default)]
    pub eta: Option<u32>,
}

/*
 * Raw row shape from Horde's /v2/status/models endpoint, before
 * normalization into AIHordeModel. Kept separate so the public
 * struct above stays exactly what the frontend needs.
 */
#[derive(Debug, Clone, serde::Deserialize)]
struct AIHordeModelStatusRow {
    name: String,

    #[serde(default)]
    count: u32,

    #[serde(default)]
    queued: f64,

    #[serde(default)]
    eta: Option<u32>,
}

/// Default base URL for AI Horde's public API.
///
/// Kept here (rather than only in provider_adapter::aihorde) so
/// this module has no dependency on the adapter — model listing
/// is a read-only status call and doesn't need the full adapter
/// machinery.
const AIHORDE_DEFAULT_BASE_URL: &str = "https://aihorde.net/api";

/// Fetch the currently online AI Horde image models.
///
/// Calls Horde's public, read-only status endpoint:
///
///     GET https://aihorde.net/api/v2/status/models?type=image
///
/// No API key is required for this endpoint, but one is
/// forwarded when supplied (including Horde's anonymous
/// placeholder "0000000000") since an authenticated caller gets
/// a slightly higher rate limit from Horde for status calls.
///
/// Online models (count > 0) are sorted first by worker count
/// descending; offline models are appended afterward sorted
/// alphabetically, so the result is directly usable by the
/// Models picker without additional frontend-side sorting.
pub async fn fetch_ai_horde_models(api_key: Option<&str>) -> Result<Vec<AIHordeModel>, String> {
    let url = format!("{}/v2/status/models?type=image", AIHORDE_DEFAULT_BASE_URL,);

    let client = reqwest::Client::new();

    let mut request = client.get(&url);

    if let Some(key) = api_key {
        let key = key.trim();

        if !key.is_empty() {
            request = request.header("apikey", key);
        }
    }

    let response = request
        .send()
        .await
        .map_err(|error| format!("AI Horde model list request failed: {}", error))?;

    let status_code = response.status();

    if !status_code.is_success() {
        let body = response.text().await.unwrap_or_default();

        return Err(format!(
            "AI Horde model list error {}: {}",
            status_code, body
        ));
    }

    let rows: Vec<AIHordeModelStatusRow> = response
        .json()
        .await
        .map_err(|error| format!("Failed to parse AI Horde model list: {}", error))?;

    let mut models: Vec<AIHordeModel> = rows
        .into_iter()
        .map(|row| AIHordeModel {
            name: row.name,
            count: row.count,
            queued: row.queued,
            eta: row.eta,
        })
        .collect();

    models.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.name.cmp(&b.name)));

    Ok(models)
}

/* =========================================================
TESTS
========================================================= */

#[cfg(test)]
mod tests {
    use super::AIHordeModel;

    #[test]
    fn sorts_online_first_by_count_then_alphabetical() {
        let mut models = vec![
            AIHordeModel {
                name: "Zebra".to_string(),
                count: 0,
                queued: 0.0,
                eta: None,
            },
            AIHordeModel {
                name: "Rev Animated".to_string(),
                count: 5,
                queued: 1.0,
                eta: Some(20),
            },
            AIHordeModel {
                name: "Aardvark".to_string(),
                count: 0,
                queued: 0.0,
                eta: None,
            },
        ];

        models.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.name.cmp(&b.name)));

        assert_eq!(models[0].name, "Rev Animated");
        assert_eq!(models[1].name, "Aardvark");
        assert_eq!(models[2].name, "Zebra");
    }
}
