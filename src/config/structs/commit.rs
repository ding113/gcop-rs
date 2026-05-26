//! Commit command configuration structures.

use serde::{Deserialize, Serialize};

/// Commit message convention style.
///
/// Controls the target format requested from the LLM.
#[derive(Debug, Clone, Deserialize, Serialize, Default, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ConventionStyle {
    /// Conventional Commits: `type(scope): description`.
    #[default]
    Conventional,
    /// Gitmoji: `:emoji: description`.
    Gitmoji,
    /// Custom format defined by [`CommitConvention::template`].
    Custom,
}

/// Commit convention configuration.
///
/// Defines team-specific commit rules injected into prompt generation.
///
/// # Example
/// ```toml
/// [commit.convention]
/// style = "conventional"
/// types = ["feat", "fix", "docs", "style", "refactor", "perf", "test", "chore", "ci"]
/// extra_prompt = "All commit messages must be in English"
/// ```
#[derive(Debug, Clone, Deserialize, Serialize, Default, PartialEq)]
pub struct CommitConvention {
    /// Convention style.
    #[serde(default)]
    pub style: ConventionStyle,

    /// Allowed commit types (used when `style = "conventional"` or `style = "custom"`).
    pub types: Option<Vec<String>>,

    /// Custom template (used when `style = "custom"`).
    /// Placeholders: `{type}`, `{scope}`, `{subject}`, `{body}`.
    pub template: Option<String>,

    /// Additional prompt text appended after built-in instructions.
    pub extra_prompt: Option<String>,
}

/// Commit command configuration.
///
/// Controls commit message generation behavior.
///
/// # Fields
/// - `show_diff_preview`: show diff preview before generation (default: `true`)
/// - `allow_edit`: allow editing generated messages (default: `true`)
/// - `split`: enable atomic split commit mode by default (default: `false`)
/// - `skip_llm_for_lockfile_only`: short-circuit the LLM and emit a fixed
///   `chore(deps): update lockfiles` message when every staged file is a
///   lockfile (default: `true`)
/// - `custom_prompt`: prompt customization text (optional; normal mode replaces base system prompt, split mode appends constraints)
/// - `max_retries`: maximum generation attempts, including the first one (default: `10`)
/// - `convention`: optional commit convention config
///
/// # Example
/// ```toml
/// [commit]
/// show_diff_preview = true
/// allow_edit = true
/// split = false
/// skip_llm_for_lockfile_only = true
/// max_retries = 10
/// custom_prompt = "Generate a concise commit message"
///
/// [commit.convention]
/// style = "conventional"
/// types = ["feat", "fix", "docs", "refactor", "test", "chore"]
/// ```
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CommitConfig {
    /// Whether to show a diff preview before generation.
    #[serde(default = "default_true")]
    pub show_diff_preview: bool,

    /// Whether to allow editing generated messages.
    #[serde(default = "default_true")]
    pub allow_edit: bool,

    /// Whether to use atomic split commit mode by default.
    #[serde(default)]
    pub split: bool,

    /// Skip the LLM call and emit a deterministic
    /// `chore(deps): update lockfiles` commit message when every staged file
    /// is a recognised lockfile (`Cargo.lock`, `package-lock.json`,
    /// `yarn.lock`, `pnpm-lock.yaml`, `go.sum`, `bun.lockb`, `uv.lock`,
    /// `flake.lock`, `Podfile.lock`, …). Honoured by normal, `--amend`,
    /// `--split`, and `--dry-run` modes. The JSON mode honours the shortcut
    /// for the generated *message* but never commits, preserving its
    /// `committed: false` contract.
    #[serde(default = "default_true")]
    pub skip_llm_for_lockfile_only: bool,

    /// Prompt customization text for commit generation.
    ///
    /// Normal mode: replaces the built-in commit system prompt.
    /// Split mode: appended as additional grouping constraints.
    ///
    /// No placeholder substitution is performed (`{diff}` is passed literally).
    #[serde(default)]
    pub custom_prompt: Option<String>,

    /// Maximum generation attempts, including the first attempt.
    #[serde(default = "default_commit_max_retries")]
    pub max_retries: usize,

    /// Optional commit convention config, usually set in `.gcop/config.toml`.
    #[serde(default)]
    pub convention: Option<CommitConvention>,

    /// Historical commit injection configuration.
    ///
    /// Defaults to a "feature on" `HistoryRefConfig` — see its docs for
    /// field-level defaults. Disable via `[commit.history]\nenabled = false`.
    #[serde(default)]
    pub history: HistoryRefConfig,
}

impl Default for CommitConfig {
    fn default() -> Self {
        Self {
            show_diff_preview: true,
            allow_edit: true,
            split: false,
            skip_llm_for_lockfile_only: true,
            custom_prompt: None,
            max_retries: default_commit_max_retries(),
            convention: None,
            history: HistoryRefConfig::default(),
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_commit_max_retries() -> usize {
    10
}

/// Historical commit injection configuration.
///
/// Controls how many past commits are sampled from the local git history and
/// injected into the LLM prompt as style references. Sampling is balanced
/// across active contributors with bias toward recent commits and commits
/// matching a known convention (Conventional Commits or gitmoji).
///
/// # Example
/// ```toml
/// [commit.history]
/// enabled = true
/// count = 30
/// max_chars = 8000     # optional explicit cap; if unset, uses ratio
/// max_chars_ratio = 0.05
/// skip_merges = true
/// prefer_format = true
/// include_body = true
/// ```
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct HistoryRefConfig {
    /// Whether to inject historical commit messages at all.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Number of historical commits to sample for the prompt.
    #[serde(default = "default_history_count")]
    pub count: usize,

    /// Explicit character-budget cap for the injected history block.
    ///
    /// When `Some(n)`, takes precedence over [`max_chars_ratio`].
    #[serde(default)]
    pub max_chars: Option<usize>,

    /// Fraction of the model's context window reserved for history when
    /// [`max_chars`] is unset.
    #[serde(default = "default_history_max_chars_ratio")]
    pub max_chars_ratio: f32,

    /// Whether to skip merge commits during sampling.
    #[serde(default = "default_true")]
    pub skip_merges: bool,

    /// Whether to boost scoring weight for commits matching a known format
    /// (Conventional Commits or gitmoji).
    #[serde(default = "default_true")]
    pub prefer_format: bool,

    /// Whether to include the body of each commit message (vs. subject only).
    #[serde(default = "default_true")]
    pub include_body: bool,

    /// Average characters per LLM token, used to convert the model's
    /// token-based context window into a character-based history budget.
    ///
    /// `None` (default) uses the built-in 3.0 heuristic, a compromise between
    /// English/code (~4) and CJK (~1.5). CJK-heavy repos should set `1.5`
    /// here; code-heavy or English-only repos can set `4.0` to maximise
    /// reference density.
    #[serde(default)]
    pub chars_per_token: Option<f32>,
}

impl Default for HistoryRefConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            count: default_history_count(),
            max_chars: None,
            max_chars_ratio: default_history_max_chars_ratio(),
            skip_merges: true,
            prefer_format: true,
            include_body: true,
            chars_per_token: None,
        }
    }
}

fn default_history_count() -> usize {
    30
}

fn default_history_max_chars_ratio() -> f32 {
    0.05
}
