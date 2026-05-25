//! Command implementations.
//!
//! Contains implementations of all gcop-rs CLI commands.
//!
//! # Modules
//! - `commit` - Commit message generation flow.
//! - `review` - Code review.
//! - `config` - Configuration management.
//! - `alias` - Git alias management.
//! - `init` - Project initialization.
//! - `stats` - Repository statistics.
//! - `hook` - Git hook management (`prepare-commit-msg`).
//! - `commit_state_machine` - Commit workflow state machine.
//! - `format` - Output format definition.
//! - `options` - Command option structs.
//! - `json` - JSON output helpers.
//!
//! # Architecture
//! ```text
//! CLI (cli.rs)
//!   ├── commands/commit.rs ─> commit_state_machine.rs
//!   ├── commands/review.rs
//!   ├── commands/config.rs
//!   ├── commands/stats.rs
//!   └── shared command options (commands/options.rs)
//! ```

/// Git alias management commands.
pub mod alias;
/// Commit generation command flow.
pub mod commit;
/// Commit workflow state machine.
pub mod commit_state_machine;
/// Configuration edit/validation commands.
pub mod config;
/// Output format types and parsing helpers.
pub mod format;
/// Git hook install/uninstall command.
pub mod hook;
/// Configuration initialization commands.
pub mod init;
/// Shared JSON output helpers.
pub mod json;
/// Shared command option structs.
pub mod options;
/// Code review command flow.
pub mod review;
/// Atomic split commit logic.
pub mod split;
/// Repository statistics command flow.
pub mod stats;

// Re-export for external use (tests, library users).
#[allow(unused_imports)]
pub use format::OutputFormat;
pub use options::{CommitOptions, ReviewOptions, StatsOptions};

use crate::git::diff::{FileDiff, split_diff_by_file};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use std::fmt::Write;

/// Filename suffixes that are typically auto-generated artifacts.
const AUTO_GENERATED_SUFFIXES: &[&str] = &[".min.js", ".min.css"];

/// Exact lockfile basenames (case-insensitive).
///
/// This list is intentionally stricter than generated-artifact detection:
/// only real dependency lockfiles are forced to summary-only output.
const LOCKFILE_BASENAMES: &[&str] = &[
    "package-lock.json",
    "npm-shrinkwrap.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "cargo.lock",
    "poetry.lock",
    "pipfile.lock",
    "uv.lock",
    "composer.lock",
    "gemfile.lock",
    "go.sum",
    "go.work.sum",
    "bun.lockb",
    "bun.lock",
    "deno.lock",
    "flake.lock",
    "conan.lock",
    "pubspec.lock",
    "mix.lock",
    "stack.yaml.lock",
    "podfile.lock",
];

/// Substrings that usually indicate generated files.
const AUTO_GENERATED_SUBSTRINGS: &[&str] = &[".generated."];

/// Returns `true` if `filename` matches an auto-generated file pattern.
fn is_auto_generated(filename: &str) -> bool {
    if AUTO_GENERATED_SUFFIXES
        .iter()
        .any(|&s| filename.ends_with(s))
    {
        return true;
    }
    if AUTO_GENERATED_SUBSTRINGS
        .iter()
        .any(|&s| filename.contains(s))
    {
        return true;
    }
    false
}

/// Returns `true` if `filename` is a dependency lockfile.
///
/// Built-in basenames are case-insensitive. User-provided glob patterns are
/// also case-insensitive and match repository-relative paths like
/// `apps/web/Cargo.lock`.
#[cfg(test)]
fn is_lockfile(filename: &str, patterns: &[String]) -> bool {
    LockfileMatcher::new(patterns).is_match(filename)
}

struct LockfileMatcher {
    custom_patterns: Option<GlobSet>,
}

impl LockfileMatcher {
    fn new(patterns: &[String]) -> Self {
        Self {
            custom_patterns: build_lockfile_globset(patterns),
        }
    }

    fn is_match(&self, filename: &str) -> bool {
        let normalized = filename.replace('\\', "/");
        let basename = normalized.rsplit('/').next().unwrap_or(&normalized);

        if LOCKFILE_BASENAMES
            .iter()
            .any(|name| basename.eq_ignore_ascii_case(name))
        {
            return true;
        }

        self.custom_patterns
            .as_ref()
            .is_some_and(|globset| globset.is_match(normalized))
    }
}

fn build_lockfile_globset(patterns: &[String]) -> Option<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    let mut has_patterns = false;

    for pattern in patterns.iter().map(|p| p.trim()).filter(|p| !p.is_empty()) {
        match GlobBuilder::new(pattern)
            .case_insensitive(true)
            .literal_separator(true)
            .build()
        {
            Ok(glob) => {
                builder.add(glob);
                has_patterns = true;
            }
            Err(err) => {
                tracing::warn!("Ignoring invalid lockfile pattern '{}': {}", pattern, err);
            }
        }
    }

    if has_patterns {
        match builder.build() {
            Ok(globset) => Some(globset),
            Err(err) => {
                tracing::warn!("Ignoring lockfile patterns: {}", err);
                None
            }
        }
    } else {
        None
    }
}

/// Truncates diffs at file granularity to reduce LLM token usage.
///
/// Replaces previous byte-level truncation. Every file keeps at least summary stats.
/// Important files keep full patches, while lockfiles, generated files, or over-budget files are downgraded to summary-only entries.
///
/// Returns `(formatted_diff, had_downgraded_files)`.
pub(crate) fn smart_truncate_diff(
    diff: &str,
    max_size: usize,
    lockfile_patterns: &[String],
) -> (String, bool) {
    let files = split_diff_by_file(diff);

    if files.is_empty() {
        return (diff.to_string(), false);
    }

    // Classify files into auto-generated and regular files.
    let mut full_files: Vec<&FileDiff> = Vec::new();
    let mut summary_files: Vec<(&FileDiff, &str)> = Vec::new(); // (file, reason)

    // Lockfiles are always downgraded to summary-only mode.
    // Auto-generated non-lockfiles are downgraded only when the total diff is over budget.
    let lockfile_matcher = LockfileMatcher::new(lockfile_patterns);
    let mut normal_files: Vec<&FileDiff> = Vec::new();
    let over_budget = diff.len() > max_size;
    for file in &files {
        if lockfile_matcher.is_match(&file.filename) {
            summary_files.push((file, "lockfile"));
        } else if over_budget && is_auto_generated(&file.filename) {
            summary_files.push((file, "auto-generated"));
        } else {
            normal_files.push(file);
        }
    }

    // Fast path: total diff size is within budget and no always-summary files exist.
    if !over_budget && summary_files.is_empty() {
        return (diff.to_string(), false);
    }

    // Sort normal files by ascending patch size (small files are kept first).
    normal_files.sort_by_key(|f| f.content.len());

    // Greedy packing into remaining budget.
    let mut budget_used = 0usize;
    for file in &normal_files {
        if budget_used + file.content.len() <= max_size {
            budget_used += file.content.len();
            full_files.push(file);
        } else {
            summary_files.push((file, "budget exceeded"));
        }
    }

    let was_truncated = !summary_files.is_empty();

    // Calculate total statistics
    let total_files = files.len();
    let total_ins: usize = files.iter().map(|f| f.insertions).sum();
    let total_del: usize = files.iter().map(|f| f.deletions).sum();

    // Formatted output
    let mut output = String::new();
    let _ = writeln!(
        output,
        "Changed files ({} files, +{} -{}):\n",
        total_files, total_ins, total_del
    );

    if !full_files.is_empty() {
        let _ = writeln!(output, "## Full diff ({} files):\n", full_files.len());
        // Output full diff in original order
        for file in &files {
            if full_files.iter().any(|f| std::ptr::eq(*f, file)) {
                let _ = writeln!(output, "{}", file.content);
            }
        }
    }

    if !summary_files.is_empty() {
        let _ = writeln!(output, "\n## Summary only ({} files):", summary_files.len());
        for (file, reason) in &summary_files {
            let _ = writeln!(
                output,
                "- {} (+{} -{}) [{}]",
                file.filename, file.insertions, file.deletions, reason
            );
        }
    }

    (output, was_truncated)
}

/// Replace lockfile patches with summary-only pseudo-diffs.
///
/// Split mode still needs one entry per staged file so the model can group all
/// files correctly. This keeps file names and change counts while omitting the
/// full lockfile patch content.
pub(crate) fn summarize_lockfile_diffs(
    file_diffs: &[FileDiff],
    lockfile_patterns: &[String],
) -> (Vec<FileDiff>, bool) {
    let lockfile_matcher = LockfileMatcher::new(lockfile_patterns);
    let mut changed = false;
    let summarized = file_diffs
        .iter()
        .map(|file| {
            if lockfile_matcher.is_match(&file.filename) {
                changed = true;
                let mut file = file.clone();
                file.content = format!(
                    "diff --git a/{0} b/{0}\n# Lockfile diff omitted; summary only: +{1} -{2} lines",
                    file.filename, file.insertions, file.deletions
                );
                file
            } else {
                file.clone()
            }
        })
        .collect();

    (summarized, changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_lockfile_builtin_basenames() {
        assert!(is_lockfile("Cargo.lock", &[]));
        assert!(is_lockfile("apps/web/yarn.lock", &[]));
        assert!(is_lockfile("POETRY.LOCK", &[]));
        assert!(is_lockfile("package-lock.json", &[]));
        assert!(is_lockfile("pnpm-lock.yaml", &[]));
        assert!(is_lockfile("go.sum", &[]));
        assert!(is_lockfile("go.work.sum", &[]));
        assert!(is_lockfile("bun.lockb", &[]));
        assert!(is_lockfile("Podfile.lock", &[]));
    }

    #[test]
    fn test_is_lockfile_custom_patterns() {
        let patterns = vec!["**/*.lock".to_string(), "locks/*.txt".to_string()];
        assert!(is_lockfile("deps.lock", &patterns));
        assert!(is_lockfile("apps/web/deps.lock", &patterns));
        assert!(is_lockfile("locks/deps.txt", &patterns));
        assert!(!is_lockfile("src/locksmith.rs", &patterns));
    }

    #[test]
    fn test_is_auto_generated_generated_files() {
        assert!(is_auto_generated("foo.generated.ts"));
        assert!(is_auto_generated("src/api.generated.rs"));
        assert!(is_auto_generated("bundle.min.js"));
        assert!(is_auto_generated("styles.min.css"));
    }

    #[test]
    fn test_is_auto_generated_normal_files() {
        assert!(!is_auto_generated("src/main.rs"));
        assert!(!is_auto_generated("README.md"));
        assert!(!is_auto_generated("Cargo.toml"));
        assert!(!is_auto_generated("Cargo.lock"));
        assert!(!is_auto_generated("src/locksmith.rs")); // Contains "lock" but does not end with .lock
    }

    #[test]
    fn test_smart_truncate_no_truncation() {
        let diff = "diff --git a/src/main.rs b/src/main.rs\n\
                     --- a/src/main.rs\n\
                     +++ b/src/main.rs\n\
                     +hello";
        // budget is big enough
        let (result, truncated) = smart_truncate_diff(diff, 10000, &[]);
        assert!(!truncated);
        assert_eq!(result, diff);
    }

    #[test]
    fn test_smart_truncate_lockfile_demoted_even_under_budget() {
        let diff = "diff --git a/Cargo.lock b/Cargo.lock\n\
                     --- a/Cargo.lock\n\
                     +++ b/Cargo.lock\n\
                     +lots of lock content";

        let (result, truncated) = smart_truncate_diff(diff, 10000, &[]);

        assert!(truncated);
        assert!(result.contains("## Summary only (1 files)"));
        assert!(result.contains("Cargo.lock (+1 -0) [lockfile]"));
        assert!(!result.contains("+lots of lock content"));
    }

    #[test]
    fn test_smart_truncate_custom_lockfile_pattern_demoted() {
        let diff = "diff --git a/apps/web/deps.snapshot b/apps/web/deps.snapshot\n\
                     --- a/apps/web/deps.snapshot\n\
                     +++ b/apps/web/deps.snapshot\n\
                     +custom lock content";
        let patterns = vec!["**/*.snapshot".to_string()];

        let (result, truncated) = smart_truncate_diff(diff, 10000, &patterns);

        assert!(truncated);
        assert!(result.contains("apps/web/deps.snapshot (+1 -0) [lockfile]"));
        assert!(!result.contains("+custom lock content"));
    }

    #[test]
    fn test_smart_truncate_auto_generated_demoted_when_over_budget() {
        let diff = "diff --git a/src/main.rs b/src/main.rs\n\
                     --- a/src/main.rs\n\
                     +++ b/src/main.rs\n\
                     +hello\n\
                     diff --git a/bundle.min.js b/bundle.min.js\n\
                     --- a/bundle.min.js\n\
                     +++ b/bundle.min.js\n\
                     +lots of generated content";
        // The budget is enough to fit everything, but smart truncation is triggered because the total size > max_size
        // Set a budget that’s just enough
        let (result, truncated) = smart_truncate_diff(diff, diff.len() - 1, &[]);
        assert!(truncated);
        assert!(result.contains("## Full diff"));
        assert!(result.contains("src/main.rs"));
        assert!(result.contains("## Summary only"));
        assert!(result.contains("bundle.min.js"));
        assert!(result.contains("[auto-generated]"));
    }

    #[test]
    fn test_smart_truncate_budget_overflow() {
        // Create a small file and a large file
        let small_diff = "diff --git a/small.rs b/small.rs\n--- a/small.rs\n+++ b/small.rs\n+x";
        let big_content = "+".repeat(500);
        let big_diff = format!(
            "diff --git a/big.rs b/big.rs\n--- a/big.rs\n+++ b/big.rs\n{}",
            big_content
        );
        let diff = format!("{}\n{}", small_diff, big_diff);

        // The budget is only enough for small files
        let (result, truncated) = smart_truncate_diff(&diff, small_diff.len() + 100, &[]);
        assert!(truncated);
        assert!(result.contains("## Full diff"));
        assert!(result.contains("small.rs"));
        assert!(result.contains("## Summary only"));
        assert!(result.contains("big.rs"));
        assert!(result.contains("[budget exceeded]"));
    }

    #[test]
    fn test_smart_truncate_all_files_too_large() {
        let big1 = format!(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n{}",
            "+".repeat(500)
        );
        let big2 = format!(
            "diff --git a/b.rs b/b.rs\n--- a/b.rs\n+++ b/b.rs\n{}",
            "+".repeat(500)
        );
        let diff = format!("{}\n{}", big1, big2);

        // The budget is extremely small and there is no room for both files.
        let (result, truncated) = smart_truncate_diff(&diff, 10, &[]);
        assert!(truncated);
        assert!(result.contains("## Summary only (2 files)"));
        assert!(result.contains("a.rs"));
        assert!(result.contains("b.rs"));
    }

    #[test]
    fn test_smart_truncate_empty_diff() {
        let (result, truncated) = smart_truncate_diff("", 1000, &[]);
        assert!(!truncated);
        assert_eq!(result, "");
    }

    #[test]
    fn test_smart_truncate_preserves_file_boundary() {
        // Create two files, budget only enough for one
        let file_a = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n+line1\n+line2";
        let file_b = "diff --git a/b.rs b/b.rs\n--- a/b.rs\n+++ b/b.rs\n+line3";
        let diff = format!("{}\n{}", file_a, file_b);
        // The budget is only enough for file_b (the smaller one), not enough for two
        let (result, truncated) = smart_truncate_diff(&diff, file_a.len(), &[]);
        assert!(truncated);
        // The file content in full diff should be complete (not cut in half)
        if result.contains("+line1") {
            // If a.rs is in full diff, line2 must also be in
            assert!(result.contains("+line2"));
        }
        // b.rs is smaller and should be in full diff
        assert!(result.contains("## Full diff"));
        assert!(result.contains("## Summary only"));
    }

    #[test]
    fn test_summarize_lockfile_diffs_for_split() {
        let files = vec![
            FileDiff {
                filename: "src/main.rs".to_string(),
                content: "+code".to_string(),
                insertions: 1,
                deletions: 0,
            },
            FileDiff {
                filename: "Cargo.lock".to_string(),
                content: "+lots of lock content".to_string(),
                insertions: 42,
                deletions: 7,
            },
        ];

        let (result, changed) = summarize_lockfile_diffs(&files, &[]);

        assert!(changed);
        assert_eq!(result[0].content, "+code");
        assert!(result[1].content.contains("summary only: +42 -7 lines"));
        assert!(!result[1].content.contains("+lots of lock content"));
    }
}
