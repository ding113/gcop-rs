//! Context-window estimation and char budget for history-prompt injection.
//!
//! The LLM has a finite context window; injecting historical commits eats
//! into that window. This module computes a *soft* character budget for the
//! history block based on either an explicit cap in config or a fraction of
//! the model's context window.
//!
//! All functions are pure; no IO, no LLM calls.

use crate::config::{HistoryRefConfig, ProviderConfig};

/// Fallback context window when neither config nor the lookup table knows.
///
/// Set conservatively to 16K so unknown small local models (Mistral 8K,
/// Phi-3 mini, etc.) don't end up with a history-injection budget that
/// exceeds their actual context window. Users with larger custom models
/// should set `[llm.providers.<name>] context_window = ...` explicitly.
const DEFAULT_CONTEXT_WINDOW: usize = 16_000;

/// Default chars-per-token heuristic used when [`HistoryRefConfig::chars_per_token`]
/// is unset. 3.0 is a compromise between English/code (~4) and CJK (~1.5);
/// CJK-heavy repos should set this to `1.5` in `.gcop/config.toml`, code-heavy
/// repos to `4.0`.
const DEFAULT_CHARS_PER_TOKEN: f32 = 3.0;

/// Substring-keyed model → context-window table. The first match wins, so
/// list more-specific keys before more-general ones.
///
/// **Ordering is load-bearing.** `gpt-4.1` MUST precede `gpt-4`, and modern
/// Claude variants (e.g. `claude-3-5-sonnet`) MUST precede the generic
/// `claude-sonnet` so substring matches resolve to the most specific entry.
const KNOWN_MODEL_CONTEXTS: &[(&str, usize)] = &[
    // Claude 4.x family (200K)
    ("claude-haiku-4", 200_000),
    ("claude-sonnet-4", 200_000),
    ("claude-opus-4", 200_000),
    // Claude 3.x family — explicit names match real model IDs like
    // "claude-3-haiku-20240307" / "claude-3-5-sonnet-20241022".
    ("claude-3-5-haiku", 200_000),
    ("claude-3-5-sonnet", 200_000),
    ("claude-3-opus", 200_000),
    ("claude-3-sonnet", 200_000),
    ("claude-3-haiku", 200_000),
    // Generic Claude families (broad fallback for unrecognised variants)
    ("claude-haiku", 200_000),
    ("claude-sonnet", 200_000),
    ("claude-opus", 200_000),
    // GPT-5 family (assume 400K — adjust when public docs settle)
    ("gpt-5", 400_000),
    // GPT-4.1 / 4o / 4-turbo before bare gpt-4 so 'gpt-4.1' doesn't
    // substring-match 'gpt-4' (which would cap it at 8K).
    ("gpt-4.1", 1_000_000),
    ("gpt-4o", 128_000),
    ("gpt-4-turbo", 128_000),
    ("gpt-4", 8_192),
    ("gpt-3.5", 16_000),
    // Gemini family
    ("gemini-1.5", 1_000_000),
    ("gemini-2", 1_000_000),
    ("gemini-3", 1_000_000),
    ("gemini-flash", 1_000_000),
    // Open-source models
    ("llama3", 8_192),
];

/// Returns the effective context window (in tokens) for the given provider.
///
/// Resolution order: explicit `provider.context_window` → substring match in
/// `KNOWN_MODEL_CONTEXTS` against the lowercased model name → fallback to
/// [`DEFAULT_CONTEXT_WINDOW`].
#[allow(dead_code)] // consumed by sampler in Iteration C
pub(crate) fn model_context_window(provider: &ProviderConfig) -> usize {
    if let Some(explicit) = provider.context_window {
        return explicit;
    }
    let lower = provider.model.to_ascii_lowercase();
    for (needle, window) in KNOWN_MODEL_CONTEXTS {
        if lower.contains(needle) {
            return *window;
        }
    }
    DEFAULT_CONTEXT_WINDOW
}

/// Returns the character budget allocated to the history block.
///
/// If `cfg.max_chars` is set, that value is returned verbatim. Otherwise the
/// budget is `context_window_tokens × chars_per_token × cfg.max_chars_ratio`,
/// where `chars_per_token` is taken from [`HistoryRefConfig::chars_per_token`]
/// (defaults to [`DEFAULT_CHARS_PER_TOKEN`] when unset).
#[allow(dead_code)] // consumed by sampler in Iteration C
pub(crate) fn history_char_budget(cfg: &HistoryRefConfig, provider: &ProviderConfig) -> usize {
    if let Some(explicit) = cfg.max_chars {
        return explicit;
    }
    let window = model_context_window(provider);
    let chars_per_token = cfg
        .chars_per_token
        .filter(|v| v.is_finite() && *v > 0.0)
        .unwrap_or(DEFAULT_CHARS_PER_TOKEN);
    let total_chars = window as f64 * chars_per_token as f64;
    (total_chars * cfg.max_chars_ratio as f64) as usize
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn mk_provider(model: &str, context_window: Option<usize>) -> ProviderConfig {
        ProviderConfig {
            api_style: None,
            endpoint: None,
            api_key: None,
            model: model.to_string(),
            max_tokens: None,
            temperature: None,
            context_window,
            extra: HashMap::new(),
        }
    }

    fn mk_history_cfg(max_chars: Option<usize>, max_chars_ratio: f32) -> HistoryRefConfig {
        HistoryRefConfig {
            enabled: true,
            count: 30,
            max_chars,
            max_chars_ratio,
            skip_merges: true,
            prefer_format: true,
            include_body: true,
            chars_per_token: None,
        }
    }

    #[test]
    fn test_model_context_window_defaults_to_16k_for_unknown() {
        // Conservative fallback: unknown models get 16K instead of the old
        // 128K so a tiny local model isn't silently over-budgeted.
        let provider = mk_provider("totally-unknown-model-name", None);
        assert_eq!(model_context_window(&provider), 16_000);
    }

    #[test]
    fn test_model_context_window_explicit_override_wins() {
        let provider = mk_provider("gpt-4", Some(500_000));
        assert_eq!(model_context_window(&provider), 500_000);
    }

    #[test]
    fn test_model_context_window_known_substring_match() {
        let table: &[(&str, usize)] = &[
            ("claude-opus-4-20250514", 200_000),
            ("Claude-Sonnet-4-5", 200_000),
            ("gpt-4o-mini", 128_000),
            ("gpt-4-turbo-preview", 128_000),
            ("gpt-4", 8_192),
            ("gpt-3.5-turbo", 16_000),
            ("gemini-1.5-pro", 1_000_000),
            ("gemini-2.0-flash", 1_000_000),
            ("gemini-3-flash-preview", 1_000_000),
            ("llama3-70b", 8_192),
        ];
        for (model, expected) in table {
            let provider = mk_provider(model, None);
            let got = model_context_window(&provider);
            assert_eq!(got, *expected, "model={model}");
        }
    }

    #[test]
    fn test_model_context_window_modern_models() {
        // Regression coverage for previously-misclassified models.
        let table: &[(&str, usize)] = &[
            // gpt-4.1 must NOT match the bare "gpt-4" → 8_192 entry.
            ("gpt-4.1", 1_000_000),
            ("gpt-4.1-mini", 1_000_000),
            // gpt-5 family
            ("gpt-5-mini", 400_000),
            ("gpt-5", 400_000),
            // Real Claude 3.x model IDs that don't contain bare 'claude-haiku'.
            ("claude-3-haiku-20240307", 200_000),
            ("claude-3-5-sonnet-20241022", 200_000),
            ("claude-3-5-haiku-latest", 200_000),
            ("claude-3-opus-20240229", 200_000),
            // Claude 4.x
            ("claude-opus-4-20250514", 200_000),
            ("claude-sonnet-4-5-20250929", 200_000),
            ("claude-haiku-4-5-20251001", 200_000),
            // Gemini flash
            ("gemini-flash-latest", 1_000_000),
        ];
        for (model, expected) in table {
            let provider = mk_provider(model, None);
            let got = model_context_window(&provider);
            assert_eq!(got, *expected, "model={model}");
        }
    }

    #[test]
    fn test_history_char_budget_uses_max_chars_when_set() {
        let provider = mk_provider("claude-opus-4", None);
        let cfg = mk_history_cfg(Some(2000), 0.5);
        assert_eq!(history_char_budget(&cfg, &provider), 2000);
    }

    #[test]
    fn test_history_char_budget_uses_ratio_against_context_window() {
        let provider = mk_provider("claude-opus-4", None);
        let cfg = mk_history_cfg(None, 0.05);
        // 200_000 tokens × 3 chars/token × 0.05 = 30_000 chars (default heuristic)
        assert_eq!(history_char_budget(&cfg, &provider), 30_000);
    }

    #[test]
    fn test_history_char_budget_respects_chars_per_token_override() {
        let provider = mk_provider("claude-opus-4", None);
        let cfg = HistoryRefConfig {
            chars_per_token: Some(1.5),
            ..mk_history_cfg(None, 0.05)
        };
        // 200_000 × 1.5 × 0.05 = 15_000 chars — CJK-safe budget.
        assert_eq!(history_char_budget(&cfg, &provider), 15_000);
    }

    #[test]
    fn test_history_char_budget_rejects_invalid_chars_per_token() {
        // Non-positive or non-finite override falls back to the default.
        let provider = mk_provider("claude-opus-4", None);
        for bad in [0.0_f32, -1.0, f32::NAN, f32::INFINITY] {
            let cfg = HistoryRefConfig {
                chars_per_token: Some(bad),
                ..mk_history_cfg(None, 0.05)
            };
            // Falls back to 3.0 heuristic → 30_000.
            assert_eq!(history_char_budget(&cfg, &provider), 30_000, "bad={bad:?}");
        }
    }
}
