use crate::storage_manager::db::DbConnection;
use crate::storage_manager::lorebook::{
    get_character_active_lorebook_ids, get_enabled_lorebook_entry_contexts_for_ids, LorebookEntry,
    LorebookEntryActivationContext, LorebookKeywordDetectionMode, LorebookKeywordMatchMode,
};
use crate::chat_manager::prompting::context_window::estimate_tokens_for_text;
use crate::infra::utils::{log_error, log_info};
use tauri::AppHandle;

/// Simple keyword matching with literal mode (case-insensitive by default).
/// This is a convenience wrapper around `keyword_matches_with_mode`.
pub fn keyword_matches(keyword: &str, text: &str, case_sensitive: bool) -> bool {
    keyword_matches_with_mode(keyword, text, case_sensitive, LorebookKeywordMatchMode::Literal)
}

fn contains_unsegmented_script(text: &str) -> bool {
    text.chars().any(|ch| {
        matches!(
            ch as u32,
            0x0E00..=0x0E7F | // Thai
            0x0E80..=0x0EFF | // Lao
            0x1000..=0x109F | // Myanmar
            0x1780..=0x17FF | // Khmer
            0x3040..=0x30FF | // Hiragana and Katakana
            0x3400..=0x4DBF | // CJK Extension A
            0x4E00..=0x9FFF | // CJK Unified Ideographs
            0xAC00..=0xD7AF | // Hangul syllables
            0xF900..=0xFAFF   // CJK Compatibility Ideographs
        )
    })
}

pub(crate) fn keyword_matches_with_mode(
    keyword: &str,
    text: &str,
    case_sensitive: bool,
    mode: LorebookKeywordMatchMode,
) -> bool {
    let keyword = keyword.trim();
    if keyword.is_empty() {
        return false;
    }

    let normalize = |s: &str| -> String {
        s.chars()
            .map(|c| {
                if c.is_alphanumeric() || c.is_whitespace() {
                    c
                } else {
                    ' '
                }
            })
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    };

    let (search_keyword, search_text) = if case_sensitive {
        (keyword.to_string(), text.to_string())
    } else {
        (keyword.to_lowercase(), text.to_lowercase())
    };

    if mode == LorebookKeywordMatchMode::Regex {
        return regex::RegexBuilder::new(&search_keyword)
            .case_insensitive(!case_sensitive)
            .build()
            .map(|regex| regex.is_match(&search_text))
            .unwrap_or(false);
    }

    if search_keyword.ends_with('*') {
        let prefix = &search_keyword[..search_keyword.len() - 1];
        if prefix.is_empty() {
            return false;
        }

        let normalized_text = normalize(&search_text);

        if contains_unsegmented_script(prefix) || contains_unsegmented_script(&normalized_text) {
            return normalized_text.contains(prefix);
        }

        for word in normalized_text.split_whitespace() {
            if word.starts_with(prefix) {
                return true;
            }
        }
        return false;
    }

    let normalized_keyword = normalize(&search_keyword);
    let normalized_text = normalize(&search_text);

    if contains_unsegmented_script(&normalized_keyword)
        || contains_unsegmented_script(&normalized_text)
    {
        return normalized_text.contains(&normalized_keyword);
    }

    if normalized_keyword.contains(' ') {
        return normalized_text.contains(&normalized_keyword);
    }

    let text_words: Vec<&str> = normalized_text.split_whitespace().collect();
    text_words.iter().any(|word| *word == normalized_keyword)
}

pub fn activate_lorebook_entries(
    entries: Vec<LorebookEntryActivationContext>,
    recent_messages: &[String],
    latest_user_message: Option<&str>,
) -> Vec<LorebookEntry> {
    if entries.is_empty() {
        return vec![];
    }
    let recent_context = recent_messages.join("\n");
    let latest_user_context = latest_user_message.unwrap_or_default();

    let mut active_entries: Vec<LorebookEntry> = vec![];

    for entry_context in entries {
        let entry = entry_context.entry;
        let keyword_context = match entry_context.keyword_detection_mode {
            LorebookKeywordDetectionMode::RecentMessageWindow => recent_context.as_str(),
            LorebookKeywordDetectionMode::LatestUserMessage => latest_user_context,
        };

        let should_activate = if entry.always_active {
            true
        } else if entry.keywords.is_empty() {
            false
        } else {
            entry.keywords.iter().any(|keyword| {
                keyword_matches_with_mode(
                    keyword,
                    keyword_context,
                    entry.case_sensitive,
                    entry.keyword_match_mode,
                )
            })
        };

        if should_activate {
            active_entries.push(entry);
        }
    }

    active_entries.sort_by(|a, b| {
        a.display_order
            .cmp(&b.display_order)
            .then_with(|| a.created_at.cmp(&b.created_at))
    });

    active_entries
}

/// Calculate the lorebook budget based on the resolved context length.
/// Returns the maximum number of tokens that can be used for lorebook entries.
pub fn calculate_lorebook_budget(context_limit: u32) -> u32 {
    // Default to 25% of the context length, similar to SillyTavern
    // Minimum budget of 256 tokens, maximum of 32768 tokens
    let budget = (context_limit as f32 * 0.25).ceil() as u32;
    budget.clamp(256, 32768)
}

/// Select lorebook entries that fit within the given token budget.
/// Returns entries that fit within the budget, sorted by priority/display_order.
/// Does not truncate entries - skips entries that don't fit.
pub fn select_lorebook_entries_with_budget(
    app: &AppHandle,
    mut active_entries: Vec<LorebookEntry>,
    budget_tokens: u32,
) -> Vec<LorebookEntry> {
    if active_entries.is_empty() || budget_tokens == 0 {
        return vec![];
    }

    // Sort by priority (higher first), then by display_order, then by created_at
    active_entries.sort_by(|a, b| {
        b.priority
            .cmp(&a.priority)
            .then_with(|| a.display_order.cmp(&b.display_order))
            .then_with(|| a.created_at.cmp(&b.created_at))
    });

    let mut selected = Vec::new();
    let mut used_tokens = 0u32;

    for entry in active_entries {
        if !entry.enabled {
            continue;
        }
        let content = entry.content.trim();
        if content.is_empty() {
            continue;
        }

        // Estimate tokens for this entry's content
        // We add some overhead for the formatting (e.g., "[Lorebook / Entry]\n\n")
        let entry_tokens = estimate_tokens_for_text(content).saturating_add(32);

        if used_tokens.saturating_add(entry_tokens) <= budget_tokens {
            used_tokens = used_tokens.saturating_add(entry_tokens);
            log_info(
                app,
                "lorebook_budget",
                format!(
                    "selected entry: id={} title='{}' tokens={} used={}/{}",
                    entry.id, entry.title, entry_tokens, used_tokens, budget_tokens
                ),
            );
            selected.push(entry);
        } else {
            log_info(
                app,
                "lorebook_budget",
                format!(
                    "skipped entry: id={} title='{}' tokens={} would_exceed_budget ({}/{})",
                    entry.id, entry.title, entry_tokens, used_tokens.saturating_add(entry_tokens), budget_tokens
                ),
            );
        }
    }

    selected
}

/// Select lorebook entries with budget based on context limit.
/// This is the main entry point that combines activation and budgeting.
pub fn select_lorebook_entries_with_budget_for_context(
    app: &AppHandle,
    conn: &DbConnection,
    lorebook_ids: &[String],
    recent_messages: &[String],
    latest_user_message: Option<&str>,
    context_limit: u32,
) -> Result<Vec<LorebookEntry>, String> {
    let budget = calculate_lorebook_budget(context_limit);
    let active_entries = get_active_lorebook_entries_for_ids(conn, lorebook_ids, recent_messages, latest_user_message)?;
    Ok(select_lorebook_entries_with_budget(app, active_entries, budget))
}

pub fn get_active_lorebook_entries(
    conn: &DbConnection,
    character_id: &str,
    recent_messages: &[String],
    latest_user_message: Option<&str>,
) -> Result<Vec<LorebookEntry>, String> {
    let lorebook_ids = get_character_active_lorebook_ids(conn, character_id)?;
    get_active_lorebook_entries_for_ids(conn, &lorebook_ids, recent_messages, latest_user_message)
}

pub fn get_active_lorebook_entries_for_ids(
    conn: &DbConnection,
    lorebook_ids: &[String],
    recent_messages: &[String],
    latest_user_message: Option<&str>,
) -> Result<Vec<LorebookEntry>, String> {
    let entries = get_enabled_lorebook_entry_contexts_for_ids(conn, lorebook_ids)?;
    Ok(activate_lorebook_entries(
        entries,
        recent_messages,
        latest_user_message,
    ))
}

pub fn format_lorebook_for_prompt(entries: &[LorebookEntry]) -> String {
    if entries.is_empty() {
        return String::new();
    }

    entries
        .iter()
        .map(|entry| entry.content.trim())
        .filter(|content| !content.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::{keyword_matches, keyword_matches_with_mode};
    use crate::storage_manager::lorebook::LorebookKeywordMatchMode;

    #[test]
    fn literal_keywords_match_text_without_word_breaks() {
        assert!(keyword_matches("東京", "今日は東京へ行く", false));
        assert!(keyword_matches("北京", "我住在北京。", false));
        assert!(keyword_matches("กรุงเทพ", "ฉันอยู่กรุงเทพมหานคร", false));
    }

    #[test]
    fn literal_keywords_preserve_word_boundaries_for_spaced_languages() {
        assert!(!keyword_matches("art", "We are going to a party", false));
        assert!(keyword_matches("art", "We made art today", false));
    }

    #[test]
    fn regex_keywords_are_opt_in() {
        assert!(keyword_matches_with_mode(
            r"東京|大阪",
            "今日は大阪へ行く",
            false,
            LorebookKeywordMatchMode::Regex,
        ));
        assert!(!keyword_matches_with_mode(
            r"東京|大阪",
            "今日は名古屋へ行く",
            false,
            LorebookKeywordMatchMode::Regex,
        ));
    }
}