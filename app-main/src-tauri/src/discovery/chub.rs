use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::AppHandle;

use super::{cache_get, cache_set, read_pure_mode_level};
use crate::storage_manager::lorebook::{
    set_character_lorebooks, upsert_lorebook, upsert_lorebook_entry, Lorebook, LorebookEntry,
};
use crate::storage_manager::media::{generate_avatar_gradient, storage_save_avatar};
use crate::utils::{log_error, log_info};

const CHUB_GATEWAY_BASE: &str = "https://gateway.chub.ai";
const CHUB_SEARCH_TTL_SECS: i64 = 300;
const CHUB_DETAIL_TTL_SECS: i64 = 300;
const CHUB_TAG_TTL_SECS: i64 = 3600;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChubSearchResultDto {
    pub nodes: Vec<Value>,
    pub page: u32,
    pub total_pages: Option<u32>,
    pub total_nodes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChubTagInfo {
    pub name: String,
    pub count: Option<u64>,
}

/// Shared HTTP client for Chub public API calls.
/// When an optional API key is configured (Settings → Advanced), it is sent via
/// Chub's documented `X-API-KEY` auth header; otherwise requests stay anonymous.
fn http_client(api_key: Option<&str>) -> reqwest::Client {
    let mut builder = reqwest::Client::builder().timeout(std::time::Duration::from_secs(30));
    if let Some(key) = api_key {
        let key = key.trim();
        if !key.is_empty() {
            if let Ok(value) = reqwest::header::HeaderValue::from_str(key) {
                let mut headers = reqwest::header::HeaderMap::new();
                headers.insert("X-API-KEY", value);
                builder = builder.default_headers(headers);
            }
        }
    }
    builder.build().unwrap_or_default()
}

fn str_field(value: &Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(item) = value.get(*key) {
            match item {
                Value::String(text) if !text.trim().is_empty() => return Some(text.clone()),
                Value::Number(number) => return Some(number.to_string()),
                _ => {}
            }
        }
    }
    None
}

fn u64_field(value: &Value, keys: &[&str]) -> Option<u64> {
    for key in keys {
        if let Some(item) = value.get(*key) {
            if let Some(number) = item.as_u64() {
                return Some(number);
            }
            if let Some(number) = item.as_f64() {
                if number.is_finite() && number >= 0.0 {
                    return Some(number as u64);
                }
            }
        }
    }
    None
}

fn bool_field(value: &Value, keys: &[&str]) -> Option<bool> {
    for key in keys {
        if let Some(item) = value.get(*key) {
            if let Some(flag) = item.as_bool() {
                return Some(flag);
            }
        }
    }
    None
}

fn string_array_field(value: &Value, key: &str) -> Vec<String> {
    let mut output = vec![];
    if let Some(Value::Array(items)) = value.get(key) {
        for item in items {
            match item {
                Value::String(text) if !text.trim().is_empty() => output.push(text.clone()),
                Value::Object(obj) => {
                    if let Some(Value::String(text)) = obj.get("name") {
                        if !text.trim().is_empty() {
                            output.push(text.clone());
                        }
                    }
                }
                _ => {}
            }
        }
    }
    output
}

// --- PNG card fallback helpers (mirrors entity_transfer logic) ---
fn decode_png_text_chunk(chunk_type: &str, chunk: &[u8]) -> Option<(String, String)> {
    match chunk_type {
        "tEXt" => {
            let separator_index = chunk.iter().position(|byte| *byte == 0)?;
            if separator_index == 0 {
                return None;
            }
            let keyword = String::from_utf8(chunk[..separator_index].to_vec()).ok()?;
            let text = String::from_utf8(chunk[separator_index + 1..].to_vec()).ok()?;
            Some((keyword, text))
        }
        "zTXt" => {
            let separator_index = chunk.iter().position(|byte| *byte == 0)?;
            if separator_index == 0 || separator_index + 2 > chunk.len() {
                return None;
            }
            let keyword = String::from_utf8(chunk[..separator_index].to_vec()).ok()?;
            let compression_method = chunk[separator_index + 1];
            if compression_method != 0 {
                return None;
            }
            let mut decoder = flate2::read::ZlibDecoder::new(&chunk[separator_index + 2..]);
            let mut text = String::new();
            use std::io::Read;
            decoder.read_to_string(&mut text).ok()?;
            Some((keyword, text))
        }
        "iTXt" => {
            let mut cursor = 0usize;
            let next_null = |cursor: &mut usize| -> Option<usize> {
                let relative = chunk.get(*cursor..)?.iter().position(|byte| *byte == 0)?;
                let index = *cursor + relative;
                *cursor = index + 1;
                Some(index)
            };
            let keyword_end = next_null(&mut cursor)?;
            if keyword_end == 0 || cursor + 1 >= chunk.len() {
                return None;
            }
            let keyword = String::from_utf8(chunk[..keyword_end].to_vec()).ok()?;
            let compression_flag = chunk[cursor];
            let compression_method = chunk[cursor + 1];
            cursor += 2;
            next_null(&mut cursor)?;
            next_null(&mut cursor)?;
            let text_bytes = chunk.get(cursor..)?;
            if compression_flag == 1 {
                if compression_method != 0 {
                    return None;
                }
                let mut decoder = flate2::read::ZlibDecoder::new(text_bytes);
                let mut text = String::new();
                use std::io::Read;
                decoder.read_to_string(&mut text).ok()?;
                return Some((keyword, text));
            }
            let text = String::from_utf8(text_bytes.to_vec()).ok()?;
            Some((keyword, text))
        }
        _ => None,
    }
}

fn try_parse_character_json(candidate: &str) -> Option<String> {
    let trimmed = candidate.trim();
    if trimmed.is_empty() {
        return None;
    }
    if serde_json::from_str::<Value>(trimmed).is_ok() {
        return Some(trimmed.to_string());
    }
    // Try base64 variants
    for engine in [
        &base64::engine::general_purpose::STANDARD,
        &base64::engine::general_purpose::STANDARD_NO_PAD,
        &base64::engine::general_purpose::URL_SAFE,
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
    ] {
        use base64::Engine as _;
        if let Ok(decoded) = engine.decode(trimmed) {
            if let Ok(text) = String::from_utf8(decoded) {
                if serde_json::from_str::<Value>(&text).is_ok() {
                    return Some(text);
                }
            }
        }
    }
    None
}

fn extract_character_json_from_png_bytes(data: &[u8]) -> Result<String, String> {
    const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
    if data.len() < PNG_SIGNATURE.len() || &data[..PNG_SIGNATURE.len()] != PNG_SIGNATURE {
        return Err("Invalid PNG file".to_string());
    }
    let mut candidates: Vec<(String, String)> = Vec::new();
    let mut offset = PNG_SIGNATURE.len();
    while offset + 12 <= data.len() {
        let length = u32::from_be_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ]) as usize;
        offset += 4;
        if offset + 8 > data.len() || offset + length + 8 > data.len() {
            return Err("Corrupted PNG metadata".to_string());
        }
        let chunk_type = std::str::from_utf8(&data[offset..offset + 4])
            .map_err(|_| "Corrupted PNG metadata".to_string())?;
        offset += 4;
        let chunk = &data[offset..offset + length];
        offset += length;
        offset += 4; // CRC
        if chunk_type == "IEND" {
            break;
        }
        if matches!(chunk_type, "tEXt" | "zTXt" | "iTXt") {
            if let Some((keyword, text)) = decode_png_text_chunk(chunk_type, chunk) {
                candidates.push((keyword, text));
            }
        }
    }
    for preferred in ["ccv3", "chara", "ccv2"] {
        for (keyword, text) in &candidates {
            if keyword.eq_ignore_ascii_case(preferred) {
                if let Some(parsed) = try_parse_character_json(text) {
                    return Ok(parsed);
                }
            }
        }
    }
    for (_, text) in &candidates {
        if let Some(parsed) = try_parse_character_json(text) {
            return Ok(parsed);
        }
    }
    Err("PNG does not contain a supported character card payload".to_string())
}

async fn fetch_chara_json_from_card_url(
    app: &AppHandle,
    card_url: &str,
    api_key: Option<&str>,
) -> Option<Value> {
    log_info(
        app,
        "chub_import",
        format!("Greetings missing — trying card PNG {}", card_url),
    );
    let bytes = match http_client(api_key).get(card_url).send().await {
        Ok(resp) if resp.status().is_success() => match resp.bytes().await {
            Ok(b) => b.to_vec(),
            Err(e) => {
                log_error(app, "chub_import", format!("Card PNG read failed: {}", e));
                return None;
            }
        },
        Ok(resp) => {
            log_error(
                app,
                "chub_import",
                format!("Card PNG fetch failed: {}", resp.status()),
            );
            return None;
        }
        Err(e) => {
            log_error(app, "chub_import", format!("Card PNG fetch error: {}", e));
            return None;
        }
    };
    let json_str = match extract_character_json_from_png_bytes(&bytes) {
        Ok(s) => s,
        Err(e) => {
            log_error(app, "chub_import", format!("Card PNG parse failed: {}", e));
            return None;
        }
    };
    match serde_json::from_str::<Value>(&json_str) {
        Ok(v) => Some(v),
        Err(e) => {
            log_error(app, "chub_import", format!("Card JSON parse failed: {}", e));
            None
        }
    }
}

/// Extract character nodes from a search payload regardless of whether the API
/// nests them under `data.nodes`, `nodes`, or returns a bare array in `data`.
fn extract_nodes(body: &Value) -> Vec<Value> {
    if let Some(Value::Array(items)) = body.get("nodes") {
        return items.clone();
    }
    if let Some(data) = body.get("data") {
        if let Some(Value::Array(items)) = data.get("nodes") {
            return items.clone();
        }
        if let Value::Array(items) = data {
            return items.clone();
        }
        if data.is_object() && data.get("id").is_some() {
            return vec![data.clone()];
        }
    }
    if let Value::Array(items) = body {
        return items.clone();
    }
    vec![]
}

fn extract_u64(body: &Value, keys: &[&str]) -> Option<u64> {
    for key in keys {
        if let Some(found) = u64_field(body, &[key]) {
            return Some(found);
        }
        if let Some(data) = body.get("data") {
            if let Some(found) = u64_field(data, &[key]) {
                return Some(found);
            }
        }
    }
    None
}

/// Extract the full-character node from a detail payload (`node`, root, or `data`).
fn extract_detail_node(body: &Value) -> Value {
    if let Some(node) = body.get("node") {
        if node.is_object() {
            return node.clone();
        }
    }
    if let Some(data) = body.get("data") {
        if data.is_object() && (data.get("id").is_some() || data.get("name").is_some()) {
            return data.clone();
        }
    }
    if body.is_object() && (body.get("id").is_some() || body.get("name").is_some()) {
        return body.clone();
    }
    Value::Null
}

/// Normalize a Chub fullPath (`author/character`) and reject anything suspicious.
fn sanitize_full_path(full_path: &str) -> Result<String, String> {
    let trimmed = full_path.trim().trim_matches('/');
    if trimmed.is_empty() || trimmed.contains("..") || trimmed.contains("://") {
        return Err(crate::utils::err_msg(
            module_path!(),
            line!(),
            "Invalid character path",
        ));
    }
    let segments: Vec<&str> = trimmed.split('/').collect();
    if segments.len() != 2 || segments.iter().any(|s| s.trim().is_empty()) {
        return Err(crate::utils::err_msg(
            module_path!(),
            line!(),
            "Invalid character path (expected author/character)",
        ));
    }
    Ok(trimmed.to_string())
}

async fn fetch_json(app: &AppHandle, url: &str, api_key: Option<&str>) -> Result<Value, String> {
    let resp = http_client(api_key)
        .get(url)
        .send()
        .await
        .map_err(|e| crate::utils::err_to_string(module_path!(), line!(), e))?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        let snippet: String = text.chars().take(200).collect();
        log_error(
            app,
            "chub",
            format!("GET {} failed: {} {}", url, status, snippet),
        );
        return Err(chub_error_message(status.as_u16()));
    }
    resp.json::<Value>()
        .await
        .map_err(|e| crate::utils::err_to_string(module_path!(), line!(), e))
}

/// Map a Chub HTTP status to a user-friendly message.
fn chub_error_message(status: u16) -> String {
    match status {
        429 => "Chub rate limit reached. Wait a moment and try again.".to_string(),
        401 | 403 => {
            "Chub refused this request. The character may be private or deleted.".to_string()
        }
        404 => {
            "Character not found on Chub (it may have been deleted or made private).".to_string()
        }
        400 => "Chub rejected this request as invalid.".to_string(),
        _ => format!("Chub request failed ({}). Please try again later.", status),
    }
}

#[tauri::command]
pub async fn chub_search_characters(
    app: AppHandle,
    query: Option<String>,
    tags: Option<Vec<String>>,
    page: Option<u32>,
    page_size: Option<u32>,
    sort: Option<String>,
    bypass_cache: Option<bool>,
    api_key: Option<String>,
) -> Result<ChubSearchResultDto, String> {
    let pure_mode = read_pure_mode_level(&app);
    let query_value = query
        .map(|q| q.trim().to_string())
        .filter(|q| !q.is_empty());
    let tags_value: Vec<String> = tags
        .unwrap_or_default()
        .into_iter()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect();
    let page_value = page.unwrap_or(1).max(1);
    let size_value = page_size.unwrap_or(15).clamp(1, 25);
    let sort_value = sort
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "trending".to_string());

    let using_api_key = api_key
        .as_deref()
        .map(|k| !k.trim().is_empty())
        .unwrap_or(false);
    let cache_key = format!(
        "chub:search:{}:{}:{}:{}:{}:auth{}",
        query_value.clone().unwrap_or_default(),
        tags_value.join(","),
        page_value,
        size_value,
        sort_value.clone(),
        using_api_key
    );
    if !bypass_cache.unwrap_or(false) {
        if let Some(cached) = cache_get::<ChubSearchResultDto>(&cache_key) {
            return Ok(cached);
        }
    }

    // Build request params (only documented parameters).
    let mut params: Vec<(String, String)> = vec![
        ("first".to_string(), size_value.to_string()),
        ("page".to_string(), page_value.to_string()),
        ("sort".to_string(), sort_value.clone()),
        ("namespace".to_string(), "characters".to_string()),
        ("nsfw".to_string(), (pure_mode == "off").to_string()),
        ("nsfw_only".to_string(), "false".to_string()),
        ("nsfl".to_string(), (pure_mode == "off").to_string()),
        ("include_forks".to_string(), "true".to_string()),
        ("inclusive_or".to_string(), "false".to_string()),
        ("asc".to_string(), "false".to_string()),
    ];
    if let Some(q) = query_value.clone() {
        params.push(("search".to_string(), q));
    }
    if !tags_value.is_empty() {
        params.push(("topics".to_string(), tags_value.join(",")));
    }

    let url = format!("{}/search", CHUB_GATEWAY_BASE);
    log_info(&app, "chub_search", format!("GET {} {:?}", url, params));

    let api_key_ref = api_key.as_deref();
    let resp = http_client(api_key_ref)
        .get(&url)
        .query(&params)
        .send()
        .await
        .map_err(|e| crate::utils::err_to_string(module_path!(), line!(), e))?;
    let status = resp.status();

    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        log_error(
            &app,
            "chub_search",
            format!("search failed: {} {}", status, text),
        );
        return Err(match status.as_u16() {
            429 => "Chub rate limit reached. Wait a moment and try again.".to_string(),
            401 | 403 => "Chub refused this request. The character may be private or deleted, or the API key may be invalid.".to_string(),
            _ => format!("Unable to load characters right now ({}). Please try again.", status),
        });
    }

    let body = resp
        .json::<Value>()
        .await
        .map_err(|e| crate::utils::err_to_string(module_path!(), line!(), e))?;

    let mut nodes = extract_nodes(&body);
    if pure_mode != "off" {
        nodes.retain(|node| !bool_field(node, &["nsfw"]).unwrap_or(false));
    }

    let total_pages = extract_u64(&body, &["totalPages", "total_pages"]).map(|v| v as u32);
    let total_nodes = extract_u64(&body, &["totalNodes", "total_nodes", "count"]);

    // Log what the public API actually exposes for this query so catalog
    // behavior is observable.
    log_info(
        &app,
        "chub_search",
        format!(
            "public API response: page={} nodes={} totalPages={:?} totalNodes={:?} query={:?} tags={:?} sort={}",
            page_value,
            nodes.len(),
            total_pages,
            total_nodes,
            query_value,
            tags_value,
            sort_value
        ),
    );

    let dto = ChubSearchResultDto {
        nodes,
        page: page_value,
        total_pages,
        total_nodes,
    };
    cache_set(cache_key, &dto, CHUB_SEARCH_TTL_SECS);
    Ok(dto)
}

#[tauri::command]
pub async fn chub_fetch_tags(
    app: AppHandle,
    search: Option<String>,
    limit: Option<u32>,
    bypass_cache: Option<bool>,
) -> Result<Vec<ChubTagInfo>, String> {
    let search_value = search
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let limit_value = limit.unwrap_or(100).clamp(1, 500);

    let cache_key = format!(
        "chub:tags:{}:{}",
        search_value.clone().unwrap_or_default(),
        limit_value
    );
    if !bypass_cache.unwrap_or(false) {
        if let Some(cached) = cache_get::<Vec<ChubTagInfo>>(&cache_key) {
            return Ok(cached);
        }
    }

    let mut params: Vec<(String, String)> = vec![
        ("first".to_string(), limit_value.to_string()),
        ("page".to_string(), "1".to_string()),
        ("sort".to_string(), "count_desc".to_string()),
        ("namespace".to_string(), "characters".to_string()),
    ];
    if let Some(s) = search_value.clone() {
        params.push(("search".to_string(), s));
    }

    let urls = [
        format!("{}/api/tags/characters", CHUB_GATEWAY_BASE),
        format!("{}/api/tags/character", CHUB_GATEWAY_BASE),
        format!("{}/api/tags", CHUB_GATEWAY_BASE),
        "https://api.chub.ai/api/tags/characters".to_string(),
        "https://api.chub.ai/api/tags/character".to_string(),
        "https://api.chub.ai/api/tags".to_string(),
    ];

    let mut body: Option<Value> = None;
    let client = http_client(None);
    for url in &urls {
        log_info(&app, "chub_tags", format!("fetching tags from {}", url));
        let resp = match client.get(url).query(&params).send().await {
            Ok(resp) => resp,
            Err(err) => {
                log_error(&app, "chub_tags", format!("tags request failed: {}", err));
                continue;
            }
        };
        if !resp.status().is_success() {
            log_error(
                &app,
                "chub_tags",
                format!("tags request failed: {}", resp.status()),
            );
            continue;
        }
        match resp.json::<Value>().await {
            Ok(json) => {
                body = Some(json);
                break;
            }
            Err(err) => {
                log_error(&app, "chub_tags", format!("tags parse failed: {}", err));
            }
        }
    }

    // Primary: parse the tag endpoint payload when one responded.
    if let Some(body) = body {
        let tags: Vec<ChubTagInfo> = extract_nodes(&body)
            .into_iter()
            .filter_map(|node| {
                let name = str_field(&node, &["name", "tag", "label"])?;
                let count = u64_field(&node, &["count", "n_characters", "usage_count", "total"]);
                Some(ChubTagInfo { name, count })
            })
            .collect();
        if !tags.is_empty() {
            cache_set(cache_key, &tags, CHUB_TAG_TTL_SECS);
            return Ok(tags);
        }
        log_error(
            &app,
            "chub_tags",
            "tag endpoints responded but contained no tags",
        );
    }

    // Fallback: derive tags from a broad live catalog slice so the selector
    // still works when the dedicated tag endpoints are unavailable.
    log_info(
        &app,
        "chub_tags",
        "tag endpoints unavailable, deriving tags from search catalog",
    );
    let derived = derive_tags_from_catalog(&app).await?;
    if derived.is_empty() {
        return Err(crate::utils::err_msg(
            module_path!(),
            line!(),
            "No tags available from Chub right now.",
        ));
    }
    cache_set(cache_key, &derived, CHUB_TAG_TTL_SECS);
    Ok(derived)
}

async fn derive_tags_from_catalog(app: &AppHandle) -> Result<Vec<ChubTagInfo>, String> {
    let params: Vec<(String, String)> = vec![
        ("first".to_string(), "100".to_string()),
        ("page".to_string(), "1".to_string()),
        ("sort".to_string(), "trending".to_string()),
        ("namespace".to_string(), "characters".to_string()),
        (
            "nsfw".to_string(),
            (read_pure_mode_level(app) == "off").to_string(),
        ),
        ("nsfw_only".to_string(), "false".to_string()),
        ("include_forks".to_string(), "true".to_string()),
        ("inclusive_or".to_string(), "false".to_string()),
        ("asc".to_string(), "false".to_string()),
    ];
    let url = format!("{}/search", CHUB_GATEWAY_BASE);
    let resp = http_client(None)
        .get(&url)
        .query(&params)
        .send()
        .await
        .map_err(|e| crate::utils::err_to_string(module_path!(), line!(), e))?;
    if !resp.status().is_success() {
        return Err(crate::utils::err_msg(
            module_path!(),
            line!(),
            "No tags available from Chub right now.",
        ));
    }
    let body = resp
        .json::<Value>()
        .await
        .map_err(|e| crate::utils::err_to_string(module_path!(), line!(), e))?;

    let mut counts: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    for node in extract_nodes(&body) {
        let mut tags = string_array_field(&node, "tags");
        tags.extend(string_array_field(&node, "topics"));
        for tag in tags {
            let key = tag.trim().to_string();
            if key.is_empty() {
                continue;
            }
            *counts.entry(key).or_insert(0) += 1;
        }
    }

    let mut tags: Vec<ChubTagInfo> = counts
        .into_iter()
        .map(|(name, count)| ChubTagInfo {
            name,
            count: Some(count),
        })
        .collect();
    tags.sort_by(|a, b| {
        b.count
            .unwrap_or(0)
            .cmp(&a.count.unwrap_or(0))
            .then_with(|| a.name.cmp(&b.name))
    });
    tags.truncate(100);
    Ok(tags)
}

#[tauri::command]
pub async fn chub_character_detail(
    app: AppHandle,
    full_path: String,
    api_key: Option<String>,
) -> Result<Value, String> {
    let full_path = sanitize_full_path(&full_path)?;
    let pure_mode = read_pure_mode_level(&app);

    let using_api_key = api_key
        .as_deref()
        .map(|k| !k.trim().is_empty())
        .unwrap_or(false);
    let cache_key = format!("chub:detail:{}:auth{}", full_path, using_api_key);
    if let Some(cached) = cache_get::<Value>(&cache_key) {
        if pure_mode != "off" && bool_field(&cached, &["nsfw"]).unwrap_or(false) {
            return Err(crate::utils::err_msg(
                module_path!(),
                line!(),
                "NSFW content is blocked in Pure Mode",
            ));
        }
        return Ok(cached);
    }

    let url = format!(
        "{}/api/characters/{}?full=true",
        CHUB_GATEWAY_BASE, full_path
    );
    log_info(&app, "chub_detail", format!("fetching detail from {}", url));

    let api_key_ref = api_key.as_deref();
    let body = fetch_json(&app, &url, api_key_ref).await?;
    let node = extract_detail_node(&body);
    if node.is_null() {
        return Err(crate::utils::err_msg(
            module_path!(),
            line!(),
            "Character not found or unsupported response from Chub.",
        ));
    }

    if pure_mode != "off" && bool_field(&node, &["nsfw"]).unwrap_or(false) {
        return Err(crate::utils::err_msg(
            module_path!(),
            line!(),
            "NSFW content is blocked in Pure Mode",
        ));
    }

    cache_set(cache_key, &node, CHUB_DETAIL_TTL_SECS);
    Ok(node)
}

fn find_imported_character_id(app: &AppHandle, full_path: &str) -> Result<Option<String>, String> {
    let characters = crate::storage_manager::characters::characters_list_typed::<Value>(app)
        .map_err(|e| {
            crate::utils::err_msg(
                module_path!(),
                line!(),
                format!("Failed to list characters: {}", e),
            )
        })?;
    for character in characters {
        if character.get("sourcePath").and_then(|v| v.as_str()) == Some(full_path) {
            if let Some(id) = character.get("id").and_then(|v| v.as_str()) {
                return Ok(Some(id.to_string()));
            }
        }
    }
    Ok(None)
}

#[tauri::command]
pub async fn chub_import_status(
    app: AppHandle,
    full_path: String,
) -> Result<Option<String>, String> {
    let full_path = sanitize_full_path(&full_path)?;
    find_imported_character_id(&app, &full_path)
}

#[tauri::command]
pub async fn chub_import_character(
    app: AppHandle,
    full_path: String,
    api_key: Option<String>,
) -> Result<String, String> {
    let full_path = match sanitize_full_path(&full_path) {
        Ok(path) => path,
        Err(err) => return Err(err),
    };

    let existing_id = find_imported_character_id(&app, &full_path);
    let existing = match existing_id {
        Ok(Some(id)) => Some(id),
        Ok(None) => None,
        Err(_) => None,
    };
    // If already imported but the saved character has no greeting scene (bug from earlier versions),
    // repair it by re-importing instead of skipping.
    let mut reuse_character_id: Option<String> = None;
    if let Some(existing_id) = existing {
        let needs_repair =
            match crate::storage_manager::characters::characters_list_typed::<Value>(&app) {
                Ok(list) => list
                    .iter()
                    .find(|c| c.get("id").and_then(|v| v.as_str()) == Some(existing_id.as_str()))
                    .map(|c| {
                        let scenes = c
                            .get("scenes")
                            .and_then(|v| v.as_array())
                            .map(|a| a.len())
                            .unwrap_or(0);
                        let has_content = c
                            .get("scenes")
                            .and_then(|v| v.as_array())
                            .and_then(|a| a.first())
                            .and_then(|s| s.get("content"))
                            .and_then(|v| v.as_str())
                            .map(|s| !s.trim().is_empty())
                            .unwrap_or(false);
                        let tags_empty = c
                            .get("tags")
                            .and_then(|v| v.as_array())
                            .map(|a| a.is_empty())
                            .unwrap_or(true);
                        scenes == 0 || !has_content || tags_empty
                    })
                    .unwrap_or(false),
                Err(_) => false,
            };
        if !needs_repair {
            log_info(
                &app,
                "chub_import",
                format!(
                    "Character {} already imported as {}",
                    full_path, existing_id
                ),
            );
            return Ok(existing_id);
        }
        log_info(
            &app,
            "chub_import",
            format!(
                "Existing character {} missing greetings — repairing",
                existing_id
            ),
        );
        reuse_character_id = Some(existing_id);
    }

    let node = chub_character_detail(app.clone(), full_path.clone(), api_key.clone()).await?;

    let mut name = str_field(&node, &["name"]).unwrap_or_else(|| "Unnamed Character".to_string());
    let mut tagline = str_field(&node, &["tagline"]);
    let mut description = str_field(&node, &["description"]);
    let mut personality = str_field(&node, &["personality"]);
    let mut scenario = str_field(&node, &["scenario"]);
    let mut first_mes = str_field(
        &node,
        &["first_mes", "firstMes", "greeting", "firstMessage"],
    );
    let mut mes_example = str_field(&node, &["mes_example", "mesExample", "example_dialogue"]);
    let mut creator_notes = str_field(&node, &["creator_notes", "creatorNotes"]);
    let mut system_prompt = str_field(&node, &["system_prompt", "systemPrompt"]);
    let mut post_history = str_field(
        &node,
        &["post_history_instructions", "postHistoryInstructions"],
    );
    let mut tags = {
        let t = string_array_field(&node, "tags");
        if !t.is_empty() {
            t
        } else {
            string_array_field(&node, "topics")
        }
    };

    let mut alternate_greetings: Vec<String> = vec![];
    if let Some(Value::Array(items)) = node
        .get("alternate_greetings")
        .or_else(|| node.get("alternateGreetings"))
    {
        for item in items {
            if let Some(text) = item.as_str() {
                if !text.trim().is_empty() {
                    alternate_greetings.push(text.to_string());
                }
            }
        }
    }

    // Fallback: many Chub search/detail responses are redacted (definition = null).
    // The real Tavern data lives inside the PNG card at max_res_url / maxResUrl.
    // If the primary greeting or tags are missing, try to pull them (and other missing fields) from the card.
    let mut card_book_fallback: Option<Value> = None;
    let needs_card_fallback = first_mes.is_none() || tags.is_empty();
    if needs_card_fallback {
        if let Some(card_url) = str_field(
            &node,
            &["max_res_url", "maxResUrl", "max_res_url", "charaCardUrl"],
        )
        .filter(|u| u.starts_with("http://") || u.starts_with("https://"))
        {
            if let Some(card_val) =
                fetch_chara_json_from_card_url(&app, &card_url, api_key.as_deref()).await
            {
                // Card may be spec/data wrapped (chara_card_v2/v3) or raw tavern object
                let data_obj: &Value =
                    if card_val.get("spec").is_some() && card_val.get("data").is_some() {
                        card_val.get("data").unwrap_or(&card_val)
                    } else {
                        &card_val
                    };
                // Fill only what was missing — don't overwrite what we already have from API
                if name == "Unnamed Character" {
                    if let Some(v) = str_field(data_obj, &["name"]) {
                        name = v;
                    }
                }
                if tagline.is_none() {
                    tagline = str_field(data_obj, &["tagline"]);
                }
                if description.is_none() {
                    description = str_field(data_obj, &["description"]);
                }
                if personality.is_none() {
                    personality = str_field(data_obj, &["personality"]);
                }
                if scenario.is_none() {
                    scenario = str_field(data_obj, &["scenario"]);
                }
                if first_mes.is_none() {
                    first_mes = str_field(
                        data_obj,
                        &["first_mes", "firstMes", "greeting", "firstMessage"],
                    );
                }
                if mes_example.is_none() {
                    mes_example = str_field(data_obj, &["mes_example", "mesExample"]);
                }
                if creator_notes.is_none() {
                    creator_notes = str_field(data_obj, &["creator_notes", "creatorNotes"]);
                }
                if system_prompt.is_none() {
                    system_prompt = str_field(data_obj, &["system_prompt", "systemPrompt"]);
                }
                if post_history.is_none() {
                    post_history = str_field(
                        data_obj,
                        &["post_history_instructions", "postHistoryInstructions"],
                    );
                }
                if tags.is_empty() {
                    let mut card_tags = string_array_field(data_obj, "tags");
                    if card_tags.is_empty() {
                        card_tags = string_array_field(data_obj, "topics");
                    }
                    if !card_tags.is_empty() {
                        tags = card_tags;
                    }
                }
                if alternate_greetings.is_empty() {
                    if let Some(Value::Array(items)) = data_obj
                        .get("alternate_greetings")
                        .or_else(|| data_obj.get("alternateGreetings"))
                    {
                        for item in items {
                            if let Some(text) = item.as_str() {
                                if !text.trim().is_empty() {
                                    alternate_greetings.push(text.to_string());
                                }
                            }
                        }
                    }
                }
                if node.get("character_book").is_none() {
                    if let Some(book) = data_obj.get("character_book").cloned() {
                        card_book_fallback = Some(book);
                    }
                }
                if first_mes.is_some() || !alternate_greetings.is_empty() {
                    log_info(
                        &app,
                        "chub_import",
                        "Recovered greetings from card PNG fallback",
                    );
                }
            }
        }
    }

    let avatar_url = str_field(&node, &["avatarUrl", "avatar_url", "avatar"])
        .filter(|u| u.starts_with("http://") || u.starts_with("https://"));

    let is_repair = reuse_character_id.is_some();
    let character_id = reuse_character_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let avatar_entity_id = format!("character-{}", character_id);

    let mut avatar_path: Option<String> = None;
    if let Some(url) = avatar_url {
        log_info(
            &app,
            "chub_import",
            format!("Downloading avatar from {}", url),
        );
        match http_client(api_key.as_deref()).get(&url).send().await {
            Ok(resp) if resp.status().is_success() => match resp.bytes().await {
                Ok(bytes) => {
                    let avatar_base64 =
                        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes);
                    match storage_save_avatar(
                        app.clone(),
                        avatar_entity_id.clone(),
                        avatar_base64,
                        None,
                        None,
                    ) {
                        Ok(path) => {
                            if let Err(err) = generate_avatar_gradient(
                                app.clone(),
                                avatar_entity_id,
                                "avatar_base.webp".into(),
                                Some(false),
                                Some("round".into()),
                            ) {
                                log_error(
                                    &app,
                                    "chub_import",
                                    format!("Failed to generate avatar gradient: {}", err),
                                );
                            }
                            avatar_path = Some(path);
                        }
                        Err(err) => {
                            log_error(
                                &app,
                                "chub_import",
                                format!("Failed to save avatar: {}", err),
                            );
                        }
                    }
                }
                Err(err) => {
                    log_error(
                        &app,
                        "chub_import",
                        format!("Failed to read avatar: {}", err),
                    );
                }
            },
            Ok(resp) => {
                log_error(
                    &app,
                    "chub_import",
                    format!("Avatar download failed: {}", resp.status()),
                );
            }
            Err(err) => {
                log_error(
                    &app,
                    "chub_import",
                    format!("Avatar download failed: {}", err),
                );
            }
        }
    }

    let now = chrono::Utc::now().timestamp_millis();
    let memory_type = if super::is_dynamic_memory_enabled(&app) {
        "dynamic"
    } else {
        "manual"
    };

    let mut scenes = vec![];
    if let Some(first_msg) = first_mes.clone() {
        let scene_id = uuid::Uuid::new_v4().to_string();
        scenes.push(serde_json::json!({
            "id": scene_id,
            "content": first_msg,
            "createdAt": now,
            "variants": []
        }));
    }
    for alt in &alternate_greetings {
        let scene_id = uuid::Uuid::new_v4().to_string();
        scenes.push(serde_json::json!({
            "id": scene_id,
            "content": alt,
            "createdAt": now,
            "variants": []
        }));
    }

    let mut definition_parts = vec![];
    super::push_definition_block(&mut definition_parts, None, description.clone());
    super::push_definition_block(
        &mut definition_parts,
        Some("Personality"),
        personality.clone(),
    );
    super::push_definition_block(&mut definition_parts, Some("Scenario"), scenario.clone());
    super::push_definition_block(
        &mut definition_parts,
        Some("System Prompt"),
        system_prompt.clone(),
    );
    super::push_definition_block(
        &mut definition_parts,
        Some("Post History Instructions"),
        post_history.clone(),
    );
    super::push_definition_block(
        &mut definition_parts,
        Some("Creator Notes"),
        creator_notes.clone(),
    );
    super::push_definition_block(
        &mut definition_parts,
        None,
        mes_example.clone().map(|examples| {
            format!(
                "<example_dialogue>\n{}\n</example_dialogue>",
                examples.trim()
            )
        }),
    );
    let definition = if definition_parts.is_empty() {
        None
    } else {
        Some(definition_parts.join("\n\n"))
    };

    let source_url = format!("https://chub.ai/characters/{}", full_path);
    let source_project_id = str_field(&node, &["id"]);

    let character = serde_json::json!({
        "id": character_id,
        "name": name.clone(),
        "description": tagline.clone().or(description.clone()).unwrap_or_default(),
        "definition": definition,
        "avatarPath": avatar_path,
        "backgroundImagePath": null,
        "rules": [],
        "defaultSceneId": if !scenes.is_empty() { scenes[0]["id"].as_str() } else { None },
        "defaultModelId": null,
        "memoryType": memory_type,
        "promptTemplateId": null,
        "voiceConfig": null,
        "voiceAutoplay": false,
        "disableAvatarGradient": avatar_path.is_none(),
        "customGradientEnabled": false,
        "customGradientColors": null,
        "customTextColor": null,
        "customTextSecondary": null,
        "scenes": scenes,
        "tags": tags,
        "source": "chub",
        "sourcePath": full_path.clone(),
        "sourceUrl": source_url,
        "sourceProjectId": source_project_id,
        "importedAt": now,
        "createdAt": now,
        "updatedAt": now
    });

    let _: Value = crate::storage_manager::characters::character_upsert_typed(&app, &character)
        .map_err(|e| {
            crate::utils::err_msg(
                module_path!(),
                line!(),
                format!("Character download failed while saving: {}", e),
            )
        })?;

    let book_to_import = node.get("character_book").or(card_book_fallback.as_ref());
    if let Some(book) = book_to_import {
        if let Err(err) = import_character_book(&app, &character_id, &name, book) {
            log_error(
                &app,
                "chub_import",
                format!("Failed to import character book: {}", err),
            );
        }
    }

    // If this was a repair of a previously broken import, the existing empty session(s)
    // need their starting scene message backfilled so the chat isn't blank.
    if is_repair {
        if let Some(greeting) = first_mes.clone().filter(|s| !s.trim().is_empty()) {
            if let Err(err) = backfill_empty_sessions_greeting(&app, &character_id, &greeting) {
                log_error(
                    &app,
                    "chub_import",
                    format!("Greeting backfill failed: {}", err),
                );
            }
        }
    }

    log_info(
        &app,
        "chub_import",
        format!(
            "Successfully imported chub character {} as {}",
            full_path, character_id
        ),
    );

    Ok(character_id)
}

fn backfill_empty_sessions_greeting(
    app: &AppHandle,
    character_id: &str,
    greeting: &str,
) -> Result<(), String> {
    let conn = crate::storage_manager::db::open_db(app)
        .map_err(|e| crate::utils::err_to_string(module_path!(), line!(), e))?;
    let mut stmt = conn
        .prepare("SELECT id FROM sessions WHERE character_id = ?1")
        .map_err(|e| crate::utils::err_to_string(module_path!(), line!(), e))?;
    let session_ids: Vec<String> = stmt
        .query_map(rusqlite::params![character_id], |r| r.get(0))
        .map_err(|e| crate::utils::err_to_string(module_path!(), line!(), e))?
        .filter_map(|r| r.ok())
        .collect();
    for sid in session_ids {
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE session_id = ?1",
                rusqlite::params![sid],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if count != 0 {
            continue;
        }
        let msg_id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().timestamp_millis();
        conn.execute(
            "INSERT INTO messages (id, session_id, role, content, created_at, visible_in_chat, scene_edited, is_pinned) VALUES (?1, ?2, 'scene', ?3, ?4, 1, 0, 0)",
            rusqlite::params![msg_id, sid, greeting, now],
        )
        .map_err(|e| crate::utils::err_to_string(module_path!(), line!(), e))?;
        conn.execute(
            "UPDATE sessions SET updated_at = ?1 WHERE id = ?2",
            rusqlite::params![now, sid],
        )
        .map_err(|e| crate::utils::err_to_string(module_path!(), line!(), e))?;
        log_info(
            app,
            "chub_import",
            format!("Backfilled greeting into empty session {}", sid),
        );
    }
    Ok(())
}

fn import_character_book(
    app: &AppHandle,
    character_id: &str,
    character_name: &str,
    book: &Value,
) -> Result<(), String> {
    let entries_value = match book.get("entries") {
        Some(Value::Array(items)) => items.clone(),
        _ => return Ok(()),
    };
    if entries_value.is_empty() {
        return Ok(());
    }

    let mut conn = crate::storage_manager::db::open_db(app).map_err(|e| {
        crate::utils::err_msg(
            module_path!(),
            line!(),
            format!("Failed to open database for lorebook import: {}", e),
        )
    })?;

    let now = chrono::Utc::now().timestamp_millis();
    let lorebook_id = uuid::Uuid::new_v4().to_string();
    let lorebook_name = match str_field(book, &["name"]) {
        Some(name) if !name.trim().is_empty() => name,
        _ => format!("{} Lorebook", character_name),
    };

    let lorebook_record = Lorebook {
        id: lorebook_id.clone(),
        name: lorebook_name,
        avatar_path: None,
        keyword_detection_mode:
            crate::storage_manager::lorebook::LorebookKeywordDetectionMode::RecentMessageWindow,
        created_at: now,
        updated_at: now,
    };

    upsert_lorebook(&conn, &lorebook_record)?;

    for (index, entry) in entries_value.iter().enumerate() {
        let content = str_field(entry, &["content"]).unwrap_or_default();
        if content.trim().is_empty() {
            continue;
        }

        let mut keys: Vec<String> = vec![];
        match entry.get("keys") {
            Some(Value::Array(items)) => {
                for item in items {
                    if let Some(key) = item.as_str() {
                        if !key.trim().is_empty() {
                            keys.push(key.to_string());
                        }
                    }
                }
            }
            Some(Value::String(single)) if !single.trim().is_empty() => {
                keys.push(single.clone());
            }
            _ => {}
        }

        let title = str_field(entry, &["name", "comment", "title"])
            .unwrap_or_else(|| format!("Entry {}", index + 1));
        let enabled = bool_field(entry, &["enabled"]).unwrap_or(true);
        let constant = bool_field(entry, &["constant"]).unwrap_or(false);
        let insertion_order = u64_field(entry, &["insertion_order", "insertionOrder"]);
        let display_order = insertion_order
            .and_then(|value| i32::try_from(value).ok())
            .unwrap_or(index as i32);
        let always_active = constant && keys.is_empty();

        let entry_record = LorebookEntry {
            id: uuid::Uuid::new_v4().to_string(),
            lorebook_id: lorebook_id.clone(),
            title,
            enabled,
            always_active,
            keywords: keys,
            case_sensitive: bool_field(entry, &["case_sensitive", "caseSensitive"])
                .unwrap_or(false),
            keyword_match_mode: crate::storage_manager::lorebook::LorebookKeywordMatchMode::Literal,
            content,
            priority: 0,
            display_order,
            created_at: now,
            updated_at: now,
        };

        upsert_lorebook_entry(&conn, &entry_record)?;
    }

    set_character_lorebooks(&mut conn, character_id, std::slice::from_ref(&lorebook_id))?;

    Ok(())
}
