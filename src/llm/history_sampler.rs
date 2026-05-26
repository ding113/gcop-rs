//! Historical commit sampling for prompt-injection style references.
//!
//! Pure functions over a collection of past commits, used to select a
//! representative subset whose messages will be shown to the LLM as style
//! reference. This module is intentionally IO-free so the sampler can be
//! unit-tested with synthetic data.
//!
//! The sampling algorithm balances three concerns:
//! 1. **Author diversity** — top-K active authors get proportional quotas so
//!    a single high-volume contributor cannot dominate the references.
//! 2. **Format quality** — commits matching Conventional Commits or gitmoji
//!    get boosted weights so the LLM sees well-formed examples.
//! 3. **Recency** — exponential decay over days so recent style wins over
//!    ancient style without ignoring older anchors entirely.

use chrono::{DateTime, Local};
use std::collections::HashMap;

use crate::config::{HistoryRefConfig, ProviderConfig};
use crate::git::GitOperations;
use crate::git::history::HistoricalCommit;

const KNOWN_TYPES: &[&str] = &[
    "feat", "fix", "docs", "style", "refactor", "perf", "test", "chore", "ci", "build", "revert",
];

/// Minimum number of author buckets to draw from when the count is small.
/// Scales upward with `cfg.count` so a count=30 sample doesn't artificially
/// collapse 30 distinct contributors into 5.
const MIN_AUTHOR_BUCKETS: usize = 5;

/// Computes the per-invocation cap on author buckets. Returns at least
/// [`MIN_AUTHOR_BUCKETS`], grows with `count / 3` so larger samples can pull
/// from more contributors. Callers further bound by the actual bucket count
/// and `cfg.count`.
fn max_author_buckets_for(count: usize) -> usize {
    MIN_AUTHOR_BUCKETS.max(count / 3)
}

/// Returns true when the subject matches the Conventional Commits format:
/// `type[(scope)][!]: description` with `type` from a known set and a
/// mandatory `": "` separator before non-empty content.
pub(crate) fn is_conventional(subject: &str) -> bool {
    let bytes = subject.as_bytes();
    let mut i = 0;

    while i < bytes.len() && bytes[i].is_ascii_lowercase() {
        i += 1;
    }
    if i == 0 || !KNOWN_TYPES.contains(&&subject[..i]) {
        return false;
    }

    if i < bytes.len() && bytes[i] == b'(' {
        let scope_start = i + 1;
        let mut j = scope_start;
        while j < bytes.len() && bytes[j] != b')' && bytes[j] != b'(' {
            j += 1;
        }
        if j >= bytes.len() || bytes[j] != b')' || j == scope_start {
            return false;
        }
        i = j + 1;
    }

    if i < bytes.len() && bytes[i] == b'!' {
        i += 1;
    }

    if i + 1 >= bytes.len() || bytes[i] != b':' || bytes[i + 1] != b' ' {
        return false;
    }
    i += 2;

    bytes[i..].iter().any(|b| !b.is_ascii_whitespace())
}

/// Returns true if `c` is in a recognised emoji-presentation Unicode block.
///
/// Whitelists the BMP symbol blocks that contain `✨` `⚡` `🔥` etc.
/// (U+2600..=U+27BF) and the supplementary-plane Emoji blocks
/// (U+1F000..=U+1FAFF). Crucially EXCLUDES CJK Unified Ideographs
/// (U+4E00..=U+9FFF) so Chinese / Japanese / Korean subjects are not
/// false-positive-matched as gitmoji.
fn is_emoji_codepoint(c: char) -> bool {
    let code = c as u32;
    (0x2600..=0x27BF).contains(&code) || (0x1F000..=0x1FAFF).contains(&code)
}

/// Returns true when the subject starts with a gitmoji — either a
/// `:shortcode:` token or a Unicode emoji code point.
///
/// Both bare shortcodes (`":art:"` with no trailing text) and the
/// shortcode-plus-content form (`":art: refactor logo"`) are accepted.
pub(crate) fn is_gitmoji(subject: &str) -> bool {
    if subject.is_empty() {
        return false;
    }

    if let Some(rest) = subject.strip_prefix(':') {
        let bytes = rest.as_bytes();
        let mut i = 0;
        while i < bytes.len() && bytes[i] != b':' {
            let c = bytes[i];
            if !(c.is_ascii_lowercase() || c == b'_' || c.is_ascii_digit()) {
                return false;
            }
            i += 1;
        }
        if i == 0 || i >= bytes.len() {
            return false;
        }
        // Accept bare ":art:" or ":art: <content>"; reject ":art:noWhitespace".
        let tail = &rest[i + 1..];
        return tail.is_empty() || tail.starts_with(|c: char| c.is_whitespace());
    }

    let mut chars = subject.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !is_emoji_codepoint(first) {
        return false;
    }
    // Accept bare "✨" or "✨ <content>"; reject "✨noWhitespace".
    match chars.next() {
        None => true,
        Some(c) => c.is_whitespace(),
    }
}

// === Sampling configuration & scoring ===

/// Sampler tuning knobs and injectable clock/seed.
///
/// `now` and `seed` are injectable so unit tests get deterministic output
/// without coupling to wall clock or thread-local RNG.
#[allow(dead_code)] // consumed by gather_reference_messages in Iteration G
#[derive(Debug, Clone)]
pub(crate) struct SamplerConfig {
    pub count: usize,
    pub skip_merges: bool,
    pub prefer_format: bool,
    pub seed: Option<u64>,
    pub now: DateTime<Local>,
}

/// Combined scoring function used as the per-commit sampling weight.
///
/// `score = format_score(commit) * recency_score(commit, now)`. When
/// `prefer_format` is `false`, the format component collapses to `1.0` so
/// only recency matters.
#[allow(dead_code)] // consumed by sample() / Iteration G
pub(crate) fn score(commit: &HistoricalCommit, cfg: &SamplerConfig) -> f64 {
    let fs = if cfg.prefer_format {
        format_score(commit)
    } else {
        1.0
    };
    fs * recency_score(commit, cfg.now)
}

fn format_score(commit: &HistoricalCommit) -> f64 {
    if is_conventional(&commit.subject) || is_gitmoji(&commit.subject) {
        1.5
    } else {
        0.5
    }
}

fn recency_score(commit: &HistoricalCommit, now: DateTime<Local>) -> f64 {
    let age = now - commit.timestamp;
    let age_days = age.num_days().max(0) as f64;
    0.5 + 0.5 * (-age_days / 90.0).exp()
}

// === Tiny PRNG (no `rand` dependency) ===

struct XorShift64 {
    state: u64,
}

impl XorShift64 {
    fn new(seed: u64) -> Self {
        Self { state: seed.max(1) }
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x
    }

    /// Uniform `[0.0, 1.0)` with 53-bit mantissa precision.
    fn next_f64(&mut self) -> f64 {
        let mantissa = self.next_u64() >> 11;
        mantissa as f64 / ((1u64 << 53) as f64)
    }
}

fn resolve_seed(explicit: Option<u64>) -> u64 {
    explicit.unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0xC0FFEE)
    })
}

// === Bucketing & quota allocation ===

fn bucket_by_author(commits: Vec<HistoricalCommit>) -> Vec<Vec<HistoricalCommit>> {
    let mut buckets: HashMap<String, Vec<HistoricalCommit>> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    for c in commits {
        let key = c.author_email.clone();
        if !buckets.contains_key(&key) {
            order.push(key.clone());
        }
        buckets.entry(key).or_default().push(c);
    }
    // Sort buckets by size descending; preserve insertion order on ties for
    // determinism across HashMap iteration orderings.
    let mut indexed: Vec<(usize, String)> = order.into_iter().enumerate().collect();
    indexed.sort_by(|a, b| {
        let la = buckets.get(&a.1).map(Vec::len).unwrap_or(0);
        let lb = buckets.get(&b.1).map(Vec::len).unwrap_or(0);
        lb.cmp(&la).then(a.0.cmp(&b.0))
    });
    indexed
        .into_iter()
        .filter_map(|(_, key)| buckets.remove(&key))
        .collect()
}

fn allocate_quotas(bucket_sizes: &[usize], count: usize) -> Vec<usize> {
    let k = bucket_sizes.len();
    if k == 0 {
        return Vec::new();
    }
    let total: usize = bucket_sizes.iter().sum();
    if total == 0 {
        return vec![0; k];
    }

    let mut quotas: Vec<usize> = bucket_sizes.iter().map(|&n| (n * count) / total).collect();
    let mut remainder = count.saturating_sub(quotas.iter().sum::<usize>());

    // Spillover: distribute remainder to buckets in size-desc order (i.e.
    // the order we get them in, since callers sort first).
    loop {
        if remainder == 0 {
            break;
        }
        let mut added = false;
        for i in 0..k {
            if remainder == 0 {
                break;
            }
            if quotas[i] < bucket_sizes[i] {
                quotas[i] += 1;
                remainder -= 1;
                added = true;
            }
        }
        if !added {
            break;
        }
    }

    quotas
}

fn weighted_pick(
    commits: &[HistoricalCommit],
    quota: usize,
    weights: &[f64],
    rng: &mut XorShift64,
) -> Vec<HistoricalCommit> {
    let mut remaining: Vec<(&HistoricalCommit, f64)> =
        commits.iter().zip(weights.iter().copied()).collect();
    let mut picked = Vec::with_capacity(quota.min(remaining.len()));

    for _ in 0..quota {
        if remaining.is_empty() {
            break;
        }
        let total: f64 = remaining.iter().map(|(_, w)| w).sum();
        let chosen_idx = if total <= 0.0 {
            0
        } else {
            let r = rng.next_f64() * total;
            let mut acc = 0.0;
            let mut idx = remaining.len() - 1;
            for (i, (_, w)) in remaining.iter().enumerate() {
                acc += *w;
                if acc > r {
                    idx = i;
                    break;
                }
            }
            idx
        };
        let (commit, _) = remaining.remove(chosen_idx);
        picked.push(commit.clone());
    }

    picked
}

// === Main entry point ===

/// Picks at most `cfg.count` historical commits using format-weighted,
/// recency-biased, author-balanced sampling.
///
/// Result is sorted by timestamp descending. Returns fewer than `cfg.count`
/// only when the filtered pool has fewer commits available.
#[allow(dead_code)] // consumed by gather_reference_messages in Iteration G
pub(crate) fn sample(history: &[HistoricalCommit], cfg: &SamplerConfig) -> Vec<HistoricalCommit> {
    let keep = |c: &&HistoricalCommit| -> bool {
        // Always drop commits with an empty trimmed subject — they render as
        // a bare "N. " entry in the prompt and waste a slot.
        if c.subject.trim().is_empty() {
            return false;
        }
        if cfg.skip_merges && c.parent_count > 1 {
            return false;
        }
        true
    };
    let mut pool: Vec<HistoricalCommit> = history.iter().filter(keep).cloned().collect();

    if pool.is_empty() || cfg.count == 0 {
        return Vec::new();
    }
    if pool.len() <= cfg.count {
        pool.sort_by_key(|c| std::cmp::Reverse(c.timestamp));
        return pool;
    }

    let mut buckets = bucket_by_author(pool);
    let k = buckets
        .len()
        .min(cfg.count)
        .min(max_author_buckets_for(cfg.count));
    buckets.truncate(k);

    let bucket_sizes: Vec<usize> = buckets.iter().map(Vec::len).collect();
    let quotas = allocate_quotas(&bucket_sizes, cfg.count);

    let mut rng = XorShift64::new(resolve_seed(cfg.seed));
    let mut result: Vec<HistoricalCommit> = Vec::with_capacity(cfg.count);
    for (bucket, quota) in buckets.iter().zip(quotas.iter()) {
        let weights: Vec<f64> = bucket.iter().map(|c| score(c, cfg)).collect();
        result.extend(weighted_pick(bucket, *quota, &weights, &mut rng));
    }

    result.sort_by_key(|c| std::cmp::Reverse(c.timestamp));
    result
}

// === Orchestrator: gather formatted style references for prompt injection ===

/// Convenience constant: walk a bigger pool than `count` so the sampler can
/// favour quality (format match, author balance) without rescanning the entire
/// repo. The multiplier of 10 covers most realistic distributions.
const WALK_LIMIT_MULTIPLIER: usize = 10;

/// Bytes reserved for the section header `\n\n## Project commit-style
/// references (newest first):\n`. Used to keep the *rendered* prompt block
/// inside the configured char budget; otherwise the header alone would
/// silently overrun on very tight budgets.
const HEADER_OVERHEAD_CHARS: usize = 64;

/// Per-entry overhead added by [`crate::llm::prompt::format_historical_examples`]:
/// the `"N. "` prefix (≤ 5 chars for N ≤ 999) plus the `"\n\n"` blank-line
/// separator between entries. Used to reserve budget headroom up-front so
/// the final rendered block stays at-or-under the requested char budget.
const PER_ENTRY_OVERHEAD_CHARS: usize = 8;

/// Constructs a placeholder provider config so callers without a real provider
/// (e.g. tests, missing provider key) still get the default-context-window
/// fallback through [`crate::llm::budget::history_char_budget`].
fn placeholder_provider() -> ProviderConfig {
    ProviderConfig {
        api_style: None,
        endpoint: None,
        api_key: None,
        model: String::new(),
        max_tokens: None,
        temperature: None,
        context_window: None,
        extra: Default::default(),
    }
}

/// Builds the prompt-ready list of historical commit style references.
///
/// End-to-end orchestrator that:
/// 1. Bails out (returning empty) when the feature is disabled or count is 0.
/// 2. Reads the commit history (subject + body) via the `GitOperations` trait,
///    capped at `count × WALK_LIMIT_MULTIPLIER` to bound revwalk cost.
/// 3. Runs the format/recency/author-balanced sampler.
/// 4. Enforces a character budget derived from the model's context window.
///
/// Failures from the git layer are swallowed (logged via `tracing::warn!`) so
/// commit generation is never blocked by an unrelated history-fetch issue.
///
/// `seed` is for deterministic tests; production callers pass `None`.
pub fn gather_reference_messages(
    repo: &dyn GitOperations,
    cfg: &HistoryRefConfig,
    provider: Option<&ProviderConfig>,
    seed: Option<u64>,
) -> Vec<String> {
    if !cfg.enabled || cfg.count == 0 {
        return Vec::new();
    }

    let walk_limit = cfg
        .count
        .saturating_mul(WALK_LIMIT_MULTIPLIER)
        .max(cfg.count);
    let history = match repo.get_commit_history_full(walk_limit) {
        Ok(h) => h,
        Err(err) => {
            tracing::warn!("history reference sampling skipped: {err}");
            return Vec::new();
        }
    };
    if history.is_empty() {
        return Vec::new();
    }

    let placeholder;
    let provider_ref = match provider {
        Some(p) => p,
        None => {
            placeholder = placeholder_provider();
            &placeholder
        }
    };
    let raw_budget = crate::llm::budget::history_char_budget(cfg, provider_ref);
    // Reserve overhead for the section header and per-entry markdown so the
    // *rendered* prompt block lands inside the configured budget rather than
    // overshooting by ~6 chars × count.
    let per_entry_reserve = cfg.count.saturating_mul(PER_ENTRY_OVERHEAD_CHARS);
    let budget = raw_budget
        .saturating_sub(HEADER_OVERHEAD_CHARS)
        .saturating_sub(per_entry_reserve);

    let sampler_cfg = SamplerConfig {
        count: cfg.count,
        skip_merges: cfg.skip_merges,
        prefer_format: cfg.prefer_format,
        seed,
        now: Local::now(),
    };
    let selected = sample(&history, &sampler_cfg);

    enforce_char_budget(&selected, budget, cfg.include_body)
}

// === Char-budget enforcement & formatting ===

/// Counts user-visible characters in `s`. Uses `chars().count()` rather than
/// `len()` so CJK and other multi-byte code points contribute 1 unit each.
fn estimate_chars(s: &str) -> usize {
    s.chars().count()
}

/// Greedy-truncates a sequence of commits to fit a character budget.
///
/// Each commit is rendered via [`HistoricalCommit::format_for_prompt`]. Commits
/// are added in order; if a commit would exceed the budget:
/// - If `result` is empty (first commit), the function attempts a two-step
///   degradation: drop the body (subject-only); if still over budget, truncate
///   the subject to `budget - 1` chars and append `"…"`.
/// - Otherwise, stop and return what fits so far.
///
/// `budget` is measured in characters, not bytes.
#[allow(dead_code)] // consumed by gather_reference_messages in Iteration G
pub(crate) fn enforce_char_budget(
    commits: &[HistoricalCommit],
    budget: usize,
    include_body: bool,
) -> Vec<String> {
    let mut result: Vec<String> = Vec::new();
    let mut used = 0usize;

    for commit in commits {
        let formatted = commit.format_for_prompt(include_body);
        let len = estimate_chars(&formatted);

        if used + len <= budget {
            result.push(formatted);
            used += len;
            continue;
        }

        // This commit's full rendering doesn't fit. Try its subject only —
        // if it fits, append and CONTINUE so subsequent commits get a chance
        // at the remaining budget room.
        let subject_only = commit.format_for_prompt(false);
        let subj_len = estimate_chars(&subject_only);
        if used + subj_len <= budget {
            result.push(subject_only);
            used += subj_len;
            continue;
        }

        // Even subject-only overflows. If this is the very first commit and
        // we have nothing yet, salvage with ellipsis-truncation so the prompt
        // gets at least one reference. Otherwise (already have entries),
        // stop here.
        if result.is_empty() {
            if budget > 1 {
                let truncated: String = subject_only.chars().take(budget - 1).collect();
                result.push(format!("{truncated}…"));
            } else if budget == 1 {
                result.push("…".to_string());
            }
        }
        break;
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    // === Conventional Commits classifier ===

    #[test]
    fn test_is_conventional_basic_feat() {
        assert!(is_conventional("feat: add login"));
    }

    #[test]
    fn test_is_conventional_with_scope_and_bang() {
        assert!(is_conventional("feat(api)!: breaking"));
    }

    #[test]
    fn test_is_conventional_rejects_unknown_type() {
        assert!(!is_conventional("foo: bar"));
    }

    #[test]
    fn test_is_conventional_rejects_no_colon_space() {
        assert!(!is_conventional("feat:no space"));
        assert!(!is_conventional("feat add"));
    }

    // === Gitmoji classifier ===

    #[test]
    fn test_is_gitmoji_shortcode() {
        assert!(is_gitmoji(":sparkles: add stuff"));
    }

    #[test]
    fn test_is_gitmoji_unicode_emoji() {
        assert!(is_gitmoji("✨ add stuff"));
        assert!(!is_gitmoji("X stuff"));
    }

    // === Sampling tests ===

    use chrono::TimeZone;
    use std::collections::HashMap as TestMap;

    fn fixed_now() -> DateTime<Local> {
        Local.with_ymd_and_hms(2025, 6, 1, 12, 0, 0).unwrap()
    }

    fn mk_hist(subject: &str, email: &str, hash_suffix: &str, days_ago: i64) -> HistoricalCommit {
        HistoricalCommit {
            hash: format!("hash{hash_suffix}"),
            parent_count: 1,
            author_name: "Test User".to_string(),
            author_email: email.to_string(),
            timestamp: fixed_now() - chrono::Duration::days(days_ago),
            subject: subject.to_string(),
            body: String::new(),
        }
    }

    fn mk_merge(subject: &str, email: &str, hash_suffix: &str, days_ago: i64) -> HistoricalCommit {
        let mut c = mk_hist(subject, email, hash_suffix, days_ago);
        c.parent_count = 2;
        c
    }

    fn mk_cfg(count: usize, seed: Option<u64>) -> SamplerConfig {
        SamplerConfig {
            count,
            skip_merges: true,
            prefer_format: true,
            seed,
            now: fixed_now(),
        }
    }

    #[test]
    fn test_score_conventional_and_gitmoji_both_boosted_to_1_5x() {
        let cfg = mk_cfg(10, Some(1));
        let conv = mk_hist("feat: foo", "a@x", "1", 0);
        let gitm = mk_hist(":sparkles: foo", "a@x", "2", 0);
        let rand = mk_hist("random message", "a@x", "3", 0);

        let s_conv = score(&conv, &cfg);
        let s_gitm = score(&gitm, &cfg);
        let s_rand = score(&rand, &cfg);

        assert!(s_conv > s_rand, "conventional should beat random");
        assert!(
            (s_conv - s_gitm).abs() < 1e-9,
            "conv and gitmoji equal weight"
        );
        assert!(
            (s_conv - s_rand * 3.0).abs() < 1e-9,
            "boost ratio is 1.5/0.5 = 3x"
        );
    }

    #[test]
    fn test_score_recency_decay_monotonic() {
        let cfg = mk_cfg(10, Some(1));
        let now = mk_hist("feat: foo", "a@x", "1", 0);
        let old = mk_hist("feat: foo", "a@x", "2", 60);
        let ancient = mk_hist("feat: foo", "a@x", "3", 365);

        assert!(score(&now, &cfg) > score(&old, &cfg));
        assert!(score(&old, &cfg) > score(&ancient, &cfg));
    }

    #[test]
    fn test_sample_empty_history_returns_empty() {
        let cfg = mk_cfg(5, Some(42));
        let result = sample(&[], &cfg);
        assert!(result.is_empty());
    }

    #[test]
    fn test_sample_history_smaller_than_count_returns_all_sorted_desc() {
        let cfg = mk_cfg(3, Some(42));
        let history = vec![
            mk_hist("feat: a", "a@x", "1", 10),
            mk_hist("fix: b", "a@x", "2", 5),
        ];
        let result = sample(&history, &cfg);
        assert_eq!(result.len(), 2);
        // Time desc: newer (5 days ago) before older (10 days ago)
        assert_eq!(result[0].hash, "hash2");
        assert_eq!(result[1].hash, "hash1");
    }

    #[test]
    fn test_sample_skips_merges_when_configured() {
        let mut cfg = mk_cfg(3, Some(42));
        cfg.skip_merges = true;
        // 2 normal + 2 merges = 4 total; with skip_merges + count=3, pool has
        // 2 left, falls into the "≤ count" branch and returns those 2.
        let history = vec![
            mk_hist("feat: a", "a@x", "1", 10),
            mk_merge("Merge x", "a@x", "m1", 8),
            mk_hist("fix: b", "a@x", "2", 5),
            mk_merge("Merge y", "a@x", "m2", 3),
        ];
        let result = sample(&history, &cfg);
        assert_eq!(result.len(), 2);
        assert!(result.iter().all(|c| c.parent_count <= 1));
    }

    #[test]
    fn test_sample_all_merge_repo_with_skip_merges_returns_empty() {
        let cfg = mk_cfg(5, Some(42));
        let history = vec![
            mk_merge("Merge x", "a@x", "m1", 1),
            mk_merge("Merge y", "a@x", "m2", 2),
        ];
        let result = sample(&history, &cfg);
        assert!(result.is_empty());
    }

    #[test]
    fn test_sample_stratifies_across_authors() {
        let cfg = mk_cfg(9, Some(42));
        let mut history = Vec::new();
        for i in 0..15 {
            history.push(mk_hist("feat: a", "a@x", &format!("a{i}"), i as i64));
        }
        for i in 0..10 {
            history.push(mk_hist("feat: b", "b@x", &format!("b{i}"), i as i64));
        }
        for i in 0..5 {
            history.push(mk_hist("feat: c", "c@x", &format!("c{i}"), i as i64));
        }

        let result = sample(&history, &cfg);
        assert_eq!(result.len(), 9);

        let mut counts: TestMap<&str, usize> = TestMap::new();
        for c in &result {
            *counts.entry(c.author_email.as_str()).or_insert(0) += 1;
        }
        assert!(
            counts.get("a@x").copied().unwrap_or(0) >= 1,
            "author a missing"
        );
        assert!(
            counts.get("b@x").copied().unwrap_or(0) >= 1,
            "author b missing"
        );
        assert!(
            counts.get("c@x").copied().unwrap_or(0) >= 1,
            "author c missing"
        );
        // Active contributor (15 commits) gets at least as many as the
        // least-active (5 commits)
        assert!(counts["a@x"] >= counts["c@x"]);
    }

    #[test]
    fn test_sample_seed_determinism() {
        let cfg = mk_cfg(5, Some(123));
        let mut history = Vec::new();
        for i in 0..20 {
            history.push(mk_hist("random subject", "a@x", &format!("h{i}"), i as i64));
        }
        let r1 = sample(&history, &cfg);
        let r2 = sample(&history, &cfg);
        let hashes1: Vec<_> = r1.iter().map(|c| c.hash.as_str()).collect();
        let hashes2: Vec<_> = r2.iter().map(|c| c.hash.as_str()).collect();
        assert_eq!(hashes1, hashes2);
    }

    // === Robustness edge cases (extras from plan §robustness) ===

    #[test]
    fn test_sample_single_author_uses_all_quota_for_one_bucket() {
        let cfg = mk_cfg(3, Some(42));
        let history = vec![
            mk_hist("feat: a", "a@x", "1", 10),
            mk_hist("feat: b", "a@x", "2", 8),
            mk_hist("feat: c", "a@x", "3", 6),
            mk_hist("feat: d", "a@x", "4", 4),
            mk_hist("feat: e", "a@x", "5", 2),
        ];
        let result = sample(&history, &cfg);
        assert_eq!(result.len(), 3);
        assert!(result.iter().all(|c| c.author_email == "a@x"));
    }

    #[test]
    fn test_sample_all_merge_skip_merges_false_uses_them() {
        let mut cfg = mk_cfg(2, Some(42));
        cfg.skip_merges = false;
        let history = vec![
            mk_merge("Merge x", "a@x", "m1", 1),
            mk_merge("Merge y", "a@x", "m2", 2),
        ];
        let result = sample(&history, &cfg);
        assert_eq!(result.len(), 2);
        assert!(result.iter().all(|c| c.parent_count == 2));
    }

    #[test]
    fn test_sample_no_format_match_still_returns_n_results() {
        let cfg = mk_cfg(3, Some(42));
        let history: Vec<HistoricalCommit> = (0..10)
            .map(|i| {
                mk_hist(
                    "just plain text no format",
                    "a@x",
                    &format!("h{i}"),
                    i as i64,
                )
            })
            .collect();
        let result = sample(&history, &cfg);
        // Lowest format_score is 0.5 (>0), so weighted sampling still picks N
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn test_sample_handles_empty_author_email() {
        let cfg = mk_cfg(2, Some(42));
        let history = vec![
            mk_hist("feat: a", "", "1", 10),
            mk_hist("feat: b", "", "2", 5),
        ];
        // Two empty-email commits go into a single empty-string bucket but
        // the sampler must not panic; with len<=count we get both back.
        let result = sample(&history, &cfg);
        assert_eq!(result.len(), 2);
    }

    // === enforce_char_budget tests ===

    fn mk_hist_with_body(
        subject: &str,
        body: &str,
        email: &str,
        hash_suffix: &str,
        days_ago: i64,
    ) -> HistoricalCommit {
        let mut c = mk_hist(subject, email, hash_suffix, days_ago);
        c.body = body.to_string();
        c
    }

    #[test]
    fn test_enforce_char_budget_keeps_under_cap() {
        // 5 commits with subject "feat: x" (7 chars each, no body): total 35
        // chars. Budget 14 should fit exactly 2 commits (7 + 7 = 14).
        let commits: Vec<_> = (0..5)
            .map(|i| mk_hist("feat: x", "a@x", &format!("{i}"), i))
            .collect();
        let result = enforce_char_budget(&commits, 14, true);
        assert_eq!(result.len(), 2);
        let total: usize = result.iter().map(|s| s.chars().count()).sum();
        assert!(total <= 14, "total chars {total} > budget 14");
    }

    #[test]
    fn test_enforce_char_budget_truncates_oversize_first_commit() {
        // Case A: subject short, body too long → expect subject-only.
        let huge_body = "x".repeat(5000);
        let c1 = mk_hist_with_body("feat: short", &huge_body, "a@x", "1", 0);
        let result = enforce_char_budget(&[c1], 200, true);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], "feat: short");

        // Case B: subject too long → expect truncation with ellipsis.
        let mut c2 = mk_hist("x".repeat(5000).as_str(), "a@x", "2", 0);
        c2.body = String::new();
        let result = enforce_char_budget(&[c2], 200, true);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].chars().count(), 200);
        assert!(result[0].ends_with('…'));
    }

    #[test]
    fn test_enforce_char_budget_cjk_counted_by_chars_not_bytes() {
        // "提交" is 6 bytes but 2 characters. With budget=3, it fits.
        let c = mk_hist("提交", "a@x", "1", 0);
        let result = enforce_char_budget(&[c], 3, false);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], "提交");
    }

    // === Fixes 2, 3, 4, 5, 13 regression tests ===

    #[test]
    fn test_is_gitmoji_rejects_cjk_first_character() {
        // CJK Unified Ideographs (U+4E00..=U+9FFF) must NOT be classified as
        // gitmoji — they would otherwise systematically over-boost CJK repos.
        assert!(!is_gitmoji("提 修复 bug"));
        assert!(!is_gitmoji("中文 commit message"));
        assert!(!is_gitmoji("日本語 開発"));
    }

    #[test]
    fn test_is_gitmoji_rejects_cyrillic_and_greek_first_character() {
        assert!(!is_gitmoji("А fix something")); // Cyrillic А
        assert!(!is_gitmoji("Α fix something")); // Greek Α
    }

    #[test]
    fn test_is_gitmoji_accepts_bare_shortcode() {
        // Bare ":art:" (no trailing description) is a valid gitmoji subject.
        assert!(is_gitmoji(":art:"));
        assert!(is_gitmoji(":sparkles:"));
    }

    #[test]
    fn test_is_gitmoji_accepts_bare_unicode_emoji() {
        assert!(is_gitmoji("✨"));
        assert!(is_gitmoji("🎨"));
    }

    #[test]
    fn test_is_gitmoji_rejects_shortcode_without_space_before_content() {
        // ":art:no-space" is malformed — closing colon must be followed by
        // whitespace (or end-of-string).
        assert!(!is_gitmoji(":art:noSpace"));
    }

    #[test]
    fn test_sample_filters_empty_subject_commits() {
        let cfg = mk_cfg(5, Some(42));
        let history = vec![
            mk_hist("feat: real", "a@x", "1", 0),
            mk_hist("", "a@x", "2", 1), // empty subject — must be filtered
            mk_hist("   ", "a@x", "3", 2), // whitespace-only — must be filtered
            mk_hist("fix: also real", "a@x", "4", 3),
        ];
        let result = sample(&history, &cfg);
        assert_eq!(result.len(), 2);
        assert!(result.iter().all(|c| !c.subject.trim().is_empty()));
    }

    #[test]
    fn test_enforce_char_budget_continues_after_first_commit_subject_fallback() {
        // First commit body overflows budget → degrade to subject-only.
        // With the fix, subsequent commits should still get a chance at the
        // remaining budget (previously the loop break aborted them).
        let huge_body = "x".repeat(5000);
        let commits = vec![
            mk_hist_with_body("feat: a", &huge_body, "a@x", "1", 0),
            mk_hist("fix: b", "a@x", "2", 1),
            mk_hist("chore: c", "a@x", "3", 2),
        ];
        let result = enforce_char_budget(&commits, 200, true);
        // Expect 3 entries: subject-only "feat: a" + the two later subjects.
        assert_eq!(result.len(), 3);
        assert_eq!(result[0], "feat: a");
        assert_eq!(result[1], "fix: b");
        assert_eq!(result[2], "chore: c");
    }

    #[test]
    fn test_enforce_char_budget_stops_when_subject_alone_overflows_after_progress() {
        // Once we have at least one commit, a subject-only fallback that
        // doesn't fit should stop the loop (don't truncate later commits).
        let commits = vec![
            mk_hist("feat: small", "a@x", "1", 0),
            mk_hist("x".repeat(500).as_str(), "a@x", "2", 1),
        ];
        let result = enforce_char_budget(&commits, 15, true);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], "feat: small");
    }

    #[test]
    fn test_max_author_buckets_scales_with_count() {
        // count=30 should allow up to 10 buckets, not the old hard cap of 5.
        assert_eq!(max_author_buckets_for(30), 10);
        // Small counts still respect the floor of 5.
        assert_eq!(max_author_buckets_for(3), 5);
        // Large counts scale linearly with count/3.
        assert_eq!(max_author_buckets_for(300), 100);
    }
}
