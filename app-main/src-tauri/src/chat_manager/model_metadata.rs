//! Model metadata cache with provider-aware fetching
//!
//! This module provides a cache for model metadata fetched from providers,
//! enabling dynamic context length resolution based on provider-reported
//! model capabilities rather than hardcoded fallbacks.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::AppHandle;
use tokio::sync::RwLock;

use crate::chat_manager::types::{Model, ProviderCredential, Settings};
use crate::infra::utils::now_secs;
use crate::infra::utils::{log_error, log_info};

/// Cached model metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedModelMetadata {
    pub model_id: String,
    pub provider_id: String,
    pub base_url: String,
    pub context_length: Option<u32>,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub fetched_at: u64, // Unix timestamp
}

/// Cache key for model metadata
#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct ModelCacheKey {
    pub provider_id: String,
    pub base_url: String,
    pub model_id: String,
}

impl ModelCacheKey {
    pub fn new(provider_id: &str, base_url: &str, model_id: &str) -> Self {
        Self {
            provider_id: provider_id.to_string(),
            base_url: normalize_base_url(base_url),
            model_id: normalize_model_id(model_id),
        }
    }
}

/// Normalize base URL for consistent cache keys
fn normalize_base_url(base_url: &str) -> String {
    let url = base_url.trim_end_matches('/');
    if url.contains("://") {
        // Extract scheme + host + port
        if let Some(idx) = url.find("://") {
            let after_scheme = &url[idx + 3..];
            if let Some(idx2) = after_scheme.find('/') {
                return format!("{}://{}", &url[..idx + 3], &after_scheme[..idx2]);
            }
            return url.to_string();
        }
    }
    url.to_string()
}

/// Normalize model ID for consistent cache keys
fn normalize_model_id(model_id: &str) -> String {
    model_id.trim().to_lowercase()
}

/// Model metadata cache
#[derive(Clone)]
pub struct ModelMetadataCache {
    cache: Arc<RwLock<HashMap<ModelCacheKey, CachedModelMetadata>>>,
    cache_ttl: Duration,
}

impl Default for ModelMetadataCache {
    fn default() -> Self {
        Self::new()
    }
}

impl ModelMetadataCache {
    pub fn new() -> Self {
        Self {
            cache: Arc::new(RwLock::new(HashMap::new())),
            cache_ttl: Duration::from_secs(3600), // 1 hour default TTL
        }
    }

    pub fn with_ttl(ttl: Duration) -> Self {
        Self {
            cache: Arc::new(RwLock::new(HashMap::new())),
            cache_ttl: ttl,
        }
    }

    /// Get cached model metadata if valid
    pub async fn get(&self, key: &ModelCacheKey) -> Option<CachedModelMetadata> {
        let cache = self.cache.read().await;
        cache.get(key).and_then(|entry| {
            if entry.fetched_at + self.cache_ttl.as_secs() > crate::utils::now_secs() {
                Some(entry.clone())
            } else {
                None
            }
        })
    }

    /// Insert or update cached model metadata
    pub async fn insert(&self, key: ModelCacheKey, metadata: CachedModelMetadata) {
        let mut cache = self.cache.write().await;
        cache.insert(key, metadata);
    }

    /// Remove expired entries
    pub async fn cleanup_expired(&self) {
        let now = crate::utils::now_secs();
        let mut cache = self.cache.write().await;
        cache.retain(|_, v| v.fetched_at + self.cache_ttl.as_secs() > now);
    }

    /// Clear all cache entries
    pub async fn clear(&self) {
        self.cache.write().await.clear();
    }
}

/// Context length resolution result
#[derive(Debug, Clone)]
pub struct ContextLengthResolution {
    pub context_length: u32,
    pub source: ContextLengthSource,
}

/// Source of the resolved context length
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextLengthSource {
    /// Explicit user configuration (session/model/settings)
    ExplicitUserConfig,
    /// Provider-reported model metadata
    ProviderMetadata,
    /// Fallback when metadata unavailable
    Fallback,
}

/// Resolve context length for a model using provider metadata
pub async fn resolve_context_length_with_metadata(
    app: &AppHandle,
    model: &crate::chat_manager::types::Model,
    credential: &crate::chat_manager::types::ProviderCredential,
    settings: &crate::chat_manager::types::Settings,
) -> ContextLengthResolution {
    // 1. Check explicit user configuration first (highest priority)
    if let Some(explicit) = resolve_explicit_context_length(model, credential, settings) {
        return crate::chat_manager::model_metadata::ContextLengthResolution {
            context_length: explicit,
            source: crate::chat_manager::model_metadata::ContextLengthSource::ExplicitUserConfig,
        };
    }

    // 2. Try to get provider metadata
    if let Some(metadata) = fetch_model_metadata(app, model, credential, settings).await {
        if let Some(ctx_len) = metadata.context_length {
            if ctx_len > 0 {
                log_info(
                    app,
                    "context_length",
                    format!(
                        "model={} provider={} context_length={} source=provider_metadata",
                        model.name, metadata.provider_id, ctx_len
                    ),
                );
                return crate::chat_manager::model_metadata::ContextLengthResolution {
                    context_length: ctx_len,
                    source: crate::chat_manager::model_metadata::ContextLengthSource::ProviderMetadata,
                };
            }
        }
    }

    // 3. Fallback with warning
    log_error(
        app,
        "context_length",
        format!(
            "model={} provider={} context_length: provider metadata unavailable; using fallback 8192 (NOT model's real limit)",
            model.name,
            get_provider_id(credential)
        ),
    );

    crate::chat_manager::model_metadata::ContextLengthResolution {
        context_length: 8192,
        source: crate::chat_manager::model_metadata::ContextLengthSource::Fallback,
    }
}

/// Resolve explicit context length from user configuration
fn resolve_explicit_context_length(
    model: &crate::chat_manager::types::Model,
    credential: &crate::chat_manager::types::ProviderCredential,
    settings: &crate::chat_manager::types::Settings,
) -> Option<u32> {
    // Check session-level model override
    if let Some(adv) = model.advanced_model_settings.as_ref() {
        if let Some(ctx) = adv.context_length {
            if ctx > 0 {
                return Some(ctx);
            }
        }
    }

    // Check model-level override
    if let Some(adv) = model.advanced_model_settings.as_ref() {
        if let Some(ctx) = adv.context_length {
            if ctx > 0 {
                return Some(ctx);
            }
        }
    }

    // Check session-level
    if let Some(adv) = settings.advanced_model_settings.context_length {
        if adv > 0 {
            return Some(adv);
        }
    }

    // Check global settings
    if let Some(adv) = settings.advanced_model_settings.context_length {
        if adv > 0 {
            return Some(adv);
        }
    }

    None
}

/// Fetch model metadata from provider
async fn fetch_model_metadata(
    app: &AppHandle,
    model: &crate::chat_manager::types::Model,
    credential: &crate::chat_manager::types::ProviderCredential,
    settings: &crate::chat_manager::types::Settings,
) -> Option<CachedModelMetadata> {
    // Skip for local providers
    if credential.provider_id.eq_ignore_ascii_case("llamacpp") {
        return None;
    }

    // Build cache key
    let base_url = credential.base_url.clone().unwrap_or_else(|| {
        crate::providers::config::resolve_base_url(
            &crate::chat_manager::types::ProviderId(credential.provider_id.clone()),
            None,
        )
    });

    let cache_key = ModelCacheKey::new(&credential.provider_id, &base_url, &model.id);

    // Check cache first
    let cache = get_model_metadata_cache();
    if let Some(cached) = cache.get(&cache_key).await {
        if cached.context_length.is_some() {
            return Some(cached);
        }
    }

    // Fetch models from provider
    let models = fetch_remote_models(app, credential).await?;
    
    // Find our model
    let model_info = models.into_iter().find(|m| {
        normalize_model_id(&m.id) == normalize_model_id(&model.id)
    })?;

    let metadata = CachedModelMetadata {
        model_id: model.id.clone(),
        provider_id: model.provider_id.clone(),
        base_url: base_url.to_string(),
        context_length: model_info.context_length.map(|v| v as u32),
        display_name: model_info.display_name,
        description: model_info.description,
        fetched_at: crate::utils::now_secs(),
    };

    // Cache it
    let cache_key = ModelCacheKey::new(&credential.provider_id, &base_url, &model.id);
    get_model_metadata_cache().insert(cache_key, metadata.clone()).await;

    Some(metadata)
}

/// Global model metadata cache instance
use std::sync::OnceLock;
static MODEL_METADATA_CACHE: std::sync::OnceLock<ModelMetadataCache> = std::sync::OnceLock::new();

pub fn get_model_metadata_cache() -> &'static ModelMetadataCache {
    MODEL_METADATA_CACHE.get_or_init(ModelMetadataCache::new)
}

/// Fetch remote models using existing infrastructure
async fn fetch_remote_models(
    app: &AppHandle,
    credential: &crate::chat_manager::types::ProviderCredential,
) -> Option<Vec<crate::chat_manager::provider_adapter::ModelInfo>> {
    // Use the existing get_remote_models command infrastructure
    crate::providers::commands::get_remote_models(app.clone(), credential.id.clone())
        .await
        .ok()
}

pub fn get_provider_id(credential: &crate::chat_manager::types::ProviderCredential) -> String {
    credential.provider_id.clone()
}