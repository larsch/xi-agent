use crate::llm::{copilot::CopilotProvider, ollama::OllamaProvider};

/// Return the context-window size (in tokens) for a known model name.
///
/// Checks the Copilot model metadata cache first (populated by
/// `CopilotProvider::list_models()`), then falls back to a hard-coded table
/// for other providers and for Copilot models used before `list_models()` has
/// been called.
///
/// Returns `None` for unrecognised models.
pub fn context_window_for_model(model: &str) -> Option<usize> {
    // Primary: live metadata from the Copilot /models API.
    if let Some(cw) = CopilotProvider::cached_context_window(model) {
        return Some(cw);
    }

    // Secondary: live metadata fetched by the Ollama /api/show endpoint
    // (covers both Ollama and Open WebUI providers).
    if let Some(cw) = OllamaProvider::cached_context_window(model) {
        return Some(cw);
    }

    // Fallback: hard-coded table for all other providers and cold-start.
    // Normalise to lowercase for matching.
    let m = model.to_ascii_lowercase();
    // Check prefixes / substrings for common model families.
    if m.starts_with("o3-mini") {
        return Some(200_000);
    }
    if m.starts_with("o3") {
        return Some(200_000);
    }
    if m.starts_with("o1-mini") {
        return Some(128_000);
    }
    if m.starts_with("o1") {
        return Some(200_000);
    }
    if m.starts_with("gpt-4o") {
        return Some(128_000);
    }
    if m.starts_with("gpt-4-turbo") {
        return Some(128_000);
    }
    if m.starts_with("gpt-4") {
        return Some(8_192);
    }
    if m.starts_with("gpt-3.5-turbo") {
        return Some(16_385);
    }
    if m.starts_with("gpt-5") {
        return Some(200_000);
    }
    if m.contains("gemini") {
        return Some(1_000_000);
    }
    if m.contains("claude-3-5") || m.contains("claude-3.5") {
        return Some(200_000);
    }
    if m.contains("claude-3") {
        return Some(200_000);
    }
    if m.contains("claude-2") {
        return Some(100_000);
    }
    // Claude 4+ models have a 1M context window.
    // (e.g. claude-sonnet-4.5, claude-sonnet-4-6, claude-opus-4, …).
    if m.contains("claude-") {
        return Some(1_000_000);
    }
    if m.contains("llama3") {
        return Some(128_000);
    }
    if m.contains("llama2") {
        return Some(4_096);
    }
    if m.contains("mistral") {
        return Some(32_000);
    }
    if m.contains("gemma") {
        return Some(8_192);
    }
    None
}

/// Compute a scaled token budget using a square-root curve, capped at `window / 3`.
///
/// `f(w) = min(w/3, floor + scale * sqrt(w / 200_000))`
///
/// This gives good utilisation of large context windows while keeping small
/// models usable (the `w/3` cap kicks in below ~200k).
///
/// Intended uses:
/// - `max_tokens` (output budget):   floor = 8_000, scale = 8_000
/// - `reserve_tokens` (compaction):  floor = 16_000, scale = 16_000
pub fn scaled_token_budget(context_window: usize, floor: usize, scale: usize) -> usize {
    let ratio = context_window as f64 / 200_000.0_f64;
    let value = floor as f64 + scale as f64 * ratio.sqrt();
    let cap = context_window / 3;
    (value as usize).min(cap).max(1)
}

#[cfg(test)]
mod tests {
    use super::{context_window_for_model, scaled_token_budget};
    use crate::llm::copilot::test_helpers;

    // ── Cache-driven context window ──────────────────────────────────────────

    #[test]
    fn context_window_prefers_cache_over_hard_coded_table() {
        // Seed cache with a non-standard size to prove it wins over the table.
        test_helpers::insert_cache("__gpt_4o_cache_test__", "Azure OpenAI", Some(999_999));
        assert_eq!(
            context_window_for_model("__gpt_4o_cache_test__"),
            Some(999_999)
        );
    }

    #[test]
    fn context_window_falls_back_to_table_when_cache_miss() {
        // gpt-4o is not seeded with this exact key — falls back to hard-coded table.
        assert_eq!(context_window_for_model("gpt-4o"), Some(128_000));
    }

    #[test]
    fn context_window_falls_back_to_table_when_cache_has_no_limit() {
        test_helpers::insert_cache("__no_limit_model__", "OpenAI", None);
        // Cache entry exists but carries no context-window value; table has no
        // match either → None.
        assert_eq!(context_window_for_model("__no_limit_model__"), None);
    }

    #[test]
    fn context_window_claude4_returns_1m() {
        assert_eq!(
            context_window_for_model("claude-sonnet-4-6"),
            Some(1_000_000)
        );
        assert_eq!(
            context_window_for_model("claude-sonnet-4.5"),
            Some(1_000_000)
        );
        assert_eq!(context_window_for_model("claude-opus-4"), Some(1_000_000));
    }

    #[test]
    fn context_window_claude3_unchanged() {
        assert_eq!(context_window_for_model("claude-3-5-sonnet"), Some(200_000));
        assert_eq!(context_window_for_model("claude-3-opus"), Some(200_000));
    }

    // ── scaled_token_budget ──────────────────────────────────────────────────

    #[test]
    fn scaled_token_budget_at_reference_window() {
        // At 200k: floor + scale * sqrt(1.0) = floor + scale
        assert_eq!(scaled_token_budget(200_000, 8_000, 8_000), 16_000);
        assert_eq!(scaled_token_budget(200_000, 16_000, 16_000), 32_000);
    }

    #[test]
    fn scaled_token_budget_at_1m() {
        // sqrt(1_000_000 / 200_000) = sqrt(5) ≈ 2.236
        // max_tokens: 8000 + 8000 * 2.236 = ~25_889, cap = 333_333 → ~25_889
        let mt = scaled_token_budget(1_000_000, 8_000, 8_000);
        assert!(mt > 24_000 && mt < 27_000, "max_tokens at 1M: {mt}");
        // reserve: 16000 + 16000 * 2.236 = ~51_778, cap = 333_333 → ~51_778
        let rv = scaled_token_budget(1_000_000, 16_000, 16_000);
        assert!(rv > 50_000 && rv < 53_000, "reserve at 1M: {rv}");
    }

    #[test]
    fn scaled_token_budget_cap_applies_on_small_window() {
        // 8k window: formula gives floor=8000 which exceeds 8192/3=2730 → cap wins
        let mt = scaled_token_budget(8_192, 8_000, 8_000);
        assert_eq!(mt, 8_192 / 3);
    }

    #[test]
    fn scaled_token_budget_at_128k() {
        // sqrt(128_000 / 200_000) = sqrt(0.64) = 0.8
        // max_tokens: 8000 + 8000 * 0.8 = 14_400, cap = 42_666 → 14_400
        let mt = scaled_token_budget(128_000, 8_000, 8_000);
        assert!(mt > 13_500 && mt < 15_000, "max_tokens at 128k: {mt}");
    }
}
