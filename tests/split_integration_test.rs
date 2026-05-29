//! Split-flow integration tests for the JSON-contract fallback.
//!
//! When the LLM ignores the split JSON contract, `run_split_flow` must degrade
//! gracefully WITHOUT committing garbage:
//!   - a plain commit message → ONE atomic commit covering every staged file
//!     (Base-mode equivalence);
//!   - valid JSON wrapped in prose/fence → the real grouping is recovered;
//!   - broken/truncated/structured JSON debris → a hard error, NO commit.
//!
//! These drive the real `run_split_flow` (a `pub` entry point) with a recording
//! `GitOperations` mock and a `MockLLM` returning a fixed response. `--split -y`
//! and `--split --json` are exercised because both bypass the interactive menu;
//! the interactive path cannot be integration-tested.

use async_trait::async_trait;
use std::sync::{Arc, Mutex};

use gcop_rs::commands::format::OutputFormat;
use gcop_rs::commands::options::CommitOptions;
use gcop_rs::config::AppConfig;
use gcop_rs::error::{GcopError, Result};
use gcop_rs::git::{CommitInfo, DiffStats, GitOperations};
use gcop_rs::llm::{CommitContext, LLMProvider, ReviewResult, ReviewType, StreamChunk};
use tokio::sync::mpsc;

// === Recording GitOperations mock ===========================================

/// Records every `commit()` together with the file set staged at that moment,
/// so tests can assert "exactly one commit covering all staged files".
struct RecordingGitOps {
    all_files: Vec<String>,
    diff: String,
    staged: Mutex<Vec<String>>,
    commits: Mutex<Vec<(String, Vec<String>)>>,
}

impl RecordingGitOps {
    fn new() -> Self {
        let all_files = vec!["a.rs".to_string(), "b.rs".to_string(), "c.rs".to_string()];
        // A multi-file unified diff that `split_diff_by_file` parses into the
        // same three filenames.
        let diff = "\
diff --git a/a.rs b/a.rs
new file mode 100644
--- /dev/null
+++ b/a.rs
@@ -0,0 +1 @@
+fn a() {}
diff --git a/b.rs b/b.rs
new file mode 100644
--- /dev/null
+++ b/b.rs
@@ -0,0 +1 @@
+fn b() {}
diff --git a/c.rs b/c.rs
new file mode 100644
--- /dev/null
+++ b/c.rs
@@ -0,0 +1 @@
+fn c() {}
"
        .to_string();
        Self {
            staged: Mutex::new(all_files.clone()),
            commits: Mutex::new(Vec::new()),
            all_files,
            diff,
        }
    }

    fn commits(&self) -> Vec<(String, Vec<String>)> {
        self.commits.lock().unwrap().clone()
    }
}

#[async_trait]
impl GitOperations for RecordingGitOps {
    fn is_empty(&self) -> Result<bool> {
        Ok(false)
    }

    fn get_current_branch(&self) -> Result<Option<String>> {
        Ok(Some("main".to_string()))
    }

    fn has_staged_changes(&self) -> Result<bool> {
        Ok(!self.staged.lock().unwrap().is_empty())
    }

    fn get_staged_diff(&self) -> Result<String> {
        Ok(self.diff.clone())
    }

    fn get_uncommitted_diff(&self) -> Result<String> {
        Ok(String::new())
    }

    fn get_commit_diff(&self, _commit: &str) -> Result<String> {
        Ok(String::new())
    }

    fn get_range_diff(&self, _range: &str) -> Result<String> {
        Ok(String::new())
    }

    fn get_file_content(&self, _path: &str) -> Result<String> {
        Ok(String::new())
    }

    fn commit(&self, message: &str) -> Result<()> {
        let staged = self.staged.lock().unwrap().clone();
        self.commits
            .lock()
            .unwrap()
            .push((message.to_string(), staged));
        Ok(())
    }

    fn commit_amend(&self, _message: &str) -> Result<()> {
        Ok(())
    }

    fn get_diff_stats(&self, _diff: &str) -> Result<DiffStats> {
        Ok(DiffStats {
            files_changed: self.all_files.clone(),
            insertions: 3,
            deletions: 0,
        })
    }

    fn get_commit_history(&self) -> Result<Vec<CommitInfo>> {
        Ok(vec![])
    }

    fn get_commit_history_full(
        &self,
        _limit: usize,
    ) -> Result<Vec<gcop_rs::git::history::HistoricalCommit>> {
        Ok(vec![])
    }

    fn get_commit_line_stats(&self, _hash: &str) -> Result<(usize, usize)> {
        Ok((0, 0))
    }

    fn get_staged_files(&self) -> Result<Vec<String>> {
        Ok(self.staged.lock().unwrap().clone())
    }

    fn unstage_all(&self) -> Result<()> {
        self.staged.lock().unwrap().clear();
        Ok(())
    }

    fn stage_files(&self, files: &[String]) -> Result<()> {
        self.staged.lock().unwrap().extend_from_slice(files);
        Ok(())
    }

    fn get_workdir(&self) -> Result<std::path::PathBuf> {
        Ok(std::env::temp_dir())
    }
}

// === MockLLM ================================================================

#[derive(Clone)]
struct MockLLM {
    message: String,
    strip_thinking: bool,
}

impl MockLLM {
    fn new(message: &str) -> Self {
        Self {
            message: message.to_string(),
            strip_thinking: false,
        }
    }

    fn with_strip_thinking(message: &str) -> Self {
        Self {
            message: message.to_string(),
            strip_thinking: true,
        }
    }
}

#[async_trait]
impl LLMProvider for MockLLM {
    async fn send_prompt(
        &self,
        _system_prompt: &str,
        _user_prompt: &str,
        _progress: Option<&dyn gcop_rs::llm::ProgressReporter>,
    ) -> Result<String> {
        Ok(self.message.clone())
    }

    async fn generate_commit_message(
        &self,
        _diff: &str,
        _context: Option<CommitContext>,
        _progress: Option<&dyn gcop_rs::llm::ProgressReporter>,
    ) -> Result<String> {
        Ok(self.message.clone())
    }

    async fn generate_commit_message_streaming(
        &self,
        _diff: &str,
        _context: Option<CommitContext>,
    ) -> Result<gcop_rs::llm::StreamHandle> {
        let (tx, rx) = mpsc::channel(10);
        let message = self.message.clone();
        tokio::spawn(async move {
            let _ = tx.send(StreamChunk::Delta(message)).await;
            let _ = tx.send(StreamChunk::Done).await;
        });
        Ok(gcop_rs::llm::StreamHandle { receiver: rx })
    }

    async fn review_code(
        &self,
        _diff: &str,
        _review_type: ReviewType,
        _custom_prompt: Option<&str>,
        _progress: Option<&dyn gcop_rs::llm::ProgressReporter>,
    ) -> Result<ReviewResult> {
        Ok(ReviewResult {
            summary: "OK".to_string(),
            issues: vec![],
            suggestions: vec![],
        })
    }

    fn name(&self) -> &str {
        "MockLLM"
    }

    fn supports_streaming(&self) -> bool {
        false
    }

    fn strip_thinking(&self) -> bool {
        self.strip_thinking
    }

    async fn validate(&self) -> Result<()> {
        Ok(())
    }
}

// === Helpers ================================================================

fn split_options(yes: bool, format: OutputFormat) -> CommitOptions<'static> {
    CommitOptions {
        dry_run: false,
        yes,
        no_edit: false,
        split: true,
        amend: false,
        format,
        feedback: &[],
        provider_override: None,
        verbose: false,
    }
}

// === Tests ==================================================================

/// `--split -y` + a plain-text (non-JSON) response → exactly ONE commit whose
/// message is the model's text and whose file set is every staged file.
#[tokio::test]
async fn integration_y_plaintext_commits_all_staged_once() {
    let config = AppConfig::default();
    let options = split_options(true, OutputFormat::Text);
    let repo = RecordingGitOps::new();
    let provider: Arc<dyn LLMProvider> = Arc::new(MockLLM::new("feat: bundle all changes"));

    let result =
        gcop_rs::commands::split::run_split_flow(&options, &config, &repo, &provider).await;
    assert!(result.is_ok(), "expected Ok, got {:?}", result);

    let commits = repo.commits();
    assert_eq!(commits.len(), 1, "exactly one commit expected");
    assert_eq!(commits[0].0, "feat: bundle all changes");

    let mut committed_files = commits[0].1.clone();
    committed_files.sort();
    assert_eq!(
        committed_files,
        vec!["a.rs".to_string(), "b.rs".to_string(), "c.rs".to_string()],
        "the single commit must cover all staged files"
    );
}

/// `--split -y` + truncated/broken JSON → hard error, ZERO commits.
#[tokio::test]
async fn integration_y_truncated_json_errors_no_commit() {
    let config = AppConfig::default();
    let options = split_options(true, OutputFormat::Text);
    let repo = RecordingGitOps::new();
    let provider: Arc<dyn LLMProvider> = Arc::new(MockLLM::new("{\"groups\": ["));

    let result =
        gcop_rs::commands::split::run_split_flow(&options, &config, &repo, &provider).await;
    assert!(
        matches!(result, Err(GcopError::SplitParseFailed(_))),
        "expected SplitParseFailed, got {:?}",
        result
    );
    assert_eq!(repo.commits().len(), 0, "no commit on broken JSON");
}

/// `--split --json` + plain-text response → emits the SplitCommitData payload
/// (committed:false dry-run contract) and creates NO real commit.
#[tokio::test]
async fn integration_json_plaintext_no_commit() {
    let config = AppConfig::default();
    let options = split_options(false, OutputFormat::Json);
    let repo = RecordingGitOps::new();
    let provider: Arc<dyn LLMProvider> = Arc::new(MockLLM::new("feat: x"));

    let result =
        gcop_rs::commands::split::run_split_flow(&options, &config, &repo, &provider).await;
    assert!(result.is_ok(), "json mode should succeed, got {:?}", result);
    assert_eq!(
        repo.commits().len(),
        0,
        "json mode honours committed:false — never commits"
    );
}

/// The production call site threads `provider.strip_thinking()`: a provider
/// that strips thinking tags must have them removed from the committed message.
#[tokio::test]
async fn integration_y_provider_strip_thinking_threaded() {
    let config = AppConfig::default();
    let options = split_options(true, OutputFormat::Text);
    let repo = RecordingGitOps::new();
    let provider: Arc<dyn LLMProvider> = Arc::new(MockLLM::with_strip_thinking(
        "<think>plan</think>\nfeat: ship it",
    ));

    let result =
        gcop_rs::commands::split::run_split_flow(&options, &config, &repo, &provider).await;
    assert!(result.is_ok(), "expected Ok, got {:?}", result);

    let commits = repo.commits();
    assert_eq!(commits.len(), 1);
    assert_eq!(commits[0].0, "feat: ship it", "thinking tags stripped");
}

/// `--split -y` + a prose preamble wrapping a VALID groups object → the real
/// grouping is recovered and committed (NOT collapsed, NOT the literal blob).
#[tokio::test]
async fn integration_y_prose_wrapped_valid_json_recovers() {
    let config = AppConfig::default();
    let options = split_options(true, OutputFormat::Text);
    let repo = RecordingGitOps::new();
    let provider: Arc<dyn LLMProvider> = Arc::new(MockLLM::new(
        "Sure!\n{\"groups\":[{\"files\":[\"a.rs\",\"b.rs\",\"c.rs\"],\"message\":\"feat: all\"}]}",
    ));

    let result =
        gcop_rs::commands::split::run_split_flow(&options, &config, &repo, &provider).await;
    assert!(result.is_ok(), "expected Ok, got {:?}", result);

    let commits = repo.commits();
    assert_eq!(commits.len(), 1);
    assert_eq!(commits[0].0, "feat: all");
    // The committed message must be the recovered subject, NOT the raw blob.
    assert!(
        !commits[0].0.contains('{'),
        "raw JSON must not be committed"
    );
}

/// `--split -y` + a prose preamble wrapping TRUNCATED JSON → hard error, ZERO
/// commits. The JSON-bearing blob must never be committed verbatim.
#[tokio::test]
async fn integration_y_prose_wrapped_truncated_json_no_commit() {
    let config = AppConfig::default();
    let options = split_options(true, OutputFormat::Text);
    let repo = RecordingGitOps::new();
    let provider: Arc<dyn LLMProvider> = Arc::new(MockLLM::new("Here you go:\n{\"groups\": ["));

    let result =
        gcop_rs::commands::split::run_split_flow(&options, &config, &repo, &provider).await;
    assert!(
        matches!(result, Err(GcopError::SplitParseFailed(_))),
        "prose-wrapped broken JSON must error, got {:?}",
        result
    );
    assert_eq!(repo.commits().len(), 0, "no commit on JSON-bearing garbage");
}

/// `--split -y` + an unterminated ```json fence around truncated JSON
/// (streaming cutoff) → hard error, ZERO commits.
#[tokio::test]
async fn integration_y_unclosed_fence_truncated_json_no_commit() {
    let config = AppConfig::default();
    let options = split_options(true, OutputFormat::Text);
    let repo = RecordingGitOps::new();
    let provider: Arc<dyn LLMProvider> = Arc::new(MockLLM::new("```json\n{\"groups\": ["));

    let result =
        gcop_rs::commands::split::run_split_flow(&options, &config, &repo, &provider).await;
    assert!(
        matches!(result, Err(GcopError::SplitParseFailed(_))),
        "unclosed-fence broken JSON must error, got {:?}",
        result
    );
    assert_eq!(repo.commits().len(), 0);
}

/// `--split --json` + truncated JSON → Err (error envelope emitted), no commit.
#[tokio::test]
async fn integration_json_truncated_json_errors_no_commit() {
    let config = AppConfig::default();
    let options = split_options(false, OutputFormat::Json);
    let repo = RecordingGitOps::new();
    let provider: Arc<dyn LLMProvider> = Arc::new(MockLLM::new("{\"groups\": ["));

    let result =
        gcop_rs::commands::split::run_split_flow(&options, &config, &repo, &provider).await;
    assert!(
        matches!(result, Err(GcopError::SplitParseFailed(_))),
        "json mode must propagate the parse error, got {:?}",
        result
    );
    assert_eq!(repo.commits().len(), 0);
}

/// `--split -y` + structured/truncated JSON debris in many disguises (stacked
/// markers, prose preamble, single-quoted, renamed keys, brace-wrapped prose)
/// → hard error, ZERO commits. The structural gate must catch all end-to-end.
#[tokio::test]
async fn integration_y_structured_debris_never_commits() {
    let debris = [
        "> > {\"x\": 1",
        "Here is the JSON:\n{groups: [{files: [a.rs]",
        "Output:\n{'groups': [{'files': ['a.rs'], 'message': 'feat: a'}]}",
        "## Commits\n{\"commit_groups\": [{\"paths\": [\"a.rs\"]",
        "Note:\n- {oops broken json blob here",
        "{Note to self: split into auth and ui}",
    ];
    for raw in debris {
        let config = AppConfig::default();
        let options = split_options(true, OutputFormat::Text);
        let repo = RecordingGitOps::new();
        let provider: Arc<dyn LLMProvider> = Arc::new(MockLLM::new(raw));

        let result =
            gcop_rs::commands::split::run_split_flow(&options, &config, &repo, &provider).await;
        assert!(
            matches!(result, Err(GcopError::SplitParseFailed(_))),
            "debris must error, got {:?} for {:?}",
            result,
            raw
        );
        assert_eq!(repo.commits().len(), 0, "no commit for debris {:?}", raw);
    }
}

/// `--split -y` + a recovery that covers only a SUBSET of staged files → must
/// NOT silently drop the uncovered file; errors with zero commits.
#[tokio::test]
async fn integration_y_recovery_subset_coverage_no_commit() {
    let config = AppConfig::default();
    let options = split_options(true, OutputFormat::Text);
    let repo = RecordingGitOps::new(); // stages a.rs, b.rs, c.rs
    // Valid JSON, but only covers a.rs (b.rs, c.rs dropped).
    let provider: Arc<dyn LLMProvider> = Arc::new(MockLLM::new(
        "Here:\n{\"groups\":[{\"files\":[\"a.rs\"],\"message\":\"feat: a\"}]}",
    ));

    let result =
        gcop_rs::commands::split::run_split_flow(&options, &config, &repo, &provider).await;
    assert!(
        matches!(result, Err(GcopError::SplitParseFailed(_))),
        "subset-covering recovery must error, got {:?}",
        result
    );
    assert_eq!(repo.commits().len(), 0);
}

/// `--split -y` + a bracket-tag plain subject ([skip ci]) → must NOT be
/// misclassified as a JSON array; commits as one collapsed commit.
#[tokio::test]
async fn integration_y_bracket_tag_subject_commits() {
    let config = AppConfig::default();
    let options = split_options(true, OutputFormat::Text);
    let repo = RecordingGitOps::new();
    let provider: Arc<dyn LLMProvider> = Arc::new(MockLLM::new("[skip ci] chore: bump deps"));

    let result =
        gcop_rs::commands::split::run_split_flow(&options, &config, &repo, &provider).await;
    assert!(
        result.is_ok(),
        "bracket-tag subject must commit, got {:?}",
        result
    );

    let commits = repo.commits();
    assert_eq!(commits.len(), 1);
    assert_eq!(commits[0].0, "[skip ci] chore: bump deps");
}
